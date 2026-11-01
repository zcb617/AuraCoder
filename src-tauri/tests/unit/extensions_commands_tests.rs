
    use super::is_global_context;

    #[test]
    fn identifies_only_missing_workspace_and_cwd_as_global_context() {
        assert!(is_global_context(None, None));
        assert!(is_global_context(Some("  \t"), None));
        assert!(!is_global_context(Some("workspace"), None));
        assert!(!is_global_context(None, Some("C:/work/project")));
    }
