use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};
use tauri::{AppHandle, Emitter, State};
use tokio::fs as tokio_fs;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    cli_tools::{
        factory::CliToolFactory, CliExecutionContext, CliLocationKind, CliRuntimePermissions,
        CliTool,
    },
    commands::threads::migrate_legacy_codex_on_failure_thread_metadata,
    db,
    engines::{
        approval_response_route_for_engine,
        normalize_approval_response_for_engine,
        trim_action_output_delta_content,
        validate_engine_sandbox_mode,
        ActionResult,
        ApprovalRequestRoute,
        BrowserAnnotationMetadata,
        // Engine 直连入口已停用；三种 CLI 统一通过 CliTool 发送消息。
        // Engine,
        EngineEvent,
        OutputStream,
        SandboxPolicy,
        ThreadScope,
        TurnAttachment,
        TurnCompletionStatus,
        TurnInput,
        TurnInputItem,
        STREAMED_DIFF_MAX_CHARS,
    },
    // 旧版 Controller 直接调用本机和 SSH CLI 生命周期的入口已停用，现由 CliTool Service 实现类承接。
    models::{
        ActionOutputDto, EngineInfoDto, EngineModelDto, MessageDto, MessageStatusDto,
        MessageWindowCursorDto, MessageWindowDto, SearchResultDto, SteerReceiptDto, ThreadDto,
        ThreadStatusDto, ThreadUpdateDto, TrustLevelDto,
    },
    runtime_env,
    ssh::remote_attachments::{self, RemoteAttachmentBatch},
    state::AppState,
};

const MAX_THREAD_TITLE_CHARS: usize = 72;
const STREAM_EVENT_COALESCE_MAX_CHARS: usize = 8_192;
const STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL: Duration = Duration::from_millis(24);
const STREAM_DB_FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const STREAM_DB_BLOCKS_FLUSH_INTERVAL: Duration = Duration::from_millis(900);
const ENGINE_EVENT_QUEUE_CAPACITY: usize = 1_280;
const TURN_EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(900);
const TURN_INTERRUPT_TIMEOUT: Duration = Duration::from_secs(10);
const TURN_TASK_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
const ACTION_OUTPUT_MAX_CHUNKS: usize = 240;
const ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS: usize = 4_096;
const TRUNCATED_SUFFIX: &str = "\n... [truncated]";
const MAX_ATTACHMENTS_PER_TURN: usize = 10;
const MAX_PASTED_IMAGE_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_BROWSER_ANNOTATION_COMMENT_CHARS: usize = 2_000;
const MAX_BROWSER_ANNOTATION_URL_CHARS: usize = 4_096;
const MAX_BROWSER_ANNOTATION_TARGET_CHARS: usize = 600;
const TEXT_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "txt", "md", "json", "js", "ts", "tsx", "jsx", "py", "rs", "go", "css", "html", "yaml", "yml",
    "toml", "xml", "sql", "sh", "csv",
];
const IMAGE_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg",
];
const MESSAGE_WINDOW_DEFAULT_LIMIT: usize = 120;
const MESSAGE_WINDOW_MAX_LIMIT: usize = 400;
const MAX_CHAT_NOTIFICATION_PREVIEW_CHARS: usize = 240;

static NEXT_ENGINE_EVENT_CONSUME_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static NEXT_STREAM_PROCESS_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static NEXT_STREAM_FLUSH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

async fn receive_engine_event_with_diagnostics(
    event_rx: &mut mpsc::Receiver<EngineEvent>,
    consumer: &str,
) -> Option<EngineEvent> {
    let receive_sequence = NEXT_ENGINE_EVENT_CONSUME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let queue_depth_before = event_rx.len();
    let started_at = Instant::now();
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_event_consume_start",
        "receive_sequence": receive_sequence,
        "consumer": consumer,
        "queue_depth": queue_depth_before,
    }))
    .await;
    let result = event_rx.recv().await;
    let wait_ms = started_at.elapsed().as_millis();
    let queue_depth_after = event_rx.len();
    let event_kind = result
        .as_ref()
        .map(crate::engines::codex::engine_event_kind);
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_event_consume_complete",
        "receive_sequence": receive_sequence,
        "consumer": consumer,
        "event_kind": event_kind,
        "queue_depth_before": queue_depth_before,
        "queue_depth_after": queue_depth_after,
        "wait_ms": wait_ms,
        "result": if result.is_some() { "event" } else { "closed" },
    }))
    .await;
    result
}

fn value_to_raw(value: &Value) -> Box<RawValue> {
    serde_json::value::to_raw_value(value).unwrap_or_else(|_| empty_raw_value())
}

