use git2::Repository;
use serde::Serialize;
use tauri::Emitter;

use crate::{
    git::{repo, worktree},
    models::{
        FileTreeEntryDto, FileTreePageDto, GitBranchPageDto, GitBranchScopeDto, GitCommitPageDto,
        GitCompareSourceDto, GitDiffPreviewDto, GitFileCompareDto, GitInitRepoStatusDto,
        GitRemoteDto, GitStashDto, GitStatusDto, GitWorktreeDto, SshConnectionRecord,
        WorkspaceGitContextDto, WorkspaceTarget,
    },
    services::workspace_service::WorkspaceService,
    ssh::runtime::validate_remote_relative_path,
    ssh::remote_git,
    state::AppState,
};

/// GitService 承接工作区 Git 查询、变更、远端操作和工作树编排业务。
pub struct GitService {
    /// Git 命令通过该应用状态访问工作区资源。
    state: AppState,
}

/// 保存远端 Git 操作使用的工作区目标和根目录。
struct RemoteGitTarget {
    /// 远端工作区与连接记录组成的目标。
    target: WorkspaceTarget,
    /// 远端 Git 仓库根目录。
    root: String,
}

/// 表示当前工作区 Git 操作对应的本机或远端目标。
enum WorkspaceGitTarget {
    Local {
        /// 本机工作区根目录。
        root_path: String,
    },
    Remote {
        /// 远端工作区 Git 操作所需的连接和仓库路径。
        target: RemoteGitTarget,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoChangedEvent {
    /// 发生 Git 仓库变化的工作区标识。
    workspace_id: String,
}

impl GitService {
    /// 创建绑定应用状态的工作区 Git 业务服务。
    pub fn new(state: AppState) -> Self {
        // 保存应用状态供 Git 命令访问工作区资源。
        Self { state }
    }

    /// 读取工作区根目录自身的 Git 上下文，不递归检查子目录仓库。
    pub async fn get_workspace_git_context(
        &self,
        workspace_id: String,
    ) -> Result<WorkspaceGitContextDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let workspace = WorkspaceService::new(state.clone())
            .get_workspace(&workspace_id)
            .await
            .map_err(err_to_string)?;
        if workspace.location_kind == "ssh" {
            let target = WorkspaceService::new(state.clone())
                .resolve_workspace_target(&workspace_id)
                .await
                .map_err(err_to_string)?;
            let connection = target.remote_connection().map_err(err_to_string)?;
            let Some(info) = remote_git::discover(connection, &workspace.root_path)
                .await
                .map_err(err_to_string)?
            else {
                return Ok(WorkspaceGitContextDto::NotRepository { workspace_id });
            };
            return Ok(WorkspaceGitContextDto::Repository {
                workspace_id,
                root_path: workspace.root_path,
                name: info.name,
                default_branch: Some(info.default_branch),
            });
        }
        Ok(local_workspace_git_context(
            &workspace_id,
            &workspace.name,
            &workspace.root_path,
        ))
    }
    /// 读取指定工作区的 Git 状态。
    pub async fn get_git_status(
        &self,
        workspace_id: String,
    ) -> Result<GitStatusDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::status(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::get_git_status(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 读取指定文件在暂存区或工作树中的 Git 差异。
    pub async fn get_file_diff(
        &self,
        workspace_id: String,
        file_path: String,
        staged: bool,
    ) -> Result<GitDiffPreviewDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 读取指定文件相对 Git 基线的比较内容。
    pub async fn get_git_file_compare(
        &self,
        workspace_id: String,
        file_path: String,
        source: String,
    ) -> Result<GitFileCompareDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        let compare_source = GitCompareSourceDto::from_str(&source);
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 将指定工作区文件加入 Git 暂存区。
    pub async fn stage_files(
        &self,
        workspace_id: String,
        files: Vec<String>,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 将指定工作区文件从 Git 暂存区移除。
    pub async fn unstage_files(
        &self,
        workspace_id: String,
        files: Vec<String>,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 丢弃指定工作区文件的未提交修改。
    pub async fn discard_files(
        &self,
        workspace_id: String,
        files: Vec<String>,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 提交指定工作区当前暂存区中的变更并返回提交标识。
    pub async fn commit(
        &self,
        workspace_id: String,
        message: String,
    ) -> Result<String, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::commit(connection, root, &message)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::commit(&repo_path, &message).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 撤销指定工作区最近一次提交并保留文件变更。
    pub async fn soft_reset_last_commit(
        &self,
        workspace_id: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 获取指定工作区 Git 远端更新。
    pub async fn fetch_git(&self, workspace_id: String) -> Result<(), String> {

        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::fetch(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::fetch_repo(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 拉取指定工作区 Git 远端更新。
    pub async fn pull_git(&self, workspace_id: String) -> Result<(), String> {

        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::pull(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::pull_repo(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 推送指定工作区 Git 本地提交。
    pub async fn push_git(&self, workspace_id: String) -> Result<(), String> {

        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::push(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::push_repo(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 按范围和分页条件读取指定工作区的 Git 分支。
    pub async fn list_git_branches(
        &self,
        workspace_id: String,
        scope: String,
        offset: Option<usize>,
        limit: Option<usize>,
        search: Option<String>,
    ) -> Result<GitBranchPageDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(200);
        let scope = GitBranchScopeDto::from_str(&scope);

        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 切换指定工作区当前 Git 分支。
    pub async fn checkout_git_branch(
        &self,
        workspace_id: String,
        branch_name: String,
        is_remote: bool,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 在指定工作区创建 Git 分支。
    pub async fn create_git_branch(
        &self,
        workspace_id: String,
        branch_name: String,
        from_ref: Option<String>,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 重命名指定工作区的 Git 分支。
    pub async fn rename_git_branch(
        &self,
        workspace_id: String,
        old_name: String,
        new_name: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 删除指定工作区的 Git 分支。
    pub async fn delete_git_branch(
        &self,
        workspace_id: String,
        branch_name: String,
        force: bool,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 按分页条件读取指定工作区的 Git 提交记录。
    pub async fn list_git_commits(
        &self,
        workspace_id: String,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<GitCommitPageDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(100);

        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 读取指定工作区的 Git 暂存记录。
    pub async fn list_git_stashes(
        &self,
        workspace_id: String,
    ) -> Result<Vec<GitStashDto>, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::stashes(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::list_git_stashes(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 保存指定工作区当前的 Git 工作树变更。
    pub async fn push_git_stash(
        &self,
        workspace_id: String,
        message: Option<String>,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 应用指定工作区的一条 Git 暂存记录。
    pub async fn apply_git_stash(
        &self,
        workspace_id: String,
        stash_index: usize,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 应用并移除指定工作区的一条 Git 暂存记录。
    pub async fn pop_git_stash(
        &self,
        workspace_id: String,
        stash_index: usize,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 读取指定提交在工作区中的 Git 差异。
    pub async fn get_commit_diff(
        &self,
        workspace_id: String,
        commit_hash: String,
    ) -> Result<GitDiffPreviewDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 读取指定工作区的 Git 文件树。
    pub async fn get_file_tree(
        &self,
        workspace_id: String,
    ) -> Result<Vec<FileTreeEntryDto>, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 按分页条件读取指定工作区的 Git 文件树。
    pub async fn get_file_tree_page(
        &self,
        workspace_id: String,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<FileTreePageDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(2000);
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 监听指定工作区的 Git 仓库变化并发送应用事件。
    pub async fn watch_git_repo(
        &self,
        app: tauri::AppHandle,
        workspace_id: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        let cache = state.file_tree_cache.clone();
        let workspace_id_for_event = workspace_id.clone();
        let callback = std::sync::Arc::new(move |changed_repo_path: String| {
            cache.invalidate_containing_path(&changed_repo_path);
            let payload = GitRepoChangedEvent {
                workspace_id: workspace_id_for_event.clone(),
            };
            let _ = app.emit("git-repo-changed", payload);
        });

        state
            .git_watchers
            .watch_repo(repo_path, callback)
            .await
            .map_err(err_to_string)
    }
    /// 在指定工作区仓库中创建 Git 工作树。
    pub async fn add_git_worktree(
        &self,
        workspace_id: String,
        worktree_path: String,
        branch_name: String,
        base_ref: Option<String>,
    ) -> Result<GitWorktreeDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        // Validate branch name
        if branch_name.contains("..")
            || branch_name.starts_with('/')
            || branch_name.ends_with('/')
            || branch_name.contains(' ')
            || branch_name.is_empty()
        {
            return Err(format!("invalid branch name: {branch_name}"));
        }
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            let remote_path = remote_worktree_path(root, &worktree_path)?;
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
                path: created.path.clone(),
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
                log::warn!(
                    "warning: failed to ensure .auracoder/ in .gitignore for '{}': {}",
                    repo_path, error
                );
            }

            Ok(created)
        })
        .await
        .map_err(|error| error.to_string())?
    }
    /// 读取指定工作区仓库登记的 Git 工作树。
    pub async fn list_git_worktrees(
        &self,
        workspace_id: String,
    ) -> Result<Vec<GitWorktreeDto>, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            let workspace_root = target.target.workspace.root_path.clone();
            return remote_git::worktrees(connection, root)
                .await
                .map_err(err_to_string)?
                .into_iter()
                .map(|worktree| {
                    if worktree.is_main {
                        return Ok(GitWorktreeDto {
                            path: workspace_root.clone(),
                            display_path: Some(worktree.path.clone()),
                            ..worktree
                        });
                    }
                    if !worktree.path.starts_with('/') || worktree.path.contains('\0') {
                        return Err("远端工作树路径无效".to_string());
                    }
                    Ok(GitWorktreeDto {
                        path: worktree.path.clone(),
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
    /// 移除指定工作区仓库中的 Git 工作树。
    pub async fn remove_git_worktree(
        &self,
        workspace_id: String,
        worktree_path: String,
        force: bool,
        branch_name: Option<String>,
        delete_branch: bool,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            if !worktree_path.starts_with('/') || worktree_path.contains('\0') {
                return Err("远端工作树路径必须是绝对路径".to_string());
            }
            let registered = remote_git::worktrees(connection, root)
                .await
                .map_err(err_to_string)?
                .into_iter()
                .any(|worktree| worktree.path == worktree_path);
            if !registered {
                return Err("远端工作树没有登记在当前项目中".to_string());
            }
            let result = remote_git::remove_worktree(connection, root, &worktree_path, force)
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
    /// 清理指定工作区仓库中的失效 Git 工作树记录。
    pub async fn prune_git_worktrees(
        &self,
        workspace_id: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 在指定工作区根目录初始化 Git 仓库或校验初始化条件。
    pub async fn init_git_repo(
        &self,
        workspace_id: String,
        validate_only: Option<bool>,
    ) -> Result<GitInitRepoStatusDto, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root_unchecked(state, &workspace_id).await?;
        if repo_path.is_empty() {
            return Err("workspace root is required".to_string());
        }
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
                    blocking_root_path: None,
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
                blocking_root_path: None,
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
    /// 读取指定工作区 Git 仓库的远端列表。
    pub async fn list_git_remotes(
        &self,
        workspace_id: String,
    ) -> Result<Vec<GitRemoteDto>, String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
            let (connection, root) = remote_parts(&target)?;
            return remote_git::remotes(connection, root)
                .await
                .map_err(err_to_string);
        }
        tokio::task::spawn_blocking(move || repo::list_remotes(&repo_path).map_err(err_to_string))
            .await
            .map_err(|error| error.to_string())?
    }
    /// 为指定工作区 Git 仓库添加远端。
    pub async fn add_git_remote(
        &self,
        workspace_id: String,
        name: String,
        url: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(format!("invalid remote name: {name}"));
        }
        if url.is_empty() {
            return Err("url is required".to_string());
        }
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 移除指定工作区 Git 仓库的远端。
    pub async fn remove_git_remote(
        &self,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
    /// 重命名指定工作区 Git 仓库的远端。
    pub async fn rename_git_remote(
        &self,
        workspace_id: String,
        old_name: String,
        new_name: String,
    ) -> Result<(), String> {
        // 按原有工作区目标选择本机或远端 Git 业务实现并返回结果。
        let state = &self.state;
        let repo_path = resolve_workspace_root(state, &workspace_id).await?;
        if new_name.is_empty() || new_name.contains(char::is_whitespace) {
            return Err(format!("invalid remote name: {new_name}"));
        }
        if let Some(target) = remote_target(state, &workspace_id).await? {
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
}

/// 按工作区主键解析唯一项目 Git 目标，并按要求校验基础仓库。
async fn resolve_workspace_git_target(
    state: &AppState,
    workspace_id: &str,
    require_repository: bool,
) -> Result<WorkspaceGitTarget, String> {
    // 通过 WorkspaceService 读取工作区和关联目标，再校验根目录仓库状态。
    let workspace = WorkspaceService::new(state.clone())
        .get_workspace(workspace_id)
        .await
        .map_err(err_to_string)?;
    if workspace.location_kind == "local" {
        if require_repository
            && (!std::path::Path::new(&workspace.root_path)
                .join(".git")
                .exists()
                || git2::Repository::open(&workspace.root_path).is_err())
        {
            return Err("workspace root is not a Git repository".to_string());
        }
        return Ok(WorkspaceGitTarget::Local {
            root_path: workspace.root_path,
        });
    }
    let target = WorkspaceService::new(state.clone())
        .resolve_workspace_target(&workspace.id)
        .await
        .map_err(err_to_string)?;
    if !target.is_remote() {
        return Err("SSH workspace target is unavailable".to_string());
    }
    let target = RemoteGitTarget {
        root: target.workspace.root_path.clone(),
        target,
    };
    if require_repository {
        let connection = target.target.remote_connection().map_err(err_to_string)?;
        if remote_git::discover(connection, &target.root)
            .await
            .map_err(err_to_string)?
            .is_none()
        {
            return Err("workspace root is not a Git repository".to_string());
        }
    }
    Ok(WorkspaceGitTarget::Remote { target })
}

/// 读取本地工作区根目录的 Git 上下文，只识别根目录自身的仓库状态。
fn local_workspace_git_context(
    workspace_id: &str,
    workspace_name: &str,
    root_path: &str,
) -> WorkspaceGitContextDto {
    // 只检查工作区根目录自身的 .git 标记并构造原有上下文。
    let root = std::path::Path::new(root_path);
    let Ok(repository) = (if root.join(".git").exists() {
        Repository::open(root)
    } else {
        Err(git2::Error::from_str(
            "workspace root is not a Git repository",
        ))
    }) else {
        return WorkspaceGitContextDto::NotRepository {
            workspace_id: workspace_id.to_string(),
        };
    };

    let name = root
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| workspace_name.to_string());
    let default_branch = repository
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(str::to_string));
    WorkspaceGitContextDto::Repository {
        workspace_id: workspace_id.to_string(),
        root_path: root_path.to_string(),
        name,
        default_branch,
    }
}

/// 解析工作区的远端 Git 目标；本地工作区返回空值。
async fn remote_target(
    state: &AppState,
    workspace_id: &str,
) -> Result<Option<RemoteGitTarget>, String> {
    // 通过 WorkspaceService 解析目标，仅返回 SSH 工作区的远端信息。
    let target = WorkspaceService::new(state.clone())
        .resolve_workspace_target(workspace_id)
        .await
        .map_err(err_to_string)?;
    if !target.is_remote() {
        return Ok(None);
    }
    let root = target.workspace.root_path.clone();
    Ok(Some(RemoteGitTarget { target, root }))
}

/// 根据工作区主键解析项目根目录，禁止调用方直接指定基础仓库路径。
async fn resolve_workspace_root(state: &AppState, workspace_id: &str) -> Result<String, String> {
    // 根据本机或远端目标返回原有工作区根目录。
    match resolve_workspace_git_target(state, workspace_id, true).await? {
        WorkspaceGitTarget::Local { root_path } => Ok(root_path),
        WorkspaceGitTarget::Remote { target } => Ok(target.root),
    }
}

/// 解析允许执行初始化的工作区根目录，初始化前允许根目录尚未存在 Git 仓库。
async fn resolve_workspace_root_unchecked(
    state: &AppState,
    workspace_id: &str,
) -> Result<String, String> {
    // 通过 WorkspaceService 读取初始化允许使用的根目录。
    let workspace = WorkspaceService::new(state.clone())
        .get_workspace(&workspace_id)
        .await
        .map_err(err_to_string)?;
    Ok(workspace.root_path)
}

/// 从远端 Git 目标读取连接记录和仓库根目录。
fn remote_parts(
    target: &RemoteGitTarget,
) -> Result<(&SshConnectionRecord, &str), String> {
    // 将模型连接记录与仓库根目录组合为远端 Git 执行参数。
    Ok((
        target.target.remote_connection().map_err(err_to_string)?,
        &target.root,
    ))
}

/// 确保指定忽略规则作为完整行存在于仓库的 .gitignore 文件中。
fn ensure_gitignore_entry(repo_path: &str, pattern: &str) -> Result<(), String> {
    // 按原有规则检查并写入 .gitignore 中的忽略条目。
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

/// 解析远端工作树创建路径，保留绝对路径或拼接已校验的项目相对路径。
fn remote_worktree_path(root: &str, path: &str) -> Result<String, String> {
    // 保留外部绝对路径，并按原规则校验项目相对路径。
    if path.contains('\0') {
        return Err("远端工作树路径无效".to_string());
    }
    if path.starts_with('/') {
        return Ok(path.to_string());
    }
    validate_remote_relative_path(path, false).map_err(err_to_string)?;
    Ok(format!("{}/{}", root.trim_end_matches('/'), path))
}

#[cfg(test)]
#[path = "../../tests/unit/git_commands_tests.rs"]
mod tests;

/// 将业务错误转换为原命令返回的字符串形式。
fn err_to_string(error: impl std::fmt::Display) -> String {
    // 返回错误对象原有的字符串表示。
    error.to_string()
}
