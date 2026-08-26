use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::Context;
use tokio::{sync::RwLock, time::Instant};

use crate::{
    engines::{
        capabilities_for_engine, map_engine_capabilities, map_model_info, opencode::OpenCodeEngine,
        Engine, ModelInfo,
    },
    models::{EngineInfoDto, OpenCodeRuntimeCatalogDto, ThreadDto, WorkspaceDto},
    ssh::{
        cli_service_lifecycle::{self, SshCliService},
        // 旧实现由客户端运行服务直接取得 Tunnel 并控制远端服务启停。
        // cli_tunnel_registry,
    },
};

#[derive(Clone)]
struct RemoteOpenCodeRuntimeEntry {
    service_generation: u64,
    local_port: u16,
    password: String,
    engine: Arc<OpenCodeEngine>,
}

static REMOTE_OPENCODE_RUNTIMES: LazyLock<RwLock<HashMap<String, RemoteOpenCodeRuntimeEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
enum RemoteOpenCodeUseKind {
    Temporary,
    Persistent { thread_id: String },
}

pub struct RemoteOpenCodeServiceUse {
    connection_id: String,
    engine: Arc<OpenCodeEngine>,
    kind: RemoteOpenCodeUseKind,
    released: bool,
}

impl RemoteOpenCodeServiceUse {
    pub fn engine(&self) -> &Arc<OpenCodeEngine> {
        &self.engine
    }

    pub async fn release(mut self) {
        self.released = true;
        release_service_use(&self.connection_id, &self.kind, &self.engine).await;
    }
}

impl Deref for RemoteOpenCodeServiceUse {
    type Target = OpenCodeEngine;

    fn deref(&self) -> &Self::Target {
        self.engine.as_ref()
    }
}

impl Drop for RemoteOpenCodeServiceUse {
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

pub fn validate_remote_opencode_workspace(workspace: &WorkspaceDto) -> anyhow::Result<&str> {
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

/// 取得 OpenCode 客户端运行对象。远端 OpenCode 服务端必须已经由
/// `cli_service_lifecycle` 在启动刷新阶段创建并登记。
pub async fn runtime(workspace: &WorkspaceDto) -> anyhow::Result<Arc<OpenCodeEngine>> {
    let connection_id = validate_remote_opencode_workspace(workspace)?;
    let service = cli_service_lifecycle::get(connection_id, "opencode").await?;
    runtime_for_service(service.as_ref()).await
}

pub async fn acquire_turn(
    workspace: &WorkspaceDto,
    thread_id: &str,
) -> anyhow::Result<RemoteOpenCodeServiceUse> {
    let connection_id = validate_remote_opencode_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "opencode")
        .await
        .with_context(|| remote_opencode_context(workspace, "取得持续对话服务"))?;

    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteOpenCodeServiceUse {
            connection_id,
            engine,
            kind: RemoteOpenCodeUseKind::Persistent {
                thread_id: thread_id.to_string(),
            },
            released: false,
        }),
        Err(error) => Err(error),
    }
}

pub async fn model_infos(
    connection_id: &str,
    active_use: Option<&RemoteOpenCodeServiceUse>,
) -> anyhow::Result<Vec<ModelInfo>> {
    let models = if let Some(service_use) = active_use {
        anyhow::ensure!(
            service_use.connection_id == connection_id,
            "SSH 远端 OpenCode 模型请求与当前连接不一致"
        );
        service_use.engine().list_models_runtime().await
    } else {
        let service = cli_service_lifecycle::get(connection_id, "opencode").await?;
        runtime_for_service(service.as_ref())
            .await?
            .list_models_runtime()
            .await
    };
    anyhow::ensure!(!models.is_empty(), "SSH 远端 OpenCode 未返回可用模型");
    Ok(models)
}

pub async fn engine_info(
    connection_id: &str,
    active_use: Option<&RemoteOpenCodeServiceUse>,
) -> anyhow::Result<EngineInfoDto> {
    let models = model_infos(connection_id, active_use).await?;
    Ok(EngineInfoDto {
        id: "opencode".to_string(),
        name: "OpenCode".to_string(),
        models: models.into_iter().map(map_model_info).collect(),
        capabilities: map_engine_capabilities(capabilities_for_engine("opencode")),
    })
}

pub async fn runtime_catalog(
    workspace: &WorkspaceDto,
) -> anyhow::Result<OpenCodeRuntimeCatalogDto> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use
        .engine()
        .runtime_catalog(&workspace.root_path)
        .await;
    service_use.release().await;
    result
}

pub async fn prewarm(workspace: &WorkspaceDto) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use.engine().prewarm().await;
    service_use.release().await;
    result
}

pub async fn validate_session(
    workspace: &WorkspaceDto,
    engine_thread_id: &str,
) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use
        .engine()
        .read_session(&workspace.root_path, engine_thread_id)
        .await
        .and_then(|session| {
            anyhow::ensure!(
                session.cwd == workspace.root_path,
                "SSH 远端 OpenCode 会话目录不匹配: expected={} actual={}",
                workspace.root_path,
                session.cwd
            );
            Ok(())
        });
    service_use.release().await;
    result
}

pub async fn set_thread_archived(
    workspace: &WorkspaceDto,
    engine_thread_id: &str,
    archived: bool,
) -> anyhow::Result<()> {
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use
        .engine()
        .set_session_archived(&workspace.root_path, engine_thread_id, archived)
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
    let service_use = acquire_temporary(workspace).await?;
    let result =
        Engine::respond_to_approval(service_use.engine().as_ref(), approval_id, response, route)
            .await
            .with_context(|| {
                format!(
                    "SSH 远端 OpenCode 审批或问题回复失败: thread_id={}",
                    thread.id
                )
            });
    service_use.release().await;
    result
}

