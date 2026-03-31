use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::handler::handle_connection;
use orcashell_ipc::{IpcEndpoint, IpcListener};
use orcashell_protocol::messages::OpenDisposition;

pub struct DaemonServer {
    endpoint: IpcEndpoint,
    shutdown: Arc<AtomicBool>,
    listener_handle: Option<JoinHandle<()>>,
    open_project_tx: async_channel::Sender<(PathBuf, OpenDisposition)>,
    open_project_rx: async_channel::Receiver<(PathBuf, OpenDisposition)>,
}

impl DaemonServer {
    pub fn start(endpoint: &IpcEndpoint) -> io::Result<Self> {
        let listener = IpcListener::bind(endpoint)?;
        listener.set_nonblocking(true)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let endpoint_name = endpoint.display_name.clone();

        let (open_project_tx, open_project_rx) = async_channel::unbounded();
        let tx_for_loop = open_project_tx.clone();

        let handle = thread::Builder::new()
            .name("orca-daemon-listener".into())
            .spawn(move || {
                Self::accept_loop(listener, shutdown_clone, &endpoint_name, tx_for_loop);
            })?;

        info!(endpoint = %endpoint.display_name, "daemon server started");

        Ok(Self {
            endpoint: endpoint.clone(),
            shutdown,
            listener_handle: Some(handle),
            open_project_tx,
            open_project_rx,
        })
    }

    fn accept_loop(
        mut listener: IpcListener,
        shutdown: Arc<AtomicBool>,
        endpoint_name: &str,
        open_tx: async_channel::Sender<(PathBuf, OpenDisposition)>,
    ) {
        loop {
            if shutdown.load(Ordering::Acquire) {
                break;
            }

            match listener.accept() {
                Ok(stream) => {
                    let name = endpoint_name.to_string();
                    let tx = open_tx.clone();
                    if let Err(e) = thread::Builder::new()
                        .name("orca-daemon-conn".into())
                        .spawn(move || {
                            if let Err(e) = handle_connection(stream, &name, &tx) {
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
    pub fn enqueue_open_project(&self, path: PathBuf, disposition: OpenDisposition) {
        // Unbounded channel - send never blocks or fails while receiver is alive.
        let _ = self.open_project_tx.try_send((path, disposition));
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.listener_handle.take() {
            let _ = handle.join();
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
