    use super::local_workspace_git_context;
    use super::remote_worktree_path;
    use std::fs;

    #[test]
    fn remote_worktree_path_accepts_external_creation_path() {
        assert_eq!(
            remote_worktree_path("/srv/repo", "/tmp/feature").unwrap(),
            "/tmp/feature"
        );
    }

    #[test]
    fn remote_worktree_path_accepts_project_relative_creation_path() {
        assert_eq!(
            remote_worktree_path("/srv/repo", ".auracoder/worktrees/feature").unwrap(),
            "/srv/repo/.auracoder/worktrees/feature"
        );
        assert_eq!(
            remote_worktree_path("/srv/repo", "custom/feature").unwrap(),
            "/srv/repo/custom/feature"
        );
    }

    /// 验证仅根目录的 Git 状态决定工作区 Git 上下文，且空仓库没有默认分支。
    #[test]
    fn local_workspace_git_context_uses_only_workspace_root_repository() {
        let root =
            std::env::temp_dir().join(format!("auracoder-git-context-{}", uuid::Uuid::new_v4()));
        let child = root.join("child");
        fs::create_dir_all(&child).expect("failed to create workspace test directories");
        git2::Repository::init(&child).expect("failed to initialize child repository");

        let context =
            local_workspace_git_context("workspace-1", "Workspace", &root.to_string_lossy());
        assert!(matches!(
            context,
            crate::models::WorkspaceGitContextDto::NotRepository { .. }
        ));

        git2::Repository::init(&root).expect("failed to initialize workspace repository");
        let context =
            local_workspace_git_context("workspace-1", "Workspace", &root.to_string_lossy());
        match context {
            crate::models::WorkspaceGitContextDto::Repository { default_branch, .. } => {
                assert!(default_branch.is_none())
            }
            crate::models::WorkspaceGitContextDto::NotRepository { .. } => {
                panic!("workspace root repository should be detected")
            }
        }
    }
