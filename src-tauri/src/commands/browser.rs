use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Webview, WebviewBuilder, WebviewUrl,
};
use tokio::fs as tokio_fs;
use tokio::sync::oneshot;

const BROWSER_WEBVIEW_LABEL_PREFIX: &str = "browser-annotation-webview";
const DEFAULT_BROWSER_URL: &str = "https://www.jxrjkf.cn/";
const BROWSER_ANNOTATION_SELECTION_SCHEME: &str = "auracoder-browser-annotation";
const MAX_BROWSER_URL_CHARS: usize = 4_096;
const MAX_TARGET_LABEL_CHARS: usize = 600;
const MAX_BROWSER_COMMENT_CHARS: usize = 1_000;
const MAX_BROWSER_SCOPE_CHARS: usize = 512;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(12);

static BROWSER_WEBVIEW_CREATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static BROWSER_WEBVIEW_SCOPES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAnnotationRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAnnotationSelection {
    pub url: String,
    #[serde(default)]
    pub title: String,
    pub target_label: String,
    pub rect: BrowserAnnotationRect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAnnotationSubmission {
    pub selection: BrowserAnnotationSelection,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserAnnotationAttachmentPayload {
    pub file_name: String,
    pub file_path: String,
    pub size_bytes: u64,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserNavigatedEvent {
    scope: String,
    url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserAnnotationSubmissionEvent {
    scope: String,
    submission: BrowserAnnotationSubmission,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserAnnotationCanceledEvent {
    scope: String,
}

#[tauri::command]
pub async fn browser_show(
    app: AppHandle,
    scope: String,
    bounds: BrowserBounds,
    initial_url: Option<String>,
) -> Result<(), String> {
    validate_browser_bounds(&bounds)?;
    let scope = normalize_browser_scope(&scope)?;
    let initial_url = normalize_browser_url(initial_url.as_deref().unwrap_or(DEFAULT_BROWSER_URL))?;
    let create_handle = app.clone();
    let create_scope = scope.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_browser_webview(&create_handle, &create_scope, initial_url)
    })
    .await
    .map_err(|error| format!("Browser WebView initialization task failed: {error}"))??;

    hide_other_browser_webviews(&app, &scope)?;
    let webview = browser_webview(&app, &scope)?;
    set_browser_bounds(&webview, &bounds)?;
    webview.show().map_err(browser_error)
}

#[tauri::command]
pub fn browser_set_bounds(
    app: AppHandle,
    scope: String,
    bounds: BrowserBounds,
) -> Result<(), String> {
    validate_browser_bounds(&bounds)?;
    let scope = normalize_browser_scope(&scope)?;
    let webview = browser_webview(&app, &scope)?;
    set_browser_bounds(&webview, &bounds)
}

#[tauri::command]
pub fn browser_hide(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    if let Some(webview) = app.get_webview(&browser_webview_label_for_scope(&scope)?) {
        webview.hide().map_err(browser_error)?;
    }
    Ok(())
}

#[tauri::command]
pub fn browser_transfer_scope(from_scope: String, to_scope: String) -> Result<(), String> {
    let from_scope = normalize_browser_scope(&from_scope)?;
    let to_scope = normalize_browser_scope(&to_scope)?;
    if from_scope == to_scope {
        return Ok(());
    }

    let scopes = BROWSER_WEBVIEW_SCOPES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut scopes = scopes
        .lock()
        .map_err(|_| "Browser conversation registry is unavailable.".to_string())?;
    let Some(label) = scopes.remove(&from_scope) else {
        return Ok(());
    };
    if scopes.contains_key(&to_scope) {
        scopes.insert(from_scope, label);
        return Err("The target browser conversation already exists.".to_string());
    }
    scopes.insert(to_scope, label);
    Ok(())
}

#[tauri::command]
pub fn browser_navigate(app: AppHandle, scope: String, url: String) -> Result<String, String> {
    let scope = normalize_browser_scope(&scope)?;
    let url = normalize_browser_url(&url)?;
    let webview = browser_webview(&app, &scope)?;
    webview.navigate(url.clone()).map_err(browser_error)?;
    Ok(url.into())
}

#[tauri::command]
pub fn browser_reload(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    browser_webview(&app, &scope)?
        .reload()
        .map_err(browser_error)
}

#[tauri::command]
pub fn browser_go_back(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    browser_webview(&app, &scope)?
        .eval("window.history.back();")
        .map_err(browser_error)
}

#[tauri::command]
pub fn browser_go_forward(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    browser_webview(&app, &scope)?
        .eval("window.history.forward();")
        .map_err(browser_error)
}

#[tauri::command]
pub fn browser_set_annotation_enabled(
    app: AppHandle,
    scope: String,
    enabled: bool,
) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    // The WebView2 document-start script can race a fast first click after navigation.
    // Re-evaluating its idempotent installer here guarantees the helper exists before toggling it.
    let script = format!(
        "{BROWSER_ANNOTATION_SCRIPT}\nwindow.__PANES_BROWSER_ANNOTATION__?.setEnabled({enabled});"
    );
    browser_webview(&app, &scope)?
        .eval(script)
        .map_err(browser_error)
}

#[tauri::command]
pub fn browser_clear_pending_annotation(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    browser_webview(&app, &scope)?
        .eval(format!(
            "{BROWSER_ANNOTATION_SCRIPT}\nwindow.__PANES_BROWSER_ANNOTATION__?.setEnabled(false); window.__PANES_BROWSER_ANNOTATION__?.clearPending();"
        ))
        .map_err(browser_error)
}

#[tauri::command]
pub fn browser_clear_all_annotations(app: AppHandle, scope: String) -> Result<(), String> {
    let scope = normalize_browser_scope(&scope)?;
    browser_webview(&app, &scope)?
        .eval(format!(
            "{BROWSER_ANNOTATION_SCRIPT}\nwindow.__PANES_BROWSER_ANNOTATION__?.clearAll();"
        ))
        .map_err(browser_error)
}

#[tauri::command]
pub async fn browser_capture_annotation(
    app: AppHandle,
    scope: String,
    number: u32,
    selection: BrowserAnnotationSelection,
) -> Result<BrowserAnnotationAttachmentPayload, String> {
    if number == 0 {
        return Err("Annotation number must be greater than zero.".to_string());
    }
    let scope = normalize_browser_scope(&scope)?;
    validate_browser_selection(&selection)?;
    let webview = browser_webview(&app, &scope)?;
    let script = format!("window.__PANES_BROWSER_ANNOTATION__?.commit({number});");
    webview.eval(&script).map_err(browser_error)?;

    // CapturePreview is asynchronous in WebView2. A small render boundary makes sure the
    // outline and number bubble have reached the compositor before the PNG is captured.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let bytes = capture_browser_preview(&webview).await?;

    let attachment_dir = browser_attachment_dir(&app)?;
    tokio_fs::create_dir_all(&attachment_dir)
        .await
        .map_err(|error| format!("Could not create browser annotation directory: {error}"))?;
    let file_name = format!(
        "browser-annotation-{}-{number}.png",
        chrono::Utc::now().timestamp_millis()
    );
    let file_path = attachment_dir.join(&file_name);
    tokio_fs::write(&file_path, &bytes)
        .await
        .map_err(|error| format!("Could not save browser annotation screenshot: {error}"))?;

    Ok(BrowserAnnotationAttachmentPayload {
        file_name,
        file_path: file_path.to_string_lossy().to_string(),
        size_bytes: bytes.len() as u64,
        mime_type: "image/png".to_string(),
    })
}

fn browser_attachment_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("attachments").join("browser-annotations"))
        .map_err(|error| format!("Could not locate app data directory: {error}"))
}

