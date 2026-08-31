use std::{
    collections::{HashMap, HashSet},
    // 旧 executable_augmented_path 实现由 runtime_env::get 接替：
    // ffi::OsString,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener as AsyncTcpListener,
    sync::{broadcast, mpsc, Mutex},
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::models::{
    OpenCodeAgentDto, OpenCodeCommandDto, OpenCodeMcpServerDto, OpenCodeRuntimeCatalogDto,
};
use crate::{
    auracoder_thread_mcp_service::AuraCoderThreadMcpService,
    computer_control_service::ComputerControlService,
};

use super::{
    normalize_approval_response_for_engine, trim_action_output_delta_content, ActionResult,
    ActionType, ApprovalRequestRoute, DiffScope, Engine, EngineEvent, EngineSteerReceipt,
    EngineThread, ImportedThreadMessage, ModelInfo, ModelLimits, OpenCodeRemoteSessionSummary,
    OutputStream, ReasoningEffortOption, SandboxPolicy, ThreadScope, ThreadSyncSnapshot,
    TokenUsage, TurnCompletionStatus, TurnInput,
};

const OPENCODE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const OPENCODE_RECONCILE_MESSAGE_LIMIT: usize = 128;
// OpenCode 完整历史同步的单次请求上限；活动回合补偿仍使用上面的固定 128 条上限。
const OPENCODE_HISTORY_PAGE_LIMIT: usize = 128;
// 完整历史同步允许多个分页请求，但仍需在有限时间内结束。
const OPENCODE_HISTORY_TIMEOUT: Duration = Duration::from_secs(60);
const OPENCODE_EVENT_BUFFER_CAPACITY: usize = 1024;
const OPENCODE_EVENT_QUEUE_CAPACITY: usize = OPENCODE_EVENT_BUFFER_CAPACITY;
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(900);
const OPENCODE_MESSAGE_ID_RANDOM_LEN: usize = 14;
const OPENCODE_ID_COUNTER_STEP: u64 = 0x1000;
const OPENCODE_ID_TIME_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

static LAST_OPENCODE_MESSAGE_SORT_VALUE: AtomicU64 = AtomicU64::new(0);

/// OpenCode 协议客户端，只负责 HTTP 请求、会话状态和事件转换。
///
/// 本对象不创建、启动、复用、停止或释放本机 OpenCode 服务；本机 endpoint 必须由
/// `LocalCliServiceLifecycle` 提供，远端 endpoint 必须由 SSH CLI 生命周期提供。
pub struct OpenCodeEngine {
    /// OpenCode 会话、协议连接和待处理请求状态。
    state: Arc<Mutex<OpenCodeState>>,
    /// OpenCode HTTP 协议请求使用的客户端。
    http: reqwest::Client,
    /// 本机计算机控制服务，用于将 GUI 工具注入当前回合。
    computer_control_service: Arc<std::sync::Mutex<Option<Arc<ComputerControlService>>>>,
    /// AuraCoder 本地会话读取工具服务。
    auracoder_thread_mcp_service: Arc<std::sync::Mutex<Option<Arc<AuraCoderThreadMcpService>>>>,
    /// 当前协议客户端绑定的传输 endpoint。
    target: OpenCodeTransportTarget,
}

#[derive(Clone)]
enum OpenCodeTransportTarget {
    /// 未绑定 endpoint 的能力对象，仅用于静态模型信息和兼容入口。
    Local,
    /// 由本机 CLI 生命周期提供的 OpenCode HTTP endpoint。
    LocalHttp(Arc<LocalOpenCodeEndpoint>),
    /// 由 SSH CLI 生命周期提供的远端 OpenCode HTTP endpoint。
    Remote(Arc<RemoteOpenCodeEndpoint>),
}

/// OpenCode 协议连接的本机 endpoint 描述。
struct LocalOpenCodeEndpoint {
    /// 本机 OpenCode 服务绑定的项目目录。
    cwd: String,
    /// 本机 OpenCode HTTP 服务基础地址。
    base_url: String,
    /// 本机 OpenCode HTTP 服务 Basic Auth 密码。
    password: String,
    /// endpoint 对应的 OpenCode CLI 版本文本。
    version: Option<String>,
    /// endpoint 对应的本机服务生命周期代数。
    generation: u64,
}

struct RemoteOpenCodeEndpoint {
    /// 远端 OpenCode HTTP 服务基础地址。
    base_url: String,
    /// 远端 OpenCode HTTP 服务 Basic Auth 密码。
    password: String,
    /// 远端 OpenCode 事件广播总线。
    event_bus: broadcast::Sender<OpenCodeBusItem>,
    /// 远端事件订阅任务的取消令牌。
    pump_cancel: CancellationToken,
    #[cfg(test)]
    /// 测试是否启用工作区事件泵。
    workspace_event_pump_enabled: bool,
}

impl Drop for RemoteOpenCodeEndpoint {
    fn drop(&mut self) {
        self.pump_cancel.cancel();
    }
}

/// OpenCode 协议客户端的共享运行状态。
#[derive(Default)]
struct OpenCodeState {
    /// 按 cwd 缓存不含服务生命周期的协议连接。
    connections: HashMap<String, Arc<OpenCodeConnection>>,
    /// 当前引擎已知的 OpenCode 会话元数据。
    sessions: HashMap<String, OpenCodeSession>,
    /// 等待审批或问题回答的协议请求。
    pending_requests: HashMap<String, PendingOpenCodeRequest>,
    /// 运行时模型目录缓存。
    runtime_model_cache: Option<Vec<ModelInfo>>,
}

/// OpenCode 会话与其协议连接的关联信息。
#[derive(Clone)]
struct OpenCodeSession {
    /// 会话所在的项目目录。
    cwd: String,
    /// 会话使用的模型标识。
    model_id: String,
    /// 会话使用的推理强度。
    reasoning_effort: Option<String>,
    /// 会话使用的 OpenCode agent。
    agent: Option<String>,
    /// 会话权限模式。
    permission_mode: OpenCodePermissionMode,
    /// 会话对应的协议连接。
    connection: Arc<OpenCodeConnection>,
}

/// OpenCode HTTP 协议连接及其事件泵状态。
struct OpenCodeConnection {
    /// 协议请求对应的项目目录。
    cwd: String,
    /// OpenCode HTTP 服务基础地址。
    base_url: String,
    /// OpenCode HTTP 服务 Basic Auth 密码。
    password: String,
    /// OpenCode 事件广播总线。
    event_bus: broadcast::Sender<OpenCodeBusItem>,
    /// OpenCode 事件泵取消令牌。
    pump_cancel: Option<CancellationToken>,
    /// OpenCode 回调服务取消令牌。
    callback_cancel: Option<CancellationToken>,
    /// 是否需要为请求附加项目目录头。
    include_directory_header: bool,
}

impl OpenCodeConnection {
    /// 停止协议事件和回调任务，但不触碰 OpenCode 服务进程。
    fn cancel_event_pump(&self) {
        if let Some(pump_cancel) = self.pump_cancel.as_ref() {
            pump_cancel.cancel();
        }
        if let Some(callback_cancel) = self.callback_cancel.as_ref() {
            callback_cancel.cancel();
        }
    }
}

impl Drop for OpenCodeConnection {
    fn drop(&mut self) {
        self.cancel_event_pump();
    }
}

#[derive(Clone)]
enum PendingOpenCodeRequest {
    Permission {
        request_id: String,
        connection: Arc<OpenCodeConnection>,
    },
    Question {
        request_id: String,
        questions: Vec<OpenCodeQuestionInfo>,
        connection: Arc<OpenCodeConnection>,
    },
}

struct OpenCodePromptBody {
    message_id: String,
    body: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenCodePermissionMode {
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct OpenCodeHealthReport {
    pub available: bool,
    pub version: Option<String>,
    pub details: Option<String>,
    pub warnings: Vec<String>,
    pub checks: Vec<String>,
    pub fixes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeHealthResponse {
    healthy: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeSessionInfo {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeSessionRecord {
    id: String,
    title: Option<String>,
    directory: String,
    permission: Option<Value>,
    time: OpenCodeSessionTime,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeSessionTime {
    created: i64,
    updated: i64,
    archived: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeAgentModelRef {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeRuntimeAgent {
    name: String,
    description: Option<String>,
    mode: String,
    native: Option<bool>,
    hidden: Option<bool>,
    model: Option<OpenCodeAgentModelRef>,
    variant: Option<String>,
    steps: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeRuntimeCommand {
    name: String,
    description: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    source: Option<String>,
    subtask: Option<bool>,
    #[serde(default)]
    hints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeProviderList {
    all: Vec<OpenCodeProvider>,
    connected: Vec<String>,
    #[allow(dead_code)]
    default: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeProvider {
    id: String,
    name: String,
    models: HashMap<String, OpenCodeProviderModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeProviderModel {
    id: String,
    name: String,
    status: Option<String>,
    limit: Option<OpenCodeModelLimit>,
    capabilities: Option<OpenCodeModelCapabilities>,
    #[serde(default)]
    variants: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeVerboseModel {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    name: String,
    status: Option<String>,
    limit: Option<OpenCodeModelLimit>,
    capabilities: Option<OpenCodeModelCapabilities>,
    #[serde(default)]
    variants: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeModelCapabilities {
    #[serde(default)]
    attachment: bool,
    input: Option<OpenCodeModelInputCapabilities>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenCodeModelLimit {
    context: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenCodeModelInputCapabilities {
    #[serde(default)]
    text: bool,
    #[serde(default)]
    image: bool,
    #[serde(default)]
    pdf: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeBusEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    properties: Value,
}

#[derive(Debug, Clone)]
enum OpenCodeBusItem {
    Event(Arc<OpenCodeBusEvent>),
    Failure(String),
}

#[derive(Debug)]
enum OpenCodeIncomingEvent {
    Message(Arc<OpenCodeBusEvent>),
    Failure(String),
    Lagged(u64),
    Closed,
}

fn spawn_opencode_incoming_pump(
    event_bus: broadcast::Sender<OpenCodeBusItem>,
) -> mpsc::Receiver<OpenCodeIncomingEvent> {
    let (queue_tx, queue_rx) = mpsc::channel(OPENCODE_EVENT_QUEUE_CAPACITY);
    let mut subscription = event_bus.subscribe();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = queue_tx.closed() => break,
                incoming = subscription.recv() => {
                    let item = match incoming {
                        Ok(OpenCodeBusItem::Event(event)) => {
                            OpenCodeIncomingEvent::Message(event)
                        }
                        Ok(OpenCodeBusItem::Failure(message)) => {
                            OpenCodeIncomingEvent::Failure(message)
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            OpenCodeIncomingEvent::Lagged(skipped)
                        }
                        Err(broadcast::error::RecvError::Closed) => OpenCodeIncomingEvent::Closed,
                    };
                    let closed = matches!(&item, OpenCodeIncomingEvent::Closed);

                    if queue_tx.send(item).await.is_err() {
                        break;
                    }

                    if closed {
                        break;
                    }
                }
            }
        }
    });

    queue_rx
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodePartEnvelope {
    part: OpenCodePart,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OpenCodePart {
    id: String,
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "type")]
    part_type: String,
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    #[serde(rename = "callID")]
    call_id: Option<String>,
    name: Option<String>,
    source: Option<Value>,
    metadata: Option<Value>,
    text: Option<String>,
    tool: Option<String>,
    state: Option<OpenCodeToolState>,
    reason: Option<String>,
    /// OpenCode 文件 part 的远端文件地址或路径。
    #[serde(default)]
    file: Option<String>,
    /// OpenCode patch part 的统一补丁文本。
    #[serde(default)]
    patch: Option<String>,
    /// OpenCode diff part 的统一差异文本。
    #[serde(default)]
    diff: Option<String>,
    /// OpenCode 文件 part 的显示文件名。
    #[serde(default)]
    filename: Option<String>,
    /// OpenCode 文件 part 的 MIME 类型。
    #[serde(default)]
    mime: Option<String>,
    /// OpenCode 文件 part 的 URL。
    #[serde(default)]
    url: Option<String>,
    /// OpenCode patch part 包含的文件差异集合。
    #[serde(default)]
    files: Option<Vec<Value>>,
    /// OpenCode patch part 的稳定哈希标识。
    #[serde(default)]
    hash: Option<String>,
    /// OpenCode step 完成时产生的费用信息。
    cost: Option<f64>,
    tokens: Option<OpenCodeStepTokenUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeMessageWithParts {
    info: OpenCodeMessageInfo,
    #[serde(default)]
    parts: Vec<OpenCodePart>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeMessageInfo {
    id: String,
    role: String,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    #[serde(rename = "sessionID", default)]
    session_id: Option<String>,
    #[serde(rename = "modelID", default)]
    model_id: Option<String>,
    #[serde(rename = "providerID", default)]
    provider_id: Option<String>,
    /// OpenCode 消息执行所使用的模式。
    #[serde(default)]
    mode: Option<String>,
    /// OpenCode 消息执行所使用的模型变体，用于还原推理档位。
    #[serde(default)]
    variant: Option<String>,
    /// OpenCode 消息的创建和完成时间。
    #[serde(default)]
    time: Option<OpenCodeMessageTime>,
    /// OpenCode 消息产生的费用。
    #[serde(default)]
    cost: Option<f64>,
    /// OpenCode 消息累计 token 用量。
    #[serde(default)]
    tokens: Option<OpenCodeStepTokenUsage>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenCodeMessageTime {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    completed: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeMessageResponseObject {
    data: Vec<OpenCodeMessageWithParts>,
    #[serde(default)]
    cursor: Option<OpenCodeMessageCursor>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OpenCodeMessageCursor {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    previous: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OpenCodeMessageResponse {
    Array(Vec<OpenCodeMessageWithParts>),
    Object(OpenCodeMessageResponseObject),
}

#[derive(Debug, Clone)]
struct OpenCodeMessagePage {
    messages: Vec<OpenCodeMessageWithParts>,
    continuation: Option<String>,
    uses_cursor: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct OpenCodeStepTokenUsage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: OpenCodeStepTokenCache,
    #[allow(dead_code)]
    total: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct OpenCodeStepTokenCache {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OpenCodeToolState {
    status: String,
    input: Option<Value>,
    raw: Option<String>,
    title: Option<String>,
    output: Option<String>,
    error: Option<String>,
    metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeQuestionInfo {
    question: String,
    header: String,
    #[serde(default)]
    options: Vec<OpenCodeQuestionOption>,
    multiple: Option<bool>,
    custom: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenCodeQuestionOption {
    label: String,
    description: String,
}

#[derive(Debug, Clone)]
struct PendingOpenCodeTextPart {
    message_id: String,
    text: String,
}

struct OpenCodeTurnMapper {
    prompt_message_id: String,
    message_roles: HashMap<String, String>,
    message_parents: HashMap<String, String>,
    emitted_text_by_part_id: HashMap<String, String>,
    pending_text_by_part_id: HashMap<String, PendingOpenCodeTextPart>,
    part_type_by_id: HashMap<String, String>,
    started_actions: HashSet<String>,
    completed_actions: HashSet<String>,
    latest_token_usage: Option<TokenUsage>,
    busy_seen: bool,
    content_seen: bool,
    completed: bool,
    failed: bool,
}

impl OpenCodeTurnMapper {
    fn new(prompt_message_id: String) -> Self {
        Self {
            prompt_message_id,
            message_roles: HashMap::new(),
            message_parents: HashMap::new(),
            emitted_text_by_part_id: HashMap::new(),
            pending_text_by_part_id: HashMap::new(),
            part_type_by_id: HashMap::new(),
            started_actions: HashSet::new(),
            completed_actions: HashSet::new(),
            latest_token_usage: None,
            busy_seen: false,
            content_seen: false,
            completed: false,
            failed: false,
        }
    }

    fn record_message(&mut self, message_id: &str, role: &str, parent_id: Option<&str>) {
        let role = role.trim().to_lowercase();
        self.message_roles
            .insert(message_id.to_string(), role.clone());
        if let Some(parent_id) = parent_id.filter(|value| !value.trim().is_empty()) {
            self.message_parents
                .insert(message_id.to_string(), parent_id.to_string());
        }
        if role == "user" {
            self.remove_pending_text_for_message(message_id);
        }
    }

    fn is_prompt_user_message(&self, message_id: &str) -> bool {
        message_id == self.prompt_message_id || is_user_message(&self.message_roles, message_id)
    }

    fn should_process_part_for_message(&self, message_id: &str) -> bool {
        if self.is_prompt_user_message(message_id) {
            return false;
        }
        self.message_parents
            .get(message_id)
            .map(|parent_id| parent_id == &self.prompt_message_id)
            .unwrap_or(true)
    }

    fn store_pending_text(&mut self, part_id: &str, message_id: &str, text: &str) {
        self.pending_text_by_part_id
            .entry(part_id.to_string())
            .and_modify(|pending| {
                pending.message_id = message_id.to_string();
                pending.text.push_str(text);
            })
            .or_insert_with(|| PendingOpenCodeTextPart {
                message_id: message_id.to_string(),
                text: text.to_string(),
            });
    }

    fn remove_pending_text_for_message(&mut self, message_id: &str) {
        self.pending_text_by_part_id
            .retain(|_, pending| pending.message_id != message_id);
    }
}

async fn emit_opencode_part_delta(
    mapper: &mut OpenCodeTurnMapper,
    event_tx: &mpsc::Sender<EngineEvent>,
    part_id: &str,
    part_type: &str,
    delta: &str,
) {
    if delta.is_empty() {
        return;
    }

    mapper.content_seen = true;
    mapper
        .emitted_text_by_part_id
        .entry(part_id.to_string())
        .and_modify(|existing| existing.push_str(delta))
        .or_insert_with(|| delta.to_string());

    let event = if part_type == "reasoning" {
        EngineEvent::ThinkingDelta {
            content: delta.to_string(),
        }
    } else {
        EngineEvent::TextDelta {
            content: delta.to_string(),
        }
    };
    event_tx.send(event).await.ok();
}

async fn emit_opencode_part_snapshot(
    mapper: &mut OpenCodeTurnMapper,
    event_tx: &mpsc::Sender<EngineEvent>,
    part_id: &str,
    part_type: &str,
    text: &str,
) {
    let previous = mapper
        .emitted_text_by_part_id
        .get(part_id)
        .map(String::as_str)
        .unwrap_or("");
    let Some(delta) = text.strip_prefix(previous) else {
        if !previous.is_empty() {
            log::debug!(
                "ignoring non-append OpenCode text snapshot for part {part_id}; previous_len={}, next_len={}",
                previous.len(),
                text.len()
            );
            return;
        }
        return emit_opencode_part_delta(mapper, event_tx, part_id, part_type, text).await;
    };
    if delta.is_empty() {
        return;
    }
    mapper
        .emitted_text_by_part_id
        .insert(part_id.to_string(), text.to_string());
    mapper.content_seen = true;
    let event = if part_type == "reasoning" {
        EngineEvent::ThinkingDelta {
            content: delta.to_string(),
        }
    } else {
        EngineEvent::TextDelta {
            content: delta.to_string(),
        }
    };
    event_tx.send(event).await.ok();
}

async fn flush_pending_opencode_text_for_part(
    mapper: &mut OpenCodeTurnMapper,
    event_tx: &mpsc::Sender<EngineEvent>,
    part_id: &str,
) {
    let Some(pending) = mapper.pending_text_by_part_id.remove(part_id) else {
        return;
    };
    if mapper.is_prompt_user_message(&pending.message_id) {
        return;
    }
    let Some(part_type) = mapper.part_type_by_id.get(part_id).cloned() else {
        mapper
            .pending_text_by_part_id
            .insert(part_id.to_string(), pending);
        return;
    };
    emit_opencode_part_delta(mapper, event_tx, part_id, &part_type, &pending.text).await;
}

async fn emit_turn_completed(
    mapper: &mut OpenCodeTurnMapper,
    event_tx: &mpsc::Sender<EngineEvent>,
    status: TurnCompletionStatus,
) {
    if mapper.completed {
        return;
    }

    mapper.completed = true;
    let token_usage = if status == TurnCompletionStatus::Completed {
        mapper.latest_token_usage.clone()
    } else {
        None
    };
    event_tx
        .send(EngineEvent::TurnCompleted {
            token_usage,
            status,
        })
        .await
        .ok();
}

impl Default for OpenCodeEngine {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Local,
        }
    }
}

#[async_trait]
impl Engine for OpenCodeEngine {
    fn id(&self) -> &str {
        "opencode"
    }

    fn name(&self) -> &str {
        "OpenCode"
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![model_info(
            "opencode/big-pickle",
            "OpenCode Big Pickle",
            "Default OpenCode-hosted coding model.",
            true,
            reasoning_efforts_from_variant_names(&["high", "max"]),
            vec!["text".to_string()],
        )]
    }

    async fn is_available(&self) -> bool {
        !matches!(&self.target, OpenCodeTransportTarget::Local)
    }

    async fn start_thread(
        &self,
        scope: ThreadScope,
        resume_engine_thread_id: Option<&str>,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<EngineThread> {
        let cwd = scope_cwd(&scope);
        let parsed_model = parse_model_slug(model)
            .with_context(|| format!("OpenCode model `{model}` must use provider/model format"))?;
        let _ = parsed_model;
        let permission_mode = permission_mode_from_policy(sandbox.approval_policy.as_ref());
        let reasoning_effort = self
            .resolve_session_reasoning_effort(&cwd, model, sandbox.reasoning_effort.as_deref())
            .await;
        let agent = normalize_opencode_agent(sandbox.opencode_agent.as_deref());

        if let Some(existing_id) = resume_engine_thread_id {
            let mut state = self.state.lock().await;
            if let Some(existing) = state.sessions.get(existing_id).cloned() {
                if existing.cwd == cwd && existing.permission_mode == permission_mode {
                    if let Some(existing) = state.sessions.get_mut(existing_id) {
                        existing.model_id = model.to_string();
                        existing.reasoning_effort = reasoning_effort.clone();
                        existing.agent = agent.clone();
                    }
                    return Ok(EngineThread {
                        engine_thread_id: existing_id.to_string(),
                    });
                }

                if existing.cwd == cwd {
                    if self.is_remote_target() {
                        anyhow::bail!(
                            "SSH 远端 OpenCode 会话权限模式与当前设置不一致；不会创建替代会话"
                        );
                    }
                    state.sessions.remove(existing_id);
                    drop(state);
                    let engine_thread_id = self
                        .create_session(existing.connection.as_ref(), permission_mode)
                        .await?;
                    self.state.lock().await.sessions.insert(
                        engine_thread_id.clone(),
                        OpenCodeSession {
                            cwd,
                            model_id: model.to_string(),
                            reasoning_effort,
                            agent,
                            permission_mode,
                            connection: existing.connection,
                        },
                    );
                    return Ok(EngineThread { engine_thread_id });
                }

                if self.is_remote_target() {
                    anyhow::bail!(
                        "SSH 远端 OpenCode 会话目录不匹配；不会在其他目录或本机创建替代会话"
                    );
                }
            }
        }

        let connection = self.connection_for_cwd(&cwd).await?;
        let engine_thread_id = match resume_engine_thread_id {
            Some(existing_id) => match self.get_session(connection.as_ref(), existing_id).await {
                Ok(session)
                    if session.directory == cwd
                        && (session_permission_matches(&session, permission_mode)
                            || (self.is_remote_target()
                                && session.permission.is_none()
                                && permission_mode == OpenCodePermissionMode::Ask)) =>
                {
                    existing_id.to_string()
                }
                Ok(session) => {
                    if self.is_remote_target() {
                        anyhow::bail!(
                            "SSH 远端 OpenCode 会话恢复失败：session={} expected_directory={} actual_directory={}；不会创建替代会话",
                            existing_id,
                            cwd,
                            session.directory
                        );
                    }
                    log::warn!(
                        "opencode session {existing_id} permission rules differ from requested mode; creating a new session"
                    );
                    self.create_session(connection.as_ref(), permission_mode)
                        .await?
                }
                Err(error) => {
                    if self.is_remote_target() {
                        return Err(error).context(
                            "SSH 远端 OpenCode 会话恢复失败；不会在远端或本机创建替代会话",
                        );
                    }
                    log::warn!(
                        "opencode session resume failed for {existing_id}, creating a new session: {error}"
                    );
                    self.create_session(connection.as_ref(), permission_mode)
                        .await?
                }
            },
            None => {
                self.create_session(connection.as_ref(), permission_mode)
                    .await?
            }
        };

        {
            let mut state = self.state.lock().await;
            let _ = state.sessions.insert(
                engine_thread_id.clone(),
                OpenCodeSession {
                    cwd: cwd.clone(),
                    model_id: model.to_string(),
                    reasoning_effort,
                    agent,
                    permission_mode,
                    connection,
                },
            );
        }

        Ok(EngineThread { engine_thread_id })
    }

    async fn send_message(
        &self,
        engine_thread_id: &str,
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let session = {
            let state = self.state.lock().await;
            state
                .sessions
                .get(engine_thread_id)
                .cloned()
                .context("no OpenCode session found; was start_thread called?")?
        };

        // Subscribe to the persistent event bus BEFORE firing the prompt.
        // broadcast::Receiver does not replay history; it only delivers events
        // emitted after subscribe(). This eliminates the cross-turn replay race
        // that caused follow-up turns to immediately complete with no content
        // when a fresh `/event` HTTP connection delivered the prior turn's
        // buffered busy/idle events into the new turn's mapper.
        let mut incoming_rx = spawn_opencode_incoming_pump(session.connection.event_bus.clone());

        let prompt = build_prompt_body(
            &session.model_id,
            session.reasoning_effort.as_deref(),
            session.agent.as_deref(),
            input,
        )?;
        let prompt_message_id = prompt.message_id.clone();
        log::info!(
            "OpenCode event subscription established: engine_thread_id={}, prompt_message_id={}, transport={}, subscribed=true",
            engine_thread_id,
            prompt_message_id,
            if self.is_remote_target() { "remote" } else { "local" },
        );
        let prompt_request =
            self.prompt_message(engine_thread_id, session.connection.as_ref(), prompt.body);
        tokio::pin!(prompt_request);

        let mut mapper = OpenCodeTurnMapper::new(prompt_message_id);
        let mut last_relevant_event_at = Instant::now();

        loop {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    self.interrupt(engine_thread_id).await?;
                    return Ok(());
                }
                result = &mut prompt_request => {
                    match result {
                        Ok(()) => {
                            if !mapper.completed {
                                self.reconcile_session_messages(
                                    engine_thread_id,
                                    &mut mapper,
                                    &event_tx,
                                    session.connection.as_ref(),
                                )
                                .await;
                                self.complete_after_idle(&mut mapper, &event_tx).await;
                            }
                            return Ok(());
                        }
                        Err(error) => {
                            if mapper.completed {
                                log::warn!(
                                    "OpenCode /message request finished with an error after turn completion: {error:#}"
                                );
                                return Ok(());
                            }
                            event_tx
                                .send(EngineEvent::Error {
                                    message: format!("failed to send OpenCode prompt: {error:#}"),
                                    recoverable: false,
                                })
                                .await
                                .ok();
                            emit_turn_completed(
                                &mut mapper,
                                &event_tx,
                                TurnCompletionStatus::Failed,
                            )
                            .await;
                            return Err(error);
                        }
                    }
                }
                incoming = timeout(SSE_IDLE_TIMEOUT, incoming_rx.recv()) => {
                    let event = match incoming.context("timed out waiting for OpenCode events")? {
                        Some(OpenCodeIncomingEvent::Message(event)) => event,
                        Some(OpenCodeIncomingEvent::Failure(message)) => {
                            anyhow::bail!("OpenCode 事件监听失败：{message}");
                        }
                        Some(OpenCodeIncomingEvent::Lagged(skipped)) => {
                            anyhow::bail!(
                                "OpenCode 事件监听丢失了 {skipped} 条本轮事件"
                            );
                        }
                        None | Some(OpenCodeIncomingEvent::Closed) => {
                            anyhow::bail!("OpenCode event bus closed before the turn completed");
                        }
                    };
                    let matched = event_matches_session(event.as_ref(), engine_thread_id);
                    log::debug!(
                        "OpenCode SSE event received: event_type={}, session_id={:?}, engine_thread_id={}, matched={}",
                        event.event_type,
                        event_session_id(event.as_ref()),
                        engine_thread_id,
                        matched,
                    );
                    if matched {
                        last_relevant_event_at = Instant::now();
                        self.handle_event(
                            engine_thread_id,
                            event.as_ref(),
                            &mut mapper,
                            &event_tx,
                            session.connection.clone(),
                            matched,
                        )
                        .await;
                    } else if last_relevant_event_at.elapsed() > SSE_IDLE_TIMEOUT {
                        anyhow::bail!("timed out waiting for OpenCode turn events");
                    }
                    if mapper.completed {
                        match timeout(OPENCODE_COMMAND_TIMEOUT, &mut prompt_request).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                log::warn!(
                                    "OpenCode /message request errored after turn completion: {error:#}"
                                );
                            }
                            Err(_) => {
                                log::warn!(
                                    "timed out draining OpenCode /message response after turn completion"
                                );
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn steer_message(
        &self,
        _engine_thread_id: &str,
        _client_steer_id: &str,
        _content: &str,
        _input: TurnInput,
    ) -> Result<EngineSteerReceipt> {
        anyhow::bail!("Mid-turn steering is not supported for OpenCode")
    }

    async fn respond_to_approval(
        &self,
        approval_id: &str,
        response: Value,
        route: Option<ApprovalRequestRoute>,
    ) -> Result<()> {
        let normalized = normalize_approval_response_for_engine("opencode", response)
            .map_err(anyhow::Error::msg)?;
        let pending = {
            let state = self.state.lock().await;
            state.pending_requests.get(approval_id).cloned()
        };
        let pending = match pending {
            Some(pending) => pending,
            None => self
                .pending_request_from_route(route)
                .await
                .with_context(|| {
                    format!("OpenCode approval request `{approval_id}` was not found")
                })?,
        };

        match pending {
            PendingOpenCodeRequest::Permission { request_id, connection } => {
                let decision = normalized
                    .get("decision")
                    .and_then(Value::as_str)
                    .unwrap_or("decline");
                let reply = match decision {
                    "accept" => "once",
                    "accept_for_session" => "always",
                    "decline" | "cancel" => "reject",
                    _ => "reject",
                };
                self.request(
                    connection.as_ref(),
                    reqwest::Method::POST,
                    &format!("/permission/{request_id}/reply"),
                )
                .json(&json!({ "reply": reply }))
                .send()
                .await?
                .error_for_status()
                .context("failed to reply to OpenCode permission request")?;
            }
            PendingOpenCodeRequest::Question {
                request_id,
                questions,
                connection,
            } => {
                if should_reject_question_response(&normalized) {
                    self.request(
                        connection.as_ref(),
                        reqwest::Method::POST,
                        &format!("/question/{request_id}/reject"),
                    )
                    .send()
                    .await?
                    .error_for_status()
                    .context("failed to reject OpenCode question request")?;
                } else {
                    let answers = build_question_answers(&questions, normalized.get("answers"));
                    self.request(
                        connection.as_ref(),
                        reqwest::Method::POST,
                        &format!("/question/{request_id}/reply"),
                    )
                    .json(&json!({ "answers": answers }))
                    .send()
                    .await?
                    .error_for_status()
                    .context("failed to reply to OpenCode question request")?;
                }
            }
        }

        self.state.lock().await.pending_requests.remove(approval_id);
        // OpenCode 服务由 CLI 生命周期持有，审批处理完成后不在协议客户端中停止。
        Ok(())
    }

    async fn interrupt(&self, engine_thread_id: &str) -> Result<()> {
        let session = {
            let state = self.state.lock().await;
            state.sessions.get(engine_thread_id).cloned()
        };
        let Some(session) = session else {
            return Ok(());
        };

        self.request(
            session.connection.as_ref(),
            reqwest::Method::POST,
            &format!("/session/{engine_thread_id}/abort"),
        )
        .send()
        .await?
        .error_for_status()
        .context("failed to abort OpenCode session")?;
        Ok(())
    }

    async fn archive_thread(&self, engine_thread_id: &str) -> Result<()> {
        let removed = self.state.lock().await.sessions.remove(engine_thread_id);
        if let Some(session) = removed {
            self.patch_session_archive(
                session.connection.as_ref(),
                engine_thread_id,
                Some(current_unix_time_millis()),
            )
            .await?;
            // OpenCode 服务由 CLI 生命周期持有，归档业务完成后不在协议客户端中停止。
        }
        Ok(())
    }

    async fn unarchive_thread(&self, _engine_thread_id: &str) -> Result<()> {
        Ok(())
    }
}

impl OpenCodeEngine {
    /// 创建绑定本机 CLI 生命周期 endpoint 的纯 OpenCode 协议客户端。
    pub fn new_local_http(
        base_url: String,
        password: String,
        version: Option<String>,
        cwd: String,
        generation: u64,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::LocalHttp(Arc::new(LocalOpenCodeEndpoint {
                cwd,
                base_url,
                password,
                version,
                generation,
            })),
        }
    }

    pub fn set_computer_control_service(&self, service: Arc<ComputerControlService>) {
        if let Ok(mut current) = self.computer_control_service.lock() {
            *current = Some(service);
        }
    }

    /// 设置 AuraCoder 本地会话读取工具服务。
    pub fn set_auracoder_thread_mcp_service(&self, service: Arc<AuraCoderThreadMcpService>) {
        if let Ok(mut current) = self.auracoder_thread_mcp_service.lock() {
            *current = Some(service);
        }
    }

    pub fn new_remote_http(base_url: String, password: String) -> Self {
        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        let pump_cancel = CancellationToken::new();
        Self {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url,
                password,
                event_bus,
                pump_cancel,
                #[cfg(test)]
                workspace_event_pump_enabled: true,
            })),
        }
    }

    fn is_remote_target(&self) -> bool {
        matches!(&self.target, OpenCodeTransportTarget::Remote(_))
    }

    /// 获取当前 cwd 对应的纯协议连接；本机服务必须已经由 CLI 生命周期提供。
    async fn connection_for_cwd(&self, cwd: &str) -> Result<Arc<OpenCodeConnection>> {
        if let Some(connection) = self.state.lock().await.connections.get(cwd).cloned() {
            return Ok(connection);
        }

        let created = Arc::new(match &self.target {
            OpenCodeTransportTarget::Local => {
                anyhow::bail!("OpenCode 本机服务 endpoint 尚未由 CLI 生命周期提供")
            }
            OpenCodeTransportTarget::LocalHttp(endpoint) => {
                anyhow::ensure!(
                    endpoint.cwd == cwd,
                    "OpenCode 本机 endpoint 与请求项目目录不一致"
                );
                let (event_bus, _) =
                    broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
                let pump_cancel = CancellationToken::new();
                tokio::spawn(run_event_pump(
                    endpoint.base_url.clone(),
                    endpoint.password.clone(),
                    None,
                    reqwest::Client::new(),
                    event_bus.clone(),
                    pump_cancel.clone(),
                ));
                OpenCodeConnection {
                    cwd: cwd.to_string(),
                    base_url: endpoint.base_url.clone(),
                    password: endpoint.password.clone(),
                    event_bus,
                    pump_cancel: Some(pump_cancel),
                    callback_cancel: None,
                    include_directory_header: false,
                }
            }
            OpenCodeTransportTarget::Remote(endpoint) => {
                let include_directory_header = !cwd.is_empty();
                #[cfg(test)]
                let workspace_event_pump_enabled = endpoint.workspace_event_pump_enabled;
                #[cfg(not(test))]
                let workspace_event_pump_enabled = true;
                let (event_bus, pump_cancel) =
                    if include_directory_header && workspace_event_pump_enabled {
                        let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(
                            OPENCODE_EVENT_BUFFER_CAPACITY,
                        );
                        let pump_cancel = CancellationToken::new();
                        tokio::spawn(run_event_pump(
                            endpoint.base_url.clone(),
                            endpoint.password.clone(),
                            Some(cwd.to_string()),
                            reqwest::Client::new(),
                            event_bus.clone(),
                            pump_cancel.clone(),
                        ));
                        (event_bus, Some(pump_cancel))
                    } else {
                        (endpoint.event_bus.clone(), None)
                    };
                OpenCodeConnection {
                    cwd: cwd.to_string(),
                    base_url: endpoint.base_url.clone(),
                    password: endpoint.password.clone(),
                    event_bus,
                    pump_cancel,
                    callback_cancel: None,
                    include_directory_header,
                }
            }
        });
        let existing = {
            let mut state = self.state.lock().await;
            if let Some(connection) = state.connections.get(cwd).cloned() {
                Some(connection)
            } else {
                state.connections.insert(cwd.to_string(), created.clone());
                None
            }
        };

        if let Some(existing) = existing {
            created.cancel_event_pump();
            Ok(existing)
        } else {
            Ok(created)
        }
    }

    async fn pending_request_from_route(
        &self,
        route: Option<ApprovalRequestRoute>,
    ) -> Result<PendingOpenCodeRequest> {
        let route = route.context("missing persisted OpenCode approval route")?;
        let details = route
            .raw_request_id
            .as_object()
            .context("invalid persisted OpenCode approval route")?;
        let request_id = details
            .get("requestID")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("persisted OpenCode approval route is missing requestID")?;
        let cwd = details
            .get("cwd")
            .and_then(Value::as_str)
            .context("persisted OpenCode approval route is missing cwd")?;
        let connection = self.connection_for_cwd(cwd).await?;

        match route.server_method.as_str() {
            "opencode/permission" => Ok(PendingOpenCodeRequest::Permission { request_id, connection }),
            "opencode/question" => {
                let questions = details
                    .get("questions")
                    .cloned()
                    .and_then(|value| {
                        serde_json::from_value::<Vec<OpenCodeQuestionInfo>>(value).ok()
                    })
                    .unwrap_or_default();
                Ok(PendingOpenCodeRequest::Question {
                    request_id,
                    questions,
                    connection,
                })
            }
            method => anyhow::bail!("unsupported OpenCode approval route `{method}`"),
        }
    }

    /// 通过已绑定的 HTTP endpoint 检查 OpenCode 服务，不启动或探测本机进程。
    pub async fn prewarm(&self) -> Result<()> {
        let (base_url, password, label) = match &self.target {
            OpenCodeTransportTarget::Local => {
                anyhow::bail!("OpenCode 本机服务 endpoint 尚未由 CLI 生命周期提供")
            }
            OpenCodeTransportTarget::LocalHttp(endpoint) => (
                endpoint.base_url.clone(),
                endpoint.password.clone(),
                "本机",
            ),
            OpenCodeTransportTarget::Remote(endpoint) => (
                endpoint.base_url.clone(),
                endpoint.password.clone(),
                "SSH 远端",
            ),
        };
        let response = self
            .http
            .get(format!("{}/global/health", base_url.trim_end_matches('/')))
            .headers(auth_headers(&password))
            .send()
            .await?
            .error_for_status()?
            .json::<OpenCodeHealthResponse>()
            .await?;
        anyhow::ensure!(response.healthy, "{label} OpenCode 服务健康检查失败");
        Ok(())
    }

    /// 读取已绑定 OpenCode HTTP endpoint 的健康状态，不执行本机 CLI 命令。
    pub async fn health_report(&self) -> OpenCodeHealthReport {
        let (base_url, password, version, details) = match &self.target {
            OpenCodeTransportTarget::Local => {
                return OpenCodeHealthReport {
                    available: false,
                    version: None,
                    details: Some("OpenCode 本机服务尚未由 CLI 生命周期提供 endpoint".to_string()),
                    warnings: vec![],
                    checks: vec!["OpenCode CLI 生命周期 endpoint".to_string()],
                    fixes: vec!["先登记并预热本机 OpenCode CLI 服务".to_string()],
                };
            }
            OpenCodeTransportTarget::LocalHttp(endpoint) => (
                endpoint.base_url.clone(),
                endpoint.password.clone(),
                endpoint.version.clone(),
                format!("本机 OpenCode HTTP endpoint，generation={}", endpoint.generation),
            ),
            OpenCodeTransportTarget::Remote(endpoint) => (
                endpoint.base_url.clone(),
                endpoint.password.clone(),
                None,
                "SSH 远端 OpenCode HTTP endpoint".to_string(),
            ),
        };
        let response = self
            .http
            .get(format!("{}/global/health", base_url.trim_end_matches('/')))
            .headers(auth_headers(&password))
            .send()
            .await;
        match response {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<OpenCodeHealthResponse>().await {
                    Ok(health) if health.healthy => OpenCodeHealthReport {
                        available: true,
                        version: version.clone(),
                        details: Some(details),
                        warnings: vec![],
                        checks: vec!["GET /global/health".to_string()],
                        fixes: vec![],
                    },
                    Ok(_) => OpenCodeHealthReport {
                        available: false,
                        version: version.clone(),
                        details: Some("OpenCode HTTP 服务返回 unhealthy".to_string()),
                        warnings: vec![],
                        checks: vec!["GET /global/health".to_string()],
                        fixes: vec![],
                    },
                    Err(error) => OpenCodeHealthReport {
                        available: false,
                        version: version.clone(),
                        details: Some(format!("解析 OpenCode 健康响应失败: {error}")),
                        warnings: vec![],
                        checks: vec!["GET /global/health".to_string()],
                        fixes: vec![],
                    },
                },
                Err(error) => OpenCodeHealthReport {
                    available: false,
                    version: version.clone(),
                    details: Some(format!("OpenCode 健康请求返回 HTTP 错误: {error}")),
                    warnings: vec![],
                    checks: vec!["GET /global/health".to_string()],
                    fixes: vec![],
                },
            },
            Err(error) => OpenCodeHealthReport {
                available: false,
                version: version.clone(),
                details: Some(format!("连接 OpenCode 健康 endpoint 失败: {error}")),
                warnings: vec![],
                checks: vec!["GET /global/health".to_string()],
                fixes: vec![],
            },
        }
    }

    pub async fn list_models_runtime(&self) -> Vec<ModelInfo> {
        let cwd = match &self.target {
            OpenCodeTransportTarget::LocalHttp(endpoint) => endpoint.cwd.as_str(),
            OpenCodeTransportTarget::Local | OpenCodeTransportTarget::Remote(_) => "",
        };
        self.list_models_runtime_for_cwd(cwd).await
    }

    /// 将 OpenCode `models --verbose` 或 `models` 的纯文本输出解析为模型目录。
    pub fn parse_cli_model_output(output: &str) -> Result<Vec<ModelInfo>> {
        let records = parse_verbose_model_records(output)?;
        if !records.is_empty() {
            let mut models = Vec::new();
            for (index, record) in records.into_iter().enumerate() {
                if record.status.as_deref() == Some("deprecated") {
                    continue;
                }
                let slug = format!("{}/{}", record.provider_id, record.id);
                if parse_model_slug(&slug).is_none() {
                    continue;
                }
                let modalities = model_modalities_from_capabilities(record.capabilities.as_ref());
                let attachment_modalities =
                    attachment_modalities_from_capabilities(record.capabilities.as_ref());
                models.push(model_info_with_metadata(
                    &slug,
                    &record.name,
                    "OpenCode model",
                    index == 0,
                    reasoning_efforts_from_variants(&record.variants),
                    modalities,
                    attachment_modalities,
                    model_limits(record.limit.as_ref()),
                ));
            }
            return Ok(models);
        }

        let mut models = Vec::new();
        for (index, line) in output.lines().enumerate() {
            let slug = line.trim();
            if parse_model_slug(slug).is_none() {
                continue;
            }
            models.push(model_info(
                slug,
                slug,
                "OpenCode model",
                index == 0,
                Vec::new(),
                vec!["text".to_string()],
            ));
        }
        Ok(models)
    }

    pub async fn list_models_runtime_for_cwd(&self, cwd: &str) -> Vec<ModelInfo> {
        {
            let state = self.state.lock().await;
            if let Some(cache) = state.runtime_model_cache.clone() {
                return cache;
            }
        }

        let models = match self.connection_for_cwd(cwd).await {
            Ok(connection) => match self.load_models_from_provider_endpoint(connection.as_ref()).await {
                Ok(models) if !models.is_empty() => models,
                Ok(_) => self.models(),
                Err(error) => {
                    log::warn!("读取 OpenCode provider 模型目录失败: {error:#}");
                    self.models()
                }
            },
            Err(error) => {
                log::debug!("OpenCode endpoint 未绑定，使用静态模型目录: {error:#}");
                self.models()
            }
        };

        if should_cache_runtime_model_catalog(&models) {
            self.state.lock().await.runtime_model_cache = Some(models.clone());
        } else {
            log::info!(
                "not caching opencode-only model catalog; provider environment may change while AuraCoder is running"
            );
        }
        models
    }

    pub async fn runtime_model_fallback(&self) -> Vec<ModelInfo> {
        self.state
            .lock()
            .await
            .runtime_model_cache
            .clone()
            .unwrap_or_else(|| self.models())
    }

    pub async fn runtime_catalog(&self, cwd: &str) -> Result<OpenCodeRuntimeCatalogDto> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = async {
            let agents = self
                .request(connection.as_ref(), reqwest::Method::GET, "/agent")
                .send()
                .await?
                .error_for_status()
                .context("failed to list OpenCode agents")?
                .json::<Vec<OpenCodeRuntimeAgent>>()
                .await
                .context("failed to parse OpenCode agents")?;

            let commands = self
                .request(connection.as_ref(), reqwest::Method::GET, "/command")
                .send()
                .await?
                .error_for_status()
                .context("failed to list OpenCode commands")?
                .json::<Vec<OpenCodeRuntimeCommand>>()
                .await
                .context("failed to parse OpenCode commands")?;

            let mcp = self
                .request(connection.as_ref(), reqwest::Method::GET, "/mcp")
                .send()
                .await?
                .error_for_status()
                .context("failed to read OpenCode MCP status")?
                .json::<HashMap<String, Value>>()
                .await
                .context("failed to parse OpenCode MCP status")?;

            Ok(OpenCodeRuntimeCatalogDto {
                agents: map_runtime_agents(agents),
                commands: map_runtime_commands(commands),
                mcp_servers: map_runtime_mcp_connections(mcp),
            })
        }
        .await;
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    pub async fn list_sessions(
        &self,
        cwd: &str,
        search_term: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Vec<OpenCodeRemoteSessionSummary>> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = async {
            let mut query = vec![
                ("directory", cwd.to_string()),
                ("roots", "true".to_string()),
                ("limit", "200".to_string()),
            ];
            if let Some(search_term) = search_term.map(str::trim).filter(|value| !value.is_empty())
            {
                query.push(("search", search_term.to_string()));
            }

            let sessions = self
                .request(connection.as_ref(), reqwest::Method::GET, "/session")
                .query(&query)
                .send()
                .await?
                .error_for_status()
                .context("failed to list OpenCode sessions")?
                .json::<Vec<OpenCodeSessionRecord>>()
                .await
                .context("failed to parse OpenCode sessions")?;

            let mut summaries = sessions
                .into_iter()
                .map(map_session_record)
                .filter(|session| {
                    archived
                        .map(|expected| session.archived == expected)
                        .unwrap_or(true)
                })
                .collect::<Vec<_>>();
            summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            Ok(summaries)
        }
        .await;
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    pub async fn read_session(
        &self,
        cwd: &str,
        session_id: &str,
    ) -> Result<OpenCodeRemoteSessionSummary> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = async {
            // 旧实现仅作架构迁移留痕，禁止恢复执行：
            // let session = self
            //     .request(
            //         connection.as_ref(),
            //         reqwest::Method::GET,
            //         &format!("/session/{session_id}"),
            //     )
            //     .query(&[("directory", cwd)])
            //     .send()
            //     .await?
            //     .error_for_status()
            //     .context("failed to read OpenCode session")?
            //     .json::<OpenCodeSessionRecord>()
            //     .await
            //     .context("failed to parse OpenCode session")?;
            // OpenCode 按 ID 查询只需要路径和现有 Basic 认证；这里不能附加目录头或 query。
            let url = format!(
                "{}/session/{session_id}",
                connection.base_url.trim_end_matches('/')
            );
            let session = self
                .http
                .request(reqwest::Method::GET, url)
                .headers(auth_headers(&connection.password))
                .send()
                .await?
                .error_for_status()
                .context("failed to read OpenCode session")?
                .json::<OpenCodeSessionRecord>()
                .await
                .context("failed to parse OpenCode session")?;
            Ok(map_session_record(session))
        }
        .await;
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    pub async fn abort_session(&self, cwd: &str, session_id: &str) -> Result<()> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = self
            .request(
                connection.as_ref(),
                reqwest::Method::POST,
                &format!("/session/{session_id}/abort"),
            )
            .send()
            .await?
            .error_for_status()
            .context("failed to abort OpenCode session")
            .map(|_| ());
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    pub async fn set_session_archived(
        &self,
        cwd: &str,
        session_id: &str,
        archived: bool,
    ) -> Result<()> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = self
            .patch_session_archive(
                connection.as_ref(),
                session_id,
                Some(if archived {
                    current_unix_time_millis()
                } else {
                    0
                }),
            )
            .await;

        if result.is_ok() {
            let _ = self.state.lock().await.sessions.remove(session_id);
        }
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    pub async fn forget_session(&self, session_id: &str) {
        let _ = self.state.lock().await.sessions.remove(session_id);
    }

    async fn resolve_session_reasoning_effort(
        &self,
        cwd: &str,
        model_id: &str,
        requested_effort: Option<&str>,
    ) -> Option<String> {
        let models = self.list_models_runtime_for_cwd(cwd).await;
        let model = models.iter().find(|model| model.id == model_id)?;
        resolve_model_reasoning_effort(model, requested_effort)
    }

    /*
    旧实现通过本机 `opencode models` 命令读取模型目录，现由 HTTP provider 协议替代：
    async fn load_models_from_verbose_command(&self) -> Result<Vec<ModelInfo>> {
        // 迁移留痕：不得在 OpenCodeEngine 中执行本机 CLI 命令。
        anyhow::bail!("OpenCode 模型目录必须通过 HTTP provider 协议读取")
    }

    async fn load_models_from_command(&self) -> Result<Vec<ModelInfo>> {
        // 迁移留痕：不得在 OpenCodeEngine 中执行本机 CLI 命令。
        anyhow::bail!("OpenCode 模型目录必须通过 HTTP provider 协议读取")
    }
    */

    #[allow(dead_code)]
    async fn load_models_from_provider_endpoint(
        &self,
        connection: &OpenCodeConnection,
    ) -> Result<Vec<ModelInfo>> {
        let list = self
            .request(connection, reqwest::Method::GET, "/provider")
            .send()
            .await?
            .error_for_status()?
            .json::<OpenCodeProviderList>()
            .await?;
        let connected: HashSet<&str> = list.connected.iter().map(String::as_str).collect();
        let mut models = Vec::new();

        for provider in list.all {
            if !connected.contains(provider.id.as_str()) {
                continue;
            }
            for model in provider.models.values() {
                if model.status.as_deref() == Some("deprecated") {
                    continue;
                }
                let slug = format!("{}/{}", provider.id, model.id);
                let modalities = model_modalities(model);
                let attachment_modalities =
                    attachment_modalities_from_capabilities(model.capabilities.as_ref());
                models.push(model_info_with_metadata(
                    &slug,
                    &model.name,
                    &format!("{} model via OpenCode", provider.name),
                    false,
                    reasoning_efforts_from_variants(&model.variants),
                    modalities,
                    attachment_modalities,
                    model_limits(model.limit.as_ref()),
                ));
            }
        }
        if let Some(first) = models.first_mut() {
            first.is_default = true;
        }
        Ok(models)
    }

    async fn create_session(
        &self,
        connection: &OpenCodeConnection,
        permission_mode: OpenCodePermissionMode,
    ) -> Result<String> {
        let session = self
            .request(connection, reqwest::Method::POST, "/session")
            .json(&json!({
                "permission": permission_rules(permission_mode),
            }))
            .send()
            .await?
            .error_for_status()
            .context("failed to create OpenCode session")?
            .json::<OpenCodeSessionInfo>()
            .await
            .context("failed to parse OpenCode session response")?;
        Ok(session.id)
    }

    async fn get_session(
        &self,
        connection: &OpenCodeConnection,
        session_id: &str,
    ) -> Result<OpenCodeSessionRecord> {
        let session = self
            .request(
                connection,
                reqwest::Method::GET,
                &format!("/session/{session_id}"),
            )
            .send()
            .await?
            .error_for_status()
            .context("failed to read OpenCode session")?
            .json::<OpenCodeSessionRecord>()
            .await
            .context("failed to parse OpenCode session")?;
        Ok(session)
    }

    async fn patch_session_archive(
        &self,
        connection: &OpenCodeConnection,
        session_id: &str,
        archived: Option<i64>,
    ) -> Result<()> {
        self.request(
            connection,
            reqwest::Method::PATCH,
            &format!("/session/{session_id}"),
        )
        .json(&json!({
            "time": {
                "archived": archived,
            },
        }))
        .send()
        .await?
        .error_for_status()
        .context("failed to update OpenCode session archive state")?;
        Ok(())
    }

    /// 将权限模式同步到当前已经存在的 OpenCode session。
    ///
    /// 该方法只 PATCH 指定 session，不创建替代 session，也不会修改其模型、思考
    /// 强度或 agent；只有远端确认成功后才更新引擎内的 session 缓存。
    pub async fn set_session_permission_mode(
        &self,
        cwd: &str,
        session_id: &str,
        approval_policy: &Value,
    ) -> Result<()> {
        let mode = permission_mode_from_policy(Some(approval_policy));
        self.set_session_permission_rules(cwd, session_id, &permission_rules(mode))
            .await
    }

    /// 将完整 OpenCode 权限规则原样同步到已有 session。
    pub async fn set_session_permission_rules(
        &self,
        cwd: &str,
        session_id: &str,
        rules: &Value,
    ) -> Result<()> {
        let rules_array = rules
            .as_array()
            .context("OpenCode 权限规则必须是数组")?;
        let action = rules_array.iter().rev().find_map(|rule| {
            (rule.get("permission").and_then(Value::as_str) == Some("*")
                && rule.get("pattern").and_then(Value::as_str) == Some("*"))
                .then(|| rule.get("action").and_then(Value::as_str))
                .flatten()
        });
        if !rules_array.is_empty() {
            let action = action.context("OpenCode 权限规则缺少最后匹配的全局规则")?;
            anyhow::ensure!(matches!(action, "allow" | "ask" | "deny"), "OpenCode 全局权限 action 无效");
        }
        let connection = self.connection_for_cwd(cwd).await?;
        self.request(
            connection.as_ref(),
            reqwest::Method::PATCH,
            &format!("/session/{session_id}"),
        )
        .json(&json!({
            "permission": rules,
        }))
        .send()
        .await
        .with_context(|| format!("PATCH OpenCode session permission failed: session_id={session_id}"))?
        .error_for_status()
        .with_context(|| {
            format!(
                "PATCH OpenCode session permission returned an error: cwd={cwd} session_id={session_id}"
            )
        })?;

        let mut state = self.state.lock().await;
        if let Some(session) = state.sessions.get_mut(session_id) {
            session.permission_mode = match action.unwrap_or("ask") {
                "allow" => OpenCodePermissionMode::Allow,
                "deny" => OpenCodePermissionMode::Deny,
                _ => OpenCodePermissionMode::Ask,
            };
        }
        Ok(())
    }

    async fn prompt_message(
        &self,
        engine_thread_id: &str,
        connection: &OpenCodeConnection,
        body: Value,
    ) -> Result<()> {
        let response = self
            .request(
                connection,
                reqwest::Method::POST,
                &opencode_prompt_message_path(engine_thread_id),
            )
            .json(&body)
            .send()
            .await
            .context("failed to send OpenCode prompt")?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("failed to read OpenCode prompt response")?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&body);
            anyhow::bail!("failed to send OpenCode prompt: HTTP {status}: {body}");
        }
        Ok(())
    }

    fn request(
        &self,
        connection: &OpenCodeConnection,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}{}", connection.base_url.trim_end_matches('/'), path);
        let request = self
            .http
            .request(method, url)
            .headers(auth_headers(&connection.password));
        if connection.include_directory_header {
            request.header("X-OpenCode-Directory", &connection.cwd)
        } else {
            request
        }
    }

    async fn handle_event(
        &self,
        engine_thread_id: &str,
        event: &OpenCodeBusEvent,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
        connection: Arc<OpenCodeConnection>,
        matched: bool,
    ) {
        if event.event_type == "permission.asked" {
            log::info!(
                "OpenCode permission.asked received: engine_thread_id={}, request_id={:?}, permission={:?}, matched={}",
                engine_thread_id,
                event.properties.get("id").and_then(Value::as_str),
                event.properties.get("permission").and_then(Value::as_str),
                matched,
            );
        }

        match event.event_type.as_str() {
            "message.updated" => {
                if let Some(info) = event.properties.get("info").and_then(Value::as_object) {
                    if let (Some(id), Some(role)) = (
                        info.get("id").and_then(Value::as_str),
                        info.get("role").and_then(Value::as_str),
                    ) {
                        mapper.record_message(
                            id,
                            role,
                            info.get("parentID").and_then(Value::as_str),
                        );
                    }
                }
            }
            "message.part.delta" => {
                let field = event
                    .properties
                    .get("field")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if field != "text" {
                    return;
                }
                let delta = event
                    .properties
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    return;
                }
                let part_id = event
                    .properties
                    .get("partID")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message_id = event
                    .properties
                    .get("messageID")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if part_id.is_empty() || message_id.is_empty() {
                    return;
                }
                if mapper.is_prompt_user_message(message_id) {
                    mapper.pending_text_by_part_id.remove(part_id);
                    return;
                }
                if !mapper.should_process_part_for_message(message_id) {
                    mapper.pending_text_by_part_id.remove(part_id);
                    return;
                }
                let Some(part_type) = mapper.part_type_by_id.get(part_id).cloned() else {
                    mapper.store_pending_text(part_id, message_id, delta);
                    return;
                };
                emit_opencode_part_delta(mapper, event_tx, part_id, &part_type, delta).await;
            }
            "message.part.updated" => {
                let Ok(envelope) =
                    serde_json::from_value::<OpenCodePartEnvelope>(event.properties.clone())
                else {
                    return;
                };
                self.handle_part_updated(&envelope.part, mapper, event_tx)
                    .await;
            }
            "session.status" => {
                let status = event
                    .properties
                    .get("status")
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str);
                match status {
                    Some("busy") | Some("retry") => {
                        mapper.busy_seen = true;
                    }
                    Some("idle") if mapper.busy_seen || mapper.content_seen => {
                        self.reconcile_session_messages(
                            engine_thread_id,
                            mapper,
                            event_tx,
                            connection.as_ref(),
                        )
                        .await;
                        self.complete_after_idle(mapper, event_tx).await;
                    }
                    _ => {}
                }
            }
            "session.idle" if mapper.busy_seen || mapper.content_seen => {
                self.reconcile_session_messages(
                    engine_thread_id,
                    mapper,
                    event_tx,
                    connection.as_ref(),
                )
                .await;
                self.complete_after_idle(mapper, event_tx).await;
            }
            "session.error" => {
                mapper.failed = true;
                mapper.content_seen = true;
                let message = session_error_message(&event.properties);
                event_tx
                    .send(EngineEvent::Error {
                        message,
                        recoverable: false,
                    })
                    .await
                    .ok();
                emit_turn_completed(mapper, event_tx, TurnCompletionStatus::Failed).await;
            }
            "session.diff" => {
                if let Some(diff) = format_session_diff(&event.properties) {
                    mapper.content_seen = true;
                    event_tx
                        .send(EngineEvent::DiffUpdated {
                            diff,
                            scope: DiffScope::Workspace,
                        })
                        .await
                        .ok();
                }
            }
            "permission.asked" => {
                mapper.content_seen = true;
                self.handle_permission_asked(&event.properties, event_tx, connection)
                    .await;
                log::info!(
                    "OpenCode permission.asked handed to approval handler: engine_thread_id={}, request_id={:?}, permission={:?}, matched={}",
                    engine_thread_id,
                    event.properties.get("id").and_then(Value::as_str),
                    event.properties.get("permission").and_then(Value::as_str),
                    matched,
                );
            }
            "question.asked" => {
                mapper.content_seen = true;
                self.handle_question_asked(&event.properties, event_tx, connection)
                    .await;
            }
            _ => {}
        }
    }

    async fn complete_after_idle(
        &self,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
    ) {
        if mapper.content_seen {
            emit_turn_completed(mapper, event_tx, TurnCompletionStatus::Completed).await;
            return;
        }

        event_tx
            .send(EngineEvent::Error {
                message: "OpenCode became idle without producing a response for this prompt."
                    .to_string(),
                recoverable: false,
            })
            .await
            .ok();
        emit_turn_completed(mapper, event_tx, TurnCompletionStatus::Failed).await;
    }

    async fn handle_part_updated(
        &self,
        part: &OpenCodePart,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
    ) {
        if !mapper.should_process_part_for_message(&part.message_id) {
            mapper.pending_text_by_part_id.remove(&part.id);
            return;
        }
        mapper
            .part_type_by_id
            .insert(part.id.clone(), part.part_type.clone());
        match part.part_type.as_str() {
            "text" | "reasoning" => {
                if mapper.is_prompt_user_message(&part.message_id) {
                    mapper.pending_text_by_part_id.remove(&part.id);
                    return;
                }
                let Some(text) = part.text.as_deref() else {
                    flush_pending_opencode_text_for_part(mapper, event_tx, &part.id).await;
                    return;
                };
                mapper.pending_text_by_part_id.remove(&part.id);
                emit_opencode_part_snapshot(mapper, event_tx, &part.id, &part.part_type, text)
                    .await;
            }
            "tool" => {
                self.handle_tool_part(part, mapper, event_tx).await;
            }
            "agent" => {
                self.handle_agent_part(part, mapper, event_tx).await;
            }
            "patch" => {
                mapper.content_seen = true;
                event_tx
                    .send(EngineEvent::DiffUpdated {
                        diff: serde_json::to_string_pretty(part).unwrap_or_default(),
                        scope: DiffScope::Workspace,
                    })
                    .await
                    .ok();
            }
            "step-finish" => {
                if let Some(usage) = token_usage_from_step_finish(part) {
                    mapper.latest_token_usage = Some(usage);
                    mapper.content_seen = true;
                }
            }
            _ => {
                mapper.pending_text_by_part_id.remove(&part.id);
            }
        }
    }

    /// 读取 OpenCode 单页消息；该请求同时兼容当前数组响应和带 cursor 的 data 响应。
    async fn fetch_session_message_page(
        &self,
        connection: &OpenCodeConnection,
        engine_thread_id: &str,
        limit: usize,
        order: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<OpenCodeMessagePage> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(order) = order.filter(|value| !value.trim().is_empty()) {
            query.push(("order", order.to_string()));
        }
        if let Some(cursor) = cursor.filter(|value| !value.trim().is_empty()) {
            query.push(("cursor", cursor.to_string()));
        }
        let response = self
            .request(
                connection,
                reqwest::Method::GET,
                &format!("/session/{engine_thread_id}/message"),
            )
            .query(&query)
            .send()
            .await?
            .error_for_status()
            .context("failed to read OpenCode session messages")?;
        let response = response
            .json::<OpenCodeMessageResponse>()
            .await
            .context("failed to parse OpenCode session messages")?;
        Ok(match response {
            OpenCodeMessageResponse::Array(messages) => OpenCodeMessagePage {
                messages,
                continuation: None,
                uses_cursor: false,
            },
            OpenCodeMessageResponse::Object(response) => {
                // 完整历史按 asc 初始请求并沿时间线前进，只能使用协议定义的 cursor.next。
                let continuation = response.cursor.and_then(|cursor| cursor.next);
                OpenCodeMessagePage {
                    messages: response.data,
                    continuation,
                    uses_cursor: true,
                }
            }
        })
    }

    /// 读取 OpenCode 会话的完整历史，并将远端消息转换成 AuraCoder 快照。
    pub async fn read_thread_sync_snapshot(
        &self,
        cwd: &str,
        engine_thread_id: &str,
    ) -> Result<ThreadSyncSnapshot> {
        let connection = self.connection_for_cwd(cwd).await?;
        let result = timeout(OPENCODE_HISTORY_TIMEOUT, async {
            let mut all_messages = HashMap::<String, OpenCodeMessageWithParts>::new();
            let mut cursor = None::<String>;
            let mut seen_continuations = HashSet::new();

            loop {
                let page = self
                    .fetch_session_message_page(
                        connection.as_ref(),
                        engine_thread_id,
                        OPENCODE_HISTORY_PAGE_LIMIT,
                        cursor.is_none().then_some("asc"),
                        cursor.as_deref(),
                    )
                    .await?;
                let page_count = page.messages.len();
                let page_messages = page.messages;
                for message in page_messages.iter() {
                    let message_id = message.info.id.trim();
                    anyhow::ensure!(
                        !message_id.is_empty(),
                        "OpenCode 历史消息缺少稳定消息 ID"
                    );
                    if let Some(session_id) = message.info.session_id.as_deref() {
                        anyhow::ensure!(
                            session_id == engine_thread_id,
                            "OpenCode 历史消息属于其他会话: expected={} actual={}",
                            engine_thread_id,
                            session_id
                        );
                    }
                    anyhow::ensure!(
                        matches!(message.info.role.as_str(), "user" | "assistant"),
                        "OpenCode 历史消息角色无法识别: {}",
                        message.info.role
                    );
                    all_messages
                        .entry(message_id.to_string())
                        .or_insert_with(|| message.clone());
                }

                if let Some(next_cursor) = page.continuation {
                    anyhow::ensure!(
                        page.uses_cursor,
                        "OpenCode 返回了未声明游标的继续标记"
                    );
                    anyhow::ensure!(
                        !next_cursor.trim().is_empty(),
                        "OpenCode 历史消息返回空的 cursor.next"
                    );
                    anyhow::ensure!(
                        cursor.as_deref() != Some(next_cursor.as_str()),
                        "OpenCode 历史消息游标未推进: {}",
                        next_cursor
                    );
                    anyhow::ensure!(
                        seen_continuations.insert(next_cursor.clone()),
                        "OpenCode 历史消息游标重复: {}",
                        next_cursor
                    );
                    cursor = Some(next_cursor);
                    continue;
                }
                if page.uses_cursor {
                    // 当前响应已明确采用 cursor 协议；没有 next 就是协议声明的最后一页，
                    // 即使本页恰好达到 limit，也不能误报为分页上限错误。
                    break;
                }
                if page_count < OPENCODE_HISTORY_PAGE_LIMIT {
                    break;
                }
                // 旧数组响应没有 continuation 字段，且项目没有证据证明其 before 语义或
                // 返回顺序；达到 limit 时无法证明已经读完，必须放弃本次落库。
                anyhow::bail!(
                    "OpenCode 历史消息数组响应未提供可验证的分页标记，无法证明完整历史已读完: count={page_count} limit={OPENCODE_HISTORY_PAGE_LIMIT}"
                );
            }

            let mut messages = all_messages.into_values().collect::<Vec<_>>();
            messages.sort_by(|left, right| {
                opencode_message_sort_key(left).cmp(&opencode_message_sort_key(right))
            });
            let imported_messages = messages
                .iter()
                .map(|message| {
                    let role = message.info.role.trim().to_ascii_lowercase();
                    let created_at = message
                        .info
                        .time
                        .as_ref()
                        .and_then(|time| time.created.or(time.completed))
                        .and_then(|value| {
                            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(value)
                                .map(|timestamp| timestamp.to_rfc3339())
                        });
                    let mut blocks = Vec::new();
                    let mut content_parts = Vec::new();
                    let mut has_error = false;
                    let mut has_running_action = false;
                    let mut part_token_input = 0_u64;
                    let mut part_token_output = 0_u64;

                    for part in &message.parts {
                        match part.part_type.trim().to_ascii_lowercase().as_str() {
                            "text" => {
                                let text = part
                                    .text
                                    .as_deref()
                                    .context("OpenCode text part 缺少文本内容")?;
                                if !text.is_empty() {
                                    content_parts.push(text.to_string());
                                    blocks.push(json!({
                                        "type": "text",
                                        "content": text,
                                    }));
                                }
                            }
                            "reasoning" => {
                                let text = part
                                    .text
                                    .as_deref()
                                    .context("OpenCode reasoning part 缺少文本内容")?;
                                if !text.is_empty() {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "content": text,
                                    }));
                                }
                            }
                            "tool" => {
                                let state = part
                                    .state
                                    .as_ref()
                                    .context("OpenCode tool part 缺少执行状态")?;
                                let raw_status = state.status.trim().to_ascii_lowercase();
                                let (status, success) = match raw_status.as_str() {
                                    "pending" => ("pending", None),
                                    "running" => {
                                        has_running_action = true;
                                        ("running", None)
                                    }
                                    "completed" | "success" | "done" => ("done", Some(true)),
                                    "error" | "failed" => {
                                        has_error = true;
                                        ("error", Some(false))
                                    }
                                    other => {
                                        anyhow::bail!(
                                            "OpenCode tool part 状态无法识别: {other}"
                                        )
                                    }
                                };
                                let tool_name = part
                                    .tool
                                    .as_deref()
                                    .filter(|value| !value.trim().is_empty())
                                    .unwrap_or("tool");
                                let output = state.output.clone().or_else(|| state.raw.clone());
                                let error = state.error.clone();
                                let result = success.map(|success| {
                                    json!({
                                        "success": success,
                                        "output": output,
                                        "error": error,
                                        "diff": Value::Null,
                                        "durationMs": 0,
                                    })
                                });
                                let output_chunks = output
                                    .as_deref()
                                    .filter(|value| !value.is_empty())
                                    .map(|value| {
                                        vec![json!({
                                            "stream": "stdout",
                                            "content": value,
                                        })]
                                    })
                                    .unwrap_or_default();
                                blocks.push(json!({
                                    "type": "action",
                                    "actionId": part.id,
                                    "engineActionId": part.call_id,
                                    "actionType": action_type_for_tool(tool_name),
                                    "summary": state.title.clone().unwrap_or_else(|| tool_name.to_string()),
                                    "details": {
                                        "tool": tool_name,
                                        "callID": part.call_id,
                                        "input": state.input,
                                        "metadata": part.metadata.clone().or_else(|| state.metadata.clone()),
                                    },
                                    "outputChunks": output_chunks,
                                    "status": status,
                                    "result": result,
                                }));
                            }
                            "patch" => {
                                let mut diff = part.patch.clone().or_else(|| part.diff.clone());
                                if diff.is_none() {
                                    if let Some(files) = part.files.as_ref() {
                                        let mut formatted = String::new();
                                        for file in files {
                                            let file_name = file
                                                .get("file")
                                                .or_else(|| file.get("path"))
                                                .and_then(Value::as_str)
                                                .unwrap_or("file");
                                            let patch = file
                                                .get("patch")
                                                .or_else(|| file.get("diff"))
                                                .and_then(Value::as_str)
                                                .unwrap_or("");
                                            if !patch.is_empty() {
                                                formatted.push_str(&format!(
                                                    "diff -- {file_name}\n{patch}\n"
                                                ));
                                            }
                                        }
                                        if !formatted.is_empty() {
                                            diff = Some(formatted);
                                        }
                                    }
                                }
                                if diff.is_none() {
                                    diff = part.source.as_ref().and_then(|source| {
                                        source.as_str().map(ToOwned::to_owned).or_else(|| {
                                            (!source.is_null())
                                                .then(|| serde_json::to_string_pretty(source).ok())
                                                .flatten()
                                        })
                                    });
                                }
                                let diff = diff.context("OpenCode patch part 缺少差异内容")?;
                                if !diff.is_empty() {
                                    blocks.push(json!({
                                        "type": "diff",
                                        "diff": diff,
                                        "scope": "workspace",
                                    }));
                                }
                            }
                            "file" => {
                                let file_path = part
                                    .url
                                    .clone()
                                    .or_else(|| part.file.clone())
                                    .context("OpenCode file part 缺少文件地址")?;
                                let file_name = part
                                    .filename
                                    .clone()
                                    .or_else(|| {
                                        Path::new(&file_path)
                                            .file_name()
                                            .and_then(|name| name.to_str())
                                            .map(ToOwned::to_owned)
                                    })
                                    .unwrap_or_else(|| "file".to_string());
                                blocks.push(json!({
                                    "type": "attachment",
                                    "fileName": file_name,
                                    "filePath": file_path,
                                    "sizeBytes": 0,
                                    "mimeType": part.mime.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
                                    "isRemote": true,
                                }));
                            }
                            "agent" => {
                                let agent_name = part
                                    .name
                                    .as_deref()
                                    .filter(|value| !value.trim().is_empty())
                                    .unwrap_or("agent");
                                blocks.push(json!({
                                    "type": "action",
                                    "actionId": part.id,
                                    "engineActionId": part.message_id,
                                    "actionType": "other",
                                    "summary": format!("OpenCode agent: {agent_name}"),
                                    "details": {
                                        "agent": agent_name,
                                        "source": part.source,
                                        "messageID": part.message_id,
                                    },
                                    "outputChunks": [],
                                    "status": "done",
                                    "result": {
                                        "success": true,
                                        "output": Value::Null,
                                        "error": Value::Null,
                                        "diff": Value::Null,
                                        "durationMs": 0,
                                    },
                                }));
                            }
                            "step-finish" => {
                                if let Some(tokens) = part.tokens.as_ref() {
                                    part_token_input = part_token_input.saturating_add(tokens.input);
                                    part_token_output = part_token_output.saturating_add(tokens.output);
                                }
                            }
                            "step-start" | "snapshot" | "retry" | "compaction" => {
                                let label = part.part_type.trim();
                                blocks.push(json!({
                                    "type": "notice",
                                    "kind": format!("opencode_{label}"),
                                    "level": "info",
                                    "title": label,
                                    "message": format!("OpenCode {label} record"),
                                }));
                            }
                            unknown => {
                                anyhow::bail!("OpenCode 历史消息 part 类型无法识别: {unknown}")
                            }
                        }
                    }

                    let info_tokens = message.info.tokens.as_ref();
                    let token_input = info_tokens
                        .map(|tokens| tokens.input)
                        .unwrap_or(part_token_input);
                    let token_output = info_tokens
                        .map(|tokens| tokens.output)
                        .unwrap_or(part_token_output);
                    let status = if role == "user" {
                        "completed"
                    } else if has_error {
                        "error"
                    } else if has_running_action || message.info.time.as_ref().is_some_and(|time| time.completed.is_none()) {
                        "streaming"
                    } else {
                        "completed"
                    };
                    let turn_model_id = match (
                        message.info.provider_id.as_deref(),
                        message.info.model_id.as_deref(),
                    ) {
                        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
                            Some(format!("{provider}/{model}"))
                        }
                        (_, Some(model)) if !model.is_empty() => Some(model.to_string()),
                        _ => None,
                    };
                    Ok(ImportedThreadMessage {
                        role,
                        content: (!content_parts.is_empty()).then(|| content_parts.join("\\n")),
                        blocks: Value::Array(blocks),
                        status: status.to_string(),
                        turn_engine_id: Some(message.info.id.trim().to_string()),
                        turn_model_id,
                        turn_reasoning_effort: message.info.variant.clone(),
                        token_input,
                        token_output,
                        created_at,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let title = imported_messages
                .iter()
                .find(|message| message.role == "user")
                .and_then(|message| message.content.clone())
                .filter(|value| !value.trim().is_empty());
            let preview = imported_messages
                .iter()
                .rev()
                .find_map(|message| message.content.clone())
                .filter(|value| !value.trim().is_empty());
            Ok(ThreadSyncSnapshot {
                title,
                preview,
                raw_status: None,
                active_flags: Vec::new(),
                imported_messages,
            })
        })
        .await
        .context("读取 OpenCode 会话历史超时")?;
        // OpenCode 服务由 CLI 生命周期持有，协议查询完成后不在客户端中停止。
        result
    }

    /// 活动回合结束后读取固定上限的消息，保持既有补偿筛选和事件行为不变。
    async fn reconcile_session_messages(
        &self,
        engine_thread_id: &str,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
        connection: &OpenCodeConnection,
    ) {
        let result = timeout(
            OPENCODE_COMMAND_TIMEOUT,
            self.fetch_session_message_page(
                connection,
                engine_thread_id,
                OPENCODE_RECONCILE_MESSAGE_LIMIT,
                None,
                None,
            ),
        )
        .await;

        let messages = match result {
            Ok(Ok(page)) => page.messages,
            Ok(Err(error)) => {
                log::warn!("failed to reconcile OpenCode messages after idle: {error}");
                return;
            }
            Err(_) => {
                log::warn!("timed out reconciling OpenCode messages after idle");
                return;
            }
        };

        for message in messages {
            mapper.record_message(
                &message.info.id,
                &message.info.role,
                message.info.parent_id.as_deref(),
            );
            if message.info.role != "assistant"
                || message.info.parent_id.as_deref() != Some(mapper.prompt_message_id.as_str())
            {
                continue;
            }
            for part in message.parts {
                self.handle_part_updated(&part, mapper, event_tx).await;
            }
        }
    }

    async fn handle_tool_part(
        &self,
        part: &OpenCodePart,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
    ) {
        let action_id = part.id.clone();
        let tool_name = part.tool.clone().unwrap_or_else(|| "tool".to_string());
        let action_type = action_type_for_tool(&tool_name);
        let state = part.state.clone();
        let summary = state
            .as_ref()
            .and_then(|state| state.title.clone())
            .unwrap_or_else(|| tool_name.clone());

        if mapper.started_actions.insert(action_id.clone()) {
            mapper.content_seen = true;
            event_tx
                .send(EngineEvent::ActionStarted {
                    action_id: action_id.clone(),
                    engine_action_id: Some(part.message_id.clone()),
                    action_type: action_type.clone(),
                    summary: summary.clone(),
                    details: json!({
                        "tool": tool_name,
                        "callID": part.call_id.clone(),
                        "state": state.as_ref().and_then(|value| value.input.clone()),
                        "metadata": part.metadata.clone()
                            .or_else(|| state.as_ref().and_then(|value| value.metadata.clone())),
                    }),
                })
                .await
                .ok();
        }

        let Some(state) = state else {
            return;
        };
        match state.status.as_str() {
            "running" => {
                if let Some(title) = state.title {
                    event_tx
                        .send(EngineEvent::ActionProgressUpdated {
                            action_id,
                            message: title,
                        })
                        .await
                        .ok();
                }
            }
            "completed" | "error" => {
                if !mapper.completed_actions.insert(action_id.clone()) {
                    return;
                }
                if let Some(output) = state.output.clone().or(state.raw.clone()) {
                    let content = trim_action_output_delta_content(&output);
                    if !content.is_empty() {
                        event_tx
                            .send(EngineEvent::ActionOutputDelta {
                                action_id: action_id.clone(),
                                stream: OutputStream::Stdout,
                                content,
                            })
                            .await
                            .ok();
                    }
                }
                event_tx
                    .send(EngineEvent::ActionCompleted {
                        action_id,
                        result: ActionResult {
                            success: state.status == "completed",
                            output: state.output,
                            error: state.error,
                            diff: None,
                            duration_ms: 0,
                        },
                    })
                    .await
                    .ok();
            }
            _ => {}
        }
    }

    async fn handle_agent_part(
        &self,
        part: &OpenCodePart,
        mapper: &mut OpenCodeTurnMapper,
        event_tx: &mpsc::Sender<EngineEvent>,
    ) {
        let action_id = part.id.clone();
        let agent_name = part.name.clone().unwrap_or_else(|| "agent".to_string());
        let summary = format!("OpenCode agent: {agent_name}");

        if mapper.started_actions.insert(action_id.clone()) {
            mapper.content_seen = true;
            event_tx
                .send(EngineEvent::ActionStarted {
                    action_id: action_id.clone(),
                    engine_action_id: Some(part.message_id.clone()),
                    action_type: ActionType::Other,
                    summary,
                    details: json!({
                        "agent": agent_name,
                        "source": part.source.clone(),
                        "sessionID": part.session_id.clone(),
                        "messageID": part.message_id.clone(),
                    }),
                })
                .await
                .ok();
        }

        if mapper.completed_actions.insert(action_id.clone()) {
            event_tx
                .send(EngineEvent::ActionCompleted {
                    action_id,
                    result: ActionResult {
                        success: true,
                        output: None,
                        error: None,
                        diff: None,
                        duration_ms: 0,
                    },
                })
                .await
                .ok();
        }
    }

    async fn handle_permission_asked(
        &self,
        properties: &Value,
        event_tx: &mpsc::Sender<EngineEvent>,
        connection: Arc<OpenCodeConnection>,
    ) {
        let session_id = properties.get("sessionID").and_then(Value::as_str);
        let permission = properties
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
            log::warn!(
                "OpenCode permission request missing request_id: session_id={:?}, permission={}",
                session_id,
                permission,
            );
            return;
        };
        let permission_mode = if let Some(session_id) = session_id {
            let state = self.state.lock().await;
            state
                .sessions
                .get(session_id)
                .map(|session| session.permission_mode)
                .unwrap_or(OpenCodePermissionMode::Ask)
        } else {
            OpenCodePermissionMode::Ask
        };
        log::info!(
            "OpenCode permission request handling started: request_id={}, session_id={:?}, permission={}, permission_mode={:?}",
            request_id,
            session_id,
            permission,
            permission_mode,
        );

        // 已选择“完全自主”或“拒绝”时，权限请求由引擎直接回复，不进入 AuraCoder
        // 审批队列，因此不会生成审批卡。请求失败只记录日志，仍不得退化为 ask。
        let automatic_reply = match permission_mode {
            OpenCodePermissionMode::Allow => Some("always"),
            OpenCodePermissionMode::Deny => Some("reject"),
            OpenCodePermissionMode::Ask => None,
        };
        if let Some(reply) = automatic_reply {
            let result = async {
                self.request(
                    connection.as_ref(),
                    reqwest::Method::POST,
                    &format!("/permission/{request_id}/reply"),
                )
                .json(&json!({ "reply": reply }))
                .send()
                .await
                .context("failed to reply to OpenCode permission request")?
                .error_for_status()
                .context("OpenCode permission automatic reply returned an error")?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => {
                    log::info!(
                        "OpenCode permission automatic reply succeeded: reply={}, request_id={}, session_id={:?}, permission={}, permission_mode={:?}, http_success=true",
                        reply,
                        request_id,
                        session_id,
                        permission,
                        permission_mode,
                    );
                }
                Err(error) => {
                    log::error!(
                        "OpenCode permission automatic reply failed: reply={} request_id={} session_id={:?} permission={} permission_mode={:?}: {error:#}",
                        reply,
                        request_id,
                        session_id,
                        permission,
                        permission_mode,
                    );
                }
            }
            return;
        }

        let approval_id = format!("opencode-permission-{request_id}");
        let action_type = action_type_for_permission(permission);
        let patterns = properties
            .get("patterns")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let cwd = connection.cwd.clone();

        self.state.lock().await.pending_requests.insert(
            approval_id.clone(),
            PendingOpenCodeRequest::Permission {
                request_id: request_id.to_string(),
                connection,
            },
        );
        log::info!(
            "OpenCode permission approval pending: approval_id={}, request_id={}, session_id={:?}, permission={}, permission_mode={:?}",
            approval_id,
            request_id,
            session_id,
            permission,
            permission_mode,
        );

        let approval_event = EngineEvent::ApprovalRequested {
                approval_id,
                action_type,
                summary: format!("OpenCode requests {permission} permission"),
                details: json!({
                    "_serverMethod": "item/permissions/requestApproval",
                    "permission": permission,
                    "patterns": patterns,
                    "metadata": properties.get("metadata").cloned().unwrap_or_else(|| json!({})),
                    "always": properties.get("always").cloned().unwrap_or_else(|| json!([])),
                    "tool": properties.get("tool").cloned(),
                    "_opencodeRequestKind": "permission",
                    "_opencodeRequestID": request_id,
                    "_opencodeSessionID": properties.get("sessionID").cloned().unwrap_or_else(|| json!(null)),
                    "_opencodeCwd": cwd,
                }),
            };
        match event_tx.send(approval_event).await {
            Ok(()) => {
                log::info!(
                    "OpenCode permission approval event sent: approval_id={}, request_id={}, session_id={:?}, permission={}, permission_mode={:?}, send_success=true",
                    format!("opencode-permission-{request_id}"),
                    request_id,
                    session_id,
                    permission,
                    permission_mode,
                );
            }
            Err(error) => {
                log::error!(
                    "OpenCode permission approval event send failed: approval_id={}, request_id={}, session_id={:?}, permission={}, permission_mode={:?}, send_success=false, send_error={error}",
                    format!("opencode-permission-{request_id}"),
                    request_id,
                    session_id,
                    permission,
                    permission_mode,
                );
            }
        }
    }

    async fn handle_question_asked(
        &self,
        properties: &Value,
        event_tx: &mpsc::Sender<EngineEvent>,
        connection: Arc<OpenCodeConnection>,
    ) {
        let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
            return;
        };
        let questions = properties
            .get("questions")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<OpenCodeQuestionInfo>>(value).ok())
            .unwrap_or_default();
        let approval_id = format!("opencode-question-{request_id}");
        let cwd = connection.cwd.clone();

        self.state.lock().await.pending_requests.insert(
            approval_id.clone(),
            PendingOpenCodeRequest::Question {
                request_id: request_id.to_string(),
                questions: questions.clone(),
                connection,
            },
        );

        let question_details = questions
            .iter()
            .enumerate()
            .map(question_details_json)
            .collect::<Vec<_>>();

        event_tx
            .send(EngineEvent::ApprovalRequested {
                approval_id,
                action_type: ActionType::Other,
                summary: "OpenCode needs input".to_string(),
                details: json!({
                    "_serverMethod": "item/tool/requestUserInput",
                    "questions": question_details,
                    "tool": properties.get("tool").cloned(),
                    "_opencodeRequestKind": "question",
                    "_opencodeRequestID": request_id,
                    "_opencodeSessionID": properties.get("sessionID").cloned().unwrap_or_else(|| json!(null)),
                    "_opencodeCwd": cwd,
                }),
            })
            .await
            .ok();
    }
}

impl OpenCodeConnection {
    /// 取消当前协议连接的事件泵和回调任务，不触碰 CLI 服务进程生命周期。
    async fn stop(&self) {
        if let Some(pump_cancel) = self.pump_cancel.as_ref() {
            pump_cancel.cancel();
        }
        if let Some(callback_cancel) = self.callback_cancel.as_ref() {
            callback_cancel.cancel();
        }
        /*
        旧实现曾在协议连接中停止本机 OpenCode 进程并清理运行目录：
        let mut child = self.child.lock().await;
        let Some(mut process) = child.take() else {
            return;
        };
        if let Err(error) = process.kill().await {
            log::debug!("failed to stop OpenCode connection process: {error}");
        }
        if let Some(run_dir) = self.run_dir.as_ref() {
            if let Err(error) = std::fs::remove_dir_all(run_dir) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::debug!(
                        "failed to remove OpenCode computer-control runtime directory {}: {error}",
                        run_dir.display()
                    );
                }
            }
        }
        该职责已迁移到 LocalCliServiceLifecycle，协议客户端只取消自己的事件任务。
        */
    }
}

struct ParsedModelSlug {
    provider_id: String,
    model_id: String,
}

fn parse_model_slug(slug: &str) -> Option<ParsedModelSlug> {
    let trimmed = slug.trim();
    let separator = trimmed.find('/')?;
    if separator == 0 || separator + 1 >= trimmed.len() {
        return None;
    }
    Some(ParsedModelSlug {
        provider_id: trimmed[..separator].to_string(),
        model_id: trimmed[separator + 1..].to_string(),
    })
}

fn should_cache_runtime_model_catalog(models: &[ModelInfo]) -> bool {
    models.iter().any(|model| {
        parse_model_slug(&model.id)
            .map(|slug| slug.provider_id != "opencode")
            .unwrap_or(false)
    })
}

fn opencode_prompt_message_path(engine_thread_id: &str) -> String {
    format!("/session/{engine_thread_id}/message")
}

/// 为 OpenCode 历史消息提供时间优先、消息 ID 兜底的稳定排序键。
fn opencode_message_sort_key(message: &OpenCodeMessageWithParts) -> (i64, String) {
    (
        message
            .info
            .time
            .as_ref()
            .and_then(|time| time.created.or(time.completed))
            .unwrap_or(i64::MAX),
        message.info.id.trim().to_string(),
    )
}

fn build_prompt_body(
    model_id: &str,
    reasoning_effort: Option<&str>,
    agent: Option<&str>,
    input: TurnInput,
) -> Result<OpenCodePromptBody> {
    let model = parse_model_slug(model_id)
        .with_context(|| format!("invalid OpenCode model `{model_id}`"))?;
    let message_id = new_message_id();
    let mut parts = vec![json!({
        "type": "text",
        "text": input.message,
    })];
    for attachment in input.attachments {
        parts.push(json!({
            "type": "file",
            "mime": attachment.mime_type.unwrap_or_else(|| "text/plain".to_string()),
            "filename": attachment.file_name,
            "url": file_url(&attachment.file_path),
        }));
    }

    let mut body = json!({
        "messageID": message_id.clone(),
        "model": {
            "providerID": model.provider_id,
            "modelID": model.model_id,
        },
        "parts": parts,
    });
    if let Some(object) = body.as_object_mut() {
        if let Some(agent) = normalize_opencode_agent(agent) {
            object.insert("agent".to_string(), json!(agent));
        }
        if let Some(variant) = reasoning_effort
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            object.insert("variant".to_string(), json!(variant));
        }
    }

    Ok(OpenCodePromptBody { message_id, body })
}

fn normalize_opencode_agent(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "build")
        .map(ToOwned::to_owned)
}

fn model_info(
    id: &str,
    display_name: &str,
    description: &str,
    is_default: bool,
    supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    input_modalities: Vec<String>,
) -> ModelInfo {
    model_info_with_metadata(
        id,
        display_name,
        description,
        is_default,
        supported_reasoning_efforts,
        input_modalities,
        vec!["text".to_string()],
        None,
    )
}

fn model_info_with_metadata(
    id: &str,
    display_name: &str,
    description: &str,
    is_default: bool,
    supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    input_modalities: Vec<String>,
    attachment_modalities: Vec<String>,
    limits: Option<ModelLimits>,
) -> ModelInfo {
    let default_reasoning_effort =
        default_reasoning_effort(&supported_reasoning_efforts).unwrap_or("medium");
    ModelInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        hidden: false,
        is_default,
        upgrade: None,
        availability_nux: None,
        upgrade_info: None,
        input_modalities,
        attachment_modalities,
        limits,
        supports_personality: false,
        default_reasoning_effort: default_reasoning_effort.to_string(),
        supported_reasoning_efforts,
    }
}

fn map_runtime_agents(agents: Vec<OpenCodeRuntimeAgent>) -> Vec<OpenCodeAgentDto> {
    agents
        .into_iter()
        .map(|agent| OpenCodeAgentDto {
            name: agent.name,
            description: agent.description,
            mode: agent.mode,
            native: agent.native.unwrap_or(false),
            hidden: agent.hidden.unwrap_or(false),
            model_provider_id: agent.model.as_ref().map(|model| model.provider_id.clone()),
            model_id: agent.model.as_ref().map(|model| model.model_id.clone()),
            variant: agent.variant,
            steps: agent.steps,
        })
        .collect()
}

fn map_runtime_commands(commands: Vec<OpenCodeRuntimeCommand>) -> Vec<OpenCodeCommandDto> {
    commands
        .into_iter()
        .map(|command| OpenCodeCommandDto {
            name: command.name,
            description: command.description,
            agent: command.agent,
            model: command.model,
            source: command.source,
            subtask: command.subtask.unwrap_or(false),
            hints: command.hints,
        })
        .collect()
}

fn map_runtime_mcp_connections(mcp: HashMap<String, Value>) -> Vec<OpenCodeMcpServerDto> {
    let mut connections = mcp
        .into_iter()
        .map(|(name, raw)| {
            let status = raw
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let detail = raw
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| raw.get("message").and_then(Value::as_str))
                .or_else(|| raw.get("detail").and_then(Value::as_str))
                .map(ToOwned::to_owned);
            OpenCodeMcpServerDto {
                name,
                status,
                detail,
                raw,
            }
        })
        .collect::<Vec<_>>();
    connections.sort_by(|a, b| a.name.cmp(&b.name));
    connections
}

fn map_session_record(session: OpenCodeSessionRecord) -> OpenCodeRemoteSessionSummary {
    OpenCodeRemoteSessionSummary {
        engine_thread_id: session.id,
        title: session.title.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }),
        cwd: session.directory,
        created_at: session.time.created,
        updated_at: session.time.updated,
        archived: session.time.archived.unwrap_or(0) > 0,
    }
}

fn current_unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn default_reasoning_effort(options: &[ReasoningEffortOption]) -> Option<&'static str> {
    for preferred in ["medium", "high", "low", "minimal", "none", "xhigh", "max"] {
        if options
            .iter()
            .any(|option| option.reasoning_effort == preferred)
        {
            return Some(preferred);
        }
    }
    None
}

fn resolve_model_reasoning_effort(
    model: &ModelInfo,
    requested_effort: Option<&str>,
) -> Option<String> {
    if model.supported_reasoning_efforts.is_empty() {
        return None;
    }

    let requested = requested_effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if let Some(requested) = requested.as_ref() {
        if model
            .supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == *requested)
        {
            return Some(requested.clone());
        }
    }

    if model
        .supported_reasoning_efforts
        .iter()
        .any(|option| option.reasoning_effort == model.default_reasoning_effort)
    {
        return Some(model.default_reasoning_effort.clone());
    }

    model
        .supported_reasoning_efforts
        .iter()
        .map(|option| option.reasoning_effort.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn reasoning_efforts_from_variants(
    variants: &HashMap<String, Value>,
) -> Vec<ReasoningEffortOption> {
    let names = variants.keys().map(String::as_str).collect::<Vec<_>>();
    reasoning_efforts_from_variant_names(&names)
}

fn reasoning_efforts_from_variant_names(names: &[&str]) -> Vec<ReasoningEffortOption> {
    const ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    ORDER
        .iter()
        .copied()
        .filter(|effort| names.iter().any(|name| name.eq_ignore_ascii_case(effort)))
        .map(|effort| ReasoningEffortOption {
            reasoning_effort: effort.to_string(),
            description: format!("OpenCode {effort} variant"),
        })
        .collect()
}

fn model_modalities_from_capabilities(
    capabilities: Option<&OpenCodeModelCapabilities>,
) -> Vec<String> {
    let mut modalities = Vec::new();
    if capabilities
        .and_then(|capabilities| capabilities.input.as_ref())
        .map(|input| input.text)
        .unwrap_or(true)
    {
        modalities.push("text".to_string());
    }
    if capabilities
        .and_then(|capabilities| capabilities.input.as_ref())
        .map(|input| input.image)
        .unwrap_or(false)
    {
        modalities.push("image".to_string());
    }
    if capabilities
        .and_then(|capabilities| capabilities.input.as_ref())
        .map(|input| input.pdf)
        .unwrap_or(false)
    {
        modalities.push("pdf".to_string());
    }
    modalities
}

fn attachment_modalities_from_capabilities(
    capabilities: Option<&OpenCodeModelCapabilities>,
) -> Vec<String> {
    let Some(capabilities) = capabilities else {
        return vec!["text".to_string()];
    };
    if !capabilities.attachment {
        return Vec::new();
    }

    let input = capabilities.input.as_ref();
    let mut modalities = Vec::new();
    if input.map(|input| input.text).unwrap_or(true) {
        modalities.push("text".to_string());
    }
    if input.map(|input| input.image).unwrap_or(false) {
        modalities.push("image".to_string());
    }
    if input.map(|input| input.pdf).unwrap_or(false) {
        modalities.push("pdf".to_string());
    }
    modalities
}

fn model_limits(limit: Option<&OpenCodeModelLimit>) -> Option<ModelLimits> {
    let limit = limit?;
    if limit.context.is_none() && limit.input.is_none() && limit.output.is_none() {
        return None;
    }
    Some(ModelLimits {
        context_tokens: limit.context,
        input_tokens: limit.input,
        output_tokens: limit.output,
    })
}

fn model_modalities(model: &OpenCodeProviderModel) -> Vec<String> {
    model_modalities_from_capabilities(model.capabilities.as_ref())
}

fn scope_cwd(scope: &ThreadScope) -> String {
    match scope {
        ThreadScope::Project { root_path, .. } => root_path.clone(),
    }
}

fn permission_mode_from_policy(policy: Option<&Value>) -> OpenCodePermissionMode {
    let Some(raw) = policy.and_then(Value::as_str) else {
        return OpenCodePermissionMode::Ask;
    };
    match raw.trim().to_lowercase().as_str() {
        "allow" | "trusted" | "never" => OpenCodePermissionMode::Allow,
        "deny" | "restricted" | "untrusted" => OpenCodePermissionMode::Deny,
        _ => OpenCodePermissionMode::Ask,
    }
}

fn permission_rules(mode: OpenCodePermissionMode) -> Value {
    match mode {
        OpenCodePermissionMode::Allow => {
            json!([{ "permission": "*", "pattern": "*", "action": "allow" }])
        }
        OpenCodePermissionMode::Deny => {
            json!([{ "permission": "*", "pattern": "*", "action": "deny" }])
        }
        OpenCodePermissionMode::Ask => json!([
            { "permission": "*", "pattern": "*", "action": "ask" },
            { "permission": "question", "pattern": "*", "action": "allow" }
        ]),
    }
}

fn session_permission_matches(
    session: &OpenCodeSessionRecord,
    mode: OpenCodePermissionMode,
) -> bool {
    let expected_action = match mode {
        OpenCodePermissionMode::Ask => "ask",
        OpenCodePermissionMode::Allow => "allow",
        OpenCodePermissionMode::Deny => "deny",
    };
    session_wildcard_permission_action(session.permission.as_ref()) == Some(expected_action)
}

fn session_wildcard_permission_action(permission: Option<&Value>) -> Option<&str> {
    let rules = permission?.as_array()?;
    // OpenCode 使用 findLast 评估权限，后出现的全局规则覆盖前面的规则。
    rules.iter().rev().find_map(|rule| {
        let permission = rule.get("permission").and_then(Value::as_str)?;
        let pattern = rule.get("pattern").and_then(Value::as_str)?;
        if permission == "*" && pattern == "*" {
            return rule.get("action").and_then(Value::as_str);
        }
        None
    })

    // 旧实现按正序查找首条全局规则，不能与 OpenCode findLast 语义保持一致：
    // rules.iter().find_map(|rule| {
    //     let permission = rule.get("permission").and_then(Value::as_str)?;
    //     let pattern = rule.get("pattern").and_then(Value::as_str)?;
    //     if permission == "*" && pattern == "*" {
    //         return rule.get("action").and_then(Value::as_str);
    //     }
    //     None
    // })
}

/// 将 OpenCode 工具的 object schema 转为插件要求的 Zod raw shape 源码。
fn opencode_tool_args_source(input_schema: &Value) -> Result<String> {
    let schema_type = input_schema
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("OpenCode 工具 inputSchema.type 必须是 object"))?;
    if schema_type != "object" {
        return Err(anyhow::anyhow!(
            "OpenCode 工具 inputSchema.type 必须是 object，实际为 {schema_type}"
        ));
    }
    opencode_object_shape_source(input_schema)
}

/// 递归生成一个 Zod schema 表达式，并严格拒绝无法保持语义的 JSON Schema。
fn opencode_zod_schema_source(schema: &Value) -> Result<String> {
    let type_value = match schema.get("type") {
        Some(type_value) => type_value,
        None => {
            let object = schema.as_object().ok_or_else(|| {
                anyhow::anyhow!("JSON Schema 缺少 type 且 schema 必须是对象")
            })?;
            let unsupported_keywords = object
                .keys()
                .filter(|key| *key != "description" && *key != "default")
                .cloned()
                .collect::<Vec<_>>();
            if !unsupported_keywords.is_empty() {
                return Err(anyhow::anyhow!(
                    "JSON Schema 缺少 type 且包含无法解释的关键字: {}",
                    unsupported_keywords.join(", ")
                ));
            }

            // 无 type 且只包含 description/default（或为空）表示任意 JSON 值。
            // 这类 schema 不能被强行映射为某一种 Zod 标量，应使用 unknown 保留语义。
            let mut expression = "tool.schema.unknown()".to_string();
            if let Some(description) = schema.get("description") {
                let description = description
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("JSON Schema description 必须是字符串"))?;
                expression.push_str(&format!(
                    ".describe({})",
                    serde_json::to_string(description)?
                ));
            }
            if let Some(default) = schema.get("default") {
                expression.push_str(&format!(
                    ".default({})",
                    serde_json::to_string(default)?
                ));
            }
            return Ok(expression);

            // 旧逻辑：无 type 时立即失败；已停用，仅保留追踪，不能执行。
            // return Err(anyhow::anyhow!("JSON Schema 缺少 type"));
        }
    };
    let (type_name, nullable) = match type_value {
        Value::String(type_name) => (type_name.as_str(), false),
        Value::Array(types) => {
            if types.len() != 2 {
                return Err(anyhow::anyhow!(
                    "JSON Schema type 数组必须包含一个基础类型和 null"
                ));
            }
            let mut base_type = None;
            let mut has_null = false;
            for item in types {
                let item_type = item
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("JSON Schema type 数组元素必须是字符串"))?;
                if item_type == "null" {
                    if has_null {
                        return Err(anyhow::anyhow!(
                            "JSON Schema type 数组不能重复声明 null"
                        ));
                    }
                    has_null = true;
                } else if base_type.replace(item_type).is_some() {
                    return Err(anyhow::anyhow!(
                        "JSON Schema type 数组只能包含一个基础类型"
                    ));
                }
            }
            if !has_null {
                return Err(anyhow::anyhow!(
                    "JSON Schema type 数组必须包含 null"
                ));
            }
            (
                base_type.ok_or_else(|| anyhow::anyhow!("JSON Schema type 数组缺少基础类型"))?,
                true,
            )
        }
        _ => {
            return Err(anyhow::anyhow!(
                "JSON Schema type 必须是字符串或 [基础类型, null]"
            ));
        }
    };

    let enum_value = schema.get("enum");
    let mut expression = if let Some(enum_value) = enum_value {
        // 已停用的旧版 string-only enum 转换完整保留如下，仅作为迁移对照；实际逻辑由标量 literal/union 转换接替：
        // if type_name != "string" {
        //     return Err(anyhow::anyhow!(
        //         "JSON Schema enum 目前只支持 string 类型"
        //     ));
        // }
        // let values = enum_value
        //     .as_array()
        //     .ok_or_else(|| anyhow::anyhow!("JSON Schema enum 必须是非空字符串数组"))?;
        // if values.is_empty() || values.iter().any(|value| !value.is_string()) {
        //     return Err(anyhow::anyhow!(
        //         "JSON Schema enum 必须是非空字符串数组"
        //     ));
        // }
        // let values_source = values
        //     .iter()
        //     .map(serde_json::to_string)
        //     .collect::<std::result::Result<Vec<_>, _>>()?
        //     .join(", ");
        // format!("tool.schema.enum([{values_source}])")
        let values = enum_value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("JSON Schema enum 必须是非空数组"))?;
        if values.is_empty() {
            return Err(anyhow::anyhow!("JSON Schema enum 必须是非空数组"));
        }
        for value in values {
            let value_source = serde_json::to_string(value)?;
            let matches_type = match value {
                Value::String(_) => type_name == "string",
                Value::Bool(_) => type_name == "boolean",
                Value::Number(number) => {
                    (type_name == "number")
                        || (type_name == "integer"
                            && (number.is_i64() || number.is_u64()))
                }
                Value::Null => nullable,
                Value::Array(_) | Value::Object(_) => false,
            };
            if !matches_type {
                return Err(anyhow::anyhow!(
                    "JSON Schema enum 值 {value_source} 与声明 type={type_name} 不匹配"
                ));
            }
        }

        if type_name == "string" && values.iter().all(Value::is_string) {
            let values_source = values
                .iter()
                .map(serde_json::to_string)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join(", ");
            format!("tool.schema.enum([{values_source}])")
        } else {
            let literal_sources = values
                .iter()
                .map(serde_json::to_string)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|value| format!("tool.schema.literal({value})"))
                .collect::<Vec<_>>();
            if literal_sources.len() == 1 {
                literal_sources[0].clone()
            } else {
                format!("tool.schema.union([{}])", literal_sources.join(", "))
            }
        }
    } else {
        match type_name {
            "string" => "tool.schema.string()".to_string(),
            "integer" => "tool.schema.number().int()".to_string(),
            "number" => "tool.schema.number()".to_string(),
            "boolean" => "tool.schema.boolean()".to_string(),
            "array" => {
                let items = schema
                    .get("items")
                    .ok_or_else(|| anyhow::anyhow!("array schema 缺少 items"))?;
                if !items.is_object() {
                    return Err(anyhow::anyhow!("array schema 的 items 必须是对象"));
                }
                format!(
                    "tool.schema.array({})",
                    opencode_zod_schema_source(items)?
                )
            }
            "object" => format!(
                "tool.schema.object({})",
                opencode_object_shape_source(schema)?
            ),
            other => {
                return Err(anyhow::anyhow!(
                    "JSON Schema 不支持 type: {other}"
                ));
            }
        }
    };

    if let Some(format_value) = schema.get("format") {
        let format_name = format_value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("JSON Schema format 必须是字符串"))?;
        if format_name != "double" {
            return Err(anyhow::anyhow!(
                "JSON Schema 不支持 format: {format_name}"
            ));
        }
        if type_name != "number" {
            return Err(anyhow::anyhow!(
                "JSON Schema format=double 只能用于 number 类型"
            ));
        }
    }

    if enum_value.is_none() {
        match type_name {
            "number" | "integer" => {
                for keyword in ["minLength", "maxLength", "pattern", "minItems", "maxItems"] {
                    if schema.get(keyword).is_some() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema {keyword} 与 type={type_name} 不匹配"
                        ));
                    }
                }
                if let Some(minimum) = schema.get("minimum") {
                    if !minimum.is_number() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema minimum 必须是 number"
                        ));
                    }
                    expression.push_str(&format!(
                        ".min({})",
                        serde_json::to_string(minimum)?
                    ));
                }
                if let Some(maximum) = schema.get("maximum") {
                    if !maximum.is_number() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema maximum 必须是 number"
                        ));
                    }
                    expression.push_str(&format!(
                        ".max({})",
                        serde_json::to_string(maximum)?
                    ));
                }
            }
            "string" => {
                for keyword in ["minimum", "maximum", "minItems", "maxItems"] {
                    if schema.get(keyword).is_some() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema {keyword} 与 type=string 不匹配"
                        ));
                    }
                }
                if let Some(min_length) = schema.get("minLength") {
                    if !min_length.is_u64() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema minLength 必须是非负整数"
                        ));
                    }
                    expression.push_str(&format!(
                        ".min({})",
                        serde_json::to_string(min_length)?
                    ));
                }
                if let Some(max_length) = schema.get("maxLength") {
                    if !max_length.is_u64() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema maxLength 必须是非负整数"
                        ));
                    }
                    expression.push_str(&format!(
                        ".max({})",
                        serde_json::to_string(max_length)?
                    ));
                }
                if let Some(pattern) = schema.get("pattern") {
                    let pattern = pattern
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("JSON Schema pattern 必须是字符串"))?;
                    expression.push_str(&format!(
                        ".regex(new RegExp({}))",
                        serde_json::to_string(pattern)?
                    ));
                }
            }
            "array" => {
                for keyword in ["minimum", "maximum", "minLength", "maxLength", "pattern"] {
                    if schema.get(keyword).is_some() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema {keyword} 与 type=array 不匹配"
                        ));
                    }
                }
                if let Some(min_items) = schema.get("minItems") {
                    if !min_items.is_u64() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema minItems 必须是非负整数"
                        ));
                    }
                    expression.push_str(&format!(
                        ".min({})",
                        serde_json::to_string(min_items)?
                    ));
                }
                if let Some(max_items) = schema.get("maxItems") {
                    if !max_items.is_u64() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema maxItems 必须是非负整数"
                        ));
                    }
                    expression.push_str(&format!(
                        ".max({})",
                        serde_json::to_string(max_items)?
                    ));
                }
            }
            "boolean" | "object" => {
                for keyword in [
                    "minimum",
                    "maximum",
                    "minLength",
                    "maxLength",
                    "pattern",
                    "minItems",
                    "maxItems",
                ] {
                    if schema.get(keyword).is_some() {
                        return Err(anyhow::anyhow!(
                            "JSON Schema {keyword} 与 type={type_name} 不匹配"
                        ));
                    }
                }
            }
            _ => unreachable!("unsupported type returned before constraints"),
        }
    } else {
        for keyword in [
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "pattern",
            "minItems",
            "maxItems",
        ] {
            if schema.get(keyword).is_some() {
                return Err(anyhow::anyhow!(
                    "JSON Schema {keyword} 不能与 enum 同时使用"
                ));
            }
        }
    }

    if enum_value.is_none() && nullable {
        expression.push_str(".nullable()");
    }
    if let Some(description) = schema.get("description") {
        let description = description
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("JSON Schema description 必须是字符串"))?;
        expression.push_str(&format!(
            ".describe({})",
            serde_json::to_string(description)?
        ));
    }
    if let Some(default) = schema.get("default") {
        expression.push_str(&format!(
            ".default({})",
            serde_json::to_string(default)?
        ));
    }
    Ok(expression)
}

