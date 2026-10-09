use super::{Inventory, capability, device, smbios};
use crate::model::*;
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

fn classify(error: &io::Error) -> CapabilityErrorKind {
    match error.kind() {
        io::ErrorKind::PermissionDenied => CapabilityErrorKind::PermissionDenied,
        io::ErrorKind::NotFound => CapabilityErrorKind::NotPresent,
        io::ErrorKind::InvalidData => CapabilityErrorKind::InvalidData,
        _ => CapabilityErrorKind::Transient,
    }
}

fn read(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inventory output limit",
        ));
    }
    Ok(bytes)
}
fn attr(path: &Path, name: &str) -> Option<String> {
    super::super::hardware::text(&String::from_utf8_lossy(
        &read(&path.join(name), 4096).ok()?,
    ))
}
fn entries(path: &Path) -> Result<Vec<PathBuf>, CapabilityErrorKind> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(|e| classify(&e))?
        .take(4096)
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<_>>()
        .map_err(|e| classify(&e))?;
    entries.sort();
    Ok(entries)
}
fn driver(path: &Path) -> Option<String> {
    fs::read_link(path.join("driver"))
        .ok()?
        .file_name()?
        .to_str()
        .and_then(super::super::hardware::text)
}
fn hex(value: Option<String>, width: usize) -> Option<String> {
    let value = value?;
    let value = value.trim_start_matches("0x");
    (value.len() == width && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}
fn number(value: Option<String>) -> Option<f64> {
    value?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}
fn physical(path: &Path, sysfs: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    let root = sysfs.join("devices").canonicalize().ok()?;
    let relative = path.strip_prefix(&root).ok()?;
    (!relative.starts_with("virtual")).then_some(path)
}

/// Resolve local pci.ids/usb.ids only; never download a database during sampling.
#[derive(Default)]
struct Ids {
    vendors: HashMap<String, String>,
    products: HashMap<(String, String), String>,
}
impl Ids {
    fn load(paths: &[&str]) -> Self {
        paths
            .iter()
            .find_map(|p| read(Path::new(p), 4 * 1024 * 1024).ok())
            .map(|bytes| Self::parse(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default()
    }
    fn parse(value: &str) -> Self {
        let mut ids = Self::default();
        let mut vendor = None;
        for line in value.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if !line.starts_with('\t') {
                vendor = None;
                if let Some((id, name)) = line.split_once("  ")
                    && let Some(id) = hex(Some(id.into()), 4)
                    && let Some(name) = super::super::hardware::text(name)
                {
                    ids.vendors.insert(id.clone(), name);
                    vendor = Some(id);
                }
            } else if !line.starts_with("\t\t")
                && let (Some(vendor), Some((id, name))) = (&vendor, line[1..].split_once("  "))
                && let (Some(id), Some(name)) =
                    (hex(Some(id.into()), 4), super::super::hardware::text(name))
            {
                ids.products.insert((vendor.clone(), id), name);
            }
        }
        ids
    }
    fn identify(&self, item: &mut HardwareDevice) {
        if let Some(vendor) = &item.vendor_id {
            if item.vendor.is_none() {
                item.vendor = self.vendors.get(vendor).cloned();
            }
            if item.model.is_none() {
                item.model = item
                    .product_id
                    .as_ref()
                    .and_then(|p| self.products.get(&(vendor.clone(), p.clone())))
                    .cloned();
            }
        }
    }
}

pub(super) fn collect(sysfs: &Path, procfs: &Path) -> Inventory {
    let pci = Ids::load(&[
        "/usr/share/hwdata/pci.ids",
        "/usr/share/misc/pci.ids",
        "/usr/share/pci.ids",
    ]);
    let usb = Ids::load(&[
        "/usr/share/hwdata/usb.ids",
        "/usr/share/misc/usb.ids",
        "/usr/share/usb.ids",
    ]);
    collect_with_ids(sysfs, procfs, &pci, &usb)
}

fn collect_with_ids(sysfs: &Path, procfs: &Path, pci: &Ids, usb: &Ids) -> Inventory {
    let (memory_modules, memory_failure) =
        match read(&sysfs.join("firmware/dmi/tables/DMI"), 1024 * 1024) {
            Ok(bytes) => smbios::parse(&bytes),
            Err(e) => (vec![], Some(classify(&e))),
        };
    let mut devices = Vec::new();
    let mut failures = [const { None }; 6];
    match entries(&sysfs.join("bus/pci/devices")) {
        Err(kind) => {
            failures[3] = Some(kind);
        }
        Ok(paths) => {
            for path in paths {
                let Some(path) = physical(&path, sysfs) else {
                    continue;
                };
                let class = attr(&path, "class").unwrap_or_default();
                let mut item = device(
                    &format!("pci:{}", path.file_name().unwrap().to_string_lossy()),
                    HardwareDeviceKind::UsbController,
                    "PCI controller",
                    "linux-sysfs-pci",
                );
                item.vendor_id = hex(attr(&path, "vendor"), 4);
                item.product_id = hex(attr(&path, "device"), 4);
                item.revision = hex(attr(&path, "revision"), 2);
                item.driver = driver(&path);
                item.bus = Some("PCI".into());
                pci.identify(&mut item);
                let model = item.model.as_deref().unwrap_or("").to_ascii_lowercase();
                let kind = if class.starts_with("0x0c03") {
                    Some(HardwareDeviceKind::UsbController)
                } else if class.starts_with("0x0401") || class.starts_with("0x0403") {
                    Some(HardwareDeviceKind::Audio)
                } else if item.driver.as_deref() == Some("thunderbolt")
                    || model.contains("thunderbolt")
                    || model.contains("usb4")
                {
                    Some(HardwareDeviceKind::Thunderbolt)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    item.kind = kind;
                    item.name = item.model.clone().unwrap_or_else(|| {
                        format!(
                            "PCI {} {}:{}",
                            match kind {
                                HardwareDeviceKind::Audio => "audio",
                                HardwareDeviceKind::Thunderbolt => "Thunderbolt",
                                _ => "USB controller",
                            },
                            item.vendor_id.as_deref().unwrap_or("????"),
                            item.product_id.as_deref().unwrap_or("????")
                        )
                    });
                    devices.push(item);
                }
            }
        }
    }
    match entries(&sysfs.join("bus/usb/devices")) {
        Err(kind) => failures[4] = Some(kind),
        Ok(paths) => {
            for path in paths {
                let Some(path) = physical(&path, sysfs) else {
                    continue;
                };
                if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().contains(':'))
                {
                    continue;
                }
                let Some(vendor_id) = hex(attr(&path, "idVendor"), 4) else {
                    continue;
                };
                let mut item = device(
                    &format!("usb:{}", path.file_name().unwrap().to_string_lossy()),
                    HardwareDeviceKind::UsbDevice,
                    "USB device",
                    "linux-sysfs-usb",
                );
                item.vendor_id = Some(vendor_id);
                item.product_id = hex(attr(&path, "idProduct"), 4);
                item.vendor = attr(&path, "manufacturer");
                item.model = attr(&path, "product");
                item.version = attr(&path, "version");
                item.revision = attr(&path, "bcdDevice");
                item.bus = Some("USB".into());
                item.driver = driver(&path);
                item.speed_mbps = number(attr(&path, "speed"));
                usb.identify(&mut item);
                item.name = item.model.clone().unwrap_or_else(|| {
                    format!(
                        "USB {}:{}",
                        item.vendor_id.as_deref().unwrap_or("????"),
                        item.product_id.as_deref().unwrap_or("????")
                    )
                });
                devices.push(item);
            }
        }
    }
    match entries(&sysfs.join("bus/thunderbolt/devices")) {
        Err(kind) => failures[0] = Some(kind),
        Ok(paths) => {
            for path in paths {
                let key = path.file_name().unwrap().to_string_lossy();
                if !key.contains('-') || key.contains(':') {
                    continue;
                }
                let Some(path) = physical(&path, sysfs) else {
                    continue;
                };
                let mut item = device(
                    &format!("thunderbolt:{key}"),
                    HardwareDeviceKind::Thunderbolt,
                    "Thunderbolt router",
                    "linux-sysfs-thunderbolt",
                );
                item.model = attr(&path, "device_name");
                item.vendor = attr(&path, "vendor_name");
                item.vendor_id = attr(&path, "vendor");
                item.product_id = attr(&path, "device");
                item.revision = attr(&path, "nvm_version");
                item.version = attr(&path, "generation").map(|v| format!("Thunderbolt {v}"));
                item.bus = Some("Thunderbolt/USB4".into());
                item.name = item.model.clone().unwrap_or(item.name);
                item.driver = driver(&path);
                devices.push(item);
            }
        }
    }
    match entries(&sysfs.join("class/drm")) {
        Err(kind) => failures[1] = Some(kind),
        Ok(paths) => {
            for path in paths {
                if attr(&path, "status").as_deref() != Some("connected") {
                    continue;
                }
                let Some(path) = physical(&path, sysfs) else {
                    continue;
                };
                let key = path.file_name().unwrap().to_string_lossy();
                let mut item = device(
                    &format!("drm:{key}"),
                    HardwareDeviceKind::Monitor,
                    &key,
                    "linux-drm-edid",
                );
                item.connection = Some(key.into_owned());
                item.bus = Some("Display".into());
                match read(&path.join("edid"), 32 * 1024) {
                    Ok(bytes) if !bytes.is_empty() => {
                        if let Some((vendor, product, name, version)) = edid(&bytes) {
                            item.vendor = Some(vendor.clone());
                            item.vendor_id = Some(vendor);
                            item.product_id = Some(product);
                            item.model = name;
                            item.version = Some(version);
                            item.name = item.model.clone().unwrap_or(item.name);
                        } else {
                            failures[1] = Some(CapabilityErrorKind::InvalidData);
                        }
                    }
                    Err(e) => failures[1] = Some(classify(&e)),
                    _ => {}
                }
                devices.push(item);
            }
        }
    }
    match entries(&sysfs.join("class/bluetooth")) {
        Err(kind) => failures[2] = Some(kind),
        Ok(paths) => {
            for path in paths {
                let key = path.file_name().unwrap().to_string_lossy();
                if !key.starts_with("hci")
                    || !key[3..].bytes().all(|v| v.is_ascii_digit())
                    || key.len() == 3
                {
                    continue;
                }
                let Some(path) = physical(&path.join("device"), sysfs) else {
                    continue;
                };
                let mut item = device(
                    &format!("bluetooth:{key}:{}", path.display()),
                    HardwareDeviceKind::Bluetooth,
                    &format!("Bluetooth {key}"),
                    "linux-sysfs-bluetooth",
                );
                identify_parent(&path, sysfs, pci, usb, &mut item);
                item.name = item.model.clone().unwrap_or(item.name);
                devices.push(item);
            }
        }
    }
    match entries(&sysfs.join("class/sound")) {
        Err(kind) => failures[5] = Some(kind),
        Ok(paths) => {
            for path in paths {
                let key = path.file_name().unwrap().to_string_lossy();
                let Some(index) = key
                    .strip_prefix("card")
                    .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                else {
                    continue;
                };
                let Some(path) = physical(&path.join("device"), sysfs) else {
                    continue;
                };
                let mut item = device(
                    &format!("alsa:{key}:{}", path.display()),
                    HardwareDeviceKind::Audio,
                    &format!("Audio {key}"),
                    "linux-alsa",
                );
                identify_parent(&path, sysfs, pci, usb, &mut item);
                let card = procfs.join("asound").join(format!("card{index}"));
                let label = attr(&card, "id");
                // HDA codec name identifies the audio chip rather than only the PCI bridge.
                if let Ok(files) = entries(&card) {
                    for codec in files.into_iter().filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with("codec#"))
                    }) {
                        if let Ok(bytes) = read(&codec, 64 * 1024)
                            && let Some(model) =
                                String::from_utf8_lossy(&bytes).lines().find_map(|line| {
                                    line.strip_prefix("Codec: ")
                                        .and_then(super::super::hardware::text)
                                })
                        {
                            let mut codec_item = item.clone();
                            codec_item.id = super::stable_id(&format!(
                                "alsa:{key}:{}",
                                codec.file_name().unwrap().to_string_lossy()
                            ));
                            codec_item.model = Some(model.clone());
                            codec_item.name = model;
                            // The codec may be from a different vendor than its PCI controller.
                            codec_item.vendor = None;
                            codec_item.vendor_id = None;
                            codec_item.product_id = None;
                            devices.push(codec_item);
                        }
                    }
                }
                item.name = item.model.clone().or(label).unwrap_or(item.name);
                devices.push(item);
            }
        }
    }
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
    let mut capabilities = vec![capability(
        "hardware.memory",
        "linux-smbios",
        memory_modules.len(),
        memory_failure,
    )];
    for (i, kind) in kinds.iter().enumerate() {
        // The Thunderbolt bus can be absent while a PCI NHI is identified.
        let count = devices.iter().filter(|d| d.kind == *kind).count();
        let failure = failures[i]
            .clone()
            .filter(|f| count == 0 || *f != CapabilityErrorKind::NotPresent);
        capabilities.push(capability(
            super::NAMES[i + 1],
            "linux-sysfs",
            count,
            failure,
        ));
    }
    Inventory {
        collected_at: Some(chrono::Utc::now()),
        memory_modules,
        devices,
        capabilities,
    }
}

