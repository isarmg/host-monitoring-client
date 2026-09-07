use std::{io, path::Path, sync::Arc};

use anyhow::Context;
use sarmg_client_fs_safety::EntryName;
use sarmg_client_runtime::{
    BoundedBytes, ClientSession, ContractId, MAX_SPOOL_ENTRIES, RecordId, SpoolLimits,
};

use crate::{
    model::{CLIENT_REPORT_MAX_BODY_BYTES, ClientReport},
    report_contract,
};

use crate::client_identity::HOST_REPORT_CONTRACT;

#[derive(Clone)]
pub struct Spool {
    inner: Arc<sarmg_client_runtime::Spool>,
    _session: Arc<ClientSession>,
}

pub struct PendingReport {
    record_id: RecordId,
    pub report: ClientReport,
}

impl Spool {
    pub fn open(state_dir: &Path, max_bytes: u64) -> io::Result<Self> {
        limits(max_bytes).validate().map_err(io::Error::other)?;
        let session = Arc::new(ClientSession::open(state_dir).map_err(io::Error::other)?);
        Self::from_session(session, max_bytes)
    }

    pub fn from_session(session: Arc<ClientSession>, max_bytes: u64) -> io::Result<Self> {
        let limits = limits(max_bytes);
        limits.validate().map_err(io::Error::other)?;
        let directory = session
            .directory()
            .create_child(&EntryName::new("spool").map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        let inner = sarmg_client_runtime::Spool::from_directory(directory, limits)
            .map_err(io::Error::other)?;
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
        let (_, bytes) = report_contract::encode_report_body(report)?;
        self.inner.enqueue(
            ContractId::new(HOST_REPORT_CONTRACT)?,
            report.collected_at.timestamp_micros(),
            BoundedBytes::new(bytes, CLIENT_REPORT_MAX_BODY_BYTES)?,
        )?;
        Ok(())
    }

    pub fn oldest(&self) -> anyhow::Result<Option<PendingReport>> {
        let Some(record) = self.inner.next()? else {
            return Ok(None);
        };
        if record.contract_id.as_str() != HOST_REPORT_CONTRACT {
            self.inner.quarantine(
                &record.record_id,
                sarmg_client_runtime::QuarantineReason::Corrupt,
            )?;
            anyhow::bail!("Foundation spool payload has a different contract identifier");
        }
        let parsed = serde_json::from_slice::<ClientReport>(record.payload.as_slice())
            .context("Foundation spool payload is not a Host Client report")
            .and_then(|report| {
                let (canonical, _) = report_contract::encode_report_body(&report)?;
                anyhow::ensure!(
                    canonical == report,
                    "spool payload is not the current canonical Host report"
                );
                Ok(report)
            });
        match parsed {
            Ok(report) => Ok(Some(PendingReport {
                record_id: record.record_id,
                report,
            })),
            Err(error) => {
                self.inner.quarantine(
                    &record.record_id,
                    sarmg_client_runtime::QuarantineReason::Corrupt,
                )?;
                Err(error)
            }
        }
    }

    pub fn health(&self) -> io::Result<sarmg_client_runtime::ClientHealth> {
        self.inner.doctor().map_err(io::Error::other)
    }
}

impl sarmg_client_runtime::DeliveryQueue for Spool {
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
        reason: sarmg_client_runtime::QuarantineReason,
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
            Err(sarmg_client_runtime::Error::AlreadyRunning)
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