/// 生成 object 的 raw shape，并在属性层处理 required/optional 语义。
fn opencode_object_shape_source(schema: &Value) -> Result<String> {
    let empty_properties = serde_json::Map::new();
    let properties = match schema.get("properties") {
        Some(properties) => properties
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("object schema 的 properties 必须是对象"))?,
        None => &empty_properties,
    };
    let required_names: HashSet<&str> = match schema.get("required") {
        Some(required) => required
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("object schema 的 required 必须是数组"))?
            .iter()
            .map(|name| {
                name.as_str()
                    .ok_or_else(|| anyhow::anyhow!("object schema 的 required 元素必须是字符串"))
            })
            .collect::<Result<HashSet<_>>>()?,
        None => HashSet::new(),
    };
    for required_name in &required_names {
        if !properties.contains_key(*required_name) {
            return Err(anyhow::anyhow!(
                "object schema 的 required 引用了不存在的属性: {required_name}"
            ));
        }
    }
    if let Some(additional_properties) = schema.get("additionalProperties") {
        if !additional_properties.is_boolean() {
            return Err(anyhow::anyhow!(
                "object schema 的 additionalProperties 必须是 boolean"
            ));
        }
        // OpenCode 的 args raw shape 会统一交给 z.object(args)，无法在此处完整表达
        // 顶层 additionalProperties=true；保留所有声明属性并允许该标记通过。
        // 旧逻辑：additionalProperties 只能是 false；已停用，仅保留追踪，不能执行。
        // if additional_properties.as_bool() != Some(false) {
        //     return Err(anyhow::anyhow!(
        //         "object schema 的 additionalProperties 只能是 false"
        //     ));
        // }
    }

    let mut fields = Vec::with_capacity(properties.len());
    for (property_name, property_schema) in properties {
        if !property_schema.is_object() {
            return Err(anyhow::anyhow!(
                "object schema 属性 {property_name} 的 schema 必须是对象"
            ));
        }
        let mut property_source = opencode_zod_schema_source(property_schema).map_err(|error| {
            anyhow::anyhow!("生成 object schema 属性 {property_name} 失败: {error:#}")
        })?;
        if !required_names.contains(property_name.as_str()) {
            property_source.push_str(".optional()");
        }
        fields.push(format!(
            "{}: {}",
            serde_json::to_string(property_name)?,
            property_source
        ));
    }
    Ok(format!("{{ {} }}", fields.join(", ")))
}

