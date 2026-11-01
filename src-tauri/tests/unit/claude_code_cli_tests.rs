use std::{fs, sync::Arc};

use super::*;
use crate::{
    config::app_config::AppConfig,
    engines::EngineManager,
    git::{repo::FileTreeCache, watcher::GitWatcherManager},
    models::SshConnectionInput,
    power::KeepAwakeManager,
    state::{AppState, TurnManager},
    terminal::TerminalManager,
    terminal_notifications::TerminalNotificationManager,
};
use uuid::Uuid;

/// 为 Claude Code 会话测试创建独立数据库和应用状态。
/// 为 Claude Code 会话测试创建独立数据库和应用状态。
fn test_app_state() -> AppState {
    // 创建隔离的测试数据库并绑定应用依赖。
    let root =
        std::env::temp_dir().join(format!("auracoder-claude-archive-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("failed to create test root");
    let db = crate::db::Database::open(root.join("workspaces.db"))
        .expect("failed to create test database");
    AppState {
        db: db.clone(),
        config: Arc::new(AppConfig::default()),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        engines: Arc::new(EngineManager::new()),
        git_watchers: Arc::new(GitWatcherManager::default()),
        terminals: Arc::new(TerminalManager::default()),
        notifications: Arc::new(TerminalNotificationManager::default()),
        keep_awake: Arc::new(KeepAwakeManager::new()),
        turns: Arc::new(TurnManager::default()),
        file_tree_cache: Arc::new(FileTreeCache::new()),
        extension_catalog_refreshes: Arc::new(
            crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
        ),
        scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
        computer_control_service: Arc::new(
            crate::computer_control_service::ComputerControlService::default(),
        ),
        auracoder_thread_mcp_service: Arc::new(
            crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()),
        ),
        mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
        remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
        ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
    }
}

/// 验证未登记远端服务时，SSH Claude 归档接口仍按既有空操作语义返回。
/// 验证未登记远端服务时，SSH Claude 归档接口仍按既有空操作语义返回。
#[tokio::test]
async fn ssh_archive_hook_returns_without_remote_archive_service() {
    // 创建可定位的 SSH workspace 和线程，供统一 CLI 接口完成归档调用。
    let state = test_app_state();
    let connection_id = format!("ssh-test-{}", Uuid::new_v4());
    let input = SshConnectionInput {
        display_name: "Claude archive test SSH".to_string(),
        host_name: "192.0.2.10".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: None,
    };
    let connection = crate::db::ssh_connections::insert(
        &state.db,
        &connection_id,
        "manual",
        &input,
        "ssh-ed25519",
        "test-key",
    )
    .expect("failed to create test SSH connection");
    crate::db::ssh_connections::set_status_if_current(
        &state.db,
        &connection.id,
        &connection.updated_at,
        crate::db::ssh_connections::STATUS_OK,
        None,
    )
    .expect("failed to mark test SSH connection ready");
    let workspace = crate::db::workspaces::create_ssh_workspace(
        &state.db,
        &connection_id,
        "Claude archive test workspace",
        &format!("/tmp/auracoder-claude-archive-{}", Uuid::new_v4()),
    )
    .expect("failed to create test SSH workspace");
    let thread = crate::db::threads::create_thread(
        &state.db,
        &workspace.id,
        "claude",
        "claude-sonnet-4-6",
        "Claude archive test thread",
    )
    .expect("failed to create test Claude thread");
    let context = CliExecutionContext::from_workspace(&workspace)
        .expect("failed to build Claude SSH execution context");

    // 故意不登记 Claude 远端服务；若实现访问了远端归档协议，统一接口调用应失败。
    let cli = ClaudeCodeCli::new(state);
    let cli: &dyn CliTool = &cli;
    cli.archive_thread(&context, &thread, "remote-claude-thread")
        .await
        .expect("SSH Claude archive hook should not require remote service");
}

/// 验证 Claude 会话搜索匹配标题或标识，并排除不匹配会话。
/// 验证 Claude 会话搜索匹配标题或标识，并排除不匹配会话。
#[test]
fn local_claude_session_search_matches_title_or_id_and_excludes_other_sessions() {
    // 使用缺少结构化分类的会话摘要验证原有搜索兼容行为。
    let session = ClaudeSessionSummary {
        id: "session-abc123".to_string(),
        cwd: "/workspace/project".to_string(),
        title: "Deploy production".to_string(),
        updated_at: "2026-08-27T00:00:00Z".to_string(),
        // 测试缺失结构化会话分类时仍保持摘要兼容。
        conversation_kind: None,
    };

    assert!(matches_claude_session_search(&session, Some("production")));
    assert!(matches_claude_session_search(&session, Some("abc123")));
    assert!(matches_claude_session_search(&session, Some("DEPLOY")));
    assert!(!matches_claude_session_search(&session, Some("staging")));
    assert!(matches_claude_session_search(&session, None));
}
