use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::{
    sync::{Mutex, Notify, RwLock},
    time::sleep,
};

use crate::{
    db::extensions::{
        self as snapshot_db, group_due_refreshes, latest_attempt_timestamp,
        latest_snapshot_timestamp,
    },
    models::{CachedExtensionCatalogDto, ExtensionCatalogRefreshErrorDto, ExtensionItemDto},
    state::AppState,
};

use super::{provider_capabilities, refresh_catalog_kinds, sources_from_items};

pub const EXTENSION_CATALOG_UPDATED_EVENT: &str = "extension-catalog-updated";
const GLOBAL_CONTEXT_KEY: &str = "global";
const CONTEXT_PREFIX: &str = "cwd:";
const SCHEDULER_RECHECK_DELAY: Duration = Duration::from_secs(15);
const EXTENSION_PROVIDER_IDS: [&str; 3] = ["codex", "claude", "opencode"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefreshKey {
    provider_id: String,
    context_key: String,
}

#[derive(Debug, Clone, Default)]
struct RefreshActivity {
    completed_at: Option<String>,
}

#[derive(Default)]
pub struct ExtensionCatalogRefreshManager {
    in_flight: RwLock<HashSet<RefreshKey>>,
    activity: RwLock<HashMap<RefreshKey, RefreshActivity>>,
    scheduled_this_run: RwLock<HashSet<RefreshKey>>,
    // Codex catalog reads share one desktop runtime: skill refresh starts an
    // app-server while plugin/MCP refresh invokes the CLI. The scheduler can
    // refresh multiple workspaces at once, so all Codex catalog reads must be
    // serialized beyond the per-context refresh key.
    codex_catalog_refresh_lock: Mutex<()>,
    scheduler_wakeup: Notify,
}

impl ExtensionCatalogRefreshManager {
    async fn begin(&self, key: RefreshKey) -> bool {
        let started = self.in_flight.write().await.insert(key.clone());
        if started {
            self.activity
                .write()
                .await
                .insert(key, RefreshActivity::default());
        }
        started
    }

    async fn finish(&self, key: &RefreshKey) {
        self.in_flight.write().await.remove(key);
        if let Some(activity) = self.activity.write().await.get_mut(key) {
            activity.completed_at = Some(Utc::now().to_rfc3339());
        }
    }

    async fn schedule_once_per_run(&self, key: RefreshKey) -> bool {
        self.scheduled_this_run.write().await.insert(key)
    }

    async fn forget_scheduled_context(&self, key: &RefreshKey) {
        self.scheduled_this_run.write().await.remove(key);
    }

    pub async fn is_refreshing(&self, provider_id: &str, context_key: &str) -> bool {
        self.in_flight.read().await.contains(&RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        })
    }

    async fn refresh_completed_at(&self, provider_id: &str, context_key: &str) -> Option<String> {
        self.activity
            .read()
            .await
            .get(&RefreshKey {
                provider_id: provider_id.to_string(),
                context_key: context_key.to_string(),
            })
            .and_then(|activity| activity.completed_at.clone())
    }

    fn wake_scheduler(&self) {
        self.scheduler_wakeup.notify_one();
    }

    async fn wait_for_scheduler_wakeup(&self) {
        self.scheduler_wakeup.notified().await;
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionCatalogUpdatedEvent {
    provider_id: String,
    cwd: Option<String>,
}

pub fn context_key(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return GLOBAL_CONTEXT_KEY.to_string();
    };
    let normalized = normalize_cwd(cwd);
    format!("{CONTEXT_PREFIX}{normalized}")
}

pub fn cwd_from_context_key(context_key: &str) -> Option<String> {
    context_key
        .strip_prefix(CONTEXT_PREFIX)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string)
}

pub fn affected_refresh_kinds(kind: &str) -> Vec<String> {
    match kind {
        // Plugin install/remove can add or remove the skills and MCP servers it provides.
        "plugin" => snapshot_db::EXTENSION_KINDS
            .iter()
            .map(|kind| (*kind).to_string())
            .collect(),
        "skill" | "mcp" => vec![kind.to_string()],
        _ => Vec::new(),
    }
}

