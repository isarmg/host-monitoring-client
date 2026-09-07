use sarmg_client_secret::SecretString;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use uuid::Uuid;

use crate::model::HostIdentity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PairingStateVersion;

pub(super) const PAIRING_STATE_VERSION: PairingStateVersion = PairingStateVersion;
#[cfg(test)]
pub(super) const PAIRING_STATE_FILE: &str = crate::state_store::StateFile::Pairing.name();
#[cfg(all(test, unix))]
pub(super) const AUTH_STATE_FILE: &str = crate::state_store::StateFile::Authorization.name();
#[cfg(test)]
pub(super) const ACTIVE_BINDING_FILE: &str = crate::state_store::StateFile::Binding.name();

impl Serialize for PairingStateVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(env!("CARGO_PKG_VERSION"))
    }
}

impl<'de> Deserialize<'de> for PairingStateVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = String::deserialize(deserializer)?;
        if version == env!("CARGO_PKG_VERSION") {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "pairing state belongs to Client {version}, expected {}",
                env!("CARGO_PKG_VERSION")
            )))
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StoredPairingState {
    Creating {
        version: PairingStateVersion,
        generation: Uuid,
        pairing_endpoint: String,
        report_endpoint: String,
        host: HostIdentity,
        #[serde(with = "crate::secret_io")]
        bearer_secret: Arc<SecretString>,
        #[serde(with = "crate::secret_io")]
        polling_secret: Arc<SecretString>,
    },
    Pending {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        expires_at: DateTime<Utc>,
        poll_interval: u64,
        pairing_endpoint: String,
        report_endpoint: String,
        #[serde(with = "crate::secret_io")]
        bearer_secret: Arc<SecretString>,
        #[serde(with = "crate::secret_io")]
        polling_secret: Arc<SecretString>,
    },
    /// Durable local commit journal. Once this phase exists, no network I/O is
    /// allowed; startup idempotently completes the token/identity/endpoint binding
    /// transition before writing Active last.
    Activating {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        expires_at: DateTime<Utc>,
        poll_interval: u64,
        instance_id: Uuid,
        pairing_endpoint: String,
        report_endpoint: String,
        #[serde(with = "crate::secret_io")]
        bearer_secret: Arc<SecretString>,
    },
    Active {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        instance_id: Uuid,
        report_endpoint: String,
        completed_at: DateTime<Utc>,
    },
    Denied {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        report_endpoint: String,
        completed_at: DateTime<Utc>,
    },
    Expired {
        version: PairingStateVersion,
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
        report_endpoint: String,
        completed_at: DateTime<Utc>,
    },
}

/// Durable binding between the current credential generation and its report endpoint.
/// This lives beside the token rather than in the administrator-owned base config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveBinding {
    pub(super) version: PairingStateVersion,
    pub(super) generation: Uuid,
    pub(super) request_id: Uuid,
    pub(super) instance_id: Uuid,
    pub(super) report_endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingSession {
    pub generation: Uuid,
    pub request_id: Uuid,
    pub activation_url: String,
    pub expires_at: DateTime<Utc>,
    pub poll_interval: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", content = "details", rename_all = "snake_case")]
pub enum PairingProgress {
    Creating {
        generation: Uuid,
        report_endpoint: String,
    },
    Waiting(PairingSession),
    Active {
        generation: Uuid,
        request_id: Uuid,
        instance_id: Uuid,
        report_endpoint: String,
    },
    Denied {
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
    },
    Expired {
        generation: Uuid,
        request_id: Uuid,
        activation_url: String,
    },
}

/// One read-only view used by `status`; it never creates locks or changes state.
#[derive(Debug)]
pub struct LocalPairingStatus {
    pub progress: Option<PairingProgress>,
    pub active_report_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAuthState {
    pub(super) version: PairingStateVersion,
    pub status: sarmg_client_runtime::CredentialAuthorization,
    pub reason: String,
    pub changed_at: DateTime<Utc>,
}

impl PairingProgress {
    pub fn active_request_id(&self) -> Option<Uuid> {
        match self {
            Self::Active { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }
}
