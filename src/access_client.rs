//! Credential-free bootstrap followed by exact-certificate-pinned polling.
use crate::{
    access::{self, BeginRequest, BeginResponse, ChallengeRequest, ChallengeResponse, PollRequest},
    connection,
    protocol::Connection,
};
use anyhow::{bail, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use rand::RngCore;
use reqwest::{redirect::Policy, tls::TlsInfo, Certificate, Client, Response};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::Deserialize;
use std::{net::IpAddr, time::Duration};

const MAX_RESPONSE: usize = 32 * 1024;

pub struct AccessClient;
pub struct PendingAccess {
    pub request_id: String,
    pub endpoint: String,
    pub verification_code: String,
    pub expires_unix: u64,
    pub poll_after_secs: u64,
    client: Client,
    key: Ed25519KeyPair,
    certificate_der: Vec<u8>,
    context: [u8; 32],
}
pub enum ClientPoll {
    Pending,
    Denied,
    Expired,
    Approved(Connection),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PollWire {
    status: String,
    #[serde(default)]
    connection: Option<Connection>,
}

impl AccessClient {
    pub async fn begin(ip: IpAddr, port: u16, label: Option<String>) -> Result<PendingAccess> {
        if port == 0 || !valid_ip(ip) {
            bail!("A concrete IP address and valid port are required");
        }
        if label.as_deref().is_some_and(|v| !valid_label(v)) {
            bail!("Invalid client label");
        }
        let endpoint = match ip {
            IpAddr::V4(ip) => format!("https://{ip}:{port}"),
            IpAddr::V6(ip) => format!("https://[{ip}]:{port}"),
        };
        let seed = {
            let mut bytes = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            bytes
        };
        let key = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| anyhow::anyhow!("Cannot create access key"))?;
        let mut client_nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut client_nonce);
        // Only this public, credential-free request may use unverified TLS.
        let bootstrap = Client::builder()
            .use_rustls_tls()
            .danger_accept_invalid_certs(true)
            .tls_info(true)
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        let response = bootstrap
            .post(format!("{endpoint}/v1/access/request"))
            .json(&BeginRequest {
                client_public_key: URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                client_nonce: URL_SAFE_NO_PAD.encode(client_nonce),
                label,
            })
            .send()
            .await
            .context("Cannot reach the Windows bridge")?;
        let observed_der = peer_der(&response)?;
        let begin: BeginResponse = bounded_json(response).await?;
        if begin.version != 1
            || begin.endpoint != endpoint
            || begin.poll_after_secs < 2
            || begin.poll_after_secs > 30
            || begin.expires_unix <= connection::now_unix()?
        {
            bail!("Access response has an invalid endpoint or lifetime");
        }
        let announced_der = URL_SAFE_NO_PAD
            .decode(&begin.certificate_der)
            .context("Invalid access certificate field")?;
        if announced_der != observed_der || observed_der.len() > 16 * 1024 {
            bail!("TLS certificate does not match the access response");
        }
        let peer: IpAddr = begin
            .observed_peer_ip
            .parse()
            .context("Invalid observed peer IP")?;
        if !valid_ip(peer) || peer.to_string() != begin.observed_peer_ip {
            bail!("Invalid observed peer IP");
        }
        let server_nonce = access::decode_exact::<32>(&begin.server_nonce)
            .map_err(|_| anyhow::anyhow!("Invalid server nonce"))?;
        let id = access::decode_exact::<16>(&begin.request_id)
            .map_err(|_| anyhow::anyhow!("Invalid request ID"))?;
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let context = access::context_digest(
            &endpoint,
            &observed_der,
            &public_key,
            &client_nonce,
            &server_nonce,
            &id,
            peer,
            begin.expires_unix,
        );
        let certificate =
            Certificate::from_der(&observed_der).context("Invalid TLS certificate")?;
        let pinned = Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .tls_info(true)
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(PendingAccess {
            request_id: begin.request_id,
            endpoint,
            verification_code: access::verification_code(&context),
            expires_unix: begin.expires_unix,
            poll_after_secs: begin.poll_after_secs,
            client: pinned,
            key,
            certificate_der: observed_der,
            context,
        })
    }
}

