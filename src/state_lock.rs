//! Windows session-local transaction lock; native Foundation handle/ACL adoption is pending.
use anyhow::Context;
use std::{fs, path::Path};

pub(crate) struct CredentialStateLock(fs::File);
impl Drop for CredentialStateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub(crate) fn lock(state_dir: &Path) -> anyhow::Result<CredentialStateLock> {
    fs::create_dir_all(state_dir).context("failed to create credential state directory")?;
    let path = state_dir.join(".credential-state.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .context("failed to open credential state lock")?;
    file.lock().context("failed to lock credential state")?;
    Ok(CredentialStateLock(file))
}
