use std::time::Instant;

use anyhow::Context;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{CliExecutionContext, CliLocationKind, McpInvocationContext, McpToolResult};
use crate::services::app_config_service::AppConfigService;
use crate::{
    computer_control_service::{
        is_known_mcp_computer_tool, mcp_operation_kind, mcp_request_targets_current_process,
        resolve_mcp_target, ComputerControlAuthorization, ComputerControlTool,
    },
    // config::app_config::AppConfig,
    models::{EngineInfoDto, ThreadDto},
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

    /// 根据数据库线程归属读取目标 CLI 的执行上下文和线程记录。
    pub(crate) async fn context_for_thread(
        &self,
        cli_id: &str,
        thread_id: &str,
    ) -> anyhow::Result<(CliExecutionContext, ThreadDto)> {
        let database = self.state.db.clone();
        let cli_id = cli_id.to_string();
        let thread_id = thread_id.to_string();
        let log_thread_id = thread_id.clone();

        // 在线程池中校验线程归属并读取对应工作区，再返回拥有所有权的上下文对象。
        let result = tokio::task::spawn_blocking(move || {
            load_cli_thread_context(&database, &cli_id, &thread_id)
        })
        .await
        .context("读取 CLI 线程上下文的数据库任务失败")
        .and_then(|result| result);
        if let Err(error) = &result {
            log::error!(
                "读取 CLI 线程上下文失败: cli_id={} thread_id={} error={error:#}",
                self.runtime.cli_id,
                log_thread_id
            );
        }
        result
    }

    /// 汇总当前 CLI 的已就绪状态和模型信息，模型读取失败时保留空目录结果。
    pub(crate) async fn active_engine_info(
        &self,
        cli: &dyn crate::cli_tools::CliTool,
        context: &CliExecutionContext,
    ) -> anyhow::Result<Option<EngineInfoDto>> {
        // 只从当前统一 CLI 接口查询就绪状态和引擎目录。
        if !cli.is_service_ready(context).await? {
            return Ok(None);
        }
        match cli.get_engine_info(context).await {
            Ok(engine_info) => Ok(Some(engine_info)),
            Err(error) => {
                log::warn!(
                    "读取已就绪 CLI 的模型目录失败，保留空模型结果: cli_id={} error={error:#}",
                    cli.id()
                );
                Ok(Some(EngineInfoDto {
                    id: cli.id().to_string(),
                    name: cli.name().to_string(),
                    models: Vec::new(),
                    capabilities: crate::engines::map_engine_capabilities(cli.capabilities()),
                }))
            }
        }
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
/*
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
*/

            // 用 Gateway 提供的助手消息 ID 从数据库确认本次 MCP 调用来源。
            let database = self.state.db.clone();
            let cli_id = self.runtime.cli_id.clone();
            let assistant_message_id = context.turn_id.clone();
            let source_result = tokio::task::spawn_blocking(move || {
                load_mcp_source_workspace(&database, &cli_id, &assistant_message_id)
            })
            .await
            .context("读取 MCP 调用来源的数据库任务失败")
            .and_then(|result| result);
            let source_workspace = match source_result {
                Ok(Some(workspace_id)) => {
                    log::info!(
                        "BaseCliMcp session source confirmed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, result_code=ok",
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
                Ok(None) => {
                    log::warn!(
                        "BaseCliMcp session source could not be confirmed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_not_allowed, is_error=true, duration_ms={}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        started_at.elapsed().as_millis(),
                    );
                    return McpToolResult::error(
                        "tool_not_allowed",
                        "无法确认当前 CLI 的会话来源，请重新发起本轮对话",
                    );
                }
                Err(error) => {
                    log::error!(
                        "BaseCliMcp session source database lookup failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, result_code=tool_execution_failed, error={error:#}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                    );
                    return McpToolResult::error(
                        "tool_execution_failed",
                        "读取当前会话来源失败，请重试",
                    );
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
                    log::error!(
                        "BaseCliMcp session target workspace read failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace=<unknown>, result_code=tool_execution_failed, 原始错误：{error:#}",
                        self.runtime.cli_id,
                        self.runtime.location,
                        tool_name,
                        call_id,
                        engine_thread_id,
                        turn_id,
                        source_workspace,
                    );
                    return McpToolResult::error(
                        "tool_execution_failed",
                        "读取目标会话信息失败，请重试",
                    );
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
                        "BaseCliMcp session engine precheck failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, target_thread_id={}, result_code=tool_execution_failed, is_error=false, 原始错误：{error:#}",
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
/*
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
*/

            // 读取 typed 消息数量供现有空消息按需同步判定复用。
            let local_message_count = match service.thread_message_count(target_thread_id).await {
                Ok(message_count) => Some(message_count),
                Err(error) => {
                    log::warn!(
                        "BaseCliMcp session message count precheck failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, target_thread_id={}, result_code=tool_execution_failed, is_error=false, 原始错误：{error:#}",
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

/*
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
*/

            // 保留原工具 JSON 字段契约，并映射 typed 查询结果。
            let query = match tool_name {
                "get_auracoder_thread_message_count" => service
                    .thread_message_count(target_thread_id)
                    .await
                    .map(|message_count| {
                        serde_json::json!({
                            "thread_id": target_thread_id,
                            "message_count": message_count
                        })
                    }),
                "get_auracoder_thread_messages_page" => {
                    let Some(page) = args
                        .get("page")
                        .and_then(Value::as_u64)
                        .filter(|value| *value > 0)
                    else {
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
                        return McpToolResult::error(
                            "invalid_request",
                            "page 必须是大于零的整数",
                        );
                    };
                    let Some(page_size) = args
                        .get("page_size")
                        .and_then(Value::as_u64)
                        .filter(|value| *value > 0)
                    else {
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
                        return McpToolResult::error(
                            "invalid_request",
                            "page_size 必须是大于零的整数",
                        );
                    };
                    service
                        .thread_messages_page(target_thread_id, page, page_size)
                        .await
                        .map(|messages| {
                            serde_json::json!({
                                "thread_id": target_thread_id,
                                "page": page,
                                "page_size": page_size,
                                "messages": messages
                            })
                        })
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
                    log::error!(
                        "BaseCliMcp session query failed: cli_id={}, location={:?}, tool_name={}, call_id={}, engine_thread_id={}, turn_id={}, source_workspace={}, target_workspace={}, result_code=tool_execution_failed, duration_ms={}, 原始错误：{error:#}",
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
                    McpToolResult::error(
                        "tool_execution_failed",
                        "读取 AuraCoder 会话消息失败，请重试",
                    )
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

        // let computer_control_enabled = match AppConfig::load_or_create() {
        let computer_control_enabled = match AppConfigService::load_or_create() {
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

/// 根据可信助手消息 ID 确认 MCP 调用来源的工作区和 CLI 归属。
fn load_mcp_source_workspace(
    database: &crate::db::Database,
    cli_id: &str,
    assistant_message_id: &str,
) -> anyhow::Result<Option<String>> {
    // 校验用于归属查询的 CLI 标识和可信助手消息 ID。
    let cli_id = cli_id.trim();
    if cli_id.is_empty() {
        log::warn!("MCP 来源校验拒绝空 CLI 标识");
        return Ok(None);
    }
    let assistant_message_id = assistant_message_id.trim();
    if assistant_message_id.is_empty() {
        log::warn!("MCP 来源校验拒绝空助手消息 ID: cli_id={cli_id}");
        return Ok(None);
    }

    // 读取助手消息并确认消息角色后，再按其数据库线程校验引擎归属。
    let Some(message) = crate::db::messages::get_message(database, assistant_message_id)
        .context("读取 MCP 来源助手消息失败")?
    else {
        log::warn!(
            "MCP 来源助手消息不存在: cli_id={} assistant_message_id={}",
            cli_id,
            assistant_message_id
        );
        return Ok(None);
    };
    if message.role != "assistant" {
        log::warn!(
            "MCP 来源消息不是助手消息: cli_id={} assistant_message_id={} role={}",
            cli_id,
            assistant_message_id,
            message.role
        );
        return Ok(None);
    }

    let Some(thread) = crate::db::threads::get_thread(database, &message.thread_id)
        .context("读取 MCP 来源会话失败")?
    else {
        log::warn!(
            "MCP 来源助手消息所属会话不存在: cli_id={} assistant_message_id={} thread_id={}",
            cli_id,
            assistant_message_id,
            message.thread_id
        );
        return Ok(None);
    };
    if thread.engine_id != cli_id {
        log::warn!(
            "MCP 来源会话引擎不匹配: cli_id={} assistant_message_id={} thread_id={} thread_engine_id={}",
            cli_id,
            assistant_message_id,
            thread.id,
            thread.engine_id
        );
        return Ok(None);
    }

    Ok(Some(thread.workspace_id))
}

/// 校验当前 CLI 对线程的所有权并读取其工作区执行上下文。
fn load_cli_thread_context(
    database: &crate::db::Database,
    cli_id: &str,
    raw_thread_id: &str,
) -> anyhow::Result<(CliExecutionContext, ThreadDto)> {
    // 拒绝空线程标识，并确认数据库线程属于调用方当前 CLI。
    let thread_id = raw_thread_id.trim();
    anyhow::ensure!(!thread_id.is_empty(), "线程标识不能为空");
    let thread = crate::db::threads::get_thread(database, thread_id)?
        .ok_or_else(|| anyhow::anyhow!("线程不存在: {thread_id}"))?;
    anyhow::ensure!(thread.engine_id == cli_id, "线程不属于当前 CLI");

    // 根据线程保存的工作区标识构造执行位置，保持既有工作区校验规则。
    let workspace = crate::db::workspaces::find_workspace_by_id(database, &thread.workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("线程所属工作区不存在: {}", thread.workspace_id))?;
    let context = CliExecutionContext::from_workspace(&workspace)?;
    Ok((context, thread))
}

/// 判断会话是否满足三种 CLI 空消息按需同步条件。
fn should_sync_cli_thread(engine_id: &str, message_count: u64) -> bool {
    matches!(engine_id, "codex" | "opencode" | "claude") && message_count == 0
}

/// 校验并规范化定时任务中交给当前 CLI 的公共运行字段。
fn normalize_scheduled_runtime_fields(
    cli_id: &str,
    mut runtime: crate::models::ScheduledRuntimeConfig,
) -> anyhow::Result<crate::models::ScheduledRuntimeConfig> {
    // 清理路由和模型字段，并确保任务配置属于当前 CLI。
    runtime.engine_id = runtime.engine_id.trim().to_string();
    runtime.model_id = runtime.model_id.trim().to_string();
    anyhow::ensure!(!runtime.engine_id.is_empty(), "定时任务执行 CLI 不能为空");
    anyhow::ensure!(!runtime.model_id.is_empty(), "定时任务模型不能为空");
    anyhow::ensure!(
        runtime.engine_id == cli_id,
        "定时任务运行配置与当前 CLI 不匹配"
    );

    // 统一推理强度大小写并将空字符串转换为未设置。
    runtime.reasoning_effort = runtime
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    Ok(runtime)
}

#[cfg(test)]
#[path = "../../tests/unit/base_cli_mcp_tests.rs"]
mod tests;

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
        /// 按任务使用的公共模型规则校验当前 CLI 的模型和推理强度。
        pub(crate) async fn prepare_scheduled_model_config(
            &self,
            cli: &dyn CliTool,
            runtime: crate::models::ScheduledRuntimeConfig,
        ) -> anyhow::Result<crate::models::ScheduledRuntimeConfig> {
            // 规范化公共字段，并沿用定时任务不绑定 workspace 的模型校验语义。
            let mut runtime = super::normalize_scheduled_runtime_fields(cli.id(), runtime)?;
            runtime.model_id = cli
                .validate_thread_model(None, &runtime.model_id)
                .await?;

            // 仅在任务配置提供推理强度时调用当前 CLI 的现有校验规则。
            if let Some(reasoning_effort) = runtime.reasoning_effort.as_deref() {
                runtime.reasoning_effort = Some(
                    cli.validate_thread_reasoning(&runtime.model_id, reasoning_effort)
                        .await?,
                );
            }
            Ok(runtime)
        }

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
            // let preset = tokio::task::spawn_blocking(|| AppConfig::load_or_create().map(|config| config.default_autonomy_preset().map(ToOwned::to_owned))).await.context("读取新线程默认设置任务失败")??;
            let preset = tokio::task::spawn_blocking(|| AppConfigService::load_or_create().map(|config| config.default_autonomy_preset().map(ToOwned::to_owned))).await.context("读取新线程默认设置任务失败")??;
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

/// 三种 CLI 复用的聊天事件、流式消息块和回合完成公共业务。
pub(crate) mod chat_business {
    use std::{
        collections::{HashMap, HashSet},
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
        sync::Arc,
        time::{Duration, Instant},
    };

    use anyhow::Context;
    use serde_json::{value::RawValue, Value};
    use tauri::{AppHandle, Emitter};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use crate::{
        cli_tools::{CliExecutionContext, CliLocationKind, CliTool},
        db,
        engines::{
            trim_action_output_delta_content, ActionResult, EngineEvent, OutputStream,
            ThreadScope, TurnAttachment, TurnCompletionStatus, TurnInput, TurnInputItem,
            STREAMED_DIFF_MAX_CHARS,
        },
        models::{
            EngineInfoDto, EngineModelDto, MessageStatusDto, ThreadDto, ThreadStatusDto,
            ThreadUpdateDto,
        },
        runtime_env,
        services::thread_service::ThreadService,
        ssh::remote_attachments::RemoteAttachmentBatch,
        state::AppState,
    };

    pub(crate) use crate::models::chat::*;

    pub(crate) const MAX_THREAD_TITLE_CHARS: usize = 72;
    pub(crate) const STREAM_EVENT_COALESCE_MAX_CHARS: usize = 8_192;
    pub(crate) const STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL: Duration = Duration::from_millis(24);
    pub(crate) const STREAM_DB_FLUSH_INTERVAL: Duration = Duration::from_millis(250);
    pub(crate) const STREAM_DB_BLOCKS_FLUSH_INTERVAL: Duration = Duration::from_millis(900);
    pub(crate) const ENGINE_EVENT_QUEUE_CAPACITY: usize = 1_280;
    pub(crate) const TURN_EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(900);
    pub(crate) const TURN_INTERRUPT_TIMEOUT: Duration = Duration::from_secs(10);
    pub(crate) const TURN_TASK_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
    pub(crate) const ACTION_OUTPUT_MAX_CHUNKS: usize = 240;
    pub(crate) const ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS: usize = 4_096;
    pub(crate) const TRUNCATED_SUFFIX: &str = "\n... [truncated]";
    pub(crate) const MAX_CHAT_NOTIFICATION_PREVIEW_CHARS: usize = 240;
    const TEXT_ATTACHMENT_EXTENSIONS: &[&str] = &[
        "txt", "md", "json", "js", "ts", "tsx", "jsx", "py", "rs", "go", "css", "html",
        "yaml", "yml", "toml", "xml", "sql", "sh", "csv",
    ];
    const IMAGE_ATTACHMENT_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg",
    ];

    static NEXT_ENGINE_EVENT_CONSUME_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    static NEXT_STREAM_PROCESS_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    static NEXT_STREAM_FLUSH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    /// 将结构化 JSON 值包装为可在消息块中原样序列化的内容。
    pub(crate) fn value_to_raw(value: &Value) -> Box<RawValue> {
        serde_json::value::to_raw_value(value).unwrap_or_else(|_| empty_raw_value())
    }

    /// 提供 JSON null 作为无法构造结构化消息详情时的默认内容。
    pub(crate) fn empty_raw_value() -> Box<RawValue> {
        RawValue::from_string("null".to_string()).expect("valid JSON null literal")
    }

    /// 执行共享聊天业务需要的数据库操作并保留数据库错误。
    pub(crate) async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
    {
        let result = match tokio::task::spawn_blocking(move || operation(&db)).await {
            Ok(result) => result,
            Err(error) => Err(anyhow::Error::new(error).context("聊天数据库任务执行失败")),
        };
        result.map_err(|error| {
            log::error!("聊天数据库任务失败: {error:#}");
            format!("{error:#}")
        })
    }

    /// 接收一个引擎事件并记录其排队等待时间。
    pub(crate) async fn receive_engine_event_with_diagnostics(
        event_rx: &mut mpsc::Receiver<EngineEvent>,
        consumer: &str,
    ) -> Option<EngineEvent> {
        let receive_sequence = NEXT_ENGINE_EVENT_CONSUME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let queue_depth_before = event_rx.len();
        let started_at = Instant::now();
        let result = event_rx.recv().await;
        let wait_ms = started_at.elapsed().as_millis();
        log::debug!(
            "engine event receive: sequence={receive_sequence}, consumer={consumer}, queue_depth_before={queue_depth_before}, queue_depth_after={}, wait_ms={wait_ms}",
            event_rx.len()
        );
        result
    }

    impl super::BaseCliMcp {
        /// 执行普通消息提交并让单一 CLI 实例贯穿本轮生命周期。
        pub(crate) async fn submit_chat_message(
            &self,
            cli: Arc<dyn CliTool>,
            app: AppHandle,
            request: SendMessageRequest,
        ) -> Result<String, ChatBusinessError> {
            submit_chat_message_inner(self.state.clone(), cli, app, request).await
        }

        /// 保存追加消息及其权限后，通过当前CLI提交steer并返回接收回执。
        pub(crate) async fn submit_chat_steer(
            &self,
            cli: Arc<dyn CliTool>,
            app: AppHandle,
            request: SteerMessageRequest,
        ) -> Result<crate::models::SteerReceiptDto, ChatBusinessError> {
            submit_chat_steer_inner(self.state.clone(), cli, app, request).await
        }

        /// 标记当前聊天回合取消，由持有该 CLI 的 run_turn 负责原生中断和收尾。
        pub(crate) async fn cancel_chat_turn(
            &self,
            thread_id: &str,
        ) -> Result<(), ChatBusinessError> {
            let thread = ThreadService::new(self.state.clone())
                .get_thread(thread_id)
                .await
                .map_err(|error| {
                    log::error!("读取待取消聊天线程失败: {error:#}");
                    ChatBusinessError::new("CHAT_OPERATION_FAILED", "无法读取待取消的聊天线程")
                })?;
            if thread.engine_id != self.runtime.cli_id {
                return Err(ChatBusinessError::new(
                    "VALIDATION_FAILED",
                    "聊天线程所属工具与当前工具不一致",
                ));
            }
            let Some(cancellation) = self.state.turns.get(thread_id).await else {
                return Ok(());
            };

            // 只取消已捕获的本轮令牌，避免重新读取并误伤后续新回合。
            cancellation.cancel();
            Ok(())
        }

        /// 校验并提交统一审批选择，再按真实回合状态原子保存审批结果。
        pub(crate) async fn answer_chat_approval(
            &self,
            cli: Arc<dyn CliTool>,
            request: ApprovalResponseRequest,
        ) -> Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
            answer_chat_approval_inner(self.state.clone(), cli, request).await
        }

        /// 核验线程的 SSH 项目位置后，通过当前 CLI 重启远端服务。
        pub(crate) async fn restart_chat_service(
            &self,
            cli: &dyn CliTool,
            thread_id: &str,
        ) -> Result<(), ChatBusinessError> {
            let thread = ThreadService::new(self.state.clone())
                .get_thread(thread_id)
                .await
                .map_err(|error| {
                    log::error!("读取待重启服务的聊天线程失败: {error:#}");
                    ChatBusinessError::new("CHAT_OPERATION_FAILED", "无法读取聊天线程")
                })?;
            if thread.engine_id != cli.id() {
                return Err(ChatBusinessError::new(
                    "VALIDATION_FAILED",
                    "聊天线程所属工具与当前工具不一致",
                ));
            }
            let context = cli
                .execution_context(Some(&thread.workspace_id))
                .await
                .map_err(|error| {
                    log::error!("创建服务重启执行上下文失败: {error:#}");
                    ChatBusinessError::new("CLI_UNAVAILABLE", "当前聊天工具暂不可用")
                })?;
            if matches!(context.location_kind, super::CliLocationKind::Ssh)
                && context.ssh_connection_id.is_none()
            {
                return Err(ChatBusinessError::new(
                    "VALIDATION_FAILED",
                    "远端项目未绑定 SSH 连接，无法重启服务",
                ));
            }
            cli.restart_service(&context).await.map_err(|error| {
                log::error!("重启聊天 CLI 服务失败: {error:#}");
                ChatBusinessError::new("CHAT_OPERATION_FAILED", "重启聊天工具服务失败")
            })
        }
    }
pub(crate) fn is_coalescable_stream_event(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::TextDelta { .. }
            | EngineEvent::ThinkingDelta { .. }
            | EngineEvent::ActionOutputDelta { .. }
            | EngineEvent::ActionProgressUpdated { .. }
    )
}

pub(crate) fn coalesced_event_content_len(event: &EngineEvent) -> usize {
    match event {
        EngineEvent::TextDelta { content }
        | EngineEvent::ThinkingDelta { content }
        | EngineEvent::ActionOutputDelta { content, .. } => content.len(),
        EngineEvent::ActionProgressUpdated { message, .. } => message.len(),
        _ => 0,
    }
}

pub(crate) fn same_output_stream(left: &OutputStream, right: &OutputStream) -> bool {
    matches!(
        (left, right),
        (OutputStream::Stdout, OutputStream::Stdout)
            | (OutputStream::Stderr, OutputStream::Stderr)
            | (OutputStream::Stdin, OutputStream::Stdin)
    )
}

#[allow(clippy::result_large_err)]
pub(crate) fn try_coalesce_stream_events(
    previous: EngineEvent,
    next: EngineEvent,
) -> Result<EngineEvent, (EngineEvent, EngineEvent)> {
    match (previous, next) {
        (
            EngineEvent::TextDelta { mut content },
            EngineEvent::TextDelta {
                content: next_content,
            },
        ) => {
            content.push_str(&next_content);
            Ok(EngineEvent::TextDelta { content })
        }
        (
            EngineEvent::ThinkingDelta { mut content },
            EngineEvent::ThinkingDelta {
                content: next_content,
            },
        ) => {
            content.push_str(&next_content);
            Ok(EngineEvent::ThinkingDelta { content })
        }
        (
            EngineEvent::ActionOutputDelta {
                action_id,
                stream,
                mut content,
            },
            EngineEvent::ActionOutputDelta {
                action_id: next_action_id,
                stream: next_stream,
                content: next_content,
            },
        ) => {
            if action_id == next_action_id && same_output_stream(&stream, &next_stream) {
                content.push_str(&next_content);
                Ok(EngineEvent::ActionOutputDelta {
                    action_id,
                    stream,
                    content,
                })
            } else {
                Err((
                    EngineEvent::ActionOutputDelta {
                        action_id,
                        stream,
                        content,
                    },
                    EngineEvent::ActionOutputDelta {
                        action_id: next_action_id,
                        stream: next_stream,
                        content: next_content,
                    },
                ))
            }
        }
        (
            EngineEvent::ActionProgressUpdated {
                action_id,
                message: _,
            },
            EngineEvent::ActionProgressUpdated {
                action_id: next_action_id,
                message: next_message,
            },
        ) if action_id == next_action_id => Ok(EngineEvent::ActionProgressUpdated {
            action_id,
            message: next_message,
        }),
        (previous, next) => Err((previous, next)),
    }
}

/// 将通用引擎事件映射为诊断日志使用的稳定事件名称。
pub(crate) fn chat_engine_event_kind(event: &EngineEvent) -> &'static str {
    match event {
        EngineEvent::TurnStarted { .. } => "turn_started",
        EngineEvent::TurnCompleted { .. } => "turn_completed",
        EngineEvent::TextDelta { .. } => "text_delta",
        EngineEvent::ThinkingDelta { .. } => "thinking_delta",
        EngineEvent::SteerApplied { .. } => "steer_applied",
        EngineEvent::ActionStarted { .. } => "action_started",
        EngineEvent::ActionOutputDelta { .. } => "action_output_delta",
        EngineEvent::ActionProgressUpdated { .. } => "action_progress_updated",
        EngineEvent::ActionBackgroundTaskAssigned { .. } => "action_background_task_assigned",
        EngineEvent::ActionCompleted { .. } => "action_completed",
        EngineEvent::DiffUpdated { .. } => "diff_updated",
        EngineEvent::ApprovalRequested { .. } => "approval_requested",
        EngineEvent::ApprovalExpired { .. } => "approval_expired",
        EngineEvent::UsageLimitsUpdated { .. } => "usage_limits_updated",
        EngineEvent::ModelRerouted { .. } => "model_rerouted",
        EngineEvent::Notice { .. } => "notice",
        EngineEvent::Error { .. } => "error",
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn process_stream_event(
    app: &tauri::AppHandle,
    state: &AppState,
    thread: &ThreadDto,
    assistant_message_id: &str,
    stream_event_topic: &str,
    approval_event_topic: &str,
    event: &EngineEvent,
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    max_output_chars: usize,
    cli: &dyn CliTool,
) -> EventProgress {
    let process_sequence = NEXT_STREAM_PROCESS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let processing_started_at = Instant::now();
    log::info!(
        "chat process_stream_event entered: thread_id={}, engine_id={}, assistant_message_id={}, event={event:?}",
        thread.id,
        thread.engine_id,
        assistant_message_id,
    );
    log::debug!("Codex transport log: {}", serde_json::json!({
        "at": runtime_env::system_time_rfc3339(),
        "event": "engine_event_processing_start",
        "process_sequence": process_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id,
        "event_kind": chat_engine_event_kind(event),
    }));
    if let Err(error) = cli
        .on_chat_event(thread, assistant_message_id, event)
        .await
    {
        log::error!(
            "当前 CLI 聊天事件专属处理失败: thread_id={}, message_id={}, error={error:#}",
            thread.id,
            assistant_message_id
        );
    }
    let mut normalized_event = event.clone();
    match &mut normalized_event {
        EngineEvent::ActionOutputDelta { content, .. } => {
            *content = trim_action_output_delta_content(content);
        }
        EngineEvent::ActionCompleted { result, .. } => {
            truncate_action_result_output(result, max_output_chars);
        }
        EngineEvent::DiffUpdated { diff, .. } => {
            *diff = truncate_chars(diff, STREAMED_DIFF_MAX_CHARS);
        }
        _ => {}
    }

    // CLI 专属事件已由上方回调处理，公共层只持久化统一事件字段。    let _ = app.emit(stream_event_topic, &normalized_event);
    if let EngineEvent::ApprovalRequested { summary, .. } = &normalized_event {
        let _ = app.emit(approval_event_topic, &normalized_event);
        // Unscoped companion event so background threads can surface pending
        // approvals without a per-thread listener.
        let _ = app.emit(
            "chat-approval-requested",
            ChatApprovalRequestedEvent {
                thread_id: thread.id.clone(),
                workspace_id: thread.workspace_id.clone(),
                engine_id: thread.engine_id.clone(),
                thread_title: thread.title.clone(),
                summary: summary.clone(),
            },
        );
        match run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            move |db| {
                db::scheduled_tasks::mark_run_needs_confirmation_by_message(
                    db,
                    &assistant_message_id,
                )
            }
        })
        .await
        {
            Ok(Some(task_id)) => {
                crate::scheduled_tasks::manager::emit_task_updated(app, &task_id);
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("failed to mark scheduled task as needing confirmation: {error}");
            }
        }
    }

    let log_event = engine_event_for_debug_log(&normalized_event);
    match serde_json::to_value(&log_event) {
        Ok(value) => {
            log::debug!(
                "engine event: thread_id={}, message_id={}, event={}",
                thread.id,
                assistant_message_id,
                value
            );
        }
        Err(error) => {
            log::error!("failed to serialize engine event: {error:#}");
        }
    }

    match &normalized_event {
        EngineEvent::TurnStarted {
            remote_turn_id: Some(remote_turn_id),
            ..
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let assistant_message_id = assistant_message_id.to_string();
                let remote_turn_id = remote_turn_id.clone();
                move |db| {
                    db::messages::bind_local_turn_to_remote_turn(
                        db,
                        &assistant_message_id,
                        &remote_turn_id,
                    )
                }
            })
            .await
            {
                log::warn!("failed to bind local turn to remote turn id: {error}");
            }
        }
        EngineEvent::ActionStarted {
            action_id,
            engine_action_id,
            action_type,
            summary,
            details,
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let action_id = action_id.clone();
                let thread_id = thread.id.clone();
                let assistant_message_id = assistant_message_id.to_string();
                let engine_action_id = engine_action_id.clone();
                let action_type = action_type.clone();
                let summary = summary.clone();
                let details = details.clone();
                move |db| {
                    db::actions::insert_action_started(
                        db,
                        &action_id,
                        &thread_id,
                        &assistant_message_id,
                        engine_action_id.as_deref(),
                        &action_type,
                        &summary,
                        &details,
                    )
                }
            })
            .await
            {
                log::warn!("failed to persist action start: {error}");
            }
        }
        EngineEvent::ActionCompleted { action_id, result } => {
            if let Err(error) = run_db(state.db.clone(), {
                let action_id = action_id.clone();
                let result = result.clone();
                move |db| db::actions::update_action_completed(db, &action_id, &result)
            })
            .await
            {
                log::warn!("failed to persist action completion: {error}");
            }
        }
        EngineEvent::ApprovalRequested {
            approval_id,
            action_type,
            summary,
            details,
        } => {
            if let Err(error) = run_db(state.db.clone(), {
                let approval_id = approval_id.clone();
                let thread_id = thread.id.clone();
                let assistant_message_id = assistant_message_id.to_string();
                let action_type = action_type.clone();
                let summary = summary.clone();
                let details = details.clone();
                move |db| {
                    db::actions::insert_approval(
                        db,
                        &approval_id,
                        &thread_id,
                        &assistant_message_id,
                        &action_type,
                        &summary,
                        &details,
                    )
                }
            })
            .await
            {
                log::warn!("failed to persist approval: {error}");
            }
        }
        EngineEvent::ApprovalExpired { approval_id, .. } => {
            // 审批随轮次结束被判死时，同步划掉 approvals 表里的 pending 记录：
            // 否则启动恢复（derive_thread_status_for_recovery）会凭这条记录把线程
            // 误判回 awaiting_approval，重启后输入框被终止键锁死（2026-09-13 实证）。
            if let Err(error) = run_db(state.db.clone(), {
                let approval_id = approval_id.clone();
                move |db| db::actions::expire_approval(db, &approval_id)
            })
            .await
            {
                log::warn!("failed to persist approval expiry: {error}");
            }
        }
        _ => {}
    }

    let progress = apply_event_to_blocks(
        blocks,
        action_index,
        approval_index,
        &normalized_event,
        max_output_chars,
    );
    if !progress.finalized_actions.is_empty() {
        if let Err(error) = run_db(state.db.clone(), {
            let finalized_actions = progress.finalized_actions.clone();
            move |db| {
                for (action_id, result) in finalized_actions {
                    db::actions::update_action_completed(db, &action_id, &result)?;
                }
                Ok(())
            }
        })
        .await
        {
            log::warn!("failed to persist action finalization after turn completion: {error}");
        }
    }
    let processing_ms = processing_started_at.elapsed().as_millis();
    let record = serde_json::json!({
        "at": runtime_env::system_time_rfc3339(),
        "event": "engine_event_processing_complete",
        "process_sequence": process_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id.to_string(),
        "event_kind": chat_engine_event_kind(&normalized_event),
        "processing_ms": processing_ms,
        "persist_engine_event_logs": state.config.debug.persist_engine_event_logs,
        "blocks_changed": progress.blocks_changed,
        "force_persist": progress.force_persist,
        "message_status_changed": progress.message_status.is_some(),
        "thread_status_changed": progress.thread_status.is_some(),
        "token_usage_changed": progress.token_usage.is_some(),
        "turn_model_changed": progress.turn_model_id.is_some(),
    });
    if processing_ms >= 25 {
        log::warn!("engine event processing: {record}");
    } else {
        log::debug!("engine event processing: {record}");
    }
    log::debug!("Codex transport log: {}", record);

    progress
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_stream_progress(
    progress: EventProgress,
    message_status: &mut MessageStatusDto,
    thread_status: &mut ThreadStatusDto,
    turn_model_id: &mut String,
    token_usage: &mut Option<(u64, u64)>,
    blocks_dirty: &mut bool,
    message_state_dirty: &mut bool,
    thread_status_dirty: &mut bool,
    turn_model_dirty: &mut bool,
) -> bool {
    if progress.blocks_changed {
        *blocks_dirty = true;
    }

    if let Some(status) = progress.message_status {
        if *message_status != status {
            *message_status = status;
            *message_state_dirty = true;
        }
    }

    if let Some(status) = progress.thread_status {
        if *thread_status != status {
            *thread_status = status;
            *thread_status_dirty = true;
        }
    }

    if let Some(next_turn_model_id) = progress.turn_model_id {
        if *turn_model_id != next_turn_model_id {
            *turn_model_id = next_turn_model_id;
            *turn_model_dirty = true;
        }
    }

    if let Some(tokens) = progress.token_usage {
        *token_usage = Some(tokens);
    }

    progress.force_persist
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn flush_stream_state(
    state: &AppState,
    thread: &ThreadDto,
    assistant_message_id: &str,
    blocks: &[ContentBlock],
    message_status: &MessageStatusDto,
    thread_status: &ThreadStatusDto,
    turn_model_id: &str,
    blocks_dirty: &mut bool,
    message_state_dirty: &mut bool,
    thread_status_dirty: &mut bool,
    turn_model_dirty: &mut bool,
    last_persisted_thread_status: &mut ThreadStatusDto,
    last_persist_at: &mut Instant,
    last_blocks_persist_at: &mut Instant,
    force: bool,
) {
    let flush_sequence = NEXT_STREAM_FLUSH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let flush_started_at = Instant::now();
    let blocks_json_for_log = match serde_json::to_string(blocks) {
        Ok(value) => value,
        Err(error) => format!("<blocks serialization failed: {error:#}>"),
    };
    log::info!(
        "chat flush_stream_state entered: thread_id={}, engine_id={}, assistant_message_id={}, blocks_json={}, message_status={message_status:?}, thread_status={thread_status:?}, turn_model_id={}, blocks_dirty={}, message_state_dirty={}, thread_status_dirty={}, turn_model_dirty={}, force={}",
        thread.id,
        thread.engine_id,
        assistant_message_id,
        blocks_json_for_log,
        turn_model_id,
        *blocks_dirty,
        *message_state_dirty,
        *thread_status_dirty,
        *turn_model_dirty,
        force,
    );
    log::debug!("Codex transport log: {}", serde_json::json!({
        "at": runtime_env::system_time_rfc3339(),
        "event": "engine_stream_state_flush_start",
        "flush_sequence": flush_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id,
        "force": force,
        "blocks_dirty": *blocks_dirty,
        "message_state_dirty": *message_state_dirty,
        "thread_status_dirty": *thread_status_dirty,
        "turn_model_dirty": *turn_model_dirty,
    }));
    if !*blocks_dirty && !*message_state_dirty && !*thread_status_dirty && !*turn_model_dirty {
        log::debug!("Codex transport log: {}", serde_json::json!({
            "at": runtime_env::system_time_rfc3339(),
            "event": "engine_stream_state_flush_complete",
            "flush_sequence": flush_sequence,
            "thread_id": thread.id.clone(),
            "message_id": assistant_message_id,
            "result": "no_dirty_state",
            "flush_ms": flush_started_at.elapsed().as_millis(),
        }));
        return;
    }

    let now = Instant::now();

    if *thread_status_dirty && *last_persisted_thread_status == *thread_status {
        *thread_status_dirty = false;
    }

    let should_flush_state =
        force || now.duration_since(*last_persist_at) >= STREAM_DB_FLUSH_INTERVAL;
    let should_flush_blocks =
        force || now.duration_since(*last_blocks_persist_at) >= STREAM_DB_BLOCKS_FLUSH_INTERVAL;

    if !should_flush_blocks && !should_flush_state {
        log::debug!("Codex transport log: {}", serde_json::json!({
            "at": runtime_env::system_time_rfc3339(),
            "event": "engine_stream_state_flush_complete",
            "flush_sequence": flush_sequence,
            "thread_id": thread.id.clone(),
            "message_id": assistant_message_id,
            "result": "deferred_by_interval",
            "flush_ms": flush_started_at.elapsed().as_millis(),
            "should_flush_state": should_flush_state,
            "should_flush_blocks": should_flush_blocks,
        }));
        return;
    }

    let mut did_flush_state = false;
    let mut did_flush_blocks = false;

    if *blocks_dirty && should_flush_blocks {
        match serde_json::to_string(blocks) {
            Ok(blocks_json) => {
                log::info!(
                    "chat flush_stream_state blocks serialized before DB write: thread_id={}, assistant_message_id={}, blocks_json={}",
                    thread.id,
                    assistant_message_id,
                    blocks_json,
                );
                let persist_result = run_db(state.db.clone(), {
                    let assistant_message_id = assistant_message_id.to_string();
                    let message_status = message_status.clone();
                    let turn_model_id = turn_model_id.to_string();
                    move |db| {
                        db::messages::update_assistant_blocks_json(
                            db,
                            &assistant_message_id,
                            &blocks_json,
                            message_status,
                            Some(turn_model_id.as_str()),
                        )
                    }
                })
                .await;
                log::info!(
                    "chat flush_stream_state blocks DB write result: thread_id={}, assistant_message_id={}, result={persist_result:?}",
                    thread.id,
                    assistant_message_id,
                );
                if let Err(error) = persist_result {
                    log::warn!("failed to persist assistant stream blocks: {error}");
                } else {
                    *blocks_dirty = false;
                    *message_state_dirty = false;
                    *turn_model_dirty = false;
                    did_flush_blocks = true;
                    did_flush_state = true;
                }
            }
            Err(error) => {
                log::warn!("failed to serialize assistant stream blocks: {error}");
            }
        }
    } else if *message_state_dirty && should_flush_state {
        let persist_result = run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            let message_status = message_status.clone();
            move |db| {
                db::messages::update_assistant_status(db, &assistant_message_id, message_status)
            }
        })
        .await;
        log::info!(
            "chat flush_stream_state message status DB write result: thread_id={}, assistant_message_id={}, message_status={message_status:?}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist assistant stream status: {error}");
        } else {
            *message_state_dirty = false;
            did_flush_state = true;
        }
    }

    if *turn_model_dirty && should_flush_state {
        let persist_result = run_db(state.db.clone(), {
            let assistant_message_id = assistant_message_id.to_string();
            let turn_model_id = turn_model_id.to_string();
            move |db| {
                db::messages::update_assistant_turn_model_id(
                    db,
                    &assistant_message_id,
                    &turn_model_id,
                )
            }
        })
        .await;
        log::info!(
            "chat flush_stream_state model DB write result: thread_id={}, assistant_message_id={}, turn_model_id={}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
            turn_model_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist assistant turn model id during stream: {error}");
        } else {
            *turn_model_dirty = false;
            did_flush_state = true;
        }
    }

    if *thread_status_dirty && should_flush_state && *last_persisted_thread_status != *thread_status
    {
        let persist_result = run_db(state.db.clone(), {
            let thread_id = thread.id.clone();
            let thread_status = thread_status.clone();
            move |db| db::threads::update_thread_status(db, &thread_id, thread_status)
        })
        .await;
        log::info!(
            "chat flush_stream_state thread status DB write result: thread_id={}, assistant_message_id={}, thread_status={thread_status:?}, result={persist_result:?}",
            thread.id,
            assistant_message_id,
        );
        if let Err(error) = persist_result {
            log::warn!("failed to persist thread status during stream: {error}");
        } else {
            *last_persisted_thread_status = thread_status.clone();
            *thread_status_dirty = false;
            did_flush_state = true;
        }
    }

    if did_flush_blocks {
        *last_blocks_persist_at = now;
    }
    if did_flush_state {
        *last_persist_at = now;
    }

    let flush_ms = flush_started_at.elapsed().as_millis();
    let record = serde_json::json!({
        "at": runtime_env::system_time_rfc3339(),
        "event": "engine_stream_state_flush_complete",
        "flush_sequence": flush_sequence,
        "engine_id": thread.engine_id.clone(),
        "thread_id": thread.id.clone(),
        "message_id": assistant_message_id.to_string(),
        "flush_ms": flush_ms,
        "did_flush_state": did_flush_state,
        "did_flush_blocks": did_flush_blocks,
        "force": force,
        "should_flush_state": should_flush_state,
        "should_flush_blocks": should_flush_blocks,
        "blocks_dirty_after": *blocks_dirty,
        "message_state_dirty_after": *message_state_dirty,
        "thread_status_dirty_after": *thread_status_dirty,
        "turn_model_dirty_after": *turn_model_dirty,
    });
    if flush_ms >= 25 {
        log::warn!("engine stream state flush: {record}");
    } else {
        log::debug!("engine stream state flush: {record}");
    }
    log::debug!("Codex transport log: {}", record);
}

    /// 使用当前 CLI 的会话预览为尚未命名的线程生成标题。
    pub(crate) async fn maybe_update_thread_title(
        state: &AppState,
        cli: &dyn CliTool,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        user_message: &str,
    ) -> Option<ThreadDto> {
        if !should_autotitle_thread(thread) {
            return None;
        }
        let preview = cli
            .read_thread_preview(context, thread, engine_thread_id)
            .await
            .ok()
            .flatten();
        let candidate = preview
            .as_deref()
            .and_then(normalize_thread_title)
            .or_else(|| normalize_thread_title(user_message))?;
        if candidate == thread.title {
            return None;
        }

        let updated_thread = match run_db(state.db.clone(), {
            let thread_id = thread.id.clone();
            let candidate = candidate.clone();
            move |db| {
                db::threads::update_thread_title(db, &thread_id, &candidate)?;
                db::threads::get_thread(db, &thread_id)?.ok_or_else(|| {
                    anyhow::anyhow!("thread not found after title update: {thread_id}")
                })
            }
        })
        .await
        {
            Ok(updated_thread) => updated_thread,
            Err(error) => {
                log::warn!("failed to update thread title: {error}");
                return None;
            }
        };
        if let Err(error) = cli
            .set_thread_name(context, thread, engine_thread_id, &candidate)
            .await
        {
            log::debug!("failed to sync thread name with current CLI: {error}");
        }
        Some(updated_thread)
    }
pub(crate) fn should_autotitle_thread(thread: &ThreadDto) -> bool {
    thread.message_count == 0 && !thread_manual_title_locked(thread.engine_metadata.as_ref())
}

pub(crate) fn thread_manual_title_locked(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(|value| value.get("manualTitle"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn normalize_thread_title(raw: &str) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = compact.trim_matches(|c| c == '"' || c == '\'').to_string();
    if title.is_empty() {
        return None;
    }

    if title.chars().count() > MAX_THREAD_TITLE_CHARS {
        title = truncate_title(title, MAX_THREAD_TITLE_CHARS);
    }

    Some(title)
}

pub(crate) fn truncate_title(value: String, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value;
    }

    if max_chars <= 3 {
        return value.chars().take(max_chars).collect::<String>();
    }

    let mut output = value.chars().take(max_chars - 3).collect::<String>();
    output.push_str("...");
    output
}

pub(crate) fn normalize_chat_notification_preview(raw: &str) -> Option<String> {
    let compact = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(truncate_title(
        trimmed.to_string(),
        MAX_CHAT_NOTIFICATION_PREVIEW_CHARS,
    ))
}

pub(crate) fn chat_notification_preview(blocks: &[ContentBlock]) -> Option<String> {
    for block in blocks {
        match block {
            ContentBlock::Text {
                is_steer: Some(true),
                ..
            } => {}
            ContentBlock::Text { content, .. }
            | ContentBlock::Thinking { content, .. }
            | ContentBlock::Error { message: content } => {
                if let Some(preview) = normalize_chat_notification_preview(content) {
                    return Some(preview);
                }
            }
            ContentBlock::Notice { message, title, .. } => {
                if let Some(preview) = normalize_chat_notification_preview(message) {
                    return Some(preview);
                }
                if let Some(preview) = normalize_chat_notification_preview(title) {
                    return Some(preview);
                }
            }
            _ => {}
        }
    }

    None
}

    /// 将聊天回合最终状态交给定时任务管理器完成独立任务的收尾。
    pub(crate) async fn finish_scheduled_task_run(
        app: &AppHandle,
        state: &AppState,
        assistant_message_id: &str,
        status: &MessageStatusDto,
        blocks: &[ContentBlock],
    ) {
        let preview = chat_notification_preview(blocks);
        if let Err(error) = crate::scheduled_tasks::manager::finish_chat_run(
            app,
            state,
            assistant_message_id,
            status,
            preview,
        )
        .await
        {
            log::error!("完成聊天定时任务运行失败: {error:#}");
        }
    }
pub(crate) fn emit_chat_turn_finished(
    app: &tauri::AppHandle,
    thread: &ThreadDto,
    status: &MessageStatusDto,
    blocks: &[ContentBlock],
) {
    if matches!(status, MessageStatusDto::Streaming) {
        log::error!(
            "不发送未结束的聊天回合完成事件: thread_id={}, status={status:?}",
            thread.id
        );
        return;
    }
    let event = ChatTurnFinishedEvent {
        thread_id: thread.id.clone(),
        workspace_id: thread.workspace_id.clone(),
        engine_id: thread.engine_id.clone(),
        thread_title: thread.title.clone(),
        status: match status {
            MessageStatusDto::Completed => "completed",
            MessageStatusDto::Interrupted => "interrupted",
            MessageStatusDto::Error => "error",
            MessageStatusDto::Streaming => "streaming",
        }
        .to_string(),
        preview: chat_notification_preview(blocks),
    };
    let _ = app.emit("chat-turn-finished", event);
}

pub(crate) fn build_final_thread_event(
    latest_thread: Option<ThreadDto>,
    fallback_thread: &ThreadDto,
) -> (ThreadUpdatedEvent, Option<ThreadDto>) {
    match latest_thread {
        Some(latest_thread) => (
            ThreadUpdatedEvent {
                thread_id: latest_thread.id.clone(),
                workspace_id: latest_thread.workspace_id.clone(),
                thread: Some(latest_thread.clone()),
            },
            Some(latest_thread),
        ),
        None => (
            ThreadUpdatedEvent {
                thread_id: fallback_thread.id.clone(),
                workspace_id: fallback_thread.workspace_id.clone(),
                thread: None,
            },
            None,
        ),
    }
}

pub(crate) fn apply_event_to_blocks(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    event: &EngineEvent,
    max_output_chars: usize,
) -> EventProgress {
    let mut progress = EventProgress::default();

    match event {
        EngineEvent::TurnStarted { .. } => {
            progress.thread_status = Some(ThreadStatusDto::Streaming);
        }
        EngineEvent::TurnCompleted {
            token_usage,
            status,
        } => {
            progress.force_persist = true;
            match status {
                TurnCompletionStatus::Completed => {
                    progress.message_status = Some(MessageStatusDto::Completed);
                    progress.thread_status = Some(ThreadStatusDto::Completed);
                }
                TurnCompletionStatus::Interrupted => {
                    progress.message_status = Some(MessageStatusDto::Interrupted);
                    progress.thread_status = Some(ThreadStatusDto::Idle);
                }
                TurnCompletionStatus::Failed => {
                    progress.message_status = Some(MessageStatusDto::Error);
                    progress.thread_status = Some(ThreadStatusDto::Error);
                }
            }
            progress.token_usage = token_usage
                .as_ref()
                .map(|usage| (usage.input, usage.output));

            let action_result = match status {
                TurnCompletionStatus::Completed => ActionResult {
                    success: true,
                    output: None,
                    error: None,
                    diff: None,
                    duration_ms: 0,
                },
                TurnCompletionStatus::Interrupted => ActionResult {
                    success: false,
                    output: None,
                    error: Some(
                        "Action did not report completion before the turn was interrupted."
                            .to_string(),
                    ),
                    diff: None,
                    duration_ms: 0,
                },
                TurnCompletionStatus::Failed => ActionResult {
                    success: false,
                    output: None,
                    error: Some(
                        "Action did not report completion because the turn failed.".to_string(),
                    ),
                    diff: None,
                    duration_ms: 0,
                },
            };

            for block in blocks.iter_mut() {
                let ContentBlock::Action {
                    action_id,
                    status,
                    result,
                    ..
                } = block
                else {
                    continue;
                };
                if status != "running" && status != "pending" {
                    continue;
                }

                *status = if action_result.success {
                    "done"
                } else {
                    "error"
                }
                .to_string();
                *result = (!action_result.success).then(|| ActionBlockResult {
                    success: action_result.success,
                    output: action_result.output.clone(),
                    error: action_result.error.clone(),
                    diff: action_result.diff.clone(),
                    duration_ms: action_result.duration_ms,
                });
                progress
                    .finalized_actions
                    .push((action_id.clone(), action_result.clone()));
                progress.blocks_changed = true;
            }

            for block in blocks.iter_mut() {
                let ContentBlock::Notice { metadata, .. } = block else {
                    continue;
                };
                let Some(raw) = metadata else {
                    continue;
                };
                let Ok(mut value) = serde_json::from_str::<Value>(raw.get()) else {
                    continue;
                };
                let Some(background_tasks) = value
                    .get_mut("backgroundTasks")
                    .and_then(Value::as_array_mut)
                else {
                    continue;
                };

                let mut blocks_changed = false;
                for task in background_tasks.iter_mut() {
                    if task.get("status").and_then(Value::as_str) != Some("running") {
                        continue;
                    }
                    let Some(status) = task.get_mut("status") else {
                        continue;
                    };
                    *status = Value::String("stopped".to_string());
                    blocks_changed = true;
                }
                if !blocks_changed {
                    continue;
                }
                let Ok(updated_metadata) = RawValue::from_string(value.to_string()) else {
                    continue;
                };
                *metadata = Some(updated_metadata);
                progress.blocks_changed = true;
            }

            for block in blocks.iter_mut() {
                let ContentBlock::Steer {
                    delivery_status, ..
                } = block
                else {
                    continue;
                };
                if delivery_status == "failed" || delivery_status == "settled" {
                    continue;
                }
                *delivery_status = "settled".to_string();
                progress.blocks_changed = true;
            }
        }
        EngineEvent::TextDelta { content } => {
            progress.blocks_changed = append_text_delta(blocks, content);
        }
        EngineEvent::ThinkingDelta { content } => {
            progress.blocks_changed = append_thinking_delta(blocks, content);
        }
        EngineEvent::SteerApplied {
            client_steer_id,
            content,
            plan_mode,
            attachments,
            input_items,
        } => {
            let attachment_blocks = attachments
                .iter()
                .map(|attachment| ContentBlock::Attachment {
                    file_name: attachment.file_name.clone(),
                    file_path: attachment.file_path.clone(),
                    preview_file_path: attachment.preview_file_path.clone(),
                    size_bytes: attachment.size_bytes,
                    mime_type: attachment.mime_type.clone(),
                    browser_annotation: attachment.browser_annotation.clone(),
                    is_remote: attachment.is_remote,
                })
                .collect();
            let mut skill_blocks = Vec::new();
            let mut mention_blocks = Vec::new();
            for item in input_items {
                match item {
                    TurnInputItem::Skill { name, path } => skill_blocks.push(ContentBlock::Skill {
                        name: name.clone(),
                        path: path.clone(),
                    }),
                    TurnInputItem::Mention { name, path } => {
                        mention_blocks.push(ContentBlock::Mention {
                            name: name.clone(),
                            path: path.clone(),
                        })
                    }
                    TurnInputItem::Text { .. } => {}
                }
            }
            blocks.push(ContentBlock::Steer {
                steer_id: client_steer_id.clone(),
                content: content.clone(),
                delivery_status: "applied".to_string(),
                plan_mode: plan_mode.then_some(true),
                attachments: attachment_blocks,
                skills: skill_blocks,
                mentions: mention_blocks,
            });
            progress.blocks_changed = true;
            progress.force_persist = true;
        }
        EngineEvent::ActionStarted {
            action_id,
            engine_action_id,
            action_type,
            summary,
            details,
        } => {
            let block = ContentBlock::Action {
                action_id: action_id.to_string(),
                engine_action_id: engine_action_id.clone(),
                action_type: action_type.as_str().to_string(),
                summary: summary.to_string(),
                details: value_to_raw(details),
                background_task_id: None,
                output_chunks: Vec::new(),
                status: "running".to_string(),
                result: None,
            };
            progress.blocks_changed = upsert_action_block(blocks, action_index, action_id, block);
        }
        EngineEvent::ActionOutputDelta {
            action_id,
            stream,
            content,
        } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    output_chunks,
                    details,
                    ..
                }) = blocks.get_mut(index)
                {
                    let stream_name = match stream {
                        OutputStream::Stdout => "stdout",
                        OutputStream::Stderr => "stderr",
                        OutputStream::Stdin => "stdin",
                    };
                    let chunk_content = truncate_chars(content, max_output_chars);
                    if chunk_content.is_empty() {
                        return progress;
                    }

                    if let Some(previous_chunk) = output_chunks.last_mut() {
                        if previous_chunk.stream == stream_name {
                            previous_chunk.content.push_str(&chunk_content);
                        } else {
                            output_chunks.push(ActionOutputChunk {
                                stream: stream_name.to_string(),
                                content: chunk_content,
                            });
                        }
                    } else {
                        output_chunks.push(ActionOutputChunk {
                            stream: stream_name.to_string(),
                            content: chunk_content,
                        });
                    }

                    if trim_action_output_chunks(output_chunks, max_output_chars) {
                        mark_output_truncated(details);
                    }
                    progress.blocks_changed = true;
                }
            }
        }
        EngineEvent::ActionProgressUpdated { action_id, message } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action { details, .. }) = blocks.get_mut(index) {
                    progress.blocks_changed = update_action_progress(details, message);
                }
            }
        }
        EngineEvent::ActionBackgroundTaskAssigned { action_id, task_id } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    background_task_id,
                    ..
                }) = blocks.get_mut(index)
                {
                    if background_task_id.as_deref() != Some(task_id.as_str()) {
                        *background_task_id = Some(task_id.clone());
                        progress.blocks_changed = true;
                    }
                }
            }
        }
        EngineEvent::ActionCompleted { action_id, result } => {
            if let Some(index) = action_index.get(action_id).copied() {
                if let Some(ContentBlock::Action {
                    status,
                    result: block_result,
                    ..
                }) = blocks.get_mut(index)
                {
                    *status = if result.success { "done" } else { "error" }.to_string();
                    *block_result = Some(ActionBlockResult {
                        success: result.success,
                        output: result.output.clone(),
                        error: result.error.clone(),
                        diff: result.diff.clone(),
                        duration_ms: result.duration_ms,
                    });
                    progress.blocks_changed = true;
                }
            }
        }
        EngineEvent::DiffUpdated { diff, scope } => {
            let scope = match scope {
                crate::engines::DiffScope::Turn => "turn",
                crate::engines::DiffScope::File => "file",
                crate::engines::DiffScope::Workspace => "workspace",
            }
            .to_string();

            let latest_matching_index =
                blocks.iter().enumerate().rev().find_map(|(index, block)| {
                    if let ContentBlock::Diff {
                        scope: block_scope, ..
                    } = block
                    {
                        if block_scope == &scope {
                            return Some(index);
                        }
                    }
                    None
                });

            if let Some(latest_matching_index) = latest_matching_index {
                let mut next_blocks = Vec::with_capacity(blocks.len());
                for (index, mut block) in blocks.drain(..).enumerate() {
                    if let ContentBlock::Diff {
                        diff: block_diff,
                        scope: block_scope,
                    } = &mut block
                    {
                        if block_scope == &scope {
                            if index == latest_matching_index {
                                if block_diff != diff {
                                    *block_diff = diff.to_string();
                                }
                                next_blocks.push(block);
                            }
                            continue;
                        }
                    }
                    next_blocks.push(block);
                }
                *blocks = next_blocks;
            } else {
                blocks.push(ContentBlock::Diff {
                    diff: diff.to_string(),
                    scope,
                });
            }
            progress.blocks_changed = true;
        }
        EngineEvent::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => {
            let block = ContentBlock::Notice {
                kind: "model_rerouted".to_string(),
                level: "info".to_string(),
                title: "Model rerouted".to_string(),
                message: format_model_reroute_notice(from_model, to_model, reason),
                // 模型切换提示不附带结构化后台任务元数据。
                metadata: None,
            };
            progress.blocks_changed = upsert_notice_block(
                blocks,
                action_index,
                approval_index,
                "model_rerouted",
                block,
            );
            progress.turn_model_id = Some(to_model.to_string());
            progress.force_persist = true;
        }
        EngineEvent::Notice {
            kind,
            level,
            title,
            message,
            metadata,
        } => {
            let block = ContentBlock::Notice {
                kind: kind.to_string(),
                level: level.to_string(),
                title: title.to_string(),
                message: message.to_string(),
                // 保留 Claude 后台任务结构化元数据，供持久化和前端卡片渲染。
                metadata: metadata.as_ref().map(value_to_raw),
            };
            progress.blocks_changed =
                upsert_notice_block(blocks, action_index, approval_index, kind, block);
            progress.force_persist = true;
        }
        EngineEvent::ApprovalRequested {
            approval_id,
            action_type,
            summary,
            details,
        } => {
            let block = ContentBlock::Approval {
                approval_id: approval_id.to_string(),
                action_type: action_type.as_str().to_string(),
                summary: summary.to_string(),
                details: value_to_raw(details),
                status: "pending".to_string(),
                decision: None,
            };
            progress.blocks_changed =
                upsert_approval_block(blocks, approval_index, approval_id, block);
            progress.thread_status = Some(ThreadStatusDto::AwaitingApproval);
            progress.force_persist = true;
        }
        EngineEvent::ApprovalExpired { approval_id, .. } => {
            // 审批随轮次结束失效时，把持久化块从 pending 改为 expired，重进会话后仍展示已失效。
            if let Some(index) = approval_index.get(approval_id).copied() {
                if let Some(ContentBlock::Approval { status, .. }) = blocks.get_mut(index) {
                    if status == "pending" {
                        *status = "expired".to_string();
                        progress.blocks_changed = true;
                        progress.force_persist = true;
                    }
                }
            }
        }
        EngineEvent::Error {
            message,
            recoverable,
        } => {
            blocks.push(ContentBlock::Error {
                message: message.to_string(),
            });
            progress.blocks_changed = true;
            if !recoverable {
                progress.message_status = Some(MessageStatusDto::Error);
                progress.thread_status = Some(ThreadStatusDto::Error);
                progress.force_persist = true;
            }
        }
        EngineEvent::UsageLimitsUpdated { .. } => {}
    }

    progress
}

