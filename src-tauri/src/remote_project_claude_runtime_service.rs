use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, LazyLock},
};

use anyhow::Context;
use tokio::sync::RwLock;

use crate::{
    engines::{
        capabilities_for_engine, claude_remote::ClaudeRemoteEngine, map_engine_capabilities,
        map_model_info, Engine, ModelInfo,
    },
    models::{EngineInfoDto, ThreadDto, WorkspaceDto},
    ssh::cli_service_lifecycle::{self, SshCliService},
};

// Claude 运行时缓存原先独立保存在 REMOTE_CLAUDE_RUNTIMES 中，服务被通用生命周期
// 管理器关闭时无法同步清理。缓存现已并入 SshCliTunnel，由同一个 MAP 对象统一失效。
// #[derive(Clone)]
// struct RemoteClaudeRuntimeEntry {
//     local_port: u16,
//     engine: Arc<ClaudeRemoteEngine>,
// }
//
// static REMOTE_CLAUDE_RUNTIMES: LazyLock<RwLock<HashMap<String, RemoteClaudeRuntimeEntry>>> =
//     LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
struct RemoteClaudeRuntimeEntry {
    service_generation: u64,
    local_port: u16,
    engine: Arc<ClaudeRemoteEngine>,
}

static REMOTE_CLAUDE_RUNTIMES: LazyLock<RwLock<HashMap<String, RemoteClaudeRuntimeEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
enum RemoteClaudeUseKind {
    Temporary,
    Persistent { thread_id: String },
}

pub struct RemoteClaudeServiceUse {
    connection_id: String,
    engine: Arc<ClaudeRemoteEngine>,
    kind: RemoteClaudeUseKind,
    released: bool,
}

impl RemoteClaudeServiceUse {
    pub fn engine(&self) -> &Arc<ClaudeRemoteEngine> {
        &self.engine
    }

    pub async fn release(mut self) {
        self.released = true;
        release_service_use(&self.connection_id, &self.kind, &self.engine).await;
    }
}

impl Deref for RemoteClaudeServiceUse {
    type Target = ClaudeRemoteEngine;

    fn deref(&self) -> &Self::Target {
        self.engine.as_ref()
    }
}

impl Drop for RemoteClaudeServiceUse {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let connection_id = self.connection_id.clone();
        let kind = self.kind.clone();
        let engine = self.engine.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                release_service_use(&connection_id, &kind, &engine).await;
            });
        }
    }
}

pub fn validate_remote_claude_workspace(workspace: &WorkspaceDto) -> anyhow::Result<&str> {
    anyhow::ensure!(
        workspace.location_kind == "ssh",
        "workspace is not an SSH remote project"
    );
    anyhow::ensure!(
        workspace.connection_deleted != Some(true),
        "SSH 连接已删除，请先恢复连接"
    );
    anyhow::ensure!(
        workspace.connection_enabled != Some(false),
        "SSH 连接已禁用"
    );
    workspace
        .ssh_connection_id
        .as_deref()
        .context("远端项目未绑定 SSH 连接")
}

/// 取得 Claude 客户端运行对象。远端 Claude 服务端必须已经由
/// `cli_service_lifecycle` 在启动刷新阶段创建并登记。
pub async fn runtime(workspace: &WorkspaceDto) -> anyhow::Result<Arc<ClaudeRemoteEngine>> {
    let connection_id = validate_remote_claude_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "claude").await?;
    runtime_for_service(service.as_ref()).await
}

pub async fn acquire_turn(
    workspace: &WorkspaceDto,
    thread_id: &str,
) -> anyhow::Result<RemoteClaudeServiceUse> {
    let connection_id = validate_remote_claude_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "claude")
        .await
        .with_context(|| remote_claude_context(workspace, "取得持续对话服务"))?;
    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteClaudeServiceUse {
            connection_id,
            engine,
            kind: RemoteClaudeUseKind::Persistent {
                thread_id: thread_id.to_string(),
            },
            released: false,
        }),
        Err(error) => Err(error),
    }
}

pub async fn model_infos(
    connection_id: &str,
    active_use: Option<&RemoteClaudeServiceUse>,
) -> anyhow::Result<Vec<ModelInfo>> {
    let result = if let Some(service_use) = active_use {
        anyhow::ensure!(
            service_use.connection_id == connection_id,
            "SSH 远端 Claude 模型请求与当前连接不一致"
        );
        service_use.engine().list_models_runtime().await
    } else {
        let service = cli_service_lifecycle::get(connection_id, "claude").await?;
        runtime_for_service(service.as_ref())
            .await?
            .list_models_runtime()
            .await
    };
    let models = result
        .with_context(|| format!("SSH 远端 Claude 读取模型失败: connection_id={connection_id}"))?;
    anyhow::ensure!(!models.is_empty(), "SSH 远端 Claude 未返回可用模型");
    Ok(models)
}

pub async fn engine_info(
    connection_id: &str,
    active_use: Option<&RemoteClaudeServiceUse>,
) -> anyhow::Result<EngineInfoDto> {
    let models = model_infos(connection_id, active_use).await?;
    Ok(EngineInfoDto {
        id: "claude".to_string(),
        name: "Claude".to_string(),
        models: models.into_iter().map(map_model_info).collect(),
        capabilities: map_engine_capabilities(capabilities_for_engine("claude")),
    })
}

pub async fn prewarm(workspace: &WorkspaceDto) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use.engine().prewarm().await;
    service_use.release().await;
    result.with_context(|| remote_claude_context(workspace, "健康检查"))
}

