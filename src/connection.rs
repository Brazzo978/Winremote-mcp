use crate::protocol::Connection;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{redirect::Policy, Certificate, Client};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::{Host, Url};

const MAX_CONNECTION_BYTES: u64 = 32 * 1024;

pub fn now_unix() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

pub fn ensure_valid(connection: &Connection) -> Result<()> {
    if connection.version != 1 {
        bail!("Unsupported connection format");
    }
    if connection.expires_unix <= now_unix()? {
        bail!("Connection expired; obtain a new invitation from the Windows host");
    }
    let token = URL_SAFE_NO_PAD
        .decode(&connection.token)
        .context("Invalid connection token")?;
    if token.len() != 32 {
        bail!("Connection token must contain 32 random bytes");
    }
    let endpoint = Url::parse(&connection.endpoint).context("Invalid endpoint")?;
    if endpoint.scheme() != "https"
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        bail!("Endpoint must be an HTTPS origin without credentials, query, fragment or path");
    }
    match endpoint.host() {
        Some(Host::Ipv4(ip))
            if !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast() => {}
        Some(Host::Ipv6(ip)) if !ip.is_unspecified() && !ip.is_multicast() => {}
        _ => bail!("Endpoint must identify a concrete IP address"),
    }
    if connection.certificate_pem.len() > 16 * 1024 {
        bail!("Certificate too large");
    }
    Certificate::from_pem(connection.certificate_pem.as_bytes())
        .context("Invalid server certificate")?;
    Ok(())
}

pub fn parse_invitation(invitation: &str) -> Result<Connection> {
    if invitation.len() > MAX_CONNECTION_BYTES as usize {
        bail!("Invitation too large");
    }
    let encoded = invitation
        .trim()
        .strip_prefix("wb1_")
        .context("Invitation must start with wb1_")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("Invalid invitation encoding")?;
    let connection: Connection =
        serde_json::from_slice(&bytes).context("Invalid invitation format")?;
    ensure_valid(&connection)?;
    Ok(connection)
}

pub fn load(path: &Path) -> Result<Connection> {
    let file = File::open(path).context("Cannot open connection file; run pair first")?;
    let mut bytes = Vec::new();
    file.take(MAX_CONNECTION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONNECTION_BYTES {
        bail!("Connection file too large");
    }
    let connection: Connection =
        serde_json::from_slice(&bytes).context("Invalid connection file")?;
    ensure_valid(&connection)?;
    Ok(connection)
}

pub fn http_client(connection: &Connection) -> Result<Client> {
    ensure_valid(connection)?;
    let certificate = Certificate::from_pem(connection.certificate_pem.as_bytes())?;
    Ok(Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(certificate)
        .https_only(true)
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(130))
        .build()?)
}

/// Creates a new credential file. Existing credentials are never silently replaced.
pub fn save(path: &Path, connection: &Connection) -> Result<()> {
    ensure_valid(connection)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("Cannot create connection file (it may already exist)")?;
    // Restrict access BEFORE writing credentials.
    if let Err(error) = restrict_new_file(path) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    let result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(connection)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    result
}

#[cfg(windows)]
fn restrict_new_file(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let root = std::env::var_os("SystemRoot").context("SystemRoot is missing")?;
    let system32 = std::path::PathBuf::from(root).join("System32");
    let output = Command::new(system32.join("WindowsPowerShell/v1.0/powershell.exe"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
        ])
        .creation_flags(0x08000000)
        .output()
        .context("Cannot determine credential file owner")?;
    if !output.status.success() {
        bail!("Cannot determine credential file owner");
    }
    let sid = String::from_utf8(output.stdout)?;
    let sid = sid.trim();
    if !sid.starts_with("S-1-")
        || !sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
    {
        bail!("Invalid Windows owner SID");
    }
    let result = Command::new(system32.join("icacls.exe"))
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(F)")])
        .creation_flags(0x08000000)
        .output()
        .context("Cannot restrict credential file permissions")?;
    if !result.status.success() {
        bail!("Cannot restrict credential file permissions");
    }
    Ok(())
}

#[cfg(not(windows))]
fn restrict_new_file(_path: &Path) -> Result<()> {
    Ok(())
}

/// Replace credentials atomically with a newly created private file in the same directory.
pub fn replace(path: &Path, connection: &Connection) -> Result<()> {
    use rand::Rng;
    ensure_valid(connection)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".winremote-{:032x}.tmp",
        rand::thread_rng().gen::<u128>()
    ));
    save(&temporary, connection)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("Cannot atomically replace connection credentials");
    }
    Ok(())
}

