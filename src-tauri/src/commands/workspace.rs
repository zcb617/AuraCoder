use tauri::State;

use crate::{
    db, fs_ops,
    git::{multi_repo, repo},
    models::{
        FileTreeEntryDto, FileTreePageDto, RepoDto, SshConnectionTestDto, SshRemoteDirectoryDto,
        TrustLevelDto, WorkspaceDto, WorkspaceGitSelectionStatusDto,
    },
    ssh::{
        gateway, remote_fs, remote_git,
        runtime::{remote_repo_marker, resolve_workspace_target},
    },
    state::AppState,
    workspace_startup::{
        normalize_workspace_startup_preset as normalize_preset,
        parse_persisted_workspace_startup_preset_json, parse_workspace_startup_preset_raw,
        resolve_workspace_path, serialize_workspace_startup_preset as serialize_preset,
        WorkspaceStartupPreset, WorkspaceStartupPresetFormat,
    },
};

const MIN_SCAN_DEPTH: i64 = 0;
const MAX_SCAN_DEPTH: i64 = 12;

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
pub async fn open_workspace(
    state: State<'_, AppState>,
    path: String,
    scan_depth: Option<i64>,
) -> Result<WorkspaceDto, String> {
    let scan_depth = normalize_scan_depth(scan_depth);
    run_db(state.db.clone(), move |db| {
        let workspace = db::workspaces::upsert_workspace(db, &path, scan_depth)?;
        let repos =
            multi_repo::scan_git_repositories(&workspace.root_path, workspace.scan_depth as usize)?;
        let repo_paths = repos
            .iter()
            .map(|repo| repo.path.clone())
            .collect::<Vec<_>>();
        db::repos::reconcile_workspace_repos(db, &workspace.id, &repo_paths)?;
        let selection_configured =
            db::workspaces::is_git_repo_selection_configured(db, &workspace.id)?;

        for repo in repos {
            let _ = db::repos::upsert_repo(
                db,
                &workspace.id,
                &repo.name,
                &repo.path,
                &repo.default_branch,
                !selection_configured,
            );
        }

        Ok(workspace)
    })
    .await
}

#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> Result<Vec<WorkspaceDto>, String> {
    run_db(state.db.clone(), db::workspaces::list_workspaces).await
}

