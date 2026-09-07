//! 用**官方** OTLP proto 定义交叉校验手写编码器。
//!
//! # 这个测试补的是哪个缺口
//!
//! `otlp.rs` 里 500 行 protobuf 字段编号是照着 OpenTelemetry spec 手抄的。抄错一个
//! tag 号，编出来的字节流就是错的——但模块内的单测发现不了，因为它们编解码用的是
//! **同一份**定义，抄错的编号在自洽的两侧同样自洽。
//!
//! 唯一能戳破这层自洽的是「独立实现的对端」。只靠 CI 里的真实 Collector 作对端是
//! 不够的：那要求起容器、只能在一个 CI job 里跑，而且它只回一个状态码——
//! 结构对不对得靠人去翻 Collector 的输出。
//!
//! 这里把 `opentelemetry-proto`（官方 proto 生成的类型）作为 **dev-dependency**
//! 引入：运行时依赖一个字节都没变，Client 二进制里不会多出任何东西，但
//! `cargo test` 就能拿到一份权威的解码器，且断言直接落在字段上。
//!
//! 分工：本测试保证**编码正确**，CI 的 otlp job 保证**对端确实接受**。

#![cfg(feature = "otlp")]

use host_monitor::model::*;
use host_monitor::otlp::encode_report;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest as OfficialRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as OfficialValue;
use opentelemetry_proto::tonic::metrics::v1::metric::Data as OfficialData;
use prost::Message;
use uuid::Uuid;

/// 用手写编码器编出字节，再用官方类型解回来。
fn round_trip(report: &ClientReport) -> OfficialRequest {
    let mine = encode_report(report);
    let mut bytes = Vec::with_capacity(mine.encoded_len());
    mine.encode(&mut bytes).expect("手写编码器必须能编码");
    OfficialRequest::decode(bytes.as_slice())
        .expect("官方 OTLP 定义必须能解出手写编码器产生的字节流")
}

fn report() -> ClientReport {
    ClientReport {
        schema_version: 1,
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

/// 资源属性：字段编号抄错的话，这些键值根本解不出来。
#[test]
fn official_definitions_decode_our_resource_attributes() {
    let decoded = round_trip(&report());
    let resource_metrics = decoded
        .resource_metrics
        .first()
        .expect("必须有一个 ResourceMetrics");
    let attributes = &resource_metrics
        .resource
        .as_ref()
        .expect("必须带 Resource")
        .attributes;

    assert_eq!(
        string_attr(attributes, "host.id").as_deref(),
        Some("00000000-0000-4000-8000-0000000000ff")
    );
    assert_eq!(string_attr(attributes, "host.name"), None);
    assert_eq!(
        string_attr(attributes, "service.name").as_deref(),
        Some("host-monitor")
    );
    // OTLP 语义约定要求用 darwin / arm64，而不是我们内部的 macos / aarch64。
    assert_eq!(
        string_attr(attributes, "os.type").as_deref(),
        Some("darwin")
    );
    assert_eq!(
        string_attr(attributes, "host.arch").as_deref(),
        Some("arm64")
    );
}

/// 指标的名称、单位、类型与数据点数量。
#[test]
fn official_definitions_decode_our_metrics() {
    let decoded = round_trip(&report());
    let scope_metrics = &decoded.resource_metrics[0].scope_metrics[0];
    assert_eq!(
        scope_metrics.scope.as_ref().map(|s| s.name.as_str()),
        Some("host-monitor.hostmetrics")
    );

    let find = |name: &str| {
        scope_metrics
            .metrics
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("解码结果里找不到 {name}"))
    };
    let points = |name: &str| match find(name).data.as_ref() {
        Some(OfficialData::Gauge(g)) => g.data_points.len(),
        Some(OfficialData::Sum(s)) => s.data_points.len(),
        other => panic!("{name} 的 data 类型出乎意料：{other:?}"),
    };

    // 单位必须是 OTLP 语义约定里的写法，写错下游图表的量纲就是错的。
    assert_eq!(find("system.cpu.utilization").unit, "1");
    assert_eq!(find("system.memory.usage").unit, "By");
    assert_eq!(find("system.uptime").unit, "s");
    assert_eq!(find("hw.temperature").unit, "Cel");
    assert_eq!(find("hw.gpu.power").unit, "W");

    // 2 网卡 × 收/发 = 4 个点，且必须收敛在**一个** metric 下。
    assert_eq!(points("system.network.io"), 4);
    assert_eq!(points("system.disk.io"), 2);
    assert_eq!(points("hw.temperature"), 1);

    // 累计量必须是 Sum 且单调递增，否则后端算不出速率。
    match find("system.network.io").data.as_ref() {
        Some(OfficialData::Sum(sum)) => {
            assert!(sum.is_monotonic, "累计字节数必须标记为单调");
            // 2 = AGGREGATION_TEMPORALITY_CUMULATIVE
            assert_eq!(sum.aggregation_temporality, 2);
        }
        other => panic!("system.network.io 应当是 Sum，实际为 {other:?}"),
    }

    // 瞬时值必须是 Gauge——错标成 Sum 会让后端把它当累计量做差分。
    assert!(matches!(
        find("system.cpu.utilization").data.as_ref(),
        Some(OfficialData::Gauge(_))
    ));

    // 同一 scope 内 metric 名必须唯一（OTLP 数据模型硬性要求）。
    let mut names: Vec<&str> = scope_metrics
        .metrics
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names, unique, "同一 scope 内出现重复 metric 名：{names:?}");
}

/// 数据点上的属性：多设备全靠它们区分，键名错了下游就无法按设备聚合。
#[test]
fn official_definitions_decode_our_data_point_attributes() {
    let decoded = round_trip(&report());
    let scope_metrics = &decoded.resource_metrics[0].scope_metrics[0];
    let network = scope_metrics
        .metrics
        .iter()
        .find(|m| m.name == "system.network.io")
        .expect("必须有 system.network.io");
    let Some(OfficialData::Sum(sum)) = network.data.as_ref() else {
        panic!("system.network.io 应当是 Sum");
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
