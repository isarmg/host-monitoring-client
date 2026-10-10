use uuid::Uuid;
use xsoc::{
    ClientConfig, ClientHealth, ClientReport, CpuSnapshot, DiskSnapshot, GpuSnapshot, HostIdentity,
    MemorySnapshot, NetworkSnapshot, SystemSnapshot, TemperatureSnapshot, transport::Reporter,
};

fn otlp_test_config(endpoint: String) -> (ClientConfig, std::path::PathBuf, Uuid) {
    let state_dir = std::env::temp_dir()
        .canonicalize()
        .expect("physical test temporary directory")
        .join(format!("xsos-otlp-live-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&state_dir).expect("create OTLP test state directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let instance_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let report_endpoint = "https://xsos.example/api/v1/xsoc/report";
    write_private_fixture(state_dir.join("client-token"), "e".repeat(64))
        .expect("seed paired test credential");
    write_private_fixture(state_dir.join("host-id"), instance_id.to_string())
        .expect("seed paired test identity");
    write_private_fixture(
        state_dir.join("auth-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": "1.0.0",
            "status": "authorized",
            "reason": "browser pairing completed",
            "changed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .expect("seed current authorization state");
    write_private_fixture(
        state_dir.join("pairing-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "phase": "active",
            "version": "1.0.0",
            "generation": generation,
            "request_id": request_id,
            "activation_url": format!(
                "https://xsos.example/activate/{request_id}"
            ),
            "instance_id": instance_id,
            "report_endpoint": report_endpoint,
            "completed_at": chrono::Utc::now()
        }))
        .unwrap(),
    )
    .expect("seed current Active pairing state");
    write_private_fixture(
        state_dir.join("active-binding.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": "1.0.0",
            "generation": generation,
            "request_id": request_id,
            "instance_id": instance_id,
            "report_endpoint": report_endpoint
        }))
        .unwrap(),
    )
    .expect("seed current active binding");
    let mut config = ClientConfig::default();
    config.state_dir = state_dir.clone();
    config.endpoint = report_endpoint.into();
    config.otlp_endpoint = Some(endpoint);
    (config, state_dir, instance_id)
}

#[test]
fn otlp_fixture_satisfies_the_current_active_binding_contract() {
    let (config, state_dir, _) = otlp_test_config("https://127.0.0.1:4318/v1/metrics".into());
    Reporter::new(&config).expect("current OTLP fixture must construct a reporter");
    std::fs::remove_dir_all(state_dir).expect("remove OTLP test state directory");
}

/// CI sets XSOC_TEST_OTLP_ENDPOINT while a real Collector is running.
/// Local test runs skip cleanly so the unit suite has no external dependency.
///
/// Set XSOC_TEST_REQUIRE_OTLP to fail instead of skipping when the environment is prepared for
/// a Collector, preventing silently inactive coverage.
/// Read the Collector endpoint; return None when unconfigured so the caller may skip.
fn otlp_endpoint(test_name: &str) -> Option<String> {
    match std::env::var("XSOC_TEST_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.trim().is_empty() => Some(endpoint),
        _ if std::env::var("XSOC_TEST_REQUIRE_OTLP").is_ok_and(|v| !v.trim().is_empty()) => {
            panic!(
                "XSOC_TEST_REQUIRE_OTLP is set, but XSOC_TEST_OTLP_ENDPOINT \
                 is missing or empty; refusing to skip `{test_name}`"
            );
        }
        _ => {
            eprintln!(
                "Skipped {test_name}: XSOC_TEST_OTLP_ENDPOINT is not set;\
                 the live OTLP encoding path was not verified"
            );
            None
        }
    }
}

