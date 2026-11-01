
    use super::*;

    #[test]
    fn parse_notify_cli_args_reads_all_supported_flags() {
        let parsed = parse_notify_cli_args(vec![
            "--title".to_string(),
            "Codex".to_string(),
            "--body".to_string(),
            "Turn complete".to_string(),
            "--workspace-id".to_string(),
            "ws-1".to_string(),
            "--session-id".to_string(),
            "term-1".to_string(),
            "--source".to_string(),
            "codex".to_string(),
        ])
        .expect("notify args should parse");

        assert_eq!(
            parsed,
            Some(NotifyCliArgs {
                title: Some("Codex".to_string()),
                body: Some("Turn complete".to_string()),
                workspace_id: Some("ws-1".to_string()),
                session_id: Some("term-1".to_string()),
                source: Some("codex".to_string()),
            })
        );
    }

    #[test]
    fn parse_notify_cli_args_returns_none_for_help() {
        let parsed = parse_notify_cli_args(vec!["--help".to_string()])
            .expect("notify help args should parse");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_clear_notification_cli_args_reads_supported_flags() {
        let parsed = parse_clear_notification_cli_args(vec![
            "--workspace-id".to_string(),
            "ws-1".to_string(),
            "--session-id".to_string(),
            "term-1".to_string(),
        ])
        .expect("clear args should parse");

        assert_eq!(
            parsed,
            Some(ClearNotificationCliArgs {
                workspace_id: Some("ws-1".to_string()),
                session_id: Some("term-1".to_string()),
            })
        );
    }

    #[test]
    fn parse_clear_notification_cli_args_returns_none_for_help() {
        let parsed = parse_clear_notification_cli_args(vec!["--help".to_string()])
            .expect("clear help args should parse");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_codex_notify_args_reads_single_payload() {
        let parsed = parse_codex_notify_args(vec![r#"{"type":"agent-turn-complete"}"#.to_string()])
            .expect("Codex notify args should parse");

        assert_eq!(
            parsed,
            Some(r#"{"type":"agent-turn-complete"}"#.to_string())
        );
    }

    #[test]
    fn parse_codex_notify_args_returns_none_for_help() {
        let parsed =
            parse_codex_notify_args(vec!["--help".to_string()]).expect("help should parse");

        assert_eq!(parsed, None);
    }

    #[test]
    fn codex_notify_cli_args_from_payload_maps_agent_turn_complete() {
        let parsed = codex_notify_cli_args_from_payload(
            r#"{"type":"agent-turn-complete","last-assistant-message":"Ship it","input-messages":["please finish"]}"#,
        )
        .expect("Codex payload should parse");

        assert_eq!(
            parsed,
            Some(NotifyCliArgs {
                title: Some("Codex".to_string()),
                body: Some("Ship it".to_string()),
                workspace_id: None,
                session_id: None,
                source: Some("codex".to_string()),
            })
        );
    }

    #[test]
    fn codex_notify_cli_args_from_payload_ignores_other_events() {
        let parsed = codex_notify_cli_args_from_payload(r#"{"type":"approval-requested"}"#)
            .expect("non-terminal Codex payload should parse");

        assert_eq!(parsed, None);
    }

    #[test]
    fn codex_notify_cli_args_from_payload_uses_codex_specific_fallback_body() {
        let parsed = codex_notify_cli_args_from_payload(r#"{"type":"agent-turn-complete"}"#)
            .expect("Codex payload should parse");

        assert_eq!(
            parsed,
            Some(NotifyCliArgs {
                title: Some("Codex".to_string()),
                body: Some("Turn complete".to_string()),
                workspace_id: None,
                session_id: None,
                source: Some("codex".to_string()),
            })
        );
    }

    #[test]
    fn build_claude_forwarded_args_injects_settings_and_session_id() {
        let forwarded = build_claude_forwarded_args_with_session(
            vec!["review this diff".to_string()],
            "session-1",
        )
        .unwrap();

        assert_eq!(forwarded[0], "--settings");
        let settings: Value =
            serde_json::from_str(&forwarded[1]).expect("settings should be valid JSON");
        assert_eq!(
            settings["hooks"][CLAUDE_STOP_HOOK_EVENT]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            settings["hooks"][CLAUDE_STOP_HOOK_EVENT][0]["matcher"],
            Value::String(".*".to_string())
        );
        assert_eq!(
            settings["hooks"][CLAUDE_STOP_HOOK_EVENT][0]["hooks"][0]["timeout"],
            Value::Number(10.into())
        );
        assert_eq!(forwarded[2], "--session-id");
        assert_eq!(forwarded[3], "session-1");
        assert_eq!(forwarded[4], "review this diff");
    }

    #[test]
    fn build_codex_forwarded_args_injects_notify_override() {
        let forwarded =
            build_codex_forwarded_args_with_notify(vec!["fix this".to_string()]).unwrap();

        assert_eq!(
            forwarded,
            vec![
                "-c".to_string(),
                CODEX_NOTIFY_CONFIG_OVERRIDE.to_string(),
                "fix this".to_string(),
            ]
        );
    }

    #[test]
    fn build_codex_forwarded_args_respects_existing_notify_override() {
        let forwarded = build_codex_forwarded_args_with_notify(vec![
            "--config".to_string(),
            r#"notify=["custom","notify"]"#.to_string(),
            "fix this".to_string(),
        ])
        .unwrap();

        assert_eq!(
            forwarded,
            vec![
                "--config".to_string(),
                r#"notify=["custom","notify"]"#.to_string(),
                "fix this".to_string(),
            ]
        );
    }

    #[test]
    fn build_codex_forwarded_args_passthroughs_help() {
        let forwarded = build_codex_forwarded_args_with_notify(vec!["--help".to_string()]).unwrap();

        assert_eq!(forwarded, vec!["--help".to_string()]);
    }

    #[test]
    fn build_claude_forwarded_args_merges_existing_settings() {
        let existing = json!({
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo custom"
                            }
                        ]
                    }
                ]
            }
        })
        .to_string();
        let forwarded = build_claude_forwarded_args_with_session(
            vec![
                "--settings".to_string(),
                existing,
                "review this diff".to_string(),
            ],
            "session-1",
        )
        .unwrap();
        let settings: Value =
            serde_json::from_str(&forwarded[1]).expect("settings should be valid JSON");
        assert_eq!(
            settings["hooks"][CLAUDE_STOP_HOOK_EVENT]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn build_claude_forwarded_args_respects_bare_mode() {
        let forwarded =
            build_claude_forwarded_args_with_session(vec!["--bare".to_string()], "session-1")
                .unwrap();

        assert_eq!(
            forwarded,
            vec![
                "--session-id".to_string(),
                "session-1".to_string(),
                "--bare".to_string(),
            ]
        );
    }

    #[test]
    fn build_claude_forwarded_args_preserves_settings_in_bare_mode() {
        let forwarded = build_claude_forwarded_args_with_session(
            vec![
                "--bare".to_string(),
                "--settings".to_string(),
                r#"{"env":{"FOO":"bar"}}"#.to_string(),
            ],
            "session-1",
        )
        .unwrap();

        assert_eq!(
            forwarded,
            vec![
                "--session-id".to_string(),
                "session-1".to_string(),
                "--bare".to_string(),
                "--settings".to_string(),
                r#"{"env":{"FOO":"bar"}}"#.to_string(),
            ]
        );
    }

    #[test]
    fn build_claude_forwarded_args_passthroughs_subcommands() {
        let forwarded = build_claude_forwarded_args_with_session(
            vec!["auth".to_string(), "status".to_string()],
            "session-1",
        )
        .unwrap();

        assert_eq!(forwarded, vec!["auth".to_string(), "status".to_string()]);
    }

    #[test]
    fn build_claude_forwarded_args_passthroughs_help() {
        let forwarded =
            build_claude_forwarded_args_with_session(vec!["--help".to_string()], "session-1")
                .unwrap();

        assert_eq!(forwarded, vec!["--help".to_string()]);
    }

    #[test]
    fn claude_hook_action_from_payload_maps_notification() {
        let action = claude_hook_action_from_payload(
            r#"{"hook_event_name":"Notification","title":"Claude needs you","message":"Approve this command","notification_type":"permission_prompt"}"#,
        )
        .expect("Claude notification payload should parse");

        assert_eq!(
            action,
            Some(ClaudeHookAction::Notify {
                title: "Claude needs you".to_string(),
                body: "Approve this command".to_string(),
                source: "claude".to_string(),
            })
        );
    }

    #[test]
    fn claude_hook_action_from_payload_maps_stop_completion() {
        let action = claude_hook_action_from_payload(
            r#"{"hook_event_name":"Stop","last_assistant_message":"All set","stop_hook_active":false}"#,
        )
        .expect("Claude stop payload should parse");

        assert_eq!(
            action,
            Some(ClaudeHookAction::Notify {
                title: "Claude".to_string(),
                body: "All set".to_string(),
                source: "claude".to_string(),
            })
        );
    }

    #[test]
    fn claude_hook_action_from_payload_ignores_active_stop_hook() {
        let action = claude_hook_action_from_payload(
            r#"{"hook_event_name":"Stop","last_assistant_message":"All set","stop_hook_active":true}"#,
        )
        .expect("Claude stop payload should parse");

        assert_eq!(action, None);
    }

    #[test]
    fn claude_hook_action_from_payload_maps_stop_failure() {
        let action = claude_hook_action_from_payload(
            r#"{"hook_event_name":"StopFailure","error":"rate_limit","last_assistant_message":"API Error: Rate limit reached"}"#,
        )
        .expect("Claude stop failure payload should parse");

        assert_eq!(
            action,
            Some(ClaudeHookAction::Notify {
                title: "Claude Error".to_string(),
                body: "API Error: Rate limit reached".to_string(),
                source: "claude".to_string(),
            })
        );
    }

    #[test]
    fn claude_hook_action_from_payload_clears_on_session_lifecycle() {
        let start = claude_hook_action_from_payload(r#"{"hook_event_name":"SessionStart"}"#)
            .expect("Claude session-start payload should parse");
        let end = claude_hook_action_from_payload(r#"{"hook_event_name":"SessionEnd"}"#)
            .expect("Claude session-end payload should parse");

        assert_eq!(start, Some(ClaudeHookAction::Clear));
        assert_eq!(end, Some(ClaudeHookAction::Clear));
    }

    #[test]
    fn normalize_notification_text_trims_collapses_and_truncates() {
        let normalized = normalize_notification_text(
            Some("  hello\n\nworld  from   auracoder  "),
            "fallback",
            11,
        );
        assert_eq!(normalized, "hello world…");
    }

    #[test]
    fn normalize_optional_value_rejects_blank_values() {
        assert_eq!(normalize_optional_value(Some("  ".to_string())), None);
        assert_eq!(
            normalize_optional_value(Some(" ws-1 ".to_string())),
            Some("ws-1".to_string())
        );
    }

    #[test]
    fn focus_matches_target_uses_window_focus_over_workspace_and_session() {
        let focused = NotificationFocusState {
            window_focused: true,
            workspace_id: Some("ws-1".to_string()),
            session_id: Some("term-1".to_string()),
        };
        let unfocused = NotificationFocusState {
            window_focused: false,
            workspace_id: Some("ws-1".to_string()),
            session_id: Some("term-1".to_string()),
        };

        assert!(focus_matches_target(&focused, "ws-2", "term-2"));
        assert!(!focus_matches_target(&unfocused, "ws-1", "term-1"));
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_cli_shim_escapes_single_quotes() {
        let contents = auracoder_cli_shim_contents(Path::new("/tmp/AuraCoder' Dev"));
        assert!(contents.contains("'/tmp/AuraCoder'\\'' Dev'"));
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_claude_cli_shim_invokes_wrapper_subcommand() {
        let contents = claude_cli_shim_contents(Path::new("/tmp/AuraCoder' Dev"));
        assert!(contents.contains(CLAUDE_WRAPPER_SUBCOMMAND));
    }
