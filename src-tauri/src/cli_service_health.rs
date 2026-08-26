//! CLI 服务定时健康检查调度。
//!
//! 按固定周期对本地和远端 CLI 生命周期 MAP 做 reconcile：
//! 本地每 5 分钟一次，远端每 1 分钟一次。reconcile 只观测 CLI 服务是否存活并
//! 对齐生命周期 MAP，不负责隧道恢复（隧道恢复属于 `cli_tunnel_registry`）。
//! MAP 一旦发生增删，先完成更新，再通过统一消息组件通知前端刷新 CLI 目录缓存。

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use tauri::AppHandle;

use crate::{
    db::{self, Database},
    local_cli_service_lifecycle::LocalCliServiceLifecycle,
    message_notify_helper::{
        notify_cli_services_updated, CliHealthReconcileResult, CliServicesUpdatedEvent,
    },
    ssh::cli_service_lifecycle,
};

/// 本地 CLI 健康检查周期：5 分钟。
const LOCAL_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(300);
/// 远端 CLI 健康检查周期：1 分钟。
const REMOTE_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(60);

static NEXT_EVENT_REVISION: AtomicU64 = AtomicU64::new(1);

/// 智能体运行工具页刷新按钮触发的本机 CLI 手动健康检查。
///
/// 只做一次 reconcile 并返回 MAP 是否发生增删；不发 `cli-services-updated`
/// 事件——前端调用方在拿到返回后自行刷新 CLI 目录缓存，保证"转圈结束"发生在
/// 新数据落地之后，事件路径无法保证这个顺序。
#[tauri::command]
// 旧命令只返回 bool，前端会把异常导致的 false 当作正常无变化：
// pub async fn refresh_local_cli_health() -> Result<bool, String> {
pub async fn refresh_local_cli_health() -> Result<CliHealthReconcileResult, String> {
    Ok(LocalCliServiceLifecycle::reconcile_health().await)
}

/// 启动本地和远端两条健康检查循环。
pub fn spawn_cli_service_health_scheduler(app: AppHandle, db: Database) {
    let local_app = app.clone();
    tauri::async_runtime::spawn(async move {
        run_local_health_loop(local_app).await;
    });
    tauri::async_runtime::spawn(async move {
        run_remote_health_loop(app, db).await;
    });
}

async fn run_local_health_loop(app: AppHandle) {
    loop {
        tokio::time::sleep(LOCAL_HEALTH_CHECK_INTERVAL).await;
        let result = LocalCliServiceLifecycle::reconcile_health().await;
        if !result.changed && result.errors.is_empty() {
            continue;
        }
        let event = CliServicesUpdatedEvent {
            scope: "local".to_string(),
            connection_id: None,
            revision: NEXT_EVENT_REVISION.fetch_add(1, Ordering::Relaxed),
            changed: result.changed,
            errors: result.errors,
        };
        if let Err(error) = notify_cli_services_updated(&app, event) {
            log::warn!("发送本机 CLI 目录更新事件失败: {error:#}");
        }
    }
}

async fn run_remote_health_loop(app: AppHandle, db: Database) {
    loop {
        tokio::time::sleep(REMOTE_HEALTH_CHECK_INTERVAL).await;

        let connection_ids = match load_monitorable_connection_ids(db.clone()).await {
            Ok(connection_ids) => connection_ids,
            Err(error) => {
                log::warn!("健康检查读取 SSH 连接列表失败: {error:#}");
                let event = CliServicesUpdatedEvent {
                    scope: "ssh".to_string(),
                    connection_id: None,
                    revision: NEXT_EVENT_REVISION.fetch_add(1, Ordering::Relaxed),
                    changed: false,
                    errors: vec![format!(
                        "AuraCoder 无法读取需要健康检查的 SSH 连接列表：{error:#}"
                    )],
                };
                if let Err(notify_error) = notify_cli_services_updated(&app, event) {
                    log::warn!("发送 SSH 健康检查异常事件失败: {notify_error:#}");
                }
                continue;
            }
        };

        for connection_id in connection_ids {
            let result = cli_service_lifecycle::reconcile_health(&connection_id).await;
            if !result.changed && result.errors.is_empty() {
                continue;
            }
            let event = CliServicesUpdatedEvent {
                scope: "ssh".to_string(),
                connection_id: Some(connection_id.clone()),
                revision: NEXT_EVENT_REVISION.fetch_add(1, Ordering::Relaxed),
                changed: result.changed,
                errors: result.errors,
            };
            if let Err(error) = notify_cli_services_updated(&app, event) {
                log::warn!(
                    "发送 SSH 远端 CLI 目录更新事件失败: connection_id={connection_id} error={error:#}"
                );
            }
        }
    }
}

/// 读取当前启用且未删除的 SSH 连接标识列表；与 SSH 连接监控使用同一筛选条件。
async fn load_monitorable_connection_ids(db: Database) -> anyhow::Result<Vec<String>> {
    tokio::task::spawn_blocking(move || {
        let records = db::ssh_connections::list_records(&db, false)?;
        Ok(records
            .into_iter()
            .filter(|record| record.dto.enabled && record.dto.deleted_at.is_none())
            .map(|record| record.dto.id)
            .collect())
    })
    .await?
}
