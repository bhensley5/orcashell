use std::fmt;
use std::path::{Path, PathBuf};

use orcashell_protocol::messages::OpenDisposition;

#[derive(Debug)]
pub enum OpenProjectEnqueueError {
    InvalidPath(String),
    QueueFull,
    ReceiverClosed,
}

impl fmt::Display for OpenProjectEnqueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(message) => f.write_str(message),
            Self::QueueFull => f.write_str("open-project queue is full"),
            Self::ReceiverClosed => f.write_str("open-project receiver is closed"),
        }
    }
}

impl std::error::Error for OpenProjectEnqueueError {}

pub(crate) fn canonicalize_project_path(path: &Path) -> Result<PathBuf, OpenProjectEnqueueError> {
    if !path.is_absolute() {
        return Err(OpenProjectEnqueueError::InvalidPath(format!(
            "path must be absolute: {}",
            path.display()
        )));
    }

    let canonical = std::fs::canonicalize(path).map_err(|_| {
        OpenProjectEnqueueError::InvalidPath(format!("not a directory: {}", path.display()))
    })?;
    if !canonical.is_dir() {
        return Err(OpenProjectEnqueueError::InvalidPath(format!(
            "not a directory: {}",
            path.display()
        )));
    }
    Ok(canonical)
}

pub(crate) fn enqueue_validated_open_project(
    open_tx: &async_channel::Sender<(PathBuf, OpenDisposition)>,
    path: PathBuf,
    disposition: OpenDisposition,
) -> Result<PathBuf, OpenProjectEnqueueError> {
    let canonical = canonicalize_project_path(&path)?;
    match open_tx.try_send((canonical.clone(), disposition)) {
        Ok(()) => Ok(canonical),
        Err(e) if e.is_full() => Err(OpenProjectEnqueueError::QueueFull),
        Err(_) => Err(OpenProjectEnqueueError::ReceiverClosed),
    }
}
