//! Local, human-confirmed access requests. No operational credential is stored here.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use ring::{digest, signature};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REQUEST_TTL: Duration = Duration::from_secs(120);
const CHALLENGE_TTL: Duration = Duration::from_secs(10);
const MIN_INTERVAL: Duration = Duration::from_secs(2);
const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_PENDING: usize = 4;
const MAX_RATE_RECORDS: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginRequest {
    pub client_public_key: String,
    pub client_nonce: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginResponse {
    pub version: u32,
    pub request_id: String,
    pub server_nonce: String,
    pub endpoint: String,
    pub certificate_der: String,
    pub observed_peer_ip: String,
    pub expires_unix: u64,
    pub poll_after_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeResponse {
    pub server_challenge: String,
    pub expires_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollRequest {
    pub request_id: String,
    pub server_challenge: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PollOutcome {
    Pending,
    Denied,
    Expired,
    Approved,
}

#[derive(Clone, Debug)]
pub struct PendingView {
    pub request_id: String,
    pub peer_ip: IpAddr,
    pub label: String,
    pub verification_code: String,
    pub expires_unix: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessError {
    Disabled,
    Expired,
    BadRequest,
    NotFound,
    RateLimited,
    Capacity,
    TooSoon,
    InvalidProof,
}
impl AccessError {
    pub fn status_code(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Disabled | Self::InvalidProof => 403,
            Self::NotFound => 404,
            Self::Expired => 410,
            Self::RateLimited | Self::Capacity | Self::TooSoon => 429,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Disabled => "local access is unavailable",
            Self::Expired => "access request expired",
            Self::BadRequest => "invalid access request",
            Self::NotFound => "access request unavailable",
            Self::RateLimited | Self::Capacity | Self::TooSoon => {
                "access request rate limit reached"
            }
            Self::InvalidProof => "access proof rejected",
        }
    }
}
impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for AccessError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    Pending,
    Approved,
    Denied,
}
struct Challenge {
    bytes: [u8; 32],
    expires: Instant,
    expires_unix: u64,
}
struct Entry {
    peer: IpAddr,
    label: String,
    public_key: [u8; 32],
    context: [u8; 32],
    code: String,
    expires: Instant,
    expires_unix: u64,
    decision: Decision,
    challenge: Option<Challenge>,
    last_challenge: Option<Instant>,
    last_poll: Option<Instant>,
}
struct State {
    enabled: bool,
    shutdown: bool,
    entries: HashMap<String, Entry>,
    rate: VecDeque<(Instant, IpAddr)>,
}

pub struct AccessManager {
    endpoint: String,
    certificate_der: Vec<u8>,
    host_expires_unix: u64,
    host_deadline: Instant,
    state: Mutex<State>,
}

impl AccessManager {
    pub fn new(
        endpoint: String,
        certificate_der: Vec<u8>,
        expires_unix: u64,
        deadline: Instant,
    ) -> Self {
        Self {
            endpoint,
            certificate_der,
            host_expires_unix: expires_unix,
            host_deadline: deadline,
            state: Mutex::new(State {
                enabled: false,
                shutdown: false,
                entries: HashMap::new(),
                rate: VecDeque::new(),
            }),
        }
    }
    pub fn set_enabled(&self, enabled: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.enabled = enabled && !state.shutdown;
        if !state.enabled {
            state.entries.clear();
        }
    }
    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.enabled = false;
        state.shutdown = true;
        state.entries.clear();
        state.rate.clear();
    }
    pub fn begin(&self, peer: IpAddr, req: BeginRequest) -> Result<BeginResponse, AccessError> {
        let public_key = decode_exact::<32>(&req.client_public_key)?;
        let client_nonce = decode_exact::<32>(&req.client_nonce)?;
        let label = req.label.unwrap_or_default();
        if !valid_label(&label)
            || !valid_peer(peer)
            || self.certificate_der.is_empty()
            || self.certificate_der.len() > 16 * 1024
        {
            return Err(AccessError::BadRequest);
        }
        let now = Instant::now();
        let unix_now = unix_now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check_available(&state, now, unix_now)?;
        purge(&mut state, now);
        if state
            .entries
            .values()
            .any(|e| e.peer == peer && e.decision != Decision::Denied)
        {
            return Err(AccessError::Capacity);
        }
        if state.entries.len() >= MAX_PENDING {
            return Err(AccessError::Capacity);
        }
        if state.rate.len() >= MAX_RATE_RECORDS
            || state.rate.iter().filter(|(_, ip)| *ip == peer).count() >= 3
        {
            return Err(AccessError::RateLimited);
        }
        let expires = (now + REQUEST_TTL).min(self.host_deadline);
        let remaining = expires.saturating_duration_since(now).as_secs();
        let expires_unix = unix_now
            .saturating_add(remaining)
            .min(self.host_expires_unix);
        if expires_unix <= unix_now {
            return Err(AccessError::Expired);
        }
        let mut id_bytes = [0u8; 16];
        let mut server_nonce = [0u8; 32];
        loop {
            rand::rngs::OsRng.fill_bytes(&mut id_bytes);
            if !state
                .entries
                .contains_key(&URL_SAFE_NO_PAD.encode(id_bytes))
            {
                break;
            }
        }
        rand::rngs::OsRng.fill_bytes(&mut server_nonce);
        let request_id = URL_SAFE_NO_PAD.encode(id_bytes);
        let context = context_digest(
            &self.endpoint,
            &self.certificate_der,
            &public_key,
            &client_nonce,
            &server_nonce,
            &id_bytes,
            peer,
            expires_unix,
        );
        let code = verification_code(&context);
        state.rate.push_back((now, peer));
        state.entries.insert(
            request_id.clone(),
            Entry {
                peer,
                label,
                public_key,
                context,
                code,
                expires,
                expires_unix,
                decision: Decision::Pending,
                challenge: None,
                last_challenge: None,
                last_poll: None,
            },
        );
        Ok(BeginResponse {
            version: 1,
            request_id,
            server_nonce: URL_SAFE_NO_PAD.encode(server_nonce),
            endpoint: self.endpoint.clone(),
            certificate_der: URL_SAFE_NO_PAD.encode(&self.certificate_der),
            observed_peer_ip: peer.to_string(),
            expires_unix,
            poll_after_secs: 2,
        })
    }
    pub fn challenge(
        &self,
        peer: IpAddr,
        req: ChallengeRequest,
    ) -> Result<ChallengeResponse, AccessError> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check_available(&state, now, unix_now())?;
        let entry = state
            .entries
            .get_mut(&req.request_id)
            .ok_or(AccessError::NotFound)?;
        if entry.peer != peer {
            return Err(AccessError::NotFound);
        }
        if entry.expires <= now || entry.expires_unix <= unix_now() {
            return Err(AccessError::Expired);
        }
        if let Some(last) = entry.last_challenge {
            if now.duration_since(last) < MIN_INTERVAL {
                return Err(AccessError::TooSoon);
            }
        }
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let expires = (now + CHALLENGE_TTL).min(entry.expires);
        let expires_unix = unix_now()
            .saturating_add(expires.saturating_duration_since(now).as_secs())
            .min(entry.expires_unix);
        entry.challenge = Some(Challenge {
            bytes,
            expires,
            expires_unix,
        });
        entry.last_challenge = Some(now);
        Ok(ChallengeResponse {
            server_challenge: URL_SAFE_NO_PAD.encode(bytes),
            expires_unix,
        })
    }
    pub fn poll(&self, peer: IpAddr, req: PollRequest) -> Result<PollOutcome, AccessError> {
        let challenge_bytes = decode_exact::<32>(&req.server_challenge)?;
        let sig = decode_exact::<64>(&req.signature)?;
        let id = decode_exact::<16>(&req.request_id)?;
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check_available(&state, now, unix_now())?;
        let entry = state
            .entries
            .get_mut(&req.request_id)
            .ok_or(AccessError::NotFound)?;
        if entry.peer != peer {
            return Err(AccessError::NotFound);
        }
        if entry.expires <= now || entry.expires_unix <= unix_now() {
            return Ok(PollOutcome::Expired);
        }
        let challenge = entry.challenge.take().ok_or(AccessError::InvalidProof)?;
        if challenge.expires <= now
            || challenge.expires_unix <= unix_now()
            || challenge.bytes != challenge_bytes
        {
            return Err(AccessError::InvalidProof);
        }
        let payload = proof_payload(
            &entry.context,
            &id,
            &challenge_bytes,
            challenge.expires_unix,
        );
        signature::UnparsedPublicKey::new(&signature::ED25519, entry.public_key)
            .verify(&payload, &sig)
            .map_err(|_| AccessError::InvalidProof)?;
        if let Some(last) = entry.last_poll {
            if now.duration_since(last) < MIN_INTERVAL {
                return Err(AccessError::TooSoon);
            }
        }
        entry.last_poll = Some(now);
        match entry.decision {
            Decision::Pending => Ok(PollOutcome::Pending),
            Decision::Denied => {
                state.entries.remove(&req.request_id);
                Ok(PollOutcome::Denied)
            }
            Decision::Approved => {
                state.entries.remove(&req.request_id);
                Ok(PollOutcome::Approved)
            }
        }
    }
    pub fn decide(&self, id: &str, approve: bool) -> Result<(), AccessError> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.check_available(&state, now, unix_now())?;
        let entry = state.entries.get_mut(id).ok_or(AccessError::NotFound)?;
        if entry.expires <= now || entry.expires_unix <= unix_now() {
            return Err(AccessError::Expired);
        }
        if entry.decision != Decision::Pending {
            return Err(AccessError::NotFound);
        }
        entry.decision = if approve {
            Decision::Approved
        } else {
            Decision::Denied
        };
        Ok(())
    }
    pub fn pending_views(&self) -> Vec<PendingView> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.check_available(&state, now, unix_now()).is_err() {
            state.entries.clear();
            return Vec::new();
        }
        purge(&mut state, now);
        let mut views: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, e)| e.decision == Decision::Pending)
            .map(|(id, e)| PendingView {
                request_id: id.clone(),
                peer_ip: e.peer,
                label: e.label.clone(),
                verification_code: e.code.clone(),
                expires_unix: e.expires_unix,
            })
            .collect();
        views.sort_by(|a, b| a.request_id.cmp(&b.request_id));
        views
    }
    fn check_available(&self, state: &State, now: Instant, unix: u64) -> Result<(), AccessError> {
        if !state.enabled || state.shutdown {
            return Err(AccessError::Disabled);
        }
        if now >= self.host_deadline || unix >= self.host_expires_unix {
            return Err(AccessError::Expired);
        }
        Ok(())
    }
}

