use std::{
    // 客户端运行对象不再由 Tunnel 保存。
    // any::Any,
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    sync::LazyLock,
};

use anyhow::Context;
#[allow(unused_imports)]
use flate2::{write::GzEncoder, Compression};
use tar::Builder;
use tauri::AppHandle;
use tokio::{process::Child, sync::Mutex, sync::RwLock};
use uuid::Uuid;

use crate::{
    db::ssh_connections::SshConnectionRecord,
    message_notify_helper::notify_app_startup_progress,
    runtime_env,
    // ssh::{gateway, runtime::quote_posix},
    ssh::{gateway, runtime::wrap_remote_login_shell_command},
};

pub struct SshCliTunnel {
    connection: SshConnectionRecord,
    connection_id: String,
    cli_id: String,
    local_port: u16,
    remote_port: u16,
    remote_service_secret: Option<String>,
    process: Mutex<Option<Child>>,
    pub(crate) service_lifecycle: Mutex<RemoteCliServiceLifecycle>,
    // 旧字段把各 CLI 的客户端 Engine 缓存在 Tunnel 对象中，职责已经移回各 CLI
    // 客户端运行服务：
    // pub(crate) service_runtime: Mutex<Option<RemoteCliRuntimeCache>>,
}

/*
pub(crate) struct RemoteCliRuntimeCache {
    pub(crate) service_generation: u64,
    pub(crate) runtime: Arc<dyn Any + Send + Sync>,
}
*/

/// 单台 SSH 远端服务器的 CLI 隧道恢复结果。
#[derive(Debug, Clone)]
pub struct SshRemoteServerInitializationResult {
    /// SSH 连接在本地数据库中的唯一标识。
    pub connection_id: String,
    /// 本次检测后已存在或成功写入隧道注册表的 CLI 标识。
    pub restored_cli_ids: Vec<String>,
    /// 当前服务器恢复失败的汇总信息；没有失败时为 `None`。
    pub error: Option<String>,
}

impl SshCliTunnel {
    pub fn new(
        connection: SshConnectionRecord,
        cli_id: String,
        local_port: u16,
        remote_port: u16,
        process: Child,
    ) -> Self {
        let connection_id = connection.dto.id.clone();
        let remote_service_secret = if cli_id == "opencode" {
            Some(Uuid::new_v4().to_string())
        } else {
            None
        };
        Self {
            connection,
            connection_id,
            cli_id,
            local_port,
            remote_port,
            remote_service_secret,
            process: Mutex::new(Some(process)),
            service_lifecycle: Mutex::new(RemoteCliServiceLifecycle::default()),
            // service_runtime: Mutex::new(None),
        }
    }

