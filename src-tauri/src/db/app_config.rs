use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use rusqlite::{params, Connection};

use crate::runtime_env;

/// 返回应用唯一使用的配置数据库路径。
pub(crate) fn configuration_database_path() -> PathBuf {
    runtime_env::app_data_dir().join("workspaces.db")
}

/// 打开配置数据库并设置原有的五秒 busy timeout。
fn open_config_database(path: &Path) -> anyhow::Result<Connection> {
    let connection = Connection::open(path)
        .context("failed to open workspaces database for app config")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

/// 从指定配置数据库读取所有原始键值项。
pub(crate) fn load_entries(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let connection = open_config_database(path)?;
    load_entries_from_connection(&connection)
}

/// 从现有连接中读取配置原始键值项，不解析 JSON 或过滤未知键。
pub(crate) fn load_entries_from_connection(
    connection: &Connection,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut statement = connection.prepare("SELECT config_key, config_value FROM config")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// 以事务形式保存配置原始键值项并保留未触及的未知键。
pub(crate) fn save_entries(path: &Path, entries: &[(String, String)]) -> anyhow::Result<()> {
    let mut connection = open_config_database(path)?;
    save_entries_on_connection(&mut connection, entries)
}

/// 在现有连接的单个事务中 upsert 全部配置键值项。
pub(crate) fn save_entries_on_connection(
    connection: &mut Connection,
    entries: &[(String, String)],
) -> anyhow::Result<()> {
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "INSERT INTO config(config_key, config_value) VALUES (?1, ?2)\
         ON CONFLICT(config_key) DO UPDATE SET config_value = excluded.config_value",
    )?;
    for (key, value) in entries {
        statement.execute(params![key, value])?;
    }
    drop(statement);
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/app_config_mapper_tests.rs"]
mod tests;