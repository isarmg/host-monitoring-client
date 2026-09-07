//! Unauthenticated management-origin health adapter; never reads Client state.
use super::validate_server_base;
use sarmg_client_secure_http::{ResponseBudget, SecureHttpClient, TlsConfig, header};
use serde::Deserialize;
use std::time::{Duration, Instant};

const MAX_SERVER_HEALTH_BODY_BYTES: usize = 16 * 1024;
const SERVER_HEALTH_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerHealthResponse {
    status: String,
    version: String,
    #[serde(rename = "uptime_seconds")]
    _uptime_seconds: i64,
}

#[derive(Debug)]
pub struct ServerConnectionStatus {
    pub status: &'static str,
    pub message: String,
    pub version: Option<String>,
    pub latency_ms: Option<u64>,
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn is_application_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

/// Lightweight management-origin reachability check for the standard-user tray.
///
/// This deliberately calls only the public `/health/live` endpoint and never reads
/// ProgramData credentials. It answers "can this desktop session reach Host Monitoring?";
/// the Server's host list remains authoritative for authenticated telemetry recency.
pub fn probe_server_connection(server: &str) -> ServerConnectionStatus {
    if server.trim().is_empty() {
        return ServerConnectionStatus {
            status: "unconfigured",
            message: "尚未配置 Server 地址".to_string(),
            version: None,
            latency_ms: None,
        };
    }
    let server = match validate_server_base(server) {
        Ok(server) => server,
        Err(_) => {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 地址无效".to_string(),
                version: None,
                latency_ms: None,
            };
        }
    };
    let mut health_url = match sarmg_client_secure_http::Url::parse(&server) {
        Ok(url) => url,
        Err(_) => {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 地址无效".to_string(),
                version: None,
                latency_ms: None,
            };
        }
    };
    health_url.set_path("/health/live");
    health_url.set_query(None);
    health_url.set_fragment(None);
    let client = match SecureHttpClient::new(
        SERVER_HEALTH_TIMEOUT,
        ResponseBudget {
            max_header_bytes: 16 * 1024,
            max_body_bytes: MAX_SERVER_HEALTH_BODY_BYTES,
        },
        TlsConfig::default(),
        format!("host-monitor-tray/{}", env!("CARGO_PKG_VERSION")),
    ) {
        Ok(client) => client,
        Err(_) => {
            return ServerConnectionStatus {
                status: "offline",
                message: "无法初始化 Server 连接检测".into(),
                version: None,
                latency_ms: None,
            };
        }
    };
    let started = Instant::now();
    let headers = [(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    )]
    .into_iter()
    .collect();
    let response = match client.get_client_blocking(health_url.as_str(), headers) {
        Ok(response) => response,
        Err(error) => {
            return ServerConnectionStatus {
                status: "offline",
                message: match error {
                    sarmg_client_secure_http::Error::Timeout => "Server 连接超时",
                    sarmg_client_secure_http::Error::ResponseTooLarge => {
                        "Server 健康响应读取失败或过大"
                    }
                    _ => "无法连接 Server（请检查地址、端口、网络或 TLS）",
                }
                .into(),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
    };
    let status = response.status;
    if status != sarmg_client_secure_http::StatusCode::OK {
        return ServerConnectionStatus {
            status: "offline",
            message: format!("Server 返回 HTTP {}", status.as_u16()),
            version: None,
            latency_ms: Some(elapsed_milliseconds(started)),
        };
    }
    let content_type_is_json = response
        .headers
        .get(sarmg_client_secure_http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_application_json);
    if !content_type_is_json {
        return ServerConnectionStatus {
            status: "offline",
            message: "Server 健康响应的 Content-Type 不是 application/json".to_string(),
            version: None,
            latency_ms: Some(elapsed_milliseconds(started)),
        };
    }
    let body = response.body;
    let health: ServerHealthResponse = match serde_json::from_slice(&body) {
        Ok(health) => health,
        Err(_) => {
            return ServerConnectionStatus {
                status: "offline",
                message: "Server 未返回可用的 Host Monitoring 健康状态（格式或版本信息无效）"
                    .to_string(),
                version: None,
                latency_ms: Some(elapsed_milliseconds(started)),
            };
        }
    };
    let ServerHealthResponse {
        status,
        version,
        _uptime_seconds: _,
    } = health;
    if status != "ok" {
        return ServerConnectionStatus {
            status: "offline",
            message: "Server 健康状态不可用".to_string(),
            version: None,
            latency_ms: Some(elapsed_milliseconds(started)),
        };
    }
    let latency_ms = elapsed_milliseconds(started);
    if version.trim().is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        return ServerConnectionStatus {
            status: "offline",
            message: "Server 版本信息不可用".to_string(),
            version: None,
            latency_ms: Some(latency_ms),
        };
    }
    if version != env!("CARGO_PKG_VERSION") {
        return ServerConnectionStatus {
            status: "offline",
            message: format!("Server 版本不匹配：需要 v{}", env!("CARGO_PKG_VERSION")),
            version: None,
            latency_ms: Some(latency_ms),
        };
    }
    let message = format!("连接正常 · Server v{version} · {latency_ms} ms");
    ServerConnectionStatus {
        status: "online",
        message,
        version: Some(version),
        latency_ms: Some(latency_ms),
    }
}

#[cfg(test)]
mod tests;
