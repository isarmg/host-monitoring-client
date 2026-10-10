//! Cross-platform read-only telemetry client for xsos.
//!
//! The telemetry process listens on no business port, executes no server commands and contains no self-updater.
//! Configuration and pairing use the CLI; the operating system service manager runs the background process.
//! Capabilities express platform differences; unavailable data uses `None` rather than a fabricated zero.

#[cfg(feature = "desktop")]
pub mod client_identity;
#[cfg(feature = "desktop")]
pub mod collectors;
#[cfg(feature = "desktop")]
pub mod config;
pub mod mobile;
pub mod model;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "desktop")]
pub mod pairing;
#[cfg(feature = "desktop")]
pub mod pairing_input;
#[cfg(all(feature = "desktop", any(not(unix), test)))]
mod private_fs;
mod report_contract;
#[cfg(feature = "desktop")]
mod secret_io;
#[cfg(feature = "desktop")]
pub mod service;
#[cfg(feature = "desktop")]
pub mod spool;
#[cfg(feature = "desktop")]
mod state_store;
#[cfg(all(test, feature = "desktop"))]
mod test_https;
#[cfg(feature = "desktop")]
mod tls_input;
#[cfg(feature = "desktop")]
pub mod transport;

#[cfg(feature = "desktop")]
pub use collectors::SystemSampler;
#[cfg(feature = "desktop")]
pub use config::{ClientCommand, ClientConfig, OutputMode};
pub use model::*;

#[cfg(feature = "desktop")]
pub const MAX_TLS_INPUT_BYTES: usize = 1024 * 1024;

#[cfg(feature = "desktop")]
pub mod maintenance;

#[cfg(feature = "desktop")]
pub use xcsc::runtime::local_status as runtime_status;
