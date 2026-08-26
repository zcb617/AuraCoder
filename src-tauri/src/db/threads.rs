use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::models::{ThreadDto, ThreadStatusDto, ThreadUpdateDto};
use crate::runtime_env;

use super::Database;

#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeRecoveryReport {
    pub messages_marked_interrupted: usize,
    pub thread_status_updates: usize,
}

pub fn create_thread(
    db: &Database,
    workspace_id: &str,
    repo_id: Option<&str>,
    engine_id: &str,
    model_id: &str,
    title: &str,
) -> anyhow::Result<ThreadDto> {
    let id = Uuid::new_v4().to_string();
    let created_at = runtime_env::system_time_rfc3339();
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO threads (
            id, workspace_id, repo_id, engine_id, model_id, title, status, created_at, last_activity_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'idle', ?7, ?7)",
        params![id, workspace_id, repo_id, engine_id, model_id, title, created_at],
    )
    .context("failed to create thread")?;

    get_thread(db, &id)?.context("thread not found after insert")
}

pub fn get_thread(db: &Database, thread_id: &str) -> anyhow::Result<Option<ThreadDto>> {
    let conn = db.connect()?;
    conn.query_row(
    "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, created_at, last_activity_at,
            plan_mode, send_method, reasoning_effort, permission_mode
     FROM threads WHERE id = ?1",
    params![thread_id],
    map_thread_row,
  )
  .optional()
  .context("failed to query thread")
}

pub fn find_thread_by_engine_thread_id(
    db: &Database,
    engine_id: &str,
    engine_thread_id: &str,
) -> anyhow::Result<Option<ThreadDto>> {
    let conn = db.connect()?;
    conn.query_row(
        "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
                COALESCE(title, ''), status, message_count, total_tokens, created_at, last_activity_at,
                plan_mode, send_method, reasoning_effort, permission_mode
         FROM threads
         WHERE engine_id = ?1
           AND engine_thread_id = ?2
         LIMIT 1",
        params![engine_id, engine_thread_id],
        map_thread_row,
    )
    .optional()
    .context("failed to query thread by engine thread id")
}

/// 按工作区、引擎和远端线程 ID 查询线程。
///
/// SSH 远端项目允许不同工作区（甚至不同 SSH 主机）出现相同的引擎线程 ID，
/// 因此同步逻辑不得调用不带 workspace 条件的旧查询函数。
pub fn find_thread_by_workspace_engine_thread_id(
    db: &Database,
    workspace_id: &str,
    engine_id: &str,
    engine_thread_id: &str,
) -> anyhow::Result<Option<ThreadDto>> {
    let conn = db.connect()?;
    conn.query_row(
        "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, COALESCE(title, ''), status, message_count,
                total_tokens, created_at, last_activity_at,
                plan_mode, send_method, reasoning_effort, permission_mode
         FROM threads
         WHERE workspace_id = ?1 AND engine_id = ?2 AND engine_thread_id = ?3
         LIMIT 1",
        params![workspace_id, engine_id, engine_thread_id],
        map_thread_row,
    )
    .optional()
    .context("failed query workspace-scoped engine thread")
}

