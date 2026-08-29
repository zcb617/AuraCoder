use std::collections::BTreeMap;

use anyhow::Context;
use tokio::process::Child;
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    time::{timeout, Duration},
};

use crate::{
    db::ssh_connections::SshConnectionRecord,
    models::SshConnectionTestDto,
    // process_utils, runtime_env,
    process_utils,
    runtime_env,
    // ssh::{known_hosts, runtime::quote_posix},
    ssh::{
        known_hosts,
        runtime::{quote_posix, wrap_remote_login_shell_command},
    },
};

// const PROBE: &str = "printf '__PANES_OS__%s\\n' \"$(uname -s)\"; printf '__PANES_HOME__%s\\n' \"$HOME\"; printf '__PANES_SHELL__%s\\n' \"$SHELL\"; if command -v git >/dev/null 2>&1; then printf '__PANES_GIT__%s\\n' \"$(git --version)\"; else printf '__PANES_GIT__missing\\n'; fi; for c in codex claude gemini agy kiro-cli opencode kilo droid; do if command -v \"$c\" >/dev/null 2>&1; then printf '__PANES_CLI__%s=%s\\n' \"$c\" \"$($c --version 2>/dev/null | head -n 1)\"; fi; done";
const PROBE: &str = "printf '__PANES_OS__%s\\n' \"$(uname -s)\"; printf '__PANES_HOME__%s\\n' \"$HOME\"; printf '__PANES_SHELL__%s\\n' \"$SHELL\"; if command -v git >/dev/null 2>&1; then printf '__PANES_GIT__%s\\n' \"$(git --version)\"; else printf '__PANES_GIT__missing\\n'; fi; for c in codex claude gemini agy kiro-cli opencode kilo droid; do version=\"$(env \"$c\" --version 2>/dev/null | head -n 1)\"; if [ -n \"$version\" ]; then printf '__PANES_CLI__%s=%s\\n' \"$c\" \"$version\"; fi; done";

/// 启动统一的 SSH 本地端口转发。远端项目阶段只需提供远端服务端口，
/// 不需要为每一种 CLI 另写连接协议。
#[allow(dead_code)]
pub async fn open_tunnel(
    record: &SshConnectionRecord,
    local_port: u16,
    remote_host: &str,
    remote_port: u16,
    mcp_reverse_port: u16,
    mcp_gateway_port: u16,
) -> anyhow::Result<Child> {
    let mut command = build_session_command(record)?;
    command
        .args(["-L"])
        .arg(format!(
            "127.0.0.1:{local_port}:{remote_host}:{remote_port}"
        ))
        .args(["-R"])
        .arg(format!(
            "127.0.0.1:{mcp_reverse_port}:127.0.0.1:{mcp_gateway_port}"
        ))
        .args([
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=1",
        ])
        .arg(session_target(record))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    Ok(command.spawn()?)
}

/// 启动用于连接状态监听的通用 SSH 会话。
///
/// 该会话只使用标准 SSH 保活参数，不依赖远端 CLI、HTTP 接口或自定义服务。
pub async fn open_monitor_session(record: &SshConnectionRecord) -> anyhow::Result<(Child, u16)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_port = listener.local_addr()?.port();
    drop(listener);
    let local_forward = format!("127.0.0.1:{local_port}");

    let mut command = build_session_command(record)?;
    command
        .args([
            "-D",
            &local_forward,
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=1",
        ])
        .arg(session_target(record))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    Ok((command.spawn()?, local_port))
}

fn expand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = runtime_env::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    value.to_string()
}

fn build_session_command(record: &SshConnectionRecord) -> anyhow::Result<Command> {
    let Some(ssh) = runtime_env::resolve_executable("ssh") else {
        anyhow::bail!("未找到本机 ssh 命令")
    };
    let mut command = Command::new(ssh);
    process_utils::configure_tokio_command(&mut command);
    command.args([
        "-N",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "NumberOfPasswordPrompts=0",
        "-o",
        "PasswordAuthentication=no",
        "-o",
        "KbdInteractiveAuthentication=no",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=yes",
    ]);
    if record.dto.source_kind == "manual" {
        let known_hosts = known_hosts::path_for(&record.dto.id);
        command
            .args(["-o", "IdentitiesOnly=yes", "-o"])
            .arg(format!("UserKnownHostsFile={}", known_hosts.display()));
        if let Some(identity) = record.dto.identity_file.as_deref() {
            command.args(["-i", &expand_home(identity)]);
        }
        command.args(["-p", &record.dto.port.to_string()]);
    }
    Ok(command)
}

fn session_target(record: &SshConnectionRecord) -> String {
    if record.dto.source_kind == "ssh_config" {
        record
            .dto
            .config_alias
            .clone()
            .unwrap_or_else(|| format!("{}@{}", record.dto.user, record.dto.host_name))
    } else {
        format!("{}@{}", record.dto.user, record.dto.host_name)
    }
}

