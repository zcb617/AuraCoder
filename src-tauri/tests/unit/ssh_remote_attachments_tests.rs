
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
