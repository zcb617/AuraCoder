use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::{
    computer_control_sdk::{is_supported_platform, CuaDriverSdkStatus, CuaWaylandHelperStatus},
    computer_control_service::{ComputerControlAuthorization, ComputerControlService},
    config::app_config::AppConfig,
    state::AppState,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerControlSdkStatusDto {
    state: String,
    initialized: bool,
    abi_version: Option<String>,
    driver_version: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerControlAdapterStatusDto {
    id: String,
    name: String,
    built_in: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerControlStatusDto {
    supported: bool,
    enabled: bool,
    sdk: ComputerControlSdkStatusDto,
    wayland_helper: CuaWaylandHelperStatus,
    adapters: Vec<ComputerControlAdapterStatusDto>,
    current_authorizations: Vec<ComputerControlAuthorization>,
}

fn sdk_state(enabled: bool, status: &CuaDriverSdkStatus) -> &'static str {
    if !is_supported_platform() {
        return "unsupported";
    }
    if !enabled {
        return "disabled";
    }
    if status.initialized {
        return "ready";
    }
    if status.error.is_some() {
        return "failed";
    }
    "uninitialized"
}

fn build_status(
    enabled: bool,
    sdk_status: CuaDriverSdkStatus,
    wayland_helper: CuaWaylandHelperStatus,
    current_authorizations: Vec<ComputerControlAuthorization>,
) -> ComputerControlStatusDto {
    ComputerControlStatusDto {
        supported: is_supported_platform(),
        enabled,
        sdk: ComputerControlSdkStatusDto {
            state: sdk_state(enabled, &sdk_status).to_string(),
            initialized: sdk_status.initialized,
            abi_version: sdk_status.abi_version,
            driver_version: sdk_status.driver_version,
            error: sdk_status.error,
        },
        wayland_helper,
        adapters: vec![
            ComputerControlAdapterStatusDto {
                id: "codex".to_string(),
                name: "Codex".to_string(),
                built_in: true,
            },
            ComputerControlAdapterStatusDto {
                id: "claude".to_string(),
                name: "Claude Code".to_string(),
                built_in: true,
            },
            ComputerControlAdapterStatusDto {
                id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                built_in: true,
            },
        ],
        current_authorizations,
    }
}

async fn current_status(
    service: Arc<ComputerControlService>,
) -> Result<ComputerControlStatusDto, String> {
    let sdk = service.sdk();
    let config_task = tokio::task::spawn_blocking(AppConfig::load_or_create);
    let helper_sdk = sdk.clone();
    let helper_task = tokio::task::spawn_blocking(move || helper_sdk.wayland_helper_status());
    let authorizations_task = service.active_authorizations();
    let (config_result, helper_result, current_authorizations) =
        tokio::join!(config_task, helper_task, authorizations_task);
    let config = config_result
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let wayland_helper = helper_result.map_err(|error| error.to_string())?;

    Ok(build_status(
        config.computer_control.enabled,
        sdk.status(),
        wayland_helper,
        current_authorizations,
    ))
}

#[tauri::command]
pub async fn get_computer_control_settings_status(
    state: State<'_, AppState>,
) -> Result<ComputerControlStatusDto, String> {
    current_status(state.computer_control_service.clone()).await
}

#[tauri::command]
pub async fn set_computer_control_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<ComputerControlStatusDto, String> {
    if enabled && !is_supported_platform() {
        return Err("computer control is not supported on this platform".to_string());
    }

    let config_write_lock = state.config_write_lock.clone();
    let service = state.computer_control_service.clone();
    let guard = config_write_lock.lock_owned().await;
    tokio::task::spawn_blocking(move || {
        AppConfig::mutate(|config| {
            config.computer_control.enabled = enabled;
            Ok(())
        })
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    drop(guard);

    let sdk = service.sdk();
    if enabled {
        let initialization_sdk = sdk.clone();
        let initialization_result =
            tokio::task::spawn_blocking(move || initialization_sdk.initialize())
                .await
                .map_err(|error| error.to_string())?;
        if let Err(error) = initialization_result {
            log::error!("failed to initialize CUA SDK after enabling computer control: {error}");
        }
        let helper_sdk = sdk.clone();
        let helper_result =
            tokio::task::spawn_blocking(move || helper_sdk.restore_wayland_helper_if_installed())
                .await
                .map_err(|error| error.to_string())?;
        if let Err(error) = helper_result {
            log::warn!(
                "failed to restore installed CUA Wayland helper after enabling computer control: {error}"
            );
        }
    } else {
        service.revoke_all().await;
        tokio::task::spawn_blocking(move || sdk.shutdown())
            .await
            .map_err(|error| error.to_string())??;
    }

    current_status(service).await
}

#[tauri::command]
pub async fn install_computer_control_wayland_helper(
    state: State<'_, AppState>,
) -> Result<CuaWaylandHelperStatus, String> {
    let sdk = state.computer_control_service.sdk();
    tokio::task::spawn_blocking(move || sdk.install_wayland_helper())
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn respond_computer_control_approval(
    state: State<'_, AppState>,
    request_id: String,
    allowed: bool,
) -> Result<(), String> {
    if state
        .computer_control_service
        .respond(request_id.trim(), allowed)
        .await?
    {
        Ok(())
    } else {
        Err("computer control authorization was not found".to_string())
    }
}

#[tauri::command]
pub async fn revoke_computer_control_authorization(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<ComputerControlStatusDto, String> {
    let service = state.computer_control_service.clone();
    if !service.revoke_authorization(request_id.trim()).await {
        return Err("computer control authorization was not found".to_string());
    }
    current_status(service).await
}

#[cfg(test)]
mod tests {
    use super::{build_status, sdk_state};
    use crate::computer_control_sdk::{is_supported_platform, CuaDriverSdkStatus};

    fn sdk_status(initialized: bool, error: Option<&str>) -> CuaDriverSdkStatus {
        CuaDriverSdkStatus {
            initialized,
            resource_dir: None,
            library_path: None,
            abi_version: Some("1.1".to_string()),
            driver_version: None,
            embedded: Some(true),
            error: error.map(str::to_string),
        }
    }

    fn wayland_helper_status() -> crate::computer_control_sdk::CuaWaylandHelperStatus {
        crate::computer_control_sdk::CuaWaylandHelperStatus {
            supported: cfg!(all(target_os = "linux", target_arch = "x86_64")),
            wayland: false,
            installed: false,
            running: false,
        }
    }

    #[test]
    fn enabled_but_uninitialized_sdk_is_not_reported_as_ready() {
        let status = sdk_status(false, None);
        let result = build_status(true, status.clone(), wayland_helper_status(), Vec::new());

        if is_supported_platform() {
            assert_eq!(sdk_state(false, &status), "disabled");
            assert_eq!(sdk_state(true, &status), "uninitialized");
            assert_eq!(result.sdk.state, "uninitialized");
        } else {
            assert_eq!(sdk_state(false, &status), "unsupported");
            assert_eq!(sdk_state(true, &status), "unsupported");
            assert_eq!(result.sdk.state, "unsupported");
        }
        assert!(!result.sdk.initialized);
        assert_eq!(result.adapters.len(), 3);
        assert!(result.adapters.iter().all(|adapter| adapter.built_in));
    }

    #[test]
    fn initialized_sdk_is_ready_and_failed_sdk_is_reported() {
        if is_supported_platform() {
            assert_eq!(sdk_state(true, &sdk_status(true, None)), "ready");
            assert_eq!(
                sdk_state(true, &sdk_status(false, Some("load failed"))),
                "failed"
            );
        }
    }
}
