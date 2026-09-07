//! Host-driven integration boundary for Android, iOS, and iPadOS applications.
//!
//! Mobile operating systems do not give an embedded library daemon semantics,
//! unrestricted whole-device telemetry, or permission to persist credentials in
//! arbitrary files. The native host application owns scheduling, lifecycle,
//! sandbox-aware collection, HTTPS transport, and Keychain/Keystore access. This
//! module only constructs and bounds the shared Union report payload.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{model::*, report_contract};

/// MIME type for [`MobileReportPayload::body`].
pub const REPORT_CONTENT_TYPE: &str = "application/json";

/// Mobile product identity carried in the shared telemetry contract.
///
/// Rust exposes both iPhone and iPad builds as `target_os = "ios"`; the host
/// supplies the product identity so Union can still distinguish iOS and iPadOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobilePlatform {
    Android,
    Ios,
    IpadOs,
}

impl MobilePlatform {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::Ios => "ios",
            Self::IpadOs => "ipados",
        }
    }
}

/// Stable identity supplied by the native application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileHostDescriptor {
    pub host_id: String,
    pub platform: MobilePlatform,
    pub os_version: Option<String>,
    pub arch: String,
}

/// One host-scheduled, sandbox-visible telemetry sample.
#[derive(Debug, Clone, PartialEq)]
pub struct MobileSample {
    pub collected_at: DateTime<Utc>,
    pub interval_seconds: f64,
    pub system: SystemSnapshot,
    pub capabilities: Vec<Capability>,
    pub spool_pending_batches: u64,
}

/// A bounded wire payload ready for the native host's HTTPS client.
///
/// Authentication is deliberately absent. The host must load a credential from
/// Android Keystore or Apple Keychain and attach it without persisting it in this
/// library. Redirect policy and TLS trust remain responsibilities of that native
/// transport adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct MobileReportPayload {
    report: ClientReport,
    body: Vec<u8>,
}

impl MobileReportPayload {
    pub const fn content_type(&self) -> &'static str {
        REPORT_CONTENT_TYPE
    }

    pub fn report(&self) -> &ClientReport {
        &self.report
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MobileAdapterError {
    #[error("mobile host_id must be a canonical lowercase hyphenated UUID")]
    InvalidHostId,
    #[error("mobile architecture must not be empty")]
    EmptyArchitecture,
    #[error("{0}")]
    InvalidReport(String),
}

/// Pure report builder embedded by a native mobile host.
///
/// It creates no runtime, thread, timer, socket, background task, or durable
/// file. Calls occur only when the host application has execution time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileHostAdapter {
    host: HostIdentity,
}

