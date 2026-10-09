use std::{io, path::Path, sync::Arc};

use anyhow::Context;
use xcsc_fs_safety::EntryName;
use xcsc_runtime::{
    BoundedBytes, ClientSession, ContractId, MAX_SPOOL_ENTRIES, RecordId, SpoolLimits,
};

use crate::{
    model::{CLIENT_REPORT_MAX_BODY_BYTES, ClientReport},
    report_contract,
};

use crate::client_identity::HOST_REPORT_CONTRACT;

#[derive(Clone)]
pub struct Spool {
    inner: Arc<xcsc_runtime::Spool>,
    _session: Arc<ClientSession>,
}

pub struct PendingReport {
    record_id: RecordId,
    pub report: ClientReport,
    /// Exact bytes durably queued before the first HTTP attempt.
    pub body: Vec<u8>,
}

impl Spool {
    pub fn open(state_dir: &Path, max_bytes: u64) -> io::Result<Self> {
        limits(max_bytes).validate().map_err(io::Error::other)?;
        let session = Arc::new(
            ClientSession::from_directory(
                crate::maintenance::runtime_directory(state_dir).map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?,
        );
        Self::from_session(session, max_bytes)
    }

    pub fn from_session(session: Arc<ClientSession>, max_bytes: u64) -> io::Result<Self> {
        let limits = limits(max_bytes);
        limits.validate().map_err(io::Error::other)?;
        let directory = session
            .directory()
            .create_child(&EntryName::new("spool").map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        let inner =
            xcsc_runtime::Spool::from_directory(directory, limits).map_err(io::Error::other)?;
        Ok(Self {
            inner: Arc::new(inner),
            _session: session,
        })
    }

    pub fn pending_count(&self) -> io::Result<u64> {
        self.inner
            .usage()
            .map(|(entries, _)| entries as u64)
            .map_err(io::Error::other)
    }

    pub fn enqueue(&self, report: &ClientReport) -> anyhow::Result<()> {
        let (bounded, bytes) = report_contract::encode_report_body(report)?;
        self.inner.enqueue(
            ContractId::new(HOST_REPORT_CONTRACT)?,
            bounded.collected_at.timestamp_micros(),
            BoundedBytes::new(bytes, CLIENT_REPORT_MAX_BODY_BYTES)?,
        )?;
        Ok(())
    }

    pub fn oldest(&self) -> anyhow::Result<Option<PendingReport>> {
        // Quarantine reports with an incompatible contract or schema; leave valid
        // records available for delivery.
        // Each invalid iteration quarantines one record, and the
        // spool has a fixed entry limit. Do not report an empty queue after an
        // arbitrary number of quarantines while valid records remain behind
        // them (notably for the explicit queue-drain command).
        loop {
            let Some(record) = self.inner.next()? else {
                return Ok(None);
            };
            if record.contract_id.as_str() != HOST_REPORT_CONTRACT {
                self.inner
                    .quarantine(&record.record_id, xcsc_runtime::QuarantineReason::Corrupt)?;
                tracing::warn!(
                    "isolated queued report with a different contract identifier; original bytes preserved, current collection continues"
                );
                continue;
            }
            if serde_json::from_slice::<serde_json::Value>(record.payload.as_slice())
                .ok()
                .and_then(|v| v.get("schema_version").and_then(serde_json::Value::as_u64))
                .is_some_and(|version| {
                    version != u64::from(crate::model::CLIENT_REPORT_SCHEMA_VERSION)
                })
            {
                self.inner
                    .quarantine(&record.record_id, xcsc_runtime::QuarantineReason::Corrupt)?;
                tracing::warn!(
                    "isolated queued report with incompatible schema; original bytes preserved, current collection continues"
                );
                continue;
            }
            let parsed = serde_json::from_slice::<ClientReport>(record.payload.as_slice())
                .context("Foundation spool payload is not a Host Client report")
                .and_then(|report| {
                    let (canonical, _) = report_contract::canonical_spool_report(&report)?;
                    anyhow::ensure!(
                        canonical == report,
                        "spool payload is not the current canonical Host report"
                    );
                    Ok(report)
                });
            match parsed {
                Ok(report) => {
                    return Ok(Some(PendingReport {
                        record_id: record.record_id,
                        report,
                        body: record.payload.as_slice().to_vec(),
                    }));
                }
                Err(_) => {
                    self.inner
                        .quarantine(&record.record_id, xcsc_runtime::QuarantineReason::Corrupt)?;
                    tracing::warn!(
                        "isolated queued report that is malformed or not canonical; original bytes preserved, current collection continues"
                    );
                }
            }
        }
    }

    pub fn health(&self) -> io::Result<xcsc_runtime::ClientHealth> {
        self.inner.doctor().map_err(io::Error::other)
    }
}

impl xcsc_runtime::DeliveryQueue for Spool {
    type Item = PendingReport;
    type Error = anyhow::Error;

    fn next(&self) -> Result<Option<PendingReport>, Self::Error> {
        self.oldest()
    }

    fn acknowledge(&self, pending: &PendingReport) -> Result<(), Self::Error> {
        self.inner.ack(&pending.record_id)?;
        Ok(())
    }
    fn quarantine(
        &self,
        pending: &PendingReport,
        reason: xcsc_runtime::QuarantineReason,
    ) -> Result<(), Self::Error> {
        self.inner.quarantine(&pending.record_id, reason)?;
        Ok(())
    }
}
fn limits(max_bytes: u64) -> SpoolLimits {
    SpoolLimits {
        max_record_bytes: CLIENT_REPORT_MAX_BODY_BYTES,
        max_entries: MAX_SPOOL_ENTRIES,
        max_bytes,
    }
}

/// Read-only inventory using the same Windows service role as the writer.
/// The generic Foundation inspector otherwise assumes the interactive user's
/// ACL and rejects a correctly protected installed service spool.
pub fn inspect_existing(
    state_dir: &Path,
    max_bytes: u64,
) -> Result<xcsc_runtime::ClientHealth, xcsc_runtime::Error> {
    let limits = limits(max_bytes);
    #[cfg(not(windows))]
    {
        xcsc_runtime::Spool::inspect_existing(state_dir.join("spool"), limits)
    }
    #[cfg(windows)]
    {
        use xcsc_runtime::Error;
        limits.validate()?;
        let directory = crate::maintenance::open_runtime_directory(&state_dir.join("spool"))
            .map_err(|error| match error.downcast::<xcsc_fs_safety::Error>() {
                Ok(error) => Error::Filesystem(error),
                Err(_) => Error::SpoolUnavailable,
            })?;
        let entries = directory.files(xcsc_fs_safety::InventoryLimits {
            max_entries: limits.max_entries + 1,
            max_total_bytes: limits.max_bytes,
        })?;
        let mut health = xcsc_runtime::ClientHealth {
            healthy: true,
            spool_entries: 0,
            spool_bytes: 0,
            quarantined_entries: 0,
            identity_mismatch_entries: 0,
            capacity_remaining: true,
        };
        for entry in entries {
            if entry.name.as_os_str() == "spool.instance.lock" {
                if entry.bytes != 0 {
                    return Err(Error::InvalidRecord);
                }
                continue;
            }
            let text = entry
                .name
                .as_os_str()
                .to_str()
                .ok_or(Error::InvalidRecord)?;
            let suffix = validate_inventory_name(text)?;
            health.spool_bytes = health
                .spool_bytes
                .checked_add(entry.bytes)
                .ok_or(Error::SpoolFull)?;
            if suffix == "record" {
                health.spool_entries += 1;
            } else {
                health.quarantined_entries += 1;
                health.identity_mismatch_entries += usize::from(suffix == "identity");
            }
        }
        let count = health.spool_entries + health.quarantined_entries;
        if count > limits.max_entries {
            return Err(Error::SpoolFull);
        }
        health.healthy = health.quarantined_entries == 0;
        health.capacity_remaining =
            count < limits.max_entries && health.spool_bytes < limits.max_bytes;
        Ok(health)
    }
}

#[cfg(any(windows, test))]
fn validate_inventory_name(text: &str) -> Result<&str, xcsc_runtime::Error> {
    // Mirror the canonical namespace of the pinned Foundation Client 0.10.5.
    // Revisit this adapter when Foundation exposes policy-aware inspection.
    use xcsc_runtime::Error;
    let (stem, suffix) = text.rsplit_once('.').ok_or(Error::InvalidRecord)?;
    if !matches!(suffix, "record" | "bad" | "identity") {
        return Err(Error::InvalidRecord);
    }
    let mut parts = stem.split('-');
    let priority = parts
        .next()
        .and_then(|p| p.parse::<u8>().ok())
        .ok_or(Error::InvalidRecord)?;
    let created = parts
        .next()
        .and_then(|p| p.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .ok_or(Error::InvalidRecord)?;
    let id = RecordId::parse(parts.next().ok_or(Error::InvalidRecord)?.to_owned())?;
    if parts.next().is_some()
        || format!("{priority:03}-{created:020}-{}.{suffix}", id.as_str()) != text
    {
        return Err(Error::InvalidRecord);
    }
    Ok(suffix)
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn inventory_namespace_is_canonical_and_fail_closed() {
        let id = RecordId::new().unwrap();
        for suffix in ["record", "bad", "identity"] {
            let name = format!("100-{:020}-{}.{suffix}", 1, id.as_str());
            assert_eq!(validate_inventory_name(&name).unwrap(), suffix);
        }
        for name in [
            format!("100-1-{}.record", id.as_str()),
            format!("256-{:020}-{}.record", 1, id.as_str()),
            format!("100-{:020}-{}.unknown", 1, id.as_str()),
            format!("100-{:020}-{}-extra.record", 1, id.as_str()),
            "unrecognized-file".into(),
        ] {
            assert!(validate_inventory_name(&name).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_inspection_matches_writer_health_without_mutating_files() {
        use std::{collections::BTreeMap, fs};
        use xcsc_fs_safety::{AtomicFile, PrivateDirectory};
        use xcsc_runtime::QuarantineReason;

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap().join("state");
        let state = PrivateDirectory::create(&path).unwrap();
        let directory = state
            .create_child(&EntryName::new("spool").unwrap())
            .unwrap();
        let writer = xcsc_runtime::Spool::from_directory(directory, limits(1024 * 1024)).unwrap();
        for reason in [
            None,
            Some(QuarantineReason::Corrupt),
            Some(QuarantineReason::IdentityMismatch),
        ] {
            let id = writer
                .enqueue(
                    ContractId::new(HOST_REPORT_CONTRACT).unwrap(),
                    1,
                    BoundedBytes::new(b"pending".to_vec(), CLIENT_REPORT_MAX_BODY_BYTES).unwrap(),
                )
                .unwrap();
            if let Some(reason) = reason {
                writer.quarantine(&id, reason).unwrap();
            }
        }
        let snapshot = || -> BTreeMap<_, _> {
            fs::read_dir(path.join("spool"))
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let name = entry.file_name();
                    let bytes = if name == "spool.instance.lock" {
                        // Windows byte-range locks intentionally prohibit reads.
                        assert_eq!(entry.metadata().unwrap().len(), 0);
                        Vec::new()
                    } else {
                        fs::read(entry.path()).unwrap()
                    };
                    (name, bytes)
                })
                .collect()
        };
        let before = snapshot();
        let expected = writer.doctor().unwrap();
        let actual = inspect_existing(&path, 1024 * 1024).unwrap();
        assert_eq!(actual.spool_entries, expected.spool_entries);
        assert_eq!(actual.spool_bytes, expected.spool_bytes);
        assert_eq!(actual.quarantined_entries, expected.quarantined_entries);
        assert_eq!(
            actual.identity_mismatch_entries,
            expected.identity_mismatch_entries
        );
        assert_eq!(actual.healthy, expected.healthy);
        assert_eq!(actual.capacity_remaining, expected.capacity_remaining);
        assert_eq!(actual.spool_entries, 1);
        assert_eq!(actual.quarantined_entries, 2);
        assert_eq!(actual.identity_mismatch_entries, 1);
        assert!(
            !inspect_existing(&path, actual.spool_bytes)
                .unwrap()
                .capacity_remaining
        );
        assert!(inspect_existing(&path, actual.spool_bytes - 1).is_err());
        assert_eq!(snapshot(), before);

        let directory = crate::maintenance::open_runtime_directory(&path.join("spool")).unwrap();
        AtomicFile::create(
            &directory,
            &EntryName::new("unknown-file").unwrap(),
            b"evidence",
        )
        .unwrap();
        assert!(inspect_existing(&path, 1024 * 1024).is_err());
        assert_eq!(
            fs::read(path.join("spool/unknown-file")).unwrap(),
            b"evidence"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_inspection_does_not_create_missing_state() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().canonicalize().unwrap().join("missing");
        assert!(matches!(
            inspect_existing(&path, 0),
            Err(xcsc_runtime::Error::InvalidLimits)
        ));
        assert!(matches!(
            inspect_existing(&path, 1024 * 1024),
            Err(xcsc_runtime::Error::Filesystem(xcsc_fs_safety::Error::Io(error)))
                if error.kind() == io::ErrorKind::NotFound
        ));
        assert!(!path.exists());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn invalid_limits_do_not_create_state_or_session_lock() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-session-limits-{}", Uuid::new_v4()));
        assert!(Spool::open(&path, 0).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn spool_clones_keep_the_delivery_session_but_do_not_block_pairing() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-session-lifetime-{}", Uuid::new_v4()));
        let spool = Spool::open(&path, 1024 * 1024).unwrap();
        let clone = spool.clone();
        drop(spool);
        assert!(matches!(
            ClientSession::open(&path),
            Err(xcsc_runtime::Error::AlreadyRunning)
        ));
        let transaction = crate::state_store::StateTransaction::begin(&path).unwrap();
        transaction
            .write(
                crate::state_store::StateFile::Credential,
                "pairing-can-commit",
            )
            .unwrap();
        drop(transaction);
        drop(clone);
        let session = ClientSession::open(&path).unwrap();
        drop(session);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn spool_creation_uses_the_sessions_anchored_state_directory() {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-session-anchor-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state");
        let session = Arc::new(ClientSession::open(&path).unwrap());
        fs::rename(&path, root.join("held")).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), "replacement").unwrap();
        let spool = Spool::from_session(session, 1024 * 1024).unwrap();
        assert_eq!(spool.pending_count().unwrap(), 0);
        assert!(root.join("held/spool").is_dir());
        assert!(!path.join("spool").exists());
        assert_eq!(
            fs::read_to_string(path.join("sentinel")).unwrap(),
            "replacement"
        );
        drop(spool);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;
    use crate::model::{ClientHealth, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot};
    use chrono::{DateTime, Duration, Utc};
    use uuid::Uuid;

    fn sample_report(collected_at: DateTime<Utc>) -> ClientReport {
        ClientReport {
            schema_version: crate::model::CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at,
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "linux".into(),
                os_version: None,
                kernel_version: None,
                arch: "x86_64".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                hardware: None,
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 10.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![10.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 100,
                    used_bytes: 50,
                    available_bytes: 50,
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
    fn clock_rollback_does_not_quarantine_a_valid_queued_report() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().canonicalize().unwrap().join("state");
        let spool = Spool::open(&state, 1024 * 1024).unwrap();
        // Models a report collected and queued before the local clock moves back.
        let report = sample_report(Utc::now() + Duration::minutes(10));
        let (canonical, bytes) = report_contract::canonical_spool_report(&report).unwrap();
        assert_eq!(canonical, report);
        spool
            .inner
            .enqueue(
                ContractId::new(HOST_REPORT_CONTRACT).unwrap(),
                report.collected_at.timestamp_micros(),
                BoundedBytes::new(bytes, CLIENT_REPORT_MAX_BODY_BYTES).unwrap(),
            )
            .unwrap();

        let pending = spool.oldest().unwrap().expect("report remains deliverable");
        assert_eq!(pending.report, report);
        assert_eq!(spool.pending_count().unwrap(), 1);
        // Fresh direct sends still apply a current-time bound; queued sends
        // retain the original bytes and let the Server retry a future date.
        assert_ne!(
            report_contract::encode_report_body(&report).unwrap().0,
            report
        );
    }

    #[test]
    fn quarantining_more_than_one_batch_of_old_reports_reaches_the_valid_report() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().canonicalize().unwrap().join("state");
        let spool = Spool::open(&state, 1024 * 1024).unwrap();
        for _ in 0..33 {
            spool
                .inner
                .enqueue(
                    ContractId::new(HOST_REPORT_CONTRACT).unwrap(),
                    1,
                    BoundedBytes::new(
                        br#"{"schema_version":1}"#.to_vec(),
                        CLIENT_REPORT_MAX_BODY_BYTES,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let report = sample_report(Utc::now());
        spool.enqueue(&report).unwrap();

        let pending = spool
            .oldest()
            .unwrap()
            .expect("valid report follows old reports");
        assert_eq!(pending.report.report_id, report.report_id);
        assert_eq!(spool.pending_count().unwrap(), 1);
        assert_eq!(spool.health().unwrap().quarantined_entries, 33);
        xcsc_runtime::DeliveryQueue::acknowledge(&spool, &pending).unwrap();
        assert_eq!(spool.pending_count().unwrap(), 0);
    }

    #[test]
    fn malformed_and_foreign_head_records_do_not_block_a_valid_tail_report() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().canonicalize().unwrap().join("state");
        let spool = Spool::open(&state, 1024 * 1024).unwrap();
        let valid = sample_report(Utc::now());
        let mut noncanonical = valid.clone();
        noncanonical.report_id = Uuid::new_v4().to_string();
        noncanonical.system.cpu.usage_percent = 150.0;

        for (contract, payload) in [
            ("example.foreign", b"foreign payload".to_vec()),
            (HOST_REPORT_CONTRACT, b"{malformed JSON".to_vec()),
            (
                HOST_REPORT_CONTRACT,
                serde_json::to_vec(&noncanonical).unwrap(),
            ),
        ] {
            spool
                .inner
                .enqueue(
                    ContractId::new(contract).unwrap(),
                    1,
                    BoundedBytes::new(payload, CLIENT_REPORT_MAX_BODY_BYTES).unwrap(),
                )
                .unwrap();
        }
        spool.enqueue(&valid).unwrap();

        let pending = spool
            .oldest()
            .unwrap()
            .expect("valid tail remains deliverable");
        assert_eq!(pending.report.report_id, valid.report_id);
        assert_eq!(spool.pending_count().unwrap(), 1);
        assert_eq!(spool.health().unwrap().quarantined_entries, 3);
        xcsc_runtime::DeliveryQueue::acknowledge(&spool, &pending).unwrap();
        assert!(spool.oldest().unwrap().is_none());
    }

    #[test]
    fn old_reports_are_isolated_without_a_queue_failure() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().canonicalize().unwrap().join("state");
        let spool = Spool::open(&state, 1024 * 1024).unwrap();
        for _ in 0..4 {
            spool
                .inner
                .enqueue(
                    ContractId::new(HOST_REPORT_CONTRACT).unwrap(),
                    1,
                    BoundedBytes::new(
                        br#"{"schema_version":1}"#.to_vec(),
                        CLIENT_REPORT_MAX_BODY_BYTES,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(spool.pending_count().unwrap(), 4);
        assert!(spool.oldest().unwrap().is_none());
        assert_eq!(spool.pending_count().unwrap(), 0);
    }
}
