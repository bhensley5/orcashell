use crate::IpcEndpoint;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(5);
const PRIVATE_DIR_MODE: u32 = 0o700;
const SOCKET_FILE_MODE: u32 = 0o600;

pub struct IpcListener {
    inner: UnixListener,
    socket_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<u32>,
}

pub(crate) fn default_runtime_dir() -> io::Result<PathBuf> {
    let uid = current_uid();
    let dir = runtime_dir_path(uid, dirs::runtime_dir(), &std::env::temp_dir());
    ensure_private_dir(&dir)?;
    Ok(dir)
}

fn runtime_dir_path(uid: u32, platform_runtime_dir: Option<PathBuf>, temp_dir: &Path) -> PathBuf {
    if cfg!(target_os = "linux") {
        match platform_runtime_dir {
            Some(dir) => dir.join("orcashell"),
            None => temp_dir.join(format!("orcashell-ipc-{uid}")),
        }
    } else {
        temp_dir.join(format!("orcashell-ipc-{uid}"))
    }
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => {
            validate_private_dir_identity(dir, &metadata, current_uid())?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(PRIVATE_DIR_MODE)
                .create(dir)?;
        }
        Err(e) => return Err(e),
    }

    let metadata = std::fs::symlink_metadata(dir)?;
    validate_private_dir_identity(dir, &metadata, current_uid())?;
    if metadata.permissions().mode() & 0o777 != PRIVATE_DIR_MODE {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    }
    let metadata = std::fs::symlink_metadata(dir)?;
    validate_private_dir_identity(dir, &metadata, current_uid())?;
    if metadata.permissions().mode() & 0o777 != PRIVATE_DIR_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("IPC runtime path is not private: {}", dir.display()),
        ));
    }
    Ok(())
}

fn validate_private_dir_identity(
    dir: &Path,
    metadata: &std::fs::Metadata,
    expected_uid: u32,
) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("IPC runtime path is a symlink: {}", dir.display()),
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("IPC runtime path is not a directory: {}", dir.display()),
        ));
    }
    if metadata.uid() != expected_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "IPC runtime path is not owned by current user: {}",
                dir.display()
            ),
        ));
    }
    Ok(())
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

        if let Some(metadata) = existing_endpoint_metadata(path)? {
            if metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("refusing IPC socket symlink: {}", path.display()),
                ));
            }
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
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_FILE_MODE))?;

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
        let peer = peer_identity(&stream)?;
        if peer.uid != current_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("rejected IPC peer with uid {}", peer.uid),
            ));
        }
        stream.set_read_timeout(Some(DEFAULT_STREAM_TIMEOUT))?;
        stream.set_write_timeout(Some(DEFAULT_STREAM_TIMEOUT))?;
        Ok(IpcStream {
            inner: stream,
            peer: Some(peer),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn private_runtime_dir_rejects_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("runtime-link");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        let err = ensure_private_dir(&link).expect_err("runtime dir symlink should be rejected");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn private_runtime_dir_mode_is_corrected_to_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("runtime");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_dir(&dir).unwrap();

        let metadata = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, PRIVATE_DIR_MODE);
        assert_eq!(metadata.uid(), current_uid());
    }

    #[test]
    fn private_runtime_dir_rejects_wrong_owner_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("runtime");
        std::fs::create_dir(&dir).unwrap();

        let metadata = std::fs::symlink_metadata(&dir).unwrap();
        let wrong_uid = current_uid().wrapping_add(1);
        let err = validate_private_dir_identity(&dir, &metadata, wrong_uid)
            .expect_err("mismatched owner should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_runtime_dir_fallback_uses_single_private_temp_dir() {
        let path = runtime_dir_path(1234, None, Path::new("/tmp"));
        assert_eq!(path, Path::new("/tmp").join("orcashell-ipc-1234"));
    }
}

pub struct IpcStream {
    inner: UnixStream,
    peer: Option<PeerIdentity>,
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
        Ok(Self {
            inner: stream,
            peer: None,
        })
    }

    pub fn peer_identity(&self) -> Option<PeerIdentity> {
        self.peer
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
            matches!(e.raw_os_error(), Some(libc::ENOTSOCK))
        }
    }
}

fn existing_endpoint_metadata(path: &Path) -> io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn current_uid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}

#[cfg(target_os = "linux")]
fn peer_identity(stream: &UnixStream) -> io::Result<PeerIdentity> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerIdentity {
        uid: cred.uid,
        gid: cred.gid,
        pid: Some(cred.pid as u32),
    })
}

#[cfg(not(target_os = "linux"))]
fn peer_identity(stream: &UnixStream) -> io::Result<PeerIdentity> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerIdentity {
        uid,
        gid,
        pid: None,
    })
}
