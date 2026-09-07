use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::model::{CLIENT_REPORT_MAX_GPUS, Capability, CapabilityErrorKind, GpuSnapshot};

use super::{extend_bounded, producer_collection_limit, push_bounded};

const DRM_ROOT: &str = "/sys/class/drm";

pub(super) struct LinuxGpuResult {
    pub gpus: Vec<GpuSnapshot>,
    pub capabilities: Vec<Capability>,
}

#[derive(Clone, Debug)]
struct SysfsError {
    kind: CapabilityErrorKind,
    message: String,
}

impl SysfsError {
    fn io(operation: &str, path: &Path, error: io::Error) -> Self {
        Self {
            kind: classify_io_error(&error),
            message: format!("{operation} {} failed: {error}", path.display()),
        }
    }

    fn invalid(path: &Path, message: impl Into<String>) -> Self {
        Self {
            kind: CapabilityErrorKind::InvalidData,
            message: format!("{}: {}", path.display(), message.into()),
        }
    }
}

#[derive(Default)]
struct VendorCollection {
    gpus: Vec<GpuSnapshot>,
    matched_devices: usize,
    first_error: Option<SysfsError>,
    coverage_error: Option<SysfsError>,
    utilization_error: Option<SysfsError>,
}

impl VendorCollection {
    fn record_coverage_error(&mut self, error: SysfsError) {
        self.coverage_error.get_or_insert_with(|| error.clone());
        self.first_error.get_or_insert(error);
    }

    fn record_card(&mut self, reading: CardReading) {
        if let Some(error) = reading.first_error {
            self.first_error.get_or_insert(error);
        }
        if let Some(error) = reading.utilization_error {
            self.utilization_error.get_or_insert(error);
        }
        push_bounded(&mut self.gpus, reading.snapshot, CLIENT_REPORT_MAX_GPUS);
    }
}

struct CardReading {
    snapshot: GpuSnapshot,
    first_error: Option<SysfsError>,
    utilization_error: Option<SysfsError>,
}

pub(super) fn collect() -> LinuxGpuResult {
    collect_from(Path::new(DRM_ROOT))
}

fn collect_from(root: &Path) -> LinuxGpuResult {
    let mut amd = VendorCollection::default();
    let mut intel = VendorCollection::default();
    let mut inspected_primary_cards = 0;

    match open_directory(root, true) {
        Ok(Some(entries)) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        let error = SysfsError::io("read DRM directory entry in", root, error);
                        amd.record_coverage_error(error.clone());
                        intel.record_coverage_error(error);
                        continue;
                    }
                };
                let card = entry.file_name().to_string_lossy().into_owned();
                if !is_primary_card(&card) {
                    continue;
                }
                if inspected_primary_cards >= producer_collection_limit(CLIENT_REPORT_MAX_GPUS) {
                    let error = enumeration_limit_error(root);
                    amd.record_coverage_error(error.clone());
                    intel.record_coverage_error(error);
                    break;
                }
                inspected_primary_cards += 1;
                let device = match resolve_card_device(root, &entry) {
                    Ok(device) => device,
                    Err(error) => {
                        amd.record_coverage_error(error.clone());
                        intel.record_coverage_error(error);
                        continue;
                    }
                };
                let vendor_path = device.join("vendor");
                let vendor = match read_required_pci_id(&vendor_path) {
                    Ok(value) => value,
                    Err(error) => {
                        amd.record_coverage_error(error.clone());
                        intel.record_coverage_error(error);
                        continue;
                    }
                };
                let target = if vendor.eq_ignore_ascii_case("0x1002") {
                    Some((&mut amd, "amd"))
                } else if vendor.eq_ignore_ascii_case("0x8086") {
                    Some((&mut intel, "intel"))
                } else {
                    None
                };
                let Some((target, vendor_name)) = target else {
                    continue;
                };
                target.matched_devices += 1;
                match collect_card(&card, &device, vendor_name) {
                    Ok(reading) => target.record_card(reading),
                    Err(error) => target.record_coverage_error(error),
                }
            }
        }
        Ok(None) => {}
        Err(error) => {
            amd.record_coverage_error(error.clone());
            intel.record_coverage_error(error);
        }
    }

    let mut capabilities = vec![
        capability_for("gpu.amd", "linux-amdgpu-sysfs", &amd),
        capability_for("gpu.intel", "linux-i915/xe-sysfs", &intel),
        Capability::unavailable(
            "gpu.apple",
            "linux-drm",
            CapabilityErrorKind::NotPresent,
            "Apple GPU telemetry is only relevant on macOS",
        ),
    ];
    capabilities.extend(utilization_capability(
        "gpu.amd.utilization",
        "linux-amdgpu-sysfs",
        &amd,
    ));
    capabilities.extend(utilization_capability(
        "gpu.intel.utilization",
        "linux-i915/xe-sysfs",
        &intel,
    ));
    let mut gpus = amd.gpus;
    extend_bounded(&mut gpus, intel.gpus, CLIENT_REPORT_MAX_GPUS);
    LinuxGpuResult { gpus, capabilities }
}

