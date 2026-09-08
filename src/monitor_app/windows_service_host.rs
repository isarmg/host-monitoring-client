use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicPtr, AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use host_monitor::service::{
    ShutdownController, ShutdownSignal, WINDOWS_SERVICE_NAME, shutdown_channel,
};
use windows::{
    Win32::{
        Foundation::{ERROR_SERVICE_SPECIFIC_ERROR, NO_ERROR},
        System::Services::{
            RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
            SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
            SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
            SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
            SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
        },
    },
    core::PWSTR,
};

static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CURRENT_STATE: AtomicU32 = AtomicU32::new(0);
static EXIT_CODE: AtomicU32 = AtomicU32::new(0);
static SERVICE_EXIT_CODE: AtomicU32 = AtomicU32::new(0);
static CHECKPOINT: AtomicU32 = AtomicU32::new(0);
static WAIT_HINT: AtomicU32 = AtomicU32::new(0);
static TRANSITION: Mutex<()> = Mutex::new(());
static SHUTDOWN_CONTROLLER: OnceLock<ShutdownController> = OnceLock::new();
static SHUTDOWN_SIGNAL: OnceLock<ShutdownSignal> = OnceLock::new();

const START_WAIT_HINT_MS: u32 = 30_000;
const STOP_WAIT_HINT_MS: u32 = 30_000;
const SERVICE_FAILURE_RUNTIME: u32 = 1;
const SERVICE_FAILURE_PANIC: u32 = 2;

pub(super) fn dispatch() -> anyhow::Result<()> {
    let mut service_name = WINDOWS_SERVICE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(service_name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // The SCM owns the calling thread until the service main function exits.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .context("failed to connect host-monitor to the Windows Service Control Manager")
}

pub(super) fn shutdown_signal() -> Option<ShutdownSignal> {
    SHUTDOWN_SIGNAL.get().cloned()
}

unsafe extern "system" fn service_main(_argument_count: u32, _arguments: *mut PWSTR) {
    let outcome = catch_unwind(AssertUnwindSafe(service_main_inner));
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("host-monitor service failed: {error:#}");
            if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                let _ = report_stopped(SERVICE_FAILURE_RUNTIME);
            }
        }
        Err(_) => {
            eprintln!("host-monitor service panicked");
            if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                let _ = report_stopped(SERVICE_FAILURE_PANIC);
            }
        }
    }
}

fn service_main_inner() -> anyhow::Result<()> {
    let (controller, signal) = shutdown_channel();
    SHUTDOWN_CONTROLLER
        .set(controller)
        .map_err(|_| anyhow::anyhow!("SCM shutdown controller was already initialized"))?;
    SHUTDOWN_SIGNAL
        .set(signal.clone())
        .map_err(|_| anyhow::anyhow!("SCM shutdown signal was already initialized"))?;

    let service_name = WINDOWS_SERVICE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            windows::core::PCWSTR(service_name.as_ptr()),
            Some(control_handler),
            None,
        )
    }
    .context("failed to register the host-monitor service control handler")?;
    STATUS_HANDLE.store(handle.0, Ordering::Release);
    report_status(SERVICE_START_PENDING, 0, 1, START_WAIT_HINT_MS)?;

    super::init_tracing()?;
    let runtime = super::build_runtime()?;

    match runtime.block_on(super::run_client(Some(report_running))) {
        Ok(()) => report_stopped(0),
        Err(error) => {
            eprintln!("host-monitor runtime failed: {error:#}");
            report_stopped(runtime_failure_code(&error))?;
            Err(error)
        }
    }
}

// SCM has no console for stderr. Publish only a fixed diagnostic category,
// never paths, endpoints, credentials or arbitrary error text.
fn runtime_failure_code(error: &anyhow::Error) -> u32 {
    let mut code = SERVICE_FAILURE_RUNTIME;
    for cause in error.chain() {
        code = match cause.to_string().as_str() {
            "service configuration" => 10,
            "service maintenance lock" => 11,
            "failed to acquire the exclusive Client delivery session" => 12,
            "service shutdown handler" => 13,
            "service authorization state" => 14,
            "service host identity" => 15,
            "service status IPC" => 16,
            "service durable spool" => 17,
            "protected state directory" => 20,
            "protected state lock file" => 21,
            _ => code,
        };
    }
    code
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
            if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOPPED.0 {
                let _ = report_status(SERVICE_STOP_PENDING, 0, 1, STOP_WAIT_HINT_MS);
                if let Some(controller) = SHUTDOWN_CONTROLLER.get() {
                    controller.request_shutdown();
                }
                start_stop_progress_reporter();
            }
        }
        SERVICE_CONTROL_INTERROGATE => {
            let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
            let _ = repeat_current_status();
        }
        _ => {}
    }
    NO_ERROR.0
}

