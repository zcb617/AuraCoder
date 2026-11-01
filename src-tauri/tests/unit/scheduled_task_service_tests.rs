use serde_json::{json, Value};

use super::{runtime_config_from_thread, validate_task_fields};
use crate::models::{ScheduledTaskInput, ThreadDto, ThreadStatusDto};

/// 验证定时任务 DTO 默认值和运行配置序列化保持前端 JSON 契约。
#[test]
fn scheduled_input_defaults_preserve_json_contract() {
    // 从省略 enabled 和 executionDeviceId 的有效 JSON 构造真实输入 DTO。
    let input: ScheduledTaskInput = serde_json::from_value(json!({
        "description": "Review workspace changes",
        "targetType": "new_thread",
        "workspaceId": "workspace-1",
        "threadId": null,
        "runtimeConfig": {
            "engineId": "codex",
            "modelId": "gpt-test",
            "reasoningEffort": "high",
            "serviceTier": "fast"
        },
        "scheduleType": "daily",
        "schedule": { "time": "09:30", "days": [1, 3] },
        "timezone": "Asia/Tokyo"
    }))
    .expect("scheduled task input JSON should deserialize");

    // 确认省略字段默认启用并使用本机执行设备。
    assert!(input.enabled);
    assert_eq!(input.execution_device_id, "local");

    // 确认运行配置字段采用 camelCase，且周期 JSON 原样保留。
    let serialized = serde_json::to_value(&input).expect("scheduled task input JSON");
    assert_eq!(serialized["schedule"], json!({ "time": "09:30", "days": [1, 3] }));
    assert_eq!(serialized["runtimeConfig"]["engineId"], "codex");
    assert_eq!(serialized["runtimeConfig"]["modelId"], "gpt-test");
    assert_eq!(serialized["runtimeConfig"]["reasoningEffort"], "high");
    assert_eq!(serialized["runtimeConfig"]["serviceTier"], "fast");
    assert!(serialized.get("runtime_config").is_none());

    let runtime: crate::models::ScheduledRuntimeConfig =
        serde_json::from_value(serialized["runtimeConfig"].clone())
            .expect("scheduled runtime config JSON");
    let runtime_json = serde_json::to_value(runtime).expect("serialized runtime config JSON");
    assert_eq!(runtime_json["engineId"], "codex");
    assert_eq!(runtime_json["modelId"], "gpt-test");
    assert_eq!(runtime_json["reasoningEffort"], "high");
    assert_eq!(runtime_json["serviceTier"], "fast");
}

/// 验证任务字段在访问数据库或 CLI 前拒绝非法值并返回规范化文本。
#[test]
fn invalid_task_fields_are_rejected_before_database() {
    // 逐项覆盖描述、执行设备、目标类型和工作区的基础输入边界。
    let mut input = valid_task_input();
    input.description = "   ".to_string();
    assert!(validate_task_fields(&input).is_err());

    let mut input = valid_task_input();
    input.description = "字".repeat(20_001);
    assert!(validate_task_fields(&input).is_err());

    let mut input = valid_task_input();
    input.execution_device_id = "remote".to_string();
    assert!(validate_task_fields(&input).is_err());

    let mut input = valid_task_input();
    input.target_type = "invalid".to_string();
    assert!(validate_task_fields(&input).is_err());

    let mut input = valid_task_input();
    input.workspace_id = "  ".to_string();
    assert!(validate_task_fields(&input).is_err());

    // 确认恰好 20000 个 Unicode 字符有效，且描述与工作区返回值已 trim。
    let mut input = valid_task_input();
    input.description = format!(" {} ", "字".repeat(20_000));
    input.workspace_id = " workspace-1 ".to_string();
    let (description, workspace_id) =
        validate_task_fields(&input).expect("valid task fields should pass");
    assert_eq!(description.chars().count(), 20_000);
    assert_eq!(workspace_id, "workspace-1");
}

/// 验证基础字段测试使用真实定时任务输入 DTO。
fn valid_task_input() -> ScheduledTaskInput {
    ScheduledTaskInput {
        description: "Review workspace changes".to_string(),
        enabled: true,
        execution_device_id: "local".to_string(),
        target_type: "new_thread".to_string(),
        workspace_id: "workspace-1".to_string(),
        thread_id: None,
        runtime_config: Some(json!({
            "engineId": "codex",
            "modelId": "gpt-test",
            "reasoningEffort": "high",
            "serviceTier": "fast"
        })),
        schedule_type: "daily".to_string(),
        schedule: json!({ "time": "09:30" }),
        timezone: "Asia/Tokyo".to_string(),
    }
}

/// 验证线程运行配置快照只包含 CLI 选择字段，并从 metadata 读取服务档位。
#[test]
fn thread_runtime_snapshot_keeps_selected_cli_fields() {
    // 从线程 metadata 读取四个任务运行字段，不扩展复制其它线程 metadata。
    let thread = thread_with_metadata(Some(json!({
        "serviceTier": "flex",
        "unrelated": "excluded"
    })));
    let runtime = runtime_config_from_thread(&thread);
    assert_eq!(runtime["engineId"], "codex");
    assert_eq!(runtime["modelId"], "gpt-selected");
    assert_eq!(runtime["reasoningEffort"], "high");
    assert_eq!(runtime["serviceTier"], "flex");
    assert_eq!(runtime.as_object().map(|fields| fields.len()), Some(4));
    assert!(runtime.get("unrelated").is_none());

    // metadata 缺失时仍输出四字段结构，并将服务档位保存为 null。
    let thread_without_metadata = thread_with_metadata(None);
    let runtime_without_metadata = runtime_config_from_thread(&thread_without_metadata);
    assert_eq!(runtime_without_metadata["serviceTier"], Value::Null);
}

/// 构造供运行配置快照检查使用的真实线程 DTO。
fn thread_with_metadata(engine_metadata: Option<serde_json::Value>) -> ThreadDto {
    ThreadDto {
        id: "thread-1".to_string(),
        workspace_id: "workspace-1".to_string(),
        engine_id: "codex".to_string(),
        model_id: "gpt-selected".to_string(),
        engine_thread_id: Some("engine-thread-1".to_string()),
        engine_metadata,
        plan_mode: None,
        send_method: None,
        reasoning_effort: Some("high".to_string()),
        permission_mode: None,
        title: "Existing thread".to_string(),
        status: ThreadStatusDto::Idle,
        message_count: 0,
        total_tokens: 0,
        context_current_tokens: None,
        context_max_tokens: None,
        context_usage_updated_at: None,
        created_at: "2026-11-01T00:00:00Z".to_string(),
        last_activity_at: "2026-11-01T00:00:00Z".to_string(),
    }
}
