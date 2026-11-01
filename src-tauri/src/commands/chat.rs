use serde_json::Value;
use tauri::{AppHandle, State};

use crate::{
    models::{
        ActionOutputDto, MessageDto, MessageWindowCursorDto, MessageWindowDto,
        PermissionComponentJson, SearchResultDto, SteerReceiptDto, ThreadDto,
    },
    models::chat::{
        ApprovalResponseOutcomeDto, AttachmentPreviewPayload, ChatAttachmentPayload,
        ChatBusinessError, ChatInputItemPayload, ClipboardImagePayload,
        CodexReviewDeliveryPayload, CodexReviewTargetPayload, SendMessageRequest,
    },
    services::{chat_service, chat_service::ChatService, thread_service::ThreadService},
    state::AppState,
};

/// 接收前端流式渲染埋点并写入统一应用日志。
#[tauri::command]
pub fn append_stream_render_debug(payload: &str) -> Result<(), String> {
    // 记录前端提交的流式渲染诊断信息，供排查渲染问题时查看。
    log::info!("[STREAM-RENDER-DEBUG] {}", payload);
    Ok(())
}

/// 从系统剪贴板读取图片或图片文件引用。
#[tauri::command]
pub async fn read_clipboard_image(app: AppHandle) -> Result<Vec<ClipboardImagePayload>, String> {
    // 将剪贴板读取交给公共聊天 Service 处理。
    chat_service::read_clipboard_image(app).await
}

/// 保存用户粘贴的图片并返回聊天附件信息。
#[tauri::command]
pub async fn save_pasted_image_attachment(
    file_name: String,
    mime_type: String,
    data_base64: String,
) -> Result<ChatAttachmentPayload, String> {
    // 将图片附件文件处理交给公共聊天 Service 处理。
    chat_service::save_pasted_image_attachment(file_name, mime_type, data_base64).await
}

/// 读取聊天附件图片预览。
#[tauri::command]
pub async fn read_attachment_preview(
    file_path: String,
    mime_type: Option<String>,
    preview_file_path: Option<String>,
) -> Result<Option<AttachmentPreviewPayload>, String> {
    // 将附件预览读取交给公共聊天 Service 处理。
    chat_service::read_attachment_preview(file_path, mime_type, preview_file_path).await
}

/// 提交一条聊天消息并返回当前 CLI 的助手消息标识。
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    request: SendMessageRequest,
) -> Result<String, ChatBusinessError> {
    // 将普通聊天发送交给公共聊天 Service 路由到目标 CLI。
    ChatService::new(state.inner().clone())
        .send_message(app, request)
        .await
}

/// 为指定线程启动 Codex 代码审查。
#[tauri::command]
pub async fn start_codex_review(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    target: CodexReviewTargetPayload,
    delivery: Option<CodexReviewDeliveryPayload>,
) -> Result<ThreadDto, ChatBusinessError> {
    // 按线程归属取得 CLI 接口并提交审查请求。
    let cli = ThreadService::new(state.inner().clone())
        .cli_for_thread(&thread_id)
        .await
        .map_err(|error| {
            log::error!("获取线程聊天 CLI 失败: {error:#}");
            ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
        })?;
    cli.submit_chat_review(
        app,
        crate::models::chat::ChatReviewRequest {
            thread_id,
            target,
            delivery,
        },
    )
    .await
}

/// 向当前活动线程追加一条消息并返回 CLI 的接收回执。
#[tauri::command]
pub async fn steer_message(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    message: String,
    attachments: Option<Vec<ChatAttachmentPayload>>,
    input_items: Option<Vec<ChatInputItemPayload>>,
    plan_mode: Option<bool>,
    permission_values: Option<PermissionComponentJson>,
    client_steer_id: String,
    referenced_thread_id: Option<String>,
) -> Result<SteerReceiptDto, ChatBusinessError> {
    // 按线程归属取得 CLI 接口，并将必填 IPC 标识包装成请求字段。
    let cli = ThreadService::new(state.inner().clone())
        .cli_for_thread(&thread_id)
        .await
        .map_err(|error| {
            log::error!("获取线程聊天 CLI 失败: {error:#}");
            ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
        })?;
    cli.submit_chat_steer(
        app,
        crate::models::chat::SteerMessageRequest {
            thread_id,
            message,
            attachments,
            input_items,
            plan_mode,
            client_steer_id: Some(client_steer_id),
            referenced_thread_id,
            permission_values,
        },
    )
    .await
}

/// 取消指定线程正在执行的聊天回合。
#[tauri::command]
pub async fn cancel_turn(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), ChatBusinessError> {
    // 将回合取消请求交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .cancel_turn(thread_id)
        .await
}

/// 重启指定线程所属的远端 CLI 服务。
#[tauri::command]
pub async fn restart_remote_cli_service(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), ChatBusinessError> {
    // 按线程归属取得 CLI 接口并请求重启远端服务。
    let cli = ThreadService::new(state.inner().clone())
        .cli_for_thread(&thread_id)
        .await
        .map_err(|error| {
            log::error!("获取线程聊天 CLI 失败: {error:#}");
            ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
        })?;
    cli.restart_chat_service(&thread_id).await
}

/// 向当前线程所属的 CLI 提交审批选择并返回实际保存结果。
#[tauri::command]
pub async fn respond_to_approval(
    state: State<'_, AppState>,
    thread_id: String,
    approval_id: String,
    response: Value,
) -> Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
    // 按线程归属取得 CLI 接口，并将审批输入交给该实现处理。
    let cli = ThreadService::new(state.inner().clone())
        .cli_for_thread(&thread_id)
        .await
        .map_err(|error| {
            log::error!("获取线程聊天 CLI 失败: {error:#}");
            ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
        })?;
    cli.answer_chat_approval(crate::models::chat::ApprovalResponseRequest {
        thread_id,
        approval_id,
        response,
    })
    .await
}

/// 查询指定线程的聊天消息。
#[tauri::command]
pub async fn get_thread_messages(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<MessageDto>, String> {
    // 将消息列表查询交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .get_thread_messages(thread_id)
        .await
}

/// 按游标和限制分页查询指定线程的聊天消息。
#[tauri::command]
pub async fn get_thread_messages_window(
    state: State<'_, AppState>,
    thread_id: String,
    cursor: Option<MessageWindowCursorDto>,
    limit: Option<usize>,
) -> Result<MessageWindowDto, String> {
    // 将分页消息查询交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .get_thread_messages_window(thread_id, cursor, limit)
        .await
}

/// 查询一条聊天消息已持久化的内容块。
#[tauri::command]
pub async fn get_message_blocks(
    state: State<'_, AppState>,
    message_id: String,
) -> Result<Option<Value>, String> {
    // 将消息块查询交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .get_message_blocks(message_id)
        .await
}

/// 查询一条消息中指定动作的已持久化输出。
#[tauri::command]
pub async fn get_action_output(
    state: State<'_, AppState>,
    message_id: String,
    action_id: String,
) -> Result<ActionOutputDto, String> {
    // 将动作输出查询交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .get_action_output(message_id, action_id)
        .await
}

/// 在指定工作区内搜索聊天消息并返回匹配摘要。
#[tauri::command]
pub async fn search_messages(
    state: State<'_, AppState>,
    workspace_id: String,
    query: String,
) -> Result<Vec<SearchResultDto>, String> {
    // 将聊天搜索交给公共聊天 Service。
    ChatService::new(state.inner().clone())
        .search_messages(workspace_id, query)
        .await
}

#[cfg(test)]
#[path = "../../tests/unit/chat_commands_tests.rs"]
mod tests;