/// 在一个事务中写入 SSH 远端会话快照。
///
/// 远端本次未返回的本地线程不会被触碰；已归档但再次出现在远端列表中的线程
/// 会被恢复，保证刷新后的远端会话仍可显示。
pub fn upsert_ssh_remote_thread_snapshot(
    db: &Database,
    workspace_id: &str,
    engine_id: &str,
    engine_thread_id: &str,
    model_id: &str,
    title: &str,
    status: ThreadStatusDto,
    metadata: &serde_json::Value,
    reasoning_effort: Option<&str>,
    last_activity_at: Option<&str>,
) -> anyhow::Result<ThreadDto> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed begin remote thread sync transaction")?;
    let existing: Option<(String, String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT id, model_id, engine_metadata_json, reasoning_effort
             FROM threads
             WHERE workspace_id = ?1 AND engine_id = ?2 AND engine_thread_id = ?3
             LIMIT 1",
            params![workspace_id, engine_id, engine_thread_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .context("failed query remote thread identity")?;

    let normalized_model = if model_id.trim().is_empty() {
        "unknown"
    } else {
        model_id.trim()
    };
    let last_activity = last_activity_at.unwrap_or("");
    if let Some((thread_id, current_model, current_metadata_raw, current_reasoning_effort)) = existing {
        // 只有本地模型仍是 unknown 时，才接受远端提供的具体模型，避免覆盖用户配置。
        let next_model = if normalized_model != "unknown" && current_model == "unknown" {
            normalized_model
        } else {
            current_model.as_str()
        };
        // 远端快照只更新自身字段；模型、思考强度等本地会话选择必须保留。
        let next_metadata = match (
            current_metadata_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
            metadata.as_object(),
        ) {
            (Some(serde_json::Value::Object(mut current)), Some(remote)) => {
                current.extend(remote.clone());
                serde_json::Value::Object(current)
            }
            _ => metadata.clone(),
        };
        tx.execute(
            "UPDATE threads
             SET model_id = ?1,
                 title = ?2,
                 status = ?3,
                 engine_metadata_json = ?4,
                 archived_at = NULL,
                 last_activity_at = CASE
                     WHEN ?5 <> '' THEN ?5
                     ELSE ?6
                 END
             WHERE id = ?7",
            params![
                next_model,
                title,
                status.as_str(),
                next_metadata.to_string(),
                last_activity,
                runtime_env::system_time_rfc3339(),
                thread_id,
            ],
        )
        .context("failed update SSH remote thread snapshot")?;
        // CLI 服务端真实返回的模型与思考强度写入独立字段，其余底部项服务端不提供。
        let remote_reasoning_effort = current_reasoning_effort.or_else(|| reasoning_effort.map(str::to_owned));
        tx.execute(
            "UPDATE threads
             SET reasoning_effort = ?1
             WHERE id = ?2",
            params![remote_reasoning_effort, thread_id],
        )
        .context("failed update SSH remote thread runtime selection")?;
    } else {
        let thread_id = Uuid::new_v4().to_string();
        // CLI 服务端真实返回的模型与思考强度写入独立字段，其余底部项服务端不提供。
        let remote_reasoning_effort = reasoning_effort;
        tx.execute(
            "INSERT INTO threads (
                id, workspace_id, repo_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, title, status, last_activity_at, created_at,
                reasoning_effort
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8,
                       CASE WHEN ?9 <> '' THEN ?9 ELSE ?10 END, ?10, ?11)",
            params![
                thread_id,
                workspace_id,
                engine_id,
                normalized_model,
                engine_thread_id,
                metadata.to_string(),
                title,
                status.as_str(),
                last_activity,
                runtime_env::system_time_rfc3339(),
                remote_reasoning_effort,
            ],
        )
        .context("failed insert SSH remote thread snapshot")?;
    }
    tx.commit()
        .context("failed commit SSH remote thread snapshot")?;
    find_thread_by_workspace_engine_thread_id(db, workspace_id, engine_id, engine_thread_id)?
        .ok_or_else(|| anyhow::anyhow!("remote thread missing after sync: {engine_thread_id}"))
}

pub fn list_threads_for_workspace(
    db: &Database,
    workspace_id: &str,
) -> anyhow::Result<Vec<ThreadDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
    "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, created_at, last_activity_at,
            plan_mode, send_method, reasoning_effort, permission_mode
     FROM threads
     WHERE workspace_id = ?1
       AND archived_at IS NULL
       AND (
         engine_thread_id IS NOT NULL
         OR EXISTS (
           SELECT 1 FROM messages
           WHERE messages.thread_id = threads.id
         )
       )
     ORDER BY last_activity_at DESC",
  )?;

    let rows = stmt.query_map(params![workspace_id], map_thread_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list_archived_threads_for_workspace(
    db: &Database,
    workspace_id: &str,
) -> anyhow::Result<Vec<ThreadDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
    "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, created_at, last_activity_at,
            plan_mode, send_method, reasoning_effort, permission_mode
     FROM threads
     WHERE workspace_id = ?1
       AND archived_at IS NOT NULL
       AND (
         engine_thread_id IS NOT NULL
         OR EXISTS (
           SELECT 1 FROM messages
           WHERE messages.thread_id = threads.id
         )
       )
     ORDER BY archived_at DESC",
  )?;

    let rows = stmt.query_map(params![workspace_id], map_thread_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn update_thread_status(
    db: &Database,
    thread_id: &str,
    status: ThreadStatusDto,
) -> anyhow::Result<()> {
    // 先读取现有状态，保持旧 SQL 的“同状态不更新”以及缺失线程返回 Ok 的语义。
    let Some(existing) = get_thread(db, thread_id)? else {
        return Ok(());
    };
    if existing.status == status {
        return Ok(());
    }
    let updated_at = runtime_env::system_time_rfc3339();
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        status: Some(status.as_str().to_owned()),
        last_activity_at: Some(updated_at),
        ..Default::default()
    };

    /*
     * 旧实现保留如下，避免删除历史逻辑；实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * conn.execute(
     *     "UPDATE threads
     *  SET status = ?1, last_activity_at = ?2
     *  WHERE id = ?3
     *    AND status != ?1",
     *     params![status.as_str(), updated_at, thread_id],
     * )
     * .context("failed to update thread status")?;
     */
    update_thread(db, &update)?;
    Ok(())
}

pub fn set_engine_thread_id(
    db: &Database,
    thread_id: &str,
    engine_thread_id: &str,
) -> anyhow::Result<()> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        engine_thread_id: Some(engine_thread_id.to_owned()),
        ..Default::default()
    };

    /*
     * 旧实现保留如下，实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * conn.execute(
     *     "UPDATE threads SET engine_thread_id = ?1 WHERE id = ?2",
     *     params![engine_thread_id, thread_id],
     * )
     * .context("failed to set engine thread id")?;
     */
    update_thread(db, &update)?;
    Ok(())
}

