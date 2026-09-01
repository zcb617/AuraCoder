use anyhow::Context;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use uuid::Uuid;

use crate::models::{ScheduledTaskDto, ScheduledTaskRunDto, ThreadDto, ThreadStatusDto};

use super::Database;

#[derive(Debug, Clone)]
pub struct ScheduledTaskWrite {
    pub description: String,
    pub enabled: bool,
    pub execution_device_id: String,
    pub target_type: String,
    pub workspace_id: String,
    pub thread_id: Option<String>,
    pub runtime_config: Option<Value>,
    pub schedule_type: String,
    pub schedule: Value,
    pub timezone: String,
    pub next_run_at: Option<String>,
}

pub fn list_tasks(db: &Database) -> anyhow::Result<Vec<ScheduledTaskDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, description, enabled, execution_device_id, target_type,
                workspace_id, thread_id, runtime_config_json, schedule_type,
                schedule_json, timezone, next_run_at, last_run_at, created_at, updated_at
         FROM scheduled_tasks
         ORDER BY updated_at DESC, created_at DESC",
    )?;
    let rows = stmt.query_map([], map_task_row)?;
    let mut tasks = rows.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for task in &mut tasks {
        hydrate_task(&conn, task)?;
    }
    Ok(tasks)
}

pub fn has_tasks_in_enabled_column(db: &Database) -> anyhow::Result<bool> {
    Ok(list_tasks(db)?
        .into_iter()
        .any(|task| task.enabled && !task.needs_confirmation))
}

pub fn get_task(db: &Database, task_id: &str) -> anyhow::Result<Option<ScheduledTaskDto>> {
    let conn = db.connect()?;
    let mut task = conn
        .query_row(
            "SELECT id, description, enabled, execution_device_id, target_type,
                    workspace_id, thread_id, runtime_config_json, schedule_type,
                    schedule_json, timezone, next_run_at, last_run_at, created_at, updated_at
             FROM scheduled_tasks
             WHERE id = ?1",
            params![task_id],
            map_task_row,
        )
        .optional()?;
    if let Some(task) = task.as_mut() {
        hydrate_task(&conn, task)?;
    }
    Ok(task)
}

pub fn create_task(db: &Database, write: &ScheduledTaskWrite) -> anyhow::Result<ScheduledTaskDto> {
    let id = Uuid::new_v4().to_string();
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO scheduled_tasks (
            id, description, enabled, execution_device_id, target_type,
            workspace_id, thread_id, runtime_config_json, schedule_type,
            schedule_json, timezone, next_run_at, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            datetime('now'), datetime('now')
         )",
        params![
            id,
            write.description,
            write.enabled,
            write.execution_device_id,
            write.target_type,
            write.workspace_id,
            write.thread_id,
            write.runtime_config.as_ref().map(Value::to_string),
            write.schedule_type,
            write.schedule.to_string(),
            write.timezone,
            write.next_run_at,
        ],
    )
    .context("failed to insert scheduled task")?;
    drop(conn);
    get_task(db, &id)?.ok_or_else(|| anyhow::anyhow!("scheduled task not found after insert"))
}

pub fn update_task(
    db: &Database,
    task_id: &str,
    write: &ScheduledTaskWrite,
) -> anyhow::Result<ScheduledTaskDto> {
    let conn = db.connect()?;
    let changed = conn.execute(
        "UPDATE scheduled_tasks
         SET description = ?1,
             enabled = ?2,
             execution_device_id = ?3,
             target_type = ?4,
             workspace_id = ?5,
             thread_id = ?6,
             runtime_config_json = ?7,
             schedule_type = ?8,
             schedule_json = ?9,
             timezone = ?10,
             next_run_at = ?11,
             updated_at = datetime('now')
         WHERE id = ?12",
        params![
            write.description,
            write.enabled,
            write.execution_device_id,
            write.target_type,
            write.workspace_id,
            write.thread_id,
            write.runtime_config.as_ref().map(Value::to_string),
            write.schedule_type,
            write.schedule.to_string(),
            write.timezone,
            write.next_run_at,
            task_id,
        ],
    )?;
    if changed == 0 {
        anyhow::bail!("scheduled task not found: {task_id}");
    }
    drop(conn);
    get_task(db, task_id)?.ok_or_else(|| anyhow::anyhow!("scheduled task not found after update"))
}

