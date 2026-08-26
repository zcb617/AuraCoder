use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// 前端用于刷新 SSH 远端项目会话列表的事件名称。
pub const SSH_REMOTE_PROJECT_SESSIONS_REFRESHED_EVENT: &str =
    "ssh-remote-project-sessions-refreshed";

/// 前端启动界面用于显示后端初始化进度的事件名称。
pub const APP_STARTUP_PROGRESS_EVENT: &str = "app-startup-progress";

/// 前端用于刷新 CLI 目录缓存的事件名称。
pub const CLI_SERVICES_UPDATED_EVENT: &str = "cli-services-updated";

/// 后端初始化阶段发生变化时发送给前端的事件载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStartupProgressEvent {
    pub phase: String,
    pub message: String,
}

/// SSH 远端项目会话刷新完成后发送给前端的事件载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteProjectSessionsRefreshedEvent {
    /// 本次刷新所属的工作区标识。
    pub workspace_id: String,
    /// 刷新成功的远端 CLI 标识列表。
    pub succeeded_cli_ids: Vec<String>,
    /// 刷新失败的远端 CLI 标识列表。
    pub failed_cli_ids: Vec<String>,
}

/// 一次 CLI 健康检查对生命周期 MAP 的处理结果。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliHealthReconcileResult {
    /// 本次检查是否已经成功增删至少一项生命周期登记。
    pub changed: bool,
    /// 阻止某项登记变化完成的异常；为空表示检查过程没有异常。
    pub errors: Vec<String>,
}

/// CLI 生命周期 MAP 被健康检查 reconcile 后发送给前端的事件载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliServicesUpdatedEvent {
    /// 发生变化的范围：local 表示本机，ssh 表示指定远端连接。
    pub scope: String,
    /// scope 为 ssh 时的 SSH 连接配置标识。
    pub connection_id: Option<String>,
    /// 单调递增的事件序号，前端用于识别乱序事件。
    pub revision: u64,
    /// 本次健康检查是否已经成功改变生命周期 MAP。
    pub changed: bool,
    /// 健康检查异常的明确业务信号；为空表示检查过程没有异常。
    pub errors: Vec<String>,
}

/// 向前端发送 CLI 目录更新事件。
///
/// 调用方必须先完成生命周期 MAP 的 reconcile 再调用本函数，保证前端收到事件后
/// 立即拉取时读到的是新状态；本函数只负责把固定契约发送给 Tauri 前端。
pub fn notify_cli_services_updated(
    app: &AppHandle,
    event: CliServicesUpdatedEvent,
) -> anyhow::Result<()> {
    app.emit(CLI_SERVICES_UPDATED_EVENT, event)
        .context("failed to emit CLI services updated event")
}

/// 向前端发送 SSH 远端项目会话刷新完成事件。
///
/// 调用方应当只在数据库事务成功提交后调用此函数；本函数不承担任何
/// 数据库、会话刷新或通知展示职责，只负责把固定契约发送给 Tauri 前端。
pub fn notify_ssh_remote_project_sessions_refreshed(
    app: &AppHandle,
    event: SshRemoteProjectSessionsRefreshedEvent,
) -> anyhow::Result<()> {
    app.emit(SSH_REMOTE_PROJECT_SESSIONS_REFRESHED_EVENT, event)
        .context("failed to emit SSH remote project sessions refreshed event")
}

/// 向前端启动界面发送当前初始化阶段。
pub fn notify_app_startup_progress(
    app: &AppHandle,
    phase: &str,
    message: &str,
) -> anyhow::Result<()> {
    app.emit(
        APP_STARTUP_PROGRESS_EVENT,
        AppStartupProgressEvent {
            phase: phase.to_string(),
            message: message.to_string(),
        },
    )
    .context("failed to emit app startup progress event")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_contract_has_fixed_name_and_camel_case_payload() {
        assert_eq!(
            SSH_REMOTE_PROJECT_SESSIONS_REFRESHED_EVENT,
            "ssh-remote-project-sessions-refreshed"
        );

        let payload = serde_json::to_value(SshRemoteProjectSessionsRefreshedEvent {
            workspace_id: "workspace-1".to_owned(),
            succeeded_cli_ids: vec!["codex".to_owned()],
            failed_cli_ids: vec!["claude".to_owned()],
        })
        .expect("event DTO should serialize");

        assert_eq!(payload["workspaceId"], "workspace-1");
        assert_eq!(payload["succeededCliIds"][0], "codex");
        assert_eq!(payload["failedCliIds"][0], "claude");
        assert!(payload.get("workspace_id").is_none());
    }

    #[test]
    fn startup_progress_event_contract_has_fixed_name_and_payload() {
        assert_eq!(APP_STARTUP_PROGRESS_EVENT, "app-startup-progress");

        let payload = serde_json::to_value(AppStartupProgressEvent {
            phase: "connecting-ssh".to_owned(),
            message: "正在建立 SSH 连接……".to_owned(),
        })
        .expect("startup progress event DTO should serialize");

        assert_eq!(payload["phase"], "connecting-ssh");
        assert_eq!(payload["message"], "正在建立 SSH 连接……");
    }

    #[test]
    fn cli_services_updated_event_exposes_changes_and_health_errors() {
        assert_eq!(CLI_SERVICES_UPDATED_EVENT, "cli-services-updated");

        let payload = serde_json::to_value(CliServicesUpdatedEvent {
            scope: "local".to_owned(),
            connection_id: None,
            revision: 7,
            changed: false,
            errors: vec!["Claude 服务启动失败".to_owned()],
        })
        .expect("CLI services event DTO should serialize");

        assert_eq!(payload["scope"], "local");
        assert_eq!(payload["connectionId"], serde_json::Value::Null);
        assert_eq!(payload["revision"], 7);
        assert_eq!(payload["changed"], false);
        assert_eq!(payload["errors"][0], "Claude 服务启动失败");
        assert!(payload.get("connection_id").is_none());
    }
}
