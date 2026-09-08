use std::{fs, path::Path, process::Command};

use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-monitor-explicit-config-integration-{}",
                Uuid::new_v4()
            ));
        fs::create_dir_all(&root).unwrap();
        let state_dir = root.join("state");
        Self { root, state_dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_host-monitor"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HOST_MONITOR_") {
                command.env_remove(name);
            }
        }
        command
            .env("HOST_MONITOR_STATE_DIR", &self.state_dir)
            .env_remove("HOST_MONITOR_CONFIG");
        command
    }
}

#[cfg(unix)]
fn bounded_output(mut command: Command) -> std::process::Output {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("read-only diagnostic hung on local inputs");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn local_tree_snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>, Vec<u64>)> {
    use std::os::unix::fs::MetadataExt;
    let mut result = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let bytes = if meta.is_file() {
            fs::read(&path).unwrap()
        } else if meta.is_symlink() {
            fs::read_link(&path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        if meta.is_dir() {
            result.extend(local_tree_snapshot(&path));
        }
        result.push((
            path,
            bytes,
            vec![
                meta.ino(),
                meta.len(),
                u64::from(meta.mode()),
                u64::from(meta.uid()),
                u64::from(meta.gid()),
                meta.nlink(),
                meta.mtime() as u64,
                meta.mtime_nsec() as u64,
            ],
        ));
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[cfg(unix)]
#[test]
fn identity_quarantine_is_reported_by_read_only_cli_without_disclosing_or_modifying_evidence() {
    use sarmg_client_runtime::{BoundedBytes, ContractId, QuarantineReason, Spool, SpoolLimits};
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::create_dir(&fixture.state_dir).unwrap();
    fs::set_permissions(&fixture.state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let mut config = host_monitor::ClientConfig::default();
    config.state_dir = fixture.state_dir.clone();
    config.endpoint = format!(
        "https://{}/api/v2/host-monitor/report",
        trap.local_addr().unwrap()
    );
    let config_path = fixture.root.join("config.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let spool = Spool::open(
        fixture.state_dir.join("spool"),
        SpoolLimits {
            max_record_bytes: 1024,
            max_entries: 8,
            max_bytes: 8192,
        },
    )
    .unwrap();
    let id = spool
        .enqueue(
            ContractId::new("host-monitoring.client-report.current").unwrap(),
            1,
            BoundedBytes::new(b"private-quarantine-payload-marker".to_vec(), 1024).unwrap(),
        )
        .unwrap();
    spool
        .quarantine(&id, QuarantineReason::IdentityMismatch)
        .unwrap();
    let before = local_tree_snapshot(&fixture.root);
    for name in ["status", "doctor"] {
        let mut command = fixture.command();
        command
            .args([name, "--output", "json", "--config"])
            .arg(&config_path);
        let output = bounded_output(command);
        assert_eq!(output.status.success(), name == "status");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("private-quarantine-payload-marker"));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let check = if name == "status" {
            assert_eq!(value["spool_identity_mismatch_batches"], 1);
            &value["checks"]["spool"]
        } else {
            value["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["id"] == "spool")
                .unwrap()
        };
        assert_eq!(check["code"], "spool_identity_mismatch");
        assert_eq!(check["status"], "error");
        assert_eq!(local_tree_snapshot(&fixture.root), before);
    }
    assert!(matches!(trap.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
}

#[cfg(target_os = "linux")]
#[test]
fn tls_diagnostics_use_real_client_inputs_without_network_writes_or_secret_output() {
    use std::{
        net::TcpListener,
        os::unix::fs::{PermissionsExt, symlink},
        sync::Arc,
    };
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let key = fixture.root.join("key.pem");
    let ca = fixture.root.join("ca.pem");
    let identity = fixture.root.join("identity.pem");
    let config_path = fixture.root.join("config.json");
    let generated = Command::new("openssl")
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
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let mut pem = fs::read(&ca).unwrap();
    pem.extend_from_slice(&fs::read(&key).unwrap());
    fs::write(&identity, &pem).unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(&ca, fs::Permissions::from_mode(0o644)).unwrap();
    let mut config = host_monitor::ClientConfig::default();
    config.state_dir = fixture.state_dir.clone();
    config.endpoint = format!(
        "http://{}/api/v2/host-monitor/report",
        listener.local_addr().unwrap()
    );
    if cfg!(feature = "otlp") {
        config.otlp_endpoint = Some(format!(
            "http://{}/v1/metrics",
            listener.local_addr().unwrap()
        ));
        config.otlp_token = Some(Arc::new(sarmg_client_secret::SecretString::new(
            "otlp-secret-marker".into(),
        )));
    }

    let check = |config: &host_monitor::ClientConfig, valid: bool| {
        fs::write(&config_path, serde_json::to_vec(config).unwrap()).unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = local_tree_snapshot(&fixture.root);
        for command_name in ["status", "doctor"] {
            let mut command = fixture.command();
            command
                .args([command_name, "--output", "json", "--config"])
                .arg(&config_path);
            let output = bounded_output(command);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            for secret in [
                "otlp-secret-marker",
                "password-secret-marker",
                "invalid-secret-marker",
                "-----BEGIN PRIVATE KEY-----",
            ] {
                assert!(
                    !stdout.contains(secret) && !stderr.contains(secret),
                    "secret in diagnostic output"
                );
            }
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|error| panic!("{command_name}: {error}: {stdout}/{stderr}"));
            let tls = if command_name == "status" {
                &value["checks"]["tls"]
            } else {
                value["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|check| check["id"] == "tls")
                    .unwrap()
            };
            assert_eq!(tls["status"], if valid { "ok" } else { "error" }, "{value}");
            if !valid {
                assert_eq!(tls["code"], "tls_configuration_invalid");
                assert_eq!(
                    value["status"],
                    if command_name == "status" {
                        "degraded"
                    } else {
                        "unhealthy"
                    }
                );
            }
            assert_eq!(
                output.status.success(),
                command_name == "status" || valid,
                "{value}/{stderr}"
            );
            assert!(!fixture.state_dir.exists());
            assert_eq!(local_tree_snapshot(&fixture.root), before);
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    };
    check(&config, true);
    config.tls_identity_pem = Some(identity.clone());
    config.tls_ca_pem = Some(ca.clone());
    check(&config, true);
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o644)).unwrap();
    check(&config, false);
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&identity, b"invalid-secret-marker").unwrap();
    check(&config, false);
    fs::write(&identity, &pem).unwrap();
    let alias = fixture.root.join("alias.pem");
    symlink(&identity, &alias).unwrap();
    config.tls_identity_pem = Some(alias);
    check(&config, false);
    config.tls_identity_pem = None;
    fs::write(&ca, b"invalid-secret-marker").unwrap();
    check(&config, false);
    fs::write(&ca, []).unwrap();
    check(&config, false);
    fs::OpenOptions::new()
        .write(true)
        .open(&ca)
        .unwrap()
        .set_len(sarmg_client_secure_http::MAX_TLS_INPUT_BYTES as u64 + 1)
        .unwrap();
    check(&config, false);
    config.tls_ca_pem = Some(fixture.root.join("missing.pem"));
    check(&config, false);
    let fifo = fixture.root.join("input.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    config.tls_ca_pem = Some(fifo);
    check(&config, false);
    config.tls_ca_pem = None;
    config.tls_identity_password = Some(Arc::new(sarmg_client_secret::SecretString::new(
        "password-secret-marker".into(),
    )));
    check(&config, false);
    config.tls_identity_pkcs12 = Some(identity);
    check(&config, false);
}

#[cfg(unix)]
#[test]
fn credential_diagnostics_are_bounded_read_only_and_reject_unsafe_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new();
    sarmg_client_fs_safety::PrivateDirectory::create(&fixture.state_dir).unwrap();
    let credential = fixture.state_dir.join("client-token");
    let config_path = fixture.root.join("config.json");
    let mut config = host_monitor::ClientConfig::default();
    config.state_dir = fixture.state_dir.clone();
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let check = |expected: &str| {
        let before = local_tree_snapshot(&fixture.root);
        for name in ["status", "doctor"] {
            let mut command = fixture.command();
            command
                .args([name, "--output", "json", "--config"])
                .arg(&config_path);
            let output = bounded_output(command);
            assert!(!String::from_utf8_lossy(&output.stdout).contains("credential-secret-marker"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("credential-secret-marker"));
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let credential = if name == "status" {
                &value["checks"]["credential"]
            } else {
                value["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|check| check["id"] == "credential")
                    .unwrap()
            };
            assert_eq!(credential["status"], expected, "{value}");
            assert_eq!(before, local_tree_snapshot(&fixture.root));
        }
    };
    check("missing");
    fs::write(&credential, "credential-secret-marker").unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    check("ok");
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
    check("error");
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&credential, [0xff]).unwrap();
    check("error");
    fs::write(&credential, " \n").unwrap();
    check("error");
    fs::OpenOptions::new()
        .write(true)
        .open(&credential)
        .unwrap()
        .set_len(4097)
        .unwrap();
    check("error");
    let saved = fixture.state_dir.join("saved-token");
    fs::rename(&credential, &saved).unwrap();
    symlink(&saved, &credential).unwrap();
    check("error");
    fs::remove_file(&credential).unwrap();
    fs::hard_link(&saved, &credential).unwrap();
    check("error");
    fs::remove_file(&credential).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&credential)
            .status()
            .unwrap()
            .success()
    );
    check("error");
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_pair_rejects_config_before_state_changes(config_path: &Path) {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args([
            "pair",
            "--server",
            "http://127.0.0.1:1",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "unsafe_or_corrupt_state");
    assert!(
        !fixture.state_dir.exists(),
        "pairing touched state before rejecting the explicit config"
    );
}

#[test]
fn pair_rejects_missing_and_directory_configs_before_state_changes() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("missing.json");
    let directory = fixture.root.join("directory-config");
    fs::create_dir(&directory).unwrap();

    assert_pair_rejects_config_before_state_changes(&missing);
    assert_pair_rejects_config_before_state_changes(&directory);
}

#[test]
fn status_reports_a_missing_explicit_config_without_creating_state() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("missing.json");
    let output = fixture
        .command()
        .args([
            "status",
            "--output",
            "json",
            "--config",
            missing.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot["checks"]["configuration"]["status"], "error");
    assert_eq!(
        snapshot["checks"]["configuration"]["code"],
        "config_invalid"
    );
    assert!(
        snapshot["checks"]["configuration"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&missing.display().to_string()))
    );
    assert!(!fixture.state_dir.exists());
}

#[cfg(unix)]
#[test]
fn delivery_lock_precedes_bootstrap_and_read_only_commands_remain_concurrent() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let session = sarmg_client_runtime::ClientSession::open(&fixture.state_dir).unwrap();
    let path = fixture.root.join("config.json");
    let mut config = host_monitor::ClientConfig::default();
    config.state_dir = fixture.state_dir.clone();
    config.endpoint = "http://127.0.0.1:9/api/v2/host-monitor/report".into();
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for args in [vec!["run"], vec!["once"], vec!["doctor", "--delivery"]] {
        let output = fixture
            .command()
            .args(args)
            .arg("--config")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(5));
        assert_eq!(result["error"]["code"], "busy");
        assert!(!fixture.state_dir.join(".credential-state.lock").exists());
        assert!(!fixture.state_dir.join("spool").exists());
    }
    for command in ["status", "doctor", "probe"] {
        let output = fixture
            .command()
            .args([command, "--output", "json"])
            .arg("--config")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("exclusive Client delivery session")
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        assert!(!fixture.state_dir.join(".credential-state.lock").exists());
        assert!(!fixture.state_dir.join("spool").exists());
    }
    assert_eq!(fs::read_dir(&fixture.state_dir).unwrap().count(), 2); // instance plus maintenance gate
    drop(session);
    assert!(sarmg_client_runtime::ClientSession::open(&fixture.state_dir).is_ok());
}