fn browser_webview(app: &AppHandle, scope: &str) -> Result<Webview, String> {
    app.get_webview(&browser_webview_label_for_scope(scope)?)
        .ok_or_else(|| "Browser panel is not initialized.".to_string())
}

fn ensure_browser_webview(
    app: &AppHandle,
    scope: &str,
    initial_url: tauri::Url,
) -> Result<(), String> {
    let lock = BROWSER_WEBVIEW_CREATE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "Browser WebView initialization lock is unavailable.".to_string())?;
    let label = browser_webview_label_for_scope(scope)?;
    remember_browser_scope(scope, &label)?;
    if app.get_webview(&label).is_some() {
        return Ok(());
    }

    let main_webview = app
        .get_webview("main")
        .ok_or_else(|| "Main window is unavailable for the browser panel.".to_string())?;
    let main_window = main_webview.window();
    let annotation_app = app.clone();
    let navigation_app = app.clone();
    let annotation_label = label.clone();
    let navigation_label = label.clone();
    let annotation_fallback_scope = scope.to_string();
    let navigation_fallback_scope = scope.to_string();
    let webview = main_window
        .add_child(
            WebviewBuilder::new(&label, WebviewUrl::External(initial_url))
                .on_navigation(move |url| {
                    if url.scheme() == BROWSER_ANNOTATION_SELECTION_SCHEME {
                        if url.host_str() == Some("cancel") {
                            let _ = annotation_app.emit(
                                "browser:annotation-canceled",
                                BrowserAnnotationCanceledEvent {
                                    scope: browser_scope_for_webview_label(&annotation_label)
                                        .unwrap_or_else(|| annotation_fallback_scope.clone()),
                                },
                            );
                        } else if let Some(submission) = browser_annotation_submission_from_url(url) {
                            let _ = annotation_app.emit(
                                "browser:annotation-submitted",
                                BrowserAnnotationSubmissionEvent {
                                    scope: browser_scope_for_webview_label(&annotation_label)
                                        .unwrap_or_else(|| annotation_fallback_scope.clone()),
                                    submission,
                                },
                            );
                        }
                        return false;
                    }
                    matches!(url.scheme(), "http" | "https")
                })
                .on_page_load(move |webview, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                        if let Err(error) = webview.eval(BROWSER_ANNOTATION_SCRIPT) {
                            log::warn!("could not install browser annotation script after navigation: {error}");
                        }
                        let _ = navigation_app.emit(
                            "browser:navigated",
                            BrowserNavigatedEvent {
                                scope: browser_scope_for_webview_label(&navigation_label)
                                    .unwrap_or_else(|| navigation_fallback_scope.clone()),
                                url: payload.url().to_string(),
                            },
                        );
                    }
                })
                .initialization_script(BROWSER_ANNOTATION_SCRIPT),
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(1.0, 1.0),
        )
        .map_err(browser_error)?;
    webview.hide().map_err(browser_error)
}

