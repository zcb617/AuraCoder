use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{
    collections::HashMap,
    // 旧 codex_augmented_path 实现保留在注释中：
    // ffi::OsString,
    path::Path,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, ChildStdin, Command},
    sync::{broadcast, oneshot, Mutex},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::{process_utils, runtime_env};

use super::codex_protocol::{
    notification_payload, parse_incoming, request_payload, response_error_payload,
    response_success_payload, IncomingMessage, RpcError, RpcResponse,
};
use super::trim_action_output_delta_content;

// This channel is only a live fan-out for active Codex subscribers. Tokio's
// broadcast ring retains every slot until it is overwritten, so keeping a large
// history here can pin already-delivered protocol payloads while AuraCoder is idle.

const INCOMING_EVENT_BUFFER_CAPACITY: usize = 6400;
const TRANSPORT_ERROR_LINE_MAX_CHARS: usize = 16 * 1024;
const TRANSPORT_ERROR_LINE_TRUNCATED_PREFIX: &str = "... [protocol line truncated; showing tail]\n";
/// Codex app-server 读取 AuraCoder MCP Gateway Bearer Token 的固定环境变量名。
const CODEX_MCP_GATEWAY_TOKEN_ENV: &str = "AURACODER_MCP_TOKEN";

#[derive(Debug, Clone, Serialize)]
pub struct CodexTransportEventDiagnostics {
    pub sequence: u64,
    pub at: String,
    pub kind: String,
    pub method: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexTransportMessage {
    pub sequence: u64,
    pub published_at: std::time::Instant,
    pub message: IncomingMessage,
}

impl CodexTransportMessage {
    pub fn diagnostics(&self) -> CodexTransportEventDiagnostics {
        let mut diagnostics = diagnostics_for_message(&self.message);
        diagnostics.sequence = self.sequence;
        diagnostics
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexTransportDiagnostics {
    pub pid: Option<u32>,
    pub process_status: Option<String>,
    pub pending_count: usize,
    pub broadcast_receiver_count: usize,
    pub broadcast_capacity: usize,
    pub next_incoming_sequence: u64,
    pub last_event: Option<CodexTransportEventDiagnostics>,
    pub last_stderr: Option<String>,
}

type CodexWebSocketWriter =
    futures::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

pub struct CodexTransport {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    websocket_writer: Mutex<Option<CodexWebSocketWriter>>,
    websocket_alive: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<RpcResponse>>>>,
    incoming_tx: broadcast::Sender<CodexTransportMessage>,
    last_event: Arc<Mutex<Option<CodexTransportEventDiagnostics>>>,
    last_stderr: Arc<Mutex<Option<String>>>,
    next_request_id: std::sync::atomic::AtomicU64,
    next_incoming_sequence: Arc<AtomicU64>,
}

impl Drop for CodexTransport {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.try_lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.start_kill();
            }
        }
        self.websocket_alive.store(false, Ordering::Relaxed);
    }
}

