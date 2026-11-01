use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::{sync::Notify, time::sleep};

use crate::{
    db,
    models::{
        chat::SendMessageRequest, MessageStatusDto, ScheduledTaskDto, ScheduledTaskRunDto,
        ThreadDto,
    },
    runtime_env,
    services::chat_service::ChatService,
    state::AppState,
};

use super::schedule::next_run_after_due;

const SCHEDULER_RECHECK_DELAY: Duration = Duration::from_secs(15);
const SCHEDULER_IDLE_DELAY: Duration = Duration::from_secs(6 * 60 * 60);
const THREAD_BUSY_RETRY_DELAY: Duration = Duration::from_secs(5);
const THREAD_BUSY_RETRY_COUNT: usize = 12;

#[derive(Default)]
pub struct ScheduledTaskManager {
    wake: Notify,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledTaskEvent {
    task_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledRuntimeConfig {
    engine_id: String,
    model_id: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

impl ScheduledTaskManager {
    /// 判断是否存在退出前需要确认的已启用定时任务。
    pub fn has_enabled_tasks(&self, state: &AppState) -> anyhow::Result<bool> {
        match db::scheduled_tasks::list_tasks(&state.db) {
            Ok(tasks) => Ok(has_runnable_enabled_tasks(&tasks)),
            Err(error) => {
                log::error!("查询退出前定时任务失败: {error:#}");
                Err(error)
            }
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn start(self: Arc<Self>, app: AppHandle, state: AppState) {
        tauri::async_runtime::spawn(async move {
            if let Err(error) = recover_interrupted_runs(&state).await {
                log::warn!("failed to recover scheduled task runs: {error}");
            }

            loop {
                if let Err(error) = process_due_tasks(&app, &state).await {
                    log::warn!("failed to process due scheduled tasks: {error}");
                }

                let delay = next_scheduler_delay(&state).await;
                tokio::select! {
                    _ = sleep(delay) => {}
                    _ = self.wake.notified() => {}
                }
            }
        });
    }
}

/// 判断任务列表中是否存在启用且无需确认即可运行的任务。
fn has_runnable_enabled_tasks(tasks: &[crate::models::ScheduledTaskDto]) -> bool {
    tasks
        .iter()
        .any(|task| task.enabled && !task.needs_confirmation)
}

async fn recover_interrupted_runs(state: &AppState) -> Result<(), String> {
    let db = state.db.clone();
    let recovered =
        tokio::task::spawn_blocking(move || db::scheduled_tasks::recover_interrupted_runs(&db))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    if recovered > 0 {
        log::info!("recovered {recovered} interrupted scheduled task runs");
    }
    Ok(())
}

async fn next_scheduler_delay(state: &AppState) -> Duration {
    let db = state.db.clone();
    let next = tokio::task::spawn_blocking(move || db::scheduled_tasks::next_due_at(&db)).await;
    let Ok(Ok(Some(next))) = next else {
        return SCHEDULER_IDLE_DELAY;
    };
    let Ok(next) = runtime_env::parse_persisted_time_to_utc(&next) else {
        return SCHEDULER_RECHECK_DELAY;
    };
    next.signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or(SCHEDULER_RECHECK_DELAY)
        .max(Duration::from_secs(1))
}

async fn process_due_tasks(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let now = Utc::now();
    let now_rfc3339 = runtime_env::system_time_rfc3339();
    let db = state.db.clone();
    let due =
        tokio::task::spawn_blocking(move || db::scheduled_tasks::list_due_tasks(&db, &now_rfc3339))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

    for task in due {
        let Some(scheduled_for_raw) = task.next_run_at.as_deref() else {
            continue;
        };
        let scheduled_for = runtime_env::parse_persisted_time_to_utc(scheduled_for_raw)
            .map_err(|error| {
                format!(
                    "invalid scheduled task next_run_at {:?}: {error}",
                    scheduled_for_raw
                )
            })?;
        let next_run = next_run_after_due(
            &task.schedule_type,
            &task.schedule,
            &task.timezone,
            scheduled_for,
            now,
        )?;
        let db = state.db.clone();
        let task_id = task.id.clone();
        let scheduled_for_raw = scheduled_for_raw.to_string();
        let scheduled_for_string = runtime_env::normalize_time_to_local(&scheduled_for.to_rfc3339())
            .map_err(|error| {
                format!(
                    "failed to normalize scheduled task next_run_at {:?}: {error}",
                    scheduled_for_raw
                )
            })?;
        let next_run_raw = next_run.to_rfc3339();
        let next_run_string = runtime_env::normalize_time_to_local(&next_run_raw).map_err(|error| {
            format!(
                "failed to normalize scheduled task computed next_run_at {:?}: {error}",
                next_run_raw
            )
        })?;
        let run = tokio::task::spawn_blocking(move || {
            db::scheduled_tasks::claim_due_task(
                &db,
                &task_id,
                &scheduled_for_string,
                &next_run_string,
            )
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        let Some(run) = run else {
            continue;
        };

        emit_task_updated(app, &task.id);
        let app = app.clone();
        let state = state.clone();
        tauri::async_runtime::spawn(async move {
            execute_claimed_task(&app, &state, task, run).await;
        });
    }
    Ok(())
}

async fn execute_claimed_task(
    app: &AppHandle,
    state: &AppState,
    task: ScheduledTaskDto,
    run: ScheduledTaskRunDto,
) {
    let result = execute_claimed_task_inner(app, state, &task, &run).await;
    if let Err(error) = result {
        let db = state.db.clone();
        let run_id = run.id.clone();
        let error_for_db = error.clone();
        if let Err(db_error) = tokio::task::spawn_blocking(move || {
            db::scheduled_tasks::mark_run_error(&db, &run_id, &error_for_db)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            log::warn!("failed to persist scheduled task execution error: {db_error}");
        }
        emit_task_updated(app, &task.id);
    }
}

async fn execute_claimed_task_inner(
    app: &AppHandle,
    state: &AppState,
    task: &ScheduledTaskDto,
    run: &ScheduledTaskRunDto,
) -> Result<(), String> {
    if task.execution_device_id != "local" {
        return Err("This scheduled task targets an unsupported execution device.".to_string());
    }

    let (thread, runtime) = match task.target_type.as_str() {
        "existing_thread" => {
            let thread = resolve_existing_thread(state, task).await?;
            let runtime = runtime_config_for_task(task, Some(&thread))?;
            if thread.engine_id != runtime.engine_id {
                return Err(
                    "The selected thread does not use the configured execution agent.".to_string(),
                );
            }
            (thread, runtime)
        }
        "new_thread" => {
            let runtime = runtime_config_for_task(task, None)?;
            let thread = create_scheduled_thread(state, task, run, &runtime).await?;
            (thread, runtime)
        }
        _ => return Err("Scheduled task target type is invalid.".to_string()),
    };

    for attempt in 0..THREAD_BUSY_RETRY_COUNT {
        if state.turns.get(&thread.id).await.is_none() {
            break;
        }
        if attempt + 1 == THREAD_BUSY_RETRY_COUNT {
            let db = state.db.clone();
            let run_id = run.id.clone();
            tokio::task::spawn_blocking(move || {
                db::scheduled_tasks::mark_run_skipped(
                    &db,
                    &run_id,
                    "The target thread remained busy for one minute.",
                )
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
            emit_task_updated(app, &task.id);
            return Ok(());
        }
        sleep(THREAD_BUSY_RETRY_DELAY).await;
    }

    ChatService::new(state.clone()).send_message(
        app.clone(),
        SendMessageRequest {
            thread_id: thread.id.clone(),
            message: task.description.clone(),
            engine_id: Some(runtime.engine_id),
            model_id: Some(runtime.model_id),
            plan_mode: Some(false),
            send_method: thread.send_method.clone(),
            reasoning_effort: runtime.reasoning_effort,
            permission_values: None,
            attachments: None,
            input_items: None,
            client_turn_id: Some(format!("scheduled:{}", run.id)),
            scheduled_run_id: Some(run.id.clone()),
            referenced_thread_id: None,
        },
    )
    .await
    .map_err(|error| error.message)?;
    emit_task_updated(app, &task.id);
    Ok(())
}

async fn resolve_existing_thread(
    state: &AppState,
    task: &ScheduledTaskDto,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let task = task.clone();
    tokio::task::spawn_blocking(move || db::scheduled_tasks::active_thread_for_task(&db, &task))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The selected thread is unavailable or archived.".to_string())
}

async fn create_scheduled_thread(
    state: &AppState,
    task: &ScheduledTaskDto,
    run: &ScheduledTaskRunDto,
    runtime: &ScheduledRuntimeConfig,
) -> Result<ThreadDto, String> {
    let title_line = task
        .description
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Scheduled task")
        .trim();
    let mut title = title_line.chars().take(72).collect::<String>();
    let scheduled_suffix = DateTime::parse_from_rfc3339(&run.scheduled_for)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| run.scheduled_for.clone());
    title.push_str(" · ");
    title.push_str(&scheduled_suffix);

    crate::services::thread_service::ThreadService::new(state.clone()).create_thread(
        task.workspace_id.clone(),
        runtime.engine_id.clone(),
        runtime.model_id.clone(),
        title,
        runtime.reasoning_effort.clone(),
        runtime.service_tier.clone(),
    )
    .await
    .map_err(|error| format!("{error:#}"))
}

fn runtime_config_for_task(
    task: &ScheduledTaskDto,
    existing_thread: Option<&ThreadDto>,
) -> Result<ScheduledRuntimeConfig, String> {
    if let Some(value) = task.runtime_config.clone() {
        return serde_json::from_value(value)
            .map_err(|error| format!("Scheduled task runtime configuration is invalid: {error}"));
    }

    let thread = existing_thread
        .ok_or_else(|| "Scheduled task runtime configuration is missing.".to_string())?;
    Ok(ScheduledRuntimeConfig {
        engine_id: thread.engine_id.clone(),
        model_id: thread.model_id.clone(),
        reasoning_effort: thread.reasoning_effort.clone(),
        service_tier: thread
            .engine_metadata
            .as_ref()
            .and_then(|value| value.get("serviceTier"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

pub fn emit_task_updated(app: &AppHandle, task_id: &str) {
    let _ = app.emit(
        "scheduled-task-updated",
        ScheduledTaskEvent {
            task_id: task_id.to_string(),
        },
    );
}

/// 根据聊天助手消息的终态完成关联的定时任务运行，并通知调度器刷新任务状态。
pub(crate) async fn finish_chat_run(
    app: &AppHandle,
    state: &AppState,
    assistant_message_id: &str,
    status: &MessageStatusDto,
    preview: Option<String>,
) -> anyhow::Result<()> {
    // 仅将真实终态映射为定时任务状态；流式状态不代表本轮已经结束。
    let run_status = match status {
        MessageStatusDto::Completed => "completed",
        MessageStatusDto::Interrupted => "interrupted",
        MessageStatusDto::Error => "error",
        MessageStatusDto::Streaming => {
            log::warn!("skip scheduled task completion for streaming assistant message {assistant_message_id}");
            return Ok(());
        }
    };
    let error_message = matches!(status, MessageStatusDto::Error)
        .then_some("The scheduled task turn ended with an error.");

    // 通过 Mapper 按助手消息定位并结算定时任务运行记录。
    let db = state.db.clone();
    let assistant_message_id = assistant_message_id.to_string();
    let task_id = tauri::async_runtime::spawn_blocking(move || {
        db::scheduled_tasks::finish_run_by_message(
            &db,
            &assistant_message_id,
            run_status,
            preview.as_deref(),
            error_message,
        )
    })
    .await??;

    // 仅在确有关联任务时广播更新并唤醒调度器。
    if let Some(task_id) = task_id {
        emit_task_updated(app, &task_id);
        state.scheduled_tasks.wake();
    }
    Ok(())
}

#[cfg(test)]
#[path="../../tests/unit/scheduled_task_manager_tests.rs"]
mod tests;
