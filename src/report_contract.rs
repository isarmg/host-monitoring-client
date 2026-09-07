//! Client-side enforcement of the shared report wire contract.
//!
//! The Server must reject untrusted input, but a report produced by the official Client should
//! never discover those limits by receiving a permanent HTTP 400/413 after it has entered the
//! durable spool. This module bounds freshly collected reports, then serializes the exact bytes
//! used by both the durable spool and the HTTP request.

use anyhow::{Context, ensure};
use chrono::{Duration, Utc};
use serde::Serialize;

use crate::model::*;

const TRUNCATED_CAPABILITY: &str = "client.report.truncated";
const TRUNCATED_SOURCE: &str = "host-monitor";
const TRUNCATED_MESSAGE: &str =
    "one or more collected values were bounded to the current report contract";

/// Return a bounded clone and the compact JSON bytes that must be sent on the wire.
pub(crate) fn encode_report_body(report: &ClientReport) -> anyhow::Result<(ClientReport, Vec<u8>)> {
    ensure!(
        report.schema_version == CLIENT_REPORT_SCHEMA_VERSION,
        "unsupported Client report schema_version {}; expected {}",
        report.schema_version,
        CLIENT_REPORT_SCHEMA_VERSION
    );
    let mut bounded = report.clone();
    bound_report(&mut bounded);
    let body = serde_json::to_vec(&bounded).context("failed to serialize bounded Client report")?;
    ensure!(
        body.len() <= CLIENT_REPORT_MAX_BODY_BYTES,
        "bounded Client report is {} bytes, above the {} byte wire limit",
        body.len(),
        CLIENT_REPORT_MAX_BODY_BYTES
    );
    Ok((bounded, body))
}

/// Make a report deterministic and acceptable to the current Server contract.
///
/// Returns whether any information had to be changed or discarded. Reapplying the function is
/// idempotent; in particular, the truncation diagnostic and collector error count are added once.
pub(crate) fn bound_report(report: &mut ClientReport) -> bool {
    let already_marked = report
        .capabilities
        .iter()
        .any(|capability| capability.name == TRUNCATED_CAPABILITY);
    let mut changed = normalize_scalars_and_text(report);

    let cpu = &mut report.system.cpu;
    if cpu.per_core_percent.is_empty() {
        cpu.per_core_percent.push(cpu.usage_percent);
        changed = true;
    }
    for value in &mut cpu.per_core_percent {
        changed |= bound_percent(value);
    }
    changed |= truncate(&mut cpu.per_core_percent, CLIENT_REPORT_MAX_CPU_CORES);
    let logical_count = u32::try_from(cpu.per_core_percent.len())
        .expect("the shared CPU core limit fits the fixed-width wire count");
    if cpu.logical_count != logical_count {
        cpu.logical_count = logical_count;
        changed = true;
    }
    if cpu
        .physical_count
        .is_some_and(|count| count == 0 || count > logical_count)
    {
        cpu.physical_count = None;
        changed = true;
    }

    let capability_count = report.capabilities.len();
    order_capabilities(&mut report.capabilities);
    report
        .capabilities
        .dedup_by(|left, right| left.name == right.name && left.source == right.source);
    changed |= report.capabilities.len() != capability_count;
    changed |= truncate(&mut report.capabilities, CLIENT_REPORT_MAX_CAPABILITIES);

    let network_anchors = prioritize_by_metrics(
        &mut report.system.networks,
        &[
            |item: &NetworkSnapshot| Some(item.received_bytes_per_second),
            |item: &NetworkSnapshot| Some(item.transmitted_bytes_per_second),
        ],
    );
    changed |= truncate(&mut report.system.networks, CLIENT_REPORT_MAX_NETWORKS);

    let disk_anchors = prioritize_by_metrics(
        &mut report.system.disks,
        &[
            |item: &DiskSnapshot| Some(item.read_bytes_per_second),
            |item: &DiskSnapshot| Some(item.written_bytes_per_second),
        ],
    );
    changed |= truncate(&mut report.system.disks, CLIENT_REPORT_MAX_DISKS);

    let temperature_anchors = prioritize_by_metrics(
        &mut report.system.temperatures,
        &[|item: &TemperatureSnapshot| item.celsius],
    );
    changed |= truncate(
        &mut report.system.temperatures,
        CLIENT_REPORT_MAX_TEMPERATURES,
    );

    let gpu_anchors = prioritize_by_metrics(
        &mut report.system.gpus,
        &[
            |item: &GpuSnapshot| item.utilization_percent,
            |item: &GpuSnapshot| item.temperature_celsius,
        ],
    );
    changed |= truncate(&mut report.system.gpus, CLIENT_REPORT_MAX_GPUS);

    if serialized_len(report) > CLIENT_REPORT_MAX_BODY_BYTES {
        changed = true;
    }
    if changed && !already_marked {
        report.capabilities.push(Capability::unavailable(
            TRUNCATED_CAPABILITY,
            TRUNCATED_SOURCE,
            CapabilityErrorKind::InvalidData,
            TRUNCATED_MESSAGE,
        ));
        report.client.collector_errors = report.client.collector_errors.saturating_add(1);
        order_capabilities(&mut report.capabilities);
        truncate(&mut report.capabilities, CLIENT_REPORT_MAX_CAPABILITIES);
    }

    fit_body(
        report,
        network_anchors.min(report.system.networks.len()),
        disk_anchors.min(report.system.disks.len()),
        temperature_anchors.min(report.system.temperatures.len()),
        gpu_anchors.min(report.system.gpus.len()),
    );
    changed
}

