use super::*;

// 旧声明在内联 mod tests 内部，解析基址 src/engines/codex/tests/ 不存在，必然 ENOENT；
// 已按 factory.rs 同例移到文件顶层，见上方模块声明。
// #[path = "../../../../tests/unit/codex_mcp_elicitation_tests.rs"]
// mod codex_mcp_elicitation_tests;

use crate::engines::ActionResult;
use serde_json::{json, Value};

#[test]
fn extract_latest_thread_context_usage_uses_latest_reliable_turn() {
    let response = json!({
        "thread": {
            "turns": [
                {
                    "id": "turn-old",
                    "tokenUsage": {
                        "last": { "totalTokens": 32000 },
                        "modelContextWindow": 128000
                    }
                },
                {
                    "id": "turn-reliable",
                    "tokenUsage": {
                        "last": { "totalTokens": 64000 },
                        "modelContextWindow": 128000
                    }
                },
                {
                    "id": "turn-latest-without-window",
                    "tokenUsage": {
                        "last": { "totalTokens": 96000 }
                    }
                }
            ]
        }
    });

    assert_eq!(
        extract_latest_thread_context_usage(&response),
        Some((64000, 128000))
    );
}

#[test]
fn codex_thread_subscription_allows_primary_and_registered_subagent_only() {
    let mut subscription = CodexThreadSubscription::new("parent".to_string());
    assert!(subscription.allows(&json!({ "threadId": "parent" })));
    assert!(subscription.allows(&json!({ "event": "without-thread-id" })));
    assert!(!subscription.allows(&json!({ "threadId": "unrelated" })));

    assert!(subscription.register_subagent_activity(&json!({
        "threadId": "parent",
        "item": {
            "type": "subAgentActivity",
            "agentThreadId": "child"
        }
    })));
    assert!(subscription.allows(&json!({ "threadId": "child" })));
    assert!(!subscription.allows(&json!({ "threadId": "unrelated" })));
}

#[test]
fn codex_thread_subscription_accepts_only_same_session_subagent_activity() {
    let mut subscription_a = CodexThreadSubscription::new("primary-a".to_string());
    let mut subscription_b = CodexThreadSubscription::new("primary-b".to_string());

    let child_b_activity = json!({
        "threadId": "primary-b",
        "item": {
            "type": "subAgentActivity",
            "agentThreadId": "child-b"
        }
    });
    assert!(!subscription_a.register_subagent_activity(&child_b_activity));
    assert!(!subscription_a.allows(&json!({ "threadId": "child-b" })));
    assert!(subscription_b.register_subagent_activity(&child_b_activity));
    assert!(subscription_b.allows(&json!({ "threadId": "child-b" })));

    let missing_source_activity = json!({
        "item": {
            "type": "subAgentActivity",
            "agentThreadId": "child-without-source"
        }
    });
    assert!(!subscription_a.register_subagent_activity(&missing_source_activity));
    assert!(!subscription_a.allows(&json!({
        "threadId": "child-without-source"
    })));

    let child_a_activity = json!({
        "threadId": "primary-a",
        "item": {
            "type": "subAgentActivity",
            "agentThreadId": "child-a"
        }
    });
    assert!(subscription_a.register_subagent_activity(&child_a_activity));
    assert!(subscription_a.allows(&json!({ "threadId": "child-a" })));

    let grandchild_a_activity = json!({
        "threadId": "child-a",
        "item": {
            "type": "subAgentActivity",
            "agentThreadId": "grandchild-a"
        }
    });
    assert!(subscription_a.register_subagent_activity(&grandchild_a_activity));
    assert!(subscription_a.allows(&json!({ "threadId": "grandchild-a" })));
}

#[tokio::test]
async fn remote_text_attachment_uses_remote_path_without_local_file_read() {
    let attachment = TurnAttachment {
        file_name: "说明.md".to_string(),
        file_path: "/home/tester/.cache/auracoder/attachments/workspace/thread/file.md"
            .to_string(),
        preview_file_path: None,
        size_bytes: 128,
        mime_type: Some("text/markdown".to_string()),
        browser_annotation: None,
        is_remote: true,
        remote_text_content: Some("AuraCoder SSH 远端附件验证码：ATTACHMENT-PLAN4".to_string()),
    };
    validate_turn_attachments(std::slice::from_ref(&attachment))
        .await
        .expect("remote attachment validation");
    let input = TurnInput {
        message: "读取附件".to_string(),
        attachments: vec![attachment],
        plan_mode: false,
        input_items: Vec::new(),
        permissions: None,
    };

    let items = build_turn_input_items(&input, false)
        .await
        .expect("remote input items");

    assert!(items.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("text")
            && item
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| {
                    text.contains(
                        "/home/tester/.cache/auracoder/attachments/workspace/thread/file.md",
                    ) && text.contains("ATTACHMENT-PLAN4")
                })
    }));
}

#[tokio::test]
async fn remote_websocket_transport_does_not_depend_on_local_codex_availability() {
    let engine = CodexEngine::new_remote_websocket("ws://127.0.0.1:43100".to_string());

    assert!(engine.is_available().await);
}

fn test_sandbox_policy(sandbox_mode: Option<&str>, allow_network: bool) -> SandboxPolicy {
    SandboxPolicy {
        writable_roots: vec!["/tmp/workspace".to_string()],
        allow_network,
        auto_approve_mcp_elicitations: false,
        approval_policy: None,
        permission_profile: None,
        approvals_reviewer: None,
        reasoning_effort: None,
        sandbox_mode: sandbox_mode.map(ToOwned::to_owned),
        service_tier: None,
        personality: None,
        output_schema: None,
        opencode_agent: None,
    }
}

#[test]
fn sandbox_policy_omits_removed_restricted_read_fields() {
    let workspace_write =
        sandbox_policy_to_json(&test_sandbox_policy(Some("workspace-write"), false), false);
    assert_eq!(
        workspace_write,
        json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/workspace"],
            "networkAccess": false,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        })
    );

    let read_only =
        sandbox_policy_to_json(&test_sandbox_policy(Some("read-only"), true), false);
    assert_eq!(
        read_only,
        json!({
            "type": "readOnly",
            "networkAccess": true,
        })
    );
}

#[test]
fn normalize_modern_accept_with_execpolicy_from_top_level() {
    let response = json!({
        "acceptWithExecpolicyAmendment": {
            "execpolicy_amendment": ["npm", "test"]
        }
    });

    let normalized =
        normalize_approval_response(Some("item/commandExecution/requestApproval"), response);

    assert_eq!(
        normalized,
        json!({
            "decision": {
                "acceptWithExecpolicyAmendment": {
                    "execpolicy_amendment": ["npm", "test"]
                }
            }
        })
    );
}

