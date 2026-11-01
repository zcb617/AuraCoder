use std::path::PathBuf;

use super::*;
use crate::engines::TurnAttachment;

// 旧声明在内联 mod tests 内部，解析基址 src/engines/opencode/tests/ 不存在，必然 ENOENT；
// 已按 factory.rs 同例移到文件顶层，见上方模块声明。
// #[path = "../../../../tests/unit/opencode_context_usage_tests.rs"]
// mod opencode_context_usage_tests;

fn test_remote_engine(base_url: String) -> OpenCodeEngine {
    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url,
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: false,
        })),
    }
}

async fn assert_permission_asked_mode(
    mode: OpenCodePermissionMode,
    expected_reply: Option<&'static str>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let (base_url, server_task) = if let Some(expected_reply) = expected_reply {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let (body_start, content_length) = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "mock client closed before permission reply");
                request_bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request_bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                {
                    let body_start = header_end + 4;
                    let headers = String::from_utf8_lossy(&request_bytes[..header_end])
                        .to_ascii_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request_bytes.len() >= body_start + content_length {
                        break (body_start, content_length);
                    }
                }
            };
            let request =
                String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
            assert!(request.starts_with("POST /permission/request-1/reply HTTP/1.1"));
            assert!(request.contains(&format!("\"reply\":\"{expected_reply}\"")));
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{address}"), Some(task))
    } else {
        ("http://127.0.0.1:9".to_string(), None)
    };

    let engine = test_remote_engine(base_url);
    let connection = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    engine.state.lock().await.sessions.insert(
        "ses_test".to_string(),
        OpenCodeSession {
            cwd: "/var/work/project-a".to_string(),
            model_id: "openai/gpt-5".to_string(),
            reasoning_effort: None,
            agent: None,
            permission_mode: mode,
            connection: connection.clone(),
        },
    );

    let (event_tx, mut event_rx) = mpsc::channel(4);
    engine
        .handle_permission_asked(
            &json!({
                "id": "request-1",
                "sessionID": "ses_test",
                "permission": "bash",
                "patterns": ["*"]
            }),
            &event_tx,
            connection,
        )
        .await;
    drop(event_tx);

    if expected_reply.is_some() {
        assert!(event_rx.try_recv().is_err());
        server_task.unwrap().await.unwrap();
    } else {
        match event_rx.recv().await {
            Some(EngineEvent::ApprovalRequested { details, .. }) => {
                assert_eq!(details["_opencodeRequestKind"], json!("permission"));
            }
            _ => panic!("expected OpenCode permission approval event"),
        }
    }
}

#[tokio::test]
async fn listener_failure_is_forwarded_to_the_active_turn() {
    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    let mut incoming = spawn_opencode_incoming_pump(event_bus.clone());

    event_bus
        .send(OpenCodeBusItem::Failure("event stream failed".to_string()))
        .expect("active turn must subscribe to the event bus");

    match incoming.recv().await {
        Some(OpenCodeIncomingEvent::Failure(message)) => {
            assert_eq!(message, "event stream failed");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn remote_http_target_uses_shared_endpoint_and_directory_header() {
    let engine = OpenCodeEngine::new_remote_http(
        "http://127.0.0.1:43101".to_string(),
        "runtime-secret".to_string(),
    );
    assert!(engine.is_available().await);

    let connection = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    let request = engine
        .request(connection.as_ref(), reqwest::Method::GET, "/provider")
        .build()
        .unwrap();

    assert_eq!(
        request
            .headers()
            .get("X-OpenCode-Directory")
            .and_then(|value| value.to_str().ok()),
        Some("/var/work/project-a")
    );
    assert!(request.headers().contains_key(AUTHORIZATION));

    let machine_server = engine.connection_for_cwd("").await.unwrap();
    let machine_model_request = engine
        .request(machine_server.as_ref(), reqwest::Method::GET, "/provider")
        .build()
        .unwrap();
    assert!(!machine_model_request
        .headers()
        .contains_key("X-OpenCode-Directory"));

    let read_request = engine
        .http
        .request(
            reqwest::Method::GET,
            "http://127.0.0.1:43101/session/ses_test",
        )
        .headers(auth_headers("runtime-secret"))
        .build()
        .unwrap();
    assert_eq!(read_request.url().path(), "/session/ses_test");
    assert!(read_request.url().query().is_none());
    assert!(!read_request.headers().contains_key("X-OpenCode-Directory"));
}

#[tokio::test]
async fn remote_event_pump_subscribes_to_each_workspace_directory() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let mut directories = Vec::new();
        let mut streams = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request_bytes = vec![0_u8; 4096];
            let mut length = 0;
            while !request_bytes[..length]
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                let read = stream.read(&mut request_bytes[length..]).await.unwrap();
                assert!(read > 0, "event pump closed before sending HTTP headers");
                length += read;
            }
            let request = String::from_utf8_lossy(&request_bytes[..length]);
            assert!(request.starts_with("GET /event HTTP/1.1"));
            assert!(request.lines().any(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            }));
            let directory = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("x-opencode-directory")
                        .then(|| value.trim().to_string())
                })
                .expect("remote event subscription must include its workspace directory");
            directories.push(directory);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .unwrap();
            streams.push(stream);
        }
        directories
    });

    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    let engine = OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url: format!("http://{address}"),
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: true,
        })),
    };

    let project_a = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    let project_b = engine
        .connection_for_cwd("/var/work/project-b")
        .await
        .unwrap();
    let mut directories = timeout(Duration::from_secs(2), server_task)
        .await
        .expect("workspace event pumps did not connect")
        .unwrap();
    project_a.cancel_event_pump();
    project_b.cancel_event_pump();

    directories.sort();
    assert_eq!(
        directories,
        vec![
            "/var/work/project-a".to_string(),
            "/var/work/project-b".to_string(),
        ]
    );
}

#[tokio::test]
async fn read_session_requests_only_id_path_with_basic_auth() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request_bytes = vec![0_u8; 4096];
        let mut length = 0;
        while !request_bytes[..length]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            let read = stream.read(&mut request_bytes[length..]).await.unwrap();
            assert!(read > 0, "mock client closed before sending HTTP headers");
            length += read;
        }
        let request = String::from_utf8_lossy(&request_bytes[..length]).to_ascii_lowercase();

        // 恢复接口只能按 ID 请求，不能把本机目录作为 query 或目录请求头发送。
        assert!(request.starts_with("get /session/ses_test http/1.1"));
        assert!(!request.contains("?directory="));
        assert!(!request.contains("x-opencode-directory:"));
        assert!(request.contains("authorization: basic "));

        let body = r#"{"id":"ses_test","title":"Restored","directory":"/var/work/project-a","permission":null,"time":{"created":1,"updated":2,"archived":0}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    // 直接组装远端 endpoint，避免测试额外启动 SSE pump 抢占 mock listener；
    // 被测 read_session 仍然走 OpenCodeEngine 的真实 HTTP 请求实现。
    let engine = OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url: format!("http://{address}"),
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: false,
        })),
    };
    let summary = engine
        .read_session("/var/work/project-a", "ses_test")
        .await
        .unwrap();
    assert_eq!(summary.engine_thread_id, "ses_test");
    assert_eq!(summary.cwd, "/var/work/project-a");
    server_task.await.unwrap();
}

