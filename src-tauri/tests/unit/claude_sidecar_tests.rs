    use super::*;

    #[test]
    fn deserializes_sessions_events_and_preserves_request_id() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "sessions",
            "id": "request-1",
            "sessions": [{
                "id": "session-1",
                "cwd": "/workspace/project",
                "title": "Project session",
                "updatedAt": "2026-08-27T00:00:00.000Z"
            }]
        }))
        .expect("sessions should deserialize");

        assert_eq!(event.request_id(), Some("request-1"));
        match event {
            SidecarEvent::Sessions { sessions, .. } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, "session-1");
                assert_eq!(sessions[0].cwd, "/workspace/project");
                assert_eq!(sessions[0].title, "Project session");
                assert_eq!(sessions[0].updated_at, "2026-08-27T00:00:00.000Z");
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn service_setters_are_safe_inside_tokio_runtime() {
        let engine = ClaudeSidecarEngine::default();
        engine.set_computer_control_service(Arc::new(ComputerControlService::default()));
        engine.set_mcp_gateway_connection(
            "http://127.0.0.1:30123/mcp".to_string(),
            "test-mcp-gateway-token".to_string(),
        );

        let db_path = std::env::temp_dir().join(format!(
            "claude-sidecar-services-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = crate::db::Database::open(db_path).expect("test database");
        engine.set_auracoder_thread_mcp_service(Arc::new(AuraCoderThreadMcpService::new(db)));

        let computer_control_service = engine
            .computer_control_service
            .lock()
            .expect("computer control service mutex should not be poisoned");
        assert!(
            computer_control_service.is_some(),
            "computer control service should be saved"
        );

        let auracoder_thread_mcp_service = engine
            .auracoder_thread_mcp_service
            .lock()
            .expect("AuraCoder thread MCP service mutex should not be poisoned");
        assert!(
            auracoder_thread_mcp_service.is_some(),
            "AuraCoder thread MCP service should be saved"
        );
        let mcp_gateway_endpoint = engine
            .mcp_gateway_endpoint
            .lock()
            .expect("MCP Gateway endpoint mutex should not be poisoned");
        assert_eq!(
            mcp_gateway_endpoint.as_deref(),
            Some("http://127.0.0.1:30123/mcp")
        );
        drop(mcp_gateway_endpoint);
        let mcp_gateway_token = engine
            .mcp_gateway_token
            .lock()
            .expect("MCP Gateway token mutex should not be poisoned");
        assert_eq!(mcp_gateway_token.as_deref(), Some("test-mcp-gateway-token"));
    }

    #[tokio::test]
    async fn startup_handshake_keeps_immediate_ready_event() {
        let temp_dir =
            std::env::temp_dir().join(format!("claude-sidecar-ready-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("temporary sidecar directory");
        let script_path = temp_dir.join("ready.mjs");
        fs::write(
            &script_path,
            r#"process.stdout.write(JSON.stringify({ type: "ready" }) + "\n"); setInterval(() => {}, 1000);"#,
        )
        .expect("ready script");

        let result = async {
            let (transport, startup_rx) = ClaudeTransport::spawn(script_path.clone()).await?;
            let event = timeout(Duration::from_secs(3), startup_rx)
                .await
                .context("ready handshake timed out")?
                .context("ready handshake sender closed")?;
            assert!(matches!(event, SidecarEvent::Ready));
            transport.kill().await;
            anyhow::Ok(())
        }
        .await;

        let _ = fs::remove_dir_all(&temp_dir);
        result.expect("immediate ready handshake should be delivered");
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_verbatim_sidecar_path_is_normalized_before_node_spawn() {
        let temp_dir =
            std::env::temp_dir().join(format!("claude-sidecar-verbatim-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("temporary sidecar directory");
        let script_path = temp_dir.join("ready.mjs");
        fs::write(
            &script_path,
            r#"process.stdout.write(JSON.stringify({ type: "ready" }) + "\n"); setInterval(() => {}, 1000);"#,
        )
        .expect("ready script");

        let result = async {
            let verbatim_path = crate::path_utils::legacy_windows_verbatim_path(&script_path)
                .context("verbatim sidecar path")?;
            assert!(verbatim_path.starts_with(r"\\?\"));
            let verbatim_path = PathBuf::from(verbatim_path);
            let (transport, startup_rx) = ClaudeTransport::spawn(verbatim_path).await?;
            let event = timeout(Duration::from_secs(3), startup_rx)
                .await
                .context("ready handshake timed out")?
                .context("ready handshake sender closed")?;
            assert!(matches!(event, SidecarEvent::Ready));
            assert!(!transport.sidecar_path.starts_with(r"\\?\"));
            assert_eq!(transport.sidecar_path, script_path.to_string_lossy());
            transport.kill().await;
            anyhow::Ok(())
        }
        .await;

        let _ = fs::remove_dir_all(&temp_dir);
        result.expect("verbatim sidecar path should be normalized before spawn");
    }

    #[tokio::test]
    async fn startup_error_preserves_stderr_paths_and_exit_status() {
        let temp_dir =
            std::env::temp_dir().join(format!("claude-sidecar-error-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("temporary sidecar directory");
        let script_path = temp_dir.join("error.mjs");
        fs::write(
            &script_path,
            r#"process.stderr.write("fixed startup diagnostic\n"); process.stdout.write(JSON.stringify({ type: "error", message: "fixed startup error", recoverable: false, errorType: "startup_dependency_load_failed", isAuthError: false }) + "\n"); process.exit(23);"#,
        )
        .expect("error script");

        let result = async {
            let (transport, startup_rx) = ClaudeTransport::spawn(script_path.clone()).await?;
            let event = timeout(Duration::from_secs(3), startup_rx)
                .await
                .context("error handshake timed out")?
                .context("error handshake sender closed")?;
            match event {
                SidecarEvent::Error { message, .. } => assert_eq!(message, "fixed startup error"),
                other => panic!("unexpected startup event: {other:?}"),
            }
            // 旧实现依赖固定 sleep 等待 stderr，保留迁移留痕；现在由 reader 完成握手接替。
            // tokio::time::sleep(Duration::from_millis(50)).await;
            let diagnostics = transport.startup_diagnostics().await;
            assert!(diagnostics.contains("fixed startup diagnostic"));
            assert!(diagnostics.contains(script_path.to_string_lossy().as_ref()));
            assert!(diagnostics.contains(&transport.runtime_path));
            assert!(diagnostics.contains("exitCode=23"));
            transport.kill().await;
            anyhow::Ok(())
        }
        .await;

        let _ = fs::remove_dir_all(&temp_dir);
        result.expect("startup error diagnostics should preserve context");
    }

    #[test]
    fn deserializes_action_output_delta_events() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "action_output_delta",
            "id": "request-1",
            "actionId": "action-1",
            "stream": "stderr",
            "content": "permission denied",
        }))
        .expect("action_output_delta should deserialize");

        assert_eq!(event.request_id(), Some("request-1"));
        match event {
            SidecarEvent::ActionOutputDelta {
                action_id,
                stream,
                content,
                ..
            } => {
                assert_eq!(action_id, "action-1");
                assert_eq!(stream, "stderr");
                assert_eq!(content, "permission denied");
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_approval_response_result_events() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "approval_response_result",
            "id": "approval-response-1",
            "approvalId": "approval-1",
            "success": false,
            "error": "approval-1 is unknown",
        }))
        .expect("approval_response_result should deserialize");

        assert_eq!(event.request_id(), Some("approval-response-1"));
        match event {
            SidecarEvent::ApprovalResponseResult {
                approval_id,
                success,
                error,
                error_code,
                ..
            } => {
                assert_eq!(approval_id.as_deref(), Some("approval-1"));
                assert!(!success);
                assert_eq!(error.as_deref(), Some("approval-1 is unknown"));
                assert_eq!(error_code, None);
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_approval_expired_events() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "id": "req-1",
            "type": "approval_expired",
            "approvalId": "approval-1",
            "reason": "Claude turn completed before approval was answered."
        }))
        .expect("approval_expired event should deserialize");
        match event {
            SidecarEvent::ApprovalExpired {
                id,
                approval_id,
                reason,
            } => {
                assert_eq!(id.as_deref(), Some("req-1"));
                assert_eq!(approval_id, "approval-1");
                assert_eq!(
                    reason.as_deref(),
                    Some("Claude turn completed before approval was answered.")
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn deserializes_action_progress_events() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "action_progress_updated",
            "id": "request-1",
            "actionId": "action-1",
            "message": "Claude finished preparing tool input.",
        }))
        .expect("action_progress_updated should deserialize");

        match event {
            SidecarEvent::ActionProgressUpdated {
                action_id, message, ..
            } => {
                assert_eq!(action_id, "action-1");
                assert_eq!(message, "Claude finished preparing tool input.");
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_computer_control_tool_calls() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "computer_control_tool_call",
            "id": "request-1",
            "callId": "call-1",
            "toolName": "click",
            "arguments": { "pid": 1234, "x": 10, "y": 20 },
            "threadId": "thread-1",
            "turnId": "turn-1"
        }))
        .expect("computer control tool call should deserialize");

        assert_eq!(event.request_id(), Some("request-1"));
        match event {
            SidecarEvent::ComputerControlToolCall {
                call_id,
                tool_name,
                thread_id,
                turn_id,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "click");
                assert_eq!(thread_id.as_deref(), Some("thread-1"));
                assert_eq!(turn_id.as_deref(), Some("turn-1"));
                assert_eq!(arguments["pid"], serde_json::json!(1234));
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn parses_output_stream_names() {
        assert!(matches!(
            ClaudeSidecarEngine::parse_output_stream("stderr"),
            OutputStream::Stderr
        ));
        assert!(matches!(
            ClaudeSidecarEngine::parse_output_stream("stdout"),
            OutputStream::Stdout
        ));
        assert!(matches!(
            ClaudeSidecarEngine::parse_output_stream("unknown"),
            OutputStream::Stdout
        ));
    }

    #[test]
    fn deserializes_turn_completed_token_usage() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "turn_completed",
            "id": "request-1",
            "status": "completed",
            "sessionId": "session-1",
            "tokenUsage": {
                "input": 42,
                "output": 24,
            },
            "stopReason": "end_turn",
        }))
        .expect("turn_completed should deserialize");

        match event {
            SidecarEvent::TurnCompleted {
                status,
                session_id,
                token_usage,
                stop_reason,
                ..
            } => {
                assert_eq!(status, "completed");
                assert_eq!(session_id.as_deref(), Some("session-1"));
                let usage = token_usage.expect("token usage");
                assert_eq!(usage.input, 42);
                assert_eq!(usage.output, 24);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn deserializes_usage_limits_events() {
        let event: SidecarEvent = serde_json::from_value(serde_json::json!({
            "type": "usage_limits_updated",
            "id": "request-1",
            "usage": {
                "fiveHourPercent": 87,
                "fiveHourResetsAt": 1740000000,
                "fableWeeklyPercent": 40,
                "fableWeeklyResetsAt": 1740100000,
            },
        }))
        .expect("usage_limits_updated should deserialize");

        match event {
            SidecarEvent::UsageLimitsUpdated { usage, .. } => {
                assert_eq!(usage.five_hour_percent, Some(87));
                assert_eq!(usage.five_hour_resets_at, Some(1_740_000_000));
                assert_eq!(usage.fable_weekly_percent, Some(40));
                assert_eq!(usage.fable_weekly_resets_at, Some(1_740_100_000));
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    #[test]
    fn maps_runtime_model_aliases_and_effort_levels() {
        let model = map_claude_model(SidecarModelInfo {
            value: "claude-fable-5[1m]".to_string(),
            display_name: "Fable".to_string(),
            description: "Fable 5".to_string(),
            supports_effort: true,
            supported_effort_levels: vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "xhigh".to_string(),
                "max".to_string(),
            ],
            resolved_model: Some("claude-fable-5".to_string()),
        })
        .expect("runtime model should map");

        assert_eq!(model.id, "claude-fable-5[1m]");
        assert_eq!(model.display_name, "Fable");
        assert_eq!(model.default_reasoning_effort, "high");
        assert_eq!(
            model
                .supported_reasoning_efforts
                .iter()
                .map(|effort| effort.reasoning_effort.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn collapses_default_alias_into_matching_concrete_model() {
        let models = map_claude_models(vec![
            SidecarModelInfo {
                value: "default".to_string(),
                display_name: "Default (recommended)".to_string(),
                description: "Opus 4.8 with 1M context".to_string(),
                supports_effort: true,
                supported_effort_levels: vec!["high".to_string()],
                resolved_model: Some("claude-opus-4-8[1m]".to_string()),
            },
            SidecarModelInfo {
                value: "opus[1m]".to_string(),
                display_name: "Opus".to_string(),
                description: "Opus 4.8 with 1M context".to_string(),
                supports_effort: true,
                supported_effort_levels: vec!["high".to_string()],
                resolved_model: Some("claude-opus-4-8[1m]".to_string()),
            },
            SidecarModelInfo {
                value: "sonnet".to_string(),
                display_name: "Sonnet".to_string(),
                description: "Sonnet 5".to_string(),
                supports_effort: true,
                supported_effort_levels: vec!["medium".to_string()],
                resolved_model: Some("claude-sonnet-5".to_string()),
            },
        ]);

        assert_eq!(
            models
                .iter()
                .map(|model| model.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Opus", "Sonnet"]
        );
        assert!(models[0].is_default);
        assert!(!models[1].is_default);
    }

    #[test]
    fn renames_unresolved_default_alias_to_inferred_family() {
        let model = map_claude_model(SidecarModelInfo {
            value: "default".to_string(),
            display_name: "Default (recommended)".to_string(),
            description: "Opus 4.6, most capable for complex work".to_string(),
            supports_effort: true,
            supported_effort_levels: vec!["high".to_string()],
            resolved_model: None,
        })
        .expect("default model should map");

        assert_eq!(model.id, "default");
        assert_eq!(model.display_name, "Opus");
        assert!(model.is_default);
    }

    #[test]
    fn runtime_models_without_effort_support_do_not_invent_effort_levels() {
        let model = map_claude_model(SidecarModelInfo {
            value: "haiku".to_string(),
            display_name: "Haiku".to_string(),
            description: "Fastest for quick answers".to_string(),
            supports_effort: false,
            supported_effort_levels: vec!["low".to_string()],
            resolved_model: None,
        })
        .expect("runtime model should map");

        assert!(model.default_reasoning_effort.is_empty());
        assert!(model.supported_reasoning_efforts.is_empty());
    }

    #[tokio::test]
    async fn runtime_model_fallback_prefers_cached_catalog() {
        let engine = ClaudeSidecarEngine::default();
        let cached = vec![ModelInfo {
            id: "claude-fable-5[1m]".to_string(),
            display_name: "Fable".to_string(),
            description: "Fable 5".to_string(),
            hidden: false,
            is_default: false,
            upgrade: None,
            availability_nux: None,
            upgrade_info: None,
            input_modalities: vec!["text".to_string(), "image".to_string()],
            attachment_modalities: vec!["text".to_string(), "image".to_string()],
            limits: None,
            supports_personality: false,
            default_reasoning_effort: "high".to_string(),
            supported_reasoning_efforts: Vec::new(),
        }];
        engine.state.lock().await.runtime_model_cache = Some(cached.clone());

        assert_eq!(
            engine
                .runtime_model_fallback()
                .await
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            cached.into_iter().map(|model| model.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn fallback_catalog_preserves_legacy_thread_model_ids() {
        let models = ClaudeSidecarEngine::default().models();
        let default_model = models
            .iter()
            .find(|model| model.id == "default")
            .expect("fallback catalog should retain the runtime default");

        assert!(default_model.is_default);
        assert!(!default_model.hidden);

        for legacy_id in [
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
        ] {
            let legacy_model = models
                .iter()
                .find(|model| model.id == legacy_id)
                .unwrap_or_else(|| panic!("fallback catalog should retain {legacy_id}"));
            assert!(legacy_model.hidden);
            assert!(!legacy_model.is_default);
        }
    }

    #[test]
    fn discovered_legacy_model_is_not_duplicated_or_hidden() {
        let discovered = claude_model_info(
            "claude-sonnet-4-6",
            "Sonnet",
            "Discovered from the active runtime",
            false,
            true,
            "medium",
            &["low", "medium", "high"],
        );
        let models = with_legacy_claude_models(vec![discovered]);
        let matching_models = models
            .iter()
            .filter(|model| model.id == "claude-sonnet-4-6")
            .collect::<Vec<_>>();

        assert_eq!(matching_models.len(), 1);
        assert!(!matching_models[0].hidden);
        assert!(matching_models[0].is_default);
    }

    #[test]
    fn claude_auth_error_detection_uses_structured_fields() {
        assert!(ClaudeSidecarEngine::is_claude_auth_error(
            "anything",
            Some("authentication_failed"),
            false,
        ));
        assert!(ClaudeSidecarEngine::is_claude_auth_error(
            "Claude authentication failed. Sign in again.",
            None,
            false,
        ));
        assert!(!ClaudeSidecarEngine::is_claude_auth_error(
            "Claude rate limit reached",
            Some("rate_limit"),
            false,
        ));
    }

    #[test]
    fn node_health_checks_use_windows_commands() {
        let checks = node_health_checks_for_platform("windows");

        assert!(checks.contains(&"where node".to_string()));
        assert!(checks.contains(&"echo %PATH%".to_string()));
        assert!(!checks.iter().any(|check| check == "command -v node"));
    }

    #[test]
    fn node_resolution_falls_back_from_incompatible_app_path_to_login_shell() {
        let resolution = resolve_node_candidates(
            Some("/usr/local/bin:/usr/bin".to_string()),
            Some(NodeExecutableCandidate {
                path: PathBuf::from("/usr/local/bin/node"),
                version: Some("16.15.1".to_string()),
                compatible: false,
            }),
            Some(NodeExecutableCandidate {
                path: PathBuf::from("/Users/test/.fnm/node"),
                version: Some("25.6.1".to_string()),
                compatible: true,
            }),
        );

        assert_eq!(
            resolution.executable,
            Some(PathBuf::from("/Users/test/.fnm/node"))
        );
        assert_eq!(resolution.source, "login-shell");
        assert_eq!(resolution.rejected_executables.len(), 1);
        assert_eq!(
            resolution.rejected_executables[0].version.as_deref(),
            Some("16.15.1")
        );
    }

    #[test]
    fn node_runtime_rejects_versions_before_disposable_child_process_support() {
        for version in ["20.0.0", "20.4.0", "18.20.0"] {
            assert!(!node_runtime_is_compatible(&NodeRuntimeProbe {
                version: version.to_string(),
                explicit_resource_management: true,
                disposable_child_process: true,
            }));
        }
    }

    #[test]
    fn node_runtime_requires_a_disposable_child_process() {
        assert!(!node_runtime_is_compatible(&NodeRuntimeProbe {
            version: "20.5.0".to_string(),
            explicit_resource_management: true,
            disposable_child_process: false,
        }));
        assert!(node_runtime_is_compatible(&NodeRuntimeProbe {
            version: "20.5.0".to_string(),
            explicit_resource_management: true,
            disposable_child_process: true,
        }));
        assert!(node_runtime_is_compatible(&NodeRuntimeProbe {
            version: "25.6.1".to_string(),
            explicit_resource_management: true,
            disposable_child_process: true,
        }));
    }

    #[test]
    fn incompatible_node_runtime_is_reported_as_unavailable() {
        let resolution = resolve_node_candidates(
            Some("/usr/local/bin:/usr/bin".to_string()),
            Some(NodeExecutableCandidate {
                path: PathBuf::from("/usr/local/bin/node"),
                version: Some("16.15.1".to_string()),
                compatible: false,
            }),
            None,
        );

        assert!(resolution.executable.is_none());
        let details = node_unavailable_details_for_platform("macos", &resolution);
        assert!(details.contains("Node.js 20.5+"));
        assert!(details.contains("Node.js 16.15.1"));
        assert!(details.contains("/usr/local/bin/node"));
    }

    #[test]
    fn node_unavailable_details_for_windows_mentions_path_guidance() {
        let details = node_unavailable_details_for_platform(
            "windows",
            &NodeExecutableResolution {
                executable: None,
                source: "unavailable",
                app_path: Some(r"C:\Windows\System32".to_string()),
                rejected_executables: Vec::new(),
            },
        );

        assert!(details.contains("install directory is in PATH"));
        assert!(details.contains("App PATH"));
    }

    #[test]
    fn node_fix_commands_for_windows_cover_where_and_restart() {
        let fixes = node_fix_commands_for_platform(
            "windows",
            &NodeExecutableResolution {
                executable: None,
                source: "unavailable",
                app_path: Some(r"C:\Windows\System32".to_string()),
                rejected_executables: Vec::new(),
            },
        );

        assert!(fixes.contains(&"where node".to_string()));
        assert!(fixes.contains(&"echo %PATH%".to_string()));
        assert!(fixes.iter().any(|fix| fix.contains("restart AuraCoder")));
    }

    /// 验证 Windows Node 修复命令和用户说明采用不同诊断动作类型。
    #[test]
    fn windows_node_fix_actions_distinguish_commands_and_instructions() {
        let resolution = NodeExecutableResolution {
            executable: None,
            source: "unavailable",
            app_path: Some(r"C:\Windows\System32".to_string()),
            rejected_executables: Vec::new(),
        };
        let actions = node_fix_actions_for_platform("windows", &resolution);
        let labels = actions
            .iter()
            .map(|action| action.label().to_string())
            .collect::<Vec<_>>();

        assert_eq!(actions.len(), 3);
        assert!(matches!(&actions[0], EngineCheckAction::Shell(_)));
        assert!(matches!(&actions[1], EngineCheckAction::Shell(_)));
        assert!(matches!(&actions[2], EngineCheckAction::Information(_)));
        assert_eq!(labels, node_fix_commands_for_platform("windows", &resolution));
    }