/// Resolve a short invitation by authenticating a fresh TLS identity before accepting credentials.
/// Legacy certificate-bearing invitations remain supported.
pub async fn resolve_invitation(invitation: &str) -> Result<Connection> {
    use crate::pairing::{self, Challenge, ChallengeRequest, FinishRequest};
    if invitation.trim().starts_with("wb1_") {
        return parse_invitation(invitation);
    }
    if invitation.len() > 512 {
        bail!("Invitation too large");
    }
    let (endpoint, code) = pairing::parse_short_invite(invitation)?;
    // This client is used ONLY for a public, credential-free challenge. Its response
    // is untrusted until the invitation code authenticates the complete TLS identity.
    let bootstrap = Client::builder()
        .use_rustls_tls()
        .danger_accept_invalid_certs(true)
        .https_only(true)
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;
    let client_nonce = pairing::new_nonce();
    let challenge: Challenge = bounded_json(
        bootstrap
            .post(format!("{endpoint}/v1/pair/challenge"))
            .json(&ChallengeRequest {
                client_nonce: client_nonce.clone(),
            })
            .send()
            .await
            .context("Cannot reach the Windows bridge")?,
    )
    .await?;
    if challenge.endpoint != endpoint || challenge.expires_unix <= now_unix()? {
        bail!("Pairing endpoint mismatch or invitation expired");
    }
    pairing::verify_server(&code, &client_nonce, &challenge)
        .context("Wrong pairing code or untrusted host")?;
    // Temporary validated identity uses a placeholder token; no credential is sent
    // until the pinned TLS connection proves possession of the authenticated key.
    let identity = Connection {
        version: 1,
        endpoint: endpoint.clone(),
        token: URL_SAFE_NO_PAD.encode([0u8; 32]),
        certificate_pem: challenge.certificate_pem.clone(),
        expires_unix: challenge.expires_unix,
    };
    let client = http_client(&identity)?;
    let proof = pairing::client_proof(&code, &client_nonce, &challenge)?;
    let connection: Connection = bounded_json(
        client
            .post(format!("{endpoint}/v1/pair/finish"))
            .json(&FinishRequest {
                client_nonce,
                server_nonce: challenge.server_nonce,
                proof,
            })
            .send()
            .await
            .context("Pinned pairing connection failed")?,
    )
    .await?;
    ensure_valid(&connection)?;
    if connection.endpoint != identity.endpoint
        || connection.certificate_pem != identity.certificate_pem
        || connection.expires_unix != identity.expires_unix
    {
        bail!("Pairing identity changed");
    }
    Ok(connection)
}

async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        bail!("Pairing rejected (HTTP {})", response.status().as_u16());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_CONNECTION_BYTES as usize {
            bail!("Pairing response too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Invalid pairing response")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid_connection() -> Connection {
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        Connection {
            version: 1,
            endpoint: "https://127.0.0.1:8443".into(),
            token: URL_SAFE_NO_PAD.encode([17u8; 32]),
            certificate_pem: cert.cert.pem(),
            expires_unix: now_unix().unwrap() + 3600,
        }
    }
    #[test]
    fn invitation_roundtrip() {
        let connection = valid_connection();
        let invitation = format!(
            "wb1_{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&connection).unwrap())
        );
        assert_eq!(
            parse_invitation(&invitation).unwrap().endpoint,
            connection.endpoint
        );
    }
    #[test]
    fn rejects_expired_and_short_token() {
        let mut connection = valid_connection();
        connection.expires_unix = 0;
        assert!(ensure_valid(&connection).is_err());
        connection.expires_unix = now_unix().unwrap() + 3600;
        connection.token = URL_SAFE_NO_PAD.encode([0u8; 4]);
        assert!(ensure_valid(&connection).is_err());
    }
    #[test]
    fn rejects_unsafe_endpoints() {
        let mut connection = valid_connection();
        for endpoint in [
            "http://127.0.0.1",
            "https://user:pass@127.0.0.1",
            "https://127.0.0.1/x",
            "https://127.0.0.1/?token=secret",
            "https://127.0.0.1/#fragment",
            "https://0.0.0.0",
            "https://example.com",
        ] {
            connection.endpoint = endpoint.into();
            assert!(ensure_valid(&connection).is_err(), "{endpoint}");
        }
    }
    #[test]
    fn does_not_overwrite_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let connection = valid_connection();
        save(&path, &connection).unwrap();
        assert_eq!(load(&path).unwrap().token, connection.token);
        assert!(save(&path, &connection).is_err());
    }

    #[test]
    fn atomic_replacement_preserves_old_credentials_on_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        let mut first = valid_connection();
        save(&path, &first).unwrap();
        let original_token = first.token.clone();
        first.expires_unix = 0;
        assert!(replace(&path, &first).is_err());
        assert_eq!(load(&path).unwrap().token, original_token);
        let mut second = valid_connection();
        second.token = URL_SAFE_NO_PAD.encode([19u8; 32]);
        replace(&path, &second).unwrap();
        assert_eq!(load(&path).unwrap().token, second.token);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