impl MobileHostAdapter {
    pub fn new(descriptor: MobileHostDescriptor) -> Result<Self, MobileAdapterError> {
        let parsed =
            Uuid::parse_str(&descriptor.host_id).map_err(|_| MobileAdapterError::InvalidHostId)?;
        if parsed.to_string() != descriptor.host_id {
            return Err(MobileAdapterError::InvalidHostId);
        }
        if descriptor.arch.trim().is_empty() {
            return Err(MobileAdapterError::EmptyArchitecture);
        }

        Ok(Self {
            host: HostIdentity {
                id: descriptor.host_id,
                os: descriptor.platform.wire_name().to_owned(),
                os_version: descriptor.os_version,
                // Mobile sandboxes do not guarantee a meaningful kernel version.
                kernel_version: None,
                arch: descriptor.arch,
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        })
    }

    pub fn host(&self) -> &HostIdentity {
        &self.host
    }

    pub fn prepare_report(
        &self,
        sample: MobileSample,
    ) -> Result<MobileReportPayload, MobileAdapterError> {
        let mut capabilities = sample.capabilities;
        insert_boundary_capability(
            &mut capabilities,
            Capability::unavailable(
                "collection.scope.whole_system",
                "mobile-host",
                CapabilityErrorKind::Unsupported,
                "mobile telemetry is limited to information visible to the application sandbox",
            ),
        );
        insert_boundary_capability(
            &mut capabilities,
            Capability::unavailable(
                "client.background.daemon",
                "mobile-host",
                CapabilityErrorKind::Unsupported,
                "execution and background delivery are controlled by the host application and operating system",
            ),
        );

        let collector_errors = capabilities
            .iter()
            .filter(|capability| {
                !capability.available
                    && matches!(
                        capability.error_kind,
                        Some(CapabilityErrorKind::Transient | CapabilityErrorKind::InvalidData)
                    )
            })
            .count() as u64;
        let report = ClientReport {
            schema_version: CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: sample.collected_at,
            host: self.host.clone(),
            interval_seconds: sample.interval_seconds,
            system: sample.system,
            capabilities,
            client: ClientHealth {
                spool_pending_batches: sample.spool_pending_batches,
                collector_errors,
            },
        };
        let (report, body) = report_contract::encode_report_body(&report)
            .map_err(|error| MobileAdapterError::InvalidReport(error.to_string()))?;
        Ok(MobileReportPayload { report, body })
    }
}

fn insert_boundary_capability(capabilities: &mut Vec<Capability>, boundary: Capability) {
    capabilities.retain(|capability| capability.name != boundary.name);
    capabilities.push(boundary);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(platform: MobilePlatform) -> MobileHostDescriptor {
        MobileHostDescriptor {
            host_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            platform,
            os_version: Some("test".into()),
            arch: "aarch64".into(),
        }
    }

    fn sample() -> MobileSample {
        MobileSample {
            collected_at: Utc::now(),
            interval_seconds: 10.0,
            system: SystemSnapshot {
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 12.5,
                    logical_count: 1,
                    physical_count: None,
                    per_core_percent: vec![12.5],
                },
                memory: MemorySnapshot {
                    total_bytes: 100,
                    used_bytes: 40,
                    available_bytes: 60,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: Vec::new(),
                disks: Vec::new(),
                temperatures: Vec::new(),
                gpus: Vec::new(),
            },
            capabilities: vec![Capability::available("system.cpu", "native-host")],
            spool_pending_batches: 0,
        }
    }

    #[test]
    fn rejects_noncanonical_identity_and_empty_architecture() {
        let mut invalid_id = descriptor(MobilePlatform::Android);
        invalid_id.host_id = invalid_id.host_id.to_uppercase();
        assert_eq!(
            MobileHostAdapter::new(invalid_id),
            Err(MobileAdapterError::InvalidHostId)
        );

        let mut empty_arch = descriptor(MobilePlatform::Android);
        empty_arch.arch = "  ".into();
        assert_eq!(
            MobileHostAdapter::new(empty_arch),
            Err(MobileAdapterError::EmptyArchitecture)
        );
    }

    #[test]
    fn ios_and_ipados_share_targets_but_keep_product_identity() {
        let ios = MobileHostAdapter::new(descriptor(MobilePlatform::Ios)).unwrap();
        let ipados = MobileHostAdapter::new(descriptor(MobilePlatform::IpadOs)).unwrap();
        assert_eq!(ios.host().os, "ios");
        assert_eq!(ipados.host().os, "ipados");
        assert_eq!(ios.host().arch, "aarch64");
        assert_eq!(ipados.host().arch, "aarch64");
    }

    #[test]
    fn payload_is_bounded_and_records_mobile_execution_boundaries() {
        let adapter = MobileHostAdapter::new(descriptor(MobilePlatform::Android)).unwrap();
        let payload = adapter.prepare_report(sample()).unwrap();

        assert_eq!(payload.content_type(), "application/json");
        assert!(payload.body().len() <= CLIENT_REPORT_MAX_BODY_BYTES);
        assert!(payload.report().capabilities.iter().any(|capability| {
            capability.name == "collection.scope.whole_system" && !capability.available
        }));
        assert!(payload.report().capabilities.iter().any(|capability| {
            capability.name == "client.background.daemon" && !capability.available
        }));
        let decoded: ClientReport = serde_json::from_slice(payload.body()).unwrap();
        assert_eq!(decoded, *payload.report());
    }

    #[test]
    fn host_cannot_override_required_boundary_capability_with_available() {
        let adapter = MobileHostAdapter::new(descriptor(MobilePlatform::Android)).unwrap();
        let mut input = sample();
        input.capabilities.push(Capability::available(
            "collection.scope.whole_system",
            "untrusted-host-claim",
        ));

        let payload = adapter.prepare_report(input).unwrap();
        let scope = payload
            .report()
            .capabilities
            .iter()
            .filter(|capability| capability.name == "collection.scope.whole_system")
            .collect::<Vec<_>>();
        assert_eq!(scope.len(), 1);
        assert!(!scope[0].available);
        assert_eq!(scope[0].source, "mobile-host");
    }
}
