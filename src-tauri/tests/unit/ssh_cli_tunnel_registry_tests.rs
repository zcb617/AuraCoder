
    use super::*;
    use crate::models::SshConnectionDto;

    fn tunnel(connection_id: &str, cli_id: &str, local_port: u16) -> SshCliTunnel {
        SshCliTunnel {
            connection: SshConnectionRecord {
                dto: SshConnectionDto {
                    id: connection_id.to_string(),
                    display_name: connection_id.to_string(),
                    source_kind: "manual".to_string(),
                    config_alias: None,
                    host_name: "127.0.0.1".to_string(),
                    user: "tester".to_string(),
                    port: 22,
                    identity_file: None,
                    host_key_type: "ssh-ed25519".to_string(),
                    enabled: true,
                    connection_status: "ok".to_string(),
                    last_connected_at: None,
                    last_error: None,
                    deleted_at: None,
                    created_at: "2026-01-01 00:00:00.000".to_string(),
                    updated_at: "2026-01-01 00:00:00.000".to_string(),
                },
                host_key_base64: String::new(),
            },
            connection_id: connection_id.to_string(),
            cli_id: cli_id.to_string(),
            local_port,
            remote_port: local_port,
            mcp_reverse_port: 30123,
            remote_service_secret: (cli_id == "opencode").then(|| "secret".to_string()),
            process: Mutex::new(None),
            service_lifecycle: Mutex::new(RemoteCliServiceLifecycle::default()),
            // service_runtime: Mutex::new(None),
        }
    }

    /*
    #[tokio::test]
    async fn invalidating_service_runtime_removes_the_cached_instance() {
        let tunnel = tunnel("host-a", "claude", 43200);
        *tunnel.service_runtime.lock().await = Some(RemoteCliRuntimeCache {
            service_generation: 1,
            runtime: Arc::new(String::from("claude-runtime")),
        });

        assert!(tunnel.invalidate_service_runtime().await);
        assert!(tunnel.service_runtime.lock().await.is_none());
        assert!(!tunnel.invalidate_service_runtime().await);
    }
    */

    #[test]
    fn remote_service_pid_tracks_the_login_shell_that_execs_the_cli() {
        let tunnel = tunnel("host-a", "codex", 43100);
        let command =
            build_remote_service_start_command(&tunnel, "test-mcp-token").expect("start command");

        assert!(command.contains("nohup \"${SHELL:-/bin/sh}\" -lic"));
        assert!(!command.contains("nohup sh -lc"));
    }

    #[tokio::test]
    async fn stopped_remote_service_requires_an_mcp_lease_token() {
        let tunnel = tunnel("host-a", "codex", 43100);
        let mut lifecycle = RemoteCliServiceLifecycle::default();

        let error = ensure_remote_cli_service_running(&tunnel, &mut lifecycle, None)
            .await
            .expect_err("stopped service must reject startup without an MCP lease token");

        assert!(error
            .to_string()
            .contains("必须由 SSH CLI 服务生命周期签发 MCP 租约后启动"));
        assert_eq!(lifecycle.service_state, RemoteCliServiceState::Stopped);
        assert_eq!(lifecycle.resident_use_count, 0);
    }

    #[test]
    fn temporary_and_persistent_uses_must_both_be_released_before_service_can_close() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 0,
            temporary_use_count: 1,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 1)]),
        };

        assert!(!lifecycle.has_no_active_uses());
        lifecycle.temporary_use_count -= 1;
        assert!(!lifecycle.has_no_active_uses());
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(lifecycle.has_no_active_uses());
    }

    #[test]
    fn resident_use_must_be_released_before_service_can_close() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 1,
            temporary_use_count: 1,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 1)]),
        };

        lifecycle.temporary_use_count -= 1;
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(!lifecycle.has_no_active_uses());

        lifecycle.resident_use_count -= 1;
        assert!(lifecycle.has_no_active_uses());
    }

    #[test]
    fn persistent_session_use_is_reference_counted_by_thread_id() {
        let mut lifecycle = RemoteCliServiceLifecycle {
            service_state: RemoteCliServiceState::Running,
            service_generation: 1,
            resident_use_count: 0,
            temporary_use_count: 0,
            persistent_session_uses: HashMap::from([(String::from("thread-a"), 2)]),
        };

        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert_eq!(lifecycle.persistent_session_uses["thread-a"], 1);
        assert!(lifecycle.release_persistent_session_use("thread-a"));
        assert!(lifecycle.persistent_session_uses.is_empty());
        assert!(!lifecycle.release_persistent_session_use("thread-a"));
    }

    #[tokio::test]
    async fn add_keeps_existing_tunnel() {
        let registry = SshCliTunnelRegistry::default();
        let first = registry.add(tunnel("host-a", "codex", 41001)).await;
        let second = registry.add(tunnel("host-a", "codex", 41002)).await;

        assert!(matches!(first, AddSshCliTunnelResult::Added(_)));
        let AddSshCliTunnelResult::Existing(existing) = second else {
            panic!("第二次添加必须返回已有隧道");
        };
        assert_eq!(existing.local_port(), 41001);
        assert_eq!(existing.mcp_reverse_port(), 30123);
        assert!((30000..=60000).contains(&existing.mcp_reverse_port()));
        assert_eq!(registry.list_by_host("host-a").await.len(), 1);
    }

    #[tokio::test]
    async fn list_by_host_only_returns_requested_host() {
        let registry = SshCliTunnelRegistry::default();
        registry.add(tunnel("host-a", "codex", 41001)).await;
        registry.add(tunnel("host-a", "opencode", 41002)).await;
        registry.add(tunnel("host-b", "claude", 42001)).await;

        let host_a = registry.list_by_host("host-a").await;
        assert_eq!(host_a.len(), 2);
        assert_eq!(host_a["codex"].local_port(), 41001);
        assert_eq!(host_a["opencode"].local_port(), 41002);
    }

    #[tokio::test]
    async fn remove_only_closes_requested_tunnel() {
        let registry = SshCliTunnelRegistry::default();
        registry.add(tunnel("host-a", "codex", 41001)).await;
        registry.add(tunnel("host-a", "opencode", 41002)).await;

        assert!(registry.remove("host-a", "codex").await);
        assert!(registry.get("host-a", "codex").await.is_none());
        assert!(registry.get("host-a", "opencode").await.is_some());
        assert!(!registry.remove("host-a", "claude").await);
    }

    #[test]
    fn build_codex_remote_service_start_command_uses_remote_port() {
        let command =
            build_remote_service_start_command(&tunnel("host-a", "codex", 41001), "test-mcp-token")
                .unwrap();
        assert!(command.contains("codex app-server --listen ws://127.0.0.1:41001"));
        assert!(command.contains("\"${SHELL:-/bin/sh}\" -lic"));
        assert!(command.contains("exec env codex app-server"));
        assert!(command.contains(" -c "));
        assert!(command.contains("mcp_servers.auracoder.url=\"http://127.0.0.1:30123/mcp\""));
        assert!(
            command.contains("mcp_servers.auracoder.bearer_token_env_var=\"AURACODER_MCP_TOKEN\"")
        );
        assert!(command.contains("AURACODER_MCP_TOKEN="));
        // 旧断言允许复用任意存活的 Codex PID，无法保证旧服务端口与新 Tunnel 一致：
        // assert!(command.contains("exit 0;"));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
        assert!(!command.contains("exit 0;"));
    }

    #[test]
    fn build_opencode_remote_service_start_command_uses_password_and_remote_port() {
        let command = build_remote_service_start_command(
            &tunnel("host-a", "opencode", 41002),
            "test-mcp-token",
        )
        .unwrap();
        let opencode_env = runtime_env::get_remote_opencode_env("");
        let opencode_env_prefix = opencode_env
            .split('=')
            .next()
            .expect("OpenCode environment prefix");
        assert!(command.contains(opencode_env_prefix));
        assert!(command.contains("opencode serve --hostname 127.0.0.1 --port 41002"));
        assert!(command.contains("\"${SHELL:-/bin/sh}\" -lic"));
        assert!(command.contains("exec env opencode serve"));
        // 旧断言验证隔离用户全局配置目录的行为，禁止恢复：
        // assert!(command.contains("OPENCODE_CONFIG_DIR"));
        // assert!(command.contains("XDG_CONFIG_HOME"));
        assert!(command.contains("OPENCODE_CONFIG="));
        assert!(command.contains("opencode.auracoder.json"));
        assert!(command.contains("> \"$OPENCODE_CONFIG\""));
        assert!(!command.contains("export OPENCODE_CONFIG_DIR="));
        assert!(!command.contains("export XDG_CONFIG_HOME="));
        // 旧断言检查隔离目录中的默认文件名，禁止恢复：
        // assert!(command.contains("opencode.json"));
        assert!(command.contains("\"type\":\"remote\""));
        assert!(command.contains("http://127.0.0.1:30123/mcp"));
        assert!(command.contains("\"Authorization\":\"Bearer test-mcp-token\""));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
        assert!(!command.contains("exit 0;"));
    }

    #[test]
    fn build_claude_remote_service_start_command_uses_session_adapter_and_remote_port() {
        let command = build_remote_service_start_command(
            &tunnel("host-a", "claude", 41003),
            "test-mcp-token",
        )
        .unwrap();
        assert!(command.contains("claude-remote-session-server.mjs"));
        assert!(command.contains("$HOME/.cache/auracoder/runtime/claude/"));
        assert!(!command.contains("'$HOME/.cache/auracoder/runtime/claude/"));
        assert!(command.contains("--host 127.0.0.1 --port 41003"));
        assert!(command.contains("--mcp-gateway-url"));
        assert!(command.contains("http://127.0.0.1:30123/mcp"));
        assert!(command.contains("AURACODER_MCP_TOKEN="));
        assert!(!command.contains("--mcp-gateway-token"));
        assert!(command.contains("type -P claude"));
        let claude_env =
            runtime_env::get_remote_claude_env("claude_path").expect("Claude environment prefix");
        let claude_env_prefix = claude_env
            .split('=')
            .next()
            .expect("Claude environment prefix");
        assert!(command.contains(claude_env_prefix));
        assert!(command.contains("pid=$(cat \"$pid_file\"); kill \"$pid\""));
    }

    #[test]
    fn claude_remote_service_waits_for_agent_health_instead_of_only_the_port() {
        let command = build_remote_service_wait_command(&tunnel("host-a", "claude", 41003));

        assert!(command.contains("/health"));
        assert!(command.contains("response.ok"));
        assert!(command.contains("seq 1 300"));
        assert!(command.contains("service.pid"));
        assert!(command.contains("service.log"));
        assert!(command.starts_with("\"${SHELL:-/bin/sh}\" -lic "));
        assert!(command.contains("type -P node"));
        assert!(command.contains("\"$node_path\" -e"));
    }

    #[test]
    fn codex_remote_service_wait_still_checks_the_listening_port() {
        let command = build_remote_service_wait_command(&tunnel("host-a", "codex", 41001));

        assert!(command.contains("ss -ltn"));
        assert!(!command.contains("/health"));
    }

    #[test]
    fn preferred_remote_port_covers_supported_cli_defaults() {
        assert_eq!(preferred_remote_port("codex"), Some(43_100));
        assert_eq!(preferred_remote_port("opencode"), Some(43_600));
        assert_eq!(preferred_remote_port("claude"), Some(43_200));
    }

    #[test]
    fn preferred_remote_port_ignores_unknown_cli() {
        assert_eq!(preferred_remote_port("unknown-cli"), None);
    }

    #[test]
    fn claude_runtime_remote_root_uses_app_version_directory() {
        let remote_root = claude_runtime_remote_root();
        assert!(remote_root.contains("/runtime/claude/"));
        assert!(remote_root.contains(&format!(
            "/{}-",
            env!(concat!("CAR", "\u{0047}", "O_PKG_VERSION"))
        )));
    }
