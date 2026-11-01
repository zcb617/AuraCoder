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
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliLocationKind},
    models::{
        CachedExtensionCatalogDto, ExtensionCatalogRefreshErrorDto, ExtensionCatalogSnapshot,
        ExtensionItemDto, ExtensionRefreshTarget, EXTENSION_KINDS,
    },
    runtime_env,
    services::workspace_service::WorkspaceService,
    state::AppState,
};

use super::{provider_capabilities, sources_from_items};

pub const EXTENSION_CATALOG_UPDATED_EVENT: &str = "extension-catalog-updated";
const GLOBAL_CONTEXT_KEY: &str = "global";
const CONTEXT_PREFIX: &str = "cwd:";
const SCHEDULER_RECHECK_DELAY: Duration = Duration::from_secs(15);

/// 扩展刷新运行状态的 provider 与上下文索引。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefreshKey {
    /// 执行扩展目录刷新的 CLI provider。
    provider_id: String,
    /// 扩展目录缓存关联的工作区或全局上下文。
    context_key: String,
}

/// 单个扩展目录上下文最近一次刷新的活动信息。
#[derive(Debug, Clone, Default)]
struct RefreshActivity {
    /// 最近一次后台刷新结束时间。
    completed_at: Option<String>,
}

/// 管理扩展目录刷新排队、并发状态和调度唤醒。
#[derive(Default)]
pub struct ExtensionCatalogRefreshManager {
    /// 当前正在刷新的 provider 与上下文集合。
    in_flight: RwLock<HashSet<RefreshKey>>,
    /// 各 provider 与上下文最近一次刷新完成时间。
    activity: RwLock<HashMap<RefreshKey, RefreshActivity>>,
    /// 当前进程已经登记过首次刷新的 provider 与上下文集合。
    scheduled_this_run: RwLock<HashSet<RefreshKey>>,
    // Codex catalog reads share one desktop runtime: skill refresh starts an
    // app-server while plugin/MCP refresh invokes the CLI. The scheduler can
    // refresh multiple workspaces at once, so all Codex catalog reads must be
    // serialized beyond the per-context refresh key.
    codex_catalog_refresh_lock: Mutex<()>,
    /// 通知后台刷新调度循环重新读取到期任务。
    scheduler_wakeup: Notify,
}

impl ExtensionCatalogRefreshManager {
    /// 标记一个 provider 与上下文正在刷新，避免同一上下文并发运行。
    pub(crate) async fn begin(&self, provider_id: &str, context_key: &str) -> bool {
        let key = RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        };
        let started = self.in_flight.write().await.insert(key.clone());
        if started {
            self.activity
                .write()
                .await
                .insert(key, RefreshActivity::default());
        }
        started
    }

    /// 清除刷新中状态并记录本次刷新结束时间。
    pub(crate) async fn finish(&self, provider_id: &str, context_key: &str) {
        let key = RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        };
        self.in_flight.write().await.remove(&key);
        if let Some(activity) = self.activity.write().await.get_mut(&key) {
            activity.completed_at = Some(runtime_env::system_time_rfc3339());
        }
    }

    /// 标记当前进程只为该 provider 与上下文安排过一次刷新。
    pub(crate) async fn schedule_once_per_run(
        &self,
        provider_id: &str,
        context_key: &str,
    ) -> bool {
        let key = RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        };
        self.scheduled_this_run.write().await.insert(key)
    }

    /// 在刷新排期失败后清除本进程的上下文排期标记。
    pub(crate) async fn forget_scheduled_context(&self, provider_id: &str, context_key: &str) {
        let key = RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        };
        self.scheduled_this_run.write().await.remove(&key);
    }

    /// 查询指定 provider 与上下文是否正在刷新。
    pub async fn is_refreshing(&self, provider_id: &str, context_key: &str) -> bool {
        self.in_flight.read().await.contains(&RefreshKey {
            provider_id: provider_id.to_string(),
            context_key: context_key.to_string(),
        })
    }

    /// 查询指定 provider 与上下文最近一次后台刷新完成时间。
    pub(crate) async fn refresh_completed_at(
        &self,
        provider_id: &str,
        context_key: &str,
    ) -> Option<String> {
        self.activity
            .read()
            .await
            .get(&RefreshKey {
                provider_id: provider_id.to_string(),
                context_key: context_key.to_string(),
            })
            .and_then(|activity| activity.completed_at.clone())
    }

    /// 唤醒后台刷新调度循环检查新排期。
    fn wake_scheduler(&self) {
        self.scheduler_wakeup.notify_one();
    }

    /// 通知扩展刷新调度器检查新排入的上下文。
    pub(crate) fn wake_catalog_scheduler(&self) {
        self.wake_scheduler();
    }

    /// 取得 Codex 扩展目录读取共用锁，串行保护当前 Codex runtime。
    pub(crate) async fn codex_catalog_refresh_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.codex_catalog_refresh_lock.lock().await
    }

    /// 等待新排期通知以避免无任务时持续轮询。
    async fn wait_for_scheduler_wakeup(&self) {
        self.scheduler_wakeup.notified().await;
    }
}

