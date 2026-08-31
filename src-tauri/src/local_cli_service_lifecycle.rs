use std::{
    collections::HashMap,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, RwLock},
    time::{sleep, timeout},
};
use uuid::Uuid;

use crate::engines::{claude_sidecar::ClaudeSidecarEngine, codex::CodexEngine};
use crate::{
    cli_tools::{factory::CliToolFactory, CliLocationKind, CliMcpRuntime, CliTool},
    commands::harness::detect_via_login_shell, mcp_gateway::AuraCoderMcpGateway,
    message_notify_helper::CliHealthReconcileResult, process_utils, runtime_env,
};

/// OpenCode 本机服务启动后提供给协议客户端的连接信息。
///
/// 该对象只携带 HTTP 连接参数和生命周期代数，不向业务层暴露子进程或运行目录。
#[derive(Clone, Debug)]
pub(crate) struct LocalOpenCodeEndpoint {
    /// 本机 OpenCode HTTP 服务的基础地址。
    pub(crate) base_url: String,
    /// 本机 OpenCode HTTP 服务的 Basic Auth 密码。
    pub(crate) password: String,
    /// 启动服务对应的 OpenCode CLI 版本文本。
    pub(crate) version: Option<String>,
    /// 本机 OpenCode 服务实例的生命周期代数。
    pub(crate) generation: u64,
}

/// 一个按项目目录登记的本机 OpenCode 服务进程。
///
/// 该类型只在本机 CLI 生命周期内部使用，负责子进程、隔离运行目录和终止清理。
struct LocalOpenCodeService {
    /// 服务启动时绑定的项目目录。
    cwd: String,
    /// 服务 HTTP 基础地址。
    base_url: String,
    /// 服务 Basic Auth 密码。
    password: String,
    /// 启动服务对应的 OpenCode CLI 版本文本。
    version: Option<String>,
    /// 服务生命周期代数，用于识别重启后的新实例。
    generation: u64,
    /// 服务子进程句柄。
    child: Mutex<Option<Child>>,
    /// 服务隔离配置和运行文件所在目录。
    run_dir: PathBuf,
}

impl LocalOpenCodeService {
    /// 将内部服务状态转换为不含进程句柄的协议连接信息。
    fn endpoint(&self) -> LocalOpenCodeEndpoint {
        LocalOpenCodeEndpoint {
            base_url: self.base_url.clone(),
            password: self.password.clone(),
            version: self.version.clone(),
            generation: self.generation,
        }
    }

    /// 终止本机 OpenCode 服务并清理其任务与隔离运行目录。
    async fn terminate(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        if let Some(mut process) = child.take() {
            if let Err(error) = process.kill().await {
                log::warn!("停止本机 OpenCode 服务进程失败: {error}");
            }
        }
        drop(child);
        if let Err(error) = std::fs::remove_dir_all(&self.run_dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "清理本机 OpenCode 隔离运行目录失败: path={} error={error}",
                    self.run_dir.display()
                );
                return Err(error).with_context(|| {
                    format!("清理本机 OpenCode 隔离运行目录失败: {}", self.run_dir.display())
                });
            }
        }
        Ok(())
    }
}

/// 本机 OpenCode CLI 生命周期句柄。
///
/// 该句柄按 cwd 复用已经启动的服务，并在生命周期终止时统一结束全部服务；
/// 协议解析和会话业务仍由 OpenCodeEngine 负责。
pub(crate) struct LocalOpenCodeServiceHandle {
    /// 按项目目录登记的本机 OpenCode 服务表。
    services: Mutex<HashMap<String, Arc<LocalOpenCodeService>>>,
    /// 保护服务创建、复用和终止的并发互斥锁。
    mutation_lock: Mutex<()>,
    /// MCP Gateway 的 HTTP 地址，用于写入隔离 OpenCode 配置。
    mcp_gateway_endpoint: String,
    /// MCP Gateway 的本地 CLI 私有租约 Token。
    mcp_gateway_token: String,
}

static NEXT_OPENCODE_SERVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// OpenCode 服务启动和就绪检查使用的最长等待时间。
const OPENCODE_STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
/// OpenCode 服务健康检查使用的最长等待时间。
const OPENCODE_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// OpenCode stdout 宣布 HTTP 服务已监听时使用的前缀。
const SERVER_READY_PREFIX: &str = "opencode server listening";
/// OpenCode CLI 版本和模型目录命令使用的最长等待时间。
const OPENCODE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// OpenCode 本机服务只绑定回环地址，避免暴露到外部网络。
const DEFAULT_HOST: &str = "127.0.0.1";

