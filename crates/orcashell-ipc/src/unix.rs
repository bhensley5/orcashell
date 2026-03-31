use crate::IpcEndpoint;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

pub struct IpcListener {
    inner: UnixListener,
    socket_path: PathBuf,
}

impl IpcListener {
    /// Bind to the given endpoint and begin listening.
    ///
    /// If the socket file already exists, a probe-connect determines whether a live
    /// daemon occupies the endpoint. If so, returns `AddrInUse`. If the socket is
    /// clearly stale (`ConnectionRefused` or `NotFound`), the file is removed and
    /// binding proceeds. Ambiguous errors (e.g. `PermissionDenied`) are propagated
    /// without touching the file.
    pub fn bind(endpoint: &IpcEndpoint) -> io::Result<Self> {
        let path = Path::new(&endpoint.address);

        if path.exists() {
            match UnixStream::connect(path) {
                Ok(_stream) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("daemon already running at {}", endpoint.display_name),
                    ));
                }
                Err(e) if is_clearly_stale(&e) => {
                    tracing::info!(?path, "removing stale socket file");
                    std::fs::remove_file(path)?;
                }
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!(
                            "endpoint busy or unavailable at {}: {e}",
                            endpoint.display_name
                        ),
                    ));
                }
            }
        }

        let listener = UnixListener::bind(path)?;

        Ok(Self {
            inner: listener,
            socket_path: path.to_path_buf(),
        })
    }

    /// Accept a connection. Returns an `IpcStream` configured with 5-second
    /// read/write timeouts.
    ///
    /// When the listener is in non-blocking mode, returns `WouldBlock` immediately
    /// if no client is waiting.
    pub fn accept(&mut self) -> io::Result<IpcStream> {
        let (stream, _addr) = self.inner.accept()?;
        stream.set_read_timeout(Some(DEFAULT_STREAM_TIMEOUT))?;
        stream.set_write_timeout(Some(DEFAULT_STREAM_TIMEOUT))?;
        Ok(IpcStream { inner: stream })
    }

    /// Set non-blocking mode on the listener.
    ///
    /// When non-blocking, `accept()` returns `WouldBlock` immediately if no client
    /// is waiting, allowing a poll loop with a shutdown flag.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub struct IpcStream {
    inner: UnixStream,
}

impl IpcStream {
    /// Connect to a listening endpoint with the given timeout.
    ///
    /// The timeout is applied as read and write deadlines on the resulting stream.
    pub fn connect(endpoint: &IpcEndpoint, timeout: Duration) -> io::Result<Self> {
        let path = Path::new(&endpoint.address);
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self { inner: stream })
    }

    /// Set the read timeout for subsequent read operations.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }

    /// Set the write timeout for subsequent write operations.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(timeout)
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Returns true if the connect error clearly indicates the endpoint is stale
/// (no live daemon), making it safe to remove the socket file.
///
/// `ConnectionRefused` and `NotFound` are standard stale indicators.
/// ENOTSOCK (raw error 38 on macOS, 88 on Linux) means the file exists but
/// is not a socket - e.g. a leftover regular file from a crash - also clearly
/// stale.
fn is_clearly_stale(e: &io::Error) -> bool {
    match e.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => true,
        _ => {
            // ENOTSOCK: "Socket operation on non-socket"
            // 38 on macOS/BSD, 88 on Linux.
            #[cfg(target_os = "macos")]
            const ENOTSOCK: i32 = 38;
            #[cfg(target_os = "linux")]
            const ENOTSOCK: i32 = 88;
            matches!(e.raw_os_error(), Some(ENOTSOCK))
        }
    }
}
