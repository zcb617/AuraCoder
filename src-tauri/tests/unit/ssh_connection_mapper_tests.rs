use super::*;
use uuid::Uuid;

/// 验证监控状态只接受当前配置版本，并持久化连接生命周期状态。
#[test]
fn monitored_status_is_versioned_and_lifecycle_states_are_persisted() {
    let path = std::env::temp_dir().join(format!("auracoder-ssh-status-{}.db", Uuid::new_v4()));
    let db = Database::open(path.clone()).expect("failed to initialize test database");
    let input = SshConnectionInput {
        display_name: "Test SSH".to_string(),
        host_name: "192.0.2.10".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: None,
    };
    let dto = insert(
        &db,
        "ssh-status-test",
        "manual",
        &input,
        "ssh-ed25519",
        "test-key",
    )
    .expect("failed to insert test connection");
    assert_eq!(dto.connection_status, STATUS_UNKNOWN);
    assert!(
        !set_status_if_current(&db, &dto.id, "stale-version", STATUS_OK, None,)
            .expect("stale status update should not fail")
    );

    let record = find(&db, &dto.id)
        .expect("failed to load test connection")
        .expect("test connection should exist");
    assert!(set_status_if_current(
        &db,
        &dto.id,
        &record.dto.updated_at,
        STATUS_CONNECTING,
        None,
    )
    .expect("failed to persist connecting status"));
    assert_eq!(
        find(&db, &dto.id)
            .expect("failed to reload test connection")
            .expect("test connection should exist")
            .dto
            .connection_status,
        STATUS_CONNECTING
    );

    let version = find(&db, &dto.id)
        .expect("failed to reload test connection")
        .expect("test connection should exist")
        .dto
        .updated_at;
    assert!(
        set_status_if_current(&db, &dto.id, &version, STATUS_OK, None,)
            .expect("failed to persist connected status")
    );
    assert_eq!(
        find(&db, &dto.id)
            .expect("failed to reload test connection")
            .expect("test connection should exist")
            .dto
            .connection_status,
        STATUS_OK
    );

    let disabled = set_enabled(&db, &dto.id, false).expect("failed to disable test connection");
    assert_eq!(disabled.connection_status, STATUS_DISABLED);
    soft_delete(&db, &dto.id).expect("failed to delete test connection");
    let restored = restore(&db, &dto.id).expect("failed to restore test connection");
    assert_eq!(restored.connection_status, STATUS_DISABLED);

    let _ = std::fs::remove_file(path);
}

