use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;

use crate::{
    cli_tools::factory::CliToolFactory,
    db::{self, scheduled_tasks::ScheduledTaskWrite, Database},
    models::{
        ScheduledRuntimeConfig, ScheduledTaskDto, ScheduledTaskInput, ThreadDto,
    },
    scheduled_tasks::schedule::initial_next_run_at,
    state::AppState,
};

/// 承载定时任务 CRUD、运行参数校验和调度器通知的业务服务。
pub struct ScheduledTaskService {
    /// 当前应用数据库及定时任务调度器依赖。
    state: AppState,
}

/// 定时任务更新事件载荷，保持前端接收的 taskId 字段契约。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledTaskEvent {
    /// 已更新或删除的定时任务标识。
    task_id: String,
}

impl ScheduledTaskService {
    /// 创建绑定当前应用状态的定时任务业务服务。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 在阻塞线程中执行定时任务 Mapper，并保留数据库和 Join 错误链。
    async fn run_db<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> Result<T> + Send + 'static,
    {
        let database = self.state.db.clone();
        tokio::task::spawn_blocking(move || operation(&database))
            .await
            .context("定时任务数据库操作线程失败")?
    }

    /// 发出定时任务更新事件；事件发送失败只记录日志，不回滚已保存的数据。
    fn emit_updated(&self, app: &tauri::AppHandle, task_id: &str) {
        let event = ScheduledTaskEvent {
            task_id: task_id.to_string(),
        };
        if let Err(error) = app.emit("scheduled-task-updated", event) {
            log::warn!("发送定时任务更新事件失败: task_id={task_id} error={error:#}");
        }
    }

    /// 查询全部定时任务并记录数据库原始错误。
    pub async fn list_scheduled_tasks(&self) -> Result<Vec<ScheduledTaskDto>> {
        let result = async {
            // 在线程池中读取任务列表，完成后返回 Mapper 查询结果。
            self.run_db(db::scheduled_tasks::list_tasks)
                .await
                .context("查询定时任务列表失败")
        }
        .await;
        result.inspect_err(|error| log::error!("查询定时任务列表失败: {error:#}"))
    }

    /// 校验并创建定时任务，保存后唤醒调度器并通知前端刷新。
    pub async fn create_scheduled_task(
        &self,
        app: tauri::AppHandle,
        input: ScheduledTaskInput,
    ) -> Result<ScheduledTaskDto> {
        let result = async {
            // 在访问数据库前完成任务字段、工作区、线程和 CLI 运行参数校验。
            let write = self.validate_and_prepare(input).await?;

            // 持久化定时任务后按既有顺序唤醒调度器并发送更新事件。
            let task = self
                .run_db(move |database| db::scheduled_tasks::create_task(database, &write))
                .await
                .context("创建定时任务失败")?;
            self.state.scheduled_tasks.wake();
            self.emit_updated(&app, &task.id);
            Ok(task)
        }
        .await;
        result.inspect_err(|error| log::error!("创建定时任务失败: {error:#}"))
    }

    /// 校验并更新定时任务，保存后唤醒调度器并通知前端刷新。
    pub async fn update_scheduled_task(
        &self,
        app: tauri::AppHandle,
        task_id: String,
        input: ScheduledTaskInput,
    ) -> Result<ScheduledTaskDto> {
        let result = async {
            // 在访问数据库前完成任务字段、工作区、线程和 CLI 运行参数校验。
            let write = self.validate_and_prepare(input).await?;
            let task_id_for_db = task_id.clone();

            // 保存任务后按既有顺序唤醒调度器并发送更新事件。
            let task = self
                .run_db(move |database| {
                    db::scheduled_tasks::update_task(database, &task_id_for_db, &write)
                })
                .await
                .context("更新定时任务失败")?;
            self.state.scheduled_tasks.wake();
            self.emit_updated(&app, &task_id);
            Ok(task)
        }
        .await;
        result.inspect_err(|error| log::error!("更新定时任务失败: {error:#}"))
    }

