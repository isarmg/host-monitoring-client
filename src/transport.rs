#[cfg(test)]
use std::fs;
use std::{error::Error as _, sync::Arc, time::Duration};

#[cfg(feature = "otlp")]
use std::io::Write;

use anyhow::{Context, bail};
#[cfg(feature = "otlp")]
use flate2::{Compression, write::GzEncoder};
use reqwest::{Certificate, Identity, Request, StatusCode, header};
use url::Url;
use uuid::Uuid;
use xcsc_error::ErrorEnvelope;
use xcsc_runtime::{ClientIdentity, CredentialSnapshot, CredentialStore};
use xcsc_secret::{SecretBytes, SecretString};

use xsos_protocol::{
    ClientReportAck, CredentialStatus, CredentialStatusResponse, HOST_PAIRING_PROTOCOL_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteBindingStatus {
    Authorized { host_id: String },
    Unauthorized,
    ProtocolUnsupported { received: u16, supported: Vec<u16> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteBindingFailure {
    InvalidEndpoint,
    LocalCredentialCorrupt,
    ConnectionFailed,
    Timeout,
    TlsValidationFailed,
    ResponseTooLarge,
    HttpTransportFailed,
    ServerUpgradeRequired,
    ReverseProxyMisconfigured,
    RateLimited,
    ServerUnavailable { status: u16 },
    UnexpectedRedirect { status: u16 },
    ContractMismatch { status: u16 },
    AuthResponseUntrusted,
}

impl RemoteBindingFailure {
    pub fn stable_code(&self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "pairing_invalid_endpoint",
            Self::LocalCredentialCorrupt => "local_credential_corrupt",
            Self::ConnectionFailed => "pairing_connection_failed",
            Self::Timeout => "pairing_connection_timeout",
            Self::TlsValidationFailed => "pairing_tls_untrusted",
            Self::ResponseTooLarge => "pairing_response_too_large",
            Self::HttpTransportFailed => "pairing_http_transport_failed",
            Self::ServerUpgradeRequired => "pairing_server_upgrade_required",
            Self::ReverseProxyMisconfigured => "pairing_reverse_proxy_misconfigured",
            Self::RateLimited => "pairing_rate_limited",
            Self::ServerUnavailable { .. } => "pairing_server_unavailable",
            Self::UnexpectedRedirect { .. } => "pairing_unexpected_redirect",
            Self::ContractMismatch { .. } => "pairing_server_contract_mismatch",
            Self::AuthResponseUntrusted => "pairing_auth_response_untrusted",
        }
    }
}

use crate::{
    config::ClientConfig,
    model::ClientReport,
    report_contract,
    state_store::{StateFile, StateReader, StateTransaction},
};

const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpTransportError {
    #[error("connection failed")]
    Connection,
    #[error("connection timed out")]
    Timeout,
    #[error("TLS validation failed")]
    Tls,
    #[error("response exceeds size limit")]
    ResponseTooLarge,
    #[error("HTTP transport failed")]
    Transport,
}

pub(crate) struct HttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: header::HeaderMap,
    pub(crate) body: Vec<u8>,
}

fn classify_reqwest_error(error: &reqwest::Error) -> HttpTransportError {
    if error.is_timeout() {
        return HttpTransportError::Timeout;
    }
    let mut source = error.source();
    while let Some(current) = source {
        let message = current.to_string().to_ascii_lowercase();
        if ["certificate", "tls", "ssl", "handshake"]
            .iter()
            .any(|marker| message.contains(marker))
        {
            return HttpTransportError::Tls;
        }
        source = current.source();
    }
    if error.is_connect() {
        HttpTransportError::Connection
    } else {
        HttpTransportError::Transport
    }
}

pub(crate) async fn execute_bounded(
    client: &reqwest::Client,
    request: Request,
    maximum: usize,
) -> Result<HttpResponse, HttpTransportError> {
    if request.body().is_some_and(|body| {
        body.as_bytes()
            .is_none_or(|bytes| bytes.len() > MAX_REQUEST_BYTES)
    }) {
        return Err(HttpTransportError::Transport);
    }
    let response = client
        .execute(request)
        .await
        .map_err(|error| classify_reqwest_error(&error))?;
    let header_bytes = response
        .headers()
        .iter()
        .try_fold(0usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len())?
                .checked_add(value.as_bytes().len())
        });
    if header_bytes.is_none_or(|size| size > MAX_HEADER_BYTES)
        || response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
    {
        return Err(HttpTransportError::ResponseTooLarge);
    }
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_reqwest_error(&error))?
    {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > maximum)
        {
            return Err(HttpTransportError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

pub(crate) async fn post_bounded(
    client: &reqwest::Client,
    url: &str,
    headers: header::HeaderMap,
    body: Vec<u8>,
) -> Result<HttpResponse, HttpTransportError> {
    let request = client
        .post(url)
        .headers(headers)
        .body(body)
        .build()
        .map_err(|_| HttpTransportError::Transport)?;
    execute_bounded(client, request, MAX_ERROR_RESPONSE_BYTES).await
}

#[cfg(all(test, target_os = "linux"))]
#[path = "transport/tls_tests.rs"]
mod tls_tests;

#[derive(Clone)]
pub struct Reporter {
    identity: ClientIdentity,
    client: reqwest::Client,
    endpoint: String,
    token: Arc<SecretString>,
    credential_revision: (Uuid, Uuid),
    // 仅 otlp feature 下读取；无该 feature 时保留字段以维持构造逻辑一致。
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    otlp_endpoint: Option<String>,
    #[cfg_attr(not(feature = "otlp"), allow(dead_code))]
    otlp_token: Option<Arc<SecretString>>,
}

impl Reporter {
    pub fn new(config: &ClientConfig) -> anyhow::Result<Self> {
        crate::pairing::reporter_for_current_active_state(config)?.context(
            "a complete current-version Active pairing state is required before creating the reporter",
        )
    }

    /// Build a reporter only from an already-issued long-lived credential.
    /// This never performs pairing or network I/O and is used while the
    /// pairing state lock protects the token/config snapshot from an
    /// overlapping browser-pairing commit.
    pub(crate) fn for_existing_credential(
        config: &ClientConfig,
        store: &StateTransaction,
    ) -> anyhow::Result<Option<Self>> {
        let Some(credential) = crate::pairing::HostCredentials::new(config, store).load()? else {
            return Ok(None);
        };
        let client = build_client(config)?;
        Self::with_client_and_credential(config, client, credential).map(Some)
    }

    pub fn credential_revision(&self) -> (Uuid, Uuid) {
        self.credential_revision
    }

    pub fn identity(&self) -> &ClientIdentity {
        &self.identity
    }

    pub fn new_with_timeout(config: &ClientConfig, timeout: Duration) -> anyhow::Result<Self> {
        let mut reporter = Self::new(config)?;
        reporter.client = build_client_with_timeout(config, timeout)?;
        Ok(reporter)
    }

    pub async fn verify_remote_binding(&self) -> Result<RemoteBindingStatus, RemoteBindingFailure> {
        let mut url = match Url::parse(&self.endpoint) {
            Ok(url) => url,
            Err(_) => return Err(RemoteBindingFailure::InvalidEndpoint),
        };
        url.set_path(xsos_protocol::CLIENT_CREDENTIAL_STATUS_PATH);
        url.set_query(None);
        url.set_fragment(None);
        let headers = match authenticated_headers(&self.token, "application/json") {
            Ok(headers) => headers,
            Err(_) => return Err(RemoteBindingFailure::LocalCredentialCorrupt),
        };
        let request = match self.client.get(url).headers(headers).build() {
            Ok(request) => request,
            Err(_) => return Err(RemoteBindingFailure::InvalidEndpoint),
        };
        let response = match execute_bounded(&self.client, request, MAX_ERROR_RESPONSE_BYTES).await
        {
            Ok(response) => response,
            Err(error) => return Err(classify_http_error(error)),
        };
        let content_type = response
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        classify_credential_status_response(
            self.identity.instance_id(),
            response.status,
            content_type,
            &response.body,
        )
    }

    fn validate_report_identity(&self, report: &ClientReport) -> anyhow::Result<()> {
        self.identity
            .ensure_matches(&crate::client_identity::for_instance(&report.host.id)?)?;
        Ok(())
    }

    fn with_client_and_credential(
        config: &ClientConfig,
        client: reqwest::Client,
        credential: CredentialSnapshot<(Uuid, Uuid)>,
    ) -> anyhow::Result<Self> {
        if credential.secret.expose().trim().is_empty() {
            bail!("the per-host token is empty");
        }
        credential
            .identity
            .ensure_matches(&crate::client_identity::for_instance(
                credential.identity.instance_id(),
            )?)?;
        Ok(Self {
            identity: credential.identity,
            client,
            endpoint: config.endpoint.clone(),
            token: credential.secret,
            credential_revision: credential.revision,
            otlp_endpoint: config.otlp_endpoint.clone(),
            otlp_token: config.otlp_token.clone(),
        })
    }

    pub async fn send_xsos(&self, report: &ClientReport) -> Result<(), SendError> {
        self.send_xsos_inner(report, None).await
    }

    /// Preserve the report ID and every field of a durable queued report across retries.
    /// A changed wall clock must never make the same report ID carry a new body.
    pub async fn send_queued_xsos(
        &self,
        report: &ClientReport,
        body: &[u8],
    ) -> Result<(), SendError> {
        self.send_xsos_inner(report, Some(body)).await
    }

    async fn send_xsos_inner(
        &self,
        report: &ClientReport,
        queued_body: Option<&[u8]>,
    ) -> Result<(), SendError> {
        crate::runtime_status::observe(
            "last_delivery_attempt_at",
            serde_json::json!(chrono::Utc::now().timestamp()),
        );
        let (bounded, body) =
            prepare_report_body(report, queued_body).inspect_err(record_delivery_failure)?;
        // A different valid identity is not an ACK or permanent content rejection.
        // Keep the record and current credential; never send it with another identity's token.
        self.validate_report_identity(&bounded)
            .map_err(|_| SendError::IdentityMismatch)
            .inspect_err(record_delivery_failure)?;
        let headers = authenticated_headers(&self.token, "application/json")
            .map_err(|_| SendError::Transient("invalid host authorization header".into()))
            .inspect_err(record_delivery_failure)?;
        let response = post_bounded(&self.client, &self.endpoint, headers, body)
            .await
            .map_err(|error| SendError::Transient(format!("xsos request failed: {error}")))
            .inspect_err(record_delivery_failure)?;
        let response_status = response.status.as_u16();
        let content_type = response
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        let result = validate_xsos_ack(response.status, content_type, &response.body, &bounded);
        if result.is_ok() {
            crate::runtime_status::observe(
                "last_ack_at",
                serde_json::json!(chrono::Utc::now().timestamp()),
            );
            crate::runtime_status::observe("last_delivery_result", serde_json::json!("accepted"));
            crate::runtime_status::observe("last_http_status", serde_json::json!(202));
            crate::runtime_status::observe("last_error_code", serde_json::Value::Null);
        } else if let Err(error) = &result {
            crate::runtime_status::observe("last_delivery_result", serde_json::json!("rejected"));
            crate::runtime_status::observe(
                "last_http_status",
                serde_json::Value::from(response_status),
            );
            crate::runtime_status::observe(
                "last_error_code",
                serde_json::json!(error.stable_code()),
            );
        }
        if matches!(
            &result,
            Err(SendError::Transient(_) | SendError::FutureTimestamp(_))
        ) && let Some(delay) = retry_after_delay(response.status, &response.headers)
        {
            tokio::time::sleep(delay).await;
        }
        result
    }

    #[cfg(feature = "otlp")]
    pub async fn send_otlp(&self, report: &ClientReport) -> anyhow::Result<()> {
        use prost::Message;
        let Some(endpoint) = &self.otlp_endpoint else {
            return Ok(());
        };
        self.validate_report_identity(report)?;
        let request = crate::otlp::encode_report(report);
        let mut protobuf = Vec::with_capacity(request.encoded_len());
        request.encode(&mut protobuf)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&protobuf)?;
        let body = encoder.finish()?;
        let mut headers = if let Some(token) = &self.otlp_token {
            authenticated_headers(token, "application/x-protobuf")?
        } else {
            let mut headers = header::HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/x-protobuf"),
            );
            headers
        };
        headers.insert(
            header::CONTENT_ENCODING,
            header::HeaderValue::from_static("gzip"),
        );
        let response = post_bounded(&self.client, endpoint, headers, body).await?;
        Ok(ensure_generic_success(response.status, "OTLP")?)
    }

    #[cfg(not(feature = "otlp"))]
    pub async fn send_otlp(&self, _report: &ClientReport) -> anyhow::Result<()> {
        Ok(())
    }
}

