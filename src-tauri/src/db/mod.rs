use std::{
    fs,
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::runtime_env;

pub mod actions;
pub mod extensions;
pub mod messages;
pub mod migrations;
pub mod repos;
pub mod scheduled_tasks;
pub mod ssh_connections;
pub mod threads;
pub mod workspaces;

const SQLITE_POOL_MAX_IDLE: usize = 8;

#[derive(Debug)]
pub struct UnsupportedDatabaseVersion {
    pub current_version: u64,
    pub supported_version: u64,
}

impl std::fmt::Display for UnsupportedDatabaseVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "database version {} is newer than supported version {}",
            self.current_version, self.supported_version
        )
    }
}

impl std::error::Error for UnsupportedDatabaseVersion {}

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
    pool: Arc<ConnectionPool>,
}

struct ConnectionPool {
    idle: Mutex<Vec<Connection>>,
    max_idle: usize,
}

pub struct PooledConnection {
    conn: Option<Connection>,
    pool: Arc<ConnectionPool>,
}

impl Deref for PooledConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("pooled sqlite connection missing inner value")
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn
            .as_mut()
            .expect("pooled sqlite connection missing inner value")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };

        let mut idle = match self.pool.idle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if idle.len() < self.pool.max_idle {
            idle.push(conn);
        }
    }
}

impl Database {
    pub fn init() -> anyhow::Result<Self> {
        runtime_env::migrate_legacy_app_data_dir()
            .context("failed to migrate legacy app data dir")?;
        let base_dir = runtime_env::app_data_dir();
        fs::create_dir_all(base_dir.join("logs")).context("failed to create app data dir")?;

        let path = base_dir.join("workspaces.db");
        Self::open(path)
    }

    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let db = Self {
            path,
            pool: Arc::new(ConnectionPool {
                idle: Mutex::new(Vec::new()),
                max_idle: SQLITE_POOL_MAX_IDLE,
            }),
        };
        db.run_migrations()?;

