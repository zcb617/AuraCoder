use super::{parse_host_key, write_file};
use uuid::Uuid;

/// 验证完整 OpenSSH 公钥行可被解析并提取密钥类型与内容。
#[test]
fn accepts_full_openssh_public_key_line() {
    let parsed = parse_host_key("ssh-ed25519 AQID test").expect("valid host key");
    assert_eq!(parsed.0, "ssh-ed25519");
    assert_eq!(parsed.1, "AQID");
}

/// 验证指纹文本不会被误当作完整的 OpenSSH 公钥行。
#[test]
fn rejects_fingerprint_only_value() {
    assert!(parse_host_key("SHA256:abc").is_err());
}

/// 验证已存在的 known_hosts 文件被完整替换且不会遗留临时文件。
#[test]
fn known_hosts_file_is_replaced_as_complete_content() {
    let directory = std::env::temp_dir().join(format!("auracoder-known-hosts-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test known_hosts directory");
    let path = directory.join("connection");
    let first = "example.com ssh-ed25519 AQID\n";
    let second = "[example.com]:2222 ssh-rsa BAUG\n";

    write_file(&path, first).expect("write first known_hosts content");
    assert_eq!(std::fs::read_to_string(&path).expect("read first content"), first);
    write_file(&path, second).expect("replace known_hosts content");
    assert_eq!(std::fs::read_to_string(&path).expect("read replaced content"), second);
    write_file(&path, second).expect("keep matching known_hosts content");
    assert_eq!(std::fs::read_to_string(&path).expect("read unchanged content"), second);
    assert_eq!(
        std::fs::read_dir(&directory)
            .expect("list test known_hosts directory")
            .count(),
        1
    );

    let _ = std::fs::remove_dir_all(directory);
}
