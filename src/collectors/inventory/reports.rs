//! Machine-readable native reports. Parse only the requested hardware classes.
use super::{Inventory, capability, device};
use crate::model::*;
use serde_json::Value;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn collect() -> Inventory {
    let result = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CapabilityErrorKind::Transient)?;
        runtime.block_on(async {
            use xcsc_runtime::process::{ProcessCaptureError, ProcessLimits, capture_bounded};
            #[cfg(target_os = "macos")]
            let mut command = {
                let mut command = tokio::process::Command::new("/usr/sbin/system_profiler");
                command.args([
                    "-json",
                    "-detailLevel",
                    "mini",
                    "SPMemoryDataType",
                    "SPThunderboltDataType",
                    "SPDisplaysDataType",
                    "SPBluetoothDataType",
                    "SPUSBHostDataType",
                    "SPUSBDataType",
                    "SPAudioDataType",
                ]);
                command
            };
            #[cfg(target_os = "windows")]
            let mut command = {
                use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
                let mut buffer = [0u16; 32768];
                // SAFETY: the Windows API writes at most the supplied buffer length.
                let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
                if length == 0 || length >= buffer.len() {
                    return Err(CapabilityErrorKind::Transient);
                }
                let system = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
                let mut command = tokio::process::Command::new(
                    system.join("WindowsPowerShell/v1.0/powershell.exe"),
                );
                command.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    include_str!("windows-inventory.ps1"),
                ]);
                command.creation_flags(0x08000000);
                command
            };
            let output = capture_bounded(
                &mut command,
                ProcessLimits {
                    timeout: Duration::from_secs(10),
                    stdout_bytes: 2 * 1024 * 1024,
                    stderr_bytes: 32 * 1024,
                },
            )
            .await
            .map_err(|e| match e {
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
            #[cfg(target_os = "macos")]
            {
                parse_macos(&output.stdout)
            }
            #[cfg(target_os = "windows")]
            {
                parse_windows(&output.stdout)
            }
        })
    })();
    let inventory = result
        .unwrap_or_else(|kind| Inventory::empty(kind, "native hardware report could not be read"));
    #[cfg(target_os = "macos")]
    let inventory = super::macos_display::complete(inventory);
    inventory
}

fn text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .and_then(super::super::hardware::text)
    })
}
#[cfg(any(target_os = "windows", test))]
fn positive(value: &Value, key: &str) -> Option<f64> {
    let value = value.get(key)?;
    let number = value.as_f64().or_else(|| value.as_str()?.parse().ok())?;
    (number.is_finite() && number > 0.0).then_some(number)
}
fn finish(
    memory_modules: Vec<MemoryModule>,
    mut devices: Vec<HardwareDevice>,
    source: &str,
    failures: [Option<CapabilityErrorKind>; 7],
) -> Inventory {
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    devices.dedup_by(|a, b| a.id == b.id && a.kind == b.kind);
    devices.truncate(MAX_HARDWARE_DEVICES);
    let kinds = [
        HardwareDeviceKind::Thunderbolt,
        HardwareDeviceKind::Monitor,
        HardwareDeviceKind::Bluetooth,
        HardwareDeviceKind::UsbController,
        HardwareDeviceKind::UsbDevice,
        HardwareDeviceKind::Audio,
    ];
    let capabilities = super::NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            capability(
                name,
                source,
                if i == 0 {
                    memory_modules.len()
                } else {
                    devices.iter().filter(|d| d.kind == kinds[i - 1]).count()
                },
                failures[i].clone(),
            )
        })
        .collect();
    Inventory {
        collected_at: Some(chrono::Utc::now()),
        memory_modules,
        devices,
        capabilities,
    }
}