fn normalize_browser_scope(value: &str) -> Result<String, String> {
    let scope = value.trim();
    if scope.is_empty() || scope.len() > MAX_BROWSER_SCOPE_CHARS {
        return Err("Browser conversation scope is invalid.".to_string());
    }
    Ok(scope.to_string())
}

fn browser_webview_label(scope: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in scope.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{BROWSER_WEBVIEW_LABEL_PREFIX}-{hash:016x}")
}

fn browser_webview_label_for_scope(scope: &str) -> Result<String, String> {
    let scopes = BROWSER_WEBVIEW_SCOPES.get_or_init(|| Mutex::new(HashMap::new()));
    let scopes = scopes
        .lock()
        .map_err(|_| "Browser conversation registry is unavailable.".to_string())?;
    Ok(scopes
        .get(scope)
        .cloned()
        .unwrap_or_else(|| browser_webview_label(scope)))
}

fn browser_scope_for_webview_label(label: &str) -> Option<String> {
    let scopes = BROWSER_WEBVIEW_SCOPES.get_or_init(|| Mutex::new(HashMap::new()));
    let scopes = scopes.lock().ok()?;
    scopes
        .iter()
        .find_map(|(scope, candidate)| (candidate == label).then(|| scope.clone()))
}

fn remember_browser_scope(scope: &str, label: &str) -> Result<(), String> {
    let scopes = BROWSER_WEBVIEW_SCOPES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut scopes = scopes
        .lock()
        .map_err(|_| "Browser conversation registry is unavailable.".to_string())?;
    if let Some((other_scope, _)) = scopes.iter().find(|(other_scope, other_label)| {
        other_scope.as_str() != scope && other_label.as_str() == label
    }) {
        return Err(format!(
            "Browser conversation scope collision with {other_scope}."
        ));
    }
    scopes.insert(scope.to_string(), label.to_string());
    Ok(())
}

