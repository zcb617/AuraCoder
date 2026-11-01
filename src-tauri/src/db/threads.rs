use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::models::{PermissionComponentJson, ThreadDto, ThreadStatusDto, ThreadUpdateDto};
use crate::runtime_env;

use super::Database;

#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeRecoveryReport {
    pub messages_marked_interrupted: usize,
    pub thread_status_updates: usize,
}

/// 启动恢复所需的线程状态事实，由 Mapper 读取并交给 Service 决策。
#[derive(Debug, Clone)]
pub(crate) struct RuntimeRecoveryFact {
    /// 需要恢复的本地线程标识。
    pub(crate) thread_id: String,
    /// 线程是否仍有等待处理的审批。
    pub(crate) has_pending_approval: bool,
    /// 按原有时间和 rowid 顺序选出的最后一条 assistant 消息状态。
    pub(crate) last_assistant_status: Option<String>,
}

/// 运行时恢复远端线程时返回的结构化结果。
#[derive(Debug, Clone)]
pub enum RuntimeThreadRestoreResult {
    /// 线程从归档状态恢复后返回最新 DTO。
    Updated(ThreadDto),
    /// 线程本来就处于活动状态并返回当前 DTO。
    AlreadyActive(ThreadDto),
    /// 指定 Codex 远端线程不存在对应的本地记录。
    NotFound,
}

/// 创建线程记录，并立即保存完整的统一默认权限对象 A。
pub fn create_thread(
    db: &Database,
    workspace_id: &str,
    engine_id: &str,
    model_id: &str,
    title: &str,
) -> anyhow::Result<ThreadDto> {
    let id = Uuid::new_v4().to_string();
    create_thread_with_id(db, &id, workspace_id, engine_id, model_id, title)
}

/// 使用调用方提供的线程标识创建线程记录，并保存统一默认权限对象 A。
pub fn create_thread_with_id(
    db: &Database,
    thread_id: &str,
    workspace_id: &str,
    engine_id: &str,
    model_id: &str,
    title: &str,
) -> anyhow::Result<ThreadDto> {
    let created_at = runtime_env::system_time_rfc3339();
    let permission_mode = serde_json::to_string(&PermissionComponentJson::default())?;
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO threads (
            id, workspace_id, engine_id, model_id, title, status, created_at, last_activity_at, permission_mode
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'idle', ?6, ?6, ?7)",
        params![thread_id, workspace_id, engine_id, model_id, title, created_at, permission_mode],
    )
    .context("failed to create thread")?;

    get_thread(db, thread_id)?.context("thread not found after insert")
}

/// 读取指定线程的 Claude 完整历史同步标记，供点击 Claude 会话时决定是否读取消息。
pub fn get_claude_sync_required(db: &Database, thread_id: &str) -> anyhow::Result<bool> {
    let conn = db.connect()?;
    let sync_required: Option<i64> = conn
        .query_row(
            "SELECT claude_sync_required FROM threads WHERE id = ?1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to query Claude thread sync flag")?;
    sync_required
        .map(|value| value != 0)
        .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))
}

/// 更新指定线程的 Claude 完整历史同步标记；线程不存在时返回明确错误。
pub fn set_claude_sync_required(
    db: &Database,
    thread_id: &str,
    sync_required: bool,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let affected = conn
        .execute(
            "UPDATE threads SET claude_sync_required = ?1 WHERE id = ?2",
            params![if sync_required { 1_i64 } else { 0_i64 }, thread_id],
        )
        .context("failed to update Claude thread sync flag")?;
    if affected == 0 {
        anyhow::bail!("thread not found: {thread_id}");
    }
    Ok(())
}

/// 按精确线程 ID 读取线程及统一权限对象 A。
pub fn get_thread(db: &Database, thread_id: &str) -> anyhow::Result<Option<ThreadDto>> {
    let conn = db.connect()?;
    get_thread_on_connection(&conn, thread_id)
}

