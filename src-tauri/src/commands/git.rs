//use git2::Repository;
//use serde::Serialize;
//use tauri::Emitter;
//use tauri::State;
//
//use crate::{
//    git::{repo, worktree},
//    models::{
//        FileTreeEntryDto, FileTreePageDto, GitBranchPageDto, GitBranchScopeDto, GitCommitPageDto,
//        GitCompareSourceDto, GitDiffPreviewDto, GitFileCompareDto, GitInitRepoStatusDto,
//        GitRemoteDto, GitStashDto, GitStatusDto, GitWorktreeDto, WorkspaceGitContextDto,
//    },
//    ssh::{
//        remote_git,
//        runtime::{resolve_workspace_target, validate_remote_relative_path, WorkspaceTarget},
//    },
//    state::AppState,
//};
//
//struct RemoteGitTarget {
//    target: WorkspaceTarget,
//    root: String,
//}
//
//enum WorkspaceGitTarget {
//    Local { root_path: String },
//    Remote { target: RemoteGitTarget },
//}
//
///// 按工作区主键解析唯一项目 Git 目标，并按要求校验基础仓库。
//async fn resolve_workspace_git_target(
//    state: &AppState,
//    workspace_id: &str,
//    require_repository: bool,
//) -> Result<WorkspaceGitTarget, String> {
//    let db = state.db.clone();
//    let id = workspace_id.to_string();
//    let workspace =
//        tokio::task::spawn_blocking(move || crate::db::workspaces::find_workspace_by_id(&db, &id))
//            .await
//            .map_err(|error| error.to_string())?
//            .map_err(err_to_string)?
//            .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
//    if workspace.location_kind == "local" {
//        if require_repository
//            && (!std::path::Path::new(&workspace.root_path)
//                .join(".git")
//                .exists()
//                || git2::Repository::open(&workspace.root_path).is_err())
//        {
//            return Err("workspace root is not a Git repository".to_string());
//        }
//        return Ok(WorkspaceGitTarget::Local {
//            root_path: workspace.root_path,
//        });
//    }
//    let db = state.db.clone();
//    let workspace_id = workspace.id.clone();
//    let target = tokio::task::spawn_blocking(move || resolve_workspace_target(&db, &workspace_id))
//        .await
//        .map_err(|error| error.to_string())?
//        .map_err(err_to_string)?;
//    if !target.is_remote() {
//        return Err("SSH workspace target is unavailable".to_string());
//    }
//    let target = RemoteGitTarget {
//        root: target.workspace.root_path.clone(),
//        target,
//    };
//    if require_repository {
//        let connection = target.target.remote_connection().map_err(err_to_string)?;
//        if remote_git::discover(connection, &target.root)
//            .await
//            .map_err(err_to_string)?
//            .is_none()
//        {
//            return Err("workspace root is not a Git repository".to_string());
//        }
//    }
//    Ok(WorkspaceGitTarget::Remote { target })
//}
//
///// 读取工作区根目录自身的 Git 上下文，不递归检查子目录仓库。
//#[tauri::command]
//pub async fn get_workspace_git_context(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<WorkspaceGitContextDto, String> {
//    let db = state.db.clone();
//    let workspace_id_for_db = workspace_id.clone();
//    let workspace = tokio::task::spawn_blocking(move || {
//        crate::db::workspaces::find_workspace_by_id(&db, &workspace_id_for_db)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//    .map_err(err_to_string)?
//    .ok_or_else(|| format!("workspace not found: {workspace_id}"))?;
//    if workspace.location_kind == "ssh" {
//        let target = resolve_workspace_target(&state.db, &workspace_id).map_err(err_to_string)?;
//        let connection = target.remote_connection().map_err(err_to_string)?;
//        let Some(info) = remote_git::discover(connection, &workspace.root_path)
//            .await
//            .map_err(err_to_string)?
//        else {
//            return Ok(WorkspaceGitContextDto::NotRepository { workspace_id });
//        };
//        return Ok(WorkspaceGitContextDto::Repository {
//            workspace_id,
//            root_path: workspace.root_path,
//            name: info.name,
//            default_branch: Some(info.default_branch),
//        });
//    }
//    Ok(local_workspace_git_context(
//        &workspace_id,
//        &workspace.name,
//        &workspace.root_path,
//    ))
//}
//
///// 读取本地工作区根目录的 Git 上下文，只识别根目录自身的仓库状态。
//fn local_workspace_git_context(
//    workspace_id: &str,
//    workspace_name: &str,
//    root_path: &str,
//) -> WorkspaceGitContextDto {
//    let root = std::path::Path::new(root_path);
//    let Ok(repository) = (if root.join(".git").exists() {
//        Repository::open(root)
//    } else {
//        Err(git2::Error::from_str(
//            "workspace root is not a Git repository",
//        ))
//    }) else {
//        return WorkspaceGitContextDto::NotRepository {
//            workspace_id: workspace_id.to_string(),
//        };
//    };
//
//    let name = root
//        .file_name()
//        .map(|value| value.to_string_lossy().to_string())
//        .unwrap_or_else(|| workspace_name.to_string());
//    let default_branch = repository
//        .head()
//        .ok()
//        .and_then(|head| head.shorthand().map(str::to_string));
//    WorkspaceGitContextDto::Repository {
//        workspace_id: workspace_id.to_string(),
//        root_path: root_path.to_string(),
//        name,
//        default_branch,
//    }
//}
//
//async fn remote_target(
//    state: &AppState,
//    workspace_id: &str,
//) -> Result<Option<RemoteGitTarget>, String> {
//    let db = state.db.clone();
//    let workspace_id = workspace_id.to_string();
//    let target = tokio::task::spawn_blocking(move || resolve_workspace_target(&db, &workspace_id))
//        .await
//        .map_err(|error| error.to_string())?
//        .map_err(err_to_string)?;
//    if !target.is_remote() {
//        return Ok(None);
//    }
//    let root = target.workspace.root_path.clone();
//    Ok(Some(RemoteGitTarget { target, root }))
//}
//
///// 根据工作区主键解析项目根目录，禁止调用方直接指定基础仓库路径。
//async fn resolve_workspace_root(state: &AppState, workspace_id: &str) -> Result<String, String> {
//    match resolve_workspace_git_target(state, workspace_id, true).await? {
//        WorkspaceGitTarget::Local { root_path } => Ok(root_path),
//        WorkspaceGitTarget::Remote { target } => Ok(target.root),
//    }
//}
//
///// 解析允许执行初始化的工作区根目录，初始化前允许根目录尚未存在 Git 仓库。
//async fn resolve_workspace_root_unchecked(
//    state: &AppState,
//    workspace_id: &str,
//) -> Result<String, String> {
//    let db = state.db.clone();
//    let workspace_id = workspace_id.to_string();
//    let workspace_id_for_db = workspace_id.clone();
//    tokio::task::spawn_blocking(move || {
//        crate::db::workspaces::find_workspace_by_id(&db, &workspace_id_for_db)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//    .map_err(err_to_string)?
//    .map(|workspace| workspace.root_path)
//    .ok_or_else(|| format!("workspace not found: {workspace_id}"))
//}
//
//fn remote_parts(
//    target: &RemoteGitTarget,
//) -> Result<(&crate::db::ssh_connections::SshConnectionRecord, &str), String> {
//    Ok((
//        target.target.remote_connection().map_err(err_to_string)?,
//        &target.root,
//    ))
//}
//
//#[tauri::command]
//pub async fn get_git_status(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<GitStatusDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::status(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::get_git_status(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn get_file_diff(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    file_path: String,
//    staged: bool,
//) -> Result<GitDiffPreviewDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::diff(connection, root, Some(&file_path), staged)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::get_file_diff(&repo_path, &file_path, staged).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn get_git_file_compare(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    file_path: String,
//    source: String,
//) -> Result<GitFileCompareDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    let compare_source = GitCompareSourceDto::from_str(&source);
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        let preview = remote_git::diff(
//            connection,
//            root,
//            Some(&file_path),
//            matches!(compare_source, GitCompareSourceDto::Staged),
//        )
//        .await
//        .map_err(err_to_string)?;
//        return Ok(GitFileCompareDto {
//            source: compare_source,
//            base_content: String::new(),
//            modified_content: preview.content,
//            base_label: "远端 Git".to_string(),
//            modified_label: "远端工作树".to_string(),
//            change_type: crate::models::GitChangeTypeDto::Modified,
//            has_staged_changes: false,
//            has_unstaged_changes: true,
//            is_binary: false,
//            is_editable: Some(false),
//            fallback_reason: Some("远端文件差异预览为只读".to_string()),
//        });
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::get_git_file_compare(&repo_path, &file_path, compare_source).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn stage_files(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    files: Vec<String>,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::stage(connection, root, &files)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::stage_files(&repo_path, &files).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn unstage_files(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    files: Vec<String>,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::unstage(connection, root, &files)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::unstage_files(&repo_path, &files).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn discard_files(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    files: Vec<String>,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::discard(connection, root, &files)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::discard_files(&repo_path, &files).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn commit(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    message: String,
//) -> Result<String, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::commit(connection, root, &message)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::commit(&repo_path, &message).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn soft_reset_last_commit(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::soft_reset_last_commit(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::soft_reset_last_commit(&repo_path).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn fetch_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::fetch(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::fetch_repo(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn pull_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::pull(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::pull_repo(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn push_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::push(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::push_repo(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn list_git_branches(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    scope: String,
//    offset: Option<usize>,
//    limit: Option<usize>,
//    search: Option<String>,
//) -> Result<GitBranchPageDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    let offset = offset.unwrap_or(0);
//    let limit = limit.unwrap_or(200);
//    let scope = GitBranchScopeDto::from_str(&scope);
//
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::branches(connection, root, scope, offset, limit, search.as_deref())
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::list_git_branches(&repo_path, scope, offset, limit, search.as_deref())
//            .map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn checkout_git_branch(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    branch_name: String,
//    is_remote: bool,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::checkout_branch(connection, root, &branch_name, is_remote)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::checkout_git_branch(&repo_path, &branch_name, is_remote).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn create_git_branch(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    branch_name: String,
//    from_ref: Option<String>,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::create_branch(connection, root, &branch_name, from_ref.as_deref())
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::create_git_branch(&repo_path, &branch_name, from_ref.as_deref())
//            .map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn rename_git_branch(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    old_name: String,
//    new_name: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::rename_branch(connection, root, &old_name, &new_name)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::rename_git_branch(&repo_path, &old_name, &new_name).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn delete_git_branch(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    branch_name: String,
//    force: bool,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::delete_branch(connection, root, &branch_name, force)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::delete_git_branch(&repo_path, &branch_name, force).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn list_git_commits(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    offset: Option<usize>,
//    limit: Option<usize>,
//) -> Result<GitCommitPageDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    let offset = offset.unwrap_or(0);
//    let limit = limit.unwrap_or(100);
//
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::commits(connection, root, offset, limit)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::list_git_commits(&repo_path, offset, limit).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn list_git_stashes(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<Vec<GitStashDto>, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::stashes(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::list_git_stashes(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn push_git_stash(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    message: Option<String>,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::stash_push(connection, root, message.as_deref())
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::push_git_stash(&repo_path, message.as_deref()).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn apply_git_stash(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    stash_index: usize,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::stash_apply(connection, root, stash_index, false)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::apply_git_stash(&repo_path, stash_index).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn pop_git_stash(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    stash_index: usize,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::stash_apply(connection, root, stash_index, true)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::pop_git_stash(&repo_path, stash_index).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn get_commit_diff(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    commit_hash: String,
//) -> Result<GitDiffPreviewDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::commit_diff(connection, root, &commit_hash)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::get_commit_diff(&repo_path, &commit_hash).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn get_file_tree(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<Vec<FileTreeEntryDto>, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return Ok(remote_git::file_tree(connection, root, 0, 10_000)
//            .await
//            .map_err(err_to_string)?
//            .entries);
//    }
//    let cache = state.file_tree_cache.clone();
//    tokio::task::spawn_blocking(move || {
//        repo::get_file_tree(&repo_path, &cache).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn get_file_tree_page(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    offset: Option<usize>,
//    limit: Option<usize>,
//) -> Result<FileTreePageDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    let offset = offset.unwrap_or(0);
//    let limit = limit.unwrap_or(2000);
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::file_tree(connection, root, offset, limit)
//            .await
//            .map_err(err_to_string);
//    }
//    let cache = state.file_tree_cache.clone();
//    tokio::task::spawn_blocking(move || {
//        repo::get_file_tree_page(&repo_path, offset, limit, &cache).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[derive(Debug, Clone, Serialize)]
//#[serde(rename_all = "camelCase")]
//struct GitRepoChangedEvent {
//    workspace_id: String,
//}
//
//#[tauri::command]
//pub async fn watch_git_repo(
//    app: tauri::AppHandle,
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    let cache = state.file_tree_cache.clone();
//    let workspace_id_for_event = workspace_id.clone();
//    let callback = std::sync::Arc::new(move |changed_repo_path: String| {
//        cache.invalidate_containing_path(&changed_repo_path);
//        let payload = GitRepoChangedEvent {
//            workspace_id: workspace_id_for_event.clone(),
//        };
//        let _ = app.emit("git-repo-changed", payload);
//    });
//
//    state
//        .git_watchers
//        .watch_repo(repo_path, callback)
//        .await
//        .map_err(err_to_string)
//}
//
//// ── Git Worktrees ──────────────────────────────────────────────
//
//#[tauri::command]
//pub async fn add_git_worktree(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    worktree_path: String,
//    branch_name: String,
//    base_ref: Option<String>,
//) -> Result<GitWorktreeDto, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    // Validate branch name
//    if branch_name.contains("..")
//        || branch_name.starts_with('/')
//        || branch_name.ends_with('/')
//        || branch_name.contains(' ')
//        || branch_name.is_empty()
//    {
//        return Err(format!("invalid branch name: {branch_name}"));
//    }
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        let remote_path = remote_worktree_path(root, &worktree_path)?;
//        let created = remote_git::add_worktree(
//            connection,
//            root,
//            &remote_path,
//            &branch_name,
//            base_ref.as_deref(),
//        )
//        .await
//        .map_err(err_to_string)?;
//        return Ok(GitWorktreeDto {
//            path: created.path.clone(),
//            display_path: Some(created.path.clone()),
//            ..created
//        });
//    }
//
//    tokio::task::spawn_blocking(move || {
//        // Ensure parent directory exists
//        if let Some(parent) = std::path::Path::new(&worktree_path).parent() {
//            std::fs::create_dir_all(parent)
//                .map_err(|e| format!("failed to create worktree parent directory: {e}"))?;
//        }
//
//        let created = worktree::add_worktree(
//            &repo_path,
//            &worktree_path,
//            &branch_name,
//            base_ref.as_deref(),
//        )
//        .map_err(err_to_string)?;
//
//        // Keep .auracoder/ ignored, but don't fail the command after successful creation.
//        if let Err(error) = ensure_gitignore_entry(&repo_path, ".auracoder/") {
//            log::warn!(
//                "warning: failed to ensure .auracoder/ in .gitignore for '{}': {}",
//                repo_path, error
//            );
//        }
//
//        Ok(created)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn list_git_worktrees(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<Vec<GitWorktreeDto>, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        let workspace_root = target.target.workspace.root_path.clone();
//        return remote_git::worktrees(connection, root)
//            .await
//            .map_err(err_to_string)?
//            .into_iter()
//            .map(|worktree| {
//                if worktree.is_main {
//                    return Ok(GitWorktreeDto {
//                        path: workspace_root.clone(),
//                        display_path: Some(worktree.path.clone()),
//                        ..worktree
//                    });
//                }
//                if !worktree.path.starts_with('/') || worktree.path.contains('\0') {
//                    return Err("远端工作树路径无效".to_string());
//                }
//                Ok(GitWorktreeDto {
//                    path: worktree.path.clone(),
//                    display_path: Some(worktree.path.clone()),
//                    ..worktree
//                })
//            })
//            .collect();
//    }
//    tokio::task::spawn_blocking(move || worktree::list_worktrees(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn remove_git_worktree(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    worktree_path: String,
//    force: bool,
//    branch_name: Option<String>,
//    delete_branch: bool,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        if !worktree_path.starts_with('/') || worktree_path.contains('\0') {
//            return Err("远端工作树路径必须是绝对路径".to_string());
//        }
//        let registered = remote_git::worktrees(connection, root)
//            .await
//            .map_err(err_to_string)?
//            .into_iter()
//            .any(|worktree| worktree.path == worktree_path);
//        if !registered {
//            return Err("远端工作树没有登记在当前项目中".to_string());
//        }
//        let result = remote_git::remove_worktree(connection, root, &worktree_path, force)
//            .await
//            .map_err(err_to_string);
//        if result.is_ok() && delete_branch {
//            if let Some(branch_name) = branch_name.as_deref() {
//                remote_git::delete_branch(connection, root, branch_name, force)
//                    .await
//                    .map_err(err_to_string)?;
//            }
//        }
//        return result;
//    }
//    tokio::task::spawn_blocking(move || {
//        worktree::remove_worktree(
//            &repo_path,
//            &worktree_path,
//            force,
//            branch_name.as_deref(),
//            delete_branch,
//        )
//        .map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn prune_git_worktrees(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::prune_worktrees(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        worktree::prune_worktrees(&repo_path).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
///// Ensures a pattern exists in the repo's .gitignore file.
//fn ensure_gitignore_entry(repo_path: &str, pattern: &str) -> Result<(), String> {
//    let gitignore_path = std::path::Path::new(repo_path).join(".gitignore");
//
//    if gitignore_path.exists() {
//        let content = std::fs::read_to_string(&gitignore_path)
//            .map_err(|e| format!("read .gitignore: {e}"))?;
//        // Check if pattern is already present (as a whole line)
//        if content.lines().any(|line| line.trim() == pattern) {
//            return Ok(());
//        }
//        // Append with newline separator if file doesn't end with one
//        let prefix = if content.ends_with('\n') { "" } else { "\n" };
//        std::fs::write(&gitignore_path, format!("{content}{prefix}{pattern}\n"))
//            .map_err(|e| format!("write .gitignore: {e}"))?;
//    } else {
//        std::fs::write(&gitignore_path, format!("{pattern}\n"))
//            .map_err(|e| format!("create .gitignore: {e}"))?;
//    }
//
//    Ok(())
//}
//
//// ── Init & Remote Management ──────────────────────────────────
//
//#[tauri::command]
//pub async fn init_git_repo(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    validate_only: Option<bool>,
//) -> Result<GitInitRepoStatusDto, String> {
//    let repo_path = resolve_workspace_root_unchecked(state.inner(), &workspace_id).await?;
//    if repo_path.is_empty() {
//        return Err("workspace root is required".to_string());
//    }
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        if validate_only.unwrap_or(false) {
//            let can_initialize = remote_git::discover(
//                target.target.remote_connection().map_err(err_to_string)?,
//                &target.root,
//            )
//            .await
//            .map_err(err_to_string)?
//            .is_none();
//            return Ok(GitInitRepoStatusDto {
//                can_initialize,
//                blocking_root_path: None,
//            });
//        }
//        remote_git::init(
//            target.target.remote_connection().map_err(err_to_string)?,
//            &target.root,
//        )
//        .await
//        .map_err(err_to_string)?;
//        return Ok(GitInitRepoStatusDto {
//            can_initialize: false,
//            blocking_root_path: None,
//        });
//    }
//    if !std::path::Path::new(&repo_path).is_dir() {
//        return Err(format!(
//            "path does not exist or is not a directory: {repo_path}"
//        ));
//    }
//    let validate_only = validate_only.unwrap_or(false);
//    tokio::task::spawn_blocking(move || {
//        repo::init_repo(&repo_path, validate_only).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn list_git_remotes(
//    state: State<'_, AppState>,
//    workspace_id: String,
//) -> Result<Vec<GitRemoteDto>, String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::remotes(connection, root)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || repo::list_remotes(&repo_path).map_err(err_to_string))
//        .await
//        .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn add_git_remote(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    name: String,
//    url: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if name.is_empty() || name.contains(char::is_whitespace) {
//        return Err(format!("invalid remote name: {name}"));
//    }
//    if url.is_empty() {
//        return Err("url is required".to_string());
//    }
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::add_remote(connection, root, &name, &url)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::add_remote(&repo_path, &name, &url).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn remove_git_remote(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    name: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::remove_remote(connection, root, &name)
//            .await
//            .map_err(err_to_string);
//    }
//    if name.is_empty() {
//        return Err("name is required".to_string());
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::remove_remote(&repo_path, &name).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//#[tauri::command]
//pub async fn rename_git_remote(
//    state: State<'_, AppState>,
//    workspace_id: String,
//    old_name: String,
//    new_name: String,
//) -> Result<(), String> {
//    let repo_path = resolve_workspace_root(state.inner(), &workspace_id).await?;
//    if new_name.is_empty() || new_name.contains(char::is_whitespace) {
//        return Err(format!("invalid remote name: {new_name}"));
//    }
//    if let Some(target) = remote_target(state.inner(), &workspace_id).await? {
//        let (connection, root) = remote_parts(&target)?;
//        return remote_git::rename_remote(connection, root, &old_name, &new_name)
//            .await
//            .map_err(err_to_string);
//    }
//    tokio::task::spawn_blocking(move || {
//        repo::rename_remote(&repo_path, &old_name, &new_name).map_err(err_to_string)
//    })
//    .await
//    .map_err(|error| error.to_string())?
//}
//
//fn remote_worktree_path(root: &str, path: &str) -> Result<String, String> {
//    if path.contains('\0') {
//        return Err("远端工作树路径无效".to_string());
//    }
//    if path.starts_with('/') {
//        return Ok(path.to_string());
//    }
//    validate_remote_relative_path(path, false).map_err(err_to_string)?;
//    Ok(format!("{}/{}", root.trim_end_matches('/'), path))
//}
//
//#[cfg(test)]
//mod tests {
//    use super::local_workspace_git_context;
//    use super::remote_worktree_path;
//    use std::fs;
//
//    #[test]
//    fn remote_worktree_path_accepts_external_creation_path() {
//        assert_eq!(
//            remote_worktree_path("/srv/repo", "/tmp/feature").unwrap(),
//            "/tmp/feature"
//        );
//    }
//
//    #[test]
//    fn remote_worktree_path_accepts_project_relative_creation_path() {
//        assert_eq!(
//            remote_worktree_path("/srv/repo", ".auracoder/worktrees/feature").unwrap(),
//            "/srv/repo/.auracoder/worktrees/feature"
//        );
//        assert_eq!(
//            remote_worktree_path("/srv/repo", "custom/feature").unwrap(),
//            "/srv/repo/custom/feature"
//        );
//    }
//
//    /// 验证仅根目录的 Git 状态决定工作区 Git 上下文，且空仓库没有默认分支。
//    #[test]
//    fn local_workspace_git_context_uses_only_workspace_root_repository() {
//        let root =
//            std::env::temp_dir().join(format!("auracoder-git-context-{}", uuid::Uuid::new_v4()));
//        let child = root.join("child");
//        fs::create_dir_all(&child).expect("failed to create workspace test directories");
//        git2::Repository::init(&child).expect("failed to initialize child repository");
//
//        let context =
//            local_workspace_git_context("workspace-1", "Workspace", &root.to_string_lossy());
//        assert!(matches!(
//            context,
//            crate::models::WorkspaceGitContextDto::NotRepository { .. }
//        ));
//
//        git2::Repository::init(&root).expect("failed to initialize workspace repository");
//        let context =
//            local_workspace_git_context("workspace-1", "Workspace", &root.to_string_lossy());
//        match context {
//            crate::models::WorkspaceGitContextDto::Repository { default_branch, .. } => {
//                assert!(default_branch.is_none())
//            }
//            crate::models::WorkspaceGitContextDto::NotRepository { .. } => {
//                panic!("workspace root repository should be detected")
//            }
//        }
//    }
//}
//
//fn err_to_string(error: impl std::fmt::Display) -> String {
//    error.to_string()
//}

use tauri::State;

use crate::{
    models::{
        FileTreeEntryDto, FileTreePageDto, GitBranchPageDto, GitCommitPageDto, GitDiffPreviewDto,
        GitFileCompareDto, GitInitRepoStatusDto, GitRemoteDto, GitStashDto, GitStatusDto,
        GitWorktreeDto, WorkspaceGitContextDto,
    },
    services::git_service::GitService,
    state::AppState,
};

/// 读取工作区根目录自身的 Git 上下文，不递归检查子目录仓库。
#[tauri::command]
pub async fn get_workspace_git_context(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<WorkspaceGitContextDto, String> {
    GitService::new(state.inner().clone())
        .get_workspace_git_context(workspace_id)
        .await
}
/// 读取指定工作区的 Git 状态。
#[tauri::command]
pub async fn get_git_status(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<GitStatusDto, String> {
    GitService::new(state.inner().clone())
        .get_git_status(workspace_id)
        .await
}
/// 读取指定文件在暂存区或工作树中的 Git 差异。
#[tauri::command]
pub async fn get_file_diff(
    state: State<'_, AppState>,
    workspace_id: String,
    file_path: String,
    staged: bool,
) -> Result<GitDiffPreviewDto, String> {
    GitService::new(state.inner().clone())
        .get_file_diff(workspace_id, file_path, staged)
        .await
}
/// 读取指定文件相对 Git 基线的比较内容。
#[tauri::command]
pub async fn get_git_file_compare(
    state: State<'_, AppState>,
    workspace_id: String,
    file_path: String,
    source: String,
) -> Result<GitFileCompareDto, String> {
    GitService::new(state.inner().clone())
        .get_git_file_compare(workspace_id, file_path, source)
        .await
}
/// 将指定工作区文件加入 Git 暂存区。
#[tauri::command]
pub async fn stage_files(
    state: State<'_, AppState>,
    workspace_id: String,
    files: Vec<String>,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .stage_files(workspace_id, files)
        .await
}
/// 将指定工作区文件从 Git 暂存区移除。
#[tauri::command]
pub async fn unstage_files(
    state: State<'_, AppState>,
    workspace_id: String,
    files: Vec<String>,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .unstage_files(workspace_id, files)
        .await
}
/// 丢弃指定工作区文件的未提交修改。
#[tauri::command]
pub async fn discard_files(
    state: State<'_, AppState>,
    workspace_id: String,
    files: Vec<String>,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .discard_files(workspace_id, files)
        .await
}
/// 提交指定工作区当前暂存区中的变更并返回提交标识。
#[tauri::command]
pub async fn commit(
    state: State<'_, AppState>,
    workspace_id: String,
    message: String,
) -> Result<String, String> {
    GitService::new(state.inner().clone())
        .commit(workspace_id, message)
        .await
}
/// 撤销指定工作区最近一次提交并保留文件变更。
#[tauri::command]
pub async fn soft_reset_last_commit(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .soft_reset_last_commit(workspace_id)
        .await
}
/// 获取指定工作区 Git 远端更新。
#[tauri::command]
pub async fn fetch_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .fetch_git(workspace_id)
        .await
}
/// 拉取指定工作区 Git 远端更新。
#[tauri::command]
pub async fn pull_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .pull_git(workspace_id)
        .await
}
/// 推送指定工作区 Git 本地提交。
#[tauri::command]
pub async fn push_git(state: State<'_, AppState>, workspace_id: String) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .push_git(workspace_id)
        .await
}
/// 按范围和分页条件读取指定工作区的 Git 分支。
#[tauri::command]
pub async fn list_git_branches(
    state: State<'_, AppState>,
    workspace_id: String,
    scope: String,
    offset: Option<usize>,
    limit: Option<usize>,
    search: Option<String>,
) -> Result<GitBranchPageDto, String> {
    GitService::new(state.inner().clone())
        .list_git_branches(workspace_id, scope, offset, limit, search)
        .await
}
/// 切换指定工作区当前 Git 分支。
#[tauri::command]
pub async fn checkout_git_branch(
    state: State<'_, AppState>,
    workspace_id: String,
    branch_name: String,
    is_remote: bool,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .checkout_git_branch(workspace_id, branch_name, is_remote)
        .await
}
/// 在指定工作区创建 Git 分支。
#[tauri::command]
pub async fn create_git_branch(
    state: State<'_, AppState>,
    workspace_id: String,
    branch_name: String,
    from_ref: Option<String>,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .create_git_branch(workspace_id, branch_name, from_ref)
        .await
}
/// 重命名指定工作区的 Git 分支。
#[tauri::command]
pub async fn rename_git_branch(
    state: State<'_, AppState>,
    workspace_id: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .rename_git_branch(workspace_id, old_name, new_name)
        .await
}
/// 删除指定工作区的 Git 分支。
#[tauri::command]
pub async fn delete_git_branch(
    state: State<'_, AppState>,
    workspace_id: String,
    branch_name: String,
    force: bool,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .delete_git_branch(workspace_id, branch_name, force)
        .await
}
/// 按分页条件读取指定工作区的 Git 提交记录。
#[tauri::command]
pub async fn list_git_commits(
    state: State<'_, AppState>,
    workspace_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<GitCommitPageDto, String> {
    GitService::new(state.inner().clone())
        .list_git_commits(workspace_id, offset, limit)
        .await
}
/// 读取指定工作区的 Git 暂存记录。
#[tauri::command]
pub async fn list_git_stashes(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<GitStashDto>, String> {
    GitService::new(state.inner().clone())
        .list_git_stashes(workspace_id)
        .await
}
/// 保存指定工作区当前的 Git 工作树变更。
#[tauri::command]
pub async fn push_git_stash(
    state: State<'_, AppState>,
    workspace_id: String,
    message: Option<String>,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .push_git_stash(workspace_id, message)
        .await
}
/// 应用指定工作区的一条 Git 暂存记录。
#[tauri::command]
pub async fn apply_git_stash(
    state: State<'_, AppState>,
    workspace_id: String,
    stash_index: usize,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .apply_git_stash(workspace_id, stash_index)
        .await
}
/// 应用并移除指定工作区的一条 Git 暂存记录。
#[tauri::command]
pub async fn pop_git_stash(
    state: State<'_, AppState>,
    workspace_id: String,
    stash_index: usize,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .pop_git_stash(workspace_id, stash_index)
        .await
}
/// 读取指定提交在工作区中的 Git 差异。
#[tauri::command]
pub async fn get_commit_diff(
    state: State<'_, AppState>,
    workspace_id: String,
    commit_hash: String,
) -> Result<GitDiffPreviewDto, String> {
    GitService::new(state.inner().clone())
        .get_commit_diff(workspace_id, commit_hash)
        .await
}
/// 读取指定工作区的 Git 文件树。
#[tauri::command]
pub async fn get_file_tree(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<FileTreeEntryDto>, String> {
    GitService::new(state.inner().clone())
        .get_file_tree(workspace_id)
        .await
}
/// 按分页条件读取指定工作区的 Git 文件树。
#[tauri::command]
pub async fn get_file_tree_page(
    state: State<'_, AppState>,
    workspace_id: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<FileTreePageDto, String> {
    GitService::new(state.inner().clone())
        .get_file_tree_page(workspace_id, offset, limit)
        .await
}
/// 监听指定工作区的 Git 仓库变化并发送应用事件。
#[tauri::command]
pub async fn watch_git_repo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .watch_git_repo(app, workspace_id)
        .await
}
/// 在指定工作区仓库中创建 Git 工作树。
#[tauri::command]
pub async fn add_git_worktree(
    state: State<'_, AppState>,
    workspace_id: String,
    worktree_path: String,
    branch_name: String,
    base_ref: Option<String>,
) -> Result<GitWorktreeDto, String> {
    GitService::new(state.inner().clone())
        .add_git_worktree(workspace_id, worktree_path, branch_name, base_ref)
        .await
}
/// 读取指定工作区仓库登记的 Git 工作树。
#[tauri::command]
pub async fn list_git_worktrees(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<GitWorktreeDto>, String> {
    GitService::new(state.inner().clone())
        .list_git_worktrees(workspace_id)
        .await
}
/// 移除指定工作区仓库中的 Git 工作树。
#[tauri::command]
pub async fn remove_git_worktree(
    state: State<'_, AppState>,
    workspace_id: String,
    worktree_path: String,
    force: bool,
    branch_name: Option<String>,
    delete_branch: bool,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .remove_git_worktree(workspace_id, worktree_path, force, branch_name, delete_branch)
        .await
}
/// 清理指定工作区仓库中的失效 Git 工作树记录。
#[tauri::command]
pub async fn prune_git_worktrees(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .prune_git_worktrees(workspace_id)
        .await
}
/// 在指定工作区根目录初始化 Git 仓库或校验初始化条件。
#[tauri::command]
pub async fn init_git_repo(
    state: State<'_, AppState>,
    workspace_id: String,
    validate_only: Option<bool>,
) -> Result<GitInitRepoStatusDto, String> {
    GitService::new(state.inner().clone())
        .init_git_repo(workspace_id, validate_only)
        .await
}
/// 读取指定工作区 Git 仓库的远端列表。
#[tauri::command]
pub async fn list_git_remotes(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<GitRemoteDto>, String> {
    GitService::new(state.inner().clone())
        .list_git_remotes(workspace_id)
        .await
}
/// 为指定工作区 Git 仓库添加远端。
#[tauri::command]
pub async fn add_git_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
    url: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .add_git_remote(workspace_id, name, url)
        .await
}
/// 移除指定工作区 Git 仓库的远端。
#[tauri::command]
pub async fn remove_git_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .remove_git_remote(workspace_id, name)
        .await
}
/// 重命名指定工作区 Git 仓库的远端。
#[tauri::command]
pub async fn rename_git_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    GitService::new(state.inner().clone())
        .rename_git_remote(workspace_id, old_name, new_name)
        .await
}