pub(crate) fn append_text_delta(blocks: &mut Vec<ContentBlock>, content: &str) -> bool {
    if content.is_empty() {
        return false;
    }

    if let Some(ContentBlock::Text {
        content: current, ..
    }) = blocks.last_mut()
    {
        current.push_str(content);
        return true;
    }

    blocks.push(ContentBlock::Text {
        content: content.to_string(),
        plan_mode: None,
        is_steer: None,
        client_steer_id: None,
    });
    true
}

pub(crate) fn append_thinking_delta(blocks: &mut Vec<ContentBlock>, content: &str) -> bool {
    if content.is_empty() {
        return false;
    }

    if let Some(ContentBlock::Thinking {
        content: current, ..
    }) = blocks.last_mut()
    {
        current.push_str(content);
        return true;
    }

    blocks.push(ContentBlock::Thinking {
        content: content.to_string(),
        started_at: None,
        duration_ms: None,
    });
    true
}

pub(crate) fn update_action_progress(details: &mut Box<RawValue>, message: &str) -> bool {
    let mut value: Value = serde_json::from_str(details.get())
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));

    let current_message = value
        .get("progressMessage")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let current_kind = value
        .get("progressKind")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    if current_message.as_deref() == Some(message) && current_kind.as_deref() == Some("mcp") {
        return false;
    }

    if !value.is_object() {
        value = Value::Object(serde_json::Map::new());
    }

    if let Some(details_object) = value.as_object_mut() {
        details_object.insert("progressKind".to_string(), Value::String("mcp".to_string()));
        details_object.insert(
            "progressMessage".to_string(),
            Value::String(message.to_string()),
        );
        *details = value_to_raw(&value);
        return true;
    }

    false
}