fn normalize_scalars_and_text(report: &mut ClientReport) -> bool {
    let mut changed = false;
    if !report.interval_seconds.is_finite() {
        report.interval_seconds = CLIENT_REPORT_MIN_INTERVAL_SECONDS;
        changed = true;
    } else {
        let bounded = report.interval_seconds.clamp(
            CLIENT_REPORT_MIN_INTERVAL_SECONDS,
            CLIENT_REPORT_MAX_INTERVAL_SECONDS as f64,
        );
        changed |= replace_f64(&mut report.interval_seconds, bounded);
    }
    let now = Utc::now();
    if report.collected_at > now + Duration::minutes(5) {
        report.collected_at = now;
        changed = true;
    }

    changed |= bound_required_text(
        &mut report.host.os,
        CLIENT_REPORT_MAX_HOST_OS_BYTES,
        "unknown",
    );
    changed |= bound_optional_text(
        &mut report.host.os_version,
        CLIENT_REPORT_MAX_HOST_VERSION_BYTES,
    );
    changed |= bound_optional_text(
        &mut report.host.kernel_version,
        CLIENT_REPORT_MAX_HOST_VERSION_BYTES,
    );
    changed |= bound_required_text(
        &mut report.host.arch,
        CLIENT_REPORT_MAX_HOST_ARCH_BYTES,
        "unknown",
    );
    changed |= bound_required_text(
        &mut report.host.client_version,
        CLIENT_REPORT_MAX_CLIENT_VERSION_BYTES,
        "unknown",
    );

    changed |= bound_percent(&mut report.system.cpu.usage_percent);
    let memory = &mut report.system.memory;
    if memory.used_bytes > memory.total_bytes {
        memory.used_bytes = memory.total_bytes;
        changed = true;
    }
    if memory.available_bytes > memory.total_bytes {
        memory.available_bytes = memory.total_bytes;
        changed = true;
    }
    if memory.swap_used_bytes > memory.swap_total_bytes {
        memory.swap_used_bytes = memory.swap_total_bytes;
        changed = true;
    }

    for capability in &mut report.capabilities {
        changed |= bound_required_text(
            &mut capability.name,
            CLIENT_REPORT_MAX_CAPABILITY_NAME_BYTES,
            "unknown.capability",
        );
        changed |= bound_required_text(
            &mut capability.source,
            CLIENT_REPORT_MAX_CAPABILITY_SOURCE_BYTES,
            "unknown",
        );
        changed |= bound_optional_nonempty_text(
            &mut capability.message,
            CLIENT_REPORT_MAX_CAPABILITY_MESSAGE_BYTES,
        );
    }
    for network in &mut report.system.networks {
        changed |= bound_required_text(
            &mut network.name,
            CLIENT_REPORT_MAX_NETWORK_NAME_BYTES,
            "unnamed-network",
        );
        changed |= bound_nonnegative(&mut network.received_bytes_per_second);
        changed |= bound_nonnegative(&mut network.transmitted_bytes_per_second);
    }
    for disk in &mut report.system.disks {
        changed |= bound_descriptive_text(&mut disk.name, CLIENT_REPORT_MAX_DISK_NAME_BYTES);
        changed |= bound_required_text(
            &mut disk.mount_point,
            CLIENT_REPORT_MAX_MOUNT_POINT_BYTES,
            "unknown",
        );
        changed |=
            bound_descriptive_text(&mut disk.file_system, CLIENT_REPORT_MAX_FILE_SYSTEM_BYTES);
        if disk.available_bytes > disk.total_bytes {
            disk.available_bytes = disk.total_bytes;
            changed = true;
        }
        changed |= bound_nonnegative(&mut disk.read_bytes_per_second);
        changed |= bound_nonnegative(&mut disk.written_bytes_per_second);
    }
    for sensor in &mut report.system.temperatures {
        changed |= bound_descriptive_text(&mut sensor.id, CLIENT_REPORT_MAX_TEMPERATURE_ID_BYTES);
        changed |=
            bound_descriptive_text(&mut sensor.label, CLIENT_REPORT_MAX_TEMPERATURE_LABEL_BYTES);
        changed |= bound_descriptive_text(
            &mut sensor.source,
            CLIENT_REPORT_MAX_TEMPERATURE_SOURCE_BYTES,
        );
        changed |= bound_optional_range(&mut sensor.celsius, -273.15, 1000.0);
        changed |= bound_optional_range(&mut sensor.max_celsius, -273.15, 1000.0);
        changed |= bound_optional_range(&mut sensor.critical_celsius, -273.15, 1000.0);
    }
    for gpu in &mut report.system.gpus {
        changed |= bound_descriptive_text(&mut gpu.id, CLIENT_REPORT_MAX_GPU_ID_BYTES);
        changed |= bound_descriptive_text(&mut gpu.vendor, CLIENT_REPORT_MAX_GPU_VENDOR_BYTES);
        changed |= bound_descriptive_text(&mut gpu.name, CLIENT_REPORT_MAX_GPU_NAME_BYTES);
        changed |= bound_descriptive_text(&mut gpu.source, CLIENT_REPORT_MAX_GPU_SOURCE_BYTES);
        changed |= bound_optional_range(&mut gpu.utilization_percent, 0.0, 100.0);
        if let (Some(used), Some(total)) = (gpu.memory_used_bytes, gpu.memory_total_bytes)
            && used > total
        {
            gpu.memory_used_bytes = Some(total);
            changed = true;
        }
        changed |= bound_optional_range(&mut gpu.temperature_celsius, -273.15, 1000.0);
        for value in [
            &mut gpu.power_watts,
            &mut gpu.core_clock_mhz,
            &mut gpu.memory_clock_mhz,
            &mut gpu.pcie_rx_bytes_per_second,
            &mut gpu.pcie_tx_bytes_per_second,
        ] {
            changed |= bound_optional_nonnegative(value);
        }
    }
    changed
}

