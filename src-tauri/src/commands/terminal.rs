use std::path::{Path, PathBuf};
use tauri::State;

use crate::{
    db,
    models::{
        TerminalNotificationDto, TerminalRendererDiagnosticsDto, TerminalResumeSessionDto,
        TerminalSessionDto,
    },
    path_utils,
    ssh::{
        gateway, remote_git,
        runtime::{
            quote_posix, remote_repo_marker, resolve_workspace_target,
            validate_remote_relative_path, workspace_id_from_repo_marker,
            worktree_path_from_repo_marker,
        },
    },
    state::AppState,
};

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|error| error.to_string())?
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_create_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> Result<TerminalSessionDto, String> {
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    let (resolved_cwd, remote_connection) = if target.is_remote() {
        let connection = target.remote_connection().map_err(err_to_string)?.clone();
        let resolved_cwd = resolve_remote_cwd(
            &connection,
            &target.workspace.id,
            &target.workspace.root_path,
            cwd.as_deref(),
        )
        .await?;
        (resolved_cwd, Some(connection))
    } else {
        let workspace_root = target.workspace.root_path;
        let workspace_root_canonical =
            canonicalize_existing_dir(&workspace_root, "workspace root directory")?;
        let resolved_cwd = match cwd {
            Some(path) => {
                let cwd_canonical = canonicalize_existing_dir(&path, "cwd")?;
                if !cwd_canonical.starts_with(&workspace_root_canonical) {
                    return Err(format!(
                        "cwd must be inside workspace root: {}",
                        workspace_root_canonical.to_string_lossy()
                    ));
                }
                cwd_canonical.to_string_lossy().to_string()
            }
            None => workspace_root_canonical.to_string_lossy().to_string(),
        };
        (resolved_cwd, None)
    };
    state
        .terminals
        .create_session(
            app,
            state.notifications.clone(),
            workspace_id,
            resolved_cwd,
            remote_connection,
            cols.max(1),
            rows.max(1),
        )
        .await
        .map_err(err_to_string)
}

async fn resolve_remote_cwd(
    connection: &crate::db::ssh_connections::SshConnectionRecord,
    workspace_id: &str,
    root: &str,
    cwd: Option<&str>,
) -> Result<String, String> {
    let requested = cwd.unwrap_or(root).trim();
    if requested.is_empty() || requested.contains('\0') {
        return Err("远端终端目录无效".to_string());
    }
    let (requested, allowed_root) = if let Some(worktree) =
        worktree_path_from_repo_marker(requested).map_err(err_to_string)?
    {
        if workspace_id_from_repo_marker(requested) != Some(workspace_id) {
            return Err("远端终端目录不属于当前项目".to_string());
        }
        let registered = remote_git::worktrees(connection, root)
            .await
            .map_err(err_to_string)?
            .into_iter()
            .any(|item| item.path == worktree.root_path);
        if !registered {
            return Err("远端终端目录不属于当前项目登记的工作树".to_string());
        }
        let requested = match worktree.relative_path {
            Some(relative) => format!("{}/{}", worktree.root_path.trim_end_matches('/'), relative),
            None => worktree.root_path.clone(),
        };
        (requested, worktree.root_path)
    } else if requested == remote_repo_marker(workspace_id) {
        (root.to_string(), root.to_string())
    } else if requested.starts_with(crate::ssh::runtime::REMOTE_REPO_PREFIX) {
        return Err("远端终端目录不属于当前项目".to_string());
    } else {
        (requested.to_string(), root.to_string())
    };
    let candidate = if requested.starts_with('/') {
        quote_posix(&requested)
    } else {
        validate_remote_relative_path(&requested, true).map_err(err_to_string)?;
        format!(
            "{}/{}",
            quote_posix(root.trim_end_matches('/')),
            quote_posix(&requested)
        )
    };
    let command = format!(
        "root={}; root_real=$(realpath -- \"$root\") || exit 21; candidate=$(realpath -- {}) || exit 22; case \"$candidate\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; [ -d \"$candidate\" ] || exit 24; printf '__PANES_CWD__%s\\n' \"$candidate\"",
        quote_posix(&allowed_root),
        candidate,
    );
    let output = gateway::run_command(connection, &command)
        .await
        .map_err(err_to_string)?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("__PANES_CWD__"))
        .filter(|path| path.starts_with('/'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| "远端终端目录解析失败".to_string())
}

