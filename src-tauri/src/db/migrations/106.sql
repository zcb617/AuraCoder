-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads plan_mode
ALTER TABLE threads
  ADD COLUMN plan_mode INTEGER;
-- PANES-MIGRATION END

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads send_method
ALTER TABLE threads
  ADD COLUMN send_method TEXT;
-- PANES-MIGRATION END

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads reasoning_effort
ALTER TABLE threads
  ADD COLUMN reasoning_effort TEXT;
-- PANES-MIGRATION END

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS threads permission_mode
ALTER TABLE threads
  ADD COLUMN permission_mode TEXT;
-- PANES-MIGRATION END

UPDATE schema_version
SET version = 106,
    migration_file = '106.sql',
    applied_at = datetime('now')
WHERE id = 1;
