//! SSH 远端项目会话同步服务。

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures::{SinkExt, StreamExt};
use rusqlite::{params_from_iter, types::Value as SqlValue};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliTool},
    db::{threads, workspaces, Database},
    message_notify_helper::{
        notify_app_startup_progress, notify_ssh_remote_project_sessions_refreshed,
        SshRemoteProjectSessionsRefreshedEvent,
    },
    models::{ThreadStatusDto, WorkspaceDto},
    path_utils, runtime_env,
    ssh::{
        cli_service_lifecycle,
        cli_tunnel_registry::{self, SshCliTunnel},
    },
    state::AppState,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PAGES: usize = 50;

/// 单个工作区的同步报告。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteProjectSessionRefreshReport {
    /// 工作区唯一标识。
    pub workspace_id: String,
    /// 成功同步的 CLI。
    pub succeeded_cli_ids: Vec<String>,
    /// 失败的 CLI。
    pub failed_cli_ids: Vec<String>,
}

static REFRESHING_WORKSPACES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

struct WorkspaceRefreshGuard {
    /// 当前互斥锁占用的工作区标识。
    workspace_id: String,
}

impl WorkspaceRefreshGuard {
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

/// 按数据库顺序刷新全部已启用 SSH 工作区。
pub async fn refresh_all_ssh_remote_project_sessions(
    app: &AppHandle,
    db: Arc<Database>,
) -> Vec<SshRemoteProjectSessionRefreshReport> {
    let records = match tokio::task::spawn_blocking({
        let db = db.clone();
        move || workspaces::list_workspaces(db.as_ref())
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
    let workspace = load_workspace(db.clone(), workspace_id).await?;
    let connection_id = workspace
        .ssh_connection_id
        .as_deref()
        .context("SSH workspace has no connection id")?
        .to_string();
    let tunnels = cli_tunnel_registry::list_by_host(&connection_id).await;
    let mut cli_ids = tunnels.keys().cloned().collect::<Vec<_>>();
    cli_ids.sort();
    let mut report = SshRemoteProjectSessionRefreshReport {
        workspace_id: workspace_id.to_string(),
        succeeded_cli_ids: Vec::new(),
        failed_cli_ids: Vec::new(),
    };
    for cli_id in cli_ids {
        match cli_id.as_str() {
            "codex" | "opencode" | "claude" => {
                match sync_cli(app, &workspace, &connection_id, &cli_id, db.clone()).await {
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
            other => log::debug!("跳过未支持 SSH 远端 CLI: {other}"),
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

fn is_enabled_ssh_workspace(workspace: &WorkspaceDto) -> bool {
    workspace.location_kind == "ssh"
        && workspace.ssh_connection_id.is_some()
        && workspace.connection_enabled == Some(true)
        && workspace.connection_deleted != Some(true)
}

async fn load_workspace(db: Arc<Database>, workspace_id: &str) -> Result<WorkspaceDto> {
    let id = workspace_id.to_string();
    let workspace =
        tokio::task::spawn_blocking(move || workspaces::find_workspace_by_id(db.as_ref(), &id))
            .await
            .context("load workspace task failed")??;
    workspace
        .filter(is_enabled_ssh_workspace)
        .ok_or_else(|| anyhow::anyhow!("enabled SSH workspace not found: {workspace_id}"))
}

async fn sync_cli(
    app: &AppHandle,
    workspace: &WorkspaceDto,
    connection_id: &str,
    cli_id: &str,
    db: Arc<Database>,
) -> Result<()> {
    // 会话扫描前先登记常驻服务。应用启动时首次建立服务并写入 Map，后续刷新复用
    // 已登记的服务；CLI 实现仍走各自的读取逻辑，但服务不会因一次扫描结束而关闭。
    cli_service_lifecycle::set(connection_id, cli_id)
        .await
        .with_context(|| {
            format!(
                "启动并登记 SSH 远端 CLI 服务失败: connection_id={connection_id} cli_id={cli_id}"
            )
        })?;

    if let Err(error) =
        notify_app_startup_progress(app, "syncing-remote-sessions", "正在同步远端会话……")
    {
        log::warn!("发送启动进度失败: {error:#}");
    }

    if cli_id == "codex" {
        let state = app.state::<AppState>();
        let context = CliExecutionContext::from_workspace(workspace)?;
        let codex = CliToolFactory::new(state.inner().clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let cli: &dyn CliTool = codex.as_ref();
        let sessions = cli.list_sessions(&context, None, Some(false)).await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| path_utils::paths_equal(&session.cwd, &workspace.root_path))
            .map(|session| RemoteSessionSnapshot {
                engine_thread_id: session.engine_thread_id,
                title: session.title,
                cwd: session.cwd,
                model_id: session.model_id,
                reasoning_effort: session.reasoning_effort,
                updated_at: session.updated_at,
                status: session.status,
                metadata: session.metadata,
            })
            .collect();
        return persist_sessions(db, workspace, "codex", sessions).await;
    }

    if cli_id == "opencode" {
        let state = app.state::<AppState>();
        let context = CliExecutionContext::from_workspace(workspace)?;
        let opencode = CliToolFactory::new(state.inner().clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let cli: &dyn CliTool = opencode.as_ref();
        let sessions = cli.list_sessions(&context, None, Some(false)).await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| path_utils::paths_equal(&session.cwd, &workspace.root_path))
            .map(|session| RemoteSessionSnapshot {
                engine_thread_id: session.engine_thread_id,
                title: session.title,
                cwd: session.cwd,
                model_id: session.model_id,
                reasoning_effort: session.reasoning_effort,
                updated_at: session.updated_at,
                status: session.status,
                metadata: session.metadata,
            })
            .collect();
        return persist_sessions(db, workspace, "opencode", sessions).await;
    }

    if cli_id == "claude" {
        let state = app.state::<AppState>();
        let context = CliExecutionContext::from_workspace(workspace)?;
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let cli: &dyn CliTool = claude.as_ref();
        let sessions = cli.list_sessions(&context, None, Some(false)).await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| path_utils::paths_equal(&session.cwd, &workspace.root_path))
            .map(|session| RemoteSessionSnapshot {
                engine_thread_id: session.engine_thread_id,
                title: session.title,
                cwd: session.cwd,
                model_id: session.model_id,
                reasoning_effort: session.reasoning_effort,
                updated_at: session.updated_at,
                status: session.status,
                metadata: session.metadata,
            })
            .collect();
        return persist_sessions(db, workspace, "claude", sessions).await;
    }

    /*
    旧实现会在刷新函数末尾直接取得并释放 Tunnel 的远端服务占用。三个受支持 CLI
    已经全部通过上面的 CLI 工厂调用返回，这段 Tunnel 直连逻辑不再执行：
    let tunnel = cli_tunnel_registry::acquire_temporary_service_use(connection_id, cli_id).await?;
    let result = match cli_id {
        "opencode" => unreachable!("OpenCode session sync already uses OpenCodeCli"),
        "claude" => unreachable!("Claude session sync already uses ClaudeCodeCli"),
        _ => Ok(()),
    };
    let release_result =
        cli_tunnel_registry::release_temporary_service_use(connection_id, cli_id).await;
    match (result, release_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(_)) => Ok(()),
        (Ok(()), Err(error)) => Err(error).context(format!(
            "failed release SSH CLI temporary use: connection_id={connection_id} cli_id={cli_id}"
        )),
    }
    */
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSessionSnapshot {
    /// 远端会话 ID。
    pub(crate) engine_thread_id: String,
    /// 远端标题。
    pub(crate) title: String,
    /// 远端 cwd。
    pub(crate) cwd: String,
    /// 远端模型；不可靠时为 unknown。
    pub(crate) model_id: String,
    /// 远端思考强度。
    pub(crate) reasoning_effort: Option<String>,
    /// 远端最后活动时间。
    pub(crate) updated_at: Option<String>,
    /// 映射后的本地状态。
    pub(crate) status: ThreadStatusDto,
    /// 远端元数据。
    pub(crate) metadata: Value,
}

async fn persist_sessions(
    db: Arc<Database>,
    workspace: &WorkspaceDto,
    engine_id: &str,
    sessions: Vec<RemoteSessionSnapshot>,
) -> Result<()> {
    if sessions.is_empty() {
        return Ok(());
    }

    if engine_id == "claude" {
        // Claude Code 启动同步只负责导入本地尚不存在的会话。把整批远端数据
        // 一次交给 SQLite，通过 NOT EXISTS 在 INSERT 前排除已有记录；这里不
        // 做前置 SELECT、不逐条查库，也不在 Rust 中维护已有 ID 集合。
        let value_groups = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; sessions.len()].join(", ");
        let sql = format!(
            r#"
            WITH remote_sessions (
                id, engine_thread_id, model_id, title, status,
                engine_metadata_json, reasoning_effort, last_activity_at, created_at
            ) AS (VALUES {value_groups})
            INSERT INTO threads (
                id, workspace_id, repo_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, title, status, reasoning_effort, last_activity_at, created_at
            )
            SELECT
                remote.id, ?, NULL, 'claude', remote.model_id, remote.engine_thread_id,
                remote.engine_metadata_json, remote.title, remote.status, remote.reasoning_effort,
                CASE
                    WHEN remote.last_activity_at <> '' THEN remote.last_activity_at
                    ELSE remote.created_at
                END,
                remote.created_at
            FROM remote_sessions AS remote
            WHERE NOT EXISTS (
                SELECT 1
                FROM threads AS local
                WHERE local.workspace_id = ?
                  AND local.engine_id = 'claude'
                  AND local.engine_thread_id = remote.engine_thread_id
            )
            "#
        );
        let mut bind_values = Vec::with_capacity(sessions.len() * 9 + 2);
        for session in sessions {
            let model_id = if session.model_id.trim().is_empty() {
                "unknown".to_string()
            } else {
                session.model_id.trim().to_string()
            };
            let created_at = runtime_env::system_time_rfc3339();
            bind_values.extend([
                SqlValue::Text(Uuid::new_v4().to_string()),
                SqlValue::Text(session.engine_thread_id),
                SqlValue::Text(model_id),
                SqlValue::Text(session.title),
                SqlValue::Text(session.status.as_str().to_string()),
                SqlValue::Text(session.metadata.to_string()),
                session
                    .reasoning_effort
                    .map(SqlValue::Text)
                    .unwrap_or(SqlValue::Null),
                SqlValue::Text(session.updated_at.unwrap_or_default()),
                SqlValue::Text(created_at),
            ]);
        }
        bind_values.push(SqlValue::Text(workspace.id.clone()));
        bind_values.push(SqlValue::Text(workspace.id.clone()));

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = db.connect()?;
            let tx = conn
                .transaction()
                .context("failed begin Claude remote thread import transaction")?;
            tx.execute(&sql, params_from_iter(bind_values))
                .context("failed insert Claude remote thread snapshots")?;
            tx.commit()
                .context("failed commit Claude remote thread import transaction")?;
            Ok(())
        })
        .await
        .context("Claude remote thread import task failed")??;
        return Ok(());
    }

    for session in sessions {
        let workspace_id = workspace.id.clone();
        let engine_id = engine_id.to_string();
        tokio::task::spawn_blocking({
            let db = db.clone();
            move || {
                threads::upsert_ssh_remote_thread_snapshot(
                    db.as_ref(),
                    &workspace_id,
                    &engine_id,
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

pub(crate) async fn list_codex_sessions(
    local_port: u16,
    cwd: &str,
) -> Result<Vec<RemoteSessionSnapshot>> {
    // 旧接口接收完整 SshCliTunnel：
    // pub(crate) async fn list_codex_sessions(tunnel: &SshCliTunnel, cwd: &str) ...
    // CLI 实现现在只从远端服务生命周期取得服务入口，不再向业务函数暴露 Tunnel。
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{local_port}/"))
        .await
        .context("connect Codex SSH tunnel")?;
    let mut request_id = 1_u64;
    send_ws_request(&mut socket, &mut request_id, "initialize", json!({"clientInfo":{"name":"auracoder-ssh-sync","title":"AuraCoder SSH Sync","version":"0"},"capabilities":{"experimentalApi":true}})).await?;
    socket
        .send(Message::Text(
            json!({"method":"initialized","params":{}})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| anyhow::anyhow!("send Codex initialized notification failed: {error}"))?;
    let mut cursor: Option<String> = None;
    let mut out = Vec::new();
    for _ in 0..MAX_PAGES {
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
            // if let Some(session) = parse_codex_session(value, cwd) {
            //     out.push(session);
            // }
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
                out.push(session);
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
    Ok(out)
}

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
        let message = tokio::time::timeout(REQUEST_TIMEOUT, socket.next())
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

fn parse_codex_session(value: &Value, expected_cwd: &str) -> Option<(RemoteSessionSnapshot, bool)> {
    let thread = value.get("thread").unwrap_or(value);
    let cwd = string_field(thread, &["cwd"])?;
    if cwd != expected_cwd {
        return None;
    }
    let id = string_field(thread, &["id"])?;
    let updated = number_field(thread, &["updatedAt", "updated_at"])
        .or_else(|| number_field(thread, &["createdAt", "created_at"]));
    // let title = string_field(value, &["name", "threadName", "title"])
    //     .or_else(|| string_field(thread, &["name", "threadName", "title"]))
    //     .unwrap_or_else(|| id.clone());
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
    Some((
        RemoteSessionSnapshot {
            engine_thread_id: id,
            title,
            cwd: cwd.clone(),
            model_id,
            updated_at: updated.map(unix_to_rfc3339),
            status: status_from_value(value.get("status").or_else(|| thread.get("status"))),
            metadata: json!({
                "sshRemote": true,
                "codexRemoteCwd": cwd,
                "codexRemote": value,
                "codexModelProvider": model_provider,
            }),
            reasoning_effort,
        },
        title_needs_read,
    ))
}

async fn list_opencode_sessions(
    tunnel: &SshCliTunnel,
    cwd: &str,
) -> Result<Vec<RemoteSessionSnapshot>> {
    let secret = tunnel
        .remote_service_secret()
        .context("OpenCode tunnel has no service secret")?;
    let auth = format!("Basic {}", BASE64.encode(format!("opencode:{secret}")));
    let values = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{}/session", tunnel.local_port()))
        .header(reqwest::header::AUTHORIZATION, auth)
        .query(&[("directory", cwd), ("roots", "true"), ("limit", "200")])
        .send()
        .await
        .context("request OpenCode sessions")?
        .error_for_status()
        .context("OpenCode session request failed")?
        .json::<Vec<Value>>()
        .await
        .context("parse OpenCode sessions")?;
    Ok(values
        .iter()
        .filter_map(|value| parse_opencode_session(value, cwd))
        .collect())
}

fn parse_opencode_session(value: &Value, expected_cwd: &str) -> Option<RemoteSessionSnapshot> {
    let cwd = string_field(value, &["directory", "cwd"])?;
    if cwd != expected_cwd {
        return None;
    }
    let id = string_field(value, &["id"])?;
    let title = string_field(value, &["title", "name"]).unwrap_or_else(|| id.clone());
    let updated = value
        .get("time")
        .and_then(|time| number_field(time, &["updated"]));
    Some(RemoteSessionSnapshot {
        engine_thread_id: id,
        title,
        cwd: cwd.clone(),
        model_id: "unknown".to_string(),
        reasoning_effort: None,
        updated_at: updated.map(unix_to_rfc3339),
        status: ThreadStatusDto::Idle,
        metadata: json!({"sshRemote":true,"opencodeRemoteCwd":cwd,"opencodeRemote":value}),
    })
}

// Claude 会话查询已经归入 ClaudeCodeCli::list_sessions。公共刷新服务只调用统一
// CLI 接口，不再直接通过 tunnel 调用 Claude /sessions 协议。
// pub(crate) async fn list_claude_sessions(
//     tunnel: &SshCliTunnel,
//     cwd: &str,
// ) -> Result<Vec<RemoteSessionSnapshot>> {
//     let values = reqwest::Client::new()
//         .get(format!("http://127.0.0.1:{}/sessions", tunnel.local_port()))
//         .query(&[("cwd", cwd)])
//         .send()
//         .await
//         .context("request Claude SSH remote sessions")?
//         .error_for_status()
//         .context("Claude SSH remote session request failed")?
//         .json::<Vec<Value>>()
//         .await
//         .context("parse Claude SSH remote sessions")?;
//     Ok(values
//         .iter()
//         .filter_map(|value| parse_claude_session(value, cwd))
//         .collect())
// }

fn parse_claude_session(value: &Value, expected_cwd: &str) -> Option<RemoteSessionSnapshot> {
    let cwd = string_field(value, &["cwd"])?;
    if cwd != expected_cwd {
        return None;
    }
    let id = string_field(value, &["id"])?;
    let title = string_field(value, &["title", "name"]).unwrap_or_else(|| id.clone());
    let updated_at = string_field(value, &["updatedAt", "updated_at"])
        .or_else(|| number_field(value, &["updatedAt", "updated_at"]).map(unix_to_rfc3339));
    Some(RemoteSessionSnapshot {
        engine_thread_id: id,
        title,
        cwd: cwd.clone(),
        model_id: "unknown".to_string(),
        reasoning_effort: None,
        updated_at,
        status: ThreadStatusDto::Idle,
        metadata: json!({"sshRemote":true,"claudeRemoteCwd":cwd,"claudeRemote":value}),
    })
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str).map(str::to_owned))
}
fn number_field(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
}
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rusqlite::params;

    fn test_database_and_workspace() -> (Database, WorkspaceDto) {
        let path = std::env::temp_dir().join(format!("auracoder-claude-refresh-{}.db", Uuid::new_v4()));
        let db = Database::open(path).expect("failed to create test database");
        let workspace_id = format!("workspace-{}", Uuid::new_v4());
        let root_path = format!("/tmp/auracoder-claude-refresh-{}", Uuid::new_v4());
        let conn = db.connect().expect("failed to connect test database");
        conn.execute(
            "INSERT INTO workspaces (id, name, root_path, location_kind)
             VALUES (?1, ?2, ?3, 'local')",
            params![workspace_id, "Test workspace", root_path],
        )
        .expect("failed to insert test workspace");
        let workspace = WorkspaceDto {
            id: workspace_id,
            name: "Test workspace".to_string(),
            root_path,
            location_kind: "ssh".to_string(),
            ssh_connection_id: Some("test-connection".to_string()),
            connection_display_name: None,
            connection_enabled: Some(true),
            connection_deleted: Some(false),
            connection_status: Some("ok".to_string()),
            scan_depth: 3,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_opened_at: "2026-01-01T00:00:00Z".to_string(),
        };
        (db, workspace)
    }

    fn test_snapshot(engine_thread_id: &str, title: &str) -> RemoteSessionSnapshot {
        RemoteSessionSnapshot {
            engine_thread_id: engine_thread_id.to_string(),
            title: title.to_string(),
            cwd: "/tmp/project".to_string(),
            model_id: String::new(),
            reasoning_effort: None,
            updated_at: Some("2026-08-18T10:00:00Z".to_string()),
            status: ThreadStatusDto::Idle,
            metadata: json!({"remote": true}),
        }
    }

    #[tokio::test]
    async fn claude_batch_import_inserts_all_new_sessions() {
        let (db, workspace) = test_database_and_workspace();
        let snapshots = vec![
            test_snapshot("claude-new-1", "first"),
            test_snapshot("claude-new-2", "second"),
        ];

        persist_sessions(Arc::new(db.clone()), &workspace, "claude", snapshots)
            .await
            .expect("Claude batch import should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count imported Claude sessions");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn claude_batch_import_accepts_remote_limit_of_500_sessions() {
        let (db, workspace) = test_database_and_workspace();
        let snapshots: Vec<_> = (0..500)
            .map(|index| {
                test_snapshot(
                    &format!("claude-batch-{index}"),
                    &format!("batch title {index}"),
                )
            })
            .collect();

        persist_sessions(Arc::new(db.clone()), &workspace, "claude", snapshots)
            .await
            .expect("Claude 500-session batch import should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count 500 imported Claude sessions");
        assert_eq!(count, 500);
    }

    #[tokio::test]
    async fn claude_batch_import_skips_existing_archived_and_unarchived_sessions() {
        let (db, workspace) = test_database_and_workspace();
        let conn = db.connect().expect("failed to connect test database");
        conn.execute(
            "INSERT INTO threads (
                 id, workspace_id, engine_id, model_id, engine_thread_id,
                 engine_metadata_json, title, status, archived_at,
                 created_at, last_activity_at
             ) VALUES (?1, ?2, 'claude', 'local-model', 'claude-existing',
                       ?3, 'local-title', 'completed', ?4, ?5, ?6)",
            params![
                "local-thread-id",
                workspace.id,
                r#"{"local":true}"#,
                "2026-08-17T10:00:00Z",
                "2026-08-16T10:00:00Z",
                "2026-08-16T11:00:00Z",
            ],
        )
        .expect("failed to insert existing Claude session");
        conn.execute(
            "INSERT INTO threads (
                 id, workspace_id, engine_id, model_id, engine_thread_id,
                 engine_metadata_json, title, status, archived_at,
                 created_at, last_activity_at
             ) VALUES (?1, ?2, 'claude', 'local-active-model', 'claude-existing-active',
                       ?3, 'local-active-title', 'idle', NULL, ?4, ?5)",
            params![
                "local-active-thread-id",
                workspace.id,
                r#"{"local":true,"active":true}"#,
                "2026-08-15T10:00:00Z",
                "2026-08-15T11:00:00Z",
            ],
        )
        .expect("failed to insert active existing Claude session");
        drop(conn);

        let mut existing = test_snapshot("claude-existing", "remote-title");
        existing.model_id = "remote-model".to_string();
        existing.status = ThreadStatusDto::Streaming;
        let mut active_existing = test_snapshot("claude-existing-active", "remote-active-title");
        active_existing.model_id = "remote-active-model".to_string();
        active_existing.status = ThreadStatusDto::Completed;
        let snapshots = vec![
            existing,
            active_existing,
            test_snapshot("claude-new", "new title"),
        ];
        persist_sessions(Arc::new(db.clone()), &workspace, "claude", snapshots)
            .await
            .expect("Claude mixed batch import should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let existing_row: (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT title, model_id, status, archived_at, created_at, last_activity_at,
                        COALESCE(engine_metadata_json, '')
                 FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = 'claude-existing'",
                params![workspace.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .expect("failed to read existing Claude session");
        assert_eq!(existing_row.0, "local-title");
        assert_eq!(existing_row.1, "local-model");
        assert_eq!(existing_row.2, "completed");
        assert_eq!(existing_row.3.as_deref(), Some("2026-08-17T10:00:00Z"));
        assert_eq!(existing_row.4, "2026-08-16T10:00:00Z");
        assert_eq!(existing_row.5, "2026-08-16T11:00:00Z");
        assert_eq!(existing_row.6, r#"{"local":true}"#);

        let active_existing_row: (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT title, model_id, status, archived_at, created_at, last_activity_at,
                        COALESCE(engine_metadata_json, '')
                 FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = 'claude-existing-active'",
                params![workspace.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .expect("failed to read active existing Claude session");
        assert_eq!(active_existing_row.0, "local-active-title");
        assert_eq!(active_existing_row.1, "local-active-model");
        assert_eq!(active_existing_row.2, "idle");
        assert_eq!(active_existing_row.3, None);
        assert_eq!(active_existing_row.4, "2026-08-15T10:00:00Z");
        assert_eq!(active_existing_row.5, "2026-08-15T11:00:00Z");
        assert_eq!(active_existing_row.6, r#"{"local":true,"active":true}"#);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count mixed Claude sessions");
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn non_claude_batch_persistence_keeps_existing_upsert_behavior() {
        let (db, workspace) = test_database_and_workspace();
        persist_sessions(
            Arc::new(db.clone()),
            &workspace,
            "codex",
            vec![test_snapshot("codex-session", "first title")],
        )
        .await
        .expect("Codex import should succeed");
        persist_sessions(
            Arc::new(db.clone()),
            &workspace,
            "codex",
            vec![test_snapshot("codex-session", "updated title")],
        )
        .await
        .expect("Codex refresh should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let (count, title): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(title) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'codex'",
                params![workspace.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("failed to read Codex session");
        assert_eq!(count, 1);
        assert_eq!(title, "updated title");
    }

    #[test]
    fn codex_parser_filters_by_cwd() {
        let value = json!({"thread":{"id":"same","cwd":"/a","createdAt":1}});
        assert!(parse_codex_session(&value, "/b").is_none());
        assert_eq!(
            parse_codex_session(&value, "/a")
                .unwrap()
                .0
                .engine_thread_id,
            "same"
        );
    }

    #[test]
    fn codex_parser_uses_preview_when_explicit_title_is_missing() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "preview": "第一行标题\n后续内容",
                "createdAt": 1
            }
        });

        let (session, title_needs_read) = parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.title, "第一行标题");
        assert!(!title_needs_read);
    }

    #[test]
    fn codex_parser_keeps_model_and_reasoning_effort() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "createdAt": 1,
                "model": "gpt-5.6-terra",
                "modelProvider": "llm_router",
                "reasoningEffort": "high"
            }
        });

        let (session, _) = parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.model_id, "gpt-5.6-terra");
        assert_eq!(session.reasoning_effort.as_deref(), Some("high"));
        /*
        // 旧实现把思考强度镜像写入 metadata，已停用：
        assert_eq!(
            session.metadata.get("reasoningEffort"),
            Some(&json!("high"))
        );
        */
        assert_eq!(
            session.metadata.get("codexModelProvider"),
            Some(&json!("llm_router"))
        );
    }

    #[test]
    fn codex_parser_marks_compacted_preview_for_thread_read() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "preview": "<<ccr:8859c5dfe071,string,279B>>",
                "createdAt": 1
            }
        });

        let (session, title_needs_read) = parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.title, "thread-a");
        assert!(title_needs_read);
    }
    #[test]
    fn opencode_parser_filters_by_cwd() {
        let value = json!({"id":"same","directory":"/a","time":{"updated":2}});
        assert!(parse_opencode_session(&value, "/b").is_none());
        assert_eq!(parse_opencode_session(&value, "/a").unwrap().cwd, "/a");
    }

    #[test]
    fn claude_parser_filters_by_cwd_and_keeps_remote_timestamp() {
        let value = json!({
            "id":"same",
            "cwd":"/a",
            "title":"remote claude",
            "updatedAt":"2026-08-14T09:00:00.000Z"
        });
        assert!(parse_claude_session(&value, "/b").is_none());
        let session = parse_claude_session(&value, "/a").unwrap();
        assert_eq!(session.engine_thread_id, "same");
        assert_eq!(
            session.updated_at.as_deref(),
            Some("2026-08-14T09:00:00.000Z")
        );
    }
}