pub fn build_pty_command(
    record: &SshConnectionRecord,
    cwd: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    let Some(ssh) = runtime_env::resolve_executable("ssh") else {
        anyhow::bail!("未找到本机 ssh 命令");
    };
    let mut args = vec![
        "-tt".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "NumberOfPasswordPrompts=0".to_string(),
        "-o".to_string(),
        "PasswordAuthentication=no".to_string(),
        "-o".to_string(),
        "KbdInteractiveAuthentication=no".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=yes".to_string(),
    ];
    if record.dto.source_kind == "manual" {
        let known_hosts = known_hosts::path_for(&record.dto.id);
        args.extend([
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
        ]);
        if let Some(identity) = record.dto.identity_file.as_deref() {
            args.extend(["-i".to_string(), expand_home(identity)]);
        }
        args.extend(["-p".to_string(), record.dto.port.to_string()]);
    }
    args.push(session_target(record));
    args.push(format!("cd -- {} && exec \"$SHELL\" -l", quote_posix(cwd)));
    Ok((ssh.to_string_lossy().to_string(), args))
}

pub async fn test(record: &SshConnectionRecord) -> SshConnectionTestDto {
    let id = record.dto.id.clone();
    let mut result = SshConnectionTestDto {
        connection_id: id,
        ok: false,
        os: None,
        home: None,
        shell: None,
        git_version: None,
        cli_versions: BTreeMap::new(),
        error: None,
    };
    let mut command = match build_exec_command(record) {
        Ok(command) => command,
        Err(error) => {
            result.error = Some(error.to_string());
            return result;
        }
    };
    command
        // .arg(PROBE)
        .arg(wrap_remote_login_shell_command(PROBE))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = match timeout(Duration::from_secs(20), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            result.error = Some(error.to_string());
            return result;
        }
        Err(_) => {
            result.error = Some("SSH 连接检测超时".to_string());
            return result;
        }
    };
    if !output.status.success() {
        result.error = Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
        return result;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(value) = line.strip_prefix("__PANES_OS__") {
            result.os = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("__PANES_HOME__") {
            result.home = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("__PANES_SHELL__") {
            result.shell = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("__PANES_GIT__") {
            result.git_version = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("__PANES_CLI__") {
            if let Some((name, version)) = value.split_once('=') {
                result
                    .cli_versions
                    .insert(name.to_string(), version.to_string());
            }
        }
    }
    result.ok = result.os.as_deref() == Some("Linux");
    if !result.ok && result.error.is_none() {
        result.error = Some("远端不是受支持的 Linux 系统".to_string());
    }
    result
}

fn build_exec_command(record: &SshConnectionRecord) -> anyhow::Result<Command> {
    let Some(ssh) = runtime_env::resolve_executable("ssh") else {
        anyhow::bail!("未找到本机 ssh 命令");
    };
    let mut command = Command::new(ssh);
    process_utils::configure_tokio_command(&mut command);
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "NumberOfPasswordPrompts=0",
        "-o",
        "PasswordAuthentication=no",
        "-o",
        "KbdInteractiveAuthentication=no",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=yes",
    ]);
    let target = if record.dto.source_kind == "ssh_config" {
        record
            .dto
            .config_alias
            .clone()
            .unwrap_or_else(|| format!("{}@{}", record.dto.user, record.dto.host_name))
    } else {
        let known_hosts = known_hosts::path_for(&record.dto.id);
        command
            .args(["-o", "IdentitiesOnly=yes", "-o"])
            .arg(format!("UserKnownHostsFile={}", known_hosts.display()));
        if let Some(identity) = record.dto.identity_file.as_deref() {
            command.args(["-i", &expand_home(identity)]);
        }
        command.args(["-p", &record.dto.port.to_string()]);
        format!("{}@{}", record.dto.user, record.dto.host_name)
    };
    command.arg(target);
    Ok(command)
}

pub async fn run_command(
    record: &SshConnectionRecord,
    remote_command: &str,
) -> anyhow::Result<String> {
    let output = run_command_with_input(record, remote_command, &[]).await?;
    Ok(String::from_utf8_lossy(&output).to_string())
}

/// 通过标准 SSH exec 通道执行命令，并把输入写入远端命令的标准输入。
///
/// 远端文件读写、Git 命令和其他工作区操作都复用这个通道，不依赖远端
/// HTTP 服务、自定义守护进程或某一种 CLI。
pub async fn run_command_with_input(
    record: &SshConnectionRecord,
    remote_command: &str,
    input: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut command = build_exec_command(record)?;
    command
        .arg(remote_command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("启动 SSH 远端命令失败")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .await
            .context("写入 SSH 远端命令输入失败")?;
        stdin
            .shutdown()
            .await
            .context("关闭 SSH 远端命令输入失败")?;
    }
    let output = timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .context("SSH 远端命令执行超时")??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(if stderr.is_empty() {
            "SSH 远端命令执行失败".to_string()
        } else {
            stderr
        });
    }
    Ok(output.stdout)
}
