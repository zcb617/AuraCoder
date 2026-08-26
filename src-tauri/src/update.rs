use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const STATE_FILE: &str = "pending-update.json";
const STATE_VERSION: u32 = 1;
const UPDATE_DOWNLOAD_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
// 记录当前 Tauri 构建类型，供本地开发更新能力判断使用。
const PANES_BUILD_TYPE: &str = env!("PANES_BUILD_TYPE");
// Rust 当前版本不支持在常量初始化中执行字符串相等比较，开发版判断保留在业务分支内完成。
// const IS_DEVELOPMENT_BUILD: bool = PANES_BUILD_TYPE == "development";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProcessState {
    pub phase: UpdatePhase,
    pub version: Option<String>,
    pub source: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
}

/// 更新安装命令返回给前端的重启方式。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdateInstallRestartMode {
    /// 使用 Tauri 原生 relaunch 重启当前进程。
    #[serde(rename = "tauriRelaunch")]
    TauriRelaunch,
    /// 由独立 macOS updater 接管关闭、替换和启动。
    #[serde(rename = "externalUpdater")]
    ExternalUpdater,
}

/// 更新安装命令返回的跨平台结果。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstallResult {
    /// 当前平台后续需要采用的重启方式。
    pub restart_mode: UpdateInstallRestartMode,
}

#[cfg(target_os = "macos")]
/// macOS updater 读取的 JSON 安装任务。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MacosUpdateJob {
    /// 当前任务模式。
    mode: MacosUpdateMode,
    /// 当前旧 AuraCoder 进程 PID。
    old_process_id: i32,
    /// 待安装归档绝对路径，relaunch 模式为空。
    archive_path: Option<String>,
    /// 当前 AuraCoder.app 绝对路径。
    target_app_path: String,
    /// 需要校验和记录的目标版本。
    expected_version: String,
    /// 需要校验的应用标识。
    expected_bundle_identifier: String,
    /// updater 就绪标记路径。
    ready_path: String,
    /// 更新完成标记路径。
    completion_path: String,
    /// updater 日志路径。
    log_path: String,
}

#[cfg(target_os = "macos")]
/// macOS updater 支持的任务模式。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum MacosUpdateMode {
    /// 解压、校验和替换应用。
    Install,
    /// 只关闭旧进程并通过 LaunchServices 重启。
    Relaunch,
}

#[cfg(target_os = "macos")]
const MACOS_UPDATER_BYTES: &[u8] = include_bytes!(env!("PANES_MACOS_UPDATER_PATH"));

impl Default for UpdateProcessState {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            version: None,
            source: None,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedUpdate {
    state_version: u32,
    version: String,
    source: String,
    file_name: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

struct RuntimeState {
    state: UpdateProcessState,
    candidate: Option<Update>,
}

pub struct UpdateManager {
    runtime: Mutex<RuntimeState>,
}

impl Default for UpdateManager {
    fn default() -> Self {
        Self {
            runtime: Mutex::new(RuntimeState {
                state: UpdateProcessState::default(),
                candidate: None,
            }),
        }
    }
}

impl UpdateManager {
    pub fn state(&self) -> UpdateProcessState {
        self.runtime
            .lock()
            .expect("update manager lock poisoned")
            .state
            .clone()
    }

    pub fn set_state(&self, state: UpdateProcessState) {
        self.runtime
            .lock()
            .expect("update manager lock poisoned")
            .state = state;
    }

    fn set_candidate(&self, candidate: Option<Update>) {
        self.runtime
            .lock()
            .expect("update manager lock poisoned")
            .candidate = candidate;
    }

    fn candidate(&self) -> Option<Update> {
        self.runtime
            .lock()
            .expect("update manager lock poisoned")
            .candidate
            .clone()
    }

    fn set_progress(&self, downloaded_bytes: u64, total_bytes: Option<u64>) {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        runtime.state.phase = UpdatePhase::Downloading;
        runtime.state.downloaded_bytes = downloaded_bytes;
        runtime.state.total_bytes = total_bytes;
        runtime.state.error = None;
    }

    fn set_error(
        &self,
        version: Option<String>,
        source: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        error: String,
    ) {
        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Error,
            version,
            source: Some(source.to_string()),
            downloaded_bytes,
            total_bytes,
            error: Some(error),
        });
    }

