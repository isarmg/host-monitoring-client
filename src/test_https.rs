use std::{
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

pub(crate) struct TestHttpsServer {
    pub(crate) origin: String,
    pub(crate) ca_path: PathBuf,
    listener: TcpListener,
    config: Arc<ServerConfig>,
    _directory: tempfile::TempDir,
}

impl TestHttpsServer {
    pub(crate) fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], key.into())
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        // macOS reports its temporary directory through `/var`, which is a
        // symlink to `/private/var`. The production TLS reader deliberately
        // rejects paths that traverse symlinks, so keep the fixture's CA path
        // in its canonical, physical form.
        let ca_path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("test-root.pem");
        std::fs::write(&ca_path, cert.pem()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ca_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            origin: format!("https://localhost:{port}"),
            ca_path,
            listener,
            config: Arc::new(config),
            _directory: directory,
        }
    }

    pub(crate) fn accept(&self) -> StreamOwned<ServerConnection, TcpStream> {
        let (stream, _) = self.listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        StreamOwned::new(ServerConnection::new(self.config.clone()).unwrap(), stream)
    }
}