fn collect_card(card: &str, device: &Path, vendor: &str) -> Result<CardReading, SysfsError> {
    let device_code = read_required_pci_id(&device.join("device"))?;
    let mut first_error = None;
    let mut utilization_error = None;

    let id = match pci_slot(device) {
        Ok(Some(value)) => value,
        Ok(None) => card.to_string(),
        Err(error) => {
            record_first_error(&mut first_error, error);
            card.to_string()
        }
    };
    let utilization_percent = match read_f64(&device.join("gpu_busy_percent")) {
        Ok(value) => value,
        Err(error) => {
            utilization_error = Some(error.clone());
            record_first_error(&mut first_error, error);
            None
        }
    };
    let memory_total_bytes = retain_sysfs(
        read_u64(&device.join("mem_info_vram_total")),
        &mut first_error,
    );
    let memory_used_bytes = retain_sysfs(
        read_u64(&device.join("mem_info_vram_used")),
        &mut first_error,
    );
    let temperature_celsius =
        retain_sysfs(first_hwmon_f64(device, "temp1_input"), &mut first_error)
            .map(|value| value / 1_000.0)
            .filter(|value| value.is_finite());
    let power_watts = retain_sysfs(first_hwmon_f64(device, "power1_average"), &mut first_error)
        .map(|microwatts| microwatts / 1_000_000.0);
    let core_clock_mhz = retain_sysfs(read_f64(&device.join("gt_cur_freq_mhz")), &mut first_error);

    Ok(CardReading {
        snapshot: GpuSnapshot {
            id,
            vendor: vendor.to_string(),
            name: format!("{} {device_code}", vendor.to_uppercase()),
            utilization_percent,
            memory_total_bytes,
            memory_used_bytes,
            temperature_celsius,
            power_watts,
            core_clock_mhz,
            memory_clock_mhz: None,
            pcie_rx_bytes_per_second: None,
            pcie_tx_bytes_per_second: None,
            source: format!("linux-{vendor}-sysfs"),
        },
        first_error,
        utilization_error,
    })
}

fn capability_for(name: &str, source: &str, values: &VendorCollection) -> Capability {
    if let Some(error) = values.first_error.as_ref() {
        Capability::unavailable(
            name,
            source,
            error.kind.clone(),
            format!(
                "collected {} of {} matching DRM devices; {}",
                values.gpus.len(),
                values.matched_devices,
                error.message
            ),
        )
    } else if values.gpus.is_empty() {
        Capability::unavailable(
            name,
            source,
            CapabilityErrorKind::NotPresent,
            "no matching DRM device was exposed by the kernel",
        )
    } else {
        Capability::available(name, source)
    }
}

fn utilization_capability(
    name: &str,
    source: &str,
    values: &VendorCollection,
) -> Option<Capability> {
    if values.matched_devices == 0 && values.gpus.is_empty() {
        return None;
    }
    let error = values
        .utilization_error
        .as_ref()
        .or(values.coverage_error.as_ref());
    Some(if let Some(error) = error {
        Capability::unavailable(name, source, error.kind.clone(), error.message.clone())
    } else if values
        .gpus
        .iter()
        .any(|value| value.utilization_percent.is_some())
    {
        Capability::available(name, source)
    } else {
        Capability::unavailable(
            name,
            source,
            CapabilityErrorKind::Unsupported,
            "the loaded kernel driver exposed no read-only utilization counter",
        )
    })
}

fn is_primary_card(name: &str) -> bool {
    name.strip_prefix("card").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.chars().all(|value| value.is_ascii_digit())
    })
}

