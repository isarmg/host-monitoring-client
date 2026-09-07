use std::{path::Path, time::Instant};

#[cfg(not(target_os = "linux"))]
use std::collections::HashSet;

use chrono::Utc;
use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System,
};
use uuid::Uuid;

#[cfg(any(feature = "nvidia", target_os = "linux", target_os = "windows"))]
use crate::model::CLIENT_REPORT_MAX_GPUS;
use crate::model::{
    CLIENT_REPORT_MAX_CAPABILITIES, CLIENT_REPORT_MAX_CPU_CORES, CLIENT_REPORT_MAX_DISKS,
    CLIENT_REPORT_MAX_NETWORKS, CLIENT_REPORT_MAX_TEMPERATURES, CLIENT_REPORT_SCHEMA_VERSION,
    Capability, CapabilityErrorKind, ClientHealth, ClientReport, CpuSnapshot, DiskSnapshot,
    GpuSnapshot, HostIdentity, MemorySnapshot, NetworkSnapshot, SystemSnapshot,
    TemperatureSnapshot,
};
#[cfg(target_os = "linux")]
mod linux_gpu;
#[cfg(target_os = "linux")]
mod linux_hwmon;
#[cfg(feature = "nvidia")]
mod nvidia;
#[cfg(any(target_os = "windows", test))]
mod pdh_buffer;
#[cfg(any(target_os = "windows", test))]
mod pdh_recovery;
#[cfg(target_os = "windows")]
mod windows_gpu;

/// 长期复用 sysinfo 对象，避免反复枚举系统并确保差值指标有正确采样基线。
pub struct SystemSampler {
    system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    last_sample: Instant,
    last_slow_sample: Option<Instant>,
    cached_temperatures: Vec<TemperatureSnapshot>,
    cached_temperature_capability: Capability,
    gpu_runtime: GpuRuntime,
}

