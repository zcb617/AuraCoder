use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{Emitter, State};

use crate::{
    commands::threads,
    db::{self, scheduled_tasks::ScheduledTaskWrite},
    models::{ScheduledTaskDto, ThreadDto},
    scheduled_tasks::schedule::initial_next_run_at,
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskInput {
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_device_id")]
    pub execution_device_id: String,
    pub target_type: String,
    pub workspace_id: String,
    pub thread_id: Option<String>,
    pub runtime_config: Option<Value>,
    pub schedule_type: String,
    pub schedule: Value,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledRuntimeConfig {
    engine_id: String,
    model_id: String,
    repo_id: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledTaskEvent {
    task_id: String,
}

fn default_enabled() -> bool {
    true
}

fn default_device_id() -> String {
    "local".to_string()
}

#[tauri::command]
pub async fn list_scheduled_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<ScheduledTaskDto>, String> {
    run_db(state.db.clone(), db::scheduled_tasks::list_tasks).await
}

#[tauri::command]
pub async fn create_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: ScheduledTaskInput,
) -> Result<ScheduledTaskDto, String> {
    let write = validate_and_prepare(state.inner(), input).await?;
    let task = run_db(state.db.clone(), move |db| {
        db::scheduled_tasks::create_task(db, &write)
    })
    .await?;
    state.scheduled_tasks.wake();
    emit_updated(&app, &task.id);
    Ok(task)
}

#[tauri::command]
pub async fn update_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    input: ScheduledTaskInput,
) -> Result<ScheduledTaskDto, String> {
    let write = validate_and_prepare(state.inner(), input).await?;
    let task_id_for_db = task_id.clone();
    let task = run_db(state.db.clone(), move |db| {
        db::scheduled_tasks::update_task(db, &task_id_for_db, &write)
    })
    .await?;
    state.scheduled_tasks.wake();
    emit_updated(&app, &task_id);
    Ok(task)
}

#[tauri::command]
pub async fn set_scheduled_task_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    enabled: bool,
) -> Result<ScheduledTaskDto, String> {
    let db = state.db.clone();
    let existing_id = task_id.clone();
    let existing = run_db(db.clone(), move |db| {
        db::scheduled_tasks::get_task(db, &existing_id)
    })
    .await?
    .ok_or_else(|| format!("scheduled task not found: {task_id}"))?;
    let next_run_at = if enabled {
        Some(
            initial_next_run_at(
                &existing.schedule_type,
                &existing.schedule,
                &existing.timezone,
                Utc::now(),
            )?
            .to_rfc3339(),
        )
    } else {
        None
    };
    let task_id_for_db = task_id.clone();
    let task = run_db(db, move |db| {
        db::scheduled_tasks::set_task_enabled(db, &task_id_for_db, enabled, next_run_at.as_deref())
    })
    .await?;
    state.scheduled_tasks.wake();
    emit_updated(&app, &task_id);
    Ok(task)
}

#[tauri::command]
pub async fn acknowledge_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<ScheduledTaskDto, String> {
    let task_id_for_db = task_id.clone();
    run_db(state.db.clone(), move |db| {
        db::scheduled_tasks::acknowledge_latest_run(db, &task_id_for_db)
    })
    .await?;
    state.scheduled_tasks.wake();
    emit_updated(&app, &task_id);
    let task_id_for_db = task_id.clone();
    run_db(state.db.clone(), move |db| {
        db::scheduled_tasks::get_task(db, &task_id_for_db)?
            .ok_or_else(|| anyhow::anyhow!("scheduled task not found: {task_id_for_db}"))
    })
    .await
}

#[tauri::command]
pub async fn delete_scheduled_task(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    let task_id_for_db = task_id.clone();
    let deleted = run_db(state.db.clone(), move |db| {
        db::scheduled_tasks::delete_task(db, &task_id_for_db)
    })
    .await?;
    if !deleted {
        return Err(format!("scheduled task not found: {task_id}"));
    }
    state.scheduled_tasks.wake();
    let _ = app.emit("scheduled-task-deleted", ScheduledTaskEvent { task_id });
    Ok(())
}

