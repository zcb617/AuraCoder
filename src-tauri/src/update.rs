use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;

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

/// 更新失败所属的业务阶段，用于区分自动重试下载和安装失败。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateErrorStage {
    /// 下载、签名校验或保存更新文件失败。
    Download,
    /// 启动或执行已保存更新安装失败。
    Install,
    /// 检查更新或恢复已保存状态失败。
    Check,
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
    /// 当前错误所属的业务阶段，非错误状态保持为空。
    pub error_stage: Option<UpdateErrorStage>,
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
            error_stage: None,
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

/// 表示下载操作是否已取得运行时状态的独占入口。
enum DownloadReservation {
    /// 当前调用已取得下载资格，并持有需要下载的更新候选。
    Started(Update),
    /// 另一调用已经进入下载或后续阶段，应直接返回已有状态。
    Existing(UpdateProcessState),
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

    /// 在检查更新网络请求前原子复核状态并抢占 Checking 阶段，避免并发请求重复访问更新服务。
    fn reserve_check(&self, source: &str) -> Option<UpdateProcessState> {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        let current = runtime.state.clone();
        if matches!(
            current.phase,
            UpdatePhase::Checking
                | UpdatePhase::Downloading
                | UpdatePhase::Downloaded
                | UpdatePhase::Installing
        ) || (current.phase == UpdatePhase::Available && runtime.candidate.is_some())
        {
            return Some(current);
        }

        runtime.state = UpdateProcessState {
            phase: UpdatePhase::Checking,
            version: None,
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
            error_stage: None,
        };
        None
    }

    /// 在下载更新网络请求前原子读取候选并抢占 Downloading 阶段，避免并发下载复用临时文件。
    fn reserve_download(&self, source: &str) -> Result<DownloadReservation, String> {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        let current = runtime.state.clone();
        if matches!(
            current.phase,
            UpdatePhase::Downloading | UpdatePhase::Downloaded | UpdatePhase::Installing
        ) {
            return Ok(DownloadReservation::Existing(current));
        }

        let update = runtime
            .candidate
            .clone()
            .ok_or_else(|| "没有可下载的更新版本".to_string())?;
        let version = update.version.clone();
        runtime.state = UpdateProcessState {
            phase: UpdatePhase::Downloading,
            version: Some(version),
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
            error_stage: None,
        };
        Ok(DownloadReservation::Started(update))
    }

    /// 在检查结果提交时原子更新候选和对应状态，避免下载观察到不匹配的中间状态。
    fn commit_check_result(&self, candidate: Option<Update>, state: UpdateProcessState) {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        runtime.candidate = candidate;
        runtime.state = state;
    }

    fn set_progress(&self, downloaded_bytes: u64, total_bytes: Option<u64>) {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        runtime.state.phase = UpdatePhase::Downloading;
        runtime.state.downloaded_bytes = downloaded_bytes;
        runtime.state.total_bytes = total_bytes;
        runtime.state.error = None;
        runtime.state.error_stage = None;
    }

    /// 记录更新流程失败及其业务阶段，供前端决定是否允许自动重试。
    fn set_error(
        &self,
        version: Option<String>,
        source: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        error_stage: UpdateErrorStage,
        error: String,
    ) {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        runtime.candidate = None;
        runtime.state = UpdateProcessState {
            phase: UpdatePhase::Error,
            version,
            source: Some(source.to_string()),
            downloaded_bytes,
            total_bytes,
            error: Some(error),
            error_stage: Some(error_stage),
        };
    }

    pub fn restore(&self, app: &AppHandle) -> Result<UpdateProcessState, String> {
        let mut runtime = self.runtime.lock().expect("update manager lock poisoned");
        let current = runtime.state.clone();
        if matches!(
            current.phase,
            UpdatePhase::Checking | UpdatePhase::Downloading | UpdatePhase::Installing
        ) || (current.phase == UpdatePhase::Available && runtime.candidate.is_some())
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
            let state = UpdateProcessState::default();
            runtime.candidate = None;
            runtime.state = state.clone();
            return Ok(state);
        };

