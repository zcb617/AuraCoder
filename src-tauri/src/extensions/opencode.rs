use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    engines::EngineManager,
    models::{
        ExtensionActionResultDto, ExtensionCatalogKindRefreshDto, ExtensionItemDto,
        OpenCodeRuntimeCatalogDto,
    },
};

use super::cli;

const PROVIDER_ID: &str = "opencode";

pub async fn refresh_kind(
    _manager: &EngineManager,
    cwd: Option<&str>,
    kind: &str,
) -> ExtensionCatalogKindRefreshDto {
    match kind {
        "skill" => {
            let skill_args = vec!["debug".to_string(), "skill".to_string()];
            match cli::run_json("opencode", &skill_args, cwd).await {
                Ok(value) => {
                    ExtensionCatalogKindRefreshDto::success(kind, parse_skills(&value, cwd))
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "plugin" => {
            let config_args = vec!["debug".to_string(), "config".to_string()];
            match cli::run_json("opencode", &config_args, cwd).await {
                Ok(value) => ExtensionCatalogKindRefreshDto::success(
                    kind,
                    parse_config(&value)
                        .into_iter()
                        .filter(|item| item.kind == "plugin")
                        .collect(),
                ),
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "mcp" => {
            refresh_kind_with_runtime(cwd, kind, Ok(OpenCodeRuntimeCatalogDto::default())).await
        }
        _ => ExtensionCatalogKindRefreshDto::failure(kind),
    }
}

/// 使用调用方已经从 OpenCode 协议客户端取得的运行时目录刷新 MCP 扩展。
///
/// 扩展模块只负责配置解析和目录合并，不获取 CLI 生命周期或 EngineManager 运行时句柄。
pub async fn refresh_kind_with_runtime(
    cwd: Option<&str>,
    kind: &str,
    runtime_result: Result<OpenCodeRuntimeCatalogDto>,
) -> ExtensionCatalogKindRefreshDto {
    if kind != "mcp" {
        return ExtensionCatalogKindRefreshDto::failure(kind);
    }
    let config_args = vec!["debug".to_string(), "config".to_string()];
    let config_result = cli::run_json("opencode", &config_args, cwd).await;
    match (config_result, runtime_result) {
        (Ok(config), Ok(runtime)) => {
            let mut items = parse_config(&config)
                .into_iter()
                .filter(|item| item.kind == "mcp")
                .map(|item| (item_key(&item), item))
                .collect::<BTreeMap<_, _>>();
            merge_runtime_mcp(&mut items, runtime);
            ExtensionCatalogKindRefreshDto::success(kind, items.into_values().collect())
        }
        _ => ExtensionCatalogKindRefreshDto::failure(kind),
    }
}

pub async fn perform_action(
    item: &ExtensionItemDto,
    action: &str,
    cwd: Option<&str>,
) -> Result<ExtensionActionResultDto> {
    let args = match (item.kind.as_str(), action) {
        ("mcp", "authenticate") => {
            vec!["mcp".to_string(), "auth".to_string(), item.id.clone()]
        }
        ("mcp", "logout") => vec!["mcp".to_string(), "logout".to_string(), item.id.clone()],
        _ => anyhow::bail!("unsupported OpenCode extension action"),
    };
    cli::run_action("opencode", &args, cwd).await?;
    Ok(ExtensionActionResultDto {
        provider_id: PROVIDER_ID.to_string(),
        kind: item.kind.clone(),
        extension_id: item.id.clone(),
        action: action.to_string(),
        requires_new_session: item.requires_new_session,
    })
}

fn parse_skills(value: &Value, cwd: Option<&str>) -> Vec<ExtensionItemDto> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            let name = string_field(skill, "name")?;
            let location = string_field(skill, "location")?;
            let scope = cwd
                .filter(|cwd| location.to_lowercase().starts_with(&cwd.to_lowercase()))
                .map(|_| "project")
                .unwrap_or("user");
            Some(ExtensionItemDto {
                id: location.clone(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "skill".to_string(),
                name,
                description: string_field(skill, "description"),
                version: None,
                scope: scope.to_string(),
                source: None,
                marketplace: None,
                path: Some(location),
                parent_plugin_id: None,
                category: None,
                officially_available: false,
                catalog_authority: None,
                installed: Some(true),
                configured: None,
                enabled: Some(true),
                health: "healthy".to_string(),
                auth_state: None,
                available_actions: Vec::new(),
                requires_new_session: false,
                read_only_reason: Some("opencode_skill".to_string()),
                warning: None,

                ..Default::default()
            })
        })
        .collect()
}

fn parse_config(value: &Value) -> Vec<ExtensionItemDto> {
    let mut items = Vec::new();
    for spec in value
        .get("plugin")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        items.push(ExtensionItemDto {
            id: plugin_display_name(spec),
            provider_id: PROVIDER_ID.to_string(),
            kind: "plugin".to_string(),
            name: plugin_display_name(spec),
            description: None,
            version: None,
            scope: "project".to_string(),
            source: Some(plugin_source_kind(spec).to_string()),
            marketplace: None,
            path: local_plugin_path(spec),
            parent_plugin_id: None,
            category: None,
            officially_available: false,
            catalog_authority: None,
            installed: Some(true),
            configured: Some(true),
            enabled: Some(true),
            health: "healthy".to_string(),
            auth_state: None,
            available_actions: Vec::new(),
            requires_new_session: true,
            read_only_reason: Some("opencode_plugin_jsonc".to_string()),
            warning: None,

            ..Default::default()
        });
    }

    if let Some(servers) = value.get("mcp").and_then(Value::as_object) {
        for (name, config) in servers {
            if name == "auracoder-computer-control" {
                continue;
            }
            let enabled = config
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            items.push(mcp_item(name, enabled, "unknown"));
        }
    }
    items
}

fn merge_runtime_mcp(
    items: &mut BTreeMap<String, ExtensionItemDto>,
    runtime: OpenCodeRuntimeCatalogDto,
) {
    for server in runtime.mcp_servers {
        if server.name == "auracoder-computer-control" {
            continue;
        }
        let key = format!("mcp:{}", server.name);
        let health = normalize_runtime_health(&server.status);
        if let Some(item) = items.get_mut(&key) {
            item.health = health.to_string();
            item.auth_state = auth_state(health);
        } else {
            items.insert(key, mcp_item(&server.name, true, health));
        }
    }
}

fn mcp_item(name: &str, enabled: bool, health: &str) -> ExtensionItemDto {
    ExtensionItemDto {
        id: name.to_string(),
        provider_id: PROVIDER_ID.to_string(),
        kind: "mcp".to_string(),
        name: name.to_string(),
        description: None,
        version: None,
        scope: "project".to_string(),
        source: None,
        marketplace: None,
        path: None,
        parent_plugin_id: None,
        category: None,
        officially_available: false,
        catalog_authority: None,
        installed: None,
        configured: Some(true),
        enabled: Some(enabled),
        health: health.to_string(),
        auth_state: auth_state(health),
        available_actions: vec!["authenticate".to_string(), "logout".to_string()],
        requires_new_session: false,
        read_only_reason: None,
        warning: None,
        ..Default::default()
    }
}

fn normalize_runtime_health(status: &str) -> &'static str {
    let status = status.to_ascii_lowercase();
    if status.contains("connected") {
        "healthy"
    } else if status.contains("auth") {
        "auth_required"
    } else if status.contains("failed") || status.contains("error") {
        "error"
    } else {
        "disconnected"
    }
}

fn auth_state(health: &str) -> Option<String> {
    match health {
        "healthy" => Some("authenticated".to_string()),
        "auth_required" => Some("required".to_string()),
        _ => Some("unknown".to_string()),
    }
}

fn plugin_name(spec: &str) -> String {
    let spec = spec.trim_start_matches("file://");
    if spec.starts_with('@') {
        let without_scope_marker = &spec[1..];
        let (scope, rest) = without_scope_marker
            .split_once('/')
            .unwrap_or((without_scope_marker, ""));
        let package = rest.split('@').next().unwrap_or(rest);
        return format!("@{scope}/{package}");
    }
    spec.rsplit(['/', '\\'])
        .next()
        .unwrap_or(spec)
        .split('@')
        .next()
        .unwrap_or(spec)
        .to_string()
}

fn plugin_display_name(spec: &str) -> String {
    let name = plugin_name(spec);
    let name = name.split(['?', '#']).next().unwrap_or_default();
    let safe = name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '@' | '/' | '_' | '-' | '.')
        })
        .collect::<String>();
    (!safe.is_empty())
        .then_some(safe)
        .unwrap_or_else(|| "plugin".to_string())
}

