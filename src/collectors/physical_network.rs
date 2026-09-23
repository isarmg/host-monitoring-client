//! Physical network adapters are inventory, separate from logical interface counters.

#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::model::MAX_HARDWARE_NETWORKS;
use crate::model::PhysicalNetworkAdapter;

#[cfg(target_os = "linux")]
pub(super) fn collect() -> Vec<PhysicalNetworkAdapter> {
    collect_linux(std::path::Path::new("/sys"))
}

#[cfg(target_os = "linux")]
fn collect_linux(sysfs: &std::path::Path) -> Vec<PhysicalNetworkAdapter> {
    use std::{collections::HashSet, fs};

    let devices = sysfs.join("devices");
    let Ok(devices) = devices.canonicalize() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(sysfs.join("class/net")) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    let mut result = Vec::new();
    let mut seen_devices = HashSet::new();
    for entry in entries {
        let Some(interface_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(device) = entry.path().join("device").canonicalize() else {
            continue;
        };
        let Ok(relative) = device.strip_prefix(&devices) else {
            continue;
        };
        if relative.starts_with("virtual") {
            continue;
        }
        let id = format!("sysfs:{}", relative.display());
        if id.len() > crate::model::MAX_HARDWARE_TEXT {
            continue;
        }
        if !seen_devices.insert(id.clone()) {
            continue;
        }
        let name = linux_device_name(&device)
            .unwrap_or_else(|| format!("Network adapter {interface_name}"));
        let mac_address = fs::read_to_string(entry.path().join("address"))
            .ok()
            .and_then(|value| normalized_mac(&value));
        let link_speed_mbps = fs::read_to_string(entry.path().join("speed"))
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0);
        result.push(PhysicalNetworkAdapter {
            id,
            name,
            interface_name: Some(interface_name),
            mac_address,
            link_speed_mbps,
            source: "linux-sysfs-net-device".into(),
        });
        if result.len() == MAX_HARDWARE_NETWORKS {
            break;
        }
    }
    result.sort_by(|left, right| left.id.cmp(&right.id));
    result
}