/// 扩展目录缓存发生变化时发送给前端的事件数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionCatalogUpdatedEvent {
    /// 发生缓存变化的 CLI provider。
    provider_id: String,
    /// 缓存变化对应的项目目录；全局目录为 None。
    cwd: Option<String>,
}

/// 根据当前工作目录生成扩展快照使用的稳定上下文键。
pub fn context_key(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return GLOBAL_CONTEXT_KEY.to_string();
    };
    let normalized = normalize_cwd(cwd);
    format!("{CONTEXT_PREFIX}{normalized}")
}

/// 从扩展刷新上下文键中取回对应的项目目录。
pub fn cwd_from_context_key(context_key: &str) -> Option<String> {
    context_key
        .strip_prefix(CONTEXT_PREFIX)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string)
}

/// 根据扩展操作类型计算需要重新读取的目录类型。
pub fn affected_refresh_kinds(kind: &str) -> Vec<String> {
    match kind {
        // Plugin install/remove can add or remove the skills and MCP servers it provides.
        "plugin" => EXTENSION_KINDS
            .iter()
            .map(|kind| (*kind).to_string())
            .collect(),
        "skill" | "mcp" => vec![kind.to_string()],
        _ => Vec::new(),
    }
}

/// 通过当前 CLI 实现读取本机扩展目录缓存，不从公共调度模块访问数据库。
pub async fn load_cached_catalog(
    state: &AppState,
    provider_id: &str,
    cwd: Option<&str>,
) -> Result<CachedExtensionCatalogDto> {
    validate_provider(provider_id)?;
    let cli = CliToolFactory::new(state.clone()).create(provider_id)?;
    let context = CliExecutionContext {
        workspace_id: String::new(),
        root_path: String::new(),
        location_kind: CliLocationKind::Local,
        ssh_connection_id: None,
    };
    cli.get_extension_catalog(&context, cwd).await
}

/// 通过当前 CLI 实现请求扩展目录后台刷新。
pub async fn request_catalog_refresh(
    app: AppHandle,
    state: AppState,
    provider_id: &str,
    cwd: Option<String>,
    requested_kinds: Vec<String>,
) -> Result<bool> {
    validate_provider(provider_id)?;
    let cli = CliToolFactory::new(state.clone()).create(provider_id)?;
    cli.request_extension_catalog_refresh(app, cwd, requested_kinds)
        .await
}

/// 启动扩展目录刷新循环，并将排期与目录读取交给每个 CLI 实现。
pub fn spawn_catalog_refresh_scheduler(app: AppHandle, state: AppState) {
    tauri::async_runtime::spawn(async move {
        schedule_startup_refreshes(&state).await;
        loop {
            let now = runtime_env::system_time_rfc3339();
            let factory = CliToolFactory::new(state.clone());
            for provider_id in CliToolFactory::supported_cli_ids() {
                let cli = match factory.create(provider_id) {
                    Ok(cli) => cli,
                    Err(error) => {
                        log::error!("创建扩展刷新 CLI 失败: provider={provider_id} error={error:#}");
                        continue;
                    }
                };
                match cli.list_due_extension_catalog_refreshes(&now).await {
                    Ok(jobs) => {
                        for job in jobs {
                            let cwd = cwd_from_context_key(&job.context_key);
                            if let Err(error) = cli
                                .request_extension_catalog_refresh(
                                    app.clone(),
                                    cwd,
                                    job.kinds,
                                )
                                .await
                            {
                                log::error!(
                                    "扩展目录后台刷新请求失败: provider={} context={} error={error:#}",
                                    job.provider_id,
                                    job.context_key
                                );
                            }
                        }
                    }
                    Err(error) => log::error!(
                        "查询已到期扩展目录刷新失败: provider={provider_id} error={error:#}"
                    ),
                }
            }
            tokio::select! {
                _ = sleep(next_scheduler_sleep(&state).await) => {}
                _ = state.extension_catalog_refreshes.wait_for_scheduler_wakeup() => {}
            }
        }
    });
}

