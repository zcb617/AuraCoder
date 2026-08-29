use std::{collections::HashMap, convert::Infallible, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming, header, http::HeaderValue, server::conn::http1::Builder as Http1Builder,
    service::service_fn, Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    auracoder_thread_mcp_service::AuraCoderThreadMcpService,
    computer_control_service::ComputerControlService,
};

/// AuraCoder MCP Gateway 的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GatewayState {
    Stopped,
    Starting,
    Running,
    Failed,
    Stopping,
}

/// 客户端租约的访问策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientAccessPolicy {
    Local,
    SshRemoteThreadOnly,
}

/// 注册成功后返回给调用方的独立 Bearer 租约。
#[derive(Debug, Clone)]
pub(crate) struct ClientLease {
    /// 只在内存中保存的强随机访问令牌。
    pub token: String,
    /// 租约所属引擎。
    pub engine: String,
    /// 租约所属引擎实例。
    pub instance_id: String,
    /// 租约访问策略。
    pub policy: ClientAccessPolicy,
    /// SSH 租约的连接标识。
    pub ssh_connection_id: Option<String>,
    /// 创建租约时的服务代次；Gateway 重启后旧代次立即失效。
    pub generation: u64,
}

impl ClientLease {
    /// 返回仅供后续适配器使用的 Authorization Header 值。
    pub(crate) fn authorization_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

/// 可信调用上下文。
#[derive(Debug, Clone)]
struct TrustedContext {
    thread_id: String,
    turn_id: String,
}

/// 内存中的客户端记录。
#[derive(Debug, Clone)]
struct ClientRecord {
    lease: ClientLease,
    context: Option<TrustedContext>,
}

/// Gateway 生命周期和服务任务状态。
struct GatewayLifecycle {
    /// 当前状态。
    state: GatewayState,
    /// 最近一次失败的原始错误。
    last_error: Option<String>,
    /// 服务任务取消令牌。
    cancellation: Option<CancellationToken>,
    /// 服务任务句柄。
    task: Option<JoinHandle<()>>,
    /// 实际 loopback 地址。
    local_addr: Option<SocketAddr>,
    /// 每次成功启动递增的服务代次。
    generation: u64,
}

impl Default for GatewayLifecycle {
    fn default() -> Self {
        Self {
            state: GatewayState::Stopped,
            last_error: None,
            cancellation: None,
            task: None,
            local_addr: None,
            generation: 0,
        }
    }
}

/// 阶段一唯一真实 MCP HTTP Gateway。
pub(crate) struct AuraCoderMcpGateway {
    /// 串行化启动和关闭。
    operation: Mutex<()>,
    /// 生命周期状态。
    lifecycle: Arc<Mutex<GatewayLifecycle>>,
    /// 电脑操作服务。
    computer: Arc<ComputerControlService>,
    /// 会话工具服务。
    threads: Arc<AuraCoderThreadMcpService>,
    /// 内存租约表。
    clients: Arc<RwLock<HashMap<String, ClientRecord>>>,
    /// MCP 会话与租约 token 的绑定。
    sessions: Arc<RwLock<HashMap<String, String>>>,
    /// 活动工具调用与其租约取消令牌。
    active_calls: Arc<RwLock<HashMap<String, (String, CancellationToken)>>>,
    /// 供所有既有 HTTP 连接读取的当前 MCP 工具目录。
    catalog: Arc<RwLock<Vec<CatalogTool>>>,
}

impl AuraCoderMcpGateway {
    /// 创建绑定现有业务服务的 Gateway。
    pub(crate) fn new(
        computer: Arc<ComputerControlService>,
        threads: Arc<AuraCoderThreadMcpService>,
    ) -> Self {
        Self {
            operation: Mutex::new(()),
            lifecycle: Arc::new(Mutex::new(GatewayLifecycle::default())),
            computer,
            threads,
            clients: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            active_calls: Arc::new(RwLock::new(HashMap::new())),
            catalog: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// 注册本机客户端租约。
    pub(crate) async fn register_local_client(
        &self,
        engine: &str,
        instance_id: &str,
    ) -> Result<ClientLease, String> {
        self.register(engine, instance_id, ClientAccessPolicy::Local, None)
            .await
    }

    /// 注册 SSH 远端线程客户端租约。
    pub(crate) async fn register_ssh_remote_thread_client(
        &self,
        engine: &str,
        instance_id: &str,
        ssh_connection_id: &str,
    ) -> Result<ClientLease, String> {
        self.register(
            engine,
            instance_id,
            ClientAccessPolicy::SshRemoteThreadOnly,
            Some(ssh_connection_id),
        )
        .await
    }

    /// 撤销租约及其上下文。
    pub(crate) async fn revoke_client(&self, token: &str) -> bool {
        let token = token.trim();
        let removed = self.clients.write().await.remove(token).is_some();
        if removed {
            self.sessions
                .write()
                .await
                .retain(|_, session_token| session_token != token);
            let calls = self.active_calls.read().await;
            for (lease_token, cancellation) in calls.values() {
                if lease_token == token {
                    cancellation.cancel();
                }
            }
        }
        removed
    }

    /// 登记未来引擎桥接提供的可信上下文。
    pub(crate) async fn register_trusted_context(
        &self,
        token: &str,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), String> {
        log::info!(
            "MCP Gateway register trusted context entered: token={}, thread_id={}, turn_id={}",
            token,
            thread_id,
            turn_id,
        );
        if thread_id.trim().is_empty() || turn_id.trim().is_empty() {
            let error = "trusted context 缺少 thread_id 或 turn_id".to_string();
            log::info!(
                "MCP Gateway register trusted context failed: token={}, thread_id={}, turn_id={}, error={}",
                token,
                thread_id,
                turn_id,
                error,
            );
            return Err(error);
        }
        let mut clients = self.clients.write().await;
        let Some(client) = clients.get_mut(token.trim()) else {
            let error = "client lease 不存在或已撤销".to_string();
            log::info!(
                "MCP Gateway register trusted context failed: token={}, thread_id={}, turn_id={}, error={}",
                token,
                thread_id,
                turn_id,
                error,
            );
            return Err(error);
        };
        client.context = Some(TrustedContext {
            thread_id: thread_id.trim().to_string(),
            turn_id: turn_id.trim().to_string(),
        });
        log::info!(
            "MCP Gateway register trusted context succeeded: token={}, thread_id={}, turn_id={}, context={:?}",
            token,
            thread_id,
            turn_id,
            client.context,
        );
        Ok(())
    }

    /// 清除可信上下文。
    pub(crate) async fn clear_trusted_context(&self, token: &str) -> bool {
        let mut clients = self.clients.write().await;
        let Some(client) = clients.get_mut(token.trim()) else {
            log::info!(
                "MCP Gateway clear trusted context result: token={}, context=None, cleared=false",
                token,
            );
            return false;
        };
        log::info!(
            "MCP Gateway clear trusted context entered: token={}, context={:?}",
            token,
            client.context,
        );
        client.context = None;
        log::info!(
            "MCP Gateway clear trusted context result: token={}, context=None, cleared=true",
            token,
        );
        true
    }

    /// 启动真实 HTTP MCP 服务。
    pub(crate) async fn start(&self) -> Result<(), String> {
        let _guard = self.operation.lock().await;
        let (old_cancel, old_task) = {
            let mut life = self.lifecycle.lock().await;
            if matches!(life.state, GatewayState::Running | GatewayState::Starting) {
                return Ok(());
            }
            life.state = GatewayState::Starting;
            life.local_addr = None;
            (life.cancellation.take(), life.task.take())
        };
        Self::finish(old_cancel, old_task).await;
        // 重新启动时先吊销上一代所有租约和 MCP session，避免旧 token 跨代复用。
        self.clients.write().await.clear();
        self.sessions.write().await.clear();
        for (_, (_, cancellation)) in self.active_calls.write().await.drain() {
            cancellation.cancel();
        }
        let catalog = match self.catalog() {
            Ok(value) => value,
            Err(error) => {
                log::error!("MCP Gateway 目录失败，原始错误：{error}");
                let mut life = self.lifecycle.lock().await;
                life.state = GatewayState::Failed;
                life.last_error = Some(error.clone());
                return Err(error);
            }
        };
        *self.catalog.write().await = catalog;
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) => {
                log::error!("MCP Gateway 绑定失败，原始错误：{error:?}");
                let mut life = self.lifecycle.lock().await;
                life.state = GatewayState::Failed;
                life.last_error = Some(error.to_string());
                return Err(error.to_string());
            }
        };
        let address = match listener.local_addr() {
            Ok(address) => address,
            Err(error) => {
                log::error!("MCP Gateway 读取监听地址失败，原始错误：{error:?}");
                let mut life = self.lifecycle.lock().await;
                life.state = GatewayState::Failed;
                life.last_error = Some(error.to_string());
                return Err(error.to_string());
            }
        };
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_server(
            listener,
            self.task_state(),
            // 旧实现：Arc::new(catalog) 作为固定目录传入本次 HTTP 服务任务。
            cancel.clone(),
        ));
        let mut life = self.lifecycle.lock().await;
        life.state = GatewayState::Running;
        life.last_error = None;
        life.generation = life.generation.saturating_add(1);
        life.local_addr = Some(address);
        life.cancellation = Some(cancel);
        life.task = Some(task);
        Ok(())
    }