fn resolve_card_device(root: &Path, entry: &fs::DirEntry) -> Result<PathBuf, SysfsError> {
    let card_type = entry
        .file_type()
        .map_err(|error| SysfsError::io("inspect DRM entry", &entry.path(), error))?;
    if !card_type.is_dir() && !card_type.is_symlink() {
        return Err(SysfsError::invalid(
            &entry.path(),
            "primary DRM entry is neither a directory nor a sysfs link",
        ));
    }
    let device_link = entry.path().join("device");
    let resolved = fs::canonicalize(&device_link)
        .map_err(|error| SysfsError::io("resolve DRM device", &device_link, error))?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| SysfsError::io("resolve DRM root", root, error))?;
    let allowed = if root == Path::new(DRM_ROOT) {
        resolved.starts_with("/sys/devices")
    } else {
        resolved.starts_with(&canonical_root)
    };
    if !allowed {
        return Err(SysfsError::invalid(
            &device_link,
            format!(
                "resolved outside the allowed sysfs tree to {}",
                resolved.display()
            ),
        ));
    }
    let metadata = fs::metadata(&resolved)
        .map_err(|error| SysfsError::io("inspect DRM device", &resolved, error))?;
    if !metadata.is_dir() {
        return Err(SysfsError::invalid(
            &resolved,
            "DRM device is not a directory",
        ));
    }
    Ok(resolved)
}

fn pci_slot(device: &Path) -> Result<Option<String>, SysfsError> {
    Ok(
        read_optional_trimmed(&device.join("uevent"))?.and_then(|uevent| {
            uevent
                .lines()
                .find_map(|line| line.strip_prefix("PCI_SLOT_NAME=").map(ToOwned::to_owned))
        }),
    )
}

fn first_hwmon_f64(device: &Path, file_name: &str) -> Result<Option<f64>, SysfsError> {
    let hwmon = device.join("hwmon");
    let Some(entries) = open_directory(&hwmon, true)? else {
        return Ok(None);
    };
    for entry in entries {
        let entry = entry.map_err(|error| SysfsError::io("read hwmon entry in", &hwmon, error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| SysfsError::io("inspect hwmon entry", &entry.path(), error))?;
        if !file_type.is_dir() {
            return Err(SysfsError::invalid(
                &entry.path(),
                "hwmon entry is not a directory",
            ));
        }
        if let Some(value) = read_f64(&entry.path().join(file_name))? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn open_directory(
    path: &Path,
    missing_is_optional: bool,
) -> Result<Option<fs::ReadDir>, SysfsError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if missing_is_optional && error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(SysfsError::io("inspect directory", path, error)),
    };
    if !metadata.is_dir() {
        return Err(SysfsError::invalid(
            path,
            "expected a directory, not a link or file",
        ));
    }
    fs::read_dir(path)
        .map(Some)
        .map_err(|error| SysfsError::io("read directory", path, error))
}

fn read_required_pci_id(path: &Path) -> Result<String, SysfsError> {
    let value = read_optional_trimmed(path)?
        .ok_or_else(|| SysfsError::invalid(path, "required PCI identifier is missing"))?;
    let Some(hex) = value.strip_prefix("0x") else {
        return Err(SysfsError::invalid(
            path,
            "PCI identifier must start with 0x",
        ));
    };
    if hex.len() != 4 || !hex.chars().all(|value| value.is_ascii_hexdigit()) {
        return Err(SysfsError::invalid(
            path,
            "PCI identifier must contain exactly four hexadecimal digits",
        ));
    }
    Ok(value)
}

fn read_optional_trimmed(path: &Path) -> Result<Option<String>, SysfsError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SysfsError::io("inspect sysfs attribute", path, error)),
    };
    if !metadata.is_file() {
        return Err(SysfsError::invalid(
            path,
            "sysfs attribute is not a regular file",
        ));
    }
    let value = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SysfsError::io("read sysfs attribute", path, error)),
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(SysfsError::invalid(path, "sysfs attribute is empty"));
    }
    Ok(Some(value.to_string()))
}

fn read_u64(path: &Path) -> Result<Option<u64>, SysfsError> {
    read_optional_trimmed(path)?
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                SysfsError::invalid(path, format!("invalid unsigned integer: {value}"))
            })
        })
        .transpose()
}

fn read_f64(path: &Path) -> Result<Option<f64>, SysfsError> {
    read_optional_trimmed(path)?
        .map(|value| {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| SysfsError::invalid(path, format!("invalid number: {value}")))?;
            if parsed.is_finite() {
                Ok(parsed)
            } else {
                Err(SysfsError::invalid(path, "number is not finite"))
            }
        })
        .transpose()
}

fn retain_sysfs<T>(
    result: Result<Option<T>, SysfsError>,
    first_error: &mut Option<SysfsError>,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            record_first_error(first_error, error);
            None
        }
    }
}

fn record_first_error(first_error: &mut Option<SysfsError>, error: SysfsError) {
    if first_error.is_none() {
        *first_error = Some(error);
    }
}

