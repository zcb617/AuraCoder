use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{Duration, Local};
use rusqlite::{params, OptionalExtension};

use crate::models::ExtensionItemDto;
use crate::runtime_env;

use super::Database;

pub const EXTENSION_KINDS: [&str; 3] = ["skill", "plugin", "mcp"];
pub const NORMAL_REFRESH_INTERVAL: Duration = Duration::hours(6);

/// 将扩展缓存操作时间转换为本机本地 RFC3339，拒绝非法输入。
fn normalize_extension_time(raw: &str) -> Result<String> {
    runtime_env::normalize_time_to_local(raw)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRefreshTarget {
    pub provider_id: String,
    pub context_key: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct ExtensionCatalogSnapshot {
    pub kind: String,
    pub items: Vec<ExtensionItemDto>,
    pub fetched_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub next_refresh_at: Option<String>,
    pub last_error: Option<String>,
}

pub fn ensure_context(
    db: &Database,
    provider_id: &str,
    context_key: &str,
    observed_at: &str,
) -> Result<()> {
    let observed_at = normalize_extension_time(observed_at)?;
    let mut conn = db.connect()?;
    let transaction = conn
        .transaction()
        .context("failed to start extension context transaction")?;
    for kind in EXTENSION_KINDS {
        transaction
            .execute(
                "INSERT INTO extension_catalog_snapshots (
                    provider_id, context_key, kind, items_json, next_refresh_at, failure_count
                 ) VALUES (?1, ?2, ?3, '[]', ?4, 0)
                 ON CONFLICT(provider_id, context_key, kind) DO NOTHING",
                params![provider_id, context_key, kind, observed_at],
            )
            .context("failed to register extension catalog context")?;
    }
    transaction
        .commit()
        .context("failed to commit extension catalog context")?;
    Ok(())
}

/// Schedule one fresh background read when the application starts. This honors
/// the startup-immediate refresh contract while retaining failure count so
/// retries within the running application still use the documented backoff.
pub fn schedule_startup_refresh(
    db: &Database,
    provider_id: &str,
    context_key: &str,
    kind: &str,
    observed_at: &str,
) -> Result<()> {
    let observed_at = normalize_extension_time(observed_at)?;
    let conn = db.connect()?;
    conn.execute(
        "UPDATE extension_catalog_snapshots
         SET next_refresh_at = ?4
         WHERE provider_id = ?1
           AND context_key = ?2
           AND kind = ?3",
        params![provider_id, context_key, kind, observed_at],
    )
    .context("failed schedule extension catalog startup refresh")?;
    Ok(())
}

pub fn load_snapshots(
    db: &Database,
    provider_id: &str,
    context_key: &str,
) -> Result<Vec<ExtensionCatalogSnapshot>> {
    let conn = db.connect()?;
    let mut statement = conn
        .prepare(
            "SELECT provider_id, context_key, kind, items_json, fetched_at, last_attempt_at,
                    next_refresh_at, last_error, failure_count
             FROM extension_catalog_snapshots
             WHERE provider_id = ?1 AND context_key = ?2
             ORDER BY kind ASC",
        )
        .context("failed to prepare extension catalog snapshot query")?;
    let rows = statement
        .query_map(params![provider_id, context_key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })
        .context("failed to query extension catalog snapshots")?;

    let mut snapshots = Vec::new();
    for row in rows {
        let (
            _provider_id,
            _context_key,
            kind,
            items_json,
            fetched_at,
            last_attempt_at,
            next_refresh_at,
            last_error,
            _failure_count,
        ) = row.context("failed to read extension catalog snapshot")?;
        let items = serde_json::from_str(&items_json).with_context(|| {
            format!(
                "failed to parse extension catalog snapshot for {provider_id}/{context_key}/{kind}"
            )
        })?;
        snapshots.push(ExtensionCatalogSnapshot {
            kind,
            items,
            fetched_at,
            last_attempt_at,
            next_refresh_at,
            last_error,
        });
    }
    Ok(snapshots)
}

pub fn list_due_refreshes(db: &Database, now: &str) -> Result<Vec<ExtensionRefreshTarget>> {
    let now = normalize_extension_time(now)?;
    let conn = db.connect()?;
    let mut statement = conn
        .prepare(
            "SELECT provider_id, context_key, kind
             FROM extension_catalog_snapshots
             WHERE next_refresh_at IS NULL OR julianday(next_refresh_at) <= julianday(?1)
             ORDER BY julianday(next_refresh_at) ASC, provider_id, context_key, kind",
        )
        .context("failed to prepare due extension catalog refresh query")?;
    let rows = statement
        .query_map(params![now], |row| {
            Ok(ExtensionRefreshTarget {
                provider_id: row.get(0)?,
                context_key: row.get(1)?,
                kind: row.get(2)?,
            })
        })
        .context("failed to query due extension catalog refreshes")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read due extension catalog refreshes")
}

pub fn next_refresh_at(db: &Database) -> Result<Option<String>> {
    let conn = db.connect()?;
    conn.query_row(
        "SELECT next_refresh_at
         FROM extension_catalog_snapshots
         WHERE next_refresh_at IS NOT NULL
         ORDER BY julianday(next_refresh_at) ASC, rowid ASC
         LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
    .context("failed to read next extension catalog refresh time")
}

pub fn record_success(
    db: &Database,
    provider_id: &str,
    context_key: &str,
    kind: &str,
    items: &[ExtensionItemDto],
    attempted_at: &str,
) -> Result<()> {
    let attempted_at = normalize_extension_time(attempted_at)?;
    let next_refresh_at = runtime_env::format_system_time(Local::now() + NORMAL_REFRESH_INTERVAL);
    let items_json = serde_json::to_string(items)
        .context("failed to serialize sanitized extension catalog snapshot")?;
    let conn = db.connect()?;
    conn.execute(
        "INSERT INTO extension_catalog_snapshots (
            provider_id, context_key, kind, items_json, fetched_at, last_attempt_at,
            next_refresh_at, last_error, failure_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, NULL, 0)
         ON CONFLICT(provider_id, context_key, kind) DO UPDATE SET
            items_json = excluded.items_json,
            fetched_at = excluded.fetched_at,
            last_attempt_at = excluded.last_attempt_at,
            next_refresh_at = excluded.next_refresh_at,
            last_error = NULL,
            failure_count = 0",
        params![
            provider_id,
            context_key,
            kind,
            items_json,
            attempted_at,
            next_refresh_at,
        ],
    )
    .context("failed to save extension catalog snapshot")?;
    Ok(())
}

pub fn record_failure(
    db: &Database,
    provider_id: &str,
    context_key: &str,
    kind: &str,
    attempted_at: &str,
    error_summary: &str,
) -> Result<u32> {
    let attempted_at = normalize_extension_time(attempted_at)?;
    let mut conn = db.connect()?;
    let transaction = conn
        .transaction()
        .context("failed to start extension catalog failure transaction")?;
    transaction
        .execute(
            "INSERT INTO extension_catalog_snapshots (
                provider_id, context_key, kind, items_json, next_refresh_at, failure_count
             ) VALUES (?1, ?2, ?3, '[]', ?4, 0)
             ON CONFLICT(provider_id, context_key, kind) DO NOTHING",
            params![provider_id, context_key, kind, attempted_at],
        )
        .context("failed to register failed extension catalog refresh")?;
    let previous_failure_count: i64 = transaction
        .query_row(
            "SELECT failure_count FROM extension_catalog_snapshots
             WHERE provider_id = ?1 AND context_key = ?2 AND kind = ?3",
            params![provider_id, context_key, kind],
            |row| row.get(0),
        )
        .context("failed to read extension catalog failure count")?;
    let failure_count = previous_failure_count.max(0) as u32 + 1;
    let next_refresh_at = runtime_env::format_system_time(Local::now() + retry_delay(failure_count));
    transaction
        .execute(
            "UPDATE extension_catalog_snapshots
             SET last_attempt_at = ?1,
                 next_refresh_at = ?2,
                 last_error = ?3,
                 failure_count = ?4
             WHERE provider_id = ?5 AND context_key = ?6 AND kind = ?7",
            params![
                attempted_at,
                next_refresh_at,
                error_summary,
                failure_count as i64,
                provider_id,
                context_key,
                kind,
            ],
        )
        .context("failed to save extension catalog refresh failure")?;
    transaction
        .commit()
        .context("failed to commit extension catalog refresh failure")?;
    Ok(failure_count)
}