#[cfg(target_os = "linux")]
fn linux_device_name(device: &std::path::Path) -> Option<String> {
    use std::fs;
    let pci = ["vendor", "device"].map(|field| fs::read_to_string(device.join(field)).ok());
    if let [Some(vendor), Some(device)] = pci {
        let vendor = vendor.trim().trim_start_matches("0x");
        let device = device.trim().trim_start_matches("0x");
        if vendor.len() == 4
            && device.len() == 4
            && vendor
                .chars()
                .chain(device.chars())
                .all(|value| value.is_ascii_hexdigit())
        {
            return Some(format!("PCI network adapter {vendor}:{device}"));
        }
    }
    fs::read_to_string(device.join("product"))
        .ok()
        .and_then(|value| clean_text(&value))
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn clean_text(value: &str) -> Option<String> {
    let value: String = value.trim().chars().filter(|ch| !ch.is_control()).collect();
    let mut end = value.len().min(crate::model::MAX_HARDWARE_TEXT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (end > 0).then(|| value[..end].to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn normalized_mac(value: &str) -> Option<String> {
    let value = value.trim();
    let bytes: Vec<_> = value.split(':').collect();
    if bytes.len() != 6
        || bytes.iter().all(|part| *part == "00")
        || !bytes
            .iter()
            .all(|part| part.len() == 2 && part.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(bytes.join(":").to_ascii_lowercase())
}

#[cfg(target_os = "windows")]
pub(super) fn collect() -> Vec<PhysicalNetworkAdapter> {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};

    struct Table(*mut MIB_IF_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: GetIfTable2 allocated this table and ownership is released once.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
    let mut raw = std::ptr::null_mut();
    // SAFETY: the API initializes the output pointer; it owns the allocation until FreeMibTable.
    if unsafe { GetIfTable2(&mut raw) }.0 != 0 || raw.is_null() {
        return Vec::new();
    }
    let table = Table(raw);
    // SAFETY: NumEntries and Table are from one OS-owned allocation. Bound iteration even if the
    // OS reports an unexpectedly large table; MIB_IF_TABLE2 may contain trailing rows.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (*table.0).Table.as_ptr(),
            ((*table.0).NumEntries as usize).min(4096),
        )
    };
    let mut result = Vec::new();
    for row in rows {
        let flags = row.InterfaceAndOperStatusFlags._bitfield;
        if flags & 0x01 == 0 || flags & 0x80 != 0 {
            continue;
        }
        let id = format!("{:?}", row.InterfaceGuid);
        let description = utf16_text(&row.Description);
        let interface_name = utf16_text(&row.Alias);
        let name = description
            .clone()
            .or_else(|| interface_name.clone())
            .unwrap_or_else(|| id.clone());
        let length = (row.PhysicalAddressLength as usize).min(row.PermanentPhysicalAddress.len());
        let permanent = &row.PermanentPhysicalAddress[..length];
        let current = &row.PhysicalAddress[..length];
        let address = if permanent.iter().any(|byte| *byte != 0) {
            permanent
        } else {
            current
        };
        let mac_address =
            (address.len() == 6 && address.iter().any(|byte| *byte != 0)).then(|| {
                address
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(":")
            });
        let speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
        result.push(PhysicalNetworkAdapter {
            id,
            name,
            interface_name,
            mac_address,
            link_speed_mbps: (speed > 0).then_some(speed as f64 / 1_000_000.0),
            source: "windows-if-table2".into(),
        });
        if result.len() == MAX_HARDWARE_NETWORKS {
            break;
        }
    }
    result.sort_by(|left, right| left.id.cmp(&right.id));
    result.dedup_by(|left, right| left.id == right.id);
    result
}

#[cfg(target_os = "windows")]
fn utf16_text(value: &[u16]) -> Option<String> {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    clean_text(&String::from_utf16_lossy(&value[..end]))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(super) fn collect() -> Vec<PhysicalNetworkAdapter> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_or_malformed_mac_is_not_a_physical_address() {
        assert_eq!(normalized_mac("00:00:00:00:00:00"), None);
        assert_eq!(
            normalized_mac("aa:bb:cc:dd:ee:ff"),
            Some("aa:bb:cc:dd:ee:ff".into())
        );
        assert_eq!(normalized_mac("aa:bb:cc:dd:ee:zz"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sysfs_excludes_virtual_interfaces() {
        use std::{fs, os::unix::fs::symlink};
        let root = tempfile::tempdir().unwrap();
        let sysfs = root.path();
        let physical = sysfs.join("devices/pci0000:00/0000:00:1f.6");
        let virtual_device = sysfs.join("devices/virtual/net/veth0");
        fs::create_dir_all(physical.join("net/eth0")).unwrap();
        fs::create_dir_all(physical.join("net/eth1")).unwrap();
        fs::create_dir_all(&virtual_device).unwrap();
        fs::create_dir_all(sysfs.join("class/net")).unwrap();
        symlink(physical.join("net/eth0"), sysfs.join("class/net/eth0")).unwrap();
        symlink(physical.join("net/eth1"), sysfs.join("class/net/eth1")).unwrap();
        symlink(&virtual_device, sysfs.join("class/net/veth0")).unwrap();
        symlink(&physical, physical.join("net/eth0/device")).unwrap();
        symlink(&physical, physical.join("net/eth1/device")).unwrap();
        symlink(&virtual_device, virtual_device.join("device")).unwrap();
        fs::write(physical.join("vendor"), "0x8086\n").unwrap();
        fs::write(physical.join("device"), "0x15f3\n").unwrap();
        fs::write(physical.join("net/eth0/address"), "02:00:00:00:00:01\n").unwrap();
        fs::write(physical.join("net/eth0/speed"), "2500\n").unwrap();
        let result = collect_linux(sysfs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "PCI network adapter 8086:15f3");
        assert_eq!(result[0].interface_name.as_deref(), Some("eth0"));
        assert_eq!(result[0].link_speed_mbps, Some(2500.0));
    }
}
