#[cfg(not(windows))]
pub(crate) fn entry() {
    eprintln!("host-monitor-tray is available only on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
pub(crate) fn entry() {
    if let Err(error) = windows_tray::entry() {
        windows_tray::show_error(&format!("host-monitor tray failed:\n\n{error:#}"));
        std::process::exit(1);
    }
}

/// Reconcile the Client child's exit with the independent NDJSON reader.
///
/// When the child fails before its first event, the reader necessarily reaches
/// EOF and reports that no authorization key was requested. That is useful only
/// when the child produced no diagnostics; otherwise it hides the real failure.
#[cfg(any(windows, test))]
#[derive(Debug)]
struct MissingAuthorizationKeyEvent;

#[cfg(any(windows, test))]
impl std::fmt::Display for MissingAuthorizationKeyEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Client did not request an authorization key")
    }
}

#[cfg(any(windows, test))]
impl std::error::Error for MissingAuthorizationKeyEvent {}

#[cfg(any(windows, test))]
const MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES: usize = 768;

#[cfg(any(windows, test))]
fn bounded_pairing_warning_detail(mut detail: String) -> String {
    if detail.len() <= MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES {
        return detail;
    }
    let ellipsis = "…";
    let mut boundary = MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES - ellipsis.len();
    while !detail.is_char_boundary(boundary) {
        boundary -= 1;
    }
    detail.truncate(boundary);
    detail.push_str(ellipsis);
    detail
}

#[cfg(any(windows, test))]
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PairingPostCommitEventWarning {
    detail: String,
}

#[cfg(any(windows, test))]
impl std::fmt::Display for PairingPostCommitEventWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "配对已成功并写入新凭据，但托盘未能完整接收完成事件：{}。\
             请单独检查 Client 状态；不要重新配对。",
            self.detail
        )
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PairingChildReconciliation {
    post_commit_event_warning: Option<PairingPostCommitEventWarning>,
}

#[cfg(any(windows, test))]
fn validate_pairing_child_reconciliation(
    reconciliation: &PairingChildReconciliation,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        reconciliation
            .post_commit_event_warning
            .as_ref()
            .is_none_or(|warning| warning.detail.len() <= MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES),
        "pairing completion warning exceeds its size limit"
    );
    Ok(())
}

#[cfg(any(windows, test))]
fn reconcile_pairing_child(
    child_succeeded: bool,
    child_status: &str,
    event_result: anyhow::Result<()>,
    diagnostics: &str,
) -> anyhow::Result<PairingChildReconciliation> {
    if child_succeeded {
        return match event_result {
            Ok(()) => Ok(PairingChildReconciliation::default()),
            Err(error)
                if error
                    .downcast_ref::<MissingAuthorizationKeyEvent>()
                    .is_some() =>
            {
                Err(anyhow::anyhow!(
                    "pairing event processing failed before commit: {error:#}"
                ))
            }
            Err(error) => {
                let detail = if diagnostics.trim().is_empty() {
                    format!("{error:#}")
                } else {
                    format!("{error:#}; Client diagnostics: {}", diagnostics.trim())
                };
                Ok(PairingChildReconciliation {
                    post_commit_event_warning: Some(PairingPostCommitEventWarning {
                        detail: bounded_pairing_warning_detail(detail),
                    }),
                })
            }
        };
    }

    match event_result {
        Ok(()) if diagnostics.trim().is_empty() => {
            anyhow::bail!("browser pairing exited with {child_status}")
        }
        Ok(()) => anyhow::bail!(
            "Client pairing failed ({child_status}): {}",
            diagnostics.trim()
        ),
        Err(error)
            if error
                .downcast_ref::<MissingAuthorizationKeyEvent>()
                .is_some()
                && !diagnostics.trim().is_empty() =>
        {
            anyhow::bail!(
                "Client pairing failed ({child_status}): {}",
                diagnostics.trim()
            )
        }
        Err(error) if diagnostics.trim().is_empty() => {
            anyhow::bail!("browser pairing exited with {child_status}: {error:#}")
        }
        Err(error) => {
            anyhow::bail!(
                "pairing event processing failed: {error:#}; Client pairing also failed ({child_status}): {}",
                diagnostics.trim()
            )
        }
    }
}

