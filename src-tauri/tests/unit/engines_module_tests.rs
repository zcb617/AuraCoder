use super::*;

#[test]
fn codex_capabilities_exclude_unsupported_on_failure_policy() {
    let capabilities = capabilities_for_engine("codex");

    assert_eq!(
        capabilities.permission_modes,
        &["untrusted", "on-request", "never"]
    );
}
#[test]
fn claude_capabilities_expose_supported_contract() {
    let capabilities = capabilities_for_engine("claude");

    assert_eq!(
        capabilities.permission_modes,
        &["restricted", "standard", "trusted"]
    );
    assert_eq!(
        capabilities.sandbox_modes,
        &["read-only", "workspace-write", "danger-full-access"]
    );
    assert_eq!(
        capabilities.approval_decisions,
        &["accept", "decline", "accept_for_session"]
    );
}

#[test]
fn opencode_capabilities_do_not_inherit_codex_sandbox_modes() {
    let capabilities = capabilities_for_engine("opencode");

    assert_eq!(capabilities.permission_modes, &["ask", "allow", "deny"]);
    assert_eq!(capabilities.sandbox_modes, &[] as &[&str]);
    assert_eq!(
        capabilities.approval_decisions,
        &["accept", "decline", "cancel", "accept_for_session"]
    );
    assert!(validate_engine_sandbox_mode("opencode", Some("danger-full-access")).is_err());
    assert!(validate_engine_sandbox_mode("opencode", Some("workspace-write")).is_err());
}

#[test]
fn validate_engine_sandbox_mode_accepts_claude_full_access() {
    assert!(validate_engine_sandbox_mode("claude", Some("danger-full-access")).is_ok());
    assert!(validate_engine_sandbox_mode("claude", Some("workspace-write")).is_ok());
}

#[test]
fn provider_usage_keeps_generic_and_fable_weekly_windows_separate() {
    let provider = map_provider_usage(
        "claude",
        "Claude",
        Ok(UsageLimitsSnapshot {
            five_hour_percent: Some(12),
            weekly_percent: Some(46),
            fable_weekly_percent: Some(76),
            ..UsageLimitsSnapshot::default()
        }),
    );

    assert!(provider.available);
    assert_eq!(provider.windows.len(), 3);
    assert_eq!(provider.windows[1].kind, "weekly");
    assert_eq!(provider.windows[1].used_percent, 46);
    assert_eq!(provider.windows[2].kind, "fable_weekly");
    assert_eq!(provider.windows[2].used_percent, 76);
}

#[test]
fn normalize_claude_approval_response_rejects_missing_and_extra_fields() {
    assert!(normalize_approval_response_for_engine("claude", json!({})).is_err());
    assert!(normalize_approval_response_for_engine(
        "claude",
        json!({ "decision": "accept", "extra": true })
    )
    .is_err());
    assert!(normalize_approval_response_for_engine(
        "claude",
        json!({ "answers": {}, "decision": "accept" })
    )
    .is_err());
}

#[test]
fn normalize_claude_approval_response_accepts_aliases() {
    assert_eq!(
        normalize_approval_response_for_engine("claude", json!({ "decision": "deny" }))
            .unwrap(),
        json!({ "decision": "decline" })
    );
    assert_eq!(
        normalize_approval_response_for_engine(
            "claude",
            json!({ "decision": "acceptForSession" })
        )
        .unwrap(),
        json!({ "decision": "accept_for_session" })
    );
    assert_eq!(
        normalize_approval_response_for_engine("claude", json!({ "action": "decline" }))
            .unwrap(),
        json!({ "decision": "decline" })
    );
    assert_eq!(
        normalize_approval_response_for_engine("claude", json!({ "action": "cancel" }))
            .unwrap(),
        json!({ "decision": "decline" })
    );
}

#[test]
fn normalize_claude_approval_response_accepts_questionnaire_answers() {
    assert_eq!(
        normalize_approval_response_for_engine(
            "claude",
            json!({
                "answers": {
                    "question-1": { "answers": ["Use pnpm"] }
                }
            })
        )
        .unwrap(),
        json!({
            "answers": {
                "question-1": { "answers": ["Use pnpm"] }
            }
        })
    );
}

#[test]
fn normalize_opencode_approval_response_accepts_decisions_and_questions() {
    assert_eq!(
        normalize_approval_response_for_engine("opencode", json!({ "decision": "accept" }))
            .unwrap(),
        json!({ "decision": "accept" })
    );
    assert_eq!(
        normalize_approval_response_for_engine(
            "opencode",
            json!({ "action": "acceptForSession" })
        )
        .unwrap(),
        json!({ "decision": "accept_for_session" })
    );
    assert_eq!(
        normalize_approval_response_for_engine(
            "opencode",
            json!({ "answers": { "question-0-name": { "answers": ["pnpm"] } } })
        )
        .unwrap(),
        json!({ "answers": { "question-0-name": { "answers": ["pnpm"] } } })
    );
    assert!(normalize_approval_response_for_engine(
        "opencode",
        json!({ "decision": "accept", "answers": {} })
    )
    .is_err());
}

#[test]
fn approval_response_route_for_codex_requires_hidden_transport_fields() {
    assert_eq!(
        approval_response_route_for_engine(
            "codex",
            &json!({
                "_serverMethod": "item/fileChange/requestApproval"
            })
        ),
        None
    );
    assert_eq!(
        approval_response_route_for_engine(
            "claude",
            &json!({
                "_serverMethod": "item/fileChange/requestApproval",
                "_rawRequestId": 42
            })
        ),
        None
    );
}
