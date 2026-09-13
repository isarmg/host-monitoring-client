//! Actual Reporter TLS handshakes against an independent OpenSSL peer. No
//! verification overrides, committed keys, remote services or platform mocks.
use super::*;
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Certificates(PathBuf);
impl Certificates {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-real-tls-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(path);
        for ca in ["ca", "untrusted"] {
            fixture.openssl(&[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                &format!("/CN={ca}"),
                "-days",
                "1",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
                "-keyout",
                &format!("{ca}.key"),
                "-out",
                &format!("{ca}.crt"),
            ]);
        }
        fixture.leaf("server", "ca", "serverAuth", "127.0.0.1", false);
        fixture.leaf("wrong-host", "ca", "serverAuth", "127.0.0.2", false);
        fixture.leaf("expired", "ca", "serverAuth", "127.0.0.1", true);
        fixture.leaf("client", "ca", "clientAuth", "", false);
        fixture.leaf("wrong-client", "untrusted", "clientAuth", "", false);
        fixture
    }

    fn openssl(&self, arguments: &[&str]) {
        let output = Command::new("openssl")
            .args(arguments)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "OpenSSL fixture generation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn leaf(&self, name: &str, ca: &str, purpose: &str, ip: &str, expired: bool) {
        self.openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            if purpose == "clientAuth" {
                "/CN=host-client-fixture"
            } else {
                "/CN=test-only.invalid"
            },
            "-keyout",
            &format!("{name}.key"),
            "-out",
            &format!("{name}.csr"),
        ]);
        let mut extensions = format!(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage={purpose}\n"
        );
        if !ip.is_empty() {
            extensions.push_str(&format!("subjectAltName=IP:{ip}\n"));
        }
        if expired {
            // A real, correctly ordered validity interval in the past. A
            // negative -days value produces an invalid interval on OpenSSL 3.5.
            fs::write(self.0.join("issued.index"), []).unwrap();
            fs::write(self.0.join("issued.serial"), "01\n").unwrap();
            fs::write(self.0.join("signer.cnf"), format!(
                "[ca]\ndefault_ca=issuer\n[issuer]\ndatabase=issued.index\nnew_certs_dir=.\ncertificate={ca}.crt\nprivate_key={ca}.key\nserial=issued.serial\ndefault_md=sha256\npolicy=subjects\nx509_extensions=leaf\n[subjects]\ncommonName=supplied\n[leaf]\n{extensions}"
            )).unwrap();
            self.openssl(&[
                "ca",
                "-batch",
                "-notext",
                "-config",
                "signer.cnf",
                "-startdate",
                "20000101000000Z",
                "-enddate",
                "20000102000000Z",
                "-in",
                &format!("{name}.csr"),
                "-out",
                &format!("{name}.crt"),
            ]);
        } else {
            fs::write(self.0.join("extensions.cnf"), extensions).unwrap();
            self.openssl(&[
                "x509",
                "-req",
                "-in",
                &format!("{name}.csr"),
                "-CA",
                &format!("{ca}.crt"),
                "-CAkey",
                &format!("{ca}.key"),
                "-set_serial",
                &Uuid::new_v4().as_u128().to_string(),
                "-days",
                "1",
                "-extfile",
                "extensions.cnf",
                "-out",
                &format!("{name}.crt"),
            ]);
        }
        let mut identity = fs::read(self.0.join(format!("{name}.crt"))).unwrap();
        identity.extend_from_slice(&fs::read(self.0.join(format!("{name}.key"))).unwrap());
        let path = self.0.join(format!("{name}.pem"));
        fs::write(&path, identity).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
