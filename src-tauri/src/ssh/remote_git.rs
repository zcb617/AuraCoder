use crate::{
    db::ssh_connections::SshConnectionRecord,
    models::{
        GitBranchDto, GitBranchPageDto, GitBranchScopeDto, GitCommitDto, GitCommitPageDto,
        GitDiffPreviewDto, GitFileStatusDto, GitRemoteDto, GitStashDto, GitStatusDto,
        GitWorktreeDto,
    },
    ssh::{gateway, remote_fs, runtime::quote_posix},
};

const DIFF_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct RemoteRepoInfo {
    pub name: String,
    pub default_branch: String,
}

pub async fn discover(
    record: &SshConnectionRecord,
    root: &str,
) -> anyhow::Result<Option<RemoteRepoInfo>> {
    let command = format!(
        "cd -- {} && if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then printf 'true\\n'; else printf 'false\\n'; fi",
        quote_posix(root)
    );
    let output = gateway::run_command(record, &command).await?;
    if output.trim() != "true" {
        return Ok(None);
    }
    let branch = gateway::run_command(
        record,
        &git_script(root, &["symbolic-ref", "--short", "HEAD"]),
    )
    .await
    .unwrap_or_else(|_| "HEAD".to_string())
    .trim()
    .to_string();
    let name = root
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("远端仓库")
        .to_string();
    let _ = output;
    Ok(Some(RemoteRepoInfo {
        name,
        default_branch: if branch.is_empty() {
            "main".to_string()
        } else {
            branch
        },
    }))
}

pub async fn init(record: &SshConnectionRecord, root: &str) -> anyhow::Result<()> {
    run(record, root, &["init".to_string()]).await.map(|_| ())
}

pub async fn status(record: &SshConnectionRecord, root: &str) -> anyhow::Result<GitStatusDto> {
    let output = gateway::run_command(
        record,
        &git_script(
            root,
            &["status", "--porcelain=v1", "--branch", "--ahead-behind"],
        ),
    )
    .await?;
    let mut lines = output.lines();
    let branch_line = lines.next().unwrap_or("## HEAD");
    let branch = branch_line
        .strip_prefix("## ")
        .unwrap_or(branch_line)
        .split("...")
        .next()
        .unwrap_or("HEAD")
        .split(" [")
        .next()
        .unwrap_or("HEAD")
        .to_string();
    let (ahead, behind) = parse_ahead_behind(branch_line);
    let files = lines.filter_map(parse_status_line).collect::<Vec<_>>();
    Ok(GitStatusDto {
        branch,
        files,
        ahead,
        behind,
    })
}

pub async fn diff(
    record: &SshConnectionRecord,
    root: &str,
    file_path: Option<&str>,
    staged: bool,
) -> anyhow::Result<GitDiffPreviewDto> {
    let mut args = vec!["diff".to_string()];
    if staged {
        args.push("--staged".to_string());
    }
    args.push("--".to_string());
    if let Some(path) = file_path {
        args.push(path.to_string());
    }
    let output = gateway::run_command(record, &git_script_owned(root, &args)).await?;
    let raw = output.as_bytes();
    let original_bytes = raw.len();
    let content = String::from_utf8_lossy(&raw[..raw.len().min(DIFF_MAX_BYTES)]).to_string();
    Ok(GitDiffPreviewDto {
        truncated: original_bytes > content.len(),
        original_bytes,
        returned_bytes: content.len(),
        content,
    })
}

pub async fn stage(
    record: &SshConnectionRecord,
    root: &str,
    files: &[String],
) -> anyhow::Result<()> {
    run_git_files(record, root, "add", files).await
}

pub async fn unstage(
    record: &SshConnectionRecord,
    root: &str,
    files: &[String],
) -> anyhow::Result<()> {
    let mut args = vec![
        "restore".to_string(),
        "--staged".to_string(),
        "--".to_string(),
    ];
    args.extend(files.iter().cloned());
    run(record, root, &args).await.map(|_| ())
}

pub async fn discard(
    record: &SshConnectionRecord,
    root: &str,
    files: &[String],
) -> anyhow::Result<()> {
    let mut args = vec![
        "restore".to_string(),
        "--worktree".to_string(),
        "--".to_string(),
    ];
    args.extend(files.iter().cloned());
    run(record, root, &args).await.map(|_| ())
}

pub async fn commit(
    record: &SshConnectionRecord,
    root: &str,
    message: &str,
) -> anyhow::Result<String> {
    let args = vec!["commit".to_string(), "-m".to_string(), message.to_string()];
    run(record, root, &args).await?;
    let hash = run(record, root, &["rev-parse".to_string(), "HEAD".to_string()]).await?;
    Ok(hash.trim().to_string())
}

