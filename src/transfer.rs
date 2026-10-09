use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use reqwest::{Client, Response};
use ring::digest::{Context as ShaContext, SHA256};
use serde::{de::DeserializeOwned, Serialize};

use crate::connection;
use crate::protocol::{
    Connection, FileStatResult, PathRequest, ReadChunkRequest, TransferResult, UploadAbortRequest,
    UploadBeginRequest, UploadBeginResult, UploadChunkQuery, UploadCommitRequest,
    MAX_TRANSFER_BYTES, MAX_TRANSFER_CHUNK_BYTES,
};

const MAX_JSON_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = MAX_TRANSFER_CHUNK_BYTES as usize;

#[derive(Debug, Serialize)]
pub struct TransferSummary {
    pub local_path: String,
    pub remote_path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

fn validate_paths(local_path: &Path, remote_path: &str) -> Result<()> {
    if !local_path.is_absolute() || remote_path.is_empty() || !Path::new(remote_path).is_absolute()
    {
        bail!("Both local and remote paths must be absolute");
    }
    if remote_path.contains('\0') {
        bail!("Invalid remote path");
    }
    Ok(())
}

fn source_metadata(path: &Path) -> Result<Metadata> {
    let metadata = std::fs::metadata(path).context("Cannot inspect local source")?;
    if !metadata.is_file() {
        bail!("Local source must be a regular file");
    }
    if metadata.len() > MAX_TRANSFER_BYTES {
        bail!("Transfer exceeds the 10 GiB limit");
    }
    Ok(metadata)
}

fn check_source_unchanged(before: &Metadata, after: &Metadata) -> Result<()> {
    if before.len() != after.len() || before.modified()? != after.modified()? {
        bail!("Local source changed during transfer");
    }
    Ok(())
}

fn digest_hex(context: ShaContext) -> String {
    let bytes = context.finish();
    let mut text = String::with_capacity(64);
    for byte in bytes.as_ref() {
        use std::fmt::Write as _;
        write!(&mut text, "{byte:02x}").unwrap();
    }
    text
}

fn route(connection: &Connection, path: &str) -> String {
    format!("{}{}", connection.endpoint.trim_end_matches('/'), path)
}

async fn checked(response: Response) -> Result<Response> {
    if !response.status().is_success() {
        bail!("Host returned HTTP {}", response.status().as_u16());
    }
    Ok(response)
}

pub(crate) async fn limited_bytes(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Host response interrupted")?
    {
        if data.len().saturating_add(chunk.len()) > limit {
            bail!("Host response exceeded size limit");
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

pub(crate) async fn limited_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    let bytes = limited_bytes(checked(response).await?, MAX_JSON_BYTES).await?;
    serde_json::from_slice(&bytes).context("Invalid host response")
}

async fn post_json<B: Serialize, T: DeserializeOwned>(
    client: &Client,
    connection: &Connection,
    path: &str,
    body: &B,
) -> Result<T> {
    connection::ensure_valid(connection)?;
    let response = client
        .post(route(connection, path))
        .bearer_auth(&connection.token)
        .json(body)
        .send()
        .await
        .context("Host request failed")?;
    limited_json(response).await
}

async fn abort(client: &Client, connection: &Connection, upload_id: &str) {
    let _ = client
        .post(route(connection, "/v1/files/upload/abort"))
        .bearer_auth(&connection.token)
        .json(&UploadAbortRequest {
            upload_id: upload_id.to_owned(),
        })
        .send()
        .await;
}

pub async fn upload(
    connection: &Connection,
    local_path: &Path,
    remote_path: &str,
    overwrite: bool,
) -> Result<TransferSummary> {
    validate_paths(local_path, remote_path)?;
    connection::ensure_valid(connection)?;
    let before = source_metadata(local_path)?;
    let mut source = File::open(local_path).context("Cannot open local source")?;
    let client = connection::http_client(connection)?;
    let begun: UploadBeginResult = post_json(
        &client,
        connection,
        "/v1/files/upload/begin",
        &UploadBeginRequest {
            path: remote_path.to_owned(),
            size_bytes: before.len(),
            overwrite,
        },
    )
    .await?;
    if begun.upload_id.is_empty() {
        bail!("Host returned an invalid upload ID");
    }
    let upload_id = begun.upload_id;
    let result = async {
        let mut context = ShaContext::new(&SHA256);
        let mut offset = 0u64;
        let mut buffer = vec![0u8; CHUNK_BYTES];
        while offset < before.len() {
            connection::ensure_valid(connection)?;
            let wanted = (before.len() - offset).min(CHUNK_BYTES as u64) as usize;
            source
                .read_exact(&mut buffer[..wanted])
                .context("Local source changed during transfer")?;
            context.update(&buffer[..wanted]);
            let response = client
                .put(route(connection, "/v1/files/upload/chunk"))
                .bearer_auth(&connection.token)
                .query(&UploadChunkQuery {
                    upload_id: upload_id.clone(),
                    offset,
                })
                .body(buffer[..wanted].to_vec())
                .send()
                .await
                .context("Host upload chunk failed")?;
            let ack: serde_json::Value = limited_json(response).await?;
            let received = ack
                .get("received_bytes")
                .and_then(|v| v.as_u64())
                .context("Invalid host chunk acknowledgement")?;
            if received != offset + wanted as u64 {
                bail!("Host chunk acknowledgement mismatch");
            }
            offset += wanted as u64;
        }
        check_source_unchanged(&before, &source.metadata()?)?;
        connection::ensure_valid(connection)?;
        let sha256 = digest_hex(context);
        let committed: TransferResult = post_json(
            &client,
            connection,
            "/v1/files/upload/commit",
            &UploadCommitRequest {
                upload_id: upload_id.clone(),
                sha256: sha256.clone(),
            },
        )
        .await?;
        if committed.path != remote_path
            || committed.size_bytes != before.len()
            || committed.sha256 != sha256
        {
            bail!("Host transfer confirmation mismatch");
        }
        Ok(TransferSummary {
            local_path: local_path.to_string_lossy().into_owned(),
            remote_path: remote_path.to_owned(),
            size_bytes: before.len(),
            sha256,
        })
    }
    .await;
    if result.is_err() {
        abort(&client, connection, &upload_id).await;
    }
    result
}

pub async fn download(
    connection: &Connection,
    local_path: &Path,
    remote_path: &str,
    overwrite: bool,
) -> Result<TransferSummary> {
    validate_paths(local_path, remote_path)?;
    connection::ensure_valid(connection)?;
    if !overwrite && local_path.exists() {
        bail!("Local destination already exists");
    }
    let parent = local_path.parent().context("Invalid local destination")?;
    let client = connection::http_client(connection)?;
    let stat: FileStatResult = post_json(
        &client,
        connection,
        "/v1/files/stat",
        &PathRequest {
            path: remote_path.to_owned(),
        },
    )
    .await?;
    if stat.path != remote_path
        || stat.is_dir
        || stat.size_bytes > MAX_TRANSFER_BYTES
        || stat.version.is_empty()
    {
        bail!("Remote source is invalid or exceeds the 10 GiB limit");
    }
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).context("Cannot stage local destination")?;
    let mut context = ShaContext::new(&SHA256);
    let mut offset = 0u64;
    while offset < stat.size_bytes {
        connection::ensure_valid(connection)?;
        let wanted = (stat.size_bytes - offset).min(CHUNK_BYTES as u64) as usize;
        let response = client
            .post(route(connection, "/v1/files/read-chunk"))
            .bearer_auth(&connection.token)
            .json(&ReadChunkRequest {
                path: remote_path.to_owned(),
                offset,
                length: wanted as u32,
                version: stat.version.clone(),
            })
            .send()
            .await
            .context("Host read chunk failed")?;
        let bytes = limited_bytes(checked(response).await?, CHUNK_BYTES).await?;
        if bytes.len() != wanted {
            bail!("Host chunk length mismatch");
        }
        staged
            .write_all(&bytes)
            .context("Cannot write staged local file")?;
        context.update(&bytes);
        offset += bytes.len() as u64;
    }
    let final_stat: FileStatResult = post_json(
        &client,
        connection,
        "/v1/files/stat",
        &PathRequest {
            path: remote_path.to_owned(),
        },
    )
    .await?;
    if final_stat.path != stat.path
        || final_stat.version != stat.version
        || final_stat.size_bytes != stat.size_bytes
        || final_stat.is_dir
    {
        bail!("Remote source changed during transfer");
    }
    staged
        .as_file()
        .sync_all()
        .context("Cannot sync staged local file")?;
    connection::ensure_valid(connection)?;
    let sha256 = digest_hex(context);
    if overwrite {
        staged
            .persist(local_path)
            .map_err(|error| error.error)
            .context("Cannot replace local destination")?;
    } else {
        staged
            .persist_noclobber(local_path)
            .map_err(|error| error.error)
            .context("Local destination already exists")?;
    }
    Ok(TransferSummary {
        local_path: local_path.to_string_lossy().into_owned(),
        remote_path: remote_path.to_owned(),
        size_bytes: stat.size_bytes,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_relative_paths_and_nonfiles() {
        let temp = tempfile::tempdir().unwrap();
        assert!(validate_paths(Path::new("relative"), "C:\\remote").is_err());
        assert!(source_metadata(temp.path()).is_err());
    }
    #[test]
    fn digest_is_lowercase_hex() {
        let mut context = ShaContext::new(&SHA256);
        context.update(b"abc");
        assert_eq!(
            digest_hex(context),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
