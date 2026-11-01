
    use super::{validate_remote_relative_path, workspace_id_from_workspace_marker};

    #[test]
    fn wraps_remote_command_with_remote_login_shell() {
        assert_eq!(
            super::wrap_remote_login_shell_command("exec env codex --version"),
            "\"${SHELL:-/bin/sh}\" -lic 'exec env codex --version'"
        );
    }

    #[test]
    fn escapes_single_quotes_in_remote_login_shell_command() {
        assert_eq!(
            super::wrap_remote_login_shell_command("printf '%s' value"),
            "\"${SHELL:-/bin/sh}\" -lic 'printf '\\''%s'\\'' value'"
        );
    }

    #[test]
    fn allows_empty_path_when_addressing_workspace_root() {
        assert!(validate_remote_relative_path("", true).is_ok());
    }

    #[test]
    fn rejects_empty_path_for_file_operations() {
        assert!(validate_remote_relative_path("", false).is_err());
    }

    #[test]
    fn still_rejects_empty_segments_inside_relative_path() {
        assert!(validate_remote_relative_path("src//main.rs", true).is_err());
    }

    #[test]
    fn parses_project_root_workspace_marker() {
        assert_eq!(
            workspace_id_from_workspace_marker("ssh://auracoder/ws-1"),
            Some("ws-1")
        );
        assert_eq!(workspace_id_from_workspace_marker("/srv/project"), None);
    }
