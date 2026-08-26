use std::{
    collections::BTreeMap,
    /*
    fs,
    path::PathBuf,
    */
    sync::{Mutex, MutexGuard, OnceLock},
    time::Duration,
};

use anyhow::Context;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::runtime_env;

/// 配置字典中允许由 AppConfig 读写的固定键集合，未知键由数据库保留。
#[cfg(test)]
const CONFIG_KEYS: [&str; 34] = [
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
];

/// 将配置字典中的单个 JSON 文本应用到目标字段，损坏值只影响当前字段。
macro_rules! apply_config_json {
    ($key:expr, $raw:expr, $target:expr) => {
        match serde_json::from_str(&$raw) {
            Ok(value) => $target = value,
            Err(error) => log::warn!("invalid JSON for app config key {}: {}", $key, error),
        }
    };
}

pub const DEFAULT_TERMINAL_FONT_SIZE: u32 = 12;
pub const MIN_TERMINAL_FONT_SIZE: u32 = 8;
pub const MAX_TERMINAL_FONT_SIZE: u32 = 32;
pub const DEFAULT_DISPLAY_SCALE: u32 = 100;
pub const VALID_DISPLAY_SCALES: [u32; 6] = [100, 110, 120, 130, 140, 150];
pub const VALID_AUTONOMY_PRESETS: [&str; 4] = ["read-only", "ask", "auto", "full"];

/// Clamp a requested terminal font size into the supported range.
pub fn clamp_terminal_font_size(font_size: u32) -> u32 {
    font_size.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE)
}

