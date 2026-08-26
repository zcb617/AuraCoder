#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    pub version: u64,
    pub file: &'static str,
    pub sql: &'static str,
    pub reason: &'static str,
    pub requires_foreign_keys_off: bool,
}

// 数据库版本独立于软件版本。新增数据库变更时，只能在这里追加更高版本。
pub const BASELINE_VERSION: u64 = 100;

// 当前程序版本明确支持的数据库版本，不通过迁移清单最后一项推断。
pub const SUPPORTED_DATABASE_VERSION: u64 = 106;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 100,
        file: "100.sql",
        sql: include_str!("100.sql"),
        reason: "baseline-version-registration",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 101,
        file: "101.sql",
        sql: include_str!("101.sql"),
        reason: "reserved-version-registration",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 102,
        file: "102.sql",
        sql: include_str!("102.sql"),
        reason: "reserved-version-registration",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 103,
        file: "103.sql",
        sql: include_str!("103.sql"),
        reason: "reserved-version-registration",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 105,
        file: "105.sql",
        sql: include_str!("105.sql"),
        reason: "ssh-remote-project",
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 106,
        file: "106.sql",
        sql: include_str!("106.sql"),
        reason: "thread-runtime-selection-columns",
        requires_foreign_keys_off: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_list_is_strictly_increasing() {
        for pair in MIGRATIONS.windows(2) {
            assert!(pair[0].version < pair[1].version);
        }
        assert_eq!(
            MIGRATIONS.first().map(|migration| migration.version),
            Some(BASELINE_VERSION)
        );
        assert!(MIGRATIONS
            .iter()
            .any(|migration| migration.version == SUPPORTED_DATABASE_VERSION));
    }
}
