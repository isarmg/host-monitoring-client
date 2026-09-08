fn open_client_service(access: u32) -> anyhow::Result<Option<ServiceHandle>> {
    let manager = unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }
        .context("failed to open the Service Control Manager")?;
    let manager = ServiceHandle(manager);
    match unsafe {
        OpenServiceW(
            manager.0,
            PCWSTR(wide_null(OsStr::new(WINDOWS_SERVICE_NAME)).as_ptr()),
            access,
        )
    } {
        Ok(service) => Ok(Some(ServiceHandle(service))),
        Err(error) if error.code().0 == hresult_from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) => {
            Ok(None)
        }
        Err(error) => Err(error).context("failed to open host-monitor service"),
    }
}

struct ServiceHandle(SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn validate_client_service(service: &ServiceHandle, paths: &FixedPaths) -> anyhow::Result<()> {
    let buffer = query_service_config(service)?;
    let config = unsafe { &*(buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()) };
    ensure!(
        config.dwServiceType == SERVICE_WIN32_OWN_PROCESS,
        "host-monitor is not an own-process service"
    );
    ensure!(
        matches!(config.dwStartType, SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED),
        "host-monitor has an unsupported service startup type"
    );
    let image_path = unsafe { config.lpBinaryPathName.to_string() }?;
    let arguments = split_windows_command_line(&image_path)?;
    ensure!(
        arguments.len() == 5,
        "host-monitor ImagePath argument count is invalid"
    );
    ensure!(
        arguments[0].eq_ignore_ascii_case(&paths.program_exe.to_string_lossy())
            && arguments[1] == "--windows-service"
            && arguments[2] == "run"
            && arguments[3] == "--config"
            && arguments[4].eq_ignore_ascii_case(&paths.config.to_string_lossy()),
        "host-monitor ImagePath does not match the fixed executable and arguments"
    );
    let start_name = unsafe { config.lpServiceStartName.to_string() }?;
    ensure!(
        is_local_service_name(&start_name),
        "host-monitor does not run as LOCAL SERVICE"
    );
    Ok(())
}

fn query_service_status(service: &ServiceHandle) -> anyhow::Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    let buffer = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(buffer), &mut needed) }
        .context("failed to query host-monitor service status")?;
    Ok(status)
}

fn service_is_active(service: &ServiceHandle) -> anyhow::Result<bool> {
    let state = query_service_status(service)?.dwCurrentState;
    if state == SERVICE_RUNNING {
        Ok(true)
    } else if state == SERVICE_STOPPED {
        Ok(false)
    } else {
        bail!(
            "host-monitor is not in a stable running/stopped state ({})",
            state.0
        )
    }
}

fn wait_for_stable_service_state(
    service: &ServiceHandle,
) -> anyhow::Result<windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let state = query_service_status(service)?.dwCurrentState;
        if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            return Ok(state);
        }
        ensure!(
            state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING,
            "host-monitor entered unsupported rollback state {}",
            state.0
        );
        ensure!(
            Instant::now() < deadline,
            "host-monitor did not reach a stable state within 30 seconds"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_service_state(
    service: &ServiceHandle,
    expected: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let state = query_service_status(service)?.dwCurrentState;
        if state == expected {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "host-monitor did not reach service state {} within 30 seconds (current {})",
            expected.0,
            state.0
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn restore_service_state(
    paths: &FixedPaths,
    sid_type: u32,
    failure_actions_on_non_crash: bool,
    should_be_running: bool,
    program_acl: ProgramAclRestore,
) -> anyhow::Result<()> {
    let service = open_client_service(
        SERVICE_CHANGE_CONFIG
            | SERVICE_QUERY_CONFIG
            | SERVICE_QUERY_STATUS
            | SERVICE_START
            | SERVICE_STOP,
    )?
    .context("the rollback host-monitor service is not present")?;
    validate_client_service(&service, paths)?;
    match program_acl {
        ProgramAclRestore::PreserveSnapshot => {
            validate_program_tree(&paths.program_root)?;
        }
        ProgramAclRestore::SecureCurrent => {
            secure_program_for_service(&paths.program_root)?;
            validate_program_tree(&paths.program_root)?;
        }
    }
    set_service_sid_type(&service, sid_type)?;
    ensure!(
        query_service_sid_type(&service)? == sid_type,
        "restored service SID type did not verify"
    );
    set_failure_actions_on_non_crash(&service, failure_actions_on_non_crash)?;
    ensure!(
        query_failure_actions_on_non_crash(&service)? == failure_actions_on_non_crash,
        "restored non-crash failure policy did not verify"
    );
    let current = wait_for_stable_service_state(&service)?;
    if should_be_running {
        if current == SERVICE_STOPPED {
            unsafe { StartServiceW(service.0, None) }
                .context("failed to restart the rollback host-monitor service")?;
        }
        wait_for_service_state(&service, SERVICE_RUNNING)
    } else {
        if current != SERVICE_STOPPED {
            let mut status = SERVICE_STATUS::default();
            unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
                .context("failed to stop the rollback host-monitor service")?;
        }
        wait_for_service_state(&service, SERVICE_STOPPED)
    }
}

fn query_service_config(service: &ServiceHandle) -> anyhow::Result<Vec<usize>> {
    let mut needed = 0;
    let first = unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) };
    ensure!(
        first.is_err() && needed >= size_of::<QUERY_SERVICE_CONFIGW>() as u32,
        "could not determine host-monitor configuration size"
    );
    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    unsafe {
        QueryServiceConfigW(
            service.0,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * size_of::<usize>()) as u32,
            &mut needed,
        )
    }?;
    Ok(buffer)
}

