use std::{path::Path, time::Instant};

#[cfg(not(target_os = "linux"))]
use std::collections::HashSet;

use chrono::Utc;
use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System,
};
use uuid::Uuid;

#[cfg(any(target_os = "linux", all(target_os = "windows", feature = "nvidia")))]
use crate::model::CLIENT_REPORT_MAX_GPUS;
use crate::model::{
    CLIENT_REPORT_MAX_CAPABILITIES, CLIENT_REPORT_MAX_CPU_CORES, CLIENT_REPORT_MAX_DISKS,
    CLIENT_REPORT_MAX_NETWORKS, CLIENT_REPORT_MAX_TEMPERATURES, CLIENT_REPORT_SCHEMA_VERSION,
    Capability, CapabilityErrorKind, ClientHealth, ClientReport, CpuSnapshot, DiskSnapshot,
    GpuSnapshot, HostIdentity, MemorySnapshot, NetworkSnapshot, SystemSnapshot,
    TemperatureSnapshot,
};
#[cfg(any(target_os = "windows", test))]
mod gpu_merge;
#[cfg(target_os = "linux")]
mod linux_gpu;
#[cfg(target_os = "linux")]
mod linux_hwmon;
#[cfg(target_os = "macos")]
mod macos_inventory;
#[cfg(target_os = "macos")]
mod macos_network;
#[cfg(all(
    feature = "nvidia",
    any(target_os = "linux", target_os = "windows", test)
))]
#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]
// Keep platform-independent NVML tests without loading it on unsupported systems.
mod nvidia;
#[cfg(any(target_os = "windows", test))]
mod nvidia_identity;
#[cfg(any(target_os = "windows", test))]
mod pdh_buffer;
#[cfg(any(target_os = "windows", test))]
mod pdh_recovery;
mod physical_network;
#[cfg(target_os = "windows")]
mod windows_gpu;
#[cfg(any(all(target_os = "windows", target_arch = "x86_64"), test))]
#[cfg_attr(not(windows), allow(dead_code))] // Exercise native ABI/conversion tests on Linux.
mod windows_vendor;

mod disk_rates;
mod hardware;
mod inventory;
pub mod smart;

/// Reuse sysinfo objects to avoid repeated enumeration and preserve the baseline for delta metrics.
pub struct SystemSampler {
    system: System,
    networks: Networks,
    disks: Disks,
    disk_rates: disk_rates::DiskRates,
    components: Components,
    last_sample: Instant,
    last_slow_sample: Option<Instant>,
    cached_temperatures: Vec<TemperatureSnapshot>,
    cached_temperature_capability: Capability,
    gpu_runtime: GpuRuntime,
    hardware: Option<crate::model::HardwareSnapshot>,
    hardware_capability: Capability,
    smart: smart::SmartCollector,
    inventory: inventory::InventoryCollector,
}

impl SystemSampler {
    pub fn new() -> Self {
        Self::with_smart_config(smart::SmartConfig::default())
    }

    pub fn with_smart_config(config: smart::SmartConfig) -> Self {
        #[allow(unused_mut)]
        let mut gpu_runtime = GpuRuntime::new();
        #[cfg(target_os = "windows")]
        let _ = gpu_runtime.collect(); // Prime PDH and start asynchronous vendor sampling.
        let disks = Disks::new_with_refreshed_list();
        let mut disk_rates = disk_rates::DiskRates::default();
        disk_rates.update(&mut collect_disks(&disks), 1.0);
        Self {
            // The Client never reads process data. `new_all()` eagerly walks
            // every process (and Linux task) and retains that unused snapshot
            // for the lifetime of this sampler.
            system: System::new_with_specifics(
                RefreshKind::nothing()
                    .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                    .with_memory(MemoryRefreshKind::everything()),
            ),
            networks: Networks::new_with_refreshed_list(),
            disks,
            disk_rates,
            components: {
                #[cfg(target_os = "linux")]
                {
                    Components::new()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Components::new_with_refreshed_list()
                }
            },
            last_sample: Instant::now(),
            last_slow_sample: None,
            cached_temperatures: Vec::new(),
            cached_temperature_capability: temperature_capability(&[]),
            gpu_runtime,
            hardware: None,
            hardware_capability: Capability::available("hardware.inventory", "sysinfo"),
            smart: smart::SmartCollector::new(config),
            inventory: inventory::InventoryCollector::new(),
        }
    }

