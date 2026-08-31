use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tauri::State;

use crate::{
    cli_tools::{
        factory::CliToolFactory, CliExecutionContext, CliSessionNotFoundError, CliSessionSnapshot,
        CliTool,
    },
    config::app_config::AppConfig,
    db,
    engines::validate_engine_sandbox_mode,
    engines::CodexRemoteThreadSummary,
    engines::ModelInfo,
    engines::OpenCodeRemoteSessionSummary,
    engines::SandboxPolicy,
    engines::ThreadSyncSnapshot,
    models::{
        CodexRemoteThreadDto, CodexRemoteThreadPageDto, MessageStatusDto, OpenCodeRemoteSessionDto,
        OpenCodeRemoteSessionPageDto, PermissionComponentJson, ThreadDto, ThreadStatusDto,
        ThreadUpdateDto, TrustLevelDto,
    },
    path_utils::paths_equal,
    state::AppState,
};

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

async fn codex_external_sandbox_for_workspace(
    state: &AppState,
    workspace: &crate::models::WorkspaceDto,
) -> Result<bool, String> {
    let context = CliExecutionContext::from_workspace(workspace).map_err(err_to_string)?;
    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    cli.uses_external_sandbox(&context)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn list_threads(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<ThreadDto>, String> {
    run_db(state.db.clone(), move |db| {
        db::threads::list_threads_for_workspace(db, &workspace_id)
    })
    .await
}

#[tauri::command]
pub async fn list_archived_threads(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<ThreadDto>, String> {
    run_db(state.db.clone(), move |db| {
        db::threads::list_archived_threads_for_workspace(db, &workspace_id)
    })
    .await
}

/// 通过统一局部更新 DTO 保存线程运行时字段。
#[tauri::command]
pub async fn update_thread(
    state: State<'_, AppState>,
    update: ThreadUpdateDto,
) -> Result<ThreadDto, String> {
    run_db(state.db.clone(), move |db| {
        db::threads::update_thread(db, &update)
    })
    .await
}

/// 持久化会话底部 6 项运行时选择。
///
/// 前端传什么就写什么，不做回退、不与 engine_metadata_json 合并，
/// 让底部状态以独立字段为唯一数据源。
#[tauri::command]
pub async fn update_thread_runtime_selection(
    state: State<'_, AppState>,
    thread_id: String,
    engine_id: String,
    model_id: String,
    plan_mode: Option<bool>,
    send_method: Option<String>,
    reasoning_effort: Option<String>,
    permission_mode: Option<String>,
) -> Result<ThreadDto, String> {
    run_db(state.db.clone(), move |db| {
        db::threads::update_thread_runtime_selection(
            db,
            &thread_id,
            &engine_id,
            &model_id,
            plan_mode,
            send_method.as_deref(),
            reasoning_effort.as_deref(),
            permission_mode.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub async fn list_codex_remote_threads(
    state: State<'_, AppState>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    search_term: Option<String>,
    archived: Option<bool>,
) -> Result<CodexRemoteThreadPageDto, String> {
    let db = state.db.clone();
    let workspace_root = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| {
            let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
            Ok(workspace.root_path)
        }
    })
    .await?;

    let normalized_search_term = normalize_remote_thread_search_term(search_term);
    let workspace = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    let matching_threads = cli
        .list_sessions(&context, normalized_search_term.as_deref(), archived)
        .await
        .map_err(err_to_string)?
        .into_iter()
        .filter(|thread| codex_remote_thread_belongs_to_workspace(&workspace_root, &thread.cwd))
        .collect::<Vec<_>>();

    let offset = parse_codex_remote_thread_cursor(cursor.as_deref())?;
    let page_size = normalize_codex_remote_thread_limit(limit);
    let page_end = offset.saturating_add(page_size).min(matching_threads.len());
    let page_threads = if offset >= matching_threads.len() {
        Vec::new()
    } else {
        matching_threads[offset..page_end].to_vec()
    };
    let next_cursor = (page_end < matching_threads.len()).then(|| page_end.to_string());

    run_db(db, move |db| {
        let threads = page_threads
            .into_iter()
            .map(|thread| {
                let local_thread_id = db::threads::find_thread_by_engine_thread_id(
                    db,
                    "codex",
                    &thread.engine_thread_id,
                )?
                .filter(|local| local.workspace_id == workspace_id)
                .map(|local| local.id);
                Ok(map_codex_remote_thread_dto(thread, local_thread_id))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(CodexRemoteThreadPageDto {
            threads,
            next_cursor,
        })
    })
    .await
}

#[tauri::command]
pub async fn attach_codex_remote_thread(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_thread_id: String,
    // 旧接口接收前端选中的 model_id，会覆盖 Codex 历史会话自己的模型；禁止恢复。
    // model_id: String,
) -> Result<ThreadDto, String> {
    // let normalized_model_id =
    //     validate_model_for_engine(state.inner(), "codex", model_id.trim()).await?;
    let db = state.db.clone();
    let existing_local_thread = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        let engine_thread_id = engine_thread_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
            let existing =
                db::threads::find_thread_by_engine_thread_id(db, "codex", &engine_thread_id)?
                    .filter(|thread| thread.workspace_id == workspace_id);
            Ok(existing)
        }
    })
    .await?;

    let workspace = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    let session = cli
        .read_session(&context, &engine_thread_id)
        .await
        .map_err(err_to_string)?;
    let validation_models = cli
        .models_for_validation(&context, &session.model_id)
        .await
        .ok();
    // Codex Desktop / vscode 创建的会话 model 为 null（映射后为 "unknown"），不在
    // 模型目录中。目录校验失败不得中断导入：优先回退到目录默认模型，目录不可用
    // 时保留会话原值，保证这类会话能正常发现并显示。
    // 旧实现校验失败直接返回错误，导致无 model 的会话被整体丢弃、项目下"没有会话"：
    // let normalized_model_id = validate_model_for_engine_from_catalog(
    //     "codex",
    //     session.model_id.trim(),
    //     validation_models.as_deref(),
    // )?;
    let normalized_model_id = match validate_model_for_engine_from_catalog(
        "codex",
        session.model_id.trim(),
        validation_models.as_deref(),
    ) {
        Ok(model_id) => model_id,
        Err(error) => {
            let fallback = validation_models.as_deref().and_then(|models| {
                models
                    .iter()
                    .find(|model| model.is_default && !model.hidden)
                    .or_else(|| models.iter().find(|model| !model.hidden))
                    .or_else(|| models.first())
            });
            match fallback {
                Some(model) => {
                    log::warn!(
                        "Codex 会话模型不在模型目录中，导入回退到目录默认模型: engine_thread_id={} session_model={} fallback_model={} error={}",
                        engine_thread_id,
                        session.model_id.trim(),
                        model.id,
                        error,
                    );
                    model.id.clone()
                }
                None => {
                    log::warn!(
                        "Codex 会话模型校验失败且无模型目录可回退，导入保留会话原模型: engine_thread_id={} session_model={} error={}",
                        engine_thread_id,
                        session.model_id.trim(),
                        error,
                    );
                    session.model_id.trim().to_string()
                }
            }
        }
    };
    let normalized_reasoning_effort = session
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    // reasoning_effort 校验失败同样不得中断导入：忽略该值、保留会话本体。
    // 旧实现直接返回错误：
    // let validated_reasoning_effort = normalized_reasoning_effort
    //     .as_deref()
    //     .map(|value| {
    //         validate_reasoning_effort_from_catalog(
    //             &normalized_model_id,
    //             value,
    //             validation_models.as_deref(),
    //         )
    //     })
    //     .transpose()?;
    let validated_reasoning_effort = normalized_reasoning_effort
        .as_deref()
        .map(|value| {
            validate_reasoning_effort_from_catalog(
                &normalized_model_id,
                value,
                validation_models.as_deref(),
            )
        })
        .transpose()
        .unwrap_or_else(|error| {
            log::warn!(
                "Codex 会话 reasoning_effort 不受支持，导入时忽略该值: engine_thread_id={} error={}",
                engine_thread_id,
                error,
            );
            None
        });
    let model_provider = session
        .metadata
        .get("codexModelProvider")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let parse_timestamp = |value: Option<String>| {
        value
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
            .unwrap_or_default()
    };
    let mut remote_thread = CodexRemoteThreadSummary {
        engine_thread_id: session.engine_thread_id.clone(),
        title: Some(session.title.clone()),
        preview: session.preview.unwrap_or_default(),
        cwd: session.cwd,
        created_at: parse_timestamp(session.created_at),
        updated_at: parse_timestamp(session.updated_at),
        model_id: Some(normalized_model_id.clone()),
        reasoning_effort: validated_reasoning_effort.clone(),
        model_provider,
        source_kind: session.source_kind.unwrap_or_default(),
        status_type: session.raw_status.unwrap_or_default(),
        active_flags: session.active_flags,
        archived: session.archived,
    };
    if remote_thread.archived {
        let operation_thread = existing_local_thread.clone().unwrap_or_else(|| ThreadDto {
            id: engine_thread_id.clone(),
            workspace_id: workspace.id.clone(),
            engine_id: "codex".to_string(),
            model_id: normalized_model_id.clone(),
            engine_thread_id: Some(engine_thread_id.clone()),
            engine_metadata: None,
            plan_mode: None,
            send_method: None,
            reasoning_effort: None,
            permission_mode: None,
            title: remote_thread
                .title
                .clone()
                .unwrap_or_else(|| engine_thread_id.clone()),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            created_at: String::new(),
            last_activity_at: String::new(),
        });
        cli.unarchive_thread(&context, &operation_thread, &engine_thread_id)
            .await
            .map_err(err_to_string)?;
        remote_thread.archived = false;
    }
    let title = build_codex_remote_thread_title(&remote_thread);
    let metadata = build_codex_remote_thread_metadata(
        &remote_thread,
        &normalized_model_id,
        validated_reasoning_effort.as_deref(),
    );
    // 只有远端返回并成功解析的更新时间才覆盖本地活动时间，避免写入 Unix epoch。
    let remote_last_activity_at = (remote_thread.updated_at > 0)
        .then(|| codex_remote_thread_timestamp_to_rfc3339(remote_thread.updated_at));

    let existing_last_activity_at = remote_last_activity_at.clone();
    if let Some(existing) = existing_local_thread {
        return run_db(db, move |db| {
            let thread = match db::threads::restore_thread(db, &existing.id) {
                Ok(restored) => restored,
                Err(_) => existing,
            };
            let updated = db::threads::update_thread_runtime_snapshot(
                db,
                &thread.id,
                Some(&title),
                map_codex_thread_status_to_local(
                    Some(remote_thread.status_type.as_str()),
                    &remote_thread.active_flags,
                    false,
                ),
                Some(&metadata),
            )?;
            // 模型以用户在 AuraCoder 中的选择为准：本地已有具体模型时保留，仅本地模型未知
            // （空或 "unknown"，如首次导入历史会话）时才接受 Codex 会话返回的模型，
            // 与 SSH 快照路径的保护语义一致。旧行为无条件用 Codex 会话模型覆盖，
            // 导致用户改模型后每次重启 attach 同步即被重置：
            // db::threads::update_thread_runtime_selection(
            //     db,
            //     &updated.id,
            //     "codex",
            //     &normalized_model_id,
            //     None,
            //     None,
            //     validated_reasoning_effort.as_deref(),
            //     None,
            // )?;
            let local_model_id = updated.model_id.trim();
            let effective_model_id = if local_model_id.is_empty() || local_model_id == "unknown" {
                normalized_model_id.as_str()
            } else {
                local_model_id
            };
            // CLI 服务端不返回计划/发送方法/权限；模型按上方保护规则、思考强度按其真实返回落库。
            db::threads::update_thread_runtime_selection(
                db,
                &updated.id,
                "codex",
                effective_model_id,
                None,
                None,
                validated_reasoning_effort.as_deref(),
                None,
            )?;
            match existing_last_activity_at.as_deref() {
                Some(last_activity_at) => {
                    db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
                }
                None => Ok(updated),
            }
        })
        .await;
    }

    run_db(db, move |db| {
        let created =
            db::threads::create_thread(db, &workspace_id, "codex", &normalized_model_id, &title)?;
        db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
        let updated = db::threads::update_thread_runtime_snapshot(
            db,
            &created.id,
            Some(&title),
            map_codex_thread_status_to_local(
                Some(remote_thread.status_type.as_str()),
                &remote_thread.active_flags,
                false,
            ),
            Some(&metadata),
        )?;
        // CLI 服务端不返回计划/发送方法/权限，仅按其真实返回的模型与思考强度落库。
        db::threads::update_thread_runtime_selection(
            db,
            &updated.id,
            "codex",
            &normalized_model_id,
            None,
            None,
            validated_reasoning_effort.as_deref(),
            None,
        )?;
        match remote_last_activity_at.as_deref() {
            Some(last_activity_at) => {
                db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
            }
            None => Ok(updated),
        }
    })
    .await
}

#[tauri::command]
pub async fn list_opencode_remote_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    search_term: Option<String>,
    archived: Option<bool>,
) -> Result<OpenCodeRemoteSessionPageDto, String> {
    let db = state.db.clone();
    let workspace = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| {
            let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
            Ok(workspace)
        }
    })
    .await?;

    let allowed_roots = collect_remote_thread_roots(&workspace.root_path);
    let normalized_search_term = normalize_remote_thread_search_term(search_term);
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let opencode = CliToolFactory::new(state.inner().clone())
        .create("opencode")
        .expect("OpenCode CLI factory mapping must exist");
    let cli: &dyn CliTool = opencode.as_ref();
    let parse_timestamp = |value: Option<String>| {
        value
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
            .unwrap_or_default()
    };
    let mut remote_sessions = cli
        .list_sessions(&context, normalized_search_term.as_deref(), archived)
        .await
        .map_err(err_to_string)?
        .into_iter()
        .filter(|session| allowed_roots.contains(session.cwd.as_str()))
        .map(|session| OpenCodeRemoteSessionSummary {
            engine_thread_id: session.engine_thread_id,
            title: Some(session.title),
            cwd: session.cwd,
            created_at: parse_timestamp(session.created_at),
            updated_at: parse_timestamp(session.updated_at),
            archived: session.archived,
        })
        .collect::<Vec<_>>();
    remote_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let mut seen_session_ids = std::collections::HashSet::new();
    remote_sessions.retain(|session| seen_session_ids.insert(session.engine_thread_id.clone()));

    let offset = parse_codex_remote_thread_cursor(cursor.as_deref())?;
    let page_size = normalize_codex_remote_thread_limit(limit);
    let page_end = offset.saturating_add(page_size).min(remote_sessions.len());
    let page_sessions = if offset >= remote_sessions.len() {
        Vec::new()
    } else {
        remote_sessions[offset..page_end].to_vec()
    };
    let next_cursor = (page_end < remote_sessions.len()).then(|| page_end.to_string());

    run_db(db, move |db| {
        let sessions = page_sessions
            .into_iter()
            .map(|session| {
                let local_thread_id = db::threads::find_thread_by_engine_thread_id(
                    db,
                    "opencode",
                    &session.engine_thread_id,
                )?
                .filter(|local| local.workspace_id == workspace_id)
                .map(|local| local.id);
                Ok(map_opencode_remote_session_dto(session, local_thread_id))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(OpenCodeRemoteSessionPageDto {
            sessions,
            next_cursor,
        })
    })
    .await
}

#[tauri::command]
pub async fn attach_opencode_remote_session(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_thread_id: String,
    cwd: String,
    model_id: String,
) -> Result<ThreadDto, String> {
    let normalized_model_id =
        validate_model_for_engine(state.inner(), "opencode", model_id.trim()).await?;
    let db = state.db.clone();
    let (workspace, existing_local_thread) = run_db(db.clone(), {
        let workspace_id = workspace_id.clone();
        let engine_thread_id = engine_thread_id.clone();
        move |db| {
            let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
            let existing =
                db::threads::find_thread_by_engine_thread_id(db, "opencode", &engine_thread_id)?
                    .filter(|thread| thread.workspace_id == workspace_id);
            Ok((workspace, existing))
        }
    })
    .await?;

    let allowed_roots = collect_remote_thread_roots(&workspace.root_path);
    if !allowed_roots.contains(cwd.as_str()) {
        return Err(format!(
            "OpenCode session cwd is outside this workspace: {cwd}"
        ));
    }

    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let opencode = CliToolFactory::new(state.inner().clone())
        .create("opencode")
        .expect("OpenCode CLI factory mapping must exist");
    let cli: &dyn CliTool = opencode.as_ref();
    let session = cli
        .read_session(&context, &engine_thread_id)
        .await
        .map_err(err_to_string)?;
    if !paths_equal(&session.cwd, &cwd) {
        return Err(format!(
            "OpenCode session cwd does not match: expected={cwd} actual={}",
            session.cwd
        ));
    }
    let parse_timestamp = |value: Option<String>| {
        value
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
            .unwrap_or_default()
    };
    let mut remote_session = OpenCodeRemoteSessionSummary {
        engine_thread_id: session.engine_thread_id,
        title: Some(session.title),
        cwd: session.cwd,
        created_at: parse_timestamp(session.created_at),
        updated_at: parse_timestamp(session.updated_at),
        archived: session.archived,
    };
    if remote_session.archived {
        let operation_thread = existing_local_thread.clone().unwrap_or_else(|| ThreadDto {
            id: engine_thread_id.clone(),
            workspace_id: workspace.id.clone(),
            engine_id: "opencode".to_string(),
            model_id: normalized_model_id.clone(),
            engine_thread_id: Some(engine_thread_id.clone()),
            engine_metadata: Some(json!({ "opencodeRemoteCwd": cwd })),
            plan_mode: None,
            send_method: None,
            reasoning_effort: None,
            permission_mode: None,
            title: remote_session
                .title
                .clone()
                .unwrap_or_else(|| engine_thread_id.clone()),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            created_at: String::new(),
            last_activity_at: String::new(),
        });
        cli.unarchive_thread(&context, &operation_thread, &engine_thread_id)
            .await
            .map_err(err_to_string)?;
        remote_session.archived = false;
    }
    let title = build_opencode_remote_session_title(&remote_session);
    // 只有远端返回并成功解析的更新时间才覆盖本地活动时间，避免写入 Unix epoch。
    let remote_last_activity_at = (remote_session.updated_at > 0)
        .then(|| codex_remote_thread_timestamp_to_rfc3339(remote_session.updated_at));

    if let Some(existing) = existing_local_thread {
        let metadata = build_opencode_remote_session_metadata(
            existing.engine_metadata.as_ref(),
            &remote_session,
            &normalized_model_id,
        );
        let existing_last_activity_at = remote_last_activity_at.clone();
        return run_db(db, move |db| {
            let thread = match db::threads::restore_thread(db, &existing.id) {
                Ok(restored) => restored,
                Err(_) => existing,
            };
            let updated = db::threads::update_thread_runtime_snapshot(
                db,
                &thread.id,
                Some(&title),
                Some(ThreadStatusDto::Idle),
                Some(&metadata),
            )?;
            // OpenCode 远端会话只返回模型，其余底部项服务端不提供，按真实返回值落库。
            db::threads::update_thread_runtime_selection(
                db,
                &updated.id,
                "opencode",
                &normalized_model_id,
                None,
                None,
                None,
                None,
            )?;
            match existing_last_activity_at.as_deref() {
                Some(last_activity_at) => {
                    db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
                }
                None => Ok(updated),
            }
        })
        .await;
    }

    let metadata =
        build_opencode_remote_session_metadata(None, &remote_session, &normalized_model_id);
    run_db(db, move |db| {
        let created = db::threads::create_thread(
            db,
            &workspace_id,
            "opencode",
            &normalized_model_id,
            &title,
        )?;
        db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
        let updated = db::threads::update_thread_runtime_snapshot(
            db,
            &created.id,
            Some(&title),
            Some(ThreadStatusDto::Idle),
            Some(&metadata),
        )?;
        // OpenCode 远端会话只返回模型，其余底部项服务端不提供，按真实返回值落库。
        db::threads::update_thread_runtime_selection(
            db,
            &updated.id,
            "opencode",
            &normalized_model_id,
            None,
            None,
            None,
            None,
        )?;
        match remote_last_activity_at.as_deref() {
            Some(last_activity_at) => {
                db::threads::update_thread_last_activity(db, &updated.id, last_activity_at)
            }
            None => Ok(updated),
        }
    })
    .await
}

pub(crate) async fn validate_model_for_engine(
    state: &AppState,
    engine_id: &str,
    requested_model_id: &str,
) -> Result<String, String> {
    let normalized_model_id = requested_model_id.trim();
    if normalized_model_id.is_empty() {
        return Err("model id cannot be empty".to_string());
    }

    let models = if engine_id == "codex" {
        let codex = CliToolFactory::new(state.clone())
            .create("codex")
            .expect("Codex CLI factory mapping must exist");
        match codex.execution_context(None).await {
            Ok(context) => {
                let cli: &dyn CliTool = codex.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .ok()
            }
            Err(_) => None,
        }
    } else if engine_id == "opencode" {
        let opencode = CliToolFactory::new(state.clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        match opencode.execution_context(None).await {
            Ok(context) => {
                let cli: &dyn CliTool = opencode.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .ok()
            }
            Err(_) => None,
        }
    } else if engine_id == "claude" {
        let claude = CliToolFactory::new(state.clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        match claude.execution_context(None).await {
            Ok(context) => {
                let cli: &dyn CliTool = claude.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .ok()
            }
            Err(_) => None,
        }
    } else {
        state
            .engines
            .models_for_validation(engine_id, normalized_model_id)
            .await
            .ok()
    };

    validate_model_for_engine_from_catalog(engine_id, normalized_model_id, models.as_deref())
}

fn validate_model_for_engine_from_catalog(
    engine_id: &str,
    normalized_model_id: &str,
    models: Option<&[ModelInfo]>,
) -> Result<String, String> {
    let Some(models) = models else {
        return Ok(normalized_model_id.to_string());
    };

    if models.iter().any(|model| model.id == normalized_model_id) {
        return Ok(normalized_model_id.to_string());
    }

    let available = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "model `{normalized_model_id}` is not supported by engine `{engine_id}`. available models: {available}"
    ))
}

async fn resolve_thread_cwd(state: &AppState, thread: &ThreadDto) -> Result<String, String> {
    let workspace_id = thread.workspace_id.clone();
    let thread_id = thread.id.clone();

    run_db(state.db.clone(), move |db| {
        let workspace = db::workspaces::find_workspace_by_id(db, &workspace_id)?
            .ok_or_else(|| anyhow::anyhow!("workspace not found for thread {thread_id}"))?;
        Ok(workspace.root_path)
    })
    .await
}

fn collect_remote_thread_roots(workspace_root: &str) -> std::collections::HashSet<String> {
    std::iter::once(workspace_root.to_string()).collect()
}

fn codex_remote_thread_belongs_to_workspace(workspace_root: &str, cwd: &str) -> bool {
    paths_equal(cwd, workspace_root)
}

fn normalize_remote_thread_search_term(search_term: Option<String>) -> Option<String> {
    search_term
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parse_codex_remote_thread_cursor(cursor: Option<&str>) -> Result<usize, String> {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(0);
    };

    cursor.parse::<usize>().map_err(|_| {
        format!("invalid Codex remote thread cursor `{cursor}`. expected a non-negative offset")
    })
}

fn normalize_codex_remote_thread_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(20).clamp(1, 100) as usize
}

fn map_codex_remote_thread_dto(
    thread: CliSessionSnapshot,
    local_thread_id: Option<String>,
) -> CodexRemoteThreadDto {
    let model_provider = thread
        .metadata
        .get("codexModelProvider")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    CodexRemoteThreadDto {
        engine_thread_id: thread.engine_thread_id,
        title: Some(thread.title),
        preview: thread.preview.unwrap_or_default(),
        cwd: thread.cwd,
        created_at: thread.created_at.unwrap_or_default(),
        updated_at: thread.updated_at.unwrap_or_default(),
        model_id: thread.model_id,
        reasoning_effort: thread.reasoning_effort,
        model_provider,
        source_kind: thread.source_kind.unwrap_or_default(),
        status_type: thread.raw_status.unwrap_or_default(),
        active_flags: thread.active_flags,
        archived: thread.archived,
        local_thread_id,
    }
}

fn codex_remote_thread_timestamp_to_rfc3339(timestamp: i64) -> String {
    let (seconds, nanos) = if timestamp > 10_000_000_000 {
        (timestamp / 1000, ((timestamp % 1000) as u32) * 1_000_000)
    } else {
        (timestamp, 0)
    };

    DateTime::<Utc>::from_timestamp(seconds, nanos)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn build_codex_remote_thread_title(thread: &CodexRemoteThreadSummary) -> String {
    if let Some(title) = thread
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_thread_title(title).unwrap_or_else(|_| {
            format!(
                "Codex thread {}",
                short_thread_label(&thread.engine_thread_id)
            )
        });
    }

    if let Some(preview) = thread
        .preview
        .trim()
        .split('\n')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_thread_title(preview).unwrap_or_else(|_| {
            format!(
                "Codex thread {}",
                short_thread_label(&thread.engine_thread_id)
            )
        });
    }

    format!(
        "Codex thread {}",
        short_thread_label(&thread.engine_thread_id)
    )
}

fn short_thread_label(engine_thread_id: &str) -> String {
    engine_thread_id.chars().take(8).collect()
}

fn build_codex_remote_thread_metadata(
    thread: &CodexRemoteThreadSummary,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> Value {
    let _ = model_id;
    let mut metadata = merge_codex_runtime_metadata(
        None,
        Some(thread.status_type.as_str()),
        &thread.active_flags,
        Some(thread.preview.as_str()),
        true,
        Some("remote_thread_attached"),
    );

    if let Some(object) = metadata.as_object_mut() {
        object.insert("codexTranscriptImported".to_string(), json!(false));
        object.insert(
            "codexModelProvider".to_string(),
            json!(thread.model_provider),
        );
        object.insert("codexSourceKind".to_string(), json!(thread.source_kind));
        object.insert("codexRemoteArchived".to_string(), json!(thread.archived));
        object.insert("codexRemoteCwd".to_string(), json!(thread.cwd));
        object.insert(
            "codexRemoteCreatedAt".to_string(),
            json!(codex_remote_thread_timestamp_to_rfc3339(thread.created_at)),
        );
        object.insert(
            "codexRemoteUpdatedAt".to_string(),
            json!(codex_remote_thread_timestamp_to_rfc3339(thread.updated_at)),
        );
    }

    metadata
}

fn map_opencode_remote_session_dto(
    session: OpenCodeRemoteSessionSummary,
    local_thread_id: Option<String>,
) -> OpenCodeRemoteSessionDto {
    OpenCodeRemoteSessionDto {
        engine_thread_id: session.engine_thread_id,
        title: session.title,
        cwd: session.cwd,
        created_at: codex_remote_thread_timestamp_to_rfc3339(session.created_at),
        updated_at: codex_remote_thread_timestamp_to_rfc3339(session.updated_at),
        archived: session.archived,
        local_thread_id,
    }
}

fn build_opencode_remote_session_title(session: &OpenCodeRemoteSessionSummary) -> String {
    if let Some(title) = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return normalize_thread_title(title).unwrap_or_else(|_| {
            format!(
                "OpenCode session {}",
                short_thread_label(&session.engine_thread_id)
            )
        });
    }

    format!(
        "OpenCode session {}",
        short_thread_label(&session.engine_thread_id)
    )
}

fn build_opencode_remote_session_metadata(
    existing: Option<&Value>,
    session: &OpenCodeRemoteSessionSummary,
    model_id: &str,
) -> Value {
    let mut metadata = existing.cloned().unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        // 线程底部六项已迁移到独立列；远端同步只保留 OpenCode 状态 metadata。
        for key in [
            "lastModelId",
            "reasoningEffort",
            "sandboxApprovalPolicy",
            "sandboxMode",
            "sandboxAllowNetwork",
            "permissionProfile",
            "approvalsReviewer",
            "claudePermissionMode",
            "opencodePermissionMode",
        ] {
            object.remove(key);
        }
        object.insert("opencodeRemoteSessionAttached".to_string(), json!(true));
        object.insert(
            "opencodeRemoteArchived".to_string(),
            json!(session.archived),
        );
        object.insert("opencodeRemoteCwd".to_string(), json!(session.cwd));
        object.insert(
            "opencodeRemoteCreatedAt".to_string(),
            json!(codex_remote_thread_timestamp_to_rfc3339(session.created_at)),
        );
        object.insert(
            "opencodeRemoteUpdatedAt".to_string(),
            json!(codex_remote_thread_timestamp_to_rfc3339(session.updated_at)),
        );
        object.insert("opencodeTranscriptImported".to_string(), json!(false));
    }

    metadata
}