/// 验证导入恢复在唯一键冲突时完整回滚，并在成功时保留连接身份和启用状态。
#[test]
fn restore_with_input_is_atomic() {
    let path = std::env::temp_dir().join(format!("auracoder-ssh-restore-{}.db", Uuid::new_v4()));
    let db = Database::open(path.clone()).expect("failed to initialize test database");
    let original_input = SshConnectionInput {
        display_name: "Original SSH".to_string(),
        host_name: "old.example.com".to_string(),
        user: "old-user".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: Some("shared-alias".to_string()),
    };
    let original = insert(
        &db,
        "ssh-restore-conflict-original",
        "ssh_config",
        &original_input,
        "config",
        "",
    )
    .expect("failed to insert original connection");
    soft_delete(&db, &original.id).expect("failed to delete original connection");

    let conflicting_input = SshConnectionInput {
        display_name: "Current SSH".to_string(),
        host_name: "current.example.com".to_string(),
        user: "current-user".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: Some("shared-alias".to_string()),
    };
    insert(
        &db,
        "ssh-restore-conflict-active",
        "ssh_config",
        &conflicting_input,
        "config",
        "",
    )
    .expect("failed to insert conflicting active connection");
    let conflicting_restore = SshConnectionInput {
        display_name: "Changed SSH".to_string(),
        host_name: "changed.example.com".to_string(),
        user: "changed-user".to_string(),
        port: 2200,
        identity_file: Some("~/.ssh/new_key".to_string()),
        host_key: String::new(),
        config_alias: Some("shared-alias".to_string()),
    };
    assert!(restore_with_input(
        &db,
        &original.id,
        &conflicting_restore,
        "config",
        "",
    )
    .is_err());
    let unchanged = find(&db, &original.id)
        .expect("failed to reload original connection")
        .expect("original connection should remain");
    assert!(unchanged.dto.deleted_at.is_some());
    assert_eq!(unchanged.dto.host_name, "old.example.com");
    assert_eq!(unchanged.dto.user, "old-user");
    assert_eq!(unchanged.dto.port, 22);

    let success_input = SshConnectionInput {
        display_name: "Restorable SSH".to_string(),
        host_name: "before.example.com".to_string(),
        user: "before-user".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: Some("restore-success".to_string()),
    };
    let success_original = insert(
        &db,
        "ssh-restore-success",
        "ssh_config",
        &success_input,
        "config",
        "",
    )
    .expect("failed to insert restorable connection");
    set_enabled(&db, &success_original.id, false).expect("failed to disable connection");
    soft_delete(&db, &success_original.id).expect("failed to delete restorable connection");
    let updated_input = SshConnectionInput {
        display_name: "Restorable SSH".to_string(),
        host_name: "after.example.com".to_string(),
        user: "after-user".to_string(),
        port: 2022,
        identity_file: Some("~/.ssh/restored_key".to_string()),
        host_key: String::new(),
        config_alias: Some("restore-success".to_string()),
    };
    let restored = restore_with_input(
        &db,
        &success_original.id,
        &updated_input,
        "config",
        "",
    )
    .expect("failed to restore connection with new SSH parameters");
    assert_eq!(restored.id, success_original.id);
    assert!(!restored.enabled);
    assert_eq!(restored.source_kind, "ssh_config");
    assert_eq!(restored.host_name, "after.example.com");
    assert_eq!(restored.user, "after-user");
    assert_eq!(restored.port, 2022);

    let _ = std::fs::remove_file(path);
}

/// 验证检测结果拒绝过期配置，并在禁用或删除时保持正确生命周期状态。
#[test]
fn probe_write_reports_stale_version_and_preserves_disabled_state() {
    let path = std::env::temp_dir().join(format!("auracoder-ssh-probe-{}.db", Uuid::new_v4()));
    let db = Database::open(path.clone()).expect("failed to initialize test database");
    let input = SshConnectionInput {
        display_name: "Probe SSH".to_string(),
        host_name: "192.0.2.10".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        host_key: String::new(),
        config_alias: None,
    };
    let dto = insert(
        &db,
        "ssh-probe-test",
        "manual",
        &input,
        "ssh-ed25519",
        "test-key",
    )
    .expect("failed to insert test connection");
    assert!(!record_test_if_current(
        &db,
        &dto.id,
        "stale-version",
        true,
        None,
    )
    .expect("stale probe write should not fail"));

    let disabled = set_enabled(&db, &dto.id, false).expect("failed to disable test connection");
    assert!(record_test_if_current(
        &db,
        &dto.id,
        &disabled.updated_at,
        true,
        None,
    )
    .expect("disabled probe write should succeed"));
    let after_probe = find(&db, &dto.id)
        .expect("failed to reload disabled connection")
        .expect("disabled connection should exist");
    assert_eq!(after_probe.dto.connection_status, STATUS_DISABLED);

    soft_delete(&db, &dto.id).expect("failed to delete test connection");
    let deleted = find(&db, &dto.id)
        .expect("failed to reload deleted connection")
        .expect("deleted connection should exist");
    assert!(!record_test_if_current(
        &db,
        &dto.id,
        &deleted.dto.updated_at,
        true,
        None,
    )
    .expect("deleted probe write should not fail"));

    let _ = std::fs::remove_file(path);
}
