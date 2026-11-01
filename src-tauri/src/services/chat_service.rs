use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;
use tauri::AppHandle;
use tokio::fs as tokio_fs;
use uuid::Uuid;

use crate::{
    cli_tools::factory::CliToolFactory,
    db,
    engines::{BrowserAnnotationMetadata, TurnAttachment, TurnInputItem},
    models::{
        chat::{
            AttachmentPreviewPayload, ChatAttachmentPayload, ChatBusinessError,
            ChatInputItemPayload, ClipboardImagePayload, SendMessageRequest,
        },
        ActionOutputDto, MessageDto, MessageWindowCursorDto, MessageWindowDto, SearchResultDto,
        ThreadDto,
    },
    runtime_env,
    services::thread_service::ThreadService,
    state::AppState,
};

const MAX_PASTED_IMAGE_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const ATTACHMENT_PREVIEW_MAX_DIMENSION: u32 = 1600;
const ATTACHMENT_PREVIEW_MAX_BYTES: usize = 700 * 1024;
const ATTACHMENT_PREVIEW_JPEG_QUALITY: u8 = 80;
const MESSAGE_WINDOW_DEFAULT_LIMIT: usize = 120;
const MESSAGE_WINDOW_MAX_LIMIT: usize = 400;
const MAX_ATTACHMENTS_PER_TURN: usize = 10;
const MAX_BROWSER_ANNOTATION_COMMENT_CHARS: usize = 2_000;
const MAX_BROWSER_ANNOTATION_URL_CHARS: usize = 4_096;
const MAX_BROWSER_ANNOTATION_TARGET_CHARS: usize = 600;
const IMAGE_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg",
];

/// 提供 CLI 无关的聊天发送路由、消息查询和附件文件处理能力。
pub struct ChatService {
    /// 当前聊天业务使用的数据库、回合状态和 CLI 工厂依赖。
    state: AppState,
}

/// 从系统剪贴板读取图片，或从剪贴板 URI 列表中收集本地图片文件。
pub async fn read_clipboard_image(app: AppHandle) -> Result<Vec<ClipboardImagePayload>, String> {
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let clipboard_image = match app.clipboard().read_image() {
            Ok(image) => image,
            Err(error) => {
                log::debug!("读取系统剪贴板图片失败或无图片: {error}");
                let text = match app.clipboard().read_text() {
                    Ok(text) => text,
                    Err(text_error) => {
                        log::debug!("读取系统剪贴板文本失败或无文本: {text_error}");
                        return Ok(Vec::new());
                    }
                };
                let mut payloads = Vec::new();
                for raw_line in text.lines() {
                    let line = raw_line.trim();
                    if line.is_empty() || line == "copy" || line == "cut" || line.starts_with('#') {
                        continue;
                    }
                    let without_scheme = line.strip_prefix("file://").unwrap_or(line);
                    let decoded = match percent_encoding::percent_decode_str(without_scheme)
                        .decode_utf8()
                    {
                        Ok(value) => value.into_owned(),
                        Err(decode_error) => {
                            log::warn!("剪贴板文件路径 URL 解码失败，已跳过: {line}，原始错误: {decode_error}");
                            continue;
                        }
                    };
                    let path = PathBuf::from(&decoded);
                    if !path.is_file() {
                        continue;
                    }
                    let Some(extension) = path
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(|value| value.to_lowercase())
                    else {
                        continue;
                    };
                    if !IMAGE_ATTACHMENT_EXTENSIONS.contains(&extension.as_str()) {
                        continue;
                    }
                    let Some(mime_type) = image_mime_type_for_extension(&extension) else {
                        continue;
                    };
                    let bytes = match std::fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(read_error) => {
                            log::error!("读取剪贴板引用的图片文件失败: {decoded}，原始错误: {read_error}");
                            continue;
                        }
                    };
                    if bytes.is_empty() || bytes.len() > MAX_PASTED_IMAGE_ATTACHMENT_BYTES {
                        continue;
                    }
                    let file_name = path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            format!(
                                "clipboard-image-{}.{}",
                                chrono::Local::now().format("%Y%m%d%H%M%S"),
                                extension
                            )
                        });
                    payloads.push(ClipboardImagePayload {
                        file_name,
                        mime_type: mime_type.to_string(),
                        data_base64: BASE64.encode(&bytes),
                        source_path: Some(decoded),
                    });
                }
                return Ok(payloads);
            }
        };
        let width = clipboard_image.width();
        let height = clipboard_image.height();
        let rgba = clipboard_image.rgba();
        let Some(rgba_image) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) else {
            log::error!(
                "剪贴板图片 RGBA 数据与尺寸不匹配: width={width}, height={height}, bytes={}",
                rgba.len()
            );
            return Err("剪贴板图片数据不完整，无法作为附件。".to_string());
        };
        let mut buffer = std::io::Cursor::new(Vec::new());
        if let Err(error) = rgba_image.write_to(&mut buffer, image::ImageFormat::Png) {
            log::error!("剪贴板图片编码 PNG 失败: {error}");
            return Err(format!("剪贴板图片编码失败: {error}"));
        }
        Ok(vec![ClipboardImagePayload {
            file_name: format!(
                "clipboard-image-{}.png",
                chrono::Local::now().format("%Y%m%d%H%M%S")
            ),
            mime_type: "image/png".to_string(),
            data_base64: BASE64.encode(buffer.into_inner()),
            source_path: None,
        }])
    })
    .await
    .map_err(|error| {
        log::error!("读取剪贴板图片任务执行失败: {error}");
        error.to_string()
    })?
}

