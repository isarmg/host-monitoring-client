use std::path::Path;

use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

/// Windows-only atomic replacement with write-through semantics. Unix state
/// and configuration publication are owned by Client Foundation.
pub(crate) fn replace(temporary: &Path, target: &Path) -> std::io::Result<()> {
    {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let temporary = wide(temporary.as_os_str());
        let target = wide(target.as_os_str());
        let result = unsafe {
            MoveFileExW(
                temporary.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