fn fit_body(
    report: &mut ClientReport,
    network_minimum: usize,
    disk_minimum: usize,
    temperature_minimum: usize,
    gpu_minimum: usize,
) {
    let capability_minimum = usize::from(
        report
            .capabilities
            .first()
            .is_some_and(|capability| capability.name == TRUNCATED_CAPABILITY),
    );
    let minima = [
        capability_minimum,
        network_minimum,
        disk_minimum,
        temperature_minimum,
        gpu_minimum,
    ];
    while serialized_len(report) > CLIENT_REPORT_MAX_BODY_BYTES {
        let lengths = collection_lengths(report);
        let removable: usize = lengths
            .iter()
            .zip(minima)
            .map(|(length, minimum)| length.saturating_sub(minimum))
            .sum();
        if removable == 0 {
            break;
        }
        let current = serialized_len(report).max(1);
        // Leave a small margin for integer rounding and JSON punctuation. Usually one pass is
        // enough; the loop makes the exact byte check authoritative.
        let ratio = (CLIENT_REPORT_MAX_BODY_BYTES as f64 / current as f64 * 0.97).min(0.99);
        shrink_collections(report, minima, ratio);
    }

    // The bounded anchor set is tiny, so a normal report is already far below the body limit.
    // This defensive fallback still guarantees progress if a future fixed field grows markedly.
    while serialized_len(report) > CLIENT_REPORT_MAX_BODY_BYTES {
        if !remove_last_optional(report, capability_minimum) {
            break;
        }
    }
}

