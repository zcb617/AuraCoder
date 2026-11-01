use super::*;
use rusqlite::Connection;
use crate::config::app_config::{
    AppConfig, ComputerControlAuthorizationConfig, RemoteAccessConfig, RemoteDeviceConfig,
};
use crate::db::app_config as config_mapper;

    /// 创建与 107.sql 相同结构的内存配置字典，避免测试修改运行时全局路径。
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
/// 配置字典中允许由 AppConfig 读写的固定键集合，未知键由数据库保留。
#[cfg(test)]
const CONFIG_KEYS: [&str; 37] = [
    "general.theme",
    "general.default_engine",
    "general.default_model",
    "general.locale",
    "general.terminal_accelerated_rendering",
    "general.terminal_font_size",
    "general.chat_notifications",
    "general.terminal_notifications",
    "general.notification_sound",
    "general.default_autonomy_preset",
    "general.default_file_open_target",
    "ui.sidebar_width",
    "ui.git_panel_width",
    "ui.font_size",
    "ui.display_scale",
    "ui.transcript_width",
    "ui.chat_input_height",
    "debug.persist_engine_event_logs",
    "debug.max_action_output_chars",
    "power.keep_awake_enabled",
    "power.prevent_display_sleep",
    "power.prevent_screen_saver",
    "power.ac_only_mode",
    "power.battery_threshold",
    "power.session_duration_secs",
    "power.prevent_closed_display_sleep",
    "computer_control.enabled",
    "computer_control.persistent_authorizations",
    "claude_code.session_mode",
    "remote_access.enabled",
    "remote_access.endpoint",
    "remote_access.tunnel_id",
    "remote_access.credential",
    "remote_access.devices",
    "remote_access.device_credential",
    "harnesses.launch_args",
    "general.gpu_acceleration_enabled",
];
/// 从测试连接读取原始配置项并调用生产配置解码器。
fn load_config_dictionary(connection: &Connection) -> anyhow::Result<AppConfig> {
    config_mapper::load_entries_from_connection(connection).map(decode_config_dictionary)
}

/// 通过生产配置编码器和 Mapper 事务写入测试连接。
fn save_config_dictionary(
    connection: &mut Connection,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let entries = encode_config_dictionary(config)?;
    config_mapper::save_entries_on_connection(connection, &entries)
}
    /// 验证配置字典可以完整往返所有配置组。
#[test]
    fn dictionary_roundtrip_preserves_all_config_groups() {
        let mut config = AppConfig::default();
        config.general.theme = "light".to_string();
        config.general.locale = Some("zh-CN".to_string());
        config.general.terminal_font_size = Some(18);
        config.ui.sidebar_width = 333;
        config.ui.transcript_width = Some("narrow".to_string());
        config.ui.chat_input_height = Some(180);
        config.debug.max_action_output_chars = 3210;
        config.power.ac_only_mode = true;
        config.power.battery_threshold = Some(22);
        config.computer_control.enabled = true;
        config.computer_control.persistent_authorizations.push(
            ComputerControlAuthorizationConfig {
                request_id: "request-1".to_string(),
                target_key: "application:editor".to_string(),
                agent: "codex".to_string(),
                tool: "launch_app".to_string(),
                call_id: "call-1".to_string(),
                application: "editor".to_string(),
                operation: "input".to_string(),
                scope: "application".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
            },
        );
        config.remote_access.enabled = true;
        config.remote_access.endpoint = "wss://example.invalid/ws/tunnel".to_string();
        config.remote_access.devices.push(RemoteDeviceConfig {
            id: "device-1".to_string(),
            name: "phone".to_string(),
            credential: "device-secret".to_string(),
            paired_at: "2026-01-01T00:00:00Z".to_string(),
            last_connected_at: "2026-01-02T00:00:00Z".to_string(),
        });
        config
            .harnesses
            .launch_args
            .insert("codex".to_string(), "--yolo".to_string());

        let mut connection = memory_config_database();
        save_config_dictionary(&mut connection, &config).expect("config should save");
        let restored = load_config_dictionary(&connection).expect("config should load");

        assert_eq!(CONFIG_KEYS.len(), 37);
        assert_eq!(config.general.theme, restored.general.theme);
        assert_eq!(config.general.locale, restored.general.locale);
        assert_eq!(config.ui.sidebar_width, restored.ui.sidebar_width);
        assert_eq!(config.ui.transcript_width, restored.ui.transcript_width);
        assert_eq!(config.ui.chat_input_height, restored.ui.chat_input_height);
        assert_eq!(
            config.debug.max_action_output_chars,
            restored.debug.max_action_output_chars
        );
        assert_eq!(
            config.power.battery_threshold,
            restored.power.battery_threshold
        );
        assert_eq!(
            config.computer_control.enabled,
            restored.computer_control.enabled
        );
        assert_eq!(
            config.computer_control.persistent_authorizations,
            restored.computer_control.persistent_authorizations
        );
        assert_eq!(config.remote_access, restored.remote_access);
        assert_eq!(config.harnesses.launch_args, restored.harnesses.launch_args);
        let row_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM config", [], |row| row.get(0))
            .expect("config row count should be readable");
        assert_eq!(row_count, 37);
    }

    /// 验证坏 JSON 只影响对应配置字段的默认回退。
