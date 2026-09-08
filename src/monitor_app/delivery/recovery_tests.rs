use super::*;
use sarmg_client_runtime::{ClientDeliveryDriver, RecoveryUpdate};
use std::os::unix::fs::PermissionsExt;

struct Fixture(ClientConfig);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-recovery-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = ClientConfig::default();
        config.state_dir = path;
        config.endpoint = "http://127.0.0.1:9/api/v2/host-monitor/report".into();
        config.request_timeout_seconds = 3;
        Self(config)
    }
    fn journal(&self, value: serde_json::Value) {
        let path = self.0.state_dir.join("pairing-state.json");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    async fn activate(&self, endpoint: &str, token: &str) -> PairingProgress {
        self.journal(serde_json::json!({
            "phase": "activating", "version": "0.9.4",
            "generation": Uuid::new_v4(), "request_id": Uuid::new_v4(),
            "activation_url": "http://127.0.0.1:9/activate/fixture",
            "expires_at": chrono::Utc::now() + chrono::TimeDelta::minutes(10),
            "poll_interval": 2, "instance_id": Uuid::new_v4(),
            "pairing_endpoint": self.0.pairing_endpoint(),
            "report_endpoint": endpoint, "bearer_secret": token,
        }));
        pairing::poll_existing(&self.0).await.unwrap().unwrap()
    }
    fn pending(&self, origin: &str) {
        self.journal(serde_json::json!({
            "phase": "pending", "version": "0.9.4",
            "generation": Uuid::new_v4(), "request_id": Uuid::new_v4(),
            "activation_url": format!("{origin}/activate/fixture"),
            "expires_at": chrono::Utc::now() + chrono::TimeDelta::minutes(10),
            "poll_interval": 2,
            "pairing_endpoint": format!("{origin}/api/v2/host-monitor/pairing-requests"),
            "report_endpoint": format!("{origin}/api/v2/host-monitor/report"),
            "bearer_secret": "c".repeat(64), "polling_secret": "p".repeat(64),
        }));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0.state_dir);
    }
}

