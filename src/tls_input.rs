//! Product TLS file selection; safety and the byte ceiling belong to Foundation.

#[cfg(unix)]
use anyhow::Context;
use sarmg_client_secret::SecretBytes;
use sarmg_client_secure_http::MAX_TLS_INPUT_BYTES;
use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) enum TlsInput {
    Identity,
    TrustAnchor,
}

pub(crate) fn read(path: &Path, kind: TlsInput) -> anyhow::Result<SecretBytes> {
    #[cfg(unix)]
    let bytes = {
        use sarmg_client_fs_safety::{ConfigurationDirectory, EntryName, InputVisibility};
        let path = std::path::absolute(path)?;
        let directory =
            ConfigurationDirectory::open(path.parent().context("TLS input has no parent")?)?;
        let name = EntryName::new(path.file_name().context("TLS input has no filename")?)?;
        let visibility = match kind {
            TlsInput::Identity => InputVisibility::Confidential,
            TlsInput::TrustAnchor => InputVisibility::Public,
        };
        directory.read_input_bounded(&name, MAX_TLS_INPUT_BYTES, visibility)?
    };
    #[cfg(not(unix))]
    let bytes = {
        // Windows native handle/reparse-point/ACL adoption is still pending.
        // This is a bounded read, not a claim of Unix security on Windows.
        let _ = kind;
        crate::private_fs::read_private(path, MAX_TLS_INPUT_BYTES)?
    };
    let bytes = SecretBytes::new(bytes);
    anyhow::ensure!(!bytes.expose().is_empty(), "TLS input is empty");
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    use uuid::Uuid;

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .expect("physical test temporary directory")
                .join(format!("host-tls-input-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn tls_inputs_are_bounded_and_reject_links_and_unsafe_permissions() {
        let directory = Directory::new();
        let path = directory.0.join("input.pem");
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read(&path, TlsInput::Identity).is_err());
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_TLS_INPUT_BYTES as u64).unwrap();
        assert_eq!(
            read(&path, TlsInput::Identity).unwrap().expose().len(),
            MAX_TLS_INPUT_BYTES
        );
        file.set_len((MAX_TLS_INPUT_BYTES + 1) as u64).unwrap();
        assert!(read(&path, TlsInput::Identity).is_err());
        assert!(read(&path, TlsInput::TrustAnchor).is_err());
        assert_eq!(
            file.metadata().unwrap().len(),
            (MAX_TLS_INPUT_BYTES + 1) as u64
        );
        drop(file);
        fs::write(&path, b"private-input-marker").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path, TlsInput::Identity).is_err());
        assert!(read(&path, TlsInput::TrustAnchor).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read(&path, TlsInput::TrustAnchor).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = directory.0.join("alias");
        fs::hard_link(&path, &alias).unwrap();
        assert!(read(&path, TlsInput::Identity).is_err());
        fs::remove_file(&alias).unwrap();
        symlink(&path, &alias).unwrap();
        assert!(read(&alias, TlsInput::TrustAnchor).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"private-input-marker");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn actual_pem_identity_and_public_ca_construct_a_client_and_errors_are_redacted() {
        // Generate ephemeral test-only material; no private key is committed.
        let directory = Directory::new();
        let key = directory.0.join("key.pem");
        let ca = directory.0.join("ca.pem");
        let identity = directory.0.join("identity.pem");
        let output = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                "/CN=test-only.invalid",
                "-days",
                "1",
                "-keyout",
            ])
            .arg(&key)
            .arg("-out")
            .arg(&ca)
            .output()
            .expect("TLS fixture generation requires OpenSSL");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut pem = fs::read(&ca).unwrap();
        pem.extend_from_slice(&fs::read(&key).unwrap());
        fs::write(&identity, pem).unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(&ca, fs::Permissions::from_mode(0o644)).unwrap();
        let config = crate::config::ClientConfig {
            tls_identity_pem: Some(identity.clone()),
            tls_ca_pem: Some(ca.clone()),
            ..crate::config::ClientConfig::default()
        };
        crate::transport::build_client(&config).unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&identity, b"private-input-marker").unwrap();
        let error = crate::transport::build_client(&config).unwrap_err();
        assert!(!format!("{error:#}/{error:?}").contains("private-input-marker"));
        let mut config = config;
        config.tls_identity_pem = None;
        fs::write(&ca, b"private-input-marker").unwrap();
        let error = crate::transport::build_client(&config).unwrap_err();
        assert!(!format!("{error:#}/{error:?}").contains("private-input-marker"));
    }
}