impl CodexTransport {
    /// 启动本地 Codex app-server，并按需追加 MCP Gateway 配置覆盖参数。
    pub async fn spawn(
        codex_executable: &str,
        mcp_gateway_endpoint: Option<&str>,
        mcp_gateway_token: Option<&str>,
    ) -> anyhow::Result<Self> {
        if mcp_gateway_endpoint.is_some() != mcp_gateway_token.is_some() {
            return Err(anyhow::anyhow!("Codex 的 AuraCoder MCP 配置不完整"));
        }

        let mut command = Command::new(codex_executable);
        process_utils::configure_tokio_command(&mut command);
        // 旧登录 Shell 环境导入和手工 PATH 处理由 runtime_env::get 接替：
        // runtime_env::apply_missing_login_shell_env(&mut command).await;
        // if let Some(augmented_path) = codex_augmented_path(codex_executable) {
        //     command.env("PATH", augmented_path);
        // }
        let mut codex_environment = runtime_env::get(Path::new(codex_executable)).await;
        // 不让父进程可能已有的同名令牌进入环境快照日志；有效令牌只通过 command.env 注入。
        codex_environment.remove(std::ffi::OsStr::new(CODEX_MCP_GATEWAY_TOKEN_ENV));
        // 旧判断保留：trace 曾被解读为局域网连接失败与这些启动变量无关。
        // 0.72.5 对照确认：本地网络用途声明与子进程应用身份过滤必须同时生效。
        #[cfg(target_os = "macos")]
        for inherited_process_key in ["XPC_SERVICE_NAME", "XPC_FLAGS", "__CFBundleIdentifier"] {
            codex_environment.remove(std::ffi::OsStr::new(inherited_process_key));
            command.env_remove(inherited_process_key);
        }
        command.envs(codex_environment.clone());

        let mut codex_arguments = vec![
            "app-server".to_string(),
            "--listen".to_string(),
            "stdio://".to_string(),
        ];
        if let (Some(endpoint), Some(token)) = (mcp_gateway_endpoint, mcp_gateway_token) {
            command.env(CODEX_MCP_GATEWAY_TOKEN_ENV, token);
            for config_override in runtime_env::codex_mcp_gateway_config_overrides(
                endpoint,
                CODEX_MCP_GATEWAY_TOKEN_ENV,
            ) {
                codex_arguments.push("-c".to_string());
                codex_arguments.push(config_override);
            }
        }
        let mut command = command
            .args(&codex_arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(target_os = "macos")]
        {
            // 记录 macOS 下 AuraCoder 与 Codex 子进程的完整启动身份、参数和环境快照。
            let auracoder_executable = match std::env::current_exe() {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(error) => error.to_string(),
            };
            let auracoder_arguments = std::env::args_os()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let auracoder_current_dir = match std::env::current_dir() {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(error) => error.to_string(),
            };
            let codex_canonical_executable = match Path::new(codex_executable).canonicalize() {
                Ok(path) => path.to_string_lossy().into_owned(),
                Err(error) => error.to_string(),
            };
            let auracoder_environment = std::env::vars_os()
                .filter(|(key, _)| key != std::ffi::OsStr::new(CODEX_MCP_GATEWAY_TOKEN_ENV))
                .map(|(key, value)| {
                    (
                        key.to_string_lossy().into_owned(),
                        serde_json::Value::String(value.to_string_lossy().into_owned()),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            let codex_environment_log = codex_environment
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_string_lossy().into_owned(),
                        serde_json::Value::String(value.to_string_lossy().into_owned()),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_process_launch",
                "auracoder_pid": std::process::id(),
                "auracoder_executable": auracoder_executable,
                "auracoder_arguments": auracoder_arguments,
                "auracoder_current_dir": auracoder_current_dir,
                "codex_executable": codex_executable,
                "codex_canonical_executable": codex_canonical_executable,
                "codex_arguments": codex_arguments.clone(),
                "auracoder_environment": auracoder_environment,
                "codex_environment": codex_environment_log,
            }))
            .await;
        }

        let mut child = match command.spawn() {
            Ok(child) => {
                #[cfg(target_os = "macos")]
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_process_spawned",
                    "auracoder_pid": std::process::id(),
                    "codex_pid": child.id(),
                    "codex_executable": codex_executable,
                    "codex_arguments": codex_arguments,
                }))
                .await;
                child
            }
            Err(error) => {
                #[cfg(target_os = "macos")]
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_process_spawn_failed",
                    "auracoder_pid": std::process::id(),
                    "codex_executable": codex_executable,
                    "codex_arguments": codex_arguments,
                    "error": error.to_string(),
                }))
                .await;
                return Err(error).with_context(|| {
                    format!("failed to spawn `codex app-server` using `{codex_executable}`")
                });
            }
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stdin not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stdout not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("codex app-server stderr not available"))?;

        let (incoming_tx, _) = broadcast::channel(INCOMING_EVENT_BUFFER_CAPACITY);
        let next_incoming_sequence = Arc::new(AtomicU64::new(1));
        let pending = Arc::new(Mutex::new(
            HashMap::<String, oneshot::Sender<RpcResponse>>::new(),
        ));
        let last_event = Arc::new(Mutex::new(None));
        let last_stderr = Arc::new(Mutex::new(None));

        {
            let pending = pending.clone();
            let incoming_tx = incoming_tx.clone();
            let last_event = last_event.clone();
            let next_incoming_sequence = next_incoming_sequence.clone();
            tokio::spawn(async move {
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_stdout_reader_started",
                }))
                .await;
                let mut lines = BufReader::new(stdout).lines();

                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let sequence = next_incoming_sequence.fetch_add(1, Ordering::Relaxed);
                            #[cfg(target_os = "macos")]
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_transport_inbound_payload",
                                "source": "stdio",
                                "sequence": sequence,
                                "payload": line.clone(),
                            }))
                            .await;
                            let line_bytes = line.len();
                            let parsed_at = Utc::now().to_rfc3339();
                            match parse_incoming(&line) {
                                Ok(IncomingMessage::Response(response)) => {
                                    let diagnostics = CodexTransportEventDiagnostics {
                                        sequence,
                                        at: parsed_at.clone(),
                                        kind: "response".to_string(),
                                        method: None,
                                        id: Some(response.id.clone()),
                                    };
                                    record_last_event(&last_event, diagnostics.clone()).await;
                                    crate::engines::codex::append_codex_transport_log(
                                        &serde_json::json!({
                                            "at": parsed_at,
                                            "event": "codex_stdout_message",
                                            "sequence": sequence,
                                            "line_bytes": line_bytes,
                                            "kind": diagnostics.kind,
                                            "method": diagnostics.method,
                                            "id": diagnostics.id,
                                            "route": "pending_response",
                                        }),
                                    )
                                    .await;
                                    let sender = pending.lock().await.remove(&response.id);
                                    let pending_found = sender.is_some();
                                    if let Some(sender) = sender {
                                        let _ = sender.send(response);
                                    }
                                    crate::engines::codex::append_codex_transport_log(
                                        &serde_json::json!({
                                            "at": Utc::now().to_rfc3339(),
                                            "event": "codex_stdout_response_routed",
                                            "sequence": sequence,
                                            "pending_found": pending_found,
                                        }),
                                    )
                                    .await;
                                }
                                Ok(other) => {
                                    // 在缓冲裁剪前保留子代理协议事件的完整参数，供后续确认真实任务标题字段。
                                    if let Some(record) =
                                        subagent_protocol_log_record(&other, sequence, "stdio")
                                    {
                                        crate::engines::codex::append_codex_transport_log(&record)
                                            .await;
                                    }
                                    #[cfg(target_os = "macos")]
                                    if let Some(record) = codex_error_notification_log_record(
                                        &other, sequence, "stdio",
                                    ) {
                                        crate::engines::codex::append_codex_transport_log(&record)
                                            .await;
                                    }
                                    let mut diagnostics = diagnostics_for_message(&other);
                                    diagnostics.sequence = sequence;
                                    record_last_event(&last_event, diagnostics.clone()).await;
                                    crate::engines::codex::append_codex_transport_log(
                                        &serde_json::json!({
                                            "at": parsed_at,
                                            "event": "codex_stdout_message",
                                            "sequence": sequence,
                                            "line_bytes": line_bytes,
                                            "kind": diagnostics.kind,
                                            "method": diagnostics.method,
                                            "id": diagnostics.id,
                                            "route": "broadcast",
                                        }),
                                    )
                                    .await;
                                    let published_at = std::time::Instant::now();
                                    let envelope = CodexTransportMessage {
                                        sequence,
                                        published_at,
                                        message: trim_buffered_incoming_message(other),
                                    };
                                    let receiver_count = incoming_tx.receiver_count();
                                    let send_result = incoming_tx.send(envelope);
                                    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                    "at": Utc::now().to_rfc3339(),
                                    "event": "codex_broadcast_publish",
                                    "sequence": sequence,
                                    "capacity": INCOMING_EVENT_BUFFER_CAPACITY,
                                    "receiver_count_before": receiver_count,
                                    "receivers_notified": send_result.as_ref().ok().copied().unwrap_or(0),
                                    "send_result": if send_result.is_ok() { "published" } else { "no_receivers" },
                                })).await;
                                }
                                Err(error) => {
                                    log::warn!("codex stdout parse error: {error}");
                                    record_last_event(
                                        &last_event,
                                        CodexTransportEventDiagnostics {
                                            sequence,
                                            at: parsed_at.clone(),
                                            kind: "parse_error".to_string(),
                                            method: Some("transport/parse_error".to_string()),
                                            id: None,
                                        },
                                    )
                                    .await;
                                    publish_transport_message(
                                        &incoming_tx,
                                        sequence,
                                        IncomingMessage::Notification {
                                            method: "transport/parse_error".to_string(),
                                            params: transport_parse_error_payload(
                                                &error.to_string(),
                                                &line,
                                            ),
                                        },
                                        "parse_error",
                                    )
                                    .await;
                                }
                            }
                        }
                        Ok(None) => {
                            let sequence = next_incoming_sequence.fetch_add(1, Ordering::Relaxed);
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_stdout_reader_eof",
                                "sequence": sequence,
                            }))
                            .await;
                            record_last_event(
                                &last_event,
                                CodexTransportEventDiagnostics {
                                    sequence,
                                    at: Utc::now().to_rfc3339(),
                                    kind: "transport".to_string(),
                                    method: Some("transport/eof".to_string()),
                                    id: None,
                                },
                            )
                            .await;
                            publish_transport_message(
                                &incoming_tx,
                                sequence,
                                IncomingMessage::Notification {
                                    method: "transport/eof".to_string(),
                                    params: serde_json::value::RawValue::from_string(
                                        "{}".to_string(),
                                    )
                                    .expect("\"{}\" is valid json"),
                                },
                                "eof",
                            )
                            .await;
                            break;
                        }
                        Err(error) => {
                            let sequence = next_incoming_sequence.fetch_add(1, Ordering::Relaxed);
                            log::warn!("codex stdout read error: {error}");
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_stdout_reader_error",
                                "sequence": sequence,
                                "error": error.to_string(),
                            }))
                            .await;
                            record_last_event(
                                &last_event,
                                CodexTransportEventDiagnostics {
                                    sequence,
                                    at: Utc::now().to_rfc3339(),
                                    kind: "transport".to_string(),
                                    method: Some("transport/read_error".to_string()),
                                    id: None,
                                },
                            )
                            .await;
                            publish_transport_message(
                                &incoming_tx,
                                sequence,
                                IncomingMessage::Notification {
                                    method: "transport/read_error".to_string(),
                                    params: serde_json::value::to_raw_value(&serde_json::json!({
                                      "error": error.to_string(),
                                    }))
                                    .expect("internal error payload is valid json"),
                                },
                                "read_error",
                            )
                            .await;
                            break;
                        }
                    }
                }
            });
        }

        {
            let last_stderr = last_stderr.clone();
            tokio::spawn(async move {
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_stderr_reader_started",
                }))
                .await;
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if !line.trim().is_empty() {
                                let trimmed = trim_transport_error_line(line.trim());
                                *last_stderr.lock().await = Some(trimmed.clone());
                                log::debug!("codex stderr: {line}");
                                #[cfg(target_os = "macos")]
                                let stderr_record = serde_json::json!({
                                        "at": Utc::now().to_rfc3339(),
                                        "event": "codex_stderr_line",
                                        "line": line,
                                        "diagnostic_line": trimmed,
                                });
                                #[cfg(not(target_os = "macos"))]
                                let stderr_record = serde_json::json!({
                                    "at": Utc::now().to_rfc3339(),
                                    "event": "codex_stderr_line",
                                    "line": trimmed,
                                });
                                crate::engines::codex::append_codex_transport_log(&stderr_record)
                                    .await;
                            }
                        }
                        Ok(None) => {
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_stderr_reader_eof",
                            }))
                            .await;
                            break;
                        }
                        Err(error) => {
                            log::debug!("codex stderr read error: {error}");
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_stderr_reader_error",
                                "error": error.to_string(),
                            }))
                            .await;
                            break;
                        }
                    }
                }
            });
        }

        Ok(Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            websocket_writer: Mutex::new(None),
            websocket_alive: Arc::new(AtomicBool::new(false)),
            pending,
            incoming_tx,
            last_event,
            last_stderr,
            next_request_id: std::sync::atomic::AtomicU64::new(1),
            next_incoming_sequence,
        })
    }

    pub async fn connect_websocket(url: &str) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_websocket_connect_start",
            "url": url,
            "auracoder_pid": std::process::id(),
        }))
        .await;

        let (socket, response) = match connect_async(url).await {
            Ok(result) => result,
            Err(error) => {
                #[cfg(target_os = "macos")]
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_websocket_connect_failed",
                    "url": url,
                    "auracoder_pid": std::process::id(),
                    "error": error.to_string(),
                }))
                .await;
                return Err(error).with_context(|| {
                    format!("failed to connect codex app-server websocket `{url}`")
                });
            }
        };

        #[cfg(target_os = "macos")]
        {
            // 记录 WebSocket 握手响应中的完整状态和逐项请求头，便于核对连接结果。
            let http_headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    let value = match value.to_str() {
                        Ok(value) => serde_json::Value::String(value.to_string()),
                        Err(_) => serde_json::Value::Array(
                            value
                                .as_bytes()
                                .iter()
                                .copied()
                                .map(serde_json::Value::from)
                                .collect(),
                        ),
                    };
                    serde_json::json!({
                        "name": name.as_str(),
                        "value": value,
                    })
                })
                .collect::<Vec<_>>();
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_websocket_connect_succeeded",
                "url": url,
                "auracoder_pid": std::process::id(),
                "http_status": response.status().as_u16(),
                "http_headers": http_headers,
            }))
            .await;
        }
        let (writer, mut reader) = socket.split();
        let (incoming_tx, _) = broadcast::channel(INCOMING_EVENT_BUFFER_CAPACITY);
        let next_incoming_sequence = Arc::new(AtomicU64::new(1));
        let pending = Arc::new(Mutex::new(
            HashMap::<String, oneshot::Sender<RpcResponse>>::new(),
        ));
        let last_event = Arc::new(Mutex::new(None));
        let last_stderr = Arc::new(Mutex::new(None));
        let websocket_alive = Arc::new(AtomicBool::new(true));

        {
            let pending = pending.clone();
            let incoming_tx = incoming_tx.clone();
            let last_event = last_event.clone();
            let next_incoming_sequence = next_incoming_sequence.clone();
            let websocket_alive = websocket_alive.clone();
            tokio::spawn(async move {
                while let Some(message) = reader.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            dispatch_websocket_line(
                                text.as_str(),
                                &pending,
                                &incoming_tx,
                                &last_event,
                                &next_incoming_sequence,
                            )
                            .await;
                        }
                        Ok(Message::Binary(bytes)) => match std::str::from_utf8(bytes.as_ref()) {
                            Ok(text) => {
                                dispatch_websocket_line(
                                    text,
                                    &pending,
                                    &incoming_tx,
                                    &last_event,
                                    &next_incoming_sequence,
                                )
                                .await;
                            }
                            Err(error) => {
                                log::warn!("codex websocket returned non-UTF-8 payload: {error}");
                                #[cfg(target_os = "macos")]
                                crate::engines::codex::append_codex_transport_log(
                                    &serde_json::json!({
                                        "at": Utc::now().to_rfc3339(),
                                        "event": "codex_websocket_binary_decode_error",
                                        "error": error.to_string(),
                                        "payload_bytes": bytes.len(),
                                        "payload": bytes.as_ref(),
                                    }),
                                )
                                .await;
                            }
                        },
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                        Ok(Message::Close(frame)) => {
                            #[cfg(target_os = "macos")]
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_websocket_closed",
                                "close_code": frame
                                    .as_ref()
                                    .map(|frame| u16::from(frame.code)),
                                "close_reason": frame
                                    .as_ref()
                                    .map(|frame| frame.reason.to_string()),
                            }))
                            .await;
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            log::warn!("codex websocket read error: {error}");
                            #[cfg(target_os = "macos")]
                            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                                "at": Utc::now().to_rfc3339(),
                                "event": "codex_websocket_read_error",
                                "error": error.to_string(),
                            }))
                            .await;
                            break;
                        }
                    }
                }
                websocket_alive.store(false, Ordering::Relaxed);
                let sequence = next_incoming_sequence.fetch_add(1, Ordering::Relaxed);
                publish_transport_message(
                    &incoming_tx,
                    sequence,
                    IncomingMessage::Notification {
                        method: "transport/eof".to_string(),
                        params: serde_json::value::RawValue::from_string("{}".to_string())
                            .expect("\"{}\" is valid json"),
                    },
                    "websocket_eof",
                )
                .await;
            });
        }

        Ok(Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            websocket_writer: Mutex::new(Some(writer)),
            websocket_alive,
            pending,
            incoming_tx,
            last_event,
            last_stderr,
            next_request_id: AtomicU64::new(1),
            next_incoming_sequence,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodexTransportMessage> {
        self.incoming_tx.subscribe()
    }

    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let request_started_at = std::time::Instant::now();
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_request_start",
            "method": method,
            "timeout_ms": timeout.as_millis(),
        }))
        .await;
        self.ensure_alive().await?;

        let id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();

        let payload = request_payload(&id, method, params);
        let (sender, receiver) = oneshot::channel::<RpcResponse>();
        self.pending.lock().await.insert(id.clone(), sender);

        if let Err(error) = self.write_payload(&payload).await {
            self.pending.lock().await.remove(&id);
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_rpc_request_complete",
                "request_id": id,
                "method": method,
                "result": "write_error",
                "error": error.to_string(),
                "elapsed_ms": request_started_at.elapsed().as_millis(),
            }))
            .await;
            return Err(error);
        }

        let response = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_rpc_request_complete",
                    "request_id": id,
                    "method": method,
                    "result": "response_channel_closed",
                    "elapsed_ms": request_started_at.elapsed().as_millis(),
                }))
                .await;
                anyhow::bail!("codex response channel closed for method `{method}`")
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_rpc_request_complete",
                    "request_id": id,
                    "method": method,
                    "result": "timeout",
                    "elapsed_ms": request_started_at.elapsed().as_millis(),
                }))
                .await;
                anyhow::bail!("codex request timeout for method `{method}`")
            }
        };

        if let Some(error) = response.error {
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_rpc_request_complete",
                "request_id": id,
                "method": method,
                "result": "rpc_error",
                "error": error.message.clone(),
                "rpc_error": &error,
                "elapsed_ms": request_started_at.elapsed().as_millis(),
            }))
            .await;
            // 迁移留痕：旧逻辑会丢失 RpcError 的 code/data，禁止恢复执行。
            // anyhow::bail!("{}", error);
            // 保留结构化 RPC 错误，调用方才能依据真实 code/message/data 区分
            // “会话不存在”和连接、服务、协议等其他失败。
            return Err(anyhow::Error::new(error));
        }

        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_request_complete",
            "request_id": id,
            "method": method,
            "result": "ok",
            "elapsed_ms": request_started_at.elapsed().as_millis(),
        }))
        .await;
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    pub async fn notify(&self, method: &str, params: serde_json::Value) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_notify_start",
            "method": method,
        }))
        .await;
        self.ensure_alive().await?;
        let result = self
            .write_payload(&notification_payload(method, params))
            .await;
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_notify_complete",
            "method": method,
            "result": if result.is_ok() { "ok" } else { "error" },
            "error": result.as_ref().err().map(ToString::to_string),
            "elapsed_ms": started_at.elapsed().as_millis(),
        }))
        .await;
        result
    }

    pub async fn respond_success(
        &self,
        request_id: &serde_json::Value,
        result: serde_json::Value,
    ) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_response_start",
            "response_kind": "success",
        }))
        .await;
        self.ensure_alive().await?;
        let result = self
            .write_payload(&response_success_payload(request_id, result))
            .await;
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_response_complete",
            "response_kind": "success",
            "result": if result.is_ok() { "ok" } else { "error" },
            "error": result.as_ref().err().map(ToString::to_string),
            "elapsed_ms": started_at.elapsed().as_millis(),
        }))
        .await;
        result
    }

    pub async fn respond_error(
        &self,
        request_id: &serde_json::Value,
        code: i64,
        message: &str,
        data: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let started_at = std::time::Instant::now();
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_response_start",
            "response_kind": "error",
            "code": code,
        }))
        .await;
        self.ensure_alive().await?;
        let result = self
            .write_payload(&response_error_payload(request_id, code, message, data))
            .await;
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_rpc_response_complete",
            "response_kind": "error",
            "code": code,
            "result": if result.is_ok() { "ok" } else { "error" },
            "error": result.as_ref().err().map(ToString::to_string),
            "elapsed_ms": started_at.elapsed().as_millis(),
        }))
        .await;
        result
    }

    pub async fn is_alive(&self) -> bool {
        self.ensure_alive().await.is_ok()
    }

    pub async fn diagnostics(&self) -> CodexTransportDiagnostics {
        let (pid, process_status) = {
            let mut child = self.child.lock().await;
            match child.as_mut() {
                Some(child) => {
                    let status = match child.try_wait() {
                        Ok(Some(status)) => Some(format!("exited: {status}")),
                        Ok(None) => Some("running".to_string()),
                        Err(error) => Some(format!("status_error: {error}")),
                    };
                    (child.id(), status)
                }
                None => (
                    None,
                    Some(if self.websocket_alive.load(Ordering::Relaxed) {
                        "websocket_connected".to_string()
                    } else {
                        "websocket_closed".to_string()
                    }),
                ),
            }
        };

        CodexTransportDiagnostics {
            pid,
            process_status,
            pending_count: self.pending.lock().await.len(),
            broadcast_receiver_count: self.incoming_tx.receiver_count(),
            broadcast_capacity: INCOMING_EVENT_BUFFER_CAPACITY,
            next_incoming_sequence: self.next_incoming_sequence.load(Ordering::Relaxed),
            last_event: self.last_event.lock().await.clone(),
            last_stderr: self.last_stderr.lock().await.clone(),
        }
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let mut child = self.child.lock().await;
        if let Some(child) = child.as_mut() {
            if child.try_wait()?.is_none() {
                child.kill().await.ok();
                child.wait().await.ok();
            }
        }
        self.websocket_alive.store(false, Ordering::Relaxed);
        if let Some(writer) = self.websocket_writer.lock().await.as_mut() {
            writer.close().await.ok();
        }
        Ok(())
    }

    async fn write_payload(&self, payload: &serde_json::Value) -> anyhow::Result<()> {
        if let Some(writer) = self.websocket_writer.lock().await.as_mut() {
            #[cfg(target_os = "macos")]
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_transport_outbound_payload",
                "source": "websocket",
                "payload": payload,
            }))
            .await;
            writer
                .send(Message::Text(serde_json::to_string(payload)?.into()))
                .await
                .context("failed writing payload to codex websocket")?;
            return Ok(());
        }

        let serialized = serde_json::to_vec(payload)?;
        let mut stdin = self.stdin.lock().await;
        let stdin = stdin
            .as_mut()
            .context("codex stdio transport is not available")?;
        #[cfg(target_os = "macos")]
        crate::engines::codex::append_codex_transport_log(&serde_json::json!({
            "at": Utc::now().to_rfc3339(),
            "event": "codex_transport_outbound_payload",
            "source": "stdio",
            "payload": payload,
        }))
        .await;
        stdin
            .write_all(&serialized)
            .await
            .context("failed writing payload to codex stdin")?;
        stdin
            .write_all(b"\n")
            .await
            .context("failed writing line terminator to codex stdin")?;
        stdin.flush().await.context("failed flushing codex stdin")?;
        Ok(())
    }

    async fn ensure_alive(&self) -> anyhow::Result<()> {
        let mut child = self.child.lock().await;
        if let Some(child) = child.as_mut() {
            let codex_pid = child.id();
            if let Some(status) = child
                .try_wait()
                .context("failed to query codex process status")?
            {
                #[cfg(target_os = "macos")]
                crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                    "at": Utc::now().to_rfc3339(),
                    "event": "codex_process_exited",
                    "auracoder_pid": std::process::id(),
                    "codex_pid": codex_pid,
                    "status": status.to_string(),
                }))
                .await;
                anyhow::bail!("codex app-server exited with status {status}");
            }
            return Ok(());
        }
        if !self.websocket_alive.load(Ordering::Relaxed) {
            #[cfg(target_os = "macos")]
            crate::engines::codex::append_codex_transport_log(&serde_json::json!({
                "at": Utc::now().to_rfc3339(),
                "event": "codex_websocket_not_alive",
                "auracoder_pid": std::process::id(),
            }))
            .await;
            anyhow::bail!("codex app-server websocket is closed");
        }
        Ok(())
    }
}

