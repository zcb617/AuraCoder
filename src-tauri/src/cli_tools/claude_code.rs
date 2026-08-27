use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use super::{
    claude_code_session_lifecycle::{
        shared_claude_code_session_handles, ClaudeCodeSessionHandleRegistry,
    },
    CliExecutionContext, CliForkedThread, CliLocationKind, CliReviewStarted,
    CliRuntimePermissionPatch, CliRuntimePermissions, CliSessionNotFoundError,
    CliSessionSnapshot, CliTool,
};
use crate::{
    config::app_config::ClaudeCodeSessionMode,
    db,
    engines::{
        capabilities_for_engine, claude_remote::RemoteClaudeSessionNotFoundError,
        claude_sidecar::ClaudeSidecarEngine, map_engine_capabilities, map_model_info,
        map_provider_usage, ApprovalRequestRoute, CodexRuntimeEvent, Engine, EngineCapabilities,
        EngineEvent, EngineSteerReceipt, EngineThread, ModelInfo, SandboxPolicy, ThreadScope,
        ThreadSyncSnapshot, TurnInput,
    },
    extensions,
    local_cli_service_lifecycle::{LocalCliHandle, LocalCliServiceLifecycle},
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, CodexAppDto, CodexPluginDto,
        CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionCatalogRefreshErrorDto, ExtensionItemDto,
        OpenCodeRuntimeCatalogDto, PermissionComponentJson, ThreadDto, ThreadStatusDto, WorkspaceDto,
    },
    path_utils, remote_project_claude_runtime_service, ssh,
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
    values.insert(key.to_string(), Value::Array(items.iter().map(|item| json!(item)).collect()));
}

fn set_or_remove(object: &mut serde_json::Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value { object.insert(key.to_string(), value); } else { object.remove(key); }
}

fn permission_choice<'a>(values: &'a PermissionComponentJson, key: &str) -> Result<Option<&'a str>> {
    let value = values.get(key).context(format!("缺少权限参数: {key}"))?;
    let array = value.as_array().context(format!("权限参数必须是数组: {key}"))?;
    Ok(array.first().and_then(Value::as_str))
}

fn validate_permission_component(values: &PermissionComponentJson) -> Result<()> {
    let allowed = [
        ("autonomyPreset", ["automatic", "read-only", "ask", "auto", "full"].as_slice()),
        ("trust", ["automatic", "trusted", "standard", "restricted"].as_slice()),
        ("approval", ["automatic", "restricted", "ask", "autonomous"].as_slice()),
        ("sandbox", ["automatic", "read-only", "workspace-write", "full-access"].as_slice()),
        ("network", ["automatic", "enabled", "restricted"].as_slice()),
        ("defaultForNewThreads", ["automatic", "read-only", "ask", "auto", "full"].as_slice()),
    ];
    for key in values.keys() {
        anyhow::ensure!(allowed.iter().any(|(name, _)| name == key), "未知权限参数: {key}");
    }
    for (key, choices) in allowed {
        let value = values.get(key).context(format!("缺少权限参数: {key}"))?;
        let array = value.as_array().context(format!("权限参数必须是数组: {key}"))?;
        anyhow::ensure!(array.len() <= 1, "权限参数数组最多只能有一个值: {key}");
        for item in array {
            let item = item.as_str().context(format!("权限参数值必须是字符串: {key}"))?;
            anyhow::ensure!(choices.contains(&item), "权限参数值不支持: {key}={item}");
        }
    }
    Ok(())
}

/// 将 Claude Code 的原始权限字段和兼容 metadata 转换为前端统一权限 JSON。
///
/// 该函数只负责数据适配，不访问 CLI 生命周期；本地和 SSH 读取路径都通过
/// `get_permissions` 调用同一套转换逻辑。
fn permissions_from_thread(thread: &ThreadDto) -> Result<PermissionComponentJson> {
    let raw = thread.permission_mode.as_deref().unwrap_or("").trim();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(raw).context("Claude 权限 JSON 格式错误")?
    };
    anyhow::ensure!(
        parsed.is_null()
            || parsed.is_object()
            || parsed.as_array().is_some_and(|items| items.is_empty()),
        "Claude 权限 JSON 必须是对象、null 或空数组"
    );

    let mut object = parsed.as_object().cloned().unwrap_or_default();
    let mode = object
        .get("permissionMode")
        .or_else(|| object.get("approvalPolicy"))
        .or_else(|| object.get("claudePermissionMode"))
        .and_then(Value::as_str)
        .filter(|value| *value != "inherit");
    let sandbox = object
        .get("sandboxMode")
        .and_then(Value::as_str)
        .filter(|value| *value != "inherit");
    let network = object
        .get("allowNetwork")
        .or_else(|| object.get("networkPolicy"))
        .or_else(|| object.get("sandboxAllowNetwork"))
        .and_then(|value| {
                value.as_bool().or_else(|| match value.as_str() {
                    Some("enabled") => Some(true),
                    Some("restricted") => Some(false),
                    Some("inherit") => None,
                    _ => None,
                })
        });

    let mut result = default_permission_component();
    let preset = match (mode, sandbox, network) {
        (Some("dontAsk"), Some("read-only"), Some(false)) => Some("read-only"),
        (Some("default"), Some("workspace-write"), Some(false)) => Some("ask"),
        (Some("acceptEdits"), Some("workspace-write"), None) => Some("auto"),
        (Some("bypassPermissions"), Some("workspace-write"), Some(true)) => Some("full"),
        (Some("restricted"), Some("read-only"), Some(false)) => Some("read-only"),
        (Some("standard"), Some("workspace-write"), Some(false)) => Some("ask"),
        (Some("trusted"), Some("workspace-write"), Some(true)) => Some("full"),
        (Some("trusted"), Some("workspace-write"), None) => Some("auto"),
        (None, None, None) => Some("automatic"),
        _ => None,
    };
    if let Some(preset) = preset {
        set_permission_array(&mut result, "autonomyPreset", &[preset]);
    } else {
        set_permission_array(&mut result, "autonomyPreset", &[]);
    }

    let approval_value = match mode {
        None => "automatic",
        Some("dontAsk") => "restricted",
        Some("default") => "ask",
        Some("acceptEdits") => "autonomous",
        Some("bypassPermissions") => "autonomous",
        Some("restricted") => "restricted",
        Some("standard") => "ask",
        Some("trusted") => "autonomous",
        Some(_) => "",
    };
    let approval_values: &[&str] = if approval_value.is_empty() {
        &[]
    } else {
        std::slice::from_ref(&approval_value)
    };
    set_permission_array(&mut result, "approval", approval_values);

    /*
    // 原先按 permissionMode 强制推导 sandbox 的实现保留如下，现由实际保存字段读取接替：
    let sandbox_value = match (mode, sandbox) {
        (Some("dontAsk"), _) => "read-only",
        (Some("default") | Some("acceptEdits") | Some("bypassPermissions"), _) => "workspace-write",
        (_, Some("read-only")) => "read-only",
        (_, Some("workspace-write")) => "workspace-write",
        (_, None) => "automatic",
        _ => "",
    };
    */
    let sandbox_value = match sandbox {
        None => "automatic",
        Some("read-only") => "read-only",
        Some("workspace-write") => "workspace-write",
        Some("full-access") | Some(_) => "",
    };
    let sandbox_values: &[&str] = if sandbox_value.is_empty() {
        &[]
    } else {
        std::slice::from_ref(&sandbox_value)
    };
    set_permission_array(&mut result, "sandbox", sandbox_values);

    /*
    // 原先按 permissionMode 强制推导 network 的实现保留如下，现由实际保存字段读取接替：
    let network_value = match (mode, network) {
        (Some("dontAsk"), _) => "restricted",
        (Some("default"), _) => "restricted",
        (Some("bypassPermissions"), _) => "enabled",
        (_, Some(true)) => "enabled",
        (_, Some(false)) => "restricted",
        (_, None) => "automatic",
        _ => "automatic",
    };
    */
    let network_value = match network {
        None => "automatic",
        Some(true) => "enabled",
        Some(false) => "restricted",
    };
    set_permission_array(&mut result, "network", &[network_value]);
    Ok(result)
}

