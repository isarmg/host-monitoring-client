fn open_client_service(access: u32) -> anyhow::Result<Option<ServiceHandle>> {
    // SAFETY: Null server/database select the local SCM; a successful handle
    // is owned by ServiceHandle and is closed once on every subsequent path.
    let manager = unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }
        .context("failed to open the Service Control Manager")?;
    let manager = ServiceHandle(manager);
    // SAFETY: The manager is live; the fixed terminated service name remains
    // borrowed for this synchronous call. The returned handle has unique ownership.
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
        Err(error) => Err(error).context("failed to open xsoc service"),
    }
}

struct ServiceHandle(SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        // SAFETY: This RAII value uniquely owns a successfully opened SCM handle.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn validate_client_service(service: &ServiceHandle, paths: &FixedPaths) -> anyhow::Result<()> {
    let buffer = query_service_config(service)?;
    // SAFETY: query_service_config uses usize-aligned storage, verifies at
    // least the native header size and reads it only after a successful SDK call.
    let config = unsafe { &*(buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()) };
    ensure!(
        config.dwServiceType == SERVICE_WIN32_OWN_PROCESS,
        "xsoc is not an own-process service"
    );
    ensure!(
        matches!(
            config.dwStartType,
            SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED
        ),
        "xsoc has an unsupported service startup type"
    );
    let image_path = service_config_string(&buffer, config.lpBinaryPathName)?;
    let arguments = split_windows_command_line(&image_path)?;
    ensure!(
        arguments.len() == 5,
        "xsoc ImagePath argument count is invalid"
    );
    ensure!(
        arguments[0].eq_ignore_ascii_case(&paths.program_exe.to_string_lossy())
            && arguments[1] == "--windows-service"
            && arguments[2] == "run"
            && arguments[3] == "--config"
            && arguments[4].eq_ignore_ascii_case(&paths.config.to_string_lossy()),
        "xsoc ImagePath does not match the fixed executable and arguments"
    );
    let start_name = service_config_string(&buffer, config.lpServiceStartName)?;
    ensure!(
        is_local_service_name(&start_name),
        "xsoc does not run as LOCAL SERVICE"
    );
    Ok(())
}

fn query_service_status(service: &ServiceHandle) -> anyhow::Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    // SAFETY: This temporary byte view covers exactly the live initialized
    // native output struct; no references to status are used while it is borrowed.
    let buffer = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    // SAFETY: The service is live and the output view has the exact requested size.
    unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(buffer), &mut needed) }
        .context("failed to query xsoc service status")?;
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
            "xsoc is not in a stable running/stopped state ({})",
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
            "xsoc entered unsupported rollback state {}",
            state.0
        );
        ensure!(
            Instant::now() < deadline,
            "xsoc did not reach a stable state within 30 seconds"
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
            "xsoc did not reach service state {} within 30 seconds (current {})",
            expected.0,
            state.0
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn stop_client_service(service: &ServiceHandle) -> anyhow::Result<()> {
    if wait_for_stable_service_state(service)? != SERVICE_STOPPED {
        let mut status = SERVICE_STATUS::default();
        // SAFETY: A live service handle with STOP access and initialized status
        // output are borrowed only for the synchronous control call.
        unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
            .context("failed to stop xsoc for maintenance")?;
    }
    wait_for_service_state(service, SERVICE_STOPPED)
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
    .context("the rollback xsoc service is not present")?;
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
            // SAFETY: The validated own-process service handle has START access;
            // no argument pointers are supplied or retained.
            unsafe { StartServiceW(service.0, None) }
                .context("failed to restart the rollback xsoc service")?;
        }
        wait_for_service_state(&service, SERVICE_RUNNING)
    } else {
        stop_client_service(&service)
    }
}

fn query_service_config(service: &ServiceHandle) -> anyhow::Result<Vec<usize>> {
    let mut needed = 0;
    // SAFETY: The size query deliberately supplies no output storage.
    let first = unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) };
    ensure!(
        first.is_err()
            && needed >= size_of::<QUERY_SERVICE_CONFIGW>() as u32
            && needed <= 8192,
        "could not determine xsoc configuration size"
    );
    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    // SAFETY: usize storage satisfies native header alignment and the checked
    // size is bounded to the documented SCM configuration maximum.
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
    // SAFETY: Only a size output is supplied for the fixed SDK information class.
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
    // SAFETY: The bounded byte buffer remains writable for the SDK call; the
    // native header below is read unaligned only after successful initialization.
    unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            Some(&mut buffer),
            &mut needed,
        )
    }?;
    // SAFETY: The successful call initialized the checked complete header.
    Ok(unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<SERVICE_SID_INFO>()) }.dwServiceSidType)
}

fn set_service_sid_type(service: &ServiceHandle, sid_type: u32) -> anyhow::Result<()> {
    let info = SERVICE_SID_INFO {
        dwServiceSidType: sid_type,
    };
    // SAFETY: The live service handle and initialized native information struct
    // are borrowed for this synchronous call; the information class matches its type.
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            Some((&info as *const SERVICE_SID_INFO).cast::<c_void>()),
        )
    }
    .context("failed to configure the xsoc service SID")
}

