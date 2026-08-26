CREATE TABLE IF NOT EXISTS schema_version (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version >= 100),
  migration_file TEXT NOT NULL,
  applied_at TEXT NOT NULL
);

INSERT INTO schema_version (id, version, migration_file, applied_at)
VALUES (1, 100, '100.sql', datetime('now'));