#[tokio::test]
async fn collector_accepts_the_client_otlp_protobuf() {
    let Some(endpoint) = otlp_endpoint("collector_accepts_the_client_otlp_protobuf") else {
        return;
    };
    let (config, state_dir, instance_id) = otlp_test_config(endpoint);
    let reporter = Reporter::new(&config).expect("build OTLP test client");
    let report = ClientReport {
        schema_version: xsos_protocol::CLIENT_REPORT_SCHEMA_VERSION,
        report_id: Uuid::new_v4().to_string(),
        collected_at: chrono::Utc::now(),
        host: HostIdentity {
            id: instance_id.to_string(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
        interval_seconds: 10.0,
        system: SystemSnapshot {
            hardware: None,
            uptime_seconds: 60,
            cpu: CpuSnapshot {
                usage_percent: 25.0,
                logical_count: 4,
                physical_count: Some(2),
                per_core_percent: vec![10.0, 20.0, 30.0, 40.0],
            },
            memory: MemorySnapshot {
                total_bytes: 16 * 1024 * 1024,
                used_bytes: 8 * 1024 * 1024,
                available_bytes: 8 * 1024 * 1024,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            networks: Vec::new(),
            disks: Vec::new(),
            temperatures: Vec::new(),
            gpus: Vec::new(),
        },
        capabilities: Vec::new(),
        client: ClientHealth {
            spool_pending_batches: 0,
            collector_errors: 0,
        },
    };
    reporter
        .send_otlp(&report)
        .await
        .expect("Collector must accept the Client's gzip OTLP protobuf");
    std::fs::remove_dir_all(state_dir).expect("remove OTLP test state directory");
}

/// Fully populated report with nonempty interface, disk, sensor and GPU lists.
///
/// # Why this requires a separate case
///
/// The preceding case leaves `networks` / `disks` / `temperatures` / `gpus` empty and therefore
/// verifies only four CPU, memory and uptime metrics. Most handwritten field numbers in `otlp.rs`
/// belong to device metrics outside that set. The CI job intended to use a real Collector
/// to verify handwritten protobuf would otherwise cover only that small basic subset.
///
/// Add a multi-device report and verify name-based `MetricSet` consolidation: two interfaces must produce
/// two points under one metric rather than two identically named metrics, which violate the OTLP data model
/// and may be rejected or interpreted ambiguously by the Collector.
#[tokio::test]
async fn collector_accepts_a_fully_populated_report_with_every_device_type() {
    let Some(endpoint) =
        otlp_endpoint("collector_accepts_a_fully_populated_report_with_every_device_type")
    else {
        return;
    };
    let (config, state_dir, instance_id) = otlp_test_config(endpoint);
    let reporter = Reporter::new(&config).expect("build OTLP test client");

    let network = |name: &str, rx: u32, tx: u32| NetworkSnapshot {
        name: name.into(),
        received_bytes_total: u64::from(rx) * 100,
        transmitted_bytes_total: u64::from(tx) * 100,
        received_bytes_per_second: f64::from(rx),
        transmitted_bytes_per_second: f64::from(tx),
        packets_received_total: 10,
        packets_transmitted_total: 20,
        receive_errors_total: 0,
        transmit_errors_total: 1,
    };
    let disk = |name: &str, mount: &str| DiskSnapshot {
        name: name.into(),
        mount_point: mount.into(),
        file_system: "ext4".into(),
        total_bytes: 1024 * 1024 * 1024,
        available_bytes: 512 * 1024 * 1024,
        read_bytes_total: 4096,
        written_bytes_total: 8192,
        read_bytes_per_second: 128.0,
        written_bytes_per_second: 256.0,
        is_read_only: false,
    };
    let sensor = |id: &str, label: &str, celsius: f64| TemperatureSnapshot {
        id: id.into(),
        label: label.into(),
        celsius: Some(celsius),
        max_celsius: Some(95.0),
        critical_celsius: Some(100.0),
        source: "linux-hwmon".into(),
    };

    let report = ClientReport {
        schema_version: xsos_protocol::CLIENT_REPORT_SCHEMA_VERSION,
        report_id: Uuid::new_v4().to_string(),
        collected_at: chrono::Utc::now(),
        host: HostIdentity {
            id: instance_id.to_string(),
            os: "linux".into(),
            os_version: Some("6.1.0".into()),
            kernel_version: Some("6.1.0-generic".into()),
            arch: "x86_64".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
        interval_seconds: 10.0,
        system: SystemSnapshot {
            hardware: None,
            uptime_seconds: 86_400,
            cpu: CpuSnapshot {
                usage_percent: 37.5,
                logical_count: 8,
                physical_count: Some(4),
                per_core_percent: vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 20.0],
            },
            memory: MemorySnapshot {
                total_bytes: 32 * 1024 * 1024 * 1024,
                used_bytes: 12 * 1024 * 1024 * 1024,
                available_bytes: 20 * 1024 * 1024 * 1024,
                swap_total_bytes: 4 * 1024 * 1024 * 1024,
                swap_used_bytes: 1024 * 1024 * 1024,
            },
            // Two interfaces must produce two data points under one metric.
            networks: vec![network("eth0", 1000, 2000), network("wlan0", 300, 400)],
            disks: vec![disk("sda1", "/"), disk("sdb1", "/data")],
            temperatures: vec![
                sensor("coretemp:0", "Package id 0", 55.0),
                sensor("coretemp:1", "Core 1", 51.5),
            ],
            gpus: vec![GpuSnapshot {
                id: "GPU-00000000-0000-0000-0000-000000000003".into(),
                vendor: "nvidia".into(),
                name: "NVIDIA Test GPU".into(),
                utilization_percent: Some(64.0),
                memory_total_bytes: Some(8 * 1024 * 1024 * 1024),
                memory_used_bytes: Some(2 * 1024 * 1024 * 1024),
                temperature_celsius: Some(72.0),
                power_watts: Some(180.5),
                core_clock_mhz: Some(1800.0),
                memory_clock_mhz: Some(7000.0),
                pcie_rx_bytes_per_second: Some(1024.0 * 1024.0),
                pcie_tx_bytes_per_second: Some(2.0 * 1024.0 * 1024.0),
                source: "nvml".into(),
            }],
        },
        capabilities: Vec::new(),
        client: ClientHealth {
            spool_pending_batches: 3,
            collector_errors: 1,
        },
    };

    reporter
        .send_otlp(&report)
        .await
        .expect("Collector must accept a fully populated report: this path exercises interface/disk/sensor/GPU field numbers");
    std::fs::remove_dir_all(state_dir).expect("remove OTLP test state directory");
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
