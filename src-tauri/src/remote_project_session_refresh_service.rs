//! 公共本地与 SSH 项目会话刷新服务，仅负责调度、工作区校验和刷新报告。

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock, Mutex},
};

use anyhow::{Context, Result};
use futures::{stream, StreamExt};
use tauri::{AppHandle, Manager};

use crate::db::Database;
use crate::db::workspaces::{find_workspace_by_id, list_workspaces};

use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliTool},
    message_notify_helper::{
        notify_app_startup_progress, notify_ssh_remote_project_sessions_refreshed,
        SshRemoteProjectSessionsRefreshedEvent,
    },
    models::WorkspaceDto,
    state::AppState,
};

/*
/// 单个工作区的 SSH 会话刷新报告。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteProjectSessionRefreshReport {
    /// 工作区唯一标识。
    pub workspace_id: String,
    /// 成功刷新的 CLI。
    pub succeeded_cli_ids: Vec<String>,
    /// 刷新失败的 CLI。
    pub failed_cli_ids: Vec<String>,
}

/// 单个本地工作区的会话刷新报告。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProjectSessionRefreshReport {
    /// 工作区唯一标识。
    pub workspace_id: String,
    /// 成功刷新的 CLI。
    pub succeeded_cli_ids: Vec<String>,
    /// 刷新失败的 CLI。
    pub failed_cli_ids: Vec<String>,
}

*/

// Service：src-tauri/src/remote_project_session_refresh_service.rs
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSessionRefreshReport {
    pub workspace_id: String,
    pub succeeded_cli_ids: Vec<String>,
    pub failed_cli_ids: Vec<String>,
}

