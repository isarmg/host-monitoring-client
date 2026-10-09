use nvml_wrapper::{
    Nvml,
    enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor},
    error::NvmlError,
};
use std::time::{Duration, Instant};

use crate::model::{CLIENT_REPORT_MAX_GPUS, Capability, CapabilityErrorKind, GpuSnapshot};

use super::{producer_collection_limit, push_bounded};

const NVML_INIT_RETRY_INITIAL: Duration = Duration::from_secs(30);
const NVML_INIT_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

struct RetryingInit<T, E> {
    value: Option<T>,
    last_error: Option<E>,
    next_retry_at: Option<Instant>,
    retry_delay: Duration,
}

impl<T, E> RetryingInit<T, E> {
    fn new() -> Self {
        Self {
            value: None,
            last_error: None,
            next_retry_at: None,
            retry_delay: NVML_INIT_RETRY_INITIAL,
        }
    }

    fn initialize_if_due(&mut self, now: Instant, init: impl FnOnce() -> Result<T, E>) {
        if self.value.is_none() {
            if self.next_retry_at.is_some_and(|retry_at| now < retry_at) {
                return;
            }

            match init() {
                Ok(value) => {
                    self.value = Some(value);
                    self.last_error = None;
                    self.next_retry_at = None;
                    self.retry_delay = NVML_INIT_RETRY_INITIAL;
                }
                Err(error) => {
                    self.last_error = Some(error);
                    self.next_retry_at = now.checked_add(self.retry_delay);
                    self.retry_delay = self.retry_delay.saturating_mul(2).min(NVML_INIT_RETRY_MAX);
                }
            }
        }
    }

    fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    fn last_error(&self) -> Option<&E> {
        self.last_error.as_ref()
    }
}

pub(super) struct NvidiaCollector {
    nvml: RetryingInit<Nvml, NvmlError>,
}

impl NvidiaCollector {
    pub fn new() -> Self {
        let mut nvml = RetryingInit::new();
        nvml.initialize_if_due(Instant::now(), Nvml::init);
        Self { nvml }
    }

    pub fn collect(&mut self) -> (Vec<GpuSnapshot>, Capability) {
        self.nvml.initialize_if_due(Instant::now(), Nvml::init);
        let Some(nvml) = self.nvml.value() else {
            let capability = self.nvml.last_error().map_or_else(
                || {
                    Capability::unavailable(
                        "gpu.nvidia",
                        "nvml",
                        CapabilityErrorKind::Transient,
                        "NVML initialization has not completed",
                    )
                },
                nvml_error_capability,
            );
            return (Vec::new(), capability);
        };
        let count = match nvml.device_count() {
            Ok(count) => count,
            Err(error) => {
                return (Vec::new(), nvml_error_capability(&error));
            }
        };
        if count == 0 {
            return (
                Vec::new(),
                Capability::unavailable(
                    "gpu.nvidia",
                    "nvml",
                    CapabilityErrorKind::NotPresent,
                    "NVML reported no NVIDIA devices",
                ),
            );
        }

        let mut gpus = Vec::new();
        let mut first_failed_device_error = None;
        let inspected_device_count = count.min(
            u32::try_from(producer_collection_limit(CLIENT_REPORT_MAX_GPUS))
                .expect("the shared GPU report limit fits NVML's fixed-width device index"),
        );
        for index in 0..inspected_device_count {
            let device = match nvml.device_by_index(index) {
                Ok(device) => device,
                Err(error) => {
                    first_failed_device_error.get_or_insert(error);
                    continue;
                }
            };

            let mut first_telemetry_error = None;
            let utilization =
                retain_nvml_result(device.utilization_rates(), &mut first_telemetry_error);
            let memory = retain_nvml_result(device.memory_info(), &mut first_telemetry_error);
            let telemetry = NvidiaTelemetry {
                utilization_percent: utilization.as_ref().map(|value| value.gpu as f64),
                memory_total_bytes: memory.as_ref().map(|value| value.total),
                memory_used_bytes: memory.as_ref().map(|value| value.used),
                temperature_celsius: retain_nvml_result(
                    device.temperature(TemperatureSensor::Gpu),
                    &mut first_telemetry_error,
                )
                .map(|value| value as f64),
                power_watts: retain_nvml_result(device.power_usage(), &mut first_telemetry_error)
                    .map(|milliwatts| milliwatts as f64 / 1_000.0),
                core_clock_mhz: retain_nvml_result(
                    device.clock_info(Clock::Graphics),
                    &mut first_telemetry_error,
                )
                .map(f64::from),
                memory_clock_mhz: retain_nvml_result(
                    device.clock_info(Clock::Memory),
                    &mut first_telemetry_error,
                )
                .map(f64::from),
                pcie_rx_bytes_per_second: retain_nvml_result(
                    device.pcie_throughput(PcieUtilCounter::Receive),
                    &mut first_telemetry_error,
                )
                .map(|kilobytes| f64::from(kilobytes) * 1024.0),
                pcie_tx_bytes_per_second: retain_nvml_result(
                    device.pcie_throughput(PcieUtilCounter::Send),
                    &mut first_telemetry_error,
                )
                .map(|kilobytes| f64::from(kilobytes) * 1024.0),
            };
            if !telemetry.has_substantive_value() {
                if first_failed_device_error.is_none() {
                    first_failed_device_error = first_telemetry_error;
                }
                continue;
            }

            push_bounded(
                &mut gpus,
                telemetry.into_snapshot(
                    device.uuid().unwrap_or_else(|_| format!("nvidia-{index}")),
                    device
                        .name()
                        .unwrap_or_else(|_| format!("NVIDIA GPU {index}")),
                ),
                CLIENT_REPORT_MAX_GPUS,
            );
        }
        finish_nvidia_collection(gpus, count, first_failed_device_error.as_ref())
    }
}

