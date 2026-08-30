use tauri::{AppHandle, State};

use crate::{
    cli_tools::{factory::CliToolFactory, CliLocationKind},
    extensions,
    models::{CachedExtensionCatalogDto, ExtensionActionResultDto, ExtensionItemDto},
    state::AppState,
};

#[tauri::command]
pub async fn get_extension_catalog(
    state: State<'_, AppState>,
    provider_id: String,
    workspace_id: Option<String>,
    cwd: Option<String>,
) -> Result<CachedExtensionCatalogDto, String> {
    let cwd = normalize_cwd(cwd);
    if is_global_context(workspace_id.as_deref(), cwd.as_deref()) {
        return extensions::refresh::load_cached_catalog(state.inner(), provider_id.trim(), None)
            .await
            .map_err(err_to_string);
    }
    let (cli, context) = extensions::resolve_extension_cli(
        state.inner(),
        provider_id.trim(),
        workspace_id.as_deref(),
        cwd.as_deref(),
    )
    .await
    .map_err(err_to_string)?;
    cli.get_extension_catalog(&context, cwd.as_deref())
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn schedule_extension_catalog_workspace_refresh(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        return Err("workspace id is required".to_string());
    }
    extensions::refresh::schedule_workspace_catalog_refresh(&state, workspace_id)
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn request_extension_catalog_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
    workspace_id: Option<String>,
    cwd: Option<String>,
    kinds: Option<Vec<String>>,
) -> Result<CachedExtensionCatalogDto, String> {
    let cwd = normalize_cwd(cwd);
    let requested_kinds = kinds.unwrap_or_default();
    if is_global_context(workspace_id.as_deref(), cwd.as_deref()) {
        extensions::refresh::request_catalog_refresh(
            app,
            state.inner().clone(),
            provider_id.trim(),
            None,
            requested_kinds,
        )
        .await
        .map_err(err_to_string)?;
        return extensions::refresh::load_cached_catalog(state.inner(), provider_id.trim(), None)
            .await
            .map_err(err_to_string);
    }
    let (cli, context) = extensions::resolve_extension_cli(
        state.inner(),
        provider_id.trim(),
        workspace_id.as_deref(),
        cwd.as_deref(),
    )
    .await
    .map_err(err_to_string)?;
    if context.location_kind == CliLocationKind::Ssh {
        cli.refresh_extension_catalog(&context, cwd.as_deref(), &requested_kinds)
            .await
            .map_err(err_to_string)?;
        return cli
            .get_extension_catalog(&context, cwd.as_deref())
            .await
            .map_err(err_to_string);
    }
    extensions::refresh::request_catalog_refresh(
        app,
        state.inner().clone(),
        provider_id.trim(),
        cwd.clone(),
        requested_kinds,
    )
    .await
    .map_err(err_to_string)?;
    cli.get_extension_catalog(&context, cwd.as_deref())
        .await
        .map_err(err_to_string)
}

#[tauri::command]
pub async fn get_extension_details(
    state: State<'_, AppState>,
    provider_id: String,
    workspace_id: Option<String>,
    kind: String,
    extension_id: String,
    cwd: Option<String>,
) -> Result<ExtensionItemDto, String> {
    let cwd = normalize_cwd(cwd);
    if is_global_context(workspace_id.as_deref(), cwd.as_deref()) {
        let catalog =
            extensions::refresh::load_cached_catalog(state.inner(), provider_id.trim(), None)
                .await
                .map_err(err_to_string)?;
        return catalog
            .items
            .into_iter()
            .find(|item| item.kind == kind && item.id == extension_id)
            .ok_or_else(|| "extension not found in the current catalog".to_string());
    }
    let (cli, context) = extensions::resolve_extension_cli(
        state.inner(),
        provider_id.trim(),
        workspace_id.as_deref(),
        cwd.as_deref(),
    )
    .await
    .map_err(err_to_string)?;
    let catalog = cli
        .get_extension_catalog(&context, cwd.as_deref())
        .await
        .map_err(err_to_string)?;

    catalog
        .items
        .into_iter()
        .find(|item| item.kind == kind && item.id == extension_id)
        .ok_or_else(|| "extension not found in the current catalog".to_string())
}

