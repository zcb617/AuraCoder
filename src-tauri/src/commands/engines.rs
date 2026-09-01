use std::{path::Path, time::Instant};

use anyhow::Context;
use tauri::State;
use tokio::process::Command;

// 旧实现曾直接读取本机 CLI 生命周期；现由 CliTool::is_service_ready 统一查询：
// use crate::local_cli_service_lifecycle::LocalCliServiceLifecycle;

use crate::runtime_env;
use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliLocationKind, CliTool},
    engines::{capabilities_for_engine, map_engine_capabilities},
    models::{
        ChatProviderUsageDto, CliContextUsageDto, CodexAppDto, CodexPluginDto, CodexSkillDto,
        EngineCheckResultDto, EngineHealthDto, EngineInfoDto, ExecutionTargetDto,
        OpenCodeRuntimeCatalogDto,
        // ThreadDto,
    },
    process_utils,
    state::AppState,
};

/// 本机 CLI 目录通过统一接口查询 Ready 服务，再读取对应 CLI 的引擎信息。
/// 本机取数不依赖具体项目，因此使用仅标记 location_kind 的本机上下文。
pub(crate) async fn list_local_engine_infos(
    state: &AppState,
) -> Result<Vec<EngineInfoDto>, String> {
    let factory = CliToolFactory::new(state.clone());
    let context = CliExecutionContext {
        workspace_id: String::new(),
        root_path: String::new(),
        location_kind: CliLocationKind::Local,
        ssh_connection_id: None,
    };
    let mut engines = Vec::new();
    for cli_id in ["codex", "opencode", "claude"] {
        let cli = factory.create(cli_id).map_err(err_to_string)?;
        let ready = match cli.is_service_ready(&context).await {
            Ok(ready) => ready,
            Err(error) => {
                log::warn!(
                    "查询本机 CLI 服务状态失败，跳过引擎: cli_id={} error={error:#}",
                    cli_id,
                );
                continue;
            }
        };
        if !ready {
            continue;
        }
        match cli.get_engine_info(&context).await {
            Ok(engine) => engines.push(engine),
            Err(error) => {
                log::warn!(
                    "读取本地引擎目录失败，保留其他引擎: cli_id={} error={error:#}",
                    cli_id,
                );
                engines.push(EngineInfoDto {
                    id: cli_id.to_string(),
                    name: cli.name().to_string(),
                    models: Vec::new(),
                    capabilities: map_engine_capabilities(capabilities_for_engine(cli_id)),
                });
            }
        }
    }
    engines.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(engines)
}

