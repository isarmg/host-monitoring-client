use super::{Failure, Reading, VendorResult, adlx_abi::*, library::Library};
use crate::model::CapabilityErrorKind;
use std::{
    marker::PhantomData,
    ptr::NonNull,
    sync::{Mutex, MutexGuard},
};
static SESSION: Mutex<()> = Mutex::new(());
const VERSION: u64 = (2u64 << 48) | 125;

struct Interface<T> {
    ptr: NonNull<Object>,
    _kind: PhantomData<T>,
}
impl<T> Interface<T> {
    fn raw(&self) -> Raw {
        self.ptr.as_ptr()
    }
    fn table(&self) -> &T {
        // SAFETY: constructed only from a successful API returning the specified interface T.
        unsafe { &*self.ptr.as_ref().vtable.cast::<T>() }
    }
}
impl<T> Drop for Interface<T> {
    fn drop(&mut self) {
        // SAFETY: every reference-counted ADLX interface starts with Base; DLL is still loaded.
        unsafe {
            let base = &*self.ptr.as_ref().vtable.cast::<Base>();
            (base.release)(self.raw());
        }
    }
}
fn output<T>(call: impl FnOnce(*mut Raw) -> Status) -> Result<Interface<T>, Failure> {
    let mut raw = std::ptr::null_mut();
    let status = call(&mut raw);
    if status != 0 {
        return Err(error(status));
    }
    let ptr = NonNull::new(raw).ok_or_else(|| {
        Failure::new(
            CapabilityErrorKind::InvalidData,
            "ADLX returned a null interface",
        )
    })?;
    Ok(Interface {
        ptr,
        _kind: PhantomData,
    })
}
fn error(code: i32) -> Failure {
    Failure::new(
        match code {
            5 | 6 | 12 => CapabilityErrorKind::Unsupported,
            9 => CapabilityErrorKind::NotPresent,
            _ => CapabilityErrorKind::Transient,
        },
        format!("ADLX status {code}"),
    )
}

