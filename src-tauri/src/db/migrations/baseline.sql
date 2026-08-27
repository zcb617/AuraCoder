CREATE TABLE IF NOT EXISTS workspaces (
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

CREATE TABLE IF NOT EXISTS threads (
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
  last_activity_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  content TEXT,
  blocks_json TEXT,
  turn_engine_id TEXT,
  remote_turn_id TEXT,
  turn_model_id TEXT,
  turn_reasoning_effort TEXT,
  schema_version INTEGER NOT NULL DEFAULT 1,
  stream_seq INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'completed',
  token_input INTEGER DEFAULT 0,
  token_output INTEGER DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS actions (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
  engine_action_id TEXT,
  action_type TEXT NOT NULL,
  summary TEXT NOT NULL,
  details_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'running',
  truncated INTEGER NOT NULL DEFAULT 0,
  result_json TEXT,
  duration_ms INTEGER,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS approvals (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
  action_type TEXT NOT NULL,
  summary TEXT NOT NULL,
  details_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  decision TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  answered_at TEXT
);

CREATE TABLE IF NOT EXISTS engine_event_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  event_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_threads_workspace ON threads(workspace_id);
CREATE INDEX IF NOT EXISTS idx_threads_activity ON threads(workspace_id, last_activity_at DESC);
CREATE INDEX IF NOT EXISTS idx_threads_workspace_status_activity
  ON threads(workspace_id, status, last_activity_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_messages_thread_status_created
  ON messages(thread_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_actions_thread ON actions(thread_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_actions_thread_status_created
  ON actions(thread_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_approvals_thread ON approvals(thread_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_approvals_message_status
  ON approvals(message_id, status, created_at ASC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_thread_remote_turn_role
  ON messages(thread_id, remote_turn_id, role)
  WHERE remote_turn_id IS NOT NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  thread_id UNINDEXED,
  role UNINDEXED,
  searchable_text,
  content=messages,
  content_rowid=rowid
);

CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, thread_id, role, searchable_text)
  VALUES (new.rowid, new.thread_id, new.role, COALESCE(new.content, ''));
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_delete BEFORE DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, thread_id, role, searchable_text)
  VALUES ('delete', old.rowid, old.thread_id, old.role, COALESCE(old.content, ''));
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, thread_id, role, searchable_text)
  VALUES ('delete', old.rowid, old.thread_id, old.role, COALESCE(old.content, ''));
  INSERT INTO messages_fts(rowid, thread_id, role, searchable_text)
  VALUES (new.rowid, new.thread_id, new.role, COALESCE(new.content, ''));
END;

CREATE TABLE IF NOT EXISTS extension_catalog_snapshots (
  provider_id TEXT NOT NULL,
  context_key TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('skill', 'plugin', 'mcp')),
  items_json TEXT NOT NULL,
  fetched_at TEXT,
  last_attempt_at TEXT,
  next_refresh_at TEXT,
  last_error TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (provider_id, context_key, kind)
);

CREATE INDEX IF NOT EXISTS idx_extension_catalog_snapshots_due
  ON extension_catalog_snapshots(next_refresh_at);

CREATE TABLE IF NOT EXISTS scheduled_tasks (
  id TEXT PRIMARY KEY,
  description TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  execution_device_id TEXT NOT NULL DEFAULT 'local',
  target_type TEXT NOT NULL,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL,
  runtime_config_json TEXT,
  schedule_type TEXT NOT NULL,
  schedule_json TEXT NOT NULL,
  timezone TEXT NOT NULL,
  next_run_at TEXT,
  last_run_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (target_type IN ('existing_thread', 'new_thread')),
  CHECK (schedule_type IN ('interval', 'daily', 'weekly'))
);

CREATE TABLE IF NOT EXISTS scheduled_task_runs (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
  scheduled_for TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT,
  thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL,
  assistant_message_id TEXT REFERENCES messages(id) ON DELETE SET NULL,
  status TEXT NOT NULL DEFAULT 'queued',
  error_message TEXT,
  result_preview TEXT,
  acknowledged_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK (status IN (
    'queued',
    'running',
    'needs_confirmation',
    'completed',
    'error',
    'interrupted',
    'skipped'
  )),
  UNIQUE(task_id, scheduled_for)
);

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
  ON scheduled_tasks(enabled, next_run_at);
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_workspace
  ON scheduled_tasks(workspace_id);
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_thread
  ON scheduled_tasks(thread_id);
CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_task_created
  ON scheduled_task_runs(task_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_message
  ON scheduled_task_runs(assistant_message_id);