pub(crate) fn upsert_action_block(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    action_id: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = action_index.get(action_id).copied() {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    let index = blocks.len();
    blocks.push(block);
    action_index.insert(action_id.to_string(), index);
    true
}

pub(crate) fn upsert_approval_block(
    blocks: &mut Vec<ContentBlock>,
    approval_index: &mut HashMap<String, usize>,
    approval_id: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = approval_index.get(approval_id).copied() {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    let index = blocks.len();
    blocks.push(block);
    approval_index.insert(approval_id.to_string(), index);
    true
}

pub(crate) fn upsert_notice_block(
    blocks: &mut Vec<ContentBlock>,
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
    kind: &str,
    block: ContentBlock,
) -> bool {
    if let Some(index) = blocks.iter().position(|existing| {
        matches!(
            existing,
            ContentBlock::Notice {
                kind: existing_kind,
                ..
            } if existing_kind == kind
        )
    }) {
        if let Some(existing) = blocks.get_mut(index) {
            *existing = block;
            return true;
        }
    }

    if kind.starts_with("hook_") || kind.starts_with("codex_hook_") {
        blocks.push(block);
        return true;
    }

    blocks.insert(0, block);
    rebuild_block_indexes(blocks, action_index, approval_index);
    true
}

pub(crate) fn rebuild_block_indexes(
    blocks: &[ContentBlock],
    action_index: &mut HashMap<String, usize>,
    approval_index: &mut HashMap<String, usize>,
) {
    action_index.clear();
    approval_index.clear();

    for (index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Action { action_id, .. } => {
                action_index.insert(action_id.clone(), index);
            }
            ContentBlock::Approval { approval_id, .. } => {
                approval_index.insert(approval_id.clone(), index);
            }
            _ => {}
        }
    }
}

pub(crate) fn format_model_reroute_notice(from_model: &str, to_model: &str, reason: &str) -> String {
    format!("Switched from {from_model} to {to_model} ({reason}).")
}

pub(crate) fn trim_action_output_chunks(
    output_chunks: &mut Vec<ActionOutputChunk>,
    max_output_chars: usize,
) -> bool {
    let mut truncated = false;

    if output_chunks.len() > ACTION_OUTPUT_MAX_CHUNKS {
        let overflow = output_chunks.len() - ACTION_OUTPUT_MAX_CHUNKS;
        output_chunks.drain(0..overflow);
        truncated = true;
    }

    let max_chars = max_output_chars.max(1);
    let total_chars: usize = output_chunks.iter().map(|chunk| chunk.content.len()).sum();
    if total_chars <= max_chars {
        return truncated;
    }

    let target_chars = max_chars.saturating_mul(2) / 3;
    let chars_to_trim = total_chars.saturating_sub(target_chars.max(1));
    if chars_to_trim == 0 {
        return truncated;
    }

    let mut remaining_to_trim = chars_to_trim;
    let mut remove_count = 0usize;
    for chunk in output_chunks.iter_mut() {
        if remaining_to_trim == 0 {
            break;
        }
        let chunk_len = chunk.content.len();
        if chunk_len <= remaining_to_trim {
            remaining_to_trim -= chunk_len;
            remove_count += 1;
            continue;
        }

        chunk.content = trim_string_start_bytes(&chunk.content, remaining_to_trim);
        remaining_to_trim = 0;
        truncated = true;
    }

    if remove_count > 0 {
        output_chunks.drain(0..remove_count);
        truncated = true;
    }

    truncated
}

pub(crate) fn engine_event_for_debug_log(event: &EngineEvent) -> EngineEvent {
    match event {
        EngineEvent::ActionOutputDelta {
            action_id,
            stream,
            content,
        } => EngineEvent::ActionOutputDelta {
            action_id: action_id.clone(),
            stream: stream.clone(),
            content: truncate_chars_within_limit(content, ENGINE_EVENT_LOG_ACTION_OUTPUT_MAX_CHARS),
        },
        _ => event.clone(),
    }
}

pub(crate) fn trim_string_start_bytes(value: &str, bytes_to_trim: usize) -> String {
    if bytes_to_trim == 0 {
        return value.to_string();
    }
    if bytes_to_trim >= value.len() {
        return String::new();
    }

    let start = value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= bytes_to_trim)
        .unwrap_or(value.len());
    value[start..].to_string()
}