    /// 重新读取电脑操作和会话工具，原子替换供既有 HTTP 连接使用的目录。
    pub(crate) async fn refresh_catalog(&self) -> Result<(), String> {
        let catalog = self.catalog()?;
        *self.catalog.write().await = catalog;
        Ok(())
    }

    /// 先完整停止当前代次，再启动新代次。
    pub(crate) async fn restart(&self) -> Result<(), String> {
        self.shutdown().await;
        self.start().await
    }

    /// 停止 HTTP MCP 服务并清空所有内存租约。
    pub(crate) async fn shutdown(&self) {
        let _guard = self.operation.lock().await;
        let already_stopped = self.lifecycle.lock().await.state == GatewayState::Stopped;
        if already_stopped {
            self.clients.write().await.clear();
            self.sessions.write().await.clear();
            for (_, (_, cancellation)) in self.active_calls.write().await.drain() {
                cancellation.cancel();
            }
            return;
        }
        let (cancel, task) = {
            let mut life = self.lifecycle.lock().await;
            life.state = GatewayState::Stopping;
            life.local_addr = None;
            (life.cancellation.take(), life.task.take())
        };
        Self::finish(cancel, task).await;
        self.clients.write().await.clear();
        self.sessions.write().await.clear();
        for (_, (_, cancellation)) in self.active_calls.write().await.drain() {
            cancellation.cancel();
        }
        self.lifecycle.lock().await.state = GatewayState::Stopped;
    }