#[test]
    fn invalid_dictionary_value_falls_back_only_for_that_key() {
        let connection = memory_config_database();
        connection
            .execute(
                "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
                rusqlite::params!["general.theme", "not-json"],
            )
            .expect("invalid value should be inserted");
        connection
            .execute(
                "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
                rusqlite::params!["general.default_model", "\"custom-model\""],
            )
            .expect("valid value should be inserted");

        let loaded = load_config_dictionary(&connection).expect("config should load");
        assert_eq!(loaded.general.theme, AppConfig::default().general.theme);
        assert_eq!(loaded.general.default_model, "custom-model");
    }

    /// 验证缺失字段使用默认值且保存时保留未知键。
#[test]
    fn missing_and_unknown_dictionary_keys_are_safe() {
        let mut connection = memory_config_database();
        connection
            .execute(
                "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
                rusqlite::params!["future.key", "true"],
            )
            .expect("unknown value should be inserted");

        let loaded = load_config_dictionary(&connection).expect("config should load");
        assert_eq!(loaded.general.theme, AppConfig::default().general.theme);
        save_config_dictionary(&mut connection, &loaded).expect("config should save");
        let unknown_value: String = connection
            .query_row(
                "SELECT config_value FROM config WHERE config_key = 'future.key'",
                [],
                |row| row.get(0),
            )
            .expect("unknown key should remain");
        assert_eq!(unknown_value, "true");
    }

    /// 验证持久化的旧远程地址按规则规范化并保存。
#[test]
    fn persisted_legacy_endpoint_is_normalized() {
        let mut connection = memory_config_database();
        connection
            .execute(
                "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)",
                rusqlite::params![
                    "remote_access.endpoint",
                    "\"wss://panes.jxrjkf.cn/ws/tunnel\""
                ],
            )
            .expect("legacy endpoint should be inserted");

        let mut loaded = load_config_dictionary(&connection).expect("config should load");
        assert!(normalize_remote_endpoint(&mut loaded));
        save_config_dictionary(&mut connection, &loaded).expect("normalized config should save");
        assert_eq!(
            loaded.remote_access.endpoint,
            "wss://auracoder.jxrjkf.cn/ws/tunnel"
        );
        let persisted: String = connection
            .query_row(
                "SELECT config_value FROM config WHERE config_key = 'remote_access.endpoint'",
                [],
                |row| row.get(0),
            )
            .expect("normalized endpoint should be readable");
        assert_eq!(persisted, "\"wss://auracoder.jxrjkf.cn/ws/tunnel\"");
    }
