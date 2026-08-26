use std::{
    // 旧实现把各 CLI 的客户端 Engine 缓存在远端服务生命周期中，造成客户端层与
    // 远端服务端生命周期层混合。客户端对象现在由各 CLI 实现自己的运行服务管理。
    // any::Any,
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock,
    },
};

use anyhow::Context;
use tokio::sync::{Mutex, RwLock};

use crate::{
    message_notify_helper::CliHealthReconcileResult,
    ssh::cli_tunnel_registry::{self, SshCliTunnel},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SshCliServiceEntryState {
    Ready,
    Terminating,
}

/// 一台远端机器上一个 CLI 服务的生命周期入口。
///
/// 服务由“SSH 连接配置 ID + CLI ID”唯一标识。各 CLI 接口实现只通过 `get` 取得
/// 已就绪的远端服务端入口；远端服务端的启动、停止和状态由本模块管理。
pub(crate) struct SshCliService {
    connection_id: String,
    cli_id: String,
    generation: u64,
    tunnel: Arc<SshCliTunnel>,
    state: Mutex<SshCliServiceEntryState>,
}

impl SshCliService {
    pub(crate) fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub(crate) fn cli_id(&self) -> &str {
        &self.cli_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// CLI 客户端实现只取得连接远端服务端所需的本地入口，不接触 Tunnel 的创建、
    /// 端口分配和远端服务启停过程。
    pub(crate) fn local_port(&self) -> u16 {
        self.tunnel.local_port()
    }

    pub(crate) fn remote_service_secret(&self) -> Option<&str> {
        self.tunnel.remote_service_secret()
    }

    /*
    旧实现把 CodexEngine、OpenCodeEngine、ClaudeRemoteEngine 等客户端运行对象登记在
    CLI 远端服务生命周期中。该职责属于各 CLI 接口实现自己的客户端运行服务，因此
    保留旧代码作为历史说明，不再参与编译。

    /// 将启动阶段已经创建的 CLI 专属运行时登记到当前服务。
    pub(crate) async fn set_runtime<T>(&self, runtime: Arc<T>) -> anyhow::Result<()>
    where
        T: Any + Send + Sync + 'static,
    {
        let state = self.state.lock().await;
        anyhow::ensure!(
            *state == SshCliServiceEntryState::Ready,
            "SSH 远端 CLI 服务正在终止，不能登记运行时: connection_id={} cli_id={}",
            self.connection_id,
            self.cli_id
        );
        drop(state);

        let service_generation = self
            .tunnel
            .service_lifecycle
            .lock()
            .await
            .service_generation;
        anyhow::ensure!(
            service_generation > 0,
            "SSH 远端 CLI 服务尚未启动，不能登记运行时: connection_id={} cli_id={}",
            self.connection_id,
            self.cli_id
        );

        *self.tunnel.service_runtime.lock().await = Some(RemoteCliRuntimeCache {
            service_generation,
            runtime,
        });
        Ok(())
    }

    /// 供 CLI 接口实现类取得启动阶段登记的专属运行时。
    pub(crate) async fn get_runtime<T>(&self) -> anyhow::Result<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let state = self.state.lock().await;
        anyhow::ensure!(
            *state == SshCliServiceEntryState::Ready,
            "SSH 远端 CLI 服务正在终止，不能读取运行时: connection_id={} cli_id={}",
            self.connection_id,
            self.cli_id
        );
        drop(state);

        let service_generation = self
            .tunnel
            .service_lifecycle
            .lock()
            .await
            .service_generation;
        let runtime = {
            let cached_runtime = self.tunnel.service_runtime.lock().await;
            let entry = cached_runtime.as_ref().with_context(|| {
                format!(
                    "SSH 远端 CLI 服务尚未登记运行时: connection_id={} cli_id={}",
                    self.connection_id, self.cli_id
                )
            })?;
            anyhow::ensure!(
                entry.service_generation == service_generation,
                "SSH 远端 CLI 服务运行时已失效: connection_id={} cli_id={}",
                self.connection_id,
                self.cli_id
            );
            entry.runtime.clone()
        };

        runtime.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "SSH 远端 CLI 服务运行时类型不匹配: connection_id={} cli_id={} expected={}",
                self.connection_id,
                self.cli_id,
                std::any::type_name::<T>()
            )
        })
    }
    */
}

#[derive(Default)]
pub(crate) struct SshCliServiceLifecycleRegistry {
    services: RwLock<HashMap<String, HashMap<String, Arc<SshCliService>>>>,
    mutation_lock: Mutex<()>,
}

static SSH_CLI_SERVICES: LazyLock<SshCliServiceLifecycleRegistry> =
    LazyLock::new(SshCliServiceLifecycleRegistry::default);
static NEXT_SERVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// 取得已由启动阶段登记的远端 CLI 服务；该方法不会启动或重连服务。
pub async fn get(connection_id: &str, cli_id: &str) -> anyhow::Result<Arc<SshCliService>> {
    SSH_CLI_SERVICES.get(connection_id, cli_id).await
}

/// 列出指定 SSH 连接中已经完成登记并处于 Ready 状态的 CLI 服务。
pub async fn list_ready(connection_id: &str) -> Vec<Arc<SshCliService>> {
    SSH_CLI_SERVICES.list_ready(connection_id).await
}

/// 启动并登记一个远端 CLI 服务。相同“连接配置 ID + CLI ID”重复调用时复用已有服务。
pub async fn set(connection_id: &str, cli_id: &str) -> anyhow::Result<Arc<SshCliService>> {
    SSH_CLI_SERVICES.set(connection_id, cli_id).await
}

/// 终止一个远端 CLI 服务并移除其运行时登记。
pub async fn terminate(connection_id: &str, cli_id: &str) -> anyhow::Result<bool> {
    SSH_CLI_SERVICES.terminate(connection_id, cli_id).await
}

/// 定时健康检查：以远端服务进程的真实存活状态为准 reconcile 生命周期 MAP。
///
/// 已登记但连续两次探活失败的服务移除登记；隧道和远端服务都存活但未登记的
/// 服务补登记。隧道的断线恢复由 `cli_tunnel_registry` 负责，本函数只观测远端
/// 服务进程并 reconcile MAP。返回本次 reconcile 是否对 MAP 做过增删，以及阻止
/// 某项增删完成的异常。
// 旧返回值只有 bool，远端登记异常与正常无变化同样无法区分：
// pub async fn reconcile_health(connection_id: &str) -> bool {
pub async fn reconcile_health(connection_id: &str) -> CliHealthReconcileResult {
    let mut changed = false;
    let mut errors = Vec::new();

    let registered = {
        let services = SSH_CLI_SERVICES.services.read().await;
        services
            .get(connection_id)
            .map(|host_services| {
                host_services
                    .iter()
                    .map(|(cli_id, service)| (cli_id.clone(), service.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    for (cli_id, service) in registered {
        if cli_tunnel_registry::probe_remote_cli_service_alive(service.tunnel.as_ref()).await {
            continue;
        }
        // 单次探活失败可能是网络抖动，间隔后再确认一次，避免误杀健康服务。
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if cli_tunnel_registry::probe_remote_cli_service_alive(service.tunnel.as_ref()).await {
            continue;
        }
        match terminate(connection_id, &cli_id).await {
            Ok(_) => {
                changed = true;
                log::info!(
                    "健康检查发现 SSH 远端 CLI 服务不存活，已移除生命周期登记: connection_id={connection_id} cli_id={cli_id}"
                );
            }
            Err(error) => {
                log::warn!(
                    "健康检查移除 SSH 远端 CLI 登记失败: connection_id={connection_id} cli_id={cli_id} error={error:#}"
                );
                errors.push(format!(
                    "SSH 连接 {connection_id} 的 {cli_id} CLI 已不可用，但 AuraCoder 无法移除该服务登记：{error:#}"
                ));
            }
        }
    }

    let tunnels = cli_tunnel_registry::list_by_host(connection_id).await;
    let registered_cli_ids = {
        let services = SSH_CLI_SERVICES.services.read().await;
        services
            .get(connection_id)
            .map(|host_services| host_services.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    for (cli_id, tunnel) in tunnels {
        if registered_cli_ids.contains(&cli_id) {
            continue;
        }
        if !cli_tunnel_registry::probe_remote_cli_service_alive(tunnel.as_ref()).await {
            continue;
        }
        match set(connection_id, &cli_id).await {
            Ok(_) => {
                changed = true;
                log::info!(
                    "健康检查发现未登记的存活 SSH 远端 CLI 服务，已补登记: connection_id={connection_id} cli_id={cli_id}"
                );
            }
            Err(error) => {
                log::warn!(
                    "健康检查补登记 SSH 远端 CLI 服务失败: connection_id={connection_id} cli_id={cli_id} error={error:#}"
                );
                errors.push(format!(
                    "SSH 连接 {connection_id} 的 {cli_id} CLI 服务仍在运行，但 AuraCoder 无法补充生命周期登记：{error:#}"
                ));
            }
        }
    }

    // 旧实现只返回 changed，异常信息到日志为止：
    // changed
    CliHealthReconcileResult { changed, errors }
}

/// 终止当前应用已登记的全部远端 CLI 服务。
pub async fn terminate_all() -> anyhow::Result<()> {
    SSH_CLI_SERVICES.terminate_all().await
}

impl SshCliServiceLifecycleRegistry {
    async fn list_ready(&self, connection_id: &str) -> Vec<Arc<SshCliService>> {
        let mut services = self
            .services
            .read()
            .await
            .get(connection_id)
            .map(|host_services| host_services.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        services.sort_by(|left, right| left.cli_id().cmp(right.cli_id()));

        let mut ready = Vec::with_capacity(services.len());
        for service in services {
            if *service.state.lock().await == SshCliServiceEntryState::Ready {
                ready.push(service);
            }
        }
        ready
    }

    async fn get(&self, connection_id: &str, cli_id: &str) -> anyhow::Result<Arc<SshCliService>> {
        let service = self
            .services
            .read()
            .await
            .get(connection_id)
            .and_then(|host_services| host_services.get(cli_id))
            .cloned()
            .with_context(|| {
                format!(
                    "SSH 远端 CLI 服务未在启动阶段登记: connection_id={connection_id} cli_id={cli_id}"
                )
            })?;

        let state = service.state.lock().await;
        anyhow::ensure!(
            *state == SshCliServiceEntryState::Ready,
            "SSH 远端 CLI 服务正在终止: connection_id={connection_id} cli_id={cli_id}"
        );
        drop(state);
        Ok(service)
    }

    async fn set(&self, connection_id: &str, cli_id: &str) -> anyhow::Result<Arc<SshCliService>> {
        // 服务创建必须按注册表串行执行，避免并发刷新为同一个 connection_id + cli_id
        // 重复启动远端服务端。
        let _mutation_guard = self.mutation_lock.lock().await;
        let existing = self
            .services
            .read()
            .await
            .get(connection_id)
            .and_then(|host_services| host_services.get(cli_id))
            .cloned();
        if let Some(service) = existing {
            let state = service.state.lock().await;
            anyhow::ensure!(
                *state == SshCliServiceEntryState::Ready,
                "SSH 远端 CLI 服务正在终止，不能重复登记: connection_id={connection_id} cli_id={cli_id}"
            );
            drop(state);
            return Ok(service);
        }

        let tunnel = cli_tunnel_registry::get(connection_id, cli_id)
            .await
            .with_context(|| {
                format!("SSH CLI Tunnel 未建立: connection_id={connection_id} cli_id={cli_id}")
            })?;
        cli_tunnel_registry::start_remote_cli_service_for_tunnel(tunnel.as_ref()).await?;
        let service = Arc::new(SshCliService {
            connection_id: connection_id.to_string(),
            cli_id: cli_id.to_string(),
            generation: NEXT_SERVICE_GENERATION.fetch_add(1, Ordering::Relaxed),
            tunnel,
            state: Mutex::new(SshCliServiceEntryState::Ready),
        });

        let registered = {
            let mut services = self.services.write().await;
            let host_services = services.entry(connection_id.to_string()).or_default();
            if let Some(existing) = host_services.get(cli_id) {
                existing.clone()
            } else {
                host_services.insert(cli_id.to_string(), service.clone());
                service
            }
        };
        let state = registered.state.lock().await;
        anyhow::ensure!(
            *state == SshCliServiceEntryState::Ready,
            "SSH 远端 CLI 服务正在终止，不能重复登记: connection_id={connection_id} cli_id={cli_id}"
        );
        drop(state);
        Ok(registered)
    }

    async fn terminate(&self, connection_id: &str, cli_id: &str) -> anyhow::Result<bool> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let service = self.get(connection_id, cli_id).await?;
        {
            let mut state = service.state.lock().await;
            *state = SshCliServiceEntryState::Terminating;
        }

        let result =
            cli_tunnel_registry::stop_remote_cli_service_for_tunnel(service.tunnel.as_ref())
                .await
                .map(|_| true);
        match result {
            Ok(stopped) => {
                let mut services = self.services.write().await;
                let remove_connection = if let Some(host_services) = services.get_mut(connection_id)
                {
                    let remove_service = host_services
                        .get(cli_id)
                        .map(|registered| Arc::ptr_eq(registered, &service))
                        .unwrap_or(false);
                    if remove_service {
                        host_services.remove(cli_id);
                    }
                    host_services.is_empty()
                } else {
                    false
                };
                if remove_connection {
                    services.remove(connection_id);
                }
                Ok(stopped)
            }
            Err(error) => {
                *service.state.lock().await = SshCliServiceEntryState::Ready;
                Err(error)
            }
        }
    }

    async fn terminate_all(&self) -> anyhow::Result<()> {
        let keys = self
            .services
            .read()
            .await
            .iter()
            .flat_map(|(connection_id, host_services)| {
                host_services
                    .keys()
                    .map(move |cli_id| (connection_id.clone(), cli_id.clone()))
            })
            .collect::<Vec<_>>();

        let mut errors = Vec::new();
        for (connection_id, cli_id) in keys {
            if let Err(error) = self.terminate(&connection_id, &cli_id).await {
                errors.push(format!(
                    "connection_id={connection_id} cli_id={cli_id} error={error:#}"
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("停止 SSH 远端 CLI 服务失败: {}", errors.join("; "));
        }
    }
}