// CIM_Chip and SMBIOS Type 17 use different form-factor enumerations.
#[cfg(any(target_os = "windows", test))]
fn windows_form_factor(value: u64) -> Option<String> {
    Some(
        match value {
            2 => "SIP",
            3 => "DIP",
            4 => "ZIP",
            5 => "SOJ",
            6 => "Proprietary Card",
            7 => "SIMM",
            8 => "DIMM",
            9 => "TSOP",
            10 => "PGA",
            11 => "RIMM",
            12 => "SO-DIMM",
            13 => "SRIMM",
            14 => "SMD",
            15 => "SSMP",
            16 => "QFP",
            17 => "TQFP",
            18 => "SOIC",
            19 => "LCC",
            20 => "PLCC",
            21 => "BGA",
            22 => "FPBGA",
            23 => "LGA",
            _ => return None,
        }
        .into(),
    )
}

#[cfg(any(target_os = "windows", test))]
fn utf16_array(value: &Value, key: &str) -> Option<String> {
    let units: Vec<_> = value
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_u64().and_then(|n| u16::try_from(n).ok()))
        .take_while(|v| *v != 0)
        .collect();
    super::super::hardware::text(&String::from_utf16_lossy(&units))
}

#[cfg(any(target_os = "windows", test))]
fn windows_ids(value: &str) -> (Option<String>, Option<String>, Option<String>) {
    let value = value.to_ascii_uppercase();
    let token = |prefix: &str, length| -> Option<String> {
        let start = value.find(prefix)? + prefix.len();
        let code = value.get(start..start + length)?;
        code.bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then(|| code.to_ascii_lowercase())
    };
    (
        token("VEN_", 4).or_else(|| token("VID_", 4)),
        token("DEV_", 4).or_else(|| token("PID_", 4)),
        token("REV_", 4).or_else(|| token("REV_", 2)),
    )
}

