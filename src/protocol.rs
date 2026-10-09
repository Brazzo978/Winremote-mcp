use serde::{Deserialize, Serialize};

pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TIMEOUT_SECS: u64 = 120;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub version: u32,
    pub endpoint: String,
    pub token: String,
    pub certificate_pem: String,
    pub expires_unix: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub script: String,
    pub cwd: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}
fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRequest {
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileRequest {
    pub path: String,
    pub content_base64: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub path: String,
    pub content_base64: String,
    pub size_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
}

pub const MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotRequest {
    #[serde(default = "default_screenshot_width")]
    pub max_width: u32,
}
fn default_screenshot_width() -> u32 {
    1920
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotResult {
    pub mime_type: String,
    pub content_base64: String,
    pub width: u32,
    pub height: u32,
    pub desktop_x: i32,
    pub desktop_y: i32,
    pub desktop_width: u32,
    pub desktop_height: u32,
    pub captured_unix: u64,
}

pub const MAX_TRANSFER_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const MAX_TRANSFER_CHUNK_BYTES: u32 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileStatResult {
    pub path: String,
    pub size_bytes: u64,
    pub is_dir: bool,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadChunkRequest {
    pub path: String,
    pub offset: u64,
    pub length: u32,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadBeginRequest {
    pub path: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub overwrite: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadBeginResult {
    pub upload_id: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadChunkQuery {
    pub upload_id: String,
    pub offset: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadCommitRequest {
    pub upload_id: String,
    pub sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadAbortRequest {
    pub upload_id: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferResult {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileCopyRequest {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub recursive: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMoveRequest {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryCreateRequest {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

// Desktop input coordinates are physical pixels of the virtual Windows desktop.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPointRequest {
    pub x: i32,
    pub y: i32,
}
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopButton {
    #[default]
    Left,
    Right,
    Middle,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopClickRequest {
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub button: DesktopButton,
    #[serde(default = "default_clicks")]
    pub clicks: u32,
}
fn default_clicks() -> u32 {
    1
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopScrollRequest {
    #[serde(default)]
    pub vertical: i32,
    #[serde(default)]
    pub horizontal: i32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopTypeRequest {
    pub text: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopKeyRequest {
    pub keys: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopInputResult {
    pub sent_inputs: u32,
}
