//! Slow, read-only inventory runs outside the sampling thread.
use crate::model::*;
use chrono::{DateTime, Utc};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos_display;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod reports;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
mod smbios;

#[derive(Clone)]
pub(super) struct Inventory {
    pub collected_at: Option<DateTime<Utc>>,
    pub memory_modules: Vec<MemoryModule>,
    pub devices: Vec<HardwareDevice>,
    pub capabilities: Vec<Capability>,
}

impl Inventory {
    fn empty(kind: CapabilityErrorKind, message: &str) -> Self {
        Self {
            collected_at: None,
            memory_modules: vec![],
            devices: vec![],
            capabilities: NAMES
                .iter()
                .map(|name| {
                    Capability::unavailable(*name, "platform-inventory", kind.clone(), message)
                })
                .collect(),
        }
    }
}

const NAMES: [&str; 7] = [
    "hardware.memory",
    "hardware.thunderbolt",
    "hardware.monitors",
    "hardware.bluetooth",
    "hardware.usb_controllers",
    "hardware.usb_devices",
    "hardware.audio",
];

pub(super) struct InventoryCollector {
    pending: Option<mpsc::Receiver<Inventory>>,
    last_started: Option<Instant>,
    cached: Inventory,
}

impl InventoryCollector {
    pub(super) fn new() -> Self {
        let mut result = Self {
            pending: None,
            last_started: None,
            cached: Inventory::empty(
                CapabilityErrorKind::NotPresent,
                "hardware inventory scan is pending",
            ),
        };
        result.poll(300);
        result
    }

    pub(super) fn poll(&mut self, interval_seconds: u64) -> Inventory {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(inventory) => {
                    self.cached = inventory;
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cached = Inventory::empty(
                        CapabilityErrorKind::Transient,
                        "hardware inventory worker stopped",
                    );
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_none()
            && self
                .last_started
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(interval_seconds.max(1)))
        {
            let (sender, receiver) = mpsc::channel();
            match std::thread::Builder::new()
                .name("hardware-inventory".into())
                .spawn(move || {
                    let _ = sender.send(collect());
                }) {
                Ok(_) => self.pending = Some(receiver),
                Err(_) => {
                    self.cached = Inventory::empty(
                        CapabilityErrorKind::Transient,
                        "could not start hardware inventory worker",
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

fn collect() -> Inventory {
    #[cfg(target_os = "linux")]
    {
        linux::collect(std::path::Path::new("/sys"), std::path::Path::new("/proc"))
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        reports::collect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Inventory::empty(
            CapabilityErrorKind::Unsupported,
            "no public hardware inventory provider on this platform",
        )
    }
}

fn device(id: &str, kind: HardwareDeviceKind, name: &str, source: &str) -> HardwareDevice {
    use super::hardware::text;
    HardwareDevice {
        id: stable_id(id),
        kind,
        name: text(name).unwrap_or_else(|| "unknown".into()),
        model: None,
        vendor: None,
        vendor_id: None,
        product_id: None,
        revision: None,
        version: None,
        bus: None,
        driver: None,
        connection: None,
        speed_mbps: None,
        source: source.into(),
    }
}

fn stable_id(value: &str) -> String {
    if value.len() <= MAX_HARDWARE_TEXT && !value.chars().any(char::is_control) && !value.is_empty()
    {
        return value.into();
    }
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("inventory:{digest}")
}

fn capability(
    name: &str,
    source: &str,
    count: usize,
    failure: Option<CapabilityErrorKind>,
) -> Capability {
    match failure {
        Some(kind) => Capability::unavailable(
            name,
            source,
            kind,
            "some inventory data could not be read; readable devices are retained",
        ),
        None if count == 0 => Capability::unavailable(
            name,
            source,
            CapabilityErrorKind::NotPresent,
            "no devices exposed by this provider",
        ),
        None => Capability::available(name, source),
    }
}
