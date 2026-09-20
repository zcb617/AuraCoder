-- 聊天输入区高度配置；不插入默认值，缺少配置时使用 textarea rows=3 的自然布局。

UPDATE schema_version
SET version = 114,
    migration_file = '114.sql',
    applied_at = datetime('now')
WHERE id = 1;
