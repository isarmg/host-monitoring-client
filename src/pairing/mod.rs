//! Browser-authorized, zero-copy host pairing.
//!
//! The browser approves a pending request but never receives either secret.
//! Both the future report bearer token and the independent polling secret are
//! generated locally. Creation sends only their SHA-256 hashes; status polling
//! exposes its secret in a sensitive Authorization header to the configured
//! Server, and the activated bearer credential authenticates reports.

#[cfg(test)]
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, bail};
use chrono::{TimeDelta, Utc};
use host_protocol::{
    ActivateClientRequestRef as ActivatePairingRequest,
    ActivateClientResponse as ActivatePairingResponse, ActivatePairingStatus,
    ClientPairingResponse as CreatePairingResponse,
    ClientPairingStatusResponse as PairingStatusResponse, HOST_PAIRING_PROTOCOL_VERSION,
    PairingStatus,
};
use sarmg_client_secure_http::{StatusCode, header};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    config::ClientConfig,
    model::HostIdentity,
    state_store::{StateFile, StateReader, StateTransaction},
    transport::{Reporter, build_client},
};

mod activation;
mod client;
mod commit;
#[cfg(test)]
mod credential_tests;
mod credentials;
pub(crate) use credentials::HostCredentials;
use sarmg_client_runtime::{CredentialAuthorization, CredentialStore};
mod state;

use activation::*;
use client::*;
use commit::*;
use state::*;
pub use state::{LocalAuthState, LocalPairingStatus, PairingProgress, PairingSession};

pub use host_protocol::ClientPairingMode as PairMode;

/// Pairing compatibility is explicit and independent from both the application
/// release string and the telemetry report schema.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CreatePairingRequest {
    protocol_version: u16,
    mode: PairMode,
    host: HostIdentity,
    token_hash: String,
    polling_secret_hash: String,
}

// Flow fragments stay in this module scope so the state machine retains its
// existing private visibility and compare-and-swap transaction invariants.
include!("create.rs");
include!("activation_flow.rs");
include!("polling.rs");
include!("commit_flow.rs");
include!("local.rs");
include!("state_storage.rs");

/// HTTP rejection is distinct from a transport failure with an uncertain result.
#[derive(Debug, thiserror::Error)]
#[error("pairing endpoint returned HTTP {status}; inspect the saved transaction before retrying")]
pub struct PairingHttpError {
    pub status: u16,
    pub code: Option<&'static str>,
    pub received: Option<u16>,
    pub supported: Vec<u16>,
}

pub use state::PERSISTED_STATE_FORMAT;
