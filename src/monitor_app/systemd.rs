use std::{
    ffi::OsStr,
    os::{
        linux::net::SocketAddrExt,
        unix::{ffi::OsStrExt, net::UnixDatagram},
    },
    path::Path,
};

use anyhow::{Context, bail};

const READY_MESSAGE: &[u8] = b"READY=1";

pub(super) fn report_ready() -> anyhow::Result<bool> {
    let Some(notify_socket) = std::env::var_os("NOTIFY_SOCKET") else {
        // Ordinary foreground runs are not supervised by systemd.
        return Ok(true);
    };
    send_ready(&notify_socket)
}

fn send_ready(notify_socket: &OsStr) -> anyhow::Result<bool> {
    let socket_bytes = notify_socket.as_bytes();
    let address = if let Some(abstract_name) = socket_bytes.strip_prefix(b"@") {
        if abstract_name.is_empty() {
            bail!("systemd notification socket has an empty abstract name");
        }
        std::os::unix::net::SocketAddr::from_abstract_name(abstract_name)
            .context("invalid abstract systemd notification socket")?
    } else {
        let socket_path = Path::new(notify_socket);
        if !socket_path.is_absolute() {
            bail!("systemd notification socket path is not absolute");
        }
        std::os::unix::net::SocketAddr::from_pathname(socket_path)
            .context("invalid systemd notification socket path")?
    };

    let socket = UnixDatagram::unbound()
        .context("failed to create the systemd notification datagram socket")?;
    socket
        .connect_addr(&address)
        .context("failed to connect to the systemd notification socket")?;
    let written = socket
        .send(READY_MESSAGE)
        .context("failed to report readiness to systemd")?;
    if written != READY_MESSAGE.len() {
        bail!("systemd readiness notification was truncated");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        os::{linux::net::SocketAddrExt, unix::net::UnixDatagram},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{READY_MESSAGE, send_ready};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

    struct SocketFile(PathBuf);

    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn unique_name(prefix: &str) -> String {
        format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn reports_ready_to_filesystem_socket() {
        let socket_path = PathBuf::from("/tmp").join(unique_name("host-monitor-notify"));
        let cleanup = SocketFile(socket_path.clone());
        let receiver = UnixDatagram::bind(&socket_path).unwrap();

        assert!(send_ready(socket_path.as_os_str()).unwrap());
        let mut message = [0_u8; 32];
        let received = receiver.recv(&mut message).unwrap();

        assert_eq!(&message[..received], READY_MESSAGE);
        drop(cleanup);
    }

    #[test]
    fn reports_ready_to_abstract_socket() {
        let abstract_name = unique_name("host-monitor-notify");
        let address =
            std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
        let receiver = UnixDatagram::bind_addr(&address).unwrap();
        let notify_socket = format!("@{abstract_name}");

        assert!(send_ready(OsStr::new(&notify_socket)).unwrap());
        let mut message = [0_u8; 32];
        let received = receiver.recv(&mut message).unwrap();

        assert_eq!(&message[..received], READY_MESSAGE);
    }

    #[test]
    fn rejects_invalid_notification_addresses() {
        assert!(send_ready(OsStr::new("relative.sock")).is_err());
        assert!(send_ready(OsStr::new("@")).is_err());
    }

    #[test]
    fn reports_a_missing_notification_socket() {
        let socket_path = PathBuf::from("/tmp").join(unique_name("host-monitor-missing"));

        assert!(send_ready(socket_path.as_os_str()).is_err());
    }
}