#[test]
fn normalize_modern_accept_for_session_to_camel_case() {
    let response = json!({ "decision": "accept_for_session" });
    let normalized =
        normalize_approval_response(Some("item/fileChange/requestApproval"), response);

    assert_eq!(normalized, json!({ "decision": "acceptForSession" }));
}

#[test]
fn normalize_modern_network_policy_amendment_from_top_level() {
    let response = json!({
        "applyNetworkPolicyAmendment": {
            "network_policy_amendment": {
                "host": "registry.npmjs.org",
                "action": "allow"
            }
        }
    });

    let normalized =
        normalize_approval_response(Some("item/commandExecution/requestApproval"), response);

    assert_eq!(
        normalized,
        json!({
            "decision": {
                "applyNetworkPolicyAmendment": {
                    "network_policy_amendment": {
                        "host": "registry.npmjs.org",
                        "action": "allow"
                    }
                }
            }
        })
    );
}

#[test]
fn normalize_legacy_accept_with_execpolicy_to_legacy_shape() {
    let response = json!({
        "acceptWithExecpolicyAmendment": {
            "execpolicy_amendment": ["pnpm", "install"]
        }
    });

    let normalized = normalize_approval_response(Some("execCommandApproval"), response);

    assert_eq!(
        normalized,
        json!({
            "decision": {
                "approved_execpolicy_amendment": {
                    "proposed_execpolicy_amendment": ["pnpm", "install"]
                }
            }
        })
    );
}

#[test]
fn normalize_legacy_network_policy_to_legacy_shape() {
    let response = json!({
        "network_policy_amendment": {
            "host": "registry.npmjs.org",
            "action": "allow"
        }
    });

    let normalized = normalize_approval_response(Some("execCommandApproval"), response);

    assert_eq!(
        normalized,
        json!({
            "decision": {
                "network_policy_amendment": {
                    "network_policy_amendment": {
                        "host": "registry.npmjs.org",
                        "action": "allow"
                    }
                }
            }
        })
    );
}

#[test]
fn normalize_dynamic_tool_call_response_is_unchanged() {
    let response = json!({
        "success": true,
        "contentItems": []
    });

    let normalized = normalize_approval_response(Some("item/tool/call"), response.clone());

    assert_eq!(normalized, response);
}

#[test]
fn thread_resume_params_include_requested_runtime() {
    let params = build_thread_resume_params(
        "thread-123",
        "gpt-5-codex",
        "/tmp/workspace",
        &json!("on-request"),
        "workspace-write",
        None,
        None,
        Some("fast"),
        Some("friendly"),
    );

    assert_eq!(
        params,
        json!({
            "threadId": "thread-123",
            "model": "gpt-5-codex",
            "cwd": "/tmp/workspace",
            "approvalPolicy": "on-request",
            "sandbox": "workspace-write",
            "serviceTier": "fast",
            "personality": "friendly",
            "persistExtendedHistory": false,
        })
    );
}

#[test]
fn thread_start_params_register_auracoder_computer_control_namespace() {
    let dynamic_tools = json!([
        {
            "type": "namespace",
            "name": "auracoder_computer_control",
            "tools": []
        }
    ]);
    let params = build_thread_start_params(
        "gpt-5-codex",
        "/tmp/workspace",
        &json!("on-request"),
        "workspace-write",
        &SandboxPolicy {
            writable_roots: Vec::new(),
            allow_network: false,
            auto_approve_mcp_elicitations: false,
            approval_policy: None,
            permission_profile: None,
            approvals_reviewer: None,
            reasoning_effort: None,
            sandbox_mode: None,
            service_tier: None,
            personality: None,
            output_schema: None,
            opencode_agent: None,
        },
        &dynamic_tools,
    );
    assert_eq!(
        params["dynamicTools"][0]["name"],
        "auracoder_computer_control"
    );
}

#[tokio::test]
async fn build_turn_start_params_uses_native_plan_mode_for_codex_when_available() {
    let runtime = ThreadRuntime {
        cwd: "/tmp/workspace".to_string(),
        model_id: "gpt-5.4".to_string(),
        approval_policy: json!("on-request"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/workspace"],
            "networkAccess": false,
        }),
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("fast".to_string()),
        personality: Some("friendly".to_string()),
        output_schema: Some(json!({"type":"object"})),
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: false,
    };
    let input = TurnInput {
        message: "Inspect the repo first".to_string(),
        attachments: Vec::new(),
        plan_mode: true,
        input_items: vec![TurnInputItem::Text {
            text: "Inspect the repo first".to_string(),
        }],
        permissions: None,
    };

    let params = build_turn_start_params(
        "thread-123",
        Some(&runtime),
        &input,
        PlanModeActivation::NativeCollaboration,
    )
    .await
    .expect("turn/start params");

    assert_eq!(
        params.get("collaborationMode"),
        Some(&json!({
            "mode": "plan",
            "settings": {
                "model": "gpt-5.4",
                "reasoning_effort": "medium",
            }
        }))
    );
    assert_eq!(params.get("summary"), Some(&json!("detailed")));

    let payload = params
        .get("input")
        .and_then(Value::as_array)
        .expect("input array");
    assert_eq!(payload.len(), 1);
    assert_eq!(payload[0].get("type").and_then(Value::as_str), Some("text"));
    let text = payload[0]
        .get("text")
        .and_then(Value::as_str)
        .expect("text payload");
    assert_eq!(text, "Inspect the repo first");
}

#[tokio::test]
async fn build_turn_start_params_resets_native_plan_mode_on_non_plan_turns() {
    let handoff_message = "Implement the plan.";
    let runtime = ThreadRuntime {
        cwd: "/tmp/workspace".to_string(),
        model_id: "gpt-5.4".to_string(),
        approval_policy: json!("on-request"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/workspace"],
            "networkAccess": false,
        }),
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("fast".to_string()),
        personality: Some("friendly".to_string()),
        output_schema: Some(json!({"type":"object"})),
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: true,
    };
    let input = TurnInput {
        message: handoff_message.to_string(),
        attachments: Vec::new(),
        plan_mode: false,
        input_items: vec![TurnInputItem::Text {
            text: handoff_message.to_string(),
        }],
        permissions: None,
    };

    let params = build_turn_start_params(
        "thread-123",
        Some(&runtime),
        &input,
        PlanModeActivation::NativeCollaboration,
    )
    .await
    .expect("turn/start params");

    assert_eq!(
        params.get("collaborationMode"),
        Some(&json!({
            "mode": "default",
            "settings": {
                "model": "gpt-5.4",
                "reasoning_effort": "medium",
            }
        }))
    );
    assert_eq!(params.get("summary"), Some(&Value::Null));

    let payload = params
        .get("input")
        .and_then(Value::as_array)
        .expect("input array");
    assert_eq!(payload.len(), 1);
    assert_eq!(
        payload[0].get("text").and_then(Value::as_str),
        Some(handoff_message)
    );
}

