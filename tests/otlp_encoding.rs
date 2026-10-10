//! Cross-check the handwritten encoder using official OTLP proto definitions.
//!
//! # The coverage gap addressed by this test
//!
//! The protobuf field numbers in `otlp.rs` are handwritten from the OpenTelemetry specification. One incorrect
//! tag produces an invalid byte stream, which module tests may miss because encoding and decoding share
//! the same definitions and therefore the same incorrect field number.
//!
//! An independently implemented peer exposes this error. Relying solely on a real Collector in CI is
//! insufficient: it requires a container, runs in only one CI job and returns only a status code.
//! Checking the structure would require manual inspection of Collector output.
//!
//! Use `opentelemetry-proto` (types generated from official proto) as a dev-dependency:
//! runtime dependencies and the client binary are unchanged, while
//! `cargo test` obtains an authoritative decoder and makes direct field assertions.
//!
//! This test verifies correct encoding; the CI otlp job verifies actual peer acceptance.

#![cfg(feature = "otlp")]

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest as OfficialRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as OfficialValue;
use opentelemetry_proto::tonic::metrics::v1::metric::Data as OfficialData;
use prost::Message;
use uuid::Uuid;
use xsoc::model::*;
use xsoc::otlp::encode_report;

/// Encode bytes with the handwritten encoder, then decode with official types.
fn round_trip(report: &ClientReport) -> OfficialRequest {
    let mine = encode_report(report);
    let mut bytes = Vec::with_capacity(mine.encoded_len());
    mine.encode(&mut bytes)
        .expect("the handwritten encoder must encode successfully");
    OfficialRequest::decode(bytes.as_slice())
        .expect("official OTLP definitions must decode the handwritten encoder output")
}