#[tokio::test]
async fn read_session_preserves_http_404_for_cli_mapping() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let response =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    // 与成功请求测试一致，不启动 SSE pump，确保本测试只验证 read_session 的 404 传播。
    let engine = OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url: format!("http://{address}"),
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: false,
        })),
    };

    let error = engine
        .read_session("/var/work/project-a", "ses_missing")
        .await
        .expect_err("HTTP 404 must be returned to the CLI mapping layer");
    let request_error = error
        .downcast_ref::<reqwest::Error>()
        .expect("anyhow context must preserve the reqwest source error");
    assert_eq!(request_error.status(), Some(reqwest::StatusCode::NOT_FOUND));
    server_task.await.unwrap();
}

#[tokio::test]
async fn set_session_permission_mode_patches_permission_and_updates_cache() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request_bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (body_start, content_length) = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0, "mock client closed before sending PATCH request");
            request_bytes.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = request_bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                let body_start = header_end + 4;
                let headers =
                    String::from_utf8_lossy(&request_bytes[..header_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request_bytes.len() >= body_start + content_length {
                    break (body_start, content_length);
                }
            }
        };
        let request = String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
        assert!(request.starts_with("PATCH /session/ses_test HTTP/1.1"));
        let body = &request_bytes[body_start..body_start + content_length];
        let body: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            body,
            json!({
                "permission": permission_rules(OpenCodePermissionMode::Allow)
            })
        );
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    let engine = OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url: format!("http://{address}"),
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: false,
        })),
    };
    let connection = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    engine.state.lock().await.sessions.insert(
        "ses_test".to_string(),
        OpenCodeSession {
            cwd: "/var/work/project-a".to_string(),
            model_id: "openai/gpt-5".to_string(),
            reasoning_effort: None,
            agent: None,
            permission_mode: OpenCodePermissionMode::Ask,
            connection,
        },
    );
    engine
        .set_session_permission_mode("/var/work/project-a", "ses_test", &json!("allow"))
        .await
        .unwrap();
    assert_eq!(
        engine
            .state
            .lock()
            .await
            .sessions
            .get("ses_test")
            .map(|session| session.permission_mode),
        Some(OpenCodePermissionMode::Allow)
    );
    server_task.await.unwrap();
}

async fn assert_empty_permission_rules_update_cache_to_ask(
    initial_mode: OpenCodePermissionMode,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request_bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (body_start, content_length) = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(
                read > 0,
                "mock client closed before sending empty PATCH request"
            );
            request_bytes.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = request_bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                let body_start = header_end + 4;
                let headers =
                    String::from_utf8_lossy(&request_bytes[..header_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request_bytes.len() >= body_start + content_length {
                    break (body_start, content_length);
                }
            }
        };
        let request = String::from_utf8_lossy(&request_bytes[..body_start + content_length]);
        assert!(request.starts_with("PATCH /session/ses_empty HTTP/1.1"));
        let body = &request_bytes[body_start..body_start + content_length];
        assert_eq!(std::str::from_utf8(body).unwrap(), r#"{"permission":[]}"#);
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let engine = test_remote_engine(format!("http://{address}"));
    let connection = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    engine.state.lock().await.sessions.insert(
        "ses_empty".to_string(),
        OpenCodeSession {
            cwd: "/var/work/project-a".to_string(),
            model_id: "openai/gpt-5".to_string(),
            reasoning_effort: None,
            agent: None,
            permission_mode: initial_mode,
            connection,
        },
    );
    engine
        .set_session_permission_rules("/var/work/project-a", "ses_empty", &json!([]))
        .await
        .unwrap();
    assert_eq!(
        engine
            .state
            .lock()
            .await
            .sessions
            .get("ses_empty")
            .map(|session| session.permission_mode),
        Some(OpenCodePermissionMode::Ask)
    );
    server_task.await.unwrap();
}

#[tokio::test]
async fn set_session_empty_permission_rules_patches_empty_array_and_updates_cache_to_ask() {
    assert_empty_permission_rules_update_cache_to_ask(OpenCodePermissionMode::Allow).await;
    assert_empty_permission_rules_update_cache_to_ask(OpenCodePermissionMode::Deny).await;
}

#[tokio::test]
async fn set_session_permission_mode_http_failure_does_not_update_cache() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let response =
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    let (event_bus, _) = broadcast::channel::<OpenCodeBusItem>(OPENCODE_EVENT_BUFFER_CAPACITY);
    let engine = OpenCodeEngine {
        state: Arc::new(Mutex::new(OpenCodeState::default())),
        http: reqwest::Client::new(),
        computer_control_service: Arc::new(std::sync::Mutex::new(None)),
        auracoder_thread_mcp_service: Arc::new(std::sync::Mutex::new(None)),
        target: OpenCodeTransportTarget::Remote(Arc::new(RemoteOpenCodeEndpoint {
            base_url: format!("http://{address}"),
            password: "runtime-secret".to_string(),
            event_bus,
            pump_cancel: CancellationToken::new(),
            workspace_event_pump_enabled: false,
        })),
    };
    let connection = engine
        .connection_for_cwd("/var/work/project-a")
        .await
        .unwrap();
    engine.state.lock().await.sessions.insert(
        "ses_test".to_string(),
        OpenCodeSession {
            cwd: "/var/work/project-a".to_string(),
            model_id: "openai/gpt-5".to_string(),
            reasoning_effort: None,
            agent: None,
            permission_mode: OpenCodePermissionMode::Ask,
            connection,
        },
    );
    assert!(engine
        .set_session_permission_mode("/var/work/project-a", "ses_test", &json!("allow"),)
        .await
        .is_err());
    assert_eq!(
        engine
            .state
            .lock()
            .await
            .sessions
            .get("ses_test")
            .map(|session| session.permission_mode),
        Some(OpenCodePermissionMode::Ask)
    );
    server_task.await.unwrap();
}