    /// 返回当前生命周期状态。
    pub(crate) async fn status(&self) -> GatewayState {
        self.lifecycle.lock().await.state
    }

    /// 返回真实服务地址。
    pub(crate) async fn local_addr(&self) -> Option<SocketAddr> {
        self.lifecycle.lock().await.local_addr
    }

    /// 返回供 MCP 客户端使用的完整 Endpoint；服务未就绪时不返回地址。
    pub(crate) async fn endpoint(&self) -> Option<String> {
        self.local_addr()
            .await
            .map(|address| format!("http://{address}{MCP_PATH}"))
    }

    /// 写入租约表并生成随机 token。
    async fn register(
        &self,
        engine: &str,
        instance: &str,
        policy: ClientAccessPolicy,
        ssh: Option<&str>,
    ) -> Result<ClientLease, String> {
        if engine.trim().is_empty() || instance.trim().is_empty() {
            return Err("client lease 缺少 engine 或 instance_id".to_string());
        }
        let generation = {
            let life = self.lifecycle.lock().await;
            if life.state != GatewayState::Running {
                return Err("MCP Gateway 当前不可签发客户端租约".to_string());
            }
            life.generation
        };
        let token = format!("ac_{}", Uuid::new_v4().simple());
        let lease = ClientLease {
            token: token.clone(),
            engine: engine.trim().to_string(),
            instance_id: instance.trim().to_string(),
            policy,
            ssh_connection_id: ssh
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            generation,
        };
        let mut clients = self.clients.write().await;
        let replaced_tokens = clients
            .iter()
            .filter(|(_, existing)| {
                existing.lease.engine == lease.engine
                    && existing.lease.instance_id == lease.instance_id
            })
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        clients.retain(|_, existing| {
            existing.lease.engine != lease.engine || existing.lease.instance_id != lease.instance_id
        });
        clients.insert(
            token,
            ClientRecord {
                lease: lease.clone(),
                context: None,
            },
        );
        drop(clients);
        if !replaced_tokens.is_empty() {
            self.sessions
                .write()
                .await
                .retain(|_, session_token| !replaced_tokens.iter().any(|old| old == session_token));
            let active_calls = self.active_calls.read().await;
            for (lease_token, cancellation) in active_calls.values() {
                if replaced_tokens.iter().any(|old| old == lease_token) {
                    cancellation.cancel();
                }
            }
        }
        Ok(lease)
    }

    /// 读取并合并两个现有服务的工具目录。
    fn catalog(&self) -> Result<Vec<CatalogTool>, String> {
        let mut result = Vec::new();
        match self.computer.sdk_tool_specs() {
            Ok(specs) => {
                for spec in specs {
                    result.push(CatalogTool::from(spec, Owner::Computer)?);
                }
            }
            Err(error) if error.starts_with("sdk_unavailable:") => {
                // CUA SDK 不可用时保留会话工具，避免电脑操作故障阻断整个 Gateway。
                log::warn!("MCP Gateway CUA 工具不可用，继续提供会话工具；原始错误：{error}");
            }
            Err(error) => return Err(format!("电脑操作目录不可用：{error}")),
        }
        for spec in self.threads.tool_specs() {
            result.push(CatalogTool::from(spec, Owner::Thread)?);
        }
        let mut names = HashMap::new();
        for tool in &result {
            if names.insert(tool.name.clone(), ()).is_some() {
                return Err(format!("工具目录存在重名：{}", tool.name));
            }
        }
        Ok(result)
    }

    /// 组装 HTTP 任务所需的共享状态。
    fn task_state(&self) -> TaskState {
        TaskState {
            computer: self.computer.clone(),
            threads: self.threads.clone(),
            clients: self.clients.clone(),
            sessions: self.sessions.clone(),
            active_calls: self.active_calls.clone(),
            lifecycle: self.lifecycle.clone(),
            catalog: self.catalog.clone(),
        }
    }

    /// 等待服务任务退出。
    async fn finish(cancel: Option<CancellationToken>, task: Option<JoinHandle<()>>) {
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// HTTP 任务共享状态。
#[derive(Clone)]
struct TaskState {
    computer: Arc<ComputerControlService>,
    threads: Arc<AuraCoderThreadMcpService>,
    clients: Arc<RwLock<HashMap<String, ClientRecord>>>,
    sessions: Arc<RwLock<HashMap<String, String>>>,
    active_calls: Arc<RwLock<HashMap<String, (String, CancellationToken)>>>,
    lifecycle: Arc<Mutex<GatewayLifecycle>>,
    /// 供 HTTP 请求读取的当前 MCP 工具目录。
    catalog: Arc<RwLock<Vec<CatalogTool>>>,
}

/// 工具业务归属。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Computer,
    Thread,
}

/// Gateway 中的工具目录项。
#[derive(Clone)]
struct CatalogTool {
    name: String,
    description: String,
    schema: Value,
    owner: Owner,
}

impl CatalogTool {
    /// 从现有服务工具规格建立目录项。
    fn from(spec: Value, owner: Owner) -> Result<Self, String> {
        let name = spec
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "MCP 工具缺少 name".to_string())?;
        let description = spec
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("MCP 工具 `{name}` 缺少 description"))?;
        let schema = spec
            .get("inputSchema")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| format!("MCP 工具 `{name}` 缺少对象 inputSchema"))?;
        Ok(Self {
            name: name.to_string(),
            description: description.to_string(),
            schema,
            owner,
        })
    }
    /// 序列化为 tools/list 的标准工具对象。
    fn json(&self) -> Value {
        json!({"name": self.name, "description": self.description, "inputSchema": self.schema})
    }
}

