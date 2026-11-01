use std::sync::Arc;
use std::{collections::HashMap, time::{Duration, Instant}};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tauri::Emitter;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{
    BaseCliMcp, CliExecutionContext, CliForkedThread, CliLocationKind, CliMcpRuntime,
    CliReviewStarted, CliSessionNotFoundError,
    CliSessionSnapshot, CliTool, CliUsageQueryScope, McpInvocationContext, McpToolResult,
    map_context_usage,
};
use crate::{
    cli_tools::{engine_check, mcp::chat_business::*},
    db,
    engines::{
        capabilities_for_engine,
        codex::{CodexEngine, CodexReviewStarted, CodexThreadNotFoundError},
        map_engine_capabilities, map_model_info, map_provider_usage,
        ApprovalRequestRoute, CodexRuntimeEvent, Engine, EngineCapabilities, EngineEvent,
        EngineSteerReceipt, EngineThread, ModelInfo, SandboxPolicy, ThreadScope,
        ThreadSyncSnapshot, TurnCompletionStatus, TurnInput,
    },
    extensions,
    local_cli_service_lifecycle::{LocalCliHandle, LocalCliServiceLifecycle},
    runtime_env,
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, CliContextUsageDto, CodexAppDto,
        CodexPluginDto, CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionItemDto, OpenCodeRuntimeCatalogDto,
        EngineCheckResultDto, EngineRuntimeUpdatedDto, ExtensionRefreshJob, MessageStatusDto,
        ThreadDto, ThreadStatusDto, ThreadUpdateDto, WorkspaceDto,
    },
    models::chat::{
        ApprovalResponseOutcomeDto, ApprovalResponseRequest, ChatBusinessError, ChatReviewRequest,
        CodexReviewDeliveryPayload, CodexReviewTargetPayload, SteerMessageRequest,
    },
    path_utils, remote_project_codex_runtime_service, ssh,
    state::AppState,
};

/// Codex SSH 会话刷新单次 WebSocket 请求的原有等待上限。
const SESSION_REFRESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Codex SSH 会话刷新保留的原有最大分页数。
const SESSION_REFRESH_MAX_PAGES: usize = 50;

/// 判断扩展目录刷新是否需要串行使用 Codex runtime 锁。
pub(crate) fn requires_codex_catalog_refresh_lock(provider_id: &str) -> bool {
    provider_id == "codex"
}

/// Codex 对统一 CLI 操作接口的实现。
///
/// 本机项目继续使用现有 Codex 业务对象；SSH 项目继续使用现有远端 Codex
/// 服务和生命周期入口。任何远端操作失败时都直接返回错误，不会改用本机 Codex。
#[derive(Clone)]
pub struct CodexCli {
    /// 当前 Codex 共用的 MCP 业务实现。
    mcp: BaseCliMcp,
    state: AppState,
    remote_turn_use: Arc<Mutex<Option<Arc<CodexEngine>>>>,
}

impl CodexCli {
    pub fn new(state: AppState) -> Self {
        Self::with_mcp_runtime(
            state,
            CliMcpRuntime {
                cli_id: "codex".to_string(),
                location: CliLocationKind::Local,
            },
        )
    }

    /// 按 Factory 指定的本机或 SSH 运行位置创建 Codex MCP 实现。
    pub(crate) fn with_mcp_runtime(state: AppState, runtime: CliMcpRuntime) -> Self {
        let mcp = BaseCliMcp::new(state.clone(), runtime);
        Self {
            mcp,
            state,
            remote_turn_use: Arc::new(Mutex::new(None)),
        }
    }

    /// 读取 Codex 本地扩展快照，并按当前上下文登记一次后台刷新排期。
    async fn load_cached_extension_catalog(
        &self,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        // 先登记当前目录的刷新时间，再读取 provider 专属缓存快照。
        // 页面请求只写排期并唤醒调度器，不在请求路径执行 Codex CLI。
        self.schedule_extension_catalog_context(cwd.map(str::to_string))
            .await?;
        let context_key = extensions::refresh::context_key(cwd);
        let snapshots = db::extensions::load_snapshots(&self.state.db, self.id(), &context_key)?;
        let is_refreshing = self
            .state
            .extension_catalog_refreshes
            .is_refreshing(self.id(), &context_key)
            .await;
        let refresh_completed_at = self
            .state
            .extension_catalog_refreshes
            .refresh_completed_at(self.id(), &context_key)
            .await;
        Ok(extensions::refresh::cached_catalog_from_snapshots(
            self.id(),
            cwd,
            &snapshots,
            is_refreshing,
            refresh_completed_at,
        ))
    }

    /// 按 Codex 原扩展目录实现刷新各类型并写入缓存状态。
    async fn run_extension_catalog_refresh(
        &self,
        app: tauri::AppHandle,
        context_key: String,
        cwd: Option<String>,
        requested_kinds: Vec<String>,
    ) {
        // 逐类型使用 Codex 原目录实现刷新，并按每类结果写入缓存。
        for kind in requested_kinds {
            let attempted_at = runtime_env::system_time_rfc3339();
            // 只有带项目目录的刷新需要解析工作区；全局缓存使用空的本机上下文。
            let context_result = match cwd.as_deref() {
                Some(cwd) => self.execution_context_for_cwd(Some(cwd)).await,
                None => Ok(CliExecutionContext {
                    workspace_id: String::new(),
                    root_path: String::new(),
                    location_kind: CliLocationKind::Local,
                    ssh_connection_id: None,
                }),
            };
            // Codex 扩展目录读取共用原有 runtime 锁，保持技能服务与 CLI 读取互斥。
            let refresh_result = match context_result {
                Ok(context) if requires_codex_catalog_refresh_lock(self.id()) => {
                    let _guard = self
                        .state
                        .extension_catalog_refreshes
                        .codex_catalog_refresh_lock()
                        .await;
                    self.refresh_extension_catalog(&context, cwd.as_deref(), &[kind.clone()])
                        .await
                }
                Ok(context) => {
                    self.refresh_extension_catalog(&context, cwd.as_deref(), &[kind.clone()])
                        .await
                }
                Err(error) => Err(error),
            };
            // 保留成功快照和 refresh_failed 存储码的原记录规则。
            let write_result = match refresh_result {
                Ok(results) => match results.into_iter().next() {
                    Some(result) if result.success => db::extensions::record_success(
                        &self.state.db,
                        self.id(),
                        &context_key,
                        &kind,
                        &result.items,
                        &attempted_at,
                    ),
                    _ => db::extensions::record_failure(
                        &self.state.db,
                        self.id(),
                        &context_key,
                        &kind,
                        &attempted_at,
                        "refresh_failed",
                    )
                    .map(|_| ()),
                },
                Err(error) => {
                    log::error!(
                        "Codex 扩展目录刷新失败: context={context_key} kind={kind} error={error:#}"
                    );
                    db::extensions::record_failure(
                        &self.state.db,
                        self.id(),
                        &context_key,
                        &kind,
                        &attempted_at,
                        "refresh_failed",
                    )
                    .map(|_| ())
                }
            };
            if let Err(error) = write_result {
                log::error!(
                    "保存 Codex 扩展目录刷新状态失败: context={context_key} kind={kind} error={error:#}"
                );
            }
            extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
        }

        // 刷新完成后清除 in-flight 标记并再次通知目录页。
        self.state
            .extension_catalog_refreshes
            .finish(self.id(), &context_key)
            .await;
        extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
    }

    async fn local_engine(&self) -> Result<Arc<CodexEngine>> {
        let service = LocalCliServiceLifecycle::get("codex").await?;
        match service.handle() {
            LocalCliHandle::Codex(engine) => Ok(engine.clone()),
            _ => anyhow::bail!("本地 CLI 生命周期返回了错误的 Codex 句柄类型"),
        }
    }

    async fn configure_local_computer_control(&self) -> Result<Arc<CodexEngine>> {
        let engine = self.local_engine().await?;
        // 旧本地动态工具注入入口保留迁移留痕，统一 MCP Gateway 已接替：
        // engine.set_computer_control_service(self.state.computer_control_service.clone());
        // engine.set_auracoder_thread_mcp_service(self.state.auracoder_thread_mcp_service.clone());
        Ok(engine)
    }

    /// 用户进入某个 workspace 的 Codex 功能时，读取该 workspace 的正式项目位置，作为后续本机或 SSH 操作的依据。
    pub async fn execution_context(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<CliExecutionContext> {
        let workspace_id = workspace_id
            .map(str::trim)
            .filter(|workspace_id| !workspace_id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("请先选择项目"))?
            .to_string();
        let db = self.state.db.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            db::workspaces::find_workspace_by_id(&db, &workspace_id)?.ok_or_else(|| {
                anyhow::anyhow!("项目不存在或已被移除，请重新选择项目: {workspace_id}")
            })
        })
        .await
        .context("读取 Codex workspace 任务失败")??;
        CliExecutionContext::from_workspace(&workspace)
    }

    /// 用户刷新某个项目目录的 Codex 扩展时，找到该目录所属的 workspace，保证 SSH 项目只读取正式绑定的远端目录。
    pub async fn execution_context_for_cwd(
        &self,
        cwd: Option<&str>,
    ) -> Result<CliExecutionContext> {
        let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) else {
            return self.execution_context(None).await;
        };
        let db = self.state.db.clone();
        let cwd = cwd.to_string();
        let workspace = tokio::task::spawn_blocking(move || {
            let workspaces = db::workspaces::list_workspaces(&db)?;
            for workspace in workspaces {
                if path_utils::paths_equal(&workspace.root_path, &cwd) {
                    return Ok::<_, anyhow::Error>(Some(workspace));
                }
            }
            Ok(None)
        })
        .await
        .context("按项目目录读取 Codex workspace 任务失败")??;
        match workspace {
            Some(workspace) => CliExecutionContext::from_workspace(&workspace),
            None => self.execution_context(None).await,
        }
    }

    // 旧的 Codex 专用整轮入口已经停用；调用方现在通过 CliTool::acquire_turn 取得整轮使用权。
    // pub async fn for_turn(
    //     state: AppState,
    //     context: &CliExecutionContext,
    //     thread_id: &str,
    // ) -> Result<Self> {
    //     let cli = Self::new(state);
    //     if context.location_kind == CliLocationKind::Ssh {
    //         let workspace = cli.load_workspace(context).await?;
    //         let service_use =
    //             remote_project_codex_runtime_service::acquire_turn(&workspace, thread_id).await?;
    //         *cli.remote_turn_use.lock().await = Some(service_use);
    //     }
    //     Ok(cli)
    // }

    async fn load_workspace(&self, context: &CliExecutionContext) -> Result<WorkspaceDto> {
        let db = self.state.db.clone();
        let workspace_id = context.workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            db::workspaces::find_workspace_by_id(&db, &workspace_id)
        })
        .await
        .context("读取当前 workspace 任务失败")??
        .ok_or_else(|| anyhow::anyhow!("workspace 不存在: {}", context.workspace_id))?;

        anyhow::ensure!(
            path_utils::paths_equal(&workspace.root_path, &context.root_path),
            "当前 workspace 的项目目录与 Codex 操作目标不一致"
        );

        match context.location_kind {
            CliLocationKind::Local => {
                anyhow::ensure!(
                    workspace.location_kind != "ssh",
                    "当前 workspace 是 SSH 远端项目，不能使用本机 Codex"
                );
            }
            CliLocationKind::Ssh => {
                anyhow::ensure!(
                    workspace.location_kind == "ssh",
                    "当前 workspace 不是 SSH 远端项目"
                );
                anyhow::ensure!(
                    workspace.ssh_connection_id == context.ssh_connection_id,
                    "当前 workspace 的 SSH 绑定与 Codex 操作目标不一致"
                );
            }
        }

        Ok(workspace)
    }

    /// 将当前线程保存的统一权限对象解析为本次 Codex 原生权限请求。
    async fn parse_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<Value> {
        // 确认工作区和会话属于本次 Codex 操作，再只从统一权限对象读取权限。
        let workspace = self.load_workspace(context).await?;
        anyhow::ensure!(thread.engine_id == "codex", "当前会话不属于 Codex");
        anyhow::ensure!(
            thread.workspace_id == workspace.id,
            "当前会话不属于本次 Codex 操作的 workspace"
        );
        let permissions = thread.permission_mode.clone().unwrap_or_default();
        permissions.validate()?;

        // 将 workspace 信任状态归一化，供 automatic 权限选择和原生 reviewer 决策复用。
        let workspace_trust = workspace.trust_level.as_str();
        let requested_trust = permissions.trust.first().map(String::as_str);
        let effective_trust = match requested_trust {
            Some(value) if value != "automatic" => value,
            _ => workspace_trust,
        };
        let preset = permissions.autonomy_preset.first().map(String::as_str);
        let approval = permissions.approval.first().map(String::as_str).unwrap_or("automatic");
        let sandbox = permissions.sandbox.first().map(String::as_str).unwrap_or("automatic");
        let network = permissions.network.first().map(String::as_str).unwrap_or("automatic");

        // 将预设或自定义统一权限字段映射为 Codex 原生审批、沙箱和网络字段。
        let (approval_policy, approval_is_automatic, mut sandbox_mode, mut allow_network, auto_mcp) =
            match preset {
                Some("read-only") => ("untrusted", false, Some("read-only"), false, false),
                Some("ask") => ("on-request", false, Some("workspace-write"), false, false),
                Some("auto") => ("on-request", true, Some("workspace-write"), true, true),
                Some("full") => ("never", false, Some("danger-full-access"), true, true),
                Some("automatic") | None => {
                    let (approval_policy, allow_network) = if effective_trust == "restricted" {
                        ("untrusted", false)
                    } else {
                        ("on-request", effective_trust == "trusted")
                    };
                    let sandbox_mode = if preset.is_some() {
                        Some("workspace-write")
                    } else {
                        match sandbox {
                            "read-only" => Some("read-only"),
                            "workspace-write" => Some("workspace-write"),
                            "full-access" => Some("danger-full-access"),
                            "automatic" => Some("workspace-write"),
                            _ => anyhow::bail!("Codex sandbox 权限值不支持: {sandbox}"),
                        }
                    };
                    let approval_policy = if preset.is_some() {
                        approval_policy
                    } else {
                        match approval {
                            "restricted" => "untrusted",
                            "ask" => "on-request",
                            "autonomous" => "never",
                            "automatic" => approval_policy,
                            _ => anyhow::bail!("Codex approval 权限值不支持: {approval}"),
                        }
                    };
                    let approval_is_automatic =
                        preset.is_some() || approval == "automatic";
                    let allow_network = if preset.is_some() {
                        allow_network
                    } else {
                        match network {
                            "enabled" => true,
                            "restricted" => false,
                            "automatic" => allow_network,
                            _ => anyhow::bail!("Codex network 权限值不支持: {network}"),
                        }
                    };
                    (approval_policy, approval_is_automatic, sandbox_mode, allow_network, false)
                }
                _ => anyhow::bail!("Codex autonomy preset 不支持: {}", preset.unwrap_or_default()),
            };

        // full-access 原生沙箱必须同时允许网络，避免生成互相矛盾的原生策略。
        if sandbox_mode == Some("danger-full-access") {
            allow_network = true;
        }
        let approvals_reviewer = if effective_trust == "trusted" && approval_is_automatic {
            "auto_review"
        } else {
            "user"
        };

        // 本地 external sandbox 继续使用其既有策略；自定义沙箱遇到不支持组合时明确失败。
        if context.location_kind == CliLocationKind::Local
            && self.local_engine().await?.uses_external_sandbox().await
        {
            if preset.is_some() && preset != Some("full") {
                sandbox_mode = None;
            } else if preset.is_none() && sandbox == "automatic" {
                sandbox_mode = None;
            } else if preset.is_none()
                && matches!(sandbox_mode, Some("read-only" | "workspace-write"))
            {
                anyhow::bail!(
                    "Codex 自定义 read-only/workspace-write 沙箱在本地 external sandbox 模式下不可用"
                );
            }
        }

        // 记录当前权限解析结果，不记录任何凭据或敏感内容。
        log::debug!(
            "Codex 当前权限已解析: thread_id={} workspace_id={} approval_policy={} approvals_reviewer={}",
            thread.id,
            workspace.id,
            approval_policy,
            approvals_reviewer
        );
        Ok(json!({
            "approvalPolicy": approval_policy,
            "approvalsReviewer": approvals_reviewer,
            "sandboxMode": sandbox_mode,
            "allowNetwork": allow_network,
            "autoApproveMcpElicitations": auto_mcp,
        }))
    }

    fn map_session(
        summary: crate::engines::CodexRemoteThreadSummary,
        is_ssh: bool,
    ) -> CliSessionSnapshot {
        let status = match summary.status_type.as_str() {
            "systemError" => ThreadStatusDto::Error,
            "active"
                if summary.active_flags.iter().any(|flag| {
                    matches!(flag.as_str(), "waitingOnApproval" | "waitingOnUserInput")
                }) =>
            {
                ThreadStatusDto::AwaitingApproval
            }
            "active" => ThreadStatusDto::Streaming,
            "completed" => ThreadStatusDto::Completed,
            _ => ThreadStatusDto::Idle,
        };
        let timestamp_to_rfc3339 = |timestamp: i64| {
            let (seconds, nanos) = if timestamp > 10_000_000_000 {
                (timestamp / 1000, ((timestamp % 1000) as u32) * 1_000_000)
            } else {
                (timestamp, 0)
            };
            chrono::DateTime::from_timestamp(seconds, nanos).map(|value| value.to_rfc3339())
        };
        let created_at = timestamp_to_rfc3339(summary.created_at);
        let updated_at = timestamp_to_rfc3339(summary.updated_at);
        let preview = (!summary.preview.trim().is_empty()).then(|| summary.preview.clone());
        let metadata = json!({
            "sshRemote": is_ssh,
            "codexRemoteCwd": summary.cwd.clone(),
            "codexRemote": {
                "id": summary.engine_thread_id.clone(),
                "title": summary.title.clone(),
                "preview": summary.preview.clone(),
                "cwd": summary.cwd.clone(),
                "model": summary.model_id.clone(),
                "reasoningEffort": summary.reasoning_effort.clone(),
                "modelProvider": summary.model_provider.clone(),
                "sourceKind": summary.source_kind.clone(),
                "status": {
                    "type": summary.status_type.clone(),
                    "activeFlags": summary.active_flags.clone(),
                },
                "archived": summary.archived,
                "createdAt": summary.created_at,
                "updatedAt": summary.updated_at,
            },
            "codexModelProvider": summary.model_provider.clone(),
            // "reasoningEffort": summary.reasoning_effort.clone(), // 顶层六字段 metadata 镜像已停用。
            "codexSourceKind": summary.source_kind.clone(),
            "codexThreadStatus": summary.status_type.clone(),
            "codexThreadActiveFlags": summary.active_flags.clone(),
        });

        CliSessionSnapshot {
            engine_thread_id: summary.engine_thread_id.clone(),
            title: summary.title.unwrap_or(summary.engine_thread_id),
            preview,
            cwd: summary.cwd,
            model_id: summary.model_id.unwrap_or_else(|| "unknown".to_string()),
            reasoning_effort: summary.reasoning_effort,
            created_at,
            updated_at,
            source_kind: Some(summary.source_kind),
            raw_status: Some(summary.status_type),
            active_flags: summary.active_flags,
            status,
            archived: summary.archived,
            metadata,
        }
    }
}

impl CodexCli {
    /// 校验 Codex 审查请求、创建审查消息，并以当前 CLI 实例启动后台轮次。
    async fn start_codex_review(
        self: Arc<Self>,
        app: tauri::AppHandle,
        request: ChatReviewRequest,
    ) -> std::result::Result<ThreadDto, ChatBusinessError> {
        let state = self.state.clone();
        let ChatReviewRequest {
            thread_id,
            target,
            delivery,
        } = request;

        // 同一线程已有活动轮次时拒绝重复创建审查任务。
        if state.turns.get(&thread_id).await.is_some() {
            return Err(ChatBusinessError::new(
                "CHAT_OPERATION_FAILED",
                "本线程已有正在运行的任务，请停止后再发起审查",
            ));
        }

        // 读取并验证审查源线程及其 workspace，避免审查错误引擎或 SSH 线程。
        let db = state.db.clone();
        let lookup_thread_id = thread_id.clone();
        let source_thread = thread_business::run_db(db.clone(), move |db| {
            db::threads::get_thread(db, &lookup_thread_id)
        })
        .await
        .map_err(|error| {
            log::error!("Codex 审查读取源线程失败: thread_id={thread_id} error={error}");
            ChatBusinessError::new("CHAT_OPERATION_FAILED", "读取审查会话失败")
        })?
        .ok_or_else(|| ChatBusinessError::new("CHAT_OPERATION_FAILED", "审查会话不存在"))?;

        if source_thread.engine_id != "codex" {
            return Err(ChatBusinessError::new(
                "UNSUPPORTED_OPERATION",
                "原生代码审查仅支持 Codex 会话",
            ));
        }

        let workspace_id = source_thread.workspace_id.clone();
        let source_workspace = thread_business::run_db(db.clone(), move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?.ok_or_else(|| {
                anyhow::anyhow!("workspace not found: {workspace_id}")
            })
        })
        .await
        .map_err(|error| {
            log::error!(
                "Codex 审查读取 workspace 失败: thread_id={} error={error}",
                source_thread.id
            );
            ChatBusinessError::new("CHAT_OPERATION_FAILED", "读取审查项目失败")
        })?;
        if source_workspace.location_kind == "ssh" {
            return Err(ChatBusinessError::new(
                "UNSUPPORTED_OPERATION",
                "SSH 远端 Codex 暂不支持原生代码审查",
            ));
        }

