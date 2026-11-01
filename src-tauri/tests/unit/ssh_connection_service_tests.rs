use std::path::PathBuf;

use super::{
    can_register_remote_cli, import_config_hosts, load_usable_ssh_connection,
    parse_remote_directories, save_directory_probe_result, validate_input, validate_remote_path,
};
use crate::{
    db::{self, Database},
    models::{SshConnectionInput, SshConnectionTestDto},
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

/// 构造无需网络连接的成功 SSH 目录探测结果。
fn successful_directory_probe(connection_id: &str) -> SshConnectionTestDto {
    SshConnectionTestDto {
        connection_id: connection_id.to_string(),
        ok: true,
        os: None,
        home: Some("/home/tester".to_string()),
        shell: None,
        git_version: None,
        cli_versions: std::collections::BTreeMap::new(),
        error: None,
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

/// 验证远端路径规则、目录协议解析、权限标志和名称排序保持一致。
#[test]
fn directory_paths_and_output_preserve_validation_and_flags() {
    // 验证绝对路径接受、相对或含控制字符路径拒绝，以及解析只保留合法目录行。
    assert!(validate_remote_path("/").is_ok());
    assert!(validate_remote_path("/远端 目录/引号'内").is_ok());
    for path in [
        "relative/path",
        "ssh://host/path",
        "/remote\0path",
        "/remote\rpath",
        "/remote\npath",
        "/remote\tpath",
    ] {
        assert!(validate_remote_path(path).is_err());
    }

    let output = concat!(
        "remote command noise\n",
        "__PANES_DIR__relative/ignored\t1\t1\n",
        "__PANES_DIR__/work/zeta\t1\t0\n",
        "__PANES_DIR__/work/Alpha\t0\t1\n",
    );
    let directories = parse_remote_directories(output).expect("directory output should parse");
    assert_eq!(directories.len(), 2);
    assert_eq!(directories[0].name, "Alpha");
    assert_eq!(directories[0].path, "/work/Alpha");
    assert!(!directories[0].readable);
    assert!(directories[0].enterable);
    assert_eq!(directories[1].name, "zeta");
    assert!(directories[1].readable);
    assert!(!directories[1].enterable);
}

/// 验证目录连接读取会拒绝未知、禁用和已删除连接。
#[test]
fn directory_connection_rejects_disabled_or_deleted_record() {
    let (db, path) = create_test_database();

    // 验证目录入口只接受存在、启用且未删除的 SSH 连接记录。
    assert!(load_usable_ssh_connection(&db, "missing-connection").is_err());
    let inserted = db::ssh_connections::insert(
        &db,
        "ssh-directory-usable",
        "manual",
        &manual_input(),
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to insert directory connection");
    assert!(load_usable_ssh_connection(&db, &inserted.id).is_ok());

    db::ssh_connections::set_enabled(&db, &inserted.id, false)
        .expect("failed to disable directory connection");
    assert!(load_usable_ssh_connection(&db, &inserted.id).is_err());
    db::ssh_connections::set_enabled(&db, &inserted.id, true)
        .expect("failed to re-enable directory connection");
    db::ssh_connections::soft_delete(&db, &inserted.id)
        .expect("failed to delete directory connection");
    assert!(load_usable_ssh_connection(&db, &inserted.id).is_err());

    drop(db);
    std::fs::remove_file(path).expect("failed to remove temporary SSH service database");
}

/// 验证旧配置版本或软删除后的 SSH 目录探测结果不能保存。
#[test]
fn directory_probe_rejects_changed_or_deleted_connection() {
    let (db, path) = create_test_database();

    // 保存一次有效探测，再确认更新或删除后的旧配置不能覆盖当前连接状态。
    let input = manual_input();
    let inserted = db::ssh_connections::insert(
        &db,
        "ssh-directory-probe-stale",
        "manual",
        &input,
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to insert directory probe connection");
    let original = db::ssh_connections::find(&db, &inserted.id)
        .expect("failed to load original directory probe connection")
        .expect("directory probe connection should exist");
    let probe = successful_directory_probe(&original.dto.id);
    save_directory_probe_result(&db, &original, &probe)
        .expect("current directory probe should be saved");

    let mut updated_input = input;
    updated_input.host_name = "updated.example.com".to_string();
    db::ssh_connections::update(
        &db,
        &original.dto.id,
        &updated_input,
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to update directory probe connection");
    let updated = db::ssh_connections::find(&db, &original.dto.id)
        .expect("failed to load updated directory probe connection")
        .expect("updated directory probe connection should exist");
    assert!(save_directory_probe_result(&db, &original, &probe).is_err());
    let after_stale_probe = db::ssh_connections::find(&db, &original.dto.id)
        .expect("failed to reload connection after stale probe")
        .expect("directory probe connection should remain available");
    assert_eq!(after_stale_probe.dto.host_name, "updated.example.com");
    assert_eq!(
        after_stale_probe.dto.connection_status,
        updated.dto.connection_status
    );
    assert_eq!(
        after_stale_probe.dto.last_connected_at,
        updated.dto.last_connected_at
    );

    db::ssh_connections::soft_delete(&db, &updated.dto.id)
        .expect("failed to delete updated directory probe connection");
    assert!(save_directory_probe_result(&db, &updated, &probe).is_err());
    let deleted = db::ssh_connections::find(&db, &updated.dto.id)
        .expect("failed to reload deleted directory probe connection")
        .expect("deleted directory probe connection should remain recorded");
    assert!(deleted.dto.deleted_at.is_some());

    drop(db);
    std::fs::remove_file(path).expect("failed to remove temporary SSH service database");
}

/// 验证禁用连接的探测结果不会使目录连接恢复为可用状态。
#[test]
fn directory_probe_does_not_accept_disabled_connection() {
    let (db, path) = create_test_database();

    // 验证条件保存完成后仍检查启用状态，禁用状态和状态码保持不变。
    let inserted = db::ssh_connections::insert(
        &db,
        "ssh-directory-probe-disabled",
        "manual",
        &manual_input(),
        "ssh-ed25519",
        "AQID",
    )
    .expect("failed to insert disabled directory probe connection");
    db::ssh_connections::set_enabled(&db, &inserted.id, false)
        .expect("failed to disable directory probe connection");
    let disabled = db::ssh_connections::find(&db, &inserted.id)
        .expect("failed to load disabled directory probe connection")
        .expect("disabled directory probe connection should exist");
    let probe = successful_directory_probe(&disabled.dto.id);

    assert!(save_directory_probe_result(&db, &disabled, &probe).is_err());
    let after_probe = db::ssh_connections::find(&db, &disabled.dto.id)
        .expect("failed to reload disabled connection after probe")
        .expect("disabled directory probe connection should remain recorded");
    assert!(!after_probe.dto.enabled);
    assert_eq!(
        after_probe.dto.connection_status,
        db::ssh_connections::STATUS_DISABLED
    );

    drop(db);
    std::fs::remove_file(path).expect("failed to remove temporary SSH service database");
}
