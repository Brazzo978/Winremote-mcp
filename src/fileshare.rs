use crate::protocol::{
    DirectoryCreateRequest, FileCopyRequest, FileMoveRequest, FileStatResult, ReadChunkRequest,
    TransferResult, UploadAbortRequest, UploadBeginRequest, UploadBeginResult, UploadChunkQuery,
    UploadCommitRequest, MAX_TRANSFER_BYTES, MAX_TRANSFER_CHUNK_BYTES,
};
use axum::http::StatusCode;
use rand::RngCore;
use ring::digest::{Context, SHA256};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant, UNIX_EPOCH},
};
use tempfile::{Builder, NamedTempFile};

#[derive(Debug)]
pub struct FileError {
    pub status: StatusCode,
    pub message: String,
}
impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for FileError {}
pub type FileResult<T> = Result<T, FileError>;
fn error(status: StatusCode, message: impl Into<String>) -> FileError {
    FileError {
        status,
        message: message.into(),
    }
}
fn bad(message: impl Into<String>) -> FileError {
    error(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> FileError {
    error(StatusCode::CONFLICT, message)
}
fn too_large(message: impl Into<String>) -> FileError {
    error(StatusCode::PAYLOAD_TOO_LARGE, message)
}
fn io_error(e: std::io::Error) -> FileError {
    let status = match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, e.to_string())
}
fn cancelled(cancel: &AtomicBool) -> FileResult<()> {
    if cancel.load(Ordering::SeqCst) {
        Err(error(
            StatusCode::GONE,
            "operation cancelled or host expired",
        ))
    } else {
        Ok(())
    }
}
fn absolute(path: &str) -> FileResult<PathBuf> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err(bad("path must be absolute"));
    }
    Ok(p)
}
fn destination(path: &str) -> FileResult<PathBuf> {
    let p = absolute(path)?;
    let name = p
        .file_name()
        .ok_or_else(|| bad("destination must name a file or directory"))?;
    let parent = p
        .parent()
        .ok_or_else(|| bad("destination parent is missing"))?
        .canonicalize()
        .map_err(io_error)?;
    if !parent.is_dir() {
        return Err(bad("destination parent is not a directory"));
    }
    Ok(parent.join(name))
}
fn is_reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn reject_reparse_ancestors(path: &Path) -> FileResult<()> {
    for ancestor in path.ancestors() {
        let meta = fs::symlink_metadata(ancestor).map_err(io_error)?;
        if is_reparse(&meta) {
            return Err(bad(
                "symlinks and reparse points are not supported for copy or move sources",
            ));
        }
    }
    Ok(())
}
fn checked_meta(path: &Path) -> FileResult<Metadata> {
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if is_reparse(&meta) {
        return Err(bad(
            "symlinks and reparse points are not supported for transfers",
        ));
    }
    Ok(meta)
}
fn metadata_version(meta: &Metadata) -> FileResult<String> {
    let modified = meta.modified().map_err(io_error)?;
    let nanos = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    Ok(format!(
        "v1:{}:{}:{}",
        meta.len(),
        nanos,
        u8::from(meta.is_dir())
    ))
}
fn sha256_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn check_sha256(value: &str) -> FileResult<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(bad("sha256 must be 64 lowercase hexadecimal characters"))
    }
}

pub fn stat(req: &crate::protocol::PathRequest) -> FileResult<FileStatResult> {
    let path = absolute(&req.path)?;
    let meta = checked_meta(&path)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(bad("path is not a regular file or directory"));
    }
    if meta.is_file() && meta.len() > MAX_TRANSFER_BYTES {
        return Err(too_large("file exceeds 10 GiB"));
    }
    Ok(FileStatResult {
        path: req.path.clone(),
        size_bytes: meta.len(),
        is_dir: meta.is_dir(),
        version: metadata_version(&meta)?,
    })
}
pub fn read_chunk(req: &ReadChunkRequest) -> FileResult<Vec<u8>> {
    if req.length == 0 || req.length > MAX_TRANSFER_CHUNK_BYTES {
        return Err(bad("length must be between 1 byte and 1 MiB"));
    }
    let path = absolute(&req.path)?;
    let meta = checked_meta(&path)?;
    if !meta.is_file() {
        return Err(bad("read-chunk requires a regular file"));
    }
    if meta.len() > MAX_TRANSFER_BYTES {
        return Err(too_large("file exceeds 10 GiB"));
    }
    if req.version != metadata_version(&meta)? {
        return Err(conflict("file changed since stat"));
    }
    if req.offset > meta.len() {
        return Err(bad("offset exceeds file size"));
    }
    let mut file = File::open(&path).map_err(io_error)?;
    if metadata_version(&file.metadata().map_err(io_error)?)? != req.version {
        return Err(conflict("file changed before read"));
    }
    file.seek(SeekFrom::Start(req.offset)).map_err(io_error)?;
    let remaining = (meta.len() - req.offset).min(req.length as u64) as usize;
    let mut data = vec![0u8; remaining];
    file.read_exact(&mut data)
        .map_err(|_| conflict("file changed during read"))?;
    if metadata_version(&file.metadata().map_err(io_error)?)? != req.version
        || metadata_version(&checked_meta(&path)?)? != req.version
    {
        return Err(conflict("file changed during read"));
    }
    Ok(data)
}

