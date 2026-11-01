
    use super::*;

    fn normalize_path(path: &str) -> String {
        path.replace('\\', "/")
    }

    #[test]
    fn windows_terminal_env_prefers_windows_dirs_without_unix_overrides() {
        let config = build_terminal_env_config_for(
            true,
            TerminalEnvInputs {
                default_home: Some(r"C:\Users\auracoder".to_string()),
                default_local_app_data: Some(r"C:\Users\auracoder\AppData\Local".to_string()),
                default_roaming_app_data: Some(r"C:\Users\auracoder\AppData\Roaming".to_string()),
                default_temp_dir: Some(r"C:\Users\auracoder\AppData\Local\Temp".to_string()),
                path: Some(r"C:\Tools;C:\Windows\System32".to_string()),
                ..TerminalEnvInputs::default()
            },
        );

        assert_eq!(
            config
                .snapshot
                .home
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder")
        );
        assert_eq!(
            config
                .user_profile
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder")
        );
        assert_eq!(
            config
                .snapshot
                .user_profile
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder")
        );
        assert_eq!(
            config
                .local_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local")
        );
        assert_eq!(
            config
                .snapshot
                .local_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local")
        );
        assert_eq!(
            config
                .roaming_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Roaming")
        );
        assert_eq!(
            config
                .snapshot
                .app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Roaming")
        );
        assert_eq!(
            config.temp.as_deref().map(normalize_path).as_deref(),
            Some("C:/Users/auracoder/AppData/Local/Temp")
        );
        assert_eq!(
            config
                .snapshot
                .temp
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local/Temp")
        );
        assert_eq!(
            config.tmp.as_deref().map(normalize_path).as_deref(),
            Some("C:/Users/auracoder/AppData/Local/Temp")
        );
        assert_eq!(
            config
                .snapshot
                .tmp
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local/Temp")
        );
        assert!(config.snapshot.xdg_config_home.is_none());
        assert!(config.snapshot.xdg_data_home.is_none());
        assert!(config.snapshot.xdg_cache_home.is_none());
        assert!(config.snapshot.xdg_state_home.is_none());
        assert_eq!(
            config
                .snapshot
                .tmpdir
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local/Temp")
        );
        assert!(config.snapshot.lang.is_none());
        assert!(config.snapshot.lc_all.is_none());
        assert!(config.snapshot.lc_ctype.is_none());
        assert_eq!(
            config.snapshot.path.as_deref(),
            Some(r"C:\Tools;C:\Windows\System32")
        );
    }

    #[test]
    fn windows_terminal_env_uses_user_profile_for_windows_dirs() {
        let config = build_terminal_env_config_for(
            true,
            TerminalEnvInputs {
                home: Some("/c/Users/auracoder".to_string()),
                user_profile: Some(r"C:\Users\auracoder".to_string()),
                path: Some(r"C:\Tools;C:\Windows\System32".to_string()),
                ..TerminalEnvInputs::default()
            },
        );

        assert_eq!(
            config
                .snapshot
                .home
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("/c/Users/auracoder")
        );
        assert_eq!(
            config
                .snapshot
                .user_profile
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder")
        );
        assert_eq!(
            config
                .local_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local")
        );
        assert_eq!(
            config
                .roaming_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Roaming")
        );
    }

    #[test]
    fn windows_command_line_extracts_tool_from_cmd_wrapper() {
        let tool = extract_tool_name_from_windows_command_line(
            "cmd",
            r#"cmd.exe /d /s /c "codex --help""#,
        );

        assert_eq!(tool.as_deref(), Some("codex"));
    }

    #[test]
    fn windows_command_line_extracts_tool_from_node_script() {
        let tool = extract_tool_name_from_windows_command_line(
            "node",
            r#""C:\Program Files\nodejs\node.exe" "C:\Users\auracoder\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js" --version"#,
        );

        assert_eq!(tool.as_deref(), Some("codex"));
    }

    #[test]
    fn windows_command_line_extracts_tool_from_powershell_wrapper() {
        let tool = extract_tool_name_from_windows_command_line(
            "powershell",
            r#"powershell.exe -NoProfile -Command "& 'C:\Users\auracoder\AppData\Roaming\npm\claude.cmd' --help""#,
        );

        assert_eq!(tool.as_deref(), Some("claude"));
    }

    #[test]
    fn windows_foreground_process_skips_conhost_and_uses_wrapped_tool_name() {
        let selected = select_windows_foreground_process(&[
            WindowsChildProcess {
                process_id: 4200,
                name: Some("conhost.exe".to_string()),
                command_line: Some(r#"C:\Windows\System32\conhost.exe 0x4"#.to_string()),
            },
            WindowsChildProcess {
                process_id: 4201,
                name: Some("cmd.exe".to_string()),
                command_line: Some(r#"cmd.exe /d /s /c "gemini --help""#.to_string()),
            },
        ]);

        assert_eq!(selected, Some((4201, "gemini".to_string())));
    }

    #[test]
    fn unix_terminal_env_keeps_xdg_and_locale_defaults() {
        let config = build_terminal_env_config_for(
            false,
            TerminalEnvInputs {
                home: Some("/home/auracoder".to_string()),
                path: Some("/custom/bin:/usr/bin".to_string()),
                ..TerminalEnvInputs::default()
            },
        );

        assert_eq!(config.snapshot.home.as_deref(), Some("/home/auracoder"));
        assert_eq!(
            config.snapshot.xdg_config_home.as_deref(),
            Some("/home/auracoder/.config")
        );
        assert_eq!(
            config.snapshot.xdg_data_home.as_deref(),
            Some("/home/auracoder/.local/share")
        );
        assert_eq!(
            config.snapshot.xdg_cache_home.as_deref(),
            Some("/home/auracoder/.cache")
        );
        assert_eq!(
            config.snapshot.xdg_state_home.as_deref(),
            Some("/home/auracoder/.local/state")
        );
        assert_eq!(config.snapshot.lang.as_deref(), Some("en_US.UTF-8"));
        assert_eq!(config.snapshot.lc_ctype.as_deref(), Some("en_US.UTF-8"));
        assert!(config.snapshot.lc_all.is_none());
        assert_eq!(
            config.snapshot.path.as_deref(),
            Some("/custom/bin:/usr/bin")
        );
        assert!(config.snapshot.user_profile.is_none());
        assert!(config.snapshot.app_data.is_none());
        assert!(config.snapshot.local_app_data.is_none());
        assert!(config.snapshot.temp.is_none());
        assert!(config.snapshot.tmp.is_none());
        assert!(config.user_profile.is_none());
        assert!(config.local_app_data.is_none());
        assert!(config.roaming_app_data.is_none());
        assert!(config.temp.is_none());
        assert!(config.tmp.is_none());
    }

    #[test]
    fn windows_terminal_env_preserves_explicit_home() {
        let config = build_terminal_env_config_for(
            true,
            TerminalEnvInputs {
                home: Some(r"D:\custom-home".to_string()),
                user_profile: Some(r"C:\Users\auracoder".to_string()),
                local_app_data: Some(r"C:\Users\auracoder\AppData\Local".to_string()),
                roaming_app_data: Some(r"C:\Users\auracoder\AppData\Roaming".to_string()),
                temp: Some(r"C:\Users\auracoder\AppData\Local\Temp".to_string()),
                ..TerminalEnvInputs::default()
            },
        );

        assert_eq!(
            config
                .snapshot
                .home
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("D:/custom-home")
        );
        assert_eq!(
            config
                .snapshot
                .user_profile
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder")
        );
        assert_eq!(
            config
                .snapshot
                .local_app_data
                .as_deref()
                .map(normalize_path)
                .as_deref(),
            Some("C:/Users/auracoder/AppData/Local")
        );
    }
