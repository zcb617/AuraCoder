use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use super::{
    BaseCliMcp, CliExecutionContext, CliForkedThread, CliLocationKind, CliMcpRuntime,
    CliReviewStarted,
    CliRuntimePermissionPatch, CliRuntimePermissions, CliSessionNotFoundError, CliSessionSnapshot,
    CliTool, McpInvocationContext, McpToolResult,
};
use crate::{
    db,
    engines::{
        capabilities_for_engine,
        codex::{CodexEngine, CodexReviewStarted, CodexThreadNotFoundError},
        map_engine_capabilities, map_model_info, map_provider_usage, validate_engine_sandbox_mode,
        ApprovalRequestRoute, CodexRuntimeEvent, Engine, EngineCapabilities, EngineEvent,
        EngineSteerReceipt, EngineThread, ModelInfo, SandboxPolicy, ThreadScope,
        ThreadSyncSnapshot, TurnInput,
    },
    extensions,
    local_cli_service_lifecycle::{LocalCliHandle, LocalCliServiceLifecycle},
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, CodexAppDto, CodexPluginDto,
        CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionItemDto, OpenCodeRuntimeCatalogDto,
        PermissionComponentJson, ThreadDto, ThreadStatusDto, WorkspaceDto,
    },
    path_utils, remote_project_codex_runtime_service, remote_project_session_refresh_service, ssh,
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

fn set_or_remove(object: &mut serde_json::Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        object.insert(key.to_string(), value);
    } else {
        object.remove(key);
    }
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