/// Resolve a persisted display scale to a supported value.
pub fn normalize_display_scale(display_scale: u32) -> u32 {
    if VALID_DISPLAY_SCALES.contains(&display_scale) {
        display_scale
    } else {
        DEFAULT_DISPLAY_SCALE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub ui: UiConfig,
    pub debug: DebugConfig,
    pub power: PowerConfig,
    pub computer_control: ComputerControlConfig,
    pub claude_code: ClaudeCodeConfig,
    #[serde(skip_serializing_if = "RemoteAccessConfig::is_default")]
    pub remote_access: RemoteAccessConfig,
    #[serde(skip_serializing_if = "HarnessesConfig::is_empty")]
    pub harnesses: HarnessesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub theme: String,
    pub default_engine: String,
    pub default_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_accelerated_rendering: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_font_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_sound: Option<String>,
    /// Autonomy preset applied to newly created chat threads
    /// (`read-only` | `ask` | `auto` | `full`); `None` follows repo trust.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_autonomy_preset: Option<String>,
    /// Stable ID of a system-discovered text editor used for external file
    /// opening. `None` keeps the operating system's default application.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_file_open_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub sidebar_width: u32,
    pub git_panel_width: u32,
    pub font_size: u32,
    pub display_scale: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugConfig {
    pub persist_engine_event_logs: bool,
    pub max_action_output_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    pub keep_awake_enabled: bool,
    pub prevent_display_sleep: bool,
    pub prevent_screen_saver: bool,
    pub ac_only_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_threshold: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_duration_secs: Option<u64>,
    pub prevent_closed_display_sleep: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputerControlConfig {
    pub enabled: bool,
    pub persistent_authorizations: Vec<ComputerControlAuthorizationConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputerControlAuthorizationConfig {
    pub request_id: String,
    pub target_key: String,
    pub agent: String,
    pub tool: String,
    pub call_id: String,
    pub application: String,
    pub operation: String,
    pub scope: String,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeCodeSessionMode {
    ReuseSession,
    PerTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeCodeConfig {
    pub session_mode: String,
}

impl ClaudeCodeConfig {
    pub fn session_mode(&self) -> ClaudeCodeSessionMode {
        match self.session_mode.trim() {
            "per_turn" => ClaudeCodeSessionMode::PerTurn,
            _ => ClaudeCodeSessionMode::ReuseSession,
        }
    }
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            session_mode: "reuse_session".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteDeviceConfig {
    pub id: String,
    pub name: String,
    pub credential: String,
    pub paired_at: String,
    pub last_connected_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteAccessConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub tunnel_id: String,
    pub credential: String,
    pub devices: Vec<RemoteDeviceConfig>,
    // 兼容旧版只保存一个手机凭据的配置；手机再次连接后会迁移到 devices。
    pub device_credential: String,
}

impl RemoteAccessConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn ensure_identity(&mut self) -> bool {
        if !self.tunnel_id.trim().is_empty() && self.credential.trim().len() >= 32 {
            return false;
        }
        self.regenerate_identity();
        true
    }

    pub fn regenerate_identity(&mut self) {
        self.tunnel_id = format!("auracoder_{}", uuid::Uuid::new_v4().simple());
        self.credential = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.devices.clear();
        self.device_credential.clear();
    }
}

impl Default for RemoteAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "wss://auracoder.jxrjkf.cn/ws/tunnel".to_string(),
            tunnel_id: String::new(),
            credential: String::new(),
            devices: Vec::new(),
            device_credential: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessesConfig {
    /// Extra CLI flags appended to a harness command when it is launched into
    /// a terminal, keyed by harness id (e.g. `codex = "--yolo"`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub launch_args: BTreeMap<String, String>,
}

impl HarnessesConfig {
    fn is_empty(&self) -> bool {
        self.launch_args.is_empty()
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            default_engine: "codex".to_string(),
            default_model: "gpt-5.4".to_string(),
            locale: None,
            terminal_accelerated_rendering: None,
            terminal_font_size: None,
            chat_notifications: None,
            terminal_notifications: None,
            notification_sound: None,
            default_autonomy_preset: None,
            default_file_open_target: None,
        }
    }
}

pub const VALID_THEME_PREFERENCES: [&str; 3] = ["dark", "light", "system"];

const WINDOWS_NOTIFICATION_SOUND_OPTIONS: [&str; 5] = ["Default", "IM", "Mail", "Reminder", "SMS"];

const MACOS_NOTIFICATION_SOUND_OPTIONS: [&str; 14] = [
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
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSoundCapability {
    pub options: Vec<String>,
    pub preview_supported: bool,
}

pub fn notification_sound_capability() -> NotificationSoundCapability {
    #[cfg(target_os = "windows")]
    {
        return NotificationSoundCapability {
            options: WINDOWS_NOTIFICATION_SOUND_OPTIONS
                .iter()
                .map(ToString::to_string)
                .collect(),
            preview_supported: true,
        };
    }

    #[cfg(target_os = "macos")]
    {
        return NotificationSoundCapability {
            options: MACOS_NOTIFICATION_SOUND_OPTIONS
                .iter()
                .map(ToString::to_string)
                .collect(),
            preview_supported: true,
        };
    }

    #[cfg(target_os = "linux")]
    {
        return NotificationSoundCapability {
            options: Vec::new(),
            preview_supported: false,
        };
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        NotificationSoundCapability {
            options: Vec::new(),
            preview_supported: false,
        }
    }
}

impl AppConfig {
    /// 校验保存或试听的通知声音是否属于当前操作系统的能力范围。
    pub fn validate_notification_sound(sound: &str) -> Result<(), String> {
        let trimmed = sound.trim();
        if trimmed.is_empty() || trimmed == "none" {
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            if WINDOWS_NOTIFICATION_SOUND_OPTIONS.contains(&trimmed) {
                return Ok(());
            }
            return Err(format!("unsupported Windows notification sound: {trimmed}"));
        }

        #[cfg(target_os = "macos")]
        {
            if MACOS_NOTIFICATION_SOUND_OPTIONS.contains(&trimmed)
                || (std::path::Path::new(trimmed).is_absolute()
                    && std::path::Path::new(trimmed).is_file())
            {
                return Ok(());
            }
            return Err(format!("unsupported macOS notification sound: {trimmed}"));
        }

        #[cfg(target_os = "linux")]
        {
            return Err("当前操作系统不支持声音通知".to_string());
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Err("当前操作系统不支持声音通知".to_string())
        }
    }

    /// 解析当前平台可安全使用的通知声音，不改写历史配置内容。
    pub fn notification_sound(&self) -> Option<&str> {
        match self.general.notification_sound.as_deref() {
            Some(sound) => {
                let trimmed = sound.trim();
                if trimmed.is_empty() || trimmed == "none" {
                    None
                } else if Self::validate_notification_sound(trimmed).is_ok() {
                    Some(trimmed)
                } else {
                    None
                }
            }
            None => default_notification_sound(),
        }
    }

    /// Resolve the configured theme preference, falling back to `"dark"` for
    /// unrecognized or legacy values so old config files always load cleanly.
    pub fn theme_preference(&self) -> &str {
        if VALID_THEME_PREFERENCES.contains(&self.general.theme.as_str()) {
            &self.general.theme
        } else {
            "dark"
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 260,
            git_panel_width: 380,
            font_size: 13,
            display_scale: DEFAULT_DISPLAY_SCALE,
        }
    }
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            persist_engine_event_logs: false,
            max_action_output_chars: 20_000,
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            keep_awake_enabled: false,
            prevent_display_sleep: false,
            prevent_screen_saver: false,
            ac_only_mode: false,
            battery_threshold: None,
            session_duration_secs: None,
            prevent_closed_display_sleep: false,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            ui: UiConfig::default(),
            debug: DebugConfig::default(),
            power: PowerConfig::default(),
            computer_control: ComputerControlConfig::default(),
            claude_code: ClaudeCodeConfig::default(),
            remote_access: RemoteAccessConfig::default(),
            harnesses: HarnessesConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn terminal_accelerated_rendering_enabled(&self) -> bool {
        self.general.terminal_accelerated_rendering.unwrap_or(true)
    }

    pub fn terminal_font_size(&self) -> u32 {
        self.general
            .terminal_font_size
            .map(clamp_terminal_font_size)
            .unwrap_or(DEFAULT_TERMINAL_FONT_SIZE)
    }

    pub fn display_scale(&self) -> u32 {
        normalize_display_scale(self.ui.display_scale)
    }

    pub fn chat_notifications_enabled(&self) -> bool {
        self.general.chat_notifications.unwrap_or(false)
    }

    pub fn terminal_notifications_enabled(&self) -> bool {
        self.general.terminal_notifications.unwrap_or(false)
    }

    /// Extra launch flags configured for a harness, or `None` when unset or
    /// blank.
    pub fn harness_launch_args(&self, harness_id: &str) -> Option<&str> {
        self.harnesses
            .launch_args
            .get(harness_id)
            .map(|args| args.trim())
            .filter(|args| !args.is_empty())
    }

    pub fn default_autonomy_preset(&self) -> Option<&str> {
        self.general
            .default_autonomy_preset
            .as_deref()
            .filter(|preset| VALID_AUTONOMY_PRESETS.contains(preset))
    }

    pub fn load_or_create() -> anyhow::Result<Self> {
        let _guard = lock_config()?;
        Self::load_or_create_unlocked()
    }

    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let _guard = lock_config()?;
        self.save_unlocked()
    }

    pub fn mutate<T>(f: impl FnOnce(&mut Self) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let _guard = lock_config()?;
        let mut config = Self::load_or_create_unlocked()?;
        let result = f(&mut config)?;
        config.save_unlocked()?;
        Ok(result)
    }

    /*
    pub fn set_display_scale(display_scale: u32) -> anyhow::Result<u32> {
        if normalize_display_scale(display_scale) != display_scale {
            anyhow::bail!("unsupported display scale: {display_scale}");
        }

        let _guard = lock_config()?;
        let mut config = Self::load_or_create_unlocked()?;
        config.ui.display_scale = display_scale;
        config.save_unlocked()?;

        let persisted_display_scale = Self::load_or_create_unlocked()?.display_scale();
        if persisted_display_scale != display_scale {
            anyhow::bail!(
                "display scale did not persist: expected {display_scale}, got {persisted_display_scale}"
            );
        }

        Ok(persisted_display_scale)
    }
    */

    fn load_or_create_unlocked() -> anyhow::Result<Self> {
        runtime_env::migrate_legacy_app_data_dir()
            .context("failed to migrate legacy app data dir")?;
        let mut connection = open_config_database()?;
        let dictionary_rows: i64 =
            connection.query_row("SELECT COUNT(*) FROM config", [], |row| row.get(0))?;

        if dictionary_rows > 0 {
            let mut config = load_config_dictionary(&connection)?;
            if normalize_remote_endpoint(&mut config) {
                save_config_dictionary(&mut connection, &config)?;
            }
            return Ok(config);
        }

        /* 配置文件已取消，不再执行以下旧 TOML 导入、备份和重命名逻辑。
        let path = Self::path();
        let legacy_file_exists = path.exists();
        let mut config = if legacy_file_exists {
            let raw = fs::read_to_string(&path)?;
            toml::from_str::<Self>(&raw).unwrap_or_default()
        } else {
            Self::default()
        };
        normalize_remote_endpoint(&mut config);
        save_config_dictionary(&mut connection, &config)?;

        if legacy_file_exists {
            let backup_path = path.with_file_name("config.toml.migrated.bak");
            if let Err(error) = fs::rename(&path, &backup_path) {
                log::warn!("failed to back up migrated config.toml: {}", error);
            }
        }
        */

        let mut config = Self::default();
        normalize_remote_endpoint(&mut config);
        save_config_dictionary(&mut connection, &config)?;
        Ok(config)
    }

    fn save_unlocked(&self) -> anyhow::Result<()> {
        /* SQLite 字典持久化已替代以下旧 TOML 写入逻辑，保留原代码以便追溯。
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let raw = toml::to_string_pretty(self)?;
        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, raw)?;
        replace_file(&temp_path, &path)?;
        Ok(())
        */

        let mut connection = open_config_database()?;
        save_config_dictionary(&mut connection, self)
    }

    /* 配置文件已取消，停用原用于定位 config.toml 的公开接口。
    pub fn path() -> PathBuf {
        runtime_env::config_path()
    }
    */
}

/// 返回当前操作系统的默认通知声音，未支持的平台不附加声音。
fn default_notification_sound() -> Option<&'static str> {
    #[cfg(target_os = "windows")]
    {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        return Some("Glass");
    }

    #[cfg(target_os = "linux")]
    {
        return None;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// 打开运行期工作区数据库，并设置配置读写使用的五秒锁等待时间。
fn open_config_database() -> anyhow::Result<Connection> {
    let database_path = runtime_env::app_data_dir().join("workspaces.db");
    let connection = Connection::open(database_path)
        .context("failed to open workspaces database for app config")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

/// 从 SQLite 配置字典加载已知配置字段，缺失字段保留默认值，未知字段继续保留在数据库中。
fn load_config_dictionary(connection: &Connection) -> anyhow::Result<AppConfig> {
    let mut config = AppConfig::default();
    let mut statement = connection.prepare("SELECT config_key, config_value FROM config")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (key, raw) = row?;
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
            "ui.sidebar_width" => apply_config_json!(&key, raw, config.ui.sidebar_width),
            "ui.git_panel_width" => apply_config_json!(&key, raw, config.ui.git_panel_width),
            "ui.font_size" => apply_config_json!(&key, raw, config.ui.font_size),
            "ui.display_scale" => apply_config_json!(&key, raw, config.ui.display_scale),
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

    Ok(config)
}

/// 在一个 SQLite 事务中写入 AppConfig 的全部固定字典字段，并保留未知键。
fn save_config_dictionary(connection: &mut Connection, config: &AppConfig) -> anyhow::Result<()> {
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)\
         ON CONFLICT(config_key) DO UPDATE SET config_value = excluded.config_value",
    )?;
    // 34 个固定字段统一使用 JSON 文本写入字典表，未知键不会被触碰。
    macro_rules! upsert {
        ($key:expr, $value:expr $(,)?) => {{
            let serialized = serde_json::to_string($value)?;
            statement.execute(params![$key, serialized])?;
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
    upsert!("ui.sidebar_width", &config.ui.sidebar_width)?;
    upsert!("ui.git_panel_width", &config.ui.git_panel_width)?;
    upsert!("ui.font_size", &config.ui.font_size)?;
    upsert!("ui.display_scale", &config.ui.display_scale)?;
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

    drop(statement);
    transaction.commit()?;
    Ok(())
}

/// 将旧版 panes 远程地址修正为 AuraCoder 当前远程地址。
fn normalize_remote_endpoint(config: &mut AppConfig) -> bool {
    if config.remote_access.endpoint == "wss://panes.jxrjkf.cn/ws/tunnel" {
        config.remote_access.endpoint = "wss://auracoder.jxrjkf.cn/ws/tunnel".to_string();
        true
    } else {
        false
    }
}

fn config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_config() -> anyhow::Result<MutexGuard<'static, ()>> {
    config_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("config lock poisoned"))
}

/* SQLite 字典持久化已替代旧 TOML 原子替换逻辑，保留原函数体以便追溯。
fn replace_file(temp_path: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Windows does not support atomic rename-over-existing. Use a backup
        // strategy: rename the existing file to .bak, rename the new file into
        // place, then remove .bak.  A crash between steps 1 and 2 leaves the
        // .bak file as a recoverable copy.
        if path.exists() {
            let backup = path.with_extension("toml.bak");
            // Clean up any stale backup from a prior interrupted save.
            let _ = fs::remove_file(&backup);
            match fs::rename(path, &backup) {
                Ok(()) => {
                    if let Err(error) = fs::rename(temp_path, path) {
                        // Restore the backup so the original config is preserved.
                        let _ = fs::rename(&backup, path);
                        return Err(error);
                    }
                    let _ = fs::remove_file(&backup);
                    return Ok(());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // File vanished between exists() and rename — proceed.
                }
                Err(error) => return Err(error),
            }
        }
    }

    fs::rename(temp_path, path)
}
*/

#[cfg(test)]
mod tests {
    /*
    use std::{fs, path::Path};
    */

    use rusqlite::Connection;

    use super::{
        load_config_dictionary, normalize_remote_endpoint, save_config_dictionary, AppConfig,
        ClaudeCodeSessionMode, ComputerControlAuthorizationConfig, RemoteDeviceConfig, CONFIG_KEYS,
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

        assert_eq!(CONFIG_KEYS.len(), 34);
        assert_eq!(config.general.theme, restored.general.theme);
        assert_eq!(config.general.locale, restored.general.locale);
        assert_eq!(config.ui.sidebar_width, restored.ui.sidebar_width);
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
        assert_eq!(row_count, 34);
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
        assert_eq!(row_count, 34);
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
}
