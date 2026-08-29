use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    convert::Infallible,
    hash::{Hash, Hasher},
    net::SocketAddr,
    sync::Arc,
    time::Instant,
};

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
    cli_tools::{CliTool, McpInvocationContext},
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

/// 注册成功后返回给调用方的独立 Bearer 租约。
#[derive(Debug, Clone)]
pub(crate) struct ClientLease {
    /// 只在内存中保存的强随机访问令牌。
    pub token: String,
    /// 租约所属引擎。
    pub engine: String,
    /// 租约所属引擎实例。
    pub instance_id: String,
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
#[derive(Clone)]
struct ClientRecord {
    lease: ClientLease,
    /// 当前 token 绑定的 CLI MCP 实现，Gateway 只通过该对象调用 MCP 接口。
    cli: Arc<dyn CliTool>,
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
    /// 内存租约表。
    clients: Arc<RwLock<HashMap<String, ClientRecord>>>,
    /// MCP 会话与租约 token 的绑定。
    sessions: Arc<RwLock<HashMap<String, String>>>,
    /// 活动工具调用与其租约取消令牌。
    active_calls: Arc<RwLock<HashMap<String, (String, CancellationToken)>>>,
}

impl AuraCoderMcpGateway {
    /// 创建只负责 MCP 协议、token 和请求生命周期的 Gateway。
    pub(crate) fn new() -> Self {
        Self {
            operation: Mutex::new(()),
            lifecycle: Arc::new(Mutex::new(GatewayLifecycle::default())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            active_calls: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册当前 CLI 实现的 token 租约，并替换同一实例的旧租约。
    pub(crate) async fn register_client(
        &self,
        engine: &str,
        instance_id: &str,
        cli: Arc<dyn CliTool>,
    ) -> Result<ClientLease, String> {
        self.register(engine, instance_id, cli).await
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
        let token_summary = safe_token_summary(token);
        let (engine, instance_id) = {
            let clients = self.clients.read().await;
            clients
                .get(token.trim())
                .map(|client| {
                    (
                        client.lease.engine.clone(),
                        client.lease.instance_id.clone(),
                    )
                })
                .unwrap_or_else(|| ("<unknown>".to_string(), "<unknown>".to_string()))
        };
        log::info!(
            "MCP Gateway register trusted context entered: token_summary={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}",
            token_summary,
            engine,
            instance_id,
            thread_id,
            turn_id,
        );
        if thread_id.trim().is_empty() || turn_id.trim().is_empty() {
            let error = "trusted context 缺少 thread_id 或 turn_id".to_string();
            log::info!(
                "MCP Gateway register trusted context failed: token_summary={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, error={}",
                token_summary,
                engine,
                instance_id,
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
                "MCP Gateway register trusted context failed: token_summary={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, error={}",
                token_summary,
                engine,
                instance_id,
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
            "MCP Gateway register trusted context succeeded: token_summary={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, context_registered=true",
            token_summary,
            client.lease.engine,
            client.lease.instance_id,
            thread_id,
            turn_id,
        );
        Ok(())
    }

    /// 清除可信上下文。
    pub(crate) async fn clear_trusted_context(&self, token: &str) -> bool {
        let token_summary = safe_token_summary(token);
        let mut clients = self.clients.write().await;
        let Some(client) = clients.get_mut(token.trim()) else {
            log::info!(
                "MCP Gateway clear trusted context result: token_summary={}, engine=<unknown>, instance_id=<unknown>, engine_thread_id=<unknown>, turn_id=<unknown>, cleared=false",
                token_summary,
            );
            return false;
        };
        let previous_context = client.context.clone();
        log::info!(
            "MCP Gateway clear trusted context entered: token_summary={}, engine={}, instance_id={}, engine_thread_id={:?}, turn_id={:?}",
            token_summary,
            client.lease.engine,
            client.lease.instance_id,
            previous_context.as_ref().map(|context| context.thread_id.as_str()),
            previous_context.as_ref().map(|context| context.turn_id.as_str()),
        );
        client.context = None;
        log::info!(
            "MCP Gateway clear trusted context result: token_summary={}, engine={}, instance_id={}, engine_thread_id={:?}, turn_id={:?}, cleared=true",
            token_summary,
            client.lease.engine,
            client.lease.instance_id,
            previous_context.as_ref().map(|context| context.thread_id.as_str()),
            previous_context.as_ref().map(|context| context.turn_id.as_str()),
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

    /// 先完整停止当前代次，再启动新代次。
    pub(crate) async fn restart(&self) -> Result<(), String> {
        self.shutdown().await;
        self.start().await
    }

    /// 保留旧设置命令的调用兼容；工具目录已改由当前 CliTool 动态提供，无需 Gateway 刷新。
    pub(crate) async fn refresh_catalog(&self) -> Result<(), String> {
        Ok(())
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
        cli: Arc<dyn CliTool>,
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
                cli,
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

    /// 组装 HTTP 任务所需的共享状态。
    fn task_state(&self) -> TaskState {
        TaskState {
            clients: self.clients.clone(),
            sessions: self.sessions.clone(),
            active_calls: self.active_calls.clone(),
            lifecycle: self.lifecycle.clone(),
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
    clients: Arc<RwLock<HashMap<String, ClientRecord>>>,
    sessions: Arc<RwLock<HashMap<String, String>>>,
    active_calls: Arc<RwLock<HashMap<String, (String, CancellationToken)>>>,
    lifecycle: Arc<Mutex<GatewayLifecycle>>,
}

/*
旧 Gateway 运行时代码：工具 Owner、CatalogTool 及工具目录校验曾由 Gateway
负责。三层 MCP 改造后，这段运行代码仅保留迁移留痕，工具目录改由当前 CliTool
直接提供。
*/

const MCP_PATH: &str = "/mcp";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";
const MAX_MCP_BODY_BYTES: usize = 1024 * 1024;

/// 生成仅用于日志关联的安全 token 摘要，避免日志写入 Bearer 原文。
fn safe_token_summary(token: &str) -> String {
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    format!("len={},hash={:016x}", token.len(), hasher.finish())
}

/// 将 MCP request id 转换成不包含请求参数的日志字段。
fn request_id_for_log(request_id: Option<&Value>) -> String {
    match request_id {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => "<none>".to_string(),
    }
}

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
    let authorization_present = request.headers().contains_key(header::AUTHORIZATION);
    log::info!(
        "MCP Gateway request entered: method={}, uri={}, authorization_present={}",
        request.method(),
        request.uri(),
        authorization_present,
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
            "MCP Gateway authentication failed after token extraction: token_summary={}, method={}, uri={}",
            safe_token_summary(&token),
            request.method(),
            request.uri(),
        );
        return Ok(authentication_error());
    };
    log::info!(
        "MCP Gateway client lease resolved: token_summary={}, engine={}, instance_id={}, generation={}, context_registered={}",
        safe_token_summary(&token),
        client.lease.engine,
        client.lease.instance_id,
        client.lease.generation,
        client.context.is_some(),
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
        "MCP Gateway request body collected: token_summary={}, engine={}, instance_id={}, body_bytes={}",
        safe_token_summary(&token),
        client.lease.engine,
        client.lease.instance_id,
        body.len(),
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
        "MCP Gateway RPC request parsed: token_summary={}, engine={}, instance_id={}, request_id={}, method={}",
        safe_token_summary(&token),
        client.lease.engine,
        client.lease.instance_id,
        request_id_for_log(rpc.id.as_ref()),
        rpc.method,
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
            "MCP Gateway notification response: status={}, token_summary={}, engine={}, instance_id={}, request_id={}, method={}",
            StatusCode::ACCEPTED,
            safe_token_summary(&token),
            client.lease.engine,
            client.lease.instance_id,
            request_id_for_log(rpc.id.as_ref()),
            rpc.method,
        );
        return Ok(response);
    }
    // 其他 MCP notifications 没有响应体；返回 202 可避免客户端把通知误判成普通结果。
    if rpc.id.is_none() && rpc.method.starts_with("notifications/") {
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = StatusCode::ACCEPTED;
        log::info!(
            "MCP Gateway notification response: status={}, token_summary={}, engine={}, instance_id={}, request_id={}, method={}",
            StatusCode::ACCEPTED,
            safe_token_summary(&token),
            client.lease.engine,
            client.lease.instance_id,
            request_id_for_log(rpc.id.as_ref()),
            rpc.method,
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
                        "serverInfo": { "name": "AuraCoder MCP Gateway", "version": "1.0.3" }
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
            match client.cli.list_mcp_tools() {
                Ok(tools) => (
                    json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": { "tools": tools }
                    }),
                    None,
                ),
                Err(error) => {
                    log::error!(
                        "MCP Gateway tools/list 调用当前 CLI 失败，原始错误：{error}"
                    );
                    (
                        rpc_error(request_id, -32603, "internal_error"),
                        None,
                    )
                }
            }
        }
        "tools/call" => {
            let tool_name = rpc
                .params
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("<missing>");
            let argument_keys = rpc
                .params
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|params| params.get("arguments"))
                .and_then(Value::as_object)
                .map(|arguments| arguments.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            log::info!(
                "MCP Gateway tools/call arrived: request_id={}, tool={}, engine={}, instance_id={}, trusted_context_present={}, argument_keys={:?}",
                request_id_for_log(rpc.id.as_ref()),
                tool_name,
                client.lease.engine,
                client.lease.instance_id,
                client.context.is_some(),
                argument_keys,
            );
            let call_result = call_cli_tool(&client, &rpc, state.clone()).await;
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
        "MCP Gateway RPC result constructed: token_summary={}, engine={}, instance_id={}, request_id={}, method={}, result_is_error={:?}, session_id={:?}",
        safe_token_summary(&token),
        client.lease.engine,
        client.lease.instance_id,
        request_id_for_log(rpc.id.as_ref()),
        rpc.method,
        result.get("result").and_then(|value| value.get("isError")),
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
        "MCP Gateway JSON response: status={}, payload_bytes={}, result_is_error={:?}, session_id={:?}",
        status,
        body.len(),
        value.get("result").and_then(|result| result.get("isError")),
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

/*
旧 Gateway 工具过滤入口已停用：
fn visible_tools(lease: &ClientLease, catalog: &[CatalogTool]) -> Vec<Value> {
    catalog
        .iter()
        .filter(|tool| lease.policy == ClientAccessPolicy::Local || tool.owner == Owner::Thread)
        .map(CatalogTool::json)
        .collect()
}
*/

/// 读取可信上下文并把 MCP 工具调用直接交给当前 token 绑定的 CLI 实现。
async fn call_cli_tool(
    client: &ClientRecord,
    rpc: &RpcRequest,
    state: TaskState,
) -> Value {
    let started_at = Instant::now();
    let request_id = request_id_for_log(rpc.id.as_ref());
    let Some(params) = rpc.params.as_ref().and_then(Value::as_object) else {
        log::warn!(
            "MCP Gateway tools/call parameter validation failed: request_id={}, call_id=<not_registered>, tool=<missing>, engine={}, instance_id={}, engine_thread_id=<none>, turn_id=<none>, is_error=true, duration_ms={}, result_code=invalid_request",
            request_id,
            client.lease.engine,
            client.lease.instance_id,
            started_at.elapsed().as_millis(),
        );
        return json!({
            "content": [{ "type": "text", "text": "invalid_request: tools/call params 必须是对象" }],
            "isError": true
        });
    };
    let Some(name) = params.get("name").and_then(Value::as_str).map(str::trim) else {
        log::warn!(
            "MCP Gateway tools/call parameter validation failed: request_id={}, call_id=<not_registered>, tool=<missing>, engine={}, instance_id={}, engine_thread_id=<none>, turn_id=<none>, is_error=true, duration_ms={}, result_code=invalid_request",
            request_id,
            client.lease.engine,
            client.lease.instance_id,
            started_at.elapsed().as_millis(),
        );
        return json!({
            "content": [{ "type": "text", "text": "invalid_request: tools/call 缺少 name" }],
            "isError": true
        });
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let context = client.context.clone();
    let Some(context) = context else {
        log::info!(
            "MCP Gateway tools/call trusted context missing: request_id={}, call_id=<not_registered>, tool={}, engine={}, instance_id={}, engine_thread_id=<none>, turn_id=<none>, is_error=true, duration_ms={}, result_code=invocation_context_missing",
            request_id,
            name,
            client.lease.engine,
            client.lease.instance_id,
            started_at.elapsed().as_millis(),
        );
        return json!({
            "content": [{ "type": "text", "text": "invocation_context_missing: trusted context 缺少 thread_id 或 turn_id" }],
            "isError": true
        });
    };
    let engine_thread_id = context.thread_id.clone();
    let turn_id = context.turn_id.clone();
    let active_call_id = active_call_key(
        &client.lease.token,
        rpc.id.as_ref().unwrap_or(&Value::String(
            Uuid::new_v4().simple().to_string(),
        )),
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
    log::info!(
        "MCP Gateway active call registered: request_id={}, call_id={}, tool={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, is_error=false, duration_ms={}, active_call_registered=true",
        request_id,
        call_id,
        name,
        client.lease.engine,
        client.lease.instance_id,
        engine_thread_id,
        turn_id,
        started_at.elapsed().as_millis(),
    );
    log::info!(
        "MCP Gateway entering CliTool: request_id={}, call_id={}, tool={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, is_error=false, duration_ms={}",
        request_id,
        call_id,
        name,
        client.lease.engine,
        client.lease.instance_id,
        engine_thread_id,
        turn_id,
        started_at.elapsed().as_millis(),
    );
    let result = client
        .cli
        .call_mcp_tool(
            name,
            arguments,
            McpInvocationContext {
                engine_thread_id: engine_thread_id.clone(),
                turn_id: turn_id.clone(),
            },
            call_id.clone(),
            cancellation,
        )
        .await;
    let result_is_error = result.is_error;
    log::info!(
        "MCP Gateway CliTool returned: request_id={}, call_id={}, tool={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, is_error={}, duration_ms={}",
        request_id,
        call_id,
        client.lease.engine,
        client.lease.instance_id,
        name,
        engine_thread_id,
        turn_id,
        result_is_error,
        started_at.elapsed().as_millis(),
    );
    let active_call_removed = state.active_calls.write().await.remove(&active_call_id).is_some();
    log::info!(
        "MCP Gateway active call cleaned: request_id={}, call_id={}, tool={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, is_error={}, duration_ms={}, active_call_removed={}",
        request_id,
        call_id,
        name,
        client.lease.engine,
        client.lease.instance_id,
        engine_thread_id,
        turn_id,
        result_is_error,
        started_at.elapsed().as_millis(),
        active_call_removed,
    );
    match serde_json::to_value(result) {
        Ok(value) => value,
        Err(error) => {
            log::error!(
                "MCP Gateway 序列化 CLI MCP 结果失败: request_id={}, call_id={}, tool={}, engine={}, instance_id={}, engine_thread_id={}, turn_id={}, is_error=true, duration_ms={}, 原始错误：{error}",
                request_id,
                call_id,
                name,
                client.lease.engine,
                client.lease.instance_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            json!({
                "content": [{ "type": "text", "text": "internal_error: MCP 工具结果不可序列化" }],
                "isError": true
            })
        }
    }
}

/*
旧 Gateway 允许从模型 `_meta` 补充上下文，现仅保留代码留痕，不再执行：
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
*/

fn active_call_key(token: &str, request_id: &Value) -> String {
    format!(
        "{token}:{}",
        serde_json::to_string(request_id).unwrap_or_default()
    )
}

/*
旧 Gateway 业务结果映射已停用，改由 BaseCliMcp 统一产生 McpToolResult：
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
*/

/*
旧 Gateway 目录和业务分发测试已停用，保留完整测试代码作为迁移留痕；
新的 MCP 验证测试只依赖真实 CliTool 和统一 register_client。
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
*/

#[cfg(test)]
mod tests {
    use super::{AuraCoderMcpGateway, GatewayState};
    use crate::{
        auracoder_thread_mcp_service::AuraCoderThreadMcpService,
        cli_tools::{codex::CodexCli, CliTool},
        computer_control_service::ComputerControlService,
        config::app_config::AppConfig,
        db::Database,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        power::KeepAwakeManager,
        scheduled_tasks::ScheduledTaskManager,
        state::{AppState, TurnManager},
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    use serde_json::{json, Value};
    use std::sync::Arc;

    /// 构造 Gateway 直调 CliTool 测试使用的完整应用状态。
    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-mcp-gateway-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("failed to create gateway test root");
        let db = Database::open(root.join("workspaces.db")).expect("failed to create gateway test db");
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
            extension_catalog_refreshes: Arc::new(
                crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
            ),
            scheduled_tasks: Arc::new(ScheduledTaskManager::new()),
            computer_control_service: Arc::new(ComputerControlService::default()),
            auracoder_thread_mcp_service: Arc::new(AuraCoderThreadMcpService::new(db)),
            mcp_gateway: Arc::new(AuraCoderMcpGateway::new()),
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    /// 验证 Gateway 只通过已登记 CliTool 提供目录，并拒绝模型 `_meta` 补充上下文。
    #[tokio::test]
    async fn gateway_registers_and_calls_current_cli_directly() {
        let state = test_app_state();
        let cli: Arc<dyn CliTool> = Arc::new(CodexCli::new(state.clone()));
        let gateway = AuraCoderMcpGateway::new();
        assert_eq!(gateway.status().await, GatewayState::Stopped);
        gateway.start().await.expect("gateway should start");
        let lease = gateway
            .register_client("codex", "test-instance", cli)
            .await
            .expect("client lease should be issued");
        let address = gateway.local_addr().await.expect("listener address");
        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list"
            }))
            .send()
            .await
            .expect("tools/list request should complete");
        let body: Value = response.json().await.expect("tools/list response JSON");
        assert_eq!(body["result"]["tools"].as_array().map(Vec::len), Some(2));

        let invalid_token = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth("invalid-token")
            .json(&json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}))
            .send()
            .await
            .expect("invalid token request should complete");
        assert_eq!(invalid_token.status(), reqwest::StatusCode::UNAUTHORIZED);
        let invalid_session = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .header("Mcp-Session-Id", "invalid-session")
            .json(&json!({"jsonrpc": "2.0", "id": 10, "method": "ping"}))
            .send()
            .await
            .expect("invalid session request should complete");
        assert_eq!(invalid_session.status(), reqwest::StatusCode::BAD_REQUEST);
        let pending_cancellation = tokio_util::sync::CancellationToken::new();
        gateway.active_calls.write().await.insert(
            "test-active-call".to_string(),
            (lease.token.clone(), pending_cancellation.clone()),
        );
        assert!(gateway.revoke_client(&lease.token).await);
        assert!(pending_cancellation.is_cancelled());

        let cli: Arc<dyn CliTool> = Arc::new(CodexCli::new(state.clone()));
        let lease = gateway
            .register_client("codex", "test-instance", cli)
            .await
            .expect("replacement client lease should be issued");

        let no_context = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "get_auracoder_thread_message_count",
                    "arguments": {"thread_id": "thread"},
                    "_meta": {"thread_id": "forged", "turn_id": "forged"}
                }
            }))
            .send()
            .await
            .expect("tools/call request should complete");
        let no_context_body: Value = no_context.json().await.expect("tools/call response JSON");
        assert_eq!(no_context_body["result"]["isError"], true);
        assert!(no_context_body["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("invocation_context_missing"));

        let workspace = crate::db::workspaces::upsert_workspace(
            &state.db,
            &std::env::temp_dir()
                .join(format!("auracoder-gateway-workspace-{}", uuid::Uuid::new_v4()))
                .to_string_lossy(),
        )
        .expect("workspace should be created");
        let target_thread = crate::db::threads::create_thread(
            &state.db,
            &workspace.id,
            "codex",
            "model",
            "Gateway direct call",
        )
        .expect("thread should be created");
        crate::db::messages::insert_user_message(
            &state.db,
            &target_thread.id,
            "one message",
            None,
            Some("codex"),
            Some("model"),
            None,
        )
        .expect("message should be inserted");
        state.auracoder_thread_mcp_service.bind_engine_thread(
            "codex",
            "engine-thread",
            &workspace.id,
        );
        gateway
            .register_trusted_context(&lease.token, "engine-thread", "turn-1")
            .await
            .expect("trusted context should be registered");
        let with_context = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth(&lease.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "get_auracoder_thread_message_count",
                    "arguments": {"thread_id": target_thread.id}
                }
            }))
            .send()
            .await
            .expect("context tools/call request should complete");
        let with_context_body: Value = with_context
            .json()
            .await
            .expect("context tools/call response JSON");
        assert_eq!(with_context_body["result"]["isError"], false);
        let text = with_context_body["result"]["content"][0]["text"]
            .as_str()
            .expect("context tools/call text content");
        let result: Value = serde_json::from_str(text).expect("context tool result JSON");
        assert_eq!(result["message_count"], 1);
        gateway.restart().await.expect("gateway should restart");
        let old_generation = client
            .post(format!(
                "http://{}/mcp",
                gateway.local_addr().await.expect("restarted address")
            ))
            .bearer_auth(&lease.token)
            .json(&json!({"jsonrpc": "2.0", "id": 11, "method": "ping"}))
            .send()
            .await
            .expect("old generation request should complete");
        assert_eq!(old_generation.status(), reqwest::StatusCode::UNAUTHORIZED);
        gateway.shutdown().await;
    }
}
