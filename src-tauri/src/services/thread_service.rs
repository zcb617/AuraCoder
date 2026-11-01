use anyhow::Context;
use serde_json::json;
use crate::runtime_env;

use crate::services::app_config_service::AppConfigService;
use crate::{
    // config::app_config::AppConfig,
    db,
    models::{
        CliContextUsageDto, PermissionComponentJson, ThreadDto, ThreadStatusDto, ThreadUpdateDto,
    },
    state::AppState,
};

// 纯模型选择规则已归到 BaseCliMcp，ThreadService 中的原 helper 保留如下：
// /// 为指定 CLI 选择尚未开始线程的模型与思考强度。
// pub(crate) fn resolve_unstarted_thread_runtime(
//     engine: &EngineInfoDto,
//     current_model: &str,
//     model_id: Option<String>,
//     reasoning_effort: Option<String>,
// ) -> Result<ThreadRuntimeSelection, String> {
//     // 显式模型优先；未指定时沿用可见默认模型、首个可见模型和当前模型的顺序。
//     let resolved_model = match model_id
//         .map(|value| value.trim().to_string())
//         .filter(|value| !value.is_empty())
//     {
//         Some(value) => value,
//         None => {
//             let visible: Vec<&crate::models::EngineModelDto> =
//                 engine.models.iter().filter(|item| !item.hidden).collect();
//             visible
//                 .iter()
//                 .find(|item| item.is_default)
//                 .map(|item| item.id.clone())
//                 .or_else(|| visible.first().map(|item| item.id.clone()))
//                 .unwrap_or_else(|| current_model.to_string())
//         }
//     };
//
//     // 显式思考强度经过 trim 后优先，否则读取最终模型的 CLI 默认思考强度。
//     let resolved_effort = match reasoning_effort
//         .map(|value| value.trim().to_string())
//         .filter(|value| !value.is_empty())
//     {
//         Some(value) => Some(value),
//         None => engine
//             .models
//             .iter()
//             .find(|item| item.id == resolved_model)
//             .map(|item| item.default_reasoning_effort.clone())
//             .filter(|value| !value.is_empty()),
//     };
//
//     Ok(ThreadRuntimeSelection {
//         engine_id: engine.id.clone(),
//         model_id: resolved_model,
//         reasoning_effort: resolved_effort,
//     })
// }

/// 提供线程统一权限对象 A 的业务校验、默认设置同步和数据库读写。
pub struct ThreadService {
    /// 当前应用状态，提供数据库和全局配置写锁。
    state: AppState,
}

impl ThreadService {
    /// 创建绑定应用状态的线程业务 Service。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 启动时原子恢复遗留消息和线程状态，供应用入口在构造 AppState 前调用。
    pub fn recover_runtime_state(
        db: &db::Database,
    ) -> anyhow::Result<db::threads::RuntimeRecoveryReport> {
        let result = db::threads::with_runtime_recovery_transaction(db, |tx| {
            // 先将上次异常退出留下的 assistant streaming 消息归一为 interrupted。
            let messages_marked_interrupted =
                db::threads::mark_streaming_assistant_messages_interrupted(tx)?;
            // 一次读取所有线程恢复事实，状态业务判断留在 Service。
            let facts = db::threads::list_runtime_recovery_facts(tx)?;
            let mut thread_status_updates = 0usize;
            for fact in facts {
                let target_status = derive_recovered_thread_status(
                    fact.has_pending_approval,
                    fact.last_assistant_status.as_deref(),
                );
                thread_status_updates += db::threads::update_recovered_thread_status(
                    tx,
                    &fact.thread_id,
                    target_status,
                )?;
            }
            Ok(db::threads::RuntimeRecoveryReport {
                messages_marked_interrupted,
                thread_status_updates,
            })
        });
        if let Err(error) = &result {
            log::error!("启动时恢复运行线程状态失败: {error:#}");
        }
        result
    }