/// Once the signed Client child exits successfully, its credential, Active
/// journal, and durable configuration are already committed. A subsequent
/// service reload failure is actionable, but must never turn that irreversible
/// success into a request to pair again.
#[cfg(any(windows, test))]
fn committed_pairing_restart_warning(
    service_was_running: bool,
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> Option<String> {
    if !service_was_running {
        return None;
    }
    restart().err().map(|error| {
        format!(
            "配对已成功并写入新凭据，但 Client 服务未能重新加载它们：{error:#}\n\n\
             当前运行的服务可能仍在使用旧凭据，或服务可能已经停止。请在配置窗口中单独启动或重启服务；不要重新配对。"
        )
    })
}

/// The elevated broker's success is the pairing commit boundary. Failure to
/// persist a standard-user tray convenience preference after that boundary is
/// a warning, not permission to create another host by pairing again.
#[cfg(any(windows, test))]
fn committed_pairing_preferences_warning(save: anyhow::Result<()>) -> Option<String> {
    save.err().map(|error| {
        format!(
            "配对已成功，但托盘偏好未能保存：{error:#}。当前 Client 凭据和 Server 主机不受影响；\
             修复当前用户的本地应用数据写入权限后重新填写 Server 地址，不要重新配对。"
        )
    })
}

#[cfg(any(windows, test))]
const MAX_TRAY_PREFERENCES_BYTES: usize = 16 * 1024;

#[cfg(any(windows, test))]
fn read_bounded_tray_preferences_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_TRAY_PREFERENCES_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod cross_platform_tests {
    use super::{
        MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES, MAX_TRAY_PREFERENCES_BYTES,
        MissingAuthorizationKeyEvent, PairingChildReconciliation, PairingPostCommitEventWarning,
        bounded_pairing_warning_detail, committed_pairing_preferences_warning,
        committed_pairing_restart_warning, read_bounded_tray_preferences_file,
        reconcile_pairing_child, validate_pairing_child_reconciliation,
    };

    #[test]
    fn child_diagnostics_replace_the_secondary_missing_key_error() {
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(MissingAuthorizationKeyEvent.into()),
            "Error: Host Monitoring returned a non-JSON pairing response",
        )
        .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("Host Monitoring returned a non-JSON pairing response"));
        assert!(!rendered.contains("Client did not request an authorization key"));
    }

    #[test]
    fn missing_key_error_is_retained_when_the_child_has_no_diagnostics() {
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(MissingAuthorizationKeyEvent.into()),
            "",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Client did not request an authorization key"));
    }

    #[test]
    fn child_stderr_does_not_hide_a_real_event_protocol_error() {
        let event_message = "Client activation URL origin differs from the confirmed server";
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(anyhow::anyhow!(event_message)),
            "Error: pairing child was cancelled after the event-stream failure",
        )
        .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.starts_with("pairing event processing failed"));
        assert!(rendered.contains(event_message));
        assert!(rendered.contains("pairing child was cancelled"));
    }

    #[test]
    fn successful_child_turns_a_post_commit_event_error_into_a_typed_warning() {
        let reconciled = reconcile_pairing_child(
            true,
            "exit code 0",
            Err(anyhow::anyhow!("simulated paired-event flush failure")),
            "Warning: paired event could not be flushed to stdout",
        )
        .expect("a successful child exit is the durable commit boundary");
        let encoded = serde_json::to_vec(&reconciled).expect("warning IPC must serialize");
        let reconciled: PairingChildReconciliation =
            serde_json::from_slice(&encoded).expect("warning IPC must deserialize");
        validate_pairing_child_reconciliation(&reconciled)
            .expect("the serialized warning must satisfy the parent bound");
        let warning = reconciled
            .post_commit_event_warning
            .expect("the event error must remain visible as a typed warning");
        let rendered = warning.to_string();
        assert!(rendered.contains("配对已成功并写入新凭据"));
        assert!(rendered.contains("simulated paired-event flush failure"));
        assert!(rendered.contains("could not be flushed to stdout"));
        assert!(rendered.contains("不要重新配对"));
    }

    #[test]
    fn failed_child_keeps_an_event_error_fatal() {
        let error = reconcile_pairing_child(
            false,
            "exit code 1",
            Err(anyhow::anyhow!("simulated pre-commit event failure")),
            "child stopped before durable commit",
        )
        .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("simulated pre-commit event failure"));
        assert!(rendered.contains("child stopped before durable commit"));
    }

    #[test]
    fn missing_authorization_event_remains_pre_commit_even_on_success_exit() {
        let error = reconcile_pairing_child(
            true,
            "exit code 0",
            Err(MissingAuthorizationKeyEvent.into()),
            "",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("failed before commit"));
    }

    #[test]
    fn post_commit_event_warning_detail_is_utf8_safe_and_bounded() {
        let detail = bounded_pairing_warning_detail("配对事件错误".repeat(200));
        assert!(detail.len() <= MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES);
        assert!(detail.ends_with('…'));
        assert!(std::str::from_utf8(detail.as_bytes()).is_ok());
    }

    #[test]
    fn parent_rejects_an_oversized_post_commit_event_warning() {
        let reconciliation = PairingChildReconciliation {
            post_commit_event_warning: Some(PairingPostCommitEventWarning {
                detail: "x".repeat(MAX_PAIRING_EVENT_WARNING_DETAIL_BYTES + 1),
            }),
        };
        assert!(validate_pairing_child_reconciliation(&reconciliation).is_err());
    }

    #[test]
    fn committed_pairing_is_not_failed_by_service_reload_cleanup() {
        let skipped = committed_pairing_restart_warning(false, || {
            panic!("a previously stopped service must remain stopped")
        });
        assert!(skipped.is_none());

        let restarted = committed_pairing_restart_warning(true, || Ok(()));
        assert!(restarted.is_none());

        let warning = committed_pairing_restart_warning(true, || {
            Err(anyhow::anyhow!("simulated service restart failure"))
        })
        .expect("a post-commit restart failure must become a warning");
        assert!(warning.contains("配对已成功并写入新凭据"));
        assert!(warning.contains("simulated service restart failure"));
        assert!(warning.contains("不要重新配对"));
    }

    #[test]
    fn committed_pairing_is_not_failed_by_tray_preference_cleanup() {
        assert!(committed_pairing_preferences_warning(Ok(())).is_none());

        let warning = committed_pairing_preferences_warning(Err(anyhow::anyhow!(
            "simulated LocalAppData write failure"
        )))
        .expect("a post-commit preference failure must become a warning");
        assert!(warning.contains("配对已成功"));
        assert!(warning.contains("simulated LocalAppData write failure"));
        assert!(warning.contains("不要重新配对"));
    }

    #[test]
    fn tray_preferences_reader_stops_after_the_size_limit_sentinel() {
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("physical test temporary directory")
            .join(format!(
                "host-monitoring-tray-preferences-bounds-{}.json",
                uuid::Uuid::new_v4()
            ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((MAX_TRAY_PREFERENCES_BYTES + 8 * 1024) as u64)
            .unwrap();
        drop(file);

        let bytes = read_bounded_tray_preferences_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(bytes.len(), MAX_TRAY_PREFERENCES_BYTES + 1);
    }
}

#[cfg(windows)]
mod windows_tray {
    use super::{PairingChildReconciliation, validate_pairing_child_reconciliation};
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString, c_void},
        fs,
        io::{BufRead, BufReader, Read, Write},
        mem::size_of,
        net::{TcpListener, TcpStream},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail, ensure};
    use host_monitor::{
        service::WINDOWS_SERVICE_NAME,
        tray_support::{
            MAX_LOCAL_HTTP_BODY_BYTES, MAX_LOCAL_HTTP_HEAD_BYTES, ServiceAction, TrayCommand,
            advance_pipe_transfer, browser_url_matches_server_origin, constant_time_eq,
            deadline_wait_millis, encode_base64url, parse_tray_arguments, quote_windows_argument,
            server_health::probe_server_connection, validate_activation_code, validate_browser_url,
            validate_server_base,
        },
    };
    use serde::{Deserialize, Serialize};
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
                ERROR_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
                GENERIC_WRITE, GetLastError, HANDLE, HLOCAL, HWND, LPARAM, LRESULT, LocalFree,
                POINT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
            },
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                },
                GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION,
                TOKEN_QUERY, TokenElevation,
            },
            Storage::FileSystem::{
                CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE,
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
                PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
                WriteFile,
            },
            System::{
                Com::CoTaskMemFree,
                IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
                LibraryLoader::GetModuleHandleW,
                Pipes::{
                    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
                    PIPE_REJECT_REMOTE_CLIENTS, PIPE_WAIT, WaitNamedPipeW,
                },
                Recovery::RegisterApplicationRestart,
                Services::{
                    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW,
                    QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
                    SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
                    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
                    SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
                },
                SystemInformation::GetWindowsDirectoryW,
                Threading::{
                    CreateEventW, CreateMutexW, GetCurrentProcess, GetExitCodeProcess,
                    GetProcessId, OpenProcessToken, SetEvent, WaitForMultipleObjects,
                    WaitForSingleObject,
                },
            },
            UI::{
                Shell::{
                    FOLDERID_LocalAppData, FOLDERID_ProgramData, FOLDERID_ProgramFiles,
                    KF_FLAG_DEFAULT, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                    NIM_MODIFY, NOTIFYICONDATAW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS,
                    SHELLEXECUTEINFOW, SHGetKnownFolderPath, Shell_NotifyIconW, ShellExecuteExW,
                    ShellExecuteW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON,
                    IDCANCEL, IDI_APPLICATION, IDOK, LoadIconW, MB_ICONERROR, MB_ICONINFORMATION,
                    MB_ICONWARNING, MB_OK, MB_OKCANCEL, MENU_ITEM_FLAGS, MESSAGEBOX_STYLE,
                    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
                    RegisterWindowMessageW, SW_HIDE, SW_SHOWNORMAL, SetForegroundWindow,
                    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY,
                    WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
                },
            },
        },
        core::{PCWSTR, w},
    };

    const ICON_ID: u32 = 1;
    const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 41;
    const OPEN_CONFIGURATION_MESSAGE: u32 = WM_APP + 42;
    const EXIT_SERVICE_STOPPED_MESSAGE: u32 = WM_APP + 43;
    const REFRESH_TRAY_STATUS_MESSAGE: u32 = WM_APP + 44;
    const WINDOW_CLASS_NAME: &str = "HostMonitorTrayMessageWindow";
    const COMMAND_OPEN_LOCAL: usize = 1001;
    const COMMAND_PAIR: usize = 1003;
    const COMMAND_STATUS: usize = 1004;
    const COMMAND_SERVICE: usize = 1005;
    const COMMAND_EXIT: usize = 1099;
    const CONFIG_DIRECTORY: &str = "host-monitor";
    const CONFIG_FILE: &str = "config.json";
    const CLIENT_EXE: &str = "host-monitor.exe";
    const TRAY_EXE: &str = "host-monitor-tray.exe";
    const PRIVILEGED_OPERATION_MUTEX: &str =
        "Global\\HostMonitorOperation-875F84C4-E2A4-4542-8DBF-C474E04826C4";
    const MAX_HTTP_CONNECTIONS: usize = 16;
    const MAX_NDJSON_LINE_BYTES: usize = 16 * 1024;
    const MAX_NDJSON_TOTAL_BYTES: usize = 128 * 1024;
    const MAX_CHILD_STDERR_BYTES: usize = 64 * 1024;
    const PAIR_OPERATION_TIMEOUT: Duration = Duration::from_secs(20 * 60);
    const PAIR_BROKER_EXIT_GRACE: Duration = Duration::from_secs(2 * 60);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    static WINDOW_HANDLE: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED_MESSAGE: AtomicU32 = AtomicU32::new(0);
    static EXIT_PENDING: AtomicBool = AtomicBool::new(false);
    static LOCAL_SERVER: OnceLock<LocalControlServer> = OnceLock::new();

    // These files are same-module fragments rather than public submodules. The
    // tray workflows share security-sensitive state, so the split improves
    // reviewability without widening visibility or changing synchronization.
    include!("control_state.rs");
    include!("control_server.rs");
    include!("control_routes.rs");
    include!("control_response.rs");

    include!("configuration_ui.rs");

    include!("tray_shell.rs");

    include!("pairing_ipc.rs");

    include!("win32.rs");

    include!("preferences.rs");

    include!("tests.rs");
}