pub async fn interrupt(workspace: &WorkspaceDto, thread: &ThreadDto) -> anyhow::Result<()> {
    let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
        return Ok(());
    };
    let service_use = acquire_temporary(workspace).await?;
    let result = service_use
        .engine()
        .abort_session(&workspace.root_path, engine_thread_id)
        .await
        .with_context(|| format!("SSH 远端 OpenCode 取消失败: thread_id={}", thread.id));
    service_use.release().await;
    result
}

pub(crate) async fn acquire_temporary(
    workspace: &WorkspaceDto,
) -> anyhow::Result<RemoteOpenCodeServiceUse> {
    let connection_id = validate_remote_opencode_workspace(workspace)?.to_string();
    let service = cli_service_lifecycle::get(&connection_id, "opencode")
        .await
        .with_context(|| remote_opencode_context(workspace, "读取运行时"))?;
    match runtime_for_service(service.as_ref()).await {
        Ok(engine) => Ok(RemoteOpenCodeServiceUse {
            connection_id,
            engine,
            kind: RemoteOpenCodeUseKind::Temporary,
            released: false,
        }),
        Err(error) => Err(error),
    }
}

async fn runtime_for_service(service: &SshCliService) -> anyhow::Result<Arc<OpenCodeEngine>> {
    anyhow::ensure!(
        service.cli_id() == "opencode",
        "远端 CLI 服务类型不是 OpenCode"
    );
    let connection_id = service.connection_id();
    let local_port = service.local_port();
    let service_generation = service.generation();
    let password = service
        .remote_service_secret()
        .context("OpenCode 远端服务认证信息不存在")?;
    if let Some(entry) = REMOTE_OPENCODE_RUNTIMES.read().await.get(connection_id) {
        if entry.service_generation == service_generation
            && entry.local_port == local_port
            && entry.password == password
        {
            return Ok(entry.engine.clone());
        }
    }

    let base_url = format!("http://127.0.0.1:{local_port}");
    let readiness_client = reqwest::Client::new();
    let readiness_started = Instant::now();
    let mut readiness_confirmed = false;
    let mut last_readiness_error = "尚未收到健康检查响应".to_string();
    while readiness_started.elapsed() < Duration::from_secs(20) {
        match readiness_client
            .get(format!("{base_url}/global/health"))
            .basic_auth("opencode", Some(password))
            .timeout(Duration::from_millis(500))
            .send()
            .await
        {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<serde_json::Value>().await {
                    Ok(body)
                        if body.get("healthy").and_then(serde_json::Value::as_bool)
                            == Some(true) =>
                    {
                        readiness_confirmed = true;
                        break;
                    }
                    Ok(body) => {
                        last_readiness_error = format!("健康检查未返回 healthy=true: {body}");
                    }
                    Err(error) => {
                        last_readiness_error = format!("解析健康检查响应失败: {error}");
                    }
                },
                Err(error) => {
                    last_readiness_error = format!("健康检查返回错误状态: {error}");
                }
            },
            Err(error) => {
                last_readiness_error = format!("健康检查请求失败: {error}");
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::ensure!(
        readiness_confirmed,
        "SSH 远端 OpenCode HTTP 服务未就绪: {last_readiness_error}"
    );

    let engine = Arc::new(OpenCodeEngine::new_remote_http(
        base_url,
        password.to_string(),
    ));
    let mut runtimes = REMOTE_OPENCODE_RUNTIMES.write().await;
    if let Some(entry) = runtimes.get(connection_id) {
        if entry.service_generation == service_generation
            && entry.local_port == local_port
            && entry.password == password
        {
            return Ok(entry.engine.clone());
        }
    }
    runtimes.insert(
        connection_id.to_string(),
        RemoteOpenCodeRuntimeEntry {
            service_generation,
            local_port,
            password: password.to_string(),
            engine: engine.clone(),
        },
    );
    Ok(engine)
}

async fn release_service_use(
    connection_id: &str,
    kind: &RemoteOpenCodeUseKind,
    engine: &Arc<OpenCodeEngine>,
) {
    /*
    旧实现：
    let result = match kind {
        RemoteOpenCodeUseKind::Temporary => {
            cli_tunnel_registry::release_temporary_service_use(connection_id, "opencode").await
        }
        RemoteOpenCodeUseKind::Persistent { thread_id } => {
            cli_tunnel_registry::release_persistent_service_use(
                connection_id,
                "opencode",
                thread_id,
            )
            .await
        }
    };
    match result {
        Ok(true) => {
            let mut runtimes = REMOTE_OPENCODE_RUNTIMES.write().await;
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
                "释放 SSH 远端 OpenCode 服务占用失败: connection_id={connection_id} error={error:#}"
            );
        }
    }
    */
    // 远端 OpenCode 服务由 cli_service_lifecycle 常驻管理，业务调用结束不再直接
    // 释放 Tunnel 或关闭远端服务端。
    let _ = (connection_id, kind, engine);
}

fn remote_opencode_context(workspace: &WorkspaceDto, action: &str) -> String {
    let connection_name = workspace
        .connection_display_name
        .as_deref()
        .unwrap_or("未命名 SSH 连接");
    format!(
        "SSH 远端 OpenCode {action}失败: connection={connection_name} workspace={}",
        workspace.name
    )
}
