
    use super::*;

    #[test]
    fn npm_package_from_install_args_matches_every_harness_definition() {
        for def in HARNESSES {
            if def.install_command == Some("npm") {
                assert!(
                    npm_package_from_install_args(def.install_args).is_some(),
                    "expected {} to have an install_args shape of [\"install\", \"-g\", <package>]",
                    def.id
                );
            }
        }
    }

    #[test]
    fn npm_package_from_install_args_rejects_unexpected_shapes() {
        assert_eq!(
            npm_package_from_install_args(&["install", "opencode-ai"]),
            None
        );
        assert_eq!(npm_package_from_install_args(&[]), None);
        assert_eq!(
            npm_package_from_install_args(&["install", "-g", "opencode-ai"]),
            Some("opencode-ai")
        );
    }

    #[test]
    fn compose_launch_command_appends_configured_args() {
        assert_eq!(compose_launch_command("codex", None).unwrap(), "codex");
        assert_eq!(
            compose_launch_command("codex", Some("--yolo")).unwrap(),
            "codex --yolo"
        );
        assert_eq!(
            compose_launch_command("claude", Some("  --dangerously-skip-permissions  ")).unwrap(),
            "claude --dangerously-skip-permissions"
        );
        assert_eq!(
            compose_launch_command("codex", Some("   ")).unwrap(),
            "codex"
        );
    }

    #[test]
    fn normalize_launch_args_accepts_portable_argument_characters() {
        assert_eq!(
            normalize_launch_args("--model=gpt-5 --config /Users/me/file.toml").unwrap(),
            "--model=gpt-5 --config /Users/me/file.toml"
        );
        assert_eq!(normalize_launch_args("--a\t--b").unwrap(), "--a --b");
    }

    #[test]
    fn normalize_launch_args_rejects_shell_syntax() {
        for args in [
            "--yolo; whoami",
            "--flag | whoami",
            "--flag $(whoami)",
            "--flag `whoami`",
            "--flag > output.txt",
            "--flag\nwhoami",
            "--name \"two words\"",
        ] {
            assert!(
                normalize_launch_args(args).is_err(),
                "accepted unsafe args: {args}"
            );
        }
    }