    pub fn restore(&self, app: &AppHandle) -> Result<UpdateProcessState, String> {
        let current = self.state();
        if matches!(
            current.phase,
            UpdatePhase::Checking | UpdatePhase::Downloading | UpdatePhase::Installing
        ) || (current.phase == UpdatePhase::Available && self.candidate().is_some())
        {
            return Ok(current);
        }

        let saved = match read_saved_update(app) {
            Ok(saved) => saved,
            Err(error) => {
                log::warn!("failed to read saved update state: {error}");
                let _ = clear_saved_update(app, None);
                None
            }
        };

        let Some(saved) = saved else {
            self.set_candidate(None);
            self.set_state(UpdateProcessState::default());
            return Ok(UpdateProcessState::default());
        };

        let current_version = app.package_info().version.to_string();
        #[cfg(target_os = "macos")]
        if compare_versions(&current_version, &saved.version) != Ordering::Less {
            let completion_marker = macos_completion_marker_path(app, &saved.version)?;
            if completion_marker.is_file() {
                fs::remove_file(&completion_marker).map_err(|error| error.to_string())?;
                clear_saved_update(app, Some(&saved))?;
                self.set_candidate(None);
                self.set_state(UpdateProcessState::default());
                return Ok(UpdateProcessState::default());
            }
            if compare_versions(&current_version, &saved.version) == Ordering::Equal {
                let target_app_path = macos_target_app_path()?;
                let updates_directory = updates_dir(app)?;
                let pid = current_process_id();
                let unique_id = macos_job_unique_id()?;
                let ready_path = updates_directory.join(format!("macos-relaunch-ready-{pid}-{unique_id}"));
                let log_path = updates_directory.join(format!("macos-relaunch-{pid}-{unique_id}.log"));
                let job = MacosUpdateJob {
                    mode: MacosUpdateMode::Relaunch,
                    old_process_id: pid,
                    archive_path: None,
                    target_app_path: target_app_path.to_string_lossy().into_owned(),
                    expected_version: saved.version.clone(),
                    // 原固定值 com.auracoder.app 已停用：更新器必须校验当前 Tauri 配置的实际标识，以区分正式版和开发版。
                    expected_bundle_identifier: app.config().identifier.clone(),
                    ready_path: ready_path.to_string_lossy().into_owned(),
                    completion_path: completion_marker.to_string_lossy().into_owned(),
                    log_path: log_path.to_string_lossy().into_owned(),
                };
                self.set_state(UpdateProcessState {
                    phase: UpdatePhase::Installing,
                    version: Some(saved.version.clone()),
                    source: Some(saved.source.clone()),
                    downloaded_bytes: saved.downloaded_bytes,
                    total_bytes: saved.total_bytes,
                    error: None,
                });
                if let Err(error) = spawn_macos_updater(app, job) {
                    self.set_error(
                        Some(saved.version),
                        &saved.source,
                        saved.downloaded_bytes,
                        saved.total_bytes,
                        error.clone(),
                    );
                    return Err(error);
                }
                return Ok(self.state());
            }
        }
        if compare_versions(&current_version, &saved.version) != Ordering::Less {
            clear_saved_update(app, Some(&saved))?;
            self.set_candidate(None);
            self.set_state(UpdateProcessState::default());
            return Ok(UpdateProcessState::default());
        }

        if !saved_file_path(app, &saved)?.is_file() {
            clear_saved_update(app, Some(&saved))?;
            self.set_candidate(None);
            self.set_state(UpdateProcessState::default());
            return Ok(UpdateProcessState::default());
        }

        let state = UpdateProcessState {
            phase: UpdatePhase::Downloaded,
            version: Some(saved.version),
            source: Some(saved.source),
            downloaded_bytes: saved.downloaded_bytes,
            total_bytes: saved.total_bytes,
            error: None,
        };
        self.set_candidate(None);
        self.set_state(state.clone());
        Ok(state)
    }