#[tokio::test]
async fn rotated_identity_isolates_original_bytes_then_delivers_the_current_instance() {
    let mut fixture = Fixture::new();
    let trap = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    trap.set_nonblocking(true).unwrap();
    fixture.0.endpoint = format!(
        "http://{}/api/v2/host-monitor/report",
        trap.local_addr().unwrap()
    );
    fixture.0.otlp_endpoint = Some(format!("http://{}/v1/metrics", trap.local_addr().unwrap()));
    fixture.activate(&fixture.0.endpoint, &"a".repeat(64)).await;
    let old_reporter = Reporter::new(&fixture.0).unwrap();
    let old_host = load_host_identity(&fixture.0.state_dir).unwrap();
    let report = SystemSampler::new().collect(old_host.clone(), 10, 0);
    let spool = Spool::open(&fixture.0.state_dir, 1024 * 1024).unwrap();
    spool.enqueue(&report).unwrap();
    let source = fs::read_dir(fixture.0.state_dir.join("spool"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "record"))
        .unwrap();
    let original = fs::read(&source).unwrap();
    let old_id = old_host.id.clone();
    let (origin, server) = http_once("202 Accepted", move |headers, body| {
        assert!(
            headers
                .to_lowercase()
                .contains(&format!("authorization: bearer {}", "b".repeat(64)))
        );
        let sent: ClientReport = serde_json::from_slice(body).unwrap();
        assert_ne!(sent.host.id, old_id);
        serde_json::json!({"accepted": true, "host_id": sent.host.id, "report_id": sent.report_id, "received_at": chrono::Utc::now()})
    });
    fixture.0.endpoint = format!("{origin}/api/v2/host-monitor/report");
    fixture.activate(&fixture.0.endpoint, &"b".repeat(64)).await;
    let current = Reporter::new(&fixture.0).unwrap();
    assert_ne!(current.identity(), old_reporter.identity());
    assert_eq!(old_reporter.identity().instance_id(), old_host.id);
    let current_host = load_host_identity(&fixture.0.state_dir).unwrap();
    let next = SystemSampler::new().collect(current_host, 10, 1);
    spool.enqueue(&next).unwrap();
    let snapshot_state = || {
        [
            "host-id",
            "client-token",
            "auth-state.json",
            "pairing-state.json",
            "active-binding.json",
        ]
        .map(|name| fs::read(fixture.0.state_dir.join(name)).unwrap())
    };
    let before = snapshot_state();
    let error = current.send_host_monitoring(&report).await.unwrap_err();
    assert!(matches!(
        error,
        host_monitor::transport::SendError::IdentityMismatch
    ));
    assert!(!error.is_permanent() && !error.is_unauthorized());
    #[cfg(feature = "otlp")]
    assert!(current.send_otlp(&report).await.is_err());
    // A failed durable isolation cannot be treated as an ACK or advance to the
    // next report. The conflicting evidence and original source both survive.
    let collision = source.with_extension("identity");
    fs::write(&collision, "preexisting isolation evidence").unwrap();
    assert!(flush_spool(&spool, &current, None).await.is_err());
    assert_eq!(spool.pending_count().unwrap(), 2);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        fs::read(&collision).unwrap(),
        b"preexisting isolation evidence"
    );
    assert_eq!(snapshot_state(), before);
    fs::remove_file(&collision).unwrap();
    assert!(matches!(
        flush_spool(&spool, &current, None).await.unwrap(),
        FlushOutcome::Drained
    ));
    server.join().unwrap();
    assert_eq!(spool.pending_count().unwrap(), 0);
    assert_eq!(spool.health().unwrap().identity_mismatch_entries, 1);
    assert_eq!(
        fs::read(source.with_extension("identity")).unwrap(),
        original
    );
    assert!(!source.exists());
    assert!(matches!(trap.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    assert_eq!(snapshot_state(), before);
    assert_eq!(
        Reporter::new(&fixture.0).unwrap().identity(),
        current.identity()
    );
}

fn http_once(
    status: &'static str,
    respond: impl FnOnce(&str, &[u8]) -> serde_json::Value + Send + 'static,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        // macOS inherits the listener's nonblocking mode on accepted sockets.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let body_start = loop {
            let mut chunk = [0; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0 && request.len() + count <= 1024 * 1024);
            request.extend_from_slice(&chunk[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let length: usize = std::str::from_utf8(&request[..end])
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break end + 4;
                }
            }
        };
        let body = respond(
            std::str::from_utf8(&request[..body_start]).unwrap(),
            &request[body_start..],
        );
        let body = serde_json::to_vec(&body).unwrap();
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
    });
    (origin, server)
}