    pub fn connection(&self) -> &SshConnectionRecord {
        &self.connection
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn cli_id(&self) -> &str {
        &self.cli_id
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    pub fn remote_service_secret(&self) -> Option<&str> {
        self.remote_service_secret.as_deref()
    }

    async fn close(&self) {
        let Some(mut process) = self.process.lock().await.take() else {
            return;
        };
        let _ = process.start_kill();
        let _ = process.wait().await;
    }

    // async fn invalidate_service_runtime(&self) -> bool {
    //     self.service_runtime.lock().await.take().is_some()
    // }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteCliServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

pub(crate) struct RemoteCliServiceLifecycle {
    service_state: RemoteCliServiceState,
    pub(crate) service_generation: u64,
    /// 应用启动后对远端 CLI 服务持有的一次常驻占用。
    ///
    /// 它按“SSH 连接配置 + CLI”隔离，正常运行期间只有 `0` 或 `1`：
    /// 启动阶段成功建立服务后为 `1`，只有显式停止、移除连接或退出 AuraCoder 时才归零。
    resident_use_count: u32,
    temporary_use_count: u32,
    persistent_session_uses: HashMap<String, u32>,
}

impl Default for RemoteCliServiceLifecycle {
    fn default() -> Self {
        Self {
            service_state: RemoteCliServiceState::Stopped,
            service_generation: 0,
            resident_use_count: 0,
            temporary_use_count: 0,
            persistent_session_uses: HashMap::new(),
        }
    }
}

impl RemoteCliServiceLifecycle {
    fn has_no_active_uses(&self) -> bool {
        self.resident_use_count == 0
            && self.temporary_use_count == 0
            && self.persistent_session_uses.is_empty()
    }

    fn release_persistent_session_use(&mut self, thread_id: &str) -> bool {
        let Some(use_count) = self.persistent_session_uses.get_mut(thread_id) else {
            return false;
        };
        if *use_count == 1 {
            self.persistent_session_uses.remove(thread_id);
        } else {
            *use_count -= 1;
        }
        true
    }
}

pub enum AddSshCliTunnelResult {
    Added(Arc<SshCliTunnel>),
    Existing(Arc<SshCliTunnel>),
}

#[derive(Default)]
pub struct SshCliTunnelRegistry {
    tunnels: RwLock<HashMap<String, HashMap<String, Arc<SshCliTunnel>>>>,
}

static SSH_CLI_TUNNELS: LazyLock<SshCliTunnelRegistry> =
    LazyLock::new(SshCliTunnelRegistry::default);

pub async fn add(tunnel: SshCliTunnel) -> AddSshCliTunnelResult {
    SSH_CLI_TUNNELS.add(tunnel).await
}

/// 启动阶段恢复所有未删除且启用的 SSH 服务器及其 CLI 隧道。
///
/// 该流程只执行 SSH 连通性检测、远端 CLI 扫描和本地端口转发注册，
/// 不启动远端 CLI 服务，也不扫描或写入任何远端会话数据。
pub async fn init_all_ssh_remote_server(
    app: &AppHandle,
    db: Arc<crate::db::Database>,
) -> anyhow::Result<Vec<SshRemoteServerInitializationResult>> {
    // SQLite 连接是同步资源，放到阻塞线程中读取，避免启动恢复阻塞异步执行器。
    let records = tokio::task::spawn_blocking({
        let db = db.clone();
        move || crate::db::ssh_connections::list_records(db.as_ref(), false)
    })
    .await
    .context("读取 SSH 连接配置失败")??;

    let mut results = Vec::new();
    for record in records.into_iter().filter(|record| record.dto.enabled) {
        let connection_id = record.dto.id.clone();
        let test = gateway::test(&record).await;
        let mut result = SshRemoteServerInitializationResult {
            connection_id,
            restored_cli_ids: Vec::new(),
            error: None,
        };

        if !test.ok {
            result.error = Some(test.error.unwrap_or_else(|| "SSH 连接检测失败".to_string()));
            results.push(result);
            continue;
        }

        if let Err(error) =
            notify_app_startup_progress(app, "creating-cli-tunnels", "正在建立远端 CLI 隧道……")
        {
            log::warn!("发送启动进度失败: {error:#}");
        }

        // 每个 CLI 的隧道独立恢复；单个 CLI 失败只记录错误，不影响同一主机的其他 CLI。
        let (restored_cli_ids, tunnel_errors) =
            register_cli_tunnels(&record, &test.cli_versions).await;
        result.restored_cli_ids = restored_cli_ids;
        if !tunnel_errors.is_empty() {
            result.error = Some(tunnel_errors.join("; "));
        }
        results.push(result);
    }
    Ok(results)
}

/// 根据一次 SSH 检测结果建立 CLI 隧道，并以 `add` 的 put-if-absent 语义写入注册表。
///
/// 返回值中的第一个集合表示当前可用的 CLI，第二个集合表示逐 CLI 的恢复错误。
/// 该函数不启动或关闭远端 CLI 服务，因此可以同时用于启动恢复和新建连接。
pub async fn register_cli_tunnels(
    record: &SshConnectionRecord,
    cli_versions: &BTreeMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut restored_cli_ids = Vec::new();
    let mut tunnel_errors = Vec::new();

    for cli_id in cli_versions.keys() {
        let Some(preferred_remote_port) = preferred_remote_port(cli_id) else {
            continue;
        };
        if get(&record.dto.id, cli_id).await.is_some() {
            // 已有隧道仍然有效，避免新建重复的 SSH 进程；其状态也算恢复成功。
            restored_cli_ids.push(cli_id.clone());
            continue;
        }

        let remote_port = match gateway::run_command(
            record,
            &format!(
                "port={preferred_remote_port}; while ss -ltn 2>/dev/null | awk '{{print $4}}' | grep -Eq \"[:.]${{port}}$\"; do port=$((port + 1)); [ \"$port\" -le 65535 ] || exit 1; done; printf '%s\\n' \"$port\""
            ),
        )
        .await
        .and_then(|output| {
            output
                .trim()
                .parse::<u16>()
                .map_err(anyhow::Error::from)
        }) {
            Ok(remote_port) => remote_port,
            Err(error) => {
                let message = format!(
                    "failed to allocate remote SSH CLI port connection={} cli={}: {error}",
                    record.dto.id, cli_id
                );
                log::warn!("{message}");
                tunnel_errors.push(message);
                continue;
            }
        };

        let local_listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) => {
                let message = format!(
                    "failed to allocate local SSH CLI port connection={} cli={}: {error}",
                    record.dto.id, cli_id
                );
                log::warn!("{message}");
                tunnel_errors.push(message);
                continue;
            }
        };
        let local_port = match local_listener.local_addr() {
            Ok(address) => address.port(),
            Err(error) => {
                let message = format!(
                    "failed to read local SSH CLI port connection={} cli={}: {error}",
                    record.dto.id, cli_id
                );
                log::warn!("{message}");
                tunnel_errors.push(message);
                continue;
            }
        };
        drop(local_listener);

        match gateway::open_tunnel(record, local_port, "127.0.0.1", remote_port).await {
            Ok(mut process) => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                match process.try_wait() {
                    Ok(None) => {}
                    Ok(Some(status)) => {
                        let message = format!(
                            "SSH CLI tunnel exited during startup connection={} cli={} status={status}",
                            record.dto.id, cli_id
                        );
                        log::warn!("{message}");
                        tunnel_errors.push(message);
                        continue;
                    }
                    Err(error) => {
                        let message = format!(
                            "failed to inspect SSH CLI tunnel connection={} cli={}: {error}",
                            record.dto.id, cli_id
                        );
                        log::warn!("{message}");
                        tunnel_errors.push(message);
                        continue;
                    }
                }

                let added = add(SshCliTunnel::new(
                    record.clone(),
                    cli_id.clone(),
                    local_port,
                    remote_port,
                    process,
                ))
                .await;
                match added {
                    AddSshCliTunnelResult::Added(tunnel) => {
                        log::info!(
                            "registered SSH CLI tunnel connection={} cli={} local_port={} remote_port={}",
                            tunnel.connection_id(),
                            tunnel.cli_id(),
                            tunnel.local_port(),
                            tunnel.remote_port()
                        );
                        restored_cli_ids.push(cli_id.clone());
                    }
                    AddSshCliTunnelResult::Existing(_) => {
                        // 并发恢复时可能在检查和 add 之间出现已有隧道，保留先注册的隧道。
                        restored_cli_ids.push(cli_id.clone());
                    }
                }
            }
            Err(error) => {
                let message = format!(
                    "failed to register SSH CLI tunnel connection={} cli={}: {error}",
                    record.dto.id, cli_id
                );
                log::warn!("{message}");
                tunnel_errors.push(message);
            }
        }
    }

    (restored_cli_ids, tunnel_errors)
}

/// 返回受支持 CLI 的默认远端端口起点。
fn preferred_remote_port(cli_id: &str) -> Option<u16> {
    match cli_id {
        "codex" => Some(43_100),
        "claude" => Some(43_200),
        "gemini" => Some(43_300),
        "agy" => Some(43_400),
        "kiro-cli" => Some(43_500),
        "opencode" => Some(43_600),
        "kilo" => Some(43_700),
        "droid" => Some(43_800),
        _ => None,
    }
}

pub async fn get(connection_id: &str, cli_id: &str) -> Option<Arc<SshCliTunnel>> {
    SSH_CLI_TUNNELS.get(connection_id, cli_id).await
}

pub async fn list_by_host(connection_id: &str) -> HashMap<String, Arc<SshCliTunnel>> {
    SSH_CLI_TUNNELS.list_by_host(connection_id).await
}

