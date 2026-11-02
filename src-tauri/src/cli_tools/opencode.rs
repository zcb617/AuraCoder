use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use super::{
    BaseCliMcp, CliExecutionContext, CliForkedThread, CliLocationKind, CliMcpRuntime,
    CliReviewStarted, CliSessionNotFoundError, CliSessionSnapshot, CliTool, McpInvocationContext,
    CliUsageQueryScope, McpToolResult, map_context_usage,
};
use crate::{
    cli_tools::engine_check,
    db,
    engines::{
        approval_response_route_for_engine, capabilities_for_engine, map_engine_capabilities,
        map_model_info, normalize_approval_response_for_engine, opencode::OpenCodeEngine,
        ApprovalRequestRoute, Engine, EngineCapabilities, EngineEvent,
        EngineSteerReceipt, EngineThread, ModelInfo, OpenCodeRemoteSessionSummary, SandboxPolicy,
        ThreadScope, ThreadSyncSnapshot, TurnInput,
    },
    extensions,
    local_cli_service_lifecycle::{LocalCliHandle, LocalCliServiceLifecycle},
    runtime_env,
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, ChatProviderUsageWindowDto, CliContextUsageDto, CodexAppDto,
        CodexPluginDto, CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionItemDto, ExtensionRefreshJob,
        OpenCodeRuntimeCatalogDto,
        EngineCheckResultDto, PermissionComponentJson, SteerReceiptDto, ThreadDto, ThreadStatusDto, WorkspaceDto,
    },
    models::chat::{
        ApprovalResponseOutcomeDto, ApprovalResponseRequest, ChatBusinessError, ChatReviewRequest,
        SendMessageRequest, SteerMessageRequest,
    },
    path_utils, remote_project_opencode_runtime_service, ssh,
    state::AppState,
};

/// OpenCode 对统一 CLI 操作接口的实现。
///
/// 本机项目继续使用现有 OpenCode 引擎；SSH 项目继续使用现有 OpenCode 服务和
/// tunnel 生命周期。远端操作失败时直接返回错误，不会改用本机 OpenCode。
#[derive(Clone)]
pub struct OpenCodeCli {
    /// 当前 OpenCode 共用的 MCP 业务实现。
    mcp: BaseCliMcp,
    /// 当前 CLI 实现使用的应用状态。
    state: AppState,
    /// SSH 当前回合持有的远端 OpenCode 协议客户端。
    remote_turn_use: Arc<Mutex<Option<Arc<OpenCodeEngine>>>>,
    /// 按项目目录缓存本机纯协议 OpenCode 客户端及其服务生命周期代数。
    local_engines: Arc<Mutex<HashMap<String, (u64, Arc<OpenCodeEngine>)>>>,
}

/// 保留目录与当前 workspace 根目录精确匹配的 OpenCode 刷新快照。
pub(crate) fn session_for_refresh(
    session: CliSessionSnapshot,
    expected_cwd: &str,
) -> Option<CliSessionSnapshot> {
    path_utils::paths_equal(&session.cwd, expected_cwd).then_some(session)
}

impl OpenCodeCli {
    pub fn new(state: AppState) -> Self {
        Self::with_mcp_runtime(
            state,
            CliMcpRuntime {
                cli_id: "opencode".to_string(),
                location: CliLocationKind::Local,
            },
        )
    }

    /// 按调用范围生成 OpenCode 的既有不可用额度结果。
    fn opencode_provider_usage(scope: CliUsageQueryScope) -> Option<ChatProviderUsageDto> {
        match scope {
            CliUsageQueryScope::Summary => None,
            CliUsageQueryScope::Explicit => Some(ChatProviderUsageDto {
                engine_id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                available: false,
                windows: Vec::new(),
            }),
        }
    }