fn purge(state: &mut State, now: Instant) {
    state
        .entries
        .retain(|_, e| e.expires > now && e.expires_unix > unix_now());
    while state
        .rate
        .front()
        .is_some_and(|(at, _)| now.duration_since(*at) >= RATE_WINDOW)
    {
        state.rate.pop_front();
    }
}
fn valid_label(label: &str) -> bool {
    label.chars().count() <= 64
        && label.len() <= 256
        && label
            .chars()
            .all(|c| !c.is_control() && c != '<' && c != '>' && c != '&')
}
fn valid_peer(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        IpAddr::V6(ip) => !ip.is_unspecified() && !ip.is_multicast(),
    }
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn decode_exact<const N: usize>(value: &str) -> Result<[u8; N], AccessError> {
    if value.len() > N.saturating_mul(2).saturating_add(4) {
        return Err(AccessError::BadRequest);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AccessError::BadRequest)?;
    if bytes.len() != N || URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(AccessError::BadRequest);
    }
    bytes.try_into().map_err(|_| AccessError::BadRequest)
}
fn field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
// Each argument is an independently bound field of the versioned cryptographic transcript.
#[allow(clippy::too_many_arguments)]
pub(crate) fn context_digest(
    endpoint: &str,
    cert_der: &[u8],
    public_key: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
    request_id: &[u8; 16],
    peer: IpAddr,
    expires_unix: u64,
) -> [u8; 32] {
    let mut data = Vec::with_capacity(cert_der.len() + 256);
    let peer_text = peer.to_string();
    let expiry = expires_unix.to_be_bytes();
    for part in [
        b"winremote-access-context-v1".as_slice(),
        endpoint.as_bytes(),
        cert_der,
        public_key,
        client_nonce,
        server_nonce,
        request_id,
        peer_text.as_bytes(),
        &expiry,
    ] {
        field(&mut data, part);
    }
    digest::digest(&digest::SHA256, &data)
        .as_ref()
        .try_into()
        .unwrap()
}
pub(crate) fn verification_code(context: &[u8; 32]) -> String {
    let mut data = Vec::new();
    field(&mut data, b"winremote-access-sas-v1");
    field(&mut data, context);
    let hash = digest::digest(&digest::SHA256, &data);
    let b = hash.as_ref();
    format!(
        "{:02X}{:02X}{:01X}-{:01X}{:02X}{:02X}",
        b[0],
        b[1],
        b[2] >> 4,
        b[2] & 15,
        b[3],
        b[4]
    )
}
pub(crate) fn proof_payload(
    context: &[u8; 32],
    id: &[u8; 16],
    challenge: &[u8; 32],
    expires_unix: u64,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(128);
    let expiry = expires_unix.to_be_bytes();
    for part in [
        b"winremote-access-proof-v1".as_slice(),
        b"poll",
        context,
        id,
        challenge,
        &expiry,
    ] {
        field(&mut data, part);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn manager() -> AccessManager {
        let manager = AccessManager::new(
            "https://192.0.2.10:8443".into(),
            vec![1, 2, 3],
            unix_now() + 600,
            Instant::now() + Duration::from_secs(600),
        );
        manager.set_enabled(true);
        manager
    }
    fn key(seed: u8) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
    }
    fn request(key: &Ed25519KeyPair, nonce: u8) -> BeginRequest {
        BeginRequest {
            client_public_key: URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
            client_nonce: URL_SAFE_NO_PAD.encode([nonce; 32]),
            label: Some("Codex".into()),
        }
    }
    fn signed_poll(
        manager: &AccessManager,
        peer: IpAddr,
        begin: &BeginResponse,
        key: &Ed25519KeyPair,
    ) -> PollRequest {
        let challenge = manager
            .challenge(
                peer,
                ChallengeRequest {
                    request_id: begin.request_id.clone(),
                },
            )
            .unwrap();
        let id = decode_exact::<16>(&begin.request_id).unwrap();
        let nonce = decode_exact::<32>(&begin.server_nonce).unwrap();
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let context = context_digest(
            &begin.endpoint,
            &[1, 2, 3],
            &public_key,
            &[7; 32],
            &nonce,
            &id,
            peer,
            begin.expires_unix,
        );
        let challenge_bytes = decode_exact::<32>(&challenge.server_challenge).unwrap();
        PollRequest {
            request_id: begin.request_id.clone(),
            server_challenge: challenge.server_challenge,
            signature: URL_SAFE_NO_PAD.encode(
                key.sign(&proof_payload(
                    &context,
                    &id,
                    &challenge_bytes,
                    challenge.expires_unix,
                ))
                .as_ref(),
            ),
        }
    }
    #[test]
    fn approved_request_requires_proof_and_is_consumed_once() {
        let manager = manager();
        let peer: IpAddr = "192.0.2.20".parse().unwrap();
        let key = key(9);
        let begin = manager.begin(peer, request(&key, 7)).unwrap();
        manager.decide(&begin.request_id, true).unwrap();
        let poll = signed_poll(&manager, peer, &begin, &key);
        assert_eq!(
            manager.poll(peer, poll.clone()).unwrap(),
            PollOutcome::Approved
        );
        assert_eq!(manager.poll(peer, poll).unwrap_err(), AccessError::NotFound);
        assert!(manager.pending_views().is_empty());
    }
    #[test]
    fn wrong_key_and_changed_challenge_cannot_redeem() {
        let manager = manager();
        let peer: IpAddr = "192.0.2.20".parse().unwrap();
        let key = key(9);
        let begin = manager.begin(peer, request(&key, 7)).unwrap();
        manager.decide(&begin.request_id, true).unwrap();
        let mut poll = signed_poll(&manager, peer, &begin, &key);
        poll.signature = URL_SAFE_NO_PAD.encode(self::key(8).sign(b"wrong key").as_ref());
        assert_eq!(
            manager.poll(peer, poll.clone()).unwrap_err(),
            AccessError::InvalidProof
        );
        assert_eq!(
            manager.poll(peer, poll).unwrap_err(),
            AccessError::InvalidProof
        );
    }
    #[test]
    fn code_and_proof_bind_all_context_fields() {
        let endpoint = "https://192.0.2.10:8443";
        let peer: IpAddr = "192.0.2.20".parse().unwrap();
        let original = context_digest(
            endpoint,
            &[1, 2, 3],
            &[4; 32],
            &[5; 32],
            &[6; 32],
            &[7; 16],
            peer,
            1234,
        );
        for changed in [
            context_digest(
                "https://192.0.2.11:8443",
                &[1, 2, 3],
                &[4; 32],
                &[5; 32],
                &[6; 32],
                &[7; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 4],
                &[4; 32],
                &[5; 32],
                &[6; 32],
                &[7; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[8; 32],
                &[5; 32],
                &[6; 32],
                &[7; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[4; 32],
                &[9; 32],
                &[6; 32],
                &[7; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[4; 32],
                &[5; 32],
                &[10; 32],
                &[7; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[4; 32],
                &[5; 32],
                &[6; 32],
                &[11; 16],
                peer,
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[4; 32],
                &[5; 32],
                &[6; 32],
                &[7; 16],
                "192.0.2.21".parse().unwrap(),
                1234,
            ),
            context_digest(
                endpoint,
                &[1, 2, 3],
                &[4; 32],
                &[5; 32],
                &[6; 32],
                &[7; 16],
                peer,
                1235,
            ),
        ] {
            assert_ne!(original, changed);
            assert_ne!(verification_code(&original), verification_code(&changed));
        }
        assert_ne!(
            proof_payload(&original, &[7; 16], &[12; 32], 1234),
            proof_payload(&original, &[7; 16], &[12; 32], 1235)
        );
    }
    #[test]
    fn disabled_and_expired_host_never_authorize() {
        let manager = manager();
        let peer: IpAddr = "192.0.2.20".parse().unwrap();
        let key = key(9);
        let begin = manager.begin(peer, request(&key, 7)).unwrap();
        manager.decide(&begin.request_id, true).unwrap();
        manager.set_enabled(false);
        assert!(manager.pending_views().is_empty());
        assert_eq!(
            manager
                .challenge(
                    peer,
                    ChallengeRequest {
                        request_id: begin.request_id
                    }
                )
                .unwrap_err(),
            AccessError::Disabled
        );
        let expired = AccessManager::new(
            "https://192.0.2.10:8443".into(),
            vec![1],
            unix_now() + 600,
            Instant::now(),
        );
        expired.set_enabled(true);
        assert_eq!(
            expired.begin(peer, request(&key, 7)).unwrap_err(),
            AccessError::Expired
        );
    }
    #[test]
    fn per_ip_rate_limit_survives_ui_toggles() {
        let manager = manager();
        let peer: IpAddr = "192.0.2.20".parse().unwrap();
        let key = key(9);
        for _ in 0..3 {
            manager.begin(peer, request(&key, 7)).unwrap();
            manager.set_enabled(false);
            manager.set_enabled(true);
        }
        assert_eq!(
            manager.begin(peer, request(&key, 7)).unwrap_err(),
            AccessError::RateLimited
        );
    }
    #[test]
    fn capacity_and_same_peer_are_bounded() {
        let manager = manager();
        let key = key(9);
        let first: IpAddr = "192.0.2.20".parse().unwrap();
        manager.begin(first, request(&key, 7)).unwrap();
        assert_eq!(
            manager.begin(first, request(&key, 7)).unwrap_err(),
            AccessError::Capacity
        );
        for last in 21..=23 {
            manager
                .begin(format!("192.0.2.{last}").parse().unwrap(), request(&key, 7))
                .unwrap();
        }
        assert_eq!(manager.pending_views().len(), 4);
        assert_eq!(
            manager
                .begin("192.0.2.24".parse().unwrap(), request(&key, 7))
                .unwrap_err(),
            AccessError::Capacity
        );
    }
    #[test]
    fn requests_get_distinct_nonces_and_peer_cannot_be_swapped() {
        let manager = manager();
        let key = key(9);
        let a: IpAddr = "192.0.2.20".parse().unwrap();
        let b: IpAddr = "192.0.2.21".parse().unwrap();
        let first = manager.begin(a, request(&key, 7)).unwrap();
        let second = manager.begin(b, request(&key, 7)).unwrap();
        assert_ne!(first.request_id, second.request_id);
        assert_ne!(first.server_nonce, second.server_nonce);
        assert_eq!(
            manager
                .challenge(
                    b,
                    ChallengeRequest {
                        request_id: first.request_id
                    }
                )
                .unwrap_err(),
            AccessError::NotFound
        );
    }
}
