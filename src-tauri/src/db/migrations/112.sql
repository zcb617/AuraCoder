-- 将数据库历史时间统一转换为运行电脑系统本地时区的 RFC3339 毫秒字符串。
-- run_migration 会在同一事务中执行本文件；验证表保证非法非空时间导致事务失败并回滚。
CREATE TEMP TABLE migration_112_time_validation (
  value TEXT NOT NULL CHECK (datetime(value) IS NOT NULL)
);

INSERT INTO migration_112_time_validation (value)
SELECT created_at FROM workspaces
UNION ALL SELECT last_opened_at FROM workspaces
UNION ALL SELECT archived_at FROM workspaces WHERE archived_at IS NOT NULL
UNION ALL SELECT startup_preset_updated_at FROM workspaces WHERE startup_preset_updated_at IS NOT NULL
UNION ALL SELECT created_at FROM threads
UNION ALL SELECT last_activity_at FROM threads
UNION ALL SELECT archived_at FROM threads WHERE archived_at IS NOT NULL
UNION ALL SELECT context_usage_updated_at FROM threads WHERE context_usage_updated_at IS NOT NULL
UNION ALL SELECT created_at FROM messages
UNION ALL SELECT created_at FROM actions
UNION ALL SELECT created_at FROM approvals
UNION ALL SELECT answered_at FROM approvals WHERE answered_at IS NOT NULL
UNION ALL SELECT created_at FROM engine_event_logs
UNION ALL SELECT next_run_at FROM scheduled_tasks WHERE next_run_at IS NOT NULL
UNION ALL SELECT last_run_at FROM scheduled_tasks WHERE last_run_at IS NOT NULL
UNION ALL SELECT created_at FROM scheduled_tasks
UNION ALL SELECT updated_at FROM scheduled_tasks
UNION ALL SELECT scheduled_for FROM scheduled_task_runs
UNION ALL SELECT started_at FROM scheduled_task_runs WHERE started_at IS NOT NULL
UNION ALL SELECT finished_at FROM scheduled_task_runs WHERE finished_at IS NOT NULL
UNION ALL SELECT acknowledged_at FROM scheduled_task_runs WHERE acknowledged_at IS NOT NULL
UNION ALL SELECT created_at FROM scheduled_task_runs
UNION ALL SELECT fetched_at FROM extension_catalog_snapshots WHERE fetched_at IS NOT NULL
UNION ALL SELECT last_attempt_at FROM extension_catalog_snapshots WHERE last_attempt_at IS NOT NULL
UNION ALL SELECT next_refresh_at FROM extension_catalog_snapshots WHERE next_refresh_at IS NOT NULL
UNION ALL SELECT created_at FROM ssh_connections
UNION ALL SELECT updated_at FROM ssh_connections
UNION ALL SELECT last_connected_at FROM ssh_connections WHERE last_connected_at IS NOT NULL
UNION ALL SELECT deleted_at FROM ssh_connections WHERE deleted_at IS NOT NULL
UNION ALL SELECT applied_at FROM schema_version;

UPDATE workspaces
SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER)),
    last_opened_at = strftime('%Y-%m-%dT%H:%M:%f', last_opened_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', last_opened_at, 'localtime') - strftime('%s', last_opened_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', last_opened_at, 'localtime') - strftime('%s', last_opened_at)) % 3600 / 60 AS INTEGER)),
    archived_at = CASE WHEN archived_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', archived_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', archived_at, 'localtime') - strftime('%s', archived_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', archived_at, 'localtime') - strftime('%s', archived_at)) % 3600 / 60 AS INTEGER)) END,
    startup_preset_updated_at = CASE WHEN startup_preset_updated_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', startup_preset_updated_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', startup_preset_updated_at, 'localtime') - strftime('%s', startup_preset_updated_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', startup_preset_updated_at, 'localtime') - strftime('%s', startup_preset_updated_at)) % 3600 / 60 AS INTEGER)) END;

UPDATE threads
SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER)),
    last_activity_at = strftime('%Y-%m-%dT%H:%M:%f', last_activity_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', last_activity_at, 'localtime') - strftime('%s', last_activity_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', last_activity_at, 'localtime') - strftime('%s', last_activity_at)) % 3600 / 60 AS INTEGER)),
    archived_at = CASE WHEN archived_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', archived_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', archived_at, 'localtime') - strftime('%s', archived_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', archived_at, 'localtime') - strftime('%s', archived_at)) % 3600 / 60 AS INTEGER)) END,
    context_usage_updated_at = CASE WHEN context_usage_updated_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', context_usage_updated_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', context_usage_updated_at, 'localtime') - strftime('%s', context_usage_updated_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', context_usage_updated_at, 'localtime') - strftime('%s', context_usage_updated_at)) % 3600 / 60 AS INTEGER)) END;

UPDATE messages SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER));
UPDATE actions SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER));
UPDATE approvals
SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER)),
    answered_at = CASE WHEN answered_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', answered_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', answered_at, 'localtime') - strftime('%s', answered_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', answered_at, 'localtime') - strftime('%s', answered_at)) % 3600 / 60 AS INTEGER)) END;
UPDATE engine_event_logs SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER));