/// 管理一轮聊天提交过程中创建的预览文件，提交失败时回收文件。
pub(crate) struct PersistentImagePreviewBatch {
    /// 本轮成功或进行中创建的预览文件路径。
    file_paths: Vec<PathBuf>,
    /// 标记预览文件已随消息写入成功而提交保留。
    committed: bool,
}

impl PersistentImagePreviewBatch {
    /// 创建用于回收本轮预览文件的批次。
    pub(crate) fn new() -> Self {
        Self {
            file_paths: Vec::new(),
            committed: false,
        }
    }

    /// 确认消息已保存并保留本批次的预览文件。
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PersistentImagePreviewBatch {
    /// 在发送未提交时删除临时预览文件。
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

/// 为本轮附件保存独立预览文件，并记录失败时需要回收的路径。
pub(crate) async fn persist_message_image_previews(
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
        anyhow::ensure!(metadata.is_file(), "图片附件不是普通文件：{}", attachment.file_name);
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
        let target_file_path = preview_directory.join(format!("{}.{}", Uuid::new_v4().simple(), extension));
        let temporary_file_path = preview_directory.join(format!(".{}.part", Uuid::new_v4().simple()));
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

/// 将预览文件保存异常转换为发送失败提示，并保留原始异常日志。
pub(crate) async fn prepare_message_image_previews(
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

impl ChatService {
    /// 创建绑定当前应用状态的公共聊天 Service。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 根据本次请求或线程归属选择唯一 CLI，并提交普通聊天消息。
    pub async fn send_message(
        &self,
        app: AppHandle,
        request: SendMessageRequest,
    ) -> Result<String, ChatBusinessError> {
        let selected_engine_id = match request
            .engine_id
            .as_deref()
            .map(str::trim)
            .filter(|engine_id| !engine_id.is_empty())
        {
            Some(engine_id) => engine_id.to_string(),
            None => ThreadService::new(self.state.clone())
                .get_thread(&request.thread_id)
                .await
                .map_err(|error| {
                    log::error!("读取聊天线程归属失败: {error:#}");
                    ChatBusinessError::new("CHAT_OPERATION_FAILED", "无法读取聊天线程")
                })?
                .engine_id,
        };

        // 将普通发送转交给所选 CLI 接口，由该实现维持整轮生命周期。
        let cli = CliToolFactory::new(self.state.clone())
            .create(&selected_engine_id)
            .map_err(|error| {
                log::error!("创建聊天 CLI `{selected_engine_id}` 失败: {error:#}");
                ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
            })?;
        cli.submit_chat_message(app, request).await
    }

    /// 通过线程归属获取当前 CLI，并取消该线程正在执行的回合。
    pub async fn cancel_turn(&self, thread_id: String) -> Result<(), ChatBusinessError> {
        let cli = ThreadService::new(self.state.clone())
            .cli_for_thread(&thread_id)
            .await
            .map_err(|error| {
                log::error!("获取线程 `{thread_id}` 的 CLI 失败: {error:#}");
                ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
            })?;
        cli.cancel_chat_turn(&thread_id).await
    }

    /// 查询线程消息列表，供聊天记录展示使用。
    pub async fn get_thread_messages(&self, thread_id: String) -> Result<Vec<MessageDto>, String> {
        run_db(self.state.db.clone(), move |database| {
            db::messages::get_thread_messages(database, &thread_id)
        })
        .await
    }

    /// 按游标和限制分页查询线程消息，避免一次返回完整历史。
    pub async fn get_thread_messages_window(
        &self,
        thread_id: String,
        cursor: Option<MessageWindowCursorDto>,
        limit: Option<usize>,
    ) -> Result<MessageWindowDto, String> {
        let clamped_limit = limit
            .unwrap_or(MESSAGE_WINDOW_DEFAULT_LIMIT)
            .clamp(1, MESSAGE_WINDOW_MAX_LIMIT);
        run_db(self.state.db.clone(), move |database| {
            db::messages::get_thread_messages_window(
                database,
                &thread_id,
                cursor.as_ref(),
                clamped_limit,
            )
        })
        .await
    }

    /// 查询一条聊天消息已持久化的内容块。
    pub async fn get_message_blocks(&self, message_id: String) -> Result<Option<Value>, String> {
        run_db(self.state.db.clone(), move |database| {
            db::messages::get_message_blocks(database, &message_id)
        })
        .await
    }

    /// 查询一条消息中指定动作的已持久化输出。
    pub async fn get_action_output(
        &self,
        message_id: String,
        action_id: String,
    ) -> Result<ActionOutputDto, String> {
        run_db(self.state.db.clone(), move |database| {
            db::messages::get_action_output(database, &message_id, &action_id)
        })
        .await
    }

    /// 在指定工作区内搜索聊天消息并返回匹配摘要。
    pub async fn search_messages(
        &self,
        workspace_id: String,
        query: String,
    ) -> Result<Vec<SearchResultDto>, String> {
        run_db(self.state.db.clone(), move |database| {
            db::messages::search_messages(database, &workspace_id, &query)
        })
        .await
    }
}

/// 在阻塞线程执行数据库读取或写入，避免阻塞异步聊天请求。
async fn run_db<T, F>(database: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    let result = match tokio::task::spawn_blocking(move || operation(&database)).await {
        Ok(result) => result,
        Err(error) => Err(anyhow::Error::new(error).context("聊天数据库任务执行失败")),
    };
    result.map_err(|error| {
        log::error!("聊天数据库任务失败: {error:#}");
        format!("{error:#}")
    })
}

/// 保存粘贴的图片并返回可提交到聊天消息中的附件信息。
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

/// 读取聊天附件的图片预览；不支持或超限的图片返回无预览结果。
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

/// 读取并缩放受支持的图片文件以生成聊天附件预览。
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

/// 按预览大小限制编码 SVG 或缩放后的位图。
fn encode_attachment_preview(
    preview_mime_type: String,
    bytes: &[u8],
) -> anyhow::Result<Option<AttachmentPreviewPayload>> {
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

/// 根据显式媒体类型或文件扩展名规范化图片预览类型。
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

/// 按媒体类型返回粘贴图片文件应使用的扩展名。
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

/// 按附件文件扩展名返回标准图片媒体类型。
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


/// 解析本线程允许引用的同一项目内AuraCoder线程。
pub(crate) async fn resolve_auracoder_thread_reference(
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
pub(crate) fn build_auracoder_thread_instruction(thread: &ThreadDto) -> String {
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
/// 在完成状态和消息字段都写入数据库后，按精确消息 ID发送远程通知。

/// 将结构化聊天输入转换为统一CLI输入项并校验文本组成。
pub(crate) fn normalize_input_items(
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

pub(crate) fn normalize_attachments(
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

pub(crate) fn normalize_browser_annotation_text(
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

pub(crate) fn normalize_browser_annotation(
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

pub(crate) fn browser_annotation_context(attachments: &[TurnAttachment]) -> String {
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

pub(crate) fn format_engine_message(message: &str, browser_annotation_context: &str) -> String {
    if browser_annotation_context.is_empty() {
        message.to_string()
    } else {
        format!("{message}{browser_annotation_context}")
    }
}

pub(crate) fn append_browser_annotation_context(
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

