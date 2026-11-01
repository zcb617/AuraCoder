
    use serde_json::json;

    use super::*;
    use crate::models::ThreadStatusDto;

    fn task(runtime_config: Option<serde_json::Value>) -> ScheduledTaskDto {
        ScheduledTaskDto {
            id: "task-1".to_string(),
            description: "Check workspace".to_string(),
            enabled: true,
            execution_device_id: "local".to_string(),
            target_type: "existing_thread".to_string(),
            workspace_id: "workspace-1".to_string(),
            thread_id: Some("thread-1".to_string()),
            runtime_config,
            schedule_type: "daily".to_string(),
            schedule: json!({ "time": "09:00" }),
            timezone: "UTC".to_string(),
            next_run_at: None,
            last_run_at: None,
            latest_run: None,
            needs_confirmation: false,
            target_valid: true,
            created_at: "2026-08-09T00:00:00Z".to_string(),
            updated_at: "2026-08-09T00:00:00Z".to_string(),
        }
    }

    fn thread() -> ThreadDto {
        ThreadDto {
            id: "thread-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            engine_id: "codex".to_string(),
            model_id: "gpt-selected".to_string(),
            engine_thread_id: None,
            engine_metadata: Some(json!({
                "serviceTier": "fast"
                // 旧六字段 metadata 夹具已停用：
                // "lastModelId": "gpt-selected",
                // "reasoningEffort": "high",
            })),
            plan_mode: None,
            send_method: None,
            reasoning_effort: Some("high".to_string()),
            permission_mode: None,
            title: "Existing".to_string(),
            status: ThreadStatusDto::Idle,
            message_count: 0,
            total_tokens: 0,
            context_current_tokens: None,
            context_max_tokens: None,
            context_usage_updated_at: None,
            created_at: "2026-08-09T00:00:00Z".to_string(),
            last_activity_at: "2026-08-09T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn existing_task_without_snapshot_uses_thread_runtime() {
        let runtime = runtime_config_for_task(&task(None), Some(&thread())).unwrap();
        assert_eq!(runtime.engine_id, "codex");
        assert_eq!(runtime.model_id, "gpt-selected");
        assert_eq!(runtime.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(runtime.service_tier.as_deref(), Some("fast"));
    }

    #[test]
    fn task_snapshot_takes_precedence_over_thread_runtime() {
        let runtime = runtime_config_for_task(
            &task(Some(json!({
                "engineId": "codex",
                "modelId": "gpt-task",
                "reasoningEffort": "xhigh"
            }))),
            Some(&thread()),
        )
        .unwrap();
        assert_eq!(runtime.model_id, "gpt-task");
        assert_eq!(runtime.reasoning_effort.as_deref(), Some("xhigh"));
    }

    /// 验证禁用任务和仍需确认的任务不会触发退出确认。
    #[test]
    fn exit_check_ignores_disabled_and_unconfirmed_tasks() {
        let mut disabled_task = task(None);
        disabled_task.enabled = false;
        let mut unconfirmed_task = task(None);
        unconfirmed_task.needs_confirmation = true;

        assert!(!has_runnable_enabled_tasks(&[
            disabled_task,
            unconfirmed_task,
        ]));
    }

    /// 验证启用且无需确认的任务会触发退出确认。
    #[test]
    fn exit_check_detects_enabled_confirmed_task() {
        assert!(has_runnable_enabled_tasks(&[task(None)]));
    }
