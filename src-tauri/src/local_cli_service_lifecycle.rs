use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock,
    },
};

use anyhow::Context;
use tokio::sync::{Mutex, RwLock};

use crate::engines::{
    claude_sidecar::ClaudeSidecarEngine, codex::CodexEngine, opencode::OpenCodeEngine,
};
use crate::{
    commands::harness::detect_via_login_shell, message_notify_helper::CliHealthReconcileResult,
    runtime_env,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalCliServiceEntryState {
    Ready,
    Terminating,
}

pub(crate) enum LocalCliHandle {
    Codex(Arc<CodexEngine>),
    OpenCode(Arc<OpenCodeEngine>),
    Claude(Arc<ClaudeSidecarEngine>),
}

/// 本机一个 CLI 服务的生命周期入口。
///
/// 服务由 CLI ID 唯一标识。业务代码只通过 `get` 取得已经由 AuraCoder 启动阶段
/// 创建并登记的本地 CLI 句柄。
pub(crate) struct LocalCliService {
    cli_id: String,
    generation: u64,
    handle: LocalCliHandle,
    state: Mutex<LocalCliServiceEntryState>,
}

impl LocalCliService {
    pub(crate) fn cli_id(&self) -> &str {
        &self.cli_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn handle(&self) -> &LocalCliHandle {
        &self.handle
    }
}

#[derive(Default)]
struct LocalCliServiceLifecycleRegistry {
    services: RwLock<HashMap<String, Arc<LocalCliService>>>,
    resource_dir: RwLock<Option<PathBuf>>,
    mutation_lock: Mutex<()>,
}

static LOCAL_CLI_SERVICES: LazyLock<LocalCliServiceLifecycleRegistry> =
    LazyLock::new(LocalCliServiceLifecycleRegistry::default);
static NEXT_SERVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// 本机支持接入生命周期的聊天 CLI 及其可执行文件名称。
const LOCAL_CLI_COMMANDS: [(&str, &str); 3] = [
    ("codex", "codex"),
    ("opencode", "opencode"),
    ("claude", "claude"),
];

pub(crate) struct LocalCliServiceLifecycle;

impl LocalCliServiceLifecycle {
    /// 探测本机三种聊天 CLI，并将已安装的 CLI 服务逐个登记到生命周期 MAP。
    // 旧入口没有接收 Tauri 实际解析出的安装包资源目录，Claude 生命周期只能回退到
    // 编译期路径；该路径在安装后的用户机器上不存在：
    // pub(crate) async fn init() -> anyhow::Result<()> {
    pub(crate) async fn init(resource_dir: Option<PathBuf>) -> anyhow::Result<()> {
        *LOCAL_CLI_SERVICES.resource_dir.write().await = resource_dir;
        for (cli_id, command) in LOCAL_CLI_COMMANDS {
            let found = runtime_env::resolve_executable(command).is_some()
                || detect_via_login_shell(command, "--version").await.is_some();
            if !found {
                log::warn!("本机未探测到 CLI，跳过生命周期登记: cli_id={cli_id}");
                continue;
            }

            Self::set(cli_id)
                .await
                .with_context(|| format!("初始化本地 CLI 服务失败: cli_id={cli_id}"))?;
        }

        Ok(())
    }

    /// 定时健康检查：以可执行文件探测结果为准 reconcile 生命周期 MAP。
    ///
    /// 探测到但未登记的 CLI 立即登记（覆盖 AuraCoder 运行期间新安装的情况）；
    /// 已登记但探测不到的 CLI 移除登记（覆盖运行期间被卸载的情况）。
    /// 返回本次 reconcile 是否对 MAP 做过增删，以及阻止某项增删完成的异常。
    // 旧返回值只有 bool，登记失败与正常无变化都会返回 false，调用方无法区分：
    // pub(crate) async fn reconcile_health() -> bool {
    pub(crate) async fn reconcile_health() -> CliHealthReconcileResult {
        let mut changed = false;
        let mut errors = Vec::new();
        for (cli_id, command) in LOCAL_CLI_COMMANDS {
            let found = runtime_env::resolve_executable(command).is_some()
                || detect_via_login_shell(command, "--version").await.is_some();
            let registered = LOCAL_CLI_SERVICES
                .services
                .read()
                .await
                .contains_key(cli_id);
            if found == registered {
                continue;
            }

            if found {
                match Self::set(cli_id).await {
                    Ok(_) => {
                        changed = true;
                        log::info!("健康检查发现本机新装 CLI，已登记生命周期: cli_id={cli_id}");
                    }
                    Err(error) => {
                        log::warn!("健康检查登记本机 CLI 失败: cli_id={cli_id} error={error:#}");
                        errors.push(format!(
                            "本机 {cli_id} CLI 已被探测到，但 AuraCoder 无法启动并登记该服务：{error:#}"
                        ));
                    }
                }
            } else {
                match Self::terminate(cli_id).await {
                    Ok(_) => {
                        changed = true;
                        log::info!(
                            "健康检查发现本机 CLI 已不可用，已移除生命周期登记: cli_id={cli_id}"
                        );
                    }
                    Err(error) => {
                        log::warn!(
                            "健康检查移除本机 CLI 登记失败: cli_id={cli_id} error={error:#}"
                        );
                        errors.push(format!(
                            "本机 {cli_id} CLI 已不可用，但 AuraCoder 无法移除该服务登记：{error:#}"
                        ));
                    }
                }
            }
        }
        // 旧实现只返回 changed，异常信息到日志为止：
        // changed
        CliHealthReconcileResult { changed, errors }
    }

    /// 取得已经由 AuraCoder 启动阶段登记的本地 CLI 服务；该方法不会启动服务。
    pub(crate) async fn get(cli_id: &str) -> anyhow::Result<Arc<LocalCliService>> {
        LOCAL_CLI_SERVICES.get(cli_id).await
    }

    /// 列出已经完成登记并处于 Ready 状态的本地 CLI 服务。
    pub(crate) async fn list_ready() -> Vec<Arc<LocalCliService>> {
        LOCAL_CLI_SERVICES.list_ready().await
    }

    /// 启动并登记一个本地 CLI 服务。相同 CLI ID 重复调用时复用已有服务。
    pub(crate) async fn set(cli_id: &str) -> anyhow::Result<Arc<LocalCliService>> {
        LOCAL_CLI_SERVICES.set(cli_id).await
    }

    /// 终止一个本地 CLI 服务并移除登记。
    pub(crate) async fn terminate(cli_id: &str) -> anyhow::Result<bool> {
        LOCAL_CLI_SERVICES.terminate(cli_id).await
    }

    /// 终止当前 AuraCoder 进程已经登记的全部本地 CLI 服务。
    pub(crate) async fn terminate_all() -> anyhow::Result<()> {
        LOCAL_CLI_SERVICES.terminate_all().await
    }
}

impl LocalCliServiceLifecycleRegistry {
    async fn get(&self, cli_id: &str) -> anyhow::Result<Arc<LocalCliService>> {
        let service = self
            .services
            .read()
            .await
            .get(cli_id)
            .cloned()
            .with_context(|| format!("本地 CLI 服务未在 AuraCoder 启动阶段登记: cli_id={cli_id}"))?;

        let state = service.state.lock().await;
        anyhow::ensure!(
            *state == LocalCliServiceEntryState::Ready,
            "本地 CLI 服务正在终止: cli_id={cli_id}"
        );
        drop(state);
        Ok(service)
    }

    async fn list_ready(&self) -> Vec<Arc<LocalCliService>> {
        let mut services = self
            .services
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        services.sort_by(|left, right| left.cli_id().cmp(right.cli_id()));

        let mut ready = Vec::with_capacity(services.len());
        for service in services {
            if *service.state.lock().await == LocalCliServiceEntryState::Ready {
                ready.push(service);
            }
        }
        ready
    }

    async fn set(&self, cli_id: &str) -> anyhow::Result<Arc<LocalCliService>> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let existing = self.services.read().await.get(cli_id).cloned();
        if let Some(service) = existing {
            let state = service.state.lock().await;
            anyhow::ensure!(
                *state == LocalCliServiceEntryState::Ready,
                "本地 CLI 服务正在终止，不能重复登记: cli_id={cli_id}"
            );
            drop(state);
            return Ok(service);
        }

        let handle = match cli_id {
            "codex" => {
                let engine = Arc::new(CodexEngine::default());
                engine.prewarm().await?;
                LocalCliHandle::Codex(engine)
            }
            "opencode" => {
                let engine = Arc::new(OpenCodeEngine::default());
                engine.prewarm().await?;
                LocalCliHandle::OpenCode(engine)
            }
            "claude" => {
                let engine = Arc::new(ClaudeSidecarEngine::default());
                let resource_dir = self.resource_dir.read().await.clone();
                let resource_engine = engine.clone();
                tokio::task::spawn_blocking(move || {
                    resource_engine.set_resource_dir(resource_dir);
                })
                .await
                .context("向 Claude 本地 CLI 服务注入安装包资源目录失败")?;
                engine.prewarm().await?;
                LocalCliHandle::Claude(engine)
            }
            _ => anyhow::bail!("不支持的本地 CLI 工具: {cli_id}"),
        };

        let service = Arc::new(LocalCliService {
            cli_id: cli_id.to_string(),
            generation: NEXT_SERVICE_GENERATION.fetch_add(1, Ordering::Relaxed),
            handle,
            state: Mutex::new(LocalCliServiceEntryState::Ready),
        });

        let registered = {
            let mut services = self.services.write().await;
            if let Some(existing) = services.get(cli_id) {
                existing.clone()
            } else {
                services.insert(cli_id.to_string(), service.clone());
                service
            }
        };

        let state = registered.state.lock().await;
        anyhow::ensure!(
            *state == LocalCliServiceEntryState::Ready,
            "本地 CLI 服务正在终止，不能重复登记: cli_id={cli_id}"
        );
        drop(state);
        Ok(registered)
    }

    async fn terminate(&self, cli_id: &str) -> anyhow::Result<bool> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let service = self.get(cli_id).await?;

        let mut state = service.state.lock().await;
        *state = LocalCliServiceEntryState::Terminating;
        drop(state);

        let mut services = self.services.write().await;
        let remove_service = services
            .get(cli_id)
            .map(|registered| Arc::ptr_eq(registered, &service))
            .unwrap_or(false);
        if remove_service {
            services.remove(cli_id);
        }
        Ok(remove_service)
    }

    async fn terminate_all(&self) -> anyhow::Result<()> {
        let cli_ids = self
            .services
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut errors = Vec::new();

        for cli_id in cli_ids {
            if let Err(error) = self.terminate(&cli_id).await {
                errors.push(format!("cli_id={cli_id} error={error:#}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("停止本地 CLI 服务失败: {}", errors.join("; "));
        }
    }
}