pub async fn load_cached_catalog(
    state: &AppState,
    provider_id: &str,
    cwd: Option<&str>,
) -> Result<CachedExtensionCatalogDto> {
    validate_provider(provider_id)?;
    let context_key = context_key(cwd);
    // The application starts its scheduler before the frontend knows which
    // workspace the user will enter. Register the actual page context once per
    // process so an existing, non-default workspace is refreshed too. This
    // only writes scheduling metadata and wakes the background task; it never
    // runs a CLI command on the page request.
    schedule_context_once_per_run(state, provider_id, &context_key).await?;
    let snapshots = snapshot_db::load_snapshots(&state.db, provider_id, &context_key)?;
    let has_snapshot = snapshots
        .iter()
        .any(|snapshot| snapshot.fetched_at.is_some());
    // Startup scheduling writes `next_refresh_at = now` before the CLI task is
    // spawned. Treat every due snapshot as refreshing, including snapshots that
    // already contain old data, so the page does not depend on an event race to
    // show the current startup refresh.
    let now = Utc::now();
    let refresh_pending = snapshots.iter().any(|snapshot| {
        snapshot
            .next_refresh_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value.with_timezone(&Utc) <= now)
    });
    let mut items = snapshots
        .iter()
        .filter(|snapshot| snapshot.fetched_at.is_some())
        .flat_map(|snapshot| snapshot.items.clone())
        .collect::<Vec<_>>();
    sort_items(&mut items);

    let mut refresh_errors = snapshots
        .iter()
        .filter_map(|snapshot| {
            snapshot
                .last_error
                .as_ref()
                .map(|code| ExtensionCatalogRefreshErrorDto {
                    kind: snapshot.kind.clone(),
                    code: code.clone(),
                })
        })
        .collect::<Vec<_>>();
    refresh_errors.sort_by(|left, right| left.kind.cmp(&right.kind));

    let next_refresh_at = snapshots
        .iter()
        .filter_map(|snapshot| snapshot.next_refresh_at.as_deref())
        .min()
        .map(str::to_string);
    let kind_fetched_at = snapshots
        .iter()
        .map(|snapshot| (snapshot.kind.clone(), snapshot.fetched_at.clone()))
        .collect::<BTreeMap<_, _>>();
    let is_refreshing = state
        .extension_catalog_refreshes
        .is_refreshing(provider_id, &context_key)
        .await;
    let refresh_completed_at = state
        .extension_catalog_refreshes
        .refresh_completed_at(provider_id, &context_key)
        .await;
    Ok(CachedExtensionCatalogDto {
        provider_id: provider_id.to_string(),
        cwd: cwd.map(str::to_string),
        sources: sources_from_items(&items),
        capabilities: provider_capabilities(provider_id),
        fetched_at: latest_snapshot_timestamp(&snapshots),
        kind_fetched_at,
        last_attempt_at: latest_attempt_timestamp(&snapshots),
        next_refresh_at,
        refreshing: refresh_pending || is_refreshing,
        refresh_completed_at,
        has_snapshot,
        refresh_errors,
        items,
    })
}

pub async fn request_catalog_refresh(
    app: AppHandle,
    state: AppState,
    provider_id: &str,
    cwd: Option<String>,
    requested_kinds: Vec<String>,
) -> Result<bool> {
    validate_provider(provider_id)?;
    let kinds = normalize_kinds(requested_kinds)?;
    let context_key = context_key(cwd.as_deref());
    snapshot_db::ensure_context(
        &state.db,
        provider_id,
        &context_key,
        &Utc::now().to_rfc3339(),
    )?;
    let key = RefreshKey {
        provider_id: provider_id.to_string(),
        context_key,
    };
    if !state.extension_catalog_refreshes.begin(key.clone()).await {
        return Ok(false);
    }

    emit_catalog_updated(&app, provider_id, cwd.as_deref());
    tauri::async_runtime::spawn(run_catalog_refresh(app, state, key, cwd, kinds));
    Ok(true)
}