#[tauri::command]
pub async fn list_archived_workspaces(
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceDto>, String> {
    run_db(state.db.clone(), db::workspaces::list_archived_workspaces).await
}

#[tauri::command]
pub async fn get_ssh_connection_home(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionTestDto, String> {
    let record = load_usable_ssh_connection(&state.db, &connection_id).await?;
    let result = gateway::test(&record).await;
    let id = result.connection_id.clone();
    let version = record.dto.updated_at.clone();
    let ok = result.ok;
    let error = result.error.clone();
    run_db(state.db.clone(), move |db| {
        db::ssh_connections::record_test(db, &id, &version, ok, error.as_deref())
    })
    .await?;
    if !result.ok {
        return Err(result
            .error
            .unwrap_or_else(|| "无法连接 SSH 远端主机".to_string()));
    }
    if result
        .home
        .as_deref()
        .is_none_or(|home| !is_absolute_remote_path(home))
    {
        return Err("远端未返回有效的 HOME 目录".to_string());
    }
    Ok(result)
}

#[tauri::command]
pub async fn list_ssh_directories(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
) -> Result<Vec<SshRemoteDirectoryDto>, String> {
    let path = path.trim().to_string();
    validate_remote_path(&path)?;
    let record = load_usable_ssh_connection(&state.db, &connection_id).await?;
    let quoted_path = quote_posix(&path);
    let command = format!(
        "base={quoted_path}; [ -d \"$base\" ] && [ -r \"$base\" ] && [ -x \"$base\" ] || exit 21; find \"$base\" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | while IFS= read -r child; do readable=0; enterable=0; [ -r \"$child\" ] && readable=1; [ -x \"$child\" ] && enterable=1; printf '__PANES_DIR__%s\\t%s\\t%s\\n' \"$child\" \"$readable\" \"$enterable\"; done"
    );
    let output = gateway::run_command(&record, &command)
        .await
        .map_err(|error| error.to_string())?;
    parse_remote_directories(&output)
}

#[tauri::command]
pub async fn resolve_ssh_directory(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    parent: Option<bool>,
) -> Result<SshRemoteDirectoryDto, String> {
    let path = path.trim().to_string();
    validate_remote_path(&path)?;
    let record = load_usable_ssh_connection(&state.db, &connection_id).await?;
    let quoted_path = quote_posix(&path);
    let command = if parent.unwrap_or(false) {
        format!(
            "candidate=$(dirname -- {quoted_path} 2>/dev/null) || exit 21; resolved=$(realpath -- \"$candidate\" 2>/dev/null) || exit 21; [ -d \"$resolved\" ] && [ -r \"$resolved\" ] && [ -x \"$resolved\" ] || exit 22; readable=0; enterable=0; [ -r \"$resolved\" ] && readable=1; [ -x \"$resolved\" ] && enterable=1; printf '__PANES_DIR__%s\\t%s\\t%s\\n' \"$resolved\" \"$readable\" \"$enterable\""
        )
    } else {
        format!(
            "resolved=$(realpath -- {quoted_path} 2>/dev/null) || exit 21; [ -d \"$resolved\" ] && [ -r \"$resolved\" ] && [ -x \"$resolved\" ] || exit 22; readable=0; enterable=0; [ -r \"$resolved\" ] && readable=1; [ -x \"$resolved\" ] && enterable=1; printf '__PANES_DIR__%s\\t%s\\t%s\\n' \"$resolved\" \"$readable\" \"$enterable\""
        )
    };
    let output = gateway::run_command(&record, &command)
        .await
        .map_err(|error| error.to_string())?;
    parse_remote_directories(&output)?
        .into_iter()
        .next()
        .ok_or_else(|| "远端目录解析失败".to_string())
}

#[tauri::command]
pub async fn create_ssh_workspace(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    name: String,
    root_path: String,
    scan_depth: Option<i64>,
) -> Result<WorkspaceDto, String> {
    let root_path = root_path.trim().to_string();
    validate_remote_path(&root_path)?;
    let record = load_usable_ssh_connection(&state.db, &connection_id).await?;
    let quoted_path = quote_posix(&root_path);
    let command = format!(
        "resolved=$(realpath -- {quoted_path} 2>/dev/null) || exit 21; [ -d \"$resolved\" ] && [ -r \"$resolved\" ] && [ -x \"$resolved\" ] || exit 22; printf '__PANES_DIR__%s\\t1\\t1\\n' \"$resolved\""
    );
    let output = gateway::run_command(&record, &command)
        .await
        .map_err(|error| error.to_string())?;
    let root_path = parse_remote_directories(&output)?
        .into_iter()
        .next()
        .map(|directory| directory.path)
        .ok_or_else(|| "远端目录解析失败".to_string())?;
    let workspace = run_db(state.db.clone(), move |db| {
        db::workspaces::create_ssh_workspace(
            db,
            &connection_id,
            &name,
            &root_path,
            normalize_scan_depth(scan_depth),
        )
    })
    .await?;

    let sync_app = app.clone();
    let sync_db = state.db.clone();
    let workspace_id = workspace.id.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            crate::remote_project_session_refresh_service::refresh_ssh_remote_project_sessions(
                &sync_app,
                sync_db.into(),
                &workspace_id,
            )
            .await
        {
            log::warn!(
                "新增 SSH 远端项目后刷新会话失败: workspace_id={} error={error:#}",
                workspace_id
            );
        }
    });

    Ok(workspace)
}

#[tauri::command]
pub async fn get_repos(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<RepoDto>, String> {
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        let connection = target.remote_connection().map_err(err_to_string)?;
        let Some(info) = remote_git::discover(connection, &target.workspace.root_path)
            .await
            .map_err(err_to_string)?
        else {
            return Ok(Vec::new());
        };
        return Ok(vec![RepoDto {
            id: format!("remote-repo-{}", target.workspace.id),
            workspace_id: target.workspace.id.clone(),
            name: info.name,
            path: remote_repo_marker(&target.workspace.id),
            default_branch: info.default_branch,
            is_active: true,
            trust_level: TrustLevelDto::Standard,
        }]);
    }
    run_db(state.db.clone(), move |db| {
        db::repos::get_repos(db, &workspace_id)
    })
    .await
}

