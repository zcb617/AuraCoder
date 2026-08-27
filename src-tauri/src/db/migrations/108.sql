DROP TABLE IF EXISTS workspaces_108_new;
CREATE TABLE workspaces_108_new (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  location_kind TEXT NOT NULL DEFAULT 'local'
    CHECK (location_kind IN ('local', 'ssh')),
  ssh_connection_id TEXT REFERENCES ssh_connections(id) ON DELETE RESTRICT,
  trust_level TEXT NOT NULL DEFAULT 'standard'
    CHECK (trust_level IN ('trusted', 'standard', 'restricted')),
  startup_preset_json TEXT,
  startup_preset_updated_at TEXT,
  archived_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  last_opened_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (
    (location_kind = 'local' AND ssh_connection_id IS NULL)
    OR (location_kind = 'ssh' AND ssh_connection_id IS NOT NULL)
  )
);
WITH cleaned_workspaces AS (
  SELECT id, name, root_path, location_kind, ssh_connection_id,
         startup_preset_json, startup_preset_updated_at, archived_at,
         created_at, last_opened_at
  FROM workspaces
), normalized_presets AS (
  SELECT id, name, root_path, location_kind, ssh_connection_id,
         CASE
           WHEN json_valid(startup_preset_json) THEN
             json_remove(startup_preset_json, '$.worktree')
           ELSE startup_preset_json
         END AS preset_json,
         startup_preset_updated_at, archived_at, created_at, last_opened_at
  FROM cleaned_workspaces
), cleaned_presets AS (
  SELECT id, name, root_path, location_kind, ssh_connection_id,
         CASE
           WHEN json_valid(preset_json) THEN
             CASE
               WHEN json_type(preset_json, '$.terminal.groups') = 'array' THEN
                 json_set(
                   preset_json,
                   '$.terminal.groups',
                   (
                     SELECT json_group_array(json(json_remove(group_entry.value, '$.worktree')))
                     FROM json_each(json_extract(preset_json, '$.terminal.groups')) AS group_entry
                   )
                 )
               ELSE preset_json
             END
           ELSE preset_json
         END AS startup_preset_json,
         startup_preset_updated_at, archived_at, created_at, last_opened_at
  FROM normalized_presets
)
INSERT INTO workspaces_108_new (
  id, name, root_path, location_kind, ssh_connection_id, trust_level,
  startup_preset_json, startup_preset_updated_at, archived_at, created_at, last_opened_at
)
SELECT id, name, root_path, location_kind, ssh_connection_id, 'standard',
       startup_preset_json, startup_preset_updated_at, archived_at, created_at, last_opened_at
FROM cleaned_presets;
DROP TABLE workspaces;
ALTER TABLE workspaces_108_new RENAME TO workspaces;
CREATE UNIQUE INDEX idx_workspaces_local_root
  ON workspaces(root_path) WHERE location_kind = 'local';
CREATE UNIQUE INDEX idx_workspaces_remote_root
  ON workspaces(ssh_connection_id, root_path) WHERE location_kind = 'ssh';
CREATE INDEX idx_workspaces_ssh_connection ON workspaces(ssh_connection_id);

DROP TABLE IF EXISTS threads_108_new;
CREATE TABLE threads_108_new (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  engine_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  engine_thread_id TEXT,
  engine_metadata_json TEXT,
  engine_capabilities_json TEXT,
  title TEXT,
  status TEXT NOT NULL DEFAULT 'idle',
  archived_at TEXT,
  message_count INTEGER NOT NULL DEFAULT 0,
  total_tokens INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
  plan_mode INTEGER,
  send_method TEXT,
  reasoning_effort TEXT,
  permission_mode TEXT
);
INSERT INTO threads_108_new (
  id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
  engine_capabilities_json, title, status, archived_at, message_count, total_tokens,
  created_at, last_activity_at, plan_mode, send_method, reasoning_effort, permission_mode
)
SELECT id, workspace_id, engine_id, model_id, engine_thread_id, engine_metadata_json,
       engine_capabilities_json, title, status, archived_at, message_count, total_tokens,
       created_at, last_activity_at, plan_mode, send_method, reasoning_effort, permission_mode
FROM threads;
DROP TABLE threads;
ALTER TABLE threads_108_new RENAME TO threads;
CREATE INDEX idx_threads_workspace ON threads(workspace_id);
CREATE INDEX idx_threads_activity ON threads(workspace_id, last_activity_at DESC);
CREATE INDEX idx_threads_workspace_status_activity
  ON threads(workspace_id, status, last_activity_at DESC);

DROP TABLE IF EXISTS repos;

UPDATE scheduled_tasks
SET runtime_config_json = CASE
  WHEN json_valid(runtime_config_json)
    THEN json_remove(runtime_config_json, '$.repoId', '$.workspaceWritableRoots', '$.workspaceWriteOptIn')
  ELSE runtime_config_json
END
WHERE runtime_config_json IS NOT NULL;

UPDATE schema_version
SET version = 108,
    migration_file = '108.sql',
    applied_at = datetime('now')
WHERE id = 1;
