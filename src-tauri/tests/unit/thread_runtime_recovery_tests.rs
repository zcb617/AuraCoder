use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{
    db,
    engines::events::ActionType,
    models::{MessageStatusDto, ThreadDto},
};

/// 为线程启动恢复测试创建独立临时数据库。
fn test_db() -> db::Database {
    let path = std::env::temp_dir().join(format!("auracoder-thread-recovery-{}.db", Uuid::new_v4()));
    db::Database::open(path).expect("failed to create recovery test database")
}

/// 在测试数据库中创建一个 Codex 本地线程。
fn test_thread(database: &db::Database) -> ThreadDto {
    let workspace_id = Uuid::new_v4().to_string();
    let root_path = std::env::temp_dir()
        .join(format!("auracoder-thread-recovery-workspace-{workspace_id}"));
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "INSERT INTO workspaces (id, name, root_path) VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id, "Recovery test", root_path.to_string_lossy()],
        )
        .expect("failed to create recovery workspace");
    db::threads::create_thread(database, &workspace_id, "codex", "gpt-test", "Recovery")
        .expect("failed to create recovery thread")
}

/// 读取消息状态文本，以验证启动恢复事务中的消息更新结果。
fn read_message_status(database: &db::Database, message_id: &str) -> String {
    let connection = database.connect().expect("database connection");
    connection
        .query_row(
            "SELECT status FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .expect("failed to read recovery message status")
}

/// 验证启动恢复会将上次进程留下的 assistant streaming 消息标记为 interrupted。
#[test]
fn recovery_marks_streaming_messages_interrupted() {
    let database = test_db();
    let thread = test_thread(&database);
    let message = db::messages::insert_assistant_placeholder(
        &database,
        &thread.id,
        None,
        None,
        None,
    )
    .expect("failed to create streaming assistant message");

    let report = ThreadService::recover_runtime_state(&database)
        .expect("runtime recovery should succeed");

    assert_eq!(report.messages_marked_interrupted, 1);
    assert_eq!(read_message_status(&database, &message.id), "interrupted");
}

/// 验证等待中的审批优先决定线程状态，即使 assistant 消息已完成。
#[test]
fn recovery_preserves_pending_approval_priority() {
    let database = test_db();
    let thread = test_thread(&database);
    let message = db::messages::insert_assistant_placeholder(
        &database,
        &thread.id,
        None,
        None,
        None,
    )
    .expect("failed to create assistant message");
    db::messages::complete_assistant_message(
        &database,
        &message.id,
        MessageStatusDto::Completed,
        None,
        None,
    )
    .expect("failed to complete assistant message");
    db::actions::insert_approval(
        &database,
        "pending-recovery-approval",
        &thread.id,
        &message.id,
        &ActionType::Other,
        "Pending recovery approval",
        &json!({}),
    )
    .expect("failed to insert pending approval");

    ThreadService::recover_runtime_state(&database).expect("runtime recovery should succeed");

    let recovered = db::threads::get_thread(&database, &thread.id)
        .expect("failed to query recovered thread")
        .expect("recovered thread should exist");
    assert_eq!(recovered.status, crate::models::ThreadStatusDto::AwaitingApproval);
}

/// 验证恢复状态使用最后一条 assistant 消息状态，并将其他状态恢复为 idle。
#[test]
fn recovery_uses_latest_assistant_status() {
    let database = test_db();
    let cases = [
        (Some("error"), crate::models::ThreadStatusDto::Error),
        (Some("completed"), crate::models::ThreadStatusDto::Completed),
        (Some("streaming"), crate::models::ThreadStatusDto::Idle),
        (Some("interrupted"), crate::models::ThreadStatusDto::Idle),
        (None, crate::models::ThreadStatusDto::Idle),
    ];

    for (message_status, expected_thread_status) in cases {
        let thread = test_thread(&database);
        if let Some(message_status) = message_status {
            let message = db::messages::insert_assistant_placeholder(
                &database,
                &thread.id,
                None,
                None,
                None,
            )
            .expect("failed to create assistant message");
            match message_status {
                "error" => db::messages::update_assistant_status(
                    &database,
                    &message.id,
                    MessageStatusDto::Error,
                ),
                "completed" => db::messages::update_assistant_status(
                    &database,
                    &message.id,
                    MessageStatusDto::Completed,
                ),
                "interrupted" => db::messages::update_assistant_status(
                    &database,
                    &message.id,
                    MessageStatusDto::Interrupted,
                ),
                _ => Ok(()),
            }
            .expect("failed to set assistant message status");
        }

        ThreadService::recover_runtime_state(&database).expect("runtime recovery should succeed");

        let recovered = db::threads::get_thread(&database, &thread.id)
            .expect("failed to query recovered thread")
            .expect("recovered thread should exist");
        assert_eq!(recovered.status, expected_thread_status);
    }
}

/// 验证启动恢复任一线程状态写入失败时，整批消息与线程更改均会回滚。
#[test]
fn recovery_rolls_back_whole_batch_on_write_failure() {
    let database = test_db();
    let thread = test_thread(&database);
    let message = db::messages::insert_assistant_placeholder(
        &database,
        &thread.id,
        None,
        None,
        None,
    )
    .expect("failed to create streaming assistant message");
    db::threads::update_thread_status(
        &database,
        &thread.id,
        crate::models::ThreadStatusDto::Error,
    )
    .expect("failed to set pre-recovery thread status");
    let connection = database.connect().expect("database connection");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_runtime_recovery_thread_update
             BEFORE UPDATE OF status ON threads
             WHEN NEW.id = '{}'
             BEGIN SELECT RAISE(ABORT, 'forced recovery failure'); END;",
            thread.id
        ))
        .expect("failed to install recovery failure trigger");

    let result = ThreadService::recover_runtime_state(&database);

    assert!(result.is_err());
    assert_eq!(read_message_status(&database, &message.id), "streaming");
    let unchanged = db::threads::get_thread(&database, &thread.id)
        .expect("failed to query unchanged thread")
        .expect("thread should exist after rollback");
    assert_eq!(unchanged.status, crate::models::ThreadStatusDto::Error);
}