        let source_engine_thread_id = source_thread.engine_thread_id.clone().ok_or_else(|| {
            ChatBusinessError::new("VALIDATION_FAILED", "Codex 审查会话尚未初始化")
        })?;
        let effective_delivery = delivery.unwrap_or(CodexReviewDeliveryPayload::Inline);
        let (target_payload, review_message, review_title) =
            Self::normalize_codex_review_target(&target).map_err(|error| {
                log::error!("Codex 审查目标无效: thread_id={thread_id} error={error}");
                ChatBusinessError::new("VALIDATION_FAILED", "代码审查目标无效")
            })?;
        let initial_turn_model_id = source_thread.model_id.clone();
        let reasoning_effort = source_thread.reasoning_effort.clone();

        // 在数据库写入前登记本轮占用，避免同一线程并发启动审查或聊天。
        let cancellation = CancellationToken::new();
        if !state
            .turns
            .try_register(&source_thread.id, cancellation.clone())
            .await
        {
            return Err(ChatBusinessError::new(
                "CHAT_OPERATION_FAILED",
                "本线程已有正在运行的任务，请停止后再发起审查",
            ));
        }

        // 按原内联或独立投递规则创建用户消息和助手占位消息。
        let (review_thread, assistant_message_id) = match thread_business::run_db(db.clone(), {
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
                    if let Some(metadata) = Self::clone_codex_review_metadata(
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
                log::error!(
                    "Codex 审查消息持久化失败: thread_id={} error={error}",
                    source_thread.id
                );
                return Err(ChatBusinessError::new(
                    "PERSISTENCE_FAILED",
                    "创建代码审查消息失败",
                ));
            }
        };

        if matches!(effective_delivery, CodexReviewDeliveryPayload::Detached)
            && !state
                .turns
                .try_register(&review_thread.id, cancellation.clone())
                .await
        {
            log::warn!(
                "failed to register cancellation token for detached review thread {}",
                review_thread.id
            );
        }

        let state_for_task = state.clone();
        let app_for_task = app.clone();
        let cli_for_task = self.clone();
        let source_thread_for_task = source_thread.clone();
        let review_thread_for_task = review_thread.clone();
        let source_engine_thread_id_for_task = source_engine_thread_id.clone();
        let assistant_message_id_for_task = assistant_message_id.clone();
        let delivery_label = match effective_delivery {
            CodexReviewDeliveryPayload::Inline => "inline".to_string(),
            CodexReviewDeliveryPayload::Detached => "detached".to_string(),
        };

        // 后台轮次接收当前 Arc<Self> 的克隆，维持 CLI 实例及生命周期占用一致。
        tokio::spawn(async move {
            cli_for_task
                .run_codex_review_turn(
                    app_for_task,
                    state_for_task,
                    source_thread_for_task,
                    review_thread_for_task,
                    source_engine_thread_id_for_task,
                    assistant_message_id_for_task,
                    initial_turn_model_id,
                    target_payload,
                    delivery_label,
                    cancellation,
                )
                .await;
        });

        Ok(review_thread)
    }

    /// 按 Codex 审查目标生成原生请求、用户消息和线程标题。
    pub(crate) fn normalize_codex_review_target(
        target: &CodexReviewTargetPayload,
    ) -> std::result::Result<(Value, String, String), String> {
        match target {
            CodexReviewTargetPayload::UncommittedChanges => Ok((
                serde_json::json!({"type": "uncommittedChanges"}),
                "Review uncommitted changes.".to_string(),
                "Review: Uncommitted changes".to_string(),
            )),
            CodexReviewTargetPayload::BaseBranch { branch } => {
                let branch = branch.trim();
                if branch.is_empty() {
                    return Err("Base branch review requires a branch name.".to_string());
                }
                Ok((
                    serde_json::json!({"type": "baseBranch", "branch": branch}),
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
                    None => serde_json::json!({"type": "commit", "sha": sha}),
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
                    serde_json::json!({"type": "custom", "instructions": instructions}),
                    instructions.to_string(),
                    "Review: Custom".to_string(),
                ))
            }
        }
    }

    /// 复制 Codex 会话 metadata 给独立审查线程，并清除仅属于运行时的字段。
    pub(crate) fn clone_codex_review_metadata(existing: Option<&Value>, model_id: &str) -> Option<Value> {
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

    /// 使用同一个 Codex CLI 实例观察原生审查事件并完成线程收尾。
    async fn run_codex_review_turn(
        self: Arc<Self>,
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

        let codex: Arc<dyn CliTool> = self.clone();
        let (context, context_error) = match self
            .execution_context(Some(source_thread.workspace_id.as_str()))
            .await
        {
            Ok(context) => (Some(context), None),
            Err(error) => {
                log::error!("Codex 审查读取执行上下文失败: thread_id={} error={error:#}", source_thread.id);
                (None, Some(format!("读取 Codex 项目失败：{error:#}")))
            }
        };
        let source_engine_thread_id_for_engine = source_engine_thread_id.clone();
        let target_for_engine = target.clone();
        let delivery_for_engine = delivery.clone();
        let cancellation_for_engine = cancellation.clone();

        let mut engine_task = tokio::spawn(async move {
            let Some(context) = context else {
                let message = context_error.unwrap_or_else(|| "读取 Codex 项目失败".to_string());
                let _ = event_tx
                    .send(EngineEvent::Error {
                        message,
                        recoverable: false,
                    })
                    .await;
                return Ok(());
            };
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
            self.as_ref(),
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

        /*
         * 旧 review 实现将通道关闭、EngineTask 错误和取消后的 Streaming 状态直接收尾。
         * 保留旧代码，新的 review 轮次观察和唯一收尾在下方实现。
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
                                self.as_ref(),
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
                    "at": runtime_env::system_time_rfc3339(),
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
                                    self.as_ref(),
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
                                self.as_ref(),
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
                        self.as_ref(),
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
                self.as_ref(),
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
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "engine_task_complete",
                    "consumer": "review",
                    "thread_id": review_thread.id.clone(),
                    "result": "ok",
                }))
                .await;
            }
            Ok(Err(error)) => {
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
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
                    "at": runtime_env::system_time_rfc3339(),
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
        */

        /// 代码审查轮次唯一允许的两个终止信号，异常只能继续观察。
        enum ReviewEndSignal {
            /// 收到并交付引擎的正常完成事件。
            TaskCompletedAndDelivered,
            /// 用户通过当前线程取消令牌主动停止本轮。
            UserClickedStop,
        }

        #[derive(Debug)]
        enum ReviewObservationWait {
            /// 当前原生审查任务已返回或发生 join 错误。
            Engine(std::result::Result<anyhow::Result<()>, tokio::task::JoinError>),
            /// 收到下一条引擎事件，None 表示事件通道关闭。
            Event(Option<EngineEvent>),
            /// 合并缓冲空闲，需要刷新待处理的事件。
            FlushPending,
            /// 当前线程 CancellationToken 已被用户取消。
            UserClickedStop,
            /// 审查事件在设定观察窗口内无新进展。
            TimedOut,
        }

        // 将审查异常转换为固定安全提示，避免原始异常进入前端错误状态。
        macro_rules! emit_safe_review_notice {
            () => {{
                let notice = EngineEvent::Notice {
                    kind: "turn_exception_observed".to_string(),
                    level: "info".to_string(),
                    title: "执行提示".to_string(),
                    message: "执行出现异常，请重试或停止本轮".to_string(),
                    // 审查异常安全提示不附带原始异常或后台任务元数据。
                    metadata: None,
                };
                let progress = process_stream_event(
                    &app,
                    &state,
                    &review_thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &notice,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
                    self.as_ref(),
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
            }};
        }

        let mut engine_task_finished = false;
        let mut event_channel_closed = false;
        let mut completed_turn_event: Option<EngineEvent> = None;
        let end_signal = 'review_turn_loop: loop {
            let wait = if pending_event.is_some() {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => ReviewObservationWait::UserClickedStop,
                    result = &mut engine_task, if !engine_task_finished => ReviewObservationWait::Engine(result),
                    event = receive_engine_event_with_diagnostics(&mut event_rx, "review_coalesce"), if !event_channel_closed => ReviewObservationWait::Event(event),
                    _ = tokio::time::sleep(STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL) => ReviewObservationWait::FlushPending,
                    _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => ReviewObservationWait::TimedOut,
                }
            } else {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => ReviewObservationWait::UserClickedStop,
                    result = &mut engine_task, if !engine_task_finished => ReviewObservationWait::Engine(result),
                    event = receive_engine_event_with_diagnostics(&mut event_rx, "review_direct"), if !event_channel_closed => ReviewObservationWait::Event(event),
                    _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => ReviewObservationWait::TimedOut,
                }
            };

            log::info!(
                "codex review turn wait result: thread_id={}, engine_id={}, source_engine_thread_id={}, wait={wait:?}",
                review_thread.id,
                review_thread.engine_id,
                source_engine_thread_id,
            );

            let incoming_event = match wait {
                ReviewObservationWait::Engine(Ok(Ok(()))) => {
                    engine_task_finished = true;
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "engine_task_complete",
                        "consumer": "review",
                        "thread_id": review_thread.id.clone(),
                        "engine_id": review_thread.engine_id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "result": "ok",
                    }))
                    .await;
                    continue;
                }
                ReviewObservationWait::Engine(Ok(Err(error))) => {
                    engine_task_finished = true;
                    let raw_error = error.to_string();
                    log::error!(
                        "codex review engine task returned error: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                        review_thread.engine_id,
                        review_thread.id,
                        source_engine_thread_id,
                    );
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "failure_kind": "engine_task",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                    continue;
                }
                ReviewObservationWait::Engine(Err(error)) => {
                    engine_task_finished = true;
                    let raw_error = error.to_string();
                    log::error!(
                        "codex review engine task join error: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                        review_thread.engine_id,
                        review_thread.id,
                        source_engine_thread_id,
                    );
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "failure_kind": "engine_task_join",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                    continue;
                }
                ReviewObservationWait::Event(Some(event)) => event,
                ReviewObservationWait::Event(None) => {
                    event_channel_closed = true;
                    let raw_error = "代码审查事件接收通道已关闭";
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "engine_event_channel_closed",
                        "consumer": "review",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                    continue;
                }
                ReviewObservationWait::FlushPending => {
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
                            self.as_ref(),
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
                ReviewObservationWait::UserClickedStop => {
                    break 'review_turn_loop ReviewEndSignal::UserClickedStop;
                }
                ReviewObservationWait::TimedOut => {
                    let raw_error = "代码审查本轮在15分钟内没有收到新的执行事件";
                    log::warn!(
                        "codex review turn observation timed out: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                        review_thread.engine_id,
                        review_thread.id,
                        source_engine_thread_id,
                    );
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "failure_kind": "timeout",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                    continue;
                }
            };

            log::info!(
                "codex review incoming EngineEvent before processing: thread_id={}, engine_id={}, source_engine_thread_id={}, event={incoming_event:?}",
                review_thread.id,
                review_thread.engine_id,
                source_engine_thread_id,
            );

            let mut current_event = match &incoming_event {
                EngineEvent::Error {
                    message,
                    recoverable,
                } => {
                    let raw_error = message.clone();
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": *recoverable,
                        "raw_error": raw_error,
                    }))
                    .await;
                    EngineEvent::Notice {
                        kind: "turn_exception_observed".to_string(),
                        level: "info".to_string(),
                        title: "执行提示".to_string(),
                        message: "执行出现异常，请重试或停止本轮".to_string(),
                        // 审查异常安全提示不附带原始异常或结构化后台任务元数据。
                        metadata: None,
                    }
                }
                EngineEvent::TurnCompleted {
                    status: TurnCompletionStatus::Failed,
                    ..
                }
                | EngineEvent::TurnCompleted {
                    status: TurnCompletionStatus::Interrupted,
                    ..
                } => {
                    let status = match &incoming_event {
                        EngineEvent::TurnCompleted { status, .. } => match status {
                            TurnCompletionStatus::Failed => "failed",
                            TurnCompletionStatus::Interrupted => "interrupted",
                            TurnCompletionStatus::Completed => "completed",
                        },
                        _ => "unknown",
                    };
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_completion_observed",
                        "consumer": "review",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "status": status,
                    }))
                    .await;
                    EngineEvent::Notice {
                        kind: "turn_exception_observed".to_string(),
                        level: "info".to_string(),
                        title: "执行提示".to_string(),
                        message: "执行出现异常，请重试或停止本轮".to_string(),
                        // 审查未完成报告不附带原始异常或结构化后台任务元数据。
                        metadata: None,
                    }
                }
                _ => incoming_event.clone(),
            };

            loop {
                if let Some(previous_event) = pending_event.take() {
                    let coalesce_result = if matches!(
                        &current_event,
                        EngineEvent::TurnCompleted {
                            status: TurnCompletionStatus::Completed,
                            ..
                        }
                    ) {
                        Err((previous_event, current_event))
                    } else {
                        try_coalesce_stream_events(previous_event, current_event)
                    };
                    match coalesce_result {
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
                                    self.as_ref(),
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
                                self.as_ref(),
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
                } else if matches!(
                    &current_event,
                    EngineEvent::TurnCompleted {
                        status: TurnCompletionStatus::Completed,
                        ..
                    }
                ) {
                    completed_turn_event = Some(current_event);
                    break 'review_turn_loop ReviewEndSignal::TaskCompletedAndDelivered;
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
                        self.as_ref(),
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
        };

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
                self.as_ref(),
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

        // 用户停止依赖 start_review 的取消令牌完成唯一底层 interrupt；两种收尾都等待任务闭合。
        if !engine_task_finished {
            engine_task_finished = true;
            let engine_task_result = if matches!(&end_signal, ReviewEndSignal::UserClickedStop) {
                match tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task).await {
                    Ok(result) => Some(result),
                    Err(_) => {
                        let raw_error = "用户停止后代码审查后台任务未在清理窗口内结束";
                        log::warn!(
                            "codex review engine task cleanup timed out: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                            review_thread.engine_id,
                            review_thread.id,
                            source_engine_thread_id,
                        );
                        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                            "at": runtime_env::system_time_rfc3339(),
                            "event": "turn_exception_observed",
                            "consumer": "review",
                            "failure_kind": "engine_task_cleanup_timeout",
                            "engine_id": review_thread.engine_id.clone(),
                            "thread_id": review_thread.id.clone(),
                            "engine_thread_id": source_engine_thread_id.clone(),
                            "recoverable": false,
                            "raw_error": raw_error,
                        }))
                        .await;
                        emit_safe_review_notice!();
                        engine_task.abort();
                        let _ = (&mut engine_task).await;
                        None
                    }
                }
            } else {
                match tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task).await {
                    Ok(result) => Some(result),
                    Err(_) => {
                        let raw_error = "正常完成后代码审查后台任务未在清理窗口内结束";
                        log::warn!(
                            "codex review engine task cleanup timed out after completion report: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                            review_thread.engine_id,
                            review_thread.id,
                            source_engine_thread_id,
                        );
                        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                            "at": runtime_env::system_time_rfc3339(),
                            "event": "turn_exception_observed",
                            "consumer": "review",
                            "failure_kind": "engine_task_cleanup_timeout",
                            "engine_id": review_thread.engine_id.clone(),
                            "thread_id": review_thread.id.clone(),
                            "engine_thread_id": source_engine_thread_id.clone(),
                            "recoverable": false,
                            "raw_error": raw_error,
                        }))
                        .await;
                        emit_safe_review_notice!();
                        engine_task.abort();
                        let _ = (&mut engine_task).await;
                        None
                    }
                }
            };
            if let Some(engine_task_result) = engine_task_result {
                match engine_task_result {
                Ok(Ok(())) => {
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "engine_task_complete",
                        "consumer": "review",
                        "thread_id": review_thread.id.clone(),
                        "engine_id": review_thread.engine_id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "result": "ok",
                    }))
                    .await;
                }
                Ok(Err(error)) => {
                    let raw_error = error.to_string();
                    log::error!(
                        "codex review engine task returned error during finalization: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                        review_thread.engine_id,
                        review_thread.id,
                        source_engine_thread_id,
                    );
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "failure_kind": "engine_task",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                }
                Err(error) => {
                    let raw_error = error.to_string();
                    log::error!(
                        "codex review engine task join error during finalization: engine_id={}, thread_id={}, source_engine_thread_id={}, raw_error={raw_error}",
                        review_thread.engine_id,
                        review_thread.id,
                        source_engine_thread_id,
                    );
                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "review",
                        "failure_kind": "engine_task_join",
                        "engine_id": review_thread.engine_id.clone(),
                        "thread_id": review_thread.id.clone(),
                        "engine_thread_id": source_engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }))
                    .await;
                    emit_safe_review_notice!();
                }
            }
            }
        }

        let final_event = match end_signal {
            ReviewEndSignal::TaskCompletedAndDelivered => completed_turn_event.unwrap_or(
                EngineEvent::TurnCompleted {
                    token_usage: None,
                    status: TurnCompletionStatus::Completed,
                },
            ),
            ReviewEndSignal::UserClickedStop => EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Interrupted,
            },
        };
        let final_progress = process_stream_event(
            &app,
            &state,
            &review_thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &final_event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
            self.as_ref(),
        )
        .await;
        let final_force_persist = apply_stream_progress(
            final_progress,
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
            final_force_persist,
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
}

/// 前端线程更新事件沿用原有字段名和可选快照载荷。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadUpdatedEvent {
    /// 本地线程标识。
    thread_id: String,
    /// 线程所属工作区标识。
    workspace_id: String,
    /// 当前线程快照；部分事件只需要通知前端重新读取时可以省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<ThreadDto>,
}

/// Codex 远端线程归档或删除通知沿用原有数据契约。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRemoteThreadRemovedEvent {
    /// 被远端归档或删除的本地线程快照。
    thread: ThreadDto,
    /// 远端本次执行的动作名称。
    remote_action: &'static str,
}

/// 审批完成后用于事件通知的已提交业务结果。
#[derive(Debug, Clone)]
struct CodexRuntimeApprovalResolution {
    /// 审批所属本地线程标识。
    thread_id: String,
    /// 需要通知页面更新的线程快照；无活动本地轮次时为空。
    updated_thread: Option<ThreadDto>,
}

