use super::{
    LocalCliHandle, LocalCliService, LocalCliServiceEntryState,
    LocalCliServiceLifecycleRegistry, LocalOpenCodeServiceHandle,
};
use crate::{
    auracoder_thread_mcp_service::AuraCoderThreadMcpService,
    computer_control_service::ComputerControlService,
    config::app_config::AppConfig,
    db::Database,
    engines::EngineManager,
    git::{repo::FileTreeCache, watcher::GitWatcherManager},
    mcp_gateway::AuraCoderMcpGateway,
    power::KeepAwakeManager,
    scheduled_tasks::ScheduledTaskManager,
    state::{AppState, TurnManager},
    terminal::TerminalManager,
    terminal_notifications::TerminalNotificationManager,
};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// 构造生命周期登记测试所需的已启动 MCP Gateway。
async fn test_gateway() -> Arc<AuraCoderMcpGateway> {
    let path = std::env::temp_dir().join(format!(
        "auracoder-local-cli-lifecycle-{}.db",
        uuid::Uuid::new_v4()
    ));
    let _db = Database::open(path).expect("test database should open");
    let gateway = Arc::new(AuraCoderMcpGateway::new());
    gateway.start().await.expect("test gateway should start");
    gateway
}

/// 构造本地生命周期测试使用的完整应用状态。
fn test_app_state() -> AppState {
    let root = std::env::temp_dir().join(format!("auracoder-local-state-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("failed to create local state root");
    let db = Database::open(root.join("workspaces.db")).expect("failed to create local state db");
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
        computer_control_service: Arc::new(ComputerControlService::default()),
        auracoder_thread_mcp_service: Arc::new(AuraCoderThreadMcpService::new(db)),
        mcp_gateway: Arc::new(AuraCoderMcpGateway::new()),
        remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
        ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
    }
}

/// 将测试用 Ready OpenCode 服务放入独立生命周期 Registry，隔离全局服务状态。
async fn test_registry(
    gateway: Arc<AuraCoderMcpGateway>,
    token: String,
) -> LocalCliServiceLifecycleRegistry {
    let registry = LocalCliServiceLifecycleRegistry {
        services: RwLock::new(std::collections::HashMap::new()),
        resource_dir: RwLock::new(None),
        mutation_lock: Mutex::new(()),
        mcp_gateway: RwLock::new(Some(gateway)),
        factory: RwLock::new(None),
    };
    registry.services.write().await.insert(
        "opencode".to_string(),
        Arc::new(LocalCliService {
            cli_id: "opencode".to_string(),
            generation: 1,
            handle: LocalCliHandle::OpenCode(Arc::new(LocalOpenCodeServiceHandle::new(
                "http://127.0.0.1:4096".to_string(),
                "test-token".to_string(),
            ))),
            cli: Arc::new(crate::cli_tools::opencode::OpenCodeCli::new(test_app_state())),
            mcp_token: token,
            state: Mutex::new(LocalCliServiceEntryState::Ready),
        }),
    );
    registry
}

/// 验证 Ready OpenCode 服务可通过私有 Token 登记并清理可信上下文。
#[tokio::test]
async fn ready_opencode_service_registers_and_clears_mcp_context() {
    let gateway = test_gateway().await;
    let lease = gateway
        .register_client(
            "opencode",
            "opencode",
            Arc::new(crate::cli_tools::opencode::OpenCodeCli::new(test_app_state())),
        )
        .await
        .expect("OpenCode test lease should be issued");
    let token = lease.token.clone();
    let registry = test_registry(gateway.clone(), token.clone()).await;

    registry
        .register_mcp_context("opencode", "engine-thread", "assistant-message")
        .await
        .expect("OpenCode context should register");
    registry
        .clear_mcp_context("opencode")
        .await
        .expect("OpenCode context should clear");

    registry
        .clear_mcp_context("opencode")
        .await
        .expect("clearing an already empty context should remain idempotent");

    gateway.shutdown().await;
}

/// 验证不存在 Ready 服务、未绑定 Gateway 和失效租约均返回业务错误且不泄露 Token。
#[tokio::test]
async fn mcp_context_errors_preserve_business_boundary_without_token() {
    let missing_registry = LocalCliServiceLifecycleRegistry::default();
    let missing_service_error = missing_registry
        .register_mcp_context("opencode", "engine-thread", "assistant-message")
        .await
        .expect_err("missing service should fail");
    assert!(missing_service_error
        .to_string()
        .contains("服务未在 AuraCoder 启动阶段登记"));

    let gateway = test_gateway().await;
    let lease = gateway
        .register_client(
            "opencode",
            "opencode",
            Arc::new(crate::cli_tools::opencode::OpenCodeCli::new(test_app_state())),
        )
        .await
        .expect("OpenCode test lease should be issued");
    let token = lease.token.clone();
    let registry = test_registry(gateway.clone(), token.clone()).await;
    gateway.revoke_client(&token).await;

    let revoked_error = registry
        .register_mcp_context("opencode", "engine-thread", "assistant-message")
        .await
        .expect_err("revoked lease should fail");
    let revoked_error_chain = format!("{revoked_error:#}");
    assert!(revoked_error_chain.contains("client lease 不存在或已撤销"));
    assert!(!revoked_error_chain.contains(&token));

    let unbound_registry = LocalCliServiceLifecycleRegistry::default();
    let service = registry
        .services
        .read()
        .await
        .get("opencode")
        .cloned()
        .expect("test service should exist");
    unbound_registry
        .services
        .write()
        .await
        .insert("opencode".to_string(), service);
    let unbound_error = unbound_registry
        .clear_mcp_context("opencode")
        .await
        .expect_err("unbound gateway should fail");
    assert!(unbound_error
        .to_string()
        .contains("尚未绑定 MCP Gateway"));

    gateway.shutdown().await;
}