#[tokio::test]
async fn build_turn_start_params_uses_prompt_fallback_when_plan_mode_is_not_native() {
    let runtime = ThreadRuntime {
        cwd: "/tmp/workspace".to_string(),
        model_id: "gpt-5.4".to_string(),
        approval_policy: json!("on-request"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/workspace"],
            "networkAccess": false,
        }),
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("fast".to_string()),
        personality: Some("friendly".to_string()),
        output_schema: Some(json!({"type":"object"})),
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: false,
    };
    let input = TurnInput {
        message: "Inspect the repo first".to_string(),
        attachments: Vec::new(),
        plan_mode: true,
        input_items: vec![TurnInputItem::Text {
            text: "Inspect the repo first".to_string(),
        }],
        permissions: None,
    };

    let params = build_turn_start_params(
        "thread-123",
        Some(&runtime),
        &input,
        PlanModeActivation::PromptPrefix,
    )
    .await
    .expect("turn/start params");

    assert_eq!(params.get("collaborationMode"), None);
    assert_eq!(params.get("summary"), None);

    let payload = params
        .get("input")
        .and_then(Value::as_array)
        .expect("input array");
    let text = payload[0]
        .get("text")
        .and_then(Value::as_str)
        .expect("text payload");
    assert!(text.starts_with(PLAN_MODE_PROMPT_PREFIX));
    assert!(text.contains("- [pending] Step"));
    assert!(text.contains("Inspect the repo first"));
}

#[tokio::test]
async fn build_turn_start_params_keeps_non_plan_text_unchanged() {
    let input = TurnInput {
        message: "Inspect the repo first".to_string(),
        attachments: Vec::new(),
        plan_mode: false,
        input_items: vec![TurnInputItem::Text {
            text: "Inspect the repo first".to_string(),
        }],
        permissions: None,
    };

    let params =
        build_turn_start_params("thread-123", None, &input, PlanModeActivation::Disabled)
            .await
            .expect("turn/start params");

    assert_eq!(params.get("collaborationMode"), None);
    assert_eq!(params.get("summary"), None);

    let payload = params
        .get("input")
        .and_then(Value::as_array)
        .expect("input array");
    assert_eq!(
        payload[0].get("text").and_then(Value::as_str),
        Some("Inspect the repo first")
    );
}

#[test]
fn plan_mode_activation_prefers_native_when_plan_is_advertised() {
    let diagnostics = CodexProtocolDiagnosticsDto {
        method_availability: vec![CodexMethodAvailabilityDto {
            method: "collaborationMode/list".to_string(),
            status: "available".to_string(),
            detail: None,
        }],
        collaboration_modes: vec!["default".to_string(), "plan".to_string()],
        experimental_features: Vec::new(),
        apps: Vec::new(),
        skills: Vec::new(),
        plugin_marketplaces: Vec::new(),
        mcp_servers: Vec::new(),
        account: None,
        config: None,
        last_config_warning: None,
        last_account_login: None,
        last_mcp_oauth: None,
        last_thread_realtime: None,
        last_windows_sandbox_setup: None,
        last_windows_world_writable_warning: None,
        fetched_at: None,
        stale: false,
    };

    assert_eq!(
        plan_mode_activation_from_diagnostics(Some(&diagnostics)),
        Some(PlanModeActivation::NativeCollaboration)
    );
}

#[test]
fn plan_mode_activation_falls_back_when_plan_is_not_advertised() {
    let diagnostics = CodexProtocolDiagnosticsDto {
        method_availability: vec![CodexMethodAvailabilityDto {
            method: "collaborationMode/list".to_string(),
            status: "unsupported".to_string(),
            detail: Some("not implemented".to_string()),
        }],
        collaboration_modes: Vec::new(),
        experimental_features: Vec::new(),
        apps: Vec::new(),
        skills: Vec::new(),
        plugin_marketplaces: Vec::new(),
        mcp_servers: Vec::new(),
        account: None,
        config: None,
        last_config_warning: None,
        last_account_login: None,
        last_mcp_oauth: None,
        last_thread_realtime: None,
        last_windows_sandbox_setup: None,
        last_windows_world_writable_warning: None,
        fetched_at: None,
        stale: false,
    };

    assert_eq!(
        plan_mode_activation_from_diagnostics(Some(&diagnostics)),
        Some(PlanModeActivation::PromptPrefix)
    );
}

#[test]
fn normalize_modern_snake_case_method_alias() {
    let response = json!({ "decision": "accept_for_session" });
    let normalized =
        normalize_approval_response(Some("item/command_execution/request_approval"), response);

    assert_eq!(normalized, json!({ "decision": "acceptForSession" }));
}

#[test]
fn transport_failure_message_detects_terminal_transport_events() {
    assert_eq!(
        transport_failure_message("transport/eof", &json!({})).as_deref(),
        Some("codex app-server closed the connection unexpectedly")
    );
    assert_eq!(
        transport_failure_message(
            "transport/readerror",
            &json!({
                "error": "broken pipe"
            })
        )
        .as_deref(),
        Some("codex app-server connection failed: broken pipe")
    );
    assert_eq!(
        transport_failure_message(
            "transport/parse_error",
            &json!({
                "error": "expected value at line 1 column 1"
            })
        )
        .as_deref(),
        Some("codex app-server sent an unreadable protocol message: expected value at line 1 column 1")
    );
}

#[test]
fn codex_transport_reset_category_preserves_the_trigger_source() {
    assert_eq!(
        codex_transport_reset_category(
            "codex transport lagged while waiting for turn events; skipped 7 messages"
        ),
        "event_queue_lagged"
    );
    assert_eq!(
        codex_transport_reset_category("codex app-server closed the connection unexpectedly"),
        "transport_eof"
    );
    assert_eq!(
        codex_transport_reset_category("codex app-server connection failed: broken pipe"),
        "transport_read_error"
    );
    assert_eq!(
        codex_transport_reset_category("codex initialize failed (attempt 1/3)"),
        "initialize_failure"
    );
}

#[test]
fn rebind_expected_turn_id_replaces_prior_turn() {
    let mut expected_turn_id = Some("turn-plan".to_string());

    rebind_expected_turn_id(
        &mut expected_turn_id,
        "turn-execute",
        "thread-123",
        "turn/started notification",
    );

    assert_eq!(expected_turn_id.as_deref(), Some("turn-execute"));
}

