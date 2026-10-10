//! Only the OTLP Metrics Protobuf subset needed by the client.
//!
//! Field numbers strictly follow OpenTelemetry proto. Retain the product JSON report as the asset/capability
//! data source; OTLP carries only numeric time-series values.

use prost::{Message, Oneof};

use crate::model::{ClientReport, GpuSnapshot};

#[derive(Clone, PartialEq, Message)]
pub struct ExportMetricsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ResourceMetrics {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_metrics: Vec<ScopeMetrics>,
    #[prost(string, tag = "3")]
    pub schema_url: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
    #[prost(uint32, tag = "2")]
    pub dropped_attributes_count: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AnyValue {
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4")]
    pub value: Option<any_value::Value>,
}

pub mod any_value {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum Value {
        #[prost(string, tag = "1")]
        StringValue(String),
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        #[prost(int64, tag = "3")]
        IntValue(i64),
        #[prost(double, tag = "4")]
        DoubleValue(f64),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct ScopeMetrics {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub metrics: Vec<Metric>,
    #[prost(string, tag = "3")]
    pub schema_url: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct InstrumentationScope {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
    #[prost(message, repeated, tag = "3")]
    pub attributes: Vec<KeyValue>,
    #[prost(uint32, tag = "4")]
    pub dropped_attributes_count: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Metric {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub description: String,
    #[prost(string, tag = "3")]
    pub unit: String,
    #[prost(oneof = "metric::Data", tags = "5, 7")]
    pub data: Option<metric::Data>,
}

pub mod metric {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum Data {
        #[prost(message, tag = "5")]
        Gauge(Gauge),
        #[prost(message, tag = "7")]
        Sum(Sum),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Gauge {
    #[prost(message, repeated, tag = "1")]
    pub data_points: Vec<NumberDataPoint>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Sum {
    #[prost(message, repeated, tag = "1")]
    pub data_points: Vec<NumberDataPoint>,
    #[prost(enumeration = "AggregationTemporality", tag = "2")]
    pub aggregation_temporality: i32,
    #[prost(bool, tag = "3")]
    pub is_monotonic: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct NumberDataPoint {
    #[prost(message, repeated, tag = "7")]
    pub attributes: Vec<KeyValue>,
    #[prost(fixed64, tag = "2")]
    pub start_time_unix_nano: u64,
    #[prost(fixed64, tag = "3")]
    pub time_unix_nano: u64,
    #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
    pub value: Option<number_data_point::Value>,
    #[prost(uint32, tag = "8")]
    pub flags: u32,
}

pub mod number_data_point {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum Value {
        #[prost(double, tag = "4")]
        AsDouble(f64),
        #[prost(sfixed64, tag = "6")]
        AsInt(i64),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum AggregationTemporality {
    Unspecified = 0,
    Delta = 1,
    Cumulative = 2,
}

pub fn encode_report(report: &ClientReport) -> ExportMetricsServiceRequest {
    let time = report
        .collected_at
        .timestamp_nanos_opt()
        .unwrap_or_default()
        .max(0) as u64;
    // The report does not carry the start time of each OS counter. Integer-second
    // uptime cannot recover it: wall-clock jitter changes the inferred boot time,
    // and a device counter can reset without a reboot. OTLP permits zero when the
    // start is unknown. Keep this pure so old spooled reports retain identical
    // timestamps after process/host restarts; consumers can detect counter drops.
    // https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/metrics/v1/metrics.proto
    let start_time = 0;
    let mut metrics = MetricSet::default();
    metrics.gauge(
        "system.cpu.utilization",
        "1",
        report.system.cpu.usage_percent / 100.0,
        time,
        vec![],
    );
    metrics.gauge(
        "system.memory.usage",
        "By",
        report.system.memory.used_bytes as f64,
        time,
        vec![attr("system.memory.state", "used")],
    );
    metrics.gauge(
        "system.memory.limit",
        "By",
        report.system.memory.total_bytes as f64,
        time,
        vec![],
    );
    metrics.gauge(
        "system.uptime",
        "s",
        report.system.uptime_seconds as f64,
        time,
        vec![],
    );
    for network in &report.system.networks {
        let attributes = vec![attr("network.interface.name", &network.name)];
        metrics.sum(
            "system.network.io",
            "By",
            network.received_bytes_total,
            start_time,
            time,
            with_attr(&attributes, "network.io.direction", "receive"),
        );
        metrics.sum(
            "system.network.io",
            "By",
            network.transmitted_bytes_total,
            start_time,
            time,
            with_attr(&attributes, "network.io.direction", "transmit"),
        );
    }
    for disk in &report.system.disks {
        let attributes = vec![
            attr("system.device", &disk.name),
            attr("system.filesystem.mountpoint", &disk.mount_point),
        ];
        metrics.gauge(
            "system.filesystem.usage",
            "By",
            (disk.total_bytes.saturating_sub(disk.available_bytes)) as f64,
            time,
            with_attr(&attributes, "system.filesystem.state", "used"),
        );
        metrics.sum(
            "system.disk.io",
            "By",
            disk.read_bytes_total,
            start_time,
            time,
            with_attr(&attributes, "disk.io.direction", "read"),
        );
        metrics.sum(
            "system.disk.io",
            "By",
            disk.written_bytes_total,
            start_time,
            time,
            with_attr(&attributes, "disk.io.direction", "write"),
        );
    }
    for temperature in &report.system.temperatures {
        if let Some(value) = temperature.celsius {
            metrics.gauge(
                "hw.temperature",
                "Cel",
                value,
                time,
                vec![
                    attr("sensor.id", &temperature.id),
                    attr("sensor.label", &temperature.label),
                    attr("telemetry.source", &temperature.source),
                ],
            );
        }
    }
    for gpu in &report.system.gpus {
        append_gpu_metrics(&mut metrics, gpu, time);
    }

    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(Resource {
                attributes: vec![
                    attr("host.id", &report.host.id.to_string()),
                    attr("os.type", otel_os_type(&report.host.os)),
                    attr("host.arch", otel_host_arch(&report.host.arch)),
                    attr("service.name", "xsoc"),
                    attr("service.version", &report.host.client_version),
                ],
                dropped_attributes_count: 0,
            }),
            scope_metrics: vec![ScopeMetrics {
                scope: Some(InstrumentationScope {
                    name: "xsoc.hostmetrics".into(),
                    version: report.host.client_version.clone(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                metrics: metrics.into_vec(),
                schema_url: "https://xsos.local/schemas/hostmetrics/1".into(),
            }],
            schema_url: "https://opentelemetry.io/schemas/1.36.0".into(),
        }],
    }
}

fn otel_os_type(value: &str) -> &str {
    match value {
        "macos" => "darwin",
        other => other,
    }
}

fn otel_host_arch(value: &str) -> &str {
    match value {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" | "i386" | "i586" | "i686" => "x86",
        other => other,
    }
}

fn append_gpu_metrics(metrics: &mut MetricSet, gpu: &GpuSnapshot, time: u64) {
    let attrs = vec![
        attr("gpu.id", &gpu.id),
        attr("gpu.vendor", &gpu.vendor),
        attr("gpu.name", &gpu.name),
    ];
    if let Some(value) = gpu.utilization_percent {
        metrics.gauge(
            "hw.gpu.utilization",
            "1",
            value / 100.0,
            time,
            attrs.clone(),
        );
    }
    if let Some(value) = gpu.memory_used_bytes {
        metrics.gauge(
            "hw.gpu.memory.usage",
            "By",
            value as f64,
            time,
            attrs.clone(),
        );
    }
    if let Some(value) = gpu.memory_total_bytes {
        metrics.gauge(
            "hw.gpu.memory.limit",
            "By",
            value as f64,
            time,
            attrs.clone(),
        );
    }
    if let Some(value) = gpu.temperature_celsius {
        metrics.gauge("hw.gpu.temperature", "Cel", value, time, attrs.clone());
    }
    if let Some(value) = gpu.power_watts {
        metrics.gauge("hw.gpu.power", "W", value, time, attrs);
    }
}

/// Metric collection consolidated by name.
///
/// # Why direct `Vec<Metric>::push` is insufficient
///
/// The OTLP data model requires one occurrence of each metric name within a scope. Multiple devices use
/// data-point attributes such as `network.interface.name` and `system.device`. Direct pushes would
/// produce two `system.network.io` metrics with the same name for two interfaces, each with one data point.
///
/// Collectors commonly accept these reports with 200, so successful end-to-end delivery does not prove
/// correct encoding. An actual otelcol-contrib v0.140 file-exporter decode produced
/// duplicate names: `system.network.io` x4, `system.disk.io` x4 and `hw.temperature` x2.
/// Downstream name-based aggregation can overwrite these entries or treat them as conflicting time series.
///
/// Consolidate by `(name, unit, type)`: create the metric on first occurrence, then append matching data points.
/// Type must be part of the key: a Gauge and a Sum with the same name represent a genuine conflict.
/// Keep them separate so tests expose the conflict rather than concealing it through merging.
#[derive(Default)]
struct MetricSet {
    /// Preserve insertion order for comparison and test assertions.
    metrics: Vec<Metric>,
}

impl MetricSet {
    fn gauge(&mut self, name: &str, unit: &str, value: f64, time: u64, attributes: Vec<KeyValue>) {
        let data_point = point(value, time, attributes);
        if let Some(metric::Data::Gauge(existing)) = self.slot(name, unit, MetricKind::Gauge) {
            existing.data_points.push(data_point);
            return;
        }
        self.metrics.push(Metric {
            name: name.into(),
            description: String::new(),
            unit: unit.into(),
            data: Some(metric::Data::Gauge(Gauge {
                data_points: vec![data_point],
            })),
        });
    }

    fn sum(
        &mut self,
        name: &str,
        unit: &str,
        value: u64,
        start_time: u64,
        time: u64,
        attributes: Vec<KeyValue>,
    ) {
        let data_point = number_point(value as f64, start_time, time, attributes);
        if let Some(metric::Data::Sum(existing)) = self.slot(name, unit, MetricKind::Sum) {
            existing.data_points.push(data_point);
            return;
        }
        self.metrics.push(Metric {
            name: name.into(),
            description: String::new(),
            unit: unit.into(),
            data: Some(metric::Data::Sum(Sum {
                data_points: vec![data_point],
                aggregation_temporality: AggregationTemporality::Cumulative as i32,
                is_monotonic: true,
            })),
        });
    }

    /// Find an existing metric to which data points may be appended.
    fn slot(&mut self, name: &str, unit: &str, kind: MetricKind) -> Option<&mut metric::Data> {
        self.metrics
            .iter_mut()
            .find(|metric| {
                metric.name == name && metric.unit == unit && MetricKind::of(metric) == Some(kind)
            })
            .and_then(|metric| metric.data.as_mut())
    }

    fn into_vec(self) -> Vec<Metric> {
        self.metrics
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetricKind {
    Gauge,
    Sum,
}

impl MetricKind {
    fn of(metric: &Metric) -> Option<Self> {
        match metric.data {
            Some(metric::Data::Gauge(_)) => Some(Self::Gauge),
            Some(metric::Data::Sum(_)) => Some(Self::Sum),
            _ => None,
        }
    }
}

fn point(value: f64, time: u64, attributes: Vec<KeyValue>) -> NumberDataPoint {
    number_point(value, 0, time, attributes)
}

fn number_point(
    value: f64,
    start_time: u64,
    time: u64,
    attributes: Vec<KeyValue>,
) -> NumberDataPoint {
    NumberDataPoint {
        attributes,
        start_time_unix_nano: start_time,
        time_unix_nano: time,
        value: Some(number_data_point::Value::AsDouble(value)),
        flags: 0,
    }
}

fn attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
    }
}

fn with_attr(attributes: &[KeyValue], key: &str, value: &str) -> Vec<KeyValue> {
    let mut values = attributes.to_vec();
    values.push(attr(key, value));
    values
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::*;

    #[test]
    fn encodes_stable_host_resource_identity() {
        let host_id = Uuid::new_v4();
        let report = ClientReport {
            schema_version: crate::model::CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            interval_seconds: 10.0,
            host: HostIdentity {
                id: host_id.to_string(),
                os: "macos".into(),
                os_version: None,
                kernel_version: None,
                arch: "aarch64".into(),
                client_version: "test".into(),
            },
            system: SystemSnapshot {
                hardware: None,
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 10.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![10.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 2,
                    used_bytes: 1,
                    available_bytes: 1,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: vec![],
                disks: vec![],
                temperatures: vec![],
                gpus: vec![],
            },
            capabilities: vec![],
            client: ClientHealth {
                spool_pending_batches: 0,
                collector_errors: 0,
            },
        };
        let request = encode_report(&report);
        let resource_metrics = &request.resource_metrics[0];
        let resource = resource_metrics.resource.as_ref().unwrap();
        assert_eq!(
            string_attribute(resource, "host.id"),
            Some(host_id.to_string())
        );
        assert_eq!(
            string_attribute(resource, "os.type").as_deref(),
            Some("darwin")
        );
        assert_eq!(
            string_attribute(resource, "host.arch").as_deref(),
            Some("arm64")
        );
    }

    /// Metric names must be unique within a scope; distinguish devices through data-point attributes.
    ///
    /// # Why the defect escaped the existing check
    ///
    /// The sole end-to-end test asserted only a 2xx response, but the Collector accepted duplicate metric names.
    /// An actual otelcol-contrib v0.140 file-exporter decode of two interfaces, two disks and
    /// two sensors produced `system.network.io` x4, `system.disk.io` x4 and
    /// `hw.temperature` x2. Successful delivery does not prove correct encoding;
    /// the decoded Collector output must be inspected to distinguish them.
    #[test]
    fn repeated_devices_collapse_into_one_metric_with_many_points() {
        let network = |name: &str| NetworkSnapshot {
            name: name.into(),
            received_bytes_total: 1,
            transmitted_bytes_total: 2,
            received_bytes_per_second: 1.0,
            transmitted_bytes_per_second: 2.0,
            packets_received_total: 0,
            packets_transmitted_total: 0,
            receive_errors_total: 0,
            transmit_errors_total: 0,
        };
        let disk = |name: &str, mount: &str| DiskSnapshot {
            name: name.into(),
            mount_point: mount.into(),
            file_system: "ext4".into(),
            total_bytes: 10,
            available_bytes: 4,
            read_bytes_total: 1,
            written_bytes_total: 2,
            read_bytes_per_second: 1.0,
            written_bytes_per_second: 2.0,
            is_read_only: false,
        };
        let sensor = |id: &str| TemperatureSnapshot {
            id: id.into(),
            label: "core".into(),
            celsius: Some(40.0),
            max_celsius: None,
            critical_celsius: None,
            source: "hwmon".into(),
        };

        let mut report = base_report();
        report.system.networks = vec![network("eth0"), network("wlan0")];
        report.system.disks = vec![disk("sda", "/"), disk("sdb", "/data")];
        report.system.temperatures = vec![sensor("t0"), sensor("t1")];

        let request = encode_report(&report);
        let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;

        // 1) No duplicate metric names.
        let mut names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        names.sort_unstable();
        let unique = {
            let mut copy = names.clone();
            copy.dedup();
            copy
        };
        assert_eq!(
            names, unique,
            "duplicate metric names within one scope violate the OTLP data model: {names:?}"
        );

        // 2) Device count is represented by data-point count rather than metric count.
        let points = |name: &str| -> usize {
            metrics
                .iter()
                .filter(|m| m.name == name)
                .map(|m| match m.data.as_ref() {
                    Some(metric::Data::Gauge(g)) => g.data_points.len(),
                    Some(metric::Data::Sum(s)) => s.data_points.len(),
                    None => 0,
                })
                .sum()
        };
        // 2 interfaces x receive/transmit = 4 points under one metric.
        assert_eq!(points("system.network.io"), 4);
        // 2 disks x read/write = 4 points.
        assert_eq!(points("system.disk.io"), 4);
        assert_eq!(points("system.filesystem.usage"), 2);
        assert_eq!(points("hw.temperature"), 2);
    }

    /// Empty device lists must produce no device metrics, avoiding metrics with zero data points.
    #[test]
    fn a_report_without_devices_emits_no_device_metrics() {
        let request = encode_report(&base_report());
        let names: Vec<&str> = request.resource_metrics[0].scope_metrics[0]
            .metrics
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        for absent in [
            "system.network.io",
            "system.disk.io",
            "system.filesystem.usage",
            "hw.temperature",
        ] {
            assert!(!names.contains(&absent), "unexpected {absent}: {names:?}");
        }
    }

    fn base_report() -> ClientReport {
        ClientReport {
            schema_version: crate::model::CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            interval_seconds: 10.0,
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "linux".into(),
                os_version: None,
                kernel_version: None,
                arch: "x86_64".into(),
                client_version: "test".into(),
            },
            system: SystemSnapshot {
                hardware: None,
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 10.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![10.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 2,
                    used_bytes: 1,
                    available_bytes: 1,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: vec![],
                disks: vec![],
                temperatures: vec![],
                gpus: vec![],
            },
            capabilities: vec![],
            client: ClientHealth {
                spool_pending_batches: 0,
                collector_errors: 0,
            },
        }
    }

    fn string_attribute(resource: &Resource, key: &str) -> Option<String> {
        resource.attributes.iter().find_map(|attribute| {
            if attribute.key != key {
                return None;
            }
            match attribute.value.as_ref()?.value.as_ref()? {
                any_value::Value::StringValue(value) => Some(value.clone()),
                _ => None,
            }
        })
    }
}