    pub fn vendor_scan_pending(&self) -> bool {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return self.gpu_runtime.vendors.is_pending();
        #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
        false
    }

    pub fn smart_scan_pending(&mut self) -> bool {
        self.smart.poll();
        self.smart.is_pending()
    }

    pub fn inventory_scan_pending(&mut self) -> bool {
        self.inventory.poll(300);
        let pending = self.inventory.is_pending();
        #[cfg(target_os = "macos")]
        {
            self.gpu_runtime.inventory.poll();
            pending || self.gpu_runtime.inventory.is_pending()
        }
        #[cfg(not(target_os = "macos"))]
        pending
    }

    pub fn collect(
        &mut self,
        host: HostIdentity,
        slow_interval_seconds: u64,
        spool_pending_batches: u64,
    ) -> ClientReport {
        let now = Instant::now();
        let elapsed_seconds = now.duration_since(self.last_sample).as_secs_f64();
        // Keep the advertised interval inside the Server contract after a long pause,
        // but calculate byte rates over the actual counter sampling window.
        let interval_seconds = contract_interval_seconds(elapsed_seconds);
        self.last_sample = now;

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);

        let refresh_slow = self
            .last_slow_sample
            .is_none_or(|last| now.duration_since(last).as_secs() >= slow_interval_seconds);
        if refresh_slow {
            self.system.refresh_cpu_frequency();
            let (hardware, capability) = hardware::collect(&self.system, &self.networks);
            self.hardware = Some(hardware);
            self.hardware_capability = capability;
            #[cfg(not(target_os = "linux"))]
            self.components.refresh(true);
            let temperature_result = collect_temperatures(&self.components);
            self.cached_temperatures = collect_bounded(
                temperature_result.temperatures,
                CLIENT_REPORT_MAX_TEMPERATURES,
            );
            self.cached_temperature_capability = temperature_result.capability;
            self.last_slow_sample = Some(now);
        }

        let gpu = self.gpu_runtime.collect();
        let mut disk_snapshots = collect_disks(&self.disks);
        self.disk_rates.update(&mut disk_snapshots, elapsed_seconds);
        let mut capabilities =
            core_capabilities(&self.cached_temperature_capability, &disk_snapshots);
        let (disk_health, smart_capability) = self.smart.poll();
        let inventory = self.inventory.poll(slow_interval_seconds);
        if let Some(hardware) = &mut self.hardware {
            hardware.inventory_collected_at = inventory.collected_at;
            hardware.memory_modules = inventory.memory_modules;
            hardware.devices = inventory.devices;
            hardware.disk_health = disk_health;
            hardware
                .sensors
                .retain(|sensor| !matches!(sensor.source.as_str(), "amd-adlx" | "intel-igcl"));
            extend_bounded(
                &mut hardware.sensors,
                gpu.sensors,
                crate::model::MAX_HARDWARE_SENSORS,
            );
        }
        capabilities.push(self.hardware_capability.clone());
        capabilities.push(smart_capability);
        extend_bounded(
            &mut capabilities,
            inventory.capabilities,
            CLIENT_REPORT_MAX_CAPABILITIES,
        );
        extend_bounded(
            &mut capabilities,
            gpu.capabilities,
            CLIENT_REPORT_MAX_CAPABILITIES,
        );
        capabilities.sort_by(|left, right| left.name.cmp(&right.name));
        capabilities.dedup_by(|left, right| left.name == right.name && left.source == right.source);
        let collector_errors = capabilities
            .iter()
            .filter(|capability| {
                !capability.available
                    && matches!(
                        capability.error_kind,
                        Some(CapabilityErrorKind::Transient | CapabilityErrorKind::InvalidData)
                    )
            })
            .count() as u64;

