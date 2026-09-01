use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use super::{
    BaseCliMcp, CliExecutionContext, CliForkedThread, CliLocationKind, CliMcpRuntime,
    CliReviewStarted, CliRuntimePermissionPatch, CliRuntimePermissions, CliSessionNotFoundError,
    CliSessionSnapshot, CliTool, McpInvocationContext, McpToolResult, map_context_usage,
};
use crate::{
    db,
    engines::{
        capabilities_for_engine, map_engine_capabilities, map_model_info, opencode::OpenCodeEngine,
        ApprovalRequestRoute, CodexRuntimeEvent, Engine, EngineCapabilities, EngineEvent,
        EngineSteerReceipt, EngineThread, ModelInfo, OpenCodeRemoteSessionSummary, SandboxPolicy,
        ThreadScope, ThreadSyncSnapshot, TurnInput,
    },
    extensions,
    local_cli_service_lifecycle::{LocalCliHandle, LocalCliServiceLifecycle},
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, CliContextUsageDto, CodexAppDto,
        CodexPluginDto, CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionItemDto, OpenCodeRuntimeCatalogDto,
        PermissionComponentJson, ThreadDto, ThreadStatusDto, WorkspaceDto,
    },
    path_utils, remote_project_opencode_runtime_service, ssh,
    state::AppState,
};

fn default_permission_component() -> PermissionComponentJson {
    let mut values = HashMap::new();
    for key in ["autonomyPreset", "trust", "approval", "sandbox", "network"] {
        values.insert(key.to_string(), json!(["automatic"]));
    }
    values.insert("defaultForNewThreads".to_string(), json!([]));
    values
}

fn set_permission_array(values: &mut PermissionComponentJson, key: &str, items: &[&str]) {
    values.insert(
        key.to_string(),
        Value::Array(items.iter().map(|item| json!(item)).collect()),
    );
}

fn permission_choice<'a>(
    values: &'a PermissionComponentJson,
    key: &str,
) -> Result<Option<&'a str>> {
    let value = values.get(key).context(format!("缺少权限参数: {key}"))?;
    let array = value
        .as_array()
        .context(format!("权限参数必须是数组: {key}"))?;
    Ok(array.first().and_then(Value::as_str))
}

fn validate_permission_component(values: &PermissionComponentJson) -> Result<()> {
    let allowed = [
        (
            "autonomyPreset",
            ["automatic", "read-only", "ask", "auto", "full"].as_slice(),
        ),
        (
            "trust",
            ["automatic", "trusted", "standard", "restricted"].as_slice(),
        ),
        (
            "approval",
            ["automatic", "restricted", "ask", "autonomous"].as_slice(),
        ),
        (
            "sandbox",
            ["automatic", "read-only", "workspace-write", "full-access"].as_slice(),
        ),
        ("network", ["automatic", "enabled", "restricted"].as_slice()),
        (
            "defaultForNewThreads",
            ["automatic", "read-only", "ask", "auto", "full"].as_slice(),
        ),
    ];
    for key in values.keys() {
        anyhow::ensure!(
            allowed.iter().any(|(name, _)| name == key),
            "未知权限参数: {key}"
        );
    }
    for (key, choices) in allowed {
        let value = values.get(key).context(format!("缺少权限参数: {key}"))?;
        let array = value
            .as_array()
            .context(format!("权限参数必须是数组: {key}"))?;
        anyhow::ensure!(array.len() <= 1, "权限参数数组最多只能有一个值: {key}");
        for item in array {
            let item = item
                .as_str()
                .context(format!("权限参数值必须是字符串: {key}"))?;
            anyhow::ensure!(choices.contains(&item), "权限参数值不支持: {key}={item}");
        }
    }
    Ok(())
}