#[test]
fn extract_reconciled_turn_completion_prefers_expected_turn() {
    let reconciled = extract_reconciled_turn_completion(
        &json!({
            "thread": {
                "turns": [
                    { "id": "turn-old", "status": "completed" },
                    {
                        "id": "turn-active",
                        "status": "failed",
                        "error": { "message": "permission denied" }
                    }
                ]
            }
        }),
        Some("turn-active"),
    )
    .expect("expected terminal turn");

    assert_eq!(
        reconciled,
        ReconciledTurnCompletion {
            status: TurnCompletionStatus::Failed,
            error_message: Some("permission denied".to_string()),
        }
    );
}

#[test]
fn extract_reconciled_turn_completion_requires_matching_turn_id() {
    assert_eq!(
        extract_reconciled_turn_completion(
            &json!({
                "thread": {
                    "turns": [
                        { "id": "turn-old", "status": "completed" },
                        { "id": "turn-latest", "status": "interrupted" }
                    ]
                }
            }),
            Some("turn-missing"),
        ),
        None
    );
}

#[test]
fn extract_reconciled_turn_completion_requires_expected_turn_id() {
    assert_eq!(
        extract_reconciled_turn_completion(
            &json!({
                "thread": {
                    "turns": [
                        { "id": "turn-latest", "status": "completed" }
                    ]
                }
            }),
            None,
        ),
        None
    );
}

#[test]
fn build_reconciled_turn_completion_events_marks_lost_completed_turn_failed() {
    let events = build_reconciled_turn_completion_events(
        ReconciledTurnCompletion {
            status: TurnCompletionStatus::Completed,
            error_message: None,
        },
        TurnCompletionRecoveryMode::StreamLost,
    );

    assert_eq!(events.len(), 2);
    match &events[0] {
        EngineEvent::Error {
            message,
            recoverable,
        } => {
            assert!(message.contains("transcript may be incomplete"));
            assert!(*recoverable);
        }
        other => panic!("expected warning error event, got {other:?}"),
    }
    match &events[1] {
        EngineEvent::TurnCompleted {
            status,
            token_usage,
        } => {
            assert_eq!(*status, TurnCompletionStatus::Failed);
            assert!(token_usage.is_none());
        }
        other => panic!("expected turn completed event, got {other:?}"),
    }
}

#[test]
fn build_reconciled_turn_completion_events_keeps_timeout_completion_status() {
    let events = build_reconciled_turn_completion_events(
        ReconciledTurnCompletion {
            status: TurnCompletionStatus::Completed,
            error_message: Some("remote failure".to_string()),
        },
        TurnCompletionRecoveryMode::CompletionTimeout,
    );

    assert_eq!(events.len(), 2);
    match &events[0] {
        EngineEvent::Error {
            message,
            recoverable,
        } => {
            assert_eq!(message, "remote failure");
            assert!(*recoverable);
        }
        other => panic!("expected remote error event, got {other:?}"),
    }
    match &events[1] {
        EngineEvent::TurnCompleted {
            status,
            token_usage,
        } => {
            assert_eq!(*status, TurnCompletionStatus::Completed);
            assert!(token_usage.is_none());
        }
        other => panic!("expected turn completed event, got {other:?}"),
    }
}

#[test]
fn extract_reconciled_turn_completion_ignores_in_progress_turns() {
    assert_eq!(
        extract_reconciled_turn_completion(
            &json!({
                "thread": {
                    "turns": [
                        { "id": "turn-active", "status": "inProgress" }
                    ]
                }
            }),
            Some("turn-active"),
        ),
        None
    );
}

#[test]
fn resolve_approval_response_target_prefers_pending_request_metadata() {
    let pending = PendingApproval {
        raw_request_id: json!(42),
        method: "item/fileChange/requestApproval".to_string(),
    };
    let persisted = ApprovalRequestRoute {
        server_method: "item/commandExecution/requestApproval".to_string(),
        raw_request_id: json!("req-2"),
    };

    let resolved = resolve_approval_response_target(Some(&pending), Some(&persisted))
        .expect("expected live pending approval target");

    assert_eq!(resolved.0, &json!(42));
    assert_eq!(resolved.1, "item/fileChange/requestApproval");
}

#[test]
fn imported_message_status_maps_in_progress_turns_to_streaming() {
    assert_eq!(
        imported_message_status_for_turn(&json!({ "status": "inProgress" })),
        "streaming"
    );
    assert_eq!(
        imported_message_status_for_turn(&json!({ "status": "failed" })),
        "error"
    );
}

#[test]
fn resolve_approval_response_target_rejects_persisted_route_after_transport_reset() {
    let persisted = ApprovalRequestRoute {
        server_method: "item/commandExecution/requestApproval".to_string(),
        raw_request_id: json!("req-2"),
    };

    assert_eq!(
        resolve_approval_response_target(None, Some(&persisted)),
        Err(ApprovalResponseTargetError::RuntimeReset)
    );
}

#[test]
fn resolve_approval_response_target_rejects_missing_request_metadata() {
    assert_eq!(
        resolve_approval_response_target(None, None),
        Err(ApprovalResponseTargetError::MissingRequestMetadata)
    );
}

#[test]
fn parse_optional_timeout_seconds_treats_zero_and_invalid_as_disabled() {
    assert_eq!(parse_optional_timeout_seconds(None), None);
    assert_eq!(parse_optional_timeout_seconds(Some("")), None);
    assert_eq!(parse_optional_timeout_seconds(Some("0")), None);
    assert_eq!(parse_optional_timeout_seconds(Some("abc")), None);
    assert_eq!(
        parse_optional_timeout_seconds(Some("120")),
        Some(Duration::from_secs(120))
    );
}

#[test]
fn normalize_legacy_snake_case_method_alias() {
    let response = json!({ "decision": "accept_for_session" });
    let normalized = normalize_approval_response(Some("exec_command_approval"), response);

    assert_eq!(normalized, json!({ "decision": "approved_for_session" }));
}

#[test]
fn sandbox_denial_ignores_opaque_action_failures() {
    let event = EngineEvent::ActionCompleted {
        action_id: "action-1".to_string(),
        result: ActionResult {
            success: false,
            output: None,
            error: Some("Action failed with status `failed`".to_string()),
            diff: None,
            duration_ms: 52,
        },
    };

    assert!(!event_indicates_sandbox_denial(&event));
}

