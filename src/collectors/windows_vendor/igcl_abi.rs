//! Read-only IGCL 1.1 ABI subset; upstream revision and layout checks in docs/windows-gpu-vendors.md.
use std::ffi::c_void;
pub type Handle = *mut c_void;
pub type Init = unsafe extern "C" fn(*mut InitArgs, *mut Handle) -> u32;
pub type Close = unsafe extern "C" fn(Handle) -> u32;
pub type Enumerate = unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> u32;
pub type Properties = unsafe extern "C" fn(Handle, *mut DeviceProperties) -> u32;
pub type GetTelemetry = unsafe extern "C" fn(Handle, *mut PowerTelemetry) -> u32;

#[repr(C)]
#[derive(Default)]
pub struct InitArgs {
    pub size: u32,
    pub version: u8,
    pub app_version: u32,
    pub flags: u32,
    pub supported_version: u32,
    pub application_uid: [u32; 4],
}
#[repr(C)]
pub struct DeviceProperties {
    pub size: u32,
    pub version: u8,
    pub device_id: *mut c_void,
    pub device_id_size: u32,
    pub device_type: u32,
    pub supported_functions: u32,
    pub driver_version: u64,
    pub firmware_version: [u64; 3],
    pub pci_vendor_id: u32,
    pub pci_device_id: u32,
    pub revision: u32,
    pub eus_per_subslice: u32,
    pub subslices_per_slice: u32,
    pub slices: u32,
    pub name: [u8; 100],
    pub adapter_properties: u32,
    pub frequency: u32,
    pub pci_subsys_id: u16,
    pub pci_subsys_vendor_id: u16,
    pub adapter_bdf: [u8; 3],
    pub num_xe_cores: u32,
    pub reserved: [u8; 108],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Item {
    // u8 instead of Rust bool: every bit pattern written by a driver is representable.
    pub supported: u8,
    pub units: u32,
    pub data_type: u32,
    pub value: u64,
}
impl Item {
    pub fn number(self, unit: u32) -> Option<f64> {
        if self.supported != 1 || self.units != unit {
            return None;
        }
        let value = match self.data_type {
            0 => self.value as i8 as f64,
            1 => self.value as u8 as f64,
            2 => self.value as i16 as f64,
            3 => self.value as u16 as f64,
            4 => self.value as i32 as f64,
            5 => self.value as u32 as f64,
            6 => self.value as i64 as f64,
            7 => self.value as f64,
            8 => f32::from_bits(self.value as u32) as f64,
            9 => f64::from_bits(self.value),
            _ => return None,
        };
        value.is_finite().then_some(value)
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Psu {
    pub supported: u8,
    pub kind: u32,
    pub energy: Item,
    pub voltage: Item,
}
#[repr(C)]
#[derive(Default)]
pub struct PowerTelemetry {
    pub size: u32,
    pub version: u8,
    pub timestamp: Item,
    pub gpu_energy: Item,
    pub gpu_voltage: Item,
    pub gpu_clock: Item,
    pub gpu_temperature: Item,
    pub global_activity: Item,
    pub render_activity: Item,
    pub media_activity: Item,
    pub gpu_limited: [u8; 5],
    pub vram_energy: Item,
    pub vram_voltage: Item,
    pub vram_clock: Item,
    pub vram_effective_frequency: Item,
    pub vram_read_counter: Item,
    pub vram_write_counter: Item,
    pub vram_temperature: Item,
    pub vram_limited: [u8; 5],
    pub board_energy: Item,
    pub psu: [Psu; 5],
    pub fans: [Item; 5],
    pub gpu_vr_temperature: Item,
    pub vram_vr_temperature: Item,
    pub sa_vr_temperature: Item,
    pub effective_clock: Item,
    pub over_voltage_percent: Item,
    pub power_percent: Item,
    pub temperature_percent: Item,
    pub vram_read_bandwidth: Item,
    pub vram_write_bandwidth: Item,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};
    // Values independently verified by compiling the pinned Intel header. The script checks 60 locations.
    #[test]
    fn layouts_match_official_x64_header() {
        assert_eq!(size_of::<InitArgs>(), 36);
        assert_eq!(size_of::<DeviceProperties>(), 320);
        assert_eq!(size_of::<Item>(), 24);
        assert_eq!(size_of::<Psu>(), 56);
        assert_eq!(size_of::<PowerTelemetry>(), 1024);
        assert_eq!(offset_of!(DeviceProperties, name), 88);
        assert_eq!(offset_of!(DeviceProperties, reserved), 208);
        assert_eq!(offset_of!(Item, value), 16);
        assert_eq!(offset_of!(PowerTelemetry, vram_energy), 208);
        assert_eq!(offset_of!(PowerTelemetry, board_energy), 384);
        assert_eq!(offset_of!(PowerTelemetry, fans), 688);
        assert_eq!(offset_of!(PowerTelemetry, vram_write_bandwidth), 1000);
    }
}
