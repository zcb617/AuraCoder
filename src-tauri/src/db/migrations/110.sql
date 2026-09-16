-- GPU 加速配置；不插入默认值，让 Rust 代码根据平台决定
-- Linux 默认关闭，Windows/macOS 默认开启
-- 用户通过设置界面修改后才会写入 config 表

UPDATE schema_version
SET version = 110,
    migration_file = '110.sql',
    applied_at = datetime('now')
WHERE id = 1;