        let current_version = app.package_info().version.to_string();
        #[cfg(target_os = "macos")]
        if compare_versions(&current_version, &saved.version) != Ordering::Less {
            let completion_marker = macos_completion_marker_path(app, &saved.version)?;
            if completion_marker.is_file() {
                fs::remove_file(&completion_marker).map_err(|error| error.to_string())?;
                clear_saved_update(app, Some(&saved))?;
                let state = UpdateProcessState::default();
                runtime.candidate = None;
                runtime.state = state.clone();
                return Ok(state);
            }
            if compare_versions(&current_version, &saved.version) == Ordering::Equal {
                let target_app_path = macos_target_app_path()?;
                let updates_directory = updates_dir(app)?;
                let pid = current_process_id();
                let unique_id = macos_job_unique_id()?;
                let ready_path =
                    updates_directory.join(format!("macos-relaunch-ready-{pid}-{unique_id}"));
                let log_path =
                    updates_directory.join(format!("macos-relaunch-{pid}-{unique_id}.log"));
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
                runtime.state = UpdateProcessState {
                    phase: UpdatePhase::Installing,
                    version: Some(saved.version.clone()),
                    source: Some(saved.source.clone()),
                    downloaded_bytes: saved.downloaded_bytes,
                    total_bytes: saved.total_bytes,
                    error: None,
                    error_stage: None,
                };
                if let Err(error) = spawn_macos_updater(app, job) {
                    runtime.state = UpdateProcessState {
                        phase: UpdatePhase::Error,
                        version: Some(saved.version),
                        source: Some(saved.source.clone()),
                        downloaded_bytes: saved.downloaded_bytes,
                        total_bytes: saved.total_bytes,
                        error: Some(error.clone()),
                        error_stage: Some(UpdateErrorStage::Install),
                    };
                    return Err(error);
                }
                return Ok(runtime.state.clone());
            }
        }
        if compare_versions(&current_version, &saved.version) != Ordering::Less {
            clear_saved_update(app, Some(&saved))?;
            let state = UpdateProcessState::default();
            runtime.candidate = None;
            runtime.state = state.clone();
            return Ok(state);
        }

        if !saved_file_path(app, &saved)?.is_file() {
            clear_saved_update(app, Some(&saved))?;
            let state = UpdateProcessState::default();
            runtime.candidate = None;
            runtime.state = state.clone();
            return Ok(state);
        }

