//! Low-frequency hardware inventory from Apple's machine-readable System Report.
//! Inventory presence does not imply availability of live GPU utilization or power.
use crate::model::{CLIENT_REPORT_MAX_GPUS, Capability, CapabilityErrorKind, GpuSnapshot};
use serde_json::Value;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

type Inventory = (Vec<GpuSnapshot>, Capability);

pub(super) struct InventoryCollector {
    pending: Option<mpsc::Receiver<Inventory>>,
    last_started: Option<Instant>,
    cached: Inventory,
}

impl InventoryCollector {
    pub(super) fn new() -> Self {
        let mut collector = Self {
            pending: None,
            last_started: None,
            cached: (
                Vec::new(),
                unavailable(
                    CapabilityErrorKind::NotPresent,
                    "System Report inventory has not completed yet",
                ),
            ),
        };
        collector.poll();
        collector
    }
    pub(super) fn poll(&mut self) -> Inventory {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(inventory) => {
                    self.cached = inventory;
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cached = (
                        Vec::new(),
                        unavailable(
                            CapabilityErrorKind::Transient,
                            "System Report worker stopped",
                        ),
                    );
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_none()
            && self
                .last_started
                .is_none_or(|started| started.elapsed() >= Duration::from_secs(300))
        {
            let (sender, receiver) = mpsc::channel();
            match std::thread::Builder::new()
                .name("macos-system-report".into())
                .spawn(move || {
                    let _ = sender.send(collect());
                }) {
                Ok(_) => self.pending = Some(receiver),
                Err(_) => {
                    self.cached = (
                        Vec::new(),
                        unavailable(
                            CapabilityErrorKind::Transient,
                            "could not start System Report worker",
                        ),
                    )
                }
            }
            self.last_started = Some(Instant::now());
        }
        self.cached.clone()
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

fn unavailable(kind: CapabilityErrorKind, message: &str) -> Capability {
    Capability::unavailable("system.gpu", "macos-system-profiler", kind, message)
}

fn collect() -> Inventory {
    let result = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CapabilityErrorKind::Transient)?;
        runtime.block_on(async {
            use xcsc::runtime::process::{ProcessCaptureError, ProcessLimits, capture_bounded};
            let mut command = tokio::process::Command::new("/usr/sbin/system_profiler");
            command.args(["-json", "-detailLevel", "mini", "SPDisplaysDataType"]);
            let output = capture_bounded(
                &mut command,
                ProcessLimits {
                    timeout: Duration::from_secs(10),
                    stdout_bytes: 1024 * 1024,
                    stderr_bytes: 32 * 1024,
                },
            )
            .await
            .map_err(|error| match error {
                ProcessCaptureError::Spawn(std::io::ErrorKind::PermissionDenied) => {
                    CapabilityErrorKind::PermissionDenied
                }
                ProcessCaptureError::Spawn(std::io::ErrorKind::NotFound) => {
                    CapabilityErrorKind::DriverMissing
                }
                ProcessCaptureError::OutputLimit(_) => CapabilityErrorKind::InvalidData,
                _ => CapabilityErrorKind::Transient,
            })?;
            if !output.status.success() {
                return Err(CapabilityErrorKind::Transient);
            }
            parse_displays(&output.stdout)
        })
    })();
    match result {
        Ok(gpus) if !gpus.is_empty() => (
            gpus,
            Capability::available("system.gpu", "macos-system-profiler"),
        ),
        Ok(_) => (
            Vec::new(),
            unavailable(
                CapabilityErrorKind::NotPresent,
                "System Report exposed no GPU inventory",
            ),
        ),
        Err(kind) => (
            Vec::new(),
            unavailable(
                kind,
                "System Report GPU inventory could not be read; live GPU telemetry is collected separately",
            ),
        ),
    }
}