/// 从现有 Claude raw object 复制新权限字段，清理被替代的旧字段并保留未知字段。
fn raw_permissions_value(
    thread: &ThreadDto,
    mode: Option<&str>,
    sandbox_mode: Option<&str>,
    allow_network: Option<bool>,
) -> Value {
    let mut raw = thread
        .permission_mode
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    raw.entry("permissionMode".to_string()).or_insert(Value::Null);
    raw.entry("sandboxMode".to_string()).or_insert(Value::Null);
    raw.entry("allowNetwork".to_string()).or_insert(Value::Null);
    raw.entry("allow".to_string()).or_insert_with(|| json!([]));
    raw.entry("ask".to_string()).or_insert_with(|| json!([]));
    raw.entry("deny".to_string()).or_insert_with(|| json!([]));
    set_or_remove(&mut raw, "permissionMode", mode.map(|value| json!(value)));
    set_or_remove(
        &mut raw,
        "sandboxMode",
        sandbox_mode.map(|value| json!(value)),
    );
    set_or_remove(
        &mut raw,
        "allowNetwork",
        allow_network.map(|value| json!(value)),
    );
    // 保存新字段时清理旧字段，避免读取时旧 approvalPolicy/networkPolicy
    // 与新 permissionMode/allowNetwork 产生两套来源；其他字段继续保留。
    raw.remove("approvalPolicy");
    raw.remove("networkPolicy");
    Value::Object(raw)
}

/// Claude Code 对统一 CLI 操作接口的实现。
pub struct ClaudeCodeCli {
    state: AppState,
    remote_turn_use:
        Arc<Mutex<Option<remote_project_claude_runtime_service::RemoteClaudeServiceUse>>>,
    session_handles: Arc<ClaudeCodeSessionHandleRegistry>,
}

impl Clone for ClaudeCodeCli {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            remote_turn_use: self.remote_turn_use.clone(),
            session_handles: self.session_handles.clone(),
        }
    }
}

