fn run_tray(open: bool) -> anyhow::Result<()> {
    let Some(_single_instance) = create_single_instance_mutex(
        "Local\\HostMonitorTray-4E473D9E-77F7-4F9C-AFE6-CC550E72F5A4",
        false,
    )?
    else {
        // HKLM Run, an explicit launch, and the current installer launcher
        // may race. One per-session tray already satisfies the request.
        if open {
            signal_existing_tray_to_open_configuration();
        }
        return Ok(());
    };
    let server = LocalControlServer::start()?;
    LOCAL_SERVER
        .set(server)
        .map_err(|_| anyhow::anyhow!("local configuration server was already initialized"))?;
    let restart = wide("--startup");
    unsafe { RegisterApplicationRestart(PCWSTR(restart.as_ptr()), Default::default()) }
        .context("failed to register the tray with Windows Restart Manager")?;

    let module = unsafe { GetModuleHandleW(None) }.context("failed to get tray module handle")?;
    let instance = windows::Win32::Foundation::HINSTANCE(module.0);
    let class_name = wide(WINDOW_CLASS_NAME);
    let icon =
        unsafe { LoadIconW(None, IDI_APPLICATION) }.context("failed to load the tray icon")?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: icon,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    ensure!(
        unsafe { RegisterClassW(&class) } != 0,
        "failed to register the tray window class: {}",
        std::io::Error::last_os_error()
    );
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            w!("host-monitor Tray"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .context("failed to create the tray message window")?;
    WINDOW_HANDLE.store(window.0 as isize, Ordering::Release);
    let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TASKBAR_CREATED_MESSAGE.store(taskbar_message, Ordering::Release);
    add_tray_icon(window, icon)?;
    if open && let Err(error) = open_local_configuration() {
        show_error(&format!("无法打开本地配置：\n\n{error:#}"));
    }

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 {
            delete_tray_icon(window);
            bail!(
                "tray message loop failed: {}",
                std::io::Error::last_os_error()
            );
        }
        if result.0 == 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire) {
        if let Ok(icon) = unsafe { LoadIconW(None, IDI_APPLICATION) } {
            let _ = add_tray_icon(window, icon);
        }
        return LRESULT(0);
    }
    match message {
        OPEN_CONFIGURATION_MESSAGE => {
            if let Err(error) = open_local_configuration() {
                show_error(&format!("无法打开本地配置：\n\n{error:#}"));
            }
            LRESULT(0)
        }
        EXIT_SERVICE_STOPPED_MESSAGE => {
            if EXIT_PENDING.swap(false, Ordering::AcqRel) {
                match query_service_state() {
                    Ok(ServiceState::Stopped) => {
                        let _ = unsafe { DestroyWindow(window) };
                    }
                    Ok(state) => show_error(&format!(
                        "Client 服务尚未停止（{}），托盘将继续运行。",
                        state.label()
                    )),
                    Err(error) => show_error(&format!(
                        "无法确认 Client 服务已停止，托盘将继续运行：\n\n{error:#}"
                    )),
                }
            }
            LRESULT(0)
        }
        REFRESH_TRAY_STATUS_MESSAGE => {
            let _ = update_tray_tooltip(window);
            LRESULT(0)
        }
        TRAY_CALLBACK_MESSAGE => {
            let event = lparam.0 as u32;
            if matches!(event, WM_RBUTTONUP | WM_CONTEXTMENU) {
                show_tray_menu(window);
            } else if event == WM_LBUTTONDBLCLK
                && let Err(error) = open_local_configuration()
            {
                show_error(&format!("无法打开本地配置：\n\n{error:#}"));
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(window) };
            LRESULT(0)
        }
        WM_DESTROY => {
            delete_tray_icon(window);
            WINDOW_HANDLE.store(0, Ordering::Release);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn signal_existing_tray_to_open_configuration() {
    let class_name = wide(WINDOW_CLASS_NAME);
    for _ in 0..20 {
        if let Ok(window) = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()) } {
            let _ = unsafe {
                PostMessageW(
                    Some(window),
                    OPEN_CONFIGURATION_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn add_tray_icon(window: HWND, icon: HICON) -> anyhow::Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    copy_wide_fixed(&mut data.szTip, &tray_tooltip());
    ensure!(
        unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool(),
        "Windows rejected the tray icon"
    );
    Ok(())
}

fn tray_tooltip() -> String {
    match query_service_state() {
        Ok(state) => format!("host-monitor · {}", state.label()),
        Err(_) => "host-monitor · 服务状态不可用".to_string(),
    }
}

fn update_tray_tooltip(window: HWND) -> anyhow::Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        uFlags: NIF_TIP,
        ..Default::default()
    };
    copy_wide_fixed(&mut data.szTip, &tray_tooltip());
    ensure!(
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool(),
        "Windows rejected the tray status update"
    );
    Ok(())
}

fn notify_tray_status_changed() {
    let raw = WINDOW_HANDLE.load(Ordering::Acquire);
    if raw != 0 {
        let window = HWND(raw as *mut c_void);
        let _ = unsafe {
            PostMessageW(
                Some(window),
                REFRESH_TRAY_STATUS_MESSAGE,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

fn delete_tray_icon(window: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        ..Default::default()
    };
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

fn show_tray_menu(window: HWND) {
    let _ = update_tray_tooltip(window);
    let result = (|| -> anyhow::Result<()> {
        let menu = unsafe { CreatePopupMenu() }.context("failed to create tray menu")?;
        let _menu = MenuHandle(menu);
        append_menu(menu, COMMAND_OPEN_LOCAL, "打开本地配置")?;
        append_menu(menu, COMMAND_PAIR, "配对")?;
        append_menu(menu, COMMAND_STATUS, "打开状态与连接检测")?;
        let action = match query_service_state() {
            Ok(ServiceState::Running) | Ok(ServiceState::StartPending) => ServiceAction::Stop,
            _ => ServiceAction::Start,
        };
        append_menu(
            menu,
            COMMAND_SERVICE,
            match action {
                ServiceAction::Start => "启动 Client 服务（需要管理员权限）",
                ServiceAction::Stop => "停止 Client 服务（需要管理员权限）",
            },
        )?;
        unsafe { AppendMenuW(menu, MENU_ITEM_FLAGS(0x0000_0800), 0, PCWSTR::null()) }
            .context("failed to add tray menu separator")?;
        append_menu(
            menu,
            COMMAND_EXIT,
            "停止本次服务并退出托盘（重启后自动运行）",
        )?;
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.context("failed to locate the tray menu")?;
        let _ = unsafe { SetForegroundWindow(window) };
        let selected = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                point.x,
                point.y,
                Some(0),
                window,
                None,
            )
        }
        .0 as usize;
        if selected != 0 {
            handle_tray_command(window, selected, action)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        show_error(&format!("托盘菜单操作失败：\n\n{error:#}"));
    }
}

fn handle_tray_command(
    window: HWND,
    command: usize,
    service_action: ServiceAction,
) -> anyhow::Result<()> {
    match command {
        COMMAND_OPEN_LOCAL | COMMAND_STATUS => open_local_configuration(),
        COMMAND_PAIR => open_pair_configuration(),
        COMMAND_SERVICE => launch_elevated_service(service_action),
        COMMAND_EXIT => {
            let confirmation = message_box(
                "将停止本次开机中的 host-monitor 服务并彻底退出托盘。\n\n服务的 Automatic 启动类型不会改变；下次启动 Windows 时 Client 仍会自动运行。",
                "停止本次服务并退出？",
                MB_OKCANCEL | MB_ICONWARNING,
            );
            if confirmation == IDOK.0 {
                request_stop_service_and_exit(window)
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

fn append_menu(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: usize,
    label: &str,
) -> anyhow::Result<()> {
    let label = wide(label);
    unsafe { AppendMenuW(menu, MF_STRING, id, PCWSTR(label.as_ptr())) }
        .context("failed to add a tray menu item")
}

struct MenuHandle(windows::Win32::UI::WindowsAndMessaging::HMENU);

impl Drop for MenuHandle {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

fn open_local_configuration() -> anyhow::Result<()> {
    LOCAL_SERVER
        .get()
        .context("local configuration server is unavailable")?
        .open_configuration()
}

fn open_pair_configuration() -> anyhow::Result<()> {
    LOCAL_SERVER
        .get()
        .context("local configuration server is unavailable")?
        .open_configuration_at("pair")
}

fn launch_elevated_pair(
    server: &str,
    callback_nonce: String,
) -> anyhow::Result<KernelHandle> {
    let mut arguments = vec![
        "--elevated-pair".to_string(),
        "--server-b64".to_string(),
        encode_base64url(server.as_bytes()),
    ];
    arguments.extend(["--callback-nonce".into(), callback_nonce]);
    launch_elevated_process(&arguments, true)?
        .context("Windows did not return the elevated pairing process handle")
}

fn launch_elevated_service(action: ServiceAction) -> anyhow::Result<()> {
    launch_elevated(&[
        "--elevated-service".into(),
        match action {
            ServiceAction::Start => "start".into(),
            ServiceAction::Stop => "stop".into(),
        },
    ])
}

fn request_stop_service_and_exit(window: HWND) -> anyhow::Result<()> {
    ensure!(
        EXIT_PENDING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "服务停止和退出操作已经在进行中"
    );
    let result = (|| {
        let process = TransferHandle::new(
            launch_elevated_process(&["--elevated-stop-for-exit".into()], true)?
                .context("Windows did not return the elevated service process handle")?,
        );
        let window_raw = window.0 as isize;
        thread::Builder::new()
            .name("host-monitoring-stop-and-exit".into())
            .spawn(move || {
                let result = wait_for_elevated_service_stop(process);
                match result {
                    Ok(()) => {
                        let window = HWND(window_raw as *mut c_void);
                        if unsafe {
                            PostMessageW(
                                Some(window),
                                EXIT_SERVICE_STOPPED_MESSAGE,
                                WPARAM(0),
                                LPARAM(0),
                            )
                        }
                        .is_err()
                        {
                            EXIT_PENDING.store(false, Ordering::Release);
                            show_error("Client 服务已停止，但无法通知托盘退出；托盘将继续运行。");
                        }
                    }
                    Err(error) => {
                        EXIT_PENDING.store(false, Ordering::Release);
                        show_error(&format!(
                            "Client 服务未能停止，托盘将继续运行：\n\n{error:#}"
                        ));
                    }
                }
            })
            .context("failed to start the service-stop waiter")?;
        Ok(())
    })();
    if result.is_err() {
        EXIT_PENDING.store(false, Ordering::Release);
    }
    result
}

fn wait_for_elevated_service_stop(process: TransferHandle) -> anyhow::Result<()> {
    wait_for_elevated_service_action(process, ServiceAction::Stop)
}

fn wait_for_elevated_service_action(
    process: TransferHandle,
    action: ServiceAction,
) -> anyhow::Result<()> {
    let process = process.into_kernel();
    let wait = unsafe { WaitForSingleObject(process.0, 60_000) };
    ensure!(
        wait == WAIT_OBJECT_0,
        "timed out or failed while waiting for the elevated stop operation ({wait:?})"
    );
    let mut exit_code = u32::MAX;
    unsafe { GetExitCodeProcess(process.0, &mut exit_code) }
        .context("failed to inspect the elevated stop operation")?;
    ensure!(
        exit_code == 0,
        "elevated stop operation failed with exit code {exit_code}"
    );
    let expected = match action {
        ServiceAction::Start => ServiceState::Running,
        ServiceAction::Stop => ServiceState::Stopped,
    };
    ensure!(
        query_service_state()? == expected,
        "elevated service operation exited without reaching {}",
        expected.label()
    );
    Ok(())
}

fn launch_elevated(arguments: &[String]) -> anyhow::Result<()> {
    launch_elevated_process(arguments, false).map(|_| ())
}

fn launch_elevated_process(
    arguments: &[String],
    retain_process: bool,
) -> anyhow::Result<Option<KernelHandle>> {
    let executable = installed_program_root()?.join(TRAY_EXE);
    ensure_fixed_regular_file(&executable, "installed tray executable")?;
    let executable_wide = wide_os(executable.as_os_str());
    let parameters = arguments
        .iter()
        .map(|argument| quote_windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters = wide(&parameters);
    let directory = executable
        .parent()
        .context("installed tray executable has no parent")?;
    let directory = wide_os(directory.as_os_str());
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI
            | if retain_process {
                SEE_MASK_NOCLOSEPROCESS
            } else {
                0
            },
        lpVerb: w!("runas"),
        lpFile: PCWSTR(executable_wide.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: PCWSTR(directory.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut execute) }
        .context("Windows elevation was cancelled or could not be started")?;
    if retain_process {
        ensure!(
            !execute.hProcess.is_invalid(),
            "Windows did not provide an elevated process handle"
        );
        Ok(Some(KernelHandle(execute.hProcess)))
    } else {
        Ok(None)
    }
}

fn open_browser(value: &str) -> anyhow::Result<()> {
    let value = validate_browser_url(value)?;
    let value = wide(&value);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(value.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    ensure!(
        result.0 as isize > 32,
        "Windows could not open the default browser (ShellExecute code {})",
        result.0 as isize
    );
    Ok(())
}

fn message_box(message: &str, title: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let message = wide(message);
    let title = wide(title);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
        )
    }
    .0
}
