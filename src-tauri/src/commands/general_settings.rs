use crate::config::app_config::AppConfig;
use tauri::command;

/// 读取 GPU 加速设置；未持久化时返回当前平台默认值。
#[command]
pub async fn get_gpu_acceleration_enabled() -> Result<bool, String> {
    tokio::task::spawn_blocking(|| {
        AppConfig::load_or_create()
            .map(|config| {
                config.general.gpu_acceleration_enabled.unwrap_or_else(|| {
                    // 平台默认值：Linux 关闭，其他开启
                    !cfg!(target_os = "linux")
                })
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 保存用户选择的 GPU 加速设置，供应用下次启动时应用。
#[command]
pub async fn set_gpu_acceleration_enabled(enabled: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        AppConfig::mutate(|config| {
            config.general.gpu_acceleration_enabled = Some(enabled);
            Ok(())
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