pub(crate) fn mark_output_truncated(details: &mut Box<RawValue>) {
    let mut value: Value = serde_json::from_str(details.get())
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));

    if !value.is_object() {
        value = Value::Object(serde_json::Map::new());
    }

    if let Some(details_object) = value.as_object_mut() {
        details_object.insert("outputTruncated".to_string(), Value::Bool(true));
        *details = value_to_raw(&value);
    }
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut output = value.chars().take(max_chars).collect::<String>();
    output.push_str(TRUNCATED_SUFFIX);
    output
}

pub(crate) fn truncate_chars_within_limit(value: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    if max_chars <= TRUNCATED_SUFFIX.len() {
        return value.chars().take(max_chars).collect();
    }

    let mut output = value
        .chars()
        .take(max_chars - TRUNCATED_SUFFIX.len())
        .collect::<String>();
    output.push_str(TRUNCATED_SUFFIX);
    output
}

pub(crate) fn truncate_action_result_output(
    result: &mut crate::engines::events::ActionResult,
    max_chars: usize,
) {
    let Some(output) = result.output.as_ref() else {
        return;
    };

    let truncated = truncate_chars(output, max_chars.max(1));
    if truncated != *output {
        result.output = Some(truncated);
    }
}

pub(crate) fn normalize_reasoning_effort_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

pub(crate) fn resolve_reasoning_effort_for_model(
    model: &EngineModelDto,
    requested_effort: Option<&str>,
) -> Option<String> {
    if model.supported_reasoning_efforts.is_empty() {
        return None;
    }

    let normalized_requested = normalize_reasoning_effort_value(requested_effort);
    if let Some(requested) = normalized_requested.as_ref() {
        if model
            .supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == *requested)
        {
            return Some(requested.clone());
        }
    }

    let normalized_default =
        normalize_reasoning_effort_value(Some(model.default_reasoning_effort.as_str()));
    if let Some(default_effort) = normalized_default.as_ref() {
        if model
            .supported_reasoning_efforts
            .iter()
            .any(|option| option.reasoning_effort == *default_effort)
        {
            return Some(default_effort.clone());
        }
    }

    model
        .supported_reasoning_efforts
        .iter()
        .map(|option| option.reasoning_effort.trim())
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or(normalized_default)
        .or(normalized_requested)
}

