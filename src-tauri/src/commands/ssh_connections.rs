use tauri::State;

use crate::{
    models::{
        SshConfigHostDto, SshConnectionDto, SshConnectionImportResultDto, SshConnectionInput,
        SshConnectionTestDto,
    },
    services::ssh_connection_service::SshConnectionService,
    state::AppState,
};

/// 查询所有未删除的 SSH 连接配置。
#[tauri::command]
pub async fn list_ssh_connections(
    state: State<'_, AppState>,
) -> Result<Vec<SshConnectionDto>, String> {
    // 将 IPC 请求转交给 SSH 连接业务服务并转换前端错误类型。
    SshConnectionService::new(state.inner().clone())
        .list_ssh_connections()
        .await
        .map_err(|error| error.to_string())
}

/// 查询所有已软删除的 SSH 连接配置。
#[tauri::command]
pub async fn list_deleted_ssh_connections(
    state: State<'_, AppState>,
) -> Result<Vec<SshConnectionDto>, String> {
    // 将 IPC 请求转交给 SSH 连接业务服务并转换前端错误类型。
    SshConnectionService::new(state.inner().clone())
        .list_deleted_ssh_connections()
        .await
        .map_err(|error| error.to_string())
}

/// 扫描本机 SSH 配置并返回导入状态标记。
#[tauri::command]
pub async fn scan_ssh_config_hosts(
    state: State<'_, AppState>,
) -> Result<Vec<SshConfigHostDto>, String> {
    // 将 SSH 配置扫描业务交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .scan_ssh_config_hosts()
        .await
        .map_err(|error| error.to_string())
}

/// 按用户选择的 SSH 配置 alias 逐条导入连接。
#[tauri::command]
pub async fn import_ssh_config_hosts(
    state: State<'_, AppState>,
    aliases: Vec<String>,
) -> Result<Vec<SshConnectionImportResultDto>, String> {
    // 将逐条导入、检测和资源注册业务交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .import_ssh_config_hosts(aliases)
        .await
        .map_err(|error| error.to_string())
}

/// 创建手动配置的 SSH 连接。
#[tauri::command]
pub async fn create_manual_ssh_connection(
    state: State<'_, AppState>,
    input: SshConnectionInput,
) -> Result<SshConnectionDto, String> {
    // 将输入校验、连接保存和连接检测交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .create_manual_ssh_connection(input)
        .await
        .map_err(|error| error.to_string())
}

/// 更新已保存的 SSH 连接参数。
#[tauri::command]
pub async fn update_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
    input: SshConnectionInput,
) -> Result<SshConnectionDto, String> {
    // 将连接更新和监控结束路径交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .update_ssh_connection(connection_id, input)
        .await
        .map_err(|error| error.to_string())
}

/// 检测指定 SSH 连接并保存当前版本的检测状态。
#[tauri::command]
pub async fn test_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionTestDto, String> {
    // 将 SSH 连通性检测交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .test_ssh_connection(connection_id)
        .await
        .map_err(|error| error.to_string())
}

/// 保存 SSH 连接启用状态并处理对应监控或关联资源。
#[tauri::command]
pub async fn set_ssh_connection_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    enabled: bool,
) -> Result<SshConnectionDto, String> {
    // 将状态持久化和副作用清理交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .set_ssh_connection_enabled(app, connection_id, enabled)
        .await
        .map_err(|error| error.to_string())
}

/// 软删除 SSH 连接并关闭关联 CLI 与工作区资源。
#[tauri::command]
pub async fn delete_ssh_connection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<(), String> {
    // 将连接删除与资源关闭交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .delete_ssh_connection(app, connection_id)
        .await
        .map_err(|error| error.to_string())
}

/// 按原连接 ID 恢复软删除的 SSH 连接。
#[tauri::command]
pub async fn restore_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionDto, String> {
    // 将连接恢复与启用状态对应的监控启动交给公共服务处理。
    SshConnectionService::new(state.inner().clone())
        .restore_ssh_connection(connection_id)
        .await
        .map_err(|error| error.to_string())
}