fn opencode_sort_prefix_for_millis(now_ms: u64, counter: u64) -> String {
    format!(
        "{:012x}",
        now_ms
            .saturating_mul(OPENCODE_ID_COUNTER_STEP)
            .saturating_add(counter)
            & OPENCODE_ID_TIME_MASK
    )
}

#[test]
fn parse_model_slug_splits_on_first_slash() {
    let parsed = parse_model_slug("openrouter/anthropic/claude-sonnet-4.5").unwrap();
    assert_eq!(parsed.provider_id, "openrouter");
    assert_eq!(parsed.model_id, "anthropic/claude-sonnet-4.5");
    assert!(parse_model_slug("missing-provider").is_none());
}

#[test]
fn verbose_models_expose_reasoning_variants() {
    let output = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "status": "active",
  "limit": { "context": 200000, "input": 200000, "output": 100000 },
  "capabilities": {
    "reasoning": true,
    "attachment": false,
    "input": { "text": true, "image": false, "pdf": false }
  },
  "variants": {
    "high": { "thinking": { "type": "enabled" } },
    "max": { "thinking": { "type": "enabled" } }
  }
}
opencode/gpt-5-nano
{
  "id": "gpt-5-nano",
  "providerID": "opencode",
  "name": "GPT-5 Nano",
  "status": "active",
  "limit": { "context": 400000, "input": 200000, "output": 128000 },
  "capabilities": {
    "reasoning": true,
    "attachment": true,
    "input": { "text": true, "image": true, "pdf": true }
  },
  "variants": {
    "minimal": { "reasoningEffort": "minimal" },
    "low": { "reasoningEffort": "low" },
    "medium": { "reasoningEffort": "medium" },
    "high": { "reasoningEffort": "high" }
  }
}"#;

    let records = parse_verbose_model_records(output).unwrap();
    assert_eq!(records.len(), 2);
    let big_pickle_efforts = reasoning_efforts_from_variants(&records[0].variants)
        .into_iter()
        .map(|option| option.reasoning_effort)
        .collect::<Vec<_>>();
    assert_eq!(big_pickle_efforts, vec!["high", "max"]);
    let nano_efforts = reasoning_efforts_from_variants(&records[1].variants)
        .into_iter()
        .map(|option| option.reasoning_effort)
        .collect::<Vec<_>>();
    assert_eq!(nano_efforts, vec!["minimal", "low", "medium", "high"]);
    assert_eq!(
        model_modalities_from_capabilities(records[1].capabilities.as_ref()),
        vec!["text".to_string(), "image".to_string(), "pdf".to_string()]
    );
    assert_eq!(
        attachment_modalities_from_capabilities(records[0].capabilities.as_ref()),
        Vec::<String>::new()
    );
    assert_eq!(
        attachment_modalities_from_capabilities(records[1].capabilities.as_ref()),
        vec!["text".to_string(), "image".to_string(), "pdf".to_string()]
    );
    let limits = model_limits(records[1].limit.as_ref()).expect("limits");
    assert_eq!(limits.context_tokens, Some(400000));
    assert_eq!(limits.output_tokens, Some(128000));
    assert_eq!(
        model_limits(records[0].limit.as_ref()).and_then(|limits| limits.input_tokens),
        Some(200000)
    );
}

#[test]
fn prompt_body_includes_selected_opencode_variant() {
    let body = build_prompt_body(
        "opencode/big-pickle",
        Some("max"),
        None,
        TurnInput {
            message: "hello".to_string(),
            attachments: Vec::new(),
            plan_mode: false,
            permissions: None,
            input_items: Vec::new(),
        },
    )
    .unwrap()
    .body;

    assert_eq!(body.get("variant"), Some(&json!("max")));
    assert_eq!(body["model"]["providerID"], json!("opencode"));
    assert_eq!(body["model"]["modelID"], json!("big-pickle"));
}

#[test]
fn remote_attachment_prompt_uses_the_uploaded_remote_file_url() {
    let body = build_prompt_body(
        "opencode/big-pickle",
        None,
        None,
        TurnInput {
            message: "读取附件".to_string(),
            attachments: vec![TurnAttachment {
                file_name: "说明.txt".to_string(),
                file_path:
                    "/home/tester/.cache/auracoder/attachments/workspace/thread/file.txt"
                        .to_string(),
                preview_file_path: None,
                size_bytes: 12,
                mime_type: Some("text/plain".to_string()),
                browser_annotation: None,
                is_remote: true,
                remote_text_content: Some("附件正文".to_string()),
            }],
            plan_mode: false,
            permissions: None,
            input_items: Vec::new(),
        },
    )
    .expect("prompt body")
    .body;

    assert_eq!(
        body["parts"][1]["url"],
        json!("file:///home/tester/.cache/auracoder/attachments/workspace/thread/file.txt")
    );
    assert_eq!(body["parts"][1]["filename"], json!("说明.txt"));
}

#[test]
fn prompt_body_includes_selected_opencode_agent() {
    let body = build_prompt_body(
        "opencode/big-pickle",
        None,
        Some("explore"),
        TurnInput {
            message: "hello".to_string(),
            attachments: Vec::new(),
            plan_mode: false,
            permissions: None,
            input_items: Vec::new(),
        },
    )
    .unwrap()
    .body;

    assert_eq!(body.get("agent"), Some(&json!("explore")));
}

#[test]
fn prompt_body_ignores_generic_plan_mode_text_for_opencode() {
    let body = build_prompt_body(
        "opencode/big-pickle",
        None,
        Some("explore"),
        TurnInput {
            message: "hello".to_string(),
            attachments: Vec::new(),
            plan_mode: true,
            permissions: None,
            input_items: Vec::new(),
        },
    )
    .unwrap()
    .body;

    assert_eq!(body.get("agent"), Some(&json!("explore")));
    assert_eq!(body["parts"][0]["text"], json!("hello"));
}

#[test]
fn build_prompt_body_returns_same_message_id_it_sends() {
    let prompt = build_prompt_body(
        "opencode/big-pickle",
        None,
        None,
        TurnInput {
            message: "hello".to_string(),
            attachments: Vec::new(),
            plan_mode: false,
            permissions: None,
            input_items: Vec::new(),
        },
    )
    .unwrap();

    assert_eq!(
        prompt.body.get("messageID").and_then(Value::as_str),
        Some(prompt.message_id.as_str())
    );
    assert!(prompt.body.get("tools").is_none());
    assert!(!json_contains_key(&prompt.body, "eager_input_streaming"));
}

fn json_contains_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(key)
                || object.values().any(|value| json_contains_key(value, key))
        }
        Value::Array(items) => items.iter().any(|value| json_contains_key(value, key)),
        _ => false,
    }
}

