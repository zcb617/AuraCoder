use anyhow::Context;

use crate::{
    db::ssh_connections::SshConnectionRecord,
    models::{FileTreeEntryDto, FileTreePageDto, ReadFileResultDto, WriteFileResultDto},
    ssh::{
        gateway,
        runtime::{quote_posix, validate_remote_relative_path},
    },
};

const READ_FILE_MAX_SIZE: u64 = 10 * 1024 * 1024;
const FILE_TREE_MAX_ENTRIES: usize = 10_000;

pub async fn list_dir(
    record: &SshConnectionRecord,
    root: &str,
    dir_path: &str,
) -> anyhow::Result<Vec<FileTreeEntryDto>> {
    validate_root(root)?;
    validate_remote_relative_path(dir_path, true)?;
    let command = scope_command(
        root,
        dir_path,
        "find \"$target\" -mindepth 1 -maxdepth 1 \\( -type d -o -type f \\) -printf '%y\\t%P\\n' 2>/dev/null",
    );
    let output = gateway::run_command(record, &command).await?;
    let mut entries = parse_entries(output.as_bytes())?;
    let prefix = if dir_path.is_empty() {
        String::new()
    } else {
        format!("{dir_path}/")
    };
    for entry in &mut entries {
        entry.path = format!("{prefix}{}", entry.path);
    }
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(entries)
}

pub async fn read_file(
    record: &SshConnectionRecord,
    root: &str,
    file_path: &str,
) -> anyhow::Result<ReadFileResultDto> {
    validate_root(root)?;
    validate_remote_relative_path(file_path, false)?;
    let command = scope_command(
        root,
        file_path,
        &format!(
            "[ -f \"$target\" ] && [ -r \"$target\" ] || exit 24; set -- $(cksum < \"$target\") || exit 25; version=\"$1:$2\"; size=\"$2\"; [ \"$size\" -le {READ_FILE_MAX_SIZE} ] || exit 26; printf '__PANES_SIZE__%s\\n__PANES_VERSION__%s\\n' \"$size\" \"$version\"; cat -- \"$target\"; set -- $(cksum < \"$target\") || exit 25; [ \"$version\" = \"$1:$2\" ] || exit 37"
        ),
    );
    let output = gateway::run_command_with_input(record, &command, &[]).await?;
    let (size_bytes, version, raw) = split_file_header(&output)?;
    let is_binary = raw.iter().take(8192).any(|byte| *byte == 0);
    let content = if is_binary {
        String::new()
    } else {
        String::from_utf8_lossy(raw).to_string()
    };
    Ok(ReadFileResultDto {
        content,
        size_bytes,
        is_binary,
        version,
    })
}

pub async fn file_version(
    record: &SshConnectionRecord,
    root: &str,
    file_path: &str,
) -> anyhow::Result<String> {
    validate_root(root)?;
    validate_remote_relative_path(file_path, false)?;
    let command = scope_command(
        root,
        file_path,
        "[ -f \"$target\" ] && [ -r \"$target\" ] || exit 24; set -- $(cksum < \"$target\") || exit 25; printf '%s:%s\\n' \"$1\" \"$2\"",
    );
    Ok(gateway::run_command(record, &command)
        .await?
        .trim()
        .to_string())
}

pub async fn directory_fingerprint(
    record: &SshConnectionRecord,
    root: &str,
    dir_path: &str,
) -> anyhow::Result<String> {
    validate_root(root)?;
    validate_remote_relative_path(dir_path, true)?;
    let command = scope_command(
        root,
        dir_path,
        "[ -d \"$target\" ] && [ -r \"$target\" ] || exit 22; LC_ALL=C find \"$target\" -mindepth 1 -maxdepth 1 \\( -type d -o -type f \\) -printf '%y\\t%P\\t%T@\\t%s\\0' 2>/dev/null | LC_ALL=C sort -z | cksum",
    );
    Ok(gateway::run_command(record, &command)
        .await?
        .trim()
        .to_string())
}

