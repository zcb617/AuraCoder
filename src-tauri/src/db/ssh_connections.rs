use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{SshConnectionDto, SshConnectionInput};

use super::Database;

pub const STATUS_UNKNOWN: &str = "unknown";
pub const STATUS_CONNECTING: &str = "connecting";
pub const STATUS_OK: &str = "ok";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_DISABLED: &str = "disabled";
pub const STATUS_DELETED: &str = "deleted";

const SELECT_COLUMNS: &str = "id, display_name, source_kind, config_alias, host_name, user_name, port, identity_file, host_key_type, enabled, connection_status, last_connected_at, last_error, deleted_at, created_at, updated_at, host_key_base64";

#[derive(Debug, Clone)]
pub struct SshConnectionRecord {
    pub dto: SshConnectionDto,
    pub host_key_base64: String,
}

pub fn list(db: &Database, deleted: bool) -> anyhow::Result<Vec<SshConnectionDto>> {
    let conn = db.connect()?;
    let query = format!(
        "SELECT {SELECT_COLUMNS}
         FROM ssh_connections WHERE (deleted_at IS NOT NULL) = ?1
         ORDER BY updated_at DESC, display_name COLLATE NOCASE"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(params![deleted], map_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("failed to list ssh connections")
}

pub fn list_records(db: &Database, deleted: bool) -> anyhow::Result<Vec<SshConnectionRecord>> {
    let conn = db.connect()?;
    let query = format!(
        "SELECT {SELECT_COLUMNS}
         FROM ssh_connections WHERE (deleted_at IS NOT NULL) = ?1
         ORDER BY updated_at DESC, display_name COLLATE NOCASE"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(params![deleted], map_record)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("failed to list ssh connection records")
}

pub fn find(db: &Database, id: &str) -> anyhow::Result<Option<SshConnectionRecord>> {
    let conn = db.connect()?;
    find_with_conn(&conn, id)
}

pub fn find_active_by_alias(
    db: &Database,
    alias: &str,
) -> anyhow::Result<Option<SshConnectionRecord>> {
    let conn = db.connect()?;
    let query = format!(
        "SELECT {SELECT_COLUMNS} FROM ssh_connections
         WHERE config_alias = ?1 AND deleted_at IS NULL"
    );
    let mut stmt = conn.prepare(&query)?;
    stmt.query_row(params![alias], map_record)
        .optional()
        .context("failed to find ssh config connection")
}

pub fn find_deleted_by_alias(
    db: &Database,
    alias: &str,
) -> anyhow::Result<Option<SshConnectionRecord>> {
    let conn = db.connect()?;
    let query = format!(
        "SELECT {SELECT_COLUMNS} FROM ssh_connections
         WHERE config_alias = ?1 AND deleted_at IS NOT NULL"
    );
    let mut stmt = conn.prepare(&query)?;
    stmt.query_row(params![alias], map_record)
        .optional()
        .context("failed to find deleted ssh config connection")
}

pub fn insert(
    db: &Database,
    id: &str,
    source_kind: &str,
    input: &SshConnectionInput,
    key_type: &str,
    key_base64: &str,
) -> anyhow::Result<SshConnectionDto> {
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO ssh_connections (
           id, display_name, source_kind, config_alias, host_name, user_name, port,
           identity_file, host_key_type, host_key_base64, connection_status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            input.display_name.trim(),
            source_kind,
            input.config_alias,
            input.host_name.trim(),
            input.user.trim(),
            input.port as i64,
            input.identity_file,
            key_type,
            key_base64,
            STATUS_UNKNOWN,
        ],
    )
    .context("failed to insert ssh connection")?;
    find_with_conn(&conn, id)?
        .map(|r| r.dto)
        .context("inserted ssh connection not found")
}

pub fn update(
    db: &Database,
    id: &str,
    input: &SshConnectionInput,
    key_type: &str,
    key_base64: &str,
) -> anyhow::Result<SshConnectionDto> {
    let conn = db.connect()?;
    let changed = conn
        .execute(
            "UPDATE ssh_connections SET
               display_name=?2, config_alias=?3, host_name=?4, user_name=?5, port=?6,
               identity_file=?7, host_key_type=?8, host_key_base64=?9,
               connection_status=?10, last_connected_at=NULL, last_error=NULL,
               updated_at=strftime('%Y-%m-%d %H:%M:%f', 'now')
             WHERE id=?1 AND deleted_at IS NULL",
            params![
                id,
                input.display_name.trim(),
                input.config_alias,
                input.host_name.trim(),
                input.user.trim(),
                input.port as i64,
                input.identity_file,
                key_type,
                key_base64,
                STATUS_UNKNOWN,
            ],
        )
        .context("failed to update ssh connection")?;
    if changed == 0 {
        anyhow::bail!("ssh connection not found or deleted: {id}");
    }
    find_with_conn(&conn, id)?
        .map(|r| r.dto)
        .context("updated ssh connection not found")
}

pub fn set_enabled(db: &Database, id: &str, enabled: bool) -> anyhow::Result<SshConnectionDto> {
    let conn = db.connect()?;
    let status = if enabled {
        STATUS_UNKNOWN
    } else {
        STATUS_DISABLED
    };
    let changed = conn
        .execute(
            "UPDATE ssh_connections SET
               enabled=?2, connection_status=?3,
               last_error=NULL,
               updated_at=strftime('%Y-%m-%d %H:%M:%f', 'now')
             WHERE id=?1 AND deleted_at IS NULL",
            params![id, enabled, status],
        )
        .context("failed to update ssh connection state")?;
    if changed == 0 {
        anyhow::bail!("ssh connection not found: {id}");
    }
    find_with_conn(&conn, id)?
        .map(|r| r.dto)
        .context("ssh connection not found")
}

pub fn record_test(
    db: &Database,
    id: &str,
    expected_updated_at: &str,
    ok: bool,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let conn = db.connect()?;
    if ok {
        conn.execute(
            "UPDATE ssh_connections SET
               connection_status=?2, last_connected_at=strftime('%Y-%m-%d %H:%M:%f', 'now'),
               last_error=NULL
             WHERE id=?1 AND updated_at=?3",
            params![id, STATUS_OK, expected_updated_at],
        )?;
    } else {
        conn.execute(
            "UPDATE ssh_connections SET connection_status=?2, last_error=?3
             WHERE id=?1 AND updated_at=?4",
            params![id, STATUS_FAILED, error, expected_updated_at],
        )?;
    }
    Ok(())
}

pub fn set_status_if_current(
    db: &Database,
    id: &str,
    expected_updated_at: &str,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<bool> {
    let conn = db.connect()?;
    let changed = match status {
        STATUS_OK => conn.execute(
            "UPDATE ssh_connections SET
               connection_status=?3, last_connected_at=strftime('%Y-%m-%d %H:%M:%f', 'now'),
               last_error=NULL
             WHERE id=?1 AND updated_at=?2 AND enabled=1 AND deleted_at IS NULL",
            params![id, expected_updated_at, status],
        )?,
        STATUS_CONNECTING => conn.execute(
            "UPDATE ssh_connections SET connection_status=?3
             WHERE id=?1 AND updated_at=?2 AND enabled=1 AND deleted_at IS NULL",
            params![id, expected_updated_at, status],
        )?,
        STATUS_FAILED => conn.execute(
            "UPDATE ssh_connections SET connection_status=?3, last_error=?4
             WHERE id=?1 AND updated_at=?2 AND enabled=1 AND deleted_at IS NULL",
            params![id, expected_updated_at, status, error],
        )?,
        _ => anyhow::bail!("unsupported monitored SSH connection status: {status}"),
    };
    Ok(changed > 0)
}

pub fn soft_delete(db: &Database, id: &str) -> anyhow::Result<()> {
    let conn = db.connect()?;
    let changed = conn.execute(
        "UPDATE ssh_connections SET
           deleted_at=strftime('%Y-%m-%d %H:%M:%f', 'now'), connection_status=?2,
           updated_at=strftime('%Y-%m-%d %H:%M:%f', 'now')
         WHERE id=?1 AND deleted_at IS NULL",
        params![id, STATUS_DELETED],
    )?;
    if changed == 0 {
        anyhow::bail!("ssh connection not found: {id}");
    }
    Ok(())
}

pub fn restore(db: &Database, id: &str) -> anyhow::Result<SshConnectionDto> {
    let conn = db.connect()?;
    let changed = conn.execute(
        "UPDATE ssh_connections SET
           deleted_at=NULL,
           connection_status=CASE WHEN enabled=1 THEN ?2 ELSE ?3 END,
           last_error=NULL,
           updated_at=strftime('%Y-%m-%d %H:%M:%f', 'now')
         WHERE id=?1 AND deleted_at IS NOT NULL",
        params![id, STATUS_UNKNOWN, STATUS_DISABLED],
    )?;
    if changed == 0 {
        anyhow::bail!("deleted ssh connection not found: {id}");
    }
    find_with_conn(&conn, id)?
        .map(|r| r.dto)
        .context("restored ssh connection not found")
}

fn find_with_conn(conn: &Connection, id: &str) -> anyhow::Result<Option<SshConnectionRecord>> {
    let query = format!("SELECT {SELECT_COLUMNS} FROM ssh_connections WHERE id=?1");
    let mut stmt = conn.prepare(&query)?;
    stmt.query_row(params![id], map_record)
        .optional()
        .context("failed to find ssh connection")
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SshConnectionDto> {
    Ok(map_record(row)?.dto)
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SshConnectionRecord> {
    Ok(SshConnectionRecord {
        dto: SshConnectionDto {
            id: row.get(0)?,
            display_name: row.get(1)?,
            source_kind: row.get(2)?,
            config_alias: row.get(3)?,
            host_name: row.get(4)?,
            user: row.get(5)?,
            port: row.get::<_, i64>(6)? as u16,
            identity_file: row.get(7)?,
            host_key_type: row.get(8)?,
            enabled: row.get(9)?,
            connection_status: row.get(10)?,
            last_connected_at: row.get(11)?,
            last_error: row.get(12)?,
            deleted_at: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        },
        host_key_base64: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn monitored_status_is_versioned_and_lifecycle_states_are_persisted() {
        let path = std::env::temp_dir().join(format!("auracoder-ssh-status-{}.db", Uuid::new_v4()));
        let db = Database::open(path.clone()).expect("failed to initialize test database");
        let input = SshConnectionInput {
            display_name: "Test SSH".to_string(),
            host_name: "192.0.2.10".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            host_key: String::new(),
            config_alias: None,
        };
        let dto = insert(
            &db,
            "ssh-status-test",
            "manual",
            &input,
            "ssh-ed25519",
            "test-key",
        )
        .expect("failed to insert test connection");
        assert_eq!(dto.connection_status, STATUS_UNKNOWN);
        assert!(
            !set_status_if_current(&db, &dto.id, "stale-version", STATUS_OK, None,)
                .expect("stale status update should not fail")
        );

        let record = find(&db, &dto.id)
            .expect("failed to load test connection")
            .expect("test connection should exist");
        assert!(set_status_if_current(
            &db,
            &dto.id,
            &record.dto.updated_at,
            STATUS_CONNECTING,
            None,
        )
        .expect("failed to persist connecting status"));
        assert_eq!(
            find(&db, &dto.id)
                .expect("failed to reload test connection")
                .expect("test connection should exist")
                .dto
                .connection_status,
            STATUS_CONNECTING
        );

        let version = find(&db, &dto.id)
            .expect("failed to reload test connection")
            .expect("test connection should exist")
            .dto
            .updated_at;
        assert!(
            set_status_if_current(&db, &dto.id, &version, STATUS_OK, None,)
                .expect("failed to persist connected status")
        );
        assert_eq!(
            find(&db, &dto.id)
                .expect("failed to reload test connection")
                .expect("test connection should exist")
                .dto
                .connection_status,
            STATUS_OK
        );

        let disabled = set_enabled(&db, &dto.id, false).expect("failed to disable test connection");
        assert_eq!(disabled.connection_status, STATUS_DISABLED);
        soft_delete(&db, &dto.id).expect("failed to delete test connection");
        let restored = restore(&db, &dto.id).expect("failed to restore test connection");
        assert_eq!(restored.connection_status, STATUS_DISABLED);

        let _ = std::fs::remove_file(path);
    }
}
