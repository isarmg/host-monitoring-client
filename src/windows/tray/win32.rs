fn canonical_uuid(value: &str, label: &str) -> anyhow::Result<uuid::Uuid> {
    let parsed = uuid::Uuid::parse_str(value).with_context(|| format!("{label} is not a UUID"))?;
    ensure!(
        parsed.to_string() == value,
        "{label} must use canonical lowercase hyphenated text"
    );
    Ok(parsed)
}

fn validate_callback_nonce(nonce: &str) -> anyhow::Result<()> {
    ensure!(
        nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid pairing pipe nonce"
    );
    Ok(())
}

fn pair_pipe_name(nonce: &str) -> String {
    format!(r"\\.\pipe\HostMonitorPair-{nonce}")
}

fn read_pipe_frame(
    handle: HANDLE,
    limit: usize,
    deadline: Instant,
    peer_process: Option<HANDLE>,
) -> anyhow::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    read_pipe_exact(handle, &mut length, deadline, peer_process)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(length <= limit, "pairing pipe frame exceeds its size limit");
    let mut body = vec![0_u8; length];
    read_pipe_exact(handle, &mut body, deadline, peer_process)?;
    Ok(body)
}

fn write_pipe_frame(
    handle: HANDLE,
    body: &[u8],
    deadline: Instant,
    peer_process: Option<HANDLE>,
) -> anyhow::Result<()> {
    let length = u32::try_from(body.len()).context("pairing pipe frame is too large")?;
    write_pipe_all(handle, &length.to_le_bytes(), deadline, peer_process)?;
    write_pipe_all(handle, body, deadline, peer_process)
}

fn read_pipe_exact(
    handle: HANDLE,
    mut destination: &mut [u8],
    deadline: Instant,
    peer_process: Option<HANDLE>,
) -> anyhow::Result<()> {
    while !destination.is_empty() {
        ensure!(
            deadline_wait_millis(Instant::now(), deadline).is_some(),
            "timed out reading from the protected pairing pipe"
        );
        let event = create_overlapped_event("pairing pipe read")?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let read = match unsafe {
            ReadFile(
                handle,
                Some(destination),
                None,
                Some(&mut overlapped),
            )
        } {
            Ok(()) => overlapped_result(handle, &overlapped, "pairing pipe read")?,
            Err(error) => {
                let code = unsafe { GetLastError() };
                ensure!(
                    code == ERROR_IO_PENDING,
                    "failed to read from the protected pairing pipe ({code:?}): {error}"
                );
                wait_for_overlapped(
                    handle,
                    &overlapped,
                    deadline,
                    peer_process,
                    "pairing pipe read",
                )?
            }
        };
        let remaining = advance_pipe_transfer(destination.len(), read)?;
        let consumed = destination.len() - remaining;
        destination = &mut destination[consumed..];
    }
    Ok(())
}

fn write_pipe_all(
    handle: HANDLE,
    mut body: &[u8],
    deadline: Instant,
    peer_process: Option<HANDLE>,
) -> anyhow::Result<()> {
    while !body.is_empty() {
        ensure!(
            deadline_wait_millis(Instant::now(), deadline).is_some(),
            "timed out writing to the protected pairing pipe"
        );
        let event = create_overlapped_event("pairing pipe write")?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let written = match unsafe { WriteFile(handle, Some(body), None, Some(&mut overlapped)) } {
            Ok(()) => overlapped_result(handle, &overlapped, "pairing pipe write")?,
            Err(error) => {
                let code = unsafe { GetLastError() };
                ensure!(
                    code == ERROR_IO_PENDING,
                    "failed to write to the protected pairing pipe ({code:?}): {error}"
                );
                wait_for_overlapped(
                    handle,
                    &overlapped,
                    deadline,
                    peer_process,
                    "pairing pipe write",
                )?
            }
        };
        let remaining = advance_pipe_transfer(body.len(), written)?;
        let consumed = body.len() - remaining;
        body = &body[consumed..];
    }
    Ok(())
}

fn create_overlapped_event(operation: &str) -> anyhow::Result<KernelHandle> {
    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .with_context(|| format!("failed to create the {operation} completion event"))?;
    Ok(KernelHandle(event))
}

