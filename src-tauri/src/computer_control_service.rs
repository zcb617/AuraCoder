use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::{
    sync::{oneshot, Mutex},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    computer_control_sdk::CuaDriverSdk,
    config::app_config::{AppConfig, ComputerControlAuthorizationConfig},
    runtime_env,
};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
const COMPUTER_CONTROL_APPROVAL_EVENT: &str = "computer-control-approval-requested";
const COMPUTER_CONTROL_NAMESPACE: &str = "auracoder_computer_control";

#[derive(Debug)]
struct PendingAuthorization {
    target_key: String,
    authorization: ComputerControlAuthorization,
    response: oneshot::Sender<bool>,
}

impl Default for ComputerControlService {
    fn default() -> Self {
        Self::new(Arc::new(CuaDriverSdk::new()))
    }
}

pub fn dynamic_tool_success(value: Value) -> Value {
    json!({
        "contentItems": content_items(value),
        "success": true
    })
}

pub fn dynamic_tool_failure(error: impl Into<String>) -> Value {
    json!({
        "contentItems": [{
            "type": "inputText",
            "text": error.into()
        }],
        "success": false
    })
}

fn content_items(value: Value) -> Vec<Value> {
    if let Some(items) = value.get("contentItems").and_then(Value::as_array) {
        return items.clone();
    }
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        let mapped = items
            .iter()
            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                Some("text") => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| json!({"type": "inputText", "text": text})),
                Some("image") => {
                    let data = item.get("data").and_then(Value::as_str)?;
                    let mime = item
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    Some(json!({
                        "type": "inputImage",
                        "imageUrl": format!("data:{mime};base64,{data}")
                    }))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if !mapped.is_empty() {
            return mapped;
        }
    }
    if let Some(image_url) = value
        .get("imageUrl")
        .or_else(|| value.get("dataUrl"))
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("data:image/"))
    {
        return vec![json!({
            "type": "inputImage",
            "imageUrl": image_url
        })];
    }

    let text = match value {
        Value::String(text) => text,
        value => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
    };
    vec![json!({
        "type": "inputText",
        "text": text
    })]
}

fn normalize_arguments(arguments: Value) -> Result<Value, String> {
    match arguments {
        Value::Object(_) => Ok(arguments),
        Value::String(raw) => serde_json::from_str(&raw).map_err(|error| {
            service_error(
                "invalid_request",
                &format!("电脑操作 arguments 不是有效 JSON：{error}"),
            )
        }),
        _ => Err(service_error(
            "invalid_request",
            "电脑操作 arguments 必须是 JSON 对象",
        )),
    }
}