        Ok(db)
    }

    pub fn connect(&self) -> anyhow::Result<PooledConnection> {
        if let Some(conn) = self.take_idle_connection() {
            return Ok(PooledConnection {
                conn: Some(conn),
                pool: self.pool.clone(),
            });
        }

        let conn = Connection::open(&self.path).context("failed to open sqlite database")?;
        configure_connection(&conn)?;
        Ok(PooledConnection {
            conn: Some(conn),
            pool: self.pool.clone(),
        })
    }

    fn take_idle_connection(&self) -> Option<Connection> {
        let mut idle = match self.pool.idle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        idle.pop()
    }

    fn run_migrations(&self) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let had_existing_schema = has_user_tables(&conn)?;
        if !had_existing_schema {
            self.initialize_baseline(&mut conn)?;
        }

        if !table_exists(&conn, "schema_version")? {
            let baseline = migrations::MIGRATIONS
                .first()
                .ok_or_else(|| anyhow::anyhow!("migration list is empty"))?;
            self.run_migration(&mut conn, baseline, had_existing_schema)?;
        }

        let current_version = read_schema_version(&conn)?
            .ok_or_else(|| anyhow::anyhow!("schema_version row is missing"))?;
        let target_version = migrations::SUPPORTED_DATABASE_VERSION;
        let target_index = migrations::MIGRATIONS
            .iter()
            .position(|migration| migration.version == target_version)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "supported database version {} is not present in the migration list",
                    target_version
                )
            })?;
        if current_version > target_version {
            return Err(UnsupportedDatabaseVersion {
                current_version,
                supported_version: target_version,
            }
            .into());
        }

        let current_index = migrations::MIGRATIONS
            .iter()
            .position(|migration| migration.version == current_version)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "database schema version {} is not present in the migration list",
                    current_version
                )
            })?;

        for migration in migrations::MIGRATIONS
            .iter()
            .take(target_index + 1)
            .skip(current_index + 1)
        {
            self.run_migration(&mut conn, migration, true)?;
        }

        let completed_version = read_schema_version(&conn)?
            .ok_or_else(|| anyhow::anyhow!("schema_version row is missing after migrations"))?;
        if completed_version != target_version {
            anyhow::bail!(
                "database migrations stopped at {}, expected {}",
                completed_version,
                target_version
            );
        }
        Ok(())
    }

    fn initialize_baseline(&self, conn: &mut Connection) -> anyhow::Result<()> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin baseline schema initialization")?;
        tx.execute_batch(include_str!("migrations/baseline.sql"))
            .context("failed to initialize baseline schema")?;
        tx.commit()
            .context("failed to commit baseline schema initialization")?;
        Ok(())
    }

    fn run_migration(
        &self,
        conn: &mut Connection,
        migration: &migrations::Migration,
        should_backup: bool,
    ) -> anyhow::Result<()> {
        if should_backup {
            self.backup_database(conn, migration.version, migration.reason)?;
        }

        if migration.requires_foreign_keys_off {
            conn.execute_batch("PRAGMA foreign_keys = OFF;")
                .context("failed to disable sqlite foreign keys for migration")?;
        }

        let migration_result = (|| -> anyhow::Result<()> {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .with_context(|| format!("failed to begin migration {}", migration.version))?;
            const CONDITIONAL_START: &str = "-- PANES-MIGRATION IF COLUMN_NOT_EXISTS ";
            const ANY_CONDITIONAL_START: &str = "-- PANES-MIGRATION IF ANY_COLUMN_NOT_EXISTS ";
            const CONDITIONAL_END: &str = "-- PANES-MIGRATION END";

            let mut remaining = migration.sql;
            loop {
                let column_conditional = remaining
                    .find(CONDITIONAL_START)
                    .map(|index| (index, CONDITIONAL_START));
                let any_column_conditional = remaining
                    .find(ANY_CONDITIONAL_START)
                    .map(|index| (index, ANY_CONDITIONAL_START));
                let Some((marker_index, marker)) = [column_conditional, any_column_conditional]
                    .into_iter()
                    .flatten()
                    .min_by_key(|(index, _)| *index)
                else {
                    break;
                };

                tx.execute_batch(&remaining[..marker_index])
                    .with_context(|| format!("failed to apply {}", migration.file))?;

                let after_start = &remaining[marker_index + marker.len()..];
                let (condition, after_condition) =
                    after_start.split_once('\n').ok_or_else(|| {
                        anyhow::anyhow!("invalid conditional block in {}", migration.file)
                    })?;
                let mut fields = condition.split_whitespace();
                let table = fields.next().ok_or_else(|| {
                    anyhow::anyhow!("conditional block missing table in {}", migration.file)
                })?;
                let columns = fields.collect::<Vec<_>>();
                if columns.is_empty() {
                    anyhow::bail!("conditional block missing column in {}", migration.file);
                }
                if marker == CONDITIONAL_START && columns.len() != 1 {
                    anyhow::bail!("invalid conditional block in {}", migration.file);
                }

                let (conditional_sql, after_conditional) =
                    after_condition.split_once(CONDITIONAL_END).ok_or_else(|| {
                        anyhow::anyhow!(
                            "conditional block missing end marker in {}",
                            migration.file
                        )
                    })?;
                let should_execute = if marker == CONDITIONAL_START {
                    !table_column_exists(&tx, table, columns[0])?
                } else {
                    let mut missing_column = false;
                    for column in &columns {
                        if !table_column_exists(&tx, table, column)? {
                            missing_column = true;
                            break;
                        }
                    }
                    missing_column
                };
                if should_execute {
                    tx.execute_batch(conditional_sql).with_context(|| {
                        format!(
                            "failed to apply conditional {}.{} change in {}",
                            table,
                            columns.join(","),
                            migration.file
                        )
                    })?;
                }
                remaining = after_conditional;
            }

            tx.execute_batch(remaining)
                .with_context(|| format!("failed to finish {}", migration.file))?;
            tx.commit()
                .with_context(|| format!("failed to commit {}", migration.file))?;
            Ok(())
        })();

        let restore_foreign_keys_result = if migration.requires_foreign_keys_off {
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .context("failed to restore sqlite foreign keys after migration")
        } else {
            Ok(())
        };
        migration_result?;
        restore_foreign_keys_result?;

        let completed_version = read_schema_version(conn)?
            .ok_or_else(|| anyhow::anyhow!("schema_version row is missing after migration"))?;
        if completed_version != migration.version {
            anyhow::bail!(
                "{} did not update schema_version to {}",
                migration.file,
                migration.version
            );
        }
        Ok(())
    }

    fn backup_database(&self, conn: &Connection, version: u64, reason: &str) -> anyhow::Result<()> {
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f");
        let database_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspaces.db");
        let backup_path = self.path.with_file_name(format!(
            "{database_name}.backup-before-db-migration-{version}-{reason}-{timestamp}.db"
        ));
        let backup_target = backup_path.to_string_lossy().into_owned();
        conn.execute("VACUUM INTO ?1", params![backup_target])
            .with_context(|| {
                format!(
                    "failed to back up database before migration {version} ({reason}) to {}",
                    backup_path.display()
                )
            })?;
        Ok(())
    }
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |row| row.get::<_, i32>(0),
    )
    .optional()
    .map(|value| value.is_some())
    .with_context(|| format!("failed inspect whether {table} table exists"))
}