    /// 按 Factory 指定的本机或 SSH 运行位置创建 OpenCode MCP 实现。
    pub(crate) fn with_mcp_runtime(state: AppState, runtime: CliMcpRuntime) -> Self {
        let mcp = BaseCliMcp::new(state.clone(), runtime);
        Self {
            mcp,
            state,
            remote_turn_use: Arc::new(Mutex::new(None)),
            local_engines: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 读取 OpenCode 本地扩展快照，并按当前上下文登记一次后台刷新排期。
    async fn load_cached_extension_catalog(
        &self,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        // 用户级目录沿用首次排期；项目目录只读取缓存，由项目业务主动获取。
        // self.schedule_extension_catalog_context(cwd.map(str::to_string)).await?;
        if cwd.is_none() {
            self.schedule_extension_catalog_context(None).await?;
        }
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

    /// 按 OpenCode 原扩展目录实现刷新各类型并写入缓存状态。
    async fn run_extension_catalog_refresh(
        &self,
        app: tauri::AppHandle,
        context_key: String,
        cwd: Option<String>,
        requested_kinds: Vec<String>,
        context: Option<CliExecutionContext>,
    ) {
        // 每种扩展分别调用 OpenCode 原目录实现，并将成功或失败结果写回对应快照。
        for kind in requested_kinds {
            let attempted_at = runtime_env::system_time_rfc3339();
            let context_result = if let Some(context) = context.as_ref() {
                Ok(context.clone())
            } else {
                match cwd.as_deref() {
                    Some(cwd) => self.execution_context_for_cwd(Some(cwd)).await,
                    None => Ok(CliExecutionContext {
                        workspace_id: String::new(),
                        root_path: String::new(),
                        location_kind: CliLocationKind::Local,
                        ssh_connection_id: None,
                    }),
                }
            };
            let refresh_result = match context_result {
                Ok(context) => {
                    self.refresh_extension_catalog(&context, cwd.as_deref(), &[kind.clone()])
                        .await
                }
                Err(error) => Err(error),
            };
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
                        "OpenCode 扩展目录刷新失败: context={context_key} kind={kind} error={error:#}"
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
                    "保存 OpenCode 扩展目录刷新状态失败: context={context_key} kind={kind} error={error:#}"
                );
            }
            // 每种类型落库后立即通知前端刷新对应目录缓存。
            extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
        }

        // 全部类型结束后清除上下文运行标记并发送最终完成事件。
        self.state
            .extension_catalog_refreshes
            .finish(self.id(), &context_key)
            .await;
        extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
    }

    /// 从本机 CLI 生命周期获取 cwd 对应 endpoint，并缓存纯协议 OpenCode 客户端。
    async fn local_engine(&self, cwd: &str) -> Result<Arc<OpenCodeEngine>> {
        let service = LocalCliServiceLifecycle::get("opencode").await?;
        let endpoint = match service.handle() {
            LocalCliHandle::OpenCode(handle) => handle.endpoint_for_cwd(cwd).await?,
            _ => anyhow::bail!("本地 CLI 生命周期返回了错误的 OpenCode 句柄类型"),
        };
        let endpoint_generation = endpoint.generation;
        let endpoint_version = endpoint.version.clone();
        let mut engines = self.local_engines.lock().await;
        if let Some((generation, engine)) = engines.get(cwd) {
            if *generation == endpoint_generation {
                return Ok(engine.clone());
            }
        }
        let engine = Arc::new(OpenCodeEngine::new_local_http(
            endpoint.base_url,
            endpoint.password,
            endpoint_version,
            cwd.to_string(),
            endpoint_generation,
        ));
        // 在缓存协议客户端之前验证服务确实提供 V2 API。
        engine.prewarm().await.inspect_err(|error| {
            log::error!("本机 OpenCode V2 服务检查失败: cwd={cwd}, error={error:#}");
        })?;
        engine.set_computer_control_service(self.state.computer_control_service.clone());
        // engine.set_auracoder_thread_mcp_service(self.state.auracoder_thread_mcp_service.clone());
        engines.insert(cwd.to_string(), (endpoint_generation, engine.clone()));
        Ok(engine)
    }

    /// 配置本机 OpenCode 协议客户端的计算机控制和会话读取服务。
    async fn configure_local_computer_control(&self, cwd: &str) -> Result<Arc<OpenCodeEngine>> {
        self.local_engine(cwd).await
    }

    /// 通过 V2 integration 和 MCP 接口完成指定目录的认证或注销。
    async fn perform_v2_extension_action(&self, engine: &OpenCodeEngine, cwd: &str, item: &ExtensionItemDto, action: &str) -> Result<ExtensionActionResultDto> {
        anyhow::ensure!(item.kind == "mcp" && item.available_actions.iter().any(|value| value == action), "OpenCode 扩展动作不可用");
        // 按名称查找对应 integration，避免操作其他服务的凭证。
        let servers = engine.get_v2_data(cwd, "/api/mcp").await?;
        let server = servers.as_array().context("OpenCode MCP 目录无效")?.iter()
            .find(|server| server.get("name").and_then(Value::as_str) == Some(item.id.as_str()))
            .context("OpenCode MCP 服务不存在")?;
        let integration_id = server.get("integrationID").and_then(Value::as_str).context("OpenCode MCP 服务不支持认证")?;
        let integration_segment = percent_encoding::utf8_percent_encode(integration_id, percent_encoding::NON_ALPHANUMERIC).to_string();
        let name_segment = percent_encoding::utf8_percent_encode(&item.id, percent_encoding::NON_ALPHANUMERIC).to_string();
        let integration_path = format!("/api/integration/{integration_segment}");
        let integration = engine.get_v2_data(cwd, &integration_path).await?;
        if action == "authenticate" {
            // 仅接受唯一 OAuth 方式，表单输入由现有界面无法提供时明确报告。
            let methods = integration.get("methods").and_then(Value::as_array).context("OpenCode integration 缺少认证方式")?;
            let oauth = methods.iter().filter(|method| method.get("type").and_then(Value::as_str) == Some("oauth")).collect::<Vec<_>>();
            anyhow::ensure!(oauth.len() == 1, "OpenCode MCP OAuth 认证方式不存在或不唯一");
            let method = oauth[0];
            anyhow::ensure!(method.get("form").and_then(Value::as_array).map_or(true, Vec::is_empty), "OpenCode MCP 认证需要填写表单: {}", method.get("form").unwrap_or(&Value::Null));
            let method_id = method.get("id").and_then(Value::as_str).context("OpenCode OAuth 缺少 methodID")?;
            let response = engine.send_v2_request(cwd, reqwest::Method::POST, &format!("{integration_path}/connect/oauth"), Some(json!({"methodID": method_id}))).await?;
            let attempt = response.get("data").context("OpenCode OAuth 响应缺少 data")?;
            let instructions = attempt.get("instructions").and_then(Value::as_str).unwrap_or("");
            let attempt_id = attempt.get("attemptID").and_then(Value::as_str).context("OpenCode OAuth 缺少 attemptID")?;
            let attempt_segment = percent_encoding::utf8_percent_encode(attempt_id, percent_encoding::NON_ALPHANUMERIC).to_string();
            if attempt.get("mode").and_then(Value::as_str) != Some("auto") {
                let cancel = engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("{integration_path}/connect/oauth/{attempt_segment}"), None).await;
                if let Err(error) = cancel { log::error!("取消 OpenCode MCP OAuth 失败: {error:#}"); }
                anyhow::bail!("OpenCode MCP 认证需要输入授权码: {instructions}");
            }
            let url = attempt.get("url").and_then(Value::as_str).context("OpenCode OAuth 缺少 URL")?;
            let parsed = reqwest::Url::parse(url).context("OpenCode OAuth URL 无效")?;
            anyhow::ensure!(matches!(parsed.scheme(), "http" | "https"), "OpenCode OAuth URL 协议无效");
            let expires = attempt.pointer("/time/expires").and_then(Value::as_f64);
            // 使用独立参数启动系统浏览器，不经过 shell 解析授权地址。
            #[cfg(target_os = "windows")]
            let mut command = { let mut command = tokio::process::Command::new("rundll32"); command.arg("url.dll,FileProtocolHandler"); command };
            #[cfg(target_os = "macos")]
            let mut command = tokio::process::Command::new("open");
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let mut command = tokio::process::Command::new("xdg-open");
            crate::process_utils::configure_tokio_command(&mut command);
            if let Err(error) = command.arg(url).spawn() {
                let cancel = engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("{integration_path}/connect/oauth/{attempt_segment}"), None).await;
                if let Err(cancel_error) = cancel { log::error!("取消 OpenCode MCP OAuth 失败: {cancel_error:#}"); }
                return Err(error).context("打开 OpenCode OAuth 浏览器失败");
            }
            let started = std::time::Instant::now();
            loop {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as f64;
                if started.elapsed() >= std::time::Duration::from_secs(300) || expires.is_some_and(|value| now >= value) {
                    let cancel = engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("{integration_path}/connect/oauth/{attempt_segment}"), None).await;
                    if let Err(error) = cancel { log::error!("取消 OpenCode MCP OAuth 失败: {error:#}"); }
                    anyhow::bail!("OpenCode MCP OAuth 已过期: {instructions}");
                }
                let status = match engine.get_v2_data(cwd, &format!("{integration_path}/connect/oauth/{attempt_segment}")).await {
                    Ok(status) => status,
                    Err(error) => {
                        let cancel = engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("{integration_path}/connect/oauth/{attempt_segment}"), None).await;
                        if let Err(cancel_error) = cancel { log::error!("取消 OpenCode MCP OAuth 失败: {cancel_error:#}"); }
                        return Err(error).context("读取 OpenCode MCP OAuth 状态失败");
                    }
                };
                match status.get("status").and_then(Value::as_str) {
                    Some("complete") => break,
                    Some("pending") => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                    _ => {
                        let cancel = engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("{integration_path}/connect/oauth/{attempt_segment}"), None).await;
                        if let Err(error) = cancel { log::error!("取消 OpenCode MCP OAuth 失败: {error:#}"); }
                        anyhow::bail!("OpenCode MCP OAuth 失败或状态无效: {status}");
                    }
                }
            }
            engine.send_v2_request(cwd, reqwest::Method::POST, &format!("/api/experimental/mcp/{name_segment}/connect"), None).await?;
        } else if action == "logout" {
            // 只清除此 integration 的存储凭证，环境变量凭证不能删除。
            let connections = integration.get("connections").and_then(Value::as_array).context("OpenCode integration 缺少连接列表")?;
            anyhow::ensure!(!connections.iter().any(|connection| connection.get("type").and_then(Value::as_str) == Some("env")), "OpenCode MCP 环境变量凭证无法删除");
            let credentials = connections.iter().filter(|connection| connection.get("type").and_then(Value::as_str) == Some("credential")).collect::<Vec<_>>();
            anyhow::ensure!(credentials.len() <= 1, "OpenCode MCP 存在多个凭证，无法确定当前连接对应的凭证");
            for connection in credentials {
                if connection.get("type").and_then(Value::as_str) == Some("credential") {
                    let id = connection.get("id").and_then(Value::as_str).context("OpenCode 凭证缺少 id")?;
                    let segment = percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC).to_string();
                    engine.send_v2_request(cwd, reqwest::Method::DELETE, &format!("/api/credential/{segment}"), None).await?;
                }
            }
            engine.send_v2_request(cwd, reqwest::Method::POST, &format!("/api/experimental/mcp/{name_segment}/disconnect"), None).await?;
        } else { anyhow::bail!("OpenCode 扩展动作不支持: {action}"); }
        Ok(ExtensionActionResultDto { provider_id: self.id().to_string(), kind: item.kind.clone(), extension_id: item.id.clone(), action: action.to_string(), requires_new_session: item.requires_new_session })
    }

    /// 根据 workspace 建立 OpenCode 操作目标。
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
        .context("读取 OpenCode workspace 任务失败")??;
        CliExecutionContext::from_workspace(&workspace)
    }

    /// 根据项目目录找到所属 workspace，供 OpenCode 参数和扩展查询使用。
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
        .context("按项目目录读取 OpenCode workspace 任务失败")??;
        match workspace {
            Some(workspace) => CliExecutionContext::from_workspace(&workspace),
            None => self.execution_context(None).await,
        }
    }

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
            "当前 workspace 的项目目录与 OpenCode 操作目标不一致"
        );

        match context.location_kind {
            CliLocationKind::Local => {
                anyhow::ensure!(
                    workspace.location_kind != "ssh",
                    "当前 workspace 是 SSH 远端项目，不能使用本机 OpenCode"
                );
            }
            CliLocationKind::Ssh => {
                anyhow::ensure!(
                    workspace.location_kind == "ssh",
                    "当前 workspace 不是 SSH 远端项目"
                );
                anyhow::ensure!(
                    workspace.ssh_connection_id == context.ssh_connection_id,
                    "当前 workspace 的 SSH 绑定与 OpenCode 操作目标不一致"
                );
            }
        }

        Ok(workspace)
    }

    async fn workspace_roots(&self, workspace: &WorkspaceDto) -> Result<Vec<String>> {
        Ok(vec![workspace.root_path.clone()])
    }

    async fn resolve_workspace_cwd(
        &self,
        workspace: &WorkspaceDto,
        cwd: Option<&str>,
    ) -> Result<String> {
        let requested = cwd
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(workspace.root_path.as_str());
        let roots = self.workspace_roots(workspace).await?;
        roots
            .into_iter()
            .find(|root| path_utils::paths_equal(root, requested))
            .ok_or_else(|| anyhow::anyhow!("OpenCode 项目目录不属于当前 workspace: {requested}"))
    }

    async fn thread_cwd(&self, workspace: &WorkspaceDto, thread: &ThreadDto) -> Result<String> {
        let _ = thread;
        Ok(workspace.root_path.clone())
    }

    async fn list_workspace_sessions(
        &self,
        context: &CliExecutionContext,
        workspace: &WorkspaceDto,
        search_term: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Vec<OpenCodeRemoteSessionSummary>> {
        let roots = self.workspace_roots(workspace).await?;
        let mut sessions = Vec::new();
        if context.location_kind == CliLocationKind::Ssh {
            // 旧实现先取得 Tunnel 临时占用；现在远端服务端由 cli_service_lifecycle
            // 常驻管理，CLI 实现只创建自己的 OpenCode 客户端。
            // let service_use =
            //     remote_project_opencode_runtime_service::acquire_temporary(workspace).await?;
            let engine = remote_project_opencode_runtime_service::runtime(workspace).await?;
            let result = async {
                for cwd in roots.iter() {
                    sessions.extend(engine.list_sessions(cwd, search_term, archived).await?);
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            // service_use.release().await;
            result?;
        } else {
            for cwd in roots.iter() {
                let engine = self.local_engine(cwd).await?;
                sessions.extend(engine.list_sessions(cwd, search_term, archived).await?);
            }
        }

        sessions.retain(|session| {
            roots
                .iter()
                .any(|root| path_utils::paths_equal(root, &session.cwd))
        });
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let mut seen = HashSet::new();
        sessions.retain(|session| seen.insert(session.engine_thread_id.clone()));
        Ok(sessions)
    }

    /// 将 OpenCode 会话摘要映射为统一 CLI 会话快照供刷新和列表展示。
    pub(crate) fn map_session(
        summary: OpenCodeRemoteSessionSummary,
        is_ssh: bool,
    ) -> CliSessionSnapshot {
        let timestamp_to_rfc3339 = |timestamp: i64| {
            let (seconds, nanos) = if timestamp > 10_000_000_000 {
                (timestamp / 1000, ((timestamp % 1000) as u32) * 1_000_000)
            } else {
                (timestamp, 0)
            };
            chrono::DateTime::from_timestamp(seconds, nanos).map(|value| value.to_rfc3339())
        };
        // sync_result?; // 误插入代码，已停用。
        let title = summary
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "OpenCode session {}",
                    summary.engine_thread_id.chars().take(8).collect::<String>()
                )
            });
        let metadata = json!({
            "sshRemote": is_ssh,
            "opencodeRemoteCwd": summary.cwd.clone(),
            "opencodeRemote": {
                "id": summary.engine_thread_id.clone(),
                "title": summary.title.clone(),
                "cwd": summary.cwd.clone(),
                "archived": summary.archived,
                "createdAt": summary.created_at,
                "updatedAt": summary.updated_at,
            },
        });

        CliSessionSnapshot {
            engine_thread_id: summary.engine_thread_id,
            title,
            preview: None,
            cwd: summary.cwd,
            model_id: "unknown".to_string(),
            reasoning_effort: None,
            created_at: timestamp_to_rfc3339(summary.created_at),
            updated_at: timestamp_to_rfc3339(summary.updated_at),
            source_kind: None,
            raw_status: None,
            active_flags: Vec::new(),
            status: ThreadStatusDto::Idle,
            archived: summary.archived,
            metadata,
        }
    }

    fn validate_thread(context: &CliExecutionContext, thread: &ThreadDto) -> Result<()> {
        anyhow::ensure!(
            thread.workspace_id == context.workspace_id,
            "OpenCode 会话不属于当前 workspace"
        );
        anyhow::ensure!(thread.engine_id == "opencode", "当前会话不属于 OpenCode");
        Ok(())
    }

    fn unsupported(action: &str) -> anyhow::Error {
        anyhow::anyhow!("OpenCode 当前不支持{action}")
    }

    /// 根据线程当前保存的统一权限配置生成 OpenCode 本次执行使用的原生规则。
    async fn parse_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<Value> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        anyhow::ensure!(
            thread.workspace_id == context.workspace_id,
            "当前会话不属于该 workspace"
        );
        let permissions = thread.permission_mode.clone().unwrap_or_default();
        permissions.validate()?;

        // OpenCode 无法表达统一权限中的 sandbox 和 network 覆盖，发送前明确拒绝此类配置。
        anyhow::ensure!(
            permissions
                .sandbox
                .first()
                .map_or(true, |value| value == "automatic"),
            "OpenCode 不支持 sandbox 覆盖"
        );
        anyhow::ensure!(
            permissions
                .network
                .first()
                .map_or(true, |value| value == "automatic"),
            "OpenCode 不支持 network 覆盖"
        );

        /*
        // 按当前统一权限配置和 workspace 信任级别生成完整 OpenCode 原生权限数组。
        let trust = match permissions.trust.first().map(String::as_str) {
            Some("automatic") | None => workspace.trust_level.as_str(),
            Some(value) => value,
        };
        let trust_rules = match trust {
            "restricted" => json!([{ "permission": "*", "pattern": "*", "action": "deny" }]),
            "trusted" | "standard" => json!([
                { "permission": "*", "pattern": "*", "action": "ask" },
                { "permission": "question", "pattern": "*", "action": "allow" }
            ]),
            _ => anyhow::bail!("workspace 信任级别不支持 OpenCode 权限映射: {trust}"),
        };
        let rules = match permissions.autonomy_preset.first().map(String::as_str) {
            Some("read-only") => {
                json!([{ "permission": "*", "pattern": "*", "action": "deny" }])
            }
            Some("ask") => json!([
                { "permission": "*", "pattern": "*", "action": "ask" },
                { "permission": "question", "pattern": "*", "action": "allow" }
            ]),
            Some("auto") | Some("full") => {
                json!([{ "permission": "*", "pattern": "*", "action": "allow" }])
            }
            Some("automatic") => trust_rules,
            None => match permissions.approval.first().map(String::as_str) {
                Some("restricted") => {
                    json!([{ "permission": "*", "pattern": "*", "action": "deny" }])
                }
                Some("ask") => json!([
                    { "permission": "*", "pattern": "*", "action": "ask" },
                    { "permission": "question", "pattern": "*", "action": "allow" }
                ]),
                Some("autonomous") => {
                    json!([{ "permission": "*", "pattern": "*", "action": "allow" }])
                }
                Some("automatic") | None => trust_rules,
                Some(value) => anyhow::bail!("OpenCode approval 配置不支持: {value}"),
            },
            Some(value) => anyhow::bail!("OpenCode autonomy preset 不支持: {value}"),
        };
        Ok(json!({ "permission": rules }))
        */
        // 先设置基线，再按 V2 最后匹配规则追加受控动作与外部目录例外。
        let trust = match permissions.trust.first().map(String::as_str) {
            Some("automatic") | None => workspace.trust_level.as_str(),
            Some(value) => value,
        };
        anyhow::ensure!(matches!(trust, "restricted" | "trusted" | "standard"),
            "workspace 信任级别不支持 OpenCode 权限映射: {trust}");
        let mode = match permissions.autonomy_preset.first().map(String::as_str) {
            Some("read-only") => "restricted",
            Some("ask") => "ask",
            Some("auto") => "workspace",
            Some("full") => "full",
            Some("automatic") | None => match permissions.approval.first().map(String::as_str) {
                Some("restricted") => "restricted",
                Some("ask") => "ask",
                Some("autonomous") => "full",
                Some("automatic") | None if trust == "restricted" => "restricted",
                Some("automatic") | None => "ask",
                Some(value) => anyhow::bail!("OpenCode approval 配置不支持: {value}"),
            },
            Some(value) => anyhow::bail!("OpenCode autonomy preset 不支持: {value}"),
        };
        let baseline = match mode { "restricted" => "deny", "ask" => "ask", _ => "allow" };
        let mut rules = vec![json!({ "action": "*", "resource": "*", "effect": baseline })];
        if mode == "restricted" || mode == "ask" {
            // 只放行只读工具和原生表单，其他动作遵循受限基线。
            for action in ["read", "glob", "grep", "question"] {
                rules.push(json!({ "action": action, "resource": "*", "effect": "allow" }));
            }
        }
        if mode == "workspace" {
            rules.push(json!({ "action": "external_directory", "resource": "*", "effect": "ask" }));
        }
        Ok(json!({ "permissions": rules }))
    }

}
#[async_trait]
impl CliTool for OpenCodeCli {
    async fn list_codex_thread_records(&self, _workspace_id: String, _cursor: Option<String>, _limit: Option<u32>, _search_term: Option<String>, _archived: Option<bool>) -> Result<crate::models::CodexRemoteThreadPageDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：list_codex_thread_records")
    }

    async fn attach_thread_record(&self, _workspace_id: String, _engine_thread_id: String) -> Result<ThreadDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：attach_thread_record")
    }

    async fn fork_thread_record(&self, _thread_id: String) -> Result<ThreadDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：fork_thread_record")
    }

    async fn rollback_thread_record(&self, _thread_id: String, _num_turns: u32) -> Result<ThreadDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：rollback_thread_record")
    }

    async fn compact_thread_record(&self, _app: tauri::AppHandle, _thread_id: String) -> Result<ThreadDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：compact_thread_record")
    }

    async fn update_codex_thread_config(&self, _thread_id: String, _update_personality: bool, _personality: Option<String>, _update_service_tier: bool, _service_tier: Option<String>, _update_output_schema: bool, _output_schema: Option<Value>) -> Result<ThreadDto> {
        anyhow::bail!("opencode 不支持此线程业务操作：update_codex_thread_config")
    }

    fn normalize_thread_service_tier(&self, value: Option<String>) -> Result<Option<String>> {
        if value.as_deref().is_some_and(|value| !value.trim().is_empty()) {
            anyhow::bail!("service tier is only supported for Codex threads")
        }
        Ok(None)
    }

    /// 使用同一个 OpenCode 实例委托公共流程完成用户消息发送。
    async fn submit_chat_message(
        self: Arc<Self>,
        app: tauri::AppHandle,
        request: SendMessageRequest,
    ) -> std::result::Result<String, ChatBusinessError> {
        // 将当前实现实例传给公共流程，保持本轮生命周期占用归属不变。
        let cli: Arc<dyn CliTool> = self.clone();
        self.mcp.submit_chat_message(cli, app, request).await
    }

    /// 返回 OpenCode 不支持运行中追加消息的明确业务错误。
    async fn submit_chat_steer(
        self: Arc<Self>,
        _app: tauri::AppHandle,
        _request: SteerMessageRequest,
    ) -> std::result::Result<SteerReceiptDto, ChatBusinessError> {
        Err(ChatBusinessError::new(
            "UNSUPPORTED_OPERATION",
            "OpenCode 暂不支持运行中追加消息",
        ))
    }

    /// 使用同一个 OpenCode 实例委托公共流程处理审批答复。
    async fn answer_chat_approval(
        self: Arc<Self>,
        request: ApprovalResponseRequest,
    ) -> std::result::Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
        // 将当前实现实例传给公共流程，保持审批对应的 CLI 生命周期归属不变。
        let cli: Arc<dyn CliTool> = self.clone();
        self.mcp.answer_chat_approval(cli, request).await
    }

    /// 按当前 OpenCode 线程取消正在执行的聊天回合。
    async fn cancel_chat_turn(
        &self,
        thread_id: &str,
    ) -> std::result::Result<(), ChatBusinessError> {
        self.mcp.cancel_chat_turn(thread_id).await
    }

    /// 通过公共 CLI 服务流程重启当前 OpenCode 聊天服务。
    async fn restart_chat_service(
        &self,
        thread_id: &str,
    ) -> std::result::Result<(), ChatBusinessError> {
        self.mcp.restart_chat_service(self, thread_id).await
    }

    /// 返回 OpenCode 不支持代码审查的明确业务错误。
    async fn submit_chat_review(
        self: Arc<Self>,
        _app: tauri::AppHandle,
        _request: ChatReviewRequest,
    ) -> std::result::Result<ThreadDto, ChatBusinessError> {
        Err(ChatBusinessError::new(
            "UNSUPPORTED_OPERATION",
            "OpenCode 暂不支持代码审查",
        ))
    }

    /// 为 OpenCode 创建仅含公共非权限上下文的沙箱策略，并读取 OpenCode 专属 Agent 配置。
    fn chat_sandbox(
        &self,
        thread: &ThreadDto,
        writable_roots: Vec<String>,
        reasoning_effort: Option<String>,
        _supports_personality: bool,
    ) -> SandboxPolicy {
        // 从当前线程 metadata 读取 OpenCode Agent，其余专属配置不混入公共权限上下文。
        let opencode_agent = thread
            .engine_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("opencodeAgent"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        SandboxPolicy {
            writable_roots,
            allow_network: false,
            auto_approve_mcp_elicitations: false,
            approval_policy: None,
            permission_profile: None,
            approvals_reviewer: None,
            reasoning_effort,
            sandbox_mode: None,
            service_tier: None,
            personality: None,
            output_schema: None,
            opencode_agent,
        }
    }

    /// 按 OpenCode 原生格式规范化审批答复，并将格式问题转为业务错误。
    fn normalize_chat_approval(
        &self,
        response: Value,
    ) -> std::result::Result<Value, ChatBusinessError> {
        // 保留底层完整错误日志，向聊天业务调用方返回稳定的中文校验错误。
        normalize_approval_response_for_engine("opencode", response).map_err(|error| {
            log::error!("OpenCode 审批答复格式校验失败: {error}");
            ChatBusinessError::new("VALIDATION_FAILED", "审批答复格式无效")
        })
    }

    /// 按 OpenCode 既有审批格式解析原生答复路由。
    fn chat_approval_route(
        &self,
        details: &Value,
    ) -> std::result::Result<Option<ApprovalRequestRoute>, ChatBusinessError> {
        Ok(approval_response_route_for_engine("opencode", details))
    }

    /// 根据 OpenCode 审批错误链分类传输不可用或原生操作失败。
    fn classify_chat_approval_error(&self, error: &anyhow::Error) -> ChatBusinessError {
        // 只依据错误链中的 reqwest 传输类型分类，不依据 HTTP 状态或错误文案推断审批状态。
        let transport_unavailable = error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|request_error| {
                    request_error.is_connect() || request_error.is_timeout()
                })
        });
        log::error!("OpenCode 审批原生操作失败: {error:#}");

        if transport_unavailable {
            ChatBusinessError::new("CLI_UNAVAILABLE", "OpenCode 审批服务暂时不可用")
        } else {
            ChatBusinessError::new("CHAT_OPERATION_FAILED", "OpenCode 审批操作失败")
        }
    }

    /// OpenCode 不需要对共享聊天事件执行额外副作用。
    async fn on_chat_event(
        &self,
        _thread: &ThreadDto,
        _assistant_message_id: &str,
        _event: &EngineEvent,
    ) -> anyhow::Result<()> {
        Ok(())
    }


    async fn list_opencode_thread_records(&self, workspace_id: String, cursor: Option<String>, limit: Option<u32>, search_term: Option<String>, archived: Option<bool>) -> Result<crate::models::OpenCodeRemoteSessionPageDto> {
        self.thread_list_records(workspace_id, cursor, limit, search_term, archived).await.map_err(anyhow::Error::msg)
    }

    async fn attach_opencode_thread_record(&self, workspace_id: String, engine_thread_id: String, cwd: String, model_id: String) -> Result<ThreadDto> {
        self.thread_attach_record(workspace_id, engine_thread_id, cwd, model_id).await.map_err(anyhow::Error::msg)
    }

    async fn update_opencode_thread_config(&self, thread_id: String, update_agent: bool, agent: Option<String>) -> Result<ThreadDto> {
        self.thread_update_config(thread_id, update_agent, agent).await.map_err(anyhow::Error::msg)
    }


    async fn create_thread_record(&self, workspace_id: String, model_id: String, title: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> Result<ThreadDto> {
        self.mcp.create_thread_record(self, "opencode", workspace_id, model_id, title, reasoning_effort, service_tier).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=create_thread_record error={error:#}"))
    }

    async fn reconfigure_thread_record(&self, thread_id: String, model_id: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> Result<ThreadDto> {
        self.mcp.reconfigure_thread_record(self, "opencode", thread_id, model_id, reasoning_effort, service_tier).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=reconfigure_thread_record error={error:#}"))
    }

    async fn set_thread_reasoning(&self, thread_id: String, reasoning_effort: Option<String>, model_id: Option<String>) -> Result<()> {
        self.mcp.set_thread_reasoning(self, "opencode", thread_id, reasoning_effort, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=set_thread_reasoning error={error:#}"))
    }

    async fn set_thread_selected_model(&self, thread_id: String, model_id: String) -> Result<ThreadDto> {
        self.mcp.set_thread_selected_model(self, "opencode", thread_id, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=set_thread_selected_model error={error:#}"))
    }

    async fn delete_thread_record(&self, thread_id: String) -> Result<()> {
        self.mcp.delete_thread_record(self, "opencode", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=delete_thread_record error={error:#}"))
    }

    async fn archive_thread_record(&self, thread_id: String) -> Result<()> {
        self.mcp.archive_thread_record(self, "opencode", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=archive_thread_record error={error:#}"))
    }

    async fn restore_thread_record(&self, thread_id: String) -> Result<ThreadDto> {
        self.mcp.restore_thread_record(self, "opencode", thread_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=restore_thread_record error={error:#}"))
    }

    async fn validate_thread_model(&self, workspace_id: Option<&str>, model_id: &str) -> Result<String> {
        self.mcp.validate_thread_model(self, "opencode", workspace_id, model_id).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=validate_thread_model error={error:#}"))
    }

    async fn validate_thread_reasoning(&self, model_id: &str, reasoning_effort: &str) -> Result<String> {
        self.mcp.validate_thread_reasoning(self, "opencode", model_id, reasoning_effort).await.inspect_err(|error| log::error!("CLI 线程业务失败: cli_id=opencode operation=validate_thread_reasoning error={error:#}"))
    }

    /// 校验并规范化 OpenCode 定时任务运行配置。
    async fn prepare_scheduled_runtime_config(
        &self,
        runtime: crate::models::ScheduledRuntimeConfig,
    ) -> Result<crate::models::ScheduledRuntimeConfig> {
        let result: Result<crate::models::ScheduledRuntimeConfig> = async {
            // 使用公共流程校验当前 OpenCode 模型和推理强度。
            let mut runtime = self
                .mcp
                .prepare_scheduled_model_config(self, runtime)
                .await?;

            // 延续非 Codex 定时任务清空服务档位的既有语义。
            runtime.service_tier = None;
            Ok(runtime)
        }
        .await;
        result
            .context("校验 OpenCode 定时任务运行配置失败")
            .inspect_err(|error| {
                log::error!("CLI 定时任务运行配置校验失败: cli_id=opencode operation=prepare_scheduled_runtime_config error={error:#}")
            })
    }

    async fn sync_thread_record(&self, thread_id: String) -> Result<ThreadDto> {
        self.thread_sync_record(thread_id).await.map_err(anyhow::Error::msg)
    }


    fn id(&self) -> &str {
        "opencode"
    }

    fn name(&self) -> &str {
        "OpenCode"
    }

    fn capabilities(&self) -> EngineCapabilities {
        capabilities_for_engine("opencode")
    }

    /// 返回 OpenCode 当前运行位置可用的 MCP 工具目录。
    fn list_mcp_tools(&self) -> Result<Vec<Value>, String> {
        self.mcp.list_mcp_tools()
    }

    /// 将 OpenCode MCP 调用委托给公共 BaseCliMcp 实现。
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

    /// 为当前 OpenCode 对话轮次登记可信 AuraCoder MCP 上下文，并按项目位置调用对应生命周期。
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

    /// 清理当前 OpenCode 对话轮次的可信 AuraCoder MCP 上下文，并保留生命周期原始错误链。
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

    /// 重启当前 OpenCode 的 SSH 远端 CLI 服务，严格按 terminate 后 set 的顺序执行。
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

    /// 同步会话完成后，关闭该项目对应的 OpenCode serve 进程，不常驻。
    async fn close_workspace_service(&self, context: &CliExecutionContext) -> Result<()> {
        if context.location_kind == CliLocationKind::Local {
            let service = LocalCliServiceLifecycle::get(self.id()).await?;
            if let crate::local_cli_service_lifecycle::LocalCliHandle::OpenCode(handle) =
                service.handle()
            {
                handle.terminate_cwd(&context.root_path).await?;
            }
        }
        Ok(())
    }

    /// 查询当前 OpenCode 服务是否已经由本机或 SSH CLI 生命周期登记并处于 Ready 状态。
    async fn is_service_ready(&self, context: &CliExecutionContext) -> Result<bool> {
        match context.location_kind {
            CliLocationKind::Local => Ok(LocalCliServiceLifecycle::get(self.id()).await.is_ok()),
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端 OpenCode 项目未绑定连接")?;
                Ok(ssh::cli_service_lifecycle::get(connection_id, self.id())
                    .await
                    .is_ok())
            }
        }
    }

    /// 通过 OpenCode 对应 CLI 生命周期取得或确保当前运行位置的服务，不直接管理 SSH Tunnel。
    async fn ensure_service(&self, context: &CliExecutionContext) -> Result<()> {
        match context.location_kind {
            CliLocationKind::Local => {
                LocalCliServiceLifecycle::set(self.id()).await?;
            }
            CliLocationKind::Ssh => {
                let connection_id = context
                    .ssh_connection_id
                    .as_deref()
                    .context("SSH 远端 OpenCode 项目未绑定连接")?;
                ssh::cli_service_lifecycle::set(connection_id, self.id()).await?;
            }
        }
        Ok(())
    }

    /// SSH 连接测试成功后，由 OpenCode CLI 生命周期建立 Tunnel 并登记、启动远端服务。
    async fn register_remote_service(&self, context: &CliExecutionContext) -> Result<()> {
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Ssh,
            "本机 OpenCode CLI 不支持注册远端服务"
        );
        let connection_id = context
            .ssh_connection_id
            .as_deref()
            .context("SSH 远端 OpenCode 注册服务未绑定连接")?;
        let lookup_connection_id = connection_id.to_string();
        let record = tokio::task::spawn_blocking({
            let db = self.state.db.clone();
            move || db::ssh_connections::find(&db, &lookup_connection_id)
        })
        .await
        .context("读取 SSH OpenCode 连接记录任务失败")?
        .context("读取 SSH OpenCode 连接记录数据库失败")?
        .with_context(|| format!("SSH OpenCode 连接记录不存在: connection_id={connection_id}"))?;
        ssh::cli_service_lifecycle::register_service(&record, self.id()).await
    }

    /// 通过 SSH CLI 生命周期注销当前连接的 OpenCode 服务，不直接管理隧道资源。
    async fn unregister_remote_service(&self, context: &CliExecutionContext) -> Result<()> {
        // 仅 SSH 项目允许显式注销远端 CLI 服务，并要求调用方提供对应连接标识。
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Ssh,
            "本机 OpenCode CLI 不支持注销远端服务"
        );
        let connection_id = context
            .ssh_connection_id
            .as_deref()
            .context("SSH 远端 OpenCode 注销服务未绑定连接")?;

        ssh::cli_service_lifecycle::unregister_service(connection_id, self.id()).await
    }

    async fn execution_context(&self, workspace_id: Option<&str>) -> Result<CliExecutionContext> {
        OpenCodeCli::execution_context(self, workspace_id).await
    }

    async fn execution_context_for_cwd(&self, cwd: Option<&str>) -> Result<CliExecutionContext> {
        OpenCodeCli::execution_context_for_cwd(self, cwd).await
    }

    async fn get_engine_info(&self, context: &CliExecutionContext) -> Result<EngineInfoDto> {
        /*
        旧实现通过 workspace.root_path 读取远端模型，导致机器级模型目录携带项目路径，
        不再执行：
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let models = remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime_for_cwd(&workspace.root_path)
                .await;
            anyhow::ensure!(!models.is_empty(), "SSH OpenCode 未返回可用模型");
            return Ok(EngineInfoDto {
                id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("opencode")),
            });
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 OpenCode 未绑定连接")?;
            let models =
                remote_project_opencode_runtime_service::model_infos(connection_id, None).await?;
            return Ok(EngineInfoDto {
                id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("opencode")),
            });
        }

        /*
        let models = if context.root_path.trim().is_empty() {
            let service = LocalCliServiceLifecycle::get("opencode").await?;
            let handle = match service.handle() {
                LocalCliHandle::OpenCode(handle) => handle,
                _ => anyhow::bail!("本地 CLI 生命周期返回了错误的 OpenCode 句柄类型"),
            };
            let verbose_models = match handle.run_command(None, &["models", "--verbose"]).await {
                Ok(output) => match OpenCodeEngine::parse_cli_model_output(&output) {
                    Ok(models) => models,
                    Err(error) => {
                        log::warn!(
                            "解析本机 OpenCode verbose 模型目录失败，回退 basic 输出: {error:#}"
                        );
                        Vec::new()
                    }
                },
                Err(error) => {
                    log::warn!(
                        "读取本机 OpenCode verbose 模型目录失败，回退 basic 输出: {error:#}"
                    );
                    Vec::new()
                }
            };
            if !verbose_models.is_empty() {
                verbose_models
            } else {
                let basic_models = match handle.run_command(None, &["models"]).await {
                    Ok(output) => match OpenCodeEngine::parse_cli_model_output(&output) {
                        Ok(models) => models,
                        Err(error) => {
                            log::warn!("解析本机 OpenCode basic 模型目录失败: {error:#}");
                            Vec::new()
                        }
                    },
                    Err(error) => {
                        log::warn!("读取本机 OpenCode basic 模型目录失败: {error:#}");
                        Vec::new()
                    }
                };
                if basic_models.is_empty() {
                    OpenCodeEngine::default().models()
                } else {
                    basic_models
                }
            }
        } else {
            self.local_engine(&context.root_path)
                .await?
                .list_models_runtime()
                .await
        };
        */
        // 模型目录统一通过当前运行位置的 V2 API 获取。
        let models = self.local_engine(&context.root_path).await?
            .model_infos(&context.root_path).await
            .inspect_err(|error| log::error!("读取本机 OpenCode V2 模型失败: {error:#}"))
            .context("读取本机 OpenCode V2 模型失败")?;
        Ok(EngineInfoDto {
            id: "opencode".to_string(),
            name: "OpenCode".to_string(),
            models: models.into_iter().map(map_model_info).collect(),
            capabilities: map_engine_capabilities(capabilities_for_engine("opencode")),
        })
    }

    async fn models_for_validation(
        &self,
        context: &CliExecutionContext,
        requested_model_id: &str,
    ) -> Result<Vec<ModelInfo>> {
        /*
        旧实现通过 workspace.root_path 读取远端模型，不再执行：
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let models = remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime_for_cwd(&workspace.root_path)
                .await;
            anyhow::ensure!(!models.is_empty(), "SSH OpenCode 未返回可用模型");
            return Ok(models);
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 OpenCode 未绑定连接")?;
            return remote_project_opencode_runtime_service::model_infos(connection_id, None).await;
        }
        // 校验使用实时 V2 模型目录，失败时保留原始错误。
        let _ = requested_model_id;
        self.local_engine(&context.root_path).await?
            .model_infos(&context.root_path).await
            .inspect_err(|error| log::error!("读取 OpenCode V2 模型校验目录失败: {error:#}"))
            .context("读取 OpenCode V2 模型校验目录失败")
    }

    /// 用户查看 OpenCode 用量时，只对显式查询返回既有不可用结果。
    async fn get_chat_provider_usage(
        &self,
        context: &CliExecutionContext,
        scope: CliUsageQueryScope,
    ) -> Result<Option<ChatProviderUsageDto>> {
        // 摘要查询不主动请求；显式查询使用当前本机或 SSH 服务。
        if matches!(scope, CliUsageQueryScope::Summary) { return Ok(None); }
        let engine = if context.location_kind == CliLocationKind::Ssh {
            let workspace = self.load_workspace(context).await?;
            remote_project_opencode_runtime_service::runtime(&workspace).await?
        } else {
            self.local_engine(&context.root_path).await?
        };
        let data = engine.get_v2_data(&context.root_path, "/api/experimental/usage/limits").await
            .inspect_err(|error| log::error!("读取 OpenCode V2 用量失败: {error:#}"))?;
        let mut windows = Vec::new();
        if let Some(go) = data.get("go") {
            for kind in ["rolling", "weekly", "monthly"] {
                let meter = go.get(kind).context("OpenCode V2 用量窗口缺失")?;
                let percent = meter.get("percent").and_then(Value::as_f64).context("OpenCode V2 用量百分比缺失")?;
                let reset = meter.get("resetsAt").and_then(Value::as_f64).context("OpenCode V2 用量重置时间缺失")?;
                windows.push(ChatProviderUsageWindowDto {
                    kind: kind.to_string(),
                    used_percent: percent.clamp(0.0, 100.0).round() as u8,
                    resets_at: Some((reset / 1000.0) as i64),
                });
            }
        }
        Ok(Some(ChatProviderUsageDto { engine_id: "opencode".to_string(), name: "OpenCode".to_string(), available: !data.is_null(), windows }))
    }

    /// 用户进入 OpenCode 线程时，读取模型上下文上限和最新可靠 assistant token 快照。
    async fn get_context_usage(
        &self,
        thread_id: &str,
    ) -> Result<Option<CliContextUsageDto>> {
        // 按数据库线程归属读取执行上下文，再沿用 OpenCode 的运行时上下文快照。
        let (context, thread) = self
            .mcp
            .context_for_thread(self.id(), thread_id)
            .await
            .inspect_err(|error| {
                log::error!(
                    "读取 OpenCode 线程上下文失败: cli_id={} thread_id={} error={error:#}",
                    self.id(),
                    thread_id
                )
            })
            .context("读取 OpenCode 线程上下文失败")?;
        let context = &context;
        let thread = &thread;

        let workspace = self.load_workspace(context).await?;
        let engine_thread_id = thread
            .engine_thread_id
            .as_deref()
            .context("OpenCode 线程缺少 CLI 会话标识")?;
        let usage = if context.location_kind == CliLocationKind::Ssh {
            context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 OpenCode 未绑定连接")?;
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .context_usage_snapshot(&context.root_path, engine_thread_id)
                .await?
        } else {
            self.local_engine(&context.root_path)
                .await?
                .context_usage_snapshot(&context.root_path, engine_thread_id)
                .await?
        };
        Ok(usage.and_then(|(current_tokens, max_context_tokens)| {
            map_context_usage(Some(current_tokens), Some(max_context_tokens))
        }))
    }

    /// 查询已登记 OpenCode 在当前运行位置的就绪状态和模型目录。
    async fn get_active_engine_info(
        &self,
        context: &CliExecutionContext,
    ) -> Result<Option<EngineInfoDto>> {
        self.mcp
            .active_engine_info(self, context)
            .await
            .inspect_err(|error| {
                log::error!(
                    "读取 OpenCode 活动模型目录失败: cli_id={} error={error:#}",
                    self.id()
                )
            })
    }

    /// 按 OpenCode 当前模型目录为尚未开始的线程准备运行时选择。
    async fn prepare_unstarted_thread_runtime(
        &self,
        current_model: &str,
        model_id: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Result<crate::models::ThreadRuntimeSelection> {
        // 通过现有活动模型接口读取本机运行时，不启动新的 OpenCode CLI。
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

    /// 执行 OpenCode 健康报告中按原始标签登记的 shell 或 HTTP 诊断动作。
    async fn run_engine_check(&self, command: &str) -> Result<EngineCheckResultDto> {
        let result: Result<EngineCheckResultDto> = async {
            // 仅从当前 OpenCode 引擎健康报告读取动作，不创建或启动引擎。
            let context = self.execution_context(None).await?;
            let engine = self.local_engine(&context.root_path).await?;
            let report = engine.health_report().await;
            let action = engine_check::find_action(&report.actions, command)?;
            match action {
                crate::engines::EngineCheckAction::Shell(shell_command) => {
                    engine_check::execute_shell(&shell_command).await
                }
                crate::engines::EngineCheckAction::NativeHealth(native_command) => {
                    let started_at = std::time::Instant::now();
                    match engine.prewarm().await {
                        Ok(()) => Ok(EngineCheckResultDto {
                            command: native_command,
                            success: true,
                            exit_code: None,
                            stdout: "OpenCode HTTP 健康检查通过".to_string(),
                            stderr: String::new(),
                            duration_ms: started_at.elapsed().as_millis(),
                        }),
                        Err(error) => {
                            log::warn!(
                                "OpenCode HTTP 健康检查失败: cli_id={} command={command:?} error={error:#}",
                                self.id()
                            );
                            Ok(EngineCheckResultDto {
                                command: native_command,
                                success: false,
                                exit_code: None,
                                stdout: String::new(),
                                stderr: format!("{error:#}"),
                                duration_ms: started_at.elapsed().as_millis(),
                            })
                        }
                    }
                }
                crate::engines::EngineCheckAction::Information(_) => {
                    anyhow::bail!("OpenCode 健康报告中的说明不能执行")
                }
            }
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "执行 OpenCode 诊断动作失败: cli_id={} command={command:?} error={error:#}",
                self.id()
            );
        }
        result.with_context(|| format!("执行 OpenCode 诊断动作失败: cli_id={}", self.id()))
    }

    async fn engine_health(&self, context: &CliExecutionContext) -> Result<EngineHealthDto> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Local {
            let report = self
                .local_engine(&context.root_path)
                .await?
                .health_report()
                .await;
            return Ok(EngineHealthDto {
                id: "opencode".to_string(),
                available: report.available,
                version: report.version,
                details: report.details,
                warnings: report.warnings,
                checks: report.checks,
                fixes: report.fixes,
                protocol_diagnostics: None,
            });
        }

        let connection_id =
            remote_project_opencode_runtime_service::validate_remote_opencode_workspace(
                &workspace,
            )?
            .to_string();
        let db = self.state.db.clone();
        let lookup_connection_id = connection_id.clone();
        let connection = tokio::task::spawn_blocking(move || {
            db::ssh_connections::find(&db, &lookup_connection_id)
        })
        .await
        .context("读取 SSH 连接任务失败")??
        .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在: {connection_id}"))?;
        let availability = match remote_project_opencode_runtime_service::runtime(&workspace).await
        {
            Ok(engine) => engine.prewarm().await,
            Err(error) => Err(error),
        };
        let version = if availability.is_ok() {
            let command = ssh::runtime::wrap_remote_login_shell_command("opencode --version");
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
            id: "opencode".to_string(),
            available: availability.is_ok(),
            version,
            details: Some(match availability {
                Ok(()) => format!("SSH 远端 OpenCode：{connection_name}"),
                Err(error) => format!("SSH 远端 OpenCode 不可用：{error:#}"),
            }),
            warnings: Vec::new(),
            checks: Vec::new(),
            fixes: Vec::new(),
            protocol_diagnostics: None,
        })
    }

    /// OpenCode 当前没有独立的桌面运行时事件流，完成统一监听入口。
    async fn run_runtime_event_bridge(&self, _app: tauri::AppHandle) -> Result<()> {
        Ok(())
    }

    async fn prewarm_engine(&self, context: &CliExecutionContext) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .prewarm()
                .await
        } else {
            self.local_engine(&context.root_path).await?.prewarm().await
        }
    }

    async fn uses_external_sandbox(&self, context: &CliExecutionContext) -> Result<bool> {
        self.load_workspace(context).await?;
        Ok(false)
    }

    async fn list_sessions(
        &self,
        context: &CliExecutionContext,
        search_term: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Vec<CliSessionSnapshot>> {
        let workspace = self.load_workspace(context).await?;
        let summaries = self
            .list_workspace_sessions(context, &workspace, search_term, archived)
            .await?;
        let is_ssh = context.location_kind == CliLocationKind::Ssh;
        Ok(summaries
            .into_iter()
            .map(|summary| Self::map_session(summary, is_ssh))
            .collect())
    }

    /// 刷新 OpenCode 项目会话，并在所有刷新结果后关闭该项目的临时服务。
    async fn refresh_workspace_sessions(&self, context: &CliExecutionContext) -> Result<()> {
        // 在结果块内完成 workspace 读取、CLI 列表过滤和 Mapper 持久化，以便外层总能收尾。
        let refresh_result = async {
            let workspace = self.load_workspace(context).await?;
            let sessions = self.list_sessions(context, None, Some(false)).await?;
            let sessions = sessions
                .into_iter()
                .filter_map(|session| session_for_refresh(session, &workspace.root_path))
                .collect::<Vec<_>>();

            // 使用既有远端线程 Mapper 保存会话快照，空结果保持本地旧记录。
            for session in sessions {
                let db = self.state.db.clone();
                let workspace_id = workspace.id.clone();
                tokio::task::spawn_blocking(move || {
                    db::threads::upsert_ssh_remote_thread_snapshot(
                        &db,
                        &workspace_id,
                        "opencode",
                        &session.engine_thread_id,
                        &session.model_id,
                        &session.title,
                        session.status,
                        &session.metadata,
                        session.reasoning_effort.as_deref(),
                        session.updated_at.as_deref(),
                    )
                })
                .await
                .context("persist remote session task failed")??;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        // 无论读取或落库结果如何，都执行现有的 workspace 服务收尾。
        let cleanup_result = self.close_workspace_service(context).await;
        match (refresh_result, cleanup_result) {
            (Err(refresh_error), Err(cleanup_error)) => {
                log::warn!(
                    "OpenCode workspace 服务收尾失败: workspace_id={} error={cleanup_error:#}; 刷新原始错误: {refresh_error:#}",
                    context.workspace_id
                );
                Err(refresh_error)
            }
            (Err(refresh_error), Ok(())) => Err(refresh_error),
            (Ok(()), Err(cleanup_error)) => {
                log::warn!(
                    "OpenCode workspace 服务收尾失败，刷新数据已提交: workspace_id={} error={cleanup_error:#}",
                    context.workspace_id
                );
                Ok(())
            }
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    async fn read_session(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<CliSessionSnapshot> {
        let workspace = self.load_workspace(context).await?;
        // 旧实现仅作架构迁移留痕，禁止恢复执行：
        // let summary = self
        //     .list_workspace_sessions(context, &workspace, None, None)
        //     .await?
        //     .into_iter()
        //     .find(|session| session.engine_thread_id == engine_thread_id)
        //     .ok_or_else(|| {
        //         anyhow::anyhow!(
        //             "OpenCode 会话不属于当前 workspace 或已不存在: {engine_thread_id}"
        //         )
        //     })?;
        // SSH 只能通过 CLI Service Lifecycle 取得 OpenCode 客户端，并且只发一次按 ID 请求。
        if context.location_kind != CliLocationKind::Ssh {
            // 本机 OpenCode 允许 workspace 根目录和各仓库目录分别拥有会话；只有明确的
            // 404 才继续尝试下一个目录，其他错误必须原样返回，不能误报为“会话不存在”。
            let roots = self.workspace_roots(&workspace).await?;
            for cwd in roots.iter() {
                let engine = self.local_engine(cwd).await?;
                match engine.read_session(cwd, engine_thread_id).await {
                    Ok(summary) => {
                        anyhow::ensure!(
                            summary.engine_thread_id == engine_thread_id,
                            "OpenCode 返回了错误的会话 ID: expected={} actual={}",
                            engine_thread_id,
                            summary.engine_thread_id
                        );
                        anyhow::ensure!(
                            roots
                                .iter()
                                .any(|root| path_utils::paths_equal(root, &summary.cwd)),
                            "OpenCode 会话目录不属于当前 workspace: {}",
                            summary.cwd
                        );
                        return Ok(Self::map_session(summary, false));
                    }
                    Err(error) => {
                        let is_not_found = error
                            .downcast_ref::<reqwest::Error>()
                            .and_then(|cause| cause.status())
                            .is_some_and(|status| status == reqwest::StatusCode::NOT_FOUND);
                        if !is_not_found {
                            return Err(error);
                        }
                    }
                }
            }
            // 所有候选目录均确认返回 404，交给公共恢复编排识别为会话不存在。
            // 迁移留痕：旧实现把“未找到”作为普通业务错误返回，恢复编排无法区分 404：
            // anyhow::bail!("OpenCode 会话不属于当前 workspace 或已不存在: {engine_thread_id}");
            return Err(CliSessionNotFoundError::new("opencode", engine_thread_id).into());
        }

        // 迁移留痕：旧实现直接使用 `.await?`，会把 SSH 404 原样暴露给公共恢复编排：
        // let summary = remote_project_opencode_runtime_service::runtime(&workspace)
        //     .await?
        //     .read_session(&workspace.root_path, engine_thread_id)
        //     .await?;
        let summary = match remote_project_opencode_runtime_service::runtime(&workspace)
            .await?
            .read_session(&workspace.root_path, engine_thread_id)
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                // SSH 只允许这一次按 ID 请求；仅确认 HTTP 404 时映射公共 NotFound，
                // 连接、解析和其他 HTTP 错误必须原样返回，不能回退到本机或列表查询。
                let is_not_found = error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(|cause| cause.status())
                    .is_some_and(|status| status == reqwest::StatusCode::NOT_FOUND);
                if is_not_found {
                    return Err(CliSessionNotFoundError::new("opencode", engine_thread_id).into());
                }
                return Err(error);
            }
        };
        anyhow::ensure!(
            summary.engine_thread_id == engine_thread_id,
            "OpenCode 返回了错误的会话 ID: expected={} actual={}",
            engine_thread_id,
            summary.engine_thread_id
        );
        let roots = self.workspace_roots(&workspace).await?;
        anyhow::ensure!(
            roots
                .iter()
                .any(|root| path_utils::paths_equal(root, &summary.cwd)),
            "OpenCode 会话目录不属于当前 workspace: {}",
            summary.cwd
        );
        Ok(Self::map_session(
            summary,
            context.location_kind == CliLocationKind::Ssh,
        ))
    }

    async fn acquire_turn(&self, context: &CliExecutionContext, thread: &ThreadDto) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        if context.location_kind == CliLocationKind::Ssh {
            *self.remote_turn_use.lock().await =
                Some(remote_project_opencode_runtime_service::runtime(&workspace).await?);
        }
        Ok(())
    }

    /// 解析当前线程权限并在本机或 SSH OpenCode 引擎中建立 session。
    async fn start_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        scope: ThreadScope,
        resume_engine_thread_id: Option<&str>,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<EngineThread> {
        let permissions = self.parse_permissions(context, thread).await?;
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        // 将当前线程转换出的 OpenCode 原生规则传入 session 建立流程。
        let permission_rules = permissions
            .get("permissions")
            .cloned()
            .context("OpenCode 原生权限规则缺失")?;
        anyhow::ensure!(permission_rules.is_array(), "OpenCode 原生权限规则必须是数组");
        let mut sandbox = sandbox;
        sandbox.approval_policy = Some(permission_rules);
        let scope_cwd = match &scope {
            ThreadScope::Project { root_path, .. } => root_path.as_str(),
        };
        self.resolve_workspace_cwd(&workspace, Some(scope_cwd))
            .await?;
        if context.location_kind == CliLocationKind::Ssh {
            let remote_turn_use = self.remote_turn_use.lock().await;
            let engine = remote_turn_use.as_ref().ok_or_else(|| {
                anyhow::anyhow!("当前 SSH 远端 OpenCode 会话尚未建立持续使用关系")
            })?;
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

        let engine = self.configure_local_computer_control(scope_cwd).await?;
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

    /// 每次发送前解析当前线程权限，并将原生规则交给本机或 SSH OpenCode 引擎。
    async fn send_message(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let mut input = input;
        // 每次发送都从线程当前 A 权限重新解析 OpenCode 原生规则并交给引擎。
        input.permissions = Some(self.parse_permissions(context, thread).await?);
        if context.location_kind == CliLocationKind::Ssh {
            let engine = self.remote_turn_use.lock().await.take().ok_or_else(|| {
                anyhow::anyhow!("当前 SSH 远端 OpenCode 会话尚未建立持续使用关系")
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

        let cwd = self.thread_cwd(&workspace, thread).await?;
        let engine = self.configure_local_computer_control(&cwd).await?;
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
        _engine_thread_id: &str,
        _client_steer_id: &str,
        _content: &str,
        _input: TurnInput,
    ) -> Result<EngineSteerReceipt> {
        self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        Err(Self::unsupported("运行中补充消息"))
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
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let engine = remote_project_opencode_runtime_service::runtime(&workspace).await?;
            Engine::respond_to_approval(engine.as_ref(), approval_id, response, route)
                .await
                .with_context(|| {
                    format!(
                        "SSH 远端 OpenCode 审批或问题回复失败: thread_id={}",
                        thread.id
                    )
                })
        } else {
            Engine::respond_to_approval(
                self.local_engine(&cwd).await?.as_ref(),
                approval_id,
                response,
                route,
            )
            .await
        }
    }

    async fn interrupt(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let Some(actual_engine_thread_id) = thread.engine_thread_id.as_deref() else {
            return Ok(());
        };
        anyhow::ensure!(
            actual_engine_thread_id == engine_thread_id,
            "OpenCode 会话标识与当前会话不一致"
        );
        let cwd = self.thread_cwd(&workspace, thread).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .abort_session(&cwd, actual_engine_thread_id)
                .await
        } else {
            let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
                return Ok(());
            };
            Engine::interrupt(self.local_engine(&cwd).await?.as_ref(), engine_thread_id).await
        }
    }

    async fn archive_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .set_session_archived(&cwd, engine_thread_id, true)
                .await
        } else {
            self.local_engine(&cwd)
                .await?
                .set_session_archived(&cwd, engine_thread_id, true)
                .await
        }
    }

    async fn unarchive_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .set_session_archived(&cwd, engine_thread_id, false)
                .await
        } else {
            self.local_engine(&cwd)
                .await?
                .set_session_archived(&cwd, engine_thread_id, false)
                .await
        }
    }

    async fn forget_session(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        let _active_turn_engine = self.remote_turn_use.lock().await.take();
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .forget_session(engine_thread_id)
                .await;
        } else {
            self.local_engine(&cwd)
                .await?
                .forget_session(engine_thread_id)
                .await;
        }
        Ok(())
    }

    async fn read_thread_preview(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        _engine_thread_id: &str,
    ) -> Result<Option<String>> {
        self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        Ok(None)
    }

    async fn read_thread_sync_snapshot(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<Option<ThreadSyncSnapshot>> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        let snapshot = if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .read_thread_sync_snapshot(&cwd, engine_thread_id)
                .await?
        } else {
            self.local_engine(&cwd)
                .await?
                .read_thread_sync_snapshot(&cwd, engine_thread_id)
                .await?
        };
        Ok(Some(snapshot))
    }

    /// 将会话标题同步到所属本机或 SSH OpenCode V2 服务。
    async fn set_thread_name(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        name: &str,
    ) -> Result<()> {
        /* 旧实现仅校验后返回成功，没有更新 OpenCode 会话：
        self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        Ok(())
        */
        // 校验会话归属，并从对应生命周期取得协议客户端。
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let cwd = self.thread_cwd(&workspace, thread).await?;
        let engine = if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace).await?
        } else {
            self.local_engine(&cwd).await?
        };
        // 只有 V2 会话更新成功才返回成功，保留服务的原始错误。
        let session_id = percent_encoding::utf8_percent_encode(engine_thread_id, percent_encoding::NON_ALPHANUMERIC);
        engine.send_v2_request(&cwd, reqwest::Method::PATCH, &format!("/api/session/{session_id}"), Some(json!({ "title": name })))
            .await
            .inspect_err(|error| log::error!("更新 OpenCode V2 会话标题失败: session_id={engine_thread_id}, error={error:#}"))?;
        Ok(())
    }

    async fn list_codex_skills(
        &self,
        context: &CliExecutionContext,
        _cwd: &str,
    ) -> Result<Vec<CodexSkillDto>> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("Codex Skill 目录"))
    }

    async fn list_codex_apps(&self, context: &CliExecutionContext) -> Result<Vec<CodexAppDto>> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("Codex Apps 目录"))
    }

    async fn list_codex_plugins(
        &self,
        context: &CliExecutionContext,
        _cwd: &str,
    ) -> Result<Vec<CodexPluginDto>> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("Codex Plugin 目录"))
    }

    async fn get_opencode_runtime_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<OpenCodeRuntimeCatalogDto> {
        let workspace = self.load_workspace(context).await?;
        let cwd = self.resolve_workspace_cwd(&workspace, Some(cwd)).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .runtime_catalog(&cwd)
                .await
        } else {
            let engine = self.local_engine(&cwd).await?;
            engine.runtime_catalog(&cwd).await
        }
    }

    async fn refresh_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
        requested_kinds: &[String],
    ) -> Result<Vec<ExtensionCatalogKindRefreshDto>> {
        /*
        if context.location_kind == CliLocationKind::Local && context.workspace_id.is_empty() {
            let mut results = Vec::new();
            for kind in requested_kinds {
                results.push(
                    extensions::opencode::refresh_kind(self.state.engines.as_ref(), None, kind)
                        .await,
                );
            }
            return Ok(results);
        }
        let workspace = self.load_workspace(context).await?;
        let cwd = self.resolve_workspace_cwd(&workspace, cwd).await?;
        if context.location_kind == CliLocationKind::Local {
            let mut results = Vec::new();
            for kind in requested_kinds {
                if kind == "mcp" {
                    let runtime_result = self.local_engine(&cwd).await?.runtime_catalog(&cwd).await;
                    results.push(
                        extensions::opencode::refresh_kind_with_runtime(
                            Some(cwd.as_str()),
                            kind,
                            runtime_result,
                        )
                        .await,
                    );
                } else {
                    results.push(
                        extensions::opencode::refresh_kind(
                            self.state.engines.as_ref(),
                            Some(cwd.as_str()),
                            kind,
                        )
                        .await,
                    );
                }
            }
            return Ok(results);
        }

        let catalog = self
            .get_extension_catalog(context, Some(cwd.as_str()))
            .await?;
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
        */
        // 根据上下文只取得对应机器的协议客户端和目录。
        let (engine, directory) = if context.location_kind == CliLocationKind::Ssh {
            let workspace = self.load_workspace(context).await?;
            let directory = self.resolve_workspace_cwd(&workspace, cwd).await?;
            (remote_project_opencode_runtime_service::runtime(&workspace).await?, directory)
        } else if context.workspace_id.is_empty() {
            (self.local_engine("").await?, String::new())
        } else {
            let workspace = self.load_workspace(context).await?;
            let directory = self.resolve_workspace_cwd(&workspace, cwd).await?;
            (self.local_engine(&directory).await?, directory)
        };
        let mut results = Vec::new();
        for kind in requested_kinds {
            let path = match kind.as_str() {
                "skill" => "/api/skill",
                "plugin" => "/api/plugin",
                "mcp" => "/api/mcp",
                _ => { results.push(ExtensionCatalogKindRefreshDto::failure(kind)); continue; }
            };
            let result = match engine.get_v2_data(&directory, path).await {
                Ok(data) => extensions::opencode::parse_v2_catalog(kind, &data, Some(&directory)),
                Err(error) => Err(error),
            };
            match result {
                Ok(items) => results.push(ExtensionCatalogKindRefreshDto::success(kind, items)),
                Err(error) => {
                    log::error!("读取 OpenCode V2 扩展目录失败: kind={kind}, cwd={directory}, error={error:#}");
                    results.push(ExtensionCatalogKindRefreshDto::failure(kind));
                }
            }
        }
        Ok(results)
    }

    /// 为当前 OpenCode 工作区创建一次性扩展目录排期。
    async fn schedule_extension_catalog_context(&self, cwd: Option<String>) -> Result<bool> {
        // 以 provider 和目录作为唯一键，避免页面重复读取造成重复首次排期。
        let context_key = extensions::refresh::context_key(cwd.as_deref());
        if !self
            .state
            .extension_catalog_refreshes
            .schedule_once_per_run(self.id(), &context_key)
            .await
        {
            return Ok(false);
        }

        // 初始化各扩展类型的缓存记录，并将首次刷新时间设为当前时间。
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

        if let Err(error) = schedule_result {
            // 数据库排期失败时撤销进程内标记，允许后续页面读取重试。
            self.state
                .extension_catalog_refreshes
                .forget_scheduled_context(self.id(), &context_key)
                .await;
            log::error!(
                "OpenCode 扩展目录排期失败: context={context_key} error={error:#}"
            );
            return Err(error);
        }

        // 唤醒后台调度器尽快处理刚排入的扩展目录任务。
        self.state
            .extension_catalog_refreshes
            .wake_catalog_scheduler();
        Ok(true)
    }

    /// 确保 OpenCode 扩展刷新上下文并启动对应的后台刷新任务。
    async fn request_extension_catalog_refresh(
        &self,
        app: tauri::AppHandle,
        cwd: Option<String>,
        requested_kinds: Vec<String>,
        context: Option<CliExecutionContext>,
    ) -> Result<bool> {
        // 按固定类型顺序规范化请求，并确保快照记录先于运行状态建立。
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

        // 先通知页面刷新状态，再由克隆的 CLI 实例在后台完成目录读取。
        extensions::refresh::emit_catalog_updated(&app, self.id(), cwd.as_deref());
        let cli = self.clone();
        tauri::async_runtime::spawn(async move {
            cli.run_extension_catalog_refresh(app, context_key, cwd, kinds, context)
                .await;
        });
        Ok(true)
    }

    /// 查询 OpenCode provider 已到期的刷新上下文及扩展类型集合。
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

    /// 查询 OpenCode provider 最近一次计划刷新时间。
    async fn next_extension_catalog_refresh_at(&self) -> Result<Option<String>> {
        db::extensions::next_refresh_at_for_provider(&self.state.db, self.id())
    }

    /// 读取当前 OpenCode 本机缓存或 SSH 目录，并为本机目录登记首次刷新。
    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        // 本机沿用扩展快照，SSH 目录直接从远端协议客户端读取。
        let mut cached = None;
        let (engine, cwd) = if context.location_kind == CliLocationKind::Local && context.workspace_id.is_empty() {
            cached = Some(self.load_cached_extension_catalog(None).await?);
            (self.local_engine("").await?, String::new())
        } else {
            let workspace = self.load_workspace(context).await?;
            let cwd = self.resolve_workspace_cwd(&workspace, cwd).await?;
            if context.location_kind == CliLocationKind::Local {
                cached = Some(self.load_cached_extension_catalog(Some(&cwd)).await?);
                (self.local_engine(&cwd).await?, cwd)
            } else {
                (remote_project_opencode_runtime_service::runtime(&workspace).await?, cwd)
            }
        };
        let runtime = engine.runtime_catalog(&cwd).await?;
        let mut items = runtime
            .agents
            .into_iter()
            .filter(|agent| !agent.hidden)
            .map(|agent| ExtensionItemDto {
                id: agent.name.clone(),
                provider_id: "opencode".to_string(),
                kind: "agent".to_string(),
                name: agent.name,
                description: agent.description,
                version: None,
                scope: if agent.native { "native" } else { "project" }.to_string(),
                source: Some(agent.mode),
                marketplace: None,
                path: None,
                parent_plugin_id: None,
                category: agent.variant,
                officially_available: false,
                catalog_authority: None,
                installed: Some(true),
                configured: Some(true),
                enabled: Some(true),
                health: "healthy".to_string(),
                auth_state: None,
                available_actions: Vec::new(),
                requires_new_session: false,
                read_only_reason: Some("ssh_remote_opencode_extension_action".to_string()),
                warning: None,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        // OpenCode 当前运行时目录只区分 Agent、Command 和 MCP；没有独立的 Skill 目录。
        // 用户在 OpenCode 里看到的 /xxx 项来自 OpenCode 自己的 Command 目录，不能强行标成 skill。
        // 因此这里保留 OpenCode 原生业务对象：Command -> kind=command，MCP -> kind=mcp。
        items.extend(
            runtime
                .commands
                .into_iter()
                .map(|command| ExtensionItemDto {
                    id: command.name.clone(),
                    provider_id: "opencode".to_string(),
                    kind: "command".to_string(),
                    name: command.name,
                    description: command.description,
                    version: None,
                    scope: "project".to_string(),
                    source: command.source,
                    marketplace: None,
                    path: None,
                    parent_plugin_id: None,
                    category: command.agent,
                    officially_available: false,
                    catalog_authority: None,
                    installed: Some(true),
                    configured: Some(true),
                    enabled: Some(true),
                    health: "healthy".to_string(),
                    auth_state: None,
                    available_actions: Vec::new(),
                    requires_new_session: false,
                    read_only_reason: Some("ssh_remote_opencode_extension_action".to_string()),
                    warning: None,
                    ..Default::default()
                }),
        );
        /*
        items.extend(
            runtime
                .mcp_servers
                .into_iter()
                .filter(|server| server.name != "auracoder-computer-control")
                .map(|server| {
                    let normalized_status = server.status.to_ascii_lowercase();
                    let health = if normalized_status.contains("connected") {
                        "healthy"
                    } else if normalized_status.contains("auth") {
                        "auth_required"
                    } else if normalized_status.contains("failed")
                        || normalized_status.contains("error")
                    {
                        "error"
                    } else {
                        "unknown"
                    };
                    let auth_state = match health {
                        "healthy" => "authenticated",
                        "auth_required" => "required",
                        _ => "unknown",
                    };
                    ExtensionItemDto {
                        id: server.name.clone(),
                        provider_id: "opencode".to_string(),
                        kind: "mcp".to_string(),
                        name: server.name,
                        description: server.detail,
                        version: None,
                        scope: "project".to_string(),
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
                        health: health.to_string(),
                        auth_state: Some(auth_state.to_string()),
                        available_actions: Vec::new(),
                        requires_new_session: false,
                        read_only_reason: Some("ssh_remote_opencode_extension_action".to_string()),
                        warning: None,
                        ..Default::default()
                    }
                }),
        );
        */
        // 本机保留快照并补充实时 agent、command；SSH 直接读取 V2 扩展目录。
        if let Some(mut catalog) = cached {
            catalog.items.extend(items);
            return Ok(catalog);
        }
        for kind in ["skill", "plugin", "mcp"] {
            let path = match kind { "skill" => "/api/skill", "plugin" => "/api/plugin", _ => "/api/mcp" };
            let data = engine.get_v2_data(&cwd, path).await.inspect_err(|error| {
                log::error!("读取 SSH OpenCode V2 扩展目录失败: kind={kind}, cwd={cwd}, error={error:#}");
            })?;
            items.extend(extensions::opencode::parse_v2_catalog(kind, &data, Some(&cwd))?);
        }
        let fetched_at = runtime_env::system_time_rfc3339();
        let kind_fetched_at = ["agent", "command", "skill", "plugin", "mcp"]
            .into_iter()
            .map(|kind| (kind.to_string(), Some(fetched_at.clone())))
            .collect();

        Ok(CachedExtensionCatalogDto {
            provider_id: "opencode".to_string(),
            cwd: Some(cwd),
            items,
            sources: Vec::new(),
            capabilities: extensions::provider_capabilities("opencode"),
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
                "agent" => {
                    item.panel = Some("agents".to_string());
                    item.group = Some("agents".to_string());
                }
                "command" => {
                    // OpenCode 的具体 Command 是可直接插入输入框的 /xxx 项，不是面板入口。
                    // 是否打开面板由 panel 字段决定；这里没有 panel，所以点击后插入 insert_text。
                    item.insert_text = Some(format!("/{} ", item.name));
                    item.group = Some("commands".to_string());
                }
                "mcp" => {
                    item.panel = Some("mcp".to_string());
                    item.group = Some("mcp".to_string());
                }
                _ => {}
            }
        }
        // 这里补的是 OpenCode 的一级面板入口。
        // 它们同样是 kind=command，但带 panel 字段；前端会按 panel 打开对应面板。
        let panel_ids = ["agents", "commands", "sessions"];
        items.extend(panel_ids.into_iter().map(|id| ExtensionItemDto {
            id: id.to_string(),
            provider_id: "opencode".to_string(),
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
        _scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto> {
        // 在 workspace 归属的运行位置执行 MCP 动作。
        let workspace = self.load_workspace(context).await?;
        let cwd = self.resolve_workspace_cwd(&workspace, None).await?;
        let engine = if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace).await?
        } else { self.local_engine(&cwd).await? };
        self.perform_v2_extension_action(&engine, &cwd, &item, action).await
    }

    /// 用户未选择 workspace 时，使用本机用户级 OpenCode 配置执行全局扩展动作。
    async fn perform_global_extension_action(
        &self,
        item: ExtensionItemDto,
        action: &str,
        _scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto> {
        // 用户级动作使用本机用户级服务。
        let engine = self.local_engine("").await?;
        self.perform_v2_extension_action(&engine, "", &item, action).await
    }

    /// 报告 OpenCode 当前不支持的会话分支业务。
    async fn fork_thread(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        _engine_thread_id: &str,
        _cwd: &str,
        _model: &str,
        _sandbox: SandboxPolicy,
    ) -> Result<CliForkedThread> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("会话分支"))
    }

    async fn rollback_thread(
        &self,
        context: &CliExecutionContext,
        _engine_thread_id: &str,
        _num_turns: u32,
    ) -> Result<ThreadSyncSnapshot> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("会话回滚"))
    }

    // 旧返回值不含压缩轮次标识，保留迁移留痕：async fn compact_thread(...) -> Result<()>;
    async fn compact_thread(
        &self,
        context: &CliExecutionContext,
        _engine_thread_id: &str,
    ) -> Result<super::CliCompactionResult> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("会话压缩"))
    }

    async fn start_review(
        &self,
        context: &CliExecutionContext,
        _source_engine_thread_id: &str,
        _target: Value,
        _delivery: Option<&str>,
        _event_tx: mpsc::Sender<EngineEvent>,
        _cancellation: CancellationToken,
        _started_tx: oneshot::Sender<CliReviewStarted>,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("代码审查"))
    }
}