pub fn set_task_enabled(
    db: &Database,
    task_id: &str,
    enabled: bool,
    next_run_at: Option<&str>,
) -> anyhow::Result<ScheduledTaskDto> {
    let conn = db.connect()?;
    let changed = conn.execute(
        "UPDATE scheduled_tasks
         SET enabled = ?1,
             next_run_at = ?2,
             updated_at = datetime('now')
         WHERE id = ?3",
        params![enabled, next_run_at, task_id],
    )?;
    if changed == 0 {
        anyhow::bail!("scheduled task not found: {task_id}");
    }
    drop(conn);
    get_task(db, task_id)?.ok_or_else(|| anyhow::anyhow!("scheduled task not found after update"))
}

pub fn delete_task(db: &Database, task_id: &str) -> anyhow::Result<bool> {
    let conn = db.connect()?;
    Ok(conn.execute(
        "DELETE FROM scheduled_tasks WHERE id = ?1",
        params![task_id],
    )? > 0)
}

pub fn acknowledge_latest_run(db: &Database, task_id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE scheduled_task_runs
         SET acknowledged_at = datetime('now')
         WHERE id = (
           SELECT id FROM scheduled_task_runs
           WHERE task_id = ?1
           ORDER BY created_at DESC, rowid DESC
           LIMIT 1
         )",
        params![task_id],
    )?;
    Ok(())
}

pub fn next_due_at(db: &Database) -> anyhow::Result<Option<String>> {
    let conn = db.connect()?;
    conn.query_row(
        "SELECT MIN(task.next_run_at)
         FROM scheduled_tasks task
         WHERE task.enabled = 1
           AND task.next_run_at IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM scheduled_task_runs run
             WHERE run.task_id = task.id
               AND (
                 run.status IN ('queued', 'running', 'needs_confirmation')
                 OR (run.status IN ('error', 'interrupted') AND run.acknowledged_at IS NULL)
               )
           )",
        [],
        |row| row.get(0),
    )
    .context("failed to query next scheduled task")
}

pub fn list_due_tasks(db: &Database, now: &str) -> anyhow::Result<Vec<ScheduledTaskDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, description, enabled, execution_device_id, target_type,
                workspace_id, thread_id, runtime_config_json, schedule_type,
                schedule_json, timezone, next_run_at, last_run_at, created_at, updated_at
         FROM scheduled_tasks task
         WHERE task.enabled = 1
           AND task.next_run_at IS NOT NULL
           AND task.next_run_at <= ?1
           AND NOT EXISTS (
             SELECT 1 FROM scheduled_task_runs run
             WHERE run.task_id = task.id
               AND (
                 run.status IN ('queued', 'running', 'needs_confirmation')
                 OR (run.status IN ('error', 'interrupted') AND run.acknowledged_at IS NULL)
               )
           )
         ORDER BY task.next_run_at ASC",
    )?;
    let tasks = stmt
        .query_map(params![now], map_task_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tasks)
}

pub fn claim_due_task(
    db: &Database,
    task_id: &str,
    scheduled_for: &str,
    next_run_at: &str,
) -> anyhow::Result<Option<ScheduledTaskRunDto>> {
    let mut conn = db.connect()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let eligible = tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM scheduled_tasks task
           WHERE task.id = ?1
             AND task.enabled = 1
             AND task.next_run_at = ?2
             AND NOT EXISTS (
               SELECT 1 FROM scheduled_task_runs run
               WHERE run.task_id = task.id
                 AND (
                   run.status IN ('queued', 'running', 'needs_confirmation')
                   OR (run.status IN ('error', 'interrupted') AND run.acknowledged_at IS NULL)
                 )
             )
         )",
        params![task_id, scheduled_for],
        |row| row.get::<_, bool>(0),
    )?;
    if !eligible {
        return Ok(None);
    }

    let run_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO scheduled_task_runs (id, task_id, scheduled_for, status)
         VALUES (?1, ?2, ?3, 'queued')",
        params![run_id, task_id, scheduled_for],
    )?;
    tx.execute(
        "UPDATE scheduled_tasks
         SET last_run_at = datetime('now'),
             next_run_at = ?1,
             updated_at = datetime('now')
         WHERE id = ?2",
        params![next_run_at, task_id],
    )?;
    tx.commit()?;
    get_run(db, &run_id)
}

