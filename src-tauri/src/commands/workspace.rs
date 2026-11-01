use tauri::State;

use crate::{
    models::{
        FileTreeEntryDto, FileTreePageDto, SshConnectionTestDto, SshRemoteDirectoryDto,
        TrustLevelDto, WorkspaceDto,
    },
    state::AppState,
    workspace_startup::{WorkspaceStartupPreset, WorkspaceStartupPresetFormat},
};

/// 按本机目录打开或登记工作区。
#[tauri::command]
pub async fn open_workspace(
    state: State<'_, AppState>,
    path: String,
) -> Result<WorkspaceDto, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .open_workspace(path)
        .await
        .map_err(|error| error.to_string())
}

/// 查询所有未归档的工作区。
#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> Result<Vec<WorkspaceDto>, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .list_workspaces()
        .await
        .map_err(|error| error.to_string())
}

/*
/// 启动时按项目同步本地 Codex、OpenCode 与 Claude 历史会话，并返回各 CLI 同步结果。
#[tauri::command]
pub async fn refresh_local_project_sessions(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<crate::remote_project_session_refresh_service::LocalProjectSessionRefreshReport, String>
{
    crate::remote_project_session_refresh_service::refresh_local_project_sessions(
        &app,
        state.db.clone().into(),
        &workspace_id,
    )
    .await
    .map_err(|error| {
        log::warn!(
            "刷新本地项目会话失败: workspace_id={} error={error:#}",
            workspace_id
        );
        format!("刷新本地项目会话失败: {error:#}")
    })
}
*/

/// 同步指定项目的 CLI 会话。
#[tauri::command]
pub async fn refresh_project_sessions(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<crate::remote_project_session_refresh_service::ProjectSessionRefreshReport, String> {
    crate::remote_project_session_refresh_service::refresh_project_sessions(
        &app,
        state.db.clone().into(),
        &workspace_id,
    )
    .await
    .map_err(|error| {
        log::error!("同步项目会话失败: workspace_id={} error={error:#}", workspace_id);
        format!("同步项目会话失败: {error:#}")
    })
}

/// 获取指定项目的 CLI 扩展。
#[tauri::command]
pub async fn refresh_project_catalogs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    crate::extensions::refresh::refresh_project_catalogs(&app, state.inner(), &workspace_id)
        .await
        .map_err(|error| {
            log::error!("获取项目扩展失败: workspace_id={} error={error:#}", workspace_id);
            format!("获取项目扩展失败: {error:#}")
        })
}

/// 查询所有已归档的工作区。
#[tauri::command]
pub async fn list_archived_workspaces(
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceDto>, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .list_archived_workspaces()
        .await
        .map_err(|error| error.to_string())
}

/// 检测 SSH 连接并获取远端 HOME 目录信息。
#[tauri::command]
pub async fn get_ssh_connection_home(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionTestDto, String> {
    crate::services::ssh_connection_service::SshConnectionService::new(state.inner().clone())
        .get_ssh_connection_home(connection_id)
        .await
        .map_err(|error| error.to_string())
}

/// 列出 SSH 连接指定目录下的直接子目录。
#[tauri::command]
pub async fn list_ssh_directories(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
) -> Result<Vec<SshRemoteDirectoryDto>, String> {
    crate::services::ssh_connection_service::SshConnectionService::new(state.inner().clone())
        .list_ssh_directories(connection_id, path)
        .await
        .map_err(|error| error.to_string())
}

/// 解析 SSH 连接下的目录或其可访问父目录。
#[tauri::command]
pub async fn resolve_ssh_directory(
    state: State<'_, AppState>,
    connection_id: String,
    path: String,
    parent: Option<bool>,
) -> Result<SshRemoteDirectoryDto, String> {
    crate::services::ssh_connection_service::SshConnectionService::new(state.inner().clone())
        .resolve_ssh_directory(connection_id, path, parent)
        .await
        .map_err(|error| error.to_string())
}

/// 创建 SSH 工作区。
#[tauri::command]
pub async fn create_ssh_workspace(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    name: String,
    root_path: String,
) -> Result<WorkspaceDto, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .create_ssh_workspace(app, connection_id, name, root_path)
        .await
        .map_err(|error| error.to_string())
}

/// 更新工作区的项目级信任等级。
#[tauri::command]
pub async fn set_workspace_trust_level(
    state: State<'_, AppState>,
    workspace_id: String,
    trust_level: TrustLevelDto,
) -> Result<(), String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .set_workspace_trust_level(workspace_id, trust_level)
        .await
        .map_err(|error| error.to_string())
}