#[derive(Debug, PartialEq)]
struct AutonomyPresetPolicy {
    approval_policy: Value,
    sandbox_mode: Option<&'static str>,
    allow_network: Option<bool>,
}

fn autonomy_policy_for_preset(
    engine_id: &str,
    preset: &str,
    codex_external_sandbox: bool,
) -> Result<AutonomyPresetPolicy, String> {
    let policy = match engine_id {
        "opencode" => AutonomyPresetPolicy {
            approval_policy: json!(match preset {
                "read-only" => "deny",
                "ask" => "ask",
                "auto" | "full" => "allow",
                _ => return Err(format!("unknown autonomy preset: {preset}")),
            }),
            sandbox_mode: None,
            allow_network: None,
        },
        "claude" => match preset {
            "read-only" => AutonomyPresetPolicy {
                approval_policy: json!("restricted"),
                sandbox_mode: Some("read-only"),
                allow_network: Some(false),
            },
            "ask" => AutonomyPresetPolicy {
                approval_policy: json!("standard"),
                sandbox_mode: Some("workspace-write"),
                allow_network: Some(false),
            },
            "auto" => AutonomyPresetPolicy {
                approval_policy: json!("trusted"),
                sandbox_mode: Some("workspace-write"),
                allow_network: None,
            },
            "full" => AutonomyPresetPolicy {
                approval_policy: json!("trusted"),
                sandbox_mode: Some("workspace-write"),
                allow_network: Some(true),
            },
            _ => return Err(format!("unknown autonomy preset: {preset}")),
        },
        "codex" => match preset {
            "read-only" => AutonomyPresetPolicy {
                approval_policy: json!("untrusted"),
                sandbox_mode: (!codex_external_sandbox).then_some("read-only"),
                allow_network: Some(false),
            },
            "ask" => AutonomyPresetPolicy {
                approval_policy: json!("on-request"),
                sandbox_mode: (!codex_external_sandbox).then_some("workspace-write"),
                allow_network: Some(false),
            },
            "auto" => AutonomyPresetPolicy {
                approval_policy: json!("on-request"),
                sandbox_mode: (!codex_external_sandbox).then_some("workspace-write"),
                allow_network: Some(true),
            },
            "full" => AutonomyPresetPolicy {
                approval_policy: json!("never"),
                sandbox_mode: Some("danger-full-access"),
                allow_network: Some(true),
            },
            _ => return Err(format!("unknown autonomy preset: {preset}")),
        },
        _ => {
            return Err(format!(
                "autonomy presets are unsupported for engine: {engine_id}"
            ))
        }
    };

    Ok(policy)
}