fn prepare_report_body(
    report: &ClientReport,
    queued_body: Option<&[u8]>,
) -> Result<(ClientReport, Vec<u8>), SendError> {
    let invalid = |reason: &str| SendError::Permanent(format!("invalid Client report: {reason}"));
    if let Some(body) = queued_body {
        if body.len() > crate::model::CLIENT_REPORT_MAX_BODY_BYTES {
            return Err(invalid("queued body exceeds the report size limit"));
        }
        let decoded: ClientReport =
            serde_json::from_slice(body).map_err(|_| invalid("queued body is malformed"))?;
        if decoded != *report {
            return Err(invalid("queued body does not match its parsed report"));
        }
        let (canonical, _) = report_contract::canonical_spool_report(report)
            .map_err(|_| invalid("queued report violates the current contract"))?;
        if canonical != *report {
            return Err(invalid("queued report is not canonical"));
        }
        return Ok((report.clone(), body.to_vec()));
    }
    report_contract::encode_report_body(report).map_err(|error| invalid(&error.to_string()))
}

fn retry_after_delay(status: StatusCode, headers: &header::HeaderMap) -> Option<Duration> {
    if status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::SERVICE_UNAVAILABLE {
        return None;
    }
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds = value.parse::<u64>().ok()?;
    (seconds > 0).then(|| Duration::from_secs(seconds.min(300)))
}