        let state = UpdateProcessState {
            phase: UpdatePhase::Downloaded,
            version: Some(saved.version),
            source: Some(saved.source),
            downloaded_bytes: saved.downloaded_bytes,
            total_bytes: saved.total_bytes,
            error: None,
            error_stage: None,
        };
        runtime.candidate = None;
        runtime.state = state.clone();
        Ok(state)
    }

    pub async fn check_for_update(
        &self,
        app: &AppHandle,
        source: &str,
    ) -> Result<UpdateProcessState, String> {
        let restored = self.restore(app)?;
        if restored.phase == UpdatePhase::Downloaded {
            return Ok(restored);
        }

        if let Some(state) = self.reserve_check(source) {
            return Ok(state);
        }

        let updater = match app
            .updater_builder()
            .configure_client(|client| client.read_timeout(UPDATE_DOWNLOAD_INACTIVITY_TIMEOUT))
            .build()
        {
            Ok(updater) => updater,
            Err(error) => {
                let message = error.to_string();
                self.set_error(
                    None,
                    source,
                    0,
                    None,
                    UpdateErrorStage::Check,
                    message.clone(),
                );
                return Err(message);
            }
        };
        let update = match updater.check().await {
            Ok(update) => update,
            Err(error) => {
                let message = error.to_string();
                self.set_error(
                    None,
                    source,
                    0,
                    None,
                    UpdateErrorStage::Check,
                    message.clone(),
                );
                return Err(message);
            }
        };

        let Some(update) = update else {
            let state = UpdateProcessState::default();
            self.commit_check_result(None, state.clone());
            return Ok(state);
        };

        let state = UpdateProcessState {
            phase: UpdatePhase::Available,
            version: Some(update.version.clone()),
            source: Some(source.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
            error_stage: None,
        };
        self.commit_check_result(Some(update.clone()), state.clone());
        Ok(state)
    }

    pub async fn download_update(
        &self,
        app: &AppHandle,
        source: &str,
    ) -> Result<UpdateProcessState, String> {
        let update = match self.reserve_download(source)? {
            DownloadReservation::Started(update) => update,
            DownloadReservation::Existing(state) => return Ok(state),
        };
        let version = update.version.clone();
        let suffix = package_suffix(update.download_url.path());

        let mut downloaded_bytes = 0_u64;
        let manager = self;
        let progress_app = app.clone();
        let bytes = match update
            .download(
                |chunk_length, content_length| {
                    downloaded_bytes = downloaded_bytes.saturating_add(chunk_length as u64);
                    let visible_downloaded_bytes = match content_length {
                        Some(total_bytes) if total_bytes > 0 => {
                            downloaded_bytes.min(total_bytes.saturating_sub(1))
                        }
                        Some(_) => 0,
                        None => downloaded_bytes,
                    };
                    manager.set_progress(visible_downloaded_bytes, content_length);
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
                    UpdateErrorStage::Download,
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
                    UpdateErrorStage::Download,
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
            error_stage: None,
        };
        self.set_state(state.clone());
        let _ = app.emit("update-download-progress", state.clone());
        Ok(state)
    }

    /// 安装已经完成下载的更新，并返回当前平台的重启方式。
    pub fn install_downloaded_update(
        &self,
        app: &AppHandle,
    ) -> Result<UpdateInstallResult, String> {
        let previous_state = self.state();
        let fallback_source = previous_state.source.as_deref().unwrap_or("unknown");
        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Installing,
            version: previous_state.version.clone(),
            source: previous_state.source.clone(),
            downloaded_bytes: previous_state.downloaded_bytes,
            total_bytes: previous_state.total_bytes,
            error: None,
            error_stage: None,
        });
        let saved = match read_saved_update(app) {
            Ok(Some(saved)) => saved,
            Ok(None) => {
                let error = "没有已下载完成的更新".to_string();
                self.set_error(
                    previous_state.version.clone(),
                    fallback_source,
                    previous_state.downloaded_bytes,
                    previous_state.total_bytes,
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
            Err(error) => {
                self.set_error(
                    previous_state.version.clone(),
                    fallback_source,
                    previous_state.downloaded_bytes,
                    previous_state.total_bytes,
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
        };

        self.set_state(UpdateProcessState {
            phase: UpdatePhase::Installing,
            version: Some(saved.version.clone()),
            source: Some(saved.source.clone()),
            downloaded_bytes: saved.downloaded_bytes,
            total_bytes: saved.total_bytes,
            error: None,
            error_stage: None,
        });

        let file_path = match saved_file_path(app, &saved) {
            Ok(path) => path,
            Err(error) => {
                self.set_error(
                    Some(saved.version.clone()),
                    &saved.source,
                    saved.downloaded_bytes,
                    saved.total_bytes,
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
        };
        let file_size = match fs::metadata(&file_path) {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                let error = error.to_string();
                self.set_error(
                    Some(saved.version.clone()),
                    &saved.source,
                    saved.downloaded_bytes,
                    saved.total_bytes,
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
        };
        if file_size == 0 {
            let error = "已下载的更新文件为空".to_string();
            self.set_error(
                Some(saved.version.clone()),
                &saved.source,
                saved.downloaded_bytes,
                saved.total_bytes,
                UpdateErrorStage::Install,
                error.clone(),
            );
            return Err(error);
        }

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
                        UpdateErrorStage::Install,
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
                        UpdateErrorStage::Install,
                        error.clone(),
                    );
                    return Err(error);
                }
            };
            let pid = current_process_id();
            let updates_directory = match updates_dir(app) {
                Ok(path) => path,
                Err(error) => {
                    self.set_error(
                        Some(saved.version.clone()),
                        &saved.source,
                        saved.downloaded_bytes,
                        saved.total_bytes,
                        UpdateErrorStage::Install,
                        error.clone(),
                    );
                    return Err(error);
                }
            };
            let unique_id = match macos_job_unique_id() {
                Ok(unique_id) => unique_id,
                Err(error) => {
                    self.set_error(
                        Some(saved.version.clone()),
                        &saved.source,
                        saved.downloaded_bytes,
                        saved.total_bytes,
                        UpdateErrorStage::Install,
                        error.clone(),
                    );
                    return Err(error);
                }
            };
            let ready_path =
                updates_directory.join(format!("macos-install-ready-{pid}-{unique_id}"));
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
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
            return Ok(UpdateInstallResult {
                restart_mode: UpdateInstallRestartMode::ExternalUpdater,
            });
        }

        #[cfg(not(target_os = "macos"))]
        let bytes = match fs::read(&file_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                let error = error.to_string();
                self.set_error(
                    Some(saved.version.clone()),
                    &saved.source,
                    saved.downloaded_bytes,
                    saved.total_bytes,
                    UpdateErrorStage::Install,
                    error.clone(),
                );
                return Err(error);
            }
        };
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
                UpdateErrorStage::Install,
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
            let metadata = fs::metadata(archive)
                .map_err(|error| format!("本地更新包不存在或无法读取: {archive_path}: {error}"))?;
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
            let bytes = fs::read(archive)
                .map_err(|error| format!("无法读取本地更新包 {archive_path}: {error}"))?;
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
                error_stage: None,
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
    let helper_directory =
        updates_directory.join(format!("macos-updater-{}-{unique}", current_process_id()));
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
        .map_err(|error| {
            format!(
                "启动 macOS updater 失败，日志路径 {}: {error}",
                job.log_path
            )
        })?;
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
    let state_file_path = directory.join(STATE_FILE);
    let file_name = format!("pending-update{suffix}");
    let file_path = directory.join(&file_name);
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
    let transaction_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let previous_file_backup =
        directory.join(format!(".{0}.backup-{transaction_id}", saved.file_name));
    let previous_state_backup = directory.join(format!(".{STATE_FILE}.backup-{transaction_id}"));
    let had_previous_file = file_path.is_file();
    if had_previous_file {
        if let Err(error) = fs::copy(&file_path, &previous_file_backup) {
            cleanup_saved_update_backup(&previous_file_backup, "旧更新包备份");
            return Err(format!("备份旧更新包失败: {error}"));
        }
    }
    let had_previous_state = state_file_path.is_file();
    if had_previous_state {
        if let Err(error) = fs::copy(&state_file_path, &previous_state_backup) {
            cleanup_saved_update_backup(&previous_file_backup, "旧更新包备份");
            return Err(format!("备份旧更新状态失败: {error}"));
        }
    }

    if let Err(error) = atomic_write(&file_path, bytes) {
        let mut message = format!("保存更新包失败: {error}");
        let recovery_error = restore_saved_update_file(
            &file_path,
            &previous_file_backup,
            had_previous_file,
            "旧更新包",
        );
        if let Some(recovery_error) = &recovery_error {
            message.push_str(&format!("; {recovery_error}"));
        } else {
            cleanup_saved_update_backup(&previous_file_backup, "旧更新包备份");
        }
        cleanup_saved_update_backup(&previous_state_backup, "旧更新状态备份");
        return Err(message);
    }

    if let Err(error) = atomic_write(&state_file_path, &text) {
        let mut recovery_errors = Vec::new();
        let file_recovery_error = restore_saved_update_file(
            &file_path,
            &previous_file_backup,
            had_previous_file,
            "旧更新包",
        );
        if let Some(recovery_error) = &file_recovery_error {
            recovery_errors.push(recovery_error.clone());
        } else {
            cleanup_saved_update_backup(&previous_file_backup, "旧更新包备份");
        }
        let state_recovery_error = restore_saved_update_file(
            &state_file_path,
            &previous_state_backup,
            had_previous_state,
            "旧更新状态",
        );
        if let Some(recovery_error) = &state_recovery_error {
            recovery_errors.push(recovery_error.clone());
        } else {
            cleanup_saved_update_backup(&previous_state_backup, "旧更新状态备份");
        }
        let recovery_message = if recovery_errors.is_empty() {
            String::new()
        } else {
            format!("; {}", recovery_errors.join("; "))
        };
        return Err(format!("保存更新状态失败: {error}{recovery_message}"));
    }

    cleanup_saved_update_backup(&previous_file_backup, "旧更新包备份");
    cleanup_saved_update_backup(&previous_state_backup, "旧更新状态备份");
    Ok(saved)
}

/// 清理更新事务产生的旧文件备份，并记录无法清理的原始异常。
fn cleanup_saved_update_backup(path: &Path, description: &str) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            log::error!("清理{description}失败: {error}; path={}", path.display());
        }
    }
}