fn overlapped_result(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    operation: &str,
) -> anyhow::Result<u32> {
    let mut transferred = 0_u32;
    unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, false) }
        .with_context(|| format!("{operation} failed"))?;
    Ok(transferred)
}

fn wait_for_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    deadline: Instant,
    peer_process: Option<HANDLE>,
    operation: &str,
) -> anyhow::Result<u32> {
    let Some(wait_millis) = deadline_wait_millis(Instant::now(), deadline) else {
        cancel_and_reap_overlapped(handle, overlapped, operation)?;
        bail!("{operation} timed out");
    };
    let wait = if let Some(process) = peer_process {
        unsafe { WaitForMultipleObjects(&[overlapped.hEvent, process], false, wait_millis) }
    } else {
        unsafe { WaitForSingleObject(overlapped.hEvent, wait_millis) }
    };
    if wait == WAIT_OBJECT_0 {
        return overlapped_result(handle, overlapped, operation);
    }
    if peer_process.is_some() && wait.0 == WAIT_OBJECT_0.0 + 1 {
        cancel_and_reap_overlapped(handle, overlapped, operation)?;
        bail!("{operation} was interrupted because the elevated broker exited");
    }
    if wait == WAIT_TIMEOUT {
        cancel_and_reap_overlapped(handle, overlapped, operation)?;
        bail!("{operation} timed out");
    }
    let wait_error = (wait == WAIT_FAILED).then(|| unsafe { GetLastError() });
    cancel_and_reap_overlapped(handle, overlapped, operation)?;
    bail!("failed to wait for {operation} ({wait:?}, {wait_error:?})")
}

/// `CancelIoEx` only requests cancellation. Always reap the exact operation
/// before its stack-resident buffer, event, or `OVERLAPPED` can be released.
fn cancel_and_reap_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    operation: &str,
) -> anyhow::Result<()> {
    let cancel_failure = match unsafe { CancelIoEx(handle, Some(overlapped)) } {
        Ok(()) => None,
        Err(error) => {
            let code = unsafe { GetLastError() };
            (code != ERROR_NOT_FOUND).then(|| format!("{code:?}: {error}"))
        }
    };
    let mut transferred = 0_u32;
    let completion_status = match unsafe {
        GetOverlappedResult(handle, overlapped, &mut transferred, true)
    } {
        Ok(()) => None,
        Err(error) => {
            let code = unsafe { GetLastError() };
            Some(format!("{code:?}: {error}"))
        }
    };
    // A cancellation race may finish normally, as cancelled, or with another
    // terminal I/O error such as ERROR_BROKEN_PIPE. The dedicated manual-reset
    // event proves that every such result is complete and the stack resources
    // can be released; the particular terminal error belongs to the original
    // timeout/peer-exit path rather than to cleanup itself.
    let completion_wait = unsafe { WaitForSingleObject(overlapped.hEvent, 0) };
    ensure!(
        completion_wait == WAIT_OBJECT_0,
        "failed to reap {operation} ({completion_wait:?}, terminal status {completion_status:?})"
    );
    ensure!(
        cancel_failure.is_none(),
        "failed to cancel {operation}: {}",
        cancel_failure.unwrap_or_default()
    );
    Ok(())
}

fn connect_pipe_with_deadline(
    pipe: HANDLE,
    peer_process: HANDLE,
    deadline: Instant,
) -> anyhow::Result<()> {
    ensure!(
        deadline_wait_millis(Instant::now(), deadline).is_some(),
        "timed out waiting for the elevated pairing broker"
    );
    let event = create_overlapped_event("pairing pipe connection")?;
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    match unsafe { ConnectNamedPipe(pipe, Some(&mut overlapped)) } {
        Ok(()) => Ok(()),
        Err(error) => {
            let code = unsafe { GetLastError() };
            match code {
                ERROR_PIPE_CONNECTED => Ok(()),
                ERROR_IO_PENDING => {
                    wait_for_overlapped(
                        pipe,
                        &overlapped,
                        deadline,
                        Some(peer_process),
                        "pairing pipe connection",
                    )?;
                    Ok(())
                }
                _ => bail!("protected pairing pipe connection failed ({code:?}): {error}"),
            }
        }
    }
}

