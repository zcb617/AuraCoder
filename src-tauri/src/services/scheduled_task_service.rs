use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;
use tokio::time::sleep;

use std::time::Duration;

use crate::{
    cli_tools::factory::CliToolFactory,
    db::{self, scheduled_tasks::ScheduledTaskWrite, Database},
    models::{
        chat::SendMessageRequest, MessageStatusDto, ScheduledRuntimeConfig, ScheduledTaskDto,
        ScheduledTaskInput, ScheduledTaskRunDto, ThreadDto,
    },
    runtime_env,
    scheduled_tasks::schedule::{initial_next_run_at, next_run_after_due},
    services::chat_service::ChatService,
    state::AppState,
};

const SCHEDULER_RECHECK_DELAY: Duration = Duration::from_secs(15);
const SCHEDULER_IDLE_DELAY: Duration = Duration::from_secs(6 * 60 * 60);
const THREAD_BUSY_RETRY_DELAY: Duration = Duration::from_secs(5);
const THREAD_BUSY_RETRY_COUNT: usize = 12;

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

    /// 判断是否存在启用且无需确认即可运行的定时任务，供应用退出确认使用。
    pub fn has_enabled_tasks(&self) -> anyhow::Result<bool> {
        // 查询任务并沿用退出策略：数据库异常返回错误，由退出调用方要求确认。
        match db::scheduled_tasks::list_tasks(&self.state.db) {
            Ok(tasks) => Ok(has_runnable_enabled_tasks(&tasks)),
            Err(error) => {
                log::error!("查询退出前定时任务失败: {error:#}");
                Err(error)
            }
        }
    }

    /// 恢复上次退出时仍处于 queued 或 running 状态的定时任务记录。
    pub(crate) async fn recover_interrupted_runs(&self) -> Result<(), String> {
        // 交由同模块的原恢复流程执行，保留原返回和错误文本。
        recover_interrupted_runs(&self.state).await
    }

    /// 按下一条到期时间和原重查规则计算调度器等待时长。
    pub(crate) async fn next_scheduler_delay(&self) -> Duration {
        // 复用原等待时长计算，避免在调度管理器中保留数据库业务。
        next_scheduler_delay(&self.state).await
    }

    /// 查询到期任务、认领运行记录并按原顺序通知和启动任务执行。
    pub(crate) async fn process_due_tasks(&self, app: tauri::AppHandle) -> Result<(), String> {
        // 将 AppHandle 借给原处理流程，所有 Mapper 操作仍由 Service 内部执行。
        process_due_tasks(&app, &self.state).await
    }

    /// 结算助手消息关联的定时任务运行，并在找到任务时通知调度器刷新。
    pub(crate) async fn finish_chat_run(
        &self,
        app: &tauri::AppHandle,
        assistant_message_id: &str,
        status: &MessageStatusDto,
        preview: Option<String>,
    ) -> anyhow::Result<()> {
        // 保留原消息终态结算流程及其事件、唤醒顺序。
        finish_chat_run(app, &self.state, assistant_message_id, status, preview).await
    }

    /// 发出定时任务更新事件，保持事件名和 taskId 载荷供前端刷新。
    pub(crate) fn emit_task_updated(app: &tauri::AppHandle, task_id: &str) {
        // 复用统一事件载荷，发送失败只记录原始异常。
        let event = ScheduledTaskEvent {
            task_id: task_id.to_string(),
        };
        if let Err(error) = app.emit("scheduled-task-updated", event) {
            log::warn!("发送定时任务更新事件失败: task_id={task_id} error={error:#}");
        }
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
        Self::emit_task_updated(app, task_id);
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

/// 判断任务列表中是否存在启用且无需确认即可运行的任务。
fn has_runnable_enabled_tasks(tasks: &[ScheduledTaskDto]) -> bool {
    // 只把已经启用且无需用户确认的任务纳入退出前确认判断。
    tasks
        .iter()
        .any(|task| task.enabled && !task.needs_confirmation)
}

/// 将上次退出时尚未完成的定时任务运行恢复为可重新调度状态。
async fn recover_interrupted_runs(state: &AppState) -> Result<(), String> {
    // 在线程池中调用既有恢复 Mapper，并保留原始 Join 和数据库错误文本。
    let db = state.db.clone();
    let recovered =
        tokio::task::spawn_blocking(move || db::scheduled_tasks::recover_interrupted_runs(&db))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    if recovered > 0 {
        log::info!("recovered {recovered} interrupted scheduled task runs");
    }
    Ok(())
}

/// 按数据库中的下一到期时间计算调度等待时长，并保留原重查间隔。
async fn next_scheduler_delay(state: &AppState) -> Duration {
    // 在线程池中读取下一到期时间；缺失或数据库异常沿用原空闲等待时长。
    let db = state.db.clone();
    let next = tokio::task::spawn_blocking(move || db::scheduled_tasks::next_due_at(&db)).await;
    let Ok(Ok(Some(next))) = next else {
        return SCHEDULER_IDLE_DELAY;
    };

    // 非法持久化时间采用原短间隔重查；有效时间保留最短一秒等待规则。
    let Ok(next) = runtime_env::parse_persisted_time_to_utc(&next) else {
        return SCHEDULER_RECHECK_DELAY;
    };
    next.signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or(SCHEDULER_RECHECK_DELAY)
        .max(Duration::from_secs(1))
}

/// 查询并认领已经到期的任务，再异步启动每个认领任务的执行流程。
async fn process_due_tasks(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    // 使用原本地时间格式读取到期任务，保留数据库 Mapper 的筛选行为。
    let now = Utc::now();
    let now_rfc3339 = runtime_env::system_time_rfc3339();
    let db = state.db.clone();
    let due =
        tokio::task::spawn_blocking(move || db::scheduled_tasks::list_due_tasks(&db, &now_rfc3339))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

    // 逐项按原周期计算下一次时间并通过事务认领，认领成功后再通知和启动执行。
    for task in due {
        let Some(scheduled_for_raw) = task.next_run_at.as_deref() else {
            continue;
        };
        let scheduled_for = runtime_env::parse_persisted_time_to_utc(scheduled_for_raw)
            .map_err(|error| {
                format!(
                    "invalid scheduled task next_run_at {:?}: {error}",
                    scheduled_for_raw
                )
            })?;
        let next_run = next_run_after_due(
            &task.schedule_type,
            &task.schedule,
            &task.timezone,
            scheduled_for,
            now,
        )?;
        let db = state.db.clone();
        let task_id = task.id.clone();
        let scheduled_for_raw = scheduled_for_raw.to_string();
        let scheduled_for_string = runtime_env::normalize_time_to_local(&scheduled_for.to_rfc3339())
            .map_err(|error| {
                format!(
                    "failed to normalize scheduled task next_run_at {:?}: {error}",
                    scheduled_for_raw
                )
            })?;
        let next_run_raw = next_run.to_rfc3339();
        let next_run_string = runtime_env::normalize_time_to_local(&next_run_raw).map_err(|error| {
            format!(
                "failed to normalize scheduled task computed next_run_at {:?}: {error}",
                next_run_raw
            )
        })?;
        let run = tokio::task::spawn_blocking(move || {
            db::scheduled_tasks::claim_due_task(
                &db,
                &task_id,
                &scheduled_for_string,
                &next_run_string,
            )
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        let Some(run) = run else {
            continue;
        };

        ScheduledTaskService::emit_task_updated(app, &task.id);
        let app = app.clone();
        let state = state.clone();
        tauri::async_runtime::spawn(async move {
            execute_claimed_task(&app, &state, task, run).await;
        });
    }
    Ok(())
}

/// 执行已经认领的定时任务，并将失败结果保存到原运行记录。
async fn execute_claimed_task(
    app: &tauri::AppHandle,
    state: &AppState,
    task: ScheduledTaskDto,
    run: ScheduledTaskRunDto,
) {
    // 复用原执行流程；发生业务错误时记录失败状态并通知前端刷新。
    let result = execute_claimed_task_inner(app, state, &task, &run).await;
    if let Err(error) = result {
        let db = state.db.clone();
        let run_id = run.id.clone();
        let error_for_db = error.clone();
        if let Err(db_error) = tokio::task::spawn_blocking(move || {
            db::scheduled_tasks::mark_run_error(&db, &run_id, &error_for_db)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            log::warn!("failed to persist scheduled task execution error: {db_error}");
        }
        ScheduledTaskService::emit_task_updated(app, &task.id);
    }
}

/// 按任务目标准备线程，保留繁忙重试、跳过和定时消息发送顺序。
async fn execute_claimed_task_inner(
    app: &tauri::AppHandle,
    state: &AppState,
    task: &ScheduledTaskDto,
    run: &ScheduledTaskRunDto,
) -> Result<(), String> {
    // 当前仅允许本机执行，保持原不支持设备的业务错误。
    if task.execution_device_id != "local" {
        return Err("This scheduled task targets an unsupported execution device.".to_string());
    }

    // 根据既有线程或新建线程目标取得线程和快照运行配置。
    let (thread, runtime) = match task.target_type.as_str() {
        "existing_thread" => {
            let thread = resolve_existing_thread(state, task).await?;
            let runtime = runtime_config_for_task(task, Some(&thread))?;
            if thread.engine_id != runtime.engine_id {
                return Err(
                    "The selected thread does not use the configured execution agent.".to_string(),
                );
            }
            (thread, runtime)
        }
        "new_thread" => {
            let runtime = runtime_config_for_task(task, None)?;
            let thread = create_scheduled_thread(state, task, run, &runtime).await?;
            (thread, runtime)
        }
        _ => return Err("Scheduled task target type is invalid.".to_string()),
    };

    // 按原次数和间隔等待目标线程空闲；持续繁忙时写入 skipped 并结束本次任务。
    for attempt in 0..THREAD_BUSY_RETRY_COUNT {
        if state.turns.get(&thread.id).await.is_none() {
            break;
        }
        if attempt + 1 == THREAD_BUSY_RETRY_COUNT {
            let db = state.db.clone();
            let run_id = run.id.clone();
            tokio::task::spawn_blocking(move || {
                db::scheduled_tasks::mark_run_skipped(
                    &db,
                    &run_id,
                    "The target thread remained busy for one minute.",
                )
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
            ScheduledTaskService::emit_task_updated(app, &task.id);
            return Ok(());
        }
        sleep(THREAD_BUSY_RETRY_DELAY).await;
    }

    // 使用现有 ChatService 按定时任务原参数发送消息并写入运行关联 ID。
    ChatService::new(state.clone()).send_message(
        app.clone(),
        SendMessageRequest {
            thread_id: thread.id.clone(),
            message: task.description.clone(),
            engine_id: Some(runtime.engine_id),
            model_id: Some(runtime.model_id),
            plan_mode: Some(false),
            send_method: thread.send_method.clone(),
            reasoning_effort: runtime.reasoning_effort,
            permission_values: None,
            attachments: None,
            input_items: None,
            client_turn_id: Some(format!("scheduled:{}", run.id)),
            scheduled_run_id: Some(run.id.clone()),
            referenced_thread_id: None,
        },
    )
    .await
    .map_err(|error| error.message)?;
    ScheduledTaskService::emit_task_updated(app, &task.id);
    Ok(())
}

/// 查询定时任务原有的、同工作区且未归档的目标线程。
async fn resolve_existing_thread(
    state: &AppState,
    task: &ScheduledTaskDto,
) -> Result<ThreadDto, String> {
    // 在线程池中调用原线程 Mapper，并保留不可用线程的业务错误。
    let db = state.db.clone();
    let task = task.clone();
    tokio::task::spawn_blocking(move || db::scheduled_tasks::active_thread_for_task(&db, &task))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The selected thread is unavailable or archived.".to_string())
}

/// 按任务描述和计划时间创建定时任务目标线程。
async fn create_scheduled_thread(
    state: &AppState,
    task: &ScheduledTaskDto,
    run: &ScheduledTaskRunDto,
    runtime: &ScheduledRuntimeConfig,
) -> Result<ThreadDto, String> {
    // 使用原标题裁剪和计划时间后缀生成线程标题。
    let title_line = task
        .description
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Scheduled task")
        .trim();
    let mut title = title_line.chars().take(72).collect::<String>();
    let scheduled_suffix = DateTime::parse_from_rfc3339(&run.scheduled_for)
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| run.scheduled_for.clone());
    title.push_str(" · ");
    title.push_str(&scheduled_suffix);

    // 复用 ThreadService 创建线程并传递任务快照中的运行字段。
    crate::services::thread_service::ThreadService::new(state.clone()).create_thread(
        task.workspace_id.clone(),
        runtime.engine_id.clone(),
        runtime.model_id.clone(),
        title,
        runtime.reasoning_effort.clone(),
        runtime.service_tier.clone(),
    )
    .await
    .map_err(|error| format!("{error:#}"))
}

/// 读取任务保存的运行配置，兼容从现有线程补齐旧任务配置。
fn runtime_config_for_task(
    task: &ScheduledTaskDto,
    existing_thread: Option<&ThreadDto>,
) -> Result<ScheduledRuntimeConfig, String> {
    // 优先解析任务快照；仅旧记录缺少快照时从选中线程提取字段。
    if let Some(value) = task.runtime_config.clone() {
        return serde_json::from_value(value)
            .map_err(|error| format!("Scheduled task runtime configuration is invalid: {error}"));
    }

    let thread = existing_thread
        .ok_or_else(|| "Scheduled task runtime configuration is missing.".to_string())?;
    Ok(ScheduledRuntimeConfig {
        engine_id: thread.engine_id.clone(),
        model_id: thread.model_id.clone(),
        reasoning_effort: thread.reasoning_effort.clone(),
        service_tier: thread
            .engine_metadata
            .as_ref()
            .and_then(|value| value.get("serviceTier"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// 按助手消息 ID 结算关联运行，并在存在任务关联时广播和唤醒调度器。
async fn finish_chat_run(
    app: &tauri::AppHandle,
    state: &AppState,
    assistant_message_id: &str,
    status: &MessageStatusDto,
    preview: Option<String>,
) -> anyhow::Result<()> {
    // 仅将真实终态映射为定时任务状态；流式状态不代表本轮已经结束。
    let run_status = match status {
        MessageStatusDto::Completed => "completed",
        MessageStatusDto::Interrupted => "interrupted",
        MessageStatusDto::Error => "error",
        MessageStatusDto::Streaming => {
            log::warn!("skip scheduled task completion for streaming assistant message {assistant_message_id}");
            return Ok(());
        }
    };
    let error_message = matches!(status, MessageStatusDto::Error)
        .then_some("The scheduled task turn ended with an error.");

    // 通过 Mapper 按助手消息定位并结算定时任务运行记录。
    let db = state.db.clone();
    let assistant_message_id = assistant_message_id.to_string();
    let task_id = tauri::async_runtime::spawn_blocking(move || {
        db::scheduled_tasks::finish_run_by_message(
            &db,
            &assistant_message_id,
            run_status,
            preview.as_deref(),
            error_message,
        )
    })
    .await??;

    // 仅在确有关联任务时广播更新并唤醒调度器。
    if let Some(task_id) = task_id {
        ScheduledTaskService::emit_task_updated(app, &task_id);
        state.scheduled_tasks.wake();
    }
    Ok(())
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

#[cfg(test)]
#[path = "../../tests/unit/scheduled_task_manager_tests.rs"]
mod manager_tests;