impl LocalOpenCodeServiceHandle {
    /// 创建一个绑定 MCP Gateway 配置的本机 OpenCode 生命周期句柄。
    pub(crate) fn new(mcp_gateway_endpoint: String, mcp_gateway_token: String) -> Self {
        Self {
            services: Mutex::new(HashMap::new()),
            mutation_lock: Mutex::new(()),
            mcp_gateway_endpoint,
            mcp_gateway_token,
        }
    }

    /// 取得指定 cwd 已就绪的 OpenCode HTTP endpoint；首次访问时启动并登记服务。
    pub(crate) async fn endpoint_for_cwd(&self, cwd: &str) -> Result<LocalOpenCodeEndpoint> {
        let cwd = cwd.trim();
        anyhow::ensure!(!cwd.is_empty(), "本机 OpenCode 服务缺少项目目录");
        if let Some(service) = self.services.lock().await.get(cwd).cloned() {
            return Ok(service.endpoint());
        }

        let _mutation_guard = self.mutation_lock.lock().await;
        if let Some(service) = self.services.lock().await.get(cwd).cloned() {
            return Ok(service.endpoint());
        }

        let service = start_local_opencode_service(
            cwd,
            &self.mcp_gateway_endpoint,
            &self.mcp_gateway_token,
        )
        .await
        .with_context(|| format!("启动本机 OpenCode 服务失败: cwd={cwd}"))?;
        let service = Arc::new(service);
        let endpoint = service.endpoint();
        self.services
            .lock()
            .await
            .insert(cwd.to_string(), service);
        Ok(endpoint)
    }

    /// 通过本机 OpenCode CLI 生命周期执行纯文本命令并返回标准输出。
    ///
    /// 该方法只负责解析可执行文件、创建进程和收集输出，不解析模型或其它业务数据。
    pub(crate) async fn run_command(
        &self,
        cwd: Option<&str>,
        args: &[&str],
    ) -> Result<String> {
        let executable = runtime_env::resolve_executable("opencode")
            .context("`opencode` executable not found")?;
        run_local_opencode_command(&executable, cwd, args)
            .await
            .with_context(|| format!("执行本机 OpenCode 命令失败: args={args:?}"))
    }

    /// 终止指定 cwd 的本机 OpenCode 服务并移除生命周期登记。
    pub(crate) async fn terminate_cwd(&self, cwd: &str) -> Result<bool> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let service = self.services.lock().await.remove(cwd.trim());
        let Some(service) = service else {
            return Ok(false);
        };
        service.terminate().await?;
        Ok(true)
    }

    /// 终止并清理该句柄登记的全部本机 OpenCode 服务。
    pub(crate) async fn terminate_all(&self) -> Result<()> {
        let _mutation_guard = self.mutation_lock.lock().await;
        let services = self
            .services
            .lock()
            .await
            .drain()
            .map(|(_, service)| service)
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for service in services {
            if let Err(error) = service.terminate().await {
                errors.push(format!("cwd={} error={error:#}", service.cwd));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("停止本机 OpenCode 服务失败: {}", errors.join("; "));
        }
    }
}