fn target_resource(tool: &str, arguments: &Value) -> Result<TargetResource, String> {
    let object = arguments.as_object();
    let desktop_scope = object
        .and_then(|value| value.get("scope"))
        .and_then(Value::as_str)
        .map(|scope| scope.eq_ignore_ascii_case("desktop"))
        .unwrap_or(false)
        || object
            .and_then(|value| value.get("capture_scope"))
            .and_then(Value::as_str)
            .map(|scope| scope.eq_ignore_ascii_case("desktop"))
            .unwrap_or(false);
    if desktop_scope {
        return Err(service_error(
            "target_scope_mismatch",
            "AuraCoder 不允许全桌面范围的电脑操作",
        ));
    }

    for key in ["launch_path", "path"] {
        if let Some(value) = object
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return resolved_application_resource(value);
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(bundle_id) = object
        .and_then(|value| value.get("bundle_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Some(path) = resolve_macos_application_path(bundle_id, true) {
            return Ok(application_resource(&path));
        }
        return Err(service_error(
            "target_not_found",
            "无法解析目标应用标识对应的实际应用",
        ));
    }
    if let Some(pid) = object
        .and_then(|value| value.get("pid"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        if let Some(application) = process_executable_path(pid) {
            return resolved_application_resource(&application);
        }
        return Err(service_error(
            "target_not_found",
            "无法读取目标进程的可执行文件名",
        ));
    }

    for key in ["application", "name", "aumid"] {
        if let Some(value) = object
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return resolved_application_resource(value);
        }
    }

    match tool {
        "list_apps" | "list_windows" => Ok(TargetResource {
            key: "observation:applications".to_string(),
            display: "Windows 应用和窗口".to_string(),
            scope: "observation",
        }),
        "clipboard_read" | "clipboard_write" => Ok(TargetResource {
            key: "resource:clipboard".to_string(),
            display: "当前任务剪贴板".to_string(),
            scope: "clipboard",
        }),
        "start_session"
        | "end_session"
        | "health_report"
        | "get_screen_size"
        | "get_cursor_position"
        | "get_session_state" => Ok(TargetResource {
            key: "metadata:computer-control".to_string(),
            display: "电脑操作运行状态".to_string(),
            scope: "metadata",
        }),
        _ => Err(service_error(
            "target_not_found",
            &format!("电脑操作工具 `{tool}` 缺少应用或窗口目标"),
        )),
    }
}

fn application_resource(application: &str) -> TargetResource {
    #[cfg(target_os = "macos")]
    let macos_bundle_name = Path::new(application.trim()).ancestors().find_map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.to_ascii_lowercase().ends_with(".app"))
    });
    let display = Path::new(application.trim())
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .map(str::trim)
        .filter(|file_name| !file_name.is_empty())
        .unwrap_or_else(|| application.trim())
        .to_string();
    #[cfg(target_os = "macos")]
    let display = macos_bundle_name.unwrap_or(&display).to_string();
    TargetResource {
        key: format!("application:{}", display.to_lowercase()),
        display,
        scope: "application",
    }
}

fn resolved_application_resource(application: &str) -> Result<TargetResource, String> {
    let application = application.trim();
    if application.is_empty() {
        return Err(service_error("target_not_found", "缺少目标可执行文件名"));
    }

    let path = Path::new(application);
    if path.is_file() {
        return Ok(application_resource(application));
    }
    #[cfg(target_os = "macos")]
    if path.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().ends_with(".app"))
    {
        return Ok(application_resource(application));
    }

    let is_bare_name = !application.contains('\\') && !application.contains('/');
    if is_bare_name {
        if let Some(resolved) = runtime_env::resolve_executable(application) {
            return Ok(application_resource(&resolved.to_string_lossy()));
        }
        #[cfg(target_os = "macos")]
        if let Some(path) = resolve_macos_application_path(application, false) {
            return Ok(application_resource(&path));
        }
        return Err(service_error(
            "target_not_found",
            "无法解析目标名称对应的实际可执行文件名",
        ));
    }

    Err(service_error(
        "target_not_found",
        "无法解析目标路径对应的实际可执行文件名",
    ))
}

#[cfg(target_os = "macos")]
fn resolve_macos_application_path(application: &str, bundle_id: bool) -> Option<String> {
    if application.contains(['\'', '"', '\n', '\r']) {
        return None;
    }
    let query = if bundle_id {
        format!("kMDItemCFBundleIdentifier == '{application}'")
    } else {
        let file_name = application.strip_suffix(".app").unwrap_or(application);
        format!(
            "kMDItemFSName == '{file_name}.app'c && kMDItemContentType == 'com.apple.application-bundle'"
        )
    };
    let output = std::process::Command::new("/usr/bin/mdfind")
        .arg(query)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|path| !path.is_empty())
        .map(str::to_string)
}

fn authorization_matches_target(
    authorization: &ComputerControlAuthorizationConfig,
    target_key: &str,
) -> bool {
    authorization.target_key == target_key
}

