use std::{fs, path::PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::runtime_env;

pub fn parse_host_key(value: &str) -> anyhow::Result<(String, String)> {
    let fields = value.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 2 || !fields[1].contains('=') && STANDARD.decode(fields[1]).is_err() {
        anyhow::bail!("Host Key 必须是完整的 OpenSSH 公钥行")
    }
    let key_type = fields
        .iter()
        .find(|field| {
            field.starts_with("ssh-") || field.starts_with("ecdsa-") || field.starts_with("sk-")
        })
        .copied()
        .unwrap_or(fields[0]);
    let key_index = fields
        .iter()
        .position(|field| *field == key_type)
        .unwrap_or(0);
    let key = fields.get(key_index + 1).copied().unwrap_or_default();
    if key.is_empty() || STANDARD.decode(key).is_err() {
        anyhow::bail!("Host Key 的公钥内容不是有效的 base64")
    }
    Ok((key_type.to_string(), key.to_string()))
}

pub fn path_for(id: &str) -> PathBuf {
    runtime_env::app_data_dir()
        .join("ssh")
        .join("known-hosts")
        .join(id)
}

pub fn write(
    id: &str,
    host: &str,
    port: u16,
    key_type: &str,
    key_base64: &str,
) -> anyhow::Result<()> {
    let path = path_for(id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    fs::write(path, format!("{host_field} {key_type} {key_base64}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_host_key;

    #[test]
    fn accepts_full_openssh_public_key_line() {
        let parsed = parse_host_key("ssh-ed25519 AQID test").expect("valid host key");
        assert_eq!(parsed.0, "ssh-ed25519");
        assert_eq!(parsed.1, "AQID");
    }

    #[test]
    fn rejects_fingerprint_only_value() {
        assert!(parse_host_key("SHA256:abc").is_err());
    }
}
