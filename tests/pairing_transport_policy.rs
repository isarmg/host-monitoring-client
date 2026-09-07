use std::{fs, process::Command};

use host_monitor::ClientConfig;
use uuid::Uuid;

struct Fixture {
    root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    config_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-monitor-pairing-transport-policy-{}",
                Uuid::new_v4()
            ));
        fs::create_dir_all(&root).unwrap();
        let state_dir = root.join("state");
        let config_path = root.join("config.json");
        let mut config = ClientConfig::default();
        config.endpoint = "http://192.0.2.10:1/api/v2/host-monitor/report".into();
        config.pairing_endpoint =
            Some("https://192.0.2.10:1/api/v2/host-monitor/pairing-requests".into());
        config.request_timeout_seconds = 1;
        config.state_dir = state_dir.clone();
        fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        Self {
            root,
            state_dir,
            config_path,
        }
    }

    fn pair(&self, arguments: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_host-monitor"));
        command.args(["pair", "--config", self.config_path.to_str().unwrap()]);
        command.args(arguments);
        command.env_remove("HOST_MONITOR_ALLOW_INSECURE_HTTP");
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_remote_http_is_rejected(arguments: &[&str]) {
    let fixture = Fixture::new();
    let output = fixture.pair(arguments);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = if arguments.is_empty() {
        "telemetry endpoint violates Foundation network policy"
    } else {
        "unknown argument: --allow-insecure-http"
    };
    assert!(
        stderr.contains(expected),
        "unexpected pairing error: {stderr}"
    );
    assert!(
        !fixture.state_dir.exists(),
        "pairing persisted state before rejecting its temporary transport policy"
    );
}

#[test]
fn remote_http_is_unconditionally_rejected() {
    assert_remote_http_is_rejected(&[]);
}

#[test]
fn removed_cli_override_is_not_accepted() {
    assert_remote_http_is_rejected(&["--allow-insecure-http"]);
}