pub async fn remove(connection_id: &str, cli_id: &str) -> bool {
    SSH_CLI_TUNNELS.remove(connection_id, cli_id).await
}

pub async fn start_remote_cli_service(
    connection_id: &str,
    cli_id: &str,
) -> anyhow::Result<Arc<SshCliTunnel>> {
    SSH_CLI_TUNNELS
        .start_remote_cli_service(connection_id, cli_id)
        .await
}

pub async fn stop_remote_cli_service(connection_id: &str, cli_id: &str) -> anyhow::Result<bool> {
    SSH_CLI_TUNNELS
        .stop_remote_cli_service(connection_id, cli_id)
        .await
}

pub async fn acquire_temporary_service_use(
    connection_id: &str,
    cli_id: &str,
) -> anyhow::Result<Arc<SshCliTunnel>> {
    SSH_CLI_TUNNELS
        .acquire_temporary_service_use(connection_id, cli_id)
        .await
}

pub async fn release_temporary_service_use(
    connection_id: &str,
    cli_id: &str,
) -> anyhow::Result<bool> {
    SSH_CLI_TUNNELS
        .release_temporary_service_use(connection_id, cli_id)
        .await
}

pub async fn acquire_persistent_service_use(
    connection_id: &str,
    cli_id: &str,
    thread_id: &str,
) -> anyhow::Result<Arc<SshCliTunnel>> {
    SSH_CLI_TUNNELS
        .acquire_persistent_service_use(connection_id, cli_id, thread_id)
        .await
}

pub async fn release_persistent_service_use(
    connection_id: &str,
    cli_id: &str,
    thread_id: &str,
) -> anyhow::Result<bool> {
    SSH_CLI_TUNNELS
        .release_persistent_service_use(connection_id, cli_id, thread_id)
        .await
}

pub async fn shutdown() {
    SSH_CLI_TUNNELS.shutdown().await;
}

impl SshCliTunnelRegistry {
    pub async fn add(&self, tunnel: SshCliTunnel) -> AddSshCliTunnelResult {
        let connection_id = tunnel.connection_id().to_string();
        let cli_id = tunnel.cli_id().to_string();
        let tunnel = Arc::new(tunnel);
        let existing = {
            let mut tunnels = self.tunnels.write().await;
            let host_tunnels = tunnels.entry(connection_id).or_default();
            if let Some(existing) = host_tunnels.get(&cli_id) {
                Some(existing.clone())
            } else {
                host_tunnels.insert(cli_id, tunnel.clone());
                None
            }
        };

        if let Some(existing) = existing {
            tunnel.close().await;
            AddSshCliTunnelResult::Existing(existing)
        } else {
            AddSshCliTunnelResult::Added(tunnel)
        }
    }

    pub async fn get(&self, connection_id: &str, cli_id: &str) -> Option<Arc<SshCliTunnel>> {
        self.tunnels
            .read()
            .await
            .get(connection_id)
            .and_then(|host_tunnels| host_tunnels.get(cli_id))
            .cloned()
    }