impl CodexCli {
    /// 持续处理 Codex 自身的运行时事件，单个事件失败时记录原始错误后继续监听。
    async fn listen_runtime_events(&self, app: tauri::AppHandle) -> Result<()> {
        let mut events = self.state.engines.subscribe_codex_runtime_events();
        let mut approval_events = crate::engines::codex::subscribe_codex_approval_events();
        loop {
            let received_event = tokio::select! {
                event = events.recv() => event,
                event = approval_events.recv() => event,
            };
            match received_event {
                Ok(event) => {
                    let event_context = format!("{event:?}");
                    if let Err(error) = self.handle_runtime_event(&app, event).await {
                        log::error!("处理 Codex 运行时事件失败: event={event_context} error={error:#}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Codex 运行时事件监听落后，跳过事件数: {skipped}");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }

    /// 将 Codex 运行时事件按既有前端事件契约更新本地状态并通知页面。
    async fn handle_runtime_event(
        &self,
        app: &tauri::AppHandle,
        event: CodexRuntimeEvent,
    ) -> Result<()> {
        match event {
            CodexRuntimeEvent::DiagnosticsUpdated { diagnostics, toast } => {
                app.emit(
                    "engine-runtime-updated",
                    EngineRuntimeUpdatedDto {
                        engine_id: "codex".to_string(),
                        protocol_diagnostics: Some(diagnostics),
                        toast,
                    },
                )
                .context("通知 Codex 运行诊断更新失败")?;
            }
            CodexRuntimeEvent::ApprovalResolved {
                engine_thread_id,
                request_id,
                source_connection_id,
            } => {
                // 按通知中的原始路由信息从数据库定位本地 UUID，不依赖引擎内存审批表。
                let database = self.state.db.clone();
                let lookup_engine_thread_id = engine_thread_id.clone();
                let lookup_request_id = request_id.clone();
                let lookup_connection_id = source_connection_id.clone();
                let record = tokio::task::spawn_blocking(move || {
                    db::actions::find_codex_approval_by_request(
                        &database,
                        &lookup_engine_thread_id,
                        &lookup_request_id,
                        &lookup_connection_id,
                    )
                })
                .await
                .context("按 Codex 原始审批请求查找数据库记录失败")??;
                let Some(record) = record else {
                    log::debug!(
                        "Codex resolved request has no saved approval: engine_thread_id={}, request_id={}, source_connection_id={}",
                        engine_thread_id,
                        request_id,
                        source_connection_id
                    );
                    return Ok(());
                };
                let approval_id = record.approval_id.clone();
                let Some(thread_id) = record.thread_id.clone() else {
                    log::error!(
                        "Codex resolved approval has no saved thread: approval_id={}, engine_thread_id={}, request_id={}, source_connection_id={}",
                        approval_id,
                        engine_thread_id,
                        request_id,
                        source_connection_id
                    );
                    return Ok(());
                };

                // pending 记录沿用既有原子结算；已经 answered 或 expired 的记录不再覆盖。
                let mut updated_thread = None;
                if record.status == "pending" {
                    if let Some(resolution) = self.resolve_runtime_approval(&approval_id).await? {
                        anyhow::ensure!(
                            resolution.thread_id == thread_id,
                            "Codex runtime approval thread does not match saved request: {approval_id}"
                        );
                        updated_thread = resolution.updated_thread;
                    }
                }

                // 以本地 UUID 重读真实数据库终态，兼容原生通知早于正常 Service 答复。
                let database = self.state.db.clone();
                let query_approval_id = approval_id.clone();
                let record = tokio::task::spawn_blocking(move || {
                    db::actions::read_runtime_approval_record(&database, &query_approval_id)
                })
                .await
                .context("重读 Codex 审批数据库终态失败")??;
                let Some(record) = record else {
                    log::debug!(
                        "Codex resolved approval disappeared before notification: approval_id={}, thread_id={}",
                        approval_id,
                        thread_id
                    );
                    return Ok(());
                };
                let payload = match record.status.as_str() {
                    "answered" => approval_resolved_payload(&approval_id),
                    "expired" => serde_json::json!({
                        "type": "ApprovalExpired",
                        "approval_id": approval_id,
                        "reason": "审批已失效",
                    }),
                    status => {
                        log::debug!(
                            "Codex resolved approval remains non-terminal: approval_id={}, thread_id={}, status={status}",
                            approval_id,
                            thread_id
                        );
                        return Ok(());
                    }
                };
                let topic = format!("stream-event-{thread_id}");
                app.emit(&topic, payload).with_context(|| {
                    format!(
                        "通知 Codex 审批状态失败: thread_id={thread_id}, approval_id={approval_id}"
                    )
                })?;
                if let Some(thread) = updated_thread {
                    Self::emit_thread_updated(app, thread)?;
                }
            }
            CodexRuntimeEvent::ThreadStatusChanged {
                engine_thread_id,
                status_type,
                active_flags,
            } => {
                if let Some(thread) = self
                    .apply_runtime_thread_update(
                        &engine_thread_id,
                        None,
                        Some(&status_type),
                        &active_flags,
                        None,
                        None,
                        None,
                    )
                    .await?
                {
                    Self::emit_thread_updated(app, thread)?;
                }
            }
            CodexRuntimeEvent::ThreadNameUpdated {
                engine_thread_id,
                thread_name,
            } => {
                let normalized_thread_name = thread_name.and_then(|name| {
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });
                let sync_required = Some(normalized_thread_name.is_none());
                let sync_reason = if normalized_thread_name.is_none() {
                    Some("thread_name_updated")
                } else {
                    None
                };
                if let Some(thread) = self
                    .apply_runtime_thread_update(
                        &engine_thread_id,
                        normalized_thread_name.as_deref(),
                        None,
                        &[],
                        None,
                        sync_required,
                        sync_reason,
                    )
                    .await?
                {
                    Self::emit_thread_updated(app, thread)?;
                }
            }
            CodexRuntimeEvent::ThreadSnapshotUpdated {
                engine_thread_id,
                thread_name,
                status_type,
                active_flags,
                preview,
            } => {
                if let Some(thread) = self
                    .apply_runtime_thread_update(
                        &engine_thread_id,
                        thread_name.as_deref(),
                        status_type.as_deref(),
                        &active_flags,
                        preview.as_deref(),
                        Some(false),
                        None,
                    )
                    .await?
                {
                    Self::emit_thread_updated(app, thread)?;
                }
            }
            CodexRuntimeEvent::ThreadArchived { engine_thread_id } => {
                self.emit_remote_thread_removed(app, &engine_thread_id, "archived")
                    .await?;
            }
            CodexRuntimeEvent::ThreadDeleted { engine_thread_id } => {
                self.emit_remote_thread_removed(app, &engine_thread_id, "deleted")
                    .await?;
            }
            CodexRuntimeEvent::ThreadUnarchived { engine_thread_id } => {
                if let Some(thread) = self.restore_runtime_thread(&engine_thread_id).await? {
                    Self::emit_thread_updated(app, thread)?;
                }
            }
        }
        Ok(())
    }

    /// 使用既有 thread-updated 事件通知前端完整线程快照。
    fn emit_thread_updated(app: &tauri::AppHandle, thread: ThreadDto) -> Result<()> {
        app.emit(
            "thread-updated",
            ThreadUpdatedEvent {
                thread_id: thread.id.clone(),
                workspace_id: thread.workspace_id.clone(),
                thread: Some(thread),
            },
        )
        .context("通知线程状态更新失败")
    }

    /// 按 Codex 远端线程 ID 合并状态、metadata 和标题的部分更新。
    async fn apply_runtime_thread_update(
        &self,
        engine_thread_id: &str,
        title: Option<&str>,
        raw_status: Option<&str>,
        active_flags: &[String],
        preview: Option<&str>,
        sync_required: Option<bool>,
        sync_reason: Option<&str>,
    ) -> Result<Option<ThreadDto>> {
        let database = self.state.db.clone();
        let lookup_engine_thread_id = engine_thread_id.to_string();
        let thread = tokio::task::spawn_blocking(move || {
            db::threads::find_thread_by_engine_thread_id(
                &database,
                "codex",
                &lookup_engine_thread_id,
            )
        })
        .await
        .context("查询 Codex 运行时线程的数据库任务失败")??;
        let Some(thread) = thread else {
            return Ok(None);
        };

        let has_local_turn = self.state.turns.get(&thread.id).await.is_some();
        let next_status = thread_business::map_codex_thread_status_to_local(
            raw_status,
            active_flags,
            has_local_turn,
        );
        let metadata = merge_codex_runtime_event_metadata(
            thread.engine_metadata.clone(),
            raw_status,
            active_flags,
            preview,
            sync_required,
            sync_reason,
        );
        let database = self.state.db.clone();
        let thread_id = thread.id.clone();
        let title = title.map(str::to_string);
        let result = tokio::task::spawn_blocking(move || {
            db::threads::update_thread_runtime_snapshot(
                &database,
                &thread_id,
                title.as_deref(),
                next_status,
                Some(&metadata),
            )
        })
        .await
        .context("保存 Codex 运行时线程更新的数据库任务失败")??;
        Ok(Some(result))
    }

    /// 查找仍处于当前工作区活动列表中的 Codex 本地线程。
    async fn find_active_runtime_thread(
        &self,
        engine_thread_id: &str,
    ) -> Result<Option<ThreadDto>> {
        let database = self.state.db.clone();
        let engine_thread_id = engine_thread_id.to_string();
        tokio::task::spawn_blocking(move || {
            let Some(thread) = db::threads::find_thread_by_engine_thread_id(
                &database,
                "codex",
                &engine_thread_id,
            )? else {
                return Ok(None);
            };
            let is_active = db::threads::list_threads_for_workspace(
                &database,
                &thread.workspace_id,
            )?
            .into_iter()
            .any(|candidate| candidate.id == thread.id);
            Ok(is_active.then_some(thread))
        })
        .await
        .context("查询活动 Codex 线程的数据库任务失败")?
    }

    /// 将远端归档或删除事件通知给仍然活动的本地线程。
    async fn emit_remote_thread_removed(
        &self,
        app: &tauri::AppHandle,
        engine_thread_id: &str,
        remote_action: &'static str,
    ) -> Result<()> {
        let Some(thread) = self.find_active_runtime_thread(engine_thread_id).await? else {
            return Ok(());
        };
        app.emit(
            "codex-remote-thread-removed",
            CodexRemoteThreadRemovedEvent {
                thread,
                remote_action,
            },
        )
        .context("通知 Codex 远端线程归档或删除失败")
    }

    /// 按 Mapper 结构化结果恢复远端取消归档的本地 Codex 线程。
    async fn restore_runtime_thread(
        &self,
        engine_thread_id: &str,
    ) -> Result<Option<ThreadDto>> {
        let database = self.state.db.clone();
        let engine_thread_id = engine_thread_id.to_string();
        let restored = tokio::task::spawn_blocking(move || {
            db::threads::restore_runtime_thread(&database, "codex", &engine_thread_id)
        })
        .await
        .context("恢复 Codex 运行时线程的数据库任务失败")??;
        match restored {
            db::threads::RuntimeThreadRestoreResult::Updated(thread)
            | db::threads::RuntimeThreadRestoreResult::AlreadyActive(thread) => Ok(Some(thread)),
            db::threads::RuntimeThreadRestoreResult::NotFound => Ok(None),
        }
    }

    /// 在审批和消息块仍匹配当前 pending 事实时原子保存 Codex 审批完成结果。
    async fn resolve_runtime_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<CodexRuntimeApprovalResolution>> {
        let database = self.state.db.clone();
        let query_approval_id = approval_id.to_string();
        let initial = tokio::task::spawn_blocking(move || {
            db::actions::read_runtime_approval_record(&database, &query_approval_id)
        })
        .await
        .context("读取 Codex 审批上下文的数据库任务失败")??;
        let Some(initial) = initial else {
            return Ok(None);
        };
        if initial.status != "pending" || initial.engine_id.as_deref() != Some("codex") {
            return Ok(None);
        }
        let thread_id = initial
            .thread_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Codex runtime approval has no thread id: {approval_id}"))?;
        let has_local_turn = self.state.turns.get(&thread_id).await.is_some();
        let database = self.state.db.clone();
        let query_approval_id = approval_id.to_string();
        let transaction_result = tokio::task::spawn_blocking(move || {
            db::actions::with_runtime_approval_transaction(&database, |tx| {
                let Some(current) =
                    db::actions::get_runtime_approval_record(tx, &query_approval_id)?
                else {
                    return Ok(None);
                };
                if current.status != "pending" || current.engine_id.as_deref() != Some("codex") {
                    return Ok(None);
                }
                let thread_id = current.thread_id.ok_or_else(|| {
                    anyhow::anyhow!("Codex runtime approval has no thread id: {query_approval_id}")
                })?;
                let message_id = current.message_id.ok_or_else(|| {
                    anyhow::anyhow!("Codex runtime approval has no message id: {query_approval_id}")
                })?;
                if current.message_thread_id.as_deref() != Some(thread_id.as_str()) {
                    anyhow::bail!("Codex runtime approval message belongs to another thread: {query_approval_id}");
                }
                let raw_blocks = current.blocks_json.ok_or_else(|| {
                    anyhow::anyhow!("Codex runtime approval message has no blocks JSON: {message_id}")
                })?;
                let mut blocks: serde_json::Value = serde_json::from_str(&raw_blocks)
                    .with_context(|| format!("failed to parse Codex runtime approval blocks: {message_id}"))?;
                let items = blocks.as_array_mut().ok_or_else(|| {
                    anyhow::anyhow!("Codex runtime approval blocks are not an array: {message_id}")
                })?;
                let mut matched_block = false;
                for block in items {
                    let Some(object) = block.as_object_mut() else {
                        continue;
                    };
                    if object.get("type").and_then(serde_json::Value::as_str) != Some("approval") {
                        continue;
                    }
                    let block_approval_id = object
                        .get("approvalId")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| object.get("approval_id").and_then(serde_json::Value::as_str));
                    if block_approval_id != Some(query_approval_id.as_str()) {
                        continue;
                    }
                    object.insert(
                        "status".to_string(),
                        serde_json::Value::String("answered".to_string()),
                    );
                    matched_block = true;
                }
                if !matched_block {
                    anyhow::bail!("Codex runtime approval block was not found: {query_approval_id}");
                }
                let answered_at = runtime_env::system_time_rfc3339();
                let blocks_json = blocks.to_string();
                let message_rows = db::actions::update_runtime_approval_message_blocks(
                    tx,
                    &message_id,
                    &thread_id,
                    &raw_blocks,
                    &blocks_json,
                )?;
                if message_rows == 0 {
                    anyhow::bail!("Codex runtime approval message changed before update: {message_id}");
                }
                let approval_rows = db::actions::update_runtime_approval_pending(
                    tx,
                    &query_approval_id,
                    &thread_id,
                    "answered",
                    &answered_at,
                )?;
                if approval_rows == 0 {
                    anyhow::bail!("Codex runtime approval changed before update: {query_approval_id}");
                }
                let mut updated_thread = None;
                if has_local_turn
                    && current.thread_status.as_deref()
                        == Some(ThreadStatusDto::AwaitingApproval.as_str())
                {
                    let thread_rows = db::actions::update_runtime_thread_status_if_current(
                        tx,
                        &thread_id,
                        ThreadStatusDto::AwaitingApproval.as_str(),
                        ThreadStatusDto::Streaming.as_str(),
                        &answered_at,
                    )?;
                    if thread_rows > 0 {
                        updated_thread = Some(
                            db::actions::get_runtime_approval_thread(tx, &thread_id)?.ok_or_else(
                                || anyhow::anyhow!("thread missing during approval resolution: {thread_id}"),
                            )?,
                        );
                    }
                }
                Ok(Some((thread_id, updated_thread)))
            })
        })
        .await
        .context("提交 Codex 审批完成事务的数据库任务失败")??;
        let Some((thread_id, updated_thread)) = transaction_result else {
            return Ok(None);
        };
        Ok(Some(CodexRuntimeApprovalResolution {
            thread_id,
            updated_thread,
        }))
    }
}

/// 合并运行时事件提供的部分 Codex metadata，未提供字段继续保留旧值。
fn merge_codex_runtime_event_metadata(
    existing: Option<serde_json::Value>,
    raw_status: Option<&str>,
    active_flags: &[String],
    preview: Option<&str>,
    sync_required: Option<bool>,
    sync_reason: Option<&str>,
) -> serde_json::Value {
    let mut metadata = existing.unwrap_or_else(|| serde_json::json!({}));
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        if raw_status.is_some() {
            match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
                Some(status) => {
                    object.insert("codexThreadStatus".to_string(), serde_json::json!(status));
                }
                None => {
                    object.remove("codexThreadStatus");
                }
            }
            if active_flags.is_empty() {
                object.remove("codexThreadActiveFlags");
            } else {
                object.insert(
                    "codexThreadActiveFlags".to_string(),
                    serde_json::json!(active_flags),
                );
            }
        }
        if preview.is_some() {
            match preview.map(str::trim).filter(|value| !value.is_empty()) {
                Some(preview) => {
                    object.insert("codexPreview".to_string(), serde_json::json!(preview));
                }
                None => {
                    object.remove("codexPreview");
                }
            }
        }
        if let Some(sync_required) = sync_required {
            object.insert(
                "codexSyncRequired".to_string(),
                serde_json::json!(sync_required),
            );
            object.insert(
                "codexSyncUpdatedAt".to_string(),
                serde_json::json!(runtime_env::system_time_rfc3339()),
            );
            match sync_reason.map(str::trim).filter(|value| !value.is_empty()) {
                Some(reason) => {
                    object.insert("codexSyncReason".to_string(), serde_json::json!(reason));
                }
                None => {
                    object.insert("codexSyncReason".to_string(), serde_json::Value::Null);
                }
            }
        }
    }
    metadata
}

/// 构造沿用原 snake_case 字段契约的审批完成通知载荷。
fn approval_resolved_payload(approval_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "ApprovalResolved",
        "approval_id": approval_id,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/codex_runtime_business_tests.rs"]
mod runtime_business_tests;

#[async_trait]
impl CliTool for CodexCli {
    async fn list_opencode_thread_records(&self, _workspace_id: String, _cursor: Option<String>, _limit: Option<u32>, _search_term: Option<String>, _archived: Option<bool>) -> Result<crate::models::OpenCodeRemoteSessionPageDto> {
        anyhow::bail!("codex 不支持此线程业务操作：list_opencode_thread_records")
    }

    async fn attach_opencode_thread_record(&self, _workspace_id: String, _engine_thread_id: String, _cwd: String, _model_id: String) -> Result<ThreadDto> {
        anyhow::bail!("codex 不支持此线程业务操作：attach_opencode_thread_record")
    }

    async fn update_opencode_thread_config(&self, _thread_id: String, _update_agent: bool, _agent: Option<String>) -> Result<ThreadDto> {
        anyhow::bail!("codex 不支持此线程业务操作：update_opencode_thread_config")
    }


    async fn list_codex_thread_records(&self, workspace_id: String, cursor: Option<String>, limit: Option<u32>, search_term: Option<String>, archived: Option<bool>) -> Result<crate::models::CodexRemoteThreadPageDto> {
        self.thread_list_records(workspace_id, cursor, limit, search_term, archived).await.map_err(anyhow::Error::msg)
    }

    async fn attach_thread_record(&self, workspace_id: String, engine_thread_id: String) -> Result<ThreadDto> {
        self.thread_attach_record(workspace_id, engine_thread_id).await.map_err(anyhow::Error::msg)
    }

    async fn fork_thread_record(&self, thread_id: String) -> Result<ThreadDto> {
        self.thread_fork_record(thread_id).await.map_err(anyhow::Error::msg)
    }

    async fn rollback_thread_record(&self, thread_id: String, num_turns: u32) -> Result<ThreadDto> {
        self.thread_rollback_record(thread_id, num_turns).await.map_err(anyhow::Error::msg)
    }

    async fn compact_thread_record(&self, app: tauri::AppHandle, thread_id: String) -> Result<ThreadDto> {
        self.thread_compact_record(app, thread_id).await.map_err(anyhow::Error::msg)
    }

    async fn update_codex_thread_config(&self, thread_id: String, update_personality: bool, personality: Option<String>, update_service_tier: bool, service_tier: Option<String>, update_output_schema: bool, output_schema: Option<Value>) -> Result<ThreadDto> {
        self.thread_update_config(thread_id, update_personality, personality, update_service_tier, service_tier, update_output_schema, output_schema).await.map_err(anyhow::Error::msg)
    }

    fn normalize_thread_service_tier(&self, value: Option<String>) -> Result<Option<String>> { thread_business::normalize_thread_service_tier(value).map_err(anyhow::Error::msg) }


    async fn create_thread_record(&self, workspace_id: String, model_id: String, title: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> Result<ThreadDto> {
        self.mcp.create_thread_record(self, "codex", workspace_id, model_id, title, reasoning_effort, service_tier).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=create_thread_record error={error:#}"))
    }

    async fn reconfigure_thread_record(&self, thread_id: String, model_id: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> Result<ThreadDto> {
        self.mcp.reconfigure_thread_record(self, "codex", thread_id, model_id, reasoning_effort, service_tier).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=reconfigure_thread_record error={error:#}"))
    }

    async fn set_thread_reasoning(&self, thread_id: String, reasoning_effort: Option<String>, model_id: Option<String>) -> Result<()> {
        self.mcp.set_thread_reasoning(self, "codex", thread_id, reasoning_effort, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=set_thread_reasoning error={error:#}"))
    }

    async fn set_thread_selected_model(&self, thread_id: String, model_id: String) -> Result<ThreadDto> {
        self.mcp.set_thread_selected_model(self, "codex", thread_id, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=set_thread_selected_model error={error:#}"))
    }

    async fn delete_thread_record(&self, thread_id: String) -> Result<()> {
        self.mcp.delete_thread_record(self, "codex", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=delete_thread_record error={error:#}"))
    }

    async fn archive_thread_record(&self, thread_id: String) -> Result<()> {
        self.mcp.archive_thread_record(self, "codex", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=archive_thread_record error={error:#}"))
    }

    async fn restore_thread_record(&self, thread_id: String) -> Result<ThreadDto> {
        self.mcp.restore_thread_record(self, "codex", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=restore_thread_record error={error:#}"))
    }

    async fn validate_thread_model(&self, workspace_id: Option<&str>, model_id: &str) -> Result<String> {
        self.mcp.validate_thread_model(self, "codex", workspace_id, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=validate_thread_model error={error:#}"))
    }

    async fn validate_thread_reasoning(&self, model_id: &str, reasoning_effort: &str) -> Result<String> {
        self.mcp.validate_thread_reasoning(self, "codex", model_id, reasoning_effort).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=codex operation=validate_thread_reasoning error={error:#}"))
    }

    /// 校验并规范化 Codex 定时任务运行配置及其专属服务档位。
    async fn prepare_scheduled_runtime_config(
        &self,
        runtime: crate::models::ScheduledRuntimeConfig,
    ) -> Result<crate::models::ScheduledRuntimeConfig> {
        let result: Result<crate::models::ScheduledRuntimeConfig> = async {
            // 使用公共流程校验当前 Codex 模型和推理强度。
            let mut runtime = self
                .mcp
                .prepare_scheduled_model_config(self, runtime)
                .await?;

            // 按定时任务原接受规则规范化 Codex 专属服务档位。
            runtime.service_tier =
                thread_business::normalize_scheduled_codex_service_tier(runtime.service_tier)?;
            Ok(runtime)
        }
        .await;
        result
            .context("校验 Codex 定时任务运行配置失败")
            .inspect_err(|error| {
                log::error!("CLI 定时任务运行配置校验失败: cli_id=codex operation=prepare_scheduled_runtime_config error={error:#}")
            })
    }

    async fn sync_thread_record(&self, thread_id: String) -> Result<ThreadDto> {
        self.thread_sync_record(thread_id).await.map_err(anyhow::Error::msg)
    }

    /// 将统一聊天消息提交到当前 Codex CLI，并沿用本轮占用的实现实例。
    async fn submit_chat_message(
        self: Arc<Self>,
        app: tauri::AppHandle,
        request: crate::models::chat::SendMessageRequest,
    ) -> std::result::Result<String, ChatBusinessError> {
        // 将当前 Codex 实例传给共享提交流程，保持整轮生命周期实例一致。
        let cli: Arc<dyn CliTool> = self.clone();
        self.mcp.submit_chat_message(cli, app, request).await
    }

    /// 准备并提交活动 Codex 轮次的追加消息。
    async fn submit_chat_steer(
        self: Arc<Self>,
        app: tauri::AppHandle,
        request: SteerMessageRequest,
    ) -> std::result::Result<crate::models::SteerReceiptDto, ChatBusinessError> {
        // 由 BaseCliMcp 完成共享准备，并通过同一个 CLI 实例调用原生 steer。
        let cli: Arc<dyn CliTool> = self.clone();
        self.mcp.submit_chat_steer(cli, app, request).await
    }

    /// 校验并发送当前 Codex 线程的审批答复。
    async fn answer_chat_approval(
        self: Arc<Self>,
        request: ApprovalResponseRequest,
    ) -> std::result::Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
        // 公共流程通过此实例的原生响应路径完成答复和本地状态结算。
        let cli: Arc<dyn CliTool> = self.clone();
        self.mcp.answer_chat_approval(cli, request).await
    }

    /// 取消指定 Codex 线程当前运行的聊天轮次。
    async fn cancel_chat_turn(
        &self,
        thread_id: &str,
    ) -> std::result::Result<(), ChatBusinessError> {
        // 公共取消流程依据当前线程 CancellationToken 决定是否中断。
        self.mcp.cancel_chat_turn(thread_id).await
    }

    /// 重启指定 Codex 线程所使用的聊天服务。
    async fn restart_chat_service(
        &self,
        thread_id: &str,
    ) -> std::result::Result<(), ChatBusinessError> {
        // 公共重启流程只接收当前 Codex 实现，不旁路重建其他 CLI。
        self.mcp.restart_chat_service(self, thread_id).await
    }

    /// 创建并启动 Codex 原生代码审查轮次。
    async fn submit_chat_review(
        self: Arc<Self>,
        app: tauri::AppHandle,
        request: ChatReviewRequest,
    ) -> std::result::Result<ThreadDto, ChatBusinessError> {
        CodexCli::start_codex_review(self, app, request).await
    }

    /// 为公共聊天流程提供 Codex metadata 和本机沙箱策略。
    fn chat_sandbox(
        &self,
        thread: &ThreadDto,
        writable_roots: Vec<String>,
        reasoning_effort: Option<String>,
        model_supports_personality: bool,
    ) -> SandboxPolicy {
        // Codex 专属模型 metadata 在实现类解释，统一权限占位值沿用既有默认值。
        SandboxPolicy {
            writable_roots,
            allow_network: false,
            auto_approve_mcp_elicitations: false,
            approval_policy: None,
            permission_profile: None,
            approvals_reviewer: None,
            reasoning_effort,
            sandbox_mode: None,
            service_tier: thread_business::thread_service_tier(
                thread.engine_metadata.as_ref(),
            ),
            personality: if model_supports_personality {
                thread_business::thread_personality(thread.engine_metadata.as_ref())
            } else {
                None
            },
            output_schema: thread_business::thread_output_schema(
                thread.engine_metadata.as_ref(),
            ),
            opencode_agent: None,
        }
    }

    /// 将用户审批答复按 Codex 原生协议归一化。
    fn normalize_chat_approval(
        &self,
        response: Value,
    ) -> std::result::Result<Value, ChatBusinessError> {
        // 保留引擎统一归一化规则，并将协议校验失败映射为中文业务错误。
        crate::engines::normalize_approval_response_for_engine("codex", response).map_err(
            |error| {
                log::error!("Codex 审批答复格式无效，原始错误：{error}");
                ChatBusinessError::new("VALIDATION_FAILED", "审批答复格式不正确")
            },
        )
    }

    /// 从审批详情读取 Codex 原生答复路由。
    fn chat_approval_route(
        &self,
        details: &Value,
    ) -> std::result::Result<Option<ApprovalRequestRoute>, ChatBusinessError> {
        // 路由结构由引擎协议层解释，Codex 实现只固定当前引擎标识。
        Ok(crate::engines::approval_response_route_for_engine(
            "codex", details,
        ))
    }

    /// 将 Codex 审批发送失败映射到结构化业务错误。
    fn classify_chat_approval_error(&self, error: &anyhow::Error) -> ChatBusinessError {
        // 仅根据保留的原生错误类型分类，不读取底层错误文案。
        log::error!("Codex 审批答复失败，原始错误：{error:#}");
        if matches!(
            error.downcast_ref::<crate::engines::codex::ApprovalResponseTargetError>(),
            Some(
                crate::engines::codex::ApprovalResponseTargetError::RuntimeReset
                    | crate::engines::codex::ApprovalResponseTargetError::MissingRequestMetadata
            )
        ) {
            return ChatBusinessError::new(
                "APPROVAL_NOT_PENDING",
                "该审批已失效，请刷新后重试",
            );
        }

        ChatBusinessError::new("CLI_UNAVAILABLE", "Codex 当前不可用，审批答复未完成")
    }

    /// 持久化 Codex 上下文用量事件并允许公共事件处理继续执行。
    async fn on_chat_event(
        &self,
        thread: &ThreadDto,
        assistant_message_id: &str,
        event: &EngineEvent,
    ) -> anyhow::Result<()> {
        // 仅处理 Codex 上下文用量事件，持久化异常记录日志但不阻断后续公共事件。
        if thread.engine_id != "codex" {
            return Ok(());
        }
        let EngineEvent::UsageLimitsUpdated { usage } = event else {
            return Ok(());
        };

        let mut conversion_failed = false;
        let context_current_tokens = match usage.current_tokens {
            Some(value) => match i64::try_from(value) {
                Ok(value) => Some(value),
                Err(error) => {
                    conversion_failed = true;
                    log::error!(
                        "Codex thread context usage persistence conversion failed: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} field=current_tokens value={} error={error:?}",
                        thread.id,
                        thread.engine_thread_id,
                        thread.workspace_id,
                        assistant_message_id,
                        value
                    );
                    None
                }
            },
            None => None,
        };
        let context_max_tokens = match usage.max_context_tokens {
            Some(value) => match i64::try_from(value) {
                Ok(value) => Some(value),
                Err(error) => {
                    conversion_failed = true;
                    log::error!(
                        "Codex thread context usage persistence conversion failed: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} field=max_context_tokens value={} error={error:?}",
                        thread.id,
                        thread.engine_thread_id,
                        thread.workspace_id,
                        assistant_message_id,
                        value
                    );
                    None
                }
            },
            None => None,
        };

        // 保留原空字段规则，转换失败或没有上下文字段时不写入线程快照。
        if conversion_failed {
            log::error!(
                "Codex thread context usage persistence skipped after conversion failure: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} usage={usage:?}",
                thread.id,
                thread.engine_thread_id,
                thread.workspace_id,
                assistant_message_id
            );
            return Ok(());
        }
        if context_current_tokens.is_none() && context_max_tokens.is_none() {
            log::info!(
                "Codex thread context usage persistence skipped: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} reason=no_context_fields usage={usage:?}",
                thread.id,
                thread.engine_thread_id,
                thread.workspace_id,
                assistant_message_id
            );
            return Ok(());
        }

        // 保存 Codex 上下文用量和更新时间；数据库失败记录原始错误并放行公共事件。
        let context_usage_updated_at = runtime_env::system_time_rfc3339();
        let update = ThreadUpdateDto {
            id: thread.id.clone(),
            context_current_tokens,
            context_max_tokens,
            context_usage_updated_at: Some(context_usage_updated_at.clone()),
            ..Default::default()
        };
        let thread_id = thread.id.clone();
        let engine_thread_id = thread.engine_thread_id.clone();
        let workspace_id = thread.workspace_id.clone();
        let assistant_message_id = assistant_message_id.to_string();
        let db = self.state.db.clone();
        let current_tokens_for_log = update.context_current_tokens;
        let max_context_tokens_for_log = update.context_max_tokens;
        match tokio::task::spawn_blocking(move || db::threads::update_thread(&db, &update)).await {
            Ok(Ok(updated_thread)) => {
                log::info!(
                    "Codex thread context usage persistence succeeded: thread_id={} updated_thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} current_tokens={:?} max_context_tokens={:?} updated_at={}",
                    thread_id,
                    updated_thread.id,
                    engine_thread_id,
                    workspace_id,
                    assistant_message_id,
                    current_tokens_for_log,
                    max_context_tokens_for_log,
                    context_usage_updated_at
                );
            }
            Ok(Err(error)) => {
                log::error!(
                    "Codex thread context usage persistence failed: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} current_tokens={:?} max_context_tokens={:?} updated_at={} error={error}",
                    thread_id,
                    engine_thread_id,
                    workspace_id,
                    assistant_message_id,
                    current_tokens_for_log,
                    max_context_tokens_for_log,
                    context_usage_updated_at
                );
            }
            Err(error) => {
                log::error!(
                    "Codex thread context usage persistence task failed: thread_id={} engine_thread_id={:?} workspace_id={} assistant_message_id={} current_tokens={:?} max_context_tokens={:?} updated_at={} error={error}",
                    thread_id,
                    engine_thread_id,
                    workspace_id,
                    assistant_message_id,
                    current_tokens_for_log,
                    max_context_tokens_for_log,
                    context_usage_updated_at
                );
            }
        }
        Ok(())
    }

    fn id(&self) -> &str {
        "codex"
    }

    fn name(&self) -> &str {
        "Codex"
    }

    fn capabilities(&self) -> EngineCapabilities {
        capabilities_for_engine("codex")
    }

    /// 返回 Codex 当前运行位置可用的 MCP 工具目录。
    fn list_mcp_tools(&self) -> Result<Vec<Value>, String> {
        self.mcp.list_mcp_tools()
    }

    /// 将 Codex MCP 调用委托给公共 BaseCliMcp 实现。
    async fn call_mcp_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        context: McpInvocationContext,
        call_id: String,
        cancellation: CancellationToken,
    ) -> McpToolResult {
        self.mcp
            .call_mcp_tool(tool_name, arguments, context, call_id, cancellation)
            .await
    }

    /// 为当前 Codex 对话轮次登记可信 AuraCoder MCP 上下文，并按项目位置调用对应生命周期。
    async fn register_mcp_context(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        match context.location_kind {
            CliLocationKind::Local => {
                LocalCliServiceLifecycle::register_mcp_context(self.id(), engine_thread_id, turn_id)
                    .await
            }
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端项目未绑定连接")?;
                ssh::cli_service_lifecycle::register_mcp_context(
                    connection_id,
                    self.id(),
                    engine_thread_id,
                    turn_id,
                )
                .await
            }
        }
    }

    /// 清理当前 Codex 对话轮次的可信 AuraCoder MCP 上下文，并保留生命周期原始错误链。
    async fn clear_mcp_context(&self, context: &CliExecutionContext) -> Result<()> {
        match context.location_kind {
            CliLocationKind::Local => LocalCliServiceLifecycle::clear_mcp_context(self.id()).await,
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端项目未绑定连接")?;
                ssh::cli_service_lifecycle::clear_mcp_context(connection_id, self.id()).await
            }
        }
    }

    /// 重启当前 Codex 的 SSH 远端 CLI 服务，严格按 terminate 后 set 的顺序执行。
    async fn restart_service(&self, context: &CliExecutionContext) -> Result<()> {
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Ssh,
            "不支持重启本机 CLI 服务"
        );
        let connection_id = context
            .ssh_connection_id
            .as_deref()
            .context("SSH 远端项目未绑定连接")?;
        ssh::cli_service_lifecycle::terminate(connection_id, self.id()).await?;
        ssh::cli_service_lifecycle::set(connection_id, self.id()).await?;
        Ok(())
    }

    /// Codex 不 spawn 独立 serve 进程，空操作。
    async fn close_workspace_service(&self, _context: &CliExecutionContext) -> Result<()> {
        Ok(())
    }

    /// 查询当前 Codex 服务是否已经由本机或 SSH CLI 生命周期登记并处于 Ready 状态。
    async fn is_service_ready(&self, context: &CliExecutionContext) -> Result<bool> {
        match context.location_kind {
            CliLocationKind::Local => Ok(LocalCliServiceLifecycle::get(self.id()).await.is_ok()),
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端 Codex 项目未绑定连接")?;
                Ok(ssh::cli_service_lifecycle::get(connection_id, self.id())
                    .await
                    .is_ok())
            }
        }
    }

    /// 通过 Codex 对应 CLI 生命周期取得或确保当前运行位置的服务，不直接管理 SSH Tunnel。
    async fn ensure_service(&self, context: &CliExecutionContext) -> Result<()> {
        match context.location_kind {
            CliLocationKind::Local => {
                LocalCliServiceLifecycle::set(self.id()).await?;
            }
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端 Codex 项目未绑定连接")?;
                ssh::cli_service_lifecycle::set(connection_id, self.id()).await?;
            }
        }
        Ok(())
    }

    /// SSH 连接测试成功后，由 Codex CLI 生命周期建立 Tunnel 并登记、启动远端服务。
    async fn register_remote_service(&self, context: &CliExecutionContext) -> Result<()> {
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Ssh,
            "本机 Codex CLI 不支持注册远端服务"
        );
        let connection_id = context
            .ssh_connection_id
            .as_deref()
            .context("SSH 远端 Codex 注册服务未绑定连接")?;
        let lookup_connection_id = connection_id.to_string();
        let record = tokio::task::spawn_blocking({
            let db = self.state.db.clone();
            move || db::ssh_connections::find(&db, &lookup_connection_id)
        })
        .await
        .context("读取 SSH Codex 连接记录任务失败")?
        .context("读取 SSH Codex 连接记录数据库失败")?
        .with_context(|| format!("SSH Codex 连接记录不存在: connection_id={connection_id}"))?;
        ssh::cli_service_lifecycle::register_service(&record, self.id()).await
    }

    /// 通过 SSH CLI 生命周期注销当前连接的 Codex 服务，不直接管理隧道资源。
    async fn unregister_remote_service(&self, context: &CliExecutionContext) -> Result<()> {
        // 仅 SSH 项目允许显式注销远端 CLI 服务，并要求调用方提供对应连接标识。
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Ssh,
            "本机 Codex CLI 不支持注销远端服务"
        );
        let connection_id = context
            .ssh_connection_id
            .as_deref()
            .context("SSH 远端 Codex 注销服务未绑定连接")?;

        ssh::cli_service_lifecycle::unregister_service(connection_id, self.id()).await
    }

    async fn execution_context(&self, workspace_id: Option<&str>) -> Result<CliExecutionContext> {
        CodexCli::execution_context(self, workspace_id).await
    }

    async fn execution_context_for_cwd(&self, cwd: Option<&str>) -> Result<CliExecutionContext> {
        CodexCli::execution_context_for_cwd(self, cwd).await
    }

    async fn get_engine_info(&self, context: &CliExecutionContext) -> Result<EngineInfoDto> {
        /*
        旧实现先通过 workspace 构造远端运行对象，再读取模型。模型目录属于机器，不再执行：
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            let models = engine.list_models_runtime().await;
            return Ok(EngineInfoDto {
                id: "codex".to_string(),
                name: "Codex".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("codex")),
            });
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 Codex 未绑定连接")?;
            let models =
                remote_project_codex_runtime_service::model_infos(connection_id, None).await?;
            return Ok(EngineInfoDto {
                id: "codex".to_string(),
                name: "Codex".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("codex")),
            });
        }

        let engine = self.local_engine().await?;
        let models = engine.list_models_runtime().await;
        Ok(EngineInfoDto {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            models: models.into_iter().map(map_model_info).collect(),
            capabilities: map_engine_capabilities(capabilities_for_engine("codex")),
        })
    }

    async fn models_for_validation(
        &self,
        context: &CliExecutionContext,
        requested_model_id: &str,
    ) -> Result<Vec<ModelInfo>> {
        /*
        旧实现通过 workspace 取得远端模型目录，不再执行：
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return Ok(remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime()
                .await);
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 Codex 未绑定连接")?;
            return remote_project_codex_runtime_service::model_infos(connection_id, None).await;
        }

        let engine = self.local_engine().await?;
        let cached_models = engine.runtime_model_fallback().await;
        if cached_models
            .iter()
            .any(|model| model.id == requested_model_id)
        {
            return Ok(cached_models);
        }
        Ok(engine.list_models_runtime().await)
    }

    /// 用户查看 Codex 用量时，读取当前本机或 SSH 运行目标上的额度。
    async fn get_chat_provider_usage(
        &self,
        context: &CliExecutionContext,
        _scope: CliUsageQueryScope,
    ) -> Result<Option<ChatProviderUsageDto>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let result = remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .usage_limits_snapshot()
                .await;
            return Ok(Some(map_provider_usage("codex", "Codex", result)));
        }

        let engine = self.local_engine().await?;
        Ok(Some(map_provider_usage(
            "codex",
            "Codex",
            engine.usage_limits_snapshot().await,
        )))
    }

    /// 用户进入 Codex 线程时，直接读取线程记录中的持久化上下文 token 快照。
    async fn get_context_usage(
        &self,
        thread_id: &str,
    ) -> Result<Option<CliContextUsageDto>> {
        // 按数据库线程归属读取执行上下文，再沿用 Codex 持久化 token 快照映射。
        let (context, thread) = self
            .mcp
            .context_for_thread(self.id(), thread_id)
            .await
            .inspect_err(|error| {
                log::error!(
                    "读取 Codex 线程上下文失败: cli_id={} thread_id={} error={error:#}",
                    self.id(),
                    thread_id
                )
            })
            .context("读取 Codex 线程上下文失败")?;
        let context = &context;
        let thread = &thread;

        log::info!(
            "Codex context usage service entered: stage=service_entry thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} root_path={:?} ssh_connection_id={:?}",
            thread.id,
            thread.engine_thread_id,
            context.workspace_id,
            context.location_kind,
            context.root_path,
            context.ssh_connection_id
        );

        log::info!(
            "Codex context usage thread snapshot read start: stage=thread_snapshot_read_start thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} root_path={:?}",
            thread.id,
            thread.engine_thread_id,
            context.workspace_id,
            context.location_kind,
            context.root_path
        );
        let current_tokens = match thread.context_current_tokens {
            Some(value) => match u64::try_from(value) {
                Ok(value) => Some(value),
                Err(error) => {
                    log::error!(
                        "Codex context usage current token conversion failed: stage=thread_snapshot_conversion thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} field=context_current_tokens value={} error={error:?}",
                        thread.id,
                        thread.engine_thread_id,
                        context.workspace_id,
                        context.location_kind,
                        value
                    );
                    return Err(error.into());
                }
            },
            None => None,
        };
        let max_context_tokens = match thread.context_max_tokens {
            Some(value) => match u64::try_from(value) {
                Ok(value) => Some(value),
                Err(error) => {
                    log::error!(
                        "Codex context usage max token conversion failed: stage=thread_snapshot_conversion thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} field=context_max_tokens value={} error={error:?}",
                        thread.id,
                        thread.engine_thread_id,
                        context.workspace_id,
                        context.location_kind,
                        value
                    );
                    return Err(error.into());
                }
            },
            None => None,
        };
        let snapshot = (current_tokens, max_context_tokens);
        log::info!(
            "Codex context usage thread snapshot read succeeded: stage=thread_snapshot_read thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} snapshot={snapshot:?} is_empty={}",
            thread.id,
            thread.engine_thread_id,
            context.workspace_id,
            context.location_kind,
            snapshot.0.is_none() && snapshot.1.is_none()
        );

        let mapped = map_context_usage(snapshot.0, snapshot.1);
        log::info!(
            "Codex context usage mapping completed: stage=dto_mapping thread_id={} engine_thread_id={:?} workspace_id={} location_kind={:?} snapshot={snapshot:?} result={mapped:?} is_none={}",
            thread.id,
            thread.engine_thread_id,
            context.workspace_id,
            context.location_kind,
            mapped.is_none()
        );
        Ok(mapped)
    }

    /// 查询已登记 Codex 在当前运行位置的就绪状态和模型目录。
    async fn get_active_engine_info(
        &self,
        context: &CliExecutionContext,
    ) -> Result<Option<EngineInfoDto>> {
        self.mcp
            .active_engine_info(self, context)
            .await
            .inspect_err(|error| {
                log::error!(
                    "读取 Codex 活动模型目录失败: cli_id={} error={error:#}",
                    self.id()
                )
            })
    }

    /// 按 Codex 当前模型目录为尚未开始的线程准备运行时选择。
    async fn prepare_unstarted_thread_runtime(
        &self,
        current_model: &str,
        model_id: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Result<crate::models::ThreadRuntimeSelection> {
        // 通过现有活动模型接口读取本机运行时，不启动新的 Codex CLI。
        let context = CliExecutionContext {
            workspace_id: String::new(),
            root_path: String::new(),
            location_kind: CliLocationKind::Local,
            ssh_connection_id: None,
        };
        let engine = self
            .get_active_engine_info(&context)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown engine: {}", self.id()))?;
        crate::cli_tools::BaseCliMcp::resolve_unstarted_thread_runtime(
            &engine,
            current_model,
            model_id,
            reasoning_effort,
        )
        .map_err(anyhow::Error::msg)
    }

    /// 执行 Codex 健康报告中按原始标签登记的 shell 诊断命令。
    async fn run_engine_check(&self, command: &str) -> Result<EngineCheckResultDto> {
        let result: Result<EngineCheckResultDto> = async {
            // 仅从现有 Codex 引擎健康报告读取已登记动作，再交给公共 shell 执行器。
            let _context = self.execution_context(None).await?;
            let engine = self.local_engine().await?;
            let report = engine.health_report().await;
            let action = engine_check::find_action(&report.actions, command)?;
            let shell_command = engine_check::shell_command_for_action(&action)?;
            engine_check::execute_shell(shell_command).await
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "执行 Codex 诊断动作失败: cli_id={} command={command:?} error={error:#}",
                self.id()
            );
        }
        result.with_context(|| format!("执行 Codex 诊断动作失败: cli_id={}", self.id()))
    }

    async fn engine_health(&self, context: &CliExecutionContext) -> Result<EngineHealthDto> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Local {
            let report = self.local_engine().await?.health_report().await;
            return Ok(EngineHealthDto {
                id: "codex".to_string(),
                available: report.available,
                version: report.version,
                details: report.details,
                warnings: report.warnings,
                checks: report.checks,
                fixes: report.fixes,
                protocol_diagnostics: report.protocol_diagnostics,
            });
        }

        let connection_id =
            remote_project_codex_runtime_service::validate_remote_codex_workspace(&workspace)?
                .to_string();
        let db = self.state.db.clone();
        let lookup_connection_id = connection_id.clone();
        let connection = tokio::task::spawn_blocking(move || {
            db::ssh_connections::find(&db, &lookup_connection_id)
        })
        .await
        .context("读取 SSH 连接任务失败")??
        .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在: {connection_id}"))?;

        let mut protocol_diagnostics = None;
        let availability = match remote_project_codex_runtime_service::runtime(&workspace).await {
            Ok(engine) => {
                let models = engine.list_models_runtime().await;
                protocol_diagnostics = engine.protocol_diagnostics_snapshot().await;
                if models.is_empty() {
                    Err(anyhow::anyhow!("远端 Codex 模型目录为空"))
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(error),
        };

        let version = if availability.is_ok() {
            let command = ssh::runtime::wrap_remote_login_shell_command("codex --version");
            ssh::gateway::run_command(&connection, &command)
                .await
                .ok()
                .and_then(|output| String::from_utf8(output.into()).ok())
                .map(|output| output.trim().to_string())
                .filter(|output| !output.is_empty())
        } else {
            None
        };
        let connection_name = workspace
            .connection_display_name
            .clone()
            .unwrap_or_else(|| "未命名 SSH 连接".to_string());

        Ok(EngineHealthDto {
            id: "codex".to_string(),
            available: availability.is_ok(),
            version,
            details: Some(match availability {
                Ok(()) => format!("SSH 远端 Codex：{connection_name}"),
                Err(error) => format!("SSH 远端 Codex 不可用：{error:#}"),
            }),
            warnings: Vec::new(),
            checks: Vec::new(),
            fixes: Vec::new(),
            protocol_diagnostics,
        })
    }

    /// 通过统一 CLI 入口运行 Codex 自身事件监听并同步页面状态。
    async fn run_runtime_event_bridge(&self, app: tauri::AppHandle) -> Result<()> {
        self.listen_runtime_events(app).await
    }

    async fn prewarm_engine(&self, context: &CliExecutionContext) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let _ = remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime()
                .await;
            Ok(())
        } else {
            self.local_engine().await?.prewarm().await
        }
    }

    async fn uses_external_sandbox(&self, context: &CliExecutionContext) -> Result<bool> {
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            Ok(false)
        } else {
            Ok(self.local_engine().await?.uses_external_sandbox().await)
        }
    }

    /// 刷新 Codex 项目会话并将完整快照保存到当前 workspace。
    async fn refresh_workspace_sessions(&self, context: &CliExecutionContext) -> Result<()> {
        // 读取当前项目的 CLI 会话并仅持久化目录精确匹配的快照。
        let workspace = self.load_workspace(context).await?;
        let sessions = self.list_sessions(context, None, Some(false)).await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| path_utils::paths_equal(&session.cwd, &workspace.root_path))
            .collect::<Vec<_>>();
        if let Err(error) = persist_session_snapshots(
            Arc::new(self.state.db.clone()),
            &workspace,
            sessions,
        )
        .await
        {
            log::error!(
                "Codex 项目会话刷新落库失败: workspace_id={} root_path={} error={error:#}",
                workspace.id,
                workspace.root_path
            );
            return Err(error);
        }
        Ok(())
    }

    async fn list_sessions(
        &self,
        context: &CliExecutionContext,
        search_term: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Vec<CliSessionSnapshot>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh
            && search_term.is_none()
            && archived != Some(true)
        {
            // 旧实现先通过 Tunnel 的临时占用启动远端服务，再直接读取 Tunnel：
            // let service_use =
            //     remote_project_codex_runtime_service::acquire_temporary(&workspace).await?;
            let connection_id =
                remote_project_codex_runtime_service::validate_remote_codex_workspace(&workspace)?;
            let service = ssh::cli_service_lifecycle::get(connection_id, "codex").await?;
            let result = list_codex_sessions(
                service.local_port(),
                &workspace.root_path,
            )
            .await;
            // service_use.release().await;
            let mut sessions = result?;
            for session in &mut sessions {
                if session.model_id == "unknown" || session.reasoning_effort.is_none() {
                    let engine_thread_id = session.engine_thread_id.clone();
                    *session = self.read_session(context, &engine_thread_id).await?;
                }
            }
            return Ok(sessions);
        }
        let summaries = if context.location_kind == CliLocationKind::Ssh {
            remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_threads(search_term, archived)
                .await?
        } else {
            self.local_engine()
                .await?
                .list_threads(search_term, archived)
                .await?
        };

        let is_ssh = context.location_kind == CliLocationKind::Ssh;
        let mut sessions = summaries
            .into_iter()
            // 旧边界逻辑允许子目录会话归入父项目，保留注释作为迁移留痕。
            // .filter(|session| path_utils::is_path_within_root(&session.cwd, &workspace.root_path))
            .filter(|session| path_utils::paths_equal(&session.cwd, &workspace.root_path))
            .map(|session| Self::map_session(session, is_ssh))
            .collect::<Vec<_>>();
        for session in &mut sessions {
            if session.model_id == "unknown" || session.reasoning_effort.is_none() {
                let engine_thread_id = session.engine_thread_id.clone();
                *session = self.read_session(context, &engine_thread_id).await?;
            }
        }
        Ok(sessions)
    }

    async fn read_session(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<CliSessionSnapshot> {
        let workspace = self.load_workspace(context).await?;
        // 迁移留痕：旧逻辑直接用 `?` 返回全部错误，无法映射明确的 Codex NotFound；禁止恢复执行。
        // let summary = if context.location_kind == CliLocationKind::Ssh {
        //     remote_project_codex_runtime_service::runtime(&workspace)
        //         .await?
        //         .read_remote_thread(engine_thread_id)
        //         .await?
        // } else {
        //     self.state
        //         .engines
        //         .read_codex_remote_thread(engine_thread_id)
        //         .await?
        // };
        let summary_result = if context.location_kind == CliLocationKind::Ssh {
            remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .read_remote_thread(engine_thread_id)
                .await
        } else {
            self.local_engine()
                .await?
                .read_remote_thread(engine_thread_id)
                .await
        };
        let mut summary = match summary_result {
            Ok(summary) => summary,
            Err(error) => {
                // 只有 Codex app-server 实测的 -32600/thread not loaded
                // 才转换为公共 NotFound；服务、连接和协议错误原样上抛。
                if error.downcast_ref::<CodexThreadNotFoundError>().is_some() {
                    return Err(CliSessionNotFoundError::new("codex", engine_thread_id).into());
                }
                return Err(error);
            }
        };
        if context.location_kind == CliLocationKind::Ssh
            && (summary.model_id.is_none() || summary.reasoning_effort.is_none())
        {
            let (model_id, reasoning_effort) =
                remote_project_codex_runtime_service::runtime(&workspace)
                    .await?
                    .read_thread_runtime(engine_thread_id)
                    .await?;
            if summary.model_id.is_none() {
                summary.model_id = model_id;
            }
            if summary.reasoning_effort.is_none() {
                summary.reasoning_effort = reasoning_effort;
            }
        }
        anyhow::ensure!(
            // 旧边界逻辑允许子目录会话归入父项目，保留注释作为迁移留痕。
            // path_utils::is_path_within_root(&summary.cwd, &workspace.root_path),
            path_utils::paths_equal(&summary.cwd, &workspace.root_path),
            "Codex 会话不属于当前 workspace"
        );
        Ok(Self::map_session(
            summary,
            context.location_kind == CliLocationKind::Ssh,
        ))
    }

    async fn acquire_turn(&self, context: &CliExecutionContext, thread: &ThreadDto) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            *self.remote_turn_use.lock().await =
                Some(remote_project_codex_runtime_service::runtime(&workspace).await?);
        }
        Ok(())
    }

    async fn start_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        scope: ThreadScope,
        resume_engine_thread_id: Option<&str>,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<EngineThread> {
        // 用当前统一权限填充本次启动参数，避免公共占位值覆盖线程实际权限。
        let permissions = self.parse_permissions(context, thread).await?;
        let mut sandbox = sandbox;
        sandbox.approval_policy = Some(permissions["approvalPolicy"].clone());
        sandbox.approvals_reviewer = Some(
            permissions["approvalsReviewer"]
                .as_str()
                .context("Codex permissions 缺少 approvalsReviewer")?
                .to_string(),
        );
        sandbox.sandbox_mode = permissions
            .get("sandboxMode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        sandbox.allow_network = permissions["allowNetwork"]
            .as_bool()
            .context("Codex permissions 缺少 allowNetwork")?;
        sandbox.auto_approve_mcp_elicitations = permissions["autoApproveMcpElicitations"]
            .as_bool()
            .context("Codex permissions 缺少 autoApproveMcpElicitations")?;
        sandbox.permission_profile = None;
        if context.location_kind == CliLocationKind::Ssh {
            let remote_turn_use = self.remote_turn_use.lock().await;
            let engine = remote_turn_use
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("当前 SSH 远端 Codex 会话尚未建立持续使用关系"))?;
            return Engine::start_thread(
                engine.as_ref(),
                scope,
                &thread.id,
                resume_engine_thread_id,
                model,
                sandbox,
            )
            .await;
        }

        let engine = self.configure_local_computer_control().await?;
        Engine::start_thread(
            engine.as_ref(),
            scope,
            &thread.id,
            thread.engine_thread_id.as_deref(),
            model,
            sandbox,
        )
        .await
    }

    async fn send_message(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        mut input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        // 将当前统一权限随本次 TurnInput 发送到本机或 SSH Codex Engine。
        input.permissions = Some(self.parse_permissions(context, thread).await?);
        if context.location_kind == CliLocationKind::Ssh {
            let engine =
                self.remote_turn_use.lock().await.take().ok_or_else(|| {
                    anyhow::anyhow!("当前 SSH 远端 Codex 会话尚未建立持续使用关系")
                })?;
            let result = Engine::send_message(
                engine.as_ref(),
                engine_thread_id,
                input,
                event_tx,
                cancellation,
            )
            .await;
            return result;
        }

        let engine = self.configure_local_computer_control().await?;
        Engine::send_message(
            engine.as_ref(),
            engine_thread_id,
            input,
            event_tx,
            cancellation,
        )
        .await
    }

    async fn steer_message(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        client_steer_id: &str,
        content: &str,
        mut input: TurnInput,
    ) -> Result<EngineSteerReceipt> {
        let workspace = self.load_workspace(context).await?;
        // 将当前统一权限随本次 steer 输入发送，并由 Engine 同步原生线程和轮次设置。
        input.permissions = Some(self.parse_permissions(context, thread).await?);
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            Engine::steer_message(
                engine.as_ref(),
                engine_thread_id,
                client_steer_id,
                content,
                input,
            )
            .await
        } else {
            let engine = self.configure_local_computer_control().await?;
            Engine::steer_message(
                engine.as_ref(),
                engine_thread_id,
                client_steer_id,
                content,
                input,
            )
            .await
        }
    }

    async fn respond_to_approval(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        approval_id: &str,
        response: Value,
        route: Option<ApprovalRequestRoute>,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            Engine::respond_to_approval(engine.as_ref(), approval_id, response, route)
                .await
                .with_context(|| format!("SSH 远端 Codex 审批回复失败: thread_id={}", thread.id))
        } else {
            let engine = self.local_engine().await?;
            Engine::respond_to_approval(engine.as_ref(), approval_id, response, route).await
        }
    }

    async fn interrupt(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        _engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
                return Ok(());
            };
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            Engine::interrupt(engine.as_ref(), engine_thread_id)
                .await
                .with_context(|| format!("SSH 远端 Codex 取消失败: thread_id={}", thread.id))
        } else {
            let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or("default");
            Engine::interrupt(self.local_engine().await?.as_ref(), engine_thread_id).await
        }
    }

    async fn archive_thread(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            Engine::archive_thread(engine.as_ref(), engine_thread_id).await
        } else {
            let engine = self.local_engine().await?;
            match Engine::archive_thread(engine.as_ref(), engine_thread_id).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    let archived = match engine.list_threads(None, Some(true)).await {
                        Ok(sessions) => sessions
                            .into_iter()
                            .any(|session| session.engine_thread_id == engine_thread_id),
                        Err(_) => return Err(error),
                    };
                    if archived {
                        return Ok(());
                    }
                    let active = match engine.list_threads(None, Some(false)).await {
                        Ok(sessions) => sessions
                            .into_iter()
                            .any(|session| session.engine_thread_id == engine_thread_id),
                        Err(_) => return Err(error),
                    };
                    if active {
                        Err(error)
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }

    async fn unarchive_thread(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
            Engine::unarchive_thread(engine.as_ref(), engine_thread_id).await
        } else {
            Engine::unarchive_thread(self.local_engine().await?.as_ref(), engine_thread_id).await
        }
    }

    async fn forget_session(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        _engine_thread_id: &str,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        Ok(())
    }

    async fn read_thread_preview(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<Option<String>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let preview = remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .read_thread_preview(engine_thread_id)
                .await;
            Ok(preview)
        } else {
            Ok(self
                .local_engine()
                .await?
                .read_thread_preview(engine_thread_id)
                .await)
        }
    }

    async fn read_thread_sync_snapshot(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<Option<ThreadSyncSnapshot>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .read_thread_sync_snapshot(engine_thread_id)
                .await
                .map(Some);
        }

        self.local_engine()
            .await?
            .read_thread_sync_snapshot(engine_thread_id)
            .await
            .map(Some)
    }

    async fn set_thread_name(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
        name: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .set_thread_name(engine_thread_id, name)
                .await
        } else {
            self.local_engine()
                .await?
                .set_thread_name(engine_thread_id, name)
                .await
        }
    }

    async fn list_codex_skills(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<Vec<CodexSkillDto>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_skills(&workspace.root_path)
                .await;
        }
        self.local_engine().await?.list_skills(cwd).await
    }

    async fn list_codex_apps(&self, context: &CliExecutionContext) -> Result<Vec<CodexAppDto>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_apps()
                .await;
        }
        self.local_engine().await?.list_apps().await
    }

    async fn list_codex_plugins(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<Vec<CodexPluginDto>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return remote_project_codex_runtime_service::runtime(&workspace)
                .await?
                .list_plugins(&workspace.root_path)
                .await;
        }
        self.local_engine().await?.list_plugins(cwd).await
    }

    async fn get_opencode_runtime_catalog(
        &self,
        context: &CliExecutionContext,
        _cwd: &str,
    ) -> Result<OpenCodeRuntimeCatalogDto> {
        self.load_workspace(context).await?;
        Err(anyhow::anyhow!("Codex 不支持 OpenCode 参数"))
    }

    async fn refresh_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
        requested_kinds: &[String],
    ) -> Result<Vec<ExtensionCatalogKindRefreshDto>> {
        if context.location_kind == CliLocationKind::Local {
            if !context.workspace_id.is_empty() {
                self.load_workspace(context).await?;
            }
            let mut results = Vec::new();
            for kind in requested_kinds {
                results.push(
                    crate::extensions::codex::refresh_kind(self.state.engines.as_ref(), cwd, kind)
                        .await,
                );
            }
            return Ok(results);
        }

        self.load_workspace(context).await?;
        let catalog = self.get_extension_catalog(context, cwd).await?;
        Ok(requested_kinds
            .iter()
            .map(|kind| {
                ExtensionCatalogKindRefreshDto::success(
                    kind,
                    catalog
                        .items
                        .iter()
                        .filter(|item| item.kind == *kind)
                        .cloned()
                        .collect(),
                )
            })
            .collect())
    }

    /// 为当前 Codex 工作区创建一次性扩展目录排期。
    async fn schedule_extension_catalog_context(&self, cwd: Option<String>) -> Result<bool> {
        // 以 provider 与目录上下文标记本进程只排期一次。
        let context_key = extensions::refresh::context_key(cwd.as_deref());
        if !self
            .state
            .extension_catalog_refreshes
            .schedule_once_per_run(self.id(), &context_key)
            .await
        {
            return Ok(false);
        }

        // 初始化快照并把所有扩展类型排为立即可刷新。
        let observed_at = runtime_env::system_time_rfc3339();
        let schedule_result = (|| {
            db::extensions::ensure_context(
                &self.state.db,
                self.id(),
                &context_key,
                &observed_at,
            )?;
            for kind in crate::models::EXTENSION_KINDS {
                db::extensions::schedule_startup_refresh(
                    &self.state.db,
                    self.id(),
                    &context_key,
                    kind,
                    &observed_at,
                )?;
            }
            Ok(())
        })();

        // 排期失败时清除本进程标记，使后续页面读取仍可重试。
        if let Err(error) = schedule_result {
            self.state
                .extension_catalog_refreshes
                .forget_scheduled_context(self.id(), &context_key)
                .await;
            log::error!(
                "Codex 扩展目录排期失败: context={context_key} error={error:#}"
            );
            return Err(error);
        }

        // 清除当前上下文运行状态，并发送整个任务完成事件。
        self.state
            .extension_catalog_refreshes
            .wake_catalog_scheduler();
        Ok(true)
    }

    /// 确保 Codex 扩展刷新上下文并启动对应的后台刷新任务。
    async fn request_extension_catalog_refresh(
        &self,
        app: tauri::AppHandle,
        cwd: Option<String>,
        requested_kinds: Vec<String>,
    ) -> Result<bool> {
        // 保持 normalize、ensure、begin、事件和后台启动的既定顺序。
        let kinds = extensions::refresh::normalize_kinds(requested_kinds)?;
        let context_key = extensions::refresh::context_key(cwd.as_deref());
        db::extensions::ensure_context(
            &self.state.db,
            self.id(),
            &context_key,
            &runtime_env::system_time_rfc3339(),
        )?;
        if !self
            .state
            .extension_catalog_refreshes
            .begin(self.id(), &context_key)
            .await
        {
            return Ok(false);
        }

        extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
        let cli = self.clone();
        tauri::async_runtime::spawn(async move {
            cli.run_extension_catalog_refresh(app, context_key, cwd, kinds)
                .await;
        });
        Ok(true)
    }

    /// 查询 Codex provider 已到期的刷新上下文及扩展类型集合。
    async fn list_due_extension_catalog_refreshes(
        &self,
        now: &str,
    ) -> Result<Vec<ExtensionRefreshJob>> {
        // 仅查询当前 CLI provider 的到期记录，再按上下文聚合给调度循环。
        let targets = db::extensions::list_due_refreshes_for_provider(
            &self.state.db,
            self.id(),
            now,
        )?;
        Ok(extensions::refresh::group_due_refreshes(targets)
            .into_iter()
            .map(|((provider_id, context_key), kinds)| ExtensionRefreshJob {
                provider_id,
                context_key,
                kinds,
            })
            .collect())
    }

    /// 查询 Codex provider 最近一次计划刷新时间。
    async fn next_extension_catalog_refresh_at(&self) -> Result<Option<String>> {
        db::extensions::next_refresh_at_for_provider(&self.state.db, self.id())
    }

    /// 读取当前 Codex 本机缓存或 SSH 目录，并为本机目录登记首次刷新。
    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        if context.location_kind == CliLocationKind::Local {
            let cwd = if context.workspace_id.is_empty() {
                cwd.map(str::to_string)
            } else {
                let workspace = self.load_workspace(context).await?;
                Some(cwd.unwrap_or(workspace.root_path.as_str()).to_string())
            };
            return self.load_cached_extension_catalog(cwd.as_deref()).await;
        }

        let workspace = self.load_workspace(context).await?;

        let engine = remote_project_codex_runtime_service::runtime(&workspace).await?;
        let skills_result = engine.list_skills(&workspace.root_path).await;
        let plugins_result = engine.list_plugins(&workspace.root_path).await;
        let diagnostics = engine.protocol_diagnostics_snapshot().await;
        let skills = skills_result?;
        let plugins = plugins_result?;
        let mcp_servers = diagnostics
            .map(|value| value.mcp_servers)
            .unwrap_or_default();

        let mut items = skills
            .into_iter()
            .map(|skill| ExtensionItemDto {
                id: skill.path.clone(),
                provider_id: "codex".to_string(),
                kind: "skill".to_string(),
                name: skill.name,
                description: (!skill.description.trim().is_empty()).then_some(skill.description),
                version: None,
                scope: skill.scope.clone(),
                source: (!skill.scope.trim().is_empty()).then_some(skill.scope),
                marketplace: None,
                path: Some(skill.path),
                parent_plugin_id: None,
                category: None,
                officially_available: false,
                catalog_authority: None,
                installed: Some(true),
                configured: None,
                enabled: Some(skill.enabled),
                health: if skill.enabled { "healthy" } else { "unknown" }.to_string(),
                auth_state: None,
                available_actions: Vec::new(),
                requires_new_session: false,
                read_only_reason: Some("ssh_remote_codex_extension_action".to_string()),
                warning: None,

                ..Default::default()
            })
            .collect::<Vec<_>>();
        items.extend(plugins.into_iter().map(|plugin| ExtensionItemDto {
            id: plugin.id,
            provider_id: "codex".to_string(),
            kind: "plugin".to_string(),
            name: plugin.name,
            description: plugin.description,
            version: None,
            scope: "user".to_string(),
            source: plugin.developer_name,
            marketplace: None,
            path: None,
            parent_plugin_id: None,
            category: None,
            officially_available: false,
            catalog_authority: None,
            installed: Some(plugin.installed),
            configured: None,
            enabled: Some(plugin.enabled),
            health: if plugin.enabled { "healthy" } else { "unknown" }.to_string(),
            auth_state: None,
            available_actions: Vec::new(),
            requires_new_session: false,
            read_only_reason: Some("ssh_remote_codex_extension_action".to_string()),
            warning: None,

            ..Default::default()
        }));
        items.extend(mcp_servers.into_iter().map(|server| ExtensionItemDto {
            id: server.name.clone(),
            provider_id: "codex".to_string(),
            kind: "mcp".to_string(),
            name: server.name,
            description: Some(format!(
                "{} tools · {} resources · {} resource templates",
                server.tool_count, server.resource_count, server.resource_template_count
            )),
            version: None,
            scope: "user".to_string(),
            source: None,
            marketplace: None,
            path: None,
            parent_plugin_id: None,
            category: None,
            officially_available: false,
            catalog_authority: None,
            installed: None,
            configured: Some(true),
            enabled: Some(true),
            health: "healthy".to_string(),
            auth_state: Some(server.auth_status),
            available_actions: Vec::new(),
            requires_new_session: false,
            read_only_reason: Some("ssh_remote_codex_extension_action".to_string()),
            warning: None,

            ..Default::default()
        }));
        let fetched_at = runtime_env::system_time_rfc3339();
        let kind_fetched_at = ["skill", "plugin", "mcp"]
            .into_iter()
            .map(|kind| (kind.to_string(), Some(fetched_at.clone())))
            .collect();

        Ok(CachedExtensionCatalogDto {
            provider_id: "codex".to_string(),
            cwd: Some(workspace.root_path),
            items,
            sources: Vec::new(),
            capabilities: extensions::provider_capabilities("codex"),
            fetched_at: Some(fetched_at.clone()),
            kind_fetched_at,
            last_attempt_at: Some(fetched_at.clone()),
            next_refresh_at: None,
            refreshing: false,
            refresh_completed_at: Some(fetched_at),
            has_snapshot: true,
            refresh_errors: Vec::new(),
        })
    }

    async fn get_extensions(&self, context: &CliExecutionContext) -> Result<Vec<ExtensionItemDto>> {
        let catalog = self.get_extension_catalog(context, None).await?;
        let mut items = catalog.items;
        for item in &mut items {
            match item.kind.as_str() {
                "skill" => {
                    item.group = Some("skills".to_string());
                }
                "plugin" => {
                    item.panel = Some("plugins".to_string());
                    item.group = Some("plugins".to_string());
                }
                "mcp" => {
                    item.panel = Some("mcp".to_string());
                    item.group = Some("mcp".to_string());
                }
                _ => {}
            }
        }
        let builtin_ids = [
            "review",
            "fork",
            "rollback",
            "compact",
            "fast",
            "personality",
            "experimental",
        ];
        items.extend(builtin_ids.into_iter().map(|id| ExtensionItemDto {
            id: id.to_string(),
            provider_id: "codex".to_string(),
            kind: "command".to_string(),
            name: id.to_string(),
            description: None,
            panel: Some(id.to_string()),
            group: Some("commands".to_string()),
            ..Default::default()
        }));
        Ok(items)
    }

    async fn perform_extension_action(
        &self,
        context: &CliExecutionContext,
        item: ExtensionItemDto,
        action: &str,
        scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto> {
        let workspace = self.load_workspace(context).await?;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 Codex 当前不执行扩展变更，也不会调用本机 Codex"
        );
        let _ = scope;
        crate::extensions::codex::perform_action(&item, action, Some(workspace.root_path.as_str()))
            .await
    }

    /// 用户未选择 workspace 时，使用本机用户级 Codex 配置执行全局扩展动作。
    async fn perform_global_extension_action(
        &self,
        item: ExtensionItemDto,
        action: &str,
        _scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto> {
        crate::extensions::codex::perform_action(&item, action, None).await
    }

    async fn fork_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        cwd: &str,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<CliForkedThread> {
        // 分支会话使用本次源线程权限创建，权限数据不落入原线程数据库字段。
        let permissions = self.parse_permissions(context, thread).await?;
        let mut sandbox = sandbox;
        sandbox.approval_policy = Some(permissions["approvalPolicy"].clone());
        sandbox.approvals_reviewer = Some(
            permissions["approvalsReviewer"]
                .as_str()
                .context("Codex permissions 缺少 approvalsReviewer")?
                .to_string(),
        );
        sandbox.sandbox_mode = permissions
            .get("sandboxMode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        sandbox.allow_network = permissions["allowNetwork"]
            .as_bool()
            .context("Codex permissions 缺少 allowNetwork")?;
        sandbox.auto_approve_mcp_elicitations = permissions["autoApproveMcpElicitations"]
            .as_bool()
            .context("Codex permissions 缺少 autoApproveMcpElicitations")?;
        sandbox.permission_profile = None;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 Codex 暂未接入会话分支，当前不会调用本机 Codex 执行"
        );
        let forked = self
            .local_engine()
            .await?
            .fork_thread(engine_thread_id, cwd, model, sandbox)
            .await?;
        Ok(CliForkedThread {
            engine_thread_id: forked.engine_thread_id,
            model_id: forked.model_id,
            title: forked.title,
            preview: forked.preview,
            raw_status: forked.raw_status,
            active_flags: forked.active_flags,
        })
    }

    async fn rollback_thread(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
        num_turns: u32,
    ) -> Result<ThreadSyncSnapshot> {
        self.load_workspace(context).await?;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 Codex 暂未接入回滚，当前不会调用本机 Codex 执行"
        );
        self.local_engine()
            .await?
            .rollback_thread(engine_thread_id, num_turns)
            .await
    }

    async fn compact_thread(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<super::CliCompactionResult> {
        let workspace = self.load_workspace(context).await?;
        // 旧实现仅允许本机；现在按正式工作区位置获取已有生命周期管理的句柄。
        // anyhow::ensure!(context.location_kind == CliLocationKind::Local,
        //     "SSH 远端 Codex 暂未接入压缩，当前不会调用本机 Codex 执行");
        let engine = match context.location_kind {
            CliLocationKind::Local => self.local_engine().await?,
            CliLocationKind::Ssh => remote_project_codex_runtime_service::runtime(&workspace).await?,
        };
        let (remote_turn_id, mut usage) = engine.compact_thread(engine_thread_id).await.map_err(|error| {
            log::error!("Codex thread compaction failed: workspace_id={} engine_thread_id={engine_thread_id} error={error:#}", workspace.id);
            error
        })?;
        // 原生压缩未携带上下文用量时，从同一生命周期读取一次实际线程快照。
        if usage.is_none() {
            match engine.context_usage_snapshot(engine_thread_id).await {
                Ok(snapshot) => usage = snapshot,
                Err(error) => {
                    log::error!("Codex compaction completed but context usage snapshot refresh failed: workspace_id={} engine_thread_id={engine_thread_id} error={error:#}", workspace.id);
                }
            }
        }
        if let Some((current, max)) = usage {
            let db = self.state.db.clone();
            let workspace_id = workspace.id.clone();
            let snapshot_engine_thread_id = engine_thread_id.to_owned();
            let saved = tokio::task::spawn_blocking(move || -> Result<()> {
                let thread = db::threads::find_thread_by_workspace_engine_thread_id(
                    &db, &workspace_id, "codex", &snapshot_engine_thread_id,
                )?.context("压缩后保存上下文用量失败：会话不存在")?;
                db::threads::update_thread(&db, &crate::models::ThreadUpdateDto {
                    id: thread.id,
                    context_current_tokens: Some(i64::try_from(current)?),
                    context_max_tokens: Some(i64::try_from(max)?),
                    context_usage_updated_at: Some(runtime_env::system_time_rfc3339()),
                    ..Default::default()
                })?;
                Ok(())
            }).await.context("压缩后保存上下文用量任务失败")
                .and_then(|result| result);
            if let Err(error) = saved {
                // 原生压缩已完成，用量持久化异常不能导致用户误判并重复压缩。
                log::error!("Codex compaction completed but context usage persistence failed: workspace_id={} engine_thread_id={engine_thread_id} error={error:#}", workspace.id);
            }
        } else {
            log::warn!("Codex compaction completed without a reliable context usage snapshot: workspace_id={} engine_thread_id={engine_thread_id}", workspace.id);
        }
        Ok(super::CliCompactionResult { remote_turn_id })
    }

    async fn start_review(
        &self,
        context: &CliExecutionContext,
        source_engine_thread_id: &str,
        target: Value,
        delivery: Option<&str>,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
        started_tx: oneshot::Sender<CliReviewStarted>,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 Codex 暂未接入代码审查，当前不会调用本机 Codex 执行"
        );
        let (codex_started_tx, codex_started_rx) = oneshot::channel::<CodexReviewStarted>();
        let forward_started = tokio::spawn(async move {
            let started = codex_started_rx.await?;
            started_tx
                .send(CliReviewStarted {
                    review_thread_id: started.review_thread_id,
                })
                .map_err(|_| anyhow::anyhow!("代码审查会话接收方已关闭"))?;
            Ok::<(), anyhow::Error>(())
        });
        self.local_engine()
            .await?
            .start_review(
                source_engine_thread_id,
                target,
                delivery,
                event_tx,
                cancellation,
                codex_started_tx,
            )
            .await?;
        forward_started.await.context("等待代码审查会话失败")??;
        Ok(())
    }
}

/// 通过 Codex 的 SSH 远端服务协议读取当前目录会话，保留分页与缺少标题时的补读行为。
async fn list_codex_sessions(local_port: u16, cwd: &str) -> Result<Vec<CliSessionSnapshot>> {
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{local_port}/"))
        .await
        .context("connect Codex SSH tunnel")?;
    let mut request_id = 1_u64;
    send_ws_request(
        &mut socket,
        &mut request_id,
        "initialize",
        json!({"clientInfo":{"name":"auracoder-ssh-sync","title":"AuraCoder SSH Sync","version":"0"},"capabilities":{"experimentalApi":true}}),
    )
    .await?;
    socket
        .send(Message::Text(
            json!({"method":"initialized","params":{}})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!("send Codex initialized notification failed: {error}"))?;
    let mut cursor: Option<String> = None;
    let mut sessions = Vec::new();

    // 按原有 50 页上限和每页 100 条读取，并对需要补读的标题逐条请求线程详情。
    for _ in 0..SESSION_REFRESH_MAX_PAGES {
        let response = send_ws_request(
            &mut socket,
            &mut request_id,
            "thread/list",
            json!({
                "cursor": cursor,
                "limit": 100,
                "archived": false,
                "sortKey": "updated_at",
                "cwd": cwd,
            }),
        )
        .await?;
        for value in response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some((mut session, title_needs_read)) = parse_codex_session(value, cwd) {
                if title_needs_read {
                    match send_ws_request(
                        &mut socket,
                        &mut request_id,
                        "thread/read",
                        json!({
                            "threadId": session.engine_thread_id.clone(),
                            "includeTurns": false,
                        }),
                    )
                    .await
                    {
                        Ok(response) => {
                            if let Some((resolved, false)) = parse_codex_session(&response, cwd) {
                                session = resolved;
                            }
                        }
                        Err(error) => log::warn!(
                            "读取 SSH 远端 Codex 会话标题失败: thread_id={} error={error:#}",
                            session.engine_thread_id
                        ),
                    }
                }
                sessions.push(session);
            }
        }
        cursor = response
            .get("nextCursor")
            .or_else(|| response.get("next_cursor"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(sessions)
}

/// 向 Codex WebSocket 服务发送单个 RPC 请求，并保留原始超时与错误上下文。
async fn send_ws_request<S>(
    socket: &mut S,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value>
where
    S: futures::Sink<Message>
        + futures::Stream<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::fmt::Display,
{
    let id = *next_id;
    *next_id += 1;
    socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!("send Codex RPC failed: {error}"))?;
    loop {
        let message = tokio::time::timeout(SESSION_REFRESH_REQUEST_TIMEOUT, socket.next())
            .await
            .context("Codex RPC timeout")?
            .ok_or_else(|| anyhow::anyhow!("Codex WebSocket closed"))??;
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).context("parse Codex RPC")?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex RPC error: {error}");
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// 将 Codex 远端列表或线程详情转换为统一会话快照，并标记是否需要补读标题。
pub(crate) fn parse_codex_session(
    value: &Value,
    expected_cwd: &str,
) -> Option<(CliSessionSnapshot, bool)> {
    let thread = value.get("thread").unwrap_or(value);
    let cwd = string_field(thread, &["cwd"])?;
    if cwd != expected_cwd {
        return None;
    }
    let id = string_field(thread, &["id"])?;
    let updated = number_field(thread, &["updatedAt", "updated_at"])
        .or_else(|| number_field(thread, &["createdAt", "created_at"]));
    let explicit_title = string_field(value, &["name", "threadName", "title"])
        .or_else(|| string_field(thread, &["name", "threadName", "title"]))
        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|title| !title.is_empty());
    let preview = string_field(value, &["preview"])
        .or_else(|| string_field(thread, &["preview"]))
        .map(|preview| preview.trim().to_string())
        .filter(|preview| !preview.is_empty());
    let title_needs_read = explicit_title.is_none()
        && preview
            .as_deref()
            .is_none_or(|preview| preview.starts_with("<<ccr:") && preview.ends_with(">>"));
    let title = explicit_title
        .or_else(|| {
            preview
                .as_deref()
                .filter(|_| !title_needs_read)
                .and_then(|preview| preview.lines().next())
                .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|title| !title.is_empty())
        })
        .map(|title| title.chars().take(120).collect())
        .unwrap_or_else(|| id.clone());
    let model_id = string_field(thread, &["model", "modelId", "model_id"])
        .or_else(|| string_field(value, &["model", "modelId", "model_id"]))
        .unwrap_or_else(|| "unknown".to_string());
    let reasoning_effort = string_field(thread, &["reasoningEffort", "reasoning_effort", "effort"])
        .or_else(|| string_field(value, &["reasoningEffort", "reasoning_effort", "effort"]));
    let model_provider = string_field(thread, &["modelProvider", "model_provider"])
        .or_else(|| string_field(value, &["modelProvider", "model_provider"]));
    let status = status_from_value(value.get("status").or_else(|| thread.get("status")));
    Some((
        CliSessionSnapshot {
            engine_thread_id: id,
            title,
            preview: None,
            cwd: cwd.clone(),
            model_id,
            reasoning_effort,
            created_at: None,
            updated_at: updated.map(unix_to_rfc3339),
            source_kind: None,
            raw_status: Some(status.as_str().to_string()),
            active_flags: Vec::new(),
            status,
            archived: false,
            metadata: json!({
                "sshRemote": true,
                "codexRemoteCwd": cwd,
                "codexRemote": value,
                "codexModelProvider": model_provider,
            }),
        },
        title_needs_read,
    ))
}

/// 从远端快照字段中按候选名称读取字符串值。
fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str).map(str::to_owned))
}

/// 从远端快照字段中按候选名称读取整数值。
fn number_field(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
}

/// 将 Codex 返回的秒或毫秒时间戳格式化为 RFC3339。
fn unix_to_rfc3339(value: i64) -> String {
    chrono::DateTime::from_timestamp(
        if value > 10_000_000_000 {
            value / 1000
        } else {
            value
        },
        0,
    )
    .map(|date| date.to_rfc3339())
    .unwrap_or_default()
}

/// 将 Codex 远端原始状态映射为统一线程状态。
fn status_from_value(value: Option<&Value>) -> ThreadStatusDto {
    match value
        .and_then(|item| item.get("type").or(Some(item)))
        .and_then(Value::as_str)
    {
        Some("inProgress") | Some("streaming") | Some("running") => ThreadStatusDto::Streaming,
        Some("error") | Some("failed") => ThreadStatusDto::Error,
        Some("completed") | Some("success") => ThreadStatusDto::Completed,
        _ => ThreadStatusDto::Idle,
    }
}

/// 使用共享线程 Mapper 将 Codex 刷新快照逐条写入当前 workspace。
pub(crate) async fn persist_session_snapshots(
    db: Arc<crate::db::Database>,
    workspace: &WorkspaceDto,
    sessions: Vec<CliSessionSnapshot>,
) -> Result<()> {
    for session in sessions {
        let workspace_id = workspace.id.clone();
        tokio::task::spawn_blocking({
            let db = db.clone();
            move || {
                db::threads::upsert_ssh_remote_thread_snapshot(
                    db.as_ref(),
                    &workspace_id,
                    "codex",
                    &session.engine_thread_id,
                    &session.model_id,
                    &session.title,
                    session.status,
                    &session.metadata,
                    session.reasoning_effort.as_deref(),
                    session.updated_at.as_deref(),
                )
            }
        })
        .await
        .context("persist remote session task failed")??;
    }
    Ok(())
}

pub(crate) mod thread_business {
    use super::*;
    use chrono::{DateTime, Utc};
    use crate::services::thread_service::normalize_thread_title;
    use tauri::{AppHandle, Emitter};
    use crate::{engines::CodexRemoteThreadSummary, models::{CodexRemoteThreadDto, CodexRemoteThreadPageDto, MessageDto, MessageStatusDto, ThreadUpdateDto}, path_utils::paths_equal, services::thread_service::ThreadService};
    use crate::cli_tools::mcp::thread_business::{validate_model_for_engine_from_catalog, validate_reasoning_effort_from_catalog};

    pub(crate) fn collect_remote_thread_roots(workspace_root: &str) -> std::collections::HashSet<String> {
        std::iter::once(workspace_root.to_string()).collect()
    }
    pub(crate) fn codex_remote_thread_belongs_to_workspace(workspace_root: &str, cwd: &str) -> bool {
        paths_equal(cwd, workspace_root)
    }
    pub(crate) fn normalize_remote_thread_search_term(search_term: Option<String>) -> Option<String> {
        search_term
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
    pub(crate) fn parse_codex_remote_thread_cursor(cursor: Option<&str>) -> Result<usize, String> {
        let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(0);
        };

        cursor.parse::<usize>().map_err(|_| {
                format!("invalid Codex remote thread cursor `{cursor}`. expected a non-negative offset")
        })
    }
    pub(crate) fn normalize_codex_remote_thread_limit(limit: Option<u32>) -> usize {
        limit.unwrap_or(20).clamp(1, 100) as usize
    }
    pub(crate) fn map_codex_remote_thread_dto(
        thread: CliSessionSnapshot,
        local_thread_id: Option<String>,
    ) -> CodexRemoteThreadDto {
        let model_provider = thread
            .metadata
            .get("codexModelProvider")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        CodexRemoteThreadDto {
            engine_thread_id: thread.engine_thread_id,
            title: Some(thread.title),
            preview: thread.preview.unwrap_or_default(),
            cwd: thread.cwd,
            created_at: thread.created_at.unwrap_or_default(),
            updated_at: thread.updated_at.unwrap_or_default(),
            model_id: thread.model_id,
            reasoning_effort: thread.reasoning_effort,
            model_provider,
            source_kind: thread.source_kind.unwrap_or_default(),
            status_type: thread.raw_status.unwrap_or_default(),
            active_flags: thread.active_flags,
            archived: thread.archived,
            local_thread_id,
        }
    }
    pub(crate) fn codex_remote_thread_timestamp_to_rfc3339(timestamp: i64) -> String {
        let (seconds, nanos) = if timestamp > 10_000_000_000 {
            (timestamp / 1000, ((timestamp % 1000) as u32) * 1_000_000)
        } else {
            (timestamp, 0)
        };

        DateTime::<Utc>::from_timestamp(seconds, nanos)
            .unwrap_or_else(Utc::now)
            .to_rfc3339()
    }
    pub(crate) fn build_codex_remote_thread_title(thread: &CodexRemoteThreadSummary) -> String {
        if let Some(title) = thread
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return normalize_thread_title(title).unwrap_or_else(|_| {
                    format!(
                        "Codex thread {}",
                        short_thread_label(&thread.engine_thread_id)
                    )
            });
        }

        if let Some(preview) = thread
            .preview
            .trim()
            .split('\n')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return normalize_thread_title(preview).unwrap_or_else(|_| {
                    format!(
                        "Codex thread {}",
                        short_thread_label(&thread.engine_thread_id)
                    )
            });
        }

        format!(
            "Codex thread {}",
            short_thread_label(&thread.engine_thread_id)
        )
    }
    pub(crate) fn short_thread_label(engine_thread_id: &str) -> String {
        engine_thread_id.chars().take(8).collect()
    }
    pub(crate) fn build_codex_remote_thread_metadata(
        thread: &CodexRemoteThreadSummary,
        model_id: &str,
        reasoning_effort: Option<&str>,
    ) -> Value {
        let _ = model_id;
        let mut metadata = merge_codex_runtime_metadata(
            None,
            Some(thread.status_type.as_str()),
            &thread.active_flags,
            Some(thread.preview.as_str()),
            true,
            Some("remote_thread_attached"),
        );

        if let Some(object) = metadata.as_object_mut() {
            object.insert("codexTranscriptImported".to_string(), json!(false));
            object.insert(
                    "codexModelProvider".to_string(),
                json!(thread.model_provider),
            );
            object.insert("codexSourceKind".to_string(), json!(thread.source_kind));
            object.insert("codexRemoteArchived".to_string(), json!(thread.archived));
            object.insert("codexRemoteCwd".to_string(), json!(thread.cwd));
            object.insert(
                    "codexRemoteCreatedAt".to_string(),
                json!(codex_remote_thread_timestamp_to_rfc3339(thread.created_at)),
            );
            object.insert(
                    "codexRemoteUpdatedAt".to_string(),
                json!(codex_remote_thread_timestamp_to_rfc3339(thread.updated_at)),
            );
        }

        metadata
    }
    pub(crate) fn mark_codex_transcript_imported(mut metadata: Value, imported: bool) -> Value {
        if imported {
            if let Some(object) = metadata.as_object_mut() {
                object.insert("codexTranscriptImported".to_string(), json!(true));
            }
        }

        metadata
    }
    pub(crate) fn imported_messages_have_streaming_turn(snapshot: &ThreadSyncSnapshot) -> bool {
        snapshot
            .imported_messages
            .iter()
            .any(|message| message.status == "streaming")
    }
    pub(crate) fn merge_codex_runtime_metadata(
        existing: Option<serde_json::Value>,
        raw_status: Option<&str>,
        active_flags: &[String],
        preview: Option<&str>,
        sync_required: bool,
        sync_reason: Option<&str>,
    ) -> serde_json::Value {
        let mut metadata = existing.unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            metadata = json!({});
        }

        if let Some(object) = metadata.as_object_mut() {
        // 线程底部六项已迁移到独立列；远端同步只保留状态 metadata，清除旧镜像键。
            for key in [
                "lastModelId",
                "reasoningEffort",
            ] {
                object.remove(key);
            }
            match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
                Some(status) => {
                    object.insert("codexThreadStatus".to_string(), json!(status));
                }
                None => {
                    object.remove("codexThreadStatus");
                }
            }

            if active_flags.is_empty() {
                object.remove("codexThreadActiveFlags");
            } else {
                object.insert("codexThreadActiveFlags".to_string(), json!(active_flags));
            }

            match preview.map(str::trim).filter(|value| !value.is_empty()) {
                Some(preview) => {
                    object.insert("codexPreview".to_string(), json!(preview));
                }
                None => {
                    object.remove("codexPreview");
                }
            }

            object.insert("codexSyncRequired".to_string(), json!(sync_required));
            if sync_required {
                object.insert(
                        "codexSyncUpdatedAt".to_string(),
                    json!(runtime_env::system_time_rfc3339()),
                );
                if let Some(reason) = sync_reason.map(str::trim).filter(|value| !value.is_empty()) {
                    object.insert("codexSyncReason".to_string(), json!(reason));
                }
            } else {
                object.insert(
                        "codexSyncUpdatedAt".to_string(),
                    json!(runtime_env::system_time_rfc3339()),
                );
                object.insert("codexSyncReason".to_string(), serde_json::Value::Null);
            }
        }

        metadata
    }
    pub(crate) fn is_codex_thread_sync_required(metadata: Option<&Value>) -> bool {
        metadata
            .and_then(|value| value.get("codexSyncRequired"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub(crate) fn should_clone_local_branch_history(source_thread: &ThreadDto) -> bool {
        !is_codex_thread_sync_required(source_thread.engine_metadata.as_ref())
        && source_thread.message_count > 0
    }
    pub(crate) fn clone_codex_branch_metadata(
        existing: Option<&Value>,
        model_id: &str,
        raw_status: Option<&str>,
        active_flags: &[String],
        preview: Option<&str>,
        sync_required: bool,
        sync_reason: Option<&str>,
    ) -> Value {
        let mut metadata = existing.cloned().unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            metadata = json!({});
        }

        if let Some(object) = metadata.as_object_mut() {
            object.remove("manualTitle");
            object.remove("manualTitleUpdatedAt");
            object.insert("codexTranscriptImported".to_string(), json!(true));
        }

        merge_codex_runtime_metadata(
            Some(metadata),
            raw_status,
            active_flags,
            preview,
            sync_required,
            sync_reason,
        )
    }
    pub(crate) fn codex_transcript_imported(metadata: Option<&Value>) -> bool {
        metadata
            .and_then(|value| value.get("codexTranscriptImported"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
    pub(crate) fn thread_reasoning_effort(_metadata: Option<&Value>) -> Option<String> {
    /*
    // 旧实现从 engine_metadata_json 读取 reasoningEffort，已停用：
    _metadata
        .and_then(|value| value.get("reasoningEffort"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    */
        None
    }
    pub(crate) fn thread_last_model_id(_metadata: Option<&Value>) -> Option<String> {
        None
    }
    pub(crate) fn thread_service_tier(metadata: Option<&Value>) -> Option<String> {
        metadata
            .and_then(|value| value.get("serviceTier"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| matches!(*value, "fast" | "flex"))
            .map(ToOwned::to_owned)
    }
    pub(crate) fn thread_personality(metadata: Option<&Value>) -> Option<String> {
        metadata
            .and_then(|value| value.get("personality"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| matches!(*value, "none" | "friendly" | "pragmatic"))
            .map(ToOwned::to_owned)
    }
    pub(crate) fn thread_output_schema(metadata: Option<&Value>) -> Option<Value> {
        metadata
            .and_then(|value| value.get("outputSchema"))
            .cloned()
    }
    pub(crate) fn thread_opencode_agent(metadata: Option<&Value>) -> Option<String> {
        metadata
            .and_then(|value| value.get("opencodeAgent"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
    pub(crate) fn map_codex_thread_status_to_local(
        raw_status: Option<&str>,
        active_flags: &[String],
        has_local_turn: bool,
    ) -> Option<ThreadStatusDto> {
        if has_local_turn {
            return None;
        }

        match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
            Some("systemError") => Some(ThreadStatusDto::Error),
            Some("idle") | Some("notLoaded") => Some(ThreadStatusDto::Idle),
            Some("active") => {
                if active_flags
                    .iter()
                    .any(|flag| matches!(flag.as_str(), "waitingOnApproval" | "waitingOnUserInput"))
                {
                    Some(ThreadStatusDto::AwaitingApproval)
                } else {
                    Some(ThreadStatusDto::Streaming)
                }
            }
            _ => None,
        }
    }
    pub(crate) fn normalize_thread_personality(value: Option<String>) -> Result<Option<String>, String> {
        let normalized = value
            .as_deref()
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .map(str::to_lowercase);

        let Some(normalized) = normalized else {
            return Ok(None);
        };

        match normalized.as_str() {
            "none" | "friendly" | "pragmatic" => Ok(Some(normalized)),
            _ => Err(format!(
            "invalid personality `{normalized}`. expected one of: none, friendly, pragmatic"
            )),
        }
    }
    pub(crate) fn normalize_thread_service_tier(value: Option<String>) -> Result<Option<String>, String> {
        let normalized = value
            .as_deref()
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .map(str::to_lowercase);

        let Some(normalized) = normalized else {
            return Ok(None);
        };

        match normalized.as_str() {
            "fast" | "flex" => Ok(Some(normalized)),
            _ => Err(format!(
            "invalid service tier `{normalized}`. expected one of: fast, flex"
            )),
        }
    }

    /// 校验 Codex 定时任务允许的服务档位，同时保留合法输入原值。
    pub(crate) fn normalize_scheduled_codex_service_tier(
        value: Option<String>,
    ) -> Result<Option<String>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let trimmed_value = value.trim();
        match trimmed_value {
            "" | "inherit" => Ok(None),
            "fast" | "flex" => Ok(Some(value)),
            _ => anyhow::bail!("Codex 定时任务服务档位无效: `{trimmed_value}`"),
        }
    }
    pub(crate) fn normalize_thread_output_schema(value: Option<Value>) -> Result<Option<Value>, String> {
        let Some(value) = value else {
            return Ok(None);
        };

        match value {
            Value::Object(_) | Value::Bool(_) => Ok(Some(value)),
            _ => Err("invalid output schema. expected a JSON Schema object or boolean".to_string()),
        }
    }
    pub(crate) fn err_to_string(error: impl std::fmt::Display) -> String {
        format!("{error:#}")
    }
    pub(crate) async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
    where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || operation(&db))
            .await
            .map_err(|error| error.to_string())?
            .map_err(err_to_string)
    }
    impl CodexCli {
        pub(crate) async fn thread_list_records(&self, workspace_id: String, cursor: Option<String>, limit: Option<u32>, search_term: Option<String>, archived: Option<bool>) -> Result<CodexRemoteThreadPageDto, String> {
            let state = &self.state;

            let db = state.db.clone();
            let workspace_root = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    move |db| {
                        let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
                        Ok(workspace.root_path)
                    }
            })
                .await?;

            let normalized_search_term = normalize_remote_thread_search_term(search_term);
            let workspace = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                    }
            })
                .await?;
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let matching_threads = cli
                .list_sessions(&context, normalized_search_term.as_deref(), archived)
                .await
                .map_err(err_to_string)?
                .into_iter()
                .filter(|thread| codex_remote_thread_belongs_to_workspace(&workspace_root, &thread.cwd))
                .collect::<Vec<_>>();

            let offset = parse_codex_remote_thread_cursor(cursor.as_deref())?;
            let page_size = normalize_codex_remote_thread_limit(limit);
            let page_end = offset.saturating_add(page_size).min(matching_threads.len());
            let page_threads = if offset >= matching_threads.len() {
                Vec::new()
            } else {
                matching_threads[offset..page_end].to_vec()
            };
            let next_cursor = (page_end < matching_threads.len()).then(|| page_end.to_string());

            run_db(db, move |db| {
                    let threads = page_threads
                        .into_iter()
                        .map(|thread| {
                            let local_thread_id = db::threads::find_thread_by_engine_thread_id(
                                db,
                                "codex",
                                &thread.engine_thread_id,
                            )?
                                .filter(|local| local.workspace_id == workspace_id)
                                .map(|local| local.id);
                            Ok(map_codex_remote_thread_dto(thread, local_thread_id))
                    })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    Ok(CodexRemoteThreadPageDto {
                            threads,
                            next_cursor,
                    })
            })
                .await

        }
        pub(crate) async fn thread_attach_record(&self, workspace_id: String, engine_thread_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;

    // let normalized_model_id =
    //     validate_model_for_engine(state, "codex", model_id.trim()).await?;
            let db = state.db.clone();
            let existing_local_thread = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    let engine_thread_id = engine_thread_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
                        let existing =
                        db::threads::find_thread_by_engine_thread_id(db, "codex", &engine_thread_id)?
                            .filter(|thread| thread.workspace_id == workspace_id);
                        Ok(existing)
                    }
            })
                .await?;

            let workspace = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                    }
            })
                .await?;
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let session = cli
                .read_session(&context, &engine_thread_id)
                .await
                .map_err(err_to_string)?;
            let validation_models = cli
                .models_for_validation(&context, &session.model_id)
                .await
                .ok();
    // Codex Desktop / vscode 创建的会话 model 为 null（映射后为 "unknown"），不在
    // 模型目录中。目录校验失败不得中断导入：优先回退到目录默认模型，目录不可用
    // 时保留会话原值，保证这类会话能正常发现并显示。
    // 旧实现校验失败直接返回错误，导致无 model 的会话被整体丢弃、项目下"没有会话"：
    // let normalized_model_id = validate_model_for_engine_from_catalog(
    //     "codex",
    //     session.model_id.trim(),
    //     validation_models.as_deref(),
    // )?;
            let normalized_model_id = match validate_model_for_engine_from_catalog(
                "codex",
                session.model_id.trim(),
                validation_models.as_deref(),
            ) {
                Ok(model_id) => model_id,
                Err(error) => {
                    let fallback = validation_models.as_deref().and_then(|models| {
                            models
                                .iter()
                                .find(|model| model.is_default && !model.hidden)
                                .or_else(|| models.iter().find(|model| !model.hidden))
                                .or_else(|| models.first())
                    });
                    match fallback {
                        Some(model) => {
                            log::warn!(
                                "Codex 会话模型不在模型目录中，导入回退到目录默认模型: engine_thread_id={} session_model={} fallback_model={} error={}",
                                engine_thread_id,
                                session.model_id.trim(),
                                model.id,
                                error,
                            );
                            model.id.clone()
                        }
                        None => {
                            log::warn!(
                                "Codex 会话模型校验失败且无模型目录可回退，导入保留会话原模型: engine_thread_id={} session_model={} error={}",
                                engine_thread_id,
                                session.model_id.trim(),
                                error,
                            );
                            session.model_id.trim().to_string()
                        }
                    }
                }
            };
            let normalized_reasoning_effort = session
                .reasoning_effort
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase);
    // reasoning_effort 校验失败同样不得中断导入：忽略该值、保留会话本体。
    // 旧实现直接返回错误：
    // let validated_reasoning_effort = normalized_reasoning_effort
    //     .as_deref()
    //     .map(|value| {
    //         validate_reasoning_effort_from_catalog(
    //             &normalized_model_id,
    //             value,
    //             validation_models.as_deref(),
    //         )
    //     })
    //     .transpose()?;
            let validated_reasoning_effort = normalized_reasoning_effort
                .as_deref()
                .map(|value| {
                    validate_reasoning_effort_from_catalog(
                        &normalized_model_id,
                        value,
                        validation_models.as_deref(),
                    )
            })
                .transpose()
                .unwrap_or_else(|error| {
                    log::warn!(
                        "Codex 会话 reasoning_effort 不受支持，导入时忽略该值: engine_thread_id={} error={}",
                        engine_thread_id,
                        error,
                    );
                    None
            });
            let model_provider = session
                .metadata
                .get("codexModelProvider")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let parse_timestamp = |value: Option<String>| {
                value
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or_default()
            };
            let mut remote_thread = CodexRemoteThreadSummary {
                engine_thread_id: session.engine_thread_id.clone(),
                title: Some(session.title.clone()),
                preview: session.preview.unwrap_or_default(),
                cwd: session.cwd,
                created_at: parse_timestamp(session.created_at),
                updated_at: parse_timestamp(session.updated_at),
                model_id: Some(normalized_model_id.clone()),
                reasoning_effort: validated_reasoning_effort.clone(),
                model_provider,
                source_kind: session.source_kind.unwrap_or_default(),
                status_type: session.raw_status.unwrap_or_default(),
                active_flags: session.active_flags,
                archived: session.archived,
            };
            if remote_thread.archived {
                let operation_thread = existing_local_thread.clone().unwrap_or_else(|| ThreadDto {
                        id: engine_thread_id.clone(),
                        workspace_id: workspace.id.clone(),
                        engine_id: "codex".to_string(),
                        model_id: normalized_model_id.clone(),
                        engine_thread_id: Some(engine_thread_id.clone()),
                        engine_metadata: None,
                        plan_mode: None,
                        send_method: None,
                        reasoning_effort: None,
                        permission_mode: None,
                        title: remote_thread
                            .title
                            .clone()
                            .unwrap_or_else(|| engine_thread_id.clone()),
                        status: ThreadStatusDto::Idle,
                        message_count: 0,
                        total_tokens: 0,
                        context_current_tokens: None,
                        context_max_tokens: None,
                        context_usage_updated_at: None,
                        created_at: String::new(),
                        last_activity_at: String::new(),
                });
                cli.unarchive_thread(&context, &operation_thread, &engine_thread_id)
                    .await
                    .map_err(err_to_string)?;
                remote_thread.archived = false;
            }
            let title = build_codex_remote_thread_title(&remote_thread);
            let metadata = build_codex_remote_thread_metadata(
                &remote_thread,
                &normalized_model_id,
                validated_reasoning_effort.as_deref(),
            );
    // 只有远端返回并成功解析的更新时间才覆盖本地活动时间，避免写入 Unix epoch。
            let remote_last_activity_at = (remote_thread.updated_at > 0)
                .then(|| codex_remote_thread_timestamp_to_rfc3339(remote_thread.updated_at));

            let existing_last_activity_at = remote_last_activity_at.clone();
            if let Some(existing) = existing_local_thread {
                return run_db(db, move |db| {
                        let thread = match db::threads::restore_thread(db, &existing.id) {
                            Ok(restored) => restored,
                            Err(_) => existing,
                        };
                        let updated = db::threads::update_thread_runtime_snapshot(
                            db,
                            &thread.id,
                            Some(&title),
                            map_codex_thread_status_to_local(
                                Some(remote_thread.status_type.as_str()),
                                &remote_thread.active_flags,
                                false,
                            ),
                            Some(&metadata),
                        )?;
            // 模型以用户在 AuraCoder 中的选择为准：本地已有具体模型时保留，仅本地模型未知
            // （空或 "unknown"，如首次导入历史会话）时才接受 Codex 会话返回的模型，
            // 与 SSH 快照路径的保护语义一致。旧行为无条件用 Codex 会话模型覆盖，
            // 导致用户改模型后每次重启 attach 同步即被重置：
            // db::threads::update_thread_runtime_selection(
            //     db,
            //     &updated.id,
            //     "codex",
            //     &normalized_model_id,
            //     None,
            //     None,
            //     validated_reasoning_effort.as_deref(),
            //     None,
            // )?;
                        let local_model_id = updated.model_id.trim();
                        let effective_model_id = if local_model_id.is_empty() || local_model_id == "unknown" {
                            normalized_model_id.as_str()
                        } else {
                            local_model_id
                        };
            // CLI 服务端不返回计划/发送方法/权限；模型按上方保护规则、思考强度按其真实返回落库。
                        db::threads::update_thread_runtime_selection(
                            db,
                            &updated.id,
                            "codex",
                            effective_model_id,
                            None,
                            None,
                            validated_reasoning_effort.as_deref(),
                            None,
                        )?;
                        match existing_last_activity_at.as_deref() {
                            Some(last_activity_at) => {
                                db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
                            }
                            None => Ok(updated),
                        }
                })
                    .await;
            }

            run_db(db, move |db| {
                    let created =
                    db::threads::create_thread(db, &workspace_id, "codex", &normalized_model_id, &title)?;
                    db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
                    let updated = db::threads::update_thread_runtime_snapshot(
                        db,
                        &created.id,
                        Some(&title),
                        map_codex_thread_status_to_local(
                            Some(remote_thread.status_type.as_str()),
                            &remote_thread.active_flags,
                            false,
                        ),
                        Some(&metadata),
                    )?;
        // CLI 服务端不返回计划/发送方法/权限，仅按其真实返回的模型与思考强度落库。
                    db::threads::update_thread_runtime_selection(
                        db,
                        &updated.id,
                        "codex",
                        &normalized_model_id,
                        None,
                        None,
                        validated_reasoning_effort.as_deref(),
                        None,
                    )?;
                    match remote_last_activity_at.as_deref() {
                        Some(last_activity_at) => {
                            db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
                        }
                        None => Ok(updated),
                    }
            })
                .await

        }
        pub(crate) async fn thread_sync_record(&self, thread_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "codex" { return Err("线程所属 CLI 与 Codex 实现不一致".to_owned()); }
            let workspace = run_db(db.clone(), {
                    let workspace_id = thread.workspace_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                    }
            })
                .await?;
            let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
                return Ok(thread);
            };
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let Some(snapshot) = cli
                .read_thread_sync_snapshot(&context, &thread, engine_thread_id)
                .await
                .map_err(err_to_string)?
            else {
                return Ok(thread);
            };

            let has_local_turn = state.turns.get(&thread_id).await.is_some();
            let has_active_remote_turn =
            !snapshot.active_flags.is_empty() || imported_messages_have_streaming_turn(&snapshot);
            let imported_messages = snapshot
                .imported_messages
                .iter()
                .map(|message| db::messages::ImportedMessageRecord {
                    role: message.role.clone(),
                    content: message.content.clone(),
                    blocks: message.blocks.clone(),
                    status: MessageStatusDto::from_str(message.status.as_str()),
                    turn_engine_id: message.turn_engine_id.clone(),
                    remote_turn_id: message.turn_engine_id.clone(),
                    turn_model_id: message.turn_model_id.clone(),
                    turn_reasoning_effort: message.turn_reasoning_effort.clone(),
                    token_input: message.token_input,
                    token_output: message.token_output,
                    created_at: message.created_at.clone(),
            })
                .collect::<Vec<_>>();
            let imported_any_messages = !imported_messages.is_empty();
            run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| {
                        db::messages::append_thread_messages(db, &thread_id, &imported_messages)?;
                        db::threads::refresh_thread_message_stats(
                            db,
                            &thread_id,
                            db::threads::ThreadMessageStatsActivityPolicy::PreserveExisting,
                        )?;
                        Ok::<_, anyhow::Error>(())
                    }
            })
                .await?;

            let sync_required = !has_local_turn && has_active_remote_turn;
            let metadata = merge_codex_runtime_metadata(
                thread.engine_metadata.clone(),
                snapshot.raw_status.as_deref(),
                &snapshot.active_flags,
                snapshot.preview.as_deref(),
                sync_required,
                sync_required.then_some("remote thread has an active turn"),
            );
            let metadata = mark_codex_transcript_imported(metadata, imported_any_messages);
            let next_status = map_codex_thread_status_to_local(
                snapshot.raw_status.as_deref(),
                &snapshot.active_flags,
                has_local_turn,
            );

            run_db(db, {
                    let thread_id = thread_id.clone();
                    let title = snapshot.title.clone();
                    let metadata = metadata.clone();
                    let next_status = next_status.clone();
                    move |db| {
            // 旧调用把 next_status 传入 update_thread_runtime_snapshot，会经 update_thread_status 触碰活动时间；点击同步改为下方 status-only 局部更新。
                        let updated = db::threads::update_thread_runtime_snapshot(
                            db,
                            &thread_id,
                            title.as_deref(),
                            None,
                            Some(&metadata),
                        )?;
                        let Some(next_status) = next_status else {
                            return Ok(updated);
                        };
                        if updated.status == next_status {
                            return Ok(updated);
                        }
                        db::threads::update_thread(
                            db,
                            &ThreadUpdateDto {
                    // 已完成标题和 metadata 同步后的线程标识。
                                id: updated.id,
                    // 只更新状态，避免状态同步路径改写活动时间。
                                status: Some(next_status.as_str().to_owned()),
                                    ..Default::default()
                            },
                        )
                    }
            })
                .await

        }
        pub(crate) async fn thread_reject_local_only_action(&self, thread: &ThreadDto, action: &str) -> Result<(), String> {
            let state = &self.state;

            let db = state.db.clone();
            let workspace_id = thread.workspace_id.clone();
            let workspace = run_db(db, move |db| {
                    db::workspaces::find_workspace_by_id(db, &workspace_id)
            })
                .await?;
            if workspace.is_some_and(|workspace| workspace.location_kind == "ssh") {
                return Err(format!(
            "SSH 远端 Codex 暂未接入{action}，当前不会调用本机 Codex 执行"
                ));
            }
            Ok(())

        }
        pub(crate) async fn thread_build_branch_context(&self, thread: &ThreadDto) -> Result<(String, String, SandboxPolicy), String> {
            let state = &self.state;

            let db = state.db.clone();
            let workspace = run_db(db, {
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
            let writable_roots = vec![workspace_root.clone()];

            Ok((
                    workspace_root,
                    thread.model_id.clone(),
                    SandboxPolicy {
                        writable_roots,
                        allow_network: false,
                        auto_approve_mcp_elicitations: false,
                        approval_policy: None,
                        permission_profile: None,
                        approvals_reviewer: None,
                        reasoning_effort: thread.reasoning_effort.clone(),
                        sandbox_mode: None,
                        service_tier: thread_service_tier(thread.engine_metadata.as_ref()),
                        personality: thread_personality(thread.engine_metadata.as_ref()),
                        output_schema: thread_output_schema(thread.engine_metadata.as_ref()),
                        opencode_agent: thread_opencode_agent(thread.engine_metadata.as_ref()),
                    },
            ))

        }
        pub(crate) async fn thread_create_branch(&self, source_thread: &ThreadDto, engine_thread_id: &str, model_id: &str, title: Option<&str>, preview: Option<&str>, raw_status: Option<&str>, active_flags: &[String], rollback_turns: Option<u32>) -> Result<ThreadDto, String> {
            let state = &self.state;

            if !codex_transcript_imported(source_thread.engine_metadata.as_ref()) {
                return Err(
            "native Codex history tools require a locally mirrored transcript. Attached remote threads without imported history cannot be forked or rolled back yet."
                        .to_string(),
                );
            }

            let db = state.db.clone();
            let created = run_db(db.clone(), {
                    let source_thread = source_thread.clone();
                    let engine_thread_id = engine_thread_id.to_string();
                    let model_id = model_id.to_string();
                    let title = title.map(str::to_string);
                    let preview = preview.map(str::to_string);
                    let raw_status = raw_status.map(str::to_string);
                    let active_flags = active_flags.to_vec();
                    move |db| {
                        let clone_local_history = should_clone_local_branch_history(&source_thread);
                        let created = db::threads::create_thread(
                            db,
                            &source_thread.workspace_id,
                            &source_thread.engine_id,
                            &model_id,
                            title.as_deref().unwrap_or(&source_thread.title),
                        )?;
                        db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
                        if clone_local_history {
                            db::messages::clone_thread_messages(db, &source_thread.id, &created.id)?;
                            if let Some(turns) = rollback_turns {
                                db::messages::drop_last_turns(db, &created.id, turns)?;
                            }
                        }
                        db::threads::refresh_thread_message_stats(
                            db,
                            &created.id,
                            db::threads::ThreadMessageStatsActivityPolicy::FromMessages,
                        )?;

                        let metadata = clone_codex_branch_metadata(
                            source_thread.engine_metadata.as_ref(),
                            &model_id,
                            raw_status.as_deref(),
                            &active_flags,
                            preview.as_deref(),
                            !clone_local_history,
                            (!clone_local_history).then_some("branch_thread_requires_sync"),
                        );
                        let next_status =
                        map_codex_thread_status_to_local(raw_status.as_deref(), &active_flags, false);
                        db::threads::update_thread_runtime_snapshot(
                            db,
                            &created.id,
                            title.as_deref(),
                            next_status,
                            Some(&metadata),
                        )
                    }
            })
                .await?;
            ThreadService::new(state.clone())
                .save_permissions(
                &created.id,
                source_thread.permission_mode.clone().unwrap_or_default(),
            )
                .await
                .map_err(err_to_string)

        }
        pub(crate) async fn thread_save_compaction_notice(&self, thread_id: &str, message_id: Option<&str>, status: &str, error: Option<&str>) -> Result<MessageDto, String> {
            let state = &self.state;

            let thread_id = thread_id.to_string();
            let message_id = message_id.map(str::to_string);
            let status = status.to_string();
            let error = error.map(str::to_string);
            run_db(state.db.clone(), move |db| {
                    let message_id = match message_id {
                        Some(message_id) => message_id,
                        None => db::messages::insert_assistant_placeholder(
                            db,
                            &thread_id,
                            Some("codex"),
                            None,
                            None,
                        )?
                            .id,
                    };
                    let level = match status.as_str() {
                        "failed" => "error",
                        "unconfirmed" => "warning",
                        _ => "info",
                    };
                    let message = match status.as_str() {
                        "in_progress" => "Compacting context...",
                        "completed" => "Context compaction completed.",
                        "failed" => "Context compaction failed.",
                        _ => "Context compaction result is unconfirmed.",
                    };
                    let mut metadata = serde_json::json!({
                            "type": "context_compaction",
                            "status": status,
                    });
                    if let Some(error) = error {
                        metadata["error"] = Value::String(error);
                    }
                    let blocks = serde_json::json!([{
                                "type": "notice",
                                "kind": "context_compaction",
                                "level": level,
                                "title": "Context compaction",
                                "message": message,
                                "metadata": metadata,
                    }]);
                    db::messages::update_assistant_blocks_json(
                        db,
                        &message_id,
                        &serde_json::to_string(&blocks)?,
                        MessageStatusDto::Completed,
                        None,
                    )?;
                    db::messages::get_message(db, &message_id)?
                        .ok_or_else(|| anyhow::anyhow!("压缩状态消息保存后无法读取: {message_id}"))
            })
                .await

        }
        pub(crate) async fn thread_fork_record(&self, thread_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;

            if state.turns.get(&thread_id).await.is_some() {
                return Err("cannot fork a thread while a turn is still active".to_string());
            }

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "codex" {
                return Err("native fork is only available for Codex threads".to_string());
            }
            self.thread_reject_local_only_action( &thread, "会话分支").await?;
            let engine_thread_id = thread
                .engine_thread_id
                .clone()
                .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;
            let (cwd, model_id, sandbox) = self.thread_build_branch_context( &thread).await?;

            let context = self
                .execution_context(Some(thread.workspace_id.as_str()))
                .await
                .map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let forked = cli
                .fork_thread(&context, &thread, &engine_thread_id, &cwd, &model_id, sandbox)
                .await
                .map_err(err_to_string)?;

            self.thread_create_branch(
                &thread,
                &forked.engine_thread_id,
                &forked.model_id,
                forked.title.as_deref(),
                forked.preview.as_deref(),
                forked.raw_status.as_deref(),
                &forked.active_flags,
                None,
            )
                .await

        }
        pub(crate) async fn thread_rollback_record(&self, thread_id: String, num_turns: u32) -> Result<ThreadDto, String> {
            let state = &self.state;

            if num_turns == 0 {
                return Err("rollback requires at least one turn".to_string());
            }
            if state.turns.get(&thread_id).await.is_some() {
                return Err("cannot rollback a thread while a turn is still active".to_string());
            }

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "codex" {
                return Err("native rollback is only available for Codex threads".to_string());
            }
            self.thread_reject_local_only_action( &thread, "回滚").await?;
            let engine_thread_id = thread
                .engine_thread_id
                .clone()
                .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;
            let (cwd, model_id, sandbox) = self.thread_build_branch_context( &thread).await?;

            let context = self
                .execution_context(Some(thread.workspace_id.as_str()))
                .await
                .map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let forked = cli
                .fork_thread(&context, &thread, &engine_thread_id, &cwd, &model_id, sandbox)
                .await
                .map_err(err_to_string)?;
            let rollback_snapshot = match cli
                .rollback_thread(&context, &forked.engine_thread_id, num_turns)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(rollback_error) => {
                    let mut cleanup_thread = thread.clone();
                    cleanup_thread.engine_thread_id = Some(forked.engine_thread_id.clone());
                    if let Err(cleanup_error) = cli
                        .archive_thread(&context, &cleanup_thread, &forked.engine_thread_id)
                        .await
                    {
                        log::warn!(
                            "failed to clean up forked engine thread {} after rollback failure: {cleanup_error}",
                            forked.engine_thread_id
                        );
                    }
                    return Err(err_to_string(rollback_error));
                }
            };

            self.thread_create_branch(
                &thread,
                &forked.engine_thread_id,
                &forked.model_id,
                rollback_snapshot
                    .title
                    .as_deref()
                    .or(forked.title.as_deref()),
                rollback_snapshot
                    .preview
                    .as_deref()
                    .or(forked.preview.as_deref()),
                rollback_snapshot
                    .raw_status
                    .as_deref()
                    .or(forked.raw_status.as_deref()),
                &rollback_snapshot.active_flags,
                Some(num_turns),
            )
                .await

        }
        pub(crate) async fn thread_compact_record(&self, app: AppHandle, thread_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;

            if state.turns.get(&thread_id).await.is_some() {
                return Err("cannot compact a thread while a turn is still active".to_string());
            }

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "codex" {
                return Err("native compact is only available for Codex threads".to_string());
            }
    // SSH 压缩由 CodexCli 按工作区位置分派到远端句柄，不再走仅本机限制。
    // self.thread_reject_local_only_action( &thread, "压缩").await?;
            let engine_thread_id = thread
                .engine_thread_id
                .clone()
                .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;


            let context = self
                .execution_context(Some(thread.workspace_id.as_str()))
                .await
                .map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
    // 旧业务直接调用路径保留迁移留痕，改由持久化消息承载手动压缩状态：
    // cli.compact_thread(&context, &engine_thread_id).await.map_err(err_to_string)?;
    // 手动压缩状态独立持久化为已完成消息，Notice 本身承载实际进行状态。
            let in_progress_message = self.thread_save_compaction_notice(
                &thread_id,
                None,
                "in_progress",
                None,
            )
                .await
                .map_err(|error| {
                    log::error!("Failed to persist Codex compaction start message for thread {thread_id}: {error}");
                    error
            })?;
            let event_name = format!("message-updated-{thread_id}");
            if let Err(error) = app.emit(&event_name, &in_progress_message) {
                log::error!("Failed to emit Codex compaction start message for thread {thread_id}: {error}");
            }

            let compaction_result = match cli.compact_thread(&context, &engine_thread_id).await {
                Ok(result) => result,
                Err(error) => {
                    let status = if error.chain().any(|cause| cause.is::<tokio::time::error::Elapsed>()) {
                "unconfirmed"
                    } else {
                "failed"
                    };
                    let compaction_error = err_to_string(error);
                    match self.thread_save_compaction_notice(
                        &thread_id,
                        Some(&in_progress_message.id),
                        status,
                        Some(&compaction_error),
                    )
                        .await
                    {
                        Ok(message) => {
                            if let Err(error) = app.emit(&event_name, &message) {
                                log::error!("Failed to emit Codex compaction failure message for thread {thread_id}: {error}");
                            }
                        }
                        Err(error) => {
                            log::error!("Failed to persist Codex compaction failure for thread {thread_id}: {error}");
                        }
                    }
                    return Err(compaction_error);
                }
            };

    // 只绑定压缩状态助手消息，避免历史导入为手动压缩重新创建重复通知。
            let binding_thread_id = thread_id.clone();
            let binding_message_id = in_progress_message.id.clone();
            let remote_turn_id = compaction_result.remote_turn_id.clone();
            let binding_error = run_db(state.db.clone(), move |db| {
                    db::messages::bind_message_to_remote_turn(
                        db,
                        &binding_thread_id,
                        &binding_message_id,
                        &remote_turn_id,
                    )
            })
                .await
                .err();
            if let Some(error) = binding_error.as_deref() {
                log::error!("Codex compaction completed but status message binding failed for thread {thread_id}: {error}");
            }

    // 压缩成功后将原消息更新为完成状态，并同步给当前对话视图。
            let completed_message = self.thread_save_compaction_notice(
                &thread_id,
                Some(&in_progress_message.id),
                "completed",
                None,
            )
                .await
                .map_err(|error| {
                    log::error!("Codex compaction completed but state persistence failed for thread {thread_id}: {error}");
                    format!("压缩已完成，状态保存失败：{error}")
            })?;
            if let Err(error) = app.emit(&event_name, &completed_message) {
                log::error!("Failed to emit Codex compaction completion message for thread {thread_id}: {error}");
            }
            if let Some(error) = binding_error {
                return Err(format!("压缩已完成，状态关联失败：{error}"));
            }

    // 返回压缩完成后持久化的上下文快照，前端不再复用压缩前的 token 数量。
            run_db(db, move |db| db::threads::get_thread(db, &thread_id))
                .await?
                .ok_or_else(|| "压缩完成后会话不存在".to_string())

        }
        pub(crate) async fn thread_update_config(&self, thread_id: String, update_personality: bool, personality: Option<String>, update_service_tier: bool, service_tier: Option<String>, update_output_schema: bool, output_schema: Option<Value>) -> Result<ThreadDto, String> {
            let state = &self.state;

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "codex" {
                return Err("Codex thread config is only available for Codex threads".to_string());
            }

            let normalized_personality = if update_personality {
                normalize_thread_personality(personality)?
            } else {
                None
            };
            let normalized_service_tier = if update_service_tier {
                normalize_thread_service_tier(service_tier)?
            } else {
                None
            };
            let normalized_output_schema = if update_output_schema {
                normalize_thread_output_schema(output_schema)?
            } else {
                None
            };

            let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
            if !metadata.is_object() {
                metadata = json!({});
            }

            if let Some(object) = metadata.as_object_mut() {
                if update_personality {
                    match normalized_personality {
                        Some(value) => {
                            object.insert("personality".to_string(), json!(value));
                        }
                        None => {
                            object.remove("personality");
                        }
                    }
                }

                if update_service_tier {
                    match normalized_service_tier {
                        Some(value) => {
                            object.insert("serviceTier".to_string(), json!(value));
                        }
                        None => {
                            object.remove("serviceTier");
                        }
                    }
                }

                if update_output_schema {
                    match normalized_output_schema {
                        Some(value) => {
                            object.insert("outputSchema".to_string(), value);
                        }
                        None => {
                            object.remove("outputSchema");
                        }
                    }
                }
            }

            run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    let metadata = metadata.clone();
                    move |db| db::threads::update_engine_metadata(db, &thread_id, &metadata)
            })
                .await?;

            run_db(db, {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found after Codex config update: {thread_id}"))

        }
    }
}
