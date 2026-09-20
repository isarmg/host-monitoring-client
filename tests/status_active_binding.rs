use std::{collections::BTreeMap, fs, path::Path, process::Command};

use host_monitor::ClientConfig;
use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    config_path: std::path::PathBuf,
    report_endpoint: String,
}

impl Fixture {
    fn new(mismatched_binding: bool, state_version: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!("host-monitoring-status-binding-{}", Uuid::new_v4()));
        let state_dir = root.join("state");
        let config_path = root.join("config.json");
        fs::create_dir_all(&state_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut config = ClientConfig::default();
        config.endpoint = "https://old.example/api/v2/host-monitor/report".into();
        config.state_dir = state_dir.clone();
        write_private_fixture(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

        let generation = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let report_endpoint = "https://new.example/api/v2/host-monitor/report".to_string();
        write_private_fixture(state_dir.join("host-id"), instance_id.to_string()).unwrap();
        write_private_fixture(state_dir.join("client-token"), "a".repeat(64)).unwrap();
        write_private_fixture(
            state_dir.join("auth-state.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": state_version,
                "status": "authorized",
                "reason": "browser pairing completed",
                "changed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        )
        .unwrap();
        write_private_fixture(
            state_dir.join("pairing-state.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "phase": "active",
                "version": state_version,
                "generation": generation,
                "request_id": request_id,
                "activation_url": "https://new.example/activate/test",
                "instance_id": instance_id,
                "report_endpoint": report_endpoint.clone(),
                "completed_at": chrono::Utc::now(),
            }))
            .unwrap(),
        )
        .unwrap();
        write_private_fixture(
            state_dir.join("active-binding.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": state_version,
                "generation": if mismatched_binding { Uuid::new_v4() } else { generation },
                "request_id": request_id,
                "instance_id": instance_id,
                "report_endpoint": report_endpoint.clone(),
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            root,
            state_dir,
            config_path,
            report_endpoint,
        }
    }

    fn status(&self) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_host-monitor"))
            .args([
                "status",
                "--output",
                "json",
                "--config",
                self.config_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn state_files(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn status_uses_the_active_binding_endpoint_without_mutating_state() {
    let fixture = Fixture::new(false, "0.9.4");
    let before = state_files(&fixture.state_dir);

    let status = fixture.status();

    assert_eq!(status["status"], "configured");
    assert_eq!(
        status["endpoint"].as_str(),
        Some(fixture.report_endpoint.as_str())
    );
    assert_eq!(status["checks"]["active_binding"]["status"], "ok");
    assert_eq!(state_files(&fixture.state_dir), before);
}

#[test]
fn status_fails_closed_on_a_mismatched_binding_without_mutating_state() {
    let fixture = Fixture::new(true, "0.9.4");
    let before = state_files(&fixture.state_dir);

    let status = fixture.status();

    assert_eq!(status["status"], "degraded");
    assert!(status["endpoint"].is_null());
    assert_eq!(status["checks"]["active_binding"]["status"], "error");
    assert_eq!(
        status["checks"]["active_binding"]["code"],
        "pairing_state_incompatible"
    );
    assert!(
        status["next_action"]
            .as_str()
            .unwrap()
            .contains("pair recover")
    );
    assert_eq!(state_files(&fixture.state_dir), before);
}

#[test]
fn status_reads_legacy_093_account_state_without_rewriting_it() {
    let fixture = Fixture::new(false, "0.9.3");
    let before = state_files(&fixture.state_dir);

    let status = fixture.status();

    assert_eq!(status["status"], "configured");
    assert_eq!(status["checks"]["pairing"]["status"], "ok");
    assert_eq!(status["checks"]["authorization"]["status"], "ok");
    assert_eq!(state_files(&fixture.state_dir), before);
}

#[test]
fn pair_status_returns_actionable_error_for_incompatible_account_state() {
    let fixture = Fixture::new(false, "0.8.0");
    let before = state_files(&fixture.state_dir);
    let output = Command::new(env!("CARGO_BIN_EXE_host-monitor"))
        .args([
            "pair",
            "status",
            "--output",
            "json",
            "--config",
            fixture.config_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "pairing_state_incompatible");
    assert!(
        result["error"]["next_step"]
            .as_str()
            .unwrap()
            .contains("pair recover --interactive")
    );
    assert_eq!(state_files(&fixture.state_dir), before);
}

#[cfg(unix)]
#[test]
fn incompatible_spool_blocks_account_recovery_before_any_archive() {
    use std::{io::Write, process::Stdio};

    let fixture = Fixture::new(false, "0.8.0");
    write_private_fixture(
        fixture.state_dir.join("spool"),
        b"important-but-unreadable-spool",
    )
    .unwrap();
    let before = state_files(&fixture.state_dir);
    let mut child = Command::new(env!("CARGO_BIN_EXE_host-monitor"))
        .args([
            "pair",
            "recover",
            "--input-stdin",
            "--non-interactive",
            "--output",
            "json",
            "--config",
            fixture.config_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({
                "server": "https://host-monitoring.example/",
                "authorization_code": "a".repeat(64)
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(10));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "important_state_incompatible");
    assert_eq!(result["error"]["detail"], "artifact=spool;preserved=true");
    let mut after = state_files(&fixture.state_dir);
    after.remove("maintenance.lock");
    assert_eq!(after, before);
    assert!(fs::read_dir(&fixture.state_dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".incompatible-")
    }));
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
