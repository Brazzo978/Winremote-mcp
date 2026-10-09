use crate::desktop;
use crate::desktop_input;
use crate::fileshare;
use crate::pairing::{self, Challenge, ChallengeRequest, FinishRequest};
use crate::platform;
use crate::protocol::{
    Connection, DesktopClickRequest, DesktopInputResult, DesktopKeyRequest, DesktopPointRequest,
    DesktopScrollRequest, DesktopTypeRequest, DirectoryCreateRequest, DirectoryEntry,
    ExecuteRequest, ExecuteResult, FileCopyRequest, FileMoveRequest, FileStatResult, PathRequest,
    ReadChunkRequest, ReadFileResult, ScreenshotRequest, ScreenshotResult, TransferResult,
    UploadAbortRequest, UploadBeginRequest, UploadBeginResult, UploadChunkQuery,
    UploadCommitRequest, WriteFileRequest, MAX_FILE_BYTES, MAX_OUTPUT_BYTES, MAX_REQUEST_BYTES,
    MAX_TIMEOUT_SECS, MAX_TRANSFER_CHUNK_BYTES,
};
use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use rand::RngCore;
use serde::Serialize;
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::{watch, Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}
fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

struct HostState {
    token: String,
    pairing_code: String,
    endpoint: String,
    certificate_pem: String,
    pending: Mutex<HashMap<String, PendingPair>>,
    uploads: Arc<fileshare::UploadManager>,
    expires_unix: u64,
    deadline: Instant,
    shell: PathBuf,
    elevated: bool,
    semaphore: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
}
struct PendingPair {
    client_nonce: String,
    expires: Instant,
}
const MAX_PENDING_PAIRS: usize = 64;
const PAIR_TTL: Duration = Duration::from_secs(30);
impl HostState {
    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
            || unix_now() >= self.expires_unix
            || *self.shutdown.borrow()
    }
    async fn operation(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        if self.expired() {
            return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
        }
        let mut shutdown = self.shutdown.subscribe();
        if self.expired() {
            return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
        }
        let permit = tokio::select! {
            permit = self.semaphore.clone().acquire_owned() => permit.map_err(internal),
            _ = tokio::time::sleep_until(self.deadline.into()) => Err(ApiError(StatusCode::GONE, "invitation expired".into())),
            _ = shutdown.changed() => Err(ApiError(StatusCode::GONE, "host is shutting down".into())),
        }?;
        if self.expired() {
            return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
        }
        Ok(permit)
    }
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn authorization_status(
    state: &HostState,
    headers: &axum::http::HeaderMap,
) -> Result<(), StatusCode> {
    if state.expired() {
        return Err(StatusCode::GONE);
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let valid = supplied
        .map(|s| s.as_bytes().ct_eq(state.token.as_bytes()).unwrap_u8() == 1)
        .unwrap_or(false);
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}
async fn authenticate(
    State(state): State<Arc<HostState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    match authorization_status(&state, req.headers()) {
        Ok(()) => next.run(req).await,
        Err(status) => ApiError(
            status,
            status
                .canonical_reason()
                .unwrap_or("request rejected")
                .into(),
        )
        .into_response(),
    }
}
async fn public_guard(
    State(state): State<Arc<HostState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.expired() {
        return ApiError(StatusCode::GONE, "invitation expired".into()).into_response();
    }
    if req.headers().contains_key(header::ORIGIN) {
        return ApiError(StatusCode::FORBIDDEN, "Origin is not accepted".into()).into_response();
    }
    next.run(req).await
}

async fn pair_challenge(
    State(state): State<Arc<HostState>>,
    Json(req): Json<ChallengeRequest>,
) -> Result<Json<Challenge>, ApiError> {
    pairing::validate_nonce(&req.client_nonce).map_err(|e| bad(e.to_string()))?;
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let mut pending = state.pending.lock().await;
    let now = Instant::now();
    pending.retain(|_, pair| pair.expires > now);
    if pending.len() >= MAX_PENDING_PAIRS {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "pairing capacity reached".into(),
        ));
    }
    let server_nonce = pairing::new_nonce();
    let mut challenge = Challenge {
        version: 1,
        endpoint: state.endpoint.clone(),
        certificate_pem: state.certificate_pem.clone(),
        expires_unix: state.expires_unix,
        server_nonce: server_nonce.clone(),
        proof: String::new(),
    };
    challenge.proof = pairing::server_proof(&state.pairing_code, &req.client_nonce, &challenge)
        .map_err(internal)?;
    pending.insert(
        server_nonce,
        PendingPair {
            client_nonce: req.client_nonce,
            expires: (now + PAIR_TTL).min(state.deadline),
        },
    );
    Ok(Json(challenge))
}