#[test]
fn model_reasoning_effort_omits_models_without_variants() {
    let model = model_info(
        "openrouter/example/plain-model",
        "Plain Model",
        "OpenCode model",
        false,
        Vec::new(),
        vec!["text".to_string()],
    );

    assert_eq!(resolve_model_reasoning_effort(&model, Some("medium")), None);
}

#[test]
fn model_reasoning_effort_falls_back_to_supported_default() {
    let model = model_info(
        "opencode/big-pickle",
        "Big Pickle",
        "OpenCode model",
        true,
        reasoning_efforts_from_variant_names(&["high", "max"]),
        vec!["text".to_string()],
    );

    assert_eq!(
        resolve_model_reasoning_effort(&model, Some("medium")).as_deref(),
        Some("high")
    );
    assert_eq!(
        resolve_model_reasoning_effort(&model, Some("max")).as_deref(),
        Some("max")
    );
}

#[test]
fn opencode_only_model_catalog_is_not_cached() {
    let opencode_only = vec![model_info(
        "opencode/big-pickle",
        "Big Pickle",
        "OpenCode model",
        true,
        Vec::new(),
        vec!["text".to_string()],
    )];
    let mixed = vec![
        opencode_only[0].clone(),
        model_info(
            "openrouter/anthropic/claude-sonnet-4.5",
            "Claude Sonnet 4.5",
            "OpenRouter model",
            false,
            Vec::new(),
            vec!["text".to_string()],
        ),
    ];

    assert!(!should_cache_runtime_model_catalog(&opencode_only));
    assert!(should_cache_runtime_model_catalog(&mixed));
}

#[test]
fn permission_mode_maps_existing_policy_names() {
    assert_eq!(
        permission_mode_from_policy(Some(&json!("ask"))),
        OpenCodePermissionMode::Ask
    );
    assert_eq!(
        permission_mode_from_policy(Some(&json!("allow"))),
        OpenCodePermissionMode::Allow
    );
    assert_eq!(
        permission_mode_from_policy(Some(&json!("deny"))),
        OpenCodePermissionMode::Deny
    );
    assert_eq!(
        permission_mode_from_policy(Some(&json!("trusted"))),
        OpenCodePermissionMode::Allow
    );
    assert_eq!(
        permission_mode_from_policy(Some(&json!("untrusted"))),
        OpenCodePermissionMode::Deny
    );
    assert_eq!(
        permission_mode_from_policy(Some(&json!("on-request"))),
        OpenCodePermissionMode::Ask
    );
}

#[test]
fn session_wildcard_permission_action_uses_last_global_rule() {
    let permission = json!([
        { "permission": "*", "pattern": "*", "action": "ask" },
        { "permission": "bash", "pattern": "*", "action": "allow" },
        { "permission": "*", "pattern": "*", "action": "allow" }
    ]);
    assert_eq!(
        session_wildcard_permission_action(Some(&permission)),
        Some("allow")
    );
}

#[tokio::test]
async fn permission_asked_allow_replies_always_without_approval_event() {
    assert_permission_asked_mode(OpenCodePermissionMode::Allow, Some("always")).await;
}

#[tokio::test]
async fn permission_asked_deny_replies_reject_without_approval_event() {
    assert_permission_asked_mode(OpenCodePermissionMode::Deny, Some("reject")).await;
}

#[tokio::test]
async fn permission_asked_ask_emits_approval_event() {
    assert_permission_asked_mode(OpenCodePermissionMode::Ask, None).await;
}

#[test]
fn question_answers_follow_opencode_question_order() {
    let questions = vec![
        OpenCodeQuestionInfo {
            question: "Which package manager?".to_string(),
            header: "Package Manager".to_string(),
            options: vec![],
            multiple: None,
            custom: None,
        },
        OpenCodeQuestionInfo {
            question: "Run tests?".to_string(),
            header: "Tests".to_string(),
            options: vec![],
            multiple: None,
            custom: None,
        },
    ];
    let answers = build_question_answers(
        &questions,
        Some(&json!({
            "question-0-package-manager": { "answers": ["pnpm"] },
            "Tests": "yes"
        })),
    );

    assert_eq!(
        answers,
        vec![vec!["pnpm".to_string()], vec!["yes".to_string()]]
    );
}

#[test]
fn question_details_preserve_opencode_selection_flags() {
    let question = OpenCodeQuestionInfo {
        question: "Which checks should OpenCode run?".to_string(),
        header: "Checks".to_string(),
        options: vec![OpenCodeQuestionOption {
            label: "typecheck".to_string(),
            description: "Run TypeScript".to_string(),
        }],
        multiple: Some(true),
        custom: Some(false),
    };

    let details = question_details_json((0, &question));

    assert_eq!(details["id"], json!("question-0-checks"));
    assert_eq!(details["multiple"], json!(true));
    assert_eq!(details["custom"], json!(false));
    assert_eq!(details["options"][0]["label"], json!("typecheck"));
}

#[test]
fn decline_and_cancel_reject_opencode_questions() {
    assert!(should_reject_question_response(
        &json!({ "decision": "decline" })
    ));
    assert!(should_reject_question_response(
        &json!({ "decision": "cancel" })
    ));
    assert!(!should_reject_question_response(&json!({
        "answers": { "question-0-checks": { "answers": ["typecheck"] } }
    })));
}

#[test]
fn step_finish_part_maps_rich_token_usage() {
    let part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_123",
        "messageID": "msg_123",
        "type": "step-finish",
        "reason": "stop",
        "cost": 0.0123,
        "tokens": {
            "input": 100,
            "output": 25,
            "reasoning": 10,
            "cache": { "read": 7, "write": 3 },
            "total": 145
        }
    }))
    .expect("step-finish part should deserialize");

    let usage = token_usage_from_step_finish(&part).expect("token usage");

    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 25);
    assert_eq!(usage.reasoning, Some(10));
    assert_eq!(usage.cache_read, Some(7));
    assert_eq!(usage.cache_write, Some(3));
    assert_eq!(usage.cost_usd, Some(0.0123));
}

#[test]
fn session_record_maps_archived_zero_as_active() {
    let session = map_session_record(OpenCodeSessionRecord {
        id: "ses_123".to_string(),
        title: Some("  Existing session  ".to_string()),
        directory: "/workspace".to_string(),
        permission: Some(permission_rules(OpenCodePermissionMode::Ask)),
        time: OpenCodeSessionTime {
            created: 1_777_155_663_506,
            updated: 1_777_155_663_524,
            archived: Some(0),
        },
    });

    assert_eq!(session.engine_thread_id, "ses_123");
    assert_eq!(session.title.as_deref(), Some("Existing session"));
    assert_eq!(session.cwd, "/workspace");
    assert!(!session.archived);
}