fn identify_parent(path: &Path, sysfs: &Path, pci: &Ids, usb: &Ids, item: &mut HardwareDevice) {
    for parent in path
        .ancestors()
        .take_while(|p| p.starts_with(sysfs.join("devices")))
    {
        item.driver = item.driver.clone().or_else(|| driver(parent));
        if let Some(vendor) = hex(attr(parent, "idVendor"), 4) {
            item.vendor_id = Some(vendor);
            item.product_id = hex(attr(parent, "idProduct"), 4);
            item.model = attr(parent, "product");
            item.vendor = attr(parent, "manufacturer");
            item.bus = Some("USB".into());
            item.version = attr(parent, "version");
            item.revision = attr(parent, "bcdDevice");
            usb.identify(item);
            break;
        }
        if let Some(vendor) = hex(attr(parent, "vendor"), 4) {
            item.vendor_id = Some(vendor);
            item.product_id = hex(attr(parent, "device"), 4);
            item.bus = Some("PCI".into());
            item.revision = hex(attr(parent, "revision"), 2);
            pci.identify(item);
            break;
        }
    }
}

fn edid(bytes: &[u8]) -> Option<(String, String, Option<String>, String)> {
    let block = bytes.get(..128)?;
    if block[..8] != [0, 255, 255, 255, 255, 255, 255, 0]
        || block.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0
    {
        return None;
    }
    let code = u16::from_be_bytes([block[8], block[9]]);
    let letters = [(code >> 10) & 31, (code >> 5) & 31, code & 31];
    if letters.iter().any(|v| !(1..=26).contains(v)) {
        return None;
    }
    let vendor: String = letters
        .iter()
        .map(|v| (b'A' + *v as u8 - 1) as char)
        .collect();
    let product = format!("{:04x}", u16::from_le_bytes([block[10], block[11]]));
    let name = block[54..126].as_chunks::<18>().0.iter().find_map(|d| {
        (d[..5] == [0, 0, 0, 0xfc, 0])
            .then(|| super::super::hardware::text(&String::from_utf8_lossy(&d[5..18])))
            .flatten()
    });
    Some((
        vendor,
        product,
        name,
        format!("EDID {}.{}", block[18], block[19]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edid_validates_checksum_and_uses_monitor_descriptor() {
        let mut block = [0u8; 128];
        block[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        block[8..10].copy_from_slice(&((4u16 << 10) | (5 << 5) | 12).to_be_bytes());
        block[10] = 0x34;
        block[11] = 0x12;
        block[18] = 1;
        block[19] = 4;
        block[54..59].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
        block[59..72].copy_from_slice(b"DELL U2723QE\n");
        block[127] = 0u8.wrapping_sub(block[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        assert_eq!(
            edid(&block),
            Some((
                "DEL".into(),
                "1234".into(),
                Some("DELL U2723QE".into()),
                "EDID 1.4".into()
            ))
        );
        block[20] ^= 1;
        assert!(edid(&block).is_none());
        assert!(edid(&block[..120]).is_none());
    }
    #[test]
    fn sysfs_identifies_usb_chip_bluetooth_and_audio_and_excludes_virtual_cards() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let sys = root.path().join("sys");
        let procfs = root.path().join("proc");
        let controller = sys.join("devices/pci0000:00/0000:00:14.0");
        let radio = controller.join("usb1/1-1");
        let bt = radio.join("1-1:1.0");
        let audio = controller.join("usb1/1-2");
        for path in [
            &controller,
            &radio,
            &bt,
            &audio,
            &sys.join("class/bluetooth/hci0"),
            &sys.join("class/sound/card0"),
            &sys.join("class/sound/card1"),
            &sys.join("devices/virtual/sound/card1"),
            &sys.join("bus/pci/devices"),
            &sys.join("bus/usb/devices"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        for (path, file, value) in [
            (&controller, "class", "0x0c0330"),
            (&controller, "vendor", "0x8086"),
            (&controller, "device", "0x7a60"),
            (&radio, "idVendor", "8087"),
            (&radio, "idProduct", "0033"),
            (&audio, "idVendor", "1234"),
            (&audio, "idProduct", "5678"),
            (&audio, "product", "USB Audio DAC"),
            (&audio, "speed", "480"),
        ] {
            fs::write(path.join(file), value).unwrap();
        }
        symlink(&controller, sys.join("bus/pci/devices/0000:00:14.0")).unwrap();
        symlink(&radio, sys.join("bus/usb/devices/1-1")).unwrap();
        symlink(&audio, sys.join("bus/usb/devices/1-2")).unwrap();
        symlink(&bt, sys.join("class/bluetooth/hci0/device")).unwrap();
        symlink(&audio, sys.join("class/sound/card0/device")).unwrap();
        symlink(
            sys.join("devices/virtual/sound/card1"),
            sys.join("class/sound/card1/device"),
        )
        .unwrap();
        let pci = Ids::parse("8086  Intel\n\t7a60  Raptor Lake USB 3.2 Controller\n");
        let usb = Ids::parse("8087  Intel\n\t0033  AX211 Bluetooth\n");
        let result = collect_with_ids(&sys, &procfs, &pci, &usb);
        assert_eq!(
            result
                .devices
                .iter()
                .find(|d| d.kind == HardwareDeviceKind::UsbController)
                .unwrap()
                .model
                .as_deref(),
            Some("Raptor Lake USB 3.2 Controller")
        );
        assert_eq!(
            result
                .devices
                .iter()
                .find(|d| d.kind == HardwareDeviceKind::Bluetooth)
                .unwrap()
                .model
                .as_deref(),
            Some("AX211 Bluetooth")
        );
        let cards: Vec<_> = result
            .devices
            .iter()
            .filter(|d| d.kind == HardwareDeviceKind::Audio)
            .collect();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].model.as_deref(), Some("USB Audio DAC"));
        assert_eq!(
            result.capabilities[0].error_kind,
            Some(CapabilityErrorKind::NotPresent)
        );
        assert!(
            result
                .capabilities
                .iter()
                .find(|c| c.name == "hardware.audio")
                .unwrap()
                .available
        );
    }
}
