use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};

pub const CODE_LEN: usize = 16;
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"-_!@#$%&*+=?";
const ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_!@#$%&*+=?";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub client_nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub version: u32,
    pub endpoint: String,
    pub certificate_pem: String,
    pub expires_unix: u64,
    pub server_nonce: String,
    pub proof: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishRequest {
    pub client_nonce: String,
    pub server_nonce: String,
    pub proof: String,
}

pub fn generate_code() -> String {
    let mut rng = rand::rngs::OsRng;
    loop {
        let code: String = (0..CODE_LEN)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
            .collect();
        let bytes = code.as_bytes();
        if bytes.iter().any(|c| UPPER.contains(c))
            && bytes.iter().any(|c| LOWER.contains(c))
            && bytes.iter().any(|c| DIGITS.contains(c))
            && bytes.iter().any(|c| SYMBOLS.contains(c))
        {
            return code;
        }
    }
}

pub fn valid_code(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|byte| ALPHABET.contains(&byte))
}

pub fn new_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn validate_nonce(nonce: &str) -> Result<()> {
    ensure!(nonce.len() == 43, "nonce must encode exactly 32 bytes");
    let bytes = URL_SAFE_NO_PAD
        .decode(nonce)
        .context("invalid base64url nonce")?;
    ensure!(
        bytes.len() == 32 && URL_SAFE_NO_PAD.encode(&bytes) == nonce,
        "nonce must use canonical base64url encoding"
    );
    Ok(())
}

fn payload(domain: &str, client_nonce: &str, challenge: &Challenge) -> Result<Vec<u8>> {
    validate_nonce(client_nonce)?;
    validate_nonce(&challenge.server_nonce)?;
    ensure!(challenge.version == 1, "unsupported pairing version");
    let mut out = Vec::with_capacity(challenge.certificate_pem.len() + 256);
    for field in [
        domain,
        client_nonce,
        &challenge.server_nonce,
        &challenge.endpoint,
        &challenge.certificate_pem,
        &challenge.expires_unix.to_string(),
    ] {
        let bytes = field.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

fn sign(code: &str, domain: &str, client_nonce: &str, challenge: &Challenge) -> Result<String> {
    ensure!(valid_code(code), "invalid pairing code");
    let key = hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(hmac::sign(&key, &payload(domain, client_nonce, challenge)?)))
}
fn verify(
    code: &str,
    domain: &str,
    client_nonce: &str,
    challenge: &Challenge,
    supplied: &str,
) -> Result<()> {
    ensure!(valid_code(code), "invalid pairing code");
    let supplied_bytes = URL_SAFE_NO_PAD
        .decode(supplied)
        .context("invalid proof encoding")?;
    ensure!(
        supplied_bytes.len() == 32 && URL_SAFE_NO_PAD.encode(&supplied_bytes) == supplied,
        "invalid proof encoding"
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, code.as_bytes());
    hmac::verify(
        &key,
        &payload(domain, client_nonce, challenge)?,
        &supplied_bytes,
    )
    .map_err(|_| anyhow::anyhow!("pairing proof mismatch"))
}

pub fn server_proof(code: &str, client_nonce: &str, challenge: &Challenge) -> Result<String> {
    sign(code, "winremote-server-proof-v1", client_nonce, challenge)
}
pub fn verify_server(code: &str, client_nonce: &str, challenge: &Challenge) -> Result<()> {
    verify(
        code,
        "winremote-server-proof-v1",
        client_nonce,
        challenge,
        &challenge.proof,
    )
}
pub fn client_proof(code: &str, client_nonce: &str, challenge: &Challenge) -> Result<String> {
    sign(code, "winremote-client-proof-v1", client_nonce, challenge)
}
pub fn verify_client(
    code: &str,
    client_nonce: &str,
    challenge: &Challenge,
    supplied: &str,
) -> Result<()> {
    verify(
        code,
        "winremote-client-proof-v1",
        client_nonce,
        challenge,
        supplied,
    )
}

pub fn parse_short_invite(input: &str) -> Result<(String, String)> {
    let input = input.trim();
    let (address, code) = input
        .rsplit_once(':')
        .context("invitation must contain address and code")?;
    ensure!(valid_code(code), "invalid 16-character pairing code");
    let (ip, port) = if let Ok(ip) = address.parse::<Ipv4Addr>() {
        (IpAddr::V4(ip), 8443)
    } else {
        let (host, port) = address
            .rsplit_once(':')
            .context("invitation must contain a port")?;
        let port: u16 = port.parse().context("invalid invitation port")?;
        ensure!(port != 0, "invalid invitation port");
        let ip = if host.starts_with('[') && host.ends_with(']') {
            host[1..host.len() - 1]
                .parse::<IpAddr>()
                .context("invalid invitation IP")?
        } else {
            IpAddr::V4(
                host.parse::<Ipv4Addr>()
                    .context("invalid invitation IPv4 address")?,
            )
        };
        (ip, port)
    };
    let endpoint = match ip {
        IpAddr::V4(ip) => format!("https://{ip}:{port}"),
        IpAddr::V6(ip) => format!("https://[{ip}]:{port}"),
    };
    Ok((endpoint, code.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn challenge() -> Challenge {
        Challenge {
            version: 1,
            endpoint: "https://192.168.2.20:8443".into(),
            certificate_pem: "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----\n".into(),
            expires_unix: 1_800_000_000,
            server_nonce: new_nonce(),
            proof: String::new(),
        }
    }
    #[test]
    fn proofs_bind_certificate_endpoint_nonces_and_expiry() {
        let code = generate_code();
        let client_nonce = new_nonce();
        let mut response = challenge();
        response.proof = server_proof(&code, &client_nonce, &response).unwrap();
        verify_server(&code, &client_nonce, &response).unwrap();
        assert!(verify_server(&generate_code(), &client_nonce, &response).is_err());
        assert!(verify_server(&code, &new_nonce(), &response).is_err());
        let mut tampered = response.clone();
        tampered.server_nonce = new_nonce();
        assert!(verify_server(&code, &client_nonce, &tampered).is_err());
        let mut tampered = response.clone();
        tampered.endpoint.push('x');
        assert!(verify_server(&code, &client_nonce, &tampered).is_err());
        let mut tampered = response.clone();
        tampered.certificate_pem.push('x');
        assert!(verify_server(&code, &client_nonce, &tampered).is_err());
        let mut tampered = response.clone();
        tampered.expires_unix += 1;
        assert!(verify_server(&code, &client_nonce, &tampered).is_err());
        let proof = client_proof(&code, &client_nonce, &response).unwrap();
        verify_client(&code, &client_nonce, &response, &proof).unwrap();
        assert!(verify_client(&code, &client_nonce, &response, &response.proof).is_err());
    }
    #[test]
    fn invitation_parse_and_charset() {
        let code = generate_code();
        assert!(valid_code(&code));
        assert!(code.bytes().any(|c| SYMBOLS.contains(&c)));
        assert_eq!(
            parse_short_invite(&format!("192.168.2.20:{code}"))
                .unwrap()
                .0,
            "https://192.168.2.20:8443"
        );
        assert_eq!(
            parse_short_invite(&format!("192.168.2.20:9000:{code}"))
                .unwrap()
                .0,
            "https://192.168.2.20:9000"
        );
        assert_eq!(
            parse_short_invite(&format!("[::1]:9000:{code}")).unwrap().0,
            "https://[::1]:9000"
        );
        assert!(parse_short_invite("192.168.2.20:short").is_err());
    }
}
