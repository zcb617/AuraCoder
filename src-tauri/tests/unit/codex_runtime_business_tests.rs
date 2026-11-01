use std::{path::PathBuf, sync::Arc};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;
use crate::{
    config::app_config::AppConfig,
    db,
    engines::{events::ActionType, EngineManager},
    git::{repo::FileTreeCache, watcher::GitWatcherManager},
    models::ThreadStatusDto,
    power::KeepAwakeManager,
    state::{AppState, TurnManager},
};

/// 为 Codex 运行时业务测试创建独立临时数据库。
fn test_database() -> db::Database {
    let path = std::env::temp_dir().join(format!("auracoder-codex-runtime-{}.db", Uuid::new_v4()));
    db::Database::open(path).expect("failed to create Codex runtime test database")
}

/// 构造绑定测试数据库和应用依赖的 Codex AppState。
fn test_state(database: db::Database) -> AppState {
    AppState {
        db: database.clone(),
        config: Arc::new(AppConfig::default()),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        engines: Arc::new(EngineManager::new()),
        git_watchers: Arc::new(GitWatcherManager::default()),
        terminals: Arc::new(crate::terminal::TerminalManager::default()),
        notifications: Arc::new(crate::terminal_notifications::TerminalNotificationManager::default()),
        keep_awake: Arc::new(KeepAwakeManager::new()),
        turns: Arc::new(TurnManager::default()),
        file_tree_cache: Arc::new(FileTreeCache::new()),
        extension_catalog_refreshes: Arc::new(
            crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
        ),
        scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
        computer_control_service: Arc::new(crate::computer_control_service::ComputerControlService::default()),
        mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
        auracoder_thread_mcp_service: Arc::new(
            crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(database),
        ),
        remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
        ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
    }
}

/// 在测试数据库中创建一个 Codex 线程。
fn test_thread(database: &db::Database) -> crate::models::ThreadDto {
    let workspace_id = Uuid::new_v4().to_string();
    let root_path = PathBuf::from(std::env::temp_dir())
        .join(format!("auracoder-codex-workspace-{workspace_id}"));
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "INSERT INTO workspaces (id, name, root_path) VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id, "Codex runtime test", root_path.to_string_lossy()],
        )
        .expect("failed to create Codex test workspace");
    db::threads::create_thread(database, &workspace_id, "codex", "gpt-test", "Codex test")
        .expect("failed to create Codex test thread")
}

/// 创建带目标审批块和可保留附加块的测试消息与 pending 审批记录。
fn insert_pending_approval(
    database: &db::Database,
    thread_id: &str,
    approval_id: &str,
    approval_key: &str,
) -> String {
    let message = db::messages::insert_assistant_placeholder(
        database,
        thread_id,
        None,
        None,
        None,
    )
    .expect("failed to create approval message");
    let mut approval_block = json!({
        "type": "approval",
        "status": "pending",
        "preserved": "approval payload"
    });
    approval_block[approval_key] = json!(approval_id);
    let blocks = json!([approval_block, { "type": "text", "text": "keep this block" }]);
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
            rusqlite::params![blocks.to_string(), message.id],
        )
        .expect("failed to set approval message blocks");
    db::actions::insert_approval(
        database,
        approval_id,
        thread_id,
        &message.id,
        &ActionType::Other,
        "Runtime approval",
        &json!({}),
    )
    .expect("failed to insert pending approval");
    message.id
}

/// 验证没有提供的运行时 metadata 字段继续保留原值。
#[test]
fn runtime_partial_metadata_preserves_unprovided_fields() {
    let original = json!({
        "codexThreadStatus": "active",
        "codexThreadActiveFlags": ["waitingOnApproval"],
        "codexPreview": "existing preview",
        "codexSyncRequired": true,
        "codexSyncUpdatedAt": "existing timestamp",
        "codexSyncReason": "existing reason",
        "other": "keep"
    });

    let merged = merge_codex_runtime_event_metadata(
        Some(original.clone()),
        None,
        &[],
        None,
        None,
        None,
    );

    assert_eq!(merged, original);
}

