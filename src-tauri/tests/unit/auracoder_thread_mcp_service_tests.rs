    use super::*;
    use rusqlite::params;

    #[test]
    fn exposes_exactly_two_tools() {
        let db = test_database();
        let service = AuraCoderThreadMcpService::new(db);
        let names = service
            .tool_specs()
            .into_iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "get_auracoder_thread_message_count",
                "get_auracoder_thread_messages_page",
            ]
        );
    }

    #[test]
    fn serializes_page_message_timestamp_as_snake_case() {
        let record = messages::AuraCoderThreadMessageRecord {
            id: "message-1".to_string(),
            role: "user".to_string(),
            content: Some("内容".to_string()),
            created_at: "2026-08-22T00:00:00Z".to_string(),
        };
        let value = serde_json::to_value(record).expect("message record should serialize");
        assert_eq!(value["created_at"], "2026-08-22T00:00:00Z");
        assert!(value.get("createdAt").is_none());
    }

    /// 验证现有 Codex、OpenCode 和不存在会话的引擎查询结果。
    #[tokio::test]
    async fn reads_engine_for_codex_opencode_and_missing_threads() {
        let db = test_database();
        let workspace = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-engine-workspace-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("workspace");
        let codex_thread =
            crate::db::threads::create_thread(&db, &workspace.id, "codex", "model", "Codex thread")
                .expect("Codex thread");
        let opencode_thread = crate::db::threads::create_thread(
            &db,
            &workspace.id,
            "opencode",
            "model",
            "OpenCode thread",
        )
        .expect("OpenCode thread");
        let service = AuraCoderThreadMcpService::new(db);

        assert_eq!(
            service
                .thread_engine(&codex_thread.id)
                .await
                .expect("Codex engine query"),
            Some("codex".to_string())
        );
        assert_eq!(
            service
                .thread_engine(&opencode_thread.id)
                .await
                .expect("OpenCode engine query"),
            Some("opencode".to_string())
        );
        assert_eq!(
            service
                .thread_engine(&uuid::Uuid::new_v4().to_string())
                .await
                .expect("missing engine query"),
            None
        );
    }

    /*
    #[tokio::test]
    async fn reads_count_and_descending_pages_with_workspace_isolation() {
        let db = test_database();
        let workspace_one = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-workspace-one-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("first workspace");
        let workspace_two = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-workspace-two-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("second workspace");
        let target_thread = crate::db::threads::create_thread(
            &db,
            &workspace_one.id,
            "claude",
            "model",
            "Referenced thread",
        )
        .expect("target thread");
        let other_thread = crate::db::threads::create_thread(
            &db,
            &workspace_two.id,
            "opencode",
            "model",
            "Other thread",
        )
        .expect("other thread");

        let early = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "早",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("early message");
        let middle = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "中",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("middle message");
        let late = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "晚",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("late message");
        {
            let conn = db.connect().expect("database connection");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:01Z", early.id],
            )
            .expect("early timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:02Z", middle.id],
            )
            .expect("middle timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:03Z", late.id],
            )
            .expect("late timestamp");
        }

        let service = AuraCoderThreadMcpService::new(db);
        service.bind_engine_thread("codex", "current-engine-thread", &workspace_one.id);
        let count = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_message_count",
                json!({ "thread_id": target_thread.id }),
            )
            .await
            .expect("count query");
        assert_eq!(count["message_count"], 3);

        let first_page = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_messages_page",
                json!({ "thread_id": target_thread.id, "page": 1, "page_size": 2 }),
            )
            .await
            .expect("first page query");
        let first_messages = first_page["messages"]
            .as_array()
            .expect("first page messages");
        assert_eq!(first_messages.len(), 2);
        assert_eq!(first_messages[0]["content"], "晚");
        assert_eq!(first_messages[1]["content"], "中");
        for message in first_messages {
            assert!(message.get("id").is_some());
            assert!(message.get("role").is_some());
            assert!(message.get("content").is_some());
            assert!(message.get("created_at").is_some());
            assert!(message.get("createdAt").is_none());
        }

        let second_page = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_messages_page",
                json!({ "thread_id": target_thread.id, "page": 2, "page_size": 2 }),
            )
            .await
            .expect("second page query");
        assert_eq!(second_page["messages"][0]["content"], "早");

        let isolation_error = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_message_count",
                json!({ "thread_id": other_thread.id }),
            )
            .await
            .expect_err("cross-workspace query must fail");
        assert_eq!(
            isolation_error,
            "AuraCoder 会话工具只允许读取当前项目的会话"
        );
    }
    */

    /// 验证公共只读服务按创建时间倒序分页，并保留 typed 字段与参数校验。
    #[tokio::test]
    async fn reads_count_and_descending_pages() {
        let db = test_database();
        let workspace = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-query-workspace-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("workspace");
        let target_thread = crate::db::threads::create_thread(
            &db,
            &workspace.id,
            "claude",
            "model",
            "Referenced thread",
        )
        .expect("target thread");
        let early = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "早",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("early message");
        let middle = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "中",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("middle message");
        let late = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "晚",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("late message");
        {
            let conn = db.connect().expect("database connection");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:01Z", early.id],
            )
            .expect("early timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:02Z", middle.id],
            )
            .expect("middle timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:03Z", late.id],
            )
            .expect("late timestamp");
        }

        let service = AuraCoderThreadMcpService::new(db);
        assert_eq!(
            service
                .thread_message_count(&target_thread.id)
                .await
                .expect("message count"),
            3
        );
        let first_page = service
            .thread_messages_page(&target_thread.id, 1, 2)
            .await
            .expect("first message page");
        assert_eq!(first_page.len(), 2);
        assert_eq!(first_page[0].content.as_deref(), Some("晚"));
        assert_eq!(first_page[1].content.as_deref(), Some("中"));
        let second_page = service
            .thread_messages_page(&target_thread.id, 2, 2)
            .await
            .expect("second message page");
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].content.as_deref(), Some("早"));

        // 序列化每条 typed 消息，确认稳定 snake_case 时间字段和记录字段。
        for message in first_page.iter().chain(second_page.iter()) {
            let value = serde_json::to_value(message).expect("message record should serialize");
            assert!(value.get("id").is_some());
            assert!(value.get("role").is_some());
            assert!(value.get("content").is_some());
            assert!(value.get("created_at").is_some());
            assert!(value.get("createdAt").is_none());
        }

        assert!(service
            .thread_messages_page(&target_thread.id, 0, 2)
            .await
            .is_err());
        assert!(service
            .thread_messages_page(&target_thread.id, 1, 0)
            .await
            .is_err());
    }

    fn test_database() -> Database {
        let path =
            std::env::temp_dir().join(format!("auracoder-thread-mcp-{}.db", uuid::Uuid::new_v4()));
        Database::open(path).expect("test database")
    }
