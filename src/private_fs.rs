#[cfg(not(unix))]
use std::{io, path::Path};
#[cfg(all(unix, test))]
use std::{io, path::Path};

/// Bounded native private reads use the shared descriptor and service policy.
#[cfg(not(unix))]
pub(crate) fn read_private(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    use xcsc_fs_safety::EntryName;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    let directory = crate::maintenance::open_runtime_directory(parent).map_err(io::Error::other)?;
    let name = EntryName::new(
        path.file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?,
    )
    .map_err(io_error)?;
    directory
        .read_private_bounded(&name, max_bytes)
        .map_err(io_error)
}

/// Create private state or validate its existing mode/owner without repairing it.
#[cfg(test)]
pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        xcsc_fs_safety::PrivateDirectory::create_for_administration(std::path::absolute(path)?)
            .map_err(io::Error::other)?;
    }
    #[cfg(not(unix))]
    drop(crate::maintenance::runtime_directory(path).map_err(io::Error::other)?);
    Ok(())
}

/// Descriptor-based atomic publication preserves the shared private service policy.
#[cfg(not(unix))]
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    use xcsc_fs_safety::{AtomicFile, EntryName};
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    let directory = crate::maintenance::open_runtime_directory(parent).map_err(io::Error::other)?;
    let name = EntryName::new(
        target
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?,
    )
    .map_err(io_error)?;
    AtomicFile::replace(&directory, &name.as_relative(), bytes).map_err(io_error)
}
#[cfg(not(unix))]
fn io_error(error: xcsc_fs_safety::Error) -> io::Error {
    match error {
        xcsc_fs_safety::Error::Io(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}