impl ClaudeCodeCli {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            remote_turn_use: Arc::new(Mutex::new(None)),
            session_handles: shared_claude_code_session_handles(),
        }
    }

    async fn local_engine(&self) -> Result<Arc<ClaudeSidecarEngine>> {
        let service = LocalCliServiceLifecycle::get("claude").await?;
        match service.handle() {
            LocalCliHandle::Claude(engine) => Ok(engine.clone()),
            _ => anyhow::bail!("本地 CLI 生命周期返回了错误的 Claude Code 句柄类型"),
        }
    }

    /// 用户进入某个 workspace 后，建立 Claude Code 的本机或 SSH 执行目标；未传 workspace 时只使用默认本机 workspace。
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
        .context("读取 Claude Code workspace 任务失败")??;
        CliExecutionContext::from_workspace(&workspace)
    }

    /// 用户刷新某个项目目录的 Claude Code 扩展时，找到该目录所属的 workspace，保证 SSH 项目不会误用本机 Claude Code。
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
                let repos = db::repos::get_repos(&db, &workspace.id)?;
                if repos
                    .iter()
                    .any(|repo| path_utils::paths_equal(&repo.path, &cwd))
                {
                    return Ok(Some(workspace));
                }
            }
            Ok(None)
        })
        .await
        .context("按项目目录读取 Claude Code workspace 任务失败")??;
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
            "当前 workspace 的项目目录与 Claude Code 操作目标不一致"
        );

        match context.location_kind {
            CliLocationKind::Local => {
                anyhow::ensure!(
                    workspace.location_kind != "ssh",
                    "当前 workspace 是 SSH 远端项目，不能使用本机 Claude Code"
                );
            }
            CliLocationKind::Ssh => {
                anyhow::ensure!(
                    workspace.location_kind == "ssh",
                    "当前 workspace 不是 SSH 远端项目"
                );
                anyhow::ensure!(
                    workspace.ssh_connection_id == context.ssh_connection_id,
                    "当前 workspace 的 SSH 连接与 Claude Code 操作目标不一致"
                );
                remote_project_claude_runtime_service::validate_remote_claude_workspace(
                    &workspace,
                )?;
            }
        }
        Ok(workspace)
    }

    async fn remote_connection(
        &self,
        workspace: &WorkspaceDto,
    ) -> Result<db::ssh_connections::SshConnectionRecord> {
        let connection_id =
            remote_project_claude_runtime_service::validate_remote_claude_workspace(workspace)?
                .to_string();
        let db = self.state.db.clone();
        let lookup_connection_id = connection_id.clone();
        tokio::task::spawn_blocking(move || db::ssh_connections::find(&db, &lookup_connection_id))
            .await
            .context("读取 SSH 连接任务失败")??
            .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在: {connection_id}"))
    }

    async fn remote_extension_catalog(
        &self,
        workspace: &WorkspaceDto,
    ) -> Result<CachedExtensionCatalogDto> {
        let connection = self.remote_connection(workspace).await?;
        let project_root = ssh::runtime::quote_posix(&workspace.root_path);
        let plugin_command = ssh::runtime::wrap_remote_login_shell_command(&format!(
            "cd -- {project_root} && env claude plugin list --available --json"
        ));
        let mut refresh_errors = Vec::new();
        let mut plugins = match ssh::gateway::run_command(&connection, &plugin_command).await {
            Ok(plugin_output) => match serde_json::from_str::<Value>(&plugin_output) {
                Ok(plugin_value) => {
                    extensions::claude::parse_plugins(&plugin_value, &HashMap::new())
                }
                Err(error) => {
                    log::warn!("解析 SSH 远端 Claude Plugin 目录失败: {error:#}");
                    refresh_errors.push(ExtensionCatalogRefreshErrorDto {
                        kind: "plugin".to_string(),
                        code: "parse_failed".to_string(),
                    });
                    Vec::new()
                }
            },
            Err(error) => {
                log::warn!("读取 SSH 远端 Claude Plugin 目录失败: {error:#}");
                refresh_errors.push(ExtensionCatalogRefreshErrorDto {
                    kind: "plugin".to_string(),
                    code: "read_failed".to_string(),
                });
                Vec::new()
            }
        };

        let project_skill_root = format!(
            "{}/.claude/skills",
            workspace.root_path.trim_end_matches('/')
        );
        let plugin_skill_roots = plugins
            .iter()
            .filter(|item| item.installed == Some(true))
            .filter_map(|item| {
                item.path.as_ref().map(|path| {
                    (
                        format!("{}/skills", path.trim_end_matches('/')),
                        item.id.clone(),
                        item.enabled.unwrap_or(true),
                    )
                })
            })
            .collect::<Vec<_>>();
        let quoted_plugin_skill_roots = plugin_skill_roots
            .iter()
            .map(|(path, _, _)| ssh::runtime::quote_posix(path))
            .collect::<Vec<_>>()
            .join(" ");
        let skill_command = ssh::runtime::wrap_remote_login_shell_command(&format!(
            "find \"$HOME/.claude/skills\" {project_root}/.claude/skills {quoted_plugin_skill_roots} -type f -name SKILL.md -print 2>/dev/null || true"
        ));
        let mut skills = match ssh::gateway::run_command(&connection, &skill_command).await {
            Ok(skill_output) => skill_output
                .lines()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(|path| {
                    let plugin = plugin_skill_roots
                        .iter()
                        .find(|(root, _, _)| path == root || path.starts_with(&format!("{root}/")));
                    let (scope, parent_plugin_id, enabled) = if path == project_skill_root
                        || path.starts_with(&format!("{project_skill_root}/"))
                    {
                        ("project", None, true)
                    } else if let Some((_, plugin_id, plugin_enabled)) = plugin {
                        ("plugin", Some(plugin_id.clone()), *plugin_enabled)
                    } else {
                        ("user", None, true)
                    };
                    let name = Path::new(path)
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|value| value.to_str())
                        .unwrap_or("Skill")
                        .to_string();
                    ExtensionItemDto {
                        id: path.to_string(),
                        provider_id: "claude".to_string(),
                        kind: "skill".to_string(),
                        name,
                        description: None,
                        version: None,
                        scope: scope.to_string(),
                        source: None,
                        marketplace: None,
                        path: Some(path.to_string()),
                        parent_plugin_id,
                        category: None,
                        officially_available: false,
                        catalog_authority: None,
                        installed: Some(true),
                        configured: None,
                        enabled: Some(enabled),
                        health: if enabled { "healthy" } else { "unknown" }.to_string(),
                        auth_state: None,
                        available_actions: Vec::new(),
                        requires_new_session: false,
                        read_only_reason: Some("ssh_remote_claude_extension_action".to_string()),
                        warning: None,

                        ..Default::default()
                    }
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                log::warn!("读取 SSH 远端 Claude Skill 目录失败: {error:#}");
                refresh_errors.push(ExtensionCatalogRefreshErrorDto {
                    kind: "skill".to_string(),
                    code: "read_failed".to_string(),
                });
                Vec::new()
            }
        };

        let mcp_command = ssh::runtime::wrap_remote_login_shell_command(&format!(
            "cd -- {project_root} && env claude mcp list"
        ));
        let mut mcp_servers = match ssh::gateway::run_command(&connection, &mcp_command).await {
            Ok(mcp_output) => extensions::claude::parse_mcp_servers(&mcp_output),
            Err(error) => {
                log::warn!("读取 SSH 远端 Claude MCP 目录失败: {error:#}");
                refresh_errors.push(ExtensionCatalogRefreshErrorDto {
                    kind: "mcp".to_string(),
                    code: "read_failed".to_string(),
                });
                Vec::new()
            }
        };
        for item in plugins
            .iter_mut()
            .chain(skills.iter_mut())
            .chain(mcp_servers.iter_mut())
        {
            item.available_actions.clear();
            item.read_only_reason = Some("ssh_remote_claude_extension_action".to_string());
        }
        let mut items = Vec::new();
        items.append(&mut skills);
        items.append(&mut plugins);
        items.append(&mut mcp_servers);
        let fetched_at = chrono::Utc::now().to_rfc3339();
        let kind_fetched_at = ["skill", "plugin", "mcp"]
            .into_iter()
            .map(|kind| (kind.to_string(), Some(fetched_at.clone())))
            .collect();

        Ok(CachedExtensionCatalogDto {
            provider_id: "claude".to_string(),
            cwd: Some(workspace.root_path.clone()),
            items,
            sources: Vec::new(),
            capabilities: extensions::provider_capabilities("claude"),
            fetched_at: Some(fetched_at.clone()),
            kind_fetched_at,
            last_attempt_at: Some(fetched_at.clone()),
            next_refresh_at: None,
            refreshing: false,
            refresh_completed_at: Some(fetched_at),
            has_snapshot: true,
            refresh_errors,
        })
    }

    // 旧实现把远端 Claude 会话快照交给公共会话刷新服务转换。会话查询和解析属于
    // Claude Code 业务，现已内联到 CliTool::list_sessions，不再经由外部服务转换。
    // fn map_session(
    //     session: remote_project_session_refresh_service::RemoteSessionSnapshot,
    // ) -> CliSessionSnapshot {
    //     let raw_status = Some(session.status.as_str().to_string());
    //     CliSessionSnapshot {
    //         engine_thread_id: session.engine_thread_id,
    //         title: session.title,
    //         preview: None,
    //         cwd: session.cwd,
    //         model_id: session.model_id,
    //         created_at: None,
    //         updated_at: session.updated_at,
    //         source_kind: Some("claude".to_string()),
    //         raw_status,
    //         active_flags: Vec::new(),
    //         status: session.status,
    //         archived: false,
    //         metadata: session.metadata,
    //     }
    // }

    fn uses_reuse_session(&self, context: &CliExecutionContext) -> bool {
        context.location_kind == CliLocationKind::Ssh
            && self.state.config.claude_code.session_mode() == ClaudeCodeSessionMode::ReuseSession
    }

    fn unsupported(action: &str) -> anyhow::Error {
        anyhow::anyhow!("Claude Code 当前不支持{action}，不会调用 Codex、OpenCode 或本机替代实现")
    }
}

#[async_trait]
impl CliTool for ClaudeCodeCli {
    fn id(&self) -> &str {
        "claude"
    }

