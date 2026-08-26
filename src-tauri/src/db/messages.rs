use anyhow::Context;
use chrono::{Duration as ChronoDuration, Local};
use std::collections::HashMap;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde_json::Value;
use uuid::Uuid;

use crate::models::{
    ActionOutputChunkDto, ActionOutputDto, MessageDto, MessageStatusDto, MessageWindowCursorDto,
    MessageWindowDto, SearchResultDto, TokenUsageDto,
};
use crate::runtime_env;

use super::Database;

#[derive(Debug, Clone)]
pub struct ImportedMessageRecord {
    pub role: String,
    pub content: Option<String>,
    pub blocks: Value,
    pub status: MessageStatusDto,
    pub turn_engine_id: Option<String>,
    pub remote_turn_id: Option<String>,
    pub turn_model_id: Option<String>,
    pub turn_reasoning_effort: Option<String>,
    pub token_input: u64,
    pub token_output: u64,
    pub created_at: Option<String>,
}

/// 供 AuraCoder 会话 MCP 返回的最小消息记录。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AuraCoderThreadMessageRecord {
    /// 消息主键。
    pub id: String,
    /// 消息角色。
    pub role: String,
    /// 消息文本；部分块消息没有文本内容。
    pub content: Option<String>,
    /// 消息创建时间。
    pub created_at: String,
}

pub fn insert_user_message(
    db: &Database,
    thread_id: &str,
    content: &str,
    blocks: Option<Value>,
    turn_engine_id: Option<&str>,
    turn_model_id: Option<&str>,
    turn_reasoning_effort: Option<&str>,
) -> anyhow::Result<MessageDto> {
    insert_message(
        db,
        thread_id,
        "user",
        Some(content.to_string()),
        blocks,
        MessageStatusDto::Completed,
        turn_engine_id,
        turn_model_id,
        turn_reasoning_effort,
    )
}

pub fn insert_assistant_placeholder(
    db: &Database,
    thread_id: &str,
    turn_engine_id: Option<&str>,
    turn_model_id: Option<&str>,
    turn_reasoning_effort: Option<&str>,
) -> anyhow::Result<MessageDto> {
    insert_message(
        db,
        thread_id,
        "assistant",
        None,
        Some(serde_json::json!([])),
        MessageStatusDto::Streaming,
        turn_engine_id,
        turn_model_id,
        turn_reasoning_effort,
    )
}

pub fn delete_message(db: &Database, message_id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute("DELETE FROM messages WHERE id = ?1", params![message_id])
        .context("failed to delete message")?;
    Ok(())
}

pub fn clone_thread_messages(
    db: &Database,
    source_thread_id: &str,
    target_thread_id: &str,
) -> anyhow::Result<usize> {
    let messages = get_thread_messages(db, source_thread_id)?;
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start thread message clone transaction")?;

    for (index, message) in messages.iter().enumerate() {
        let token_usage = message
            .token_usage
            .as_ref()
            .cloned()
            .unwrap_or(TokenUsageDto {
                input: 0,
                output: 0,
            });
        let created_at = runtime_env::format_system_time(
            Local::now() + ChronoDuration::milliseconds(index as i64),
        );

        tx.execute(
            "INSERT INTO messages (
                id, thread_id, role, content, blocks_json, turn_engine_id, turn_model_id,
                turn_reasoning_effort, schema_version, stream_seq, status, token_input,
                token_output, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13)",
            params![
                Uuid::new_v4().to_string(),
                target_thread_id,
                message.role,
                message.content,
                message.blocks.as_ref().map(Value::to_string),
                message.turn_engine_id,
                message.turn_model_id,
                message.turn_reasoning_effort,
                message.schema_version,
                message.status.as_str(),
                token_usage.input as i64,
                token_usage.output as i64,
                created_at,
            ],
        )
        .context("failed to clone thread message")?;
    }

    tx.commit()
        .context("failed to commit thread message clone transaction")?;
    Ok(messages.len())
}

pub fn append_thread_messages(
    db: &Database,
    thread_id: &str,
    messages: &[ImportedMessageRecord],
) -> anyhow::Result<usize> {
    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start thread message import transaction")?;

    let fallback_created_at_base = Local::now();
    let mut inserted = 0;
    for (index, message) in messages.iter().enumerate() {
        let Some(remote_turn_id) = message
            .remote_turn_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if message.role == "user" {
            bind_legacy_local_turn_to_remote_turn(&tx, thread_id, message, remote_turn_id)?;
        }
        let created_at = message.created_at.clone().unwrap_or_else(|| {
            runtime_env::format_system_time(
                fallback_created_at_base + ChronoDuration::milliseconds(index as i64),
            )
        });
        let existing = tx
            .query_row(
                "SELECT id, turn_engine_id
                 FROM messages
                 WHERE thread_id = ?1
                   AND remote_turn_id = ?2
                   AND role = ?3
                 LIMIT 1",
                params![thread_id, remote_turn_id, message.role],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .context("failed to find imported thread message")?;

        match existing {
            Some((message_id, Some(existing_turn_engine_id)))
                if existing_turn_engine_id == remote_turn_id
                    && is_latest_thread_message(&tx, thread_id, &message_id)? =>
            {
                tx.execute(
                    "UPDATE messages
                     SET content = ?1,
                         blocks_json = ?2,
                         turn_model_id = ?3,
                         turn_reasoning_effort = ?4,
                         status = ?5,
                         token_input = ?6,
                         token_output = ?7
                     WHERE id = ?8",
                    params![
                        message.content,
                        message.blocks.to_string(),
                        message.turn_model_id,
                        message.turn_reasoning_effort,
                        message.status.as_str(),
                        message.token_input as i64,
                        message.token_output as i64,
                        message_id,
                    ],
                )
                .context("failed to replace latest imported thread message")?;
            }
            Some(_) => {}
            None => {
                inserted += tx
                    .execute(
                        "INSERT INTO messages (
                            id, thread_id, role, content, blocks_json, turn_engine_id, remote_turn_id, turn_model_id,
                            turn_reasoning_effort, schema_version, stream_seq, status, token_input,
                            token_output, created_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 0, ?10, ?11, ?12, ?13)",
                        params![
                            Uuid::new_v4().to_string(),
                            thread_id,
                            message.role,
                            message.content,
                            message.blocks.to_string(),
                            message.turn_engine_id,
                            remote_turn_id,
                            message.turn_model_id,
                            message.turn_reasoning_effort,
                            message.status.as_str(),
                            message.token_input as i64,
                            message.token_output as i64,
                            created_at,
                        ],
                    )
                    .context("failed to insert imported thread message")?;
            }
        }
    }

    tx.commit()
        .context("failed to commit thread message import transaction")?;
    Ok(inserted)
}

