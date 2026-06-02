use std::path::PathBuf;

use anyhow::{Context, Result};
use orcashell_ipc::IpcStream;
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope, OpenDisposition};
use orcashell_protocol::version::CURRENT_PROTOCOL_VERSION;

use crate::open_project::{enqueue_validated_open_project, OpenProjectEnqueueError};

pub fn handle_connection(
    mut stream: IpcStream,
    endpoint_name: &str,
    open_tx: &async_channel::Sender<(PathBuf, OpenDisposition)>,
    required_capability: Option<&str>,
) -> Result<()> {
    let request_bytes = read_frame(&mut stream).context("failed to read request frame")?;

    let request_str = std::str::from_utf8(&request_bytes).context("request is not valid UTF-8")?;

    let envelope: Envelope<ClientCommand> =
        serde_json::from_str(request_str).context("failed to deserialize request")?;

    let response = if !envelope
        .protocol_version
        .is_compatible(&CURRENT_PROTOCOL_VERSION)
    {
        Envelope {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            auth: None,
            payload: DaemonResponse::Error {
                message: format!(
                    "protocol version mismatch: client={}.{}, daemon={}.{}",
                    envelope.protocol_version.major,
                    envelope.protocol_version.minor,
                    CURRENT_PROTOCOL_VERSION.major,
                    CURRENT_PROTOCOL_VERSION.minor,
                ),
            },
        }
    } else {
        match envelope.payload {
            ClientCommand::DaemonStatus => Envelope {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                auth: None,
                payload: DaemonResponse::Status {
                    ok: true,
                    pid: std::process::id(),
                    endpoint: endpoint_name.to_string(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                },
            },
            ClientCommand::OpenProject { path, disposition } => {
                if !is_authorized(envelope.auth.as_ref(), required_capability) {
                    return write_response(
                        &mut stream,
                        Envelope {
                            protocol_version: CURRENT_PROTOCOL_VERSION,
                            auth: None,
                            payload: DaemonResponse::Error {
                                message: "unauthorized IPC command".to_string(),
                            },
                        },
                    );
                }
                let pb = PathBuf::from(&path);
                match enqueue_validated_open_project(open_tx, pb, disposition) {
                    Ok(canonical) => Envelope {
                        protocol_version: CURRENT_PROTOCOL_VERSION,
                        auth: None,
                        payload: DaemonResponse::ProjectOpened {
                            path: canonical.to_string_lossy().into_owned(),
                        },
                    },
                    Err(error) => {
                        let message = match error {
                            OpenProjectEnqueueError::InvalidPath(message) => message,
                            OpenProjectEnqueueError::QueueFull => {
                                "open-project queue is full".to_string()
                            }
                            OpenProjectEnqueueError::ReceiverClosed => {
                                "open-project receiver is closed".to_string()
                            }
                        };
                        Envelope {
                            protocol_version: CURRENT_PROTOCOL_VERSION,
                            auth: None,
                            payload: DaemonResponse::Error { message },
                        }
                    }
                }
            }
        }
    };

    write_response(&mut stream, response)
}

fn is_authorized(
    auth: Option<&orcashell_protocol::messages::CommandAuth>,
    required_capability: Option<&str>,
) -> bool {
    let Some(expected) = required_capability else {
        return true;
    };
    let Some(provided) = auth.map(|auth| auth.capability.as_str()) else {
        return false;
    };
    constant_time_eq(provided.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&left, &right) in left.iter().zip(right) {
        diff |= left ^ right;
    }
    diff == 0
}

fn write_response(stream: &mut IpcStream, response: Envelope<DaemonResponse>) -> Result<()> {
    let response_json = serde_json::to_string(&response).context("failed to serialize response")?;
    write_frame(stream, response_json.as_bytes()).context("failed to write response frame")?;
    Ok(())
}