/// 在更新事务失败时恢复旧包或删除未提交的新文件，并返回恢复异常。
fn restore_saved_update_file(
    path: &Path,
    backup_path: &Path,
    had_previous_file: bool,
    description: &str,
) -> Option<String> {
    if had_previous_file {
        fs::copy(backup_path, path)
            .map(|_| ())
            .map_err(|error| format!("恢复{description}失败: {error}"))
            .err()
    } else {
        match fs::remove_file(path) {
            Ok(()) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(format!("删除新{description}失败: {error}")),
        }
    }
}

/// 先写入同目录临时文件，再替换目标文件，保证更新状态不会留下半写入内容。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "更新文件名无效".to_string())?;
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    if let Err(error) = fs::write(&temporary, bytes) {
        if let Err(cleanup_error) = fs::remove_file(&temporary) {
            log::error!(
                "清理更新临时文件失败: {cleanup_error}; path={}; 原始写入错误: {error}",
                temporary.display()
            );
        }
        return Err(error.to_string());
    }

    #[cfg(target_os = "windows")]
    let replace_result = {
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{
                MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
            },
        };

        let temporary_wide = encode_wide(temporary.as_os_str());
        let path_wide = encode_wide(path.as_os_str());
        unsafe {
            MoveFileExW(
                PCWSTR::from_raw(temporary_wide.as_ptr()),
                PCWSTR::from_raw(path_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| error.to_string())
    };

    #[cfg(not(target_os = "windows"))]
    let replace_result = fs::rename(&temporary, path).map_err(|error| error.to_string());

    if let Err(error) = replace_result {
        if let Err(cleanup_error) = fs::remove_file(&temporary) {
            log::error!(
                "清理更新临时文件失败: {cleanup_error}; path={}; 原始替换错误: {error}",
                temporary.display()
            );
        }
        return Err(error);
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

/// 启动当前平台的已保存更新安装器；Windows 启动成功后结束旧进程，Linux 交由调用方重启。
fn install_saved_file(path: &Path, file_name: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use windows::{
            core::PCWSTR,
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOW},
        };

        let lower_file_name = file_name.to_ascii_lowercase();
        let (installer_path, parameters) = if lower_file_name.ends_with(".exe") {
            (
                encode_wide(path.as_os_str()),
                build_nsis_install_parameters(std::env::args_os().skip(1)),
            )
        } else if lower_file_name.ends_with(".msi") {
            let package_argument = escape_nsis_current_exe_arg(path.as_os_str());
            let parameter_text =
                format!("/i {package_argument} /passive /promptrestart AUTOLAUNCHAPP=True");
            (
                encode_wide(OsStr::new("msiexec.exe")),
                encode_wide(OsStr::new(&parameter_text)),
            )
        } else {
            return Err("当前 Windows 更新包格式不受支持".to_string());
        };
        let result = unsafe {
            ShellExecuteW(
                None,
                windows::core::w!("open"),
                PCWSTR::from_raw(installer_path.as_ptr()),
                PCWSTR::from_raw(parameters.as_ptr()),
                PCWSTR::null(),
                SW_SHOW,
            )
        };
        let error_code = result.0 as usize;
        if error_code <= 32 {
            return Err(format!(
                "启动 Windows 更新安装程序失败，错误码 {error_code}"
            ));
        }
        std::process::exit(0);
    }

    #[cfg(target_os = "macos")]
    {
        return install_macos_file(path);
    }

    #[cfg(target_os = "linux")]
    {
        let lower_file_name = file_name.to_ascii_lowercase();
        if lower_file_name.ends_with(".deb") {
            let status = Command::new("pkexec")
                .args(["dpkg", "-i", path.to_string_lossy().as_ref()])
                .status()
                .map_err(|error| error.to_string())?;
            if !status.success() {
                return Err(format!(
                    "Linux DEB 更新安装失败，退出码 {}",
                    process_exit_code(&status)
                ));
            }
            return Ok(());
        }
        if lower_file_name.ends_with(".rpm") {
            let status = Command::new("pkexec")
                .args(["rpm", "-U", path.to_string_lossy().as_ref()])
                .status()
                .map_err(|error| error.to_string())?;
            if !status.success() {
                return Err(format!(
                    "Linux RPM 更新安装失败，退出码 {}",
                    process_exit_code(&status)
                ));
            }
            return Ok(());
        }
        if lower_file_name.ends_with(".appimage") {
            install_linux_appimage(path)?;
            return Ok(());
        }
        return Err("当前 Linux 更新包格式不受支持".to_string());
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持持久化更新安装".to_string())
}

#[cfg(target_os = "linux")]
/// 将进程退出状态转换为安装失败提示所需的原始退出码。
fn process_exit_code(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "未知".to_string(), |code| code.to_string())
}

