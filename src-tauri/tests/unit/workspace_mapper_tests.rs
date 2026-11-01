
    use std::sync::{Arc, Mutex};

    use uuid::Uuid;

    use crate::db::{ConnectionPool, SQLITE_POOL_MAX_IDLE};

    use super::*;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-workspaces-{}.db", Uuid::new_v4()));
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

    fn insert_test_connection(db: &Database, id: &str, display_name: &str) {
        let conn = db.connect().expect("failed to open test connection");
        conn.execute(
            "INSERT INTO ssh_connections (
                id, display_name, source_kind, host_name, user_name, port,
                host_key_type, host_key_base64, connection_status
             ) VALUES (?1, ?2, 'manual', '192.0.2.10', 'tester', 22, 'ssh-ed25519', 'test-key', 'ok')",
            params![id, display_name],
        )
        .expect("failed to insert test ssh connection");
    }

    #[test]
    fn remote_workspace_keeps_identity_and_hides_only_when_connection_is_deleted() {
        let db = test_db();
        insert_test_connection(&db, "ssh-a", "Remote A");
        insert_test_connection(&db, "ssh-b", "Remote B");

        let first = create_ssh_workspace(&db, "ssh-a", "Repo", "/home/tester/Repo")
            .expect("failed to create remote workspace");
        assert_eq!(first.location_kind, "ssh");
        assert_eq!(first.ssh_connection_id.as_deref(), Some("ssh-a"));
        assert_eq!(first.root_path, "/home/tester/Repo");

        let duplicate = create_ssh_workspace(&db, "ssh-a", "Repo 2", "/home/tester/Repo")
            .expect("an existing remote workspace should be reused");
        assert_eq!(duplicate.id, first.id);

        let same_path_other_host = create_ssh_workspace(&db, "ssh-b", "Repo", "/home/tester/Repo")
            .expect("same path on another host should be allowed");
        assert_ne!(first.id, same_path_other_host.id);

        archive_workspace(&db, &first.id).expect("failed to archive remote workspace");
        let restored = create_ssh_workspace(&db, "ssh-a", "Renamed", "/home/tester/Repo")
            .expect("failed to restore archived remote workspace");
        assert_eq!(restored.id, first.id);
        assert_eq!(restored.name, "Renamed");

        crate::db::ssh_connections::soft_delete(&db, "ssh-a")
            .expect("failed to soft delete test connection");
        let visible_after_delete = list_workspaces(&db).expect("failed to list workspaces");
        assert!(visible_after_delete
            .iter()
            .all(|workspace| workspace.id != first.id));

        crate::db::ssh_connections::restore(&db, "ssh-a").expect("failed to restore connection");
        let visible_after_restore = list_workspaces(&db).expect("failed to list workspaces");
        assert!(visible_after_restore
            .iter()
            .any(|workspace| workspace.id == first.id));

        crate::db::ssh_connections::set_enabled(&db, "ssh-a", false)
            .expect("failed to disable test connection");
        let disabled = list_workspaces(&db)
            .expect("failed to list workspaces after disabling connection")
            .into_iter()
            .find(|workspace| workspace.id == first.id)
            .expect("disabled connection project should remain visible");
        assert_eq!(disabled.connection_enabled, Some(false));
    }

    #[test]
    fn lists_every_workspace_bound_to_an_ssh_connection() {
        let db = test_db();
        insert_test_connection(&db, "ssh-a", "Remote A");
        insert_test_connection(&db, "ssh-b", "Remote B");

        let first = create_ssh_workspace(&db, "ssh-a", "Repo A", "/srv/repo-a")
            .expect("failed to create first remote workspace");
        let second = create_ssh_workspace(&db, "ssh-a", "Repo B", "/srv/repo-b")
            .expect("failed to create second remote workspace");
        create_ssh_workspace(&db, "ssh-b", "Repo C", "/srv/repo-c")
            .expect("failed to create unrelated remote workspace");

        let mut workspace_ids = workspace_ids_for_ssh_connection(&db, "ssh-a")
            .expect("failed to list workspaces for connection");
        workspace_ids.sort();
        let mut expected = vec![first.id, second.id];
        expected.sort();
        assert_eq!(workspace_ids, expected);
    }

    #[test]
    fn upsert_workspace_preserves_identity_and_uses_standard_trust_by_default() {
        let db = test_db();
        let root = std::env::temp_dir().join(format!("auracoder-workspace-{}", Uuid::new_v4()));
        let root = root.to_string_lossy().to_string();

        let created = upsert_workspace(&db, &root).expect("failed to create workspace");
        let reopened = upsert_workspace(&db, &root).expect("failed to reopen workspace");

        assert_eq!(created.id, reopened.id);
        assert_eq!(reopened.trust_level, crate::models::TrustLevelDto::Standard);
    }