fn report_running() -> anyhow::Result<bool> {
    let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
    if SHUTDOWN_SIGNAL
        .get()
        .is_some_and(ShutdownSignal::is_requested)
    {
        report_status(SERVICE_STOP_PENDING, 0, 1, STOP_WAIT_HINT_MS)?;
        start_stop_progress_reporter();
        return Ok(false);
    }
    report_status(SERVICE_RUNNING, 0, 0, 0)?;
    Ok(true)
}

fn start_stop_progress_reporter() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("host-monitoring-service-stop-progress".into())
        .spawn(|| {
            let mut checkpoint = 2;
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
                if CURRENT_STATE.load(Ordering::Acquire) != SERVICE_STOP_PENDING.0 {
                    return;
                }
                let _ = report_status(SERVICE_STOP_PENDING, 0, checkpoint, STOP_WAIT_HINT_MS);
                checkpoint = checkpoint.saturating_add(1);
            }
        })
    {
        // This function is called from an extern "system" SCM callback.
        // Never panic across that FFI boundary if the OS cannot allocate a
        // progress thread; the already-published STOP_PENDING status and
        // main shutdown signal remain valid.
        eprintln!("failed to start service stop progress reporter: {error}");
    }
}

fn report_stopped(service_exit_code: u32) -> anyhow::Result<()> {
    let _transition = TRANSITION.lock().unwrap_or_else(|error| error.into_inner());
    let win32_exit_code = if service_exit_code == 0 {
        NO_ERROR.0
    } else {
        ERROR_SERVICE_SPECIFIC_ERROR.0
    };
    set_service_status(SERVICE_STOPPED, service_exit_code, 0, 0, win32_exit_code)
}

fn repeat_current_status() -> anyhow::Result<()> {
    let state = SERVICE_STATUS_CURRENT_STATE(CURRENT_STATE.load(Ordering::Acquire));
    set_service_status(
        state,
        SERVICE_EXIT_CODE.load(Ordering::Acquire),
        CHECKPOINT.load(Ordering::Acquire),
        WAIT_HINT.load(Ordering::Acquire),
        EXIT_CODE.load(Ordering::Acquire),
    )
}

fn report_status(
    state: SERVICE_STATUS_CURRENT_STATE,
    service_exit_code: u32,
    checkpoint: u32,
    wait_hint: u32,
) -> anyhow::Result<()> {
    set_service_status(state, service_exit_code, checkpoint, wait_hint, NO_ERROR.0)
}

fn set_service_status(
    state: SERVICE_STATUS_CURRENT_STATE,
    service_exit_code: u32,
    checkpoint: u32,
    wait_hint: u32,
    win32_exit_code: u32,
) -> anyhow::Result<()> {
    let raw_handle = STATUS_HANDLE.load(Ordering::Acquire);
    if raw_handle.is_null() {
        anyhow::bail!("the Windows service status handle is unavailable");
    }
    let controls = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls,
        dwWin32ExitCode: win32_exit_code,
        dwServiceSpecificExitCode: service_exit_code,
        dwCheckPoint: checkpoint,
        dwWaitHint: wait_hint,
    };
    unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(raw_handle), &status) }
        .context("failed to report host-monitor service status")?;
    CURRENT_STATE.store(state.0, Ordering::Release);
    EXIT_CODE.store(win32_exit_code, Ordering::Release);
    SERVICE_EXIT_CODE.store(service_exit_code, Ordering::Release);
    CHECKPOINT.store(checkpoint, Ordering::Release);
    WAIT_HINT.store(wait_hint, Ordering::Release);
    Ok(())
}