fn empty_raw_value() -> Box<RawValue> {
    RawValue::from_string("null".to_string()).expect("\"null\" is a valid JSON literal")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        content: String,
        #[serde(rename = "planMode", skip_serializing_if = "Option::is_none")]
        plan_mode: Option<bool>,
        #[serde(rename = "isSteer", skip_serializing_if = "Option::is_none")]
        is_steer: Option<bool>,
        #[serde(rename = "clientSteerId", skip_serializing_if = "Option::is_none")]
        client_steer_id: Option<String>,
    },

    #[serde(rename = "steer")]
    Steer {
        #[serde(rename = "steerId")]
        steer_id: String,
        content: String,
        #[serde(rename = "deliveryStatus")]
        delivery_status: String,
        #[serde(rename = "planMode", skip_serializing_if = "Option::is_none")]
        plan_mode: Option<bool>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skills: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        mentions: Vec<ContentBlock>,
    },

    #[serde(rename = "diff")]
    Diff { diff: String, scope: String },

    #[serde(rename = "action")]
    Action {
        #[serde(rename = "actionId")]
        action_id: String,
        #[serde(rename = "engineActionId", skip_serializing_if = "Option::is_none")]
        engine_action_id: Option<String>,
        #[serde(rename = "actionType")]
        action_type: String,
        summary: String,
        details: Box<RawValue>,
        #[serde(rename = "backgroundTaskId", skip_serializing_if = "Option::is_none")]
        background_task_id: Option<String>,
        #[serde(rename = "outputChunks")]
        output_chunks: Vec<ActionOutputChunk>,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<ActionBlockResult>,
    },

    #[serde(rename = "approval")]
    Approval {
        #[serde(rename = "approvalId")]
        approval_id: String,
        #[serde(rename = "actionType")]
        action_type: String,
        summary: String,
        details: Box<RawValue>,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },

    #[serde(rename = "thinking")]
    Thinking {
        content: String,
        #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
        started_at: Option<f64>,
        #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
    },

    #[serde(rename = "notice")]
    Notice {
        kind: String,
        level: String,
        title: String,
        message: String,
        /// Notice 业务附带的可选结构化元数据，持久化后原样提供给前端。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Box<RawValue>>,
    },

    #[serde(rename = "error")]
    Error { message: String },

    #[serde(rename = "attachment")]
    Attachment {
        #[serde(rename = "fileName")]
        file_name: String,
        #[serde(rename = "filePath")]
        file_path: String,
        #[serde(
            rename = "previewFilePath",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        preview_file_path: Option<String>,
        #[serde(rename = "sizeBytes")]
        size_bytes: u64,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(rename = "browserAnnotation", skip_serializing_if = "Option::is_none")]
        browser_annotation: Option<BrowserAnnotationMetadata>,
        #[serde(rename = "isRemote", default)]
        is_remote: bool,
    },

    #[serde(rename = "skill")]
    Skill { name: String, path: String },

    #[serde(rename = "mention")]
    Mention { name: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActionOutputChunk {
    stream: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionBlockResult {
    success: bool,
    output: Option<String>,
    error: Option<String>,
    diff: Option<String>,
    duration_ms: u64,
}

#[derive(Default)]
struct EventProgress {
    message_status: Option<MessageStatusDto>,
    thread_status: Option<ThreadStatusDto>,
    token_usage: Option<(u64, u64)>,
    turn_model_id: Option<String>,
    finalized_actions: Vec<(String, ActionResult)>,
    blocks_changed: bool,
    force_persist: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadUpdatedEvent {
    thread_id: String,
    workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<ThreadDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatTurnFinishedEvent {
    thread_id: String,
    workspace_id: String,
    engine_id: String,
    thread_title: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatApprovalRequestedEvent {
    thread_id: String,
    workspace_id: String,
    engine_id: String,
    thread_title: String,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliServiceRestartRequiredEvent {
    thread_id: String,
    workspace_id: String,
    engine_id: String,
    thread_title: String,
    connection_id: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentPayload {
    pub file_name: String,
    pub file_path: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub browser_annotation: Option<BrowserAnnotationMetadata>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPreviewPayload {
    pub mime_type: String,
    pub data_base64: String,
}

#[tauri::command]
pub async fn save_pasted_image_attachment(
    file_name: String,
    mime_type: String,
    data_base64: String,
) -> Result<ChatAttachmentPayload, String> {
    let _display_file_name = file_name.trim();
    let normalized_mime = mime_type.trim().to_lowercase();
    if !normalized_mime.starts_with("image/") {
        return Err("Pasted attachment is not an image.".to_string());
    }

    let encoded = data_base64
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(data_base64.as_str())
        .trim();
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| "Pasted image data is not valid base64.".to_string())?;
    if bytes.is_empty() {
        return Err("Pasted image data is empty.".to_string());
    }
    if bytes.len() > MAX_PASTED_IMAGE_ATTACHMENT_BYTES {
        return Err("Pasted image exceeds the 10 MB attachment limit.".to_string());
    }

    let extension = image_extension_for_mime_type(&normalized_mime)
        .ok_or_else(|| "Pasted image type is not supported.".to_string())?;
    let stored_file_name = format!("pasted-image-{}.{}", Uuid::new_v4().simple(), extension);
    let attachment_dir = runtime_env::app_data_dir()
        .join("attachments")
        .join("pasted-images");
    tokio_fs::create_dir_all(&attachment_dir)
        .await
        .map_err(|error| format!("failed to create pasted image attachment directory: {error}"))?;
    let file_path = attachment_dir.join(&stored_file_name);
    tokio_fs::write(&file_path, &bytes)
        .await
        .map_err(|error| format!("failed to save pasted image attachment: {error}"))?;

    Ok(ChatAttachmentPayload {
        file_name: stored_file_name,
        file_path: file_path.display().to_string(),
        size_bytes: bytes.len() as u64,
        mime_type: Some(normalized_mime),
        browser_annotation: None,
    })
}

/// 预览图最长边；原图超过时逐级缩小直到编码字节数达标。
const ATTACHMENT_PREVIEW_MAX_DIMENSION: u32 = 1600;
/// 预览图编码后字节上限；base64 膨胀后约 933KB，低于 Relay 单帧上限并保留信封余量。
const ATTACHMENT_PREVIEW_MAX_BYTES: usize = 700 * 1024;
/// 缩略图 JPEG 编码质量；越高越清晰但编码体积越大。
const ATTACHMENT_PREVIEW_JPEG_QUALITY: u8 = 80;

#[tauri::command]
pub async fn read_attachment_preview(
    file_path: String,
    mime_type: Option<String>,
    preview_file_path: Option<String>,
) -> Result<Option<AttachmentPreviewPayload>, String> {
    let selected_file_path = preview_file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| file_path.trim());
    if selected_file_path.is_empty() {
        return Ok(None);
    }

    match read_attachment_preview_file(selected_file_path, mime_type.as_deref()).await {
        Ok(preview) => Ok(preview),
        Err(error) => {
            log::error!("{error:#}");
            Err("图片预览加载失败".to_string())
        }
    }
}

async fn read_attachment_preview_file(
    file_path: &str,
    mime_type: Option<&str>,
) -> anyhow::Result<Option<AttachmentPreviewPayload>> {
    let Some(preview_mime_type) = normalize_image_preview_mime_type(file_path, mime_type) else {
        return Ok(None);
    };

    let metadata = tokio_fs::metadata(file_path)
        .await
        .with_context(|| format!("failed to read attachment metadata: {file_path}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PASTED_IMAGE_ATTACHMENT_BYTES as u64 {
        return Ok(None);
    }

    let bytes = tokio_fs::read(file_path)
        .await
        .with_context(|| format!("failed to read attachment preview: {file_path}"))?;
    encode_attachment_preview(preview_mime_type, &bytes)
}

fn encode_attachment_preview(
    preview_mime_type: String,
    bytes: &[u8],
) -> anyhow::Result<Option<AttachmentPreviewPayload>> {
    // SVG 是矢量格式，不参与位图缩放；超限则放弃预览。
    if preview_mime_type == "image/svg+xml" {
        if bytes.len() > ATTACHMENT_PREVIEW_MAX_BYTES {
            return Ok(None);
        }
        return Ok(Some(AttachmentPreviewPayload {
            mime_type: preview_mime_type,
            data_base64: BASE64.encode(bytes),
        }));
    }

    let image = image::load_from_memory(bytes).context("解析图片预览失败")?;
    let has_alpha = image.color().has_alpha();
    let (format, mime_type) = if has_alpha {
        (image::ImageFormat::Png, "image/png")
    } else {
        (image::ImageFormat::Jpeg, "image/jpeg")
    };
    for dimension in [ATTACHMENT_PREVIEW_MAX_DIMENSION, 1280, 960, 640] {
        let resized = if image.width() > dimension || image.height() > dimension {
            image.thumbnail(dimension, dimension)
        } else {
            image.clone()
        };
        let mut buffer: Vec<u8> = Vec::new();
        if has_alpha {
            resized
                .write_to(&mut std::io::Cursor::new(&mut buffer), format)
                .context("编码 PNG 图片预览失败")?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut std::io::Cursor::new(&mut buffer),
                ATTACHMENT_PREVIEW_JPEG_QUALITY,
            )
            .encode_image(&resized)
            .context("编码 JPEG 图片预览失败")?;
        }
        if buffer.len() <= ATTACHMENT_PREVIEW_MAX_BYTES {
            return Ok(Some(AttachmentPreviewPayload {
                mime_type: mime_type.to_string(),
                data_base64: BASE64.encode(&buffer),
            }));
        }
    }
    Ok(None)
}

struct PersistentImagePreviewBatch {
    file_paths: Vec<PathBuf>,
    committed: bool,
}

impl PersistentImagePreviewBatch {
    fn new() -> Self {
        Self {
            file_paths: Vec::new(),
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PersistentImagePreviewBatch {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for file_path in &self.file_paths {
            if let Err(error) = std::fs::remove_file(file_path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("{error}");
                }
            }
        }
    }
}

async fn persist_message_image_previews(
    attachments: &mut [TurnAttachment],
) -> anyhow::Result<PersistentImagePreviewBatch> {
    let mut batch = PersistentImagePreviewBatch::new();
    let preview_directory = runtime_env::app_data_dir()
        .join("attachments")
        .join("message-images");
    let mut preview_directory_ready = false;

    for attachment in attachments {
        let Some(preview_mime_type) = normalize_image_preview_mime_type(
            &attachment.file_path,
            attachment.mime_type.as_deref(),
        ) else {
            continue;
        };
        let extension = image_extension_for_mime_type(&preview_mime_type).ok_or_else(|| {
            anyhow::anyhow!(
                "无法保存附件 `{}` 的图片预览：不支持的图片格式",
                attachment.file_name
            )
        })?;
        let source_file_path = attachment.file_path.trim();
        let metadata = tokio_fs::metadata(source_file_path)
            .await
            .with_context(|| format!("读取图片附件元数据失败：{}", attachment.file_name))?;
        anyhow::ensure!(
            metadata.is_file(),
            "图片附件不是普通文件：{}",
            attachment.file_name
        );
        anyhow::ensure!(
            metadata.len() <= MAX_PASTED_IMAGE_ATTACHMENT_BYTES as u64,
            "图片附件 `{}` 超过 10 MB 大小限制",
            attachment.file_name
        );
        let bytes = tokio_fs::read(source_file_path)
            .await
            .with_context(|| format!("读取图片附件失败：{}", attachment.file_name))?;
        anyhow::ensure!(
            bytes.len() <= MAX_PASTED_IMAGE_ATTACHMENT_BYTES,
            "图片附件 `{}` 超过 10 MB 大小限制",
            attachment.file_name
        );
        anyhow::ensure!(
            encode_attachment_preview(preview_mime_type, &bytes)?.is_some(),
            "无法为图片附件 `{}` 生成预览",
            attachment.file_name
        );

        if !preview_directory_ready {
            tokio_fs::create_dir_all(&preview_directory)
                .await
                .context("创建聊天图片永久预览目录失败")?;
            preview_directory_ready = true;
        }
        let target_file_path =
            preview_directory.join(format!("{}.{}", Uuid::new_v4().simple(), extension));
        let temporary_file_path =
            preview_directory.join(format!(".{}.part", Uuid::new_v4().simple()));
        batch.file_paths.push(temporary_file_path.clone());
        tokio_fs::write(&temporary_file_path, &bytes)
            .await
            .with_context(|| format!("写入图片永久预览失败：{}", attachment.file_name))?;
        tokio_fs::rename(&temporary_file_path, &target_file_path)
            .await
            .with_context(|| format!("提交图片永久预览失败：{}", attachment.file_name))?;
        if let Some(last_file_path) = batch.file_paths.last_mut() {
            *last_file_path = target_file_path.clone();
        }
        attachment.preview_file_path = Some(target_file_path.display().to_string());
    }

    Ok(batch)
}

async fn prepare_message_image_previews(
    attachments: &mut [TurnAttachment],
) -> Result<PersistentImagePreviewBatch, String> {
    match persist_message_image_previews(attachments).await {
        Ok(batch) => Ok(batch),
        Err(error) => {
            log::error!("{error:#}");
            Err("无法保存图片预览，消息未发送".to_string())
        }
    }
}

fn image_extension_for_mime_type(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    }
}

fn normalize_image_preview_mime_type(file_path: &str, mime_type: Option<&str>) -> Option<String> {
    if let Some(mime_type) = mime_type.map(str::trim).filter(|value| !value.is_empty()) {
        let normalized = mime_type.to_lowercase();
        if normalized.starts_with("image/") {
            return Some(match normalized.as_str() {
                "image/jpg" => "image/jpeg".to_string(),
                _ => normalized,
            });
        }
    }

    let extension = Path::new(file_path)
        .extension()
        .and_then(|value| value.to_str())?
        .to_lowercase();
    image_mime_type_for_extension(&extension).map(ToOwned::to_owned)
}

fn image_mime_type_for_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatInputItemPayload {
    Text { text: String },
    Skill { name: String, path: String },
    Mention { name: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexReviewTargetPayload {
    #[serde(rename = "uncommittedChanges")]
    UncommittedChanges,
    #[serde(rename = "baseBranch")]
    BaseBranch { branch: String },
    #[serde(rename = "commit")]
    Commit {
        sha: String,
        #[serde(default)]
        title: Option<String>,
    },
    #[serde(rename = "custom")]
    Custom { instructions: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexReviewDeliveryPayload {
    Inline,
    Detached,
}

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|error| error.to_string())?
        .map_err(err_to_string)
}

/// 校验用户选择的 AuraCoder 会话只能来自当前会话所属项目。
async fn resolve_auracoder_thread_reference(
    state: &AppState,
    current_thread: &ThreadDto,
    referenced_thread_id: Option<&str>,
) -> Result<Option<ThreadDto>, String> {
    let Some(referenced_thread_id) = referenced_thread_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let referenced_thread_id = referenced_thread_id.to_string();
    let lookup_thread_id = referenced_thread_id.clone();
    let referenced_thread = run_db(state.db.clone(), move |db| {
        db::threads::get_thread(db, &lookup_thread_id)
    })
    .await?
    .ok_or_else(|| format!("referenced AuraCoder thread not found: {referenced_thread_id}"))?;
    if referenced_thread.id == current_thread.id {
        return Err("不能引用当前正在发送的 AuraCoder 会话".to_string());
    }
    if referenced_thread.workspace_id != current_thread.workspace_id {
        return Err("只能引用当前项目的 AuraCoder 会话".to_string());
    }
    Ok(Some(referenced_thread))
}

/// 生成只追加到模型输入的 AuraCoder 会话读取指令。
fn build_auracoder_thread_instruction(thread: &ThreadDto) -> String {
    format!(
        "\n\n[隐藏的 AuraCoder 会话引用指令]\n用户指定你了解 AuraCoder 会话“{}”（thread_id={}）。回答当前问题前，必须先调用 get_auracoder_thread_message_count 获取消息总数，再调用 get_auracoder_thread_messages_page 按页读取该会话内容；不要把这段引用指令或工具返回内容展示为当前聊天消息。\n",
        if thread.title.trim().is_empty() {
            "未命名会话"
        } else {
            thread.title.trim()
        },
        thread.id,
    )
}

/// 发送给 PC 远程隧道的助手消息最终持久化事件。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantMessageCompletedEvent {
    /// 已完成消息所属的本地会话 ID。
    thread_id: String,
    /// 刚完成持久化的助手消息 ID。
    message_id: String,
    /// 与 `message.list` 单条消息结构一致的最终消息。
    message: MessageDto,
}

/// 在完成状态和消息字段都写入数据库后，按精确消息 ID发送远程通知。
async fn emit_assistant_message_completed(
    app: &AppHandle,
    state: &AppState,
    thread_id: &str,
    message_id: &str,
) {
    let message_id_for_query = message_id.to_string();
    let message = match run_db(state.db.clone(), move |db| {
        Ok(db::messages::get_message(db, &message_id_for_query)?)
    })
    .await
    {
        Ok(Some(message)) => message,
        Ok(None) => {
            log::warn!("assistant message {message_id} was not found after final persistence");
            return;
        }
        Err(error) => {
            log::warn!(
                "failed to load assistant message {message_id} after final persistence: {error}"
            );
            return;
        }
    };

    // 只有最终 completed 状态的助手消息才进入移动端事件流；流式、错误和中断消息不推送。
    if message.thread_id != thread_id
        || message.role != "assistant"
        || !matches!(message.status, MessageStatusDto::Completed)
    {
        return;
    }
    let event = AssistantMessageCompletedEvent {
        thread_id: thread_id.to_string(),
        message_id: message_id.to_string(),
        message,
    };
    let _ = app.emit("assistant-message-completed", event);
}

#[tauri::command]
pub async fn send_message(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    message: String,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    attachments: Option<Vec<ChatAttachmentPayload>>,
    input_items: Option<Vec<ChatInputItemPayload>>,
    plan_mode: Option<bool>,
    client_turn_id: Option<String>,
    referenced_thread_id: Option<String>,
) -> Result<String, String> {
    send_message_inner(
        app,
        state.inner(),
        thread_id,
        message,
        model_id,
        reasoning_effort,
        attachments,
        input_items,
        plan_mode,
        client_turn_id,
        None,
        referenced_thread_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_message_inner(
    app: tauri::AppHandle,
    state: &AppState,
    thread_id: String,
    message: String,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    attachments: Option<Vec<ChatAttachmentPayload>>,
    input_items: Option<Vec<ChatInputItemPayload>>,
    plan_mode: Option<bool>,
    client_turn_id: Option<String>,
    scheduled_run_id: Option<String>,
    referenced_thread_id: Option<String>,
) -> Result<String, String> {
    let already_running = state.turns.get(&thread_id).await.is_some();
    if already_running {
        return Err(
            "A turn is already running for this thread. Cancel it before sending another message."
                .to_string(),
        );
    }

    let db = state.db.clone();
    let mut thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    migrate_legacy_codex_on_failure_thread_metadata(state, &mut thread).await;
    let execution_workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    if execution_workspace.location_kind == "ssh"
        && !matches!(thread.engine_id.as_str(), "codex" | "opencode" | "claude")
    {
        return Err(format!(
            "SSH 远端项目当前阶段尚未接入 {} 正式对话",
            thread.engine_id
        ));
    }
    let referenced_thread =
        resolve_auracoder_thread_reference(state, &thread, referenced_thread_id.as_deref()).await?;
    let auracoder_thread_instruction = referenced_thread
        .as_ref()
        .map(build_auracoder_thread_instruction);
    let requested_model_id = model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut attachments = normalize_attachments(attachments)?;
    let input_items = normalize_input_items(message.as_str(), input_items)?;
    let plan_mode = plan_mode.unwrap_or(false);
    let browser_annotation_context = browser_annotation_context(&attachments);
    let mut engine_message = format_engine_message(&message, &browser_annotation_context);
    let mut engine_input_items =
        append_browser_annotation_context(&input_items, &browser_annotation_context);
    if let Some(instruction) = auracoder_thread_instruction.as_deref() {
        engine_message.push_str(instruction);
        engine_input_items.push(TurnInputItem::Text {
            text: instruction.to_string(),
        });
    }
    // SSH 附件必须在模型校验和远端线程准备完成后上传，再使用远端路径构造 TurnInput。
    // let turn_input = TurnInput {
    //     message: engine_message,
    //     attachments: attachments.clone(),
    //     plan_mode,
    //     input_items: engine_input_items,
    // };
    let current_turn_model_id = thread.model_id.clone();
    let model_switch_requested = requested_model_id
        .map(|value| value != current_turn_model_id.as_str())
        .unwrap_or(false);
    let codex_context = if thread.engine_id == "codex" {
        Some(CliExecutionContext::from_workspace(&execution_workspace).map_err(err_to_string)?)
    } else {
        None
    };
    let codex_cli = if let Some(context) = codex_context.as_ref() {
        let codex = CliToolFactory::new(state.clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let cli: &dyn CliTool = codex.as_ref();
        cli.acquire_turn(context, &thread)
            .await
            .map_err(err_to_string)?;
        Some(codex)
    } else {
        None
    };
    let opencode_context = if thread.engine_id == "opencode" {
        Some(CliExecutionContext::from_workspace(&execution_workspace).map_err(err_to_string)?)
    } else {
        None
    };
    let opencode_cli = if let Some(context) = opencode_context.as_ref() {
        let opencode = CliToolFactory::new(state.clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let cli: &dyn CliTool = opencode.as_ref();
        cli.acquire_turn(context, &thread)
            .await
            .map_err(err_to_string)?;
        Some(opencode)
    } else {
        None
    };
    // OpenCode 的本机与 SSH 整轮占用现在统一由 OpenCodeCli::acquire_turn 处理。
    // let remote_opencode_use = if execution_workspace.location_kind == "ssh"
    //     && thread.engine_id == "opencode"
    // {
    //     Some(
    //         remote_project_opencode_runtime_service::acquire_turn(
    //             &execution_workspace,
    //             &thread.id,
    //         )
    //         .await
    //         .map_err(err_to_string)?,
    //     )
    // } else {
    //     None
    // };
    let claude_context = if thread.engine_id == "claude" {
        Some(CliExecutionContext::from_workspace(&execution_workspace).map_err(err_to_string)?)
    } else {
        None
    };
    let claude_cli = if let Some(context) = claude_context.as_ref() {
        let claude = CliToolFactory::new(state.clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let cli: &dyn CliTool = claude.as_ref();
        cli.acquire_turn(context, &thread)
            .await
            .map_err(err_to_string)?;
        Some(claude)
    } else {
        None
    };
    let remote_engine_catalog = if execution_workspace.location_kind == "ssh" {
        // 阶段计划 3 在这里拒绝所有本机附件；阶段计划 4 改为模型校验后统一上传。
        // if !attachments.is_empty() {
        //     return Err(format!(
        //         "SSH 远端项目附件将在第4阶段剩余工作阶段计划4中接入；当前不能把本机附件路径发送给远端 {}",
        //         match thread.engine_id.as_str() {
        //             "opencode" => "OpenCode",
        //             "claude" => "Claude",
        //             _ => "Codex",
        //         }
        //     ));
        // }
        let engine = if let (Some(codex), Some(context)) =
            (codex_cli.as_ref(), codex_context.as_ref())
        {
            let cli: &dyn CliTool = codex.as_ref();
            cli.get_engine_info(context).await.map_err(err_to_string)?
        } else if let (Some(opencode), Some(context)) =
            (opencode_cli.as_ref(), opencode_context.as_ref())
        {
            let cli: &dyn CliTool = opencode.as_ref();
            cli.get_engine_info(context).await.map_err(err_to_string)?
        } else if let (Some(claude), Some(context)) = (claude_cli.as_ref(), claude_context.as_ref())
        {
            let cli: &dyn CliTool = claude.as_ref();
            cli.get_engine_info(context).await.map_err(err_to_string)?
        } else {
            return Err(format!("SSH 远端 {} 运行时未建立", thread.engine_id));
        };
        Some(vec![engine])
    } else {
        None
    };
    let validation_catalog = if let Some(catalog) = remote_engine_catalog.as_ref() {
        Some(catalog.clone())
    } else if model_switch_requested {
        if let (Some(opencode), Some(context)) = (opencode_cli.as_ref(), opencode_context.as_ref())
        {
            let cli: &dyn CliTool = opencode.as_ref();
            cli.get_engine_info(context)
                .await
                .ok()
                .map(|engine| vec![engine])
        } else if let (Some(claude), Some(context)) = (claude_cli.as_ref(), claude_context.as_ref())
        {
            let cli: &dyn CliTool = claude.as_ref();
            cli.get_engine_info(context)
                .await
                .ok()
                .map(|engine| vec![engine])
        } else {
            crate::commands::engines::list_local_engine_infos(state)
                .await
                .ok()
        }
    } else {
        None
    };
    let effective_model_id =
        resolve_turn_model_id(&thread, requested_model_id, validation_catalog.as_deref())?;
    if execution_workspace.location_kind == "ssh"
        && !validation_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|engine| engine.id == thread.engine_id)
            .is_some_and(|engine| {
                engine
                    .models
                    .iter()
                    .any(|model| model.id == effective_model_id)
            })
    {
        return Err(format!(
            "模型 `{effective_model_id}` 在当前 SSH 远端 {} 上不可用，请重新选择模型",
            match thread.engine_id.as_str() {
                "opencode" => "OpenCode",
                "claude" => "Claude",
                _ => "Codex",
            }
        ));
    }
    let attachment_catalog = if attachments.is_empty() {
        None
    } else if let Some(catalog) = validation_catalog.as_ref() {
        Some(catalog.clone())
    } else if let (Some(claude), Some(context)) = (claude_cli.as_ref(), claude_context.as_ref()) {
        let cli: &dyn CliTool = claude.as_ref();
        Some(vec![cli
            .get_engine_info(context)
            .await
            .map_err(err_to_string)?])
    } else {
        Some(
            crate::commands::engines::list_local_engine_infos(state)
                .await
                .map_err(err_to_string)?,
        )
    };
    validate_attachments_for_engine_model(
        &attachments,
        &thread.engine_id,
        &effective_model_id,
        attachment_catalog.as_deref(),
    )?;

    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        let thread_id = thread.id.clone();
        move |db| {
            let workspace = db::workspaces::list_workspaces(db)?
                .into_iter()
                .find(|item| item.id == workspace_id)
                .ok_or_else(|| anyhow::anyhow!("workspace not found for thread {thread_id}"))?;
            Ok(workspace)
        }
    })
    .await?;

    let workspace_root = workspace.root_path.clone();
    let requested_reasoning_effort = normalize_reasoning_effort_value(reasoning_effort.as_deref());
    let stored_reasoning_effort = thread.reasoning_effort.clone();
    let configured_reasoning_effort = requested_reasoning_effort
        .clone()
        .or_else(|| stored_reasoning_effort.clone());
    let reasoning_effort = if requested_reasoning_effort.is_some() {
        requested_reasoning_effort
    } else if model_switch_requested {
        validation_catalog
            .as_deref()
            .map(|engines| {
                resolve_reasoning_effort_from_catalog(
                    engines,
                    thread.engine_id.as_str(),
                    effective_model_id.as_str(),
                    configured_reasoning_effort.as_deref(),
                )
            })
            .unwrap_or_else(|| {
                normalize_reasoning_effort_value(configured_reasoning_effort.as_deref())
            })
    } else {
        configured_reasoning_effort.clone()
    };
    let (cli_permissions, auto_approve_mcp_elicitations) = if let (Some(codex), Some(context)) =
        (codex_cli.as_ref(), codex_context.as_ref())
    {
        let cli: &dyn CliTool = codex.as_ref();
        let permissions = cli
            .runtime_permissions(context, &thread)
            .await
            .map_err(err_to_string)?;
        let auto_approve_mcp_elicitations = cli
            .auto_approve_mcp_elicitations(context, &thread)
            .await
            .map_err(err_to_string)?;
        (permissions, auto_approve_mcp_elicitations)
    } else if let (Some(opencode), Some(context)) =
        (opencode_cli.as_ref(), opencode_context.as_ref())
    {
        let cli: &dyn CliTool = opencode.as_ref();
        let permissions = cli
            .runtime_permissions(context, &thread)
            .await
            .map_err(err_to_string)?;
        let auto_approve_mcp_elicitations = cli
            .auto_approve_mcp_elicitations(context, &thread)
            .await
            .map_err(err_to_string)?;
        (permissions, auto_approve_mcp_elicitations)
    } else if let (Some(claude), Some(context)) = (claude_cli.as_ref(), claude_context.as_ref()) {
        let cli: &dyn CliTool = claude.as_ref();
        let permissions = cli
            .runtime_permissions(context, &thread)
            .await
            .map_err(err_to_string)?;
        let auto_approve_mcp_elicitations = cli
            .auto_approve_mcp_elicitations(context, &thread)
            .await
            .map_err(err_to_string)?;
        (permissions, auto_approve_mcp_elicitations)
    } else {
        (CliRuntimePermissions::default(), false)
    };
    let sandbox_mode_override = cli_permissions.sandbox_mode.clone();
    let supports_auracoder_sandbox = thread.engine_id != "opencode";
    let sandbox_mode = if supports_auracoder_sandbox {
        Some(
            sandbox_mode_override
                .clone()
                .unwrap_or_else(|| "workspace-write".to_string()),
        )
    } else {
        if sandbox_mode_override.is_some() {
            log::warn!(
                "ignoring sandbox mode override on OpenCode thread {}",
                thread.id
            );
        }
        None
    };
    let scope = ThreadScope::Project {
        root_path: workspace_root.clone(),
        writable_roots: vec![workspace_root.clone()],
    };

    let trust_level = workspace.trust_level.clone();
    let codex_external_sandbox_active =
        if let (Some(codex), Some(context)) = (codex_cli.as_ref(), codex_context.as_ref()) {
            let cli: &dyn CliTool = codex.as_ref();
            cli.uses_external_sandbox(context)
                .await
                .map_err(err_to_string)?
        } else {
            false
        };
    let permission_profile = if thread.engine_id == "codex" {
        cli_permissions.permission_profile.clone()
    } else {
        None
    };

    if permission_profile.is_none() {
        if let Some(sandbox_mode) = sandbox_mode.as_deref() {
            if unsupported_thread_sandbox_override_for_external_sandbox(
                sandbox_mode_override.as_deref(),
                codex_external_sandbox_active,
            ) {
                return Err(
                "Codex read-only and workspace-write sandbox overrides are unavailable while AuraCoder is using external sandbox mode. Clear the override or restore local Codex sandboxing first.".to_string(),
            );
            }

            validate_engine_sandbox_mode(thread.engine_id.as_str(), Some(sandbox_mode))?;
        }
    }

    if requested_model_id.is_some() || reasoning_effort != stored_reasoning_effort {
        let update = ThreadUpdateDto {
            id: thread.id.clone(),
            model_id: requested_model_id.map(ToOwned::to_owned),
            reasoning_effort: reasoning_effort.clone(),
            ..Default::default()
        };
        thread = run_db(db.clone(), move |db| {
            db::threads::update_thread(db, &update)
        })
        .await?;
    }

    let writable_roots = match &scope {
        ThreadScope::Project {
            writable_roots,
            root_path,
        } => {
            if writable_roots.is_empty() {
                vec![root_path.clone()]
            } else {
                writable_roots.clone()
            }
        }
    };

    let allow_network =
        if thread.engine_id == "codex" && sandbox_mode.as_deref() == Some("danger-full-access") {
            true
        } else {
            cli_permissions
                .allow_network
                .unwrap_or_else(|| allow_network_for_trust_level(&trust_level))
        };
    let model_supports_selected_personality = if execution_workspace.location_kind == "ssh" {
        validation_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|engine| engine.id == thread.engine_id)
            .and_then(|engine| {
                engine
                    .models
                    .iter()
                    .find(|model| model.id == effective_model_id)
            })
            .is_some_and(|model| model.supports_personality)
    } else {
        model_supports_personality(state, &thread.engine_id, &effective_model_id).await
    };
    let personality = if thread.engine_id == "codex" && model_supports_selected_personality {
        thread_personality(thread.engine_metadata.as_ref())
    } else {
        None
    };

    let approval_policy_override = cli_permissions.approval_policy.clone();

    let sandbox = SandboxPolicy {
        writable_roots,
        allow_network,
        auto_approve_mcp_elicitations,
        approval_policy: Some(approval_policy_override.unwrap_or_else(|| {
            Value::String(
                approval_policy_for_engine_and_trust_level(thread.engine_id.as_str(), &trust_level)
                    .to_string(),
            )
        })),
        permission_profile,
        approvals_reviewer: if thread.engine_id == "codex" {
            cli_permissions.approvals_reviewer.clone()
        } else {
            None
        },
        reasoning_effort: reasoning_effort.clone(),
        sandbox_mode,
        service_tier: thread_service_tier(thread.engine_metadata.as_ref()),
        personality,
        output_schema: thread_output_schema(thread.engine_metadata.as_ref()),
        opencode_agent: thread_opencode_agent(thread.engine_metadata.as_ref()),
    };

    // SSH 远端 CLI 的持续占用已在模型读取前取得，避免模型查询结束时关闭服务。
    let engine_thread_id = if let (Some(codex), Some(context)) =
        (codex_cli.as_ref(), codex_context.as_ref())
    {
        let cli: &dyn CliTool = codex.as_ref();
        cli.start_thread(
            context,
            &thread,
            scope,
            thread.engine_thread_id.as_deref(),
            effective_model_id.as_str(),
            sandbox,
        )
        .await
        .map(|engine_thread| engine_thread.engine_thread_id)
        .map_err(err_to_string)?
    } else if let (Some(opencode), Some(context)) =
        (opencode_cli.as_ref(), opencode_context.as_ref())
    {
        let cli: &dyn CliTool = opencode.as_ref();
        cli.start_thread(
            context,
            &thread,
            scope,
            thread.engine_thread_id.as_deref(),
            effective_model_id.as_str(),
            sandbox,
        )
        .await
        .map(|engine_thread| engine_thread.engine_thread_id)
        .map_err(err_to_string)?
    } else if let (Some(claude), Some(context)) = (claude_cli.as_ref(), claude_context.as_ref()) {
        let cli: &dyn CliTool = claude.as_ref();
        cli.start_thread(
            context,
            &thread,
            scope,
            thread.engine_thread_id.as_deref(),
            effective_model_id.as_str(),
            sandbox,
        )
        .await
        .map(|engine_thread| engine_thread.engine_thread_id)
        .map_err(err_to_string)?
    } else {
        state
            .engines
            .ensure_engine_thread(&thread, Some(effective_model_id.as_str()), scope, sandbox)
            .await
            .map_err(err_to_string)?
    };

    if thread.engine_thread_id.as_deref() != Some(&engine_thread_id) {
        run_db(db.clone(), {
            let thread_id = thread.id.clone();
            let engine_thread_id = engine_thread_id.clone();
            move |db| db::threads::set_engine_thread_id(db, &thread_id, &engine_thread_id)
        })
        .await?;
        thread.engine_thread_id = Some(engine_thread_id.clone());
    }
    state.auracoder_thread_mcp_service.bind_engine_thread(
        &thread.engine_id,
        &engine_thread_id,
        &thread.workspace_id,
    );

    let persistent_image_preview_batch = prepare_message_image_previews(&mut attachments).await?;
    let remote_attachment_batch = if execution_workspace.location_kind == "ssh"
        && !attachments.is_empty()
    {
        let connection_id = execution_workspace
            .ssh_connection_id
            .as_deref()
            .ok_or_else(|| "SSH 远端项目未绑定连接，无法上传附件".to_string())?
            .to_string();
        let connection = run_db(db.clone(), move |db| {
            db::ssh_connections::find(db, &connection_id)?
                .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在，无法上传远端附件: {connection_id}"))
        })
        .await?;
        Some(
            remote_attachments::upload_turn_attachments(
                &connection,
                &execution_workspace.id,
                &thread.id,
                &attachments,
            )
            .await
            .map_err(err_to_string)?,
        )
    } else {
        None
    };
    let attachments = remote_attachment_batch
        .as_ref()
        .map(|batch| batch.attachments.clone())
        .unwrap_or(attachments);
    let turn_input = TurnInput {
        message: engine_message.clone(),
        attachments: attachments.clone(),
        plan_mode,
        input_items: engine_input_items,
    };

    let cancellation = CancellationToken::new();
    if !state
        .turns
        .try_register(&thread.id, cancellation.clone())
        .await
    {
        if let Some(batch) = remote_attachment_batch.as_ref() {
            batch.cleanup().await;
        }
        return Err(
            "A turn is already running for this thread. Cancel it before sending another message."
                .to_string(),
        );
    }

    let (assistant_message, streaming_thread) = match run_db(db.clone(), {
        let thread_id = thread.id.clone();
        let message = message.clone();
        let attachments = attachments.clone();
        let input_items = input_items.clone();
        let plan_mode_enabled = plan_mode;
        let engine_id = thread.engine_id.clone();
        let model_id = effective_model_id.clone();
        let reasoning_effort = reasoning_effort.clone();
        let scheduled_run_id = scheduled_run_id.clone();
        move |db| {
            let user_blocks = build_user_blocks(
                &message,
                &input_items,
                &attachments,
                plan_mode_enabled,
                false,
                None,
            );
            db::messages::insert_user_message(
                db,
                &thread_id,
                &message,
                Some(serde_json::to_value(&user_blocks)?),
                Some(engine_id.as_str()),
                Some(model_id.as_str()),
                reasoning_effort.as_deref(),
            )?;
            let assistant_message = db::messages::insert_assistant_placeholder(
                db,
                &thread_id,
                Some(engine_id.as_str()),
                Some(model_id.as_str()),
                reasoning_effort.as_deref(),
            )?;
            db::threads::update_thread_status(db, &thread_id, ThreadStatusDto::Streaming)?;
            let streaming_thread = db::threads::get_thread(db, &thread_id)?.ok_or_else(|| {
                anyhow::anyhow!("thread not found after marking it streaming: {thread_id}")
            })?;
            if let Some(run_id) = scheduled_run_id.as_deref() {
                db::scheduled_tasks::mark_run_started(
                    db,
                    run_id,
                    &thread_id,
                    &assistant_message.id,
                )?;
            }
            Ok((assistant_message, streaming_thread))
        }
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            if let Some(batch) = remote_attachment_batch.as_ref() {
                batch.cleanup().await;
            }
            state.turns.finish(&thread.id).await;
            return Err(error);
        }
    };
    persistent_image_preview_batch.commit();

    let _ = app.emit(
        "thread-updated",
        ThreadUpdatedEvent {
            thread_id: streaming_thread.id.clone(),
            workspace_id: streaming_thread.workspace_id.clone(),
            thread: Some(streaming_thread),
        },
    );

    let state_cloned = state.clone();
    let app_handle = app.clone();
    let assistant_message_id = assistant_message.id.clone();
    let turn_input_for_task = turn_input.clone();
    let thread_for_task = thread.clone();
    let initial_turn_model_id = effective_model_id.clone();

    tokio::spawn(async move {
        run_turn(
            app_handle,
            state_cloned,
            thread_for_task,
            engine_thread_id,
            assistant_message_id,
            initial_turn_model_id,
            turn_input_for_task,
            client_turn_id,
            codex_cli.zip(codex_context),
            opencode_cli.zip(opencode_context),
            claude_cli.zip(claude_context),
            remote_attachment_batch,
            cancellation,
        )
        .await;
    });

    Ok(assistant_message.id)
}

#[tauri::command]
pub async fn start_codex_review(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    target: CodexReviewTargetPayload,
    delivery: Option<CodexReviewDeliveryPayload>,
) -> Result<ThreadDto, String> {
    if state.turns.get(&thread_id).await.is_some() {
        return Err(
            "A turn is already running for this thread. Cancel it before starting a review."
                .to_string(),
        );
    }

    let db = state.db.clone();
    let source_thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if source_thread.engine_id != "codex" {
        return Err("Native review is only available for Codex threads.".to_string());
    }
    let source_workspace = run_db(db.clone(), {
        let workspace_id = source_thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    if source_workspace.location_kind == "ssh" {
        return Err("SSH 远端 Codex 原生审查不在第4阶段剩余工作阶段计划1范围内，当前不会调用本机 Codex 执行".to_string());
    }

    let source_engine_thread_id = source_thread
        .engine_thread_id
        .clone()
        .ok_or_else(|| "Codex review requires an initialized server-backed thread.".to_string())?;
    let effective_delivery = delivery.unwrap_or(CodexReviewDeliveryPayload::Inline);
    let (target_payload, review_message, review_title) = normalize_codex_review_target(&target)?;
    let initial_turn_model_id = source_thread.model_id.clone();
    let reasoning_effort = source_thread.reasoning_effort.clone();

    let cancellation = CancellationToken::new();
    if !state
        .turns
        .try_register(&source_thread.id, cancellation.clone())
        .await
    {
        return Err(
            "A turn is already running for this thread. Cancel it before starting a review."
                .to_string(),
        );
    }

    let (review_thread, assistant_message_id) = match run_db(db.clone(), {
        let source_thread = source_thread.clone();
        let review_message = review_message.clone();
        let review_title = review_title.clone();
        let initial_turn_model_id = initial_turn_model_id.clone();
        let reasoning_effort = reasoning_effort.clone();
        let detached = matches!(effective_delivery, CodexReviewDeliveryPayload::Detached);
        move |db| {
            let review_thread = if detached {
                let created = db::threads::create_thread(
                    db,
                    &source_thread.workspace_id,
                    &source_thread.engine_id,
                    &initial_turn_model_id,
                    &review_title,
                )?;
                if let Some(metadata) = clone_codex_review_metadata(
                    source_thread.engine_metadata.as_ref(),
                    &initial_turn_model_id,
                ) {
                    db::threads::update_engine_metadata(db, &created.id, &metadata)?;
                }
                created
            } else {
                source_thread.clone()
            };

            let user_blocks = build_user_blocks(&review_message, &[], &[], false, false, None);
            db::messages::insert_user_message(
                db,
                &review_thread.id,
                &review_message,
                Some(serde_json::to_value(&user_blocks)?),
                Some(source_thread.engine_id.as_str()),
                Some(initial_turn_model_id.as_str()),
                reasoning_effort.as_deref(),
            )?;
            let assistant_message = db::messages::insert_assistant_placeholder(
                db,
                &review_thread.id,
                Some(source_thread.engine_id.as_str()),
                Some(initial_turn_model_id.as_str()),
                reasoning_effort.as_deref(),
            )?;
            db::threads::update_thread_status(db, &review_thread.id, ThreadStatusDto::Streaming)?;
            let updated_thread = db::threads::get_thread(db, &review_thread.id)?
                .ok_or_else(|| anyhow::anyhow!("review thread not found after setup"))?;
            Ok((updated_thread, assistant_message.id))
        }
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            state.turns.finish(&source_thread.id).await;
            return Err(error);
        }
    };

    if matches!(effective_delivery, CodexReviewDeliveryPayload::Detached) {
        if !state
            .turns
            .try_register(&review_thread.id, cancellation.clone())
            .await
        {
            log::warn!(
                "failed to register cancellation token for detached review thread {}",
                review_thread.id
            );
        }
    }

    let state_cloned = state.inner().clone();
    let app_handle = app.clone();
    let review_thread_for_task = review_thread.clone();
    let review_target_for_task = target_payload.clone();
    let source_engine_thread_id_for_task = source_engine_thread_id.clone();
    let assistant_message_id_for_task = assistant_message_id.clone();
    let delivery_label = match effective_delivery {
        CodexReviewDeliveryPayload::Inline => "inline".to_string(),
        CodexReviewDeliveryPayload::Detached => "detached".to_string(),
    };

    tokio::spawn(async move {
        run_codex_review_turn(
            app_handle,
            state_cloned,
            source_thread,
            review_thread_for_task,
            source_engine_thread_id_for_task,
            assistant_message_id_for_task,
            initial_turn_model_id,
            review_target_for_task,
            delivery_label,
            cancellation,
        )
        .await;
    });

    Ok(review_thread)
}

#[tauri::command]
pub async fn steer_message(
    state: State<'_, AppState>,
    thread_id: String,
    message: String,
    attachments: Option<Vec<ChatAttachmentPayload>>,
    input_items: Option<Vec<ChatInputItemPayload>>,
    plan_mode: Option<bool>,
    client_steer_id: String,
    referenced_thread_id: Option<String>,
) -> Result<SteerReceiptDto, String> {
    if client_steer_id.trim().is_empty() {
        return Err("clientSteerId is required for mid-turn steering.".to_string());
    }

    let steer_started_at = Instant::now();
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "steer_submit_started",
        "thread_id": thread_id,
        "client_steer_id": client_steer_id,
    }))
    .await;

    if state.turns.get(&thread_id).await.is_none() {
        return Err(
            "No active turn is running for this thread yet. Wait for Codex to start the turn before steering."
                .to_string(),
        );
    }

    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "codex" {
        return Err("Mid-turn steering is only available for Codex threads.".to_string());
    }
    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;

    let engine_thread_id = thread
        .engine_thread_id
        .clone()
        .ok_or_else(|| format!("thread `{thread_id}` has no active engine thread id"))?;
    let mut attachments = normalize_attachments(attachments)?;
    let referenced_thread =
        resolve_auracoder_thread_reference(state.inner(), &thread, referenced_thread_id.as_deref())
            .await?;
    let persistent_image_preview_batch = prepare_message_image_previews(&mut attachments).await?;
    // 阶段计划 3 直接拒绝 SSH steer 附件；阶段计划 4 使用同一安全上传前置流程。
    // if workspace.location_kind == "ssh" && !attachments.is_empty() {
    //     return Err("SSH 远端项目附件将在第4阶段剩余工作阶段计划4中接入；当前不能把本机附件路径发送给远端 Codex".to_string());
    // }
    let remote_attachment_batch = if workspace.location_kind == "ssh" && !attachments.is_empty() {
        let connection_id = workspace
            .ssh_connection_id
            .as_deref()
            .ok_or_else(|| "SSH 远端项目未绑定连接，无法上传附件".to_string())?
            .to_string();
        let connection = run_db(db.clone(), move |db| {
            db::ssh_connections::find(db, &connection_id)?
                .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在，无法上传远端附件: {connection_id}"))
        })
        .await?;
        Some(
            remote_attachments::upload_turn_attachments(
                &connection,
                &workspace.id,
                &thread.id,
                &attachments,
            )
            .await
            .map_err(err_to_string)?,
        )
    } else {
        None
    };
    let attachments = remote_attachment_batch
        .as_ref()
        .map(|batch| batch.attachments.clone())
        .unwrap_or(attachments);
    let input_items = normalize_input_items(message.as_str(), input_items)?;
    let plan_mode = plan_mode.unwrap_or(false);
    let browser_annotation_context = browser_annotation_context(&attachments);
    let mut engine_message = format_engine_message(&message, &browser_annotation_context);
    let mut engine_input_items =
        append_browser_annotation_context(&input_items, &browser_annotation_context);
    if let Some(reference) = referenced_thread.as_ref() {
        let instruction = build_auracoder_thread_instruction(reference);
        engine_message.push_str(&instruction);
        engine_input_items.push(TurnInputItem::Text { text: instruction });
    }
    let turn_input = TurnInput {
        message: engine_message.clone(),
        attachments: attachments.clone(),
        plan_mode,
        input_items: engine_input_items,
    };
    let effective_model_id = thread.model_id.clone();
    let reasoning_effort = thread.reasoning_effort.clone();
    let user_blocks = build_user_blocks(
        &message,
        &input_items,
        &attachments,
        plan_mode,
        true,
        Some(client_steer_id.as_str()),
    );

    let user_message = match run_db(db.clone(), {
        let thread_id = thread.id.clone();
        let message = message.clone();
        let user_blocks = user_blocks.clone();
        let engine_id = thread.engine_id.clone();
        let model_id = effective_model_id.clone();
        let reasoning_effort = reasoning_effort.clone();
        move |db| {
            db::messages::insert_user_message(
                db,
                &thread_id,
                &message,
                Some(serde_json::to_value(&user_blocks)?),
                Some(engine_id.as_str()),
                Some(model_id.as_str()),
                reasoning_effort.as_deref(),
            )
        }
    })
    .await
    {
        Ok(user_message) => user_message,
        Err(error) => {
            if let Some(batch) = remote_attachment_batch.as_ref() {
                batch.cleanup().await;
            }
            return Err(error);
        }
    };

    state.auracoder_thread_mcp_service.bind_engine_thread(
        &thread.engine_id,
        &engine_thread_id,
        &thread.workspace_id,
    );
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    let engine_receipt = match cli
        .steer_message(
            &context,
            &thread,
            &engine_thread_id,
            client_steer_id.as_str(),
            engine_message.as_str(),
            turn_input,
        )
        .await
    {
        Ok(receipt) => receipt,
        Err(error) => {
            if let Some(batch) = remote_attachment_batch.as_ref() {
                batch.cleanup().await;
            }
            let rollback_result = run_db(db, {
                let message_id = user_message.id.clone();
                move |db| db::messages::delete_message(db, &message_id)
            })
            .await;
            if let Err(rollback_error) = rollback_result {
                log::warn!(
                    "failed to roll back persisted steer message {} after steer error: {}",
                    user_message.id,
                    rollback_error
                );
                persistent_image_preview_batch.commit();
            }

            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "steer_rpc_failed",
                "thread_id": thread.id,
                "engine_thread_id": engine_thread_id,
                "client_steer_id": client_steer_id,
                "elapsed_ms": steer_started_at.elapsed().as_millis(),
                "error": error.to_string(),
            }))
            .await;

            return Err(err_to_string(error));
        }
    };
    persistent_image_preview_batch.commit();

    let accepted_at = chrono::Utc::now().to_rfc3339();
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": accepted_at,
        "event": "steer_rpc_accepted",
        "thread_id": thread.id,
        "engine_thread_id": engine_thread_id,
        "client_steer_id": client_steer_id,
        "expected_turn_id": engine_receipt.expected_turn_id,
        "elapsed_ms": steer_started_at.elapsed().as_millis(),
    }))
    .await;

    Ok(SteerReceiptDto {
        client_steer_id,
        expected_turn_id: engine_receipt.expected_turn_id,
        accepted_at,
    })
}

fn build_user_blocks(
    message: &str,
    input_items: &[TurnInputItem],
    attachments: &[TurnAttachment],
    plan_mode: bool,
    is_steer: bool,
    client_steer_id: Option<&str>,
) -> Vec<ContentBlock> {
    let mut user_blocks = Vec::with_capacity(
        input_items
            .len()
            .saturating_add(attachments.len())
            .saturating_add(1),
    );

    let mut structured_text_parts: Vec<String> = Vec::new();

    for item in input_items {
        match item {
            TurnInputItem::Skill { name, path } => {
                user_blocks.push(ContentBlock::Skill {
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            TurnInputItem::Mention { name, path } => {
                user_blocks.push(ContentBlock::Mention {
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            TurnInputItem::Text { text } => {
                structured_text_parts.push(text.clone());
            }
        }
    }

    for attachment in attachments {
        user_blocks.push(ContentBlock::Attachment {
            file_name: attachment.file_name.clone(),
            file_path: attachment.file_path.clone(),
            preview_file_path: attachment.preview_file_path.clone(),
            size_bytes: attachment.size_bytes,
            mime_type: attachment.mime_type.clone(),
            browser_annotation: attachment.browser_annotation.clone(),
            is_remote: attachment.is_remote,
        });
    }

    let final_text = if structured_text_parts.is_empty() {
        message.to_string()
    } else {
        structured_text_parts.join("\n")
    };
    user_blocks.push(ContentBlock::Text {
        content: final_text,
        plan_mode: if plan_mode { Some(true) } else { None },
        is_steer: if is_steer { Some(true) } else { None },
        client_steer_id: client_steer_id.map(str::to_string),
    });

    user_blocks
}

fn normalize_input_items(
    message: &str,
    input_items: Option<Vec<ChatInputItemPayload>>,
) -> Result<Vec<TurnInputItem>, String> {
    let mut normalized = Vec::new();

    for item in input_items.unwrap_or_default() {
        match item {
            ChatInputItemPayload::Text { text } => {
                if !text.is_empty() {
                    normalized.push(TurnInputItem::Text { text });
                }
            }
            ChatInputItemPayload::Skill { name, path } => {
                let name = name.trim();
                let path = path.trim();
                if name.is_empty() || path.is_empty() {
                    return Err("skill input items require non-empty name and path".to_string());
                }
                normalized.push(TurnInputItem::Skill {
                    name: name.to_string(),
                    path: path.to_string(),
                });
            }
            ChatInputItemPayload::Mention { name, path } => {
                let name = name.trim();
                let path = path.trim();
                if name.is_empty() || path.is_empty() {
                    return Err("mention input items require non-empty name and path".to_string());
                }
                normalized.push(TurnInputItem::Mention {
                    name: name.to_string(),
                    path: path.to_string(),
                });
            }
        }
    }

    if normalized.is_empty() {
        normalized.push(TurnInputItem::Text {
            text: message.to_string(),
        });
        return Ok(normalized);
    }

    let has_text_item = normalized
        .iter()
        .any(|item| matches!(item, TurnInputItem::Text { text } if !text.is_empty()));
    if !has_text_item && !message.trim().is_empty() {
        return Err(
            "input items must include at least one text segment when message text is provided"
                .to_string(),
        );
    }

    let mut merged = Vec::with_capacity(normalized.len());
    for item in normalized {
        match item {
            TurnInputItem::Text { text } => {
                if let Some(TurnInputItem::Text { text: current }) = merged.last_mut() {
                    current.push_str(&text);
                } else {
                    merged.push(TurnInputItem::Text { text });
                }
            }
            other => merged.push(other),
        }
    }

    Ok(merged)
}

fn normalize_attachments(
    attachments: Option<Vec<ChatAttachmentPayload>>,
) -> Result<Vec<TurnAttachment>, String> {
    let attachments = attachments.unwrap_or_default();
    if attachments.len() > MAX_ATTACHMENTS_PER_TURN {
        return Err(format!(
            "You can attach at most {MAX_ATTACHMENTS_PER_TURN} files per turn."
        ));
    }

    let mut normalized = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let file_path = attachment.file_path.trim().to_string();
        if file_path.is_empty() {
            return Err("Attachment path cannot be empty.".to_string());
        }

        let file_name = if attachment.file_name.trim().is_empty() {
            Path::new(&file_path)
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| file_path.clone())
        } else {
            attachment.file_name.trim().to_string()
        };

        let browser_annotation = normalize_browser_annotation(attachment.browser_annotation)?;

        normalized.push(TurnAttachment {
            file_name,
            file_path,
            preview_file_path: None,
            size_bytes: attachment.size_bytes,
            mime_type: attachment.mime_type,
            browser_annotation,
            is_remote: false,
            remote_text_content: None,
        });
    }

    Ok(normalized)
}

fn normalize_browser_annotation_text(
    value: Option<String>,
    maximum_length: usize,
    field_name: &str,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > maximum_length {
        return Err(format!("{field_name} is too long."));
    }
    Ok(Some(value.to_string()))
}

fn normalize_browser_annotation(
    annotation: Option<BrowserAnnotationMetadata>,
) -> Result<Option<BrowserAnnotationMetadata>, String> {
    let Some(annotation) = annotation else {
        return Ok(None);
    };
    let Some(comment) = normalize_browser_annotation_text(
        Some(annotation.comment),
        MAX_BROWSER_ANNOTATION_COMMENT_CHARS,
        "Browser annotation comment",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(BrowserAnnotationMetadata {
        comment,
        number: annotation.number.filter(|number| *number > 0),
        source_url: normalize_browser_annotation_text(
            annotation.source_url,
            MAX_BROWSER_ANNOTATION_URL_CHARS,
            "Browser annotation URL",
        )?,
        target_label: normalize_browser_annotation_text(
            annotation.target_label,
            MAX_BROWSER_ANNOTATION_TARGET_CHARS,
            "Browser annotation target",
        )?,
    }))
}

fn browser_annotation_context(attachments: &[TurnAttachment]) -> String {
    let mut lines = Vec::new();
    for attachment in attachments {
        let Some(annotation) = attachment.browser_annotation.as_ref() else {
            continue;
        };
        let number = annotation
            .number
            .map(|number| format!(" #{number}"))
            .unwrap_or_default();
        lines.push(format!("标注{number}：{}", annotation.comment));
        if let Some(url) = annotation.source_url.as_deref() {
            lines.push(format!("页面：{url}"));
        }
        if let Some(target) = annotation.target_label.as_deref() {
            lines.push(format!("定位目标：{target}"));
        }
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[浏览器标注附件：以下文字与对应截图关联，仅用于理解用户的问题，不要在回复中复述为用户的聊天原文。]\n{}",
            lines.join("\n")
        )
    }
}

fn format_engine_message(message: &str, browser_annotation_context: &str) -> String {
    if browser_annotation_context.is_empty() {
        message.to_string()
    } else {
        format!("{message}{browser_annotation_context}")
    }
}

fn append_browser_annotation_context(
    input_items: &[TurnInputItem],
    browser_annotation_context: &str,
) -> Vec<TurnInputItem> {
    let mut result = input_items.to_vec();
    if !browser_annotation_context.is_empty() {
        result.push(TurnInputItem::Text {
            text: browser_annotation_context.to_string(),
        });
    }
    result
}

fn validate_attachments_for_engine_model(
    attachments: &[TurnAttachment],
    engine_id: &str,
    model_id: &str,
    catalog: Option<&[EngineInfoDto]>,
) -> Result<(), String> {
    if attachments.is_empty() {
        return Ok(());
    }

    let Some(model) = catalog
        .and_then(|engines| engines.iter().find(|engine| engine.id == engine_id))
        .and_then(|engine| engine.models.iter().find(|model| model.id == model_id))
    else {
        return Ok(());
    };

    let allowed_modalities = if model.attachment_modalities.is_empty() {
        HashSet::new()
    } else {
        model
            .attachment_modalities
            .iter()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<HashSet<_>>()
    };

    if allowed_modalities.is_empty() {
        return Err(format!(
            "{} does not support file attachments.",
            model.display_name
        ));
    }

    for attachment in attachments {
        let Some(modality) = attachment_modality(attachment) else {
            return Err(format!(
                "{} is not a supported attachment type for {}.",
                attachment.file_name, model.display_name
            ));
        };
        if !allowed_modalities.contains(modality) {
            return Err(format!(
                "{} attachments are not supported by {}.",
                attachment_modality_label(modality),
                model.display_name
            ));
        }
    }

    Ok(())
}

fn attachment_modality(attachment: &TurnAttachment) -> Option<&'static str> {
    let extension = Path::new(&attachment.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_lowercase);
    let mime_type = attachment
        .mime_type
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase);

    if extension.as_deref() == Some("pdf") || mime_type.as_deref() == Some("application/pdf") {
        return Some("pdf");
    }
    if extension
        .as_deref()
        .map(|value| IMAGE_ATTACHMENT_EXTENSIONS.contains(&value))
        .unwrap_or(false)
        || mime_type
            .as_deref()
            .map(|value| value.starts_with("image/"))
            .unwrap_or(false)
    {
        return Some("image");
    }
    if extension
        .as_deref()
        .map(|value| TEXT_ATTACHMENT_EXTENSIONS.contains(&value))
        .unwrap_or(false)
        || mime_type
            .as_deref()
            .map(is_text_attachment_mime_type)
            .unwrap_or(false)
    {
        return Some("text");
    }

    None
}

fn is_text_attachment_mime_type(value: &str) -> bool {
    value.starts_with("text/")
        || matches!(
            value,
            "application/json"
                | "application/javascript"
                | "application/typescript"
                | "application/xml"
                | "application/x-sh"
                | "application/x-yaml"
                | "application/yaml"
                | "text/csv"
        )
}

fn attachment_modality_label(modality: &str) -> &'static str {
    match modality {
        "image" => "Image",
        "pdf" => "PDF",
        _ => "Text file",
    }
}

#[tauri::command]
pub async fn cancel_turn(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    cancel_turn_inner(state.inner(), thread_id).await
}

#[tauri::command]
pub async fn restart_remote_cli_service(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    let workspace = run_db(db, {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    if workspace.location_kind != "ssh" {
        return Err("当前会话不是 SSH 远端会话，不能重启远端 CLI 服务".to_string());
    }
    if !matches!(thread.engine_id.as_str(), "codex" | "opencode" | "claude") {
        return Err(format!("不支持重启该远端 CLI 服务: {}", thread.engine_id));
    }
    let _connection_id = workspace
        .ssh_connection_id
        .as_deref()
        .ok_or_else(|| "SSH 远端项目未绑定连接".to_string())?;

    let cli = CliToolFactory::new(state.inner().clone())
        .create(&thread.engine_id)
        .map_err(err_to_string)?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    cli.restart_service(&context).await.map_err(err_to_string)?;
    Ok(())
}

pub(crate) async fn cancel_turn_inner(state: &AppState, thread_id: String) -> Result<(), String> {
    state.turns.cancel(&thread_id).await;

    let db = state.db.clone();
    if let Some(thread) = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    {
        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await?;
        if thread.engine_id == "codex" {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let codex = CliToolFactory::new(state.clone())
                .create("codex")
                .expect("Codex CLI factory mapping must exist");
            let cli: &dyn CliTool = codex.as_ref();
            cli.interrupt(
                &context,
                &thread,
                thread.engine_thread_id.as_deref().unwrap_or("default"),
            )
            .await
            .map_err(err_to_string)?;
        } else if thread.engine_id == "opencode" {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let opencode = CliToolFactory::new(state.clone())
                .create("opencode")
                .expect("OpenCode CLI factory mapping must exist");
            let cli: &dyn CliTool = opencode.as_ref();
            cli.interrupt(
                &context,
                &thread,
                thread.engine_thread_id.as_deref().unwrap_or("default"),
            )
            .await
            .map_err(err_to_string)?;
        } else if thread.engine_id == "claude" {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let claude = CliToolFactory::new(state.clone())
                .create("claude")
                .expect("Claude CLI factory mapping must exist");
            let cli: &dyn CliTool = claude.as_ref();
            cli.interrupt(
                &context,
                &thread,
                thread.engine_thread_id.as_deref().unwrap_or("default"),
            )
            .await
            .map_err(err_to_string)?;
        } else if workspace.location_kind == "ssh" {
            return Err(format!(
                "SSH 远端项目当前阶段尚未接入 {} 取消操作",
                thread.engine_id
            ));
        } else {
            state
                .engines
                .interrupt(&thread)
                .await
                .map_err(err_to_string)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn respond_to_approval(
    state: State<'_, AppState>,
    thread_id: String,
    approval_id: String,
    response: Value,
) -> Result<(), String> {
    respond_to_approval_inner(state.inner(), thread_id, approval_id, response).await
}

pub(crate) async fn respond_to_approval_inner(
    state: &AppState,
    thread_id: String,
    approval_id: String,
    response: Value,
) -> Result<(), String> {
    if !response.is_object() {
        return Err("approval response must be a JSON object".to_string());
    }

    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    let normalized_response =
        normalize_approval_response_for_engine(thread.engine_id.as_str(), response)?;
    let approval_route =
        load_approval_response_route(db.clone(), thread.engine_id.as_str(), &approval_id).await?;

    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    if thread.engine_id == "codex" {
        let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
        let codex = CliToolFactory::new(state.clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let cli: &dyn CliTool = codex.as_ref();
        cli.respond_to_approval(
            &context,
            &thread,
            &approval_id,
            normalized_response.clone(),
            approval_route,
        )
        .await
        .map_err(err_to_string)?;
    } else if thread.engine_id == "opencode" {
        let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
        let opencode = CliToolFactory::new(state.clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let cli: &dyn CliTool = opencode.as_ref();
        cli.respond_to_approval(
            &context,
            &thread,
            &approval_id,
            normalized_response.clone(),
            approval_route,
        )
        .await
        .map_err(err_to_string)?;
    } else if thread.engine_id == "claude" {
        let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
        let claude = CliToolFactory::new(state.clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let cli: &dyn CliTool = claude.as_ref();
        cli.respond_to_approval(
            &context,
            &thread,
            &approval_id,
            normalized_response.clone(),
            approval_route,
        )
        .await
        .map_err(err_to_string)?;
    } else if workspace.location_kind == "ssh" {
        return Err(format!(
            "SSH 远端项目当前阶段尚未接入 {} 审批回复",
            thread.engine_id
        ));
    } else {
        state
            .engines
            .respond_to_approval(
                &thread,
                &approval_id,
                normalized_response.clone(),
                approval_route,
            )
            .await
            .map_err(err_to_string)?;
    }

    let decision = approval_response_decision_for_persistence(&normalized_response);
    run_db(db, {
        let approval_id = approval_id.clone();
        let thread_id = thread_id.clone();
        let decision = decision.to_string();
        move |db| {
            db::actions::answer_approval(db, &approval_id, &decision)?;
            if let Some(message_id) = db::actions::find_approval_message_id(db, &approval_id)? {
                let _ = db::messages::mark_approval_block_answered(
                    db,
                    &message_id,
                    &approval_id,
                    &decision,
                );
            }
            db::threads::update_thread_status(db, &thread_id, ThreadStatusDto::Streaming)?;
            Ok(())
        }
    })
    .await?;

    Ok(())
}

async fn load_approval_response_route(
    db: crate::db::Database,
    engine_id: &str,
    approval_id: &str,
) -> Result<Option<ApprovalRequestRoute>, String> {
    let engine_id = engine_id.to_string();
    let approval_id = approval_id.to_string();
    run_db(db, move |db| {
        let details = db::actions::find_approval_details(db, &approval_id)?;
        Ok(details.and_then(|details| approval_response_route_for_engine(&engine_id, &details)))
    })
    .await
}

fn approval_response_decision_for_persistence(response: &Value) -> &'static str {
    if let Some(decision) = response.get("decision").and_then(Value::as_str) {
        return match decision {
            "deny" => "decline",
            "acceptForSession" => "accept_for_session",
            "accept" => "accept",
            "decline" => "decline",
            "cancel" => "cancel",
            "accept_for_session" => "accept_for_session",
            _ => "custom",
        };
    }

    if let Some(action) = response.get("action").and_then(Value::as_str) {
        return match action {
            "accept" => "accept",
            "decline" => "decline",
            "cancel" => "cancel",
            _ => "custom",
        };
    }

    if response.get("permissions").is_some() {
        if permission_profile_is_empty(response.get("permissions")) {
            return "decline";
        }
        if matches!(
            response.get("scope").and_then(Value::as_str),
            Some("session")
        ) {
            return "accept_for_session";
        }
        return "accept";
    }

    "custom"
}

fn permission_profile_is_empty(value: Option<&Value>) -> bool {
    fn has_granted_permission(value: &Value) -> bool {
        match value {
            Value::Bool(value) => *value,
            Value::String(value) => !value.trim().is_empty() && value != "none",
            Value::Array(items) => items.iter().any(has_granted_permission),
            Value::Object(map) => map.values().any(has_granted_permission),
            _ => false,
        }
    }

    match value {
        Some(value) => !has_granted_permission(value),
        None => true,
    }
}

#[tauri::command]
pub async fn get_thread_messages(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<MessageDto>, String> {
    run_db(state.db.clone(), move |db| {
        db::messages::get_thread_messages(db, &thread_id)
    })
    .await
}

#[tauri::command]
pub async fn get_thread_messages_window(
    state: State<'_, AppState>,
    thread_id: String,
    cursor: Option<MessageWindowCursorDto>,
    limit: Option<usize>,
) -> Result<MessageWindowDto, String> {
    let requested_limit = limit.unwrap_or(MESSAGE_WINDOW_DEFAULT_LIMIT);
    let clamped_limit = requested_limit.clamp(1, MESSAGE_WINDOW_MAX_LIMIT);

    run_db(state.db.clone(), move |db| {
        db::messages::get_thread_messages_window(db, &thread_id, cursor.as_ref(), clamped_limit)
    })
    .await
}

#[tauri::command]
pub async fn get_message_blocks(
    state: State<'_, AppState>,
    message_id: String,
) -> Result<Option<Value>, String> {
    run_db(state.db.clone(), move |db| {
        db::messages::get_message_blocks(db, &message_id)
    })
    .await
}

#[tauri::command]
pub async fn get_action_output(
    state: State<'_, AppState>,
    message_id: String,
    action_id: String,
) -> Result<ActionOutputDto, String> {
    run_db(state.db.clone(), move |db| {
        db::messages::get_action_output(db, &message_id, &action_id)
    })
    .await
}

#[tauri::command]
pub async fn search_messages(
    state: State<'_, AppState>,
    workspace_id: String,
    query: String,
) -> Result<Vec<SearchResultDto>, String> {
    run_db(state.db.clone(), move |db| {
        db::messages::search_messages(db, &workspace_id, &query)
    })
    .await
}

fn normalize_codex_review_target(
    target: &CodexReviewTargetPayload,
) -> Result<(Value, String, String), String> {
    match target {
        CodexReviewTargetPayload::UncommittedChanges => Ok((
            serde_json::json!({
                "type": "uncommittedChanges",
            }),
            "Review uncommitted changes.".to_string(),
            "Review: Uncommitted changes".to_string(),
        )),
        CodexReviewTargetPayload::BaseBranch { branch } => {
            let branch = branch.trim();
            if branch.is_empty() {
                return Err("Base branch review requires a branch name.".to_string());
            }
            Ok((
                serde_json::json!({
                    "type": "baseBranch",
                    "branch": branch,
                }),
                format!("Review changes against base branch `{branch}`."),
                truncate_title(format!("Review: {branch}"), MAX_THREAD_TITLE_CHARS),
            ))
        }
        CodexReviewTargetPayload::Commit { sha, title } => {
            let sha = sha.trim();
            if sha.is_empty() {
                return Err("Commit review requires a commit SHA.".to_string());
            }
            let short_sha = sha.chars().take(12).collect::<String>();
            let normalized_title = title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let message = match normalized_title.as_deref() {
                Some(title) => format!("Review commit `{sha}`: {title}"),
                None => format!("Review commit `{sha}`."),
            };
            let target = match normalized_title {
                Some(title) => serde_json::json!({
                    "type": "commit",
                    "sha": sha,
                    "title": title,
                }),
                None => serde_json::json!({
                    "type": "commit",
                    "sha": sha,
                }),
            };
            Ok((
                target,
                message,
                truncate_title(format!("Review: {short_sha}"), MAX_THREAD_TITLE_CHARS),
            ))
        }
        CodexReviewTargetPayload::Custom { instructions } => {
            let instructions = instructions.trim();
            if instructions.is_empty() {
                return Err("Custom review requires instructions.".to_string());
            }
            Ok((
                serde_json::json!({
                    "type": "custom",
                    "instructions": instructions,
                }),
                instructions.to_string(),
                "Review: Custom".to_string(),
            ))
        }
    }
}

fn clone_codex_review_metadata(existing: Option<&Value>, model_id: &str) -> Option<Value> {
    let mut metadata = existing.cloned().unwrap_or_else(|| serde_json::json!({}));
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }

    let object = metadata.as_object_mut()?;
    object.remove("manualTitle");
    object.remove("manualTitleUpdatedAt");
    object.remove("codexPreview");
    object.remove("codexThreadStatus");
    object.remove("codexThreadActiveFlags");
    object.remove("codexSyncRequired");
    object.remove("codexSyncReason");
    let _ = model_id;
    Some(metadata)
}

async fn run_turn(
    app: tauri::AppHandle,
    state: AppState,
    thread: crate::models::ThreadDto,
    engine_thread_id: String,
    assistant_message_id: String,
    initial_turn_model_id: String,
    turn_input: TurnInput,
    client_turn_id: Option<String>,
    codex_cli: Option<(Arc<dyn CliTool>, CliExecutionContext)>,
    opencode_cli: Option<(Arc<dyn CliTool>, CliExecutionContext)>,
    claude_cli: Option<(Arc<dyn CliTool>, CliExecutionContext)>,
    remote_attachment_batch: Option<RemoteAttachmentBatch>,
    cancellation: CancellationToken,
) {
    let max_output_chars = state.config.debug.max_action_output_chars;
    log::info!(
        "chat run_turn entered: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, client_turn_id={:?}, turn_input={turn_input:?}, codex_cli_present={}, opencode_cli_present={}, claude_cli_present={}",
        thread.id,
        thread.engine_id,
        engine_thread_id,
        assistant_message_id,
        client_turn_id,
        codex_cli.is_some(),
        opencode_cli.is_some(),
        claude_cli.is_some(),
    );
    let (event_tx, mut event_rx) = mpsc::channel::<EngineEvent>(ENGINE_EVENT_QUEUE_CAPACITY);

    let engines = state.engines.clone();
    let thread_for_engine = thread.clone();
    let input_for_engine = turn_input.clone();
    let engine_thread_for_engine = engine_thread_id.clone();
    let engine_cancellation = CancellationToken::new();
    let cancellation_for_engine = engine_cancellation.clone();
    let cli_turn = codex_cli.or(opencode_cli).or(claude_cli);
    let cli_turn_for_engine = cli_turn.clone();
    let engines_for_engine = engines.clone();
    let assistant_message_id_for_engine = assistant_message_id.clone();

    let mut engine_task = tokio::spawn(async move {
        if let Some((cli, context)) = cli_turn_for_engine.as_ref() {
            cli.register_mcp_context(
                context,
                &engine_thread_for_engine,
                &assistant_message_id_for_engine,
            )
            .await
            .context("当前CLI无法登记AuraCoder MCP调用上下文")?;
        }
        if let Some((cli, context)) = cli_turn_for_engine {
            let send_result = cli
                .send_message(
                    &context,
                    &thread_for_engine,
                    &engine_thread_for_engine,
                    input_for_engine,
                    event_tx,
                    cancellation_for_engine,
                )
                .await;
            /*
             * 旧实现只在 send_message 返回 Err 时自行记录并取消，无法覆盖同一轮中的
             * 事件接收、超时和任务退出异常。保留原代码，统一异常处理移到 run_turn。
            if let Err(send_error) = send_result {
                log::error!(
                    "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={send_error:#}",
                    thread_for_engine.engine_id,
                    thread_for_engine.id,
                    engine_thread_for_engine,
                );
                return match cli
                    .interrupt(&context, &thread_for_engine, &engine_thread_for_engine)
                    .await
                {
                    Ok(()) => Err(anyhow::anyhow!(
                        "本轮对话执行失败，远端执行已取消：{send_error:#}"
                    )),
                    Err(interrupt_error) => {
                        log::error!(
                            "failed to interrupt chat turn after execution error: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
                            thread_for_engine.engine_id,
                            thread_for_engine.id,
                            engine_thread_for_engine,
                        );
                        Err(anyhow::anyhow!(
                            "本轮对话执行失败，并且取消远端执行失败。执行错误：{send_error:#}；取消错误：{interrupt_error:#}"
                        ))
                    }
                };
            }
            Ok(())
             */
            send_result
        } else {
            let send_result = engines_for_engine
                .send_message(
                    &thread_for_engine,
                    &engine_thread_for_engine,
                    input_for_engine,
                    event_tx,
                    cancellation_for_engine,
                )
                .await;
            /*
             * 旧 Engine 兼容入口的异常收尾同样移到 run_turn，发送任务只返回 Result。
            if let Err(send_error) = send_result {
                log::error!(
                    "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={send_error:#}",
                    thread_for_engine.engine_id,
                    thread_for_engine.id,
                    engine_thread_for_engine,
                );
                return match engines_for_engine.interrupt(&thread_for_engine).await {
                    Ok(()) => Err(anyhow::anyhow!(
                        "本轮对话执行失败，远端执行已取消：{send_error:#}"
                    )),
                    Err(interrupt_error) => {
                        log::error!(
                            "failed to interrupt chat turn after execution error: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
                            thread_for_engine.engine_id,
                            thread_for_engine.id,
                            engine_thread_for_engine,
                        );
                        Err(anyhow::anyhow!(
                            "本轮对话执行失败，并且取消远端执行失败。执行错误：{send_error:#}；取消错误：{interrupt_error:#}"
                        ))
                    }
                };
            }
            Ok(())
             */
            send_result
        }
    });

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut action_index: HashMap<String, usize> = HashMap::new();
    let mut approval_index: HashMap<String, usize> = HashMap::new();
    let mut message_status = MessageStatusDto::Streaming;
    let mut thread_status = ThreadStatusDto::Streaming;
    let mut turn_model_id = initial_turn_model_id;
    let mut token_usage: Option<(u64, u64)> = None;
    let mut blocks_dirty = false;
    let mut message_state_dirty = false;
    let mut thread_status_dirty = false;
    let mut turn_model_dirty = false;
    let mut last_persist_at = Instant::now();
    let mut last_blocks_persist_at = Instant::now();
    let mut last_persisted_thread_status = thread_status.clone();
    let stream_event_topic = format!("stream-event-{}", thread.id);
    let approval_event_topic = format!("approval-request-{}", thread.id);
    let mut pending_event: Option<EngineEvent> = None;

    let initial_turn_started_event = EngineEvent::TurnStarted {
        client_turn_id,
        remote_turn_id: None,
    };
    let initial_progress = process_stream_event(
        &app,
        &state,
        &thread,
        &assistant_message_id,
        &stream_event_topic,
        &approval_event_topic,
        &initial_turn_started_event,
        &mut blocks,
        &mut action_index,
        &mut approval_index,
        max_output_chars,
    )
    .await;
    let initial_force_persist = apply_stream_progress(
        initial_progress,
        &mut message_status,
        &mut thread_status,
        &mut turn_model_id,
        &mut token_usage,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
    );
    flush_stream_state(
        &state,
        &thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        initial_force_persist,
    )
    .await;

    enum TurnOutcome {
        Completed,
        Cancelled,
        Failed(anyhow::Error),
    }

    #[derive(Debug)]
    enum TurnWait {
        Engine(std::result::Result<anyhow::Result<()>, tokio::task::JoinError>),
        Event(Option<EngineEvent>),
        FlushPending,
        Cancelled,
        TimedOut,
    }

    let mut engine_task_finished = false;
    let mut event_channel_closed = false;
    let mut failed_completion_processed = false;
    let mut turn_outcome = 'turn_loop: loop {
        let wait = if pending_event.is_some() {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnWait::Cancelled,
                result = &mut engine_task, if !engine_task_finished => TurnWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_coalesce"), if !event_channel_closed => TurnWait::Event(event),
                _ = tokio::time::sleep(STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL) => TurnWait::FlushPending,
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnWait::TimedOut,
            }
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnWait::Cancelled,
                result = &mut engine_task, if !engine_task_finished => TurnWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_direct"), if !event_channel_closed => TurnWait::Event(event),
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnWait::TimedOut,
            }
        };

        log::info!(
            "chat run_turn wait result: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, wait={wait:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        let incoming_event = match wait {
            TurnWait::Engine(Ok(Ok(()))) => {
                engine_task_finished = true;
                if event_channel_closed {
                    break 'turn_loop TurnOutcome::Completed;
                }
                continue;
            }
            TurnWait::Engine(Ok(Err(error))) => {
                engine_task_finished = true;
                break 'turn_loop TurnOutcome::Failed(error);
            }
            TurnWait::Engine(Err(error)) => {
                engine_task_finished = true;
                break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                    "执行本轮对话的后台任务异常退出：{error}"
                ));
            }
            TurnWait::Event(Some(event)) => event,
            TurnWait::Event(None) => {
                event_channel_closed = true;
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": chrono::Utc::now().to_rfc3339(),
                    "event": "engine_event_channel_closed",
                    "consumer": "chat",
                    "thread_id": thread.id.clone(),
                }))
                .await;
                if engine_task_finished {
                    break 'turn_loop TurnOutcome::Completed;
                }
                if matches!(message_status, MessageStatusDto::Streaming) {
                    break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                        "本轮对话尚未完成，但事件接收通道已经关闭"
                    ));
                }
                continue;
            }
            TurnWait::FlushPending => {
                if let Some(event) = pending_event.take() {
                    let progress = process_stream_event(
                        &app,
                        &state,
                        &thread,
                        &assistant_message_id,
                        &stream_event_topic,
                        &approval_event_topic,
                        &event,
                        &mut blocks,
                        &mut action_index,
                        &mut approval_index,
                        max_output_chars,
                    )
                    .await;
                    let force_persist = apply_stream_progress(
                        progress,
                        &mut message_status,
                        &mut thread_status,
                        &mut turn_model_id,
                        &mut token_usage,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                    );
                    flush_stream_state(
                        &state,
                        &thread,
                        &assistant_message_id,
                        &blocks,
                        &message_status,
                        &thread_status,
                        &turn_model_id,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                        &mut last_persisted_thread_status,
                        &mut last_persist_at,
                        &mut last_blocks_persist_at,
                        force_persist,
                    )
                    .await;
                }
                continue;
            }
            TurnWait::Cancelled => break 'turn_loop TurnOutcome::Cancelled,
            TurnWait::TimedOut => {
                break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                    "本轮对话在15分钟内没有收到新的执行事件"
                ));
            }
        };

        log::info!(
            "chat run_turn incoming EngineEvent before processing: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, event={incoming_event:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        if let EngineEvent::Error {
            message,
            recoverable: false,
        } = &incoming_event
        {
            break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(message.clone()));
        }

        let failed_completion = matches!(
            &incoming_event,
            EngineEvent::TurnCompleted {
                status: TurnCompletionStatus::Failed,
                ..
            }
        );

        let mut current_event = incoming_event;

        loop {
            if let Some(previous_event) = pending_event.take() {
                match try_coalesce_stream_events(previous_event, current_event) {
                    Ok(merged_event) => {
                        if coalesced_event_content_len(&merged_event)
                            >= STREAM_EVENT_COALESCE_MAX_CHARS
                        {
                            let progress = process_stream_event(
                                &app,
                                &state,
                                &thread,
                                &assistant_message_id,
                                &stream_event_topic,
                                &approval_event_topic,
                                &merged_event,
                                &mut blocks,
                                &mut action_index,
                                &mut approval_index,
                                max_output_chars,
                            )
                            .await;
                            let force_persist = apply_stream_progress(
                                progress,
                                &mut message_status,
                                &mut thread_status,
                                &mut turn_model_id,
                                &mut token_usage,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                            );
                            flush_stream_state(
                                &state,
                                &thread,
                                &assistant_message_id,
                                &blocks,
                                &message_status,
                                &thread_status,
                                &turn_model_id,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                                &mut last_persisted_thread_status,
                                &mut last_persist_at,
                                &mut last_blocks_persist_at,
                                force_persist,
                            )
                            .await;
                        } else {
                            pending_event = Some(merged_event);
                        }
                        break;
                    }
                    Err((unmerged_previous_event, unmerged_current_event)) => {
                        let progress = process_stream_event(
                            &app,
                            &state,
                            &thread,
                            &assistant_message_id,
                            &stream_event_topic,
                            &approval_event_topic,
                            &unmerged_previous_event,
                            &mut blocks,
                            &mut action_index,
                            &mut approval_index,
                            max_output_chars,
                        )
                        .await;
                        let force_persist = apply_stream_progress(
                            progress,
                            &mut message_status,
                            &mut thread_status,
                            &mut turn_model_id,
                            &mut token_usage,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                        );
                        flush_stream_state(
                            &state,
                            &thread,
                            &assistant_message_id,
                            &blocks,
                            &message_status,
                            &thread_status,
                            &turn_model_id,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                            &mut last_persisted_thread_status,
                            &mut last_persist_at,
                            &mut last_blocks_persist_at,
                            force_persist,
                        )
                        .await;
                        current_event = unmerged_current_event;
                    }
                }
            } else if is_coalescable_stream_event(&current_event) {
                pending_event = Some(current_event);
                break;
            } else {
                let progress = process_stream_event(
                    &app,
                    &state,
                    &thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &current_event,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
                )
                .await;
                let force_persist = apply_stream_progress(
                    progress,
                    &mut message_status,
                    &mut thread_status,
                    &mut turn_model_id,
                    &mut token_usage,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                );
                flush_stream_state(
                    &state,
                    &thread,
                    &assistant_message_id,
                    &blocks,
                    &message_status,
                    &thread_status,
                    &turn_model_id,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                    &mut last_persisted_thread_status,
                    &mut last_persist_at,
                    &mut last_blocks_persist_at,
                    force_persist,
                )
                .await;
                break;
            }
        }

        if failed_completion {
            failed_completion_processed = true;
            break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!("当前 CLI 报告本轮对话执行失败"));
        }
    };

    if let Some(event) = pending_event.take() {
        let progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        )
        .await;
        let force_persist = apply_stream_progress(
            progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            force_persist,
        )
        .await;
    }

    if matches!(&turn_outcome, TurnOutcome::Completed)
        && matches!(message_status, MessageStatusDto::Streaming)
    {
        turn_outcome = TurnOutcome::Failed(anyhow::anyhow!(
            "当前 CLI 已结束本轮任务，但没有上报本轮完成状态"
        ));
    }

    /*
     * 旧实现只在事件通道关闭后检查 engine_task，并分别处理发送错误和 JoinError。
     * 它没有统一等待事件异常、取消和超时。保留原代码，改由下方唯一出口处理。
    let mut engine_failed = false;
    match engine_task.await {
        Ok(Ok(())) => {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "ok",
            }))
            .await;
        }
        Ok(Err(error)) => {
            engine_failed = true;
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "error",
                "error": error.to_string(),
            }))
            .await;
            blocks.push(ContentBlock::Error {
                message: format!("Engine error: {error:#}"),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
            let _ = app.emit(
                &stream_event_topic,
                EngineEvent::Error {
                    message: format!("{error:#}"),
                    recoverable: false,
                },
            );
        }
        Err(error) => {
            engine_failed = true;
            let interrupt_result = if let Some((cli, context)) = cli_turn_for_join_error {
                cli.interrupt(
                    &context,
                    &thread_for_join_error,
                    &engine_thread_for_join_error,
                )
                .await
            } else {
                engines_for_join_error
                    .interrupt(&thread_for_join_error)
                    .await
            };
            let error_message = match interrupt_result {
                Ok(()) => format!("Engine task join error，远端执行已取消: {error}"),
                Err(interrupt_error) => {
                    log::error!(
                        "failed to interrupt chat turn after join error: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
                        thread_for_join_error.engine_id,
                        thread_for_join_error.id,
                        engine_thread_for_join_error,
                    );
                    format!(
                        "Engine task join error，并且取消远端执行失败: {error}；取消错误：{interrupt_error:#}"
                    )
                }
            };
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "join_error",
                "error": error.to_string(),
            }))
            .await;
            blocks.push(ContentBlock::Error {
                message: error_message.clone(),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
            let _ = app.emit(
                &stream_event_topic,
                EngineEvent::Error {
                    message: error_message,
                    recoverable: false,
                },
            );
        }
    }
     */

    let initial_failure_message = match &turn_outcome {
        TurnOutcome::Failed(error) => {
            Some(format!("本轮对话执行失败，正在取消远端执行：{error:#}"))
        }
        TurnOutcome::Completed | TurnOutcome::Cancelled => None,
    };
    if let Some(error_message) = initial_failure_message {
        log::error!(
            "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={error_message}",
            thread.engine_id,
            thread.id,
            engine_thread_id,
        );
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "event": "engine_task_complete",
            "consumer": "chat",
            "thread_id": thread.id.clone(),
            "result": "error",
            "error": error_message.clone(),
        }))
        .await;
        blocks.push(ContentBlock::Error {
            message: error_message.clone(),
        });
        blocks_dirty = true;
        if message_status != MessageStatusDto::Error {
            message_status = MessageStatusDto::Error;
            message_state_dirty = true;
        }
        if thread_status != ThreadStatusDto::Error {
            thread_status = ThreadStatusDto::Error;
            thread_status_dirty = true;
        }
        let _ = app.emit(
            &stream_event_topic,
            EngineEvent::Error {
                message: error_message,
                recoverable: false,
            },
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            true,
        )
        .await;
    }

    let must_interrupt = !matches!(&turn_outcome, TurnOutcome::Completed);
    let interrupt_result = if must_interrupt {
        Some(
            match tokio::time::timeout(TURN_INTERRUPT_TIMEOUT, async {
                if let Some((cli, context)) = cli_turn.as_ref() {
                    cli.interrupt(context, &thread, &engine_thread_id).await
                } else {
                    engines.interrupt(&thread).await
                }
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("取消远端执行超过10秒仍未完成")),
            },
        )
    } else {
        None
    };

    if must_interrupt {
        engine_cancellation.cancel();
        if !engine_task_finished {
            if tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task)
                .await
                .is_err()
            {
                engine_task.abort();
                let _ = engine_task.await;
            }
        }
    }

    if let Some((cli, context)) = cli_turn.as_ref() {
        if let Err(error) = cli.clear_mcp_context(context).await {
            log::error!(
                "清理当前 CLI AuraCoder MCP 调用上下文失败: engine_id={}, thread_id={}, engine_thread_id={}, error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
        }
    }

    let interrupt_error = interrupt_result.and_then(Result::err);
    if let Some(interrupt_error) = &interrupt_error {
        log::error!(
            "failed to interrupt chat turn: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
            thread.engine_id,
            thread.id,
            engine_thread_id,
        );
        if let Some((_, context)) = cli_turn.as_ref() {
            if context.location_kind == CliLocationKind::Ssh {
                if let Some(connection_id) = context.ssh_connection_id.as_ref() {
                    let _ = app.emit(
                        "chat-cli-service-restart-required",
                        CliServiceRestartRequiredEvent {
                            thread_id: thread.id.clone(),
                            workspace_id: thread.workspace_id.clone(),
                            engine_id: thread.engine_id.clone(),
                            thread_title: thread.title.clone(),
                            connection_id: connection_id.clone(),
                            reason: format!("{interrupt_error:#}"),
                        },
                    );
                }
            }
        }
    }
    let mut engine_failed = matches!(&turn_outcome, TurnOutcome::Failed(_));
    let turn_failure_message = match turn_outcome {
        TurnOutcome::Completed => {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "ok",
            }))
            .await;
            None
        }
        TurnOutcome::Cancelled => {
            interrupt_error.map(|error| format!("本轮对话已停止，但取消远端执行失败：{error:#}"))
        }
        TurnOutcome::Failed(error) => {
            /*
             * 旧实现直到 interrupt 返回后才记录本轮错误并通知界面。远端取消卡住时，
             * 用户也会一直看不到错误。日志和首次界面反馈已移到 interrupt 之前。
            log::error!(
                "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "error",
                "error": error.to_string(),
            }))
            .await;
             */
            match interrupt_error {
                Some(interrupt_error) => Some(format!(
                    "本轮对话执行失败，并且取消远端执行失败。执行错误：{error:#}；取消错误：{interrupt_error:#}"
                )),
                None => {
                    let notice = EngineEvent::Notice {
                        kind: "remote_interrupt_completed".to_string(),
                        level: "info".to_string(),
                        title: "远端任务已终止".to_string(),
                        message: "本轮异常对应的远端执行已经取消。".to_string(),
                        // 远端中断提示不附带结构化后台任务元数据。
                        metadata: None,
                    };
                    let progress = process_stream_event(
                        &app,
                        &state,
                        &thread,
                        &assistant_message_id,
                        &stream_event_topic,
                        &approval_event_topic,
                        &notice,
                        &mut blocks,
                        &mut action_index,
                        &mut approval_index,
                        max_output_chars,
                    )
                    .await;
                    apply_stream_progress(
                        progress,
                        &mut message_status,
                        &mut thread_status,
                        &mut turn_model_id,
                        &mut token_usage,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                    );
                    None
                }
            }
        }
    };

    // engine_failed 已在统一异常出口根据 TurnOutcome 初始化。
    if let Some(error_message) = turn_failure_message {
        engine_failed = true;
        blocks.push(ContentBlock::Error {
            message: error_message.clone(),
        });
        blocks_dirty = true;
        if message_status != MessageStatusDto::Error {
            message_status = MessageStatusDto::Error;
            message_state_dirty = true;
        }
        if thread_status != ThreadStatusDto::Error {
            thread_status = ThreadStatusDto::Error;
            thread_status_dirty = true;
        }
        let _ = app.emit(
            &stream_event_topic,
            EngineEvent::Error {
                message: error_message,
                recoverable: false,
            },
        );
    }

    if engine_failed && !failed_completion_processed {
        let failed_event = EngineEvent::TurnCompleted {
            token_usage: None,
            status: TurnCompletionStatus::Failed,
        };
        let failed_progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &failed_event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        )
        .await;
        apply_stream_progress(
            failed_progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
    }

    if cancellation.is_cancelled() && matches!(message_status, MessageStatusDto::Streaming) {
        let interrupted_event = EngineEvent::TurnCompleted {
            token_usage: None,
            status: TurnCompletionStatus::Interrupted,
        };
        let interrupted_progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &interrupted_event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        )
        .await;
        let interrupted_force_persist = apply_stream_progress(
            interrupted_progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            interrupted_force_persist,
        )
        .await;
    }

    if cancellation.is_cancelled() && matches!(message_status, MessageStatusDto::Streaming) {
        message_status = MessageStatusDto::Interrupted;
        message_state_dirty = true;
        thread_status = ThreadStatusDto::Idle;
        thread_status_dirty = true;
    }

    flush_stream_state(
        &state,
        &thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        true,
    )
    .await;

    if engine_failed {
        if let Some(batch) = remote_attachment_batch.as_ref() {
            batch.cleanup().await;
        }
    }
    state.turns.finish(&thread.id).await;

    let assistant_message_persisted = match run_db(state.db.clone(), {
        let assistant_message_id = assistant_message_id.clone();
        let message_status = message_status.clone();
        let token_usage = token_usage;
        move |db| {
            db::messages::complete_assistant_message(
                db,
                &assistant_message_id,
                message_status,
                token_usage,
                Some(turn_model_id.as_str()),
            )
        }
    })
    .await
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!("failed to complete assistant message: {error}");
            false
        }
    };

    if assistant_message_persisted && matches!(message_status, MessageStatusDto::Completed) {
        emit_assistant_message_completed(&app, &state, &thread.id, &assistant_message_id).await;
    }

    if matches!(message_status, MessageStatusDto::Completed) {
        if let Err(error) = run_db(state.db.clone(), {
            let thread_id = thread.id.clone();
            let token_usage = token_usage;
            move |db| db::threads::bump_message_counters(db, &thread_id, token_usage)
        })
        .await
        {
            log::warn!("failed to bump thread counters: {error}");
        }
    }

    let _ =
        maybe_update_thread_title(&state, &thread, &engine_thread_id, &turn_input.message).await;

    let latest_thread = run_db(state.db.clone(), {
        let thread_id = thread.id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await
    .unwrap_or_else(|error| {
        log::warn!("failed to load thread before final thread-updated emit: {error}");
        None
    });

    let (thread_updated_event, final_thread) = build_final_thread_event(latest_thread, &thread);
    let _ = app.emit("thread-updated", thread_updated_event);
    finish_scheduled_task_run(
        &app,
        &state,
        &assistant_message_id,
        &message_status,
        &blocks,
    )
    .await;
    if let Some(final_thread) = final_thread.as_ref() {
        emit_chat_turn_finished(&app, final_thread, &message_status, &blocks);
    }
}

async fn run_codex_review_turn(
    app: tauri::AppHandle,
    state: AppState,
    source_thread: crate::models::ThreadDto,
    review_thread: crate::models::ThreadDto,
    source_engine_thread_id: String,
    assistant_message_id: String,
    initial_turn_model_id: String,
    target: Value,
    delivery: String,
    cancellation: CancellationToken,
) {
    let max_output_chars = state.config.debug.max_action_output_chars;
    let (event_tx, mut event_rx) = mpsc::channel::<EngineEvent>(ENGINE_EVENT_QUEUE_CAPACITY);
    let (started_tx, started_rx) = oneshot::channel();

    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = match codex
        .execution_context(Some(source_thread.workspace_id.as_str()))
        .await
    {
        Ok(context) => context,
        Err(error) => {
            let _ = event_tx
                .send(EngineEvent::Error {
                    message: format!("读取 Codex 项目失败：{error:#}"),
                    recoverable: false,
                })
                .await;
            return;
        }
    };
    let source_engine_thread_id_for_engine = source_engine_thread_id.clone();
    let target_for_engine = target.clone();
    let delivery_for_engine = delivery.clone();
    let cancellation_for_engine = cancellation.clone();

    let engine_task = tokio::spawn(async move {
        let cli: &dyn CliTool = codex.as_ref();
        cli.start_review(
            &context,
            &source_engine_thread_id_for_engine,
            target_for_engine,
            Some(delivery_for_engine.as_str()),
            event_tx,
            cancellation_for_engine,
            started_tx,
        )
        .await
    });

    let state_for_started = state.clone();
    let app_for_started = app.clone();
    let review_thread_for_started = review_thread.clone();
    let started_task = tokio::spawn(async move {
        let Ok(started) = started_rx.await else {
            return;
        };

        let updated_thread = match run_db(state_for_started.db.clone(), {
            let review_thread_id = review_thread_for_started.id.clone();
            let review_thread_engine_id = review_thread_for_started.engine_thread_id.clone();
            let review_thread_model_id = review_thread_for_started.model_id.clone();
            let review_thread_metadata = review_thread_for_started.engine_metadata.clone();
            let review_thread_status = review_thread_for_started.status.clone();
            let review_thread_title = review_thread_for_started.title.clone();
            let review_thread_workspace_id = review_thread_for_started.workspace_id.clone();
            move |db| {
                if review_thread_engine_id.as_deref() == Some(started.review_thread_id.as_str()) {
                    return db::threads::get_thread(db, &review_thread_id)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "review thread not found after review/start: {review_thread_id}"
                        )
                    });
                }

                db::threads::set_engine_thread_id(db, &review_thread_id, &started.review_thread_id)?;
                let current = db::threads::get_thread(db, &review_thread_id)?.ok_or_else(|| {
                    anyhow::anyhow!("review thread not found after engine thread update")
                })?;
                let metadata = current.engine_metadata.or(review_thread_metadata.clone());
                db::threads::update_thread_runtime_snapshot(
                    db,
                    &review_thread_id,
                    Some(&review_thread_title),
                    Some(review_thread_status.clone()),
                    metadata.as_ref(),
                )?;
                db::threads::get_thread(db, &review_thread_id)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "review thread not found after runtime snapshot update: {review_thread_workspace_id}:{review_thread_model_id}"
                    )
                })
            }
        })
        .await
        {
            Ok(thread) => thread,
            Err(error) => {
                log::warn!("failed to persist codex review thread id: {error}");
                return;
            }
        };

        let _ = app_for_started.emit(
            "thread-updated",
            ThreadUpdatedEvent {
                thread_id: updated_thread.id.clone(),
                workspace_id: updated_thread.workspace_id.clone(),
                thread: Some(updated_thread),
            },
        );
    });

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut action_index: HashMap<String, usize> = HashMap::new();
    let mut approval_index: HashMap<String, usize> = HashMap::new();
    let mut message_status = MessageStatusDto::Streaming;
    let mut thread_status = ThreadStatusDto::Streaming;
    let mut turn_model_id = initial_turn_model_id;
    let mut token_usage: Option<(u64, u64)> = None;
    let mut blocks_dirty = false;
    let mut message_state_dirty = false;
    let mut thread_status_dirty = false;
    let mut turn_model_dirty = false;
    let mut last_persist_at = Instant::now();
    let mut last_blocks_persist_at = Instant::now();
    let mut last_persisted_thread_status = thread_status.clone();
    let stream_event_topic = format!("stream-event-{}", review_thread.id);
    let approval_event_topic = format!("approval-request-{}", review_thread.id);
    let mut pending_event: Option<EngineEvent> = None;

    let initial_turn_started_event = EngineEvent::TurnStarted {
        client_turn_id: None,
        remote_turn_id: None,
    };
    let initial_progress = process_stream_event(
        &app,
        &state,
        &review_thread,
        &assistant_message_id,
        &stream_event_topic,
        &approval_event_topic,
        &initial_turn_started_event,
        &mut blocks,
        &mut action_index,
        &mut approval_index,
        max_output_chars,
    )
    .await;
    let initial_force_persist = apply_stream_progress(
        initial_progress,
        &mut message_status,
        &mut thread_status,
        &mut turn_model_id,
        &mut token_usage,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
    );
    flush_stream_state(
        &state,
        &review_thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        initial_force_persist,
    )
    .await;

    if let Err(error) = started_task.await {
        log::warn!("failed to join codex review start task: {error}");
    }

    loop {
        let incoming_event = if pending_event.is_some() {
            match tokio::time::timeout(
                STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL,
                receive_engine_event_with_diagnostics(&mut event_rx, "review_coalesce"),
            )
            .await
            {
                Ok(event) => event,
                Err(_) => {
                    if let Some(event) = pending_event.take() {
                        let progress = process_stream_event(
                            &app,
                            &state,
                            &review_thread,
                            &assistant_message_id,
                            &stream_event_topic,
                            &approval_event_topic,
                            &event,
                            &mut blocks,
                            &mut action_index,
                            &mut approval_index,
                            max_output_chars,
                        )
                        .await;
                        let force_persist = apply_stream_progress(
                            progress,
                            &mut message_status,
                            &mut thread_status,
                            &mut turn_model_id,
                            &mut token_usage,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                        );
                        flush_stream_state(
                            &state,
                            &review_thread,
                            &assistant_message_id,
                            &blocks,
                            &message_status,
                            &thread_status,
                            &turn_model_id,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                            &mut last_persisted_thread_status,
                            &mut last_persist_at,
                            &mut last_blocks_persist_at,
                            force_persist,
                        )
                        .await;
                    }
                    continue;
                }
            }
        } else {
            receive_engine_event_with_diagnostics(&mut event_rx, "review_direct").await
        };

        let Some(incoming_event) = incoming_event else {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_event_channel_closed",
                "consumer": "review",
                "thread_id": review_thread.id.clone(),
            }))
            .await;
            break;
        };

        let mut current_event = incoming_event;

        loop {
            if let Some(previous_event) = pending_event.take() {
                match try_coalesce_stream_events(previous_event, current_event) {
                    Ok(merged_event) => {
                        if coalesced_event_content_len(&merged_event)
                            >= STREAM_EVENT_COALESCE_MAX_CHARS
                        {
                            let progress = process_stream_event(
                                &app,
                                &state,
                                &review_thread,
                                &assistant_message_id,
                                &stream_event_topic,
                                &approval_event_topic,
                                &merged_event,
                                &mut blocks,
                                &mut action_index,
                                &mut approval_index,
                                max_output_chars,
                            )
                            .await;
                            let force_persist = apply_stream_progress(
                                progress,
                                &mut message_status,
                                &mut thread_status,
                                &mut turn_model_id,
                                &mut token_usage,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                            );
                            flush_stream_state(
                                &state,
                                &review_thread,
                                &assistant_message_id,
                                &blocks,
                                &message_status,
                                &thread_status,
                                &turn_model_id,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                                &mut last_persisted_thread_status,
                                &mut last_persist_at,
                                &mut last_blocks_persist_at,
                                force_persist,
                            )
                            .await;
                        } else {
                            pending_event = Some(merged_event);
                        }
                        break;
                    }
                    Err((unmerged_previous_event, unmerged_current_event)) => {
                        let progress = process_stream_event(
                            &app,
                            &state,
                            &review_thread,
                            &assistant_message_id,
                            &stream_event_topic,
                            &approval_event_topic,
                            &unmerged_previous_event,
                            &mut blocks,
                            &mut action_index,
                            &mut approval_index,
                            max_output_chars,
                        )
                        .await;
                        let force_persist = apply_stream_progress(
                            progress,
                            &mut message_status,
                            &mut thread_status,
                            &mut turn_model_id,
                            &mut token_usage,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                        );
                        flush_stream_state(
                            &state,
                            &review_thread,
                            &assistant_message_id,
                            &blocks,
                            &message_status,
                            &thread_status,
                            &turn_model_id,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                            &mut last_persisted_thread_status,
                            &mut last_persist_at,
                            &mut last_blocks_persist_at,
                            force_persist,
                        )
                        .await;
                        current_event = unmerged_current_event;
                    }
                }
            } else if is_coalescable_stream_event(&current_event) {
                pending_event = Some(current_event);
                break;
            } else {
                let progress = process_stream_event(
                    &app,
                    &state,
                    &review_thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &current_event,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
                )
                .await;
                let force_persist = apply_stream_progress(
                    progress,
                    &mut message_status,
                    &mut thread_status,
                    &mut turn_model_id,
                    &mut token_usage,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                );
                flush_stream_state(
                    &state,
                    &review_thread,
                    &assistant_message_id,
                    &blocks,
                    &message_status,
                    &thread_status,
                    &turn_model_id,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                    &mut last_persisted_thread_status,
                    &mut last_persist_at,
                    &mut last_blocks_persist_at,
                    force_persist,
                )
                .await;
                break;
            }
        }
    }

    if let Some(event) = pending_event.take() {
        let progress = process_stream_event(
            &app,
            &state,
            &review_thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        )
        .await;
        let force_persist = apply_stream_progress(
            progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &review_thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            force_persist,
        )
        .await;
    }

    match engine_task.await {
        Ok(Ok(())) => {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "review",
                "thread_id": review_thread.id.clone(),
                "result": "ok",
            }))
            .await;
        }
        Ok(Err(error)) => {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "review",
                "thread_id": review_thread.id.clone(),
                "result": "error",
                "error": error.to_string(),
            }))
            .await;
            blocks.push(ContentBlock::Error {
                message: format!("Engine error: {error}"),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
            let _ = app.emit(
                &stream_event_topic,
                EngineEvent::Error {
                    message: format!("{error}"),
                    recoverable: false,
                },
            );
        }
        Err(error) => {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": chrono::Utc::now().to_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "review",
                "thread_id": review_thread.id.clone(),
                "result": "join_error",
                "error": error.to_string(),
            }))
            .await;
            blocks.push(ContentBlock::Error {
                message: format!("Engine task join error: {error}"),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
        }
    }

    if cancellation.is_cancelled() && matches!(message_status, MessageStatusDto::Streaming) {
        message_status = MessageStatusDto::Interrupted;
        message_state_dirty = true;
        thread_status = ThreadStatusDto::Idle;
        thread_status_dirty = true;
    }

    flush_stream_state(
        &state,
        &review_thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        true,
    )
    .await;

    state.turns.finish(&source_thread.id).await;
    state.turns.finish(&review_thread.id).await;

    let assistant_message_persisted = match run_db(state.db.clone(), {
        let assistant_message_id = assistant_message_id.clone();
        let message_status = message_status.clone();
        let token_usage = token_usage;
        move |db| {
            db::messages::complete_assistant_message(
                db,
                &assistant_message_id,
                message_status,
                token_usage,
                Some(turn_model_id.as_str()),
            )
        }
    })
    .await
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!("failed to complete review assistant message: {error}");
            false
        }
    };

    if assistant_message_persisted && matches!(message_status, MessageStatusDto::Completed) {
        emit_assistant_message_completed(&app, &state, &review_thread.id, &assistant_message_id)
            .await;
    }

    if matches!(message_status, MessageStatusDto::Completed) {
        if let Err(error) = run_db(state.db.clone(), {
            let thread_id = review_thread.id.clone();
            let token_usage = token_usage;
            move |db| db::threads::bump_message_counters(db, &thread_id, token_usage)
        })
        .await
        {
            log::warn!("failed to bump review thread counters: {error}");
        }
    }

    let latest_review_thread = run_db(state.db.clone(), {
        let review_thread_id = review_thread.id.clone();
        move |db| db::threads::get_thread(db, &review_thread_id)
    })
    .await
    .unwrap_or_else(|error| {
        log::warn!("failed to load review thread before final thread-updated emit: {error}");
        None
    });
    let (thread_updated_event, final_review_thread) =
        build_final_thread_event(latest_review_thread, &review_thread);
    let _ = app.emit("thread-updated", thread_updated_event);
    finish_scheduled_task_run(
        &app,
        &state,
        &assistant_message_id,
        &message_status,
        &blocks,
    )
    .await;
    if let Some(final_review_thread) = final_review_thread.as_ref() {
        emit_chat_turn_finished(&app, final_review_thread, &message_status, &blocks);
    }
}

