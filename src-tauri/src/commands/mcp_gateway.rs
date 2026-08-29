use serde::Serialize;
use tauri::State;

use crate::{mcp_gateway::GatewayState, state::AppState};

/// 向前端返回 MCP Gateway 的最小业务可见状态。
#[derive(Debug, Clone, Serialize)]
pub struct McpServiceStatusDto {
    /// Gateway 运行时状态：仅 Running 映射为 normal，其余状态均为 abnormal。
    pub state: &'static str,
}

/// 查询真实 MCP Gateway 生命周期并映射为前端需要的 normal/abnormal 状态。
#[tauri::command]
pub async fn get_mcp_service_status(
    state: State<'_, AppState>,
) -> Result<McpServiceStatusDto, String> {
    let gateway_state = state.mcp_gateway.status().await;
    let state = if gateway_state == GatewayState::Running {
        "normal"
    } else {
        "abnormal"
    };
    Ok(McpServiceStatusDto { state })
}