#[test]
fn sandbox_denial_requires_explicit_sandbox_signature() {
    let event = EngineEvent::ActionCompleted {
        action_id: "action-1".to_string(),
        result: ActionResult {
            success: false,
            output: None,
            error: Some(
                "sandbox denied: operation not permitted while writing /etc/hosts".to_string(),
            ),
            diff: None,
            duration_ms: 52,
        },
    };

    assert!(event_indicates_sandbox_denial(&event));
}

#[test]
fn opaque_workspace_probe_error_excludes_transport_failures() {
    assert!(!is_opaque_workspace_probe_failure(
        "all rpc methods failed: command/exec: timed out waiting for response"
    ));
    assert!(is_opaque_workspace_probe_failure(
        "all rpc methods failed: command/exec: failed"
    ));
}

#[test]
fn workspace_probe_result_detects_failed_status_payload() {
    let payload = json!({
        "status": "failed",
        "exitCode": null,
        "stderr": ""
    });

    assert!(workspace_probe_result_indicates_failure(&payload));
}

#[test]
fn workspace_probe_result_detects_non_zero_exit_code() {
    let payload = json!({
        "status": "completed",
        "exitCode": 137,
        "stderr": "sandbox error: command was killed by a signal"
    });

    assert!(workspace_probe_result_indicates_failure(&payload));
}

#[test]
fn workspace_probe_result_accepts_successful_payload() {
    let payload = json!({
        "status": "completed",
        "exitCode": 0,
        "stdout": "",
        "stderr": ""
    });

    assert!(!workspace_probe_result_indicates_failure(&payload));
}

#[test]
fn thread_runtime_preserves_requested_reasoning_effort_from_start_response() {
    let response = json!({
        "cwd": "/tmp/effective",
        "model": "gpt-5.3-codex",
        "approvalPolicy": "untrusted",
        "sandbox": {
            "type": "externalSandbox",
            "networkAccess": "restricted"
        },
        "reasoningEffort": "high"
    });

    let runtime = thread_runtime_from_start_response(
        &response,
        "/tmp/fallback",
        "gpt-5",
        &json!("on-request"),
        None,
        None,
        &json!({"type":"workspaceWrite"}),
        Some("medium".to_string()),
        Some("flex".to_string()),
        Some("friendly".to_string()),
        Some(json!({"type":"object"})),
        false,
    );

    assert_eq!(runtime.cwd, "/tmp/effective");
    assert_eq!(runtime.model_id, "gpt-5.3-codex");
    assert_eq!(runtime.approval_policy, json!("untrusted"));
    assert_eq!(
        runtime.sandbox_policy,
        json!({
            "type": "externalSandbox",
            "networkAccess": "restricted"
        })
    );
    // 旧错误断言保留在注释中：
    // assert_eq!(runtime.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(runtime.service_tier.as_deref(), Some("flex"));
    assert_eq!(runtime.personality.as_deref(), Some("friendly"));
    assert_eq!(runtime.output_schema, Some(json!({"type":"object"})));
}

#[test]
fn thread_runtime_falls_back_when_response_omits_fields() {
    let response = json!({});
    let runtime = thread_runtime_from_start_response(
        &response,
        "/tmp/fallback",
        "gpt-5",
        &json!("on-request"),
        None,
        None,
        &json!({"type":"workspaceWrite","networkAccess":false}),
        Some("medium".to_string()),
        Some("fast".to_string()),
        Some("pragmatic".to_string()),
        Some(json!(true)),
        false,
    );

    assert_eq!(runtime.cwd, "/tmp/fallback");
    assert_eq!(runtime.model_id, "gpt-5");
    assert_eq!(runtime.approval_policy, json!("on-request"));
    assert_eq!(
        runtime.sandbox_policy,
        json!({"type":"workspaceWrite","networkAccess":false})
    );
    assert_eq!(runtime.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(runtime.service_tier.as_deref(), Some("fast"));
    assert_eq!(runtime.personality.as_deref(), Some("pragmatic"));
    assert_eq!(runtime.output_schema, Some(json!(true)));
}

#[test]
fn thread_runtime_keeps_requested_none_when_start_response_has_default_effort() {
    let response = json!({
        "reasoningEffort": "medium"
    });
    let runtime = thread_runtime_from_start_response(
        &response,
        "/tmp/fallback",
        "gpt-5",
        &json!("on-request"),
        None,
        None,
        &json!({"type":"workspaceWrite"}),
        None,
        None,
        None,
        None,
        false,
    );

    assert_eq!(runtime.reasoning_effort, None);
}

#[tokio::test]
async fn build_turn_start_params_uses_requested_effort_after_start_response_default() {
    let response = json!({
        "reasoningEffort": "medium"
    });
    let runtime = thread_runtime_from_start_response(
        &response,
        "/tmp/workspace",
        "gpt-5.4",
        &json!("on-request"),
        None,
        None,
        &json!({"type":"workspaceWrite"}),
        Some("high".to_string()),
        None,
        None,
        None,
        false,
    );
    let input = TurnInput {
        message: "Inspect the repo".to_string(),
        attachments: Vec::new(),
        plan_mode: false,
        input_items: vec![TurnInputItem::Text {
            text: "Inspect the repo".to_string(),
        }],
        permissions: None,
    };

    let params = build_turn_start_params(
        "thread-123",
        Some(&runtime),
        &input,
        PlanModeActivation::Disabled,
    )
    .await
    .expect("turn/start params");

    assert_eq!(params.get("effort"), Some(&json!("high")));

    let runtime_without_requested_effort = thread_runtime_from_start_response(
        &response,
        "/tmp/workspace",
        "gpt-5.4",
        &json!("on-request"),
        None,
        None,
        &json!({"type":"workspaceWrite"}),
        None,
        None,
        None,
        None,
        false,
    );
    let params_without_requested_effort = build_turn_start_params(
        "thread-123",
        Some(&runtime_without_requested_effort),
        &input,
        PlanModeActivation::Disabled,
    )
    .await
    .expect("turn/start params without effort");

    assert!(!params_without_requested_effort
        .as_object()
        .unwrap()
        .contains_key("effort"));
}

#[test]
fn thread_runtime_from_resume_response_prefers_requested_runtime() {
    let requested_runtime = ThreadRuntime {
        cwd: "/tmp/requested".to_string(),
        model_id: "gpt-5.1-codex-mini".to_string(),
        approval_policy: json!("on-request"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/requested"],
            "networkAccess": false,
        }),
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("flex".to_string()),
        personality: Some("friendly".to_string()),
        output_schema: Some(json!({"type":"object"})),
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: false,
    };
    let response = json!({
        "cwd": "/tmp/stale",
        "model": "gpt-5.3-codex",
        "approvalPolicy": "never",
        "sandbox": {
            "type": "dangerFullAccess",
        },
        "reasoningEffort": "xhigh"
    });

    let runtime = thread_runtime_from_resume_response(&response, &requested_runtime);

    assert_eq!(runtime, requested_runtime);
}