fn report() -> ClientReport {
    ClientReport {
        schema_version: xsos_protocol::CLIENT_REPORT_SCHEMA_VERSION,
        report_id: Uuid::new_v4().to_string(),
        collected_at: chrono::Utc::now(),
        interval_seconds: 10.0,
        host: HostIdentity {
            id: Uuid::parse_str("00000000-0000-4000-8000-0000000000ff")
                .unwrap()
                .to_string(),
            os: "macos".into(),
            os_version: Some("15.0".into()),
            kernel_version: Some("24.0.0".into()),
            arch: "aarch64".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
        system: SystemSnapshot {
            hardware: None,
            uptime_seconds: 3600,
            cpu: CpuSnapshot {
                usage_percent: 40.0,
                logical_count: 8,
                physical_count: Some(4),
                per_core_percent: vec![40.0; 8],
            },
            memory: MemorySnapshot {
                total_bytes: 16_000,
                used_bytes: 4_000,
                available_bytes: 12_000,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
            },
            networks: vec![
                NetworkSnapshot {
                    name: "eth0".into(),
                    received_bytes_total: 1_000,
                    transmitted_bytes_total: 2_000,
                    received_bytes_per_second: 10.0,
                    transmitted_bytes_per_second: 20.0,
                    packets_received_total: 1,
                    packets_transmitted_total: 2,
                    receive_errors_total: 0,
                    transmit_errors_total: 0,
                },
                NetworkSnapshot {
                    name: "wlan0".into(),
                    received_bytes_total: 3_000,
                    transmitted_bytes_total: 4_000,
                    received_bytes_per_second: 30.0,
                    transmitted_bytes_per_second: 40.0,
                    packets_received_total: 3,
                    packets_transmitted_total: 4,
                    receive_errors_total: 0,
                    transmit_errors_total: 0,
                },
            ],
            disks: vec![DiskSnapshot {
                name: "sda".into(),
                mount_point: "/".into(),
                file_system: "apfs".into(),
                total_bytes: 1_000,
                available_bytes: 400,
                read_bytes_total: 50,
                written_bytes_total: 60,
                read_bytes_per_second: 5.0,
                written_bytes_per_second: 6.0,
                is_read_only: false,
            }],
            temperatures: vec![TemperatureSnapshot {
                id: "cpu-0".into(),
                label: "CPU".into(),
                celsius: Some(48.5),
                max_celsius: None,
                critical_celsius: Some(100.0),
                source: "smc".into(),
            }],
            gpus: vec![GpuSnapshot {
                id: "gpu-0".into(),
                vendor: "apple".into(),
                name: "Apple GPU".into(),
                utilization_percent: Some(25.0),
                memory_total_bytes: Some(8_000),
                memory_used_bytes: Some(2_000),
                temperature_celsius: Some(55.0),
                power_watts: Some(15.5),
                core_clock_mhz: Some(1_200.0),
                memory_clock_mhz: Some(3_200.0),
                pcie_rx_bytes_per_second: None,
                pcie_tx_bytes_per_second: None,
                source: "iokit".into(),
            }],
        },
        capabilities: vec![],
        client: ClientHealth {
            spool_pending_batches: 0,
            collector_errors: 0,
        },
    }
}

fn string_attr(
    attributes: &[opentelemetry_proto::tonic::common::v1::KeyValue],
    key: &str,
) -> Option<String> {
    attributes.iter().find_map(|kv| {
        if kv.key != key {
            return None;
        }
        match kv.value.as_ref()?.value.as_ref()? {
            OfficialValue::StringValue(value) => Some(value.clone()),
            _ => None,
        }
    })
}

/// Resource attributes: incorrect field numbers would prevent these keys and values from decoding.
#[test]
fn official_definitions_decode_our_resource_attributes() {
    let decoded = round_trip(&report());
    let resource_metrics = decoded
        .resource_metrics
        .first()
        .expect("one ResourceMetrics is required");
    let attributes = &resource_metrics
        .resource
        .as_ref()
        .expect("Resource is required")
        .attributes;

    assert_eq!(
        string_attr(attributes, "host.id").as_deref(),
        Some("00000000-0000-4000-8000-0000000000ff")
    );
    assert_eq!(string_attr(attributes, "host.name"), None);
    assert_eq!(
        string_attr(attributes, "service.name").as_deref(),
        Some("xsoc")
    );
    // OTLP semantic conventions use darwin / arm64 rather than the internal macos / aarch64 names.
    assert_eq!(
        string_attr(attributes, "os.type").as_deref(),
        Some("darwin")
    );
    assert_eq!(
        string_attr(attributes, "host.arch").as_deref(),
        Some("arm64")
    );
}

/// Metric names, units, types and data-point counts.
#[test]
fn official_definitions_decode_our_metrics() {
    let decoded = round_trip(&report());
    let scope_metrics = &decoded.resource_metrics[0].scope_metrics[0];
    assert_eq!(
        scope_metrics.scope.as_ref().map(|s| s.name.as_str()),
        Some("xsoc.hostmetrics")
    );

    let find = |name: &str| {
        scope_metrics
            .metrics
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("decoded output lacks {name}"))
    };
    let points = |name: &str| match find(name).data.as_ref() {
        Some(OfficialData::Gauge(g)) => g.data_points.len(),
        Some(OfficialData::Sum(s)) => s.data_points.len(),
        other => panic!("unexpected data type for {name}: {other:?}"),
    };

    // Units must follow OTLP semantic conventions to preserve downstream dimensions.
    assert_eq!(find("system.cpu.utilization").unit, "1");
    assert_eq!(find("system.memory.usage").unit, "By");
    assert_eq!(find("system.uptime").unit, "s");
    assert_eq!(find("hw.temperature").unit, "Cel");
    assert_eq!(find("hw.gpu.power").unit, "W");

    // 2 interfaces x receive/transmit = 4 points consolidated under one metric.
    assert_eq!(points("system.network.io"), 4);
    assert_eq!(points("system.disk.io"), 2);
    assert_eq!(points("hw.temperature"), 1);

    // Cumulative values must be monotonic Sums so the backend can derive rates.
    match find("system.network.io").data.as_ref() {
        Some(OfficialData::Sum(sum)) => {
            assert!(
                sum.is_monotonic,
                "cumulative byte counts must be marked monotonic"
            );
            // 2 = AGGREGATION_TEMPORALITY_CUMULATIVE
            assert_eq!(sum.aggregation_temporality, 2);
        }
        other => panic!("system.network.io must be Sum; received {other:?}"),
    }

    // Instantaneous values must be Gauges; marking them as Sums makes the backend difference them as cumulative values.
    assert!(matches!(
        find("system.cpu.utilization").data.as_ref(),
        Some(OfficialData::Gauge(_))
    ));

    // Metric names must be unique within a scope, as required by the OTLP data model.
    let mut names: Vec<&str> = scope_metrics
        .metrics
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(
        names, unique,
        "duplicate metric names within one scope: {names:?}"
    );
}