impl SystemSampler {
    pub fn new() -> Self {
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
            disks: Disks::new_with_refreshed_list(),
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
            gpu_runtime: GpuRuntime::new(),
        }
    }

    pub fn collect(
        &mut self,
        host: HostIdentity,
        slow_interval_seconds: u64,
        spool_pending_batches: u64,
    ) -> ClientReport {
        let now = Instant::now();
        // 钳到服务端契约区间之内。**两端都要钳**，理由完全对称：落在区间之外的
        // `interval_seconds` 会被服务端判为 400（永久拒绝），投递 worker 随后会把
        // 这份必失败报文从 spool 确认丢弃——一次不可恢复的数据缺口。
        //
        // 下限：主循环的 ticker 有 jitter，理论上可以短到 0.5 秒以下。
        // 上限：run 模式已把网络投递与补传移到独立 worker，正常采样周期只由 ticker
        //       决定。配置 3600 秒时，10% 默认 jitter 仍能推到 3960 秒，因此
        //       `ClientConfig::validate()` 会在启动时拒绝。这里额外守的是配置检查覆盖不到
        //       的暂停：机器休眠唤醒、进程被 SIGSTOP 挂起、宿主机时钟源异常。
        //
        // 被钳住时速率会偏高（真实窗口更长，除数却按上限算），这与下限处"宁可略微
        // 保守"是同一个取舍：一份读数略有偏差的报文，好过一份凭空消失的采样。
        let interval_seconds =
            contract_interval_seconds(now.duration_since(self.last_sample).as_secs_f64());
        self.last_sample = now;

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);

        let refresh_slow = self
            .last_slow_sample
            .is_none_or(|last| now.duration_since(last).as_secs() >= slow_interval_seconds);
        if refresh_slow {
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

        let (gpus, gpu_capabilities) = self.gpu_runtime.collect();
        let mut capabilities = core_capabilities(&self.cached_temperature_capability);
        extend_bounded(
            &mut capabilities,
            gpu_capabilities,
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
                networks: collect_networks(&self.networks, interval_seconds),
                disks: collect_disks(&self.disks, interval_seconds),
                temperatures: collect_bounded(
                    self.cached_temperatures.iter().cloned(),
                    CLIENT_REPORT_MAX_TEMPERATURES,
                ),
                gpus,
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

fn collect_disks(disks: &Disks, interval_seconds: f64) -> Vec<DiskSnapshot> {
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
                read_bytes_per_second: per_second(usage.read_bytes, interval_seconds),
                written_bytes_per_second: per_second(usage.written_bytes, interval_seconds),
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
    #[cfg(not(target_os = "linux"))]
    values.retain(|item| seen.insert((item.source.clone(), item.id.clone())));

    #[cfg(target_os = "linux")]
    return TemperatureCollection {
        temperatures: result.temperatures,
        capability: result.capability,
    };

    #[cfg(not(target_os = "linux"))]
    TemperatureCollection {
        capability: temperature_capability(&values),
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

fn core_capabilities(temperature: &Capability) -> Vec<Capability> {
    let mut capabilities = vec![
        Capability::available("system.cpu", "sysinfo"),
        Capability::available("system.memory", "sysinfo"),
        Capability::available("system.network", "sysinfo"),
        Capability::available("system.disk", "sysinfo"),
    ];
    capabilities.push(temperature.clone());
    capabilities
}

struct GpuRuntime {
    #[cfg(feature = "nvidia")]
    nvidia: nvidia::NvidiaCollector,
    #[cfg(target_os = "windows")]
    windows: windows_gpu::WindowsGpuCollector,
}

impl GpuRuntime {
    fn new() -> Self {
        Self {
            #[cfg(feature = "nvidia")]
            nvidia: nvidia::NvidiaCollector::new(),
            #[cfg(target_os = "windows")]
            windows: windows_gpu::WindowsGpuCollector::new(),
        }
    }

    fn collect(&mut self) -> (Vec<GpuSnapshot>, Vec<Capability>) {
        #[allow(unused_mut)] // macOS baseline build intentionally has no private GPU collector.
        let mut gpus = Vec::new();
        let mut capabilities = Vec::new();

        #[cfg(feature = "nvidia")]
        {
            let result = self.nvidia.collect();
            extend_bounded(&mut gpus, result.0, CLIENT_REPORT_MAX_GPUS);
            push_bounded(&mut capabilities, result.1, CLIENT_REPORT_MAX_CAPABILITIES);
        }
        #[cfg(not(feature = "nvidia"))]
        push_bounded(
            &mut capabilities,
            Capability::unavailable(
                "gpu.nvidia",
                "nvml",
                CapabilityErrorKind::Unsupported,
                "client was built without the nvidia feature",
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
            extend_bounded(&mut gpus, result.0, CLIENT_REPORT_MAX_GPUS);
            push_bounded(&mut capabilities, result.1, CLIENT_REPORT_MAX_CAPABILITIES);
            push_bounded(
                &mut capabilities,
                Capability::unavailable(
                    "gpu.amd.vendor",
                    "amd-adlx",
                    CapabilityErrorKind::Unsupported,
                    "ADLX enrichment is not present; WDDM utilization remains available",
                ),
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
            push_bounded(
                &mut capabilities,
                Capability::unavailable(
                    "gpu.intel.vendor",
                    "intel-igcl",
                    CapabilityErrorKind::Unsupported,
                    "IGCL enrichment is not present; WDDM utilization remains available",
                ),
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
        }
        #[cfg(target_os = "macos")]
        {
            extend_bounded(
                &mut capabilities,
                platform_gpu_capabilities("metal/thermal-state"),
                CLIENT_REPORT_MAX_CAPABILITIES,
            );
        }
        (gpus, capabilities)
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

/// 把实测经过时间收敛到服务端契约区间之内。
///
/// 抽成独立函数是为了可测：`collect()` 需要一个真实的 `SystemSampler` 和两次相隔
/// 足够久的采样，无法用它验证边界行为，而这里守的恰恰是边界。
fn contract_interval_seconds(elapsed_seconds: f64) -> f64 {
    // NaN 不会被 clamp 修正（`f64::clamp` 在 NaN 上返回 NaN），而服务端的
    // `is_finite()` 检查会把它判成 400。回退到下限而不是原样传出去。
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

    #[test]
    fn native_cpu_counts_use_a_checked_fixed_width_conversion() {
        assert_eq!(wire_cpu_count(8), 8);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(wire_cpu_count(usize::MAX), u32::MAX);
    }

    #[test]
    fn empty_temperature_input_is_reported_as_a_capability_gap() {
        let temperature_capability = temperature_capability(&[]);
        let temperature = core_capabilities(&temperature_capability)
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

    /// 报文里的 `interval_seconds` 必须**始终**落在服务端契约区间内。
    ///
    /// 回归：此前只钳了下限。区间之外的值会被服务端判为 400（永久拒绝），投递
    /// worker 随后会把它从 spool 确认丢弃——一次不可恢复的数据缺口。
    #[test]
    fn the_reported_interval_always_satisfies_the_server_contract() {
        use crate::config::{MAX_REPORT_INTERVAL_SECONDS, MIN_REPORT_INTERVAL_SECONDS};
        let max = MAX_REPORT_INTERVAL_SECONDS as f64;

        for elapsed in [
            0.0,
            0.001,
            0.05,        // jitter 把周期压得过短
            10.0,        // 常规
            max,         // 恰好在上限
            max + 0.001, // ticker 调度延迟把周期略微推出上限
            5_400.0,     // interval=3600 配 50% jitter
            86_400.0,    // 休眠一天后唤醒
            f64::INFINITY,
            f64::NAN,
        ] {
            let reported = contract_interval_seconds(elapsed);
            assert!(
                reported.is_finite() && (MIN_REPORT_INTERVAL_SECONDS..=max).contains(&reported),
                "elapsed={elapsed} 产出了越界的 interval_seconds={reported}，\
                 服务端会以 400 拒绝并丢弃该报文"
            );
        }

        // 区间之内的值必须原样透传，clamp 不该改动正常读数。
        assert_eq!(contract_interval_seconds(10.0), 10.0);
    }
}
