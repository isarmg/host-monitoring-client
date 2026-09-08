//! Native, nonblocking credential transaction lock. Creates only missing private state; never repairs existing ACLs.
use std::path::Path;
pub(crate) struct CredentialStateLock {
    _guard: crate::maintenance::Guard,
}
pub(crate) fn lock(state_dir: &Path) -> anyhow::Result<CredentialStateLock> {
    Ok(CredentialStateLock {
        _guard: crate::maintenance::Guard::acquire_named(state_dir, ".credential-state.lock")?,
    })
}
