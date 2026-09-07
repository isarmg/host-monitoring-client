#[cfg(test)]
use std::fs;
use std::sync::Arc;

#[cfg(feature = "otlp")]
use std::io::Write;

use anyhow::{Context, bail};
#[cfg(feature = "otlp")]
use flate2::{Compression, write::GzEncoder};
use sarmg_client_error::ErrorEnvelope;
use sarmg_client_runtime::{ClientIdentity, CredentialSnapshot, CredentialStore};
use sarmg_client_secret::{SecretBytes, SecretString};
use sarmg_client_secure_http::{Certificate, Identity, StatusCode, header};
use sarmg_client_secure_http::{ResponseBudget, SecureHttpClient, TlsConfig};
use uuid::Uuid;

use host_protocol::ClientReportAck;

use crate::{
    config::ClientConfig,
    model::ClientReport,
    report_contract,
    state_store::{StateFile, StateReader, StateTransaction},
};

const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1024;

#[cfg(all(test, target_os = "linux"))]
#[path = "transport/tls_tests.rs"]
mod tls_tests;

#[derive(Clone)]
pub struct Reporter {
    identity: ClientIdentity,
    client: SecureHttpClient,
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

    fn validate_report_identity(&self, report: &ClientReport) -> anyhow::Result<()> {
        self.identity
            .ensure_matches(&crate::client_identity::for_instance(&report.host.id)?)?;
        Ok(())
    }

    fn with_client_and_credential(
        config: &ClientConfig,
        client: SecureHttpClient,
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

    pub async fn send_host_monitoring(&self, report: &ClientReport) -> Result<(), SendError> {
        let (bounded, body) = report_contract::encode_report_body(report)
            .map_err(|error| SendError::Permanent(format!("invalid Client report: {error}")))?;
        // A different valid identity is not an ACK or permanent content rejection.
        // Keep the record and current credential; never send it with another identity's token.
        self.validate_report_identity(&bounded)
            .map_err(|_| SendError::IdentityMismatch)?;
        let headers = authenticated_headers(&self.token, "application/json")
            .map_err(|_| SendError::Transient("invalid host authorization header".into()))?;
        let response = self
            .client
            .post_client(&self.endpoint, headers, body)
            .await
            .map_err(|error| {
                SendError::Transient(format!("Host Monitoring request failed: {error}"))
            })?;
        let content_type = response
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        validate_host_monitoring_ack(response.status, content_type, &response.body, &bounded)
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
        let response = self.client.post_client(endpoint, headers, body).await?;
        Ok(ensure_generic_success(response.status, "OTLP")?)
    }

    #[cfg(not(feature = "otlp"))]
    pub async fn send_otlp(&self, _report: &ClientReport) -> anyhow::Result<()> {
        Ok(())
    }
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
    Ok(!text.trim().is_empty())
}

pub(crate) fn build_client(config: &ClientConfig) -> anyhow::Result<SecureHttpClient> {
    let mut tls = TlsConfig::default();
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
                .with_context(|| format!("failed to read TLS identity {}", path.display()))?;
            tls.identity = Some(
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
                .with_context(|| format!("failed to read TLS identity {}", path.display()))?;
            tls.identity = Some(
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
            .with_context(|| format!("failed to read TLS CA {}", path.display()))?;
        let certificates = Certificate::from_pem_bundle(bytes.expose())
            .map_err(|_| anyhow::anyhow!("invalid TLS CA certificate"))?;
        anyhow::ensure!(
            !certificates.is_empty(),
            "TLS CA input contains no certificates"
        );
        for certificate in certificates {
            tls.roots.push(certificate);
        }
    }
    Ok(SecureHttpClient::new(
        config.request_timeout(),
        ResponseBudget {
            max_header_bytes: 64 * 1024,
            max_body_bytes: MAX_ERROR_RESPONSE_BYTES,
        },
        tls,
        format!("host-monitor/{}", env!("CARGO_PKG_VERSION")),
    )?)
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
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("{kind} {} is empty", path.display());
    }
    Ok(SecretString::new(token))
}

/// 上报失败的性质。判据是**要让同一份报文最终被接受，需要改变什么**：
///
/// | 变体 | 需要改变的东西 | 处置 |
/// |---|---|---|
/// | `Permanent`  | 报文内容本身（改不了） | 丢弃 |
/// | `Unauthorized` | 服务端稳定 `unauthorized` 机器码确认凭据失效 | 需要创建新实例并再次配对 |
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
    /// Host Monitoring 以 401 和稳定 `unauthorized` 机器码确认凭据不被接受。主机进入
    /// `reauth_required`，只能通过创建新实例并执行当前 v2 配对流程恢复；Client 不会自动生成
    /// 或替换凭据。代理/WAF 生成的未知 401 不得使用此变体。
    #[error("{0}")]
    Unauthorized(String),
    /// 网络故障或服务端暂时不可用，保留记录并退避重试。
    #[error("{0}")]
    Transient(String),
}

