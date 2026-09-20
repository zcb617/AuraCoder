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
pub const SUPPORTED_DATABASE_VERSION: u64 = 114;

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
    Migration {
        version: 107,
        file: "107.sql",
        sql: include_str!("107.sql"),
        reason: "app-config-dictionary",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 108,
        file: "108.sql",
        sql: include_str!("108.sql"),
        reason: "project-root-git-context",
        requires_foreign_keys_off: true,
    },
    Migration {
        version: 109,
        file: "109.sql",
        sql: include_str!("109.sql"),
        reason: "Codex thread context usage snapshot",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 110,
        file: "110.sql",
        sql: include_str!("110.sql"),
        reason: "gpu-acceleration-config",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 111,
        file: "111.sql",
        sql: include_str!("111.sql"),
        reason: "Claude thread sync flag",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 112,
        file: "112.sql",
        sql: include_str!("112.sql"),
        reason: "system-local-time-normalization",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 113,
        file: "113.sql",
        sql: include_str!("113.sql"),
        reason: "chat-transcript-width-config",
        requires_foreign_keys_off: false,
    },
    Migration {
        version: 114,
        file: "114.sql",
        sql: include_str!("114.sql"),
        reason: "chat-input-height-config",
        requires_foreign_keys_off: false,
    },
];

#[cfg(test)]
#[path = "../../../tests/unit/chat_input_height_migration_tests.rs"]
mod chat_input_height_migration_tests;

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