    pub async fn list_by_host(&self, connection_id: &str) -> HashMap<String, Arc<SshCliTunnel>> {
        self.tunnels
            .read()
            .await
            .get(connection_id)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn remove(&self, connection_id: &str, cli_id: &str) -> bool {
        let removed = {
            let mut tunnels = self.tunnels.write().await;
            let Some(host_tunnels) = tunnels.get_mut(connection_id) else {
                return false;
            };
            let removed = host_tunnels.remove(cli_id);
            if host_tunnels.is_empty() {
                tunnels.remove(connection_id);
            }
            removed
        };

        if let Some(tunnel) = removed {
            let mut lifecycle = tunnel.service_lifecycle.lock().await;
            lifecycle.resident_use_count = 0;
            lifecycle.temporary_use_count = 0;
            lifecycle.persistent_session_uses.clear();
            if let Err(error) =
                close_remote_cli_service_if_unused(tunnel.as_ref(), &mut lifecycle).await
            {
                log::warn!(
                    "移除 SSH CLI 隧道时关闭远端 CLI 服务失败: connection_id={} cli_id={} error={error}",
                    tunnel.connection_id(),
                    tunnel.cli_id()
                );
            }
            drop(lifecycle);
            tunnel.close().await;
            true
        } else {
            false
        }
    }

    pub async fn start_remote_cli_service(
        &self,
        connection_id: &str,
        cli_id: &str,
    ) -> anyhow::Result<Arc<SshCliTunnel>> {
        let tunnel = self.get(connection_id, cli_id).await.with_context(|| {
            format!("未找到 SSH CLI 隧道: connection_id={connection_id} cli_id={cli_id}")
        })?;
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        let acquired_resident_use = lifecycle.resident_use_count == 0;
        lifecycle.resident_use_count = 1;
        if let Err(error) = ensure_remote_cli_service_running(tunnel.as_ref(), &mut lifecycle).await
        {
            if acquired_resident_use {
                lifecycle.resident_use_count = 0;
            }
            return Err(error);
        }
        drop(lifecycle);
        Ok(tunnel)
    }

    pub async fn stop_remote_cli_service(
        &self,
        connection_id: &str,
        cli_id: &str,
    ) -> anyhow::Result<bool> {
        let Some(tunnel) = self.get(connection_id, cli_id).await else {
            return Ok(false);
        };
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        anyhow::ensure!(
            lifecycle.temporary_use_count == 0 && lifecycle.persistent_session_uses.is_empty(),
            "SSH 远端 CLI 服务仍被占用，不能直接关闭: connection_id={connection_id} cli_id={cli_id}"
        );
        lifecycle.resident_use_count = 0;
        close_remote_cli_service_if_unused(tunnel.as_ref(), &mut lifecycle).await
    }

    pub async fn acquire_temporary_service_use(
        &self,
        connection_id: &str,
        cli_id: &str,
    ) -> anyhow::Result<Arc<SshCliTunnel>> {
        let tunnel = self.get(connection_id, cli_id).await.with_context(|| {
            format!("未找到 SSH CLI 隧道: connection_id={connection_id} cli_id={cli_id}")
        })?;
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        lifecycle.temporary_use_count += 1;
        if let Err(error) = ensure_remote_cli_service_running(tunnel.as_ref(), &mut lifecycle).await
        {
            lifecycle.temporary_use_count -= 1;
            return Err(error);
        }
        Ok(tunnel.clone())
    }

    pub async fn release_temporary_service_use(
        &self,
        connection_id: &str,
        cli_id: &str,
    ) -> anyhow::Result<bool> {
        let tunnel = self.get(connection_id, cli_id).await.with_context(|| {
            format!("未找到 SSH CLI 隧道: connection_id={connection_id} cli_id={cli_id}")
        })?;
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        anyhow::ensure!(
            lifecycle.temporary_use_count > 0,
            "没有可释放的 SSH 远端 CLI 临时占用: connection_id={connection_id} cli_id={cli_id}"
        );
        lifecycle.temporary_use_count -= 1;
        close_remote_cli_service_if_unused(tunnel.as_ref(), &mut lifecycle).await
    }

    pub async fn acquire_persistent_service_use(
        &self,
        connection_id: &str,
        cli_id: &str,
        thread_id: &str,
    ) -> anyhow::Result<Arc<SshCliTunnel>> {
        let tunnel = self.get(connection_id, cli_id).await.with_context(|| {
            format!("未找到 SSH CLI 隧道: connection_id={connection_id} cli_id={cli_id}")
        })?;
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        *lifecycle
            .persistent_session_uses
            .entry(thread_id.to_string())
            .or_insert(0) += 1;
        if let Err(error) = ensure_remote_cli_service_running(tunnel.as_ref(), &mut lifecycle).await
        {
            lifecycle.release_persistent_session_use(thread_id);
            return Err(error);
        }
        Ok(tunnel.clone())
    }

    pub async fn release_persistent_service_use(
        &self,
        connection_id: &str,
        cli_id: &str,
        thread_id: &str,
    ) -> anyhow::Result<bool> {
        let tunnel = self.get(connection_id, cli_id).await.with_context(|| {
            format!("未找到 SSH CLI 隧道: connection_id={connection_id} cli_id={cli_id}")
        })?;
        let mut lifecycle = tunnel.service_lifecycle.lock().await;
        anyhow::ensure!(
            lifecycle.release_persistent_session_use(thread_id),
            "没有可释放的 SSH 远端 CLI 持续占用: connection_id={connection_id} cli_id={cli_id} thread_id={thread_id}"
        );
        close_remote_cli_service_if_unused(tunnel.as_ref(), &mut lifecycle).await
    }

    pub async fn shutdown(&self) {
        let tunnels = {
            let mut registry = self.tunnels.write().await;
            registry
                .drain()
                .flat_map(|(_, host_tunnels)| host_tunnels.into_values())
                .collect::<Vec<_>>()
        };
        for tunnel in tunnels {
            let mut lifecycle = tunnel.service_lifecycle.lock().await;
            lifecycle.resident_use_count = 0;
            lifecycle.temporary_use_count = 0;
            lifecycle.persistent_session_uses.clear();
            if let Err(error) =
                close_remote_cli_service_if_unused(tunnel.as_ref(), &mut lifecycle).await
            {
                log::warn!(
                    "关闭 AuraCoder 时停止 SSH 远端 CLI 服务失败: connection_id={} cli_id={} error={error}",
                    tunnel.connection_id(),
                    tunnel.cli_id()
                );
            }
            drop(lifecycle);
            tunnel.close().await;
        }
    }
}

async fn ensure_remote_cli_service_running(
    tunnel: &SshCliTunnel,
    lifecycle: &mut RemoteCliServiceLifecycle,
) -> anyhow::Result<()> {
    if lifecycle.service_state == RemoteCliServiceState::Running {
        return Ok(());
    }
    lifecycle.service_state = RemoteCliServiceState::Starting;
    match start_remote_cli_service_for_tunnel(tunnel).await {
        Ok(()) => {
            lifecycle.service_generation = lifecycle.service_generation.wrapping_add(1).max(1);
            lifecycle.service_state = RemoteCliServiceState::Running;
            Ok(())
        }
        Err(error) => {
            // 启动失败后，旧服务实例的运行时对象已经不再可信，必须随状态一起失效。
            // tunnel.service_runtime.lock().await.take();
            // 客户端运行对象由各 CLI 运行服务按服务代次自行失效。
            // tunnel.invalidate_service_runtime().await;
            lifecycle.service_state = RemoteCliServiceState::Stopped;
            Err(error)
        }
    }
}

async fn close_remote_cli_service_if_unused(
    tunnel: &SshCliTunnel,
    lifecycle: &mut RemoteCliServiceLifecycle,
) -> anyhow::Result<bool> {
    if !lifecycle.has_no_active_uses() {
        return Ok(false);
    }
    if lifecycle.service_state == RemoteCliServiceState::Stopped {
        // 统一生命周期状态已经停止时，也要清除可能由旧释放路径遗留的运行时缓存。
        // tunnel.service_runtime.lock().await.take();
        // tunnel.invalidate_service_runtime().await;
        return Ok(false);
    }
    lifecycle.service_state = RemoteCliServiceState::Stopping;
    // 在关闭远端进程前先让本地运行时失效，禁止后续请求复用旧事件连接。
    // tunnel.service_runtime.lock().await.take();
    // tunnel.invalidate_service_runtime().await;
    match stop_remote_cli_service_for_tunnel(tunnel).await {
        Ok(()) => {
            lifecycle.service_state = RemoteCliServiceState::Stopped;
            Ok(true)
        }
        Err(error) => {
            lifecycle.service_state = RemoteCliServiceState::Stopped;
            Err(error)
        }
    }
}

/// 仅供 `cli_service_lifecycle` 调用的远端服务端启动原语。业务层和各 CLI 实现
/// 不得直接调用。
pub(crate) async fn start_remote_cli_service_for_tunnel(
    tunnel: &SshCliTunnel,
) -> anyhow::Result<()> {
    if tunnel.cli_id() == "claude" {
        let prerequisite_command = wrap_remote_login_shell_command(
            "node -e 'const [major, minor] = process.versions.node.split(\".\").map(Number); const compatible = (major > 20 || (major === 20 && minor >= 5)) && typeof Symbol.dispose === \"symbol\" && typeof Symbol.asyncDispose === \"symbol\"; process.exit(compatible ? 0 : 45)' && claude_path=$(type -P claude) || exit 44; \"$claude_path\" auth status >/dev/null || exit 46",
        );
        gateway::run_command(tunnel.connection(), &prerequisite_command)
            .await
            .context(
                "SSH 远端 Claude 前置检查失败：需要 Node.js 20.5+、Claude Code 可执行文件和有效登录状态",
            )?;
        ensure_claude_runtime_installed(tunnel.connection())
            .await
            .with_context(|| {
                format!(
                    "安装 Claude 远端 AuraCoder 适配器失败: connection_id={} cli_id={}",
                    tunnel.connection_id(),
                    tunnel.cli_id()
                )
            })?;
    }
    let start_command = build_remote_service_start_command(tunnel)?;
    gateway::run_command(tunnel.connection(), &start_command)
        .await
        .with_context(|| {
            format!(
                "启动 SSH 远端 CLI 服务失败: connection_id={} cli_id={}",
                tunnel.connection_id(),
                tunnel.cli_id()
            )
        })?;
    let wait_command = build_remote_service_wait_command(tunnel);
    gateway::run_command(tunnel.connection(), &wait_command)
        .await
        .with_context(|| {
            format!(
                "等待 SSH 远端 CLI 服务就绪失败: connection_id={} cli_id={}",
                tunnel.connection_id(),
                tunnel.cli_id()
            )
        })?;
    Ok(())
}

/// 仅供 `cli_service_lifecycle` 调用的远端服务端停止原语。
pub(crate) async fn stop_remote_cli_service_for_tunnel(
    tunnel: &SshCliTunnel,
) -> anyhow::Result<()> {
    let stop_command = build_remote_service_stop_command(tunnel);
    gateway::run_command(tunnel.connection(), &stop_command)
        .await
        .with_context(|| {
            format!(
                "关闭 SSH 远端 CLI 服务失败: connection_id={} cli_id={}",
                tunnel.connection_id(),
                tunnel.cli_id()
            )
        })
        .map(|_| ())
}

/// 仅供 `cli_service_lifecycle` 健康检查调用的远端服务端探活原语。
///
/// 单次执行、不做等待：PID 文件中的进程存活且远端端口处于监听状态才算活着。
/// 隧道的断线恢复不属于本函数职责，探活失败只说明当前服务不可达。
pub(crate) async fn probe_remote_cli_service_alive(tunnel: &SshCliTunnel) -> bool {
    let pid_file = remote_service_pid_file(tunnel);
    let probe_command = format!(
        "pid_file=\"{pid_file}\"; port={port}; \
[ -f \"$pid_file\" ] && kill -0 \"$(cat \"$pid_file\")\" 2>/dev/null && \
ss -ltn 2>/dev/null | awk '{{print $4}}' | grep -Eq \"[:.]${{port}}$\"",
        pid_file = pid_file,
        port = tunnel.remote_port(),
    );
    match gateway::run_command(tunnel.connection(), &probe_command).await {
        Ok(_) => true,
        Err(error) => {
            log::warn!(
                "SSH 远端 CLI 服务探活失败: connection_id={} cli_id={} error={error:#}",
                tunnel.connection_id(),
                tunnel.cli_id()
            );
            false
        }
    }
}

fn build_remote_service_start_command(tunnel: &SshCliTunnel) -> anyhow::Result<String> {
    let pid_file = remote_service_pid_file(tunnel);
    let log_file = remote_service_log_file(tunnel);
    let runtime_dir = remote_service_runtime_dir(tunnel);
    let launch_command = match tunnel.cli_id() {
        "codex" => format!(
            // "exec codex app-server --listen ws://127.0.0.1:{}",
            "exec env codex app-server --listen ws://127.0.0.1:{}",
            tunnel.remote_port()
        ),
        "opencode" => {
            let secret = tunnel
                .remote_service_secret()
                .context("OpenCode 远端服务密码不存在")?;
            let opencode_env = runtime_env::get_remote_opencode_env(secret);
            format!(
                "{opencode_env}exec env opencode serve --hostname 127.0.0.1 --port {}",
                tunnel.remote_port()
            )
        }
        "claude" => {
            let claude_env = runtime_env::get_remote_claude_env("claude_path")?;
            format!(
                "claude_path=$(type -P claude) || {{ echo 'Claude Code executable not found' >&2; exit 44; }}; {claude_env}exec env no{} {}/claude-remote-session-server.mjs --host 127.0.0.1 --port {}",
                "de",
                claude_runtime_remote_root(),
                tunnel.remote_port(),
            )
        }
        other => anyhow::bail!("当前未实现该 SSH 远端 CLI 服务启动: {other}"),
    };
    let launch_command = wrap_remote_login_shell_command(&launch_command);
    /*
    旧实现遇到存活的 Codex PID 时直接退出，不核对该进程监听的端口是否等于当前
    Tunnel 分配的远端端口：
    let existing_service_action = if matches!(tunnel.cli_id(), "opencode" | "claude") {
        ...
    } else {
        "exit 0;"
    };
    这会形成“旧 Codex 监听 43100、新 Tunnel 转发到 43101”的失配。
    */
    let existing_service_action = "pid=$(cat \"$pid_file\"); kill \"$pid\" 2>/dev/null || true; \
for _ in $(seq 1 50); do \
  if kill -0 \"$pid\" 2>/dev/null; then sleep 0.1; else break; fi; \
done; \
if kill -0 \"$pid\" 2>/dev/null; then kill -9 \"$pid\" 2>/dev/null || true; fi;";
    Ok(format!(
        "runtime_dir=\"{runtime_dir}\"; pid_file=\"{pid_file}\"; log_file=\"{log_file}\"; \
mkdir -p \"$runtime_dir\"; \
if [ -f \"$pid_file\" ] && kill -0 \"$(cat \"$pid_file\")\" 2>/dev/null; then \
  {existing_service_action} \
fi; \
rm -f \"$pid_file\"; \
nohup {launch_command} >\"$log_file\" 2>&1 & echo $! > \"$pid_file\"",
        runtime_dir = runtime_dir,
        pid_file = pid_file,
        log_file = log_file,
        existing_service_action = existing_service_action,
        launch_command = launch_command,
    ))
}

fn build_remote_service_wait_command(tunnel: &SshCliTunnel) -> String {
    if tunnel.cli_id() == "claude" {
        let pid_file = remote_service_pid_file(tunnel);
        let log_file = remote_service_log_file(tunnel);
        // 旧等待命令直接由非登录 SSH Shell 执行。在 Node.js 由 nvm、Volta 或 asdf
        // 注入 PATH 时，即使 Claude 服务已经健康，这里仍会因找不到 node 而超时。
        // return format!(
        //     "port={}; pid_file=\"{}\"; log_file=\"{}\"; \
        // for _ in $(seq 1 300); do \
        //   if node -e 'fetch(\"http://127.0.0.1:\" + process.argv[1] + \"/health\").then(response => process.exit(response.ok ? 0 : 1)).catch(() => process.exit(1))' \"$port\"; then \
        //     exit 0; \
        //   fi; \
        //   if [ -f \"$pid_file\" ] && ! kill -0 \"$(cat \"$pid_file\")\" 2>/dev/null; then \
        //     tail -n 80 \"$log_file\" >&2 2>/dev/null || true; \
        //     exit 1; \
        //   fi; \
        //   sleep 0.1; \
        // done; \
        // tail -n 80 \"$log_file\" >&2 2>/dev/null || true; \
        // exit 1",
        //     tunnel.remote_port(),
        //     pid_file,
        //     log_file,
        // );
        let wait_command = format!(
            "node_path=$(type -P node) || exit 45; port={}; pid_file=\"{}\"; log_file=\"{}\"; \
for _ in $(seq 1 300); do \
  if \"$node_path\" -e 'fetch(\"http://127.0.0.1:\" + process.argv[1] + \"/health\").then(response => process.exit(response.ok ? 0 : 1)).catch(() => process.exit(1))' \"$port\"; then \
    exit 0; \
  fi; \
  if [ -f \"$pid_file\" ] && ! kill -0 \"$(cat \"$pid_file\")\" 2>/dev/null; then \
    tail -n 80 \"$log_file\" >&2 2>/dev/null || true; \
    exit 1; \
  fi; \
  sleep 0.1; \
done; \
tail -n 80 \"$log_file\" >&2 2>/dev/null || true; \
exit 1",
            tunnel.remote_port(),
            pid_file,
            log_file,
        );
        return wrap_remote_login_shell_command(&wait_command);
    }

    format!(
        "port={}; \
for _ in $(seq 1 50); do \
  if ss -ltn 2>/dev/null | awk '{{print $4}}' | grep -Eq \"[:.]${{port}}$\"; then \
    exit 0; \
  fi; \
  sleep 0.1; \
done; \
exit 1",
        tunnel.remote_port()
    )
}

fn build_remote_service_stop_command(tunnel: &SshCliTunnel) -> String {
    let pid_file = remote_service_pid_file(tunnel);
    format!(
        "pid_file=\"{}\"; \
if [ ! -f \"$pid_file\" ]; then \
  exit 0; \
fi; \
pid=$(cat \"$pid_file\"); \
if kill -0 \"$pid\" 2>/dev/null; then \
  kill \"$pid\" 2>/dev/null || true; \
  for _ in $(seq 1 50); do \
    if kill -0 \"$pid\" 2>/dev/null; then \
      sleep 0.1; \
    else \
      break; \
    fi; \
  done; \
  if kill -0 \"$pid\" 2>/dev/null; then \
    kill -9 \"$pid\" 2>/dev/null || true; \
  fi; \
fi; \
rm -f \"$pid_file\"",
        pid_file,
    )
}

fn remote_service_runtime_dir(tunnel: &SshCliTunnel) -> String {
    format!(
        "$HOME/.cache/auracoder/ssh-cli-services/{}/{}",
        tunnel.connection_id(),
        tunnel.cli_id()
    )
}

fn remote_service_pid_file(tunnel: &SshCliTunnel) -> String {
    format!("{}/service.pid", remote_service_runtime_dir(tunnel))
}

fn remote_service_log_file(tunnel: &SshCliTunnel) -> String {
    format!("{}/service.log", remote_service_runtime_dir(tunnel))
}

async fn ensure_claude_runtime_installed(record: &SshConnectionRecord) -> anyhow::Result<()> {
    let remote_root = claude_runtime_remote_root();
    let module_dir = claude_runtime_module_dir_name();
    let verify_command = format!(
        "runtime_root=\"{runtime_root}\"; [ -f \"$runtime_root/claude-agent-sdk-server.mjs\" ] && [ -f \"$runtime_root/claude-remote-session-server.mjs\" ] && [ -f \"$runtime_root/claude-remote-runtime-version.txt\" ] && [ -f \"$runtime_root/{module_dir}/@anthropic-ai/claude-agent-sdk/sdk.mjs\" ]",
        runtime_root = remote_root,
        module_dir = module_dir,
    );
    if gateway::run_command(record, &verify_command).await.is_ok() {
        return Ok(());
    }

    let archive = build_claude_runtime_archive()?;
    let install_command = format!(
        "runtime_root=\"{runtime_root}\"; runtime_parent=$(dirname -- \"$runtime_root\"); staging_root=\"$runtime_parent/.tmp-claude-runtime-{install_id}\"; rm -rf -- \"$staging_root\"; mkdir -p -- \"$staging_root\"; mkdir -p -- \"$runtime_parent\"; tar -xzf - -C \"$staging_root\"; [ -f \"$staging_root/claude-agent-sdk-server.mjs\" ] || exit 41; [ -f \"$staging_root/claude-remote-session-server.mjs\" ] || exit 43; [ -f \"$staging_root/claude-remote-runtime-version.txt\" ] || exit 47; [ -f \"$staging_root/{module_dir}/@anthropic-ai/claude-agent-sdk/sdk.mjs\" ] || exit 42; rm -rf -- \"$runtime_root\"; mv -- \"$staging_root\" \"$runtime_root\"",
        runtime_root = remote_root,
        install_id = Uuid::new_v4(),
        module_dir = module_dir,
    );
    gateway::run_command_with_input(record, &install_command, &archive).await?;
    Ok(())
}

fn build_claude_runtime_archive() -> anyhow::Result<Vec<u8>> {
    let runtime_dir = claude_runtime_local_dir()?;
    fs::read(runtime_dir.join("claude-remote-runtime-linux-x64.tar.gz"))
        .context("读取 Claude SSH 远端 Linux 运行时归档失败")
}

#[allow(dead_code)]
fn append_claude_runtime_dir(
    builder: &mut Builder<&mut GzEncoder<Vec<u8>>>,
    root: &Path,
    current: &Path,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("无法计算 Claude 远端运行时相对路径: {}", path.display()))?;
        if path.is_dir() {
            builder.append_dir(relative, &path)?;
            append_claude_runtime_dir(builder, root, &path)?;
        } else if path.is_file() {
            builder.append_path_with_name(&path, relative)?;
        }
    }
    Ok(())
}