/// 通过已解析的 OpenCode 可执行文件运行本机 CLI 文本命令。
///
/// 该函数只负责进程生命周期和原始文本输出，不解析任何 CLI 业务语义。
async fn run_local_opencode_command(
    executable: &Path,
    cwd: Option<&str>,
    args: &[&str],
) -> Result<String> {
    let mut command = Command::new(executable);
    process_utils::configure_tokio_command(&mut command);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) {
        command.current_dir(cwd);
    }
    command.envs(runtime_env::get(executable).await);

    let output = timeout(OPENCODE_COMMAND_TIMEOUT, command.output())
        .await
        .context("执行本机 OpenCode CLI 命令超时")?
        .context("创建本机 OpenCode CLI 命令进程失败")?;
    if !output.status.success() {
        anyhow::bail!(
            "本机 OpenCode CLI 命令执行失败: status={:?}, stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 启动一个按 cwd 隔离配置的本机 OpenCode 服务，并等待其 HTTP endpoint 就绪。
async fn start_local_opencode_service(
    cwd: &str,
    mcp_gateway_endpoint: &str,
    mcp_gateway_token: &str,
) -> Result<LocalOpenCodeService> {
    let executable = runtime_env::resolve_executable("opencode")
        .context("`opencode` executable not found")?;
    let version = match run_local_opencode_command(&executable, Some(cwd), &["--version"]).await {
        Ok(output) => output
            .lines()
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        Err(error) => {
            log::warn!(
                "读取本机 OpenCode CLI 版本失败，继续启动服务: cwd={cwd}, error={error:#}"
            );
            None
        }
    };
    let port = allocate_loopback_port()?;
    let password = Uuid::new_v4().to_string();
    let run_dir = runtime_env::app_data_dir()
        .join("computer-control")
        .join("opencode-runs")
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("创建本机 OpenCode 运行目录失败: {}", run_dir.display()))?;
    let config_dir = run_dir.join(".opencode");
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("创建本机 OpenCode 隔离配置目录失败: {}", config_dir.display()))?;
    anyhow::ensure!(
        !mcp_gateway_endpoint.trim().is_empty() && !mcp_gateway_token.trim().is_empty(),
        "OpenCode 的 AuraCoder MCP 配置不完整"
    );

    let config_path = config_dir.join("opencode.json");
    let mut config = if config_path.is_file() {
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("读取本机 OpenCode 配置失败: {}", config_path.display()))?;
        serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("本机 OpenCode 配置不是有效 JSON: {}", config_path.display()))?
    } else {
        Value::Object(serde_json::Map::new())
    };
    let config_object = config
        .as_object_mut()
        .context("本机 OpenCode 配置必须是 JSON 对象")?;
    let mcp_object = config_object
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context("本机 OpenCode MCP 配置必须是 JSON 对象")?;
    let gateway_config = runtime_env::opencode_mcp_gateway_authenticated_config(
        mcp_gateway_endpoint,
        &format!("Bearer {mcp_gateway_token}"),
    );
    let gateway_entry = gateway_config
        .get("mcp")
        .and_then(Value::as_object)
        .and_then(|mcp| mcp.get("auracoder"))
        .cloned()
        .context("OpenCode MCP Gateway 配置生成失败")?;
    mcp_object.insert("auracoder".to_string(), gateway_entry);
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).context("序列化本机 OpenCode 配置失败")?,
    )
    .with_context(|| format!("写入本机 OpenCode 配置失败: {}", config_path.display()))?;

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<String>();
    let mut command = Command::new(&executable);
    process_utils::configure_tokio_command(&mut command);
    command
        .arg("serve")
        .arg("--hostname")
        .arg(DEFAULT_HOST)
        .arg("--port")
        .arg(port.to_string())
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command.envs(runtime_env::get_opencode_env(&executable, &password, &run_dir).await);

    let mut child = command.spawn().with_context(|| {
        format!("启动本机 OpenCode 服务进程失败: executable={}", executable.display())
    })?;
    let stdout = child.stdout.take().context("OpenCode stdout 不可用")?;
    let stderr = child.stderr.take().context("OpenCode stderr 不可用")?;
    tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::debug!("opencode stdout: {line}");
            if line.starts_with(SERVER_READY_PREFIX) {
                if let Some(tx) = ready_tx.take() {
                    let url = line
                        .split_whitespace()
                        .last()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("http://{DEFAULT_HOST}:{port}"));
                    let _ = tx.send(url);
                }
            }
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::debug!("opencode stderr: {line}");
        }
    });

    let base_url = timeout(OPENCODE_STARTUP_TIMEOUT, ready_rx)
        .await
        .context("等待本机 OpenCode 服务启动超时")?
        .context("本机 OpenCode 服务在启动完成前退出")?;
    let service = LocalOpenCodeService {
        cwd: cwd.to_string(),
        base_url,
        password,
        version,
        generation: NEXT_OPENCODE_SERVICE_GENERATION.fetch_add(1, Ordering::Relaxed),
        child: Mutex::new(Some(child)),
        run_dir,
    };
    if let Err(error) = wait_for_local_opencode_health(&service).await {
        service.terminate().await.ok();
        return Err(error);
    }
    Ok(service)
}

/// 为本机 OpenCode 生命周期分配尚未占用的回环端口。
fn allocate_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind((DEFAULT_HOST, 0))?;
    Ok(listener.local_addr()?.port())
}

