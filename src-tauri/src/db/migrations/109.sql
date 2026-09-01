-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads context_current_tokens
ALTER TABLE threads
  ADD COLUMN context_current_tokens INTEGER;
-- PANES-MIGRATION END

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads context_max_tokens
ALTER TABLE threads
  ADD COLUMN context_max_tokens INTEGER;
-- PANES-MIGRATION END

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads context_usage_updated_at
ALTER TABLE threads
  ADD COLUMN context_usage_updated_at TEXT;
-- PANES-MIGRATION END

UPDATE schema_version
SET version = 109,
    migration_file = '109.sql',
    applied_at = datetime('now')
WHERE id = 1;
