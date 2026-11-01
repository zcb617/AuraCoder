    use std::{fs, sync::Arc};

    use super::*;
    use crate::{
        config::app_config::AppConfig,
        db::Database,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        power::KeepAwakeManager,
        state::TurnManager,
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    use uuid::Uuid;

    fn test_app_state() -> AppState {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root is missing")
            .join(".tmp")
            .join(format!("auracoder-extension-cache-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create test root");
        let db =
            Database::open(root.join("workspaces.db")).expect("failed to create test database");
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
            extension_catalog_refreshes: Arc::new(ExtensionCatalogRefreshManager::default()),
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

    #[tokio::test]
    async fn startup_refresh_keeps_an_empty_workspace_list_empty() {
        let state = test_app_state();

        schedule_startup_refreshes(&state).await;

        assert!(
            crate::db::workspaces::list_workspaces(&state.db)
                .expect("failed to list workspaces")
                .is_empty(),
            "startup extension refresh must not create a workspace",
        );
    }

    #[test]
    fn context_key_normalizes_windows_separators_without_losing_drive_root() {
        assert_eq!(context_key(None), "global");
        assert_eq!(context_key(Some("C:\\")), "cwd:c:/");
        assert_eq!(context_key(Some("C:\\work\\repo\\")), "cwd:c:/work/repo");
    }

    #[test]
    fn requested_kinds_are_deduplicated_in_stable_order() {
        assert_eq!(
            normalize_kinds(vec![
                "mcp".to_string(),
                "skill".to_string(),
                "mcp".to_string()
            ])
            .unwrap(),
            vec!["skill", "mcp"]
        );
        assert!(normalize_kinds(vec!["unknown".to_string()]).is_err());
        assert_eq!(
            affected_refresh_kinds("plugin"),
            vec!["skill", "plugin", "mcp"]
        );
    }

    #[test]
    fn serializes_all_codex_catalog_refresh_kinds() {
        assert!(requires_codex_catalog_refresh_lock("codex"));
        assert!(!requires_codex_catalog_refresh_lock("claude"));
        assert!(!requires_codex_catalog_refresh_lock("opencode"));
    }

    #[tokio::test]
    async fn codex_catalog_refresh_lock_is_shared_across_refresh_contexts() {
        let manager = ExtensionCatalogRefreshManager::default();
        let first = manager.codex_catalog_refresh_lock.lock().await;
        assert!(manager.codex_catalog_refresh_lock.try_lock().is_err());
        drop(first);
        assert!(manager.codex_catalog_refresh_lock.try_lock().is_ok());
    }

    #[tokio::test]
    async fn cache_read_registers_an_empty_context_without_a_snapshot() {
        let state = test_app_state();
        let catalog = load_cached_catalog(&state, "codex", Some("C:\\work\\project"))
            .await
            .expect("failed to read empty extension catalog cache");

        assert!(!catalog.has_snapshot);
        assert!(catalog.items.is_empty());
        assert!(catalog.refreshing);
        assert_eq!(
            snapshot_db::load_snapshots(
                &state.db,
                "codex",
                &context_key(Some("C:\\work\\project"))
            )
            .unwrap()
            .len(),
            snapshot_db::EXTENSION_KINDS.len(),
        );
        tokio::time::timeout(
            Duration::from_millis(50),
            state
                .extension_catalog_refreshes
                .wait_for_scheduler_wakeup(),
        )
        .await
        .expect("registering a context should wake the background scheduler");
    }

    /// 验证首次读取全局目录时注册三个扩展类型，并立即保留后台刷新状态。
    #[tokio::test]
    async fn cache_read_registers_global_context_without_a_snapshot() {
        let state = test_app_state();
        let catalog = load_cached_catalog(&state, "codex", None)
            .await
            .expect("failed to read empty global extension catalog cache");

        assert_eq!(catalog.cwd, None);
        assert!(!catalog.has_snapshot);
        assert!(catalog.items.is_empty());
        assert!(catalog.refreshing);
        assert_eq!(
            snapshot_db::load_snapshots(&state.db, "codex", &context_key(None))
                .unwrap()
                .len(),
            snapshot_db::EXTENSION_KINDS.len(),
        );
        tokio::time::timeout(
            Duration::from_millis(50),
            state
                .extension_catalog_refreshes
                .wait_for_scheduler_wakeup(),
        )
        .await
        .expect("registering the global context should wake the background scheduler");
    }

    /// 验证全局目录已有旧快照时仍返回旧数据，同时报告后台刷新状态。
    #[tokio::test]
    async fn cache_read_preserves_global_snapshot_while_refreshing() {
        let state = test_app_state();
        let context_key = context_key(None);
        let attempted_at = Utc::now().to_rfc3339();
        snapshot_db::ensure_context(&state.db, "codex", &context_key, &attempted_at)
            .expect("failed to register global cache context");
        let previous_item = ExtensionItemDto {
            id: "global-skill".to_string(),
            provider_id: "codex".to_string(),
            kind: "skill".to_string(),
            name: "Global Skill".to_string(),
            installed: Some(true),
            ..Default::default()
        };
        snapshot_db::record_success(
            &state.db,
            "codex",
            &context_key,
            "skill",
            &[previous_item],
            &attempted_at,
        )
        .expect("failed to save global cache snapshot");

        let catalog = load_cached_catalog(&state, "codex", None)
            .await
            .expect("failed to read global extension catalog snapshot");

        assert_eq!(catalog.cwd, None);
        assert!(catalog.has_snapshot);
        assert!(catalog.refreshing);
        assert_eq!(catalog.items.len(), 1);
        assert_eq!(catalog.items[0].id, "global-skill");
    }

    #[tokio::test]
    async fn cache_read_schedules_existing_context_once_per_run() {
        let state = test_app_state();
        let cwd = "C:\\work\\project";
        let context_key = context_key(Some(cwd));
        let attempted_at = Utc::now().to_rfc3339();
        snapshot_db::ensure_context(&state.db, "codex", &context_key, &attempted_at)
            .expect("failed to register cache context");
        for kind in snapshot_db::EXTENSION_KINDS {
            snapshot_db::record_success(&state.db, "codex", &context_key, kind, &[], &attempted_at)
                .expect("failed to save cached snapshot");
        }

        let catalog = load_cached_catalog(&state, "codex", Some(cwd))
            .await
            .expect("failed to read scheduled extension catalog cache");

        assert!(catalog.has_snapshot);
        assert!(catalog.refreshing);
        assert!(catalog
            .kind_fetched_at
            .values()
            .all(|fetched_at| fetched_at.is_some()));
    }

    #[tokio::test]
    async fn active_workspace_schedule_uses_that_workspace_context() {
        let state = test_app_state();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root is missing")
            .join(".tmp")
            .join(format!(
                "auracoder-active-extension-workspace-{}",
                Uuid::new_v4()
            ));
        fs::create_dir_all(&root).expect("failed to create active workspace root");
        let workspace = crate::db::workspaces::upsert_workspace(&state.db, &root.to_string_lossy())
            .expect("failed to create active workspace");

        schedule_workspace_catalog_refresh(&state, &workspace.id)
            .await
            .expect("failed to schedule active workspace catalogs");

        let context_key = context_key(Some(&workspace.root_path));
        for provider_id in EXTENSION_PROVIDER_IDS {
            let snapshots = snapshot_db::load_snapshots(&state.db, provider_id, &context_key)
                .expect("failed to load scheduled snapshots");
            assert_eq!(snapshots.len(), snapshot_db::EXTENSION_KINDS.len());
            for snapshot in snapshots {
                let next_refresh_at = snapshot
                    .next_refresh_at
                    .as_deref()
                    .expect("scheduled snapshot should have next_refresh_at");
                let next_refresh_at_utc = runtime_env::parse_persisted_time_to_utc(next_refresh_at)
                    .expect("persisted next_refresh_at should be parseable");
                assert!(next_refresh_at_utc <= Utc::now());
            }
        }
    }

    #[tokio::test]
    async fn catalog_keeps_this_process_refresh_completion_after_the_page_misses_events() {
        let state = test_app_state();
        let cwd = "C:\\work\\project";
        let key = RefreshKey {
            provider_id: "codex".to_string(),
            context_key: context_key(Some(cwd)),
        };
        assert!(state.extension_catalog_refreshes.begin(key.clone()).await);
        state.extension_catalog_refreshes.finish(&key).await;

        let catalog = load_cached_catalog(&state, "codex", Some(cwd))
            .await
            .expect("failed to read catalog completion state");

        assert!(catalog.refresh_completed_at.is_some());
    }