pub(crate) fn resolve_reasoning_effort_from_catalog(
    engines: &[EngineInfoDto],
    engine_id: &str,
    model_id: &str,
    requested_effort: Option<&str>,
) -> Option<String> {
    let normalized_requested = normalize_reasoning_effort_value(requested_effort);
    let Some(model) = engines
        .iter()
        .find(|engine| engine.id == engine_id)
        .and_then(|engine| engine.models.iter().find(|model| model.id == model_id))
    else {
        return normalized_requested;
    };

    resolve_reasoning_effort_for_model(model, normalized_requested.as_deref())
}



    pub(crate) async fn emit_assistant_message_completed(
    app: &AppHandle,
    state: &AppState,
    thread_id: &str,
    message_id: &str,
) {
    let message_id_for_query = message_id.to_string();
    let message = match run_db(state.db.clone(), move |db| {
        Ok(db::messages::get_message(db, &message_id_for_query)?)
    })
    .await
    {
        Ok(Some(message)) => message,
        Ok(None) => {
            log::warn!("assistant message {message_id} was not found after final persistence");
            return;
        }
        Err(error) => {
            log::warn!(
                "failed to load assistant message {message_id} after final persistence: {error}"
            );
            return;
        }
    };

    // 只有最终 completed 状态的助手消息才进入移动端事件流；流式、错误和中断消息不推送。
    if message.thread_id != thread_id
        || message.role != "assistant"
        || !matches!(message.status, MessageStatusDto::Completed)
    {
        return;
    }
    let event = AssistantMessageCompletedEvent {
        thread_id: thread_id.to_string(),
        message_id: message_id.to_string(),
        message,
    };
    let _ = app.emit("assistant-message-completed", event);
}
    pub(crate) fn build_user_blocks(
    message: &str,
    input_items: &[TurnInputItem],
    attachments: &[TurnAttachment],
    plan_mode: bool,
    is_steer: bool,
    client_steer_id: Option<&str>,
) -> Vec<ContentBlock> {
    let mut user_blocks = Vec::with_capacity(
        input_items
            .len()
            .saturating_add(attachments.len())
            .saturating_add(1),
    );

    let mut structured_text_parts: Vec<String> = Vec::new();

    for item in input_items {
        match item {
            TurnInputItem::Skill { name, path } => {
                user_blocks.push(ContentBlock::Skill {
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            TurnInputItem::Mention { name, path } => {
                user_blocks.push(ContentBlock::Mention {
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            TurnInputItem::Text { text } => {
                structured_text_parts.push(text.clone());
            }
        }
    }

    for attachment in attachments {
        user_blocks.push(ContentBlock::Attachment {
            file_name: attachment.file_name.clone(),
            file_path: attachment.file_path.clone(),
            preview_file_path: attachment.preview_file_path.clone(),
            size_bytes: attachment.size_bytes,
            mime_type: attachment.mime_type.clone(),
            browser_annotation: attachment.browser_annotation.clone(),
            is_remote: attachment.is_remote,
        });
    }

    let final_text = if structured_text_parts.is_empty() {
        message.to_string()
    } else {
        structured_text_parts.join("\n")
    };
    user_blocks.push(ContentBlock::Text {
        content: final_text,
        plan_mode: if plan_mode { Some(true) } else { None },
        is_steer: if is_steer { Some(true) } else { None },
        client_steer_id: client_steer_id.map(str::to_string),
    });

    user_blocks
}

pub(crate) async fn run_turn(
    app: tauri::AppHandle,
    state: AppState,
    thread: crate::models::ThreadDto,
    runtime_thread_id: String,
    assistant_message_id: String,
    initial_turn_model_id: String,
    turn_input: TurnInput,
    client_turn_id: Option<String>,
    cli: Arc<dyn CliTool>,
    context: CliExecutionContext,
    remote_attachment_batch: Option<RemoteAttachmentBatch>,
    cancellation: CancellationToken,
) {
    // 兼容本函数既有收尾分支，统一将传入值作为引擎内部运行键使用。
    let engine_thread_id = runtime_thread_id;
    let max_output_chars = state.config.debug.max_action_output_chars;
    log::info!(
        "chat run_turn entered: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, client_turn_id={:?}, turn_input={turn_input:?}, cli_id={}",
        thread.id,
        thread.engine_id,
        engine_thread_id,
        assistant_message_id,
        client_turn_id,
        cli.id(),
    );
    let (event_tx, mut event_rx) = mpsc::channel::<EngineEvent>(ENGINE_EVENT_QUEUE_CAPACITY);

    let db = state.db.clone();
    let thread_for_engine = thread.clone();
    let input_for_engine = turn_input.clone();
    let engine_thread_for_engine = engine_thread_id.clone();
    let engine_cancellation = CancellationToken::new();
    let cancellation_for_engine = engine_cancellation.clone();
    let cli_turn = Some((cli.clone(), context.clone()));
    let cli_turn_for_engine = cli_turn.clone();
    let assistant_message_id_for_engine = assistant_message_id.clone();

    let mut engine_task = tokio::spawn(async move {
        if let Some((cli, context)) = cli_turn_for_engine.as_ref() {
            cli.register_mcp_context(
                context,
                &engine_thread_for_engine,
                &assistant_message_id_for_engine,
            )
            .await
            .context("当前CLI无法登记AuraCoder MCP调用上下文")?;
        }
        if let Some((cli, context)) = cli_turn_for_engine {
            let send_result = cli
                .send_message(
                    &context,
                    &thread_for_engine,
                    &engine_thread_for_engine,
                    input_for_engine,
                    event_tx,
                    cancellation_for_engine,
                )
                .await;
            /*
             * 旧实现只在 send_message 返回 Err 时自行记录并取消，无法覆盖同一轮中的
             * 事件接收、超时和任务退出异常。保留原代码，统一异常处理移到 run_turn。
            if let Err(send_error) = send_result {
                log::error!(
                    "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={send_error:#}",
                    thread_for_engine.engine_id,
                    thread_for_engine.id,
                    engine_thread_for_engine,
                );
                return match cli
                    .interrupt(&context, &thread_for_engine, &engine_thread_for_engine)
                    .await
                {
                    Ok(()) => Err(anyhow::anyhow!(
                        "本轮对话执行失败，远端执行已取消：{send_error:#}"
                    )),
                    Err(interrupt_error) => {
                        log::error!(
                            "failed to interrupt chat turn after execution error: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
                            thread_for_engine.engine_id,
                            thread_for_engine.id,
                            engine_thread_for_engine,
                        );
                        Err(anyhow::anyhow!(
                            "本轮对话执行失败，并且取消远端执行失败。执行错误：{send_error:#}；取消错误：{interrupt_error:#}"
                        ))
                    }
                };
            }
            Ok(())
             */
            send_result
        } else {
            anyhow::bail!("当前 CLI 发送实例未准备完成")
        }
    });

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut action_index: HashMap<String, usize> = HashMap::new();
    let mut approval_index: HashMap<String, usize> = HashMap::new();
    let mut message_status = MessageStatusDto::Streaming;
    let mut thread_status = ThreadStatusDto::Streaming;
    let mut turn_model_id = initial_turn_model_id;
    let mut token_usage: Option<(u64, u64)> = None;
    let mut blocks_dirty = false;
    let mut message_state_dirty = false;
    let mut thread_status_dirty = false;
    let mut turn_model_dirty = false;
    let mut last_persist_at = Instant::now();
    let mut last_blocks_persist_at = Instant::now();
    let mut last_persisted_thread_status = thread_status.clone();
    let stream_event_topic = format!("stream-event-{}", thread.id);
    let approval_event_topic = format!("approval-request-{}", thread.id);
    let mut pending_event: Option<EngineEvent> = None;

    let initial_turn_started_event = EngineEvent::TurnStarted {
        client_turn_id,
        remote_turn_id: None,
    };
    let initial_progress = process_stream_event(
        &app,
        &state,
        &thread,
        &assistant_message_id,
        &stream_event_topic,
        &approval_event_topic,
        &initial_turn_started_event,
        &mut blocks,
        &mut action_index,
        &mut approval_index,
        max_output_chars,
        cli.as_ref(),
    )
    .await;
    let initial_force_persist = apply_stream_progress(
        initial_progress,
        &mut message_status,
        &mut thread_status,
        &mut turn_model_id,
        &mut token_usage,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
    );
    flush_stream_state(
        &state,
        &thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        initial_force_persist,
    )
    .await;

    /*
     * 旧实现把异常、超时、通道关闭和 CLI 的 Failed 结果统一映射为 Failed。
     * 保留旧轮次观察实现，新的业务终止信号在下方重新定义。
    enum TurnOutcome {
        Completed,
        Cancelled,
        Failed(anyhow::Error),
    }

    #[derive(Debug)]
    enum TurnWait {
        Engine(std::result::Result<anyhow::Result<()>, tokio::task::JoinError>),
        Event(Option<EngineEvent>),
        FlushPending,
        Cancelled,
        TimedOut,
    }

    let mut engine_task_finished = false;
    let mut event_channel_closed = false;
    let mut failed_completion_processed = false;
    let mut turn_outcome = 'turn_loop: loop {
        let wait = if pending_event.is_some() {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnWait::Cancelled,
                result = &mut engine_task, if !engine_task_finished => TurnWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_coalesce"), if !event_channel_closed => TurnWait::Event(event),
                _ = tokio::time::sleep(STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL) => TurnWait::FlushPending,
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnWait::TimedOut,
            }
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnWait::Cancelled,
                result = &mut engine_task, if !engine_task_finished => TurnWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_direct"), if !event_channel_closed => TurnWait::Event(event),
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnWait::TimedOut,
            }
        };

        log::info!(
            "chat run_turn wait result: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, wait={wait:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        let incoming_event = match wait {
            TurnWait::Engine(Ok(Ok(()))) => {
                engine_task_finished = true;
                if event_channel_closed {
                    break 'turn_loop TurnOutcome::Completed;
                }
                continue;
            }
            TurnWait::Engine(Ok(Err(error))) => {
                engine_task_finished = true;
                break 'turn_loop TurnOutcome::Failed(error);
            }
            TurnWait::Engine(Err(error)) => {
                engine_task_finished = true;
                break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                    "执行本轮对话的后台任务异常退出：{error}"
                ));
            }
            TurnWait::Event(Some(event)) => event,
            TurnWait::Event(None) => {
                event_channel_closed = true;
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "engine_event_channel_closed",
                    "consumer": "chat",
                    "thread_id": thread.id.clone(),
                }));
                if engine_task_finished {
                    break 'turn_loop TurnOutcome::Completed;
                }
                if matches!(message_status, MessageStatusDto::Streaming) {
                    break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                        "本轮对话尚未完成，但事件接收通道已经关闭"
                    ));
                }
                continue;
            }
            TurnWait::FlushPending => {
                if let Some(event) = pending_event.take() {
                    let progress = process_stream_event(
                        &app,
                        &state,
                        &thread,
                        &assistant_message_id,
                        &stream_event_topic,
                        &approval_event_topic,
                        &event,
                        &mut blocks,
                        &mut action_index,
                        &mut approval_index,
                        max_output_chars,
        cli.as_ref(),
                    )
                    .await;
                    let force_persist = apply_stream_progress(
                        progress,
                        &mut message_status,
                        &mut thread_status,
                        &mut turn_model_id,
                        &mut token_usage,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                    );
                    flush_stream_state(
                        &state,
                        &thread,
                        &assistant_message_id,
                        &blocks,
                        &message_status,
                        &thread_status,
                        &turn_model_id,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                        &mut last_persisted_thread_status,
                        &mut last_persist_at,
                        &mut last_blocks_persist_at,
                        force_persist,
                    )
                    .await;
                }
                continue;
            }
            TurnWait::Cancelled => break 'turn_loop TurnOutcome::Cancelled,
            TurnWait::TimedOut => {
                break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(
                    "本轮对话在15分钟内没有收到新的执行事件"
                ));
            }
        };

        log::info!(
            "chat run_turn incoming EngineEvent before processing: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, event={incoming_event:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        if let EngineEvent::Error {
            message,
            recoverable: false,
        } = &incoming_event
        {
            break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!(message.clone()));
        }

        let failed_completion = matches!(
            &incoming_event,
            EngineEvent::TurnCompleted {
                status: TurnCompletionStatus::Failed,
                ..
            }
        );

        let mut current_event = incoming_event;

        loop {
            if let Some(previous_event) = pending_event.take() {
                match try_coalesce_stream_events(previous_event, current_event) {
                    Ok(merged_event) => {
                        if coalesced_event_content_len(&merged_event)
                            >= STREAM_EVENT_COALESCE_MAX_CHARS
                        {
                            let progress = process_stream_event(
                                &app,
                                &state,
                                &thread,
                                &assistant_message_id,
                                &stream_event_topic,
                                &approval_event_topic,
                                &merged_event,
                                &mut blocks,
                                &mut action_index,
                                &mut approval_index,
                                max_output_chars,
        cli.as_ref(),
                            )
                            .await;
                            let force_persist = apply_stream_progress(
                                progress,
                                &mut message_status,
                                &mut thread_status,
                                &mut turn_model_id,
                                &mut token_usage,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                            );
                            flush_stream_state(
                                &state,
                                &thread,
                                &assistant_message_id,
                                &blocks,
                                &message_status,
                                &thread_status,
                                &turn_model_id,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                                &mut last_persisted_thread_status,
                                &mut last_persist_at,
                                &mut last_blocks_persist_at,
                                force_persist,
                            )
                            .await;
                        } else {
                            pending_event = Some(merged_event);
                        }
                        break;
                    }
                    Err((unmerged_previous_event, unmerged_current_event)) => {
                        let progress = process_stream_event(
                            &app,
                            &state,
                            &thread,
                            &assistant_message_id,
                            &stream_event_topic,
                            &approval_event_topic,
                            &unmerged_previous_event,
                            &mut blocks,
                            &mut action_index,
                            &mut approval_index,
                            max_output_chars,
        cli.as_ref(),
                        )
                        .await;
                        let force_persist = apply_stream_progress(
                            progress,
                            &mut message_status,
                            &mut thread_status,
                            &mut turn_model_id,
                            &mut token_usage,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                        );
                        flush_stream_state(
                            &state,
                            &thread,
                            &assistant_message_id,
                            &blocks,
                            &message_status,
                            &thread_status,
                            &turn_model_id,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                            &mut last_persisted_thread_status,
                            &mut last_persist_at,
                            &mut last_blocks_persist_at,
                            force_persist,
                        )
                        .await;
                        current_event = unmerged_current_event;
                    }
                }
            } else if is_coalescable_stream_event(&current_event) {
                pending_event = Some(current_event);
                break;
            } else {
                let progress = process_stream_event(
                    &app,
                    &state,
                    &thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &current_event,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
        cli.as_ref(),
                )
                .await;
                let force_persist = apply_stream_progress(
                    progress,
                    &mut message_status,
                    &mut thread_status,
                    &mut turn_model_id,
                    &mut token_usage,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                );
                flush_stream_state(
                    &state,
                    &thread,
                    &assistant_message_id,
                    &blocks,
                    &message_status,
                    &thread_status,
                    &turn_model_id,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                    &mut last_persisted_thread_status,
                    &mut last_persist_at,
                    &mut last_blocks_persist_at,
                    force_persist,
                )
                .await;
                break;
            }
        }

        if failed_completion {
            failed_completion_processed = true;
            break 'turn_loop TurnOutcome::Failed(anyhow::anyhow!("当前 CLI 报告本轮对话执行失败"));
        }
    };

    if let Some(event) = pending_event.take() {
        let progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        cli.as_ref(),
        )
        .await;
        let force_persist = apply_stream_progress(
            progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            force_persist,
        )
        .await;
    }

    if matches!(&turn_outcome, TurnOutcome::Completed)
        && matches!(message_status, MessageStatusDto::Streaming)
    {
        turn_outcome = TurnOutcome::Failed(anyhow::anyhow!(
            "当前 CLI 已结束本轮任务，但没有上报本轮完成状态"
        ));
    }
    */

    /// 本轮业务唯一允许的两个终止信号，异常和 CLI 报告只能继续观察。
    enum TurnEndSignal {
        TaskCompletedAndDelivered,
        UserClickedStop,
    }

    #[derive(Debug)]
    enum TurnObservationWait {
        Engine(std::result::Result<anyhow::Result<()>, tokio::task::JoinError>),
        Event(Option<EngineEvent>),
        FlushPending,
        UserClickedStop,
        TimedOut,
    }

    // 将执行异常转换为固定安全提示，避免原始异常进入前端消息和错误状态。
    macro_rules! emit_safe_turn_notice {
        () => {{
            if cancellation.is_cancelled() {
                log::info!(
                    "用户手动终止，抑制执行异常提示: thread_id={}, engine_id={}",
                    thread.id,
                    thread.engine_id
                );
            } else {
                let notice = EngineEvent::Notice {
                    kind: "turn_exception_observed".to_string(),
                    level: "info".to_string(),
                    title: "本轮状态待确认".to_string(),
                    message: "本轮处理遇到异常，当前执行状态待确认。".to_string(),
                    // 异常安全提示不附带原始异常或结构化后台任务元数据。
                    metadata: None,
                };
                let progress = process_stream_event(
                    &app,
                    &state,
                    &thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &notice,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
        cli.as_ref(),
                )
                .await;
                let force_persist = apply_stream_progress(
                    progress,
                    &mut message_status,
                    &mut thread_status,
                    &mut turn_model_id,
                    &mut token_usage,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                );
                flush_stream_state(
                    &state,
                    &thread,
                    &assistant_message_id,
                    &blocks,
                    &message_status,
                    &thread_status,
                    &turn_model_id,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                    &mut last_persisted_thread_status,
                    &mut last_persist_at,
                    &mut last_blocks_persist_at,
                    force_persist,
                )
                .await;
            }
        }};
    }

    let mut engine_task_finished = false;
    let mut event_channel_closed = false;
    // 仅保留既有远端附件清理条件，不参与轮次终止或消息状态判断。
    let mut remote_attachment_cleanup_required = false;
    let mut completed_turn_event: Option<EngineEvent> = None;
    let end_signal = 'turn_loop: loop {
        let wait = if pending_event.is_some() {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnObservationWait::UserClickedStop,
                result = &mut engine_task, if !engine_task_finished => TurnObservationWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_coalesce"), if !event_channel_closed => TurnObservationWait::Event(event),
                _ = tokio::time::sleep(STREAM_EVENT_COALESCE_IDLE_FLUSH_INTERVAL) => TurnObservationWait::FlushPending,
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnObservationWait::TimedOut,
            }
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => TurnObservationWait::UserClickedStop,
                result = &mut engine_task, if !engine_task_finished => TurnObservationWait::Engine(result),
                event = receive_engine_event_with_diagnostics(&mut event_rx, "chat_direct"), if !event_channel_closed => TurnObservationWait::Event(event),
                _ = tokio::time::sleep(TURN_EVENT_IDLE_TIMEOUT) => TurnObservationWait::TimedOut,
            }
        };

        log::info!(
            "chat run_turn wait result: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, wait={wait:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        let incoming_event = match wait {
            TurnObservationWait::Engine(Ok(Ok(()))) => {
                engine_task_finished = true;
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "engine_task_complete",
                    "consumer": "chat",
                    "thread_id": thread.id.clone(),
                    "engine_id": thread.engine_id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "result": "ok",
                }));
                continue;
            }
            TurnObservationWait::Engine(Ok(Err(error))) => {
                engine_task_finished = true;
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task returned error: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
                continue;
            }
            TurnObservationWait::Engine(Err(error)) => {
                engine_task_finished = true;
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task join error: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task_join",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
                continue;
            }
            TurnObservationWait::Event(Some(event)) => event,
            TurnObservationWait::Event(None) => {
                event_channel_closed = true;
                let raw_error = "引擎事件接收通道已关闭";
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "engine_event_channel_closed",
                    "consumer": "chat",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                if matches!(message_status, MessageStatusDto::Streaming) {
                    remote_attachment_cleanup_required = true;
                }
                emit_safe_turn_notice!();
                continue;
            }
            TurnObservationWait::FlushPending => {
                if let Some(event) = pending_event.take() {
                    let progress = process_stream_event(
                        &app,
                        &state,
                        &thread,
                        &assistant_message_id,
                        &stream_event_topic,
                        &approval_event_topic,
                        &event,
                        &mut blocks,
                        &mut action_index,
                        &mut approval_index,
                        max_output_chars,
        cli.as_ref(),
                    )
                    .await;
                    let force_persist = apply_stream_progress(
                        progress,
                        &mut message_status,
                        &mut thread_status,
                        &mut turn_model_id,
                        &mut token_usage,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                    );
                    flush_stream_state(
                        &state,
                        &thread,
                        &assistant_message_id,
                        &blocks,
                        &message_status,
                        &thread_status,
                        &turn_model_id,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                        &mut last_persisted_thread_status,
                        &mut last_persist_at,
                        &mut last_blocks_persist_at,
                        force_persist,
                    )
                    .await;
                }
                continue;
            }
            TurnObservationWait::UserClickedStop => {
                break 'turn_loop TurnEndSignal::UserClickedStop;
            }
            TurnObservationWait::TimedOut => {
                let raw_error = "本轮对话在15分钟内没有收到新的执行事件";
                log::warn!(
                    "chat turn observation timed out: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "timeout",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                remote_attachment_cleanup_required = true;
                emit_safe_turn_notice!();
                continue;
            }
        };

        log::info!(
            "chat run_turn incoming EngineEvent before processing: thread_id={}, engine_id={}, engine_thread_id={}, assistant_message_id={}, event={incoming_event:?}",
            thread.id,
            thread.engine_id,
            engine_thread_id,
            assistant_message_id,
        );

        let mut current_event = match &incoming_event {
            EngineEvent::Error {
                message,
                recoverable,
            } => {
                let raw_error = message.clone();
                if !*recoverable {
                    remote_attachment_cleanup_required = true;
                }
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": *recoverable,
                    "raw_error": raw_error,
                }));
                EngineEvent::Notice {
                    kind: "turn_exception_observed".to_string(),
                    level: "info".to_string(),
                    title: "本轮状态待确认".to_string(),
                    message: "本轮处理遇到异常，当前执行状态待确认。".to_string(),
                    // 异常安全提示不附带原始异常或结构化后台任务元数据。
                    metadata: None,
                }
            }
            EngineEvent::TurnCompleted {
                status: TurnCompletionStatus::Failed,
                ..
            }
            | EngineEvent::TurnCompleted {
                status: TurnCompletionStatus::Interrupted,
                ..
            } => {
                if matches!(
                    &incoming_event,
                    EngineEvent::TurnCompleted {
                        status: TurnCompletionStatus::Failed,
                        ..
                    }
                ) {
                    remote_attachment_cleanup_required = true;
                }
                let status = match &incoming_event {
                    EngineEvent::TurnCompleted { status, .. } => match status {
                        TurnCompletionStatus::Failed => "failed",
                        TurnCompletionStatus::Interrupted => "interrupted",
                        TurnCompletionStatus::Completed => "completed",
                    },
                    _ => "unknown",
                };
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_completion_observed",
                    "consumer": "chat",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "status": status,
                }));
                EngineEvent::Notice {
                    kind: "turn_exception_observed".to_string(),
                    level: "info".to_string(),
                    title: "本轮状态待确认".to_string(),
                    message: "本轮处理遇到异常，当前执行状态待确认。".to_string(),
                    // CLI 未完成报告不附带原始异常或结构化后台任务元数据。
                    metadata: None,
                }
            }
            _ => incoming_event.clone(),
        };

        loop {
            if let Some(previous_event) = pending_event.take() {
                let coalesce_result = if matches!(
                    &current_event,
                    EngineEvent::TurnCompleted {
                        status: TurnCompletionStatus::Completed,
                        ..
                    }
                ) {
                    Err((previous_event, current_event))
                } else {
                    try_coalesce_stream_events(previous_event, current_event)
                };
                match coalesce_result {
                    Ok(merged_event) => {
                        if coalesced_event_content_len(&merged_event)
                            >= STREAM_EVENT_COALESCE_MAX_CHARS
                        {
                            let progress = process_stream_event(
                                &app,
                                &state,
                                &thread,
                                &assistant_message_id,
                                &stream_event_topic,
                                &approval_event_topic,
                                &merged_event,
                                &mut blocks,
                                &mut action_index,
                                &mut approval_index,
                                max_output_chars,
        cli.as_ref(),
                            )
                            .await;
                            let force_persist = apply_stream_progress(
                                progress,
                                &mut message_status,
                                &mut thread_status,
                                &mut turn_model_id,
                                &mut token_usage,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                            );
                            flush_stream_state(
                                &state,
                                &thread,
                                &assistant_message_id,
                                &blocks,
                                &message_status,
                                &thread_status,
                                &turn_model_id,
                                &mut blocks_dirty,
                                &mut message_state_dirty,
                                &mut thread_status_dirty,
                                &mut turn_model_dirty,
                                &mut last_persisted_thread_status,
                                &mut last_persist_at,
                                &mut last_blocks_persist_at,
                                force_persist,
                            )
                            .await;
                        } else {
                            pending_event = Some(merged_event);
                        }
                        break;
                    }
                    Err((unmerged_previous_event, unmerged_current_event)) => {
                        let progress = process_stream_event(
                            &app,
                            &state,
                            &thread,
                            &assistant_message_id,
                            &stream_event_topic,
                            &approval_event_topic,
                            &unmerged_previous_event,
                            &mut blocks,
                            &mut action_index,
                            &mut approval_index,
                            max_output_chars,
        cli.as_ref(),
                        )
                        .await;
                        let force_persist = apply_stream_progress(
                            progress,
                            &mut message_status,
                            &mut thread_status,
                            &mut turn_model_id,
                            &mut token_usage,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                        );
                        flush_stream_state(
                            &state,
                            &thread,
                            &assistant_message_id,
                            &blocks,
                            &message_status,
                            &thread_status,
                            &turn_model_id,
                            &mut blocks_dirty,
                            &mut message_state_dirty,
                            &mut thread_status_dirty,
                            &mut turn_model_dirty,
                            &mut last_persisted_thread_status,
                            &mut last_persist_at,
                            &mut last_blocks_persist_at,
                            force_persist,
                        )
                        .await;
                        current_event = unmerged_current_event;
                    }
                }
            } else if matches!(
                &current_event,
                EngineEvent::TurnCompleted {
                    status: TurnCompletionStatus::Completed,
                    ..
                }
            ) {
                completed_turn_event = Some(current_event);
                break 'turn_loop TurnEndSignal::TaskCompletedAndDelivered;
            } else if is_coalescable_stream_event(&current_event) {
                pending_event = Some(current_event);
                break;
            } else {
                let progress = process_stream_event(
                    &app,
                    &state,
                    &thread,
                    &assistant_message_id,
                    &stream_event_topic,
                    &approval_event_topic,
                    &current_event,
                    &mut blocks,
                    &mut action_index,
                    &mut approval_index,
                    max_output_chars,
        cli.as_ref(),
                )
                .await;
                let force_persist = apply_stream_progress(
                    progress,
                    &mut message_status,
                    &mut thread_status,
                    &mut turn_model_id,
                    &mut token_usage,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                );
                flush_stream_state(
                    &state,
                    &thread,
                    &assistant_message_id,
                    &blocks,
                    &message_status,
                    &thread_status,
                    &turn_model_id,
                    &mut blocks_dirty,
                    &mut message_state_dirty,
                    &mut thread_status_dirty,
                    &mut turn_model_dirty,
                    &mut last_persisted_thread_status,
                    &mut last_persist_at,
                    &mut last_blocks_persist_at,
                    force_persist,
                )
                .await;
                break;
            }
        }
    };

    // 用户停止时先清空待处理流事件，再发送唯一的 Interrupted 收尾事件。
    if let Some(event) = pending_event.take() {
        let progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        cli.as_ref(),
        )
        .await;
        let force_persist = apply_stream_progress(
            progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            force_persist,
        )
        .await;
    }

    /*
     * 旧实现只在事件通道关闭后检查 engine_task，并分别处理发送错误和 JoinError。
     * 它没有统一等待事件异常、取消和超时。保留原代码，改由下方唯一出口处理。
    let mut engine_failed = false;
    match engine_task.await {
        Ok(Ok(())) => {
            log::debug!("Codex transport log: {}", serde_json::json!({
                "at": runtime_env::system_time_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "ok",
            }));
        }
        Ok(Err(error)) => {
            engine_failed = true;
            log::debug!("Codex transport log: {}", serde_json::json!({
                "at": runtime_env::system_time_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "error",
                "error": error.to_string(),
            }));
            blocks.push(ContentBlock::Error {
                message: format!("Engine error: {error:#}"),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
            let _ = app.emit(
                &stream_event_topic,
                EngineEvent::Error {
                    message: format!("{error:#}"),
                    recoverable: false,
                },
            );
        }
        Err(error) => {
            engine_failed = true;
            let interrupt_result = if let Some((cli, context)) = cli_turn_for_join_error {
                cli.interrupt(
                    &context,
                    &thread_for_join_error,
                    &engine_thread_for_join_error,
                )
                .await
            } else {
                engines_for_join_error
                    .interrupt(&thread_for_join_error)
                    .await
            };
            let error_message = match interrupt_result {
                Ok(()) => format!("Engine task join error，远端执行已取消: {error}"),
                Err(interrupt_error) => {
                    log::error!(
                        "failed to interrupt chat turn after join error: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
                        thread_for_join_error.engine_id,
                        thread_for_join_error.id,
                        engine_thread_for_join_error,
                    );
                    format!(
                        "Engine task join error，并且取消远端执行失败: {error}；取消错误：{interrupt_error:#}"
                    )
                }
            };
            log::debug!("Codex transport log: {}", serde_json::json!({
                "at": runtime_env::system_time_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "join_error",
                "error": error.to_string(),
            }));
            blocks.push(ContentBlock::Error {
                message: error_message.clone(),
            });
            blocks_dirty = true;
            if message_status != MessageStatusDto::Error {
                message_status = MessageStatusDto::Error;
                message_state_dirty = true;
            }
            if thread_status != ThreadStatusDto::Error {
                thread_status = ThreadStatusDto::Error;
                thread_status_dirty = true;
            }
            let _ = app.emit(
                &stream_event_topic,
                EngineEvent::Error {
                    message: error_message,
                    recoverable: false,
                },
            );
        }
    }
     */

    /*
     * 旧实现的失败终态、错误消息、Error 状态和失败/Interrupted 伪造事件全部保留。
     * 新业务只允许两个终止信号，并在下方统一完成最终事件收尾。
    let initial_failure_message = match &turn_outcome {
        TurnOutcome::Failed(error) => {
            Some(format!("本轮对话执行失败，正在取消远端执行：{error:#}"))
        }
        TurnOutcome::Completed | TurnOutcome::Cancelled => None,
    };
    if let Some(error_message) = initial_failure_message {
        log::error!(
            "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={error_message}",
            thread.engine_id,
            thread.id,
            engine_thread_id,
        );
        log::debug!("Codex transport log: {}", serde_json::json!({
            "at": runtime_env::system_time_rfc3339(),
            "event": "engine_task_complete",
            "consumer": "chat",
            "thread_id": thread.id.clone(),
            "result": "error",
            "error": error_message.clone(),
        }));
        blocks.push(ContentBlock::Error {
            message: error_message.clone(),
        });
        blocks_dirty = true;
        if message_status != MessageStatusDto::Error {
            message_status = MessageStatusDto::Error;
            message_state_dirty = true;
        }
        if thread_status != ThreadStatusDto::Error {
            thread_status = ThreadStatusDto::Error;
            thread_status_dirty = true;
        }
        let _ = app.emit(
            &stream_event_topic,
            EngineEvent::Error {
                message: error_message,
                recoverable: false,
            },
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            true,
        )
        .await;
    }

    let must_interrupt = !matches!(&turn_outcome, TurnOutcome::Completed);
    let interrupt_result = if must_interrupt {
        Some(
            match tokio::time::timeout(TURN_INTERRUPT_TIMEOUT, async {
                if let Some((cli, context)) = cli_turn.as_ref() {
                    cli.interrupt(context, &thread, &engine_thread_id).await
                } else {
                    Err(anyhow::anyhow!("当前线程未关联 CLI 执行上下文"))
                }
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("取消远端执行超过10秒仍未完成")),
            },
        )
    } else {
        None
    };

    if must_interrupt {
        engine_cancellation.cancel();
        if !engine_task_finished {
            if tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task)
                .await
                .is_err()
            {
                engine_task.abort();
                let _ = engine_task.await;
            }
        }
    }

    if let Some((cli, context)) = cli_turn.as_ref() {
        if let Err(error) = cli.clear_mcp_context(context).await {
            log::error!(
                "清理当前 CLI AuraCoder MCP 调用上下文失败: engine_id={}, thread_id={}, engine_thread_id={}, error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
        }
    }

    let interrupt_error = interrupt_result.and_then(Result::err);
    if let Some(interrupt_error) = &interrupt_error {
        log::error!(
            "failed to interrupt chat turn: engine_id={}, thread_id={}, engine_thread_id={}, error={interrupt_error:#}",
            thread.engine_id,
            thread.id,
            engine_thread_id,
        );
        if let Some((_, context)) = cli_turn.as_ref() {
            if context.location_kind == CliLocationKind::Ssh {
                if let Some(connection_id) = context.ssh_connection_id.as_ref() {
                    let _ = app.emit(
                        "chat-cli-service-restart-required",
                        CliServiceRestartRequiredEvent {
                            thread_id: thread.id.clone(),
                            workspace_id: thread.workspace_id.clone(),
                            engine_id: thread.engine_id.clone(),
                            thread_title: thread.title.clone(),
                            connection_id: connection_id.clone(),
                            reason: format!("{interrupt_error:#}"),
                        },
                    );
                }
            }
        }
    }
    let mut engine_failed = matches!(&turn_outcome, TurnOutcome::Failed(_));
    let turn_failure_message = match turn_outcome {
        TurnOutcome::Completed => {
            log::debug!("Codex transport log: {}", serde_json::json!({
                "at": runtime_env::system_time_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "ok",
            }));
            None
        }
        TurnOutcome::Cancelled => {
            interrupt_error.map(|error| format!("本轮对话已停止，但取消远端执行失败：{error:#}"))
        }
        TurnOutcome::Failed(error) => {
            /*
             * 旧实现直到 interrupt 返回后才记录本轮错误并通知界面。远端取消卡住时，
             * 用户也会一直看不到错误。日志和首次界面反馈已移到 interrupt 之前。
            log::error!(
                "chat turn failed: engine_id={}, thread_id={}, engine_thread_id={}, error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
            log::debug!("Codex transport log: {}", serde_json::json!({
                "at": runtime_env::system_time_rfc3339(),
                "event": "engine_task_complete",
                "consumer": "chat",
                "thread_id": thread.id.clone(),
                "result": "error",
                "error": error.to_string(),
            }));
             */
            match interrupt_error {
                Some(interrupt_error) => Some(format!(
                    "本轮对话执行失败，并且取消远端执行失败。执行错误：{error:#}；取消错误：{interrupt_error:#}"
                )),
                None => {
                    let notice = EngineEvent::Notice {
                        kind: "remote_interrupt_completed".to_string(),
                        level: "info".to_string(),
                        title: "远端任务已终止".to_string(),
                        message: "本轮异常对应的远端执行已经取消。".to_string(),
                        // 远端中断提示不附带结构化后台任务元数据。
                        metadata: None,
                    };
                    let progress = process_stream_event(
                        &app,
                        &state,
                        &thread,
                        &assistant_message_id,
                        &stream_event_topic,
                        &approval_event_topic,
                        &notice,
                        &mut blocks,
                        &mut action_index,
                        &mut approval_index,
                        max_output_chars,
        cli.as_ref(),
                    )
                    .await;
                    apply_stream_progress(
                        progress,
                        &mut message_status,
                        &mut thread_status,
                        &mut turn_model_id,
                        &mut token_usage,
                        &mut blocks_dirty,
                        &mut message_state_dirty,
                        &mut thread_status_dirty,
                        &mut turn_model_dirty,
                    );
                    None
                }
            }
        }
    };

    // engine_failed 已在统一异常出口根据 TurnOutcome 初始化。
    if let Some(error_message) = turn_failure_message {
        engine_failed = true;
        blocks.push(ContentBlock::Error {
            message: error_message.clone(),
        });
        blocks_dirty = true;
        if message_status != MessageStatusDto::Error {
            message_status = MessageStatusDto::Error;
            message_state_dirty = true;
        }
        if thread_status != ThreadStatusDto::Error {
            thread_status = ThreadStatusDto::Error;
            thread_status_dirty = true;
        }
        let _ = app.emit(
            &stream_event_topic,
            EngineEvent::Error {
                message: error_message,
                recoverable: false,
            },
        );
    }

    if engine_failed && !failed_completion_processed {
        let failed_event = EngineEvent::TurnCompleted {
            token_usage: None,
            status: TurnCompletionStatus::Failed,
        };
        let failed_progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &failed_event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        cli.as_ref(),
        )
        .await;
        apply_stream_progress(
            failed_progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
    }

    if cancellation.is_cancelled() && matches!(message_status, MessageStatusDto::Streaming) {
        let interrupted_event = EngineEvent::TurnCompleted {
            token_usage: None,
            status: TurnCompletionStatus::Interrupted,
        };
        let interrupted_progress = process_stream_event(
            &app,
            &state,
            &thread,
            &assistant_message_id,
            &stream_event_topic,
            &approval_event_topic,
            &interrupted_event,
            &mut blocks,
            &mut action_index,
            &mut approval_index,
            max_output_chars,
        cli.as_ref(),
        )
        .await;
        let interrupted_force_persist = apply_stream_progress(
            interrupted_progress,
            &mut message_status,
            &mut thread_status,
            &mut turn_model_id,
            &mut token_usage,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
        );
        flush_stream_state(
            &state,
            &thread,
            &assistant_message_id,
            &blocks,
            &message_status,
            &thread_status,
            &turn_model_id,
            &mut blocks_dirty,
            &mut message_state_dirty,
            &mut thread_status_dirty,
            &mut turn_model_dirty,
            &mut last_persisted_thread_status,
            &mut last_persist_at,
            &mut last_blocks_persist_at,
            interrupted_force_persist,
        )
        .await;
    }

    if cancellation.is_cancelled() && matches!(message_status, MessageStatusDto::Streaming) {
        message_status = MessageStatusDto::Interrupted;
        message_state_dirty = true;
        thread_status = ThreadStatusDto::Idle;
        thread_status_dirty = true;
    }

    flush_stream_state(
        &state,
        &thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        true,
    )
    .await;

    if engine_failed {
        if let Some(batch) = remote_attachment_batch.as_ref() {
            batch.cleanup().await;
        }
    }
    state.turns.finish(&thread.id).await;
    */

    // 只有正常交付或用户点击停止后，才进入唯一的最终收尾事件。
    let user_clicked_stop = matches!(&end_signal, TurnEndSignal::UserClickedStop);
    let interrupt_result = if user_clicked_stop {
        Some(
            match tokio::time::timeout(TURN_INTERRUPT_TIMEOUT, async {
                if let Some((cli, context)) = cli_turn.as_ref() {
                    cli.interrupt(context, &thread, &engine_thread_id).await
                } else {
                    Err(anyhow::anyhow!("当前线程未关联 CLI 执行上下文"))
                }
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("取消远端执行超过10秒仍未完成")),
            },
        )
    } else {
        None
    };

    // 用户停止时不再取消发送任务的 CancellationToken，避免触发第二个取消执行者。
    if user_clicked_stop && !engine_task_finished {
        engine_task_finished = true;
        match tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task returned error during stop cleanup: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
            }
            Ok(Err(error)) => {
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task join error during stop cleanup: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task_join",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
            }
            Err(_) => {
                let raw_error = "用户停止后后台任务未在清理窗口内结束";
                log::warn!(
                    "chat engine task cleanup timed out: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task_cleanup_timeout",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
                engine_task.abort();
                let _ = (&mut engine_task).await;
            }
        }
    }

    // 正常完成候选可能早于发送任务返回，先等待任务闭合再清理 CLI 上下文。
    if !user_clicked_stop && !engine_task_finished {
        engine_task_finished = true;
        let engine_task_result =
            match tokio::time::timeout(TURN_TASK_CLEANUP_TIMEOUT, &mut engine_task).await {
                Ok(result) => Some(result),
                Err(_) => {
                    remote_attachment_cleanup_required = true;
                    let raw_error = "正常完成后后台任务未在清理窗口内结束";
                    log::warn!(
                        "chat engine task cleanup timed out after completion report: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                        thread.engine_id,
                        thread.id,
                        engine_thread_id,
                    );
                    log::debug!("Codex transport log: {}", serde_json::json!({
                        "at": runtime_env::system_time_rfc3339(),
                        "event": "turn_exception_observed",
                        "consumer": "chat",
                        "failure_kind": "engine_task_cleanup_timeout",
                        "engine_id": thread.engine_id.clone(),
                        "thread_id": thread.id.clone(),
                        "engine_thread_id": engine_thread_id.clone(),
                        "recoverable": false,
                        "raw_error": raw_error,
                    }));
                    emit_safe_turn_notice!();
                    engine_task.abort();
                    let _ = (&mut engine_task).await;
                    None
                }
            };
        if let Some(engine_task_result) = engine_task_result {
            match engine_task_result {
            Ok(Ok(())) => {
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "engine_task_complete",
                    "consumer": "chat",
                    "thread_id": thread.id.clone(),
                    "engine_id": thread.engine_id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "result": "ok",
                }));
            }
            Ok(Err(error)) => {
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task returned error after completion report: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
            }
            Err(error) => {
                remote_attachment_cleanup_required = true;
                let raw_error = error.to_string();
                log::error!(
                    "chat engine task join error after completion report: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
                    thread.engine_id,
                    thread.id,
                    engine_thread_id,
                );
                log::debug!("Codex transport log: {}", serde_json::json!({
                    "at": runtime_env::system_time_rfc3339(),
                    "event": "turn_exception_observed",
                    "consumer": "chat",
                    "failure_kind": "engine_task_join",
                    "engine_id": thread.engine_id.clone(),
                    "thread_id": thread.id.clone(),
                    "engine_thread_id": engine_thread_id.clone(),
                    "recoverable": false,
                    "raw_error": raw_error,
                }));
                emit_safe_turn_notice!();
            }
        }
        }
    }

    // 引擎任务已完成并清理后读取真实外部 ID，覆盖 SessionInit/TurnCompleted 的消费竞态。
    let external_engine_thread_id_result = if let Some((cli, context)) = cli_turn.as_ref() {
        cli.current_external_engine_thread_id(context, &thread, &engine_thread_id)
            .await
    } else {
        Err(anyhow::anyhow!("当前线程未关联 CLI 执行上下文"))
    };
    match external_engine_thread_id_result {
        Ok(Some(external_engine_thread_id)) if !external_engine_thread_id.trim().is_empty() => {
            if thread.engine_thread_id.as_deref() != Some(external_engine_thread_id.as_str()) {
                let thread_id = thread.id.clone();
                // 为数据库异步闭包复制真实外部 ID，保留原值用于失败日志输出。
                let external_engine_thread_id_for_db = external_engine_thread_id.clone();
                if let Err(error) = run_db(db.clone(), move |db| {
                    db::threads::set_engine_thread_id(
                        db,
                        &thread_id,
                        &external_engine_thread_id_for_db,
                    )
                })
                .await
                {
                    log::error!(
                        "持久化 Claude 真实外部 session ID 失败: engine_id={}, thread_id={}, runtime_thread_id={}, external_engine_thread_id={}, raw_error={error:#}",
                        thread.engine_id,
                        thread.id,
                        engine_thread_id,
                        external_engine_thread_id,
                    );
                }
            }
        }
        Ok(Some(_)) | Ok(None) => {}
        Err(error) => {
            log::error!(
                "读取引擎真实外部 session ID 失败: engine_id={}, thread_id={}, runtime_thread_id={}, raw_error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
        }
    }

    if let Some((cli, context)) = cli_turn.as_ref() {
        if let Err(error) = cli.clear_mcp_context(context).await {
            log::error!(
                "清理当前 CLI AuraCoder MCP 调用上下文失败: engine_id={}, thread_id={}, engine_thread_id={}, error={error:#}",
                thread.engine_id,
                thread.id,
                engine_thread_id,
            );
        }
    }

    if let Some(interrupt_error) = interrupt_result.as_ref().and_then(|result| result.as_ref().err()) {
        let raw_error = interrupt_error.to_string();
        log::error!(
            "failed to interrupt chat turn: engine_id={}, thread_id={}, engine_thread_id={}, raw_error={raw_error}",
            thread.engine_id,
            thread.id,
            engine_thread_id,
        );
        log::debug!("Codex transport log: {}", serde_json::json!({
            "at": runtime_env::system_time_rfc3339(),
            "event": "turn_exception_observed",
            "consumer": "chat",
            "failure_kind": "interrupt",
            "engine_id": thread.engine_id.clone(),
            "thread_id": thread.id.clone(),
            "engine_thread_id": engine_thread_id.clone(),
            "recoverable": false,
            "raw_error": raw_error.clone(),
        }));
        emit_safe_turn_notice!();
        if let Some((_, context)) = cli_turn.as_ref() {
            if context.location_kind == CliLocationKind::Ssh {
                if let Some(connection_id) = context.ssh_connection_id.as_ref() {
                    let _ = app.emit(
                        "chat-cli-service-restart-required",
                        CliServiceRestartRequiredEvent {
                            thread_id: thread.id.clone(),
                            workspace_id: thread.workspace_id.clone(),
                            engine_id: thread.engine_id.clone(),
                            thread_title: thread.title.clone(),
                            connection_id: connection_id.clone(),
                            reason: "本轮执行状态待确认，请检查远端 CLI 服务。".to_string(),
                        },
                    );
                }
            }
        }
    }

    let final_event = match end_signal {
        TurnEndSignal::TaskCompletedAndDelivered => completed_turn_event.unwrap_or(
            EngineEvent::TurnCompleted {
                token_usage: None,
                status: TurnCompletionStatus::Completed,
            },
        ),
        TurnEndSignal::UserClickedStop => EngineEvent::TurnCompleted {
            token_usage: None,
            status: TurnCompletionStatus::Interrupted,
        },
    };
    let final_progress = process_stream_event(
        &app,
        &state,
        &thread,
        &assistant_message_id,
        &stream_event_topic,
        &approval_event_topic,
        &final_event,
        &mut blocks,
        &mut action_index,
        &mut approval_index,
        max_output_chars,
        cli.as_ref(),
    )
    .await;
    let final_force_persist = apply_stream_progress(
        final_progress,
        &mut message_status,
        &mut thread_status,
        &mut turn_model_id,
        &mut token_usage,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
    );
    flush_stream_state(
        &state,
        &thread,
        &assistant_message_id,
        &blocks,
        &message_status,
        &thread_status,
        &turn_model_id,
        &mut blocks_dirty,
        &mut message_state_dirty,
        &mut thread_status_dirty,
        &mut turn_model_dirty,
        &mut last_persisted_thread_status,
        &mut last_persist_at,
        &mut last_blocks_persist_at,
        final_force_persist,
    )
    .await;

    if remote_attachment_cleanup_required {
        if let Some(batch) = remote_attachment_batch.as_ref() {
            batch.cleanup().await;
        }
    }
    state.turns.finish(&thread.id).await;

    let assistant_message_persisted = match run_db(state.db.clone(), {
        let assistant_message_id = assistant_message_id.clone();
        let message_status = message_status.clone();
        let token_usage = token_usage;
        move |db| {
            db::messages::complete_assistant_message(
                db,
                &assistant_message_id,
                message_status,
                token_usage,
                Some(turn_model_id.as_str()),
            )
        }
    })
    .await
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!("failed to complete assistant message: {error}");
            false
        }
    };

    if assistant_message_persisted && matches!(message_status, MessageStatusDto::Completed) {
        emit_assistant_message_completed(&app, &state, &thread.id, &assistant_message_id).await;
    }

    if matches!(message_status, MessageStatusDto::Completed) {
        if let Err(error) = run_db(state.db.clone(), {
            let thread_id = thread.id.clone();
            let token_usage = token_usage;
            move |db| db::threads::bump_message_counters(db, &thread_id, token_usage)
        })
        .await
        {
            log::warn!("failed to bump thread counters: {error}");
        }
    }

    let _ = maybe_update_thread_title(
        &state,
        cli.as_ref(),
        &context,
        &thread,
        &engine_thread_id,
        &turn_input.message,
    )
    .await;

    let latest_thread = run_db(state.db.clone(), {
        let thread_id = thread.id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await
    .unwrap_or_else(|error| {
        log::warn!("failed to load thread before final thread-updated emit: {error}");
        None
    });

    let (thread_updated_event, final_thread) = build_final_thread_event(latest_thread, &thread);
    let _ = app.emit("thread-updated", thread_updated_event);
    finish_scheduled_task_run(
        &app,
        &state,
        &assistant_message_id,
        &message_status,
        &blocks,
    )
    .await;
    if let Some(final_thread) = final_thread.as_ref() {
        emit_chat_turn_finished(&app, final_thread, &message_status, &blocks);
    }
}

    /// 按当前CLI返回的模型能力校验附件格式与模态。
pub(crate) fn validate_attachments_for_engine_model(
    attachments: &[TurnAttachment],
    engine_id: &str,
    model_id: &str,
    catalog: Option<&[EngineInfoDto]>,
) -> Result<(), String> {
    if attachments.is_empty() {
        return Ok(());
    }

    let Some(model) = catalog
        .and_then(|engines| engines.iter().find(|engine| engine.id == engine_id))
        .and_then(|engine| engine.models.iter().find(|model| model.id == model_id))
    else {
        return Ok(());
    };

    let allowed_modalities = if model.attachment_modalities.is_empty() {
        HashSet::new()
    } else {
        model
            .attachment_modalities
            .iter()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<HashSet<_>>()
    };

    if allowed_modalities.is_empty() {
        return Err(format!(
            "{} does not support file attachments.",
            model.display_name
        ));
    }

    for attachment in attachments {
        let Some(modality) = attachment_modality(attachment) else {
            return Err(format!(
                "{} is not a supported attachment type for {}.",
                attachment.file_name, model.display_name
            ));
        };
        if !allowed_modalities.contains(modality) {
            return Err(format!(
                "{} attachments are not supported by {}.",
                attachment_modality_label(modality),
                model.display_name
            ));
        }
    }

    Ok(())
}

pub(crate) fn attachment_modality(attachment: &TurnAttachment) -> Option<&'static str> {
    let extension = Path::new(&attachment.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_lowercase);
    let mime_type = attachment
        .mime_type
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase);

    if extension.as_deref() == Some("pdf") || mime_type.as_deref() == Some("application/pdf") {
        return Some("pdf");
    }
    if extension
        .as_deref()
        .map(|value| IMAGE_ATTACHMENT_EXTENSIONS.contains(&value))
        .unwrap_or(false)
        || mime_type
            .as_deref()
            .map(|value| value.starts_with("image/"))
            .unwrap_or(false)
    {
        return Some("image");
    }
    if extension
        .as_deref()
        .map(|value| TEXT_ATTACHMENT_EXTENSIONS.contains(&value))
        .unwrap_or(false)
        || mime_type
            .as_deref()
            .map(is_text_attachment_mime_type)
            .unwrap_or(false)
    {
        return Some("text");
    }

    None
}

