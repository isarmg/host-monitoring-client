//! Only driver-installed System32 DLLs may supply vendor telemetry.
use super::Failure;
use crate::model::CapabilityErrorKind;
use std::ffi::CStr;

pub(super) struct Library {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HMODULE,
}
impl Library {
    pub fn open(name: &str) -> Result<Self, Failure> {
        #[cfg(windows)]
        {
            use windows::{
                Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
                core::PCWSTR,
            };
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: static allowlisted basename, terminated UTF-16, restricted DLL search.
            let handle = unsafe {
                LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            }
            .map_err(|error| {
                Failure::new(
                    match error.code().0 as u32 & 0xffff {
                        5 => CapabilityErrorKind::PermissionDenied,
                        126 | 1157 => CapabilityErrorKind::DriverMissing,
                        193 => CapabilityErrorKind::Unsupported,
                        _ => CapabilityErrorKind::Transient,
                    },
                    format!("cannot load system driver {name}: {error}"),
                )
            })?;
            Ok(Self { handle })
        }
        #[cfg(not(windows))]
        {
            Err(Failure::new(
                CapabilityErrorKind::Unsupported,
                format!("{name} requires Windows x64"),
            ))
        }
    }
    /// Caller supplies the exact ABI of the named, allowlisted SDK export.
    pub unsafe fn symbol<T: Copy>(&self, name: &'static CStr) -> Result<T, Failure> {
        #[cfg(windows)]
        {
            use windows::{Win32::System::LibraryLoader::GetProcAddress, core::PCSTR};
            // SAFETY: DLL remains loaded for the lifetime of all resolved function pointers.
            let raw = unsafe { GetProcAddress(self.handle, PCSTR(name.as_ptr().cast())) }
                .ok_or_else(|| {
                    Failure::new(
                        CapabilityErrorKind::Unsupported,
                        format!("driver lacks {}", name.to_string_lossy()),
                    )
                })?;
            assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&raw));
            // SAFETY: caller guarantees T is the SDK export's function-pointer type.
            Ok(unsafe { std::mem::transmute_copy(&raw) })
        }
        #[cfg(not(windows))]
        {
            Err(Failure::new(
                CapabilityErrorKind::Unsupported,
                name.to_string_lossy().into_owned(),
            ))
        }
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: sessions release every SDK object before the Library field is dropped.
        unsafe {
            let _ = windows::Win32::Foundation::FreeLibrary(self.handle);
        }
    }
}
