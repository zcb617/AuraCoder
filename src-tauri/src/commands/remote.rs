use tauri::{AppHandle, Emitter, State};

use crate::{config::app_config::AppConfig, remote::RemoteAccessStatusDto, state::AppState};

#[tauri::command]
pub async fn get_remote_access_status(
    state: State<'_, AppState>,
) -> Result<RemoteAccessStatusDto, String> {
    Ok(state.remote_access.status().await)
}

#[tauri::command]
pub async fn set_remote_access_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<RemoteAccessStatusDto, String> {
    let _write_guard = state.config_write_lock.lock().await;
    let config = tokio::task::spawn_blocking(move || {
        AppConfig::mutate(|config| {
            config.remote_access.enabled = enabled;
            if enabled {
                config.remote_access.ensure_identity();
            }
            Ok(config.remote_access.clone())
        })
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    state
        .remote_access
        .configure(app, state.inner().clone(), config)
        .await;
    Ok(state.remote_access.status().await)
}

#[tauri::command]
pub async fn regenerate_remote_access_identity(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RemoteAccessStatusDto, String> {
    let _write_guard = state.config_write_lock.lock().await;
    let config = tokio::task::spawn_blocking(move || {
        AppConfig::mutate(|config| {
            config.remote_access.regenerate_identity();
            Ok(config.remote_access.clone())
        })
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    state
        .remote_access
        .configure(app, state.inner().clone(), config)
        .await;
    Ok(state.remote_access.status().await)
}

#[tauri::command]
pub async fn refresh_remote_pairing_token(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RemoteAccessStatusDto, String> {
    let status = state.remote_access.refresh_pairing_token().await;
    let _ = app.emit("remote-access-updated", status.clone());
    Ok(status)
}

#[tauri::command]
pub async fn revoke_remote_device(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: String,
) -> Result<RemoteAccessStatusDto, String> {
    let device_id = device_id.trim().to_string();
    if device_id.is_empty() {
        return Err("device_id is required".to_string());
    }
    let _write_guard = state.config_write_lock.lock().await;
    let config = tokio::task::spawn_blocking(move || {
        AppConfig::mutate(|config| {
            let removed = if device_id == "legacy" {
                let existed = !config.remote_access.device_credential.is_empty();
                config.remote_access.device_credential.clear();
                existed
            } else {
                let previous_len = config.remote_access.devices.len();
                config
                    .remote_access
                    .devices
                    .retain(|device| device.id != device_id);
                config.remote_access.devices.len() != previous_len
            };
            if !removed {
                return Err(anyhow::anyhow!("Remote device was not found"));
            }
            Ok(config.remote_access.clone())
        })
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    state
        .remote_access
        .configure(app, state.inner().clone(), config)
        .await;
    Ok(state.remote_access.status().await)
}
