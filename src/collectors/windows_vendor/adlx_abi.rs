//! ADLX 2.0 C vtable prefixes. Non-read operations are opaque, uncallable slots.
use std::ffi::{c_char, c_void};
pub type Raw = *mut Object;
#[repr(C)]
pub struct Object {
    pub vtable: *const c_void,
}
pub type Status = i32;
pub type Release = unsafe extern "system" fn(Raw) -> i32;
pub type Query = unsafe extern "system" fn(Raw, *const u16, *mut Raw) -> Status;
pub type GetObject = unsafe extern "system" fn(Raw, *mut Raw) -> Status;
pub type GetMetric = unsafe extern "system" fn(Raw, Raw, *mut Raw) -> Status;
pub type GetString = unsafe extern "system" fn(Raw, *mut *const c_char) -> Status;
pub type GetInt = unsafe extern "system" fn(Raw, *mut i32) -> Status;
pub type GetDouble = unsafe extern "system" fn(Raw, *mut f64) -> Status;
pub type GetBool = unsafe extern "system" fn(Raw, *mut u8) -> Status;
pub type Init = unsafe extern "C" fn(u64, *mut Raw) -> Status;
pub type Terminate = unsafe extern "C" fn() -> Status;
#[repr(C)]
pub struct Base {
    pub acquire: usize,
    pub release: Release,
    pub query: Query,
}
#[repr(C)]
pub struct System {
    pub hybrid: usize,
    pub get_gpus: GetObject,
    pub query: Query,
    pub unused: [usize; 6],
    pub get_performance: GetObject,
}
#[repr(C)]
pub struct GpuList {
    pub base: Base,
    pub size: unsafe extern "system" fn(Raw) -> u32,
    pub empty: usize,
    pub begin: unsafe extern "system" fn(Raw) -> u32,
    pub end: usize,
    pub unused: [usize; 4],
    pub at_gpu: unsafe extern "system" fn(Raw, u32, *mut Raw) -> Status,
}
#[repr(C)]
pub struct Gpu {
    pub base: Base,
    pub vendor: usize,
    pub asic: usize,
    pub kind: usize,
    pub external: usize,
    pub name: GetString,
    pub driver_path: usize,
    pub pnp: GetString,
    pub desktops: usize,
    pub total_vram: unsafe extern "system" fn(Raw, *mut u32) -> Status,
    pub unused: [usize; 6],
    pub unique_id: GetInt,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Luid {
    pub low: u32,
    pub high: i32,
}
#[repr(C)]
pub struct Gpu2 {
    pub gpu: Gpu,
    // Includes power-control entries, intentionally represented only as opaque storage.
    pub unused: [usize; 15],
    pub luid: unsafe extern "system" fn(Raw, *mut Luid) -> Status,
}
#[repr(C)]
pub struct Performance {
    pub base: Base,
    pub unused: [usize; 15],
    pub current_gpu_metrics: GetMetric,
    pub unused2: [usize; 2],
    pub supported_gpu_metrics: GetMetric,
}
#[repr(C)]
pub struct Support {
    pub base: Base,
    pub supported: [GetBool; 10],
}
#[repr(C)]
pub struct Metrics {
    pub base: Base,
    pub timestamp: usize,
    pub usage: GetDouble,
    pub clock: GetInt,
    pub memory_clock: GetInt,
    pub temperature: GetDouble,
    pub hotspot: GetDouble,
    pub power: GetDouble,
    pub board_power: GetDouble,
    pub fan: GetInt,
    pub vram: GetInt,
    pub voltage: GetInt,
}