/// 验证远程身份在首次生成和主动重置时保持原有清理规则。
#[test]
fn remote_identity_is_generated_and_rotates() {
    let mut config = RemoteAccessConfig::default();
    assert!(AppConfigService::ensure_remote_identity(&mut config));
    assert!(config.tunnel_id.starts_with("auracoder_"));
    assert!(config.credential.len() >= 64);
    let first_id = config.tunnel_id.clone();
    assert!(!AppConfigService::ensure_remote_identity(&mut config));
    config.device_credential = "paired-device".to_string();
    AppConfigService::regenerate_remote_identity(&mut config);
    assert_ne!(config.tunnel_id, first_id);
    assert!(config.device_credential.is_empty());
}
/// 创建仅含配置字典表的唯一临时数据库，供配置服务核心函数隔离读写。
fn temporary_config_database() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "auracoder-app-config-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).expect("temporary config directory should be created");
    let path = directory.join("workspaces.db");
    let connection = Connection::open(&path).expect("temporary config database should open");
    connection
        .execute_batch(
            "CREATE TABLE config (
                config_key TEXT PRIMARY KEY NOT NULL,
                config_value TEXT NOT NULL
            )",
        )
        .expect("config table should be created");
    drop(connection);
    path
}
/// 验证空配置表会初始化 37 个字段，而未知键单独存在时不触发初始化。
#[test]
fn empty_and_unknown_only_dictionaries_preserve_initialization_rules() {
    let empty_path = temporary_config_database();
    let empty_config =
        load_or_create_unlocked(&empty_path).expect("empty config dictionary should initialize");
    assert_eq!(empty_config.general.theme, AppConfig::default().general.theme);
    assert_eq!(config_mapper::load_entries(&empty_path).expect("entries should load").len(), 37);

    let unknown_path = temporary_config_database();
    let mut connection =
        Connection::open(&unknown_path).expect("unknown-only config database should open");
    config_mapper::save_entries_on_connection(
        &mut connection,
        &[("future.key".to_string(), "true".to_string())],
    )
    .expect("unknown config value should save");
    drop(connection);

    let unknown_config = load_or_create_unlocked(&unknown_path)
        .expect("unknown-only config dictionary should load");
    assert_eq!(unknown_config.general.theme, AppConfig::default().general.theme);
    let entries = config_mapper::load_entries(&unknown_path).expect("entries should load");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], ("future.key".to_string(), "true".to_string()));
}
/// 验证 mutate 保存最新字段、保留未知键并在变更失败时不保存该次修改。
#[test]
fn mutate_saves_latest_values_and_does_not_save_failed_change() {
    let database_path = temporary_config_database();
    load_or_create_unlocked(&database_path).expect("config should initialize");

    let result = mutate_unlocked(&database_path, |config| {
        config.general.theme = "light".to_string();
        Ok("changed")
    })
    .expect("config change should save");
    assert_eq!(result, "changed");
    let changed = load_or_create_unlocked(&database_path).expect("config should reload");
    assert_eq!(changed.general.theme, "light");

    config_mapper::save_entries(
        &database_path,
        &[("future.key".to_string(), "opaque-value".to_string())],
    )
    .expect("unknown config value should save");
    mutate_unlocked(&database_path, |config| {
        config.ui.font_size = 17;
        Ok(())
    })
    .expect("latest config change should save");
    let latest = load_or_create_unlocked(&database_path).expect("latest config should reload");
    assert_eq!(latest.general.theme, "light");
    assert_eq!(latest.ui.font_size, 17);
    assert_eq!(
        config_mapper::load_entries(&database_path)
            .expect("entries should load")
            .iter()
            .find(|(key, _)| key == "future.key")
            .map(|(_, value)| value.as_str()),
        Some("opaque-value")
    );

    let failed = mutate_unlocked(&database_path, |config| {
        config.general.theme = "system".to_string();
        Err::<(), _>(anyhow::anyhow!("expected change failure"))
    });
    assert!(failed.is_err());
    let unchanged = load_or_create_unlocked(&database_path).expect("config should reload");
    assert_eq!(unchanged.general.theme, "light");
}
/// 验证公共配置编解码保留 CLI 原始字符串且身份重置清空旧配对凭据。
#[test]
fn dictionary_codec_keeps_cli_values_and_identity_reset_contract() {
    let mut config = AppConfig::default();
    config.claude_code.session_mode = " custom-mode ".to_string();
    config
        .harnesses
        .launch_args
        .insert("codex".to_string(), "  --custom flag  ".to_string());
    let restored = decode_config_dictionary(
        encode_config_dictionary(&config).expect("config should encode"),
    );
    assert_eq!(restored.claude_code.session_mode, " custom-mode ");
    assert_eq!(
        restored.harnesses.launch_args.get("codex").map(String::as_str),
        Some("  --custom flag  ")
    );

    let mut remote = RemoteAccessConfig::default();
    remote.devices.push(RemoteDeviceConfig {
        id: "device-1".to_string(),
        name: "phone".to_string(),
        credential: "dummy-device-credential".to_string(),
        paired_at: "2026-01-01T00:00:00Z".to_string(),
        last_connected_at: "2026-01-02T00:00:00Z".to_string(),
    });
    remote.device_credential = "dummy-legacy-credential".to_string();
    let old_id = remote.tunnel_id.clone();
    AppConfigService::regenerate_remote_identity(&mut remote);
    assert_ne!(remote.tunnel_id, old_id);
    assert!(remote.devices.is_empty());
    assert!(remote.device_credential.is_empty());
    assert!(remote.credential.len() >= 64);
}