fn classify_credential_status_response(
    expected_host_id: &str,
    status_code: StatusCode,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<RemoteBindingStatus, RemoteBindingFailure> {
    if status_code == StatusCode::OK && content_type.is_some_and(is_application_json) {
        let Ok(status) = serde_json::from_slice::<CredentialStatusResponse>(body) else {
            return Err(RemoteBindingFailure::ContractMismatch {
                status: status_code.as_u16(),
            });
        };
        let CredentialStatus::Authorized = status.status;
        if status.protocol_version != HOST_PAIRING_PROTOCOL_VERSION {
            return Ok(RemoteBindingStatus::ProtocolUnsupported {
                received: HOST_PAIRING_PROTOCOL_VERSION,
                supported: vec![status.protocol_version],
            });
        }
        if status.host_id == status.instance_id && status.host_id == expected_host_id {
            return Ok(RemoteBindingStatus::Authorized {
                host_id: status.host_id,
            });
        }
        return Err(RemoteBindingFailure::ContractMismatch {
            status: status_code.as_u16(),
        });
    }
    let envelope = content_type
        .filter(|value| is_application_json(value))
        .and_then(|_| serde_json::from_slice::<ErrorEnvelope>(body).ok());
    if status_code == StatusCode::UNAUTHORIZED
        && envelope
            .as_ref()
            .is_some_and(|error| error.code.as_str() == "unauthorized" && !error.retryable)
    {
        return Ok(RemoteBindingStatus::Unauthorized);
    }
    if status_code == StatusCode::BAD_REQUEST
        && let Some(error) = envelope
        && error.code.as_str() == "unsupported_client_protocol"
        && !error.retryable
    {
        let received = error
            .details
            .get("received")
            .and_then(|value| value.as_u64());
        let supported = error
            .details
            .get("supported")
            .and_then(|value| value.as_array());
        if let (Some(received), Some(supported)) = (received, supported)
            && let Ok(received) = u16::try_from(received)
        {
            let supported: Vec<u16> = supported
                .iter()
                .filter_map(|value| value.as_u64().and_then(|v| u16::try_from(v).ok()))
                .collect();
            if !supported.is_empty() {
                return Ok(RemoteBindingStatus::ProtocolUnsupported {
                    received,
                    supported,
                });
            }
        }
    }
    match status_code.as_u16() {
        300..=399 => Err(RemoteBindingFailure::UnexpectedRedirect {
            status: status_code.as_u16(),
        }),
        401 => Err(RemoteBindingFailure::AuthResponseUntrusted),
        404 | 405 | 426 => Err(RemoteBindingFailure::ServerUpgradeRequired),
        421 => Err(RemoteBindingFailure::ReverseProxyMisconfigured),
        429 => Err(RemoteBindingFailure::RateLimited),
        500..=599 => Err(RemoteBindingFailure::ServerUnavailable {
            status: status_code.as_u16(),
        }),
        _ => Err(RemoteBindingFailure::ContractMismatch {
            status: status_code.as_u16(),
        }),
    }
}

fn classify_http_error(error: HttpTransportError) -> RemoteBindingFailure {
    match error {
        HttpTransportError::Connection => RemoteBindingFailure::ConnectionFailed,
        HttpTransportError::Timeout => RemoteBindingFailure::Timeout,
        HttpTransportError::Tls => RemoteBindingFailure::TlsValidationFailed,
        HttpTransportError::ResponseTooLarge => RemoteBindingFailure::ResponseTooLarge,
        HttpTransportError::Transport => RemoteBindingFailure::HttpTransportFailed,
    }
}

fn record_delivery_failure(error: &SendError) {
    crate::runtime_status::observe("last_delivery_result", serde_json::json!("rejected"));
    crate::runtime_status::observe(
        "last_http_status",
        error
            .http_status()
            .map_or(serde_json::Value::Null, serde_json::Value::from),
    );
    crate::runtime_status::observe("last_error_code", serde_json::json!(error.stable_code()));
}

fn authenticated_headers(
    token: &SecretString,
    content_type: &'static str,
) -> anyhow::Result<header::HeaderMap> {
    let value = SecretString::new(format!("Bearer {}", token.expose()));
    let mut value = header::HeaderValue::from_str(value.expose())
        .map_err(|_| anyhow::anyhow!("invalid authorization header"))?;
    value.set_sensitive(true);
    let mut headers = header::HeaderMap::new();
    headers.insert(header::AUTHORIZATION, value);
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    Ok(headers)
}

/// A deliberately source-free diagnostic: backend errors can contain secrets
/// or configured paths, so they must not be included in Display, Debug or chains.
#[derive(Debug, thiserror::Error)]
#[error(
    "local TLS configuration is invalid; check identity format, password, CA certificates, file safety and the 1 MiB input limit"
)]
pub struct LocalTlsConfigurationError;

#[derive(Debug, thiserror::Error)]
#[error("credential does not use the current fixed encoding")]
pub struct LocalCredentialCorrupt;

/// Construct and discard the same transport used for delivery, without DNS,
/// requests, credential locks, state creation or file repairs. Success validates
/// local inputs only, not peer trust, expiry at handshake time or connectivity.
pub fn validate_local_tls(config: &ClientConfig) -> Result<(), LocalTlsConfigurationError> {
    build_client(config)
        .map(|_| ())
        .map_err(|_| LocalTlsConfigurationError)
}