    pub async fn check_for_update(
        &self,
        app: &AppHandle,
        source: &str,
    ) -> Result<UpdateProcessState, String> {
        let current = self.state();
        if matches!(
            current.phase,
            UpdatePhase::Checking
                | UpdatePhase::Downloading
                | UpdatePhase::Downloaded
                | UpdatePhase::Installing
        ) || (current.phase == UpdatePhase::Available && self.candidate().is_some())
        {
            return Ok(current);
        }

        let restored = self.restore(app)?;
        if restored.phase == UpdatePhase::Downloaded {
            return Ok(restored);
        }

        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Checking,
            version: None,
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        });

        let updater = match app
            .updater_builder()
            .configure_client(|client| client.read_timeout(UPDATE_DOWNLOAD_INACTIVITY_TIMEOUT))
            .build()
        {
            Ok(updater) => updater,
            Err(error) => {
                let message = error.to_string();
                self.set_error(None, source, 0, None, message.clone());
                return Err(message);
            }
        };
        let update = match updater.check().await {
            Ok(update) => update,
            Err(error) => {
                let message = error.to_string();
                self.set_error(None, source, 0, None, message.clone());
                return Err(message);
            }
        };

        let Some(update) = update else {
            let state = UpdateProcessState::default();
            self.set_candidate(None);
            self.set_state(state.clone());
            return Ok(state);
        };

        let state = UpdateProcessState {
            phase: UpdatePhase::Available,
            version: Some(update.version.clone()),
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        };
        self.set_candidate(Some(update));
        self.set_state(state.clone());
        Ok(state)
    }

    pub async fn download_update(
        &self,
        app: &AppHandle,
        source: &str,
    ) -> Result<UpdateProcessState, String> {
        let current_state = self.state();
        if matches!(
            current_state.phase,
            UpdatePhase::Downloading | UpdatePhase::Downloaded | UpdatePhase::Installing
        ) {
            return Ok(current_state);
        }

        let update = self
            .candidate()
            .ok_or_else(|| "没有可下载的更新版本".to_string())?;
        let version = update.version.clone();
        let suffix = package_suffix(update.download_url.path());
        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Downloading,
            version: Some(version.clone()),
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        });

        let mut downloaded_bytes = 0_u64;
        let manager = self;
        let progress_app = app.clone();
        let bytes = match update
            .download(
                |chunk_length, content_length| {
                    downloaded_bytes = downloaded_bytes.saturating_add(chunk_length as u64);
                    manager.set_progress(downloaded_bytes, content_length);
                    let _ = progress_app.emit("update-download-progress", manager.state());
                },
                || {},
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                let message = error.to_string();
                self.set_error(
                    Some(version),
                    source,
                    downloaded_bytes,
                    self.state().total_bytes,
                    message.clone(),
                );
                return Err(message);
            }
        };

        let saved = match save_downloaded_update(
            app,
            &version,
            source,
            &suffix,
            &bytes,
            downloaded_bytes,
            self.state().total_bytes,
        ) {
            Ok(saved) => saved,
            Err(error) => {
                self.set_error(
                    Some(version),
                    source,
                    downloaded_bytes,
                    self.state().total_bytes,
                    error.clone(),
                );
                return Err(error);
            }
        };
        let state = UpdateProcessState {
            phase: UpdatePhase::Downloaded,
            version: Some(saved.version),
            source: Some(saved.source),
            downloaded_bytes: saved.downloaded_bytes,
            total_bytes: saved.total_bytes,
            error: None,
        };
        self.set_state(state.clone());
        Ok(state)
    }

    /// 安装已经完成下载的更新，并返回当前平台的重启方式。
    pub fn install_downloaded_update(&self, app: &AppHandle) -> Result<UpdateInstallResult, String> {
        let saved = read_saved_update(app)?.ok_or_else(|| "没有已下载完成的更新".to_string())?;
        let file_path = saved_file_path(app, &saved)?;
        let file_size = fs::metadata(&file_path)
            .map_err(|error| error.to_string())?
            .len();
        if file_size == 0 {
            return Err("已下载的更新文件为空".to_string());
        }

        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Installing,
            version: Some(saved.version.clone()),
            source: Some(saved.source.clone()),
            downloaded_bytes: saved.downloaded_bytes,
            total_bytes: saved.total_bytes,
            error: None,
        });

        #[cfg(target_os = "macos")]
        {
            let target_app_path = match macos_target_app_path() {
                Ok(path) => path,
                Err(error) => {
                    self.set_error(
                        Some(saved.version.clone()),
                        &saved.source,
                        saved.downloaded_bytes,
                        saved.total_bytes,
                        error.clone(),
                    );
                    return Err(error);
                }
            };
            let completion_path = match macos_completion_marker_path(app, &saved.version) {
                Ok(path) => path,
                Err(error) => {
                    self.set_error(
                        Some(saved.version.clone()),
                        &saved.source,
                        saved.downloaded_bytes,
                        saved.total_bytes,
                        error.clone(),
                    );
                    return Err(error);
                }
            };
            let pid = current_process_id();
            let updates_directory = updates_dir(app)?;
            let unique_id = macos_job_unique_id()?;
            let ready_path = updates_directory.join(format!("macos-install-ready-{pid}-{unique_id}"));
            let log_path = updates_directory.join(format!("macos-install-{pid}-{unique_id}.log"));
            let job = MacosUpdateJob {
                mode: MacosUpdateMode::Install,
                old_process_id: pid,
                archive_path: Some(file_path.to_string_lossy().into_owned()),
                target_app_path: target_app_path.to_string_lossy().into_owned(),
                expected_version: saved.version.clone(),
                // 原固定值 com.auracoder.app 已停用：更新器必须校验当前 Tauri 配置的实际标识，以区分正式版和开发版。
                expected_bundle_identifier: app.config().identifier.clone(),
                ready_path: ready_path.to_string_lossy().into_owned(),
                completion_path: completion_path.to_string_lossy().into_owned(),
                log_path: log_path.to_string_lossy().into_owned(),
            };
            if let Err(error) = spawn_macos_updater(app, job) {
                self.set_error(
                    Some(saved.version),
                    &saved.source,
                    saved.downloaded_bytes,
                    saved.total_bytes,
                    error.clone(),
                );
                return Err(error);
            }
            return Ok(UpdateInstallResult {
                restart_mode: UpdateInstallRestartMode::ExternalUpdater,
            });
        }

        #[cfg(not(target_os = "macos"))]
        let bytes = fs::read(&file_path).map_err(|error| error.to_string())?;
        #[cfg(not(target_os = "macos"))]
        let result = if let Some(update) = self.candidate() {
            update.install(bytes).map_err(|error| error.to_string())
        } else {
            install_saved_file(&file_path, &saved.file_name)
        };
        #[cfg(not(target_os = "macos"))]
        if let Err(error) = result {
            self.set_error(
                Some(saved.version),
                &saved.source,
                saved.downloaded_bytes,
                saved.total_bytes,
                error.clone(),
            );
            return Err(error);
        }
        #[cfg(not(target_os = "macos"))]
        return Ok(UpdateInstallResult {
            restart_mode: UpdateInstallRestartMode::TauriRelaunch,
        });

        #[allow(unreachable_code)]
        Err("当前平台不支持更新安装".to_string())
    }

    /// 在开发版 macOS 中准备用户选择的本地更新包，并复用现有安装流程。
    pub fn prepare_local_update_for_development(
        &self,
        app: &AppHandle,
        archive_path: &str,
    ) -> Result<UpdateProcessState, String> {
        if PANES_BUILD_TYPE != "development" {
            return Err("本地更新测试仅开发版可用".to_string());
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = app;
            let _ = archive_path;
            return Err("本地更新测试仅 macOS 开发版可用".to_string());
        }

        #[cfg(target_os = "macos")]
        {
            let archive = Path::new(archive_path);
            let metadata = fs::metadata(archive).map_err(|error| {
                format!("本地更新包不存在或无法读取: {archive_path}: {error}")
            })?;
            if !metadata.is_file() {
                return Err(format!("本地更新包不是普通文件: {archive_path}"));
            }
            let file_name = archive
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("本地更新包文件名无效: {archive_path}"))?;
            if !file_name.ends_with(".app.tar.gz") {
                return Err(format!(
                    "本地更新包格式不受支持，需要 .app.tar.gz: {archive_path}"
                ));
            }
            let bytes = fs::read(archive).map_err(|error| {
                format!("无法读取本地更新包 {archive_path}: {error}")
            })?;
            if bytes.is_empty() {
                return Err(format!("本地更新包为空: {archive_path}"));
            }

            let file_size = bytes.len() as u64;
            let version = app.package_info().version.to_string();
            let saved = save_downloaded_update(
                app,
                &version,
                "manual",
                ".app.tar.gz",
                &bytes,
                file_size,
                Some(file_size),
            )?;
            self.set_candidate(None);
            let state = UpdateProcessState {
                phase: UpdatePhase::Downloaded,
                version: Some(saved.version),
                source: Some(saved.source),
                downloaded_bytes: saved.downloaded_bytes,
                total_bytes: saved.total_bytes,
                error: None,
            };
            self.set_state(state.clone());
            Ok(state)
        }
    }

    pub fn is_downloaded(&self, app: &AppHandle) -> Result<bool, String> {
        if self.state().phase == UpdatePhase::Downloaded {
            return Ok(true);
        }
        Ok(self.restore(app)?.phase == UpdatePhase::Downloaded)
    }
}