fn write_opencode_computer_control_tool(
    run_dir: &Path,
    callback_url: &str,
    callback_token: &str,
    tool_specs: &[Value],
) -> Result<()> {
    let endpoint = serde_json::to_string(callback_url)?;
    let token = serde_json::to_string(callback_token)?;

    let mut generated_tools = Vec::with_capacity(tool_specs.len());
    let mut conversion_errors = Vec::new();

    // 先完整遍历并转换所有工具，确保未来多个 schema 缺陷一次集中暴露。
    for spec in tool_specs {
        let tool_label = spec
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<缺少名称>");
        let generated = (|| -> Result<String> {
            let name = spec["name"].as_str().ok_or_else(|| {
                anyhow::anyhow!("CUA SDK 工具缺少可用于 OpenCode 导出的 name")
            })?;
            if name.is_empty()
                || !name
                    .chars()
                    .enumerate()
                    .all(|(index, character)| {
                        character == '_'
                            || character.is_ascii_alphanumeric()
                                && (index > 0 || character.is_ascii_alphabetic())
                    })
            {
                return Err(anyhow::anyhow!(
                    "CUA SDK 返回了不能作为 OpenCode 工具导出的名称：{name}"
                ));
            }
            let description = serde_json::to_string(&spec["description"])?;
            let args = opencode_tool_args_source(&spec["inputSchema"])
                .with_context(|| format!("转换 CUA 工具 {name} 的 inputSchema 失败"))?;
            let tool_name = serde_json::to_string(name)?;
            Ok(format!(
                "export const {name} = tool({{\n  description: {description},\n  args: {args},\n  async execute(args, context) {{\n    return invoke({tool_name}, args, context);\n  }},\n}});\n"
            ))
        })();
        match generated {
            Ok(source) => generated_tools.push(source),
            Err(error) => conversion_errors.push(format!("CUA 工具 {tool_label}: {error:#}")),
        }
    }

    // 旧版 fail-fast 循环完整保留如下，仅作为迁移对照；旧代码全部处于注释中，不会执行：
    // for spec in tool_specs {
    //     let args = opencode_tool_args_source(&spec["inputSchema"])?;
    //     source.push_str(&format!("...{args}..."));
    // }
    if !conversion_errors.is_empty() {
        return Err(anyhow::anyhow!(
            "OpenCode CUA 工具 schema 转换失败（{} 个）：\n- {}",
            conversion_errors.len(),
            conversion_errors.join("\n- ")
        ));
    }

    let tools_dir = run_dir.join(".opencode").join("tools");
    std::fs::create_dir_all(&tools_dir)
        .with_context(|| format!("failed to create OpenCode tools directory {}", tools_dir.display()))?;
    let mut source = format!(
        "import {{ tool }} from \"@opencode-ai/plugin\";\n\nconst endpoint = {endpoint};\nconst token = {token};\nasync function invoke(toolName, args, context) {{\n  const response = await fetch(endpoint, {{\n    method: \"POST\",\n    headers: {{ \"content-type\": \"application/json\", \"x-auracoder-computer-control-token\": token }},\n    body: JSON.stringify({{\n      tool: toolName,\n      arguments: args ?? {{}},\n      threadId: context?.sessionID ?? context?.sessionId ?? context?.session_id,\n      turnId: context?.messageID ?? context?.messageId ?? context?.callID ?? context?.callId,\n      callId: context?.callID ?? context?.callId,\n    }}),\n  }});\n  const result = await response.json();\n  if (!response.ok || result?.isError) {{\n    throw new Error(result?.error || `AuraCoder computer control callback failed (HTTP ${{response.status}})`);\n  }}\n  return JSON.stringify(result);\n}}\n\n",
    );
    source.push_str(&generated_tools.join(""));

    // 旧版直接在转换循环中拼装 source 的逻辑已停用；只有全量转换成功后才写入文件。
    // source.push_str(...) must not execute before the conversion_errors check above.

    let tool_file = tools_dir.join("auracoder_computer_control.ts");
    std::fs::write(&tool_file, source)
        .with_context(|| format!("failed to write OpenCode tool file {}", tool_file.display()))?;
    Ok(())
}