fn collection_lengths(report: &ClientReport) -> [usize; 5] {
    [
        report.capabilities.len(),
        report.system.networks.len(),
        report.system.disks.len(),
        report.system.temperatures.len(),
        report.system.gpus.len(),
    ]
}

fn shrink_collections(report: &mut ClientReport, minima: [usize; 5], ratio: f64) {
    shrink(&mut report.capabilities, minima[0], ratio);
    shrink(&mut report.system.networks, minima[1], ratio);
    shrink(&mut report.system.disks, minima[2], ratio);
    shrink(&mut report.system.temperatures, minima[3], ratio);
    shrink(&mut report.system.gpus, minima[4], ratio);
}

fn shrink<T>(values: &mut Vec<T>, minimum: usize, ratio: f64) {
    if values.len() <= minimum {
        return;
    }
    let mut target = ((values.len() as f64) * ratio).floor() as usize;
    target = target.max(minimum);
    if target >= values.len() {
        target = values.len() - 1;
    }
    values.truncate(target);
}

fn remove_last_optional(report: &mut ClientReport, capability_minimum: usize) -> bool {
    if report.system.temperatures.pop().is_some()
        || report.system.disks.pop().is_some()
        || report.system.networks.pop().is_some()
        || report.system.gpus.pop().is_some()
    {
        return true;
    }
    if report.capabilities.len() > capability_minimum {
        report.capabilities.pop();
        return true;
    }
    false
}

fn order_capabilities(values: &mut [Capability]) {
    values.sort_by(|left, right| {
        let left_marker = left.name == TRUNCATED_CAPABILITY;
        let right_marker = right.name == TRUNCATED_CAPABILITY;
        right_marker
            .cmp(&left_marker)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| right.available.cmp(&left.available))
            .then_with(|| serialized_key(left).cmp(&serialized_key(right)))
    });
}

/// Canonicalize enumeration order and place the distinct summary-metric maxima first.
fn prioritize_by_metrics<T>(values: &mut Vec<T>, metrics: &[fn(&T) -> Option<f64>]) -> usize
where
    T: Clone + Serialize,
{
    values.sort_by_cached_key(serialized_key);
    let mut anchors = Vec::new();
    for metric in metrics {
        let candidate = values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| metric(value).map(|metric| (index, metric)))
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index);
        if let Some(index) = candidate
            && !anchors.contains(&index)
        {
            anchors.push(index);
        }
    }
    let mut ordered = Vec::with_capacity(values.len());
    ordered.extend(anchors.iter().map(|index| values[*index].clone()));
    ordered.extend(
        values
            .drain(..)
            .enumerate()
            .filter_map(|(index, value)| (!anchors.contains(&index)).then_some(value)),
    );
    *values = ordered;
    anchors.len()
}

fn serialized_key<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn serialized_len(report: &ClientReport) -> usize {
    serde_json::to_vec(report)
        .map(|body| body.len())
        .unwrap_or(usize::MAX)
}

fn truncate<T>(values: &mut Vec<T>, maximum: usize) -> bool {
    if values.len() <= maximum {
        return false;
    }
    values.truncate(maximum);
    true
}

fn bound_required_text(value: &mut String, maximum: usize, fallback: &str) -> bool {
    let original = value.clone();
    strip_controls_and_truncate(value, maximum);
    if value.trim().is_empty() {
        *value = fallback.to_string();
        truncate_utf8(value, maximum);
    }
    *value != original
}

fn bound_descriptive_text(value: &mut String, maximum: usize) -> bool {
    let original = value.clone();
    strip_controls_and_truncate(value, maximum);
    *value != original
}

fn bound_optional_text(value: &mut Option<String>, maximum: usize) -> bool {
    let Some(value) = value else { return false };
    bound_descriptive_text(value, maximum)
}

fn bound_optional_nonempty_text(value: &mut Option<String>, maximum: usize) -> bool {
    let original = value.clone();
    if let Some(text) = value {
        strip_controls_and_truncate(text, maximum);
        if text.trim().is_empty() {
            *value = None;
        }
    }
    *value != original
}

fn strip_controls_and_truncate(value: &mut String, maximum: usize) {
    value.retain(|character| !character.is_control());
    truncate_utf8(value, maximum);
}