fn hide_other_browser_webviews(app: &AppHandle, active_scope: &str) -> Result<(), String> {
    let active_label = browser_webview_label_for_scope(active_scope)?;
    let scopes = BROWSER_WEBVIEW_SCOPES.get_or_init(|| Mutex::new(HashMap::new()));
    let labels = scopes
        .lock()
        .map_err(|_| "Browser conversation registry is unavailable.".to_string())?
        .values()
        .filter(|label| label.as_str() != active_label.as_str())
        .cloned()
        .collect::<Vec<_>>();
    for label in labels {
        if let Some(webview) = app.get_webview(&label) {
            webview.hide().map_err(browser_error)?;
        }
    }
    Ok(())
}

fn normalize_browser_url(value: &str) -> Result<tauri::Url, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Browser URL cannot be empty.".to_string());
    }
    if value.len() > MAX_BROWSER_URL_CHARS {
        return Err("Browser URL is too long.".to_string());
    }
    let with_scheme = if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let url =
        tauri::Url::parse(&with_scheme).map_err(|error| format!("Invalid browser URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Browser only supports http and https URLs.".to_string());
    }
    Ok(url)
}

fn validate_browser_bounds(bounds: &BrowserBounds) -> Result<(), String> {
    let values = [bounds.x, bounds.y, bounds.width, bounds.height];
    if values.iter().any(|value| !value.is_finite())
        || bounds.width < 1.0
        || bounds.height < 1.0
        || bounds.width > 20_000.0
        || bounds.height > 20_000.0
        || bounds.x < 0.0
        || bounds.y < 0.0
    {
        return Err("Browser bounds are invalid.".to_string());
    }
    Ok(())
}

fn set_browser_bounds(webview: &Webview, bounds: &BrowserBounds) -> Result<(), String> {
    webview
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(browser_error)?;
    webview
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(browser_error)
}

fn validate_browser_selection(selection: &BrowserAnnotationSelection) -> Result<(), String> {
    if selection.url.trim().is_empty() || selection.url.len() > MAX_BROWSER_URL_CHARS {
        return Err("Browser annotation URL is invalid.".to_string());
    }
    if selection.target_label.trim().is_empty()
        || selection.target_label.len() > MAX_TARGET_LABEL_CHARS
    {
        return Err("Browser annotation target is invalid.".to_string());
    }
    let rect = &selection.rect;
    let values = [rect.x, rect.y, rect.width, rect.height];
    if values.iter().any(|value| !value.is_finite())
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.width > 100_000.0
        || rect.height > 100_000.0
    {
        return Err("Browser annotation target bounds are invalid.".to_string());
    }
    Ok(())
}

fn validate_browser_annotation_submission(
    submission: &BrowserAnnotationSubmission,
) -> Result<(), String> {
    validate_browser_selection(&submission.selection)?;
    let comment = submission.comment.trim();
    if comment.is_empty() || comment.chars().count() > MAX_BROWSER_COMMENT_CHARS {
        return Err("Browser annotation comment is invalid.".to_string());
    }
    Ok(())
}

fn browser_annotation_submission_from_url(url: &tauri::Url) -> Option<BrowserAnnotationSubmission> {
    if url.scheme() != BROWSER_ANNOTATION_SELECTION_SCHEME {
        return None;
    }
    let submission = url
        .query_pairs()
        .find(|(key, _)| key == "submission")
        .and_then(|(_, value)| serde_json::from_str::<BrowserAnnotationSubmission>(&value).ok())?;
    validate_browser_annotation_submission(&submission).ok()?;
    Some(submission)
}

fn browser_error(error: impl std::fmt::Display) -> String {
    format!("Browser WebView error: {error}")
}