pub(crate) fn is_text_attachment_mime_type(value: &str) -> bool {
    value.starts_with("text/")
        || matches!(
            value,
            "application/json"
                | "application/javascript"
                | "application/typescript"
                | "application/xml"
                | "application/x-sh"
                | "application/x-yaml"
                | "application/yaml"
                | "text/csv"
        )
}

pub(crate) fn attachment_modality_label(modality: &str) -> &'static str {
    match modality {
        "image" => "Image",
        "pdf" => "PDF",
        _ => "Text file",
    }
}

    /// 校验本轮模型切换是否被当前CLI模型目录支持。
pub(crate) fn resolve_turn_model_id(
    thread: &ThreadDto,
    requested_model_id: Option<&str>,
    engines: Option<&[EngineInfoDto]>,
) -> Result<String, String> {
    let Some(requested_model_id) = requested_model_id else {
        return Ok(thread.model_id.clone());
    };

    if requested_model_id == thread.model_id {
        return Ok(thread.model_id.clone());
    }

    if let Some(engines) = engines {
        if let Some(engine) = engines.iter().find(|engine| engine.id == thread.engine_id) {
            if engine
                .models
                .iter()
                .any(|model| model.id == requested_model_id)
            {
                return Ok(requested_model_id.to_string());
            }

            let available = engine
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "model `{requested_model_id}` is not supported by engine `{}`. available models: {available}",
                thread.engine_id
            ));
        }
    }

    Ok(requested_model_id.to_string())
}

    /// 将底层聊天操作异常记入日志并转换为统一中文业务错误。
    fn chat_operation_error(
        code: &str,
        message: &str,
        error: impl std::fmt::Display,
    ) -> ChatBusinessError {
        log::error!("聊天业务操作失败: {error:#}");
        ChatBusinessError::new(code, message)
    }

    /// 按统一响应内容映射需要持久化到审批记录的最终决定。
    pub(crate) fn approval_response_decision_for_persistence(response: &Value) -> &'static str {
        if let Some(decision) = response.get("decision").and_then(Value::as_str) {
            return match decision {
                "deny" | "decline" => "decline",
                "acceptForSession" | "accept_for_session" => "accept_for_session",
                "accept" => "accept",
                "cancel" => "cancel",
                _ => "custom",
            };
        }

        if let Some(action) = response.get("action").and_then(Value::as_str) {
            return match action {
                "accept" => "accept",
                "decline" => "decline",
                "cancel" => "cancel",
                _ => "custom",
            };
        }

        if response.get("permissions").is_some() {
            if permission_profile_is_empty(response.get("permissions")) {
                return "decline";
            }
            if response.get("scope").and_then(Value::as_str) == Some("session") {
                return "accept_for_session";
            }
            return "accept";
        }

        "custom"
    }

    /// 判断单个权限值是否授予了至少一项权限。
    fn permission_value_has_grant(value: &Value) -> bool {
        match value {
            Value::Bool(granted) => *granted,
            Value::String(value) => !value.trim().is_empty() && value != "none",
            Value::Array(items) => items.iter().any(permission_value_has_grant),
            Value::Object(fields) => fields.values().any(permission_value_has_grant),
            _ => false,
        }
    }

    /// 判断权限对象是否没有任何已授予的权限值。
    pub(crate) fn permission_profile_is_empty(value: Option<&Value>) -> bool {
        match value {
            Some(value) => !permission_value_has_grant(value),
            None => true,
        }
    }

    /// 按审批记录读取最终持久化结果，供原生事件先结算的竞态返回真实状态。
    async fn read_settled_chat_approval_outcome(
        state: &AppState,
        thread_id: &str,
        approval_id: &str,
    ) -> Result<Option<ApprovalResponseOutcomeDto>, ChatBusinessError> {
        let db = state.db.clone();
        let thread_id_owned = thread_id.to_string();
        let approval_id_owned = approval_id.to_string();
        let persisted = run_db(db.clone(), move |db| {
            let Some(record) = db::actions::get_chat_approval_record(db, &approval_id_owned)? else {
                return Ok(None);
            };
            if record.status == "pending" || record.thread_id != thread_id_owned {
                return Ok(None);
            }
            let thread = db::threads::get_thread(db, &record.thread_id)?;
            let message = db::messages::get_message(db, &record.message_id)?;
            Ok(Some((record, thread, message)))
        })
        .await
        .map_err(|error| {
            chat_operation_error(
                "PERSISTENCE_FAILED",
                "读取审批已保存结果失败",
                error,
            )
        })?;

        let Some((record, Some(thread), Some(message))) = persisted else {
            return Ok(None);
        };
        Ok(Some(ApprovalResponseOutcomeDto {
            approval_id: approval_id.to_string(),
            status: record.status,
            decision: record.decision,
            thread,
            message: Some(message),
            warning: None,
        }))
    }

    /// 原子保存审批记录、消息块和线程状态，并检查是否已由其他事件结算。
    async fn settle_chat_approval(
        state: &AppState,
        thread_id: &str,
        approval_id: &str,
        status: &str,
        decision: Option<&str>,
        thread_status: ThreadStatusDto,
        message_status: Option<MessageStatusDto>,
    ) -> Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
        let db = state.db.clone();
        let thread_id_owned = thread_id.to_string();
        let approval_id_owned = approval_id.to_string();
        let status_owned = status.to_string();
        let decision_owned = decision.map(str::to_string);
        match run_db(db, move |db| {
            db::actions::settle_chat_approval(
                db,
                &thread_id_owned,
                &approval_id_owned,
                &status_owned,
                decision_owned.as_deref(),
                thread_status,
                message_status,
            )
        })
        .await
        {
            Ok((thread, message)) => Ok(ApprovalResponseOutcomeDto {
                approval_id: approval_id.to_string(),
                status: status.to_string(),
                decision: decision.map(str::to_string),
                thread,
                message: Some(message),
                warning: None,
            }),
            Err(error) => {
                log::error!("保存聊天审批结果失败: {error:#}");
                match read_settled_chat_approval_outcome(state, thread_id, approval_id).await {
                    Ok(Some(outcome)) => Err(ChatBusinessError::new(
                        "APPROVAL_NOT_PENDING",
                        "该审批已由其他处理完成，请刷新后查看实际结果",
                    )
                    .with_outcome(outcome)),
                    Ok(None) => Err(ChatBusinessError::new(
                        "PERSISTENCE_FAILED",
                        "审批结果未能保存",
                    )),
                    Err(read_error) => {
                        log::error!("重读聊天审批实际结果失败: {}", read_error.message);
                        Err(ChatBusinessError::new(
                            "PERSISTENCE_FAILED",
                            "审批结果未能保存",
                        ))
                    }
                }
            }
        }
    }

    /// 根据其他挂起审批和助手消息状态决定结算审批后的线程状态。
    async fn chat_approval_thread_status(
        state: &AppState,
        thread_id: &str,
        message_id: &str,
        approval_id: &str,
        orphan_cancel_candidate: bool,
    ) -> Result<(ThreadStatusDto, Option<MessageStatusDto>), ChatBusinessError> {
        let db = state.db.clone();
        let thread_id_owned = thread_id.to_string();
        let message_id_owned = message_id.to_string();
        let approval_id = approval_id.to_string();
        let (other_pending, current_message, latest_assistant_message_id, current_thread) = run_db(db, move |db| {
            let other_pending = db::actions::count_other_pending_chat_approvals(
                db,
                &thread_id_owned,
                &approval_id,
            )?;
            let current_message = db::messages::get_message(db, &message_id_owned)?;
            let latest_assistant_message_id =
                db::messages::latest_chat_assistant_message_id(db, &thread_id_owned)?;
            let current_thread = db::threads::get_thread(db, &thread_id_owned)?;
            Ok((
                other_pending,
                current_message,
                latest_assistant_message_id,
                current_thread,
            ))
        })
        .await
        .map_err(|error| {
            chat_operation_error(
                "PERSISTENCE_FAILED",
                "读取线程待处理审批状态失败",
                error,
            )
        })?;
        let current_message = current_message.ok_or_else(|| {
            ChatBusinessError::new("PERSISTENCE_FAILED", "读取审批消息最新状态失败")
        })?;
        let current_thread = current_thread.ok_or_else(|| {
            ChatBusinessError::new("PERSISTENCE_FAILED", "读取审批线程最新状态失败")
        })?;
        if current_message.thread_id != thread_id {
            return Err(ChatBusinessError::new(
                "PERSISTENCE_FAILED",
                "审批消息的线程归属发生变化",
            ));
        }
        if latest_assistant_message_id.as_deref() != Some(message_id) {
            return Ok((current_thread.status, None));
        }

        // 重启后缺失本地回合令牌时，只把仍为最新助手消息的孤儿 Streaming 回合收为 Interrupted。
        let current_turn_registered = state.turns.get(thread_id).await.is_some();
        if orphan_cancel_candidate
            && !current_turn_registered
            && current_message.status == MessageStatusDto::Streaming
        {
            return Ok((
                ThreadStatusDto::Idle,
                Some(MessageStatusDto::Interrupted),
            ));
        }
        if other_pending > 0 {
            return Ok((ThreadStatusDto::AwaitingApproval, None));
        }

        // 当前仍登记的回合保持运行态，包括取消令牌已标记但 run_turn 尚未完成收尾的阶段。
        if current_turn_registered {
            return Ok((ThreadStatusDto::Streaming, None));
        }

        Ok((match current_message.status {
            MessageStatusDto::Completed => ThreadStatusDto::Completed,
            MessageStatusDto::Interrupted => ThreadStatusDto::Idle,
            MessageStatusDto::Error => ThreadStatusDto::Error,
            MessageStatusDto::Streaming => ThreadStatusDto::Streaming,
        }, None))
    }

    /// 等待指定取消令牌对应的回合完成清理，不延长等待到后续新回合。
    async fn wait_for_cancelled_chat_turn_cleanup(
        state: &AppState,
        thread_id: &str,
    ) -> bool {
        tokio::time::timeout(
            TURN_INTERRUPT_TIMEOUT + TURN_TASK_CLEANUP_TIMEOUT + Duration::from_secs(2),
            async {
                loop {
                    match state.turns.get(thread_id).await {
                        Some(active_turn) if active_turn.is_cancelled() => {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        _ => return,
                    }
                }
            },
        )
        .await
        .is_err()
    }

    /// 读取线程与审批归属，提交当前 CLI 答复并原子保存真实审批状态。
    async fn answer_chat_approval_inner(
        state: AppState,
        cli: Arc<dyn CliTool>,
        request: ApprovalResponseRequest,
    ) -> Result<ApprovalResponseOutcomeDto, ChatBusinessError> {
        if request.thread_id.trim().is_empty() || request.approval_id.trim().is_empty() {
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "审批线程或审批标识不能为空",
            ));
        }

        // 先读取审批归属，确保答复不会被转发到其他线程或 CLI。
        let db = state.db.clone();
        let approval_id = request.approval_id.clone();
        let record = run_db(db, move |db| db::actions::get_chat_approval_record(db, &approval_id))
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "读取审批记录失败", error)
            })?
            .ok_or_else(|| {
                ChatBusinessError::new("APPROVAL_NOT_PENDING", "该审批不存在或已失效")
            })?;
        let thread = ThreadService::new(state.clone())
            .get_thread(&request.thread_id)
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "读取审批所属线程失败", error)
            })?;
        if record.thread_id != request.thread_id || thread.id != record.thread_id {
            return Err(ChatBusinessError::new(
                "APPROVAL_MISMATCH",
                "审批记录与当前线程不匹配",
            ));
        }
        if thread.engine_id != cli.id() {
            return Err(ChatBusinessError::new(
                "APPROVAL_MISMATCH",
                "审批记录与当前聊天工具不匹配",
            ));
        }

        // 已回答的取消重复请求直接返回保存结果，避免再次触达原生运行时。
        if record.status != "pending" {
            if record.status == "answered" && record.decision.as_deref() == Some("cancel") {
                let normalized = cli.normalize_chat_approval(request.response.clone())?;
                if approval_response_decision_for_persistence(&normalized) == "cancel" {
                    if let Some(outcome) =
                        read_settled_chat_approval_outcome(&state, &thread.id, &request.approval_id)
                            .await?
                    {
                        return Ok(outcome);
                    }
                }
            }
            let outcome = read_settled_chat_approval_outcome(
                &state,
                &thread.id,
                &request.approval_id,
            )
            .await?;
            let error = ChatBusinessError::new(
                "APPROVAL_NOT_PENDING",
                "该审批已处理或失效，请刷新后重试",
            );
            return match outcome {
                Some(outcome) => Err(error.with_outcome(outcome)),
                None => Err(error),
            };
        }

        // 将统一审批选择按当前 CLI 归一化，并映射为稳定的本地决定值。
        let normalized_response = cli.normalize_chat_approval(request.response)?;
        let decision = approval_response_decision_for_persistence(&normalized_response);
        let (approval_message, latest_assistant_message_id) = {
            let db = state.db.clone();
            let thread_id = thread.id.clone();
            let approval_message_id = record.message_id.clone();
            run_db(db, move |db| {
                let approval_message = db::messages::get_message(db, &approval_message_id)?;
                let latest_assistant_message_id =
                    db::messages::latest_chat_assistant_message_id(db, &thread_id)?;
                Ok((approval_message, latest_assistant_message_id))
            })
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "读取审批消息失败", error)
            })?
        };
        let Some(approval_message) = approval_message else {
            return Err(ChatBusinessError::new(
                "APPROVAL_MISMATCH",
                "审批关联消息不存在",
            ));
        };
        if approval_message.role != "assistant" {
            return Err(ChatBusinessError::new(
                "APPROVAL_MISMATCH",
                "审批必须关联助手消息",
            ));
        }
        if approval_message.thread_id != thread.id {
            return Err(ChatBusinessError::new(
                "APPROVAL_MISMATCH",
                "审批关联消息不属于当前线程",
            ));
        }

        // 仅当审批属于当前活动助手消息时捕获该回合令牌，避免旧审批取消新回合。
        let approval_is_current_assistant = latest_assistant_message_id.as_deref()
            == Some(approval_message.id.as_str())
            && approval_message.status == MessageStatusDto::Streaming;
        let active_turn = if decision == "cancel" && approval_is_current_assistant {
            state.turns.get(&thread.id).await
        } else {
            None
        };
        // 原生处理前重读审批状态，避免重复答复已由并发事件完成的审批。
        let current_record = run_db(state.db.clone(), {
            let approval_id = request.approval_id.clone();
            move |db| db::actions::get_chat_approval_record(db, &approval_id)
        })
        .await
        .map_err(|error| {
            chat_operation_error("CHAT_OPERATION_FAILED", "重新核验审批状态失败", error)
        })?
        .ok_or_else(|| {
            ChatBusinessError::new("APPROVAL_NOT_PENDING", "该审批不存在或已失效")
        })?;
        if current_record.status != "pending" {
            let outcome = read_settled_chat_approval_outcome(
                &state,
                &thread.id,
                &request.approval_id,
            )
            .await?;
            let error = ChatBusinessError::new(
                "APPROVAL_NOT_PENDING",
                "该审批已由其他处理完成，请刷新后查看实际结果",
            );
            return match outcome {
                Some(outcome)
                    if outcome.status == "answered"
                        && outcome.decision.as_deref() == Some("cancel")
                        && decision == "cancel" => Ok(outcome),
                Some(outcome) => Err(error.with_outcome(outcome)),
                None => Err(error),
            };
        }

        // 将归一化后的响应发送给本次线程所属的 CLI，并保留其结构化失败分类。
        let native_result: Result<(), ChatBusinessError> = async {
            // 原生路由、项目上下文和 CLI 请求组成同一准备阶段，取消场景失败也会继续本地清理。
            let route = cli.chat_approval_route(&record.details)?;
            let workspace = run_db(state.db.clone(), {
                let workspace_id = thread.workspace_id.clone();
                move |db| {
                    db::workspaces::find_workspace_by_id(db, &workspace_id)?
                        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                }
            })
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "读取审批项目配置失败", error)
            })?;
            let context = cli
                .execution_context(Some(&workspace.id))
                .await
                .map_err(|error| cli.classify_chat_approval_error(&error))?;
            cli.respond_to_approval(
                &context,
                &thread,
                &request.approval_id,
                normalized_response,
                route,
            )
            .await
            .map_err(|error| cli.classify_chat_approval_error(&error))
        }
        .await;
        let native_error = native_result.err();

        if decision != "cancel" {
            if let Some(error) = native_error {
                if error.code != "APPROVAL_NOT_PENDING" {
                    return Err(error);
                }

                // 仅 CLI 明确判定审批已失效时，将三处本地审批状态原子标记为 expired。
                let (thread_status, message_status) = chat_approval_thread_status(
                    &state,
                    &thread.id,
                    &approval_message.id,
                    &request.approval_id,
                    false,
                )
                .await?;
                let outcome = settle_chat_approval(
                    &state,
                    &thread.id,
                    &request.approval_id,
                    "expired",
                    None,
                    thread_status,
                    message_status,
                )
                .await?;
                return Err(error.with_outcome(outcome));
            }

            // 正常答复只在原生接受后才统一结算本地审批和线程状态。
            let (thread_status, message_status) = chat_approval_thread_status(
                &state,
                &thread.id,
                &approval_message.id,
                &request.approval_id,
                false,
            )
            .await?;
            return settle_chat_approval(
                &state,
                &thread.id,
                &request.approval_id,
                "answered",
                Some(decision),
                thread_status,
                message_status,
            )
            .await;
        }

        // 取消本地对应回合并等待其持有的 CLI 完成清理，超时仍保留真实 Streaming 状态。
        let orphan_cancel_candidate = active_turn.is_none()
            && approval_is_current_assistant
            && approval_message.status == MessageStatusDto::Streaming;
        let mut local_cancel_requested = false;
        let mut cleanup_timed_out = false;
        if let Some(cancellation) = active_turn {
            local_cancel_requested = true;
            cancellation.cancel();
            if wait_for_cancelled_chat_turn_cleanup(&state, &thread.id).await {
                log::warn!(
                    "等待聊天审批取消后的回合清理超时: thread_id={}, approval_id={}",
                    thread.id,
                    request.approval_id
                );
                cleanup_timed_out = true;
            }
        }
        let (thread_status, message_status) = chat_approval_thread_status(
            &state,
            &thread.id,
            &approval_message.id,
            &request.approval_id,
            orphan_cancel_candidate,
        )
        .await?;
        if message_status == Some(MessageStatusDto::Interrupted) {
            local_cancel_requested = true;
        }
        let mut warning = native_error.as_ref().map(|error| {
            if local_cancel_requested {
                ChatBusinessWarning {
                    code: if error.code == "CLI_UNAVAILABLE" {
                        "CLI_UNAVAILABLE".to_string()
                    } else {
                        "CHAT_OPERATION_FAILED".to_string()
                    },
                    message: format!("审批取消已在本地保存，但远端未确认取消：{}", error.message),
                }
            } else {
                ChatBusinessWarning {
                    code: "CHAT_OPERATION_FAILED".to_string(),
                    message: format!("审批远端取消失败，未中断当前回合：{}", error.message),
                }
            }
        });
        if cleanup_timed_out {
            let code = warning
                .as_ref()
                .filter(|warning| warning.code == "CLI_UNAVAILABLE")
                .map(|warning| warning.code.clone())
                .unwrap_or_else(|| "CHAT_OPERATION_FAILED".to_string());
            let message = match native_error.as_ref() {
                Some(error) => format!(
                    "审批取消已在本地保存，但本地回合仍在收尾，远端状态未确认：{}",
                    error.message
                ),
                None => "审批取消已保存，但本地回合仍在收尾，当前线程状态尚未确认".to_string(),
            };
            warning = Some(ChatBusinessWarning { code, message });
        }
        let mut outcome = match settle_chat_approval(
            &state,
            &thread.id,
            &request.approval_id,
            "answered",
            Some("cancel"),
            thread_status,
            message_status,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(mut error) => {
                if let Some(outcome) = error.outcome.as_mut() {
                    if outcome.status == "answered"
                        && outcome.decision.as_deref() == Some("cancel")
                    {
                        outcome.warning = warning;
                    } else if local_cancel_requested || warning.is_some() {
                        outcome.warning = Some(ChatBusinessWarning {
                            code: "CHAT_OPERATION_FAILED".to_string(),
                            message: if local_cancel_requested {
                                "本地回合已请求停止，但该审批已由其他处理结算；未覆盖其已保存决定".to_string()
                            } else {
                                "该审批已由其他处理结算，已返回实际保存结果".to_string()
                            },
                        });
                    }
                }
                return Err(error);
            }
        };
        outcome.warning = warning;
        Ok(outcome)
    }

    /// 保存普通消息和本轮配置后，通过固定的 CLI 实例启动流式回合。
    async fn submit_chat_message_inner(
        state: AppState,
        cli: Arc<dyn CliTool>,
        app: AppHandle,
        request: SendMessageRequest,
    ) -> Result<String, ChatBusinessError> {
        let SendMessageRequest {
            thread_id,
            message,
            engine_id,
            model_id,
            plan_mode,
            send_method,
            reasoning_effort,
            permission_values,
            attachments,
            input_items,
            client_turn_id,
            scheduled_run_id,
            referenced_thread_id,
        } = request;

        if state.turns.get(&thread_id).await.is_some() {
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "本线程已有正在执行的回合，请先停止后再发送消息",
            ));
        }

        let db = state.db.clone();
        let mut thread = run_db(db.clone(), {
            let thread_id = thread_id.clone();
            move |db| db::threads::get_thread(db, &thread_id)
        })
        .await
        .map_err(|error| chat_operation_error("CHAT_OPERATION_FAILED", "读取聊天线程失败", error))?
        .ok_or_else(|| {
            ChatBusinessError::new("CHAT_OPERATION_FAILED", "聊天线程不存在或已被删除")
        })?;

        let original_thread_model_id = thread.model_id.clone();
        let selected_engine_id = engine_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(thread.engine_id.as_str());
        if selected_engine_id != cli.id() {
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "提交的聊天工具与线程配置不一致",
            ));
        }
        let selected_model_id = model_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(thread.model_id.as_str())
            .to_string();
        let selected_plan_mode = plan_mode.unwrap_or(false);

        // 保持权限对象A先保存到ThreadService，再由当前CLI沿既有权限解析链使用。
        if let Some(values) = permission_values {
            thread = ThreadService::new(state.clone())
                .save_permissions(&thread.id, values)
                .await
                .map_err(|error| {
                    chat_operation_error(
                        "CHAT_OPERATION_FAILED",
                        "保存线程权限设置失败",
                        error,
                    )
                })?;
        }
        thread = run_db(db.clone(), {
            let thread_id = thread.id.clone();
            let engine_id = cli.id().to_string();
            let model_id = selected_model_id.clone();
            let send_method = send_method.clone();
            let reasoning_effort = reasoning_effort.clone();
            move |db| {
                db::threads::update_thread_runtime_selection(
                    db,
                    &thread_id,
                    &engine_id,
                    &model_id,
                    Some(selected_plan_mode),
                    send_method.as_deref(),
                    reasoning_effort.as_deref(),
                    None,
                )
            }
        })
        .await
        .map_err(|error| {
            chat_operation_error("CHAT_OPERATION_FAILED", "保存本轮聊天设置失败", error)
        })?;

        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await
        .map_err(|error| {
            chat_operation_error("CHAT_OPERATION_FAILED", "读取聊天项目配置失败", error)
        })?;
        let context = cli
            .execution_context(Some(&workspace.id))
            .await
            .map_err(|error| chat_operation_error("CLI_UNAVAILABLE", "当前聊天工具暂不可用", error))?;
        cli.acquire_turn(&context, &thread)
            .await
            .map_err(|error| chat_operation_error("CLI_UNAVAILABLE", "当前聊天工具暂不可用", error))?;

        let referenced_thread = crate::services::chat_service::resolve_auracoder_thread_reference(
            &state,
            &thread,
            referenced_thread_id.as_deref(),
        )
        .await
        .map_err(|error| {
            chat_operation_error("VALIDATION_FAILED", "引用的聊天线程无效", error)
        })?;
        let auracoder_thread_instruction = referenced_thread
            .as_ref()
            .map(crate::services::chat_service::build_auracoder_thread_instruction);
        let requested_model_id = model_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let model_switch_requested = requested_model_id
            .as_deref()
            .is_some_and(|value| value != original_thread_model_id);
        let mut attachments = crate::services::chat_service::normalize_attachments(attachments).map_err(|error| {
            chat_operation_error("VALIDATION_FAILED", "聊天附件参数不正确", error)
        })?;
        let input_items = crate::services::chat_service::normalize_input_items(&message, input_items).map_err(
            |error| chat_operation_error("VALIDATION_FAILED", "聊天输入内容不正确", error),
        )?;
        let browser_annotation_context = crate::services::chat_service::browser_annotation_context(&attachments);
        let mut engine_message = crate::services::chat_service::format_engine_message(
            &message,
            &browser_annotation_context,
        );
        let mut engine_input_items = crate::services::chat_service::append_browser_annotation_context(
            &input_items,
            &browser_annotation_context,
        );
        if let Some(instruction) = auracoder_thread_instruction.as_deref() {
            engine_message.push_str(instruction);
            engine_input_items.push(TurnInputItem::Text {
                text: instruction.to_string(),
            });
        }

        // 通过当前CLI自身目录读取模型和附件能力，避免跨CLI回退。
        let engine_info = cli
            .get_engine_info(&context)
            .await
            .map_err(|error| chat_operation_error("CLI_UNAVAILABLE", "读取聊天模型信息失败", error))?;
        let validation_catalog = vec![engine_info];
        let effective_model_id = resolve_turn_model_id(
            &thread,
            requested_model_id.as_deref(),
            Some(&validation_catalog),
        )
        .map_err(|error| {
            chat_operation_error("VALIDATION_FAILED", "当前聊天模型不可用", error)
        })?;
        validate_attachments_for_engine_model(
            &attachments,
            &thread.engine_id,
            &effective_model_id,
            Some(&validation_catalog),
        )
        .map_err(|error| {
            chat_operation_error("VALIDATION_FAILED", "当前模型不支持所选附件", error)
        })?;

        let requested_reasoning_effort = normalize_reasoning_effort_value(reasoning_effort.as_deref());
        let stored_reasoning_effort = thread.reasoning_effort.clone();
        let configured_reasoning_effort = requested_reasoning_effort
            .clone()
            .or_else(|| stored_reasoning_effort.clone());
        let selected_reasoning_effort = if requested_reasoning_effort.is_some() {
            requested_reasoning_effort
        } else if model_switch_requested {
            resolve_reasoning_effort_from_catalog(
                &validation_catalog,
                &thread.engine_id,
                &effective_model_id,
                configured_reasoning_effort.as_deref(),
            )
        } else {
            configured_reasoning_effort
        };
        let workspace_root = workspace.root_path.clone();
        let scope = ThreadScope::Project {
            root_path: workspace_root.clone(),
            writable_roots: vec![workspace_root.clone()],
        };
        if requested_model_id.is_some() || selected_reasoning_effort != stored_reasoning_effort {
            let update = ThreadUpdateDto {
                id: thread.id.clone(),
                model_id: requested_model_id,
                reasoning_effort: selected_reasoning_effort.clone(),
                ..Default::default()
            };
            thread = run_db(db.clone(), move |db| db::threads::update_thread(db, &update))
                .await
                .map_err(|error| {
                    chat_operation_error("CHAT_OPERATION_FAILED", "保存聊天模型设置失败", error)
                })?;
        }
        let writable_roots = match &scope {
            ThreadScope::Project {
                writable_roots,
                root_path,
            } if writable_roots.is_empty() => vec![root_path.clone()],
            ThreadScope::Project { writable_roots, .. } => writable_roots.clone(),
        };
        let model_supports_personality = validation_catalog
            .iter()
            .find(|engine| engine.id == cli.id())
            .and_then(|engine| {
                engine
                    .models
                    .iter()
                    .find(|model| model.id == effective_model_id)
            })
            .is_some_and(|model| model.supports_personality);
        let sandbox = cli.chat_sandbox(
            &thread,
            writable_roots,
            selected_reasoning_effort.clone(),
            model_supports_personality,
        );

        // 使用当前CLI实例准备本地会话，再按同一实例发起后续消息。
        let engine_thread = cli
            .start_thread(
                &context,
                &thread,
                scope,
                thread.engine_thread_id.as_deref(),
                &effective_model_id,
                sandbox,
            )
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "准备聊天会话失败", error)
            })?;
        let runtime_thread_id = engine_thread.runtime_thread_id.clone();
        if let Some(external_engine_thread_id) = engine_thread
            .external_engine_thread_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if thread.engine_thread_id.as_deref() != Some(external_engine_thread_id) {
                let thread_id = thread.id.clone();
                let external_engine_thread_id = external_engine_thread_id.to_string();
                let external_engine_thread_id_for_db = external_engine_thread_id.clone();
                run_db(db.clone(), move |db| {
                    db::threads::set_engine_thread_id(
                        db,
                        &thread_id,
                        &external_engine_thread_id_for_db,
                    )
                })
                .await
                .map_err(|error| {
                    chat_operation_error(
                        "CHAT_OPERATION_FAILED",
                        "保存聊天会话标识失败",
                        error,
                    )
                })?;
                thread.engine_thread_id = Some(external_engine_thread_id);
            }
        }
        /*
        state.auracoder_thread_mcp_service.bind_engine_thread(
            cli.id(),
            &runtime_thread_id,
            &thread.workspace_id,
        );
        */

        let persistent_image_preview_batch =
            crate::services::chat_service::prepare_message_image_previews(&mut attachments)
                .await
                .map_err(|error| {
                    chat_operation_error(
                        "VALIDATION_FAILED",
                        "无法准备聊天图片预览，消息未发送",
                        error,
                    )
                })?;
        let remote_attachment_batch = if matches!(context.location_kind, super::CliLocationKind::Ssh)
            && !attachments.is_empty()
        {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .ok_or_else(|| {
                    ChatBusinessError::new("VALIDATION_FAILED", "远端项目未绑定SSH连接")
                })?
                .to_string();
            let connection = run_db(db.clone(), move |db| {
                db::ssh_connections::find(db, &connection_id)?
                    .ok_or_else(|| anyhow::anyhow!("SSH connection not found: {connection_id}"))
            })
            .await
            .map_err(|error| {
                chat_operation_error("CHAT_OPERATION_FAILED", "读取SSH连接失败", error)
            })?;
            Some(
                crate::ssh::remote_attachments::upload_turn_attachments(
                    &connection,
                    &workspace.id,
                    &thread.id,
                    &attachments,
                )
                .await
                .map_err(|error| {
                    chat_operation_error("CHAT_OPERATION_FAILED", "上传聊天附件失败", error)
                })?,
            )
        } else {
            None
        };
        let attachments = remote_attachment_batch
            .as_ref()
            .map(|batch| batch.attachments.clone())
            .unwrap_or(attachments);
        let turn_input = TurnInput {
            message: engine_message,
            attachments: attachments.clone(),
            plan_mode: selected_plan_mode,
            input_items: engine_input_items,
            permissions: None,
        };
        let cancellation = CancellationToken::new();
        if !state
            .turns
            .try_register(&thread.id, cancellation.clone())
            .await
        {
            if let Some(batch) = remote_attachment_batch.as_ref() {
                batch.cleanup().await;
            }
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "本线程已有正在执行的回合，请先停止后再发送消息",
            ));
        }

        let (assistant_message, streaming_thread) = match run_db(db.clone(), {
            let thread_id = thread.id.clone();
            let message = message.clone();
            let attachments = attachments.clone();
            let input_items = input_items.clone();
            let plan_mode_enabled = selected_plan_mode;
            let engine_id = thread.engine_id.clone();
            let model_id = effective_model_id.clone();
            let reasoning_effort = selected_reasoning_effort.clone();
            let scheduled_run_id = scheduled_run_id.clone();
            move |db| {
                let user_blocks = build_user_blocks(
                    &message,
                    &input_items,
                    &attachments,
                    plan_mode_enabled,
                    false,
                    None,
                );
                db::messages::insert_user_message(
                    db,
                    &thread_id,
                    &message,
                    Some(serde_json::to_value(&user_blocks)?),
                    Some(&engine_id),
                    Some(&model_id),
                    reasoning_effort.as_deref(),
                )?;
                let assistant_message = db::messages::insert_assistant_placeholder(
                    db,
                    &thread_id,
                    Some(&engine_id),
                    Some(&model_id),
                    reasoning_effort.as_deref(),
                )?;
                db::threads::update_thread_status(db, &thread_id, ThreadStatusDto::Streaming)?;
                let streaming_thread = db::threads::get_thread(db, &thread_id)?.ok_or_else(|| {
                    anyhow::anyhow!("thread missing after starting chat: {thread_id}")
                })?;
                if let Some(run_id) = scheduled_run_id.as_deref() {
                    db::scheduled_tasks::mark_run_started(
                        db,
                        run_id,
                        &thread_id,
                        &assistant_message.id,
                    )?;
                }
                Ok((assistant_message, streaming_thread))
            }
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Some(batch) = remote_attachment_batch.as_ref() {
                    batch.cleanup().await;
                }
                state.turns.finish(&thread.id).await;
                return Err(chat_operation_error(
                    "CHAT_OPERATION_FAILED",
                    "保存聊天消息失败，消息未发送",
                    error,
                ));
            }
        };
        persistent_image_preview_batch.commit();
        let _ = app.emit(
            "thread-updated",
            ThreadUpdatedEvent {
                thread_id: streaming_thread.id.clone(),
                workspace_id: streaming_thread.workspace_id.clone(),
                thread: Some(streaming_thread),
            },
        );
        let app_handle = app.clone();
        let state_for_task = state.clone();
        let thread_for_task = thread.clone();
        let assistant_message_id = assistant_message.id.clone();
        let initial_turn_model_id = effective_model_id;
        tokio::spawn(async move {
            run_turn(
                app_handle,
                state_for_task,
                thread_for_task,
                runtime_thread_id,
                assistant_message_id,
                initial_turn_model_id,
                turn_input,
                client_turn_id,
                cli,
                context,
                remote_attachment_batch,
                cancellation,
            )
            .await;
        });
        Ok(assistant_message.id)
    }

    /// 按当前线程归属向既有CLI回合追加消息并保存服务端接收回执。
    async fn submit_chat_steer_inner(
        state: AppState,
        cli: Arc<dyn CliTool>,
        app: AppHandle,
        request: SteerMessageRequest,
    ) -> Result<crate::models::SteerReceiptDto, ChatBusinessError> {
        let _app_handle = app;
        let client_steer_id = request
            .client_steer_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ChatBusinessError::new("VALIDATION_FAILED", "追加消息缺少幂等标识")
            })?
            .to_string();
        let steer_started_at = Instant::now();
        if state.turns.get(&request.thread_id).await.is_none() {
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "当前线程没有正在执行的回合，无法追加消息",
            ));
        }

        let db = state.db.clone();
        let mut thread = run_db(db.clone(), {
            let thread_id = request.thread_id.clone();
            move |db| db::threads::get_thread(db, &thread_id)
        })
        .await
        .map_err(|error| chat_operation_error("CHAT_OPERATION_FAILED", "读取聊天线程失败", error))?
        .ok_or_else(|| ChatBusinessError::new("CHAT_OPERATION_FAILED", "聊天线程不存在或已删除"))?;
        if thread.engine_id != cli.id() {
            return Err(ChatBusinessError::new(
                "VALIDATION_FAILED",
                "提交的聊天工具与线程配置不一致",
            ));
        }

        // 保持追加请求中的权限对象A先经过ThreadService保存，再由当前CLI转换为原生权限B。
        if let Some(permission_values) = request.permission_values.clone() {
            thread = ThreadService::new(state.clone())
                .save_permissions(&thread.id, permission_values)
                .await
                .map_err(|error| {
                    chat_operation_error(
                        "CHAT_OPERATION_FAILED",
                        "保存线程权限设置失败",
                        error,
                    )
                })?;
        }
        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await
        .map_err(|error| chat_operation_error("CHAT_OPERATION_FAILED", "读取项目配置失败", error))?;
        let context = cli
            .execution_context(Some(&workspace.id))
            .await
            .map_err(|error| chat_operation_error("CLI_UNAVAILABLE", "当前聊天工具暂不可用", error))?;
        let engine_thread_id = thread.engine_thread_id.clone().ok_or_else(|| {
            ChatBusinessError::new("VALIDATION_FAILED", "当前线程尚未建立CLI会话")
        })?;
        let mut attachments = crate::services::chat_service::normalize_attachments(
            request.attachments.clone(),
        )
        .map_err(|error| chat_operation_error("VALIDATION_FAILED", "聊天附件参数不正确", error))?;
        let referenced_thread = crate::services::chat_service::resolve_auracoder_thread_reference(
            &state,
            &thread,
            request.referenced_thread_id.as_deref(),
        )
        .await
        .map_err(|error| chat_operation_error("VALIDATION_FAILED", "引用的聊天线程无效", error))?;
        let persistent_image_preview_batch =
            crate::services::chat_service::prepare_message_image_previews(&mut attachments)
                .await
                .map_err(|error| {
                    chat_operation_error(
                        "VALIDATION_FAILED",
                        "无法准备聊天图片预览，追加消息未发送",
                        error,
                    )
                })?;
        let remote_attachment_batch = if matches!(context.location_kind, super::CliLocationKind::Ssh)
            && !attachments.is_empty()
        {
            let connection_id = context
                .ssh_connection_id
                .as_deref()
                .ok_or_else(|| ChatBusinessError::new("VALIDATION_FAILED", "远端项目未绑定SSH连接"))?
                .to_string();
            let connection = run_db(db.clone(), move |db| {
                db::ssh_connections::find(db, &connection_id)?
                    .ok_or_else(|| anyhow::anyhow!("SSH connection not found: {connection_id}"))
            })
            .await
            .map_err(|error| chat_operation_error("CHAT_OPERATION_FAILED", "读取SSH连接失败", error))?;
            Some(
                crate::ssh::remote_attachments::upload_turn_attachments(
                    &connection,
                    &workspace.id,
                    &thread.id,
                    &attachments,
                )
                .await
                .map_err(|error| chat_operation_error("CHAT_OPERATION_FAILED", "上传聊天附件失败", error))?,
            )
        } else {
            None
        };
        let attachments = remote_attachment_batch
            .as_ref()
            .map(|batch| batch.attachments.clone())
            .unwrap_or(attachments);
        let input_items = crate::services::chat_service::normalize_input_items(
            &request.message,
            request.input_items.clone(),
        )
        .map_err(|error| chat_operation_error("VALIDATION_FAILED", "聊天输入内容不正确", error))?;
        let plan_mode = request.plan_mode.unwrap_or(false);
        let browser_annotation_context =
            crate::services::chat_service::browser_annotation_context(&attachments);
        let mut engine_message = crate::services::chat_service::format_engine_message(
            &request.message,
            &browser_annotation_context,
        );
        let mut engine_input_items =
            crate::services::chat_service::append_browser_annotation_context(
                &input_items,
                &browser_annotation_context,
            );
        if let Some(reference) = referenced_thread.as_ref() {
            let instruction = crate::services::chat_service::build_auracoder_thread_instruction(
                reference,
            );
            engine_message.push_str(&instruction);
            engine_input_items.push(TurnInputItem::Text { text: instruction });
        }
        let turn_input = TurnInput {
            message: engine_message.clone(),
            attachments: attachments.clone(),
            plan_mode,
            input_items: engine_input_items,
            permissions: None,
        };
        let user_blocks = build_user_blocks(
            &request.message,
            &input_items,
            &attachments,
            plan_mode,
            true,
            Some(&client_steer_id),
        );
        let user_message = match run_db(db.clone(), {
            let thread_id = thread.id.clone();
            let message = request.message.clone();
            let engine_id = thread.engine_id.clone();
            let model_id = thread.model_id.clone();
            let reasoning_effort = thread.reasoning_effort.clone();
            move |db| {
                db::messages::insert_user_message(
                    db,
                    &thread_id,
                    &message,
                    Some(serde_json::to_value(&user_blocks)?),
                    Some(&engine_id),
                    Some(&model_id),
                    reasoning_effort.as_deref(),
                )
            }
        })
        .await
        {
            Ok(user_message) => user_message,
            Err(error) => {
                if let Some(batch) = remote_attachment_batch.as_ref() {
                    batch.cleanup().await;
                }
                return Err(chat_operation_error(
                    "CHAT_OPERATION_FAILED",
                    "保存追加消息失败，消息未发送",
                    error,
                ));
            }
        };

        /*
        state.auracoder_thread_mcp_service.bind_engine_thread(
            cli.id(),
            &engine_thread_id,
            &thread.workspace_id,
        );
        */
        let receipt = match cli
            .steer_message(
                &context,
                &thread,
                &engine_thread_id,
                &client_steer_id,
                &engine_message,
                turn_input,
            )
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Some(batch) = remote_attachment_batch.as_ref() {
                    batch.cleanup().await;
                }
                let message_id = user_message.id.clone();
                match run_db(db.clone(), move |db| db::messages::delete_message(db, &message_id))
                    .await
                {
                    Ok(()) => {}
                    Err(rollback_error) => {
                        log::error!("回滚失败的追加消息时数据库操作失败: {rollback_error:#}");
                        persistent_image_preview_batch.commit();
                    }
                }
                return Err(chat_operation_error(
                    "CHAT_OPERATION_FAILED",
                    "追加消息提交失败",
                    error,
                ));
            }
        };
        persistent_image_preview_batch.commit();
        let accepted_at = runtime_env::system_time_rfc3339();
        log::info!(
            "聊天追加消息已被CLI接收: thread_id={}, engine_thread_id={}, client_steer_id={}, expected_turn_id={}, elapsed_ms={}",
            thread.id,
            engine_thread_id,
            client_steer_id,
            receipt.expected_turn_id,
            steer_started_at.elapsed().as_millis(),
        );
        Ok(crate::models::SteerReceiptDto {
            client_steer_id,
            expected_turn_id: receipt.expected_turn_id,
            accepted_at,
        })
    }
}