/// 持久化调用方已经验证并格式化的线程最后活动时间。
pub fn update_thread_last_activity(
    db: &Database,
    thread_id: &str,
    last_activity_at: &str,
) -> anyhow::Result<ThreadDto> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        last_activity_at: Some(last_activity_at.to_owned()),
        ..Default::default()
    };

    /*
     * 旧实现保留如下，实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * let affected = conn
     *     .execute(
     *         "UPDATE threads SET last_activity_at = ?1 WHERE id = ?2",
     *         params![last_activity_at, thread_id],
     *     )
     *     .context("failed to update thread last activity")?;
     * if affected == 0 {
     *     anyhow::bail!("thread not found: {thread_id}");
     * }
     * get_thread(db, thread_id)?.ok_or_else(|| {
     *     anyhow::anyhow!("thread not found after last activity update: {thread_id}")
     * })
     */
    update_thread(db, &update)
}

/// 重新配置尚未启动的会话运行时。
///
/// 仅允许未建立远端会话、没有消息计数且消息表中也没有记录的会话切换 CLI。
/// `message_count` 是冗余统计值，不能作为唯一依据；消息表检查用于阻止统计尚未
/// 刷新时覆盖已有对话历史。
pub fn reconfigure_unstarted_thread_runtime(
    db: &Database,
    thread_id: &str,
    engine_id: &str,
    model_id: &str,
    metadata: Option<&serde_json::Value>,
) -> anyhow::Result<ThreadDto> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to begin unstarted thread runtime reconfiguration transaction")?;
    let metadata_json = metadata.map(serde_json::Value::to_string);
    let updated_count = tx
        .execute(
            "UPDATE threads
             SET engine_id = ?1,
                 model_id = ?2,
                 engine_metadata_json = ?3
             WHERE id = ?4
               AND engine_thread_id IS NULL
               AND message_count = 0
               AND NOT EXISTS (
                 SELECT 1
                 FROM messages
                 WHERE messages.thread_id = threads.id
               )",
            params![engine_id, model_id, metadata_json, thread_id],
        )
        .context("failed to reconfigure unstarted thread runtime")?;

    if updated_count == 0 {
        let exists = tx
            .query_row(
                "SELECT 1 FROM threads WHERE id = ?1",
                params![thread_id],
                |_| Ok(()),
            )
            .optional()
            .context("failed to inspect thread before runtime reconfiguration")?
            .is_some();
        if exists {
            anyhow::bail!("thread has already started and its CLI tool cannot be changed");
        }
        anyhow::bail!("thread not found: {thread_id}");
    }

    let updated = tx
        .query_row(
            "SELECT id, workspace_id, repo_id, engine_id, model_id, engine_thread_id,
                    engine_metadata_json, COALESCE(title, ''), status, message_count,
                    total_tokens, created_at, last_activity_at,
                    plan_mode, send_method, reasoning_effort, permission_mode
             FROM threads
             WHERE id = ?1",
            params![thread_id],
            map_thread_row,
        )
        .context("failed to load reconfigured unstarted thread")?;
    tx.commit()
        .context("failed to commit unstarted thread runtime reconfiguration transaction")?;

    Ok(updated)
}

pub fn delete_thread(db: &Database, thread_id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let affected = conn
        .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
        .context("failed to delete thread")?;

    if affected == 0 {
        anyhow::bail!("thread not found: {thread_id}");
    }

    Ok(())
}

pub fn archive_thread(db: &Database, thread_id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let archived_at = runtime_env::system_time_rfc3339();
    let affected = conn
        .execute(
            "UPDATE threads
       SET archived_at = ?2
       WHERE id = ?1
         AND archived_at IS NULL",
            params![thread_id, archived_at],
        )
        .context("failed to archive thread")?;

    if affected == 0 {
        anyhow::bail!("thread not found or already archived: {thread_id}");
    }

    Ok(())
}

pub fn restore_thread(db: &Database, thread_id: &str) -> anyhow::Result<ThreadDto> {
    let conn = db.connect()?;
    let affected = conn
        .execute(
            "UPDATE threads
       SET archived_at = NULL
       WHERE id = ?1
         AND archived_at IS NOT NULL",
            params![thread_id],
        )
        .context("failed to restore thread")?;

    if affected == 0 {
        anyhow::bail!("thread not found or not archived: {thread_id}");
    }

    get_thread(db, thread_id)?
        .ok_or_else(|| anyhow::anyhow!("thread not found after restore: {thread_id}"))
}

pub fn update_engine_metadata(
    db: &Database,
    thread_id: &str,
    metadata: &serde_json::Value,
) -> anyhow::Result<()> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        engine_metadata_json: Some(metadata.to_string()),
        ..Default::default()
    };

    /*
     * 旧实现保留如下，实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * conn.execute(
     *     "UPDATE threads SET engine_metadata_json = ?1 WHERE id = ?2",
     *     params![metadata.to_string(), thread_id],
     * )
     * .context("failed to update engine metadata")?;
     */
    update_thread(db, &update)?;
    Ok(())
}