async fn pair_finish(
    State(state): State<Arc<HostState>>,
    Json(req): Json<FinishRequest>,
) -> Result<Json<Connection>, ApiError> {
    pairing::validate_nonce(&req.server_nonce)
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "invalid pairing proof".into()))?;
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let pair = {
        let mut pending = state.pending.lock().await;
        let pair = pending.remove(&req.server_nonce);
        let now = Instant::now();
        pending.retain(|_, item| item.expires > now);
        pair
    }
    .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "invalid pairing proof".into()))?;
    if Instant::now() >= pair.expires || state.expired() {
        return Err(ApiError(StatusCode::GONE, "pairing expired".into()));
    }
    let nonce_matches = req
        .client_nonce
        .as_bytes()
        .ct_eq(pair.client_nonce.as_bytes())
        .unwrap_u8()
        == 1;
    if !nonce_matches {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "invalid pairing proof".into(),
        ));
    }
    let challenge = Challenge {
        version: 1,
        endpoint: state.endpoint.clone(),
        certificate_pem: state.certificate_pem.clone(),
        expires_unix: state.expires_unix,
        server_nonce: req.server_nonce,
        proof: String::new(),
    };
    pairing::verify_client(
        &state.pairing_code,
        &req.client_nonce,
        &challenge,
        &req.proof,
    )
    .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "invalid pairing proof".into()))?;
    Ok(Json(Connection {
        version: 1,
        endpoint: state.endpoint.clone(),
        token: state.token.clone(),
        certificate_pem: state.certificate_pem.clone(),
        expires_unix: state.expires_unix,
    }))
}
#[derive(Serialize)]
struct Info {
    user: String,
    elevated: bool,
    os: &'static str,
    shell: String,
    capabilities: [&'static str; 16],
    expires_unix: u64,
    native_exit_semantics: &'static str,
}
async fn info(State(state): State<Arc<HostState>>) -> Json<Info> {
    Json(Info { user: platform::effective_user(), elevated: state.elevated, os: "Windows", shell: state.shell.to_string_lossy().into_owned(), capabilities: ["execute", "files.read", "files.write", "files.list", "desktop.screenshot", "files.stat", "files.read-chunk", "files.upload", "files.copy", "files.move", "files.mkdir", "desktop.move", "desktop.click", "desktop.scroll", "desktop.type", "desktop.key"], expires_unix: state.expires_unix, native_exit_semantics: "The final native process exit code is propagated; terminating PowerShell errors exit 1." })
}

async fn execute(
    State(state): State<Arc<HostState>>,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<ExecuteResult>, ApiError> {
    let _permit = state.operation().await?;
    if req.script.len() > 8 * 1024 {
        return Err(bad(
            "script exceeds the Windows command-line limit (8 KiB maximum)",
        ));
    }
    if req.timeout_secs == 0 || req.timeout_secs > MAX_TIMEOUT_SECS {
        return Err(bad("timeout_secs must be between 1 and 120"));
    }
    let cwd = platform::ensure_absolute_directory(&req.cwd).map_err(|e| bad(e.to_string()))?;
    let remaining = state.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let timeout = Duration::from_secs(req.timeout_secs).min(remaining);
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(cancelled.clone());
    let shell = state.shell.clone();
    let script = req.script;
    let mut task = tokio::task::spawn_blocking(move || {
        process::run(&shell, &cwd, &script, timeout, cancelled)
    });
    let mut shutdown = state.shutdown.subscribe();
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let result = tokio::select! {
        result = &mut task => result.map_err(internal)?.map_err(internal)?,
        _ = tokio::time::sleep_until(state.deadline.into()) => return Err(ApiError(StatusCode::GONE, "invitation expired".into())),
        _ = shutdown.changed() => return Err(ApiError(StatusCode::GONE, "host is shutting down".into())),
    };
    drop(guard);
    Ok(Json(result))
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn file_read(
    State(state): State<Arc<HostState>>,
    Json(req): Json<PathRequest>,
) -> Result<Json<ReadFileResult>, ApiError> {
    let _permit = state.operation().await?;
    let path = absolute_path(&req.path)?;
    let file = tokio::fs::File::open(&path).await.map_err(internal)?;
    use tokio::io::AsyncReadExt;
    let mut data = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .await
        .map_err(internal)?;
    if data.len() > MAX_FILE_BYTES {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds 2 MiB".into(),
        ));
    }
    Ok(Json(ReadFileResult {
        path: path.to_string_lossy().into_owned(),
        content_base64: STANDARD.encode(&data),
        size_bytes: data.len(),
    }))
}
#[derive(Serialize)]
struct WriteResult {
    path: String,
    size_bytes: usize,
}
async fn file_write(
    State(state): State<Arc<HostState>>,
    Json(req): Json<WriteFileRequest>,
) -> Result<Json<WriteResult>, ApiError> {
    let _permit = state.operation().await?;
    let path = absolute_path(&req.path)?;
    if req.content_base64.len() > MAX_FILE_BYTES.div_ceil(3) * 4 + 8 {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds 2 MiB".into(),
        ));
    }
    let data = STANDARD
        .decode(&req.content_base64)
        .map_err(|_| bad("invalid base64 content"))?;
    if data.len() > MAX_FILE_BYTES {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds 2 MiB".into(),
        ));
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true);
    if req.overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options.open(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            ApiError(StatusCode::CONFLICT, "file exists".into())
        } else {
            internal(e)
        }
    })?;
    use tokio::io::AsyncWriteExt;
    file.write_all(&data).await.map_err(internal)?;
    file.flush().await.map_err(internal)?;
    Ok(Json(WriteResult {
        path: path.to_string_lossy().into_owned(),
        size_bytes: data.len(),
    }))
}
#[derive(Serialize)]
struct ListResult {
    entries: Vec<DirectoryEntry>,
    truncated: bool,
}
async fn file_list(
    State(state): State<Arc<HostState>>,
    Json(req): Json<PathRequest>,
) -> Result<Json<ListResult>, ApiError> {
    let _permit = state.operation().await?;
    let path = absolute_path(&req.path)?;
    let mut dir = tokio::fs::read_dir(&path).await.map_err(internal)?;
    let mut entries = Vec::new();
    while let Some(entry) = dir.next_entry().await.map_err(internal)? {
        if entries.len() == 1000 {
            return Ok(Json(ListResult {
                entries,
                truncated: true,
            }));
        }
        let meta = entry.metadata().await.map_err(internal)?;
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size_bytes: if meta.is_file() { meta.len() } else { 0 },
        });
    }
    Ok(Json(ListResult {
        entries,
        truncated: false,
    }))
}
fn absolute_path(path: &str) -> Result<PathBuf, ApiError> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err(bad("path must be absolute"));
    }
    Ok(p)
}

