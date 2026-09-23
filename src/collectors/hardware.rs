use crate::model::*;
use chrono::Utc;
use sysinfo::{Networks, System};

pub(super) fn text(value: &str) -> Option<String> {
    let value: String = value.trim().chars().filter(|c| !c.is_control()).collect();
    let mut end = value.len().min(MAX_HARDWARE_TEXT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (end > 0).then(|| value[..end].to_owned())
}

pub(super) fn collect(system: &System, networks: &Networks) -> (HardwareSnapshot, Capability) {
    let frequencies: Vec<_> = system
        .cpus()
        .iter()
        .take(CLIENT_REPORT_MAX_CPU_CORES)
        .map(|cpu| (cpu.frequency() > 0).then_some(cpu.frequency() as f64))
        .collect();
    let valid: Vec<_> = frequencies.iter().flatten().copied().collect();
    let cpu = CpuHardware {
        model: system.cpus().first().and_then(|cpu| text(cpu.brand())),
        vendor: system.cpus().first().and_then(|cpu| text(cpu.vendor_id())),
        frequency_mhz: (!valid.is_empty()).then(|| valid.iter().sum::<f64>() / valid.len() as f64),
        max_frequency_mhz: max_cpu_frequency(),
        per_core_frequency_mhz: frequencies,
        load_average: {
            #[cfg(unix)]
            {
                let load = System::load_average();
                Some([load.one, load.five, load.fifteen])
            }
            #[cfg(not(unix))]
            {
                None
            }
        },
    };
    let mut network_hardware: Vec<_> = networks
        .iter()
        .take(MAX_HARDWARE_NETWORKS)
        .map(|(name, data)| {
            let mac = data.mac_address().to_string();
            NetworkHardware {
                name: text(name).unwrap_or_else(|| "unknown".into()),
                mac_address: (mac != "00:00:00:00:00:00").then_some(mac),
                ip_addresses: data
                    .ip_networks()
                    .iter()
                    .take(64)
                    .map(ToString::to_string)
                    .collect(),
                mtu: (data.mtu() > 0).then_some(data.mtu()),
                link_speed_mbps: network_speed(name),
                operational_state: network_state(name),
            }
        })
        .collect();
    network_hardware.sort_by(|a, b| a.name.cmp(&b.name));
    let (sensors, capability) = collect_sensors();
    (
        HardwareSnapshot {
            collected_at: Utc::now(),
            cpu,
            networks: network_hardware,
            sensors,
            disk_health: vec![],
        },
        capability,
    )
}

#[cfg(target_os = "linux")]
fn max_cpu_frequency() -> Option<f64> {
    let root = std::path::Path::new("/sys/devices/system/cpu/cpufreq");
    std::fs::read_dir(root)
        .ok()?
        .take(CLIENT_REPORT_MAX_CPU_CORES)
        .filter_map(Result::ok)
        .filter_map(|p| read_number(&p.path().join("cpuinfo_max_freq")))
        .filter(|v| *v > 0.0)
        .reduce(f64::max)
        .map(|v| v / 1000.0)
}
#[cfg(not(target_os = "linux"))]
fn max_cpu_frequency() -> Option<f64> {
    None
}

#[cfg(target_os = "linux")]
fn network_path(name: &str, field: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    Some(
        std::path::Path::new("/sys/class/net")
            .join(name)
            .join(field),
    )
}
#[cfg(target_os = "linux")]
fn network_speed(name: &str) -> Option<f64> {
    read_number(&network_path(name, "speed")?).filter(|v| *v > 0.0)
}
#[cfg(not(target_os = "linux"))]
fn network_speed(_: &str) -> Option<f64> {
    None
}
#[cfg(target_os = "linux")]
fn network_state(name: &str) -> Option<String> {
    text(&std::fs::read_to_string(network_path(name, "operstate")?).ok()?)
}
#[cfg(not(target_os = "linux"))]
fn network_state(_: &str) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn read_number(path: &std::path::Path) -> Option<f64> {
    let value = std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    value.is_finite().then_some(value)
}

#[cfg(not(target_os = "linux"))]
fn collect_sensors() -> (Vec<HardwareSensor>, Capability) {
    (
        vec![],
        Capability::unavailable(
            "hardware.sensors",
            "platform",
            CapabilityErrorKind::Unsupported,
            "this platform has no enabled public motherboard sensor provider",
        ),
    )
}
#[cfg(target_os = "linux")]
fn collect_sensors() -> (Vec<HardwareSensor>, Capability) {
    sensors_from(
        std::path::Path::new("/sys/class/hwmon"),
        std::path::Path::new("/sys/devices"),
    )
}

#[cfg(target_os = "linux")]
fn sensor_kind(name: &str) -> Option<(SensorKind, f64, &str)> {
    let stem = name
        .strip_suffix("_input")
        .or_else(|| name.strip_suffix("_average"))?;
    for (prefix, kind, divisor) in [
        ("fan", SensorKind::FanRpm, 1.0),
        ("in", SensorKind::VoltageVolts, 1000.0),
        ("curr", SensorKind::CurrentAmps, 1000.0),
        ("power", SensorKind::PowerWatts, 1_000_000.0),
        ("energy", SensorKind::EnergyJoules, 1_000_000.0),
    ] {
        if name.ends_with("_average") && prefix != "power" {
            continue;
        }
        if stem
            .strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            return Some((kind, divisor, stem));
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn sensors_from(
    root: &std::path::Path,
    allowed: &std::path::Path,
) -> (Vec<HardwareSensor>, Capability) {
    use std::{fs, io::ErrorKind};
    let mut sensors = Vec::new();
    let mut failure = None;
    let classify = |e: std::io::Error| match e.kind() {
        ErrorKind::PermissionDenied => CapabilityErrorKind::PermissionDenied,
        ErrorKind::NotFound => CapabilityErrorKind::NotPresent,
        _ => CapabilityErrorKind::Transient,
    };
    let mut seen = std::collections::HashSet::new();
    match fs::read_dir(root) {
        Err(e) => {
            failure = Some(classify(e));
        }
        Ok(entries) => {
            for entry in entries.take(MAX_HARDWARE_SENSORS) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        failure.get_or_insert(classify(e));
                        continue;
                    }
                };
                let path = match fs::canonicalize(entry.path()) {
                    Ok(p) if p.starts_with(allowed) => p,
                    Ok(_) => {
                        failure.get_or_insert(CapabilityErrorKind::InvalidData);
                        continue;
                    }
                    Err(e) => {
                        failure.get_or_insert(classify(e));
                        continue;
                    }
                };
                if !seen.insert(path.clone()) {
                    continue;
                }
                let chip = fs::read_to_string(path.join("name"))
                    .ok()
                    .and_then(|s| text(&s))
                    .unwrap_or_else(|| "hwmon".into());
                // hwmonN numbers can change across boots; use physical parent and chip identity.
                let device = fs::canonicalize(path.join("device"))
                    .ok()
                    .unwrap_or_else(|| path.parent().unwrap_or(&path).to_owned());
                let entries = match fs::read_dir(&path) {
                    Ok(e) => e,
                    Err(e) => {
                        failure.get_or_insert(classify(e));
                        continue;
                    }
                };
                for attr in entries.take(8192).flatten() {
                    let name = attr.file_name().to_string_lossy().into_owned();
                    let Some((kind, divisor, stem)) = sensor_kind(&name) else {
                        continue;
                    };
                    if name.ends_with("_average") && path.join(format!("{stem}_input")).exists() {
                        continue;
                    }
                    if read_number(&path.join(format!("{stem}_enable"))) == Some(0.0)
                        || read_number(&path.join(format!("{stem}_fault"))) == Some(1.0)
                    {
                        continue;
                    }
                    let value = match fs::read_to_string(attr.path()) {
                        Ok(s) => match s.trim().parse::<f64>() {
                            Ok(v)
                                if v.is_finite()
                                    && (v >= 0.0
                                        || matches!(
                                            kind,
                                            SensorKind::VoltageVolts | SensorKind::CurrentAmps
                                        )) =>
                            {
                                v / divisor
                            }
                            _ => {
                                failure.get_or_insert(CapabilityErrorKind::InvalidData);
                                continue;
                            }
                        },
                        Err(e) => {
                            failure.get_or_insert(classify(e));
                            continue;
                        }
                    };
                    let label = fs::read_to_string(path.join(format!("{stem}_label")))
                        .ok()
                        .and_then(|s| text(&s))
                        .unwrap_or_else(|| format!("{chip} {stem}"));
                    // Stable digest keeps long physical paths within the wire text bound.
                    use sha2::{Digest, Sha256};
                    let identity = format!("{}:{chip}:{stem}", device.display());
                    let id = format!("hwmon:{:x}", Sha256::digest(identity.as_bytes()));
                    sensors.push(HardwareSensor {
                        id,
                        label,
                        kind,
                        value,
                        source: "linux-hwmon".into(),
                    });
                    if sensors.len() >= MAX_HARDWARE_SENSORS {
                        break;
                    }
                }
                if sensors.len() >= MAX_HARDWARE_SENSORS {
                    break;
                }
            }
        }
    }
    sensors.sort_by(|a, b| a.id.cmp(&b.id));
    sensors.dedup_by(|a, b| a.id == b.id);
    let capability = match failure {
        Some(kind) => Capability::unavailable(
            "hardware.sensors",
            "linux-hwmon",
            kind,
            "some hwmon readings could not be collected; readable sensors are retained",
        ),
        None if sensors.is_empty() => Capability::unavailable(
            "hardware.sensors",
            "linux-hwmon",
            CapabilityErrorKind::NotPresent,
            "no fan, voltage, current, power or energy sensors exposed",
        ),
        None => Capability::available("hardware.sensors", "linux-hwmon"),
    };
    (sensors, capability)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn hwmon_units_faults_and_negative_voltages() {
        let dir = tempfile::tempdir().unwrap();
        let chip = dir.path().join("hwmon0");
        std::fs::create_dir(&chip).unwrap();
        for (file, value) in [
            ("name", "board"),
            ("fan1_input", "1200"),
            ("in0_input", "-12000"),
            ("curr1_input", "2000"),
            ("power1_input", "65000000"),
            ("power1_average", "60000000"),
            ("energy1_input", "123000000"),
            ("fan2_input", "900"),
            ("fan2_fault", "1"),
            ("fan3_input", "NaN"),
        ] {
            std::fs::write(chip.join(file), value).unwrap();
        }
        let (s, c) = sensors_from(dir.path(), dir.path());
        assert_eq!(s.len(), 5);
        for (kind, value) in [
            (SensorKind::FanRpm, 1200.0),
            (SensorKind::VoltageVolts, -12.0),
            (SensorKind::CurrentAmps, 2.0),
            (SensorKind::PowerWatts, 65.0),
            (SensorKind::EnergyJoules, 123.0),
        ] {
            assert_eq!(s.iter().find(|s| s.kind == kind).unwrap().value, value);
        }
        assert_eq!(c.error_kind, Some(CapabilityErrorKind::InvalidData));
    }
}