fn classify_io_error(error: &io::Error) -> CapabilityErrorKind {
    match error.kind() {
        io::ErrorKind::PermissionDenied => CapabilityErrorKind::PermissionDenied,
        io::ErrorKind::NotFound | io::ErrorKind::InvalidData => CapabilityErrorKind::InvalidData,
        _ => CapabilityErrorKind::Transient,
    }
}

fn enumeration_limit_error(root: &Path) -> SysfsError {
    SysfsError::invalid(
        root,
        format!(
            "primary-card enumeration exceeded producer limit of {} inspected devices",
            producer_collection_limit(CLIENT_REPORT_MAX_GPUS)
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestTree {
        base: PathBuf,
        drm: PathBuf,
    }

    impl TestTree {
        fn new() -> Self {
            let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir()
                .canonicalize()
                .expect("physical test temporary directory")
                .join(format!(
                    "host-monitoring-linux-gpu-test-{}-{id}",
                    std::process::id()
                ));
            let drm = base.join("drm");
            fs::create_dir_all(&drm).unwrap();
            Self { base, drm }
        }

        fn add_card(&self, index: u32, vendor: &str, device_id: &str) -> PathBuf {
            let device = self.drm.join(format!("card{index}/device"));
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("vendor"), vendor).unwrap();
            fs::write(device.join("device"), device_id).unwrap();
            device
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn capability<'a>(result: &'a LinuxGpuResult, name: &str) -> &'a Capability {
        result
            .capabilities
            .iter()
            .find(|capability| capability.name == name)
            .unwrap()
    }

    #[test]
    fn excludes_render_nodes_and_connectors() {
        assert!(is_primary_card("card0"));
        assert!(!is_primary_card("card0-DP-1"));
        assert!(!is_primary_card("renderD128"));
    }

    #[test]
    fn absent_optional_attributes_remain_absent() {
        let tree = TestTree::new();
        tree.add_card(0, "0x1002", "0x73bf");

        let result = collect_from(&tree.drm);
        assert_eq!(result.gpus.len(), 1);
        assert!(capability(&result, "gpu.amd").available);
        let utilization = capability(&result, "gpu.amd.utilization");
        assert_eq!(
            utilization.error_kind,
            Some(CapabilityErrorKind::Unsupported)
        );
    }

    #[test]
    fn malformed_telemetry_preserves_partial_snapshots_and_marks_capability() {
        let tree = TestTree::new();
        let good = tree.add_card(0, "0x1002", "0x73bf");
        fs::write(good.join("gpu_busy_percent"), "42").unwrap();
        let malformed = tree.add_card(1, "0x1002", "0x73df");
        fs::write(malformed.join("gpu_busy_percent"), "not-a-number").unwrap();

        let result = collect_from(&tree.drm);
        assert_eq!(
            result.gpus.len(),
            2,
            "successful identity snapshots are retained"
        );
        let amd = capability(&result, "gpu.amd");
        assert!(!amd.available);
        assert_eq!(amd.error_kind, Some(CapabilityErrorKind::InvalidData));
        let utilization = capability(&result, "gpu.amd.utilization");
        assert!(!utilization.available);
        assert_eq!(
            utilization.error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }

    #[test]
    fn io_error_classification_is_stable_under_root() {
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::PermissionDenied)),
            CapabilityErrorKind::PermissionDenied
        );
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::InvalidData)),
            CapabilityErrorKind::InvalidData
        );
        assert_eq!(
            classify_io_error(&io::Error::from(io::ErrorKind::NotFound)),
            CapabilityErrorKind::InvalidData
        );
        assert_eq!(
            classify_io_error(&io::Error::other("injected I/O failure")),
            CapabilityErrorKind::Transient
        );
    }

    #[test]
    fn primary_card_limit_is_an_explicit_coverage_error() {
        let error = enumeration_limit_error(Path::new("/injected/drm"));
        assert_eq!(error.kind, CapabilityErrorKind::InvalidData);
        assert!(
            error
                .message
                .contains("primary-card enumeration exceeded producer limit")
        );
    }

    #[cfg(unix)]
    #[test]
    fn card_link_cannot_escape_injected_sysfs_root() {
        use std::os::unix::fs::symlink;

        let tree = TestTree::new();
        let outside = tree.base.join("outside/card0/device");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("vendor"), "0x1002").unwrap();
        fs::write(outside.join("device"), "0x73bf").unwrap();
        symlink(tree.base.join("outside/card0"), tree.drm.join("card0")).unwrap();

        let result = collect_from(&tree.drm);
        assert!(result.gpus.is_empty());
        assert_eq!(
            capability(&result, "gpu.amd").error_kind,
            Some(CapabilityErrorKind::InvalidData)
        );
    }
}
