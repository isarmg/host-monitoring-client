//! Outer, bounded maintenance gate. Fixed order: maintenance, instance, state.
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
pub struct Guard {
    _directory: sarmg_client_fs_safety::PrivateDirectory,
    _lock: sarmg_client_fs_safety::AdvisoryLock,
}
#[cfg(unix)]
impl Guard {
    pub fn acquire(path: &Path) -> anyhow::Result<Self> {
        use sarmg_client_fs_safety::{AdvisoryLock, EntryName, PrivateDirectory};
        let directory = PrivateDirectory::create_for_administration(path)?;
        let lock = AdvisoryLock::acquire(
            &directory,
            &EntryName::new("maintenance.lock")?.as_relative(),
        )?;
        Ok(Self {
            _directory: directory,
            _lock: lock,
        })
    }
}
#[cfg(windows)]
#[path = "windows_maintenance_gate.rs"]
pub(crate) mod windows;
#[cfg(windows)]
pub use windows::Guard;
