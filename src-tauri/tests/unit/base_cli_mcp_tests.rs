use super::{
    load_cli_thread_context, load_mcp_source_workspace, normalize_scheduled_runtime_fields,
    should_sync_cli_thread, BaseCliMcp, CliMcpRuntime,
};
use crate::{
    cli_tools::{CliLocationKind, McpInvocationContext},
    db::{self, Database},
    state::AppState,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 验证 Codex 会话无本地消息时需要执行按需同步。
#[test]
fn codex_empty_thread_requires_sync() {
    assert!(should_sync_cli_thread("codex", 0));
}

/// 验证已有本地消息的 Codex 会话不会执行按需同步。
#[test]
fn codex_thread_with_messages_skips_sync() {
    assert!(!should_sync_cli_thread("codex", 1));
}

/// 验证 OpenCode 和 Claude Code 空消息会话也执行按需同步。
#[test]
fn supported_cli_threads_require_sync_when_empty() {
    assert!(should_sync_cli_thread("opencode", 0));
    assert!(should_sync_cli_thread("claude", 0));
}

/// 验证未知引擎和已有消息的会话不会执行按需同步。
#[test]
fn unsupported_or_non_empty_thread_skips_sync() {
    assert!(!should_sync_cli_thread("unknown", 0));
    assert!(!should_sync_cli_thread("claude", 1));
}

/// 验证定时任务公共运行字段按 CLI 规则规范化，且服务档位保持原值。
#[test]
fn scheduled_runtime_fields_are_normalized() {
    let runtime = crate::models::ScheduledRuntimeConfig {
        engine_id: " codex ".to_string(),
        model_id: " gpt-test ".to_string(),
        reasoning_effort: Some(" XHIGH ".to_string()),
        service_tier: Some(" flex ".to_string()),
    };

    // 验证路由和模型去除空白、推理强度转小写，服务档位不由公共流程改写。
    let normalized = normalize_scheduled_runtime_fields("codex", runtime)
        .expect("scheduled runtime fields should normalize");
    assert_eq!(normalized.engine_id, "codex");
    assert_eq!(normalized.model_id, "gpt-test");
    assert_eq!(normalized.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(normalized.service_tier.as_deref(), Some(" flex "));

    let runtime_without_reasoning = crate::models::ScheduledRuntimeConfig {
        engine_id: "codex".to_string(),
        model_id: "gpt-test".to_string(),
        reasoning_effort: Some("   ".to_string()),
        service_tier: None,
    };
    let normalized_without_reasoning =
        normalize_scheduled_runtime_fields("codex", runtime_without_reasoning)
            .expect("blank reasoning effort should be omitted");
    assert_eq!(normalized_without_reasoning.reasoning_effort, None);
}

/// 验证定时任务公共运行配置拒绝错误 CLI 标识和空路由、模型字段。
#[test]
fn scheduled_runtime_fields_reject_wrong_cli_or_blank_model() {
    let wrong_cli = crate::models::ScheduledRuntimeConfig {
        engine_id: "opencode".to_string(),
        model_id: "model".to_string(),
        reasoning_effort: None,
        service_tier: None,
    };
    assert!(normalize_scheduled_runtime_fields("codex", wrong_cli).is_err());

    let blank_engine = crate::models::ScheduledRuntimeConfig {
        engine_id: "   ".to_string(),
        model_id: "model".to_string(),
        reasoning_effort: None,
        service_tier: None,
    };
    assert!(normalize_scheduled_runtime_fields("codex", blank_engine).is_err());

    let blank_model = crate::models::ScheduledRuntimeConfig {
        engine_id: "codex".to_string(),
        model_id: "  ".to_string(),
        reasoning_effort: None,
        service_tier: None,
    };
    assert!(normalize_scheduled_runtime_fields("codex", blank_model).is_err());
}

/// 验证 CLI 线程上下文只接受当前 CLI 所属线程和仍存在的工作区。
#[test]
fn thread_context_loader_checks_cli_ownership_and_workspace() {
    let temp_root = std::env::temp_dir()
        .join(format!("auracoder-cli-thread-context-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&temp_root)
        .expect("failed to create temporary CLI thread context root");
    let db_path = temp_root.join("thread.db");
    let database = Database::open(db_path)
        .expect("failed to initialize CLI thread context database");
    let workspace_root = temp_root.join("project");
    std::fs::create_dir_all(&workspace_root)
        .expect("failed to create CLI thread workspace root");
    let workspace = db::workspaces::upsert_workspace(
        &database,
        workspace_root
            .to_str()
            .expect("temporary workspace path should be valid UTF-8"),
    )
    .expect("failed to insert CLI thread workspace");
    let thread = db::threads::create_thread(&database, &workspace.id, "codex", "model", "title")
        .expect("failed to insert CLI thread");

    // 空标识、未知线程和错误 CLI 均拒绝，正确 CLI 返回归属线程及本机执行上下文。
    assert!(load_cli_thread_context(&database, "codex", "  ").is_err());
    assert!(load_cli_thread_context(&database, "codex", "missing-thread").is_err());
    assert!(load_cli_thread_context(&database, "claude", &thread.id).is_err());
    let (context, loaded_thread) = load_cli_thread_context(&database, "codex", &thread.id)
        .expect("thread context should match its CLI owner");
    assert_eq!(loaded_thread.id, thread.id);
    assert_eq!(context.workspace_id, workspace.id);
    assert_eq!(context.root_path, workspace.root_path);
    assert!(matches!(
        context.location_kind,
        crate::cli_tools::CliLocationKind::Local
    ));

    // 删除工作区后，相同线程不再能构造执行目标。
    db::workspaces::delete_workspace(&database, &workspace.id)
        .expect("failed to delete CLI thread workspace");
    assert!(load_cli_thread_context(&database, "codex", &thread.id).is_err());

    // 关闭数据库后清理仅由本测试创建的临时目录。
    drop(database);
    std::fs::remove_dir_all(temp_root)
        .expect("failed to remove temporary CLI thread context root");
}



/// 构造 MCP 会话工具测试需要的独立应用状态和临时数据库。
fn test_mcp_app_state() -> AppState {
    let root = std::env::temp_dir().join(format!("auracoder-base-cli-mcp-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("failed to create MCP test root");
    let db = Database::open(root.join("workspaces.db")).expect("failed to create MCP test db");
    AppState {
        db: db.clone(),
        config: Arc::new(crate::config::app_config::AppConfig::default()),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        engines: Arc::new(crate::engines::EngineManager::new()),
        git_watchers: Arc::new(crate::git::watcher::GitWatcherManager::default()),
        terminals: Arc::new(crate::terminal::TerminalManager::default()),
        notifications: Arc::new(crate::terminal_notifications::TerminalNotificationManager::default()),
        keep_awake: Arc::new(crate::power::KeepAwakeManager::new()),
        turns: Arc::new(crate::state::TurnManager::default()),
        file_tree_cache: Arc::new(crate::git::repo::FileTreeCache::new()),
        extension_catalog_refreshes: Arc::new(
            crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
        ),
        scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
        computer_control_service: Arc::new(crate::computer_control_service::ComputerControlService::default()),
        auracoder_thread_mcp_service: Arc::new(
            crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()),
        ),
        mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
        remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
        ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
    }
}

/// 验证可信助手消息按 CLI 所属线程映射来源工作区并拒绝非助手消息。
#[test]
fn load_mcp_source_workspace_reads_assistant_owner() {
    let state = test_mcp_app_state();
    let workspace_root = std::env::temp_dir().join(format!(
        "auracoder-mcp-source-workspace-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&workspace_root).expect("failed to create MCP source workspace");
    let workspace = db::workspaces::upsert_workspace(
        &state.db,
        workspace_root
            .to_str()
            .expect("temporary workspace path should be valid UTF-8"),
    )
    .expect("MCP source workspace");
    let mut source_records = Vec::new();

    // 为三种受支持 CLI 创建真实助手消息，供来源归属检查使用。
    for cli_id in ["codex", "opencode", "claude"] {
        let thread = db::threads::create_thread(
            &state.db,
            &workspace.id,
            cli_id,
            "model",
            "MCP source thread",
        )
        .expect("MCP source thread");
        let assistant_message = db::messages::insert_assistant_placeholder(
            &state.db,
            &thread.id,
            Some(cli_id),
            Some("model"),
            None,
        )
        .expect("MCP source assistant message");
        let user_message = db::messages::insert_user_message(
            &state.db,
            &thread.id,
            "user message",
            None,
            Some(cli_id),
            Some("model"),
            None,
        )
        .expect("MCP source user message");
        source_records.push((
            cli_id.to_string(),
            assistant_message.id,
            user_message.id,
        ));
    }

    // 只接受消息存在、角色为助手且会话引擎与当前 CLI 相同的来源。
    for (cli_id, assistant_message_id, user_message_id) in &source_records {
        assert_eq!(
            load_mcp_source_workspace(&state.db, cli_id, assistant_message_id)
                .expect("assistant source lookup"),
            Some(workspace.id.clone())
        );
        let other_cli = if cli_id == "codex" { "opencode" } else { "codex" };
        assert!(load_mcp_source_workspace(&state.db, other_cli, assistant_message_id)
            .expect("other CLI source lookup")
            .is_none());
        assert!(load_mcp_source_workspace(&state.db, cli_id, user_message_id)
            .expect("user message source lookup")
            .is_none());
        assert!(load_mcp_source_workspace(&state.db, "", assistant_message_id)
            .expect("empty CLI source lookup")
            .is_none());
        assert!(load_mcp_source_workspace(&state.db, cli_id, "")
            .expect("empty assistant ID lookup")
            .is_none());
        assert!(load_mcp_source_workspace(&state.db, cli_id, "missing-assistant-message")
            .expect("missing assistant lookup")
            .is_none());
    }

    // 删除数据库中的助手消息后，相同来源 ID 不再被接受。
    db::messages::delete_message(&state.db, &source_records[0].1)
        .expect("MCP source assistant message should be deleted");
    assert!(load_mcp_source_workspace(&state.db, "codex", &source_records[0].1)
        .expect("deleted assistant lookup")
        .is_none());
}

/// 验证会话工具从助手消息数据库来源查询，并维持计数和倒序分页契约。
#[tokio::test]
async fn thread_tools_use_database_source_and_preserve_contract() {
    let state = test_mcp_app_state();
    let workspace_root = std::env::temp_dir().join(format!(
        "auracoder-mcp-thread-source-workspace-{}",
        Uuid::new_v4()
    ));
    let other_workspace_root = std::env::temp_dir().join(format!(
        "auracoder-mcp-thread-other-workspace-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&workspace_root).expect("failed to create MCP source workspace");
    std::fs::create_dir_all(&other_workspace_root).expect("failed to create MCP other workspace");
    let workspace = db::workspaces::upsert_workspace(
        &state.db,
        workspace_root
            .to_str()
            .expect("temporary workspace path should be valid UTF-8"),
    )
    .expect("MCP source workspace");
    let other_workspace = db::workspaces::upsert_workspace(
        &state.db,
        other_workspace_root
            .to_str()
            .expect("temporary other workspace path should be valid UTF-8"),
    )
    .expect("MCP other workspace");
    let codex_source_thread =
        db::threads::create_thread(&state.db, &workspace.id, "codex", "model", "Codex source")
            .expect("Codex source thread");
    let codex_source_message = db::messages::insert_assistant_placeholder(
        &state.db,
        &codex_source_thread.id,
        Some("codex"),
        Some("model"),
        None,
    )
    .expect("Codex source assistant message");
    let opencode_source_thread =
        db::threads::create_thread(&state.db, &workspace.id, "opencode", "model", "OpenCode source")
            .expect("OpenCode source thread");
    let opencode_source_message = db::messages::insert_assistant_placeholder(
        &state.db,
        &opencode_source_thread.id,
        Some("opencode"),
        Some("model"),
        None,
    )
    .expect("OpenCode source assistant message");
    let claude_source_thread =
        db::threads::create_thread(&state.db, &workspace.id, "claude", "model", "Claude source")
            .expect("Claude source thread");
    let claude_source_message = db::messages::insert_assistant_placeholder(
        &state.db,
        &claude_source_thread.id,
        Some("claude"),
        Some("model"),
        None,
    )
    .expect("Claude source assistant message");

    let target_thread = db::threads::create_thread(
        &state.db,
        &workspace.id,
        "claude",
        "model",
        "Target thread",
    )
    .expect("MCP target thread");
    let early = db::messages::insert_user_message(
        &state.db,
        &target_thread.id,
        "早",
        None,
        Some("claude"),
        Some("model"),
        None,
    )
    .expect("early MCP target message");
    let middle = db::messages::insert_user_message(
        &state.db,
        &target_thread.id,
        "中",
        None,
        Some("claude"),
        Some("model"),
        None,
    )
    .expect("middle MCP target message");
    let late = db::messages::insert_user_message(
        &state.db,
        &target_thread.id,
        "晚",
        None,
        Some("claude"),
        Some("model"),
        None,
    )
    .expect("late MCP target message");
    let foreign_thread = db::threads::create_thread(
        &state.db,
        &other_workspace.id,
        "claude",
        "model",
        "Foreign target thread",
    )
    .expect("foreign MCP target thread");
    db::messages::insert_user_message(
        &state.db,
        &foreign_thread.id,
        "foreign message",
        None,
        Some("claude"),
        Some("model"),
        None,
    )
    .expect("foreign MCP target message");
    {
        let conn = state.db.connect().expect("MCP test database connection");
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params!["2026-08-22T00:00:01Z", early.id],
        )
        .expect("early MCP target timestamp");
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params!["2026-08-22T00:00:02Z", middle.id],
        )
        .expect("middle MCP target timestamp");
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params!["2026-08-22T00:00:03Z", late.id],
        )
        .expect("late MCP target timestamp");
    }

    // 同一工作区目标线程可由任一当前 CLI 读取，外部引擎线程 ID 不参与数据库映射。
    for (cli_id, assistant_message_id) in [
        ("codex", codex_source_message.id.as_str()),
        ("opencode", opencode_source_message.id.as_str()),
        ("claude", claude_source_message.id.as_str()),
    ] {
        for location in [CliLocationKind::Local, CliLocationKind::Ssh] {
            let mcp = BaseCliMcp::new(
                state.clone(),
                CliMcpRuntime {
                    cli_id: cli_id.to_string(),
                    location,
                },
            );
            let context = McpInvocationContext {
                engine_thread_id: format!("native-engine-thread-{cli_id}"),
                turn_id: assistant_message_id.to_string(),
            };
            let count_result = mcp
                .call_mcp_tool(
                    "get_auracoder_thread_message_count",
                    json!({"thread_id": target_thread.id}),
                    context.clone(),
                    "count-call".to_string(),
                    CancellationToken::new(),
                )
                .await;
            assert!(!count_result.is_error);
            let count_text = count_result.content[0]["text"]
                .as_str()
                .expect("count result text");
            let count_json: Value =
                serde_json::from_str(count_text).expect("count result should be JSON");
            assert_eq!(count_json["thread_id"], target_thread.id);
            assert_eq!(count_json["message_count"], 3);

            let first_page_result = mcp
                .call_mcp_tool(
                    "get_auracoder_thread_messages_page",
                    json!({"thread_id": target_thread.id, "page": 1, "page_size": 2}),
                    context.clone(),
                    "first-page-call".to_string(),
                    CancellationToken::new(),
                )
                .await;
            assert!(!first_page_result.is_error);
            let first_page_text = first_page_result.content[0]["text"]
                .as_str()
                .expect("first page result text");
            let first_page_json: Value =
                serde_json::from_str(first_page_text).expect("first page result should be JSON");
            assert_eq!(first_page_json["page"], 1);
            assert_eq!(first_page_json["page_size"], 2);
            assert_eq!(first_page_json["messages"][0]["content"], "晚");
            assert_eq!(first_page_json["messages"][1]["content"], "中");
            assert!(first_page_json["messages"][0].get("created_at").is_some());
            assert!(first_page_json["messages"][0].get("createdAt").is_none());

            let second_page_result = mcp
                .call_mcp_tool(
                    "get_auracoder_thread_messages_page",
                    json!({"thread_id": target_thread.id, "page": 2, "page_size": 2}),
                    context,
                    "second-page-call".to_string(),
                    CancellationToken::new(),
                )
                .await;
            assert!(!second_page_result.is_error);
            let second_page_text = second_page_result.content[0]["text"]
                .as_str()
                .expect("second page result text");
            let second_page_json: Value =
                serde_json::from_str(second_page_text).expect("second page result should be JSON");
            assert_eq!(second_page_json["messages"][0]["content"], "早");
            assert!(second_page_json["messages"][0].get("created_at").is_some());
            assert!(second_page_json["messages"][0].get("createdAt").is_none());
        }
    }

    let mcp = BaseCliMcp::new(
        state.clone(),
        CliMcpRuntime {
            cli_id: "codex".to_string(),
            location: CliLocationKind::Local,
        },
    );
    let valid_context = McpInvocationContext {
        engine_thread_id: "native-engine-thread-codex".to_string(),
        turn_id: codex_source_message.id.clone(),
    };
    let cross_project = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": foreign_thread.id}),
            valid_context.clone(),
            "cross-project-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(cross_project.is_error);
    assert_eq!(
        cross_project.content[0]["text"]
            .as_str()
            .expect("cross-project error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("tool_not_allowed")
    );

    let wrong_cli_mcp = BaseCliMcp::new(
        state.clone(),
        CliMcpRuntime {
            cli_id: "opencode".to_string(),
            location: CliLocationKind::Local,
        },
    );
    let wrong_cli = wrong_cli_mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": target_thread.id}),
            valid_context.clone(),
            "wrong-cli-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(wrong_cli.is_error);
    assert_eq!(
        wrong_cli.content[0]["text"]
            .as_str()
            .expect("wrong-CLI error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("tool_not_allowed")
    );

    let user_source = db::messages::insert_user_message(
        &state.db,
        &codex_source_thread.id,
        "not a trusted assistant message",
        None,
        Some("codex"),
        Some("model"),
        None,
    )
    .expect("user source message");
    let user_source_result = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": target_thread.id}),
            McpInvocationContext {
                engine_thread_id: "native-engine-thread-codex".to_string(),
                turn_id: user_source.id,
            },
            "user-source-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(user_source_result.is_error);
    assert_eq!(
        user_source_result.content[0]["text"]
            .as_str()
            .expect("user-source error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("tool_not_allowed")
    );

    let missing_assistant = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": target_thread.id}),
            McpInvocationContext {
                engine_thread_id: "native-engine-thread-codex".to_string(),
                turn_id: "missing-assistant-message".to_string(),
            },
            "missing-assistant-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(missing_assistant.is_error);
    assert_eq!(
        missing_assistant.content[0]["text"]
            .as_str()
            .expect("missing-assistant error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("tool_not_allowed")
    );

    let missing_target = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": "missing-thread"}),
            valid_context.clone(),
            "missing-target-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(missing_target.is_error);
    assert_eq!(
        missing_target.content[0]["text"]
            .as_str()
            .expect("missing-target error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("tool_not_found")
    );

    let null_arguments = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            Value::Null,
            valid_context.clone(),
            "null-arguments-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(null_arguments.is_error);
    assert_eq!(
        null_arguments.content[0]["text"]
            .as_str()
            .expect("null-arguments error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("invalid_request")
    );

    let missing_thread_id = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({}),
            valid_context.clone(),
            "missing-thread-id-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(missing_thread_id.is_error);
    assert_eq!(
        missing_thread_id.content[0]["text"]
            .as_str()
            .expect("missing thread ID error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("invalid_request")
    );

    let zero_page = mcp
        .call_mcp_tool(
            "get_auracoder_thread_messages_page",
            json!({"thread_id": target_thread.id, "page": 0, "page_size": 2}),
            valid_context.clone(),
            "zero-page-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(zero_page.is_error);
    assert_eq!(
        zero_page.content[0]["text"]
            .as_str()
            .expect("zero-page error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("invalid_request")
    );

    let zero_page_size = mcp
        .call_mcp_tool(
            "get_auracoder_thread_messages_page",
            json!({"thread_id": target_thread.id, "page": 1, "page_size": 0}),
            valid_context.clone(),
            "zero-page-size-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(zero_page_size.is_error);
    assert_eq!(
        zero_page_size.content[0]["text"]
            .as_str()
            .expect("zero-page-size error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("invalid_request")
    );

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            Value::Null,
            valid_context,
            "cancelled-call".to_string(),
            cancellation,
        )
        .await;
    assert!(cancelled.is_error);
    assert_eq!(
        cancelled.content[0]["text"]
            .as_str()
            .expect("cancelled error text")
            .split_once(':')
            .map(|(code, _)| code),
        Some("request_cancelled")
    );
}

/// 验证来源数据库读取异常返回稳定错误码和中文失败信息。
#[tokio::test]
async fn thread_tools_return_structured_database_failure() {
    let state = test_mcp_app_state();
    let workspace_root = std::env::temp_dir().join(format!(
        "auracoder-mcp-source-error-workspace-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&workspace_root).expect("failed to create MCP source workspace");
    let workspace = db::workspaces::upsert_workspace(
        &state.db,
        workspace_root
            .to_str()
            .expect("temporary workspace path should be valid UTF-8"),
    )
    .expect("MCP error workspace");
    let source_thread = db::threads::create_thread(
        &state.db,
        &workspace.id,
        "codex",
        "model",
        "MCP error source",
    )
    .expect("MCP error source thread");
    let assistant_message = db::messages::insert_assistant_placeholder(
        &state.db,
        &source_thread.id,
        Some("codex"),
        Some("model"),
        None,
    )
    .expect("MCP error source assistant message");
    {
        let conn = state.db.connect().expect("MCP error test database connection");
        conn.execute("DROP TABLE messages", [])
            .expect("test message table should be dropped");
    }
    let mcp = BaseCliMcp::new(
        state,
        CliMcpRuntime {
            cli_id: "codex".to_string(),
            location: CliLocationKind::Local,
        },
    );
    let result = mcp
        .call_mcp_tool(
            "get_auracoder_thread_message_count",
            json!({"thread_id": source_thread.id}),
            McpInvocationContext {
                engine_thread_id: "native-engine-thread-codex".to_string(),
                turn_id: assistant_message.id,
            },
            "database-failure-call".to_string(),
            CancellationToken::new(),
        )
        .await;
    assert!(result.is_error);
    let error_text = result.content[0]["text"]
        .as_str()
        .expect("database failure error text");
    assert_eq!(
        error_text.split_once(':').map(|(code, _)| code),
        Some("tool_execution_failed")
    );
    assert!(error_text.contains("来源"));
}