fn is_coalescable_stream_event(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::TextDelta { .. }
            | EngineEvent::ThinkingDelta { .. }
            | EngineEvent::ActionOutputDelta { .. }
            | EngineEvent::ActionProgressUpdated { .. }
    )
}

fn coalesced_event_content_len(event: &EngineEvent) -> usize {
    match event {
        EngineEvent::TextDelta { content }
        | EngineEvent::ThinkingDelta { content }
        | EngineEvent::ActionOutputDelta { content, .. } => content.len(),
        EngineEvent::ActionProgressUpdated { message, .. } => message.len(),
        _ => 0,
    }
}

fn same_output_stream(left: &OutputStream, right: &OutputStream) -> bool {
    matches!(
        (left, right),
        (OutputStream::Stdout, OutputStream::Stdout)
            | (OutputStream::Stderr, OutputStream::Stderr)
            | (OutputStream::Stdin, OutputStream::Stdin)
    )
}

#[allow(clippy::result_large_err)]
fn try_coalesce_stream_events(
    previous: EngineEvent,
    next: EngineEvent,
) -> Result<EngineEvent, (EngineEvent, EngineEvent)> {
    match (previous, next) {
        (
            EngineEvent::TextDelta { mut content },
            EngineEvent::TextDelta {
                content: next_content,
            },
        ) => {
            content.push_str(&next_content);
            Ok(EngineEvent::TextDelta { content })
        }
        (
            EngineEvent::ThinkingDelta { mut content },
            EngineEvent::ThinkingDelta {
                content: next_content,
            },
        ) => {
            content.push_str(&next_content);
            Ok(EngineEvent::ThinkingDelta { content })
        }
        (
            EngineEvent::ActionOutputDelta {
                action_id,
                stream,
                mut content,
            },
            EngineEvent::ActionOutputDelta {
                action_id: next_action_id,
                stream: next_stream,
                content: next_content,
            },
        ) => {
            if action_id == next_action_id && same_output_stream(&stream, &next_stream) {
                content.push_str(&next_content);
                Ok(EngineEvent::ActionOutputDelta {
                    action_id,
                    stream,
                    content,
                })
            } else {
                Err((
                    EngineEvent::ActionOutputDelta {
                        action_id,
                        stream,
                        content,
                    },
                    EngineEvent::ActionOutputDelta {
                        action_id: next_action_id,
                        stream: next_stream,
                        content: next_content,
                    },
                ))
            }
        }
        (
            EngineEvent::ActionProgressUpdated {
                action_id,
                message: _,
            },
            EngineEvent::ActionProgressUpdated {
                action_id: next_action_id,
                message: next_message,
            },
        ) if action_id == next_action_id => Ok(EngineEvent::ActionProgressUpdated {
            action_id,
            message: next_message,
        }),
        (previous, next) => Err((previous, next)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_stream_event(
    app: &tauri::AppHandle,
    state: &AppState,
    thread: &ThreadDto,
    assistant_message_id: &str,
    stream_event_topic: &str,
    approval_event_topic: &str,
    event: &EngineEvent,
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    max_output_chars: usize,
) -> EventProgress {
    let process_sequence = NEXT_STREAM_PROCESS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let processing_started_at = Instant::now();
    log::info!(
        "chat process_stream_event entered: thread_id={}, engine_id={}, assistant_message_id={}, event={event:?}",
        thread.id,
        thread.engine_id,
        assistant_message_id,
    );
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_event_processing_start",
        "process_sequence": process_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id,
        "event_kind": crate::engines::codex::engine_event_kind(event),
    }))
    .await;
    let mut normalized_event = event.clone();
    match &mut normalized_event {
        EngineEvent::ActionOutputDelta { content, .. } => {
            *content = trim_action_output_delta_content(content);
        }
        EngineEvent::ActionCompleted { result, .. } => {
            truncate_action_result_output(result, max_output_chars);
        }
        EngineEvent::DiffUpdated { diff, .. } => {
            *diff = truncate_chars(diff, STREAMED_DIFF_MAX_CHARS);
        }
        _ => {}
    }

    let _ = app.emit(stream_event_topic, &normalized_event);
    if let EngineEvent::ApprovalRequested { summary, .. } = &normalized_event {
        let _ = app.emit(approval_event_topic, &normalized_event);
        // Unscoped companion event so background threads can surface pending
        // approvals without a per-thread listener.
        let _ = app.emit(
            "chat-approval-requested",
            ChatApprovalRequestedEvent {
                thread_id: thread.id.clone(),
                workspace_id: thread.workspace_id.clone(),
                engine_id: thread.engine_id.clone(),
                thread_title: thread.title.clone(),
                summary: summary.clone(),
            },
        );
        match run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            move |db| {
                db::scheduled_tasks::mark_run_needs_confirmation_by_message(
                    db,
                    &assistant_message_id,
                )
            }
        })
        .await
        {
            Ok(Some(task_id)) => {
                crate::scheduled_tasks::manager::emit_task_updated(app, &task_id);
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("failed to mark scheduled task as needing confirmation: {error}");
            }
        }
    }

    if state.config.debug.persist_engine_event_logs {
        let log_event = engine_event_for_debug_log(&normalized_event);
        if let Ok(value) = serde_json::to_value(&log_event) {
            if let Err(error) = run_db(state.db.clone(), {
                let thread_id = thread.id.clone();
                let assistant_message_id = assistant_message_id.to_string();
                let value = value.clone();
                move |db| {
                    db::actions::append_event_log(db, &thread_id, &assistant_message_id, &value)
                }
            })
            .await
            {
                log::warn!("failed to append engine event log: {error}");
            }
        }
    }

    match &normalized_event {
        EngineEvent::TurnStarted {
            remote_turn_id: Some(remote_turn_id),
            ..
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let assistant_message_id = assistant_message_id.to_string();
                let remote_turn_id = remote_turn_id.clone();
                move |db| {
                    db::messages::bind_local_turn_to_remote_turn(
                        db,
                        &assistant_message_id,
                        &remote_turn_id,
                    )
                }
            })
            .await
            {
                log::warn!("failed to bind local turn to remote turn id: {error}");
            }
        }
        EngineEvent::ActionStarted {
            action_id,
            engine_action_id,
            action_type,
            summary,
            details,
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let action_id = action_id.clone();
                let thread_id = thread.id.clone();
                let assistant_message_id = assistant_message_id.to_string();
                let engine_action_id = engine_action_id.clone();
                let action_type = action_type.clone();
                let summary = summary.clone();
                let details = details.clone();
                move |db| {
                    db::actions::insert_action_started(
                        db,
                        &action_id,
                        &thread_id,
                        &assistant_message_id,
                        engine_action_id.as_deref(),
                        &action_type,
                        &summary,
                        &details,
                    )
                }
            })
            .await
            {
                log::warn!("failed to persist action start: {error}");
            }
        }
        EngineEvent::ActionCompleted { action_id, result } => {
            if let Err(error) = run_db(state.db.clone(), {
                let action_id = action_id.clone();
                let result = result.clone();
                move |db| db::actions::update_action_completed(db, &action_id, &result)
            })
            .await
            {
                log::warn!("failed to persist action completion: {error}");
            }
        }
        EngineEvent::ApprovalRequested {
            approval_id,
            action_type,
            summary,
            details,
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let approval_id = approval_id.clone();
                let thread_id = thread.id.clone();
                let assistant_message_id = assistant_message_id.to_string();
                let action_type = action_type.clone();
                let summary = summary.clone();
                let details = details.clone();
                move |db| {
                    db::actions::insert_approval(
                        db,
                        &approval_id,
                        &thread_id,
                        &assistant_message_id,
                        &action_type,
                        &summary,
                        &details,
                    )
                }
            })
            .await
            {
                log::warn!("failed to persist approval: {error}");
            }
        }
        _ => {}
    }

    let progress = apply_event_to_blocks(
        blocks,
        action_index,
        approval_index,
        &normalized_event,
        max_output_chars,
    );
    if !progress.finalized_actions.is_empty() {
        if let Err(error) = run_db(state.db.clone(), {
            let finalized_actions = progress.finalized_actions.clone();
            move |db| {
                for (action_id, result) in finalized_actions {
                    db::actions::update_action_completed(db, &action_id, &result)?;
                }
                Ok(())
            }
        })
        .await
        {
            log::warn!("failed to persist action finalization after turn completion: {error}");
        }
    }
    let processing_ms = processing_started_at.elapsed().as_millis();
    let record = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_event_processing_complete",
        "process_sequence": process_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id.to_string(),
        "event_kind": crate::engines::codex::engine_event_kind(&normalized_event),
        "processing_ms": processing_ms,
        "persist_engine_event_logs": state.config.debug.persist_engine_event_logs,
        "blocks_changed": progress.blocks_changed,
        "force_persist": progress.force_persist,
        "message_status_changed": progress.message_status.is_some(),
        "thread_status_changed": progress.thread_status.is_some(),
        "token_usage_changed": progress.token_usage.is_some(),
        "turn_model_changed": progress.turn_model_id.is_some(),
    });
    if processing_ms >= 25 {
        log::warn!("engine event processing: {record}");
    } else {
        log::debug!("engine event processing: {record}");
    }
    crate::engines::codex::append_codex_transport_log(&record).await;

    progress
}

