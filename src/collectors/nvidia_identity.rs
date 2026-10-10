//! NVIDIA's driver UUID/LUID query bridges NVML UUIDs to Windows DXGI adapters.
//! No CUDA Toolkit, context, kernel, allocation, or device configuration is involved.
use std::collections::HashMap;
use uuid::Uuid;

pub(super) fn lookup<'a>(identities: &'a HashMap<Uuid, String>, nvml_id: &str) -> Option<&'a str> {
    let id = Uuid::parse_str(nvml_id.strip_prefix("GPU-")?).ok()?;
    identities.get(&id).map(String::as_str)
}

fn identities(readings: impl IntoIterator<Item = ([u8; 16], [u8; 8])>) -> HashMap<Uuid, String> {
    let readings: Vec<_> = readings
        .into_iter()
        .map(|(uuid, luid)| (Uuid::from_bytes(uuid), luid))
        .filter(|(uuid, luid)| !uuid.is_nil() && *luid != [0; 8])
        .collect();
    let mut uuid_counts = HashMap::new();
    let mut luid_counts = HashMap::new();
    for (uuid, luid) in &readings {
        *uuid_counts.entry(*uuid).or_insert(0) += 1;
        *luid_counts.entry(*luid).or_insert(0) += 1;
    }
    readings
        .into_iter()
        .filter(|(uuid, luid)| {
            uuid_counts.get(uuid) == Some(&1) && luid_counts.get(luid) == Some(&1)
        })
        .map(|(uuid, luid)| {
            let low = u32::from_le_bytes(luid[..4].try_into().expect("four bytes"));
            let high = u32::from_le_bytes(luid[4..].try_into().expect("four bytes"));
            (uuid, format!("luid_{high:08x}_{low:08x}"))
        })
        .collect()
}

#[cfg(windows)]
pub(super) fn windows_luids() -> HashMap<Uuid, String> {
    native::scan().map(identities).unwrap_or_default()
}

#[cfg(windows)]
mod native {
    use std::ffi::CStr;
    use windows::{
        Win32::{
            Foundation::{FreeLibrary, HMODULE},
            System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        },
        core::{PCSTR, w},
    };

    // CUDA Driver API: CUdevice/CUresult are 32-bit C int, CUuuid is char[16].
    // CUDAAPI uses __stdcall on Windows; extern "system" follows that ABI.
    // https://docs.nvidia.com/cuda/archive/12.8.1/cuda-driver-api/group__CUDA__DEVICE.html
    #[repr(C)]
    struct DeviceUuid {
        bytes: [u8; 16],
    }
    type Init = unsafe extern "system" fn(u32) -> i32;
    type DeviceCount = unsafe extern "system" fn(*mut i32) -> i32;
    type DeviceGet = unsafe extern "system" fn(*mut i32, i32) -> i32;
    type GetUuid = unsafe extern "system" fn(*mut DeviceUuid, i32) -> i32;
    type GetLuid = unsafe extern "system" fn(*mut u8, *mut u32, i32) -> i32;

    struct Library(HMODULE);
    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: all resolved function pointers are local to scan and no driver object escapes.
            unsafe {
                let _ = FreeLibrary(self.0);
            }
        }
    }
    impl Library {
        unsafe fn symbol<T: Copy>(&self, name: &'static CStr) -> Option<T> {
            // SAFETY: terminated static export name and a live library handle.
            let symbol = unsafe { GetProcAddress(self.0, PCSTR(name.as_ptr().cast())) }?;
            assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&symbol));
            // SAFETY: the caller supplies the official function signature above.
            Some(unsafe { std::mem::transmute_copy(&symbol) })
        }
    }

    pub(super) fn scan() -> Option<Vec<([u8; 16], [u8; 8])>> {
        // SAFETY: fixed NVIDIA driver basename, System32-only search. No working-directory DLLs.
        let library = Library(
            unsafe { LoadLibraryExW(w!("nvcuda.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?,
        );
        // SAFETY: signatures match the documented C driver API. Fixed-size initialized
        // output buffers remain live through each synchronous call. The library outlives
        // all function pointers. Initialization creates no CUDA context.
        unsafe {
            let init: Init = library.symbol(c"cuInit")?;
            let count: DeviceCount = library.symbol(c"cuDeviceGetCount")?;
            let get: DeviceGet = library.symbol(c"cuDeviceGet")?;
            let uuid: GetUuid = library.symbol(c"cuDeviceGetUuid")?;
            let luid: GetLuid = library.symbol(c"cuDeviceGetLuid")?;
            let mut length = 0;
            if init(0) != 0 || count(&mut length) != 0 || length < 0 {
                return None;
            }
            let mut values = Vec::new();
            for index in 0..length.min(crate::model::CLIENT_REPORT_MAX_GPUS as i32) {
                let mut device = 0;
                let mut id = DeviceUuid { bytes: [0; 16] };
                let mut address = [0; 8];
                let mut node_mask = 0;
                if get(&mut device, index) == 0
                    && uuid(&mut id, device) == 0
                    && luid(address.as_mut_ptr(), &mut node_mask, device) == 0
                    && node_mask != 0
                {
                    values.push((id.bytes, address));
                }
            }
            Some(values)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn joins_uuid_to_native_windows_luid_without_device_names_or_order() {
        let id = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let values = identities([(
            *id.as_bytes(),
            [0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x90],
        )]);
        assert_eq!(
            lookup(&values, "GPU-00112233-4455-6677-8899-AABBCCDDEEFF"),
            Some("luid_90abcdef_12345678")
        );
        assert_eq!(lookup(&values, "nvidia-0"), None);
    }
    #[test]
    fn absent_zero_and_ambiguous_native_identities_remain_unknown() {
        let first = *Uuid::from_u128(1).as_bytes();
        let second = *Uuid::from_u128(2).as_bytes();
        assert!(identities([(first, [1; 8]), (first, [2; 8])]).is_empty());
        assert!(identities([(first, [1; 8]), (second, [1; 8])]).is_empty());
        assert!(identities([(first, [1; 8]), (first, [2; 8]), (second, [1; 8])]).is_empty());
        assert!(identities([(first, [0; 8]), ([0; 16], [1; 8])]).is_empty());
    }
}
