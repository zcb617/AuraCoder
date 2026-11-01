use std::path::{Path, PathBuf};

use crate::{
    models::{
        SshConnectionRecord, TerminalNotificationDto, TerminalRendererDiagnosticsDto,
        TerminalResumeSessionDto, TerminalSessionDto, WorkspaceTarget,
    },
    path_utils,
    services::workspace_service::WorkspaceService,
    ssh::{
        gateway,
        runtime::{
            quote_posix, remote_workspace_marker, validate_remote_relative_path,
            workspace_id_from_workspace_marker, REMOTE_WORKSPACE_PREFIX,
        },
    },
    state::AppState,
};

/// 管理终端命令对应的终端会话与通知业务。
pub struct TerminalService {
    /// 保存应用状态，用于访问终端会话管理器和通知管理器。
    state: AppState,
}

impl TerminalService {
    /// 创建终端业务服务并保存应用状态。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 按项目工作目录限制创建终端会话并返回会话信息。
    pub async fn terminal_create_session(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
    ) -> Result<TerminalSessionDto, String> {
        let state = &self.state;
        // 通过工作区服务解析项目及远端 SSH 连接，维持原查询错误映射。
        let target: WorkspaceTarget = WorkspaceService::new(state.clone())
            .resolve_workspace_target(&workspace_id)
            .await
            .map_err(err_to_string)?;
        // 按本地或远端项目规则解析终端目录，阻止目录越出项目根路径。
        let (resolved_cwd, remote_connection) = if target.is_remote() {
            let connection = target.remote_connection().map_err(err_to_string)?.clone();
            let resolved_cwd = resolve_remote_cwd(
                &connection,
                &target.workspace.id,
                &target.workspace.root_path,
                cwd.as_deref(),
            )
            .await?;
            (resolved_cwd, Some(connection))
        } else {
            let workspace_root = target.workspace.root_path;
            let workspace_root_canonical =
                canonicalize_existing_dir(&workspace_root, "workspace root directory")?;
            let resolved_cwd = match cwd {
                Some(path) => {
                    let cwd_canonical = canonicalize_existing_dir(&path, "cwd")?;
                    if !cwd_canonical.starts_with(&workspace_root_canonical) {
                        return Err(format!(
                            "cwd must be inside workspace root: {}",
                            workspace_root_canonical.to_string_lossy()
                        ));
                    }
                    cwd_canonical.to_string_lossy().to_string()
                }
                None => workspace_root_canonical.to_string_lossy().to_string(),
            };
            (resolved_cwd, None)
        };
        // 按原参数顺序创建终端资源，并保留最小行列值限制。
        state
            .terminals
            .create_session(
                app,
                state.notifications.clone(),
                workspace_id,
                resolved_cwd,
                remote_connection,
                cols.max(1),
                rows.max(1),
            )
            .await
            .map_err(err_to_string)
    }

    /// 向指定终端会话写入文本数据。
    pub async fn terminal_write(
        &self,
        workspace_id: String,
        session_id: String,
        data: String,
    ) -> Result<(), String> {
        let state = &self.state;
        // 将文本数据写入对应项目的终端会话。
        state
            .terminals
            .write(&workspace_id, &session_id, data)
            .await
            .map_err(err_to_string)
    }

    /// 向指定终端会话写入原始字节数据。
    pub async fn terminal_write_bytes(
        &self,
        workspace_id: String,
        session_id: String,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let state = &self.state;
        // 保持终端字节输出原样并交由对应会话处理。
        state
            .terminals
            .write_bytes(&workspace_id, &session_id, data)
            .await
            .map_err(err_to_string)
    }

    /// 调整指定终端会话的字符尺寸和像素尺寸。
    pub async fn terminal_resize(
        &self,
        workspace_id: String,
        session_id: String,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), String> {
        let state = &self.state;
        // 保留最小字符行列限制后转交终端管理器调整尺寸。
        state
            .terminals
            .resize(
                &workspace_id,
                &session_id,
                cols.max(1),
                rows.max(1),
                pixel_width,
                pixel_height,
            )
            .await
            .map_err(err_to_string)
    }

    /// 关闭指定终端会话并清除其相关通知。
    pub async fn terminal_close_session(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
        session_id: String,
    ) -> Result<(), String> {
        let state = &self.state;
        // 先关闭会话，再按原顺序清理该会话的通知。
        state
            .terminals
            .close_session(app.clone(), &workspace_id, &session_id)
            .await
            .map_err(err_to_string)?;
        state
            .notifications
            .clear_for_session(&app, &workspace_id, &session_id)
            .await;
        Ok(())
    }

    /// 关闭指定项目的全部终端会话并清除项目通知。
    pub async fn terminal_close_workspace_sessions(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
    ) -> Result<(), String> {
        let state = &self.state;
        // 先关闭项目会话，再按原顺序清理项目通知。
        state
            .terminals
            .close_workspace(app.clone(), &workspace_id)
            .await
            .map_err(err_to_string)?;
        state
            .notifications
            .clear_for_workspace(&app, &workspace_id)
            .await;
        Ok(())
    }

    /// 列出指定项目的全部终端会话。
    pub async fn terminal_list_sessions(
        &self,
        workspace_id: String,
    ) -> Result<Vec<TerminalSessionDto>, String> {
        let state = &self.state;
        // 获取终端管理器维护的项目会话列表。
        Ok(state.terminals.list_sessions(&workspace_id).await)
    }

