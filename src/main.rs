mod cli;
mod cli_common;
mod monitor_app;
fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    if host_monitor::service::windows_service_requested(std::env::args_os()) {
        return match monitor_app::entry() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(_) => std::process::ExitCode::from(8),
        };
    }
    std::process::ExitCode::from(cli::entry(std::env::args().skip(1).collect()))
}
