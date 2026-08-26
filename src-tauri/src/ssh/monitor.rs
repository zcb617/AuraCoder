use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context;
use tokio::{
    io::AsyncReadExt,
    net::TcpStream,
    process::Child,
    sync::Mutex,
    task::JoinHandle,
    time::{sleep, Duration, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    db::{self, ssh_connections::SshConnectionRecord, Database},
    ssh::gateway,
};

const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(12);
const SESSION_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
pub struct SshConnectionMonitor {
    tasks: Arc<Mutex<HashMap<String, MonitorTask>>>,
}

struct MonitorTask {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl SshConnectionMonitor {
    pub async fn start(&self, db: Database, connection_id: &str) -> anyhow::Result<()> {
        let connection_id = connection_id.to_string();
        let record = load_record(db.clone(), connection_id.clone()).await?;
        let Some(record) = record else {
            return Ok(());
        };
        if !is_monitorable(&record) {
            return Ok(());
        }

        let mut tasks = self.tasks.lock().await;
        if tasks.contains_key(&connection_id) {
            return Ok(());
        }

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_id = connection_id.clone();
        let handle = tokio::spawn(async move {
            run_monitor(db, task_id, task_cancel).await;
        });
        tasks.insert(connection_id, MonitorTask { cancel, handle });
        Ok(())
    }

    pub async fn stop(&self, connection_id: &str) {
        let task = self.tasks.lock().await.remove(connection_id);
        let Some(task) = task else {
            return;
        };
        task.cancel.cancel();
        let _ = task.handle.await;
    }

    pub async fn reconcile(&self, db: Database) -> anyhow::Result<()> {
        let records = load_records(db.clone()).await?;
        let desired = records
            .iter()
            .filter(|record| is_monitorable(record))
            .map(|record| record.dto.id.clone())
            .collect::<HashSet<_>>();
        let tracked = self.tasks.lock().await.keys().cloned().collect::<Vec<_>>();

        for connection_id in tracked {
            if !desired.contains(&connection_id) {
                self.stop(&connection_id).await;
            }
        }
        for record in records {
            if desired.contains(&record.dto.id) {
                self.start(db.clone(), &record.dto.id).await?;
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        let connection_ids = self.tasks.lock().await.keys().cloned().collect::<Vec<_>>();
        for connection_id in connection_ids {
            self.stop(&connection_id).await;
        }
    }
}

async fn run_monitor(db: Database, connection_id: String, cancel: CancellationToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }

        let Some(record) = (match load_record(db.clone(), connection_id.clone()).await {
            Ok(record) => record,
            Err(error) => {
                log::warn!("failed to load SSH connection {connection_id} for monitoring: {error}");
                return;
            }
        }) else {
            return;
        };
        if !is_monitorable(&record) {
            return;
        }
        let version = record.dto.updated_at.clone();

        if !set_status(
            db.clone(),
            &connection_id,
            &version,
            db::ssh_connections::STATUS_CONNECTING,
            None,
        )
        .await
        {
            return;
        }

        let (mut child, local_port) = match gateway::open_monitor_session(&record).await {
            Ok(session) => session,
            Err(error) => {
                if !set_status(
                    db.clone(),
                    &connection_id,
                    &version,
                    db::ssh_connections::STATUS_FAILED,
                    Some(&error.to_string()),
                )
                .await
                {
                    return;
                }
                continue;
            }
        };

        match wait_until_ready(&mut child, local_port, &cancel).await {
            ReadyState::Cancelled => return,
            ReadyState::Exited(error) => {
                if !set_status(
                    db.clone(),
                    &connection_id,
                    &version,
                    db::ssh_connections::STATUS_FAILED,
                    Some(&error),
                )
                .await
                {
                    return;
                }
                continue;
            }
            ReadyState::Ready => {}
        }

        if !set_status(
            db.clone(),
            &connection_id,
            &version,
            db::ssh_connections::STATUS_OK,
            None,
        )
        .await
        {
            terminate_child(&mut child).await;
            return;
        }

        let exit_result = tokio::select! {
            _ = cancel.cancelled() => {
                terminate_child(&mut child).await;
                return;
            }
            result = child.wait() => result,
        };
        let error = match exit_result {
            Ok(status) => {
                let stderr = read_stderr(&mut child).await;
                if stderr.is_empty() {
                    format!("SSH 连接已断开（{status}）")
                } else {
                    stderr
                }
            }
            Err(error) => error.to_string(),
        };
        if !set_status(
            db.clone(),
            &connection_id,
            &version,
            db::ssh_connections::STATUS_FAILED,
            Some(&error),
        )
        .await
        {
            return;
        }
        // 用户要求连接失败后立即重连，这里不使用退避计时器。
    }
}

enum ReadyState {
    Ready,
    Exited(String),
    Cancelled,
}

async fn wait_until_ready(
    child: &mut Child,
    local_port: u16,
    cancel: &CancellationToken,
) -> ReadyState {
    let deadline = Instant::now() + SESSION_READY_TIMEOUT;
    loop {
        if cancel.is_cancelled() {
            terminate_child(child).await;
            return ReadyState::Cancelled;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                return ReadyState::Exited(
                    read_stderr(child)
                        .await
                        .if_empty_then(|| format!("SSH 连接建立失败（{status}）")),
                );
            }
            Ok(None) => {}
            Err(error) => return ReadyState::Exited(error.to_string()),
        }

        if TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
            return ReadyState::Ready;
        }

        if Instant::now() >= deadline {
            terminate_child(child).await;
            return ReadyState::Exited("SSH 连接建立超时".to_string());
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                terminate_child(child).await;
                return ReadyState::Cancelled;
            }
            _ = sleep(SESSION_READY_POLL_INTERVAL) => {}
        }
    }
}

async fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
}

async fn read_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if stderr.read_to_end(&mut bytes).await.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).trim().to_string()
}

async fn load_record(
    db: Database,
    connection_id: String,
) -> anyhow::Result<Option<SshConnectionRecord>> {
    tokio::task::spawn_blocking(move || db::ssh_connections::find(&db, &connection_id))
        .await
        .context("SSH connection monitor database task failed")?
}

async fn load_records(db: Database) -> anyhow::Result<Vec<SshConnectionRecord>> {
    tokio::task::spawn_blocking(move || db::ssh_connections::list_records(&db, false))
        .await
        .context("SSH connection monitor database task failed")?
}

async fn set_status(
    db: Database,
    connection_id: &str,
    version: &str,
    status: &str,
    error: Option<&str>,
) -> bool {
    let connection_id = connection_id.to_string();
    let version = version.to_string();
    let status = status.to_string();
    let error = error.map(str::to_string);
    match tokio::task::spawn_blocking(move || {
        db::ssh_connections::set_status_if_current(
            &db,
            &connection_id,
            &version,
            &status,
            error.as_deref(),
        )
    })
    .await
    {
        Ok(Ok(changed)) => changed,
        Ok(Err(error)) => {
            log::warn!("failed to update SSH connection monitor status: {error}");
            false
        }
        Err(error) => {
            log::warn!("SSH connection monitor database task failed: {error}");
            false
        }
    }
}

fn is_monitorable(record: &SshConnectionRecord) -> bool {
    record.dto.enabled && record.dto.deleted_at.is_none()
}

trait EmptyStringFallback {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String;
}

impl EmptyStringFallback for String {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}
