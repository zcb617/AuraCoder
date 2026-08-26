CREATE TABLE IF NOT EXISTS ssh_connections (
  id TEXT PRIMARY KEY,
  display_name TEXT NOT NULL,
  source_kind TEXT NOT NULL CHECK (source_kind IN ('ssh_config', 'manual')),
  config_alias TEXT,
  host_name TEXT NOT NULL,
  user_name TEXT NOT NULL,
  port INTEGER NOT NULL DEFAULT 22,
  identity_file TEXT,
  host_key_type TEXT NOT NULL,
  host_key_base64 TEXT NOT NULL,
  connection_status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (connection_status IN ('unknown', 'connecting', 'ok', 'failed', 'disabled', 'deleted')),
  enabled INTEGER NOT NULL DEFAULT 1,
  last_connected_at TEXT,
  last_error TEXT,
  deleted_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_ssh_connections_active_alias
  ON ssh_connections(config_alias)
  WHERE source_kind = 'ssh_config'
    AND config_alias IS NOT NULL
    AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_ssh_connections_active
  ON ssh_connections(deleted_at, updated_at DESC);

-- PANES-MIGRATION IF COLUMN_NOT_EXISTS ssh_connections connection_status
ALTER TABLE ssh_connections
  ADD COLUMN connection_status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (connection_status IN ('unknown', 'connecting', 'ok', 'failed', 'disabled', 'deleted'));
-- PANES-MIGRATION END

-- PANES-MIGRATION IF ANY_COLUMN_NOT_EXISTS workspaces location_kind ssh_connection_id
DROP TABLE IF EXISTS workspaces_105_new;

CREATE TABLE workspaces_105_new (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  location_kind TEXT NOT NULL DEFAULT 'local'
    CHECK (location_kind IN ('local', 'ssh')),
  ssh_connection_id TEXT REFERENCES ssh_connections(id) ON DELETE RESTRICT,
  scan_depth INTEGER NOT NULL DEFAULT 3,
  startup_preset_json TEXT,
  startup_preset_updated_at TEXT,
  archived_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  last_opened_at TEXT NOT NULL DEFAULT (datetime('now')),
  git_repo_selection_configured INTEGER NOT NULL DEFAULT 0,
  CHECK (
    (location_kind = 'local' AND ssh_connection_id IS NULL)
    OR (location_kind = 'ssh' AND ssh_connection_id IS NOT NULL)
  )
);

INSERT INTO workspaces_105_new (
  id,
  name,
  root_path,
  location_kind,
  ssh_connection_id,
  scan_depth,
  startup_preset_json,
  startup_preset_updated_at,
  archived_at,
  created_at,
  last_opened_at,
  git_repo_selection_configured
)
SELECT
  id,
  name,
  root_path,
  'local',
  NULL,
  scan_depth,
  startup_preset_json,
  startup_preset_updated_at,
  archived_at,
  created_at,
  last_opened_at,
  git_repo_selection_configured
FROM workspaces;

DROP TABLE workspaces;
ALTER TABLE workspaces_105_new RENAME TO workspaces;
-- PANES-MIGRATION END

CREATE UNIQUE INDEX IF NOT EXISTS idx_workspaces_local_root
  ON workspaces(root_path)
  WHERE location_kind = 'local';
CREATE UNIQUE INDEX IF NOT EXISTS idx_workspaces_remote_root
  ON workspaces(ssh_connection_id, root_path)
  WHERE location_kind = 'ssh';
CREATE INDEX IF NOT EXISTS idx_workspaces_ssh_connection
  ON workspaces(ssh_connection_id);

UPDATE schema_version
SET version = 105,
    migration_file = '105.sql',
    applied_at = datetime('now')
WHERE id = 1;