fn raw_rules_from_thread(thread: &ThreadDto) -> Result<Value> {
    let raw = thread.permission_mode.as_deref().unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(json!([]));
    }
    let parsed: Value = serde_json::from_str(raw).context("OpenCode 权限 JSON 格式错误")?;
    if parsed.is_null() || parsed == json!({}) || parsed == json!([]) {
        return Ok(json!([]));
    }
    if let Some(rules) = parsed.get("permission") {
        anyhow::ensure!(rules.is_array(), "OpenCode permission 必须是数组");
        return Ok(rules.clone());
    }
    if let Some(policy) = parsed
        .get("approvalPolicy")
        .or_else(|| parsed.get("opencodePermissionMode"))
        .and_then(Value::as_str)
    {
        return Ok(match policy {
            "allow" => json!([{ "permission": "*", "pattern": "*", "action": "allow" }]),
            "deny" => json!([{ "permission": "*", "pattern": "*", "action": "deny" }]),
            "ask" => json!([
                { "permission": "*", "pattern": "*", "action": "ask" },
                { "permission": "question", "pattern": "*", "action": "allow" }
            ]),
            "inherit" => json!([]),
            _ => anyhow::bail!("OpenCode approvalPolicy 值无效: {policy}"),
        });
    }
    Ok(json!([]))
}

