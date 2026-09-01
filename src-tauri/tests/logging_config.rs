use std::path::Path;

use log4rs::config::load_config_file;

/// 验证仓库默认 log4rs 配置可以被解析。
#[test]
fn default_log4rs_configuration_is_valid() {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("log4rs.yaml");
    load_config_file(&config_path, Default::default())
        .expect("default log4rs configuration should be valid");
}