/// 按现有规则物理删除工作区及其级联数据。
#[tauri::command]
pub async fn delete_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .delete_workspace(workspace_id)
        .await
        .map_err(|error| error.to_string())
}

/// 将工作区标记为已归档。
#[tauri::command]
pub async fn archive_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .archive_workspace(workspace_id)
        .await
        .map_err(|error| error.to_string())
}

/// 恢复已归档的工作区。
#[tauri::command]
pub async fn restore_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<WorkspaceDto, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .restore_workspace(workspace_id)
        .await
        .map_err(|error| error.to_string())
}

/// 读取工作区已保存的启动预设。
#[tauri::command]
pub async fn get_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Option<WorkspaceStartupPreset>, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .get_workspace_startup_preset(workspace_id)
        .await
        .map_err(|error| error.to_string())
}

/// 按所属工作区规则规范化启动预设。
#[tauri::command]
pub async fn normalize_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
) -> Result<WorkspaceStartupPreset, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .normalize_workspace_startup_preset(workspace_id, preset)
        .await
        .map_err(|error| error.to_string())
}

/// 按工作区规则规范化预设并转换为 JSON 或 TOML 文本。
#[tauri::command]
pub async fn serialize_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
    format: WorkspaceStartupPresetFormat,
) -> Result<String, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .serialize_workspace_startup_preset(workspace_id, preset, format)
        .await
        .map_err(|error| error.to_string())
}

/// 将原始 JSON 或 TOML 启动预设解析并规范化。
#[tauri::command]
pub async fn normalize_workspace_startup_preset_raw(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
    raw_text: String,
) -> Result<WorkspaceStartupPreset, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .normalize_workspace_startup_preset_raw(workspace_id, format, raw_text)
        .await
        .map_err(|error| error.to_string())
}

/// 规范化并保存工作区启动预设。
#[tauri::command]
pub async fn set_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    preset: WorkspaceStartupPreset,
) -> Result<WorkspaceStartupPreset, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .set_workspace_startup_preset(workspace_id, preset)
        .await
        .map_err(|error| error.to_string())
}

/// 将原始 JSON 或 TOML 启动预设解析、规范化后保存。
#[tauri::command]
pub async fn set_workspace_startup_preset_raw(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
    raw_text: String,
) -> Result<WorkspaceStartupPreset, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .set_workspace_startup_preset_raw(workspace_id, format, raw_text)
        .await
        .map_err(|error| error.to_string())
}

/// 清除工作区已保存的启动预设。
#[tauri::command]
pub async fn clear_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .clear_workspace_startup_preset(workspace_id)
        .await
        .map_err(|error| error.to_string())
}

/// 导出已保存的工作区启动预设。
#[tauri::command]
pub async fn export_workspace_startup_preset(
    state: State<'_, AppState>,
    workspace_id: String,
    format: WorkspaceStartupPresetFormat,
) -> Result<String, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .export_workspace_startup_preset(workspace_id, format)
        .await
        .map_err(|error| error.to_string())
}

/// 查询本机或 SSH 工作区中的子目录。
#[tauri::command]
pub async fn list_workspace_dirs(
    state: State<'_, AppState>,
    workspace_id: String,
    dir_path: Option<String>,
) -> Result<Vec<FileTreeEntryDto>, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .list_workspace_dirs(workspace_id, dir_path)
        .await
        .map_err(|error| error.to_string())
}

/// 分页读取本机或 SSH 工作区的文件树。
#[tauri::command]
pub async fn get_workspace_file_tree_page(
    state: State<'_, AppState>,
    workspace_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
    refresh: Option<bool>,
) -> Result<FileTreePageDto, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .get_workspace_file_tree_page(workspace_id, offset, limit, refresh)
        .await
        .map_err(|error| error.to_string())
}

/// 按查询词分页搜索本机或 SSH 工作区文件。
#[tauri::command]
pub async fn search_workspace_files(
    state: State<'_, AppState>,
    workspace_id: String,
    query: String,
    offset: Option<usize>,
    limit: Option<usize>,
    refresh: Option<bool>,
) -> Result<FileTreePageDto, String> {
    crate::services::workspace_service::WorkspaceService::new(state.inner().clone())
        .search_workspace_files(workspace_id, query, offset, limit, refresh)
        .await
        .map_err(|error| error.to_string())
}