#[test]
fn session_permission_match_compares_current_rules() {
    let session = OpenCodeSessionRecord {
        id: "ses_123".to_string(),
        title: None,
        directory: "/workspace".to_string(),
        permission: Some(json!([
            { "permission": "question", "pattern": "*", "action": "allow" },
            { "permission": "*", "pattern": "*", "action": "ask" }
        ])),
        time: OpenCodeSessionTime {
            created: 1,
            updated: 1,
            archived: None,
        },
    };

    assert!(session_permission_matches(
        &session,
        &json!([
            { "permission": "question", "pattern": "*", "action": "allow" },
            { "permission": "*", "pattern": "*", "action": "ask" }
        ])
    ));
    assert!(!session_permission_matches(
        &session,
        &json!([{ "permission": "*", "pattern": "*", "action": "allow" }])
    ));
}

#[test]
fn file_url_escapes_local_paths() {
    assert_eq!(
        file_url("/tmp/auracoder test/file.txt"),
        "file:///tmp/auracoder%20test/file.txt"
    );
}

#[test]
fn generated_computer_control_tools_use_isolated_callback_contract() {
    let run_dir =
        std::env::temp_dir().join(format!("auracoder-opencode-tools-test-{}", Uuid::new_v4()));
    write_opencode_computer_control_tool(
        &run_dir,
        "http://127.0.0.1:45678/invoke",
        "one-time-token",
        &[
            json!({
                "name": "click",
                "description": "SDK supplied click tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer", "description": "Process id" },
                        "button": { "type": "string" }
                    },
                    "required": ["pid"]
                }
            }),
            json!({
                "name": "verify_state",
                "description": "SDK supplied verify_state tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer" },
                        "expect": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "element": {
                                        "type": ["object", "null"],
                                        "properties": {
                                            "exists": { "type": "boolean", "enum": [true] }
                                        },
                                        "required": ["exists"],
                                        "additionalProperties": false
                                    }
                                },
                                "required": ["element"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["pid", "expect"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "set_config",
                "description": "SDK supplied set_config tool",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "value": {
                            "description": "New value for `key`. JSON type depends on the key."
                        }
                    },
                    "required": ["key"],
                    "additionalProperties": true
                }
            }),
        ],
    )
    .expect("OpenCode tool source should be generated");

    let tool_file = run_dir
        .join(".opencode")
        .join("tools")
        .join("auracoder_computer_control.ts");
    let source = std::fs::read_to_string(&tool_file).expect("tool source should exist");
    assert!(source.contains("import { tool } from \"@opencode-ai/plugin\""));
    assert!(source.contains("x-auracoder-computer-control-token"));
    assert!(source.contains("http://127.0.0.1:45678/invoke"));
    assert!(source.contains("\"one-time-token\""));
    assert!(source.contains("export const click = tool({"));
    assert!(source.contains("export const verify_state = tool({"));
    assert!(source.contains("export const set_config = tool({"));
    assert!(source.contains("\"pid\": tool.schema.number().int().describe(\"Process id\")"));
    assert!(source.contains("\"button\": tool.schema.string().optional()"));
    assert!(source.contains("\"exists\": tool.schema.literal(true)"));
    assert!(source.contains("\"value\": tool.schema.unknown().describe(\"New value for `key`. JSON type depends on the key.\").optional()"));
    assert!(!source.contains("parameters:"));
    assert!(!source.contains("JSON Schema enum 目前只支持 string 类型"));
    assert!(source.contains("if (!response.ok || result?.isError)"));
    assert!(source.contains("result?.isError"));
    assert!(source.contains("throw new Error"));
    assert!(source.contains("return JSON.stringify(result);"));

    let _ = std::fs::remove_dir_all(run_dir);
}

#[test]
fn computer_control_tool_generation_aggregates_schema_errors_without_partial_write() {
    let run_dir =
        std::env::temp_dir().join(format!("auracoder-opencode-tools-error-{}", Uuid::new_v4()));
    let tools_dir = run_dir.join(".opencode").join("tools");
    std::fs::create_dir_all(&tools_dir).expect("test tools directory should be created");
    let tool_file = tools_dir.join("auracoder_computer_control.ts");
    std::fs::write(&tool_file, "sentinel").expect("sentinel should be written");

    let error = write_opencode_computer_control_tool(
        &run_dir,
        "http://127.0.0.1:45678/invoke",
        "one-time-token",
        &[
            json!({
                "name": "bad_schema_one",
                "description": "first bad schema",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "value": { "properties": {} }
                    }
                }
            }),
            json!({
                "name": "bad_schema_two",
                "description": "second bad schema",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "unsupported" }
                    }
                }
            }),
        ],
    )
    .expect_err("multiple schema errors should be returned");
    let error_text = error.to_string();
    assert!(error_text.contains("bad_schema_one"));
    assert!(error_text.contains("bad_schema_two"));
    assert!(error_text.contains("缺少 type 且包含无法解释的关键字"));
    assert!(error_text.contains("不支持 type: unsupported"));
    assert_eq!(
        std::fs::read_to_string(&tool_file).expect("sentinel should remain"),
        "sentinel"
    );

    let _ = std::fs::remove_dir_all(run_dir);
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires CUA v0.28.2 runtime"]
fn cua_catalog_converts_all_opencode_tools() {
    let sdk = Arc::new(crate::computer_control_sdk::CuaDriverSdk::new());
    let manifest_key = concat!("CAR", "G", "O_MANIFEST_DIR");
    sdk.set_resource_dir(Some(
        PathBuf::from(std::env::var(manifest_key).expect("manifest path")).join("resources"),
    ));
    let run_dir =
        std::env::temp_dir().join(format!("auracoder-opencode-cua-catalog-{}", Uuid::new_v4()));

    let conversion = (|| -> Result<()> {
        sdk.initialize().map_err(anyhow::Error::msg)?;
        let service = ComputerControlService::new(sdk.clone());
        let specs = service.sdk_tool_specs().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!specs.is_empty(), "CUA tool catalog is empty");
        anyhow::ensure!(
            specs.iter().any(|spec| spec["name"] == "set_config"),
            "CUA catalog does not contain set_config"
        );
        anyhow::ensure!(
            specs.iter().any(|spec| spec["name"] == "verify_state"),
            "CUA catalog does not contain verify_state"
        );

        write_opencode_computer_control_tool(
            &run_dir,
            "http://127.0.0.1:45678/invoke",
            "catalog-token",
            &specs,
        )?;
        let tool_file = run_dir
            .join(".opencode")
            .join("tools")
            .join("auracoder_computer_control.ts");
        let source = std::fs::read_to_string(&tool_file)?;
        let export_count = source
            .lines()
            .filter(|line| line.starts_with("export const "))
            .count();
        anyhow::ensure!(
            export_count == specs.len(),
            "generated export count {export_count} differs from CUA spec count {}",
            specs.len()
        );
        anyhow::ensure!(
            source.contains("export const set_config = tool({")
                && source.contains("tool.schema.unknown()"),
            "set_config value did not use tool.schema.unknown()"
        );
        anyhow::ensure!(
            source.contains("export const verify_state = tool({")
                && source.contains("tool.schema.literal(true)"),
            "verify_state boolean enum did not use tool.schema.literal(true)"
        );
        Ok(())
    })();

    let cleanup = std::fs::remove_dir_all(&run_dir);
    let shutdown = sdk.shutdown();
    if let Err(error) = conversion {
        panic!("CUA catalog conversion failed: {error:#}");
    }
    cleanup.expect("temporary CUA catalog directory should be removed");
    shutdown.expect("CUA SDK should shut down");
}

