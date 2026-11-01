use serde_json::Value;
use tauri::{AppHandle, State};
use crate::{cli_tools::factory::CliToolFactory, models::{CodexRemoteThreadPageDto, OpenCodeRemoteSessionPageDto, PermissionComponentJson, ThreadDto, ThreadUpdateDto}, services::thread_service::ThreadService, state::AppState};

fn err_to_string(error: impl std::fmt::Display) -> String { format!("{error:#}") }

/// 通过线程 Service 读取工作区活动线程及其统一权限对象 A。
#[tauri::command]
pub async fn list_threads(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<ThreadDto>, String> {
    ThreadService::new(state.inner().clone())
        .list_threads(&workspace_id)
        .await
        .map_err(err_to_string)
}

/// 通过线程 Service 读取工作区归档线程及其统一权限对象 A。
#[tauri::command]
pub async fn list_archived_threads(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<ThreadDto>, String> {
    ThreadService::new(state.inner().clone())
        .list_archived_threads(&workspace_id)
        .await
        .map_err(err_to_string)
}

/// 通过线程 Service 保存局部字段和统一权限对象 A。
#[tauri::command]
pub async fn update_thread(
    state: State<'_, AppState>,
    update: ThreadUpdateDto,
) -> Result<ThreadDto, String> {
    ThreadService::new(state.inner().clone())
        .update_thread(update)
        .await
        .map_err(err_to_string)
}

/// 持久化会话底部 6 项运行时选择。
///
/// 前端传什么就写什么，不做回退、不与 engine_metadata_json 合并，
/// 让底部状态以独立字段为唯一数据源。
#[tauri::command]
pub async fn update_thread_runtime_selection(
    state: State<'_, AppState>,
    thread_id: String,
    engine_id: String,
    model_id: String,
    plan_mode: Option<bool>,
    send_method: Option<String>,
    reasoning_effort: Option<String>,
    permission_mode: Option<PermissionComponentJson>,
) -> Result<ThreadDto, String> {
    ThreadService::new(state.inner().clone())
        .update_thread(ThreadUpdateDto {
            id: thread_id,
            engine_id: Some(engine_id),
            model_id: Some(model_id),
            plan_mode,
            send_method,
            reasoning_effort,
            permission_mode,
            ..Default::default()
        })
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn list_codex_remote_threads(
    state: State<'_, AppState>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    search_term: Option<String>,
    archived: Option<bool>,
) -> Result<CodexRemoteThreadPageDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.list_codex_thread_records(workspace_id, cursor, limit, search_term, archived).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn attach_codex_remote_thread(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_thread_id: String,
    // 旧接口接收前端选中的 model_id，会覆盖 Codex 历史会话自己的模型；禁止恢复。
    // model_id: String,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.attach_thread_record(workspace_id, engine_thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn list_opencode_remote_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    search_term: Option<String>,
    archived: Option<bool>,
) -> Result<OpenCodeRemoteSessionPageDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("opencode").map_err(err_to_string)?;
    cli.list_opencode_thread_records(workspace_id, cursor, limit, search_term, archived).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn attach_opencode_remote_session(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_thread_id: String,
    cwd: String,
    model_id: String,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("opencode").map_err(err_to_string)?;
    cli.attach_opencode_thread_record(workspace_id, engine_thread_id, cwd, model_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn create_thread(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_id: String,
    model_id: String,
    title: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> Result<ThreadDto, String> {
    ThreadService::new(state.inner().clone()).create_thread(workspace_id, engine_id, model_id, title, reasoning_effort, service_tier).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn reconfigure_unstarted_thread_runtime(
    state: State<'_, AppState>,
    thread_id: String,
    engine_id: String,
    model_id: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create(&engine_id).map_err(err_to_string)?;
    cli.reconfigure_thread_record(thread_id, model_id, reasoning_effort, service_tier).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn set_thread_reasoning_effort(
    state: State<'_, AppState>,
    thread_id: String,
    reasoning_effort: Option<String>,
    model_id: Option<String>,
) -> Result<(), String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.set_thread_reasoning(thread_id, reasoning_effort, model_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn rename_thread(
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
) -> Result<ThreadDto, String> {
    ThreadService::new(state.inner().clone()).rename_thread(thread_id, title).await
}

#[tauri::command]
pub async fn delete_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.delete_thread_record(thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn archive_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.archive_thread_record(thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn archive_thread_locally(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    ThreadService::new(state.inner().clone()).archive_thread_locally(&thread_id).await
}

#[tauri::command]
pub async fn restore_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.restore_thread_record(thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn sync_thread_from_engine(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.sync_thread_record(thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn fork_codex_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.fork_thread_record(thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn rollback_codex_thread(
    state: State<'_, AppState>,
    thread_id: String,
    num_turns: u32,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.rollback_thread_record(thread_id, num_turns).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn compact_codex_thread(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.compact_thread_record(app, thread_id).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn set_ssh_remote_thread_selected_model(
    state: State<'_, AppState>,
    thread_id: String,
    model_id: String,
) -> Result<ThreadDto, String> {
    let cli = ThreadService::new(state.inner().clone()).cli_for_thread(&thread_id).await.map_err(err_to_string)?;
    cli.set_thread_selected_model(thread_id, model_id).await.map_err(err_to_string)
}

/// 按线程标识读取完整统一权限对象 A。
#[tauri::command]
pub async fn get_thread_permissions(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<PermissionComponentJson, String> {
    ThreadService::new(state.inner().clone())
        .get_permissions(&thread_id)
        .await
        .map_err(err_to_string)
}

/// 校验并按线程标识直接保存统一权限对象 A，不依赖线程使用的 CLI。
#[tauri::command]
pub async fn set_thread_permissions(
    state: State<'_, AppState>,
    thread_id: String,
    values: PermissionComponentJson,
) -> Result<PermissionComponentJson, String> {
    let updated = ThreadService::new(state.inner().clone())
        .save_permissions(&thread_id, values)
        .await
        .map_err(err_to_string)?;
    updated
        .permission_mode
        .ok_or_else(|| "thread permission object is unavailable after update".to_string())
}

#[tauri::command]
pub async fn set_thread_codex_config(
    state: State<'_, AppState>,
    thread_id: String,
    update_personality: bool,
    personality: Option<String>,
    update_service_tier: bool,
    service_tier: Option<String>,
    update_output_schema: bool,
    output_schema: Option<Value>,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("codex").map_err(err_to_string)?;
    cli.update_codex_thread_config(thread_id, update_personality, personality, update_service_tier, service_tier, update_output_schema, output_schema).await.map_err(err_to_string)
}

#[tauri::command]
pub async fn set_thread_opencode_config(
    state: State<'_, AppState>,
    thread_id: String,
    update_agent: bool,
    agent: Option<String>,
) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.inner().clone()).create("opencode").map_err(err_to_string)?;
    cli.update_opencode_thread_config(thread_id, update_agent, agent).await.map_err(err_to_string)
}

#[cfg(test)]
#[path = "../../tests/unit/thread_commands_tests.rs"]
mod tests;