pub async fn respond_to_approval(
    workspace: &WorkspaceDto,
    thread: &ThreadDto,
    approval_id: &str,
    response: serde_json::Value,
    route: Option<crate::engines::ApprovalRequestRoute>,
) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result =
        Engine::respond_to_approval(service_use.engine().as_ref(), approval_id, response, route)
            .await
            .with_context(|| format!("SSH 远端 Claude 审批回复失败: thread_id={}", thread.id));
    service_use.release().await;
    result
}

pub async fn interrupt(workspace: &WorkspaceDto, thread: &ThreadDto) -> anyhow::Result<()> {
    let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
        return Ok(());
    };
    let service_use = acquire_temporary(workspace).await?;
    let result = Engine::interrupt(service_use.engine().as_ref(), engine_thread_id)
        .await
        .with_context(|| format!("SSH 远端 Claude 取消失败: thread_id={}", thread.id));
    service_use.release().await;
    result
}

pub(crate) async fn acquire_temporary(
    workspace: &WorkspaceDto,
) -> anyhow::Result<RemoteClaudeServiceUse> {
    let connection_id = validate_remote_claude_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "claude")
        .await
        .with_context(|| remote_claude_context(workspace, "读取运行时"))?;
    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteClaudeServiceUse {
            connection_id,
            engine,
            kind: RemoteClaudeUseKind::Temporary,
            released: false,
        }),
        Err(error) => Err(error),
    }
}

// 旧实现只按 connection_id + local_port 复用缓存。远端服务在同一端口重启后，
// 该判断会把新服务误认为旧服务，因此保留注释，不再执行。
// async fn runtime_for_tunnel(
//     connection_id: &str,
//     local_port: u16,
// ) -> anyhow::Result<Arc<ClaudeRemoteEngine>> {
//     if let Some(entry) = REMOTE_CLAUDE_RUNTIMES.read().await.get(connection_id) {
//         if entry.local_port == local_port {
//             return Ok(entry.engine.clone());
//         }
//     }
//     let engine = Arc::new(ClaudeRemoteEngine::new(format!(
//         "http://127.0.0.1:{local_port}"
//     )));
//     let mut runtimes = REMOTE_CLAUDE_RUNTIMES.write().await;
//     if let Some(entry) = runtimes.get(connection_id) {
//         if entry.local_port == local_port {
//             return Ok(entry.engine.clone());
//         }
//     }
//     runtimes.insert(
//         connection_id.to_string(),
//         RemoteClaudeRuntimeEntry {
//             local_port,
//             engine: engine.clone(),
//         },
//     );
//     Ok(engine)
// }

async fn runtime_for_service(service: &SshCliService) -> anyhow::Result<Arc<ClaudeRemoteEngine>> {
    anyhow::ensure!(service.cli_id() == "claude", "远端 CLI 服务类型不是 Claude");
    let connection_id = service.connection_id();
    let local_port = service.local_port();
    let service_generation = service.generation();
    if let Some(entry) = REMOTE_CLAUDE_RUNTIMES.read().await.get(connection_id) {
        if entry.service_generation == service_generation && entry.local_port == local_port {
            return Ok(entry.engine.clone());
        }
    }

    let engine = Arc::new(ClaudeRemoteEngine::new(format!(
        "http://127.0.0.1:{local_port}"
    )));
    let mut runtimes = REMOTE_CLAUDE_RUNTIMES.write().await;
    if let Some(entry) = runtimes.get(connection_id) {
        if entry.service_generation == service_generation && entry.local_port == local_port {
            return Ok(entry.engine.clone());
        }
    }
    runtimes.insert(
        connection_id.to_string(),
        RemoteClaudeRuntimeEntry {
            service_generation,
            local_port,
            engine: engine.clone(),
        },
    );
    Ok(engine)
}

async fn release_service_use(
    connection_id: &str,
    kind: &RemoteClaudeUseKind,
    engine: &Arc<ClaudeRemoteEngine>,
) {
    /*
    旧实现会在每次业务调用结束后直接释放 Tunnel 的远端服务占用：
    let result = match kind {
        RemoteClaudeUseKind::Temporary => {
            cli_tunnel_registry::release_temporary_service_use(connection_id, "claude").await
        }
        RemoteClaudeUseKind::Persistent { thread_id } => {
            cli_tunnel_registry::release_persistent_service_use(connection_id, "claude", thread_id)
                .await
        }
    };
    match result {
        Ok(true) => {
            // 旧实现会在这里清理独立的 REMOTE_CLAUDE_RUNTIMES。运行时缓存现由
            // SshCliTunnelRegistry 在服务进入 Stopping/Stopped 时统一清理。
            // let mut runtimes = REMOTE_CLAUDE_RUNTIMES.write().await;
            // if runtimes
            //     .get(connection_id)
            //     .is_some_and(|entry| Arc::ptr_eq(&entry.engine, engine))
            // {
            //     runtimes.remove(connection_id);
            // }
            let _ = engine;
        }
        Ok(false) => {}
        Err(error) => {
            log::warn!(
                "释放 SSH 远端 Claude 服务占用失败: connection_id={connection_id} error={error:#}"
            );
        }
    }
    */
    // 远端 Claude 服务由 cli_service_lifecycle 常驻管理，业务调用结束只释放当前
    // 客户端引用，不再改变远端服务端和 Tunnel 的生命周期。
    let _ = (connection_id, kind, engine);
}

fn remote_claude_context(workspace: &WorkspaceDto, action: &str) -> String {
    let connection_name = workspace
        .connection_display_name
        .as_deref()
        .unwrap_or("未命名 SSH 连接");
    format!(
        "SSH 远端 Claude {action}失败: connection={connection_name} workspace={}",
        workspace.name
    )
}
