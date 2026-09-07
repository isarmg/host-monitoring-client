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
    ClientPairingRequest as CreatePairingRequest, ClientPairingResponse as CreatePairingResponse,
    ClientPairingStatusResponse as PairingStatusResponse, PairingStatus,
};
use sarmg_client_secure_http::{StatusCode, header};
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

// Flow fragments stay in this module scope so the state machine retains its
// existing private visibility and compare-and-swap transaction invariants.
include!("create.rs");
include!("activation_flow.rs");
include!("polling.rs");
include!("commit_flow.rs");
include!("local.rs");
include!("state_storage.rs");
