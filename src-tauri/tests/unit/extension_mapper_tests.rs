
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::db::Database;

    /// 创建使用唯一文件路径的扩展测试数据库，避免迁移备份目标发生冲突。
    fn test_database() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-extensions-{}.db", Uuid::new_v4()));
        Database::open(path).expect("failed to open test database")
    }

    #[test]
    fn retry_delay_matches_documented_backoff() {
        assert_eq!(retry_delay(1), Duration::minutes(1));
        assert_eq!(retry_delay(2), Duration::minutes(30));
        assert_eq!(retry_delay(3), Duration::hours(1));
        assert_eq!(retry_delay(4), Duration::hours(6));
    }

    #[test]
    fn failed_kind_preserves_previous_successful_snapshot() {
        let db = test_database();
        let now = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &now).unwrap();
        record_success(&db, "codex", "workspace:/demo", "mcp", &[], &now).unwrap();
        record_failure(
            &db,
            "codex",
            "workspace:/demo",
            "mcp",
            &Utc::now().to_rfc3339(),
            "MCP refresh failed.",
        )
        .unwrap();

        let snapshot = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "mcp")
            .unwrap();
        assert!(snapshot.fetched_at.is_some());
        assert_eq!(snapshot.last_error.as_deref(), Some("MCP refresh failed."));
    }

    #[test]
    fn context_registration_does_not_overwrite_existing_snapshots() {
        let db = test_database();
        let now = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &now).unwrap();
        record_success(&db, "codex", "workspace:/demo", "skill", &[], &now).unwrap();
        ensure_context(&db, "codex", "workspace:/demo", &Utc::now().to_rfc3339()).unwrap();

        let snapshots = load_snapshots(&db, "codex", "workspace:/demo").unwrap();
        assert_eq!(snapshots.len(), EXTENSION_KINDS.len());
        assert!(snapshots
            .iter()
            .find(|snapshot| snapshot.kind == "skill")
            .and_then(|snapshot| snapshot.fetched_at.as_ref())
            .is_some());
    }

    #[test]
    fn startup_refresh_reschedules_success_and_failure() {
        let db = test_database();
        let first_attempt = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &first_attempt).unwrap();
        record_success(
            &db,
            "codex",
            "workspace:/demo",
            "skill",
            &[],
            &first_attempt,
        )
        .unwrap();

        let startup_at = "2030-01-02T03:04:05+00:00";
        let expected_startup_at = runtime_env::normalize_time_to_local(startup_at).unwrap();
        schedule_startup_refresh(&db, "codex", "workspace:/demo", "skill", startup_at).unwrap();
        let scheduled = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "skill")
            .unwrap();
        assert_eq!(
            scheduled.next_refresh_at.as_deref(),
            Some(expected_startup_at.as_str())
        );

        record_failure(
            &db,
            "codex",
            "workspace:/demo",
            "skill",
            &Utc::now().to_rfc3339(),
            "refresh_failed",
        )
        .unwrap();
        let restarted_at = "2030-01-02T04:05:06+00:00";
        let expected_restarted_at = runtime_env::normalize_time_to_local(restarted_at).unwrap();
        schedule_startup_refresh(&db, "codex", "workspace:/demo", "skill", restarted_at).unwrap();
        let after_restart = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "skill")
            .and_then(|snapshot| snapshot.next_refresh_at)
            .unwrap();
        assert_eq!(after_restart, expected_restarted_at);
    }