async fn screenshot(
    State(state): State<Arc<HostState>>,
    Json(req): Json<ScreenshotRequest>,
) -> Result<Json<ScreenshotResult>, ApiError> {
    let _permit = state.operation().await?;
    if !(320..=3840).contains(&req.max_width) {
        return Err(bad("max_width must be between 320 and 3840"));
    }
    let mut shutdown = state.shutdown.subscribe();
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let mut task = tokio::task::spawn_blocking(move || desktop::capture(&req));
    let result = tokio::select! {
        result = &mut task => result.map_err(internal)?.map_err(internal)?,
        _ = tokio::time::sleep_until(state.deadline.into()) => return Err(ApiError(StatusCode::GONE, "invitation expired".into())),
        _ = shutdown.changed() => return Err(ApiError(StatusCode::GONE, "host is shutting down".into())),
    };
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    Ok(Json(result))
}

// Keep the operation permit inside the blocking task so a dropped HTTP request
// cannot allow another input operation to overlap with an injection in flight.
async fn input_job(
    state: Arc<HostState>,
    job: impl FnOnce() -> anyhow::Result<DesktopInputResult> + Send + 'static,
) -> Result<Json<DesktopInputResult>, ApiError> {
    let permit = state.operation().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if state.expired() {
            return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
        }
        let result = job();
        if state.expired() {
            return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
        }
        result.map(Json).map_err(internal)
    })
    .await
    .map_err(internal)?
}
async fn desktop_move(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DesktopPointRequest>,
) -> Result<Json<DesktopInputResult>, ApiError> {
    desktop_input::validate_point(&req).map_err(|e| bad(e.to_string()))?;
    input_job(state, move || desktop_input::move_pointer(&req)).await
}
async fn desktop_click(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DesktopClickRequest>,
) -> Result<Json<DesktopInputResult>, ApiError> {
    desktop_input::validate_click(&req).map_err(|e| bad(e.to_string()))?;
    input_job(state, move || desktop_input::click(&req)).await
}
async fn desktop_scroll(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DesktopScrollRequest>,
) -> Result<Json<DesktopInputResult>, ApiError> {
    desktop_input::validate_scroll(&req).map_err(|e| bad(e.to_string()))?;
    input_job(state, move || desktop_input::scroll(&req)).await
}
async fn desktop_type(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DesktopTypeRequest>,
) -> Result<Json<DesktopInputResult>, ApiError> {
    desktop_input::validate_type(&req).map_err(|e| bad(e.to_string()))?;
    let action_state = state.clone();
    input_job(state, move || {
        desktop_input::type_text_checked(&req, || !action_state.expired())
    })
    .await
}
async fn desktop_key(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DesktopKeyRequest>,
) -> Result<Json<DesktopInputResult>, ApiError> {
    desktop_input::validate_key(&req).map_err(|e| bad(e.to_string()))?;
    input_job(state, move || desktop_input::key(&req)).await
}

