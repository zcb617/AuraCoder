use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::LazyLock,
};

use anyhow::Context;
use tokio::{fs as tokio_fs, sync::Mutex};
use uuid::Uuid;

use crate::{
    db::ssh_connections::SshConnectionRecord,
    engines::TurnAttachment,
    ssh::{gateway, runtime::quote_posix},
};

const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
const REMOTE_ATTACHMENT_CACHE_ROOT: &str = ".cache/auracoder/attachments";
const REMOTE_ATTACHMENT_EXPIRY_DAYS: u32 = 7;
const TEXT_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "txt", "md", "json", "js", "ts", "tsx", "jsx", "py", "rs", "go", "css", "html", "yaml", "yml",
    "toml", "xml", "sql", "sh", "csv", "svg",
];

static PENDING_CLEANUP_PATHS: LazyLock<Mutex<HashMap<String, BTreeSet<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct RemoteAttachmentBatch {
    pub attachments: Vec<TurnAttachment>,
    record: SshConnectionRecord,
    remote_paths: Vec<String>,
}

impl RemoteAttachmentBatch {
    pub async fn cleanup(&self) {
        if self.remote_paths.is_empty() {
            return;
        }
        if let Err(error) = cleanup_remote_paths(&self.record, &self.remote_paths).await {
            let mut pending = PENDING_CLEANUP_PATHS.lock().await;
            pending
                .entry(self.record.dto.id.clone())
                .or_default()
                .extend(self.remote_paths.iter().cloned());
            log::warn!(
                "清理 SSH 远端附件缓存失败，已保留待重试记录: connection_id={} error={error:#}",
                self.record.dto.id
            );
        }
    }
}

pub async fn upload_turn_attachments(
    record: &SshConnectionRecord,
    workspace_id: &str,
    thread_id: &str,
    attachments: &[TurnAttachment],
) -> anyhow::Result<RemoteAttachmentBatch> {
    validate_cache_component(workspace_id, "workspace ID")?;
    validate_cache_component(thread_id, "thread ID")?;
    retry_pending_cleanup(record).await;
    expire_old_cache_files(record).await;

    let mut batch = RemoteAttachmentBatch {
        attachments: Vec::with_capacity(attachments.len()),
        record: record.clone(),
        remote_paths: Vec::with_capacity(attachments.len()),
    };

    for attachment in attachments {
        let upload_result = async {
            let local_path = attachment.file_path.trim();
            anyhow::ensure!(!local_path.is_empty(), "附件路径不能为空");
            let metadata = tokio_fs::metadata(local_path)
                .await
                .with_context(|| format!("本机附件不存在或不可读：{}", attachment.file_name))?;
            anyhow::ensure!(
                metadata.is_file(),
                "本机附件不是普通文件：{}",
                attachment.file_name
            );
            anyhow::ensure!(
                metadata.len() <= MAX_ATTACHMENT_BYTES,
                "附件 `{}` 超过 10 MB 大小限制",
                attachment.file_name
            );
            let bytes = tokio_fs::read(local_path)
                .await
                .with_context(|| format!("读取本机附件失败：{}", attachment.file_name))?;
            anyhow::ensure!(
                bytes.len() as u64 <= MAX_ATTACHMENT_BYTES,
                "附件 `{}` 超过 10 MB 大小限制",
                attachment.file_name
            );

            let remote_text_content = if attachment
                .mime_type
                .as_deref()
                .map(|mime_type| {
                    let mime_type = mime_type.trim().to_lowercase();
                    mime_type.starts_with("text/")
                        || mime_type.contains("json")
                        || mime_type.contains("javascript")
                        || mime_type.contains("typescript")
                        || mime_type == "image/svg+xml"
                })
                .unwrap_or(false)
                || Path::new(&attachment.file_name)
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|extension| {
                        TEXT_ATTACHMENT_EXTENSIONS.contains(&extension.to_lowercase().as_str())
                    })
                    .unwrap_or(false)
            {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            } else {
                None
            };
            let stored_file_name = safe_stored_file_name(&attachment.file_name, local_path);
            let relative_directory =
                format!("{REMOTE_ATTACHMENT_CACHE_ROOT}/{workspace_id}/{thread_id}");
            let command = format!(
                "set -eu; umask 077; dir=\"$HOME/{}\"; mkdir -p -- \"$dir\"; target=\"$dir/{}\"; tmp=\"$target.part-{}\"; cleanup() {{ rm -f -- \"$tmp\"; }}; trap cleanup EXIT HUP INT TERM; cat > \"$tmp\"; actual=$(wc -c < \"$tmp\" | tr -d '[:space:]'); expected={}; if [ \"$actual\" != \"$expected\" ]; then echo \"SSH 远端附件字节数校验失败: expected=$expected actual=$actual\" >&2; exit 48; fi; mv -f -- \"$tmp\" \"$target\"; trap - EXIT HUP INT TERM; printf '%s' \"$target\"",
                relative_directory,
                stored_file_name,
                Uuid::new_v4().simple(),
                bytes.len(),
            );
            let output = gateway::run_command_with_input(record, &command, &bytes)
                .await
                .with_context(|| format!("上传 SSH 远端附件失败：{}", attachment.file_name))?;
            let remote_path = String::from_utf8(output)
                .context("SSH 远端附件路径不是有效 UTF-8")?
                .trim()
                .to_string();
            anyhow::ensure!(
                remote_path.starts_with('/')
                    && !remote_path.contains('\n')
                    && !remote_path.contains('\r'),
                "SSH 远端附件返回了无效缓存路径"
            );

            let mut uploaded = attachment.clone();
            uploaded.file_path = remote_path.clone();
            uploaded.size_bytes = bytes.len() as u64;
            uploaded.is_remote = true;
            uploaded.remote_text_content = remote_text_content;
            anyhow::Ok((uploaded, remote_path))
        }
        .await;

        match upload_result {
            Ok((uploaded, remote_path)) => {
                batch.attachments.push(uploaded);
                batch.remote_paths.push(remote_path);
            }
            Err(error) => {
                batch.cleanup().await;
                return Err(error);
            }
        }
    }

    Ok(batch)
}