    /// 启用或禁用定时任务，并按既有规则刷新下一次执行时间。
    pub async fn set_scheduled_task_enabled(
        &self,
        app: tauri::AppHandle,
        task_id: String,
        enabled: bool,
    ) -> Result<ScheduledTaskDto> {
        let result = async {
            // 先读取任务以保留启用时按原周期计算首次运行时间的行为。
            let existing_id = task_id.clone();
            let existing = self
                .run_db(move |database| db::scheduled_tasks::get_task(database, &existing_id))
                .await
                .context("读取待启停的定时任务失败")?
                .ok_or_else(|| anyhow::anyhow!("定时任务不存在: {task_id}"))?;
            let next_run_at = if enabled {
                Some(
                    initial_next_run_at(
                        &existing.schedule_type,
                        &existing.schedule,
                        &existing.timezone,
                        Utc::now(),
                    )
                    .map_err(anyhow::Error::msg)?
                    .to_rfc3339(),
                )
            } else {
                None
            };
            let task_id_for_db = task_id.clone();

            // 写入启停状态和计算结果后刷新调度器及前端任务状态。
            let task = self
                .run_db(move |database| {
                    db::scheduled_tasks::set_task_enabled(
                        database,
                        &task_id_for_db,
                        enabled,
                        next_run_at.as_deref(),
                    )
                })
                .await
                .context("更新定时任务启用状态失败")?;
            self.state.scheduled_tasks.wake();
            self.emit_updated(&app, &task_id);
            Ok(task)
        }
        .await;
        result.inspect_err(|error| log::error!("更新定时任务启用状态失败: {error:#}"))
    }

    /// 确认定时任务最近一次运行结果，并按既有顺序刷新任务状态。
    pub async fn acknowledge_scheduled_task(
        &self,
        app: tauri::AppHandle,
        task_id: String,
    ) -> Result<ScheduledTaskDto> {
        let result = async {
            // 先确认最近一次运行，再唤醒调度器并通知任务状态变化。
            let task_id_for_db = task_id.clone();
            self.run_db(move |database| {
                db::scheduled_tasks::acknowledge_latest_run(database, &task_id_for_db)
            })
            .await
            .context("确认定时任务运行结果失败")?;
            self.state.scheduled_tasks.wake();
            self.emit_updated(&app, &task_id);
            let task_id_for_db = task_id.clone();

            // 在确认事件完成后重新读取任务，返回最新的确认状态。
            self.run_db(move |database| {
                db::scheduled_tasks::get_task(database, &task_id_for_db)?
                    .ok_or_else(|| anyhow::anyhow!("定时任务不存在: {task_id_for_db}"))
            })
            .await
            .context("读取已确认的定时任务失败")
        }
        .await;
        result.inspect_err(|error| log::error!("确认定时任务运行结果失败: {error:#}"))
    }

    /// 删除定时任务，删除成功后唤醒调度器并发送删除事件。
    pub async fn delete_scheduled_task(
        &self,
        app: tauri::AppHandle,
        task_id: String,
    ) -> Result<()> {
        let result = async {
            // 通过 Mapper 删除任务，并将不存在状态转成中文业务错误。
            let task_id_for_db = task_id.clone();
            let deleted = self
                .run_db(move |database| db::scheduled_tasks::delete_task(database, &task_id_for_db))
                .await
                .context("删除定时任务失败")?;
            anyhow::ensure!(deleted, "定时任务不存在: {task_id}");

            // 删除成功后先刷新调度器，再发送兼容原事件名和载荷的通知。
            self.state.scheduled_tasks.wake();
            let event = ScheduledTaskEvent { task_id };
            if let Err(error) = app.emit("scheduled-task-deleted", event) {
                log::warn!("发送定时任务删除事件失败: error={error:#}");
            }
            Ok(())
        }
        .await;
        result.inspect_err(|error| log::error!("删除定时任务失败: {error:#}"))
    }