#[tauri::command]
pub async fn set_repo_trust_level(
    state: State<'_, AppState>,
    repo_id: String,
    trust_level: TrustLevelDto,
) -> Result<(), String> {
    if repo_id.starts_with("remote-repo-") {
        return Ok(());
    }
    run_db(state.db.clone(), move |db| {
        db::repos::set_repo_trust_level(db, &repo_id, trust_level)
    })
    .await
}

#[tauri::command]
pub async fn set_repo_git_active(
    state: State<'_, AppState>,
    repo_id: String,
    is_active: bool,
) -> Result<(), String> {
    if repo_id.starts_with("remote-repo-") {
        return Ok(());
    }
    run_db(state.db.clone(), move |db| {
        db::repos::set_repo_active(db, &repo_id, is_active)?;

        if let Some(repo) = db::repos::find_repo_by_id(db, &repo_id)? {
            db::workspaces::set_git_repo_selection_configured(db, &repo.workspace_id, true)?;
        }

        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_workspace_git_active_repos(
    state: State<'_, AppState>,
    workspace_id: String,
    repo_ids: Vec<String>,
) -> Result<(), String> {
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        return Ok(());
    }
    run_db(state.db.clone(), move |db| {
        db::repos::set_workspace_active_repos(db, &workspace_id, &repo_ids)?;
        db::workspaces::set_git_repo_selection_configured(db, &workspace_id, true)?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn has_workspace_git_selection(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<WorkspaceGitSelectionStatusDto, String> {
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        return Ok(WorkspaceGitSelectionStatusDto { configured: true });
    }
    let configured = run_db(state.db.clone(), move |db| {
        db::workspaces::is_git_repo_selection_configured(db, &workspace_id)
    })
    .await?;
    Ok(WorkspaceGitSelectionStatusDto { configured })
}

#[tauri::command]
pub async fn delete_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    run_db(state.db.clone(), move |db| {
        db::workspaces::delete_workspace(db, &workspace_id)
    })
    .await
}

#[tauri::command]
pub async fn archive_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    run_db(state.db.clone(), move |db| {
        db::workspaces::archive_workspace(db, &workspace_id)
    })
    .await
}

#[tauri::command]
pub async fn restore_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<WorkspaceDto, String> {
    run_db(state.db.clone(), move |db| {
        db::workspaces::restore_workspace(db, &workspace_id)
    })
    .await
}

#[tauri::command]
pub async fn get_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Option<WorkspaceStartupPreset>, String> {
    run_db(state.db.clone(), move |db| {
        load_workspace(db, &workspace_id)?;
        db::workspaces::get_workspace_startup_preset_json(db, &workspace_id)?
            .as_deref()
            .map(parse_persisted_workspace_startup_preset_json)
            .transpose()
    })
    .await
}

#[tauri::command]
pub async fn normalize_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
) -> Result<WorkspaceStartupPreset, String> {
    run_db(state.db.clone(), move |db| {
        let workspace = load_workspace(db, &workspace_id)?;
        normalize_preset_for_workspace(&workspace, preset)
    })
    .await
}

#[tauri::command]
pub async fn serialize_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
    format: WorkspaceStartupPresetFormat,
) -> Result<String, String> {
    run_db(state.db.clone(), move |db| {
        let workspace = load_workspace(db, &workspace_id)?;
        let normalized = normalize_preset_for_workspace(&workspace, preset)?;
        serialize_preset(&normalized, format)
    })
    .await
}

#[tauri::command]
pub async fn normalize_workspace_startup_preset_raw(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
    raw_text: String,
) -> Result<WorkspaceStartupPreset, String> {
    run_db(state.db.clone(), move |db| {
        let workspace = load_workspace(db, &workspace_id)?;
        let parsed = parse_workspace_startup_preset_raw(format, &raw_text)?;
        normalize_preset_for_workspace(&workspace, parsed)
    })
    .await
}

#[tauri::command]
pub async fn set_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
) -> Result<WorkspaceStartupPreset, String> {
    run_db(state.db.clone(), move |db| {
        let workspace = load_workspace(db, &workspace_id)?;
        let normalized = normalize_preset_for_workspace(&workspace, preset)?;
        let raw_json = serde_json::to_string(&normalized)
            .map_err(|error| anyhow::anyhow!("failed to serialize startup preset JSON: {error}"))?;
        db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, Some(&raw_json))?;
        Ok(normalized)
    })
    .await
}

