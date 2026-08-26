-- 本版本不包含业务结构或业务数据变更，只按正常升级流程登记版本。
SELECT 1;

UPDATE schema_version
SET version = 101,
    migration_file = '101.sql',
    applied_at = datetime('now')
WHERE id = 1;