fn set_clean_client_environment(command: &mut Command) -> anyhow::Result<()> {
    command.env_clear();
    let windows_directory = windows_directory()?;
    let program_data = known_folder(&FOLDERID_ProgramData)?;
    let state_root = program_data.join(CONFIG_DIRECTORY);
    command.env("SystemRoot", &windows_directory);
    command.env("WINDIR", &windows_directory);
    command.env("PROGRAMDATA", &program_data);
    command.env("HOST_MONITOR_STATE_DIR", &state_root);
    // Deliberately do not inherit HOST_MONITOR_*, proxy variables, PATH,
    // TEMP/TMP, certificate overrides, or user profile variables into the
    // privileged child. PROGRAMDATA and HOST_MONITOR_STATE_DIR above are
    // independently derived from a Windows Known Folder, never inherited.
    Ok(())
}

fn ensure_process_is_elevated() -> anyhow::Result<()> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("failed to inspect the elevated process token")?;
    let _token = KernelHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0_u32;
    unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .context("failed to read process elevation state")?;
    ensure!(
        returned as usize >= size_of::<TOKEN_ELEVATION>() && elevation.TokenIsElevated != 0,
        "this internal operation requires Windows administrator elevation"
    );
    Ok(())
}

fn create_single_instance_mutex(
    name: &str,
    privileged: bool,
) -> anyhow::Result<Option<KernelHandle>> {
    let name = wide(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
        .context("failed to create the operation mutex")?;
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe { CloseHandle(handle) }.ok();
        return Ok(None);
    }
    if privileged {
        ensure_process_is_elevated()?;
    }
    Ok(Some(KernelHandle(handle)))
}

struct KernelHandle(HANDLE);

impl Drop for KernelHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// `windows::HANDLE` intentionally is not `Send`. Transfer only the
/// integer handle value between threads, then reconstruct ownership in the
/// destination thread. This wrapper owns and closes the handle if a thread
/// fails to start before the transfer is consumed.
struct TransferHandle(Option<isize>);

impl TransferHandle {
    fn new(handle: KernelHandle) -> Self {
        let raw = handle.0.0 as isize;
        std::mem::forget(handle);
        Self(Some(raw))
    }

    fn into_kernel(mut self) -> KernelHandle {
        let raw = self
            .0
            .take()
            .expect("transferred handle was already consumed");
        KernelHandle(HANDLE(raw as *mut c_void))
    }
}

impl Drop for TransferHandle {
    fn drop(&mut self) {
        if let Some(raw) = self.0.take() {
            let _ = unsafe { CloseHandle(HANDLE(raw as *mut c_void)) };
        }
    }
}

struct PairChildGuard {
    child: Option<Child>,
    cancel_event_raw: isize,
}

impl PairChildGuard {
    fn new(child: Child, cancel_event: HANDLE) -> Self {
        Self {
            child: Some(child),
            cancel_event_raw: cancel_event.0 as isize,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("pairing child was disarmed")
    }

    fn disarm(&mut self) {
        self.child.take();
    }
}

impl Drop for PairChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let event = HANDLE(self.cancel_event_raw as *mut c_void);
        let _ = unsafe { SetEvent(event) };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                _ => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    Other(u32),
}

impl ServiceState {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::StartPending => "正在启动",
            Self::StopPending => "正在停止",
            Self::Running => "正在运行",
            Self::Other(_) => "其他状态",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::StartPending => "starting",
            Self::StopPending => "stopping",
            Self::Running => "running",
            Self::Other(_) => "unknown",
        }
    }
}

struct ScmHandle(SC_HANDLE);

impl Drop for ScmHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn open_service(access: u32) -> anyhow::Result<(ScmHandle, ScmHandle)> {
    let manager = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
        .context("failed to open the Windows Service Control Manager")?;
    let manager = ScmHandle(manager);
    let service_name = wide(WINDOWS_SERVICE_NAME);
    let service = unsafe { OpenServiceW(manager.0, PCWSTR(service_name.as_ptr()), access) }
        .context("host-monitor Windows service is not installed or cannot be accessed")?;
    Ok((manager, ScmHandle(service)))
}

fn query_service_state() -> anyhow::Result<ServiceState> {
    let (_manager, service) = open_service(SERVICE_QUERY_STATUS)?;
    query_open_service_state(service.0)
}