/// 使用调用方提供的连接读取线程，复用完整的权限对象 A 行映射。
pub(super) fn get_thread_on_connection(
    conn: &rusqlite::Connection,
    thread_id: &str,
) -> anyhow::Result<Option<ThreadDto>> {
    conn.query_row(
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
                COALESCE(title, ''), status, message_count, total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, COALESCE(title, ''), status, message_count,
                total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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
    let fallback_last_activity = runtime_env::system_time_rfc3339();
    let last_activity = match last_activity_at {
        Some(raw) if !raw.trim().is_empty() => match runtime_env::normalize_time_to_local(raw) {
            Ok(value) => value,
            Err(error) => {
                log::warn!(
                    "SSH 远端线程时间归一化失败，使用当前本地时间: raw={} workspace_id={} engine_thread_id={} error={error:#}",
                    raw,
                    workspace_id,
                    engine_thread_id
                );
                fallback_last_activity.clone()
            }
        },
        _ => fallback_last_activity.clone(),
    };
    if let Some((thread_id, current_model, current_metadata_raw, current_reasoning_effort)) =
        existing
    {
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
        let remote_reasoning_effort =
            current_reasoning_effort.or_else(|| reasoning_effort.map(str::to_owned));
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
                id, workspace_id, engine_id, model_id, engine_thread_id,
                engine_metadata_json, title, status, last_activity_at, created_at,
                reasoning_effort
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
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

/// Claude 刷新事务中供 Service 执行业务判断的既有线程字段。
pub struct SessionRefreshExistingRecord {
    /// 既有线程主键，用于后续更新目标。
    pub id: String,
    /// 数据库中保存的最近活动时间，用于与远端时间比较。
    pub last_activity_at: Option<String>,
    /// 数据库中保存的引擎元数据原文，用于保留用户维护字段。
    pub engine_metadata_json: Option<String>,
    /// 数据库中保存的会话标题，用于保护手工标题。
    pub title: Option<String>,
    /// Claude 完整历史同步标记，用于保留已有同步状态。
    pub claude_sync_required: i64,
}

/// Claude 新建线程时由 Service 决定并提交的完整 INSERT 字段。
pub struct SessionRefreshInsertRecord<'a> {
    /// 新线程的主键，由 Claude Service 生成。
    pub id: &'a str,
    /// 新线程所属的 workspace 标识。
    pub workspace_id: &'a str,
    /// 新线程所属的引擎标识。
    pub engine_id: &'a str,
    /// 新线程采用的模型标识。
    pub model_id: &'a str,
    /// 引擎侧原生会话标识。
    pub engine_thread_id: &'a str,
    /// 远端会话元数据 JSON。
    pub engine_metadata_json: &'a str,
    /// 远端会话标题。
    pub title: &'a str,
    /// 远端会话状态。
    pub status: &'a str,
    /// 远端会话思考强度。
    pub reasoning_effort: Option<&'a str>,
    /// 远端活动时间已转换成本地时区后的值。
    pub last_activity_at: &'a str,
    /// Service 创建的新线程时间。
    pub created_at: &'a str,
    /// Service 判断的新线程完整历史同步标记。
    pub claude_sync_required: i64,
}

/// 在单个 SQLite 事务中执行 Claude 会话刷新读写，保证整批快照原子提交。
pub fn with_session_refresh_transaction<T>(
    db: &Database,
    work: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut conn = db.connect().context("failed connect Claude refresh database")?;
    let tx = conn
        .transaction()
        .context("failed begin Claude remote thread import transaction")?;
    // 先在事务内完成整批业务读写，再一次性提交所有 Claude 会话快照。
    let result = work(&tx)?;
    tx.commit()
        .context("failed commit Claude remote thread import transaction")?;
    Ok(result)
}

/// 按 workspace、引擎和原生会话标识读取 Claude 刷新所需的既有线程字段。
pub fn find_session_refresh_record(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    engine_id: &str,
    engine_thread_id: &str,
) -> anyhow::Result<Option<SessionRefreshExistingRecord>> {
    tx.query_row(
        "SELECT id, last_activity_at, engine_metadata_json, title, claude_sync_required
         FROM threads
         WHERE workspace_id = ?1 AND engine_id = ?2 AND engine_thread_id = ?3
         LIMIT 1",
        params![workspace_id, engine_id, engine_thread_id],
        |row| {
            Ok(SessionRefreshExistingRecord {
                id: row.get(0)?,
                last_activity_at: row.get(1)?,
                engine_metadata_json: row.get(2)?,
                title: row.get(3)?,
                claude_sync_required: row.get(4)?,
            })
        },
    )
    .optional()
    .context("failed query existing Claude remote thread")
}

/// 插入由 Claude Service 完整决策的远端线程快照，不在 Mapper 中补默认业务值。
pub fn insert_session_refresh_record(
    tx: &rusqlite::Transaction<'_>,
    record: &SessionRefreshInsertRecord<'_>,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO threads (
             id, workspace_id, engine_id, model_id, engine_thread_id,
             engine_metadata_json, title, status, reasoning_effort,
             last_activity_at, created_at, claude_sync_required
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.id,
            record.workspace_id,
            record.engine_id,
            record.model_id,
            record.engine_thread_id,
            record.engine_metadata_json,
            record.title,
            record.status,
            record.reasoning_effort,
            record.last_activity_at,
            record.created_at,
            record.claude_sync_required,
        ],
    )
    .context("failed insert Claude remote thread snapshot")?;
    Ok(())
}