/// 创建会话并在失败时记录包含运行时参数的完整后端诊断日志。
#[tauri::command]
pub async fn create_thread(
    state: State<'_, AppState>,
    workspace_id: String,
    engine_id: String,
    model_id: String,
    title: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> Result<ThreadDto, String> {
    let log_workspace_id = workspace_id.clone();
    let log_engine_id = engine_id.clone();
    let log_model_id = model_id.clone();

    match create_thread_with_defaults(
        state.inner(),
        workspace_id,
        engine_id,
        model_id,
        title,
        reasoning_effort,
        service_tier,
    )
    .await
    {
        Ok(thread) => Ok(thread),
        Err(error) => {
            log::error!(
                "operation=create_thread workspace_id={} engine_id={} model_id={} error={error:#}",
                log_workspace_id,
                log_engine_id,
                log_model_id,
            );
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_thread_with_defaults(
    state: &AppState,
    workspace_id: String,
    engine_id: String,
    model_id: String,
    title: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> Result<ThreadDto, String> {
    let default_autonomy_preset = tokio::task::spawn_blocking(|| {
        AppConfig::load_or_create()
            .map(|config| config.default_autonomy_preset().map(ToOwned::to_owned))
            .map_err(err_to_string)
    })
    .await
    .map_err(err_to_string)??;

    create_thread_inner(
        state,
        workspace_id,
        engine_id,
        model_id,
        title,
        reasoning_effort,
        service_tier,
        default_autonomy_preset,
    )
    .await
}

async fn create_thread_inner(
    state: &AppState,
    workspace_id: String,
    engine_id: String,
    model_id: String,
    title: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    initial_autonomy_preset: Option<String>,
) -> Result<ThreadDto, String> {
    let normalized_service_tier = if engine_id == "codex" {
        normalize_thread_service_tier(service_tier)?
    } else {
        let candidate = service_tier
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if candidate.is_some() {
            return Err("service tier is only supported for Codex threads".to_string());
        }
        None
    };
    let normalized_model_id = model_id.trim();
    if normalized_model_id.is_empty() {
        return Err("model id cannot be empty".to_string());
    }
    let target_workspace = run_db(state.db.clone(), {
        let workspace_id = workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    if target_workspace.location_kind == "ssh"
        && !matches!(engine_id.as_str(), "codex" | "opencode" | "claude")
    {
        return Err(format!("SSH 远端项目当前阶段尚未接入 {engine_id} 正式对话"));
    }
    let validation_models = if target_workspace.location_kind == "ssh" {
        Some(match engine_id.as_str() {
            "codex" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let codex = CliToolFactory::new(state.clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let cli: &dyn CliTool = codex.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "opencode" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let opencode = CliToolFactory::new(state.clone())
                    .create("opencode")
                    .expect("OpenCode CLI factory mapping must exist");
                let cli: &dyn CliTool = opencode.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "claude" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let claude = CliToolFactory::new(state.clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let cli: &dyn CliTool = claude.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            _ => unreachable!(),
        })
    } else if engine_id == "claude" {
        let context =
            CliExecutionContext::from_workspace(&target_workspace).map_err(err_to_string)?;
        let claude = CliToolFactory::new(state.clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let cli: &dyn CliTool = claude.as_ref();
        cli.models_for_validation(&context, normalized_model_id)
            .await
            .ok()
    } else {
        state
            .engines
            .models_for_validation(&engine_id, normalized_model_id)
            .await
            .ok()
    };
    let effective_model_id = validate_model_for_engine_from_catalog(
        &engine_id,
        normalized_model_id,
        validation_models.as_deref(),
    )?;
    let normalized_reasoning_effort = reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let validated_reasoning_effort =
        if let Some(requested_effort) = normalized_reasoning_effort.as_deref() {
            Some(validate_reasoning_effort_from_catalog(
                &effective_model_id,
                requested_effort,
                validation_models.as_deref(),
            )?)
        } else {
            None
        };
    let mut metadata = serde_json::Map::new();
    if let Some(value) = normalized_service_tier {
        metadata.insert("serviceTier".to_string(), json!(value));
    }

    // 初始权限预设由 CLI 权限接口处理，不再写入 engine_metadata_json 镜像。
    // 旧实现忽略 initial_autonomy_preset 的语句已停用：let _ = initial_autonomy_preset;

    let metadata = (!metadata.is_empty()).then_some(Value::Object(metadata));

    let mut created = run_db(state.db.clone(), move |db| {
        let created =
            db::threads::create_thread(db, &workspace_id, &engine_id, &effective_model_id, &title)?;
        if let Some(metadata) = metadata.as_ref() {
            db::threads::update_engine_metadata(db, &created.id, metadata)?;
        }
        db::threads::update_thread_runtime_selection(
            db,
            &created.id,
            &engine_id,
            &effective_model_id,
            None,
            None,
            validated_reasoning_effort.as_deref(),
            None,
        )?;
        db::threads::get_thread(db, &created.id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found after insert: {}", created.id))
    })
    .await?;

    if let Some(preset) = initial_autonomy_preset {
        let cli = CliToolFactory::new(state.clone())
            .create(created.engine_id.as_str())
            .map_err(err_to_string)?;
        let context =
            CliExecutionContext::from_workspace(&target_workspace).map_err(err_to_string)?;
        let mut permissions = cli
            .get_permissions(&context, &created)
            .await
            .map_err(err_to_string)?;
        permissions.insert("autonomyPreset".to_string(), json!([preset]));
        let _saved_permissions = cli
            .set_permissions(&context, &created, permissions)
            .await
            .map_err(err_to_string)?;
        let created_id = created.id.clone();
        created = run_db(state.db.clone(), {
            let thread_id = created_id.clone();
            move |db| db::threads::get_thread(db, &thread_id)
        })
        .await?
        .ok_or_else(|| {
            format!("thread not found after initial autonomy preset update: {created_id}")
        })?;
    }

    Ok(created)
}

/// 将尚未启动的会话改为用户首次发送时选择的 CLI 运行时。
///
/// 已建立远端会话或已经写入消息的会话不允许跨 CLI 修改，防止把没有兼容历史
/// 上下文的 CLI 误当成原会话继续使用。
#[tauri::command]
pub async fn reconfigure_unstarted_thread_runtime(
    state: State<'_, AppState>,
    thread_id: String,
    engine_id: String,
    model_id: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let (thread, target_workspace) = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| {
            let thread = db::threads::get_thread(db, &thread_id)?
                .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))?;
            let workspace = db::workspaces::find_workspace_by_id(db, &thread.workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {}", thread.workspace_id))?;
            Ok((thread, workspace))
        }
    })
    .await?;

    if thread.engine_thread_id.is_some() || thread.message_count != 0 {
        return Err("thread has already started and its CLI tool cannot be changed".to_string());
    }

    let normalized_service_tier = if engine_id == "codex" {
        normalize_thread_service_tier(service_tier)?
    } else {
        let candidate = service_tier
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if candidate.is_some() {
            return Err("service tier is only supported for Codex threads".to_string());
        }
        None
    };
    let normalized_model_id = model_id.trim();
    if normalized_model_id.is_empty() {
        return Err("model id cannot be empty".to_string());
    }
    if target_workspace.location_kind == "ssh"
        && !matches!(engine_id.as_str(), "codex" | "opencode" | "claude")
    {
        return Err(format!("SSH 远端项目当前阶段尚未接入 {engine_id} 正式对话"));
    }
    let validation_models = if target_workspace.location_kind == "ssh" {
        Some(match engine_id.as_str() {
            "codex" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let codex = CliToolFactory::new(state.inner().clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let cli: &dyn CliTool = codex.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "opencode" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let opencode = CliToolFactory::new(state.inner().clone())
                    .create("opencode")
                    .expect("OpenCode CLI factory mapping must exist");
                let cli: &dyn CliTool = opencode.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "claude" => {
                let context = CliExecutionContext::from_workspace(&target_workspace)
                    .map_err(err_to_string)?;
                let claude = CliToolFactory::new(state.inner().clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let cli: &dyn CliTool = claude.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            _ => unreachable!(),
        })
    } else if engine_id == "claude" {
        let context =
            CliExecutionContext::from_workspace(&target_workspace).map_err(err_to_string)?;
        let claude = CliToolFactory::new(state.inner().clone())
            .create("claude")
            .expect("Claude CLI factory mapping must exist");
        let cli: &dyn CliTool = claude.as_ref();
        cli.models_for_validation(&context, normalized_model_id)
            .await
            .ok()
    } else {
        state
            .engines
            .models_for_validation(&engine_id, normalized_model_id)
            .await
            .ok()
    };
    let effective_model_id = validate_model_for_engine_from_catalog(
        &engine_id,
        normalized_model_id,
        validation_models.as_deref(),
    )?;
    let normalized_reasoning_effort = reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let validated_reasoning_effort =
        if let Some(requested_effort) = normalized_reasoning_effort.as_deref() {
            Some(validate_reasoning_effort_from_catalog(
                &effective_model_id,
                requested_effort,
                validation_models.as_deref(),
            )?)
        } else {
            None
        };

    let default_autonomy_preset = tokio::task::spawn_blocking(|| {
        AppConfig::load_or_create()
            .map(|config| config.default_autonomy_preset().map(ToOwned::to_owned))
            .map_err(err_to_string)
    })
    .await
    .map_err(err_to_string)??;
    let mut metadata = serde_json::Map::new();
    if let Some(value) = normalized_service_tier {
        metadata.insert("serviceTier".to_string(), json!(value));
    }
    // 初始权限预设由 CLI 权限接口处理，不再写入 engine_metadata_json 镜像。
    let _ = default_autonomy_preset;

    if let Some(existing_metadata) = thread.engine_metadata.as_ref().and_then(Value::as_object) {
        for key in ["manualTitle", "manualTitleUpdatedAt"] {
            if let Some(value) = existing_metadata.get(key) {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    let metadata = (!metadata.is_empty()).then_some(Value::Object(metadata));

    let reconfigure_engine_id = engine_id.clone();
    let reconfigure_model_id = effective_model_id.clone();
    let first_engine_id = reconfigure_engine_id.clone();
    let first_model_id = reconfigure_model_id.clone();
    let updated = run_db(db.clone(), move |db| {
        db::threads::reconfigure_unstarted_thread_runtime(
            db,
            &thread_id,
            &first_engine_id,
            &first_model_id,
            metadata.as_ref(),
        )
    })
    .await?;
    run_db(db, move |db| {
        db::threads::update_thread_runtime_selection(
            db,
            &updated.id,
            &reconfigure_engine_id,
            &reconfigure_model_id,
            None,
            None,
            validated_reasoning_effort.as_deref(),
            None,
        )
    })
    .await
    /*
    // 误插入到 reconfigure_unstarted_thread_runtime 的初始预设处理，已停用；
    // 初始预设处理属于 create_thread_inner。
    if let Some(preset) = initial_autonomy_preset {
        let cli = CliToolFactory::new(state.clone())
            .create(created.engine_id.as_str())
            .map_err(err_to_string)?;
        let context = CliExecutionContext::from_workspace(&target_workspace)
            .map_err(err_to_string)?;
        let mut permissions = cli
            .get_permissions(&context, &created)
            .await
            .map_err(err_to_string)?;
        permissions.insert("autonomyPreset".to_string(), json!([preset]));
        let _saved_permissions = cli
            .set_permissions(&context, &created, permissions)
            .await
            .map_err(err_to_string)?;
        let created_id = created.id.clone();
        created = run_db(state.db.clone(), {
            let thread_id = created_id.clone();
            move |db| db::threads::get_thread(db, &thread_id)
        })
        .await?
        .ok_or_else(|| format!("thread not found after initial autonomy preset update: {created_id}"))?;
    }
    */
}

#[tauri::command]
pub async fn set_thread_reasoning_effort(
    state: State<'_, AppState>,
    thread_id: String,
    reasoning_effort: Option<String>,
    model_id: Option<String>,
) -> Result<(), String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let normalized_model_id = model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let effective_model_id = match normalized_model_id {
        Some(model_id) => {
            validate_model_for_thread_engine(state.inner(), &thread, model_id).await?
        }
        None => thread.model_id.clone(),
    };

    let normalized_effort = reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);

    let validated_effort = if let Some(value) = normalized_effort.as_deref() {
        if workspace.location_kind == "ssh" {
            let models = match thread.engine_id.as_str() {
                "codex" => {
                    let context =
                        CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                    let codex = CliToolFactory::new(state.inner().clone())
                        .create("codex")
                        .expect("Codex CLI factory mapping must exist");
                    let cli: &dyn CliTool = codex.as_ref();
                    cli.models_for_validation(&context, &effective_model_id)
                        .await
                        .map_err(err_to_string)?
                }
                "opencode" => {
                    let context =
                        CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                    let opencode = CliToolFactory::new(state.inner().clone())
                        .create("opencode")
                        .expect("OpenCode CLI factory mapping must exist");
                    let cli: &dyn CliTool = opencode.as_ref();
                    cli.models_for_validation(&context, &effective_model_id)
                        .await
                        .map_err(err_to_string)?
                }
                "claude" => {
                    let context =
                        CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                    let claude = CliToolFactory::new(state.inner().clone())
                        .create("claude")
                        .expect("Claude CLI factory mapping must exist");
                    let cli: &dyn CliTool = claude.as_ref();
                    cli.models_for_validation(&context, &effective_model_id)
                        .await
                        .map_err(err_to_string)?
                }
                _ => {
                    return Err(format!(
                        "SSH 远端项目当前阶段尚未接入 {} 思考强度",
                        thread.engine_id
                    ));
                }
            };
            Some(validate_reasoning_effort_from_catalog(
                effective_model_id.as_str(),
                value,
                Some(models.as_slice()),
            )?)
        } else {
            Some(
                validate_reasoning_effort(
                    state.inner(),
                    &thread.engine_id,
                    effective_model_id.as_str(),
                    value,
                )
                .await?,
            )
        }
    } else {
        None
    };

    run_db(db, move |db| {
        db::threads::update_thread(
            db,
            &ThreadUpdateDto {
                id: thread_id,
                reasoning_effort: Some(validated_effort.unwrap_or_default()),
                ..Default::default()
            },
        )
        .map(|_| ())
    })
    .await
}

#[tauri::command]
pub async fn rename_thread(
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
) -> Result<ThreadDto, String> {
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
            json!(Utc::now().to_rfc3339()),
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

#[tauri::command]
pub async fn delete_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    state.turns.cancel(&thread_id).await;

    let db = state.db.clone();
    if let Some(thread) = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    {
        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await?;
        if thread.engine_id == "codex" {
            let result = async {
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let codex = CliToolFactory::new(state.inner().clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let cli: &dyn CliTool = codex.as_ref();
                cli.interrupt(
                    &context,
                    &thread,
                    thread.engine_thread_id.as_deref().unwrap_or("default"),
                )
                .await
                .map_err(err_to_string)
            }
            .await;
            if let Err(error) = result {
                log::warn!("failed to interrupt Codex thread before deletion: {error}");
            }
        } else if thread.engine_id == "claude" {
            let result = async {
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let claude = CliToolFactory::new(state.inner().clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let cli: &dyn CliTool = claude.as_ref();
                cli.interrupt(
                    &context,
                    &thread,
                    thread.engine_thread_id.as_deref().unwrap_or("default"),
                )
                .await
                .map_err(err_to_string)
            }
            .await;
            if let Err(error) = result {
                log::warn!("failed to interrupt Claude thread before deletion: {error}");
            }
        } else if thread.engine_id == "opencode" {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let opencode = CliToolFactory::new(state.inner().clone())
                .create("opencode")
                .expect("OpenCode CLI factory mapping must exist");
            let cli: &dyn CliTool = opencode.as_ref();
            let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or("default");
            if let Err(error) = cli.interrupt(&context, &thread, engine_thread_id).await {
                log::warn!("failed to interrupt OpenCode thread before deletion: {error:#}");
            }
            if let Some(engine_thread_id) = thread.engine_thread_id.as_deref() {
                cli.forget_session(&context, &thread, engine_thread_id)
                    .await
                    .map_err(err_to_string)?;
            }
        } else if let Err(error) = state.engines.interrupt(&thread).await {
            log::warn!("failed to interrupt thread before deletion: {error}");
        }
    } else {
        state.turns.finish(&thread_id).await;
        return Err(format!("thread not found: {thread_id}"));
    }

    run_db(db, {
        let thread_id = thread_id.clone();
        move |db| db::threads::delete_thread(db, &thread_id)
    })
    .await?;
    state.turns.finish(&thread_id).await;
    Ok(())
}

/// 将 Codex 会话归档的本地会话、引擎会话和执行位置写入既有传输日志，便于开发排查。
///
/// `error` 仅在引擎归档失败时写入，内容保留完整 anyhow 错误链；日志失败不改变归档主流程。
async fn append_codex_thread_archive_log(
    event: &str,
    thread: &ThreadDto,
    context: &CliExecutionContext,
    engine_thread_id: &str,
    error: Option<&str>,
) {
    // 归档日志只记录稳定的小写位置类型，避免 Debug 输出成为后续检索条件。
    let location_kind = match context.location_kind {
        crate::cli_tools::CliLocationKind::Local => "local",
        crate::cli_tools::CliLocationKind::Ssh => "ssh",
    };
    let mut record = json!({
        // 日志产生时间。
        "at": Utc::now().to_rfc3339(),
        // Codex 归档生命周期事件名。
        "event": event,
        // AuraCoder 本地会话标识。
        "auracoder_thread_id": thread.id,
        // Codex 引擎会话标识。
        "engine_thread_id": engine_thread_id,
        // 会话所属 workspace 标识。
        "workspace_id": context.workspace_id,
        // CLI 实际操作的项目根目录。
        "workspace_root_path": context.root_path,
        // 项目执行位置：本机或 SSH 远端。
        "location_kind": location_kind,
        // SSH 连接标识；本机项目序列化为 null。
        "ssh_connection_id": context.ssh_connection_id,
    });
    if let Some(error) = error {
        // 引擎失败时保留完整 anyhow 错误链，便于关联既有 rpc_error。
        record["error"] = Value::String(error.to_owned());
    }
    crate::engines::codex::append_codex_transport_log(&record).await;
}

#[tauri::command]
pub async fn archive_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    state.turns.cancel(&thread_id).await;

    let db = state.db.clone();
    let result = async {
        let thread = run_db(db.clone(), {
            let thread_id = thread_id.clone();
            move |db| db::threads::get_thread(db, &thread_id)
        })
        .await?
        .ok_or_else(|| format!("thread not found: {thread_id}"))?;

        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await?;
        if thread.engine_id == "codex" {
            let context =
                CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let codex = CliToolFactory::new(state.inner().clone()).create("codex").expect("Codex CLI factory mapping must exist");
            let cli: &dyn CliTool = codex.as_ref();
            let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or("default");
            if let Err(error) = cli
                .interrupt(&context, &thread, engine_thread_id)
                .await
            {
                log::warn!("failed to interrupt Codex thread before archive: {error:#}");
            }
            append_codex_thread_archive_log(
                "codex_thread_archive_started",
                &thread,
                &context,
                engine_thread_id,
                None,
            )
            .await;
            if thread.engine_thread_id.is_some() {
                match cli
                    .archive_thread(&context, &thread, engine_thread_id)
                    .await
                {
                    Ok(()) => {
                        append_codex_thread_archive_log(
                            "codex_thread_archive_engine_completed",
                            &thread,
                            &context,
                            engine_thread_id,
                            None,
                        )
                        .await;
                    }
                    Err(error) => {
                        let error_text = format!("{error:#}");
                        append_codex_thread_archive_log(
                            "codex_thread_archive_engine_failed",
                            &thread,
                            &context,
                            engine_thread_id,
                            Some(&error_text),
                        )
                        .await;
                        return Err(err_to_string(error));
                    }
                }
            }
            run_db(db.clone(), {
                let thread_id = thread_id.clone();
                move |db| db::threads::archive_thread(db, &thread_id)
            })
            .await?;
            append_codex_thread_archive_log(
                "codex_thread_archive_local_completed",
                &thread,
                &context,
                engine_thread_id,
                None,
            )
            .await;
            return Ok(());
        }
        if thread.engine_id == "claude" {
            let context =
                CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let claude = CliToolFactory::new(state.inner().clone()).create("claude").expect("Claude CLI factory mapping must exist");
            let cli: &dyn CliTool = claude.as_ref();
            let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or("default");
            if let Err(error) = cli
                .interrupt(&context, &thread, engine_thread_id)
                .await
            {
                log::warn!("failed to interrupt Claude thread before archive: {error:#}");
            }
            if thread.engine_thread_id.is_some() {
                // 归档必须经过统一 CliTool 接口；SSH Claude 的具体实现不访问远端归档协议，
                // 成功返回后才由下面的公共数据库流程写入本地 archived_at。
                cli.archive_thread(&context, &thread, engine_thread_id)
                    .await
                    .map_err(err_to_string)?;
            }
            run_db(db.clone(), {
                let thread_id = thread_id.clone();
                move |db| db::threads::archive_thread(db, &thread_id)
            })
            .await?;
            return Ok(());
        }
        if thread.engine_id == "opencode" {
            let context =
                CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let opencode = CliToolFactory::new(state.inner().clone()).create("opencode").expect("OpenCode CLI factory mapping must exist");
            let cli: &dyn CliTool = opencode.as_ref();
            let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or("default");
            if let Err(error) = cli
                .interrupt(&context, &thread, engine_thread_id)
                .await
            {
                log::warn!("failed to interrupt OpenCode thread before archive: {error:#}");
            }
            if thread.engine_thread_id.is_some() {
                cli.archive_thread(&context, &thread, engine_thread_id)
                    .await
                    .map_err(err_to_string)?;
            }
            run_db(db.clone(), {
                let thread_id = thread_id.clone();
                move |db| db::threads::archive_thread(db, &thread_id)
            })
            .await?;
            return Ok(());
        }

        if let Err(error) = state.engines.interrupt(&thread).await {
            log::warn!("failed to interrupt thread before archive: {error}");
        }

        if thread.engine_id == "opencode" {
            unreachable!("OpenCode archive already uses OpenCodeCli");
        } else {
            match state.engines.archive_thread(&thread).await {
                Ok(()) => {}
                Err(error) if thread.engine_id == "codex" => {
                    let engine_thread_id = thread.engine_thread_id.as_deref().unwrap_or_default();
                    match codex_remote_thread_archive_state(state.inner(), engine_thread_id).await {
                        Ok(CodexRemoteThreadArchiveState::Archived) => {
                            log::info!(
                                "codex thread {} is already archived remotely; archiving its local record",
                                engine_thread_id
                            );
                        }
                        Ok(CodexRemoteThreadArchiveState::Missing) => {
                            log::info!(
                                "codex thread {} is already absent remotely; archiving its local record",
                                engine_thread_id
                            );
                        }
                        Ok(CodexRemoteThreadArchiveState::Active) => Err(err_to_string(error))?,
                        Err(state_error) => {
                            log::warn!(
                                "failed to resolve codex thread {} state after its archive request failed: {state_error}",
                                engine_thread_id
                            );
                            Err(err_to_string(error))?;
                        }
                    }
                }
                Err(error) => Err(err_to_string(error))?,
            }
        }

        run_db(db, {
            let thread_id = thread_id.clone();
            move |db| db::threads::archive_thread(db, &thread_id)
        })
        .await?;

        Ok(())
    }
    .await;

    state.turns.finish(&thread_id).await;
    result
}

#[tauri::command]
pub async fn archive_thread_locally(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    archive_thread_locally_inner(state.inner(), &thread_id).await
}

async fn archive_thread_locally_inner(state: &AppState, thread_id: &str) -> Result<(), String> {
    state.turns.cancel(thread_id).await;
    let db = state.db.clone();
    let thread_id = thread_id.to_string();
    let db_thread_id = thread_id.clone();
    let result = run_db(db, move |db| db::threads::archive_thread(db, &db_thread_id)).await;
    state.turns.finish(&thread_id).await;
    result
}

#[tauri::command]
pub async fn restore_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;

    // 统一恢复编排：先确认 CLI 会话仍存在，再执行 CLI 解归档，最后只更新 AuraCoder 数据库。
    // Claude SSH 的 unarchive_thread 只做本地业务确认，不向远端发送归档或恢复请求。
    if matches!(thread.engine_id.as_str(), "codex" | "opencode" | "claude") {
        if let Some(engine_thread_id) = thread.engine_thread_id.as_deref() {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli = CliToolFactory::new(state.inner().clone())
                .create(&thread.engine_id)
                .map_err(err_to_string)?;
            if let Err(error) = cli.read_session(&context, engine_thread_id).await {
                if error.downcast_ref::<CliSessionNotFoundError>().is_some() {
                    return Err(
                        if context.location_kind == crate::cli_tools::CliLocationKind::Ssh {
                            "该会话在远端已不存在，无法恢复。".to_string()
                        } else {
                            "该会话在本机 CLI 中已不存在，无法恢复。".to_string()
                        },
                    );
                }
                return Err(format!("恢复前读取 CLI 会话失败: {}", err_to_string(error)));
            }
            cli.unarchive_thread(&context, &thread, engine_thread_id)
                .await
                .map_err(err_to_string)?;
        }

        return run_db(db, move |db| db::threads::restore_thread(db, &thread_id)).await;
    }

    /*
    // 旧实现按具体 CLI 分支直接执行 unarchive_thread，未先确认会话存在。
    // 完整保留在此处作为迁移留痕；统一流程已在上方接管恢复顺序。
    if thread.engine_id == "codex" {
        if let Some(engine_thread_id) = thread.engine_thread_id.as_deref() {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let codex = CliToolFactory::new(state.inner().clone())
                .create("codex")
                .expect("Codex CLI factory mapping must exist");
            let cli: &dyn CliTool = codex.as_ref();
            cli.unarchive_thread(&context, &thread, engine_thread_id)
                .await
                .map_err(err_to_string)?;
        }
        return run_db(db, move |db| db::threads::restore_thread(db, &thread_id)).await;
    }
    if thread.engine_id == "claude" {
        if let Some(engine_thread_id) = thread.engine_thread_id.as_deref() {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let claude = CliToolFactory::new(state.inner().clone())
                .create("claude")
                .expect("Claude CLI factory mapping must exist");
            let cli: &dyn CliTool = claude.as_ref();
            cli.unarchive_thread(&context, &thread, engine_thread_id)
                .await
                .map_err(err_to_string)?;
        }
        return run_db(db, move |db| db::threads::restore_thread(db, &thread_id)).await;
    }
    if thread.engine_id == "opencode" {
        if let Some(engine_thread_id) = thread.engine_thread_id.as_deref() {
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let opencode = CliToolFactory::new(state.inner().clone())
                .create("opencode")
                .expect("OpenCode CLI factory mapping must exist");
            let cli: &dyn CliTool = opencode.as_ref();
            cli.unarchive_thread(&context, &thread, engine_thread_id)
                .await
                .map_err(err_to_string)?;
        }
        return run_db(db, move |db| db::threads::restore_thread(db, &thread_id)).await;
    }
    */
    state
        .engines
        .unarchive_thread(&thread)
        .await
        .map_err(err_to_string)?;

    let restored = run_db(db, move |db| db::threads::restore_thread(db, &thread_id)).await?;

    Ok(restored)
}

/// 对消息为空的 OpenCode 或 Claude Code 线程导入完整原生历史，并刷新本地统计。
async fn sync_empty_cli_thread_from_engine(
    state: &AppState,
    thread_id: &str,
    thread: &ThreadDto,
) -> Result<ThreadDto, String> {
    let message_count = run_db(state.db.clone(), {
        let thread_id = thread_id.to_string();
        move |db| db::messages::count_thread_messages(db, &thread_id)
    })
    .await?;
    let mut summary_synced_thread = thread.clone();
    if thread.engine_id == "opencode" {
        if let Some(engine_thread_id) = thread
            .engine_thread_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let workspace = run_db(state.db.clone(), {
                let workspace_id = thread.workspace_id.clone();
                move |db| {
                    db::workspaces::find_workspace_by_id(db, &workspace_id)?
                        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
                }
            })
            .await?;
            let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
            let cli_tool = CliToolFactory::new(state.clone())
                .create("opencode")
                .map_err(|error| format!("unsupported CLI engine opencode: {error}"))?;
            let cli: &dyn CliTool = cli_tool.as_ref();
            let snapshot = match cli.read_session(&context, engine_thread_id).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log::debug!("failed to sync OpenCode session {engine_thread_id}: {error}");
                    return Ok(thread.clone());
                }
            };
            let parse_timestamp = |value: Option<String>| {
                value
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .unwrap_or_default()
            };
            let session = OpenCodeRemoteSessionSummary {
                engine_thread_id: snapshot.engine_thread_id,
                title: Some(snapshot.title),
                cwd: snapshot.cwd,
                created_at: parse_timestamp(snapshot.created_at),
                updated_at: parse_timestamp(snapshot.updated_at),
                archived: snapshot.archived,
            };
            let title = build_opencode_remote_session_title(&session);
            let metadata = build_opencode_remote_session_metadata(
                thread.engine_metadata.as_ref(),
                &session,
                &thread.model_id,
            );
            let thread_id = thread_id.to_string();
            summary_synced_thread = run_db(state.db.clone(), move |db| {
                db::threads::update_thread_runtime_snapshot(
                    db,
                    &thread_id,
                    Some(&title),
                    Some(ThreadStatusDto::Idle),
                    Some(&metadata),
                )
            })
            .await?;
            if message_count != 0 {
                return Ok(summary_synced_thread);
            }
        }
    }
    if message_count != 0 {
        return Ok(thread.clone());
    }

    let Some(engine_thread_id) = thread
        .engine_thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(thread.clone());
    };

    let workspace = run_db(state.db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let cli_tool = CliToolFactory::new(state.clone())
        .create(&thread.engine_id)
        .map_err(|error| format!("unsupported CLI engine {}: {error}", thread.engine_id))?;
    let cli: &dyn CliTool = cli_tool.as_ref();
    let snapshot = match cli
        .read_thread_sync_snapshot(&context, thread, engine_thread_id)
        .await
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(summary_synced_thread),
        Err(error) => {
            // 旧完整历史入口会把原始读取错误返回给点击层和 MCP；两者再回退到本地结果。
            return Err(err_to_string(error));
            // 旧实现曾在此处记录 debug 后返回本地线程，保留迁移留痕但不再吞掉原生错误：
            // log::debug!("failed to sync {} thread {}: {error}", thread.engine_id, thread_id);
            // return Ok(thread.clone());
        }
    };

    let imported_messages = snapshot
        .imported_messages
        .iter()
        .map(|message| db::messages::ImportedMessageRecord {
            role: message.role.clone(),
            content: message.content.clone(),
            blocks: message.blocks.clone(),
            status: MessageStatusDto::from_str(message.status.as_str()),
            turn_engine_id: message.turn_engine_id.clone(),
            remote_turn_id: message.turn_engine_id.clone(),
            turn_model_id: message.turn_model_id.clone(),
            turn_reasoning_effort: message.turn_reasoning_effort.clone(),
            token_input: message.token_input,
            token_output: message.token_output,
            created_at: message.created_at.clone(),
        })
        .collect::<Vec<_>>();
    run_db(state.db.clone(), {
        let thread_id = thread_id.to_string();
        move |db| {
            db::messages::append_thread_messages(db, &thread_id, &imported_messages)?;
            db::threads::refresh_thread_message_stats(
                db,
                &thread_id,
                db::threads::ThreadMessageStatsActivityPolicy::PreserveExisting,
            )?;
            Ok::<_, anyhow::Error>(())
        }
    })
    .await?;

    let title = snapshot
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    run_db(state.db.clone(), {
        let thread_id = thread_id.to_string();
        let title = title.map(str::to_string);
        move |db| {
            db::threads::update_thread_runtime_snapshot(
                db,
                &thread_id,
                title.as_deref(),
                None,
                None,
            )
        }
    })
    .await
}

/// 通过 Tauri 命令同步指定引擎线程的最新会话内容。
#[tauri::command]
pub async fn sync_thread_from_engine(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    sync_thread_from_engine_inner(state.inner(), thread_id).await
}

/// 按目标线程所属引擎同步最新会话内容，供 Tauri 命令和 MCP 按需复用。
pub(crate) async fn sync_thread_from_engine_inner(
    state: &AppState,
    thread_id: String,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if matches!(thread.engine_id.as_str(), "opencode" | "claude") {
        return sync_empty_cli_thread_from_engine(state, &thread_id, &thread).await;
    }

    // 旧 OpenCode 摘要同步分支保留为迁移留痕；完整历史同步已由上方统一业务入口接替。
    /*
    if thread.engine_id == "opencode" {
        let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
            return Ok(thread);
        };
        let workspace = run_db(db.clone(), {
            let workspace_id = thread.workspace_id.clone();
            move |db| {
                db::workspaces::find_workspace_by_id(db, &workspace_id)?
                    .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
            }
        })
        .await?;
        let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
        let opencode = CliToolFactory::new(state.clone())
            .create("opencode")
            .expect("OpenCode CLI factory mapping must exist");
        let cli: &dyn CliTool = opencode.as_ref();
        let snapshot = match cli.read_session(&context, engine_thread_id).await {
            Ok(session) => session,
            Err(error) => {
                log::debug!("failed to sync OpenCode session {engine_thread_id}: {error}");
                return Ok(thread);
            }
        };
        let parse_timestamp = |value: Option<String>| {
            value
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp_millis())
                .unwrap_or_default()
        };
        let session = OpenCodeRemoteSessionSummary {
            engine_thread_id: snapshot.engine_thread_id,
            title: Some(snapshot.title),
            cwd: snapshot.cwd,
            created_at: parse_timestamp(snapshot.created_at),
            updated_at: parse_timestamp(snapshot.updated_at),
            archived: snapshot.archived,
        };
        let title = build_opencode_remote_session_title(&session);
        let metadata = build_opencode_remote_session_metadata(
            thread.engine_metadata.as_ref(),
            &session,
            &thread.model_id,
        );
        return run_db(db, move |db| {
            db::threads::update_thread_runtime_snapshot(
                db,
                &thread_id,
                Some(&title),
                Some(ThreadStatusDto::Idle),
                Some(&metadata),
            )
        })
        .await;
    }
    */

    if thread.engine_id != "codex" {
        return Ok(thread);
    }

    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let Some(engine_thread_id) = thread.engine_thread_id.as_deref() else {
        return Ok(thread);
    };
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    let Some(snapshot) = cli
        .read_thread_sync_snapshot(&context, &thread, engine_thread_id)
        .await
        .map_err(err_to_string)?
    else {
        return Ok(thread);
    };

    let has_local_turn = state.turns.get(&thread_id).await.is_some();
    let has_active_remote_turn =
        !snapshot.active_flags.is_empty() || imported_messages_have_streaming_turn(&snapshot);
    let imported_messages = snapshot
        .imported_messages
        .iter()
        .map(|message| db::messages::ImportedMessageRecord {
            role: message.role.clone(),
            content: message.content.clone(),
            blocks: message.blocks.clone(),
            status: MessageStatusDto::from_str(message.status.as_str()),
            turn_engine_id: message.turn_engine_id.clone(),
            remote_turn_id: message.turn_engine_id.clone(),
            turn_model_id: message.turn_model_id.clone(),
            turn_reasoning_effort: message.turn_reasoning_effort.clone(),
            token_input: message.token_input,
            token_output: message.token_output,
            created_at: message.created_at.clone(),
        })
        .collect::<Vec<_>>();
    let imported_any_messages = !imported_messages.is_empty();
    run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| {
            db::messages::append_thread_messages(db, &thread_id, &imported_messages)?;
            db::threads::refresh_thread_message_stats(
                db,
                &thread_id,
                db::threads::ThreadMessageStatsActivityPolicy::PreserveExisting,
            )?;
            Ok::<_, anyhow::Error>(())
        }
    })
    .await?;

    let sync_required = !has_local_turn && has_active_remote_turn;
    let metadata = merge_codex_runtime_metadata(
        thread.engine_metadata.clone(),
        snapshot.raw_status.as_deref(),
        &snapshot.active_flags,
        snapshot.preview.as_deref(),
        sync_required,
        sync_required.then_some("remote thread has an active turn"),
    );
    let metadata = mark_codex_transcript_imported(metadata, imported_any_messages);
    let next_status = map_codex_thread_status_to_local(
        snapshot.raw_status.as_deref(),
        &snapshot.active_flags,
        has_local_turn,
    );

    run_db(db, {
        let thread_id = thread_id.clone();
        let title = snapshot.title.clone();
        let metadata = metadata.clone();
        let next_status = next_status.clone();
        move |db| {
            // 旧调用把 next_status 传入 update_thread_runtime_snapshot，会经 update_thread_status 触碰活动时间；点击同步改为下方 status-only 局部更新。
            let updated = db::threads::update_thread_runtime_snapshot(
                db,
                &thread_id,
                title.as_deref(),
                None,
                Some(&metadata),
            )?;
            let Some(next_status) = next_status else {
                return Ok(updated);
            };
            if updated.status == next_status {
                return Ok(updated);
            }
            db::threads::update_thread(
                db,
                &ThreadUpdateDto {
                    // 已完成标题和 metadata 同步后的线程标识。
                    id: updated.id,
                    // 只更新状态，避免状态同步路径改写活动时间。
                    status: Some(next_status.as_str().to_owned()),
                    ..Default::default()
                },
            )
        }
    })
    .await
}

fn mark_codex_transcript_imported(mut metadata: Value, imported: bool) -> Value {
    if imported {
        if let Some(object) = metadata.as_object_mut() {
            object.insert("codexTranscriptImported".to_string(), json!(true));
        }
    }

    metadata
}

fn imported_messages_have_streaming_turn(snapshot: &ThreadSyncSnapshot) -> bool {
    snapshot
        .imported_messages
        .iter()
        .any(|message| message.status == "streaming")
}

async fn reject_ssh_remote_codex_local_only_action(
    state: &AppState,
    thread: &ThreadDto,
    action: &str,
) -> Result<(), String> {
    let db = state.db.clone();
    let workspace_id = thread.workspace_id.clone();
    let workspace = run_db(db, move |db| {
        db::workspaces::find_workspace_by_id(db, &workspace_id)
    })
    .await?;
    if workspace.is_some_and(|workspace| workspace.location_kind == "ssh") {
        return Err(format!(
            "SSH 远端 Codex 暂未接入{action}，当前不会调用本机 Codex 执行"
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn fork_codex_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    if state.turns.get(&thread_id).await.is_some() {
        return Err("cannot fork a thread while a turn is still active".to_string());
    }

    let db = state.db.clone();
    let mut thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "codex" {
        return Err("native fork is only available for Codex threads".to_string());
    }
    reject_ssh_remote_codex_local_only_action(state.inner(), &thread, "会话分支").await?;
    migrate_legacy_codex_on_failure_thread_metadata(state.inner(), &mut thread).await;
    let engine_thread_id = thread
        .engine_thread_id
        .clone()
        .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;
    let (cwd, model_id, sandbox) = build_codex_branch_context(state.inner(), &thread).await?;
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(Some(thread.workspace_id.as_str()))
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    let forked = cli
        .fork_thread(&context, &engine_thread_id, &cwd, &model_id, sandbox)
        .await
        .map_err(err_to_string)?;

    create_codex_branch_thread(
        state.inner(),
        &thread,
        &forked.engine_thread_id,
        &forked.model_id,
        forked.title.as_deref(),
        forked.preview.as_deref(),
        forked.raw_status.as_deref(),
        &forked.active_flags,
        None,
    )
    .await
}

#[tauri::command]
pub async fn rollback_codex_thread(
    state: State<'_, AppState>,
    thread_id: String,
    num_turns: u32,
) -> Result<ThreadDto, String> {
    if num_turns == 0 {
        return Err("rollback requires at least one turn".to_string());
    }
    if state.turns.get(&thread_id).await.is_some() {
        return Err("cannot rollback a thread while a turn is still active".to_string());
    }

    let db = state.db.clone();
    let mut thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "codex" {
        return Err("native rollback is only available for Codex threads".to_string());
    }
    reject_ssh_remote_codex_local_only_action(state.inner(), &thread, "回滚").await?;
    migrate_legacy_codex_on_failure_thread_metadata(state.inner(), &mut thread).await;
    let engine_thread_id = thread
        .engine_thread_id
        .clone()
        .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;
    let (cwd, model_id, sandbox) = build_codex_branch_context(state.inner(), &thread).await?;
    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(Some(thread.workspace_id.as_str()))
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    let forked = cli
        .fork_thread(&context, &engine_thread_id, &cwd, &model_id, sandbox)
        .await
        .map_err(err_to_string)?;
    let rollback_snapshot = match cli
        .rollback_thread(&context, &forked.engine_thread_id, num_turns)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(rollback_error) => {
            let mut cleanup_thread = thread.clone();
            cleanup_thread.engine_thread_id = Some(forked.engine_thread_id.clone());
            if let Err(cleanup_error) = cli
                .archive_thread(&context, &cleanup_thread, &forked.engine_thread_id)
                .await
            {
                log::warn!(
                    "failed to clean up forked engine thread {} after rollback failure: {cleanup_error}",
                    forked.engine_thread_id
                );
            }
            return Err(err_to_string(rollback_error));
        }
    };

    create_codex_branch_thread(
        state.inner(),
        &thread,
        &forked.engine_thread_id,
        &forked.model_id,
        rollback_snapshot
            .title
            .as_deref()
            .or(forked.title.as_deref()),
        rollback_snapshot
            .preview
            .as_deref()
            .or(forked.preview.as_deref()),
        rollback_snapshot
            .raw_status
            .as_deref()
            .or(forked.raw_status.as_deref()),
        &rollback_snapshot.active_flags,
        Some(num_turns),
    )
    .await
}

#[tauri::command]
pub async fn compact_codex_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDto, String> {
    if state.turns.get(&thread_id).await.is_some() {
        return Err("cannot compact a thread while a turn is still active".to_string());
    }

    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "codex" {
        return Err("native compact is only available for Codex threads".to_string());
    }
    reject_ssh_remote_codex_local_only_action(state.inner(), &thread, "压缩").await?;
    let engine_thread_id = thread
        .engine_thread_id
        .clone()
        .ok_or_else(|| "Codex thread has not been initialized yet".to_string())?;

    let codex = CliToolFactory::new(state.inner().clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let context = codex
        .execution_context(Some(thread.workspace_id.as_str()))
        .await
        .map_err(err_to_string)?;
    let cli: &dyn CliTool = codex.as_ref();
    cli.compact_thread(&context, &engine_thread_id)
        .await
        .map_err(err_to_string)?;

    Ok(thread)
}

#[tauri::command]
pub async fn set_ssh_remote_thread_selected_model(
    state: State<'_, AppState>,
    thread_id: String,
    model_id: String,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| {
            db::threads::get_thread(db, &thread_id)?
                .ok_or_else(|| anyhow::anyhow!("thread not found: {thread_id}"))
        }
    })
    .await?;
    let workspace_id = thread.workspace_id.clone();
    let workspace = run_db(db.clone(), move |db| {
        db::workspaces::find_workspace_by_id(db, &workspace_id)?
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
    })
    .await?;
    if workspace.location_kind != "ssh" {
        return Err("only SSH remote project threads can persist this model selection".to_string());
    }
    if !matches!(thread.engine_id.as_str(), "codex" | "opencode" | "claude") {
        return Err(format!(
            "SSH 远端项目当前阶段尚未接入 {} 模型选择持久化",
            thread.engine_id
        ));
    }

    let selected_model_id =
        validate_model_for_thread_engine(state.inner(), &thread, model_id.trim()).await?;

    run_db(db, move |db| {
        db::threads::update_thread(
            db,
            &ThreadUpdateDto {
                id: thread_id.clone(),
                model_id: Some(selected_model_id.clone()),
                ..Default::default()
            },
        )?;
        db::threads::get_thread(db, &thread_id)?
            .ok_or_else(|| anyhow::anyhow!("thread not found after model selection: {thread_id}"))
    })
    .await
}

#[tauri::command]
pub async fn set_thread_execution_policy(
    state: State<'_, AppState>,
    thread_id: String,
    update_approval_policy: bool,
    approval_policy: Option<Value>,
    update_sandbox_mode: bool,
    sandbox_mode: Option<String>,
    update_allow_network: bool,
    allow_network: Option<bool>,
    update_permission_profile: bool,
    permission_profile: Option<Value>,
    update_approvals_reviewer: bool,
    approvals_reviewer: Option<String>,
) -> Result<ThreadDto, String> {
    set_thread_execution_policy_inner(
        state.inner(),
        thread_id,
        update_approval_policy,
        approval_policy,
        update_sandbox_mode,
        sandbox_mode,
        update_allow_network,
        allow_network,
        update_permission_profile,
        permission_profile,
        update_approvals_reviewer,
        approvals_reviewer,
    )
    .await
}

async fn set_thread_execution_policy_inner(
    state: &AppState,
    thread_id: String,
    update_approval_policy: bool,
    approval_policy: Option<Value>,
    update_sandbox_mode: bool,
    sandbox_mode: Option<String>,
    update_allow_network: bool,
    allow_network: Option<bool>,
    update_permission_profile: bool,
    permission_profile: Option<Value>,
    update_approvals_reviewer: bool,
    approvals_reviewer: Option<String>,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    /*
    // 旧 Controller 适配逻辑已停用：Controller 曾按 engine_id 识别并校验
    // Codex、Claude Code、OpenCode 的 approvalPolicy、sandboxMode、permissionProfile
    // 和 approvalsReviewer。原始字段和值现在由 CliTool 实现层负责转换。
    let normalized_approval_policy = if update_approval_policy {
        normalize_thread_approval_policy_for_engine(thread.engine_id.as_str(), approval_policy)?
    } else { None };
    let normalized_sandbox_mode = if update_sandbox_mode {
        let normalized = normalize_thread_sandbox_mode(sandbox_mode)?;
        validate_engine_sandbox_mode(thread.engine_id.as_str(), normalized.as_deref())?;
        normalized
    } else { None };
    let normalized_permission_profile = if update_permission_profile {
        normalize_thread_permission_profile(permission_profile)?
    } else { None };
    let normalized_approvals_reviewer = if update_approvals_reviewer {
        normalize_thread_approvals_reviewer(approvals_reviewer)?
    } else { None };
    */
    let workspace = run_db(db.clone(), {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let original_permission_mode = thread.permission_mode.clone();
    let cli = CliToolFactory::new(state.clone())
        .create(thread.engine_id.as_str())
        .map_err(err_to_string)?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let patch = crate::cli_tools::CliRuntimePermissionPatch {
        approval_policy: update_approval_policy.then_some(approval_policy),
        sandbox_mode: update_sandbox_mode.then_some(sandbox_mode),
        allow_network: update_allow_network.then_some(allow_network),
        permission_profile: update_permission_profile.then_some(permission_profile),
        approvals_reviewer: update_approvals_reviewer.then_some(approvals_reviewer),
    };
    let updated_thread = cli
        .patch_runtime_permissions(&context, &thread, patch)
        .await
        .map_err(err_to_string)?;

    if update_approval_policy && updated_thread.engine_thread_id.is_some() {
        let policy = cli
            .runtime_permissions(&context, &updated_thread)
            .await
            .map_err(err_to_string)?
            .approval_policy
            .unwrap_or(Value::Null);
        if let Err(sync_error) = cli
            .sync_thread_execution_policy(&context, &updated_thread, &policy)
            .await
        {
            let rollback_permission_mode =
                if original_permission_mode.is_none() && thread.engine_id == "opencode" {
                    Some("[]".to_string())
                } else {
                    original_permission_mode.clone()
                };
            let rollback_result = run_db(db.clone(), {
                let thread_id = thread_id.clone();
                move |db| {
                    db::threads::update_thread_permissions(
                        db,
                        &thread_id,
                        rollback_permission_mode.as_deref(),
                    )
                    .map(|_| ())
                }
            })
            .await;
            if let Err(rollback_error) = rollback_result {
                return Err(format!(
                    "同步 OpenCode session 执行权限失败: {sync_error:#}; 回滚线程权限配置也失败: {rollback_error}"
                ));
            }
            return Err(format!(
                "同步 OpenCode session 执行权限失败: {sync_error:#}"
            ));
        }
    }

    run_db(db, {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found after execution policy update: {thread_id}"))
}

/// 读取当前线程对应 CLI 转换后的统一权限组件数据。
#[tauri::command]
pub async fn get_thread_permissions(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<PermissionComponentJson, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    let workspace = run_db(db, {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let tool = CliToolFactory::new(state.inner().clone())
        .create(thread.engine_id.as_str())
        .map_err(err_to_string)?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let cli: &dyn CliTool = tool.as_ref();
    cli.get_permissions(&context, &thread)
        .await
        .map_err(err_to_string)
}

/// 保存统一权限组件数据，由当前 CLI 实现转换并持久化其原始权限 JSON。
#[tauri::command]
pub async fn set_thread_permissions(
    state: State<'_, AppState>,
    thread_id: String,
    values: PermissionComponentJson,
) -> Result<PermissionComponentJson, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;
    let workspace = run_db(db, {
        let workspace_id = thread.workspace_id.clone();
        move |db| {
            db::workspaces::find_workspace_by_id(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
        }
    })
    .await?;
    let tool = CliToolFactory::new(state.inner().clone())
        .create(thread.engine_id.as_str())
        .map_err(err_to_string)?;
    let context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let cli: &dyn CliTool = tool.as_ref();
    cli.set_permissions(&context, &thread, values)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn set_thread_codex_config(
    state: State<'_, AppState>,
    thread_id: String,
    update_personality: bool,
    personality: Option<String>,
    update_service_tier: bool,
    service_tier: Option<String>,
    update_output_schema: bool,
    output_schema: Option<Value>,
) -> Result<ThreadDto, String> {
    set_thread_codex_config_inner(
        state.inner(),
        thread_id,
        update_personality,
        personality,
        update_service_tier,
        service_tier,
        update_output_schema,
        output_schema,
    )
    .await
}

async fn set_thread_codex_config_inner(
    state: &AppState,
    thread_id: String,
    update_personality: bool,
    personality: Option<String>,
    update_service_tier: bool,
    service_tier: Option<String>,
    update_output_schema: bool,
    output_schema: Option<Value>,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "codex" {
        return Err("Codex thread config is only available for Codex threads".to_string());
    }

    let normalized_personality = if update_personality {
        normalize_thread_personality(personality)?
    } else {
        None
    };
    let normalized_service_tier = if update_service_tier {
        normalize_thread_service_tier(service_tier)?
    } else {
        None
    };
    let normalized_output_schema = if update_output_schema {
        normalize_thread_output_schema(output_schema)?
    } else {
        None
    };

    let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        if update_personality {
            match normalized_personality {
                Some(value) => {
                    object.insert("personality".to_string(), json!(value));
                }
                None => {
                    object.remove("personality");
                }
            }
        }

        if update_service_tier {
            match normalized_service_tier {
                Some(value) => {
                    object.insert("serviceTier".to_string(), json!(value));
                }
                None => {
                    object.remove("serviceTier");
                }
            }
        }

        if update_output_schema {
            match normalized_output_schema {
                Some(value) => {
                    object.insert("outputSchema".to_string(), value);
                }
                None => {
                    object.remove("outputSchema");
                }
            }
        }
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
    .ok_or_else(|| format!("thread not found after Codex config update: {thread_id}"))
}

#[tauri::command]
pub async fn set_thread_opencode_config(
    state: State<'_, AppState>,
    thread_id: String,
    update_agent: bool,
    agent: Option<String>,
) -> Result<ThreadDto, String> {
    set_thread_opencode_config_inner(state.inner(), thread_id, update_agent, agent).await
}

async fn set_thread_opencode_config_inner(
    state: &AppState,
    thread_id: String,
    update_agent: bool,
    agent: Option<String>,
) -> Result<ThreadDto, String> {
    let db = state.db.clone();
    let thread = run_db(db.clone(), {
        let thread_id = thread_id.clone();
        move |db| db::threads::get_thread(db, &thread_id)
    })
    .await?
    .ok_or_else(|| format!("thread not found: {thread_id}"))?;

    if thread.engine_id != "opencode" {
        return Err("OpenCode thread config is only available for OpenCode threads".to_string());
    }

    let normalized_agent = if update_agent {
        normalize_thread_opencode_agent(agent)?
    } else {
        None
    };

    let mut metadata = thread.engine_metadata.unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        if update_agent {
            match normalized_agent {
                Some(value) => {
                    object.insert("opencodeAgent".to_string(), json!(value));
                }
                None => {
                    object.remove("opencodeAgent");
                }
            }
        }
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
    .ok_or_else(|| format!("thread not found after OpenCode config update: {thread_id}"))
}

pub(crate) async fn validate_reasoning_effort(
    state: &AppState,
    engine_id: &str,
    model_id: &str,
    requested_effort: &str,
) -> Result<String, String> {
    let models = state
        .engines
        .models_for_validation(engine_id, model_id)
        .await
        .ok();

    validate_reasoning_effort_from_catalog(model_id, requested_effort, models.as_deref())
}

fn validate_reasoning_effort_from_catalog(
    model_id: &str,
    requested_effort: &str,
    models: Option<&[ModelInfo]>,
) -> Result<String, String> {
    const KNOWN_REASONING_EFFORTS: &[&str] =
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    if !KNOWN_REASONING_EFFORTS.contains(&requested_effort) {
        return Err(format!(
            "invalid reasoning effort `{requested_effort}`. expected one of: {}",
            KNOWN_REASONING_EFFORTS.join(", ")
        ));
    }

    let Some(model) = models.and_then(|items| items.iter().find(|model| model.id == model_id))
    else {
        return Ok(requested_effort.to_string());
    };

    if let Some(option) = model
        .supported_reasoning_efforts
        .iter()
        .find(|option| option.reasoning_effort == requested_effort)
    {
        return Ok(option.reasoning_effort.clone());
    }

    let supported = model
        .supported_reasoning_efforts
        .iter()
        .map(|option| option.reasoning_effort.clone())
        .collect::<Vec<_>>()
        .join(", ");

    Err(format!(
        "reasoning effort `{requested_effort}` is not supported by model `{}`. supported values: {}",
        model.id, supported
    ))
}

async fn validate_model_for_thread_engine(
    state: &AppState,
    thread: &ThreadDto,
    requested_model_id: &str,
) -> Result<String, String> {
    if requested_model_id == thread.model_id {
        return Ok(thread.model_id.clone());
    }

    let db = state.db.clone();
    let workspace_id = thread.workspace_id.clone();
    let workspace = run_db(db, move |db| {
        db::workspaces::find_workspace_by_id(db, &workspace_id)?
            .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))
    })
    .await?;
    if workspace.location_kind == "ssh" {
        if !matches!(thread.engine_id.as_str(), "codex" | "opencode" | "claude") {
            return Err(format!(
                "SSH 远端项目当前阶段尚未接入 {} 正式对话",
                thread.engine_id
            ));
        }
        let normalized_model_id = requested_model_id.trim();
        if normalized_model_id.is_empty() {
            return Err("model id cannot be empty".to_string());
        }
        let models = match thread.engine_id.as_str() {
            "codex" => {
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let codex = CliToolFactory::new(state.clone())
                    .create("codex")
                    .expect("Codex CLI factory mapping must exist");
                let cli: &dyn CliTool = codex.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "opencode" => {
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let opencode = CliToolFactory::new(state.clone())
                    .create("opencode")
                    .expect("OpenCode CLI factory mapping must exist");
                let cli: &dyn CliTool = opencode.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            "claude" => {
                let context =
                    CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
                let claude = CliToolFactory::new(state.clone())
                    .create("claude")
                    .expect("Claude CLI factory mapping must exist");
                let cli: &dyn CliTool = claude.as_ref();
                cli.models_for_validation(&context, normalized_model_id)
                    .await
                    .map_err(err_to_string)?
            }
            _ => unreachable!(),
        };
        return validate_model_for_engine_from_catalog(
            &thread.engine_id,
            normalized_model_id,
            Some(models.as_slice()),
        );
    }
    validate_model_for_engine(state, &thread.engine_id, requested_model_id).await
}

fn merge_codex_runtime_metadata(
    existing: Option<serde_json::Value>,
    raw_status: Option<&str>,
    active_flags: &[String],
    preview: Option<&str>,
    sync_required: bool,
    sync_reason: Option<&str>,
) -> serde_json::Value {
    let mut metadata = existing.unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        // 线程底部六项已迁移到独立列；远端同步只保留状态 metadata，清除旧镜像键。
        for key in [
            "lastModelId",
            "reasoningEffort",
            "sandboxApprovalPolicy",
            "sandboxMode",
            "sandboxAllowNetwork",
            "permissionProfile",
            "approvalsReviewer",
            "claudePermissionMode",
            "opencodePermissionMode",
        ] {
            object.remove(key);
        }
        match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
            Some(status) => {
                object.insert("codexThreadStatus".to_string(), json!(status));
            }
            None => {
                object.remove("codexThreadStatus");
            }
        }

        if active_flags.is_empty() {
            object.remove("codexThreadActiveFlags");
        } else {
            object.insert("codexThreadActiveFlags".to_string(), json!(active_flags));
        }

        match preview.map(str::trim).filter(|value| !value.is_empty()) {
            Some(preview) => {
                object.insert("codexPreview".to_string(), json!(preview));
            }
            None => {
                object.remove("codexPreview");
            }
        }

        object.insert("codexSyncRequired".to_string(), json!(sync_required));
        if sync_required {
            object.insert(
                "codexSyncUpdatedAt".to_string(),
                json!(Utc::now().to_rfc3339()),
            );
            if let Some(reason) = sync_reason.map(str::trim).filter(|value| !value.is_empty()) {
                object.insert("codexSyncReason".to_string(), json!(reason));
            }
        } else {
            object.insert(
                "codexSyncUpdatedAt".to_string(),
                json!(Utc::now().to_rfc3339()),
            );
            object.insert("codexSyncReason".to_string(), serde_json::Value::Null);
        }
    }

    metadata
}

async fn build_codex_branch_context(
    state: &AppState,
    thread: &ThreadDto,
) -> Result<(String, String, SandboxPolicy), String> {
    let db = state.db.clone();
    let workspace = run_db(db, {
        let workspace_id = thread.workspace_id.clone();
        let thread_id = thread.id.clone();
        move |db| {
            let workspace = db::workspaces::list_workspaces(db)?
                .into_iter()
                .find(|item| item.id == workspace_id)
                .ok_or_else(|| anyhow::anyhow!("workspace not found for thread {thread_id}"))?;
            Ok(workspace)
        }
    })
    .await?;

    let workspace_root = workspace.root_path.clone();
    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .map_err(err_to_string)?;
    let cli_context = CliExecutionContext::from_workspace(&workspace).map_err(err_to_string)?;
    let runtime_permissions = codex
        .runtime_permissions(&cli_context, thread)
        .await
        .map_err(err_to_string)?;
    let auto_approve_mcp_elicitations = codex
        .auto_approve_mcp_elicitations(&cli_context, thread)
        .await
        .map_err(err_to_string)?;
    let sandbox_mode_override = runtime_permissions.sandbox_mode.clone();
    let sandbox_mode = sandbox_mode_override
        .clone()
        .unwrap_or_else(|| "workspace-write".to_string());
    let trust_level = workspace.trust_level.clone();
    let codex_external_sandbox_active =
        codex_external_sandbox_for_workspace(state, &workspace).await?;
    let permission_profile = runtime_permissions.permission_profile.clone();

    if permission_profile.is_none() {
        if unsupported_thread_sandbox_override_for_external_sandbox(
            sandbox_mode_override.as_deref(),
            codex_external_sandbox_active,
        ) {
            return Err(
                "Codex read-only and workspace-write sandbox overrides are unavailable while AuraCoder is using external sandbox mode. Clear the override or restore local Codex sandboxing first.".to_string(),
            );
        }

        validate_engine_sandbox_mode(thread.engine_id.as_str(), Some(sandbox_mode.as_str()))?;
    }

    let writable_roots = vec![workspace_root.clone()];
    let allow_network = if sandbox_mode.eq_ignore_ascii_case("danger-full-access") {
        true
    } else {
        runtime_permissions
            .allow_network
            .unwrap_or_else(|| allow_network_for_trust_level(&trust_level))
    };
    let approval_policy_override = runtime_permissions.approval_policy.clone();

    Ok((
        workspace_root,
        thread.model_id.clone(),
        SandboxPolicy {
            writable_roots,
            allow_network,
            auto_approve_mcp_elicitations,
            approval_policy: Some(approval_policy_override.unwrap_or_else(|| {
                Value::String(
                    approval_policy_for_engine_and_trust_level(
                        thread.engine_id.as_str(),
                        &trust_level,
                    )
                    .to_string(),
                )
            })),
            permission_profile,
            approvals_reviewer: runtime_permissions.approvals_reviewer.clone(),
            reasoning_effort: thread.reasoning_effort.clone(),
            sandbox_mode: Some(sandbox_mode),
            service_tier: thread_service_tier(thread.engine_metadata.as_ref()),
            personality: thread_personality(thread.engine_metadata.as_ref()),
            output_schema: thread_output_schema(thread.engine_metadata.as_ref()),
            opencode_agent: thread_opencode_agent(thread.engine_metadata.as_ref()),
        },
    ))
}

async fn create_codex_branch_thread(
    state: &AppState,
    source_thread: &ThreadDto,
    engine_thread_id: &str,
    model_id: &str,
    title: Option<&str>,
    preview: Option<&str>,
    raw_status: Option<&str>,
    active_flags: &[String],
    rollback_turns: Option<u32>,
) -> Result<ThreadDto, String> {
    if !codex_transcript_imported(source_thread.engine_metadata.as_ref()) {
        return Err(
            "native Codex history tools require a locally mirrored transcript. Attached remote threads without imported history cannot be forked or rolled back yet."
                .to_string(),
        );
    }

    let db = state.db.clone();
    run_db(db.clone(), {
        let source_thread = source_thread.clone();
        let engine_thread_id = engine_thread_id.to_string();
        let model_id = model_id.to_string();
        let title = title.map(str::to_string);
        let preview = preview.map(str::to_string);
        let raw_status = raw_status.map(str::to_string);
        let active_flags = active_flags.to_vec();
        move |db| {
            let clone_local_history = should_clone_local_branch_history(&source_thread);
            let created = db::threads::create_thread(
                db,
                &source_thread.workspace_id,
                &source_thread.engine_id,
                &model_id,
                title.as_deref().unwrap_or(&source_thread.title),
            )?;
            db::threads::set_engine_thread_id(db, &created.id, &engine_thread_id)?;
            if clone_local_history {
                db::messages::clone_thread_messages(db, &source_thread.id, &created.id)?;
                if let Some(turns) = rollback_turns {
                    db::messages::drop_last_turns(db, &created.id, turns)?;
                }
            }
            db::threads::refresh_thread_message_stats(
                db,
                &created.id,
                db::threads::ThreadMessageStatsActivityPolicy::FromMessages,
            )?;

            let metadata = clone_codex_branch_metadata(
                source_thread.engine_metadata.as_ref(),
                &model_id,
                raw_status.as_deref(),
                &active_flags,
                preview.as_deref(),
                !clone_local_history,
                (!clone_local_history).then_some("branch_thread_requires_sync"),
            );
            let next_status =
                map_codex_thread_status_to_local(raw_status.as_deref(), &active_flags, false);
            db::threads::update_thread_runtime_snapshot(
                db,
                &created.id,
                title.as_deref(),
                next_status,
                Some(&metadata),
            )
        }
    })
    .await
}

fn is_codex_thread_sync_required(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(|value| value.get("codexSyncRequired"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn should_clone_local_branch_history(source_thread: &ThreadDto) -> bool {
    !is_codex_thread_sync_required(source_thread.engine_metadata.as_ref())
        && source_thread.message_count > 0
}

fn clone_codex_branch_metadata(
    existing: Option<&Value>,
    model_id: &str,
    raw_status: Option<&str>,
    active_flags: &[String],
    preview: Option<&str>,
    sync_required: bool,
    sync_reason: Option<&str>,
) -> Value {
    let mut metadata = existing.cloned().unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        object.remove("manualTitle");
        object.remove("manualTitleUpdatedAt");
        object.insert("codexTranscriptImported".to_string(), json!(true));
    }

    merge_codex_runtime_metadata(
        Some(metadata),
        raw_status,
        active_flags,
        preview,
        sync_required,
        sync_reason,
    )
}

fn codex_transcript_imported(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(|value| value.get("codexTranscriptImported"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn approval_policy_for_engine_and_trust_level(
    engine_id: &str,
    trust_level: &TrustLevelDto,
) -> &'static str {
    match engine_id {
        "claude" => match trust_level {
            TrustLevelDto::Trusted => "trusted",
            TrustLevelDto::Standard => "standard",
            TrustLevelDto::Restricted => "restricted",
        },
        "opencode" => match trust_level {
            TrustLevelDto::Trusted | TrustLevelDto::Standard => "ask",
            TrustLevelDto::Restricted => "deny",
        },
        _ => match trust_level {
            TrustLevelDto::Trusted | TrustLevelDto::Standard => "on-request",
            TrustLevelDto::Restricted => "untrusted",
        },
    }
}

fn allow_network_for_trust_level(trust_level: &TrustLevelDto) -> bool {
    matches!(trust_level, TrustLevelDto::Trusted)
}

pub(crate) async fn migrate_legacy_codex_on_failure_thread_metadata(
    state: &AppState,
    thread: &mut ThreadDto,
) {
    let _ = (state, thread);
    /*
    // 旧 metadata 权限迁移执行体已停用，保留完整实现记录：
    if thread.engine_id != "codex"
        || thread
            .engine_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("sshRemote"))
            .and_then(Value::as_bool)
            == Some(true)
        || !is_legacy_codex_on_failure_metadata(thread.engine_metadata.as_ref())
    {
        return;
    }

    let db = state.db.clone();
    let workspace_id = thread.workspace_id.clone();
    let is_ssh_workspace = run_db(db.clone(), move |db| {
        Ok(db::workspaces::find_workspace_by_id(db, &workspace_id)?
            .is_some_and(|workspace| workspace.location_kind == "ssh"))
    })
    .await
    .unwrap_or(false);
    if is_ssh_workspace {
        return;
    }

    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let Ok(context) = codex
        .execution_context(Some(thread.workspace_id.as_str()))
        .await
    else {
        return;
    };
    let cli: &dyn CliTool = codex.as_ref();
    let Ok(codex_external_sandbox_active) = cli.uses_external_sandbox(&context).await else {
        return;
    };
    let Some(metadata) = migrate_legacy_codex_on_failure_metadata(
        thread.engine_metadata.as_ref(),
        codex_external_sandbox_active,
    ) else {
        return;
    };

    let thread_id = thread.id.clone();
    let metadata_for_db = metadata.clone();
    if let Err(error) = run_db(db, move |db| {
        db::threads::update_engine_metadata(db, &thread_id, &metadata_for_db)
    })
    .await
    {
        log::warn!(
            "failed to persist legacy Codex on-failure policy migration for thread {}: {error}",
            thread.id
        );
    }

    thread.engine_metadata = Some(metadata);
    */
}

fn is_legacy_codex_on_failure_metadata(metadata: Option<&Value>) -> bool {
    metadata
        .and_then(Value::as_object)
        .and_then(|object| object.get("sandboxApprovalPolicy"))
        .and_then(Value::as_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("on-failure"))
}

fn migrate_legacy_codex_on_failure_metadata(
    _metadata: Option<&Value>,
    _codex_external_sandbox_active: bool,
) -> Option<Value> {
    None
}
fn thread_approval_policy_override_value(
    _engine_id: &str,
    _metadata: Option<&Value>,
) -> Result<Option<Value>, String> {
    Ok(None)
}

fn thread_allow_network_override(_metadata: Option<&Value>) -> Option<bool> {
    None
}

fn thread_sandbox_mode(_metadata: Option<&Value>) -> Result<Option<String>, String> {
    Ok(None)
}

fn unsupported_thread_sandbox_override_for_external_sandbox(
    sandbox_mode: Option<&str>,
    external_sandbox_active: bool,
) -> bool {
    external_sandbox_active && matches!(sandbox_mode, Some("read-only" | "workspace-write"))
}

fn thread_reasoning_effort(_metadata: Option<&Value>) -> Option<String> {
    /*
    // 旧实现从 engine_metadata_json 读取 reasoningEffort，已停用：
    _metadata
        .and_then(|value| value.get("reasoningEffort"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    */
    None
}

fn thread_last_model_id(_metadata: Option<&Value>) -> Option<String> {
    None
}

fn thread_service_tier(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("serviceTier"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "fast" | "flex"))
        .map(ToOwned::to_owned)
}

fn thread_personality(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("personality"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| matches!(*value, "none" | "friendly" | "pragmatic"))
        .map(ToOwned::to_owned)
}

fn thread_output_schema(metadata: Option<&Value>) -> Option<Value> {
    metadata
        .and_then(|value| value.get("outputSchema"))
        .cloned()
}

fn thread_permission_profile(metadata: Option<&Value>) -> Option<Value> {
    metadata
        .and_then(|value| value.get("permissionProfile"))
        .cloned()
}

fn thread_approvals_reviewer(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("approvalsReviewer"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn thread_opencode_agent(metadata: Option<&Value>) -> Option<String> {
    metadata
        .and_then(|value| value.get("opencodeAgent"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn map_codex_thread_status_to_local(
    raw_status: Option<&str>,
    active_flags: &[String],
    has_local_turn: bool,
) -> Option<ThreadStatusDto> {
    if has_local_turn {
        return None;
    }

    match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
        Some("systemError") => Some(ThreadStatusDto::Error),
        Some("idle") | Some("notLoaded") => Some(ThreadStatusDto::Idle),
        Some("active") => {
            if active_flags
                .iter()
                .any(|flag| matches!(flag.as_str(), "waitingOnApproval" | "waitingOnUserInput"))
            {
                Some(ThreadStatusDto::AwaitingApproval)
            } else {
                Some(ThreadStatusDto::Streaming)
            }
        }
        _ => None,
    }
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexRemoteThreadArchiveState {
    Active,
    Archived,
    Missing,
}

async fn codex_remote_thread_archive_state(
    state: &AppState,
    engine_thread_id: &str,
) -> anyhow::Result<CodexRemoteThreadArchiveState> {
    let engine_thread_id = engine_thread_id.trim();
    if engine_thread_id.is_empty() {
        return Ok(CodexRemoteThreadArchiveState::Active);
    }

    let archived_threads = state
        .engines
        .list_codex_remote_threads(None, Some(true))
        .await?;
    if archived_threads
        .iter()
        .any(|thread| thread.engine_thread_id.as_str() == engine_thread_id)
    {
        return Ok(CodexRemoteThreadArchiveState::Archived);
    }

    let active_threads = state
        .engines
        .list_codex_remote_threads(None, Some(false))
        .await?;
    if active_threads
        .iter()
        .any(|thread| thread.engine_thread_id.as_str() == engine_thread_id)
    {
        Ok(CodexRemoteThreadArchiveState::Active)
    } else {
        Ok(CodexRemoteThreadArchiveState::Missing)
    }
}

fn approval_policy_metadata_key(engine_id: &str) -> &'static str {
    match engine_id {
        "claude" => "claudePermissionMode",
        "opencode" => "opencodePermissionMode",
        _ => "sandboxApprovalPolicy",
    }
}

fn normalize_thread_approval_policy_for_engine(
    engine_id: &str,
    value: Option<Value>,
) -> Result<Option<Value>, String> {
    let Some(value) = value else {
        return Ok(None);
    };

    match engine_id {
        "claude" => {
            let normalized = value
                .as_str()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .map(str::to_lowercase)
                .ok_or_else(|| {
                    "invalid Claude permission mode. expected a string value".to_string()
                })?;

            match normalized.as_str() {
                "restricted" | "standard" | "trusted" => {
                    Ok(Some(Value::String(normalized)))
                }
                _ => Err(format!(
                    "invalid Claude permission mode `{normalized}`. expected one of: restricted, standard, trusted"
                )),
            }
        }
        "opencode" => {
            let normalized = value
                .as_str()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .map(str::to_lowercase)
                .ok_or_else(|| {
                    "invalid OpenCode permission mode. expected a string value".to_string()
                })?;

            match normalized.as_str() {
                "ask" | "allow" | "deny" => Ok(Some(Value::String(normalized))),
                _ => Err(format!(
                    "invalid OpenCode permission mode `{normalized}`. expected one of: ask, allow, deny"
                )),
            }
        }
        _ => normalize_codex_approval_policy(value).map(Some),
    }
}

fn normalize_codex_approval_policy(value: Value) -> Result<Value, String> {
    match value {
        Value::String(raw) => {
            let normalized = raw.trim().to_lowercase();
            match normalized.as_str() {
                "on-failure" => Ok(Value::String("on-request".to_string())),
                "untrusted" | "on-request" | "never" => {
                    Ok(Value::String(normalized))
                }
                _ => Err(format!(
                    "invalid approval policy `{normalized}`. expected one of: untrusted, on-request, never"
                )),
            }
        }
        Value::Object(object) => {
            let reject = object
                .get("reject")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    "invalid structured approval policy. expected a `reject` object".to_string()
                })?;

            for required_key in ["mcp_elicitations", "rules", "sandbox_approval"] {
                if !reject.get(required_key).and_then(Value::as_bool).is_some() {
                    return Err(format!(
                        "invalid structured approval policy. missing boolean reject.{required_key}"
                    ));
                }
            }

            if reject.contains_key("request_permissions")
                && reject
                    .get("request_permissions")
                    .and_then(Value::as_bool)
                    .is_none()
            {
                return Err(
                    "invalid structured approval policy. reject.request_permissions must be a boolean"
                        .to_string(),
                );
            }

            Ok(Value::Object(object))
        }
        _ => Err(
            "invalid approval policy. expected a string mode or structured reject object"
                .to_string(),
        ),
    }
}

fn normalize_thread_personality(value: Option<String>) -> Result<Option<String>, String> {
    let normalized = value
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_lowercase);

    let Some(normalized) = normalized else {
        return Ok(None);
    };

    match normalized.as_str() {
        "none" | "friendly" | "pragmatic" => Ok(Some(normalized)),
        _ => Err(format!(
            "invalid personality `{normalized}`. expected one of: none, friendly, pragmatic"
        )),
    }
}

fn normalize_thread_service_tier(value: Option<String>) -> Result<Option<String>, String> {
    let normalized = value
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_lowercase);

    let Some(normalized) = normalized else {
        return Ok(None);
    };

    match normalized.as_str() {
        "fast" | "flex" => Ok(Some(normalized)),
        _ => Err(format!(
            "invalid service tier `{normalized}`. expected one of: fast, flex"
        )),
    }
}

fn normalize_thread_output_schema(value: Option<Value>) -> Result<Option<Value>, String> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        Value::Object(_) | Value::Bool(_) => Ok(Some(value)),
        _ => Err("invalid output schema. expected a JSON Schema object or boolean".to_string()),
    }
}

fn normalize_thread_permission_profile(value: Option<Value>) -> Result<Option<Value>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err("invalid permission profile. expected a profile object".to_string());
    };
    let profile_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| "invalid permission profile. missing string `type`".to_string())?;
    match profile_type {
        "managed" => {
            validate_permission_profile_file_system(object.get("fileSystem"))?;
            validate_permission_profile_network(object.get("network"))?;
        }
        "external" => {
            validate_permission_profile_network(object.get("network"))?;
        }
        "disabled" => {}
        _ => {
            return Err(format!(
                "invalid permission profile type `{profile_type}`. expected one of: managed, external, disabled"
            ));
        }
    }
    Ok(Some(value))
}

fn validate_permission_profile_file_system(value: Option<&Value>) -> Result<(), String> {
    let Some(file_system) = value.and_then(Value::as_object) else {
        return Err("invalid permission profile. managed.fileSystem must be an object".to_string());
    };
    let fs_type = file_system
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| {
            "invalid permission profile. managed.fileSystem.type must be a string".to_string()
        })?;
    match fs_type {
        "unrestricted" => Ok(()),
        "restricted" => {
            let entries = file_system.get("entries").and_then(Value::as_array).ok_or_else(
                || {
                    "invalid permission profile. managed.fileSystem.entries must be an array"
                        .to_string()
                },
            )?;
            for entry in entries {
                validate_permission_profile_file_system_entry(entry)?;
            }
            Ok(())
        }
        _ => Err(format!(
            "invalid permission profile filesystem type `{fs_type}`. expected one of: restricted, unrestricted"
        )),
    }
}

fn validate_permission_profile_file_system_entry(value: &Value) -> Result<(), String> {
    let Some(entry) = value.as_object() else {
        return Err("invalid permission profile. fileSystem entry must be an object".to_string());
    };
    let access = entry
        .get("access")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| {
            "invalid permission profile. fileSystem entry access must be a string".to_string()
        })?;
    if !matches!(access, "read" | "write" | "none") {
        return Err(format!(
            "invalid permission profile fileSystem entry access `{access}`. expected one of: read, write, none"
        ));
    }
    let Some(path) = entry.get("path").and_then(Value::as_object) else {
        return Err(
            "invalid permission profile. fileSystem entry path must be an object".to_string(),
        );
    };
    if path.get("type").and_then(Value::as_str).is_none() {
        return Err(
            "invalid permission profile. fileSystem entry path.type must be a string".to_string(),
        );
    }
    Ok(())
}

fn validate_permission_profile_network(value: Option<&Value>) -> Result<(), String> {
    let Some(network) = value.and_then(Value::as_object) else {
        return Err("invalid permission profile. network must be an object".to_string());
    };
    if network.get("enabled").and_then(Value::as_bool).is_none() {
        return Err("invalid permission profile. network.enabled must be a boolean".to_string());
    }
    Ok(())
}

fn normalize_thread_approvals_reviewer(value: Option<String>) -> Result<Option<String>, String> {
    let normalized = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let Some(normalized) = normalized else {
        return Ok(None);
    };
    match normalized.as_str() {
        "user" | "auto_review" | "guardian_subagent" => Ok(Some(normalized)),
        _ => Err(format!(
            "invalid approvals reviewer `{normalized}`. expected one of: user, auto_review, guardian_subagent"
        )),
    }
}

fn normalize_thread_opencode_agent(value: Option<String>) -> Result<Option<String>, String> {
    let normalized = value
        .as_deref()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty());

    let Some(normalized) = normalized else {
        return Ok(None);
    };

    if normalized.chars().count() > 120 {
        return Err("invalid OpenCode agent. name is too long".to_string());
    }

    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "invalid OpenCode agent. expected letters, numbers, dots, dashes, or underscores"
                .to_string(),
        );
    }

    if normalized == "build" {
        return Ok(None);
    }

    Ok(Some(normalized.to_string()))
}

fn normalize_thread_sandbox_mode(value: Option<String>) -> Result<Option<String>, String> {
    let normalized = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase());

    let Some(normalized) = normalized else {
        return Ok(None);
    };

    let canonical = match normalized.as_str() {
        "readonly" | "read-only" | "read_only" => "read-only",
        "workspacewrite" | "workspace-write" | "workspace_write" => "workspace-write",
        "dangerfullaccess" | "danger-full-access" | "danger_full_access" => {
            "danger-full-access"
        }
        _ => {
            return Err(format!(
                "invalid sandbox mode `{normalized}`. expected one of: read-only, workspace-write, danger-full-access"
            ))
        }
    };

    Ok(Some(canonical.to_string()))
}

#[cfg(test)]
fn thread_allow_network(metadata: Option<&serde_json::Value>) -> Option<bool> {
    metadata
        .and_then(serde_json::Value::as_object)
        .and_then(|value| value.get("sandboxAllowNetwork"))
        .and_then(serde_json::Value::as_bool)
}

fn normalize_thread_title(raw: &str) -> Result<String, String> {
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

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use super::*;
    use crate::{
        config::app_config::AppConfig,
        engines::EngineManager,
        git::{repo::FileTreeCache, watcher::GitWatcherManager},
        power::KeepAwakeManager,
        state::{AppState, TurnManager},
        terminal::TerminalManager,
        terminal_notifications::TerminalNotificationManager,
    };
    use uuid::Uuid;

    fn test_app_state() -> AppState {
        let root = std::env::temp_dir().join(format!("auracoder-threads-cmd-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("failed to create temp root");
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
            scheduled_tasks: Arc::new(crate::scheduled_tasks::ScheduledTaskManager::new()),
            computer_control_service: Arc::new(
                crate::computer_control_service::ComputerControlService::default(),
            ),
            auracoder_thread_mcp_service: Arc::new(
                crate::auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()),
            ),
            mcp_gateway: Arc::new(crate::mcp_gateway::AuraCoderMcpGateway::new()),
            remote_access: Arc::new(crate::remote::RemoteTunnelManager::default()),
            ssh_monitor: Arc::new(crate::ssh::monitor::SshConnectionMonitor::default()),
        }
    }

    fn test_workspace(state: &AppState) -> crate::models::WorkspaceDto {
        let workspace_root =
            std::env::temp_dir().join(format!("auracoder-threads-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&workspace_root).expect("failed to create workspace root");
        crate::db::workspaces::upsert_workspace(
            &state.db,
            workspace_root.to_string_lossy().as_ref(),
        )
        .expect("failed to create workspace")
    }

    fn test_thread(state: &AppState, engine_id: &str, model_id: &str) -> ThreadDto {
        let workspace = test_workspace(state);
        crate::db::threads::create_thread(&state.db, &workspace.id, engine_id, model_id, "Thread")
            .expect("failed to create thread")
    }

    #[test]
    fn pending_opencode_permission_query_excludes_questions() {
        let state = test_app_state();
        let thread = test_thread(&state, "opencode", "openai/gpt-5");
        let message = db::messages::insert_user_message(
            &state.db,
            &thread.id,
            "permission request",
            None,
            Some("opencode"),
            Some("openai/gpt-5"),
            None,
        )
        .unwrap();
        let action_type = crate::engines::events::ActionType::Command;
        db::actions::insert_approval(
            &state.db,
            "permission-approval",
            &thread.id,
            &message.id,
            &action_type,
            "permission",
            &json!({ "_opencodeRequestKind": "permission" }),
        )
        .unwrap();
        db::actions::insert_approval(
            &state.db,
            "question-approval",
            &thread.id,
            &message.id,
            &action_type,
            "question",
            &json!({ "_opencodeRequestKind": "question" }),
        )
        .unwrap();

        assert_eq!(
            db::actions::find_pending_opencode_permission_approval_ids(&state.db, &thread.id)
                .unwrap(),
            vec!["permission-approval".to_string()]
        );
    }

    #[tokio::test]
    async fn opencode_permission_update_failure_keeps_original_metadata() {
        let state = test_app_state();
        let thread = test_thread(&state, "opencode", "openai/gpt-5");
        db::threads::set_engine_thread_id(&state.db, &thread.id, "session-1").unwrap();
        db::threads::update_engine_metadata(
            &state.db,
            &thread.id,
            &json!({ "opencodePermissionMode": "ask" }),
        )
        .unwrap();

        let result = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            true,
            Some(json!("allow")),
            false,
            None,
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .await;
        assert!(result.is_err());
        let latest = db::threads::get_thread(&state.db, &thread.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            latest.engine_metadata,
            Some(json!({ "opencodePermissionMode": "ask" }))
        );
    }

    #[test]
    fn thread_allow_network_reads_explicit_override_in_full_access_mode() {
        let metadata = json!({
            "sandboxMode": "danger-full-access",
            "sandboxAllowNetwork": false,
        });

        assert_eq!(thread_allow_network(Some(&metadata)), Some(false));
    }

    #[test]
    fn normalize_thread_sandbox_mode_accepts_aliases() {
        assert_eq!(
            normalize_thread_sandbox_mode(Some("danger_full_access".to_string())).unwrap(),
            Some("danger-full-access".to_string())
        );
        assert_eq!(
            normalize_thread_sandbox_mode(Some("read_only".to_string())).unwrap(),
            Some("read-only".to_string())
        );
    }

    #[test]
    fn normalize_thread_approval_policy_accepts_claude_modes() {
        assert_eq!(
            normalize_thread_approval_policy_for_engine("claude", Some(json!("trusted"))).unwrap(),
            Some(json!("trusted"))
        );
        assert_eq!(
            normalize_thread_approval_policy_for_engine("claude", Some(json!("STANDARD"))).unwrap(),
            Some(json!("standard"))
        );
    }
    #[test]
    fn normalize_thread_approval_policy_migrates_legacy_codex_on_failure() {
        assert_eq!(
            normalize_thread_approval_policy_for_engine("codex", Some(json!("on-failure")))
                .unwrap(),
            Some(json!("on-request"))
        );
    }

    #[test]
    fn legacy_codex_on_failure_metadata_migration_is_disabled() {
        let metadata = json!({
            "sandboxApprovalPolicy": "on-failure",
            "sandboxMode": "read-only",
            "sandboxAllowNetwork": false,
        });

        assert_eq!(
            migrate_legacy_codex_on_failure_metadata(Some(&metadata), false),
            None
        );
        /*
        // 旧 metadata 迁移已停用：
        assert_eq!(
            migrate_legacy_codex_on_failure_metadata(Some(&metadata), false),
            Some(json!({
                "sandboxApprovalPolicy": "on-request",
                "sandboxMode": "workspace-write",
                "sandboxAllowNetwork": false,
            }))
        );
        */
    }

    #[test]
    fn legacy_codex_on_failure_metadata_migration_is_disabled_for_external_sandbox() {
        let metadata = json!({
            "sandboxApprovalPolicy": "on-failure",
            "sandboxMode": "workspace-write",
        });

        assert_eq!(
            migrate_legacy_codex_on_failure_metadata(Some(&metadata), true),
            None
        );
        /*
        // 旧 metadata 迁移已停用：
        let migrated = migrate_legacy_codex_on_failure_metadata(Some(&metadata), true)
            .expect("legacy metadata should migrate");
        assert_eq!(
            migrated.get("sandboxApprovalPolicy"),
            Some(&json!("on-request"))
        );
        assert!(migrated.get("sandboxMode").is_none());
        */
    }

    #[test]
    fn normalize_thread_approval_policy_rejects_codex_values_for_claude() {
        assert!(
            normalize_thread_approval_policy_for_engine("claude", Some(json!("on-request")))
                .is_err()
        );
    }

    #[test]
    fn normalize_thread_approval_policy_accepts_opencode_modes() {
        assert_eq!(
            normalize_thread_approval_policy_for_engine("opencode", Some(json!("ALLOW"))).unwrap(),
            Some(json!("allow"))
        );
        assert_eq!(
            normalize_thread_approval_policy_for_engine("opencode", Some(json!("ask"))).unwrap(),
            Some(json!("ask"))
        );
        assert!(
            normalize_thread_approval_policy_for_engine("opencode", Some(json!("on-request")))
                .is_err()
        );
    }

    #[test]
    fn normalize_thread_approval_policy_accepts_structured_codex_policy() {
        let normalized = normalize_thread_approval_policy_for_engine(
            "codex",
            Some(json!({
                "reject": {
                    "mcp_elicitations": false,
                    "request_permissions": true,
                    "rules": true,
                    "sandbox_approval": false
                }
            })),
        )
        .expect("expected structured policy to validate");

        assert_eq!(
            normalized,
            Some(json!({
                "reject": {
                    "mcp_elicitations": false,
                    "request_permissions": true,
                    "rules": true,
                    "sandbox_approval": false
                }
            }))
        );
    }

    #[test]
    fn normalize_thread_personality_accepts_known_values() {
        assert_eq!(
            normalize_thread_personality(Some("Friendly".to_string())).unwrap(),
            Some("friendly".to_string())
        );
        assert_eq!(
            normalize_thread_service_tier(Some(" FLEX ".to_string())).unwrap(),
            Some("flex".to_string())
        );
        assert_eq!(
            normalize_thread_output_schema(Some(json!(true))).unwrap(),
            Some(json!(true))
        );
    }

    #[test]
    fn merge_codex_runtime_metadata_sets_runtime_fields() {
        let metadata = merge_codex_runtime_metadata(
            Some(json!({
                "existing": true,
                "codexSyncRequired": true,
                "codexSyncReason": "stale",
            })),
            Some("active"),
            &["waitingOnApproval".to_string()],
            Some("Preview"),
            false,
            None,
        );

        assert_eq!(metadata.get("existing"), Some(&json!(true)));
        assert_eq!(metadata.get("codexThreadStatus"), Some(&json!("active")));
        assert_eq!(
            metadata.get("codexThreadActiveFlags"),
            Some(&json!(["waitingOnApproval"]))
        );
        assert_eq!(metadata.get("codexPreview"), Some(&json!("Preview")));
        assert_eq!(metadata.get("codexSyncRequired"), Some(&json!(false)));
        assert_eq!(
            metadata.get("codexSyncReason"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn map_codex_thread_status_to_local_honors_waiting_flags() {
        assert_eq!(
            map_codex_thread_status_to_local(
                Some("active"),
                &["waitingOnApproval".to_string()],
                false,
            ),
            Some(ThreadStatusDto::AwaitingApproval)
        );
        assert_eq!(
            map_codex_thread_status_to_local(Some("systemError"), &[], false),
            Some(ThreadStatusDto::Error)
        );
        assert_eq!(
            map_codex_thread_status_to_local(Some("active"), &[], true),
            None
        );
    }

    #[test]
    fn build_codex_remote_thread_title_prefers_thread_title_then_preview() {
        let titled = CodexRemoteThreadSummary {
            engine_thread_id: "thread-12345678".to_string(),
            title: Some("  Remote title  ".to_string()),
            preview: "Preview line".to_string(),
            cwd: "/workspace".to_string(),
            created_at: 1_710_000_000,
            updated_at: 1_710_000_001,
            model_id: Some("gpt-5.4".to_string()),
            reasoning_effort: Some("high".to_string()),
            model_provider: "openai".to_string(),
            source_kind: "appServer".to_string(),
            status_type: "idle".to_string(),
            active_flags: Vec::new(),
            archived: false,
        };
        let preview_only = CodexRemoteThreadSummary {
            title: None,
            preview: "First line\nSecond line".to_string(),
            ..titled.clone()
        };

        assert_eq!(build_codex_remote_thread_title(&titled), "Remote title");
        assert_eq!(build_codex_remote_thread_title(&preview_only), "First line");
    }

    #[test]
    fn build_codex_remote_thread_metadata_sets_remote_fields() {
        let summary = CodexRemoteThreadSummary {
            engine_thread_id: "thread-12345678".to_string(),
            title: Some("Remote title".to_string()),
            preview: "Preview line".to_string(),
            cwd: "/workspace".to_string(),
            created_at: 1_710_000_000,
            updated_at: 1_710_000_001,
            model_id: Some("gpt-5.4".to_string()),
            reasoning_effort: Some("high".to_string()),
            model_provider: "openai".to_string(),
            source_kind: "appServer".to_string(),
            status_type: "active".to_string(),
            active_flags: vec!["waitingOnApproval".to_string()],
            archived: true,
        };

        let metadata = build_codex_remote_thread_metadata(&summary, "gpt-5.4", Some("high"));

        assert_eq!(metadata.get("lastModelId"), None);
        assert_eq!(metadata.get("reasoningEffort"), None);
        /*
        // 旧远端 metadata 镜像断言已停用：
        assert_eq!(metadata.get("lastModelId"), Some(&json!("gpt-5.4")));
        assert_eq!(metadata.get("reasoningEffort"), Some(&json!("high")));
        */
        assert_eq!(metadata.get("codexTranscriptImported"), Some(&json!(false)));
        assert_eq!(metadata.get("codexModelProvider"), Some(&json!("openai")));
        assert_eq!(metadata.get("codexSourceKind"), Some(&json!("appServer")));
        assert_eq!(metadata.get("codexRemoteArchived"), Some(&json!(true)));
        assert_eq!(metadata.get("codexRemoteCwd"), Some(&json!("/workspace")));
        assert_eq!(metadata.get("codexThreadStatus"), Some(&json!("active")));
        assert_eq!(
            metadata.get("codexThreadActiveFlags"),
            Some(&json!(["waitingOnApproval"]))
        );
        assert_eq!(metadata.get("codexPreview"), Some(&json!("Preview line")));
        assert_eq!(metadata.get("codexSyncRequired"), Some(&json!(true)));
        assert_eq!(
            metadata.get("codexSyncReason"),
            Some(&json!("remote_thread_attached"))
        );
    }

    #[test]
    fn remote_timestamp_format_accepts_opencode_milliseconds() {
        assert_eq!(
            codex_remote_thread_timestamp_to_rfc3339(1_777_155_663_506),
            "2026-04-25T22:21:03.506+00:00"
        );
    }

    #[tokio::test]
    async fn archive_thread_locally_hides_thread_without_calling_the_engine() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.4");
        crate::db::threads::set_engine_thread_id(&state.db, &thread.id, "engine-thread-1")
            .expect("expected engine thread id to be set");

        archive_thread_locally_inner(&state, &thread.id)
            .await
            .expect("expected local archive to succeed");

        let visible =
            crate::db::threads::list_threads_for_workspace(&state.db, &thread.workspace_id)
                .expect("expected visible thread list");
        assert!(!visible.iter().any(|candidate| candidate.id == thread.id));

        let archived = crate::db::threads::list_archived_threads_for_workspace(
            &state.db,
            &thread.workspace_id,
        )
        .expect("expected archived thread list");
        assert!(archived.iter().any(|candidate| candidate.id == thread.id));
    }

    #[test]
    fn build_opencode_remote_session_metadata_sets_remote_fields() {
        let summary = OpenCodeRemoteSessionSummary {
            engine_thread_id: "ses_12345678".to_string(),
            title: Some("OpenCode title".to_string()),
            cwd: "/workspace".to_string(),
            created_at: 1_777_155_663_506,
            updated_at: 1_777_155_663_524,
            archived: true,
        };

        let metadata = build_opencode_remote_session_metadata(
            Some(&json!({
                "opencodeAgent": "plan",
                "reasoningEffort": "high"
            })),
            &summary,
            "opencode/big-pickle",
        );

        assert_eq!(
            build_opencode_remote_session_title(&summary),
            "OpenCode title"
        );
        assert_eq!(metadata.get("lastModelId"), None);
        assert_eq!(
            metadata.get("opencodeRemoteSessionAttached"),
            Some(&json!(true))
        );
        assert_eq!(metadata.get("opencodeRemoteArchived"), Some(&json!(true)));
        assert_eq!(
            metadata.get("opencodeRemoteCwd"),
            Some(&json!("/workspace"))
        );
        assert_eq!(
            metadata.get("opencodeTranscriptImported"),
            Some(&json!(false))
        );
        assert_eq!(metadata.get("opencodeAgent"), Some(&json!("plan")));
        assert_eq!(metadata.get("reasoningEffort"), None);
        /*
        // 旧远端 metadata 镜像断言已停用：
        assert_eq!(
            metadata.get("lastModelId"),
            Some(&json!("opencode/big-pickle"))
        );
        assert_eq!(metadata.get("reasoningEffort"), Some(&json!("high")));
        */
    }

    #[test]
    fn clone_codex_branch_metadata_marks_local_transcript_as_imported() {
        let metadata = clone_codex_branch_metadata(
            Some(&json!({
                "codexTranscriptImported": false,
                "manualTitle": true,
            })),
            "gpt-5.4",
            Some("idle"),
            &[],
            Some("Preview"),
            false,
            None,
        );

        assert_eq!(metadata.get("codexTranscriptImported"), Some(&json!(true)));
        assert_eq!(metadata.get("lastModelId"), None);
        /*
        // 旧分支 metadata 镜像断言已停用：
        assert_eq!(metadata.get("lastModelId"), Some(&json!("gpt-5.4")));
        */
        assert_eq!(metadata.get("manualTitle"), None);
    }

    #[test]
    fn should_clone_local_branch_history_requires_synced_local_messages() {
        let mut thread = ThreadDto {
            id: "thread-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            engine_id: "codex".to_string(),
            model_id: "gpt-5.4".to_string(),
            engine_thread_id: Some("engine-thread-1".to_string()),
            engine_metadata: Some(json!({
                "codexSyncRequired": false,
            })),
            plan_mode: None,
            send_method: None,
            reasoning_effort: None,
            permission_mode: None,
            title: "Thread".to_string(),
            status: ThreadStatusDto::Idle,
            message_count: 2,
            total_tokens: 0,
            created_at: "2026-03-13T00:00:00Z".to_string(),
            last_activity_at: "2026-03-13T00:00:00Z".to_string(),
        };

        assert!(should_clone_local_branch_history(&thread));

        thread.message_count = 0;
        assert!(!should_clone_local_branch_history(&thread));

        thread.message_count = 2;
        thread.engine_metadata = Some(json!({
            "codexSyncRequired": true,
        }));
        assert!(!should_clone_local_branch_history(&thread));
    }

    #[tokio::test]
    async fn create_codex_branch_thread_rejects_threads_without_imported_transcript() {
        let state = test_app_state();
        let mut thread = test_thread(&state, "codex", "gpt-5.4");
        thread.engine_metadata = Some(json!({
            "codexTranscriptImported": false,
        }));

        let error = create_codex_branch_thread(
            &state,
            &thread,
            "engine-thread-branch",
            "gpt-5.4",
            Some("Fork"),
            None,
            Some("idle"),
            &[],
            None,
        )
        .await
        .expect_err("expected branch creation to reject missing local transcript");

        assert!(error.contains("locally mirrored transcript"));
    }

    #[test]
    fn clone_codex_branch_metadata_preserves_sync_needed_state() {
        let metadata = clone_codex_branch_metadata(
            Some(&json!({
                "manualTitle": true,
                "manualTitleUpdatedAt": "2026-03-12T00:00:00Z",
                "codexPreview": "old preview",
                "codexThreadStatus": "active",
                "codexThreadActiveFlags": ["waitingOnApproval"],
                "codexSyncRequired": false,
                "serviceTier": "fast",
            })),
            "gpt-5.4",
            Some("active"),
            &["waitingOnApproval".to_string()],
            Some("Fresh preview"),
            true,
            Some("branch_thread_requires_sync"),
        );

        assert_eq!(metadata.get("manualTitle"), None);
        assert_eq!(metadata.get("manualTitleUpdatedAt"), None);
        assert_eq!(metadata.get("lastModelId"), None);
        /*
        // 旧分支 metadata 镜像断言已停用：
        assert_eq!(metadata.get("lastModelId"), Some(&json!("gpt-5.4")));
        */
        assert_eq!(metadata.get("codexPreview"), Some(&json!("Fresh preview")));
        assert_eq!(metadata.get("codexThreadStatus"), Some(&json!("active")));
        assert_eq!(
            metadata.get("codexThreadActiveFlags"),
            Some(&json!(["waitingOnApproval"]))
        );
        assert_eq!(metadata.get("codexSyncRequired"), Some(&json!(true)));
        assert_eq!(
            metadata.get("codexSyncReason"),
            Some(&json!("branch_thread_requires_sync"))
        );
        assert_eq!(metadata.get("serviceTier"), Some(&json!("fast")));
    }

    #[tokio::test]
    async fn create_thread_inner_persists_initial_independent_runtime_fields() {
        let state = test_app_state();
        let workspace = test_workspace(&state);

        let created = create_thread_inner(
            &state,
            workspace.id.clone(),
            "codex".to_string(),
            "gpt-5.4".to_string(),
            "Thread".to_string(),
            Some("HIGH".to_string()),
            Some("FAST".to_string()),
            None,
        )
        .await
        .expect("expected thread creation to succeed");

        assert_eq!(created.reasoning_effort.as_deref(), Some("high"));
        let metadata = created
            .engine_metadata
            .expect("expected service tier metadata to be stored");
        assert_eq!(metadata.get("reasoningEffort"), None);
        assert_eq!(metadata.get("lastModelId"), None);
        /*
        // 旧 reasoning metadata 镜像断言已停用：
        assert_eq!(metadata.get("reasoningEffort"), Some(&json!("high")));
        */
        assert_eq!(metadata.get("serviceTier"), Some(&json!("fast")));
    }

    #[tokio::test]
    async fn create_thread_inner_persists_initial_autonomy_policy() {
        let state = test_app_state();
        let workspace = test_workspace(&state);

        let created = create_thread_inner(
            &state,
            workspace.id.clone(),
            "claude".to_string(),
            "claude-sonnet-4-6".to_string(),
            "Thread".to_string(),
            None,
            None,
            Some("read-only".to_string()),
        )
        .await
        .expect("expected thread creation to succeed");

        let context = CliExecutionContext::from_workspace(&workspace)
            .expect("expected Claude workspace context");
        let claude = CliToolFactory::new(state.clone())
            .create("claude")
            .expect("expected Claude CLI implementation");
        let permissions = claude
            .get_permissions(&context, &created)
            .await
            .expect("expected Claude permissions");
        assert_eq!(
            permissions.get("autonomyPreset"),
            Some(&json!(["read-only"]))
        );
        let metadata = created.engine_metadata.unwrap_or_else(|| json!({}));
        assert_eq!(metadata.get("claudePermissionMode"), None);
        assert_eq!(metadata.get("sandboxMode"), None);
        assert_eq!(metadata.get("sandboxAllowNetwork"), None);
    }

    #[tokio::test]
    async fn create_thread_inner_rejects_invalid_reasoning_effort() {
        let state = test_app_state();
        let workspace = test_workspace(&state);

        let error = create_thread_inner(
            &state,
            workspace.id,
            "codex".to_string(),
            "gpt-5.4".to_string(),
            "Thread".to_string(),
            Some("turbo".to_string()),
            None,
            None,
        )
        .await
        .expect_err("expected invalid effort to be rejected");

        assert!(error.contains("invalid reasoning effort `turbo`"));
    }

    #[tokio::test]
    async fn create_thread_inner_rejects_service_tier_for_non_codex_threads() {
        let state = test_app_state();
        let workspace = test_workspace(&state);

        let error = create_thread_inner(
            &state,
            workspace.id,
            "claude".to_string(),
            "claude-sonnet-4-6".to_string(),
            "Thread".to_string(),
            None,
            Some("fast".to_string()),
            None,
        )
        .await
        .expect_err("expected non-codex service tier to be rejected");

        assert!(error.contains("service tier is only supported for Codex threads"));
    }

    #[test]
    fn autonomy_policy_for_preset_matches_engine_contracts() {
        assert_eq!(
            autonomy_policy_for_preset("codex", "auto", false).unwrap(),
            AutonomyPresetPolicy {
                approval_policy: json!("on-request"),
                sandbox_mode: Some("workspace-write"),
                allow_network: Some(true),
            }
        );
        assert_eq!(
            autonomy_policy_for_preset("codex", "read-only", false).unwrap(),
            AutonomyPresetPolicy {
                approval_policy: json!("untrusted"),
                sandbox_mode: Some("read-only"),
                allow_network: Some(false),
            }
        );
        assert_eq!(
            autonomy_policy_for_preset("claude", "full", false).unwrap(),
            AutonomyPresetPolicy {
                approval_policy: json!("trusted"),
                sandbox_mode: Some("workspace-write"),
                allow_network: Some(true),
            }
        );
        assert_eq!(
            autonomy_policy_for_preset("opencode", "auto", false).unwrap(),
            AutonomyPresetPolicy {
                approval_policy: json!("allow"),
                sandbox_mode: None,
                allow_network: None,
            }
        );
    }

    #[test]
    fn external_sandbox_omits_blocked_codex_overrides() {
        for preset in ["read-only", "ask", "auto"] {
            assert_eq!(
                autonomy_policy_for_preset("codex", preset, true)
                    .unwrap()
                    .sandbox_mode,
                None
            );
        }
        assert_eq!(
            autonomy_policy_for_preset("codex", "full", true)
                .unwrap()
                .sandbox_mode,
            Some("danger-full-access")
        );
    }

    #[tokio::test]
    async fn set_thread_execution_policy_allows_claude_read_only() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");

        let updated = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            false,
            None,
            true,
            Some("read-only".to_string()),
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .expect("expected read-only update to succeed");

        let workspace =
            crate::db::workspaces::find_workspace_by_id(&state.db, &thread.workspace_id)
                .unwrap()
                .expect("expected workspace");
        let context = CliExecutionContext::from_workspace(&workspace).unwrap();
        let claude = CliToolFactory::new(state.clone()).create("claude").unwrap();
        let permissions = claude
            .runtime_permissions(&context, &updated)
            .await
            .unwrap();
        assert_eq!(permissions.sandbox_mode.as_deref(), Some("read-only"));
        assert!(updated
            .engine_metadata
            .as_ref()
            .is_none_or(|value| value.get("sandboxMode").is_none()));
    }

    #[tokio::test]
    async fn set_thread_execution_policy_allows_claude_workspace_write() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");

        let updated = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            false,
            None,
            true,
            Some("workspace-write".to_string()),
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .expect("expected workspace-write update to succeed");

        let workspace =
            crate::db::workspaces::find_workspace_by_id(&state.db, &thread.workspace_id)
                .unwrap()
                .expect("expected workspace");
        let context = CliExecutionContext::from_workspace(&workspace).unwrap();
        let claude = CliToolFactory::new(state.clone()).create("claude").unwrap();
        let permissions = claude
            .runtime_permissions(&context, &updated)
            .await
            .unwrap();
        assert_eq!(permissions.sandbox_mode.as_deref(), Some("workspace-write"));
        assert!(updated
            .engine_metadata
            .as_ref()
            .is_none_or(|value| value.get("sandboxMode").is_none()));
    }

    #[tokio::test]
    async fn set_thread_execution_policy_rejects_claude_danger_full_access() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");

        let error = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            false,
            None,
            true,
            Some("danger-full-access".to_string()),
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .expect_err("expected danger-full-access to be rejected");

        assert!(error.contains("Claude sandbox mode `danger-full-access` is not supported"));
    }

    #[tokio::test]
    async fn set_thread_execution_policy_clears_permission_profile_when_sandbox_changes() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.4");

        let profile = json!({
            "type": "managed",
            "fileSystem": {
                "type": "unrestricted"
            },
            "network": {
                "enabled": true
            }
        });
        let updated = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            false,
            None,
            false,
            None,
            false,
            None,
            true,
            Some(profile),
            false,
            None,
        )
        .await
        .expect("expected permission profile update to succeed");
        let workspace =
            crate::db::workspaces::find_workspace_by_id(&state.db, &thread.workspace_id)
                .unwrap()
                .expect("expected workspace");
        let context = CliExecutionContext::from_workspace(&workspace).unwrap();
        let codex = CliToolFactory::new(state.clone()).create("codex").unwrap();
        let permissions = codex.runtime_permissions(&context, &updated).await.unwrap();
        assert!(permissions.permission_profile.is_some());

        let updated = set_thread_execution_policy_inner(
            &state,
            thread.id.clone(),
            false,
            None,
            true,
            Some("danger-full-access".to_string()),
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .await
        .expect("expected sandbox update to succeed");
        let permissions = codex.runtime_permissions(&context, &updated).await.unwrap();
        assert_eq!(permissions.permission_profile, None);
        assert_eq!(
            permissions.sandbox_mode.as_deref(),
            Some("danger-full-access")
        );
    }

    #[test]
    fn normalize_thread_permission_profile_rejects_incomplete_profiles() {
        let error = normalize_thread_permission_profile(Some(json!({
            "type": "managed",
            "fileSystem": {
                "type": "unrestricted"
            }
        })))
        .expect_err("expected missing network to be rejected");

        assert!(error.contains("network must be an object"));
    }

    #[test]
    fn normalize_thread_approvals_reviewer_rejects_unknown_values() {
        let error = normalize_thread_approvals_reviewer(Some("robot".to_string()))
            .expect_err("expected unknown reviewer to be rejected");

        assert!(error.contains("invalid approvals reviewer `robot`"));
    }

    #[tokio::test]
    async fn set_thread_codex_config_persists_values() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.4");

        let updated = set_thread_codex_config_inner(
            &state,
            thread.id.clone(),
            true,
            Some("Friendly".to_string()),
            true,
            Some("FLEX".to_string()),
            true,
            Some(json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" }
                }
            })),
        )
        .await
        .expect("expected codex config update to succeed");

        let metadata = updated
            .engine_metadata
            .expect("expected engine metadata to be present");
        assert_eq!(metadata.get("personality"), Some(&json!("friendly")));
        assert_eq!(metadata.get("serviceTier"), Some(&json!("flex")));
        assert_eq!(
            metadata.get("outputSchema"),
            Some(&json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" }
                }
            }))
        );
    }

    #[tokio::test]
    async fn set_thread_codex_config_rejects_non_codex_threads() {
        let state = test_app_state();
        let thread = test_thread(&state, "claude", "claude-sonnet-4-6");

        let error = set_thread_codex_config_inner(
            &state,
            thread.id.clone(),
            true,
            Some("friendly".to_string()),
            false,
            None,
            false,
            None,
        )
        .await
        .expect_err("expected non-codex thread to be rejected");

        assert!(error.contains("Codex thread config is only available for Codex threads"));
    }

    #[tokio::test]
    async fn set_thread_opencode_config_persists_agent() {
        let state = test_app_state();
        let thread = test_thread(&state, "opencode", "opencode/big-pickle");

        let updated = set_thread_opencode_config_inner(
            &state,
            thread.id.clone(),
            true,
            Some("Explore_1".to_string()),
        )
        .await
        .expect("expected OpenCode config update to succeed");

        let metadata = updated
            .engine_metadata
            .expect("expected engine metadata to be present");
        assert_eq!(metadata.get("opencodeAgent"), Some(&json!("Explore_1")));
    }

    #[tokio::test]
    async fn set_thread_opencode_config_clears_build_agent() {
        let state = test_app_state();
        let thread = test_thread(&state, "opencode", "opencode/big-pickle");

        let updated = set_thread_opencode_config_inner(
            &state,
            thread.id.clone(),
            true,
            Some("build".to_string()),
        )
        .await
        .expect("expected OpenCode build agent to clear override");

        assert!(updated
            .engine_metadata
            .and_then(|metadata| metadata.get("opencodeAgent").cloned())
            .is_none());
    }

    #[tokio::test]
    async fn set_thread_opencode_config_rejects_non_opencode_threads() {
        let state = test_app_state();
        let thread = test_thread(&state, "codex", "gpt-5.4");

        let error = set_thread_opencode_config_inner(
            &state,
            thread.id.clone(),
            true,
            Some("explore".to_string()),
        )
        .await
        .expect_err("expected non-opencode thread to be rejected");

        assert!(error.contains("OpenCode thread config is only available for OpenCode threads"));
    }
}