fn parse_displays(bytes: &[u8]) -> Result<Vec<GpuSnapshot>, CapabilityErrorKind> {
    let report: Value =
        serde_json::from_slice(bytes).map_err(|_| CapabilityErrorKind::InvalidData)?;
    let devices = report
        .get("SPDisplaysDataType")
        .and_then(Value::as_array)
        .ok_or(CapabilityErrorKind::InvalidData)?;
    if devices.len() > CLIENT_REPORT_MAX_GPUS {
        return Err(CapabilityErrorKind::InvalidData);
    }
    devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            // Prefer the model; _name can be Apple's untranslated kHW_AppleM1Item key.
            let name = device
                .get("sppci_model")
                .or_else(|| device.get("_name"))
                .and_then(Value::as_str)
                .and_then(super::hardware::text)
                .ok_or(CapabilityErrorKind::InvalidData)?;
            let vendor_text = device
                .get("spdisplays_vendor")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let vendor = vendor_text
                .split(|character: char| !character.is_ascii_alphanumeric())
                .find_map(|token| match token {
                    "apple" => Some("apple"),
                    "nvidia" | "0x10de" => Some("nvidia"),
                    "amd" | "ati" | "0x1002" => Some("amd"),
                    "intel" | "0x8086" => Some("intel"),
                    _ => None,
                })
                .unwrap_or("unknown");
            Ok(GpuSnapshot {
                id: format!("system-profiler:gpu:{index}"),
                vendor: vendor.into(),
                name,
                utilization_percent: None,
                memory_total_bytes: None,
                memory_used_bytes: None,
                temperature_celsius: None,
                power_watts: None,
                core_clock_mhz: None,
                memory_clock_mhz: None,
                pcie_rx_bytes_per_second: None,
                pcie_tx_bytes_per_second: None,
                source: "macos-system-profiler".into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_inventory_does_not_invent_live_metrics_or_dedicated_vram() {
        let gpus = parse_displays(br#"{"SPDisplaysDataType":[{"_name":"kHW_AppleM1Item","sppci_model":"Apple M1","spdisplays_vendor":"sppci_vendor_Apple","sppci_cores":"8","spdisplays_vram_shared":"16 GB"}]}"#).unwrap();
        assert_eq!(gpus[0].name, "Apple M1");
        assert_eq!(gpus[0].vendor, "apple");
        assert_eq!(gpus[0].utilization_percent, None);
        assert_eq!(gpus[0].memory_total_bytes, None);
        assert_eq!(gpus[0].power_watts, None);
    }

    #[test]
    fn vendor_tokens_do_not_misclassify_an_unknown_corporation_as_ati() {
        for (reported, expected) in [
            ("Unknown Corporation", "unknown"),
            ("sppci_vendor_Apple", "apple"),
            ("ATI (0x1002)", "amd"),
            ("sppci_vendor_Intel", "intel"),
            ("NVIDIA (0x10de)", "nvidia"),
        ] {
            let json = serde_json::json!({"SPDisplaysDataType": [{"sppci_model": "GPU", "spdisplays_vendor": reported}]});
            let gpus = parse_displays(&serde_json::to_vec(&json).unwrap()).unwrap();
            assert_eq!(gpus[0].vendor, expected);
        }
    }

    #[test]
    fn malformed_missing_or_oversized_inventory_is_not_a_successful_empty_scan() {
        assert!(parse_displays(b"not JSON").is_err());
        assert!(parse_displays(b"{}").is_err());
        assert!(parse_displays(br#"{"SPDisplaysDataType":[{}]}"#).is_err());
        let json = serde_json::json!({"SPDisplaysDataType": vec![serde_json::json!({"sppci_model":"GPU"}); CLIENT_REPORT_MAX_GPUS + 1]});
        assert!(parse_displays(&serde_json::to_vec(&json).unwrap()).is_err());
        assert!(
            parse_displays(br#"{"SPDisplaysDataType":[]}"#)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn native_system_report_reads_mac_gpu_inventory() {
        let mut collector = InventoryCollector::new();
        let deadline = Instant::now() + Duration::from_secs(11);
        while collector.is_pending() && Instant::now() < deadline {
            collector.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !collector.is_pending(),
            "System Report exceeded its process budget"
        );
        let (gpus, capability) = collector.poll();
        // A headless/virtualized Mac can legitimately expose an explicit empty list.
        if gpus.is_empty() {
            assert_eq!(capability.error_kind, Some(CapabilityErrorKind::NotPresent));
        } else {
            assert!(capability.available, "{capability:?}");
        }
    }
}