async fn dispatch_websocket_line(
    line: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<RpcResponse>>>>,
    incoming_tx: &broadcast::Sender<CodexTransportMessage>,
    last_event: &Arc<Mutex<Option<CodexTransportEventDiagnostics>>>,
    next_incoming_sequence: &Arc<AtomicU64>,
) {
    let sequence = next_incoming_sequence.fetch_add(1, Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": Utc::now().to_rfc3339(),
        "event": "codex_transport_inbound_payload",
        "source": "websocket",
        "sequence": sequence,
        "payload": line,
    }))
    .await;
    match parse_incoming(line) {
        Ok(IncomingMessage::Response(response)) => {
            let diagnostics = CodexTransportEventDiagnostics {
                sequence,
                at: Utc::now().to_rfc3339(),
                kind: "response".to_string(),
                method: None,
                id: Some(response.id.clone()),
            };
            record_last_event(last_event, diagnostics).await;
            if let Some(sender) = pending.lock().await.remove(&response.id) {
                let _ = sender.send(response);
            }
        }
        Ok(message) => {
            // WebSocket 与 stdio 共用同一筛选函数，且必须在消息裁剪前记录原始参数。
            if let Some(record) = subagent_protocol_log_record(&message, sequence, "websocket") {
                crate::engines::codex::append_codex_transport_log(&record).await;
            }
            #[cfg(target_os = "macos")]
            if let Some(record) =
                codex_error_notification_log_record(&message, sequence, "websocket")
            {
                crate::engines::codex::append_codex_transport_log(&record).await;
            }
            let mut diagnostics = diagnostics_for_message(&message);
            diagnostics.sequence = sequence;
            record_last_event(last_event, diagnostics).await;
            publish_transport_message(
                incoming_tx,
                sequence,
                trim_buffered_incoming_message(message),
                "websocket",
            )
            .await;
        }
        Err(error) => {
            log::warn!("codex websocket parse error: {error}");
            record_last_event(
                last_event,
                CodexTransportEventDiagnostics {
                    sequence,
                    at: Utc::now().to_rfc3339(),
                    kind: "parse_error".to_string(),
                    method: Some("transport/parse_error".to_string()),
                    id: None,
                },
            )
            .await;
            publish_transport_message(
                incoming_tx,
                sequence,
                IncomingMessage::Notification {
                    method: "transport/parse_error".to_string(),
                    params: transport_parse_error_payload(&error.to_string(), line),
                },
                "websocket_parse_error",
            )
            .await;
        }
    }
}

