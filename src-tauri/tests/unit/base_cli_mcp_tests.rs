use super::{normalize_scheduled_runtime_fields, should_sync_cli_thread};

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
