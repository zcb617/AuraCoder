    use super::{AuraCoderMcpGateway, GatewayState};
    use crate::{
        auracoder_thread_mcp_service::AuraCoderThreadMcpService,
        cli_tools::{factory::CliToolFactory, CliTool},
        computer_control_service::ComputerControlService,
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
    use serde_json::{json, Value};
    use std::sync::Arc;

    /// 构造 Gateway 直调 CliTool 测试使用的完整应用状态。
    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-mcp-gateway-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("failed to create gateway test root");
        let db = Database::open(root.join("workspaces.db")).expect("failed to create gateway test db");
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

    /// 验证 Gateway 只通过已登记 CliTool 提供目录，并拒绝模型 `_meta` 补充上下文。
    #[tokio::test]
    async fn gateway_registers_and_calls_current_cli_directly() {
        let state = test_app_state();
        let cli: Arc<dyn CliTool> = CliToolFactory::new(state.clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let gateway = AuraCoderMcpGateway::new();
        assert_eq!(gateway.status().await, GatewayState::Stopped);
        gateway.start().await.expect("gateway should start");
        let lease = gateway
            .register_client("codex", "test-instance", cli)
            .await
            .expect("client lease should be issued");
        let address = gateway.local_addr().await.expect("listener address");
        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list"
            }))
            .send()
            .await
            .expect("tools/list request should complete");
        let body: Value = response.json().await.expect("tools/list response JSON");
        assert_eq!(body["result"]["tools"].as_array().map(Vec::len), Some(2));

        let invalid_token = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth("invalid-token")
            .json(&json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}))
            .send()
            .await
            .expect("invalid token request should complete");
        assert_eq!(invalid_token.status(), reqwest::StatusCode::UNAUTHORIZED);
        let invalid_session = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .header("Mcp-Session-Id", "invalid-session")
            .json(&json!({"jsonrpc": "2.0", "id": 10, "method": "ping"}))
            .send()
            .await
            .expect("invalid session request should complete");
        assert_eq!(invalid_session.status(), reqwest::StatusCode::BAD_REQUEST);
        let pending_cancellation = tokio_util::sync::CancellationToken::new();
        gateway.active_calls.write().await.insert(
            "test-active-call".to_string(),
            (lease.token.clone(), pending_cancellation.clone()),
        );
        assert!(gateway.revoke_client(&lease.token).await);
        assert!(pending_cancellation.is_cancelled());

        let cli: Arc<dyn CliTool> = CliToolFactory::new(state.clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        let lease = gateway
            .register_client("codex", "test-instance", cli)
            .await
            .expect("replacement client lease should be issued");

        let no_context = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "get_auracoder_thread_message_count",
                    "arguments": {"thread_id": "thread"},
                    "_meta": {"thread_id": "forged", "turn_id": "forged"}
                }
            }))
            .send()
            .await
            .expect("tools/call request should complete");
        let no_context_body: Value = no_context.json().await.expect("tools/call response JSON");
        assert_eq!(no_context_body["result"]["isError"], true);
        assert!(no_context_body["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("invocation_context_missing"));

        let workspace = crate::db::workspaces::upsert_workspace(
            &state.db,
            &std::env::temp_dir()
                .join(format!("auracoder-gateway-workspace-{}", uuid::Uuid::new_v4()))
                .to_string_lossy(),
        )
        .expect("workspace should be created");
        let target_thread = crate::db::threads::create_thread(
            &state.db,
            &workspace.id,
            "codex",
            "model",
            "Gateway direct call",
        )
        .expect("thread should be created");
        crate::db::messages::insert_user_message(
            &state.db,
            &target_thread.id,
            "one message",
            None,
            Some("codex"),
            Some("model"),
            None,
        )
        .expect("message should be inserted");
        /*
        state.auracoder_thread_mcp_service.bind_engine_thread(
            "codex",
            "engine-thread",
            &workspace.id,
        );
        */
        let source_thread = crate::db::threads::create_thread(
            &state.db,
            &workspace.id,
            "codex",
            "model",
            "Gateway trusted source",
        )
        .expect("source thread should be created");
        let source_message = crate::db::messages::insert_assistant_placeholder(
            &state.db,
            &source_thread.id,
            Some("codex"),
            Some("model"),
            None,
        )
        .expect("assistant source message should be inserted");
        /*
        gateway
            .register_trusted_context(&lease.token, "engine-thread", "turn-1")
            .await
            .expect("trusted context should be registered");
        */
        gateway
            .register_trusted_context(&lease.token, "engine-thread", &source_message.id)
            .await
            .expect("trusted context should be registered");
        let with_context = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "get_auracoder_thread_message_count",
                    "arguments": {"thread_id": target_thread.id}
                }
            }))
            .send()
            .await
            .expect("context tools/call request should complete");
        let with_context_body: Value = with_context
            .json()
            .await
            .expect("context tools/call response JSON");
        assert_eq!(with_context_body["result"]["isError"], false);
        let text = with_context_body["result"]["content"][0]["text"]
            .as_str()
            .expect("context tools/call text content");
        let result: Value = serde_json::from_str(text).expect("context tool result JSON");
        assert_eq!(result["message_count"], 1);
        gateway.restart().await.expect("gateway should restart");
        let old_generation = client
            .post(format!(
                "http://{}/mcp",
                gateway.local_addr().await.expect("restarted address")
            ))
            .bearer_auth(&lease.token)
            .json(&json!({"jsonrpc": "2.0", "id": 11, "method": "ping"}))
            .send()
            .await
            .expect("old generation request should complete");
        assert_eq!(old_generation.status(), reqwest::StatusCode::UNAUTHORIZED);
        gateway.shutdown().await;
    }
