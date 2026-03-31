mod endpoint;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use endpoint::{default_endpoint, IpcEndpoint};
#[cfg(unix)]
pub use unix::{IpcListener, IpcStream};
#[cfg(windows)]
pub use windows::{IpcListener, IpcStream};