#[cfg(target_os = "windows")]
async fn capture_browser_preview(webview: &Webview) -> Result<Vec<u8>, String> {
    use webview2_com::{
        CapturePreviewCompletedHandler,
        Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
    };
    use windows::Win32::{
        Foundation::HGLOBAL,
        System::Com::{
            IStream, StructuredStorage::CreateStreamOnHGlobal, STATFLAG_NONAME, STATSTG,
            STREAM_SEEK_SET,
        },
    };

    type CaptureSender = Arc<Mutex<Option<oneshot::Sender<Result<Vec<u8>, String>>>>>;
    fn send_capture_result(sender: &CaptureSender, result: Result<Vec<u8>, String>) {
        if let Ok(mut sender) = sender.lock() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(result);
            }
        }
    }
    fn read_stream(stream: &IStream) -> Result<Vec<u8>, String> {
        let mut stat = STATSTG::default();
        unsafe {
            stream
                .Stat(&mut stat, STATFLAG_NONAME)
                .map_err(|error| format!("Could not inspect screenshot stream: {error}"))?;
            stream
                .Seek(0, STREAM_SEEK_SET, None)
                .map_err(|error| format!("Could not rewind screenshot stream: {error}"))?;
        }
        let size = usize::try_from(stat.cbSize)
            .map_err(|_| "Browser screenshot is too large.".to_string())?;
        if size == 0 {
            return Err("Browser screenshot is empty.".to_string());
        }
        let mut bytes = vec![0_u8; size];
        let mut total_read = 0_usize;
        while total_read < size {
            let chunk_len = (size - total_read).min(u32::MAX as usize) as u32;
            let mut read = 0_u32;
            unsafe {
                stream
                    .Read(
                        bytes[total_read..].as_mut_ptr().cast(),
                        chunk_len,
                        Some(&mut read),
                    )
                    .ok()
                    .map_err(|error| format!("Could not read screenshot stream: {error}"))?;
            }
            if read == 0 {
                break;
            }
            total_read += read as usize;
        }
        bytes.truncate(total_read);
        if bytes.is_empty() {
            return Err("Browser screenshot is empty.".to_string());
        }
        Ok(bytes)
    }

    let (sender, receiver) = oneshot::channel();
    let sender: CaptureSender = Arc::new(Mutex::new(Some(sender)));
    let sender_for_webview = sender.clone();
    webview
        .with_webview(move |platform| {
            let result = (|| -> Result<(), String> {
                let controller = platform.controller();
                let core = unsafe { controller.CoreWebView2() }
                    .map_err(|error| format!("Could not access WebView2: {error}"))?;
                let stream = unsafe { CreateStreamOnHGlobal(HGLOBAL::default(), true) }
                    .map_err(|error| format!("Could not create screenshot stream: {error}"))?;
                let stream_for_callback = stream.clone();
                let sender_for_callback = sender_for_webview.clone();
                let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
                    let bytes = result
                        .map_err(|error| format!("WebView2 screenshot failed: {error}"))
                        .and_then(|_| read_stream(&stream_for_callback));
                    send_capture_result(&sender_for_callback, bytes);
                    Ok(())
                }));
                unsafe {
                    core.CapturePreview(
                        COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                        &stream,
                        &handler,
                    )
                }
                .map_err(|error| format!("Could not start browser screenshot capture: {error}"))?;
                Ok(())
            })();
            if let Err(error) = result {
                send_capture_result(&sender_for_webview, Err(error));
            }
        })
        .map_err(browser_error)?;

    tokio::time::timeout(CAPTURE_TIMEOUT, receiver)
        .await
        .map_err(|_| "Browser screenshot timed out.".to_string())?
        .map_err(|_| "Browser screenshot channel closed unexpectedly.".to_string())?
}

#[cfg(not(target_os = "windows"))]
async fn capture_browser_preview(_webview: &Webview) -> Result<Vec<u8>, String> {
    Err("Browser screenshot annotations currently require Windows.".to_string())
}