#[tauri::command]
pub async fn get_execution_target(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<ExecutionTargetDto, String> {
    let Some(workspace_id) = workspace_id else {
        return Ok(ExecutionTargetDto {
            target_key: "local".to_string(),
            kind: "local".to_string(),
            display_name: "本机".to_string(),
            connection_id: None,
            host_name: None,
            user: None,
            port: None,
            project_path: None,
            connection_status: Some("ok".to_string()),
        });
    };

    let db = state.db.clone();
    let lookup_workspace_id = workspace_id.clone();
    let workspace = tokio::task::spawn_blocking(move || {
        crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?
    .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;

    if workspace.location_kind != "ssh" {
        return Ok(ExecutionTargetDto {
            target_key: "local".to_string(),
            kind: "local".to_string(),
            display_name: "本机".to_string(),
            connection_id: None,
            host_name: None,
            user: None,
            port: None,
            project_path: Some(workspace.root_path),
            connection_status: Some("ok".to_string()),
        });
    }

    let connection_id = workspace
        .ssh_connection_id
        .as_deref()
        .ok_or_else(|| "远端项目未绑定 SSH 连接".to_string())?
        .to_string();
    let db = state.db.clone();
    let lookup_connection_id = connection_id.clone();
    let connection = tokio::task::spawn_blocking(move || {
        crate::db::ssh_connections::find(&db, &lookup_connection_id)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?
    .ok_or_else(|| format!("SSH connection not found: {connection_id}"))?;

    Ok(ExecutionTargetDto {
        target_key: format!("ssh:{connection_id}"),
        kind: "ssh".to_string(),
        display_name: connection.dto.display_name,
        connection_id: Some(connection_id),
        host_name: Some(connection.dto.host_name),
        user: Some(connection.dto.user),
        port: Some(connection.dto.port),
        project_path: Some(workspace.root_path),
        connection_status: Some(connection.dto.connection_status),
    })
}

#[tauri::command]
pub async fn list_actived_clis(
    state: State<'_, AppState>,
    connection_id: Option<String>,
) -> Result<Vec<EngineInfoDto>, String> {
    if let Some(connection_id) = connection_id {
        let factory = CliToolFactory::new(state.inner().clone());
        let context = CliExecutionContext {
            workspace_id: String::new(),
            root_path: String::new(),
            location_kind: CliLocationKind::Ssh,
            ssh_connection_id: Some(connection_id.clone()),
        };
        let mut engines = Vec::new();
        for cli_id in ["codex", "opencode", "claude"] {
            let cli = factory.create(cli_id).map_err(err_to_string)?;
            let ready = match cli.is_service_ready(&context).await {
                Ok(ready) => ready,
                Err(error) => {
                    log::warn!(
                        "查询 SSH 远端 CLI 服务状态失败，跳过引擎: connection_id={} cli_id={} error={error:#}",
                        connection_id,
                        cli_id,
                    );
                    continue;
                }
            };
            if !ready {
                continue;
            }
            match cli.get_engine_info(&context).await {
                Ok(engine) => engines.push(engine),
                Err(error) => {
                    log::warn!(
                        "读取 SSH 远端引擎目录失败，保留其他引擎: connection_id={} cli_id={} error={error:#}",
                        connection_id,
                        cli_id,
                    );
                    engines.push(EngineInfoDto {
                        id: cli_id.to_string(),
                        name: cli.name().to_string(),
                        models: Vec::new(),
                        capabilities: map_engine_capabilities(capabilities_for_engine(cli_id)),
                    });
                }
            }
        }
        if engines.is_empty() {
            return Err("SSH 远端机器没有已激活的 Codex、OpenCode 或 Claude CLI 工具".to_string());
        }
        engines.sort_by(|left, right| left.id.cmp(&right.id));
        return Ok(engines);
    }

    /*
    旧实现接收 workspaceId，先查询项目，再从远端项目取得 connectionId。CLI 列表属于
    本地电脑或指定的远端电脑，不属于项目目录，因此不再执行这段项目查询和分流逻辑：
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            if workspace.connection_deleted == Some(true) {
                return Err("SSH 连接已删除，请先恢复连接".to_string());
            }
            if workspace.connection_enabled == Some(false) {
                return Err("SSH 连接已禁用".to_string());
            }
            let connection_id = workspace
                .ssh_connection_id
                .as_deref()
                .ok_or_else(|| "远端项目未绑定 SSH 连接".to_string())?
                .to_string();
            /*
            旧实现用 Tunnel Registry 判断 CLI 工具是否可用，并等待隧道数量稳定。隧道存在
            不代表统一 CLI 生命周期中的服务句柄已经 Ready，因此不再执行：
            let mut observed_tunnel_count = 0;
            let mut stable_checks = 0;
            for _ in 0..150 {
                let tunnel_count = crate::ssh::cli_tunnel_registry::list_by_host(&connection_id)
                    .await
                    .len();
                if tunnel_count > observed_tunnel_count {
                    observed_tunnel_count = tunnel_count;
                    stable_checks = 0;
                } else if tunnel_count > 0 {
                    stable_checks += 1;
                    if stable_checks >= 5 {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let tunnels = crate::ssh::cli_tunnel_registry::list_by_host(&connection_id).await;
            if tunnels.is_empty() {
                return Err(
                    "SSH 远端机器没有可用的 Codex、OpenCode 或 Claude 对话运行时"
                        .to_string(),
                );
            }

            let mut engines = Vec::new();
            for (cli_id, engine_name) in [
                ("codex", "Codex"),
                ("opencode", "OpenCode"),
                ("claude", "Claude"),
            ] {
                if !tunnels.contains_key(cli_id) {
                    continue;
                }
                let discovered = match cli_id {
                    "codex" => {
                        let codex = CliToolFactory::new(state.inner().clone())
                            .create("codex")
                            .expect("Codex CLI factory mapping must exist");
                        let context = CliExecutionContext::from_workspace(&workspace)
                            .map_err(err_to_string)?;
                        let cli: &dyn CliTool = codex.as_ref();
                        cli.get_engine_info(&context).await
                    }
                    "opencode" => {
                        let opencode = CliToolFactory::new(state.inner().clone())
                            .create("opencode")
                            .expect("OpenCode CLI factory mapping must exist");
                        let context = CliExecutionContext::from_workspace(&workspace)
                            .map_err(err_to_string)?;
                        let cli: &dyn CliTool = opencode.as_ref();
                        cli.get_engine_info(&context).await
                    }
                    "claude" => {
                        let claude = CliToolFactory::new(state.inner().clone())
                            .create("claude")
                            .expect("Claude CLI factory mapping must exist");
                        let context = CliExecutionContext::from_workspace(&workspace)
                            .map_err(err_to_string)?;
                        let cli: &dyn CliTool = claude.as_ref();
                        cli.get_engine_info(&context).await
                    }
                    _ => unreachable!(),
                };
                let _ = (engine_name, discovered);
            }
            */
            let services = crate::ssh::cli_service_lifecycle::list_ready(&connection_id).await;
            if services.is_empty() {
                return Err(
                    "SSH 远端机器没有已激活的 Codex、OpenCode 或 Claude CLI 工具".to_string(),
                );
            }

            let mut engines = Vec::new();
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let factory = CliToolFactory::new(state.inner().clone());
            for service in services {
                let cli_id = service.cli_id();
                let cli = factory.create(cli_id).map_err(err_to_string)?;
                let discovered = cli.get_engine_info(&context).await;
                match discovered {
                    Ok(engine) => engines.push(engine),
                    Err(error) => {
                        log::warn!(
                            "读取 SSH 远端引擎目录失败，保留其他引擎: connection_id={} cli_id={} error={error:#}",
                            connection_id,
                            cli_id,
                        );
                        engines.push(EngineInfoDto {
                            id: cli_id.to_string(),
                            name: cli.name().to_string(),
                            models: Vec::new(),
                            capabilities: map_engine_capabilities(capabilities_for_engine(cli_id)),
                        });
                    }
                }
            }
            return Ok(engines);
        }
    }
    */
    /*
    旧实现先由 EngineManager 自有的未预热引擎实例取模型目录，再拿
    LocalCliServiceLifecycle::list_ready() 过滤；本机首次读取会冷启动新的
    codex app-server，导致页面转圈。现在直接由本地 CLI 生命周期取数，不再执行：
    let local_services = LocalCliServiceLifecycle::list_ready().await;
    let engines = state
        .engines
        .list_actived_clis()
        .await
        .map_err(err_to_string)?;
    Ok(engines
        .into_iter()
        .filter(|engine| {
            local_services
                .iter()
                .any(|service| service.cli_id() == engine.id.as_str())
        })
        .collect())
    */
    list_local_engine_infos(state.inner()).await
}

#[tauri::command]
pub async fn get_engine_info(
    state: State<'_, AppState>,
    engine_id: String,
    workspace_id: Option<String>,
) -> Result<EngineInfoDto, String> {
    if engine_id == "codex" {
        let codex = CliToolFactory::new(state.inner().clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let context = codex
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = codex.as_ref();
        return cli.get_engine_info(&context).await.map_err(err_to_string);
    }
    if engine_id == "opencode" {
        let opencode = CliToolFactory::new(state.inner().clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let context = opencode
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = opencode.as_ref();
        return cli.get_engine_info(&context).await.map_err(err_to_string);
    }
    if engine_id == "claude" {
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let context = claude
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = claude.as_ref();
        return cli.get_engine_info(&context).await.map_err(err_to_string);
    }
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            return match engine_id.as_str() {
                "codex" => {
                    let codex = CliToolFactory::new(state.inner().clone())
                        .create("codex")
                        .expect("Codex CLI factory mapping must exist");
                    let context =
                        CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                    let cli: &dyn CliTool = codex.as_ref();
                    cli.get_engine_info(&context).await
                }
                "opencode" => unreachable!("OpenCode engine info already uses OpenCodeCli"),
                "claude" => {
                    let claude = CliToolFactory::new(state.inner().clone())
                        .create("claude")
                        .expect("Claude CLI factory mapping must exist");
                    let context =
                        CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                    let cli: &dyn CliTool = claude.as_ref();
                    cli.get_engine_info(&context).await
                }
                _ => Err(anyhow::anyhow!(
                    "SSH 远端项目当前阶段尚未接入 {} 正式对话",
                    engine_id
                )),
            }
            .map_err(err_to_string);
        }
    }

    /*
    旧实现回落到 EngineManager 的 list_actived_clis；该方法使用的引擎实例未在启动
    阶段预热，已整体停用。codex、opencode、claude 都在上方分支返回，走到这里的是
    不支持的引擎 id，不再执行：
    state
        .engines
        .list_actived_clis()
        .await
        .map_err(err_to_string)?
        .into_iter()
        .find(|engine| engine.id == engine_id)
        .ok_or_else(|| format!("engine not found: {engine_id}"))
    */
    Err(format!("engine not found: {engine_id}"))
}

#[tauri::command]
pub async fn get_chat_provider_usage(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    engine_id: Option<String>,
) -> Result<Vec<ChatProviderUsageDto>, String> {
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            let requested_engine = engine_id.as_deref();
            let mut usage = Vec::new();
            if requested_engine.is_none() || requested_engine == Some("codex") {
                let codex = CliToolFactory::new(state.inner().clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = codex.as_ref();
                if let Some(codex_usage) = cli
                    .get_chat_provider_usage(&context)
                    .await
                    .map_err(err_to_string)?
                {
                    usage.push(codex_usage);
                }
            }
            if requested_engine.is_none() || requested_engine == Some("claude") {
                let claude = CliToolFactory::new(state.inner().clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = claude.as_ref();
                if let Some(claude_usage) = cli
                    .get_chat_provider_usage(&context)
                    .await
                    .map_err(err_to_string)?
                {
                    usage.push(claude_usage);
                }
            }
            if requested_engine == Some("opencode") {
                let opencode = CliToolFactory::new(state.inner().clone())
                    .create("opencode")
                    .expect("OpenCode CLI factory mapping must exist");
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = opencode.as_ref();
                if let Some(opencode_usage) = cli
                    .get_chat_provider_usage(&context)
                    .await
                    .map_err(err_to_string)?
                {
                    usage.push(opencode_usage);
                } else {
                    usage.push(ChatProviderUsageDto {
                        engine_id: "opencode".to_string(),
                        name: "OpenCode".to_string(),
                        available: false,
                        windows: Vec::new(),
                    });
                }
            }
            return Ok(usage);
        }
    }

    let requested_engine = engine_id.as_deref();
    let mut usage = Vec::new();
    if requested_engine.is_none() || requested_engine == Some("codex") {
        let codex = CliToolFactory::new(state.inner().clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let context = codex.execution_context(None).await.map_err(err_to_string)?;
        let cli: &dyn CliTool = codex.as_ref();
        if let Some(codex_usage) = cli
            .get_chat_provider_usage(&context)
            .await
            .map_err(err_to_string)?
        {
            usage.push(codex_usage);
        }
    }
    if requested_engine.is_none() || requested_engine == Some("claude") {
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let context = claude
            .execution_context(None)
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = claude.as_ref();
        if let Some(claude_usage) = cli
            .get_chat_provider_usage(&context)
            .await
            .map_err(err_to_string)?
        {
            usage.push(claude_usage);
        }
    }
    if requested_engine == Some("opencode") {
        let opencode = CliToolFactory::new(state.inner().clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let context = opencode
            .execution_context(None)
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = opencode.as_ref();
        if let Some(opencode_usage) = cli
            .get_chat_provider_usage(&context)
            .await
            .map_err(err_to_string)?
        {
            usage.push(opencode_usage);
        } else {
            usage.push(ChatProviderUsageDto {
                engine_id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                available: false,
                windows: Vec::new(),
            });
        }
    }
    Ok(usage)
}

/// 查询指定 AuraCoder 线程所属 CLI 的真实上下文窗口快照。
///
/// 业务调用只按线程保存的 engine_id 选择统一 CLI 接口，并通过 workspace
/// 上下文校验执行位置；具体协议和本机/SSH 生命周期由 CLI 实现负责。
#[tauri::command]
pub async fn get_cli_context_usage(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Option<CliContextUsageDto>, String> {
    let raw_thread_id = thread_id.clone();
    let thread_id = thread_id.trim().to_string();
    log::info!(
        "CLI context usage controller request started: operation=get_cli_context_usage stage=validate_thread_id raw_thread_id={raw_thread_id:?} thread_id={thread_id:?}"
    );
    if thread_id.is_empty() {
        log::error!(
            "CLI context usage controller request rejected: operation=get_cli_context_usage stage=validate_thread_id raw_thread_id={raw_thread_id:?} thread_id={thread_id:?} error=thread id must not be empty"
        );
        return Err("thread id must not be empty".to_string());
    }

    let db = state.db.clone();
    let lookup_thread_id = thread_id.clone();
    let thread = tokio::task::spawn_blocking(move || {
        crate::db::threads::get_thread(&db, &lookup_thread_id)
    })
    .await
    .map_err(|error| {
        log::error!(
            "CLI context usage controller thread query join failed: operation=get_cli_context_usage stage=thread_query_join thread_id={} error={error:?}",
            thread_id
        );
        err_to_string(error)
    })?
    .map_err(|error| {
        log::error!(
            "CLI context usage controller thread query failed: operation=get_cli_context_usage stage=thread_query thread_id={} error={error:?}",
            thread_id
        );
        err_to_string(error)
    })?
    .ok_or_else(|| {
        log::error!(
            "CLI context usage controller thread lookup returned no row: operation=get_cli_context_usage stage=thread_query_not_found thread_id={}",
            thread_id
        );
        format!("thread not found: {thread_id}")
    })?;

    let db = state.db.clone();
    let workspace_id = thread.workspace_id.clone();
    let lookup_workspace_id = workspace_id.clone();
    let workspace = tokio::task::spawn_blocking(move || {
        crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
    })
    .await
    .map_err(|error| {
        log::error!(
            "CLI context usage controller workspace query join failed: operation=get_cli_context_usage stage=workspace_query_join thread_id={} workspace_id={} error={error:?}",
            thread.id,
            workspace_id
        );
        err_to_string(error)
    })?
    .map_err(|error| {
        log::error!(
            "CLI context usage controller workspace query failed: operation=get_cli_context_usage stage=workspace_query thread_id={} workspace_id={} error={error:?}",
            thread.id,
            workspace_id
        );
        err_to_string(error)
    })?
    .ok_or_else(|| {
        log::error!(
            "CLI context usage controller workspace lookup returned no row: operation=get_cli_context_usage stage=workspace_query_not_found thread_id={} workspace_id={}",
            thread.id,
            workspace_id
        );
        format!("workspace not found: {workspace_id}")
    })?;

    log::info!(
        "CLI context usage controller records resolved: operation=get_cli_context_usage stage=thread_workspace_resolved thread_id={} engine_id={} workspace_id={} engine_thread_id={:?} location_kind={}",
        thread.id,
        thread.engine_id,
        workspace.id,
        thread.engine_thread_id,
        workspace.location_kind
    );

    log::info!(
        "CLI context usage controller factory create start: operation=get_cli_context_usage stage=factory_create thread_id={} engine_id={}",
        thread.id,
        thread.engine_id
    );
    let cli = match CliToolFactory::new(state.inner().clone()).create(&thread.engine_id) {
        Ok(cli) => {
            log::info!(
                "CLI context usage controller factory create success: operation=get_cli_context_usage stage=factory_create_success thread_id={} engine_id={}",
                thread.id,
                thread.engine_id
            );
            cli
        }
        Err(error) => {
            log::error!(
                "CLI context usage controller factory create failed: operation=get_cli_context_usage stage=factory_create thread_id={} engine_id={} error={error:?}",
                thread.id,
                thread.engine_id
            );
            return Err(err_to_string(error));
        }
    };

    log::info!(
        "CLI context usage controller execution context start: operation=get_cli_context_usage stage=execution_context thread_id={} engine_id={} workspace_id={} location_kind={}",
        thread.id,
        thread.engine_id,
        workspace.id,
        workspace.location_kind
    );
    let context = match CliExecutionContext::from_workspace(&workspace) {
        Ok(context) => {
            log::info!(
                "CLI context usage controller execution context success: operation=get_cli_context_usage stage=execution_context_success thread_id={} engine_id={} workspace_id={} location_kind={:?} root_path={:?}",
                thread.id,
                thread.engine_id,
                context.workspace_id,
                context.location_kind,
                context.root_path
            );
            context
        }
        Err(error) => {
            log::error!(
                "CLI context usage controller execution context failed: operation=get_cli_context_usage stage=execution_context thread_id={} engine_id={} workspace_id={} error={error:?}",
                thread.id,
                thread.engine_id,
                workspace.id
            );
            return Err(err_to_string(error));
        }
    };

    log::info!(
        "CLI context usage controller CLI interface call start: operation=get_cli_context_usage stage=cli_interface_call thread_id={} engine_id={} workspace_id={} engine_thread_id={:?} location_kind={:?}",
        thread.id,
        thread.engine_id,
        context.workspace_id,
        thread.engine_thread_id,
        context.location_kind
    );
    match cli.get_context_usage(&context, &thread).await {
        Ok(usage) => {
            log::info!(
                "CLI context usage controller CLI interface call success: operation=get_cli_context_usage stage=cli_interface_result thread_id={} engine_id={} workspace_id={} engine_thread_id={:?} location_kind={:?} result={usage:?} is_null={}",
                thread.id,
                thread.engine_id,
                context.workspace_id,
                thread.engine_thread_id,
                context.location_kind,
                usage.is_none()
            );
            Ok(usage)
        }
        Err(error) => {
            log::error!(
                "CLI context usage controller CLI interface call failed: operation=get_cli_context_usage stage=cli_interface_call thread_id={} engine_id={} workspace_id={} engine_thread_id={:?} location_kind={:?} error={error:?}",
                thread.id,
                thread.engine_id,
                context.workspace_id,
                thread.engine_thread_id,
                context.location_kind
            );
            Err(err_to_string(error))
        }
    }
}

#[tauri::command]
pub async fn codex_uses_external_sandbox(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<bool, String> {
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(workspace_id.as_deref())
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    cli.uses_external_sandbox(&context)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn engine_health(
    state: State<'_, AppState>,
    engine_id: String,
    workspace_id: Option<String>,
) -> Result<EngineHealthDto, String> {
    if engine_id == "opencode" {
        let opencode = CliToolFactory::new(state.inner().clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let context = opencode
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = opencode.as_ref();
        return cli.engine_health(&context).await.map_err(err_to_string);
    }
    if let Some(workspace_id) = workspace_id.as_deref() {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.to_string();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            if engine_id == "codex" {
                let codex = CliToolFactory::new(state.inner().clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = codex.as_ref();
                return cli.engine_health(&context).await.map_err(err_to_string);
            }
            if engine_id == "claude" {
                let claude = CliToolFactory::new(state.inner().clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let cli: &dyn CliTool = claude.as_ref();
                return cli.engine_health(&context).await.map_err(err_to_string);
            }
            let connection_name = workspace
                .connection_display_name
                .clone()
                .unwrap_or_else(|| "未命名 SSH 连接".to_string());
            let connection_id = workspace
                .ssh_connection_id
                .as_deref()
                .ok_or_else(|| "远端项目未绑定 SSH 连接".to_string())?
                .to_string();
            let db = state.db.clone();
            let lookup_connection_id = connection_id.clone();
            let connection = tokio::task::spawn_blocking(move || {
                crate::db::ssh_connections::find(&db, &lookup_connection_id)
            })
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?
            .ok_or_else(|| format!("SSH connection not found: {connection_id}"))?;
            let engine_name = match engine_id.as_str() {
                "codex" => "Codex",
                "opencode" => "OpenCode",
                "claude" => "Claude",
                _ => {
                    return Err(format!("SSH 远端项目当前阶段尚未接入 {engine_id} 正式对话"));
                }
            };
            let mut protocol_diagnostics = None;
            let result = match engine_id.as_str() {
                "codex" => {
                    let service_use =
                        crate::remote_project_codex_runtime_service::acquire_temporary(&workspace)
                            .await;
                    match service_use {
                        Ok(service_use) => {
                            let result = service_use.engine().list_models_runtime().await;
                            protocol_diagnostics =
                                service_use.engine().protocol_diagnostics_snapshot().await;
                            service_use.release().await;
                            if result.is_empty() {
                                Err(anyhow::anyhow!("远端 Codex 模型目录为空"))
                            } else {
                                Ok(())
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                "opencode" => unreachable!("OpenCode health already uses OpenCodeCli"),
                "claude" => unreachable!("Claude health already uses ClaudeCodeCli"),
                _ => unreachable!(),
            };
            let version = if result.is_ok() {
                let version_command = crate::ssh::runtime::wrap_remote_login_shell_command(
                    match engine_id.as_str() {
                        "codex" => "codex --version",
                        "opencode" => "opencode --version",
                        "claude" => "claude --version",
                        _ => unreachable!(),
                    },
                );
                crate::ssh::gateway::run_command(&connection, &version_command)
                    .await
                    .ok()
                    .and_then(|output| String::from_utf8(output.into()).ok())
                    .map(|output| output.trim().to_string())
                    .filter(|output| !output.is_empty())
            } else {
                None
            };
            return Ok(EngineHealthDto {
                id: engine_id.clone(),
                available: result.is_ok(),
                version,
                details: Some(match result {
                    Ok(()) => format!("SSH 远端 {engine_name}：{connection_name}"),
                    Err(error) => format!("SSH 远端 {engine_name} 不可用：{error:#}"),
                }),
                warnings: Vec::new(),
                checks: Vec::new(),
                fixes: Vec::new(),
                protocol_diagnostics,
            });
        }
    }
    if engine_id == "codex" {
        let codex = CliToolFactory::new(state.inner().clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let context = codex
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = codex.as_ref();
        return cli.engine_health(&context).await.map_err(err_to_string);
    }
    if engine_id == "claude" {
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let context = claude
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = claude.as_ref();
        return cli.engine_health(&context).await.map_err(err_to_string);
    }
    state
        .engines
        .health(&engine_id)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn prewarm_engine(
    state: State<'_, AppState>,
    engine_id: String,
    workspace_id: Option<String>,
) -> Result<(), String> {
    if engine_id == "opencode" {
        let opencode = CliToolFactory::new(state.inner().clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let context = opencode
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = opencode.as_ref();
        return cli.prewarm_engine(&context).await.map_err(err_to_string);
    }
    if engine_id == "codex" {
        let codex = CliToolFactory::new(state.inner().clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let context = codex
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = codex.as_ref();
        return cli.prewarm_engine(&context).await.map_err(err_to_string);
    }
    if engine_id == "claude" {
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let context = claude
            .execution_context(workspace_id.as_deref())
            .await
            .map_err(err_to_string)?;
        let cli: &dyn CliTool = claude.as_ref();
        return cli.prewarm_engine(&context).await.map_err(err_to_string);
    }
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            return match engine_id.as_str() {
                "codex" => {
                    let connection_id = crate::remote_project_codex_runtime_service::validate_remote_codex_workspace(&workspace)
                        .map_err(err_to_string)?;
                    crate::remote_project_codex_runtime_service::engine_info(connection_id, None)
                        .await
                        .map(|_| ())
                        .map_err(err_to_string)
                }
                "opencode" => unreachable!("OpenCode prewarm already uses OpenCodeCli"),
                "claude" => unreachable!("Claude prewarm already uses ClaudeCodeCli"),
                _ => Err(format!("SSH 远端项目当前阶段尚未接入 {engine_id} 正式对话")),
            };
        }
    }
    state
        .engines
        .prewarm(&engine_id)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn list_codex_skills(
    state: State<'_, AppState>,
    cwd: String,
    workspace_id: Option<String>,
) -> Result<Vec<CodexSkillDto>, String> {
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(workspace_id.as_deref())
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    return cli
        .list_codex_skills(&context, cwd.trim())
        .await
        .map_err(err_to_string);

    #[allow(unreachable_code)]
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            let service_use =
                crate::remote_project_codex_runtime_service::acquire_temporary(&workspace)
                    .await
                    .map_err(err_to_string)?;
            let result = service_use.engine().list_skills(&workspace.root_path).await;
            service_use.release().await;
            return result.map_err(err_to_string);
        }
    }
    state
        .engines
        .list_codex_skills(cwd.trim())
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn list_codex_apps(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<Vec<CodexAppDto>, String> {
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(workspace_id.as_deref())
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    return cli.list_codex_apps(&context).await.map_err(err_to_string);

    #[allow(unreachable_code)]
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            let service_use =
                crate::remote_project_codex_runtime_service::acquire_temporary(&workspace)
                    .await
                    .map_err(err_to_string)?;
            let result = service_use.engine().list_apps().await;
            service_use.release().await;
            return result.map_err(err_to_string);
        }
    }
    state.engines.list_codex_apps().await.map_err(err_to_string)
}

#[tauri::command]
pub async fn list_codex_plugins(
    state: State<'_, AppState>,
    cwd: String,
    workspace_id: Option<String>,
) -> Result<Vec<CodexPluginDto>, String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err("cwd is required".to_string());
    }
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(workspace_id.as_deref())
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    return cli
        .list_codex_plugins(&context, cwd)
        .await
        .map_err(err_to_string);

    #[allow(unreachable_code)]
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err("cwd is required".to_string());
    }
    if let Some(workspace_id) = workspace_id {
        let db = state.db.clone();
        let lookup_workspace_id = workspace_id.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            crate::db::workspaces::find_workspace_by_id(&db, &lookup_workspace_id)
        })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?
        .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
        if workspace.location_kind == "ssh" {
            let service_use =
                crate::remote_project_codex_runtime_service::acquire_temporary(&workspace)
                    .await
                    .map_err(err_to_string)?;
            // SSH 项目必须使用数据库中的远端项目根目录，不能把前端仓库路径传给远端 CLI。
            let result = service_use
                .engine()
                .list_plugins(&workspace.root_path)
                .await;
            service_use.release().await;
            return result.map_err(err_to_string);
        }
    }
    state
        .engines
        .list_codex_plugins(cwd)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn get_opencode_runtime_catalog(
    state: State<'_, AppState>,
    cwd: String,
    workspace_id: Option<String>,
) -> Result<OpenCodeRuntimeCatalogDto, String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err("cwd is required".to_string());
    }
    let opencode = CliToolFactory::new(state.inner().clone())
        .create("opencode")
        .expect("OpenCode CLI factory mapping must exist");
    let context = if let Some(workspace_id) = workspace_id.as_deref() {
        opencode.execution_context(Some(workspace_id)).await
    } else {
        opencode.execution_context_for_cwd(Some(cwd)).await
    }
    .map_err(err_to_string)?;
    let cli: &dyn CliTool = opencode.as_ref();
    cli.get_opencode_runtime_catalog(&context, cwd)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn run_engine_check(
    state: State<'_, AppState>,
    engine_id: String,
    command: String,
) -> Result<EngineCheckResultDto, String> {
    let health = state
        .engines
        .health(&engine_id)
        .await
        .map_err(err_to_string)?;
    let is_allowed = health
        .checks
        .iter()
        .chain(health.fixes.iter())
        .any(|value| value == &command);

    if !is_allowed {
        return Err("command is not allowed for this engine check".to_string());
    }

    execute_engine_check_command(&command)
        .await
        .map_err(err_to_string)
}

async fn execute_engine_check_command(command: &str) -> anyhow::Result<EngineCheckResultDto> {
    let started = Instant::now();

    let output = build_shell_command(command)
        .await
        .output()
        .await
        .with_context(|| format!("failed to execute check command: `{command}`"))?;

    let duration_ms = started.elapsed().as_millis();

    Ok(EngineCheckResultDto {
        command: command.to_string(),
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: truncate_output(&String::from_utf8_lossy(&output.stdout), 12_000),
        stderr: truncate_output(&String::from_utf8_lossy(&output.stderr), 12_000),
        duration_ms,
    })
}

#[cfg(target_os = "windows")]
async fn build_shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    process_utils::configure_tokio_command(&mut cmd);
    cmd.arg("/C").arg(command);
    cmd.envs(runtime_env::get(Path::new("cmd")).await);
    cmd
}

#[cfg(not(target_os = "windows"))]
async fn build_shell_command(command: &str) -> Command {
    let spec = runtime_env::command_shell_for_string(command);
    let mut cmd = Command::new(&spec.program);
    process_utils::configure_tokio_command(&mut cmd);
    cmd.args(&spec.args);
    // 旧手工 PATH 处理由 runtime_env::get 接替：
    // if let Some(augmented_path) = runtime_env::augmented_path_with_prepend(
    //     spec.program
    //         .parent()
    //         .into_iter()
    //         .map(|value| value.to_path_buf()),
    // ) {
    //     cmd.env("PATH", augmented_path);
    // }
    cmd.envs(runtime_env::get(&spec.program).await);
    cmd
}

fn truncate_output(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }

    let mut out = chars.into_iter().take(max_chars).collect::<String>();
    out.push_str("\n...[truncated]");
    out
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}