/// Read-only, bounded credential-content check. False means empty/whitespace;
/// missing and unsafe files are errors. This does not assert authorization or
/// validate a multi-file Active binding, and does not acquire transaction locks.
pub fn stored_credential_is_nonempty(config: &ClientConfig) -> std::io::Result<bool> {
    let reader = StateReader::open(&config.state_dir)?;
    let bytes = SecretBytes::new(reader.read(StateFile::Credential)?);
    let text = std::str::from_utf8(bytes.expose()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "credential is not UTF-8")
    })?;
    validate_current_token(text).map_err(std::io::Error::other)?;
    Ok(true)
}

pub(crate) fn build_client(config: &ClientConfig) -> anyhow::Result<reqwest::Client> {
    build_client_with_timeout(config, config.request_timeout())
}

fn build_client_with_timeout(
    config: &ClientConfig,
    timeout: Duration,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::none())
        .http2_max_header_list_size(MAX_HEADER_BYTES as u32)
        .user_agent(format!("xsoc/{}", env!("CARGO_PKG_VERSION")));
    #[cfg(any(windows, target_os = "macos"))]
    {
        builder = builder.tls_backend_native();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        builder = builder.tls_backend_rustls();
    }
    if config.tls_identity_password.is_some() && config.tls_identity_pkcs12.is_none() {
        bail!("tls_identity_password requires tls_identity_pkcs12");
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if config.tls_identity_pkcs12.is_some() {
            bail!(
                "tls_identity_pkcs12 is supported only on Windows and macOS; use \
                 tls_identity_pem on this platform"
            );
        }
        if let Some(path) = &config.tls_identity_pem {
            let bytes = crate::tls_input::read(path, crate::tls_input::TlsInput::Identity)
                .context("failed to read TLS identity")?;
            builder = builder.identity(
                Identity::from_pem(bytes.expose())
                    .map_err(|_| anyhow::anyhow!("invalid TLS PEM identity"))?,
            );
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        if config.tls_identity_pem.is_some() {
            bail!(
                "the native TLS backend requires tls_identity_pkcs12 instead of tls_identity_pem"
            );
        }
        if let Some(path) = &config.tls_identity_pkcs12 {
            let bytes = crate::tls_input::read(path, crate::tls_input::TlsInput::Identity)
                .context("failed to read TLS identity")?;
            builder = builder.identity(
                Identity::from_pkcs12_der(
                    bytes.expose(),
                    config
                        .tls_identity_password
                        .as_deref()
                        .map(SecretString::expose)
                        .unwrap_or(""),
                )
                .map_err(|_| anyhow::anyhow!("invalid TLS PKCS#12 identity or password"))?,
            );
        }
    }
    if let Some(path) = &config.tls_ca_pem {
        let bytes = crate::tls_input::read(path, crate::tls_input::TlsInput::TrustAnchor)
            .context("failed to read TLS CA")?;
        let certificates = Certificate::from_pem_bundle(bytes.expose())
            .map_err(|_| anyhow::anyhow!("invalid TLS CA certificate"))?;
        anyhow::ensure!(
            !certificates.is_empty(),
            "TLS CA input contains no certificates"
        );
        builder = builder.tls_certs_merge(certificates);
    }
    Ok(builder.build()?)
}

pub(crate) fn read_secret(store: &StateReader, kind: &str) -> anyhow::Result<SecretString> {
    let path = store.path(StateFile::Credential);
    let bytes = SecretBytes::new(
        store
            .read(StateFile::Credential)
            .with_context(|| format!("failed to read {kind} {}", path.display()))?,
    );
    let token =
        std::str::from_utf8(bytes.expose()).with_context(|| format!("{kind} is not UTF-8"))?;
    validate_current_token(token)
        .with_context(|| format!("{kind} {} is corrupt", path.display()))?;
    Ok(SecretString::new(token.to_owned()))
}

pub(crate) fn validate_current_token(token: &str) -> Result<(), LocalCredentialCorrupt> {
    if token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(LocalCredentialCorrupt)
    }
}

/// 上报失败的性质。判据是**要让同一份报文最终被接受，需要改变什么**：
///
/// | 变体 | 需要改变的东西 | 处置 |
/// |---|---|---|
/// | `Permanent`  | 报文内容本身（改不了） | 丢弃 |
/// | `Unauthorized` | 服务端稳定 `unauthorized` 机器码确认凭据失效 | 使用授权恢复流程重新配对；仅明确放弃旧身份时才替换实例 |
/// | `IdentityMismatch` | 报告不属于当前凭据身份 | 保留原字节隔离，继续队列 |
/// | `Transient`  | 等待网络或服务恢复 | 保留并退避重试 |
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// Local mismatch: preserve original bytes in quarantine; do not contact the
    /// network, authorize deletion, or invalidate a newer credential.
    #[error("report does not match the active Client identity")]
    IdentityMismatch,
    /// 服务端以严格当前 envelope 拒绝了报文内容本身（400/409/413）。重试必然
    /// 再次失败，继续入队只会让 spool 被必失败的数据占满并挤掉后续有效报文。
    #[error("{0}")]
    Permanent(String),
    /// xsos 以 401 和稳定 `unauthorized` 机器码确认凭据不被接受。主机进入
    /// `reauth_required`，需要显式执行授权恢复配对；Client 不会自动生成或替换凭据，
    /// 也不会在恢复过程中改换 Host UUID。代理/WAF 生成的未知 401 不得使用此变体。
    #[error("{0}")]
    Unauthorized(String),
    /// The Server rejected the wire version. Re-pairing cannot repair this; do not retry the report.
    #[error("{0}")]
    UnsupportedProtocol(String),
    /// 网络故障或服务端暂时不可用，保留记录并退避重试。
    #[error("{0}")]
    Transient(String),
    /// The original queued bytes must be retained until the Server's clock
    /// admits their collection timestamp.
    #[error("{0}")]
    FutureTimestamp(String),
}

impl SendError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_) | Self::UnsupportedProtocol(_))
    }

    /// 凭据已失效，需要显式恢复授权后才可能成功。
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized(_))
    }

    pub fn stable_code(&self) -> &'static str {
        match self {
            Self::IdentityMismatch => "identity_mismatch",
            Self::Permanent(_) => "invalid_report",
            Self::Unauthorized(_) => "unauthorized",
            Self::UnsupportedProtocol(_) => "unsupported_client_protocol",
            Self::Transient(_) => "server_unavailable",
            Self::FutureTimestamp(_) => "report_future_timestamp",
        }
    }

    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized(_) => Some(401),
            Self::UnsupportedProtocol(_) => Some(400),
            Self::FutureTimestamp(_) => Some(400),
            _ => None,
        }
    }
}