// private：按项目 ID 同步会话
async fn sync_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
    workspace_id: &str,
) -> Result<ProjectSessionRefreshReport> {
    let _guard = WorkspaceRefreshGuard::acquire(workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace refresh already in progress: {workspace_id}"))?;

    let id = workspace_id.to_string();
    let workspace = tokio::task::spawn_blocking({
        let db = db.clone();
        move || find_workspace_by_id(db.as_ref(), &id)
    })
    .await
    .context("读取项目任务失败")??
    .ok_or_else(|| anyhow::anyhow!("项目不存在: {workspace_id}"))?;

    let is_ssh = workspace.location_kind == "ssh";
    if is_ssh && !is_enabled_ssh_workspace(&workspace) {
        anyhow::bail!("SSH 项目不可用: {workspace_id}");
    }

    let factory = CliToolFactory::new(app.state::<AppState>().inner().clone());
    let context = CliExecutionContext::from_workspace(&workspace)?;
    let mut report = ProjectSessionRefreshReport {
        workspace_id: workspace_id.to_string(),
        succeeded_cli_ids: Vec::new(),
        failed_cli_ids: Vec::new(),
    };
    let mut prepared_clis: Vec<(&str, Arc<dyn CliTool>)> = Vec::new();

    if !is_ssh {
        log::info!(
            "本地项目会话同步开始: workspace_id={} workspace_root_path={} cli_order=codex,opencode,claude succeeded_cli_ids=[] failed_cli_ids=[]",
            workspace_id,
            workspace.root_path
        );
    }

    for cli_id in ["codex", "opencode", "claude"] {
        let cli = match factory.create(cli_id) {
            Ok(cli) => cli,
            Err(error) => {
                log::warn!(
                    "创建 CLI 失败: workspace_id={} cli_id={} error={error:#}",
                    workspace_id,
                    cli_id
                );
                report.failed_cli_ids.push(cli_id.to_string());
                continue;
            }
        };

        if is_ssh {
            if let Err(error) = cli.ensure_service(&context).await {
                log::warn!(
                    "确保 SSH CLI 服务失败: workspace_id={} cli_id={} error={error:#}",
                    workspace_id,
                    cli_id
                );
                report.failed_cli_ids.push(cli_id.to_string());
                continue;
            }
        }
        prepared_clis.push((cli_id, cli));
    }

    for (cli_id, cli) in prepared_clis {
        if is_ssh {
            if let Err(error) = notify_app_startup_progress(
                app,
                "syncing-remote-sessions",
                "正在同步远端会话……",
            ) {
                log::warn!("发送启动进度失败: {error:#}");
            }
        }
        match sync_cli(&factory, &workspace, cli_id, Some(cli)).await {
            Ok(()) => report.succeeded_cli_ids.push(cli_id.to_string()),
            Err(error) => {
                log::warn!(
                    "同步项目会话失败: workspace_id={} cli_id={} error={error:#}",
                    workspace_id,
                    cli_id
                );
                report.failed_cli_ids.push(cli_id.to_string());
            }
        }
    }

    if !is_ssh {
        log::info!(
            "本地项目会话同步结束: workspace_id={} workspace_root_path={} cli_order=codex,opencode,claude succeeded_cli_ids={:?} failed_cli_ids={:?}",
            workspace_id,
            workspace.root_path,
            report.succeeded_cli_ids,
            report.failed_cli_ids
        );
    }

    if is_ssh {
        if let Err(error) = notify_ssh_remote_project_sessions_refreshed(
            app,
            SshRemoteProjectSessionsRefreshedEvent {
                workspace_id: report.workspace_id.clone(),
                succeeded_cli_ids: report.succeeded_cli_ids.clone(),
                failed_cli_ids: report.failed_cli_ids.clone(),
            },
        ) {
            log::warn!("发送 SSH 会话刷新通知失败: {error:#}");
        }
    }

    Ok(report)
}

static REFRESHING_WORKSPACES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// 防止同一工作区同时执行多个刷新任务。
struct WorkspaceRefreshGuard {
    /// 当前互斥锁占用的工作区标识。
    workspace_id: String,
}

impl WorkspaceRefreshGuard {
    /// 获取工作区刷新锁；已有刷新任务时拒绝重复进入。
    fn acquire(id: &str) -> Option<Self> {
        let mut set = REFRESHING_WORKSPACES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        set.insert(id.to_string()).then(|| Self {
            workspace_id: id.to_string(),
        })
    }
}

impl Drop for WorkspaceRefreshGuard {
    fn drop(&mut self) {
        let mut set = REFRESHING_WORKSPACES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        set.remove(&self.workspace_id);
    }
}

/*
/// 按数据库顺序刷新全部已启用 SSH 工作区，并汇总可完成的报告。
pub async fn refresh_all_ssh_remote_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
) -> Vec<SshRemoteProjectSessionRefreshReport> {
    // 先统一读取工作区，再逐个调用公开的单工作区刷新入口。
    let records = match tokio::task::spawn_blocking({
        let db = db.clone();
        move || list_workspaces(db.as_ref())
    })
    .await
    {
        Ok(Ok(records)) => records,
        Ok(Err(error)) => {
            log::warn!("读取 SSH 工作区失败: {error:#}");
            return Vec::new();
        }
        Err(error) => {
            log::warn!("读取 SSH 工作区任务失败: {error:#}");
            return Vec::new();
        }
    };
    let mut reports = Vec::new();
    for workspace in records.into_iter().filter(is_enabled_ssh_workspace) {
        match refresh_ssh_remote_project_sessions(app, db.clone(), &workspace.id).await {
            Ok(report) => reports.push(report),
            Err(error) => log::warn!(
                "刷新 SSH 工作区失败: workspace_id={} error={error:#}",
                workspace.id
            ),
        }
    }
    reports
}

/// 刷新一个 SSH 工作区；远端缺失的本地线程不会被删除。
pub async fn refresh_ssh_remote_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
    workspace_id: &str,
) -> Result<SshRemoteProjectSessionRefreshReport> {
    let _guard = WorkspaceRefreshGuard::acquire(workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace refresh already in progress: {workspace_id}"))?;
    let workspace = load_workspace(db, workspace_id).await?;
    let connection_id = workspace
        .ssh_connection_id
        .as_deref()
        .context("SSH workspace has no connection id")?
        .to_string();
    let state = app.state::<AppState>();
    let factory = CliToolFactory::new(state.inner().clone());
    let context = CliExecutionContext::from_workspace(&workspace)?;
    let mut prepared_clis = Vec::new();
    let mut report = SshRemoteProjectSessionRefreshReport {
        workspace_id: workspace_id.to_string(),
        succeeded_cli_ids: Vec::new(),
        failed_cli_ids: Vec::new(),
    };

    // 第一阶段按固定顺序创建 CLI 并确保服务就绪；每个失败 CLI 都立刻进入报告。
    for cli_id in ["codex", "opencode", "claude"] {
        let cli = match factory.create(cli_id) {
            Ok(cli) => cli,
            Err(error) => {
                log::warn!(
                    "创建 SSH 远端 CLI 统一接口失败: connection_id={} cli_id={} error={error:#}",
                    connection_id,
                    cli_id,
                );
                report.failed_cli_ids.push(cli_id.to_string());
                continue;
            }
        };
        match cli.ensure_service(&context).await {
            Ok(()) => prepared_clis.push((cli_id.to_string(), cli)),
            Err(error) => {
                log::warn!(
                    "确保 SSH 远端 CLI 服务失败: connection_id={} cli_id={} error={error:#}",
                    connection_id,
                    cli_id,
                );
                report.failed_cli_ids.push(cli_id.to_string());
            }
        }
    }

    // 第二阶段逐个刷新就绪 CLI；单个读取或落库错误只记入报告并继续后续 CLI。
    for (cli_id, cli) in prepared_clis {
        if let Err(error) =
            notify_app_startup_progress(app, "syncing-remote-sessions", "正在同步远端会话……")
        {
            log::warn!("发送启动进度失败: {error:#}");
        }
        match sync_cli(&factory, &workspace, &cli_id, Some(cli)).await {
            Ok(()) => report.succeeded_cli_ids.push(cli_id),
            Err(error) => {
                log::warn!(
                    "同步 SSH 远端 CLI 失败: workspace_id={} cli_id={} error={error:#}",
                    workspace_id,
                    cli_id
                );
                report.failed_cli_ids.push(cli_id);
            }
        }
    }
    if let Err(error) = notify_ssh_remote_project_sessions_refreshed(
        app,
        SshRemoteProjectSessionsRefreshedEvent {
            workspace_id: report.workspace_id.clone(),
            succeeded_cli_ids: report.succeeded_cli_ids.clone(),
            failed_cli_ids: report.failed_cli_ids.clone(),
        },
    ) {
        log::warn!("SSH 会话刷新通知失败（数据已提交）: {error:#}");
    }
    Ok(report)
}

*/

// Service：src-tauri/src/remote_project_session_refresh_service.rs
// public：同步全部项目
pub async fn refresh_all_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
) -> Vec<ProjectSessionRefreshReport> {
    let workspaces = match tokio::task::spawn_blocking({
        let db = db.clone();
        move || list_workspaces(db.as_ref())
    })
    .await
    {
        Ok(Ok(workspaces)) => workspaces,
        Ok(Err(error)) => {
            log::warn!("读取项目列表失败: {error:#}");
            return Vec::new();
        }
        Err(error) => {
            log::warn!("读取项目列表任务失败: {error:#}");
            return Vec::new();
        }
    };

    let max_concurrent_projects = match std::thread::available_parallelism() {
        Ok(threads) => (threads.get() / 2).max(1),
        Err(error) => {
            log::warn!("读取 CPU 逻辑线程数失败，项目同步并发数使用 6: error={error:#}");
            6
        }
    };
    log::info!("全量项目会话同步开始: max_concurrent_projects={max_concurrent_projects}");

    // 原逐项目串行同步保留注释，改由下方按并发上限调度。
    /*
    let mut reports = Vec::new();
    for workspace in workspaces {
        if !is_local_workspace(&workspace) && !is_enabled_ssh_workspace(&workspace) {
            continue;
        }
        match sync_project_sessions(app, db.clone(), &workspace.id).await {
            Ok(report) => reports.push(report),
            Err(error) => {
                log::warn!(
                    "同步项目会话失败: workspace_id={} error={error:#}",
                    workspace.id,
                );
            }
        }
    }
    reports
    */
    let mut pending = stream::iter(workspaces.into_iter().filter(|workspace| {
        is_local_workspace(workspace) || is_enabled_ssh_workspace(workspace)
    }))
    .map(|workspace| {
        let db = db.clone();
        async move {
            let result = sync_project_sessions(app, db, &workspace.id).await;
            (workspace.id, result)
        }
    })
    .buffer_unordered(max_concurrent_projects);

    let mut reports = Vec::new();
    while let Some((workspace_id, result)) = pending.next().await {
        match result {
            Ok(report) => reports.push(report),
            Err(error) => {
                log::warn!("同步项目会话失败: workspace_id={} error={error:#}", workspace_id);
            }
        }
    }
    reports
}

