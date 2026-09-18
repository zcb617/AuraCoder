-- 聊天内容宽度配置；不插入默认值，使用代码默认 medium
-- 用户通过设置界面修改后才会写入 config 表

UPDATE schema_version
SET version = 113,
    migration_file = '113.sql',
    applied_at = datetime('now')
WHERE id = 1;