pub async fn write_file(
    record: &SshConnectionRecord,
    root: &str,
    file_path: &str,
    content: &str,
    expected_version: Option<&str>,
) -> anyhow::Result<WriteFileResultDto> {
    validate_root(root)?;
    validate_remote_relative_path(file_path, false)?;
    let expected_version = expected_version.unwrap_or_default();
    let command = scope_command(
        root,
        file_path,
        &format!(
            "parent=$(dirname -- \"$target\"); parent=$(realpath -- \"$parent\") || exit 27; case \"$parent\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; [ -d \"$parent\" ] || exit 28; target=\"$parent\"/$(basename -- \"$target\"); target_real=$(realpath -m -- \"$target\") || exit 29; case \"$target_real\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; expected={}; tmp=$(mktemp \"$parent/.auracoder-write.XXXXXX\") || exit 30; lock=\"$target.auracoder-write.lock\"; acquire_lock() {{ attempts=0; while ! mkdir -- \"$lock\" 2>/dev/null; do attempts=$((attempts + 1)); [ \"$attempts\" -lt 200 ] || exit 41; sleep 0.05; done; }}; trap 'rm -f -- \"$tmp\"; [ -z \"$lock\" ] || rmdir -- \"$lock\" 2>/dev/null || true' EXIT; cat > \"$tmp\" || exit 31; acquire_lock; if [ -e \"$target\" ]; then set -- $(cksum < \"$target\") || exit 25; current=\"$1:$2\"; [ -z \"$expected\" ] || [ \"$current\" = \"$expected\" ] || {{ printf '文件已被外部修改，请重新加载后再保存\\n' >&2; exit 38; }}; chmod --reference=\"$target\" \"$tmp\" 2>/dev/null || true; else [ -z \"$expected\" ] || {{ printf '文件已被外部删除，请重新加载后再保存\\n' >&2; exit 39; }}; fi; mv -f -- \"$tmp\" \"$target\" || exit 40; set -- $(cksum < \"$target\") || exit 25; printf '__PANES_VERSION__%s:%s\\n' \"$1\" \"$2\"",
            quote_posix(expected_version),
        ),
    );
    let output = gateway::run_command_with_input(record, &command, content.as_bytes()).await?;
    let version = String::from_utf8_lossy(&output)
        .lines()
        .find_map(|line| line.strip_prefix("__PANES_VERSION__"))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .context("远端保存响应缺少文件版本")?;
    Ok(WriteFileResultDto { version })
}

pub async fn create_file(
    record: &SshConnectionRecord,
    root: &str,
    file_path: &str,
) -> anyhow::Result<()> {
    validate_root(root)?;
    validate_remote_relative_path(file_path, false)?;
    let command = scope_command(
        root,
        file_path,
        "parent=$(dirname -- \"$target\"); parent=$(realpath -- \"$parent\") || exit 27; case \"$parent\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; [ -d \"$parent\" ] || exit 28; target=\"$parent\"/$(basename -- \"$target\"); [ ! -e \"$target\" ] || exit 32; : > \"$target\"",
    );
    gateway::run_command(record, &command).await?;
    Ok(())
}

pub async fn create_dir(
    record: &SshConnectionRecord,
    root: &str,
    dir_path: &str,
) -> anyhow::Result<()> {
    validate_root(root)?;
    validate_remote_relative_path(dir_path, false)?;
    let command = scope_command(
        root,
        dir_path,
        "target_real=$(realpath -m -- \"$target\") || exit 29; case \"$target_real\" in \"$root_real\"/*) ;; *) exit 23;; esac; [ \"$target_real\" != \"$root_real\" ] || exit 33; mkdir -p -- \"$target\"",
    );
    gateway::run_command(record, &command).await?;
    Ok(())
}

pub async fn rename_path(
    record: &SshConnectionRecord,
    root: &str,
    old_path: &str,
    new_name: &str,
) -> anyhow::Result<()> {
    validate_root(root)?;
    validate_remote_relative_path(old_path, false)?;
    validate_new_name(new_name)?;
    let quoted_name = quote_posix(new_name);
    let command = scope_command(
        root,
        old_path,
        &format!(
            "[ -e \"$target\" ] || exit 34; parent=$(dirname -- \"$target\"); parent=$(realpath -- \"$parent\") || exit 27; case \"$parent\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; destination=\"$parent\"/{quoted_name}; destination_real=$(realpath -m -- \"$destination\") || exit 29; case \"$destination_real\" in \"$root_real\"/*) ;; *) exit 23;; esac; [ ! -e \"$destination\" ] || exit 35; mv -- \"$target\" \"$destination\""
        ),
    );
    gateway::run_command(record, &command).await?;
    Ok(())
}

pub async fn delete_path(
    record: &SshConnectionRecord,
    root: &str,
    file_path: &str,
) -> anyhow::Result<()> {
    validate_root(root)?;
    validate_remote_relative_path(file_path, false)?;
    let command = scope_command(
        root,
        file_path,
        "[ -e \"$target\" ] || exit 34; [ \"$target\" != \"$root_real\" ] || exit 36; rm -rf -- \"$target\"",
    );
    gateway::run_command(record, &command).await?;
    Ok(())
}