fn is_latest_thread_message(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
    message_id: &str,
) -> anyhow::Result<bool> {
    let latest_message_id = tx
        .query_row(
            "SELECT id
             FROM messages
             WHERE thread_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1",
            params![thread_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to find latest thread message")?;
    Ok(latest_message_id.as_deref() == Some(message_id))
}

fn bind_legacy_local_turn_to_remote_turn(
    tx: &rusqlite::Transaction<'_>,
    thread_id: &str,
    remote_message: &ImportedMessageRecord,
    remote_turn_id: &str,
) -> anyhow::Result<()> {
    let Some(content) = remote_message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let local_user_id = tx
        .query_row(
            "SELECT id
             FROM messages
             WHERE thread_id = ?1
               AND role = 'user'
               AND remote_turn_id IS NULL
               AND content = ?2
             ORDER BY created_at ASC, rowid ASC
             LIMIT 1",
            params![thread_id, content],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to find legacy local user message for remote turn binding")?;
    let Some(local_user_id) = local_user_id else {
        return Ok(());
    };

    let bound_user_count = tx
        .execute(
            "UPDATE messages
             SET remote_turn_id = ?1
             WHERE id = ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM messages
                   WHERE thread_id = ?3
                     AND remote_turn_id = ?1
                     AND role = 'user'
               )",
            params![remote_turn_id, local_user_id, thread_id],
        )
        .context("failed to bind legacy local user message to remote turn id")?;
    if bound_user_count == 0 {
        return Ok(());
    }

    tx.execute(
        "UPDATE messages
         SET remote_turn_id = ?1
         WHERE id = (
             SELECT candidate.id
             FROM messages AS candidate
             JOIN messages AS user ON user.id = ?2
             WHERE candidate.thread_id = ?3
               AND candidate.role = 'assistant'
               AND candidate.remote_turn_id IS NULL
               AND (
                   candidate.created_at > user.created_at
                   OR (candidate.created_at = user.created_at AND candidate.rowid > user.rowid)
               )
             ORDER BY candidate.created_at ASC, candidate.rowid ASC
             LIMIT 1
         )
           AND NOT EXISTS (
               SELECT 1
               FROM messages
               WHERE thread_id = ?3
                 AND remote_turn_id = ?1
                 AND role = 'assistant'
           )",
        params![remote_turn_id, local_user_id, thread_id],
    )
    .context("failed to bind legacy local assistant message to remote turn id")?;
    Ok(())
}

pub fn bind_local_turn_to_remote_turn(
    db: &Database,
    assistant_message_id: &str,
    remote_turn_id: &str,
) -> anyhow::Result<()> {
    let remote_turn_id = remote_turn_id.trim();
    if remote_turn_id.is_empty() {
        return Ok(());
    }

    let conn = db.connect()?;
    conn.execute(
        "UPDATE messages
         SET remote_turn_id = ?1
         WHERE id = ?2
            OR id = (
                SELECT candidate.id
                FROM messages AS candidate
                JOIN messages AS assistant ON assistant.id = ?2
                WHERE candidate.thread_id = assistant.thread_id
                  AND candidate.role = 'user'
                  AND (
                    candidate.created_at < assistant.created_at
                    OR (candidate.created_at = assistant.created_at AND candidate.rowid < assistant.rowid)
                  )
                ORDER BY candidate.created_at DESC, candidate.rowid DESC
                LIMIT 1
            )",
        params![remote_turn_id, assistant_message_id],
    )
    .context("failed to bind local messages to remote turn id")?;
    Ok(())
}

pub fn drop_last_turns(db: &Database, thread_id: &str, num_turns: u32) -> anyhow::Result<usize> {
    let messages = get_thread_messages(db, thread_id)?;
    let user_message_indexes = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.role == "user" && !message_has_steer_marker(message)).then_some(index)
        })
        .collect::<Vec<_>>();

    let turns_to_drop = usize::try_from(num_turns).unwrap_or(usize::MAX);
    if turns_to_drop == 0 {
        anyhow::bail!("num_turns must be at least 1");
    }
    if user_message_indexes.len() < turns_to_drop {
        anyhow::bail!(
            "cannot drop {turns_to_drop} turns from local thread history with only {} user turns",
            user_message_indexes.len()
        );
    }

    let cutoff_index = user_message_indexes[user_message_indexes.len() - turns_to_drop];
    let message_ids = messages
        .iter()
        .skip(cutoff_index)
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    if message_ids.is_empty() {
        return Ok(0);
    }

    let mut conn = db.connect()?;
    let tx = conn
        .transaction()
        .context("failed to start thread rollback transaction")?;

    for chunk in message_ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let delete_actions_sql =
            format!("DELETE FROM actions WHERE thread_id = ? AND message_id IN ({placeholders})");
        let delete_approvals_sql =
            format!("DELETE FROM approvals WHERE thread_id = ? AND message_id IN ({placeholders})");
        let delete_messages_sql =
            format!("DELETE FROM messages WHERE thread_id = ? AND id IN ({placeholders})");

        let mut actions_params = Vec::with_capacity(chunk.len() + 1);
        actions_params.push(rusqlite::types::Value::from(thread_id.to_string()));
        actions_params.extend(chunk.iter().cloned().map(rusqlite::types::Value::from));
        tx.execute(&delete_actions_sql, params_from_iter(actions_params))
            .context("failed to delete rolled-back thread actions")?;

        let mut approvals_params = Vec::with_capacity(chunk.len() + 1);
        approvals_params.push(rusqlite::types::Value::from(thread_id.to_string()));
        approvals_params.extend(chunk.iter().cloned().map(rusqlite::types::Value::from));
        tx.execute(&delete_approvals_sql, params_from_iter(approvals_params))
            .context("failed to delete rolled-back thread approvals")?;

        let mut message_params = Vec::with_capacity(chunk.len() + 1);
        message_params.push(rusqlite::types::Value::from(thread_id.to_string()));
        message_params.extend(chunk.iter().cloned().map(rusqlite::types::Value::from));
        tx.execute(&delete_messages_sql, params_from_iter(message_params))
            .context("failed to delete rolled-back thread messages")?;
    }

    // Note: all approvals (including pending ones) for the deleted messages are already
    // removed by the chunked delete_approvals_sql above, which scopes deletions to
    // message_id IN (...). The previous thread-wide pending approval sweep is intentionally
    // removed here to avoid deleting pending approvals that belong to retained messages.

    tx.commit()
        .context("failed to commit thread rollback transaction")?;
    Ok(message_ids.len())
}