pub fn mark_run_started(
    db: &Database,
    run_id: &str,
    thread_id: &str,
    assistant_message_id: &str,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE scheduled_task_runs
         SET status = 'running',
             started_at = COALESCE(started_at, datetime('now')),
             thread_id = ?1,
             assistant_message_id = ?2,
             error_message = NULL
         WHERE id = ?3",
        params![thread_id, assistant_message_id, run_id],
    )?;
    Ok(())
}

pub fn mark_run_error(db: &Database, run_id: &str, message: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE scheduled_task_runs
         SET status = 'error',
             started_at = COALESCE(started_at, datetime('now')),
             finished_at = datetime('now'),
             error_message = ?1,
             acknowledged_at = NULL
         WHERE id = ?2",
        params![message, run_id],
    )?;
    Ok(())
}

pub fn mark_run_skipped(db: &Database, run_id: &str, message: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE scheduled_task_runs
         SET status = 'skipped',
             started_at = COALESCE(started_at, datetime('now')),
             finished_at = datetime('now'),
             error_message = ?1
         WHERE id = ?2",
        params![message, run_id],
    )?;
    Ok(())
}

pub fn mark_run_needs_confirmation_by_message(
    db: &Database,
    assistant_message_id: &str,
) -> anyhow::Result<Option<String>> {
    let conn = db.connect()?;
    let task_id = conn
        .query_row(
            "SELECT task_id FROM scheduled_task_runs
             WHERE assistant_message_id = ?1
             ORDER BY created_at DESC
             LIMIT 1",
            params![assistant_message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if task_id.is_some() {
        conn.execute(
            "UPDATE scheduled_task_runs
             SET status = 'needs_confirmation', acknowledged_at = NULL
             WHERE assistant_message_id = ?1",
            params![assistant_message_id],
        )?;
    }
    Ok(task_id)
}

pub fn finish_run_by_message(
    db: &Database,
    assistant_message_id: &str,
    status: &str,
    result_preview: Option<&str>,
    error_message: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let conn = db.connect()?;
    let task_id = conn
        .query_row(
            "SELECT task_id FROM scheduled_task_runs
             WHERE assistant_message_id = ?1
             ORDER BY created_at DESC
             LIMIT 1",
            params![assistant_message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if task_id.is_some() {
        conn.execute(
            "UPDATE scheduled_task_runs
             SET status = ?1,
                 finished_at = datetime('now'),
                 result_preview = ?2,
                 error_message = ?3,
                 acknowledged_at = CASE WHEN ?1 IN ('completed', 'skipped')
                                        THEN datetime('now') ELSE NULL END
             WHERE assistant_message_id = ?4",
            params![status, result_preview, error_message, assistant_message_id],
        )?;
    }
    Ok(task_id)
}

pub fn recover_interrupted_runs(db: &Database) -> anyhow::Result<usize> {
    let conn = db.connect()?;
    Ok(conn.execute(
        "UPDATE scheduled_task_runs
         SET status = 'interrupted',
             finished_at = datetime('now'),
             error_message = COALESCE(error_message, 'AuraCoder exited before this scheduled run finished.'),
             acknowledged_at = NULL
         WHERE status IN ('queued', 'running')",
        [],
    )?)
}

pub fn active_thread_for_task(
    db: &Database,
    task: &ScheduledTaskDto,
) -> anyhow::Result<Option<ThreadDto>> {
    let Some(thread_id) = task.thread_id.as_deref() else {
        return Ok(None);
    };
    let conn = db.connect()?;
    conn.query_row(
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, title, status, message_count, total_tokens,
                context_current_tokens, context_max_tokens, context_usage_updated_at,
                created_at, last_activity_at,
                plan_mode, send_method, reasoning_effort, permission_mode
         FROM threads
         WHERE id = ?1 AND workspace_id = ?2 AND archived_at IS NULL",
        params![thread_id, task.workspace_id],
        |row| {
            let metadata: Option<String> = row.get(5)?;
            Ok(ThreadDto {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                engine_id: row.get(2)?,
                model_id: row.get(3)?,
                engine_thread_id: row.get(4)?,
                engine_metadata: metadata.and_then(|value| serde_json::from_str(&value).ok()),
                plan_mode: row.get(15)?,
                send_method: row.get(16)?,
                reasoning_effort: row.get(17)?,
                permission_mode: row.get(18)?,
                title: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                status: ThreadStatusDto::from_str(&row.get::<_, String>(7)?),
                message_count: row.get(8)?,
                total_tokens: row.get(9)?,
                context_current_tokens: row.get(10)?,
                context_max_tokens: row.get(11)?,
                context_usage_updated_at: row.get(12)?,
                created_at: row.get(13)?,
                last_activity_at: row.get(14)?,
            })
        },
    )
    .optional()
    .context("failed to resolve scheduled task thread")
}

fn get_run(db: &Database, run_id: &str) -> anyhow::Result<Option<ScheduledTaskRunDto>> {
    let conn = db.connect()?;
    conn.query_row(
        "SELECT id, task_id, scheduled_for, started_at, finished_at, thread_id,
                assistant_message_id, status, error_message, result_preview,
                acknowledged_at, created_at
         FROM scheduled_task_runs WHERE id = ?1",
        params![run_id],
        map_run_row,
    )
    .optional()
    .context("failed to load scheduled task run")
}

fn hydrate_task(conn: &rusqlite::Connection, task: &mut ScheduledTaskDto) -> anyhow::Result<()> {
    task.latest_run = conn
        .query_row(
            "SELECT id, task_id, scheduled_for, started_at, finished_at, thread_id,
                    assistant_message_id, status, error_message, result_preview,
                    acknowledged_at, created_at
             FROM scheduled_task_runs
             WHERE task_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1",
            params![task.id],
            map_run_row,
        )
        .optional()?;

    task.target_valid = if task.target_type == "existing_thread" {
        match task.thread_id.as_deref() {
            Some(thread_id) => conn.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM threads
                   WHERE id = ?1 AND workspace_id = ?2 AND archived_at IS NULL
                 )",
                params![thread_id, task.workspace_id],
                |row| row.get(0),
            )?,
            None => false,
        }
    } else {
        conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM workspaces WHERE id = ?1 AND archived_at IS NULL
             )",
            params![task.workspace_id],
            |row| row.get(0),
        )?
    };

    task.needs_confirmation = !task.target_valid
        || task.latest_run.as_ref().is_some_and(|run| {
            run.acknowledged_at.is_none()
                && matches!(
                    run.status.as_str(),
                    "needs_confirmation" | "error" | "interrupted"
                )
        });
    Ok(())
}