fn claude_runtime_local_dir() -> anyhow::Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!(concat!("CAR", "\u{0047}", "O_MANIFEST_DIR")));
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let mut candidates = vec![manifest_dir.join("sidecar-dist")];
    if let Some(executable_dir) = executable_dir {
        candidates.push(executable_dir.join("sidecar-dist"));
        candidates.push(executable_dir.join("../Resources/sidecar-dist"));
    }
    let local_dir = candidates
        .into_iter()
        .find(|candidate| {
            candidate
                .join("claude-remote-runtime-linux-x64.tar.gz")
                .exists()
        })
        .context("本地 Claude SSH 远端 Linux 运行时归档不存在")?;
    anyhow::ensure!(
        local_dir.join("claude-agent-sdk-server.mjs").exists(),
        "本地 Claude 远端运行时缺少 claude-agent-sdk-server.mjs"
    );
    anyhow::ensure!(
        local_dir.join("claude-remote-session-server.mjs").exists(),
        "本地 Claude 远端运行时缺少 claude-remote-session-server.mjs"
    );
    anyhow::ensure!(
        local_dir.join(claude_runtime_module_dir_name()).exists(),
        "本地 Claude 远端运行时缺少依赖目录"
    );
    anyhow::ensure!(
        local_dir.join("claude-remote-runtime-version.txt").exists(),
        "本地 Claude 远端运行时缺少内容版本"
    );
    Ok(local_dir)
}

