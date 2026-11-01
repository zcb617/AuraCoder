use super::{
    load_cli_thread_context, normalize_scheduled_runtime_fields, should_sync_cli_thread,
};
use crate::db::{self, Database};
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
