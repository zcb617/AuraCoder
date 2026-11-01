use std::time::Instant;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{CliLocationKind, McpInvocationContext, McpToolResult};
use crate::{
    computer_control_service::{
        is_known_mcp_computer_tool, mcp_operation_kind, mcp_request_targets_current_process,
        resolve_mcp_target, ComputerControlAuthorization, ComputerControlTool,
    },
    config::app_config::AppConfig,
    state::AppState,
};

/// 当前 CLI MCP 实现使用的固定运行时位置。
#[derive(Debug, Clone)]
pub(crate) struct CliMcpRuntime {
    /// 当前 CLI 的稳定标识，用于查找对应引擎线程绑定。
    pub cli_id: String,
    /// 当前 CLI 位于本机还是 SSH 远端。
    pub location: CliLocationKind,
}

/// 三种 CLI 共用的基础业务实现，包含 MCP 和相同的线程业务流程。
///
/// CLI 专属规则由对应实现类通过统一接口提供；公共线程流程只复用相同的业务步骤。
/// MCP 组合当前应用的数据查询、电脑授权和 CUA 执行依赖，不管理 token、
/// HTTP、CLI 创建或生命周期。Gateway 和具体 CLI 实现分别承担各自边界。
#[derive(Clone)]
pub(crate) struct BaseCliMcp {
    /// 当前应用的数据库、会话绑定和电脑授权依赖。
    state: AppState,
    /// 当前 CLI 的固定身份和运行位置。
    runtime: CliMcpRuntime,
}

impl BaseCliMcp {
    /// 创建绑定当前应用依赖和固定 CLI 运行位置的 MCP 公共实现。
    pub(crate) fn new(state: AppState, runtime: CliMcpRuntime) -> Self {
        Self { state, runtime }
    }

