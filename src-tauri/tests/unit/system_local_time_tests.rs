use std::path::PathBuf;

use crate::{
    db::{messages, threads, workspaces, Database},
    models::{MessageStatusDto, ThreadStatusDto},
    runtime_env,
};
use serde_json::json;
use uuid::Uuid;

/// 创建独立临时数据库，供本地时间回归场景隔离使用。
fn test_database() -> Database {
    let path = std::env::temp_dir().join(format!("auracoder-system-local-time-{}.db", Uuid::new_v4()));
    Database::open(path).expect("测试数据库应成功打开")
}

/// 创建消息排序测试所需的工作区和线程。
fn test_thread(db: &Database) -> String {
    let root = std::env::temp_dir().join(format!("auracoder-system-local-time-root-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("测试工作区根目录应创建");
    let workspace = workspaces::upsert_workspace(db, root.to_string_lossy().as_ref())
        .expect("测试工作区应创建");
    threads::create_thread(db, &workspace.id, "codex", "test-model", "时间测试")
        .expect("测试线程应创建")
        .id
}

/// 验证同一绝对时间的协议偏移输入统一为当前电脑本地偏移。
#[test]
fn normalize_time_uses_one_local_offset_for_equivalent_inputs() {
    let z = runtime_env::normalize_time_to_local("2026-08-18T10:00:00.000Z").unwrap();
    let offset = runtime_env::normalize_time_to_local("2026-08-18T18:00:00.000+08:00").unwrap();
    assert_eq!(z, offset);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&z)
            .unwrap()
            .offset()
            .local_minus_utc(),
        chrono::Local::now().offset().local_minus_utc()
    );
}

/// 验证历史 SQLite 无时区 UTC 字符串会转换为本机本地 RFC3339。
#[test]
fn normalize_time_converts_sqlite_utc_without_timezone() {
    let value = runtime_env::normalize_time_to_local("2026-08-18 10:00:00.000").unwrap();
    assert!(!value.ends_with('Z'));
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&value)
            .unwrap()
            .offset()
            .local_minus_utc(),
        chrono::Local::now().offset().local_minus_utc()
    );
}

/// 验证远端消息入库不保留 Z 后缀且按绝对时间升序读取。
#[test]
fn imported_messages_are_local_and_sorted_by_absolute_time() {
    let db = test_database();
    let thread_id = test_thread(&db);
    let imported = vec![
        messages::ImportedMessageRecord {
            role: "user".to_string(),
            content: Some("later".to_string()),
            blocks: json!([]),
            status: MessageStatusDto::Completed,
            turn_engine_id: None,
            remote_turn_id: Some("turn-later".to_string()),
            turn_model_id: None,
            turn_reasoning_effort: None,
            token_input: 0,
            token_output: 0,
            created_at: Some("2026-08-18T19:00:00+08:00".to_string()),
        },
        messages::ImportedMessageRecord {
            role: "assistant".to_string(),
            content: Some("earlier".to_string()),
            blocks: json!([]),
            status: MessageStatusDto::Completed,
            turn_engine_id: None,
            remote_turn_id: Some("turn-earlier".to_string()),
            turn_model_id: None,
            turn_reasoning_effort: None,
            token_input: 0,
            token_output: 0,
            created_at: Some("2026-08-18T10:00:00Z".to_string()),
        },
    ];
    messages::append_thread_messages(&db, &thread_id, &imported).unwrap();
    let result = messages::get_thread_messages(&db, &thread_id).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].content.as_deref(), Some("earlier"));
    assert!(result.iter().all(|message| !message.created_at.ends_with('Z')));
}

/// 验证混合时区消息窗口分页按绝对时间连续推进游标。
#[test]
fn mixed_offset_message_window_keeps_cursor_continuity() {
    let db = test_database();
    let thread_id = test_thread(&db);
    let values = [
        ("a", "2026-08-18T09:00:00Z"),
        ("b", "2026-08-18T12:00:00+02:00"),
        ("c", "2026-08-18T11:00:00Z"),
    ];
    for (id, created_at) in values {
        messages::append_thread_messages(
            &db,
            &thread_id,
            &[messages::ImportedMessageRecord {
                role: "user".to_string(),
                content: Some(id.to_string()),
                blocks: json!([]),
                status: MessageStatusDto::Completed,
                turn_engine_id: None,
                remote_turn_id: Some(format!("turn-{id}")),
                turn_model_id: None,
                turn_reasoning_effort: None,
                token_input: 0,
                token_output: 0,
                created_at: Some(created_at.to_string()),
            }],
        )
        .unwrap();
    }
    let first = messages::get_thread_messages_window(&db, &thread_id, None, 2).unwrap();
    assert_eq!(
        first.messages.iter().map(|message| message.content.as_deref()).collect::<Vec<_>>(),
        vec![Some("b"), Some("c")]
    );
    let cursor = first.next_cursor.expect("第一页应返回游标");
    let second = messages::get_thread_messages_window(&db, &thread_id, Some(&cursor), 2).unwrap();
    assert_eq!(second.messages.len(), 1);
    assert_eq!(second.messages[0].content.as_deref(), Some("a"));
}

/// 验证远端线程时间本地化后仍可设置 Claude 完整历史同步标记。
#[test]
fn remote_thread_timestamp_is_local_and_sync_flag_can_be_set() {
    let db = test_database();
    let root = PathBuf::from(format!("/tmp/auracoder-remote-time-{}", Uuid::new_v4()));
    let workspace = workspaces::upsert_workspace(&db, root.to_string_lossy().as_ref()).unwrap();
    let thread = threads::upsert_ssh_remote_thread_snapshot(
        &db,
        &workspace.id,
        "claude",
        "remote-thread",
        "unknown",
        "远端线程",
        ThreadStatusDto::Idle,
        &json!({"remote": true}),
        None,
        Some("2026-08-18T10:00:00Z"),
    )
    .unwrap();
    assert!(!thread.last_activity_at.ends_with('Z'));
    threads::set_claude_sync_required(&db, &thread.id, true).unwrap();
    assert!(threads::get_claude_sync_required(&db, &thread.id).unwrap());
}