pub fn group_due_refreshes(
    targets: Vec<ExtensionRefreshTarget>,
) -> BTreeMap<(String, String), Vec<String>> {
    let mut grouped = BTreeMap::<(String, String), Vec<String>>::new();
    for target in targets {
        grouped
            .entry((target.provider_id, target.context_key))
            .or_default()
            .push(target.kind);
    }
    grouped
}

pub fn retry_delay(failure_count: u32) -> Duration {
    match failure_count {
        1 => Duration::minutes(1),
        2 => Duration::minutes(30),
        3 => Duration::hours(1),
        _ => NORMAL_REFRESH_INTERVAL,
    }
}

pub fn latest_snapshot_timestamp(snapshots: &[ExtensionCatalogSnapshot]) -> Option<String> {
    snapshots
        .iter()
        .filter_map(|snapshot| snapshot.fetched_at.as_deref())
        .max_by_key(|value| runtime_env::parse_persisted_time_to_utc(value).ok())
        .map(str::to_string)
}

pub fn latest_attempt_timestamp(snapshots: &[ExtensionCatalogSnapshot]) -> Option<String> {
    snapshots
        .iter()
        .filter_map(|snapshot| snapshot.last_attempt_at.as_deref())
        .max_by_key(|value| runtime_env::parse_persisted_time_to_utc(value).ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::db::Database;

    /// 创建使用唯一文件路径的扩展测试数据库，避免迁移备份目标发生冲突。
    fn test_database() -> Database {
        let path = std::env::temp_dir().join(format!("auracoder-extensions-{}.db", Uuid::new_v4()));
        Database::open(path).expect("failed to open test database")
    }

    #[test]
    fn retry_delay_matches_documented_backoff() {
        assert_eq!(retry_delay(1), Duration::minutes(1));
        assert_eq!(retry_delay(2), Duration::minutes(30));
        assert_eq!(retry_delay(3), Duration::hours(1));
        assert_eq!(retry_delay(4), Duration::hours(6));
    }

    #[test]
    fn failed_kind_preserves_previous_successful_snapshot() {
        let db = test_database();
        let now = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &now).unwrap();
        record_success(&db, "codex", "workspace:/demo", "mcp", &[], &now).unwrap();
        record_failure(
            &db,
            "codex",
            "workspace:/demo",
            "mcp",
            &Utc::now().to_rfc3339(),
            "MCP refresh failed.",
        )
        .unwrap();

        let snapshot = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "mcp")
            .unwrap();
        assert!(snapshot.fetched_at.is_some());
        assert_eq!(snapshot.last_error.as_deref(), Some("MCP refresh failed."));
    }

    #[test]
    fn context_registration_does_not_overwrite_existing_snapshots() {
        let db = test_database();
        let now = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &now).unwrap();
        record_success(&db, "codex", "workspace:/demo", "skill", &[], &now).unwrap();
        ensure_context(&db, "codex", "workspace:/demo", &Utc::now().to_rfc3339()).unwrap();

        let snapshots = load_snapshots(&db, "codex", "workspace:/demo").unwrap();
        assert_eq!(snapshots.len(), EXTENSION_KINDS.len());
        assert!(snapshots
            .iter()
            .find(|snapshot| snapshot.kind == "skill")
            .and_then(|snapshot| snapshot.fetched_at.as_ref())
            .is_some());
    }

    #[test]
    fn startup_refresh_reschedules_success_and_failure() {
        let db = test_database();
        let first_attempt = Utc::now().to_rfc3339();
        ensure_context(&db, "codex", "workspace:/demo", &first_attempt).unwrap();
        record_success(
            &db,
            "codex",
            "workspace:/demo",
            "skill",
            &[],
            &first_attempt,
        )
        .unwrap();

        let startup_at = "2030-01-02T03:04:05+00:00";
        let expected_startup_at = runtime_env::normalize_time_to_local(startup_at).unwrap();
        schedule_startup_refresh(&db, "codex", "workspace:/demo", "skill", startup_at).unwrap();
        let scheduled = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "skill")
            .unwrap();
        assert_eq!(
            scheduled.next_refresh_at.as_deref(),
            Some(expected_startup_at.as_str())
        );

        record_failure(
            &db,
            "codex",
            "workspace:/demo",
            "skill",
            &Utc::now().to_rfc3339(),
            "refresh_failed",
        )
        .unwrap();
        let restarted_at = "2030-01-02T04:05:06+00:00";
        let expected_restarted_at = runtime_env::normalize_time_to_local(restarted_at).unwrap();
        schedule_startup_refresh(&db, "codex", "workspace:/demo", "skill", restarted_at).unwrap();
        let after_restart = load_snapshots(&db, "codex", "workspace:/demo")
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.kind == "skill")
            .and_then(|snapshot| snapshot.next_refresh_at)
            .unwrap();
        assert_eq!(after_restart, expected_restarted_at);
    }
}
