use std::path::PathBuf;

use crate::{
    db::{self, Database},
    models::{SshConnectionInput, TrustLevelDto, WorkspaceDto, STATUS_DISABLED},
    workspace_startup::{
        WorkspaceDefaultView, WorkspacePathBase, WorkspaceStartupApplyWhen,
        WorkspaceStartupGroup, WorkspaceStartupPreset, WorkspaceStartupSession,
        WorkspaceStartupSplitNode, WorkspaceTerminalStartupPreset,
    },
};
use uuid::Uuid;

use super::{load_execution_target, load_workspace, normalize_preset_for_workspace};

/// 构造用于工作区预设校验的最小工作区 DTO。
fn workspace_dto(location_kind: &str, root_path: &str) -> WorkspaceDto {
    WorkspaceDto {
        id: "workspace-test-id".to_string(),
        name: "Workspace Test".to_string(),
        root_path: root_path.to_string(),
        location_kind: location_kind.to_string(),
        ssh_connection_id: None,
        connection_display_name: None,
        connection_enabled: None,
        connection_deleted: None,
        connection_status: None,
        trust_level: TrustLevelDto::Standard,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_opened_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

/// 构造不含终端配置的基础启动预设。
fn empty_preset() -> WorkspaceStartupPreset {
    WorkspaceStartupPreset {
        version: 1,
        default_view: WorkspaceDefaultView::Chat,
        split_panel_size: None,
        terminal: None,
    }
}

/// 构造包含一个 session 的启动预设，用于验证远端目录基准规则。
fn session_preset(cwd_base: WorkspacePathBase) -> WorkspaceStartupPreset {
    WorkspaceStartupPreset {
        version: 1,
        default_view: WorkspaceDefaultView::Terminal,
        split_panel_size: None,
        terminal: Some(WorkspaceTerminalStartupPreset {
            apply_when: WorkspaceStartupApplyWhen::NoLiveSessions,
            groups: vec![WorkspaceStartupGroup {
                id: "group-1".to_string(),
                name: "Group 1".to_string(),
                broadcast_on_start: false,
                worktree: None,
                sessions: vec![WorkspaceStartupSession {
                    id: "session-1".to_string(),
                    title: None,
                    cwd: "/__auracoder_remote_only__/session".to_string(),
                    cwd_base: Some(cwd_base),
                    harness_id: None,
                    launch_harness_on_create: None,
                }],
                root: WorkspaceStartupSplitNode::Leaf {
                    session_id: "session-1".to_string(),
                },
            }],
            active_group_id: None,
            focused_session_id: None,
        }),
    }
}

/// 验证远端预设规范化不在本机解析远端根路径，而本机工作区仍要求根路径可解析。
#[test]
fn remote_preset_does_not_resolve_remote_root_on_local_machine() {
    let remote_workspace = workspace_dto("ssh", "/__auracoder_remote_only__/missing");
    let preset = empty_preset();

    // 验证远端预设按远端规则通过，同时本机路径规则仍拒绝不存在的根路径。
    let normalized = normalize_preset_for_workspace(&remote_workspace, preset.clone())
        .expect("remote preset should not resolve its root on the local machine");
    assert_eq!(normalized, preset);

    let local_workspace = workspace_dto("local", "/__auracoder_remote_only__/missing");
    assert!(normalize_preset_for_workspace(&local_workspace, preset).is_err());
}

/// 验证远端 session 拒绝未登记的绝对目录基准并接受工作区基准。
#[test]
fn remote_preset_rejects_unregistered_absolute_session_directory() {
    let remote_workspace = workspace_dto("ssh", "/__auracoder_remote_only__/missing");

    // 验证远端 session 只能使用已登记的工作区目录基准。
    assert!(normalize_preset_for_workspace(
        &remote_workspace,
        session_preset(WorkspacePathBase::Absolute),
    )
    .is_err());
    let registered_preset = session_preset(WorkspacePathBase::Workspace);
    let normalized = normalize_preset_for_workspace(&remote_workspace, registered_preset.clone())
        .expect("workspace based remote session should be accepted");
    assert_eq!(normalized, registered_preset);
}

/// 验证工作区查询先报告缺失记录，并能从临时数据库读取已登记工作区。
#[test]
fn missing_workspace_is_reported_before_preset_processing() {
    let temp_root: PathBuf = std::env::temp_dir()
        .join(format!("auracoder-workspace-service-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&temp_root).expect("failed to create temporary workspace test root");
    let db_path = temp_root.join("workspace.db");
    let db = Database::open(db_path).expect("failed to initialize temporary workspace database");

    // 验证不存在的工作区直接返回查询错误，之后通过真实 Mapper 保存并重读有效工作区。
    let missing_error = load_workspace(&db, "workspace-does-not-exist")
        .expect_err("missing workspace should be rejected before preset processing");
    assert!(missing_error.to_string().contains("工作区不存在"));

    let workspace_root = temp_root.join("project");
    std::fs::create_dir_all(&workspace_root).expect("failed to create temporary workspace root");
    let inserted = db::workspaces::upsert_workspace(
        &db,
        workspace_root
            .to_str()
            .expect("temporary workspace path should be valid UTF-8"),
    )
    .expect("failed to insert temporary workspace");
    let loaded = load_workspace(&db, &inserted.id).expect("inserted workspace should be readable");
    assert_eq!(loaded.id, inserted.id);
    assert_eq!(loaded.root_path, inserted.root_path);

    // 关闭数据库后清理仅由本测试创建的临时目录。
    drop(db);
    std::fs::remove_dir_all(temp_root).expect("failed to remove temporary workspace test root");
}

/// 验证本机与 SSH 执行目标展示数据库记录，并保留已禁用 SSH 连接状态。
#[test]
fn execution_target_display_keeps_disabled_ssh_state() {
    let temp_root: PathBuf = std::env::temp_dir()
        .join(format!("auracoder-execution-target-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&temp_root).expect("failed to create temporary execution target root");
    let db_path = temp_root.join("workspace.db");
    let db = Database::open(db_path).expect("failed to initialize execution target database");

    // 验证没有工作区时显示默认本机目标，登记本机工作区后显示其项目根目录。
    let default_target = load_execution_target(&db, &None)
        .expect("default local execution target should be available");
    assert_eq!(default_target.kind, "local");
    assert_eq!(default_target.project_path, None);

    let local_root = temp_root.join("local-project");
    std::fs::create_dir_all(&local_root).expect("failed to create local project root");
    let local_workspace = db::workspaces::upsert_workspace(
        &db,
        local_root
            .to_str()
            .expect("temporary local project path should be valid UTF-8"),
    )
    .expect("failed to insert local workspace");
    let local_target = load_execution_target(&db, &Some(local_workspace.id.clone()))
        .expect("local workspace target should be available");
    assert_eq!(local_target.kind, "local");
    assert_eq!(
        local_target.project_path.as_deref(),
        Some(local_workspace.root_path.as_str())
    );

    // 通过连接 Mapper 完成远端项目绑定，再禁用连接并读取原展示资料。
    let connection_id = format!("ssh-execution-target-{}", Uuid::new_v4());
    let input = SshConnectionInput {
        display_name: "Execution Target SSH".to_string(),
        host_name: "192.0.2.10".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: None,
    };
    let connection = db::ssh_connections::insert(
        &db,
        &connection_id,
        "manual",
        &input,
        "ssh-ed25519",
        "test-key",
    )
    .expect("failed to insert SSH connection");
    db::ssh_connections::record_test(&db, &connection_id, &connection.updated_at, true, None)
        .expect("failed to record successful SSH connection test");
    let ssh_workspace = db::workspaces::create_ssh_workspace(
        &db,
        &connection_id,
        "Remote Project",
        "/srv/remote-project",
    )
    .expect("failed to insert SSH workspace");
    let disabled_connection = db::ssh_connections::set_enabled(&db, &connection_id, false)
        .expect("failed to disable SSH connection");

    let ssh_target = load_execution_target(&db, &Some(ssh_workspace.id.clone()))
        .expect("disabled SSH workspace should remain displayable");
    assert_eq!(ssh_target.kind, "ssh");
    assert_eq!(ssh_target.connection_id.as_deref(), Some(connection_id.as_str()));
    assert_eq!(ssh_target.display_name, "Execution Target SSH");
    assert_eq!(ssh_target.host_name.as_deref(), Some("192.0.2.10"));
    assert_eq!(ssh_target.port, Some(22));
    assert_eq!(ssh_target.project_path.as_deref(), Some("/srv/remote-project"));
    assert_eq!(
        ssh_target.connection_status.as_deref(),
        Some(disabled_connection.connection_status.as_str())
    );
    assert_eq!(
        ssh_target.connection_status.as_deref(),
        // Some(db::ssh_connections::STATUS_DISABLED)
        Some(STATUS_DISABLED)
    );

    // 关闭数据库后清理仅由本测试创建的临时目录。
    drop(db);
    std::fs::remove_dir_all(temp_root).expect("failed to remove temporary execution target root");
}