fn validate_cache_component(value: &str, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "无效的 SSH 远端附件缓存 {label}"
    );
    Ok(())
}

fn safe_stored_file_name(display_name: &str, local_path: &str) -> String {
    let extension = Path::new(display_name)
        .extension()
        .or_else(|| Path::new(local_path).extension())
        .and_then(|value| value.to_str())
        .map(|value| {
            value
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .take(12)
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|value| !value.is_empty());
    match extension {
        Some(extension) => format!("{}.{}", Uuid::new_v4().simple(), extension),
        None => Uuid::new_v4().simple().to_string(),
    }
}

async fn cleanup_remote_paths(
    record: &SshConnectionRecord,
    remote_paths: &[String],
) -> anyhow::Result<()> {
    let paths = remote_paths
        .iter()
        .map(|path| quote_posix(path))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("rm -f -- {paths}");
    gateway::run_command(record, &command)
        .await
        .context("删除 SSH 远端附件缓存失败")?;
    Ok(())
}

async fn retry_pending_cleanup(record: &SshConnectionRecord) {
    let pending_paths = PENDING_CLEANUP_PATHS
        .lock()
        .await
        .remove(&record.dto.id)
        .map(|paths| paths.into_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if pending_paths.is_empty() {
        return;
    }
    if let Err(error) = cleanup_remote_paths(record, &pending_paths).await {
        PENDING_CLEANUP_PATHS
            .lock()
            .await
            .entry(record.dto.id.clone())
            .or_default()
            .extend(pending_paths);
        log::warn!(
            "重试清理 SSH 远端附件缓存失败: connection_id={} error={error:#}",
            record.dto.id
        );
    }
}

async fn expire_old_cache_files(record: &SshConnectionRecord) {
    let command = format!(
        "root=\"$HOME/{REMOTE_ATTACHMENT_CACHE_ROOT}\"; if [ -d \"$root\" ]; then find \"$root\" -type f -mtime +{REMOTE_ATTACHMENT_EXPIRY_DAYS} -delete 2>/dev/null || true; find \"$root\" -depth -type d -empty -delete 2>/dev/null || true; fi"
    );
    if let Err(error) = gateway::run_command(record, &command).await {
        log::warn!(
            "清理过期 SSH 远端附件缓存失败: connection_id={} error={error:#}",
            record.dto.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_components_reject_path_escape_characters() {
        assert!(validate_cache_component("workspace-1", "workspace ID").is_ok());
        assert!(validate_cache_component("../workspace", "workspace ID").is_err());
        assert!(validate_cache_component("workspace/thread", "workspace ID").is_err());
        assert!(validate_cache_component("C:\\workspace", "workspace ID").is_err());
    }

    #[test]
    fn stored_file_names_ignore_untrusted_original_names() {
        let stored = safe_stored_file_name("../../报告 final.PNG", "C:\\tmp\\source.PNG");
        assert!(stored.ends_with(".png"));
        assert!(!stored.contains("报告"));
        assert!(!stored.contains(".."));
        assert!(!stored.contains('/') && !stored.contains('\\'));
    }
}