pub(crate) mod thread_business {
    use super::*;
    use chrono::{DateTime, Utc};
    use crate::models::{OpenCodeRemoteSessionDto, OpenCodeRemoteSessionPageDto, ThreadUpdateDto, MessageStatusDto};
    use crate::path_utils::paths_equal;
    use crate::services::thread_service::normalize_thread_title;

    pub(crate) fn collect_remote_thread_roots(workspace_root: &str) -> std::collections::HashSet<String> {
        std::iter::once(workspace_root.to_string()).collect()
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
    pub(crate) fn short_thread_label(engine_thread_id: &str) -> String {
        engine_thread_id.chars().take(8).collect()
    }
    pub(crate) fn map_opencode_remote_session_dto(
        session: OpenCodeRemoteSessionSummary,
        local_thread_id: Option<String>,
    ) -> OpenCodeRemoteSessionDto {
        OpenCodeRemoteSessionDto {
            engine_thread_id: session.engine_thread_id,
            title: session.title,
            cwd: session.cwd,
            created_at: codex_remote_thread_timestamp_to_rfc3339(session.created_at),
            updated_at: codex_remote_thread_timestamp_to_rfc3339(session.updated_at),
            archived: session.archived,
            local_thread_id,
        }
    }
    pub(crate) fn build_opencode_remote_session_title(session: &OpenCodeRemoteSessionSummary) -> String {
        if let Some(title) = session
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return normalize_thread_title(title).unwrap_or_else(|_| {
                    format!(
                        "OpenCode session {}",
                        short_thread_label(&session.engine_thread_id)
                    )
            });
        }

        format!(
            "OpenCode session {}",
            short_thread_label(&session.engine_thread_id)
        )
    }
    pub(crate) fn build_opencode_remote_session_metadata(
        existing: Option<&Value>,
        session: &OpenCodeRemoteSessionSummary,
        model_id: &str,
    ) -> Value {
        let mut metadata = existing.cloned().unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            metadata = json!({});
        }