    /// 列出当前 CLI 可用的会话工具和本机 CUA 工具。
    pub(crate) fn list_mcp_tools(&self) -> Result<Vec<Value>, String> {
        log::info!(
            "BaseCliMcp list tools started: cli_id={}, location={:?}",
            self.runtime.cli_id,
            self.runtime.location,
        );
        let mut tools = self.state.auracoder_thread_mcp_service.tool_specs();
        if self.runtime.location == CliLocationKind::Local {
            let computer_tool = ComputerControlTool::new(self.state.computer_control_service.sdk());
            match computer_tool.tool_specs() {
                Ok(computer_tools) => tools.extend(computer_tools),
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp 读取本机 CUA 工具目录失败，保留会话工具；cli_id={}, location={:?}, result_code=tool_catalog_unavailable, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                    );
                }
            }
        }
        log::info!(
            "BaseCliMcp list tools finished: cli_id={}, location={:?}, tool_count={}",
            self.runtime.cli_id,
            self.runtime.location,
            tools.len(),
        );
        Ok(tools)
    }

    /// 按统一 MCP 调用链执行会话查询或本机电脑工具，并映射业务结果。
    pub(crate) async fn call_mcp_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        context: McpInvocationContext,
        call_id: String,
        cancellation: CancellationToken,
    ) -> McpToolResult {
        let started_at = Instant::now();
        let tool_name = tool_name.trim();
        let engine_thread_id = context.engine_thread_id.clone();
        let turn_id = context.turn_id.clone();
        let argument_keys = arguments
            .as_object()
            .map(|value| value.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        log::info!(
            "BaseCliMcp MCP call started: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, argument_keys={:?}",
            self.runtime.cli_id,
            self.runtime.location,
            tool_name,
            call_id,
            engine_thread_id,
            turn_id,
            argument_keys,
        );
        if tool_name.is_empty() {
            log::warn!(
                "BaseCliMcp MCP call rejected: cli_id={}, location={:?}, tool_name=<empty>, call_id={}, engine_thread_id={}, turn_id={}, result_code=invalid_request, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("invalid_request", "MCP 工具名称不能为空");
        }
        if cancellation.is_cancelled() {
            log::info!(
                "BaseCliMcp MCP call rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=request_cancelled, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                tool_name,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("request_cancelled", "工具调用已停止");
        }

        let is_thread_tool = matches!(
            tool_name,
            "get_auracoder_thread_message_count" | "get_auracoder_thread_messages_page"
        );
        if is_thread_tool {
            let service = &self.state.auracoder_thread_mcp_service;
            let source_workspace = match service
                .workspace_for_engine_thread(&self.runtime.cli_id, &context.engine_thread_id)
            {
                Ok(workspace_id) => {
                    log::info!(
                        "BaseCliMcp session tool binding read: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, result_code=ok",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        workspace_id,
                    );
                    workspace_id
                }
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session tool binding read failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace=<unknown>, result_code=tool_not_allowed, is_error=true, duration_ms={}, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        started_at.elapsed().as_millis(),
                    );
                    return McpToolResult::error("tool_not_allowed", error);
                }
            };
            let Some(args) = arguments.as_object() else {
                log::warn!(
                    "BaseCliMcp session tool arguments rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace=<unknown>, result_code=invalid_request, is_error=true, duration_ms={}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                    source_workspace,
                    started_at.elapsed().as_millis(),
                );
                return McpToolResult::error(
                    "invalid_request",
                    "AuraCoder 会话工具 arguments 必须是对象",
                );
            };
            let Some(target_thread_id) = args
                .get("thread_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                log::warn!(
                    "BaseCliMcp session tool arguments rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace=<unknown>, result_code=invalid_request, is_error=true, duration_ms={}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                    source_workspace,
                    started_at.elapsed().as_millis(),
                );
                return McpToolResult::error("invalid_request", "缺少必填参数 thread_id");
            };
            let target_workspace = match service.thread_workspace(target_thread_id).await {
                Ok(Some(workspace_id)) => {
                    log::info!(
                        "BaseCliMcp session target workspace read: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=ok",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        workspace_id,
                    );
                    workspace_id
                }
                Ok(None) => {
                    log::warn!(
                        "BaseCliMcp session target workspace read: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace=<missing>, result_code=tool_not_found, is_error=true, duration_ms={}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        started_at.elapsed().as_millis(),
                    );
                    return McpToolResult::error("tool_not_found", "指定的 AuraCoder 会话不存在");
                }
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session target workspace read failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace=<unknown>, result_code=tool_execution_failed, is_error=true, duration_ms={}, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        started_at.elapsed().as_millis(),
                    );
                    return McpToolResult::error("tool_execution_failed", error);
                }
            };
            if target_workspace != source_workspace {
                log::warn!(
                    "BaseCliMcp session workspace policy rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=tool_not_allowed, is_error=true, duration_ms={}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                    source_workspace,
                    target_workspace,
                    started_at.elapsed().as_millis(),
                );
                return McpToolResult::error(
                    "tool_not_allowed",
                    "AuraCoder 会话工具只允许读取当前项目的会话",
                );
            }

            let target_engine = match service.thread_engine(target_thread_id).await {
                Ok(engine_id) => engine_id,
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session engine precheck failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, target_thread_id={}, result_code=tool_execution_failed, is_error=false, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        target_workspace,
                        target_thread_id,
                    );
                    None
                }
            };
            let local_message_count = match service.thread_message_count(target_thread_id).await {
                Ok(value) => match value.get("message_count").and_then(Value::as_u64) {
                    Some(message_count) => Some(message_count),
                    None => {
                        log::warn!(
                            "BaseCliMcp session message count precheck returned invalid data: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, target_thread_id={}, result_code=tool_execution_failed, is_error=false, returned_value={value}",
                            self.runtime.cli_id,
                            self.runtime.location,
                            tool_name,
                            call_id,
                            engine_thread_id,
                            turn_id,
                            source_workspace,
                            target_workspace,
                            target_thread_id,
                        );
                        None
                    }
                },
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session message count precheck failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, target_thread_id={}, result_code=tool_execution_failed, is_error=false, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        target_workspace,
                        target_thread_id,
                    );
                    None
                }
            };
            if target_engine.as_deref().is_some_and(|engine_id| {
                local_message_count
                    .is_some_and(|message_count| should_sync_cli_thread(engine_id, message_count))
            }) {
                if let Err(error) = async {
                    let cli = crate::services::thread_service::ThreadService::new(self.state.clone()).cli_for_thread(target_thread_id).await?;
                    cli.sync_thread_record(target_thread_id.to_string()).await
                }.await
                {
                    log::warn!(
                        "BaseCliMcp on-demand CLI thread sync failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, target_thread_id={}, result_code=thread_sync_failed, is_error=false, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        target_thread_id,
                    );
                }
            }

            let query = match tool_name {
                "get_auracoder_thread_message_count" => {
                    service.thread_message_count(target_thread_id).await
                }
                "get_auracoder_thread_messages_page" => {
                    let Some(page) = args.get("page").and_then(Value::as_u64) else {
                        log::warn!(
                            "BaseCliMcp session tool arguments rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=invalid_request, is_error=true, duration_ms={}",
                            self.runtime.cli_id,
                            self.runtime.location,
                            tool_name,
                            call_id,
                            engine_thread_id,
                            turn_id,
                            source_workspace,
                            target_workspace,
                            started_at.elapsed().as_millis(),
                        );
                        return McpToolResult::error("invalid_request", "缺少必填参数 page");
                    };
                    let Some(page_size) = args.get("page_size").and_then(Value::as_u64) else {
                        log::warn!(
                            "BaseCliMcp session tool arguments rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=invalid_request, is_error=true, duration_ms={}",
                            self.runtime.cli_id,
                            self.runtime.location,
                            tool_name,
                            call_id,
                            engine_thread_id,
                            turn_id,
                            source_workspace,
                            target_workspace,
                            started_at.elapsed().as_millis(),
                        );
                        return McpToolResult::error("invalid_request", "缺少必填参数 page_size");
                    };
                    service
                        .thread_messages_page(target_thread_id, page, page_size)
                        .await
                }
                _ => unreachable!("thread tool names are checked above"),
            };
            return match query {
                Ok(value) => {
                    log::info!(
                        "BaseCliMcp session query returned: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=ok, is_error=false, duration_ms={}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        target_workspace,
                        started_at.elapsed().as_millis(),
                    );
                    McpToolResult::success(value)
                }
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session query failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=tool_execution_failed, is_error=true, duration_ms={}, 原始错误：{error}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                        target_workspace,
                        started_at.elapsed().as_millis(),
                    );
                    McpToolResult::error("tool_execution_failed", error)
                }
            };
        }

        if self.runtime.location == CliLocationKind::Ssh {
            // SSH CLI 只允许会话工具，电脑工具不得进入 CUA 执行路径。
            if is_known_mcp_computer_tool(tool_name) {
                log::warn!(
                    "BaseCliMcp SSH computer tool rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_allowed, is_error=true, duration_ms={}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                    started_at.elapsed().as_millis(),
                );
                return McpToolResult::error("tool_not_allowed", "SSH 远端不允许电脑操作工具");
            }
            log::warn!(
                "BaseCliMcp SSH unknown tool rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_found, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                tool_name,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("tool_not_found", "未知的 MCP 工具");
        }

        let computer_control_enabled = match AppConfig::load_or_create() {
            Ok(config) => config.computer_control.enabled,
            Err(error) => {
                log::error!(
                    "BaseCliMcp computer control setting read failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_allowed, 原始错误：{error}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                );
                false
            }
        };
        if !computer_control_enabled {
            log::warn!(
                "BaseCliMcp computer tool rejected by feature switch: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_allowed, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                tool_name,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("tool_not_allowed", "AuraCoder 的电脑操作能力开关未开启");
        }

        let computer_tool = ComputerControlTool::new(self.state.computer_control_service.sdk());
        if !computer_tool.sdk_ready() {
            log::warn!(
                "BaseCliMcp CUA SDK unavailable: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_found, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                tool_name,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("tool_not_found", "电脑操作工具当前不可用");
        }
        let computer_specs = match computer_tool.tool_specs() {
            Ok(specs) => specs,
            Err(error) => {
                log::warn!(
                    "BaseCliMcp CUA tool catalog read failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_found, is_error=true, duration_ms={}, 原始错误：{error}",
                    self.runtime.cli_id,
                    self.runtime.location,
                    tool_name,
                    call_id,
                    engine_thread_id,
                    turn_id,
                    started_at.elapsed().as_millis(),
                );
                return McpToolResult::error("tool_not_found", "电脑操作工具当前不可用");
            }
        };
        if !computer_specs
            .iter()
            .any(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))
        {
            log::warn!(
                "BaseCliMcp unknown computer tool rejected: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_found, is_error=true, duration_ms={}",
                self.runtime.cli_id,
                self.runtime.location,
                tool_name,
                call_id,
                engine_thread_id,
                turn_id,
                started_at.elapsed().as_millis(),
            );
            return McpToolResult::error("tool_not_found", "未知的 MCP 工具");
        }

        let arguments = match arguments {
            Value::Object(_) => arguments,
            Value::String(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(Value::Object(value)) => Value::Object(value),
                Ok(_) => {
                    return McpToolResult::error(
                        "invalid_request",
                        "电脑操作 arguments 必须是 JSON 对象",
                    )
                }
                Err(error) => {
                    log::warn!("BaseCliMcp 解析电脑工具 arguments 失败，原始错误：{error}");
                    return McpToolResult::error(
                        "invalid_request",
                        "电脑操作 arguments 不是有效 JSON",
                    );
                }
            },
            _ => {
                return McpToolResult::error(
                    "invalid_request",
                    "电脑操作 arguments 必须是 JSON 对象",
                )
            }
        };
        let Some(_argument_object) = arguments.as_object() else {
            return McpToolResult::error("invalid_request", "电脑操作 arguments 必须是 JSON 对象");
        };
        if mcp_request_targets_current_process(&arguments) {
            return McpToolResult::error(
                "tool_not_allowed",
                "AuraCoder 不允许把自身窗口作为电脑操作目标",
            );
        }
        let target = match resolve_mcp_target(tool_name, &arguments) {
            Ok(target) => target,
            Err(error) => {
                log::warn!("BaseCliMcp 解析电脑工具目标失败，原始错误：{error}");
                return McpToolResult::error("tool_not_allowed", error);
            }
        };
        let operation = mcp_operation_kind(tool_name);
        let computer_service = &self.state.computer_control_service;
        if !computer_service.has_persistent_authorization(&target.key) {
            let authorization = ComputerControlAuthorization {
                request_id: uuid::Uuid::new_v4().to_string(),
                agent: self.runtime.cli_id.clone(),
                tool: tool_name.to_string(),
                call_id,
                application: target.display,
                operation: operation.to_string(),
                scope: target.scope.to_string(),
                thread_id: context.engine_thread_id,
                turn_id: context.turn_id,
            };
            if let Err(error) = computer_service
                .request_authorization(authorization, target.key, cancellation.clone())
                .await
            {
                log::warn!("BaseCliMcp 电脑操作授权失败，原始错误：{error}");
                return McpToolResult::error("authorization_required", error);
            }
        }
        match computer_tool.execute(tool_name, arguments) {
            Ok(value) => McpToolResult::success(value),
            Err(error) => {
                log::warn!("BaseCliMcp CUA 工具执行失败，原始错误：{error}");
                McpToolResult::error("tool_execution_failed", error)
            }
        }
    }
}

/// 判断会话是否满足三种 CLI 空消息按需同步条件。
fn should_sync_cli_thread(engine_id: &str, message_count: u64) -> bool {
    matches!(engine_id, "codex" | "opencode" | "claude") && message_count == 0
}

#[cfg(test)]
mod tests {
    use super::should_sync_cli_thread;

    /// 验证 Codex 会话无本地消息时需要执行按需同步。
    #[test]
    fn codex_empty_thread_requires_sync() {
        assert!(should_sync_cli_thread("codex", 0));
    }

    /// 验证已有本地消息的 Codex 会话不会执行按需同步。
    #[test]
    fn codex_thread_with_messages_skips_sync() {
        assert!(!should_sync_cli_thread("codex", 1));
    }

    /// 验证 OpenCode 和 Claude Code 空消息会话也执行按需同步。
    #[test]
    fn supported_cli_threads_require_sync_when_empty() {
        assert!(should_sync_cli_thread("opencode", 0));
        assert!(should_sync_cli_thread("claude", 0));
    }

    /// 验证未知引擎和已有消息的会话不会执行按需同步。
    #[test]
    fn unsupported_or_non_empty_thread_skips_sync() {
        assert!(!should_sync_cli_thread("unknown", 0));
        assert!(!should_sync_cli_thread("claude", 1));
    }
}

pub(crate) mod thread_business {
    use super::*;
    use anyhow::Context;
    use crate::engines::ModelInfo;
    use crate::{db, cli_tools::{CliTool, CliExecutionContext, CliLocationKind, CliSessionNotFoundError}, models::{ThreadDto, ThreadUpdateDto, PermissionComponentJson}, services::thread_service::ThreadService};
    use serde_json::json;

    async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
    where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || operation(&db))
            .await
            .map_err(|error| error.to_string())?
            .map_err(err_to_string)
    }
    pub(crate) fn err_to_string(error: impl std::fmt::Display) -> String {
        format!("{error:#}")
    }
    pub(crate) fn validate_model_for_engine_from_catalog(
        engine_id: &str,
        normalized_model_id: &str,
        models: Option<&[ModelInfo]>,
    ) -> Result<String, String> {
        let Some(models) = models else {
            return Ok(normalized_model_id.to_string());
        };

        if models.iter().any(|model| model.id == normalized_model_id) {
            return Ok(normalized_model_id.to_string());
        }

        let available = models
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
        "model `{normalized_model_id}` is not supported by engine `{engine_id}`. available models: {available}"
        ))
    }
    pub(crate) fn validate_reasoning_effort_from_catalog(
        model_id: &str,
        requested_effort: &str,
        models: Option<&[ModelInfo]>,
    ) -> Result<String, String> {
        const KNOWN_REASONING_EFFORTS: &[&str] =
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
        if !KNOWN_REASONING_EFFORTS.contains(&requested_effort) {
            return Err(format!(
                    "invalid reasoning effort `{requested_effort}`. expected one of: {}",
                    KNOWN_REASONING_EFFORTS.join(", ")
            ));
        }

        let Some(model) = models.and_then(|items| items.iter().find(|model| model.id == model_id))
        else {
            return Ok(requested_effort.to_string());
        };

        if let Some(option) = model
            .supported_reasoning_efforts
            .iter()
            .find(|option| option.reasoning_effort == requested_effort)
        {
            return Ok(option.reasoning_effort.clone());
        }

        let supported = model
            .supported_reasoning_efforts
            .iter()
            .map(|option| option.reasoning_effort.clone())
            .collect::<Vec<_>>()
            .join(", ");

        Err(format!(
                "reasoning effort `{requested_effort}` is not supported by model `{}`. supported values: {}",
                model.id, supported
        ))
    }
    impl BaseCliMcp {
    /// 查询当前 CLI 的模型目录并校验；目录不可用策略沿用原入口，禁止跨 CLI 回退。
        pub(crate) async fn validate_thread_model(&self, cli: &dyn CliTool, _engine_id: &str, workspace_id: Option<&str>, model_id: &str) -> anyhow::Result<String> {
            let model_id = model_id.trim();
            anyhow::ensure!(!model_id.is_empty(), "model id cannot be empty");
            let models = match cli.execution_context(workspace_id).await {
                Ok(context) => {
                    if context.location_kind == CliLocationKind::Ssh {
                        Some(cli.models_for_validation(&context, model_id).await?)
                    } else {
                        cli.models_for_validation(&context, model_id).await.ok()
                    }
                }
                Err(error) if workspace_id.is_some() => return Err(error),
                Err(_) => None,
            };
            validate_model_for_engine_from_catalog(_engine_id, model_id, models.as_deref()).map_err(anyhow::Error::msg)
        }

        pub(crate) async fn validate_thread_reasoning(&self, cli: &dyn CliTool, _engine_id: &str, model_id: &str, reasoning_effort: &str) -> anyhow::Result<String> {
            let models = match cli.execution_context(None).await {
                Ok(context) => cli.models_for_validation(&context, model_id).await.ok(),
                Err(_) => None,
            };
            validate_reasoning_effort_from_catalog(model_id, reasoning_effort, models.as_deref()).map_err(anyhow::Error::msg)
        }

        pub(crate) async fn create_thread_record(&self, cli: &dyn CliTool, engine_id: &str, workspace_id: String, model_id: String, title: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> anyhow::Result<ThreadDto> {
            let preset = tokio::task::spawn_blocking(|| AppConfig::load_or_create().map(|config| config.default_autonomy_preset().map(ToOwned::to_owned))).await.context("读取新线程默认设置任务失败")??;
            self.create_thread_record_inner(cli, engine_id, workspace_id, model_id, title, reasoning_effort, service_tier, preset).await.map_err(anyhow::Error::msg)
        }
        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn create_thread_record_inner(&self, cli: &dyn CliTool, engine_id: &str, workspace_id: String, model_id: String, title: String, reasoning_effort: Option<String>, service_tier: Option<String>, initial_autonomy_preset: Option<String>) -> Result<ThreadDto, String> {
            let state = &self.state;
            let engine_id = engine_id.to_owned();

            let normalized_service_tier = cli.normalize_thread_service_tier(service_tier).map_err(err_to_string)?;
            let normalized_model_id = model_id.trim();
            if normalized_model_id.is_empty() {
                return Err("model id cannot be empty".to_string());
            }
            let target_workspace = run_db(state.db.clone(), {
                    let workspace_id = workspace_id.clone();
                    move |db| {
                        db::workspaces::find_workspace_by_id(db, &workspace_id)?
                            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                    }
            })
                .await?;
            let context = cli.execution_context(Some(&workspace_id)).await.map_err(err_to_string)?;
            let validation_models = if context.location_kind == CliLocationKind::Ssh {
                Some(cli.models_for_validation(&context, normalized_model_id).await.map_err(err_to_string)?)
            } else {
                cli.models_for_validation(&context, normalized_model_id).await.ok()
            };
            let effective_model_id = validate_model_for_engine_from_catalog(
                &engine_id,
                normalized_model_id,
                validation_models.as_deref(),
            )?;
            let normalized_reasoning_effort = reasoning_effort
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase);
            let validated_reasoning_effort =
            if let Some(requested_effort) = normalized_reasoning_effort.as_deref() {
                Some(validate_reasoning_effort_from_catalog(
                        &effective_model_id,
                        requested_effort,
                        validation_models.as_deref(),
                    )?)
            } else {
                None
            };
            let mut metadata = serde_json::Map::new();
            if let Some(value) = normalized_service_tier {
                metadata.insert("serviceTier".to_string(), json!(value));
            }

    // 根据项目统一信任设置和新线程默认预设构造初始权限对象 A。
            let mut permission_mode = PermissionComponentJson::default();
            permission_mode.trust = vec![target_workspace.trust_level.as_str().to_owned()];
            if let Some(preset) = initial_autonomy_preset {
                permission_mode.autonomy_preset = vec![preset.clone()];
                permission_mode.default_for_new_threads = vec![preset];
            }

            let metadata = (!metadata.is_empty()).then_some(Value::Object(metadata));

            let created = run_db(state.db.clone(), move |db| {
                    let created =
                    db::threads::create_thread(db, &workspace_id, &engine_id, &effective_model_id, &title)?;
                    if let Some(metadata) = metadata.as_ref() {
                        db::threads::update_engine_metadata(db, &created.id, metadata)?;
                    }
                    db::threads::update_thread_runtime_selection(
                        db,
                        &created.id,
                        &engine_id,
                        &effective_model_id,
                        None,
                        None,
                        validated_reasoning_effort.as_deref(),
                        None,
                    )?;
                    db::threads::get_thread(db, &created.id)?
                        .ok_or_else(|| anyhow::anyhow!("thread not found after insert: {}", created.id))
            })
                .await?;

            ThreadService::new(state.clone())
                .save_permissions(&created.id, permission_mode)
                .await
                .map_err(err_to_string)

        }
        pub(crate) async fn reconfigure_thread_record(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String, model_id: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> anyhow::Result<ThreadDto> {
            let state = &self.state;
            let engine_id = engine_id.to_owned();
            let result: Result<ThreadDto, String> = async {

                let db = state.db.clone();
                let (thread, _target_workspace) = run_db(db.clone(), {
                        let thread_id = thread_id.clone();
                        move |db| {
                            let thread = db::threads::get_thread(db, &thread_id)?
                                .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
                            let workspace = db::workspaces::find_workspace_by_id(db, &thread.workspace_id)?
                                .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", thread.workspace_id))?;
                            Ok((thread, workspace))
                        }
                })
                    .await?;

                if thread.engine_thread_id.is_some() || thread.message_count != 0 {
                    return Err("thread has already started and its CLI tool cannot be changed".to_string());
                }

                let normalized_service_tier = cli.normalize_thread_service_tier(service_tier).map_err(err_to_string)?;
                let normalized_model_id = model_id.trim();
                if normalized_model_id.is_empty() {
                    return Err("model id cannot be empty".to_string());
                }
                let context = cli.execution_context(Some(&thread.workspace_id)).await.map_err(err_to_string)?;
                let validation_models = if context.location_kind == CliLocationKind::Ssh {
                    Some(cli.models_for_validation(&context, normalized_model_id).await.map_err(err_to_string)?)
                } else {
                    cli.models_for_validation(&context, normalized_model_id).await.ok()
                };
                let effective_model_id = validate_model_for_engine_from_catalog(
                    &engine_id,
                    normalized_model_id,
                    validation_models.as_deref(),
                )?;
                let normalized_reasoning_effort = reasoning_effort
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_lowercase);
                let validated_reasoning_effort =
                if let Some(requested_effort) = normalized_reasoning_effort.as_deref() {
                    Some(validate_reasoning_effort_from_catalog(
                            &effective_model_id,
                            requested_effort,
                            validation_models.as_deref(),
                        )?)
                } else {
                    None
                };

                let mut metadata = serde_json::Map::new();
                if let Some(value) = normalized_service_tier {
                    metadata.insert("serviceTier".to_string(), json!(value));
                }

                if let Some(existing_metadata) = thread.engine_metadata.as_ref().and_then(Value::as_object) {
                    for key in ["manualTitle", "manualTitleUpdatedAt"] {
                        if let Some(value) = existing_metadata.get(key) {
                            metadata.insert(key.to_string(), value.clone());
                        }
                    }
                }
                let metadata = (!metadata.is_empty()).then_some(Value::Object(metadata));

                let reconfigure_engine_id = engine_id.clone();
                let reconfigure_model_id = effective_model_id.clone();
                let first_engine_id = reconfigure_engine_id.clone();
                let first_model_id = reconfigure_model_id.clone();
                let updated = run_db(db.clone(), move |db| {
                        db::threads::reconfigure_unstarted_thread_runtime(
                            db,
                            &thread_id,
                            &first_engine_id,
                            &first_model_id,
                            metadata.as_ref(),
                        )
                })
                    .await?;
                run_db(db, move |db| {
                        db::threads::update_thread_runtime_selection(
                            db,
                            &updated.id,
                            &reconfigure_engine_id,
                            &reconfigure_model_id,
                            None,
                            None,
                            validated_reasoning_effort.as_deref(),
                            None,
                        )
                })
                    .await
            }.await;
            result.map_err(anyhow::Error::msg)
        }
        pub(crate) async fn set_thread_reasoning(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String, reasoning_effort: Option<String>, model_id: Option<String>) -> anyhow::Result<()> {
            let thread = ThreadService::new(self.state.clone()).get_thread(&thread_id).await?;
            anyhow::ensure!(thread.engine_id == engine_id, "线程所属 CLI 与当前实现不一致");
            let model_id = match model_id.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
                Some(model) if model != thread.model_id => cli.validate_thread_model(Some(&thread.workspace_id), model).await?,
                _ => thread.model_id.clone(),
            };
            let effort = reasoning_effort.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_lowercase);
            let effort = if let Some(value) = effort {
                let context = cli.execution_context(Some(&thread.workspace_id)).await?;
                let models = if context.location_kind == CliLocationKind::Ssh { Some(cli.models_for_validation(&context, &model_id).await?) } else { cli.models_for_validation(&context, &model_id).await.ok() };
                Some(validate_reasoning_effort_from_catalog(&model_id, &value, models.as_deref()).map_err(anyhow::Error::msg)?)
            } else { None };
            let db = self.state.db.clone();
            tokio::task::spawn_blocking(move || db::threads::update_thread(&db, &ThreadUpdateDto { id: thread_id, reasoning_effort: Some(effort.unwrap_or_default()), ..Default::default() })).await.context("保存线程思考强度任务失败")??;
            Ok(())
        }

        pub(crate) async fn set_thread_selected_model(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String, model_id: String) -> anyhow::Result<ThreadDto> {
            let thread = ThreadService::new(self.state.clone()).get_thread(&thread_id).await?;
            anyhow::ensure!(thread.engine_id == engine_id, "线程所属 CLI 与当前实现不一致");
            let context = cli.execution_context(Some(&thread.workspace_id)).await?;
            anyhow::ensure!(context.location_kind == CliLocationKind::Ssh, "only SSH remote project threads can persist this model selection");
            let model_id = if model_id.trim() == thread.model_id { thread.model_id } else { cli.validate_thread_model(Some(&thread.workspace_id), &model_id).await? };
            let db = self.state.db.clone();
            tokio::task::spawn_blocking(move || db::threads::update_thread(&db, &ThreadUpdateDto { id: thread_id, model_id: Some(model_id), ..Default::default() })).await.context("保存 SSH 线程模型选择任务失败")?
        }

        pub(crate) async fn delete_thread_record(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String) -> anyhow::Result<()> {
            self.state.turns.cancel(&thread_id).await;
            let result = async {
                let thread = ThreadService::new(self.state.clone()).get_thread(&thread_id).await?;
                anyhow::ensure!(thread.engine_id == engine_id, "线程所属 CLI 与当前实现不一致");
                let context = cli.execution_context(Some(&thread.workspace_id)).await?;
                if let Err(error) = cli.interrupt(&context, &thread, thread.engine_thread_id.as_deref().unwrap_or("default")).await { log::warn!("删除前中断线程 {} 失败: {error:#}", thread.id); }
                if let Some(id) = thread.engine_thread_id.as_deref() { cli.forget_session(&context, &thread, id).await?; }
                let db = self.state.db.clone();
                let id = thread_id.clone();
                tokio::task::spawn_blocking(move || db::threads::delete_thread(&db, &id)).await.context("删除线程数据库任务失败")?
            }.await;
            self.state.turns.finish(&thread_id).await;
            result
        }

        pub(crate) async fn archive_thread_record(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String) -> anyhow::Result<()> {
            self.state.turns.cancel(&thread_id).await;
            let result = async {
                let thread = ThreadService::new(self.state.clone()).get_thread(&thread_id).await?;
                anyhow::ensure!(thread.engine_id == engine_id, "线程所属 CLI 与当前实现不一致");
                let context = cli.execution_context(Some(&thread.workspace_id)).await?;
                if let Err(error) = cli.interrupt(&context, &thread, thread.engine_thread_id.as_deref().unwrap_or("default")).await { log::warn!("归档前中断线程 {} 失败: {error:#}", thread.id); }
                if let Some(id) = thread.engine_thread_id.as_deref() { cli.archive_thread(&context, &thread, id).await?; }
                let db = self.state.db.clone();
                let id = thread_id.clone();
                tokio::task::spawn_blocking(move || db::threads::archive_thread(&db, &id)).await.context("归档线程数据库任务失败")?
            }.await;
            self.state.turns.finish(&thread_id).await;
            result
        }

        pub(crate) async fn restore_thread_record(&self, cli: &dyn CliTool, engine_id: &str, thread_id: String) -> anyhow::Result<ThreadDto> {
            let thread = ThreadService::new(self.state.clone()).get_thread(&thread_id).await?;
            anyhow::ensure!(thread.engine_id == engine_id, "线程所属 CLI 与当前实现不一致");
            let context = cli.execution_context(Some(&thread.workspace_id)).await?;
            if let Some(id) = thread.engine_thread_id.as_deref() {
                if let Err(error) = cli.read_session(&context, id).await {
                    if error.downcast_ref::<CliSessionNotFoundError>().is_some() {
                        anyhow::bail!(if context.location_kind == CliLocationKind::Ssh { "该会话在远端已不存在，无法恢复。" } else { "该会话在本机 CLI 中已不存在，无法恢复。" });
                    }
                    return Err(error.context("恢复前读取 CLI 会话失败"));
                }
                cli.unarchive_thread(&context, &thread, id).await?;
            }
            let db = self.state.db.clone();
            tokio::task::spawn_blocking(move || db::threads::restore_thread(&db, &thread_id)).await.context("恢复线程数据库任务失败")?
        }
    }
}