impl Drop for Certificates {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Peer {
    process: Child,
    events: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    origin: String,
}
impl Peer {
    fn start(root: &Path, certificate: &str, mtls: bool, version: &str, target: &str) -> Self {
        let mut process = Command::new("python3")
            .args(["-u", "-c", include_str!("tls_server.py")])
            .arg(root)
            .args([
                certificate,
                if mtls { "yes" } else { "no" },
                version,
                target,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = process.stdout.take().unwrap();
        let (sender, events) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut peer = Self {
            process,
            events,
            reader: Some(reader),
            origin: String::new(),
        };
        let ready = peer.event();
        let port = ready["port"].as_u64().expect("peer readiness port");
        peer.origin = format!("https://127.0.0.1:{port}");
        peer
    }

    fn event(&self) -> serde_json::Value {
        serde_json::from_str(
            &self
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("TLS peer event deadline"),
        )
        .unwrap()
    }

    fn finish(&mut self) -> serde_json::Value {
        let result = self.event();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                assert!(status.success(), "TLS peer failed: {status}");
                return result;
            }
            assert!(Instant::now() < deadline, "TLS peer exit deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tls_and_mtls_verify_peer_identity_before_report_delivery() {
    let certificates = Certificates::new();
    // All negative cases use syntactically valid inputs and construct a client;
    // only the real handshake exposes the trust/identity failure.
    for version in ["TLSv1_2", "TLSv1_3"] {
        for (server, trust, client, mtls, accepted) in [
            ("server", Some("ca"), None, false, true),
            ("server", Some("ca"), Some("client"), true, true),
            ("server", None, None, false, false),
            ("server", Some("untrusted"), None, false, false),
            ("wrong-host", Some("ca"), None, false, false),
            ("expired", Some("ca"), None, false, false),
            ("server", Some("ca"), None, true, false),
            ("server", Some("ca"), Some("wrong-client"), true, false),
        ] {
            let mut peer = Peer::start(&certificates.0, server, mtls, version, "report");
            let config = ClientConfig {
                endpoint: format!("{}/api/v2/host-monitor/report", peer.origin),
                tls_ca_pem: trust.map(|name| certificates.0.join(format!("{name}.crt"))),
                tls_identity_pem: client.map(|name| certificates.0.join(format!("{name}.pem"))),
                request_timeout_seconds: 3,
                ..ClientConfig::default()
            };
            config.validate_for_diagnostics().unwrap();
            validate_local_tls(&config).unwrap();
            let report = super::tests::report();
            let mut reporter = Reporter::with_client_and_credential(
                &config,
                build_client(&config).unwrap(),
                CredentialSnapshot {
                    identity: crate::client_identity::for_instance(&report.host.id).unwrap(),
                    revision: (Uuid::new_v4(), Uuid::new_v4()),
                    secret: Arc::new(SecretString::new("tls-fixture-secret-marker".into())),
                },
            )
            .unwrap();
            reporter.network_policy_override = Some(NetworkPolicy::PrivateDevice {
                allow_loopback: true,
                allow_link_local: false,
            });
            let result = reporter.send_host_monitoring(&report).await;
            assert_eq!(
                result.is_ok(),
                accepted,
                "case {version}/{server}/{trust:?}/{client:?}/{mtls}: {result:?}"
            );
            if let Err(error) = result {
                assert!(
                    matches!(error, SendError::Transient(_)),
                    "TLS failures must retain unacknowledged reports"
                );
                assert!(!format!("{error}/{error:?}").contains("tls-fixture-secret-marker"));
                assert!(!format!("{error}/{error:?}").contains(&peer.origin));
            }
            let observed = peer.finish();
            assert_eq!(observed["http"], accepted, "{observed}");
            if accepted {
                assert_eq!(observed["handshake"], true);
                assert_eq!(observed["client_authenticated"], mtls);
                assert_eq!(observed["report_id"], report.report_id);
                assert_eq!(observed["host_id"], report.host.id);
                assert_eq!(observed["version"], version.replace('_', "."));
            } else {
                assert_eq!(observed["tls_rejected"], true, "{observed}");
            }
        }
    }
}

#[cfg(feature = "otlp")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otlp_uses_the_same_verified_mtls_transport() {
    let certificates = Certificates::new();
    for identity in [Some("client"), None] {
        let mut peer = Peer::start(&certificates.0, "server", true, "TLSv1_3", "otlp");
        let config = ClientConfig {
            otlp_endpoint: Some(format!("{}/v1/metrics", peer.origin)),
            otlp_token: Some(Arc::new(SecretString::new(
                "tls-fixture-secret-marker".into(),
            ))),
            tls_ca_pem: Some(certificates.0.join("ca.crt")),
            tls_identity_pem: identity.map(|name| certificates.0.join(format!("{name}.pem"))),
            request_timeout_seconds: 3,
            ..ClientConfig::default()
        };
        let report = super::tests::report();
        let reporter = Reporter::with_client_and_credential(
            &config,
            build_client(&config).unwrap(),
            CredentialSnapshot {
                identity: crate::client_identity::for_instance(&report.host.id).unwrap(),
                revision: (Uuid::new_v4(), Uuid::new_v4()),
                secret: Arc::new(SecretString::new("unused-report-token".into())),
            },
        )
        .unwrap();
        let result = reporter.send_otlp(&report).await;
        assert_eq!(result.is_ok(), identity.is_some());
        if let Err(error) = result {
            assert!(!format!("{error:#}/{error:?}").contains("tls-fixture-secret-marker"));
        }
        let observed = peer.finish();
        assert_eq!(observed["http"], identity.is_some());
        assert_eq!(observed["client_authenticated"], identity.is_some());
    }
}