fn updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("updates"))
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
/// 返回当前 AuraCoder 进程 PID，供 updater 正常终止旧程序。
fn current_process_id() -> i32 {
    std::process::id() as i32
}

#[cfg(target_os = "macos")]
/// 生成当前更新任务使用的纳秒级唯一标识，避免 ready 和日志文件复用。
fn macos_job_unique_id() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
/// 严格从当前可执行文件路径解析并校验所在的 AuraCoder.app。
fn macos_target_app_path() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let binary_name = current
        .file_name()
        .ok_or_else(|| "当前 macOS 可执行文件路径无效".to_string())?;
    let macos_directory = current
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "MacOS"))
        .ok_or_else(|| "当前可执行文件不在 .app/Contents/MacOS 目录".to_string())?;
    let contents_directory = macos_directory
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "Contents"))
        .ok_or_else(|| "当前可执行文件缺少 Contents 目录".to_string())?;
    let app_directory = contents_directory
        .parent()
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or_else(|| "当前可执行文件缺少 .app 应用目录".to_string())?;
    let target_binary = macos_directory.join(binary_name);
    if !target_binary.is_file() || !app_directory.is_dir() {
        return Err("当前 macOS 应用路径不存在".to_string());
    }
    Ok(app_directory.to_path_buf())
}