        if let Some(object) = metadata.as_object_mut() {
        // 线程底部六项已迁移到独立列；远端同步只保留 OpenCode 状态 metadata。
            for key in [
                "lastModelId",
                "reasoningEffort",
            ] {
                object.remove(key);
            }
            object.insert("opencodeRemoteSessionAttached".to_string(), json!(true));
            object.insert(
                    "opencodeRemoteArchived".to_string(),
                json!(session.archived),
            );
            object.insert("opencodeRemoteCwd".to_string(), json!(session.cwd));
            object.insert(
                    "opencodeRemoteCreatedAt".to_string(),
                json!(codex_remote_thread_timestamp_to_rfc3339(session.created_at)),
            );
            object.insert(
                    "opencodeRemoteUpdatedAt".to_string(),
                json!(codex_remote_thread_timestamp_to_rfc3339(session.updated_at)),
            );
            object.insert("opencodeTranscriptImported".to_string(), json!(false));
        }

        metadata
    }
    pub(crate) fn normalize_thread_opencode_agent(value: Option<String>) -> Result<Option<String>, String> {
        let normalized = value
            .as_deref()
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty());

        let Some(normalized) = normalized else {
            return Ok(None);
        };

        if normalized.chars().count() > 120 {
            return Err("invalid OpenCode agent. name is too long".to_string());
        }

        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(
            "invalid OpenCode agent. expected letters, numbers, dots, dashes, or underscores"
                    .to_string(),
            );
        }

        if normalized == "build" {
            return Ok(None);
        }

        Ok(Some(normalized.to_string()))
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
    pub(crate) fn err_to_string(error: impl std::fmt::Display) -> String {
        format!("{error:#}")
    }
    impl OpenCodeCli {
        pub(crate) async fn thread_list_records(&self, workspace_id: String, cursor: Option<String>, limit: Option<u32>, search_term: Option<String>, archived: Option<bool>) -> Result<OpenCodeRemoteSessionPageDto, String> {
            let state = &self.state;

            let db = state.db.clone();
            let workspace = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    move |db| {
                        let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
                        Ok(workspace)
                    }
            })
                .await?;

            let allowed_roots = collect_remote_thread_roots(&workspace.root_path);
            let normalized_search_term = normalize_remote_thread_search_term(search_term);
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let parse_timestamp = |value: Option<String>| {
                value
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or_default()
            };
            let mut remote_sessions = cli
                .list_sessions(&context, normalized_search_term.as_deref(), archived)
                .await
                .map_err(err_to_string)?
                .into_iter()
                .filter(|session| allowed_roots.contains(session.cwd.as_str()))
                .map(|session| OpenCodeRemoteSessionSummary {
                    engine_thread_id: session.engine_thread_id,
                    title: Some(session.title),
                    cwd: session.cwd,
                    created_at: parse_timestamp(session.created_at),
                    updated_at: parse_timestamp(session.updated_at),
                    archived: session.archived,
            })
                .collect::<Vec<_>>();
            remote_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            let mut seen_session_ids = std::collections::HashSet::new();
            remote_sessions.retain(|session| seen_session_ids.insert(session.engine_thread_id.clone()));

            let offset = parse_codex_remote_thread_cursor(cursor.as_deref())?;
            let page_size = normalize_codex_remote_thread_limit(limit);
            let page_end = offset.saturating_add(page_size).min(remote_sessions.len());
            let page_sessions = if offset >= remote_sessions.len() {
                Vec::new()
            } else {
                remote_sessions[offset..page_end].to_vec()
            };
            let next_cursor = (page_end < remote_sessions.len()).then(|| page_end.to_string());

            run_db(db, move |db| {
                    let sessions = page_sessions
                        .into_iter()
                        .map(|session| {
                            let local_thread_id = db::threads::find_thread_by_engine_thread_id(
                                db,
                                "opencode",
                                &session.engine_thread_id,
                            )?
                                .filter(|local| local.workspace_id == workspace_id)
                                .map(|local| local.id);
                            Ok(map_opencode_remote_session_dto(session, local_thread_id))
                    })
                        .collect::<anyhow::Result<Vec<_>>>()?;

                    Ok(OpenCodeRemoteSessionPageDto {
                            sessions,
                            next_cursor,
                    })
            })
                .await

        }
        pub(crate) async fn thread_attach_record(&self, workspace_id: String, engine_thread_id: String, cwd: String, model_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;

            let normalized_model_id =
            self.validate_thread_model(Some(&workspace_id), model_id.trim()).await.map_err(err_to_string)?;
            let db = state.db.clone();
            let (workspace, existing_local_thread) = run_db(db.clone(), {
                    let workspace_id = workspace_id.clone();
                    let engine_thread_id = engine_thread_id.clone();
                    move |db| {
                        let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
                        let existing =
                        db::threads::find_thread_by_engine_thread_id(db, "opencode", &engine_thread_id)?
                            .filter(|thread| thread.workspace_id == workspace_id);
                        Ok((workspace, existing))
                    }
            })
                .await?;

            let allowed_roots = collect_remote_thread_roots(&workspace.root_path);
            if !allowed_roots.contains(cwd.as_str()) {
                return Err(format!(
            "OpenCode session cwd is outside this workspace: {cwd}"
                ));
            }

            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let session = cli
                .read_session(&context, &engine_thread_id)
                .await
                .map_err(err_to_string)?;
            if !paths_equal(&session.cwd, &cwd) {
                return Err(format!(
                        "OpenCode session cwd does not match: expected={cwd} actual={}",
                        session.cwd
                ));
            }
            let parse_timestamp = |value: Option<String>| {
                value
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or_default()
            };
            let mut remote_session = OpenCodeRemoteSessionSummary {
                engine_thread_id: session.engine_thread_id,
                title: Some(session.title),
                cwd: session.cwd,
                created_at: parse_timestamp(session.created_at),
                updated_at: parse_timestamp(session.updated_at),
                archived: session.archived,
            };
            if remote_session.archived {
                let operation_thread = existing_local_thread.clone().unwrap_or_else(|| ThreadDto {
                        id: engine_thread_id.clone(),
                        workspace_id: workspace.id.clone(),
                        engine_id: "opencode".to_string(),
                        model_id: normalized_model_id.clone(),
                        engine_thread_id: Some(engine_thread_id.clone()),
                        engine_metadata: Some(json!({ "opencodeRemoteCwd": cwd })),
                        plan_mode: None,
                        send_method: None,
                        reasoning_effort: None,
                        permission_mode: None,
                        title: remote_session
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
                remote_session.archived = false;
            }
            let title = build_opencode_remote_session_title(&remote_session);
    // 只有远端返回并成功解析的更新时间才覆盖本地活动时间，避免写入 Unix epoch。
            let remote_last_activity_at = (remote_session.updated_at > 0)
                .then(|| codex_remote_thread_timestamp_to_rfc3339(remote_session.updated_at));

            if let Some(existing) = existing_local_thread {
                let metadata = build_opencode_remote_session_metadata(
                    existing.engine_metadata.as_ref(),
                    &remote_session,
                    &normalized_model_id,
                );
                let existing_last_activity_at = remote_last_activity_at.clone();
                return run_db(db, move |db| {
                        let thread = match db::threads::restore_thread(db, &existing.id) {
                            Ok(restored) => restored,
                            Err(_) => existing,
                        };
                        let updated = db::threads::update_thread_runtime_snapshot(
                            db,
                            &thread.id,
                            Some(&title),
                            Some(ThreadStatusDto::Idle),
                            Some(&metadata),
                        )?;
            // OpenCode 远端会话只返回模型，其余底部项服务端不提供，按真实返回值落库。
                        db::threads::update_thread_runtime_selection(
                            db,
                            &updated.id,
                            "opencode",
                            &normalized_model_id,
                            None,
                            None,
                            None,
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

            let metadata =
            build_opencode_remote_session_metadata(None, &remote_session, &normalized_model_id);
            run_db(db, move |db| {
                    let created = db::threads::create_thread(
                        db,
                        &workspace_id,
                        "opencode",
                        &normalized_model_id,
                        &title,
                    )?;
                    db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
                    let updated = db::threads::update_thread_runtime_snapshot(
                        db,
                        &created.id,
                        Some(&title),
                        Some(ThreadStatusDto::Idle),
                        Some(&metadata),
                    )?;
        // OpenCode 远端会话只返回模型，其余底部项服务端不提供，按真实返回值落库。
                    db::threads::update_thread_runtime_selection(
                        db,
                        &updated.id,
                        "opencode",
                        &normalized_model_id,
                        None,
                        None,
                        None,
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
        pub(crate) async fn thread_update_config(&self, thread_id: String, update_agent: bool, agent: Option<String>) -> Result<ThreadDto, String> {
            let state = &self.state;

            let db = state.db.clone();
            let thread = run_db(db.clone(), {
                    let thread_id = thread_id.clone();
                    move |db| db::threads::get_thread(db, &thread_id)
            })
                .await?
                .ok_or_else(|| format!("thread not found: {thread_id}"))?;

            if thread.engine_id != "opencode" {
                return Err("OpenCode thread config is only available for OpenCode threads".to_string());
            }

            let normalized_agent = if update_agent {
                normalize_thread_opencode_agent(agent)?
            } else {
                None
            };

            let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
            if !metadata.is_object() {
                metadata = json!({});
            }

            if let Some(object) = metadata.as_object_mut() {
                if update_agent {
                    match normalized_agent {
                        Some(value) => {
                            object.insert("opencodeAgent".to_string(), json!(value));
                        }
                        None => {
                            object.remove("opencodeAgent");
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
                .ok_or_else(|| format!("thread not found after OpenCode config update: {thread_id}"))

        }
        pub(crate) async fn thread_sync_record(&self, thread_id: String) -> Result<ThreadDto, String> {
            let state = &self.state;
            let thread = crate::services::thread_service::ThreadService::new(state.clone()).get_thread(&thread_id).await.map_err(err_to_string)?;
            if thread.engine_id != "opencode" { return Err("线程所属 CLI 与当前实现不一致".to_owned()); }
            let thread = &thread;
            let thread_id = thread_id.as_str();
            let mut summary_synced_thread = thread.clone();

            let message_count = run_db(state.db.clone(), {
                    let thread_id = thread_id.to_string();
                    move |db| db::messages::count_thread_messages(db, &thread_id)
            })
                .await?;
            if let Some(engine_thread_id) = thread
                .engine_thread_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let workspace = run_db(state.db.clone(), {
                        let workspace_id = thread.workspace_id.clone();
                        move |db| {
                            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                        }
                })
                    .await?;
                let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = self;
                let snapshot = match cli.read_session(&context, engine_thread_id).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        log::debug!("failed to sync OpenCode session {engine_thread_id}: {error}");
                        return Ok(thread.clone());
                    }
                };
                let parse_timestamp = |value: Option<String>| {
                    value
                        .as_deref()
                        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.timestamp_millis())
                        .unwrap_or_default()
                };
                let session = OpenCodeRemoteSessionSummary {
                    engine_thread_id: snapshot.engine_thread_id,
                    title: Some(snapshot.title),
                    cwd: snapshot.cwd,
                    created_at: parse_timestamp(snapshot.created_at),
                    updated_at: parse_timestamp(snapshot.updated_at),
                    archived: snapshot.archived,
                };
                let title = build_opencode_remote_session_title(&session);
                let metadata = build_opencode_remote_session_metadata(
                    thread.engine_metadata.as_ref(),
                    &session,
                    &thread.model_id,
                );
                let thread_id = thread_id.to_string();
                summary_synced_thread = run_db(state.db.clone(), move |db| {
                        db::threads::update_thread_runtime_snapshot(
                            db,
                            &thread_id,
                            Some(&title),
                            Some(ThreadStatusDto::Idle),
                            Some(&metadata),
                        )
                })
                    .await?;
                if message_count != 0 {
                    return Ok(summary_synced_thread);
                }
            }
            if message_count != 0 {
                return Ok(thread.clone());
            }

            let Some(engine_thread_id) = thread
                .engine_thread_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Ok(thread.clone());
            };

            let workspace = run_db(state.db.clone(), {
                    let workspace_id = thread.workspace_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                    }
            })
                .await?;
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli: &dyn CliTool = self;
            let snapshot = match cli
                .read_thread_sync_snapshot(&context, thread, engine_thread_id)
                .await
            {
                Ok(Some(snapshot)) => {
                    snapshot
                }
                Ok(None) => {
                    return Ok(summary_synced_thread);
                }
                Err(error) => {
            // 旧完整历史入口会把原始读取错误返回给点击层和 MCP；两者再回退到本地结果。
                    let error_text = err_to_string(error);
                    return Err(error_text);
            // 旧实现曾在此处记录 debug 后返回本地线程，保留迁移留痕但不再吞掉原生错误：
            // log::debug!("failed to sync {} thread {}: {error}", thread.engine_id, thread_id);
            // return Ok(thread.clone());
                }
            };

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
            run_db(state.db.clone(), {
                    let thread_id = thread_id.to_string();
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

            let title = snapshot
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let updated_thread = run_db(state.db.clone(), {
                    let thread_id = thread_id.to_string();
                    let title = title.map(str::to_string);
                    move |db| {
                        db::threads::update_thread_runtime_snapshot(
                            db,
                            &thread_id,
                            title.as_deref(),
                            None,
                            None,
                        )
                    }
            })
                .await?;


            Ok(updated_thread)

        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/opencode_cli_tests.rs"]
mod engine_business_tests;