/// 持久化 CLI 原始权限；非权限 metadata 不由该接口改写。
pub fn update_thread_permissions(
    db: &Database,
    thread_id: &str,
    permission_mode: Option<&str>,
) -> anyhow::Result<ThreadDto> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        permission_mode: permission_mode.map(str::to_owned),
        ..Default::default()
    };

    /*
     * 旧事务实现保留如下，实际执行路径已统一转入 update_thread：
     *
     * let mut conn = db.connect()?;
     * let tx = conn.transaction()?;
     * let affected = tx.execute(
     *     "UPDATE threads
     *      SET permission_mode = ?1
     *      WHERE id = ?3",
     *     params![permission_mode, thread_id],
     * )?;
     * if affected == 0 {
     *     anyhow::bail!("thread not found: {thread_id}");
     * }
     * tx.commit()?;
     */
    update_thread(db, &update)
}

/// 按局部更新 DTO 更新 threads 表；NULL 保留原值，空字符串照常持久化。
pub fn update_thread(db: &Database, update: &ThreadUpdateDto) -> anyhow::Result<ThreadDto> {
    let conn = db.connect()?;
    let affected = conn
        .execute(
            "UPDATE threads
             SET workspace_id = COALESCE(?1, workspace_id),
                 repo_id = COALESCE(?2, repo_id),
                 engine_id = COALESCE(?3, engine_id),
                 model_id = COALESCE(?4, model_id),
                 engine_thread_id = COALESCE(?5, engine_thread_id),
                 engine_metadata_json = COALESCE(?6, engine_metadata_json),
                 engine_capabilities_json = COALESCE(?7, engine_capabilities_json),
                 title = COALESCE(?8, title),
                 status = COALESCE(?9, status),
                 archived_at = COALESCE(?10, archived_at),
                 message_count = COALESCE(?11, message_count),
                 total_tokens = COALESCE(?12, total_tokens),
                 created_at = COALESCE(?13, created_at),
                 last_activity_at = COALESCE(?14, last_activity_at),
                 plan_mode = COALESCE(?15, plan_mode),
                 send_method = COALESCE(?16, send_method),
                 reasoning_effort = COALESCE(?17, reasoning_effort),
                 permission_mode = COALESCE(?18, permission_mode)
             WHERE id = ?19",
            params![
                update.workspace_id,
                update.repo_id,
                update.engine_id,
                update.model_id,
                update.engine_thread_id,
                update.engine_metadata_json,
                update.engine_capabilities_json,
                update.title,
                update.status,
                update.archived_at,
                update.message_count,
                update.total_tokens,
                update.created_at,
                update.last_activity_at,
                update.plan_mode,
                update.send_method,
                update.reasoning_effort,
                update.permission_mode,
                update.id,
            ],
        )
        .context("failed to update thread")?;
    if affected == 0 {
        anyhow::bail!("thread not found: {}", update.id);
    }

    get_thread(db, &update.id)?
        .ok_or_else(|| anyhow::anyhow!("thread not found after update: {}", update.id))
}

/// 持久化会话底部 6 项运行时选择。
///
/// 调用方传入什么就写入什么，不做回退、不与 engine_metadata_json 合并，
/// 保证底部状态以独立字段为唯一数据源。
#[allow(clippy::too_many_arguments)]
pub fn update_thread_runtime_selection(
    db: &Database,
    thread_id: &str,
    engine_id: &str,
    model_id: &str,
    plan_mode: Option<bool>,
    send_method: Option<&str>,
    reasoning_effort: Option<&str>,
    permission_mode: Option<&str>,
) -> anyhow::Result<ThreadDto> {
    // 旧参数保留兼容；权限现在由 update_thread_permissions 原子保存。
    /*
     * 旧运行时选择 SQL 曾包含以下权限列写入，保留迁移记录但不再执行：
     *
     * UPDATE threads
     * SET engine_id = ?1, model_id = ?2, plan_mode = ?3, send_method = ?4,
     *     reasoning_effort = ?5, permission_mode = ?6
     * WHERE id = ?7
     *
     * 权限原始 JSON 和 engine_metadata_json 现在只能由
     * update_thread_permissions 在同一事务中保存；本函数只负责底部运行时选择。
     */
    let _ = permission_mode;
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        engine_id: Some(engine_id.to_owned()),
        model_id: Some(model_id.to_owned()),
        plan_mode,
        send_method: send_method.map(str::to_owned),
        reasoning_effort: reasoning_effort.map(str::to_owned),
        ..Default::default()
    };

    /*
     * 旧直接 UPDATE 保留如下，实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * let affected = conn.execute(
     *     "UPDATE threads
     *      SET engine_id = ?1, model_id = ?2, plan_mode = ?3,
     *          send_method = ?4, reasoning_effort = ?5
     *      WHERE id = ?6",
     *     params![engine_id, model_id, plan_mode, send_method, reasoning_effort, thread_id],
     * )?;
     * if affected == 0 {
     *     anyhow::bail!("thread not found: {thread_id}");
     * }
     * get_thread(db, thread_id)?.ok_or_else(|| {
     *     anyhow::anyhow!("thread not found after runtime selection update: {thread_id}")
     * })
     */
    update_thread(db, &update)
}