/*
旧集成测试依赖 Engine 内部启动 OpenCode 服务，现由 LocalCliServiceLifecycle 负责，保留源码作为迁移留痕：
#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "requires CUA and OpenCode runtime"]
async fn cua_catalog_loads_in_opencode_tool_ids() {
    let sdk = Arc::new(crate::computer_control_sdk::CuaDriverSdk::new());
    let manifest_key = concat!("CAR", "G", "O_MANIFEST_DIR");
    sdk.set_resource_dir(Some(
        PathBuf::from(std::env::var(manifest_key).expect("manifest path"))
            .join("resources"),
    ));
    let run_dir = std::env::temp_dir().join(format!(
        "auracoder-opencode-tool-ids-{}",
        Uuid::new_v4()
    ));
    let mut connection = None;

    let result = async {
        sdk.initialize().map_err(anyhow::Error::msg)?;
        let service = Arc::new(ComputerControlService::new(sdk.clone()));
        let specs = service.sdk_tool_specs().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!specs.is_empty(), "CUA tool catalog is empty");
        std::fs::create_dir_all(&run_dir)?;
        connection = Some(start_server(&run_dir.to_string_lossy(), None, None).await?);
        let active_server = connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OpenCode connection was not created"))?;
        let response = reqwest::Client::new()
            .get(format!(
                "{}/experimental/tool/ids",
                active_server.base_url.trim_end_matches('/')
            ))
            .headers(auth_headers(&active_server.password))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "OpenCode /experimental/tool/ids returned HTTP {status}: {body}"
        );
        let payload: Value = serde_json::from_str(&body)
            .with_context(|| format!("OpenCode /experimental/tool/ids returned invalid JSON: {body}"))?;
        let ids = payload
            .as_array()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenCode /experimental/tool/ids response must be an array: {body}"
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    anyhow::anyhow!(
                        "OpenCode /experimental/tool/ids response contains a non-string id: {body}"
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = specs
            .iter()
            .map(|spec| {
                let name = spec["name"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("CUA SDK tool catalog contains a tool without name")
                })?;
                Ok(format!("auracoder_computer_control_{name}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let missing = expected
            .iter()
            .filter(|tool_id| !ids.iter().any(|id| id == *tool_id))
            .cloned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            missing.is_empty(),
            "OpenCode /experimental/tool/ids missing CUA tools: {}",
            missing.join(", ")
        );
        let hit_count = expected
            .iter()
            .filter(|tool_id| ids.iter().any(|id| id == *tool_id))
            .count();
        anyhow::ensure!(
            hit_count == specs.len(),
            "OpenCode /experimental/tool/ids matched {hit_count} CUA tools, expected {}",
            specs.len()
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(active_server) = connection.as_ref() {
        active_server.cancel_event_pump();
    }
    let shutdown = sdk.shutdown();
    let cleanup = std::fs::remove_dir_all(&run_dir);
    if let Err(error) = result {
        panic!("OpenCode tool id integration failed: {error:#}");
    }
    shutdown.expect("CUA SDK should shut down");
    cleanup.expect("temporary OpenCode cwd should be removed");
}
*/
#[test]
fn opencode_schema_source_supports_zod_types_constraints_and_errors() {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "enum": ["one", "two"],
                "description": "Name",
                "default": "one"
            },
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": 3
            },
            "ratio": { "type": "number", "format": "double" },
            "enabled": { "type": "boolean" },
            "tags": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": 2
            },
            "child": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": ["string", "null"],
                        "minLength": 2,
                        "maxLength": 4,
                        "pattern": "^[a-z]+$"
                    }
                },
                "required": ["code"],
                "additionalProperties": false
            },
            "expect": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "element": {
                            "type": ["object", "null"],
                            "properties": {
                                "exists": { "type": "boolean", "enum": [true] }
                            },
                            "required": ["exists"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["element"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["name", "count", "ratio", "enabled", "tags", "child", "expect"],
        "additionalProperties": false
    });
    let source = opencode_tool_args_source(&schema).expect("schema should convert");
    assert!(source
        .contains("tool.schema.enum([\"one\", \"two\"]).describe(\"Name\").default(\"one\")"));
    assert!(source.contains("tool.schema.number().int().min(1).max(3)"));
    assert!(source.contains("tool.schema.number()"));
    assert!(source.contains("tool.schema.boolean()"));
    assert!(source.contains("tool.schema.array(tool.schema.string()).min(1).max(2)"));
    assert!(source.contains(
        "tool.schema.string().min(2).max(4).regex(new RegExp(\"^[a-z]+$\")).nullable()"
    ));
    assert!(source.contains("\"name\": tool.schema.enum"));
    assert!(source.contains("\"exists\": tool.schema.literal(true)"));

    let boolean_union = opencode_zod_schema_source(&json!({
        "type": "boolean",
        "enum": [true, false]
    }))
    .expect("boolean enum should convert");
    assert_eq!(
        boolean_union,
        "tool.schema.union([tool.schema.literal(true), tool.schema.literal(false)])"
    );

    let integer_union = opencode_zod_schema_source(&json!({
        "type": "integer",
        "enum": [1, 2]
    }))
    .expect("integer enum should convert");
    assert_eq!(
        integer_union,
        "tool.schema.union([tool.schema.literal(1), tool.schema.literal(2)])"
    );

    let number_literal = opencode_zod_schema_source(&json!({
        "type": "number",
        "enum": [1.5]
    }))
    .expect("number enum should convert");
    assert_eq!(number_literal, "tool.schema.literal(1.5)");

    let nullable_enum = opencode_zod_schema_source(&json!({
        "type": ["string", "null"],
        "enum": ["x", null]
    }))
    .expect("nullable enum should convert");
    assert_eq!(
        nullable_enum,
        "tool.schema.union([tool.schema.literal(\"x\"), tool.schema.literal(null)])"
    );
    assert!(!nullable_enum.contains(".nullable()"));

    let mismatched_enum = opencode_zod_schema_source(&json!({
        "type": "boolean",
        "enum": ["true"]
    }))
    .expect_err("mismatched enum value should fail");
    assert!(mismatched_enum.to_string().contains("enum"));

    let null_without_nullable = opencode_zod_schema_source(&json!({
        "type": "string",
        "enum": [null]
    }))
    .expect_err("null enum value without nullable should fail");
    assert!(null_without_nullable.to_string().contains("enum"));

    let array_enum = opencode_zod_schema_source(&json!({
        "type": "array",
        "enum": [[]]
    }))
    .expect_err("array enum value should fail");
    assert!(array_enum.to_string().contains("enum"));

    let object_enum = opencode_zod_schema_source(&json!({
        "type": "object",
        "enum": [{}]
    }))
    .expect_err("object enum value should fail");
    assert!(object_enum.to_string().contains("enum"));

    let optional_source = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": { "optional": { "type": "string" } },
        "required": []
    }))
    .expect("optional schema should convert");
    assert!(optional_source.contains("optional\": tool.schema.string().optional()"));

    let unsupported_type = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": { "value": { "type": "null" } },
        "required": ["value"]
    }))
    .expect_err("unsupported type should fail");
    assert!(unsupported_type.to_string().contains("type"));

    let additional_properties = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": {},
        "additionalProperties": true
    }))
    .expect("additionalProperties=true should be accepted");
    assert_eq!(additional_properties, "{  }");

    let invalid_additional_properties = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": {},
        "additionalProperties": { "type": "string" }
    }))
    .expect_err("non-boolean additionalProperties should fail");
    assert!(invalid_additional_properties
        .to_string()
        .contains("additionalProperties"));

    let description_only = opencode_zod_schema_source(&json!({
        "description": "New value for `key`. JSON type depends on the key."
    }))
    .expect("description-only schema should convert to unknown");
    assert_eq!(
        description_only,
        "tool.schema.unknown().describe(\"New value for `key`. JSON type depends on the key.\")"
    );
    assert_eq!(
        opencode_zod_schema_source(&json!({})).expect("empty schema should convert to unknown"),
        "tool.schema.unknown()"
    );

    let missing_type_with_keywords = opencode_zod_schema_source(&json!({
        "properties": {},
        "description": "ambiguous"
    }))
    .expect_err("schema with keywords but no type should fail");
    assert!(missing_type_with_keywords
        .to_string()
        .contains("缺少 type 且包含无法解释的关键字"));

    let invalid_enum = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": { "value": { "type": "string", "enum": [] } }
    }))
    .expect_err("empty enum should fail");
    assert!(invalid_enum.to_string().contains("enum"));

    let mismatched_constraint = opencode_tool_args_source(&json!({
        "type": "object",
        "properties": { "value": { "type": "boolean", "minimum": 1 } }
    }))
    .expect_err("mismatched constraint should fail");
    assert!(mismatched_constraint.to_string().contains("minimum"));
}