fn message_has_steer_marker(message: &MessageDto) -> bool {
    let Some(blocks) = message.blocks.as_ref().and_then(Value::as_array) else {
        return false;
    };

    blocks.iter().any(|block| {
        let Some(block_type) = block.get("type").and_then(Value::as_str) else {
            return false;
        };
        if block_type != "text" {
            return false;
        }

        block
            .get("isSteer")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

pub fn update_assistant_blocks_json(
    db: &Database,
    message_id: &str,
    blocks_json: &str,
    status: MessageStatusDto,
    turn_model_id: Option<&str>,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let normalized_blocks_json = normalize_blocks_json_for_message(&conn, message_id, blocks_json)?;
    // Mirror the text blocks into the content column: the messages_fts
    // triggers index only content, so leaving it NULL keeps assistant replies
    // out of global search.
    let searchable_text = extract_searchable_text_from_blocks(&normalized_blocks_json);
    conn.execute(
        "UPDATE messages
     SET blocks_json = ?1, content = ?2, status = ?3, turn_model_id = COALESCE(?4, turn_model_id)
     WHERE id = ?5",
        params![
            normalized_blocks_json,
            searchable_text,
            status.as_str(),
            turn_model_id,
            message_id
        ],
    )
    .context("failed to update assistant blocks")?;
    Ok(())
}

fn extract_searchable_text_from_blocks(blocks_json: &str) -> Option<String> {
    let blocks: Value = serde_json::from_str(blocks_json).ok()?;
    let parts: Vec<&str> = blocks
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("content").and_then(Value::as_str))
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

pub fn update_assistant_status(
    db: &Database,
    message_id: &str,
    status: MessageStatusDto,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE messages
     SET status = ?1
     WHERE id = ?2",
        params![status.as_str(), message_id],
    )
    .context("failed to update assistant status")?;
    Ok(())
}

pub fn complete_assistant_message(
    db: &Database,
    message_id: &str,
    status: MessageStatusDto,
    token_usage: Option<(u64, u64)>,
    turn_model_id: Option<&str>,
) -> anyhow::Result<()> {
    let (input, output) = token_usage.unwrap_or((0, 0));
    let conn = db.connect()?;
    conn.execute(
        "UPDATE messages
     SET status = ?1,
         token_input = ?2,
         token_output = ?3,
         turn_model_id = COALESCE(?4, turn_model_id)
     WHERE id = ?5",
        params![
            status.as_str(),
            input as i64,
            output as i64,
            turn_model_id,
            message_id
        ],
    )
    .context("failed to complete assistant message")?;
    Ok(())
}

/// 按精确消息 ID读取单条消息，供远程完成事件使用。
///
/// 与 `get_thread_messages` 相同地补齐已处理审批块，但不会扫描整个会话。
pub fn get_message(db: &Database, message_id: &str) -> anyhow::Result<Option<MessageDto>> {
    let conn = db.connect()?;
    let mut message = conn
        .query_row(
            "SELECT id, thread_id, role, content, blocks_json, schema_version, status,
                token_input, token_output, turn_engine_id, turn_model_id, turn_reasoning_effort, created_at
         FROM messages
         WHERE id = ?1",
            params![message_id],
            map_message_row,
        )
        .optional()
        .context("failed to load message")?;

    if let Some(message) = message.as_mut() {
        if let Some(blocks) = message.blocks.as_mut() {
            reconcile_answered_approvals_for_message(&conn, &message.id, blocks)?;
        }
    }
    Ok(message)
}

pub fn update_assistant_turn_model_id(
    db: &Database,
    message_id: &str,
    turn_model_id: &str,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    conn.execute(
        "UPDATE messages
     SET turn_model_id = ?1
     WHERE id = ?2",
        params![turn_model_id, message_id],
    )
    .context("failed to update assistant turn model id")?;
    Ok(())
}

pub fn get_thread_messages(db: &Database, thread_id: &str) -> anyhow::Result<Vec<MessageDto>> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, role, content, blocks_json, schema_version, status,
            token_input, token_output, turn_engine_id, turn_model_id, turn_reasoning_effort, created_at
     FROM messages
     WHERE thread_id = ?1
     ORDER BY created_at ASC, rowid ASC",
    )?;

    let rows = stmt.query_map(params![thread_id], map_message_row)?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    reconcile_answered_approvals_for_messages(&conn, &mut out)?;

    Ok(out)
}

/// 查询指定 AuraCoder 会话中的消息行数。
pub fn count_thread_messages(db: &Database, thread_id: &str) -> anyhow::Result<u64> {
    let conn = db.connect()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
        params![thread_id],
        |row| row.get(0),
    )?;
    u64::try_from(count).context("message count is negative")
}

