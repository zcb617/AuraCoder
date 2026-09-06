/// 初始化当前实例的 log4rs 日志配置并注册全局日志后端。
pub(crate) fn initialize() -> anyhow::Result<()> {
    let app_data_dir = crate::runtime_env::app_data_dir();
    std::fs::create_dir_all(&app_data_dir)?;

    let config_path = app_data_dir.join("log4rs.yaml");
    if !config_path.exists() {
        std::fs::write(&config_path, include_str!("../log4rs.yaml"))?;
    }

    // log4rs.yaml 中的日志路径是相对进程工作目录的 logs/，开发版 cargo run 的工作目录是
    // src-tauri/，日志会落进 tauri 文件监听范围导致无限重建重启；这里把相对的 logs/ 替换为
    // 应用数据目录下的绝对路径再加载，不改动用户可编辑的 log4rs.yaml 本体。
    let logs_dir = app_data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;
    let raw_config = std::fs::read_to_string(&config_path)?;
    let resolved_config = raw_config.replace("logs/", &format!("{}/", logs_dir.display()));
    let resolved_path = app_data_dir.join("log4rs.resolved.yaml");
    std::fs::write(&resolved_path, resolved_config)?;

    log4rs::init_file(&resolved_path, Default::default())?;
    Ok(())
}
