use std::{
    collections::{HashMap, HashSet},
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
    // 负责调用远端 Claude 会话服务的 HTTP 客户端。
    client: Client,
    // 负责控制本地会话句柄的空闲回收时长。
    idle_timeout: Duration,
    // 负责按 threadId 保存本地持续会话句柄。
    handles: Mutex<HashMap<String, Arc<ClaudeCodeSessionSlot>>>,
    // 负责记录远端销毁失败后下次创建必须替换旧句柄的 threadId。
    replacement_required_thread_ids: Mutex<HashSet<String>>,
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
            replacement_required_thread_ids: Mutex::new(HashSet::new()),
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

        // 读取当前 threadId 的恢复标记，只有标记存在时才请求远端替换旧句柄。
        let replace_existing = self
            .replacement_required_thread_ids
            .lock()
            .await
            .contains(thread_id);
        let result = slot
            .handle
            .get_or_try_init(|| async {
                let mut body = match request {
                    Value::Object(object) => object,
                    _ => Map::new(),
                };
                body.insert("threadId".to_string(), Value::String(thread_id.to_string()));
                if replace_existing {
                    body.insert("replaceExisting".to_string(), Value::Bool(true));
                } else {
                    // 未设置恢复标记时禁止发送替换控制字段，保持正常创建请求语义。
                    body.remove("replaceExisting");
                }
                let endpoint = Self::endpoint(&slot.remote_base_url, &["session-handles"])?;
                let response = self
                    .client
                    .post(endpoint.clone())
                    .json(&body)
                    .send()
                    .await
                    .context("调用 Claude Code 远端会话建立接口失败")?;
                let status = response.status();
                if !status.is_success() {
                    let response_body = match response.text().await {
                        Ok(body) => body,
                        Err(error) => {
                            let error_text = error.to_string();
                            log::error!(
                                "Claude Code 远端会话建立失败: event=claude_code_remote_session_create_failed thread_id={} endpoint={} status={} replace_existing={} response_body=<读取失败> response_body_read_error={}",
                                thread_id,
                                endpoint,
                                status,
                                replace_existing,
                                error_text,
                            );
                            return Err(anyhow::Error::new(error).context(format!(
                                "Claude Code 远端会话建立失败: thread_id={} endpoint={} status={} replace_existing={} response_body=<读取失败> response_body_read_error={}",
                                thread_id,
                                endpoint,
                                status,
                                replace_existing,
                                error_text,
                            )));
                        }
                    };
                    log::error!(
                        "Claude Code 远端会话建立失败: event=claude_code_remote_session_create_failed thread_id={} endpoint={} status={} replace_existing={} response_body={}",
                        thread_id,
                        endpoint,
                        status,
                        replace_existing,
                        response_body,
                    );
                    return Err(anyhow::anyhow!(
                        "Claude Code 远端会话建立失败: thread_id={} endpoint={} status={} replace_existing={} response_body={}",
                        thread_id,
                        endpoint,
                        status,
                        replace_existing,
                        response_body,
                    ));
                }
                response
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
        if result.is_ok() && replace_existing {
            // 远端新句柄已成功建立，本地恢复标记不再需要继续触发替换。
            self.replacement_required_thread_ids
                .lock()
                .await
                .remove(thread_id);
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
        let endpoint = Self::endpoint(
            &slot.remote_base_url,
            &["session-handles", thread_id, "messages"],
        )?;
        let result = async {
            // 原有直接 error_for_status 调用保留为注释，避免非 2xx 响应体被丢弃。
            // .error_for_status()
            // .context("Claude Code 远端连续消息发送失败")?
            let response = self
                .client
                .post(endpoint.clone())
                .json(&body)
                .send()
                .await
                .context("调用 Claude Code 远端连续消息接口失败")?;
            let status = response.status();
            if !status.is_success() {
                let response_body = match response.text().await {
                    Ok(body) => body,
                    Err(error) => {
                        let error_text = error.to_string();
                        log::error!(
                            "Claude Code 远端连续消息发送失败: event=claude_code_remote_session_message_failed thread_id={} expected_handle_id={} endpoint={} status={} response_body=<读取失败> response_body_read_error={}",
                            thread_id,
                            expected_handle_id,
                            endpoint,
                            status,
                            error_text,
                        );
                        return Err(anyhow::Error::new(error).context(format!(
                            "Claude Code 远端连续消息发送失败: thread_id={} handle_id={} endpoint={} status={} response_body=<读取失败> response_body_read_error={}",
                            thread_id,
                            expected_handle_id,
                            endpoint,
                            status,
                            error_text,
                        )));
                    }
                };
                log::error!(
                    "Claude Code 远端连续消息发送失败: event=claude_code_remote_session_message_failed thread_id={} expected_handle_id={} endpoint={} status={} response_body={}",
                    thread_id,
                    expected_handle_id,
                    endpoint,
                    status,
                    response_body,
                );
                return Err(anyhow::anyhow!(
                    "Claude Code 远端连续消息发送失败: thread_id={} handle_id={} endpoint={} status={} response_body={}",
                    thread_id,
                    expected_handle_id,
                    endpoint,
                    status,
                    response_body,
                ));
            }
            let result = response
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
        let endpoint = Self::endpoint(
            &slot.remote_base_url,
            &["session-handles", thread_id, "interrupt"],
        )?;
        // 原有直接 error_for_status 调用保留为注释，避免非 2xx 响应体被丢弃。
        // .error_for_status()
        // .context("Claude Code 远端会话中断失败")?
        let response = self
            .client
            .post(endpoint.clone())
            .send()
            .await
            .context("调用 Claude Code 远端会话中断接口失败")?;
        let status = response.status();
        if !status.is_success() {
            let response_body = match response.text().await {
                Ok(body) => body,
                Err(error) => {
                    let error_text = error.to_string();
                    log::error!(
                        "Claude Code 远端会话中断失败: event=claude_code_remote_session_interrupt_failed thread_id={} handle_id={} endpoint={} status={} response_body=<读取失败> response_body_read_error={}",
                        thread_id,
                        handle.handle_id,
                        endpoint,
                        status,
                        error_text,
                    );
                    return Err(anyhow::Error::new(error).context(format!(
                        "Claude Code 远端会话中断失败: thread_id={} handle_id={} endpoint={} status={} response_body=<读取失败> response_body_read_error={}",
                        thread_id,
                        handle.handle_id,
                        endpoint,
                        status,
                        error_text,
                    )));
                }
            };
            log::error!(
                "Claude Code 远端会话中断失败: event=claude_code_remote_session_interrupt_failed thread_id={} handle_id={} endpoint={} status={} response_body={}",
                thread_id,
                handle.handle_id,
                endpoint,
                status,
                response_body,
            );
            return Err(anyhow::anyhow!(
                "Claude Code 远端会话中断失败: thread_id={} handle_id={} endpoint={} status={} response_body={}",
                thread_id,
                handle.handle_id,
                endpoint,
                status,
                response_body,
            ));
        }
        let result = response
            .json::<ClaudeCodeSessionInterruptResult>()
            .await
            .context("解析 Claude Code 远端会话中断结果失败")?;
        anyhow::ensure!(
            result.handle_id == handle.handle_id,
            "Claude Code 远端中断返回了其他会话句柄"
        );
        Ok(result)
    }

    /// relay 持续轮次失败时移除本地句柄并立即销毁远端持续会话，避免失效句柄进入空闲复用。
    pub(super) async fn discard_failed_turn(&self, thread_id: &str) -> Result<()> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            let error = anyhow::anyhow!("Claude Code 会话编号不能为空");
            log::error!(
                "Claude Code 失败轮次会话销毁失败: event=claude_code_failed_turn_discard thread_id=<empty> handle_id=<unknown> endpoint=<unknown> status=<unavailable> response_body=<none> request_error={error:#}",
            );
            return Err(error);
        }

        let slot = {
            let mut handles = self.handles.lock().await;
            match handles.remove(thread_id) {
                Some(slot) => slot,
                None => {
                    let error = anyhow::anyhow!(
                        "Claude Code 失败轮次会话句柄不存在: thread_id={thread_id}"
                    );
                    log::error!(
                        "Claude Code 失败轮次会话销毁失败: event=claude_code_failed_turn_discard thread_id={} handle_id=<unknown> endpoint=<unknown> status=<unavailable> response_body=<none> request_error={error:#}",
                        thread_id,
                    );
                    return Err(error);
                }
            }
        };

        /*
        // 原有失败轮次直接返回远端销毁结果的实现保留为迁移留痕；远端销毁失败时现需登记恢复标记。
        self.destroy_remote_handle(thread_id, slot.as_ref(), "claude_code_failed_turn_discard")
            .await
            .map(|_| ())
        */
        let result = self
            .destroy_remote_handle(thread_id, slot.as_ref(), "claude_code_failed_turn_discard")
            .await;
        match result {
            Ok(_) => {
                // 远端旧句柄已确认销毁，清除该 threadId 的替换恢复标记。
                self.replacement_required_thread_ids
                    .lock()
                    .await
                    .remove(thread_id);
                Ok(())
            }
            Err(error) => {
                // 远端销毁失败时保留原始错误，并登记下次创建必须替换旧句柄。
                self.replacement_required_thread_ids
                    .lock()
                    .await
                    .insert(thread_id.to_string());
                Err(error)
            }
        }
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

    /// 在给定持续会话句柄上执行远端 DELETE，读取原始响应并校验销毁结果。
    async fn destroy_remote_handle(
        &self,
        thread_id: &str,
        slot: &ClaudeCodeSessionSlot,
        event: &str,
    ) -> Result<(Url, reqwest::StatusCode, ClaudeCodeSessionDestroyResult)> {
        let handle_id = match slot.handle.get() {
            Some(handle) => handle.handle_id.clone(),
            None => {
                let error = anyhow::anyhow!(
                    "Claude Code 会话句柄尚未建立: thread_id={thread_id}"
                );
                log::error!(
                    "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id=<unknown> endpoint=<unknown> status=<unavailable> response_body=<none> request_error={error:#}",
                    event,
                    thread_id,
                );
                return Err(error);
            }
        };
        let endpoint = match Self::endpoint(&slot.remote_base_url, &["session-handles", thread_id])
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let error = error.context(format!(
                    "构造 Claude Code 远端会话销毁地址失败: thread_id={thread_id} handle_id={handle_id} endpoint=<unavailable>"
                ));
                log::error!(
                    "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint=<unavailable> status=<unavailable> response_body=<none> request_error={error:#}",
                    event,
                    thread_id,
                    handle_id,
                );
                return Err(error);
            }
        };

        let response = match self.client.delete(endpoint.clone()).send().await {
            Ok(response) => response,
            Err(error) => {
                let error = anyhow::Error::new(error).context(format!(
                    "调用 Claude Code 远端会话销毁接口失败: thread_id={thread_id} handle_id={handle_id} endpoint={endpoint} request_error"
                ));
                log::error!(
                    "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint={} status=<unavailable> response_body=<none> request_error={error:#}",
                    event,
                    thread_id,
                    handle_id,
                    endpoint,
                );
                return Err(error);
            }
        };
        let status = response.status();
        let response_body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                let error_text = error.to_string();
                let error = anyhow::Error::new(error).context(format!(
                    "读取 Claude Code 远端会话销毁响应失败: thread_id={thread_id} handle_id={handle_id} endpoint={endpoint} status={status} response_body=<读取失败> response_body_read_error={error_text}"
                ));
                log::error!(
                    "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint={} status={} response_body=<读取失败> response_body_read_error={}",
                    event,
                    thread_id,
                    handle_id,
                    endpoint,
                    status,
                    error_text,
                );
                return Err(error);
            }
        };
        if !status.is_success() {
            let error = anyhow::anyhow!(
                "Claude Code 远端会话销毁失败: thread_id={} handle_id={} endpoint={} status={} response_body={}",
                thread_id,
                handle_id,
                endpoint,
                status,
                response_body,
            );
            log::error!(
                "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint={} status={} response_body={}",
                event,
                thread_id,
                handle_id,
                endpoint,
                status,
                response_body,
            );
            return Err(error);
        }

        let destroy_result = match serde_json::from_str::<ClaudeCodeSessionDestroyResult>(
            &response_body,
        ) {
            Ok(result) => result,
            Err(error) => {
                let error_text = error.to_string();
                let error = anyhow::Error::new(error).context(format!(
                    "解析 Claude Code 远端会话销毁结果失败: thread_id={thread_id} handle_id={handle_id} endpoint={endpoint} status={status} response_body={response_body} response_parse_error={error_text}"
                ));
                log::error!(
                    "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint={} status={} response_body={} response_parse_error={}",
                    event,
                    thread_id,
                    handle_id,
                    endpoint,
                    status,
                    response_body,
                    error_text,
                );
                return Err(error);
            }
        };
        if destroy_result.handle_id.as_deref() != Some(handle_id.as_str())
            || !destroy_result.success
        {
            let error = anyhow::anyhow!(
                "Claude Code 远端会话销毁返回无效结果: thread_id={} handle_id={} endpoint={} status={} returned_handle_id={:?} success={} error={:?} response_body={}",
                thread_id,
                handle_id,
                endpoint,
                status,
                destroy_result.handle_id,
                destroy_result.success,
                destroy_result.error,
                response_body,
            );
            log::error!(
                "Claude Code 远端会话销毁失败: event={} thread_id={} handle_id={} endpoint={} status={} returned_handle_id={:?} success={} error={:?} response_body={}",
                event,
                thread_id,
                handle_id,
                endpoint,
                status,
                destroy_result.handle_id,
                destroy_result.success,
                destroy_result.error,
                response_body,
            );
            return Err(error);
        }

        Ok((endpoint, status, destroy_result))
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

        /*
        // 原有空闲会话直接 DELETE 实现保留为迁移留痕，现由统一远端销毁辅助逻辑接替：
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
        */
        let result = self
            .destroy_remote_handle(thread_id, slot.as_ref(), "claude_code_idle_session_destroy")
            .await;

        match result {
            Ok((endpoint, status, body)) => {
                log::info!(
                    "Claude Code 空闲会话销毁结果: event=claude_code_idle_session_destroy thread_id={} handle_id={:?} returned_thread_id={:?} success={} endpoint={} status={} error={:?}",
                    thread_id,
                    body.handle_id,
                    body.thread_id,
                    body.success,
                    endpoint,
                    status,
                    body.error,
                );
            }
            Err(error) => {
                log::warn!(
                    "Claude Code 空闲会话销毁结果: event=claude_code_idle_session_destroy thread_id={} request_error={error:#}",
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
        // 原有只验证请求失败的断言保留为注释，新增原始错误内容断言。
        // registry
        //     .send_message("thread-1", json!({ "prompt": "fail" }))
        //     .await
        //     .expect_err("reject failed message");
        let send_error = registry
            .send_message("thread-1", json!({ "prompt": "fail" }))
            .await
            .expect_err("reject failed message");
        let send_error_text = format!("{send_error:#}");
        assert!(send_error_text.contains("Claude Code 远端连续消息发送失败"));
        assert!(send_error_text.contains("500 Internal Server Error"));
        assert!(send_error_text.contains("send failed"));
        sleep(Duration::from_millis(100)).await;
        assert_eq!(destroy_count.load(Ordering::SeqCst), 1);
        sleep(Duration::from_millis(160)).await;
        assert_eq!(message_count.load(Ordering::SeqCst), 2);
        assert_eq!(destroy_count.load(Ordering::SeqCst), 2);

        server.abort();
    }

    #[tokio::test]
    async fn preserves_interrupt_failure_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind interrupt test server");
        let address = listener
            .local_addr()
            .expect("read interrupt test server address");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("accept interrupt request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read interrupt request");
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
                    (
                        "201 Created",
                        json!({
                            "threadId": "thread-1",
                            "handleId": "handle-1",
                            "sessionId": "session-1",
                            "reused": false,
                        }),
                    )
                } else if request_line.starts_with("POST /session-handles/thread-1/interrupt ") {
                    (
                        "500 Internal Server Error",
                        json!({ "error": "interrupt failed" }),
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
                    .expect("write interrupt response");
            }
        });

        let base_url: reqwest::Url = format!("http://{address}")
            .parse()
            .expect("parse interrupt test url");
        let registry = ClaudeCodeSessionHandleRegistry::with_idle_timeout(Duration::from_secs(1));
        registry
            .create_or_get("thread-1", base_url, None, json!({ "prompt": "first" }))
            .await
            .expect("create interrupt test handle");
        let error = registry
            .interrupt("thread-1")
            .await
            .expect_err("reject failed interrupt");
        let error_text = format!("{error:#}");
        assert!(error_text.contains("Claude Code 远端会话中断失败"));
        assert!(error_text.contains("500 Internal Server Error"));
        assert!(error_text.contains("interrupt failed"));

        server.await.expect("finish interrupt test server");
    }

    /// 验证失败轮次会移除本地句柄、销毁远端句柄，并携带既有 Claude sessionId 重建下一轮。
    #[tokio::test]
    async fn discard_failed_turn_removes_local_handle_and_destroys_remote_handle() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind discard test server");
        let address = listener
            .local_addr()
            .expect("read discard test server address");
        let create_count = Arc::new(AtomicUsize::new(0));
        let destroy_count = Arc::new(AtomicUsize::new(0));
        let server_create_count = create_count.clone();
        let server_destroy_count = destroy_count.clone();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.expect("accept discard request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read discard request");
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
                    let create_index = server_create_count.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        request.contains("\"resume\":\"session-previous\""),
                        "create request must preserve Claude session resume parameter"
                    );
                    let handle_id = if create_index == 0 {
                        "handle-1"
                    } else {
                        "handle-2"
                    };
                    (
                        "201 Created",
                        json!({
                            "threadId": "thread-1",
                            "handleId": handle_id,
                            "sessionId": "session-previous",
                            "reused": false,
                        }),
                    )
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
                    .expect("write discard response");
            }
        });

        let base_url: reqwest::Url = format!("http://{address}")
            .parse()
            .expect("parse discard test url");
        let registry = ClaudeCodeSessionHandleRegistry::with_idle_timeout(Duration::from_secs(60));
        let first = registry
            .create_or_get(
                "thread-1",
                base_url.clone(),
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create first discard test handle");
        assert_eq!(first.handle_id, "handle-1");
        assert!(registry.contains("thread-1").await);

        registry
            .discard_failed_turn("thread-1")
            .await
            .expect("destroy failed-turn handle");
        assert!(!registry.contains("thread-1").await);
        assert_eq!(destroy_count.load(Ordering::SeqCst), 1);

        let second = registry
            .create_or_get(
                "thread-1",
                base_url,
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create replacement discard test handle");
        assert_eq!(second.handle_id, "handle-2");
        assert_eq!(create_count.load(Ordering::SeqCst), 2);

        server.await.expect("finish discard test server");
    }

    /// 验证远端失败轮次句柄销毁失败时，原始响应仍返回且本地句柄已移除。
    #[tokio::test]
    async fn discard_failed_turn_removes_local_handle_when_remote_destroy_fails() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind failed discard test server");
        let address = listener
            .local_addr()
            .expect("read failed discard test server address");
        let create_count = Arc::new(AtomicUsize::new(0));
        let server_create_count = create_count.clone();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener
                    .accept()
                    .await
                    .expect("accept failed discard request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read failed discard request");
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
                    let create_index = server_create_count.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        request.contains("\"resume\":\"session-previous\""),
                        "create request must preserve Claude session resume parameter"
                    );
                    let handle_id = if create_index == 0 {
                        "handle-1"
                    } else {
                        assert!(
                            request.contains("\"replaceExisting\":true"),
                            "replacement create request must include replaceExisting=true"
                        );
                        "handle-2"
                    };
                    (
                        "201 Created",
                        json!({
                            "threadId": "thread-1",
                            "handleId": handle_id,
                            "sessionId": "session-previous",
                            "reused": false,
                        }),
                    )
                } else if request_line.starts_with("DELETE /session-handles/thread-1 ") {
                    (
                        "500 Internal Server Error",
                        json!({ "error": "destroy failed" }),
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
                    .expect("write failed discard response");
            }
        });

        let base_url: reqwest::Url = format!("http://{address}")
            .parse()
            .expect("parse failed discard test url");
        let registry = ClaudeCodeSessionHandleRegistry::with_idle_timeout(Duration::from_secs(60));
        registry
            .create_or_get(
                "thread-1",
                base_url.clone(),
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create failed discard test handle");

        let error = registry
            .discard_failed_turn("thread-1")
            .await
            .expect_err("reject failed remote destroy");
        let error_text = format!("{error:#}");
        assert!(error_text.contains("destroy failed"));
        assert!(!registry.contains("thread-1").await);

        let second = registry
            .create_or_get(
                "thread-1",
                base_url,
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create replacement after failed discard");
        assert_eq!(second.handle_id, "handle-2");
        assert_eq!(create_count.load(Ordering::SeqCst), 2);

        server.await.expect("finish failed discard test server");
    }

    /// 验证替换创建失败时保留恢复标记，后续创建继续请求替换并成功取得新句柄。
    #[tokio::test]
    async fn replacement_create_failure_keeps_remote_handle_replacement_marker() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind replacement failure test server");
        let address = listener
            .local_addr()
            .expect("read replacement failure test server address");
        let create_count = Arc::new(AtomicUsize::new(0));
        let server_create_count = create_count.clone();
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener
                    .accept()
                    .await
                    .expect("accept replacement failure request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read replacement failure request");
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
                    let create_index = server_create_count.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        request.contains("\"resume\":\"session-previous\""),
                        "create request must preserve Claude session resume parameter"
                    );
                    if create_index == 0 {
                        (
                            "201 Created",
                            json!({
                                "threadId": "thread-replacement-failure",
                                "handleId": "handle-1",
                                "sessionId": "session-previous",
                                "reused": false,
                            }),
                        )
                    } else if create_index == 1 {
                        assert!(
                            request.contains("\"replaceExisting\":true"),
                            "failed replacement request must include replaceExisting=true"
                        );
                        (
                            "500 Internal Server Error",
                            json!({ "error": "replacement cleanup failed" }),
                        )
                    } else {
                        assert!(
                            request.contains("\"replaceExisting\":true"),
                            "retry replacement request must include replaceExisting=true"
                        );
                        (
                            "201 Created",
                            json!({
                                "threadId": "thread-replacement-failure",
                                "handleId": "handle-2",
                                "sessionId": "session-previous",
                                "reused": false,
                            }),
                        )
                    }
                } else if request_line
                    .starts_with("DELETE /session-handles/thread-replacement-failure ")
                {
                    (
                        "500 Internal Server Error",
                        json!({ "error": "destroy failed" }),
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
                    .expect("write replacement failure response");
            }
        });

        let base_url: reqwest::Url = format!("http://{address}")
            .parse()
            .expect("parse replacement failure test url");
        let registry = ClaudeCodeSessionHandleRegistry::with_idle_timeout(Duration::from_secs(60));
        let first = registry
            .create_or_get(
                "thread-replacement-failure",
                base_url.clone(),
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create initial replacement failure test handle");
        assert_eq!(first.handle_id, "handle-1");

        let destroy_error = registry
            .discard_failed_turn("thread-replacement-failure")
            .await
            .expect_err("reject failed remote destroy before replacement");
        assert!(format!("{destroy_error:#}").contains("destroy failed"));
        assert!(!registry.contains("thread-replacement-failure").await);

        let replacement_error = registry
            .create_or_get(
                "thread-replacement-failure",
                base_url.clone(),
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect_err("reject failed replacement create");
        assert!(format!("{replacement_error:#}").contains("replacement cleanup failed"));
        assert!(!registry.contains("thread-replacement-failure").await);

        let second = registry
            .create_or_get(
                "thread-replacement-failure",
                base_url,
                None,
                json!({ "resume": "session-previous" }),
            )
            .await
            .expect("create replacement after failed replacement create");
        assert_eq!(second.handle_id, "handle-2");
        assert_eq!(create_count.load(Ordering::SeqCst), 3);

        server.await.expect("finish replacement failure test server");
    }
}