/// 从现有 Codex raw object 复制权限字段，保留未知字段和 approvalsReviewer。
fn raw_permissions_value(
    thread: &ThreadDto,
    approval_raw: Option<&str>,
    sandbox_raw: Option<&str>,
    network_raw: Option<bool>,
) -> Value {
    let mut raw_object = thread
        .permission_mode
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    set_or_remove(
        &mut raw_object,
        "approvalPolicy",
        approval_raw.map(|value| json!(value)),
    );
    set_or_remove(
        &mut raw_object,
        "sandboxMode",
        sandbox_raw.map(|value| json!(value)),
    );
    set_or_remove(
        &mut raw_object,
        "allowNetwork",
        network_raw.map(|value| json!(value)),
    );
    if approval_raw.is_some() || sandbox_raw.is_some() || network_raw.is_some() {
        // permissionProfile 与显式三字段冲突时清理该旧聚合字段，其余未知字段原样保留。
        raw_object.remove("permissionProfile");
    }
    Value::Object(raw_object)
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

    /// 将历史或当前 Codex 原始权限转换为统一组件 JSON。
    ///
    /// `external_sandbox` 为 true 时按外部沙箱契约识别预设：保存端在该模式下
    /// 会抹掉 read-only/workspace-write 沙箱字段，识别端用同一前提把预设补回。
    fn permissions_from_thread(
        thread: &ThreadDto,
        external_sandbox: bool,
    ) -> Result<PermissionComponentJson> {
        let mut result = default_permission_component();
        let raw = thread.permission_mode.as_deref().unwrap_or("").trim();
        let parsed = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_str::<Value>(raw).context("Codex 权限 JSON 格式错误")?
        };
        anyhow::ensure!(
            parsed.is_null()
                || parsed.is_object()
                || parsed.as_array().is_some_and(|items| items.is_empty()),
            "Codex 权限 JSON 必须是对象、null 或空数组"
        );
        let mut object = parsed.as_object().cloned().unwrap_or_default();
        let approval = object
            .get("approvalPolicy")
            .or_else(|| object.get("sandboxApprovalPolicy"));
        let sandbox = object.get("sandboxMode");
        let network = object
            .get("allowNetwork")
            .or_else(|| object.get("networkPolicy"))
            .or_else(|| object.get("sandboxAllowNetwork"));
        // 旧运行时把 inherit 作为“未覆盖”写入；读取时必须还原成统一契约的 automatic。
        let approval_str = approval
            .and_then(Value::as_str)
            .filter(|value| *value != "inherit");
        let sandbox_str = sandbox
            .and_then(Value::as_str)
            .filter(|value| *value != "inherit");
        let network_bool = network.and_then(|value| {
            value.as_bool().or_else(|| match value.as_str() {
                Some("enabled") => Some(true),
                Some("restricted") => Some(false),
                Some("inherit") => None,
                _ => None,
            })
        });
        let preset = match (approval_str, sandbox_str, network_bool) {
            (Some("untrusted"), Some("read-only"), Some(false)) => Some("read-only"),
            (Some("on-request"), Some("workspace-write"), Some(false)) => Some("ask"),
            (Some("on-request"), Some("workspace-write"), Some(true)) => Some("auto"),
            (Some("never"), Some("danger-full-access"), Some(true)) => Some("full"),
            (None, None, None) => Some("automatic"),
            // 外部沙箱模式下保存端抹掉了沙箱字段，沙箱缺席 + 审批和网络匹配同样算对应预设。
            (Some("untrusted"), None, Some(false)) if external_sandbox => Some("read-only"),
            (Some("on-request"), None, Some(false)) if external_sandbox => Some("ask"),
            (Some("on-request"), None, Some(true)) if external_sandbox => Some("auto"),
            _ => None,
        };
        if let Some(preset) = preset {
            set_permission_array(&mut result, "autonomyPreset", &[preset]);
        } else {
            set_permission_array(&mut result, "autonomyPreset", &[]);
        }
        if approval.is_some_and(Value::is_object)
            || object
                .get("permissionProfile")
                .is_some_and(Value::is_object)
        {
            set_permission_array(&mut result, "approval", &[]);
            set_permission_array(&mut result, "autonomyPreset", &[]);
        }
        let approval_value = match approval_str {
            None => "automatic",
            Some("untrusted") => "restricted",
            Some("on-request") => "ask",
            Some("never") => "autonomous",
            Some(_) => "",
        };
        let approval_values: &[&str] = if approval_value.is_empty() {
            &[]
        } else {
            std::slice::from_ref(&approval_value)
        };
        set_permission_array(&mut result, "approval", approval_values);
        if approval.is_some_and(Value::is_object)
            || object
                .get("permissionProfile")
                .is_some_and(Value::is_object)
        {
            set_permission_array(&mut result, "approval", &[]);
        }
        let sandbox_value = match sandbox_str {
            None => "automatic",
            Some("read-only") => "read-only",
            Some("workspace-write") => "workspace-write",
            Some("danger-full-access") => "full-access",
            Some(_) => "",
        };
        let sandbox_values: &[&str] = if sandbox_value.is_empty() {
            &[]
        } else {
            std::slice::from_ref(&sandbox_value)
        };
        set_permission_array(&mut result, "sandbox", sandbox_values);
        let network_value = match network_bool {
            None => "automatic",
            Some(true) => "enabled",
            Some(false) => "restricted",
        };
        set_permission_array(&mut result, "network", &[network_value]);
        Ok(result)
    }

    /// 按统一组件契约生成 Codex 原始权限和兼容 metadata。
    async fn save_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        values: PermissionComponentJson,
    ) -> Result<PermissionComponentJson> {
        self.load_workspace(context).await?;
        anyhow::ensure!(thread.engine_id == "codex", "当前会话不属于 Codex");
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
        let sandbox = permission_choice(&values, "sandbox")?;
        let network = permission_choice(&values, "network")?;
        let autonomy_is_empty = values
            .get("autonomyPreset")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        let (approval_raw, mut sandbox_raw, network_raw) = match preset {
            Some("automatic") => (None, None, None),
            None if autonomy_is_empty
                || (approval.is_none() && sandbox.is_none() && network.is_none()) =>
            {
                (None, None, None)
            }
            Some("read-only") => (Some("untrusted"), Some("read-only"), Some(false)),
            Some("ask") => (Some("on-request"), Some("workspace-write"), Some(false)),
            Some("auto") => (Some("on-request"), Some("workspace-write"), Some(true)),
            Some("full") => (Some("never"), Some("danger-full-access"), Some(true)),
            _ => (
                match approval {
                    Some("restricted") => Some("untrusted"),
                    Some("ask") => Some("on-request"),
                    Some("autonomous") => Some("never"),
                    _ => None,
                },
                match sandbox {
                    Some("read-only") => Some("read-only"),
                    Some("workspace-write") => Some("workspace-write"),
                    Some("full-access") => Some("danger-full-access"),
                    _ => None,
                },
                match network {
                    Some("enabled") => Some(true),
                    Some("restricted") => Some(false),
                    _ => None,
                },
            ),
        };
        let external_sandbox = if context.location_kind == CliLocationKind::Ssh {
            false
        } else {
            self.local_engine().await?.uses_external_sandbox().await
        };
        if external_sandbox && matches!(sandbox_raw, Some("read-only" | "workspace-write")) {
            sandbox_raw = None;
        }
        let raw_value = raw_permissions_value(thread, approval_raw, sandbox_raw, network_raw);
        let raw = raw_value.to_string();
        let saved = db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw))?;
        let mut result = Self::permissions_from_thread(&saved, external_sandbox)?;
        for key in ["trust", "defaultForNewThreads"] {
            if let Some(value) = values.get(key) {
                result.insert(key.to_string(), value.clone());
            }
        }
        Ok(result)
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

