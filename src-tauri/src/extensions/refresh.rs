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
    runtime_env,
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
            activity.completed_at = Some(runtime_env::system_time_rfc3339());
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
        &runtime_env::system_time_rfc3339(),
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
            let now = runtime_env::system_time_rfc3339();
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

    let observed_at = runtime_env::system_time_rfc3339();
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
        let attempted_at = runtime_env::system_time_rfc3339();
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
#[path = "../../tests/unit/extension_refresh_tests.rs"]
mod tests;
