use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{
    computer_control_service::ComputerControlService, config::app_config::AppConfig, db::Database,
    engines::EngineManager, extensions::refresh::ExtensionCatalogRefreshManager,
    git::repo::FileTreeCache, git::watcher::GitWatcherManager, power::KeepAwakeManager,
    remote::RemoteTunnelManager, scheduled_tasks::ScheduledTaskManager,
    ssh::monitor::SshConnectionMonitor, terminal::TerminalManager,
    terminal_notifications::TerminalNotificationManager,
    auracoder_thread_mcp_service::AuraCoderThreadMcpService,
};

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub config: Arc<AppConfig>,
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    pub engines: Arc<EngineManager>,
    pub git_watchers: Arc<GitWatcherManager>,
    pub terminals: Arc<TerminalManager>,
    pub notifications: Arc<TerminalNotificationManager>,
    pub keep_awake: Arc<KeepAwakeManager>,
    pub turns: Arc<TurnManager>,
    pub file_tree_cache: Arc<FileTreeCache>,
    pub extension_catalog_refreshes: Arc<ExtensionCatalogRefreshManager>,
    pub scheduled_tasks: Arc<ScheduledTaskManager>,
    pub computer_control_service: Arc<ComputerControlService>,
    /// 读取 AuraCoder 本地会话的 MCP 服务。
    pub auracoder_thread_mcp_service: Arc<AuraCoderThreadMcpService>,
    pub remote_access: Arc<RemoteTunnelManager>,
    pub ssh_monitor: Arc<SshConnectionMonitor>,
}

#[derive(Default)]
pub struct TurnManager {
    active: RwLock<HashMap<String, CancellationToken>>,
}

impl TurnManager {
    pub async fn try_register(&self, thread_id: &str, token: CancellationToken) -> bool {
        let mut active = self.active.write().await;
        if active.contains_key(thread_id) {
            return false;
        }

        active.insert(thread_id.to_string(), token);
        true
    }

    pub async fn get(&self, thread_id: &str) -> Option<CancellationToken> {
        self.active.read().await.get(thread_id).cloned()
    }

    pub async fn cancel(&self, thread_id: &str) {
        if let Some(token) = self.active.read().await.get(thread_id).cloned() {
            token.cancel();
        }
    }

    pub async fn finish(&self, thread_id: &str) {
        self.active.write().await.remove(thread_id);
    }
}
