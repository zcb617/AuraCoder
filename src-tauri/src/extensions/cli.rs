use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::{process::Command, time::timeout};

use crate::{process_utils, runtime_env};

const CATALOG_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_CHARS: usize = 2_000;

pub async fn run_json(binary: &str, args: &[String], cwd: Option<&str>) -> Result<Value> {
    let stdout = run(binary, args, cwd, CATALOG_TIMEOUT).await?;
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("failed to parse structured output from {binary}"))
}

pub async fn run_text(binary: &str, args: &[String], cwd: Option<&str>) -> Result<String> {
    run(binary, args, cwd, CATALOG_TIMEOUT).await
}

pub async fn run_action(binary: &str, args: &[String], cwd: Option<&str>) -> Result<()> {
    let _ = run(binary, args, cwd, ACTION_TIMEOUT).await?;
    Ok(())
}

async fn run(
    binary: &str,
    args: &[String],
    cwd: Option<&str>,
    duration: Duration,
) -> Result<String> {
    let executable = runtime_env::resolve_executable(binary)
        .ok_or_else(|| anyhow::anyhow!("{binary} is not installed or not available in PATH"))?;

    let mut command = Command::new(&executable);
    process_utils::configure_tokio_command(&mut command);
    // 旧登录 Shell 环境导入由 runtime_env::get 接替：
    // runtime_env::apply_missing_login_shell_env(&mut command).await;
    command.args(args).kill_on_drop(true);

    if let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) {
        let cwd_path = Path::new(cwd);
        if cwd_path.is_dir() {
            command.current_dir(cwd_path);
        }
    }

    // 旧手工 PATH 处理由 runtime_env::get 接替：
    // if let Some(path) = runtime_env::augmented_path_with_prepend(
    //     executable
    //         .parent()
    //         .into_iter()
    //         .map(|value| value.to_path_buf()),
    // ) {
    //     command.env("PATH", path);
    // }
    command.envs(runtime_env::get(&executable).await);

    let output = timeout(duration, command.output())
        .await
        .with_context(|| format!("timed out running {binary}"))?
        .with_context(|| format!("failed to run {binary}"))?;

    if !output.status.success() {
        let detail = sanitize_error(&String::from_utf8_lossy(&output.stderr));
        if detail.is_empty() {
            anyhow::bail!(
                "{binary} command failed with status {:?}",
                output.status.code()
            );
        }
        anyhow::bail!(
            "{binary} command failed with status {:?}: {detail}",
            output.status.code()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sanitize_error(value: &str) -> String {
    let mut output = String::new();
    for line in value.lines() {
        let normalized = line.to_ascii_lowercase();
        if [
            "authorization",
            "api_key",
            "apikey",
            "cookie",
            "secret",
            "token",
            "header",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
        {
            continue;
        }

        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(line.trim());
        if output.chars().count() >= MAX_ERROR_CHARS {
            break;
        }
    }

    output.chars().take(MAX_ERROR_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_error;

    #[test]
    fn sensitive_error_lines_are_removed() {
        let value = "ordinary failure\nAuthorization: Bearer secret\nAPI_KEY=value\nretry later";
        assert_eq!(sanitize_error(value), "ordinary failure\nretry later");
    }

    #[test]
    fn errors_are_bounded() {
        let value = "x".repeat(3_000);
        assert_eq!(sanitize_error(&value).chars().count(), 2_000);
    }
}
