use rusqlite::Connection;

use super::{load_config_dictionary, save_config_dictionary, AppConfig};

/// 创建只包含配置字典表的内存数据库，验证缺失配置不影响其他默认配置。
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

/// 验证缺少聊天输入区高度时保持未配置，由前端使用自然 rows=3 布局。
#[test]
fn missing_chat_input_height_keeps_natural_layout() {
    let connection = memory_config_database();
    let config = load_config_dictionary(&connection).expect("config should load");
    assert_eq!(config.chat_input_height(), None);
}

/// 验证聊天输入区高度的非法 JSON 或零值不会形成可用配置。
#[test]
fn invalid_chat_input_height_is_ignored() {
    let mut connection = memory_config_database();
    connection
        .execute(
            "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
            rusqlite::params!["ui.chat_input_height", "0"],
        )
        .expect("invalid height should be inserted");
    let config = load_config_dictionary(&connection).expect("config should load");
    assert_eq!(config.chat_input_height(), None);
}

/// 验证聊天输入区高度保存后可以从配置字典回读确认。
#[test]
fn chat_input_height_saves_and_roundtrips() {
    let mut connection = memory_config_database();
    let mut config = AppConfig::default();
    config.ui.chat_input_height = Some(180);
    save_config_dictionary(&mut connection, &config).expect("config should save");

    let restored = load_config_dictionary(&connection).expect("config should load");
    assert_eq!(restored.chat_input_height(), Some(180));
}
