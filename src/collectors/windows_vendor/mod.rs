//! Driver-local, read-only AMD/Intel telemetry. There is no control or command interface.
mod adlx;
mod adlx_abi;
mod igcl;
mod igcl_abi;
mod library;

use crate::model::{
    Capability, CapabilityErrorKind, GpuSnapshot, HardwareSensor, SensorKind, TemperatureSnapshot,
};
use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
struct Failure {
    kind: CapabilityErrorKind,
    message: String,
}
impl Failure {
    fn new(kind: CapabilityErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
#[derive(Clone, Default)]
struct VendorResult {
    readings: Vec<Reading>,
    failure: Option<Failure>,
}
#[derive(Clone)]
struct Reading {
    gpu: GpuSnapshot,
    sensors: Vec<HardwareSensor>,
    temperatures: Vec<TemperatureSnapshot>,
}
impl Reading {
    fn new(id: String, vendor: &str, name: String, source: &str) -> Self {
        Self {
            gpu: GpuSnapshot {
                id,
                vendor: vendor.into(),
                name,
                source: source.into(),
                utilization_percent: None,
                memory_total_bytes: None,
                memory_used_bytes: None,
                temperature_celsius: None,
                power_watts: None,
                core_clock_mhz: None,
                memory_clock_mhz: None,
                pcie_rx_bytes_per_second: None,
                pcie_tx_bytes_per_second: None,
            },
            sensors: vec![],
            temperatures: vec![],
        }
    }
    fn fan(&mut self, channel: &str, value: Option<f64>) {
        self.sensor(channel, SensorKind::FanRpm, value);
    }
    fn voltage(&mut self, channel: &str, value: Option<f64>) {
        self.sensor(channel, SensorKind::VoltageVolts, value);
    }
    fn sensor(&mut self, channel: &str, kind: SensorKind, value: Option<f64>) {
        if let Some(value) = value.and_then(nonnegative) {
            self.sensors.push(HardwareSensor {
                id: format!("{}:{channel}", self.gpu.id),
                label: format!("{} {channel}", self.gpu.name),
                kind,
                value,
                source: self.gpu.source.clone(),
            });
        }
    }
    fn temperature(&mut self, channel: &str, value: Option<f64>) {
        if let Some(celsius) = value.filter(|v| v.is_finite() && (-273.15..=1000.0).contains(v)) {
            self.temperatures.push(TemperatureSnapshot {
                id: format!("{}:{channel}", self.gpu.id),
                label: format!("{} {channel}", self.gpu.name),
                celsius: Some(celsius),
                max_celsius: None,
                critical_celsius: None,
                source: self.gpu.source.clone(),
            });
        }
    }
    fn has_telemetry(&self) -> bool {
        [
            self.gpu.utilization_percent,
            self.gpu.temperature_celsius,
            self.gpu.power_watts,
            self.gpu.core_clock_mhz,
            self.gpu.memory_clock_mhz,
        ]
        .iter()
        .any(Option::is_some)
            || self.gpu.memory_used_bytes.is_some()
            || !self.sensors.is_empty()
            || !self.temperatures.is_empty()
    }
}
fn nonnegative(v: f64) -> Option<f64> {
    (v.is_finite() && v >= 0.0).then_some(v)
}
fn luid_id(low: u32, high: i32) -> String {
    format!("luid_{:08x}_{low:08x}", high as u32)
}

trait Provider: Sized {
    fn open() -> Result<Self, Failure>;
    fn sample(&mut self) -> Result<VendorResult, Failure>;
}
impl Provider for adlx::Adlx {
    fn open() -> Result<Self, Failure> {
        Self::new()
    }
    fn sample(&mut self) -> Result<VendorResult, Failure> {
        self.collect()
    }
}
impl Provider for igcl::Igcl {
    fn open() -> Result<Self, Failure> {
        Self::new()
    }
    fn sample(&mut self) -> Result<VendorResult, Failure> {
        self.collect()
    }
}
struct Slot<T> {
    session: Option<T>,
    retry_at: Instant,
    failure: Option<Failure>,
}
impl<T: Provider> Slot<T> {
    fn new() -> Self {
        Self {
            session: None,
            retry_at: Instant::now(),
            failure: None,
        }
    }
    fn collect(&mut self, present: bool) -> VendorResult {
        if !present {
            self.session = None;
            self.failure = None;
            self.retry_at = Instant::now();
            return VendorResult::default();
        }
        if self.session.is_none() && Instant::now() >= self.retry_at {
            match T::open() {
                Ok(session) => {
                    self.session = Some(session);
                    self.failure = None;
                }
                Err(failure) => {
                    self.failure = Some(failure);
                    self.retry_at = Instant::now() + Duration::from_secs(60);
                }
            }
        }
        if let Some(session) = &mut self.session {
            match session.sample() {
                Ok(result) => return result,
                Err(failure) => {
                    self.session = None;
                    self.failure = Some(failure);
                    self.retry_at = Instant::now() + Duration::from_secs(60);
                }
            }
        }
        VendorResult {
            readings: vec![],
            failure: self.failure.clone(),
        }
    }
}
#[derive(Clone, Copy)]
struct Request {
    amd: bool,
    intel: bool,
}
#[derive(Clone)]
struct Sample {
    time: Instant,
    results: [VendorResult; 2],
}

pub(super) struct VendorWorker {
    request: Option<mpsc::SyncSender<Request>>,
    latest: Arc<Mutex<Option<Sample>>>,
}
impl VendorWorker {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::sync_channel::<Request>(1);
        let latest = Arc::new(Mutex::new(None));
        let output = Arc::clone(&latest);
        // Sessions never leave this thread. Only owned Rust readings cross the boundary.
        let worker = std::thread::Builder::new()
            .name("gpu-vendor-readings".into())
            .spawn(move || {
                let mut amd = Slot::<adlx::Adlx>::new();
                let mut intel = Slot::<igcl::Igcl>::new();
                let Ok(mut request) = requests.recv() else {
                    return;
                };
                loop {
                    let time = Instant::now();
                    let results = [amd.collect(request.amd), intel.collect(request.intel)];
                    // Timestamp starts before calls, so a stalled driver never returns apparently fresh data.
                    if let Ok(mut latest) = output.lock() {
                        *latest = Some(Sample { time, results });
                    }
                    match requests.recv_timeout(Duration::from_secs(1)) {
                        Ok(next) => request = next,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
        Self {
            request: worker.ok().map(|_| tx),
            latest,
        }
    }
    pub fn is_pending(&self) -> bool {
        self.request.is_some() && self.latest.lock().map(|v| v.is_none()).unwrap_or(false)
    }
    pub fn collect(
        &mut self,
        gpus: &mut Vec<GpuSnapshot>,
    ) -> (
        Vec<Capability>,
        Vec<HardwareSensor>,
        Vec<TemperatureSnapshot>,
    ) {
        let cached = self.latest.lock().ok().and_then(|sample| sample.clone());
        let unknown_adapter = gpus.is_empty() || gpus.iter().any(|g| g.vendor == "unknown");
        let request = Request {
            amd: unknown_adapter || gpus.iter().any(|g| g.vendor == "amd"),
            intel: unknown_adapter || gpus.iter().any(|g| g.vendor == "intel"),
        };
        if let Some(tx) = &self.request {
            let _ = tx.try_send(request);
        }
        let mut capabilities = Vec::new();
        let mut sensors = Vec::new();
        let mut temperatures = Vec::new();
        for (index, (name, source, present)) in [
            ("gpu.amd.vendor", "amd-adlx", request.amd),
            ("gpu.intel.vendor", "intel-igcl", request.intel),
        ]
        .into_iter()
        .enumerate()
        {
            if !present {
                capabilities.push(Capability::unavailable(
                    name,
                    source,
                    CapabilityErrorKind::NotPresent,
                    "no matching Windows display adapter",
                ));
                continue;
            }
            let sample = cached
                .as_ref()
                .filter(|sample| sample.time.elapsed() <= Duration::from_secs(120));
            let Some(sample) = sample else {
                capabilities.push(Capability::unavailable(name,source,CapabilityErrorKind::Transient,"vendor readings are pending or expired; generic Windows telemetry remains available"));
                continue;
            };
            let result = &sample.results[index];
            for reading in &result.readings {
                merge(gpus, reading);
                sensors.extend(reading.sensors.iter().cloned());
                temperatures.extend(reading.temperatures.iter().cloned());
            }
            capabilities.push(if let Some(failure) = &result.failure {
                Capability::unavailable(name, source, failure.kind.clone(), failure.message.clone())
            } else if result.readings.is_empty() {
                Capability::unavailable(
                    name,
                    source,
                    CapabilityErrorKind::NotPresent,
                    "SDK enumerated no readable matching GPU",
                )
            } else {
                Capability::available(name, source)
            });
        }
        (capabilities, sensors, temperatures)
    }
}
fn merge(gpus: &mut Vec<GpuSnapshot>, reading: &Reading) {
    let extra = &reading.gpu;
    if let Some(base) = gpus
        .iter_mut()
        .find(|g| g.id == extra.id && g.vendor == extra.vendor)
    {
        macro_rules! enrich {($($field:ident),*)=>{$(if extra.$field.is_some(){base.$field=extra.$field;})*};}
        enrich!(
            utilization_percent,
            memory_total_bytes,
            memory_used_bytes,
            temperature_celsius,
            power_watts,
            core_clock_mhz,
            memory_clock_mhz
        );
        base.source = format!("windows-dxgi-pdh+{}", extra.source);
    } else if gpus.len() < crate::model::CLIENT_REPORT_MAX_GPUS {
        gpus.push(extra.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_wddm_adapter_still_probes_both_vendor_sdks() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut worker = VendorWorker {
            request: Some(tx),
            latest: Arc::new(Mutex::new(None)),
        };
        let mut gpus = vec![
            Reading::new(
                "windows-wddm".into(),
                "unknown",
                "GPU".into(),
                "windows-pdh",
            )
            .gpu,
        ];
        worker.collect(&mut gpus);
        let request = rx.try_recv().unwrap();
        assert!(request.amd && request.intel);
    }
    #[test]
    fn expired_native_readings_never_replace_generic_readings() {
        let mut reading = Reading::new(luid_id(1, 0), "amd", "GPU".into(), "amd-adlx");
        reading.gpu.power_watts = Some(100.0);
        reading.fan("fan0", Some(1500.0));
        let mut worker = VendorWorker {
            request: None,
            latest: Arc::new(Mutex::new(Some(Sample {
                time: Instant::now() - Duration::from_secs(121),
                results: [
                    VendorResult {
                        readings: vec![reading],
                        failure: None,
                    },
                    VendorResult::default(),
                ],
            }))),
        };
        let mut generic = Reading::new(luid_id(1, 0), "amd", "GPU".into(), "windows-dxgi-pdh");
        generic.gpu.utilization_percent = Some(30.0);
        let mut gpus = vec![generic.gpu];
        let (capabilities, sensors, _) = worker.collect(&mut gpus);
        assert_eq!(gpus[0].power_watts, None);
        assert_eq!(gpus[0].utilization_percent, Some(30.0));
        assert!(sensors.is_empty());
        assert_eq!(
            capabilities[0].error_kind,
            Some(CapabilityErrorKind::Transient)
        );
    }
    #[test]
    fn session_failures_release_handles_and_back_off() {
        struct Failing;
        impl Provider for Failing {
            fn open() -> Result<Self, Failure> {
                panic!("must wait for retry deadline")
            }
            fn sample(&mut self) -> Result<VendorResult, Failure> {
                Err(Failure::new(
                    CapabilityErrorKind::Unsupported,
                    "version mismatch",
                ))
            }
        }
        let mut slot = Slot {
            session: Some(Failing),
            retry_at: Instant::now(),
            failure: None,
        };
        assert!(slot.collect(true).failure.is_some());
        assert!(slot.session.is_none());
        assert!(slot.retry_at > Instant::now());
        assert_eq!(
            slot.collect(true).failure.unwrap().kind,
            CapabilityErrorKind::Unsupported
        );
        assert!(slot.collect(false).failure.is_none());
    }
    #[test]
    fn two_identically_named_gpus_are_merged_only_by_luid() {
        let mut first = Reading::new(
            luid_id(1, 0),
            "amd",
            "same model".into(),
            "windows-dxgi-pdh",
        );
        first.gpu.utilization_percent = Some(20.0);
        let second = Reading::new(
            luid_id(2, 0),
            "amd",
            "same model".into(),
            "windows-dxgi-pdh",
        );
        let mut gpus = vec![first.gpu, second.gpu];
        let mut extra = Reading::new(luid_id(2, 0), "amd", "same model".into(), "amd-adlx");
        extra.gpu.temperature_celsius = Some(65.0);
        merge(&mut gpus, &extra);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].temperature_celsius, None);
        assert_eq!(gpus[0].utilization_percent, Some(20.0));
        assert_eq!(gpus[1].temperature_celsius, Some(65.0));
    }
    #[test]
    fn zero_fan_rpm_is_real_but_invalid_values_are_not_sensors() {
        let mut r = Reading::new("gpu".into(), "amd", "test".into(), "amd-adlx");
        r.fan("fan0", Some(0.0));
        r.fan("fan1", Some(-1.0));
        r.voltage("core", Some(f64::NAN));
        assert_eq!(r.sensors.len(), 1);
        assert_eq!(r.sensors[0].value, 0.0);
    }
}
