
    use super::*;
    use std::{
        ffi::{OsStr, OsString},
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };
    use uuid::Uuid;

    #[test]
    fn system_time_contains_the_operating_system_offset() {
        let value = system_time_rfc3339();
        let parsed = chrono::DateTime::parse_from_rfc3339(&value).unwrap();
        assert_eq!(
            parsed.offset().local_minus_utc(),
            chrono::Local::now().offset().local_minus_utc()
        );
        assert!(!value.ends_with('Z'));
    }

    #[test]
    fn persisted_time_formats_share_one_absolute_local_time() {
        let z = normalize_time_to_local("2026-08-18T10:00:00.123Z").unwrap();
        let offset = normalize_time_to_local("2026-08-18T18:00:00.123+08:00").unwrap();
        let sqlite = normalize_time_to_local("2026-08-18 10:00:00.123").unwrap();
        assert_eq!(z, offset);
        assert_eq!(z, sqlite);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&z)
                .unwrap()
                .offset()
                .local_minus_utc(),
            chrono::Local::now().offset().local_minus_utc()
        );
    }

    #[test]
    fn persisted_time_parse_error_contains_original_input() {
        let raw = "not-a-time";
        let error = normalize_time_to_local(raw).expect_err("invalid time should fail");
        assert!(error.to_string().contains(raw));
    }

    fn normalize_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn env_lock() -> &'static Mutex<()> {
        crate::process_utils::test_env_lock()
    }

    struct PathEnvGuard {
        original_path: Option<OsString>,
        temp_dir: PathBuf,
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            match self.original_path.as_ref() {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
            let _ = std::fs::remove_dir_all(&self.temp_dir);
        }
    }

    struct EnvironmentVariableGuard {
        /// 被保护的进程环境变量名称。
        name: &'static str,
        /// 测试开始前该环境变量的原始值。
        original: Option<OsString>,
    }

    impl Drop for EnvironmentVariableGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn environment_variable_guard(name: &'static str) -> EnvironmentVariableGuard {
        EnvironmentVariableGuard {
            name,
            original: std::env::var_os(name),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_preserves_current_environment_and_prepends_executable_parent() {
        let _env_guard = env_lock().lock().expect("env lock poisoned");
        let variable_name = "PANES_RUNTIME_ENV_DEFAULT_TEST";
        let _variable_guard = environment_variable_guard(variable_name);
        std::env::set_var(variable_name, "current-value");

        let executable = std::env::temp_dir()
            .join("auracoder-runtime-env-bin")
            .join("cli");
        let executable_parent = executable
            .parent()
            .expect("executable parent")
            .to_path_buf();
        let values = get(&executable).await;

        assert_eq!(
            values.get(OsStr::new(variable_name)),
            Some(&OsString::from("current-value"))
        );
        let path = values.get(OsStr::new("PATH")).expect("PATH should exist");
        assert_eq!(
            env::split_paths(path).next(),
            Some(executable_parent),
            "executable parent should be the first PATH entry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_opencode_env_overrides_all_special_values() {
        let _env_guard = env_lock().lock().expect("env lock poisoned");
        let _password_guard = environment_variable_guard("OPENCODE_SERVER_PASSWORD");
        let _config_guard = environment_variable_guard("OPENCODE_CONFIG_DIR");
        let _xdg_guard = environment_variable_guard("XDG_CONFIG_HOME");
        std::env::set_var("OPENCODE_SERVER_PASSWORD", "old-password");
        std::env::set_var("OPENCODE_CONFIG_DIR", "old-config");
        std::env::set_var("XDG_CONFIG_HOME", "old-xdg");

        let executable = std::env::temp_dir().join("opencode");
        let run_dir = std::env::temp_dir().join("auracoder-opencode-run");
        let values = get_opencode_env(&executable, "new-password", &run_dir).await;

        assert_eq!(
            values.get(OsStr::new("OPENCODE_SERVER_PASSWORD")),
            Some(&OsString::from("new-password"))
        );
        assert_eq!(
            values.get(OsStr::new("OPENCODE_CONFIG_DIR")),
            Some(&run_dir.join(".opencode").into_os_string())
        );
        assert_eq!(
            values.get(OsStr::new("XDG_CONFIG_HOME")),
            Some(&run_dir.clone().into_os_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_claude_env_adds_provided_values_and_preserves_defaults_for_none() {
        let _env_guard = env_lock().lock().expect("env lock poisoned");
        let _sdk_guard = environment_variable_guard("CLAUDE_AGENT_SDK_MODULE");
        let _executable_guard = environment_variable_guard("PANES_CLAUDE_CODE_EXECUTABLE");
        std::env::set_var("CLAUDE_AGENT_SDK_MODULE", "old-sdk");
        std::env::set_var("PANES_CLAUDE_CODE_EXECUTABLE", "old-executable");

        let runtime_executable = std::env::temp_dir().join("runtime");
        let claude_executable = PathBuf::from("/opt/claude/bin/claude");
        let values = get_claude_env(
            &runtime_executable,
            Some("/opt/claude/sdk"),
            Some(&claude_executable),
        )
        .await;
        assert_eq!(
            values.get(OsStr::new("CLAUDE_AGENT_SDK_MODULE")),
            Some(&OsString::from("/opt/claude/sdk"))
        );
        assert_eq!(
            values.get(OsStr::new("PANES_CLAUDE_CODE_EXECUTABLE")),
            Some(&claude_executable.clone().into_os_string())
        );

        std::env::set_var("CLAUDE_AGENT_SDK_MODULE", "parent-sdk");
        std::env::set_var("PANES_CLAUDE_CODE_EXECUTABLE", "parent-executable");
        let values = get_claude_env(&runtime_executable, None, None).await;
        assert_eq!(
            values.get(OsStr::new("CLAUDE_AGENT_SDK_MODULE")),
            Some(&OsString::from("parent-sdk"))
        );
        assert_eq!(
            values.get(OsStr::new("PANES_CLAUDE_CODE_EXECUTABLE")),
            Some(&OsString::from("parent-executable"))
        );
    }

    #[test]
    fn remote_environment_helpers_quote_and_validate_values() {
        assert_eq!(
            get_remote_opencode_env("pa'ss"),
            "export OPENCODE_SERVER_PASSWORD='pa'\\''ss';"
        );
        assert_eq!(
            get_remote_claude_env("claude_path").expect("valid shell variable"),
            "export PANES_CLAUDE_CODE_EXECUTABLE=\"${claude_path}\";"
        );
        assert!(get_remote_claude_env("a-b").is_err());
        assert!(get_remote_claude_env("").is_err());

        let codex_overrides =
            codex_mcp_gateway_config_overrides("http://127.0.0.1:30123/mcp", "AURACODER_MCP_TOKEN");
        assert_eq!(codex_overrides.len(), 2);
        assert_eq!(
            codex_overrides[0],
            "mcp_servers.auracoder.url=\"http://127.0.0.1:30123/mcp\""
        );
        assert_eq!(
            codex_overrides[1],
            "mcp_servers.auracoder.bearer_token_env_var=\"AURACODER_MCP_TOKEN\""
        );
        assert!(codex_overrides
            .iter()
            .all(|value| !value.starts_with("mcp_servers={")));
        assert!(codex_overrides
            .iter()
            .all(|value| !value.contains("ac_test_token")));

        let opencode_config = opencode_mcp_gateway_config("http://127.0.0.1:30123/mcp");
        assert_eq!(
            opencode_config["mcp"]["auracoder"]["type"],
            serde_json::Value::String("remote".to_string())
        );
        assert_eq!(
            opencode_config["mcp"]["auracoder"]["url"],
            serde_json::Value::String("http://127.0.0.1:30123/mcp".to_string())
        );
        let authenticated_opencode_config = opencode_mcp_gateway_authenticated_config(
            "http://127.0.0.1:30123/mcp",
            "Bearer ac_test_token",
        );
        assert_eq!(
            authenticated_opencode_config["mcp"]["auracoder"]["type"],
            serde_json::Value::String("remote".to_string())
        );
        assert_eq!(
            authenticated_opencode_config["mcp"]["auracoder"]["url"],
            serde_json::Value::String("http://127.0.0.1:30123/mcp".to_string())
        );
        assert_eq!(
            authenticated_opencode_config["mcp"]["auracoder"]["enabled"],
            true
        );
        assert_eq!(
            authenticated_opencode_config["mcp"]["auracoder"]["headers"]["Authorization"],
            serde_json::Value::String("Bearer ac_test_token".to_string())
        );
        assert_eq!(
            authenticated_opencode_config
                .as_object()
                .map(|value| value.len()),
            Some(1)
        );
        assert!(authenticated_opencode_config.get("oauth").is_none());
    }

    #[test]
    fn terminal_shell_args_match_shell_type() {
        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                terminal_shell_args(Path::new("cmd.exe")),
                Vec::<String>::new()
            );
            assert_eq!(
                terminal_shell_args(Path::new("pwsh.exe")),
                Vec::<String>::new()
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(
                terminal_shell_args(Path::new("/bin/bash")),
                vec!["-l".to_string(), "-i".to_string()]
            );
            assert_eq!(
                terminal_shell_args(Path::new("/bin/zsh")),
                vec!["-l".to_string(), "-i".to_string()]
            );
            assert_eq!(
                terminal_shell_args(Path::new("/bin/sh")),
                vec!["-l".to_string(), "-i".to_string()]
            );
            assert_eq!(
                terminal_shell_args(Path::new("/usr/bin/fish")),
                vec!["-l".to_string(), "-i".to_string()]
            );
        }
    }

    #[test]
    fn command_shell_args_match_shell_type() {
        assert_eq!(
            command_shell_args_for(Path::new("/bin/bash"), "echo hi"),
            vec!["-lc".to_string(), "echo hi".to_string()]
        );
        assert_eq!(
            command_shell_args_for(Path::new("/usr/bin/fish"), "echo hi"),
            vec!["-l".to_string(), "-c".to_string(), "echo hi".to_string()]
        );
        assert_eq!(
            command_shell_args_for(Path::new("/bin/sh"), "echo hi"),
            vec!["-lc".to_string(), "echo hi".to_string()]
        );
        assert_eq!(
            command_shell_args_for(Path::new("cmd.exe"), "echo hi"),
            vec!["/C".to_string(), "echo hi".to_string()]
        );
        assert_eq!(
            command_shell_args_for(Path::new("pwsh.exe"), "echo hi"),
            vec!["-Command".to_string(), "echo hi".to_string()]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn login_probe_shell_args_match_shell_type() {
        assert_eq!(
            login_probe_shell_args(Path::new("/bin/bash"), "command -v node"),
            vec![
                "-l".to_string(),
                "-i".to_string(),
                "-c".to_string(),
                "command -v node".to_string(),
            ]
        );
        assert_eq!(
            login_probe_shell_args(Path::new("/usr/bin/fish"), "command -v node"),
            vec![
                "-l".to_string(),
                "-i".to_string(),
                "-c".to_string(),
                "command -v node".to_string(),
            ]
        );
        assert_eq!(
            login_probe_shell_args(Path::new("/bin/sh"), "command -v node"),
            vec![
                "-l".to_string(),
                "-c".to_string(),
                "command -v node".to_string(),
            ]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn parse_login_probe_output_skips_banner_lines() {
        assert_eq!(
            parse_login_probe_output("Welcome to fish\n/usr/local/bin/node\nv22.0.0\n"),
            Some(("/usr/local/bin/node".to_string(), "v22.0.0".to_string()))
        );
    }

    #[test]
    fn parse_login_shell_env_output_starts_after_marker() {
        let mut output = b"profile noise\n".to_vec();
        output.extend_from_slice(LOGIN_ENV_PROBE_MARKER.as_bytes());
        output.push(0);
        output.extend_from_slice(b"OPENROUTER_API_KEY=secret-value");
        output.push(0);
        output.extend_from_slice(b"BROKEN_ENTRY");
        output.push(0);
        output.extend_from_slice(b"EMPTY=");
        output.push(0);

        let parsed = parse_login_shell_env_output(&output);

        assert_eq!(
            parsed.get(&OsString::from("OPENROUTER_API_KEY")),
            Some(&OsString::from("secret-value"))
        );
        assert_eq!(
            parsed.get(&OsString::from("EMPTY")),
            Some(&OsString::from(""))
        );
        assert!(!parsed.contains_key(&OsString::from("BROKEN_ENTRY")));
    }

    #[test]
    fn login_shell_env_import_skips_process_and_path_keys() {
        assert!(!should_import_login_shell_env_var(OsStr::new("PATH")));
        assert!(!should_import_login_shell_env_var(OsStr::new("PWD")));
        assert!(!should_import_login_shell_env_var(OsStr::new(
            "XPC_SERVICE_NAME"
        )));
        assert!(should_import_login_shell_env_var(OsStr::new(
            "OPENROUTER_API_KEY"
        )));
    }

    #[test]
    fn parse_windows_login_probe_output_skips_profile_noise() {
        assert_eq!(
            parse_windows_login_probe_output(
                "Loading profile...\nC:\\Users\\auracoder\\AppData\\Roaming\\npm\\codex.cmd\n0.42.0\n",
            ),
            Some((
                "C:\\Users\\auracoder\\AppData\\Roaming\\npm\\codex.cmd".to_string(),
                "0.42.0".to_string(),
            ))
        );
    }

    #[test]
    fn parse_windows_single_path_output_skips_profile_noise() {
        assert_eq!(
            parse_windows_single_path_output("Welcome back\nC:\\Program Files\\nodejs\\node.exe\n",),
            Some("C:\\Program Files\\nodejs\\node.exe".to_string())
        );
    }

    #[test]
    fn windows_login_probe_shells_prefer_pwsh_before_powershell() {
        let _env_guard = env_lock().lock().expect("env lock poisoned");
        let temp_dir = std::env::temp_dir().join(format!(
            "auracoder-runtime-env-pwsh-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let _path_guard = PathEnvGuard {
            original_path: std::env::var_os("PATH"),
            temp_dir: temp_dir.clone(),
        };

        #[cfg(target_os = "windows")]
        let shell_names = ["pwsh.exe", "powershell.exe"];
        #[cfg(not(target_os = "windows"))]
        let shell_names = ["pwsh", "powershell"];

        for shell in shell_names {
            let path = temp_dir.join(shell);
            std::fs::write(&path, "").expect("write shell stub");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&path, permissions).expect("set permissions");
            }
        }

        std::env::set_var("PATH", &temp_dir);
        let shells = windows_login_probe_shells();

        #[cfg(target_os = "windows")]
        {
            assert_eq!(shells[0], temp_dir.join("pwsh.exe"));
            assert!(shells.contains(&temp_dir.join("powershell.exe")));
        }

        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(shells[0], temp_dir.join("pwsh"));
            assert!(shells.contains(&temp_dir.join("powershell")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_shell_prefers_zsh_before_other_available_shells() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join(format!(
            "auracoder-runtime-env-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        for shell in ["zsh", "bash", "sh"] {
            let path = temp_dir.join(shell);
            std::fs::write(&path, "#!/bin/sh\n").expect("write shell stub");
            let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("set permissions");
        }

        let selected = command_shell_program_for(None, None, Some(temp_dir.as_os_str()));
        assert_eq!(selected, temp_dir.join("zsh"));

        std::fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[cfg(not(target_os = "windows"))]
    #[cfg(unix)]
    #[test]
    fn command_shell_prefers_shell_env_when_available() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join(format!(
            "auracoder-runtime-env-command-shell-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        for shell in ["fish", "zsh", "bash", "sh"] {
            let path = temp_dir.join(shell);
            std::fs::write(&path, "#!/bin/sh\n").expect("write shell stub");
            let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("set permissions");
        }

        let selected = command_shell_program_for(
            Some(temp_dir.join("fish").to_string_lossy().as_ref()),
            None,
            Some(temp_dir.as_os_str()),
        );
        assert_eq!(selected, temp_dir.join("fish"));
        assert_eq!(
            command_shell_args_for(&selected, "echo hi"),
            vec!["-l".to_string(), "-c".to_string(), "echo hi".to_string()]
        );

        std::fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[cfg(not(target_os = "windows"))]
    #[cfg(unix)]
    #[test]
    fn login_probe_shells_prefer_shell_env_and_include_fish_and_sh() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join(format!(
            "auracoder-runtime-env-shells-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        for shell in ["fish", "zsh", "bash", "sh"] {
            let path = temp_dir.join(shell);
            std::fs::write(&path, "#!/bin/sh\n").expect("write shell stub");
            let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("set permissions");
        }

        let shells = login_probe_shells_for(
            Some(temp_dir.join("fish").to_string_lossy().as_ref()),
            Some(temp_dir.as_os_str()),
        );

        assert_eq!(shells[0], temp_dir.join("fish"));
        assert!(shells.contains(&temp_dir.join("zsh")));
        assert!(shells.contains(&temp_dir.join("bash")));
        assert!(shells.contains(&temp_dir.join("sh")));

        std::fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn prepended_paths_stay_first() {
        let value =
            augmented_path_with_prepend([PathBuf::from("/custom/bin"), PathBuf::from("/usr/bin")])
                .expect("joined path");
        let joined = value.to_string_lossy();
        assert!(joined.starts_with("/custom/bin"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_augmented_path_includes_expected_user_bins() {
        let home = Path::new("/home/auracoder");
        let current_path = OsStr::new("/usr/bin:/bin");
        let entries = augmented_path_entries_for(Some(home), Some(current_path), None, None);

        assert!(entries.contains(&home.join(".local/share/npm/bin")));
        assert!(entries.contains(&home.join(".npm-global/bin")));
        assert!(entries.contains(&home.join(".volta/bin")));
        assert!(entries.contains(&home.join(".local/share/fnm/aliases/default/bin")));
        assert!(entries.contains(&home.join(".local/share/pnpm")));
        assert!(entries.contains(&home.join(".cargo/bin")));
        assert!(entries.contains(&home.join(".deno/bin")));
        assert!(entries.contains(&home.join(".bun/bin")));
        assert!(entries.contains(&home.join(".local/share/mise/shims")));
        assert!(entries.contains(&home.join(".proto/shims")));
        assert!(entries.contains(&home.join(".proto/bin")));
        assert!(entries.contains(&home.join(".fnm/aliases/default/bin")));
        assert!(entries.contains(&home.join("bin")));
        assert!(entries.contains(&home.join(".nix-profile/bin")));
        assert!(entries.contains(&PathBuf::from("/snap/bin")));
        assert!(entries.contains(&PathBuf::from("/home/linuxbrew/.linuxbrew/bin")));
        assert!(entries.contains(&PathBuf::from("/nix/var/nix/profiles/default/bin")));
        assert!(entries.contains(&PathBuf::from("/run/current-system/sw/bin")));
        assert!(!entries.contains(&home.join("Library/pnpm")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_terminal_shell_skips_empty_shell_env() {
        let home = Path::new("/home/auracoder");
        let shell = terminal_shell_for(Some(""), Some(home), Some(OsStr::new("/usr/bin:/bin")));
        assert_ne!(shell.as_os_str(), "");
        assert_ne!(shell, PathBuf::from("/bin/zsh"));
    }

    #[test]
    fn home_dir_from_env_uses_windows_fallbacks_when_home_is_missing() {
        let from_user_profile =
            home_dir_from_env(None, Some(OsStr::new(r"C:\Users\auracoder")), None, None)
                .expect("user profile path");
        assert_eq!(normalize_path(&from_user_profile), "C:/Users/auracoder");

        let from_home_drive = home_dir_from_env(
            None,
            None,
            Some(OsStr::new("C:")),
            Some(OsStr::new(r"\Users\auracoder")),
        )
        .expect("home drive + home path");
        let rendered = normalize_path(&from_home_drive);
        assert!(rendered.starts_with("C:"));
        assert!(rendered.ends_with("/Users/auracoder"));
    }

    #[test]
    fn app_data_dir_for_windows_prefers_local_app_data() {
        let path = app_data_dir_for(
            true,
            false,
            Some(Path::new(r"C:\Users\auracoder\AppData\Local")),
            Some(Path::new(r"C:\Users\auracoder\AppData\Roaming")),
            Some(Path::new(r"C:\Users\auracoder")),
        );
        assert_eq!(
            normalize_path(&path),
            "C:/Users/auracoder/AppData/Local/AuraCoder"
        );
    }

    #[test]
    fn app_data_dir_for_unix_uses_dot_agent_workspace() {
        let path = app_data_dir_for(false, false, None, None, Some(Path::new("/home/auracoder")));
        assert_eq!(path, PathBuf::from("/home/auracoder/.agent-workspace"));
    }

    #[test]
    fn app_data_dir_for_windows_falls_back_to_absolute_temp_dir() {
        let path = app_data_dir_for(true, false, None, None, None);
        assert_eq!(path, std::env::temp_dir().join("AuraCoder"));
        assert!(path.is_absolute());
    }

    #[test]
    fn app_data_dir_for_windows_development_uses_independent_directory() {
        let path = app_data_dir_for(
            true,
            true,
            Some(Path::new(r"C:\Users\auracoder\AppData\Local")),
            None,
            None,
        );
        assert_eq!(
            normalize_path(&path),
            "C:/Users/auracoder/AppData/Local/AuraCoder-Development"
        );
    }

    #[test]
    fn app_data_dir_for_unix_development_uses_independent_directory() {
        let path = app_data_dir_for(false, true, None, None, Some(Path::new("/home/auracoder")));
        assert_eq!(
            path,
            PathBuf::from("/home/auracoder/.agent-workspace-development")
        );
    }

    #[test]
    fn app_data_dir_for_development_fallbacks_are_isolated() {
        let unix_path = app_data_dir_for(false, true, None, None, None);
        assert_eq!(unix_path, PathBuf::from("./.agent-workspace-development"));

        let windows_path = app_data_dir_for(true, true, None, None, None);
        assert!(windows_path.is_absolute());
        assert!(windows_path.ends_with("AuraCoder-Development"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_path_from_env_file_extracts_paths() {
        let dir = std::env::temp_dir().join(format!("auracoder-env-file-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let env_file = dir.join("environment");
        let home = Path::new("/home/auracoder");

        fs::write(
            &env_file,
            "# comment\nPATH=\"/usr/local/sbin:/usr/local/bin:/usr/bin:/snap/bin\"\nLANG=en_US.UTF-8\n",
        )
        .expect("write env file");

        let paths = parse_path_from_env_file(&env_file, Some(home));
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/usr/local/sbin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/snap/bin"),
            ]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_path_from_env_file_skips_variable_references() {
        let dir = std::env::temp_dir().join(format!("auracoder-env-var-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let env_file = dir.join("environment");
        let home = Path::new("/home/auracoder");

        fs::write(&env_file, "PATH=${PATH}:/custom/bin:/other/bin\n").expect("write env file");

        let paths = parse_path_from_env_file(&env_file, Some(home));
        // ${PATH} entry should be skipped, literal paths kept
        assert!(paths.contains(&PathBuf::from("/custom/bin")));
        assert!(paths.contains(&PathBuf::from("/other/bin")));
        assert!(!paths.iter().any(|p| p.to_string_lossy().contains('$')));

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_path_from_env_file_expands_home_and_supports_spaced_assignment() {
        let dir = std::env::temp_dir().join(format!("auracoder-env-home-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let env_file = dir.join("environment");
        let home = Path::new("/home/auracoder");

        fs::write(
            &env_file,
            "PATH = \"$HOME/.local/bin:${HOME}/.cargo/bin:${PATH}:~/bin\"\n",
        )
        .expect("write env file");

        let paths = parse_path_from_env_file(&env_file, Some(home));
        assert!(paths.contains(&home.join(".local/bin")));
        assert!(paths.contains(&home.join(".cargo/bin")));
        assert!(paths.contains(&home.join("bin")));
        assert!(!paths.iter().any(|p| p.to_string_lossy().contains('$')));

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_user_environment_path_entries_reads_conf_files() {
        let dir = std::env::temp_dir().join(format!("auracoder-envd-{}", Uuid::new_v4()));
        let env_d = dir.join(".config/environment.d");
        fs::create_dir_all(&env_d).expect("create environment.d dir");

        fs::write(env_d.join("custom.conf"), "PATH=/custom/tools/bin\n").expect("write conf file");
        // non-.conf files should be ignored
        fs::write(env_d.join("readme.txt"), "PATH=/should/be/ignored\n").expect("write txt file");

        let paths = linux_user_environment_path_entries(&dir);
        assert!(paths.contains(&PathBuf::from("/custom/tools/bin")));
        assert!(!paths.contains(&PathBuf::from("/should/be/ignored")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn npm_global_bin_from_npmrc_reads_prefix() {
        let dir = std::env::temp_dir().join(format!("auracoder-npmrc-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");

        // Absolute prefix
        fs::write(dir.join(".npmrc"), "prefix=/opt/npm-global\n").expect("write npmrc");
        assert_eq!(
            npm_global_bin_from_npmrc(&dir),
            Some(PathBuf::from("/opt/npm-global/bin"))
        );

        // Tilde prefix
        fs::write(
            dir.join(".npmrc"),
            "# comment\n; another comment\nprefix=~/npm-packages\n",
        )
        .expect("write npmrc");
        assert_eq!(
            npm_global_bin_from_npmrc(&dir),
            Some(dir.join("npm-packages/bin"))
        );

        // INI-style spacing and ${HOME} interpolation
        fs::write(dir.join(".npmrc"), "prefix = ${HOME}/.npm-packages\n").expect("write npmrc");
        assert_eq!(
            npm_global_bin_from_npmrc(&dir),
            Some(dir.join(".npm-packages/bin"))
        );

        // Quoted prefix
        fs::write(dir.join(".npmrc"), "prefix=\"/quoted/path\"\n").expect("write npmrc");
        assert_eq!(
            npm_global_bin_from_npmrc(&dir),
            Some(PathBuf::from("/quoted/path/bin"))
        );

        // No prefix
        fs::write(dir.join(".npmrc"), "registry=https://registry.npmjs.org/\n")
            .expect("write npmrc");
        assert_eq!(npm_global_bin_from_npmrc(&dir), None);

        // No file
        let _ = fs::remove_file(dir.join(".npmrc"));
        assert_eq!(npm_global_bin_from_npmrc(&dir), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_legacy_app_data_dir_moves_existing_legacy_tree() {
        let root =
            std::env::temp_dir().join(format!("auracoder-app-data-migrate-{}", Uuid::new_v4()));
        let current = root.join("AppData").join("Local").join("AuraCoder");
        let legacy = root.join(".agent-workspace");

        fs::create_dir_all(legacy.join("logs")).expect("legacy app data dir should exist");
        fs::write(legacy.join("workspaces.db"), "legacy database\n")
            .expect("legacy database should be written");
        fs::write(legacy.join("logs").join("events.log"), "hello\n")
            .expect("legacy log should be written");

        migrate_legacy_app_data_dir_for(&current, Some(&legacy))
            .expect("legacy app data should migrate");

        assert!(current.join("workspaces.db").exists());
        assert!(current.join("logs").join("events.log").exists());
        assert!(!legacy.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_legacy_app_data_dir_preserves_existing_target_data() {
        let root =
            std::env::temp_dir().join(format!("auracoder-app-data-preserve-{}", Uuid::new_v4()));
        let current = root.join("AppData").join("Local").join("AuraCoder");
        let legacy = root.join(".agent-workspace");

        fs::create_dir_all(&current).expect("current app data dir should exist");
        fs::create_dir_all(&legacy).expect("legacy app data dir should exist");
        fs::write(current.join("workspaces.db"), "current database\n")
            .expect("current database should be written");
        fs::write(legacy.join("workspaces.db"), "legacy database\n")
            .expect("legacy database should be written");

        migrate_legacy_app_data_dir_for(&current, Some(&legacy))
            .expect("migration should skip populated targets");

        assert_eq!(
            fs::read_to_string(current.join("workspaces.db"))
                .expect("current database should exist"),
            "current database\n"
        );
        assert!(legacy.join("workspaces.db").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_flatpak_reflects_flatpak_id_env_var() {
        let _env_guard = env_lock().lock().expect("env lock poisoned");
        let original = std::env::var_os("FLATPAK_ID");

        std::env::remove_var("FLATPAK_ID");
        assert!(!is_flatpak());

        std::env::set_var("FLATPAK_ID", "com.auracoder.app");
        assert!(is_flatpak());

        match original {
            Some(value) => std::env::set_var("FLATPAK_ID", value),
            None => std::env::remove_var("FLATPAK_ID"),
        }
    }