UPDATE scheduled_tasks
SET next_run_at = CASE WHEN next_run_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', next_run_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', next_run_at, 'localtime') - strftime('%s', next_run_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', next_run_at, 'localtime') - strftime('%s', next_run_at)) % 3600 / 60 AS INTEGER)) END,
    last_run_at = CASE WHEN last_run_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', last_run_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', last_run_at, 'localtime') - strftime('%s', last_run_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', last_run_at, 'localtime') - strftime('%s', last_run_at)) % 3600 / 60 AS INTEGER)) END,
    created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER)),
    updated_at = strftime('%Y-%m-%dT%H:%M:%f', updated_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', updated_at, 'localtime') - strftime('%s', updated_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', updated_at, 'localtime') - strftime('%s', updated_at)) % 3600 / 60 AS INTEGER));

UPDATE scheduled_task_runs
SET scheduled_for = strftime('%Y-%m-%dT%H:%M:%f', scheduled_for, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', scheduled_for, 'localtime') - strftime('%s', scheduled_for)) / 3600 AS INTEGER), CAST(abs(strftime('%s', scheduled_for, 'localtime') - strftime('%s', scheduled_for)) % 3600 / 60 AS INTEGER)),
    started_at = CASE WHEN started_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', started_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', started_at, 'localtime') - strftime('%s', started_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', started_at, 'localtime') - strftime('%s', started_at)) % 3600 / 60 AS INTEGER)) END,
    finished_at = CASE WHEN finished_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', finished_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', finished_at, 'localtime') - strftime('%s', finished_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', finished_at, 'localtime') - strftime('%s', finished_at)) % 3600 / 60 AS INTEGER)) END,
    acknowledged_at = CASE WHEN acknowledged_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', acknowledged_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', acknowledged_at, 'localtime') - strftime('%s', acknowledged_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', acknowledged_at, 'localtime') - strftime('%s', acknowledged_at)) % 3600 / 60 AS INTEGER)) END,
    created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER));

UPDATE extension_catalog_snapshots
SET fetched_at = CASE WHEN fetched_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', fetched_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', fetched_at, 'localtime') - strftime('%s', fetched_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', fetched_at, 'localtime') - strftime('%s', fetched_at)) % 3600 / 60 AS INTEGER)) END,
    last_attempt_at = CASE WHEN last_attempt_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', last_attempt_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', last_attempt_at, 'localtime') - strftime('%s', last_attempt_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', last_attempt_at, 'localtime') - strftime('%s', last_attempt_at)) % 3600 / 60 AS INTEGER)) END,
    next_refresh_at = CASE WHEN next_refresh_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', next_refresh_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', next_refresh_at, 'localtime') - strftime('%s', next_refresh_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', next_refresh_at, 'localtime') - strftime('%s', next_refresh_at)) % 3600 / 60 AS INTEGER)) END;

UPDATE ssh_connections
SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', created_at, 'localtime') - strftime('%s', created_at)) % 3600 / 60 AS INTEGER)),
    updated_at = strftime('%Y-%m-%dT%H:%M:%f', updated_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', updated_at, 'localtime') - strftime('%s', updated_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', updated_at, 'localtime') - strftime('%s', updated_at)) % 3600 / 60 AS INTEGER)),
    last_connected_at = CASE WHEN last_connected_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', last_connected_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', last_connected_at, 'localtime') - strftime('%s', last_connected_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', last_connected_at, 'localtime') - strftime('%s', last_connected_at)) % 3600 / 60 AS INTEGER)) END,
    deleted_at = CASE WHEN deleted_at IS NULL THEN NULL ELSE strftime('%Y-%m-%dT%H:%M:%f', deleted_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', deleted_at, 'localtime') - strftime('%s', deleted_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', deleted_at, 'localtime') - strftime('%s', deleted_at)) % 3600 / 60 AS INTEGER)) END;

UPDATE schema_version
SET applied_at = strftime('%Y-%m-%dT%H:%M:%f', applied_at, 'localtime') || printf('%+03d:%02d', CAST((strftime('%s', applied_at, 'localtime') - strftime('%s', applied_at)) / 3600 AS INTEGER), CAST(abs(strftime('%s', applied_at, 'localtime') - strftime('%s', applied_at)) % 3600 / 60 AS INTEGER)),
    version = 112,
    migration_file = '112.sql';

DROP TABLE migration_112_time_validation;
