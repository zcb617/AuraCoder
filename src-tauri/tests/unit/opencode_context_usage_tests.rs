use super::*;

/// 验证 OpenCode 上下文快照选择最新 assistant token，并匹配规范 provider/model ID。
#[tokio::test]
async fn context_usage_snapshot_matches_latest_assistant_and_provider_model() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock OpenCode server");
    let address = listener.local_addr().expect("read mock server address");
    let server_task = tokio::spawn(async move {
        let message_response = r#"[
            {
                "info": {
                    "id": "msg_old",
                    "role": "assistant",
                    "modelID": "big-pickle",
                    "providerID": "opencode",
                    "time": {
                        "created": 1700000000000,
                        "completed": 1700000001000
                    },
                    "tokens": {
                        "input": 15997,
                        "output": 1,
                        "reasoning": 0,
                        "cache": {
                            "read": 0,
                            "write": 0
                        },
                        "total": 16000
                    }
                },
                "parts": []
            },
            {
                "info": {
                    "id": "msg_new",
                    "role": "assistant",
                    "modelID": "big-pickle",
                    "providerID": "opencode",
                    "time": {
                        "created": 1700000002000,
                        "completed": 1700000003000
                    },
                    "tokens": {
                        "input": 137,
                        "output": 2,
                        "reasoning": 0,
                        "cache": {
                            "read": 0,
                            "write": 0
                        },
                        "total": 200
                    }
                },
                "parts": []
            }
        ]"#;
        let provider_response = r#"{
            "all": [
                {
                    "id": "opencode",
                    "name": "OpenCode",
                    "models": {
                        "big-pickle": {
                            "id": "big-pickle",
                            "name": "Big Pickle",
                            "status": "active",
                            "limit": {
                                "context": 200000
                            },
                            "capabilities": {
                                "attachment": false
                            },
                            "variants": {}
                        }
                    }
                }
            ],
            "connected": ["opencode"],
            "default": {}
        }"#;
        let responses = [
            ("GET /session/ses_test/message?", message_response),
            ("GET /provider ", provider_response),
        ];

        for (expected_request, body) in responses {
            let (mut stream, _) = listener.accept().await.expect("accept mock request");
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("read mock request");
                assert!(read > 0, "mock client closed before request headers");
                request_bytes.extend_from_slice(&buffer[..read]);
                if request_bytes
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
                {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request_bytes);
            assert!(
                request.starts_with(expected_request),
                "unexpected mock request: {request}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.as_bytes().len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write mock response");
        }
    });

    let engine = OpenCodeEngine::new_remote_http(
        format!("http://{address}"),
        "runtime-secret".to_string(),
    );
    let snapshot = engine
        .context_usage_snapshot("", "ses_test")
        .await
        .expect("read OpenCode context usage snapshot");

    assert_eq!(snapshot, Some((137, 200000)));
    server_task.await.expect("wait for mock OpenCode server");
}