/// 从现有 OpenCode raw object 复制权限数组，保留 CLI 未知顶层字段。
fn raw_permission_value(thread: &ThreadDto, rules: &Value) -> Value {
    let mut raw_object = thread
        .permission_mode
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    raw_object.insert("permission".to_string(), rules.clone());
    Value::Object(raw_object)
}

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
        engine.set_computer_control_service(self.state.computer_control_service.clone());
        engine.set_auracoder_thread_mcp_service(self.state.auracoder_thread_mcp_service.clone());
        engines.insert(cwd.to_string(), (endpoint_generation, engine.clone()));
        Ok(engine)
    }

    /// 配置本机 OpenCode 协议客户端的计算机控制和会话读取服务。
    async fn configure_local_computer_control(&self, cwd: &str) -> Result<Arc<OpenCodeEngine>> {
        self.local_engine(cwd).await
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

    fn map_session(summary: OpenCodeRemoteSessionSummary, is_ssh: bool) -> CliSessionSnapshot {
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
}

#[async_trait]
impl CliTool for OpenCodeCli {
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
        let engine = self.local_engine(&context.root_path).await?;
        let cached_models = engine.runtime_model_fallback().await;
        if cached_models
            .iter()
            .any(|model| model.id == requested_model_id)
        {
            return Ok(cached_models);
        }
        Ok(engine.list_models_runtime().await)
    }

    async fn get_chat_provider_usage(
        &self,
        context: &CliExecutionContext,
    ) -> Result<Option<ChatProviderUsageDto>> {
        self.load_workspace(context).await?;
        Ok(None)
    }

    /// 用户进入 OpenCode 线程时，读取模型上下文上限和最新可靠 assistant token 快照。
    async fn get_context_usage(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<Option<CliContextUsageDto>> {
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

    fn subscribe_codex_runtime_events(&self) -> broadcast::Receiver<CodexRuntimeEvent> {
        let (_event_tx, event_rx) = broadcast::channel(1);
        event_rx
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

    async fn get_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<PermissionComponentJson> {
        self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        anyhow::ensure!(
            thread.workspace_id == context.workspace_id,
            "当前会话不属于该 workspace"
        );
        let mut rules = raw_rules_from_thread(thread)?;
        /*
        // 旧实现曾用该标记判断是否回退到 engine_metadata_json 权限镜像；
        // 统一权限来源改为 permission_mode 后，该分支已停用。
        let has_explicit_permission = thread
            .permission_mode
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|raw| raw.get("permission").cloned())
            .is_some();
        */
        // 权限为空时保持 CLI 默认行为，不再回退读取 engine_metadata_json。
        let mut result = default_permission_component();
        let action = rules.as_array().and_then(|rules| {
            rules.iter().rev().find_map(|rule| {
                (rule.get("permission").and_then(Value::as_str) == Some("*")
                    && rule.get("pattern").and_then(Value::as_str) == Some("*"))
                .then(|| rule.get("action").and_then(Value::as_str))
                .flatten()
            })
        });
        match action {
            Some("allow") => {
                set_permission_array(&mut result, "autonomyPreset", &["full"]);
                set_permission_array(&mut result, "approval", &["autonomous"]);
            }
            Some("ask") => {
                set_permission_array(&mut result, "autonomyPreset", &["ask"]);
                set_permission_array(&mut result, "approval", &["ask"]);
            }
            Some("deny") => {
                set_permission_array(&mut result, "autonomyPreset", &["read-only"]);
                set_permission_array(&mut result, "approval", &["restricted"]);
            }
            Some(_) => {
                set_permission_array(&mut result, "autonomyPreset", &[]);
                set_permission_array(&mut result, "approval", &[]);
            }
            None => {}
        }
        Ok(result)
    }

    async fn set_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        values: PermissionComponentJson,
    ) -> Result<PermissionComponentJson> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        anyhow::ensure!(
            thread.workspace_id == context.workspace_id,
            "当前会话不属于该 workspace"
        );
        validate_permission_component(&values)?;
        let current = <Self as CliTool>::get_permissions(self, context, thread).await?;
        if current.get("autonomyPreset") == values.get("autonomyPreset")
            && current.get("approval") == values.get("approval")
            && current.get("sandbox") == values.get("sandbox")
            && current.get("network") == values.get("network")
        {
            let mut result = current;
            if let Some(value) = values.get("trust") {
                result.insert("trust".to_string(), value.clone());
            }
            if let Some(value) = values.get("defaultForNewThreads") {
                result.insert("defaultForNewThreads".to_string(), value.clone());
            }
            return Ok(result);
        }
        let preset = permission_choice(&values, "autonomyPreset")?;
        let approval = permission_choice(&values, "approval")?;
        let autonomy_is_empty = values
            .get("autonomyPreset")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        anyhow::ensure!(
            permission_choice(&values, "sandbox")?.map_or(true, |v| v == "automatic"),
            "OpenCode 不支持 sandbox 覆盖"
        );
        anyhow::ensure!(
            permission_choice(&values, "network")?.map_or(true, |v| v == "automatic"),
            "OpenCode 不支持 network 覆盖"
        );
        let rules = match preset {
            Some("automatic") => json!([]),
            None if autonomy_is_empty || approval == Some("automatic") => json!([]),
            Some("read-only") => json!([{ "permission": "*", "pattern": "*", "action": "deny" }]),
            Some("ask") => json!([
                { "permission": "*", "pattern": "*", "action": "ask" },
                { "permission": "question", "pattern": "*", "action": "allow" }
            ]),
            Some("auto") | Some("full") => {
                json!([{ "permission": "*", "pattern": "*", "action": "allow" }])
            }
            _ => match approval {
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
                _ => raw_rules_from_thread(thread)?,
            },
        };
        // 只替换 OpenCode 的 permission 字段；原始 JSON 中其他顶层字段属于 CLI
        // 自身扩展，必须随权限变更继续保留。
        let raw = if thread
            .permission_mode
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .is_some_and(|value| value.is_array())
        {
            rules.to_string()
        } else {
            raw_permission_value(thread, &rules).to_string()
        };
        let engine_thread_id = thread.engine_thread_id.clone();
        let saved = db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw))?;
        if let Some(engine_thread_id) = engine_thread_id.as_deref() {
            let cwd = self.thread_cwd(&workspace, thread).await?;
            let result = if context.location_kind == CliLocationKind::Ssh {
                remote_project_opencode_runtime_service::runtime(&workspace)
                    .await?
                    .set_session_permission_rules(&cwd, engine_thread_id, &rules)
                    .await
            } else {
                self.local_engine(&cwd)
                    .await?
                    .set_session_permission_rules(&cwd, engine_thread_id, &rules)
                    .await
            };
            if let Err(error) = result {
                let rollback_value = thread
                    .permission_mode
                    .clone()
                    .unwrap_or_else(|| "[]".to_string());
                if let Err(rollback_error) = db::threads::update_thread_permissions(
                    &self.state.db,
                    &thread.id,
                    Some(&rollback_value),
                ) {
                    return Err(anyhow::anyhow!(
                        "同步 OpenCode session 权限失败: {error:#}; 回滚线程权限配置也失败: {rollback_error}"
                    ));
                }
                return Err(error).context("同步 OpenCode session 权限失败");
            }
        }
        let mut result = self.get_permissions(context, &saved).await?;
        for key in ["trust", "defaultForNewThreads"] {
            if let Some(value) = values.get(key) {
                result.insert(key.to_string(), value.clone());
            }
        }
        Ok(result)
    }

    /// 将 OpenCode 全局权限规则转换为统一运行时权限结构。
    async fn runtime_permissions(
        &self,
        _context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<CliRuntimePermissions> {
        let rules = raw_rules_from_thread(thread)?;
        let approval_policy = rules.as_array().and_then(|items| {
            items.iter().rev().find_map(|rule| {
                (rule.get("permission").and_then(Value::as_str) == Some("*")
                    && rule.get("pattern").and_then(Value::as_str) == Some("*"))
                .then(|| rule.get("action").cloned())
                .flatten()
            })
        });
        Ok(CliRuntimePermissions {
            approval_policy,
            sandbox_mode: None,
            allow_network: None,
            permission_profile: None,
            approvals_reviewer: None,
        })
    }

    /// 将统一权限补丁转换为 OpenCode permission 规则数组并持久化到线程。
    async fn patch_runtime_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        patch: CliRuntimePermissionPatch,
    ) -> Result<ThreadDto> {
        self.load_workspace(context).await?;
        let parsed = thread
            .permission_mode
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or_else(|| json!([]));
        anyhow::ensure!(
            patch.sandbox_mode.is_none()
                && patch.allow_network.is_none()
                && patch.permission_profile.is_none()
                && patch.approvals_reviewer.is_none(),
            "OpenCode 仅支持更新 permission 规则"
        );
        let mut rules = raw_rules_from_thread(thread)?;
        if let Some(value) = patch.approval_policy {
            let action = value.as_ref().and_then(Value::as_str);
            rules = match action {
                Some("allow") => json!([{ "permission": "*", "pattern": "*", "action": "allow" }]),
                Some("deny") => json!([{ "permission": "*", "pattern": "*", "action": "deny" }]),
                Some("ask") => json!([{ "permission": "*", "pattern": "*", "action": "ask" }]),
                None => json!([]),
                Some(other) => anyhow::bail!("OpenCode 权限动作无效: {other}"),
            };
        }
        let raw = if parsed.is_object() {
            raw_permission_value(thread, &rules).to_string()
        } else {
            rules.to_string()
        };
        db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw))
    }

    async fn sync_thread_execution_policy(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        approval_policy: &Value,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
        let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
            // 尚未建立 OpenCode session 时只保留 AuraCoder 的权限配置，后续创建 session
            // 会继续使用该配置生成权限规则。
            return Ok(());
        };
        let cwd = self.thread_cwd(&workspace, thread).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_opencode_runtime_service::runtime(&workspace)
                .await?
                .set_session_permission_mode(&cwd, engine_thread_id, approval_policy)
                .await
                .with_context(|| {
                    format!(
                        "同步 SSH OpenCode session 执行权限失败: thread_id={} engine_thread_id={}",
                        thread.id, engine_thread_id
                    )
                })
        } else {
            self.local_engine(&cwd)
                .await?
                .set_session_permission_mode(&cwd, engine_thread_id, approval_policy)
                .await
                .with_context(|| {
                    format!(
                        "同步本机 OpenCode session 执行权限失败: thread_id={} engine_thread_id={}",
                        thread.id, engine_thread_id
                    )
                })
        }?;

        // OpenCode 的 ask/allow/deny 规则由 OpenCode 实现层解释；切换到
        // allow 或 deny 时，同时处理该线程已有的待处理权限请求。
        if let Some(action) = approval_policy.as_str().filter(|value| *value != "ask") {
            let db = self.state.db.clone();
            let thread_id = thread.id.clone();
            let approval_ids = tokio::task::spawn_blocking(move || {
                db::actions::find_pending_opencode_permission_approval_ids(&db, &thread_id)
            })
            .await
            .context("查询 OpenCode 待处理权限请求失败")??;
            let decision = if action == "allow" {
                "accept_for_session"
            } else {
                "decline"
            };
            for approval_id in approval_ids {
                crate::commands::chat::respond_to_approval_inner(
                    &self.state,
                    thread.id.clone(),
                    approval_id,
                    json!({ "decision": decision }),
                )
                .await
                .map_err(anyhow::Error::msg)?;
            }
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
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
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
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
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

    async fn set_thread_name(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        _engine_thread_id: &str,
        _name: &str,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        Self::validate_thread(context, thread)?;
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
    }

    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        let workspace = self.load_workspace(context).await?;
        let cwd = self.resolve_workspace_cwd(&workspace, cwd).await?;
        if context.location_kind == CliLocationKind::Local {
            return extensions::refresh::load_cached_catalog(
                &self.state,
                "opencode",
                Some(cwd.as_str()),
            )
            .await;
        }

        let runtime = self
            .get_opencode_runtime_catalog(context, cwd.as_str())
            .await?;
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
        let fetched_at = chrono::Utc::now().to_rfc3339();
        let kind_fetched_at = ["agent", "command", "mcp"]
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
        let workspace = self.load_workspace(context).await?;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 OpenCode 当前不执行扩展变更，也不会调用本机 OpenCode"
        );
        let cwd = self.resolve_workspace_cwd(&workspace, None).await?;
        extensions::opencode::perform_action(&item, action, Some(cwd.as_str())).await
    }

    /// 用户未选择 workspace 时，使用本机用户级 OpenCode 配置执行全局扩展动作。
    async fn perform_global_extension_action(
        &self,
        item: ExtensionItemDto,
        action: &str,
        _scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto> {
        extensions::opencode::perform_action(&item, action, None).await
    }

    async fn fork_thread(
        &self,
        context: &CliExecutionContext,
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

    async fn compact_thread(
        &self,
        context: &CliExecutionContext,
        _engine_thread_id: &str,
    ) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(permission_mode: Option<&str>, metadata: Option<Value>) -> ThreadDto {
        ThreadDto {
            id: "thread".to_string(),
            workspace_id: "workspace".to_string(),
            engine_id: "opencode".to_string(),
            model_id: "model".to_string(),
            engine_thread_id: None,
            engine_metadata: metadata,
            plan_mode: None,
            send_method: None,
            reasoning_effort: None,
            permission_mode: permission_mode.map(str::to_string),
            title: "thread".to_string(),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            context_current_tokens: None,
            context_max_tokens: None,
            context_usage_updated_at: None,
            created_at: String::new(),
            last_activity_at: String::new(),
        }
    }

    #[test]
    fn permissions_read_empty_and_legacy_modes() {
        assert_eq!(
            raw_rules_from_thread(&thread(None, None)).unwrap(),
            json!([])
        );
        let rules =
            raw_rules_from_thread(&thread(Some(r#"{"approvalPolicy":"ask"}"#), None)).unwrap();
        assert_eq!(rules.as_array().map(Vec::len), Some(2));
        assert_eq!(rules[0]["action"], json!("ask"));
    }

    #[test]
    fn permissions_last_global_rule_ignores_question_allow() {
        let rules = json!([
            { "permission": "*", "pattern": "*", "action": "ask" },
            { "permission": "question", "pattern": "*", "action": "allow" }
        ]);
        let action = rules.as_array().unwrap().iter().rev().find_map(|rule| {
            (rule.get("permission").and_then(Value::as_str) == Some("*")
                && rule.get("pattern").and_then(Value::as_str) == Some("*"))
            .then(|| rule.get("action").and_then(Value::as_str))
            .flatten()
        });
        assert_eq!(action, Some("ask"));
    }

    #[test]
    fn permissions_save_preserves_unknown_raw_fields() {
        let thread = thread(
            Some(r#"{"permission":[],"unknown":{"keep":true},"sessionMode":"build"}"#),
            None,
        );
        let raw = raw_permission_value(
            &thread,
            &json!([{ "permission": "*", "pattern": "*", "action": "allow" }]),
        );
        assert_eq!(raw["permission"][0]["action"], json!("allow"));
        assert_eq!(raw["unknown"], json!({"keep": true}));
        assert_eq!(raw["sessionMode"], json!("build"));
    }

    #[test]
    fn permissions_invalid_json_returns_error() {
        let error = raw_rules_from_thread(&thread(Some("{bad"), None)).unwrap_err();
        assert!(error.to_string().contains("格式错误"));
    }
}
