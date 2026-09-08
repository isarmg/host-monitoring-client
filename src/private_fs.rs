#[cfg(not(unix))]
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

#[cfg(not(unix))]
use uuid::Uuid;

#[cfg(not(unix))]
use crate::atomic_file;
#[cfg(all(unix, test))]
use std::{io, path::Path};

#[cfg(not(unix))]
const ATOMIC_TEMPORARY_PREFIX: &str = ".private-";
#[cfg(not(unix))]
const ATOMIC_TEMPORARY_SUFFIX: &str = ".tmp";

/// Bounded Windows read with native handle/ACL validation. Unix state reads
/// use StateReader and Foundation directly, never this path-based helper.
#[cfg(not(unix))]
pub(crate) fn read_private(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    #[cfg(not(unix))]
    {
        use io::Read;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        let _directory = crate::maintenance::windows::open_root(parent, false)?;
        let file = crate::maintenance::windows::private_file(path, false, false)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > max_bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private file type or size is invalid",
            ));
        }
        let mut bytes = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private file exceeds its byte budget",
            ));
        }
        Ok(bytes)
    }
}

/// Create private state or validate its existing mode/owner without repairing it.
#[cfg(test)]
pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        sarmg_client_fs_safety::PrivateDirectory::create_for_administration(std::path::absolute(
            path,
        )?)
        .map_err(io::Error::other)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}

/// Windows private write-through publication. Unix publication uses Foundation directly.
#[cfg(not(unix))]
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let _directory = crate::maintenance::windows::open_root(parent, false)?;
    if target.try_exists()? {
        drop(crate::maintenance::windows::private_file(
            target, false, false,
        )?);
    }
    let temporary = parent.join(format!(
        "{ATOMIC_TEMPORARY_PREFIX}{}{ATOMIC_TEMPORARY_SUFFIX}",
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = crate::maintenance::windows::create_temporary(&temporary)?;
        // A cleanup pass may run in another Client process. Holding an advisory
        // lock lets it distinguish this live write from a file abandoned by a
        // process that died before the atomic rename.
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        atomic_file::replace(&temporary, target)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