#[allow(clippy::too_many_arguments)]
fn apply_stream_progress(
    progress: EventProgress,
    message_status: &mut MessageStatusDto,
    thread_status: &mut ThreadStatusDto,
    turn_model_id: &mut String,
    token_usage: &mut Option<(u64, u64)>,
    blocks_dirty: &mut bool,
    message_state_dirty: &mut bool,
    thread_status_dirty: &mut bool,
    turn_model_dirty: &mut bool,
) -> bool {
    if progress.blocks_changed {
        *blocks_dirty = true;
    }

    if let Some(status) = progress.message_status {
        if *message_status != status {
            *message_status = status;
            *message_state_dirty = true;
        }
    }

    if let Some(status) = progress.thread_status {
        if *thread_status != status {
            *thread_status = status;
            *thread_status_dirty = true;
        }
    }

    if let Some(next_turn_model_id) = progress.turn_model_id {
        if *turn_model_id != next_turn_model_id {
            *turn_model_id = next_turn_model_id;
            *turn_model_dirty = true;
        }
    }

    if let Some(tokens) = progress.token_usage {
        *token_usage = Some(tokens);
    }

    progress.force_persist
}

#[allow(clippy::too_many_arguments)]
async fn flush_stream_state(
    state: &AppState,
    thread: &ThreadDto,
    assistant_message_id: &str,
    blocks: &[ContentBlock],
    message_status: &MessageStatusDto,
    thread_status: &ThreadStatusDto,
    turn_model_id: &str,
    blocks_dirty: &mut bool,
    message_state_dirty: &mut bool,
    thread_status_dirty: &mut bool,
    turn_model_dirty: &mut bool,
    last_persisted_thread_status: &mut ThreadStatusDto,
    last_persist_at: &mut Instant,
    last_blocks_persist_at: &mut Instant,
    force: bool,
) {
    let flush_sequence = NEXT_STREAM_FLUSH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let flush_started_at = Instant::now();
    let blocks_json_for_log = match serde_json::to_string(blocks) {
        Ok(value) => value,
        Err(error) => format!("<blocks serialization failed: {error:#}>"),
    };
    log::info!(
        "chat flush_stream_state entered: thread_id={}, engine_id={}, assistant_message_id={}, blocks_json={}, message_status={message_status:?}, thread_status={thread_status:?}, turn_model_id={}, blocks_dirty={}, message_state_dirty={}, thread_status_dirty={}, turn_model_dirty={}, force={}",
        thread.id,
        thread.engine_id,
        assistant_message_id,
        blocks_json_for_log,
        turn_model_id,
        *blocks_dirty,
        *message_state_dirty,
        *thread_status_dirty,
        *turn_model_dirty,
        force,
    );
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_stream_state_flush_start",
        "flush_sequence": flush_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id,
        "force": force,
        "blocks_dirty": *blocks_dirty,
        "message_state_dirty": *message_state_dirty,
        "thread_status_dirty": *thread_status_dirty,
        "turn_model_dirty": *turn_model_dirty,
    }))
    .await;
    if !*blocks_dirty && !*message_state_dirty && !*thread_status_dirty && !*turn_model_dirty {
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "event": "engine_stream_state_flush_complete",
            "flush_sequence": flush_sequence,
            "thread_id": thread.id.clone(),
            "message_id": assistant_message_id,
            "result": "no_dirty_state",
            "flush_ms": flush_started_at.elapsed().as_millis(),
        }))
        .await;
        return;
    }

    let now = Instant::now();

    if *thread_status_dirty && *last_persisted_thread_status == *thread_status {
        *thread_status_dirty = false;
    }

    let should_flush_state =
        force || now.duration_since(*last_persist_at) >= STREAM_DB_FLUSH_INTERVAL;
    let should_flush_blocks =
        force || now.duration_since(*last_blocks_persist_at) >= STREAM_DB_BLOCKS_FLUSH_INTERVAL;

    if !should_flush_blocks && !should_flush_state {
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "event": "engine_stream_state_flush_complete",
            "flush_sequence": flush_sequence,
            "thread_id": thread.id.clone(),
            "message_id": assistant_message_id,
            "result": "deferred_by_interval",
            "flush_ms": flush_started_at.elapsed().as_millis(),
            "should_flush_state": should_flush_state,
            "should_flush_blocks": should_flush_blocks,
        }))
        .await;
        return;
    }

    let mut did_flush_state = false;
    let mut did_flush_blocks = false;

    if *blocks_dirty && should_flush_blocks {
        match serde_json::to_string(blocks) {
            Ok(blocks_json) => {
                log::info!(
                    "chat flush_stream_state blocks serialized before DB write: thread_id={}, assistant_message_id={}, blocks_json={}",
                    thread.id,
                    assistant_message_id,
                    blocks_json,
                );
                let persist_result = run_db(state.db.clone(), {
                    let assistant_message_id = assistant_message_id.to_string();
                    let message_status = message_status.clone();
                    let turn_model_id = turn_model_id.to_string();
                    move |db| {
                        db::messages::update_assistant_blocks_json(
                            db,
                            &assistant_message_id,
                            &blocks_json,
                            message_status,
                            Some(turn_model_id.as_str()),
                        )
                    }
                })
                .await;
                log::info!(
                    "chat flush_stream_state blocks DB write result: thread_id={}, assistant_message_id={}, result={persist_result:?}",
                    thread.id,
                    assistant_message_id,
                );
                if let Err(error) = persist_result {
                    log::warn!("failed to persist assistant stream blocks: {error}");
                } else {
                    *blocks_dirty = false;
                    *message_state_dirty = false;
                    *turn_model_dirty = false;
                    did_flush_blocks = true;
                    did_flush_state = true;
                }
            }
            Err(error) => {
                log::warn!("failed to serialize assistant stream blocks: {error}");
            }
        }
    } else if *message_state_dirty && should_flush_state {
        let persist_result = run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            let message_status = message_status.clone();
            move |db| {
                db::messages::update_assistant_status(db, &assistant_message_id, message_status)
            }
        })
        .await;
        log::info!(
            "chat flush_stream_state message status DB write result: thread_id={}, assistant_message_id={}, message_status={message_status:?}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist assistant stream status: {error}");
        } else {
            *message_state_dirty = false;
            did_flush_state = true;
        }
    }

    if *turn_model_dirty && should_flush_state {
        let persist_result = run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            let turn_model_id = turn_model_id.to_string();
            move |db| {
                db::messages::update_assistant_turn_model_id(
                    db,
                    &assistant_message_id,
                    &turn_model_id,
                )
            }
        })
        .await;
        log::info!(
            "chat flush_stream_state model DB write result: thread_id={}, assistant_message_id={}, turn_model_id={}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
            turn_model_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist assistant turn model id during stream: {error}");
        } else {
            *turn_model_dirty = false;
            did_flush_state = true;
        }
    }

    if *thread_status_dirty && should_flush_state && *last_persisted_thread_status != *thread_status
    {
        let persist_result = run_db(state.db.clone(), {
            let thread_id = thread.id.clone();
            let thread_status = thread_status.clone();
            move |db| db::threads::update_thread_status(db, &thread_id, thread_status)
        })
        .await;
        log::info!(
            "chat flush_stream_state thread status DB write result: thread_id={}, assistant_message_id={}, thread_status={thread_status:?}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist thread status during stream: {error}");
        } else {
            *last_persisted_thread_status = thread_status.clone();
            *thread_status_dirty = false;
            did_flush_state = true;
        }
    }

    if did_flush_blocks {
        *last_blocks_persist_at = now;
    }
    if did_flush_state {
        *last_persist_at = now;
    }

    let flush_ms = flush_started_at.elapsed().as_millis();
    let record = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "event": "engine_stream_state_flush_complete",
        "flush_sequence": flush_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id.to_string(),
        "flush_ms": flush_ms,
        "did_flush_state": did_flush_state,
        "did_flush_blocks": did_flush_blocks,
        "force": force,
        "should_flush_state": should_flush_state,
        "should_flush_blocks": should_flush_blocks,
        "blocks_dirty_after": *blocks_dirty,
        "message_state_dirty_after": *message_state_dirty,
        "thread_status_dirty_after": *thread_status_dirty,
        "turn_model_dirty_after": *turn_model_dirty,
    });
    if flush_ms >= 25 {
        log::warn!("engine stream state flush: {record}");
    } else {
        log::debug!("engine stream state flush: {record}");
    }
    crate::engines::codex::append_codex_transport_log(&record).await;
}