#[async_trait]
impl CliTool for CodexCli {
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

    async fn get_chat_provider_usage(
        &self,
        context: &CliExecutionContext,
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

    fn subscribe_codex_runtime_events(&self) -> broadcast::Receiver<CodexRuntimeEvent> {
        self.state.engines.subscribe_codex_runtime_events()
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
            let result = remote_project_session_refresh_service::list_codex_sessions(
                service.local_port(),
                &workspace.root_path,
            )
            .await;
            // service_use.release().await;
            let mut sessions = result?
                .into_iter()
                .map(|session| CliSessionSnapshot {
                    engine_thread_id: session.engine_thread_id,
                    title: session.title,
                    preview: None,
                    cwd: session.cwd,
                    model_id: session.model_id,
                    reasoning_effort: session.reasoning_effort.clone(),
                    created_at: None,
                    updated_at: session.updated_at,
                    source_kind: None,
                    raw_status: Some(session.status.as_str().to_string()),
                    active_flags: Vec::new(),
                    status: session.status,
                    archived: false,
                    metadata: session.metadata,
                })
                .collect::<Vec<_>>();
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

    async fn get_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<PermissionComponentJson> {
        self.load_workspace(context).await?;
        anyhow::ensure!(thread.engine_id == "codex", "当前会话不属于 Codex");
        anyhow::ensure!(
            thread.workspace_id == context.workspace_id,
            "当前会话不属于该 workspace"
        );
        let external_sandbox = if context.location_kind == CliLocationKind::Ssh {
            false
        } else {
            self.local_engine().await?.uses_external_sandbox().await
        };
        let mut result = Self::permissions_from_thread(thread, external_sandbox)?;
        let preset = result
            .get("autonomyPreset")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str);
        if matches!(preset, Some("read-only" | "ask" | "auto")) && external_sandbox {
            result.insert("sandbox".to_string(), json!(["automatic"]));
        }
        Ok(result)
    }

    async fn set_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        values: PermissionComponentJson,
    ) -> Result<PermissionComponentJson> {
        self.save_permissions(context, thread, values).await
    }