#[tauri::command]
pub async fn set_workspace_startup_preset_raw(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
    raw_text: String,
) -> Result<WorkspaceStartupPreset, String> {
    run_db(state.db.clone(), move |db| {
        let workspace = load_workspace(db, &workspace_id)?;
        let parsed = parse_workspace_startup_preset_raw(format, &raw_text)?;
        let normalized = normalize_preset_for_workspace(&workspace, parsed)?;
        let raw_json = serde_json::to_string(&normalized)
            .map_err(|error| anyhow::anyhow!("failed to serialize startup preset JSON: {error}"))?;
        db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, Some(&raw_json))?;
        Ok(normalized)
    })
    .await
}

#[tauri::command]
pub async fn clear_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    run_db(state.db.clone(), move |db| {
        db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, None)
    })
    .await
}

#[tauri::command]
pub async fn export_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
) -> Result<String, String> {
    run_db(state.db.clone(), move |db| {
        load_workspace(db, &workspace_id)?;
        let raw_json = db::workspaces::get_workspace_startup_preset_json(db, &workspace_id)?
            .ok_or_else(|| anyhow::anyhow!("workspace startup preset is not configured"))?;
        let preset = parse_persisted_workspace_startup_preset_json(&raw_json)?;
        serialize_preset(&preset, format)
    })
    .await
}

#[tauri::command]
pub async fn list_workspace_dirs(
    state: State<'_, AppState>,
    workspace_id: String,
    dir_path: Option<String>,
) -> Result<Vec<FileTreeEntryDto>, String> {
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        let connection = target.remote_connection().map_err(err_to_string)?;
        let entries = remote_fs::list_dir(
            connection,
            &target.workspace.root_path,
            dir_path.as_deref().unwrap_or(""),
        )
        .await
        .map_err(err_to_string)?;
        return Ok(entries.into_iter().filter(|entry| entry.is_dir).collect());
    }
    let root_path = target.workspace.root_path;
    run_db(state.db.clone(), move |_db| {
        let mut entries = fs_ops::list_dir(&root_path, dir_path.as_deref().unwrap_or(""))?;
        entries.retain(|entry| entry.is_dir);
        Ok(entries)
    })
    .await
}

#[tauri::command]
pub async fn get_workspace_file_tree_page(
    state: State<'_, AppState>,
    workspace_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
    refresh: Option<bool>,
) -> Result<FileTreePageDto, String> {
    let cache = state.file_tree_cache.clone();
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        let connection = target.remote_connection().map_err(err_to_string)?;
        return remote_fs::file_tree_page(
            connection,
            &target.workspace.root_path,
            offset.unwrap_or(0),
            limit.unwrap_or(2000),
        )
        .await
        .map_err(err_to_string);
    }
    let root_path = target.workspace.root_path;
    run_db(state.db.clone(), move |_db| {
        if refresh.unwrap_or(false) {
            cache.invalidate_workspace(&root_path);
        }
        repo::get_workspace_file_tree_page(
            &root_path,
            offset.unwrap_or(0),
            limit.unwrap_or(2000),
            &cache,
        )
    })
    .await
}