async fn maybe_update_thread_title(
    state: &AppState,
    thread: &ThreadDto,
    engine_thread_id: &str,
    user_message: &str,
) -> Option<ThreadDto> {
    if !should_autotitle_thread(thread) {
        return None;
    }

    let codex_target = if thread.engine_id == "codex" {
        let db = state.db.clone();
        let workspace_id = thread.workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            db::workspaces::find_workspace_by_id(&db, &workspace_id)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        workspace.and_then(|workspace| {
            CliExecutionContext::from_workspace(&workspace)
                .ok()
                .map(|context| {
                    (
                        CliToolFactory::new(state.clone())
                            .create("codex")
                            .expect("Codex CLI factory mapping must exist"),
                        context,
                    )
                })
        })
    } else {
        None
    };
    let claude_target = if thread.engine_id == "claude" {
        let db = state.db.clone();
        let workspace_id = thread.workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            db::workspaces::find_workspace_by_id(&db, &workspace_id)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        workspace.and_then(|workspace| {
            CliExecutionContext::from_workspace(&workspace)
                .ok()
                .map(|context| {
                    (
                        CliToolFactory::new(state.clone())
                            .create("claude")
                            .expect("Claude CLI factory mapping must exist"),
                        context,
                    )
                })
        })
    } else {
        None
    };
    let preview = if let Some((codex, context)) = codex_target.as_ref() {
        let cli: &dyn CliTool = codex.as_ref();
        cli.read_thread_preview(context, thread, engine_thread_id)
            .await
            .ok()
            .flatten()
    } else if let Some((claude, context)) = claude_target.as_ref() {
        let cli: &dyn CliTool = claude.as_ref();
        cli.read_thread_preview(context, thread, engine_thread_id)
            .await
            .ok()
            .flatten()
    } else {
        state
            .engines
            .read_thread_preview(thread, engine_thread_id)
            .await
    };
    let candidate = preview
        .as_deref()
        .and_then(normalize_thread_title)
        .or_else(|| normalize_thread_title(user_message))?;

    if candidate == thread.title {
        return None;
    }

    let updated_thread = match run_db(state.db.clone(), {
        let thread_id = thread.id.clone();
        let candidate = candidate.clone();
        move |db| {
            db::threads::update_thread_title(db, &thread_id, &candidate)?;
            db::threads::get_thread(db, &thread_id)?
                .ok_or_else(|| anyhow::anyhow!("thread not found after title update: {thread_id}"))
        }
    })
    .await
    {
        Ok(updated_thread) => updated_thread,
        Err(error) => {
            log::warn!("failed to update thread title: {error}");
            return None;
        }
    };

    let set_name_result = if let Some((codex, context)) = codex_target.as_ref() {
        let cli: &dyn CliTool = codex.as_ref();
        cli.set_thread_name(context, thread, engine_thread_id, &candidate)
            .await
    } else if let Some((claude, context)) = claude_target.as_ref() {
        let cli: &dyn CliTool = claude.as_ref();
        cli.set_thread_name(context, thread, engine_thread_id, &candidate)
            .await
    } else {
        state
            .engines
            .set_thread_name(thread, engine_thread_id, &candidate)
            .await
    };
    if let Err(error) = set_name_result {
        log::debug!("failed to sync thread name with engine: {error}");
    }

    Some(updated_thread)
}