/// 验证空的事件字段只清除该事件明确要求更新的 metadata 键。
#[test]
fn runtime_empty_metadata_fields_clear_only_requested_keys() {
    let merged = merge_codex_runtime_event_metadata(
        Some(json!({
            "codexThreadStatus": "active",
            "codexThreadActiveFlags": ["waitingOnApproval"],
            "codexPreview": "existing preview",
            "codexSyncRequired": true,
            "codexSyncUpdatedAt": "existing timestamp",
            "codexSyncReason": "existing reason",
            "other": "keep"
        })),
        Some(" "),
        &[],
        Some(" "),
        Some(false),
        None,
    );

    assert!(merged.get("codexThreadStatus").is_none());
    assert!(merged.get("codexThreadActiveFlags").is_none());
    assert!(merged.get("codexPreview").is_none());
    assert_eq!(merged["codexSyncRequired"], false);
    assert!(merged["codexSyncUpdatedAt"].as_str().is_some());
    assert!(merged["codexSyncReason"].is_null());
    assert_eq!(merged["other"], "keep");
}

/// 验证已有运行轮次会阻止远端状态映射覆盖本地线程状态。
#[test]
fn runtime_status_mapping_preserves_local_turn() {
    let status = thread_business::map_codex_thread_status_to_local(
        Some("systemError"),
        &[],
        true,
    );

    assert_eq!(status, None);
}

/// 验证真实审批事务只更新目标审批块并保留其他消息块内容。
#[tokio::test]
async fn runtime_approval_resolution_updates_card_and_preserves_other_blocks() {
    let database = test_database();
    let thread = test_thread(&database);
    let message_id = insert_pending_approval(&database, &thread.id, "approval-camel", "approvalId");
    db::threads::update_thread_status(&database, &thread.id, ThreadStatusDto::AwaitingApproval)
        .expect("failed to set awaiting approval thread status");
    let state = test_state(database.clone());
    state
        .turns
        .try_register(&thread.id, CancellationToken::new())
        .await;
    let codex = CodexCli::new(state);

    let resolution = codex
        .resolve_runtime_approval("approval-camel")
        .await
        .expect("approval transaction should succeed")
        .expect("pending approval should be resolved");

    assert_eq!(resolution.thread_id, thread.id);
    assert_eq!(
        resolution.updated_thread.expect("active thread update").status,
        ThreadStatusDto::Streaming
    );
    let connection = database.connect().expect("database connection");
    let raw_blocks: String = connection
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .expect("failed to read resolved approval blocks");
    let blocks: serde_json::Value = serde_json::from_str(&raw_blocks).expect("valid blocks JSON");
    assert_eq!(blocks[0]["status"], "answered");
    assert_eq!(blocks[0]["preserved"], "approval payload");
    assert_eq!(blocks[1]["text"], "keep this block");
}

/// 验证迟到的终态审批事件不会覆盖已过期审批状态或卡片。
#[tokio::test]
async fn runtime_approval_resolution_preserves_expired_state() {
    let database = test_database();
    let thread = test_thread(&database);
    let message_id = insert_pending_approval(&database, &thread.id, "approval-expired", "approval_id");
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "UPDATE approvals SET status = 'expired' WHERE id = ?1",
            rusqlite::params!["approval-expired"],
        )
        .expect("failed to expire approval");
    let before_blocks: String = connection
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .expect("failed to read approval blocks");
    let codex = CodexCli::new(test_state(database.clone()));

    assert!(codex
        .resolve_runtime_approval("approval-expired")
        .await
        .expect("expired approval should be ignored")
        .is_none());
    let status: String = connection
        .query_row(
            "SELECT status FROM approvals WHERE id = ?1",
            rusqlite::params!["approval-expired"],
            |row| row.get(0),
        )
        .expect("failed to read expired approval state");
    let after_blocks: String = connection
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .expect("failed to read approval blocks after event");
    assert_eq!(status, "expired");
    assert_eq!(after_blocks, before_blocks);
}

/// 验证重复审批完成事件不会重复修改数据库或重新产生业务结果。
#[tokio::test]
async fn runtime_approval_resolution_is_repeat_safe() {
    let database = test_database();
    let thread = test_thread(&database);
    insert_pending_approval(&database, &thread.id, "approval-repeat", "approvalId");
    let codex = CodexCli::new(test_state(database.clone()));

    assert!(codex
        .resolve_runtime_approval("approval-repeat")
        .await
        .expect("first approval resolution should succeed")
        .is_some());
    assert!(codex
        .resolve_runtime_approval("approval-repeat")
        .await
        .expect("repeated approval event should be ignored")
        .is_none());
}