#[test]
fn generated_auracoder_thread_tools_use_independent_default_exports() {
    let run_dir = std::env::temp_dir().join(format!(
        "auracoder-opencode-thread-tools-test-{}",
        Uuid::new_v4()
    ));
    write_opencode_auracoder_thread_mcp_tool(
        &run_dir,
        "http://127.0.0.1:45679/invoke",
        "thread-token",
        &[
            json!({
                "name": "get_auracoder_thread_message_count",
                "description": "Get message count",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string" }
                    },
                    "required": ["thread_id"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "get_auracoder_thread_messages_page",
                "description": "Get message page",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["thread_id", "page", "page_size"],
                    "additionalProperties": false
                }
            }),
        ],
    )
    .expect("AuraCoder thread tool sources should be generated");

    let tools_dir = run_dir.join(".opencode").join("tools");
    let count_source =
        std::fs::read_to_string(tools_dir.join("get_auracoder_thread_message_count.ts"))
            .expect("message count tool source should exist");
    let page_source =
        std::fs::read_to_string(tools_dir.join("get_auracoder_thread_messages_page.ts"))
            .expect("message page tool source should exist");
    for source in [&count_source, &page_source] {
        assert!(source.contains("import { tool } from \"@opencode-ai/plugin\""));
        assert!(source.contains("export default tool({"));
        assert!(!source.contains("parameters:"));
        assert!(source.contains("toolKind: \"auracoder_thread\""));
        assert!(source.contains("x-auracoder-computer-control-token"));
        assert!(source.contains("http://127.0.0.1:45679/invoke"));
        assert!(source.contains("\"thread-token\""));
        assert!(source.contains("return JSON.stringify(result);"));
        assert!(source.contains("if (!response.ok || result?.isError)"));
        assert!(source.contains("result?.isError"));
        assert!(source.contains("throw new Error"));
    }
    assert!(count_source.contains("\"thread_id\": tool.schema.string()"));
    assert!(count_source
        .contains("return invoke(\"get_auracoder_thread_message_count\", args, context);"));
    assert!(page_source.contains("\"thread_id\": tool.schema.string()"));
    assert!(page_source.contains("\"page\": tool.schema.number().int().min(1)"));
    assert!(page_source.contains("\"page_size\": tool.schema.number().int().min(1)"));
    assert!(page_source
        .contains("return invoke(\"get_auracoder_thread_messages_page\", args, context);"));
    assert!(!tools_dir.join("auracoder_thread_mcp.ts").exists());

    let _ = std::fs::remove_dir_all(run_dir);
}

#[test]
fn opencode_tool_names_are_normalized_before_authorization() {
    assert_eq!(
        normalize_opencode_tool_name("auracoder_computer_control_click"),
        "click"
    );
    assert_eq!(normalize_opencode_tool_name("click"), "click");
}

#[test]
fn is_user_message_only_matches_known_user_roles() {
    let mut roles = HashMap::new();
    roles.insert("user-message".to_string(), "user".to_string());
    roles.insert("assistant-message".to_string(), "assistant".to_string());

    assert!(is_user_message(&roles, "user-message"));
    assert!(!is_user_message(&roles, "assistant-message"));
    assert!(!is_user_message(&roles, "unknown"));
}