fn should_autotitle_thread(thread: &ThreadDto) -> bool {
    thread.message_count == 0 && !thread_manual_title_locked(thread.engine_metadata.as_ref())
}

fn thread_manual_title_locked(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(|value| value.get("manualTitle"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn normalize_thread_title(raw: &str) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = compact.trim_matches(|c| c == '"' || c == '\'').to_string();
    if title.is_empty() {
        return None;
    }

    if title.chars().count() > MAX_THREAD_TITLE_CHARS {
        title = truncate_title(title, MAX_THREAD_TITLE_CHARS);
    }

    Some(title)
}

fn truncate_title(value: String, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value;
    }

    if max_chars <= 3 {
        return value.chars().take(max_chars).collect::<String>();
    }

    let mut output = value.chars().take(max_chars - 3).collect::<String>();
    output.push_str("...");
    output
}

fn normalize_chat_notification_preview(raw: &str) -> Option<String> {
    let compact = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(truncate_title(
        trimmed.to_string(),
        MAX_CHAT_NOTIFICATION_PREVIEW_CHARS,
    ))
}

fn chat_notification_preview(blocks: &[ContentBlock]) -> Option<String> {
    for block in blocks {
        match block {
            ContentBlock::Text {
                is_steer: Some(true),
                ..
            } => {}
            ContentBlock::Text { content, .. }
            | ContentBlock::Thinking { content, .. }
            | ContentBlock::Error { message: content } => {
                if let Some(preview) = normalize_chat_notification_preview(content) {
                    return Some(preview);
                }
            }
            ContentBlock::Notice { message, title, .. } => {
                if let Some(preview) = normalize_chat_notification_preview(message) {
                    return Some(preview);
                }
                if let Some(preview) = normalize_chat_notification_preview(title) {
                    return Some(preview);
                }
            }
            _ => {}
        }
    }

    None
}

async fn finish_scheduled_task_run(
    app: &tauri::AppHandle,
    state: &AppState,
    assistant_message_id: &str,
    status: &MessageStatusDto,
    blocks: &[ContentBlock],
) {
    let run_status = match status {
        MessageStatusDto::Completed | MessageStatusDto::Streaming => "completed",
        MessageStatusDto::Interrupted => "interrupted",
        MessageStatusDto::Error => "error",
    };
    let preview = chat_notification_preview(blocks);
    let error_message = matches!(status, MessageStatusDto::Error)
        .then_some("The scheduled task turn ended with an error.");
    match run_db(state.db.clone(), {
        let assistant_message_id = assistant_message_id.to_string();
        let preview = preview.clone();
        move |db| {
            db::scheduled_tasks::finish_run_by_message(
                db,
                &assistant_message_id,
                run_status,
                preview.as_deref(),
                error_message,
            )
        }
    })
    .await
    {
        Ok(Some(task_id)) => {
            crate::scheduled_tasks::manager::emit_task_updated(app, &task_id);
            state.scheduled_tasks.wake();
        }
        Ok(None) => {}
        Err(error) => log::warn!("failed to finish scheduled task run: {error}"),
    }
}

fn emit_chat_turn_finished(
    app: &tauri::AppHandle,
    thread: &ThreadDto,
    status: &MessageStatusDto,
    blocks: &[ContentBlock],
) {
    let event = ChatTurnFinishedEvent {
        thread_id: thread.id.clone(),
        workspace_id: thread.workspace_id.clone(),
        engine_id: thread.engine_id.clone(),
        thread_title: thread.title.clone(),
        status: match status {
            MessageStatusDto::Completed => "completed",
            MessageStatusDto::Interrupted => "interrupted",
            MessageStatusDto::Error => "error",
            MessageStatusDto::Streaming => "completed",
        }
        .to_string(),
        preview: chat_notification_preview(blocks),
    };
    let _ = app.emit("chat-turn-finished", event);
}

fn build_final_thread_event(
    latest_thread: Option<ThreadDto>,
    fallback_thread: &ThreadDto,
) -> (ThreadUpdatedEvent, Option<ThreadDto>) {
    match latest_thread {
        Some(latest_thread) => (
            ThreadUpdatedEvent {
                thread_id: latest_thread.id.clone(),
                workspace_id: latest_thread.workspace_id.clone(),
                thread: Some(latest_thread.clone()),
            },
            Some(latest_thread),
        ),
        None => (
            ThreadUpdatedEvent {
                thread_id: fallback_thread.id.clone(),
                workspace_id: fallback_thread.workspace_id.clone(),
                thread: None,
            },
            None,
        ),
    }
}

fn apply_event_to_blocks(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    event: &EngineEvent,
    max_output_chars: usize,
) -> EventProgress {
    let mut progress = EventProgress::default();

    match event {
        EngineEvent::TurnStarted { .. } => {
            progress.thread_status = Some(ThreadStatusDto::Streaming);
        }
        EngineEvent::TurnCompleted {
            token_usage,
            status,
        } => {
            progress.force_persist = true;
            match status {
                TurnCompletionStatus::Completed => {
                    progress.message_status = Some(MessageStatusDto::Completed);
                    progress.thread_status = Some(ThreadStatusDto::Completed);
                }
                TurnCompletionStatus::Interrupted => {
                    progress.message_status = Some(MessageStatusDto::Interrupted);
                    progress.thread_status = Some(ThreadStatusDto::Idle);
                }
                TurnCompletionStatus::Failed => {
                    progress.message_status = Some(MessageStatusDto::Error);
                    progress.thread_status = Some(ThreadStatusDto::Error);
                }
            }
            progress.token_usage = token_usage
                .as_ref()
                .map(|usage| (usage.input, usage.output));

            let action_result = match status {
                TurnCompletionStatus::Completed => ActionResult {
                    success: true,
                    output: None,
                    error: None,
                    diff: None,
                    duration_ms: 0,
                },
                TurnCompletionStatus::Interrupted => ActionResult {
                    success: false,
                    output: None,
                    error: Some(
                        "Action did not report completion before the turn was interrupted."
                            .to_string(),
                    ),
                    diff: None,
                    duration_ms: 0,
                },
                TurnCompletionStatus::Failed => ActionResult {
                    success: false,
                    output: None,
                    error: Some(
                        "Action did not report completion because the turn failed.".to_string(),
                    ),
                    diff: None,
                    duration_ms: 0,
                },
            };

            for block in blocks.iter_mut() {
                let ContentBlock::Action {
                    action_id,
                    status,
                    result,
                    ..
                } = block
                else {
                    continue;
                };
                if status != "running" && status != "pending" {
                    continue;
                }

                *status = if action_result.success {
                    "done"
                } else {
                    "error"
                }
                .to_string();
                *result = (!action_result.success).then(|| ActionBlockResult {
                    success: action_result.success,
                    output: action_result.output.clone(),
                    error: action_result.error.clone(),
                    diff: action_result.diff.clone(),
                    duration_ms: action_result.duration_ms,
                });
                progress
                    .finalized_actions
                    .push((action_id.clone(), action_result.clone()));
                progress.blocks_changed = true;
            }

            for block in blocks.iter_mut() {
                let ContentBlock::Steer {
                    delivery_status, ..
                } = block
                else {
                    continue;
                };
                if delivery_status == "failed" || delivery_status == "settled" {
                    continue;
                }
                *delivery_status = "settled".to_string();
                progress.blocks_changed = true;
            }
        }
        EngineEvent::TextDelta { content } => {
            progress.blocks_changed = append_text_delta(blocks, content);
        }
        EngineEvent::ThinkingDelta { content } => {
            progress.blocks_changed = append_thinking_delta(blocks, content);
        }
        EngineEvent::SteerApplied {
            client_steer_id,
            content,
            plan_mode,
            attachments,
            input_items,
        } => {
            let attachment_blocks = attachments
                .iter()
                .map(|attachment| ContentBlock::Attachment {
                    file_name: attachment.file_name.clone(),
                    file_path: attachment.file_path.clone(),
                    preview_file_path: attachment.preview_file_path.clone(),
                    size_bytes: attachment.size_bytes,
                    mime_type: attachment.mime_type.clone(),
                    browser_annotation: attachment.browser_annotation.clone(),
                    is_remote: attachment.is_remote,
                })
                .collect();
            let mut skill_blocks = Vec::new();
            let mut mention_blocks = Vec::new();
            for item in input_items {
                match item {
                    TurnInputItem::Skill { name, path } => skill_blocks.push(ContentBlock::Skill {
                        name: name.clone(),
                        path: path.clone(),
                    }),
                    TurnInputItem::Mention { name, path } => {
                        mention_blocks.push(ContentBlock::Mention {
                            name: name.clone(),
                            path: path.clone(),
                        })
                    }
                    TurnInputItem::Text { .. } => {}
                }
            }
            blocks.push(ContentBlock::Steer {
                steer_id: client_steer_id.clone(),
                content: content.clone(),
                delivery_status: "applied".to_string(),
                plan_mode: plan_mode.then_some(true),
                attachments: attachment_blocks,
                skills: skill_blocks,
                mentions: mention_blocks,
            });
            progress.blocks_changed = true;
            progress.force_persist = true;
        }
        EngineEvent::ActionStarted {
            action_id,
            engine_action_id,
            action_type,
            summary,
            details,
        } => {
            let block = ContentBlock::Action {
                action_id: action_id.to_string(),
                engine_action_id: engine_action_id.clone(),
                action_type: action_type.as_str().to_string(),
                summary: summary.to_string(),
                details: value_to_raw(details),
                background_task_id: None,
                output_chunks: Vec::new(),
                status: "running".to_string(),
                result: None,
            };
            progress.blocks_changed = upsert_action_block(blocks, action_index, action_id, block);
        }
        EngineEvent::ActionOutputDelta {
            action_id,
            stream,
            content,
        } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    output_chunks,
                    details,
                    ..
                }) = blocks.get_mut(index)
                {
                    let stream_name = match stream {
                        OutputStream::Stdout => "stdout",
                        OutputStream::Stderr => "stderr",
                        OutputStream::Stdin => "stdin",
                    };
                    let chunk_content = truncate_chars(content, max_output_chars);
                    if chunk_content.is_empty() {
                        return progress;
                    }

                    if let Some(previous_chunk) = output_chunks.last_mut() {
                        if previous_chunk.stream == stream_name {
                            previous_chunk.content.push_str(&chunk_content);
                        } else {
                            output_chunks.push(ActionOutputChunk {
                                stream: stream_name.to_string(),
                                content: chunk_content,
                            });
                        }
                    } else {
                        output_chunks.push(ActionOutputChunk {
                            stream: stream_name.to_string(),
                            content: chunk_content,
                        });
                    }

                    if trim_action_output_chunks(output_chunks, max_output_chars) {
                        mark_output_truncated(details);
                    }
                    progress.blocks_changed = true;
                }
            }
        }
        EngineEvent::ActionProgressUpdated { action_id, message } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action { details, .. }) = blocks.get_mut(index) {
                    progress.blocks_changed = update_action_progress(details, message);
                }
            }
        }
        EngineEvent::ActionBackgroundTaskAssigned { action_id, task_id } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    background_task_id,
                    ..
                }) = blocks.get_mut(index)
                {
                    if background_task_id.as_deref() != Some(task_id.as_str()) {
                        *background_task_id = Some(task_id.clone());
                        progress.blocks_changed = true;
                    }
                }
            }
        }
        EngineEvent::ActionCompleted { action_id, result } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    status,
                    result: block_result,
                    ..
                }) = blocks.get_mut(index)
                {
                    *status = if result.success { "done" } else { "error" }.to_string();
                    *block_result = Some(ActionBlockResult {
                        success: result.success,
                        output: result.output.clone(),
                        error: result.error.clone(),
                        diff: result.diff.clone(),
                        duration_ms: result.duration_ms,
                    });
                    progress.blocks_changed = true;
                }
            }
        }
        EngineEvent::DiffUpdated { diff, scope } => {
            let scope = match scope {
                crate::engines::DiffScope::Turn => "turn",
                crate::engines::DiffScope::File => "file",
                crate::engines::DiffScope::Workspace => "workspace",
            }
            .to_string();

            let latest_matching_index =
                blocks.iter().enumerate().rev().find_map(|(index, block)| {
                    if let ContentBlock::Diff {
                        scope: block_scope, ..
                    } = block
                    {
                        if block_scope == &scope {
                            return Some(index);
                        }
                    }
                    None
                });

            if let Some(latest_matching_index) = latest_matching_index {
                let mut next_blocks = Vec::with_capacity(blocks.len());
                for (index, mut block) in blocks.drain(..).enumerate() {
                    if let ContentBlock::Diff {
                        diff: block_diff,
                        scope: block_scope,
                    } = &mut block
                    {
                        if block_scope == &scope {
                            if index == latest_matching_index {
                                if block_diff != diff {
                                    *block_diff = diff.to_string();
                                }
                                next_blocks.push(block);
                            }
                            continue;
                        }
                    }
                    next_blocks.push(block);
                }
                *blocks = next_blocks;
            } else {
                blocks.push(ContentBlock::Diff {
                    diff: diff.to_string(),
                    scope,
                });
            }
            progress.blocks_changed = true;
        }
        EngineEvent::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => {
            let block = ContentBlock::Notice {
                kind: "model_rerouted".to_string(),
                level: "info".to_string(),
                title: "Model rerouted".to_string(),
                message: format_model_reroute_notice(from_model, to_model, reason),
                // 模型切换提示不附带结构化后台任务元数据。
                metadata: None,
            };
            progress.blocks_changed = upsert_notice_block(
                blocks,
                action_index,
                approval_index,
                "model_rerouted",
                block,
            );
            progress.turn_model_id = Some(to_model.to_string());
            progress.force_persist = true;
        }
        EngineEvent::Notice {
            kind,
            level,
            title,
            message,
            metadata,
        } => {
            let block = ContentBlock::Notice {
                kind: kind.to_string(),
                level: level.to_string(),
                title: title.to_string(),
                message: message.to_string(),
                // 保留 Claude 后台任务结构化元数据，供持久化和前端卡片渲染。
                metadata: metadata.as_ref().map(value_to_raw),
            };
            progress.blocks_changed =
                upsert_notice_block(blocks, action_index, approval_index, kind, block);
            progress.force_persist = true;
        }
        EngineEvent::ApprovalRequested {
            approval_id,
            action_type,
            summary,
            details,
        } => {
            let block = ContentBlock::Approval {
                approval_id: approval_id.to_string(),
                action_type: action_type.as_str().to_string(),
                summary: summary.to_string(),
                details: value_to_raw(details),
                status: "pending".to_string(),
                decision: None,
            };
            progress.blocks_changed =
                upsert_approval_block(blocks, approval_index, approval_id, block);
            progress.thread_status = Some(ThreadStatusDto::AwaitingApproval);
            progress.force_persist = true;
        }
        EngineEvent::Error {
            message,
            recoverable,
        } => {
            blocks.push(ContentBlock::Error {
                message: message.to_string(),
            });
            progress.blocks_changed = true;
            if !recoverable {
                progress.message_status = Some(MessageStatusDto::Error);
                progress.thread_status = Some(ThreadStatusDto::Error);
                progress.force_persist = true;
            }
        }
        EngineEvent::UsageLimitsUpdated { .. } => {}
    }

    progress
}

fn append_text_delta(blocks: &mut Vec<ContentBlock>, content: &str) -> bool {
    if content.is_empty() {
        return false;
    }

    if let Some(ContentBlock::Text {
        content: current, ..
    }) = blocks.last_mut()
    {
        current.push_str(content);
        return true;
    }

    blocks.push(ContentBlock::Text {
        content: content.to_string(),
        plan_mode: None,
        is_steer: None,
        client_steer_id: None,
    });
    true
}

fn append_thinking_delta(blocks: &mut Vec<ContentBlock>, content: &str) -> bool {
    if content.is_empty() {
        return false;
    }

    if let Some(ContentBlock::Thinking {
        content: current, ..
    }) = blocks.last_mut()
    {
        current.push_str(content);
        return true;
    }

    blocks.push(ContentBlock::Thinking {
        content: content.to_string(),
        started_at: None,
        duration_ms: None,
    });
    true
}

fn update_action_progress(details: &mut Box<RawValue>, message: &str) -> bool {
    let mut value: Value = serde_json::from_str(details.get())
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));

    let current_message = value
        .get("progressMessage")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let current_kind = value
        .get("progressKind")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    if current_message.as_deref() == Some(message) && current_kind.as_deref() == Some("mcp") {
        return false;
    }

    if !value.is_object() {
        value = Value::Object(serde_json::Map::new());
    }

    if let Some(details_object) = value.as_object_mut() {
        details_object.insert("progressKind".to_string(), Value::String("mcp".to_string()));
        details_object.insert(
            "progressMessage".to_string(),
            Value::String(message.to_string()),
        );
        *details = value_to_raw(&value);
        return true;
    }

    false
}

