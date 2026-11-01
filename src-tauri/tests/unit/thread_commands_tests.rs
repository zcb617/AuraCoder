
use std::{fs, sync::Arc};

use super::*;
use serde_json::{json, Value};
use crate::{db, engines::{CodexRemoteThreadSummary, OpenCodeRemoteSessionSummary}, models::ThreadStatusDto};
use crate::cli_tools::codex::thread_business::{merge_codex_runtime_metadata, map_codex_thread_status_to_local, build_codex_remote_thread_title, build_codex_remote_thread_metadata, codex_remote_thread_timestamp_to_rfc3339, clone_codex_branch_metadata, should_clone_local_branch_history, normalize_thread_personality, normalize_thread_service_tier, normalize_thread_output_schema, codex_remote_thread_belongs_to_workspace};
use crate::cli_tools::opencode::thread_business::{build_opencode_remote_session_title, build_opencode_remote_session_metadata, normalize_thread_opencode_agent};
use crate::cli_tools::mcp::thread_business::{validate_model_for_engine_from_catalog, validate_reasoning_effort_from_catalog};

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
        context_current_tokens: None,
        context_max_tokens: None,
        context_usage_updated_at: None,
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
async fn archive_thread_locally_inner(state: &AppState, id: &str) -> Result<(), String> { ThreadService::new(state.clone()).archive_thread_locally(id).await }
#[allow(clippy::too_many_arguments)]
async fn create_thread_inner(state: &AppState, workspace_id: String, engine_id: String, model_id: String, title: String, effort: Option<String>, tier: Option<String>, preset: Option<String>) -> Result<ThreadDto, String> {
    let cli = CliToolFactory::new(state.clone()).create(&engine_id).map_err(err_to_string)?;
    let base = crate::cli_tools::BaseCliMcp::new(state.clone(), crate::cli_tools::CliMcpRuntime { cli_id: engine_id.clone(), location: crate::cli_tools::CliLocationKind::Local });
    base.create_thread_record_inner(cli.as_ref(), &engine_id, workspace_id, model_id, title, effort, tier, preset).await
}
#[allow(clippy::too_many_arguments)]
async fn create_codex_branch_thread(state: &AppState, source: &ThreadDto, id: &str, model: &str, title: Option<&str>, preview: Option<&str>, status: Option<&str>, flags: &[String], turns: Option<u32>) -> Result<ThreadDto, String> {
    crate::cli_tools::codex::CodexCli::new(state.clone()).thread_create_branch(source, id, model, title, preview, status, flags, turns).await
}
#[allow(clippy::too_many_arguments)]
async fn set_thread_codex_config_inner(state: &AppState, id: String, up: bool, p: Option<String>, ut: bool, t: Option<String>, us: bool, s: Option<Value>) -> Result<ThreadDto, String> {
    CliToolFactory::new(state.clone()).create("codex").map_err(err_to_string)?.update_codex_thread_config(id, up, p, ut, t, us, s).await.map_err(err_to_string)
}
async fn set_thread_opencode_config_inner(state: &AppState, id: String, update: bool, agent: Option<String>) -> Result<ThreadDto, String> {
    CliToolFactory::new(state.clone()).create("opencode").map_err(err_to_string)?.update_opencode_thread_config(id, update, agent).await.map_err(err_to_string)
}