async fn record_last_event(
    last_event: &Arc<Mutex<Option<CodexTransportEventDiagnostics>>>,
    event: CodexTransportEventDiagnostics,
) {
    *last_event.lock().await = Some(event);
}

async fn publish_transport_message(
    incoming_tx: &broadcast::Sender<CodexTransportMessage>,
    sequence: u64,
    message: IncomingMessage,
    source: &str,
) {
    let diagnostics = diagnostics_for_message(&message);
    let receiver_count = incoming_tx.receiver_count();
    let envelope = CodexTransportMessage {
        sequence,
        published_at: std::time::Instant::now(),
        message,
    };
    let send_result = incoming_tx.send(envelope);
    crate::engines::codex::append_codex_transport_log(&serde_json::json!({
        "at": Utc::now().to_rfc3339(),
        "event": "codex_broadcast_publish",
        "sequence": sequence,
        "capacity": INCOMING_EVENT_BUFFER_CAPACITY,
        "source": source,
        "kind": diagnostics.kind,
        "method": diagnostics.method,
        "receiver_count_before": receiver_count,
        "receivers_notified": send_result.as_ref().ok().copied().unwrap_or(0),
        "send_result": if send_result.is_ok() { "published" } else { "no_receivers" },
    }))
    .await;
}

/// 从入站通知中筛选子代理生命周期事件，并保留完整参数用于协议排查。
fn subagent_protocol_log_record(
    message: &IncomingMessage,
    sequence: u64,
    source: &str,
) -> Option<serde_json::Value> {
    let IncomingMessage::Notification { method, params } = message else {
        return None;
    };

    let method_signature = method_signature(method);
    if method_signature != "itemstarted" && method_signature != "itemcompleted" {
        return None;
    }

    let params_value = serde_json::from_str::<serde_json::Value>(params.get()).ok()?;
    let item_type = params_value
        .get("item")
        .and_then(serde_json::Value::as_object)
        .and_then(|item| item.get("type"))
        .and_then(serde_json::Value::as_str)?;
    if item_type != "collabAgentToolCall" && item_type != "subAgentActivity" {
        return None;
    }

    Some(serde_json::json!({
        // 记录生成时间，便于与现有 transport 日志按时间交叉查询。
        "at": Utc::now().to_rfc3339(),
        // 固定事件名，便于从混合 transport 日志中筛选。
        "event": "codex_subagent_protocol_item",
        // 记录实际接收通道，stdio 与 websocket 保持相同字段结构。
        "source": source,
        // 记录入站序号，便于恢复协议事件顺序。
        "sequence": sequence,
        // 保留协议原始方法名，不使用规范化后的签名。
        "method": method,
        // 保留完整参数对象，包括当前实现尚未识别的字段。
        "params": params_value,
    }))
}

