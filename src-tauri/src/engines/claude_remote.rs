use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    claude_sidecar::{map_claude_models, with_legacy_claude_models, SidecarModelInfo},
    normalize_approval_response_for_engine, trim_action_output_delta_content, ActionResult,
    ActionType, ApprovalRequestRoute, Engine, EngineEvent, EngineSteerReceipt, EngineThread,
    ModelInfo, OutputStream, SandboxPolicy, ThreadScope, TurnCompletionStatus, TurnInput,
};

const EVENT_BUFFER_CAPACITY: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RemoteClaudeEvent {
    Ready,
    TransportClosed,
    SessionInit {
        id: Option<String>,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    TurnStarted {
        id: Option<String>,
    },
    TextDelta {
        id: Option<String>,
        content: String,
    },
    ThinkingDelta {
        id: Option<String>,
        content: String,
    },
    ActionStarted {
        id: Option<String>,
        #[serde(rename = "actionId")]
        action_id: String,
        #[serde(rename = "actionType")]
        action_type: String,
        summary: String,
        details: Option<serde_json::Value>,
    },
    ActionOutputDelta {
        id: Option<String>,
        #[serde(rename = "actionId")]
        action_id: String,
        stream: String,
        content: String,
    },
    ActionProgressUpdated {
        id: Option<String>,
        #[serde(rename = "actionId")]
        action_id: String,
        message: String,
    },
    ActionCompleted {
        id: Option<String>,
        #[serde(rename = "actionId")]
        action_id: String,
        success: bool,
        output: Option<String>,
        error: Option<String>,
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
    },
    ApprovalRequested {
        id: Option<String>,
        #[serde(rename = "approvalId")]
        approval_id: String,
        #[serde(rename = "actionType")]
        action_type: String,
        summary: String,
        details: Option<serde_json::Value>,
    },
    TurnCompleted {
        id: Option<String>,
        status: String,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
        #[serde(rename = "tokenUsage")]
        token_usage: Option<RemoteClaudeTokenUsage>,
        #[serde(rename = "stopReason")]
        stop_reason: Option<String>,
    },
    Notice {
        id: Option<String>,
        kind: String,
        level: String,
        title: String,
        message: String,
    },
    UsageLimitsUpdated {
        id: Option<String>,
        usage: RemoteClaudeUsageLimits,
    },
    Models {
        id: Option<String>,
        models: Vec<SidecarModelInfo>,
    },
    Error {
        id: Option<String>,
        message: String,
        recoverable: Option<bool>,
    },
    Version {
        id: Option<String>,
        #[serde(rename = "version")]
        _version: String,
    },
    SessionHandleCreated {
        id: Option<String>,
    },
    SessionMessageAccepted {
        id: Option<String>,
    },
    SessionHandleInterrupted {
        id: Option<String>,
    },
    SessionHandleDestroyed {
        id: Option<String>,
    },
}

impl RemoteClaudeEvent {
    fn request_id(&self) -> Option<&str> {
        match self {
            Self::Ready | Self::TransportClosed => None,
            Self::SessionInit { id, .. }
            | Self::TurnStarted { id }
            | Self::TextDelta { id, .. }
            | Self::ThinkingDelta { id, .. }
            | Self::ActionStarted { id, .. }
            | Self::ActionOutputDelta { id, .. }
            | Self::ActionProgressUpdated { id, .. }
            | Self::ActionCompleted { id, .. }
            | Self::ApprovalRequested { id, .. }
            | Self::TurnCompleted { id, .. }
            | Self::Notice { id, .. }
            | Self::UsageLimitsUpdated { id, .. }
            | Self::Models { id, .. }
            | Self::Error { id, .. }
            | Self::Version { id, .. }
            | Self::SessionHandleCreated { id }
            | Self::SessionMessageAccepted { id }
            | Self::SessionHandleInterrupted { id }
            | Self::SessionHandleDestroyed { id } => id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteClaudeTokenUsage {
    input: u64,
    output: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteClaudeUsageLimits {
    current_tokens: Option<u64>,
    max_context_tokens: Option<u64>,
    context_window_percent: Option<u8>,
    five_hour_percent: Option<u8>,
    weekly_percent: Option<u8>,
    fable_weekly_percent: Option<u8>,
    opus_weekly_percent: Option<u8>,
    sonnet_weekly_percent: Option<u8>,
    five_hour_resets_at: Option<i64>,
    weekly_resets_at: Option<i64>,
    fable_weekly_resets_at: Option<i64>,
    opus_weekly_resets_at: Option<i64>,
    sonnet_weekly_resets_at: Option<i64>,
}

struct RemoteClaudeTransport {
    base_url: String,
    http: reqwest::Client,
    event_tx: broadcast::Sender<RemoteClaudeEvent>,
    alive: Arc<AtomicBool>,
}

impl RemoteClaudeTransport {
    async fn connect(base_url: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::new();
        let readiness_deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            match http
                .get(format!("{base_url}/health"))
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
            {
                Ok(health) if health.status().is_success() => break,
                Ok(health)
                    if health.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
                        && Instant::now() < readiness_deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(health) => {
                    let body = health.text().await.unwrap_or_default();
                    anyhow::bail!("SSH 远端 Claude 运行时不可用: {body}");
                }
                Err(_) if Instant::now() < readiness_deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => {
                    return Err(error).context("连接 SSH 远端 Claude 健康端点失败");
                }
            }
        }

        let response = http
            .get(format!("{base_url}/events"))
            .send()
            .await
            .context("订阅 SSH 远端 Claude 事件失败")?
            .error_for_status()
            .context("SSH 远端 Claude 事件端点不可用")?;
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        let pump_alive = alive.clone();
        let pump_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffered = Vec::<u8>::new();
            'event_stream: while let Some(chunk) = stream.next().await {
                let Ok(chunk) = chunk else {
                    break;
                };
                buffered.extend_from_slice(&chunk);
                while let Some(index) = buffered.iter().position(|byte| *byte == b'\n') {
                    let line = buffered.drain(..=index).collect::<Vec<_>>();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<RemoteClaudeEvent>(line) {
                        Ok(event) => {
                            let _ = pump_tx.send(event);
                        }
                        Err(error) => {
                            log::warn!("解析 SSH 远端 Claude 事件失败: {error}; line={line}");
                            break 'event_stream;
                        }
                    }
                }
            }
            pump_alive.store(false, Ordering::SeqCst);
            let _ = pump_tx.send(RemoteClaudeEvent::TransportClosed);
        });

        Ok(Self {
            base_url,
            http,
            event_tx,
            alive,
        })
    }

    async fn send_command(&self, command: &serde_json::Value) -> anyhow::Result<()> {
        anyhow::ensure!(self.is_alive(), "SSH 远端 Claude 事件连接已断开");
        let response = self
            .http
            .post(format!("{}/command", self.base_url))
            .timeout(REQUEST_TIMEOUT)
            .json(command)
            .send()
            .await
            .context("发送 SSH 远端 Claude 命令失败")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("SSH 远端 Claude 命令被拒绝: status={status} body={body}");
        }
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<RemoteClaudeEvent> {
        self.event_tx.subscribe()
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct RemoteClaudeThreadConfig {
    scope: ThreadScope,
    model_id: String,
    sandbox: SandboxPolicy,
    agent_session_id: Option<String>,
    active_request_id: Option<String>,
}

#[derive(Default)]
struct RemoteClaudeState {
    transport: Option<Arc<RemoteClaudeTransport>>,
    threads: HashMap<String, RemoteClaudeThreadConfig>,
}

pub struct ClaudeRemoteEngine {
    base_url: String,
    state: Arc<Mutex<RemoteClaudeState>>,
}

pub(crate) struct ClaudePersistentTurn {
    pub params: serde_json::Value,
    events: broadcast::Receiver<RemoteClaudeEvent>,
}

impl ClaudeRemoteEngine {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            state: Arc::new(Mutex::new(RemoteClaudeState::default())),
        }
    }

    async fn ensure_transport(&self) -> anyhow::Result<Arc<RemoteClaudeTransport>> {
        if let Some(transport) = self.state.lock().await.transport.clone() {
            if transport.is_alive() {
                return Ok(transport);
            }
        }
        let transport = Arc::new(RemoteClaudeTransport::connect(self.base_url.clone()).await?);
        let mut state = self.state.lock().await;
        if let Some(existing) = state.transport.as_ref() {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
        }
        state.transport = Some(transport.clone());
        Ok(transport)
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) async fn prepare_persistent_turn(
        &self,
        engine_thread_id: &str,
        input: TurnInput,
    ) -> anyhow::Result<ClaudePersistentTurn> {
        let transport = self.ensure_transport().await?;
        let events = transport.subscribe();
        let thread_config = self
            .state
            .lock()
            .await
            .threads
            .get(engine_thread_id)
            .cloned()
            .context("SSH 远端 Claude 会话配置不存在；必须先恢复或创建会话")?;
        let cwd = match &thread_config.scope {
            ThreadScope::Repo { repo_path } => repo_path.clone(),
            ThreadScope::Workspace { root_path, .. } => root_path.clone(),
        };
        let TurnInput {
            message,
            attachments,
            plan_mode,
            input_items: _,
        } = input;
        anyhow::ensure!(
            attachments.iter().all(|attachment| attachment.is_remote),
            "SSH 远端 Claude 只能接收已上传的远端附件路径"
        );
        let attachments = attachments
            .into_iter()
            .map(|attachment| {
                serde_json::json!({
                    "fileName": attachment.file_name,
                    "filePath": attachment.file_path,
                    "sizeBytes": attachment.size_bytes,
                    "mimeType": attachment.mime_type,
                })
            })
            .collect::<Vec<_>>();
        let mut params = serde_json::json!({
            "prompt": message,
            "attachments": attachments,
            "cwd": cwd,
            "model": thread_config.model_id,
            "approvalPolicy": thread_config.sandbox.approval_policy.as_ref().and_then(serde_json::Value::as_str),
            "allowNetwork": thread_config.sandbox.allow_network,
            "writableRoots": thread_config.sandbox.writable_roots,
            "sandboxMode": thread_config.sandbox.sandbox_mode,
            "reasoningEffort": thread_config.sandbox.reasoning_effort,
            "planMode": plan_mode,
            "settingSources": ["user", "project"],
            "strictMcpConfig": true,
            "enforceApprovalRouting": true,
        });
        if let Some(session_id) = thread_config.agent_session_id.as_ref() {
            params["resume"] = serde_json::Value::String(session_id.clone());
        } else {
            params["sessionId"] = serde_json::Value::String(engine_thread_id.to_string());
        }
        Ok(ClaudePersistentTurn { params, events })
    }

    pub(crate) async fn relay_persistent_turn(
        &self,
        engine_thread_id: &str,
        request_id: &str,
        mut turn: ClaudePersistentTurn,
        event_tx: mpsc::Sender<EngineEvent>,
    ) -> anyhow::Result<()> {
        if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
            config.active_request_id = Some(request_id.to_string());
        }
        loop {
            let event = match turn.events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    anyhow::bail!("SSH 远端 Claude 事件流丢失 {skipped} 条事件");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                }
            };
            if event.request_id().is_some_and(|id| id != request_id) {
                continue;
            }
            match event {
                RemoteClaudeEvent::TurnStarted { .. } => {
                    let _ = event_tx
                        .send(EngineEvent::TurnStarted {
                            client_turn_id: None,
                            remote_turn_id: Some(request_id.to_string()),
                        })
                        .await;
                }
                RemoteClaudeEvent::SessionInit { session_id, .. } => {
                    if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id)
                    {
                        config.agent_session_id = Some(session_id);
                    }
                }
                RemoteClaudeEvent::TextDelta { content, .. } => {
                    let _ = event_tx.send(EngineEvent::TextDelta { content }).await;
                }
                RemoteClaudeEvent::ThinkingDelta { content, .. } => {
                    let _ = event_tx.send(EngineEvent::ThinkingDelta { content }).await;
                }
                RemoteClaudeEvent::ActionStarted {
                    action_id,
                    action_type,
                    summary,
                    details,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::ActionStarted {
                            action_id,
                            engine_action_id: None,
                            action_type: Self::parse_action_type(&action_type),
                            summary,
                            details: details.unwrap_or_else(|| serde_json::json!({})),
                        })
                        .await;
                }
                RemoteClaudeEvent::ActionOutputDelta {
                    action_id,
                    stream,
                    content,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::ActionOutputDelta {
                            action_id,
                            stream: Self::parse_output_stream(&stream),
                            content: trim_action_output_delta_content(&content),
                        })
                        .await;
                }
                RemoteClaudeEvent::ActionProgressUpdated {
                    action_id, message, ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::ActionProgressUpdated { action_id, message })
                        .await;
                }
                RemoteClaudeEvent::ActionCompleted {
                    action_id,
                    success,
                    output,
                    error,
                    duration_ms,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::ActionCompleted {
                            action_id,
                            result: ActionResult {
                                success,
                                output,
                                error,
                                diff: None,
                                duration_ms: duration_ms.unwrap_or_default(),
                            },
                        })
                        .await;
                }
                RemoteClaudeEvent::ApprovalRequested {
                    approval_id,
                    action_type,
                    summary,
                    details,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::ApprovalRequested {
                            approval_id,
                            action_type: Self::parse_action_type(&action_type),
                            summary,
                            details: details.unwrap_or_else(|| serde_json::json!({})),
                        })
                        .await;
                }
                RemoteClaudeEvent::Notice {
                    kind,
                    level,
                    title,
                    message,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::Notice {
                            kind,
                            level,
                            title,
                            message,
                        })
                        .await;
                }
                RemoteClaudeEvent::UsageLimitsUpdated { usage, .. } => {
                    let _ = event_tx
                        .send(EngineEvent::UsageLimitsUpdated {
                            usage: super::UsageLimitsSnapshot {
                                current_tokens: usage.current_tokens,
                                max_context_tokens: usage.max_context_tokens,
                                context_window_percent: usage.context_window_percent,
                                five_hour_percent: usage.five_hour_percent,
                                weekly_percent: usage.weekly_percent,
                                fable_weekly_percent: usage.fable_weekly_percent,
                                opus_weekly_percent: usage.opus_weekly_percent,
                                sonnet_weekly_percent: usage.sonnet_weekly_percent,
                                five_hour_resets_at: usage.five_hour_resets_at,
                                weekly_resets_at: usage.weekly_resets_at,
                                fable_weekly_resets_at: usage.fable_weekly_resets_at,
                                opus_weekly_resets_at: usage.opus_weekly_resets_at,
                                sonnet_weekly_resets_at: usage.sonnet_weekly_resets_at,
                            },
                        })
                        .await;
                }
                RemoteClaudeEvent::Error {
                    message,
                    recoverable,
                    ..
                } => {
                    let _ = event_tx
                        .send(EngineEvent::Error {
                            message,
                            recoverable: recoverable.unwrap_or(false),
                        })
                        .await;
                }
                RemoteClaudeEvent::TurnCompleted {
                    status,
                    session_id,
                    token_usage,
                    stop_reason,
                    ..
                } => {
                    if let Some(session_id) = session_id {
                        if let Some(config) =
                            self.state.lock().await.threads.get_mut(engine_thread_id)
                        {
                            config.agent_session_id = Some(session_id);
                        }
                    }
                    if let Some(stop_reason) = stop_reason.filter(|reason| reason != "end_turn") {
                        let _ = event_tx
                            .send(EngineEvent::Notice {
                                kind: "claude_stop_reason".to_string(),
                                level: "info".to_string(),
                                title: "Claude stop reason".to_string(),
                                message: stop_reason,
                            })
                            .await;
                    }
                    let status = match status.as_str() {
                        "completed" => TurnCompletionStatus::Completed,
                        "interrupted" => TurnCompletionStatus::Interrupted,
                        _ => TurnCompletionStatus::Failed,
                    };
                    let _ = event_tx
                        .send(EngineEvent::TurnCompleted {
                            token_usage: token_usage.map(|usage| super::TokenUsage {
                                input: usage.input,
                                output: usage.output,
                                reasoning: None,
                                cache_read: None,
                                cache_write: None,
                                cost_usd: None,
                            }),
                            status,
                        })
                        .await;
                    break;
                }
                RemoteClaudeEvent::TransportClosed => {
                    anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                }
                RemoteClaudeEvent::Ready
                | RemoteClaudeEvent::Models { .. }
                | RemoteClaudeEvent::Version { .. }
                | RemoteClaudeEvent::SessionHandleCreated { .. }
                | RemoteClaudeEvent::SessionMessageAccepted { .. }
                | RemoteClaudeEvent::SessionHandleInterrupted { .. }
                | RemoteClaudeEvent::SessionHandleDestroyed { .. } => {}
            }
        }
        if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
            config.active_request_id = None;
        }
        Ok(())
    }

    pub async fn list_models_runtime(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let transport = self.ensure_transport().await?;
        let request_id = Uuid::new_v4().to_string();
        let mut events = transport.subscribe();
        transport
            .send_command(&serde_json::json!({
                "id": request_id,
                "method": "list_models",
                "params": {},
            }))
            .await?;
        let models = tokio::time::timeout(REQUEST_TIMEOUT, async {
            loop {
                match events.recv().await {
                    Ok(RemoteClaudeEvent::Models { id, models })
                        if id.as_deref() == Some(request_id.as_str()) =>
                    {
                        return Ok(map_claude_models(models));
                    }
                    Ok(RemoteClaudeEvent::Error { id, message, .. })
                        if id.as_deref() == Some(request_id.as_str()) =>
                    {
                        anyhow::bail!(message);
                    }
                    Ok(RemoteClaudeEvent::TransportClosed) => {
                        anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                    }
                }
            }
        })
        .await
        .context("读取 SSH 远端 Claude 模型超时")??;
        Ok(with_legacy_claude_models(models))
    }

    pub async fn prewarm(&self) -> anyhow::Result<()> {
        self.ensure_transport().await.map(|_| ())
    }

    /// 通过远端 Claude 按会话 ID读取单个会话摘要。
    ///
    /// 该方法只负责 Claude 协议请求；远端服务入口由上层
    /// `remote_project_claude_runtime_service` 通过 CLI Service Lifecycle 提供。
    pub async fn read_remote_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<RemoteClaudeSession> {
        let mut url = reqwest::Url::parse(&format!("{}/", self.base_url))
            .context("构造 SSH 远端 Claude 会话地址失败")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("SSH 远端 Claude 会话地址不支持路径片段"))?
            .push("sessions")
            .push(session_id);

        let response = reqwest::Client::new()
            .get(url)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("读取 SSH 远端 Claude 会话失败")?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow::Error::new(RemoteClaudeSessionNotFoundError {
                session_id: session_id.to_string(),
            }));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "SSH 远端 Claude 按 ID 读取失败: HTTP {status} session_id={session_id} detail={body}"
            );
        }

        response
            .json::<RemoteClaudeSession>()
            .await
            .context("解析 SSH 远端 Claude 会话失败")
    }

    async fn validate_remote_session(&self, cwd: &str, session_id: &str) -> anyhow::Result<()> {
        // 旧实现：请求 `/sessions?cwd=...` 列表后匹配 ID 和 cwd。
        // 该逻辑保留为注释，按 ID 校验必须统一复用 read_remote_session：
        // let sessions = reqwest::Client::new()
        //     .get(format!("{}/sessions", self.base_url))
        //     .query(&[("cwd", cwd)])
        //     .timeout(REQUEST_TIMEOUT)
        //     .send()
        //     .await
        //     .context("读取 SSH 远端 Claude 会话失败")?
        //     .error_for_status()
        //     .context("SSH 远端 Claude 会话读取被拒绝")?
        //     .json::<Vec<RemoteClaudeSession>>()
        //     .await
        //     .context("解析 SSH 远端 Claude 会话失败")?;
        // anyhow::ensure!(
        //     sessions
        //         .iter()
        //         .any(|session| session.id == session_id && session.cwd == cwd),
        //     "SSH 远端 Claude 会话不存在或目录不匹配；不会在远端或本机创建替代会话: session_id={session_id} cwd={cwd}"
        // );

        let session = self.read_remote_session(session_id).await?;
        anyhow::ensure!(
            session.id == session_id
                && session.session_id == session_id
                && crate::path_utils::paths_equal(&session.cwd, cwd),
            "SSH 远端 Claude 会话不存在或目录不匹配；不会在远端或本机创建替代会话: session_id={session_id} cwd={cwd}"
        );
        Ok(())
    }

    fn parse_action_type(value: &str) -> ActionType {
        match value {
            "file_read" => ActionType::FileRead,
            "file_write" => ActionType::FileWrite,
            "file_edit" => ActionType::FileEdit,
            "file_delete" => ActionType::FileDelete,
            "command" => ActionType::Command,
            "git" => ActionType::Git,
            "search" => ActionType::Search,
            _ => ActionType::Other,
        }
    }

    fn parse_output_stream(value: &str) -> OutputStream {
        match value {
            "stderr" => OutputStream::Stderr,
            _ => OutputStream::Stdout,
        }
    }
}