pub fn spawn_catalog_refresh_scheduler(app: AppHandle, state: AppState) {
    tauri::async_runtime::spawn(async move {
        schedule_startup_refreshes(&state).await;
        loop {
            let now = Utc::now().to_rfc3339();
            match snapshot_db::list_due_refreshes(&state.db, &now) {
                Ok(targets) => {
                    for ((provider_id, context_key), kinds) in group_due_refreshes(targets) {
                        let key = RefreshKey {
                            provider_id,
                            context_key,
                        };
                        if !state.extension_catalog_refreshes.begin(key.clone()).await {
                            continue;
                        }
                        let cwd = cwd_from_context_key(&key.context_key);
                        emit_catalog_updated(&app, &key.provider_id, cwd.as_deref());
                        tauri::async_runtime::spawn(run_catalog_refresh(
                            app.clone(),
                            state.clone(),
                            key,
                            cwd,
                            kinds,
                        ));
                    }
                }
                Err(_) => log::warn!("failed to find scheduled extension catalog refreshes"),
            }
            tokio::select! {
                _ = sleep(next_scheduler_sleep(&state)) => {}
                _ = state.extension_catalog_refreshes.wait_for_scheduler_wakeup() => {}
            }
        }
    });
}

async fn schedule_startup_refreshes(state: &AppState) {
    let workspaces = match crate::db::workspaces::list_workspaces(&state.db) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            log::warn!("failed to list workspaces for startup extension refresh: {error}");
            return;
        }
    };

    for workspace in workspaces {
        if workspace.location_kind != "local" {
            continue;
        }
        if let Err(error) = schedule_workspace_catalog_refresh(state, &workspace.id).await {
            log::warn!(
                "failed to schedule startup extension refresh for workspace {}: {error}",
                workspace.id
            );
        }
    }
}

