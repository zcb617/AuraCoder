use super::*;
use crate::{
    config::app_config::AppConfig,
    db::Database,
    engines::EngineManager,
    git::{repo::FileTreeCache, watcher::GitWatcherManager},
    power::KeepAwakeManager,
    scheduled_tasks::ScheduledTaskManager,
    state::{AppState, TurnManager},
    terminal::TerminalManager,
    terminal_notifications::TerminalNotificationManager,
};
use std::sync::Arc;

/// 构造权限能力测试所需的完整应用状态，避免权限读取依赖真实桌面运行时。
fn test_app_state() -> AppState {
    let root = std::env::temp_dir().join(format!(
        "auracoder-codex-permissions-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("failed to create test root");
    let db = Database::open(root.join("workspaces.db")).expect("failed to create test database");
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
        scheduled_tasks: Arc::new(ScheduledTaskManager::new()),
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

/// 构造权限能力读取使用的本机 workspace 上下文。
fn test_context() -> CliExecutionContext {
    CliExecutionContext {
        workspace_id: "workspace".to_string(),
        root_path: "/tmp/workspace".to_string(),
        location_kind: CliLocationKind::Local,
        ssh_connection_id: None,
    }
}

/// 读取指定原始权限 JSON 中的 MCP elicitation 自动批准能力。
async fn auto_approval(permission_mode: Option<&str>) -> bool {
    let cli = CodexCli::new(test_app_state());
    cli.auto_approve_mcp_elicitations(&test_context(), &thread(permission_mode, None))
        .await
        .expect("permission mode should be readable")
}

#[tokio::test]
async fn explicit_auto_marker_and_legacy_auto_permissions_enable_auto_approval() {
    assert!(
        auto_approval(Some(
            r#"{"autoApproveMcpElicitations":true,"approvalPolicy":"never"}"#
        ))
        .await
    );
    assert!(
        auto_approval(Some(
            r#"{"approvalPolicy":"on-request","sandboxMode":"workspace-write","allowNetwork":true}"#
        ))
        .await
    );
}

#[tokio::test]
async fn non_auto_permissions_and_explicit_false_disable_auto_approval() {
    assert!(!auto_approval(Some(
        r#"{"autoApproveMcpElicitations":false,"approvalPolicy":"on-request","sandboxMode":"workspace-write","allowNetwork":true}"#
    ))
    .await);
    assert!(!auto_approval(Some(
        r#"{"approvalPolicy":"on-request","sandboxMode":"workspace-write","allowNetwork":false}"#
    ))
    .await);
    assert!(
        !auto_approval(Some(
            r#"{"approvalPolicy":"untrusted","sandboxMode":"read-only","allowNetwork":false}"#
        ))
        .await
    );
    assert!(!auto_approval(Some("{}")).await);
    assert!(!auto_approval(Some("not-json")).await);
}

#[test]
fn raw_permissions_value_writes_and_clears_auto_marker_while_preserving_unknown_fields() {
    let thread = thread(
        Some(r#"{"unknown":{"keep":true},"autoApproveMcpElicitations":false}"#),
        None,
    );
    let saved = raw_permissions_value(
        &thread,
        Some("on-request"),
        Some("workspace-write"),
        Some(true),
        Some(true),
    );
    assert_eq!(saved["autoApproveMcpElicitations"], json!(true));
    assert_eq!(saved["unknown"], json!({"keep": true}));

    let cleared = raw_permissions_value(&thread, None, None, None, None);
    assert!(cleared.get("autoApproveMcpElicitations").is_none());
    assert_eq!(cleared["unknown"], json!({"keep": true}));
}

/// 验证 Codex 局部权限更新只在权限边界变化时清除 MCP 自动授权标记。
#[tokio::test]
async fn patch_runtime_permissions_clears_marker_for_permission_updates() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "auracoder-codex-patch-permissions-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("failed to create test workspace root");
    let workspace = db::workspaces::upsert_workspace(&state.db, root.to_string_lossy().as_ref())
        .expect("failed to create test workspace");
    let context = CliExecutionContext::from_workspace(&workspace)
        .expect("failed to build test execution context");
    let mut thread = db::threads::create_thread(
        &state.db,
        &workspace.id,
        "codex",
        "gpt-5.3-codex",
        "Permissions",
    )
    .expect("failed to create test thread");
    let db = state.db.clone();
    let cli = CodexCli::new(state);
    let cases = vec![
        (
            "approval",
            CliRuntimePermissionPatch {
                // 审批策略更新代表权限边界变化，必须清除自动授权标记。
                approval_policy: Some(Some(json!("on-request"))),
                ..Default::default()
            },
            true,
        ),
        (
            "sandbox",
            CliRuntimePermissionPatch {
                // 沙箱模式更新代表权限边界变化，必须清除自动授权标记。
                sandbox_mode: Some(None),
                ..Default::default()
            },
            true,
        ),
        (
            "network",
            CliRuntimePermissionPatch {
                // 网络开关更新代表权限边界变化，必须清除自动授权标记。
                allow_network: Some(Some(false)),
                ..Default::default()
            },
            true,
        ),
        (
            "permission_profile",
            CliRuntimePermissionPatch {
                // 权限配置对象更新代表权限边界变化，必须清除自动授权标记。
                permission_profile: Some(None),
                ..Default::default()
            },
            true,
        ),
        (
            "approvals_reviewer",
            CliRuntimePermissionPatch {
                // 仅审核人更新不改变权限边界，应保留自动授权标记。
                approvals_reviewer: Some(Some("reviewer".to_string())),
                ..Default::default()
            },
            false,
        ),
    ];

    for (name, patch, clears_marker) in cases {
        thread = db::threads::update_thread_permissions(
            &db,
            &thread.id,
            Some(r#"{"autoApproveMcpElicitations":true,"unknown":{"keep":true}}"#),
        )
        .expect("failed to seed automatic approval marker");
        let updated = cli
            .patch_runtime_permissions(&context, &thread, patch)
            .await
            .expect("failed to patch Codex runtime permissions");
        let object = updated
            .permission_mode
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .expect("updated permissions should be a JSON object");
        assert_eq!(
            object.get("autoApproveMcpElicitations").is_none(),
            clears_marker,
            "{name} permission update marker behavior is incorrect"
        );
        assert_eq!(object["unknown"], json!({"keep": true}));
        thread = updated;
    }
}
