use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::Value;

use crate::{
    engines::EngineManager,
    models::{ExtensionActionResultDto, ExtensionCatalogKindRefreshDto, ExtensionItemDto},
};

use super::cli;

const PROVIDER_ID: &str = "claude";
const OFFICIAL_MARKETPLACE: &str = "claude-plugins-official";

pub async fn refresh_kind(
    _manager: &EngineManager,
    cwd: Option<&str>,
    kind: &str,
) -> ExtensionCatalogKindRefreshDto {
    match kind {
        "plugin" => {
            let plugin_args = vec![
                "plugin".to_string(),
                "list".to_string(),
                "--available".to_string(),
                "--json".to_string(),
            ];
            let marketplace_args = vec![
                "plugin".to_string(),
                "marketplace".to_string(),
                "list".to_string(),
                "--json".to_string(),
            ];
            let (plugins_result, marketplace_result) = tokio::join!(
                cli::run_json("claude", &plugin_args, cwd),
                cli::run_json("claude", &marketplace_args, cwd),
            );
            match plugins_result {
                Ok(value) => {
                    let categories = marketplace_result
                        .ok()
                        .map(|value| load_official_plugin_categories(&value).0)
                        .unwrap_or_default();
                    ExtensionCatalogKindRefreshDto::success(
                        kind,
                        parse_plugins(&value, &categories),
                    )
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "skill" => {
            let plugin_args = vec![
                "plugin".to_string(),
                "list".to_string(),
                "--available".to_string(),
                "--json".to_string(),
            ];
            match cli::run_json("claude", &plugin_args, cwd).await {
                Ok(value) => {
                    let plugin_skill_roots = parse_plugins(&value, &HashMap::new())
                        .into_iter()
                        .filter(|item| item.installed == Some(true))
                        .filter_map(|item| {
                            item.path.map(|path| {
                                (
                                    Path::new(&path).join("skills"),
                                    item.id,
                                    item.enabled.unwrap_or(true),
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    ExtensionCatalogKindRefreshDto::success(
                        kind,
                        scan_skills(cwd, &plugin_skill_roots),
                    )
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "mcp" => {
            let mcp_args = vec!["mcp".to_string(), "list".to_string()];
            match cli::run_text("claude", &mcp_args, cwd).await {
                Ok(output) => {
                    ExtensionCatalogKindRefreshDto::success(kind, parse_mcp_servers(&output))
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        _ => ExtensionCatalogKindRefreshDto::failure(kind),
    }
}

pub async fn perform_action(
    item: &ExtensionItemDto,
    action: &str,
    requested_scope: Option<&str>,
    cwd: Option<&str>,
) -> Result<ExtensionActionResultDto> {
    let scope = match requested_scope.unwrap_or(item.scope.as_str()) {
        value @ ("user" | "project" | "local") => value,
        _ => "user",
    };
    let args = match (item.kind.as_str(), action) {
        ("plugin", "install") => vec![
            "plugin".to_string(),
            "install".to_string(),
            item.id.clone(),
            "--scope".to_string(),
            scope.to_string(),
        ],
        ("plugin", "uninstall") => vec![
            "plugin".to_string(),
            "uninstall".to_string(),
            item.id.clone(),
            "--scope".to_string(),
            scope.to_string(),
            "-y".to_string(),
        ],
        ("plugin", "enable") | ("plugin", "disable") => vec![
            "plugin".to_string(),
            action.to_string(),
            item.id.clone(),
            "--scope".to_string(),
            scope.to_string(),
        ],
        ("mcp", "remove") => vec!["mcp".to_string(), "remove".to_string(), item.id.clone()],
        ("mcp", "authenticate") => {
            vec!["mcp".to_string(), "login".to_string(), item.id.clone()]
        }
        ("mcp", "logout") => vec!["mcp".to_string(), "logout".to_string(), item.id.clone()],
        _ => anyhow::bail!("unsupported Claude extension action"),
    };

    cli::run_action("claude", &args, cwd).await?;
    Ok(ExtensionActionResultDto {
        provider_id: PROVIDER_ID.to_string(),
        kind: item.kind.clone(),
        extension_id: item.id.clone(),
        action: action.to_string(),
        requires_new_session: item.requires_new_session,
    })
}

pub(crate) fn parse_plugins(
    value: &Value,
    categories: &HashMap<String, String>,
) -> Vec<ExtensionItemDto> {
    let mut items = BTreeMap::<String, ExtensionItemDto>::new();
    for plugin in value
        .get("installed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = string_field_any(plugin, &["id", "pluginId"]) else {
            continue;
        };
        let enabled = bool_field(plugin, "enabled").unwrap_or(true);
        let scope = string_field(plugin, "scope").unwrap_or_else(|| "user".to_string());
        let normalized_scope = normalize_scope(&scope);
        let managed = normalized_scope == "managed";
        let marketplace = string_field(plugin, "marketplaceName");
        let item = ExtensionItemDto {
            id: id.clone(),
            provider_id: PROVIDER_ID.to_string(),
            kind: "plugin".to_string(),
            name: string_field(plugin, "name").unwrap_or_else(|| plugin_name(&id)),
            description: string_field(plugin, "description"),
            version: string_field(plugin, "version"),
            scope: normalized_scope,
            source: marketplace.clone(),
            marketplace: marketplace.clone(),
            path: string_field(plugin, "installPath"),
            parent_plugin_id: None,
            category: official_plugin_category(&id, marketplace.as_deref(), categories),
            officially_available: false,
            catalog_authority: None,
            installed: Some(true),
            configured: None,
            enabled: Some(enabled),
            health: if enabled { "healthy" } else { "unknown" }.to_string(),
            auth_state: None,
            available_actions: if managed {
                Vec::new()
            } else {
                vec![
                    "uninstall".to_string(),
                    if enabled { "disable" } else { "enable" }.to_string(),
                ]
            },
            requires_new_session: true,
            read_only_reason: managed.then(|| "managed_policy".to_string()),
            warning: None,

            ..Default::default()
        };
        items.insert(id, item);
    }

    for plugin in value
        .get("available")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if string_field(plugin, "marketplaceName").as_deref() != Some(OFFICIAL_MARKETPLACE) {
            continue;
        }
        let Some(id) = string_field_any(plugin, &["pluginId", "id"]) else {
            continue;
        };
        if let Some(installed) = items.get_mut(&id) {
            installed.officially_available = true;
            installed.catalog_authority = Some("provider_official".to_string());
            installed.marketplace = Some(OFFICIAL_MARKETPLACE.to_string());
            installed.category = installed
                .category
                .take()
                .or_else(|| categories.get(&plugin_name(&id)).cloned());
            installed.description = installed
                .description
                .take()
                .or_else(|| string_field(plugin, "description"));
            continue;
        }
        items.insert(
            id.clone(),
            ExtensionItemDto {
                id: id.clone(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "plugin".to_string(),
                name: string_field(plugin, "name").unwrap_or_else(|| plugin_name(&id)),
                description: string_field(plugin, "description"),
                version: string_field(plugin, "version"),
                scope: "user".to_string(),
                source: Some(OFFICIAL_MARKETPLACE.to_string()),
                marketplace: Some(OFFICIAL_MARKETPLACE.to_string()),
                path: None,
                parent_plugin_id: None,
                category: categories.get(&plugin_name(&id)).cloned(),
                officially_available: true,
                catalog_authority: Some("provider_official".to_string()),
                installed: Some(false),
                configured: None,
                enabled: Some(false),
                health: "unknown".to_string(),
                auth_state: None,
                available_actions: vec!["install".to_string()],
                requires_new_session: true,
                read_only_reason: None,
                warning: None,

                ..Default::default()
            },
        );
    }
    items.into_values().collect()
}

fn scan_skills(
    cwd: Option<&str>,
    plugin_roots: &[(PathBuf, String, bool)],
) -> Vec<ExtensionItemDto> {
    let mut roots = Vec::<(PathBuf, String, Option<String>, bool)>::new();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        roots.push((
            PathBuf::from(home).join(".claude/skills"),
            "user".to_string(),
            None,
            true,
        ));
    }
    if let Some(cwd) = cwd {
        roots.push((
            Path::new(cwd).join(".claude/skills"),
            "project".to_string(),
            None,
            true,
        ));
    }
    roots.extend(plugin_roots.iter().map(|(path, plugin_id, enabled)| {
        (
            path.clone(),
            "plugin".to_string(),
            Some(plugin_id.clone()),
            *enabled,
        )
    }));

    let mut items = BTreeMap::<String, ExtensionItemDto>::new();
    for (root, scope, parent_plugin_id, enabled) in roots {
        let mut manifests = Vec::new();
        collect_skill_manifests(&root, 0, &mut manifests);
        for manifest in manifests {
            let (name, description) = parse_skill_manifest(&manifest);
            let id = manifest.to_string_lossy().to_string();
            let item = ExtensionItemDto {
                id: id.clone(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "skill".to_string(),
                name: name.unwrap_or_else(|| {
                    manifest
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|value| value.to_str())
                        .unwrap_or("Skill")
                        .to_string()
                }),
                description,
                version: None,
                scope: scope.clone(),
                source: parent_plugin_id.clone(),
                marketplace: None,
                path: Some(id.clone()),
                parent_plugin_id: parent_plugin_id.clone(),
                category: None,
                officially_available: false,
                catalog_authority: None,
                installed: Some(true),
                configured: None,
                enabled: Some(enabled),
                health: if enabled { "healthy" } else { "unknown" }.to_string(),
                auth_state: None,
                available_actions: Vec::new(),
                requires_new_session: false,
                read_only_reason: Some("claude_skill_toggle".to_string()),
                warning: None,

                ..Default::default()
            };
            items.insert(id, item);
        }
    }
    items.into_values().collect()
}

fn collect_skill_manifests(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 4 || !root.is_dir() {
        return;
    }
    let manifest = root.join("SKILL.md");
    if manifest.is_file() {
        output.push(manifest);
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_skill_manifests(&path, depth + 1, output);
        }
    }
}

fn parse_skill_manifest(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(content) = fs::read_to_string(path) else {
        return (None, None);
    };
    parse_skill_frontmatter(&content)
}

fn parse_skill_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("name:") {
            name = clean_yaml_scalar(value);
        } else if let Some(value) = line.strip_prefix("description:") {
            description = clean_yaml_scalar(value);
        }
    }
    (name, description)
}

pub(crate) fn parse_mcp_servers(output: &str) -> Vec<ExtensionItemDto> {
    output
        .lines()
        .filter_map(|line| {
            let (definition, status) = line.trim().rsplit_once(" - ")?;
            let (name, _) = definition.rsplit_once(": ")?;
            if name.trim().is_empty() {
                return None;
            }
            let status_lower = status.to_ascii_lowercase();
            let health = if status_lower.contains("connected") && !status_lower.contains("failed") {
                "healthy"
            } else if status_lower.contains("auth") {
                "auth_required"
            } else if status_lower.contains("failed") || status_lower.contains("error") {
                "error"
            } else {
                "disconnected"
            };
            let name = name.trim();
            let managed_by_plugin = name.starts_with("plugin:");
            Some(ExtensionItemDto {
                id: name.to_string(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "mcp".to_string(),
                name: name.to_string(),
                description: None,
                version: None,
                scope: if managed_by_plugin { "plugin" } else { "user" }.to_string(),
                source: None,
                marketplace: None,
                path: None,
                parent_plugin_id: managed_by_plugin.then(|| {
                    name.trim_start_matches("plugin:")
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_string()
                }),
                category: None,
                officially_available: false,
                catalog_authority: None,
                installed: None,
                configured: Some(true),
                enabled: Some(true),
                health: health.to_string(),
                auth_state: match health {
                    "healthy" => Some("authenticated".to_string()),
                    "auth_required" => Some("required".to_string()),
                    _ => Some("unknown".to_string()),
                },
                available_actions: if managed_by_plugin {
                    Vec::new()
                } else {
                    vec![
                        "remove".to_string(),
                        "authenticate".to_string(),
                        "logout".to_string(),
                    ]
                },
                requires_new_session: false,
                read_only_reason: managed_by_plugin.then(|| "plugin_managed_mcp".to_string()),
                warning: None,

                ..Default::default()
            })
        })
        .collect()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn string_field_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_field(value, key))
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn plugin_name(id: &str) -> String {
    id.split('@').next().unwrap_or(id).to_string()
}

fn official_plugin_category(
    id: &str,
    marketplace: Option<&str>,
    categories: &HashMap<String, String>,
) -> Option<String> {
    let official_id = id.ends_with(&format!("@{OFFICIAL_MARKETPLACE}"));
    if marketplace != Some(OFFICIAL_MARKETPLACE) && !official_id {
        return None;
    }
    categories.get(&plugin_name(id)).cloned()
}

fn load_official_plugin_categories(value: &Value) -> (HashMap<String, String>, Vec<String>) {
    let Some(marketplace) = value.as_array().into_iter().flatten().find(|marketplace| {
        string_field(marketplace, "name").as_deref() == Some(OFFICIAL_MARKETPLACE)
    }) else {
        return (HashMap::new(), Vec::new());
    };
    let Some(install_location) = string_field(marketplace, "installLocation") else {
        return (HashMap::new(), Vec::new());
    };
    let manifest_path = Path::new(&install_location).join(".claude-plugin/marketplace.json");
    match super::read_catalog_manifest(&manifest_path) {
        Ok(manifest) => (parse_marketplace_categories(&manifest), Vec::new()),
        Err(error) => (
            HashMap::new(),
            vec![format!(
                "Failed to read categories for Claude marketplace {OFFICIAL_MARKETPLACE}: {error:#}"
            )],
        ),
    }
}

fn parse_marketplace_categories(value: &Value) -> HashMap<String, String> {
    value
        .get("plugins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|plugin| {
            let name = string_field(plugin, "name")?;
            let category = string_field(plugin, "category")?;
            (!category.trim().is_empty()).then(|| (name, category))
        })
        .collect()
}

fn normalize_scope(scope: &str) -> String {
    match scope {
        "project" | "local" | "plugin" | "managed" => scope.to_string(),
        _ => "user".to_string(),
    }
}

fn clean_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['\'', '"']).trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{parse_marketplace_categories, parse_plugins, parse_skill_frontmatter};

    #[test]
    fn filters_non_official_available_but_keeps_installed() {
        let value = json!({
            "installed": [{"id": "local@personal", "enabled": true}],
            "available": [
                {"pluginId": "official@claude-plugins-official", "marketplaceName": "claude-plugins-official"},
                {"pluginId": "community@other", "marketplaceName": "other"}
            ]
        });
        let items = parse_plugins(&value, &HashMap::new());
        assert!(items.iter().any(|item| item.id == "local@personal"));
        assert!(items
            .iter()
            .any(|item| item.id == "official@claude-plugins-official"));
        assert!(!items.iter().any(|item| item.id == "community@other"));
    }

    #[test]
    fn reads_categories_from_the_official_marketplace_manifest() {
        let manifest = json!({
            "plugins": [
                {"name": "linear", "category": "productivity"},
                {"name": "uncategorized"}
            ]
        });
        let categories = parse_marketplace_categories(&manifest);
        assert_eq!(
            categories.get("linear").map(String::as_str),
            Some("productivity")
        );
        assert!(!categories.contains_key("uncategorized"));
    }

    #[test]
    fn reads_only_skill_frontmatter_summary() {
        let (name, description) = parse_skill_frontmatter(
            "---\nname: example\ndescription: 'Example skill'\n---\nSensitive body",
        );
        assert_eq!(name.as_deref(), Some("example"));
        assert_eq!(description.as_deref(), Some("Example skill"));
    }
}
