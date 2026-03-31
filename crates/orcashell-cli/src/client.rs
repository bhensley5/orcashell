use anyhow::{bail, Context, Result};
use orcashell_ipc::{default_endpoint, IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope};
use orcashell_protocol::version::CURRENT_PROTOCOL_VERSION;
use std::time::Duration;

const CONN_TIMEOUT: Duration = Duration::from_secs(5);

pub fn send_command(command: ClientCommand) -> Result<DaemonResponse> {
    let endpoint = default_endpoint().context("failed to resolve default IPC endpoint")?;
    send_command_to(&endpoint, command)
}

pub fn send_command_to(endpoint: &IpcEndpoint, command: ClientCommand) -> Result<DaemonResponse> {
    let mut stream = IpcStream::connect(endpoint, CONN_TIMEOUT)
        .context("failed to connect to daemon - is the OrcaShell app running?")?;

    let envelope = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: command,
    };

    let request_json = serde_json::to_string(&envelope).context("failed to serialize request")?;
    write_frame(&mut stream, request_json.as_bytes()).context("failed to send request")?;

    let response_bytes = read_frame(&mut stream).context("failed to read response")?;
    let response_str =
        std::str::from_utf8(&response_bytes).context("response is not valid UTF-8")?;
    let response_envelope: Envelope<DaemonResponse> =
        serde_json::from_str(response_str).context("failed to deserialize response")?;

    if !response_envelope
        .protocol_version
        .is_compatible(&CURRENT_PROTOCOL_VERSION)
    {
        bail!(
            "daemon protocol version mismatch: expected major {}, got {}",
            CURRENT_PROTOCOL_VERSION.major,
            response_envelope.protocol_version.major,
        );
    }

    Ok(response_envelope.payload)
}