fn query_failure_actions_on_non_crash(service: &ServiceHandle) -> anyhow::Result<bool> {
    let mut needed = 0;
    // SAFETY: Only a size output is supplied for the fixed SDK information class.
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
    // SAFETY: The bounded byte buffer remains writable for the SDK call; the
    // native header below is read unaligned only after successful initialization.
    unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            Some(&mut buffer),
            &mut needed,
        )
    }?;
    // SAFETY: The successful call initialized the checked complete header.
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
    // SAFETY: The live service handle and initialized native information struct
    // are borrowed for this synchronous call; the information class matches its type.
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            Some((&info as *const SERVICE_FAILURE_ACTIONS_FLAG).cast::<c_void>()),
        )
    }
    .context("failed to configure non-crash xsoc recovery")
}

fn service_sid_string() -> anyhow::Result<String> {
    Ok(xsoc::service::windows_service_sid())
}

struct NativeArguments(*mut PWSTR);
impl Drop for NativeArguments {
    fn drop(&mut self) {
        // SAFETY: CommandLineToArgvW returns one LocalAlloc block owned by this
        // value. It is released on conversion errors as well as success.
        unsafe { LocalFree(Some(HLOCAL(self.0.cast()))) };
    }
}

fn split_windows_command_line(command_line: &str) -> anyhow::Result<Vec<String>> {
    ensure!(command_line.encode_utf16().count() <= 8192, "service ImagePath exceeds its limit");
    let wide = wide_null(OsStr::new(command_line));
    let mut count = 0;
    // SAFETY: The terminated input and initialized count output remain live;
    // the SDK returns a single allocation containing pointers and their strings.
    let raw = unsafe {
        windows::Win32::UI::Shell::CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut count)
    };
    ensure!(!raw.is_null(), "failed to parse service ImagePath");
    let arguments = NativeArguments(raw);
    ensure!((1..=128).contains(&count), "service ImagePath argument count exceeds its limit");
    // SAFETY: A successful SDK call initialized exactly count entries in the
    // still-owned allocation; each string remains valid until arguments drops.
    let values = unsafe { std::slice::from_raw_parts(arguments.0, count as usize) }
        .iter()
        .map(|value| {
            // SAFETY: These pointers refer to SDK-created terminated strings in
            // the live CommandLineToArgvW allocation, not to external input memory.
            unsafe { value.to_string() }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values)
}

fn service_config_string(buffer: &[usize], value: PWSTR) -> anyhow::Result<String> {
    let base = buffer.as_ptr() as usize;
    let end = base.checked_add(std::mem::size_of_val(buffer)).context("service config bounds overflow")?;
    let pointer = value.as_ptr() as usize;
    ensure!(pointer >= base && pointer < end && pointer.is_multiple_of(2), "service config string is outside its returned buffer");
    let units = (end - pointer) / 2;
    // SAFETY: The pointer was checked for u16 alignment and lies in the live
    // usize-backed allocation; this view ends before its allocation boundary.
    let text = unsafe { std::slice::from_raw_parts(value.as_ptr(), units) };
    let length = text.iter().position(|unit| *unit == 0).context("service config string is not terminated")?;
    String::from_utf16(&text[..length]).context("service config string is invalid UTF-16")
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
    // SAFETY: The live std File owns the native handle and the SDK initializes
    // a complete native output struct; no handle ownership is transferred.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .with_context(|| format!("failed to query link count for {}", path.display()))?;
    Ok(information.nNumberOfLinks)
}

fn hresult_from_win32(code: u32) -> i32 {
    (0x8007_0000u32 | code) as i32
}

#[cfg(test)]
mod service_bounds_tests {
    use super::*;

    #[test]
    fn scm_strings_must_be_terminated_inside_the_returned_allocation() {
        let mut buffer = [0usize; 4];
        buffer[0] = 0x0042_0041;
        let text = PWSTR(buffer.as_mut_ptr().cast());
        assert_eq!(service_config_string(&buffer, text).unwrap(), "AB");
        assert!(service_config_string(&buffer, PWSTR::null()).is_err());
        let outside = PWSTR(buffer.as_mut_ptr().wrapping_add(buffer.len()).cast());
        assert!(service_config_string(&buffer, outside).is_err());
        let unaligned = PWSTR(buffer.as_mut_ptr().cast::<u8>().wrapping_add(1).cast());
        assert!(service_config_string(&buffer, unaligned).is_err());
        buffer.fill(usize::MAX);
        assert!(service_config_string(&buffer, text).is_err());
        buffer.fill(0);
        buffer[0] = 0xd800;
        assert!(service_config_string(&buffer, text).is_err());
    }

    #[test]
    fn scm_image_path_parsing_handles_quotes_and_rejects_unbounded_input() {
        assert_eq!(
            split_windows_command_line("\"C:\\Program Files\\xsoc.exe\" run").unwrap(),
            ["C:\\Program Files\\xsoc.exe", "run"]
        );
        assert!(split_windows_command_line(&"x".repeat(8193)).is_err());
        assert!(split_windows_command_line(&"x ".repeat(129)).is_err());
    }
}
