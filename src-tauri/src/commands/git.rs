use serde::Serialize;
use tauri::Emitter;
use tauri::State;

use crate::{
    git::{repo, worktree},
    models::{
        FileTreeEntryDto, FileTreePageDto, GitBranchPageDto, GitBranchScopeDto, GitCommitPageDto,
        GitCompareSourceDto, GitDiffPreviewDto, GitFileCompareDto, GitInitRepoStatusDto,
        GitRemoteDto, GitStashDto, GitStatusDto, GitWorktreeDto,
    },
    ssh::{
        remote_git,
        runtime::{
            remote_repo_marker, remote_worktree_marker, resolve_workspace_target,
            validate_remote_relative_path, workspace_id_from_repo_marker,
            worktree_path_from_repo_marker, WorkspaceTarget, REMOTE_REPO_PREFIX,
        },
    },
    state::AppState,
};

struct RemoteGitTarget {
    target: WorkspaceTarget,
    root: String,
}

async fn remote_target(
    state: &AppState,
    repo_path: &str,
) -> Result<Option<RemoteGitTarget>, String> {
    let Some(workspace_id) = workspace_id_from_repo_marker(repo_path) else {
        return Ok(None);
    };
    let db = state.db.clone();
    let workspace_id = workspace_id.to_string();
    let target = tokio::task::spawn_blocking(move || resolve_workspace_target(&db, &workspace_id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(err_to_string)?;
    if !target.is_remote() {
        return Err("远端仓库标识未绑定远端工作区".to_string());
    }
    let root = if let Some(worktree_path) =
        worktree_path_from_repo_marker(repo_path).map_err(err_to_string)?
    {
        if worktree_path.relative_path.is_some() {
            return Err("Git 仓库路径不能指向工作树内的子目录".to_string());
        }
        let connection = target.remote_connection().map_err(err_to_string)?;
        let registered = remote_git::worktrees(connection, &target.workspace.root_path)
            .await
            .map_err(err_to_string)?
            .into_iter()
            .any(|worktree| worktree.path == worktree_path.root_path);
        if !registered {
            return Err("远端工作树没有登记在当前项目中".to_string());
        }
        worktree_path.root_path
    } else {
        target.workspace.root_path.clone()
    };
    Ok(Some(RemoteGitTarget { target, root }))
}

fn remote_parts(
    target: &RemoteGitTarget,
) -> Result<(&crate::db::ssh_connections::SshConnectionRecord, &str), String> {
    Ok((
        target.target.remote_connection().map_err(err_to_string)?,
        &target.root,
    ))
}

#[tauri::command]
pub async fn get_git_status(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<GitStatusDto, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::status(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::get_git_status(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_file_diff(
    state: State<'_, AppState>,
    repo_path: String,
    file_path: String,
    staged: bool,
) -> Result<GitDiffPreviewDto, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::diff(connection, root, Some(&file_path), staged)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::get_file_diff(&repo_path, &file_path, staged).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_git_file_compare(
    state: State<'_, AppState>,
    repo_path: String,
    file_path: String,
    source: String,
) -> Result<GitFileCompareDto, String> {
    let compare_source = GitCompareSourceDto::from_str(&source);
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        let preview = remote_git::diff(
            connection,
            root,
            Some(&file_path),
            matches!(compare_source, GitCompareSourceDto::Staged),
        )
        .await
        .map_err(err_to_string)?;
        return Ok(GitFileCompareDto {
            source: compare_source,
            base_content: String::new(),
            modified_content: preview.content,
            base_label: "远端 Git".to_string(),
            modified_label: "远端工作树".to_string(),
            change_type: crate::models::GitChangeTypeDto::Modified,
            has_staged_changes: false,
            has_unstaged_changes: true,
            is_binary: false,
            is_editable: Some(false),
            fallback_reason: Some("远端文件差异预览为只读".to_string()),
        });
    }
    tokio::task::spawn_blocking(move || {
        repo::get_git_file_compare(&repo_path, &file_path, compare_source).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn stage_files(
    state: State<'_, AppState>,
    repo_path: String,
    files: Vec<String>,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::stage(connection, root, &files)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::stage_files(&repo_path, &files).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn unstage_files(
    state: State<'_, AppState>,
    repo_path: String,
    files: Vec<String>,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::unstage(connection, root, &files)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::unstage_files(&repo_path, &files).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn discard_files(
    state: State<'_, AppState>,
    repo_path: String,
    files: Vec<String>,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::discard(connection, root, &files)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::discard_files(&repo_path, &files).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn commit(
    state: State<'_, AppState>,
    repo_path: String,
    message: String,
) -> Result<String, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::commit(connection, root, &message)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::commit(&repo_path, &message).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn soft_reset_last_commit(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::soft_reset_last_commit(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::soft_reset_last_commit(&repo_path).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn fetch_git(state: State<'_, AppState>, repo_path: String) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::fetch(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::fetch_repo(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn pull_git(state: State<'_, AppState>, repo_path: String) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::pull(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::pull_repo(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn push_git(state: State<'_, AppState>, repo_path: String) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::push(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::push_repo(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_git_branches(
    state: State<'_, AppState>,
    repo_path: String,
    scope: String,
    offset: Option<usize>,
    limit: Option<usize>,
    search: Option<String>,
) -> Result<GitBranchPageDto, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(200);
    let scope = GitBranchScopeDto::from_str(&scope);

    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::branches(connection, root, scope, offset, limit, search.as_deref())
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::list_git_branches(&repo_path, scope, offset, limit, search.as_deref())
            .map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn checkout_git_branch(
    state: State<'_, AppState>,
    repo_path: String,
    branch_name: String,
    is_remote: bool,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::checkout_branch(connection, root, &branch_name, is_remote)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::checkout_git_branch(&repo_path, &branch_name, is_remote).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn create_git_branch(
    state: State<'_, AppState>,
    repo_path: String,
    branch_name: String,
    from_ref: Option<String>,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::create_branch(connection, root, &branch_name, from_ref.as_deref())
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::create_git_branch(&repo_path, &branch_name, from_ref.as_deref())
            .map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn rename_git_branch(
    state: State<'_, AppState>,
    repo_path: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::rename_branch(connection, root, &old_name, &new_name)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::rename_git_branch(&repo_path, &old_name, &new_name).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn delete_git_branch(
    state: State<'_, AppState>,
    repo_path: String,
    branch_name: String,
    force: bool,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::delete_branch(connection, root, &branch_name, force)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::delete_git_branch(&repo_path, &branch_name, force).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_git_commits(
    state: State<'_, AppState>,
    repo_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<GitCommitPageDto, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100);

    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::commits(connection, root, offset, limit)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::list_git_commits(&repo_path, offset, limit).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_git_stashes(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<Vec<GitStashDto>, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::stashes(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::list_git_stashes(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn push_git_stash(
    state: State<'_, AppState>,
    repo_path: String,
    message: Option<String>,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::stash_push(connection, root, message.as_deref())
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::push_git_stash(&repo_path, message.as_deref()).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn apply_git_stash(
    state: State<'_, AppState>,
    repo_path: String,
    stash_index: usize,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::stash_apply(connection, root, stash_index, false)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::apply_git_stash(&repo_path, stash_index).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn pop_git_stash(
    state: State<'_, AppState>,
    repo_path: String,
    stash_index: usize,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::stash_apply(connection, root, stash_index, true)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::pop_git_stash(&repo_path, stash_index).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_commit_diff(
    state: State<'_, AppState>,
    repo_path: String,
    commit_hash: String,
) -> Result<GitDiffPreviewDto, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::commit_diff(connection, root, &commit_hash)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::get_commit_diff(&repo_path, &commit_hash).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_file_tree(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<Vec<FileTreeEntryDto>, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return Ok(remote_git::file_tree(connection, root, 0, 10_000)
            .await
            .map_err(err_to_string)?
            .entries);
    }
    let cache = state.file_tree_cache.clone();
    tokio::task::spawn_blocking(move || {
        repo::get_file_tree(&repo_path, &cache).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_file_tree_page(
    state: State<'_, AppState>,
    repo_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<FileTreePageDto, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(2000);
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::file_tree(connection, root, offset, limit)
            .await
            .map_err(err_to_string);
    }
    let cache = state.file_tree_cache.clone();
    tokio::task::spawn_blocking(move || {
        repo::get_file_tree_page(&repo_path, offset, limit, &cache).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoChangedEvent {
    repo_path: String,
}

#[tauri::command]
pub async fn watch_git_repo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<(), String> {
    if workspace_id_from_repo_marker(&repo_path).is_some() {
        return Ok(());
    }
    let cache = state.file_tree_cache.clone();
    let callback = std::sync::Arc::new(move |changed_repo_path: String| {
        cache.invalidate_containing_path(&changed_repo_path);
        let payload = GitRepoChangedEvent {
            repo_path: changed_repo_path,
        };
        let _ = app.emit("git-repo-changed", payload);
    });

    state
        .git_watchers
        .watch_repo(repo_path, callback)
        .await
        .map_err(err_to_string)
}

// ── Git Worktrees ──────────────────────────────────────────────

#[tauri::command]
pub async fn add_git_worktree(
    state: State<'_, AppState>,
    repo_path: String,
    worktree_path: String,
    branch_name: String,
    base_ref: Option<String>,
) -> Result<GitWorktreeDto, String> {
    // Validate branch name
    if branch_name.contains("..")
        || branch_name.starts_with('/')
        || branch_name.ends_with('/')
        || branch_name.contains(' ')
        || branch_name.is_empty()
    {
        return Err(format!("invalid branch name: {branch_name}"));
    }
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        let workspace_id = target.target.workspace.id.as_str();
        let remote_path = remote_worktree_path(workspace_id, root, &worktree_path)?;
        let created = remote_git::add_worktree(
            connection,
            root,
            &remote_path,
            &branch_name,
            base_ref.as_deref(),
        )
        .await
        .map_err(err_to_string)?;
        return Ok(GitWorktreeDto {
            path: remote_worktree_marker(workspace_id, &created.path),
            display_path: Some(created.path.clone()),
            ..created
        });
    }

    tokio::task::spawn_blocking(move || {
        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&worktree_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create worktree parent directory: {e}"))?;
        }

        let created = worktree::add_worktree(
            &repo_path,
            &worktree_path,
            &branch_name,
            base_ref.as_deref(),
        )
        .map_err(err_to_string)?;

        // Keep .auracoder/ ignored, but don't fail the command after successful creation.
        if let Err(error) = ensure_gitignore_entry(&repo_path, ".auracoder/") {
            eprintln!(
                "warning: failed to ensure .auracoder/ in .gitignore for '{}': {}",
                repo_path, error
            );
        }

        Ok(created)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_git_worktrees(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<Vec<GitWorktreeDto>, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        let workspace_id = target.target.workspace.id.as_str();
        return remote_git::worktrees(connection, root)
            .await
            .map_err(err_to_string)?
            .into_iter()
            .map(|worktree| {
                if worktree.is_main {
                    return Ok(GitWorktreeDto {
                        path: remote_repo_marker(workspace_id),
                        display_path: Some(worktree.path.clone()),
                        ..worktree
                    });
                }
                if !worktree.path.starts_with('/') || worktree.path.contains('\0') {
                    return Err("远端工作树路径无效".to_string());
                }
                Ok(GitWorktreeDto {
                    path: remote_worktree_marker(workspace_id, &worktree.path),
                    display_path: Some(worktree.path.clone()),
                    ..worktree
                })
            })
            .collect();
    }
    tokio::task::spawn_blocking(move || worktree::list_worktrees(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn remove_git_worktree(
    state: State<'_, AppState>,
    repo_path: String,
    worktree_path: String,
    force: bool,
    branch_name: Option<String>,
    delete_branch: bool,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        if workspace_id_from_repo_marker(&worktree_path)
            != Some(target.target.workspace.id.as_str())
        {
            return Err("远端工作树不属于当前项目".to_string());
        }
        let remote_path = worktree_path_from_repo_marker(&worktree_path)
            .map_err(err_to_string)?
            .ok_or_else(|| "远端工作树必须使用程序登记的路径".to_string())?;
        if remote_path.relative_path.is_some() {
            return Err("移除工作树时不能指定工作树内的子目录".to_string());
        }
        let registered = remote_git::worktrees(connection, root)
            .await
            .map_err(err_to_string)?
            .into_iter()
            .any(|worktree| worktree.path == remote_path.root_path);
        if !registered {
            return Err("远端工作树没有登记在当前项目中".to_string());
        }
        let result = remote_git::remove_worktree(connection, root, &remote_path.root_path, force)
            .await
            .map_err(err_to_string);
        if result.is_ok() && delete_branch {
            if let Some(branch_name) = branch_name.as_deref() {
                remote_git::delete_branch(connection, root, branch_name, force)
                    .await
                    .map_err(err_to_string)?;
            }
        }
        return result;
    }
    tokio::task::spawn_blocking(move || {
        worktree::remove_worktree(
            &repo_path,
            &worktree_path,
            force,
            branch_name.as_deref(),
            delete_branch,
        )
        .map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn prune_git_worktrees(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::prune_worktrees(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        worktree::prune_worktrees(&repo_path).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Ensures a pattern exists in the repo's .gitignore file.
fn ensure_gitignore_entry(repo_path: &str, pattern: &str) -> Result<(), String> {
    let gitignore_path = std::path::Path::new(repo_path).join(".gitignore");

    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)
            .map_err(|e| format!("read .gitignore: {e}"))?;
        // Check if pattern is already present (as a whole line)
        if content.lines().any(|line| line.trim() == pattern) {
            return Ok(());
        }
        // Append with newline separator if file doesn't end with one
        let prefix = if content.ends_with('\n') { "" } else { "\n" };
        std::fs::write(&gitignore_path, format!("{content}{prefix}{pattern}\n"))
            .map_err(|e| format!("write .gitignore: {e}"))?;
    } else {
        std::fs::write(&gitignore_path, format!("{pattern}\n"))
            .map_err(|e| format!("create .gitignore: {e}"))?;
    }

    Ok(())
}

// ── Init & Remote Management ──────────────────────────────────

#[tauri::command]
pub async fn init_git_repo(
    state: State<'_, AppState>,
    repo_path: String,
    validate_only: Option<bool>,
) -> Result<GitInitRepoStatusDto, String> {
    if repo_path.is_empty() {
        return Err("repo_path is required".to_string());
    }
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        if validate_only.unwrap_or(false) {
            let can_initialize = remote_git::discover(
                target.target.remote_connection().map_err(err_to_string)?,
                &target.root,
            )
            .await
            .map_err(err_to_string)?
            .is_none();
            return Ok(GitInitRepoStatusDto {
                can_initialize,
                blocking_repo_path: None,
            });
        }
        remote_git::init(
            target.target.remote_connection().map_err(err_to_string)?,
            &target.root,
        )
        .await
        .map_err(err_to_string)?;
        return Ok(GitInitRepoStatusDto {
            can_initialize: false,
            blocking_repo_path: None,
        });
    }
    if !std::path::Path::new(&repo_path).is_dir() {
        return Err(format!(
            "path does not exist or is not a directory: {repo_path}"
        ));
    }
    let validate_only = validate_only.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        repo::init_repo(&repo_path, validate_only).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_git_remotes(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<Vec<GitRemoteDto>, String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::remotes(connection, root)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || repo::list_remotes(&repo_path).map_err(err_to_string))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn add_git_remote(
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
    url: String,
) -> Result<(), String> {
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(format!("invalid remote name: {name}"));
    }
    if url.is_empty() {
        return Err("url is required".to_string());
    }
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::add_remote(connection, root, &name, &url)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::add_remote(&repo_path, &name, &url).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn remove_git_remote(
    state: State<'_, AppState>,
    repo_path: String,
    name: String,
) -> Result<(), String> {
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::remove_remote(connection, root, &name)
            .await
            .map_err(err_to_string);
    }
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    tokio::task::spawn_blocking(move || {
        repo::remove_remote(&repo_path, &name).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn rename_git_remote(
    state: State<'_, AppState>,
    repo_path: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    if new_name.is_empty() || new_name.contains(char::is_whitespace) {
        return Err(format!("invalid remote name: {new_name}"));
    }
    if let Some(target) = remote_target(state.inner(), &repo_path).await? {
        let (connection, root) = remote_parts(&target)?;
        return remote_git::rename_remote(connection, root, &old_name, &new_name)
            .await
            .map_err(err_to_string);
    }
    tokio::task::spawn_blocking(move || {
        repo::rename_remote(&repo_path, &old_name, &new_name).map_err(err_to_string)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn remote_worktree_path(workspace_id: &str, root: &str, path: &str) -> Result<String, String> {
    if path.contains('\0') {
        return Err("远端工作树路径无效".to_string());
    }
    if path.starts_with('/') {
        return Ok(path.to_string());
    }
    if path.starts_with(REMOTE_REPO_PREFIX) {
        let marker = remote_repo_marker(workspace_id);
        if path.starts_with(&format!("{marker}/worktree/")) {
            let worktree = worktree_path_from_repo_marker(path)
                .map_err(err_to_string)?
                .ok_or_else(|| "远端工作树创建路径无效".to_string())?;
            if worktree.root_path != root {
                return Err("远端工作树创建路径不属于当前仓库".to_string());
            }
            let relative = worktree
                .relative_path
                .ok_or_else(|| "远端工作树创建路径不能为空".to_string())?;
            return Ok(format!("{}/{}", root.trim_end_matches('/'), relative));
        }
        let relative = path
            .strip_prefix(&format!("{marker}/"))
            .ok_or_else(|| "远端工作树路径不属于当前项目".to_string())?;
        validate_remote_relative_path(relative, false).map_err(err_to_string)?;
        return Ok(format!("{}/{}", root.trim_end_matches('/'), relative));
    }
    validate_remote_relative_path(path, false).map_err(err_to_string)?;
    Ok(format!("{}/{}", root.trim_end_matches('/'), path))
}

#[cfg(test)]
mod tests {
    use super::remote_worktree_path;

    #[test]
    fn remote_worktree_path_accepts_external_creation_path() {
        assert_eq!(
            remote_worktree_path("ws-1", "/srv/repo", "/tmp/feature").unwrap(),
            "/tmp/feature"
        );
    }

    #[test]
    fn remote_worktree_path_accepts_project_relative_creation_path() {
        assert_eq!(
            remote_worktree_path("ws-1", "/srv/repo", ".auracoder/worktrees/feature").unwrap(),
            "/srv/repo/.auracoder/worktrees/feature"
        );
        assert_eq!(
            remote_worktree_path("ws-1", "/srv/repo", "custom/feature").unwrap(),
            "/srv/repo/custom/feature"
        );
        assert_eq!(
            remote_worktree_path(
                "ws-1",
                "/srv/repo",
                "ssh://auracoder/ws-1/.auracoder/worktrees/feature"
            )
            .unwrap(),
            "/srv/repo/.auracoder/worktrees/feature"
        );
        assert!(remote_worktree_path(
            "ws-1",
            "/srv/repo",
            "ssh://auracoder/ws-2/.auracoder/worktrees/feature"
        )
        .is_err());
    }

    #[test]
    fn remote_worktree_path_accepts_relative_path_under_registered_worktree_marker() {
        let marker = crate::ssh::runtime::remote_worktree_marker("ws-1", "/srv/worktree-a");
        assert_eq!(
            remote_worktree_path(
                "ws-1",
                "/srv/worktree-a",
                &format!("{marker}/.auracoder/worktrees/feature")
            )
            .unwrap(),
            "/srv/worktree-a/.auracoder/worktrees/feature"
        );
    }
}

fn err_to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
