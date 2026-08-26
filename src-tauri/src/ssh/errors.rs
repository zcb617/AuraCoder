use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum SshError {
    #[error("未找到本机 ssh 命令，请先安装 OpenSSH")]
    SshUnavailable,
    #[error("SSH 配置文件不存在: {0}")]
    ConfigNotFound(String),
    #[error("SSH 配置解析失败: {0}")]
    Config(String),
    #[error("Host Key 必须填写完整的 OpenSSH 公钥行，例如 ssh-ed25519 AAAA...")]
    InvalidHostKey,
    #[error("SSH 连接检测失败: {0}")]
    Connection(String),
}
