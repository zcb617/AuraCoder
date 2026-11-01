use super::*;

use crate::engines::EngineCheckAction;

/// 验证精确标签匹配会拒绝空值、未知值和外部追加的命令。
#[test]
fn unknown_action_is_rejected() {
    let actions = vec![EngineCheckAction::Shell("where node".to_string())];

    assert!(find_action(&actions, "").is_err());
    assert!(find_action(&actions, "where codex").is_err());
    assert!(find_action(&actions, "where node && whoami").is_err());
    assert!(matches!(
        find_action(&actions, "where node"),
        Ok(EngineCheckAction::Shell(command)) if command == "where node"
    ));
}

/// 验证原生健康检查和说明动作无法被当作 shell 命令执行。
#[test]
fn non_shell_action_cannot_be_executed_as_shell() {
    assert_eq!(
        shell_command_for_action(&EngineCheckAction::Shell("node --version".to_string()))
            .expect("shell action should expose its command"),
        "node --version"
    );
    assert!(shell_command_for_action(&EngineCheckAction::NativeHealth(
        "GET /global/health".to_string()
    ))
    .is_err());
    assert!(shell_command_for_action(&EngineCheckAction::Information(
        "请检查网络设置".to_string()
    ))
    .is_err());
}

/// 验证输出截断按 Unicode 字符计数并保留既有标记。
#[test]
fn output_limit_preserves_unicode() {
    let short = "中文输出";
    assert_eq!(truncate_output(short, 12_000), short);

    let long = "诊断信息".repeat(3_001);
    let truncated = truncate_output(&long, 12_000);
    assert_eq!(truncated.chars().take(12_000).collect::<String>(), "诊断信息".repeat(3_000));
    assert!(truncated.ends_with("\n...[truncated]"));
}