fn file_error(error: fileshare::FileError) -> ApiError {
    ApiError(error.status, error.message)
}
async fn file_job<T: Send + 'static>(
    job: impl FnOnce() -> fileshare::FileResult<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(internal)?
        .map_err(file_error)
}
async fn fs_stat(
    State(state): State<Arc<HostState>>,
    Json(req): Json<PathRequest>,
) -> Result<Json<FileStatResult>, ApiError> {
    let _permit = state.operation().await?;
    Ok(Json(file_job(move || fileshare::stat(&req)).await?))
}
async fn fs_read_chunk(
    State(state): State<Arc<HostState>>,
    Json(req): Json<ReadChunkRequest>,
) -> Result<Response, ApiError> {
    let _permit = state.operation().await?;
    let data = file_job(move || fileshare::read_chunk(&req)).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Bytes::from(data),
    )
        .into_response())
}
async fn fs_upload_begin(
    State(state): State<Arc<HostState>>,
    Json(req): Json<UploadBeginRequest>,
) -> Result<Json<UploadBeginResult>, ApiError> {
    let _permit = state.operation().await?;
    let uploads = state.uploads.clone();
    Ok(Json(file_job(move || uploads.begin(req)).await?))
}
async fn fs_upload_chunk(
    State(state): State<Arc<HostState>>,
    Query(query): Query<UploadChunkQuery>,
    bytes: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _permit = state.operation().await?;
    if bytes.is_empty() || bytes.len() > MAX_TRANSFER_CHUNK_BYTES as usize {
        return Err(bad("chunk must contain 1 byte to 1 MiB"));
    }
    let uploads = state.uploads.clone();
    let received = file_job(move || uploads.chunk(&query, &bytes)).await?;
    Ok(Json(serde_json::json!({"received_bytes":received})))
}
async fn fs_upload_commit(
    State(state): State<Arc<HostState>>,
    Json(req): Json<UploadCommitRequest>,
) -> Result<Json<TransferResult>, ApiError> {
    let _permit = state.operation().await?;
    let uploads = state.uploads.clone();
    Ok(Json(file_job(move || uploads.commit(req)).await?))
}
async fn fs_upload_abort(
    State(state): State<Arc<HostState>>,
    Json(req): Json<UploadAbortRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _permit = state.operation().await?;
    let uploads = state.uploads.clone();
    let aborted = file_job(move || uploads.abort(req)).await?;
    Ok(Json(serde_json::json!({"aborted":aborted})))
}
async fn fs_copy(
    State(state): State<Arc<HostState>>,
    Json(req): Json<FileCopyRequest>,
) -> Result<Json<fileshare::CopyResult>, ApiError> {
    let _permit = state.operation().await?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(cancelled.clone());
    let mut task = tokio::task::spawn_blocking(move || fileshare::copy(req, &cancelled));
    let mut shutdown = state.shutdown.subscribe();
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let result = tokio::select! {
        result=&mut task => result.map_err(internal)?.map_err(file_error)?,
        _=tokio::time::sleep(Duration::from_secs(120)) => return Err(ApiError(StatusCode::REQUEST_TIMEOUT,"copy exceeded 120 seconds".into())),
        _=tokio::time::sleep_until(state.deadline.into()) => return Err(ApiError(StatusCode::GONE,"invitation expired".into())),
        _=shutdown.changed() => return Err(ApiError(StatusCode::GONE,"host is shutting down".into())),
    };
    drop(guard);
    Ok(Json(result))
}
async fn fs_move(
    State(state): State<Arc<HostState>>,
    Json(req): Json<FileMoveRequest>,
) -> Result<Json<fileshare::MoveResult>, ApiError> {
    let _permit = state.operation().await?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(cancelled.clone());
    let mut task = tokio::task::spawn_blocking(move || fileshare::move_entry(req, &cancelled));
    let mut shutdown = state.shutdown.subscribe();
    if state.expired() {
        return Err(ApiError(StatusCode::GONE, "invitation expired".into()));
    }
    let result = tokio::select! {
        result=&mut task => result.map_err(internal)?.map_err(file_error)?,
        _=tokio::time::sleep(Duration::from_secs(120)) => return Err(ApiError(StatusCode::REQUEST_TIMEOUT,"move exceeded 120 seconds".into())),
        _=tokio::time::sleep_until(state.deadline.into()) => return Err(ApiError(StatusCode::GONE,"invitation expired".into())),
        _=shutdown.changed() => return Err(ApiError(StatusCode::GONE,"host is shutting down".into())),
    };
    drop(guard);
    Ok(Json(result))
}
async fn fs_mkdir(
    State(state): State<Arc<HostState>>,
    Json(req): Json<DirectoryCreateRequest>,
) -> Result<Json<fileshare::MkdirResult>, ApiError> {
    let _permit = state.operation().await?;
    Ok(Json(file_job(move || fileshare::mkdir(req)).await?))
}
fn router(state: Arc<HostState>) -> Router {
    let protected = Router::new()
        .route("/v1/info", get(info))
        .route("/v1/execute", post(execute))
        .route("/v1/files/read", post(file_read))
        .route("/v1/files/write", post(file_write))
        .route("/v1/files/list", post(file_list))
        .route("/v1/files/stat", post(fs_stat))
        .route("/v1/files/read-chunk", post(fs_read_chunk))
        .route("/v1/files/upload/begin", post(fs_upload_begin))
        .route(
            "/v1/files/upload/chunk",
            put(fs_upload_chunk).layer(DefaultBodyLimit::max(MAX_TRANSFER_CHUNK_BYTES as usize)),
        )
        .route("/v1/files/upload/commit", post(fs_upload_commit))
        .route("/v1/files/upload/abort", post(fs_upload_abort))
        .route("/v1/files/copy", post(fs_copy))
        .route("/v1/files/move", post(fs_move))
        .route("/v1/files/mkdir", post(fs_mkdir))
        .route("/v1/desktop/screenshot", post(screenshot))
        .route("/v1/desktop/move", post(desktop_move))
        .route("/v1/desktop/click", post(desktop_click))
        .route("/v1/desktop/scroll", post(desktop_scroll))
        .route("/v1/desktop/type", post(desktop_type))
        .route("/v1/desktop/key", post(desktop_key))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));
    let pairing = Router::new()
        .route("/v1/pair/challenge", post(pair_challenge))
        .route("/v1/pair/finish", post(pair_finish))
        .layer(DefaultBodyLimit::max(4096));
    protected
        .merge(pairing)
        .layer(middleware::from_fn_with_state(state.clone(), public_guard))
        .with_state(state)
}
pub async fn run(bind: SocketAddr, ttl_secs: u64, require_admin: bool) -> Result<()> {
    anyhow::ensure!(
        ttl_secs > 0 && ttl_secs <= 24 * 3600,
        "ttl_secs must be between 1 and 86400"
    );
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let elevated = platform::is_elevated()?;
    anyhow::ensure!(
        !require_admin || elevated,
        "host must be launched from an elevated terminal"
    );
    let shell = platform::choose_shell()?;
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let token = URL_SAFE_NO_PAD.encode(secret);
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(bind.ip()));
    let key = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    let cert_pem = cert.pem();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert_pem.as_bytes().to_vec(),
        key.serialize_pem().into_bytes(),
    )
    .await?;
    let expires_unix = unix_now()
        .checked_add(ttl_secs)
        .context("expiration overflow")?;
    let listener = std::net::TcpListener::bind(bind).with_context(|| format!("bind {bind}"))?;
    listener.set_nonblocking(true)?;
    let actual = listener.local_addr()?;
    let endpoint = format!("https://{actual}");
    let pairing_code = pairing::generate_code();
    let invitation = match actual {
        SocketAddr::V4(addr) if addr.port() == 8443 => format!("{}:{pairing_code}", addr.ip()),
        SocketAddr::V4(addr) => format!("{}:{}:{pairing_code}", addr.ip(), addr.port()),
        SocketAddr::V6(addr) => format!("[{}]:{}:{pairing_code}", addr.ip(), addr.port()),
    };
    let (shutdown_tx, _) = watch::channel(false);
    let state = Arc::new(HostState {
        token,
        pairing_code,
        endpoint,
        certificate_pem: cert_pem,
        pending: Mutex::new(HashMap::new()),
        uploads: Arc::new(fileshare::UploadManager::new()),
        expires_unix,
        deadline: Instant::now() + Duration::from_secs(ttl_secs),
        shell,
        elevated,
        semaphore: Arc::new(Semaphore::new(1)),
        shutdown: shutdown_tx,
    });
    let cleanup_uploads = state.uploads.clone();
    let mut cleanup_shutdown = state.shutdown.subscribe();
    let cleanup_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = interval.tick() => cleanup_uploads.cleanup_expired(),
                _ = cleanup_shutdown.changed() => break,
            }
        }
    });
    let routes = router(state.clone());
    let handle = axum_server::Handle::new();
    let server = axum_server::from_tcp_rustls(listener, tls)
        .handle(handle.clone())
        .serve(routes.into_make_service());
    println!("{invitation}");
    tokio::pin!(server);
    let outcome = tokio::select! {
        result = &mut server => result.context("HTTPS server failed"),
        _ = tokio::time::sleep_until(state.deadline.into()) => {
            state.shutdown.send_replace(true);
            handle.shutdown();
            server.await.context("HTTPS server shutdown failed")
        },
        _ = tokio::signal::ctrl_c() => {
            state.shutdown.send_replace(true);
            handle.shutdown();
            server.await.context("HTTPS server shutdown failed")
        },
    };
    state.shutdown.send_replace(true);
    let _ = cleanup_task.await;
    state.uploads.abort_all();
    outcome?;
    Ok(())
}

