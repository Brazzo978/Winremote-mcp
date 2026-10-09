//! Sequential stdio MCP adapter. Request cancellation is not supported in this MVP.
use std::path::{Path, PathBuf};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{
    DesktopClickRequest, DesktopInputResult, DesktopKeyRequest, DesktopPointRequest,
    DesktopScrollRequest, DesktopTypeRequest, DirectoryCreateRequest, ExecuteRequest,
    ExecuteResult, FileCopyRequest, FileMoveRequest, PathRequest, ScreenshotRequest,
    ScreenshotResult, WriteFileRequest, MAX_REQUEST_BYTES, MAX_SCREENSHOT_BYTES,
    MAX_SCREENSHOT_PIXELS,
};
use crate::{connection, desktop_input, transfer};

const SUPPORTED_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const DEFAULT_VERSION: &str = "2025-11-25";

#[derive(Default)]
struct Session {
    initialized: bool,
    version: Option<String>,
}

pub async fn run(connection_path: PathBuf) -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve(BufReader::new(stdin), stdout, &connection_path).await
}

async fn serve<R, W>(mut input: R, mut output: W, connection_path: &Path) -> Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut session = Session::default();
    loop {
        let mut line = Vec::new();
        let mut oversized = false;
        loop {
            let available = input.fill_buf().await?;
            if available.is_empty() {
                return Ok(());
            }
            let end = available.iter().position(|&b| b == b'\n');
            let take = end.map_or(available.len(), |index| index + 1);
            if !oversized {
                if line.len().saturating_add(take) > MAX_REQUEST_BYTES {
                    oversized = true;
                    line.clear();
                } else {
                    line.extend_from_slice(&available[..take]);
                }
            }
            input.consume(take);
            if end.is_some() {
                break;
            }
        }
        let response = if oversized {
            Some(rpc_error(Value::Null, -32600, "request too large"))
        } else {
            handle_line(&line, &mut session, connection_path).await
        };
        if let Some(response) = response {
            output
                .write_all(serde_json::to_string(&response)?.as_bytes())
                .await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }
}