/// 为 OpenCode 导出 AuraCoder 会话读取工具，复用本机回调服务器。
fn write_opencode_auracoder_thread_mcp_tool(
    run_dir: &Path,
    callback_url: &str,
    callback_token: &str,
    tool_specs: &[Value],
) -> Result<()> {
    let tools_dir = run_dir.join(".opencode").join("tools");
    std::fs::create_dir_all(&tools_dir)
        .with_context(|| format!("failed to create OpenCode tools directory {}", tools_dir.display()))?;
    let endpoint = serde_json::to_string(callback_url)?;
    let token = serde_json::to_string(callback_token)?;

    // 已停用的旧版聚合文件生成实现完整保留如下，仅作为迁移对照；旧代码全部处于注释中，不会执行：
    // let endpoint = serde_json::to_string(callback_url)?;
    // let token = serde_json::to_string(callback_token)?;
    // let mut source = format!(
    //     "const endpoint = {endpoint};\nconst token = {token};\nasync function invoke(tool, args, context) {{\n  const response = await fetch(endpoint, {{ method: \"POST\", headers: {{ \"content-type\": \"application/json\", \"x-auracoder-computer-control-token\": token }}, body: JSON.stringify({{ tool, toolKind: \"auracoder_thread\", arguments: args ?? {{}}, threadId: context?.sessionID ?? context?.sessionId ?? context?.session_id, turnId: context?.messageID ?? context?.messageId ?? context?.callID ?? context?.callId, callId: context?.callID ?? context?.callId }}) }});\n  const result = await response.json();\n  if (!response.ok) throw new Error(result?.error || `AuraCoder thread callback failed (HTTP ${{response.status}})`);\n  return result;\n}}\n",
    // );
    // for spec in tool_specs {
    //     let Some(name) = spec["name"].as_str() else { continue };
    //     let description = serde_json::to_string(&spec["description"])?;
    //     let input_schema = serde_json::to_string(&spec["inputSchema"])?;
    //     let tool_name = serde_json::to_string(name)?;
    //     source.push_str(&format!(
    //         "export const {name} = {{ description: {description}, parameters: {input_schema}, execute: (args, context) => invoke({tool_name}, args, context) }};\n",
    //     ));
    // }
    // let tool_file = tools_dir.join("auracoder_thread_mcp.ts");
    // std::fs::write(&tool_file, source)
    //     .with_context(|| format!("failed to write OpenCode tool file {}", tool_file.display()))?;

    for spec in tool_specs {
        let Some(name) = spec["name"].as_str() else {
            continue;
        };
        if name.is_empty()
            || !name.chars().enumerate().all(|(index, character)| {
                character == '_'
                    || character.is_ascii_alphanumeric()
                        && (index > 0 || character.is_ascii_alphabetic())
            })
        {
            return Err(anyhow::anyhow!(
                "AuraCoder 会话工具不能作为 OpenCode 工具导出的名称：{name}"
            ));
        }
        let description = serde_json::to_string(&spec["description"])?;
        let args = opencode_tool_args_source(&spec["inputSchema"])
            .with_context(|| format!("转换会话工具 {name} 的 inputSchema 失败"))?;
        let tool_name = serde_json::to_string(name)?;
        let source = format!(
            "import {{ tool }} from \"@opencode-ai/plugin\";\n\nconst endpoint = {endpoint};\nconst token = {token};\nasync function invoke(toolName, args, context) {{\n  const response = await fetch(endpoint, {{\n    method: \"POST\",\n    headers: {{ \"content-type\": \"application/json\", \"x-auracoder-computer-control-token\": token }},\n    body: JSON.stringify({{\n      tool: toolName,\n      toolKind: \"auracoder_thread\",\n      arguments: args ?? {{}},\n      threadId: context?.sessionID ?? context?.sessionId ?? context?.session_id,\n      turnId: context?.messageID ?? context?.messageId ?? context?.callID ?? context?.callId,\n      callId: context?.callID ?? context?.callId,\n    }}),\n  }});\n  const result = await response.json();\n  if (!response.ok || result?.isError) {{\n    throw new Error(result?.error || `AuraCoder thread callback failed (HTTP ${{response.status}})`);\n  }}\n  return JSON.stringify(result);\n}}\n\nexport default tool({{\n  description: {description},\n  args: {args},\n  async execute(args, context) {{\n    return invoke({tool_name}, args, context);\n  }},\n}});\n",
        );
        let tool_file = tools_dir.join(format!("{name}.ts"));
        std::fs::write(&tool_file, source)
            .with_context(|| format!("failed to write OpenCode tool file {}", tool_file.display()))?;
    }
    Ok(())
}

