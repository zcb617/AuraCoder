use std::{path::Path, time::Instant};

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::{
    engines::EngineCheckAction,
    models::EngineCheckResultDto,
    process_utils,
    runtime_env,
};

/// 执行已由当前 CLI 健康报告许可的 shell 命令并收集有限长度输出。
pub(crate) async fn execute_shell(command: &str) -> Result<EngineCheckResultDto> {
    let started = Instant::now();

    // 使用当前平台 shell 和 runtime_env 提供的环境执行已许可命令。
    let output = build_shell_command(command)
        .await
        .output()
        .await
        .with_context(|| format!("执行诊断 shell 命令失败: `{command}`"))?;
    let duration_ms = started.elapsed().as_millis();

    // 保持命令原文、退出码、耗时和 12000 Unicode 字符截断契约。
    Ok(EngineCheckResultDto {
        command: command.to_string(),
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: truncate_output(&String::from_utf8_lossy(&output.stdout), 12_000),
        stderr: truncate_output(&String::from_utf8_lossy(&output.stderr), 12_000),
        duration_ms,
    })
}

/// 构造使用应用运行环境的当前平台 shell 命令。
#[cfg(target_os = "windows")]
async fn build_shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    process_utils::configure_tokio_command(&mut cmd);
    cmd.arg("/C").arg(command);
    cmd.envs(runtime_env::get(Path::new("cmd")).await);
    cmd
}

/// 构造使用应用运行环境的当前平台 shell 命令。
#[cfg(not(target_os = "windows"))]
async fn build_shell_command(command: &str) -> Command {
    let spec = runtime_env::command_shell_for_string(command);
    let mut cmd = Command::new(&spec.program);
    process_utils::configure_tokio_command(&mut cmd);
    cmd.args(&spec.args);
    cmd.envs(runtime_env::get(&spec.program).await);
    cmd
}

/// 按完整 Unicode 字符数量截断诊断输出并追加既有标记。
fn truncate_output(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }

    let mut output = chars.into_iter().take(max_chars).collect::<String>();
    output.push_str("\n...[truncated]");
    output
}

/// 按健康报告中的精确标签查找唯一诊断动作。
pub(crate) fn find_action(
    actions: &[EngineCheckAction],
    requested: &str,
) -> Result<EngineCheckAction> {
    anyhow::ensure!(!requested.is_empty(), "诊断动作不能为空");
    actions
        .iter()
        .find(|action| action.label() == requested)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("未找到匹配的诊断动作"))
}

/// 仅从 Shell typed 动作中取得可交给系统 shell 的命令。
pub(crate) fn shell_command_for_action(action: &EngineCheckAction) -> Result<&str> {
    match action {
        EngineCheckAction::Shell(command) => Ok(command),
        EngineCheckAction::NativeHealth(_) => anyhow::bail!("原生健康检查不能作为 shell 命令执行"),
        EngineCheckAction::Information(_) => anyhow::bail!("诊断说明不能作为 shell 命令执行"),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/engine_check_tests.rs"]
mod tests;