#[cfg(target_os = "windows")]
fn process_executable_path(pid: u32) -> Option<String> {
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    result
        .ok()
        .map(|_| String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn process_executable_path(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn process_executable_path(pid: u32) -> Option<String> {
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffer_size: u32) -> i32;
    }

    let pid = i32::try_from(pid).ok()?;
    let mut buffer = vec![0_u8; PROC_PIDPATHINFO_MAXSIZE];
    let length = unsafe {
        proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    let length = usize::try_from(length).ok()?.min(buffer.len());
    let path = &buffer[..length];
    let path = path.strip_suffix(&[0]).unwrap_or(path);
    Some(String::from_utf8_lossy(path).into_owned())
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn process_executable_path(_pid: u32) -> Option<String> {
    None
}

fn target_is_auracoder(application: &str) -> bool {
    let Some(current_path) = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
    else {
        return false;
    };
    let candidate = Path::new(application);
    application.eq_ignore_ascii_case(&current_path.to_string_lossy())
        || candidate
            .file_name()
            .zip(current_path.file_name())
            .map(|(left, right)| left.eq_ignore_ascii_case(right))
            .unwrap_or(false)
        || candidate
            .file_stem()
            .zip(current_path.file_stem())
            .map(|(left, right)| left.eq_ignore_ascii_case(right))
            .unwrap_or(false)
}

fn operation_kind(tool: &str) -> &'static str {
    match tool {
        "click" | "double_click" | "right_click" | "drag" | "type_text" | "press_key"
        | "hotkey" | "set_value" | "invoke_menu" | "scroll" | "move_cursor" | "zoom"
        | "bring_to_front" | "launch_app" => "input",
        "clipboard_read" | "clipboard_write" => "clipboard",
        "start_session" | "end_session" => "session",
        _ => "observe",
    }
}

fn service_error(code: &str, message: &str) -> String {
    format!("{code}: {message}")
}

#[cfg(test)]
mod tests {
    use super::{
        application_resource, dynamic_tool_failure, dynamic_tool_success,
        resolved_application_resource, target_resource, ComputerControlService,
    };
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn dynamic_tools_are_not_registered_before_sdk_is_ready() {
        let service = ComputerControlService::default();
        assert!(!service.sdk().status().initialized);
        assert_eq!(
            service
                .dynamic_tools_spec()
                .expect("an uninitialized SDK should register no tools"),
            json!([])
        );
        let _ = Arc::new(service);
    }

    #[test]
    fn desktop_scope_and_unscoped_input_are_rejected() {
        assert!(target_resource("click", &json!({"scope": "desktop"})).is_err());
        assert!(target_resource("click", &json!({"x": 10, "y": 20})).is_err());
        assert!(target_resource("click", &json!({"path": "notepad.exe"})).is_ok());
    }

    #[test]
    fn application_file_name_is_extracted_without_guessed_extension() {
        let system_notepad = application_resource(r"C:\\Windows\\System32\\notepad.exe");
        let custom_notepad = application_resource(r"D:\\tools\\notepad.exe");
        let custom_extension = application_resource(r"D:\\tools\\notepad.custom");
        let bare_notepad = application_resource("Notepad");

        assert_eq!(system_notepad.key, "application:notepad.exe");
        assert_eq!(custom_notepad.key, "application:notepad.exe");
        assert_eq!(custom_extension.key, "application:notepad.custom");
        assert_eq!(bare_notepad.key, "application:notepad");
        assert_eq!(system_notepad.display, "notepad.exe");
        assert_eq!(custom_notepad.display, "notepad.exe");
        assert_eq!(custom_extension.display, "notepad.custom");
        assert_eq!(bare_notepad.display, "Notepad");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn bare_windows_name_uses_windows_actual_executable_name() {
        let notepad = resolved_application_resource("notepad")
            .expect("Windows should resolve the built-in Notepad executable");
        let notepad_exe = resolved_application_resource("notepad.exe")
            .expect("Windows should resolve the built-in Notepad executable");

        assert_eq!(notepad.key, "application:notepad.exe");
        assert_eq!(notepad.display.to_ascii_lowercase(), "notepad.exe");
        assert_eq!(notepad.key, notepad_exe.key);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_pid_uses_proc_executable_name() {
        let target = target_resource("list_windows", &json!({"pid": std::process::id()}))
            .expect("Linux should resolve the current process through /proc");
        let current_name = std::env::current_exe()
            .expect("current executable path should resolve")
            .file_name()
            .expect("current executable should have a file name")
            .to_string_lossy()
            .to_lowercase();

        assert_eq!(target.key, format!("application:{current_name}"));
        assert_eq!(target.scope, "application");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pid_uses_proc_executable_name() {
        let target = target_resource("list_windows", &json!({"pid": std::process::id()}))
            .expect("macOS should resolve the current process through proc_pidpath");
        let current_name = std::env::current_exe()
            .expect("current executable path should resolve")
            .file_name()
            .expect("current executable should have a file name")
            .to_string_lossy()
            .to_lowercase();

        assert_eq!(target.key, format!("application:{current_name}"));
        assert_eq!(target.scope, "application");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_application_name_and_bundle_id_share_the_bundle_authorization() {
        let by_name = target_resource("launch_app", &json!({"name": "Calculator"}))
            .expect("macOS application name should resolve through system metadata");
        let by_bundle_id =
            target_resource("launch_app", &json!({"bundle_id": "com.apple.calculator"}))
                .expect("macOS bundle identifier should resolve through system metadata");

        assert_eq!(by_name.key, "application:calculator.app");
        assert_eq!(by_name.key, by_bundle_id.key);
        assert_eq!(by_name.display, by_bundle_id.display);
        assert_eq!(by_name.scope, by_bundle_id.scope);
    }

    #[test]
    fn unresolved_bare_name_is_not_given_an_extension() {
        assert!(resolved_application_resource("auracoder-cua-not-a-real-application").is_err());
    }

    #[test]
    fn target_scopes_are_classified() {
        assert_eq!(
            target_resource("start_session", &json!({}))
                .expect("session target should resolve")
                .scope,
            "metadata"
        );
        assert_eq!(
            target_resource("list_windows", &json!({}))
                .expect("observation target should resolve")
                .scope,
            "observation"
        );
        assert_eq!(
            target_resource("launch_app", &json!({"path": "notepad.exe"}))
                .expect("application target should resolve")
                .scope,
            "application"
        );
    }

    #[test]
    fn dynamic_tool_result_has_codex_content_items() {
        assert_eq!(dynamic_tool_success(json!({"ok": true}))["success"], true);
        assert_eq!(
            dynamic_tool_failure("permission_denied: no")["success"],
            false
        );
        let image = dynamic_tool_success(json!({
            "content": [{"type": "image", "data": "AQ==", "mimeType": "image/png"}]
        }));
        assert_eq!(image["contentItems"][0]["type"], "inputImage");
    }
}

#[derive(Default)]
struct AuthorizationState {
    pending: HashMap<String, PendingAuthorization>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerControlAuthorization {
    pub request_id: String,
    pub agent: String,
    pub tool: String,
    pub call_id: String,
    pub application: String,
    pub operation: String,
    pub scope: String,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone)]
struct TargetResource {
    key: String,
    display: String,
    scope: &'static str,
}

pub struct ComputerControlService {
    sdk: Arc<CuaDriverSdk>,
    state: Mutex<AuthorizationState>,
    app_handle: StdMutex<Option<AppHandle>>,
}

impl ComputerControlService {
    pub fn new(sdk: Arc<CuaDriverSdk>) -> Self {
        Self {
            sdk,
            state: Mutex::new(AuthorizationState::default()),
            app_handle: StdMutex::new(None),
        }
    }

    pub fn bind_app_handle(&self, handle: AppHandle) {
        if let Ok(mut current) = self.app_handle.lock() {
            *current = Some(handle);
        }
    }

    pub fn sdk_tool_specs(&self) -> Result<Vec<Value>, String> {
        if !self.sdk.status().initialized {
            return Ok(Vec::new());
        }

        let catalog = self
            .sdk
            .list_tools()
            .map_err(|error| service_error("sdk_unavailable", &error))?;
        let catalog_tools = catalog
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                service_error("sdk_invalid_catalog", "CUA SDK 的工具目录没有 tools 数组")
            })?;

        let mut tools = Vec::new();
        for spec in catalog_tools {
            let name = spec
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| service_error("sdk_invalid_catalog", "CUA SDK 工具缺少 name"))?;
            if spec.get("description").and_then(Value::as_str).is_none()
                || spec.get("inputSchema").is_none()
            {
                return Err(service_error(
                    "sdk_invalid_catalog",
                    &format!("CUA SDK 工具 `{name}` 缺少 description 或 inputSchema"),
                ));
            }
            if !spec["inputSchema"].is_object() {
                return Err(service_error(
                    "sdk_invalid_catalog",
                    &format!("CUA SDK 工具 `{name}` 的 inputSchema 不是 JSON 对象"),
                ));
            }
            tools.push(json!({
                "name": name,
                "description": spec["description"].clone(),
                "inputSchema": spec["inputSchema"].clone(),
            }));
        }

        if tools.is_empty() {
            return Err(service_error(
                "sdk_invalid_catalog",
                "CUA SDK 未返回任何电脑操作工具",
            ));
        }
        Ok(tools)
    }

    pub fn dynamic_tools_spec(&self) -> Result<Value, String> {
        let tools = self
            .sdk_tool_specs()?
            .into_iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "name": spec["name"].clone(),
                    "description": spec["description"].clone(),
                    "inputSchema": spec["inputSchema"].clone(),
                })
            })
            .collect::<Vec<_>>();
        if tools.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        Ok(json!([
            {
                "type": "namespace",
                "name": COMPUTER_CONTROL_NAMESPACE,
                "description": "AuraCoder 的电脑操作能力。每次实际调用都由 AuraCoder 在执行前申请授权。",
                "tools": tools
            }
        ]))
    }

    pub fn sdk(&self) -> Arc<CuaDriverSdk> {
        self.sdk.clone()
    }

    pub async fn active_authorizations(&self) -> Vec<ComputerControlAuthorization> {
        let mut authorizations = AppConfig::load_or_create()
            .map(|config| {
                let mut applications = HashMap::new();
                for authorization in config
                    .computer_control
                    .persistent_authorizations
                    .into_iter()
                    .filter(|authorization| authorization.target_key.starts_with("application:"))
                {
                    let application = resolved_application_resource(&authorization.application)
                        .unwrap_or_else(|_| application_resource(&authorization.application));
                    applications.entry(application.key).or_insert_with(|| {
                        ComputerControlAuthorization {
                            request_id: authorization.request_id,
                            agent: authorization.agent,
                            tool: authorization.tool,
                            call_id: authorization.call_id,
                            application: application.display,
                            operation: authorization.operation,
                            scope: authorization.scope,
                            thread_id: authorization.thread_id,
                            turn_id: authorization.turn_id,
                        }
                    });
                }
                applications.into_values().collect::<Vec<_>>()
            })
            .unwrap_or_default();
        authorizations.sort_by(|left, right| left.application.cmp(&right.application));
        authorizations
    }

    pub async fn revoke_authorization(&self, request_id: &str) -> bool {
        let request_id = request_id.trim();
        let revoked = AppConfig::mutate(|config| {
            let authorizations = &mut config.computer_control.persistent_authorizations;
            let Some(target_key) = authorizations
                .iter()
                .find(|authorization| authorization.request_id == request_id)
                .map(|authorization| {
                    if authorization.target_key.starts_with("application:") {
                        resolved_application_resource(&authorization.application)
                            .unwrap_or_else(|_| application_resource(&authorization.application))
                            .key
                    } else {
                        authorization.target_key.clone()
                    }
                })
            else {
                return Ok(false);
            };
            let count_before = authorizations.len();
            authorizations
                .retain(|authorization| !authorization_matches_target(authorization, &target_key));
            Ok(authorizations.len() != count_before)
        })
        .unwrap_or(false);
        let pending_response = {
            let mut state = self.state.lock().await;
            let pending = state.pending.remove(request_id);
            pending.map(|authorization| authorization.response)
        };
        let pending_was_cancelled = pending_response.is_some();
        if let Some(response) = pending_response {
            let _ = response.send(false);
        }
        revoked || pending_was_cancelled
    }

    fn has_persistent_authorization(target_key: &str) -> bool {
        AppConfig::load_or_create()
            .map(|config| {
                config
                    .computer_control
                    .persistent_authorizations
                    .iter()
                    .any(|authorization| authorization_matches_target(authorization, target_key))
            })
            .unwrap_or(false)
    }

    pub async fn respond(&self, request_id: &str, allowed: bool) -> Result<bool, String> {
        let pending = {
            let mut state = self.state.lock().await;
            state.pending.remove(request_id)
        };
        let Some(pending) = pending else {
            return Ok(false);
        };

        let PendingAuthorization {
            target_key,
            authorization,
            response,
        } = pending;
        if allowed {
            AppConfig::mutate(|config| {
                let authorizations = &mut config.computer_control.persistent_authorizations;
                authorizations
                    .retain(|existing| !authorization_matches_target(existing, &target_key));
                authorizations.push(ComputerControlAuthorizationConfig {
                    request_id: authorization.request_id.clone(),
                    target_key,
                    agent: authorization.agent.clone(),
                    tool: authorization.tool.clone(),
                    call_id: authorization.call_id.clone(),
                    application: authorization.application.clone(),
                    operation: authorization.operation.clone(),
                    scope: authorization.scope.clone(),
                    thread_id: authorization.thread_id.clone(),
                    turn_id: authorization.turn_id.clone(),
                });
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        }
        let _ = response.send(allowed);
        Ok(true)
    }

    pub async fn revoke_all(&self) {
        let pending = {
            let mut state = self.state.lock().await;
            state
                .pending
                .drain()
                .map(|(_, pending)| pending.response)
                .collect::<Vec<_>>()
        };
        for response in pending {
            let _ = response.send(false);
        }
    }

    pub async fn revoke_turn(&self, thread_id: &str, turn_id: Option<&str>) {
        let prefix = match turn_id.map(str::trim).filter(|value| !value.is_empty()) {
            Some(turn_id) => format!("{}\n{}\n", thread_id.trim(), turn_id),
            None => format!("{}\n", thread_id.trim()),
        };
        let pending = {
            let mut state = self.state.lock().await;
            let request_ids = state
                .pending
                .iter()
                .filter(|(_, pending)| {
                    format!(
                        "{}\n{}\n",
                        pending.authorization.thread_id, pending.authorization.turn_id
                    )
                    .starts_with(&prefix)
                })
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            request_ids
                .into_iter()
                .filter_map(|request_id| state.pending.remove(&request_id))
                .map(|pending| pending.response)
                .collect::<Vec<_>>()
        };
        for response in pending {
            let _ = response.send(false);
        }
    }

    pub async fn invoke_for_codex(
        &self,
        thread_id: &str,
        turn_id: &str,
        tool: &str,
        call_id: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.invoke_for_engine(
            "codex",
            thread_id,
            turn_id,
            tool,
            call_id,
            arguments,
            cancellation,
        )
        .await
    }

    pub async fn invoke_for_engine(
        &self,
        agent: &str,
        thread_id: &str,
        turn_id: &str,
        tool: &str,
        call_id: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        let agent = agent.trim();
        let thread_id = thread_id.trim();
        let turn_id = turn_id.trim();
        let tool = tool.trim();
        if agent.is_empty() || thread_id.is_empty() || turn_id.is_empty() {
            return Err(service_error(
                "invalid_request",
                "电脑操作请求缺少 agent、threadId 或 turnId",
            ));
        }
        let enabled = AppConfig::load_or_create()
            .map(|config| config.computer_control.enabled)
            .unwrap_or(false);
        if !enabled {
            return Err(service_error(
                "computer_control_disabled",
                "AuraCoder 的电脑操作能力开关未开启",
            ));
        }
        if !self.sdk.status().initialized {
            return Err(service_error(
                "sdk_unavailable",
                "CUA SDK 尚未就绪，AuraCoder 不会发起电脑操作授权",
            ));
        }
        let tool_is_available = self
            .sdk_tool_specs()?
            .iter()
            .any(|spec| spec.get("name").and_then(Value::as_str) == Some(tool));
        if !tool_is_available {
            return Err(service_error(
                "tool_not_available",
                &format!("CUA SDK 未提供电脑操作工具：{tool}"),
            ));
        }
        let arguments = normalize_arguments(arguments)?;
        let target = target_resource(tool, &arguments)?;
        if target_is_auracoder(&target.display) {
            return Err(service_error(
                "target_scope_mismatch",
                "AuraCoder 不允许把自身窗口作为电脑操作目标",
            ));
        }
        let operation = operation_kind(tool);
        if !Self::has_persistent_authorization(&target.key) {
            self.request_authorization(
                agent,
                thread_id,
                turn_id,
                tool,
                call_id,
                &target,
                operation,
                target.key.clone(),
                cancellation.clone(),
            )
            .await?;
        }
        self.sdk
            .invoke(tool, &arguments)
            .map_err(|error| service_error("sdk_unavailable", &error))
    }

    async fn request_authorization(
        &self,
        agent: &str,
        thread_id: &str,
        turn_id: &str,
        tool: &str,
        call_id: &str,
        target: &TargetResource,
        operation: &str,
        target_key: String,
        cancellation: CancellationToken,
    ) -> Result<(), String> {
        let request_id = Uuid::new_v4().to_string();
        let (response_tx, response_rx) = oneshot::channel();
        let request = ComputerControlAuthorization {
            request_id: request_id.clone(),
            agent: agent.to_string(),
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            application: target.display.clone(),
            operation: operation.to_string(),
            scope: target.scope.to_string(),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
        };
        {
            let mut state = self.state.lock().await;
            state.pending.insert(
                request_id.clone(),
                PendingAuthorization {
                    target_key,
                    authorization: request.clone(),
                    response: response_tx,
                },
            );
        }
        let emit_result = self
            .app_handle
            .lock()
            .map_err(|_| "电脑操作授权窗口状态已损坏".to_string())?
            .as_ref()
            .ok_or_else(|| "AuraCoder 窗口尚未就绪，无法发起电脑操作授权".to_string())?
            .emit(COMPUTER_CONTROL_APPROVAL_EVENT, request);
        if let Err(error) = emit_result {
            self.state.lock().await.pending.remove(&request_id);
            return Err(service_error(
                "authorization_required",
                &format!("无法显示电脑操作授权窗口：{error}"),
            ));
        }

        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(service_error("request_timeout", "电脑操作任务已取消")),
            result = timeout(APPROVAL_TIMEOUT, response_rx) => match result {
                Ok(Ok(true)) => Ok(()),
                Ok(Ok(false)) => Err(service_error("permission_denied", "用户拒绝了电脑操作授权")),
                Ok(Err(_)) => Err(service_error("authorization_required", "电脑操作授权请求已失效")),
                Err(_) => Err(service_error("request_timeout", "电脑操作授权等待超时")),
            },
        };

        self.state.lock().await.pending.remove(&request_id);
        result
    }
}