/// Data-point attributes distinguish devices; incorrect keys prevent downstream per-device aggregation.
#[test]
fn official_definitions_decode_our_data_point_attributes() {
    let decoded = round_trip(&report());
    let scope_metrics = &decoded.resource_metrics[0].scope_metrics[0];
    let network = scope_metrics
        .metrics
        .iter()
        .find(|m| m.name == "system.network.io")
        .expect("system.network.io is required");
    let Some(OfficialData::Sum(sum)) = network.data.as_ref() else {
        panic!("system.network.io must be Sum");
    };

    let interfaces: Vec<String> = sum
        .data_points
        .iter()
        .filter_map(|point| string_attr(&point.attributes, "network.interface.name"))
        .collect();
    assert!(interfaces.iter().any(|name| name == "eth0"));
    assert!(interfaces.iter().any(|name| name == "wlan0"));

    let directions: Vec<String> = sum
        .data_points
        .iter()
        .filter_map(|point| string_attr(&point.attributes, "network.io.direction"))
        .collect();
    assert!(directions.iter().any(|d| d == "receive"));
    assert!(directions.iter().any(|d| d == "transmit"));
}

#[test]
fn cumulative_counter_timestamps_survive_jitter_resets_and_spool_replay() {
    use opentelemetry_proto::tonic::metrics::v1::number_data_point::Value;

    let mut first = report();
    first.collected_at = chrono::DateTime::parse_from_rfc3339("2026-10-05T00:00:00.123456789Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let queued = serde_json::to_vec(&first).unwrap();
    let original_bytes = encode_report(&first).encode_to_vec();

    let mut second = first.clone();
    second.collected_at += chrono::Duration::milliseconds(10_675);
    second.system.uptime_seconds += 10;
    second.system.networks[0].received_bytes_total += 100;
    second.system.disks[0].read_bytes_total += 20;

    // A device can reset independently of host uptime (hotplug/driver restart).
    let mut device_reset = second.clone();
    device_reset.collected_at += chrono::Duration::seconds(10);
    device_reset.system.uptime_seconds += 10;
    device_reset.system.networks[0].received_bytes_total = 5;
    device_reset.system.disks[0].read_bytes_total = 2;

    let mut reboot = device_reset.clone();
    reboot.collected_at += chrono::Duration::minutes(10);
    reboot.system.uptime_seconds = 3;
    reboot.system.networks[0].received_bytes_total = 1;
    reboot.system.disks[0].read_bytes_total = 0;

    for sample in [&first, &second, &device_reset, &reboot] {
        let decoded = round_trip(sample);
        for metric in &decoded.resource_metrics[0].scope_metrics[0].metrics {
            let Some(OfficialData::Sum(sum)) = metric.data.as_ref() else {
                continue;
            };
            assert!(sum.is_monotonic);
            assert_eq!(sum.aggregation_temporality, 2);
            for point in &sum.data_points {
                // The true counter start is unknown, not a fabricated boot timestamp.
                assert_eq!(point.start_time_unix_nano, 0);
                assert_eq!(
                    point.time_unix_nano,
                    sample.collected_at.timestamp_nanos_opt().unwrap() as u64
                );
            }
            let expected = match metric.name.as_str() {
                "system.network.io" => sample.system.networks[0].received_bytes_total,
                "system.disk.io" => sample.system.disks[0].read_bytes_total,
                _ => continue,
            };
            // Retain the OS counter, including decreases that let a receiver detect resets.
            assert_eq!(
                sum.data_points[0].value,
                Some(Value::AsDouble(expected as f64))
            );
        }
    }

    // Encoding a report queued before a reboot must use its own capture time and
    // produce the same bytes, irrespective of newer reports or the current OS boot.
    let replayed: ClientReport = serde_json::from_slice(&queued).unwrap();
    assert_eq!(encode_report(&replayed).encode_to_vec(), original_bytes);
}