#[tokio::test]
async fn mapper_ignores_prompt_user_text_parts() {
    let engine = OpenCodeEngine::default();
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
    mapper.record_message("msg_user", "user", None);
    let part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_user",
        "messageID": "msg_user",
        "type": "text",
        "text": "hello"
    }))
    .unwrap();

    engine
        .handle_part_updated(&part, &mut mapper, &event_tx)
        .await;

    assert!(!mapper.content_seen);
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn mapper_flushes_pending_reasoning_after_part_type_is_known() {
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
    mapper.store_pending_text("prt_reasoning", "msg_assistant", "thinking");

    mapper
        .part_type_by_id
        .insert("prt_reasoning".to_string(), "reasoning".to_string());
    flush_pending_opencode_text_for_part(&mut mapper, &event_tx, "prt_reasoning").await;

    match event_rx
        .try_recv()
        .expect("expected pending reasoning to flush")
    {
        EngineEvent::ThinkingDelta { content } => assert_eq!(content, "thinking"),
        other => panic!("expected thinking delta, got {other:?}"),
    }
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn mapper_accepts_non_prompt_text_without_message_role() {
    let engine = OpenCodeEngine::default();
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
    let part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_text",
        "messageID": "msg_assistant",
        "type": "text",
        "text": "response"
    }))
    .unwrap();

    engine
        .handle_part_updated(&part, &mut mapper, &event_tx)
        .await;

    match event_rx.try_recv().expect("expected text delta") {
        EngineEvent::TextDelta { content } => assert_eq!(content, "response"),
        other => panic!("expected text delta, got {other:?}"),
    }
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn mapper_ignores_assistant_parts_for_previous_prompt() {
    let engine = OpenCodeEngine::default();
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_current_user".to_string());
    mapper.record_message("msg_old_user", "user", None);
    mapper.record_message("msg_old_assistant", "assistant", Some("msg_old_user"));
    let part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_old_text",
        "messageID": "msg_old_assistant",
        "type": "text",
        "text": "stale response"
    }))
    .unwrap();

    engine
        .handle_part_updated(&part, &mut mapper, &event_tx)
        .await;

    assert!(!mapper.content_seen);
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn idle_without_current_prompt_response_reports_error_without_completion() {
    let engine = OpenCodeEngine::default();
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_current_user".to_string());
    mapper.busy_seen = true;

    engine.complete_after_idle(&mut mapper, &event_tx).await;

    match event_rx.try_recv().expect("expected error event") {
        EngineEvent::Error {
            message,
            recoverable,
        } => {
            assert!(!recoverable);
            assert!(message.contains("without producing a response"));
        }
        other => panic!("expected error event, got {other:?}"),
    }
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn mapper_ignores_non_append_text_snapshots() {
    let engine = OpenCodeEngine::default();
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let mut mapper = OpenCodeTurnMapper::new("msg_user".to_string());
    let first_part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_reasoning",
        "messageID": "msg_assistant",
        "type": "reasoning",
        "text": "first"
    }))
    .unwrap();
    let revised_part: OpenCodePart = serde_json::from_value(json!({
        "id": "prt_reasoning",
        "messageID": "msg_assistant",
        "type": "reasoning",
        "text": "second"
    }))
    .unwrap();

    engine
        .handle_part_updated(&first_part, &mut mapper, &event_tx)
        .await;
    engine
        .handle_part_updated(&revised_part, &mut mapper, &event_tx)
        .await;

    match event_rx.try_recv().expect("expected initial reasoning") {
        EngineEvent::ThinkingDelta { content } => assert_eq!(content, "first"),
        other => panic!("expected thinking delta, got {other:?}"),
    }
    assert!(event_rx.try_recv().is_err());
}

#[test]
fn new_message_id_uses_opencode_ascending_shape() {
    let id = new_message_id();

    assert_eq!(id.len(), "msg_".len() + 26);
    assert!(id.starts_with("msg_"));

    let sortable = &id[4..16];
    let suffix = &id[16..];

    assert_eq!(sortable.len(), 12);
    assert!(sortable
        .chars()
        .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)));
    assert_eq!(suffix.len(), 14);
    assert!(suffix.chars().all(|ch| ch.is_ascii_alphanumeric()));
}

#[test]
fn opencode_prompt_message_path_uses_synchronous_message_endpoint() {
    assert_eq!(
        opencode_prompt_message_path("ses_123"),
        "/session/ses_123/message"
    );
}

#[test]
fn new_message_id_is_lexicographically_monotonic() {
    let mut previous = new_message_id();

    for _ in 0..1000 {
        let current = new_message_id();
        assert!(previous < current, "expected {previous} < {current}");
        previous = current;
    }
}

#[test]
fn opencode_sort_prefix_matches_observed_timestamp_formula() {
    assert_eq!(
        opencode_sort_prefix_for_millis(1_777_173_925_808, 1),
        "dc7d20fb0001"
    );
    assert_eq!(
        opencode_sort_prefix_for_millis(1_777_173_926_670, 1),
        "dc7d2130e001"
    );
}

#[test]
fn generated_style_user_id_sorts_between_observed_turn_messages() {
    let prior_assistant = "msg_dc7d20fb0001rH5hSHrepNXLgJ";
    let user_prefix = opencode_sort_prefix_for_millis(1_777_173_926_670, 1);
    let user_id = format!("msg_{user_prefix}00000000000000");
    let next_assistant = "msg_dc7d2132b001iU68RZlw7CFMwn";

    assert!(prior_assistant < user_id.as_str());
    assert!(user_id.as_str() < next_assistant);
}

#[test]
fn event_matching_reads_nested_part_session_id() {
    let event = OpenCodeBusEvent {
        event_type: "message.part.updated".to_string(),
        properties: json!({
            "part": {
                "sessionID": "ses_1",
                "id": "part_1"
            }
        }),
    };

    assert!(event_matches_session(&event, "ses_1"));
    assert!(!event_matches_session(&event, "ses_2"));
}

#[test]
fn extracts_persisted_opencode_approval_routes() {
    let route = extract_persisted_approval_route(&json!({
        "_opencodeRequestKind": "question",
        "_opencodeRequestID": "req_1",
        "_opencodeSessionID": "ses_1",
        "_opencodeCwd": "/tmp/project",
        "questions": [{ "id": "question-0", "question": "Run tests?" }]
    }))
    .unwrap();

    assert_eq!(route.server_method, "opencode/question");
    assert_eq!(route.raw_request_id["requestID"], "req_1");
    assert_eq!(route.raw_request_id["questions"][0]["id"], "question-0");
}