    /// 将 Codex 的原始权限字段转换为统一运行时权限结构。
    async fn runtime_permissions(
        &self,
        _context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<CliRuntimePermissions> {
        let object = thread
            .permission_mode
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        Ok(CliRuntimePermissions {
            approval_policy: object
                .get("approvalPolicy")
                .or_else(|| object.get("sandboxApprovalPolicy"))
                .cloned(),
            sandbox_mode: object
                .get("sandboxMode")
                .and_then(Value::as_str)
                .map(str::to_owned),
            allow_network: object
                .get("allowNetwork")
                .or_else(|| object.get("sandboxAllowNetwork"))
                .and_then(Value::as_bool),
            permission_profile: object.get("permissionProfile").cloned(),
            approvals_reviewer: object
                .get("approvalsReviewer")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// 将统一权限补丁转换为 Codex 原始权限 JSON 并持久化到线程。
    async fn patch_runtime_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        patch: CliRuntimePermissionPatch,
    ) -> Result<ThreadDto> {
        self.load_workspace(context).await?;
        let updates_sandbox = patch.sandbox_mode.is_some();
        let updates_network = patch.allow_network.is_some();
        let updates_permission_profile = patch.permission_profile.is_some();
        let mut raw = thread
            .permission_mode
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Some(value) = patch.approval_policy {
            if let Some(value) = value.as_ref() {
                anyhow::ensure!(
                    value.is_string() || value.is_object(),
                    "Codex approval policy 必须是字符串或结构化对象"
                );
            }
            set_or_remove(&mut raw, "approvalPolicy", value);
        }
        if let Some(value) = patch.sandbox_mode {
            let value = if let Some(value) = value {
                let normalized = match value.trim().to_lowercase().as_str() {
                    "readonly" | "read-only" | "read_only" => "read-only",
                    "workspacewrite" | "workspace-write" | "workspace_write" => "workspace-write",
                    "dangerfullaccess" | "danger-full-access" | "danger_full_access" => {
                        "danger-full-access"
                    }
                    _ => value.as_str(),
                };
                validate_engine_sandbox_mode("codex", Some(normalized))
                    .map_err(anyhow::Error::msg)?;
                if matches!(normalized, "read-only" | "workspace-write")
                    && context.location_kind == CliLocationKind::Local
                    && self.local_engine().await?.uses_external_sandbox().await
                {
                    anyhow::bail!(
                        "Codex read-only and workspace-write sandbox overrides are unavailable while AuraCoder is using external sandbox mode."
                    );
                }
                Some(json!(normalized))
            } else {
                None
            };
            set_or_remove(&mut raw, "sandboxMode", value);
        }
        if let Some(value) = patch.allow_network {
            set_or_remove(&mut raw, "allowNetwork", value.map(|value| json!(value)));
        }
        /*
        // 旧实现仅按字段直接写入 permissionProfile，已由下方互斥规则接管：
        if let Some(value) = patch.permission_profile {
            set_or_remove(&mut raw, "permissionProfile", value);
        }
        */
        if let Some(value) = patch.approvals_reviewer {
            set_or_remove(
                &mut raw,
                "approvalsReviewer",
                value.map(|value| json!(value)),
            );
        }
        if (updates_sandbox || updates_network) && !updates_permission_profile {
            raw.remove("permissionProfile");
        }
        if let Some(value) = patch.permission_profile {
            if let Some(value) = value {
                raw.insert("permissionProfile".to_string(), value);
                raw.remove("sandboxMode");
                raw.remove("allowNetwork");
                raw.remove("sandboxAllowNetwork");
            } else {
                raw.remove("permissionProfile");
            }
        }
        let raw = Value::Object(raw).to_string();
        db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw))
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
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let remote_turn_use = self.remote_turn_use.lock().await;
            let engine = remote_turn_use
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("当前 SSH 远端 Codex 会话尚未建立持续使用关系"))?;
            return Engine::start_thread(
                engine.as_ref(),
                scope,
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
            thread.engine_thread_id.as_deref(),
            model,
            sandbox,
        )
        .await
    }

