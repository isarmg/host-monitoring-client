//! Read-only smartctl JSON adapter. Runs off the sampler thread with bounded time/output.
use crate::model::*;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmartConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    /// Absolute path; omitted means known system installation locations only.
    pub executable: Option<PathBuf>,
}
impl Default for SmartConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 300,
            executable: None,
        }
    }
}

type ResultSet = (Vec<DiskHealth>, Capability);
pub(super) struct SmartCollector {
    config: SmartConfig,
    last_started: Option<Instant>,
    pending: Option<mpsc::Receiver<ResultSet>>,
    cached: ResultSet,
}
impl SmartCollector {
    pub fn new(config: SmartConfig) -> Self {
        let message = if config.enabled {
            "SMART scan has not completed yet"
        } else {
            "SMART collection disabled in configuration"
        };
        let mut collector = Self {
            config,
            last_started: None,
            pending: None,
            cached: (
                vec![],
                unavailable(CapabilityErrorKind::NotPresent, message),
            ),
        };
        collector.poll();
        collector
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn poll(&mut self) -> ResultSet {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.cached = result;
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cached = (
                        vec![],
                        unavailable(CapabilityErrorKind::Transient, "SMART worker stopped"),
                    );
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.config.enabled
            && self.pending.is_none()
            && self
                .last_started
                .is_none_or(|t| t.elapsed().as_secs() >= self.config.interval_seconds.max(60))
        {
            let (tx, rx) = mpsc::channel();
            let config = self.config.clone();
            match std::thread::Builder::new()
                .name("hardware-smart".into())
                .spawn(move || {
                    let _ = tx.send(collect(&config));
                }) {
                Ok(_) => {
                    self.pending = Some(rx);
                }
                Err(_) => {
                    self.cached = (
                        vec![],
                        unavailable(
                            CapabilityErrorKind::Transient,
                            "could not start SMART worker",
                        ),
                    )
                }
            }
            self.last_started = Some(Instant::now());
        }
        self.cached.clone()
    }
}
fn unavailable(kind: CapabilityErrorKind, message: &str) -> Capability {
    Capability::unavailable("hardware.disk_health", "smartctl-json", kind, message)
}
fn executable(config: &SmartConfig) -> Option<PathBuf> {
    if let Some(p) = &config.executable {
        return p.is_absolute().then(|| p.clone());
    }
    #[cfg(windows)]
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(bundled_executable)
    {
        return Some(bundled);
    }
    #[cfg(windows)]
    let candidates = [
        r"C:\Program Files\smartmontools\bin\smartctl.exe",
        r"C:\Program Files\smartmontools\smartctl.exe",
    ];
    #[cfg(not(windows))]
    let candidates = [
        "/usr/sbin/smartctl",
        "/usr/bin/smartctl",
        "/usr/local/sbin/smartctl",
        "/opt/homebrew/sbin/smartctl",
    ];
    candidates
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

#[cfg(windows)]
fn bundled_executable(current_exe: &Path) -> Option<PathBuf> {
    let candidate = current_exe
        .parent()?
        .join("smartmontools")
        .join("bin")
        .join("smartctl.exe");
    candidate.is_file().then_some(candidate)
}

fn collect(config: &SmartConfig) -> ResultSet {
    let Some(exe) = executable(config) else {
        return (
            vec![],
            unavailable(
                CapabilityErrorKind::DriverMissing,
                "smartctl not found; install smartmontools or configure smart.executable",
            ),
        );
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let scan = match run_json(&exe, &["--scan", "--json"], deadline) {
        Ok((v, code)) if code & 7 == 0 => v,
        Ok((v, _)) => return (vec![], smart_error(&v)),
        Err(kind) => {
            return (
                vec![],
                unavailable(kind, "SMART device scan failed or timed out"),
            );
        }
    };
    let Some(devices) = scan["devices"].as_array() else {
        return if scan["devices"].is_null() {
            (
                vec![],
                unavailable(CapabilityErrorKind::NotPresent, "no SMART devices found"),
            )
        } else {
            (
                vec![],
                unavailable(
                    CapabilityErrorKind::InvalidData,
                    "invalid SMART device list",
                ),
            )
        };
    };
    let mut disks = Vec::new();
    let mut failure = None;
    let mut seen = std::collections::HashSet::new();
    for device in devices.iter().take(MAX_HARDWARE_DISKS) {
        let Some(name) = device["name"].as_str().filter(|s| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.len() <= MAX_HARDWARE_TEXT
                && !s.chars().any(char::is_control)
        }) else {
            continue;
        };
        if !seen.insert(name) {
            continue;
        }
        let mut args = vec![
            "--json",
            "--info",
            "--health",
            "--attributes",
            "--nocheck=standby,0",
        ];
        if let Some(kind) = device["type"].as_str() {
            args.extend(["--device", kind]);
        }
        args.push(name);
        match run_json(&exe, &args, deadline) {
            Ok((json, code)) => {
                if code & 7 != 0 {
                    failure.get_or_insert_with(|| smart_error(&json));
                }
                if let Some(disk) = parse_disk(name, &json) {
                    disks.push(disk);
                }
            }
            Err(kind) => {
                failure.get_or_insert_with(|| unavailable(kind,"one or more SMART devices could not be read within the collection deadline"));
            }
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    if devices.len() > MAX_HARDWARE_DISKS {
        failure.get_or_insert_with(|| {
            unavailable(
                CapabilityErrorKind::InvalidData,
                "SMART device count exceeds report limit",
            )
        });
    }
    let capability = failure.unwrap_or_else(|| {
        if disks.is_empty() {
            unavailable(
                CapabilityErrorKind::NotPresent,
                "no readable SMART health data; devices may be asleep or inaccessible",
            )
        } else {
            Capability::available("hardware.disk_health", "smartctl-json")
        }
    });
    (disks, capability)
}

fn smart_error(v: &Value) -> Capability {
    let denied = v["smartctl"]["messages"].as_array().is_some_and(|a| {
        a.iter().any(|m| {
            let s = m["string"].as_str().unwrap_or("").to_ascii_lowercase();
            s.contains("permission denied")
                || s.contains("access is denied")
                || s.contains("access denied")
        })
    });
    unavailable(
        if denied {
            CapabilityErrorKind::PermissionDenied
        } else {
            CapabilityErrorKind::Transient
        },
        "smartctl could not read all requested data; check device access and driver support",
    )
}

fn run_json(
    exe: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<(Value, i32), CapabilityErrorKind> {
    const LIMIT: u64 = 1024 * 1024;
    let deadline = deadline.min(Instant::now() + Duration::from_secs(5));
    if Instant::now() >= deadline {
        return Err(CapabilityErrorKind::Transient);
    }
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CapabilityErrorKind::DriverMissing,
        std::io::ErrorKind::PermissionDenied => CapabilityErrorKind::PermissionDenied,
        _ => CapabilityErrorKind::Transient,
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    if std::thread::Builder::new()
        .name("smartctl-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = tx.send(result);
        })
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(CapabilityErrorKind::Transient);
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CapabilityErrorKind::Transient);
            }
        }
    };
    let bytes = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| CapabilityErrorKind::Transient)?
        .map_err(|_| CapabilityErrorKind::Transient)?;
    if bytes.len() as u64 > LIMIT {
        return Err(CapabilityErrorKind::InvalidData);
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| CapabilityErrorKind::InvalidData)?;
    Ok((value, status.code().ok_or(CapabilityErrorKind::Transient)?))
}

