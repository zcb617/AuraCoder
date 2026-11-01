use std::path::PathBuf;

use super::{can_register_remote_cli, import_config_hosts, validate_input};
use crate::{
    db::{self, Database},
    models::SshConnectionInput,
    ssh::config::ParsedHost,
};
use uuid::Uuid;

/// 创建具有唯一临时数据库路径的 SSH 服务测试数据库。
fn create_test_database() -> (Database, PathBuf) {
    let path = std::env::temp_dir().join(format!("auracoder-ssh-service-{}.db", Uuid::new_v4()));
    let db = Database::open(path.clone()).expect("failed to initialize test database");
    (db, path)
}

/// 构造无需网络连接的手动 SSH 输入样例。
fn manual_input() -> SshConnectionInput {
    SshConnectionInput {
        display_name: "Test SSH".to_string(),
        host_name: "192.0.2.10".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        host_key: "ssh-ed25519 AQID test".to_string(),
        config_alias: None,
    }
}

/// 验证空必填、端口零和缺少 Host Key 的手动连接输入会在写库前被拒绝。
#[test]
fn invalid_connection_input_is_rejected_before_write() {
    let mut input = manual_input();
    input.display_name.clear();
    assert!(validate_input(&input, true).is_err());

    let mut input = manual_input();
    input.port = 0;
    assert!(validate_input(&input, true).is_err());

    let mut input = manual_input();
    input.host_key.clear();
    assert!(validate_input(&input, true).is_err());
}

/// 验证导入已删除 SSH alias 时恢复原 ID 并保留原 disabled 状态。
#[test]
fn imported_connection_restore_keeps_id_and_enabled() {
    let (db, path) = create_test_database();
    let mut original_input = manual_input();
    original_input.config_alias = Some("config-host".to_string());
    let original = db::ssh_connections::insert(
        &db,
        "ssh-import-restore",
        "ssh_config",
        &original_input,
        "config",
        "",
    )
    .expect("failed to insert original connection");
    db::ssh_connections::set_enabled(&db, &original.id, false)
        .expect("failed to disable original connection");
    db::ssh_connections::soft_delete(&db, &original.id).expect("failed to delete original");

    let results = import_config_hosts(
        &db,
        vec![ParsedHost {
            alias: "config-host".to_string(),
            host_name: "updated.example.com".to_string(),
            user: "updated-user".to_string(),
            port: 2222,
            identity_file: Some("~/.ssh/id_updated".to_string()),
        }],
    )
    .expect("failed to process imported SSH configuration");
    assert_eq!(results.len(), 1);
    assert!(results[0].restored);
    let restored = results[0]
        .connection
        .as_ref()
        .expect("restored connection should be returned");
    assert_eq!(restored.id, original.id);
    assert!(!restored.enabled);
    assert_eq!(restored.host_name, "updated.example.com");
    assert_eq!(restored.user, "updated-user");

    let _ = std::fs::remove_file(path);
}

/// 验证单条非法导入保留错误结果，后续可写配置仍保存并返回成功。
#[test]
fn one_import_failure_preserves_other_results() {
    let (db, path) = create_test_database();
    let results = import_config_hosts(
        &db,
        vec![
            ParsedHost {
                alias: "invalid-host".to_string(),
                host_name: String::new(),
                user: "tester".to_string(),
                port: 22,
                identity_file: None,
            },
            ParsedHost {
                alias: "valid-host".to_string(),
                host_name: "valid.example.com".to_string(),
                user: "tester".to_string(),
                port: 22,
                identity_file: None,
            },
        ],
    )
    .expect("failed to process SSH configuration batch");
    assert_eq!(results.len(), 2);
    assert!(results[0].connection.is_none());
    assert!(results[0].error.is_some());
    assert!(results[1].connection.is_some());
    assert_eq!(
        db::ssh_connections::list(&db, false)
            .expect("failed to list saved connections")
            .len(),
        1
    );

    let _ = std::fs::remove_file(path);
}

/// 验证配置更新或软删除后拒绝旧检测，并且禁用连接检测成功也不允许注册。
#[test]
fn stale_or_deleted_probe_results_are_not_accepted() {
    let (db, path) = create_test_database();
    let input = manual_input();
    let original = db::ssh_connections::insert(
        &db,
        "ssh-probe-stale",
        "manual",
        &input,
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to insert probe connection");
    let mut updated_input = input.clone();
    updated_input.host_name = "updated.example.com".to_string();
    db::ssh_connections::update(
        &db,
        &original.id,
        &updated_input,
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to update probe connection");
    assert!(!db::ssh_connections::record_test_if_current(
        &db,
        &original.id,
        &original.updated_at,
        true,
        None,
    )
    .expect("stale probe result should not fail"));

    let current = db::ssh_connections::find(&db, &original.id)
        .expect("failed to reload updated connection")
        .expect("updated connection should exist");
    db::ssh_connections::soft_delete(&db, &original.id).expect("failed to delete connection");
    assert!(!db::ssh_connections::record_test_if_current(
        &db,
        &original.id,
        &current.dto.updated_at,
        true,
        None,
    )
    .expect("deleted probe result should not fail"));

    let disabled = db::ssh_connections::insert(
        &db,
        "ssh-probe-disabled",
        "manual",
        &input,
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to insert disabled connection");
    let disabled = db::ssh_connections::set_enabled(&db, &disabled.id, false)
        .expect("failed to disable connection");
    assert!(db::ssh_connections::record_test_if_current(
        &db,
        &disabled.id,
        &disabled.updated_at,
        true,
        None,
    )
    .expect("disabled probe result should be saved"));
    let after_probe = db::ssh_connections::find(&db, &disabled.id)
        .expect("failed to reload disabled connection")
        .expect("disabled connection should exist");
    assert!(!can_register_remote_cli(
        true,
        &disabled.updated_at,
        &after_probe,
    ));
    assert_eq!(after_probe.dto.connection_status, db::ssh_connections::STATUS_DISABLED);

    let _ = std::fs::remove_file(path);
}