    /// 按工作区读取活动线程，并返回包含统一权限对象 A 的线程 DTO。
    pub async fn list_threads(&self, workspace_id: &str) -> anyhow::Result<Vec<ThreadDto>> {
        let log_workspace_id = workspace_id.to_owned();
        let db = self.state.db.clone();
        let query_workspace_id = log_workspace_id.clone();
        let result = async move {
            tokio::task::spawn_blocking(move || {
                db::threads::list_threads_for_workspace(&db, &query_workspace_id)
            })
            .await
            .context("读取工作区线程列表的数据库任务失败")?
            .context("读取工作区线程列表失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("读取工作区 {} 的线程列表失败: {error:#}", log_workspace_id);
        }
        result
    }

    /// 按工作区读取已归档线程，并返回包含统一权限对象 A 的线程 DTO。
    pub async fn list_archived_threads(
        &self,
        workspace_id: &str,
    ) -> anyhow::Result<Vec<ThreadDto>> {
        let log_workspace_id = workspace_id.to_owned();
        let db = self.state.db.clone();
        let query_workspace_id = log_workspace_id.clone();
        let result = async move {
            tokio::task::spawn_blocking(move || {
                db::threads::list_archived_threads_for_workspace(&db, &query_workspace_id)
            })
            .await
            .context("读取工作区归档线程列表的数据库任务失败")?
            .context("读取工作区归档线程列表失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("读取工作区 {} 的归档线程列表失败: {error:#}", log_workspace_id);
        }
        result
    }

    /// 保存线程字段；权限对象 A 的新线程默认值变化时同步全局新线程设置。
    pub async fn update_thread(&self, update: ThreadUpdateDto) -> anyhow::Result<ThreadDto> {
        let log_thread_id = update.id.clone();
        let state = self.state.clone();
        let result = async move {
            // 先校验完整权限对象，确保无效 A 不会触发全局配置副作用。
            if let Some(permission_mode) = update.permission_mode.as_ref() {
                permission_mode
                    .validate()
                    .context("线程统一权限对象 A 校验失败")?;
            }

            // 只有默认预设字段变化时才同步全局新线程设置。
            if let Some(permission_mode) = update.permission_mode.as_ref() {
                let db = state.db.clone();
                let query_thread_id = update.id.clone();
                let current_thread = tokio::task::spawn_blocking(move || {
                    db::threads::get_thread(&db, &query_thread_id)?
                        .context("待更新线程不存在")
                })
                .await
                .context("读取线程当前统一权限对象 A 的数据库任务失败")??;
                let default_value_changed = current_thread
                    .permission_mode
                    .as_ref()
                    .is_none_or(|current| {
                        current.default_for_new_threads != permission_mode.default_for_new_threads
                    });

                if default_value_changed {
                    let default_autonomy_preset = match permission_mode
                        .default_for_new_threads
                        .first()
                        .map(String::as_str)
                    {
                        None | Some("automatic") => None,
                        Some(value)
                            if crate::config::app_config::VALID_AUTONOMY_PRESETS.contains(&value) =>
                        {
                            Some(value.to_owned())
                        }
                        Some(value) => anyhow::bail!("未知统一新线程默认预设: {value}"),
                    };

                    // 配置写入与现有全局设置入口共用同一把锁，避免并发覆盖。
                    let config_write_lock = state.config_write_lock.clone();
                    let _guard = config_write_lock.lock_owned().await;
                    tokio::task::spawn_blocking(move || {
                        // AppConfig::mutate(|config| {
                        AppConfigService::mutate(|config| {
                            config.general.default_autonomy_preset = default_autonomy_preset.clone();
                            Ok(())
                        })
                    })
                    .await
                    .context("更新全局新线程默认预设的任务失败")?
                    .context("更新全局新线程默认预设失败")?;
                }
            }

            // 持久化更新后的线程 DTO 和统一权限对象 A。
            let db = state.db.clone();
            tokio::task::spawn_blocking(move || db::threads::update_thread(&db, &update))
                .await
                .context("更新线程记录的数据库任务失败")?
                .context("更新线程记录失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("更新线程 {} 失败: {error:#}", log_thread_id);
        }
        result
    }

    /// 按线程标识读取数据库中的统一权限对象 A。
    pub async fn get_permissions(&self, thread_id: &str) -> anyhow::Result<PermissionComponentJson> {
        let log_thread_id = thread_id.to_owned();
        let db = self.state.db.clone();
        let query_thread_id = log_thread_id.clone();
        let result = async move {
            let thread = tokio::task::spawn_blocking(move || {
                db::threads::get_thread(&db, &query_thread_id)?.context("待读取线程不存在")
            })
            .await
            .context("读取线程统一权限对象 A 的数据库任务失败")??;

            thread
                .permission_mode
                .context("线程统一权限对象 A 不存在")
        }
        .await;
        if let Err(error) = &result {
            log::error!("读取线程 {} 的统一权限对象 A 失败: {error:#}", log_thread_id);
        }
        result
    }

    /// 校验并按线程标识保存统一权限对象 A，返回更新后的完整线程 DTO。
    pub async fn save_permissions(
        &self,
        thread_id: &str,
        values: PermissionComponentJson,
    ) -> anyhow::Result<ThreadDto> {
        let log_thread_id = thread_id.to_owned();
        let db = self.state.db.clone();
        let query_thread_id = log_thread_id.clone();
        let result = async move {
            values.validate().context("线程统一权限对象 A 校验失败")?;
            tokio::task::spawn_blocking(move || {
                db::threads::update_thread_permissions(&db, &query_thread_id, Some(&values))
            })
            .await
            .context("保存线程统一权限对象 A 的数据库任务失败")?
            .context("保存线程统一权限对象 A 失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("保存线程 {} 的统一权限对象 A 失败: {error:#}", log_thread_id);
        }
        result
    }
}

/// 按启动恢复业务规则推导线程目标状态，pending 审批优先于末条 assistant 状态。
fn derive_recovered_thread_status(
    has_pending_approval: bool,
    last_assistant_status: Option<&str>,
) -> ThreadStatusDto {
    if has_pending_approval {
        return ThreadStatusDto::AwaitingApproval;
    }
    match last_assistant_status {
        Some("error") => ThreadStatusDto::Error,
        Some("completed") => ThreadStatusDto::Completed,
        _ => ThreadStatusDto::Idle,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/thread_runtime_recovery_tests.rs"]
mod runtime_recovery_tests;

const MAX_THREAD_TITLE_CHARS: usize = 120;

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
T: Send + 'static,
F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|error| error.to_string())?
        .map_err(err_to_string)
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

pub(crate) fn normalize_thread_title(raw: &str) -> Result<String, String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return Err("thread title cannot be empty".to_string());
    }

    let title = if trimmed.chars().count() > MAX_THREAD_TITLE_CHARS {
        trimmed
            .chars()
            .take(MAX_THREAD_TITLE_CHARS)
            .collect::<String>()
    } else {
        trimmed.to_string()
    };

    Ok(title)
}

impl ThreadService {
    /// 只读取线程业务记录，供 Controller 选择接口实现及公共业务使用。
    pub async fn get_thread(&self, thread_id: &str) -> anyhow::Result<ThreadDto> {
        let query_id = thread_id.to_owned();
        let db = self.state.db.clone();
        let result = tokio::task::spawn_blocking(move || {
                db::threads::get_thread(&db, &query_id)?.context("线程不存在")
        }).await.context("读取线程数据库任务失败").and_then(|result| result);
        if let Err(error) = &result { log::error!("读取线程 {thread_id} 失败: {error:#}"); }
        result
    }

    /// 为未开始的线程准备并保存指定 CLI 的运行时选择。
    pub async fn update_unstarted_thread_runtime(
        &self,
        thread_id: String,
        engine_id: String,
        model_id: Option<String>,
        reasoning_effort: Option<String>,
    ) -> anyhow::Result<ThreadDto> {
        let result: anyhow::Result<ThreadDto> = async {
            // 读取线程原记录，并在任何运行时准备或保存前验证空会话条件。
            let query_thread_id = thread_id.clone();
            let db = self.state.db.clone();
            let thread = tokio::task::spawn_blocking(move || {
                db::threads::get_thread(&db, &query_thread_id)
            })
            .await??
            .ok_or_else(|| anyhow::anyhow!("thread not found"))?;
            anyhow::ensure!(
                thread.message_count == 0 && thread.engine_thread_id.is_none(),
                "thread already started; runtime cannot be changed"
            );

            // 经工厂取得目标 CLI，由其自身目录规则解析新模型和思考强度。
            let cli = crate::cli_tools::factory::CliToolFactory::new(self.state.clone())
                .create(&engine_id)?;
            let selection = cli
                .prepare_unstarted_thread_runtime(&thread.model_id, model_id, reasoning_effort)
                .await?;

            // 只更新引擎、模型与思考强度，保持 plan/send/权限字段不参与本次保存。
            let db = self.state.db.clone();
            let updated_thread_id = thread_id.clone();
            tokio::task::spawn_blocking(move || {
                db::threads::update_thread_runtime_selection(
                    &db,
                    &updated_thread_id,
                    &selection.engine_id,
                    &selection.model_id,
                    None,
                    None,
                    selection.reasoning_effort.as_deref(),
                    None,
                )
            })
            .await?
        }
        .await;

        if let Err(error) = &result {
            log::error!("更新未开始线程运行时失败: {error:#}");
        }
        result
    }

    /// 根据数据库中的线程归属取得接口，不包含任何具体 CLI 业务分支。
    pub async fn cli_for_thread(&self, thread_id: &str) -> anyhow::Result<std::sync::Arc<dyn crate::cli_tools::CliTool>> {
        let thread = self.get_thread(thread_id).await?;
        crate::cli_tools::factory::CliToolFactory::new(self.state.clone()).create(&thread.engine_id)
    }

    /// 按线程归属选择 CLI 并读取该线程的上下文用量。
    pub async fn get_cli_context_usage(
        &self,
        thread_id: &str,
    ) -> anyhow::Result<Option<CliContextUsageDto>> {
        let thread_id = thread_id.trim();
        let result: anyhow::Result<Option<CliContextUsageDto>> = async {
            // 拒绝空线程标识后，按线程记录中的 CLI 归属选择统一接口。
            anyhow::ensure!(!thread_id.is_empty(), "线程标识不能为空");
            let cli = self
                .cli_for_thread(thread_id)
                .await
                .context("按线程归属获取 CLI 失败")?;

            // 将线程标识交给当前 CLI，由实现类读取既有用量来源。
            cli.get_context_usage(thread_id)
                .await
                .context("读取线程上下文用量失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("读取线程 {thread_id} 的 CLI 上下文用量失败: {error:#}");
        }
        result
    }

    /// 创建业务统一入口，供页面、定时任务和远程调用；具体业务由目标 CLI 实现。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_thread(&self, workspace_id: String, engine_id: String, model_id: String, title: String, reasoning_effort: Option<String>, service_tier: Option<String>) -> anyhow::Result<ThreadDto> {
        let cli = crate::cli_tools::factory::CliToolFactory::new(self.state.clone()).create(&engine_id)?;
        cli.create_thread_record(workspace_id, model_id, title, reasoning_effort, service_tier).await
    }

    pub async fn rename_thread(&self, thread_id: String, title: String) -> Result<ThreadDto, String> {
        let state = &self.state;

        let db = state.db.clone();
        let thread = run_db(db.clone(), {
                let thread_id = thread_id.clone();
                move |db| db::threads::get_thread(db, &thread_id)
        })
            .await?
            .ok_or_else(|| format!("thread not found: {thread_id}"))?;

        let normalized_title = normalize_thread_title(&title)?;

        run_db(db.clone(), {
                let thread_id = thread_id.clone();
                let normalized_title = normalized_title.clone();
                move |db| db::threads::update_thread_title(db, &thread_id, &normalized_title)
        })
            .await?;

        let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
        if !metadata.is_object() {
            metadata = json!({});
        }

        if let Some(object) = metadata.as_object_mut() {
            object.insert("manualTitle".to_string(), json!(true));
            object.insert(
                    "manualTitleUpdatedAt".to_string(),
                json!(runtime_env::system_time_rfc3339()),
            );
        }

        run_db(db.clone(), {
                let thread_id = thread_id.clone();
                let metadata = metadata.clone();
                move |db| db::threads::update_engine_metadata(db, &thread_id, &metadata)
        })
            .await?;

        run_db(db, {
                let thread_id = thread_id.clone();
                move |db| db::threads::get_thread(db, &thread_id)
        })
            .await?
            .ok_or_else(|| format!("thread not found after rename: {thread_id}"))

    }

    pub async fn archive_thread_locally(&self, thread_id: &str) -> Result<(), String> {
        let state = &self.state;

        state.turns.cancel(thread_id).await;
        let db = state.db.clone();
        let thread_id = thread_id.to_string();
        let db_thread_id = thread_id.clone();
        let result = run_db(db, move |db| db::threads::archive_thread(db, &db_thread_id)).await;
        state.turns.finish(&thread_id).await;
        result

    }
}