/// Queues the active workspace's catalogs without running a CLI command on the
/// IPC caller. The scheduler owns execution and emits refresh events when it
/// begins each queued job.
pub async fn schedule_workspace_catalog_refresh(
    state: &AppState,
    workspace_id: &str,
) -> Result<()> {
    let workspace = crate::db::workspaces::find_workspace_by_id(&state.db, workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
    // SSH 扩展目录由当前 CLI 在当前目标上即时读取。这里的 SQLite 快照 key 只包含 cwd，
    // 不能用于隔离不同 SSH 机器上同名项目的目录。
    if workspace.location_kind == "ssh" {
        return Ok(());
    }
    let context_key = context_key(Some(&workspace.root_path));
    let mut first_error = None;

    for provider_id in EXTENSION_PROVIDER_IDS {
        if let Err(error) = schedule_context_once_per_run(state, provider_id, &context_key).await {
            log::warn!(
                "failed to schedule extension refresh for workspace={} provider={provider_id}: {error}",
                workspace.id
            );
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn schedule_context_once_per_run(
    state: &AppState,
    provider_id: &str,
    context_key: &str,
) -> Result<bool> {
    let key = RefreshKey {
        provider_id: provider_id.to_string(),
        context_key: context_key.to_string(),
    };
    let is_first_schedule = state
        .extension_catalog_refreshes
        .schedule_once_per_run(key.clone())
        .await;
    if !is_first_schedule {
        return Ok(false);
    }

    let observed_at = Utc::now().to_rfc3339();
    let schedule_result = (|| {
        snapshot_db::ensure_context(&state.db, provider_id, context_key, &observed_at)?;
        for kind in snapshot_db::EXTENSION_KINDS {
            snapshot_db::schedule_startup_refresh(
                &state.db,
                provider_id,
                context_key,
                kind,
                &observed_at,
            )?;
        }
        Ok(())
    })();

    if let Err(error) = schedule_result {
        state
            .extension_catalog_refreshes
            .forget_scheduled_context(&key)
            .await;
        return Err(error);
    }

    state.extension_catalog_refreshes.wake_scheduler();
    Ok(true)
}

async fn run_catalog_refresh(
    app: AppHandle,
    state: AppState,
    key: RefreshKey,
    cwd: Option<String>,
    requested_kinds: Vec<String>,
) {
    for kind in requested_kinds {
        let attempted_at = Utc::now().to_rfc3339();
        let refresh_result = if requires_codex_catalog_refresh_lock(&key.provider_id) {
            let _guard = state
                .extension_catalog_refreshes
                .codex_catalog_refresh_lock
                .lock()
                .await;
            refresh_catalog_kinds(&state, &key.provider_id, cwd.as_deref(), &[kind.clone()]).await
        } else {
            refresh_catalog_kinds(&state, &key.provider_id, cwd.as_deref(), &[kind.clone()]).await
        };
        let write_result = match refresh_result {
            Ok(results) => match results.into_iter().next() {
                Some(result) if result.success => snapshot_db::record_success(
                    &state.db,
                    &key.provider_id,
                    &key.context_key,
                    &kind,
                    &result.items,
                    &attempted_at,
                ),
                _ => snapshot_db::record_failure(
                    &state.db,
                    &key.provider_id,
                    &key.context_key,
                    &kind,
                    &attempted_at,
                    "refresh_failed",
                )
                .map(|_| ()),
            },
            Err(_) => snapshot_db::record_failure(
                &state.db,
                &key.provider_id,
                &key.context_key,
                &kind,
                &attempted_at,
                "refresh_failed",
            )
            .map(|_| ()),
        };
        if write_result.is_err() {
            log::warn!(
                "failed to persist extension catalog refresh state for provider={} kind={}",
                key.provider_id,
                kind
            );
        }
        emit_catalog_updated(&app, &key.provider_id, cwd.as_deref());
    }

    state.extension_catalog_refreshes.finish(&key).await;
    emit_catalog_updated(&app, &key.provider_id, cwd.as_deref());
}

fn emit_catalog_updated(app: &AppHandle, provider_id: &str, cwd: Option<&str>) {
    if app
        .emit(
            EXTENSION_CATALOG_UPDATED_EVENT,
            ExtensionCatalogUpdatedEvent {
                provider_id: provider_id.to_string(),
                cwd: cwd.map(str::to_string),
            },
        )
        .is_err()
    {
        log::warn!("failed to emit extension catalog update event");
    }
}

fn normalize_kinds(requested_kinds: Vec<String>) -> Result<Vec<String>> {
    let kinds = if requested_kinds.is_empty() {
        snapshot_db::EXTENSION_KINDS
            .iter()
            .map(|kind| (*kind).to_string())
            .collect::<Vec<_>>()
    } else {
        requested_kinds
    };
    let kinds = kinds
        .into_iter()
        .map(|kind| kind.trim().to_string())
        .collect::<BTreeSet<_>>();
    if kinds.is_empty() || kinds.iter().any(|kind| !is_supported_kind(kind)) {
        anyhow::bail!("unsupported extension refresh kind");
    }
    Ok(snapshot_db::EXTENSION_KINDS
        .iter()
        .filter(|kind| kinds.contains(**kind))
        .map(|kind| (*kind).to_string())
        .collect())
}

fn validate_provider(provider_id: &str) -> Result<()> {
    if matches!(provider_id, "codex" | "claude" | "opencode") {
        return Ok(());
    }
    anyhow::bail!("unsupported extension provider")
}

fn is_supported_kind(kind: &str) -> bool {
    snapshot_db::EXTENSION_KINDS.contains(&kind)
}

fn requires_codex_catalog_refresh_lock(provider_id: &str) -> bool {
    provider_id == "codex"
}

fn normalize_cwd(cwd: &str) -> String {
    let mut normalized = cwd.replace('\\', "/");
    while normalized.ends_with('/') && normalized.len() > 1 {
        if normalized.len() == 3 && normalized.as_bytes().get(1) == Some(&b':') {
            break;
        }
        normalized.pop();
    }
    #[cfg(target_os = "windows")]
    {
        normalized.make_ascii_lowercase();
    }
    normalized
}

fn sort_items(items: &mut [ExtensionItemDto]) {
    items.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

fn next_scheduler_sleep(state: &AppState) -> Duration {
    let Ok(Some(next_refresh_at)) = snapshot_db::next_refresh_at(&state.db) else {
        // New contexts notify the scheduler explicitly. With no contexts there
        // is no CLI work to perform, so avoid an unnecessary polling loop.
        return Duration::from_secs(6 * 60 * 60);
    };
    let Ok(next_refresh_at) = DateTime::parse_from_rfc3339(&next_refresh_at) else {
        return SCHEDULER_RECHECK_DELAY;
    };
    let delay = next_refresh_at.with_timezone(&Utc) - Utc::now();
    delay
        .to_std()
        .unwrap_or(SCHEDULER_RECHECK_DELAY)
        .max(SCHEDULER_RECHECK_DELAY)
}

#[cfg(test)]
mod tests {
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
                crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db),
            ),
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
            assert!(snapshots.iter().all(|snapshot| snapshot
                .next_refresh_at
                .as_deref()
                .is_some_and(|value| value <= Utc::now().to_rfc3339().as_str())));
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
}