#[tokio::test]
async fn missed_active_recovers_bound_reporter_host_and_endpoint_while_next_pairing_waits() {
    let fixture = Fixture::new();
    let first_progress = fixture.activate(&fixture.0.endpoint, &"a".repeat(64)).await;
    let first_reporter = Reporter::new(&fixture.0).unwrap();
    let first_host = load_host_identity(&fixture.0.state_dir).unwrap();
    let spool = Spool::open(&fixture.0.state_dir, 1024 * 1024).unwrap();
    let (host_updates, host_receiver) = watch::channel(first_host.clone());
    let mut config = fixture.0.clone();
    config.otlp_endpoint = Some("http://127.0.0.1:9/v1/metrics".into());
    let mut driver = HostDeliveryDriver::new(
        config,
        first_host,
        spool.clone(),
        first_reporter,
        host_updates,
    );
    let old_otlp = driver.otlp_queue.as_ref().unwrap().clone();

    let (expected_host_sender, expected_host_receiver) = std::sync::mpsc::channel::<Uuid>();
    let (report_origin, report_server) = http_once("202 Accepted", move |headers, body| {
        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {}", "b".repeat(64)))
        );
        let report: serde_json::Value = serde_json::from_slice(body).unwrap();
        let expected_host = expected_host_receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert_eq!(report["host"]["id"], expected_host.to_string());
        serde_json::json!({ "host_id": report["host"]["id"], "report_id": report["report_id"],
            "accepted": true, "received_at": chrono::Utc::now() })
    });
    let endpoint = format!("{report_origin}/api/v2/host-monitor/report");
    let second = fixture.activate(&endpoint, &"b".repeat(64)).await;
    let PairingProgress::Active {
        generation,
        request_id,
        instance_id,
        ..
    } = second
    else {
        panic!("expected Active");
    };
    expected_host_sender.send(instance_id).unwrap();
    let (pairing_origin, pairing_server) = http_once("200 OK", |headers, _| {
        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: pairing {}", "p".repeat(64)))
        );
        serde_json::json!({ "status": "waiting" })
    });
    fixture.pending(&pairing_origin);
    let journal = fs::read(fixture.0.state_dir.join("pairing-state.json")).unwrap();
    let probe = driver.recover().await.unwrap();
    assert!(matches!(probe, HostRecoveryProbe::Credential(_)));
    assert!(
        matches!(driver.apply_recovery(probe).unwrap(), RecoveryUpdate::Renewed { poll_after } if poll_after == Duration::from_secs(2))
    );
    assert_eq!(
        driver.reporter.credential_revision(),
        (generation, request_id)
    );
    assert_eq!(driver.host.id, instance_id.to_string());
    assert_eq!(host_receiver.borrow().id, instance_id.to_string());
    assert_eq!(driver.config.endpoint, endpoint);
    assert!(!std::sync::Arc::ptr_eq(
        &old_otlp,
        driver.otlp_queue.as_ref().unwrap()
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !old_otlp.worker.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let waiting = driver.recover().await.unwrap();
    assert!(matches!(
        waiting,
        HostRecoveryProbe::Pairing(Some(PairingProgress::Waiting(_)))
    ));
    assert!(
        matches!(driver.apply_recovery(waiting).unwrap(), RecoveryUpdate::Unchanged { poll_after } if poll_after == Duration::from_secs(2))
    );
    // Even an old Active probe cannot roll the fully bound snapshot back.
    assert!(matches!(
        driver
            .apply_recovery(HostRecoveryProbe::Pairing(Some(first_progress)))
            .unwrap(),
        RecoveryUpdate::Unchanged { .. }
    ));
    assert_eq!(
        driver.reporter.credential_revision(),
        (generation, request_id)
    );
    assert_eq!(
        fs::read(fixture.0.state_dir.join("pairing-state.json")).unwrap(),
        journal
    );
    let report = SystemSampler::new().collect(host_receiver.borrow().clone(), 10, 0);
    assert_eq!(report.host.id, instance_id.to_string());
    spool.enqueue(&report).unwrap();
    assert!(matches!(
        driver.batch().await.unwrap(),
        FlushOutcome::Drained
    ));
    assert_eq!(spool.pending_count().unwrap(), 0);
    drop(driver);
    pairing_server.join().unwrap();
    report_server.join().unwrap();
}

#[tokio::test]
async fn startup_uses_bound_identity_and_endpoint_during_an_incomplete_replacement() {
    let fixture = Fixture::new();
    fixture.activate(&fixture.0.endpoint, &"a".repeat(64)).await;
    let mut old_host = load_host_identity(&fixture.0.state_dir).unwrap();
    let endpoint = "https://second.example/api/v2/host-monitor/report";
    let second = fixture.activate(endpoint, &"b".repeat(64)).await;
    let PairingProgress::Active {
        generation,
        request_id,
        instance_id,
        ..
    } = second
    else {
        panic!("expected Active");
    };
    fixture.pending("http://127.0.0.1:9");
    let mut config = fixture.0.clone();
    let (_sender, shutdown) = shutdown_channel();
    let reporter = prepare_reporter(&mut config, &mut old_host, ClientCommand::Run, &shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reporter.credential_revision(), (generation, request_id));
    assert_eq!(old_host.id, instance_id.to_string());
    assert_eq!(config.endpoint, endpoint);
}