fn table_column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let query = format!(
        "SELECT 1 FROM pragma_table_info('{}') WHERE name = ?1 LIMIT 1",
        table.replace('\'', "''")
    );
    conn.query_row(&query, params![column], |row| row.get::<_, i32>(0))
        .optional()
        .map(|value| value.is_some())
        .with_context(|| format!("failed inspect whether {table}.{column} column exists"))
}

fn has_user_tables(conn: &Connection) -> anyhow::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_master
           WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         )",
        [],
        |row| row.get::<_, i32>(0),
    )
    .map(|value| value != 0)
    .context("failed inspect whether database has user tables")
}

fn read_schema_version(conn: &Connection) -> anyhow::Result<Option<u64>> {
    let raw_version = conn
        .query_row(
            "SELECT version FROM schema_version WHERE id = 1",
            [],
            |row| row.get::<_, u64>(0),
        )
        .optional()
        .context("failed read database schema version")?;
    Ok(raw_version)
}

fn configure_connection(conn: &Connection) -> anyhow::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable sqlite foreign keys")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable sqlite WAL mode")?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .context("failed to set sqlite synchronous mode")?;
    conn.pragma_update(None, "temp_store", "MEMORY")
        .context("failed to set sqlite temp_store mode")?;
    conn.busy_timeout(Duration::from_millis(5_000))
        .context("failed to set sqlite busy timeout")?;
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    fn test_db() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-db-migrations-{}.db", Uuid::new_v4()));
        let db = Database {
            path,
            pool: Arc::new(ConnectionPool {
                idle: Mutex::new(Vec::new()),
                max_idle: SQLITE_POOL_MAX_IDLE,
            }),
        };
        db.run_migrations()
            .expect("failed to initialize test database");
        db
    }

    #[test]
    fn baseline_and_migrations_record_the_integer_version() {
        let db = test_db();
        let conn = Connection::open(&db.path).expect("failed to open migrated test database");
        let version = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get::<_, u64>(0),
            )
            .expect("failed to read schema version");
        assert_eq!(version, migrations::SUPPORTED_DATABASE_VERSION);

        let workspace_columns = conn
            .prepare("PRAGMA table_info(workspaces)")
            .expect("failed to inspect workspace schema")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("failed to read workspace schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to decode workspace schema");
        assert!(workspace_columns.iter().any(|name| name == "location_kind"));

        let ssh_columns = conn
            .prepare("PRAGMA table_info(ssh_connections)")
            .expect("failed to inspect SSH schema")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("failed to read SSH schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to decode SSH schema");
        assert!(ssh_columns.iter().any(|name| name == "connection_status"));
    }

    #[test]
    fn migration_105_accepts_development_versions_and_preserves_data() {
        for version in [101_u64, 102, 103] {
            let path = std::env::temp_dir().join(format!(
                "auracoder-db-migration-105-from-{version}-{}.db",
                Uuid::new_v4()
            ));
            let db = Database {
                path,
                pool: Arc::new(ConnectionPool {
                    idle: Mutex::new(Vec::new()),
                    max_idle: SQLITE_POOL_MAX_IDLE,
                }),
            };
            let conn = Connection::open(&db.path).expect("failed to create development database");
            configure_connection(&conn).expect("failed to configure development database");
            conn.execute_batch(include_str!("migrations/baseline.sql"))
                .expect("failed to initialize baseline schema");
            conn.execute_batch(include_str!("migrations/100.sql"))
                .expect("failed to initialize baseline version");
            conn.execute_batch(
                "CREATE TABLE ssh_connections (
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
                    enabled INTEGER NOT NULL DEFAULT 1,
                    last_connected_at TEXT,
                    last_error TEXT,
                    deleted_at TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO ssh_connections (
                    id, display_name, source_kind, host_name, user_name,
                    host_key_type, host_key_base64
                ) VALUES (
                    'ssh-1', 'existing ssh', 'manual', 'example.com', 'user',
                    'ssh-ed25519', 'abc'
                );",
            )
            .expect("failed to initialize version 101 SSH schema");

            if version >= 102 {
                conn.execute_batch(
                    "CREATE TABLE workspaces_new (
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
                    DROP TABLE workspaces;
                    ALTER TABLE workspaces_new RENAME TO workspaces;
                    CREATE UNIQUE INDEX idx_workspaces_local_root
                        ON workspaces(root_path) WHERE location_kind = 'local';
                    CREATE UNIQUE INDEX idx_workspaces_remote_root
                        ON workspaces(ssh_connection_id, root_path) WHERE location_kind = 'ssh';
                    CREATE INDEX idx_workspaces_ssh_connection
                        ON workspaces(ssh_connection_id);",
                )
                .expect("failed to initialize version 102 workspace schema");
                conn.execute(
                    "INSERT INTO workspaces (
                        id, name, root_path, location_kind, ssh_connection_id
                    ) VALUES ('workspace-1', 'existing remote', '/srv/project', 'ssh', 'ssh-1')",
                    [],
                )
                .expect("failed to insert existing remote workspace");
            } else {
                conn.execute(
                    "INSERT INTO workspaces (id, name, root_path)
                     VALUES ('workspace-1', 'existing local', 'D:/project')",
                    [],
                )
                .expect("failed to insert existing local workspace");
            }

            if version >= 103 {
                conn.execute_batch(
                    "ALTER TABLE ssh_connections
                     ADD COLUMN connection_status TEXT NOT NULL DEFAULT 'unknown'
                     CHECK (connection_status IN (
                         'unknown', 'connecting', 'ok', 'failed', 'disabled', 'deleted'
                     ));",
                )
                .expect("failed to initialize version 103 SSH schema");
            }
            conn.execute(
                "UPDATE schema_version
                 SET version = ?1, migration_file = ?2, applied_at = datetime('now')
                 WHERE id = 1",
                params![version, format!("{version}.sql")],
            )
            .expect("failed to record development database version");
            drop(conn);

            db.run_migrations()
                .expect("105.sql should accept the development database version");
            let conn = Connection::open(&db.path).expect("failed to reopen upgraded database");
            let upgraded_version = read_schema_version(&conn)
                .expect("failed to read upgraded version")
                .expect("upgraded version row should exist");
            assert_eq!(upgraded_version, migrations::SUPPORTED_DATABASE_VERSION);
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM ssh_connections WHERE id = 'ssh-1'",
                    [],
                    |row| row.get::<_, u64>(0),
                )
                .expect("failed to check existing SSH connection"),
                1
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM workspaces WHERE id = 'workspace-1'",
                    [],
                    |row| row.get::<_, u64>(0),
                )
                .expect("failed to check existing workspace"),
                1
            );
            if version >= 102 {
                assert_eq!(
                    conn.query_row(
                        "SELECT location_kind || ':' || ssh_connection_id
                         FROM workspaces WHERE id = 'workspace-1'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("failed to check existing remote workspace"),
                    "ssh:ssh-1"
                );
            }
        }
    }

    #[test]
    fn migrations_are_idempotent_after_the_target_version() {
        let db = test_db();
        db.run_migrations()
            .expect("running migrations a second time should succeed");
        let conn = Connection::open(&db.path).expect("failed to reopen migrated database");
        let version = conn
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get::<_, u64>(0),
            )
            .expect("failed to read schema version after rerun");
        assert_eq!(version, migrations::SUPPORTED_DATABASE_VERSION);
    }

    #[test]
    fn every_migration_file_updates_the_schema_version_it_declares() {
        for migration in migrations::MIGRATIONS {
            assert!(migration.sql.contains("schema_version"));
            assert!(migration.sql.contains(&migration.version.to_string()));
            assert!(migration.sql.contains(migration.file));
        }
    }

    #[test]
    fn version_100_only_bootstraps_the_version_table() {
        let sql = migrations::MIGRATIONS
            .iter()
            .find(|migration| migration.version == migrations::BASELINE_VERSION)
            .expect("baseline migration should be registered")
            .sql;
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS schema_version"));
        assert!(sql.contains("INSERT INTO schema_version"));
        assert!(!sql.contains("ALTER TABLE"));
        assert!(!sql.contains("UPDATE schema_version"));
    }

    #[test]
    fn migration_list_starts_at_the_declared_baseline() {
        assert_eq!(
            migrations::MIGRATIONS[0].version,
            migrations::BASELINE_VERSION
        );
    }
}