#[cfg(target_os = "macos")]
/// 返回按目标版本命名且仅位于 updates 目录中的完成标记路径。
fn macos_completion_marker_path(app: &AppHandle, version: &str) -> Result<PathBuf, String> {
    let safe_version = version
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(updates_dir(app)?.join(format!("completed-{safe_version}.marker")))
}

#[cfg(target_os = "macos")]
/// 写入 job、启动独立 updater 并等待其 ready 文件确认窗口已显示。
fn spawn_macos_updater(app: &AppHandle, job: MacosUpdateJob) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    let updates_directory = updates_dir(app)?;
    fs::create_dir_all(&updates_directory).map_err(|error| error.to_string())?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let helper_directory = updates_directory.join(format!("macos-updater-{}-{unique}", current_process_id()));
    fs::create_dir_all(&helper_directory).map_err(|error| error.to_string())?;
    let helper_path = helper_directory.join("AuraCoderUpdater");
    fs::write(&helper_path, MACOS_UPDATER_BYTES).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(&helper_path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&helper_path, permissions).map_err(|error| error.to_string())?;
    let ready_path = PathBuf::from(&job.ready_path);
    let log_path = PathBuf::from(&job.log_path);
    if ready_path.exists() || log_path.exists() {
        return Err(format!(
            "macOS updater job 文件已存在，拒绝复用 ready/log 路径: {}",
            helper_directory.display()
        ));
    }
    let job_path = helper_directory.join("job.json");
    let job_bytes = serde_json::to_vec_pretty(&job).map_err(|error| error.to_string())?;
    fs::write(&job_path, job_bytes).map_err(|error| error.to_string())?;

    let mut child = Command::new(&helper_path)
        .arg("--job")
        .arg(&job_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动 macOS updater 失败，日志路径 {}: {error}", job.log_path))?;
    let helper_pid = child.id();
    log::info!(
        "macOS updater started mode={:?} old_pid={} archive={:?} target={} job={} ready={} completion={} helper_pid={}",
        job.mode,
        job.old_process_id,
        job.archive_path,
        job.target_app_path,
        job_path.display(),
        job.ready_path,
        job.completion_path,
        helper_pid
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if ready_path.is_file() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "macOS updater 提前退出，状态 {status}，日志路径 {}",
                job.log_path
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "macOS updater 等待 ready 超时，日志路径 {}",
        job.log_path
    ))
}

