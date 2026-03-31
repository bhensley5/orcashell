use crate::version::ProtocolVersion;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub protocol_version: ProtocolVersion,
    pub payload: T,
}

/// How to open a directory: as a new tab in the most-recent window, or in a fresh window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenDisposition {
    NewTab,
    NewWindow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientCommand {
    DaemonStatus,
    /// Open the given absolute directory path in OrcaShell.
    OpenProject {
        path: String,
        disposition: OpenDisposition,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonResponse {
    Status {
        ok: bool,
        pid: u32,
        #[serde(alias = "socket_path")]
        endpoint: String,
        protocol_version: ProtocolVersion,
    },
    /// Sent when the daemon has enqueued an open-project request.
    ProjectOpened {
        path: String,
    },
    Error {
        message: String,
    },
}