async fn validate_and_prepare(
    state: &AppState,
    input: ScheduledTaskInput,
) -> Result<ScheduledTaskWrite, String> {
    let description = input.description.trim().to_string();
    if description.is_empty() {
        return Err("scheduled task description is required".to_string());
    }
    if description.chars().count() > 20_000 {
        return Err("scheduled task description is too long".to_string());
    }
    if input.execution_device_id != "local" {
        return Err("only the local execution device is supported".to_string());
    }
    if !matches!(input.target_type.as_str(), "existing_thread" | "new_thread") {
        return Err("invalid scheduled task target type".to_string());
    }

    let workspace_id = input.workspace_id.trim().to_string();
    if workspace_id.is_empty() {
        return Err("scheduled task workspace is required".to_string());
    }
    let db = state.db.clone();
    let workspace_id_for_db = workspace_id.clone();
    let workspace_exists = run_db(db.clone(), move |db| {
        Ok(db::workspaces::list_workspaces(db)?
            .iter()
            .any(|workspace| workspace.id == workspace_id_for_db))
    })
    .await?;
    if !workspace_exists {
        return Err("scheduled task workspace is unavailable".to_string());
    }

    let thread_id = input
        .thread_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let selected_thread = if input.target_type == "existing_thread" {
        let thread_id_value = thread_id
            .as_deref()
            .ok_or_else(|| "an existing thread must be selected".to_string())?;
        let workspace_id_for_db = workspace_id.clone();
        let thread_id_for_db = thread_id_value.to_string();
        let thread = run_db(db, move |db| {
            Ok(
                db::threads::list_threads_for_workspace(db, &workspace_id_for_db)?
                    .into_iter()
                    .find(|thread| thread.id == thread_id_for_db),
            )
        })
        .await?;
        Some(thread.ok_or_else(|| "selected thread is unavailable or archived".to_string())?)
    } else {
        None
    };

    let runtime_value = input
        .runtime_config
        .or_else(|| selected_thread.as_ref().map(runtime_config_from_thread));
    let runtime = validate_runtime_config(state, runtime_value.as_ref()).await?;
    if selected_thread
        .as_ref()
        .is_some_and(|thread| thread.engine_id != runtime.engine_id)
    {
        return Err("selected thread does not use the selected execution agent".to_string());
    }

    let timezone = input.timezone.trim().to_string();
    let next_run_at = if input.enabled {
        Some(
            initial_next_run_at(&input.schedule_type, &input.schedule, &timezone, Utc::now())?
                .to_rfc3339(),
        )
    } else {
        crate::scheduled_tasks::schedule::validate_schedule(
            &input.schedule_type,
            &input.schedule,
            &timezone,
        )?;
        None
    };

    let target_type = input.target_type;
    let existing_thread_target = target_type == "existing_thread";
    Ok(ScheduledTaskWrite {
        description,
        enabled: input.enabled,
        execution_device_id: input.execution_device_id,
        target_type,
        workspace_id,
        thread_id: if existing_thread_target {
            thread_id
        } else {
            None
        },
        runtime_config: Some(serde_json::to_value(runtime).map_err(|error| error.to_string())?),
        schedule_type: input.schedule_type,
        schedule: input.schedule,
        timezone,
        next_run_at,
    })
}

async fn validate_runtime_config(
    state: &AppState,
    value: Option<&Value>,
) -> Result<ScheduledRuntimeConfig, String> {
    let mut runtime: ScheduledRuntimeConfig = serde_json::from_value(
        value
            .cloned()
            .ok_or_else(|| "scheduled task runtime configuration is required".to_string())?,
    )
    .map_err(|error| format!("scheduled task runtime configuration is invalid: {error}"))?;
    runtime.engine_id = runtime.engine_id.trim().to_string();
    runtime.model_id = runtime.model_id.trim().to_string();
    if runtime.engine_id.is_empty() {
        return Err("scheduled task execution agent is required".to_string());
    }
    if runtime.model_id.is_empty() {
        return Err("scheduled task model is required".to_string());
    }

    runtime.model_id =
        threads::validate_model_for_engine(state, &runtime.engine_id, &runtime.model_id).await?;
    runtime.reasoning_effort = runtime
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if let Some(reasoning_effort) = runtime.reasoning_effort.as_deref() {
        runtime.reasoning_effort = Some(
            threads::validate_reasoning_effort(
                state,
                &runtime.engine_id,
                &runtime.model_id,
                reasoning_effort,
            )
            .await?,
        );
    }
    runtime.repo_id = runtime
        .repo_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    runtime.service_tier = match runtime
        .service_tier
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "inherit")
    {
        Some("fast" | "flex") if runtime.engine_id == "codex" => runtime.service_tier,
        Some(_) if runtime.engine_id != "codex" => None,
        Some(value) => return Err(format!("invalid Codex service tier `{value}`")),
        None => None,
    };
    Ok(runtime)
}

fn runtime_config_from_thread(thread: &ThreadDto) -> Value {
    let metadata = thread.engine_metadata.as_ref();
    serde_json::json!({
        "engineId": thread.engine_id,
        "modelId": thread.model_id,
        "repoId": thread.repo_id,
        "reasoningEffort": thread.reasoning_effort,
        "serviceTier": metadata
            .and_then(|value| value.get("serviceTier"))
            .and_then(Value::as_str),
    })
}

fn emit_updated(app: &tauri::AppHandle, task_id: &str) {
    let _ = app.emit(
        "scheduled-task-updated",
        ScheduledTaskEvent {
            task_id: task_id.to_string(),
        },
    );
}

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}