/// 记录 macOS 下 Codex app-server 的完整 error 通知参数，便于诊断本机对话失败原因。
#[cfg(target_os = "macos")]
fn codex_error_notification_log_record(
    message: &IncomingMessage,
    sequence: u64,
    source: &str,
) -> Option<serde_json::Value> {
    let IncomingMessage::Notification { method, params } = message else {
        return None;
    };

    if method_signature(method) != "error" {
        return None;
    }

    let params_value = serde_json::from_str::<serde_json::Value>(params.get())
        .unwrap_or_else(|_| serde_json::Value::String(params.get().to_string()));

    Some(serde_json::json!({
        // 记录生成时间，便于与现有 transport 日志按时间交叉查询。
        "at": Utc::now().to_rfc3339(),
        // 固定事件名，便于从混合 transport 日志中筛选 Codex error 通知。
        "event": "codex_error_notification",
        // 记录实际接收通道，stdio 与 websocket 保持相同字段结构。
        "source": source,
        // 记录入站序号，便于恢复协议事件顺序。
        "sequence": sequence,
        // 保留协议原始方法名，不使用规范化后的签名。
        "method": method,
        // 保留完整参数对象；解析异常时保留原始 JSON 文本。
        "params": params_value,
    }))
}

fn diagnostics_for_message(message: &IncomingMessage) -> CodexTransportEventDiagnostics {
    match message {
        IncomingMessage::Response(response) => CodexTransportEventDiagnostics {
            sequence: 0,
            at: Utc::now().to_rfc3339(),
            kind: "response".to_string(),
            method: None,
            id: Some(response.id.clone()),
        },
        IncomingMessage::Request { id, method, .. } => CodexTransportEventDiagnostics {
            sequence: 0,
            at: Utc::now().to_rfc3339(),
            kind: "request".to_string(),
            method: Some(method.clone()),
            id: Some(id.clone()),
        },
        IncomingMessage::Notification { method, .. } => CodexTransportEventDiagnostics {
            sequence: 0,
            at: Utc::now().to_rfc3339(),
            kind: "notification".to_string(),
            method: Some(method.clone()),
            id: None,
        },
    }
}

