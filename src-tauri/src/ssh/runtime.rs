use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

use crate::{
    db::{self, ssh_connections::SshConnectionRecord, Database},
    models::WorkspaceDto,
};

pub const REMOTE_REPO_PREFIX: &str = "ssh://auracoder/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWorktreePath {
    pub root_path: String,
    pub relative_path: Option<String>,
}

pub fn remote_repo_marker(workspace_id: &str) -> String {
    format!("{REMOTE_REPO_PREFIX}{workspace_id}")
}

pub fn workspace_id_from_repo_marker(repo_path: &str) -> Option<&str> {
    repo_path
        .strip_prefix(REMOTE_REPO_PREFIX)
        .and_then(|value| value.split('/').next())
        .filter(|value| !value.is_empty())
}

pub fn remote_worktree_marker(workspace_id: &str, absolute_path: &str) -> String {
    format!(
        "{}/worktree/{}",
        remote_repo_marker(workspace_id).trim_end_matches('/'),
        URL_SAFE_NO_PAD.encode(absolute_path.as_bytes()),
    )
}

pub fn worktree_path_from_repo_marker(
    repo_path: &str,
) -> anyhow::Result<Option<RemoteWorktreePath>> {
    let Some(value) = repo_path.strip_prefix(REMOTE_REPO_PREFIX) else {
        return Ok(None);
    };
    let Some((_, suffix)) = value.split_once('/') else {
        return Ok(None);
    };
    let marker = suffix
        .strip_prefix("worktree/")
        .ok_or_else(|| anyhow::anyhow!("远端工作树标识无效"))?;
    let (encoded, relative_path) = match marker.split_once('/') {
        Some((encoded, relative_path)) => {
            validate_remote_relative_path(relative_path, false)?;
            (encoded, Some(relative_path.to_string()))
        }
        None => (marker, None),
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("远端工作树标识无效"))?;
    let path = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("远端工作树路径无效"))?;
    anyhow::ensure!(
        path.starts_with('/') && !path.contains('\0'),
        "远端工作树路径无效"
    );
    Ok(Some(RemoteWorktreePath {
        root_path: path,
        relative_path,
    }))
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
    use super::{
        remote_worktree_marker, validate_remote_relative_path, workspace_id_from_repo_marker,
        worktree_path_from_repo_marker,
    };

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
    fn encodes_registered_worktree_path_in_remote_repo_marker() {
        let marker = remote_worktree_marker("ws-1", "/srv/worktrees/feature");
        assert_eq!(workspace_id_from_repo_marker(&marker), Some("ws-1"));
        assert_eq!(
            worktree_path_from_repo_marker(&marker).unwrap(),
            Some(super::RemoteWorktreePath {
                root_path: "/srv/worktrees/feature".to_string(),
                relative_path: None,
            })
        );
        assert_eq!(
            worktree_path_from_repo_marker(&format!("{marker}/src/main")).unwrap(),
            Some(super::RemoteWorktreePath {
                root_path: "/srv/worktrees/feature".to_string(),
                relative_path: Some("src/main".to_string()),
            })
        );
    }
}
