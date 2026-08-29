use std::time::Instant;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{CliLocationKind, McpInvocationContext, McpToolResult};
use crate::{
    computer_control_service::{
        is_known_mcp_computer_tool, mcp_operation_kind, mcp_request_targets_current_process,
        resolve_mcp_target,
        ComputerControlAuthorization, ComputerControlTool,
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

/// 三种 CLI 共用的 MCP 业务实现。
///
/// 该对象只组合当前应用的数据查询、电脑授权和 CUA 执行依赖，不管理 token、
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
            let computer_tool =
                ComputerControlTool::new(self.state.computer_control_service.sdk());
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
                    return McpToolResult::error("tool_not_found", "指定的 AuraCoder 会话不存在")
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
        if !computer_specs.iter().any(|tool| {
            tool.get("name").and_then(Value::as_str) == Some(tool_name)
        }) {
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
                    return McpToolResult::error("invalid_request", "电脑操作 arguments 不是有效 JSON");
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
            return McpToolResult::error("tool_not_allowed", "AuraCoder 不允许把自身窗口作为电脑操作目标");
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
