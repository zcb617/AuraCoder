use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Mutex, MutexGuard, OnceLock},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::runtime_env;

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
        let path = Self::path();

        if !path.exists() {
            let config = Self::default();
            config.save_unlocked()?;
            return Ok(config);
        }

        let raw = fs::read_to_string(&path)?;
        let config = toml::from_str::<Self>(&raw).unwrap_or_default();

        /*
        let mut config = toml::from_str::<Self>(&raw).unwrap_or_default();

        // Existing installations can have a config file created before
        // `display_scale` existed. Reading a missing field falls back to 100, but
        // the config must also be migrated so the field is explicitly persisted.
        let persisted_display_scale = toml::from_str::<toml::Value>(&raw)
            .ok()
            .and_then(|value| {
                value
                    .get("ui")
                    .and_then(toml::Value::as_table)
                    .and_then(|ui| ui.get("display_scale"))
                    .and_then(toml::Value::as_integer)
            })
            .and_then(|value| u32::try_from(value).ok());
        let normalized_display_scale = config.display_scale();
        if persisted_display_scale != Some(normalized_display_scale) {
            config.ui.display_scale = normalized_display_scale;
            config.save_unlocked()?;
        }
        */

        Ok(config)
    }

    fn save_unlocked(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let raw = toml::to_string_pretty(self)?;
        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, raw)?;
        replace_file(&temp_path, &path)?;
        Ok(())
    }

    pub fn path() -> PathBuf {
        runtime_env::config_path()
    }
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

fn config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_config() -> anyhow::Result<MutexGuard<'static, ()>> {
    config_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("config lock poisoned"))
}

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

#[cfg(test)]
mod tests {
    use super::{AppConfig, ClaudeCodeSessionMode};

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
}