fn counter(v: &Value, key: &str) -> Option<u64> {
    // smartctl -j emits exact decimal _s companions for counters beyond JSON's safe range.
    if let Some(s) = v.get(format!("{key}_s")).and_then(Value::as_str) {
        return s.parse().ok();
    }
    v[key].as_u64()
}
fn nvme_bytes(v: &Value, key: &str) -> Option<u64> {
    counter(v, key)?.checked_mul(512_000)
}
fn parse_disk(device: &str, v: &Value) -> Option<DiskHealth> {
    let nvme = &v["nvme_smart_health_information_log"];
    if v["smart_status"]["passed"].is_null() && !nvme.is_object() && !v["temperature"].is_object() {
        return None;
    }
    let string = |key: &str| v[key].as_str().and_then(super::hardware::text);
    Some(DiskHealth {
        device: device.into(),
        model: string("model_name").or_else(|| string("product")),
        serial_number: string("serial_number"),
        protocol: v["device"]["protocol"]
            .as_str()
            .and_then(super::hardware::text),
        collected_at: Utc::now(),
        healthy: v["smart_status"]["passed"].as_bool(),
        temperature_celsius: v["temperature"]["current"]
            .as_f64()
            .or_else(|| nvme["temperature"].as_f64()),
        percentage_used: nvme["percentage_used"].as_f64(),
        available_spare_percent: nvme["available_spare"].as_f64(),
        critical_warning: counter(nvme, "critical_warning").and_then(|v| v.try_into().ok()),
        power_on_hours: counter(&v["power_on_time"], "hours")
            .or_else(|| counter(nvme, "power_on_hours")),
        power_cycles: counter(v, "power_cycle_count").or_else(|| counter(nvme, "power_cycles")),
        unsafe_shutdowns: counter(nvme, "unsafe_shutdowns"),
        media_errors: counter(nvme, "media_errors"),
        bytes_read: nvme_bytes(nvme, "data_units_read"),
        bytes_written: nvme_bytes(nvme, "data_units_written"),
        source: "smartctl-json".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nvme_endurance_and_exact_counters_do_not_lose_health_failures() {
        let v = serde_json::json!({"model_name":"NVMe SSD", "smart_status":{"passed":false},
            "nvme_smart_health_information_log":{"percentage_used":105,"available_spare":8,"critical_warning":4,
            "data_units_read":2,"data_units_written":3,"media_errors_s":"9007199254740993"}});
        let d = parse_disk("/dev/nvme0", &v).unwrap();
        assert_eq!(d.healthy, Some(false));
        assert_eq!(d.percentage_used, Some(105.0));
        assert_eq!(d.bytes_read, Some(1_024_000));
        assert_eq!(d.bytes_written, Some(1_536_000));
        assert_eq!(d.media_errors, Some(9_007_199_254_740_993));
        assert!(
            parse_disk(
                "/dev/sda",
                &serde_json::json!({"smartctl":{"exit_status":0}})
            )
            .is_none()
        );
        assert_eq!(
            nvme_bytes(&serde_json::json!({"n_s":"18446744073709551615"}), "n"),
            None
        );
    }
    #[test]
    fn ata_temperature_and_permission_diagnostics() {
        let v = serde_json::json!({"smart_status":{"passed":true},"temperature":{"current":42},"power_on_time":{"hours":321}});
        let d = parse_disk("/dev/sda", &v).unwrap();
        assert_eq!(d.temperature_celsius, Some(42.0));
        assert_eq!(d.power_on_hours, Some(321));
        assert_eq!(d.percentage_used, None);
        assert_eq!(
            smart_error(
                &serde_json::json!({"smartctl":{"messages":[{"string":"Permission denied"}]}})
            )
            .error_kind,
            Some(CapabilityErrorKind::PermissionDenied)
        );
    }
    #[cfg(windows)]
    #[test]
    fn bundled_smartctl_is_resolved_relative_to_the_client() {
        let root = tempfile::tempdir().unwrap();
        let smartctl = root.path().join("smartmontools/bin/smartctl.exe");
        std::fs::create_dir_all(smartctl.parent().unwrap()).unwrap();
        std::fs::write(&smartctl, []).unwrap();

        assert_eq!(
            bundled_executable(&root.path().join("host-monitor.exe")),
            Some(smartctl)
        );
    }
    #[cfg(unix)]
    #[test]
    fn subprocess_timeout_is_bounded() {
        let now = Instant::now();
        assert!(
            run_json(
                Path::new("/bin/sleep"),
                &["1"],
                now + Duration::from_millis(80)
            )
            .is_err()
        );
        assert!(now.elapsed() < Duration::from_secs(1));
    }
}