/// 按创建时间倒序读取消息页；同一时间使用 SQLite rowid 倒序保证稳定顺序。
pub fn get_thread_messages_page_desc(
    db: &Database,
    thread_id: &str,
    page: u64,
    page_size: u64,
) -> anyhow::Result<Vec<AuraCoderThreadMessageRecord>> {
    if page == 0 || page_size == 0 {
        anyhow::bail!("page and page_size must be greater than zero");
    }
    let offset = page
        .checked_sub(1)
        .and_then(|value| value.checked_mul(page_size))
        .context("message page offset overflow")?;
    let limit = i64::try_from(page_size).context("message page size is too large")?;
    let offset = i64::try_from(offset).context("message page offset is too large")?;
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, role, content, created_at
         FROM messages
         WHERE thread_id = ?1
         ORDER BY created_at DESC, rowid DESC
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(params![thread_id, limit, offset], |row| {
        Ok(AuraCoderThreadMessageRecord {
            id: row.get(0)?,
            role: row.get(1)?,
            content: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

pub fn get_thread_messages_window(
    db: &Database,
    thread_id: &str,
    cursor: Option<&MessageWindowCursorDto>,
    limit: usize,
) -> anyhow::Result<MessageWindowDto> {
    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, role, content, blocks_json, schema_version, status,
            token_input, token_output, turn_engine_id, turn_model_id, turn_reasoning_effort, created_at, rowid
     FROM messages
     WHERE thread_id = ?1
       AND (
         ?2 IS NULL
         OR created_at < ?2
         OR (
           created_at = ?2
           AND (
             (?3 IS NOT NULL AND rowid < ?3)
             OR (?3 IS NULL AND ?4 IS NOT NULL AND id < ?4)
           )
         )
       )
     ORDER BY created_at DESC, rowid DESC
     LIMIT ?5",
    )?;

    let cursor_created_at = cursor.map(|value| value.created_at.as_str());
    let cursor_row_id = cursor.and_then(|value| value.row_id);
    let cursor_id = cursor.map(|value| value.id.as_str());
    let query_limit = limit.max(1).saturating_add(1) as i64;
    let rows = stmt.query_map(
        params![
            thread_id,
            cursor_created_at,
            cursor_row_id,
            cursor_id,
            query_limit
        ],
        |row| {
            let message = map_message_row(row)?;
            let row_id: i64 = row.get(13)?;
            Ok((message, row_id))
        },
    )?;

    let mut messages_desc: Vec<(MessageDto, i64)> = Vec::new();
    for row in rows {
        messages_desc.push(row?);
    }

    let page_limit = limit.max(1);
    let has_more = messages_desc.len() > page_limit;
    if has_more {
        messages_desc.pop();
    }

    let next_cursor = if has_more {
        messages_desc
            .last()
            .map(|(message, row_id)| MessageWindowCursorDto {
                created_at: message.created_at.clone(),
                id: message.id.clone(),
                row_id: Some(*row_id),
            })
    } else {
        None
    };

    messages_desc.reverse();
    let mut messages: Vec<MessageDto> = messages_desc
        .into_iter()
        .map(|(message, _)| message)
        .collect();
    reconcile_answered_approvals_for_messages(&conn, &mut messages)?;
    Ok(MessageWindowDto {
        messages,
        next_cursor,
    })
}

pub fn get_message_blocks(db: &Database, message_id: &str) -> anyhow::Result<Option<Value>> {
    let conn = db.connect()?;
    let raw_blocks: Option<Option<String>> = conn
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to load message blocks")?;

    let Some(raw_blocks) = raw_blocks else {
        return Ok(None);
    };

    let mut blocks = raw_blocks
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!([]));
    reconcile_answered_approvals_for_message(&conn, message_id, &mut blocks)?;
    Ok(Some(blocks))
}

pub fn get_action_output(
    db: &Database,
    message_id: &str,
    action_id: &str,
) -> anyhow::Result<ActionOutputDto> {
    let Some(blocks) = get_message_blocks(db, message_id)? else {
        return Ok(ActionOutputDto {
            found: false,
            output_chunks: Vec::new(),
            truncated: false,
        });
    };

    let Some(items) = blocks.as_array() else {
        return Ok(ActionOutputDto {
            found: false,
            output_chunks: Vec::new(),
            truncated: false,
        });
    };

    for block in items {
        let Some(object) = block.as_object() else {
            continue;
        };

        if object.get("type").and_then(Value::as_str) != Some("action") {
            continue;
        }

        let candidate_id = object
            .get("actionId")
            .and_then(Value::as_str)
            .or_else(|| object.get("action_id").and_then(Value::as_str));
        if candidate_id != Some(action_id) {
            continue;
        }

        let output_chunks = object
            .get("outputChunks")
            .or_else(|| object.get("output_chunks"))
            .and_then(Value::as_array)
            .map(|chunks| {
                chunks
                    .iter()
                    .filter_map(|chunk| {
                        let object = chunk.as_object()?;
                        let stream = object
                            .get("stream")
                            .and_then(Value::as_str)
                            .unwrap_or("stdout")
                            .to_string();
                        let content = match object.get("content") {
                            Some(Value::String(value)) => value.to_string(),
                            Some(value) => value.to_string(),
                            None => String::new(),
                        };
                        Some(ActionOutputChunkDto { stream, content })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let truncated = object
            .get("details")
            .and_then(Value::as_object)
            .and_then(|details| {
                details
                    .get("outputTruncated")
                    .or_else(|| details.get("output_truncated"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);

        return Ok(ActionOutputDto {
            found: true,
            output_chunks,
            truncated,
        });
    }

    Ok(ActionOutputDto {
        found: false,
        output_chunks: Vec::new(),
        truncated: false,
    })
}

pub fn mark_approval_block_answered(
    db: &Database,
    message_id: &str,
    approval_id: &str,
    decision: &str,
) -> anyhow::Result<bool> {
    mark_approval_block_resolved(db, message_id, approval_id, Some(decision))
}

pub fn mark_approval_block_resolved(
    db: &Database,
    message_id: &str,
    approval_id: &str,
    decision: Option<&str>,
) -> anyhow::Result<bool> {
    let conn = db.connect()?;
    let Some(raw_blocks): Option<String> = conn
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to load message blocks for approval update")?
    else {
        return Ok(false);
    };

    let mut blocks_value: Value =
        serde_json::from_str(&raw_blocks).unwrap_or_else(|_| serde_json::json!([]));
    let mut resolved = HashMap::new();
    resolved.insert(
        approval_id.to_string(),
        decision.map(std::string::ToString::to_string),
    );
    let changed = apply_resolved_approvals_to_blocks(&mut blocks_value, &resolved);
    if !changed {
        return Ok(false);
    }

    conn.execute(
        "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
        params![blocks_value.to_string(), message_id],
    )
    .context("failed to persist answered approval in message blocks")?;

    Ok(true)
}

pub fn search_messages(
    db: &Database,
    workspace_id: &str,
    query: &str,
) -> anyhow::Result<Vec<SearchResultDto>> {
    let Some(search_query) = build_search_messages_query(query) else {
        return Ok(Vec::new());
    };

    let conn = db.connect()?;
    let mut stmt = conn.prepare(
        "SELECT m.thread_id,
            t.title,
            w.name,
            t.repo_id,
            m.id,
            COALESCE(m.content, '')
     FROM messages_fts
     JOIN messages m ON m.rowid = messages_fts.rowid
     JOIN threads t ON t.id = m.thread_id
     JOIN workspaces w ON w.id = t.workspace_id
     WHERE t.workspace_id = ?1
       AND t.archived_at IS NULL
       AND messages_fts MATCH ?2
     ORDER BY rank
     LIMIT 50",
    )?;

    let rows = stmt.query_map(params![workspace_id, search_query], |row| {
        Ok(SearchResultDto {
            thread_id: row.get(0)?,
            thread_title: row.get(1)?,
            workspace_name: row.get(2)?,
            repo_id: row.get(3)?,
            message_id: row.get(4)?,
            snippet: build_search_result_snippet(&row.get::<_, String>(5)?, query),
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }

    Ok(out)
}

fn build_search_messages_query(query: &str) -> Option<String> {
    let tokens = tokenize_search_query(query)
        .into_iter()
        .filter_map(|token| format_search_messages_token(&token))
        .collect::<Vec<_>>();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

fn format_search_messages_token(token: &str) -> Option<String> {
    let wildcard_trimmed = token.trim_end_matches('*');
    if wildcard_trimmed.is_empty() {
        return None;
    }

    let escaped = wildcard_trimmed.replace('"', "\"\"");
    let suffix = if wildcard_trimmed.len() < token.len() {
        "*"
    } else {
        ""
    };

    Some(format!("\"{}\"{}", escaped, suffix))
}

fn build_search_result_snippet(content: &str, query: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let terms = tokenize_search_query(query);
    let match_index = terms
        .iter()
        .filter_map(|term| {
            let snippet_term = term.trim_end_matches('*');
            if snippet_term.is_empty() {
                None
            } else {
                find_term_case_insensitive(trimmed, snippet_term)
            }
        })
        .min()
        .unwrap_or(0);

    let context_before = 48usize;
    let context_after = 120usize;
    let start;
    if match_index > context_before {
        start = trimmed
            .char_indices()
            .take_while(|(idx, _)| *idx <= match_index.saturating_sub(context_before))
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    } else {
        start = 0;
    }

    let end_target = (match_index + context_after).min(trimmed.len());
    let end = trimmed
        .char_indices()
        .take_while(|(idx, _)| *idx <= end_target)
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(trimmed.len());

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("… ");
    }
    snippet.push_str(trimmed[start..end].trim());
    if end < trimmed.len() {
        snippet.push_str(" …");
    }
    snippet
}

fn find_term_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    let folded_needle = needle
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if folded_needle.is_empty() {
        return Some(0);
    }

    let mut folded_haystack = String::with_capacity(haystack.len());
    let mut byte_to_original = Vec::new();

    for (idx, ch) in haystack.char_indices() {
        for lower in ch.to_lowercase() {
            let mut buffer = [0; 4];
            let encoded = lower.encode_utf8(&mut buffer);
            folded_haystack.push_str(encoded);
            byte_to_original.extend(std::iter::repeat_n(idx, encoded.len()));
        }
    }

    folded_haystack
        .find(&folded_needle)
        .and_then(|folded_idx| byte_to_original.get(folded_idx).copied())
}

fn tokenize_search_query(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quotes {
                    let token = current.trim();
                    if !token.is_empty() {
                        tokens.push(token.to_string());
                    }
                    current.clear();
                    in_quotes = false;
                } else {
                    let token = current.trim();
                    if !token.is_empty() {
                        tokens.push(token.to_string());
                        current.clear();
                    }
                    in_quotes = true;
                }
            }
            ch if ch.is_whitespace() && !in_quotes => {
                let token = current.trim();
                if !token.is_empty() {
                    tokens.push(token.to_string());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    let trailing = current.trim();
    if !trailing.is_empty() {
        tokens.push(trailing.to_string());
    }

    tokens
}

#[allow(clippy::too_many_arguments)]
fn insert_message(
    db: &Database,
    thread_id: &str,
    role: &str,
    content: Option<String>,
    blocks: Option<Value>,
    status: MessageStatusDto,
    turn_engine_id: Option<&str>,
    turn_model_id: Option<&str>,
    turn_reasoning_effort: Option<&str>,
) -> anyhow::Result<MessageDto> {
    let id = Uuid::new_v4().to_string();
    let created_at = runtime_env::system_time_rfc3339();
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO messages (
            id, thread_id, role, content, blocks_json, schema_version, status, turn_engine_id, turn_model_id, turn_reasoning_effort, created_at
        )
     VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            thread_id,
            role,
            content,
            blocks.map(|value| value.to_string()),
            status.as_str(),
            turn_engine_id,
            turn_model_id,
            turn_reasoning_effort,
            created_at
        ],
    )
    .context("failed to insert message")?;

    let conn = db.connect()?;
    conn.query_row(
        "SELECT id, thread_id, role, content, blocks_json, schema_version, status,
            token_input, token_output, turn_engine_id, turn_model_id, turn_reasoning_effort, created_at
     FROM messages
     WHERE id = ?1",
        params![id],
        map_message_row,
    )
    .context("failed to load inserted message")
}

fn map_message_row(row: &Row<'_>) -> rusqlite::Result<MessageDto> {
    let blocks_raw: Option<String> = row.get(4)?;
    let token_input: i64 = row.get(7)?;
    let token_output: i64 = row.get(8)?;
    Ok(MessageDto {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        blocks: blocks_raw.and_then(|raw| serde_json::from_str(&raw).ok()),
        turn_engine_id: row.get(9)?,
        turn_model_id: row.get(10)?,
        turn_reasoning_effort: row.get(11)?,
        schema_version: row.get(5)?,
        status: MessageStatusDto::from_str(&row.get::<_, String>(6)?),
        token_usage: if token_input > 0 || token_output > 0 {
            Some(TokenUsageDto {
                input: token_input as u64,
                output: token_output as u64,
            })
        } else {
            None
        },
        created_at: row.get(12)?,
    })
}

fn apply_resolved_approvals_to_blocks(
    blocks: &mut Value,
    resolved: &HashMap<String, Option<String>>,
) -> bool {
    let Some(items) = blocks.as_array_mut() else {
        return false;
    };

    let mut changed = false;
    for block in items.iter_mut() {
        let Some(object) = block.as_object_mut() else {
            continue;
        };

        if object.get("type").and_then(Value::as_str) != Some("approval") {
            continue;
        }

        let approval_id = object
            .get("approvalId")
            .and_then(Value::as_str)
            .or_else(|| object.get("approval_id").and_then(Value::as_str));
        let Some(approval_id) = approval_id else {
            continue;
        };

        let Some(decision) = resolved.get(approval_id) else {
            continue;
        };

        let should_update_status = object
            .get("status")
            .and_then(Value::as_str)
            .map(|value| value != "answered")
            .unwrap_or(true);

        if should_update_status {
            object.insert("status".to_string(), Value::String("answered".to_string()));
            changed = true;
        }
        if let Some(decision) = decision {
            let should_update_decision = object
                .get("decision")
                .and_then(Value::as_str)
                .map(|value| value != decision)
                .unwrap_or(true);
            if should_update_decision {
                object.insert("decision".to_string(), Value::String(decision.to_string()));
                changed = true;
            }
        }
    }

    changed
}

fn normalize_blocks_json_for_message(
    conn: &Connection,
    message_id: &str,
    blocks_json: &str,
) -> anyhow::Result<String> {
    let mut blocks_value: Value = match serde_json::from_str(blocks_json) {
        Ok(value) => value,
        Err(_) => return Ok(blocks_json.to_string()),
    };

    reconcile_answered_approvals_for_message(conn, message_id, &mut blocks_value)?;
    Ok(blocks_value.to_string())
}

fn reconcile_answered_approvals_for_messages(
    conn: &Connection,
    messages: &mut [MessageDto],
) -> anyhow::Result<()> {
    let message_ids: Vec<String> = messages
        .iter()
        .filter(|message| message.blocks.is_some())
        .map(|message| message.id.clone())
        .collect();
    if message_ids.is_empty() {
        return Ok(());
    }

    let resolved_by_message = load_resolved_approvals_for_message_ids(conn, &message_ids)?;
    if resolved_by_message.is_empty() {
        return Ok(());
    }

    for message in messages {
        let Some(blocks) = message.blocks.as_mut() else {
            continue;
        };
        let Some(resolved) = resolved_by_message.get(&message.id) else {
            continue;
        };
        apply_resolved_approvals_to_blocks(blocks, resolved);
    }

    Ok(())
}

fn reconcile_answered_approvals_for_message(
    conn: &Connection,
    message_id: &str,
    blocks: &mut Value,
) -> anyhow::Result<bool> {
    let resolved = load_resolved_approvals_for_message(conn, message_id)?;
    if resolved.is_empty() {
        return Ok(false);
    }

    Ok(apply_resolved_approvals_to_blocks(blocks, &resolved))
}

fn load_resolved_approvals_for_message(
    conn: &Connection,
    message_id: &str,
) -> anyhow::Result<HashMap<String, Option<String>>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, decision
         FROM approvals
         WHERE message_id = ?1
           AND status = 'answered'",
        )
        .context("failed to prepare approval lookup for message")?;
    let rows = stmt
        .query_map(params![message_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .context("failed to query answered approvals for message")?;

    let mut resolved = HashMap::new();
    for row in rows {
        let (approval_id, decision) = row?;
        resolved.insert(approval_id, decision);
    }

    Ok(resolved)
}

fn load_resolved_approvals_for_message_ids(
    conn: &Connection,
    message_ids: &[String],
) -> anyhow::Result<HashMap<String, HashMap<String, Option<String>>>> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT message_id, id, decision
         FROM approvals
         WHERE message_id IS NOT NULL
           AND status = 'answered'
           AND message_id IN ({placeholders})"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare approval lookup for messages")?;
    let rows = stmt
        .query_map(params_from_iter(message_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .context("failed to query answered approvals for messages")?;

    let mut resolved_by_message: HashMap<String, HashMap<String, Option<String>>> = HashMap::new();
    for row in rows {
        let (message_id, approval_id, decision) = row?;
        resolved_by_message
            .entry(message_id)
            .or_default()
            .insert(approval_id, decision);
    }

    Ok(resolved_by_message)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use serde_json::json;

    use crate::{
        db::{actions, threads, workspaces, ConnectionPool, SQLITE_POOL_MAX_IDLE},
        engines::events::ActionType,
    };

    use super::*;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-messages-{}.db", Uuid::new_v4()));
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

    fn test_workspace(db: &Database) -> String {
        let root = std::env::temp_dir().join(format!("auracoder-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp workspace root");
        let workspace =
            workspaces::upsert_workspace(db, root.to_string_lossy().as_ref(), Some(1)).unwrap();
        workspace.id
    }

    fn test_thread(db: &Database) -> String {
        let workspace_id = test_workspace(db);
        let thread =
            threads::create_thread(db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        thread.id
    }

    #[test]
    fn created_times_use_the_operating_system_offset() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let message = insert_user_message(&db, &thread_id, "hello", None, None, None, None)
            .unwrap();
        let thread = threads::get_thread(&db, &thread_id).unwrap().unwrap();
        let expected_offset = chrono::Local::now().offset().local_minus_utc();

        let message_time = chrono::DateTime::parse_from_rfc3339(&message.created_at).unwrap();
        let thread_time = chrono::DateTime::parse_from_rfc3339(&thread.created_at).unwrap();
        assert_eq!(message_time.offset().local_minus_utc(), expected_offset);
        assert_eq!(thread_time.offset().local_minus_utc(), expected_offset);
    }

    fn approval_blocks_json(approval_id: &str) -> Value {
        json!([
            {
                "type": "approval",
                "approvalId": approval_id,
                "actionType": "command",
                "summary": "Run tests",
                "details": {},
                "status": "pending"
            }
        ])
    }

    fn steer_blocks_json(content: &str) -> Value {
        json!([
            {
                "type": "text",
                "content": content,
                "isSteer": true
            }
        ])
    }

    fn approval_block_status(blocks: &Value) -> Option<(&str, Option<&str>)> {
        let items = blocks.as_array()?;
        let approval = items.first()?.as_object()?;
        Some((
            approval.get("status")?.as_str()?,
            approval.get("decision").and_then(Value::as_str),
        ))
    }

    #[test]
    fn append_thread_messages_deduplicates_by_remote_turn_id_and_preserves_local_blocks() {
        let db = test_db();
        let thread_id = test_thread(&db);
        insert_message(
            &db,
            &thread_id,
            "user",
            Some("local prompt".to_string()),
            Some(json!([{ "type": "text", "content": "local prompt" }])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();
        let local_message = insert_message(
            &db,
            &thread_id,
            "assistant",
            Some("local answer".to_string()),
            Some(json!([{
                "type": "action",
                "actionId": "local-action",
                "summary": "Keep this local tool output"
            }])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();
        let imported = vec![
            ImportedMessageRecord {
                role: "user".to_string(),
                content: Some("local prompt".to_string()),
                blocks: json!([{ "type": "text", "content": "local prompt" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-existing".to_string()),
                remote_turn_id: Some("remote-existing".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 10:00:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("local answer".to_string()),
                blocks: json!([{ "type": "text", "content": "local answer" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-existing".to_string()),
                remote_turn_id: Some("remote-existing".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 10:01:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "user".to_string(),
                content: Some("new remote prompt".to_string()),
                blocks: json!([{ "type": "text", "content": "new remote prompt" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-new".to_string()),
                remote_turn_id: Some("remote-new".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:00:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("new remote answer".to_string()),
                blocks: json!([{ "type": "text", "content": "new remote answer" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-new".to_string()),
                remote_turn_id: Some("remote-new".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:00:00.000".to_string()),
            },
        ];

        let appended = append_thread_messages(&db, &thread_id, &imported).unwrap();

        assert_eq!(appended, 2);
        let messages = get_thread_messages(&db, &thread_id).unwrap();
        assert_eq!(messages.len(), 4);
        let preserved_local = messages
            .iter()
            .find(|message| message.id == local_message.id)
            .expect("local message should remain");
        assert_eq!(
            preserved_local
                .blocks
                .as_ref()
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
                .and_then(|block| block.get("actionId"))
                .and_then(Value::as_str),
            Some("local-action")
        );
        assert!(messages
            .iter()
            .any(|message| message.content.as_deref() == Some("new remote answer")));

        assert_eq!(
            append_thread_messages(&db, &thread_id, &imported).unwrap(),
            0
        );
        assert_eq!(get_thread_messages(&db, &thread_id).unwrap().len(), 4);
    }

    #[test]
    fn append_thread_messages_imports_all_remote_messages_without_local_history() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let imported = vec![ImportedMessageRecord {
            role: "user".to_string(),
            content: Some("remote prompt".to_string()),
            blocks: json!([{ "type": "text", "content": "remote prompt" }]),
            status: MessageStatusDto::Completed,
            turn_engine_id: Some("remote-turn".to_string()),
            remote_turn_id: Some("remote-turn".to_string()),
            turn_model_id: None,
            turn_reasoning_effort: None,
            token_input: 0,
            token_output: 0,
            created_at: Some("2026-08-08 12:00:00.000".to_string()),
        }];

        let appended = append_thread_messages(&db, &thread_id, &imported).unwrap();

        assert_eq!(appended, 1);
        assert_eq!(get_thread_messages(&db, &thread_id).unwrap().len(), 1);
    }

    #[test]
    fn append_thread_messages_replaces_only_the_latest_remote_message() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let initial = vec![
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("older partial answer".to_string()),
                blocks: json!([{ "type": "text", "content": "older partial answer" }]),
                status: MessageStatusDto::Streaming,
                turn_engine_id: Some("remote-older".to_string()),
                remote_turn_id: Some("remote-older".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:00:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("latest partial answer".to_string()),
                blocks: json!([{ "type": "text", "content": "latest partial answer" }]),
                status: MessageStatusDto::Streaming,
                turn_engine_id: Some("remote-latest".to_string()),
                remote_turn_id: Some("remote-latest".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:01:00.000".to_string()),
            },
        ];
        append_thread_messages(&db, &thread_id, &initial).unwrap();

        let refreshed = vec![
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("older complete answer".to_string()),
                blocks: json!([{ "type": "text", "content": "older complete answer" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-older".to_string()),
                remote_turn_id: Some("remote-older".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:00:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("latest complete answer".to_string()),
                blocks: json!([
                    { "type": "text", "content": "latest complete answer" },
                    { "type": "action", "actionId": "latest-action" }
                ]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-latest".to_string()),
                remote_turn_id: Some("remote-latest".to_string()),
                turn_model_id: Some("gpt-5.3-codex".to_string()),
                turn_reasoning_effort: Some("high".to_string()),
                token_input: 12,
                token_output: 34,
                created_at: Some("2026-08-08 12:01:00.000".to_string()),
            },
        ];

        assert_eq!(
            append_thread_messages(&db, &thread_id, &refreshed).unwrap(),
            0
        );

        let messages = get_thread_messages(&db, &thread_id).unwrap();
        let older = messages
            .iter()
            .find(|message| message.content.as_deref() == Some("older partial answer"))
            .unwrap();
        assert_eq!(older.content.as_deref(), Some("older partial answer"));
        assert_eq!(older.status, MessageStatusDto::Streaming);

        let latest = messages
            .iter()
            .find(|message| message.content.as_deref() == Some("latest complete answer"))
            .unwrap();
        assert_eq!(latest.content.as_deref(), Some("latest complete answer"));
        assert_eq!(latest.status, MessageStatusDto::Completed);
        assert_eq!(latest.turn_model_id.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(
            latest
                .token_usage
                .as_ref()
                .map(|usage| (usage.input, usage.output)),
            Some((12, 34))
        );
        assert!(latest
            .blocks
            .as_ref()
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|block| block.get("actionId").and_then(Value::as_str) == Some("latest-action")));
    }

    #[test]
    fn bind_local_turn_to_remote_turn_prevents_future_remote_duplicates() {
        let db = test_db();
        let thread_id = test_thread(&db);
        insert_message(
            &db,
            &thread_id,
            "user",
            Some("local prompt".to_string()),
            Some(json!([{ "type": "text", "content": "local prompt" }])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();
        let assistant = insert_message(
            &db,
            &thread_id,
            "assistant",
            Some("local answer".to_string()),
            Some(json!([{ "type": "text", "content": "local answer" }])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();
        bind_local_turn_to_remote_turn(&db, &assistant.id, "remote-turn").unwrap();

        let imported = vec![
            ImportedMessageRecord {
                role: "user".to_string(),
                content: Some("local prompt".to_string()),
                blocks: json!([{ "type": "text", "content": "local prompt" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-turn".to_string()),
                remote_turn_id: Some("remote-turn".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:00:00.000".to_string()),
            },
            ImportedMessageRecord {
                role: "assistant".to_string(),
                content: Some("local answer".to_string()),
                blocks: json!([{ "type": "text", "content": "local answer" }]),
                status: MessageStatusDto::Completed,
                turn_engine_id: Some("remote-turn".to_string()),
                remote_turn_id: Some("remote-turn".to_string()),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some("2026-08-08 12:01:00.000".to_string()),
            },
        ];

        assert_eq!(
            append_thread_messages(&db, &thread_id, &imported).unwrap(),
            0
        );
        assert_eq!(get_thread_messages(&db, &thread_id).unwrap().len(), 2);
    }

    #[test]
    fn build_search_messages_query_quotes_free_text_terms() {
        assert_eq!(
            build_search_messages_query("foo bar:baz \"quoted\""),
            Some("\"foo\" AND \"bar:baz\" AND \"quoted\"".to_string())
        );
    }

    #[test]
    fn build_search_messages_query_preserves_quoted_phrases() {
        assert_eq!(
            build_search_messages_query("\"foo bar\" baz"),
            Some("\"foo bar\" AND \"baz\"".to_string())
        );
    }

    #[test]
    fn build_search_messages_query_tolerates_unterminated_quotes() {
        assert_eq!(
            build_search_messages_query("foo \"bar baz"),
            Some("\"foo\" AND \"bar baz\"".to_string())
        );
    }

    #[test]
    fn build_search_messages_query_preserves_prefix_wildcards() {
        assert_eq!(
            build_search_messages_query("foo* bar*"),
            Some("\"foo\"* AND \"bar\"*".to_string())
        );
    }

    #[test]
    fn search_messages_accepts_non_fts_user_input() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        insert_user_message(
            &db,
            &thread.id,
            "searchable payload",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let results = search_messages(&db, &workspace_id, "foo:bar baz(");
        assert!(results.is_ok());
    }

    #[test]
    fn search_messages_finds_assistant_text_after_blocks_update() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        let message = insert_assistant_placeholder(&db, &thread.id, None, None, None).unwrap();

        let blocks = json!([
            { "type": "thinking", "content": "planning the reply" },
            { "type": "text", "content": "the flux capacitor needs recalibration" }
        ]);
        update_assistant_blocks_json(
            &db,
            &message.id,
            &blocks.to_string(),
            MessageStatusDto::Completed,
            None,
        )
        .unwrap();

        let results = search_messages(&db, &workspace_id, "capacitor").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message_id, message.id);
        assert!(results[0].snippet.contains("flux capacitor"));
    }

    #[test]
    fn migrations_backfill_assistant_content_for_legacy_rows() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        let message = insert_assistant_placeholder(&db, &thread.id, None, None, None).unwrap();

        // Simulate a row written before the fix: blocks_json holds the reply,
        // content stays NULL.
        let blocks = json!([
            { "type": "text", "content": "legacy assistant reply about quasars" }
        ]);
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE messages SET blocks_json = ?1, status = 'completed' WHERE id = ?2",
            params![blocks.to_string(), message.id],
        )
        .unwrap();
        drop(conn);

        assert!(search_messages(&db, &workspace_id, "quasars")
            .unwrap()
            .is_empty());

        db.run_migrations().expect("failed to rerun migrations");

        let results = search_messages(&db, &workspace_id, "quasars").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message_id, message.id);
    }

    #[test]
    fn search_messages_snippet_tracks_case_insensitive_fts_match() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        let content = format!(
            "{} Workspace marker {}",
            "opening prelude ".repeat(20),
            "closing context ".repeat(10)
        );
        insert_user_message(&db, &thread.id, &content, None, None, None, None).unwrap();

        let results = search_messages(&db, &workspace_id, "workspace").unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("Workspace marker"));
        assert!(!results[0].snippet.starts_with("opening prelude"));
    }

    #[test]
    fn search_messages_preserves_phrase_queries() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        insert_user_message(
            &db,
            &thread.id,
            "prefix foo bar suffix",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(
            &db,
            &thread.id,
            "prefix foo closing gap bar suffix",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let results = search_messages(&db, &workspace_id, "\"foo bar\"").unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("foo bar"));
    }

    #[test]
    fn search_messages_preserves_prefix_queries() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        insert_user_message(
            &db,
            &thread.id,
            "prefix foobar suffix",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(&db, &thread.id, "prefix bar suffix", None, None, None, None).unwrap();

        let results = search_messages(&db, &workspace_id, "foo*").unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("foobar"));
    }

    #[test]
    fn search_messages_unicode_snippet_tracks_fts_match() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "test")
                .unwrap();
        let content = format!(
            "{} CAFÉ marker {}",
            "opening prelude ".repeat(20),
            "closing context ".repeat(10)
        );
        insert_user_message(&db, &thread.id, &content, None, None, None, None).unwrap();

        let results = search_messages(&db, &workspace_id, "café").unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("CAFÉ marker"));
        assert!(!results[0].snippet.starts_with("opening prelude"));
    }

    #[test]
    fn search_messages_excludes_archived_threads() {
        let db = test_db();
        let workspace_id = test_workspace(&db);
        let active_thread =
            threads::create_thread(&db, &workspace_id, None, "codex", "gpt-5.3-codex", "active")
                .unwrap();
        let archived_thread = threads::create_thread(
            &db,
            &workspace_id,
            None,
            "codex",
            "gpt-5.3-codex",
            "archived",
        )
        .unwrap();

        insert_user_message(
            &db,
            &active_thread.id,
            "shared search marker active",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(
            &db,
            &archived_thread.id,
            "shared search marker archived",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        threads::archive_thread(&db, &archived_thread.id).unwrap();

        let results = search_messages(&db, &workspace_id, "shared").unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].thread_id, active_thread.id);
    }

    #[test]
    fn update_assistant_blocks_json_preserves_answered_approval_status() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let approval_id = "approval-1";
        let pending_blocks = approval_blocks_json(approval_id);
        let message = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(pending_blocks.clone()),
            MessageStatusDto::Streaming,
            None,
            None,
            None,
        )
        .unwrap();

        actions::insert_approval(
            &db,
            approval_id,
            &thread_id,
            &message.id,
            &ActionType::Command,
            "Run tests",
            &json!({}),
        )
        .unwrap();
        actions::answer_approval(&db, approval_id, "accept").unwrap();

        update_assistant_blocks_json(
            &db,
            &message.id,
            &pending_blocks.to_string(),
            MessageStatusDto::Completed,
            None,
        )
        .unwrap();

        let blocks = get_message_blocks(&db, &message.id).unwrap().unwrap();
        let (status, decision) = approval_block_status(&blocks).unwrap();
        assert_eq!(status, "answered");
        assert_eq!(decision, Some("accept"));
    }

    #[test]
    fn update_assistant_blocks_json_preserves_resolved_approval_without_decision() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let approval_id = "approval-runtime-1";
        let pending_blocks = approval_blocks_json(approval_id);
        let message = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(pending_blocks.clone()),
            MessageStatusDto::Streaming,
            None,
            None,
            None,
        )
        .unwrap();

        actions::insert_approval(
            &db,
            approval_id,
            &thread_id,
            &message.id,
            &ActionType::Command,
            "Run tests",
            &json!({}),
        )
        .unwrap();
        actions::resolve_approval(&db, approval_id).unwrap();

        update_assistant_blocks_json(
            &db,
            &message.id,
            &pending_blocks.to_string(),
            MessageStatusDto::Completed,
            None,
        )
        .unwrap();

        let blocks = get_message_blocks(&db, &message.id).unwrap().unwrap();
        let (status, decision) = approval_block_status(&blocks).unwrap();
        assert_eq!(status, "answered");
        assert_eq!(decision, None);
    }

    #[test]
    fn update_assistant_turn_model_id_updates_message_metadata() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let message = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(json!([])),
            MessageStatusDto::Streaming,
            Some("codex"),
            Some("gpt-5.1-codex-mini"),
            None,
        )
        .unwrap();

        update_assistant_turn_model_id(&db, &message.id, "gpt-5.3-codex").unwrap();

        let reloaded = get_thread_messages(&db, &thread_id).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].turn_model_id.as_deref(), Some("gpt-5.3-codex"));
    }

    #[test]
    fn get_message_loads_exact_message_without_thread_history() {
        let db = test_db();
        let first_thread_id = test_thread(&db);
        let second_thread_id = test_thread(&db);
        let first = insert_message(
            &db,
            &first_thread_id,
            "assistant",
            Some("first".to_string()),
            Some(json!([])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();
        let second = insert_message(
            &db,
            &second_thread_id,
            "assistant",
            Some("second".to_string()),
            Some(json!([])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();

        let loaded = get_message(&db, &second.id).unwrap().unwrap();
        assert_eq!(loaded.id, second.id);
        assert_eq!(loaded.thread_id, second_thread_id);
        assert_eq!(loaded.content.as_deref(), Some("second"));
        assert!(get_message(&db, "missing-message").unwrap().is_none());
        assert_ne!(loaded.id, first.id);
    }

    #[test]
    fn get_thread_messages_window_reconciles_stale_approval_blocks() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let approval_id = "approval-2";
        let pending_blocks = approval_blocks_json(approval_id);
        let message = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(pending_blocks.clone()),
            MessageStatusDto::Completed,
            None,
            None,
            None,
        )
        .unwrap();

        actions::insert_approval(
            &db,
            approval_id,
            &thread_id,
            &message.id,
            &ActionType::Command,
            "Run tests",
            &json!({}),
        )
        .unwrap();
        actions::answer_approval(&db, approval_id, "accept").unwrap();

        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
            params![pending_blocks.to_string(), message.id],
        )
        .unwrap();
        drop(conn);

        let window = get_thread_messages_window(&db, &thread_id, None, 20).unwrap();
        let blocks = window.messages[0].blocks.as_ref().unwrap();
        let (status, decision) = approval_block_status(blocks).unwrap();
        assert_eq!(status, "answered");
        assert_eq!(decision, Some("accept"));
    }

    #[test]
    fn get_thread_messages_window_reconciles_resolved_approval_without_decision() {
        let db = test_db();
        let thread_id = test_thread(&db);
        let approval_id = "approval-runtime-2";
        let pending_blocks = approval_blocks_json(approval_id);
        let message = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(pending_blocks.clone()),
            MessageStatusDto::Completed,
            None,
            None,
            None,
        )
        .unwrap();

        actions::insert_approval(
            &db,
            approval_id,
            &thread_id,
            &message.id,
            &ActionType::Command,
            "Run tests",
            &json!({}),
        )
        .unwrap();
        actions::resolve_approval(&db, approval_id).unwrap();

        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
            params![pending_blocks.to_string(), message.id],
        )
        .unwrap();
        drop(conn);

        let window = get_thread_messages_window(&db, &thread_id, None, 20).unwrap();
        let blocks = window.messages[0].blocks.as_ref().unwrap();
        let (status, decision) = approval_block_status(blocks).unwrap();
        assert_eq!(status, "answered");
        assert_eq!(decision, None);
    }

    #[test]
    fn clone_thread_messages_copies_message_history_and_metadata() {
        let db = test_db();
        let source_thread_id = test_thread(&db);
        let target_thread_id = test_thread(&db);

        let user_message = insert_user_message(
            &db,
            &source_thread_id,
            "Branch this history",
            Some(json!([{ "type": "text", "content": "Branch this history" }])),
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("medium"),
        )
        .unwrap();
        let assistant_message = insert_message(
            &db,
            &source_thread_id,
            "assistant",
            Some("Created branch preview".to_string()),
            Some(json!([{ "type": "text", "content": "Created branch preview" }])),
            MessageStatusDto::Completed,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("medium"),
        )
        .unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE messages SET token_input = 7, token_output = 11 WHERE id = ?1",
            params![assistant_message.id],
        )
        .unwrap();
        drop(conn);

        let cloned = clone_thread_messages(&db, &source_thread_id, &target_thread_id).unwrap();
        assert_eq!(cloned, 2);

        let source_messages = get_thread_messages(&db, &source_thread_id).unwrap();
        let target_messages = get_thread_messages(&db, &target_thread_id).unwrap();
        assert_eq!(source_messages.len(), target_messages.len());
        assert_ne!(source_messages[0].id, target_messages[0].id);
        assert_eq!(target_messages[0].role, "user");
        assert_eq!(
            target_messages[0].content.as_deref(),
            Some("Branch this history")
        );
        assert_eq!(target_messages[0].turn_engine_id.as_deref(), Some("codex"));
        assert_eq!(
            target_messages[0].turn_model_id.as_deref(),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            target_messages[0].turn_reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(target_messages[0].status, MessageStatusDto::Completed);
        assert_eq!(target_messages[1].role, "assistant");
        assert_eq!(
            target_messages[1].content.as_deref(),
            Some("Created branch preview")
        );
        assert_eq!(target_messages[1].status, MessageStatusDto::Completed);
        assert_eq!(
            target_messages[1]
                .token_usage
                .as_ref()
                .map(|usage| (usage.input, usage.output)),
            Some((7, 11))
        );
        assert!(
            target_messages[0].created_at <= target_messages[1].created_at,
            "cloned message ordering should be preserved",
        );
        let _ = user_message;
    }

    #[test]
    fn drop_last_turns_removes_latest_turn_and_pending_approvals() {
        let db = test_db();
        let thread_id = test_thread(&db);
        insert_user_message(&db, &thread_id, "turn 1", None, None, None, None).unwrap();
        insert_message(
            &db,
            &thread_id,
            "assistant",
            Some("answer 1".to_string()),
            Some(json!([])),
            MessageStatusDto::Completed,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(&db, &thread_id, "turn 2", None, None, None, None).unwrap();
        let pending_assistant = insert_message(
            &db,
            &thread_id,
            "assistant",
            None,
            Some(approval_blocks_json("approval-rollback")),
            MessageStatusDto::Streaming,
            None,
            None,
            None,
        )
        .unwrap();
        actions::insert_approval(
            &db,
            "approval-rollback",
            &thread_id,
            &pending_assistant.id,
            &ActionType::Command,
            "Run cleanup",
            &json!({}),
        )
        .unwrap();

        let removed = drop_last_turns(&db, &thread_id, 1).unwrap();
        assert_eq!(removed, 2);

        let remaining_messages = get_thread_messages(&db, &thread_id).unwrap();
        assert_eq!(remaining_messages.len(), 2);
        assert_eq!(remaining_messages[0].content.as_deref(), Some("turn 1"));
        assert_eq!(remaining_messages[1].content.as_deref(), Some("answer 1"));

        let conn = db.connect().unwrap();
        let approval_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM approvals WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(approval_count, 0);
    }

    #[test]
    fn drop_last_turns_ignores_mid_turn_steer_messages() {
        let db = test_db();
        let thread_id = test_thread(&db);
        insert_user_message(&db, &thread_id, "turn 1", None, None, None, None).unwrap();
        insert_message(
            &db,
            &thread_id,
            "assistant",
            Some("answer 1".to_string()),
            Some(json!([{ "type": "text", "content": "answer 1" }])),
            MessageStatusDto::Completed,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(&db, &thread_id, "turn 2", None, None, None, None).unwrap();
        insert_message(
            &db,
            &thread_id,
            "assistant",
            Some("working".to_string()),
            Some(json!([{ "type": "text", "content": "working" }])),
            MessageStatusDto::Completed,
            None,
            None,
            None,
        )
        .unwrap();
        insert_user_message(
            &db,
            &thread_id,
            "focus on tests",
            Some(steer_blocks_json("focus on tests")),
            None,
            None,
            None,
        )
        .unwrap();

        let removed = drop_last_turns(&db, &thread_id, 1).unwrap();
        assert_eq!(removed, 3);

        let remaining_messages = get_thread_messages(&db, &thread_id).unwrap();
        assert_eq!(remaining_messages.len(), 2);
        assert_eq!(remaining_messages[0].content.as_deref(), Some("turn 1"));
        assert_eq!(remaining_messages[1].content.as_deref(), Some("answer 1"));
    }
}