#[cfg(target_os = "windows")]
/// 按 NSIS 官方约定转义当前进程参数，避免安装器丢失参数边界。
fn escape_nsis_current_exe_arg(arg: &std::ffi::OsStr) -> String {
    let arg = arg.to_string_lossy();
    let mut command: Vec<char> = Vec::new();
    let quote = arg
        .chars()
        .any(|character| matches!(character, ' ' | '\t' | '/'))
        || arg.is_empty();
    let mut backslashes = 0_usize;

    if quote {
        command.push('"');
    }
    for character in arg.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            let escaped_backslashes = backslashes.saturating_mul(2).saturating_add(1);
            command.extend((0..escaped_backslashes).map(|_| '\\'));
        } else {
            command.extend((0..backslashes).map(|_| '\\'));
        }
        backslashes = 0;
        command.push(character);
    }
    if quote {
        command.extend((0..backslashes.saturating_mul(2)).map(|_| '\\'));
        command.push('"');
    } else {
        command.extend((0..backslashes).map(|_| '\\'));
    }
    command.into_iter().collect()
}

#[cfg(target_os = "windows")]
/// 构造 NSIS 更新安装器需要的固定参数和当前进程参数宽字符串。
fn build_nsis_install_parameters<I>(args: I) -> Vec<u16>
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
{
    let mut parameters = String::from("/P /R /UPDATE /ARGS");
    for argument in args {
        parameters.push(' ');
        parameters.push_str(&escape_nsis_current_exe_arg(argument.as_ref()));
    }
    encode_wide(std::ffi::OsStr::new(&parameters))
}