fn canonicalize_existing_dir(path: &str, label: &str) -> Result<PathBuf, String> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(format!("{label} does not exist: {path}"));
    }

    path_utils::canonicalize_path(dir)
        .map_err(|error| format!("failed to resolve {label}: {error}"))
}

#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
    data: String,
) -> Result<(), String> {
    state
        .terminals
        .write(&workspace_id, &session_id, data)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_write_bytes(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    state
        .terminals
        .write_bytes(&workspace_id, &session_id, data)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
    cols: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
) -> Result<(), String> {
    state
        .terminals
        .resize(
            &workspace_id,
            &session_id,
            cols.max(1),
            rows.max(1),
            pixel_width,
            pixel_height,
        )
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_close_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
) -> Result<(), String> {
    state
        .terminals
        .close_session(app.clone(), &workspace_id, &session_id)
        .await
        .map_err(err_to_string)?;
    state
        .notifications
        .clear_for_session(&app, &workspace_id, &session_id)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_close_workspace_sessions(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    state
        .terminals
        .close_workspace(app.clone(), &workspace_id)
        .await
        .map_err(err_to_string)?;
    state
        .notifications
        .clear_for_workspace(&app, &workspace_id)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_list_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<TerminalSessionDto>, String> {
    Ok(state.terminals.list_sessions(&workspace_id).await)
}

#[tauri::command]
pub async fn terminal_get_renderer_diagnostics(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
) -> Result<TerminalRendererDiagnosticsDto, String> {
    state
        .terminals
        .renderer_diagnostics(&workspace_id, &session_id)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_resume_session(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
    from_seq: Option<u64>,
) -> Result<TerminalResumeSessionDto, String> {
    state
        .terminals
        .resume_session(&workspace_id, &session_id, from_seq)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_drain_output(
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: String,
    from_seq: Option<u64>,
    target_bytes: u64,
) -> Result<TerminalResumeSessionDto, String> {
    state
        .terminals
        .drain_output(
            &workspace_id,
            &session_id,
            from_seq,
            target_bytes.clamp(1, 1024 * 1024) as usize,
        )
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn terminal_list_notifications(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<TerminalNotificationDto>, String> {
    Ok(state.notifications.list_for_workspace(&workspace_id).await)
}

#[tauri::command]
pub async fn terminal_clear_notification(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    session_id: Option<String>,
) -> Result<(), String> {
    match session_id.as_deref() {
        Some(session_id) => {
            state
                .notifications
                .clear_for_session(&app, &workspace_id, session_id)
                .await;
        }
        None => {
            state
                .notifications
                .clear_for_workspace(&app, &workspace_id)
                .await;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn terminal_set_notification_focus(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: Option<String>,
    session_id: Option<String>,
    window_focused: bool,
) -> Result<(), String> {
    state
        .notifications
        .set_focus(window_focused, workspace_id.clone(), session_id.clone())
        .await;

    if window_focused {
        if let (Some(workspace_id), Some(session_id)) =
            (workspace_id.as_deref(), session_id.as_deref())
        {
            state
                .notifications
                .clear_for_session(&app, workspace_id, session_id)
                .await;
        }
    }

    Ok(())
}

async fn workspace_root_path(state: &AppState, workspace_id: &str) -> Result<String, String> {
    run_db(state.db.clone(), {
        let workspace_id = workspace_id.to_string();
        move |db| {
            db::workspaces::list_workspaces(db)?
                .into_iter()
                .find(|workspace| workspace.id == workspace_id)
                .map(|workspace| workspace.root_path)
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
