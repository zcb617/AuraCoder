use tauri::State;

use crate::{
    models::{ScheduledTaskDto, ScheduledTaskInput},
    services::scheduled_task_service::ScheduledTaskService,
    state::AppState,
};

/// 查询全部定时任务并返回兼容前端命令的结果。
#[tauri::command]
pub async fn list_scheduled_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<ScheduledTaskDto>, String> {
    // 将任务查询交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .list_scheduled_tasks()
        .await
        .map_err(|error| error.to_string())
}

/// 创建定时任务并保持原命令名称、入参和返回 DTO。
#[tauri::command]
pub async fn create_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: ScheduledTaskInput,
) -> Result<ScheduledTaskDto, String> {
    // 将创建校验和持久化交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .create_scheduled_task(app, input)
        .await
        .map_err(|error| error.to_string())
}

/// 更新定时任务并保持原命令名称、入参和返回 DTO。
#[tauri::command]
pub async fn update_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    input: ScheduledTaskInput,
) -> Result<ScheduledTaskDto, String> {
    // 将更新校验和持久化交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .update_scheduled_task(app, task_id, input)
        .await
        .map_err(|error| error.to_string())
}

/// 按指定状态启用或禁用定时任务。
#[tauri::command]
pub async fn set_scheduled_task_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    enabled: bool,
) -> Result<ScheduledTaskDto, String> {
    // 将启停状态和下次运行时间更新交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .set_scheduled_task_enabled(app, task_id, enabled)
        .await
        .map_err(|error| error.to_string())
}

/// 确认定时任务最近一次运行结果。
#[tauri::command]
pub async fn acknowledge_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<ScheduledTaskDto, String> {
    // 将运行结果确认及状态刷新交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .acknowledge_scheduled_task(app, task_id)
        .await
        .map_err(|error| error.to_string())
}

/// 删除指定定时任务。
#[tauri::command]
pub async fn delete_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    // 将删除及调度器刷新交给公共定时任务业务服务。
    let service = ScheduledTaskService::new(state.inner().clone());
    service
        .delete_scheduled_task(app, task_id)
        .await
        .map_err(|error| error.to_string())
}