async fn handle_line(line: &[u8], session: &mut Session, path: &Path) -> Option<Value> {
    let message: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(_) => return Some(rpc_error(Value::Null, -32700, "parse error")),
    };
    let object = match message.as_object() {
        Some(object) => object,
        None => return Some(rpc_error(Value::Null, -32600, "invalid request")),
    };
    let id = object.get("id").cloned();
    let response_id = id.clone().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || !matches!(
            id,
            None | Some(Value::Null) | Some(Value::String(_)) | Some(Value::Number(_))
        )
    {
        return Some(rpc_error(response_id, -32600, "invalid request"));
    }
    let method = match object.get("method").and_then(Value::as_str) {
        Some(method) => method,
        None => return Some(rpc_error(response_id, -32600, "invalid request")),
    };
    // Notifications never receive JSON-RPC responses.
    if id.is_none() {
        if method == "notifications/initialized" && session.version.is_some() {
            session.initialized = true;
        }
        return None;
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            let Some(requested) = requested else {
                return Some(rpc_error(
                    response_id,
                    -32602,
                    "protocolVersion is required",
                ));
            };
            let selected = if SUPPORTED_VERSIONS.contains(&requested) {
                requested
            } else {
                DEFAULT_VERSION
            };
            session.version = Some(selected.to_owned());
            session.initialized = false;
            Some(rpc_result(
                response_id,
                json!({
                    "protocolVersion": selected,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "winremote-mcp", "version": env!("CARGO_PKG_VERSION") }
                }),
            ))
        }
        "ping" => Some(rpc_result(response_id, json!({}))),
        _ if !session.initialized => Some(rpc_error(response_id, -32000, "not initialized")),
        "tools/list" => Some(rpc_result(response_id, json!({ "tools": tools_list() }))),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(response_id, -32602, "tool name is required"));
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = match invoke_tool(
                name,
                arguments,
                path,
                session.version.as_deref().unwrap_or(DEFAULT_VERSION),
            )
            .await
            {
                Ok(value) => value,
                Err(ToolFailure::InvalidArguments) => {
                    return Some(rpc_error(response_id, -32602, "invalid tool arguments"));
                }
                Err(ToolFailure::UnknownTool) => {
                    return Some(rpc_error(response_id, -32602, "unknown tool"));
                }
                Err(ToolFailure::Execution(message)) => {
                    return Some(rpc_result(
                        response_id,
                        json!({
                            "content": [{ "type": "text", "text": message }],
                            "isError": true
                        }),
                    ));
                }
            };
            Some(rpc_result(response_id, result))
        }
        _ => Some(rpc_error(response_id, -32601, "method not found")),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools_list() -> Vec<Value> {
    vec![
        tool("file_upload", "Upload a local file to the Windows host in verified chunks, up to 10 GiB", transfer_schema()),
        tool("file_download", "Download a Windows host file to a local path in verified chunks, up to 10 GiB", transfer_schema()),
        tool("file_stat", "Get remote file or directory size and version", path_schema()),
        tool("file_copy", "Copy a remote file or directory; directory copies require recursive=true", json!({
            "type":"object",
            "properties":{
                "source":{"type":"string"},"destination":{"type":"string"},
                "overwrite":{"type":"boolean"},"recursive":{"type":"boolean"}
            },
            "required":["source","destination"],"additionalProperties":false
        })),
        tool("file_move", "Move a remote file or directory on the same filesystem", json!({
            "type":"object",
            "properties":{
                "source":{"type":"string"},"destination":{"type":"string"},
                "overwrite":{"type":"boolean"}
            },
            "required":["source","destination"],"additionalProperties":false
        })),
        tool("directory_create", "Create a remote directory", json!({
            "type":"object",
            "properties":{"path":{"type":"string"},"recursive":{"type":"boolean"}},
            "required":["path"],"additionalProperties":false
        })),
        tool(
            "bridge_connect",
            "Connect this MCP client to a Windows bridge using its invitation; replaces the saved connection only after the host verifies successfully",
            json!({
                "type":"object",
                "properties":{"invitation":{"type":"string","maxLength":32768}},
                "required":["invitation"], "additionalProperties":false
            }),
        ),
        tool(
            "windows_info",
            "Get Windows host information and capabilities",
            json!({
                "type":"object", "properties":{}, "required":[], "additionalProperties":false
            }),
        ),
        tool(
            "desktop_screenshot",
            "Capture the remote Windows virtual desktop as a PNG image with original desktop bounds and image dimensions. Requires an unlocked interactive desktop; captures visible windows. Optional max_width controls downscaling.",
            json!({"type":"object", "properties":{"max_width":{"type":"integer","minimum":320,"maximum":3840,"default":1920}},"required":[],"additionalProperties":false}),
        ),
        tool(
            "desktop_move",
            "Move the pointer on the unlocked Windows desktop using physical virtual desktop coordinates. Convert screenshot coordinates with desktop_x + image_x * desktop_width / width and desktop_y + image_y * desktop_height / height. Input targets the foreground desktop; the response acknowledges injection, not an application outcome. Take a follow-up screenshot to verify the result.",
            json!({"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"}},"required":["x","y"],"additionalProperties":false}),
        ),
        tool(
            "desktop_click",
            "Click at physical virtual desktop coordinates on the unlocked Windows desktop. Convert screenshot coordinates with desktop_x + image_x * desktop_width / width and desktop_y + image_y * desktop_height / height. Input targets the foreground desktop; the response acknowledges injection, not an application outcome. Take a follow-up screenshot to verify the result.",
            json!({"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"},"button":{"type":"string","enum":["left","right","middle"],"default":"left"},"clicks":{"type":"integer","minimum":1,"maximum":2,"default":1}},"required":["x","y"],"additionalProperties":false}),
        ),
        tool(
            "desktop_scroll",
            "Scroll at the current pointer position on the unlocked Windows desktop. Signed notches: positive vertical scrolls up and positive horizontal scrolls right. Input targets the foreground desktop; the response acknowledges injection, not an application outcome. Take a follow-up screenshot to verify the result.",
            json!({"type":"object","properties":{"vertical":{"type":"integer","minimum":-100,"maximum":100,"default":0},"horizontal":{"type":"integer","minimum":-100,"maximum":100,"default":0}},"required":[],"additionalProperties":false}),
        ),
        tool(
            "desktop_type",
            "Type nonempty Unicode text (at most 4096 UTF-16 units) into the foreground target on the unlocked Windows desktop. Newlines send Enter and tabs send Tab; no clipboard is used. The response acknowledges injection, not an application outcome. Take a follow-up screenshot to verify the result.",
            json!({"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":4096}},"required":["text"],"additionalProperties":false}),
        ),
        tool(
            "desktop_key",
            "Send a key or chord to the foreground target on the unlocked Windows desktop. Use distinct modifiers CTRL, ALT, SHIFT, WIN followed by exactly one key: Enter, Escape, Tab, Backspace, Delete, arrows, Home, End, PageUp, PageDown, Space, Insert, F1-F24, A-Z, or 0-9. Ctrl/Control aliases are accepted. The response acknowledges injection, not an application outcome. Take a follow-up screenshot to verify the result.",
            json!({"type":"object","properties":{"keys":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"string"}}},"required":["keys"],"additionalProperties":false}),
        ),
        tool(
            "powershell_execute",
            "Execute PowerShell as the bridge user. Absolute cwd is required and is not a sandbox. A new process runs per call; its descendants end when the call completes.",
            json!({
                "type":"object",
                "properties":{
                    "script":{"type":"string"},
                    "cwd":{"type":"string"},
                    "timeout_secs":{"type":"integer","minimum":1,"maximum":120}
                },
                "required":["script","cwd"], "additionalProperties":false
            }),
        ),
        tool("file_read", "Read a file up to 2 MiB as standard RFC 4648 base64", path_schema()),
        tool(
            "file_write",
            "Write up to 2 MiB of standard RFC 4648 base64 file content; overwrite defaults to false",
            json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "content_base64":{"type":"string"},
                    "overwrite":{"type":"boolean"}
                },
                "required":["path","content_base64"], "additionalProperties":false
            }),
        ),
        tool(
            "directory_list",
            "List entries in a directory",
            path_schema(),
        ),
    ]
}