#[cfg(any(target_os = "windows", test))]
fn windows_device_id(value: &Value, key: &str) -> Option<String> {
    // PnP/WMI identities can exceed the display-text budget. Keep the full
    // identity until stable_id hashes it, rather than merging a shared prefix.
    let value = value.get(key)?.as_str()?.trim();
    (!value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control))
        .then(|| value.to_ascii_uppercase())
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows(bytes: &[u8]) -> Result<Inventory, CapabilityErrorKind> {
    let report: Value =
        serde_json::from_slice(bytes).map_err(|_| CapabilityErrorKind::InvalidData)?;
    let mut failures = [const { None }; 7];
    let error = |key| {
        report.get("errors").and_then(|e| e.get(key)).map(|v| {
            if v.as_str() == Some("permission_denied") {
                CapabilityErrorKind::PermissionDenied
            } else {
                CapabilityErrorKind::Transient
            }
        })
    };
    let items = |key| {
        report
            .get(key)
            .and_then(Value::as_array)
            .ok_or(CapabilityErrorKind::InvalidData)
    };
    failures[0] = error("memory");
    let mut memory_modules = vec![];
    for (index, value) in items("memory")?.iter().take(MAX_MEMORY_MODULES).enumerate() {
        let id = text(value, &["Tag", "DeviceLocator"]).unwrap_or_else(|| index.to_string());
        memory_modules.push(MemoryModule {
            id: super::stable_id(&format!("cim:{id}:{index}")),
            locator: text(value, &["DeviceLocator", "BankLabel"]),
            model: text(value, &["PartNumber"]).and_then(|s| super::smbios::firmware_text(&s)),
            vendor: text(value, &["Manufacturer"]).and_then(|s| super::smbios::firmware_text(&s)),
            memory_type: value
                .get("SMBIOSMemoryType")
                .and_then(Value::as_u64)
                .and_then(super::smbios::memory_type),
            form_factor: value
                .get("FormFactor")
                .and_then(Value::as_u64)
                .and_then(windows_form_factor),
            module_version: text(value, &["Version"])
                .and_then(|s| super::smbios::firmware_text(&s)),
            capacity_bytes: value
                .get("Capacity")
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                .filter(|v| *v > 0),
            speed_mt_s: positive(value, "Speed"),
            configured_speed_mt_s: positive(value, "ConfiguredClockSpeed"),
            reported_speed: None,
            source: "windows-cim".into(),
        });
    }
    let mut devices = vec![];
    for value in items("pnp")?.iter().take(1024) {
        let Some(id) = windows_device_id(value, "PNPDeviceID") else {
            failures[1..].fill(Some(CapabilityErrorKind::InvalidData));
            continue;
        };
        let name = text(value, &["Name"])
            .or_else(|| super::super::hardware::text(&id))
            .unwrap_or_else(|| "unknown".into());
        let class = text(value, &["PNPClass"])
            .unwrap_or_default()
            .to_ascii_lowercase();
        let lower = name.to_ascii_lowercase();
        let kind = if lower.contains("thunderbolt") || lower.contains("usb4") {
            HardwareDeviceKind::Thunderbolt
        } else {
            match class.as_str() {
                "usb" if id.to_ascii_uppercase().starts_with("PCI\\") => {
                    HardwareDeviceKind::UsbController
                }
                "usb" => HardwareDeviceKind::UsbDevice,
                "bluetooth"
                    if id.to_ascii_uppercase().starts_with("USB\\")
                        || id.to_ascii_uppercase().starts_with("PCI\\")
                        || id.to_ascii_uppercase().starts_with("ACPI\\") =>
                {
                    HardwareDeviceKind::Bluetooth
                }
                // Enumerators and paired peripherals do not identify a radio chip.
                "bluetooth" => continue,
                "media" | "audioendpoint" => HardwareDeviceKind::Audio,
                "monitor" => HardwareDeviceKind::Monitor,
                _ => continue,
            }
        };
        let mut item = device(&id, kind, &name, "windows-cim-pnp");
        item.model = Some(name);
        item.vendor = text(value, &["Manufacturer"]);
        item.driver = text(value, &["Service"]);
        let hardware_id = value
            .get("HardwareID")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .and_then(Value::as_str)
            .unwrap_or(&id);
        (item.vendor_id, item.product_id, item.revision) = windows_ids(hardware_id);
        item.bus = id.split_once('\\').map(|(bus, _)| bus.into());
        devices.push(item);
    }
    if let Some(kind) = error("pnp") {
        failures[1..].fill(Some(kind));
    }
    if let Some(kind) = error("monitors") {
        failures[2] = Some(kind);
    }
    for value in items("monitors")?.iter().take(64) {
        let Some(instance) = windows_device_id(value, "InstanceName") else {
            failures[2] = Some(CapabilityErrorKind::InvalidData);
            continue;
        };
        let id = instance
            .rsplit_once('_')
            .filter(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
            .map_or(instance.as_str(), |(id, _)| id);
        let stable = super::stable_id(id);
        let name = utf16_array(value, "UserFriendlyName");
        let existing = devices
            .iter_mut()
            .find(|d| d.kind == HardwareDeviceKind::Monitor && d.id == stable);
        let mut additional = device(
            id,
            HardwareDeviceKind::Monitor,
            name.as_deref().unwrap_or(id),
            "windows-wmi-monitor",
        );
        let item = existing.unwrap_or(&mut additional);
        item.vendor = utf16_array(value, "ManufacturerName");
        item.product_id = utf16_array(value, "ProductCodeID");
        if let Some(name) = name {
            item.name = name.clone();
            item.model = Some(name);
        }
        if item.source == "windows-wmi-monitor" {
            devices.push(additional);
        }
    }
    if let Some(kind) = error("sound") {
        failures[6] = Some(kind);
    }
    for value in items("sound")?.iter().take(256) {
        let Some(id) = windows_device_id(value, "PNPDeviceID") else {
            continue;
        };
        let stable = super::stable_id(&id);
        if devices
            .iter()
            .any(|d| d.kind == HardwareDeviceKind::Audio && d.id == stable)
        {
            continue;
        }
        let name = text(value, &["Name"])
            .or_else(|| super::super::hardware::text(&id))
            .unwrap_or_else(|| "unknown".into());
        let mut item = device(&id, HardwareDeviceKind::Audio, &name, "windows-cim-sound");
        item.model = Some(name);
        item.vendor = text(value, &["Manufacturer"]);
        (item.vendor_id, item.product_id, item.revision) = windows_ids(&id);
        item.bus = id.split_once('\\').map(|(b, _)| b.into());
        devices.push(item);
    }
    Ok(finish(memory_modules, devices, "windows-cim", failures))
}

#[cfg(any(target_os = "macos", test))]
fn walk<'a>(items: &'a [Value], depth: usize, path: &str, result: &mut Vec<(String, &'a Value)>) {
    if depth > 12 || result.len() >= 2048 {
        return;
    }
    for (i, item) in items.iter().take(256).enumerate() {
        if result.len() >= 2048 {
            break;
        }
        let id = format!("{path}/{i}:{}", text(item, &["_name"]).unwrap_or_default());
        result.push((id.clone(), item));
        if let Some(children) = item.get("_items").and_then(Value::as_array) {
            walk(children, depth + 1, &id, result);
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn capacity(label: &str) -> Option<u64> {
    let mut tokens = label.split_whitespace();
    let number = tokens.next()?.parse::<f64>().ok()?;
    let factor = match tokens.next()?.to_ascii_uppercase().as_str() {
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0_f64.powi(2),
        "GB" | "GIB" => 1024.0_f64.powi(3),
        "TB" | "TIB" => 1024.0_f64.powi(4),
        "B" => 1.0,
        _ => return None,
    };
    let size = number * factor;
    (size.is_finite() && size > 0.0 && size < u64::MAX as f64).then_some(size as u64)
}

#[cfg(any(target_os = "macos", test))]
fn usb_speed_mbps(label: &str) -> Option<f64> {
    let mut tokens = label.split_whitespace();
    let number = tokens.next()?.parse::<f64>().ok()?;
    let factor = match tokens.next()? {
        "Gb/s" => 1000.0,
        "Mb/s" => 1.0,
        "Kb/s" => 0.001,
        _ => return None,
    };
    let speed = number * factor;
    (tokens.next().is_none() && speed.is_finite() && speed > 0.0).then_some(speed)
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos(bytes: &[u8]) -> Result<Inventory, CapabilityErrorKind> {
    let report: Value =
        serde_json::from_slice(bytes).map_err(|_| CapabilityErrorKind::InvalidData)?;
    if !report.is_object() {
        return Err(CapabilityErrorKind::InvalidData);
    }
    let mut failures = [const { None }; 7];
    let mut memory_modules = vec![];
    let mut devices = vec![];
    // Tahoe exposes USB through USBHost; earlier macOS uses USB. Some report
    // providers include both names, so consume one populated tree only.
    let usb_category = if report
        .get("SPUSBHostDataType")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        "SPUSBHostDataType"
    } else {
        "SPUSBDataType"
    };
    let categories = [
        "SPMemoryDataType",
        "SPThunderboltDataType",
        "SPDisplaysDataType",
        "SPBluetoothDataType",
        usb_category,
        "SPAudioDataType",
    ];
    for category in categories {
        let slot = match category {
            "SPMemoryDataType" => 0,
            "SPThunderboltDataType" => 1,
            "SPDisplaysDataType" => 2,
            "SPBluetoothDataType" => 3,
            "SPUSBDataType" | "SPUSBHostDataType" => 4,
            _ => 6,
        };
        let Some(items) = report.get(category).and_then(Value::as_array) else {
            failures[slot] = Some(CapabilityErrorKind::NotPresent);
            if slot == 4 {
                failures[5] = failures[4].clone();
            }
            continue;
        };
        let mut nodes = vec![];
        walk(items, 0, category, &mut nodes);
        for (path, value) in nodes {
            match category {
                "SPMemoryDataType" => {
                    let size = text(value, &["dimm_size", "SPMemoryDataType"]);
                    if let Some(size) = size.and_then(|s| capacity(&s)) {
                        if memory_modules.len() >= MAX_MEMORY_MODULES {
                            continue;
                        }
                        let reported_speed = text(value, &["dimm_speed"]);
                        let speed_mt_s = reported_speed
                            .as_deref()
                            .and_then(|s| s.strip_suffix(" MT/s")?.parse::<f64>().ok())
                            .filter(|v| v.is_finite() && *v > 0.0);
                        memory_modules.push(MemoryModule {
                            id: super::stable_id(&path),
                            locator: text(value, &["_name"]),
                            model: text(value, &["dimm_part_number"]),
                            vendor: text(value, &["dimm_manufacturer"]),
                            memory_type: text(value, &["dimm_type"]),
                            module_version: text(value, &["dimm_firmware_version"]),
                            form_factor: None,
                            capacity_bytes: Some(size),
                            speed_mt_s,
                            configured_speed_mt_s: None,
                            reported_speed,
                            source: "macos-system-profiler".into(),
                        });
                    }
                }
                "SPDisplaysDataType" => {
                    if let Some(monitors) = value.get("spdisplays_ndrvs").and_then(Value::as_array)
                    {
                        for (i, monitor) in monitors.iter().take(64).enumerate() {
                            let name =
                                text(monitor, &["_name"]).unwrap_or_else(|| "Monitor".into());
                            let mut item = device(
                                &format!("{path}/display:{i}"),
                                HardwareDeviceKind::Monitor,
                                &name,
                                "macos-system-profiler",
                            );
                            item.model = Some(name);
                            item.vendor_id = text(monitor, &["spdisplays_display-vendor-id"]);
                            item.product_id = text(monitor, &["spdisplays_display-product-id"]);
                            item.connection = text(monitor, &["spdisplays_connection_type"]);
                            item.bus = Some("Display".into());
                            devices.push(item);
                        }
                    }
                }
                "SPBluetoothDataType" => {
                    // Deliberately skip paired/remote device lists and their addresses.
                    let controller = value.get("controller_properties").unwrap_or(value);
                    if let Some(chipset) = text(controller, &["controller_chipset", "chipset"]) {
                        let mut item = device(
                            &path,
                            HardwareDeviceKind::Bluetooth,
                            &chipset,
                            "macos-system-profiler",
                        );
                        item.model = Some(chipset);
                        item.vendor_id = text(controller, &["controller_vendorID"]);
                        item.product_id = text(controller, &["controller_productID"]);
                        item.revision = text(controller, &["controller_firmwareVersion"]);
                        item.bus = text(controller, &["controller_transport"]);
                        devices.push(item);
                    }
                }
                "SPUSBDataType" | "SPUSBHostDataType" => {
                    let driver = text(value, &["Driver", "host_controller"]);
                    let is_controller = value.get("host_controller").is_some()
                        || (category == "SPUSBHostDataType"
                            && driver.is_some()
                            && value.get("USBDeviceKeyProductID").is_none());
                    let kind = if is_controller {
                        HardwareDeviceKind::UsbController
                    } else {
                        HardwareDeviceKind::UsbDevice
                    };
                    let name = text(value, &["_name"]).or_else(|| driver.clone());
                    if let Some(name) = name {
                        let mut item = device(&path, kind, &name, "macos-system-profiler");
                        // A host controller driver is not a reported chip model.
                        item.model = (!is_controller).then_some(name);
                        item.driver = driver;
                        item.vendor = text(value, &["USBDeviceKeyVendorName", "manufacturer"]);
                        item.vendor_id =
                            text(value, &["USBDeviceKeyVendorID", "vendor_id", "pci_vendor"]);
                        item.product_id = text(
                            value,
                            &["USBDeviceKeyProductID", "product_id", "pci_device"],
                        );
                        item.revision = text(
                            value,
                            &["USBDeviceKeyProductVersion", "version", "pci_revision"],
                        );
                        item.speed_mbps = text(value, &["USBDeviceKeyLinkSpeed"])
                            .and_then(|speed| usb_speed_mbps(&speed));
                        item.bus = Some("USB".into());
                        devices.push(item);
                    }
                }
                "SPThunderboltDataType" => {
                    if let Some(name) = text(value, &["device_name_key", "device_name", "_name"]) {
                        let mut item = device(
                            &path,
                            HardwareDeviceKind::Thunderbolt,
                            &name,
                            "macos-system-profiler",
                        );
                        item.model = text(value, &["device_name_key", "device_name"]);
                        item.vendor = text(value, &["vendor_name_key", "vendor_name"]);
                        item.revision = text(value, &["firmware_version_key", "firmware_version"]);
                        item.bus = Some("Thunderbolt/USB4".into());
                        devices.push(item);
                    }
                }
                "SPAudioDataType" => {
                    if value.get("_items").is_some() {
                        continue;
                    }
                    if let Some(name) = text(value, &["_name"]) {
                        let mut item = device(
                            &path,
                            HardwareDeviceKind::Audio,
                            &name,
                            "macos-system-profiler",
                        );
                        item.model = Some(name);
                        item.vendor = text(value, &["coreaudio_device_manufacturer"]);
                        item.bus = text(value, &["coreaudio_device_transport"]);
                        devices.push(item);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(finish(
        memory_modules,
        devices,
        "macos-system-profiler",
        failures,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_partial_permission_failure_preserves_radio_and_usb4_inventory() {
        let report = serde_json::json!({"memory":[], "monitors":[], "sound":[],
            "pnp":[{"PNPDeviceID":"USB\\VID_8087&PID_0033\\0", "Name":"Intel AX211 Bluetooth", "PNPClass":"Bluetooth"},
                {"PNPDeviceID":"PCI\\VEN_8086&DEV_1234", "Name":"USB4 Host Router", "PNPClass":"USB"},
                {"PNPDeviceID":"BTH\\MS_BTHBRB", "Name":"Microsoft Bluetooth Enumerator", "PNPClass":"Bluetooth"}],
            "errors":{"memory":"permission_denied"}});
        let result = parse_windows(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(result.devices.len(), 2);
        assert!(
            result
                .devices
                .iter()
                .any(|d| d.kind == HardwareDeviceKind::Thunderbolt)
        );
        assert!(
            result
                .devices
                .iter()
                .any(|d| d.kind == HardwareDeviceKind::Bluetooth
                    && d.product_id.as_deref() == Some("0033"))
        );
        assert_eq!(
            result.capabilities[0].error_kind,
            Some(CapabilityErrorKind::PermissionDenied)
        );
        assert!(result.capabilities[1].available);
        assert!(result.capabilities[3].available);
        assert_eq!(windows_form_factor(8).as_deref(), Some("DIMM"));
        assert_eq!(windows_form_factor(12).as_deref(), Some("SO-DIMM"));
    }

    #[test]
    fn windows_cim_memory_pnp_monitor_and_sound_are_joined() {
        let report = serde_json::json!({"memory":[{"Tag":"Physical Memory 0","PartNumber":"ABC123", "Manufacturer":"Kingston", "Capacity":"17179869184", "SMBIOSMemoryType":34,"FormFactor":8,"Speed":5600,"ConfiguredClockSpeed":4800}], "pnp":[{"PNPDeviceID":"PCI\\VEN_8086&DEV_7A60&REV_11","Name":"Intel USB Controller","PNPClass":"USB"},{"PNPDeviceID":"DISPLAY\\DEL1234\\0","Name":"Generic Monitor","PNPClass":"Monitor"},{"PNPDeviceID":"BTHENUM\\paired","Name":"Paired phone","PNPClass":"Bluetooth"}], "monitors":[{"InstanceName":"DISPLAY\\DEL1234\\0_0", "UserFriendlyName":[68,69,76,76,0],"ManufacturerName":[68,69,76,0]}], "sound":[{"PNPDeviceID":"HDAUDIO\\FUNC_01&VEN_10EC&DEV_0295", "Name":"Realtek Audio", "Manufacturer":"Realtek"}],"errors":{}});
        let result = parse_windows(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(
            result.memory_modules[0].memory_type.as_deref(),
            Some("DDR5")
        );
        assert_eq!(result.memory_modules[0].configured_speed_mt_s, Some(4800.0));
        assert_eq!(result.devices.len(), 3);
        let monitor = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::Monitor)
            .unwrap();
        assert_eq!(monitor.name, "DELL");
        let usb = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::UsbController)
            .unwrap();
        assert_eq!(usb.product_id.as_deref(), Some("7a60"));
        assert_eq!(usb.revision.as_deref(), Some("11"));
        assert!(parse_windows(b"{}").is_err());
    }

    #[test]
    fn windows_long_identities_are_distinct_and_joined_before_text_truncation() {
        let prefix = format!("DISPLAY\\DEL1234\\{}", "a".repeat(MAX_HARDWARE_TEXT));
        let monitor_a = format!("{prefix}1");
        let monitor_b = format!("{prefix}2");
        let audio = format!("HDAUDIO\\FUNC_01\\{}", "b".repeat(MAX_HARDWARE_TEXT));
        let report = serde_json::json!({
            "memory": [],
            "pnp": [
                {"PNPDeviceID": monitor_a.to_ascii_lowercase(), "Name": "Generic A", "PNPClass": "Monitor"},
                {"PNPDeviceID": monitor_b.to_ascii_lowercase(), "Name": "Generic B", "PNPClass": "Monitor"},
                {"PNPDeviceID": audio.to_ascii_lowercase(), "Name": "Existing audio", "PNPClass": "Media"}
            ],
            "monitors": [
                {"InstanceName": format!("{}_0", monitor_a.to_ascii_uppercase()), "UserFriendlyName": [68,69,76,76,0]},
                {"InstanceName": format!("{}_12", monitor_b.to_ascii_uppercase()), "UserFriendlyName": [65,67,69,82,0]}
            ],
            "sound": [{"PNPDeviceID": audio.to_ascii_uppercase(), "Name": "Duplicate audio"}],
            "errors": {}
        });
        let result = parse_windows(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(result.devices.len(), 3);
        let id_a = super::super::stable_id(&monitor_a.to_ascii_uppercase());
        let id_b = super::super::stable_id(&monitor_b.to_ascii_uppercase());
        assert_ne!(
            id_a, id_b,
            "shared display-text prefixes are not identities"
        );
        for (id, name) in [(id_a, "DELL"), (id_b, "ACER")] {
            let monitor = result.devices.iter().find(|d| d.id == id).unwrap();
            assert_eq!(monitor.kind, HardwareDeviceKind::Monitor);
            assert_eq!(monitor.name, name);
            assert_eq!(monitor.model.as_deref(), Some(name));
        }
        let audio = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::Audio)
            .unwrap();
        assert_eq!(audio.name, "Existing audio");
        assert!(
            result
                .devices
                .iter()
                .all(|d| d.id.len() <= MAX_HARDWARE_TEXT)
        );
    }

    #[test]
    fn windows_identity_validation_and_fallback_names_remain_bounded() {
        for id in [String::new(), "USB\\bad\nidentity".into(), "a".repeat(4097)] {
            assert!(windows_device_id(&serde_json::json!({"id": id}), "id").is_none());
        }
        let id = format!("usb\\{}", "x".repeat(4092));
        assert_eq!(
            windows_device_id(&serde_json::json!({"id": id}), "id"),
            Some(id.to_ascii_uppercase())
        );
        let report = serde_json::json!({
            "memory": [], "pnp": [{"PNPDeviceID": id, "PNPClass": "USB"}],
            "monitors": [{"InstanceName": "display\\test\\0_", "UserFriendlyName": [65,0]}],
            "sound": [], "errors": {}
        });
        let result = parse_windows(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(result.devices.len(), 2);
        let usb = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::UsbDevice)
            .unwrap();
        assert_eq!(usb.name.len(), MAX_HARDWARE_TEXT);
        let monitor = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::Monitor)
            .unwrap();
        assert_eq!(
            monitor.id, "DISPLAY\\TEST\\0_",
            "an empty suffix is part of the identity"
        );
    }

    #[test]
    fn macos_report_recognizes_unified_memory_nested_devices_and_audio() {
        let report = serde_json::json!({"SPMemoryDataType":[{"SPMemoryDataType":"16 GB","dimm_type":"LPDDR5","dimm_manufacturer":"Apple","dimm_speed":"6400 MHz"}], "SPUSBDataType":[{"_name":"USB Bus","host_controller":"AppleT8112USBXHCI", "_items":[{"_name":"USB DAC","vendor_id":"0x1234","product_id":"0x5678"}]}], "SPBluetoothDataType":[{"controller_properties":{"controller_chipset":"BCM_4388"},"device_connected":[{"_name":"Private phone"}]}], "SPDisplaysDataType":[{"sppci_model":"Apple M2", "spdisplays_ndrvs":[{"_name":"DELL U2723QE"}]}],"SPAudioDataType":[{"_name":"Audio","_items":[{"_name":"MacBook Microphone", "coreaudio_device_manufacturer":"Apple Inc."}]}], "SPThunderboltDataType":[{"device_name":"MacBook", "vendor_name":"Apple Inc."}]});
        let result = parse_macos(&serde_json::to_vec(&report).unwrap()).unwrap();
        let m = &result.memory_modules[0];
        assert_eq!(m.capacity_bytes, Some(16 * 1024 * 1024 * 1024));
        assert_eq!(m.memory_type.as_deref(), Some("LPDDR5"));
        assert_eq!(m.speed_mt_s, None);
        assert_eq!(m.reported_speed.as_deref(), Some("6400 MHz"));
        assert_eq!(result.devices.len(), 6);
        assert!(!result.devices.iter().any(|d| d.name.contains("Private")));
        assert!(result.capabilities.iter().all(|c| c.available));
        assert!(parse_macos(b"not json").is_err());
    }

    #[test]
    fn macos_tahoe_report_retains_usb_hosts_devices_and_thunderbolt_identity() {
        let usb = serde_json::json!([{
            "_name":"USB 3.1 Bus", "Driver":"AppleT8132USBXHCI", "USBKeyHardwareType":"Built-in",
            "_items":[{"_name":"USB3 Hub", "USBDeviceKeyVendorName":"Apple",
                "USBDeviceKeyVendorID":"0x05ac", "USBDeviceKeyProductID":"0x800c",
                "USBDeviceKeyProductVersion":"0x5615", "USBDeviceKeyLinkSpeed":"10 Gb/s",
                "USBDeviceKeySerialNumber":"PRIVATE-USB-SERIAL"}]
        }]);
        let report = serde_json::json!({
            "SPUSBHostDataType":usb, "SPUSBDataType":usb,
            "SPThunderboltDataType":[{"_name":"thunderboltusb4_bus_3",
                "device_name_key":"Mac mini", "vendor_name_key":"Apple Inc.",
                "firmware_version_key":"1.2", "domain_uuid_key":"PRIVATE-DOMAIN-ID"}]
        });
        let result = parse_macos(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(
            result.devices.len(),
            3,
            "USB provider trees must not duplicate devices"
        );
        let host = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::UsbController)
            .unwrap();
        assert_eq!(host.name, "USB 3.1 Bus");
        assert_eq!(host.driver.as_deref(), Some("AppleT8132USBXHCI"));
        assert_eq!(host.model, None);
        let hub = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::UsbDevice)
            .unwrap();
        assert_eq!(hub.vendor.as_deref(), Some("Apple"));
        assert_eq!(hub.vendor_id.as_deref(), Some("0x05ac"));
        assert_eq!(hub.product_id.as_deref(), Some("0x800c"));
        assert_eq!(hub.revision.as_deref(), Some("0x5615"));
        assert_eq!(hub.speed_mbps, Some(10000.0));
        let thunderbolt = result
            .devices
            .iter()
            .find(|d| d.kind == HardwareDeviceKind::Thunderbolt)
            .unwrap();
        assert_eq!(thunderbolt.name, "Mac mini");
        assert_eq!(thunderbolt.model.as_deref(), Some("Mac mini"));
        assert_eq!(thunderbolt.vendor.as_deref(), Some("Apple Inc."));
        assert_eq!(thunderbolt.revision.as_deref(), Some("1.2"));
        assert!(
            result.capabilities[1].available
                && result.capabilities[4].available
                && result.capabilities[5].available
        );
        let encoded = serde_json::to_string(&result.devices).unwrap();
        assert!(!encoded.contains("PRIVATE-"));
        assert_eq!(usb_speed_mbps("480 Mb/s"), Some(480.0));
        assert_eq!(usb_speed_mbps("NaN Gb/s"), None);
        assert_eq!(usb_speed_mbps("10 unknown"), None);
    }
}