fn query_open_service_state(service: SC_HANDLE) -> anyhow::Result<ServiceState> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0_u32;
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    unsafe { QueryServiceStatusEx(service, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }
        .context("failed to query the host-monitor service")?;
    Ok(match status.dwCurrentState {
        value if value == SERVICE_STOPPED => ServiceState::Stopped,
        value if value == SERVICE_START_PENDING => ServiceState::StartPending,
        value if value == SERVICE_STOP_PENDING => ServiceState::StopPending,
        value if value == SERVICE_RUNNING => ServiceState::Running,
        value => ServiceState::Other(value.0),
    })
}

fn wait_for_stable_service_state(timeout: Duration) -> anyhow::Result<ServiceState> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = query_service_state()?;
        if !matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        ) {
            return Ok(state);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for the Client service to become stable"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn start_service() -> anyhow::Result<()> {
    let (_manager, service) = open_service(SERVICE_QUERY_STATUS | SERVICE_START)?;
    match query_open_service_state(service.0)? {
        ServiceState::Running => return Ok(()),
        ServiceState::StartPending => {}
        ServiceState::Stopped => unsafe { StartServiceW(service.0, None) }
            .context("failed to start the host-monitor service")?,
        state => bail!(
            "cannot start the Client service from state {}",
            state.label()
        ),
    }
    wait_open_service_for(service.0, ServiceState::Running, Duration::from_secs(30))
}

fn stop_service() -> anyhow::Result<()> {
    let (_manager, service) = open_service(SERVICE_QUERY_STATUS | SERVICE_STOP)?;
    match query_open_service_state(service.0)? {
        ServiceState::Stopped => return Ok(()),
        ServiceState::StopPending => {}
        ServiceState::Running => {
            let mut status = SERVICE_STATUS::default();
            unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
                .context("failed to stop the host-monitor service")?;
        }
        state => bail!("cannot stop the Client service from state {}", state.label()),
    }
    wait_open_service_for(service.0, ServiceState::Stopped, Duration::from_secs(30))
}

fn restart_service() -> anyhow::Result<()> {
    stop_service().context("pairing succeeded but the Client service could not be stopped")?;
    start_service().context("pairing succeeded but the Client service could not be restarted")
}

fn wait_open_service_for(
    service: SC_HANDLE,
    expected: ServiceState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = query_open_service_state(service)?;
        if state == expected {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for the Client service to reach {} (currently {})",
            expected.label(),
            state.label()
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn installed_program_root() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramFiles)?.join(CONFIG_DIRECTORY))
}

fn installed_state_root() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramData)?.join(CONFIG_DIRECTORY))
}

fn local_preferences_path() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_LocalAppData)?
        .join(CONFIG_DIRECTORY)
        .join("tray.json"))
}

fn known_folder(identifier: &windows::core::GUID) -> anyhow::Result<PathBuf> {
    let raw = unsafe { SHGetKnownFolderPath(identifier, KF_FLAG_DEFAULT, None) }
        .context("failed to resolve a Windows Known Folder")?;
    let result = (|| {
        let mut length = 0_usize;
        while unsafe { *raw.0.add(length) } != 0 {
            length += 1;
            ensure!(length < 32 * 1024, "Known Folder path is too long");
        }
        let value = unsafe { std::slice::from_raw_parts(raw.0, length) };
        let path = PathBuf::from(OsString::from_wide(value));
        ensure!(
            path.is_absolute(),
            "Known Folder did not return an absolute path"
        );
        Ok(path)
    })();
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };
    result
}

fn windows_directory() -> anyhow::Result<PathBuf> {
    let mut buffer = vec![0_u16; 260];
    loop {
        let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
        ensure!(length != 0, "GetWindowsDirectoryW failed");
        if length < buffer.len() {
            buffer.truncate(length);
            break;
        }
        ensure!(length < 32 * 1024, "Windows directory path is too long");
        buffer.resize(length + 1, 0);
    }
    let path = PathBuf::from(OsString::from_wide(&buffer));
    ensure!(path.is_absolute() && path.is_dir(), "SystemRoot is invalid");
    Ok(path)
}
