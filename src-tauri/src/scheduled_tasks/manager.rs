use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::{sync::Notify, time::sleep};

use crate::{
    commands::{chat, threads},
    db,
    models::{ScheduledTaskDto, ScheduledTaskRunDto, ThreadDto},
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
    repo_id: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

impl ScheduledTaskManager {
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
    let Ok(next) = DateTime::parse_from_rfc3339(&next) else {
        return SCHEDULER_RECHECK_DELAY;
    };
    next.with_timezone(&Utc)
        .signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or(SCHEDULER_RECHECK_DELAY)
        .max(Duration::from_secs(1))
}

async fn process_due_tasks(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let now = Utc::now();
    let now_rfc3339 = now.to_rfc3339();
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
        let scheduled_for = DateTime::parse_from_rfc3339(scheduled_for_raw)
            .map_err(|error| format!("invalid scheduled task next_run_at: {error}"))?
            .with_timezone(&Utc);
        let next_run = next_run_after_due(
            &task.schedule_type,
            &task.schedule,
            &task.timezone,
            scheduled_for,
            now,
        )?;
        let db = state.db.clone();
        let task_id = task.id.clone();
        let scheduled_for_string = scheduled_for.to_rfc3339();
        let next_run_string = next_run.to_rfc3339();
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

    chat::send_message_inner(
        app.clone(),
        state,
        thread.id.clone(),
        task.description.clone(),
        Some(runtime.model_id),
        runtime.reasoning_effort,
        None,
        None,
        Some(false),
        Some(format!("scheduled:{}", run.id)),
        Some(run.id.clone()),
        None,
    )
    .await?;
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

    threads::create_thread_with_defaults(
        state,
        task.workspace_id.clone(),
        runtime.repo_id.clone(),
        runtime.engine_id.clone(),
        runtime.model_id.clone(),
        title,
        runtime.reasoning_effort.clone(),
        runtime.service_tier.clone(),
    )
    .await
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
        repo_id: thread.repo_id.clone(),
        reasoning_effort: thread.reasoning_effort.clone(),
        service_tier: thread.engine_metadata.as_ref()
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::models::ThreadStatusDto;

    fn task(runtime_config: Option<serde_json::Value>) -> ScheduledTaskDto {
        ScheduledTaskDto {
            id: "task-1".to_string(),
            description: "Check workspace".to_string(),
            enabled: true,
            execution_device_id: "local".to_string(),
            target_type: "existing_thread".to_string(),
            workspace_id: "workspace-1".to_string(),
            thread_id: Some("thread-1".to_string()),
            runtime_config,
            schedule_type: "daily".to_string(),
            schedule: json!({ "time": "09:00" }),
            timezone: "UTC".to_string(),
            next_run_at: None,
            last_run_at: None,
            latest_run: None,
            needs_confirmation: false,
            target_valid: true,
            created_at: "2026-08-09T00:00:00Z".to_string(),
            updated_at: "2026-08-09T00:00:00Z".to_string(),
        }
    }

    fn thread() -> ThreadDto {
        ThreadDto {
            id: "thread-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            repo_id: Some("repo-1".to_string()),
            engine_id: "codex".to_string(),
            model_id: "gpt-selected".to_string(),
            engine_thread_id: None,
            engine_metadata: Some(json!({
                "serviceTier": "fast"
                // 旧六字段 metadata 夹具已停用：
                // "lastModelId": "gpt-selected",
                // "reasoningEffort": "high",
            })),
            plan_mode: None,
            send_method: None,
            reasoning_effort: Some("high".to_string()),
            permission_mode: None,
            title: "Existing".to_string(),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            created_at: "2026-08-09T00:00:00Z".to_string(),
            last_activity_at: "2026-08-09T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn existing_task_without_snapshot_uses_thread_runtime() {
        let runtime = runtime_config_for_task(&task(None), Some(&thread())).unwrap();
        assert_eq!(runtime.engine_id, "codex");
        assert_eq!(runtime.model_id, "gpt-selected");
        assert_eq!(runtime.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(runtime.service_tier.as_deref(), Some("fast"));
        assert_eq!(runtime.repo_id.as_deref(), Some("repo-1"));
    }

    #[test]
    fn task_snapshot_takes_precedence_over_thread_runtime() {
        let runtime = runtime_config_for_task(
            &task(Some(json!({
                "engineId": "codex",
                "modelId": "gpt-task",
                "reasoningEffort": "xhigh"
            }))),
            Some(&thread()),
        )
        .unwrap();
        assert_eq!(runtime.model_id, "gpt-task");
        assert_eq!(runtime.reasoning_effort.as_deref(), Some("xhigh"));
    }
}