    fn name(&self) -> &str {
        "Claude"
    }

    fn capabilities(&self) -> EngineCapabilities {
        capabilities_for_engine("claude")
    }

    async fn execution_context(&self, workspace_id: Option<&str>) -> Result<CliExecutionContext> {
        ClaudeCodeCli::execution_context(self, workspace_id).await
    }

    async fn execution_context_for_cwd(&self, cwd: Option<&str>) -> Result<CliExecutionContext> {
        ClaudeCodeCli::execution_context_for_cwd(self, cwd).await
    }

    async fn get_engine_info(&self, context: &CliExecutionContext) -> Result<EngineInfoDto> {
        /*
        旧实现先通过 workspace 构造远端运行对象，再读取模型。模型目录属于机器，不再执行：
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let models = remote_project_claude_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime()
                .await?;
            anyhow::ensure!(!models.is_empty(), "SSH 远端 Claude 未返回可用模型");
            return Ok(EngineInfoDto {
                id: "claude".to_string(),
                name: "Claude".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("claude")),
            });
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 Claude 未绑定连接")?;
            let models =
                remote_project_claude_runtime_service::model_infos(connection_id, None).await?;
            return Ok(EngineInfoDto {
                id: "claude".to_string(),
                name: "Claude".to_string(),
                models: models.into_iter().map(map_model_info).collect(),
                capabilities: map_engine_capabilities(capabilities_for_engine("claude")),
            });
        }

        let engine = self.local_engine().await?;
        let models = engine.list_models_runtime().await;
        Ok(EngineInfoDto {
            id: "claude".to_string(),
            name: "Claude".to_string(),
            models: models.into_iter().map(map_model_info).collect(),
            capabilities: map_engine_capabilities(capabilities_for_engine("claude")),
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
            let models = remote_project_claude_runtime_service::runtime(&workspace)
                .await?
                .list_models_runtime()
                .await?;
            anyhow::ensure!(!models.is_empty(), "SSH 远端 Claude 未返回可用模型");
            return Ok(models);
        }
        */
        if context.location_kind == CliLocationKind::Ssh {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .context("SSH 远端 Claude 未绑定连接")?;
            return remote_project_claude_runtime_service::model_infos(connection_id, None).await;
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
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return Ok(Some(ChatProviderUsageDto {
                engine_id: "claude".to_string(),
                name: "Claude".to_string(),
                available: false,
                windows: Vec::new(),
            }));
        }
        let engine = self.local_engine().await?;
        Ok(Some(map_provider_usage(
            "claude",
            "Claude",
            engine.usage_limits_snapshot().await,
        )))
    }

    async fn engine_health(&self, context: &CliExecutionContext) -> Result<EngineHealthDto> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Local {
            let report = self.local_engine().await?.health_report().await;
            return Ok(EngineHealthDto {
                id: "claude".to_string(),
                available: report.available,
                version: report.version,
                details: Some(report.details),
                warnings: report.warnings,
                checks: report.checks,
                fixes: report.fixes,
                protocol_diagnostics: None,
            });
        }

        let connection = self.remote_connection(&workspace).await?;

