use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn probe_health_body(body: &str) -> ServerConnectionStatus {
    probe_health_response("200 OK", Some("application/json"), body)
}

fn probe_health_response(
    status: &str,
    content_type: Option<&str>,
    body: &str,
) -> ServerConnectionStatus {
    let content_type = content_type
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    probe_raw(response)
}

fn probe_raw(response: String) -> ServerConnectionStatus {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("health fixture accept: {error}"),
            }
        };
        // macOS and Windows may inherit nonblocking mode from the listener.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0 && request.len() + count <= 8192);
            request.extend_from_slice(&chunk[..count]);
        }
        let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /health/live http/1.1\r\n"));
        assert!(!request.contains("authorization:") && !request.contains("cookie:"));
        let _ = stream.write_all(response.as_bytes());
    });
    let result = probe_server_connection(&format!("http://{address}"));
    worker.join().unwrap();
    result
}

#[test]
fn health_probe_enforces_budgets_and_does_not_follow_redirects() {
    let body = format!(
        r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
        env!("CARGO_PKG_VERSION")
    );
    let exact = format!(
        "{body}{}",
        " ".repeat(MAX_SERVER_HEALTH_BODY_BYTES - body.len())
    );
    assert_eq!(probe_health_body(&exact).status, "online");
    let excessive = probe_health_body(&format!("{exact} "));
    assert_eq!(excessive.status, "offline");
    assert!(excessive.message.contains("过大"));
    let excessive_headers = probe_raw(format!(
        "HTTP/1.1 200 OK\r\nX-Secret: {}\r\nContent-Length: 0\r\n\r\n",
        "private-marker".repeat(1500)
    ));
    assert_eq!(excessive_headers.status, "offline");
    assert!(!format!("{excessive_headers:?}").contains("private-marker"));
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let redirected = probe_raw(format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/\r\nContent-Length: 0\r\n\r\n",
        trap.local_addr().unwrap()
    ));
    assert_eq!(redirected.status, "offline");
    assert!(redirected.message.contains("307"));
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn health_probe_rejects_unsafe_origins_and_reflected_secrets() {
    for origin in [
        "http://192.0.2.1",
        "https://secret:password@example.com",
        "https://example.com/path",
        "https://example.com/?secret=marker",
    ] {
        let result = probe_server_connection(origin);
        assert_eq!(result.status, "offline");
        assert!(!format!("{result:?}").contains("secret"));
    }
    for body in [
        r#"{"secret":"private-marker"}"#,
        r#"{"status":"ok","version":"private-marker","uptime_seconds":1}"#,
    ] {
        let result = probe_health_body(body);
        assert_eq!(result.status, "offline");
        assert!(!format!("{result:?}").contains("private-marker"));
    }
}

#[test]
fn connection_probe_distinguishes_unconfigured_and_healthy_server() {
    let unconfigured = probe_server_connection("");
    assert_eq!(unconfigured.status, "unconfigured");
    assert!(unconfigured.version.is_none());

    let healthy = probe_health_body(&format!(
        r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
        env!("CARGO_PKG_VERSION")
    ));
    assert_eq!(healthy.status, "online");
    assert_eq!(healthy.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    assert!(healthy.latency_ms.is_some());
}

#[test]
fn connection_probe_rejects_missing_or_mismatched_server_version() {
    let missing = probe_health_body(r#"{"status":"ok","uptime_seconds":1}"#);
    assert_eq!(missing.status, "offline");
    assert!(missing.version.is_none());
    assert_eq!(
        missing.message,
        "Server 未返回可用的 Host Monitoring 健康状态（格式或版本信息无效）"
    );

    let mismatched = probe_health_body(
        r#"{"status":"ok","version":"incompatible-test-version","uptime_seconds":1}"#,
    );
    assert_eq!(mismatched.status, "offline");
    assert!(mismatched.version.is_none());
    assert!(!format!("{mismatched:?}").contains("incompatible-test-version"));
    assert!(mismatched.message.contains("版本不匹配"));
    assert!(mismatched.message.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn connection_probe_rejects_non_host_monitoring_success_response() {
    let invalid = probe_health_body("<html>not Host Monitoring</html>");
    assert_eq!(invalid.status, "offline");
    assert!(invalid.message.contains("Host Monitoring"));

    let current_body = format!(
        r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
        env!("CARGO_PKG_VERSION")
    );
    let wrong_status =
        probe_health_response("204 No Content", Some("application/json"), &current_body);
    assert_eq!(wrong_status.status, "offline");
    assert!(wrong_status.message.contains("204"));

    for content_type in [
        None,
        Some("text/plain"),
        Some("application/vnd.host-monitoring+json"),
    ] {
        let wrong_type = probe_health_response("200 OK", content_type, &current_body);
        assert_eq!(wrong_type.status, "offline");
        assert!(wrong_type.message.contains("Content-Type"));
    }
}

#[test]
fn health_dto_is_strict() {
    let health_response = format!(
        r#"{{"status":"ok","version":"{}","uptime_seconds":1}}"#,
        env!("CARGO_PKG_VERSION")
    );
    assert!(serde_json::from_str::<ServerHealthResponse>(&health_response).is_ok());
    assert!(
        serde_json::from_str::<ServerHealthResponse>(r#"{"status":"ok","uptime_seconds":1}"#)
            .is_err()
    );
    let health_response_with_unknown_field = format!(
        r#"{{"status":"ok","version":"{}","uptime_seconds":1,"unknown_extension":true}}"#,
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        serde_json::from_str::<ServerHealthResponse>(&health_response_with_unknown_field).is_err()
    );
}
