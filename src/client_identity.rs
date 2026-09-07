//! Host's adapter between its UUID wire contract and Foundation runtime identity.
use crate::state_store::{StateFile, StateReader};
use sarmg_client_runtime::{ClientIdentity, ContractId};
use std::{io, path::Path};

pub(crate) const HOST_REPORT_CONTRACT: &str = "host-monitoring.client-report.current";

#[derive(Debug, thiserror::Error)]
pub enum IdentityLoadError {
    #[error("could not safely read Client identity state: {0}")]
    State(#[from] io::Error),
    #[error("host identity is not valid current identity data")]
    Invalid,
}

pub(crate) fn for_instance(value: &str) -> io::Result<ClientIdentity> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "host identity must be a canonical lowercase hyphenated UUID",
        )
    };
    let id = uuid::Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.to_string() != value {
        return Err(invalid());
    }
    ClientIdentity::new(
        "host-monitoring",
        value,
        ContractId::new(HOST_REPORT_CONTRACT).map_err(io::Error::other)?,
    )
    .map_err(io::Error::other)
}

/// Read-only: never creates, locks, repairs or generates a durable identity.
pub fn load(state_dir: &Path) -> Result<ClientIdentity, IdentityLoadError> {
    from_state(&StateReader::open(state_dir)?)
}

pub(crate) fn from_state(reader: &StateReader) -> Result<ClientIdentity, IdentityLoadError> {
    let bytes = reader.read(StateFile::Identity)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| IdentityLoadError::Invalid)?;
    for_instance(value.trim()).map_err(|_| IdentityLoadError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_wire_uuid_constraint_is_stricter_than_generic_runtime_identity() {
        let canonical = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let identity = for_instance(canonical).unwrap();
        assert_eq!(identity.product_id(), "host-monitoring");
        assert_eq!(identity.instance_id(), canonical);
        assert_eq!(identity.contract_id().as_str(), HOST_REPORT_CONTRACT);
        for invalid in [
            canonical.to_uppercase(),
            canonical.replace('-', ""),
            format!(" {canonical}"),
            "not-a-uuid".into(),
        ] {
            assert!(for_instance(&invalid).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn identity_load_is_anchored_read_only_and_never_echoes_bad_contents() {
        use crate::state_store::StateTransaction;
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
        };
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-typed-identity-{}", uuid::Uuid::new_v4()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        assert!(
            matches!(load(&root), Err(IdentityLoadError::State(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        assert!(!root.exists());
        let transaction = StateTransaction::begin(&root).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        transaction
            .write(StateFile::Identity, &format!("{id}\n"))
            .unwrap();
        let expected = for_instance(&id).unwrap();
        assert_eq!(from_state(&transaction).unwrap(), expected);
        drop(transaction);
        let reader = StateReader::open(&root).unwrap();
        let old = root.join("original");
        let bytes = fs::read(root.join("host-id")).unwrap();
        fs::rename(root.join("host-id"), &old).unwrap();
        symlink(&old, root.join("host-id")).unwrap();
        assert!(matches!(
            from_state(&reader),
            Err(IdentityLoadError::State(_))
        ));
        assert_eq!(fs::read(&old).unwrap(), bytes);
        fs::remove_file(root.join("host-id")).unwrap();
        fs::write(root.join("host-id"), "private-invalid-identity-marker").unwrap();
        fs::set_permissions(root.join("host-id"), fs::Permissions::from_mode(0o600)).unwrap();
        let error = from_state(&reader).unwrap_err();
        assert!(matches!(error, IdentityLoadError::Invalid));
        assert!(!format!("{error:?}/{error}").contains("private-invalid"));
        assert_eq!(
            fs::read_to_string(root.join("host-id")).unwrap(),
            "private-invalid-identity-marker"
        );
    }
}
