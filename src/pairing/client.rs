use sarmg_client_secret::{SecretKey, SecretString};
use sarmg_client_secure_http::{StatusCode, header};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) fn random_secret() -> Arc<SecretString> {
    let random = SecretKey::new(rand::random::<[u8; 32]>());
    Arc::new(SecretString::new(hex(random.expose())))
}

pub(super) fn sha256_hex(secret: &SecretString) -> String {
    hex(&Sha256::digest(secret.expose().as_bytes()))
}

pub(super) fn pairing_authorization(secret: &SecretString) -> anyhow::Result<header::HeaderValue> {
    let value = SecretString::new(format!("Pairing {}", secret.expose()));
    let mut header = header::HeaderValue::from_str(value.expose())
        .map_err(|_| anyhow::anyhow!("invalid pairing authorization header"))?;
    header.set_sensitive(true);
    Ok(header)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    value
}

pub(super) fn json_headers() -> header::HeaderMap {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    headers
}

pub(super) fn pairing_response_content_type(
    response: &sarmg_client_secure_http::BoundedResponse,
) -> String {
    let raw = response
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>");
    pairing_content_type_for_diagnostics(raw)
}

pub(super) fn pairing_content_type_for_diagnostics(content_type: &str) -> String {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "application/json" | "text/html" | "text/plain" | "application/octet-stream" => media_type,
        "<missing>" => media_type,
        value if value.starts_with("application/") && value.ends_with("+json") => {
            "application/*+json".to_string()
        }
        _ => "<unexpected>".to_string(),
    }
}

pub(super) fn pairing_origin_for_diagnostics(endpoint: &str) -> String {
    sarmg_client_secure_http::Url::parse(endpoint)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null")
        .unwrap_or_else(|| "<invalid Server origin>".to_string())
}

pub(super) fn parse_pairing_json<T: DeserializeOwned>(
    body: &[u8],
    content_type: &str,
    endpoint: &str,
    response_kind: &str,
) -> anyhow::Result<T> {
    let invalid_response = || {
        let origin = pairing_origin_for_diagnostics(endpoint);
        let content_type = pairing_content_type_for_diagnostics(content_type);
        anyhow::anyhow!(
            "Host Monitoring returned an unexpected or malformed {response_kind} from Server origin {origin} (HTTP 2xx Content-Type: {content_type}); the configured Server address or port may be wrong. Use the complete Host Monitoring management-console origin, including its port"
        )
    };
    if pairing_content_type_for_diagnostics(content_type) != "application/json" {
        return Err(invalid_response());
    }
    serde_json::from_slice(body).map_err(|_| invalid_response())
}

pub(super) fn ensure_pairing_status(
    status: StatusCode,
    allowed: &[StatusCode],
    operation: &str,
) -> anyhow::Result<()> {
    if allowed.contains(&status) {
        return Ok(());
    }
    let _ = operation;
    Err(super::PairingHttpError {
        status: status.as_u16(),
    }
    .into())
}
