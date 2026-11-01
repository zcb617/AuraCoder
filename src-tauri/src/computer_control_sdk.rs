#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{ffi::OsStr, process::Command};
use std::{
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;

const ABI_MAJOR: u16 = 1;
const ABI_MINOR: u16 = 1;

pub const fn is_supported_platform() -> bool {
    cfg!(any(
        target_os = "windows",
        target_os = "macos",
        all(target_os = "linux", target_arch = "x86_64")
    ))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CuaWaylandHelperStatus {
    pub supported: bool,
    pub wayland: bool,
    pub installed: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuaDriverSdkRuntimeInfo {
    pub abi_version: String,
    pub driver_version: Option<String>,
    pub embedded: bool,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuaDriverSdkStatus {
    pub initialized: bool,
    pub resource_dir: Option<String>,
    pub library_path: Option<String>,
    pub abi_version: Option<String>,
    pub driver_version: Option<String>,
    pub embedded: Option<bool>,
    pub error: Option<String>,
}

pub struct CuaDriverSdk {
    resource_dir: Mutex<Option<PathBuf>>,
    runtime: Mutex<Option<Runtime>>,
    runtime_info: Mutex<Option<CuaDriverSdkRuntimeInfo>>,
    last_error: Mutex<Option<String>>,
}

impl CuaDriverSdk {
    pub fn new() -> Self {
        Self {
            resource_dir: Mutex::new(None),
            runtime: Mutex::new(None),
            runtime_info: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    pub fn set_resource_dir(&self, resource_dir: Option<PathBuf>) {
        if let Ok(mut current) = self.resource_dir.lock() {
            if self
                .runtime
                .lock()
                .map(|runtime| runtime.is_none())
                .unwrap_or(false)
            {
                *current = resource_dir;
            }
        }
    }

    pub fn status(&self) -> CuaDriverSdkStatus {
        let resource_dir = self
            .resource_dir
            .lock()
            .ok()
            .and_then(|value| value.clone());
        let library_path = resource_dir
            .as_deref()
            .and_then(resolve_library_path)
            .map(|path| path.to_string_lossy().into_owned());
        let runtime_info = self
            .runtime_info
            .lock()
            .ok()
            .and_then(|value| value.clone());
        let error = self.last_error.lock().ok().and_then(|value| value.clone());

        CuaDriverSdkStatus {
            initialized: runtime_info.is_some(),
            resource_dir: resource_dir.map(|path| path.to_string_lossy().into_owned()),
            library_path,
            abi_version: runtime_info.as_ref().map(|value| value.abi_version.clone()),
            driver_version: runtime_info
                .as_ref()
                .and_then(|value| value.driver_version.clone()),
            embedded: runtime_info.as_ref().map(|value| value.embedded),
            error,
        }
    }

    pub fn wayland_helper_status(&self) -> CuaWaylandHelperStatus {
        wayland_helper_status()
    }

    pub fn restore_wayland_helper_if_installed(&self) -> Result<CuaWaylandHelperStatus, String> {
        activate_wayland_helper_if_installed()
    }

    /// 安装并激活当前 GNOME 版本兼容的 CUA Wayland 辅助组件。
    pub fn install_wayland_helper(&self) -> Result<CuaWaylandHelperStatus, String> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            // 先确认 GNOME 主版本支持官方辅助组件，避免在不支持的桌面环境启动安装器。
            if !wayland_helper_status().supported {
                return Err("当前桌面环境不支持 CUA 窗口辅助组件（需要 GNOME 45–49）".to_string());
            }

            if !is_wayland_session() {
                return Err("Wayland helper is available only in a Wayland session".to_string());
            }

            let resource_dir = self
                .resource_dir
                .lock()
                .map_err(|_| "CUA SDK resource state is poisoned".to_string())?
                .clone()
                .ok_or_else(|| "AuraCoder resource directory is not configured".to_string())?;
            let installer =
                resolve_wayland_helper_installer_path(&resource_dir).ok_or_else(|| {
                    format!(
                        "official CUA Wayland helper installer was not found under {}",
                        resource_dir.display()
                    )
                })?;

            let output = Command::new(&installer).output().map_err(|error| {
                format!(
                    "failed to start official CUA Wayland helper installer {}: {error}",
                    installer.display()
                )
            })?;
            if !output.status.success() {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(if detail.is_empty() {
                    format!(
                        "official CUA Wayland helper installer exited with {}",
                        output.status
                    )
                } else {
                    format!(
                        "official CUA Wayland helper installer exited with {}: {detail}",
                        output.status
                    )
                });
            }

            let status = wayland_helper_status();
            if !status.installed {
                return Err(
                    "official CUA Wayland helper installer completed, but the installed files were not found"
                        .to_string(),
                );
            }
            activate_wayland_helper_if_installed()
        }

        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            Err("Wayland helper is supported only on Linux x86_64".to_string())
        }
    }

    pub fn initialize(&self) -> Result<CuaDriverSdkRuntimeInfo, String> {
        {
            let runtime_info = self
                .runtime_info
                .lock()
                .map_err(|_| "CUA SDK runtime state is poisoned".to_string())?;
            if let Some(runtime_info) = runtime_info.clone() {
                return Ok(runtime_info);
            }
        }

        let result = (|| {
            let resource_dir = self
                .resource_dir
                .lock()
                .map_err(|_| "CUA SDK resource state is poisoned".to_string())?
                .clone()
                .ok_or_else(|| "AuraCoder resource directory is not configured".to_string())?;
            let library_path = resolve_library_path(&resource_dir).ok_or_else(|| {
                format!(
                    "official CUA SDK library was not found under {}",
                    resource_dir.display()
                )
            })?;

            prepare_runtime_environment();
            let runtime = Runtime::create(&library_path).map_err(|error| error.to_string())?;
            let runtime_info = runtime.info().map_err(|error| error.to_string())?;

            let mut runtime_slot = self
                .runtime
                .lock()
                .map_err(|_| "CUA SDK runtime state is poisoned".to_string())?;
            if runtime_slot.is_none() {
                *runtime_slot = Some(runtime);
                *self
                    .runtime_info
                    .lock()
                    .map_err(|_| "CUA SDK runtime info is poisoned".to_string())? =
                    Some(runtime_info.clone());
                self.set_error(None);
                Ok(runtime_info)
            } else {
                Err("CUA SDK runtime was initialized concurrently".to_string())
            }
        })();

        if let Err(error) = &result {
            self.set_error(Some(error.clone()));
        }
        result
    }

    pub fn list_tools(&self) -> Result<Value, String> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "CUA SDK runtime state is poisoned".to_string())?;
        runtime
            .as_mut()
            .ok_or_else(|| "CUA SDK runtime is not initialized".to_string())?
            .list_tools()
            .map_err(|error| self.remember_error(error))
    }

    pub fn invoke(&self, tool: &str, arguments: &Value) -> Result<Value, String> {
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| "CUA SDK runtime state is poisoned".to_string())?;
        runtime
            .as_mut()
            .ok_or_else(|| "CUA SDK runtime is not initialized".to_string())?
            .invoke(tool, arguments)
            .map_err(|error| self.remember_error(error))
    }

    pub fn shutdown(&self) -> Result<(), String> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| "CUA SDK runtime state is poisoned".to_string())?
            .take();
        *self
            .runtime_info
            .lock()
            .map_err(|_| "CUA SDK runtime info is poisoned".to_string())? = None;
        if let Some(mut runtime) = runtime {
            runtime
                .shutdown()
                .map_err(|error| self.remember_error(error))?;
        }
        Ok(())
    }

    fn set_error(&self, value: Option<String>) {
        if let Ok(mut error) = self.last_error.lock() {
            *error = value;
        }
    }

    fn remember_error(&self, error: RuntimeError) -> String {
        let message = error.to_string();
        self.set_error(Some(message.clone()));
        message
    }
}