const MCP_PATH: &str = "/mcp";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";
const MAX_MCP_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

async fn run_server(
    listener: TcpListener,
    state: TaskState,
    // 旧实现：catalog: Arc<Vec<CatalogTool>>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            result = listener.accept() => match result {
                Ok((stream, peer)) => {
                    let request_state = state.clone();
                    // 旧实现：let request_catalog = catalog.clone();
                    tokio::spawn(async move {
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |request| {
                            // 旧实现：handle_request(request, request_state.clone(), request_catalog.clone())
                            handle_request(request, request_state.clone())
                        });
                        if let Err(error) = Http1Builder::new().serve_connection(io, service).await {
                            log::debug!("MCP Gateway HTTP 连接已关闭，peer={peer}, 原始错误：{error}");
                        }
                    });
                }
                Err(error) => {
                    if !cancellation.is_cancelled() {
                        log::error!("MCP Gateway 接收连接失败，原始错误：{error:?}");
                        let mut lifecycle = state.lifecycle.lock().await;
                        lifecycle.state = GatewayState::Failed;
                        lifecycle.last_error = Some(error.to_string());
                        lifecycle.local_addr = None;
                    }
                    break;
                }
            },
        }
    }
}

async fn handle_request(
    request: Request<Incoming>,
    state: TaskState,
    // 旧实现：catalog: Arc<Vec<CatalogTool>>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    log::info!(
        "MCP Gateway request entered: method={}, uri={}, headers={:?}",
        request.method(),
        request.uri(),
        request.headers(),
    );
    if request.uri().path() != MCP_PATH {
        return Ok(simple_response(
            StatusCode::NOT_FOUND,
            "MCP endpoint 不存在",
        ));
    }

    if request.method() == Method::OPTIONS {
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = StatusCode::NO_CONTENT;
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST, OPTIONS"));
        return Ok(response);
    }
    if request.method() != Method::POST {
        let mut response = simple_response(StatusCode::METHOD_NOT_ALLOWED, "MCP 仅支持 POST");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST, OPTIONS"));
        return Ok(response);
    }

    let token = match bearer_token(request.headers()) {
        Some(token) => token,
        None => return Ok(authentication_error()),
    };
    let client = {
        let clients = state.clients.read().await;
        clients.get(&token).cloned()
    };
    let Some(client) = client else {
        log::info!(
            "MCP Gateway authentication failed after token extraction: token={}, method={}, uri={}",
            token,
            request.method(),
            request.uri(),
        );
        return Ok(authentication_error());
    };
    log::info!(
        "MCP Gateway client lease resolved: token={}, lease={:?}, context={:?}",
        token,
        client.lease,
        client.context,
    );
    {
        let lifecycle = state.lifecycle.lock().await;
        if lifecycle.state != GatewayState::Running
            || lifecycle.generation != client.lease.generation
        {
            return Ok(json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                rpc_error(Value::Null, -32001, "gateway_not_ready"),
                None,
            ));
        }
    }

    if let Some(session_id) = request
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        let sessions = state.sessions.read().await;
        if sessions.get(session_id) != Some(&token) {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32002, "mcp_session_invalid"),
                None,
            ));
        }
    }

    let body = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            log::warn!("MCP Gateway 读取请求体失败，原始错误：{error}");
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, "invalid_json"),
                None,
            ));
        }
    };
    log::info!(
        "MCP Gateway request body collected: token={}, lease={:?}, body={}",
        token,
        client.lease,
        String::from_utf8_lossy(&body),
    );
    if body.len() > MAX_MCP_BODY_BYTES {
        return Ok(simple_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "MCP 请求体过大",
        ));
    }
    let rpc = match serde_json::from_slice::<RpcRequest>(&body) {
        Ok(rpc) => rpc,
        Err(error) => {
            log::warn!("MCP Gateway JSON-RPC 解析失败，原始错误：{error}");
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, "invalid_json"),
                None,
            ));
        }
    };
    log::info!(
        "MCP Gateway RPC request parsed: token={}, lease={:?}, rpc={rpc:?}",
        token,
        client.lease,
    );
    if rpc
        .jsonrpc
        .as_deref()
        .is_some_and(|version| version != "2.0")
    {
        return Ok(json_response(
            StatusCode::BAD_REQUEST,
            rpc_error(rpc.id.unwrap_or(Value::Null), -32600, "invalid_request"),
            None,
        ));
    }

    // MCP notifications 没有响应体；取消通知仍需先撤销对应活动调用。
    if rpc.id.is_none() && rpc.method == "notifications/cancelled" {
        if let Some(request_id) = rpc
            .params
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|params| params.get("requestId").or_else(|| params.get("request_id")))
        {
            let key = active_call_key(&token, request_id);
            if let Some((_, cancellation)) = state.active_calls.read().await.get(&key) {
                cancellation.cancel();
            }
        }
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = StatusCode::ACCEPTED;
        log::info!(
            "MCP Gateway notification response: status={}, token={}, rpc={rpc:?}",
            StatusCode::ACCEPTED,
            token,
        );
        return Ok(response);
    }
    // 其他 MCP notifications 没有响应体；返回 202 可避免客户端把通知误判成普通结果。
    if rpc.id.is_none() && rpc.method.starts_with("notifications/") {
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = StatusCode::ACCEPTED;
        log::info!(
            "MCP Gateway notification response: status={}, token={}, rpc={rpc:?}",
            StatusCode::ACCEPTED,
            token,
        );
        return Ok(response);
    }

    let request_id = rpc.id.clone().unwrap_or(Value::Null);
    let (result, session_id) = match rpc.method.as_str() {
        "initialize" => {
            let session_id = format!("ac_session_{}", Uuid::new_v4().simple());
            state
                .sessions
                .write()
                .await
                .insert(session_id.clone(), token.clone());
            let requested_protocol = rpc
                .params
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(MCP_PROTOCOL_VERSION);
            (
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": requested_protocol,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "AuraCoder MCP Gateway", "version": "1.0.1" }
                    }
                }),
                Some(session_id),
            )
        }
        "notifications/initialized" => (json!({}), None),
        "ping" => (
            json!({ "jsonrpc": "2.0", "id": request_id, "result": {} }),
            None,
        ),
        "tools/list" => {
            // 旧实现：let tools = visible_tools(&client.lease, &catalog);
            let tools = {
                let catalog = state.catalog.read().await;
                visible_tools(&client.lease, &catalog)
            };
            (
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": { "tools": tools }
                }),
                None,
            )
        }
        "tools/call" => {
            // 旧实现：dispatch_tool_call(&client, &rpc, &catalog, state.clone()).await
            let call_result = dispatch_tool_call(&client, &rpc, state.clone()).await;
            (
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": call_result
                }),
                None,
            )
        }
        _ => (
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": RpcError { code: -32601, message: "method_not_found".to_string() }
            }),
            None,
        ),
    };

    log::info!(
        "MCP Gateway RPC result constructed: token={}, engine={}, instance_id={}, request_id={:?}, method={}, result={result:?}, session_id={:?}",
        token,
        client.lease.engine,
        client.lease.instance_id,
        rpc.id,
        rpc.method,
        session_id,
    );
    let session_header = session_id.as_deref();
    Ok(json_response(StatusCode::OK, result, session_header))
}