#[cfg(target_os = "windows")]
/// 将 Windows 路径或参数编码为 ShellExecuteW 使用的 null 结尾 UTF-16。
fn encode_wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "linux")]
/// 从当前运行路径定位并替换 Linux AppImage，避免直接覆盖打开中的可执行文件。
fn install_linux_appimage(path: &Path) -> Result<(), String> {
    let current = match std::env::var_os("APPIMAGE") {
        Some(appimage_value) => {
            let appimage_path = PathBuf::from(appimage_value);
            let metadata = fs::metadata(&appimage_path).map_err(|error| {
                format!(
                    "读取 APPIMAGE 环境变量路径失败 {}: {error}",
                    appimage_path.display()
                )
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "APPIMAGE 环境变量路径不是普通文件: {}",
                    appimage_path.display()
                ));
            }
            appimage_path
        }
        None => std::env::current_exe().map_err(|error| error.to_string())?,
    };
    replace_linux_appimage(&current, path)
}

#[cfg(target_os = "linux")]
/// 原子替换指定 AppImage，写入失败时恢复旧文件并保留原文件权限。
fn replace_linux_appimage(current: &Path, path: &Path) -> Result<(), String> {
    let current_parent = current
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| "当前 Linux 可执行文件目录无效".to_string())?;
    let permissions = fs::metadata(current)
        .map_err(|error| error.to_string())?
        .permissions();
    let unique_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let temporary_directory = current_parent.join(format!(
        ".auracoder-update-{}-{unique_id}",
        std::process::id()
    ));
    fs::create_dir(&temporary_directory).map_err(|error| error.to_string())?;
    let backup_path = temporary_directory.join("current-appimage");

    if let Err(error) = fs::rename(current, &backup_path) {
        let _ = fs::remove_dir_all(&temporary_directory);
        return Err(error.to_string());
    }

    let replace_result = fs::copy(path, current)
        .map(|_| ())
        .and_then(|_| fs::set_permissions(current, permissions));
    let Err(write_error) = replace_result else {
        if let Err(cleanup_error) = fs::remove_dir_all(&temporary_directory) {
            log::error!(
                "Linux AppImage 更新已替换成功，但清理旧可执行文件目录失败: {cleanup_error}; directory={}",
                temporary_directory.display()
            );
        }
        return Ok(());
    };

    let remove_error = match fs::remove_file(current) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(error),
    };
    let restore_result = fs::rename(&backup_path, current);
    let restore_succeeded = restore_result.is_ok();
    let restore_error = restore_result.err();
    let recovery_error = match (remove_error, restore_error) {
        (None, None) => None,
        (Some(remove_error), None) => Some(format!("删除不完整目标失败: {remove_error}")),
        (None, Some(restore_error)) => Some(format!("恢复旧可执行文件失败: {restore_error}")),
        (Some(remove_error), Some(restore_error)) => Some(format!(
            "删除不完整目标失败: {remove_error}; 恢复旧可执行文件失败: {restore_error}"
        )),
    };
    let cleanup_error = if restore_succeeded {
        fs::remove_dir_all(&temporary_directory).err()
    } else {
        None
    };
    if let Some(cleanup_error) = &cleanup_error {
        log::error!(
            "Linux AppImage 更新失败后清理临时目录失败: {cleanup_error}; directory={}",
            temporary_directory.display()
        );
    }
    match (recovery_error, cleanup_error) {
        (Some(recovery_error), Some(cleanup_error)) => Err(format!(
            "替换 Linux AppImage 失败: {write_error}; {recovery_error}; 清理备份目录失败: {cleanup_error}"
        )),
        (Some(recovery_error), None) => {
            Err(format!("替换 Linux AppImage 失败: {write_error}; {recovery_error}"))
        }
        (None, Some(cleanup_error)) => Err(format!(
            "替换 Linux AppImage 失败: {write_error}; 清理备份目录失败: {cleanup_error}"
        )),
        (None, None) => Err(write_error.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn escapes_nsis_current_process_arguments() {
        let cases = [
            ("", "\"\""),
            ("some space", "\"some space\""),
            ("some\ttab", "\"some\ttab\""),
            ("slash/value", "\"slash/value\""),
            ("C:\\work", "C:\\work"),
            ("quote\"value", "quote\\\"value"),
            ("path\\with space\\", "\"path\\with space\\\\\""),
        ];

        for (original, escaped) in cases {
            assert_eq!(
                escape_nsis_current_exe_arg(std::ffi::OsStr::new(original)),
                escaped
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn builds_nsis_shell_execute_parameters() {
        let arguments = [
            std::ffi::OsStr::new("--workspace"),
            std::ffi::OsStr::new("workspace path"),
        ];
        let parameters = build_nsis_install_parameters(arguments.iter().copied());

        assert_eq!(parameters.last(), Some(&0));
        let text = String::from_utf16(&parameters[..parameters.len() - 1]).unwrap();
        assert_eq!(text, "/P /R /UPDATE /ARGS --workspace \"workspace path\"");
    }

    #[cfg(target_os = "windows")]
    /// 验证 Windows 目标文件已存在时第二次原子写入可以完成替换。
    #[test]
    fn overwrites_existing_target_on_windows() {
        let unique_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auracoder-atomic-write-test-{}-{unique_id}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let target = directory.join("pending-update.exe");

        assert!(atomic_write(&target, b"first update").is_ok());
        assert!(atomic_write(&target, b"second update").is_ok());
        assert_eq!(fs::read(&target).unwrap(), b"second update");

        fs::remove_dir_all(directory).unwrap();
    }

    /// 验证检查结果提交会同步清空候选并写入状态，下载入口不会观察到可下载候选。
    #[test]
    fn commits_check_result_before_download_reservation() {
        let manager = UpdateManager::default();
        let state = UpdateProcessState::default();
        manager.commit_check_result(None, state.clone());

        assert!(manager.reserve_download("test").is_err());
        assert_eq!(manager.state(), state);
        let runtime = manager
            .runtime
            .lock()
            .expect("update manager lock poisoned");
        assert!(runtime.candidate.is_none());
        assert_eq!(runtime.state.phase, UpdatePhase::Idle);
    }

    /// 验证并发更新检查只有一个调用可以原子抢占 Checking 阶段。
    #[test]
    fn reserves_checking_once_for_concurrent_checks() {
        let manager = std::sync::Arc::new(UpdateManager::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let manager = std::sync::Arc::clone(&manager);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.reserve_check("test")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_none()).count(), 1);
        assert!(results.iter().all(|result| {
            result
                .as_ref()
                .is_none_or(|state| state.phase == UpdatePhase::Checking)
        }));
        assert_eq!(manager.state().phase, UpdatePhase::Checking);
    }

    /// 验证第二个并发下载调用在已有 Downloading 状态时直接返回现有状态。
    #[test]
    fn rejects_concurrent_download_after_reservation() {
        let manager = std::sync::Arc::new(UpdateManager::default());
        manager.set_state(UpdateProcessState {
            phase: UpdatePhase::Downloading,
            version: Some("1.0.4".to_string()),
            source: Some("test".to_string()),
            downloaded_bytes: 0,
            total_bytes: Some(10),
            error: None,
            error_stage: None,
        });
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let manager = std::sync::Arc::clone(&manager);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.reserve_download("test")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();

        for handle in handles {
            let result = handle.join().unwrap();
            assert!(matches!(
                result,
                Ok(DownloadReservation::Existing(state))
                    if state.phase == UpdatePhase::Downloading
            ));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    /// 验证 Linux AppImage 成功替换后清理临时备份目录。
    fn removes_linux_appimage_backup_after_success() {
        let unique_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auracoder-update-success-test-{}-{unique_id}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let current = directory.join("current.AppImage");
        let replacement = directory.join("replacement.AppImage");
        fs::write(&current, b"old appimage").unwrap();
        fs::write(&replacement, b"new appimage").unwrap();

        let result = replace_linux_appimage(&current, &replacement);

        assert!(result.is_ok());
        assert_eq!(fs::read(&current).unwrap(), b"new appimage");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restores_linux_appimage_when_replacement_fails() {
        let unique_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auracoder-update-test-{}-{unique_id}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let current = directory.join("current.AppImage");
        let missing_update = directory.join("missing.AppImage");
        fs::write(&current, b"old appimage").unwrap();

        let result = replace_linux_appimage(&current, &missing_update);

        assert!(result.is_err());
        assert_eq!(fs::read(&current).unwrap(), b"old appimage");
        fs::remove_dir_all(directory).unwrap();
    }
}