/// 更新 Claude Service 已完成决策的四个可变快照字段。
pub fn update_session_refresh_record(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
    title: Option<&str>,
    metadata_json: &str,
    last_activity_at: &str,
    sync_required: i64,
) -> anyhow::Result<()> {
    tx.execute(
        "UPDATE threads
         SET title = ?1,
             engine_metadata_json = ?2,
             last_activity_at = ?3,
             claude_sync_required = ?4
         WHERE id = ?5",
        params![title, metadata_json, last_activity_at, sync_required, thread_id],
    )
    .context("failed update Claude remote thread snapshot")?;
    Ok(())
}

pub fn list_threads_for_workspace(
    db: &Database,
    workspace_id: &str,
) -> anyhow::Result<Vec<ThreadDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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
     ORDER BY julianday(last_activity_at) DESC, rowid DESC",
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
        "SELECT id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
            COALESCE(title, ''), status, message_count, total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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
     ORDER BY julianday(archived_at) DESC, rowid DESC",
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
            "SELECT id, workspace_id, engine_id, model_id, engine_thread_id,
                    engine_metadata_json, COALESCE(title, ''), status, message_count,
                    total_tokens, context_current_tokens, context_max_tokens,
            context_usage_updated_at, created_at, last_activity_at,
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

/// 按引擎线程标识结构化恢复 Codex 运行时线程，不解释前端事件或 CLI 请求。
pub fn restore_runtime_thread(
    db: &Database,
    engine_id: &str,
    engine_thread_id: &str,
) -> anyhow::Result<RuntimeThreadRestoreResult> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start runtime thread restore transaction")?;
    let thread_state = tx
        .query_row(
            "SELECT id, archived_at FROM threads
             WHERE engine_id = ?1 AND engine_thread_id = ?2
             LIMIT 1",
            params![engine_id, engine_thread_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .context("failed to find runtime thread for restore")?;

    let Some((thread_id, archived_at)) = thread_state else {
        tx.commit()
            .context("failed to commit missing runtime thread restore")?;
        return Ok(RuntimeThreadRestoreResult::NotFound);
    };

    // 仅清除归档标记，保留线程 DTO 的其他持久化状态。
    let result = if archived_at.is_some() {
        tx.execute(
            "UPDATE threads SET archived_at = NULL WHERE id = ?1 AND archived_at IS NOT NULL",
            params![thread_id],
        )
        .context("failed to restore archived runtime thread")?;
        RuntimeThreadRestoreResult::Updated(
            get_thread_on_connection(&tx, &thread_id)?
                .ok_or_else(|| anyhow::anyhow!("runtime thread missing after restore: {thread_id}"))?,
        )
    } else {
        RuntimeThreadRestoreResult::AlreadyActive(
            get_thread_on_connection(&tx, &thread_id)?
                .ok_or_else(|| anyhow::anyhow!("runtime thread missing after lookup: {thread_id}"))?,
        )
    };

    tx.commit()
        .context("failed to commit runtime thread restore")?;
    Ok(result)
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

/// 校验并持久化统一权限对象 A；线程权限列不再保存 CLI 原生权限。
pub fn update_thread_permissions(
    db: &Database,
    thread_id: &str,
    permission_mode: Option<&PermissionComponentJson>,
) -> anyhow::Result<ThreadDto> {
    if let Some(permission_mode) = permission_mode {
        permission_mode.validate()?;
    }
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        permission_mode: permission_mode.cloned(),
        ..Default::default()
    };
    update_thread(db, &update)
}

/// 按局部更新 DTO 更新 threads 表；NULL 保留原值，空字符串照常持久化。
pub fn update_thread(db: &Database, update: &ThreadUpdateDto) -> anyhow::Result<ThreadDto> {
    if let Some(permission_mode) = update.permission_mode.as_ref() {
        permission_mode.validate()?;
    }
    let normalized_archived_at = update
        .archived_at
        .as_deref()
        .map(runtime_env::normalize_time_to_local)
        .transpose()?;
    let normalized_created_at = update
        .created_at
        .as_deref()
        .map(runtime_env::normalize_time_to_local)
        .transpose()?;
    let normalized_last_activity_at = update
        .last_activity_at
        .as_deref()
        .map(runtime_env::normalize_time_to_local)
        .transpose()?;
    let normalized_context_usage_updated_at = update
        .context_usage_updated_at
        .as_deref()
        .map(runtime_env::normalize_time_to_local)
        .transpose()?;
    let permission_mode_json = update
        .permission_mode
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let conn = db.connect()?;
    let affected = conn
        .execute(
            "UPDATE threads
             SET workspace_id = COALESCE(?1, workspace_id),
                 engine_id = COALESCE(?2, engine_id),
                 model_id = COALESCE(?3, model_id),
                 engine_thread_id = COALESCE(?4, engine_thread_id),
                 engine_metadata_json = COALESCE(?5, engine_metadata_json),
                 engine_capabilities_json = COALESCE(?6, engine_capabilities_json),
                 title = COALESCE(?7, title),
                 status = COALESCE(?8, status),
                 archived_at = COALESCE(?9, archived_at),
                 message_count = COALESCE(?10, message_count),
                 total_tokens = COALESCE(?11, total_tokens),
                 created_at = COALESCE(?12, created_at),
                 last_activity_at = COALESCE(?13, last_activity_at),
                 plan_mode = COALESCE(?14, plan_mode),
                 send_method = COALESCE(?15, send_method),
                 reasoning_effort = COALESCE(?16, reasoning_effort),
                 permission_mode = COALESCE(?17, permission_mode),
                 context_current_tokens = COALESCE(?18, context_current_tokens),
                 context_max_tokens = COALESCE(?19, context_max_tokens),
                 context_usage_updated_at = COALESCE(?20, context_usage_updated_at)
             WHERE id = ?21",
            params![
                update.workspace_id,
                update.engine_id,
                update.model_id,
                update.engine_thread_id,
                update.engine_metadata_json,
                update.engine_capabilities_json,
                update.title,
                update.status,
                normalized_archived_at,
                update.message_count,
                update.total_tokens,
                normalized_created_at,
                normalized_last_activity_at,
                update.plan_mode,
                update.send_method,
                update.reasoning_effort,
                permission_mode_json,
                update.context_current_tokens,
                update.context_max_tokens,
                normalized_context_usage_updated_at,
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

/// 读取线程持久化的上下文 token 快照，供 CLI 线程上下文展示使用。
///
/// 两个 token 字段都为空时返回 None；线程不存在或 token 无法转换为 u64 时返回错误。
pub fn get_context_usage_snapshot(
    db: &Database,
    thread_id: &str,
) -> anyhow::Result<Option<(Option<u64>, Option<u64>)>> {
    let conn = db.connect()?;
    let snapshot: Option<(Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT context_current_tokens, context_max_tokens
             FROM threads
             WHERE id = ?1",
            params![thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("failed to query thread context usage snapshot")?;
    let Some((current_tokens, max_context_tokens)) = snapshot else {
        anyhow::bail!("thread not found: {thread_id}");
    };
    if current_tokens.is_none() && max_context_tokens.is_none() {
        return Ok(None);
    }

    let current_tokens = current_tokens.map(u64::try_from).transpose()?;
    let max_context_tokens = max_context_tokens.map(u64::try_from).transpose()?;
    Ok(Some((current_tokens, max_context_tokens)))
}

/// 持久化会话底部运行时选择，并接受统一权限对象 A 作为可选更新值。
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
    permission_mode: Option<&PermissionComponentJson>,
) -> anyhow::Result<ThreadDto> {
    let update = ThreadUpdateDto {
        id: thread_id.to_owned(),
        engine_id: Some(engine_id.to_owned()),
        model_id: Some(model_id.to_owned()),
        plan_mode,
        send_method: send_method.map(str::to_owned),
        reasoning_effort: reasoning_effort.map(str::to_owned),
        permission_mode: permission_mode.cloned(),
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
                (
                    SELECT created_at FROM messages
                    WHERE thread_id = ?1
                    ORDER BY julianday(created_at) DESC, rowid DESC
                    LIMIT 1
                )
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

/// 在单个 SQLite 事务中执行启动线程恢复数据操作并提交结果。
pub(crate) fn with_runtime_recovery_transaction<T>(
    db: &Database,
    work: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start runtime recovery transaction")?;
    let result = work(&tx)?;
    tx.commit()
        .context("failed to commit runtime recovery transaction")?;
    Ok(result)
}

/// 将遗留 assistant streaming 消息标记为 interrupted 并返回变更数量。
pub(crate) fn mark_streaming_assistant_messages_interrupted(
    tx: &rusqlite::Transaction<'_>,
) -> anyhow::Result<usize> {
    tx.execute(
        "UPDATE messages
         SET status = 'interrupted'
         WHERE role = 'assistant' AND status = 'streaming'",
        [],
    )
    .context("failed to normalize stale streaming assistant messages")
}

/// 在事务中读取每个线程的 pending 审批和最新 assistant 消息状态。
pub(crate) fn list_runtime_recovery_facts(
    tx: &rusqlite::Transaction<'_>,
) -> anyhow::Result<Vec<RuntimeRecoveryFact>> {
    let mut statement = tx
        .prepare(
            "SELECT t.id,
                    EXISTS(SELECT 1 FROM approvals a WHERE a.thread_id = t.id AND a.status = 'pending'),
                    (SELECT m.status FROM messages m
                     WHERE m.thread_id = t.id AND m.role = 'assistant'
                     ORDER BY julianday(m.created_at) DESC, m.rowid DESC LIMIT 1)
             FROM threads t",
        )
        .context("failed to prepare runtime recovery facts query")?;
    let rows = statement
        .query_map([], |row| {
            Ok(RuntimeRecoveryFact {
                thread_id: row.get(0)?,
                has_pending_approval: row.get(1)?,
                last_assistant_status: row.get(2)?,
            })
        })
        .context("failed to query runtime recovery facts")?;
    let mut facts = Vec::new();
    for row in rows {
        facts.push(row.context("failed to decode runtime recovery facts")?);
    }
    Ok(facts)
}

/// 仅在线程当前状态与业务目标状态不同时写入恢复后的状态。
pub(crate) fn update_recovered_thread_status(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
    target_status: ThreadStatusDto,
) -> anyhow::Result<usize> {
    tx.execute(
        "UPDATE threads SET status = ?1 WHERE id = ?2 AND status != ?1",
        params![target_status.as_str(), thread_id],
    )
    .context("failed to apply runtime recovery thread status")
}

/// 解码线程统一权限对象 A；空值或旧原生格式返回默认 A，并记录原始错误。
pub(crate) fn decode_thread_permission_mode(
    thread_id: &str,
    raw_permission_mode: Option<String>,
) -> Option<PermissionComponentJson> {
    match raw_permission_mode {
        Some(raw) => match serde_json::from_str::<PermissionComponentJson>(&raw) {
            Ok(value) => match value.validate() {
                Ok(()) => Some(value),
                Err(error) => {
                    log::warn!(
                        "线程权限记录不符合统一权限契约，thread_id={} raw={} error={error:#}；展示默认权限且不改写记录",
                        thread_id,
                        raw
                    );
                    Some(PermissionComponentJson::default())
                }
            },
            Err(error) => {
                log::warn!(
                    "线程权限记录无法解析为统一权限对象，thread_id={} raw={} error={error:#}；展示默认权限且不改写记录",
                    thread_id,
                    raw
                );
                Some(PermissionComponentJson::default())
            }
        },
        None => Some(PermissionComponentJson::default()),
    }
}

/// 将数据库线程行转换为 DTO，并把空值或旧原生格式权限显示为默认对象 A。
fn map_thread_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadDto> {
    let thread_id: String = row.get(0)?;
    let metadata_raw: Option<String> = row.get(5)?;
    let metadata = metadata_raw.and_then(|raw| serde_json::from_str(&raw).ok());
    let permission_mode_raw: Option<String> = row.get(18)?;
    let permission_mode = decode_thread_permission_mode(&thread_id, permission_mode_raw);

    Ok(ThreadDto {
        id: thread_id,
        workspace_id: row.get(1)?,
        engine_id: row.get(2)?,
        model_id: row.get(3)?,
        engine_thread_id: row.get(4)?,
        engine_metadata: metadata,
        plan_mode: row.get(15)?,
        send_method: row.get(16)?,
        reasoning_effort: row.get(17)?,
        permission_mode,
        title: row.get(6)?,
        status: ThreadStatusDto::from_str(&row.get::<_, String>(7)?),
        message_count: row.get(8)?,
        total_tokens: row.get(9)?,
        context_current_tokens: row.get(10)?,
        context_max_tokens: row.get(11)?,
        context_usage_updated_at: row.get(12)?,
        created_at: row.get(13)?,
        last_activity_at: row.get(14)?,
    })
}

#[cfg(test)]
#[path="../../tests/unit/thread_mapper_tests.rs"]
mod tests;
