use std::{
    ffi::{OsStr, c_void},
    os::windows::ffi::OsStrExt,
    thread,
};

use anyhow::{Context, ensure};
use host_monitor::service::{ShutdownSignal, shutdown_channel};
use windows::{
    Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject},
    },
    core::PCWSTR,
};

pub(super) fn open(name: &str) -> anyhow::Result<ShutdownSignal> {
    let name = OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) }
        .context("failed to open the tray pairing cancellation event")?;
    let event_raw = event.0 as isize;
    let (controller, signal) = shutdown_channel();
    thread::Builder::new()
        .name("host-monitoring-pair-cancel".into())
        .spawn(move || {
            let event = windows::Win32::Foundation::HANDLE(event_raw as *mut c_void);
            let result = unsafe { WaitForSingleObject(event, u32::MAX) };
            let _ = unsafe { CloseHandle(event) };
            if result == WAIT_OBJECT_0 {
                controller.request_shutdown();
            }
        })
        .context("failed to start the tray cancellation waiter")?;
    ensure!(!signal.is_requested(), "tray pairing was already cancelled");
    Ok(signal)
}
