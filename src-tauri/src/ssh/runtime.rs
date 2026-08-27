use crate::{
    db::{self, ssh_connections::SshConnectionRecord, Database},
    models::WorkspaceDto,
};

pub const REMOTE_WORKSPACE_PREFIX: &str = "ssh://auracoder/";

pub fn remote_workspace_marker(workspace_id: &str) -> String {
    format!("{REMOTE_WORKSPACE_PREFIX}{workspace_id}")
}

pub fn workspace_id_from_workspace_marker(root_path: &str) -> Option<&str> {
    root_path
        .strip_prefix(REMOTE_WORKSPACE_PREFIX)
        .and_then(|value| value.split('/').next())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone)]
pub struct WorkspaceTarget {
    pub workspace: WorkspaceDto,
    pub connection: Option<SshConnectionRecord>,
}

impl WorkspaceTarget {
    pub fn is_remote(&self) -> bool {
        self.workspace.location_kind == "ssh"
    }

    pub fn remote_connection(&self) -> anyhow::Result<&SshConnectionRecord> {
        self.connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("远端项目未绑定 SSH 连接"))
    }
}

pub fn resolve_workspace_target(
    db: &Database,
    workspace_id: &str,
) -> anyhow::Result<WorkspaceTarget> {
    let workspace = db::workspaces::find_workspace_by_id(db, workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
    let connection = match workspace.ssh_connection_id.as_deref() {
        Some(connection_id) => {
            let record = db::ssh_connections::find(db, connection_id)?
                .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在: {connection_id}"))?;
            if record.dto.deleted_at.is_some() {
                anyhow::bail!("SSH 连接已删除，请先恢复连接");
            }
            if !record.dto.enabled {
                anyhow::bail!("SSH 连接已禁用");
            }
            Some(record)
        }
        None => None,
    };

    if workspace.location_kind == "ssh" && connection.is_none() {
        anyhow::bail!("远端项目未绑定 SSH 连接");
    }
    if workspace.location_kind != "ssh" && connection.is_some() {
        anyhow::bail!("本地项目不能绑定 SSH 连接");
    }

    Ok(WorkspaceTarget {
        workspace,
        connection,
    })
}

pub fn validate_remote_relative_path(path: &str, allow_empty: bool) -> anyhow::Result<()> {
    if path.contains('\0') {
        anyhow::bail!("路径不能包含空字符");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        anyhow::bail!("远端路径必须是项目内相对路径");
    }
    if path.is_empty() {
        if allow_empty {
            return Ok(());
        }
        anyhow::bail!("远端路径不能为空");
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            anyhow::bail!("远端路径包含非法路径段");
        }
    }
    Ok(())
}

pub fn quote_posix(value: &str) -> String {
    /*
    旧 POSIX 引号实现由 runtime_env 的公共方法接替，保留代码以便追溯：
    format!("'{}'", value.replace('\'', "'\\''"))
    */
    crate::runtime_env::quote_posix(value)
}

/// 使用远端账号自己的登录交互式 shell 执行命令，让 CLI 解析规则与用户登录后保持一致。
pub fn wrap_remote_login_shell_command(command: &str) -> String {
    format!("\"${{SHELL:-/bin/sh}}\" -lic {}", quote_posix(command))
}

pub fn remote_path(root: &str, relative: &str) -> anyhow::Result<String> {
    validate_remote_relative_path(relative, true)?;
    if relative.is_empty() {
        Ok(root.to_string())
    } else {
        Ok(format!("{}/{}", root.trim_end_matches('/'), relative))
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_remote_relative_path, workspace_id_from_workspace_marker};

    #[test]
    fn wraps_remote_command_with_remote_login_shell() {
        assert_eq!(
            super::wrap_remote_login_shell_command("exec env codex --version"),
            "\"${SHELL:-/bin/sh}\" -lic 'exec env codex --version'"
        );
    }

    #[test]
    fn escapes_single_quotes_in_remote_login_shell_command() {
        assert_eq!(
            super::wrap_remote_login_shell_command("printf '%s' value"),
            "\"${SHELL:-/bin/sh}\" -lic 'printf '\\''%s'\\'' value'"
        );
    }

    #[test]
    fn allows_empty_path_when_addressing_workspace_root() {
        assert!(validate_remote_relative_path("", true).is_ok());
    }

    #[test]
    fn rejects_empty_path_for_file_operations() {
        assert!(validate_remote_relative_path("", false).is_err());
    }

    #[test]
    fn still_rejects_empty_segments_inside_relative_path() {
        assert!(validate_remote_relative_path("src//main.rs", true).is_err());
    }

    #[test]
    fn parses_project_root_workspace_marker() {
        assert_eq!(
            workspace_id_from_workspace_marker("ssh://auracoder/ws-1"),
            Some("ws-1")
        );
        assert_eq!(workspace_id_from_workspace_marker("/srv/project"), None);
    }
}