#[test]
fn preserve_live_thread_runtime_flags_keeps_native_plan_mode_from_existing_runtime() {
    let existing_runtime = ThreadRuntime {
        cwd: "/tmp/original".to_string(),
        model_id: "gpt-5.4".to_string(),
        approval_policy: json!("on-request"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/original"],
            "networkAccess": false,
        }),
        reasoning_effort: Some("medium".to_string()),
        service_tier: Some("default".to_string()),
        personality: Some("friendly".to_string()),
        output_schema: Some(json!({"type":"object"})),
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: true,
    };
    let requested_runtime = ThreadRuntime {
        cwd: "/tmp/updated".to_string(),
        model_id: "gpt-5.4".to_string(),
        approval_policy: json!("never"),
        permission_profile: None,
        approvals_reviewer: None,
        writable_roots: Vec::new(),
        sandbox_policy: json!({
            "type": "workspaceWrite",
            "writableRoots": ["/tmp/updated"],
            "networkAccess": true,
        }),
        reasoning_effort: Some("high".to_string()),
        service_tier: Some("flex".to_string()),
        personality: Some("precise".to_string()),
        output_schema: None,
        auto_approve_mcp_elicitations: false,
        native_plan_mode_active: false,
    };

    let preserved_runtime =
        preserve_live_thread_runtime_flags(requested_runtime, Some(&existing_runtime));

    assert!(preserved_runtime.native_plan_mode_active);
    assert_eq!(preserved_runtime.cwd, "/tmp/updated");
    assert_eq!(preserved_runtime.approval_policy, json!("never"));
    assert_eq!(preserved_runtime.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn extract_codex_remote_thread_summary_reads_thread_list_shape() {
    let summary = extract_codex_remote_thread_summary(
        &json!({
            "id": "thread-123",
            "name": "Remote thread",
            "preview": "Most recent preview",
            "cwd": "/tmp/workspace",
            "createdAt": 1710000000,
            "updatedAt": 1710003600,
            "model": "gpt-5.6-terra",
            "reasoningEffort": "high",
            "modelProvider": "openai",
            "source": "appServer",
            "status": {
                "type": "active",
                "activeFlags": ["waitingOnApproval"]
            }
        }),
        false,
    )
    .expect("expected summary");

    assert_eq!(summary.engine_thread_id, "thread-123");
    assert_eq!(summary.title.as_deref(), Some("Remote thread"));
    assert_eq!(summary.preview, "Most recent preview");
    assert_eq!(summary.cwd, "/tmp/workspace");
    assert_eq!(summary.created_at, 1710000000);
    assert_eq!(summary.updated_at, 1710003600);
    assert_eq!(summary.model_id.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(summary.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(summary.model_provider, "openai");
    assert_eq!(summary.source_kind, "appServer");
    assert_eq!(summary.status_type, "active");
    assert_eq!(summary.active_flags, vec!["waitingOnApproval".to_string()]);
    assert!(!summary.archived);
}

#[test]
fn extract_codex_turn_runtime_reads_model_and_reasoning_effort() {
    let (model_id, reasoning_effort) = extract_codex_turn_runtime(&json!({
        "model": "gpt-5.6-terra",
        "reasoningEffort": "high"
    }));

    assert_eq!(model_id.as_deref(), Some("gpt-5.6-terra"));
    assert_eq!(reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn extract_codex_remote_thread_summary_maps_sub_agent_sources() {
    let summary = extract_codex_remote_thread_summary(
        &json!({
            "thread": {
                "id": "thread-456",
                "cwd": "/tmp/repo",
                "createdAt": 1710000000,
                "updatedAt": 1710000001,
                "modelProvider": "openai",
                "source": {
                    "subAgent": {
                        "thread_spawn": true
                    }
                },
                "status": {
                    "type": "idle",
                    "activeFlags": []
                }
            },
            "preview": "Preview"
        }),
        true,
    )
    .expect("expected summary");

    assert_eq!(summary.source_kind, "subAgentThreadSpawn");
    assert!(summary.archived);
}

#[tokio::test]
async fn resolve_pending_approval_request_removes_matching_request() {
    let state = Arc::new(Mutex::new(CodexState::default()));
    {
        let mut locked = state.lock().await;
        locked.approval_requests.insert(
            "approval-1".to_string(),
            PendingApproval {
                raw_request_id: json!(42),
                method: "item/fileChange/requestApproval".to_string(),
            },
        );
    }

    let approval_id = resolve_pending_approval_request(state.clone(), &json!(42)).await;

    assert_eq!(approval_id.as_deref(), Some("approval-1"));
    let locked = state.lock().await;
    assert!(locked.approval_requests.is_empty());
}

#[tokio::test]
async fn runtime_model_fallback_prefers_cached_runtime_models() {
    let engine = CodexEngine::default();
    let cached_models = vec![ModelInfo {
        id: "cached-model".to_string(),
        display_name: "cached-model".to_string(),
        description: "Runtime cached model".to_string(),
        hidden: false,
        is_default: true,
        upgrade: None,
        availability_nux: None,
        upgrade_info: None,
        input_modalities: vec!["text".to_string()],
        attachment_modalities: vec!["text".to_string()],
        limits: None,
        supports_personality: true,
        default_reasoning_effort: "minimal".to_string(),
        supported_reasoning_efforts: vec![ReasoningEffortOption {
            reasoning_effort: "minimal".to_string(),
            description: "Fastest".to_string(),
        }],
    }];

    engine
        .store_runtime_model_cache(cached_models.clone())
        .await;

    assert_eq!(
        engine
            .runtime_model_fallback()
            .await
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        cached_models
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn update_protocol_diagnostics_with_config_warning_tracks_end_range() {
    let state = Arc::new(Mutex::new(CodexState::default()));

    let diagnostics = update_protocol_diagnostics_with_config_warning(
        state,
        &json!({
            "summary": "Bad config",
            "path": "/tmp/config.toml",
            "range": {
                "start": { "line": 2, "column": 4 },
                "end": { "line": 2, "column": 12 }
            }
        }),
    )
    .await
    .expect("diagnostics should update");

    let warning = diagnostics
        .last_config_warning
        .expect("config warning should be stored");
    assert_eq!(warning.start_line, Some(2));
    assert_eq!(warning.start_column, Some(4));
    assert_eq!(warning.end_line, Some(2));
    assert_eq!(warning.end_column, Some(12));
}

#[tokio::test]
async fn update_protocol_diagnostics_with_windows_and_realtime_notifications() {
    let state = Arc::new(Mutex::new(CodexState::default()));

    let diagnostics = update_protocol_diagnostics_with_windows_world_writable_warning(
        state.clone(),
        &json!({
            "samplePaths": ["C:/tmp/a", "C:/tmp/b"],
            "extraCount": 3,
            "failedScan": true
        }),
    )
    .await
    .expect("world writable warning should update");
    assert_eq!(
        diagnostics
            .last_windows_world_writable_warning
            .as_ref()
            .map(|value| value.extra_count),
        Some(3)
    );

    let diagnostics = update_protocol_diagnostics_with_windows_sandbox_setup(
        state.clone(),
        &json!({
            "mode": "unelevated",
            "success": false,
            "error": "permission denied"
        }),
    )
    .await
    .expect("sandbox setup should update");
    assert_eq!(
        diagnostics
            .last_windows_sandbox_setup
            .as_ref()
            .map(|value| value.error.as_deref()),
        Some(Some("permission denied"))
    );

    let diagnostics = update_protocol_diagnostics_with_thread_realtime(
        state,
        &normalize_method("thread/realtime/outputAudio/delta"),
        &json!({
            "threadId": "thread-123",
            "audio": {
                "sampleRate": 24000,
                "numChannels": 2,
                "samplesPerChannel": 480,
                "data": "abc"
            }
        }),
    )
    .await
    .expect("thread realtime should update");
    let realtime = diagnostics
        .last_thread_realtime
        .expect("thread realtime event should be stored");
    assert_eq!(realtime.kind, "thread/realtime/outputaudio/delta");
    assert_eq!(realtime.thread_id, "thread-123");
    assert_eq!(realtime.sample_rate, Some(24000));
    assert_eq!(realtime.num_channels, Some(2));
    assert_eq!(realtime.samples_per_channel, Some(480));
}

#[tokio::test]
async fn update_protocol_diagnostics_with_account_update_tracks_auth_mode() {
    let state = Arc::new(Mutex::new(CodexState::default()));

    let diagnostics = update_protocol_diagnostics_with_account_update(
        state,
        &json!({
            "authMode": "chatgptAuthTokens",
            "planType": "team"
        }),
    )
    .await
    .expect("account update should update diagnostics");

    let account = diagnostics
        .account
        .expect("account diagnostics should exist");
    assert_eq!(account.provider, "chatgpt");
    assert_eq!(account.auth_mode.as_deref(), Some("chatgptAuthTokens"));
    assert_eq!(account.plan_type.as_deref(), Some("team"));
}

#[test]
fn known_codex_notification_methods_include_remaining_runtime_notifications() {
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/started"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/archived"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/deleted"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/unarchived"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/closed"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "item/reasoning/summaryPartAdded"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "item/commandExecution/terminalInteraction"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/realtime/started"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "thread/realtime/outputAudio/delta"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "windows/worldWritableWarning"
    )));
    assert!(is_known_codex_notification_method(&normalize_method(
        "windowsSandbox/setupCompleted"
    )));
}

#[test]
fn unsupported_external_auth_tokens_message_includes_context() {
    let message =
        unsupported_external_auth_tokens_message(Some("acc_123"), Some("unauthorized"));
    assert!(message.contains("acc_123"));
    assert!(message.contains("unauthorized"));
    assert!(message.contains("cannot refresh"));
}

#[test]
fn event_indicates_auth_failure_for_top_level_error() {
    let event = EngineEvent::Error {
        message: "401 Unauthorized".to_string(),
        recoverable: false,
    };

    assert!(event_indicates_auth_failure(&event));
}

#[test]
fn event_indicates_auth_failure_ignores_failed_tool_output() {
    let event = EngineEvent::ActionCompleted {
        action_id: "action-1".to_string(),
        result: ActionResult {
            success: false,
            output: Some("curl failed with 401 Unauthorized".to_string()),
            error: Some("request failed".to_string()),
            diff: None,
            duration_ms: 10,
        },
    };

    assert!(!event_indicates_auth_failure(&event));
}

#[test]
fn codex_health_checks_use_windows_commands() {
    let checks = codex_health_checks_for_platform("windows");

    assert!(checks.contains(&"where codex".to_string()));
    assert!(checks.contains(&"where node".to_string()));
    assert!(checks.contains(&"echo %PATH%".to_string()));
    assert!(!checks.iter().any(|check| check == "command -v codex"));
}

#[test]
fn codex_unavailable_details_for_windows_mentions_appdata_npm() {
    let details = codex_unavailable_details_for_platform(
        "windows",
        &CodexExecutableResolution {
            executable: None,
            source: "unavailable",
            app_path: Some(r"C:\Windows\System32".to_string()),
            login_shell_executable: None,
        },
    )
    .expect("details should exist");

    assert!(details.contains("%APPDATA%\\npm"));
    assert!(details.contains("App PATH"));
}

#[test]
fn codex_fix_commands_for_windows_cover_install_and_path() {
    let fixes = codex_fix_commands_for_platform(
        "windows",
        &CodexExecutableResolution {
            executable: None,
            source: "unavailable",
            app_path: Some(r"C:\Windows\System32".to_string()),
            login_shell_executable: None,
        },
        None,
    );

    assert!(fixes.contains(&"npm install -g @openai/codex".to_string()));
    assert!(fixes.contains(&"where codex".to_string()));
    assert!(fixes.iter().any(|fix| fix.contains("%APPDATA%\\npm")));
}

#[test]
fn codex_execution_failure_details_for_windows_mentions_node_path() {
    let details = codex_execution_failure_details_for_platform(
        "windows",
        &CodexExecutableResolution {
            executable: Some(std::path::PathBuf::from(
                r"C:\Users\auracoder\AppData\Roaming\npm\codex.cmd",
            )),
            source: "app-path",
            app_path: Some(r"C:\Windows\System32".to_string()),
            login_shell_executable: None,
        },
        "env: node: no such file or directory",
    );

    assert!(details.contains("missing from PATH on Windows"));
    assert!(details.contains("node"));
}

#[test]
fn map_codex_model_preserves_runtime_metadata() {
    let model = CodexModel {
        id: "gpt-5.4".to_string(),
        display_name: Some("gpt-5.4".to_string()),
        description: Some("Latest frontier agentic coding model.".to_string()),
        hidden: Some(false),
        is_default: Some(true),
        upgrade: Some("gpt-5.5".to_string()),
        availability_nux: Some(CodexModelAvailabilityNux {
            message: "Try this model for your current plan.".to_string(),
        }),
        upgrade_info: Some(CodexModelUpgradeInfo {
            model: "gpt-5.5".to_string(),
            upgrade_copy: Some("Upgrade available".to_string()),
            model_link: Some("https://example.com".to_string()),
            migration_markdown: Some("Introducing GPT-5.5".to_string()),
        }),
        input_modalities: vec!["text".to_string(), "image".to_string()],
        supports_personality: Some(true),
        default_reasoning_effort: Some("minimal".to_string()),
        supported_reasoning_efforts: vec![CodexReasoningEffortOption {
            reasoning_effort: "minimal".to_string(),
            description: "Fastest responses".to_string(),
        }],
    };

    let mapped = map_codex_model(model);

    assert_eq!(mapped.upgrade.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        mapped
            .availability_nux
            .as_ref()
            .map(|value| value.message.as_str()),
        Some("Try this model for your current plan.")
    );
    assert_eq!(
        mapped
            .upgrade_info
            .as_ref()
            .map(|value| value.model.as_str()),
        Some("gpt-5.5")
    );
    assert_eq!(
        mapped
            .upgrade_info
            .as_ref()
            .and_then(|value| value.upgrade_copy.as_deref()),
        Some("Upgrade available")
    );
    assert_eq!(mapped.input_modalities, vec!["text", "image"]);
    assert!(mapped.supports_personality);
    assert_eq!(mapped.default_reasoning_effort, "minimal");
    assert_eq!(
        mapped.supported_reasoning_efforts[0].reasoning_effort,
        "minimal"
    );
}

#[test]
fn map_codex_model_defaults_modalities_when_runtime_omits_them() {
    let model = CodexModel {
        id: "gpt-5.4".to_string(),
        display_name: None,
        description: None,
        hidden: None,
        is_default: None,
        upgrade: None,
        availability_nux: None,
        upgrade_info: None,
        input_modalities: Vec::new(),
        supports_personality: None,
        default_reasoning_effort: None,
        supported_reasoning_efforts: Vec::new(),
    };

    let mapped = map_codex_model(model);

    assert_eq!(mapped.input_modalities, vec!["text", "image"]);
    assert!(!mapped.supports_personality);
}

#[test]
fn map_skill_entries_flattens_and_sorts_skills() {
    let mapped = map_skill_entries(&[
        json!({
            "cwd": "/tmp/workspace",
            "skills": [
                {
                    "name": "repo-skill",
                    "path": "/tmp/workspace/.codex/repo-skill",
                    "description": "Repo-local skill",
                    "enabled": true,
                    "scope": "repo"
                },
                {
                    "name": "user-skill",
                    "path": "/Users/auracoder/.codex/user-skill",
                    "description": "User skill",
                    "enabled": true,
                    "scope": "user"
                }
            ],
            "errors": []
        }),
        json!({
            "cwd": "/tmp/workspace",
            "skills": [
                {
                    "name": "repo-skill",
                    "path": "/tmp/workspace/.codex/repo-skill",
                    "description": "Repo-local skill",
                    "enabled": true,
                    "scope": "repo"
                }
            ],
            "errors": []
        }),
    ]);

    assert_eq!(
        mapped
            .iter()
            .map(|skill| (skill.scope.as_str(), skill.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("repo", "repo-skill"), ("user", "user-skill")]
    );
}

#[test]
fn map_plugin_marketplaces_prefers_display_metadata() {
    let mapped = map_plugin_marketplaces(&json!({
        "marketplaces": [
            {
                "name": "default",
                "path": "/tmp/plugins",
                "plugins": [
                    {
                        "id": "deploy",
                        "name": "deploy",
                        "enabled": true,
                        "installed": true,
                        "interface": {
                            "displayName": "Deploy Helper",
                            "developerName": "OpenAI",
                            "shortDescription": "Ship builds faster",
                            "capabilities": ["composer", "review"]
                        }
                    }
                ]
            }
        ]
    }));

    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].plugins[0].name, "Deploy Helper");
    assert_eq!(
        mapped[0].plugins[0].developer_name.as_deref(),
        Some("OpenAI")
    );
    assert_eq!(
        mapped[0].plugins[0].capabilities,
        vec!["composer".to_string(), "review".to_string()]
    );
}

#[test]
fn map_config_state_uses_layers_and_structured_values() {
    let mapped = map_config_state(&json!({
        "config": {
            "model": "gpt-5.4",
            "model_provider": "openai",
            "service_tier": "flex",
            "approval_policy": {
                "reject": {
                    "mcp_elicitations": true,
                    "rules": false,
                    "sandbox_approval": false
                }
            },
            "sandbox_mode": "workspace-write",
            "web_search": "enabled",
            "profile": "default"
        },
        "layers": [
            {
                "name": {
                    "type": "user",
                    "file": "/Users/auracoder/.codex/config.toml"
                },
                "version": "v2",
                "config": {}
            }
        ],
        "origins": {}
    }));

    assert_eq!(mapped.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(mapped.model_provider.as_deref(), Some("openai"));
    assert_eq!(mapped.service_tier.as_deref(), Some("flex"));
    assert_eq!(mapped.sandbox_mode.as_deref(), Some("workspace-write"));
    assert_eq!(mapped.web_search.as_deref(), Some("enabled"));
    assert_eq!(mapped.profile.as_deref(), Some("default"));
    assert_eq!(mapped.layers.len(), 1);
    assert_eq!(
        mapped.layers[0].source,
        "user:/Users/auracoder/.codex/config.toml"
    );
    assert_eq!(mapped.layers[0].version, "v2");
    assert!(mapped.approval_policy.is_some());
}

/// 验证 Windows 修复命令与用户说明保留原标签并拥有各自动作类型。
#[test]
fn windows_codex_fix_actions_distinguish_commands_and_instructions() {
    let resolution = CodexExecutableResolution {
        executable: None,
        source: "unavailable",
        app_path: Some(r"C:\Windows\System32".to_string()),
        login_shell_executable: None,
    };
    let actions = codex_fix_actions_for_platform("windows", &resolution, None);
    let labels = actions
        .iter()
        .map(|action| action.label().to_string())
        .collect::<Vec<_>>();

    assert_eq!(actions.len(), 4);
    assert!(matches!(&actions[0], EngineCheckAction::Shell(_)));
    assert!(matches!(&actions[1], EngineCheckAction::Shell(_)));
    assert!(matches!(&actions[2], EngineCheckAction::Shell(_)));
    assert!(matches!(&actions[3], EngineCheckAction::Information(_)));
    assert_eq!(
        labels,
        codex_fix_commands_for_platform("windows", &resolution, None)
    );
}

