use anyhow::{Context, ensure};
use std::net::IpAddr;
pub fn validate_server_base(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "server URL is required");
    ensure!(value.len() <= 2048, "server URL is too long");
    let url = sarmg_client_secure_http::Url::parse(value).context("invalid server URL")?;
    ensure!(!url.cannot_be_a_base(), "server URL must be hierarchical");
    ensure!(url.host_str().is_some(), "server URL must contain a host");
    ensure!(url.port() != Some(0), "server URL must not use port zero");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "server URL must not embed credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "server URL must not contain a query string or fragment"
    );
    ensure!(
        url.path() == "/",
        "server URL must be a complete management-console origin without a path; include only the scheme, host, and optional port"
    );
    sarmg_client_secure_http::client_network_policy(&url)
        .context("server violates Client network policy")?;
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub fn validate_activation_code(value: &str) -> anyhow::Result<&str> {
    ensure!(!value.is_empty(), "authorization key is required");
    ensure!(
        value.len() <= 256,
        "authorization key must not exceed 256 bytes"
    );
    ensure!(
        !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control()),
        "authorization key must not contain whitespace or control characters"
    );
    Ok(value)
}

pub(crate) fn is_loopback_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}
