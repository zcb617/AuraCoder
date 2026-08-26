use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, LazyLock},
};

use anyhow::Context;
use tokio::sync::RwLock;

use crate::{
    engines::{
        capabilities_for_engine, codex::CodexEngine, map_engine_capabilities, map_model_info,
        Engine, ModelInfo, ThreadSyncSnapshot,
    },
    models::{EngineInfoDto, ThreadDto, WorkspaceDto},
    ssh::{
        cli_service_lifecycle::{self, SshCliService},
        // 旧实现由客户端运行服务直接取得 Tunnel 并控制远端服务启停。
        // cli_tunnel_registry,
    },
};

#[derive(Clone)]
struct RemoteCodexRuntimeEntry {
    service_generation: u64,
    local_port: u16,
    engine: Arc<CodexEngine>,
}

static REMOTE_CODEX_RUNTIMES: LazyLock<RwLock<HashMap<String, RemoteCodexRuntimeEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
enum RemoteCodexUseKind {
    Temporary,
    Persistent { thread_id: String },
}

pub struct RemoteCodexServiceUse {
    connection_id: String,
    engine: Arc<CodexEngine>,
    kind: RemoteCodexUseKind,
    released: bool,
}

impl RemoteCodexServiceUse {
    pub fn engine(&self) -> &Arc<CodexEngine> {
        &self.engine
    }

    pub async fn release(mut self) {
        self.released = true;
        release_service_use(&self.connection_id, &self.kind, &self.engine).await;
    }
}

impl Deref for RemoteCodexServiceUse {
    type Target = CodexEngine;

    fn deref(&self) -> &Self::Target {
        self.engine.as_ref()
    }
}

impl Drop for RemoteCodexServiceUse {
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

pub fn validate_remote_codex_workspace(workspace: &WorkspaceDto) -> anyhow::Result<&str> {
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

/// 取得 Codex 客户端运行对象。远端 Codex 服务端必须已经由
/// `cli_service_lifecycle` 在启动刷新阶段创建并登记。
pub async fn runtime(workspace: &WorkspaceDto) -> anyhow::Result<Arc<CodexEngine>> {
    let connection_id = validate_remote_codex_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "codex").await?;
    runtime_for_service(service.as_ref()).await
}

pub async fn acquire_turn(
    workspace: &WorkspaceDto,
    thread_id: &str,
) -> anyhow::Result<RemoteCodexServiceUse> {
    let connection_id = validate_remote_codex_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "codex")
        .await
        .with_context(|| remote_codex_context(workspace, "取得持续对话服务"))?;

    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteCodexServiceUse {
            connection_id,
            engine,
            kind: RemoteCodexUseKind::Persistent {
                thread_id: thread_id.to_string(),
            },
            released: false,
        }),
        Err(error) => Err(error),
    }
}

pub async fn model_infos(
    connection_id: &str,
    active_use: Option<&RemoteCodexServiceUse>,
) -> anyhow::Result<Vec<ModelInfo>> {
    if let Some(service_use) = active_use {
        anyhow::ensure!(
            service_use.connection_id == connection_id,
            "SSH 远端 Codex 模型请求与当前连接不一致"
        );
        return Ok(service_use.engine().list_models_runtime().await);
    }
    let service = cli_service_lifecycle::get(connection_id, "codex").await?;
    Ok(runtime_for_service(service.as_ref())
        .await?
        .list_models_runtime()
        .await)
}

pub async fn engine_info(
    connection_id: &str,
    active_use: Option<&RemoteCodexServiceUse>,
) -> anyhow::Result<EngineInfoDto> {
    let models = model_infos(connection_id, active_use).await?;
    Ok(EngineInfoDto {
        id: "codex".to_string(),
        name: "Codex".to_string(),
        models: models.into_iter().map(map_model_info).collect(),
        capabilities: map_engine_capabilities(capabilities_for_engine("codex")),
    })
}

pub async fn set_thread_archived(
    workspace: &WorkspaceDto,
    engine_thread_id: &str,
    archived: bool,
) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result = if archived {
        Engine::archive_thread(service_use.engine().as_ref(), engine_thread_id).await
    } else {
        Engine::unarchive_thread(service_use.engine().as_ref(), engine_thread_id).await
    };
    service_use.release().await;
    result
}

pub async fn read_thread_sync_snapshot(
    workspace: &WorkspaceDto,
    engine_thread_id: &str,
) -> anyhow::Result<ThreadSyncSnapshot> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use
        .engine()
        .read_thread_sync_snapshot(engine_thread_id)
        .await;
    service_use.release().await;
    result
}

pub async fn respond_to_approval(
    workspace: &WorkspaceDto,
    thread: &ThreadDto,
    approval_id: &str,
    response: serde_json::Value,
    route: Option<crate::engines::ApprovalRequestRoute>,
) -> anyhow::Result<()> {
    let connection_id = validate_remote_codex_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "codex")
        .await
        .with_context(|| remote_codex_context(workspace, "回复审批"))?;
    let engine = runtime_for_service(service.as_ref()).await?;
    Engine::respond_to_approval(engine.as_ref(), approval_id, response, route)
        .await
        .with_context(|| format!("SSH 远端 Codex 审批回复失败: thread_id={}", thread.id))
}

