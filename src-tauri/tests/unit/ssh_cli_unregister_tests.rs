use super::*;
use crate::models::SshConnectionDto;

/// 构造不启动真实 SSH 子进程的隧道，用于验证本地注销资源路径。
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
    }
}

/// 注销一个 SSH 连接的 CLI 隧道时保留另一个连接的独立登记。
#[tokio::test]
async fn remove_after_unregister_removes_only_requested_cli() {
    let target_connection_id = Uuid::new_v4().to_string();
    let other_connection_id = Uuid::new_v4().to_string();

    // 以无 SSH 进程的隧道验证目标隔离和关闭路径不会执行远端命令。
    SSH_CLI_TUNNELS
        .add(tunnel(&target_connection_id, "codex", 43_100))
        .await;
    SSH_CLI_TUNNELS
        .add(tunnel(&other_connection_id, "opencode", 43_600))
        .await;

    assert!(remove_after_unregister(&target_connection_id, "codex").await);
    assert!(get(&target_connection_id, "codex").await.is_none());
    assert!(get(&other_connection_id, "opencode").await.is_some());

    // 清理隔离断言保留的第二条测试隧道，避免影响其他测试。
    assert!(remove_after_unregister(&other_connection_id, "opencode").await);
}

/// 没有服务或隧道登记时重复注销仍成功且不会发起远端请求。
#[tokio::test]
async fn repeated_unregister_without_service_is_success() {
    let connection_id = Uuid::new_v4().to_string();

    // 连续通过真实生命周期入口注销不存在的连接和 CLI，验证幂等行为。
    assert!(crate::ssh::cli_service_lifecycle::unregister_service(&connection_id, "codex")
        .await
        .is_ok());
    assert!(crate::ssh::cli_service_lifecycle::unregister_service(&connection_id, "codex")
        .await
        .is_ok());
}
