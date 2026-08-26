use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
    Engine as _,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::{SinkExt, StreamExt};
use qrcode::{render::svg, QrCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Listener, Url};
use tokio::{
    fs as tokio_fs,
    sync::{Mutex, RwLock},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    cli_tools::{factory::CliToolFactory, CliTool},
    commands::{
        chat::{
            cancel_turn_inner, save_pasted_image_attachment, send_message_inner,
            ChatAttachmentPayload,
        },
        threads::create_thread_with_defaults,
    },
    config::app_config::{AppConfig, RemoteAccessConfig, RemoteDeviceConfig},
    // 历史分页已恢复：MessageWindowCursorDto 用于 message.list 游标分页。
    models::MessageWindowCursorDto,
    state::AppState,
};

const REMOTE_PROTOCOL_VERSION: u32 = 1;
/// 未提交批次的最长空闲时间；超时后只清理暂存文件，不影响已提交附件。
const REMOTE_BATCH_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// 远程附件单文件上限，与既有聊天附件限制保持一致。
const REMOTE_ATTACHMENT_MAX_BYTES: usize = 10 * 1024 * 1024;
// 历史分页窗口限制已恢复（游标分页已启用）：手机进入会话时按页读取消息。
const MESSAGE_WINDOW_DEFAULT_LIMIT: usize = 50;
const MESSAGE_WINDOW_MAX_LIMIT: usize = 100;
/// 单页响应序列化字节预算；低于 Relay 单帧上限并保留信封余量。
const MESSAGE_PAGE_BUDGET_BYTES: usize = 720 * 1024;
/// 单条移动消息序列化上限；超过时以占位文本替换 blocks。
const MESSAGE_SINGLE_MESSAGE_BUDGET_BYTES: usize = 512 * 1024;
/// 手机端可见文本（text/steer/error/notice 的 content、message 字段）截断上限。
const MOBILE_VISIBLE_TEXT_MAX_CHARS: usize = 64 * 1024;
/// 手机端不渲染的输出类字段（action 的 result.output/result.diff、thinking 的 content、diff 块的 diff）截断上限。
const MOBILE_HIDDEN_OUTPUT_MAX_CHARS: usize = 4 * 1024;
const MOBILE_TRUNCATED_SUFFIX: &str = "\n... [truncated]";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceDto {
    pub id: String,
    pub name: String,
    pub paired_at: Option<String>,
    pub last_connected_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatusDto {
    pub enabled: bool,
    pub endpoint: String,
    pub tunnel_id: String,
    pub connected: bool,
    pub peer_online: bool,
    pub last_error: Option<String>,
    pub pairing_payload: Option<String>,
    pub pairing_qr_svg: Option<String>,
    pub pairing_expires_at: Option<String>,
    pub paired: bool,
    pub devices: Vec<RemoteDeviceDto>,
}

#[derive(Debug, Clone)]
struct RemoteRuntimeState {
    generation: u64,
    config: RemoteAccessConfig,
    connected: bool,
    peer_online: bool,
    last_error: Option<String>,
    pairing_token: Option<String>,
    pairing_expires_at: Option<DateTime<Utc>>,
}

impl Default for RemoteRuntimeState {
    fn default() -> Self {
        Self {
            generation: 0,
            config: RemoteAccessConfig::default(),
            connected: false,
            peer_online: false,
            last_error: None,
            pairing_token: None,
            pairing_expires_at: None,
        }
    }
}

#[derive(Default)]
pub struct RemoteTunnelManager {
    runtime: RwLock<RemoteRuntimeState>,
    /// 当前隧道内已经完成 `device.identify` 的在线设备集合。
    online_devices: RwLock<HashSet<String>>,
    uploads: Mutex<HashMap<String, RemoteUploadState>>,
    /// 按已鉴别设备和批次保存尚未提交的附件路径，路径值是精确归属校验依据。
    batches: Mutex<HashMap<String, RemoteBatchState>>,
    cancellation: Mutex<Option<CancellationToken>>,
}

#[derive(Debug, Deserialize)]
struct RemoteRequest {
    version: u32,
    kind: String,
    id: String,
    method: String,
    auth: String,
    #[serde(default)]
    payload: Value,
}

/// `message.send` 中的附件引用；新客户端只填写 attachment_key，旧客户端可继续填写 file_path。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAttachmentInput {
    /// Relay HTTPS 暂存附件键。
    #[serde(default, alias = "attachment_key")]
    attachment_key: Option<String>,
    /// 客户端展示文件名；Relay 响应头是最终可信文件名。
    #[serde(default, alias = "file_name")]
    file_name: Option<String>,
    /// 旧客户端本机附件路径。
    #[serde(default, alias = "file_path")]
    file_path: Option<String>,
    /// 客户端声明的文件大小。
    #[serde(default, alias = "size_bytes")]
    size_bytes: Option<u64>,
    /// 客户端声明的 MIME 类型。
    #[serde(default, alias = "mime_type")]
    mime_type: Option<String>,
    /// 旧客户端浏览器标注元数据，Relay 新路径不使用。
    #[serde(default, alias = "browser_annotation")]
    browser_annotation: Option<Value>,
}

struct RemoteUploadState {
    /// 已鉴别远程连接对应的设备 ID，不读取手机 payload 中的 device_id。
    device_id: String,
    /// 手机一次发送生成的批次 ID；旧客户端路径为空字符串。
    batch_id: String,
    /// 原始文件名。
    file_name: String,
    /// 上传声明的 MIME 类型。
    mime_type: String,
    /// 该上传的分块总数。
    chunk_count: u32,
    /// 下一个期望接收的分块序号。
    next_chunk: u32,
    /// 已接收但尚未落盘的 Base64 数据。
    data_base64: String,
    /// 最近一次收到分块的时间，用于清理中止的旧客户端上传。
    last_activity: Instant,
}

struct RemoteBatchState {
    /// 已鉴别远程连接对应的设备 ID。
    device_id: String,
    /// 手机一次发送生成的批次 ID。
    batch_id: String,
    /// 批次内已落盘附件，键为必须精确匹配的 file_path。
    files: HashMap<String, ChatAttachmentPayload>,
    /// 最近一次上传或校验活动的时间。
    last_activity: Instant,
}

fn device_name_from_payload(payload: &Value) -> String {
    payload
        .get("device_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(80).collect())
        .unwrap_or_else(|| "AuraCoder Mobile".to_string())
}

/// 解析可选批次 ID；字段缺失表示旧客户端，字段存在但非法则拒绝请求。
fn parse_optional_batch_id(payload: &Value) -> Result<Option<String>, String> {
    let Some(value) = payload.get("batch_id") else {
        return Ok(None);
    };
    let batch_id = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| "batch_id is invalid".to_string())?;
    if batch_id.chars().any(char::is_control) {
        return Err("batch_id is invalid".to_string());
    }
    Ok(Some(batch_id.to_string()))
}

/// 设备 ID 和批次 ID 都来自服务端已鉴别状态，使用长度前缀避免字符串拼接碰撞。
fn remote_batch_key(device_id: &str, batch_id: &str) -> String {
    format!("{}:{}:{}", device_id.len(), device_id, batch_id)
}

/// 上传 ID 也纳入设备和批次命名空间，避免不同设备或批次互相覆盖。
fn remote_upload_key(device_id: &str, batch_id: &str, upload_id: &str) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        device_id.len(),
        device_id,
        batch_id.len(),
        batch_id,
        upload_id
    )
}

/// 将字符串编码为安全的目录名，避免批次 ID 进入路径时产生目录穿越。
fn remote_storage_component(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        value.to_string()
    } else {
        URL_SAFE_NO_PAD.encode(value.as_bytes())
    }
}

/// 批次暂存目录按设备隔离，再按批次隔离；已提交文件仍留在该目录供模型读取。
fn remote_batch_storage_dir(device_id: &str, batch_id: &str) -> std::path::PathBuf {
    crate::runtime_env::app_data_dir()
        .join("attachments")
        .join("mobile-batches")
        .join(remote_storage_component(device_id))
        .join(remote_storage_component(batch_id))
}

/// 处理手机端可能携带的 data URL，再以标准 Base64 解码上传内容。
fn decode_remote_attachment_data(data_base64: &str) -> Result<Vec<u8>, String> {
    let encoded = data_base64
        .trim()
        .split_once(',')
        .filter(|(prefix, _)| prefix.starts_with("data:") && prefix.contains(";base64"))
        .map(|(_, data)| data)
        .unwrap_or_else(|| data_base64.trim());
    BASE64
        .decode(encoded)
        .map_err(|_| "attachment data is not valid base64".to_string())
}

/// 根据请求认证凭据解析服务端保存的设备身份，不接受手机传入的 device_id。
async fn authenticated_device_id(manager: &RemoteTunnelManager, auth: &str) -> Option<String> {
    let runtime = manager.runtime.read().await;
    runtime
        .config
        .devices
        .iter()
        .find(|device| device.credential == auth)
        .map(|device| device.id.clone())
        .or_else(|| (runtime.config.device_credential == auth).then(|| "legacy".to_string()))
}

/// 解析可选的顶层 device_id；新客户端必须让它与认证凭据映射结果一致。
fn parse_optional_device_id(payload: &Value) -> Result<Option<String>, String> {
    let Some(value) = payload.get("device_id") else {
        return Ok(None);
    };
    let device_id = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| "device_id is invalid".to_string())?;
    if device_id.chars().any(char::is_control) {
        return Err("device_id is invalid".to_string());
    }
    Ok(Some(device_id.to_string()))
}

/// 读取 `message.send.attachments`，兼容新 attachment_key 和旧 file_path 字段。
fn parse_remote_attachment_inputs(
    payload: &Value,
) -> Result<Option<Vec<RemoteAttachmentInput>>, String> {
    let Some(value) = payload.get("attachments") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_array() {
        return Err("attachments must be an array".to_string());
    }
    serde_json::from_value::<Vec<RemoteAttachmentInput>>(value.clone())
        .map(Some)
        .map_err(|error| error.to_string())
}

/// 将旧客户端附件引用还原成现有 ChatAttachmentPayload，不影响旧路径协议。
fn legacy_attachment_payloads(
    inputs: &[RemoteAttachmentInput],
) -> Result<Vec<ChatAttachmentPayload>, String> {
    inputs
        .iter()
        .map(|input| {
            serde_json::to_value(input)
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ChatAttachmentPayload>(value)
                        .map_err(|error| error.to_string())
                })
        })
        .collect()
}

/// 将桌面端 WSS 隧道地址转换为同源 Relay HTTPS/HTTP 附件地址。
fn relay_attachment_url(
    endpoint: &str,
    tunnel_id: &str,
    attachment_key: &str,
) -> Result<Url, String> {
    let mut url =
        Url::parse(endpoint).map_err(|error| format!("invalid remote endpoint: {error}"))?;
    match url.scheme() {
        "wss" => url
            .set_scheme("https")
            .map_err(|_| "failed to convert remote endpoint scheme".to_string())?,
        "ws" => url
            .set_scheme("http")
            .map_err(|_| "failed to convert remote endpoint scheme".to_string())?,
        "https" | "http" => {}
        _ => return Err("remote endpoint must use ws, wss, http, or https".to_string()),
    }
    url.set_path("/");
    url.set_query(None);
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "remote endpoint cannot be used for attachment fetch".to_string())?;
        segments.clear();
        segments
            .push("api")
            .push("mobile")
            .push("attachments")
            .push(attachment_key);
    }
    url.query_pairs_mut().append_pair("tunnel_id", tunnel_id);
    Ok(url)
}

/// 严格解码 Relay URL 编码响应头，非法百分号或非 UTF-8 一律拒绝。
fn decode_relay_header_value(value: &str) -> Result<String, String> {
    let source = value.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        match source[index] {
            b'%' => {
                if index + 2 >= source.len() {
                    return Err("relay attachment header has invalid URL encoding".to_string());
                }
                let high = char::from(source[index + 1]).to_digit(16).ok_or_else(|| {
                    "relay attachment header has invalid URL encoding".to_string()
                })?;
                let low = char::from(source[index + 2]).to_digit(16).ok_or_else(|| {
                    "relay attachment header has invalid URL encoding".to_string()
                })?;
                decoded.push((high * 16 + low) as u8);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| "relay attachment file name is not valid UTF-8".to_string())
}

/// 从 Relay 响应头读取必需字段，并校验文件名不是路径穿越。
fn relay_required_header(
    response: &reqwest::Response,
    name: &str,
    decode_url: bool,
) -> Result<String, String> {
    let value = response
        .headers()
        .get(name)
        .ok_or_else(|| format!("relay attachment response is missing {name}"))?
        .to_str()
        .map_err(|_| format!("relay attachment response has invalid {name}"))?;
    let value = if decode_url {
        decode_relay_header_value(value)?
    } else {
        value.to_string()
    };
    if value.trim().is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(format!("relay attachment response has invalid {name}"));
    }
    Ok(value)
}

