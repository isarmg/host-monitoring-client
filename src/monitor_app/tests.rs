use super::*;

#[cfg(unix)]
const SIGTERM_READY_MARKER: &str = "host-monitoring-sigterm-listener-ready";
#[cfg(unix)]
const SIGTERM_HELPER_ENV: &str = "HOST_MONITORING_SIGTERM_TEST_HELPER";

#[cfg(unix)]
#[tokio::test]
#[ignore = "subprocess helper invoked only by process_sigterm_survives_an_unobserved_window"]
async fn process_sigterm_sticky_subprocess_helper() {
    use std::io::Write as _;

    if std::env::var(SIGTERM_HELPER_ENV).as_deref() != Ok("1") {
        return;
    }
    let shutdown = install_process_shutdown_signal().unwrap();
    println!("{SIGTERM_READY_MARKER}");
    std::io::stdout().flush().unwrap();

    // Do not create a cancellation future until the real process signal reaches the background
    // listener. Merely checking the atomic flag cannot consume the sticky notification.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shutdown.is_requested() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("isolated helper did not receive SIGTERM");
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(1), shutdown.cancelled())
        .await
        .expect("SIGTERM received during the gap must remain observable");
}

#[cfg(unix)]
#[test]
fn process_sigterm_survives_an_unobserved_window() {
    use std::{
        io::{BufRead as _, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        thread,
    };

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "monitor_app::tests::process_sigterm_sticky_subprocess_helper",
            "--ignored",
            "--nocapture",
        ])
        .env(SIGTERM_HELPER_ENV, "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn isolated SIGTERM helper");
    let stdout = child.stdout.take().expect("capture helper stdout");
    let (ready_sender, ready_receiver) = mpsc::channel();
    let output_reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) if line.contains(SIGTERM_READY_MARKER) => {
                    let _ = ready_sender.send(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    ready_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("helper did not register signal listeners in time");

    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(result, 0, "send SIGTERM to isolated helper");

    let exit_deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll isolated helper") {
            break status;
        }
        if Instant::now() >= exit_deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated helper did not observe SIGTERM in time");
        }
        thread::sleep(Duration::from_millis(10));
    };
    output_reader.join().expect("join helper output reader");
    assert!(status.success(), "isolated helper failed after SIGTERM");
}

#[test]
fn pairing_activation_loads_the_server_assigned_identity() {
    let directory = std::env::temp_dir()
        .canonicalize()
        .expect("physical test temporary directory")
        .join(format!("host-monitoring-active-host-{}", Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let instance_id = Uuid::new_v4();
    write_private_fixture(directory.join("host-id"), instance_id.to_string()).unwrap();
    let stale_id = Uuid::new_v4();
    let mut config = ClientConfig::default();
    config.state_dir = directory.clone();
    config.config_path = Some(directory.join("config.json"));
    let mut host = host_monitor::HostIdentity {
        id: stale_id.to_string(),
        os: "test".into(),
        os_version: None,
        kernel_version: None,
        arch: "test".into(),
        client_version: "test".into(),
    };

    let generation = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    write_private_fixture(directory.join("client-token"), "paired-token").unwrap();
    write_private_fixture(directory.join("host-id"), instance_id.to_string()).unwrap();
    write_private_fixture(
        directory.join("auth-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": "0.9.4",
            "status": "authorized",
            "reason": "current pairing completed",
            "changed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .unwrap();
    write_private_fixture(
        directory.join("pairing-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "phase": "active",
            "version": "0.9.4",
            "generation": generation,
            "request_id": request_id,
            "activation_url": "https://host-monitoring.example/activate/test",
            "instance_id": instance_id,
            "report_endpoint": "https://host-monitoring.example/api/v2/host-monitor/report",
            "completed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .unwrap();
    write_private_fixture(
        directory.join("active-binding.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": "0.9.4",
            "generation": generation,
            "request_id": request_id,
            "instance_id": instance_id,
            "report_endpoint": "https://host-monitoring.example/api/v2/host-monitor/report"
        }))
        .unwrap(),
    )
    .unwrap();

    let _reporter = pairing::activate_reporter_snapshot(
        &mut config,
        &mut host,
        generation,
        request_id,
        instance_id,
        "https://host-monitoring.example/api/v2/host-monitor/report",
    )
    .unwrap();

    assert_eq!(host.id, instance_id.to_string());
    assert_eq!(
        config.endpoint,
        "https://host-monitoring.example/api/v2/host-monitor/report"
    );
    fs::remove_dir_all(directory).unwrap();
}

fn write_private_fixture(
    path: impl AsRef<std::path::Path>,
    bytes: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