pub fn bump_message_counters(
    db: &Database,
    thread_id: &str,
    tokens: Option<(u64, u64)>,
) -> anyhow::Result<()> {
    let (input, output) = tokens.unwrap_or((0, 0));
    let conn = db.connect()?;
    let updated_at = runtime_env::system_time_rfc3339();
    conn.execute(
        "UPDATE threads
     SET message_count = message_count + 1,
         total_tokens = total_tokens + ?1 + ?2,
         last_activity_at = ?3
     WHERE id = ?4",
        params![input as i64, output as i64, updated_at, thread_id],
    )
    .context("failed to bump thread counters")?;
    Ok(())
}

pub fn update_thread_title(db: &Database, thread_id: &str, title: &str) -> anyhow::Result<()> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        title: Some(title.to_owned()),
        ..Default::default()
    };

    /*
     * 旧实现保留如下，实际执行路径已统一转入 update_thread：
     *
     * let conn = db.connect()?;
     * conn.execute(
     *     "UPDATE threads SET title = ?1 WHERE id = ?2",
     *     params![title, thread_id],
     * )
     * .context("failed to update thread title")?;
     */
    update_thread(db, &update)?;
    Ok(())
}

/// 按指定活动时间策略重算线程消息数量和 Token 统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadMessageStatsActivityPolicy {
    /// 根据最新消息时间更新线程活动时间；无消息时使用当前 RFC3339 时间。
    FromMessages,
    /// 只更新消息数量和 Token 统计，保留线程已有活动时间。
    PreserveExisting,
}

/// 按指定活动时间策略重算线程消息数量和 Token 统计。
pub fn refresh_thread_message_stats(
    db: &Database,
    thread_id: &str,
    activity_policy: ThreadMessageStatsActivityPolicy,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let (message_count, total_tokens, latest_message_at): (i64, i64, Option<String>) = conn
        .query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(COALESCE(token_input, 0) + COALESCE(token_output, 0)), 0),
                MAX(created_at)
             FROM messages
             WHERE thread_id = ?1",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("failed to recalculate thread message stats")?;

    // 旧逻辑无条件使用 latest_message_at.unwrap_or_else(runtime_env::system_time_rfc3339) 更新活动时间，现由策略分支替代。
    let last_activity_at = match activity_policy {
        ThreadMessageStatsActivityPolicy::FromMessages => {
            Some(latest_message_at.unwrap_or_else(runtime_env::system_time_rfc3339))
        }
        ThreadMessageStatsActivityPolicy::PreserveExisting => None,
    };
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        message_count: Some(message_count),
        total_tokens: Some(total_tokens),
        last_activity_at,
        ..Default::default()
    };

    /*
     * 旧直接 UPDATE 保留如下，实际执行路径已统一转入 update_thread：
     *
     * conn.execute(
     *     "UPDATE threads
     *      SET message_count = ?1, total_tokens = ?2,
     *          last_activity_at = COALESCE(?3, ?4)
     *      WHERE id = ?5",
     *     params![
     *         message_count,
     *         total_tokens,
     *         latest_message_at,
     *         runtime_env::system_time_rfc3339(),
     *         thread_id
     *     ],
     * )
     * .context("failed to persist recalculated thread message stats")?;
     */
    update_thread(db, &update)?;

    Ok(())
}

pub fn update_thread_runtime_snapshot(
    db: &Database,
    thread_id: &str,
    title: Option<&str>,
    status: Option<ThreadStatusDto>,
    metadata: Option<&serde_json::Value>,
) -> anyhow::Result<ThreadDto> {
    let existing = get_thread(db, thread_id)?
        .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;

    if let Some(title) =
        title.filter(|_| !thread_manual_title_locked(existing.engine_metadata.as_ref()))
    {
        update_thread_title(db, thread_id, title)?;
    }
    if let Some(status) = status {
        update_thread_status(db, thread_id, status)?;
    }
    if let Some(metadata) = metadata {
        update_engine_metadata(db, thread_id, metadata)?;
    }
    get_thread(db, thread_id)?.ok_or_else(|| {
        anyhow::anyhow!("thread not found after runtime snapshot update: {thread_id}")
    })
}

fn thread_manual_title_locked(metadata: Option<&serde_json::Value>) -> bool {
    metadata
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get("manualTitle"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub fn reconcile_runtime_state(db: &Database) -> anyhow::Result<RuntimeRecoveryReport> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start runtime recovery transaction")?;

    let messages_marked_interrupted = tx
        .execute(
            "UPDATE messages
       SET status = 'interrupted'
       WHERE role = 'assistant'
         AND status = 'streaming'",
            [],
        )
        .context("failed to normalize stale streaming assistant messages")?;

    let thread_ids = {
        let mut stmt = tx
            .prepare("SELECT id FROM threads")
            .context("failed to load threads for runtime recovery")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to iterate threads for runtime recovery")?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("failed to decode thread id during runtime recovery")?);
        }
        out
    };

    let mut thread_status_updates = 0usize;
    for thread_id in thread_ids {
        let next_status = derive_thread_status_for_recovery(&tx, &thread_id)?;
        let changed = tx
            .execute(
                "UPDATE threads
           SET status = ?1
           WHERE id = ?2
             AND status != ?1",
                params![next_status.as_str(), thread_id],
            )
            .context("failed to apply runtime recovery thread status")?;
        thread_status_updates += changed;
    }

    tx.commit()
        .context("failed to commit runtime recovery transaction")?;

    Ok(RuntimeRecoveryReport {
        messages_marked_interrupted,
        thread_status_updates,
    })
}