fn map_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTaskDto> {
    let runtime_config_json: Option<String> = row.get(7)?;
    let schedule_json: String = row.get(9)?;
    Ok(ScheduledTaskDto {
        id: row.get(0)?,
        description: row.get(1)?,
        enabled: row.get(2)?,
        execution_device_id: row.get(3)?,
        target_type: row.get(4)?,
        workspace_id: row.get(5)?,
        thread_id: row.get(6)?,
        runtime_config: runtime_config_json.and_then(|value| serde_json::from_str(&value).ok()),
        schedule_type: row.get(8)?,
        schedule: serde_json::from_str(&schedule_json).unwrap_or(Value::Null),
        timezone: row.get(10)?,
        next_run_at: row.get(11)?,
        last_run_at: row.get(12)?,
        latest_run: None,
        needs_confirmation: false,
        target_valid: true,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn map_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTaskRunDto> {
    Ok(ScheduledTaskRunDto {
        id: row.get(0)?,
        task_id: row.get(1)?,
        scheduled_for: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        thread_id: row.get(5)?,
        assistant_message_id: row.get(6)?,
        status: row.get(7)?,
        error_message: row.get(8)?,
        result_preview: row.get(9)?,
        acknowledged_at: row.get(10)?,
        created_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!(
            "auracoder-scheduled-task-db-{}.sqlite",
            Uuid::new_v4()
        ));
        Database::open(path).expect("failed to create test database")
    }

    fn test_workspace(db: &Database) -> crate::models::WorkspaceDto {
        let root = std::env::temp_dir().join(format!(
            "auracoder-scheduled-task-workspace-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("failed to create test workspace root");
        db::workspaces::upsert_workspace(db, root.to_string_lossy().as_ref())
            .expect("failed to create test workspace")
    }

    #[test]
    fn create_and_list_task_with_latest_run() {
        let db = test_db();
        let workspace = test_workspace(&db);
        let task = create_task(
            &db,
            &ScheduledTaskWrite {
                description: "Check the workspace".into(),
                enabled: true,
                execution_device_id: "local".into(),
                target_type: "new_thread".into(),
                workspace_id: workspace.id,
                thread_id: None,
                runtime_config: Some(serde_json::json!({
                    "engineId":"codex",
                    "modelId":"gpt-5.4",
                    "reasoningEffort":"high"
                })),
                schedule_type: "interval".into(),
                schedule: serde_json::json!({"every": 5, "unit": "minutes"}),
                timezone: "Asia/Hong_Kong".into(),
                next_run_at: Some("2026-08-09T08:00:00+00:00".into()),
            },
        )
        .unwrap();
        assert!(task.enabled);
        assert!(task.target_valid);
        assert!(has_tasks_in_enabled_column(&db).unwrap());
        assert_eq!(
            task.runtime_config
                .as_ref()
                .and_then(|value| value.get("reasoningEffort"))
                .and_then(Value::as_str),
            Some("high")
        );

        let run = claim_due_task(
            &db,
            &task.id,
            "2026-08-09T08:00:00+00:00",
            "2026-08-09T08:05:00+00:00",
        )
        .unwrap()
        .expect("task should be claimed");
        mark_run_error(&db, &run.id, "target unavailable").unwrap();

        let listed = list_tasks(&db).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].needs_confirmation);
        assert_eq!(listed[0].latest_run.as_ref().unwrap().status, "error");
        assert!(!has_tasks_in_enabled_column(&db).unwrap());

        acknowledge_latest_run(&db, &task.id).unwrap();
        assert!(has_tasks_in_enabled_column(&db).unwrap());

        set_task_enabled(&db, &task.id, false, None).unwrap();
        assert!(!has_tasks_in_enabled_column(&db).unwrap());
    }

    #[test]
    fn claim_is_idempotent_for_same_scheduled_time() {
        let db = test_db();
        let workspace = test_workspace(&db);
        let task = create_task(
            &db,
            &ScheduledTaskWrite {
                description: "Check".into(),
                enabled: true,
                execution_device_id: "local".into(),
                target_type: "new_thread".into(),
                workspace_id: workspace.id,
                thread_id: None,
                runtime_config: None,
                schedule_type: "daily".into(),
                schedule: serde_json::json!({"time": "09:00"}),
                timezone: "UTC".into(),
                next_run_at: Some("2026-08-09T09:00:00+00:00".into()),
            },
        )
        .unwrap();

        assert!(claim_due_task(
            &db,
            &task.id,
            "2026-08-09T09:00:00+00:00",
            "2026-08-10T09:00:00+00:00"
        )
        .unwrap()
        .is_some());
        assert!(claim_due_task(
            &db,
            &task.id,
            "2026-08-09T09:00:00+00:00",
            "2026-08-10T09:00:00+00:00"
        )
        .unwrap()
        .is_none());
    }
}