fn upsert_action_block(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    action_id: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = action_index.get(action_id).copied() {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    let index = blocks.len();
    blocks.push(block);
    action_index.insert(action_id.to_string(), index);
    true
}

fn upsert_approval_block(
    blocks: &mut Vec<ContentBlock>,
    approval_index: &mut HashMap<String, usize>,
    approval_id: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = approval_index.get(approval_id).copied() {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    let index = blocks.len();
    blocks.push(block);
    approval_index.insert(approval_id.to_string(), index);
    true
}

fn upsert_notice_block(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    kind: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = blocks.iter().position(|existing| {
        matches!(
            existing,
            ContentBlock::Notice {
                kind: existing_kind,
                ..
            } if existing_kind == kind
        )
    }) {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    if kind.starts_with("hook_") || kind.starts_with("codex_hook_") {
        blocks.push(block);
        return true;
    }

    blocks.insert(0, block);
    rebuild_block_indexes(blocks, action_index, approval_index);
    true
}

fn rebuild_block_indexes(
    blocks: &[ContentBlock],
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
) {
    action_index.clear();
    approval_index.clear();

    for (index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Action { action_id, .. } => {
                action_index.insert(action_id.clone(), index);
            }
            ContentBlock::Approval { approval_id, .. } => {
                approval_index.insert(approval_id.clone(), index);
            }
            _ => {}
        }
    }
}

fn format_model_reroute_notice(from_model: &str, to_model: &str, reason: &str) -> String {
    format!("Switched from {from_model} to {to_model} ({reason}).")
}

fn trim_action_output_chunks(
    output_chunks: &mut Vec<ActionOutputChunk>,
    max_output_chars: usize,
) -> bool {
    let mut truncated = false;

    if output_chunks.len() > ACTION_OUTPUT_MAX_CHUNKS {
        let overflow = output_chunks.len() - ACTION_OUTPUT_MAX_CHUNKS;
        output_chunks.drain(0..overflow);
        truncated = true;
    }

    let max_chars = max_output_chars.max(1);
    let total_chars: usize = output_chunks.iter().map(|chunk| chunk.content.len()).sum();
    if total_chars <= max_chars {
        return truncated;
    }

    let target_chars = max_chars.saturating_mul(2) / 3;
    let chars_to_trim = total_chars.saturating_sub(target_chars.max(1));
    if chars_to_trim == 0 {
        return truncated;
    }

    let mut remaining_to_trim = chars_to_trim;
    let mut remove_count = 0usize;
    for chunk in output_chunks.iter_mut() {
        if remaining_to_trim == 0 {
            break;
        }
        let chunk_len = chunk.content.len();
        if chunk_len <= remaining_to_trim {
            remaining_to_trim -= chunk_len;
            remove_count += 1;
            continue;
        }

        chunk.content = trim_string_start_bytes(&chunk.content, remaining_to_trim);
        remaining_to_trim = 0;
        truncated = true;
    }

    if remove_count > 0 {
        output_chunks.drain(0..remove_count);
        truncated = true;
    }

    truncated
}

fn engine_event_for_debug_log(event: &EngineEvent) -> EngineEvent {
    match event {
        EngineEvent::ActionOutputDelta {
            action_id,
            stream,
            content,
        } => EngineEvent::ActionOutputDelta {
            action_id: action_id.clone(),
            stream: stream.clone(),
            content: truncate_chars_within_limit(content, ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS),
        },
        _ => event.clone(),
    }
}

fn trim_string_start_bytes(value: &str, bytes_to_trim: usize) -> String {
    if bytes_to_trim == 0 {
        return value.to_string();
    }
    if bytes_to_trim >= value.len() {
        return String::new();
    }

    let start = value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= bytes_to_trim)
        .unwrap_or(value.len());
    value[start..].to_string()
}

fn mark_output_truncated(details: &mut Box<RawValue>) {
    let mut value: Value = serde_json::from_str(details.get())
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));

    if !value.is_object() {
        value = Value::Object(serde_json::Map::new());
    }

    if let Some(details_object) = value.as_object_mut() {
        details_object.insert("outputTruncated".to_string(), Value::Bool(true));
        *details = value_to_raw(&value);
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut output = value.chars().take(max_chars).collect::<String>();
    output.push_str(TRUNCATED_SUFFIX);
    output
}

fn truncate_chars_within_limit(value: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    if max_chars <= TRUNCATED_SUFFIX.len() {
        return value.chars().take(max_chars).collect();
    }

    let mut output = value
        .chars()
        .take(max_chars - TRUNCATED_SUFFIX.len())
        .collect::<String>();
    output.push_str(TRUNCATED_SUFFIX);
    output
}

fn truncate_action_result_output(
    result: &mut crate::engines::events::ActionResult,
    max_chars: usize,
) {
    let Some(output) = result.output.as_ref() else {
        return;
    };

    let truncated = truncate_chars(output, max_chars.max(1));
    if truncated != *output {
        result.output = Some(truncated);
    }
}

fn approval_policy_for_engine_and_trust_level(
    engine_id: &str,
    trust_level: &TrustLevelDto,
) -> &'static str {
    match engine_id {
        "claude" => match trust_level {
            TrustLevelDto::Trusted => "trusted",
            TrustLevelDto::Standard => "standard",
            TrustLevelDto::Restricted => "restricted",
        },
        "opencode" => match trust_level {
            TrustLevelDto::Trusted | TrustLevelDto::Standard => "ask",
            TrustLevelDto::Restricted => "deny",
        },
        _ => match trust_level {
            TrustLevelDto::Trusted => "on-request",
            TrustLevelDto::Standard => "on-request",
            TrustLevelDto::Restricted => "untrusted",
        },
    }
}

fn allow_network_for_trust_level(trust_level: &TrustLevelDto) -> bool {
    matches!(trust_level, TrustLevelDto::Trusted)
}

fn thread_approval_policy_override_value(
    _engine_id: &str,
    _metadata: Option<&Value>,
) -> Result<Option<Value>, String> {
    // 旧 metadata 适配入口保留；运行代码改由 CliTool::runtime_permissions 提供。
    Ok(None)
}

fn thread_allow_network_override(_metadata: Option<&Value>) -> Option<bool> {
    None
}

fn thread_sandbox_mode(_metadata: Option<&Value>) -> Result<Option<String>, String> {
    // 旧 metadata 适配入口保留；运行代码改由 CliTool::runtime_permissions 提供。
    Ok(None)
}

fn unsupported_thread_sandbox_override_for_external_sandbox(
    sandbox_mode: Option<&str>,
    external_sandbox_active: bool,
) -> bool {
    external_sandbox_active && matches!(sandbox_mode, Some("read-only" | "workspace-write"))
}

fn thread_reasoning_effort(_metadata: Option<&Value>) -> Option<String> {
    // 旧 metadata 读取入口保留，运行代码改由 ThreadDto.reasoning_effort 提供。
    None
}

fn thread_last_model_id(_metadata: Option<&Value>) -> Option<String> {
    // 旧 metadata 读取入口保留，运行代码改由 ThreadDto.model_id 提供。
    None
}

fn thread_service_tier(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("serviceTier"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "fast" | "flex"))
        .map(ToOwned::to_owned)
}

fn thread_personality(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("personality"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "none" | "friendly" | "pragmatic"))
        .map(ToOwned::to_owned)
}

fn thread_output_schema(metadata: Option<&Value>) -> Option<Value> {
    metadata
        .and_then(|value| value.get("outputSchema"))
        .cloned()
}

fn thread_permission_profile(_metadata: Option<&Value>) -> Option<Value> {
    None
}

fn thread_approvals_reviewer(_metadata: Option<&Value>) -> Option<String> {
    None
}