    /// 校验任务目标和 CLI 运行配置，并生成 Mapper 使用的保存对象。
    async fn validate_and_prepare(&self, input: ScheduledTaskInput) -> Result<ScheduledTaskWrite> {
        // 先完成不依赖数据库或 CLI 的任务字段校验。
        let (description, workspace_id) = validate_task_fields(&input)?;

        // 确认任务工作区仍存在，沿用原有不按本地或远端类型筛选的规则。
        let workspace_id_for_db = workspace_id.clone();
        let workspace_exists = self
            .run_db(move |database| {
                Ok(db::workspaces::list_workspaces(database)?
                    .iter()
                    .any(|workspace| workspace.id == workspace_id_for_db))
            })
            .await
            .context("查询定时任务工作区失败")?;
        anyhow::ensure!(workspace_exists, "定时任务工作区不可用");

        // existing_thread 目标必须选中当前工作区内未归档的线程。
        let thread_id = input
            .thread_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let selected_thread = if input.target_type == "existing_thread" {
            let thread_id_value = thread_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请选择要复用的现有线程"))?;
            let workspace_id_for_db = workspace_id.clone();
            let thread_id_for_db = thread_id_value.to_string();
            let thread = self
                .run_db(move |database| {
                    Ok(db::threads::list_threads_for_workspace(database, &workspace_id_for_db)?
                        .into_iter()
                        .find(|thread| thread.id == thread_id_for_db))
                })
                .await
                .context("查询定时任务目标线程失败")?;
            Some(thread.ok_or_else(|| anyhow::anyhow!("所选线程不可用或已归档"))?)
        } else {
            None
        };

        // 优先使用显式运行配置，否则只从选中线程提取四个既有运行字段。
        let runtime_value = input
            .runtime_config
            .or_else(|| selected_thread.as_ref().map(runtime_config_from_thread));
        let runtime_value = runtime_value
            .ok_or_else(|| anyhow::anyhow!("定时任务运行配置不能为空"))?;
        let mut runtime: ScheduledRuntimeConfig =
            serde_json::from_value(runtime_value).context("定时任务运行配置格式无效")?;
        runtime.engine_id = runtime.engine_id.trim().to_string();
        anyhow::ensure!(!runtime.engine_id.is_empty(), "定时任务执行 CLI 不能为空");

        // 由工厂取得当前 CLI，并让该 CLI 自行校验和规范化任务运行配置。
        let cli = CliToolFactory::new(self.state.clone())
            .create(&runtime.engine_id)
            .context("获取定时任务执行 CLI 失败")?;
        runtime = cli.prepare_scheduled_runtime_config(runtime).await?;
        if selected_thread
            .as_ref()
            .is_some_and(|thread| thread.engine_id != runtime.engine_id)
        {
            anyhow::bail!("所选线程与定时任务执行 CLI 不一致");
        }

        // 按当前时区验证周期，并将下次运行时间交给现有 Mapper 格式保存。
        let timezone = input.timezone.trim().to_string();
        let next_run_at = if input.enabled {
            Some(
                initial_next_run_at(
                    &input.schedule_type,
                    &input.schedule,
                    &timezone,
                    Utc::now(),
                )
                .map_err(anyhow::Error::msg)?
                .to_rfc3339(),
            )
        } else {
            crate::scheduled_tasks::schedule::validate_schedule(
                &input.schedule_type,
                &input.schedule,
                &timezone,
            )
            .map_err(anyhow::Error::msg)?;
            None
        };

        // 保持 new_thread 的 threadId 为空，并序列化共享运行配置供 Mapper 保存。
        let target_type = input.target_type;
        let existing_thread_target = target_type == "existing_thread";
        let runtime_config = serde_json::to_value(runtime).context("序列化定时任务运行配置失败")?;
        Ok(ScheduledTaskWrite {
            description,
            enabled: input.enabled,
            execution_device_id: input.execution_device_id,
            target_type,
            workspace_id,
            thread_id: if existing_thread_target { thread_id } else { None },
            runtime_config: Some(runtime_config),
            schedule_type: input.schedule_type,
            schedule: input.schedule,
            timezone,
            next_run_at,
        })
    }
}

/// 校验定时任务基础字段并返回去除首尾空白的描述和工作区标识。
fn validate_task_fields(input: &ScheduledTaskInput) -> Result<(String, String)> {
    // 按既有顺序校验描述长度、执行设备、目标类型和工作区标识。
    let description = input.description.trim().to_string();
    anyhow::ensure!(!description.is_empty(), "定时任务描述不能为空");
    anyhow::ensure!(
        description.chars().count() <= 20_000,
        "定时任务描述不能超过 20000 个字符"
    );
    anyhow::ensure!(
        input.execution_device_id == "local",
        "当前仅支持本机执行定时任务"
    );
    anyhow::ensure!(
        matches!(input.target_type.as_str(), "existing_thread" | "new_thread"),
        "定时任务目标类型无效"
    );
    let workspace_id = input.workspace_id.trim().to_string();
    anyhow::ensure!(!workspace_id.is_empty(), "定时任务工作区不能为空");
    Ok((description, workspace_id))
}

/// 从现有线程提取定时任务运行所需的 CLI、模型、推理强度和服务档位。
fn runtime_config_from_thread(thread: &ThreadDto) -> Value {
    // 只快照当前任务使用的四个字段，不复制线程权限或其它 metadata。
    let metadata = thread.engine_metadata.as_ref();
    serde_json::json!({
        "engineId": thread.engine_id,
        "modelId": thread.model_id,
        "reasoningEffort": thread.reasoning_effort,
        "serviceTier": metadata
            .and_then(|value| value.get("serviceTier"))
            .and_then(Value::as_str),
    })
}

#[cfg(test)]
#[path = "../../tests/unit/scheduled_task_service_tests.rs"]
mod tests;
