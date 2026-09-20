use rusqlite::Connection;

use super::{MIGRATIONS, SUPPORTED_DATABASE_VERSION};

/// 验证数据库从 113 版本执行 114 迁移后记录聊天输入区配置版本。
#[test]
fn migration_113_to_114_updates_schema_version() {
    let connection = Connection::open_in_memory().expect("memory database should open");
    connection
        .execute_batch(
            "CREATE TABLE schema_version (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL,
                migration_file TEXT NOT NULL,
                applied_at TEXT NOT NULL
            );
            INSERT INTO schema_version(id, version, migration_file, applied_at)
            VALUES (1, 113, '113.sql', '2026-01-01 00:00:00');",
        )
        .expect("schema version should be created");

    connection
        .execute_batch(include_str!("../../src/db/migrations/114.sql"))
        .expect("114 migration should apply");

    let version: u64 = connection
        .query_row("SELECT version FROM schema_version WHERE id = 1", [], |row| row.get(0))
        .expect("schema version should be readable");
    let migration_file: String = connection
        .query_row(
            "SELECT migration_file FROM schema_version WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("migration file should be readable");
    assert_eq!(version, 114);
    assert_eq!(migration_file, "114.sql");
}

/// 验证 114 已是唯一最新目标版本，迁移清单不会重复注册该版本。
#[test]
fn migration_114_is_latest_and_registered_once() {
    assert_eq!(SUPPORTED_DATABASE_VERSION, 114);
    assert_eq!(MIGRATIONS.iter().filter(|migration| migration.version == 114).count(), 1);
    assert_eq!(MIGRATIONS.last().map(|migration| migration.version), Some(114));
}
