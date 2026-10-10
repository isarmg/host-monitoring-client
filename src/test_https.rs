use std::{
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
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
        listener.set_nonblocking(true).unwrap();
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
        let ca_root = directory.path().canonicalize().unwrap().join("trust");
        let private = xcsc::fs_safety::PrivateDirectory::create(&ca_root).unwrap();
        xcsc::fs_safety::AtomicFile::create(
            &private,
            &xcsc::fs_safety::EntryName::new("test-root.pem").unwrap(),
            cert.pem().as_bytes(),
        )
        .unwrap();
        let ca_path = ca_root.join("test-root.pem");
        Self {
            origin: format!("https://localhost:{port}"),
            ca_path,
            listener,
            config: Arc::new(config),
            _directory: directory,
        }
    }

    pub(crate) fn accept(&self) -> StreamOwned<ServerConnection, TcpStream> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (stream, _) = loop {
            match self.listener.accept() {
                Ok(peer) => break peer,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "test HTTPS client did not connect before deadline"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => panic!("test HTTPS listener failed"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        StreamOwned::new(ServerConnection::new(self.config.clone()).unwrap(), stream)
    }
}