fn bearer_token(headers: &hyper::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn authentication_error() -> Response<Full<Bytes>> {
    let mut response = json_response(
        StatusCode::UNAUTHORIZED,
        rpc_error(Value::Null, -32000, "authentication_failed"),
        None,
    );
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn simple_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    log::info!(
        "MCP Gateway simple response: status={}, message={}",
        status,
        message,
    );
    let mut response = Response::new(Full::new(Bytes::from(message.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn json_response(
    status: StatusCode,
    value: Value,
    session_id: Option<&str>,
) -> Response<Full<Bytes>> {
    let body = match serde_json::to_vec(&value) {
        Ok(body) => body,
        Err(error) => {
            log::error!("MCP Gateway 序列化响应失败，原始错误：{error}");
            b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"internal_error\"}}".to_vec()
        }
    };
    log::info!(
        "MCP Gateway JSON response: status={}, value={value:?}, body={}, session_id={:?}",
        status,
        String::from_utf8_lossy(&body),
        session_id,
    );
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response
                .headers_mut()
                .insert(header::HeaderName::from_static("mcp-session-id"), value);
        }
    }
    response
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn visible_tools(lease: &ClientLease, catalog: &[CatalogTool]) -> Vec<Value> {
    catalog
        .iter()
        .filter(|tool| lease.policy == ClientAccessPolicy::Local || tool.owner == Owner::Thread)
        .map(CatalogTool::json)
        .collect()
}

async fn dispatch_tool_call(
    client: &ClientRecord,
    rpc: &RpcRequest,
    // 旧实现：catalog: &[CatalogTool],
    state: TaskState,
) -> Value {
    let dispatch_started_at = std::time::Instant::now();
    log::info!(
        "MCP Gateway dispatch tool call entered: lease={:?}, context={:?}, rpc={rpc:?}",
        client.lease,
        client.context,
    );
    let Some(params) = rpc.params.as_ref().and_then(Value::as_object) else {
        return tool_error("invalid_request: tools/call params 必须是对象");
    };
    let Some(name) = params.get("name").and_then(Value::as_str).map(str::trim) else {
        return tool_error("invalid_request: tools/call 缺少 name");
    };
    // 旧实现：let Some(tool) = catalog.iter().find(|tool| tool.name == name) else {
    // 旧实现：    return tool_error("tool_not_found");
    // 旧实现：};
    let tool = {
        let catalog = state.catalog.read().await;
        catalog.iter().find(|tool| tool.name == name).cloned()
    };
    let Some(tool) = tool else {
        return tool_error("tool_not_found");
    };
    if client.lease.policy == ClientAccessPolicy::SshRemoteThreadOnly
        && tool.owner == Owner::Computer
    {
        return tool_error("tool_not_allowed");
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let context = client
        .context
        .clone()
        .or_else(|| context_from_meta(params.get("_meta")));
    let Some(context) = context else {
        log::info!(
            "MCP Gateway dispatch tool call missing context: token={}, engine={}, instance_id={}, request_id={:?}, tool={}, arguments={arguments:?}, thread_id=None, turn_id=None, owner={:?}",
            client.lease.token,
            client.lease.engine,
            client.lease.instance_id,
            rpc.id,
            name,
            tool.owner,
        );
        return tool_error("invocation_context_missing: trusted context 缺少 thread_id 或 turn_id");
    };
    log::info!(
        "MCP Gateway dispatch tool call resolved: token={}, engine={}, instance_id={}, request_id={:?}, tool={}, arguments={arguments:?}, thread_id={}, turn_id={}, owner={:?}",
        client.lease.token,
        client.lease.engine,
        client.lease.instance_id,
        rpc.id,
        name,
        context.thread_id,
        context.turn_id,
        tool.owner,
    );
    let active_call_id = active_call_key(
        &client.lease.token,
        rpc.id
            .as_ref()
            .unwrap_or(&Value::String(Uuid::new_v4().simple().to_string())),
    );
    let cancellation = CancellationToken::new();
    state.active_calls.write().await.insert(
        active_call_id.clone(),
        (client.lease.token.clone(), cancellation.clone()),
    );
    let call_id = rpc
        .id
        .as_ref()
        .map(|value| match value {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| format!("mcp-call-{}", Uuid::new_v4().simple()));
    let result = match tool.owner {
        Owner::Thread => {
            // 旧实现：直接 await invoke_for_engine，不监听租约替换产生的停止信号。
            tokio::select! {
                _ = cancellation.cancelled() => {
                    Err("request_cancelled: 工具调用已停止".to_string())
                }
                result = state.threads.invoke_for_engine(
                    &client.lease.engine,
                    &context.thread_id,
                    &tool.name,
                    arguments,
                ) => result,
            }
        }
        Owner::Computer => {
            state
                .computer
                .invoke_for_engine(
                    &client.lease.engine,
                    &context.thread_id,
                    &context.turn_id,
                    &tool.name,
                    &call_id,
                    arguments,
                    cancellation,
                )
                .await
        }
    };
    state.active_calls.write().await.remove(&active_call_id);
    log::info!(
        "MCP Gateway dispatch tool call execution finished: token={}, engine={}, instance_id={}, request_id={:?}, tool={}, thread_id={}, turn_id={}, owner={:?}, elapsed_ms={}, raw_result={result:?}",
        client.lease.token,
        client.lease.engine,
        client.lease.instance_id,
        rpc.id,
        name,
        context.thread_id,
        context.turn_id,
        tool.owner,
        dispatch_started_at.elapsed().as_millis(),
    );
    let mapped_result = match result {
        Ok(value) => tool_success(value),
        Err(error) => {
            log::warn!("MCP Gateway 工具执行失败，tool={name}，原始错误：{error}");
            mapped_tool_error(&error)
        }
    };
    log::info!(
        "MCP Gateway dispatch tool call mapped result: token={}, engine={}, instance_id={}, request_id={:?}, tool={}, result={mapped_result:?}",
        client.lease.token,
        client.lease.engine,
        client.lease.instance_id,
        rpc.id,
        name,
    );
    mapped_result
}

fn context_from_meta(meta: Option<&Value>) -> Option<TrustedContext> {
    let object = meta?.as_object()?;
    let thread_id = object
        .get("threadId")
        .or_else(|| object.get("thread_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let turn_id = object
        .get("turnId")
        .or_else(|| object.get("turn_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(TrustedContext {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
    })
}

fn active_call_key(token: &str, request_id: &Value) -> String {
    format!(
        "{token}:{}",
        serde_json::to_string(request_id).unwrap_or_default()
    )
}

fn tool_success(value: Value) -> Value {
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        return json!({ "content": content, "isError": false });
    }
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
        }],
        "isError": false
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

fn mapped_tool_error(error: &str) -> Value {
    let code = error.split(':').next().unwrap_or_default().trim();
    let (public_code, public_message) = match code {
        "authorization_required" => ("authorization_required", "该操作需要用户授权"),
        "permission_denied" => ("authorization_denied", "用户拒绝了该操作"),
        "request_timeout" => ("gateway_timeout", "工具调用等待超时"),
        "target_scope_mismatch" => ("tool_not_allowed", "当前目标不允许执行该工具"),
        "tool_not_available" => ("tool_not_found", "工具当前不可用"),
        "computer_control_disabled" => ("tool_not_allowed", "电脑操作能力未启用"),
        "sdk_unavailable" => ("tool_execution_failed", "电脑操作服务当前不可用"),
        _ => ("tool_execution_failed", "工具执行失败，请稍后重试"),
    };
    tool_error(&format!("{public_code}: {public_message}"))
}

#[cfg(test)]
mod tests {
    use super::{
        visible_tools, AuraCoderMcpGateway, CatalogTool, ClientAccessPolicy, GatewayState, Owner,
    };
    use crate::{
        auracoder_thread_mcp_service::AuraCoderThreadMcpService,
        computer_control_service::ComputerControlService, db::Database,
    };
    use serde_json::{json, Value};
    use std::{sync::Arc, time::Duration};

    fn test_gateway() -> AuraCoderMcpGateway {
        let path =
            std::env::temp_dir().join(format!("auracoder-mcp-gateway-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(path).expect("test database should open");
        AuraCoderMcpGateway::new(
            Arc::new(ComputerControlService::default()),
            Arc::new(AuraCoderThreadMcpService::new(db)),
        )
    }

    async fn post_json(
        url: &str,
        token: &str,
        session: Option<&str>,
        payload: Value,
    ) -> reqwest::Response {
        let client = reqwest::Client::new();
        let mut request = client.post(url).bearer_auth(token).json(&payload);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request.send().await.expect("request should complete")
    }

    #[tokio::test]
    async fn lifecycle_and_streamable_http_protocol_are_available() {
        let gateway = test_gateway();
        assert_eq!(gateway.status().await, GatewayState::Stopped);
        assert!(gateway
            .register_local_client("codex", "before-start")
            .await
            .is_err());

        gateway.start().await.expect("gateway should start");
        assert_eq!(gateway.status().await, GatewayState::Running);
        let address = gateway.local_addr().await.expect("listener address");
        assert_eq!(
            address.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        assert_ne!(address.port(), 0);

        let lease = gateway
            .register_local_client("codex", "test-instance")
            .await
            .expect("local lease should be issued");
        assert_eq!(lease.policy, ClientAccessPolicy::Local);
        let url = format!("http://{address}/mcp");

        let initialize = post_json(
            &url,
            &lease.token,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }
            }),
        )
        .await;
        assert_eq!(initialize.status(), reqwest::StatusCode::OK);
        let session = initialize
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .expect("initialize should return a session id");
        let initialize_body: Value = initialize.json().await.expect("initialize JSON");
        assert_eq!(
            initialize_body["result"]["serverInfo"]["name"],
            "AuraCoder MCP Gateway"
        );

        let list = post_json(
            &url,
            &lease.token,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
        )
        .await;
        assert_eq!(list.status(), reqwest::StatusCode::OK);
        let list_body: Value = list.json().await.expect("tools/list JSON");
        let names = list_body["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"get_auracoder_thread_message_count"));
        assert!(names.contains(&"get_auracoder_thread_messages_page"));

        let ping = post_json(
            &url,
            &lease.token,
            Some(&session),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
        )
        .await;
        assert_eq!(ping.status(), reqwest::StatusCode::OK);
        assert_eq!(
            ping.json::<Value>().await.expect("ping JSON")["result"],
            json!({})
        );

        let unauthorized = reqwest::Client::new()
            .post(&url)
            .json(&json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }))
            .send()
            .await
            .expect("unauthorized request should complete");
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        gateway.restart().await.expect("gateway should restart");
        let restarted_address = gateway
            .local_addr()
            .await
            .expect("restarted listener address");
        let restarted_lease = gateway
            .register_local_client("codex", "test-instance")
            .await
            .expect("restarted lease should be issued");
        assert_ne!(restarted_lease.token, lease.token);
        let old_generation = post_json(
            &format!("http://{restarted_address}/mcp"),
            &lease.token,
            None,
            json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" }),
        )
        .await;
        assert_eq!(old_generation.status(), reqwest::StatusCode::UNAUTHORIZED);

        gateway.shutdown().await;
        assert_eq!(gateway.status().await, GatewayState::Stopped);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(gateway.local_addr().await.is_none());
        assert!(gateway.revoke_client(&lease.token).await == false);
    }

    #[tokio::test]
    async fn remote_lease_cannot_see_or_call_computer_tools() {
        let gateway = test_gateway();
        gateway.start().await.expect("gateway should start");
        let lease = gateway
            .register_ssh_remote_thread_client("claude", "remote-instance", "ssh-1")
            .await
            .expect("remote lease should be issued");
        assert_eq!(lease.policy, ClientAccessPolicy::SshRemoteThreadOnly);
        assert_eq!(lease.ssh_connection_id.as_deref(), Some("ssh-1"));
        gateway.shutdown().await;
    }

    #[tokio::test]
    async fn tools_call_dispatches_to_the_existing_thread_service() {
        let path = std::env::temp_dir().join(format!(
            "auracoder-mcp-gateway-dispatch-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Database::open(path).expect("test database should open");
        let workspace = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!("auracoder-mcp-workspace-{}", uuid::Uuid::new_v4()))
                .to_string_lossy(),
        )
        .expect("workspace should be created");
        let target_thread = crate::db::threads::create_thread(
            &db,
            &workspace.id,
            "codex",
            "model",
            "MCP dispatch test",
        )
        .expect("thread should be created");
        crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "one message",
            None,
            Some("codex"),
            Some("model"),
            None,
        )
        .expect("message should be inserted");

        let threads = Arc::new(AuraCoderThreadMcpService::new(db));
        threads.bind_engine_thread("codex", "engine-thread", &workspace.id);
        let gateway =
            AuraCoderMcpGateway::new(Arc::new(ComputerControlService::default()), threads);
        gateway.start().await.expect("gateway should start");
        let lease = gateway
            .register_local_client("codex", "dispatch-instance")
            .await
            .expect("local lease should be issued");
        gateway
            .register_trusted_context(&lease.token, "engine-thread", "turn-1")
            .await
            .expect("trusted context should be registered");
        let address = gateway.local_addr().await.expect("listener address");
        let url = format!("http://{address}/mcp");

        let initialize = post_json(
            &url,
            &lease.token,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": "init-dispatch",
                "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }
            }),
        )
        .await;
        let session = initialize
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .expect("initialize should return a session id");
        let call = post_json(
            &url,
            &lease.token,
            Some(&session),
            json!({
                "jsonrpc": "2.0",
                "id": "call-dispatch",
                "method": "tools/call",
                "params": {
                    "name": "get_auracoder_thread_message_count",
                    "arguments": { "thread_id": target_thread.id }
                }
            }),
        )
        .await;
        assert_eq!(call.status(), reqwest::StatusCode::OK);
        let call_body: Value = call.json().await.expect("tools/call JSON");
        assert_eq!(call_body["result"]["isError"], false);
        let text = call_body["result"]["content"][0]["text"]
            .as_str()
            .expect("tools/call text content");
        let service_result: Value = serde_json::from_str(text).expect("service result JSON");
        assert_eq!(service_result["message_count"], 1);
        gateway.shutdown().await;
    }

    /// 验证既有客户端在不重启 Gateway 的情况下读取到刷新后的工具目录。
    #[tokio::test]
    async fn refresh_catalog_updates_tools_for_existing_client_without_restart() {
        let gateway = test_gateway();
        gateway.start().await.expect("gateway should start");
        let lease = gateway
            .register_local_client("codex", "refresh-instance")
            .await
            .expect("local lease should be issued");
        let address = gateway.local_addr().await.expect("listener address");
        let endpoint = format!("http://{address}/mcp");
        gateway.catalog.write().await.push(CatalogTool {
            name: "computer_click".to_string(),
            description: "computer click test tool".to_string(),
            schema: json!({ "type": "object" }),
            owner: Owner::Computer,
        });

        let first_list = post_json(
            &endpoint,
            &lease.token,
            None,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        assert_eq!(first_list.status(), reqwest::StatusCode::OK);
        let first_body: Value = first_list.json().await.expect("first tools/list JSON");
        let first_names = first_body["result"]["tools"]
            .as_array()
            .expect("first tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(first_names.contains(&"computer_click"));

        gateway
            .refresh_catalog()
            .await
            .expect("catalog should refresh");
        let second_list = post_json(
            &endpoint,
            &lease.token,
            None,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
        assert_eq!(second_list.status(), reqwest::StatusCode::OK);
        let second_body: Value = second_list.json().await.expect("second tools/list JSON");
        let second_names = second_body["result"]["tools"]
            .as_array()
            .expect("second tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!second_names.contains(&"computer_click"));
        assert!(second_names.contains(&"get_auracoder_thread_message_count"));
        assert_eq!(gateway.local_addr().await, Some(address));
        assert!(gateway.clients.read().await.contains_key(&lease.token));
        let still_usable = post_json(
            &endpoint,
            &lease.token,
            None,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
        )
        .await;
        assert_eq!(still_usable.status(), reqwest::StatusCode::OK);
        gateway.shutdown().await;
    }

    /// 验证同一引擎实例重新注册时会停止旧租约的活动调用并清理旧会话。
    #[tokio::test]
    async fn replacing_client_stops_old_active_calls() {
        let gateway = test_gateway();
        gateway.start().await.expect("gateway should start");
        let old_lease = gateway
            .register_local_client("codex", "same-instance")
            .await
            .expect("old lease should be issued");
        gateway
            .sessions
            .write()
            .await
            .insert("old-session".to_string(), old_lease.token.clone());
        let cancellation = tokio_util::sync::CancellationToken::new();
        gateway.active_calls.write().await.insert(
            "old-call".to_string(),
            (old_lease.token.clone(), cancellation.clone()),
        );

        let new_lease = gateway
            .register_local_client("codex", "same-instance")
            .await
            .expect("new lease should be issued");
        assert_ne!(old_lease.token, new_lease.token);
        let clients = gateway.clients.read().await;
        assert!(!clients.contains_key(&old_lease.token));
        assert!(clients.contains_key(&new_lease.token));
        drop(clients);
        assert!(!gateway.sessions.read().await.contains_key("old-session"));
        assert!(cancellation.is_cancelled());
        gateway.shutdown().await;
    }

    #[test]
    fn remote_visibility_filters_computer_tools_before_http_dispatch() {
        let local = super::ClientLease {
            token: "local".to_string(),
            engine: "codex".to_string(),
            instance_id: "local-instance".to_string(),
            policy: ClientAccessPolicy::Local,
            ssh_connection_id: None,
            generation: 1,
        };
        let remote = super::ClientLease {
            token: "remote".to_string(),
            engine: "codex".to_string(),
            instance_id: "remote-instance".to_string(),
            policy: ClientAccessPolicy::SshRemoteThreadOnly,
            ssh_connection_id: Some("ssh-1".to_string()),
            generation: 1,
        };
        let catalog = vec![
            CatalogTool {
                name: "computer_click".to_string(),
                description: "computer".to_string(),
                schema: json!({ "type": "object" }),
                owner: Owner::Computer,
            },
            CatalogTool {
                name: "get_auracoder_thread_message_count".to_string(),
                description: "thread".to_string(),
                schema: json!({ "type": "object" }),
                owner: Owner::Thread,
            },
        ];
        assert_eq!(visible_tools(&local, &catalog).len(), 2);
        let remote_tools = visible_tools(&remote, &catalog);
        assert_eq!(remote_tools.len(), 1);
        assert_eq!(
            remote_tools[0]["name"],
            "get_auracoder_thread_message_count"
        );
    }
}
