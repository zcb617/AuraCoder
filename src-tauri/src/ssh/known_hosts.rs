use std::{
    fs,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use anyhow::Context;
use uuid::Uuid;

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
    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    write_file(&path, &format!("{host_field} {key_type} {key_base64}\n"))?;
    Ok(())
}

/// 完整写入同目录临时文件，并在内容变化时原子替换 known_hosts 文件。
fn write_file(path: &Path, content: &str) -> anyhow::Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == content.as_bytes() => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("known_hosts 文件路径缺少文件名"))?
        .to_os_string();
    temporary_name.push(format!(".{}.tmp", Uuid::new_v4()));
    let temporary_path = path.with_file_name(temporary_name);
    // 临时文件写入或替换失败时删除本次临时文件，并保留最初 IO 错误。
    let replace_result =
        fs::write(&temporary_path, content).and_then(|_| fs::rename(&temporary_path, path));

    if let Err(error) = replace_result {
        if let Err(cleanup_error) = fs::remove_file(&temporary_path) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                log::error!(
                    "清理 SSH known_hosts 临时文件失败: {cleanup_error}; path={}; 原始替换错误: {error}",
                    temporary_path.display()
                );
            }
        }
        return Err(error)
            .context("写入或替换 SSH known_hosts 文件失败")
            .with_context(|| format!("临时文件: {}", temporary_path.display()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/ssh_known_hosts_tests.rs"]
mod tests;
