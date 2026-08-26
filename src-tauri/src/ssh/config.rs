use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use tokio::{
    process::Command,
    time::{timeout, Duration},
};

use crate::{models::SshConfigHostDto, process_utils, runtime_env};

#[derive(Debug, Clone)]
pub struct ParsedHost {
    pub alias: String,
    pub host_name: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
}

pub async fn scan() -> anyhow::Result<Vec<ParsedHost>> {
    let path = runtime_env::home_dir()
        .ok_or_else(|| anyhow::anyhow!("无法确定用户主目录"))?
        .join(".ssh")
        .join("config");
    if !path.exists() {
        anyhow::bail!("SSH 配置文件不存在: {}", path.display());
    }
    let mut aliases = BTreeSet::new();
    let mut visited = BTreeSet::new();
    parse_file(&path, &mut visited, &mut aliases)?;
    let mut hosts = Vec::new();
    for alias in aliases {
        if let Some(host) = resolve_alias(&alias).await? {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

fn parse_file(
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    aliases: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let content = fs::read_to_string(&canonical)?;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(keyword) = parts.next() else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("host") {
            for token in parts {
                if !token.starts_with('!')
                    && !token.contains('*')
                    && !token.contains('?')
                    && token != "*"
                {
                    aliases.insert(token.to_string());
                }
            }
        } else if keyword.eq_ignore_ascii_case("include") {
            for include in parts {
                for candidate in
                    expand_include(canonical.parent().unwrap_or(Path::new(".")), include)
                {
                    parse_file(&candidate, visited, aliases)?;
                }
            }
        }
    }
    Ok(())
}

fn expand_include(base: &Path, value: &str) -> Vec<PathBuf> {
    let path = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        base.join(value)
    };
    if !value.contains('*') && !value.contains('?') {
        return vec![path];
    }
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Some(pattern) = path.file_name().and_then(|v| v.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            glob_match(pattern, &name).then_some(entry.path())
        })
        .collect()
}

fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let mut remainder = value;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }
    !pattern.ends_with('*') && remainder.is_empty() || pattern.ends_with('*')
}

async fn resolve_alias(alias: &str) -> anyhow::Result<Option<ParsedHost>> {
    let Some(ssh) = runtime_env::resolve_executable("ssh") else {
        return Ok(None);
    };
    let mut command = Command::new(ssh);
    process_utils::configure_tokio_command(&mut command);
    command
        .args(["-G", alias])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = match timeout(Duration::from_secs(5), command.output()).await {
        Ok(result) => result?,
        Err(_) => return Ok(None),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut host_name = None;
    let mut user = None;
    let mut port = None;
    let mut identity_file = None;
    for line in text.lines() {
        let mut parts = line.splitn(2, ' ');
        let Some(key) = parts.next() else {
            continue;
        };
        let value = parts.next().unwrap_or("").trim();
        match key {
            "hostname" => host_name = Some(value.to_string()),
            "user" => user = Some(value.to_string()),
            "port" => port = value.parse().ok(),
            "identityfile" if identity_file.is_none() && value != "none" => {
                identity_file = Some(value.to_string())
            }
            _ => {}
        }
    }
    Ok(Some(ParsedHost {
        alias: alias.to_string(),
        host_name: host_name.unwrap_or_else(|| alias.to_string()),
        user: user.unwrap_or_default(),
        port: port.unwrap_or(22),
        identity_file,
    }))
}

pub fn as_dto(host: ParsedHost, imported: bool, deleted: bool) -> SshConfigHostDto {
    SshConfigHostDto {
        alias: host.alias,
        host_name: host.host_name,
        user: host.user,
        port: host.port,
        identity_file: host.identity_file,
        imported,
        deleted,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use uuid::Uuid;

    use super::parse_file;

    #[test]
    fn parses_include_and_ignores_wildcard_hosts() {
        let root = std::env::temp_dir().join(format!("auracoder-ssh-config-{}", Uuid::new_v4()));
        let include_dir = root.join("conf.d");
        fs::create_dir_all(&include_dir).expect("create test config directory");
        fs::write(
            root.join("config"),
            "Host *\n  User ignored\nInclude conf.d/*.conf\nHost direct\n",
        )
        .expect("write root config");
        fs::write(
            include_dir.join("remote.conf"),
            "Host included\n  HostName example.com\n",
        )
        .expect("write included config");
        let mut visited = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        parse_file(&root.join("config"), &mut visited, &mut aliases).expect("parse config");
        assert_eq!(
            aliases.into_iter().collect::<Vec<_>>(),
            vec!["direct", "included"]
        );
        let _ = fs::remove_dir_all(root);
    }
}
