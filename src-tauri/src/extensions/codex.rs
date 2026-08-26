use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::Result;
use serde_json::Value;

use crate::{
    engines::EngineManager,
    models::{
        CodexProtocolDiagnosticsDto, ExtensionActionResultDto, ExtensionCatalogKindRefreshDto,
        ExtensionItemDto,
    },
};

use super::cli;

const PROVIDER_ID: &str = "codex";
const OFFICIAL_MARKETPLACES: &[&str] = &["openai-bundled", "openai-curated"];
const REMOTE_INSTALL_MARKER: &str = ".codex-remote-plugin-install.json";
const MAX_SKILL_FRONTMATTER_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone)]
struct RemoteInstalledPlugin {
    id: String,
    name: String,
    description: Option<String>,
    version: Option<String>,
    marketplace: String,
    package_root: PathBuf,
    skills_root: Option<PathBuf>,
}

pub async fn refresh_kind(
    manager: &EngineManager,
    cwd: Option<&str>,
    kind: &str,
) -> ExtensionCatalogKindRefreshDto {
    match kind {
        "skill" => {
            let remote_skills = remote_plugin_skills(&installed_remote_plugins());
            let result = match cwd {
                Some(cwd) => manager.list_codex_skills(cwd).await,
                None => Ok(Vec::new()),
            };
            match result {
                Ok(skills) => {
                    let mut items = skills
                        .into_iter()
                        .map(|skill| {
                            let id = skill.path.clone();
                            (
                                id,
                                ExtensionItemDto {
                                    id: skill.path.clone(),
                                    provider_id: PROVIDER_ID.to_string(),
                                    kind: "skill".to_string(),
                                    name: skill.name,
                                    description: non_empty(skill.description),
                                    version: None,
                                    scope: normalize_skill_scope(&skill.scope),
                                    source: non_empty(skill.scope),
                                    marketplace: None,
                                    path: Some(skill.path),
                                    parent_plugin_id: None,
                                    category: None,
                                    officially_available: false,
                                    catalog_authority: None,
                                    installed: Some(true),
                                    configured: None,
                                    enabled: Some(skill.enabled),
                                    health: if skill.enabled { "healthy" } else { "unknown" }
                                        .to_string(),
                                    auth_state: None,
                                    available_actions: Vec::new(),
                                    requires_new_session: false,
                                    read_only_reason: Some("codex_skill_toggle".to_string()),
                                    warning: None,

                                    ..Default::default()},
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    for skill in remote_skills {
                        items.entry(skill.id.clone()).or_insert(skill);
                    }
                    ExtensionCatalogKindRefreshDto::success(kind, items.into_values().collect())
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "plugin" => {
            let remote_plugins = installed_remote_plugins();
            let plugin_args = vec![
                "plugin".to_string(),
                "list".to_string(),
                "--available".to_string(),
                "--json".to_string(),
            ];
            match cli::run_json("codex", &plugin_args, cwd).await {
                Ok(value) => {
                    let categories = HashMap::new();
                    ExtensionCatalogKindRefreshDto::success(
                        kind,
                        // A catalog refresh must not wait for the much heavier
                        // Codex transport health probe. The list command is the
                        // source of truth here; diagnostics are optional UI
                        // enrichment and cannot be allowed to hide valid data.
                        parse_plugins(&value, None, &categories, &remote_plugins),
                    )
                }
                Err(_) => ExtensionCatalogKindRefreshDto::failure(kind),
            }
        }
        "mcp" => {
            let mcp_args = vec!["mcp".to_string(), "list".to_string(), "--json".to_string()];
            match cli::run_json("codex", &mcp_args, cwd).await {
                Ok(value) => {
                    ExtensionCatalogKindRefreshDto::success(kind, parse_mcp_servers(&value, None))
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
    cwd: Option<&str>,
) -> Result<ExtensionActionResultDto> {
    let args = match (item.kind.as_str(), action) {
        ("plugin", "install") => vec![
            "plugin".to_string(),
            "add".to_string(),
            item.id.clone(),
            "--json".to_string(),
        ],
        ("plugin", "uninstall") => vec![
            "plugin".to_string(),
            "remove".to_string(),
            item.id.clone(),
            "--json".to_string(),
        ],
        ("mcp", "remove") => vec!["mcp".to_string(), "remove".to_string(), item.id.clone()],
        ("mcp", "authenticate") => {
            vec!["mcp".to_string(), "login".to_string(), item.id.clone()]
        }
        ("mcp", "logout") => vec!["mcp".to_string(), "logout".to_string(), item.id.clone()],
        _ => anyhow::bail!("unsupported Codex extension action"),
    };

    cli::run_action("codex", &args, cwd).await?;
    Ok(ExtensionActionResultDto {
        provider_id: PROVIDER_ID.to_string(),
        kind: item.kind.clone(),
        extension_id: item.id.clone(),
        action: action.to_string(),
        requires_new_session: item.requires_new_session,
    })
}

fn parse_plugins(
    value: &Value,
    diagnostics: Option<&CodexProtocolDiagnosticsDto>,
    categories: &HashMap<String, String>,
    remote_plugins: &[RemoteInstalledPlugin],
) -> Vec<ExtensionItemDto> {
    let mut descriptions = HashMap::<String, String>::new();
    if let Some(diagnostics) = diagnostics {
        for marketplace in &diagnostics.plugin_marketplaces {
            for plugin in &marketplace.plugins {
                let id = plugin_selector(&plugin.id, &marketplace.name);
                if let Some(description) = plugin.description.clone().or_else(|| {
                    (!plugin.capabilities.is_empty()).then(|| plugin.capabilities.join(", "))
                }) {
                    descriptions.insert(id, description);
                }
            }
        }
    }

    let mut items = BTreeMap::<String, ExtensionItemDto>::new();
    for plugin in remote_plugins {
        items.insert(plugin.id.clone(), remote_plugin_item(plugin));
    }
    for plugin in value
        .get("installed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = string_field(plugin, "pluginId") else {
            continue;
        };
        let marketplace = string_field(plugin, "marketplaceName");
        let item = ExtensionItemDto {
            id: id.clone(),
            provider_id: PROVIDER_ID.to_string(),
            kind: "plugin".to_string(),
            name: string_field(plugin, "name").unwrap_or_else(|| plugin_name(&id)),
            description: descriptions.get(&id).cloned(),
            version: string_field(plugin, "version"),
            scope: "user".to_string(),
            source: marketplace.clone(),
            marketplace,
            path: nested_string_field(plugin, &["source", "path"]),
            parent_plugin_id: None,
            category: categories
                .get(&id)
                .cloned()
                .or_else(|| category_from_plugin_source(plugin)),
            officially_available: false,
            catalog_authority: None,
            installed: Some(true),
            configured: None,
            enabled: Some(bool_field(plugin, "enabled").unwrap_or(true)),
            health: "healthy".to_string(),
            auth_state: None,
            available_actions: vec!["uninstall".to_string()],
            requires_new_session: true,
            read_only_reason: None,
            warning: None,

            ..Default::default()};
        items.insert(id, item);
    }

    for plugin in value
        .get("available")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = string_field(plugin, "pluginId") else {
            continue;
        };
        let Some(marketplace) = string_field(plugin, "marketplaceName") else {
            continue;
        };
        if !is_official_marketplace(&marketplace) {
            continue;
        }

        if let Some(installed) = items.get_mut(&id) {
            installed.officially_available = true;
            installed.catalog_authority = Some("provider_official".to_string());
            installed.category = installed
                .category
                .take()
                .or_else(|| categories.get(&id).cloned())
                .or_else(|| category_from_plugin_source(plugin));
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
                description: string_field(plugin, "description")
                    .or_else(|| descriptions.get(&id).cloned()),
                version: string_field(plugin, "version"),
                scope: "user".to_string(),
                source: Some(marketplace.clone()),
                marketplace: Some(marketplace),
                path: None,
                parent_plugin_id: None,
                category: categories
                    .get(&id)
                    .cloned()
                    .or_else(|| category_from_plugin_source(plugin)),
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

                ..Default::default()},
        );
    }

    items.into_values().collect()
}

fn remote_plugin_item(plugin: &RemoteInstalledPlugin) -> ExtensionItemDto {
    let officially_available = is_official_marketplace(&plugin.marketplace);
    ExtensionItemDto {
        id: plugin.id.clone(),
        provider_id: PROVIDER_ID.to_string(),
        kind: "plugin".to_string(),
        name: plugin.name.clone(),
        description: plugin.description.clone(),
        version: plugin.version.clone(),
        scope: "user".to_string(),
        source: Some(plugin.marketplace.clone()),
        marketplace: Some(plugin.marketplace.clone()),
        path: Some(plugin.package_root.to_string_lossy().to_string()),
        parent_plugin_id: None,
        category: None,
        officially_available,
        catalog_authority: officially_available.then(|| "provider_official".to_string()),
        installed: Some(true),
        configured: None,
        enabled: Some(true),
        health: "healthy".to_string(),
        auth_state: None,
        // `codex plugin list` does not expose a lifecycle command for desktop
        // remote plugins, so do not offer a CLI action that cannot be verified.
        available_actions: Vec::new(),
        requires_new_session: false,
        read_only_reason: Some("codex_remote_plugin_managed".to_string()),
        warning: None,
        ..Default::default()
    }
}

fn installed_remote_plugins() -> Vec<RemoteInstalledPlugin> {
    codex_plugin_cache_root()
        .map(|cache_root| collect_remote_installed_plugins(&cache_root))
        .unwrap_or_default()
}

fn codex_plugin_cache_root() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home).join("plugins/cache"));
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".codex/plugins/cache"))
}

fn collect_remote_installed_plugins(cache_root: &Path) -> Vec<RemoteInstalledPlugin> {
    let Ok(marketplace_entries) = fs::read_dir(cache_root) else {
        return Vec::new();
    };

    let mut plugins = BTreeMap::<String, RemoteInstalledPlugin>::new();
    for marketplace_entry in marketplace_entries.flatten() {
        let Ok(file_type) = marketplace_entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let marketplace = marketplace_entry
            .file_name()
            .to_string_lossy()
            .trim_end_matches("-remote")
            .to_string();
        let Ok(package_entries) = fs::read_dir(marketplace_entry.path()) else {
            continue;
        };
        for package_entry in package_entries.flatten() {
            let Ok(file_type) = package_entry.file_type() else {
                continue;
            };
            if !file_type.is_dir()
                || !is_regular_file(&package_entry.path().join(REMOTE_INSTALL_MARKER))
            {
                continue;
            }
            if let Some(plugin) = remote_plugin_from_cache_dir(&package_entry.path(), &marketplace)
            {
                plugins.insert(plugin.id.clone(), plugin);
            }
        }
    }
    plugins.into_values().collect()
}

fn remote_plugin_from_cache_dir(
    cache_package_dir: &Path,
    marketplace: &str,
) -> Option<RemoteInstalledPlugin> {
    let mut package_roots = fs::read_dir(cache_package_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_dir())
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    package_roots.sort_by(|left, right| {
        right
            .file_name()
            .unwrap_or_default()
            .cmp(left.file_name().unwrap_or_default())
    });

    for package_root in package_roots {
        let manifest_path = package_root.join(".codex-plugin/plugin.json");
        let Ok(manifest) = super::read_catalog_manifest(&manifest_path) else {
            continue;
        };
        let Some(plugin_name) = string_field(&manifest, "name") else {
            continue;
        };
        let skills_root = string_field(&manifest, "skills")
            .and_then(|path| safe_manifest_child(&package_root, &path));
        return Some(RemoteInstalledPlugin {
            id: plugin_selector(&plugin_name, marketplace),
            name: nested_string_field(&manifest, &["interface", "displayName"])
                .unwrap_or(plugin_name),
            description: nested_string_field(&manifest, &["interface", "shortDescription"])
                .or_else(|| string_field(&manifest, "description")),
            version: string_field(&manifest, "version"),
            marketplace: marketplace.to_string(),
            package_root,
            skills_root,
        });
    }
    None
}

fn safe_manifest_child(package_root: &Path, value: &str) -> Option<PathBuf> {
    let child = Path::new(value);
    if child.is_absolute()
        || child.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let child = package_root.join(child);
    child.is_dir().then_some(child)
}

fn remote_plugin_skills(plugins: &[RemoteInstalledPlugin]) -> Vec<ExtensionItemDto> {
    let mut items = BTreeMap::<String, ExtensionItemDto>::new();
    for plugin in plugins {
        let Some(skills_root) = plugin.skills_root.as_deref() else {
            continue;
        };
        let mut manifests = Vec::new();
        collect_skill_manifests(skills_root, 0, &mut manifests);
        for manifest in manifests {
            let (name, description) = parse_skill_manifest(&manifest);
            let id = manifest.to_string_lossy().to_string();
            items.entry(id.clone()).or_insert_with(|| ExtensionItemDto {
                id: id.clone(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "skill".to_string(),
                name: name.unwrap_or_else(|| skill_name_from_manifest(&manifest)),
                description,
                version: None,
                scope: "plugin".to_string(),
                source: Some(plugin.id.clone()),
                marketplace: Some(plugin.marketplace.clone()),
                path: Some(id.clone()),
                parent_plugin_id: Some(plugin.id.clone()),
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
                read_only_reason: Some("codex_skill_toggle".to_string()),
                warning: None,

                ..Default::default()});
        }
    }
    items.into_values().collect()
}

fn collect_skill_manifests(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 4 || !root.is_dir() {
        return;
    }
    let manifest = root.join("SKILL.md");
    if is_regular_file(&manifest) {
        output.push(manifest);
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
        {
            collect_skill_manifests(&entry.path(), depth + 1, output);
        }
    }
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn parse_skill_manifest(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(metadata) = fs::metadata(path) else {
        return (None, None);
    };
    if metadata.len() > MAX_SKILL_FRONTMATTER_BYTES {
        return (None, None);
    }
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

fn clean_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['\'', '"']).trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn skill_name_from_manifest(manifest: &Path) -> String {
    manifest
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Skill")
        .to_string()
}

fn parse_mcp_servers(
    value: &Value,
    diagnostics: Option<&CodexProtocolDiagnosticsDto>,
) -> Vec<ExtensionItemDto> {
    let diagnostic_by_name = diagnostics
        .map(|diagnostics| {
            diagnostics
                .mcp_servers
                .iter()
                .map(|server| (server.name.as_str(), server))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|server| {
            let name = string_field(server, "name")?;
            if name == "auracoder-computer-control" {
                return None;
            }
            let enabled = bool_field(server, "enabled").unwrap_or(true);
            let auth_status = string_field(server, "auth_status")
                .or_else(|| string_field(server, "authStatus"))
                .unwrap_or_else(|| "unknown".to_string());
            let auth_state = normalize_auth_state(&auth_status);
            let parent_plugin_id = managed_plugin_name(&name);
            let managed_by_plugin = parent_plugin_id.is_some();
            let mut available_actions = Vec::new();
            if !managed_by_plugin {
                available_actions.push("remove".to_string());
                match auth_state.as_deref() {
                    Some("required") | Some("failed") => {
                        available_actions.push("authenticate".to_string())
                    }
                    Some("authenticated") => available_actions.push("logout".to_string()),
                    _ => {}
                }
            }

            let diagnostic = diagnostic_by_name.get(name.as_str()).copied();
            Some(ExtensionItemDto {
                id: name.clone(),
                provider_id: PROVIDER_ID.to_string(),
                kind: "mcp".to_string(),
                name,
                description: diagnostic.map(|server| {
                    format!(
                        "{} tools · {} resources",
                        server.tool_count, server.resource_count
                    )
                }),
                version: None,
                scope: "user".to_string(),
                source: transport_label(server.get("transport")),
                marketplace: None,
                path: None,
                parent_plugin_id,
                category: None,
                officially_available: false,
                catalog_authority: None,
                installed: None,
                configured: Some(true),
                enabled: Some(enabled),
                health: if !enabled {
                    "disconnected"
                } else if auth_state.as_deref() == Some("required") {
                    "auth_required"
                } else if diagnostic.is_some() {
                    "healthy"
                } else {
                    "unknown"
                }
                .to_string(),
                auth_state,
                available_actions,
                requires_new_session: false,
                read_only_reason: managed_by_plugin.then(|| "plugin_managed_mcp".to_string()),
                // MCP diagnostic text can include transport details. Persist only the
                // normalized health state, never a raw configuration-derived reason.
                warning: None,

                ..Default::default()})
        })
        .collect()
}

fn plugin_selector(id: &str, marketplace: &str) -> String {
    if id.contains('@') {
        id.to_string()
    } else {
        format!("{id}@{marketplace}")
    }
}

fn plugin_name(id: &str) -> String {
    id.split('@').next().unwrap_or(id).to_string()
}

fn managed_plugin_name(name: &str) -> Option<String> {
    let plugin_name = name.strip_prefix("plugin:")?.split(':').next()?.trim();
    (!plugin_name.is_empty()).then(|| plugin_name.to_string())
}

fn category_from_plugin_source(plugin: &Value) -> Option<String> {
    let source_path = nested_string_field(plugin, &["source", "path"])?;
    let manifest_path = Path::new(&source_path).join(".codex-plugin/plugin.json");
    let manifest = super::read_catalog_manifest(&manifest_path).ok()?;
    nested_string_field(&manifest, &["interface", "category"])
}

fn is_official_marketplace(value: &str) -> bool {
    OFFICIAL_MARKETPLACES.contains(&value)
}

fn normalize_skill_scope(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "system" | "builtin" => "builtin",
        "workspace" | "repo" | "project" => "project",
        "plugin" => "plugin",
        "managed" | "enterprise" => "managed",
        _ => "user",
    }
    .to_string()
}

fn normalize_auth_state(value: &str) -> Option<String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("authenticated") || normalized.contains("logged") {
        Some("authenticated".to_string())
    } else if normalized.contains("required") || normalized.contains("unauth") {
        Some("required".to_string())
    } else if normalized.contains("fail") || normalized.contains("error") {
        Some("failed".to_string())
    } else {
        Some("unknown".to_string())
    }
}

fn transport_label(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.contains("stdio") {
                Some("stdio".to_string())
            } else if normalized.contains("http") || normalized.contains("sse") {
                Some("http".to_string())
            } else {
                Some("MCP".to_string())
            }
        }
        Some(Value::Object(value)) => ["type", "kind", "transport"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::to_string)
            .or_else(|| {
                if value.contains_key("command") {
                    Some("stdio".to_string())
                } else if value.contains_key("url") {
                    Some("http".to_string())
                } else {
                    Some("MCP".to_string())
                }
            }),
        _ => Some("MCP".to_string()),
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn nested_string_field(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_string)
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, path::PathBuf};

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        collect_remote_installed_plugins, parse_mcp_servers, parse_plugins, remote_plugin_skills,
    };

    #[test]
    fn auracoder_computer_control_is_hidden_from_the_extension_catalog() {
        let items = parse_mcp_servers(
            &json!([
                { "name": "auracoder-computer-control", "enabled": true },
                { "name": "user-server", "enabled": true }
            ]),
            None,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "user-server");
    }

    #[test]
    fn official_available_and_all_installed_plugins_are_preserved() {
        let value = json!({
            "installed": [
                {
                    "pluginId": "local-tool@personal",
                    "name": "local-tool",
                    "marketplaceName": "personal",
                    "installed": true,
                    "enabled": true
                }
            ],
            "available": [
                {
                    "pluginId": "official-tool@openai-curated",
                    "name": "official-tool",
                    "marketplaceName": "openai-curated",
                    "installed": false,
                    "enabled": false
                },
                {
                    "pluginId": "community-tool@community",
                    "name": "community-tool",
                    "marketplaceName": "community",
                    "installed": false,
                    "enabled": false
                }
            ]
        });

        let items = parse_plugins(&value, None, &HashMap::new(), &[]);
        assert!(items.iter().any(|item| item.id == "local-tool@personal"));
        assert!(items
            .iter()
            .any(|item| item.id == "official-tool@openai-curated" && item.officially_available));
        assert!(!items
            .iter()
            .any(|item| item.id == "community-tool@community"));
    }

    #[test]
    fn remote_install_marker_merges_plugin_and_plugin_skills() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.tmp")
            .join(format!("codex-remote-plugin-test-{}", Uuid::new_v4()));
        let package = root.join("openai-curated-remote/github");
        let version_root = package.join("0.1.8-test");
        let plugin_manifest = version_root.join(".codex-plugin/plugin.json");
        let skill_manifest = version_root.join("skills/github/SKILL.md");

        fs::create_dir_all(plugin_manifest.parent().expect("plugin manifest parent"))
            .expect("create plugin manifest directory");
        fs::create_dir_all(skill_manifest.parent().expect("skill manifest parent"))
            .expect("create skill directory");
        fs::write(package.join(super::REMOTE_INSTALL_MARKER), "{}")
            .expect("write remote install marker");
        fs::write(
            &plugin_manifest,
            r#"{
                "name": "github",
                "version": "0.1.8-test",
                "skills": "./skills/",
                "interface": {
                    "displayName": "GitHub",
                    "shortDescription": "Triage PRs, issues, CI, and publish flows"
                }
            }"#,
        )
        .expect("write plugin manifest");
        fs::write(
            &skill_manifest,
            "---\nname: github:github\ndescription: Triage GitHub work\n---\n",
        )
        .expect("write skill manifest");

        let remote_plugins = collect_remote_installed_plugins(&root);
        assert_eq!(remote_plugins.len(), 1);
        let value = json!({
            "installed": [],
            "available": [{
                "pluginId": "github@openai-curated",
                "name": "github",
                "marketplaceName": "openai-curated",
                "version": "0.1.6"
            }]
        });
        let plugins = parse_plugins(&value, None, &HashMap::new(), &remote_plugins);
        let github = plugins
            .iter()
            .find(|item| item.id == "github@openai-curated")
            .expect("GitHub plugin should be present");
        assert_eq!(github.installed, Some(true));
        assert!(github.officially_available);
        assert_eq!(github.name, "GitHub");
        assert_eq!(github.version.as_deref(), Some("0.1.8-test"));
        assert_eq!(
            github.description.as_deref(),
            Some("Triage PRs, issues, CI, and publish flows")
        );

        let skills = remote_plugin_skills(&remote_plugins);
        assert!(skills.iter().any(|item| {
            item.name == "github:github"
                && item.parent_plugin_id.as_deref() == Some("github@openai-curated")
                && item.installed == Some(true)
        }));

        fs::remove_dir_all(&root).expect("remove temporary fixture");
    }
}
