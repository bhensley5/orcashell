use std::io;

/// A platform-agnostic IPC endpoint descriptor.
///
/// On Unix this wraps a filesystem path to a Unix domain socket.
/// On Windows this wraps a named-pipe path like `\\.\pipe\orcashell-<sid>`.
#[derive(Clone, Debug)]
pub struct IpcEndpoint {
    /// Human-readable name for display and logging.
    pub display_name: String,
    /// Platform-specific address used internally by bind/connect.
    pub(crate) address: String,
}

impl IpcEndpoint {
    /// Create an endpoint with an explicit display name and address.
    ///
    /// On Unix the address is a socket file path.
    /// On Windows the address is a named-pipe path.
    pub fn new(display_name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            address: address.into(),
        }
    }
}

/// Returns the default IPC endpoint for the current user.
///
/// On Unix this is `/tmp/orcashell.sock`.
/// On Windows this is `\\.\pipe\orcashell-<current-user-SID>`.
#[cfg(unix)]
pub fn default_endpoint() -> io::Result<IpcEndpoint> {
    let path = "/tmp/orcashell.sock";
    Ok(IpcEndpoint {
        display_name: path.to_string(),
        address: path.to_string(),
    })
}

#[cfg(windows)]
pub fn default_endpoint() -> io::Result<IpcEndpoint> {
    let sid = crate::windows::get_current_user_sid()?;
    let pipe = format!(r"\\.\pipe\orcashell-{sid}");
    Ok(IpcEndpoint {
        display_name: pipe.clone(),
        address: pipe,
    })
}