impl PendingAccess {
    pub async fn poll(&mut self) -> Result<ClientPoll> {
        let response = self
            .client
            .post(format!("{}/v1/access/challenge", self.endpoint))
            .json(&ChallengeRequest {
                request_id: self.request_id.clone(),
            })
            .send()
            .await
            .context("Cannot reach the pinned Windows bridge")?;
        check_peer(&response, &self.certificate_der)?;
        if response.status().as_u16() == 410 {
            return Ok(ClientPoll::Expired);
        }
        if response.status().as_u16() == 429 {
            return Ok(ClientPoll::Pending);
        }
        let challenge: ChallengeResponse = bounded_json(response).await?;
        let bytes = access::decode_exact::<32>(&challenge.server_challenge)
            .map_err(|_| anyhow::anyhow!("Invalid server challenge"))?;
        if challenge.expires_unix <= connection::now_unix()?
            || challenge.expires_unix > self.expires_unix
        {
            bail!("Invalid challenge expiration");
        }
        let id = access::decode_exact::<16>(&self.request_id)
            .map_err(|_| anyhow::anyhow!("Invalid request ID"))?;
        // The context is reconstructed only from the verified bootstrap response. Store its
        // digest alongside the key, so each signature binds all request fields and TLS DER.
        let signature = self.key.sign(&access::proof_payload(
            &self.context,
            &id,
            &bytes,
            challenge.expires_unix,
        ));
        let response = self
            .client
            .post(format!("{}/v1/access/poll", self.endpoint))
            .json(&PollRequest {
                request_id: self.request_id.clone(),
                server_challenge: challenge.server_challenge,
                signature: URL_SAFE_NO_PAD.encode(signature.as_ref()),
            })
            .send()
            .await
            .context("Cannot poll the pinned Windows bridge")?;
        check_peer(&response, &self.certificate_der)?;
        if response.status().as_u16() == 410 {
            return Ok(ClientPoll::Expired);
        }
        if response.status().as_u16() == 429 {
            return Ok(ClientPoll::Pending);
        }
        let result: PollWire = bounded_json(response).await?;
        match (result.status.as_str(), result.connection) {
            ("pending", None) => Ok(ClientPoll::Pending),
            ("denied", None) => Ok(ClientPoll::Denied),
            ("expired", None) => Ok(ClientPoll::Expired),
            ("approved", Some(connection)) => {
                connection::ensure_valid(&connection)?;
                if connection.endpoint != self.endpoint
                    || pem_der(&connection.certificate_pem)? != self.certificate_der
                {
                    bail!("Approved connection changed TLS identity");
                }
                Ok(ClientPoll::Approved(connection))
            }
            _ => bail!("Invalid access status response"),
        }
    }
}

fn peer_der(response: &Response) -> Result<Vec<u8>> {
    let info = response
        .extensions()
        .get::<TlsInfo>()
        .context("TLS peer certificate unavailable")?;
    let der = info
        .peer_certificate()
        .context("TLS peer certificate unavailable")?;
    if der.is_empty() || der.len() > 16 * 1024 {
        bail!("Invalid TLS peer certificate");
    }
    Ok(der.to_vec())
}
fn check_peer(response: &Response, expected: &[u8]) -> Result<()> {
    if peer_der(response)? != expected {
        bail!("TLS peer certificate changed");
    }
    Ok(())
}
async fn bounded_json<T: serde::de::DeserializeOwned>(mut response: Response) -> Result<T> {
    if !response.status().is_success() {
        bail!(
            "Access request rejected (HTTP {})",
            response.status().as_u16()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            bail!("Access response too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Invalid access response")
}
fn pem_der(pem: &str) -> Result<Vec<u8>> {
    if pem.len() > 16 * 1024 {
        bail!("Certificate too large");
    }
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN CERTIFICATE-----")
        .and_then(|s| s.strip_suffix("-----END CERTIFICATE-----"))
        .context("Invalid certificate PEM")?;
    if !body.bytes().all(|b| {
        b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=' || b == b'\r' || b == b'\n'
    }) {
        bail!("Invalid certificate PEM");
    }
    let compact: String = body.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    STANDARD
        .decode(compact)
        .context("Invalid certificate PEM encoding")
}
fn valid_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        IpAddr::V6(ip) => !ip.is_unspecified() && !ip.is_multicast(),
    }
}
fn valid_label(label: &str) -> bool {
    label.chars().count() <= 64
        && label.len() <= 256
        && label
            .chars()
            .all(|c| !c.is_control() && c != '<' && c != '>' && c != '&')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_certificate_must_be_one_exact_der() {
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let pem = certificate.cert.pem();
        let expected = certificate.cert.der().as_ref();
        assert_eq!(pem_der(&pem).unwrap(), expected);
        assert!(pem_der(&(pem.clone() + &pem)).is_err());
        assert!(pem_der("-----BEGIN CERTIFICATE-----\nZm9v!\n-----END CERTIFICATE-----").is_err());
    }

    #[test]
    fn label_and_ip_validation_reject_unsafe_inputs() {
        assert!(!valid_label("agent\nother"));
        assert!(!valid_label("<script>"));
        assert!(!valid_ip("0.0.0.0".parse().unwrap()));
        assert!(!valid_ip("224.0.0.1".parse().unwrap()));
        assert!(valid_ip("192.0.2.1".parse().unwrap()));
    }
}
