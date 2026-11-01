use super::*;

use rusqlite::Connection;

/// 创建仅含配置表的内存连接，供 Mapper 原始键值读写验证使用。
fn memory_config_database() -> Connection {
    let connection = Connection::open_in_memory().expect("memory database should open");
    connection
        .execute_batch(
            "CREATE TABLE config (
                config_key TEXT PRIMARY KEY NOT NULL,
                config_value TEXT NOT NULL
            )",
        )
        .expect("config table should be created");
    connection
}

/// 验证 Mapper 保持配置值不透明，并在 upsert 时保留未触及的未知键。
#[test]
fn entries_are_opaque_and_unknown_keys_survive_upsert() {
    let mut connection = memory_config_database();
    save_entries_on_connection(
        &mut connection,
        &[
            ("future.key".to_string(), "not-json".to_string()),
            ("known.key".to_string(), "old-value".to_string()),
        ],
    )
    .expect("initial config entries should save");

    save_entries_on_connection(
        &mut connection,
        &[("known.key".to_string(), "new-value".to_string())],
    )
    .expect("known config entry should update");

    let entries =
        load_entries_from_connection(&connection).expect("config entries should load");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries
            .iter()
            .find(|(key, _)| key == "future.key")
            .map(|(_, value)| value.as_str()),
        Some("not-json")
    );
    assert_eq!(
        entries
            .iter()
            .find(|(key, _)| key == "known.key")
            .map(|(_, value)| value.as_str()),
        Some("new-value")
    );
}

/// 验证任一键 upsert 失败时 Mapper 回滚同一批次的全部写入。
#[test]
fn failed_upsert_rolls_back_the_whole_batch() {
    let mut connection = memory_config_database();
    connection
        .execute(
            "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
            rusqlite::params!["existing", "old"],
        )
        .expect("existing config entry should insert");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_config_insert
             BEFORE INSERT ON config
             WHEN NEW.config_key = 'reject'
             BEGIN
                 SELECT RAISE(ABORT, 'expected config write failure');
             END;",
        )
        .expect("config write trigger should be created");

    let result = save_entries_on_connection(
        &mut connection,
        &[
            ("existing".to_string(), "new".to_string()),
            ("reject".to_string(), "bad".to_string()),
        ],
    );
    assert!(result.is_err());

    let existing_value: String = connection
        .query_row(
            "SELECT config_value FROM config WHERE config_key = 'existing'",
            [],
            |row| row.get(0),
        )
        .expect("existing config entry should remain readable");
    assert_eq!(existing_value, "old");
    let rejected_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM config WHERE config_key = 'reject'",
            [],
            |row| row.get(0),
        )
        .expect("rejected config entry count should be readable");
    assert_eq!(rejected_count, 0);
}