fn derive_thread_status_for_recovery(
    conn: &rusqlite::Connection,
    thread_id: &str,
) -> anyhow::Result<ThreadStatusDto> {
    let has_pending_approval = conn
        .query_row(
            "SELECT 1
       FROM approvals
       WHERE thread_id = ?1
         AND status = 'pending'
       LIMIT 1",
            params![thread_id],
            |_| Ok(()),
        )
        .optional()
        .context("failed to inspect pending approvals during runtime recovery")?
        .is_some();

    if has_pending_approval {
        return Ok(ThreadStatusDto::AwaitingApproval);
    }

    let last_assistant_status = conn
        .query_row(
            "SELECT status
       FROM messages
       WHERE thread_id = ?1
         AND role = 'assistant'
       ORDER BY created_at DESC, rowid DESC
       LIMIT 1",
            params![thread_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to inspect latest assistant message during runtime recovery")?;

    let status = match last_assistant_status.as_deref() {
        Some("error") => ThreadStatusDto::Error,
        Some("completed") => ThreadStatusDto::Completed,
        Some("streaming") | Some("interrupted") => ThreadStatusDto::Idle,
        _ => ThreadStatusDto::Idle,
    };

    Ok(status)
}

fn map_thread_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadDto> {
    let metadata_raw: Option<String> = row.get(6)?;
    let metadata = metadata_raw.and_then(|raw| serde_json::from_str(&raw).ok());

    Ok(ThreadDto {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        repo_id: row.get(2)?,
        engine_id: row.get(3)?,
        model_id: row.get(4)?,
        engine_thread_id: row.get(5)?,
        engine_metadata: metadata,
        plan_mode: row.get(13)?,
        send_method: row.get(14)?,
        reasoning_effort: row.get(15)?,
        permission_mode: row.get(16)?,
        title: row.get(7)?,
        status: ThreadStatusDto::from_str(&row.get::<_, String>(8)?),
        message_count: row.get(9)?,
        total_tokens: row.get(10)?,
        created_at: row.get(11)?,
        last_activity_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use uuid::Uuid;

    use crate::db::{messages, workspaces, ConnectionPool, SQLITE_POOL_MAX_IDLE};
    use super::*;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-threads-{}.db", Uuid::new_v4()));
        let db = Database {
            path,
            pool: Arc::new(ConnectionPool {
                idle: Mutex::new(Vec::new()),
                max_idle: SQLITE_POOL_MAX_IDLE,
            }),
        };
        db.run_migrations().expect("failed to run test migrations");
        db
    }

    fn test_thread(db: &Database, title: &str) -> ThreadDto {
        let root = std::env::temp_dir().join(format!("auracoder-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp workspace root");
        let workspace =
            workspaces::upsert_workspace(db, root.to_string_lossy().as_ref(), Some(1)).unwrap();
        create_thread(db, &workspace.id, None, "codex", "gpt-5.3-codex", title).unwrap()
    }

    #[test]
    fn update_thread_runtime_snapshot_preserves_manual_title() {
        let db = test_db();
        let thread = test_thread(&db, "Manual title");
        let manual_metadata = json!({
            "manualTitle": true,
            "manualTitleUpdatedAt": "2026-03-06T12:00:00Z",
        });
        update_engine_metadata(&db, &thread.id, &manual_metadata).unwrap();

        let updated = update_thread_runtime_snapshot(
            &db,
            &thread.id,
            Some("Engine renamed title"),
            Some(ThreadStatusDto::Idle),
            Some(&json!({
                "manualTitle": true,
                "codexThreadStatus": "idle",
                "codexSyncRequired": false,
            })),
        )
        .unwrap();

        assert_eq!(updated.title, "Manual title");
        assert_eq!(updated.status, ThreadStatusDto::Idle);
        assert_eq!(
            updated
                .engine_metadata
                .as_ref()
                .and_then(|value| value.get("codexThreadStatus"))
                .and_then(serde_json::Value::as_str),
            Some("idle")
        );
        assert_eq!(
            updated
                .engine_metadata
                .as_ref()
                .and_then(|value| value.get("manualTitle"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn update_thread_last_activity_persists_exact_timestamp() {
        let db = test_db();
        let thread = test_thread(&db, "Timestamp");
        let expected_last_activity_at = "2026-08-19T12:33:53+00:00";

        let updated =
            update_thread_last_activity(&db, &thread.id, expected_last_activity_at).unwrap();

        assert_eq!(updated.last_activity_at, expected_last_activity_at);
        assert_eq!(updated.created_at, thread.created_at);

        let persisted = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(persisted.last_activity_at, expected_last_activity_at);
        assert_eq!(persisted.created_at, thread.created_at);
    }

    #[test]
    fn update_thread_last_activity_rejects_missing_thread() {
        let db = test_db();
        let error = update_thread_last_activity(&db, "missing-thread", "2026-08-19T12:33:53+00:00")
            .expect_err("missing thread must not update successfully");

        assert!(error
            .to_string()
            .contains("thread not found: missing-thread"));
    }

    #[test]
    fn ssh_remote_snapshot_preserves_local_runtime_selection_metadata() {
        let db = test_db();
        let thread = test_thread(&db, "Remote thread");
        set_engine_thread_id(&db, &thread.id, "remote-thread-1").unwrap();
        update_engine_metadata(
            &db,
            &thread.id,
            &json!({
                "codexRemote": {"version": "old"},
            }),
        )
        .unwrap();
        update_thread(
            &db,
            &ThreadUpdateDto {
                id: thread.id.clone(),
                reasoning_effort: Some("high".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let updated = upsert_ssh_remote_thread_snapshot(
            &db,
            &thread.workspace_id,
            "codex",
            "remote-thread-1",
            "openai",
            "Refreshed remote title",
            ThreadStatusDto::Idle,
            &json!({
                "sshRemote": true,
                "codexRemoteCwd": "/var/work/project",
                "codexRemote": {"version": "new"},
            }),
            Some("medium"),
            Some("2026-08-14T12:00:00Z"),
        )
        .unwrap();

        let metadata = updated.engine_metadata.unwrap();
        assert_eq!(updated.model_id, "gpt-5.3-codex");
        assert_eq!(updated.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            metadata.get("codexRemote"),
            Some(&json!({"version": "new"}))
        );
        assert_eq!(metadata.get("sshRemote"), Some(&json!(true)));
    }

    #[test]
    fn reconfigure_unstarted_thread_runtime_reuses_the_same_empty_thread() {
        let db = test_db();
        let thread = test_thread(&db, "New thread");
        let metadata = json!({"opencodePermissionMode": "ask"});

        let updated = reconfigure_unstarted_thread_runtime(
            &db,
            &thread.id,
            "opencode",
            "opencode-model",
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(updated.id, thread.id);
        assert_eq!(updated.title, "New thread");
        assert_eq!(updated.engine_id, "opencode");
        assert_eq!(updated.model_id, "opencode-model");
        assert_eq!(updated.engine_metadata, Some(metadata));
    }

    #[test]
    fn reconfigure_unstarted_thread_runtime_rejects_started_threads() {
        let db = test_db();
        let remote_thread = test_thread(&db, "Remote thread");
        set_engine_thread_id(&db, &remote_thread.id, "remote-thread-1").unwrap();

        let remote_error = reconfigure_unstarted_thread_runtime(
            &db,
            &remote_thread.id,
            "opencode",
            "opencode-model",
            None,
        )
        .unwrap_err();
        assert!(remote_error
            .to_string()
            .contains("thread has already started"));

        let message_thread = test_thread(&db, "Message thread");
        messages::insert_user_message(
            &db,
            &message_thread.id,
            "Do not change this CLI",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();

        let stale_counter_thread = get_thread(&db, &message_thread.id).unwrap().unwrap();
        assert_eq!(stale_counter_thread.message_count, 0);

        let message_error = reconfigure_unstarted_thread_runtime(
            &db,
            &message_thread.id,
            "opencode",
            "opencode-model",
            None,
        )
        .unwrap_err();
        assert!(message_error
            .to_string()
            .contains("thread has already started"));
    }

    #[test]
    fn update_thread_permissions_persists_raw_without_changing_metadata() {
        let db = test_db();
        let thread = test_thread(&db, "Permissions");
        let raw = r#"{"permissionMode":"standard","sandboxMode":"workspace-write"}"#;
        let original_metadata = json!({"unrelated": true});
        update_engine_metadata(&db, &thread.id, &original_metadata).unwrap();

        /*
        // 旧权限 metadata 镜像写入已停用：
        let metadata = json!({
            "claudePermissionMode": "standard",
            "sandboxMode": "workspace-write",
        });
        let updated = update_thread_permissions(&db, &thread.id, Some(raw), &metadata).unwrap();
        assert_eq!(updated.engine_metadata, Some(metadata.clone()));
        */

        let updated = update_thread_permissions(&db, &thread.id, Some(raw)).unwrap();

        assert_eq!(updated.permission_mode.as_deref(), Some(raw));
        assert_eq!(updated.engine_metadata, Some(original_metadata.clone()));
        let persisted = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(persisted.permission_mode.as_deref(), Some(raw));
        assert_eq!(persisted.engine_metadata, Some(original_metadata));
    }

    #[test]
    fn update_thread_permissions_rejects_missing_thread() {
        let db = test_db();
        let error = update_thread_permissions(
            &db,
            "missing-thread",
            Some("{}"),
            /* 旧第四参数 metadata 已停用：&json!({"permission": "missing"}), */
        )
        .expect_err("missing thread must not update successfully");

        assert!(error
            .to_string()
            .contains("thread not found: missing-thread"));
        assert!(get_thread(&db, "missing-thread").unwrap().is_none());
    }

    #[test]
    fn update_thread_permissions_does_not_change_another_thread_on_failure() {
        let db = test_db();
        let thread = test_thread(&db, "Permissions unchanged");
        let original_metadata = json!({"original": true});
        update_engine_metadata(&db, &thread.id, &original_metadata).unwrap();

        let _ = update_thread_permissions(
            &db,
            "missing-thread",
            Some(r#"{"changed":true}"#),
            /* 旧第四参数 metadata 已停用：&json!({"changed": true}), */
        )
        .expect_err("missing thread must roll back the failed transaction");

        let unchanged = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(unchanged.permission_mode, thread.permission_mode);
        assert_eq!(unchanged.engine_metadata, Some(original_metadata));
    }

    /// 验证按消息时间策略重算消息数量、Token 和活动时间。
    #[test]
    fn refresh_thread_message_stats_recomputes_counters_from_messages() {
        let db = test_db();
        let thread = test_thread(&db, "Stats");
        let old_last_activity_at = "2026-01-01T00:00:00+00:00";
        update_thread_last_activity(&db, &thread.id, old_last_activity_at).unwrap();
        messages::insert_user_message(
            &db,
            &thread.id,
            "Count this turn",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        let assistant = messages::insert_assistant_placeholder(
            &db,
            &thread.id,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        messages::complete_assistant_message(
            &db,
            &assistant.id,
            crate::models::MessageStatusDto::Completed,
            Some((13, 21)),
            Some("gpt-5.3-codex"),
        )
        .unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::FromMessages,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 2);
        assert_eq!(refreshed.total_tokens, 34);
        assert!(!refreshed.last_activity_at.is_empty());
        assert_ne!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证无消息时仅重算统计不会改写已有活动时间。
    #[test]
    fn refresh_thread_message_stats_preserves_existing_activity_without_messages() {
        let db = test_db();
        let thread = test_thread(&db, "Empty stats");
        let old_last_activity_at = "2026-01-02T00:00:00+00:00";
        update_thread_last_activity(&db, &thread.id, old_last_activity_at).unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::PreserveExisting,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 0);
        assert_eq!(refreshed.total_tokens, 0);
        assert_eq!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证有消息时仅重算统计不会改写已有活动时间。
    #[test]
    fn refresh_thread_message_stats_preserves_existing_activity_while_recomputing_counters() {
        let db = test_db();
        let thread = test_thread(&db, "Preserved stats");
        let old_last_activity_at = "2026-01-03T00:00:00+00:00";
        update_thread_last_activity(&db, &thread.id, old_last_activity_at).unwrap();
        messages::insert_user_message(
            &db,
            &thread.id,
            "Count this turn",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        let assistant = messages::insert_assistant_placeholder(
            &db,
            &thread.id,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        messages::complete_assistant_message(
            &db,
            &assistant.id,
            crate::models::MessageStatusDto::Completed,
            Some((13, 21)),
            Some("gpt-5.3-codex"),
        )
        .unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::PreserveExisting,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 2);
        assert_eq!(refreshed.total_tokens, 34);
        assert_eq!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证只更新线程状态时保留已有活动时间。
    #[test]
    fn update_thread_preserves_last_activity_when_only_status_changes() {
        let db = test_db();
        let thread = test_thread(&db, "Status only");
        let old_last_activity_at = "2026-01-04T00:00:00+00:00";
        update_thread_last_activity(&db, &thread.id, old_last_activity_at).unwrap();

        let updated = update_thread(
            &db,
            &ThreadUpdateDto {
                // 目标线程标识，确保只更新本条会话。
                id: thread.id.clone(),
                // 远端同步后的新运行状态。
                status: Some(ThreadStatusDto::AwaitingApproval.as_str().to_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(updated.status, ThreadStatusDto::AwaitingApproval);
        assert_eq!(updated.last_activity_at, old_last_activity_at);
    }

    #[test]
    fn list_threads_for_workspace_includes_engine_backed_threads_without_messages() {
        let db = test_db();
        let visible = test_thread(&db, "Remote");
        let hidden = test_thread(&db, "Hidden");
        set_engine_thread_id(&db, &visible.id, "codex-thread-123").unwrap();

        let listed = list_threads_for_workspace(&db, &visible.workspace_id).unwrap();
        let listed_ids = listed
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();

        assert!(listed_ids.contains(&visible.id));
        assert!(!listed_ids.contains(&hidden.id));
    }

    #[test]
    fn list_archived_threads_for_workspace_includes_engine_backed_threads_without_messages() {
        let db = test_db();
        let visible = test_thread(&db, "Archived remote");
        let hidden = test_thread(&db, "Archived hidden");
        set_engine_thread_id(&db, &visible.id, "codex-thread-archived").unwrap();
        archive_thread(&db, &visible.id).unwrap();
        archive_thread(&db, &hidden.id).unwrap();

        let listed = list_archived_threads_for_workspace(&db, &visible.workspace_id).unwrap();
        let listed_ids = listed
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();

        assert!(listed_ids.contains(&visible.id));
        assert!(!listed_ids.contains(&hidden.id));
    }
}