pub fn is_wayland_session() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if let Ok(session_type) = std::env::var("XDG_SESSION_TYPE") {
        if !session_type.trim().is_empty() {
            return session_type.eq_ignore_ascii_case("wayland");
        }
    }
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 获取 CUA Wayland 辅助组件在当前桌面环境中的兼容、安装和运行状态。
fn wayland_helper_status() -> CuaWaylandHelperStatus {
    let wayland = is_wayland_session();
    let supported = is_supported_gnome_version();
    let installed = wayland_helper_install_dir()
        .is_some_and(|directory| wayland_helper_files_exist(&directory));
    let running = supported && wayland && wayland_helper_dbus_is_running();
    CuaWaylandHelperStatus {
        supported,
        wayland,
        installed,
        running,
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn wayland_helper_status() -> CuaWaylandHelperStatus {
    CuaWaylandHelperStatus {
        supported: false,
        wayland: false,
        installed: false,
        running: false,
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 读取 GNOME Shell 主版本，供 CUA Wayland 辅助组件判断桌面兼容性。
fn gnome_shell_major_version() -> Option<u32> {
    let connection = match zbus::blocking::Connection::session() {
        Ok(connection) => connection,
        Err(error) => {
            log::error!("failed to connect to the GNOME Shell D-Bus service: {error}");
            return None;
        }
    };
    let proxy = match zbus::blocking::Proxy::new(
        &connection,
        "org.gnome.Shell",
        "/org/gnome/Shell",
        "org.gnome.Shell",
    ) {
        Ok(proxy) => proxy,
        Err(error) => {
            log::error!("failed to create the GNOME Shell D-Bus proxy: {error}");
            return None;
        }
    };
    let version = match proxy.get_property::<String>("ShellVersion") {
        Ok(version) => version,
        Err(error) => {
            log::error!("failed to read GNOME Shell ShellVersion: {error}");
            return None;
        }
    };
    let Some(major) = version.split('.').next() else {
        log::error!("failed to parse GNOME Shell ShellVersion: {version:?}");
        return None;
    };
    match major.parse::<u32>() {
        Ok(major) => Some(major),
        Err(error) => {
            log::error!("failed to parse GNOME Shell ShellVersion {version:?}: {error}");
            None
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 判断当前 GNOME Shell 主版本是否处于官方辅助组件支持范围。
fn is_supported_gnome_version() -> bool {
    gnome_shell_major_version().is_some_and(|major| (45..=49).contains(&major))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn wayland_helper_install_dir() -> Option<PathBuf> {
    wayland_helper_install_dir_from_env(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn wayland_helper_install_dir_from_env(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    let data_home = xdg_data_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("share"))
        })?;
    Some(
        data_home
            .join("gnome-shell")
            .join("extensions")
            .join("winrects@cua"),
    )
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 判断 CUA Wayland 辅助组件是否已安装且提供当前 SDK 要求的 API 版本。
fn wayland_helper_files_exist(directory: &Path) -> bool {
    let metadata_path = directory.join("metadata.json");
    if !metadata_path.is_file() || !directory.join("extension.js").is_file() {
        return false;
    }
    let metadata_json = match std::fs::read(&metadata_path) {
        Ok(metadata_json) => metadata_json,
        Err(error) => {
            log::error!("failed to read CUA Wayland helper metadata {}: {error}", metadata_path.display());
            return false;
        }
    };
    let metadata: Value = match serde_json::from_slice(&metadata_json) {
        Ok(metadata) => metadata,
        Err(error) => {
            log::error!("failed to parse CUA Wayland helper metadata {}: {error}", metadata_path.display());
            return false;
        }
    };
    let Some(version) = metadata.get("version").and_then(Value::as_u64) else {
        log::error!("CUA Wayland helper metadata has no numeric API version: {metadata:?}");
        return false;
    };
    version >= 10
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 检查已运行的 CUA Wayland 辅助组件是否提供当前 SDK 所需 API。
fn wayland_helper_dbus_is_running() -> bool {
    let connection = match zbus::blocking::Connection::session() {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let proxy = match zbus::blocking::Proxy::new(
        &connection,
        "org.cua.WinRects",
        "/org/cua/WinRects",
        "org.cua.WinRects",
    ) {
        Ok(proxy) => proxy,
        Err(_) => return false,
    };
    proxy
        .call::<_, _, u32>("GetVersion", &())
        .is_ok_and(|version| version >= 10)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
/// 在兼容的 Wayland 桌面会话中启用已安装的 CUA 辅助组件。
fn activate_wayland_helper_if_installed() -> Result<CuaWaylandHelperStatus, String> {
    let mut status = wayland_helper_status();
    if !status.supported || !status.wayland || !status.installed || status.running {
        return Ok(status);
    }

    enable_gnome_user_extensions()?;
    request_wayland_helper_enable_in_current_session();
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(100));
        status = wayland_helper_status();
        if status.running {
            break;
        }
    }
    Ok(status)
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn activate_wayland_helper_if_installed() -> Result<CuaWaylandHelperStatus, String> {
    Ok(wayland_helper_status())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn enable_gnome_user_extensions() -> Result<(), String> {
    let current = Command::new("gsettings")
        .args(["get", "org.gnome.shell", "disable-user-extensions"])
        .output()
        .map_err(|error| format!("failed to read GNOME user extension setting: {error}"))?;
    if !current.status.success() {
        let detail = String::from_utf8_lossy(&current.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!(
                "gsettings could not read the GNOME user extension setting and exited with {}",
                current.status
            )
        } else {
            format!(
                "gsettings could not read the GNOME user extension setting and exited with {}: {detail}",
                current.status
            )
        });
    }

    match String::from_utf8_lossy(&current.stdout).trim() {
        "false" => return Ok(()),
        "true" => {}
        value => {
            return Err(format!(
                "gsettings returned an unexpected GNOME user extension setting: {value}"
            ));
        }
    }

    let output = Command::new("gsettings")
        .args(["set", "org.gnome.shell", "disable-user-extensions", "false"])
        .output()
        .map_err(|error| format!("failed to enable GNOME user extensions: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!(
            "gsettings could not enable GNOME user extensions and exited with {}",
            output.status
        )
    } else {
        format!(
            "gsettings could not enable GNOME user extensions and exited with {}: {detail}",
            output.status
        )
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn request_wayland_helper_enable_in_current_session() {
    let Ok(connection) = zbus::blocking::Connection::session() else {
        return;
    };
    let Ok(proxy) = zbus::blocking::Proxy::new(
        &connection,
        "org.gnome.Shell.Extensions",
        "/org/gnome/Shell/Extensions",
        "org.gnome.Shell.Extensions",
    ) else {
        return;
    };

    let _ = proxy.set_property("UserExtensionsEnabled", true);
    let _ = proxy.call::<_, _, bool>("EnableExtension", &("winrects@cua",));
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn prepare_runtime_environment() {
    if is_wayland_session() && std::env::var_os("CUA_DRIVER_RS_ENABLE_WAYLAND").is_none() {
        std::env::set_var("CUA_DRIVER_RS_ENABLE_WAYLAND", "1");
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn prepare_runtime_environment() {}

impl Default for CuaDriverSdk {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CuaDriverSdk {
    fn drop(&mut self) {
        let runtime = self.runtime.get_mut().ok().and_then(Option::take);
        if let Some(mut runtime) = runtime {
            let _ = runtime.shutdown();
        }
    }
}

fn resolve_library_path(resource_dir: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let (platform_dir, library_name) = ("windows-x86_64", "cua_driver_sdk.dll");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let (platform_dir, library_name) = ("linux-x86_64", "libcua_driver_sdk.so");
    #[cfg(target_os = "macos")]
    let (platform_dir, library_name) = ("macos-universal", "libcua_driver_sdk.dylib");
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        let _ = resource_dir;
        return None;
    }

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    {
        [
            resource_dir
                .join("resources")
                .join("cua-driver")
                .join(platform_dir)
                .join(library_name),
            resource_dir
                .join("resources")
                .join("cua-driver")
                .join(library_name),
            resource_dir
                .join("cua-driver")
                .join(platform_dir)
                .join(library_name),
            resource_dir.join("cua-driver").join(library_name),
            resource_dir.join(library_name),
        ]
        .into_iter()
        .find(|path| path.is_file())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn resolve_wayland_helper_installer_path(resource_dir: &Path) -> Option<PathBuf> {
    [
        resource_dir
            .join("resources")
            .join("cua-driver")
            .join("linux-x86_64")
            .join("wayland-helper")
            .join("install.sh"),
        resource_dir
            .join("cua-driver")
            .join("linux-x86_64")
            .join("wayland-helper")
            .join("install.sh"),
        resource_dir
            .join("linux-x86_64")
            .join("wayland-helper")
            .join("install.sh"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[derive(Debug)]
struct RuntimeError(String);

impl RuntimeError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
)))]
struct Runtime;

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
)))]
impl Runtime {
    fn create(_library_path: &Path) -> Result<Self, RuntimeError> {
        Err(RuntimeError::new(
            "CUA SDK direct runtime is not implemented for this platform yet",
        ))
    }

    fn info(&self) -> Result<CuaDriverSdkRuntimeInfo, RuntimeError> {
        Err(RuntimeError::new("CUA SDK runtime is unavailable"))
    }

    fn list_tools(&mut self) -> Result<Value, RuntimeError> {
        Err(RuntimeError::new("CUA SDK runtime is unavailable"))
    }

    fn invoke(&mut self, _name: &str, _arguments: &Value) -> Result<Value, RuntimeError> {
        Err(RuntimeError::new("CUA SDK runtime is unavailable"))
    }

    fn shutdown(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
))]
mod native_impl {
    use super::{
        Condvar, CuaDriverSdkRuntimeInfo, Duration, Mutex, Path, RuntimeError, ABI_MAJOR, ABI_MINOR,
    };
    use serde_json::Value;
    use std::{
        ffi::{c_char, c_void},
        mem, ptr, slice,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AbiVersion {
        struct_size: u32,
        major: u16,
        minor: u16,
        patch: u16,
        reserved: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Buffer {
        data: *mut u8,
        len: usize,
        capacity: usize,
    }

    #[repr(C)]
    struct Driver;

    #[repr(C)]
    struct Operation;

    type Completion = unsafe extern "C" fn(*mut c_void, i32, Buffer, Buffer);
    type AbiVersionFn = unsafe extern "C" fn(*mut AbiVersion) -> i32;
    type AbiCompatibleFn = unsafe extern "C" fn(u16, u16) -> bool;
    type BufferFreeFn = unsafe extern "C" fn(*mut Buffer);
    type CreateFn = unsafe extern "C" fn(*const u8, usize, *mut *mut Driver, *mut Buffer) -> i32;
    type DestroyFn = unsafe extern "C" fn(*mut *mut Driver);
    type AvailableFn = unsafe extern "C" fn(*mut Driver, *mut bool, *mut Buffer) -> i32;
    type JsonFn = unsafe extern "C" fn(*mut Driver, *mut Buffer, *mut Buffer) -> i32;
    type InvokeFn = unsafe extern "C" fn(
        *mut Driver,
        *const u8,
        usize,
        *const u8,
        usize,
        Option<Completion>,
        *mut c_void,
        *mut *mut Operation,
        *mut Buffer,
    ) -> i32;
    type ShutdownFn = unsafe extern "C" fn(
        *mut Driver,
        Option<Completion>,
        *mut c_void,
        *mut *mut Operation,
        *mut Buffer,
    ) -> i32;
    type OperationCancelFn = unsafe extern "C" fn(*mut Operation);
    type OperationReleaseFn = unsafe extern "C" fn(*mut *mut Operation);

    struct NativeApi {
        /// 已加载的官方 CUA 动态库句柄。
        module: *mut c_void,
        /// 超时后标记原生回调仍可能执行，阻止动态库过早卸载。
        keep_loaded: AtomicBool,
        /// 查询动态库 ABI 版本的入口。
        abi_version: AbiVersionFn,
        /// 判断当前 ABI 版本是否兼容的入口。
        abi_compatible: AbiCompatibleFn,
        /// 释放动态库返回的缓冲区的入口。
        buffer_free: BufferFreeFn,
        /// 创建 CUA driver 实例的入口。
        create: CreateFn,
        /// 销毁 CUA driver 实例的入口。
        destroy: DestroyFn,
        /// 查询 CUA driver 是否可用的入口。
        available: AvailableFn,
        /// 获取 driver 元数据 JSON 的入口。
        metadata: JsonFn,
        /// 获取 driver 工具列表 JSON 的入口。
        list_tools: JsonFn,
        /// 调用 CUA 工具的入口。
        invoke: InvokeFn,
        /// 关闭 driver 的异步入口。
        shutdown: ShutdownFn,
        /// 请求取消仍在运行的异步操作的入口。
        operation_cancel: OperationCancelFn,
        /// 释放 CUA 异步操作句柄的入口。
        operation_release: OperationReleaseFn,
    }

    unsafe impl Send for NativeApi {}
    unsafe impl Sync for NativeApi {}

    struct CompletionState {
        /// 保存异步操作完成状态和返回内容。
        value: Mutex<Option<(i32, String, String)>>,
        /// 通知等待方异步操作已经返回。
        wake: Condvar,
    }

    struct CompletionContext {
        /// 延长动态库生命周期到原生回调真正执行完毕。
        api: Arc<NativeApi>,
        /// 延长完成状态生命周期到原生回调真正执行完毕。
        state: Arc<CompletionState>,
    }

    pub(super) struct Runtime {
        /// 当前 driver 使用的官方 CUA 动态库 API。
        api: Option<Arc<NativeApi>>,
        /// 当前 Runtime 持有的 CUA driver 实例。
        driver: *mut Driver,
    }

    unsafe impl Send for Runtime {}
    unsafe impl Sync for Runtime {}

    #[cfg(target_os = "windows")]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    #[cfg_attr(all(target_os = "linux", target_arch = "x86_64"), link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
        fn dlerror() -> *const c_char;
    }

    #[cfg(target_os = "windows")]
    unsafe fn load_module(path: &Path) -> Result<*mut c_void, RuntimeError> {
        use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
        let wide = OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let module = LoadLibraryW(wide.as_ptr());
        if module.is_null() {
            Err(RuntimeError::new(format!(
                "failed to load official CUA SDK library {}",
                path.display()
            )))
        } else {
            Ok(module)
        }
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    unsafe fn load_module(path: &Path) -> Result<*mut c_void, RuntimeError> {
        use std::{
            ffi::{CStr, CString},
            os::unix::ffi::OsStrExt,
        };
        let path_bytes = path.as_os_str().as_bytes();
        let path_c = CString::new(path_bytes).map_err(|_| {
            RuntimeError::new(format!(
                "CUA SDK library path contains NUL: {}",
                path.display()
            ))
        })?;
        const RTLD_NOW: i32 = 2;
        let _ = dlerror();
        let module = dlopen(path_c.as_ptr(), RTLD_NOW);
        if module.is_null() {
            let error = dlerror();
            let detail = if error.is_null() {
                "unknown dlopen error".to_string()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            };
            Err(RuntimeError::new(format!(
                "failed to load official CUA SDK library {}: {detail}",
                path.display()
            )))
        } else {
            Ok(module)
        }
    }

    #[cfg(target_os = "windows")]
    unsafe fn symbol_address(module: *mut c_void, name: *const c_char) -> *mut c_void {
        GetProcAddress(module, name)
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    unsafe fn symbol_address(module: *mut c_void, name: *const c_char) -> *mut c_void {
        dlsym(module, name)
    }

    #[cfg(target_os = "windows")]
    unsafe fn unload_module(module: *mut c_void) {
        let _ = FreeLibrary(module);
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    unsafe fn unload_module(module: *mut c_void) {
        let _ = dlclose(module);
    }

    unsafe fn load_symbol<T>(module: *mut c_void, name: &'static [u8]) -> Result<T, RuntimeError> {
        let address = symbol_address(module, name.as_ptr() as *const c_char);
        if address.is_null() {
            return Err(RuntimeError::new(format!(
                "official CUA SDK is missing symbol {}",
                String::from_utf8_lossy(name)
            )));
        }
        Ok(mem::transmute_copy(&address))
    }

    /// 加载官方 CUA 动态库符号并将模块释放责任交给共享 API 对象。
    fn load_api(path: &Path) -> Result<Arc<NativeApi>, RuntimeError> {
        let module = unsafe { load_module(path)? };

        let result = (|| unsafe {
            Ok(Arc::new(NativeApi {
                module,
                keep_loaded: AtomicBool::new(false),
                abi_version: load_symbol(module, b"cua_driver_abi_version_v1\0")?,
                abi_compatible: load_symbol(module, b"cua_driver_abi_is_compatible_v1\0")?,
                buffer_free: load_symbol(module, b"cua_driver_buffer_free_v1\0")?,
                create: load_symbol(module, b"cua_driver_create_v1\0")?,
                destroy: load_symbol(module, b"cua_driver_destroy_v1\0")?,
                available: load_symbol(module, b"cua_driver_is_available_v1\0")?,
                metadata: load_symbol(module, b"cua_driver_metadata_json_v1\0")?,
                list_tools: load_symbol(module, b"cua_driver_list_tools_json_v1\0")?,
                invoke: load_symbol(module, b"cua_driver_invoke_v1\0")?,
                shutdown: load_symbol(module, b"cua_driver_shutdown_v1\0")?,
                operation_cancel: load_symbol(module, b"cua_driver_operation_cancel_v1\0")?,
                operation_release: load_symbol(module, b"cua_driver_operation_release_v1\0")?,
            }))
        })();
        if result.is_err() {
            unsafe {
                unload_module(module);
            }
        }
        result
    }

    impl Drop for NativeApi {
        fn drop(&mut self) {
            // SDK 进程级异步执行器可能继续引用动态库，因此保留模块映射至进程退出。
            if self.keep_loaded.load(Ordering::Acquire) {
                log::debug!(
                    "CUA SDK asynchronous executor may still reference the module; keeping it loaded until process exit"
                );
                return;
            }
            unsafe { unload_module(self.module) };
        }
    }

    unsafe fn read_buffer(api: &NativeApi, mut buffer: Buffer) -> String {
        let value = if buffer.data.is_null() || buffer.len == 0 {
            String::new()
        } else {
            String::from_utf8_lossy(slice::from_raw_parts(buffer.data, buffer.len)).into_owned()
        };
        (api.buffer_free)(&mut buffer);
        value
    }

    /// 接收 CUA 原生完成回调并回收其独占上下文。
    unsafe extern "C" fn completion_callback(
        context: *mut c_void,
        status: i32,
        result: Buffer,
        error: Buffer,
    ) {
        if context.is_null() {
            return;
        }
        // 原生回调收到该指针后独占并回收已移交的 CompletionContext。
        let context = Box::from_raw(context as *mut CompletionContext);
        let result = read_buffer(&context.api, result);
        let error = read_buffer(&context.api, error);
        let completed = (status, result, error);
        match context.state.value.lock() {
            Ok(mut value) => *value = Some(completed),
            Err(poisoned) => {
                log::error!("CUA completion state lock poisoned: {poisoned}");
                *poisoned.into_inner() = Some(completed);
            }
        }
        context.state.wake.notify_one();
    }

    /// 按调用预算等待 CUA 原生操作完成并返回其状态及结果。
    fn wait_for_completion(
        state: &CompletionState,
        timeout: Duration,
    ) -> Result<(i32, String, String), RuntimeError> {
        let guard = match state.value.lock() {
            Ok(guard) => guard,
            Err(error) => {
                log::error!("CUA completion state lock poisoned: {error}");
                let guard = error.into_inner();
                if let Some(value) = guard.clone() {
                    return Ok(value);
                }
                guard
            }
        };
        let (guard, result) = match state
            .wake
            .wait_timeout_while(guard, timeout, |value| value.is_none())
        {
            Ok(value) => value,
            Err(error) => {
                log::error!("CUA completion wait lock poisoned: {error}");
                let message = error.to_string();
                let (guard, _) = error.into_inner();
                if let Some(value) = guard.clone() {
                    return Ok(value);
                }
                return Err(RuntimeError::new(format!("CUA completion wait failed: {message}")));
            }
        };
        if result.timed_out() {
            return Err(RuntimeError::new(
                format!("CUA native operation timed out after {timeout:?}; operation may still be running"),
            ));
        }
        guard
            .clone()
            .ok_or_else(|| RuntimeError::new("CUA operation completed without a result"))
    }

    unsafe fn call_json(
        api: &NativeApi,
        driver: *mut Driver,
        call: JsonFn,
    ) -> Result<Value, RuntimeError> {
        let mut output = Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        let mut error = output;
        let status = call(driver, &mut output, &mut error);
        let output = read_buffer(api, output);
        let error = read_buffer(api, error);
        if status != 0 {
            return Err(RuntimeError::new(format!(
                "CUA JSON call failed with status {status}: {error}"
            )));
        }
        serde_json::from_str(&output).map_err(|parse_error| {
            RuntimeError::new(format!("CUA returned invalid JSON: {parse_error}"))
        })
    }

    /// 提交异步 CUA 调用，在完成或超时后释放操作句柄。
    unsafe fn call_async(
        api: &Arc<NativeApi>,
        driver: *mut Driver,
        name: Option<&[u8]>,
        arguments: Option<&[u8]>,
        shutdown: bool,
        tool_name: &str,
        timeout: Duration,
    ) -> Result<(i32, String, String), RuntimeError> {
        let state = Arc::new(CompletionState {
            value: Mutex::new(None),
            wake: Condvar::new(),
        });
        let context = Box::new(CompletionContext {
            api: Arc::clone(api),
            state: Arc::clone(&state),
        });
        // 所有权交给原生 SDK，失败时依据 SDK 契约由当前线程收回。
        let context = Box::into_raw(context);
        let mut operation = ptr::null_mut();
        let mut error = Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        };
        // SDK 的进程级异步执行器可能在回调返回后仍引用动态库代码。
        api.keep_loaded.store(true, Ordering::Release);
        let status = if shutdown {
            (api.shutdown)(
                driver,
                Some(completion_callback),
                context as *mut c_void,
                &mut operation,
                &mut error,
            )
        } else {
            let name = name.expect("CUA invoke name is required");
            let arguments = arguments.expect("CUA invoke arguments are required");
            (api.invoke)(
                driver,
                name.as_ptr(),
                name.len(),
                arguments.as_ptr(),
                arguments.len(),
                Some(completion_callback),
                context as *mut c_void,
                &mut operation,
                &mut error,
            )
        };
        let immediate_error = read_buffer(api, error);
        if status != 0 {
            // SDK 保证 admission 失败时不会回调，因此本线程回收移交的上下文。
            drop(Box::from_raw(context));
            if !operation.is_null() {
                (api.operation_release)(&mut operation);
            }
            return Err(RuntimeError::new(format!(
                "CUA operation admission failed with status {status}: {immediate_error}"
            )));
        }

        let completed = wait_for_completion(&state, timeout);
        if let Err(error) = &completed {
            if operation.is_null() {
                log::warn!(
                    "CUA tool {tool_name} wait failed without a native operation handle; cancellation could not be requested: {error}"
                );
            } else {
                (api.operation_cancel)(operation);
                log::warn!(
                    "CUA tool {tool_name} wait failed; cancellation was requested: {error}"
                );
            }
        }
        if !operation.is_null() {
            (api.operation_release)(&mut operation);
        }
        completed
    }

    impl Runtime {
        /// 创建 CUA driver，并校验动态库 ABI 和 driver 可用状态。
        pub(super) fn create(path: &Path) -> Result<Self, RuntimeError> {
            unsafe {
                let api = load_api(path)?;
                let mut version = AbiVersion {
                    struct_size: std::mem::size_of::<AbiVersion>() as u32,
                    major: 0,
                    minor: 0,
                    patch: 0,
                    reserved: 0,
                };
                let status = (api.abi_version)(&mut version);
                if status != 0 {
                    drop(api);
                    return Err(RuntimeError::new(format!(
                        "CUA ABI version call failed with status {status}"
                    )));
                }
                if version.major != ABI_MAJOR || !(api.abi_compatible)(ABI_MAJOR, ABI_MINOR) {
                    drop(api);
                    return Err(RuntimeError::new(format!(
                        "CUA ABI mismatch: runtime={}.{}.{} expected={}.{}",
                        version.major, version.minor, version.patch, ABI_MAJOR, ABI_MINOR
                    )));
                }

                let options = b"{}";
                let mut driver = ptr::null_mut();
                let mut error = Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                };
                let status = (api.create)(options.as_ptr(), options.len(), &mut driver, &mut error);
                let error = read_buffer(&api, error);
                if status != 0 || driver.is_null() {
                    drop(api);
                    return Err(RuntimeError::new(format!(
                        "CUA runtime creation failed with status {status}: {error}"
                    )));
                }
                let mut available = false;
                let mut availability_error = Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                };
                let availability_status =
                    (api.available)(driver, &mut available, &mut availability_error);
                let availability_error = read_buffer(&api, availability_error);
                if availability_status != 0 || !available {
                    (api.destroy)(&mut driver);
                    drop(api);
                    return Err(RuntimeError::new(format!(
                        "CUA runtime is unavailable with status {availability_status}: {availability_error}"
                    )));
                }
                Ok(Self {
                    api: Some(api),
                    driver,
                })
            }
        }

        pub(super) fn info(&self) -> Result<CuaDriverSdkRuntimeInfo, RuntimeError> {
            let api = self
                .api
                .as_ref()
                .ok_or_else(|| RuntimeError::new("CUA runtime library is unloaded"))?;
            unsafe {
                let mut version = AbiVersion {
                    struct_size: std::mem::size_of::<AbiVersion>() as u32,
                    major: 0,
                    minor: 0,
                    patch: 0,
                    reserved: 0,
                };
                let status = (api.abi_version)(&mut version);
                if status != 0 {
                    return Err(RuntimeError::new(format!(
                        "CUA ABI version call failed with status {status}"
                    )));
                }
                let metadata = call_json(api, self.driver, api.metadata)?;
                let driver_version = metadata
                    .get("driver_version")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let embedded = metadata
                    .get("embedded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(CuaDriverSdkRuntimeInfo {
                    abi_version: format!("{}.{}.{}", version.major, version.minor, version.patch),
                    driver_version,
                    embedded,
                    metadata,
                })
            }
        }

        pub(super) fn list_tools(&mut self) -> Result<Value, RuntimeError> {
            let api = self
                .api
                .as_ref()
                .ok_or_else(|| RuntimeError::new("CUA runtime library is unloaded"))?;
            unsafe { call_json(api, self.driver, api.list_tools) }
        }

        /// 按工具业务超时预算调用 CUA 工具并校验原生完成状态。
        pub(super) fn invoke(
            &mut self,
            name: &str,
            arguments: &Value,
        ) -> Result<Value, RuntimeError> {
            let api = self
                .api
                .as_ref()
                .ok_or_else(|| RuntimeError::new("CUA runtime library is unloaded"))?;
            // run_script 的 SDK 执行预算采用参数 timeout_ms，其他工具统一允许 130 秒。
            let timeout = if name == "run_script" {
                let timeout_ms = arguments
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .clamp(100, 120_000);
                Duration::from_millis(timeout_ms + 10_000)
            } else {
                Duration::from_secs(130)
            };
            let arguments = serde_json::to_vec(arguments).map_err(|error| {
                RuntimeError::new(format!("failed to encode CUA arguments: {error}"))
            })?;
            let name_bytes = name.as_bytes();
            let (status, result, error) = unsafe {
                call_async(
                    api,
                    self.driver,
                    Some(name_bytes),
                    Some(&arguments),
                    false,
                    name,
                    timeout,
                )?
            };
            if status != 0 || !error.is_empty() {
                return Err(RuntimeError::new(format!(
                    "CUA tool {name} failed with status {status}: {error}"
                )));
            }
            serde_json::from_str(&result).map_err(|parse_error| {
                RuntimeError::new(format!("CUA tool returned invalid JSON: {parse_error}"))
            })
        }

        /// 在固定 130 秒预算内关闭 CUA driver 并释放其运行资源。
        pub(super) fn shutdown(&mut self) -> Result<(), RuntimeError> {
            let Some(api) = self.api.take() else {
                return Ok(());
            };
            unsafe {
                let result = call_async(
                    &api,
                    self.driver,
                    None,
                    None,
                    true,
                    "shutdown",
                    Duration::from_secs(130),
                );
                (api.destroy)(&mut self.driver);
                result.and_then(|(status, _, error)| {
                    if status != 0 || !error.is_empty() {
                        Err(RuntimeError::new(format!(
                            "CUA shutdown failed with status {status}: {error}"
                        )))
                    } else {
                        Ok(())
                    }
                })
            }
        }
    }

    impl Drop for Runtime {
        fn drop(&mut self) {
            let _ = self.shutdown();
        }
    }
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
))]
use native_impl::Runtime;

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::CuaDriverSdk;
    use std::{env, path::PathBuf};

    #[test]
    fn official_macos_release_runtime_smoke() {
        if env::var_os("PANES_CUA_SDK_SPIKE").is_none() {
            return;
        }
        let resource_dir = env::var_os("PANES_CUA_SDK_RESOURCE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let sdk = CuaDriverSdk::new();
        sdk.set_resource_dir(Some(resource_dir));
        let info = sdk
            .initialize()
            .expect("macOS CUA runtime should initialize");
        assert_eq!(info.abi_version, "1.1.0");
        assert_eq!(info.driver_version.as_deref(), Some("0.38.0"));
        assert!(info.embedded);
        let tools = sdk.list_tools().expect("tool inventory should load");
        assert!(tools
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|tools| !tools.is_empty()));
        assert!(sdk
            .invoke("get_screen_size", &serde_json::json!({}))
            .expect("screen size should be readable")
            .is_object());
        sdk.shutdown().expect("macOS CUA runtime should shut down");
        assert!(!sdk.status().initialized);
    }

    #[test]
    fn tool_calls_require_explicit_startup_initialization() {
        let sdk = CuaDriverSdk::new();
        let error = sdk
            .invoke("get_screen_size", &serde_json::json!({}))
            .expect_err("tool call must not initialize the CUA runtime on demand");
        assert!(error.contains("CUA SDK runtime is not initialized"));
        assert!(!sdk.status().initialized);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::CuaDriverSdk;
    use std::{env, path::PathBuf};

    #[test]
    fn official_windows_release_runtime_smoke() {
        if env::var_os("PANES_CUA_SDK_SPIKE").is_none() {
            return;
        }
        let resource_dir = PathBuf::from("target/release");
        let sdk = CuaDriverSdk::new();
        sdk.set_resource_dir(Some(resource_dir));
        let info = sdk.initialize().expect("CUA runtime should initialize");
        assert_eq!(info.abi_version, "1.1.0");
        assert_eq!(info.driver_version.as_deref(), Some("0.38.0"));
        assert!(info.embedded);
        assert!(sdk
            .list_tools()
            .expect("tool inventory should load")
            .is_object());
        assert!(sdk
            .invoke("get_screen_size", &serde_json::json!({}))
            .expect("screen size should be readable")
            .is_object());
        sdk.shutdown().expect("CUA runtime should shut down");
        assert!(!sdk.status().initialized);
    }

    #[test]
    fn tool_calls_require_explicit_startup_initialization() {
        let sdk = CuaDriverSdk::new();

        let error = sdk
            .invoke("get_screen_size", &serde_json::json!({}))
            .expect_err("tool call must not initialize the CUA runtime on demand");

        assert!(error.contains("CUA SDK runtime is not initialized"));
        assert!(!sdk.status().initialized);
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod linux_tests {
    use super::{prepare_runtime_environment, wayland_helper_install_dir_from_env, CuaDriverSdk};
    use std::{env, ffi::OsStr, path::PathBuf, sync::Mutex};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn wayland_helper_install_location_follows_xdg_then_home() {
        assert_eq!(
            wayland_helper_install_dir_from_env(
                Some(OsStr::new("/tmp/auracoder-data")),
                Some(OsStr::new("/tmp/auracoder-home")),
            ),
            Some(PathBuf::from(
                "/tmp/auracoder-data/gnome-shell/extensions/winrects@cua"
            ))
        );
        assert_eq!(
            wayland_helper_install_dir_from_env(None, Some(OsStr::new("/tmp/auracoder-home"))),
            Some(PathBuf::from(
                "/tmp/auracoder-home/.local/share/gnome-shell/extensions/winrects@cua"
            ))
        );
    }

    #[test]
    fn wayland_runtime_is_enabled_without_overriding_explicit_configuration() {
        let _guard = ENV_LOCK.lock().expect("environment test lock poisoned");
        let original_session = env::var_os("XDG_SESSION_TYPE");
        let original_display = env::var_os("WAYLAND_DISPLAY");
        let original_enabled = env::var_os("CUA_DRIVER_RS_ENABLE_WAYLAND");

        env::set_var("XDG_SESSION_TYPE", "wayland");
        env::remove_var("WAYLAND_DISPLAY");
        env::remove_var("CUA_DRIVER_RS_ENABLE_WAYLAND");
        prepare_runtime_environment();
        assert_eq!(env::var("CUA_DRIVER_RS_ENABLE_WAYLAND").as_deref(), Ok("1"));

        env::set_var("CUA_DRIVER_RS_ENABLE_WAYLAND", "0");
        prepare_runtime_environment();
        assert_eq!(env::var("CUA_DRIVER_RS_ENABLE_WAYLAND").as_deref(), Ok("0"));

        restore_env("XDG_SESSION_TYPE", original_session);
        restore_env("WAYLAND_DISPLAY", original_display);
        restore_env("CUA_DRIVER_RS_ENABLE_WAYLAND", original_enabled);
    }

    #[test]
    fn explicit_x11_session_is_not_mistaken_for_wayland() {
        let _guard = ENV_LOCK.lock().expect("environment test lock poisoned");
        let original_session = env::var_os("XDG_SESSION_TYPE");
        let original_display = env::var_os("WAYLAND_DISPLAY");

        env::set_var("XDG_SESSION_TYPE", "x11");
        env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert!(!super::is_wayland_session());

        restore_env("XDG_SESSION_TYPE", original_session);
        restore_env("WAYLAND_DISPLAY", original_display);
    }

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            env::set_var(key, value);
        } else {
            env::remove_var(key);
        }
    }

    #[test]
    fn official_linux_release_runtime_smoke() {
        let _guard = ENV_LOCK.lock().expect("environment test lock poisoned");
        if env::var_os("PANES_CUA_SDK_SPIKE").is_none() {
            return;
        }
        let resource_dir = env::var_os("PANES_CUA_SDK_RESOURCE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let sdk = CuaDriverSdk::new();
        sdk.set_resource_dir(Some(resource_dir));
        let info = sdk
            .initialize()
            .expect("Linux CUA runtime should initialize");
        assert_eq!(info.abi_version, "1.1.0");
        assert_eq!(info.driver_version.as_deref(), Some("0.38.0"));
        assert!(info.embedded);
        let tools = sdk.list_tools().expect("tool inventory should load");
        assert!(tools
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|tools| !tools.is_empty()));
        sdk.shutdown().expect("Linux CUA runtime should shut down");
        assert!(!sdk.status().initialized);
    }
}
