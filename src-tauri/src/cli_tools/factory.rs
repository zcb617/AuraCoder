use std::sync::Arc;

use anyhow::Result;

use super::{claude_code::ClaudeCodeCli, codex::CodexCli, opencode::OpenCodeCli, CliTool};
use crate::state::AppState;

/// 按 CLI 工具名称创建对应的统一 CLI 业务接口实现。
pub struct CliToolFactory {
    state: AppState,
}

impl CliToolFactory {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub fn create(&self, cli_id: &str) -> Result<Arc<dyn CliTool>> {
        match cli_id {
            "codex" => Ok(Arc::new(CodexCli::new(self.state.clone()))),
            "opencode" => Ok(Arc::new(OpenCodeCli::new(self.state.clone()))),
            "claude" => Ok(Arc::new(ClaudeCodeCli::new(self.state.clone()))),
            _ => anyhow::bail!("不支持的 CLI 工具: {cli_id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use super::CliToolFactory;
    use crate::{
        config::app_config::AppConfig,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        power::KeepAwakeManager,
        scheduled_tasks::ScheduledTaskManager,
        state::{AppState, TurnManager},
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    use uuid::Uuid;

    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-cli-tool-factory-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create test root");

        let db = crate::db::Database::open(root.join("workspaces.db"))
            .expect("failed to create test database");
        AppState {
            db: db.clone(),
            config: Arc::new(AppConfig::default()),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            engines: Arc::new(EngineManager::new()),
            git_watchers: Arc::new(GitWatcherManager::default()),
            terminals: Arc::new(TerminalManager::default()),
            notifications: Arc::new(TerminalNotificationManager::default()),
            keep_awake: Arc::new(KeepAwakeManager::new()),
            turns: Arc::new(TurnManager::default()),
            file_tree_cache: Arc::new(FileTreeCache::new()),
            extension_catalog_refreshes: Arc::new(
                crate::extensions::refresh::ExtensionCatalogRefreshManager::default(),
            ),
            scheduled_tasks: Arc::new(ScheduledTaskManager::new()),
            /*
            computer_control_approvals: Arc::new(
                crate::commands::computer_control::ComputerControlApprovalManager::default(),
            ),
            */
            computer_control_service: Arc::new(
                crate::computer_control_service::ComputerControlService::default(),
            ),
            auracoder_thread_mcp_service: Arc::new(
                crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db),
            ),
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    #[test]
    fn create_returns_matching_cli_tool() {
        let factory = CliToolFactory::new(test_app_state());

        for (cli_id, expected_id) in [
            ("codex", "codex"),
            ("opencode", "opencode"),
            ("claude", "claude"),
        ] {
            let cli = factory.create(cli_id).expect("factory should resolve CLI");
            assert_eq!(cli.id(), expected_id);
        }
    }

    #[tokio::test]
    async fn execution_context_does_not_create_workspace_without_valid_id() {
        let state = test_app_state();
        let db = state.db.clone();
        let factory = CliToolFactory::new(state);

        for cli_id in ["codex", "opencode", "claude"] {
            let cli = factory.create(cli_id).expect("factory should resolve CLI");

            let missing_id_error = cli
                .execution_context(None)
                .await
                .expect_err("missing workspace id should be rejected");
            assert!(missing_id_error.to_string().contains("请先选择项目"));

            let unknown_id_error = cli
                .execution_context(Some("missing-workspace"))
                .await
                .expect_err("unknown workspace id should be rejected");
            assert!(unknown_id_error
                .to_string()
                .contains("项目不存在或已被移除，请重新选择项目"));
        }

        assert!(
            crate::db::workspaces::list_workspaces(&db)
                .expect("failed to list workspaces")
                .is_empty(),
            "CLI execution context lookup must not create a workspace",
        );
    }
}