const MAX_SESSIONS: usize = 8;
const SESSION_TTL: Duration = Duration::from_secs(600);
struct UploadSession {
    path: String,
    target: PathBuf,
    temp: NamedTempFile,
    expected: u64,
    received: u64,
    overwrite: bool,
    digest: Context,
    touched: Instant,
}
pub struct UploadManager {
    sessions: Mutex<HashMap<String, UploadSession>>,
}
impl Default for UploadManager {
    fn default() -> Self {
        Self::new()
    }
}
impl UploadManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
    fn lock(&self) -> FileResult<std::sync::MutexGuard<'_, HashMap<String, UploadSession>>> {
        self.sessions.lock().map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "upload state lock poisoned",
            )
        })
    }
    pub fn cleanup_expired(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            let now = Instant::now();
            sessions.retain(|_, s| now.duration_since(s.touched) < SESSION_TTL);
        }
    }
    pub fn abort_all(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }
    pub fn begin(&self, req: UploadBeginRequest) -> FileResult<UploadBeginResult> {
        if req.size_bytes > MAX_TRANSFER_BYTES {
            return Err(too_large("upload exceeds 10 GiB"));
        }
        let target = destination(&req.path)?;
        if let Ok(meta) = fs::symlink_metadata(&target) {
            if is_reparse(&meta) || !meta.is_file() {
                return Err(conflict("destination is not a regular file"));
            }
            if !req.overwrite {
                return Err(conflict("destination exists; set overwrite=true"));
            }
        }
        self.cleanup_expired();
        let mut sessions = self.lock()?;
        if sessions.len() >= MAX_SESSIONS {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many active uploads",
            ));
        }
        let parent = target
            .parent()
            .ok_or_else(|| bad("destination parent is missing"))?;
        let temp = NamedTempFile::new_in(parent).map_err(io_error)?;
        let mut random = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut random);
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let id = URL_SAFE_NO_PAD.encode(random);
        sessions.insert(
            id.clone(),
            UploadSession {
                path: req.path,
                target,
                temp,
                expected: req.size_bytes,
                received: 0,
                overwrite: req.overwrite,
                digest: Context::new(&SHA256),
                touched: Instant::now(),
            },
        );
        Ok(UploadBeginResult { upload_id: id })
    }
    pub fn chunk(&self, query: &UploadChunkQuery, bytes: &[u8]) -> FileResult<u64> {
        if bytes.is_empty() || bytes.len() > MAX_TRANSFER_CHUNK_BYTES as usize {
            return Err(bad("chunk must contain 1 byte to 1 MiB"));
        }
        self.cleanup_expired();
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&query.upload_id)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "upload session not found or expired"))?;
        if query.offset != session.received {
            return Err(conflict("chunk offset is not the next expected offset"));
        }
        let next = session
            .received
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| too_large("upload size overflow"))?;
        if next > session.expected || next > MAX_TRANSFER_BYTES {
            return Err(too_large("chunk exceeds declared upload size"));
        }
        session
            .temp
            .as_file_mut()
            .write_all(bytes)
            .map_err(io_error)?;
        session.digest.update(bytes);
        session.received = next;
        session.touched = Instant::now();
        Ok(next)
    }
    pub fn commit(&self, req: UploadCommitRequest) -> FileResult<TransferResult> {
        check_sha256(&req.sha256)?;
        self.cleanup_expired();
        let mut session = self
            .lock()?
            .remove(&req.upload_id)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "upload session not found or expired"))?;
        if session.received != session.expected {
            return Err(conflict("upload is incomplete"));
        }
        let actual = sha256_hex(session.digest.finish().as_ref());
        if actual != req.sha256 {
            return Err(conflict("upload SHA-256 mismatch"));
        }
        session.temp.as_file_mut().flush().map_err(io_error)?;
        session.temp.as_file().sync_all().map_err(io_error)?;
        let path = session.path.clone();
        let size_bytes = session.received;
        let saved = if session.overwrite {
            session.temp.persist(&session.target)
        } else {
            session.temp.persist_noclobber(&session.target)
        };
        saved.map_err(|e| io_error(e.error))?;
        Ok(TransferResult {
            path,
            size_bytes,
            sha256: actual,
        })
    }
    pub fn abort(&self, req: UploadAbortRequest) -> FileResult<bool> {
        self.cleanup_expired();
        Ok(self.lock()?.remove(&req.upload_id).is_some())
    }
}