const BROWSER_ANNOTATION_SCRIPT: &str = r#"
(() => {
  if (window.__PANES_BROWSER_ANNOTATION__) return;

  const state = {
    enabled: false,
    hovered: null,
    selected: null,
    overlay: null,
    pinLayer: null,
    annotations: [],
    composer: null,
    refreshQueued: false,
  };
  const clampText = (value, length) => String(value || "").replace(/\s+/g, " ").trim().slice(0, length);
  const targetLabel = (element) => {
    const tag = element.tagName ? element.tagName.toLowerCase() : "element";
    const text = clampText(element.innerText || element.getAttribute?.("aria-label") || element.textContent, 180);
    return text ? `${tag}: ${text}` : tag;
  };
  const validTarget = (element) => {
    if (!(element instanceof Element) || !element.isConnected) return false;
    if (element.id === "__auracoder-browser-annotation-root") return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 1 && rect.height > 1;
  };
  const ensureOverlay = () => {
    if (state.overlay) return;
    const mount = () => {
      if (state.overlay || !document.documentElement) return;
      const root = document.createElement("div");
      root.id = "__auracoder-browser-annotation-root";
      root.style.pointerEvents = "none";
      const shadow = root.attachShadow({ mode: "closed" });
      shadow.innerHTML = `
        <style>
          .layer { position: fixed; inset: 0; pointer-events: none; z-index: 2147483647; font-family: ui-sans-serif, system-ui, sans-serif; }
          .outline { position: fixed; box-sizing: border-box; border: 2px solid #1677ff; border-radius: 2px; box-shadow: 0 0 0 1px rgba(22,119,255,.2); display: none; }
          .pin { position: fixed; box-sizing: border-box; border: 2px solid #1677ff; border-radius: 2px; box-shadow: 0 0 0 1px rgba(22,119,255,.2); pointer-events: none; }
          .bubble { position: absolute; top: -15px; right: -15px; min-width: 28px; height: 28px; padding: 0 7px; box-sizing: border-box; border: 2px solid #fff; border-radius: 999px; color: #fff; background: #1677ff; font: inherit; font-size: 14px; font-weight: 700; line-height: 24px; text-align: center; box-shadow: 0 1px 4px rgba(0,0,0,.24); cursor: pointer; pointer-events: auto; }
          .bubble:hover { background: #0958d9; }
          .comment-popover { position: fixed; width: min(310px, calc(100vw - 16px)); display: flex; align-items: center; gap: 6px; padding: 7px; box-sizing: border-box; border: 1px solid rgba(22,119,255,.72); border-radius: 7px; color: #1f1f1f; background: #fff; box-shadow: 0 8px 26px rgba(0,0,0,.2); pointer-events: auto; }
          .comment-input { min-width: 0; height: 28px; flex: 1; padding: 0 8px; border: 1px solid #d9d9d9; border-radius: 4px; outline: none; color: #1f1f1f; background: #fff; font: inherit; font-size: 12px; }
          .comment-input:focus { border-color: #1677ff; box-shadow: 0 0 0 2px rgba(22,119,255,.14); }
          .comment-button { height: 28px; padding: 0 8px; border: 1px solid #d9d9d9; border-radius: 4px; color: #595959; background: #fff; font: inherit; font-size: 12px; cursor: pointer; }
          .comment-button:hover { border-color: #1677ff; color: #1677ff; }
          .comment-submit { border-color: #1677ff; color: #fff; background: #1677ff; }
          .comment-submit:hover { color: #fff; background: #0958d9; }
        </style>
        <div class="layer"><div class="outline"></div><div class="pins"></div></div>`;
      document.documentElement.appendChild(root);
      state.overlay = shadow.querySelector(".outline");
      state.pinLayer = shadow.querySelector(".pins");
    };
    mount();
    if (!state.overlay) document.addEventListener("DOMContentLoaded", mount, { once: true });
  };
  const setRect = (node, rect) => {
    if (!node || !rect) return;
    node.style.left = `${Math.round(rect.left)}px`;
    node.style.top = `${Math.round(rect.top)}px`;
    node.style.width = `${Math.max(1, Math.round(rect.width))}px`;
    node.style.height = `${Math.max(1, Math.round(rect.height))}px`;
  };
  const showHovered = () => {
    ensureOverlay();
    if (!state.overlay || !state.hovered || !state.enabled) return;
    setRect(state.overlay, state.hovered.getBoundingClientRect());
    state.overlay.style.display = "block";
  };
  const showSelected = () => {
    ensureOverlay();
    if (!state.overlay || !state.selected) return;
    setRect(state.overlay, state.selected.getBoundingClientRect());
    state.overlay.style.display = "block";
  };
  const hideHovered = () => {
    if (state.overlay) state.overlay.style.display = "none";
  };
  const selectAt = (event) => {
    const element = document.elementFromPoint(event.clientX, event.clientY);
    if (!validTarget(element)) return null;
    return element;
  };
  const updateHovered = (event) => {
    if (!state.enabled) return;
    const next = selectAt(event);
    if (next === state.hovered) return;
    state.hovered = next;
    if (next) showHovered(); else hideHovered();
  };
  const createSelection = (element) => {
    const rect = element.getBoundingClientRect();
    return {
      url: location.href,
      title: document.title || "",
      targetLabel: targetLabel(element),
      rect: { x: rect.left, y: rect.top, width: rect.width, height: rect.height }
    };
  };
  const notifyCanceled = () => {
    location.href = "auracoder-browser-annotation://cancel";
  };
  const submitSelection = (element, comment) => {
    const submission = { selection: createSelection(element), comment };
    // Navigation interception is available for every child WebView. Unlike the
    // WebMessage bridge it does not depend on a page-provided messaging object.
    location.href = `auracoder-browser-annotation://submit?submission=${encodeURIComponent(JSON.stringify(submission))}`;
  };
  const setEnabled = (enabled) => {
    state.enabled = Boolean(enabled);
    if (!state.enabled) hideHovered();
  };
  const closeComposer = () => {
    if (state.composer) state.composer.remove();
    state.composer = null;
  };
  const clearPending = () => {
    closeComposer();
    state.selected = null;
    state.hovered = null;
    hideHovered();
  };
  const removeAnnotation = (annotation) => {
    annotation.pin.remove();
    state.annotations = state.annotations.filter((entry) => entry !== annotation);
  };
  const refreshAnnotations = () => {
    state.annotations.slice().forEach((annotation) => {
      if (!validTarget(annotation.element)) {
        removeAnnotation(annotation);
        return;
      }
      setRect(annotation.pin, annotation.element.getBoundingClientRect());
    });
  };
  const scheduleRefresh = () => {
    if (state.refreshQueued) return;
    state.refreshQueued = true;
    requestAnimationFrame(() => {
      state.refreshQueued = false;
      refreshAnnotations();
      if (state.enabled) showHovered();
      else if (state.selected) showSelected();
    });
  };
  const clearAll = () => {
    state.annotations.slice().forEach(removeAnnotation);
  };
  const openComposer = (element, event) => {
    ensureOverlay();
    if (!state.pinLayer) return;
    closeComposer();
    const composer = document.createElement("form");
    composer.className = "comment-popover";
    const input = document.createElement("input");
    input.className = "comment-input";
    input.placeholder = "输入标注说明";
    input.maxLength = 1000;
    input.autocomplete = "off";
    input.setAttribute("aria-label", "标注说明");
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "comment-button";
    cancel.textContent = "取消";
    const submit = document.createElement("button");
    submit.type = "submit";
    submit.className = "comment-button comment-submit";
    submit.textContent = "确定";
    composer.append(input, cancel, submit);
    composer.style.left = `${event.clientX + 12}px`;
    composer.style.top = `${event.clientY + 12}px`;
    state.pinLayer.appendChild(composer);
    state.composer = composer;
    const cancelPending = () => {
      clearPending();
      notifyCanceled();
    };
    cancel.addEventListener("click", cancelPending);
    input.addEventListener("keydown", (keyEvent) => {
      if (keyEvent.key === "Escape") {
        keyEvent.preventDefault();
        cancelPending();
      }
    });
    composer.addEventListener("submit", (submitEvent) => {
      submitEvent.preventDefault();
      const comment = input.value.trim();
      if (!comment) {
        input.focus();
        return;
      }
      closeComposer();
      setEnabled(false);
      hideHovered();
      submitSelection(element, comment);
    });
    requestAnimationFrame(() => {
      if (state.composer !== composer) return;
      const rect = composer.getBoundingClientRect();
      composer.style.left = `${Math.max(8, Math.min(event.clientX + 12, window.innerWidth - rect.width - 8))}px`;
      composer.style.top = `${Math.max(8, Math.min(event.clientY + 12, window.innerHeight - rect.height - 8))}px`;
      input.focus();
    });
  };
  const commit = (number) => {
    ensureOverlay();
    if (!state.pinLayer || !validTarget(state.selected)) return;
    const element = state.selected;
    const pin = document.createElement("div");
    pin.className = "pin";
    setRect(pin, element.getBoundingClientRect());
    const bubble = document.createElement("button");
    bubble.type = "button";
    bubble.className = "bubble";
    bubble.textContent = String(number);
    bubble.title = "点击删除标注";
    pin.appendChild(bubble);
    state.pinLayer.appendChild(pin);
    const annotation = { element, pin };
    state.annotations.push(annotation);
    bubble.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      removeAnnotation(annotation);
    });
    clearPending();
  };
  document.addEventListener("pointermove", updateHovered, true);
  document.addEventListener("click", (event) => {
    if (!state.enabled) return;
    const element = selectAt(event);
    if (!element) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    state.selected = element;
    state.hovered = element;
    showHovered();
    setEnabled(false);
    showSelected();
    openComposer(element, event);
  }, true);
  window.addEventListener("scroll", scheduleRefresh, true);
  window.addEventListener("resize", scheduleRefresh);
  window.__PANES_BROWSER_ANNOTATION__ = { setEnabled, clearPending, clearAll, commit };
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_navigation_recovers_the_submitted_comment_and_dom_node() {
        let selection = BrowserAnnotationSelection {
            url: "https://www.jxrjkf.cn/".to_string(),
            title: "云祥软件".to_string(),
            target_label: "div: 嘉兴市 雨 25℃".to_string(),
            rect: BrowserAnnotationRect {
                x: 48.0,
                y: 36.0,
                width: 382.0,
                height: 37.0,
            },
        };
        let submission = BrowserAnnotationSubmission {
            selection: selection.clone(),
            comment: "文字太小了".to_string(),
        };
        let mut url = tauri::Url::parse("auracoder-browser-annotation://submit").unwrap();
        url.query_pairs_mut()
            .append_pair("submission", &serde_json::to_string(&submission).unwrap());

        assert_eq!(
            browser_annotation_submission_from_url(&url)
                .expect("submission should be decoded")
                .selection
                .target_label,
            selection.target_label
        );
    }

    #[test]
    fn non_annotation_navigation_is_not_treated_as_a_selection() {
        let url = tauri::Url::parse("https://www.jxrjkf.cn/").unwrap();
        assert!(browser_annotation_submission_from_url(&url).is_none());
    }

    #[test]
    fn annotation_comment_limit_counts_characters_for_chinese_text() {
        let submission = BrowserAnnotationSubmission {
            selection: BrowserAnnotationSelection {
                url: "https://www.jxrjkf.cn/".to_string(),
                title: "云祥软件".to_string(),
                target_label: "div: 嘉兴市 雨 25℃".to_string(),
                rect: BrowserAnnotationRect {
                    x: 48.0,
                    y: 36.0,
                    width: 382.0,
                    height: 37.0,
                },
            },
            comment: "标".repeat(MAX_BROWSER_COMMENT_CHARS),
        };

        assert!(validate_browser_annotation_submission(&submission).is_ok());
    }

    #[test]
    fn each_conversation_scope_has_a_stable_distinct_webview_label() {
        let first = browser_webview_label("thread:conversation-a");
        let second = browser_webview_label("thread:conversation-b");

        assert_eq!(first, browser_webview_label("thread:conversation-a"));
        assert_ne!(first, second);
    }

    #[test]
    fn transferring_a_draft_preserves_its_webview_identity_for_the_new_thread() {
        let draft_scope = "draft:browser-transfer-test";
        let thread_scope = "thread:browser-transfer-test";
        let label = browser_webview_label(draft_scope);

        remember_browser_scope(draft_scope, &label).expect("draft scope should be registered");
        browser_transfer_scope(draft_scope.to_string(), thread_scope.to_string())
            .expect("draft scope should be reassigned to the created thread");

        assert_eq!(
            browser_webview_label_for_scope(thread_scope).expect("thread label should resolve"),
            label
        );
        assert_eq!(
            browser_webview_label_for_scope(draft_scope).expect("draft fallback should resolve"),
            browser_webview_label(draft_scope)
        );
    }
}