fn state_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(updates_dir(app)?.join(STATE_FILE))
}

fn saved_file_path(app: &AppHandle, saved: &SavedUpdate) -> Result<PathBuf, String> {
    let file_name = Path::new(&saved.file_name)
        .file_name()
        .ok_or_else(|| "更新文件名无效".to_string())?;
    Ok(updates_dir(app)?.join(file_name))
}

fn read_saved_update(app: &AppHandle) -> Result<Option<SavedUpdate>, String> {
    let path = state_path(app)?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let saved = serde_json::from_str::<SavedUpdate>(&text).map_err(|error| error.to_string())?;
    if saved.state_version != STATE_VERSION {
        return Ok(None);
    }
    Ok(Some(saved))
}

fn save_downloaded_update(
    app: &AppHandle,
    version: &str,
    source: &str,
    suffix: &str,
    bytes: &[u8],
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
) -> Result<SavedUpdate, String> {
    let directory = updates_dir(app)?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let file_name = format!("pending-update{suffix}");
    let file_path = directory.join(&file_name);
    atomic_write(&file_path, bytes)?;

    let saved = SavedUpdate {
        state_version: STATE_VERSION,
        version: version.to_string(),
        source: source.to_string(),
        file_name,
        downloaded_bytes: if downloaded_bytes == 0 {
            bytes.len() as u64
        } else {
            downloaded_bytes
        },
        total_bytes,
    };
    let text = serde_json::to_vec_pretty(&saved).map_err(|error| error.to_string())?;
    atomic_write(&state_path(app)?, &text)?;
    Ok(saved)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "更新文件名无效".to_string())?;
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn clear_saved_update(app: &AppHandle, saved: Option<&SavedUpdate>) -> Result<(), String> {
    if let Some(saved) = saved {
        if let Ok(path) = saved_file_path(app, saved) {
            let _ = fs::remove_file(path);
        }
    }
    if let Ok(directory) = updates_dir(app) {
        for suffix in [
            ".app.tar.gz",
            ".appimage",
            ".exe",
            ".msi",
            ".deb",
            ".rpm",
            ".bin",
        ] {
            let _ = fs::remove_file(directory.join(format!("pending-update{suffix}")));
        }
    }
    let _ = fs::remove_file(state_path(app)?);
    Ok(())
}

