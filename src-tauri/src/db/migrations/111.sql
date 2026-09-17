-- Claude thread sync flag；启动同步仅同步会话抬头，点击会话时按需读取完整消息
-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads claude_sync_required
ALTER TABLE threads
  ADD COLUMN claude_sync_required INTEGER NOT NULL DEFAULT 0;
-- PANES-MIGRATION END

UPDATE schema_version
SET version = 111,
    migration_file = '111.sql',
    applied_at = datetime('now')
WHERE id = 1;