#[derive(Default)]
struct NvidiaTelemetry {
    utilization_percent: Option<f64>,
    memory_total_bytes: Option<u64>,
    memory_used_bytes: Option<u64>,
    temperature_celsius: Option<f64>,
    power_watts: Option<f64>,
    core_clock_mhz: Option<f64>,
    memory_clock_mhz: Option<f64>,
    pcie_rx_bytes_per_second: Option<f64>,
    pcie_tx_bytes_per_second: Option<f64>,
}

impl NvidiaTelemetry {
    fn has_substantive_value(&self) -> bool {
        self.utilization_percent.is_some()
            || self.memory_total_bytes.is_some()
            || self.memory_used_bytes.is_some()
            || self.temperature_celsius.is_some()
            || self.power_watts.is_some()
            || self.core_clock_mhz.is_some()
            || self.memory_clock_mhz.is_some()
            || self.pcie_rx_bytes_per_second.is_some()
            || self.pcie_tx_bytes_per_second.is_some()
    }

    fn into_snapshot(self, id: String, name: String) -> GpuSnapshot {
        GpuSnapshot {
            id,
            vendor: "nvidia".to_string(),
            name,
            utilization_percent: self.utilization_percent,
            memory_total_bytes: self.memory_total_bytes,
            memory_used_bytes: self.memory_used_bytes,
            temperature_celsius: self.temperature_celsius,
            power_watts: self.power_watts,
            core_clock_mhz: self.core_clock_mhz,
            memory_clock_mhz: self.memory_clock_mhz,
            pcie_rx_bytes_per_second: self.pcie_rx_bytes_per_second,
            pcie_tx_bytes_per_second: self.pcie_tx_bytes_per_second,
            source: "nvml".to_string(),
        }
    }
}

fn retain_nvml_result<T>(
    result: Result<T, NvmlError>,
    first_error: &mut Option<NvmlError>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            if first_error.is_none() {
                *first_error = Some(error);
            }
            None
        }
    }
}

fn finish_nvidia_collection(
    gpus: Vec<GpuSnapshot>,
    expected_devices: u32,
    first_error: Option<&NvmlError>,
) -> (Vec<GpuSnapshot>, Capability) {
    if u32::try_from(gpus.len()) == Ok(expected_devices) {
        return (gpus, Capability::available("gpu.nvidia", "nvml"));
    }

    let message = format!(
        "NVML collected substantive telemetry for {} of {expected_devices} NVIDIA devices",
        gpus.len()
    );
    let capability = first_error.map_or_else(
        || {
            Capability::unavailable(
                "gpu.nvidia",
                "nvml",
                CapabilityErrorKind::InvalidData,
                format!("{message}; no NVML error explained the missing telemetry"),
            )
        },
        |error| {
            Capability::unavailable(
                "gpu.nvidia",
                "nvml",
                classify_nvml_error(error),
                format!("{message}; first error: {error}"),
            )
        },
    );
    (gpus, capability)
}