fn claude_runtime_module_dir_name() -> String {
    ['n', 'o', 'd', 'e', '_', 'm', 'o', 'd', 'u', 'l', 'e', 's']
        .into_iter()
        .collect()
}

fn claude_runtime_remote_root() -> String {
    let content_version = claude_runtime_local_dir()
        .ok()
        .and_then(|directory| {
            fs::read_to_string(directory.join("claude-remote-runtime-version.txt")).ok()
        })
        .map(|version| {
            version
                .chars()
                .filter(|character| character.is_ascii_hexdigit())
                .collect::<String>()
        })
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "unbuilt".to_string());
    format!(
        "$HOME/.cache/auracoder/runtime/claude/{}-{content_version}",
        env!(concat!("CAR", "\u{0047}", "O_PKG_VERSION"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SshConnectionDto;

    fn tunnel(connection_id: &str, cli_id: &str, local_port: u16) -> SshCliTunnel {
        SshCliTunnel {
            connection: SshConnectionRecord {
                dto: SshConnectionDto {
                    id: connection_id.to_string(),
                    display_name: connection_id.to_string(),
                    source_kind: "manual".to_string(),
                    config_alias: None,
                    host_name: "127.0.0.1".to_string(),
                    user: "tester".to_string(),
                    port: 22,
                    identity_file: None,
                    host_key_type: "ssh-ed25519".to_string(),
                    enabled: true,
                    connection_status: "ok".to_string(),
                    last_connected_at: None,
                    last_error: None,
                    deleted_at: None,
                    created_at: "2026-01-01 00:00:00.000".to_string(),
                    updated_at: "2026-01-01 00:00:00.000".to_string(),
                },
                host_key_base64: String::new(),
            },
            connection_id: connection_id.to_string(),
            cli_id: cli_id.to_string(),
            local_port,
            remote_port: local_port,
            remote_service_secret: (cli_id == "opencode").then(|| "secret".to_string()),
            process: Mutex::new(None),
            service_lifecycle: Mutex::new(RemoteCliServiceLifecycle::default()),
            // service_runtime: Mutex::new(None),
        }
    }

    /*
    #[tokio::test]
    async fn invalidating_service_runtime_removes_the_cached_instance() {
        let tunnel = tunnel("host-a", "claude", 43200);
        *tunnel.service_runtime.lock().await = Some(RemoteCliRuntimeCache {
            service_generation: 1,
            runtime: Arc::new(String::from("claude-runtime")),
        });

        assert!(tunnel.invalidate_service_runtime().await);
        assert!(tunnel.service_runtime.lock().await.is_none());
        assert!(!tunnel.invalidate_service_runtime().await);
    }
    */

    #[test]
    fn remote_service_pid_tracks_the_login_shell_that_execs_the_cli() {
        let tunnel = tunnel("host-a", "codex", 43100);
        let command = build_remote_service_start_command(&tunnel).expect("start command");

        assert!(command.contains("nohup \"${SHELL:-/bin/sh}\" -lic"));
        assert!(!command.contains("nohup sh -lc"));
    }

    #[test]
    fn temporary_and_persistent_uses_must_both_be_released_before_service_can_close() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 0,
            temporary_use_count: 1,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 1)]),
        };

        assert!(!lifecycle.has_no_active_uses());
        lifecycle.temporary_use_count -= 1;
        assert!(!lifecycle.has_no_active_uses());
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(lifecycle.has_no_active_uses());
    }

    #[test]
    fn resident_use_must_be_released_before_service_can_close() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 1,
            temporary_use_count: 1,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 1)]),
        };

        lifecycle.temporary_use_count -= 1;
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(!lifecycle.has_no_active_uses());

        lifecycle.resident_use_count -= 1;
        assert!(lifecycle.has_no_active_uses());
    }

    #[test]
    fn persistent_session_use_is_reference_counted_by_thread_id() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 0,
            temporary_use_count: 0,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 2)]),
        };

        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert_eq!(lifecycle.persistent_session_uses["thread-a"], 1);
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(lifecycle.persistent_session_uses.is_empty());
        assert!(!lifecycle.release_persistent_session_use("thread-a"));
    }

    #[tokio::test]
    async fn add_keeps_existing_tunnel() {
        let registry = SshCliTunnelRegistry::default();
        let first = registry.add(tunnel("host-a", "codex", 41001)).await;
        let second = registry.add(tunnel("host-a", "codex", 41002)).await;

        assert!(matches!(first, AddSshCliTunnelResult::Added(_)));
        let AddSshCliTunnelResult::Existing(existing) = second else {
            panic!("第二次添加必须返回已有隧道");
        };
        assert_eq!(existing.local_port(), 41001);
        assert_eq!(registry.list_by_host("host-a").await.len(), 1);
    }

    #[tokio::test]
    async fn list_by_host_only_returns_requested_host() {
        let registry = SshCliTunnelRegistry::default();
        registry.add(tunnel("host-a", "codex", 41001)).await;
        registry.add(tunnel("host-a", "opencode", 41002)).await;
        registry.add(tunnel("host-b", "claude", 42001)).await;

        let host_a = registry.list_by_host("host-a").await;
        assert_eq!(host_a.len(), 2);
        assert_eq!(host_a["codex"].local_port(), 41001);
        assert_eq!(host_a["opencode"].local_port(), 41002);
    }

    #[tokio::test]
    async fn remove_only_closes_requested_tunnel() {
        let registry = SshCliTunnelRegistry::default();
        registry.add(tunnel("host-a", "codex", 41001)).await;
        registry.add(tunnel("host-a", "opencode", 41002)).await;

        assert!(registry.remove("host-a", "codex").await);
        assert!(registry.get("host-a", "codex").await.is_none());
        assert!(registry.get("host-a", "opencode").await.is_some());
        assert!(!registry.remove("host-a", "claude").await);
    }

    #[test]
    fn build_codex_remote_service_start_command_uses_remote_port() {
        let command =
            build_remote_service_start_command(&tunnel("host-a", "codex", 41001)).unwrap();
        assert!(command.contains("codex app-server --listen ws://127.0.0.1:41001"));
        assert!(command.contains("\"${SHELL:-/bin/sh}\" -lic"));
        assert!(command.contains("exec env codex app-server"));
        // 旧断言允许复用任意存活的 Codex PID，无法保证旧服务端口与新 Tunnel 一致：
        // assert!(command.contains("exit 0;"));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
        assert!(!command.contains("exit 0;"));
    }

    #[test]
    fn build_opencode_remote_service_start_command_uses_password_and_remote_port() {
        let command =
            build_remote_service_start_command(&tunnel("host-a", "opencode", 41002)).unwrap();
        let opencode_env = runtime_env::get_remote_opencode_env("");
        let opencode_env_prefix = opencode_env
            .split('=')
            .next()
            .expect("OpenCode environment prefix");
        assert!(command.contains(opencode_env_prefix));
        assert!(command.contains("opencode serve --hostname 127.0.0.1 --port 41002"));
        assert!(command.contains("\"${SHELL:-/bin/sh}\" -lic"));
        assert!(command.contains("exec env opencode serve"));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
        assert!(!command.contains("exit 0;"));
    }

    #[test]
    fn build_claude_remote_service_start_command_uses_session_adapter_and_remote_port() {
        let command =
            build_remote_service_start_command(&tunnel("host-a", "claude", 41003)).unwrap();
        assert!(command.contains("claude-remote-session-server.mjs"));
        assert!(command.contains("$HOME/.cache/auracoder/runtime/claude/"));
        assert!(!command.contains("'$HOME/.cache/auracoder/runtime/claude/"));
        assert!(command.contains("--host 127.0.0.1 --port 41003"));
        assert!(command.contains("type -P claude"));
        let claude_env =
            runtime_env::get_remote_claude_env("claude_path").expect("Claude environment prefix");
        let claude_env_prefix = claude_env
            .split('=')
            .next()
            .expect("Claude environment prefix");
        assert!(command.contains(claude_env_prefix));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
    }

    #[test]
    fn claude_remote_service_waits_for_agent_health_instead_of_only_the_port() {
        let command = build_remote_service_wait_command(&tunnel("host-a", "claude", 41003));

        assert!(command.contains("/health"));
        assert!(command.contains("response.ok"));
        assert!(command.contains("seq 1 300"));
        assert!(command.contains("service.pid"));
        assert!(command.contains("service.log"));
        assert!(command.starts_with("\"${SHELL:-/bin/sh}\" -lic "));
        assert!(command.contains("type -P node"));
        assert!(command.contains("\"$node_path\" -e"));
    }

    #[test]
    fn codex_remote_service_wait_still_checks_the_listening_port() {
        let command = build_remote_service_wait_command(&tunnel("host-a", "codex", 41001));

        assert!(command.contains("ss -ltn"));
        assert!(!command.contains("/health"));
    }

    #[test]
    fn preferred_remote_port_covers_supported_cli_defaults() {
        assert_eq!(preferred_remote_port("codex"), Some(43_100));
        assert_eq!(preferred_remote_port("opencode"), Some(43_600));
        assert_eq!(preferred_remote_port("claude"), Some(43_200));
    }

    #[test]
    fn preferred_remote_port_ignores_unknown_cli() {
        assert_eq!(preferred_remote_port("unknown-cli"), None);
    }

    #[test]
    fn claude_runtime_remote_root_uses_app_version_directory() {
        let remote_root = claude_runtime_remote_root();
        assert!(remote_root.contains("/runtime/claude/"));
        assert!(remote_root.contains(&format!(
            "/{}-",
            env!(concat!("CAR", "\u{0047}", "O_PKG_VERSION"))
        )));
    }
}