fn query_service_sid_type(service: &ServiceHandle) -> anyhow::Result<u32> {
    let mut needed = 0;
    let first = unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            None,
            &mut needed,
        )
    };
    ensure!(
        first.is_err() && needed as usize >= size_of::<SERVICE_SID_INFO>(),
        "could not size service SID query"
    );
    let mut buffer = vec![0u8; needed as usize];
    unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            Some(&mut buffer),
            &mut needed,
        )
    }?;
    Ok(unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<SERVICE_SID_INFO>()) }.dwServiceSidType)
}

fn set_service_sid_type(service: &ServiceHandle, sid_type: u32) -> anyhow::Result<()> {
    let info = SERVICE_SID_INFO {
        dwServiceSidType: sid_type,
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            Some((&info as *const SERVICE_SID_INFO).cast::<c_void>()),
        )
    }
    .context("failed to configure the host-monitor service SID")
}

fn query_failure_actions_on_non_crash(service: &ServiceHandle) -> anyhow::Result<bool> {
    let mut needed = 0;
    let first = unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            None,
            &mut needed,
        )
    };
    ensure!(
        first.is_err() && needed as usize >= size_of::<SERVICE_FAILURE_ACTIONS_FLAG>(),
        "could not size the non-crash failure policy query"
    );
    let mut buffer = vec![0u8; needed as usize];
    unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            Some(&mut buffer),
            &mut needed,
        )
    }?;
    Ok(
        unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<SERVICE_FAILURE_ACTIONS_FLAG>()) }
            .fFailureActionsOnNonCrashFailures
            .as_bool(),
    )
}

fn set_failure_actions_on_non_crash(service: &ServiceHandle, enabled: bool) -> anyhow::Result<()> {
    let info = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: enabled.into(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            Some((&info as *const SERVICE_FAILURE_ACTIONS_FLAG).cast::<c_void>()),
        )
    }
    .context("failed to configure non-crash host-monitor recovery")
}

fn service_sid_string() -> anyhow::Result<String> {
    // Service SIDs are deterministic: S-1-5-80 plus five little-endian
    // u32 words from SHA-1(uppercase UTF-16 service name).
    use sha1::{Digest, Sha1};
    let mut bytes = Vec::new();
    for code_unit in WINDOWS_SERVICE_NAME.to_ascii_uppercase().encode_utf16() {
        bytes.extend_from_slice(&code_unit.to_le_bytes());
    }
    let digest = Sha1::digest(&bytes);
    let parts = digest
        .as_chunks::<4>()
        .0
        .iter()
        .map(|part| u32::from_le_bytes(*part))
        .map(|part| part.to_string())
        .collect::<Vec<_>>();
    ensure!(parts.len() == 5, "invalid SHA-1 service SID digest");
    Ok(format!("S-1-5-80-{}", parts.join("-")))
}

fn split_windows_command_line(command_line: &str) -> anyhow::Result<Vec<String>> {
    let wide = wide_null(OsStr::new(command_line));
    let mut count = 0;
    let raw =
        unsafe { windows::Win32::UI::Shell::CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut count) };
    ensure!(
        !raw.is_null() && count > 0,
        "failed to parse service ImagePath"
    );
    let values = unsafe { std::slice::from_raw_parts(raw, count as usize) }
        .iter()
        .map(|value| unsafe { value.to_string() })
        .collect::<Result<Vec<_>, _>>()?;
    unsafe { LocalFree(Some(windows::Win32::Foundation::HLOCAL(raw.cast()))) };
    Ok(values)
}

fn is_local_service_name(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "s-1-5-19" | "localservice" | "nt authority\\localservice" | "nt authority\\local service"
    )
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn file_link_count(path: &Path) -> anyhow::Result<u32> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .with_context(|| {
            format!(
                "failed to open {} without following reparse points",
                path.display()
            )
        })?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .with_context(|| format!("failed to query link count for {}", path.display()))?;
    Ok(information.nNumberOfLinks)
}

fn hresult_from_win32(code: u32) -> i32 {
    (0x8007_0000u32 | code) as i32
}