fn package_suffix(path: &str) -> String {
    let path = path.to_ascii_lowercase();
    [".app.tar.gz", ".appimage", ".exe", ".msi", ".deb", ".rpm"]
        .iter()
        .find(|suffix| path.ends_with(**suffix))
        .unwrap_or(&".bin")
        .to_string()
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left = version_parts(left);
    let right = version_parts(right);
    let length = left.len().max(right.len());
    (0..length)
        .map(|index| {
            (
                left.get(index).copied().unwrap_or(0),
                right.get(index).copied().unwrap_or(0),
            )
        })
        .find_map(|(left, right)| match left.cmp(&right) {
            Ordering::Equal => None,
            ordering => Some(ordering),
        })
        .unwrap_or(Ordering::Equal)
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn install_saved_file(path: &Path, file_name: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if file_name.ends_with(".exe") {
            let mut installer = Command::new(path);
            installer.args(["/P", "/R", "/UPDATE", "/ARGS"]);
            installer.args(std::env::args_os().skip(1));
            installer.spawn().map_err(|error| error.to_string())?;
        } else if file_name.ends_with(".msi") {
            let mut installer = Command::new("msiexec.exe");
            installer.args([
                "/i",
                path.to_string_lossy().as_ref(),
                "/passive",
                "/promptrestart",
                "AUTOLAUNCHAPP=True",
            ]);
            installer.spawn().map_err(|error| error.to_string())?;
        } else {
            return Err("当前 Windows 更新包格式不受支持".to_string());
        }
        std::process::exit(0);
    }

    #[cfg(target_os = "macos")]
    {
        return install_macos_file(path);
    }

    #[cfg(target_os = "linux")]
    {
        if file_name.ends_with(".deb") {
            Command::new("pkexec")
                .args(["dpkg", "-i", path.to_string_lossy().as_ref()])
                .spawn()
                .map_err(|error| error.to_string())?;
            std::process::exit(0);
        }
        if file_name.ends_with(".rpm") {
            Command::new("pkexec")
                .args(["rpm", "-U", path.to_string_lossy().as_ref()])
                .spawn()
                .map_err(|error| error.to_string())?;
            std::process::exit(0);
        }
        let current = std::env::current_exe().map_err(|error| error.to_string())?;
        fs::copy(path, current).map_err(|error| error.to_string())?;
        std::process::exit(0);
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持持久化更新安装".to_string())
}

#[cfg(target_os = "macos")]
fn install_macos_file(path: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let app_dir = current
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| "无法定位当前 macOS 应用目录".to_string())?
        .to_path_buf();
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let mut decoder = GzDecoder::new(bytes.as_slice());
    let mut tar_bytes = Vec::new();
    decoder
        .read_to_end(&mut tar_bytes)
        .map_err(|error| error.to_string())?;
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    tar::Archive::new(tar_bytes.as_slice())
        .unpack(temporary.path())
        .map_err(|error| error.to_string())?;
    let replacement = find_app_dir(temporary.path())
        .ok_or_else(|| "更新文件中没有找到 macOS 应用".to_string())?;
    let backup = app_dir.with_extension("old");
    let _ = fs::remove_dir_all(&backup);
    fs::rename(&app_dir, &backup).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&replacement, &app_dir) {
        let _ = fs::rename(&backup, &app_dir);
        return Err(error.to_string());
    }
    let _ = fs::remove_dir_all(backup);
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn find_app_dir(root: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.extension().is_some_and(|extension| extension == "app") {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_app_dir(&path) {
                return Some(found);
            }
        }
    }
    None
}
