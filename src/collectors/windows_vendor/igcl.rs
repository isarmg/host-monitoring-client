use super::{Failure, Reading, VendorResult, igcl_abi::*, library::Library};
use crate::model::CapabilityErrorKind;
use std::{collections::HashMap, mem::size_of, ptr};

#[derive(Clone, Copy, Default)]
struct Counters {
    timestamp: Option<f64>,
    gpu_energy: Option<f64>,
    board_energy: Option<f64>,
    activity: Option<f64>,
}
fn rate(current: Option<f64>, previous: Option<f64>, elapsed: Option<f64>) -> Option<f64> {
    let (current, previous, elapsed) = (current?, previous?, elapsed?);
    if current < previous || !(0.05..=120.0).contains(&elapsed) {
        return None;
    }
    super::nonnegative((current - previous) / elapsed)
}
fn error(code: u32) -> Failure {
    Failure::new(
        match code {
            0x40000006 => CapabilityErrorKind::PermissionDenied,
            0x40000009 | 0x4000000a | 0x40000010 => CapabilityErrorKind::Unsupported,
            0x40000026 => CapabilityErrorKind::DriverMissing,
            _ => CapabilityErrorKind::Transient,
        },
        format!("IGCL status 0x{code:08x}"),
    )
}

pub(super) struct Igcl {
    handle: Handle,
    close: Close,
    enumerate: Enumerate,
    properties: Properties,
    telemetry: GetTelemetry,
    previous: HashMap<String, Counters>,
    _library: Library,
}
impl Igcl {
    pub fn new() -> Result<Self, Failure> {
        let library = Library::open("ControlLib.dll")?;
        // SAFETY: exact pinned IGCL 1.1 C ABIs; no control/set functions are resolved.
        let (init, close, enumerate, properties, telemetry) = unsafe {
            (
                library.symbol::<Init>(c"ctlInit")?,
                library.symbol::<Close>(c"ctlClose")?,
                library.symbol::<Enumerate>(c"ctlEnumerateDevices")?,
                library.symbol::<Properties>(c"ctlGetDeviceProperties")?,
                library.symbol::<GetTelemetry>(c"ctlPowerTelemetryGet")?,
            )
        };
        let mut args = InitArgs {
            size: size_of::<InitArgs>() as u32,
            app_version: 0x10001,
            flags: 1,
            ..Default::default()
        };
        // Only USE_LEVEL_ZERO, never the IGSC firmware functionality flag.
        let mut handle = ptr::null_mut();
        let status = unsafe { init(&mut args, &mut handle) };
        if status != 0 {
            return Err(error(status));
        }
        if handle.is_null() {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "IGCL initialized with a null handle",
            ));
        }
        Ok(Self {
            handle,
            close,
            enumerate,
            properties,
            telemetry,
            previous: HashMap::new(),
            _library: library,
        })
    }
    pub fn collect(&mut self) -> Result<VendorResult, Failure> {
        let mut count = 0;
        // SAFETY: two-phase enumeration, capacity strictly bounded before allocating.
        let status = unsafe { (self.enumerate)(self.handle, &mut count, ptr::null_mut()) };
        if status != 0 {
            return Err(error(status));
        }
        if count as usize > crate::model::CLIENT_REPORT_MAX_GPUS {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "IGCL device count exceeds report limit",
            ));
        }
        if count == 0 {
            self.previous.clear();
            return Ok(VendorResult::default());
        }
        let mut handles = vec![ptr::null_mut(); count as usize];
        let capacity = count;
        let status = unsafe { (self.enumerate)(self.handle, &mut count, handles.as_mut_ptr()) };
        if status != 0 {
            return Err(error(status));
        }
        if count > capacity {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "IGCL device count changed during enumeration",
            ));
        }
        let mut result = VendorResult::default();
        let mut seen = std::collections::HashSet::new();
        for handle in handles.into_iter().take(count as usize) {
            if handle.is_null() {
                continue;
            }
            match self.read_gpu(handle) {
                Ok(Some(reading)) => {
                    seen.insert(reading.gpu.id.clone());
                    result.readings.push(reading);
                }
                Ok(None) => {}
                Err(error) => {
                    result.failure.get_or_insert(error);
                }
            }
        }
        self.previous.retain(|id, _| seen.contains(id));
        Ok(result)
    }
    fn read_gpu(&mut self, handle: Handle) -> Result<Option<Reading>, Failure> {
        let mut luid = super::adlx_abi::Luid::default();
        // SAFETY: ABI struct contains integers, arrays and pointers; zero is valid for each field.
        let mut props: DeviceProperties = unsafe { std::mem::zeroed() };
        props.size = size_of::<DeviceProperties>() as u32;
        props.device_id = (&mut luid as *mut super::adlx_abi::Luid).cast();
        props.device_id_size = 8;
        let status = unsafe { (self.properties)(handle, &mut props) };
        if status != 0 {
            return Err(error(status));
        }
        if props.device_type != 1 || props.pci_vendor_id != 0x8086 {
            return Ok(None);
        }
        if props.device_id_size != 8 || (luid.low == 0 && luid.high == 0) {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "IGCL device has no usable Windows LUID",
            ));
        }
        let name = String::from_utf8_lossy(
            &props.name[..props
                .name
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(props.name.len())],
        );
        let mut reading = Reading::new(
            super::luid_id(luid.low, luid.high),
            "intel",
            super::super::hardware::text(&name).unwrap_or_else(|| "Intel GPU".into()),
            "intel-igcl",
        );
        let mut telemetry = PowerTelemetry {
            size: size_of::<PowerTelemetry>() as u32,
            version: 1,
            ..Default::default()
        };
        let status = unsafe { (self.telemetry)(handle, &mut telemetry) };
        if status != 0 {
            self.previous.remove(&reading.gpu.id);
            return Err(error(status));
        }
        let previous = self.previous.get(&reading.gpu.id).copied();
        let counters = append_telemetry(&mut reading, &telemetry, previous);
        self.previous.insert(reading.gpu.id.clone(), counters);
        if !reading.has_telemetry()
            && !(counters.timestamp.is_some()
                && (counters.board_energy.is_some() || counters.gpu_energy.is_some()))
        {
            return Err(Failure::new(
                CapabilityErrorKind::Unsupported,
                "IGCL exposes no supported GPU telemetry",
            ));
        }
        Ok(Some(reading))
    }
}
impl Drop for Igcl {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
        }
    }
}
fn append_telemetry(
    reading: &mut Reading,
    t: &PowerTelemetry,
    previous: Option<Counters>,
) -> Counters {
    let positive = |item: Item, unit| item.number(unit).and_then(super::nonnegative);
    let current = Counters {
        timestamp: positive(t.timestamp, 7),
        gpu_energy: positive(t.gpu_energy, 6),
        board_energy: positive(t.board_energy, 6),
        activity: positive(t.global_activity, 7),
    };
    if let Some(previous) = previous {
        let elapsed = current
            .timestamp
            .zip(previous.timestamp)
            .map(|(a, b)| a - b);
        reading.gpu.power_watts = rate(current.board_energy, previous.board_energy, elapsed)
            .or_else(|| rate(current.gpu_energy, previous.gpu_energy, elapsed));
        reading.gpu.utilization_percent = rate(current.activity, previous.activity, elapsed)
            .map(|v| v * 100.0)
            .filter(|v| *v <= 100.0);
    }
    reading.gpu.temperature_celsius = t
        .gpu_temperature
        .number(5)
        .filter(|v| (-273.15..=1000.0).contains(v));
    reading.gpu.core_clock_mhz = positive(t.gpu_clock, 0);
    reading.gpu.memory_clock_mhz = positive(t.vram_clock, 0);
    reading.voltage("core", positive(t.gpu_voltage, 3));
    reading.voltage("vram", positive(t.vram_voltage, 3));
    for (index, fan) in t.fans.iter().enumerate() {
        reading.fan(&format!("fan{index}"), positive(*fan, 9));
    }
    reading.temperature("vram", t.vram_temperature.number(5));
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(windows))]
    #[test]
    fn native_getters_map_device_identity_and_clear_baseline_after_driver_error() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        static FAIL: AtomicBool = AtomicBool::new(false);
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn close(_: Handle) -> u32 {
            CLOSED.fetch_add(1, Ordering::SeqCst);
            0
        }
        unsafe extern "C" fn enumerate(_: Handle, count: *mut u32, out: *mut Handle) -> u32 {
            unsafe {
                *count = 1;
                if !out.is_null() {
                    *out = ptr::dangling_mut::<u8>().cast();
                }
            }
            0
        }
        unsafe extern "C" fn properties(_: Handle, out: *mut DeviceProperties) -> u32 {
            let out = unsafe { &mut *out };
            assert_eq!(out.size as usize, size_of::<DeviceProperties>());
            assert_eq!(out.device_id_size, 8);
            out.device_type = 1;
            out.pci_vendor_id = 0x8086;
            unsafe {
                *out.device_id.cast::<super::super::adlx_abi::Luid>() =
                    super::super::adlx_abi::Luid { low: 123, high: 1 };
            }
            0
        }
        unsafe extern "C" fn telemetry(_: Handle, out: *mut PowerTelemetry) -> u32 {
            let out = unsafe { &mut *out };
            assert_eq!(out.size as usize, size_of::<PowerTelemetry>());
            assert_eq!(out.version, 1);
            if FAIL.load(Ordering::SeqCst) {
                return 0x40000009;
            }
            out.timestamp = number(10.0, 7);
            out.board_energy = number(100.0, 6);
            out.gpu_temperature = number(55.0, 5);
            out.gpu_voltage = number(1.05, 3);
            out.fans[0] = number(0.0, 9);
            0
        }
        let mut sdk = Igcl {
            handle: ptr::dangling_mut::<u8>().cast(),
            close,
            enumerate,
            properties,
            telemetry,
            previous: HashMap::new(),
            _library: Library {},
        };
        let result = sdk.collect().unwrap();
        assert!(result.failure.is_none());
        assert_eq!(result.readings[0].gpu.id, "luid_00000001_0000007b");
        assert_eq!(result.readings[0].gpu.temperature_celsius, Some(55.0));
        assert_eq!(result.readings[0].gpu.power_watts, None);
        assert_eq!(result.readings[0].sensors.len(), 2);
        assert_eq!(sdk.previous.len(), 1);
        FAIL.store(true, Ordering::SeqCst);
        let result = sdk.collect().unwrap();
        assert!(result.readings.is_empty());
        assert_eq!(
            result.failure.unwrap().kind,
            CapabilityErrorKind::Unsupported
        );
        assert!(sdk.previous.is_empty());
        drop(sdk);
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }
    fn number(value: f64, units: u32) -> Item {
        Item {
            supported: 1,
            units,
            data_type: 9,
            value: value.to_bits(),
        }
    }
    #[test]
    fn power_uses_energy_delta_and_sdk_time_not_absolute_energy() {
        let mut r = Reading::new("test".into(), "intel", "GPU".into(), "intel-igcl");
        let mut t = PowerTelemetry {
            timestamp: number(10.0, 7),
            board_energy: number(1000.0, 6),
            global_activity: number(1.0, 7),
            gpu_clock: number(2200.0, 0),
            ..Default::default()
        };
        let first = append_telemetry(&mut r, &t, None);
        assert_eq!(r.gpu.power_watts, None);
        t.timestamp = number(12.0, 7);
        t.board_energy = number(1300.0, 6);
        t.global_activity = number(2.0, 7);
        let second = append_telemetry(&mut r, &t, Some(first));
        assert_eq!(r.gpu.power_watts, Some(150.0));
        assert_eq!(r.gpu.utilization_percent, Some(50.0));
        append_telemetry(&mut r, &t, Some(second));
        assert_eq!(r.gpu.power_watts, None);
        t.timestamp = number(13.0, 7);
        t.board_energy = number(1.0, 6);
        append_telemetry(&mut r, &t, Some(second));
        assert_eq!(r.gpu.power_watts, None);
        assert_eq!(r.gpu.core_clock_mhz, Some(2200.0));
        assert_eq!(rate(Some(2000.0), Some(1000.0), Some(121.0)), None);
        assert_eq!(rate(Some(2000.0), None, Some(1.0)), None);
    }
    #[test]
    fn unit_type_support_and_invalid_numbers_are_checked() {
        assert_eq!(number(50.0, 5).number(0), None);
        assert_eq!(number(f64::NAN, 5).number(5), None);
        let mut item = number(0.0, 9);
        assert_eq!(item.number(9), Some(0.0));
        item.supported = 0;
        assert_eq!(item.number(9), None);
        item = Item {
            supported: 1,
            units: 0,
            data_type: 5,
            value: 2400,
        };
        assert_eq!(item.number(0), Some(2400.0));
    }
}