#[derive(Debug, Serialize)]
pub struct CopyResult {
    pub source: String,
    pub destination: String,
    pub size_bytes: u64,
    pub files: u64,
}
#[derive(Debug, Serialize)]
pub struct MoveResult {
    pub source: String,
    pub destination: String,
}
#[derive(Debug, Serialize)]
pub struct MkdirResult {
    pub path: String,
}
fn is_same_or_child(src: &Path, dst: &Path) -> bool {
    #[cfg(windows)]
    {
        let src = src.to_string_lossy().replace('/', "\\").to_lowercase();
        let dst = dst.to_string_lossy().replace('/', "\\").to_lowercase();
        dst == src || dst.starts_with(&(src.trim_end_matches('\\').to_owned() + "\\"))
    }
    #[cfg(not(windows))]
    {
        dst == src || dst.starts_with(src)
    }
}
fn check_target(src: &Path, dst: &Path, is_dir: bool) -> FileResult<()> {
    if (is_dir && is_same_or_child(src, dst))
        || is_same_or_child(src, dst) && is_same_or_child(dst, src)
    {
        return Err(bad("source and destination overlap"));
    }
    Ok(())
}
#[derive(Default, Copy, Clone)]
struct Inventory {
    files: u64,
    bytes: u64,
    entries: u64,
}
fn inventory(source: &Path, recursive: bool, cancel: &AtomicBool) -> FileResult<Inventory> {
    let meta = checked_meta(source)?;
    if meta.is_file() {
        if meta.len() > MAX_TRANSFER_BYTES {
            return Err(too_large("source exceeds 10 GiB"));
        }
        return Ok(Inventory {
            files: 1,
            bytes: meta.len(),
            entries: 1,
        });
    }
    if !meta.is_dir() {
        return Err(bad("source must be a regular file or directory"));
    }
    if !recursive {
        return Err(bad("directory copy requires recursive=true"));
    }
    let mut result = Inventory::default();
    let mut stack = vec![(source.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        cancelled(cancel)?;
        if depth > 64 {
            return Err(too_large("directory depth exceeds 64"));
        }
        for entry in fs::read_dir(&dir).map_err(io_error)? {
            cancelled(cancel)?;
            let entry = entry.map_err(io_error)?;
            let path = entry.path();
            let meta = checked_meta(&path)?;
            result.entries += 1;
            if result.entries > 10000 {
                return Err(too_large("directory has more than 10000 entries"));
            }
            if meta.is_dir() {
                stack.push((path, depth + 1));
            } else if meta.is_file() {
                result.files += 1;
                result.bytes = result
                    .bytes
                    .checked_add(meta.len())
                    .ok_or_else(|| too_large("directory size overflow"))?;
                if result.bytes > MAX_TRANSFER_BYTES {
                    return Err(too_large("directory exceeds 10 GiB"));
                }
            } else {
                return Err(bad("source contains a non-regular entry"));
            }
        }
    }
    Ok(result)
}
fn copy_one(
    source: &Path,
    target: &mut File,
    cancel: &AtomicBool,
    total: &mut u64,
) -> FileResult<u64> {
    let meta = checked_meta(source)?;
    if !meta.is_file() {
        return Err(bad("source is not a regular file"));
    }
    let version = metadata_version(&meta)?;
    let mut input = File::open(source).map_err(io_error)?;
    if metadata_version(&input.metadata().map_err(io_error)?)? != version {
        return Err(conflict("source changed before copy"));
    }
    let mut buffer = vec![0u8; MAX_TRANSFER_CHUNK_BYTES as usize];
    let mut copied = 0u64;
    loop {
        cancelled(cancel)?;
        let n = input.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        *total = total
            .checked_add(n as u64)
            .ok_or_else(|| too_large("copy size overflow"))?;
        if *total > MAX_TRANSFER_BYTES {
            return Err(too_large("copy exceeds 10 GiB"));
        }
        target.write_all(&buffer[..n]).map_err(io_error)?;
    }
    if copied != meta.len()
        || metadata_version(&input.metadata().map_err(io_error)?)? != version
        || metadata_version(&checked_meta(source)?)? != version
    {
        return Err(conflict("source changed during copy"));
    }
    target.flush().map_err(io_error)?;
    Ok(copied)
}
fn stage_directory(source: &Path, stage: &Path, cancel: &AtomicBool) -> FileResult<Inventory> {
    let mut result = Inventory::default();
    let mut stack = vec![(source.to_path_buf(), stage.to_path_buf(), 0usize)];
    while let Some((src, dst, depth)) = stack.pop() {
        cancelled(cancel)?;
        if depth > 64 {
            return Err(too_large("directory depth exceeds 64"));
        }
        for entry in fs::read_dir(&src).map_err(io_error)? {
            cancelled(cancel)?;
            let entry = entry.map_err(io_error)?;
            let src_path = entry.path();
            let meta = checked_meta(&src_path)?;
            let dst_path = dst.join(entry.file_name());
            result.entries += 1;
            if result.entries > 10000 {
                return Err(too_large("directory has more than 10000 entries"));
            }
            if meta.is_dir() {
                fs::create_dir(&dst_path).map_err(io_error)?;
                stack.push((src_path, dst_path, depth + 1));
            } else if meta.is_file() {
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dst_path)
                    .map_err(io_error)?;
                copy_one(&src_path, &mut output, cancel, &mut result.bytes)?;
                output.sync_all().map_err(io_error)?;
                result.files += 1;
            } else {
                return Err(bad("source contains a non-regular entry"));
            }
        }
    }
    Ok(result)
}
pub fn copy(req: FileCopyRequest, cancel: &AtomicBool) -> FileResult<CopyResult> {
    let original = absolute(&req.source)?;
    reject_reparse_ancestors(&original)?;
    checked_meta(&original)?;
    let source = original.canonicalize().map_err(io_error)?;
    let meta = checked_meta(&source)?;
    let target = destination(&req.destination)?;
    check_target(&source, &target, meta.is_dir())?;
    let planned = inventory(&source, req.recursive, cancel)?;
    if let Ok(dest_meta) = fs::symlink_metadata(&target) {
        if is_reparse(&dest_meta) || dest_meta.is_dir() || meta.is_dir() || !req.overwrite {
            return Err(conflict(
                "destination exists or is not a replaceable regular file",
            ));
        }
    }
    cancelled(cancel)?;
    let mut total = 0u64;
    if meta.is_file() {
        let mut temp = NamedTempFile::new_in(target.parent().unwrap()).map_err(io_error)?;
        let copied = copy_one(&source, temp.as_file_mut(), cancel, &mut total)?;
        if copied != planned.bytes {
            return Err(conflict("source changed after preflight"));
        }
        temp.as_file().sync_all().map_err(io_error)?;
        cancelled(cancel)?;
        let saved = if req.overwrite {
            temp.persist(&target)
        } else {
            temp.persist_noclobber(&target)
        };
        saved.map_err(|e| io_error(e.error))?;
    } else {
        let stage = Builder::new()
            .prefix(".winremote-copy-")
            .tempdir_in(target.parent().unwrap())
            .map_err(io_error)?;
        let copied = stage_directory(&source, stage.path(), cancel)?;
        if copied.bytes != planned.bytes
            || copied.files != planned.files
            || copied.entries != planned.entries
        {
            return Err(conflict("source changed after preflight"));
        }
        cancelled(cancel)?;
        move_path(stage.path(), &target, false).map_err(io_error)?;
        let _ = stage.keep();
        total = copied.bytes;
    }
    Ok(CopyResult {
        source: req.source,
        destination: req.destination,
        size_bytes: total,
        files: planned.files,
    })
}
#[cfg(windows)]
fn move_path(source: &Path, target: &Path, overwrite: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};
    let src: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let flags = if overwrite {
        MOVEFILE_REPLACE_EXISTING
    } else {
        0
    };
    if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn move_path(source: &Path, target: &Path, overwrite: bool) -> std::io::Result<()> {
    if !overwrite && target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists",
        ));
    }
    fs::rename(source, target)
}
pub fn move_entry(req: FileMoveRequest, cancel: &AtomicBool) -> FileResult<MoveResult> {
    let original = absolute(&req.source)?;
    reject_reparse_ancestors(&original)?;
    checked_meta(&original)?;
    let source = original.canonicalize().map_err(io_error)?;
    let meta = checked_meta(&source)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(bad("source must be regular file or directory"));
    }
    let target = destination(&req.destination)?;
    check_target(&source, &target, meta.is_dir())?;
    let _ = inventory(&source, true, cancel)?;
    if let Ok(dest_meta) = fs::symlink_metadata(&target) {
        if is_reparse(&dest_meta) || dest_meta.is_dir() || meta.is_dir() || !req.overwrite {
            return Err(conflict("destination exists or cannot be replaced"));
        }
    }
    cancelled(cancel)?;
    move_path(&source, &target, req.overwrite).map_err(io_error)?;
    Ok(MoveResult {
        source: req.source,
        destination: req.destination,
    })
}
pub fn mkdir(req: DirectoryCreateRequest) -> FileResult<MkdirResult> {
    let path = absolute(&req.path)?;
    if req.recursive {
        fs::create_dir_all(&path).map_err(io_error)?;
    } else {
        fs::create_dir(&path).map_err(io_error)?;
    }
    Ok(MkdirResult { path: req.path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PathRequest;
    fn entry_count(path: &Path) -> usize {
        fs::read_dir(path).unwrap().count()
    }
    #[test]
    fn upload_verified_commit_no_clobber_abort_and_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        let name = path.to_string_lossy().into_owned();
        let uploads = UploadManager::new();
        let id = uploads
            .begin(UploadBeginRequest {
                path: name.clone(),
                size_bytes: 3,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        assert!(uploads
            .commit(UploadCommitRequest {
                upload_id: id.clone(),
                sha256: sha256_hex(ring::digest::digest(&SHA256, b"abc").as_ref())
            })
            .is_err());
        assert!(!path.exists());
        assert_eq!(entry_count(dir.path()), 0);
        let id = uploads
            .begin(UploadBeginRequest {
                path: name.clone(),
                size_bytes: 3,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        assert_eq!(
            uploads
                .chunk(
                    &UploadChunkQuery {
                        upload_id: id.clone(),
                        offset: 0
                    },
                    b"abc"
                )
                .unwrap(),
            3
        );
        assert!(uploads
            .commit(UploadCommitRequest {
                upload_id: id,
                sha256: "0".repeat(64)
            })
            .is_err());
        assert!(!path.exists());
        assert_eq!(entry_count(dir.path()), 0);
        let id = uploads
            .begin(UploadBeginRequest {
                path: name.clone(),
                size_bytes: 3,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        uploads
            .chunk(
                &UploadChunkQuery {
                    upload_id: id.clone(),
                    offset: 0,
                },
                b"abc",
            )
            .unwrap();
        let hash = sha256_hex(ring::digest::digest(&SHA256, b"abc").as_ref());
        let result = uploads
            .commit(UploadCommitRequest {
                upload_id: id,
                sha256: hash.clone(),
            })
            .unwrap();
        assert_eq!(result.path, name);
        assert_eq!(result.sha256, hash);
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        assert!(uploads
            .begin(UploadBeginRequest {
                path: name.clone(),
                size_bytes: 1,
                overwrite: false
            })
            .is_err());
        let second = dir.path().join("abort.bin");
        let second_name = second.to_string_lossy().into_owned();
        let id = uploads
            .begin(UploadBeginRequest {
                path: second_name.clone(),
                size_bytes: 1,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        assert!(uploads.abort(UploadAbortRequest { upload_id: id }).unwrap());
        assert!(!second.exists());
        assert_eq!(entry_count(dir.path()), 1);
        let id = uploads
            .begin(UploadBeginRequest {
                path: second_name,
                size_bytes: 1,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        assert_eq!(
            uploads
                .chunk(
                    &UploadChunkQuery {
                        upload_id: id,
                        offset: 0
                    },
                    b"x"
                )
                .unwrap(),
            1
        );
        uploads.abort_all();
        assert!(!second.exists());
        assert_eq!(entry_count(dir.path()), 1);
    }
    #[test]
    fn read_chunk_detects_version_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a");
        fs::write(&p, b"hello").unwrap();
        let path = p.to_string_lossy().into_owned();
        let info = stat(&PathRequest { path: path.clone() }).unwrap();
        assert_eq!(
            read_chunk(&ReadChunkRequest {
                path: path.clone(),
                offset: 1,
                length: 3,
                version: info.version.clone()
            })
            .unwrap(),
            b"ell"
        );
        fs::write(&p, b"different").unwrap();
        assert_eq!(
            read_chunk(&ReadChunkRequest {
                path,
                offset: 0,
                length: 1,
                version: info.version
            })
            .unwrap_err()
            .status,
            StatusCode::CONFLICT
        );
    }
    #[test]
    fn recursive_copy_rejects_child_and_preserves_collision() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a"), b"x").unwrap();
        let child = src.join("child");
        let cancel = AtomicBool::new(false);
        assert!(copy(
            FileCopyRequest {
                source: src.to_string_lossy().into_owned(),
                destination: child.to_string_lossy().into_owned(),
                overwrite: false,
                recursive: true
            },
            &cancel
        )
        .is_err());
        let dst = dir.path().join("dst");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("keep"), b"ok").unwrap();
        assert!(copy(
            FileCopyRequest {
                source: src.to_string_lossy().into_owned(),
                destination: dst.to_string_lossy().into_owned(),
                overwrite: true,
                recursive: true
            },
            &cancel
        )
        .is_err());
        assert_eq!(fs::read(dst.join("keep")).unwrap(), b"ok");
    }
    #[test]
    fn upload_racing_destination_does_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("raced.bin");
        let path = target.to_string_lossy().into_owned();
        let uploads = UploadManager::new();
        let upload_id = uploads
            .begin(UploadBeginRequest {
                path,
                size_bytes: 3,
                overwrite: false,
            })
            .unwrap()
            .upload_id;
        uploads
            .chunk(
                &UploadChunkQuery {
                    upload_id: upload_id.clone(),
                    offset: 0,
                },
                b"new",
            )
            .unwrap();
        fs::write(&target, b"external").unwrap();
        let hash = sha256_hex(ring::digest::digest(&SHA256, b"new").as_ref());
        assert!(uploads
            .commit(UploadCommitRequest {
                upload_id,
                sha256: hash
            })
            .is_err());
        assert_eq!(fs::read(&target).unwrap(), b"external");
        assert_eq!(entry_count(dir.path()), 1);
    }
    #[test]
    fn cancelled_copy_leaves_no_stage_or_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), vec![7u8; 1024 * 1024]).unwrap();
        let target = dir.path().join("target");
        let cancel = AtomicBool::new(true);
        assert!(copy(
            FileCopyRequest {
                source: source.to_string_lossy().into_owned(),
                destination: target.to_string_lossy().into_owned(),
                overwrite: false,
                recursive: true
            },
            &cancel
        )
        .is_err());
        assert!(!target.exists());
        assert_eq!(entry_count(dir.path()), 1);
    }
    #[test]
    fn reparse_source_is_rejected_for_copy_and_move_when_available() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("real.bin");
        fs::write(&source, b"safe").unwrap();
        let link = dir.path().join("link.bin");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&source, &link).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&source, &link).is_ok();
        #[cfg(not(any(windows, unix)))]
        let linked = false;
        if !linked {
            return;
        }
        let target = dir.path().join("destination.bin");
        let cancel = AtomicBool::new(false);
        assert!(copy(
            FileCopyRequest {
                source: link.to_string_lossy().into_owned(),
                destination: target.to_string_lossy().into_owned(),
                overwrite: false,
                recursive: false
            },
            &cancel
        )
        .is_err());
        assert!(move_entry(
            FileMoveRequest {
                source: link.to_string_lossy().into_owned(),
                destination: target.to_string_lossy().into_owned(),
                overwrite: false
            },
            &cancel
        )
        .is_err());
        assert_eq!(fs::read(&source).unwrap(), b"safe");
        assert!(!target.exists());
    }
}