pub async fn soft_reset_last_commit(
    record: &SshConnectionRecord,
    root: &str,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "reset".to_string(),
            "--soft".to_string(),
            "HEAD~1".to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn fetch(record: &SshConnectionRecord, root: &str) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "fetch".to_string(),
            "--all".to_string(),
            "--prune".to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn pull(record: &SshConnectionRecord, root: &str) -> anyhow::Result<()> {
    run(record, root, &["pull".to_string(), "--ff-only".to_string()])
        .await
        .map(|_| ())
}

pub async fn push(record: &SshConnectionRecord, root: &str) -> anyhow::Result<()> {
    run(record, root, &["push".to_string()]).await.map(|_| ())
}

pub async fn branches(
    record: &SshConnectionRecord,
    root: &str,
    scope: GitBranchScopeDto,
    offset: usize,
    limit: usize,
    search: Option<&str>,
) -> anyhow::Result<GitBranchPageDto> {
    let args = match scope {
        GitBranchScopeDto::Local => vec!["branch".to_string(), "--no-color".to_string()],
        GitBranchScopeDto::Remote => vec![
            "branch".to_string(),
            "-r".to_string(),
            "--no-color".to_string(),
        ],
    };
    let output = run(record, root, &args).await?;
    let mut entries = output
        .lines()
        .filter_map(|line| {
            let current = line.starts_with('*');
            let name = line.trim_start_matches(['*', ' ']).trim().to_string();
            if name.is_empty()
                || search.is_some_and(|query| !name.to_lowercase().contains(&query.to_lowercase()))
            {
                return None;
            }
            Some(GitBranchDto {
                full_name: name.clone(),
                name: name.strip_prefix("origin/").unwrap_or(&name).to_string(),
                is_current: current,
                is_remote: matches!(scope, GitBranchScopeDto::Remote),
                upstream: None,
                ahead: 0,
                behind: 0,
                last_commit_at: None,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    paginate_branches(entries, offset, limit)
}

pub async fn checkout_branch(
    record: &SshConnectionRecord,
    root: &str,
    branch: &str,
    is_remote: bool,
) -> anyhow::Result<()> {
    let args = if is_remote {
        vec!["checkout".to_string(), "-t".to_string(), branch.to_string()]
    } else {
        vec!["checkout".to_string(), branch.to_string()]
    };
    run(record, root, &args).await.map(|_| ())
}

pub async fn create_branch(
    record: &SshConnectionRecord,
    root: &str,
    branch: &str,
    from_ref: Option<&str>,
) -> anyhow::Result<()> {
    let mut args = vec!["checkout".to_string(), "-b".to_string(), branch.to_string()];
    if let Some(reference) = from_ref {
        args.push(reference.to_string());
    }
    run(record, root, &args).await.map(|_| ())
}

pub async fn rename_branch(
    record: &SshConnectionRecord,
    root: &str,
    old: &str,
    new: &str,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "branch".to_string(),
            "-m".to_string(),
            old.to_string(),
            new.to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn delete_branch(
    record: &SshConnectionRecord,
    root: &str,
    branch: &str,
    force: bool,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "branch".to_string(),
            if force { "-D" } else { "-d" }.to_string(),
            branch.to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn commits(
    record: &SshConnectionRecord,
    root: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<GitCommitPageDto> {
    let format = "%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e";
    let output = run(
        record,
        root,
        &["log".to_string(), format!("--format={format}")],
    )
    .await?;
    let entries = output
        .split('\u{1e}')
        .filter_map(|record| {
            let fields = record.split('\u{1f}').collect::<Vec<_>>();
            (fields.len() >= 6).then(|| GitCommitDto {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                author_name: fields[2].to_string(),
                author_email: fields[3].to_string(),
                authored_at: fields[4].to_string(),
                subject: fields[5].trim().to_string(),
                body: String::new(),
            })
        })
        .collect::<Vec<_>>();
    paginate_commits(entries, offset, limit)
}

pub async fn commit_diff(
    record: &SshConnectionRecord,
    root: &str,
    hash: &str,
) -> anyhow::Result<GitDiffPreviewDto> {
    let output = run(
        record,
        root,
        &[
            "show".to_string(),
            "--format=fuller".to_string(),
            hash.to_string(),
        ],
    )
    .await?;
    let raw = output.as_bytes();
    let content = String::from_utf8_lossy(&raw[..raw.len().min(DIFF_MAX_BYTES)]).to_string();
    Ok(GitDiffPreviewDto {
        truncated: raw.len() > content.len(),
        original_bytes: raw.len(),
        returned_bytes: content.len(),
        content,
    })
}

pub async fn stashes(record: &SshConnectionRecord, root: &str) -> anyhow::Result<Vec<GitStashDto>> {
    let output = run(record, root, &["stash".to_string(), "list".to_string()]).await?;
    Ok(output
        .lines()
        .enumerate()
        .map(|(index, line)| GitStashDto {
            index,
            name: line.to_string(),
            branch_hint: None,
            created_at: None,
        })
        .collect())
}

pub async fn stash_push(
    record: &SshConnectionRecord,
    root: &str,
    message: Option<&str>,
) -> anyhow::Result<()> {
    let mut args = vec!["stash".to_string(), "push".to_string()];
    if let Some(message) = message {
        args.extend(["-m".to_string(), message.to_string()]);
    }
    run(record, root, &args).await.map(|_| ())
}

pub async fn stash_apply(
    record: &SshConnectionRecord,
    root: &str,
    index: usize,
    pop: bool,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "stash".to_string(),
            if pop { "pop" } else { "apply" }.to_string(),
            format!("stash@{{{index}}}"),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn worktrees(
    record: &SshConnectionRecord,
    root: &str,
) -> anyhow::Result<Vec<GitWorktreeDto>> {
    let output = run(
        record,
        root,
        &[
            "worktree".to_string(),
            "list".to_string(),
            "--porcelain".to_string(),
        ],
    )
    .await?;
    Ok(parse_worktrees(&output))
}

pub async fn add_worktree(
    record: &SshConnectionRecord,
    root: &str,
    path: &str,
    branch: &str,
    base_ref: Option<&str>,
) -> anyhow::Result<GitWorktreeDto> {
    anyhow::ensure!(
        path.starts_with('/') && !path.contains('\0'),
        "远端工作树路径无效"
    );
    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| anyhow::anyhow!("远端工作树路径无效"))?;
    anyhow::ensure!(
        !name.is_empty() && name != "." && name != "..",
        "远端工作树路径无效"
    );
    let parent = if parent.is_empty() { "/" } else { parent };
    let output = gateway::run_command(
        record,
        &format!(
            "mkdir -p -- {}; parent=$(realpath -- {}) || exit 21; printf '__PANES_WORKTREE_PARENT__%s\\n' \"$parent\"",
            quote_posix(parent),
            quote_posix(parent),
        ),
    )
    .await?;
    let parent = output
        .lines()
        .find_map(|line| line.strip_prefix("__PANES_WORKTREE_PARENT__"))
        .filter(|value| value.starts_with('/'))
        .ok_or_else(|| anyhow::anyhow!("远端工作树父目录解析失败"))?;
    let path = format!("{}/{}", parent.trim_end_matches('/'), name);
    let mut args = vec![
        "worktree".to_string(),
        "add".to_string(),
        "-b".to_string(),
        branch.to_string(),
        path.clone(),
    ];
    if let Some(reference) = base_ref {
        args.push(reference.to_string());
    }
    run(record, root, &args).await?;
    worktrees(record, root)
        .await?
        .into_iter()
        .find(|item| item.path == path)
        .ok_or_else(|| anyhow::anyhow!("远端工作树创建后未找到"))
}

pub async fn remove_worktree(
    record: &SshConnectionRecord,
    root: &str,
    path: &str,
    force: bool,
) -> anyhow::Result<()> {
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if force {
        args.push("--force".to_string());
    }
    args.push(path.to_string());
    run(record, root, &args).await.map(|_| ())
}

pub async fn prune_worktrees(record: &SshConnectionRecord, root: &str) -> anyhow::Result<()> {
    run(record, root, &["worktree".to_string(), "prune".to_string()])
        .await
        .map(|_| ())
}

pub async fn file_tree(
    record: &SshConnectionRecord,
    root: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<crate::models::FileTreePageDto> {
    remote_fs::file_tree_page(record, root, offset, limit).await
}

pub async fn remotes(
    record: &SshConnectionRecord,
    root: &str,
) -> anyhow::Result<Vec<GitRemoteDto>> {
    let output = run(record, root, &["remote".to_string(), "-v".to_string()]).await?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?.to_string();
            let url = fields.next()?.to_string();
            Some(GitRemoteDto { name, url })
        })
        .collect())
}

pub async fn add_remote(
    record: &SshConnectionRecord,
    root: &str,
    name: &str,
    url: &str,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "remote".to_string(),
            "add".to_string(),
            name.to_string(),
            url.to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn remove_remote(
    record: &SshConnectionRecord,
    root: &str,
    name: &str,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &["remote".to_string(), "remove".to_string(), name.to_string()],
    )
    .await
    .map(|_| ())
}

pub async fn rename_remote(
    record: &SshConnectionRecord,
    root: &str,
    old: &str,
    new: &str,
) -> anyhow::Result<()> {
    run(
        record,
        root,
        &[
            "remote".to_string(),
            "rename".to_string(),
            old.to_string(),
            new.to_string(),
        ],
    )
    .await
    .map(|_| ())
}

async fn run_git_files(
    record: &SshConnectionRecord,
    root: &str,
    command: &str,
    files: &[String],
) -> anyhow::Result<()> {
    let mut args = vec![command.to_string(), "--".to_string()];
    args.extend(files.iter().cloned());
    run(record, root, &args).await.map(|_| ())
}

async fn run(record: &SshConnectionRecord, root: &str, args: &[String]) -> anyhow::Result<String> {
    gateway::run_command(record, &git_script_owned(root, args)).await
}

fn git_script(root: &str, args: &[&str]) -> String {
    let owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    git_script_owned(root, &owned)
}

fn git_script_owned(root: &str, args: &[String]) -> String {
    let rendered = args
        .iter()
        .map(|arg| quote_posix(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!("cd -- {} && git {rendered}", quote_posix(root))
}

fn parse_ahead_behind(line: &str) -> (usize, usize) {
    let Some(summary) = line
        .split_once(" [")
        .and_then(|(_, value)| value.strip_suffix(']'))
    else {
        return (0, 0);
    };
    let ahead = summary
        .split(',')
        .find_map(|part| {
            part.trim()
                .strip_prefix("ahead ")
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0);
    let behind = summary
        .split(',')
        .find_map(|part| {
            part.trim()
                .strip_prefix("behind ")
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0);
    (ahead, behind)
}

fn parse_status_line(line: &str) -> Option<GitFileStatusDto> {
    if line.len() < 4 {
        return None;
    }
    let bytes = line.as_bytes();
    let path = line[3..].split(" -> ").last()?.to_string();
    Some(GitFileStatusDto {
        path,
        index_status: status_label(bytes[0]),
        worktree_status: status_label(bytes[1]),
    })
}

fn status_label(value: u8) -> Option<String> {
    match value as char {
        ' ' => None,
        'M' => Some("modified".to_string()),
        'A' => Some("added".to_string()),
        'D' => Some("deleted".to_string()),
        'R' => Some("renamed".to_string()),
        'C' => Some("copied".to_string()),
        '?' => Some("untracked".to_string()),
        'U' => Some("unmerged".to_string()),
        other => Some(other.to_string()),
    }
}

fn paginate_branches(
    mut entries: Vec<GitBranchDto>,
    offset: usize,
    limit: usize,
) -> anyhow::Result<GitBranchPageDto> {
    let total = entries.len();
    let offset = offset.min(total);
    let end = offset.saturating_add(limit.max(1)).min(total);
    Ok(GitBranchPageDto {
        entries: entries.drain(offset..end).collect(),
        offset,
        limit: limit.max(1),
        total,
        has_more: end < total,
    })
}

fn paginate_commits(
    mut entries: Vec<GitCommitDto>,
    offset: usize,
    limit: usize,
) -> anyhow::Result<GitCommitPageDto> {
    let total = entries.len();
    let offset = offset.min(total);
    let end = offset.saturating_add(limit.max(1)).min(total);
    Ok(GitCommitPageDto {
        entries: entries.drain(offset..end).collect(),
        offset,
        limit: limit.max(1),
        total,
        has_more: end < total,
    })
}

fn parse_worktrees(output: &str) -> Vec<GitWorktreeDto> {
    let mut result = Vec::new();
    let mut current: Option<GitWorktreeDto> = None;
    for line in output.lines() {
        if line.is_empty() {
            if let Some(item) = current.take() {
                result.push(item);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(item) = current.take() {
                result.push(item);
            }
            current = Some(GitWorktreeDto {
                display_path: Some(path.to_string()),
                path: path.to_string(),
                head_sha: None,
                branch: None,
                is_main: result.is_empty(),
                is_locked: false,
                is_prunable: false,
            });
        } else if let Some(item) = current.as_mut() {
            if let Some(value) = line.strip_prefix("HEAD ") {
                item.head_sha = Some(value.to_string());
            }
            if let Some(value) = line.strip_prefix("branch refs/heads/") {
                item.branch = Some(value.to_string());
            }
            if line == "locked" {
                item.is_locked = true;
            }
            if line == "prunable" {
                item.is_prunable = true;
            }
        }
    }
    if let Some(item) = current {
        result.push(item);
    }
    result
}