fn nvml_error_capability(error: &NvmlError) -> Capability {
    Capability::unavailable(
        "gpu.nvidia",
        "nvml",
        classify_nvml_error(error),
        error.to_string(),
    )
}

#[allow(deprecated)]
fn classify_nvml_error(error: &NvmlError) -> CapabilityErrorKind {
    match error {
        NvmlError::NoPermission | NvmlError::OperatingSystem => {
            CapabilityErrorKind::PermissionDenied
        }
        NvmlError::LibloadingError(_)
        | NvmlError::DriverNotLoaded
        | NvmlError::LibraryNotFound
        | NvmlError::LibRmVersionMismatch => CapabilityErrorKind::DriverMissing,
        NvmlError::FailedToLoadSymbol(_)
        | NvmlError::NotSupported
        | NvmlError::FunctionNotFound
        | NvmlError::VgpuEccNotSupported => CapabilityErrorKind::Unsupported,
        NvmlError::NotFound | NvmlError::NoData => CapabilityErrorKind::NotPresent,
        NvmlError::Utf8Error(_)
        | NvmlError::NulError(_)
        | NvmlError::StringTooLong { .. }
        | NvmlError::IncorrectBits(_)
        | NvmlError::UnexpectedVariant(_)
        | NvmlError::PciInfoToCFailed
        | NvmlError::InvalidArg
        | NvmlError::InsufficientSize(_)
        | NvmlError::CorruptedInfoROM => CapabilityErrorKind::InvalidData,
        NvmlError::SetReleaseFailed
        | NvmlError::GetPciInfoFailed
        | NvmlError::Uninitialized
        | NvmlError::AlreadyInitialized
        | NvmlError::InsufficientPower
        | NvmlError::Timeout
        | NvmlError::IrqIssue
        | NvmlError::GpuLost
        | NvmlError::ResetRequired
        | NvmlError::InUse
        | NvmlError::InsufficientMemory
        | NvmlError::Unknown => CapabilityErrorKind::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_initialization_retries_after_backoff_and_reuses_success() {
        let started_at = Instant::now();
        let mut attempts = 0;
        let mut state = RetryingInit::<usize, &str>::new();

        state.initialize_if_due(started_at, || {
            attempts += 1;
            Err("driver unavailable")
        });
        assert!(state.value().is_none());
        assert_eq!(state.last_error(), Some(&"driver unavailable"));
        assert_eq!(attempts, 1);

        state.initialize_if_due(
            started_at + NVML_INIT_RETRY_INITIAL - Duration::from_millis(1),
            || {
                attempts += 1;
                Ok(7)
            },
        );
        assert!(state.value().is_none());
        assert_eq!(state.last_error(), Some(&"driver unavailable"));
        assert_eq!(attempts, 1);

        state.initialize_if_due(started_at + NVML_INIT_RETRY_INITIAL, || {
            attempts += 1;
            Ok(7)
        });
        assert_eq!(state.value(), Some(&7));
        assert_eq!(state.last_error(), None);
        assert_eq!(attempts, 2);

        state.initialize_if_due(
            started_at + NVML_INIT_RETRY_INITIAL * 2,
            || -> Result<usize, &str> {
                attempts += 1;
                panic!("a successful initialization must be reused")
            },
        );
        assert_eq!(state.value(), Some(&7));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn initialization_retry_delay_is_bounded() {
        let mut now = Instant::now();
        let mut state = RetryingInit::<(), &str>::new();

        for _ in 0..16 {
            let attempted_delay = state.retry_delay;
            state.initialize_if_due(now, || Err("still unavailable"));
            assert!(state.value().is_none());
            assert!(state.retry_delay <= NVML_INIT_RETRY_MAX);
            now += attempted_delay;
        }

        assert_eq!(state.retry_delay, NVML_INIT_RETRY_MAX);
    }

    #[test]
    fn nvml_errors_map_to_actionable_capability_kinds() {
        let cases = [
            (
                NvmlError::NoPermission,
                CapabilityErrorKind::PermissionDenied,
            ),
            (
                NvmlError::OperatingSystem,
                CapabilityErrorKind::PermissionDenied,
            ),
            (
                NvmlError::DriverNotLoaded,
                CapabilityErrorKind::DriverMissing,
            ),
            (
                NvmlError::LibraryNotFound,
                CapabilityErrorKind::DriverMissing,
            ),
            (
                NvmlError::LibRmVersionMismatch,
                CapabilityErrorKind::DriverMissing,
            ),
            (NvmlError::NotSupported, CapabilityErrorKind::Unsupported),
            (
                NvmlError::FunctionNotFound,
                CapabilityErrorKind::Unsupported,
            ),
            (NvmlError::NotFound, CapabilityErrorKind::NotPresent),
            (NvmlError::NoData, CapabilityErrorKind::NotPresent),
            (NvmlError::InvalidArg, CapabilityErrorKind::InvalidData),
            (
                NvmlError::CorruptedInfoROM,
                CapabilityErrorKind::InvalidData,
            ),
            (NvmlError::Timeout, CapabilityErrorKind::Transient),
            (NvmlError::GpuLost, CapabilityErrorKind::Transient),
            (NvmlError::Unknown, CapabilityErrorKind::Transient),
        ];

        for (error, expected) in cases {
            assert_eq!(classify_nvml_error(&error), expected, "{error}");
        }

        let capability = nvml_error_capability(&NvmlError::NoPermission);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::PermissionDenied)
        );
        assert_eq!(
            capability.message.as_deref(),
            Some("the current user does not have permission to perform this operation")
        );
    }

    #[test]
    fn snapshot_requires_substantive_telemetry_and_preserves_first_field_error() {
        assert!(!NvidiaTelemetry::default().has_substantive_value());
        assert!(
            NvidiaTelemetry {
                power_watts: Some(0.0),
                ..NvidiaTelemetry::default()
            }
            .has_substantive_value()
        );

        let mut first_error = None;
        assert!(
            retain_nvml_result::<u32>(Err(NvmlError::NoPermission), &mut first_error).is_none()
        );
        assert!(retain_nvml_result::<u32>(Err(NvmlError::Timeout), &mut first_error).is_none());
        assert!(matches!(first_error, Some(NvmlError::NoPermission)));
    }

    #[test]
    fn partial_collection_keeps_snapshots_but_is_not_fully_available() {
        let snapshot = NvidiaTelemetry {
            utilization_percent: Some(42.0),
            ..NvidiaTelemetry::default()
        }
        .into_snapshot("gpu-0".to_string(), "GPU 0".to_string());

        let (gpus, capability) =
            finish_nvidia_collection(vec![snapshot.clone()], 2, Some(&NvmlError::NoPermission));
        assert_eq!(gpus, vec![snapshot]);
        assert!(!capability.available);
        assert_eq!(
            capability.error_kind,
            Some(CapabilityErrorKind::PermissionDenied)
        );
        assert!(
            capability
                .message
                .as_deref()
                .is_some_and(|message| message.contains("1 of 2"))
        );
    }

    #[test]
    fn complete_and_unexplained_empty_collections_have_distinct_capabilities() {
        let snapshot = NvidiaTelemetry {
            temperature_celsius: Some(55.0),
            ..NvidiaTelemetry::default()
        }
        .into_snapshot("gpu-0".to_string(), "GPU 0".to_string());
        let (_, complete) = finish_nvidia_collection(vec![snapshot], 1, None);
        assert!(complete.available);

        let (gpus, failed) =
            finish_nvidia_collection(Vec::new(), 2, Some(&NvmlError::NotSupported));
        assert!(gpus.is_empty());
        assert!(!failed.available);
        assert_eq!(failed.error_kind, Some(CapabilityErrorKind::Unsupported));

        let (gpus, unexplained) = finish_nvidia_collection(Vec::new(), 1, None);
        assert!(gpus.is_empty());
        assert!(!unexplained.available);
        assert_eq!(
            unexplained.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }
}
