use std::sync::Arc;

use anyhow::Result;

use super::{
    claude_code::ClaudeCodeCli,
    codex::CodexCli,
    opencode::OpenCodeCli,
    CliLocationKind,
    CliMcpRuntime,
    CliTool,
};
use crate::state::AppState;

/// 按 CLI 工具名称创建对应的统一 CLI 业务接口实现。
pub struct CliToolFactory {
    state: AppState,
}

impl CliToolFactory {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 按 CLI 标识创建普通业务调用使用的统一 CLI 接口实现。
    pub fn create(&self, cli_id: &str) -> Result<Arc<dyn CliTool>> {
        match cli_id {
            "codex" => Ok(Arc::new(CodexCli::new(self.state.clone()))),
            "opencode" => Ok(Arc::new(OpenCodeCli::new(self.state.clone()))),
            "claude" => Ok(Arc::new(ClaudeCodeCli::new(self.state.clone()))),
            _ => anyhow::bail!("不支持的 CLI 工具: {cli_id}"),
        }
    }

    /// 按指定 CLI 和固定运行位置创建可供 Gateway 直接调用的 MCP CLI 实现。
    pub(crate) fn create_mcp_cli(
        &self,
        cli_id: &str,
        runtime: CliMcpRuntime,
    ) -> Result<Arc<dyn CliTool>> {
        anyhow::ensure!(
            runtime.cli_id == cli_id,
            "MCP CLI runtime 标识与请求 CLI 不一致: {} != {cli_id}",
            runtime.cli_id
        );
        anyhow::ensure!(
            matches!(runtime.location, CliLocationKind::Local | CliLocationKind::Ssh),
            "不支持的 MCP CLI 运行位置"
        );
        match cli_id {
            "codex" => Ok(Arc::new(CodexCli::with_mcp_runtime(
                self.state.clone(),
                runtime,
            ))),
            "opencode" => Ok(Arc::new(OpenCodeCli::with_mcp_runtime(
                self.state.clone(),
                runtime,
            ))),
            "claude" => Ok(Arc::new(ClaudeCodeCli::with_mcp_runtime(
                self.state.clone(),
                runtime,
            ))),
            _ => anyhow::bail!("不支持的 CLI 工具: {cli_id}"),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/factory_tests.rs"]
mod tests;
