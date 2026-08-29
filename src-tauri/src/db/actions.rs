use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use crate::engines::events::{ActionResult, ActionType};
use crate::runtime_env;

use super::Database;

#[allow(clippy::too_many_arguments)]
pub fn insert_action_started(
    db: &Database,
    action_id: &str,
    thread_id: &str,
    message_id: &str,
    engine_action_id: Option<&str>,
    action_type: &ActionType,
    summary: &str,
    details: &Value,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let created_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO actions (
      id, thread_id, message_id, engine_action_id, action_type, summary, details_json, status, created_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)",
        params![
            action_id,
            thread_id,
            message_id,
            engine_action_id,
            action_type.as_str(),
            summary,
            details.to_string(),
            created_at
        ],
    )
    .context("failed to insert action")?;
    Ok(())
}

pub fn update_action_completed(
    db: &Database,
    action_id: &str,
    result: &ActionResult,
) -> anyhow::Result<()> {
    let status = if result.success { "done" } else { "error" };
    let conn = db.connect()?;
    conn.execute(
        "UPDATE actions
     SET status = ?1, result_json = ?2, duration_ms = ?3
     WHERE id = ?4",
        params![
            status,
            serde_json::to_string(result)?,
            result.duration_ms as i64,
            action_id
        ],
    )
    .context("failed to complete action")?;
    Ok(())
}

pub fn insert_approval(
    db: &Database,
    approval_id: &str,
    thread_id: &str,
    message_id: &str,
    action_type: &ActionType,
    summary: &str,
    details: &Value,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let created_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO approvals (
      id, thread_id, message_id, action_type, summary, details_json, status, created_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
        params![
            approval_id,
            thread_id,
            message_id,
            action_type.as_str(),
            summary,
            details.to_string(),
            created_at
        ],
    )
    .context("failed to insert approval")?;
    Ok(())
}

pub fn answer_approval(db: &Database, approval_id: &str, decision: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let answered_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "UPDATE approvals
     SET status = 'answered', decision = ?1, answered_at = ?2
     WHERE id = ?3",
        params![decision, answered_at, approval_id],
    )
    .context("failed to answer approval")?;
    Ok(())
}

#[cfg(test)]
pub fn resolve_approval(db: &Database, approval_id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let answered_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "UPDATE approvals
     SET status = 'answered', answered_at = COALESCE(answered_at, ?1)
     WHERE id = ?2",
        params![answered_at, approval_id],
    )
    .context("failed to resolve approval")?;
    Ok(())
}

pub fn find_approval_message_id(
    db: &Database,
    approval_id: &str,
) -> anyhow::Result<Option<String>> {
    let conn = db.connect()?;
    let message_id = conn
        .query_row(
            "SELECT message_id FROM approvals WHERE id = ?1",
            params![approval_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to load approval message id")?;
    Ok(message_id)
}

pub fn find_approval_details(db: &Database, approval_id: &str) -> anyhow::Result<Option<Value>> {
    let conn = db.connect()?;
    let raw_details = conn
        .query_row(
            "SELECT details_json FROM approvals WHERE id = ?1",
            params![approval_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to load approval details")?;

    let Some(raw_details) = raw_details else {
        return Ok(None);
    };

    match serde_json::from_str::<Value>(&raw_details) {
        Ok(details) => Ok(Some(details)),
        Err(error) => {
            log::warn!("failed to parse approval details for {approval_id}: {error}");
            Ok(None)
        }
    }
}

/// 查询指定线程中仍等待 OpenCode 工具权限回复的审批。
///
/// question.asked 与无法解析的详情都不会进入结果，避免切换执行权限时误回答
/// 用户输入问题；解析失败只记录日志并继续处理其他审批。
pub fn find_pending_opencode_permission_approval_ids(
    db: &Database,
    thread_id: &str,
) -> anyhow::Result<Vec<String>> {
    let conn = db.connect()?;
    let mut statement = conn
        .prepare(
            "SELECT id, details_json
             FROM approvals
             WHERE thread_id = ?1
               AND status = 'pending'
             ORDER BY created_at ASC, id ASC",
        )
        .context("failed to prepare pending OpenCode permission approvals query")?;
    let rows = statement
        .query_map(params![thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("failed to query pending OpenCode permission approvals")?;

    let mut approval_ids = Vec::new();
    for row in rows {
        let (approval_id, raw_details) = row.context("failed to read pending approval row")?;
        let details = match serde_json::from_str::<Value>(&raw_details) {
            Ok(details) => details,
            Err(error) => {
                log::warn!("failed to parse pending approval details for {approval_id}: {error}");
                continue;
            }
        };
        if details.get("_opencodeRequestKind").and_then(Value::as_str) == Some("permission") {
            approval_ids.push(approval_id);
        }
    }
    Ok(approval_ids)
}

pub fn find_approval_context(
    db: &Database,
    approval_id: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let conn = db.connect()?;
    let context = conn
        .query_row(
            "SELECT thread_id, message_id
             FROM approvals
             WHERE id = ?1
               AND thread_id IS NOT NULL
               AND message_id IS NOT NULL",
            params![approval_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("failed to load approval context")?;
    Ok(context)
}

pub fn append_event_log(
    db: &Database,
    thread_id: &str,
    message_id: &str,
    event: &Value,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let created_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "INSERT INTO engine_event_logs (thread_id, message_id, event_json, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![thread_id, message_id, event.to_string(), created_at],
    )
    .context("failed to append engine event log")?;
    Ok(())
}