fn trim_buffered_incoming_message(message: IncomingMessage) -> IncomingMessage {
    match message {
        IncomingMessage::Notification { method, params } => IncomingMessage::Notification {
            params: trim_large_output_params(&method, params),
            method,
        },
        IncomingMessage::Request {
            id,
            raw_id,
            method,
            params,
        } => IncomingMessage::Request {
            id,
            raw_id,
            params: trim_large_output_params(&method, params),
            method,
        },
        IncomingMessage::Response(response) => IncomingMessage::Response(response),
    }
}

fn transport_parse_error_payload(error: &str, line: &str) -> Box<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&serde_json::json!({
        "error": error,
        "line": trim_transport_error_line(line),
    }))
    .expect("internal error payload is valid json")
}

fn trim_transport_error_line(line: &str) -> String {
    if line.chars().count() <= TRANSPORT_ERROR_LINE_MAX_CHARS {
        return line.to_string();
    }

    let tail_chars = TRANSPORT_ERROR_LINE_MAX_CHARS
        .saturating_sub(TRANSPORT_ERROR_LINE_TRUNCATED_PREFIX.len())
        .max(1);
    let mut tail = line.chars().rev().take(tail_chars).collect::<Vec<_>>();
    tail.reverse();

    format!(
        "{}{}",
        TRANSPORT_ERROR_LINE_TRUNCATED_PREFIX,
        tail.into_iter().collect::<String>()
    )
}

