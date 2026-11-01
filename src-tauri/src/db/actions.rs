use anyhow::Context;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use crate::engines::events::{ActionResult, ActionType};
use crate::models::{MessageDto, MessageStatusDto, ThreadDto, ThreadStatusDto};
use crate::runtime_env;

use super::Database;

/// 保存聊天审批答复前所需的持久化归属和路由详情。
#[derive(Debug, Clone)]
pub struct ChatApprovalRecord {
    /// 审批所属线程，用于校验答复目标。
    pub thread_id: String,
    /// 审批所在消息，用于同步审批卡片状态。
    pub message_id: String,
    /// 审批当前持久化状态。
    pub status: String,
    /// 审批已经保存的决定；仍在等待时通常为空。
    pub decision: Option<String>,
    /// 审批路由及业务详情，必须按数据库原文解析。
    pub details: Value,
}

/// 按审批标识读取持久化归属、状态和路由详情。
pub fn get_chat_approval_record(
    db: &Database,
    approval_id: &str,
) -> anyhow::Result<Option<ChatApprovalRecord>> {
    let conn = db.connect()?;
    let record = conn
        .query_row(
            "SELECT thread_id, message_id, status, decision, details_json
             FROM approvals
             WHERE id = ?1",
            params![approval_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .context("failed to load chat approval record")?;

    let Some((thread_id, message_id, status, decision, details_json)) = record else {
        return Ok(None);
    };

    // 保留数据库审批详情的真实解析结果，避免用空对象替代损坏的路由数据。
    let details = serde_json::from_str::<Value>(&details_json)
        .context("failed to parse chat approval details_json")?;

    Ok(Some(ChatApprovalRecord {
        thread_id,
        message_id,
        status,
        decision,
        details,
    }))
}

/// 统计同一线程中除当前审批外仍待处理的审批记录。
pub fn count_other_pending_chat_approvals(
    db: &Database,
    thread_id: &str,
    approval_id: &str,
) -> anyhow::Result<i64> {
    let conn = db.connect()?;
    let count = conn
        .query_row(
            "SELECT COUNT(*)
             FROM approvals
             WHERE thread_id = ?1
               AND id <> ?2
               AND status = 'pending'",
            params![thread_id, approval_id],
            |row| row.get(0),
        )
        .context("failed to count other pending chat approvals")?;
    Ok(count)
}

/// 在单个 SQLite 事务内保存审批、审批消息块、可选消息状态和线程状态。
pub fn settle_chat_approval(
    db: &Database,
    thread_id: &str,
    approval_id: &str,
    status: &str,
    decision: Option<&str>,
    thread_status: ThreadStatusDto,
    message_status: Option<MessageStatusDto>,
    allow_resolved_without_decision: bool,
) -> anyhow::Result<(ThreadDto, MessageDto)> {
    // 只接受 Service 契约规定的两种结算状态，不在 Mapper 推导业务状态。
    if status != "answered" && status != "expired" {
        anyhow::bail!("unsupported chat approval settlement status: {status}");
    }

    let mut conn = db.connect()?;
    let tx = conn.transaction().context("failed to start chat approval transaction")?;

    // 先读取审批并校验归属和 pending 状态，所有校验均发生在任何写入之前。
    let approval = tx
        .query_row(
            "SELECT thread_id, message_id, status, decision
             FROM approvals
             WHERE id = ?1",
            params![approval_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .context("failed to load approval for settlement")?;
    let Some((approval_thread_id, message_id, current_status, current_decision)) = approval else {
        anyhow::bail!("chat approval not found: {approval_id}");
    };
    if approval_thread_id != thread_id {
        anyhow::bail!("chat approval does not belong to thread: {thread_id}");
    }
    let can_complete_resolved_without_decision = allow_resolved_without_decision
        && status == "answered"
        && decision.is_some()
        && current_status == "answered"
        && current_decision.is_none();
    if current_status != "pending" && !can_complete_resolved_without_decision {
        anyhow::bail!("chat approval is not pending: {approval_id}");
    }

    // 验证审批消息仍属于目标线程，并严格解析消息块以避免覆盖损坏数据。
    let message_record = tx
        .query_row(
            "SELECT thread_id, blocks_json
             FROM messages
             WHERE id = ?1",
            params![message_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()
        .context("failed to load approval message for settlement")?;
    let Some((message_thread_id, raw_blocks)) = message_record else {
        anyhow::bail!("chat approval message not found: {message_id}");
    };
    if message_thread_id != thread_id {
        anyhow::bail!("chat approval message does not belong to thread: {thread_id}");
    }
    let raw_blocks = raw_blocks.context("chat approval message has no blocks_json")?;
    let mut blocks = serde_json::from_str::<Value>(&raw_blocks)
        .context("failed to parse chat approval message blocks_json")?;
    let Some(items) = blocks.as_array_mut() else {
        anyhow::bail!("chat approval message blocks_json is not an array");
    };

    // 只改目标审批卡片的既有状态字段，保留其他卡片及 response/details 等业务数据。
    let mut approval_block_found = false;
    for block in items.iter_mut() {
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("approval") {
            continue;
        }
        let block_approval_id = object
            .get("approvalId")
            .and_then(Value::as_str)
            .or_else(|| object.get("approval_id").and_then(Value::as_str));
        if block_approval_id != Some(approval_id) {
            continue;
        }

        approval_block_found = true;
        if status == "answered" {
            if object.get("status").and_then(Value::as_str) != Some("answered") {
                object.insert("status".to_string(), Value::String("answered".to_string()));
            }
            if let Some(decision) = decision {
                if object.get("decision").and_then(Value::as_str) != Some(decision) {
                    object.insert("decision".to_string(), Value::String(decision.to_string()));
                }
            }
        } else {
            object.insert("status".to_string(), Value::String("expired".to_string()));
        }
    }
    if !approval_block_found {
        anyhow::bail!("chat approval block not found in message: {approval_id}");
    }

    // 使用 Service 指定的审批与线程状态受控写入，并验证每条 SQL 恰好更新一行。
    let answered_at = if status == "answered" {
        Some(runtime_env::system_time_rfc3339())
    } else {
        None
    };
    let approval_rows = tx
        .execute(
            "UPDATE approvals
             SET status = ?1,
                 decision = COALESCE(?2, decision),
                 answered_at = CASE WHEN ?1 = 'answered' THEN ?3 ELSE answered_at END
             WHERE id = ?4
               AND thread_id = ?5
               AND (
                   status = 'pending'
                   OR (?6 AND status = 'answered' AND decision IS NULL)
               )",
            params![
                status,
                decision,
                answered_at,
                approval_id,
                thread_id,
                allow_resolved_without_decision
            ],
        )
        .context("failed to settle chat approval")?;
    if approval_rows != 1 {
        anyhow::bail!("chat approval settlement affected {approval_rows} rows");
    }

    // 将可选消息状态按既有 DTO 字符串映射保存；None 时沿用数据库原状态。
    let blocks_json = blocks.to_string();
    let message_status = message_status.map(|status| status.as_str().to_string());
    let message_rows = tx
        .execute(
            "UPDATE messages
             SET blocks_json = ?1,
                 status = COALESCE(?2, status)
             WHERE id = ?3
               AND thread_id = ?4
               AND blocks_json = ?5",
            params![blocks_json, message_status, message_id, thread_id, raw_blocks],
        )
        .context("failed to update chat approval message blocks")?;
    if message_rows != 1 {
        anyhow::bail!("chat approval message update affected {message_rows} rows");
    }

    let thread_rows = tx
        .execute(
            "UPDATE threads SET status = ?1 WHERE id = ?2",
            params![thread_status.as_str(), thread_id],
        )
        .context("failed to update thread status after chat approval")?;
    if thread_rows != 1 {
        anyhow::bail!("chat approval thread update affected {thread_rows} rows");
    }

    // 在同一事务中读取 Mapper 原有 DTO 映射结果，确保返回值对应本次写入。
    let thread = super::threads::get_thread_on_connection(&tx, thread_id)?
        .context("thread not found after chat approval settlement")?;
    let message = super::messages::get_message_on_connection(&tx, &message_id)?
        .context("message not found after chat approval settlement")?;
    tx.commit()
        .context("failed to commit chat approval transaction")?;

    Ok((thread, message))
}

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

/// 使用独立数据库连接保存待审批记录，供现有审批保存流程使用。
#[cfg(test)]
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
    insert_approval_on_connection(
        &conn,
        approval_id,
        thread_id,
        message_id,
        action_type,
        summary,
        details,
    )
}

/// 在调用方提供的连接上保存待审批记录，供审批事务共用同一连接。
#[allow(clippy::too_many_arguments)]
fn insert_approval_on_connection(
    conn: &rusqlite::Connection,
    approval_id: &str,
    thread_id: &str,
    message_id: &str,
    action_type: &ActionType,
    summary: &str,
    details: &Value,
) -> anyhow::Result<()> {
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

/// 在单个事务中保存待审批记录、助手卡片和线程待审批状态。
#[allow(clippy::too_many_arguments)]
pub fn insert_chat_approval(
    db: &Database,
    approval_id: &str,
    thread_id: &str,
    message_id: &str,
    action_type: &ActionType,
    summary: &str,
    details: &Value,
    blocks_json: &str,
    message_status: MessageStatusDto,
    thread_status: ThreadStatusDto,
) -> anyhow::Result<()> {
    // 使用一个连接和事务承载本次审批创建的全部持久化更新。
    let mut conn = db.connect().context("failed to connect for chat approval")?;
    let tx = conn
        .transaction()
        .context("failed to begin chat approval transaction")?;

    // 先在同一事务中写入 pending 审批记录。
    insert_approval_on_connection(
        &tx,
        approval_id,
        thread_id,
        message_id,
        action_type,
        summary,
        details,
    )
    .context("failed to insert chat approval")?;

    // 合并既有已答复审批状态，避免新卡片序列化覆盖用户已完成的答复。
    let normalized_blocks_json = super::messages::normalize_blocks_json_for_message(
        &tx,
        message_id,
        blocks_json,
    )
    .context("failed to normalize chat approval blocks")?;

    // 保存审批卡片及调用方指定的助手消息状态，并确认目标消息属于当前线程。
    let updated_message_rows = tx
        .execute(
            "UPDATE messages SET blocks_json=?1,status=?2 WHERE id=?3 AND thread_id=?4",
            params![
                normalized_blocks_json,
                message_status.as_str(),
                message_id,
                thread_id
            ],
        )
        .context("failed to update chat approval message")?;
    if updated_message_rows != 1 {
        anyhow::bail!(
            "failed to update chat approval message: expected 1 row, updated {updated_message_rows}"
        );
    }

    // 保存调用方指定的线程状态，并确认目标线程存在。
    let updated_thread_rows = tx
        .execute(
            "UPDATE threads SET status=?1 WHERE id=?2",
            params![thread_status.as_str(), thread_id],
        )
        .context("failed to update chat approval thread status")?;
    if updated_thread_rows != 1 {
        anyhow::bail!(
            "failed to update chat approval thread status: expected 1 row, updated {updated_thread_rows}"
        );
    }

    // 仅全部数据库更新成功后提交审批创建事务。
    tx.commit()
        .context("failed to commit chat approval transaction")?;
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

/// 把 approvals 表中仍为 pending 的审批记录置为 expired。
///
/// 审批被引擎判死（运行时 ApprovalExpired 事件或回答时回执 "no longer pending"）后调用，
/// 避免启动恢复（derive_thread_status_for_recovery）凭这条 pending 记录把线程误判回
/// awaiting_approval，导致重启后输入框被终止键锁死。返回是否实际发生了状态变更。
pub fn expire_approval(db: &Database, approval_id: &str) -> anyhow::Result<bool> {
    let conn = db.connect()?;
    let changed = conn
        .execute(
            "UPDATE approvals
       SET status = 'expired'
       WHERE id = ?1
         AND status = 'pending'",
            params![approval_id],
        )
        .context("failed to expire approval")?;
    Ok(changed > 0)
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

/// 保存 Codex 运行时审批完成事件时使用的事务化审批数据。
#[derive(Debug, Clone)]
pub struct RuntimeApprovalRecord {
    /// 审批持久化标识。
    pub approval_id: String,
    /// 审批所属线程标识；缺失时由业务层判定记录无效。
    pub thread_id: Option<String>,
    /// 审批卡片所在消息标识；缺失时由业务层判定记录无效。
    pub message_id: Option<String>,
    /// 审批持久化状态，用于阻止迟到事件覆盖终态。
    pub status: String,
    /// 所属线程引擎标识，用于限制 Codex 运行时事件的归属。
    pub engine_id: Option<String>,
    /// 所属线程当前状态，用于条件性推进本地活动轮次。
    pub thread_status: Option<String>,
    /// 消息所属线程标识，用于校验审批卡片归属。
    pub message_thread_id: Option<String>,
    /// 审批卡片 JSON 原文，用于严格解析并原子更新。
    pub blocks_json: Option<String>,
}

/// 在单个 SQLite 事务中执行运行时审批业务并提交结果。
pub fn with_runtime_approval_transaction<T>(
    db: &Database,
    work: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start runtime approval transaction")?;
    let result = work(&tx)?;
    tx.commit()
        .context("failed to commit runtime approval transaction")?;
    Ok(result)
}

/// 在连接或事务内读取审批、线程和消息的当前持久化事实。
pub fn get_runtime_approval_record(
    conn: &rusqlite::Connection,
    approval_id: &str,
) -> anyhow::Result<Option<RuntimeApprovalRecord>> {
    conn.query_row(
        "SELECT a.id, a.thread_id, a.message_id, a.status,
                t.engine_id, t.status, m.thread_id, m.blocks_json
         FROM approvals a
         LEFT JOIN threads t ON t.id = a.thread_id
         LEFT JOIN messages m ON m.id = a.message_id
         WHERE a.id = ?1",
        params![approval_id],
        |row| {
            Ok(RuntimeApprovalRecord {
                approval_id: row.get(0)?,
                thread_id: row.get(1)?,
                message_id: row.get(2)?,
                status: row.get(3)?,
                engine_id: row.get(4)?,
                thread_status: row.get(5)?,
                message_thread_id: row.get(6)?,
                blocks_json: row.get(7)?,
            })
        },
    )
    .optional()
    .context("failed to load runtime approval record")
}

/// 从数据库读取 Codex 运行时审批上下文并复用连接级事实查询。
pub fn read_runtime_approval_record(
    db: &Database,
    approval_id: &str,
) -> anyhow::Result<Option<RuntimeApprovalRecord>> {
    let connection = db.connect()?;
    get_runtime_approval_record(&connection, approval_id)
}

/// 按 Codex 原始请求、引擎线程和连接身份定位唯一的数据库审批记录。
pub fn find_codex_approval_by_request(
    db: &Database,
    engine_thread_id: &str,
    request_id: &Value,
    source_connection_id: &str,
) -> anyhow::Result<Option<RuntimeApprovalRecord>> {
    // 仅从 Codex 线程读取包含原线程与连接身份的审批详情。
    let conn = db.connect().context("failed to connect for Codex approval lookup")?;
    let mut statement = conn
        .prepare(
            "SELECT a.id, a.details_json
             FROM approvals a
             JOIN threads t ON t.id = a.thread_id
             WHERE t.engine_id = 'codex'
               AND json_valid(a.details_json)
               AND COALESCE(
                   json_extract(a.details_json, '$.threadId'),
                   json_extract(a.details_json, '$.thread_id'),
                   t.engine_thread_id
               ) = ?1
               AND json_extract(a.details_json, '$._codexConnectionId') = ?2",
        )
        .context("failed to prepare Codex approval request lookup")?;
    let rows = statement
        .query_map(params![engine_thread_id, source_connection_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("failed to query Codex approval requests")?;

    // 通过 serde_json::Value 严格比较原始 ID，区分数字与字符串等不同 JSON 类型。
    let mut matching_ids = Vec::new();
    for row in rows {
        let (approval_id, details_json) = row.context("failed to read Codex approval request row")?;
        let details = serde_json::from_str::<Value>(&details_json)
            .context("failed to parse Codex approval details_json")?;
        if details.get("_rawRequestId") == Some(request_id) {
            matching_ids.push(approval_id);
        }
    }

    // 仅唯一匹配时读取完整审批事实，歧义请求不任意挑选记录。
    match matching_ids.len() {
        0 => Ok(None),
        1 => get_runtime_approval_record(&conn, &matching_ids[0])
            .context("failed to load matching Codex runtime approval record"),
        count => anyhow::bail!(
            "ambiguous Codex approval request match: engine_thread_id={engine_thread_id}, source_connection_id={source_connection_id}, matches={count}"
        ),
    }
}

/// 在审批事务内读取目标线程 DTO，供提交后通知页面使用。
pub fn get_runtime_approval_thread(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
) -> anyhow::Result<Option<ThreadDto>> {
    super::threads::get_thread_on_connection(tx, thread_id)
        .context("failed to read runtime approval thread snapshot")
}

/// 仅在审批仍为 pending 且线程归属匹配时保存审批终态。
pub fn update_runtime_approval_pending(
    tx: &rusqlite::Transaction<'_>,
    approval_id: &str,
    thread_id: &str,
    target_status: &str,
    answered_at: &str,
) -> anyhow::Result<usize> {
    tx.execute(
        "UPDATE approvals
         SET status = ?1, answered_at = COALESCE(answered_at, ?2)
         WHERE id = ?3 AND thread_id = ?4 AND status = 'pending'",
        params![target_status, answered_at, approval_id, thread_id],
    )
    .context("failed to update pending runtime approval")
}

/// 仅当消息内容仍等于业务读取的原文时保存新的审批卡片 JSON。
pub fn update_runtime_approval_message_blocks(
    tx: &rusqlite::Transaction<'_>,
    message_id: &str,
    thread_id: &str,
    old_blocks_json: &str,
    new_blocks_json: &str,
) -> anyhow::Result<usize> {
    tx.execute(
        "UPDATE messages SET blocks_json = ?1
         WHERE id = ?2 AND thread_id = ?3 AND blocks_json = ?4",
        params![new_blocks_json, message_id, thread_id, old_blocks_json],
    )
    .context("failed to update runtime approval message blocks")
}

/// 仅当线程仍处于预期状态时更新运行时审批对应的线程状态。
pub fn update_runtime_thread_status_if_current(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
    current_status: &str,
    target_status: &str,
    last_activity_at: &str,
) -> anyhow::Result<usize> {
    tx.execute(
        "UPDATE threads SET status = ?1, last_activity_at = ?2
         WHERE id = ?3 AND status = ?4",
        params![target_status, last_activity_at, thread_id, current_status],
    )
    .context("failed to conditionally update runtime thread status")
}

// 废弃，使用log4jrs
// pub fn append_event_log(
//     db: &Database,
//     thread_id: &str,
//     message_id: &str,
//     event: &Value,
// ) -> anyhow::Result<()> {
//     let conn = db.connect()?;
//     let created_at = runtime_env::system_time_rfc3339();
//     conn.execute(
//         "INSERT INTO engine_event_logs (thread_id, message_id, event_json, created_at)
//          VALUES (?1, ?2, ?3, ?4)",
//         params![thread_id, message_id, event.to_string(), created_at],
//     )
//     .context("failed to append engine event log")?;
//     Ok(())
// }
