use std::{
    path::Path,
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::Context;

use crate::config::app_config::{AppConfig, RemoteAccessConfig};
use crate::{db::app_config as config_mapper, runtime_env};

/// 将配置字典中的单个 JSON 文本应用到目标字段，损坏值只影响当前字段。
macro_rules! apply_config_json {
    ($key:expr, $raw:expr, $target:expr) => {
        match serde_json::from_str(&$raw) {
            Ok(value) => $target = value,
            Err(error) => log::warn!("invalid JSON for app config key {}: {}", $key, error),
        }
    };
}

/// 统一处理应用配置读取、写入和原子更新的业务服务。
pub struct AppConfigService;

impl AppConfigService {
    /// 读取应用配置；首次初始化和远程地址修正与读取保持在同一写锁内。
    pub fn load_or_create() -> anyhow::Result<AppConfig> {
        let result = (|| {
            let _guard = lock_config()?;
            runtime_env::migrate_legacy_app_data_dir()
                .context("failed to migrate legacy app data dir")?;
            let database_path = config_mapper::configuration_database_path();
            load_or_create_unlocked(&database_path)
        })();

        if let Err(error) = &result {
            log::error!("failed to load app config: {error:#}");
        }
        result
    }

    /// 保存完整应用配置并记录保存失败的原始错误。
    pub fn save(config: &AppConfig) -> anyhow::Result<()> {
        let result = (|| {
            let _guard = lock_config()?;
            let database_path = config_mapper::configuration_database_path();
            save_unlocked(&database_path, config)
        })();

        if let Err(error) = &result {
            log::error!("failed to save app config: {error:#}");
        }
        result
    }

    /// 在同一配置写锁内完成读取、修改和保存，避免独立读写之间丢失更新。
    pub fn mutate<T>(
        change: impl FnOnce(&mut AppConfig) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let result = (|| {
            let _guard = lock_config()?;
            runtime_env::migrate_legacy_app_data_dir()
                .context("failed to migrate legacy app data dir")?;
            let database_path = config_mapper::configuration_database_path();
            mutate_unlocked(&database_path, change)
        })();

        if let Err(error) = &result {
            log::error!("failed to mutate app config: {error:#}");
        }
        result
    }

    /// 为传入的远程访问配置补齐身份信息，并返回是否发生了变更。
    pub fn ensure_remote_identity(config: &mut RemoteAccessConfig) -> bool {
        if !config.tunnel_id.trim().is_empty() && config.credential.trim().len() >= 32 {
            return false;
        }
        Self::regenerate_remote_identity(config);
        true
    }

    /// 为传入的远程访问配置生成新身份并清除旧配对凭据。
    pub fn regenerate_remote_identity(config: &mut RemoteAccessConfig) {
        config.tunnel_id = format!("auracoder_{}", uuid::Uuid::new_v4().simple());
        config.credential = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        config.devices.clear();
        config.device_credential.clear();
    }
}

/// 在应用启动或设置修改期间串行化配置字典的读取和写入。
fn config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 获取配置写锁并将锁中毒错误转换为业务错误。
fn lock_config() -> anyhow::Result<MutexGuard<'static, ()>> {
    config_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("config lock poisoned"))
}

/// 从运行时配置字典加载默认配置，并在首次初始化或 endpoint 修正时保存。
fn load_or_create_unlocked(database_path: &Path) -> anyhow::Result<AppConfig> {
    let entries = config_mapper::load_entries(database_path)?;
    let was_empty = entries.is_empty();
    let mut config = decode_config_dictionary(entries);
    let normalized = normalize_remote_endpoint(&mut config);
    if was_empty || normalized {
        save_unlocked(database_path, &config)?;
    }
    Ok(config)
}

/// 将配置字段编码为 JSON 字典后交给 Mapper 原子写入。
fn save_unlocked(database_path: &Path, config: &AppConfig) -> anyhow::Result<()> {
    let entries = encode_config_dictionary(config)?;
    config_mapper::save_entries(database_path, &entries)
}

/// 在给定数据库中完成一次无锁读取、变更和保存，锁由公开 mutate 入口持有。
fn mutate_unlocked<T>(
    database_path: &Path,
    change: impl FnOnce(&mut AppConfig) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut config = load_or_create_unlocked(database_path)?;
    let result = change(&mut config)?;
    save_unlocked(database_path, &config)?;
    Ok(result)
}