/// 验证缺失匹配审批块时事务失败且审批记录和消息原文均保持不变。
#[tokio::test]
async fn runtime_approval_resolution_rolls_back_on_invalid_blocks() {
    let database = test_database();
    let thread = test_thread(&database);
    let message_id = insert_pending_approval(&database, &thread.id, "approval-invalid", "approvalId");
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "UPDATE messages SET blocks_json = 'not-json' WHERE id = ?1",
            rusqlite::params![message_id],
        )
        .expect("failed to corrupt approval blocks for rollback test");
    let codex = CodexCli::new(test_state(database.clone()));

    assert!(codex
        .resolve_runtime_approval("approval-invalid")
        .await
        .is_err());
    let status: String = connection
        .query_row(
            "SELECT status FROM approvals WHERE id = ?1",
            rusqlite::params!["approval-invalid"],
            |row| row.get(0),
        )
        .expect("failed to read approval state after rollback");
    let blocks: String = connection
        .query_row(
            "SELECT blocks_json FROM messages WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .expect("failed to read approval blocks after rollback");
    assert_eq!(status, "pending");
    assert_eq!(blocks, "not-json");
}

/// 验证运行时线程恢复通过 Mapper 返回 Updated、AlreadyActive 和 NotFound 结构结果。
#[test]
fn runtime_restore_uses_structured_results() {
    let database = test_database();
    let mut thread = test_thread(&database);
    thread.engine_thread_id = Some("native-runtime-thread".to_string());
    let connection = database.connect().expect("database connection");
    connection
        .execute(
            "UPDATE threads SET engine_thread_id = ?1 WHERE id = ?2",
            rusqlite::params!["native-runtime-thread", thread.id],
        )
        .expect("failed to set native thread id");
    db::threads::archive_thread(&database, &thread.id).expect("failed to archive runtime thread");

    assert!(matches!(
        db::threads::restore_runtime_thread(&database, "codex", "native-runtime-thread")
            .expect("first runtime restore should succeed"),
        db::threads::RuntimeThreadRestoreResult::Updated(_)
    ));
    assert!(matches!(
        db::threads::restore_runtime_thread(&database, "codex", "native-runtime-thread")
            .expect("repeated runtime restore should succeed"),
        db::threads::RuntimeThreadRestoreResult::AlreadyActive(_)
    ));
    assert!(matches!(
        db::threads::restore_runtime_thread(&database, "codex", "unknown-runtime-thread")
            .expect("unknown runtime thread lookup should succeed"),
        db::threads::RuntimeThreadRestoreResult::NotFound
    ));
}

/// 验证事件名、线程字段 camelCase 以及审批载荷 snake_case 与前端契约一致。
#[test]
fn runtime_event_payload_contracts_keep_existing_names() {
    let thread_update = ThreadUpdatedEvent {
        thread_id: "thread-1".to_string(),
        workspace_id: "workspace-1".to_string(),
        thread: None,
    };
    let thread_update_json = serde_json::to_value(thread_update).expect("thread event JSON");
    assert_eq!(thread_update_json["threadId"], "thread-1");
    assert_eq!(thread_update_json["workspaceId"], "workspace-1");
    assert!(thread_update_json.get("thread").is_none());

    let remote_removed = CodexRemoteThreadRemovedEvent {
        thread: crate::models::ThreadDto {
            id: "thread-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            engine_id: "codex".to_string(),
            model_id: "gpt-test".to_string(),
            engine_thread_id: Some("native-thread-1".to_string()),
            engine_metadata: None,
            plan_mode: None,
            send_method: None,
            reasoning_effort: None,
            permission_mode: None,
            title: "Codex test".to_string(),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            context_current_tokens: None,
            context_max_tokens: None,
            context_usage_updated_at: None,
            created_at: "2026-08-09T00:00:00Z".to_string(),
            last_activity_at: "2026-08-09T00:00:00Z".to_string(),
        },
        remote_action: "archived",
    };
    let remote_removed_json = serde_json::to_value(remote_removed).expect("remote event JSON");
    assert_eq!(remote_removed_json["remoteAction"], "archived");

    let approval_payload = approval_resolved_payload("approval-1");
    assert_eq!(approval_payload["type"], "ApprovalResolved");
    assert_eq!(approval_payload["approval_id"], "approval-1");
}
