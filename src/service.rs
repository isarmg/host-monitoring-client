//! Process-lifetime service helpers shared by the platform service hosts.
//!
//! The cancellation primitive deliberately lives outside the Windows-only
//! module so its edge cases remain testable on every CI runner.

use std::{
    ffi::OsStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::watch;

/// Hidden flag used only by the Windows Service Control Manager image path.
pub const WINDOWS_SERVICE_ARGUMENT: &str = "--windows-service";

/// Internal SCM service name used by the current package and service host.
pub const WINDOWS_SERVICE_NAME: &str = "host-monitor";

/// Returns true only for an exact `--windows-service` argument.
///
/// Keeping this testable and independent of the config parser prevents a
/// value such as `--config=C:\\--windows-service` from accidentally switching
/// a normal console invocation into SCM dispatcher mode.
pub fn windows_service_requested<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .nth(1)
        .is_some_and(|argument| argument.as_ref() == OsStr::new(WINDOWS_SERVICE_ARGUMENT))
}

/// The control side of a process-wide graceful-shutdown signal.
#[derive(Clone, Debug)]
pub struct ShutdownController {
    requested: Arc<AtomicBool>,
    sender: watch::Sender<bool>,
}

/// The async side of a process-wide graceful-shutdown signal.
#[derive(Clone, Debug)]
pub struct ShutdownSignal {
    requested: Arc<AtomicBool>,
    receiver: watch::Receiver<bool>,
}

/// Creates a one-way, idempotent shutdown channel.
pub fn shutdown_channel() -> (ShutdownController, ShutdownSignal) {
    let requested = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = watch::channel(false);
    (
        ShutdownController {
            requested: Arc::clone(&requested),
            sender,
        },
        ShutdownSignal {
            requested,
            receiver,
        },
    )
}

impl ShutdownController {
    /// Requests shutdown. Returns true only for the first request.
    pub fn request_shutdown(&self) -> bool {
        if self.requested.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.sender.send_replace(true);
        true
    }
}

impl ShutdownSignal {
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// Resolves for both an explicit request and loss of the controller.
    pub async fn cancelled(&self) {
        if self.is_requested() {
            return;
        }
        let mut receiver = self.receiver.clone();
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn service_flag_detection_requires_an_exact_argument() {
        assert!(windows_service_requested([
            "host-monitor",
            WINDOWS_SERVICE_ARGUMENT,
            "run",
        ]));
        assert!(!windows_service_requested([
            "host-monitor",
            "run",
            WINDOWS_SERVICE_ARGUMENT,
        ]));
        assert!(!windows_service_requested([
            "host-monitor",
            "--config=C:\\--windows-service",
        ]));
        assert!(!windows_service_requested([
            "host-monitor",
            "--WINDOWS-SERVICE",
        ]));
    }

    #[tokio::test]
    async fn cancellation_wakes_existing_and_late_observers() {
        let (controller, signal) = shutdown_channel();
        let existing = tokio::spawn({
            let signal = signal.clone();
            async move { signal.cancelled().await }
        });

        assert!(controller.request_shutdown());
        assert!(!controller.request_shutdown(), "the request is idempotent");
        tokio::time::timeout(Duration::from_secs(1), existing)
            .await
            .expect("existing observer should wake")
            .expect("observer task should not panic");
        tokio::time::timeout(Duration::from_secs(1), signal.cancelled())
            .await
            .expect("observer created after cancellation should return immediately");
        assert!(signal.is_requested());
    }

    #[tokio::test]
    async fn dropping_the_controller_does_not_leave_waiters_hung() {
        let (controller, signal) = shutdown_channel();
        drop(controller);
        tokio::time::timeout(Duration::from_secs(1), signal.cancelled())
            .await
            .expect("a closed control channel should release waiters");
    }
}