        let availability = match remote_project_claude_runtime_service::runtime(&workspace).await {
            Ok(engine) => engine.prewarm().await,
            Err(error) => Err(error),
        };
        let version = if availability.is_ok() {
            let command = ssh::runtime::wrap_remote_login_shell_command("claude --version");
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
            id: "claude".to_string(),
            available: availability.is_ok(),
            version,
            details: Some(match availability {
                Ok(()) => format!("SSH 远端 Claude：{connection_name}"),
                Err(error) => format!("SSH 远端 Claude 不可用：{error:#}"),
            }),
            warnings: Vec::new(),
            checks: Vec::new(),
            fixes: Vec::new(),
            protocol_diagnostics: None,
        })
    }

    fn subscribe_codex_runtime_events(&self) -> broadcast::Receiver<CodexRuntimeEvent> {
        let (sender, receiver) = broadcast::channel(1);
        drop(sender);
        receiver
    }

    async fn prewarm_engine(&self, context: &CliExecutionContext) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            remote_project_claude_runtime_service::runtime(&workspace)
                .await?
                .prewarm()
                .await
        } else {
            self.local_engine().await?.prewarm().await
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
        if context.location_kind == CliLocationKind::Local || archived == Some(true) {
            return Ok(Vec::new());
        }

        let connection_id =
            remote_project_claude_runtime_service::validate_remote_claude_workspace(&workspace)?;
        // 旧实现通过临时 Tunnel 占用控制远端服务启停：
        // let service_use =
        //     remote_project_claude_runtime_service::acquire_temporary(&workspace).await?;
        let service = ssh::cli_service_lifecycle::get(connection_id, "claude").await?;
        let result = async {
            reqwest::Client::new()
                .get(format!(
                    "http://127.0.0.1:{}/sessions",
                    service.local_port()
                ))
                .query(&[("cwd", workspace.root_path.as_str())])
                .send()
                .await
                .context("读取 SSH 远端 Claude 会话失败")?
                .error_for_status()
                .context("SSH 远端 Claude 会话读取被拒绝")?
                .json::<Vec<Value>>()
                .await
                .context("解析 SSH 远端 Claude 会话失败")
        }
        .await;
        // service_use.release().await;
        let query = search_term.map(str::trim).filter(|value| !value.is_empty());
        result.map(|values| {
            values
                .into_iter()
                .filter_map(|value| {
                    let cwd = value.get("cwd")?.as_str()?.to_string();
                    if !path_utils::paths_equal(&cwd, &workspace.root_path) {
                        return None;
                    }
                    let engine_thread_id = value.get("id")?.as_str()?.to_string();
                    let title = value
                        .get("title")
                        .or_else(|| value.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| engine_thread_id.clone());
                    if query.is_some_and(|query| {
                        !title.to_lowercase().contains(&query.to_lowercase())
                            && !engine_thread_id.contains(query)
                    }) {
                        return None;
                    }
                    let updated_at = value
                        .get("updatedAt")
                        .or_else(|| value.get("updated_at"))
                        .and_then(|value| {
                            value.as_str().map(str::to_string).or_else(|| {
                                value.as_i64().and_then(|timestamp| {
                                    chrono::DateTime::from_timestamp(
                                        if timestamp > 10_000_000_000 {
                                            timestamp / 1000
                                        } else {
                                            timestamp
                                        },
                                        0,
                                    )
                                    .map(|date| date.to_rfc3339())
                                })
                            })
                        });
                    Some(CliSessionSnapshot {
                        engine_thread_id,
                        title,
                        preview: None,
                        cwd: cwd.clone(),
                        model_id: "unknown".to_string(),
                        reasoning_effort: None,
                        created_at: None,
                        updated_at,
                        source_kind: Some("claude".to_string()),
                        raw_status: Some("idle".to_string()),
                        active_flags: Vec::new(),
                        status: ThreadStatusDto::Idle,
                        archived: false,
                        metadata: json!({
                            "sshRemote": true,
                            "claudeRemoteCwd": cwd,
                            "claudeRemote": value,
                        }),
                    })
                })
                .collect()
        })
    }

    async fn read_session(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<CliSessionSnapshot> {
        // 旧实现：先读取当前 workspace 的会话列表，再在内存中按 ID 查找。
        // 该逻辑保留为注释，不能重新启用为 SSH 远端按 ID 查询的后备路径：
        // self.list_sessions(context, None, Some(false))
        //     .await?
        //     .into_iter()
        //     .find(|session| session.engine_thread_id == engine_thread_id)
        //     .ok_or_else(|| {
        //         anyhow::anyhow!(
        //             "Claude Code 会话不存在或目录不匹配: session_id={engine_thread_id}"
        //         )
        //     })
        //
        // 本机行为保持原状；SSH 远端只能使用 Claude 自己的按 ID 协议请求，
        // 不能在远端失败后回退本机或旧的列表查询。
        if context.location_kind == CliLocationKind::Local {
            // 迁移留痕：旧实现通过列表查询后自行构造普通 anyhow 错误，完整保留但禁止恢复执行。
            // return self
            //     .list_sessions(context, None, Some(false))
            //     .await?
            //     .into_iter()
            //     .find(|session| session.engine_thread_id == engine_thread_id)
            //     .ok_or_else(|| {
            //         anyhow::anyhow!(
            //             "Claude Code 会话不存在或目录不匹配: session_id={engine_thread_id}"
            //         )
            //     });
            return self
                .list_sessions(context, None, Some(false))
                .await?
                .into_iter()
                .find(|session| session.engine_thread_id == engine_thread_id)
                .ok_or_else(|| {
                    anyhow::Error::new(CliSessionNotFoundError::new("claude", engine_thread_id))
                });
        }

        let workspace = self.load_workspace(context).await?;
        let session = remote_project_claude_runtime_service::runtime(&workspace)
            .await?
            .read_remote_session(engine_thread_id)
            .await
            .map_err(|error| {
                if error
                    .downcast_ref::<RemoteClaudeSessionNotFoundError>()
                    .is_some()
                {
                    anyhow::Error::new(CliSessionNotFoundError::new("claude", engine_thread_id))
                } else {
                    error
                }
            })?;
        anyhow::ensure!(
            session.id == engine_thread_id && session.session_id == engine_thread_id,
            "SSH 远端 Claude 返回的会话 ID 与请求不一致: requested={engine_thread_id} id={} sessionId={}",
            session.id,
            session.session_id
        );
        anyhow::ensure!(
            path_utils::is_path_within_root(&session.cwd, &workspace.root_path),
            "SSH 远端 Claude 会话不属于当前 workspace: session_id={engine_thread_id} cwd={} workspace_root={}",
            session.cwd,
            workspace.root_path
        );
        let remote_metadata =
            serde_json::to_value(&session).context("序列化 SSH 远端 Claude 会话元数据失败")?;

        Ok(CliSessionSnapshot {
            engine_thread_id: session.id.clone(),
            title: session.title.clone(),
            preview: None,
            cwd: session.cwd.clone(),
            model_id: "unknown".to_string(),
            reasoning_effort: None,
            created_at: None,
            updated_at: Some(session.updated_at.clone()),
            source_kind: Some("claude".to_string()),
            raw_status: Some("idle".to_string()),
            active_flags: Vec::new(),
            status: ThreadStatusDto::Idle,
            archived: false,
            metadata: json!({
                "sshRemote": true,
                "claudeRemoteCwd": session.cwd.clone(),
                "claudeRemote": remote_metadata,
            }),
        })
    }

    async fn get_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<PermissionComponentJson> {
        self.load_workspace(context).await?;
        anyhow::ensure!(thread.engine_id == "claude", "当前会话不属于 Claude Code");
        anyhow::ensure!(thread.workspace_id == context.workspace_id, "当前会话不属于该 workspace");
        permissions_from_thread(thread)
    }

    async fn set_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        values: PermissionComponentJson,
    ) -> Result<PermissionComponentJson> {
        self.load_workspace(context).await?;
        anyhow::ensure!(thread.engine_id == "claude", "当前会话不属于 Claude Code");
        anyhow::ensure!(thread.workspace_id == context.workspace_id, "当前会话不属于该 workspace");
        validate_permission_component(&values)?;
        let current = <Self as CliTool>::get_permissions(self, context, thread).await?;
        if current.get("autonomyPreset") == values.get("autonomyPreset")
            && current.get("approval") == values.get("approval")
            && current.get("sandbox") == values.get("sandbox")
            && current.get("network") == values.get("network")
        {
            let mut result = current;
            if let Some(value) = values.get("trust") { result.insert("trust".to_string(), value.clone()); }
            if let Some(value) = values.get("defaultForNewThreads") { result.insert("defaultForNewThreads".to_string(), value.clone()); }
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
        anyhow::ensure!(sandbox != Some("full-access"), "Claude Code 不支持 full-access sandbox");
        let (mode, sandbox_mode, allow_network) = match preset {
            Some("automatic") => (None, None, None),
            None if autonomy_is_empty || (approval.is_none() && sandbox.is_none() && network.is_none()) => (None, None, None),
            Some("read-only") => (Some("dontAsk"), Some("read-only"), Some(false)),
            Some("ask") => (Some("default"), Some("workspace-write"), Some(false)),
            Some("auto") => (Some("acceptEdits"), Some("workspace-write"), None),
            Some("full") => (Some("bypassPermissions"), Some("workspace-write"), Some(true)),
            _ => (
                /*
                // 旧权限 preset 映射已由 SDK 原生 permissionMode 接替：
                match approval { Some("restricted") => Some("restricted"), Some("ask") => Some("standard"), Some("autonomous") => Some("trusted"), _ => None },
                */
                match approval { Some("restricted") => Some("dontAsk"), Some("ask") => Some("default"), Some("autonomous") => Some("bypassPermissions"), _ => None },
                match sandbox { Some("read-only") => Some("read-only"), Some("workspace-write") => Some("workspace-write"), _ => None },
                match network { Some("enabled") => Some(true), Some("restricted") => Some(false), _ => None },
            ),
        };
        let raw_value = raw_permissions_value(thread, mode, sandbox_mode, allow_network);
        let raw_string = raw_value.to_string();
        let saved = db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw_string))?;
        let mut result = self.get_permissions(context, &saved).await?;
        for key in ["trust", "defaultForNewThreads"] {
            if let Some(value) = values.get(key) { result.insert(key.to_string(), value.clone()); }
        }
        Ok(result)
    }

    /// 将 Claude Code 的原始权限字段转换为统一运行时权限结构。
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
        let approval_policy = object
            .get("permissionMode")
            .or_else(|| object.get("approvalPolicy"))
            .or_else(|| object.get("claudePermissionMode"))
            .and_then(Value::as_str)
            .map(|mode| match mode {
                "restricted" => "dontAsk",
                "standard" => "default",
                "trusted" => {
                    if object.get("allowNetwork").and_then(Value::as_bool) == Some(true) {
                        "bypassPermissions"
                    } else {
                        "acceptEdits"
                    }
                }
                _ => mode,
            })
            .map(|mode| json!(mode));
        /*
        // 旧 runtime 权限直接透传逻辑已由上面的历史值归一化接替：
        let approval_policy = object
            .get("permissionMode")
            .or_else(|| object.get("approvalPolicy"))
            .or_else(|| object.get("claudePermissionMode"))
            .cloned();
        */
        Ok(CliRuntimePermissions {
            approval_policy,
            sandbox_mode: object
                .get("sandboxMode")
                .and_then(Value::as_str)
                .map(str::to_owned),
            allow_network: object
                .get("allowNetwork")
                .or_else(|| object.get("networkPolicy"))
                .or_else(|| object.get("sandboxAllowNetwork"))
                .and_then(|value| {
                    value.as_bool().or_else(|| match value.as_str() {
                        Some("enabled") => Some(true),
                        Some("restricted") => Some(false),
                        _ => None,
                    })
                }),
            permission_profile: object.get("permissionProfile").cloned(),
            approvals_reviewer: object
                .get("approvalsReviewer")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// 将统一权限补丁转换为 Claude Code 原始权限 JSON 并持久化到线程。
    async fn patch_runtime_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        patch: CliRuntimePermissionPatch,
    ) -> Result<ThreadDto> {
        self.load_workspace(context).await?;
        let mut raw = thread
            .permission_mode
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Some(value) = patch.approval_policy {
            let value = if let Some(value) = value {
                let normalized = value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_lowercase)
                    .ok_or_else(|| anyhow::anyhow!("Claude permission mode must be a string"))?;
                anyhow::ensure!(matches!(normalized.as_str(), "dontask" | "default" | "acceptedits" | "bypasspermissions" | "restricted" | "standard" | "trusted"), "invalid Claude permission mode `{normalized}`. expected one of: dontAsk, default, acceptEdits, bypassPermissions");
                let normalized = match normalized.as_str() {
                    "dontask" | "restricted" => "dontAsk",
                    "default" | "standard" => "default",
                    "acceptedits" => "acceptEdits",
                    "bypasspermissions" | "trusted" => "bypassPermissions",
                    _ => unreachable!(),
                };
                Some(json!(normalized))
            } else {
                None
            };
            set_or_remove(&mut raw, "permissionMode", value);
        }
        if let Some(value) = patch.sandbox_mode {
            let value = if let Some(value) = value {
                let normalized = match value.trim().to_lowercase().as_str() {
                    "read-only" | "read_only" | "readonly" => "read-only",
                    "workspace-write" | "workspace_write" | "workspacewrite" => "workspace-write",
                    _ => {
                        anyhow::bail!("Claude sandbox mode `{value}` is not supported")
                    }
                };
                Some(json!(normalized))
            } else {
                None
            };
            set_or_remove(&mut raw, "sandboxMode", value);
        }
        if let Some(value) = patch.allow_network {
            set_or_remove(&mut raw, "allowNetwork", value.map(|value| json!(value)));
        }
        if patch.permission_profile.is_some() {
            anyhow::bail!("Claude Code 不支持 permission profile");
        }
        if patch.approvals_reviewer.is_some() {
            anyhow::bail!("Claude Code 不支持 approvals reviewer");
        }
        let raw = Value::Object(raw).to_string();
        db::threads::update_thread_permissions(&self.state.db, &thread.id, Some(&raw))
    }

    async fn acquire_turn(&self, context: &CliExecutionContext, thread: &ThreadDto) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            if self.uses_reuse_session(context)
                && self.session_handles.prepare_turn(&thread.id).await
            {
                return Ok(());
            }
            let mut remote_turn_use = self.remote_turn_use.lock().await;
            anyhow::ensure!(
                remote_turn_use.is_none(),
                "当前 Claude Code 工具实例已经持有其他整轮使用权"
            );
            *remote_turn_use = Some(
                remote_project_claude_runtime_service::acquire_turn(&workspace, &thread.id).await?,
            );
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
            if self.uses_reuse_session(context) && self.session_handles.contains(&thread.id).await {
                let (engine, _) = self.session_handles.session_runtime(&thread.id).await?;
                return Engine::start_thread(
                    engine.as_ref(),
                    scope,
                    resume_engine_thread_id,
                    model,
                    sandbox,
                )
                .await;
            }
            let remote_turn_use = self.remote_turn_use.lock().await;
            let service_use = remote_turn_use
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("当前 SSH 远端 Claude 会话尚未建立持续使用关系"))?;
            return Engine::start_thread(
                service_use.engine().as_ref(),
                scope,
                resume_engine_thread_id,
                model,
                sandbox,
            )
            .await;
        }
        let engine = self.local_engine().await?;
        engine.set_computer_control_service(self.state.computer_control_service.clone());
        engine.set_auracoder_thread_mcp_service(self.state.auracoder_thread_mcp_service.clone());
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
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            if self.uses_reuse_session(context) {
                let session_exists = self.session_handles.contains(&thread.id).await;
                let (engine, remote_base_url, service_use) = if session_exists {
                    let (engine, remote_base_url) =
                        self.session_handles.session_runtime(&thread.id).await?;
                    (engine, remote_base_url, None)
                } else {
                    let service_use =
                        self.remote_turn_use.lock().await.take().ok_or_else(|| {
                            anyhow::anyhow!("当前 SSH 远端 Claude 会话尚未建立持续使用关系")
                        })?;
                    let engine = service_use.engine().clone();
                    let remote_base_url = reqwest::Url::parse(engine.base_url())
                        .context("解析 SSH 远端 Claude 服务地址失败")?;
                    (engine, remote_base_url, Some(service_use))
                };
                let persistent_turn = engine
                    .prepare_persistent_turn(engine_thread_id, input)
                    .await?;
                let handle_id = if let Some(service_use) = service_use {
                    self.session_handles
                        .create_or_get(
                            &thread.id,
                            remote_base_url,
                            Some(service_use),
                            persistent_turn.params.clone(),
                        )
                        .await?
                        .handle_id
                } else {
                    self.session_handles
                        .send_message(&thread.id, persistent_turn.params.clone())
                        .await?
                        .handle_id
                };

                let session_handles = self.session_handles.clone();
                let cancel_thread_id = thread.id.clone();
                let cancel_token = cancellation.clone();
                let cancel_task = tokio::spawn(async move {
                    cancel_token.cancelled().await;
                    if let Err(error) = session_handles.interrupt(&cancel_thread_id).await {
                        log::warn!(
                            "中断 SSH 远端 Claude 复用会话失败: thread_id={} error={error:#}",
                            cancel_thread_id
                        );
                    }
                });
                let result = engine
                    .relay_persistent_turn(engine_thread_id, &handle_id, persistent_turn, event_tx)
                    .await;
                cancel_task.abort();
                let idle_result = self.session_handles.mark_turn_completed(&thread.id).await;
                if let Err(error) = result {
                    let _ = idle_result;
                    return Err(error);
                }
                return idle_result;
            } else {
                let service_use = self.remote_turn_use.lock().await.take().ok_or_else(|| {
                    anyhow::anyhow!("当前 SSH 远端 Claude 会话尚未建立持续使用关系")
                })?;
                let result = Engine::send_message(
                    service_use.engine().as_ref(),
                    engine_thread_id,
                    input,
                    event_tx,
                    cancellation,
                )
                .await;
                service_use.release().await;
                return result;
            }
        }
        let engine = self.local_engine().await?;
        engine.set_computer_control_service(self.state.computer_control_service.clone());
        engine.set_auracoder_thread_mcp_service(self.state.auracoder_thread_mcp_service.clone());
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
            let engine = remote_project_claude_runtime_service::runtime(&workspace).await?;
            return Engine::steer_message(
                engine.as_ref(),
                engine_thread_id,
                client_steer_id,
                content,
                input,
            )
            .await;
        }
        Engine::steer_message(
            self.local_engine().await?.as_ref(),
            engine_thread_id,
            client_steer_id,
            content,
            input,
        )
        .await
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
            let engine = remote_project_claude_runtime_service::runtime(&workspace).await?;
            Engine::respond_to_approval(engine.as_ref(), approval_id, response, route)
                .await
                .with_context(|| format!("SSH 远端 Claude 审批回复失败: thread_id={}", thread.id))
        } else {
            Engine::respond_to_approval(
                self.local_engine().await?.as_ref(),
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
        _engine_thread_id: &str,
    ) -> Result<()> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            if self.uses_reuse_session(context) {
                if self.session_handles.contains(&thread.id).await {
                    self.session_handles.interrupt(&thread.id).await?;
                }
                Ok(())
            } else {
                let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
                    return Ok(());
                };
                let engine = remote_project_claude_runtime_service::runtime(&workspace).await?;
                Engine::interrupt(engine.as_ref(), engine_thread_id)
                    .await
                    .with_context(|| format!("SSH 远端 Claude 取消失败: thread_id={}", thread.id))
            }
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
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            // Claude Code 的 SSH 服务没有普通会话归档协议。这里仍然通过统一
            // CliTool 接口返回成功，由上层公共流程随后写入 AuraCoder 本地
            // `threads.archived_at`；不能在此处通过远端运行时发送归档请求。
            Ok(())
        } else {
            Engine::archive_thread(self.local_engine().await?.as_ref(), engine_thread_id).await
        }
    }

    async fn unarchive_thread(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            Ok(())
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
        _engine_thread_id: &str,
    ) -> Result<Option<String>> {
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            Ok(None)
        } else {
            Ok(None)
        }
    }

    async fn read_thread_sync_snapshot(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        _engine_thread_id: &str,
    ) -> Result<Option<ThreadSyncSnapshot>> {
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            Ok(None)
        } else {
            Ok(None)
        }
    }

    async fn set_thread_name(
        &self,
        context: &CliExecutionContext,
        _thread: &ThreadDto,
        _engine_thread_id: &str,
        _name: &str,
    ) -> Result<()> {
        self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            Ok(())
        } else {
            Ok(())
        }
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
        _cwd: &str,
    ) -> Result<OpenCodeRuntimeCatalogDto> {
        self.load_workspace(context).await?;
        Err(Self::unsupported("OpenCode 参数"))
    }

    async fn refresh_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
        requested_kinds: &[String],
    ) -> Result<Vec<ExtensionCatalogKindRefreshDto>> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            let catalog = self.remote_extension_catalog(&workspace).await?;
            return Ok(requested_kinds
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
                .collect());
        }
        let mut results = Vec::new();
        for kind in requested_kinds {
            results.push(extensions::claude::refresh_kind(&self.state.engines, cwd, kind).await);
        }
        Ok(results)
    }

    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto> {
        let workspace = self.load_workspace(context).await?;
        if context.location_kind == CliLocationKind::Ssh {
            return self.remote_extension_catalog(&workspace).await;
        }
        extensions::refresh::load_cached_catalog(
            &self.state,
            "claude",
            cwd.or(Some(workspace.root_path.as_str())),
        )
        .await
    }

    async fn get_extensions(&self, context: &CliExecutionContext) -> Result<Vec<ExtensionItemDto>> {
        let catalog = self.get_extension_catalog(context, None).await?;
        let mut items = catalog.items;
        for item in &mut items {
            match item.kind.as_str() {
                "skill" => {
                    item.insert_text = Some(format!("/{} ", item.name));
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
        let panel_ids = ["skills", "plugins", "mcp"];
        items.extend(panel_ids.into_iter().map(|id| ExtensionItemDto {
            id: id.to_string(),
            provider_id: "claude".to_string(),
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
            "SSH 远端 Claude Code 当前不执行扩展变更，也不会调用本机 Claude Code"
        );
        extensions::claude::perform_action(&item, action, scope, Some(workspace.root_path.as_str()))
            .await
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
        Err(Self::unsupported("Codex 代码审查"))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use super::*;
    use crate::{
        config::app_config::AppConfig,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        models::SshConnectionInput,
        power::KeepAwakeManager,
        state::{AppState, TurnManager},
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    use uuid::Uuid;

    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-claude-archive-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create test root");
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
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    #[tokio::test]
    async fn ssh_archive_hook_returns_without_remote_archive_service() {
        let state = test_app_state();
        let connection_id = format!("ssh-test-{}", Uuid::new_v4());
        let input = SshConnectionInput {
            display_name: "Claude archive test SSH".to_string(),
            host_name: "192.0.2.10".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            host_key: String::new(),
            config_alias: None,
        };
        let connection = crate::db::ssh_connections::insert(
            &state.db,
            &connection_id,
            "manual",
            &input,
            "ssh-ed25519",
            "test-key",
        )
        .expect("failed to create test SSH connection");
        crate::db::ssh_connections::set_status_if_current(
            &state.db,
            &connection.id,
            &connection.updated_at,
            crate::db::ssh_connections::STATUS_OK,
            None,
        )
        .expect("failed to mark test SSH connection ready");
        let workspace = crate::db::workspaces::create_ssh_workspace(
            &state.db,
            &connection_id,
            "Claude archive test workspace",
            &format!("/tmp/auracoder-claude-archive-{}", Uuid::new_v4()),
            None,
        )
        .expect("failed to create test SSH workspace");
        let thread = crate::db::threads::create_thread(
            &state.db,
            &workspace.id,
            None,
            "claude",
            "claude-sonnet-4-6",
            "Claude archive test thread",
        )
        .expect("failed to create test Claude thread");
        let context = CliExecutionContext::from_workspace(&workspace)
            .expect("failed to build Claude SSH execution context");

        // 故意不登记 Claude 远端服务；若实现访问了远端归档协议，统一接口调用应失败。
        let cli = ClaudeCodeCli::new(state);
        let cli: &dyn CliTool = &cli;
        cli.archive_thread(&context, &thread, "remote-claude-thread")
            .await
            .expect("SSH Claude archive hook should not require remote service");
    }

    fn permission_thread(permission_mode: Option<&str>, metadata: Option<Value>) -> ThreadDto {
        ThreadDto {
            id: "thread".to_string(),
            workspace_id: "workspace".to_string(),
            repo_id: None,
            engine_id: "claude".to_string(),
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
        let values = permissions_from_thread(&permission_thread(None, None)).unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["automatic"])));
        assert_eq!(values.get("approval"), Some(&json!(["automatic"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
    }

    #[test]
    fn permissions_read_legacy_fields() {
        let values = permissions_from_thread(&permission_thread(
            Some(r#"{"approvalPolicy":"restricted","sandboxMode":"read-only","networkPolicy":"restricted"}"#),
            None,
        ))
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["read-only"])));
        assert_eq!(values.get("approval"), Some(&json!(["restricted"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["read-only"])));
        assert_eq!(values.get("network"), Some(&json!(["restricted"])));
    }

    #[test]
    fn permissions_read_all_legacy_inherit_values_as_automatic() {
        let values = permissions_from_thread(&permission_thread(
            Some(r#"{"approvalPolicy":"inherit","sandboxMode":"inherit","networkPolicy":"inherit"}"#),
            None,
        ))
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["automatic"])));
        assert_eq!(values.get("approval"), Some(&json!(["automatic"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
    }

    #[test]
    fn permissions_read_current_fields() {
        let values = permissions_from_thread(&permission_thread(
            Some(r#"{"permissionMode":"standard","sandboxMode":"workspace-write","allowNetwork":false}"#),
            None,
        ))
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["ask"])));
        assert_eq!(values.get("approval"), Some(&json!(["ask"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["workspace-write"])));
        assert_eq!(values.get("network"), Some(&json!(["restricted"])));
    }

    #[test]
    fn permissions_read_native_permission_modes() {
        let expected = [
            ("dontAsk", "read-only", "restricted", "read-only", "restricted"),
            ("default", "ask", "ask", "workspace-write", "restricted"),
            ("acceptEdits", "auto", "autonomous", "workspace-write", "automatic"),
            ("bypassPermissions", "full", "autonomous", "workspace-write", "enabled"),
        ];
        for (mode, preset, approval, sandbox, network) in expected {
            let network_field = if mode == "acceptEdits" { "" } else { ",\"allowNetwork\":false" };
            let network_field = if mode == "dontAsk" { ",\"allowNetwork\":false" } else { network_field };
            let network_field = if mode == "bypassPermissions" { ",\"allowNetwork\":true" } else { network_field };
            let values = permissions_from_thread(&permission_thread(
                Some(&format!(r#"{{"permissionMode":"{mode}","sandboxMode":"{sandbox}"{network_field}}}"#)),
                None,
            ))
            .unwrap();
            assert_eq!(values.get("autonomyPreset"), Some(&json!([preset])));
            assert_eq!(values.get("approval"), Some(&json!([approval])));
            assert_eq!(values.get("sandbox"), Some(&json!([sandbox])));
            assert_eq!(values.get("network"), Some(&json!([network])));
        }
    }

    #[test]
    fn permissions_save_native_permission_modes() {
        let cases = [
            ("dontAsk", "read-only", false),
            ("default", "workspace-write", false),
            ("acceptEdits", "workspace-write", false),
            ("bypassPermissions", "workspace-write", true),
        ];
        for (mode, sandbox, network) in cases {
            let raw = raw_permissions_value(&permission_thread(None, None), Some(mode), Some(sandbox), Some(network));
            assert_eq!(raw["permissionMode"], json!(mode));
            assert_eq!(raw["sandboxMode"], json!(sandbox));
            assert_eq!(raw["allowNetwork"], json!(network));
        }
    }

    #[test]
    fn permissions_save_preserves_unknown_fields_and_clears_legacy_fields() {
        let thread = permission_thread(
            Some(
                r#"{"approvalPolicy":"standard","networkPolicy":"enabled","allow":["Read"],"ask":["Write"],"deny":[],"unknown":{"keep":true}}"#,
            ),
            None,
        );
        let raw = raw_permissions_value(&thread, Some("bypassPermissions"), Some("workspace-write"), Some(true));
        assert_eq!(raw["permissionMode"], json!("bypassPermissions"));
        /*
        // 旧 trusted 保存值已由 SDK 原生 bypassPermissions 接替：
        assert_eq!(raw["permissionMode"], json!("trusted"));
        */
        assert_eq!(raw["sandboxMode"], json!("workspace-write"));
        assert_eq!(raw["allowNetwork"], json!(true));
        assert_eq!(raw["allow"], json!(["Read"]));
        assert_eq!(raw["ask"], json!(["Write"]));
        assert_eq!(raw["unknown"], json!({"keep": true}));
        assert!(raw.get("approvalPolicy").is_none());
        assert!(raw.get("networkPolicy").is_none());
    }

    #[test]
    fn permissions_ignore_metadata_when_raw_is_empty() {
        let values = permissions_from_thread(&permission_thread(
            Some("{}"),
            Some(json!({
                "claudePermissionMode": "trusted",
                "sandboxMode": "workspace-write",
                "sandboxAllowNetwork": true,
            })),
        ))
        .unwrap();
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["automatic"])));
        assert_eq!(values.get("approval"), Some(&json!(["automatic"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["automatic"])));
        assert_eq!(values.get("network"), Some(&json!(["automatic"])));
        /*
        // 旧 metadata 回退已停用：
        assert_eq!(values.get("autonomyPreset"), Some(&json!(["full"])));
        assert_eq!(values.get("approval"), Some(&json!(["autonomous"])));
        assert_eq!(values.get("sandbox"), Some(&json!(["workspace-write"])));
        assert_eq!(values.get("network"), Some(&json!(["enabled"])));
        */
    }

    #[test]
    fn permissions_reject_invalid_non_empty_json() {
        let error = permissions_from_thread(&permission_thread(Some("[1]"), None)).unwrap_err();
        assert!(error.to_string().contains("必须是对象"));
    }
}
