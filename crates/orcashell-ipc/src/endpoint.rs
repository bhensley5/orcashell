use std::io;
use std::path::{Path, PathBuf};

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
    /// Optional per-daemon command capability token file.
    capability_path: Option<PathBuf>,
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
            capability_path: None,
        }
    }

    /// Create an endpoint that also has a command capability token path.
    pub fn new_with_capability(
        display_name: impl Into<String>,
        address: impl Into<String>,
        capability_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            display_name: display_name.into(),
            address: address.into(),
            capability_path: Some(capability_path.into()),
        }
    }

    pub fn capability_path(&self) -> Option<&Path> {
        self.capability_path.as_deref()
    }

    pub fn read_capability_token(&self) -> io::Result<Option<String>> {
        let Some(path) = &self.capability_path else {
            return Ok(None);
        };
        let token = std::fs::read_to_string(path)?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("empty IPC capability token at {}", path.display()),
            ));
        }
        Ok(Some(token))
    }
}

/// Returns the default IPC endpoint for the current user.
///
/// On Unix this is a socket inside a private current-user runtime directory.
/// On Windows this is `\\.\pipe\orcashell-<current-logon-SID>`.
#[cfg(unix)]
pub fn default_endpoint() -> io::Result<IpcEndpoint> {
    let dir = crate::unix::default_runtime_dir()?;
    let path = dir.join("daemon.sock");
    let token_path = dir.join("capability.token");
    let path = path.to_string_lossy().into_owned();
    Ok(IpcEndpoint {
        display_name: path.clone(),
        address: path,
        capability_path: Some(token_path),
    })
}

#[cfg(windows)]
pub fn default_endpoint() -> io::Result<IpcEndpoint> {
    let sid = crate::windows::get_current_logon_sid()?;
    let pipe = format!(r"\\.\pipe\orcashell-{sid}");
    let token_path = dirs::data_local_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no local data directory found"))?
        .join("orcashell")
        .join("ipc")
        .join(format!("{sid}.token"));
    Ok(IpcEndpoint {
        display_name: pipe.clone(),
        address: pipe,
        capability_path: Some(token_path),
    })
}
