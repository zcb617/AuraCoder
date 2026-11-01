//! 项目会话刷新测试，覆盖三种 CLI 生产 helper 的解析、持久化和过滤规则。
use std::sync::Arc;
use super::is_local_workspace;
    use crate::{
        cli_tools::{
            claude_code::{parse_ssh_session, ClaudeCodeCli},
            opencode::{session_for_refresh, OpenCodeCli},
            CliSessionSnapshot,
        },
        db::Database,
        engines::OpenCodeRemoteSessionSummary,
        models::{ThreadStatusDto, WorkspaceDto},
        runtime_env,
    };
    use serde_json::{json, Value};
    use uuid::Uuid;

    use rusqlite::params;

    /// 创建会话刷新测试所需的独立数据库和 workspace。
    fn test_database_and_workspace() -> (Database, WorkspaceDto) {
        let path =
            std::env::temp_dir().join(format!("auracoder-claude-refresh-{}.db", Uuid::new_v4()));
        let db = Database::open(path).expect("failed to create test database");
        let workspace_id = format!("workspace-{}", Uuid::new_v4());
        let root_path = format!("/tmp/auracoder-claude-refresh-{}", Uuid::new_v4());
        let conn = db.connect().expect("failed to connect test database");
        conn.execute(
            "INSERT INTO workspaces (id, name, root_path, location_kind)
             VALUES (?1, ?2, ?3, 'local')",
            params![workspace_id, "Test workspace", root_path],
        )
        .expect("failed to insert test workspace");
        let workspace = WorkspaceDto {
            id: workspace_id,
            name: "Test workspace".to_string(),
            root_path,
            location_kind: "ssh".to_string(),
            ssh_connection_id: Some("test-connection".to_string()),
            connection_display_name: None,
            connection_enabled: Some(true),
            connection_deleted: Some(false),
            connection_status: Some("ok".to_string()),
            trust_level: crate::models::TrustLevelDto::Standard,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_opened_at: "2026-01-01T00:00:00Z".to_string(),
        };
        (db, workspace)
    }

    /// 构造完整的 CLI 会话快照以驱动真实刷新持久化 helper。
    fn test_snapshot(engine_thread_id: &str, title: &str) -> CliSessionSnapshot {
        CliSessionSnapshot {
            engine_thread_id: engine_thread_id.to_string(),
            title: title.to_string(),
            preview: None,
            cwd: "/tmp/project".to_string(),
            model_id: String::new(),
            reasoning_effort: None,
            created_at: None,
            updated_at: Some("2026-08-18T10:00:00Z".to_string()),
            source_kind: None,
            raw_status: Some("idle".to_string()),
            active_flags: Vec::new(),
            status: ThreadStatusDto::Idle,
            archived: false,
            metadata: json!({"remote": true}),
        }
    }

    /// 验证 Claude 批量导入所有新会话并设置同步标记。
    #[tokio::test]
    async fn claude_batch_import_inserts_all_new_sessions() {
        let (db, workspace) = test_database_and_workspace();
        let snapshots = vec![
            test_snapshot("claude-new-1", "first"),
            test_snapshot("claude-new-2", "second"),
        ];

        ClaudeCodeCli::persist_session_snapshots(Arc::new(db.clone()), &workspace, snapshots)
            .await
            .expect("Claude batch import should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count imported Claude sessions");
        assert_eq!(count, 2);
        let sync_flags: Vec<i64> = conn
            .prepare(
                "SELECT claude_sync_required FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'
                 ORDER BY engine_thread_id",
            )
            .expect("failed to prepare imported Claude sync flag query")
            .query_map(params![workspace.id], |row| row.get(0))
            .expect("failed to read imported Claude sync flags")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("failed to decode imported Claude sync flags");
        assert_eq!(sync_flags, vec![1, 1]);
    }

    /// 验证 Claude 一次事务可导入远端 500 条会话。
    #[tokio::test]
    async fn claude_batch_import_accepts_remote_limit_of_500_sessions() {
        let (db, workspace) = test_database_and_workspace();
        let snapshots: Vec<_> = (0..500)
            .map(|index| {
                test_snapshot(
                    &format!("claude-batch-{index}"),
                    &format!("batch title {index}"),
                )
            })
            .collect();

        ClaudeCodeCli::persist_session_snapshots(Arc::new(db.clone()), &workspace, snapshots)
            .await
            .expect("Claude 500-session batch import should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count 500 imported Claude sessions");
        assert_eq!(count, 500);
    }

    /// 验证 Claude 刷新更新允许字段并保护本地状态与配置。
    #[tokio::test]
    async fn claude_batch_import_updates_existing_sessions_without_overwriting_local_state() {
        let (db, workspace) = test_database_and_workspace();
        let conn = db.connect().expect("failed to connect test database");
        conn.execute(
            "INSERT INTO threads (
                 id, workspace_id, engine_id, model_id, engine_thread_id,
                 engine_metadata_json, title, status, archived_at,
                 message_count, total_tokens, plan_mode, send_method,
                 reasoning_effort, permission_mode, created_at, last_activity_at
             ) VALUES (?1, ?2, 'claude', 'local-model', 'claude-existing',
                       ?3, 'local-title', 'completed', ?4, 5, 42, 1, 'manual',
                       'low', 'default', ?5, ?6)",
            params![
                "local-thread-id",
                workspace.id,
                r#"{"local":true,"manualTitle":true,"manualTitleUpdatedAt":"2026-08-17T09:00:00Z"}"#,
                "2026-08-17T10:00:00Z",
                "2026-08-16T10:00:00Z",
                "2026-08-16T11:00:00Z",
            ],
        )
        .expect("failed to insert existing Claude session");
        conn.execute(
            "INSERT INTO threads (
                 id, workspace_id, engine_id, model_id, engine_thread_id,
                 engine_metadata_json, title, status, archived_at,
                 message_count, total_tokens, plan_mode, send_method,
                 reasoning_effort, permission_mode, created_at, last_activity_at
             ) VALUES (?1, ?2, 'claude', 'local-active-model', 'claude-existing-active',
                       ?3, 'local-active-title', 'idle', NULL, 7, 84, 0, 'auto',
                       'medium', 'safe', ?4, ?5)",
            params![
                "local-active-thread-id",
                workspace.id,
                r#"{"local":true,"active":true}"#,
                "2026-08-15T10:00:00Z",
                "2026-08-15T11:00:00Z",
            ],
        )
        .expect("failed to insert active existing Claude session");
        drop(conn);

        let mut existing = test_snapshot("claude-existing", "remote-title");
        existing.model_id = "remote-model".to_string();
        existing.status = ThreadStatusDto::Streaming;
        existing.reasoning_effort = Some("remote-effort".to_string());
        existing.metadata = json!({
            "remote": true,
            "manualTitle": false,
            "manualTitleUpdatedAt": "remote-value",
            "remoteOnly": "new"
        });
        let mut active_existing = test_snapshot("claude-existing-active", "remote-active-title");
        active_existing.model_id = "remote-active-model".to_string();
        active_existing.status = ThreadStatusDto::Completed;
        active_existing.metadata = json!({"remoteActive": true});
        let snapshots = vec![
            existing,
            active_existing,
            test_snapshot("claude-new", "new title"),
        ];
        ClaudeCodeCli::persist_session_snapshots(Arc::new(db.clone()), &workspace, snapshots)
            .await
            .expect("Claude mixed batch import should succeed");

        let conn = db.connect().expect("failed to read Claude sessions");
        let existing_row: (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            i64,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        ) = conn
            .query_row(
                "SELECT title, model_id, status, archived_at, created_at, last_activity_at,
                        COALESCE(engine_metadata_json, ''), message_count, total_tokens,
                        plan_mode, send_method, reasoning_effort, permission_mode,
                        claude_sync_required
                 FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = 'claude-existing'",
                params![workspace.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                    ))
                },
            )
            .expect("failed to read existing Claude session");
        assert_eq!(existing_row.0, "local-title");
        assert_eq!(existing_row.1, "local-model");
        assert_eq!(existing_row.2, "completed");
        assert_eq!(existing_row.3.as_deref(), Some("2026-08-17T10:00:00Z"));
        assert_eq!(existing_row.4, "2026-08-16T10:00:00Z");
        assert_eq!(
            existing_row.5,
            runtime_env::normalize_time_to_local("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(existing_row.7, 5);
        assert_eq!(existing_row.8, 42);
        assert_eq!(existing_row.9, Some(1));
        assert_eq!(existing_row.10.as_deref(), Some("manual"));
        assert_eq!(existing_row.11.as_deref(), Some("low"));
        assert_eq!(existing_row.12.as_deref(), Some("default"));
        assert_eq!(existing_row.13, 1);
        let existing_metadata: Value = serde_json::from_str(&existing_row.6)
            .expect("existing Claude metadata should remain valid JSON");
        assert_eq!(existing_metadata.get("local"), Some(&json!(true)));
        assert_eq!(existing_metadata.get("remote"), Some(&json!(true)));
        assert_eq!(existing_metadata.get("remoteOnly"), Some(&json!("new")));
        assert_eq!(existing_metadata.get("manualTitle"), Some(&json!(true)));
        assert_eq!(
            existing_metadata.get("manualTitleUpdatedAt"),
            Some(&json!("2026-08-17T09:00:00Z"))
        );

        let active_existing_row: (String, String, String, Option<String>, String, String, String, i64, i64, Option<i64>, Option<String>, Option<String>, Option<String>, i64) = conn
            .query_row(
                "SELECT title, model_id, status, archived_at, created_at, last_activity_at,
                        COALESCE(engine_metadata_json, ''), message_count, total_tokens,
                        plan_mode, send_method, reasoning_effort, permission_mode,
                        claude_sync_required
                 FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = 'claude-existing-active'",
                params![workspace.id],
                |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?,
                )),
            )
            .expect("failed to read active existing Claude session");
        assert_eq!(active_existing_row.0, "remote-active-title");
        assert_eq!(active_existing_row.1, "local-active-model");
        assert_eq!(active_existing_row.2, "idle");
        assert_eq!(active_existing_row.3, None);
        assert_eq!(active_existing_row.4, "2026-08-15T10:00:00Z");
        assert_eq!(
            active_existing_row.5,
            runtime_env::normalize_time_to_local("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(active_existing_row.7, 7);
        assert_eq!(active_existing_row.8, 84);
        assert_eq!(active_existing_row.9, Some(0));
        assert_eq!(active_existing_row.10.as_deref(), Some("auto"));
        assert_eq!(active_existing_row.11.as_deref(), Some("medium"));
        assert_eq!(active_existing_row.12.as_deref(), Some("safe"));
        assert_eq!(active_existing_row.13, 1);
        let active_metadata: Value = serde_json::from_str(&active_existing_row.6)
            .expect("active Claude metadata should remain valid JSON");
        assert_eq!(active_metadata.get("local"), Some(&json!(true)));
        assert_eq!(active_metadata.get("active"), Some(&json!(true)));
        assert_eq!(active_metadata.get("remoteActive"), Some(&json!(true)));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'claude'",
                params![workspace.id],
                |row| row.get(0),
            )
            .expect("failed to count mixed Claude sessions");
        assert_eq!(count, 3);
    }

    /// 验证 Claude RFC3339 时间比较、边界处理和数据库时间修复。
    #[tokio::test]
    async fn claude_refresh_compares_rfc3339_times_and_repairs_database_time() {
        let (db, workspace) = test_database_and_workspace();
        let conn = db.connect().expect("failed to connect test database");
        for (thread_id, last_activity_at, metadata) in [
            ("claude-equal-zone", Some("2026-08-18T10:00:00Z"), "{\"keep\":true}"),
            ("claude-older", Some("2026-08-18T10:00:00Z"), "{\"keep\":true}"),
            ("claude-millisecond", Some("2026-08-18T10:00:00.000Z"), "{\"keep\":true}"),
            ("claude-invalid-db", Some("not-a-time"), "{\"keep\":true}"),
            ("claude-empty-db", Some(""), "{\"keep\":true}"),
            ("claude-invalid-remote", Some("2026-08-18T10:00:00Z"), "{\"keep\":true}"),
            ("claude-none-remote", Some("2026-08-18T10:00:00Z"), "{\"keep\":true}"),
            ("claude-empty-remote", Some("2026-08-18T10:00:00Z"), "{\"keep\":true}"),
        ] {
            conn.execute(
                "INSERT INTO threads (
                     id, workspace_id, engine_id, model_id, engine_thread_id,
                     engine_metadata_json, title, status, created_at, last_activity_at
                 ) VALUES (?1, ?2, 'claude', 'local-model', ?3, ?4, 'local-title',
                           'completed', '2026-08-16T10:00:00Z', ?5)",
                params![Uuid::new_v4().to_string(), workspace.id, thread_id, metadata, last_activity_at],
            )
            .expect("failed to insert time comparison fixture");
        }
        drop(conn);

        let mut equal_zone = test_snapshot("claude-equal-zone", "remote-equal");
        equal_zone.updated_at = Some("2026-08-18T12:00:00+02:00".to_string());
        let mut older = test_snapshot("claude-older", "remote-older");
        older.updated_at = Some("2026-08-18T09:59:59Z".to_string());
        let mut millisecond = test_snapshot("claude-millisecond", "remote-millisecond");
        millisecond.updated_at = Some("2026-08-18T10:00:00.001Z".to_string());
        let mut invalid_db = test_snapshot("claude-invalid-db", "remote-invalid-db");
        invalid_db.updated_at = Some("2026-08-18T11:00:00Z".to_string());
        let mut empty_db = test_snapshot("claude-empty-db", "remote-empty-db");
        empty_db.updated_at = Some("2026-08-18T11:00:00Z".to_string());
        let mut invalid_remote = test_snapshot("claude-invalid-remote", "remote-invalid");
        invalid_remote.updated_at = Some("not-a-time".to_string());
        let mut none_remote = test_snapshot("claude-none-remote", "remote-none");
        none_remote.updated_at = None;
        let mut empty_remote = test_snapshot("claude-empty-remote", "remote-empty");
        empty_remote.updated_at = Some("   ".to_string());
        ClaudeCodeCli::persist_session_snapshots(Arc::new(db.clone()), &workspace, vec![
                equal_zone,
                older,
                millisecond,
                invalid_db,
                empty_db,
                invalid_remote,
                none_remote,
                empty_remote,
            ],
        )
        .await
        .expect("Claude time comparison should succeed");

        let conn = db.connect().expect("failed to read time comparison fixtures");
        let read_last_activity = |thread_id: &str| -> String {
            conn.query_row(
                "SELECT last_activity_at FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = ?2",
                params![workspace.id, thread_id],
                |row| row.get(0),
            )
            .expect("failed to read last activity timestamp")
        };
        let read_sync_flag = |thread_id: &str| -> i64 {
            conn.query_row(
                "SELECT claude_sync_required FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = ?2",
                params![workspace.id, thread_id],
                |row| row.get(0),
            )
            .expect("failed to read Claude sync flag")
        };
        assert_eq!(
            runtime_env::parse_persisted_time_to_utc(&read_last_activity("claude-equal-zone"))
                .unwrap(),
            runtime_env::parse_persisted_time_to_utc("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(
            runtime_env::parse_persisted_time_to_utc(&read_last_activity("claude-older"))
                .unwrap(),
            runtime_env::parse_persisted_time_to_utc("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(
            read_last_activity("claude-millisecond"),
            runtime_env::normalize_time_to_local("2026-08-18T10:00:00.001Z").unwrap()
        );
        assert_eq!(
            read_last_activity("claude-invalid-db"),
            runtime_env::normalize_time_to_local("2026-08-18T11:00:00Z").unwrap()
        );
        assert_eq!(
            read_last_activity("claude-empty-db"),
            runtime_env::normalize_time_to_local("2026-08-18T11:00:00Z").unwrap()
        );
        assert_eq!(
            runtime_env::parse_persisted_time_to_utc(&read_last_activity("claude-invalid-remote"))
                .unwrap(),
            runtime_env::parse_persisted_time_to_utc("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(
            runtime_env::parse_persisted_time_to_utc(&read_last_activity("claude-none-remote"))
                .unwrap(),
            runtime_env::parse_persisted_time_to_utc("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(
            runtime_env::parse_persisted_time_to_utc(&read_last_activity("claude-empty-remote"))
                .unwrap(),
            runtime_env::parse_persisted_time_to_utc("2026-08-18T10:00:00Z").unwrap()
        );
        assert_eq!(read_sync_flag("claude-equal-zone"), 0);
        assert_eq!(read_sync_flag("claude-older"), 0);
        assert_eq!(read_sync_flag("claude-millisecond"), 1);
        assert_eq!(read_sync_flag("claude-invalid-db"), 1);
        assert_eq!(read_sync_flag("claude-empty-db"), 1);
        assert_eq!(read_sync_flag("claude-invalid-remote"), 0);
        assert_eq!(read_sync_flag("claude-none-remote"), 0);
        assert_eq!(read_sync_flag("claude-empty-remote"), 0);
    }

    /// 验证 Claude 刷新保留无效 metadata 原文并更新活动时间。
    #[tokio::test]
    async fn claude_refresh_preserves_invalid_existing_metadata() {
        let (db, workspace) = test_database_and_workspace();
        let conn = db.connect().expect("failed to connect test database");
        conn.execute(
            "INSERT INTO threads (
                 id, workspace_id, engine_id, model_id, engine_thread_id,
                 engine_metadata_json, title, status, created_at, last_activity_at
             ) VALUES (?1, ?2, 'claude', 'local-model', 'claude-invalid-metadata',
                       ?3, 'local-title', 'completed', ?4, ?5)",
            params![
                "invalid-metadata-thread",
                workspace.id,
                "not-json",
                "2026-08-16T10:00:00Z",
                "2026-08-16T11:00:00Z",
            ],
        )
        .expect("failed to insert invalid metadata fixture");
        drop(conn);

        let mut snapshot = test_snapshot("claude-invalid-metadata", "remote-title");
        snapshot.updated_at = Some("2026-08-18T11:00:00Z".to_string());
        ClaudeCodeCli::persist_session_snapshots(Arc::new(db.clone()), &workspace, vec![snapshot])
            .await
            .expect("Claude invalid metadata refresh should succeed");

        let conn = db.connect().expect("failed to read invalid metadata fixture");
        let row: (String, String, String) = conn
            .query_row(
                "SELECT engine_metadata_json, title, last_activity_at FROM threads
                 WHERE workspace_id = ?1 AND engine_thread_id = 'claude-invalid-metadata'",
                params![workspace.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("failed to read invalid metadata row");
        assert_eq!(row.0, "not-json");
        assert_eq!(row.1, "remote-title");
        assert_eq!(
            row.2,
            runtime_env::normalize_time_to_local("2026-08-18T11:00:00Z").unwrap()
        );
    }

    /// 验证 Codex 刷新仍通过既有 upsert 行为更新会话。
    #[tokio::test]
    async fn non_claude_batch_persistence_keeps_existing_upsert_behavior() {
        let (db, workspace) = test_database_and_workspace();
        crate::cli_tools::codex::persist_session_snapshots(Arc::new(db.clone()), &workspace, vec![test_snapshot("codex-session", "first title")],
        )
        .await
        .expect("Codex import should succeed");
        crate::cli_tools::codex::persist_session_snapshots(Arc::new(db.clone()), &workspace, vec![test_snapshot("codex-session", "updated title")],
        )
        .await
        .expect("Codex refresh should succeed");

        let conn = db.connect().expect("failed to connect test database");
        let (count, title): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(title) FROM threads
                 WHERE workspace_id = ?1 AND engine_id = 'codex'",
                params![workspace.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("failed to read Codex session");
        assert_eq!(count, 1);
        assert_eq!(title, "updated title");
    }

    /// 通过 Codex 生产 parser 验证项目目录过滤。
    #[test]
    fn codex_parser_filters_by_cwd() {
        let value = json!({"thread":{"id":"same","cwd":"/a","createdAt":1}});
        assert!(crate::cli_tools::codex::parse_codex_session(&value, "/b").is_none());
        assert_eq!(
            crate::cli_tools::codex::parse_codex_session(&value, "/a")
                .unwrap()
                .0
                .engine_thread_id,
            "same"
        );
    }

    /// 通过 Codex 生产 parser 验证从预览生成标题。
    #[test]
    fn codex_parser_uses_preview_when_explicit_title_is_missing() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "preview": "第一行标题\n后续内容",
                "createdAt": 1
            }
        });

        let (session, title_needs_read) = crate::cli_tools::codex::parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.title, "第一行标题");
        assert!(!title_needs_read);
    }

    /// 通过 Codex 生产 parser 验证模型与思考强度保留。
    #[test]
    fn codex_parser_keeps_model_and_reasoning_effort() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "createdAt": 1,
                "model": "gpt-5.6-terra",
                "modelProvider": "llm_router",
                "reasoningEffort": "high"
            }
        });

        let (session, _) = crate::cli_tools::codex::parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.model_id, "gpt-5.6-terra");
        assert_eq!(session.reasoning_effort.as_deref(), Some("high"));
        /*
        // 旧实现把思考强度镜像写入 metadata，已停用：
        assert_eq!(
            session.metadata.get("reasoningEffort"),
            Some(&json!("high"))
        );
        */
        assert_eq!(
            session.metadata.get("codexModelProvider"),
            Some(&json!("llm_router"))
        );
    }

    /// 通过 Codex 生产 parser 验证压缩预览触发详情补读。
    #[test]
    fn codex_parser_marks_compacted_preview_for_thread_read() {
        let value = json!({
            "thread": {
                "id": "thread-a",
                "cwd": "/a",
                "preview": "<<ccr:8859c5dfe071,string,279B>>",
                "createdAt": 1
            }
        });

        let (session, title_needs_read) = crate::cli_tools::codex::parse_codex_session(&value, "/a").unwrap();
        assert_eq!(session.title, "thread-a");
        assert!(title_needs_read);
    }
    /// 通过生产映射和刷新过滤器验证 OpenCode 只接受当前项目目录。
    #[test]
    fn opencode_parser_filters_by_cwd() {
        let summary = OpenCodeRemoteSessionSummary {
            engine_thread_id: "same".to_string(),
            title: Some("OpenCode session".to_string()),
            cwd: "/a".to_string(),
            created_at: 1,
            updated_at: 2,
            archived: false,
        };
        let session = OpenCodeCli::map_session(summary, true);
        assert!(session_for_refresh(session.clone(), "/b").is_none());
        assert_eq!(session_for_refresh(session, "/a").unwrap().cwd, "/a");
    }

    /// 通过 Claude 生产 parser 验证目录过滤和远端时间保留。
    #[test]
    fn claude_parser_filters_by_cwd_and_keeps_remote_timestamp() {
        let value = json!({
            "id":"same",
            "cwd":"/a",
            "title":"remote claude",
            "updatedAt":"2026-08-14T09:00:00.000Z"
        });
        assert!(parse_ssh_session(&value, "/b", None).is_none());
        let session = parse_ssh_session(&value, "/a", None).unwrap();
        assert_eq!(session.engine_thread_id, "same");
        assert_eq!(
            session.updated_at.as_deref(),
            Some("2026-08-14T09:00:00.000Z")
        );
    }

    /// 验证本机刷新入口拒绝 SSH workspace。
    #[test]
    fn local_workspace_predicate_rejects_ssh_workspace() {
        let (_db, mut workspace) = test_database_and_workspace();
        workspace.location_kind = "local".to_string();
        assert!(is_local_workspace(&workspace));
        workspace.location_kind = "ssh".to_string();
        assert!(!is_local_workspace(&workspace));
    }
