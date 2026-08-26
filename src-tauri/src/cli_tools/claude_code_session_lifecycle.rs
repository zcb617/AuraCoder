use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::{
    sync::{Mutex, OnceCell},
    time::sleep,
};

use crate::{
    engines::claude_remote::ClaudeRemoteEngine,
    remote_project_claude_runtime_service::RemoteClaudeServiceUse,
};

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Claude Code 会话句柄生命周期管理类。
///
/// 该类只服务于会话复用线路，不改变现有的单轮启动线路。
/// 后续由 ClaudeCodeCli 根据系统运行模式选择是否操作该类。
#[allow(dead_code)]
pub(super) struct ClaudeCodeSessionHandleRegistry {
    client: Client,
    idle_timeout: Duration,
    handles: Mutex<HashMap<String, Arc<ClaudeCodeSessionSlot>>>,
}

#[allow(dead_code)]
struct ClaudeCodeSessionSlot {
    handle: OnceCell<ClaudeCodeSessionHandle>,
    lifecycle: Mutex<ClaudeCodeSessionLifecycle>,
    remote_base_url: Url,
    service_use: Option<RemoteClaudeServiceUse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(super) struct ClaudeCodeSessionHandle {
    pub thread_id: String,
    pub handle_id: String,
    pub session_id: Option<String>,
    pub reused: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(super) struct ClaudeCodeSessionMessageResult {
    pub thread_id: String,
    pub handle_id: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(super) struct ClaudeCodeSessionInterruptResult {
    pub thread_id: String,
    pub handle_id: String,
    pub interrupted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCodeSessionDestroyResult {
    thread_id: Option<String>,
    handle_id: Option<String>,
    success: bool,
    error: Option<String>,
}

#[allow(dead_code)]
struct ClaudeCodeSessionLifecycle {
    active: bool,
    idle_generation: u64,
}

impl Default for ClaudeCodeSessionLifecycle {
    fn default() -> Self {
        Self {
            active: true,
            idle_generation: 0,
        }
    }
}

pub(super) fn shared_claude_code_session_handles() -> Arc<ClaudeCodeSessionHandleRegistry> {
    static REGISTRY: OnceLock<Arc<ClaudeCodeSessionHandleRegistry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(ClaudeCodeSessionHandleRegistry::new()))
        .clone()
}

#[allow(dead_code)]
impl ClaudeCodeSessionHandleRegistry {
    pub fn new() -> Self {
        Self::with_idle_timeout(DEFAULT_IDLE_TIMEOUT)
    }

    fn with_idle_timeout(idle_timeout: Duration) -> Self {
        Self {
            client: Client::new(),
            idle_timeout,
            handles: Mutex::new(HashMap::new()),
        }
    }

    pub async fn contains(&self, thread_id: &str) -> bool {
        self.handles.lock().await.contains_key(thread_id)
    }

    pub async fn prepare_turn(self: &Arc<Self>, thread_id: &str) -> bool {
        let prepared = {
            let handles = self.handles.lock().await;
            let Some(slot) = handles.get(thread_id).cloned() else {
                return false;
            };
            let generation = {
                let mut lifecycle = slot.lifecycle.lock().await;
                lifecycle.active = false;
                lifecycle.idle_generation = lifecycle.idle_generation.wrapping_add(1);
                lifecycle.idle_generation
            };
            (slot, generation)
        };
        self.start_idle_countdown(thread_id.to_string(), prepared.0, prepared.1);
        true
    }

    pub async fn session_runtime(&self, thread_id: &str) -> Result<(Arc<ClaudeRemoteEngine>, Url)> {
        let slot = self
            .handles
            .lock()
            .await
            .get(thread_id)
            .cloned()
            .with_context(|| format!("Claude Code 会话句柄不存在: thread_id={thread_id}"))?;
        let service_use = slot.service_use.as_ref().with_context(|| {
            format!("Claude Code 会话没有对应的 SSH 远端服务占用: thread_id={thread_id}")
        })?;
        Ok((service_use.engine().clone(), slot.remote_base_url.clone()))
    }

    /// 首次发送消息时建立会话句柄；同一 AuraCoder 会话已经存在句柄时直接复用。
    pub async fn create_or_get(
        &self,
        thread_id: &str,
        remote_base_url: Url,
        service_use: Option<RemoteClaudeServiceUse>,
        request: Value,
    ) -> Result<ClaudeCodeSessionHandle> {
        let thread_id = thread_id.trim();
        anyhow::ensure!(!thread_id.is_empty(), "Claude Code 会话编号不能为空");

        let slot = {
            let mut handles = self.handles.lock().await;
            handles
                .entry(thread_id.to_string())
                .or_insert_with(|| {
                    Arc::new(ClaudeCodeSessionSlot {
                        handle: OnceCell::new(),
                        lifecycle: Mutex::new(ClaudeCodeSessionLifecycle::default()),
                        remote_base_url,
                        service_use,
                    })
                })
                .clone()
        };

        let result = slot
            .handle
            .get_or_try_init(|| async {
                let mut body = match request {
                    Value::Object(object) => object,
                    _ => Map::new(),
                };
                body.insert("threadId".to_string(), Value::String(thread_id.to_string()));
                self.client
                    .post(Self::endpoint(&slot.remote_base_url, &["session-handles"])?)
                    .json(&body)
                    .send()
                    .await
                    .context("调用 Claude Code 远端会话建立接口失败")?
                    .error_for_status()
                    .context("Claude Code 远端会话建立失败")?
                    .json::<ClaudeCodeSessionHandle>()
                    .await
                    .context("解析 Claude Code 远端会话句柄失败")
            })
            .await
            .cloned();

        if result.is_err() {
            let mut handles = self.handles.lock().await;
            if handles
                .get(thread_id)
                .is_some_and(|current| Arc::ptr_eq(current, &slot))
                && slot.handle.get().is_none()
            {
                handles.remove(thread_id);
            }
        }

        result
    }

    /// 同一会话发送后续消息时，通过远端组件把消息送入原 Claude Code 会话。
    pub async fn send_message(
        self: &Arc<Self>,
        thread_id: &str,
        request: Value,
    ) -> Result<ClaudeCodeSessionMessageResult> {
        let (slot, expected_handle_id, turn_generation) = {
            // MAP 和会话状态统一按照 MAP → 生命周期状态的顺序加锁。
            // 取得句柄与标记活跃必须处于同一个临界区，禁止空闲任务在中间删除句柄。
            let handles = self.handles.lock().await;
            let slot = handles
                .get(thread_id)
                .cloned()
                .with_context(|| format!("Claude Code 会话句柄不存在: thread_id={thread_id}"))?;
            let expected_handle_id = slot
                .handle
                .get()
                .with_context(|| format!("Claude Code 会话句柄尚未建立: thread_id={thread_id}"))?
                .handle_id
                .clone();
            let turn_generation = {
                let mut lifecycle = slot.lifecycle.lock().await;
                lifecycle.active = true;
                lifecycle.idle_generation = lifecycle.idle_generation.wrapping_add(1);
                lifecycle.idle_generation
            };
            (slot, expected_handle_id, turn_generation)
        };

        let body = match request {
            Value::Object(object) => Value::Object(object),
            _ => Value::Object(Map::new()),
        };
        let result = async {
            let result = self
                .client
                .post(Self::endpoint(
                    &slot.remote_base_url,
                    &["session-handles", thread_id, "messages"],
                )?)
                .json(&body)
                .send()
                .await
                .context("调用 Claude Code 远端连续消息接口失败")?
                .error_for_status()
                .context("Claude Code 远端连续消息发送失败")?
                .json::<ClaudeCodeSessionMessageResult>()
                .await
                .context("解析 Claude Code 远端连续消息结果失败")?;
            anyhow::ensure!(
                result.handle_id == expected_handle_id,
                "Claude Code 远端连续消息返回了其他会话句柄"
            );
            Ok(result)
        }
        .await;

        if result.is_err() {
            let idle_generation = {
                let mut lifecycle = slot.lifecycle.lock().await;
                if lifecycle.idle_generation != turn_generation {
                    return result;
                }
                lifecycle.active = false;
                lifecycle.idle_generation = lifecycle.idle_generation.wrapping_add(1);
                lifecycle.idle_generation
            };
            self.start_idle_countdown(thread_id.to_string(), slot, idle_generation);
        }

        result
    }

    /// 只中断当前一轮，保留会话句柄和对应的 Claude Code 进程。
    pub async fn interrupt(&self, thread_id: &str) -> Result<ClaudeCodeSessionInterruptResult> {
        let slot = self
            .handles
            .lock()
            .await
            .get(thread_id)
            .cloned()
            .with_context(|| format!("Claude Code 会话句柄不存在: thread_id={thread_id}"))?;
        let handle = slot
            .handle
            .get()
            .with_context(|| format!("Claude Code 会话句柄尚未建立: thread_id={thread_id}"))?;
        let result = self
            .client
            .post(Self::endpoint(
                &slot.remote_base_url,
                &["session-handles", thread_id, "interrupt"],
            )?)
            .send()
            .await
            .context("调用 Claude Code 远端会话中断接口失败")?
            .error_for_status()
            .context("Claude Code 远端会话中断失败")?
            .json::<ClaudeCodeSessionInterruptResult>()
            .await
            .context("解析 Claude Code 远端会话中断结果失败")?;
        anyhow::ensure!(
            result.handle_id == handle.handle_id,
            "Claude Code 远端中断返回了其他会话句柄"
        );
        Ok(result)
    }

    /// 一轮对话完成后开始独立的五分钟空闲计时。
    pub async fn mark_turn_completed(self: &Arc<Self>, thread_id: &str) -> Result<()> {
        let (slot, generation) = {
            let handles = self.handles.lock().await;
            let slot = handles
                .get(thread_id)
                .cloned()
                .with_context(|| format!("Claude Code 会话句柄不存在: thread_id={thread_id}"))?;
            let generation = {
                let mut lifecycle = slot.lifecycle.lock().await;
                lifecycle.active = false;
                lifecycle.idle_generation = lifecycle.idle_generation.wrapping_add(1);
                lifecycle.idle_generation
            };
            (slot, generation)
        };
        self.start_idle_countdown(thread_id.to_string(), slot, generation);
        Ok(())
    }

    fn start_idle_countdown(
        self: &Arc<Self>,
        thread_id: String,
        slot: Arc<ClaudeCodeSessionSlot>,
        generation: u64,
    ) {
        let registry = self.clone();
        tokio::spawn(async move {
            sleep(registry.idle_timeout).await;
            registry
                .close_if_still_idle(&thread_id, &slot, generation)
                .await;
        });
    }

    fn endpoint(remote_base_url: &Url, segments: &[&str]) -> Result<Url> {
        let mut endpoint = remote_base_url.clone();
        {
            let mut path = endpoint
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("Claude Code 远端组件地址不能作为接口地址"))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(endpoint)
    }

    async fn close_if_still_idle(
        &self,
        thread_id: &str,
        slot: &Arc<ClaudeCodeSessionSlot>,
        generation: u64,
    ) {
        {
            // 检查状态与删除 MAP 记录属于同一个临界区。
            // 新消息同样先锁 MAP 再锁生命周期状态，因此二者只能有一方完成状态变更。
            let mut handles = self.handles.lock().await;
            let Some(current) = handles.get(thread_id).cloned() else {
                return;
            };
            if !Arc::ptr_eq(&current, slot) {
                return;
            }
            let lifecycle = current.lifecycle.lock().await;
            if lifecycle.active || lifecycle.idle_generation != generation {
                return;
            }
            handles.remove(thread_id);
        }

        let result = async {
            let response = self
                .client
                .delete(Self::endpoint(
                    &slot.remote_base_url,
                    &["session-handles", thread_id],
                )?)
                .send()
                .await
                .context("调用 Claude Code 远端会话销毁接口失败")?;
            let status = response.status();
            let body = response
                .json::<ClaudeCodeSessionDestroyResult>()
                .await
                .context("解析 Claude Code 远端会话销毁结果失败")?;
            Ok::<_, anyhow::Error>((status, body))
        }
        .await;

        match result {
            Ok((status, body)) => {
                log::info!(
                    "Claude Code 空闲会话销毁结果: thread_id={} handle_id={:?} returned_thread_id={:?} success={} status={} error={:?}",
                    thread_id,
                    body.handle_id,
                    body.thread_id,
                    body.success,
                    status,
                    body.error,
                );
            }
            Err(error) => {
                log::warn!(
                    "Claude Code 空闲会话销毁结果: thread_id={} request_error={error:#}",
                    thread_id,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{sleep, Duration},
    };

    use super::ClaudeCodeSessionHandleRegistry;

    #[tokio::test]
    async fn reuses_handle_and_restarts_idle_countdown_after_send_failure() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("read test server address");
        let create_count = Arc::new(AtomicUsize::new(0));
        let message_count = Arc::new(AtomicUsize::new(0));
        let destroy_count = Arc::new(AtomicUsize::new(0));
        let server_create_count = create_count.clone();
        let server_message_count = message_count.clone();
        let server_destroy_count = destroy_count.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length: ")
                                    .or_else(|| line.strip_prefix("Content-Length: "))
                            })
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let request_line = request.lines().next().unwrap_or_default();
                let (status, body) = if request_line.starts_with("POST /session-handles ") {
                    server_create_count.fetch_add(1, Ordering::SeqCst);
                    (
                        "201 Created",
                        json!({
                            "threadId": "thread-1",
                            "handleId": "handle-1",
                            "sessionId": "session-1",
                            "reused": false,
                        }),
                    )
                } else if request_line.starts_with("POST /session-handles/thread-1/messages ") {
                    server_message_count.fetch_add(1, Ordering::SeqCst);
                    if request.contains("\"prompt\":\"fail\"") {
                        (
                            "500 Internal Server Error",
                            json!({ "error": "send failed" }),
                        )
                    } else {
                        (
                            "202 Accepted",
                            json!({
                                "threadId": "thread-1",
                                "handleId": "handle-1",
                                "accepted": true,
                            }),
                        )
                    }
                } else if request_line.starts_with("DELETE /session-handles/thread-1 ") {
                    server_destroy_count.fetch_add(1, Ordering::SeqCst);
                    (
                        "200 OK",
                        json!({
                            "threadId": "thread-1",
                            "handleId": "handle-1",
                            "success": true,
                            "error": null,
                        }),
                    )
                } else {
                    ("404 Not Found", json!({ "error": "not found" }))
                };
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        let base_url: reqwest::Url = format!("http://{address}").parse().expect("parse test url");
        let registry = Arc::new(ClaudeCodeSessionHandleRegistry::with_idle_timeout(
            Duration::from_millis(200),
        ));
        let first = registry
            .create_or_get(
                "thread-1",
                base_url.clone(),
                None,
                json!({ "prompt": "first" }),
            )
            .await
            .expect("create handle");
        let second = registry
            .create_or_get(
                "thread-1",
                base_url.clone(),
                None,
                json!({ "prompt": "unused" }),
            )
            .await
            .expect("reuse handle");
        assert_eq!(first.handle_id, second.handle_id);
        assert_eq!(create_count.load(Ordering::SeqCst), 1);

        registry
            .mark_turn_completed("thread-1")
            .await
            .expect("start first idle countdown");
        sleep(Duration::from_millis(80)).await;
        registry
            .send_message("thread-1", json!({ "prompt": "second" }))
            .await
            .expect("send second message");
        registry
            .mark_turn_completed("thread-1")
            .await
            .expect("restart idle countdown");
        sleep(Duration::from_millis(100)).await;
        assert_eq!(destroy_count.load(Ordering::SeqCst), 0);
        sleep(Duration::from_millis(160)).await;
        assert_eq!(message_count.load(Ordering::SeqCst), 1);
        assert_eq!(destroy_count.load(Ordering::SeqCst), 1);

        registry
            .create_or_get("thread-1", base_url, None, json!({ "prompt": "third" }))
            .await
            .expect("create replacement handle");
        registry
            .mark_turn_completed("thread-1")
            .await
            .expect("start replacement idle countdown");
        sleep(Duration::from_millis(80)).await;
        registry
            .send_message("thread-1", json!({ "prompt": "fail" }))
            .await
            .expect_err("reject failed message");
        sleep(Duration::from_millis(100)).await;
        assert_eq!(destroy_count.load(Ordering::SeqCst), 1);
        sleep(Duration::from_millis(160)).await;
        assert_eq!(message_count.load(Ordering::SeqCst), 2);
        assert_eq!(destroy_count.load(Ordering::SeqCst), 2);

        server.abort();
    }
}