pub async fn steer_message(
    workspace: &WorkspaceDto,
    thread: &ThreadDto,
    engine_thread_id: &str,
    client_steer_id: &str,
    content: &str,
    input: crate::engines::TurnInput,
) -> anyhow::Result<crate::engines::EngineSteerReceipt> {
    let connection_id = validate_remote_codex_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "codex")
        .await
        .with_context(|| remote_codex_context(workspace, "追加消息"))?;
    let engine = runtime_for_service(service.as_ref()).await?;
    Engine::steer_message(
        engine.as_ref(),
        engine_thread_id,
        client_steer_id,
        content,
        input,
    )
    .await
    .with_context(|| format!("SSH 远端 Codex 追加消息失败: thread_id={}", thread.id))
}

pub async fn interrupt(workspace: &WorkspaceDto, thread: &ThreadDto) -> anyhow::Result<()> {
    let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
        return Ok(());
    };
    let connection_id = validate_remote_codex_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "codex")
        .await
        .with_context(|| remote_codex_context(workspace, "取消对话"))?;
    let engine = runtime_for_service(service.as_ref()).await?;
    Engine::interrupt(engine.as_ref(), engine_thread_id)
        .await
        .with_context(|| format!("SSH 远端 Codex 取消失败: thread_id={}", thread.id))
}

// 对话创建和发送直接调用 `Engine` trait；这里不再为单一调用点增加转发函数。

pub(crate) async fn acquire_temporary(
    workspace: &WorkspaceDto,
) -> anyhow::Result<RemoteCodexServiceUse> {
    let connection_id = validate_remote_codex_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "codex")
        .await
        .with_context(|| remote_codex_context(workspace, "读取运行时"))?;
    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteCodexServiceUse {
            connection_id,
            engine,
            kind: RemoteCodexUseKind::Temporary,
            released: false,
        }),
        Err(error) => Err(error),
    }
}

async fn runtime_for_service(service: &SshCliService) -> anyhow::Result<Arc<CodexEngine>> {
    anyhow::ensure!(service.cli_id() == "codex", "远端 CLI 服务类型不是 Codex");
    let connection_id = service.connection_id();
    let local_port = service.local_port();
    let service_generation = service.generation();
    if let Some(entry) = REMOTE_CODEX_RUNTIMES.read().await.get(connection_id) {
        if entry.service_generation == service_generation && entry.local_port == local_port {
            return Ok(entry.engine.clone());
        }
    }

    let engine = Arc::new(CodexEngine::new_remote_websocket(format!(
        "ws://127.0.0.1:{local_port}"
    )));
    let mut runtimes = REMOTE_CODEX_RUNTIMES.write().await;
    if let Some(entry) = runtimes.get(connection_id) {
        if entry.service_generation == service_generation && entry.local_port == local_port {
            return Ok(entry.engine.clone());
        }
    }
    runtimes.insert(
        connection_id.to_string(),
        RemoteCodexRuntimeEntry {
            service_generation,
            local_port,
            engine: engine.clone(),
        },
    );
    Ok(engine)
}

async fn release_service_use(
    connection_id: &str,
    kind: &RemoteCodexUseKind,
    engine: &Arc<CodexEngine>,
) {
    /*
    旧实现：
    let result = match kind {
        RemoteCodexUseKind::Temporary => {
            cli_tunnel_registry::release_temporary_service_use(connection_id, "codex").await
        }
        RemoteCodexUseKind::Persistent { thread_id } => {
            cli_tunnel_registry::release_persistent_service_use(connection_id, "codex", thread_id)
                .await
        }
    };
    match result {
        Ok(true) => {
            let mut runtimes = REMOTE_CODEX_RUNTIMES.write().await;
            if runtimes
                .get(connection_id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.engine, engine))
            {
                runtimes.remove(connection_id);
            }
        }
        Ok(false) => {}
        Err(error) => {
            log::warn!(
                "释放 SSH 远端 Codex 服务占用失败: connection_id={connection_id} error={error:#}"
            );
        }
    }
    */
    // 旧实现会在一次业务调用结束后直接释放 Tunnel 服务占用，并可能关闭远端
    // Codex 服务。现在远端服务由 cli_service_lifecycle 常驻管理，业务调用结束只
    // 释放当前客户端引用，不再改变远端服务端生命周期。
    let _ = (connection_id, kind, engine);
}

fn remote_codex_context(workspace: &WorkspaceDto, action: &str) -> String {
    let connection_name = workspace
        .connection_display_name
        .as_deref()
        .unwrap_or("未命名 SSH 连接");
    format!(
        "SSH 远端 Codex {action}失败: connection={connection_name} workspace={}",
        workspace.name
    )
}