/// 将原始配置字典解码为应用配置，未知键保留在存储中且不影响默认值。
fn decode_config_dictionary(entries: Vec<(String, String)>) -> AppConfig {
    let mut config = AppConfig::default();
    for (key, raw) in entries {
        match key.as_str() {
            "general.theme" => apply_config_json!(&key, raw, config.general.theme),
            "general.default_engine" => {
                apply_config_json!(&key, raw, config.general.default_engine)
            }
            "general.default_model" => {
                apply_config_json!(&key, raw, config.general.default_model)
            }
            "general.locale" => apply_config_json!(&key, raw, config.general.locale),
            "general.terminal_accelerated_rendering" => {
                apply_config_json!(&key, raw, config.general.terminal_accelerated_rendering)
            }
            "general.terminal_font_size" => {
                apply_config_json!(&key, raw, config.general.terminal_font_size)
            }
            "general.chat_notifications" => {
                apply_config_json!(&key, raw, config.general.chat_notifications)
            }
            "general.terminal_notifications" => {
                apply_config_json!(&key, raw, config.general.terminal_notifications)
            }
            "general.notification_sound" => {
                apply_config_json!(&key, raw, config.general.notification_sound)
            }
            "general.default_autonomy_preset" => {
                apply_config_json!(&key, raw, config.general.default_autonomy_preset)
            }
            "general.default_file_open_target" => {
                apply_config_json!(&key, raw, config.general.default_file_open_target)
            }
            "general.gpu_acceleration_enabled" => {
                apply_config_json!(&key, raw, config.general.gpu_acceleration_enabled)
            }
            "ui.sidebar_width" => apply_config_json!(&key, raw, config.ui.sidebar_width),
            "ui.git_panel_width" => apply_config_json!(&key, raw, config.ui.git_panel_width),
            "ui.font_size" => apply_config_json!(&key, raw, config.ui.font_size),
            "ui.display_scale" => apply_config_json!(&key, raw, config.ui.display_scale),
            "ui.transcript_width" => apply_config_json!(&key, raw, config.ui.transcript_width),
            "ui.chat_input_height" => apply_config_json!(&key, raw, config.ui.chat_input_height),
            "debug.persist_engine_event_logs" => {
                apply_config_json!(&key, raw, config.debug.persist_engine_event_logs)
            }
            "debug.max_action_output_chars" => {
                apply_config_json!(&key, raw, config.debug.max_action_output_chars)
            }
            "power.keep_awake_enabled" => {
                apply_config_json!(&key, raw, config.power.keep_awake_enabled)
            }
            "power.prevent_display_sleep" => {
                apply_config_json!(&key, raw, config.power.prevent_display_sleep)
            }
            "power.prevent_screen_saver" => {
                apply_config_json!(&key, raw, config.power.prevent_screen_saver)
            }
            "power.ac_only_mode" => apply_config_json!(&key, raw, config.power.ac_only_mode),
            "power.battery_threshold" => {
                apply_config_json!(&key, raw, config.power.battery_threshold)
            }
            "power.session_duration_secs" => {
                apply_config_json!(&key, raw, config.power.session_duration_secs)
            }
            "power.prevent_closed_display_sleep" => {
                apply_config_json!(&key, raw, config.power.prevent_closed_display_sleep)
            }
            "computer_control.enabled" => {
                apply_config_json!(&key, raw, config.computer_control.enabled)
            }
            "computer_control.persistent_authorizations" => {
                apply_config_json!(&key, raw, config.computer_control.persistent_authorizations)
            }
            "claude_code.session_mode" => {
                apply_config_json!(&key, raw, config.claude_code.session_mode)
            }
            "remote_access.enabled" => {
                apply_config_json!(&key, raw, config.remote_access.enabled)
            }
            "remote_access.endpoint" => {
                apply_config_json!(&key, raw, config.remote_access.endpoint)
            }
            "remote_access.tunnel_id" => {
                apply_config_json!(&key, raw, config.remote_access.tunnel_id)
            }
            "remote_access.credential" => {
                apply_config_json!(&key, raw, config.remote_access.credential)
            }
            "remote_access.devices" => apply_config_json!(&key, raw, config.remote_access.devices),
            "remote_access.device_credential" => {
                apply_config_json!(&key, raw, config.remote_access.device_credential)
            }
            "harnesses.launch_args" => {
                apply_config_json!(&key, raw, config.harnesses.launch_args)
            }
            _ => {}
        }
    }

    config
}