fn trim_large_output_params(
    method: &str,
    params: Box<serde_json::value::RawValue>,
) -> Box<serde_json::value::RawValue> {
    if !is_large_output_event(method) {
        return params;
    }

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(params.get()) else {
        return params;
    };

    if method_signature(method).contains("terminalinteraction") {
        trim_string_field(&mut value, "stdin");
    } else {
        for key in ["delta", "output", "text", "content"] {
            trim_string_field(&mut value, key);
        }
    }

    serde_json::value::to_raw_value(&value).unwrap_or(params)
}

fn trim_string_field(value: &mut serde_json::Value, key: &str) {
    let Some(field) = value.get_mut(key) else {
        return;
    };
    let Some(content) = field.as_str() else {
        return;
    };

    *field = serde_json::Value::String(trim_action_output_delta_content(content));
}

fn is_large_output_event(method: &str) -> bool {
    matches!(
        method_signature(method).as_str(),
        "itemcommandexecutionoutputdelta"
            | "itemfilechangeoutputdelta"
            | "itemcommandexecutionterminalinteraction"
            | "terminalinteraction"
    )
}

fn method_signature(method: &str) -> String {
    method
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/*
旧 codex_augmented_path 实现由 runtime_env::get 接替，保留代码以便追溯：
fn codex_augmented_path(executable: &str) -> Option<OsString> {
    runtime_env::augmented_path_with_prepend([Path::new(executable).parent()?.to_path_buf()])
}
*/

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn incoming_event_buffer_capacity_bounds_idle_retention() {
        assert!(
            INCOMING_EVENT_BUFFER_CAPACITY <= 6400,
            "Codex incoming events are live fan-out only; raising this can retain large protocol payloads while idle"
        );
    }

    #[test]
    fn subagent_protocol_log_record_preserves_collab_agent_params() {
        let params = serde_json::json!({
            "item": {
                "type": "collabAgentToolCall",
                "prompt": "Inspect the child agent activity",
                "agentPath": "/root/archive_logging",
                "futureTitleField": {
                    "label": "Fix child card hook order finished"
                }
            }
        });
        let message = IncomingMessage::Notification {
            method: "item/started".to_string(),
            params: serde_json::value::to_raw_value(&params).expect("valid params"),
        };

        let record = subagent_protocol_log_record(&message, 17, "stdio")
            .expect("collab agent item should be logged");

        assert_eq!(
            record.get("event"),
            Some(&serde_json::json!("codex_subagent_protocol_item"))
        );
        assert_eq!(record.get("source"), Some(&serde_json::json!("stdio")));
        assert_eq!(record.get("sequence"), Some(&serde_json::json!(17)));
        assert_eq!(
            record.get("method"),
            Some(&serde_json::json!("item/started"))
        );
        assert_eq!(record.get("params"), Some(&params));
    }

    #[test]
    fn subagent_protocol_log_record_preserves_subagent_activity_params() {
        let params = serde_json::json!({
            "item": {
                "type": "subAgentActivity",
                "agentPath": "/root/archive_logging",
                "unknownTitleCandidate": "Fix child card hook order finished"
            },
            "metadata": {
                "source": "codex"
            }
        });
        let message = IncomingMessage::Notification {
            method: "item/completed".to_string(),
            params: serde_json::value::to_raw_value(&params).expect("valid params"),
        };

        let record = subagent_protocol_log_record(&message, 23, "websocket")
            .expect("subagent activity item should be logged");

        assert_eq!(record.get("source"), Some(&serde_json::json!("websocket")));
        assert_eq!(record.get("sequence"), Some(&serde_json::json!(23)));
        assert_eq!(
            record.get("method"),
            Some(&serde_json::json!("item/completed"))
        );
        assert_eq!(record.get("params"), Some(&params));
    }

    #[test]
    fn subagent_protocol_log_record_excludes_non_subagent_messages() {
        let command_execution = IncomingMessage::Notification {
            method: "item/started".to_string(),
            params: serde_json::value::to_raw_value(&serde_json::json!({
                "item": {
                    "type": "commandExecution"
                }
            }))
            .expect("valid params"),
        };
        let non_item_method = IncomingMessage::Notification {
            method: "turn/started".to_string(),
            params: serde_json::value::to_raw_value(&serde_json::json!({
                "item": {
                    "type": "collabAgentToolCall"
                }
            }))
            .expect("valid params"),
        };
        let response = IncomingMessage::Response(RpcResponse {
            id: "response-1".to_string(),
            result: Some(serde_json::json!({})),
            error: None,
        });

        assert!(subagent_protocol_log_record(&command_execution, 1, "stdio").is_none());
        assert!(subagent_protocol_log_record(&non_item_method, 2, "stdio").is_none());
        assert!(subagent_protocol_log_record(&response, 3, "stdio").is_none());
    }

    #[tokio::test]
    async fn websocket_transport_routes_rpc_response() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("websocket handshake");
            let request = socket
                .next()
                .await
                .expect("request frame")
                .expect("request payload");
            let Message::Text(request) = request else {
                panic!("request must be text");
            };
            let request: Value = serde_json::from_str(request.as_str()).expect("request json");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": request.get("id").cloned().expect("request id"),
                        "result": {"pong": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("response");
        });

        let transport = CodexTransport::connect_websocket(&format!("ws://{address}"))
            .await
            .expect("connect websocket transport");
        let response = transport
            .request("test/ping", serde_json::json!({}), Duration::from_secs(2))
            .await
            .expect("rpc response");
        assert_eq!(response, serde_json::json!({"pong": true}));
        server.await.expect("server task");
    }

    #[test]
    fn transport_parse_error_payload_trims_large_protocol_lines() {
        let line = "x".repeat(TRANSPORT_ERROR_LINE_MAX_CHARS + 2048);

        let payload = transport_parse_error_payload("bad json", &line);
        let parsed: Value = serde_json::from_str(payload.get()).expect("valid json payload");
        let trimmed_line = parsed
            .get("line")
            .and_then(Value::as_str)
            .expect("line should be present");

        assert!(trimmed_line.starts_with(TRANSPORT_ERROR_LINE_TRUNCATED_PREFIX));
        assert!(trimmed_line.chars().count() <= TRANSPORT_ERROR_LINE_MAX_CHARS);
        assert!(trimmed_line.ends_with(&"x".repeat(64)));
        assert_eq!(
            parsed.get("error").and_then(Value::as_str),
            Some("bad json")
        );
    }

    #[tokio::test]
    async fn websocket_transport_preserves_structured_rpc_error_source() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("websocket handshake");
            let request = socket
                .next()
                .await
                .expect("request frame")
                .expect("request payload");
            let Message::Text(request) = request else {
                panic!("request must be text");
            };
            let request: Value = serde_json::from_str(request.as_str()).expect("request json");
            socket
                .send(
                    serde_json::json!({
                        "id": request.get("id").cloned().expect("request id"),
                        "error": {
                            "code": -32600,
                            "message": "thread not loaded: missing-thread",
                            "data": {
                                "source": "thread-store",
                                "details": {
                                    "path": "state_5.sqlite",
                                    "os_error": 2
                                }
                            }
                        }
                    })
                    .to_string()
                    .into(),
                )
                .await
                .expect("response");
        });

        let transport = CodexTransport::connect_websocket(&format!("ws://{address}"))
            .await
            .expect("connect websocket transport");
        let error = transport
            .request("thread/read", serde_json::json!({}), Duration::from_secs(2))
            .await
            .expect_err("RPC error should be returned");
        let rpc_error = error
            .downcast_ref::<RpcError>()
            .expect("structured RpcError source should be preserved");
        assert_eq!(rpc_error.code, Some(-32600));
        assert_eq!(rpc_error.message, "thread not loaded: missing-thread");
        assert_eq!(
            rpc_error.data,
            Some(serde_json::json!({
                "source": "thread-store",
                "details": {
                    "path": "state_5.sqlite",
                    "os_error": 2
                }
            }))
        );
        assert_eq!(
            serde_json::to_value(rpc_error).expect("serialize structured RPC error"),
            serde_json::json!({
                "code": -32600,
                "message": "thread not loaded: missing-thread",
                "data": {
                    "source": "thread-store",
                    "details": {
                        "path": "state_5.sqlite",
                        "os_error": 2
                    }
                }
            })
        );
        server.await.expect("server task");
    }
}