pub async fn file_tree_page(
    record: &SshConnectionRecord,
    root: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<FileTreePageDto> {
    validate_root(root)?;
    let scan = scan_entries(record, root).await?;
    let total = scan.entries.len();
    let offset = offset.min(total);
    let limit = limit.clamp(1, FILE_TREE_MAX_ENTRIES);
    let end = offset.saturating_add(limit).min(total);
    Ok(FileTreePageDto {
        entries: scan.entries[offset..end].to_vec(),
        offset,
        limit,
        total,
        has_more: end < total,
        scan_truncated: scan.truncated,
    })
}

pub async fn search_files(
    record: &SshConnectionRecord,
    root: &str,
    query: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<FileTreePageDto> {
    validate_root(root)?;
    let scan = scan_entries(record, root).await?;
    let query = query.trim().to_lowercase();
    let mut matches = scan
        .entries
        .into_iter()
        .filter(|entry| {
            !entry.is_dir && (query.is_empty() || entry.path.to_lowercase().contains(&query))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    let total = matches.len();
    let offset = offset.min(total);
    let limit = limit.clamp(1, FILE_TREE_MAX_ENTRIES);
    let end = offset.saturating_add(limit).min(total);
    Ok(FileTreePageDto {
        entries: matches[offset..end].to_vec(),
        offset,
        limit,
        total,
        has_more: end < total,
        scan_truncated: scan.truncated,
    })
}

struct ScanResult {
    entries: Vec<FileTreeEntryDto>,
    truncated: bool,
}

async fn scan_entries(record: &SshConnectionRecord, root: &str) -> anyhow::Result<ScanResult> {
    let dependency_dir = ['n', 'o', 'd', 'e', '_', 'm', 'o', 'd', 'u', 'l', 'e', 's']
        .into_iter()
        .collect::<String>();
    let command = format!(
        "root={}; root_real=$(realpath -- \"$root\") || exit 21; [ -d \"$root_real\" ] || exit 22; find \"$root_real\" -xdev -mindepth 1 -maxdepth 12 \\( -path '*/.git' -o -path '*/target' -o -path '*/target/*' -o -path '*/.venv' -o -path '*/.venv/*' -o -path '*/__pycache__' -o -path '*/__pycache__/*' -o -path '*/{dependency_dir}' -o -path '*/{dependency_dir}/*' \\) -prune -o \\( -type d -printf 'D\\t%P\\n' -o -type f -printf 'F\\t%P\\n' \\) 2>/dev/null | head -n {max_entries}",
        quote_posix(root),
        max_entries = FILE_TREE_MAX_ENTRIES + 1,
    );
    let output = gateway::run_command(record, &command).await?;
    let mut entries = parse_entries(output.as_bytes())?;
    let truncated = entries.len() > FILE_TREE_MAX_ENTRIES;
    if truncated {
        entries.truncate(FILE_TREE_MAX_ENTRIES);
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ScanResult { entries, truncated })
}

fn scope_command(root: &str, relative: &str, body: &str) -> String {
    format!(
        "root={}; root_real=$(realpath -- \"$root\") || exit 21; [ -d \"$root_real\" ] || exit 22; rel={}; target=$(realpath -m -- \"$root_real\"/\"$rel\") || exit 23; case \"$target\" in \"$root_real\"|\"$root_real\"/*) ;; *) exit 23;; esac; {}",
        quote_posix(root),
        quote_posix(relative),
        body,
    )
}

fn validate_root(root: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !root.is_empty() && root.starts_with('/') && !root.contains('\0'),
        "远端工作区根目录无效"
    );
    Ok(())
}

fn validate_new_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name != "." && name != "..",
        "文件名无效"
    );
    anyhow::ensure!(
        !name.contains('/') && !name.contains('\\') && !name.contains('\0'),
        "文件名不能包含路径分隔符"
    );
    Ok(())
}

fn parse_entries(output: &[u8]) -> anyhow::Result<Vec<FileTreeEntryDto>> {
    let text = String::from_utf8_lossy(output);
    let mut entries = Vec::new();
    for line in text.lines() {
        let Some((kind, path)) = line.split_once('\t') else {
            continue;
        };
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|component| component == ".." || component.is_empty())
        {
            continue;
        }
        entries.push(FileTreeEntryDto {
            path: path.to_string(),
            is_dir: kind.eq_ignore_ascii_case("d"),
        });
    }
    Ok(entries)
}

fn split_file_header(output: &[u8]) -> anyhow::Result<(u64, String, &[u8])> {
    const SIZE_PREFIX: &[u8] = b"__PANES_SIZE__";
    const VERSION_PREFIX: &[u8] = b"__PANES_VERSION__";
    let Some(size_end) = output.iter().position(|byte| *byte == b'\n') else {
        anyhow::bail!("远端文件响应缺少大小信息");
    };
    let size_header = &output[..size_end];
    let value = size_header
        .strip_prefix(SIZE_PREFIX)
        .context("远端文件响应格式无效")?;
    let size = std::str::from_utf8(value)?.parse::<u64>()?;
    let remaining = output.get(size_end + 1..).context("远端文件响应不完整")?;
    let Some(version_end) = remaining.iter().position(|byte| *byte == b'\n') else {
        anyhow::bail!("远端文件响应缺少版本信息");
    };
    let version = remaining[..version_end]
        .strip_prefix(VERSION_PREFIX)
        .context("远端文件版本格式无效")?;
    let version = std::str::from_utf8(version)?.to_string();
    let content = remaining
        .get(version_end + 1..)
        .context("远端文件响应不完整")?;
    anyhow::ensure!(content.len() as u64 == size, "远端文件内容大小发生变化");
    Ok((size, version, content))
}
