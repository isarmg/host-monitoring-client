use anyhow::{Context, bail};
use uuid::Uuid;

pub(super) fn activation_endpoint(
    pairing_endpoint: &str,
) -> anyhow::Result<sarmg_client_secure_http::Url> {
    crate::config::validate_pairing_endpoint(pairing_endpoint)
        .context("stored pairing endpoint is unsafe")?;
    let mut endpoint = sarmg_client_secure_http::Url::parse(pairing_endpoint)
        .context("stored pairing endpoint is not a valid URL")?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        bail!("stored pairing endpoint unexpectedly contains credentials");
    }
    let path = endpoint.path().trim_end_matches('/');
    let base = path
        .strip_suffix("/pairing-requests")
        .context("stored pairing endpoint does not end in /pairing-requests")?;
    endpoint.set_path(&format!("{base}/activate"));
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

pub(super) fn validate_activation_url_request(
    activation_url: &str,
    pairing_endpoint: &str,
    request_id: Uuid,
) -> anyhow::Result<()> {
    let url = sarmg_client_secure_http::Url::parse(activation_url)
        .context("stored pairing activation URL is invalid")?;
    let pairing = sarmg_client_secure_http::Url::parse(pairing_endpoint)
        .context("stored pairing endpoint is invalid")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.scheme() != pairing.scheme()
        || url.host_str().map(str::to_ascii_lowercase)
            != pairing.host_str().map(str::to_ascii_lowercase)
        || url.port_or_known_default() != pairing.port_or_known_default()
        || url.path()
            != format!(
                "{}{request_id}",
                host_protocol::BROWSER_ACTIVATION_PATH_PREFIX
            )
    {
        bail!("stored activation URL does not match the pending pairing request");
    }
    Ok(())
}

pub(super) fn resolve_activation_url(
    pairing_endpoint: &str,
    activation_url: &str,
) -> anyhow::Result<String> {
    let base = sarmg_client_secure_http::Url::parse(pairing_endpoint)
        .context("invalid pairing endpoint URL")?;
    let url = match sarmg_client_secure_http::Url::parse(activation_url) {
        Ok(url) => url,
        Err(_) => base
            .join(activation_url)
            .context("invalid activation URL returned by Host Monitoring")?,
    };
    if !url.username().is_empty() || url.password().is_some() {
        bail!("Host Monitoring returned an activation URL containing credentials");
    }
    match url.scheme() {
        "https" => {}
        "http" if crate::pairing_input::is_loopback_host(url.host_str()) => {}
        "http" => bail!("Host Monitoring returned an insecure non-loopback activation URL"),
        scheme => bail!("Host Monitoring returned an unsupported activation URL scheme: {scheme}"),
    }
    Ok(url.to_string())
}