/// 轮询本机 OpenCode HTTP 健康接口，确认服务已经能够接收协议请求。
async fn wait_for_local_opencode_health(service: &LocalOpenCodeService) -> Result<()> {
    let client = reqwest::Client::new();
    let started = Instant::now();
    loop {
        let result = client
            .get(format!(
                "{}/global/health",
                service.base_url.trim_end_matches('/')
            ))
            .headers(local_opencode_auth_headers(&service.password))
            .send()
            .await;
        if let Ok(response) = result {
            if let Ok(response) = response.error_for_status() {
                let health = response.json::<Value>().await?;
                if health.get("healthy").and_then(Value::as_bool) == Some(true) {
                    return Ok(());
                }
            }
        }
        if started.elapsed() > OPENCODE_HEALTH_TIMEOUT {
            anyhow::bail!("本机 OpenCode HTTP 服务未达到健康状态");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// 生成本机 OpenCode 健康检查使用的 Basic Auth 请求头。
fn local_opencode_auth_headers(password: &str) -> reqwest::header::HeaderMap {
    use base64::{engine::general_purpose, Engine as _};
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

    let mut headers = HeaderMap::new();
    let token = general_purpose::STANDARD.encode(format!("opencode:{password}"));
    if let Ok(value) = HeaderValue::from_str(&format!("Basic {token}")) {
        headers.insert(AUTHORIZATION, value);
    }
    headers
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalCliServiceEntryState {
    Ready,
    Terminating,
}

pub(crate) enum LocalCliHandle {
    /// 本机 Codex CLI 的生命周期句柄。
    Codex(Arc<CodexEngine>),
    /// 本机 OpenCode CLI 服务的生命周期句柄。
    OpenCode(Arc<LocalOpenCodeServiceHandle>),
    /// 本机 Claude Code CLI 的生命周期句柄。
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
    /// 当前生命周期服务登记到 Gateway 的 CLI MCP 实现。
    cli: Arc<dyn CliTool>,
    /// 当前本地 CLI 服务持有的 MCP Gateway 私有租约 Token。
    mcp_token: String,
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
    /// 当前应用 MCP Gateway 的绑定引用，用于本地 CLI 租约注册和撤销。
    mcp_gateway: RwLock<Option<Arc<AuraCoderMcpGateway>>>,
    /// 创建本机 CLI MCP 实现的统一工厂。
    factory: RwLock<Option<Arc<CliToolFactory>>>,
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
    /// 绑定本地 CLI 生命周期使用的 MCP Gateway。
    pub(crate) async fn bind_mcp_gateway(
        gateway: Arc<AuraCoderMcpGateway>,
        factory: Arc<CliToolFactory>,
    ) {
        *LOCAL_CLI_SERVICES.mcp_gateway.write().await = Some(gateway);
        *LOCAL_CLI_SERVICES.factory.write().await = Some(factory);
    }

    /// 为本地 CLI 当前轮次登记 AuraCoder MCP Gateway 可信上下文。
    ///
    /// 业务调用方只提供 CLI、引擎线程和 AuraCoder 轮次标识；本方法从 Ready 服务
    /// 内部取得私有 MCP Token，避免 Token 暴露到聊天业务层。
    pub(crate) async fn register_mcp_context(
        cli_id: &str,
        engine_thread_id: &str,
        turn_id: &str,
    ) -> anyhow::Result<()> {
        LOCAL_CLI_SERVICES
            .register_mcp_context(cli_id, engine_thread_id, turn_id)
            .await
    }

    /// 清除本地 CLI 当前轮次的 AuraCoder MCP Gateway 可信上下文。
    ///
    /// 清理失败会返回原始错误链，调用方可以记录异常但不需要接触私有 MCP Token。
    pub(crate) async fn clear_mcp_context(cli_id: &str) -> anyhow::Result<()> {
        LOCAL_CLI_SERVICES.clear_mcp_context(cli_id).await
    }

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
            .with_context(|| {
                format!("本地 CLI 服务未在 AuraCoder 启动阶段登记: cli_id={cli_id}")
            })?;

        let state = service.state.lock().await;
        anyhow::ensure!(
            *state == LocalCliServiceEntryState::Ready,
            "本地 CLI 服务正在终止: cli_id={cli_id}"
        );
        drop(state);
        Ok(service)
    }

    /// 使用 Ready 本地 CLI 服务持有的私有 Token 登记 Gateway 可信上下文。
    async fn register_mcp_context(
        &self,
        cli_id: &str,
        engine_thread_id: &str,
        turn_id: &str,
    ) -> anyhow::Result<()> {
        let service = self.get(cli_id).await?;
        let gateway = self
            .mcp_gateway
            .read()
            .await
            .clone()
            .context("本地 CLI 服务尚未绑定 MCP Gateway")?;
        gateway
            .register_trusted_context(&service.mcp_token, engine_thread_id, turn_id)
            .await
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("登记本地 CLI MCP 可信上下文失败: cli_id={cli_id}"))
    }

    /// 使用 Ready 本地 CLI 服务持有的私有 Token 清除 Gateway 可信上下文。
    async fn clear_mcp_context(&self, cli_id: &str) -> anyhow::Result<()> {
        let service = self.get(cli_id).await?;
        let gateway = self
            .mcp_gateway
            .read()
            .await
            .clone()
            .context("本地 CLI 服务尚未绑定 MCP Gateway")?;
        anyhow::ensure!(
            gateway.clear_trusted_context(&service.mcp_token).await,
            "清除本地 CLI MCP 可信上下文失败：租约不存在或已撤销: cli_id={cli_id}"
        );
        Ok(())
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

        let gateway = self
            .mcp_gateway
            .read()
            .await
            .clone()
            .context("本地 CLI 服务尚未绑定 MCP Gateway")?;
        let factory = self
            .factory
            .read()
            .await
            .clone()
            .context("本地 CLI 服务尚未绑定 CLI Tool Factory")?;
        let cli = factory.create_mcp_cli(
            cli_id,
            CliMcpRuntime {
                cli_id: cli_id.to_string(),
                location: CliLocationKind::Local,
            },
        )?;
        let mcp_lease = gateway
            .register_client(cli_id, cli_id, cli.clone())
            .await
            .map_err(|error| anyhow::anyhow!("注册本地 CLI MCP 租约失败: {error}"))?;
        let endpoint = match gateway.endpoint().await {
            Some(endpoint) => endpoint,
            None => {
                gateway.revoke_client(&mcp_lease.token).await;
                anyhow::bail!("MCP Gateway 尚未启动，无法配置本地 CLI 服务")
            }
        };
        let handle_result: anyhow::Result<LocalCliHandle> = async {
            Ok(match cli_id {
                "codex" => {
                    let engine = Arc::new(CodexEngine::default());
                    engine.set_mcp_gateway_connection(endpoint.clone(), mcp_lease.token.clone());
                    engine.prewarm().await?;
                    LocalCliHandle::Codex(engine)
                }
                "opencode" => {
                    LocalCliHandle::OpenCode(Arc::new(LocalOpenCodeServiceHandle::new(
                        endpoint.clone(),
                        mcp_lease.token.clone(),
                    )))
                }
                "claude" => {
                    let engine = Arc::new(ClaudeSidecarEngine::default());
                    engine.set_mcp_gateway_connection(endpoint.clone(), mcp_lease.token.clone());
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
            })
        }
        .await;
        let handle = match handle_result {
            Ok(handle) => handle,
            Err(error) => {
                gateway.revoke_client(&mcp_lease.token).await;
                return Err(error);
            }
        };

        let service = Arc::new(LocalCliService {
            cli_id: cli_id.to_string(),
            generation: NEXT_SERVICE_GENERATION.fetch_add(1, Ordering::Relaxed),
            handle,
            cli,
            mcp_token: mcp_lease.token.clone(),
            state: Mutex::new(LocalCliServiceEntryState::Ready),
        });

        let registered = {
            let mut services = self.services.write().await;
            if let Some(existing) = services.get(cli_id) {
                existing.clone()
            } else {
                services.insert(cli_id.to_string(), service.clone());
                service.clone()
            }
        };

        if !Arc::ptr_eq(&registered, &service) {
            gateway.revoke_client(&mcp_lease.token).await;
        }

        let ready = *registered.state.lock().await == LocalCliServiceEntryState::Ready;
        if !ready {
            if Arc::ptr_eq(&registered, &service) {
                gateway.revoke_client(&mcp_lease.token).await;
            }
            anyhow::bail!("本地 CLI 服务正在终止，不能重复登记: cli_id={cli_id}");
        }
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
        drop(services);
        if remove_service {
            if let LocalCliHandle::OpenCode(handle) = service.handle() {
                handle
                    .terminate_all()
                    .await
                    .with_context(|| format!("终止本机 OpenCode 服务失败: cli_id={cli_id}"))?;
            }
            if let Some(gateway) = self.mcp_gateway.read().await.clone() {
                gateway.revoke_client(&service.mcp_token).await;
            }
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

#[cfg(test)]
#[path = "../tests/unit/local_cli_service_lifecycle_tests.rs"]
mod tests;