/// 将应用配置的 37 个固定字段编码为 JSON 字典项。
fn encode_config_dictionary(config: &AppConfig) -> anyhow::Result<Vec<(String, String)>> {
    let mut entries = Vec::with_capacity(37);
    macro_rules! upsert {
        ($key:expr, $value:expr $(,)?) => {{
            let serialized = serde_json::to_string($value)?;
            entries.push(($key.to_string(), serialized));
            Ok::<(), anyhow::Error>(())
        }};
    }

    upsert!("general.theme", &config.general.theme)?;
    upsert!("general.default_engine", &config.general.default_engine)?;
    upsert!("general.default_model", &config.general.default_model)?;
    upsert!("general.locale", &config.general.locale)?;
    upsert!(
        "general.terminal_accelerated_rendering",
        &config.general.terminal_accelerated_rendering,
    )?;
    upsert!(
        "general.terminal_font_size",
        &config.general.terminal_font_size,
    )?;
    upsert!(
        "general.chat_notifications",
        &config.general.chat_notifications,
    )?;
    upsert!(
        "general.terminal_notifications",
        &config.general.terminal_notifications,
    )?;
    upsert!(
        "general.notification_sound",
        &config.general.notification_sound,
    )?;
    upsert!(
        "general.default_autonomy_preset",
        &config.general.default_autonomy_preset,
    )?;
    upsert!(
        "general.default_file_open_target",
        &config.general.default_file_open_target,
    )?;
    upsert!(
        "general.gpu_acceleration_enabled",
        &config.general.gpu_acceleration_enabled,
    )?;
    upsert!("ui.sidebar_width", &config.ui.sidebar_width)?;
    upsert!("ui.git_panel_width", &config.ui.git_panel_width)?;
    upsert!("ui.font_size", &config.ui.font_size)?;
    upsert!("ui.display_scale", &config.ui.display_scale)?;
    upsert!("ui.transcript_width", &config.ui.transcript_width)?;
    upsert!("ui.chat_input_height", &config.ui.chat_input_height)?;
    upsert!(
        "debug.persist_engine_event_logs",
        &config.debug.persist_engine_event_logs,
    )?;
    upsert!(
        "debug.max_action_output_chars",
        &config.debug.max_action_output_chars,
    )?;
    upsert!("power.keep_awake_enabled", &config.power.keep_awake_enabled)?;
    upsert!(
        "power.prevent_display_sleep",
        &config.power.prevent_display_sleep,
    )?;
    upsert!(
        "power.prevent_screen_saver",
        &config.power.prevent_screen_saver,
    )?;
    upsert!("power.ac_only_mode", &config.power.ac_only_mode)?;
    upsert!("power.battery_threshold", &config.power.battery_threshold)?;
    upsert!(
        "power.session_duration_secs",
        &config.power.session_duration_secs,
    )?;
    upsert!(
        "power.prevent_closed_display_sleep",
        &config.power.prevent_closed_display_sleep,
    )?;
    upsert!("computer_control.enabled", &config.computer_control.enabled)?;
    upsert!(
        "computer_control.persistent_authorizations",
        &config.computer_control.persistent_authorizations,
    )?;
    upsert!("claude_code.session_mode", &config.claude_code.session_mode)?;
    upsert!("remote_access.enabled", &config.remote_access.enabled)?;
    upsert!("remote_access.endpoint", &config.remote_access.endpoint)?;
    upsert!("remote_access.tunnel_id", &config.remote_access.tunnel_id)?;
    upsert!("remote_access.credential", &config.remote_access.credential)?;
    upsert!("remote_access.devices", &config.remote_access.devices)?;
    upsert!(
        "remote_access.device_credential",
        &config.remote_access.device_credential,
    )?;
    upsert!("harnesses.launch_args", &config.harnesses.launch_args)?;

    Ok(entries)
}

/// 将旧版 panes 远程地址修正为 AuraCoder 当前远程地址。
/// 将旧版 panes 远程地址修正为 AuraCoder 当前远程地址。
fn normalize_remote_endpoint(config: &mut AppConfig) -> bool {
    if config.remote_access.endpoint == "wss://panes.jxrjkf.cn/ws/tunnel" {
        config.remote_access.endpoint = "wss://auracoder.jxrjkf.cn/ws/tunnel".to_string();
        true
    } else {
        false
    }
}

#[cfg(test)]
#[path = "../../tests/unit/chat_input_height_config_tests.rs"]
mod chat_input_height_config_tests;

#[cfg(test)]
#[path = "../../tests/unit/app_config_service_tests.rs"]
mod tests;