        let mut report = ClientReport {
            schema_version: CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host,
            interval_seconds,
            system: SystemSnapshot {
                hardware: self.hardware.clone(),
                uptime_seconds: System::uptime(),
                cpu: CpuSnapshot {
                    usage_percent: finite(self.system.global_cpu_usage() as f64).unwrap_or(0.0),
                    logical_count: wire_cpu_count(self.system.cpus().len()),
                    physical_count: System::physical_core_count().map(wire_cpu_count),
                    // sysinfo owns its internal platform enumeration; this layer avoids making a
                    // second unbounded copy of it before the report contract is applied.
                    per_core_percent: collect_bounded(
                        self.system
                            .cpus()
                            .iter()
                            .map(|cpu| finite(cpu.cpu_usage() as f64).unwrap_or(0.0)),
                        CLIENT_REPORT_MAX_CPU_CORES,
                    ),
                },
                memory: MemorySnapshot {
                    total_bytes: self.system.total_memory(),
                    used_bytes: self.system.used_memory(),
                    available_bytes: self.system.available_memory(),
                    swap_total_bytes: self.system.total_swap(),
                    swap_used_bytes: self.system.used_swap(),
                },
                networks: collect_networks(&self.networks, elapsed_seconds),
                disks: disk_snapshots,
                temperatures: collect_bounded(
                    self.cached_temperatures
                        .iter()
                        .cloned()
                        .chain(gpu.temperatures),
                    CLIENT_REPORT_MAX_TEMPERATURES,
                ),
                gpus: gpu.gpus,
            },
            capabilities,
            client: ClientHealth {
                spool_pending_batches,
                collector_errors,
            },
        };
        crate::report_contract::bound_report(&mut report);
        report
    }
}

impl Default for SystemSampler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load_host_identity(state_dir: &Path) -> anyhow::Result<HostIdentity> {
    let reader = crate::state_store::StateReader::open(state_dir)?;
    load_host_identity_from(&reader)
}

pub(crate) fn load_host_identity_from(
    reader: &crate::state_store::StateReader,
) -> anyhow::Result<HostIdentity> {
    let identity = crate::client_identity::from_state(reader)?;
    Ok(host_details(identity.instance_id().to_owned()))
}

/// Build a collection identity without touching durable state. Used only by
/// read-only local diagnostics and capability probes.
pub fn transient_host_identity(id: Uuid) -> HostIdentity {
    host_details(id.to_string())
}

