/// 初始化当前实例的 log4rs 日志配置并注册全局日志后端。
pub(crate) fn initialize() -> anyhow::Result<()> {
    let app_data_dir = crate::runtime_env::app_data_dir();
    std::fs::create_dir_all(&app_data_dir)?;

    let config_path = app_data_dir.join("log4rs.yaml");
    if !config_path.exists() {
        std::fs::write(&config_path, include_str!("../log4rs.yaml"))?;
    }

    log4rs::init_file(&config_path, Default::default())?;
    Ok(())
}
