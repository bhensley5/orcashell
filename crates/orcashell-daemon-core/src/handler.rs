use std::path::PathBuf;

use anyhow::{Context, Result};
use orcashell_ipc::IpcStream;
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope, OpenDisposition};
use orcashell_protocol::version::CURRENT_PROTOCOL_VERSION;

pub fn handle_connection(
    mut stream: IpcStream,
    endpoint_name: &str,
    open_tx: &async_channel::Sender<(PathBuf, OpenDisposition)>,
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
                payload: DaemonResponse::Status {
                    ok: true,
                    pid: std::process::id(),
                    endpoint: endpoint_name.to_string(),
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                },
            },
            ClientCommand::OpenProject { path, disposition } => {
                let pb = PathBuf::from(&path);
                if !pb.is_absolute() {
                    Envelope {
                        protocol_version: CURRENT_PROTOCOL_VERSION,
                        payload: DaemonResponse::Error {
                            message: format!("path must be absolute: {path}"),
                        },
                    }
                } else if !pb.is_dir() {
                    Envelope {
                        protocol_version: CURRENT_PROTOCOL_VERSION,
                        payload: DaemonResponse::Error {
                            message: format!("not a directory: {path}"),
                        },
                    }
                } else {
                    // Unbounded channel - try_send never fails while the receiver is alive.
                    let _ = open_tx.try_send((pb, disposition));
                    Envelope {
                        protocol_version: CURRENT_PROTOCOL_VERSION,
                        payload: DaemonResponse::ProjectOpened { path },
                    }
                }
            }
        }
    };

    let response_json = serde_json::to_string(&response).context("failed to serialize response")?;
    write_frame(&mut stream, response_json.as_bytes()).context("failed to write response frame")?;

    Ok(())
}