// public：同步指定项目
pub async fn refresh_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
    workspace_id: &str,
) -> Result<ProjectSessionRefreshReport> {
    sync_project_sessions(app, db, workspace_id).await
}

/// 判断工作区是否允许走 SSH 项目会话刷新入口。
fn is_enabled_ssh_workspace(workspace: &WorkspaceDto) -> bool {
    workspace.location_kind == "ssh"
        && workspace.ssh_connection_id.is_some()
        && workspace.connection_enabled == Some(true)
        && workspace.connection_deleted != Some(true)
}

/// 判断工作区是否允许走本机历史会话刷新入口。
fn is_local_workspace(workspace: &WorkspaceDto) -> bool {
    workspace.location_kind != "ssh"
}

/*
/// 从数据库读取并校验一个已启用 SSH 工作区。
async fn load_workspace(db: Arc<Database>, workspace_id: &str) -> Result<WorkspaceDto> {
    let id = workspace_id.to_string();
    let workspace = tokio::task::spawn_blocking(move || find_workspace_by_id(db.as_ref(), &id))
        .await
        .context("load workspace task failed")??;
    workspace
        .filter(is_enabled_ssh_workspace)
        .ok_or_else(|| anyhow::anyhow!("enabled SSH workspace not found: {workspace_id}"))
}

/// 从数据库读取并校验一个本地工作区，拒绝 SSH 工作区误用本地刷新入口。
async fn load_local_workspace(db: Arc<Database>, workspace_id: &str) -> Result<WorkspaceDto> {
    let id = workspace_id.to_string();
    let workspace = tokio::task::spawn_blocking(move || find_workspace_by_id(db.as_ref(), &id))
        .await
        .context("读取本地 workspace 任务失败")??;
    workspace
        .filter(is_local_workspace)
        .ok_or_else(|| anyhow::anyhow!("本地 workspace 不存在或实际为 SSH 项目: {workspace_id}"))
}

/// 按固定的 Codex、OpenCode、Claude 顺序刷新一个本地工作区的历史会话。
pub async fn refresh_local_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
    workspace_id: &str,
) -> Result<LocalProjectSessionRefreshReport> {
    let _guard = WorkspaceRefreshGuard::acquire(workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace refresh already in progress: {workspace_id}"))?;
    let workspace = load_local_workspace(db, workspace_id).await?;
    log::info!(
        "本地项目会话同步开始: workspace_id={} workspace_root_path={} cli_order=codex,opencode,claude succeeded_cli_ids=[] failed_cli_ids=[]",
        workspace_id,
        workspace.root_path
    );
    let state = app.state::<AppState>();
    let factory = CliToolFactory::new(state.inner().clone());
    let mut report = LocalProjectSessionRefreshReport {
        workspace_id: workspace_id.to_string(),
        succeeded_cli_ids: Vec::new(),
        failed_cli_ids: Vec::new(),
    };

    // 每个 CLI 独立创建和刷新，失败时继续处理固定顺序中的其他 CLI。
    for cli_id in ["codex", "opencode", "claude"] {
        match sync_cli(&factory, &workspace, cli_id, None).await {
            Ok(()) => report.succeeded_cli_ids.push(cli_id.to_string()),
            Err(error) => {
                log::warn!(
                    "同步本地项目 CLI 失败: workspace_id={} cli_id={} error={error:#}",
                    workspace_id,
                    cli_id
                );
                report.failed_cli_ids.push(cli_id.to_string());
            }
        }
    }
    log::info!(
        "本地项目会话同步结束: workspace_id={} workspace_root_path={} cli_order=codex,opencode,claude succeeded_cli_ids={:?} failed_cli_ids={:?}",
        workspace_id,
        workspace.root_path,
        report.succeeded_cli_ids,
        report.failed_cli_ids
    );
    Ok(report)
}

*/

/// 通过 CLI 工厂和统一刷新接口完成一个 CLI 的项目会话刷新。
async fn sync_cli(
    factory: &CliToolFactory,
    workspace: &WorkspaceDto,
    cli_id: &str,
    prepared_cli: Option<Arc<dyn CliTool>>,
) -> Result<()> {
    // 复用 SSH 预先确保就绪的实现，或为本地刷新创建对应 CLI 实现。
    let cli = match prepared_cli {
        Some(cli) => cli,
        None => factory.create(cli_id)?,
    };
    let context = CliExecutionContext::from_workspace(workspace)?;
    cli.refresh_workspace_sessions(&context).await
}

#[cfg(test)]
#[path = "../tests/unit/project_session_refresh_tests.rs"]
mod tests;