pub(super) struct Adlx {
    performance: Option<Interface<Performance>>,
    system: NonNull<Object>,
    terminate: Terminate,
    _library: Library,
    _session: MutexGuard<'static, ()>,
}
impl Adlx {
    pub fn new() -> Result<Self, Failure> {
        let session = SESSION.try_lock().map_err(|_| {
            Failure::new(
                CapabilityErrorKind::Transient,
                "another ADLX collector owns the process session",
            )
        })?;
        let library = Library::open("amdadlx64.dll")?;
        // SAFETY: symbols use the pinned ADLX 2.0 C initialization ABI.
        let (init, terminate) = unsafe {
            (
                library.symbol::<Init>(c"ADLXInitialize")?,
                library.symbol::<Terminate>(c"ADLXTerminate")?,
            )
        };
        let mut system = std::ptr::null_mut();
        // SAFETY: valid writable output and exact compile-time SDK version. No incompatible-driver initialization.
        let status = unsafe { init(VERSION, &mut system) };
        // An already-initialized singleton is not ours to terminate.
        if status != 0 {
            return Err(error(status));
        }
        let Some(system) = NonNull::new(system) else {
            unsafe {
                terminate();
            }
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "ADLX initialized without a system interface",
            ));
        };
        let mut result = Self {
            performance: None,
            system,
            terminate,
            _library: library,
            _session: session,
        };
        // SAFETY: IADLXSystem is a library-owned singleton, not a reference-counted Base.
        let table = unsafe { &*system.as_ref().vtable.cast::<System>() };
        result.performance = Some(output(|out| unsafe {
            (table.get_performance)(system.as_ptr(), out)
        })?);
        Ok(result)
    }
    pub fn collect(&mut self) -> Result<VendorResult, Failure> {
        // SAFETY: singleton lifetime is bounded by this initialized session.
        let table = unsafe { &*self.system.as_ref().vtable.cast::<System>() };
        let list: Interface<GpuList> =
            output(|out| unsafe { (table.get_gpus)(self.system.as_ptr(), out) })?;
        let begin = unsafe { (list.table().begin)(list.raw()) };
        let count = unsafe { (list.table().size)(list.raw()) };
        if count as usize > crate::model::CLIENT_REPORT_MAX_GPUS {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "ADLX GPU count exceeds report limit",
            ));
        }
        let mut result = VendorResult::default();
        for index in 0..count {
            let Some(index) = begin.checked_add(index) else {
                return Err(Failure::new(
                    CapabilityErrorKind::InvalidData,
                    "ADLX GPU list index overflow",
                ));
            };
            let reading = (|| {
                let gpu: Interface<Gpu> =
                    output(|out| unsafe { (list.table().at_gpu)(list.raw(), index, out) })?;
                self.read_gpu(&gpu)
            })();
            match reading {
                Ok(reading) => result.readings.push(reading),
                Err(error) => {
                    result.failure.get_or_insert(error);
                }
            }
        }
        Ok(result)
    }
    fn read_gpu(&self, gpu: &Interface<Gpu>) -> Result<Reading, Failure> {
        let id: Vec<u16> = "IADLXGPU2".encode_utf16().chain(Some(0)).collect();
        let gpu2: Interface<Gpu2> =
            output(|out| unsafe { (gpu.table().base.query)(gpu.raw(), id.as_ptr(), out) })?;
        let mut luid = Luid::default();
        let status = unsafe { (gpu2.table().luid)(gpu2.raw(), &mut luid) };
        if status != 0 {
            return Err(error(status));
        }
        if luid.low == 0 && luid.high == 0 {
            return Err(Failure::new(
                CapabilityErrorKind::InvalidData,
                "ADLX device has no usable Windows LUID",
            ));
        }
        let mut name_ptr = std::ptr::null();
        let mut name = "AMD GPU".to_string();
        if unsafe { (gpu.table().name)(gpu.raw(), &mut name_ptr) } == 0 && !name_ptr.is_null() {
            // SAFETY: SDK owns a terminated string until gpu.Release; cap copying to report text size.
            let mut bytes = Vec::new();
            for i in 0..crate::model::MAX_HARDWARE_TEXT {
                let byte = unsafe { *name_ptr.add(i) as u8 };
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
            }
            if let Some(value) = super::super::hardware::text(&String::from_utf8_lossy(&bytes)) {
                name = value;
            }
        }
        let mut reading =
            Reading::new(super::luid_id(luid.low, luid.high), "amd", name, "amd-adlx");
        let mut total_mb = 0;
        if unsafe { (gpu.table().total_vram)(gpu.raw(), &mut total_mb) } == 0 && total_mb > 0 {
            reading.gpu.memory_total_bytes = Some(u64::from(total_mb) * 1024 * 1024);
        }
        let perf = self
            .performance
            .as_ref()
            .expect("initialized performance service");
        let support: Interface<Support> = output(|out| unsafe {
            (perf.table().supported_gpu_metrics)(perf.raw(), gpu.raw(), out)
        })?;
        let metrics: Interface<Metrics> = output(|out| unsafe {
            (perf.table().current_gpu_metrics)(perf.raw(), gpu.raw(), out)
        })?;
        let supported = |index: usize| {
            let mut supported = 0u8;
            unsafe {
                (support.table().supported[index])(support.raw(), &mut supported) == 0
                    && supported == 1
            }
        };
        let double = |index, getter: GetDouble| {
            let mut value = f64::NAN;
            if supported(index) && unsafe { getter(metrics.raw(), &mut value) } == 0 {
                value.is_finite().then_some(value)
            } else {
                None
            }
        };
        let integer = |index, getter: GetInt| {
            let mut value = -1;
            if supported(index) && unsafe { getter(metrics.raw(), &mut value) } == 0 {
                super::nonnegative(f64::from(value))
            } else {
                None
            }
        };
        let m = metrics.table();
        reading.gpu.utilization_percent = double(0, m.usage).filter(|v| (0.0..=100.0).contains(v));
        reading.gpu.core_clock_mhz = integer(1, m.clock);
        reading.gpu.memory_clock_mhz = integer(2, m.memory_clock);
        reading.gpu.temperature_celsius =
            double(3, m.temperature).filter(|v| (-273.15..=1000.0).contains(v));
        reading.gpu.power_watts = double(6, m.board_power)
            .and_then(super::nonnegative)
            .or_else(|| double(5, m.power).and_then(super::nonnegative));
        reading.gpu.memory_used_bytes = integer(8, m.vram).map(|mb| (mb as u64) * 1024 * 1024);
        reading.fan("fan0", integer(7, m.fan));
        reading.voltage("core", integer(9, m.voltage).map(|mv| mv / 1000.0));
        reading.temperature("hotspot", double(4, m.hotspot));
        if !reading.has_telemetry() {
            return Err(Failure::new(
                CapabilityErrorKind::Unsupported,
                "ADLX exposes no supported GPU telemetry",
            ));
        }
        Ok(reading)
    }
}
impl Drop for Adlx {
    fn drop(&mut self) {
        self.performance.take();
        // SAFETY: all acquired interfaces have been released, and this session owns initialization.
        unsafe {
            (self.terminate)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn acquired_interface_is_released_once_and_null_output_is_rejected() {
        static RELEASES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn release(_: Raw) -> i32 {
            RELEASES.fetch_add(1, Ordering::SeqCst);
            0
        }
        unsafe extern "system" fn query(_: Raw, _: *const u16, _: *mut Raw) -> Status {
            6
        }
        let table = Base {
            acquire: 0,
            release,
            query,
        };
        let mut object = Object {
            vtable: (&table as *const Base).cast(),
        };
        let interface: Interface<Base> = output(|out| {
            unsafe {
                *out = &mut object;
            }
            0
        })
        .unwrap();
        drop(interface);
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
        assert!(output::<Base>(|_| 0).is_err());
        assert_eq!(
            output::<Base>(|_| 5).err().unwrap().kind,
            CapabilityErrorKind::Unsupported
        );
        assert_eq!(RELEASES.load(Ordering::SeqCst), 1);
    }
}