fn plugin_source_kind(spec: &str) -> &'static str {
    if local_plugin_path(spec).is_some() {
        "local"
    } else if spec.contains("://") {
        "remote"
    } else {
        "package"
    }
}

fn local_plugin_path(spec: &str) -> Option<String> {
    let value = spec.strip_prefix("file://").unwrap_or(spec);
    (spec.starts_with("file://") || value.contains('\\') || value.starts_with('.'))
        .then(|| value.to_string())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn item_key(item: &ExtensionItemDto) -> String {
    format!("{}:{}", item.kind, item.id)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_config, parse_skills};

    #[test]
    fn local_config_is_not_promoted_to_official_catalog() {
        let items = parse_config(&json!({
            "plugin": ["example@1.0.0"],
            "mcp": {
                "local": {},
                "auracoder-computer-control": {"type": "local"}
            }
        }));
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|item| item.id != "auracoder-computer-control"));
        assert!(items.iter().all(|item| !item.officially_available));
    }

    #[test]
    fn plugin_specs_are_sanitized_before_they_enter_the_catalog() {
        let items = parse_config(&json!({
            "plugin": ["https://example.test/plugin?token=secret"]
        }));
        let plugin = items.first().unwrap();
        assert_eq!(plugin.source.as_deref(), Some("remote"));
        assert!(!plugin.id.contains("secret"));
        assert!(!plugin.name.contains("secret"));
    }

    #[test]
    fn skill_content_is_dropped() {
        let items = parse_skills(
            &json!([{
                "name": "example",
                "description": "summary",
                "location": "C:/skills/example",
                "content": "secret body"
            }]),
            None,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].description.as_deref(), Some("summary"));
    }
}
