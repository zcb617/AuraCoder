use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use anyhow::Context as _;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use super::*;
use crate::{
    cli_tools::{
        codex::CodexCli,
        mcp::chat_business::*,
        CliTool,
    },
    config::app_config::AppConfig,
    db,
    engines::{
        ActionResult, ApprovalRequestRoute, BrowserAnnotationMetadata, EngineEvent, EngineManager,
        OutputStream, TurnAttachment, TurnCompletionStatus, TurnInputItem,
    },
    git::{repo::FileTreeCache, watcher::GitWatcherManager},
    models::{
        EngineCapabilitiesDto, EngineInfoDto, EngineModelDto, MessageStatusDto,
        ReasoningEffortOptionDto, SteerReceiptDto, ThreadStatusDto,
    },
    power::KeepAwakeManager,
    services::{
        chat_service::{
            browser_annotation_context, normalize_browser_annotation, normalize_input_items,
            persist_message_image_previews, read_attachment_preview,
            save_pasted_image_attachment,
        },
        thread_service::ThreadService,
    },
    state::{AppState, TurnManager},
    terminal::TerminalManager,
    terminal_notifications::TerminalNotificationManager,
};
use rusqlite::params;
use uuid::Uuid;

/// 图片预览最长边上限，验证大图预览不会超过服务的编码尺寸。
const ATTACHMENT_PREVIEW_MAX_DIMENSION: u32 = 1600;
/// 图片预览编码字节上限，验证生成结果不会超过服务的负载限制。
const ATTACHMENT_PREVIEW_MAX_BYTES: usize = 700 * 1024;

    /// 验证追加消息回执使用前端约定字段名序列化。
    #[test]
    fn steer_receipt_serializes_with_frontend_field_names() {
        let receipt = SteerReceiptDto {
            client_steer_id: "steer-1".to_string(),
            expected_turn_id: "turn-1".to_string(),
            accepted_at: "2026-08-11T15:26:57.608Z".to_string(),
        };

        assert_eq!(
            serde_json::to_value(receipt).expect("steer receipt should serialize"),
            serde_json::json!({
                "clientSteerId": "steer-1",
                "expectedTurnId": "turn-1",
                "acceptedAt": "2026-08-11T15:26:57.608Z",
            })
        );
    }

    /// 构造绑定临时数据库和聊天服务依赖的测试应用状态。
    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-chat-cmd-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp root");
        let db = crate::db::Database::open(root.join("workspaces.db"))
            .expect("failed to create test database");
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
            scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
            computer_control_service: Arc::new(
                crate::computer_control_service::ComputerControlService::default(),
            ),
            auracoder_thread_mcp_service: Arc::new(
                crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()),
            ),
            mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    /// 在测试数据库中创建指定 CLI 与模型的聊天线程。
    fn test_thread(state: &AppState, engine_id: &str, model_id: &str) -> ThreadDto {
        let workspace_root =
            std::env::temp_dir().join(format!("auracoder-chat-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&workspace_root).expect("failed to create workspace root");
        let workspace =
            db::workspaces::upsert_workspace(&state.db, workspace_root.to_string_lossy().as_ref())
                .expect("failed to create workspace");
        db::threads::create_thread(&state.db, &workspace.id, engine_id, model_id, "Thread")
            .expect("failed to create thread")
    }

    /// 构造模型附件能力目录供附件校验场景使用。
    fn attachment_validation_catalog(attachment_modalities: Vec<&str>) -> Vec<EngineInfoDto> {
        vec![EngineInfoDto {
            id: "opencode".to_string(),
            name: "OpenCode".to_string(),
            models: vec![EngineModelDto {
                id: "opencode/test".to_string(),
                display_name: "OpenCode Test".to_string(),
                description: String::new(),
                hidden: false,
                is_default: true,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string(), "image".to_string(), "pdf".to_string()],
                attachment_modalities: attachment_modalities
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect(),
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: Vec::new(),
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }]
    }

    /// 构造指定文件名和 MIME 类型的聊天附件。
    fn test_attachment(file_name: &str, mime_type: Option<&str>) -> TurnAttachment {
        TurnAttachment {
            file_name: file_name.to_string(),
            file_path: format!("/tmp/{file_name}"),
            preview_file_path: None,
            size_bytes: 1,
            mime_type: mime_type.map(ToOwned::to_owned),
            browser_annotation: None,
            is_remote: false,
            remote_text_content: None,
        }
    }

    /// 验证浏览器截图标注评论作为上下文传入但不混入用户可见消息文本。
    #[test]
    fn browser_annotation_comment_is_hidden_from_visible_message_text() {
        let attachment = TurnAttachment {
            file_name: "browser-annotation.png".to_string(),
            file_path: "/tmp/browser-annotation.png".to_string(),
            preview_file_path: None,
            size_bytes: 1,
            mime_type: Some("image/png".to_string()),
            browser_annotation: Some(BrowserAnnotationMetadata {
                comment: "文字太小了".to_string(),
                number: Some(1),
                source_url: Some("https://www.jxrjkf.cn/".to_string()),
                target_label: Some("div: 嘉兴市 雨 25℃".to_string()),
            }),
            is_remote: false,
            remote_text_content: None,
        };

        let blocks = build_user_blocks(
            "请优化这个页面",
            &[TurnInputItem::Text {
                text: "请优化这个页面".to_string(),
            }],
            &[attachment.clone()],
            false,
            false,
            None,
        );
        let visible_text = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(visible_text, "请优化这个页面");
        assert!(!visible_text.contains("文字太小了"));
        assert!(browser_annotation_context(&[attachment]).contains("文字太小了"));
    }

    /// 验证缺少有效评论的浏览器标注元数据会被拒绝。
    #[test]
    fn browser_annotation_metadata_requires_a_comment() {
        let annotation = BrowserAnnotationMetadata {
            comment: "   ".to_string(),
            number: Some(1),
            source_url: Some("https://www.jxrjkf.cn/".to_string()),
            target_label: Some("div: 嘉兴市 雨 25℃".to_string()),
        };

        assert!(normalize_browser_annotation(Some(annotation))
            .expect("empty annotation comment should not fail")
            .is_none());
    }

    /// 验证附件类型在模型能力范围内时可以通过发送校验。
    #[test]
    fn validates_attachments_against_model_attachment_modalities() {
        let catalog = attachment_validation_catalog(vec!["text", "image"]);
        let attachments = vec![
            test_attachment("notes.md", Some("text/markdown")),
            test_attachment("screenshot.png", Some("image/png")),
        ];

        assert!(validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .is_ok());
    }

    /// 验证模型不支持附件时聊天发送拒绝文件。
    #[test]
    fn rejects_attachments_when_model_disables_files() {
        let catalog = attachment_validation_catalog(Vec::new());
        let attachments = vec![test_attachment("notes.md", Some("text/markdown"))];

        let error = validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .expect_err("model without attachment modalities should reject files");

        assert!(error.contains("does not support file attachments"));
    }

    /// 验证模型不支持的附件模态会被拒绝。
    #[test]
    fn rejects_attachment_modalities_not_allowed_by_model() {
        let catalog = attachment_validation_catalog(vec!["text"]);
        let attachments = vec![test_attachment("diagram.png", Some("image/png"))];

        let error = validate_attachments_for_engine_model(
            &attachments,
            "opencode",
            "opencode/test",
            Some(&catalog),
        )
        .expect_err("image should be blocked for text-only models");

        assert!(error.contains("Image attachments are not supported"));
    }

    /// 在测试数据库中插入带有原生路由详情的待处理审批。
    fn insert_pending_approval_with_details(
        state: &AppState,
        thread: &ThreadDto,
        approval_id: &str,
        details: Value,
    ) -> String {
        let assistant_message = db::messages::insert_assistant_placeholder(
            &state.db,
            &thread.id,
            Some(thread.engine_id.as_str()),
            Some(thread.model_id.as_str()),
            None,
        )
        .expect("failed to create assistant message");
        db::actions::insert_approval(
            &state.db,
            approval_id,
            &thread.id,
            &assistant_message.id,
            &crate::engines::events::ActionType::Command,
            "Run command",
            &details,
        )
        .expect("failed to insert approval");
        db::threads::update_thread_status(&state.db, &thread.id, ThreadStatusDto::AwaitingApproval)
            .expect("failed to set thread status");

        let blocks = serde_json::json!([
            {
                "type": "approval",
                "approvalId": approval_id,
                "actionType": "command",
                "summary": "Run command",
                "details": details,
                "status": "pending"
            }
        ]);
        let conn = state.db.connect().expect("failed to open db connection");
        conn.execute(
            "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
            params![blocks.to_string(), assistant_message.id],
        )
        .expect("failed to persist approval block");
        assistant_message.id
    }

    /// 在测试数据库中插入待处理审批及其消息卡片。
    fn insert_pending_approval(state: &AppState, thread: &ThreadDto, approval_id: &str) -> String {
        insert_pending_approval_with_details(
            state,
            thread,
            approval_id,
            serde_json::json!({ "command": "touch file.txt" }),
        )
    }

    /// 验证线程存在时最终事件携带最新线程数据。
    #[test]
    fn build_final_thread_event_uses_latest_thread_when_present() {
        let state = test_app_state();
        let fallback_thread = test_thread(&state, "codex", "gpt-5.5-codex");
        let mut latest_thread = fallback_thread.clone();
        latest_thread.title = "Renamed".to_string();

        let (event, final_thread) =
            build_final_thread_event(Some(latest_thread.clone()), &fallback_thread);

        assert_eq!(event.thread_id, latest_thread.id);
        assert_eq!(event.workspace_id, latest_thread.workspace_id);
        assert_eq!(
            event.thread.as_ref().map(|thread| thread.title.as_str()),
            Some("Renamed")
        );
        assert_eq!(
            final_thread.as_ref().map(|thread| thread.title.as_str()),
            Some("Renamed")
        );
    }

    /// 验证线程已删除时最终事件通知前端移除线程。
    #[test]
    fn build_final_thread_event_emits_removal_when_thread_is_missing() {
        let state = test_app_state();
        let fallback_thread = test_thread(&state, "codex", "gpt-5.5-codex");

        let (event, final_thread) = build_final_thread_event(None, &fallback_thread);

        assert_eq!(event.thread_id, fallback_thread.id);
        assert_eq!(event.workspace_id, fallback_thread.workspace_id);
        assert!(event.thread.is_none());
        assert!(final_thread.is_none());
    }

    /// 验证输入归一化合并相邻文本并保留技能和提及项。
    #[test]
    fn normalize_input_items_merges_adjacent_text_and_preserves_typed_items() {
        let normalized = normalize_input_items(
            "fallback",
            Some(vec![
                ChatInputItemPayload::Text {
                    text: "Use ".to_string(),
                },
                ChatInputItemPayload::Text {
                    text: "$lint".to_string(),
                },
                ChatInputItemPayload::Skill {
                    name: "lint".to_string(),
                    path: "/tmp/skills/lint".to_string(),
                },
                ChatInputItemPayload::Mention {
                    name: "Docs".to_string(),
                    path: "app://docs".to_string(),
                },
                ChatInputItemPayload::Text {
                    text: " now".to_string(),
                },
            ]),
        )
        .expect("input items should normalize");

        assert_eq!(
            normalized,
            vec![
                TurnInputItem::Text {
                    text: "Use $lint".to_string(),
                },
                TurnInputItem::Skill {
                    name: "lint".to_string(),
                    path: "/tmp/skills/lint".to_string(),
                },
                TurnInputItem::Mention {
                    name: "Docs".to_string(),
                    path: "app://docs".to_string(),
                },
                TurnInputItem::Text {
                    text: " now".to_string(),
                },
            ]
        );
    }

    /// 验证通知摘要忽略追加消息块并选取首个助手文本块。
    #[test]
    fn chat_notification_preview_ignores_steer_blocks_and_uses_first_text_block() {
        let preview = chat_notification_preview(&[
            ContentBlock::Text {
                content: "hidden steer".to_string(),
                plan_mode: None,
                is_steer: Some(true),
                client_steer_id: Some("steer-preview".to_string()),
            },
            ContentBlock::Text {
                content: "  First line\n\nSecond line  ".to_string(),
                plan_mode: None,
                is_steer: None,
                client_steer_id: None,
            },
        ]);

        assert_eq!(preview.as_deref(), Some("First line Second line"));
    }

    /// 验证没有助手正文时通知摘要使用错误块文本。
    #[test]
    fn chat_notification_preview_falls_back_to_error_blocks() {
        let preview = chat_notification_preview(&[ContentBlock::Error {
            message: "Command failed".to_string(),
        }]);

        assert_eq!(preview.as_deref(), Some("Command failed"));
    }

    /// 验证技能或提及字段为空时输入项归一化会拒绝请求。
    #[test]
    fn normalize_input_items_rejects_blank_typed_entries() {
        let error = normalize_input_items(
            "fallback",
            Some(vec![ChatInputItemPayload::Skill {
                name: " ".to_string(),
                path: "/tmp/skills/lint".to_string(),
            }]),
        )
        .expect_err("blank skill names should be rejected");

        assert!(error.contains("skill input items require non-empty name and path"));
    }

    /// 验证 Codex 提交审查目标转换为原生请求、消息和标题。
    #[test]
    fn normalize_codex_review_target_builds_commit_payload() {
        let (target, message, title) =
            CodexCli::normalize_codex_review_target(&CodexReviewTargetPayload::Commit {
                sha: "abcdef1234567890".to_string(),
                title: Some("Refactor auth flow".to_string()),
            })
            .expect("commit target should normalize");

        assert_eq!(
            target,
            serde_json::json!({
                "type": "commit",
                "sha": "abcdef1234567890",
                "title": "Refactor auth flow",
            })
        );
        assert_eq!(
            message,
            "Review commit `abcdef1234567890`: Refactor auth flow"
        );
        assert_eq!(title, "Review: abcdef123456");
    }

    /// 验证独立审查线程复制元数据时清除运行时专属字段。
    #[test]
    fn clone_codex_review_metadata_clears_runtime_only_fields() {
        let metadata = CodexCli::clone_codex_review_metadata(
            Some(&serde_json::json!({
                "manualTitle": true,
                "manualTitleUpdatedAt": "2026-03-12T00:00:00Z",
                "codexPreview": "old preview",
                "codexThreadStatus": "active",
                "codexThreadActiveFlags": ["waitingOnApproval"],
                "codexSyncRequired": true,
                "codexSyncReason": "stale",
                "serviceTier": "fast",
            })),
            "gpt-5.4",
        )
        .expect("metadata should clone");

        assert_eq!(metadata.get("manualTitle"), None);
        assert_eq!(metadata.get("codexPreview"), None);
        assert_eq!(metadata.get("codexThreadStatus"), None);
        assert_eq!(metadata.get("codexThreadActiveFlags"), None);
        assert_eq!(metadata.get("codexSyncRequired"), None);
        assert_eq!(metadata.get("codexSyncReason"), None);
        assert_eq!(
            metadata.get("serviceTier"),
            Some(&serde_json::json!("fast"))
        );
        assert_eq!(metadata.get("lastModelId"), None);
    }

    /// 验证消息正文非空但结构化输入没有文本时拒绝请求。
    #[test]
    fn normalize_input_items_rejects_non_empty_message_without_text_segments() {
        let error = normalize_input_items(
            "Use lint",
            Some(vec![ChatInputItemPayload::Skill {
                name: "lint".to_string(),
                path: "/tmp/skills/lint".to_string(),
            }]),
        )
        .expect_err("non-empty message text requires a text segment");

        assert!(error.contains("input items must include at least one text segment"));
    }

    /// 验证动作进度合并后保留最新进度文本。
    #[test]
    fn action_progress_coalescing_keeps_latest_message() {
        let merged = try_coalesce_stream_events(
            EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Connecting".to_string(),
            },
            EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Fetching results".to_string(),
            },
        )
        .expect("expected coalesced action progress");

        match merged {
            EngineEvent::ActionProgressUpdated { action_id, message } => {
                assert_eq!(action_id, "action-1");
                assert_eq!(message, "Fetching results");
            }
            other => panic!("expected action progress event, got {other:?}"),
        }
    }

    /// 验证诊断事件日志裁剪过长的动作输出内容。
    #[test]
    fn debug_event_log_trims_action_output_payload() {
        let content = "x".repeat(ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS + 128);
        let event = EngineEvent::ActionOutputDelta {
            action_id: "action-1".to_string(),
            stream: OutputStream::Stdout,
            content,
        };

        let log_event = engine_event_for_debug_log(&event);

        match log_event {
            EngineEvent::ActionOutputDelta { content, .. } => {
                assert_eq!(content.len(), ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS);
            }
            other => panic!("expected action output event, got {other:?}"),
        }
    }

    /// 验证追加消息在流事件边界持久化并在回合结束时结算。
    #[test]
    fn steer_applied_persists_at_the_exact_stream_boundary_and_settles_on_completion() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "插话之前".to_string(),
            },
            1000,
        );
        let applied = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::SteerApplied {
                client_steer_id: "client-steer-1".to_string(),
                content: "插话内容".to_string(),
                plan_mode: true,
                attachments: Vec::new(),
                input_items: vec![
                    TurnInputItem::Skill {
                        name: "测试技能".to_string(),
                        path: "skill://test".to_string(),
                    },
                    TurnInputItem::Mention {
                        name: "说明".to_string(),
                        path: "app://docs".to_string(),
                    },
                ],
            },
            1000,
        );
        assert!(applied.blocks_changed);
        assert!(applied.force_persist);
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "插话之后".to_string(),
            },
            1000,
        );

        let serialized = serde_json::to_value(&blocks).expect("blocks should serialize");
        assert_eq!(serialized[0]["content"], "插话之前");
        assert_eq!(serialized[1]["type"], "steer");
        assert_eq!(serialized[1]["steerId"], "client-steer-1");
        assert_eq!(serialized[1]["deliveryStatus"], "applied");
        assert_eq!(serialized[1]["planMode"], true);
        assert_eq!(serialized[1]["skills"][0]["name"], "测试技能");
        assert_eq!(serialized[1]["mentions"][0]["name"], "说明");
        assert_eq!(serialized[2]["content"], "插话之后");

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Completed,
            },
            1000,
        );
        let settled = serde_json::to_value(&blocks).expect("settled blocks should serialize");
        assert_eq!(settled[1]["deliveryStatus"], "settled");
    }

    /// 验证动作输出分块超限时保留末尾内容。
    #[test]
    fn trim_action_output_chunks_keeps_tail_of_oversized_chunk() {
        let mut chunks = vec![ActionOutputChunk {
            stream: "stdout".to_string(),
            content: "0123456789".to_string(),
        }];

        assert!(trim_action_output_chunks(&mut chunks, 6));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "6789");
    }

    /// 验证模型切换通知更新消息块后动作索引保持正确。
    #[test]
    fn model_reroute_notice_reindexes_action_blocks() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let started = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "action-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Other,
                summary: "search_docs".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        assert!(started.blocks_changed);

        let rerouted = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ModelRerouted {
                from_model: "gpt-5.1-codex-mini".to_string(),
                to_model: "gpt-5.3-codex".to_string(),
                reason: "highRiskCyberActivity".to_string(),
            },
            1000,
        );
        assert!(rerouted.blocks_changed);
        assert_eq!(rerouted.turn_model_id.as_deref(), Some("gpt-5.3-codex"));

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionProgressUpdated {
                action_id: "action-1".to_string(),
                message: "Fetching results".to_string(),
            },
            1000,
        );
        assert!(progress.blocks_changed);

        assert!(matches!(
            &blocks[0],
            ContentBlock::Notice {
                kind,
                level,
                title,
                ..
            } if kind == "model_rerouted" && level == "info" && title == "Model rerouted"
        ));
        match &blocks[1] {
            ContentBlock::Action { details, .. } => {
                let details_value: Value = serde_json::from_str(details.get())
                    .expect("action details should parse as JSON");
                assert_eq!(
                    details_value
                        .get("progressKind")
                        .and_then(serde_json::Value::as_str),
                    Some("mcp")
                );
                assert_eq!(
                    details_value
                        .get("progressMessage")
                        .and_then(serde_json::Value::as_str),
                    Some("Fetching results")
                );
            }
            other => panic!("expected action block, got {other:?}"),
        }
    }

    /// 验证正常回合结束时未完成动作被写入最终状态。
    #[test]
    fn turn_completion_finalizes_unfinished_actions() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let started = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        assert!(started.blocks_changed);

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Completed,
            },
            1000,
        );

        assert!(progress.blocks_changed);
        assert_eq!(progress.finalized_actions.len(), 1);
        assert_eq!(progress.finalized_actions[0].0, "web-search-1");
        assert!(progress.finalized_actions[0].1.success);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: None,
                ..
            } if status == "done"
        ));
    }

    /// 验证中断回合将未完成动作标记为错误。
    #[test]
    fn interrupted_turn_finalizes_unfinished_actions_as_error() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-interrupted".to_string(),
                engine_action_id: Some("item-interrupted".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Interrupted,
            },
            1000,
        );

        assert_eq!(progress.message_status, Some(MessageStatusDto::Interrupted));
        assert_eq!(progress.thread_status, Some(ThreadStatusDto::Idle));
        assert_eq!(progress.finalized_actions.len(), 1);
        assert!(!progress.finalized_actions[0].1.success);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: Some(result),
                ..
            } if status == "error"
                && !result.success
                && result.error.as_deref()
                    == Some("Action did not report completion before the turn was interrupted.")
        ));
    }

    /// 验证中断收尾不会覆盖已完成的动作结果。
    #[test]
    fn interrupted_turn_does_not_overwrite_an_already_completed_action() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "web-search-completed".to_string(),
                engine_action_id: Some("item-completed".to_string()),
                action_type: crate::engines::events::ActionType::Search,
                summary: "Web search".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionCompleted {
                action_id: "web-search-completed".to_string(),
                result: ActionResult {
                    success: true,
                    output: Some("done".to_string()),
                    error: None,
                    diff: None,
                    duration_ms: 12,
                },
            },
            1000,
        );

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Interrupted,
            },
            1000,
        );

        assert!(progress.finalized_actions.is_empty());
        assert!(matches!(
            &blocks[0],
            ContentBlock::Action {
                status,
                result: Some(result),
                ..
            } if status == "done" && result.success
        ));
    }

    /// 验证同一范围的差异更新会合并旧差异块。
    #[test]
    fn diff_update_collapses_existing_same_scope_diff_blocks() {
        let mut blocks = vec![
            ContentBlock::Diff {
                diff: "old diff 1".to_string(),
                scope: "turn".to_string(),
            },
            ContentBlock::Text {
                content: "kept".to_string(),
                plan_mode: None,
                is_steer: None,
                client_steer_id: None,
            },
            ContentBlock::Diff {
                diff: "old diff 2".to_string(),
                scope: "turn".to_string(),
            },
        ];
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        let progress = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::DiffUpdated {
                diff: "new diff".to_string(),
                scope: crate::engines::DiffScope::Turn,
            },
            1000,
        );

        assert!(progress.blocks_changed);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Text { content, .. } if content == "kept"
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Diff { diff, scope } if diff == "new diff" && scope == "turn"
        ));
    }

    /// 验证不支持的粘贴图片 MIME 类型无法保存为聊天附件。
    #[tokio::test]
    async fn image_extension_rejects_unknown_image_mime() {
        let error = save_pasted_image_attachment(
            "image.heic".to_string(),
            "image/heic".to_string(),
            "aGVsbG8=".to_string(),
        )
        .await
        .expect_err("unsupported pasted image MIME type should fail");

        assert_eq!(error, "Pasted image type is not supported.");
    }

    /// 验证同类通用通知块更新已有内容而不重复创建。
    #[test]
    fn generic_notice_blocks_are_upserted_by_kind() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "Visible assistant text.".to_string(),
            },
            1000,
        );
        let first = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "deprecation_notice".to_string(),
                level: "warning".to_string(),
                title: "Deprecation notice".to_string(),
                message: "Use the newer API.".to_string(),
                // 通用测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        assert!(first.blocks_changed);

        let second = apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "deprecation_notice".to_string(),
                level: "warning".to_string(),
                title: "Deprecation notice".to_string(),
                message: "Use the newer permissions API.".to_string(),
                // 通用测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        assert!(second.blocks_changed);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Notice { message, .. } if message == "Use the newer permissions API."
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Text { content, .. } if content == "Visible assistant text."
        ));
    }

    /// 验证 Hook 通知更新时保留流式消息中的原有位置。
    #[test]
    fn hook_notice_blocks_preserve_stream_position() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "before hooks".to_string(),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "hook_started_first".to_string(),
                level: "info".to_string(),
                title: "Hook started".to_string(),
                message: "first hook started".to_string(),
                // Hook 测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ActionStarted {
                action_id: "action-1".to_string(),
                engine_action_id: Some("item-1".to_string()),
                action_type: crate::engines::events::ActionType::Other,
                summary: "Tool call".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::Notice {
                kind: "hook_completed_first".to_string(),
                level: "info".to_string(),
                title: "Hook completed".to_string(),
                message: "first hook completed".to_string(),
                // Hook 测试通知不附带结构化后台任务元数据。
                metadata: None,
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::TextDelta {
                content: "after hooks".to_string(),
            },
            1000,
        );

        assert_eq!(blocks.len(), 5);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Text { content, .. } if content == "before hooks"
        ));
        assert!(matches!(
            &blocks[1],
            ContentBlock::Notice { kind, .. } if kind == "hook_started_first"
        ));
        assert!(matches!(
            &blocks[2],
            ContentBlock::Action { action_id, .. } if action_id == "action-1"
        ));
        assert!(matches!(
            &blocks[3],
            ContentBlock::Notice { kind, .. } if kind == "hook_completed_first"
        ));
        assert!(matches!(
            &blocks[4],
            ContentBlock::Text { content, .. } if content == "after hooks"
        ));
        assert_eq!(action_index.get("action-1"), Some(&2));
    }

    /// 验证审批过期事件会更新待处理审批卡片状态。
    #[test]
    fn approval_expired_marks_pending_approval_block() {
        let mut blocks = Vec::new();
        let mut action_index = HashMap::new();
        let mut approval_index = HashMap::new();

        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ApprovalRequested {
                approval_id: "approval-1".to_string(),
                action_type: crate::engines::events::ActionType::Other,
                summary: "Ask".to_string(),
                details: serde_json::json!({}),
            },
            1000,
        );
        apply_event_to_blocks(
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            &EngineEvent::ApprovalExpired {
                approval_id: "approval-1".to_string(),
                reason: "test".to_string(),
            },
            1000,
        );
        assert!(matches!(
            &blocks[0],
            ContentBlock::Approval { status, .. } if status == "expired"
        ));
    }

    /// 验证会话级权限审批答复持久化为会话接受决定。
    #[test]
    fn approval_response_persistence_tracks_permissions_session_scope() {
        let response = serde_json::json!({
            "permissions": {
                "network": {
                    "enabled": true
                }
            },
            "scope": "session"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "accept_for_session"
        );
    }

    /// 验证 MCP 询问答复持久化为对应操作决定。
    #[test]
    fn approval_response_persistence_tracks_mcp_elicitation_actions() {
        let response = serde_json::json!({
            "action": "decline"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "decline"
        );
    }

    /// 验证空权限集合的审批答复按拒绝持久化。
    #[test]
    fn approval_response_persistence_treats_empty_permissions_as_decline() {
        let response = serde_json::json!({
            "permissions": {},
            "scope": "turn"
        });

        assert_eq!(
            approval_response_decision_for_persistence(&response),
            "decline"
        );
    }

    /// 验证 Claude 审批答复格式无效时审批、消息和线程仍保持待处理。
    #[tokio::test]
    async fn invalid_claude_approval_response_keeps_approval_pending() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");
        let approval_id = "approval-invalid";
        let message_id = insert_pending_approval(&state, &thread, approval_id);

        let cli = ThreadService::new(state.clone())
            .cli_for_thread(&thread.id)
            .await
            .expect("thread CLI should be available");
        let error = cli
            .answer_chat_approval(ApprovalResponseRequest {
                thread_id: thread.id.clone(),
                approval_id: approval_id.to_string(),
                response: serde_json::json!({}),
            })
            .await
            .expect_err("expected invalid approval payload to fail");

        assert_eq!(error.code, "VALIDATION_FAILED");
        assert!(error.outcome.is_none());

        let conn = state.db.connect().expect("failed to open db connection");
        let approval_row = conn
            .query_row(
                "SELECT status, decision FROM approvals WHERE id = ?1",
                params![approval_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("failed to load approval row");
        assert_eq!(approval_row.0, "pending");
        assert_eq!(approval_row.1, None);

        let thread_status = conn
            .query_row(
                "SELECT status FROM threads WHERE id = ?1",
                params![thread.id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load thread status");
        assert_eq!(thread_status, "awaiting_approval");

        let raw_blocks = conn
            .query_row(
                "SELECT blocks_json FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load message blocks");
        let blocks: Value =
            serde_json::from_str(&raw_blocks).expect("message blocks should deserialize");
        assert_eq!(
            blocks
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("status"))
                .and_then(Value::as_str),
            Some("pending")
        );
        assert!(blocks
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("decision"))
            .is_none());
    }

    /// 验证本地 Codex CLI 不可用时拒绝答复且审批、消息和线程仍保持待处理。
    #[tokio::test]
    async fn missing_live_codex_approval_request_keeps_approval_pending() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.5-codex");
        let approval_id = "approval-reset";
        let message_id = insert_pending_approval_with_details(
            &state,
            &thread,
            approval_id,
            serde_json::json!({
                "command": "touch file.txt",
                "_serverMethod": "item/fileChange/requestApproval",
                "_rawRequestId": 42
            }),
        );

        let cli = ThreadService::new(state.clone())
            .cli_for_thread(&thread.id)
            .await
            .expect("thread CLI should be available");
        let error = cli
            .answer_chat_approval(ApprovalResponseRequest {
                thread_id: thread.id.clone(),
                approval_id: approval_id.to_string(),
                response: serde_json::json!({ "decision": "accept" }),
            })
            .await
            .expect_err("expected unavailable Codex CLI to reject the approval response");

        assert_eq!(error.code, "CLI_UNAVAILABLE");
        assert!(error.outcome.is_none());

        let conn = state.db.connect().expect("failed to open db connection");
        let approval_row = conn
            .query_row(
                "SELECT status, decision FROM approvals WHERE id = ?1",
                params![approval_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("failed to load approval row");
        assert_eq!(approval_row.0, "pending");
        assert_eq!(approval_row.1, None);

        let thread_status = conn
            .query_row(
                "SELECT status FROM threads WHERE id = ?1",
                params![thread.id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load thread status");
        assert_eq!(thread_status, "awaiting_approval");

        let raw_blocks = conn
            .query_row(
                "SELECT blocks_json FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, String>(0),
            )
            .expect("failed to load message blocks");
        let blocks: Value =
            serde_json::from_str(&raw_blocks).expect("message blocks should deserialize");
        assert_eq!(
            blocks
                .as_array()
                .and_then(|items| items.first())
                .and_then(|item| item.get("status"))
                .and_then(Value::as_str),
            Some("pending")
        );
        assert!(blocks
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("decision"))
            .is_none());
    }

    /// 验证 Codex 运行时重置错误按结构化错误类型分类，不依赖错误文案。
    #[test]
    fn codex_runtime_reset_error_is_classified_as_approval_not_pending() {
        let cli = CodexCli::new(test_app_state());
        let error = anyhow::Error::new(
            crate::engines::codex::ApprovalResponseTargetError::RuntimeReset,
        )
        .context("审批请求路由未完成");

        let business_error = cli.classify_chat_approval_error(&error);

        assert_eq!(business_error.code, "APPROVAL_NOT_PENDING");
        assert!(business_error.outcome.is_none());
    }

    /// 验证审批路由从数据库保存的原生传输元数据中读取。
    #[tokio::test]
    async fn load_codex_approval_response_route_reads_persisted_transport_metadata() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.5-codex");
        insert_pending_approval_with_details(
            &state,
            &thread,
            "approval-route",
            serde_json::json!({
                "command": "touch file.txt",
                "_serverMethod": "item/fileChange/requestApproval",
                "_rawRequestId": 42
            }),
        );

        let record = db::actions::get_chat_approval_record(&state.db, "approval-route")
            .expect("approval should load")
            .expect("approval record should exist");
        let cli = ThreadService::new(state.clone())
            .cli_for_thread(&thread.id)
            .await
            .expect("thread CLI should be available");
        let route = cli
            .chat_approval_route(&record.details)
            .expect("Codex route should parse");

        assert_eq!(
            route,
            Some(ApprovalRequestRoute {
                server_method: "item/fileChange/requestApproval".to_string(),
                raw_request_id: serde_json::json!(42),
            })
        );
    }

    /// 验证不支持请求值时使用模型目录中的默认推理强度。
    #[test]
    fn resolve_reasoning_effort_from_catalog_falls_back_to_model_default() {
        let engines = vec![EngineInfoDto {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            models: vec![EngineModelDto {
                id: "gpt-5.1-codex-mini".to_string(),
                display_name: "GPT-5.1 Codex Mini".to_string(),
                description: String::new(),
                hidden: false,
                is_default: false,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string()],
                attachment_modalities: vec!["text".to_string()],
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: vec![
                    ReasoningEffortOptionDto {
                        reasoning_effort: "medium".to_string(),
                        description: String::new(),
                    },
                    ReasoningEffortOptionDto {
                        reasoning_effort: "high".to_string(),
                        description: String::new(),
                    },
                ],
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }];

        assert_eq!(
            resolve_reasoning_effort_from_catalog(
                &engines,
                "codex",
                "gpt-5.1-codex-mini",
                Some("xhigh"),
            ),
            Some("medium".to_string())
        );
    }

    /// 验证模型目录支持请求的推理强度时保留该值。
    #[test]
    fn resolve_reasoning_effort_from_catalog_keeps_supported_effort() {
        let engines = vec![EngineInfoDto {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            models: vec![EngineModelDto {
                id: "gpt-5.1-codex-mini".to_string(),
                display_name: "GPT-5.1 Codex Mini".to_string(),
                description: String::new(),
                hidden: false,
                is_default: false,
                upgrade: None,
                availability_nux: None,
                upgrade_info: None,
                input_modalities: vec!["text".to_string()],
                attachment_modalities: vec!["text".to_string()],
                limits: None,
                supports_personality: false,
                default_reasoning_effort: "medium".to_string(),
                supported_reasoning_efforts: vec![
                    ReasoningEffortOptionDto {
                        reasoning_effort: "medium".to_string(),
                        description: String::new(),
                    },
                    ReasoningEffortOptionDto {
                        reasoning_effort: "high".to_string(),
                        description: String::new(),
                    },
                ],
            }],
            capabilities: EngineCapabilitiesDto {
                permission_modes: Vec::new(),
                sandbox_modes: Vec::new(),
                approval_decisions: Vec::new(),
            },
        }];

        assert_eq!(
            resolve_reasoning_effort_from_catalog(
                &engines,
                "codex",
                "gpt-5.1-codex-mini",
                Some("high"),
            ),
            Some("high".to_string())
        );
    }

    /// 验证模型未声明推理强度选项时不发送该设置。
    #[test]
    fn resolve_reasoning_effort_omits_effort_for_unsupported_models() {
        let model = EngineModelDto {
            id: "haiku".to_string(),
            display_name: "Haiku".to_string(),
            description: String::new(),
            hidden: false,
            is_default: false,
            upgrade: None,
            availability_nux: None,
            upgrade_info: None,
            input_modalities: vec!["text".to_string(), "image".to_string()],
            attachment_modalities: vec!["text".to_string(), "image".to_string()],
            limits: None,
            supports_personality: false,
            default_reasoning_effort: "low".to_string(),
            supported_reasoning_efforts: Vec::new(),
        };

        assert_eq!(
            resolve_reasoning_effort_for_model(&model, Some("high")),
            None
        );
    }

    /// 验证缺少模型目录时沿用线程已保存的模型标识。
    #[test]
    fn resolve_turn_model_id_accepts_thread_last_model_without_catalog() {
        let state = test_app_state();
        let mut thread = test_thread(&state, "codex", "gpt-5.5-codex");
        thread.model_id = "gpt-5.1-codex-mini".to_string();

        assert_eq!(
            resolve_turn_model_id(&thread, Some("gpt-5.1-codex-mini"), None)
                .expect("last model should resolve without a catalog"),
            "gpt-5.1-codex-mini"
        );
    }

    /// 大图（3000×2000 无透明 PNG）预览应逐级缩小并转 JPEG 编码，编码字节数与边长均不超过上限。
    #[tokio::test]
    async fn read_attachment_preview_shrinks_large_images() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-large-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for large image");
        let file_path = root.join("large.png");
        image::RgbImage::from_pixel(3000, 2000, image::Rgb([120, 60, 30]))
            .save(&file_path)
            .expect("failed to save large preview image");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect("large image preview should succeed");

        let payload = preview.expect("large image preview should be Some");
        assert_eq!(payload.mime_type, "image/jpeg");
        let bytes = BASE64
            .decode(payload.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        assert!(
            bytes.len() <= ATTACHMENT_PREVIEW_MAX_BYTES,
            "preview bytes {} should not exceed limit {}",
            bytes.len(),
            ATTACHMENT_PREVIEW_MAX_BYTES
        );
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert!(decoded.width() <= ATTACHMENT_PREVIEW_MAX_DIMENSION);
        assert!(decoded.height() <= ATTACHMENT_PREVIEW_MAX_DIMENSION);

        fs::remove_dir_all(&root).expect("failed to clean up large image temp dir");
    }

    /// 小图（100×100 带透明 PNG）预览应保持原尺寸并保留透明通道（PNG），不做放大。
    #[tokio::test]
    async fn read_attachment_preview_keeps_small_images_at_original_size() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-small-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for small image");
        let file_path = root.join("small.png");
        image::RgbaImage::from_pixel(100, 100, image::Rgba([10, 20, 30, 128]))
            .save(&file_path)
            .expect("failed to save small preview image");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect("small image preview should succeed");

        let payload = preview.expect("small image preview should be Some");
        assert_eq!(payload.mime_type, "image/png");
        let bytes = BASE64
            .decode(payload.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        assert!(
            bytes.len() <= ATTACHMENT_PREVIEW_MAX_BYTES,
            "preview bytes {} should not exceed limit {}",
            bytes.len(),
            ATTACHMENT_PREVIEW_MAX_BYTES
        );
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 100);

        fs::remove_dir_all(&root).expect("failed to clean up small image temp dir");
    }

    /// 新发送图片生成永久预览路径；消息尚未持久化时，批次释放应清理永久副本。
    #[tokio::test]
    async fn persistent_image_preview_batch_cleans_uncommitted_files() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-message-image-preview-uncommitted-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let source_file_path = root.join("source.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 128]))
            .save(&source_file_path)
            .expect("failed to save persistent preview source image");
        let mut attachment = test_attachment("source.png", Some("image/png"));
        attachment.file_path = source_file_path.display().to_string();
        attachment.size_bytes = fs::metadata(&source_file_path)
            .expect("failed to read source image metadata")
            .len();

        let batch = persist_message_image_previews(std::slice::from_mut(&mut attachment))
            .await
            .expect("persistent preview should be created");
        let preview_file_path = PathBuf::from(
            attachment
                .preview_file_path
                .as_deref()
                .expect("attachment should contain persistent preview path"),
        );
        assert!(preview_file_path.is_file());

        drop(batch);

        assert!(!preview_file_path.exists());
        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 消息持久化后提交批次，永久图片副本应继续保留。
    #[tokio::test]
    async fn persistent_image_preview_batch_keeps_committed_files() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-message-image-preview-committed-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let source_file_path = root.join("source.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([120, 60, 30]))
            .save(&source_file_path)
            .expect("failed to save persistent preview source image");
        let mut attachment = test_attachment("source.png", Some("image/png"));
        attachment.file_path = source_file_path.display().to_string();
        attachment.size_bytes = fs::metadata(&source_file_path)
            .expect("failed to read source image metadata")
            .len();

        let batch = persist_message_image_previews(std::slice::from_mut(&mut attachment))
            .await
            .expect("persistent preview should be created");
        let preview_file_path = PathBuf::from(
            attachment
                .preview_file_path
                .as_deref()
                .expect("attachment should contain persistent preview path"),
        );

        batch.commit();

        assert!(preview_file_path.is_file());
        fs::remove_file(&preview_file_path)
            .expect("failed to clean up committed persistent preview file");
        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 已发送消息读取时应优先使用永久副本，即使原附件路径已经失效。
    #[tokio::test]
    async fn read_attachment_preview_prefers_persistent_file_path() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-persistent-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for persistent preview");
        let preview_file_path = root.join("persistent.png");
        image::RgbaImage::from_pixel(12, 10, image::Rgba([10, 20, 30, 128]))
            .save(&preview_file_path)
            .expect("failed to save persistent preview image");

        let preview = read_attachment_preview(
            root.join("missing-original.png").display().to_string(),
            Some("image/png".to_string()),
            Some(preview_file_path.display().to_string()),
        )
        .await
        .expect("persistent preview path should be preferred")
        .expect("persistent preview should be available");

        assert_eq!(preview.mime_type, "image/png");
        let bytes = BASE64
            .decode(preview.data_base64.as_bytes())
            .expect("preview data should decode as base64");
        let decoded =
            image::load_from_memory(&bytes).expect("preview bytes should decode as an image");
        assert_eq!((decoded.width(), decoded.height()), (12, 10));

        fs::remove_dir_all(&root).expect("failed to clean up persistent preview temp dir");
    }

    /// 消息块序列化时应保留永久预览路径，供前端重新加载聊天记录后读取。
    #[test]
    fn user_attachment_block_serializes_persistent_preview_path() {
        let mut attachment = test_attachment("result.png", Some("image/png"));
        attachment.preview_file_path =
            Some("C:\\AuraCoder\\attachments\\message-images\\result.png".to_string());

        let blocks = build_user_blocks("查看结果", &[], &[attachment], false, false, None);
        let value = serde_json::to_value(blocks).expect("user blocks should serialize");

        assert_eq!(
            value[0]["previewFilePath"],
            "C:\\AuraCoder\\attachments\\message-images\\result.png"
        );
    }

    /// 图片内容损坏时保留底层解码异常，IPC 仅返回业务语义错误。
    #[tokio::test]
    async fn read_attachment_preview_reports_invalid_image_as_business_error() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-invalid-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for invalid image");
        let file_path = root.join("invalid.png");
        fs::write(&file_path, b"not an image").expect("failed to write invalid image");

        let error = read_attachment_preview(
            file_path.display().to_string(),
            Some("image/png".to_string()),
            None,
        )
        .await
        .expect_err("invalid image should return a business error");

        assert_eq!(error, "图片预览加载失败");
        fs::remove_dir_all(&root).expect("failed to clean up invalid image temp dir");
    }

    /// 非图片附件（text/plain）预览应返回 None，不产生编码数据。
    #[tokio::test]
    async fn read_attachment_preview_rejects_non_image() {
        let root = std::env::temp_dir().join(format!(
            "auracoder-attachment-preview-text-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root for text file");
        let file_path = root.join("hello.txt");
        fs::write(&file_path, "hello world").expect("failed to write text file");

        let preview = read_attachment_preview(
            file_path.display().to_string(),
            Some("text/plain".to_string()),
            None,
        )
        .await
        .expect("non-image preview should not error");

        assert!(preview.is_none());

        fs::remove_dir_all(&root).expect("failed to clean up text file temp dir");
    }

