    /*
    use std::{fs, path::Path};
    */

    use rusqlite::Connection;

    use super::{
        load_config_dictionary, normalize_remote_endpoint, save_config_dictionary, AppConfig,
        ClaudeCodeSessionMode, ComputerControlAuthorizationConfig, RemoteDeviceConfig,
    };

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

    /* 配置文件已取消，不再执行旧 TOML 导入测试辅助。
    /// 将显式路径上的旧 TOML 配置导入测试字典，并执行旧 endpoint 规范化。
    fn import_legacy_config(connection: &mut Connection, legacy_path: &Path) -> AppConfig {
        let raw = fs::read_to_string(legacy_path).expect("legacy config should be readable");
        let mut config = toml::from_str::<AppConfig>(&raw).unwrap_or_default();
        normalize_remote_endpoint(&mut config);
        save_config_dictionary(connection, &config).expect("legacy config should be persisted");
        config
    }
    */

    #[test]
    fn missing_locale_field_uses_none() {
        let raw = r#"
[general]
theme = "dark"
default_engine = "codex"
default_model = "gpt-5.4"

[ui]
sidebar_width = 260
git_panel_width = 380
font_size = 13

[debug]
persist_engine_event_logs = false
max_action_output_chars = 20000
"#;

        let config = toml::from_str::<AppConfig>(raw).expect("config should deserialize");

        assert_eq!(config.general.locale, None);
        assert!(!config.power.keep_awake_enabled);
        assert_eq!(config.general.terminal_accelerated_rendering, None);
        assert_eq!(config.general.terminal_notifications, None);
        assert!(!config.power.prevent_display_sleep);
        assert!(!config.power.prevent_screen_saver);
        assert!(!config.power.ac_only_mode);
        assert_eq!(config.power.battery_threshold, None);
        assert_eq!(config.power.session_duration_secs, None);
        assert!(!config.power.prevent_closed_display_sleep);
        assert!(!config.computer_control.enabled);
    }

    #[test]
    fn default_config_omits_optional_general_fields_from_toml() {
        let raw = toml::to_string_pretty(&AppConfig::default()).expect("config should serialize");

        assert!(!raw.contains("locale"));
        assert!(raw.contains("[power]"));
        assert!(raw.contains("keep_awake_enabled = false"));
        assert!(!raw.contains("terminal_accelerated_rendering"));
        assert!(!raw.contains("terminal_notifications"));
        assert!(!raw.contains("terminal_font_size"));
        assert!(!raw.contains("default_file_open_target"));
        assert!(!raw.contains("harnesses"));
        assert!(raw.contains("[claude_code]"));
        assert!(raw.contains("session_mode = \"reuse_session\""));
    }

    #[test]
    fn claude_code_session_mode_defaults_and_parses_per_turn() {
        let default_config = AppConfig::default();
        assert_eq!(
            default_config.claude_code.session_mode(),
            ClaudeCodeSessionMode::ReuseSession
        );

        let per_turn = toml::from_str::<AppConfig>(
            r#"
[claude_code]
session_mode = "per_turn"
"#,
        )
        .expect("Claude Code session mode should deserialize");
        assert_eq!(
            per_turn.claude_code.session_mode(),
            ClaudeCodeSessionMode::PerTurn
        );
    }

    #[test]
    fn persistent_computer_control_authorization_roundtrips() {
        let mut config = AppConfig::default();
        config.computer_control.persistent_authorizations.push(
            super::ComputerControlAuthorizationConfig {
                request_id: "authorization-1".to_string(),
                target_key: "application:notepad.exe".to_string(),
                agent: "codex".to_string(),
                tool: "launch_app".to_string(),
                call_id: "call-1".to_string(),
                application: "notepad.exe".to_string(),
                operation: "input".to_string(),
                scope: "application".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
            },
        );

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let restored = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert_eq!(restored.computer_control.persistent_authorizations.len(), 1);
        assert_eq!(
            restored.computer_control.persistent_authorizations[0].target_key,
            "application:notepad.exe"
        );
        assert_eq!(
            restored.computer_control.persistent_authorizations[0].application,
            "notepad.exe"
        );
    }

    #[test]
    fn harness_launch_args_roundtrip_and_lookup() {
        let mut config = AppConfig::default();
        config
            .harnesses
            .launch_args
            .insert("codex".to_string(), "--yolo".to_string());
        config
            .harnesses
            .launch_args
            .insert("claude-code".to_string(), "  ".to_string());

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        assert!(raw.contains("[harnesses.launch_args]"));
        assert!(raw.contains("codex = \"--yolo\""));

        let reloaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");
        assert_eq!(reloaded.harness_launch_args("codex"), Some("--yolo"));
        // Blank values are treated as unset.
        assert_eq!(reloaded.harness_launch_args("claude-code"), None);
        assert_eq!(reloaded.harness_launch_args("gemini-cli"), None);
    }

    #[test]
    fn legacy_native_window_decorations_field_is_ignored() {
        let raw = r#"
[general]
theme = "dark"
default_engine = "codex"
default_model = "gpt-5.4"
native_window_decorations = false

[ui]
sidebar_width = 260
git_panel_width = 380
font_size = 13

[debug]
persist_engine_event_logs = false
max_action_output_chars = 20000
"#;

        let config = toml::from_str::<AppConfig>(raw).expect("legacy config should deserialize");

        assert_eq!(config.general.locale, None);
        assert_eq!(config.general.terminal_accelerated_rendering, None);
        assert_eq!(config.general.terminal_notifications, None);
        assert_eq!(config.general.terminal_font_size, None);
    }

    #[test]
    fn terminal_font_size_defaults_when_unset() {
        let config = AppConfig::default();

        assert_eq!(config.general.terminal_font_size, None);
        assert_eq!(
            config.terminal_font_size(),
            super::DEFAULT_TERMINAL_FONT_SIZE
        );
    }

    #[test]
    fn terminal_font_size_clamps_out_of_range_values() {
        assert_eq!(
            super::clamp_terminal_font_size(1),
            super::MIN_TERMINAL_FONT_SIZE
        );
        assert_eq!(
            super::clamp_terminal_font_size(1000),
            super::MAX_TERMINAL_FONT_SIZE
        );
        assert_eq!(super::clamp_terminal_font_size(18), 18);
    }

    #[test]
    fn terminal_font_size_serialize_roundtrip() {
        let mut config = AppConfig::default();
        config.general.terminal_font_size = Some(16);

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let loaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert_eq!(loaded.general.terminal_font_size, Some(16));
        assert_eq!(loaded.terminal_font_size(), 16);
    }

    #[test]
    fn display_scale_defaults_and_normalizes_unknown_values() {
        let config = AppConfig::default();
        assert_eq!(config.display_scale(), super::DEFAULT_DISPLAY_SCALE);

        let mut invalid = AppConfig::default();
        invalid.ui.display_scale = 125;
        assert_eq!(invalid.display_scale(), super::DEFAULT_DISPLAY_SCALE);
    }

    #[test]
    fn display_scale_serialize_roundtrip() {
        let mut config = AppConfig::default();
        config.ui.display_scale = 150;

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let loaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert_eq!(loaded.display_scale(), 150);
    }

    #[test]
    fn transcript_width_defaults_and_normalizes_unknown_values() {
        let config = AppConfig::default();
        assert_eq!(config.transcript_width(), super::DEFAULT_TRANSCRIPT_WIDTH);

        let mut invalid = AppConfig::default();
        invalid.ui.transcript_width = Some("extra-wide".to_string());
        assert_eq!(invalid.transcript_width(), super::DEFAULT_TRANSCRIPT_WIDTH);
    }

    #[test]
    fn transcript_width_serialize_roundtrip() {
        let mut config = AppConfig::default();
        config.ui.transcript_width = Some("wide".to_string());

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let loaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert_eq!(loaded.transcript_width(), "wide");
    }

    #[test]
    fn chat_input_height_defaults_and_rejects_zero() {
        let config = AppConfig::default();
        assert_eq!(config.chat_input_height(), None);
        assert_eq!(super::normalize_chat_input_height(Some(0)), None);
        assert_eq!(super::normalize_chat_input_height(Some(180)), Some(180));
    }

    #[test]
    fn chat_input_height_serialize_roundtrip() {
        let mut config = AppConfig::default();
        config.ui.chat_input_height = Some(180);

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let loaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert_eq!(loaded.chat_input_height(), Some(180));
    }

    #[test]
    fn terminal_accelerated_rendering_defaults_to_enabled() {
        let config = AppConfig::default();

        assert!(config.terminal_accelerated_rendering_enabled());
    }

    #[test]
    fn terminal_notifications_default_to_disabled() {
        let config = AppConfig::default();

        assert!(!config.terminal_notifications_enabled());
    }

    #[test]
    fn theme_preference_defaults_to_dark() {
        let config = AppConfig::default();

        assert_eq!(config.theme_preference(), "dark");
    }

    #[test]
    fn theme_preference_accepts_light_and_system() {
        let mut config = AppConfig::default();

        config.general.theme = "light".to_string();
        assert_eq!(config.theme_preference(), "light");

        config.general.theme = "system".to_string();
        assert_eq!(config.theme_preference(), "system");
    }

    #[test]
    fn theme_preference_falls_back_to_dark_for_unknown_values() {
        let mut config = AppConfig::default();
        config.general.theme = "solarized".to_string();

        assert_eq!(config.theme_preference(), "dark");
    }

    #[test]
    fn new_power_fields_serialize_roundtrip() {
        let mut config = AppConfig::default();
        config.power.prevent_display_sleep = true;
        config.power.prevent_screen_saver = true;
        config.power.ac_only_mode = true;
        config.power.battery_threshold = Some(20);
        config.power.session_duration_secs = Some(3600);
        config.power.prevent_closed_display_sleep = true;

        let raw = toml::to_string_pretty(&config).expect("config should serialize");
        let loaded = toml::from_str::<AppConfig>(&raw).expect("config should deserialize");

        assert!(loaded.power.prevent_display_sleep);
        assert!(loaded.power.prevent_screen_saver);
        assert!(loaded.power.ac_only_mode);
        assert_eq!(loaded.power.battery_threshold, Some(20));
        assert_eq!(loaded.power.session_duration_secs, Some(3600));
        assert!(loaded.power.prevent_closed_display_sleep);
    }

    #[test]
    fn old_config_without_new_power_fields_loads() {
        let raw = r#"
[general]
theme = "dark"
default_engine = "codex"
default_model = "gpt-5.4"

[ui]
sidebar_width = 260
git_panel_width = 380
font_size = 13

[debug]
persist_engine_event_logs = false
max_action_output_chars = 20000

[power]
keep_awake_enabled = true
"#;

        let config = toml::from_str::<AppConfig>(raw).expect("old config should deserialize");

        assert!(config.power.keep_awake_enabled);
        assert!(!config.power.prevent_display_sleep);
        assert!(!config.power.prevent_screen_saver);
        assert!(!config.power.ac_only_mode);
        assert_eq!(config.power.battery_threshold, None);
        assert_eq!(config.power.session_duration_secs, None);
        assert!(!config.power.prevent_closed_display_sleep);
    }

    #[test]
    fn notification_sound_capability_matches_current_platform() {
        let capability = super::notification_sound_capability();

        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                capability.options,
                vec!["Default", "IM", "Mail", "Reminder", "SMS"]
            );
            assert!(capability.preview_supported);
        }

        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                capability.options,
                vec![
                    "Glass",
                    "Ping",
                    "Pop",
                    "Purr",
                    "Tink",
                    "Blow",
                    "Bottle",
                    "Frog",
                    "Funk",
                    "Hero",
                    "Morse",
                    "Sosumi",
                    "Submarine",
                    "Basso",
                ]
            );
            assert!(capability.preview_supported);
        }

        #[cfg(target_os = "linux")]
        {
            assert!(capability.options.is_empty());
            assert!(!capability.preview_supported);
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            assert!(capability.options.is_empty());
            assert!(!capability.preview_supported);
        }
    }

    #[test]
    fn notification_sound_resolution_validates_saved_value() {
        let mut config = AppConfig::default();

        #[cfg(target_os = "windows")]
        {
            config.general.notification_sound = Some("Default".to_string());
            assert!(AppConfig::validate_notification_sound("Default").is_ok());
            assert_eq!(config.notification_sound(), Some("Default"));

            config.general.notification_sound = Some("Glass".to_string());
            assert!(AppConfig::validate_notification_sound("Glass").is_err());
            assert_eq!(config.notification_sound(), None);
        }

        #[cfg(target_os = "macos")]
        {
            config.general.notification_sound = Some("Glass".to_string());
            assert!(AppConfig::validate_notification_sound("Glass").is_ok());
            assert_eq!(config.notification_sound(), Some("Glass"));

            config.general.notification_sound = Some("Default".to_string());
            assert!(AppConfig::validate_notification_sound("Default").is_err());
            assert_eq!(config.notification_sound(), None);
        }

        #[cfg(target_os = "linux")]
        {
            config.general.notification_sound = Some("Glass".to_string());
            let error = AppConfig::validate_notification_sound("Glass")
                .expect_err("unsupported platforms must reject sounds");
            assert!(error.contains("不支持声音通知"));
            assert_eq!(config.notification_sound(), None);
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            config.general.notification_sound = Some("Glass".to_string());
            let error = AppConfig::validate_notification_sound("Glass")
                .expect_err("unsupported platforms must reject sounds");
            assert!(error.contains("不支持声音通知"));
            assert_eq!(config.notification_sound(), None);
        }
    }

    #[test]
    fn notification_sound_config_parses_without_rewriting_value() {
        let raw = r#"
[general]
theme = "dark"
default_engine = "codex"
default_model = "gpt-5.4"
notification_sound = "Glass"
"#;
        let config =
            toml::from_str::<AppConfig>(raw).expect("notification sound should deserialize");

        assert_eq!(config.general.notification_sound.as_deref(), Some("Glass"));
        #[cfg(target_os = "windows")]
        assert_eq!(config.notification_sound(), None);
        #[cfg(target_os = "macos")]
        assert_eq!(config.notification_sound(), Some("Glass"));
        #[cfg(target_os = "linux")]
        assert_eq!(config.notification_sound(), None);
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        assert_eq!(config.notification_sound(), None);
    }

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

    /* 配置文件已取消，不再执行旧 TOML 导入测试。
    #[test]
    fn legacy_toml_import_preserves_values_and_normalizes_endpoint() {
        let test_dir = std::env::temp_dir().join(format!(
            "auracoder-app-config-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&test_dir).expect("test directory should be created");
        let legacy_path = test_dir.join("config.toml");
        let mut legacy = AppConfig::default();
        legacy.general.theme = "light".to_string();
        legacy.remote_access.endpoint = "wss://panes.jxrjkf.cn/ws/tunnel".to_string();
        legacy.remote_access.device_credential = "test-device-credential".to_string();
        legacy.remote_access.devices.push(RemoteDeviceConfig {
            id: "legacy-device".to_string(),
            name: "legacy phone".to_string(),
            credential: "test-credential".to_string(),
            paired_at: "2026-01-01T00:00:00Z".to_string(),
            last_connected_at: "2026-01-01T00:00:00Z".to_string(),
        });
        fs::write(
            &legacy_path,
            toml::to_string_pretty(&legacy).expect("legacy config should serialize"),
        )
        .expect("legacy config should be written");

        let mut connection = memory_config_database();
        let imported = import_legacy_config(&mut connection, &legacy_path);
        assert_eq!(imported.general.theme, "light");
        assert_eq!(
            imported.remote_access.endpoint,
            "wss://auracoder.jxrjkf.cn/ws/tunnel"
        );
        assert_eq!(imported.remote_access.devices.len(), 1);
        assert_eq!(
            imported.remote_access.device_credential,
            "test-device-credential"
        );
        let row_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM config", [], |row| row.get(0))
            .expect("config row count should be readable");
        assert_eq!(row_count, 37);
        fs::remove_dir_all(&test_dir).expect("test directory should be removed");
    }
    */

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