fn thread_opencode_agent(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("opencodeAgent"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_reasoning_effort_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

fn resolve_reasoning_effort_for_model(
    model: &EngineModelDto,
    requested_effort: Option<&str>,
) -> Option<String> {
    if model.supported_reasoning_efforts.is_empty() {
        return None;
    }

    let normalized_requested = normalize_reasoning_effort_value(requested_effort);
    if let Some(requested) = normalized_requested.as_ref() {
        if model
            .supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == *requested)
        {
            return Some(requested.clone());
        }
    }

    let normalized_default =
        normalize_reasoning_effort_value(Some(model.default_reasoning_effort.as_str()));
    if let Some(default_effort) = normalized_default.as_ref() {
        if model
            .supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == *default_effort)
        {
            return Some(default_effort.clone());
        }
    }

    model
        .supported_reasoning_efforts
        .iter()
        .map(|option| option.reasoning_effort.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or(normalized_default)
        .or(normalized_requested)
}

fn resolve_reasoning_effort_from_catalog(
    engines: &[EngineInfoDto],
    engine_id: &str,
    model_id: &str,
    requested_effort: Option<&str>,
) -> Option<String> {
    let normalized_requested = normalize_reasoning_effort_value(requested_effort);
    let Some(model) = engines
        .iter()
        .find(|engine| engine.id == engine_id)
        .and_then(|engine| engine.models.iter().find(|model| model.id == model_id))
    else {
        return normalized_requested;
    };

    resolve_reasoning_effort_for_model(model, normalized_requested.as_deref())
}

fn normalize_codex_approval_policy_value(value: &Value) -> Result<Value, String> {
    match value {
        Value::String(raw) => {
            let normalized = raw.trim().to_lowercase();
            let normalized = normalized.as_str();
            if matches!(normalized, "untrusted" | "on-request" | "never") {
                Ok(Value::String(normalized.to_string()))
            } else if normalized == "on-failure" {
                Ok(Value::String("on-request".to_string()))
            } else {
                Err(format!(
                    "invalid approval policy `{normalized}`. expected one of: untrusted, on-request, never"
                ))
            }
        }
        Value::Object(object) => {
            let reject = object
                .get("reject")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    "invalid structured approval policy. expected a `reject` object".to_string()
                })?;

            for required_key in ["mcp_elicitations", "rules", "sandbox_approval"] {
                if !reject.get(required_key).and_then(Value::as_bool).is_some() {
                    return Err(format!(
                        "invalid structured approval policy. missing boolean reject.{required_key}"
                    ));
                }
            }

            if reject.contains_key("request_permissions")
                && reject
                    .get("request_permissions")
                    .and_then(Value::as_bool)
                    .is_none()
            {
                return Err(
                    "invalid structured approval policy. reject.request_permissions must be a boolean"
                        .to_string(),
                );
            }

            Ok(Value::Object(object.clone()))
        }
        _ => Err(
            "invalid approval policy. expected a string mode or structured reject object"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use super::*;
    use crate::{
        config::app_config::AppConfig,
        db,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        models::{EngineCapabilitiesDto, ReasoningEffortOptionDto},
        power::KeepAwakeManager,
        state::{AppState, TurnManager},
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    // Engine trait 已由 use super::* 从父模块带入作用域，此行冗余（保留记录）。
    // use base64::Engine as _;
    use rusqlite::params;
    use uuid::Uuid;

    #[test]
    fn steer_receipt_serializes_with_frontend_field_names() {
        let receipt = SteerReceiptDto {
            client_steer_id: "steer-1".to_string(),
            expected_turn_id: "turn-1".to_string(),
            accepted_at: "2026-08-11T15:26:57.608Z".to_string(),
        };

        assert_eq!(
            serde_json::to_value(receipt).expect("steer receipt should serialize"),
            serde_json::json!({
                "clientSteerId": "steer-1",
                "expectedTurnId": "turn-1",
                "acceptedAt": "2026-08-11T15:26:57.608Z",
            })
        );
    }

    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-chat-cmd-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp root");
        let db = crate::db::Database::open(root.join("workspaces.db"))
            .expect("failed to create test database");
        AppState {
            db: db.clone(),
            config: Arc::new(AppConfig::default()),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            engines: Arc::new(EngineManager::new()),
            git_watchers: Arc::new(GitWatcherManager::default()),
            terminals: Arc::new(TerminalManager::default()),
            notifications: Arc::new(TerminalNotificationManager::default()),
            keep_awake: Arc::new(KeepAwakeManager::new()),
            turns: Arc::new(TurnManager::default()),
            file_tree_cache: Arc::new(FileTreeCache::new()),
            extension_catalog_refreshes: Arc::new(
                crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
            ),
            scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
            computer_control_service: Arc::new(
                crate::computer_control_service::ComputerControlService::default(),
            ),
            auracoder_thread_mcp_service: Arc::new(
                crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()),
            ),
            mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    fn test_thread(state: &AppState, engine_id: &str, model_id: &str) -> ThreadDto {
        let workspace_root =
            std::env::temp_dir().join(format!("auracoder-chat-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&workspace_root).expect("failed to create workspace root");
        let workspace =
            db::workspaces::upsert_workspace(&state.db, workspace_root.to_string_lossy().as_ref())
                .expect("failed to create workspace");
        db::threads::create_thread(&state.db, &workspace.id, engine_id, model_id, "Thread")
            .expect("failed to create thread")
    }

    fn attachment_validation_catalog(attachment_modalities: Vec<&str>) -> Vec<EngineInfoDto> {
        vec![EngineInfoDto {
            id: "opencode".to_string(),
            name: "OpenCode".to_string(),
            models: vec![EngineModelDto {
                id: "opencode/test".to_string(),
                display_name: "OpenCode Test".to_string(),
                description: String::new(),
                hidden: false,
                is_default: true,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string(), "image".to_string(), "pdf".to_string()],
                attachment_modalities: attachment_modalities
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect(),
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: Vec::new(),
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }]
    }

    fn test_attachment(file_name: &str, mime_type: Option<&str>) -> TurnAttachment {
        TurnAttachment {
            file_name: file_name.to_string(),
            file_path: format!("/tmp/{file_name}"),
            preview_file_path: None,
            size_bytes: 1,
            mime_type: mime_type.map(ToOwned::to_owned),
            browser_annotation: None,
            is_remote: false,
            remote_text_content: None,
        }
    }

    #[test]
    fn browser_annotation_comment_is_hidden_from_visible_message_text() {
        let attachment = TurnAttachment {
            file_name: "browser-annotation.png".to_string(),
            file_path: "/tmp/browser-annotation.png".to_string(),
            preview_file_path: None,
            size_bytes: 1,
            mime_type: Some("image/png".to_string()),
            browser_annotation: Some(BrowserAnnotationMetadata {
                comment: "文字太小了".to_string(),
                number: Some(1),
                source_url: Some("https://www.jxrjkf.cn/".to_string()),
                target_label: Some("div: 嘉兴市 雨 25℃".to_string()),
            }),
            is_remote: false,
            remote_text_content: None,
        };

        let blocks = build_user_blocks(
            "请优化这个页面",
            &[TurnInputItem::Text {
                text: "请优化这个页面".to_string(),
            }],
            &[attachment.clone()],
            false,
            false,
            None,
        );
        let visible_text = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(visible_text, "请优化这个页面");
        assert!(!visible_text.contains("文字太小了"));
        assert!(browser_annotation_context(&[attachment]).contains("文字太小了"));
    }

    #[test]
    fn browser_annotation_metadata_requires_a_comment() {
        let annotation = BrowserAnnotationMetadata {
            comment: "   ".to_string(),
            number: Some(1),
            source_url: Some("https://www.jxrjkf.cn/".to_string()),
            target_label: Some("div: 嘉兴市 雨 25℃".to_string()),
        };

        assert!(normalize_browser_annotation(Some(annotation))
            .expect("empty annotation comment should not fail")
            .is_none());
    }

    #[test]
    fn validates_attachments_against_model_attachment_modalities() {
        let catalog = attachment_validation_catalog(vec!["text", "image"]);
        let attachments = vec![
            test_attachment("notes.md", Some("text/markdown")),
            test_attachment("screenshot.png", Some("image/png")),
        ];

        assert!(validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .is_ok());
    }

    #[test]
    fn rejects_attachments_when_model_disables_files() {
        let catalog = attachment_validation_catalog(Vec::new());
        let attachments = vec![test_attachment("notes.md", Some("text/markdown"))];

        let error = validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .expect_err("model without attachment modalities should reject files");

        assert!(error.contains("does not support file attachments"));
    }

    #[test]
    fn rejects_attachment_modalities_not_allowed_by_model() {
        let catalog = attachment_validation_catalog(vec!["text"]);
        let attachments = vec![test_attachment("diagram.png", Some("image/png"))];

        let error = validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .expect_err("image should be blocked for text-only models");

        assert!(error.contains("Image attachments are not supported"));
    }

    fn insert_pending_approval_with_details(
        state: &AppState,
        thread: &ThreadDto,
        approval_id: &str,
        details: Value,
    ) -> String {
        let assistant_message = db::messages::insert_assistant_placeholder(
            &state.db,
            &thread.id,
            Some(thread.engine_id.as_str()),
            Some(thread.model_id.as_str()),
            None,
        )
        .expect("failed to create assistant message");
        db::actions::insert_approval(
            &state.db,
            approval_id,
            &thread.id,
            &assistant_message.id,
            &crate::engines::events::ActionType::Command,
            "Run command",
            &details,
        )
        .expect("failed to insert approval");
        db::threads::update_thread_status(&state.db, &thread.id, ThreadStatusDto::AwaitingApproval)
            .expect("failed to set thread status");

        let blocks = serde_json::json!([
            {
                "type": "approval",
                "approvalId": approval_id,
                "actionType": "command",
                "summary": "Run command",
                "details": details,
                "status": "pending"
            }
        ]);
        let conn = state.db.connect().expect("failed to open db connection");
        conn.execute(
            "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
            params![blocks.to_string(), assistant_message.id],
        )
        .expect("failed to persist approval block");
        assistant_message.id
    }

    fn insert_pending_approval(state: &AppState, thread: &ThreadDto, approval_id: &str) -> String {
        insert_pending_approval_with_details(
            state,
            thread,
            approval_id,
            serde_json::json!({ "command": "touch file.txt" }),
        )
    }

    #[test]
    fn build_final_thread_event_uses_latest_thread_when_present() {
        let state = test_app_state();
        let fallback_thread = test_thread(&state, "codex", "gpt-5.5-codex");
        let mut latest_thread = fallback_thread.clone();
        latest_thread.title = "Renamed".to_string();

        let (event, final_thread) =
            build_final_thread_event(Some(latest_thread.clone()), &fallback_thread);

        assert_eq!(event.thread_id, latest_thread.id);
        assert_eq!(event.workspace_id, latest_thread.workspace_id);
        assert_eq!(
            event.thread.as_ref().map(|thread| thread.title.as_str()),
            Some("Renamed")
        );
        assert_eq!(
            final_thread.as_ref().map(|thread| thread.title.as_str()),
            Some("Renamed")
        );
    }

    #[test]
    fn build_final_thread_event_emits_removal_when_thread_is_missing() {
        let state = test_app_state();
        let fallback_thread = test_thread(&state, "codex", "gpt-5.5-codex");

        let (event, final_thread) = build_final_thread_event(None, &fallback_thread);

        assert_eq!(event.thread_id, fallback_thread.id);
        assert_eq!(event.workspace_id, fallback_thread.workspace_id);
        assert!(event.thread.is_none());
        assert!(final_thread.is_none());
    }

    #[test]
    fn external_sandbox_allows_default_workspace_write_mode() {
        assert!(!unsupported_thread_sandbox_override_for_external_sandbox(
            None, true,
        ));
    }

    #[test]
    fn normalize_input_items_merges_adjacent_text_and_preserves_typed_items() {
        let normalized = normalize_input_items(
            "fallback",
            Some(vec![
                ChatInputItemPayload::Text {
                    text: "Use ".to_string(),
                },
                ChatInputItemPayload::Text {
                    text: "$lint".to_string(),
                },
                ChatInputItemPayload::Skill {
                    name: "lint".to_string(),
                    path: "/tmp/skills/lint".to_string(),
                },
                ChatInputItemPayload::Mention {
                    name: "Docs".to_string(),
                    path: "app://docs".to_string(),
                },
                ChatInputItemPayload::Text {
                    text: " now".to_string(),
                },
            ]),
        )
        .expect("input items should normalize");

        assert_eq!(
            normalized,
            vec![
                TurnInputItem::Text {
                    text: "Use $lint".to_string(),
                },
                TurnInputItem::Skill {
                    name: "lint".to_string(),
                    path: "/tmp/skills/lint".to_string(),
                },
                TurnInputItem::Mention {
                    name: "Docs".to_string(),
                    path: "app://docs".to_string(),
                },
                TurnInputItem::Text {
                    text: " now".to_string(),
                },
            ]
        );
    }

    #[test]
    fn chat_notification_preview_ignores_steer_blocks_and_uses_first_text_block() {
        let preview = chat_notification_preview(&[
            ContentBlock::Text {
                content: "hidden steer".to_string(),
                plan_mode: None,
                is_steer: Some(true),
                client_steer_id: Some("steer-preview".to_string()),
            },
            ContentBlock::Text {
                content: "  First line\n\nSecond line  ".to_string(),
                plan_mode: None,
                is_steer: None,
                client_steer_id: None,
            },
        ]);

        assert_eq!(preview.as_deref(), Some("First line Second line"));
    }

    #[test]
    fn chat_notification_preview_falls_back_to_error_blocks() {
        let preview = chat_notification_preview(&[ContentBlock::Error {
            message: "Command failed".to_string(),
        }]);

        assert_eq!(preview.as_deref(), Some("Command failed"));
    }

    #[test]
    fn normalize_input_items_rejects_blank_typed_entries() {
        let error = normalize_input_items(
            "fallback",
            Some(vec![ChatInputItemPayload::Skill {
                name: " ".to_string(),
                path: "/tmp/skills/lint".to_string(),
            }]),
        )
        .expect_err("blank skill names should be rejected");

        assert!(error.contains("skill input items require non-empty name and path"));
    }

    #[test]
    fn normalize_codex_review_target_builds_commit_payload() {
        let (target, message, title) =
            normalize_codex_review_target(&CodexReviewTargetPayload::Commit {
                sha: "abcdef1234567890".to_string(),
                title: Some("Refactor auth flow".to_string()),
            })
            .expect("commit target should normalize");

        assert_eq!(
            target,
            serde_json::json!({
                "type": "commit",
                "sha": "abcdef1234567890",
                "title": "Refactor auth flow",
            })
        );
        assert_eq!(
            message,
            "Review commit `abcdef1234567890`: Refactor auth flow"
        );
        assert_eq!(title, "Review: abcdef123456");
    }

    #[test]
    fn clone_codex_review_metadata_clears_runtime_only_fields() {
        let metadata = clone_codex_review_metadata(
            Some(&serde_json::json!({
                "manualTitle": true,
                "manualTitleUpdatedAt": "2026-03-12T00:00:00Z",
                "codexPreview": "old preview",
                "codexThreadStatus": "active",
                "codexThreadActiveFlags": ["waitingOnApproval"],
                "codexSyncRequired": true,
                "codexSyncReason": "stale",
                "serviceTier": "fast",
            })),
            "gpt-5.4",
        )
        .expect("metadata should clone");

        assert_eq!(metadata.get("manualTitle"), None);
        assert_eq!(metadata.get("codexPreview"), None);
        assert_eq!(metadata.get("codexThreadStatus"), None);
        assert_eq!(metadata.get("codexThreadActiveFlags"), None);
        assert_eq!(metadata.get("codexSyncRequired"), None);
        assert_eq!(metadata.get("codexSyncReason"), None);
        assert_eq!(
            metadata.get("serviceTier"),
            Some(&serde_json::json!("fast"))
        );
        assert_eq!(metadata.get("lastModelId"), None);
    }

    #[test]
    fn normalize_input_items_rejects_non_empty_message_without_text_segments() {
        let error = normalize_input_items(
            "Use lint",
            Some(vec![ChatInputItemPayload::Skill {
                name: "lint".to_string(),
                path: "/tmp/skills/lint".to_string(),
            }]),
        )
        .expect_err("non-empty message text requires a text segment");

        assert!(error.contains("input items must include at least one text segment"));
    }

    #[test]
    fn external_sandbox_blocks_explicit_workspace_write_override() {
        assert!(unsupported_thread_sandbox_override_for_external_sandbox(
            Some("workspace-write"),
            true,
        ));
        assert!(unsupported_thread_sandbox_override_for_external_sandbox(
            Some("read-only"),
            true,
        ));
        assert!(!unsupported_thread_sandbox_override_for_external_sandbox(
            Some("danger-full-access"),
            true,
        ));
    }

    #[test]
    fn claude_defaults_follow_trust_level_directly() {
        assert_eq!(
            approval_policy_for_engine_and_trust_level("claude", &TrustLevelDto::Trusted),
            "trusted"
        );
        assert_eq!(
            approval_policy_for_engine_and_trust_level("claude", &TrustLevelDto::Standard),
            "standard"
        );
        assert_eq!(
            approval_policy_for_engine_and_trust_level("claude", &TrustLevelDto::Restricted),
            "restricted"
        );
    }

    #[test]
    fn opencode_defaults_use_permission_modes_not_codex_sandbox_policies() {
        assert_eq!(
            approval_policy_for_engine_and_trust_level("opencode", &TrustLevelDto::Trusted),
            "ask"
        );
        assert_eq!(
            approval_policy_for_engine_and_trust_level("opencode", &TrustLevelDto::Standard),
            "ask"
        );
        assert_eq!(
            approval_policy_for_engine_and_trust_level("opencode", &TrustLevelDto::Restricted),
            "deny"
        );
    }

    #[test]
    fn claude_permission_mode_override_uses_claude_key() {
        let metadata = serde_json::json!({
            "claudePermissionMode": "restricted",
            "sandboxApprovalPolicy": "never",
        });

        assert_eq!(
            thread_approval_policy_override_value("claude", Some(&metadata)).unwrap(),
            Some(Value::String("restricted".to_string()))
        );
        assert_eq!(
            thread_approval_policy_override_value("codex", Some(&metadata)).unwrap(),
            Some(Value::String("never".to_string()))
        );
    }

    #[test]
    fn opencode_permission_mode_override_uses_opencode_key() {
        let metadata = serde_json::json!({
            "opencodePermissionMode": "allow",
            "sandboxApprovalPolicy": "never",
        });

        assert_eq!(
            thread_approval_policy_override_value("opencode", Some(&metadata)).unwrap(),
            Some(Value::String("allow".to_string()))
        );
        assert_eq!(
            thread_approval_policy_override_value("codex", Some(&metadata)).unwrap(),
            Some(Value::String("never".to_string()))
        );
    }

    #[test]
    fn legacy_codex_on_failure_override_is_migrated_before_thread_start() {
        let metadata = serde_json::json!({
            "sandboxApprovalPolicy": "on-failure",
        });

        assert_eq!(
            thread_approval_policy_override_value("codex", Some(&metadata)).unwrap(),
            Some(Value::String("on-request".to_string()))
        );
    }

    #[test]
    fn invalid_structured_codex_approval_policy_is_rejected() {
        let metadata = serde_json::json!({
            "sandboxApprovalPolicy": {
                "reject": {
                    "rules": true,
                    "sandbox_approval": false
                }
            }
        });

        let error = thread_approval_policy_override_value("codex", Some(&metadata))
            .expect_err("expected malformed structured approval policy to fail");

        assert!(error.contains("reject.mcp_elicitations"));
    }

    #[test]
    fn action_progress_coalescing_keeps_latest_message() {
        let merged = try_coalesce_stream_events(
            EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Connecting".to_string(),
            },
            EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Fetching results".to_string(),
            },
        )
        .expect("expected coalesced action progress");

        match merged {
            EngineEvent::ActionProgressUpdated { action_id, message } => {
                assert_eq!(action_id, "action-1");
                assert_eq!(message, "Fetching results");
            }
            other => panic!("expected action progress event, got {other:?}"),
        }
    }

    #[test]
    fn debug_event_log_trims_action_output_payload() {
        let content = "x".repeat(ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS + 128);
        let event = EngineEvent::ActionOutputDelta {
            action_id: "action-1".to_string(),
            stream: OutputStream::Stdout,
            content,
        };

        let log_event = engine_event_for_debug_log(&event);

        match log_event {
            EngineEvent::ActionOutputDelta { content, .. } => {
                assert_eq!(content.len(), ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS);
            }
            other => panic!("expected action output event, got {other:?}"),
        }
    }

    #[test]
    fn steer_applied_persists_at_the_exact_stream_boundary_and_settles_on_completion() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "插话之前".to_string(),
            },
            1000,
        );
        let applied = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::SteerApplied {
                client_steer_id: "client-steer-1".to_string(),
                content: "插话内容".to_string(),
                plan_mode: true,
                attachments: Vec::new(),
                input_items: vec![
                    TurnInputItem::Skill {
                        name: "测试技能".to_string(),
                        path: "skill://test".to_string(),
                    },
                    TurnInputItem::Mention {
                        name: "说明".to_string(),
                        path: "app://docs".to_string(),
                    },
                ],
            },
            1000,
        );
        assert!(applied.blocks_changed);
        assert!(applied.force_persist);
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "插话之后".to_string(),
            },
            1000,
        );

        let serialized = serde_json::to_value(&blocks).expect("blocks should serialize");
        assert_eq!(serialized[0]["content"], "插话之前");
        assert_eq!(serialized[1]["type"], "steer");
        assert_eq!(serialized[1]["steerId"], "client-steer-1");
        assert_eq!(serialized[1]["deliveryStatus"], "applied");
        assert_eq!(serialized[1]["planMode"], true);
        assert_eq!(serialized[1]["skills"][0]["name"], "测试技能");
        assert_eq!(serialized[1]["mentions"][0]["name"], "说明");
        assert_eq!(serialized[2]["content"], "插话之后");

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Completed,
            },
            1000,
        );
        let settled = serde_json::to_value(&blocks).expect("settled blocks should serialize");
        assert_eq!(settled[1]["deliveryStatus"], "settled");
    }

    #[test]
    fn trim_action_output_chunks_keeps_tail_of_oversized_chunk() {
        let mut chunks = vec![ActionOutputChunk {
            stream: "stdout".to_string(),
            content: "0123456789".to_string(),
        }];

        assert!(trim_action_output_chunks(&mut chunks, 6));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "6789");
    }

    #[test]
    fn model_reroute_notice_reindexes_action_blocks() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let started = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "action-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Other,
                summary: "search_docs".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        assert!(started.blocks_changed);

        let rerouted = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ModelRerouted {
                from_model: "gpt-5.1-codex-mini".to_string(),
                to_model: "gpt-5.3-codex".to_string(),
                reason: "highRiskCyberActivity".to_string(),
            },
            1000,
        );
        assert!(rerouted.blocks_changed);
        assert_eq!(rerouted.turn_model_id.as_deref(), Some("gpt-5.3-codex"));

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Fetching results".to_string(),
            },
            1000,
        );
        assert!(progress.blocks_changed);

        assert!(matches!(
            &blocks[0],
            ContentBlock::Notice {
                kind,
                level,
                title,
                ..
            } if kind == "model_rerouted" && level == "info" && title == "Model rerouted"
        ));
        match &blocks[1] {
            ContentBlock::Action { details, .. } => {
                let details_value: Value = serde_json::from_str(details.get())
                    .expect("action details should parse as JSON");
                assert_eq!(
                    details_value
                        .get("progressKind")
                        .and_then(serde_json::Value::as_str),
                    Some("mcp")
                );
                assert_eq!(
                    details_value
                        .get("progressMessage")
                        .and_then(serde_json::Value::as_str),
                    Some("Fetching results")
                );
            }
            other => panic!("expected action block, got {other:?}"),
        }
    }

    #[test]
    fn turn_completion_finalizes_unfinished_actions() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let started = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        assert!(started.blocks_changed);

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Completed,
            },
            1000,
        );

        assert!(progress.blocks_changed);
        assert_eq!(progress.finalized_actions.len(), 1);
        assert_eq!(progress.finalized_actions[0].0, "web-search-1");
        assert!(progress.finalized_actions[0].1.success);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: None,
                ..
            } if status == "done"
        ));
    }

    #[test]
    fn interrupted_turn_finalizes_unfinished_actions_as_error() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-interrupted".to_string(),
                engine_action_id: Some("item-interrupted".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Interrupted,
            },
            1000,
        );

        assert_eq!(progress.message_status, Some(MessageStatusDto::Interrupted));
        assert_eq!(progress.thread_status, Some(ThreadStatusDto::Idle));
        assert_eq!(progress.finalized_actions.len(), 1);
        assert!(!progress.finalized_actions[0].1.success);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: Some(result),
                ..
            } if status == "error"
                && !result.success
                && result.error.as_deref()
                    == Some("Action did not report completion before the turn was interrupted.")
        ));
    }

    #[test]
    fn interrupted_turn_does_not_overwrite_an_already_completed_action() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-completed".to_string(),
                engine_action_id: Some("item-completed".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionCompleted {
                action_id: "web-search-completed".to_string(),
                result: ActionResult {
                    success: true,
                    output: Some("done".to_string()),
                    error: None,
                    diff: None,
                    duration_ms: 12,
                },
            },
            1000,
        );

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Interrupted,
            },
            1000,
        );

        assert!(progress.finalized_actions.is_empty());
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: Some(result),
                ..
            } if status == "done" && result.success
        ));
    }

    #[test]
    fn diff_update_collapses_existing_same_scope_diff_blocks() {
        let mut blocks = vec![
            ContentBlock::Diff {
                diff: "old diff 1".to_string(),
                scope: "turn".to_string(),
            },
            ContentBlock::Text {
                content: "kept".to_string(),
                plan_mode: None,
                is_steer: None,
                client_steer_id: None,
            },
            ContentBlock::Diff {
                diff: "old diff 2".to_string(),
                scope: "turn".to_string(),
            },
        ];
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::DiffUpdated {
                diff: "new diff".to_string(),
                scope: crate::engines::DiffScope::Turn,
            },
            1000,
        );

        assert!(progress.blocks_changed);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Text { content, .. } if content == "kept"
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Diff { diff, scope } if diff == "new diff" && scope == "turn"
        ));
    }

    #[test]
    fn image_extension_rejects_unknown_image_mime() {
        assert_eq!(image_extension_for_mime_type("image/heic"), None);
    }

    #[test]
    fn generic_notice_blocks_are_upserted_by_kind() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "Visible assistant text.".to_string(),
            },
            1000,
        );
        let first = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "deprecation_notice".to_string(),
                level: "warning".to_string(),
                title: "Deprecation notice".to_string(),
                message: "Use the newer API.".to_string(),
                // 通用测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        assert!(first.blocks_changed);

        let second = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "deprecation_notice".to_string(),
                level: "warning".to_string(),
                title: "Deprecation notice".to_string(),
                message: "Use the newer permissions API.".to_string(),
                // 通用测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        assert!(second.blocks_changed);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Notice { message, .. } if message == "Use the newer permissions API."
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Text { content, .. } if content == "Visible assistant text."
        ));
    }

    #[test]
    fn hook_notice_blocks_preserve_stream_position() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "before hooks".to_string(),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "hook_started_first".to_string(),
                level: "info".to_string(),
                title: "Hook started".to_string(),
                message: "first hook started".to_string(),
                // Hook 测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "action-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Other,
                summary: "Tool call".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "hook_completed_first".to_string(),
                level: "info".to_string(),
                title: "Hook completed".to_string(),
                message: "first hook completed".to_string(),
                // Hook 测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "after hooks".to_string(),
            },
            1000,
        );

        assert_eq!(blocks.len(), 5);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Text { content, .. } if content == "before hooks"
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Notice { kind, .. } if kind == "hook_started_first"
        ));
        assert!(matches!(
            &blocks[2],
            ContentBlock::Action { action_id, .. } if action_id == "action-1"
        ));
        assert!(matches!(
            &blocks[3],
            ContentBlock::Notice { kind, .. } if kind == "hook_completed_first"
        ));
        assert!(matches!(
            &blocks[4],
            ContentBlock::Text { content, .. } if content == "after hooks"
        ));
        assert_eq!(action_index.get("action-1"), Some(&2));
    }

    #[test]
    fn approval_response_persistence_tracks_permissions_session_scope() {
        let response = serde_json::json!({
            "permissions": {
                "network": {
                    "enabled": true
                }
            },
            "scope": "session"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "accept_for_session"
        );
    }

    #[test]
    fn approval_response_persistence_tracks_mcp_elicitation_actions() {
        let response = serde_json::json!({
            "action": "decline"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "decline"
        );
    }

    #[test]
    fn approval_response_persistence_treats_empty_permissions_as_decline() {
        let response = serde_json::json!({
            "permissions": {},
            "scope": "turn"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "decline"
        );
    }

    #[tokio::test]
    async fn invalid_claude_approval_response_keeps_approval_pending() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");
        let approval_id = "approval-invalid";
        let message_id = insert_pending_approval(&state, &thread, approval_id);

        let error = respond_to_approval_inner(
            &state,
            thread.id.clone(),
            approval_id.to_string(),
            serde_json::json!({}),
        )
        .await
        .expect_err("expected invalid approval payload to fail");

        assert!(error.contains("explicit `decision`"));

        let conn = state.db.connect().expect("failed to open db connection");
        let approval_row = conn
            .query_row(
                "SELECT status, decision FROM approvals WHERE id = ?1",
                params![approval_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("failed to load approval row");
        assert_eq!(approval_row.0, "pending");
        assert_eq!(approval_row.1, None);

        let thread_status = conn
            .query_row(
                "SELECT status FROM threads WHERE id = ?1",
                params![thread.id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load thread status");
        assert_eq!(thread_status, "awaiting_approval");

        let raw_blocks = conn
            .query_row(
                "SELECT blocks_json FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load message blocks");
        let blocks: Value =
            serde_json::from_str(&raw_blocks).expect("message blocks should deserialize");
        assert_eq!(
            blocks
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("status"))
                .and_then(Value::as_str),
            Some("pending")
        );
        assert!(blocks
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("decision"))
            .is_none());
    }

    #[tokio::test]
    async fn missing_live_codex_approval_request_keeps_approval_pending() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.5-codex");
        let approval_id = "approval-reset";
        let message_id = insert_pending_approval_with_details(
            &state,
            &thread,
            approval_id,
            serde_json::json!({
                "command": "touch file.txt",
                "_serverMethod": "item/fileChange/requestApproval",
                "_rawRequestId": 42
            }),
        );

        let error = respond_to_approval_inner(
            &state,
            thread.id.clone(),
            approval_id.to_string(),
            serde_json::json!({ "decision": "accept" }),
        )
        .await
        .expect_err("expected codex approval without live request to fail");

        assert!(error.contains("runtime connection was reset"));

        let conn = state.db.connect().expect("failed to open db connection");
        let approval_row = conn
            .query_row(
                "SELECT status, decision FROM approvals WHERE id = ?1",
                params![approval_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("failed to load approval row");
        assert_eq!(approval_row.0, "pending");
        assert_eq!(approval_row.1, None);

        let thread_status = conn
            .query_row(
                "SELECT status FROM threads WHERE id = ?1",
                params![thread.id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load thread status");
        assert_eq!(thread_status, "awaiting_approval");

        let raw_blocks = conn
            .query_row(
                "SELECT blocks_json FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load message blocks");
        let blocks: Value =
            serde_json::from_str(&raw_blocks).expect("message blocks should deserialize");
        assert_eq!(
            blocks
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("status"))
                .and_then(Value::as_str),
            Some("pending")
        );
        assert!(blocks
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("decision"))
            .is_none());
    }

    #[tokio::test]
    async fn load_codex_approval_response_route_reads_persisted_transport_metadata() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.5-codex");
        insert_pending_approval_with_details(
            &state,
            &thread,
            "approval-route",
            serde_json::json!({
                "command": "touch file.txt",
                "_serverMethod": "item/fileChange/requestApproval",
                "_rawRequestId": 42
            }),
        );

        let route = load_approval_response_route(state.db.clone(), "codex", "approval-route")
            .await
            .unwrap();

        assert_eq!(
            route,
            Some(ApprovalRequestRoute {
                server_method: "item/fileChange/requestApproval".to_string(),
                raw_request_id: serde_json::json!(42),
            })
        );
    }

    #[test]
    fn resolve_reasoning_effort_from_catalog_falls_back_to_model_default() {
        let engines = vec![EngineInfoDto {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            models: vec![EngineModelDto {
                id: "gpt-5.1-codex-mini".to_string(),
                display_name: "GPT-5.1 Codex Mini".to_string(),
                description: String::new(),
                hidden: false,
                is_default: false,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string()],
                attachment_modalities: vec!["text".to_string()],
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: vec![
                    ReasoningEffortOptionDto {
                        reasoning_effort: "medium".to_string(),
                        description: String::new(),
                    },
                    ReasoningEffortOptionDto {
                        reasoning_effort: "high".to_string(),
                        description: String::new(),
                    },
                ],
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }];

        assert_eq!(
            resolve_reasoning_effort_from_catalog(
                &engines,
                "codex",
                "gpt-5.1-codex-mini",
                Some("xhigh"),
            ),
            Some("medium".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_from_catalog_keeps_supported_effort() {
        let engines = vec![EngineInfoDto {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            models: vec![EngineModelDto {
                id: "gpt-5.1-codex-mini".to_string(),
                display_name: "GPT-5.1 Codex Mini".to_string(),
                description: String::new(),
                hidden: false,
                is_default: false,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string()],
                attachment_modalities: vec!["text".to_string()],
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: vec![
                    ReasoningEffortOptionDto {
                        reasoning_effort: "medium".to_string(),
                        description: String::new(),
                    },
                    ReasoningEffortOptionDto {
                        reasoning_effort: "high".to_string(),
                        description: String::new(),
                    },
                ],
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }];

        assert_eq!(
            resolve_reasoning_effort_from_catalog(
                &engines,
                "codex",
                "gpt-5.1-codex-mini",
                Some("high"),
            ),
            Some("high".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_omits_effort_for_unsupported_models() {
        let model = EngineModelDto {
            id: "haiku".to_string(),
            display_name: "Haiku".to_string(),
            description: String::new(),
            hidden: false,
            is_default: false,
            upgrade: None,
            availability_nux: None,
            upgrade_info: None,
            input_modalities: vec!["text".to_string(), "image".to_string()],
            attachment_modalities: vec!["text".to_string(), "image".to_string()],
            limits: None,
            supports_personality: false,
            default_reasoning_effort: "low".to_string(),
            supported_reasoning_efforts: Vec::new(),
        };

        assert_eq!(
            resolve_reasoning_effort_for_model(&model, Some("high")),
            None
        );
    }

    #[test]
    fn resolve_turn_model_id_accepts_thread_last_model_without_catalog() {
        let state = test_app_state();
        let mut thread = test_thread(&state, "codex", "gpt-5.5-codex");
        thread.model_id = "gpt-5.1-codex-mini".to_string();

        assert_eq!(
            resolve_turn_model_id(&thread, Some("gpt-5.1-codex-mini"), None)
                .expect("last model should resolve without a catalog"),
            "gpt-5.1-codex-mini"
        );
    }

    /// 大图（3000×2000 无透明 PNG）预览应逐级缩小并转 JPEG 编码，编码字节数与边长均不超过上限。
    #[tokio::test]
    async fn read_attachment_preview_shrinks_large_images() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-large-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for large image");
        let file_path = root.join("large.png");
        image::RgbImage::from_pixel(3000, 2000, image::Rgb([120, 60, 30]))
            .save(&file_path)
            .expect("failed to save large preview image");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect("large image preview should succeed");

        let payload = preview.expect("large image preview should be Some");
        assert_eq!(payload.mime_type, "image/jpeg");
        let bytes = BASE64
            .decode(payload.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        assert!(
            bytes.len() <= ATTACHMENT_PREVIEW_MAX_BYTES,
            "preview bytes {} should not exceed limit {}",
            bytes.len(),
            ATTACHMENT_PREVIEW_MAX_BYTES
        );
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert!(decoded.width() <= ATTACHMENT_PREVIEW_MAX_DIMENSION);
        assert!(decoded.height() <= ATTACHMENT_PREVIEW_MAX_DIMENSION);

        fs::remove_dir_all(&root).expect("failed to clean up large image temp dir");
    }

    /// 小图（100×100 带透明 PNG）预览应保持原尺寸并保留透明通道（PNG），不做放大。
    #[tokio::test]
    async fn read_attachment_preview_keeps_small_images_at_original_size() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-small-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for small image");
        let file_path = root.join("small.png");
        image::RgbaImage::from_pixel(100, 100, image::Rgba([10, 20, 30, 128]))
            .save(&file_path)
            .expect("failed to save small preview image");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect("small image preview should succeed");

        let payload = preview.expect("small image preview should be Some");
        assert_eq!(payload.mime_type, "image/png");
        let bytes = BASE64
            .decode(payload.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        assert!(
            bytes.len() <= ATTACHMENT_PREVIEW_MAX_BYTES,
            "preview bytes {} should not exceed limit {}",
            bytes.len(),
            ATTACHMENT_PREVIEW_MAX_BYTES
        );
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 100);

        fs::remove_dir_all(&root).expect("failed to clean up small image temp dir");
    }

    /// 新发送图片生成永久预览路径；消息尚未持久化时，批次释放应清理永久副本。
    #[tokio::test]
    async fn persistent_image_preview_batch_cleans_uncommitted_files() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-message-image-preview-uncommitted-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let source_file_path = root.join("source.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 128]))
            .save(&source_file_path)
            .expect("failed to save persistent preview source image");
        let mut attachment = test_attachment("source.png", Some("image/png"));
        attachment.file_path = source_file_path.display().to_string();
        attachment.size_bytes = fs::metadata(&source_file_path)
            .expect("failed to read source image metadata")
            .len();

        let batch = persist_message_image_previews(std::slice::from_mut(&mut attachment))
            .await
            .expect("persistent preview should be created");
        let preview_file_path = PathBuf::from(
            attachment
                .preview_file_path
                .as_deref()
                .expect("attachment should contain persistent preview path"),
        );
        assert!(preview_file_path.is_file());

        drop(batch);

        assert!(!preview_file_path.exists());
        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 消息持久化后提交批次，永久图片副本应继续保留。
    #[tokio::test]
    async fn persistent_image_preview_batch_keeps_committed_files() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-message-image-preview-committed-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let source_file_path = root.join("source.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([120, 60, 30]))
            .save(&source_file_path)
            .expect("failed to save persistent preview source image");
        let mut attachment = test_attachment("source.png", Some("image/png"));
        attachment.file_path = source_file_path.display().to_string();
        attachment.size_bytes = fs::metadata(&source_file_path)
            .expect("failed to read source image metadata")
            .len();

        let batch = persist_message_image_previews(std::slice::from_mut(&mut attachment))
            .await
            .expect("persistent preview should be created");
        let preview_file_path = PathBuf::from(
            attachment
                .preview_file_path
                .as_deref()
                .expect("attachment should contain persistent preview path"),
        );

        batch.commit();

        assert!(preview_file_path.is_file());
        fs::remove_file(&preview_file_path)
            .expect("failed to clean up committed persistent preview file");
        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 已发送消息读取时应优先使用永久副本，即使原附件路径已经失效。
    #[tokio::test]
    async fn read_attachment_preview_prefers_persistent_file_path() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-persistent-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let preview_file_path = root.join("persistent.png");
        image::RgbaImage::from_pixel(12, 10, image::Rgba([10, 20, 30, 128]))
            .save(&preview_file_path)
            .expect("failed to save persistent preview image");

        let preview = read_attachment_preview(
            root.join("missing-original.png").display().to_string(),
            Some("image/png".to_string()),
            Some(preview_file_path.display().to_string()),
        )
        .await
        .expect("persistent preview path should be preferred")
        .expect("persistent preview should be available");

        assert_eq!(preview.mime_type, "image/png");
        let bytes = BASE64
            .decode(preview.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert_eq!((decoded.width(), decoded.height()), (12, 10));

        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 消息块序列化时应保留永久预览路径，供前端重新加载聊天记录后读取。
    #[test]
    fn user_attachment_block_serializes_persistent_preview_path() {
        let mut attachment = test_attachment("result.png", Some("image/png"));
        attachment.preview_file_path =
            Some("C:\\AuraCoder\\attachments\\message-images\\result.png".to_string());

        let blocks = build_user_blocks("查看结果", &[], &[attachment], false, false, None);
        let value = serde_json::to_value(blocks).expect("user blocks should serialize");

        assert_eq!(
            value[0]["previewFilePath"],
            "C:\\AuraCoder\\attachments\\message-images\\result.png"
        );
    }

    /// 图片内容损坏时保留底层解码异常，IPC 仅返回业务语义错误。
    #[tokio::test]
    async fn read_attachment_preview_reports_invalid_image_as_business_error() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-invalid-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for invalid image");
        let file_path = root.join("invalid.png");
        fs::write(&file_path, b"not an image").expect("failed to write invalid image");

        let error = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect_err("invalid image should return a business error");

        assert_eq!(error, "图片预览加载失败");
        fs::remove_dir_all(&root).expect("failed to clean up invalid image temp dir");
    }

    /// 非图片附件（text/plain）预览应返回 None，不产生编码数据。
    #[tokio::test]
    async fn read_attachment_preview_rejects_non_image() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-text-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for text file");
        let file_path = root.join("hello.txt");
        fs::write(&file_path, "hello world").expect("failed to write text file");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("text/plain".to_string()),
            None,
        )
        .await
        .expect("non-image preview should not error");

        assert!(preview.is_none());

        fs::remove_dir_all(&root).expect("failed to clean up text file temp dir");
    }
}

fn resolve_turn_model_id(
    thread: &ThreadDto,
    requested_model_id: Option<&str>,
    engines: Option<&[EngineInfoDto]>,
) -> Result<String, String> {
    let Some(requested_model_id) = requested_model_id else {
        return Ok(thread.model_id.clone());
    };

    if requested_model_id == thread.model_id {
        return Ok(thread.model_id.clone());
    }

    if let Some(engines) = engines {
        if let Some(engine) = engines.iter().find(|engine| engine.id == thread.engine_id) {
            if engine
                .models
                .iter()
                .any(|model| model.id == requested_model_id)
            {
                return Ok(requested_model_id.to_string());
            }

            let available = engine
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "model `{requested_model_id}` is not supported by engine `{}`. available models: {available}",
                thread.engine_id
            ));
        }
    }

    Ok(requested_model_id.to_string())
}

async fn model_supports_personality(state: &AppState, engine_id: &str, model_id: &str) -> bool {
    let Ok(engines) = crate::commands::engines::list_local_engine_infos(state).await else {
        return false;
    };

    engines
        .iter()
        .find(|engine| engine.id == engine_id)
        .and_then(|engine| engine.models.iter().find(|model| model.id == model_id))
        .map(|model| model.supports_personality)
        .unwrap_or(false)
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}