/// 为已有的本机工作区排入各 CLI 的首次扩展目录刷新。
async fn schedule_startup_refreshes(state: &AppState) {
    // 只为本机工作区登记 provider 专属快照，SSH 项目不混用本机目录缓存。
    let workspaces = match WorkspaceService::new(state.clone()).list_workspaces().await {
        Ok(workspaces) => workspaces,
        Err(error) => {
            log::error!("启动时读取工作区以排期扩展刷新失败: {error:#}");
            return;
        }
    };

    for workspace in workspaces {
        if workspace.location_kind != "local" {
            continue;
        }
        let factory = CliToolFactory::new(state.clone());
        for provider_id in CliToolFactory::supported_cli_ids() {
            match factory.create(provider_id) {
                Ok(cli) => {
                    if let Err(error) = cli
                        .schedule_extension_catalog_context(Some(workspace.root_path.clone()))
                        .await
                    {
                        log::error!(
                            "启动时排期扩展刷新失败: workspace={} provider={provider_id} error={error:#}",
                            workspace.id
                        );
                    }
                }
                Err(error) => log::error!(
                    "创建扩展刷新 CLI 失败: workspace={} provider={provider_id} error={error:#}",
                    workspace.id
                ),
            }
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
    let workspace = WorkspaceService::new(state.clone())
        .get_workspace(workspace_id)
        .await?;
    // SSH 扩展目录由当前 CLI 在当前目标上即时读取。这里的 SQLite 快照 key 只包含 cwd，
    // 不能用于隔离不同 SSH 机器上同名项目的目录。
    if workspace.location_kind == "ssh" {
        return Ok(());
    }
    // 逐个 provider 尝试排期，并保留原有“完成全部 provider 后返回首个错误”的行为。
    let factory = CliToolFactory::new(state.clone());
    let mut first_error = None;
    for provider_id in CliToolFactory::supported_cli_ids() {
        let schedule_result = match factory.create(provider_id) {
            Ok(cli) => cli
                .schedule_extension_catalog_context(Some(workspace.root_path.clone()))
                .await,
            Err(error) => Err(error),
        };
        if let Err(error) = schedule_result {
            log::error!(
                "工作区扩展目录排期失败: workspace={} provider={provider_id} error={error:#}",
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

/// 发出当前 provider 与目录上下文的缓存变化事件。
pub(crate) fn emit_catalog_updated(app: &AppHandle, provider_id: &str, cwd: Option<&str>) {
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

/// 按扩展类型固定顺序去重、校验并规范化刷新请求。
pub(crate) fn normalize_kinds(requested_kinds: Vec<String>) -> Result<Vec<String>> {
    let kinds = if requested_kinds.is_empty() {
        EXTENSION_KINDS
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
    Ok(EXTENSION_KINDS
        .iter()
        .filter(|kind| kinds.contains(**kind))
        .map(|kind| (*kind).to_string())
        .collect())
}

/// 校验请求中的 provider 标识属于当前 CLI 工厂支持范围。
fn validate_provider(provider_id: &str) -> Result<()> {
    if CliToolFactory::supported_cli_ids()
        .iter()
        .any(|supported_id| *supported_id == provider_id)
    {
        return Ok(());
    }
    anyhow::bail!("unsupported extension provider")
}

/// 判断扩展目录类型是否属于当前支持范围。
fn is_supported_kind(kind: &str) -> bool {
    EXTENSION_KINDS.contains(&kind)
}

// #[cfg(test)]
// /// 供刷新锁测试确认只有 Codex 需要串行读取扩展目录。
// fn requires_codex_catalog_refresh_lock(provider_id: &str) -> bool {
//     provider_id == "codex"
// }

/// 统一工作目录分隔符并保留原有 Windows 大小写规则。
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

/// 按扩展类型和名称排序缓存项目，保持前端展示顺序稳定。
fn sort_items(items: &mut [ExtensionItemDto]) {
    items.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

/// 按 provider 与上下文聚合已到期的扩展类型，保持原缓存调度分组规则。
pub(crate) fn group_due_refreshes(
    targets: Vec<ExtensionRefreshTarget>,
) -> BTreeMap<(String, String), Vec<String>> {
    let mut grouped = BTreeMap::<(String, String), Vec<String>>::new();
    for target in targets {
        grouped
            .entry((target.provider_id, target.context_key))
            .or_default()
            .push(target.kind);
    }
    grouped
}

/// 读取快照中最近一次成功刷新时间，保留原时间解析与最大值规则。
pub(crate) fn latest_snapshot_timestamp(
    snapshots: &[ExtensionCatalogSnapshot],
) -> Option<String> {
    snapshots
        .iter()
        .filter_map(|snapshot| snapshot.fetched_at.as_deref())
        .max_by_key(|value| runtime_env::parse_persisted_time_to_utc(value).ok())
        .map(str::to_string)
}

/// 读取快照中最近一次刷新尝试时间，保留原时间解析与最大值规则。
pub(crate) fn latest_attempt_timestamp(
    snapshots: &[ExtensionCatalogSnapshot],
) -> Option<String> {
    snapshots
        .iter()
        .filter_map(|snapshot| snapshot.last_attempt_at.as_deref())
        .max_by_key(|value| runtime_env::parse_persisted_time_to_utc(value).ok())
        .map(str::to_string)
}

/// 将 CLI 已读取的快照转换为扩展目录 DTO，不执行数据库查询或刷新操作。
pub(crate) fn cached_catalog_from_snapshots(
    provider_id: &str,
    cwd: Option<&str>,
    snapshots: &[ExtensionCatalogSnapshot],
    is_refreshing: bool,
    refresh_completed_at: Option<String>,
) -> CachedExtensionCatalogDto {
    let has_snapshot = snapshots
        .iter()
        .any(|snapshot| snapshot.fetched_at.is_some());
    let now = Utc::now();
    // 启动排期会先把到期时间写为当前时间；旧快照也应在后台请求开始前显示为刷新中。
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
    CachedExtensionCatalogDto {
        provider_id: provider_id.to_string(),
        cwd: cwd.map(str::to_string),
        sources: sources_from_items(&items),
        capabilities: provider_capabilities(provider_id),
        fetched_at: latest_snapshot_timestamp(snapshots),
        kind_fetched_at,
        last_attempt_at: latest_attempt_timestamp(snapshots),
        next_refresh_at,
        refreshing: refresh_pending || is_refreshing,
        refresh_completed_at,
        has_snapshot,
        refresh_errors,
        items,
    }
}

/// 按各 CLI 提供的最近刷新时间计算下一次调度等待时长。
async fn next_scheduler_sleep(state: &AppState) -> Duration {
    // 汇总各 CLI 的最早到期时间，无法读取的 provider 不阻止其他 provider 调度。
    let factory = CliToolFactory::new(state.clone());
    let mut next_refresh_at = None::<String>;
    for provider_id in CliToolFactory::supported_cli_ids() {
        let cli = match factory.create(provider_id) {
            Ok(cli) => cli,
            Err(error) => {
                log::error!("创建扩展刷新 CLI 失败: provider={provider_id} error={error:#}");
                continue;
            }
        };
        match cli.next_extension_catalog_refresh_at().await {
            Ok(Some(value)) => {
                let value_time = DateTime::parse_from_rfc3339(&value).ok();
                match next_refresh_at.as_ref() {
                    None => next_refresh_at = Some(value),
                    Some(current) => {
                        let current_time = DateTime::parse_from_rfc3339(current).ok();
                        let is_earlier = match (value_time.as_ref(), current_time.as_ref()) {
                            (Some(value_time), Some(current_time)) => value_time < current_time,
                            (Some(_), None) => true,
                            _ => false,
                        };
                        if is_earlier {
                            next_refresh_at = Some(value);
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(error) => log::error!(
                "查询下次扩展目录刷新时间失败: provider={provider_id} error={error:#}"
            ),
        }
    }
    let Some(next_refresh_at) = next_refresh_at else {
        // 新上下文会唤醒调度器；无排期时长时间休眠，避免空转。
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
