//! Client-facing re-export of the shared, versioned telemetry wire contract.
//!
//! Platform collectors convert native values into these fixed-width DTOs at their boundary. Keep
//! Client-only configuration and runtime state out of this module.

pub use host_protocol::*;