fn validate_xsos_ack(
    status: StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    report: &ClientReport,
) -> Result<(), SendError> {
    if status != StatusCode::ACCEPTED {
        if status.is_success() {
            return Err(SendError::Transient(format!(
                "xsos returned unexpected HTTP {status}; report acknowledgements require HTTP 202 Accepted"
            )));
        }
        return classify_xsos_response(status, content_type, body);
    }
    if !content_type.is_some_and(is_application_json) {
        return Err(SendError::Transient(format!(
            "xsos returned HTTP {status} without Content-Type application/json"
        )));
    }
    let ack: ClientReportAck = serde_json::from_slice(body).map_err(|error| {
        SendError::Transient(format!(
            "xsos returned HTTP {status} with an invalid acknowledgement at line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    if ack.host_id != report.host.id || ack.report_id != report.report_id {
        return Err(SendError::Transient(
            "xsos acknowledgement identity mismatch".into(),
        ));
    }
    Ok(())
}

fn is_application_json(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

/// Classify a xsos response using both HTTP status and the strict
/// Foundation `ErrorEnvelope`. A proxy/WAF body, a non-contract `{message}` body or
/// an envelope with unknown/missing fields is deliberately never allowed to
/// trigger credential deletion or permanent spool removal.
pub fn classify_xsos_response(
    status: StatusCode,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(), SendError> {
    if status.is_success() {
        return Ok(());
    }
    let envelope = content_type
        .filter(|value| is_application_json(value))
        .and_then(|_| serde_json::from_slice::<ErrorEnvelope>(body).ok());
    // Even a valid envelope may reflect credentials in message/request_id.
    // Only fixed, locally recognized labels are safe for durable diagnostics.
    let detail = match envelope.as_ref().map(|error| error.code.as_str()) {
        Some("bad_request") => "bad_request",
        Some("conflict") => "conflict",
        Some("payload_too_large") => "payload_too_large",
        Some("unauthorized") => "unauthorized",
        Some("client_host_mismatch") => "client_host_mismatch",
        Some("unsupported_client_protocol") => "unsupported_client_protocol",
        Some("report_future_timestamp") => "report_future_timestamp",
        _ => "unrecognized error response",
    };
    let message = format!("xsos rejected telemetry with HTTP {status}: {detail}");
    // 404/408/421/429 与 5xx 留作可重试：服务端重启、反代修复、限流退避之后，
    // 同一份报文仍可能被接受。
    match status {
        StatusCode::BAD_REQUEST => match envelope.as_ref() {
            Some(error)
                if error.code.as_str() == "unsupported_client_protocol" && !error.retryable =>
            {
                Err(SendError::UnsupportedProtocol(format!(
                    "{message}; client/server protocol versions do not match; upgrade both to the same release. Re-pairing will not fix this error"
                )))
            }
            Some(error) if error.code.as_str() == "bad_request" && !error.retryable => {
                Err(SendError::Permanent(format!(
                    "{message}; report schema {} was rejected. Check that client and server use the same protocol version; this report will not be retried",
                    crate::model::CLIENT_REPORT_SCHEMA_VERSION
                )))
            }
            Some(error) if error.code.as_str() == "report_future_timestamp" && error.retryable => {
                Err(SendError::FutureTimestamp(message))
            }
            _ => Err(SendError::Transient(message)),
        },
        StatusCode::CONFLICT => match envelope.as_ref() {
            Some(error) if error.code.as_str() == "conflict" && !error.retryable => {
                Err(SendError::Permanent(message))
            }
            _ => Err(SendError::Transient(message)),
        },
        StatusCode::PAYLOAD_TOO_LARGE => match envelope.as_ref() {
            Some(error) if error.code.as_str() == "payload_too_large" && !error.retryable => {
                Err(SendError::Permanent(message))
            }
            _ => Err(SendError::Transient(message)),
        },
        // 421 = 请求没走对链路（反向代理未透传 X-Forwarded-*），**不是**凭据问题。
        // 必须早于下面这一支匹配，否则会误判为需要创建新实例并再次配对。
        StatusCode::MISDIRECTED_REQUEST => Err(SendError::Transient(format!(
            "{message}（这是部署配置问题，不是凭据失效：请检查反向代理是否透传 \
             X-Forwarded-Proto 与 X-Forwarded-For）"
        ))),
        StatusCode::UNAUTHORIZED => match envelope.as_ref() {
            Some(error) if error.code.as_str() == "unauthorized" && !error.retryable => {
                Err(SendError::Unauthorized(message))
            }
            // A reverse proxy, WAF, or temporary upstream auth layer may generate its own 401.
            // Only xsos's stable machine code proves that the host credential is invalid;
            // otherwise keep the report queued and retry after the deployment recovers.
            _ => Err(SendError::Transient(message)),
        },
        StatusCode::FORBIDDEN => match envelope.as_ref() {
            // A valid credential accompanied by another host identity can never make this exact
            // report valid. This is the expected fate of old queued reports after pairing to a
            // different server/instance, so discard only that report and continue the FIFO.
            Some(error) if error.code.as_str() == "client_host_mismatch" && !error.retryable => {
                Err(SendError::Permanent(message))
            }
            // A proxy or WAF may generate an unrelated 403. Retrying is safer than permanently
            // deauthorizing a valid credential or deleting telemetry.
            _ => Err(SendError::Transient(message)),
        },
        _ => Err(SendError::Transient(message)),
    }
}

#[cfg(feature = "otlp")]
fn ensure_generic_success(status: StatusCode, target: &str) -> Result<(), SendError> {
    if status.is_success() {
        return Ok(());
    }
    Err(SendError::Transient(format!(
        "{target} rejected telemetry with HTTP {status}"
    )))
}

/// Explicit unauthenticated network diagnostic using the configured protected TLS inputs.
pub async fn network_probe(config: &ClientConfig) -> anyhow::Result<serde_json::Value> {
    let mut url = Url::parse(&config.endpoint)?;
    url.set_path("/healthz");
    url.set_query(None);
    url.set_fragment(None);
    let client = build_client(config)?;
    let request = client.get(url).build()?;
    let response = execute_bounded(&client, request, MAX_ERROR_RESPONSE_BYTES).await?;
    anyhow::ensure!(
        response.status == StatusCode::NO_CONTENT,
        "public health endpoint unavailable"
    );
    Ok(
        serde_json::json!({"reachable":true,"scope":"public_health_endpoint","trust_context":"current_cli_account"}),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as ChronoDuration, Utc};
    use uuid::Uuid;

    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn network_probe_uses_the_current_public_liveness_contract() {
        use std::io::{Read, Write};

        for status in ["204 No Content", "200 OK", "503 Service Unavailable"] {
            let https = crate::test_https::TestHttpsServer::new();
            let config = ClientConfig {
                endpoint: format!("{}/api/v1/xsoc/report", https.origin),
                tls_ca_pem: Some(https.ca_path.clone()),
                otlp_token: Some(Arc::new(SecretString::new("private-otlp-marker".into()))),
                ..Default::default()
            };
            let server = std::thread::spawn(move || {
                let mut stream = https.accept();
                let mut headers = Vec::new();
                let mut buffer = [0; 1024];
                while !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0 && headers.len() + count <= MAX_HEADER_BYTES);
                    headers.extend_from_slice(&buffer[..count]);
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                stream.flush().unwrap();
                String::from_utf8(headers).unwrap()
            });
            let result = network_probe(&config).await;
            let headers = server.join().unwrap();
            assert!(headers.starts_with("GET /healthz HTTP/1.1\r\n"));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            assert!(!headers.contains("private-otlp-marker"));
            assert_eq!(result.is_ok(), status == "204 No Content");
        }
    }

    #[test]
    fn retry_after_is_bounded_and_only_applies_to_retryable_server_statuses() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("3"));
        assert_eq!(
            retry_after_delay(StatusCode::TOO_MANY_REQUESTS, &headers),
            Some(Duration::from_secs(3))
        );
        assert_eq!(retry_after_delay(StatusCode::BAD_REQUEST, &headers), None);
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_static("9999"),
        );
        assert_eq!(
            retry_after_delay(StatusCode::SERVICE_UNAVAILABLE, &headers),
            Some(Duration::from_secs(300))
        );
        headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("3s"));
        assert_eq!(
            retry_after_delay(StatusCode::TOO_MANY_REQUESTS, &headers),
            None
        );
    }

    #[test]
    fn queued_send_preserves_exact_bytes_after_clock_correction() {
        let mut queued = report();
        queued.collected_at = Utc::now() + ChronoDuration::minutes(10);
        let (_, canonical) = report_contract::canonical_spool_report(&queued).unwrap();
        let original = format!("\n{}\n", std::str::from_utf8(&canonical).unwrap()).into_bytes();

        for _ in 0..2 {
            let (sent, body) = prepare_report_body(&queued, Some(&original)).unwrap();
            assert_eq!(sent, queued);
            assert_eq!(body, original);
        }
        assert_ne!(prepare_report_body(&queued, None).unwrap().0, queued);
    }

    #[test]
    fn future_timestamp_response_retains_queued_report() {
        let body = br#"{"code":"report_future_timestamp","message":"collected_at is too far in the future","retryable":true}"#;
        assert!(matches!(
            classify_xsos_response(StatusCode::BAD_REQUEST, Some("application/json"), body,),
            Err(SendError::FutureTimestamp(_))
        ));
    }

    #[test]
    fn reflected_credentials_never_enter_response_error_messages() {
        let marker = "reflected-private-credential";
        let body = serde_json::to_vec(&serde_json::json!({
            "code": "unauthorized", "message": marker,
            "request_id": "req-test", "retryable": false,
        }))
        .unwrap();
        let recognized =
            classify_xsos_response(StatusCode::UNAUTHORIZED, Some("application/json"), &body)
                .unwrap_err();
        assert!(recognized.is_unauthorized());
        assert!(!format!("{recognized:?}/{recognized}").contains(marker));
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let error =
                classify_xsos_response(status, Some("text/plain"), marker.as_bytes()).unwrap_err();
            assert!(!format!("{error:?}/{error}").contains(marker));
            #[cfg(feature = "otlp")]
            {
                let error = ensure_generic_success(status, "OTLP").unwrap_err();
                assert!(!format!("{error:?}/{error}").contains(marker));
            }
        }
        let ack = serde_json::json!({ "private-credential-field": marker }).to_string();
        let error = validate_xsos_ack(
            StatusCode::ACCEPTED,
            Some("application/json"),
            ack.as_bytes(),
            &report(),
        )
        .unwrap_err();
        assert!(!format!("{error:?}/{error}").contains("private-credential"));
    }

    #[test]
    fn credential_status_requires_the_strict_contract_and_matching_identity() {
        let host_id = Uuid::new_v4().to_string();
        let body = serde_json::to_vec(&serde_json::json!({
            "status": "authorized",
            "host_id": host_id,
            "instance_id": host_id,
            "protocol_version": HOST_PAIRING_PROTOCOL_VERSION,
        }))
        .unwrap();
        assert_eq!(
            classify_credential_status_response(
                &host_id,
                StatusCode::OK,
                Some("application/json"),
                &body,
            ),
            Ok(RemoteBindingStatus::Authorized {
                host_id: host_id.clone()
            })
        );
        assert_eq!(
            classify_credential_status_response(
                &Uuid::new_v4().to_string(),
                StatusCode::OK,
                Some("application/json"),
                &body,
            ),
            Err(RemoteBindingFailure::ContractMismatch { status: 200 })
        );
        assert_eq!(
            classify_credential_status_response(
                &host_id,
                StatusCode::UNAUTHORIZED,
                Some("application/json"),
                br#"{"code":"unauthorized","message":"denied","retryable":false}"#,
            ),
            Ok(RemoteBindingStatus::Unauthorized)
        );
        assert_eq!(
            classify_credential_status_response(
                &host_id,
                StatusCode::UNAUTHORIZED,
                Some("text/html"),
                b"unauthorized",
            ),
            Err(RemoteBindingFailure::AuthResponseUntrusted)
        );
        for (status, expected) in [
            (StatusCode::NOT_FOUND, "pairing_server_upgrade_required"),
            (
                StatusCode::MISDIRECTED_REQUEST,
                "pairing_reverse_proxy_misconfigured",
            ),
            (StatusCode::TOO_MANY_REQUESTS, "pairing_rate_limited"),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "pairing_server_unavailable",
            ),
            (
                StatusCode::TEMPORARY_REDIRECT,
                "pairing_unexpected_redirect",
            ),
        ] {
            let failure = classify_credential_status_response(
                &host_id,
                status,
                Some("application/json"),
                br#"{}"#,
            )
            .unwrap_err();
            assert_eq!(failure.stable_code(), expected);
        }
    }

    use crate::model::{ClientHealth, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot};

    pub(super) fn report() -> ClientReport {
        ClientReport {
            schema_version: crate::model::CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                client_version: "test".into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                hardware: None,
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 0.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![0.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 1,
                    used_bytes: 0,
                    available_bytes: 1,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: Vec::new(),
                disks: Vec::new(),
                temperatures: Vec::new(),
                gpus: Vec::new(),
            },
            capabilities: Vec::new(),
            client: ClientHealth {
                spool_pending_batches: 0,
                collector_errors: 0,
            },
        }
    }

    #[test]
    fn accepts_only_the_current_exact_host_token_encoding() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("xsoc-token-{}", Uuid::new_v4()));
        crate::state_store::StateTransaction::begin(&directory)
            .unwrap()
            .write(StateFile::Credential, &"a".repeat(64))
            .unwrap();
        let token = read_secret(&StateReader::open(&directory).unwrap(), "host token").unwrap();
        assert_eq!(token.expose(), "a".repeat(64));
        assert_eq!(format!("{token:?}/{token}"), "[REDACTED]/[REDACTED]");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = directory.join("client-token");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_truncated_uppercase_or_whitespace_wrapped_host_tokens() {
        for token in [
            "a".repeat(63),
            "A".repeat(64),
            format!("{}\n", "a".repeat(64)),
        ] {
            assert!(validate_current_token(&token).is_err());
        }
    }

    #[test]
    fn reporter_rejects_credential_identity_from_another_product_or_contract() {
        let config = ClientConfig::default();
        let id = Uuid::new_v4().to_string();
        for (product, contract) in [
            (
                "another-product",
                crate::client_identity::HOST_REPORT_CONTRACT,
            ),
            ("xsos", "another-contract"),
        ] {
            let identity = ClientIdentity::new(
                product,
                &id,
                xcsc_runtime::ContractId::new(contract).unwrap(),
            )
            .unwrap();
            let result = Reporter::with_client_and_credential(
                &config,
                build_client(&config).unwrap(),
                CredentialSnapshot {
                    identity,
                    revision: (Uuid::new_v4(), Uuid::new_v4()),
                    secret: Arc::new(SecretString::new("private-identity-token".into())),
                },
            );
            let error = result
                .err()
                .expect("a matching instance ID alone cannot authorize this reporter");
            let detail = format!("{error:?}/{error}");
            assert!(!detail.contains("private-identity-token") && !detail.contains(&id));
        }
    }

    #[test]
    fn reporter_snapshots_share_redacted_secret_ownership() {
        let config = ClientConfig {
            otlp_token: Some(Arc::new(SecretString::new("private-otlp-marker".into()))),
            ..ClientConfig::default()
        };
        let reporter = Reporter::with_client_and_credential(
            &config,
            build_client(&config).unwrap(),
            CredentialSnapshot {
                identity: crate::client_identity::for_instance(&report().host.id).unwrap(),
                revision: (Uuid::new_v4(), Uuid::new_v4()),
                secret: Arc::new(SecretString::new("private-host-marker".into())),
            },
        )
        .unwrap();
        let snapshot = reporter.clone();
        assert_eq!(reporter.identity(), snapshot.identity());
        assert!(Arc::ptr_eq(
            config.otlp_token.as_ref().unwrap(),
            reporter.otlp_token.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&reporter.token, &snapshot.token));
        assert_eq!(
            reporter.credential_revision(),
            snapshot.credential_revision()
        );
        assert!(Arc::ptr_eq(
            reporter.otlp_token.as_ref().unwrap(),
            snapshot.otlp_token.as_ref().unwrap()
        ));
        assert!(!format!("{:?}/{:?}", snapshot.token, snapshot.otlp_token).contains("marker"));
        drop(reporter);
        assert_eq!(snapshot.token.expose(), "private-host-marker");
    }

    #[cfg(unix)]
    #[test]
    fn credential_reader_distinguishes_missing_from_unsafe_or_oversized_state() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-token-read-{}", Uuid::new_v4()));
        assert!(
            matches!(StateReader::open(&directory), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!directory.exists());
        crate::private_fs::ensure_private_directory(&directory).unwrap();
        let path = directory.join("client-token");
        symlink(directory.join("absent"), &path).unwrap();
        assert!(
            read_secret(&StateReader::open(&directory).unwrap(), "host token").is_err(),
            "dangling links are not missing credentials"
        );
        fs::remove_file(&path).unwrap();
        crate::state_store::StateTransaction::begin(&directory)
            .unwrap()
            .write(StateFile::Credential, "secret")
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_secret(&StateReader::open(&directory).unwrap(), "host token").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len((StateFile::Credential.max_bytes() + 1) as u64)
            .unwrap();
        assert!(
            format!(
                "{:#}",
                read_secret(&StateReader::open(&directory).unwrap(), "host token").unwrap_err()
            )
            .contains("budget")
        );
        drop(file);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn client_builder_rejects_unbound_identity_password() {
        let config = ClientConfig {
            tls_identity_password: Some(Arc::new(SecretString::new("secret".into()))),
            ..ClientConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an otherwise unused TLS identity password must not be ignored");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn client_builder_rejects_pkcs12_on_non_native_tls_backend() {
        let config = ClientConfig {
            tls_identity_pkcs12: Some("missing-client-identity.p12".into()),
            ..ClientConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an unsupported PKCS#12 identity must not be silently ignored");
        assert!(error.to_string().contains("tls_identity_pem"));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn client_builder_rejects_pem_on_native_tls_backend() {
        let config = ClientConfig {
            tls_identity_pem: Some("missing-client-identity.pem".into()),
            ..ClientConfig::default()
        };
        let error = build_client(&config)
            .expect_err("an unsupported PEM identity must not reach request construction");
        assert!(error.to_string().contains("tls_identity_pkcs12"));
    }

    #[test]
    fn report_id_conflicts_are_permanent() {
        let error = classify_xsos_response(
            StatusCode::CONFLICT,
            Some("application/json"),
            br#"{"code":"conflict","message":"report_id already belongs to another host","retryable":false}"#,
        )
        .expect_err("409 cannot become successful by retrying the same report");
        assert!(error.is_permanent());

        let non_contract = classify_xsos_response(
            StatusCode::CONFLICT,
            Some("application/json"),
            br#"{"message":"report_id already belongs to another host"}"#,
        )
        .expect_err("a non-contract 409 response must not become a permanent server decision");
        assert!(matches!(non_contract, SendError::Transient(_)));
    }

    #[test]
    fn strict_bad_request_and_payload_limit_codes_are_permanent() {
        for (status, code) in [
            (StatusCode::BAD_REQUEST, "bad_request"),
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "code": code,
                "message": "report cannot be accepted",
                "retryable": false
            }))
            .unwrap();
            let error = classify_xsos_response(status, Some("application/json"), &body)
                .expect_err("the current server contract rejected the report permanently");
            assert!(error.is_permanent());
        }
    }

    #[test]
    fn protocol_mismatch_is_terminal_without_invalidating_credentials() {
        let error = classify_xsos_response(
            StatusCode::BAD_REQUEST,
            Some("application/json"),
            br#"{"code":"unsupported_client_protocol","message":"unsupported","retryable":false,"details":{"received":2,"supported":[1]}}"#,
        )
        .expect_err("the explicit protocol rejection must not be treated as report content loss");
        assert!(matches!(error, SendError::UnsupportedProtocol(_)));
        assert!(!error.is_unauthorized());
        assert!(error.is_permanent());
        assert!(error.to_string().contains("upgrade both"));
        assert_eq!(error.stable_code(), "unsupported_client_protocol");
        assert_eq!(error.http_status(), Some(400));
    }

    #[test]
    fn non_contract_422_is_not_treated_as_a_current_permanent_rejection() {
        let error = classify_xsos_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("text/plain"),
            b"unexpected response",
        )
        .expect_err("422 is not part of the current Server report contract");
        assert!(matches!(error, SendError::Transient(_)));
    }

    #[test]
    fn a_successful_report_requires_a_matching_acknowledgement() {
        let report = report();
        let body = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "accepted": false,
            "received_at": Utc::now()
        }))
        .unwrap();
        validate_xsos_ack(
            StatusCode::ACCEPTED,
            Some("application/json; charset=utf-8"),
            &body,
            &report,
        )
        .unwrap();

        let error = validate_xsos_ack(StatusCode::OK, Some("application/json"), &body, &report)
            .expect_err("a structurally valid HTTP 200 must not acknowledge a report");
        assert!(matches!(error, SendError::Transient(_)));

        assert!(matches!(
            validate_xsos_ack(StatusCode::ACCEPTED, Some("text/plain"), &body, &report),
            Err(SendError::Transient(_))
        ));
        assert!(matches!(
            validate_xsos_ack(StatusCode::ACCEPTED, None, &body, &report),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn an_acknowledgement_for_another_report_is_not_accepted() {
        let report = report();
        let body = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": Uuid::new_v4(),
            "accepted": true,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_xsos_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &body,
                &report
            ),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn acknowledgement_rejects_missing_or_unknown_current_contract_fields() {
        let report = report();
        let without_accepted = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_xsos_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &without_accepted,
                &report
            ),
            Err(SendError::Transient(_))
        ));

        let with_unknown_field = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id,
            "report_id": report.report_id,
            "accepted": true,
            "received_at": Utc::now(),
            "unknown_status_detail": "ok"
        }))
        .unwrap();
        assert!(matches!(
            validate_xsos_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &with_unknown_field,
                &report
            ),
            Err(SendError::Transient(_))
        ));

        let noncanonical_uuid = serde_json::to_vec(&serde_json::json!({
            "host_id": report.host.id.to_uppercase(),
            "report_id": report.report_id,
            "accepted": true,
            "received_at": Utc::now()
        }))
        .unwrap();
        assert!(matches!(
            validate_xsos_ack(
                StatusCode::ACCEPTED,
                Some("application/json"),
                &noncanonical_uuid,
                &report
            ),
            Err(SendError::Transient(_))
        ));
    }

    #[test]
    fn stable_unauthorized_code_requires_new_pairing() {
        let error = classify_xsos_response(
            StatusCode::UNAUTHORIZED,
            Some("application/json; charset=utf-8"),
            br#"{"code":"unauthorized","message":"unauthorized","retryable":false}"#,
        )
        .expect_err("xsos's stable unauthorized code must require a newly authorized pairing");
        assert!(error.is_unauthorized());
    }

    #[test]
    fn unrecognized_unauthorized_response_keeps_the_credential_retryable() {
        let responses: &[&[u8]] = &[
            b"<html><body>temporary proxy authentication</body></html>",
            br#"{"code":"upstream_auth_required","message":"try again","retryable":false}"#,
            br#"{"message":"missing machine code"}"#,
            br#"{"code":"Unauthorized","message":"machine codes are case-sensitive","retryable":false}"#,
            br#"{"code":"unauthorized","message":"unknown field","retryable":false,"unknown_extension":true}"#,
            br#"{"code":"unauthorized","message":"server says retry","retryable":true}"#,
            b"{\"code\":\"unauthorized\",\"message\":\"invalid UTF-8: \xff\",\"retryable\":false}",
        ];
        for body in responses {
            let error =
                classify_xsos_response(StatusCode::UNAUTHORIZED, Some("application/json"), body)
                    .expect_err("an unknown 401 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_unauthorized());
        }

        let wrong_content_type = classify_xsos_response(
            StatusCode::UNAUTHORIZED,
            Some("text/plain"),
            br#"{"code":"unauthorized","message":"unauthorized","retryable":false}"#,
        )
        .expect_err("a non-JSON content type must not authorize credential state changes");
        assert!(matches!(wrong_content_type, SendError::Transient(_)));
    }

    #[test]
    fn forbidden_host_identity_mismatch_is_permanent_for_that_report() {
        let error = classify_xsos_response(
            StatusCode::FORBIDDEN,
            Some("application/json"),
            br#"{"code":"client_host_mismatch","message":"token does not belong to host","retryable":false}"#,
        )
        .expect_err("a queued report for another host can never match the current credential");
        assert!(error.is_permanent());
    }

    #[test]
    fn unrecognized_forbidden_response_keeps_the_credential_retryable() {
        for body in [
            b"temporary policy rejection".as_slice(),
            br#"{"code":"forbidden","message":"unrelated access policy","retryable":false}"#,
        ] {
            let error =
                classify_xsos_response(StatusCode::FORBIDDEN, Some("application/json"), body)
                    .expect_err("an unknown 403 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_permanent());
        }
    }
}