/// Claude 远端按 ID 接口明确返回 HTTP 404 时使用的底层错误。
///
/// 调用方可以安全地将其转换为
/// 公共 `CliSessionNotFoundError`；其他 HTTP 状态必须保留原始服务错误。
#[derive(Debug, Clone)]
pub struct RemoteClaudeSessionNotFoundError {
    /// 远端报告不存在的 Claude 会话标识。
    pub session_id: String,
}

impl std::fmt::Display for RemoteClaudeSessionNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "SSH 远端 Claude 会话不存在: session_id={}",
            self.session_id
        )
    }
}

impl std::error::Error for RemoteClaudeSessionNotFoundError {}

/// Claude 远端按 ID 接口返回的单个会话摘要。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteClaudeSession {
    /// 远端会话主标识。
    pub id: String,
    /// 远端接口显式返回的会话标识。
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// 会话所属的远端工作目录。
    pub cwd: String,
    /// 会话标题。
    pub title: String,
    /// 会话最后更新时间。
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[async_trait]
impl Engine for ClaudeRemoteEngine {
    fn id(&self) -> &str {
        "claude"
    }

    fn name(&self) -> &str {
        "Claude"
    }

    fn models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }

    async fn is_available(&self) -> bool {
        self.prewarm().await.is_ok()
    }

    async fn start_thread(
        &self,
        scope: ThreadScope,
        resume_engine_thread_id: Option<&str>,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<EngineThread, anyhow::Error> {
        let existing_session = if let Some(resume_id) = resume_engine_thread_id {
            let cached = self
                .state
                .lock()
                .await
                .threads
                .get(resume_id)
                .and_then(|config| config.agent_session_id.clone());
            if cached.is_none() {
                let cwd = match &scope {
                    ThreadScope::Repo { repo_path } => repo_path,
                    ThreadScope::Workspace { root_path, .. } => root_path,
                };
                self.validate_remote_session(cwd, resume_id).await?;
            }
            cached.or_else(|| Some(resume_id.to_string()))
        } else {
            None
        };
        let engine_thread_id = existing_session
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.state.lock().await.threads.insert(
            engine_thread_id.clone(),
            RemoteClaudeThreadConfig {
                scope,
                model_id: model.to_string(),
                sandbox,
                agent_session_id: existing_session,
                active_request_id: None,
            },
        );
        Ok(EngineThread { engine_thread_id })
    }

    async fn send_message(
        &self,
        engine_thread_id: &str,
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<(), anyhow::Error> {
        let transport = self.ensure_transport().await?;
        let thread_config = self
            .state
            .lock()
            .await
            .threads
            .get(engine_thread_id)
            .cloned()
            .context("SSH 远端 Claude 会话配置不存在；必须先恢复或创建会话")?;
        let request_id = Uuid::new_v4().to_string();
        if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
            config.active_request_id = Some(request_id.clone());
        }
        let cwd = match &thread_config.scope {
            ThreadScope::Repo { repo_path } => repo_path.clone(),
            ThreadScope::Workspace { root_path, .. } => root_path.clone(),
        };
        let TurnInput {
            message,
            attachments,
            plan_mode,
            input_items: _,
        } = input;
        // 阶段计划 3 明确拒绝远端附件；阶段计划 4 已在发送前把本机文件上传并
        // 转换为远端绝对路径，因此这里只接受标记为远端缓存的附件。
        // anyhow::ensure!(
        //     attachments.is_empty(),
        //     "SSH 远端 Claude 附件将在第4阶段剩余工作阶段计划4中接入"
        // );
        anyhow::ensure!(
            attachments.iter().all(|attachment| attachment.is_remote),
            "SSH 远端 Claude 只能接收已上传的远端附件路径"
        );
        let attachments = attachments
            .into_iter()
            .map(|attachment| {
                serde_json::json!({
                    "fileName": attachment.file_name,
                    "filePath": attachment.file_path,
                    "sizeBytes": attachment.size_bytes,
                    "mimeType": attachment.mime_type,
                })
            })
            .collect::<Vec<_>>();
        let mut params = serde_json::json!({
            "prompt": message,
            "attachments": attachments,
            "cwd": cwd,
            "model": thread_config.model_id,
            "approvalPolicy": thread_config.sandbox.approval_policy.as_ref().and_then(serde_json::Value::as_str),
            "allowNetwork": thread_config.sandbox.allow_network,
            "writableRoots": thread_config.sandbox.writable_roots,
            "sandboxMode": thread_config.sandbox.sandbox_mode,
            "reasoningEffort": thread_config.sandbox.reasoning_effort,
            "planMode": plan_mode,
            "settingSources": ["user", "project"],
            "strictMcpConfig": true,
            "enforceApprovalRouting": true,
        });
        if let Some(session_id) = thread_config.agent_session_id.as_ref() {
            params["resume"] = serde_json::Value::String(session_id.clone());
        } else {
            params["sessionId"] = serde_json::Value::String(engine_thread_id.to_string());
        }
        let mut events = transport.subscribe();
        transport
            .send_command(&serde_json::json!({
                "id": request_id,
                "method": "query",
                "params": params,
            }))
            .await?;

        loop {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    transport.send_command(&serde_json::json!({
                        "method": "cancel",
                        "params": { "requestId": request_id },
                    })).await?;
                    if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
                        config.active_request_id = None;
                    }
                    return Ok(());
                }
                incoming = events.recv() => {
                    let event = match incoming {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            anyhow::bail!("SSH 远端 Claude 事件流丢失 {skipped} 条事件");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                        }
                    };
                    if event.request_id().is_some_and(|id| id != request_id) {
                        continue;
                    }
                    match event {
                        RemoteClaudeEvent::TurnStarted { .. } => {
                            let _ = event_tx.send(EngineEvent::TurnStarted {
                                client_turn_id: None,
                                remote_turn_id: Some(request_id.clone()),
                            }).await;
                        }
                        RemoteClaudeEvent::SessionInit { session_id, .. } => {
                            if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
                                config.agent_session_id = Some(session_id);
                            }
                        }
                        RemoteClaudeEvent::TextDelta { content, .. } => {
                            let _ = event_tx.send(EngineEvent::TextDelta { content }).await;
                        }
                        RemoteClaudeEvent::ThinkingDelta { content, .. } => {
                            let _ = event_tx.send(EngineEvent::ThinkingDelta { content }).await;
                        }
                        RemoteClaudeEvent::ActionStarted { action_id, action_type, summary, details, .. } => {
                            let _ = event_tx.send(EngineEvent::ActionStarted {
                                action_id,
                                engine_action_id: None,
                                action_type: Self::parse_action_type(&action_type),
                                summary,
                                details: details.unwrap_or_else(|| serde_json::json!({})),
                            }).await;
                        }
                        RemoteClaudeEvent::ActionOutputDelta { action_id, stream, content, .. } => {
                            let _ = event_tx.send(EngineEvent::ActionOutputDelta {
                                action_id,
                                stream: Self::parse_output_stream(&stream),
                                content: trim_action_output_delta_content(&content),
                            }).await;
                        }
                        RemoteClaudeEvent::ActionProgressUpdated { action_id, message, .. } => {
                            let _ = event_tx.send(EngineEvent::ActionProgressUpdated { action_id, message }).await;
                        }
                        RemoteClaudeEvent::ActionCompleted { action_id, success, output, error, duration_ms, .. } => {
                            let _ = event_tx.send(EngineEvent::ActionCompleted {
                                action_id,
                                result: ActionResult {
                                    success,
                                    output,
                                    error,
                                    diff: None,
                                    duration_ms: duration_ms.unwrap_or_default(),
                                },
                            }).await;
                        }
                        RemoteClaudeEvent::ApprovalRequested { approval_id, action_type, summary, details, .. } => {
                            let _ = event_tx.send(EngineEvent::ApprovalRequested {
                                approval_id,
                                action_type: Self::parse_action_type(&action_type),
                                summary,
                                details: details.unwrap_or_else(|| serde_json::json!({})),
                            }).await;
                        }
                        RemoteClaudeEvent::Notice { kind, level, title, message, .. } => {
                            let _ = event_tx.send(EngineEvent::Notice { kind, level, title, message }).await;
                        }
                        RemoteClaudeEvent::UsageLimitsUpdated { usage, .. } => {
                            let _ = event_tx.send(EngineEvent::UsageLimitsUpdated {
                                usage: super::UsageLimitsSnapshot {
                                    current_tokens: usage.current_tokens,
                                    max_context_tokens: usage.max_context_tokens,
                                    context_window_percent: usage.context_window_percent,
                                    five_hour_percent: usage.five_hour_percent,
                                    weekly_percent: usage.weekly_percent,
                                    fable_weekly_percent: usage.fable_weekly_percent,
                                    opus_weekly_percent: usage.opus_weekly_percent,
                                    sonnet_weekly_percent: usage.sonnet_weekly_percent,
                                    five_hour_resets_at: usage.five_hour_resets_at,
                                    weekly_resets_at: usage.weekly_resets_at,
                                    fable_weekly_resets_at: usage.fable_weekly_resets_at,
                                    opus_weekly_resets_at: usage.opus_weekly_resets_at,
                                    sonnet_weekly_resets_at: usage.sonnet_weekly_resets_at,
                                },
                            }).await;
                        }
                        RemoteClaudeEvent::Error { message, recoverable, .. } => {
                            let _ = event_tx.send(EngineEvent::Error {
                                message,
                                recoverable: recoverable.unwrap_or(false),
                            }).await;
                        }
                        RemoteClaudeEvent::TurnCompleted { status, session_id, token_usage, stop_reason, .. } => {
                            if let Some(session_id) = session_id {
                                if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
                                    config.agent_session_id = Some(session_id);
                                }
                            }
                            if let Some(stop_reason) = stop_reason.filter(|reason| reason != "end_turn") {
                                let _ = event_tx.send(EngineEvent::Notice {
                                    kind: "claude_stop_reason".to_string(),
                                    level: "info".to_string(),
                                    title: "Claude stop reason".to_string(),
                                    message: stop_reason,
                                }).await;
                            }
                            let status = match status.as_str() {
                                "completed" => TurnCompletionStatus::Completed,
                                "interrupted" => TurnCompletionStatus::Interrupted,
                                _ => TurnCompletionStatus::Failed,
                            };
                            let _ = event_tx.send(EngineEvent::TurnCompleted {
                                token_usage: token_usage.map(|usage| super::TokenUsage {
                                    input: usage.input,
                                    output: usage.output,
                                    reasoning: None,
                                    cache_read: None,
                                    cache_write: None,
                                    cost_usd: None,
                                }),
                                status,
                            }).await;
                            break;
                        }
                        RemoteClaudeEvent::TransportClosed => {
                            anyhow::bail!("SSH 远端 Claude 事件连接已关闭");
                        }
                        RemoteClaudeEvent::Ready
                        | RemoteClaudeEvent::Models { .. }
                        | RemoteClaudeEvent::Version { .. }
                        | RemoteClaudeEvent::SessionHandleCreated { .. }
                        | RemoteClaudeEvent::SessionMessageAccepted { .. }
                        | RemoteClaudeEvent::SessionHandleInterrupted { .. }
                        | RemoteClaudeEvent::SessionHandleDestroyed { .. } => {}
                    }
                }
            }
        }
        if let Some(config) = self.state.lock().await.threads.get_mut(engine_thread_id) {
            config.active_request_id = None;
        }
        Ok(())
    }

    async fn steer_message(
        &self,
        _engine_thread_id: &str,
        _client_steer_id: &str,
        _content: &str,
        _input: TurnInput,
    ) -> Result<EngineSteerReceipt, anyhow::Error> {
        anyhow::bail!("Claude does not support mid-turn steering")
    }

    async fn respond_to_approval(
        &self,
        approval_id: &str,
        response: serde_json::Value,
        _route: Option<ApprovalRequestRoute>,
    ) -> Result<(), anyhow::Error> {
        let normalized = normalize_approval_response_for_engine("claude", response)
            .map_err(anyhow::Error::msg)?;
        let transport = self.ensure_transport().await?;
        transport
            .send_command(&serde_json::json!({
                "method": "approval_response",
                "params": {
                    "approvalId": approval_id,
                    "response": normalized,
                },
            }))
            .await
    }

    async fn interrupt(&self, engine_thread_id: &str) -> Result<(), anyhow::Error> {
        let transport = self.ensure_transport().await?;
        let request_id = self
            .state
            .lock()
            .await
            .threads
            .get(engine_thread_id)
            .and_then(|config| config.active_request_id.clone());
        let Some(request_id) = request_id else {
            return Ok(());
        };
        transport
            .send_command(&serde_json::json!({
                "method": "cancel",
                "params": { "requestId": request_id },
            }))
            .await
    }

    async fn archive_thread(&self, engine_thread_id: &str) -> Result<(), anyhow::Error> {
        self.state.lock().await.threads.remove(engine_thread_id);
        Ok(())
    }

    async fn unarchive_thread(&self, _engine_thread_id: &str) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn remote_event_routes_request_identity() {
        let event: RemoteClaudeEvent = serde_json::from_value(serde_json::json!({
            "type": "text_delta",
            "id": "query-1",
            "content": "hello",
        }))
        .unwrap();
        assert_eq!(event.request_id(), Some("query-1"));
    }

    #[test]
    fn remote_event_deserializes_models() {
        let event: RemoteClaudeEvent = serde_json::from_value(serde_json::json!({
            "type": "models",
            "id": "models-1",
            "models": [],
        }))
        .unwrap();
        assert!(matches!(event, RemoteClaudeEvent::Models { models, .. } if models.is_empty()));
    }

    #[tokio::test]
    async fn read_remote_session_uses_id_path_without_cwd_query() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let session_id = "11111111-1111-4111-8111-111111111111";
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(&format!("GET /sessions/{session_id} HTTP/1.1")));
            assert!(!request.contains("cwd="));
            let body = format!(
                r#"{{"id":"{session_id}","sessionId":"{session_id}","cwd":"/work/project","title":"检查远端项目","updatedAt":"2026-08-19T00:00:00.000Z"}}"#
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let engine = ClaudeRemoteEngine::new(format!("http://{address}"));
        let session = engine.read_remote_session(session_id).await.unwrap();
        assert_eq!(session.id, session_id);
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.cwd, "/work/project");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn read_remote_session_preserves_http_status_and_body() {
        // 迁移留痕：404 旧测试曾与普通 HTTP 错误共用此循环，生产代码现已将其转换为
        // RemoteClaudeSessionNotFoundError，禁止恢复该 tuple。
        // (404, "Not Found", "session not found"),
        for (status, reason, detail) in [
            (400, "Bad Request", "invalid session id"),
            (409, "Conflict", "multiple session files"),
            (500, "Internal Server Error", "session summary unavailable"),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let detail_for_server = detail.to_string();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await.unwrap();
                let body = format!(r#"{{"error":"{detail_for_server}"}}"#);
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });

            let engine = ClaudeRemoteEngine::new(format!("http://{address}"));
            let error = engine
                .read_remote_session("22222222-2222-4222-8222-222222222222")
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(&format!("HTTP {status} {reason}")));
            assert!(error.contains(detail));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn read_remote_session_404_maps_to_not_found_error() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let session_id = "22222222-2222-4222-8222-222222222222";
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let body = r#"{"error":"session not found"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let engine = ClaudeRemoteEngine::new(format!("http://{address}"));
        let error = engine.read_remote_session(session_id).await.unwrap_err();
        let not_found = error
            .downcast_ref::<RemoteClaudeSessionNotFoundError>()
            .expect("HTTP 404 should map to RemoteClaudeSessionNotFoundError");
        assert_eq!(not_found.session_id, session_id);
        server.await.unwrap();
    }
}
