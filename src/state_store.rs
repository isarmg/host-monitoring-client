//! Host state names, budgets and transaction capabilities. Unix filesystem
//! mechanics belong to Client Foundation; Windows holds native directory handles and validates ACLs.
#[cfg(unix)]
use sarmg_client_fs_safety::EntryName;
#[cfg(unix)]
use sarmg_client_fs_safety::{AdvisoryLock, AtomicFile, PrivateDirectory};
use std::{
    io,
    ops::Deref,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum StateFile {
    Identity,
    Credential,
    Pairing,
    Authorization,
    Binding,
}

impl StateFile {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Identity => "host-id",
            Self::Credential => "client-token",
            Self::Pairing => "pairing-state.json",
            Self::Authorization => "auth-state.json",
            Self::Binding => "active-binding.json",
        }
    }
    pub(crate) const fn max_bytes(self) -> usize {
        match self {
            Self::Identity => 128,
            Self::Credential => 4096,
            Self::Pairing => 64 * 1024,
            Self::Authorization | Self::Binding => 16 * 1024,
        }
    }
    #[cfg(unix)]
    fn entry(self) -> EntryName {
        EntryName::new(self.name()).expect("fixed Host state entry")
    }
}

pub(crate) struct StateReader {
    path: PathBuf,
    #[cfg(unix)]
    directory: PrivateDirectory,
    #[cfg(windows)]
    _directory: crate::maintenance::windows::DirectoryGuard,
}

impl StateReader {
    /// No locks, creation, repair or recovery. Hold this reader for the entire
    /// multi-file diagnostic snapshot, not one pathname reopen per file.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        #[cfg(unix)]
        let directory = PrivateDirectory::open_for_administration(&path).map_err(io_error)?;
        #[cfg(windows)]
        let directory = crate::maintenance::windows::open_root(&path, false)?;
        Ok(Self {
            path,
            #[cfg(unix)]
            directory,
            #[cfg(windows)]
            _directory: directory,
        })
    }
    pub(crate) fn path(&self, file: StateFile) -> PathBuf {
        self.path.join(file.name())
    }
    pub(crate) fn read(&self, file: StateFile) -> io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            self.directory
                .read_private_bounded(&file.entry(), file.max_bytes())
                .map_err(io_error)
        }
        #[cfg(not(unix))]
        {
            crate::private_fs::read_private(&self.path(file), file.max_bytes())
        }
    }
}

/// A write capability cannot be obtained without the transaction lock. Readers,
/// writers and the lock all use the same Unix directory descriptor; this object
/// must be dropped before network I/O.
pub(crate) struct StateTransaction {
    #[cfg(unix)]
    _lock: AdvisoryLock,
    #[cfg(not(unix))]
    _lock: crate::state_lock::CredentialStateLock,
    reader: StateReader,
}
impl Deref for StateTransaction {
    type Target = StateReader;
    fn deref(&self) -> &Self::Target {
        &self.reader
    }
}
impl StateTransaction {
    pub(crate) fn begin(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        #[cfg(unix)]
        {
            let directory = PrivateDirectory::create_for_administration(&path).map_err(io_error)?;
            let lock = AdvisoryLock::acquire(
                &directory,
                &EntryName::new(".credential-state.lock")
                    .expect("fixed lock entry")
                    .as_relative(),
            )
            .map_err(io_error)?;
            Ok(Self {
                _lock: lock,
                reader: StateReader { path, directory },
            })
        }
        #[cfg(not(unix))]
        {
            let lock = crate::state_lock::lock(&path).map_err(io::Error::other)?;
            Ok(Self {
                _lock: lock,
                reader: StateReader::open(&path)?,
            })
        }
    }
    pub(crate) fn write(&self, file: StateFile, value: &str) -> io::Result<()> {
        let bytes = value.trim().as_bytes();
        if bytes.is_empty() || bytes.len() > file.max_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private state is empty or exceeds its byte budget",
            ));
        }
        #[cfg(unix)]
        {
            AtomicFile::replace(&self.reader.directory, &file.entry().as_relative(), bytes)
                .map_err(io_error)
        }
        #[cfg(not(unix))]
        {
            crate::private_fs::write_atomic(&self.path(file), bytes)
        }
    }
}

#[cfg(unix)]
fn io_error(error: sarmg_client_fs_safety::Error) -> io::Error {
    match error {
        sarmg_client_fs_safety::Error::Io(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn transaction_budgets_are_symmetric_and_rejections_preserve_committed_state() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-state-budget-{}", uuid::Uuid::new_v4()));
        let transaction = StateTransaction::begin(&path).unwrap();
        for file in [
            StateFile::Identity,
            StateFile::Credential,
            StateFile::Pairing,
            StateFile::Authorization,
            StateFile::Binding,
        ] {
            let content = "a".repeat(file.max_bytes());
            transaction.write(file, &content).unwrap();
            assert_eq!(transaction.read(file).unwrap(), content.as_bytes());
            assert!(transaction.write(file, &format!("{content}a")).is_err());
            assert!(transaction.write(file, " \n").is_err());
            assert_eq!(transaction.read(file).unwrap(), content.as_bytes());
        }
        assert_eq!(fs::read_dir(&path).unwrap().count(), 6);
        drop(transaction);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn transaction_and_read_only_capabilities_reject_unsafe_paths_and_links() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-state-links-{}", uuid::Uuid::new_v4()));
        assert!(StateReader::open(&path).is_err());
        assert!(!path.exists());
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(StateTransaction::begin(&path).is_err());
        assert!(!path.join(".credential-state.lock").exists());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let victim = path.join("victim");
        fs::write(&victim, "untouched").unwrap();
        let lock = path.join(".credential-state.lock");
        symlink(&victim, &lock).unwrap();
        assert!(StateTransaction::begin(&path).is_err());
        fs::remove_file(&lock).unwrap();
        fs::hard_link(&victim, &lock).unwrap();
        assert!(StateTransaction::begin(&path).is_err());
        fs::remove_file(&lock).unwrap();
        let transaction = StateTransaction::begin(&path).unwrap();
        let target = transaction.path(StateFile::Credential);
        symlink(&victim, &target).unwrap();
        assert!(transaction.write(StateFile::Credential, "secret").is_err());
        fs::remove_file(&target).unwrap();
        fs::hard_link(&victim, &target).unwrap();
        assert!(transaction.write(StateFile::Credential, "secret").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
        drop(transaction);
        fs::remove_dir_all(path).unwrap();
    }
}