    async fn send_message(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.load_workspace(context).await?;
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
        _thread: &ThreadDto,
        engine_thread_id: &str,
        client_steer_id: &str,
        content: &str,
        input: TurnInput,
    ) -> Result<EngineSteerReceipt> {
        let workspace = self.load_workspace(context).await?;
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
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Local {
            let mut results = Vec::new();
            for kind in requested_kinds {
                results.push(
                    crate::extensions::codex::refresh_kind(self.state.engines.as_ref(), cwd, kind)
                        .await,
                );
            }
            return Ok(results);
        }

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

    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Local {
            return extensions::refresh::load_cached_catalog(
                &self.state,
                "codex",
                cwd.or(Some(workspace.root_path.as_str())),
            )
            .await;
        }

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
        let fetched_at = chrono::Utc::now().to_rfc3339();
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
        engine_thread_id: &str,
        cwd: &str,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<CliForkedThread> {
        self.load_workspace(context).await?;
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
    ) -> Result<()> {
        self.load_workspace(context).await?;
        anyhow::ensure!(
            context.location_kind == CliLocationKind::Local,
            "SSH 远端 Codex 暂未接入压缩，当前不会调用本机 Codex 执行"
        );
        self.local_engine()
            .await?
            .compact_thread(engine_thread_id)
            .await
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(permission_mode: Option<&str>, metadata: Option<Value>) -> ThreadDto {
        ThreadDto {
            id: "thread".to_string(),
            workspace_id: "workspace".to_string(),
            engine_id: "codex".to_string(),
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
            created_at: String::new(),
            last_activity_at: String::new(),
        }
    }

    #[test]
    fn permissions_read_empty_as_automatic() {
        let values = CodexCli::permissions_from_thread(&thread(None, None), false).unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["automatic"])));
        assert_eq!(values.get("approval"), Some(&json!(["automatic"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
    }

    #[test]
    fn permissions_read_legacy_network_policy_without_metadata_fallback() {
        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"on-request","sandboxMode":"workspace-write","networkPolicy":"enabled"}"#),
                None,
            ),
            false,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["auto"])));
        assert_eq!(values.get("network"), Some(&json!(["enabled"])));

        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"untrusted"}"#),
                Some(json!({"sandboxMode":"read-only","sandboxAllowNetwork":false})),
            ),
            false,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!([])));
        assert_eq!(values.get("approval"), Some(&json!(["restricted"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
        /*
        // 旧 metadata 回退已停用：
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["read-only"])));
        */
    }

    #[test]
    fn permissions_read_all_legacy_inherit_values_as_automatic() {
        let values = CodexCli::permissions_from_thread(&thread(
            Some(r#"{"approvalPolicy":"inherit","sandboxMode":"inherit","networkPolicy":"inherit"}"#),
            None,
        ), false)
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["automatic"])));
        assert_eq!(values.get("approval"), Some(&json!(["automatic"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
    }

    #[test]
    fn permissions_save_preserves_unknown_raw_fields_and_reviewer() {
        let thread = thread(
            Some(
                r#"{"approvalPolicy":"on-request","sandboxMode":"workspace-write","allowNetwork":false,"permissionProfile":{"old":true},"approvalsReviewer":"user","unknown":{"keep":true}}"#,
            ),
            None,
        );
        let raw = raw_permissions_value(
            &thread,
            Some("never"),
            Some("danger-full-access"),
            Some(true),
        );
        assert_eq!(raw["approvalPolicy"], json!("never"));
        assert_eq!(raw["sandboxMode"], json!("danger-full-access"));
        assert_eq!(raw["allowNetwork"], json!(true));
        assert_eq!(raw["approvalsReviewer"], json!("user"));
        assert_eq!(raw["unknown"], json!({"keep": true}));
        assert!(raw.get("permissionProfile").is_none());
    }

    #[test]
    fn permissions_reject_invalid_non_empty_json() {
        let error =
            CodexCli::permissions_from_thread(&thread(Some("[1]"), None), false).unwrap_err();
        assert!(error.to_string().contains("必须是对象"));
    }

    #[test]
    fn permissions_read_external_sandbox_presets_without_sandbox_field() {
        // 外部沙箱模式下保存端抹掉沙箱字段，识别端按同一契约补回预设。
        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"untrusted","allowNetwork":false}"#),
                None,
            ),
            true,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["read-only"])));

        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"on-request","allowNetwork":false}"#),
                None,
            ),
            true,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["ask"])));

        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"on-request","allowNetwork":true}"#),
                None,
            ),
            true,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["auto"])));
    }

    #[test]
    fn permissions_read_missing_sandbox_stays_custom_without_external_sandbox() {
        // 同样的三元组在外部沙箱关闭时仍是用户手工组合，必须保持自定义。
        let values = CodexCli::permissions_from_thread(
            &thread(
                Some(r#"{"approvalPolicy":"on-request","allowNetwork":false}"#),
                None,
            ),
            false,
        )
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!([])));
    }
}