#[tauri::command]
pub async fn perform_extension_action(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
    workspace_id: Option<String>,
    kind: String,
    extension_id: String,
    action: String,
    scope: Option<String>,
    cwd: Option<String>,
) -> Result<ExtensionActionResultDto, String> {
    let cwd = normalize_cwd(cwd);
    if is_global_context(workspace_id.as_deref(), cwd.as_deref()) {
        let item =
            extensions::refresh::load_cached_catalog(state.inner(), provider_id.trim(), None)
                .await
                .map_err(err_to_string)?
                .items
                .into_iter()
                .find(|item| item.kind == kind.trim() && item.id == extension_id.trim())
                .ok_or_else(|| "extension not found in the cached catalog".to_string())?;
        let cli = CliToolFactory::new(state.inner().clone())
            .create(provider_id.trim())
            .map_err(err_to_string)?;
        let result = cli
            .perform_global_extension_action(item, action.trim(), scope.as_deref())
            .await
            .map_err(err_to_string)?;
        if extensions::refresh::request_catalog_refresh(
            app,
            state.inner().clone(),
            result.provider_id.as_str(),
            None,
            extensions::refresh::affected_refresh_kinds(&result.kind),
        )
        .await
        .is_err()
        {
            log::warn!(
                "failed to queue global extension catalog refresh after an extension action"
            );
        }
        return Ok(result);
    }
    let (cli, context) = extensions::resolve_extension_cli(
        state.inner(),
        provider_id.trim(),
        workspace_id.as_deref(),
        cwd.as_deref(),
    )
    .await
    .map_err(err_to_string)?;
    let item = cli
        .get_extension_catalog(&context, cwd.as_deref())
        .await
        .map_err(err_to_string)?
        .items
        .into_iter()
        .find(|item| item.kind == kind.trim() && item.id == extension_id.trim())
        .ok_or_else(|| "extension not found in the cached catalog".to_string())?;
    let result = cli
        .perform_extension_action(&context, item, action.trim(), scope.as_deref())
        .await
        .map_err(err_to_string)?;
    if context.location_kind == CliLocationKind::Local
        && extensions::refresh::request_catalog_refresh(
            app,
            state.inner().clone(),
            result.provider_id.as_str(),
            cwd,
            extensions::refresh::affected_refresh_kinds(&result.kind),
        )
        .await
        .is_err()
    {
        log::warn!("failed to queue extension catalog refresh after an extension action");
    }
    Ok(result)
}

/// 用户在聊天输入框中按下斜杠时，按当前 CLI 和 workspace 读取统一扩展菜单项。
/// 业务调用只负责构造上下文、从工厂取得一个 CLI 实现并调用一次 get_extensions，
/// 不按 codex/opencode/claude 分支取数。
#[tauri::command]
pub async fn get_cli_extensions(
    state: State<'_, AppState>,
    cli_id: String,
    workspace_id: Option<String>,
) -> Result<Vec<ExtensionItemDto>, String> {
    let cli = CliToolFactory::new(state.inner().clone())
        .create(cli_id.trim())
        .map_err(err_to_string)?;
    let context = cli
        .execution_context(workspace_id.as_deref())
        .await
        .map_err(err_to_string)?;
    cli.get_extensions(&context).await.map_err(err_to_string)
}

fn normalize_cwd(cwd: Option<String>) -> Option<String> {
    cwd.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 判断扩展请求是否应读取本机用户级全局目录，而不是任何 workspace 目录。
fn is_global_context(workspace_id: Option<&str>, cwd: Option<&str>) -> bool {
    workspace_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && cwd.is_none()
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

#[cfg(test)]
mod tests {
    use super::is_global_context;

    #[test]
    fn identifies_only_missing_workspace_and_cwd_as_global_context() {
        assert!(is_global_context(None, None));
        assert!(is_global_context(Some("  \t"), None));
        assert!(!is_global_context(Some("workspace"), None));
        assert!(!is_global_context(None, Some("C:/work/project")));
    }
}
