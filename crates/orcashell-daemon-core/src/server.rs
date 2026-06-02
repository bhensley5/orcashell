use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::handler::handle_connection;
use crate::open_project::enqueue_validated_open_project;
pub use crate::open_project::OpenProjectEnqueueError;
use orcashell_ipc::{IpcEndpoint, IpcListener};
use orcashell_protocol::messages::OpenDisposition;

pub const OPEN_PROJECT_QUEUE_CAPACITY: usize = 64;
#[cfg(windows)]
const MAX_CONNECTION_HANDLERS: usize = (orcashell_ipc::PIPE_MAX_INSTANCES as usize) / 2;
#[cfg(not(windows))]
const MAX_CONNECTION_HANDLERS: usize = 16;
const CAPABILITY_TOKEN_BYTES: usize = 32;

pub struct DaemonServer {
    endpoint: IpcEndpoint,
    shutdown: Arc<AtomicBool>,
    listener_handle: Option<JoinHandle<()>>,
    open_project_tx: async_channel::Sender<(PathBuf, OpenDisposition)>,
    open_project_rx: async_channel::Receiver<(PathBuf, OpenDisposition)>,
    capability_path: Option<PathBuf>,
}

impl DaemonServer {
    pub fn start(endpoint: &IpcEndpoint) -> io::Result<Self> {
        let listener = IpcListener::bind(endpoint)?;
        listener.set_nonblocking(true)?;
        let capability = match endpoint.capability_path() {
            Some(path) => Some(write_capability_token(path)?),
            None => None,
        };
        let capability_path = endpoint.capability_path().map(Path::to_path_buf);

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let endpoint_name = endpoint.display_name.clone();

        let (open_project_tx, open_project_rx) =
            async_channel::bounded(OPEN_PROJECT_QUEUE_CAPACITY);
        let tx_for_loop = open_project_tx.clone();
        let active_handlers = Arc::new(AtomicUsize::new(0));

        let handle = thread::Builder::new()
            .name("orca-daemon-listener".into())
            .spawn(move || {
                Self::accept_loop(
                    listener,
                    shutdown_clone,
                    &endpoint_name,
                    tx_for_loop,
                    capability,
                    active_handlers,
                );
            })?;

        info!(endpoint = %endpoint.display_name, "daemon server started");

        Ok(Self {
            endpoint: endpoint.clone(),
            shutdown,
            listener_handle: Some(handle),
            open_project_tx,
            open_project_rx,
            capability_path,
        })
    }

    fn accept_loop(
        mut listener: IpcListener,
        shutdown: Arc<AtomicBool>,
        endpoint_name: &str,
        open_tx: async_channel::Sender<(PathBuf, OpenDisposition)>,
        capability: Option<String>,
        active_handlers: Arc<AtomicUsize>,
    ) {
        loop {
            if shutdown.load(Ordering::Acquire) {
                break;
            }

            match listener.accept() {
                Ok(stream) => {
                    let Some(slot) = HandlerSlot::try_acquire(active_handlers.clone()) else {
                        warn!("dropping IPC connection: handler limit reached");
                        continue;
                    };
                    let name = endpoint_name.to_string();
                    let tx = open_tx.clone();
                    let required_capability = capability.clone();
                    if let Err(e) = thread::Builder::new()
                        .name("orca-daemon-conn".into())
                        .spawn(move || {
                            let _slot = slot;
                            if let Err(e) = handle_connection(
                                stream,
                                &name,
                                &tx,
                                required_capability.as_deref(),
                            ) {
                                warn!("connection handler error: {e}");
                            }
                        })
                    {
                        error!("failed to spawn connection handler thread: {e}");
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(ref e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    warn!("rejected IPC connection: {e}");
                }
                Err(e) => {
                    error!("accept error: {e}");
                    break;
                }
            }
        }
    }

    /// Returns a clone of the open-project receiver channel handle.
    ///
    /// **Note:** `async_channel` uses a multi-producer, multi-consumer model.  Multiple
    /// clones of the same receiver compete for the same messages (each message is delivered
    /// to exactly one receiver).  The app poll task should be the **sole** consumer; avoid
    /// calling this method more than once in production code.
    pub fn open_project_receiver(&self) -> async_channel::Receiver<(PathBuf, OpenDisposition)> {
        self.open_project_rx.clone()
    }

    /// Directly enqueue an open-project request without going through IPC.
    /// Used for cold-launch routing from CLI args.
    pub fn enqueue_open_project(
        &self,
        path: PathBuf,
        disposition: OpenDisposition,
    ) -> Result<(), OpenProjectEnqueueError> {
        enqueue_validated_open_project(&self.open_project_tx, path, disposition).map(|_| ())
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.listener_handle.take() {
            let _ = handle.join();
        }
        if let Some(path) = self.capability_path.take() {
            let _ = std::fs::remove_file(path);
        }
        info!("daemon server stopped");
    }

    pub fn endpoint(&self) -> &IpcEndpoint {
        &self.endpoint
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn windows_handler_limit_reserves_pipe_instance_headroom() {
        assert!(orcashell_ipc::PIPE_MAX_INSTANCES as usize > super::MAX_CONNECTION_HANDLERS);
    }
}

struct HandlerSlot {
    active: Arc<AtomicUsize>,
}

impl HandlerSlot {
    fn try_acquire(active: Arc<AtomicUsize>) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_CONNECTION_HANDLERS).then_some(count + 1)
            })
            .ok()?;
        Some(Self { active })
    }
}

impl Drop for HandlerSlot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn write_capability_token(path: &Path) -> io::Result<String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let rng = SystemRandom::new();
    let mut bytes = [0u8; CAPABILITY_TOKEN_BYTES];
    rng.fill(&mut bytes)
        .map_err(|_| io::Error::other("failed to generate IPC capability"))?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);

    let tmp_path = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp_path)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    drop(file);
    orcashell_platform::replace_file(&tmp_path, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(token)
}