impl SendError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// 凭据已失效，需要创建新实例并再次配对后才可能成功。
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized(_))
    }
}

fn validate_host_monitoring_ack(
    status: StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    report: &ClientReport,
) -> Result<(), SendError> {
    if status != StatusCode::ACCEPTED {
        if status.is_success() {
            return Err(SendError::Transient(format!(
                "Host Monitoring returned unexpected HTTP {status}; report acknowledgements require HTTP 202 Accepted"
            )));
        }
        return classify_host_monitoring_response(status, content_type, body);
    }
    if !content_type.is_some_and(is_application_json) {
        return Err(SendError::Transient(format!(
            "Host Monitoring returned HTTP {status} without Content-Type application/json"
        )));
    }
    let ack: ClientReportAck = serde_json::from_slice(body).map_err(|error| {
        SendError::Transient(format!(
            "Host Monitoring returned HTTP {status} with an invalid acknowledgement at line {}, column {}",
            error.line(), error.column()
        ))
    })?;
    if ack.host_id != report.host.id || ack.report_id != report.report_id {
        return Err(SendError::Transient(
            "Host Monitoring acknowledgement identity mismatch".into(),
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

/// Classify a Host Monitoring response using both HTTP status and the strict
/// Foundation `ErrorEnvelope`. A proxy/WAF body, a non-contract `{message}` body or
/// an envelope with unknown/missing fields is deliberately never allowed to
/// trigger credential deletion or permanent spool removal.
pub fn classify_host_monitoring_response(
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
        _ => "unrecognized error response",
    };
    let message = format!("Host Monitoring rejected telemetry with HTTP {status}: {detail}");
    // 404/408/421/429 与 5xx 留作可重试：服务端重启、反代修复、限流退避之后，
    // 同一份报文仍可能被接受。
    match status {
        StatusCode::BAD_REQUEST => match envelope.as_ref() {
            Some(error) if error.code.as_str() == "bad_request" && !error.retryable => {
                Err(SendError::Permanent(message))
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
            // Only Host Monitoring's stable machine code proves that the host credential is invalid;
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn reflected_credentials_never_enter_response_error_messages() {
        let marker = "reflected-private-credential";
        let body = serde_json::to_vec(&serde_json::json!({
            "code": "unauthorized", "message": marker,
            "request_id": "req-test", "retryable": false,
        }))
        .unwrap();
        let recognized = classify_host_monitoring_response(
            StatusCode::UNAUTHORIZED,
            Some("application/json"),
            &body,
        )
        .unwrap_err();
        assert!(recognized.is_unauthorized());
        assert!(!format!("{recognized:?}/{recognized}").contains(marker));
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let error =
                classify_host_monitoring_response(status, Some("text/plain"), marker.as_bytes())
                    .unwrap_err();
            assert!(!format!("{error:?}/{error}").contains(marker));
            #[cfg(feature = "otlp")]
            {
                let error = ensure_generic_success(status, "OTLP").unwrap_err();
                assert!(!format!("{error:?}/{error}").contains(marker));
            }
        }
        let ack = serde_json::json!({ "private-credential-field": marker }).to_string();
        let error = validate_host_monitoring_ack(
            StatusCode::ACCEPTED,
            Some("application/json"),
            ack.as_bytes(),
            &report(),
        )
        .unwrap_err();
        assert!(!format!("{error:?}/{error}").contains("private-credential"));
    }
    use crate::model::{ClientHealth, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot};

    pub(super) fn report() -> ClientReport {
        ClientReport {
            schema_version: 1,
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
    fn persists_trimmed_host_token() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-monitor-token-{}", Uuid::new_v4()));
        crate::state_store::StateTransaction::begin(&directory)
            .unwrap()
            .write(StateFile::Credential, " secret-token\n")
            .unwrap();
        let token = read_secret(&StateReader::open(&directory).unwrap(), "host token").unwrap();
        assert_eq!(token.expose(), "secret-token");
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
    fn reporter_rejects_credential_identity_from_another_product_or_contract() {
        let config = ClientConfig::default();
        let id = Uuid::new_v4().to_string();
        for (product, contract) in [
            (
                "another-product",
                crate::client_identity::HOST_REPORT_CONTRACT,
            ),
            ("host-monitoring", "another-contract"),
        ] {
            let identity = ClientIdentity::new(
                product,
                &id,
                sarmg_client_runtime::ContractId::new(contract).unwrap(),
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
        let error = classify_host_monitoring_response(
            StatusCode::CONFLICT,
            Some("application/json"),
            br#"{"code":"conflict","message":"report_id already belongs to another host","retryable":false}"#,
        )
        .expect_err("409 cannot become successful by retrying the same report");
        assert!(error.is_permanent());

        let non_contract = classify_host_monitoring_response(
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
            let error = classify_host_monitoring_response(status, Some("application/json"), &body)
                .expect_err("the current server contract rejected the report permanently");
            assert!(error.is_permanent());
        }
    }

    #[test]
    fn non_contract_422_is_not_treated_as_a_current_permanent_rejection() {
        let error = classify_host_monitoring_response(
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
        validate_host_monitoring_ack(
            StatusCode::ACCEPTED,
            Some("application/json; charset=utf-8"),
            &body,
            &report,
        )
        .unwrap();

        let error =
            validate_host_monitoring_ack(StatusCode::OK, Some("application/json"), &body, &report)
                .expect_err("a structurally valid HTTP 200 must not acknowledge a report");
        assert!(matches!(error, SendError::Transient(_)));

        assert!(matches!(
            validate_host_monitoring_ack(StatusCode::ACCEPTED, Some("text/plain"), &body, &report),
            Err(SendError::Transient(_))
        ));
        assert!(matches!(
            validate_host_monitoring_ack(StatusCode::ACCEPTED, None, &body, &report),
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
            validate_host_monitoring_ack(
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
            validate_host_monitoring_ack(
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
            validate_host_monitoring_ack(
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
            validate_host_monitoring_ack(
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
        let error = classify_host_monitoring_response(
            StatusCode::UNAUTHORIZED,
            Some("application/json; charset=utf-8"),
            br#"{"code":"unauthorized","message":"unauthorized","retryable":false}"#,
        )
        .expect_err(
            "Host Monitoring's stable unauthorized code must require a newly authorized pairing",
        );
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
            let error = classify_host_monitoring_response(
                StatusCode::UNAUTHORIZED,
                Some("application/json"),
                body,
            )
            .expect_err("an unknown 401 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_unauthorized());
        }

        let wrong_content_type = classify_host_monitoring_response(
            StatusCode::UNAUTHORIZED,
            Some("text/plain"),
            br#"{"code":"unauthorized","message":"unauthorized","retryable":false}"#,
        )
        .expect_err("a non-JSON content type must not authorize credential state changes");
        assert!(matches!(wrong_content_type, SendError::Transient(_)));
    }

    #[test]
    fn forbidden_host_identity_mismatch_is_permanent_for_that_report() {
        let error = classify_host_monitoring_response(
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
            let error = classify_host_monitoring_response(
                StatusCode::FORBIDDEN,
                Some("application/json"),
                body,
            )
            .expect_err("an unknown 403 must not be accepted");
            assert!(matches!(error, SendError::Transient(_)));
            assert!(!error.is_permanent());
        }
    }
}