#[cfg(not(windows))]
mod process {
    use super::*;
    pub fn run(
        _: &std::path::Path,
        _: &std::path::Path,
        _: &str,
        _: Duration,
        _: Arc<AtomicBool>,
    ) -> Result<ExecuteResult> {
        anyhow::bail!("Windows host is supported only on Windows")
    }
}
#[cfg(windows)]
mod process {
    use super::*;
    use std::{
        ffi::c_void,
        io::Read,
        mem::{size_of, zeroed},
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        path::Path,
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Pipes::CreatePipe,
            Threading::{
                CreateProcessW, GetExitCodeProcess, ResumeThread, TerminateProcess,
                WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION,
                STARTF_USESTDHANDLES, STARTUPINFOW,
            },
        },
    };

    struct Handle(HANDLE);
    unsafe impl Send for Handle {}
    impl Handle {
        fn new(raw: HANDLE) -> Result<Self> {
            if raw.is_null() {
                anyhow::bail!("Windows handle creation failed: {}", unsafe {
                    GetLastError()
                });
            }
            Ok(Self(raw))
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }
    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
    fn pipe() -> Result<(Handle, Handle)> {
        unsafe {
            let mut attrs: SECURITY_ATTRIBUTES = zeroed();
            attrs.nLength = size_of::<SECURITY_ATTRIBUTES>() as u32;
            attrs.bInheritHandle = 1;
            let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
            if CreatePipe(&mut read, &mut write, &attrs, 0) == 0 {
                anyhow::bail!("CreatePipe failed: {}", GetLastError());
            }
            let read = Handle::new(read)?;
            let write = Handle::new(write)?;
            if SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) == 0 {
                anyhow::bail!("SetHandleInformation failed: {}", GetLastError());
            }
            Ok((read, write))
        }
    }
    fn stdin_pipe() -> Result<(Handle, Handle)> {
        unsafe {
            let mut attrs: SECURITY_ATTRIBUTES = zeroed();
            attrs.nLength = size_of::<SECURITY_ATTRIBUTES>() as u32;
            attrs.bInheritHandle = 1;
            let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
            if CreatePipe(&mut read, &mut write, &attrs, 0) == 0 {
                anyhow::bail!("CreatePipe(stdin) failed: {}", GetLastError());
            }
            let read = Handle::new(read)?;
            let write = Handle::new(write)?;
            if SetHandleInformation(write.0, HANDLE_FLAG_INHERIT, 0) == 0 {
                anyhow::bail!("SetHandleInformation(stdin) failed: {}", GetLastError());
            }
            Ok((read, write))
        }
    }
    fn drain(handle: Handle) -> (String, bool) {
        let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
        std::mem::forget(handle);
        let mut output = Vec::with_capacity(8192);
        let mut truncated = false;
        let mut chunk = [0u8; 8192];
        loop {
            match file.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let keep = (MAX_OUTPUT_BYTES - output.len()).min(n);
                    output.extend_from_slice(&chunk[..keep]);
                    if keep < n {
                        truncated = true;
                    }
                }
            }
        }
        let mut text = String::from_utf8_lossy(&output).into_owned();
        if text.len() > MAX_OUTPUT_BYTES {
            let mut end = MAX_OUTPUT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        (text, truncated)
    }
    fn encoded(script: &str) -> String {
        use base64::engine::general_purpose::STANDARD;
        let wrapped = format!("[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); $OutputEncoding = [Console]::OutputEncoding; $ErrorActionPreference = 'Stop'; try {{ & {{\n{script}\n}}; if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }} }} catch {{ [Console]::Error.WriteLine($_.ToString()); exit 1 }}");
        let bytes: Vec<u8> = wrapped.encode_utf16().flat_map(u16::to_le_bytes).collect();
        STANDARD.encode(bytes)
    }
    pub fn run(
        shell: &Path,
        cwd: &Path,
        script: &str,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ExecuteResult> {
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        let (stdin_read, stdin_write) = stdin_pipe()?;
        let job = Handle::new(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        unsafe {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                anyhow::bail!("SetInformationJobObject failed: {}", GetLastError());
            }
        }
        let command = format!(
            "\"{}\" -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
            shell.display(),
            encoded(script)
        );
        let mut command_w = wide(std::ffi::OsStr::new(&command));
        let shell_w = wide(shell.as_os_str());
        let cwd_w = wide(cwd.as_os_str());
        let mut startup: STARTUPINFOW = unsafe { zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        startup.dwFlags = STARTF_USESTDHANDLES;
        startup.hStdInput = stdin_read.0;
        startup.hStdOutput = stdout_write.0;
        startup.hStdError = stderr_write.0;
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        unsafe {
            if CreateProcessW(
                shell_w.as_ptr(),
                command_w.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                CREATE_SUSPENDED | CREATE_NO_WINDOW,
                ptr::null(),
                cwd_w.as_ptr(),
                &startup,
                &mut info,
            ) == 0
            {
                anyhow::bail!("CreateProcessW failed: {}", GetLastError());
            }
        }
        let process = Handle::new(info.hProcess)?;
        let thread = Handle::new(info.hThread)?;
        unsafe {
            if AssignProcessToJobObject(job.0, process.0) == 0 {
                let code = GetLastError();
                TerminateProcess(process.0, 1);
                anyhow::bail!("AssignProcessToJobObject failed: {code}");
            }
            if ResumeThread(thread.0) == u32::MAX {
                let code = GetLastError();
                TerminateJobObject(job.0, 1);
                anyhow::bail!("ResumeThread failed: {code}");
            }
        }
        drop(thread);
        drop(stdout_write);
        drop(stderr_write);
        drop(stdin_read);
        drop(stdin_write);
        let stdout_task = std::thread::spawn(move || drain(stdout_read));
        let stderr_task = std::thread::spawn(move || drain(stderr_read));
        let started = Instant::now();
        let mut timed_out = false;
        let mut exit_code = None;
        loop {
            if cancelled.load(Ordering::SeqCst) || started.elapsed() >= timeout {
                timed_out = true;
                unsafe {
                    TerminateJobObject(job.0, 1);
                }
                break;
            }
            let wait = unsafe { WaitForSingleObject(process.0, 50) };
            if wait == WAIT_OBJECT_0 {
                let mut code = 0u32;
                if unsafe { GetExitCodeProcess(process.0, &mut code) } != 0 {
                    exit_code = Some(code as i32);
                }
                break;
            }
            if wait != WAIT_TIMEOUT {
                unsafe {
                    TerminateJobObject(job.0, 1);
                }
                anyhow::bail!("WaitForSingleObject failed: {}", unsafe { GetLastError() });
            }
        }
        // A script may have started descendants. End the whole job before waiting for pipe EOF.
        unsafe {
            TerminateJobObject(job.0, 1);
        }
        let (stdout, out_truncated) = stdout_task
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))?;
        let (stderr, err_truncated) = stderr_task
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
        Ok(ExecuteResult {
            stdout,
            stderr,
            exit_code,
            timed_out,
            output_truncated: out_truncated || err_truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use tower::ServiceExt;

    fn state(expired: bool) -> Arc<HostState> {
        let (shutdown, _) = watch::channel(false);
        Arc::new(HostState {
            token: "test-token".into(),
            pairing_code: "Abcdefgh12345!@#".into(),
            endpoint: "https://127.0.0.1:8443".into(),
            certificate_pem: "test-certificate".into(),
            pending: Mutex::new(HashMap::new()),
            uploads: Arc::new(fileshare::UploadManager::new()),
            expires_unix: if expired {
                unix_now() - 1
            } else {
                unix_now() + 60
            },
            deadline: Instant::now() + Duration::from_secs(if expired { 0 } else { 60 }),
            shell: PathBuf::new(),
            elevated: false,
            semaphore: Arc::new(Semaphore::new(1)),
            shutdown,
        })
    }
    async fn status(state: Arc<HostState>, auth: Option<&str>, origin: bool) -> StatusCode {
        let mut req = Request::builder().method(Method::GET).uri("/v1/info");
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        if origin {
            req = req.header(header::ORIGIN, "https://example.invalid");
        }
        router(state)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }
    #[tokio::test]
    async fn authentication_rejects_missing_wrong_expired_and_browser_origin() {
        assert_eq!(
            status(state(false), None, false).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(state(false), Some("Bearer incorrect"), false).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(state(false), Some("Bearer test-token"), true).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(state(true), Some("Bearer test-token"), false).await,
            StatusCode::GONE
        );
        assert_eq!(
            status(state(false), Some("Bearer test-token"), false).await,
            StatusCode::OK
        );
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/files/write")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router(state(false)).oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/desktop/screenshot")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router(state(false)).oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn desktop_input_authentication_and_validation() {
        use serde_json::json;
        for path in [
            "/v1/desktop/move",
            "/v1/desktop/click",
            "/v1/desktop/scroll",
            "/v1/desktop/type",
            "/v1/desktop/key",
        ] {
            for (token, origin, expired, expected) in [
                (false, false, false, StatusCode::UNAUTHORIZED),
                (true, true, false, StatusCode::FORBIDDEN),
                (true, false, true, StatusCode::GONE),
            ] {
                let mut builder = Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json");
                if token {
                    builder = builder.header(header::AUTHORIZATION, "Bearer test-token");
                }
                if origin {
                    builder = builder.header(header::ORIGIN, "https://example.invalid");
                }
                let req = builder.body(Body::from("{}")).unwrap();
                assert_eq!(
                    router(state(expired)).oneshot(req).await.unwrap().status(),
                    expected,
                    "{path}"
                );
            }
        }
        for (path, body) in [
            ("/v1/desktop/click", json!({"x": 0,"y":0,"clicks":3})),
            ("/v1/desktop/scroll", json!({})),
            ("/v1/desktop/scroll", json!({"vertical":101})),
            ("/v1/desktop/type", json!({"text":""})),
            ("/v1/desktop/type", json!({"text":"a\u{0000}b"})),
            ("/v1/desktop/key", json!({"keys":["CTRL"]})),
            ("/v1/desktop/key", json!({"keys":["A", "B"]})),
        ] {
            let req = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap();
            assert_eq!(
                router(state(false)).oneshot(req).await.unwrap().status(),
                StatusCode::BAD_REQUEST,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn transfer_routes_require_bearer() {
        for path in [
            "/v1/files/stat",
            "/v1/files/read-chunk",
            "/v1/files/upload/begin",
            "/v1/files/upload/commit",
            "/v1/files/upload/abort",
            "/v1/files/copy",
            "/v1/files/move",
            "/v1/files/mkdir",
        ] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                router(state(false))
                    .oneshot(request)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        let request = Request::builder()
            .method(Method::PUT)
            .uri("/v1/files/upload/chunk?upload_id=test&offset=0")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router(state(false))
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn pairing_consumes_challenge_and_never_exposes_token_without_proof() {
        let state = state(false);
        let client_nonce = pairing::new_nonce();
        let challenge = pair_challenge(
            State(state.clone()),
            Json(ChallengeRequest {
                client_nonce: client_nonce.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(state.pending.lock().await.len(), 1);
        assert!(!serde_json::to_string(&challenge)
            .unwrap()
            .contains(&state.token));
        pairing::verify_server(&state.pairing_code, &client_nonce, &challenge).unwrap();
        let proof = pairing::client_proof(&state.pairing_code, &client_nonce, &challenge).unwrap();
        let finish = FinishRequest {
            client_nonce: client_nonce.clone(),
            server_nonce: challenge.server_nonce.clone(),
            proof: proof.clone(),
        };
        let connection = pair_finish(State(state.clone()), Json(finish.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(connection.token, state.token);
        assert_eq!(state.pending.lock().await.len(), 0);
        assert_eq!(
            pair_finish(State(state.clone()), Json(finish))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::UNAUTHORIZED
        );
        let challenge = pair_challenge(
            State(state.clone()),
            Json(ChallengeRequest {
                client_nonce: client_nonce.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        let wrong = FinishRequest {
            client_nonce: client_nonce.clone(),
            server_nonce: challenge.server_nonce.clone(),
            proof: "invalid".into(),
        };
        assert_eq!(
            pair_finish(State(state.clone()), Json(wrong))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::UNAUTHORIZED
        );
        let retry = FinishRequest {
            client_nonce: client_nonce.clone(),
            server_nonce: challenge.server_nonce.clone(),
            proof: pairing::client_proof(&state.pairing_code, &client_nonce, &challenge).unwrap(),
        };
        assert_eq!(
            pair_finish(State(state.clone()), Json(retry))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn pairing_rejects_origin_expiry_and_excess_pending_challenges() {
        let live_state = state(false);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/pair/challenge")
            .header(header::ORIGIN, "https://example.invalid")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({"client_nonce":pairing::new_nonce()}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            router(live_state.clone())
                .oneshot(req)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/pair/challenge")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({"client_nonce":pairing::new_nonce()}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            router(state(true)).oneshot(req).await.unwrap().status(),
            StatusCode::GONE
        );
        for _ in 0..MAX_PENDING_PAIRS {
            let _ = pair_challenge(
                State(live_state.clone()),
                Json(ChallengeRequest {
                    client_nonce: pairing::new_nonce(),
                }),
            )
            .await
            .unwrap();
        }
        assert_eq!(live_state.pending.lock().await.len(), MAX_PENDING_PAIRS);
        assert_eq!(
            pair_challenge(
                State(live_state),
                Json(ChallengeRequest {
                    client_nonce: pairing::new_nonce()
                })
            )
            .await
            .err()
            .unwrap()
            .0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    #[tokio::test]
    async fn write_without_overwrite_preserves_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        tokio::fs::write(&path, b"original").await.unwrap();
        let req = WriteFileRequest {
            path: path.to_string_lossy().into_owned(),
            content_base64: STANDARD.encode(b"replacement"),
            overwrite: false,
        };
        let err = file_write(State(state(false)), Json(req))
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"original");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_output_errors_and_trailing_comments() {
        let shell = platform::choose_shell().unwrap();
        let cwd = std::env::temp_dir().canonicalize().unwrap();
        let run = |script: &str| {
            process::run(
                &shell,
                &cwd,
                script,
                Duration::from_secs(10),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
        };
        let result = run("Write-Output 'café 世界' # trailing comment");
        assert!(result.stdout.contains("café 世界"), "{:?}", result.stdout);
        assert_eq!(result.exit_code, Some(0));
        let result = run("Write-Error 'expected failure'");
        assert_eq!(result.exit_code, Some(1));
        assert!(result.stderr.contains("expected failure"));
        let result = run("cmd.exe /c exit 7");
        assert_eq!(result.exit_code, Some(7));
    }
    #[cfg(windows)]
    #[test]
    fn powershell_timeout_terminates_job() {
        let shell = platform::choose_shell().unwrap();
        let cwd = std::env::temp_dir().canonicalize().unwrap();
        let result = process::run(
            &shell,
            &cwd,
            "Start-Sleep -Seconds 10",
            Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.timed_out);
        assert!(result.exit_code.is_none());
    }
    #[cfg(windows)]
    #[test]
    fn powershell_output_is_drained_and_bounded() {
        let shell = platform::choose_shell().unwrap();
        let cwd = std::env::temp_dir().canonicalize().unwrap();
        let result = process::run(
            &shell,
            &cwd,
            "[Console]::Out.Write(('x' * 1050000))",
            Duration::from_secs(10),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output_truncated);
        assert_eq!(result.stdout.len(), MAX_OUTPUT_BYTES);
    }
}
