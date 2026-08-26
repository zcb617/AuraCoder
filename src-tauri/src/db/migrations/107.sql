CREATE TABLE config (
    config_key TEXT PRIMARY KEY NOT NULL,
    config_value TEXT NOT NULL
);

UPDATE schema_version
SET version = 107,
    migration_file = '107.sql',
    applied_at = datetime('now')
WHERE id = 1;