fn host_details(id: String) -> HostIdentity {
    HostIdentity {
        id,
        os: std::env::consts::OS.to_string(),
        os_version: System::os_version(),
        kernel_version: System::kernel_version(),
        arch: std::env::consts::ARCH.to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn collect_networks(networks: &Networks, interval_seconds: f64) -> Vec<NetworkSnapshot> {
    // `Networks` retains sysinfo's own enumeration, but the report-facing copy is bounded.
    collect_bounded(
        networks.iter().map(|(name, data)| NetworkSnapshot {
            name: name.clone(),
            received_bytes_total: data.total_received(),
            transmitted_bytes_total: data.total_transmitted(),
            received_bytes_per_second: per_second(data.received(), interval_seconds),
            transmitted_bytes_per_second: per_second(data.transmitted(), interval_seconds),
            packets_received_total: data.total_packets_received(),
            packets_transmitted_total: data.total_packets_transmitted(),
            receive_errors_total: data.total_errors_on_received(),
            transmit_errors_total: data.total_errors_on_transmitted(),
        }),
        CLIENT_REPORT_MAX_NETWORKS,
    )
}

fn collect_disks(disks: &Disks) -> Vec<DiskSnapshot> {
    // `Disks` retains sysinfo's own enumeration, but the report-facing copy is bounded.
    collect_bounded(
        disks.iter().map(|disk| {
            let usage = disk.usage();
            DiskSnapshot {
                name: disk.name().to_string_lossy().into_owned(),
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                read_bytes_total: usage.total_read_bytes,
                written_bytes_total: usage.total_written_bytes,
                // Filled only after the sampler has an earlier observation of this volume.
                read_bytes_per_second: 0.0,
                written_bytes_per_second: 0.0,
                is_read_only: disk.is_read_only(),
            }
        }),
        CLIENT_REPORT_MAX_DISKS,
    )
}

pub(super) fn producer_collection_limit(maximum: usize) -> usize {
    maximum.checked_add(1).unwrap_or(maximum)
}

fn collect_bounded<T>(values: impl IntoIterator<Item = T>, maximum: usize) -> Vec<T> {
    values
        .into_iter()
        .take(producer_collection_limit(maximum))
        .collect()
}

pub(super) fn push_bounded<T>(values: &mut Vec<T>, value: T, maximum: usize) -> bool {
    if values.len() >= producer_collection_limit(maximum) {
        return false;
    }
    values.push(value);
    true
}

pub(super) fn extend_bounded<T>(
    values: &mut Vec<T>,
    additional: impl IntoIterator<Item = T>,
    maximum: usize,
) {
    let remaining = producer_collection_limit(maximum).saturating_sub(values.len());
    values.extend(additional.into_iter().take(remaining));
}

struct TemperatureCollection {
    temperatures: Vec<TemperatureSnapshot>,
    capability: Capability,
}

fn collect_temperatures(_components: &Components) -> TemperatureCollection {
    #[cfg(not(target_os = "linux"))]
    let values = collect_bounded(
        _components.iter().map(|component| TemperatureSnapshot {
            id: component
                .id()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| component.label().to_string()),
            label: component.label().to_string(),
            celsius: component
                .temperature()
                .and_then(|value| finite(value as f64)),
            // sysinfo's max() is the maximum observed by this process, not a
            // hardware threshold. Do not present it as the sensor upper limit.
            max_celsius: None,
            critical_celsius: component.critical().and_then(|value| finite(value as f64)),
            source: "sysinfo-components".to_string(),
        }),
        CLIENT_REPORT_MAX_TEMPERATURES,
    );

    #[cfg(target_os = "linux")]
    let result = linux_hwmon::collect();

    #[cfg(not(target_os = "linux"))]
    let mut seen = HashSet::new();
    #[cfg(not(target_os = "linux"))]
    let mut values = values;

    #[cfg(target_os = "windows")]
    let windows_thermal_error = match windows_gpu::thermal_zone_temperatures() {
        Ok(thermal_zones) => {
            extend_bounded(
                &mut values,
                thermal_zones
                    .into_iter()
                    .map(|(id, label, celsius)| TemperatureSnapshot {
                        id,
                        label,
                        celsius: Some(celsius),
                        max_celsius: None,
                        critical_celsius: None,
                        source: "windows-pdh-thermal-zone".to_string(),
                    }),
                CLIENT_REPORT_MAX_TEMPERATURES,
            );
            None
        }
        Err(error) => Some(error),
    };
    #[cfg(not(target_os = "linux"))]
    values.retain(|item| seen.insert((item.source.clone(), item.id.clone())));

    #[cfg(target_os = "linux")]
    return TemperatureCollection {
        temperatures: result.temperatures,
        capability: result.capability,
    };

    #[cfg(not(target_os = "linux"))]
    TemperatureCollection {
        capability: {
            let capability = temperature_capability(&values);
            #[cfg(target_os = "windows")]
            if !capability.available
                && let Some(error) = windows_thermal_error
            {
                Capability::unavailable(
                    "system.temperature",
                    "sysinfo/windows-pdh-thermal-zone",
                    CapabilityErrorKind::Unsupported,
                    format!(
                        "the operating system exposed no readable numeric sensor; Windows thermal-zone counter: {error}"
                    ),
                )
            } else {
                capability
            }
            #[cfg(not(target_os = "windows"))]
            capability
        },
        temperatures: values,
    }
}

fn temperature_capability(temperatures: &[TemperatureSnapshot]) -> Capability {
    if temperatures.iter().any(|value| value.celsius.is_some()) {
        Capability::available("system.temperature", "sysinfo/hwmon")
    } else {
        Capability::unavailable(
            "system.temperature",
            "sysinfo/hwmon",
            CapabilityErrorKind::Unsupported,
            "the operating system or hardware exposed no readable numeric sensor",
        )
    }
}

fn core_capabilities(temperature: &Capability, disks: &[DiskSnapshot]) -> Vec<Capability> {
    let mut capabilities = vec![
        Capability::available("system.cpu", "sysinfo"),
        Capability::available("system.memory", "sysinfo"),
        Capability::available("system.network", "sysinfo"),
        if disks.is_empty() {
            Capability::unavailable(
                "system.disk",
                "sysinfo-mounted-volumes",
                CapabilityErrorKind::NotPresent,
                "the operating system exposed no mounted volume",
            )
        } else {
            Capability::available("system.disk", "sysinfo-mounted-volumes")
        },
    ];
    capabilities.push(temperature.clone());
    capabilities
}

struct GpuCollection {
    gpus: Vec<GpuSnapshot>,
    capabilities: Vec<Capability>,
    sensors: Vec<crate::model::HardwareSensor>,
    temperatures: Vec<TemperatureSnapshot>,
}

struct GpuRuntime {
    #[cfg(target_os = "macos")]
    inventory: macos_inventory::InventoryCollector,
    #[cfg(all(feature = "nvidia", any(target_os = "linux", target_os = "windows")))]
    nvidia: nvidia::NvidiaCollector,
    #[cfg(target_os = "windows")]
    windows: windows_gpu::WindowsGpuCollector,
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    vendors: windows_vendor::VendorWorker,
}

impl GpuRuntime {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inventory: macos_inventory::InventoryCollector::new(),
            #[cfg(all(feature = "nvidia", any(target_os = "linux", target_os = "windows")))]
            nvidia: nvidia::NvidiaCollector::new(),
            #[cfg(target_os = "windows")]
            windows: windows_gpu::WindowsGpuCollector::new(),
            #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
            vendors: windows_vendor::VendorWorker::new(),
        }
    }

    fn collect(&mut self) -> GpuCollection {
        #[allow(unused_mut)] // macOS baseline build intentionally has no private GPU collector.
        let mut gpus: Vec<GpuSnapshot> = Vec::new();
        let mut capabilities = Vec::new();
        #[allow(unused_mut)]
        let mut sensors = Vec::new();
        #[allow(unused_mut)]
        let mut temperatures = Vec::new();

        #[cfg(all(feature = "nvidia", any(target_os = "linux", target_os = "windows")))]
        {
            let result = self.nvidia.collect();
            extend_bounded(&mut gpus, result.0, CLIENT_REPORT_MAX_GPUS);
            push_bounded(&mut capabilities, result.1, CLIENT_REPORT_MAX_CAPABILITIES);
        }
        #[cfg(not(all(feature = "nvidia", any(target_os = "linux", target_os = "windows"))))]
        push_bounded(
            &mut capabilities,
            Capability::unavailable(
                "gpu.nvidia",
                "nvml",
                CapabilityErrorKind::Unsupported,
                if cfg!(feature = "nvidia") {
                    "NVML telemetry requires Linux or Windows"
                } else {
                    "client was built without the nvidia feature"
                },
            ),
            CLIENT_REPORT_MAX_CAPABILITIES,
        );

        #[cfg(target_os = "linux")]
        {
            let result = linux_gpu::collect();
            extend_bounded(&mut gpus, result.gpus, CLIENT_REPORT_MAX_GPUS);
            extend_bounded(
                &mut capabilities,
                result.capabilities,
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
        }
        #[cfg(target_os = "windows")]
        {
            let result = self.windows.collect();
            let identities =
                if !gpus.is_empty() && result.0.iter().any(|gpu| gpu.vendor == "nvidia") {
                    nvidia_identity::windows_luids()
                } else {
                    Default::default()
                };
            let nvml_luids: Vec<_> = gpus
                .iter()
                .map(|gpu| nvidia_identity::lookup(&identities, &gpu.id))
                .collect();
            if let Some(diagnostic) = gpu_merge::merge_windows(&mut gpus, result.0, &nvml_luids) {
                push_bounded(
                    &mut capabilities,
                    diagnostic,
                    CLIENT_REPORT_MAX_CAPABILITIES,
                );
            }
            push_bounded(&mut capabilities, result.1, CLIENT_REPORT_MAX_CAPABILITIES);
            #[cfg(target_arch = "x86_64")]
            {
                let (vendor_capabilities, vendor_sensors, vendor_temperatures) =
                    self.vendors.collect(&mut gpus);
                extend_bounded(
                    &mut capabilities,
                    vendor_capabilities,
                    CLIENT_REPORT_MAX_CAPABILITIES,
                );
                sensors.extend(vendor_sensors);
                temperatures.extend(vendor_temperatures);
            }
            #[cfg(not(target_arch = "x86_64"))]
            for (name, source) in [
                ("gpu.amd.vendor", "amd-adlx"),
                ("gpu.intel.vendor", "intel-igcl"),
            ] {
                capabilities.push(Capability::unavailable(
                    name,
                    source,
                    CapabilityErrorKind::Unsupported,
                    "vendor telemetry requires Windows x64",
                ));
            }
        }
        #[cfg(target_os = "macos")]
        {
            let (inventory, capability) = self.inventory.poll();
            extend_bounded(&mut gpus, inventory, crate::model::CLIENT_REPORT_MAX_GPUS);
            push_bounded(
                &mut capabilities,
                capability,
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
            extend_bounded(
                &mut capabilities,
                platform_gpu_capabilities("metal/thermal-state"),
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
        }
        GpuCollection {
            gpus,
            capabilities,
            sensors,
            temperatures,
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_gpu_capabilities(source: &str) -> Vec<Capability> {
    let platform = std::env::consts::OS;
    vec![
        Capability::unavailable(
            "gpu.amd",
            source,
            CapabilityErrorKind::Unsupported,
            format!("AMD telemetry is not enabled in the {platform} baseline build"),
        ),
        Capability::unavailable(
            "gpu.intel",
            source,
            CapabilityErrorKind::Unsupported,
            format!("Intel telemetry is not enabled in the {platform} baseline build"),
        ),
        Capability::unavailable(
            "gpu.apple",
            source,
            CapabilityErrorKind::Unsupported,
            "public APIs do not expose stable whole-system Apple GPU utilization",
        ),
    ]
}

/// Clamp measured elapsed time to the server contract interval.
///
/// Keep this function independently testable: `collect()` requires a real `SystemSampler` and two samples
/// separated by enough time, which cannot reliably exercise these boundary conditions.
fn contract_interval_seconds(elapsed_seconds: f64) -> f64 {
    // Clamping does not repair NaN (`f64::clamp` returns NaN); the server
    // `is_finite()` check rejects it with 400. Fall back to the lower bound rather than sending it.
    if !elapsed_seconds.is_finite() {
        return crate::config::MIN_REPORT_INTERVAL_SECONDS;
    }
    elapsed_seconds.clamp(
        crate::config::MIN_REPORT_INTERVAL_SECONDS,
        crate::config::MAX_REPORT_INTERVAL_SECONDS as f64,
    )
}

fn per_second(delta: u64, interval_seconds: f64) -> f64 {
    u64_as_f64(delta) / interval_seconds.max(0.001)
}

/// Convert a platform-sized CPU count to the fixed-width wire type without an unchecked cast.
/// Saturation is only a defensive fallback: no supported kernel can expose `u32::MAX` CPUs.
fn wire_cpu_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Convert a counter to the protocol's floating-point rate domain without narrowing through `as`.
/// Values above 2^53 are necessarily rounded by IEEE-754, but remain finite and monotonic.
fn u64_as_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).expect("upper u64 half always fits u32");
    let low = u32::try_from(value & u64::from(u32::MAX)).expect("lower u64 half always fits u32");
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn sampler_initialization_does_not_enumerate_processes() {
        let sampler = SystemSampler::new();
        assert!(sampler.system.processes().is_empty());
        if sysinfo::IS_SUPPORTED_SYSTEM {
            assert!(
                !sampler.system.cpus().is_empty(),
                "the narrow refresh policy must still initialize CPU telemetry"
            );
        }
    }

    #[test]
    fn rate_uses_actual_interval() {
        assert_eq!(per_second(1_000, 2.0), 500.0);
        assert_eq!(per_second(5, 2.0), 2.5);
    }

    // macOS exposes volume I/O for this fixture; Linux temporary files may live on tmpfs.
    #[cfg(target_os = "macos")]
    #[test]
    fn sampler_rates_use_elapsed_time_beyond_the_report_interval_limit() {
        use std::io::Write;

        let mut sampler = SystemSampler::with_smart_config(smart::SmartConfig {
            enabled: false,
            ..Default::default()
        });
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![1; 8 * 1024 * 1024]).unwrap();
        file.sync_all().unwrap();
        let previous = Instant::now() - std::time::Duration::from_secs(7_200);
        sampler.last_sample = previous;
        let report = sampler.collect(transient_host_identity(Uuid::new_v4()), 30, 0);
        let elapsed = sampler.last_sample.duration_since(previous).as_secs_f64();
        assert_eq!(
            report.interval_seconds,
            crate::config::MAX_REPORT_INTERVAL_SECONDS as f64
        );

        let mut observed_io = false;
        for disk in &report.system.disks {
            let usage = sampler
                .disks
                .iter()
                .find(|native| native.mount_point().to_string_lossy() == disk.mount_point)
                .unwrap()
                .usage();
            observed_io |= usage.read_bytes > 0 || usage.written_bytes > 0;
            assert_eq!(
                disk.read_bytes_per_second,
                per_second(usage.read_bytes, elapsed)
            );
            assert_eq!(
                disk.written_bytes_per_second,
                per_second(usage.written_bytes, elapsed)
            );
        }
        assert!(observed_io, "the fixture must exercise a nonzero disk rate");
        for network in &report.system.networks {
            let native = &sampler.networks[&network.name];
            assert_eq!(
                network.received_bytes_per_second,
                per_second(native.received(), elapsed)
            );
            assert_eq!(
                network.transmitted_bytes_per_second,
                per_second(native.transmitted(), elapsed)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_nvidia_capability_reports_unsupported_without_a_driver_error() {
        let result = GpuRuntime::new().collect();
        let nvidia = result
            .capabilities
            .iter()
            .find(|item| item.name == "gpu.nvidia")
            .unwrap();
        assert!(!nvidia.available);
        assert_eq!(nvidia.error_kind, Some(CapabilityErrorKind::Unsupported));
        assert!(!nvidia.message.as_deref().unwrap().contains("nvml.dll"));
    }

    #[test]
    fn native_cpu_counts_use_a_checked_fixed_width_conversion() {
        assert_eq!(wire_cpu_count(8), 8);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(wire_cpu_count(usize::MAX), u32::MAX);
    }

    #[test]
    fn empty_temperature_input_is_reported_as_a_capability_gap() {
        let temperature_capability = temperature_capability(&[]);
        let temperature = core_capabilities(&temperature_capability, &[])
            .into_iter()
            .find(|capability| capability.name == "system.temperature")
            .expect("core capabilities always describe temperature support");

        assert_eq!(
            temperature,
            Capability::unavailable(
                "system.temperature",
                "sysinfo/hwmon",
                CapabilityErrorKind::Unsupported,
                "the operating system or hardware exposed no readable numeric sensor",
            )
        );
    }

    #[test]
    fn disk_capability_requires_an_enumerated_volume() {
        let temperature_capability = temperature_capability(&[]);
        let disk = core_capabilities(&temperature_capability, &[])
            .into_iter()
            .find(|capability| capability.name == "system.disk")
            .expect("core capabilities always describe mounted-volume support");
        assert_eq!(
            disk,
            Capability::unavailable(
                "system.disk",
                "sysinfo-mounted-volumes",
                CapabilityErrorKind::NotPresent,
                "the operating system exposed no mounted volume",
            )
        );
    }

    #[test]
    fn producer_limit_keeps_one_checked_truncation_sentinel() {
        assert_eq!(producer_collection_limit(7), 8);
        assert_eq!(producer_collection_limit(usize::MAX), usize::MAX);

        let consumed = Cell::new(0);
        let values = collect_bounded(
            (0..).inspect(|_| consumed.set(consumed.get() + 1)),
            CLIENT_REPORT_MAX_CPU_CORES,
        );
        assert_eq!(values.len(), CLIENT_REPORT_MAX_CPU_CORES + 1);
        assert_eq!(consumed.get(), CLIENT_REPORT_MAX_CPU_CORES + 1);
    }

    #[test]
    fn producer_sentinel_is_bounded_and_reported_by_the_wire_contract() {
        let per_core_percent = collect_bounded(
            std::iter::repeat_n(10.0, CLIENT_REPORT_MAX_CPU_CORES + 2),
            CLIENT_REPORT_MAX_CPU_CORES,
        );
        assert_eq!(per_core_percent.len(), CLIENT_REPORT_MAX_CPU_CORES + 1);

        let mut report = ClientReport {
            schema_version: CLIENT_REPORT_SCHEMA_VERSION,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                os: "linux".into(),
                os_version: None,
                kernel_version: None,
                arch: "x86_64".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                hardware: None,
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 10.0,
                    logical_count: wire_cpu_count(per_core_percent.len()),
                    physical_count: None,
                    per_core_percent,
                },
                memory: MemorySnapshot {
                    total_bytes: 1,
                    used_bytes: 0,
                    available_bytes: 1,
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

        assert!(crate::report_contract::bound_report(&mut report));
        assert_eq!(
            report.system.cpu.per_core_percent.len(),
            CLIENT_REPORT_MAX_CPU_CORES
        );
        assert!(
            report
                .capabilities
                .iter()
                .any(|capability| capability.name == "client.report.truncated")
        );
    }

    /// The report `interval_seconds` must always remain within the server contract interval.
    ///
    /// Even after ticker delays or resume from sleep, keep the reported interval within the contract
    /// so the server accepts and persists the sample.
    #[test]
    fn the_reported_interval_always_satisfies_the_server_contract() {
        use crate::config::{MAX_REPORT_INTERVAL_SECONDS, MIN_REPORT_INTERVAL_SECONDS};
        let max = MAX_REPORT_INTERVAL_SECONDS as f64;

        for elapsed in [
            0.0,
            0.001,
            0.05,        // jitter shortens the cycle too far
            10.0,        // ordinary cycle
            max,         // exactly at the upper bound
            max + 0.001, // ticker delay exceeds the upper bound slightly
            5_400.0,     // interval=3600 with 50% jitter
            86_400.0,    // resume after a day asleep
            f64::INFINITY,
            f64::NAN,
        ] {
            let reported = contract_interval_seconds(elapsed);
            assert!(
                reported.is_finite() && (MIN_REPORT_INTERVAL_SECONDS..=max).contains(&reported),
                "elapsed={elapsed} produced out-of-range interval_seconds={reported},\
                 which the server would reject with 400 and discard"
            );
        }

        // Values within the interval must pass through unchanged; clamping must preserve normal readings.
        assert_eq!(contract_interval_seconds(10.0), 10.0);
    }
}
