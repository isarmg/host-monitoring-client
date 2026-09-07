use host_monitor::{
    ClientConfig, ClientHealth, ClientReport, CpuSnapshot, DiskSnapshot, GpuSnapshot, HostIdentity,
    MemorySnapshot, NetworkSnapshot, SystemSnapshot, TemperatureSnapshot, transport::Reporter,
};
use uuid::Uuid;

fn otlp_test_config(endpoint: String) -> (ClientConfig, std::path::PathBuf, Uuid) {
    let state_dir = std::env::temp_dir()
        .canonicalize()
        .expect("physical test temporary directory")
        .join(format!("host-monitoring-otlp-live-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&state_dir).expect("create OTLP test state directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let instance_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let report_endpoint = "https://host-monitoring.example/api/v2/host-monitor/report";
    write_private_fixture(state_dir.join("client-token"), "test-only-host-token")
        .expect("seed paired test credential");
    write_private_fixture(state_dir.join("host-id"), instance_id.to_string())
        .expect("seed paired test identity");
    write_private_fixture(
        state_dir.join("auth-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
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
            "version": env!("CARGO_PKG_VERSION"),
            "generation": generation,
            "request_id": request_id,
            "activation_url": format!(
                "https://host-monitoring.example/activate/{request_id}"
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
            "version": env!("CARGO_PKG_VERSION"),
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
    let (config, state_dir, _) = otlp_test_config("http://127.0.0.1:4318/v1/metrics".into());
    Reporter::new(&config).expect("current OTLP fixture must construct a reporter");
    std::fs::remove_dir_all(state_dir).expect("remove OTLP test state directory");
}

/// CI sets HOST_MONITOR_TEST_OTLP_ENDPOINT while a real Collector is running.
/// Local test runs skip cleanly so the unit suite has no external dependency.
///
/// 设置 HOST_MONITOR_TEST_REQUIRE_OTLP 可把"跳过"升级为"失败"，供已备好
/// Collector 的环境使用，避免测试在无人察觉的情况下静默失效。
/// 读取 Collector 端点；未配置时返回 None（调用方跳过）。
fn otlp_endpoint(test_name: &str) -> Option<String> {
    match std::env::var("HOST_MONITOR_TEST_OTLP_ENDPOINT") {
        Ok(endpoint) if !endpoint.trim().is_empty() => Some(endpoint),
        _ if std::env::var("HOST_MONITOR_TEST_REQUIRE_OTLP")
            .is_ok_and(|v| !v.trim().is_empty()) =>
        {
            panic!(
                "HOST_MONITOR_TEST_REQUIRE_OTLP 已设置，但 HOST_MONITOR_TEST_OTLP_ENDPOINT \
                 缺失或为空；拒绝跳过 `{test_name}`"
            );
        }
        _ => {
            eprintln!(
                "⚠  已跳过 {test_name}：未设置 HOST_MONITOR_TEST_OTLP_ENDPOINT，\
                 OTLP 编码路径未经验证"
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
        schema_version: 1,
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

/// 满配报文：网卡、磁盘、传感器、GPU 全部非空。
///
/// # 为什么必须单列一个用例
///
/// 上面那个用例把 `networks` / `disks` / `temperatures` / `gpus` 全部留空，因此它
/// 只验证了 CPU、内存、uptime 四个指标——而 `otlp.rs` 里手写的 500 行字段编号，
/// **绝大部分**恰恰是这四个之外的设备类指标。也就是说，专门为"让真实 Collector
/// 校验手写 protobuf"而设的 CI job，实际只覆盖了最平凡的那一小块。
///
/// 这里补上多设备报文，同时顺带验证 `MetricSet` 的按名收敛：2 张网卡必须收敛成
/// 一个 metric 下的 2 个数据点，而不是两个同名 metric（后者违反 OTLP 数据模型，
/// Collector 会拒绝或产生歧义）。
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
        schema_version: 1,
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
            // 两张网卡 → 同一个 metric 下必须是 2 个数据点。
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
        .expect("Collector must accept a fully populated report: 网卡/磁盘/传感器/GPU 的字段编号都在这条路径上");
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