fn truncate_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn bound_percent(value: &mut f64) -> bool {
    let bounded = if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    };
    replace_f64(value, bounded)
}

fn bound_nonnegative(value: &mut f64) -> bool {
    let bounded = if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    };
    replace_f64(value, bounded)
}

fn bound_optional_nonnegative(value: &mut Option<f64>) -> bool {
    bound_optional_range(value, 0.0, f64::MAX)
}

fn bound_optional_range(value: &mut Option<f64>, minimum: f64, maximum: f64) -> bool {
    if value.is_some_and(|value| !value.is_finite() || !(minimum..=maximum).contains(&value)) {
        *value = None;
        true
    } else {
        false
    }
}

fn replace_f64(value: &mut f64, replacement: f64) -> bool {
    if value.to_bits() == replacement.to_bits() {
        return false;
    }
    *value = replacement;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn report() -> ClientReport {
        ClientReport {
            schema_version: CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "linux".into(),
                os_version: Some("test".into()),
                kernel_version: None,
                arch: "x86_64".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 10.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![10.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 100,
                    used_bytes: 50,
                    available_bytes: 50,
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
        }
    }

    fn network(name: impl Into<String>, receive: f64, transmit: f64) -> NetworkSnapshot {
        NetworkSnapshot {
            name: name.into(),
            received_bytes_total: 1,
            transmitted_bytes_total: 1,
            received_bytes_per_second: receive,
            transmitted_bytes_per_second: transmit,
            packets_received_total: 1,
            packets_transmitted_total: 1,
            receive_errors_total: 0,
            transmit_errors_total: 0,
        }
    }

    fn disk(name: impl Into<String>, mount_point: impl Into<String>) -> DiskSnapshot {
        DiskSnapshot {
            name: name.into(),
            mount_point: mount_point.into(),
            file_system: "ext4".into(),
            total_bytes: 100,
            available_bytes: 50,
            read_bytes_total: 1,
            written_bytes_total: 1,
            read_bytes_per_second: 1.0,
            written_bytes_per_second: 1.0,
            is_read_only: false,
        }
    }

    fn temperature(id: impl Into<String>) -> TemperatureSnapshot {
        TemperatureSnapshot {
            id: id.into(),
            label: "sensor".into(),
            celsius: Some(40.0),
            max_celsius: None,
            critical_celsius: None,
            source: "test".into(),
        }
    }

    fn gpu(id: impl Into<String>) -> GpuSnapshot {
        GpuSnapshot {
            id: id.into(),
            vendor: "test".into(),
            name: "gpu".into(),
            utilization_percent: Some(10.0),
            memory_total_bytes: Some(100),
            memory_used_bytes: Some(50),
            temperature_celsius: Some(40.0),
            power_watts: None,
            core_clock_mhz: None,
            memory_clock_mhz: None,
            pcie_rx_bytes_per_second: None,
            pcie_tx_bytes_per_second: None,
            source: "test".into(),
        }
    }

    #[test]
    fn every_variable_collection_is_bounded_and_cpu_fields_stay_consistent() {
        let mut value = report();
        value.system.cpu.logical_count = u32::MAX;
        value.system.cpu.physical_count = Some(u32::MAX);
        value.system.cpu.per_core_percent = vec![1.0; CLIENT_REPORT_MAX_CPU_CORES + 1];
        value.system.networks = (0..=CLIENT_REPORT_MAX_NETWORKS)
            .map(|index| network(format!("network-{index}"), 1.0, 1.0))
            .collect();
        value.system.disks = (0..=CLIENT_REPORT_MAX_DISKS)
            .map(|index| disk(format!("disk-{index}"), format!("/{index}")))
            .collect();
        value.system.temperatures = (0..=CLIENT_REPORT_MAX_TEMPERATURES)
            .map(|index| temperature(format!("temperature-{index}")))
            .collect();
        value.system.gpus = (0..=CLIENT_REPORT_MAX_GPUS)
            .map(|index| gpu(format!("gpu-{index}")))
            .collect();
        value.capabilities = (0..=CLIENT_REPORT_MAX_CAPABILITIES)
            .map(|index| Capability::available(format!("capability-{index}"), "test"))
            .collect();

        assert!(bound_report(&mut value));
        assert!(value.capabilities.len() <= CLIENT_REPORT_MAX_CAPABILITIES);
        assert!(value.system.networks.len() <= CLIENT_REPORT_MAX_NETWORKS);
        assert!(value.system.disks.len() <= CLIENT_REPORT_MAX_DISKS);
        assert!(value.system.temperatures.len() <= CLIENT_REPORT_MAX_TEMPERATURES);
        assert!(value.system.gpus.len() <= CLIENT_REPORT_MAX_GPUS);
        assert_eq!(
            value.system.cpu.per_core_percent.len(),
            CLIENT_REPORT_MAX_CPU_CORES
        );
        assert_eq!(
            value.system.cpu.logical_count as usize,
            value.system.cpu.per_core_percent.len()
        );
        assert_eq!(value.system.cpu.physical_count, None);
        assert_eq!(value.capabilities[0].name, TRUNCATED_CAPABILITY);
    }

    #[test]
    fn enumeration_order_does_not_change_the_bounded_report() {
        let mut ascending = report();
        ascending.system.networks = (0..20)
            .map(|index| {
                network(
                    format!("network-{index:02}"),
                    index as f64,
                    (20 - index) as f64,
                )
            })
            .collect();
        ascending.system.disks = (0..20)
            .map(|index| disk(format!("disk-{index:02}"), format!("/{index:02}")))
            .collect();
        ascending.capabilities = (0..20)
            .map(|index| Capability::available(format!("capability-{index:02}"), "test"))
            .collect();
        let mut descending = ascending.clone();
        descending.system.networks.reverse();
        descending.system.disks.reverse();
        descending.capabilities.reverse();

        assert!(!bound_report(&mut ascending));
        assert!(!bound_report(&mut descending));
        assert_eq!(ascending, descending);
    }

    #[test]
    fn bounding_is_idempotent_and_utf8_safe() {
        let mut value = report();
        value.host.os = format!("bad\n{}", "界".repeat(200));
        value.capabilities.push(Capability {
            name: "empty-message".into(),
            available: false,
            source: "test".into(),
            error_kind: Some(CapabilityErrorKind::InvalidData),
            message: Some("\n\t".into()),
        });
        value.system.cpu.logical_count = 0;
        value.system.cpu.physical_count = Some(0);
        value.system.cpu.per_core_percent.clear();

        assert!(bound_report(&mut value));
        assert!(value.host.os.len() <= CLIENT_REPORT_MAX_HOST_OS_BYTES);
        assert!(!value.host.os.chars().any(char::is_control));
        assert_eq!(value.capabilities[1].message, None);
        let once = value.clone();
        assert!(!bound_report(&mut value));
        assert_eq!(value, once);
        assert_eq!(value.client.collector_errors, 1);
    }

    #[test]
    fn exact_json_size_is_bounded_without_losing_summary_peaks() {
        let mut value = report();
        let long_mount = format!("/{}", "界".repeat(1300));
        value.system.disks = (0..CLIENT_REPORT_MAX_DISKS)
            .map(|index| {
                let mut item = disk(format!("disk-{index:04}"), format!("{long_mount}-{index}"));
                if index == 17 {
                    item.read_bytes_per_second = 999.0;
                }
                if index == 29 {
                    item.written_bytes_per_second = 888.0;
                }
                item
            })
            .collect();
        assert!(serde_json::to_vec(&value).unwrap().len() > CLIENT_REPORT_MAX_BODY_BYTES);

        let (bounded, body) = encode_report_body(&value).unwrap();
        assert!(body.len() <= CLIENT_REPORT_MAX_BODY_BYTES);
        assert!(
            bounded
                .system
                .disks
                .iter()
                .any(|item| item.read_bytes_per_second == 999.0)
        );
        assert!(
            bounded
                .system
                .disks
                .iter()
                .any(|item| item.written_bytes_per_second == 888.0)
        );
        assert_eq!(
            serde_json::from_slice::<ClientReport>(&body).unwrap(),
            bounded
        );
    }

    #[test]
    fn an_unknown_schema_is_never_silently_rewritten() {
        let mut value = report();
        value.schema_version = CLIENT_REPORT_SCHEMA_VERSION + 1;
        let original = value.clone();

        let error = encode_report_body(&value).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Client report schema_version")
        );
        assert_eq!(value, original);
    }
}
