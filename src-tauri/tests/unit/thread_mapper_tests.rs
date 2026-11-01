
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::db::{messages, workspaces, ConnectionPool, SQLITE_POOL_MAX_IDLE};

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-threads-{}.db", Uuid::new_v4()));
        let db = Database {
            path,
            pool: Arc::new(ConnectionPool {
                idle: Mutex::new(Vec::new()),
                max_idle: SQLITE_POOL_MAX_IDLE,
            }),
        };
        db.run_migrations().expect("failed to run test migrations");
        db
    }

    fn test_thread(db: &Database, title: &str) -> ThreadDto {
        let root = std::env::temp_dir().join(format!("auracoder-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp workspace root");
        let workspace = workspaces::upsert_workspace(db, root.to_string_lossy().as_ref()).unwrap();
        create_thread(db, &workspace.id, "codex", "gpt-5.3-codex", title).unwrap()
    }

    /// 验证 Claude 线程同步标记默认关闭，并可独立切换后读取。
    #[test]
    fn claude_sync_required_defaults_to_false_and_round_trips() {
        let db = test_db();
        let thread = test_thread(&db, "Claude sync flag");

        assert!(!get_claude_sync_required(&db, &thread.id).unwrap());
        set_claude_sync_required(&db, &thread.id, true).unwrap();
        assert!(get_claude_sync_required(&db, &thread.id).unwrap());
        set_claude_sync_required(&db, &thread.id, false).unwrap();
        assert!(!get_claude_sync_required(&db, &thread.id).unwrap());
    }

    #[test]
    fn update_thread_runtime_snapshot_preserves_manual_title() {
        let db = test_db();
        let thread = test_thread(&db, "Manual title");
        let manual_metadata = json!({
            "manualTitle": true,
            "manualTitleUpdatedAt": "2026-03-06T12:00:00Z",
        });
        update_engine_metadata(&db, &thread.id, &manual_metadata).unwrap();

        let updated = update_thread_runtime_snapshot(
            &db,
            &thread.id,
            Some("Engine renamed title"),
            Some(ThreadStatusDto::Idle),
            Some(&json!({
                "manualTitle": true,
                "codexThreadStatus": "idle",
                "codexSyncRequired": false,
            })),
        )
        .unwrap();

        assert_eq!(updated.title, "Manual title");
        assert_eq!(updated.status, ThreadStatusDto::Idle);
        assert_eq!(
            updated
                .engine_metadata
                .as_ref()
                .and_then(|value| value.get("codexThreadStatus"))
                .and_then(serde_json::Value::as_str),
            Some("idle")
        );
        assert_eq!(
            updated
                .engine_metadata
                .as_ref()
                .and_then(|value| value.get("manualTitle"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn update_thread_last_activity_persists_exact_timestamp() {
        let db = test_db();
        let thread = test_thread(&db, "Timestamp");
        let raw_last_activity_at = "2026-08-19T12:33:53+00:00";
        let expected_last_activity_at = runtime_env::normalize_time_to_local(raw_last_activity_at)
            .unwrap();

        let updated = update_thread_last_activity(&db, &thread.id, raw_last_activity_at).unwrap();

        assert_eq!(updated.last_activity_at, expected_last_activity_at);
        assert_eq!(updated.created_at, thread.created_at);

        let persisted = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(persisted.last_activity_at, expected_last_activity_at);
        assert_eq!(persisted.created_at, thread.created_at);
    }

    #[test]
    fn update_thread_last_activity_rejects_missing_thread() {
        let db = test_db();
        let error = update_thread_last_activity(&db, "missing-thread", "2026-08-19T12:33:53+00:00")
            .expect_err("missing thread must not update successfully");

        assert!(error
            .to_string()
            .contains("thread not found: missing-thread"));
    }

    #[test]
    fn ssh_remote_snapshot_preserves_local_runtime_selection_metadata() {
        let db = test_db();
        let thread = test_thread(&db, "Remote thread");
        set_engine_thread_id(&db, &thread.id, "remote-thread-1").unwrap();
        update_engine_metadata(
            &db,
            &thread.id,
            &json!({
                "codexRemote": {"version": "old"},
            }),
        )
        .unwrap();
        update_thread(
            &db,
            &ThreadUpdateDto {
                id: thread.id.clone(),
                reasoning_effort: Some("high".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let updated = upsert_ssh_remote_thread_snapshot(
            &db,
            &thread.workspace_id,
            "codex",
            "remote-thread-1",
            "openai",
            "Refreshed remote title",
            ThreadStatusDto::Idle,
            &json!({
                "sshRemote": true,
                "codexRemoteCwd": "/var/work/project",
                "codexRemote": {"version": "new"},
            }),
            Some("medium"),
            Some("2026-08-14T12:00:00Z"),
        )
        .unwrap();

        let metadata = updated.engine_metadata.unwrap();
        assert_eq!(updated.model_id, "gpt-5.3-codex");
        assert_eq!(updated.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            metadata.get("codexRemote"),
            Some(&json!({"version": "new"}))
        );
        assert_eq!(metadata.get("sshRemote"), Some(&json!(true)));
    }

    #[test]
    fn reconfigure_unstarted_thread_runtime_reuses_the_same_empty_thread() {
        let db = test_db();
        let thread = test_thread(&db, "New thread");
        let metadata = json!({"opencodePermissionMode": "ask"});

        let updated = reconfigure_unstarted_thread_runtime(
            &db,
            &thread.id,
            "opencode",
            "opencode-model",
            Some(&metadata),
        )
        .unwrap();

        assert_eq!(updated.id, thread.id);
        assert_eq!(updated.title, "New thread");
        assert_eq!(updated.engine_id, "opencode");
        assert_eq!(updated.model_id, "opencode-model");
        assert_eq!(updated.engine_metadata, Some(metadata));
    }

    #[test]
    fn reconfigure_unstarted_thread_runtime_rejects_started_threads() {
        let db = test_db();
        let remote_thread = test_thread(&db, "Remote thread");
        set_engine_thread_id(&db, &remote_thread.id, "remote-thread-1").unwrap();

        let remote_error = reconfigure_unstarted_thread_runtime(
            &db,
            &remote_thread.id,
            "opencode",
            "opencode-model",
            None,
        )
        .unwrap_err();
        assert!(remote_error
            .to_string()
            .contains("thread has already started"));

        let message_thread = test_thread(&db, "Message thread");
        messages::insert_user_message(
            &db,
            &message_thread.id,
            "Do not change this CLI",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            None,
        )
        .unwrap();

        let stale_counter_thread = get_thread(&db, &message_thread.id).unwrap().unwrap();
        assert_eq!(stale_counter_thread.message_count, 0);

        let message_error = reconfigure_unstarted_thread_runtime(
            &db,
            &message_thread.id,
            "opencode",
            "opencode-model",
            None,
        )
        .unwrap_err();
        assert!(message_error
            .to_string()
            .contains("thread has already started"));
    }

    #[test]
    fn update_thread_permissions_persists_component_without_changing_metadata() {
        let db = test_db();
        let thread = test_thread(&db, "Permissions");
        let permission_values = PermissionComponentJson::default();
        let original_metadata = json!({"unrelated": true});
        update_engine_metadata(&db, &thread.id, &original_metadata).unwrap();

        let updated =
            update_thread_permissions(&db, &thread.id, Some(&permission_values)).unwrap();

        assert_eq!(updated.permission_mode.as_ref(), Some(&permission_values));
        assert_eq!(updated.engine_metadata, Some(original_metadata.clone()));
        let persisted = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(persisted.permission_mode.as_ref(), Some(&permission_values));
        assert_eq!(persisted.engine_metadata, Some(original_metadata));
    }

    #[test]
    fn update_thread_permissions_rejects_missing_thread() {
        let db = test_db();
        let permission_values = PermissionComponentJson::default();
        let error = update_thread_permissions(
            &db,
            "missing-thread",
            Some(&permission_values),
        )
        .expect_err("missing thread must not update successfully");

        assert!(error
            .to_string()
            .contains("thread not found: missing-thread"));
        assert!(get_thread(&db, "missing-thread").unwrap().is_none());
    }

    #[test]
    fn update_thread_permissions_does_not_change_another_thread_on_failure() {
        let db = test_db();
        let thread = test_thread(&db, "Permissions unchanged");
        let mut attempted_permissions = PermissionComponentJson::default();
        attempted_permissions.autonomy_preset = vec!["full".to_owned()];
        let original_metadata = json!({"original": true});
        update_engine_metadata(&db, &thread.id, &original_metadata).unwrap();

        let _ = update_thread_permissions(
            &db,
            "missing-thread",
            Some(&attempted_permissions),
        )
        .expect_err("missing thread must roll back the failed transaction");

        let unchanged = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(unchanged.permission_mode, thread.permission_mode);
        assert_eq!(unchanged.engine_metadata, Some(original_metadata));
    }

    /// 验证按消息时间策略重算消息数量、Token 和活动时间。
    #[test]
    fn refresh_thread_message_stats_recomputes_counters_from_messages() {
        let db = test_db();
        let thread = test_thread(&db, "Stats");
        let old_last_activity_at = "2026-01-01T00:00:00+00:00";
        update_thread_last_activity(&db, &thread.id, old_last_activity_at).unwrap();
        messages::insert_user_message(
            &db,
            &thread.id,
            "Count this turn",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        let assistant = messages::insert_assistant_placeholder(
            &db,
            &thread.id,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        messages::complete_assistant_message(
            &db,
            &assistant.id,
            crate::models::MessageStatusDto::Completed,
            Some((13, 21)),
            Some("gpt-5.3-codex"),
        )
        .unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::FromMessages,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 2);
        assert_eq!(refreshed.total_tokens, 34);
        assert!(!refreshed.last_activity_at.is_empty());
        assert_ne!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证无消息时仅重算统计不会改写已有活动时间。
    #[test]
    fn refresh_thread_message_stats_preserves_existing_activity_without_messages() {
        let db = test_db();
        let thread = test_thread(&db, "Empty stats");
        let raw_last_activity_at = "2026-01-02T00:00:00+00:00";
        let old_last_activity_at = runtime_env::normalize_time_to_local(raw_last_activity_at).unwrap();
        update_thread_last_activity(&db, &thread.id, raw_last_activity_at).unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::PreserveExisting,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 0);
        assert_eq!(refreshed.total_tokens, 0);
        assert_eq!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证有消息时仅重算统计不会改写已有活动时间。
    #[test]
    fn refresh_thread_message_stats_preserves_existing_activity_while_recomputing_counters() {
        let db = test_db();
        let thread = test_thread(&db, "Preserved stats");
        let raw_last_activity_at = "2026-01-03T00:00:00+00:00";
        let old_last_activity_at = runtime_env::normalize_time_to_local(raw_last_activity_at).unwrap();
        update_thread_last_activity(&db, &thread.id, raw_last_activity_at).unwrap();
        messages::insert_user_message(
            &db,
            &thread.id,
            "Count this turn",
            None,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        let assistant = messages::insert_assistant_placeholder(
            &db,
            &thread.id,
            Some("codex"),
            Some("gpt-5.3-codex"),
            Some("low"),
        )
        .unwrap();
        messages::complete_assistant_message(
            &db,
            &assistant.id,
            crate::models::MessageStatusDto::Completed,
            Some((13, 21)),
            Some("gpt-5.3-codex"),
        )
        .unwrap();

        refresh_thread_message_stats(
            &db,
            &thread.id,
            ThreadMessageStatsActivityPolicy::PreserveExisting,
        )
        .unwrap();

        let refreshed = get_thread(&db, &thread.id).unwrap().unwrap();
        assert_eq!(refreshed.message_count, 2);
        assert_eq!(refreshed.total_tokens, 34);
        assert_eq!(refreshed.last_activity_at, old_last_activity_at);
    }

    /// 验证只更新线程状态时保留已有活动时间。
    #[test]
    fn update_thread_preserves_last_activity_when_only_status_changes() {
        let db = test_db();
        let thread = test_thread(&db, "Status only");
        let raw_last_activity_at = "2026-01-04T00:00:00+00:00";
        let old_last_activity_at = runtime_env::normalize_time_to_local(raw_last_activity_at).unwrap();
        update_thread_last_activity(&db, &thread.id, raw_last_activity_at).unwrap();

        let updated = update_thread(
            &db,
            &ThreadUpdateDto {
                // 目标线程标识，确保只更新本条会话。
                id: thread.id.clone(),
                // 远端同步后的新运行状态。
                status: Some(ThreadStatusDto::AwaitingApproval.as_str().to_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(updated.status, ThreadStatusDto::AwaitingApproval);
        assert_eq!(updated.last_activity_at, old_last_activity_at);
    }

    #[test]
    fn list_threads_for_workspace_includes_engine_backed_threads_without_messages() {
        let db = test_db();
        let visible = test_thread(&db, "Remote");
        let hidden = test_thread(&db, "Hidden");
        set_engine_thread_id(&db, &visible.id, "codex-thread-123").unwrap();

        let listed = list_threads_for_workspace(&db, &visible.workspace_id).unwrap();
        let listed_ids = listed
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();

        assert!(listed_ids.contains(&visible.id));
        assert!(!listed_ids.contains(&hidden.id));
    }

    #[test]
    fn list_archived_threads_for_workspace_includes_engine_backed_threads_without_messages() {
        let db = test_db();
        let visible = test_thread(&db, "Archived remote");
        let hidden = test_thread(&db, "Archived hidden");
        set_engine_thread_id(&db, &visible.id, "codex-thread-archived").unwrap();
        archive_thread(&db, &visible.id).unwrap();
        archive_thread(&db, &hidden.id).unwrap();

        let listed = list_archived_threads_for_workspace(&db, &visible.workspace_id).unwrap();
        let listed_ids = listed
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();

        assert!(listed_ids.contains(&visible.id));
        assert!(!listed_ids.contains(&hidden.id));
    }