/// 通过 Relay 下载单个附件，校验设备、批次、响应头和 10 MiB 上限后落盘。
async fn fetch_relay_attachment(
    client: &reqwest::Client,
    endpoint: &str,
    tunnel_id: &str,
    relay_credential: &str,
    device_id: &str,
    batch_id: &str,
    input: &RemoteAttachmentInput,
) -> Result<ChatAttachmentPayload, String> {
    let attachment_key = input
        .attachment_key
        .as_deref()
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "attachment_key is invalid".to_string())?;
    let declared_size = input
        .size_bytes
        .ok_or_else(|| "attachment size_bytes is required".to_string())?;
    if declared_size > REMOTE_ATTACHMENT_MAX_BYTES as u64 {
        return Err("attachment exceeds the 10 MB limit".to_string());
    }
    if input
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        || input
            .mime_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err("attachment file_name and mime_type are required".to_string());
    }
    let url = relay_attachment_url(endpoint, tunnel_id, attachment_key)?;
    let response = client
        .get(url)
        .header("x-auracoder-relay-credential", relay_credential)
        .send()
        .await
        .map_err(|error| format!("failed to fetch relay attachment: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "relay attachment fetch failed with status {}",
            response.status()
        ));
    }
    let response_device_id = relay_required_header(&response, "x-auracoder-device-id", false)?;
    let response_batch_id = relay_required_header(&response, "x-auracoder-batch-id", false)?;
    let response_file_name = relay_required_header(&response, "x-auracoder-file-name", true)?;
    let response_mime_type = relay_required_header(&response, "content-type", false)?;
    if response_device_id != device_id {
        return Err("relay attachment device does not match authenticated device".to_string());
    }
    if response_batch_id != batch_id {
        return Err("relay attachment batch does not match message batch".to_string());
    }
    if response_file_name.contains('/') || response_file_name.contains('\\') {
        return Err("relay attachment file name cannot contain path separators".to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read relay attachment: {error}"))?;
    if bytes.is_empty() {
        return Err("relay attachment is empty".to_string());
    }
    if bytes.len() > REMOTE_ATTACHMENT_MAX_BYTES {
        return Err("attachment exceeds the 10 MB limit".to_string());
    }
    let extension = Path::new(&response_file_name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 24
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let attachment_dir = remote_batch_storage_dir(device_id, batch_id);
    tokio_fs::create_dir_all(&attachment_dir)
        .await
        .map_err(|error| format!("failed to create mobile batch directory: {error}"))?;
    let file_path = attachment_dir.join(format!(
        "mobile-file-{}{}",
        Uuid::new_v4().simple(),
        extension
    ));
    if let Err(error) = tokio_fs::write(&file_path, &bytes).await {
        let _ = tokio_fs::remove_file(&file_path).await;
        return Err(format!("failed to save mobile attachment: {error}"));
    }
    Ok(ChatAttachmentPayload {
        file_name: response_file_name,
        file_path: file_path.display().to_string(),
        size_bytes: bytes.len() as u64,
        mime_type: Some(response_mime_type),
        browser_annotation: None,
    })
}

/// 拉取并登记一个批次内的全部 Relay 附件；任何失败由调用方清理本机暂存文件。
async fn fetch_relay_attachments(
    manager: &RemoteTunnelManager,
    endpoint: &str,
    tunnel_id: &str,
    relay_credential: &str,
    device_id: &str,
    batch_id: &str,
    inputs: &[RemoteAttachmentInput],
) -> Result<Vec<ChatAttachmentPayload>, String> {
    if inputs.len() > 10 {
        return Err("at most 10 attachments are allowed".to_string());
    }
    let client = reqwest::Client::new();
    let mut seen_keys = HashSet::new();
    let mut attachments = Vec::with_capacity(inputs.len());
    for input in inputs {
        let key = input
            .attachment_key
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if !seen_keys.insert(key.to_string()) {
            return Err("duplicate attachment_key is not allowed".to_string());
        }
        let attachment = fetch_relay_attachment(
            &client,
            endpoint,
            tunnel_id,
            relay_credential,
            device_id,
            batch_id,
            input,
        )
        .await?;
        attachments.push(attachment);
    }
    let batch_key = remote_batch_key(device_id, batch_id);
    let mut batches = manager.batches.lock().await;
    let batch = batches
        .entry(batch_key)
        .or_insert_with(|| RemoteBatchState {
            device_id: device_id.to_string(),
            batch_id: batch_id.to_string(),
            files: HashMap::new(),
            last_activity: Instant::now(),
        });
    for attachment in &attachments {
        batch
            .files
            .insert(attachment.file_path.clone(), attachment.clone());
    }
    batch.last_activity = Instant::now();
    Ok(attachments)
}

/// 发送成功后尽力删除 Relay 暂存键；删除失败不回滚已创建的本机消息。
async fn delete_relay_attachment(
    client: &reqwest::Client,
    endpoint: &str,
    tunnel_id: &str,
    relay_credential: &str,
    attachment_key: &str,
) -> Result<(), String> {
    let url = relay_attachment_url(endpoint, tunnel_id, attachment_key)?;
    let response = client
        .delete(url)
        .header("x-auracoder-relay-credential", relay_credential)
        .send()
        .await
        .map_err(|error| format!("failed to delete relay attachment: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "relay attachment delete failed with status {}",
            response.status()
        ))
    }
}

/// 处理带 batch_id 的分块上传，并把完成文件登记到设备批次映射中。
async fn process_batched_attachment_upload(
    manager: &RemoteTunnelManager,
    device_id: String,
    batch_id: String,
    upload_id: String,
    file_name: String,
    mime_type: String,
    chunk_index: u32,
    chunk_count: u32,
    data_base64: String,
) -> Result<Value, String> {
    if chunk_index >= chunk_count {
        return Err("chunk_index is invalid".to_string());
    }
    let upload_key = remote_upload_key(&device_id, &batch_id, &upload_id);
    let completed: Result<Option<RemoteUploadState>, String> = {
        let mut uploads = manager.uploads.lock().await;
        let now = Instant::now();
        if chunk_index == 0 {
            if uploads.contains_key(&upload_key) {
                return Err("attachment upload is already in progress".to_string());
            }
            uploads.insert(
                upload_key.clone(),
                RemoteUploadState {
                    device_id: device_id.clone(),
                    batch_id: batch_id.clone(),
                    file_name: file_name.clone(),
                    mime_type: mime_type.clone(),
                    chunk_count,
                    next_chunk: 0,
                    data_base64: String::new(),
                    last_activity: now,
                },
            );
        }
        let upload = uploads
            .get_mut(&upload_key)
            .ok_or_else(|| "attachment upload was not initialized".to_string())?;
        if upload.file_name != file_name
            || upload.mime_type != mime_type
            || upload.chunk_count != chunk_count
            || upload.next_chunk != chunk_index
        {
            uploads.remove(&upload_key);
            return Err("attachment chunks are out of sequence".to_string());
        }
        upload.data_base64.push_str(&data_base64);
        upload.last_activity = now;
        if upload.data_base64.len() > 14_000_000 {
            uploads.remove(&upload_key);
            return Err("attachment exceeds the 10 MB limit".to_string());
        }
        upload.next_chunk += 1;
        if upload.next_chunk == upload.chunk_count {
            Ok(uploads.remove(&upload_key))
        } else {
            Ok(None)
        }
    };

    let Some(upload) = completed? else {
        return Ok(json!({ "complete": false }));
    };
    let bytes = decode_remote_attachment_data(&upload.data_base64)?;
    if bytes.is_empty() {
        return Err("attachment data is empty".to_string());
    }
    if bytes.len() > REMOTE_ATTACHMENT_MAX_BYTES {
        return Err("attachment exceeds the 10 MB limit".to_string());
    }
    let extension = Path::new(&upload.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 24
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let attachment_dir = remote_batch_storage_dir(&upload.device_id, &upload.batch_id);
    tokio_fs::create_dir_all(&attachment_dir)
        .await
        .map_err(|error| format!("failed to create mobile batch directory: {error}"))?;
    let file_path = attachment_dir.join(format!(
        "mobile-file-{}{}",
        Uuid::new_v4().simple(),
        extension
    ));
    if let Err(error) = tokio_fs::write(&file_path, &bytes).await {
        let _ = tokio_fs::remove_file(&file_path).await;
        return Err(format!("failed to save mobile attachment: {error}"));
    }
    let attachment = ChatAttachmentPayload {
        file_name: upload.file_name,
        file_path: file_path.display().to_string(),
        size_bytes: bytes.len() as u64,
        mime_type: Some(upload.mime_type),
        browser_annotation: None,
    };
    let batch_key = remote_batch_key(&device_id, &batch_id);
    manager
        .batches
        .lock()
        .await
        .entry(batch_key)
        .or_insert_with(|| RemoteBatchState {
            device_id,
            batch_id,
            files: HashMap::new(),
            last_activity: Instant::now(),
        })
        .files
        .insert(attachment.file_path.clone(), attachment.clone());
    // 最后一块沿用既有 ChatAttachment 顶层字段，移动端可直接读取 filePath。
    serde_json::to_value(attachment).map_err(|error| error.to_string())
}

/// 将聊天完成通知包装成移动端约定的远程事件。
///
/// 事件只携带刚刚完成持久化的单条助手消息，避免按线程扫描或发送流式差异。
fn build_completed_message_event(target_device_id: &str, payload: &Value) -> Option<Value> {
    let thread_id = payload
        .get("threadId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let message_id = payload
        .get("messageId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let message = payload.get("message")?.clone();
    Some(json!({
        "version": REMOTE_PROTOCOL_VERSION,
        "kind": "event",
        "event": "thread.message.completed",
        "targetDeviceId": target_device_id,
        "payload": {
            "threadId": thread_id,
            "messageId": message_id,
            "message": message,
        },
    }))
}

impl RemoteTunnelManager {
    /// 删除超过空闲期限且尚未提交的批次及其暂存文件。
    async fn cleanup_expired_batches(&self) {
        let now = Instant::now();
        let mut expired_keys = HashSet::new();
        let mut files_to_remove = Vec::new();
        {
            let mut batches = self.batches.lock().await;
            let keys = batches
                .iter()
                .filter(|(_, batch)| {
                    now.duration_since(batch.last_activity) >= REMOTE_BATCH_IDLE_TIMEOUT
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(batch) = batches.remove(&key) {
                    expired_keys.insert(key);
                    files_to_remove.extend(batch.files.into_keys());
                }
            }
        }
        if !expired_keys.is_empty() {
            let mut uploads = self.uploads.lock().await;
            uploads.retain(|_, upload| {
                upload.batch_id.is_empty()
                    || !expired_keys
                        .contains(&remote_batch_key(&upload.device_id, &upload.batch_id))
            });
            uploads.retain(|_, upload| {
                now.duration_since(upload.last_activity) < REMOTE_BATCH_IDLE_TIMEOUT
            });
        } else {
            // 即使批次尚未完成首个文件，也必须按分块活动时间清理内存缓存。
            self.uploads.lock().await.retain(|_, upload| {
                now.duration_since(upload.last_activity) < REMOTE_BATCH_IDLE_TIMEOUT
            });
        }
        for file_path in files_to_remove {
            // 暂存文件属于未提交批次；删除失败不应阻塞其他批次清理。
            let _ = tokio_fs::remove_file(file_path).await;
        }
    }

    /// 取消当前设备的一个批次，只删除尚未提交的附件文件。
    async fn abort_batch(&self, device_id: &str, batch_id: &str) {
        let key = remote_batch_key(device_id, batch_id);
        let batch = self.batches.lock().await.remove(&key);
        {
            let mut uploads = self.uploads.lock().await;
            uploads.retain(|_, upload| {
                upload.batch_id.is_empty()
                    || remote_batch_key(&upload.device_id, &upload.batch_id) != key
            });
        }
        if let Some(batch) = batch {
            for file_path in batch.files.into_keys() {
                // 只清理该设备、该批次状态中登记的精确路径。
                let _ = tokio_fs::remove_file(file_path).await;
            }
        }
    }

    /// 隧道重载或关闭时清理内存中登记的全部未提交批次文件。
    async fn cleanup_all_batches(&self) {
        let files_to_remove = {
            let mut batches = self.batches.lock().await;
            batches
                .drain()
                .flat_map(|(_, batch)| batch.files.into_keys())
                .collect::<Vec<_>>()
        };
        self.uploads.lock().await.clear();
        for file_path in files_to_remove {
            // 仅删除尚未提交批次登记的文件，不触碰历史消息附件。
            let _ = tokio_fs::remove_file(file_path).await;
        }
    }

    /// 提交成功后移除批次状态，但保留文件路径，供后续模型调用继续读取。
    async fn commit_batch(&self, device_id: &str, batch_id: &str) {
        let key = remote_batch_key(device_id, batch_id);
        self.batches.lock().await.remove(&key);
        let mut uploads = self.uploads.lock().await;
        uploads.retain(|_, upload| {
            upload.batch_id.is_empty()
                || remote_batch_key(&upload.device_id, &upload.batch_id) != key
        });
    }

    /// 校验批次附件路径必须精确属于当前认证设备和批次。
    async fn validate_batch_attachments(
        &self,
        device_id: &str,
        batch_id: &str,
        attachments: &[ChatAttachmentPayload],
    ) -> Result<(), String> {
        if attachments.len() > 10 {
            return Err("at most 10 attachments are allowed".to_string());
        }
        let key = remote_batch_key(device_id, batch_id);
        let mut batches = self.batches.lock().await;
        let Some(batch) = batches.get_mut(&key) else {
            if attachments.is_empty() {
                return Ok(());
            }
            return Err(
                "attachment batch is missing or does not belong to this device".to_string(),
            );
        };
        if batch.device_id != device_id || batch.batch_id != batch_id {
            return Err("attachment batch is not owned by this device".to_string());
        }
        let unique_paths = attachments
            .iter()
            .map(|attachment| attachment.file_path.as_str())
            .collect::<HashSet<_>>();
        if unique_paths.len() != attachments.len() {
            return Err("duplicate attachment file_path is not allowed".to_string());
        }
        if attachments.len() != batch.files.len() {
            return Err("attachment list must exactly match the uploaded batch".to_string());
        }
        for attachment in attachments {
            if !batch.files.contains_key(&attachment.file_path) {
                return Err(
                    "attachment file_path does not belong to this device and batch".to_string(),
                );
            }
        }
        if unique_paths.len() != batch.files.keys().collect::<HashSet<_>>().len()
            || !batch
                .files
                .keys()
                .all(|path| unique_paths.contains(path.as_str()))
        {
            return Err("attachment list must exactly match the uploaded batch".to_string());
        }
        batch.last_activity = Instant::now();
        Ok(())
    }

    pub async fn status(&self) -> RemoteAccessStatusDto {
        let runtime = self.runtime.read().await;
        let pairing_token = runtime.pairing_token.as_ref().filter(|_| {
            runtime
                .pairing_expires_at
                .map(|expires_at| expires_at > Utc::now())
                .unwrap_or(false)
        });
        let pairing_payload = if runtime.config.enabled
            && !runtime.config.tunnel_id.is_empty()
            && !runtime.config.credential.is_empty()
            && pairing_token.is_some()
        {
            serde_json::to_string(&json!({
                "version": REMOTE_PROTOCOL_VERSION,
                "endpoint": runtime.config.endpoint,
                "tunnel_id": runtime.config.tunnel_id,
                "relay_credential": runtime.config.credential,
                "pairing_token": pairing_token,
                "expires_at": runtime.pairing_expires_at.map(|value| value.to_rfc3339()),
            }))
            .ok()
        } else {
            None
        };
        let pairing_qr_svg = pairing_payload.as_ref().and_then(|payload| {
            QrCode::new(payload.as_bytes()).ok().map(|code| {
                code.render::<svg::Color>()
                    .min_dimensions(256, 256)
                    .dark_color(svg::Color("#111827"))
                    .light_color(svg::Color("#ffffff"))
                    .build()
            })
        });
        let mut devices = runtime
            .config
            .devices
            .iter()
            .map(|device| RemoteDeviceDto {
                id: device.id.clone(),
                name: if device.name.trim().is_empty() {
                    "AuraCoder Mobile".to_string()
                } else {
                    device.name.clone()
                },
                paired_at: (!device.paired_at.is_empty()).then(|| device.paired_at.clone()),
                last_connected_at: (!device.last_connected_at.is_empty())
                    .then(|| device.last_connected_at.clone()),
            })
            .collect::<Vec<_>>();
        if !runtime.config.device_credential.is_empty() {
            devices.push(RemoteDeviceDto {
                id: "legacy".to_string(),
                name: "AuraCoder Mobile".to_string(),
                paired_at: None,
                last_connected_at: None,
            });
        }
        RemoteAccessStatusDto {
            enabled: runtime.config.enabled,
            endpoint: runtime.config.endpoint.clone(),
            tunnel_id: runtime.config.tunnel_id.clone(),
            connected: runtime.connected,
            peer_online: runtime.peer_online,
            last_error: runtime.last_error.clone(),
            pairing_payload,
            pairing_qr_svg,
            pairing_expires_at: runtime.pairing_expires_at.map(|value| value.to_rfc3339()),
            paired: !devices.is_empty(),
            devices,
        }
    }

    pub async fn refresh_pairing_token(&self) -> RemoteAccessStatusDto {
        let mut runtime = self.runtime.write().await;
        if runtime.config.enabled {
            runtime.pairing_token = Some(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ));
            runtime.pairing_expires_at = Some(Utc::now() + ChronoDuration::minutes(5));
        }
        drop(runtime);
        self.status().await
    }

    pub async fn configure(
        self: &Arc<Self>,
        app: AppHandle,
        state: AppState,
        config: RemoteAccessConfig,
    ) {
        if let Some(cancellation) = self.cancellation.lock().await.take() {
            cancellation.cancel();
        }
        self.online_devices.write().await.clear();
        self.cleanup_all_batches().await;

        let generation = {
            let mut runtime = self.runtime.write().await;
            runtime.generation = runtime.generation.wrapping_add(1);
            runtime.config = config.clone();
            runtime.connected = false;
            runtime.peer_online = false;
            runtime.last_error = None;
            // 配对凭据只在用户点击“添加设备”时生成，打开设置或重连服务时不生成二维码。
            runtime.pairing_token = None;
            runtime.pairing_expires_at = None;
            runtime.generation
        };
        let _ = app.emit("remote-access-updated", self.status().await);

        if !config.enabled {
            return;
        }

        let cancellation = CancellationToken::new();
        *self.cancellation.lock().await = Some(cancellation.clone());
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let retry_delays = [1_u64, 2, 5, 10, 30];
            let mut retry_index = 0_usize;

            loop {
                if cancellation.is_cancelled() {
                    break;
                }

                let connection = tokio::select! {
                    _ = cancellation.cancelled() => break,
                    result = connect_async(config.endpoint.as_str()) => result,
                };

                let socket = match connection {
                    Ok((socket, _)) => socket,
                    Err(error) => {
                        {
                            let mut runtime = manager.runtime.write().await;
                            if runtime.generation != generation {
                                break;
                            }
                            runtime.connected = false;
                            runtime.peer_online = false;
                            runtime.last_error = Some(error.to_string());
                        }
                        let _ = app.emit("remote-access-updated", manager.status().await);
                        let delay = retry_delays[retry_index.min(retry_delays.len() - 1)];
                        retry_index = (retry_index + 1).min(retry_delays.len() - 1);
                        tokio::select! {
                            _ = cancellation.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                        }
                        continue;
                    }
                };

                retry_index = 0;
                let (mut sink, mut stream) = socket.split();
                let hello = json!({
                    "version": REMOTE_PROTOCOL_VERSION,
                    "type": "tunnel.hello",
                    "role": "desktop",
                    "tunnel_id": config.tunnel_id,
                    "credential": config.credential,
                });
                if let Err(error) = sink.send(Message::Text(hello.to_string().into())).await {
                    let mut runtime = manager.runtime.write().await;
                    if runtime.generation == generation {
                        runtime.last_error = Some(error.to_string());
                    }
                    continue;
                }

                /*
                // 旧版按 thread-updated 做全线程差异扫描的实现已废弃。
                let mut snapshots: HashMap<String, String> = HashMap::new();
                let mut snapshot_interval = tokio::time::interval(Duration::from_millis(650));
                snapshot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let (thread_updated_tx, mut thread_updated_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                let thread_updated_listener = app.listen("thread-updated", move |event| {
                    let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else { return; };
                    let Some(thread_id) = payload.get("threadId").and_then(Value::as_str) else { return; };
                    let _ = thread_updated_tx.send(thread_id.to_string());
                });
                let mut message_versions: HashMap<String, HashMap<String, String>> = HashMap::new();
                */
                // 只监听助手消息完成事件；流式块和线程状态更新不会进入远程推送通道。
                let (completed_message_tx, mut completed_message_rx) =
                    tokio::sync::mpsc::unbounded_channel::<Value>();
                let completed_message_listener =
                    app.listen("assistant-message-completed", move |event| {
                        let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                            return;
                        };
                        let _ = completed_message_tx.send(payload);
                    });
                let mut disconnected_error: Option<String> = None;
                let mut batch_cleanup_interval = tokio::time::interval(Duration::from_secs(60));
                batch_cleanup_interval
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        _ = cancellation.cancelled() => {
                            let _ = sink.close().await;
                            break;
                        }
                        incoming = stream.next() => {
                            let Some(incoming) = incoming else {
                                disconnected_error = Some("WSS connection closed".to_string());
                                break;
                            };
                            let incoming = match incoming {
                                Ok(message) => message,
                                Err(error) => {
                                    disconnected_error = Some(error.to_string());
                                    break;
                                }
                            };
                            match incoming {
                                Message::Ping(payload) => {
                                    if let Err(error) = sink.send(Message::Pong(payload)).await {
                                        disconnected_error = Some(error.to_string());
                                        break;
                                    }
                                }
                                Message::Close(_) => {
                                    disconnected_error = Some("WSS connection closed".to_string());
                                    break;
                                }
                                Message::Text(text) => {
                                    let value: Value = match serde_json::from_str(text.as_str()) {
                                        Ok(value) => value,
                                        Err(_) => continue,
                                    };
                                    match value.get("type").and_then(Value::as_str) {
                                        Some("tunnel.ready") => {
                                            let peer_online = value.get("peer_online").and_then(Value::as_bool).unwrap_or(false);
                                            {
                                                let mut runtime = manager.runtime.write().await;
                                                if runtime.generation != generation { break; }
                                                runtime.connected = true;
                                                runtime.peer_online = peer_online;
                                                runtime.last_error = None;
                                            }
                                            let _ = app.emit("remote-access-updated", manager.status().await);
                                            continue;
                                        }
                                        Some("tunnel.peer_online") => {
                                            {
                                                let mut runtime = manager.runtime.write().await;
                                                if runtime.generation != generation { break; }
                                                runtime.peer_online = true;
                                            }
                                            let _ = app.emit("remote-access-updated", manager.status().await);
                                            continue;
                                        }
                                        Some("tunnel.peer_offline") => {
                                            {
                                                let mut runtime = manager.runtime.write().await;
                                                if runtime.generation != generation { break; }
                                                runtime.peer_online = false;
                                            }
                                            // 隧道确认全部移动对端离线后，清理在线设备；切换会话或离开页面不会触发此处。
                                            manager.online_devices.write().await.clear();
                                            let _ = app.emit("remote-access-updated", manager.status().await);
                                            continue;
                                        }
                                        _ => {}
                                    }

                                    let request: RemoteRequest = match serde_json::from_value(value) {
                                        Ok(request) => request,
                                        Err(error) => {
                                            let response = json!({
                                                "version": REMOTE_PROTOCOL_VERSION,
                                                "kind": "response",
                                                "id": "invalid",
                                                "ok": false,
                                                "error": { "code": "invalid_request", "message": error.to_string() }
                                            });
                                            let _ = sink.send(Message::Text(response.to_string().into())).await;
                                            continue;
                                        }
                                    };

                                    if request.version != REMOTE_PROTOCOL_VERSION
                                        || request.kind != "request"
                                        || request.id.trim().is_empty()
                                        || request.id.len() > 128
                                    {
                                        let response = json!({
                                            "version": REMOTE_PROTOCOL_VERSION,
                                            "kind": "response",
                                            "id": request.id,
                                            "ok": false,
                                            "error": { "code": "unauthorized", "message": "Invalid remote request or credential" }
                                        });
                                        let _ = sink.send(Message::Text(response.to_string().into())).await;
                                        continue;
                                    }

                                    let auth_valid = if request.method == "device.pair" {
                                        let runtime = manager.runtime.read().await;
                                        runtime.pairing_token.as_deref() == Some(request.auth.as_str())
                                            && runtime.pairing_expires_at
                                                .map(|expires_at| expires_at > Utc::now())
                                                .unwrap_or(false)
                                    } else {
                                        let runtime = manager.runtime.read().await;
                                        runtime.config.devices.iter().any(|device| {
                                            !device.credential.is_empty()
                                                && request.auth == device.credential
                                        }) || (!runtime.config.device_credential.is_empty()
                                            && request.auth == runtime.config.device_credential)
                                    };
                                    if !auth_valid {
                                        let response = json!({
                                            "version": REMOTE_PROTOCOL_VERSION,
                                            "kind": "response",
                                            "id": request.id,
                                            "ok": false,
                                            "error": { "code": "unauthorized", "message": "Remote credential is invalid or expired" }
                                        });
                                        let _ = sink.send(Message::Text(response.to_string().into())).await;
                                        continue;
                                    }

                                    // 设备归属只由本次请求已验证的认证凭据解析，绝不采用手机 payload 的 device_id。
                                    let request_device_id =
                                        authenticated_device_id(&manager, &request.auth).await;

                                    let result: Result<Value, String> = match request.method.as_str() {
                                        "device.pair" => {
                                            let device_credential = format!(
                                                "{}{}",
                                                uuid::Uuid::new_v4().simple(),
                                                uuid::Uuid::new_v4().simple()
                                            );
                                            let now = Utc::now().to_rfc3339();
                                            let device = RemoteDeviceConfig {
                                                id: format!("mobile_{}", uuid::Uuid::new_v4().simple()),
                                                name: device_name_from_payload(&request.payload),
                                                credential: device_credential.clone(),
                                                paired_at: now.clone(),
                                                last_connected_at: now,
                                            };
                                            let device_to_save = device.clone();
                                            let _write_guard = state.config_write_lock.lock().await;
                                            let saved = tokio::task::spawn_blocking(move || {
                                                AppConfig::mutate(|app_config| {
                                                    app_config.remote_access.devices.push(device_to_save);
                                                    Ok(())
                                                })
                                            })
                                            .await
                                            .map_err(|error| error.to_string())
                                            .and_then(|result| result.map_err(|error| error.to_string()));
                                            match saved {
                                                Ok(()) => {
                                                    let mut runtime = manager.runtime.write().await;
                                                    runtime.config.devices.push(device.clone());
                                                    runtime.pairing_token = None;
                                                    runtime.pairing_expires_at = None;
                                                    drop(runtime);
                                                    // 配对成功即视为该设备在线，避免首次配对后等待额外 identify 才能收到完成事件。
                                                    manager.online_devices.write().await.insert(device.id.clone());
                                                    let _ = app.emit("remote-access-updated", manager.status().await);
                                                    Ok(json!({
                                                        "device_credential": device_credential,
                                                        "device_id": device.id,
                                                    }))
                                                }
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "device.identify" => {
                                            let credential = request.auth.clone();
                                            let device_name = device_name_from_payload(&request.payload);
                                            let connected_at = Utc::now().to_rfc3339();
                                            let credential_to_save = credential.clone();
                                            let name_to_save = device_name.clone();
                                            let connected_at_to_save = connected_at.clone();
                                            let _write_guard = state.config_write_lock.lock().await;
                                            let saved = tokio::task::spawn_blocking(move || {
                                                AppConfig::mutate(|app_config| {
                                                    if let Some(device) = app_config
                                                        .remote_access
                                                        .devices
                                                        .iter_mut()
                                                        .find(|device| device.credential == credential_to_save)
                                                    {
                                                        device.name = name_to_save;
                                                        device.last_connected_at = connected_at_to_save;
                                                        return Ok(Some(device.clone()));
                                                    }
                                                    if app_config.remote_access.device_credential == credential_to_save {
                                                        let device = RemoteDeviceConfig {
                                                            id: format!("mobile_{}", uuid::Uuid::new_v4().simple()),
                                                            name: name_to_save,
                                                            credential: credential_to_save,
                                                            paired_at: connected_at_to_save.clone(),
                                                            last_connected_at: connected_at_to_save,
                                                        };
                                                        app_config.remote_access.device_credential.clear();
                                                        app_config.remote_access.devices.push(device.clone());
                                                        return Ok(Some(device));
                                                    }
                                                    Ok(None)
                                                })
                                            })
                                            .await
                                            .map_err(|error| error.to_string())
                                            .and_then(|result| result.map_err(|error| error.to_string()));
                                            match saved {
                                                Ok(Some(device)) => {
                                                    let mut runtime = manager.runtime.write().await;
                                                    runtime.config.device_credential.clear();
                                                    if let Some(current) = runtime
                                                        .config
                                                        .devices
                                                        .iter_mut()
                                                        .find(|current| current.id == device.id)
                                                    {
                                                        *current = device.clone();
                                                    } else {
                                                        runtime.config.devices.push(device.clone());
                                                    }
                                                    drop(runtime);
                                                    // `device.identify` 是设备在线注册点，重连时会覆盖同一 deviceId。
                                                    manager.online_devices.write().await.insert(device.id.clone());
                                                    let _ = app.emit("remote-access-updated", manager.status().await);
                                                    Ok(json!({ "device_id": device.id }))
                                                }
                                                Ok(None) => Err("Remote device is no longer authorized".to_string()),
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "desktop.get_status" => Ok(json!({
                                            "version": app.package_info().version.to_string(),
                                            "online": true,
                                        })),
                                        "engine.list" => crate::commands::engines::list_local_engine_infos(&state)
                                            .await
                                            .map_err(|error| error.to_string())
                                            .and_then(|items| serde_json::to_value(items).map_err(|error| error.to_string())),
                                        "workspace.list" => {
                                            let db = state.db.clone();
                                            tokio::task::spawn_blocking(move || crate::db::workspaces::list_workspaces(&db))
                                                .await
                                                .map_err(|error| error.to_string())
                                                .and_then(|result| result.map_err(|error| error.to_string()))
                                                .and_then(|items| serde_json::to_value(items).map_err(|error| error.to_string()))
                                        }
                                        "thread.list" => {
                                            let workspace_id = request.payload.get("workspace_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "workspace_id is required".to_string());
                                            match workspace_id {
                                                Ok(workspace_id) => {
                                                    let db = state.db.clone();
                                                    tokio::task::spawn_blocking(move || crate::db::threads::list_threads_for_workspace(&db, &workspace_id))
                                                        .await
                                                        .map_err(|error| error.to_string())
                                                        .and_then(|result| result.map_err(|error| error.to_string()))
                                                        .and_then(|items| serde_json::to_value(items).map_err(|error| error.to_string()))
                                                }
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "thread.create" => {
                                            let workspace_id = request.payload.get("workspace_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "workspace_id is required".to_string());
                                            let engine_id = request.payload.get("engine_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .unwrap_or("codex")
                                                .to_string();
                                            let model_id = request.payload.get("model_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .unwrap_or("gpt-5.4")
                                                .to_string();
                                            let reasoning_effort = request.payload.get("reasoning_effort")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string);
                                            let service_tier = request.payload.get("service_tier")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string);
                                            match workspace_id {
                                                Ok(workspace_id) => create_thread_with_defaults(
                                                    &state,
                                                    workspace_id,
                                                    None,
                                                    engine_id,
                                                    model_id,
                                                    "新会话".to_string(),
                                                    reasoning_effort,
                                                    service_tier,
                                                )
                                                .await
                                                .and_then(|thread| serde_json::to_value(thread).map_err(|error| error.to_string())),
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "thread.set_autonomy_preset" => {
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            let preset = request.payload.get("preset")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| matches!(*value, "inherit" | "read-only" | "ask" | "auto" | "full"))
                                                .map(str::to_string)
                                                .ok_or_else(|| "preset is invalid".to_string());
                                            match (thread_id, preset) {
                                                (Ok(thread_id), Ok(preset)) => {
                                                    async {
                                                    // 权限业务统一经过 CLI 工厂和 CLI 实现层；本地、SSH
                                                    // 只由实现层内部决定具体的生命周期和数据来源。
                                                    let db = state.db.clone();
                                                    let query_thread_id = thread_id.clone();
                                                    let thread = tokio::task::spawn_blocking(move || {
                                                        crate::db::threads::get_thread(&db, &query_thread_id)
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()))?
                                                    .ok_or_else(|| "thread not found".to_string())?;
                                                    let cli = CliToolFactory::new(state.clone())
                                                        .create(&thread.engine_id)
                                                        .map_err(|error| error.to_string())?;
                                                    let context = cli
                                                        .execution_context(Some(&thread.workspace_id))
                                                        .await
                                                        .map_err(|error| error.to_string())?;
                                                    let mut permissions = cli
                                                        .get_permissions(&context, &thread)
                                                        .await
                                                        .map_err(|error| error.to_string())?;
                                                    permissions.insert(
                                                        "autonomyPreset".to_string(),
                                                        if preset == "inherit" {
                                                            json!([])
                                                        } else {
                                                            json!([preset])
                                                        },
                                                    );
                                                    cli.set_permissions(&context, &thread, permissions)
                                                        .await
                                                        .map_err(|error| error.to_string())?;
                                                    let db = state.db.clone();
                                                    let result_thread_id = thread_id.clone();
                                                    let saved_thread = tokio::task::spawn_blocking(move || {
                                                        crate::db::threads::get_thread(&db, &result_thread_id)
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()))?
                                                    .ok_or_else(|| "thread not found after permission update".to_string())?;
                                                    serde_json::to_value(saved_thread).map_err(|error| error.to_string())
                                                    /*
                                                    // 旧实现已停用：Controller 直接修改 engine_metadata_json，
                                                    // 并根据 Codex 外部沙箱分支拼接三种 CLI 的原始字段。
                                                    let db = state.db.clone();
                                                    let codex = CliToolFactory::new(state.clone())
                                                        .create("codex")
                                                        .expect("Codex CLI factory mapping must exist");
                                                    let codex_uses_external_sandbox = match codex.execution_context(None).await {
                                                        Ok(context) => {
                                                            let cli: &dyn CliTool = codex.as_ref();
                                                            cli.uses_external_sandbox(&context).await.unwrap_or(false)
                                                        }
                                                        Err(_) => false,
                                                    };
                                                    tokio::task::spawn_blocking(move || {
                                                        let thread = crate::db::threads::get_thread(&db, &thread_id)?
                                                            .ok_or_else(|| anyhow::anyhow!("thread not found"))?;
                                                        let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
                                                        if !metadata.is_object() { metadata = json!({}); }
                                                        let values = metadata.as_object_mut().expect("metadata must be an object");
                                                        values.remove("permissionProfile");
                                                        values.remove("sandboxApprovalPolicy");
                                                        values.remove("sandboxMode");
                                                        values.remove("sandboxAllowNetwork");
                                                        values.remove("claudePermissionMode");
                                                        values.remove("opencodePermissionMode");
                                                        if preset != "inherit" {
                                                            match thread.engine_id.as_str() {
                                                                "codex" => {
                                                                    let (approval, sandbox, allow_network) = match preset.as_str() {
                                                                        "read-only" => ("untrusted", (!codex_uses_external_sandbox).then_some("read-only"), false),
                                                                        "ask" => ("on-request", (!codex_uses_external_sandbox).then_some("workspace-write"), false),
                                                                        "auto" => ("on-request", (!codex_uses_external_sandbox).then_some("workspace-write"), true),
                                                                        "full" => ("never", Some("danger-full-access"), true),
                                                                        _ => unreachable!(),
                                                                    };
                                                                    values.insert("sandboxApprovalPolicy".to_string(), json!(approval));
                                                                    if let Some(sandbox) = sandbox { values.insert("sandboxMode".to_string(), json!(sandbox)); }
                                                                    values.insert("sandboxAllowNetwork".to_string(), json!(allow_network));
                                                                }
                                                                "claude" => {
                                                                    let (permission_mode, sandbox, allow_network) = match preset.as_str() {
                                                                        "read-only" => ("default", "read-only", false),
                                                                        "ask" => ("default", "workspace-write", false),
                                                                        "auto" => ("acceptEdits", "workspace-write", true),
                                                                        "full" => ("bypassPermissions", "danger-full-access", true),
                                                                        _ => unreachable!(),
                                                                    };
                                                                    values.insert("claudePermissionMode".to_string(), json!(permission_mode));
                                                                    values.insert("sandboxMode".to_string(), json!(sandbox));
                                                                    values.insert("sandboxAllowNetwork".to_string(), json!(allow_network));
                                                                }
                                                                "opencode" => {
                                                                    let permission_mode = match preset.as_str() {
                                                                        "read-only" | "ask" => "default",
                                                                        "auto" | "full" => "bypassPermissions",
                                                                        _ => unreachable!(),
                                                                    };
                                                                    values.insert("opencodePermissionMode".to_string(), json!(permission_mode));
                                                                }
                                                                _ => return Err(anyhow::anyhow!("unsupported engine")),
                                                            }
                                                        }
                                                        crate::db::threads::update_engine_metadata(&db, &thread_id, &metadata)?;
                                                        crate::db::threads::get_thread(&db, &thread_id)?
                                                            .ok_or_else(|| anyhow::anyhow!("thread not found after update"))
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()))
                                                    .and_then(|thread| serde_json::to_value(thread).map_err(|error| error.to_string()))
                                                    */
                                                    }.await
                                                }
                                                (Err(error), _) | (_, Err(error)) => Err(error),
                                            }
                                        }
                                        "message.list" => {
                                            // 已恢复游标分页；旧全量实现保留追溯。
                                            // 旧实现：一次读取完整历史并按内联逻辑转换后整体响应，超大历史会撑爆 Relay 单帧上限。
                                            // /*
                                            // let thread_id = request.payload.get("thread_id")
                                            //     .and_then(Value::as_str)
                                            //     .map(str::trim)
                                            //     .filter(|value| !value.is_empty())
                                            //     .map(str::to_string)
                                            //     .ok_or_else(|| "thread_id is required".to_string());
                                            // match thread_id {
                                            //     Ok(thread_id) => {
                                            //         // 历史分页实现已停用：手机首次打开会话时直接取得全部消息。
                                            //         /*
                                            //         let cursor = request.payload.get("cursor")
                                            //             .filter(|value| !value.is_null())
                                            //             .cloned()
                                            //             .map(serde_json::from_value::<MessageWindowCursorDto>)
                                            //             .transpose()
                                            //             .map_err(|error| error.to_string());
                                            //         */
                                            //         let db = state.db.clone();
                                            //         let query_thread_id = thread_id.clone();
                                            //         let messages = tokio::task::spawn_blocking(move || {
                                            //             crate::db::messages::get_thread_messages(&db, &query_thread_id)
                                            //         })
                                            //         .await
                                            //         .map_err(|error| error.to_string())
                                            //         .and_then(|result| result.map_err(|error| error.to_string()));
                                            //         match messages {
                                            //             Ok(messages) => {
                                            //                 // 历史查询不参与实时消息投递。消息本身全部来自数据库；附件仅暴露
                                            //                 // 手机显示需要的元数据，桌面绝对路径不发送到远端设备。
                                            //                 /*
                                            //                 let versions = messages.iter()
                                            //                     .map(|message| serde_json::to_string(message)
                                            //                         .map(|value| (message.id.clone(), value)))
                                            //                     .collect::<Result<HashMap<String, String>, _>>()
                                            //                     .map_err(|error| error.to_string());
                                            //                 match versions {
                                            //                     Ok(versions) => {
                                            //                         message_versions.insert(thread_id, versions);
                                            //                         Ok(json!({ "messages": messages }))
                                            //                     }
                                            //                     Err(error) => Err(error),
                                            //                 }
                                            //                 */
                                            //                 let mobile_messages = messages
                                            //                     .into_iter()
                                            //                     .map(|message| {
                                            //                         let attachments = message
                                            //                             .blocks
                                            //                             .as_ref()
                                            //                             .and_then(Value::as_array)
                                            //                             .map(|blocks| {
                                            //                                 blocks
                                            //                                     .iter()
                                            //                                     .enumerate()
                                            //                                     .filter_map(|(index, block)| {
                                            //                                         (block
                                            //                                             .get("type")
                                            //                                             .and_then(Value::as_str)
                                            //                                             == Some("attachment"))
                                            //                                             .then(|| {
                                            //                                                 let file_name = block
                                            //                                                     .get("fileName")
                                            //                                                     .and_then(Value::as_str)
                                            //                                                     .unwrap_or("附件");
                                            //                                                 let mime_type = block
                                            //                                                     .get("mimeType")
                                            //                                                     .and_then(Value::as_str)
                                            //                                                     .unwrap_or_default();
                                            //                                                 json!({
                                            //                                                     "id": format!("{}:attachment:{index}", message.id),
                                            //                                                     "fileName": file_name,
                                            //                                                     // 历史附件只能由桌面端读取；手机端不能取得或使用桌面绝对路径。
                                            //                                                     "filePath": "",
                                            //                                                     "sizeBytes": block.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0),
                                            //                                                     "mimeType": mime_type,
                                            //                                                     "source": if mime_type.starts_with("image/") { "image" } else { "file" },
                                            //                                                     "remoteAttachmentIndex": index,
                                            //                                                 })
                                            //                                             })
                                            //                                     })
                                            //                                     .collect::<Vec<_>>()
                                            //                             })
                                            //                             .unwrap_or_default();
                                            //                         let mut value = serde_json::to_value(message)
                                            //                             .map_err(|error| error.to_string())?;
                                            //                         // blocks 仍供手机渲染文本，但其中的桌面附件路径不能泄露到远端设备。
                                            //                         if let Some(blocks) = value
                                            //                             .get_mut("blocks")
                                            //                             .and_then(Value::as_array_mut)
                                            //                         {
                                            //                             for block in blocks {
                                            //                                 if block.get("type").and_then(Value::as_str)
                                            //                                     == Some("attachment")
                                            //                                 {
                                            //                                     if let Some(object) = block.as_object_mut() {
                                            //                                         object.insert(
                                            //                                             "filePath".to_string(),
                                            //                                             Value::String(String::new()),
                                            //                                         );
                                            //                                     }
                                            //                                 }
                                            //                             }
                                            //                         }
                                            //                         if !attachments.is_empty() {
                                            //                             if let Some(object) = value.as_object_mut() {
                                            //                                 object.insert(
                                            //                                     "attachments".to_string(),
                                            //                                     Value::Array(attachments),
                                            //                                 );
                                            //                             }
                                            //                         }
                                            //                         Ok::<Value, String>(value)
                                            //                     })
                                            //                     .collect::<Result<Vec<_>, _>>();
                                            //                 mobile_messages.map(|messages| json!({ "messages": messages }))
                                            //             }
                                            //             Err(error) => Err(error),
                                            //         }
                                            //     }
                                            //     Err(error) => Err(error),
                                            // }
                                            // */
                                            // 新实现：按游标分页读取历史，并做页级字节预算封顶。
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            let limit = request.payload.get("limit")
                                                .and_then(Value::as_u64)
                                                .and_then(|value| usize::try_from(value).ok())
                                                .unwrap_or(MESSAGE_WINDOW_DEFAULT_LIMIT)
                                                .clamp(1, MESSAGE_WINDOW_MAX_LIMIT);
                                            let cursor = match request.payload.get("cursor") {
                                                None | Some(Value::Null) => Ok(None),
                                                Some(value) => serde_json::from_value::<MessageWindowCursorDto>(value.clone())
                                                    .map(Some)
                                                    .map_err(|error| format!("cursor is invalid: {error}")),
                                            };
                                            match (thread_id, cursor) {
                                                (Ok(thread_id), Ok(cursor)) => {
                                                    let db = state.db.clone();
                                                    let query_thread_id = thread_id.clone();
                                                    let window = tokio::task::spawn_blocking(move || {
                                                        crate::db::messages::get_thread_messages_window_with_row_ids(
                                                            &db,
                                                            &query_thread_id,
                                                            cursor.as_ref(),
                                                            limit,
                                                        )
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()));
                                                    match window {
                                                        Ok((window, row_ids)) => {
                                                            // 单条消息转手机端结构：附件仅暴露元数据并剥离输出类字段。
                                                            let converted = window
                                                                .messages
                                                                .into_iter()
                                                                .map(mobile_message_value)
                                                                .collect::<Result<Vec<_>, _>>();
                                                            match converted {
                                                                Ok(mobile_messages) => {
                                                                    // 页级字节预算封顶：从最旧端丢弃直到整页不超过预算，并重算 nextCursor。
                                                                    let (mobile_messages, next_cursor) = enforce_message_page_budget(
                                                                        mobile_messages,
                                                                        row_ids,
                                                                        window.next_cursor,
                                                                        MESSAGE_PAGE_BUDGET_BYTES,
                                                                    );
                                                                    Ok(json!({ "messages": mobile_messages, "nextCursor": next_cursor }))
                                                                }
                                                                Err(error) => Err(error),
                                                            }
                                                        }
                                                        Err(error) => Err(error),
                                                    }
                                                }
                                                (Err(error), _) | (_, Err(error)) => Err(error),
                                            }
                                        }
                                        "message.attachment.preview" => {
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            let message_id = request.payload.get("message_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "message_id is required".to_string());
                                            let attachment_index = request.payload.get("attachment_index")
                                                .and_then(Value::as_u64)
                                                .and_then(|value| usize::try_from(value).ok())
                                                .ok_or_else(|| "attachment_index is required".to_string());
                                            match (thread_id, message_id, attachment_index) {
                                                (Ok(thread_id), Ok(message_id), Ok(attachment_index)) => {
                                                    let db = state.db.clone();
                                                    let query_message_id = message_id.clone();
                                                    let message = tokio::task::spawn_blocking(move || {
                                                        crate::db::messages::get_message(&db, &query_message_id)
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()));
                                                    match message {
                                                        Ok(Some(message)) if message.thread_id == thread_id => {
                                                            let attachment = message.blocks.as_ref()
                                                                .and_then(Value::as_array)
                                                                .and_then(|blocks| blocks.get(attachment_index))
                                                                .filter(|block| block.get("type").and_then(Value::as_str) == Some("attachment"));
                                                            let file_path = attachment
                                                                .and_then(|block| block.get("filePath"))
                                                                .and_then(Value::as_str)
                                                                .map(str::trim)
                                                                .filter(|value| !value.is_empty())
                                                                .map(str::to_string);
                                                            let mime_type = attachment
                                                                .and_then(|block| block.get("mimeType"))
                                                                .and_then(Value::as_str)
                                                                .map(str::to_string);
                                                            match file_path {
                                                                Some(file_path) => crate::commands::chat::read_attachment_preview(file_path, mime_type)
                                                                    .await
                                                                    .and_then(|preview| serde_json::to_value(preview).map_err(|error| error.to_string())),
                                                                None => Err("history attachment was not found".to_string()),
                                                            }
                                                        }
                                                        Ok(Some(_)) => Err("message does not belong to thread".to_string()),
                                                        Ok(None) => Err("message was not found".to_string()),
                                                        Err(error) => Err(error),
                                                    }
                                                }
                                                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
                                            }
                                        }
                                        "attachment.upload" => {
                                            /*
                                            // 旧版 Base64 分块上传协议已停用，代码保留以便兼容审计；
                                            // 新客户端必须通过 Relay HTTPS 上传并在 message.send 提交 attachment_key。
                                            let batch_id = parse_optional_batch_id(&request.payload);
                                            let upload_id = request.payload.get("upload_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty() && value.len() <= 128)
                                                .map(str::to_string)
                                                .ok_or_else(|| "upload_id is required".to_string());
                                            let file_name = request.payload.get("file_name")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty() && value.len() <= 255)
                                                .map(str::to_string)
                                                .ok_or_else(|| "file_name is required".to_string());
                                            let mime_type = request.payload.get("mime_type")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| {
                                                    value.len() <= 128
                                                        && value.split_once('/').is_some_and(|(kind, subtype)| !kind.is_empty() && !subtype.is_empty())
                                                        && value.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '+' | '-'))
                                                })
                                                .map(str::to_string)
                                                .ok_or_else(|| "attachment mime_type is invalid".to_string());
                                            let chunk_index = request.payload.get("chunk_index")
                                                .and_then(Value::as_u64)
                                                .filter(|value| *value <= u32::MAX as u64)
                                                .map(|value| value as u32)
                                                .ok_or_else(|| "chunk_index is required".to_string());
                                            let chunk_count = request.payload.get("chunk_count")
                                                .and_then(Value::as_u64)
                                                .filter(|value| *value > 0 && *value <= 64)
                                                .map(|value| value as u32)
                                                .ok_or_else(|| "chunk_count is invalid".to_string());
                                            let data_base64 = request.payload.get("data_base64")
                                                .and_then(Value::as_str)
                                                .filter(|value| !value.is_empty() && value.len() <= 300_000)
                                                .map(str::to_string)
                                                .ok_or_else(|| "attachment chunk is invalid".to_string());
                                            match (batch_id, upload_id, file_name, mime_type, chunk_index, chunk_count, data_base64) {
                                                (Err(error), _, _, _, _, _, _)
                                                | (_, Err(error), _, _, _, _, _)
                                                | (_, _, Err(error), _, _, _, _)
                                                | (_, _, _, Err(error), _, _, _)
                                                | (_, _, _, _, Err(error), _, _)
                                                | (_, _, _, _, _, Err(error), _)
                                                | (_, _, _, _, _, _, Err(error)) => Err(error),
                                                (Ok(Some(batch_id)), Ok(upload_id), Ok(file_name), Ok(mime_type), Ok(chunk_index), Ok(chunk_count), Ok(data_base64)) => {
                                                    match request_device_id.clone() {
                                                        Some(device_id) => process_batched_attachment_upload(
                                                            &manager,
                                                            device_id,
                                                            batch_id,
                                                            upload_id,
                                                            file_name,
                                                            mime_type,
                                                            chunk_index,
                                                            chunk_count,
                                                            data_base64,
                                                        )
                                                        .await,
                                                        None => Err("authenticated device identity is required".to_string()),
                                                    }
                                                }
                                                (Ok(None), Ok(upload_id), Ok(file_name), Ok(mime_type), Ok(chunk_index), Ok(chunk_count), Ok(data_base64)) => {
                                                    // 旧客户端缺失 batch_id 时继续使用原有上传路径。
                                                    let mut uploads = manager.uploads.lock().await;
                                                    let completed = (|| -> Result<Option<RemoteUploadState>, String> {
                                                        if chunk_index >= chunk_count {
                                                            return Err("chunk_index is invalid".to_string());
                                                        }
                                                        if chunk_index == 0 {
                                                            uploads.insert(upload_id.clone(), RemoteUploadState {
                                                                device_id: request_device_id.clone().unwrap_or_else(|| "legacy".to_string()),
                                                                batch_id: String::new(),
                                                                file_name: file_name.clone(),
                                                                mime_type: mime_type.clone(),
                                                                chunk_count,
                                                                next_chunk: 0,
                                                                data_base64: String::new(),
                                                                last_activity: Instant::now(),
                                                            });
                                                        }
                                                        let upload = uploads.get_mut(&upload_id)
                                                            .ok_or_else(|| "attachment upload was not initialized".to_string())?;
                                                        if upload.file_name != file_name
                                                            || upload.mime_type != mime_type
                                                            || upload.chunk_count != chunk_count
                                                            || upload.next_chunk != chunk_index
                                                        {
                                                            uploads.remove(&upload_id);
                                                            return Err("attachment chunks are out of sequence".to_string());
                                                        }
                                                        upload.data_base64.push_str(&data_base64);
                                                        upload.last_activity = Instant::now();
                                                        if upload.data_base64.len() > 14_000_000 {
                                                            uploads.remove(&upload_id);
                                                            return Err("attachment exceeds the 10 MB limit".to_string());
                                                        }
                                                        upload.next_chunk += 1;
                                                        if upload.next_chunk == upload.chunk_count {
                                                            Ok(uploads.remove(&upload_id))
                                                        } else {
                                                            Ok(None)
                                                        }
                                                    })();
                                                    drop(uploads);
                                                    match completed {
                                                        Ok(Some(upload)) => {
                                                            let original_file_name = upload.file_name.clone();
                                                            if upload.mime_type.starts_with("image/") {
                                                                save_pasted_image_attachment(
                                                                    upload.file_name,
                                                                    upload.mime_type,
                                                                    upload.data_base64,
                                                                )
                                                                .await
                                                                .and_then(|mut attachment| {
                                                                    attachment.file_name = original_file_name;
                                                                    serde_json::to_value(attachment).map_err(|error| error.to_string())
                                                                })
                                                            } else {
                                                                let decoded = BASE64
                                                                    .decode(upload.data_base64.trim())
                                                                    .map_err(|_| "attachment data is not valid base64".to_string());
                                                                match decoded {
                                                                    Ok(bytes) if bytes.is_empty() => Err("attachment data is empty".to_string()),
                                                                    Ok(bytes) if bytes.len() > REMOTE_ATTACHMENT_MAX_BYTES => Err("attachment exceeds the 10 MB limit".to_string()),
                                                                    Ok(bytes) => {
                                                                        let extension = Path::new(&original_file_name)
                                                                            .extension()
                                                                            .and_then(|value| value.to_str())
                                                                            .filter(|value| !value.is_empty() && value.len() <= 24 && value.chars().all(|character| character.is_ascii_alphanumeric()))
                                                                            .map(|value| format!(".{value}"))
                                                                            .unwrap_or_default();
                                                                        let stored_file_name = format!("mobile-file-{}{}", Uuid::new_v4().simple(), extension);
                                                                        let attachment_dir = crate::runtime_env::app_data_dir()
                                                                            .join("attachments")
                                                                            .join("mobile-files");
                                                                        match tokio_fs::create_dir_all(&attachment_dir).await {
                                                                            Ok(()) => {
                                                                                let file_path = attachment_dir.join(stored_file_name);
                                                                                match tokio_fs::write(&file_path, &bytes).await {
                                                                                    Ok(()) => serde_json::to_value(ChatAttachmentPayload {
                                                                                        file_name: original_file_name,
                                                                                        file_path: file_path.display().to_string(),
                                                                                        size_bytes: bytes.len() as u64,
                                                                                        mime_type: Some(upload.mime_type),
                                                                                        browser_annotation: None,
                                                                                    }).map_err(|error| error.to_string()),
                                                                                    Err(error) => Err(format!("failed to save mobile attachment: {error}")),
                                                                                }
                                                                            }
                                                                            Err(error) => Err(format!("failed to create mobile attachment directory: {error}")),
                                                                        }
                                                                    }
                                                                    Err(error) => Err(error),
                                                                }
                                                            }
                                                        }
                                                        Ok(None) => Ok(json!({ "complete": false })),
                                                        Err(error) => Err(error),
                                                    }
                                                }
                                            }
                                            */
                                            Err("attachment.upload is deprecated; use Relay attachment_key".to_string())
                                        }
                                        "attachment.batch.abort" => {
                                            match parse_optional_batch_id(&request.payload) {
                                                Ok(Some(batch_id)) => match request_device_id.clone() {
                                                    Some(device_id) => {
                                                        manager.abort_batch(&device_id, &batch_id).await;
                                                        Ok(json!({ "aborted": true }))
                                                    }
                                                    None => Err("authenticated device identity is required".to_string()),
                                                },
                                                Ok(None) => Err("batch_id is required".to_string()),
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "message.send" => {
                                            let batch_id = parse_optional_batch_id(&request.payload);
                                            let payload_device_id = parse_optional_device_id(&request.payload);
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            let message = request.payload.get("message")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .unwrap_or_default()
                                                .to_string();
                                            let model_id = request.payload.get("model_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string);
                                            let reasoning_effort = request.payload.get("reasoning_effort")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string);
                                            let attachment_inputs = parse_remote_attachment_inputs(&request.payload);
                                            match (batch_id, payload_device_id, thread_id, attachment_inputs) {
                                                (Ok(batch_id), Ok(payload_device_id), Ok(thread_id), Ok(attachment_inputs)) => {
                                                    let had_attachments = attachment_inputs.is_some();
                                                    let inputs = attachment_inputs.unwrap_or_default();
                                                    let has_relay_key = inputs.iter().any(|input| {
                                                        input
                                                            .attachment_key
                                                            .as_deref()
                                                            .map(str::trim)
                                                            .filter(|value| !value.is_empty())
                                                            .is_some()
                                                    });
                                                    if message.len() > 100_000 {
                                                        Err("message is too large".to_string())
                                                    } else if message.is_empty() && inputs.is_empty() {
                                                        Err("message or attachment is required".to_string())
                                                    } else if has_relay_key {
                                                        match (
                                                            batch_id.as_deref(),
                                                            payload_device_id.as_deref(),
                                                            request_device_id.clone(),
                                                        ) {
                                                            (Some(batch_id), Some(payload_device_id), Some(device_id)) => {
                                                                if payload_device_id != device_id {
                                                                    Err("message device_id does not match authenticated device".to_string())
                                                                } else if inputs.len() > 10
                                                                    || inputs.iter().any(|input| {
                                                                        input.attachment_key.is_none()
                                                                            || input.file_path.is_some()
                                                                    })
                                                                {
                                                                    Err("all mobile attachments must use attachment_key".to_string())
                                                                } else {
                                                                    let relay_config = {
                                                                        let runtime = manager.runtime.read().await;
                                                                        (
                                                                            runtime.config.endpoint.clone(),
                                                                            runtime.config.tunnel_id.clone(),
                                                                            runtime.config.credential.clone(),
                                                                        )
                                                                    };
                                                                    let attachment_keys = inputs
                                                                        .iter()
                                                                        .filter_map(|input| input.attachment_key.clone())
                                                                        .collect::<Vec<_>>();
                                                                    let fetched = fetch_relay_attachments(
                                                                        &manager,
                                                                        &relay_config.0,
                                                                        &relay_config.1,
                                                                        &relay_config.2,
                                                                        &device_id,
                                                                        batch_id,
                                                                        &inputs,
                                                                    )
                                                                    .await;
                                                                    match fetched {
                                                                        Ok(attachments) => {
                                                                            match manager
                                                                                .validate_batch_attachments(
                                                                                    &device_id,
                                                                                    batch_id,
                                                                                    &attachments,
                                                                                )
                                                                                .await
                                                                            {
                                                                                Ok(()) => {
                                                                                    let send_result = send_message_inner(
                                                                                        app.clone(),
                                                                                        &state,
                                                                                        thread_id,
                                                                                        message,
                                                                                        model_id,
                                                                                        reasoning_effort,
                                                                                        Some(attachments),
                                                                                        None,
                                                                                        Some(false),
                                                                                        Some(request.id.clone()),
                                                                                        None,
                                                                                        None,
                                                                                    )
                                                                                    .await;
                                                                                    match send_result {
                                                                                        Ok(assistant_message_id) => {
                                                                                            manager.commit_batch(&device_id, batch_id).await;
                                                                                            let client = reqwest::Client::new();
                                                                                            for attachment_key in attachment_keys {
                                                                                                if let Err(error) = delete_relay_attachment(
                                                                                                    &client,
                                                                                                    &relay_config.0,
                                                                                                    &relay_config.1,
                                                                                                    &relay_config.2,
                                                                                                    &attachment_key,
                                                                                                )
                                                                                                .await
                                                                                                {
                                                                                                    log::warn!("failed to delete relay attachment {attachment_key}: {error}");
                                                                                                }
                                                                                            }
                                                                                            Ok(json!({ "assistant_message_id": assistant_message_id }))
                                                                                        }
                                                                                        Err(error) => {
                                                                                            manager.abort_batch(&device_id, batch_id).await;
                                                                                            Err(error)
                                                                                        }
                                                                                    }
                                                                                }
                                                                                Err(error) => {
                                                                                    manager.abort_batch(&device_id, batch_id).await;
                                                                                    Err(error)
                                                                                }
                                                                            }
                                                                        }
                                                                        Err(error) => {
                                                                            manager.abort_batch(&device_id, batch_id).await;
                                                                            Err(error)
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            (None, _, _) => Err("batch_id is required for attachment_key".to_string()),
                                                            (Some(_), None, _) => Err("device_id is required for attachment_key".to_string()),
                                                            (Some(_), Some(_), None) => Err("authenticated device identity is required".to_string()),
                                                        }
                                                    } else {
                                                        if batch_id.is_some() && payload_device_id.is_some() && !inputs.is_empty() {
                                                            Err("mobile attachments must use attachment_key".to_string())
                                                        } else {
                                                            match legacy_attachment_payloads(&inputs) {
                                                                Ok(attachments) => {
                                                                    let attachments = if inputs.is_empty() && !had_attachments {
                                                                        None
                                                                    } else {
                                                                        Some(attachments)
                                                                    };
                                                                    if message.is_empty()
                                                                        && attachments.as_ref().map(|items| items.is_empty()).unwrap_or(true)
                                                                    {
                                                                        Err("message or attachment is required".to_string())
                                                                    } else {
                                                                        // 缺少 batch_id 或 attachment_key 的请求沿用旧 file_path 路径。
                                                                        send_message_inner(
                                                                            app.clone(),
                                                                            &state,
                                                                            thread_id,
                                                                            message,
                                                                            model_id,
                                                                            reasoning_effort,
                                                                            attachments,
                                                                            None,
                                                                            Some(false),
                                                                            Some(request.id.clone()),
                                                                            None,
                                                                            None,
                                                                        )
                                                                        .await
                                                                        .map(|assistant_message_id| json!({ "assistant_message_id": assistant_message_id }))
                                                                    }
                                                                }
                                                                Err(error) => Err(error),
                                                            }
                                                        }
                                                    }
                                                }
                                                (Err(error), _, _, _)
                                                | (_, Err(error), _, _)
                                                | (_, _, Err(error), _)
                                                | (_, _, _, Err(error)) => Err(error),
                                            }
                                        }
                                        "turn.stop" => {
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            match thread_id {
                                                Ok(thread_id) => cancel_turn_inner(&state, thread_id).await.map(|_| json!({})),
                                                Err(error) => Err(error),
                                            }
                                        }
                                        "thread.subscribe" => {
                                            // 兼容旧客户端，但不再按 threadId 控制完成消息投递。
                                            /*
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            match thread_id {
                                                Ok(thread_id) => {
                                                    // 先记录当前完整会话的版本。这样订阅与首次取数之间若有新消息，
                                                    // 后续只会推送那一条新消息，不会把整段历史再次当作实时消息发送。
                                                    let db = state.db.clone();
                                                    let query_thread_id = thread_id.clone();
                                                    let messages = tokio::task::spawn_blocking(move || {
                                                        crate::db::messages::get_thread_messages(&db, &query_thread_id)
                                                    })
                                                    .await
                                                    .map_err(|error| error.to_string())
                                                    .and_then(|result| result.map_err(|error| error.to_string()));
                                                    match messages {
                                                        Ok(messages) => {
                                                            let versions = messages.into_iter()
                                                                .map(|message| serde_json::to_string(&message)
                                                                    .map(|value| (message.id, value)))
                                                                .collect::<Result<HashMap<String, String>, _>>()
                                                                .map_err(|error| error.to_string());
                                                            match versions {
                                                                Ok(versions) => {
                                                                    manager.subscriptions.write().await
                                                                        .entry(thread_id.clone())
                                                                        .or_default()
                                                                        .insert(device_id.clone());
                                                                    message_versions.insert(thread_id, versions);
                                                                    Ok(json!({}))
                                                                }
                                                                Err(error) => Err(error),
                                                            }
                                                        }
                                                        Err(error) => Err(error),
                                                    }
                                                }
                                                Err(error) => Err(error),
                                            }
                                            */
                                            Ok(json!({ "deprecated": true }))
                                        }
                                        "thread.unsubscribe" => {
                                            // 离开会话不能取消设备注册；设备在线状态只由 identify/隧道离线维护。
                                            /*
                                            let thread_id = request.payload.get("thread_id")
                                                .and_then(Value::as_str)
                                                .map(str::trim)
                                                .filter(|value| !value.is_empty())
                                                .map(str::to_string)
                                                .ok_or_else(|| "thread_id is required".to_string());
                                            match thread_id {
                                                Ok(thread_id) => {
                                                    let mut subscriptions = manager.subscriptions.write().await;
                                                    if let Some(device_ids) = subscriptions.get_mut(&thread_id) {
                                                        device_ids.remove(&device_id);
                                                        if device_ids.is_empty() {
                                                            subscriptions.remove(&thread_id);
                                                            message_versions.remove(&thread_id);
                                                        }
                                                    }
                                                    Ok(json!({}))
                                                }
                                                Err(error) => Err(error),
                                            }
                                            */
                                            Ok(json!({ "deprecated": true }))
                                        }
                                        _ => Err(format!("unknown remote method: {}", request.method)),
                                    };

                                    let response = match result {
                                        Ok(payload) => json!({
                                            "version": REMOTE_PROTOCOL_VERSION,
                                            "kind": "response",
                                            "id": request.id,
                                            "ok": true,
                                            "payload": payload,
                                        }),
                                        Err(error) => json!({
                                            "version": REMOTE_PROTOCOL_VERSION,
                                            "kind": "response",
                                            "id": request.id,
                                            "ok": false,
                                            "error": { "code": "request_failed", "message": error },
                                        }),
                                    };
                                    if let Err(error) = sink.send(Message::Text(response.to_string().into())).await {
                                        disconnected_error = Some(error.to_string());
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        /*
                        Some(changed_thread_id) = thread_updated_rx.recv() => {
                            let device_ids = manager.subscriptions.read().await
                                .get(&changed_thread_id)
                                .cloned()
                                .unwrap_or_default();
                            if device_ids.is_empty() {
                                continue;
                            }
                            let db = state.db.clone();
                            let query_thread_id = changed_thread_id.clone();
                            let messages = tokio::task::spawn_blocking(move || {
                                crate::db::messages::get_thread_messages(&db, &query_thread_id)
                            })
                            .await
                            .ok()
                            .and_then(Result::ok);
                            let Some(messages) = messages else { continue; };
                            let changed_messages = {
                                let versions = message_versions.entry(changed_thread_id.clone()).or_default();
                                messages.into_iter().filter_map(|message| {
                                    let serialized = serde_json::to_string(&message).ok()?;
                                    if versions.get(&message.id) == Some(&serialized) {
                                        return None;
                                    }
                                    versions.insert(message.id.clone(), serialized);
                                    serde_json::to_value(message).ok()
                                }).collect::<Vec<_>>()
                            };
                            for message in changed_messages {
                                for device_id in &device_ids {
                                    let event = json!({
                                        "version": REMOTE_PROTOCOL_VERSION,
                                        "kind": "event",
                                        "event": "thread.message",
                                        "targetDeviceId": device_id,
                                        "payload": { "message": message },
                                    });
                                    if let Err(error) = sink.send(Message::Text(event.to_string().into())).await {
                                        disconnected_error = Some(error.to_string());
                                        break;
                                    }
                                }
                                if disconnected_error.is_some() { break; }
                            }
                            if disconnected_error.is_some() { break; }
                        }
                        */
                        _ = batch_cleanup_interval.tick() => {
                            manager.cleanup_expired_batches().await;
                        }
                        Some(completed_payload) = completed_message_rx.recv() => {
                            let device_ids = manager.online_devices.read().await.clone();
                            if device_ids.is_empty() {
                                continue;
                            }
                            for device_id in device_ids {
                                let Some(event) = build_completed_message_event(
                                    &device_id,
                                    &completed_payload,
                                ) else {
                                    continue;
                                };
                                if let Err(error) = sink.send(Message::Text(event.to_string().into())).await {
                                    disconnected_error = Some(error.to_string());
                                    break;
                                }
                            }
                            if disconnected_error.is_some() { break; }
                        }
                        /*
                        _ = snapshot_interval.tick() => {
                            let thread_ids: Vec<String> = manager.subscriptions.read().await.iter().cloned().collect();
                            for thread_id in thread_ids {
                                let db = state.db.clone();
                                let query_thread_id = thread_id.clone();
                                let snapshot = tokio::task::spawn_blocking(move || {
                                    let thread = crate::db::threads::get_thread(&db, &query_thread_id)?;
                                    let messages = crate::db::messages::get_thread_messages_window(
                                        &db,
                                        &query_thread_id,
                                        None,
                                        MESSAGE_WINDOW_MAX_LIMIT,
                                    )?;
                                    Ok::<Value, anyhow::Error>(json!({ "thread": thread, "messages": messages }))
                                })
                                .await
                                .ok()
                                .and_then(Result::ok);
                                let Some(snapshot) = snapshot else { continue; };
                                let serialized = snapshot.to_string();
                                if snapshots.get(&thread_id) == Some(&serialized) {
                                    continue;
                                }
                                snapshots.insert(thread_id.clone(), serialized);
                                let event = json!({
                                    "version": REMOTE_PROTOCOL_VERSION,
                                    "kind": "event",
                                    "event": "thread.snapshot",
                                    "payload": snapshot,
                                });
                                if let Err(error) = sink.send(Message::Text(event.to_string().into())).await {
                                    disconnected_error = Some(error.to_string());
                                    break;
                                }
                            }
                            if disconnected_error.is_some() { break; }
                        }
                        */
                    }
                }

                app.unlisten(completed_message_listener);

                {
                    let mut runtime = manager.runtime.write().await;
                    if runtime.generation != generation {
                        break;
                    }
                    runtime.connected = false;
                    runtime.peer_online = false;
                    runtime.last_error = disconnected_error;
                }
                manager.online_devices.write().await.clear();
                let _ = app.emit("remote-access-updated", manager.status().await);

                if cancellation.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
    }

    pub async fn shutdown(&self) {
        if let Some(cancellation) = self.cancellation.lock().await.take() {
            cancellation.cancel();
        }
        self.cleanup_all_batches().await;
    }
}

/// 截断字符串到指定字符数并追加截断标记。
fn truncate_mobile_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut output: String = value.chars().take(max_chars).collect();
    output.push_str(MOBILE_TRUNCATED_SUFFIX);
    output
}

/// 手机端只渲染 blocks 的 content/summary/message 字段；
/// 输出类字段不进隧道，可见文本按上限截断。
fn downsize_mobile_blocks(value: &mut Value) {
    let Some(blocks) = value.get_mut("blocks").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or_default();
        match block_type {
            "action" => {
                if let Some(object) = block.as_object_mut() {
                    object.remove("outputChunks");
                    object.remove("details");
                    if let Some(result) = object.get_mut("result").and_then(Value::as_object_mut) {
                        for key in ["output", "diff", "error"] {
                            if let Some(text) = result.get(key).and_then(Value::as_str).map(str::to_string) {
                                result.insert(
                                    key.to_string(),
                                    Value::String(truncate_mobile_text(&text, MOBILE_HIDDEN_OUTPUT_MAX_CHARS)),
                                );
                            }
                        }
                    }
                }
            }
            "approval" => {
                if let Some(object) = block.as_object_mut() {
                    object.remove("details");
                }
            }
            "diff" => {
                if let Some(object) = block.as_object_mut() {
                    if let Some(text) = object.get("diff").and_then(Value::as_str).map(str::to_string) {
                        object.insert("diff".to_string(), Value::String(truncate_mobile_text(&text, MOBILE_HIDDEN_OUTPUT_MAX_CHARS)));
                    }
                }
            }
            "thinking" => {
                if let Some(object) = block.as_object_mut() {
                    if let Some(text) = object.get("content").and_then(Value::as_str).map(str::to_string) {
                        object.insert("content".to_string(), Value::String(truncate_mobile_text(&text, MOBILE_HIDDEN_OUTPUT_MAX_CHARS)));
                    }
                }
            }
            "text" | "steer" | "error" | "notice" => {
                if let Some(object) = block.as_object_mut() {
                    for key in ["content", "message"] {
                        if let Some(text) = object.get(key).and_then(Value::as_str).map(str::to_string) {
                            object.insert(key.to_string(), Value::String(truncate_mobile_text(&text, MOBILE_VISIBLE_TEXT_MAX_CHARS)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// 单条消息转换为手机端结构：附件只保留元数据并剥离桌面路径，
/// 随后剥离手机端不渲染的输出类字段。
fn mobile_message_value(message: crate::models::MessageDto) -> Result<Value, String> {
    // 附件只保留手机显示需要的元数据，桌面绝对路径不发送到远端设备。
    let attachments = message
        .blocks
        .as_ref()
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .enumerate()
                .filter_map(|(index, block)| {
                    (block
                        .get("type")
                        .and_then(Value::as_str)
                        == Some("attachment"))
                        .then(|| {
                            let file_name = block
                                .get("fileName")
                                .and_then(Value::as_str)
                                .unwrap_or("附件");
                            let mime_type = block
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            json!({
                                "id": format!("{}:attachment:{index}", message.id),
                                "fileName": file_name,
                                // 历史附件只能由桌面端读取；手机端不能取得或使用桌面绝对路径。
                                "filePath": "",
                                "sizeBytes": block.get("sizeBytes").and_then(Value::as_u64).unwrap_or(0),
                                "mimeType": mime_type,
                                "source": if mime_type.starts_with("image/") { "image" } else { "file" },
                                "remoteAttachmentIndex": index,
                            })
                        })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut value = serde_json::to_value(message).map_err(|error| error.to_string())?;
    // blocks 仍供手机渲染文本，但其中的桌面附件路径不能泄露到远端设备。
    if let Some(blocks) = value
        .get_mut("blocks")
        .and_then(Value::as_array_mut)
    {
        for block in blocks {
            if block.get("type").and_then(Value::as_str)
                == Some("attachment")
            {
                if let Some(object) = block.as_object_mut() {
                    object.insert(
                        "filePath".to_string(),
                        Value::String(String::new()),
                    );
                }
            }
        }
    }
    if !attachments.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "attachments".to_string(),
                Value::Array(attachments),
            );
        }
    }
    // 剥离手机端不渲染的输出类字段。
    downsize_mobile_blocks(&mut value);
    Ok(value)
}

/// 页级字节预算：先把单条超限消息替换为占位 blocks，
/// 再从最旧端丢弃消息直到整页序列化不超过预算；
/// 只要发生过丢弃，就用幸存最旧一条重算 nextCursor（已知必还有更早历史）。
fn enforce_message_page_budget(
    mut messages: Vec<Value>,
    mut row_ids: Vec<i64>,
    next_cursor: Option<MessageWindowCursorDto>,
    budget_bytes: usize,
) -> (Vec<Value>, Option<MessageWindowCursorDto>) {
    for message in messages.iter_mut() {
        let serialized = serde_json::to_string(message).map(|value| value.len()).unwrap_or(0);
        if serialized > MESSAGE_SINGLE_MESSAGE_BUDGET_BYTES {
            if let Some(object) = message.as_object_mut() {
                object.insert(
                    "blocks".to_string(),
                    json!([{ "type": "text", "content": "该消息内容过大，请在桌面端查看。" }]),
                );
                if let Some(content) = object.get("content").and_then(Value::as_str).map(str::to_string) {
                    object.insert("content".to_string(), Value::String(truncate_mobile_text(&content, MOBILE_VISIBLE_TEXT_MAX_CHARS)));
                }
            }
        }
    }
    let total_bytes = |items: &[Value]| {
        items
            .iter()
            .map(|item| serde_json::to_string(item).map(|value| value.len()).unwrap_or(0))
            .sum::<usize>()
    };
    let mut trimmed = false;
    while messages.len() > 1 && total_bytes(&messages) > budget_bytes {
        messages.remove(0);
        row_ids.remove(0);
        trimmed = true;
    }
    let next_cursor = if trimmed {
        messages.first().map(|message| MessageWindowCursorDto {
            created_at: message.get("createdAt").and_then(Value::as_str).unwrap_or_default().to_string(),
            id: message.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
            row_id: row_ids.first().copied(),
        })
    } else {
        next_cursor
    };
    (messages, next_cursor)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        build_completed_message_event, decode_relay_header_value, enforce_message_page_budget,
        mobile_message_value, parse_optional_batch_id, parse_remote_attachment_inputs,
        relay_attachment_url, remote_batch_key, truncate_mobile_text, RemoteBatchState,
        RemoteTunnelManager,
    };
    use crate::commands::chat::ChatAttachmentPayload;
    use crate::config::app_config::RemoteAccessConfig;
    use crate::models::{MessageDto, MessageStatusDto, MessageWindowCursorDto};
    use serde_json::{json, Value};
    use tokio::fs as tokio_fs;

    #[test]
    fn remote_identity_is_generated_and_rotates() {
        let mut config = RemoteAccessConfig::default();
        assert!(config.ensure_identity());
        assert!(config.tunnel_id.starts_with("auracoder_"));
        assert!(config.credential.len() >= 64);
        let first_id = config.tunnel_id.clone();
        assert!(!config.ensure_identity());
        config.device_credential = "paired-device".to_string();
        config.regenerate_identity();
        assert_ne!(config.tunnel_id, first_id);
        assert!(config.device_credential.is_empty());
    }

    #[test]
    fn completed_message_event_targets_device_and_preserves_exact_message() {
        let payload = json!({
            "threadId": "thread-1",
            "messageId": "message-1",
            "message": {
                "id": "message-1",
                "threadId": "thread-1",
                "role": "assistant",
                "status": "completed",
                "content": "done",
            },
        });

        let event = build_completed_message_event("mobile-1", &payload)
            .expect("valid completed message payload");
        assert_eq!(event["event"], "thread.message.completed");
        assert_eq!(event["targetDeviceId"], "mobile-1");
        assert_eq!(event["payload"]["threadId"], "thread-1");
        assert_eq!(event["payload"]["messageId"], "message-1");
        assert_eq!(event["payload"]["message"]["id"], "message-1");
    }

    #[test]
    fn completed_message_event_rejects_missing_exact_ids() {
        let payload = json!({
            "threadId": "thread-1",
            "message": { "id": "message-1" },
        });
        assert!(build_completed_message_event("mobile-1", &payload).is_none());
    }

    #[tokio::test]
    async fn batch_attachment_validation_requires_exact_current_device_set() {
        let manager = RemoteTunnelManager::default();
        let device_id = "device-a";
        let batch_id = "batch-a";
        let attachment = ChatAttachmentPayload {
            file_name: "a.txt".to_string(),
            file_path: "C:/batch-a/a.txt".to_string(),
            size_bytes: 1,
            mime_type: Some("text/plain".to_string()),
            browser_annotation: None,
        };
        manager.batches.lock().await.insert(
            remote_batch_key(device_id, batch_id),
            RemoteBatchState {
                device_id: device_id.to_string(),
                batch_id: batch_id.to_string(),
                files: HashMap::from([(attachment.file_path.clone(), attachment.clone())]),
                last_activity: std::time::Instant::now(),
            },
        );
        assert!(manager
            .validate_batch_attachments(device_id, batch_id, std::slice::from_ref(&attachment))
            .await
            .is_ok());
        assert!(manager
            .validate_batch_attachments("other-device", batch_id, std::slice::from_ref(&attachment))
            .await
            .is_err());
        let duplicate = vec![attachment.clone(), attachment];
        assert!(manager
            .validate_batch_attachments(device_id, batch_id, &duplicate)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn abort_batch_removes_only_unsubmitted_batch_files() {
        let manager = RemoteTunnelManager::default();
        let device_id = "device-abort";
        let batch_id = "batch-abort";
        let file_path =
            std::env::temp_dir().join(format!("auracoder-remote-test-{}", uuid::Uuid::new_v4()));
        tokio_fs::write(&file_path, b"temporary")
            .await
            .expect("create test temporary attachment");
        let file_path_string = file_path.display().to_string();
        manager.batches.lock().await.insert(
            remote_batch_key(device_id, batch_id),
            RemoteBatchState {
                device_id: device_id.to_string(),
                batch_id: batch_id.to_string(),
                files: HashMap::from([(
                    file_path_string,
                    ChatAttachmentPayload {
                        file_name: "temporary.txt".to_string(),
                        file_path: file_path.display().to_string(),
                        size_bytes: 9,
                        mime_type: Some("text/plain".to_string()),
                        browser_annotation: None,
                    },
                )]),
                last_activity: std::time::Instant::now(),
            },
        );
        manager.abort_batch(device_id, batch_id).await;
        assert!(!file_path.exists());
        assert!(manager.batches.lock().await.is_empty());
    }

    #[test]
    fn missing_batch_id_keeps_legacy_client_path_available() {
        assert_eq!(
            parse_optional_batch_id(&json!({"message": "legacy"})),
            Ok(None)
        );
    }

    #[test]
    fn relay_attachment_url_converts_same_origin_and_encodes_query() {
        let url = relay_attachment_url(
            "wss://relay.example.test/ws/tunnel",
            "tunnel id",
            "key/with slash",
        )
        .expect("valid relay endpoint");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.path(), "/api/mobile/attachments/key%2Fwith%20slash");
        assert_eq!(url.query(), Some("tunnel_id=tunnel+id"));
    }

    #[test]
    fn relay_file_name_header_requires_valid_percent_encoding() {
        assert_eq!(
            decode_relay_header_value("report%20%E6%B5%8B%E8%AF%95.txt").unwrap(),
            "report 测试.txt"
        );
        assert!(decode_relay_header_value("broken%ZZ.txt").is_err());
    }

    #[test]
    fn message_attachment_inputs_accept_mobile_snake_case_attachment_keys() {
        let payload = json!({
            "attachments": [{
                "attachment_key": "K-1",
                "file_name": "report.txt",
                "size_bytes": 4,
                "mime_type": "text/plain"
            }]
        });
        let inputs = parse_remote_attachment_inputs(&payload)
            .expect("valid attachment input")
            .expect("attachment list");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].attachment_key.as_deref(), Some("K-1"));
        assert_eq!(inputs[0].size_bytes, Some(4));
    }

    #[test]
    fn mobile_message_value_strips_action_output_and_keeps_summary() {
        let blocks = json!([
            {
                "type": "action",
                "actionId": "action-1",
                "actionType": "run",
                "summary": "keep this summary",
                "details": { "command": "npm test" },
                "outputChunks": [{ "stream": "stdout", "content": "chunk" }],
                "result": { "output": "x".repeat(10_000), "diff": "d", "error": null }
            },
            {
                "type": "attachment",
                "fileName": "report.txt",
                "mimeType": "text/plain",
                "sizeBytes": 123,
                "filePath": "C:/secret/report.txt"
            }
        ]);
        let message = MessageDto {
            id: "message-1".to_string(),
            thread_id: "thread-1".to_string(),
            role: "assistant".to_string(),
            content: Some("answer".to_string()),
            blocks: Some(blocks),
            turn_engine_id: None,
            turn_model_id: None,
            turn_reasoning_effort: None,
            schema_version: 1,
            status: MessageStatusDto::Completed,
            token_usage: None,
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
        };

        let value = mobile_message_value(message).expect("convert message to mobile value");
        let block_values = value.get("blocks").and_then(Value::as_array).unwrap();
        let action = block_values
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("action"))
            .expect("action block should remain");
        assert!(action.get("outputChunks").is_none());
        assert!(action.get("details").is_none());
        assert_eq!(
            action.get("summary").and_then(Value::as_str),
            Some("keep this summary")
        );
        let result = action.get("result").and_then(Value::as_object).unwrap();
        let output = result.get("output").and_then(Value::as_str).unwrap();
        assert!(output.ends_with(super::MOBILE_TRUNCATED_SUFFIX));
        assert_eq!(
            output.chars().count(),
            super::MOBILE_HIDDEN_OUTPUT_MAX_CHARS + super::MOBILE_TRUNCATED_SUFFIX.chars().count()
        );

        let attachments = value.get("attachments").and_then(Value::as_array).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0].get("filePath").and_then(Value::as_str),
            Some("")
        );
        let attachment_block = block_values
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("attachment"))
            .expect("attachment block should remain");
        assert_eq!(
            attachment_block.get("filePath").and_then(Value::as_str),
            Some("")
        );
    }

    #[test]
    fn truncate_mobile_text_appends_suffix_only_when_needed() {
        assert_eq!(truncate_mobile_text("", 10), "");
        assert_eq!(truncate_mobile_text("short", 10), "short");
        let long = "x".repeat(100);
        let truncated = truncate_mobile_text(&long, 10);
        assert!(truncated.ends_with(super::MOBILE_TRUNCATED_SUFFIX));
        assert_eq!(
            truncated.chars().count(),
            10 + super::MOBILE_TRUNCATED_SUFFIX.chars().count()
        );
    }

    #[test]
    fn enforce_message_page_budget_drops_oldest_and_recomputes_cursor() {
        let messages: Vec<Value> = (1..=5)
            .map(|index| {
                json!({
                    "id": format!("m{index}"),
                    "createdAt": "2026-08-01T00:00:00.000Z",
                    "content": "x".repeat(100),
                })
            })
            .collect();
        let row_ids = vec![1, 2, 3, 4, 5];
        let next_cursor = Some(MessageWindowCursorDto {
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
            id: "m1".to_string(),
            row_id: Some(1),
        });
        // 每条约 100 字节；预算只容纳 2 条，最旧的 m1/m2/m3 被丢弃。
        let single_size = messages[0].to_string().len();
        let budget = single_size * 2;
        let (kept, recomputed) = enforce_message_page_budget(
            messages.clone(),
            row_ids.clone(),
            next_cursor.clone(),
            budget,
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].get("id").and_then(Value::as_str), Some("m4"));
        assert_eq!(kept[1].get("id").and_then(Value::as_str), Some("m5"));
        let recomputed = recomputed.expect("cursor should be recomputed after trimming");
        assert_eq!(recomputed.id, "m4");
        assert_eq!(recomputed.row_id, Some(4));

        // 未触发裁剪时 next_cursor 原样返回。
        let (kept_all, untouched) = enforce_message_page_budget(
            messages.clone(),
            row_ids,
            next_cursor.clone(),
            usize::MAX,
        );
        assert_eq!(kept_all.len(), 5);
        let untouched = untouched.expect("cursor should be untouched without trimming");
        assert_eq!(untouched.id, "m1");
        assert_eq!(untouched.row_id, Some(1));
    }

    #[test]
    fn enforce_message_page_budget_replaces_single_oversize_message() {
        let messages = vec![json!({
            "id": "m-oversize",
            "createdAt": "2026-08-01T00:00:00.000Z",
            "content": "x".repeat(super::MESSAGE_SINGLE_MESSAGE_BUDGET_BYTES + 1024),
        })];
        let (kept, next_cursor) = enforce_message_page_budget(
            messages,
            vec![1],
            None,
            super::MESSAGE_PAGE_BUDGET_BYTES,
        );
        assert_eq!(kept.len(), 1);
        assert!(next_cursor.is_none());
        let blocks = kept[0].get("blocks").and_then(Value::as_array).unwrap();
        assert_eq!(
            blocks[0].get("content").and_then(Value::as_str),
            Some("该消息内容过大，请在桌面端查看。")
        );
    }
}
