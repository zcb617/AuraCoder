
    use std::collections::HashMap;

    use super::{
        build_completed_message_event, decode_relay_header_value, enforce_message_page_budget,
        mobile_message_value, parse_optional_batch_id, parse_remote_attachment_inputs,
        relay_attachment_url, remote_batch_key, resolve_remote_runtime_update,
        truncate_mobile_text, RemoteBatchState, RemoteTunnelManager,
    };
    use crate::config::app_config::RemoteAccessConfig;
    use crate::models::{
        chat::ChatAttachmentPayload,
        EngineCapabilitiesDto, EngineInfoDto, EngineModelDto, MessageDto, MessageStatusDto,
        MessageWindowCursorDto, ThreadDto, ThreadStatusDto,
    };
    use serde_json::{json, Value};
    use tokio::fs as tokio_fs;

    fn test_thread(message_count: i64, engine_thread_id: Option<&str>) -> ThreadDto {
        ThreadDto {
            id: "thread-1".to_string(),
            workspace_id: "ws-1".to_string(),
            engine_id: "codex".to_string(),
            model_id: "gpt-5.4".to_string(),
            engine_thread_id: engine_thread_id.map(str::to_string),
            engine_metadata: None,
            plan_mode: None,
            send_method: None,
            reasoning_effort: Some("high".to_string()),
            permission_mode: None,
            title: "新会话".to_string(),
            status: ThreadStatusDto::Idle,
            message_count,
            total_tokens: 0,
            context_current_tokens: None,
            context_max_tokens: None,
            context_usage_updated_at: None,
            created_at: "2026-08-27T00:00:00Z".to_string(),
            last_activity_at: "2026-08-27T00:00:00Z".to_string(),
        }
    }

    fn test_engine(id: &str, models: Vec<EngineModelDto>) -> EngineInfoDto {
        EngineInfoDto {
            id: id.to_string(),
            name: id.to_string(),
            models,
            capabilities: EngineCapabilitiesDto {
                permission_modes: vec![],
                sandbox_modes: vec![],
                approval_decisions: vec![],
            },
        }
    }

    fn test_model(id: &str, is_default: bool, hidden: bool) -> EngineModelDto {
        EngineModelDto {
            id: id.to_string(),
            display_name: id.to_string(),
            description: String::new(),
            hidden,
            is_default,
            upgrade: None,
            availability_nux: None,
            upgrade_info: None,
            input_modalities: vec![],
            attachment_modalities: vec![],
            limits: None,
            supports_personality: false,
            default_reasoning_effort: "medium".to_string(),
            supported_reasoning_efforts: vec![],
        }
    }

    #[test]
    fn resolve_remote_runtime_update_allows_unstarted_thread_and_defaults_model() {
        let thread = test_thread(0, None);
        let engines = vec![test_engine(
            "claude",
            vec![
                test_model("claude-a", false, false),
                test_model("claude-b", true, false),
            ],
        )];
        let result = resolve_remote_runtime_update(&thread, &engines, "claude", None, None)
            .expect("unstarted thread must allow engine switch");
        assert_eq!(result.0, "claude");
        assert_eq!(result.1, "claude-b");
        assert_eq!(result.2.as_deref(), Some("medium"));
    }

    #[test]
    fn resolve_remote_runtime_update_rejects_started_thread_with_messages() {
        let thread = test_thread(1, None);
        let engines = vec![test_engine(
            "claude",
            vec![test_model("claude-b", true, false)],
        )];
        let error = resolve_remote_runtime_update(&thread, &engines, "claude", None, None)
            .expect_err("started thread must reject engine switch");
        assert!(
            error.contains("already started"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_remote_runtime_update_rejects_started_thread_with_engine_session() {
        let thread = test_thread(0, Some("cli-session-1"));
        let engines = vec![test_engine(
            "claude",
            vec![test_model("claude-b", true, false)],
        )];
        let error = resolve_remote_runtime_update(&thread, &engines, "claude", None, None)
            .expect_err("thread with engine session must reject engine switch");
        assert!(
            error.contains("already started"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_remote_runtime_update_rejects_unknown_engine() {
        let thread = test_thread(0, None);
        let engines = vec![test_engine(
            "claude",
            vec![test_model("claude-b", true, false)],
        )];
        let error = resolve_remote_runtime_update(&thread, &engines, "unknown", None, None)
            .expect_err("unknown engine must be rejected");
        assert!(
            error.contains("unknown engine_id"),
            "unexpected error: {error}"
        );
    }

    // #[test]
//     fn remote_identity_is_generated_and_rotates() {
//         let mut config = RemoteAccessConfig::default();
//         assert!(config.ensure_identity());
//         assert!(config.tunnel_id.starts_with("auracoder_"));
//         assert!(config.credential.len() >= 64);
//         let first_id = config.tunnel_id.clone();
//         assert!(!config.ensure_identity());
//         config.device_credential = "paired-device".to_string();
//         config.regenerate_identity();
//         assert_ne!(config.tunnel_id, first_id);
//         assert!(config.device_credential.is_empty());
//     }


    #[test]
    fn completed_message_event_targets_device_and_preserves_exact_message() {
        let payload = json!({
            "threadId": "thread-1",
            "messageId": "message-1",
            "message": {
                "id": "message-1",
                "threadId": "thread-1",
                "role": "assistant",
                "status": "completed",
                "content": "done",
            },
        });

        let event = build_completed_message_event("mobile-1", &payload)
            .expect("valid completed message payload");
        assert_eq!(event["event"], "thread.message.completed");
        assert_eq!(event["targetDeviceId"], "mobile-1");
        assert_eq!(event["payload"]["threadId"], "thread-1");
        assert_eq!(event["payload"]["messageId"], "message-1");
        assert_eq!(event["payload"]["message"]["id"], "message-1");
    }

    #[test]
    fn completed_message_event_rejects_missing_exact_ids() {
        let payload = json!({
            "threadId": "thread-1",
            "message": { "id": "message-1" },
        });
        assert!(build_completed_message_event("mobile-1", &payload).is_none());
    }

    #[tokio::test]
    async fn batch_attachment_validation_requires_exact_current_device_set() {
        let manager = RemoteTunnelManager::default();
        let device_id = "device-a";
        let batch_id = "batch-a";
        let attachment = ChatAttachmentPayload {
            file_name: "a.txt".to_string(),
            file_path: "C:/batch-a/a.txt".to_string(),
            size_bytes: 1,
            mime_type: Some("text/plain".to_string()),
            browser_annotation: None,
        };
        manager.batches.lock().await.insert(
            remote_batch_key(device_id, batch_id),
            RemoteBatchState {
                device_id: device_id.to_string(),
                batch_id: batch_id.to_string(),
                files: HashMap::from([(attachment.file_path.clone(), attachment.clone())]),
                last_activity: std::time::Instant::now(),
            },
        );
        assert!(manager
            .validate_batch_attachments(device_id, batch_id, std::slice::from_ref(&attachment))
            .await
            .is_ok());
        assert!(manager
            .validate_batch_attachments("other-device", batch_id, std::slice::from_ref(&attachment))
            .await
            .is_err());
        let duplicate = vec![attachment.clone(), attachment];
        assert!(manager
            .validate_batch_attachments(device_id, batch_id, &duplicate)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn abort_batch_removes_only_unsubmitted_batch_files() {
        let manager = RemoteTunnelManager::default();
        let device_id = "device-abort";
        let batch_id = "batch-abort";
        let file_path =
            std::env::temp_dir().join(format!("auracoder-remote-test-{}", uuid::Uuid::new_v4()));
        tokio_fs::write(&file_path, b"temporary")
            .await
            .expect("create test temporary attachment");
        let file_path_string = file_path.display().to_string();
        manager.batches.lock().await.insert(
            remote_batch_key(device_id, batch_id),
            RemoteBatchState {
                device_id: device_id.to_string(),
                batch_id: batch_id.to_string(),
                files: HashMap::from([(
                    file_path_string,
                    ChatAttachmentPayload {
                        file_name: "temporary.txt".to_string(),
                        file_path: file_path.display().to_string(),
                        size_bytes: 9,
                        mime_type: Some("text/plain".to_string()),
                        browser_annotation: None,
                    },
                )]),
                last_activity: std::time::Instant::now(),
            },
        );
        manager.abort_batch(device_id, batch_id).await;
        assert!(!file_path.exists());
        assert!(manager.batches.lock().await.is_empty());
    }

    #[test]
    fn missing_batch_id_keeps_legacy_client_path_available() {
        assert_eq!(
            parse_optional_batch_id(&json!({"message": "legacy"})),
            Ok(None)
        );
    }

    #[test]
    fn relay_attachment_url_converts_same_origin_and_encodes_query() {
        let url = relay_attachment_url(
            "wss://relay.example.test/ws/tunnel",
            "tunnel id",
            "key/with slash",
        )
        .expect("valid relay endpoint");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.path(), "/api/mobile/attachments/key%2Fwith%20slash");
        assert_eq!(url.query(), Some("tunnel_id=tunnel+id"));
    }

    #[test]
    fn relay_file_name_header_requires_valid_percent_encoding() {
        assert_eq!(
            decode_relay_header_value("report%20%E6%B5%8B%E8%AF%95.txt").unwrap(),
            "report 测试.txt"
        );
        assert!(decode_relay_header_value("broken%ZZ.txt").is_err());
    }

    #[test]
    fn message_attachment_inputs_accept_mobile_snake_case_attachment_keys() {
        let payload = json!({
            "attachments": [{
                "attachment_key": "K-1",
                "file_name": "report.txt",
                "size_bytes": 4,
                "mime_type": "text/plain"
            }]
        });
        let inputs = parse_remote_attachment_inputs(&payload)
            .expect("valid attachment input")
            .expect("attachment list");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].attachment_key.as_deref(), Some("K-1"));
        assert_eq!(inputs[0].size_bytes, Some(4));
    }

    #[test]
    fn mobile_message_value_strips_action_output_and_keeps_summary() {
        let unified_diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n+new\n+added\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-kept\n+changed\n";
        let blocks = json!([
            {
                "type": "action",
                "actionId": "action-1",
                "actionType": "run",
                "summary": "keep this summary",
                "details": { "command": "npm test" },
                "outputChunks": [{ "stream": "stdout", "content": "chunk" }],
                "result": { "output": "x".repeat(10_000), "diff": unified_diff, "error": null }
            },
            { "type": "diff", "diff": unified_diff, "scope": "turn" },
            {
                "type": "attachment",
                "fileName": "report.txt",
                "mimeType": "text/plain",
                "sizeBytes": 123,
                "filePath": "C:/secret/report.txt",
                "previewFilePath": "C:/secret/message-images/report.txt"
            }
        ]);
        let message = MessageDto {
            id: "message-1".to_string(),
            thread_id: "thread-1".to_string(),
            role: "assistant".to_string(),
            content: Some("answer".to_string()),
            blocks: Some(blocks),
            turn_engine_id: None,
            turn_model_id: None,
            turn_reasoning_effort: None,
            schema_version: 1,
            status: MessageStatusDto::Completed,
            token_usage: None,
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
        };

        let value = mobile_message_value(message).expect("convert message to mobile value");
        let block_values = value.get("blocks").and_then(Value::as_array).unwrap();
        let action = block_values
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("action"))
            .expect("action block should remain");
        assert!(action.get("outputChunks").is_none());
        assert!(action.get("details").is_none());
        assert_eq!(
            action.get("summary").and_then(Value::as_str),
            Some("keep this summary")
        );
        let result = action.get("result").and_then(Value::as_object).unwrap();
        let output = result.get("output").and_then(Value::as_str).unwrap();
        assert!(output.ends_with(super::MOBILE_TRUNCATED_SUFFIX));
        assert_eq!(
            output.chars().count(),
            super::MOBILE_HIDDEN_OUTPUT_MAX_CHARS + super::MOBILE_TRUNCATED_SUFFIX.chars().count()
        );
        let action_summary = result
            .get("mobileSummary")
            .and_then(Value::as_object)
            .expect("action Diff summary should remain");
        assert_eq!(action_summary.get("fileCount").and_then(Value::as_u64), Some(2));
        assert_eq!(action_summary.get("additions").and_then(Value::as_u64), Some(3));
        assert_eq!(action_summary.get("deletions").and_then(Value::as_u64), Some(2));

        let diff_block = block_values
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("diff"))
            .expect("diff block should remain");
        let diff_summary = diff_block
            .get("mobileSummary")
            .and_then(Value::as_object)
            .expect("top-level Diff summary should remain");
        assert_eq!(diff_summary.get("fileCount").and_then(Value::as_u64), Some(2));
        assert_eq!(diff_summary.get("additions").and_then(Value::as_u64), Some(3));
        assert_eq!(diff_summary.get("deletions").and_then(Value::as_u64), Some(2));

        let attachments = value.get("attachments").and_then(Value::as_array).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            attachments[0].get("filePath").and_then(Value::as_str),
            Some("")
        );
        let attachment_block = block_values
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("attachment"))
            .expect("attachment block should remain");
        assert_eq!(
            attachment_block.get("filePath").and_then(Value::as_str),
            Some("")
        );
        assert!(attachment_block.get("previewFilePath").is_none());
    }

    #[test]
    fn truncate_mobile_text_appends_suffix_only_when_needed() {
        assert_eq!(truncate_mobile_text("", 10), "");
        assert_eq!(truncate_mobile_text("short", 10), "short");
        let long = "x".repeat(100);
        let truncated = truncate_mobile_text(&long, 10);
        assert!(truncated.ends_with(super::MOBILE_TRUNCATED_SUFFIX));
        assert_eq!(
            truncated.chars().count(),
            10 + super::MOBILE_TRUNCATED_SUFFIX.chars().count()
        );
    }

    #[test]
    fn enforce_message_page_budget_drops_oldest_and_recomputes_cursor() {
        let messages: Vec<Value> = (1..=5)
            .map(|index| {
                json!({
                    "id": format!("m{index}"),
                    "createdAt": "2026-08-01T00:00:00.000Z",
                    "content": "x".repeat(100),
                })
            })
            .collect();
        let row_ids = vec![1, 2, 3, 4, 5];
        let next_cursor = Some(MessageWindowCursorDto {
            created_at: "2026-08-01T00:00:00.000Z".to_string(),
            id: "m1".to_string(),
            row_id: Some(1),
        });
        // 每条约 100 字节；预算只容纳 2 条，最旧的 m1/m2/m3 被丢弃。
        let single_size = messages[0].to_string().len();
        let budget = single_size * 2;
        let (kept, recomputed) = enforce_message_page_budget(
            messages.clone(),
            row_ids.clone(),
            next_cursor.clone(),
            budget,
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].get("id").and_then(Value::as_str), Some("m4"));
        assert_eq!(kept[1].get("id").and_then(Value::as_str), Some("m5"));
        let recomputed = recomputed.expect("cursor should be recomputed after trimming");
        assert_eq!(recomputed.id, "m4");
        assert_eq!(recomputed.row_id, Some(4));

        // 未触发裁剪时 next_cursor 原样返回。
        let (kept_all, untouched) = enforce_message_page_budget(
            messages.clone(),
            row_ids,
            next_cursor.clone(),
            usize::MAX,
        );
        assert_eq!(kept_all.len(), 5);
        let untouched = untouched.expect("cursor should be untouched without trimming");
        assert_eq!(untouched.id, "m1");
        assert_eq!(untouched.row_id, Some(1));
    }

    #[test]
    fn enforce_message_page_budget_replaces_single_oversize_message() {
        let messages = vec![json!({
            "id": "m-oversize",
            "createdAt": "2026-08-01T00:00:00.000Z",
            "content": "x".repeat(super::MESSAGE_SINGLE_MESSAGE_BUDGET_BYTES + 1024),
        })];
        let (kept, next_cursor) = enforce_message_page_budget(
            messages,
            vec![1],
            None,
            super::MESSAGE_PAGE_BUDGET_BYTES,
        );
        assert_eq!(kept.len(), 1);
        assert!(next_cursor.is_none());
        let blocks = kept[0].get("blocks").and_then(Value::as_array).unwrap();
        assert_eq!(
            blocks[0].get("content").and_then(Value::as_str),
            Some("该消息内容过大，请在桌面端查看。")
        );
    }