fn transfer_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "local_path":{"type":"string"},"remote_path":{"type":"string"},
            "overwrite":{"type":"boolean"}
        },
        "required":["local_path","remote_path"],"additionalProperties":false
    })
}
fn path_schema() -> Value {
    json!({
        "type":"object", "properties":{"path":{"type":"string"}},
        "required":["path"], "additionalProperties":false
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

#[derive(Debug)]
enum ToolFailure {
    InvalidArguments,
    UnknownTool,
    Execution(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferArguments {
    local_path: String,
    remote_path: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectArguments {
    invitation: String,
}

fn parse_args<T: DeserializeOwned>(value: Value) -> std::result::Result<T, ToolFailure> {
    serde_json::from_value(value).map_err(|_| ToolFailure::InvalidArguments)
}

async fn invoke_tool(
    name: &str,
    arguments: Value,
    path: &Path,
    version: &str,
) -> std::result::Result<Value, ToolFailure> {
    if name == "bridge_connect" {
        let args: ConnectArguments = parse_args(arguments)?;
        if args.invitation.len() > 32768 {
            return Err(ToolFailure::InvalidArguments);
        }
        return connect_bridge(args.invitation, path, version).await;
    }
    if name == "file_upload" || name == "file_download" {
        let args: TransferArguments = parse_args(arguments)?;
        let local = Path::new(&args.local_path);
        let remote = Path::new(&args.remote_path);
        if !local.is_absolute() || !remote.is_absolute() {
            return Err(ToolFailure::InvalidArguments);
        }
        let connection = connection::load(path)
            .map_err(|_| ToolFailure::Execution("connection unavailable".into()))?;
        let summary = if name == "file_upload" {
            transfer::upload(&connection, local, &args.remote_path, args.overwrite).await
        } else {
            transfer::download(&connection, local, &args.remote_path, args.overwrite).await
        }
        .map_err(|error| {
            let message = error.to_string();
            let safe = message
                .strip_prefix("Host returned HTTP ")
                .and_then(|status| status.parse::<u16>().ok())
                .map(|status| format!("file transfer failed (HTTP {status})"))
                .unwrap_or_else(|| "file transfer failed".into());
            ToolFailure::Execution(safe)
        })?;
        return tool_success(json!(summary), false, version);
    }
    enum Operation {
        Info,
        Screenshot(ScreenshotRequest),
        DesktopMove(DesktopPointRequest),
        DesktopClick(DesktopClickRequest),
        DesktopScroll(DesktopScrollRequest),
        DesktopType(DesktopTypeRequest),
        DesktopKey(DesktopKeyRequest),
        Execute(ExecuteRequest),
        Read(PathRequest),
        Write(WriteFileRequest),
        List(PathRequest),
        Stat(PathRequest),
        Copy(FileCopyRequest),
        Move(FileMoveRequest),
        Mkdir(DirectoryCreateRequest),
    }
    let operation = match name {
        "windows_info" => {
            let _: EmptyArguments = parse_args(arguments)?;
            Operation::Info
        }
        "desktop_screenshot" => {
            let request: ScreenshotRequest = parse_args(arguments)?;
            if !(320..=3840).contains(&request.max_width) {
                return Err(ToolFailure::InvalidArguments);
            }
            Operation::Screenshot(request)
        }
        "desktop_move" => {
            let request: DesktopPointRequest = parse_args(arguments)?;
            desktop_input::validate_point(&request).map_err(|_| ToolFailure::InvalidArguments)?;
            Operation::DesktopMove(request)
        }
        "desktop_click" => {
            let request: DesktopClickRequest = parse_args(arguments)?;
            desktop_input::validate_click(&request).map_err(|_| ToolFailure::InvalidArguments)?;
            Operation::DesktopClick(request)
        }
        "desktop_scroll" => {
            let request: DesktopScrollRequest = parse_args(arguments)?;
            desktop_input::validate_scroll(&request).map_err(|_| ToolFailure::InvalidArguments)?;
            Operation::DesktopScroll(request)
        }
        "desktop_type" => {
            let request: DesktopTypeRequest = parse_args(arguments)?;
            desktop_input::validate_type(&request).map_err(|_| ToolFailure::InvalidArguments)?;
            Operation::DesktopType(request)
        }
        "desktop_key" => {
            let request: DesktopKeyRequest = parse_args(arguments)?;
            desktop_input::validate_key(&request).map_err(|_| ToolFailure::InvalidArguments)?;
            Operation::DesktopKey(request)
        }
        "powershell_execute" => {
            let request: ExecuteRequest = parse_args(arguments)?;
            if request.script.is_empty()
                || request.cwd.is_empty()
                || !(1..=120).contains(&request.timeout_secs)
            {
                return Err(ToolFailure::InvalidArguments);
            }
            Operation::Execute(request)
        }
        "file_read" => Operation::Read(parse_args(arguments)?),
        "file_write" => Operation::Write(parse_args(arguments)?),
        "directory_list" => Operation::List(parse_args(arguments)?),
        "file_stat" => Operation::Stat(parse_args(arguments)?),
        "file_copy" => Operation::Copy(parse_args(arguments)?),
        "file_move" => Operation::Move(parse_args(arguments)?),
        "directory_create" => Operation::Mkdir(parse_args(arguments)?),
        _ => return Err(ToolFailure::UnknownTool),
    };
    let connection = connection::load(path)
        .map_err(|_| ToolFailure::Execution("connection unavailable".into()))?;
    connection::ensure_valid(&connection)
        .map_err(|_| ToolFailure::Execution("connection expired or invalid".into()))?;
    let client = connection::http_client(&connection)
        .map_err(|_| ToolFailure::Execution("secure client unavailable".into()))?;
    let endpoint = connection.endpoint.trim_end_matches('/');
    let (route, body): (&str, Option<Value>) = match &operation {
        Operation::Info => ("/v1/info", None),
        Operation::Screenshot(request) => ("/v1/desktop/screenshot", Some(json!(request))),
        Operation::DesktopMove(request) => ("/v1/desktop/move", Some(json!(request))),
        Operation::DesktopClick(request) => ("/v1/desktop/click", Some(json!(request))),
        Operation::DesktopScroll(request) => ("/v1/desktop/scroll", Some(json!(request))),
        Operation::DesktopType(request) => ("/v1/desktop/type", Some(json!(request))),
        Operation::DesktopKey(request) => ("/v1/desktop/key", Some(json!(request))),
        Operation::Execute(request) => ("/v1/execute", Some(json!(request))),
        Operation::Read(request) => ("/v1/files/read", Some(json!(request))),
        Operation::Write(request) => ("/v1/files/write", Some(json!(request))),
        Operation::List(request) => ("/v1/files/list", Some(json!(request))),
        Operation::Stat(request) => ("/v1/files/stat", Some(json!(request))),
        Operation::Copy(request) => ("/v1/files/copy", Some(json!(request))),
        Operation::Move(request) => ("/v1/files/move", Some(json!(request))),
        Operation::Mkdir(request) => ("/v1/files/mkdir", Some(json!(request))),
    };
    let request = if let Some(body) = body {
        client.post(format!("{endpoint}{route}")).json(&body)
    } else {
        client.get(format!("{endpoint}{route}"))
    };
    let response = request
        .bearer_auth(&connection.token)
        .send()
        .await
        .map_err(|_| ToolFailure::Execution("host request failed".into()))?;
    if !response.status().is_success() {
        return Err(ToolFailure::Execution(format!(
            "host returned HTTP {}",
            response.status().as_u16()
        )));
    }
    if let Operation::Screenshot(request) = &operation {
        let mut response = response;
        let mut bytes = Vec::new();
        let max_response = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4 + 65536;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ToolFailure::Execution("screenshot transfer failed".into()))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_response {
                return Err(ToolFailure::Execution(
                    "screenshot response too large".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let screenshot: ScreenshotResult = serde_json::from_slice(&bytes)
            .map_err(|_| ToolFailure::Execution("invalid screenshot response".into()))?;
        if screenshot.width > request.max_width {
            return Err(ToolFailure::Execution(
                "invalid screenshot dimensions".into(),
            ));
        }
        return screenshot_success(screenshot, version);
    }
    let payload: Value = if matches!(
        &operation,
        Operation::Stat(_)
            | Operation::Copy(_)
            | Operation::Move(_)
            | Operation::Mkdir(_)
            | Operation::DesktopMove(_)
            | Operation::DesktopClick(_)
            | Operation::DesktopScroll(_)
            | Operation::DesktopType(_)
            | Operation::DesktopKey(_)
    ) {
        transfer::limited_json(response)
            .await
            .map_err(|_| ToolFailure::Execution("invalid host response".into()))?
    } else {
        response
            .json()
            .await
            .map_err(|_| ToolFailure::Execution("invalid host response".into()))?
    };
    if matches!(
        &operation,
        Operation::DesktopMove(_)
            | Operation::DesktopClick(_)
            | Operation::DesktopScroll(_)
            | Operation::DesktopType(_)
            | Operation::DesktopKey(_)
    ) {
        let result: DesktopInputResult = serde_json::from_value(payload)
            .map_err(|_| ToolFailure::Execution("invalid host response".into()))?;
        return tool_success(json!(result), false, version);
    }
    let mut is_error = false;
    if matches!(operation, Operation::Execute(_)) {
        let result: ExecuteResult = serde_json::from_value(payload.clone())
            .map_err(|_| ToolFailure::Execution("invalid host response".into()))?;
        is_error = result.timed_out || result.exit_code != Some(0);
    }
    tool_success(payload, is_error, version)
}

fn tool_success(
    payload: Value,
    is_error: bool,
    version: &str,
) -> std::result::Result<Value, ToolFailure> {
    let text = serde_json::to_string(&payload)
        .map_err(|_| ToolFailure::Execution("invalid host response".into()))?;
    let mut result = json!({ "content": [{ "type": "text", "text": text }] });
    if is_error {
        result["isError"] = json!(true);
    }
    if matches!(version, "2025-06-18" | "2025-11-25") {
        result["structuredContent"] = payload;
    }
    Ok(result)
}

fn screenshot_success(
    screenshot: ScreenshotResult,
    version: &str,
) -> std::result::Result<Value, ToolFailure> {
    let invalid = || ToolFailure::Execution("invalid screenshot response".into());
    if screenshot.mime_type != "image/png"
        || screenshot.width == 0
        || screenshot.height == 0
        || screenshot.desktop_width == 0
        || screenshot.desktop_height == 0
        || screenshot.width > screenshot.desktop_width
        || screenshot.height > screenshot.desktop_height
        || u64::from(screenshot.width) * u64::from(screenshot.height) > MAX_SCREENSHOT_PIXELS
        || screenshot.content_base64.len() > MAX_SCREENSHOT_BYTES.div_ceil(3) * 4
    {
        return Err(invalid());
    }
    let bytes = STANDARD
        .decode(&screenshot.content_base64)
        .map_err(|_| invalid())?;
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(invalid());
    }
    let png = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .map_err(|_| invalid())?;
    if png.info().width != screenshot.width || png.info().height != screenshot.height {
        return Err(invalid());
    }
    let metadata = json!({"mime_type": screenshot.mime_type, "width": screenshot.width,
        "height": screenshot.height, "desktop_x": screenshot.desktop_x, "desktop_y": screenshot.desktop_y,
        "desktop_width": screenshot.desktop_width, "desktop_height": screenshot.desktop_height,
        "captured_unix": screenshot.captured_unix,
        "coordinate_mapping": "desktop_x + image_x * desktop_width / width; desktop_y + image_y * desktop_height / height"});
    let mut result = tool_success(metadata, false, version)?;
    result["content"]
        .as_array_mut()
        .ok_or_else(invalid)?
        .push(json!({
            "type": "image", "data": screenshot.content_base64, "mimeType": "image/png"
        }));
    Ok(result)
}

async fn connect_bridge(
    invitation: String,
    path: &Path,
    version: &str,
) -> std::result::Result<Value, ToolFailure> {
    let connection = connection::resolve_invitation(&invitation)
        .await
        .map_err(|_| ToolFailure::Execution("invalid or expired invitation".into()))?;
    let client = connection::http_client(&connection)
        .map_err(|_| ToolFailure::Execution("secure client unavailable".into()))?;
    let response = client
        .get(format!(
            "{}/v1/info",
            connection.endpoint.trim_end_matches('/')
        ))
        .bearer_auth(&connection.token)
        .send()
        .await
        .map_err(|_| ToolFailure::Execution("host verification failed".into()))?;
    if !response.status().is_success() {
        return Err(ToolFailure::Execution(format!(
            "host verification returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let info: Value = response
        .json()
        .await
        .map_err(|_| ToolFailure::Execution("invalid host response".into()))?;
    if !info.is_object() {
        return Err(ToolFailure::Execution("invalid host response".into()));
    }
    connection::replace(path, &connection)
        .map_err(|_| ToolFailure::Execution("cannot save connection".into()))?;
    tool_success(
        json!({
            "connected": true,
            "endpoint": connection.endpoint,
            "expires_unix": connection.expires_unix,
            "host": info
        }),
        false,
        version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn transcript(lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n") + "\n";
        let (mut writer, reader) = tokio::io::duplex(8192);
        writer.write_all(input.as_bytes()).await.unwrap();
        drop(writer);
        let (out, mut output) = tokio::io::duplex(8192);
        serve(BufReader::new(reader), out, Path::new("unused"))
            .await
            .unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut output, &mut bytes)
            .await
            .unwrap();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn initialize_then_list_without_banners() {
        let values = transcript(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ]).await;
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(values[1]["result"]["tools"].as_array().unwrap().len(), 18);
        assert_eq!(values[1]["result"]["tools"][0]["name"], "file_upload");
        let tools = values[1]["result"]["tools"].as_array().unwrap();
        for name in [
            "desktop_move",
            "desktop_click",
            "desktop_scroll",
            "desktop_type",
            "desktop_key",
        ] {
            let input = &tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"];
            assert_eq!(input["additionalProperties"], false);
            assert!(input["required"].is_array());
        }
    }

    #[tokio::test]
    async fn desktop_input_invalid_arguments_are_rejected_before_connection() {
        let values = transcript(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"desktop_move","arguments":{"x":1}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_click","arguments":{"x":1,"y":2,"clicks":3}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"desktop_scroll","arguments":{"vertical":0}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"desktop_scroll","arguments":{"horizontal":101}}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"desktop_type","arguments":{"text":""}}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"desktop_key","arguments":{"keys":["CTRL","CONTROL","A"]}}}"#,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"desktop_key","arguments":{"keys":["A","CTRL"]}}}"#,
        ]).await;
        assert_eq!(values.len(), 8);
        for value in &values[1..] {
            assert_eq!(value["error"]["code"], -32602);
            assert_eq!(value["error"]["message"], "invalid tool arguments");
        }
    }

    #[tokio::test]
    async fn invalid_invitation_with_missing_connection_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-connection.json");
        let mut session = Session::default();
        let init = serde_json::to_vec(&json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-11-25"}
        }))
        .unwrap();
        handle_line(&init, &mut session, &path).await.unwrap();
        handle_line(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &mut session,
            &path,
        )
        .await;
        let secret = "wb1_secretTOKEN";
        let call = serde_json::to_vec(&json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"bridge_connect","arguments":{"invitation":secret}}
        }))
        .unwrap();
        let response = handle_line(&call, &mut session, &path).await.unwrap();
        assert_eq!(response["result"]["isError"], true);
        let serialized = response.to_string();
        assert!(serialized.contains("invalid or expired invitation"));
        assert!(!serialized.contains(secret));
        assert!(!path.exists());
    }
    #[test]
    fn screenshot_is_image_content_with_separate_metadata() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0])
                .unwrap();
        }
        let mut screenshot = ScreenshotResult {
            mime_type: "image/png".into(),
            content_base64: STANDARD.encode(bytes),
            width: 1,
            height: 1,
            desktop_x: -1920,
            desktop_y: 0,
            desktop_width: 1920,
            desktop_height: 1080,
            captured_unix: 123,
        };
        let result = screenshot_success(
            serde_json::from_value(json!(screenshot)).unwrap(),
            "2025-11-25",
        )
        .unwrap();
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["mimeType"], "image/png");
        assert_eq!(result["structuredContent"]["desktop_x"], -1920);
        assert!(result["structuredContent"].get("content_base64").is_none());
        screenshot.width = 2;
        assert!(screenshot_success(screenshot, "2025-11-25").is_err());
    }

    #[tokio::test]
    async fn malformed_unsupported_and_early_call() {
        let values = transcript(&[
            "not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"unknown"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"no/such/method"}"#,
        ]).await;
        assert_eq!(values.len(), 4);
        assert_eq!(values[0]["error"]["code"], -32700);
        assert_eq!(values[1]["error"]["code"], -32000);
        assert_eq!(values[2]["result"]["protocolVersion"], DEFAULT_VERSION);
        assert_eq!(values[3]["error"]["code"], -32601);
    }
}