    /// 读取指定终端会话的渲染器诊断信息。
    pub async fn terminal_get_renderer_diagnostics(
        &self,
        workspace_id: String,
        session_id: String,
    ) -> Result<TerminalRendererDiagnosticsDto, String> {
        let state = &self.state;
        // 获取指定会话的渲染器诊断信息并保持原错误转换。
        state
            .terminals
            .renderer_diagnostics(&workspace_id, &session_id)
            .await
            .map_err(err_to_string)
    }

    /// 恢复指定终端会话并读取给定序号后的输出。
    pub async fn terminal_resume_session(
        &self,
        workspace_id: String,
        session_id: String,
        from_seq: Option<u64>,
    ) -> Result<TerminalResumeSessionDto, String> {
        let state = &self.state;
        // 按起始序号恢复会话输出。
        state
            .terminals
            .resume_session(&workspace_id, &session_id, from_seq)
            .await
            .map_err(err_to_string)
    }

    /// 读取指定终端会话的受限字节数输出片段。
    pub async fn terminal_drain_output(
        &self,
        workspace_id: String,
        session_id: String,
        from_seq: Option<u64>,
        target_bytes: u64,
    ) -> Result<TerminalResumeSessionDto, String> {
        let state = &self.state;
        // 按原有字节范围限制读取会话输出。
        state
            .terminals
            .drain_output(
                &workspace_id,
                &session_id,
                from_seq,
                target_bytes.clamp(1, 1024 * 1024) as usize,
            )
            .await
            .map_err(err_to_string)
    }

    /// 列出指定项目的终端通知。
    pub async fn terminal_list_notifications(
        &self,
        workspace_id: String,
    ) -> Result<Vec<TerminalNotificationDto>, String> {
        let state = &self.state;
        // 获取通知管理器中的项目通知。
        Ok(state.notifications.list_for_workspace(&workspace_id).await)
    }

    /// 清理指定终端会话或整个项目的通知。
    pub async fn terminal_clear_notification(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
        session_id: Option<String>,
    ) -> Result<(), String> {
        let state = &self.state;
        // 根据会话参数选择清理单个会话通知或项目全部通知。
        match session_id.as_deref() {
            Some(session_id) => {
                state
                    .notifications
                    .clear_for_session(&app, &workspace_id, session_id)
                    .await;
            }
            None => {
                state
                    .notifications
                    .clear_for_workspace(&app, &workspace_id)
                    .await;
            }
        }
        Ok(())
    }

    /// 同步主窗口焦点状态，并在重新聚焦时清理全部终端通知。
    pub async fn terminal_set_notification_focus(
        &self,
        app: tauri::AppHandle,
        workspace_id: Option<String>,
        session_id: Option<String>,
        window_focused: bool,
    ) -> Result<(), String> {
        let state = &self.state;
        // 先同步焦点状态，聚焦时再按原行为清理全部通知。
        state
            .notifications
            .set_focus(window_focused, workspace_id.clone(), session_id.clone())
            .await;

        if window_focused {
            state.notifications.clear_all(&app).await;
        }

        Ok(())
    }
}

/// 在远端项目根目录范围内解析用户请求的终端工作目录。
async fn resolve_remote_cwd(
    connection: &SshConnectionRecord,
    workspace_id: &str,
    root: &str,
    cwd: Option<&str>,
) -> Result<String, String> {
    let requested = cwd.unwrap_or(root).trim();
    if requested.is_empty() || requested.contains('\0') {
        return Err("远端终端目录无效".to_string());
    }
    let (requested, allowed_root) =
        if requested == remote_workspace_marker(workspace_id) || requested == root {
            (root.to_string(), root.to_string())
        } else if requested.starts_with(REMOTE_WORKSPACE_PREFIX)
            || workspace_id_from_workspace_marker(requested).is_some()
        {
            return Err("远端终端目录不属于当前项目".to_string());
        } else {
            (requested.to_string(), root.to_string())
        };
    let candidate = if requested.starts_with('/') {
        quote_posix(&requested)
    } else {
        validate_remote_relative_path(&requested, true).map_err(err_to_string)?;
        format!(
            "{}/{}",
            quote_posix(root.trim_end_matches('/')),
            quote_posix(&requested)
        )
    };
    let command = format!(
        "root={}; root_real=$(realpath -- \"$root\") || exit 21; candidate=$(realpath -- {}) || exit 22; case \"$candidate\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; [ -d \"$candidate\" ] || exit 24; printf '__PANES_CWD__%s\\n' \"$candidate\"",
        quote_posix(&allowed_root),
        candidate,
    );
    let output = gateway::run_command(connection, &command)
        .await
        .map_err(err_to_string)?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("__PANES_CWD__"))
        .filter(|path| path.starts_with('/'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| "远端终端目录解析失败".to_string())
}

/// 将已存在的本地目录规范化为绝对路径。
fn canonicalize_existing_dir(path: &str, label: &str) -> Result<PathBuf, String> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(format!("{label} does not exist: {path}"));
    }

    path_utils::canonicalize_path(dir)
        .map_err(|error| format!("failed to resolve {label}: {error}"))
}

/// 将业务错误转换为终端 IPC 使用的字符串错误。
fn err_to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}