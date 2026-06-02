use crate::version::ProtocolVersion;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub protocol_version: ProtocolVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CommandAuth>,
    pub payload: T,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAuth {
    pub capability: String,
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

impl ClientCommand {
    pub fn requires_auth(&self) -> bool {
        match self {
            Self::DaemonStatus => false,
            Self::OpenProject { .. } => true,
        }
    }
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