async fn run_opencode_callback_server(
    listener: AsyncTcpListener,
    callback_token: String,
    computer_control_service: Option<Arc<ComputerControlService>>,
    auracoder_thread_mcp_service: Option<Arc<AuraCoderThreadMcpService>>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let token = callback_token.clone();
                let service = computer_control_service.clone();
                let auracoder_thread_service = auracoder_thread_mcp_service.clone();
                tokio::spawn(async move {
                    handle_opencode_callback(stream, &token, service, auracoder_thread_service).await;
                });
            }
        }
    }
}

async fn handle_opencode_callback(
    mut stream: tokio::net::TcpStream,
    callback_token: &str,
    computer_control_service: Option<Arc<ComputerControlService>>,
    auracoder_thread_mcp_service: Option<Arc<AuraCoderThreadMcpService>>,
) {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 1024 * 1024 {
            write_opencode_http_response(&mut stream, 413, json!({"error":"request too large"})).await;
            return;
        }
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = headers.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut auth_token = None;
    let mut content_length = 0_usize;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim().to_ascii_lowercase().as_str() {
                "x-auracoder-computer-control-token" => auth_token = Some(value.trim().to_string()),
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if !request_line.starts_with("POST /invoke ") || auth_token.as_deref() != Some(callback_token) {
        write_opencode_http_response(&mut stream, 401, json!({"error":"unauthorized"})).await;
        return;
    }

    let body_start = header_end + 4;
    while buffer.len().saturating_sub(body_start) < content_length {
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 1024 * 1024 {
            write_opencode_http_response(&mut stream, 413, json!({"error":"request too large"})).await;
            return;
        }
    }

    let request: Value = match serde_json::from_slice(&buffer[body_start..body_start + content_length]) {
        Ok(value) => value,
        Err(error) => {
            write_opencode_http_response(&mut stream, 400, json!({"error": error.to_string()})).await;
            return;
        }
    };
    let tool_kind = request
        .get("toolKind")
        .and_then(Value::as_str)
        .unwrap_or("computer_control");
    let tool = request
        .get("tool")
        .and_then(Value::as_str)
        .map(|tool| {
            if tool_kind == "auracoder_thread" {
                tool.strip_prefix("auracoder_thread_").unwrap_or(tool)
            } else {
                normalize_opencode_tool_name(tool)
            }
        })
        .unwrap_or_default();
    let thread_id = request
        .get("threadId")
        .or_else(|| request.get("sessionId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let turn_id = request
        .get("turnId")
        .or_else(|| request.get("callId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let call_id = request
        .get("callId")
        .and_then(Value::as_str)
        .unwrap_or(turn_id);
    let arguments = request.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let result = if tool_kind == "auracoder_thread" {
        match auracoder_thread_mcp_service.as_ref() {
            Some(service) => service
                .invoke_for_engine("opencode", thread_id, tool, arguments)
                .await,
            None => Err("AuraCoder 会话 MCP 服务尚未绑定到 OpenCode 引擎".to_string()),
        }
    } else {
        match computer_control_service.as_ref() {
            Some(service) => service
                .invoke_for_engine(
                    "opencode",
                    thread_id,
                    turn_id,
                    tool,
                    call_id,
                    arguments,
                    CancellationToken::new(),
                )
                .await,
            None => Err("AuraCoder 电脑操作服务尚未绑定到 OpenCode 引擎".to_string()),
        }
    };
    match result {
        Ok(value) => write_opencode_http_response(&mut stream, 200, value).await,
        Err(error) => {
            write_opencode_http_response(
                &mut stream,
                200,
                json!({
                    "isError": true,
                    "error": error,
                    "content": [{"type":"text","text": error}]
                }),
            )
            .await;
        }
    }
}

fn normalize_opencode_tool_name(tool: &str) -> &str {
    tool.strip_prefix("auracoder_computer_control_").unwrap_or(tool)
}

async fn write_opencode_http_response(stream: &mut tokio::net::TcpStream, status: u16, body: Value) {
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"error\":\"serialization failed\"}".to_vec());
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&payload).await;
}

/*
旧实现由 LocalCliServiceLifecycle 接管，保留源码作为迁移留痕：
async fn start_server(
    cwd: &str,
    mcp_gateway_endpoint: Option<&str>,
    mcp_gateway_token: Option<&str>,
) -> Result<OpenCodeConnection> {
    let executable = resolve_opencode_executable().context("`opencode` executable not found")?;
    let port = allocate_loopback_port()?;
    let password = Uuid::new_v4().to_string();
    let run_dir = runtime_env::app_data_dir()
        .join("computer-control")
        .join("opencode-runs")
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create OpenCode runtime directory {}", run_dir.display()))?;
    let config_dir = run_dir.join(".opencode");
    std::fs::create_dir_all(&config_dir).with_context(|| {
        format!(
            "failed to create isolated OpenCode config directory {}",
            config_dir.display()
        )
    })?;
    anyhow::ensure!(
        mcp_gateway_endpoint.is_some() == mcp_gateway_token.is_some(),
        "OpenCode 的 AuraCoder MCP 配置不完整"
    );
    if let (Some(endpoint), Some(token)) = (mcp_gateway_endpoint, mcp_gateway_token) {
        let config_path = config_dir.join("opencode.json");
        let mut config = if config_path.is_file() {
            let raw = std::fs::read_to_string(&config_path).with_context(|| {
                format!("failed to read isolated OpenCode config {}", config_path.display())
            })?;
            serde_json::from_str::<Value>(&raw).with_context(|| {
                format!("isolated OpenCode config is invalid JSON: {}", config_path.display())
            })?
        } else {
            Value::Object(serde_json::Map::new())
        };
        let config_object = config
            .as_object_mut()
            .context("isolated OpenCode config must be a JSON object")?;
        let mcp_object = config_object
            .entry("mcp".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .context("isolated OpenCode mcp config must be a JSON object")?;
        let gateway_config = runtime_env::opencode_mcp_gateway_authenticated_config(
            endpoint,
            &format!("Bearer {token}"),
        );
        let gateway_entry = gateway_config
            .get("mcp")
            .and_then(Value::as_object)
            .and_then(|mcp| mcp.get("auracoder"))
            .cloned()
            .context("OpenCode MCP Gateway 配置生成失败")?;
        mcp_object.insert("auracoder".to_string(), gateway_entry);
        let encoded = serde_json::to_vec_pretty(&config)
            .context("failed to serialize isolated OpenCode MCP config")?;
        std::fs::write(&config_path, encoded).with_context(|| {
            format!("failed to write isolated OpenCode config {}", config_path.display())
        })?;
    }
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<String>();

    let mut command = Command::new(&executable);
    process_utils::configure_tokio_command(&mut command);
    // 旧实现由 runtime_env::get_opencode_env 接替：
    // runtime_env::apply_missing_login_shell_env(&mut command).await;
    command
        .arg("serve")
        .arg("--hostname")
        .arg(DEFAULT_HOST)
        .arg("--port")
        .arg(port.to_string())
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // 旧 OpenCode 专用环境变量链和手工 PATH 处理由 runtime_env 统一接替：
    // command.env("OPENCODE_SERVER_PASSWORD", &password)
    //     .env("OPENCODE_CONFIG_DIR", run_dir.join(".opencode"))
    //     .env("XDG_CONFIG_HOME", &run_dir);
    // if let Some(path) = executable_augmented_path(&executable) {
    //     command.env("PATH", path);
    // }
    command.envs(runtime_env::get_opencode_env(&executable, &password, &run_dir).await);

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to spawn OpenCode connection at {}",
            executable.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .context("OpenCode stdout not available")?;
    let stderr = child
        .stderr
        .take()
        .context("OpenCode stderr not available")?;

    tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::debug!("opencode stdout: {line}");
            if line.starts_with(SERVER_READY_PREFIX) {
                if let Some(tx) = ready_tx.take() {
                    let url = line
                        .split_whitespace()
                        .last()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("http://{DEFAULT_HOST}:{port}"));
                    let _ = tx.send(url);
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::debug!("opencode stderr: {line}");
        }
    });

    let base_url = timeout(OPENCODE_STARTUP_TIMEOUT, ready_rx)
        .await
        .context("timed out waiting for OpenCode connection startup")?
        .context("OpenCode connection exited before startup completed")?;

    let (event_bus, _) =
        broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    let pump_cancel = CancellationToken::new();
    // 保留服务器结构中的取消令牌；旧 callback 服务器已停用，不再启动额外回调任务。
    let callback_cancel = CancellationToken::new();
    let connection = OpenCodeConnection {
        cwd: cwd.to_string(),
        base_url,
        password,
        child: Mutex::new(Some(child)),
        event_bus: event_bus.clone(),
        pump_cancel: Some(pump_cancel.clone()),
        callback_cancel: Some(callback_cancel.clone()),
        run_dir: Some(run_dir),
        include_directory_header: false,
    };

    wait_for_server_health(&connection).await?;

    let pump_url = connection.base_url.clone();
    let pump_password = connection.password.clone();
    let pump_http = reqwest::Client::new();
    tokio::spawn(async move {
        run_event_pump(
            pump_url,
            pump_password,
            None,
            pump_http,
            event_bus,
            pump_cancel,
        )
        .await;
    });

    // 旧 callback 工具服务器和动态工具文件已停用，MCP 请求统一经过 Gateway。

    Ok(connection)
}
*/
async fn run_event_pump(
    base_url: String,
    password: String,
    directory: Option<String>,
    http: reqwest::Client,
    event_bus: broadcast::Sender<OpenCodeBusItem>,
    cancel: CancellationToken,
) {
    let url = format!("{}/event", base_url.trim_end_matches('/'));
    let mut backoff = Duration::from_millis(100);
    let max_backoff = Duration::from_secs(10);

    loop {
        if cancel.is_cancelled() {
            return;
        }

        let request = http.get(&url).headers(auth_headers(&password));
        let request = if let Some(directory) = directory.as_deref() {
            request.header("X-OpenCode-Directory", directory)
        } else {
            request
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                let message = format!("SSE连接失败：{error}");
                log::warn!("opencode {message}");
                if let Err(send_error) = event_bus.send(OpenCodeBusItem::Failure(message)) {
                    log::debug!(
                        "OpenCode SSE failure event bus send failed: event_type=connection_failure, session_id=None, send_error={send_error}"
                    );
                }
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error) => {
                let message = format!("SSE连接状态异常：{error}");
                log::warn!("opencode {message}");
                if let Err(send_error) = event_bus.send(OpenCodeBusItem::Failure(message)) {
                    log::debug!(
                        "OpenCode SSE failure event bus send failed: event_type=http_status_failure, session_id=None, send_error={send_error}"
                    );
                }
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        backoff = Duration::from_millis(100);
        let mut bytes = response.bytes_stream();
        let mut buffer = String::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                chunk = bytes.next() => {
                    let Some(chunk) = chunk else { break };
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            let message = format!("SSE事件读取失败：{error}");
                            log::warn!("opencode {message}");
                            if let Err(send_error) = event_bus.send(OpenCodeBusItem::Failure(message)) {
                                log::debug!(
                                    "OpenCode SSE failure event bus send failed: event_type=stream_read_failure, session_id=None, send_error={send_error}"
                                );
                            }
                            break;
                        }
                    };
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim_end_matches('\r').to_string();
                        buffer = buffer[line_end + 1..].to_string();
                        if let Some(raw_event) = line.strip_prefix("data:") {
                            let raw_event = raw_event.trim();
                            if raw_event.is_empty() {
                                continue;
                            }
                            let event: OpenCodeBusEvent = match serde_json::from_str(raw_event) {
                                Ok(event) => event,
                                Err(error) => {
                                    log::warn!(
                                        "opencode event parse failed: {error}; event_bytes={}"
                                        , raw_event.len()
                                    );
                                    if let Err(send_error) = event_bus.send(OpenCodeBusItem::Failure(
                                        format!("SSE事件解析失败：{error}"),
                                    )) {
                                        log::debug!(
                                            "OpenCode SSE failure event bus send failed: event_type=parse_failure, session_id=None, send_error={send_error}"
                                        );
                                    }
                                    continue;
                                }
                            };
                            let event_type = event.event_type.clone();
                            let session_id = event_session_id(&event).map(str::to_string);
                            let event = Arc::new(event);
                            match event_bus.send(OpenCodeBusItem::Event(event)) {
                                Ok(receiver_count) => {
                                    log::debug!(
                                        "OpenCode SSE event bus sent: event_type={}, session_id={:?}, receiver_count={}",
                                        event_type,
                                        session_id,
                                        receiver_count,
                                    );
                                }
                                Err(send_error) => {
                                    log::debug!(
                                        "OpenCode SSE event bus send failed: event_type={}, session_id={:?}, send_error={send_error}",
                                        event_type,
                                        session_id,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/*
旧实现中的本机健康等待、端口分配、可执行文件解析和命令执行均已迁移到 LocalCliServiceLifecycle：
async fn wait_for_server_health(connection: &OpenCodeConnection) -> Result<()> {
    let client = reqwest::Client::new();
    let started = Instant::now();
    loop {
        let result = client
            .get(format!(
                "{}/global/health",
                connection.base_url.trim_end_matches('/')
            ))
            .headers(auth_headers(&connection.password))
            .send()
            .await;
        if let Ok(response) = result {
            if let Ok(response) = response.error_for_status() {
                let health = response.json::<OpenCodeHealthResponse>().await?;
                if health.healthy {
                    return Ok(());
                }
            }
        }
        if started.elapsed() > OPENCODE_HEALTH_TIMEOUT {
            anyhow::bail!("OpenCode connection did not become healthy");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn allocate_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind((DEFAULT_HOST, 0))?;
    Ok(listener.local_addr()?.port())
}
*/
fn auth_headers(password: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let token = general_purpose::STANDARD.encode(format!("opencode:{password}"));
    if let Ok(value) = HeaderValue::from_str(&format!("Basic {token}")) {
        headers.insert(AUTHORIZATION, value);
    }
    headers
}

/*
旧实现中的本机可执行文件解析和命令执行已迁移到 LocalCliServiceLifecycle：
fn resolve_opencode_executable() -> Option<PathBuf> {
    runtime_env::resolve_executable("opencode")
}

async fn run_opencode_command(executable: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new(executable);
    process_utils::configure_tokio_command(&mut command);
    // 旧登录 Shell 环境导入和手工 PATH 处理由 runtime_env::get 接替：
    // runtime_env::apply_missing_login_shell_env(&mut command).await;
    command.args(args);
    // if let Some(path) = executable_augmented_path(executable) {
    //     command.env("PATH", path);
    // }
    command.envs(runtime_env::get(executable).await);

    let output = timeout(OPENCODE_COMMAND_TIMEOUT, command.output())
        .await
        .context("timed out running opencode command")?
        .context("failed to run opencode command")?;
    if !output.status.success() {
        anyhow::bail!(
            "opencode command failed with status {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
*/
fn parse_verbose_model_records(output: &str) -> Result<Vec<OpenCodeVerboseModel>> {
    let mut records = Vec::new();
    let mut pending_slug: Option<String> = None;
    let mut json_buffer = String::new();
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut collecting = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if !collecting {
            if parse_model_slug(trimmed).is_some() {
                pending_slug = Some(trimmed.to_string());
                continue;
            }
            if !trimmed.starts_with('{') {
                continue;
            }
            collecting = true;
        }

        json_buffer.push_str(line);
        json_buffer.push('\n');
        for character in line.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if in_string {
                match character {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match character {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }

        if collecting && depth == 0 {
            let mut record: OpenCodeVerboseModel = serde_json::from_str(&json_buffer)
                .context("failed to parse verbose OpenCode model metadata")?;
            if let Some(slug) = pending_slug.take() {
                if let Some(parsed) = parse_model_slug(&slug) {
                    if record.provider_id.trim().is_empty() {
                        record.provider_id = parsed.provider_id;
                    }
                    if record.id.trim().is_empty() {
                        record.id = parsed.model_id;
                    }
                }
            }
            records.push(record);
            json_buffer.clear();
            depth = 0;
            in_string = false;
            escaped = false;
            collecting = false;
        }
    }

    if collecting {
        anyhow::bail!("unterminated verbose OpenCode model JSON object");
    }

    Ok(records)
}

/*
旧 executable_augmented_path 实现由 runtime_env::get 接替，保留代码以便追溯：
fn executable_augmented_path(executable: &Path) -> Option<OsString> {
    runtime_env::augmented_path_with_prepend(
        executable
            .parent()
            .into_iter()
            .map(|value| value.to_path_buf()),
    )
}
*/

/// 从 OpenCode SSE 事件的允许字段中提取安全会话标识，供链路日志和会话匹配共用。
fn event_session_id(event: &OpenCodeBusEvent) -> Option<&str> {
    event
        .properties
        .get("sessionID")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .properties
                .get("info")
                .and_then(|value| value.get("sessionID"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .properties
                .get("part")
                .and_then(|value| value.get("sessionID"))
                .and_then(Value::as_str)
        })
}

/// 判断 OpenCode SSE 事件是否属于当前引擎线程，保持原有字段优先级和过滤语义。
fn event_matches_session(event: &OpenCodeBusEvent, session_id: &str) -> bool {
    event_session_id(event)
        .map(|value| value == session_id)
        .unwrap_or(false)
}

pub fn extract_persisted_approval_route(details: &Value) -> Option<ApprovalRequestRoute> {
    let kind = details.get("_opencodeRequestKind")?.as_str()?.trim();
    let request_id = details.get("_opencodeRequestID")?.as_str()?.trim();
    let session_id = details.get("_opencodeSessionID")?.as_str()?.trim();
    let cwd = details.get("_opencodeCwd")?.as_str()?.trim();
    if kind.is_empty() || request_id.is_empty() || session_id.is_empty() || cwd.is_empty() {
        return None;
    }

    let server_method = match kind {
        "permission" => "opencode/permission",
        "question" => "opencode/question",
        _ => return None,
    };

    let mut raw_request_id = json!({
        "kind": kind,
        "requestID": request_id,
        "sessionID": session_id,
        "cwd": cwd,
    });
    if kind == "question" {
        if let Some(questions) = details.get("questions") {
            raw_request_id["questions"] = questions.clone();
        }
    }

    Some(ApprovalRequestRoute {
        server_method: server_method.to_string(),
        raw_request_id,
    })
}

fn is_user_message(message_roles: &HashMap<String, String>, message_id: &str) -> bool {
    message_roles
        .get(message_id)
        .map(|role| role == "user")
        .unwrap_or(false)
}

fn session_error_message(properties: &Value) -> String {
    properties
        .get("error")
        .and_then(|value| value.get("data"))
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            properties
                .get("error")
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("OpenCode session failed")
        .to_string()
}

fn format_session_diff(properties: &Value) -> Option<String> {
    let diffs = properties.get("diff")?.as_array()?;
    let mut output = String::new();
    for diff in diffs {
        let file = diff.get("file").and_then(Value::as_str).unwrap_or("file");
        let patch = diff.get("patch").and_then(Value::as_str).unwrap_or("");
        if patch.is_empty() {
            continue;
        }
        output.push_str(&format!("diff -- {file}\n{patch}\n"));
    }
    (!output.is_empty()).then_some(output)
}

fn action_type_for_permission(permission: &str) -> ActionType {
    match permission {
        "bash" => ActionType::Command,
        "edit" => ActionType::FileEdit,
        "read" => ActionType::FileRead,
        "webfetch" | "websearch" | "codesearch" => ActionType::Search,
        _ => ActionType::Other,
    }
}

fn action_type_for_tool(tool: &str) -> ActionType {
    let normalized = tool.to_lowercase();
    if normalized.contains("bash") || normalized.contains("command") {
        ActionType::Command
    } else if normalized.contains("edit") || normalized.contains("write") {
        ActionType::FileEdit
    } else if normalized.contains("read") {
        ActionType::FileRead
    } else if normalized.contains("grep") || normalized.contains("search") {
        ActionType::Search
    } else {
        ActionType::Other
    }
}

fn question_id(index: usize, question: &OpenCodeQuestionInfo) -> String {
    let mut normalized = question
        .header
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while normalized.contains("--") {
        normalized = normalized.replace("--", "-");
    }
    let normalized = normalized.trim_matches('-');
    if normalized.is_empty() {
        format!("question-{index}")
    } else {
        format!("question-{index}-{normalized}")
    }
}

fn question_details_json((index, question): (usize, &OpenCodeQuestionInfo)) -> Value {
    json!({
        "id": question_id(index, question),
        "header": question.header,
        "question": question.question,
        "multiple": question.multiple.unwrap_or(false),
        "custom": question.custom.unwrap_or(true),
        "options": question.options.iter().map(|option| {
            json!({
                "label": option.label,
                "description": option.description,
            })
        }).collect::<Vec<_>>(),
    })
}

fn should_reject_question_response(response: &Value) -> bool {
    matches!(
        response.get("decision").and_then(Value::as_str),
        Some("decline" | "cancel")
    )
}

fn build_question_answers(
    questions: &[OpenCodeQuestionInfo],
    answers: Option<&Value>,
) -> Vec<Vec<String>> {
    let answer_object = answers.and_then(Value::as_object);
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let candidates = [
                question_id(index, question),
                question.header.clone(),
                question.question.clone(),
            ];
            for candidate in candidates {
                if let Some(answer) = answer_object.and_then(|object| object.get(&candidate)) {
                    return answer_to_vec(answer);
                }
            }
            Vec::new()
        })
        .collect()
}

fn answer_to_vec(value: &Value) -> Vec<String> {
    if let Some(text) = value.as_str() {
        return non_empty_answer(text).into_iter().collect();
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .filter_map(Value::as_str)
            .filter_map(non_empty_answer)
            .collect();
    }
    if let Some(object) = value.as_object() {
        if let Some(array) = object.get("answers").and_then(Value::as_array) {
            return array
                .iter()
                .filter_map(Value::as_str)
                .filter_map(non_empty_answer)
                .collect();
        }
        if let Some(label) = object.get("label").and_then(Value::as_str) {
            return non_empty_answer(label).into_iter().collect();
        }
    }
    Vec::new()
}

fn non_empty_answer(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn token_usage_from_step_finish(part: &OpenCodePart) -> Option<TokenUsage> {
    if part.part_type != "step-finish" {
        return None;
    }

    let tokens = part.tokens.as_ref()?;
    Some(TokenUsage {
        input: tokens.input,
        output: tokens.output,
        reasoning: Some(tokens.reasoning),
        cache_read: Some(tokens.cache.read),
        cache_write: Some(tokens.cache.write),
        cost_usd: part.cost,
    })
}

fn file_url(path: &str) -> String {
    let mut encoded = String::from("file://");
    for byte in path.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(*byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn new_message_id() -> String {
    let now_ms = current_unix_time_millis().max(0) as u64;
    let sort_value = next_opencode_message_sort_value(now_ms);
    format!(
        "msg_{:012x}{}",
        sort_value & OPENCODE_ID_TIME_MASK,
        random_base62(OPENCODE_MESSAGE_ID_RANDOM_LEN)
    )
}

fn next_opencode_message_sort_value(now_ms: u64) -> u64 {
    let base = now_ms.saturating_mul(OPENCODE_ID_COUNTER_STEP);

    loop {
        let last = LAST_OPENCODE_MESSAGE_SORT_VALUE.load(Ordering::Relaxed);
        let candidate = if base <= last {
            last.saturating_add(1)
        } else {
            base.saturating_add(1)
        };

        if LAST_OPENCODE_MESSAGE_SORT_VALUE
            .compare_exchange_weak(last, candidate, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return candidate;
        }
    }
}

fn random_base62(len: usize) -> String {
    const CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    let mut output = String::with_capacity(len);
    while output.len() < len {
        let uuid = Uuid::new_v4();
        for byte in uuid.as_bytes() {
            output.push(CHARS[*byte as usize % CHARS.len()] as char);
            if output.len() == len {
                break;
            }
        }
    }
    output
}

#[cfg(test)]
fn opencode_sort_prefix_for_millis(now_ms: u64, counter: u64) -> String {
    format!(
        "{:012x}",
        now_ms
            .saturating_mul(OPENCODE_ID_COUNTER_STEP)
            .saturating_add(counter)
            & OPENCODE_ID_TIME_MASK
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::engines::TurnAttachment;

    fn test_remote_engine(base_url: String) -> OpenCodeEngine {
        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url,
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: false,
            })),
        }
    }

    async fn assert_permission_asked_mode(
        mode: OpenCodePermissionMode,
        expected_reply: Option<&'static str>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let (base_url, server_task) = if let Some(expected_reply) = expected_reply {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let (body_start, content_length) = loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0, "mock client closed before permission reply");
                    request_bytes.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) =
                        request_bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let body_start = header_end + 4;
                        let headers = String::from_utf8_lossy(&request_bytes[..header_end])
                            .to_ascii_lowercase();
                        let content_length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request_bytes.len() >= body_start + content_length {
                            break (body_start, content_length);
                        }
                    }
                };
                let request =
                    String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
                assert!(request.starts_with("POST /permission/request-1/reply HTTP/1.1"));
                assert!(request.contains(&format!("\"reply\":\"{expected_reply}\"")));
                let response =
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            (format!("http://{address}"), Some(task))
        } else {
            ("http://127.0.0.1:9".to_string(), None)
        };

        let engine = test_remote_engine(base_url);
        let connection = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        engine.state.lock().await.sessions.insert(
            "ses_test".to_string(),
            OpenCodeSession {
                cwd: "/var/work/project-a".to_string(),
                model_id: "openai/gpt-5".to_string(),
                reasoning_effort: None,
                agent: None,
                permission_mode: mode,
                connection: connection.clone(),
            },
        );

        let (event_tx, mut event_rx) = mpsc::channel(4);
        engine
            .handle_permission_asked(
                &json!({
                    "id": "request-1",
                    "sessionID": "ses_test",
                    "permission": "bash",
                    "patterns": ["*"]
                }),
                &event_tx,
                connection,
            )
            .await;
        drop(event_tx);

        if expected_reply.is_some() {
            assert!(event_rx.try_recv().is_err());
            server_task.unwrap().await.unwrap();
        } else {
            match event_rx.recv().await {
                Some(EngineEvent::ApprovalRequested { details, .. }) => {
                    assert_eq!(details["_opencodeRequestKind"], json!("permission"));
                }
                _ => panic!("expected OpenCode permission approval event"),
            }
        }
    }


    #[tokio::test]
    async fn listener_failure_is_forwarded_to_the_active_turn() {
        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        let mut incoming = spawn_opencode_incoming_pump(event_bus.clone());

        event_bus
            .send(OpenCodeBusItem::Failure("event stream failed".to_string()))
            .expect("active turn must subscribe to the event bus");

        match incoming.recv().await {
            Some(OpenCodeIncomingEvent::Failure(message)) => {
                assert_eq!(message, "event stream failed");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remote_http_target_uses_shared_endpoint_and_directory_header() {
        let engine = OpenCodeEngine::new_remote_http(
            "http://127.0.0.1:43101".to_string(),
            "runtime-secret".to_string(),
        );
        assert!(engine.is_available().await);

        let connection = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        let request = engine
            .request(connection.as_ref(), reqwest::Method::GET, "/provider")
            .build()
            .unwrap();

        assert_eq!(
            request
                .headers()
                .get("X-OpenCode-Directory")
                .and_then(|value| value.to_str().ok()),
            Some("/var/work/project-a")
        );
        assert!(request.headers().contains_key(AUTHORIZATION));

        let machine_server = engine.connection_for_cwd("").await.unwrap();
        let machine_model_request = engine
            .request(machine_server.as_ref(), reqwest::Method::GET, "/provider")
            .build()
            .unwrap();
        assert!(!machine_model_request
            .headers()
            .contains_key("X-OpenCode-Directory"));

        let read_request = engine
            .http
            .request(
                reqwest::Method::GET,
                "http://127.0.0.1:43101/session/ses_test",
            )
            .headers(auth_headers("runtime-secret"))
            .build()
            .unwrap();
        assert_eq!(read_request.url().path(), "/session/ses_test");
        assert!(read_request.url().query().is_none());
        assert!(!read_request.headers().contains_key("X-OpenCode-Directory"));
    }

    #[tokio::test]
    async fn remote_event_pump_subscribes_to_each_workspace_directory() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let mut directories = Vec::new();
            let mut streams = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request_bytes = vec![0_u8; 4096];
                let mut length = 0;
                while !request_bytes[..length]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
                {
                    let read = stream.read(&mut request_bytes[length..]).await.unwrap();
                    assert!(read > 0, "event pump closed before sending HTTP headers");
                    length += read;
                }
                let request = String::from_utf8_lossy(&request_bytes[..length]);
                assert!(request.starts_with("GET /event HTTP/1.1"));
                assert!(request.lines().any(|line| {
                    line.split_once(':').is_some_and(|(name, _)| {
                        name.eq_ignore_ascii_case("authorization")
                    })
                }));
                let directory = request
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("x-opencode-directory")
                            .then(|| value.trim().to_string())
                    })
                    .expect("remote event subscription must include its workspace directory");
                directories.push(directory);
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
                    )
                    .await
                    .unwrap();
                streams.push(stream);
            }
            directories
        });

        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        let engine = OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url: format!("http://{address}"),
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: true,
            })),
        };

        let project_a = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        let project_b = engine.connection_for_cwd("/var/work/project-b").await.unwrap();
        let mut directories = timeout(Duration::from_secs(2), server_task)
            .await
            .expect("workspace event pumps did not connect")
            .unwrap();
        project_a.cancel_event_pump();
        project_b.cancel_event_pump();

        directories.sort();
        assert_eq!(
            directories,
            vec![
                "/var/work/project-a".to_string(),
                "/var/work/project-b".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn read_session_requests_only_id_path_with_basic_auth() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request_bytes = vec![0_u8; 4096];
            let mut length = 0;
            while !request_bytes[..length]
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                let read = stream.read(&mut request_bytes[length..]).await.unwrap();
                assert!(read > 0, "mock client closed before sending HTTP headers");
                length += read;
            }
            let request = String::from_utf8_lossy(&request_bytes[..length]).to_ascii_lowercase();

            // 恢复接口只能按 ID 请求，不能把本机目录作为 query 或目录请求头发送。
            assert!(request.starts_with("get /session/ses_test http/1.1"));
            assert!(!request.contains("?directory="));
            assert!(!request.contains("x-opencode-directory:"));
            assert!(request.contains("authorization: basic "));

            let body = r#"{"id":"ses_test","title":"Restored","directory":"/var/work/project-a","permission":null,"time":{"created":1,"updated":2,"archived":0}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        // 直接组装远端 endpoint，避免测试额外启动 SSE pump 抢占 mock listener；
        // 被测 read_session 仍然走 OpenCodeEngine 的真实 HTTP 请求实现。
        let engine = OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url: format!("http://{address}"),
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: false,
            })),
        };
        let summary = engine
            .read_session("/var/work/project-a", "ses_test")
            .await
            .unwrap();
        assert_eq!(summary.engine_thread_id, "ses_test");
        assert_eq!(summary.cwd, "/var/work/project-a");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn read_session_preserves_http_404_for_cli_mapping() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response =
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        // 与成功请求测试一致，不启动 SSE pump，确保本测试只验证 read_session 的 404 传播。
        let engine = OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url: format!("http://{address}"),
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: false,
            })),
        };

        let error = engine
            .read_session("/var/work/project-a", "ses_missing")
            .await
            .expect_err("HTTP 404 must be returned to the CLI mapping layer");
        let request_error = error
            .downcast_ref::<reqwest::Error>()
            .expect("anyhow context must preserve the reqwest source error");
        assert_eq!(request_error.status(), Some(reqwest::StatusCode::NOT_FOUND));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn set_session_permission_mode_patches_permission_and_updates_cache() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let (body_start, content_length) = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "mock client closed before sending PATCH request");
                request_bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request_bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let body_start = header_end + 4;
                    let headers = String::from_utf8_lossy(&request_bytes[..header_end]).to_ascii_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request_bytes.len() >= body_start + content_length {
                        break (body_start, content_length);
                    }
                }
            };
            let request = String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
            assert!(request.starts_with("PATCH /session/ses_test HTTP/1.1"));
            let body = &request_bytes[body_start..body_start + content_length];
            let body: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(
                body,
                json!({
                    "permission": permission_rules(OpenCodePermissionMode::Allow)
                })
            );
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        let engine = OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url: format!("http://{address}"),
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: false,
            })),
        };
        let connection = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        engine.state.lock().await.sessions.insert(
            "ses_test".to_string(),
            OpenCodeSession {
                cwd: "/var/work/project-a".to_string(),
                model_id: "openai/gpt-5".to_string(),
                reasoning_effort: None,
                agent: None,
                permission_mode: OpenCodePermissionMode::Ask,
                connection,
            },
        );
        engine
            .set_session_permission_mode(
                "/var/work/project-a",
                "ses_test",
                &json!("allow"),
            )
            .await
            .unwrap();
        assert_eq!(
            engine
                .state
                .lock()
                .await
                .sessions
                .get("ses_test")
                .map(|session| session.permission_mode),
            Some(OpenCodePermissionMode::Allow)
        );
        server_task.await.unwrap();
    }

    async fn assert_empty_permission_rules_update_cache_to_ask(
        initial_mode: OpenCodePermissionMode,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let (body_start, content_length) = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "mock client closed before sending empty PATCH request");
                request_bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) =
                    request_bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let body_start = header_end + 4;
                    let headers = String::from_utf8_lossy(&request_bytes[..header_end])
                        .to_ascii_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request_bytes.len() >= body_start + content_length {
                        break (body_start, content_length);
                    }
                }
            };
            let request = String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
            assert!(request.starts_with("PATCH /session/ses_empty HTTP/1.1"));
            let body = &request_bytes[body_start..body_start + content_length];
            assert_eq!(
                std::str::from_utf8(body).unwrap(),
                r#"{"permission":[]}"#
            );
            let response =
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let engine = test_remote_engine(format!("http://{address}"));
        let connection = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        engine.state.lock().await.sessions.insert(
            "ses_empty".to_string(),
            OpenCodeSession {
                cwd: "/var/work/project-a".to_string(),
                model_id: "openai/gpt-5".to_string(),
                reasoning_effort: None,
                agent: None,
                permission_mode: initial_mode,
                connection,
            },
        );
        engine
            .set_session_permission_rules(
                "/var/work/project-a",
                "ses_empty",
                &json!([]),
            )
            .await
            .unwrap();
        assert_eq!(
            engine
                .state
                .lock()
                .await
                .sessions
                .get("ses_empty")
                .map(|session| session.permission_mode),
            Some(OpenCodePermissionMode::Ask)
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn set_session_empty_permission_rules_patches_empty_array_and_updates_cache_to_ask() {
        assert_empty_permission_rules_update_cache_to_ask(OpenCodePermissionMode::Allow).await;
        assert_empty_permission_rules_update_cache_to_ask(OpenCodePermissionMode::Deny).await;
    }

    #[tokio::test]
    async fn set_session_permission_mode_http_failure_does_not_update_cache() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response =
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let (event_bus, _) =
            broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
        let engine = OpenCodeEngine {
            state: Arc::new(Mutex::new(OpenCodeState::default())),
            http: reqwest::Client::new(),
            computer_control_service: Arc::new(std::sync::Mutex::new(None)),
            auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
            target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
                base_url: format!("http://{address}"),
                password: "runtime-secret".to_string(),
                event_bus,
                pump_cancel: CancellationToken::new(),
                workspace_event_pump_enabled: false,
            })),
        };
        let connection = engine.connection_for_cwd("/var/work/project-a").await.unwrap();
        engine.state.lock().await.sessions.insert(
            "ses_test".to_string(),
            OpenCodeSession {
                cwd: "/var/work/project-a".to_string(),
                model_id: "openai/gpt-5".to_string(),
                reasoning_effort: None,
                agent: None,
                permission_mode: OpenCodePermissionMode::Ask,
                connection,
            },
        );
        assert!(engine
            .set_session_permission_mode(
                "/var/work/project-a",
                "ses_test",
                &json!("allow"),
            )
            .await
            .is_err());
        assert_eq!(
            engine
                .state
                .lock()
                .await
                .sessions
                .get("ses_test")
                .map(|session| session.permission_mode),
            Some(OpenCodePermissionMode::Ask)
        );
        server_task.await.unwrap();
    }

    #[test]
    fn parse_model_slug_splits_on_first_slash() {
        let parsed = parse_model_slug("openrouter/anthropic/claude-sonnet-4.5").unwrap();
        assert_eq!(parsed.provider_id, "openrouter");
        assert_eq!(parsed.model_id, "anthropic/claude-sonnet-4.5");
        assert!(parse_model_slug("missing-provider").is_none());
    }

    #[test]
    fn verbose_models_expose_reasoning_variants() {
        let output = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "status": "active",
  "limit": { "context": 200000, "input": 200000, "output": 100000 },
  "capabilities": {
    "reasoning": true,
    "attachment": false,
    "input": { "text": true, "image": false, "pdf": false }
  },
  "variants": {
    "high": { "thinking": { "type": "enabled" } },
    "max": { "thinking": { "type": "enabled" } }
  }
}
opencode/gpt-5-nano
{
  "id": "gpt-5-nano",
  "providerID": "opencode",
  "name": "GPT-5 Nano",
  "status": "active",
  "limit": { "context": 400000, "input": 200000, "output": 128000 },
  "capabilities": {
    "reasoning": true,
    "attachment": true,
    "input": { "text": true, "image": true, "pdf": true }
  },
  "variants": {
    "minimal": { "reasoningEffort": "minimal" },
    "low": { "reasoningEffort": "low" },
    "medium": { "reasoningEffort": "medium" },
    "high": { "reasoningEffort": "high" }
  }
}"#;

        let records = parse_verbose_model_records(output).unwrap();
        assert_eq!(records.len(), 2);
        let big_pickle_efforts = reasoning_efforts_from_variants(&records[0].variants)
            .into_iter()
            .map(|option| option.reasoning_effort)
            .collect::<Vec<_>>();
        assert_eq!(big_pickle_efforts, vec!["high", "max"]);
        let nano_efforts = reasoning_efforts_from_variants(&records[1].variants)
            .into_iter()
            .map(|option| option.reasoning_effort)
            .collect::<Vec<_>>();
        assert_eq!(nano_efforts, vec!["minimal", "low", "medium", "high"]);
        assert_eq!(
            model_modalities_from_capabilities(records[1].capabilities.as_ref()),
            vec!["text".to_string(), "image".to_string(), "pdf".to_string()]
        );
        assert_eq!(
            attachment_modalities_from_capabilities(records[0].capabilities.as_ref()),
            Vec::<String>::new()
        );
        assert_eq!(
            attachment_modalities_from_capabilities(records[1].capabilities.as_ref()),
            vec!["text".to_string(), "image".to_string(), "pdf".to_string()]
        );
        let limits = model_limits(records[1].limit.as_ref()).expect("limits");
        assert_eq!(limits.context_tokens, Some(400000));
        assert_eq!(limits.output_tokens, Some(128000));
        assert_eq!(
            model_limits(records[0].limit.as_ref()).and_then(|limits| limits.input_tokens),
            Some(200000)
        );
    }

    #[test]
    fn prompt_body_includes_selected_opencode_variant() {
        let body = build_prompt_body(
            "opencode/big-pickle",
            Some("max"),
            None,
            TurnInput {
                message: "hello".to_string(),
                attachments: Vec::new(),
                plan_mode: false,
                input_items: Vec::new(),
            },
        )
        .unwrap()
        .body;

        assert_eq!(body.get("variant"), Some(&json!("max")));
        assert_eq!(body["model"]["providerID"], json!("opencode"));
        assert_eq!(body["model"]["modelID"], json!("big-pickle"));
    }

    #[test]
    fn remote_attachment_prompt_uses_the_uploaded_remote_file_url() {
        let body = build_prompt_body(
            "opencode/big-pickle",
            None,
            None,
            TurnInput {
                message: "读取附件".to_string(),
                attachments: vec![TurnAttachment {
                    file_name: "说明.txt".to_string(),
                    file_path: "/home/tester/.cache/auracoder/attachments/workspace/thread/file.txt"
                        .to_string(),
                    preview_file_path: None,
                    size_bytes: 12,
                    mime_type: Some("text/plain".to_string()),
                    browser_annotation: None,
                    is_remote: true,
                    remote_text_content: Some("附件正文".to_string()),
                }],
                plan_mode: false,
                input_items: Vec::new(),
            },
        )
        .expect("prompt body")
        .body;

        assert_eq!(
            body["parts"][1]["url"],
            json!("file:///home/tester/.cache/auracoder/attachments/workspace/thread/file.txt")
        );
        assert_eq!(body["parts"][1]["filename"], json!("说明.txt"));
    }

    #[test]
    fn prompt_body_includes_selected_opencode_agent() {
        let body = build_prompt_body(
            "opencode/big-pickle",
            None,
            Some("explore"),
            TurnInput {
                message: "hello".to_string(),
                attachments: Vec::new(),
                plan_mode: false,
                input_items: Vec::new(),
            },
        )
        .unwrap()
        .body;

        assert_eq!(body.get("agent"), Some(&json!("explore")));
    }

    #[test]
    fn prompt_body_ignores_generic_plan_mode_text_for_opencode() {
        let body = build_prompt_body(
            "opencode/big-pickle",
            None,
            Some("explore"),
            TurnInput {
                message: "hello".to_string(),
                attachments: Vec::new(),
                plan_mode: true,
                input_items: Vec::new(),
            },
        )
        .unwrap()
        .body;

        assert_eq!(body.get("agent"), Some(&json!("explore")));
        assert_eq!(body["parts"][0]["text"], json!("hello"));
    }

    #[test]
    fn build_prompt_body_returns_same_message_id_it_sends() {
        let prompt = build_prompt_body(
            "opencode/big-pickle",
            None,
            None,
            TurnInput {
                message: "hello".to_string(),
                attachments: Vec::new(),
                plan_mode: false,
                input_items: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(
            prompt.body.get("messageID").and_then(Value::as_str),
            Some(prompt.message_id.as_str())
        );
        assert!(prompt.body.get("tools").is_none());
        assert!(!json_contains_key(&prompt.body, "eager_input_streaming"));
    }

    fn json_contains_key(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(key)
                    || object.values().any(|value| json_contains_key(value, key))
            }
            Value::Array(items) => items.iter().any(|value| json_contains_key(value, key)),
            _ => false,
        }
    }

    #[test]
    fn model_reasoning_effort_omits_models_without_variants() {
        let model = model_info(
            "openrouter/example/plain-model",
            "Plain Model",
            "OpenCode model",
            false,
            Vec::new(),
            vec!["text".to_string()],
        );

        assert_eq!(resolve_model_reasoning_effort(&model, Some("medium")), None);
    }

    #[test]
    fn model_reasoning_effort_falls_back_to_supported_default() {
        let model = model_info(
            "opencode/big-pickle",
            "Big Pickle",
            "OpenCode model",
            true,
            reasoning_efforts_from_variant_names(&["high", "max"]),
            vec!["text".to_string()],
        );

        assert_eq!(
            resolve_model_reasoning_effort(&model, Some("medium")).as_deref(),
            Some("high")
        );
        assert_eq!(
            resolve_model_reasoning_effort(&model, Some("max")).as_deref(),
            Some("max")
        );
    }

    #[test]
    fn opencode_only_model_catalog_is_not_cached() {
        let opencode_only = vec![model_info(
            "opencode/big-pickle",
            "Big Pickle",
            "OpenCode model",
            true,
            Vec::new(),
            vec!["text".to_string()],
        )];
        let mixed = vec![
            opencode_only[0].clone(),
            model_info(
                "openrouter/anthropic/claude-sonnet-4.5",
                "Claude Sonnet 4.5",
                "OpenRouter model",
                false,
                Vec::new(),
                vec!["text".to_string()],
            ),
        ];

        assert!(!should_cache_runtime_model_catalog(&opencode_only));
        assert!(should_cache_runtime_model_catalog(&mixed));
    }

    #[test]
    fn permission_mode_maps_existing_policy_names() {
        assert_eq!(
            permission_mode_from_policy(Some(&json!("ask"))),
            OpenCodePermissionMode::Ask
        );
        assert_eq!(
            permission_mode_from_policy(Some(&json!("allow"))),
            OpenCodePermissionMode::Allow
        );
        assert_eq!(
            permission_mode_from_policy(Some(&json!("deny"))),
            OpenCodePermissionMode::Deny
        );
        assert_eq!(
            permission_mode_from_policy(Some(&json!("trusted"))),
            OpenCodePermissionMode::Allow
        );
        assert_eq!(
            permission_mode_from_policy(Some(&json!("untrusted"))),
            OpenCodePermissionMode::Deny
        );
        assert_eq!(
            permission_mode_from_policy(Some(&json!("on-request"))),
            OpenCodePermissionMode::Ask
        );
    }

    #[test]
    fn session_wildcard_permission_action_uses_last_global_rule() {
        let permission = json!([
            { "permission": "*", "pattern": "*", "action": "ask" },
            { "permission": "bash", "pattern": "*", "action": "allow" },
            { "permission": "*", "pattern": "*", "action": "allow" }
        ]);
        assert_eq!(
            session_wildcard_permission_action(Some(&permission)),
            Some("allow")
        );
    }

    #[tokio::test]
    async fn permission_asked_allow_replies_always_without_approval_event() {
        assert_permission_asked_mode(OpenCodePermissionMode::Allow, Some("always")).await;
    }

    #[tokio::test]
    async fn permission_asked_deny_replies_reject_without_approval_event() {
        assert_permission_asked_mode(OpenCodePermissionMode::Deny, Some("reject")).await;
    }

    #[tokio::test]
    async fn permission_asked_ask_emits_approval_event() {
        assert_permission_asked_mode(OpenCodePermissionMode::Ask, None).await;
    }

    #[test]
    fn question_answers_follow_opencode_question_order() {
        let questions = vec![
            OpenCodeQuestionInfo {
                question: "Which package manager?".to_string(),
                header: "Package Manager".to_string(),
                options: vec![],
                multiple: None,
                custom: None,
            },
            OpenCodeQuestionInfo {
                question: "Run tests?".to_string(),
                header: "Tests".to_string(),
                options: vec![],
                multiple: None,
                custom: None,
            },
        ];
        let answers = build_question_answers(
            &questions,
            Some(&json!({
                "question-0-package-manager": { "answers": ["pnpm"] },
                "Tests": "yes"
            })),
        );

        assert_eq!(
            answers,
            vec![vec!["pnpm".to_string()], vec!["yes".to_string()]]
        );
    }

    #[test]
    fn question_details_preserve_opencode_selection_flags() {
        let question = OpenCodeQuestionInfo {
            question: "Which checks should OpenCode run?".to_string(),
            header: "Checks".to_string(),
            options: vec![OpenCodeQuestionOption {
                label: "typecheck".to_string(),
                description: "Run TypeScript".to_string(),
            }],
            multiple: Some(true),
            custom: Some(false),
        };

        let details = question_details_json((0, &question));

        assert_eq!(details["id"], json!("question-0-checks"));
        assert_eq!(details["multiple"], json!(true));
        assert_eq!(details["custom"], json!(false));
        assert_eq!(details["options"][0]["label"], json!("typecheck"));
    }

    #[test]
    fn decline_and_cancel_reject_opencode_questions() {
        assert!(should_reject_question_response(
            &json!({ "decision": "decline" })
        ));
        assert!(should_reject_question_response(
            &json!({ "decision": "cancel" })
        ));
        assert!(!should_reject_question_response(&json!({
            "answers": { "question-0-checks": { "answers": ["typecheck"] } }
        })));
    }

    #[test]
    fn step_finish_part_maps_rich_token_usage() {
        let part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_123",
            "messageID": "msg_123",
            "type": "step-finish",
            "reason": "stop",
            "cost": 0.0123,
            "tokens": {
                "input": 100,
                "output": 25,
                "reasoning": 10,
                "cache": { "read": 7, "write": 3 },
                "total": 145
            }
        }))
        .expect("step-finish part should deserialize");

        let usage = token_usage_from_step_finish(&part).expect("token usage");

        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 25);
        assert_eq!(usage.reasoning, Some(10));
        assert_eq!(usage.cache_read, Some(7));
        assert_eq!(usage.cache_write, Some(3));
        assert_eq!(usage.cost_usd, Some(0.0123));
    }

    #[test]
    fn session_record_maps_archived_zero_as_active() {
        let session = map_session_record(OpenCodeSessionRecord {
            id: "ses_123".to_string(),
            title: Some("  Existing session  ".to_string()),
            directory: "/workspace".to_string(),
            permission: Some(permission_rules(OpenCodePermissionMode::Ask)),
            time: OpenCodeSessionTime {
                created: 1_777_155_663_506,
                updated: 1_777_155_663_524,
                archived: Some(0),
            },
        });

        assert_eq!(session.engine_thread_id, "ses_123");
        assert_eq!(session.title.as_deref(), Some("Existing session"));
        assert_eq!(session.cwd, "/workspace");
        assert!(!session.archived);
    }

    #[test]
    fn session_permission_match_compares_current_rules() {
        let session = OpenCodeSessionRecord {
            id: "ses_123".to_string(),
            title: None,
            directory: "/workspace".to_string(),
            permission: Some(json!([
                { "permission": "question", "pattern": "*", "action": "allow" },
                { "permission": "*", "pattern": "*", "action": "ask" }
            ])),
            time: OpenCodeSessionTime {
                created: 1,
                updated: 1,
                archived: None,
            },
        };

        assert!(session_permission_matches(
            &session,
            OpenCodePermissionMode::Ask
        ));
        assert!(!session_permission_matches(
            &session,
            OpenCodePermissionMode::Allow
        ));
    }

    #[test]
    fn file_url_escapes_local_paths() {
        assert_eq!(
            file_url("/tmp/auracoder test/file.txt"),
            "file:///tmp/auracoder%20test/file.txt"
        );
    }

    #[test]
    fn generated_computer_control_tools_use_isolated_callback_contract() {
        let run_dir = std::env::temp_dir().join(format!(
            "auracoder-opencode-tools-test-{}",
            Uuid::new_v4()
        ));
        write_opencode_computer_control_tool(
            &run_dir,
            "http://127.0.0.1:45678/invoke",
            "one-time-token",
            &[json!({
                "name": "click",
                "description": "SDK supplied click tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer", "description": "Process id" },
                        "button": { "type": "string" }
                    },
                    "required": ["pid"]
                }
            }), json!({
                "name": "verify_state",
                "description": "SDK supplied verify_state tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer" },
                        "expect": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "element": {
                                        "type": ["object", "null"],
                                        "properties": {
                                            "exists": { "type": "boolean", "enum": [true] }
                                        },
                                        "required": ["exists"],
                                        "additionalProperties": false
                                    }
                                },
                                "required": ["element"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["pid", "expect"],
                    "additionalProperties": false
                }
            }), json!({
                "name": "set_config",
                "description": "SDK supplied set_config tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "value": {
                            "description": "New value for `key`. JSON type depends on the key."
                        }
                    },
                    "required": ["key"],
                    "additionalProperties": true
                }
            })],
        )
        .expect("OpenCode tool source should be generated");

        let tool_file = run_dir
            .join(".opencode")
            .join("tools")
            .join("auracoder_computer_control.ts");
        let source = std::fs::read_to_string(&tool_file).expect("tool source should exist");
        assert!(source.contains("import { tool } from \"@opencode-ai/plugin\""));
        assert!(source.contains("x-auracoder-computer-control-token"));
        assert!(source.contains("http://127.0.0.1:45678/invoke"));
        assert!(source.contains("\"one-time-token\""));
        assert!(source.contains("export const click = tool({"));
        assert!(source.contains("export const verify_state = tool({"));
        assert!(source.contains("export const set_config = tool({"));
        assert!(source.contains("\"pid\": tool.schema.number().int().describe(\"Process id\")"));
        assert!(source.contains("\"button\": tool.schema.string().optional()"));
        assert!(source.contains("\"exists\": tool.schema.literal(true)"));
        assert!(source.contains("\"value\": tool.schema.unknown().describe(\"New value for `key`. JSON type depends on the key.\").optional()"));
        assert!(!source.contains("parameters:"));
        assert!(!source.contains("JSON Schema enum 目前只支持 string 类型"));
        assert!(source.contains("if (!response.ok || result?.isError)"));
        assert!(source.contains("result?.isError"));
        assert!(source.contains("throw new Error"));
        assert!(source.contains("return JSON.stringify(result);"));

        let _ = std::fs::remove_dir_all(run_dir);
    }

    #[test]
    fn computer_control_tool_generation_aggregates_schema_errors_without_partial_write() {
        let run_dir = std::env::temp_dir().join(format!(
            "auracoder-opencode-tools-error-{}",
            Uuid::new_v4()
        ));
        let tools_dir = run_dir.join(".opencode").join("tools");
        std::fs::create_dir_all(&tools_dir).expect("test tools directory should be created");
        let tool_file = tools_dir.join("auracoder_computer_control.ts");
        std::fs::write(&tool_file, "sentinel").expect("sentinel should be written");

        let error = write_opencode_computer_control_tool(
            &run_dir,
            "http://127.0.0.1:45678/invoke",
            "one-time-token",
            &[
                json!({
                    "name": "bad_schema_one",
                    "description": "first bad schema",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "value": { "properties": {} }
                        }
                    }
                }),
                json!({
                    "name": "bad_schema_two",
                    "description": "second bad schema",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "value": { "type": "unsupported" }
                        }
                    }
                }),
            ],
        )
        .expect_err("multiple schema errors should be returned");
        let error_text = error.to_string();
        assert!(error_text.contains("bad_schema_one"));
        assert!(error_text.contains("bad_schema_two"));
        assert!(error_text.contains("缺少 type 且包含无法解释的关键字"));
        assert!(error_text.contains("不支持 type: unsupported"));
        assert_eq!(
            std::fs::read_to_string(&tool_file).expect("sentinel should remain"),
            "sentinel"
        );

        let _ = std::fs::remove_dir_all(run_dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires CUA v0.19.3 runtime"]
    fn cua_catalog_converts_all_opencode_tools() {
        let sdk = Arc::new(crate::computer_control_sdk::CuaDriverSdk::new());
        let manifest_key = concat!("CAR", "G", "O_MANIFEST_DIR");
        sdk.set_resource_dir(Some(
            PathBuf::from(std::env::var(manifest_key).expect("manifest path"))
                .join("resources"),
        ));
        let run_dir = std::env::temp_dir().join(format!(
            "auracoder-opencode-cua-catalog-{}",
            Uuid::new_v4()
        ));

        let conversion = (|| -> Result<()> {
            sdk.initialize().map_err(anyhow::Error::msg)?;
            let service = ComputerControlService::new(sdk.clone());
            let specs = service.sdk_tool_specs().map_err(anyhow::Error::msg)?;
            anyhow::ensure!(!specs.is_empty(), "CUA tool catalog is empty");
            anyhow::ensure!(
                specs.iter().any(|spec| spec["name"] == "set_config"),
                "CUA catalog does not contain set_config"
            );
            anyhow::ensure!(
                specs.iter().any(|spec| spec["name"] == "verify_state"),
                "CUA catalog does not contain verify_state"
            );

            write_opencode_computer_control_tool(
                &run_dir,
                "http://127.0.0.1:45678/invoke",
                "catalog-token",
                &specs,
            )?;
            let tool_file = run_dir
                .join(".opencode")
                .join("tools")
                .join("auracoder_computer_control.ts");
            let source = std::fs::read_to_string(&tool_file)?;
            let export_count = source
                .lines()
                .filter(|line| line.starts_with("export const "))
                .count();
            anyhow::ensure!(
                export_count == specs.len(),
                "generated export count {export_count} differs from CUA spec count {}",
                specs.len()
            );
            anyhow::ensure!(
                source.contains("export const set_config = tool({")
                    && source.contains("tool.schema.unknown()"),
                "set_config value did not use tool.schema.unknown()"
            );
            anyhow::ensure!(
                source.contains("export const verify_state = tool({")
                    && source.contains("tool.schema.literal(true)"),
                "verify_state boolean enum did not use tool.schema.literal(true)"
            );
            Ok(())
        })();

        let cleanup = std::fs::remove_dir_all(&run_dir);
        let shutdown = sdk.shutdown();
        if let Err(error) = conversion {
            panic!("CUA catalog conversion failed: {error:#}");
        }
        cleanup.expect("temporary CUA catalog directory should be removed");
        shutdown.expect("CUA SDK should shut down");
    }

    /*
    旧集成测试依赖 Engine 内部启动 OpenCode 服务，现由 LocalCliServiceLifecycle 负责，保留源码作为迁移留痕：
    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires CUA and OpenCode runtime"]
    async fn cua_catalog_loads_in_opencode_tool_ids() {
        let sdk = Arc::new(crate::computer_control_sdk::CuaDriverSdk::new());
        let manifest_key = concat!("CAR", "G", "O_MANIFEST_DIR");
        sdk.set_resource_dir(Some(
            PathBuf::from(std::env::var(manifest_key).expect("manifest path"))
                .join("resources"),
        ));
        let run_dir = std::env::temp_dir().join(format!(
            "auracoder-opencode-tool-ids-{}",
            Uuid::new_v4()
        ));
        let mut connection = None;

        let result = async {
            sdk.initialize().map_err(anyhow::Error::msg)?;
            let service = Arc::new(ComputerControlService::new(sdk.clone()));
            let specs = service.sdk_tool_specs().map_err(anyhow::Error::msg)?;
            anyhow::ensure!(!specs.is_empty(), "CUA tool catalog is empty");
            std::fs::create_dir_all(&run_dir)?;
            connection = Some(start_server(&run_dir.to_string_lossy(), None, None).await?);
            let active_server = connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("OpenCode connection was not created"))?;
            let response = reqwest::Client::new()
                .get(format!(
                    "{}/experimental/tool/ids",
                    active_server.base_url.trim_end_matches('/')
                ))
                .headers(auth_headers(&active_server.password))
                .send()
                .await?;
            let status = response.status();
            let body = response.text().await?;
            anyhow::ensure!(
                status.is_success(),
                "OpenCode /experimental/tool/ids returned HTTP {status}: {body}"
            );
            let payload: Value = serde_json::from_str(&body)
                .with_context(|| format!("OpenCode /experimental/tool/ids returned invalid JSON: {body}"))?;
            let ids = payload
                .as_array()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "OpenCode /experimental/tool/ids response must be an array: {body}"
                    )
                })?
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        anyhow::anyhow!(
                            "OpenCode /experimental/tool/ids response contains a non-string id: {body}"
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let expected = specs
                .iter()
                .map(|spec| {
                    let name = spec["name"].as_str().ok_or_else(|| {
                        anyhow::anyhow!("CUA SDK tool catalog contains a tool without name")
                    })?;
                    Ok(format!("auracoder_computer_control_{name}"))
                })
                .collect::<Result<Vec<_>>>()?;
            let missing = expected
                .iter()
                .filter(|tool_id| !ids.iter().any(|id| id == *tool_id))
                .cloned()
                .collect::<Vec<_>>();
            anyhow::ensure!(
                missing.is_empty(),
                "OpenCode /experimental/tool/ids missing CUA tools: {}",
                missing.join(", ")
            );
            let hit_count = expected
                .iter()
                .filter(|tool_id| ids.iter().any(|id| id == *tool_id))
                .count();
            anyhow::ensure!(
                hit_count == specs.len(),
                "OpenCode /experimental/tool/ids matched {hit_count} CUA tools, expected {}",
                specs.len()
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;

        if let Some(active_server) = connection.as_ref() {
            active_server.cancel_event_pump();
        }
        let shutdown = sdk.shutdown();
        let cleanup = std::fs::remove_dir_all(&run_dir);
        if let Err(error) = result {
            panic!("OpenCode tool id integration failed: {error:#}");
        }
        shutdown.expect("CUA SDK should shut down");
        cleanup.expect("temporary OpenCode cwd should be removed");
    }
    */
    #[test]
    fn opencode_schema_source_supports_zod_types_constraints_and_errors() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "enum": ["one", "two"],
                    "description": "Name",
                    "default": "one"
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3
                },
                "ratio": { "type": "number", "format": "double" },
                "enabled": { "type": "boolean" },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": 2
                },
                "child": {
                    "type": "object",
                    "properties": {
                        "code": {
                            "type": ["string", "null"],
                            "minLength": 2,
                            "maxLength": 4,
                            "pattern": "^[a-z]+$"
                        }
                    },
                    "required": ["code"],
                    "additionalProperties": false
                },
                "expect": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "element": {
                                "type": ["object", "null"],
                                "properties": {
                                    "exists": { "type": "boolean", "enum": [true] }
                                },
                                "required": ["exists"],
                                "additionalProperties": false
                            }
                        },
                        "required": ["element"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["name", "count", "ratio", "enabled", "tags", "child", "expect"],
            "additionalProperties": false
        });
        let source = opencode_tool_args_source(&schema).expect("schema should convert");
        assert!(source.contains("tool.schema.enum([\"one\", \"two\"]).describe(\"Name\").default(\"one\")"));
        assert!(source.contains("tool.schema.number().int().min(1).max(3)"));
        assert!(source.contains("tool.schema.number()"));
        assert!(source.contains("tool.schema.boolean()"));
        assert!(source.contains("tool.schema.array(tool.schema.string()).min(1).max(2)"));
        assert!(source.contains("tool.schema.string().min(2).max(4).regex(new RegExp(\"^[a-z]+$\")).nullable()"));
        assert!(source.contains("\"name\": tool.schema.enum"));
        assert!(source.contains("\"exists\": tool.schema.literal(true)"));

        let boolean_union = opencode_zod_schema_source(&json!({
            "type": "boolean",
            "enum": [true, false]
        }))
        .expect("boolean enum should convert");
        assert_eq!(
            boolean_union,
            "tool.schema.union([tool.schema.literal(true), tool.schema.literal(false)])"
        );

        let integer_union = opencode_zod_schema_source(&json!({
            "type": "integer",
            "enum": [1, 2]
        }))
        .expect("integer enum should convert");
        assert_eq!(
            integer_union,
            "tool.schema.union([tool.schema.literal(1), tool.schema.literal(2)])"
        );

        let number_literal = opencode_zod_schema_source(&json!({
            "type": "number",
            "enum": [1.5]
        }))
        .expect("number enum should convert");
        assert_eq!(number_literal, "tool.schema.literal(1.5)");

        let nullable_enum = opencode_zod_schema_source(&json!({
            "type": ["string", "null"],
            "enum": ["x", null]
        }))
        .expect("nullable enum should convert");
        assert_eq!(
            nullable_enum,
            "tool.schema.union([tool.schema.literal(\"x\"), tool.schema.literal(null)])"
        );
        assert!(!nullable_enum.contains(".nullable()"));

        let mismatched_enum = opencode_zod_schema_source(&json!({
            "type": "boolean",
            "enum": ["true"]
        }))
        .expect_err("mismatched enum value should fail");
        assert!(mismatched_enum.to_string().contains("enum"));

        let null_without_nullable = opencode_zod_schema_source(&json!({
            "type": "string",
            "enum": [null]
        }))
        .expect_err("null enum value without nullable should fail");
        assert!(null_without_nullable.to_string().contains("enum"));

        let array_enum = opencode_zod_schema_source(&json!({
            "type": "array",
            "enum": [[]]
        }))
        .expect_err("array enum value should fail");
        assert!(array_enum.to_string().contains("enum"));

        let object_enum = opencode_zod_schema_source(&json!({
            "type": "object",
            "enum": [{}]
        }))
        .expect_err("object enum value should fail");
        assert!(object_enum.to_string().contains("enum"));

        let optional_source = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": { "optional": { "type": "string" } },
            "required": []
        }))
        .expect("optional schema should convert");
        assert!(optional_source.contains("optional\": tool.schema.string().optional()"));

        let unsupported_type = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": { "value": { "type": "null" } },
            "required": ["value"]
        }))
        .expect_err("unsupported type should fail");
        assert!(unsupported_type.to_string().contains("type"));

        let additional_properties = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        }))
        .expect("additionalProperties=true should be accepted");
        assert_eq!(additional_properties, "{  }");

        let invalid_additional_properties = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": {},
            "additionalProperties": { "type": "string" }
        }))
        .expect_err("non-boolean additionalProperties should fail");
        assert!(invalid_additional_properties
            .to_string()
            .contains("additionalProperties"));

        let description_only = opencode_zod_schema_source(&json!({
            "description": "New value for `key`. JSON type depends on the key."
        }))
        .expect("description-only schema should convert to unknown");
        assert_eq!(
            description_only,
            "tool.schema.unknown().describe(\"New value for `key`. JSON type depends on the key.\")"
        );
        assert_eq!(
            opencode_zod_schema_source(&json!({})).expect("empty schema should convert to unknown"),
            "tool.schema.unknown()"
        );

        let missing_type_with_keywords = opencode_zod_schema_source(&json!({
            "properties": {},
            "description": "ambiguous"
        }))
        .expect_err("schema with keywords but no type should fail");
        assert!(missing_type_with_keywords
            .to_string()
            .contains("缺少 type 且包含无法解释的关键字"));

        let invalid_enum = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": { "value": { "type": "string", "enum": [] } }
        }))
        .expect_err("empty enum should fail");
        assert!(invalid_enum.to_string().contains("enum"));

        let mismatched_constraint = opencode_tool_args_source(&json!({
            "type": "object",
            "properties": { "value": { "type": "boolean", "minimum": 1 } }
        }))
        .expect_err("mismatched constraint should fail");
        assert!(mismatched_constraint.to_string().contains("minimum"));
    }

    #[test]
    fn generated_auracoder_thread_tools_use_independent_default_exports() {
        let run_dir = std::env::temp_dir().join(format!(
            "auracoder-opencode-thread-tools-test-{}",
            Uuid::new_v4()
        ));
        write_opencode_auracoder_thread_mcp_tool(
            &run_dir,
            "http://127.0.0.1:45679/invoke",
            "thread-token",
            &[
                json!({
                    "name": "get_auracoder_thread_message_count",
                    "description": "Get message count",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "thread_id": { "type": "string" }
                        },
                        "required": ["thread_id"],
                        "additionalProperties": false
                    }
                }),
                json!({
                    "name": "get_auracoder_thread_messages_page",
                    "description": "Get message page",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "thread_id": { "type": "string" },
                            "page": { "type": "integer", "minimum": 1 },
                            "page_size": { "type": "integer", "minimum": 1 }
                        },
                        "required": ["thread_id", "page", "page_size"],
                        "additionalProperties": false
                    }
                }),
            ],
        )
        .expect("AuraCoder thread tool sources should be generated");

        let tools_dir = run_dir.join(".opencode").join("tools");
        let count_source = std::fs::read_to_string(
            tools_dir.join("get_auracoder_thread_message_count.ts"),
        )
        .expect("message count tool source should exist");
        let page_source = std::fs::read_to_string(
            tools_dir.join("get_auracoder_thread_messages_page.ts"),
        )
        .expect("message page tool source should exist");
        for source in [&count_source, &page_source] {
            assert!(source.contains("import { tool } from \"@opencode-ai/plugin\""));
            assert!(source.contains("export default tool({"));
            assert!(!source.contains("parameters:"));
            assert!(source.contains("toolKind: \"auracoder_thread\""));
            assert!(source.contains("x-auracoder-computer-control-token"));
            assert!(source.contains("http://127.0.0.1:45679/invoke"));
            assert!(source.contains("\"thread-token\""));
            assert!(source.contains("return JSON.stringify(result);"));
            assert!(source.contains("if (!response.ok || result?.isError)"));
            assert!(source.contains("result?.isError"));
            assert!(source.contains("throw new Error"));
        }
        assert!(count_source.contains("\"thread_id\": tool.schema.string()"));
        assert!(count_source.contains(
            "return invoke(\"get_auracoder_thread_message_count\", args, context);"
        ));
        assert!(page_source.contains("\"thread_id\": tool.schema.string()"));
        assert!(page_source.contains("\"page\": tool.schema.number().int().min(1)"));
        assert!(page_source.contains("\"page_size\": tool.schema.number().int().min(1)"));
        assert!(page_source.contains(
            "return invoke(\"get_auracoder_thread_messages_page\", args, context);"
        ));
        assert!(!tools_dir.join("auracoder_thread_mcp.ts").exists());

        let _ = std::fs::remove_dir_all(run_dir);
    }

    #[test]
    fn opencode_tool_names_are_normalized_before_authorization() {
        assert_eq!(
            normalize_opencode_tool_name("auracoder_computer_control_click"),
            "click"
        );
        assert_eq!(normalize_opencode_tool_name("click"), "click");
    }

    #[test]
    fn is_user_message_only_matches_known_user_roles() {
        let mut roles = HashMap::new();
        roles.insert("user-message".to_string(), "user".to_string());
        roles.insert("assistant-message".to_string(), "assistant".to_string());

        assert!(is_user_message(&roles, "user-message"));
        assert!(!is_user_message(&roles, "assistant-message"));
        assert!(!is_user_message(&roles, "unknown"));
    }

    #[tokio::test]
    async fn mapper_ignores_prompt_user_text_parts() {
        let engine = OpenCodeEngine::default();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
        mapper.record_message("msg_user", "user", None);
        let part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_user",
            "messageID": "msg_user",
            "type": "text",
            "text": "hello"
        }))
        .unwrap();

        engine
            .handle_part_updated(&part, &mut mapper, &event_tx)
            .await;

        assert!(!mapper.content_seen);
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mapper_flushes_pending_reasoning_after_part_type_is_known() {
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
        mapper.store_pending_text("prt_reasoning", "msg_assistant", "thinking");

        mapper
            .part_type_by_id
            .insert("prt_reasoning".to_string(), "reasoning".to_string());
        flush_pending_opencode_text_for_part(&mut mapper, &event_tx, "prt_reasoning").await;

        match event_rx
            .try_recv()
            .expect("expected pending reasoning to flush")
        {
            EngineEvent::ThinkingDelta { content } => assert_eq!(content, "thinking"),
            other => panic!("expected thinking delta, got {other:?}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mapper_accepts_non_prompt_text_without_message_role() {
        let engine = OpenCodeEngine::default();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
        let part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_text",
            "messageID": "msg_assistant",
            "type": "text",
            "text": "response"
        }))
        .unwrap();

        engine
            .handle_part_updated(&part, &mut mapper, &event_tx)
            .await;

        match event_rx.try_recv().expect("expected text delta") {
            EngineEvent::TextDelta { content } => assert_eq!(content, "response"),
            other => panic!("expected text delta, got {other:?}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mapper_ignores_assistant_parts_for_previous_prompt() {
        let engine = OpenCodeEngine::default();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_current_user".to_string());
        mapper.record_message("msg_old_user", "user", None);
        mapper.record_message("msg_old_assistant", "assistant", Some("msg_old_user"));
        let part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_old_text",
            "messageID": "msg_old_assistant",
            "type": "text",
            "text": "stale response"
        }))
        .unwrap();

        engine
            .handle_part_updated(&part, &mut mapper, &event_tx)
            .await;

        assert!(!mapper.content_seen);
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn idle_without_current_prompt_response_fails_turn() {
        let engine = OpenCodeEngine::default();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_current_user".to_string());
        mapper.busy_seen = true;

        engine.complete_after_idle(&mut mapper, &event_tx).await;

        match event_rx.try_recv().expect("expected error event") {
            EngineEvent::Error {
                message,
                recoverable,
            } => {
                assert!(!recoverable);
                assert!(message.contains("without producing a response"));
            }
            other => panic!("expected error event, got {other:?}"),
        }
        match event_rx.try_recv().expect("expected failed completion") {
            EngineEvent::TurnCompleted { status, .. } => {
                assert_eq!(status, TurnCompletionStatus::Failed);
            }
            other => panic!("expected failed completion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mapper_ignores_non_append_text_snapshots() {
        let engine = OpenCodeEngine::default();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
        let first_part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_reasoning",
            "messageID": "msg_assistant",
            "type": "reasoning",
            "text": "first"
        }))
        .unwrap();
        let revised_part: OpenCodePart = serde_json::from_value(json!({
            "id": "prt_reasoning",
            "messageID": "msg_assistant",
            "type": "reasoning",
            "text": "second"
        }))
        .unwrap();

        engine
            .handle_part_updated(&first_part, &mut mapper, &event_tx)
            .await;
        engine
            .handle_part_updated(&revised_part, &mut mapper, &event_tx)
            .await;

        match event_rx.try_recv().expect("expected initial reasoning") {
            EngineEvent::ThinkingDelta { content } => assert_eq!(content, "first"),
            other => panic!("expected thinking delta, got {other:?}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn new_message_id_uses_opencode_ascending_shape() {
        let id = new_message_id();

        assert_eq!(id.len(), "msg_".len() + 26);
        assert!(id.starts_with("msg_"));

        let sortable = &id[4..16];
        let suffix = &id[16..];

        assert_eq!(sortable.len(), 12);
        assert!(sortable
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)));
        assert_eq!(suffix.len(), 14);
        assert!(suffix.chars().all(|ch| ch.is_ascii_alphanumeric()));
    }

    #[test]
    fn opencode_prompt_message_path_uses_synchronous_message_endpoint() {
        assert_eq!(
            opencode_prompt_message_path("ses_123"),
            "/session/ses_123/message"
        );
    }

    #[test]
    fn new_message_id_is_lexicographically_monotonic() {
        let mut previous = new_message_id();

        for _ in 0..1000 {
            let current = new_message_id();
            assert!(previous < current, "expected {previous} < {current}");
            previous = current;
        }
    }

    #[test]
    fn opencode_sort_prefix_matches_observed_timestamp_formula() {
        assert_eq!(
            opencode_sort_prefix_for_millis(1_777_173_925_808, 1),
            "dc7d20fb0001"
        );
        assert_eq!(
            opencode_sort_prefix_for_millis(1_777_173_926_670, 1),
            "dc7d2130e001"
        );
    }

    #[test]
    fn generated_style_user_id_sorts_between_observed_turn_messages() {
        let prior_assistant = "msg_dc7d20fb0001rH5hSHrepNXLgJ";
        let user_prefix = opencode_sort_prefix_for_millis(1_777_173_926_670, 1);
        let user_id = format!("msg_{user_prefix}00000000000000");
        let next_assistant = "msg_dc7d2132b001iU68RZlw7CFMwn";

        assert!(prior_assistant < user_id.as_str());
        assert!(user_id.as_str() < next_assistant);
    }

    #[test]
    fn event_matching_reads_nested_part_session_id() {
        let event = OpenCodeBusEvent {
            event_type: "message.part.updated".to_string(),
            properties: json!({
                "part": {
                    "sessionID": "ses_1",
                    "id": "part_1"
                }
            }),
        };

        assert!(event_matches_session(&event, "ses_1"));
        assert!(!event_matches_session(&event, "ses_2"));
    }

    #[test]
    fn extracts_persisted_opencode_approval_routes() {
        let route = extract_persisted_approval_route(&json!({
            "_opencodeRequestKind": "question",
            "_opencodeRequestID": "req_1",
            "_opencodeSessionID": "ses_1",
            "_opencodeCwd": "/tmp/project",
            "questions": [{ "id": "question-0", "question": "Run tests?" }]
        }))
        .unwrap();

        assert_eq!(route.server_method, "opencode/question");
        assert_eq!(route.raw_request_id["requestID"], "req_1");
        assert_eq!(route.raw_request_id["questions"][0]["id"], "question-0");
    }
}