#[tauri::command]
pub async fn search_workspace_files(
    state: State<'_, AppState>,
    workspace_id: String,
    query: String,
    offset: Option<usize>,
    limit: Option<usize>,
    refresh: Option<bool>,
) -> Result<FileTreePageDto, String> {
    let cache = state.file_tree_cache.clone();
    let target = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| resolve_workspace_target(db, &workspace_id)
    })
    .await?;
    if target.is_remote() {
        let connection = target.remote_connection().map_err(err_to_string)?;
        return remote_fs::search_files(
            connection,
            &target.workspace.root_path,
            &query,
            offset.unwrap_or(0),
            limit.unwrap_or(80),
        )
        .await
        .map_err(err_to_string);
    }
    let root_path = target.workspace.root_path;
    run_db(state.db.clone(), move |_db| {
        if refresh.unwrap_or(false) {
            cache.invalidate_workspace(&root_path);
        }
        repo::search_workspace_files(
            &root_path,
            &query,
            offset.unwrap_or(0),
            limit.unwrap_or(80),
            &cache,
        )
    })
    .await
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn load_workspace(db: &crate::db::Database, workspace_id: &str) -> anyhow::Result<WorkspaceDto> {
    db::workspaces::find_workspace_by_id(db, workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
}

fn normalize_preset_for_workspace(
    workspace: &WorkspaceDto,
    mut preset: WorkspaceStartupPreset,
) -> anyhow::Result<WorkspaceStartupPreset> {
    if workspace.location_kind == "ssh" {
        if let Some(terminal) = preset.terminal.as_mut() {
            for group in &mut terminal.groups {
                if let Some(worktree) = group.worktree.as_mut() {
                    if let Some(base_dir) = worktree.base_dir.as_mut() {
                        *base_dir = base_dir.trim().trim_end_matches('/').to_string();
                        anyhow::ensure!(!base_dir.is_empty(), "远端工作树目录不能为空");
                        anyhow::ensure!(
                            !base_dir.contains('\0')
                                && !base_dir.contains('\n')
                                && !base_dir.contains('\r')
                                && !base_dir.starts_with("ssh://"),
                            "远端工作树目录无效"
                        );
                        if !base_dir.starts_with('/') {
                            crate::ssh::runtime::validate_remote_relative_path(base_dir, false)?;
                        }
                    }
                }
                for session in &mut group.sessions {
                    if matches!(
                        session.cwd_base,
                        Some(crate::workspace_startup::WorkspacePathBase::Absolute)
                    ) {
                        anyhow::bail!("远端启动终端不能使用未登记的绝对目录");
                    }
                }
            }
        }
        let encoded = serde_json::to_string(&preset)?;
        anyhow::ensure!(
            !encoded.contains('\0') && !encoded.contains('\n') && !encoded.contains('\r'),
            "远端启动配置包含非法字符"
        );
        return Ok(preset);
    }
    let workspace_root = resolve_workspace_path(&workspace.root_path)?;
    normalize_preset(preset, &workspace_root)
}

fn normalize_scan_depth(value: Option<i64>) -> Option<i64> {
    value.map(|depth| depth.clamp(MIN_SCAN_DEPTH, MAX_SCAN_DEPTH))
}

async fn load_usable_ssh_connection(
    db: &crate::db::Database,
    connection_id: &str,
) -> Result<db::ssh_connections::SshConnectionRecord, String> {
    let connection_id = connection_id.to_string();
    let record = run_db(db.clone(), move |db| {
        db::ssh_connections::find(db, &connection_id)
    })
    .await?;
    let record = record.ok_or_else(|| "SSH 连接不存在".to_string())?;
    if record.dto.deleted_at.is_some() {
        return Err("SSH 连接已删除，请先恢复连接".to_string());
    }
    if !record.dto.enabled {
        return Err("SSH 连接已禁用".to_string());
    }
    Ok(record)
}

fn is_absolute_remote_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('\0')
        && !path.contains('\n')
        && !path.contains('\r')
        && !path.contains('\t')
}

fn validate_remote_path(path: &str) -> Result<(), String> {
    if !is_absolute_remote_path(path.trim()) {
        return Err("远端目录必须是绝对路径".to_string());
    }
    Ok(())
}

fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn parse_remote_directories(output: &str) -> Result<Vec<SshRemoteDirectoryDto>, String> {
    let mut directories = Vec::new();
    for line in output.lines() {
        let Some(payload) = line.strip_prefix("__PANES_DIR__") else {
            continue;
        };
        let mut fields = payload.splitn(3, '\t');
        let Some(path) = fields.next() else {
            continue;
        };
        let readable = fields.next() == Some("1");
        let enterable = fields.next() == Some("1");
        if !is_absolute_remote_path(path) {
            continue;
        }
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("/")
            .to_string();
        directories.push(SshRemoteDirectoryDto {
            path: path.to_string(),
            name,
            readable,
            enterable,
        });
    }
    directories.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(directories)
}
