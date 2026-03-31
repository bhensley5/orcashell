use anyhow::{bail, Result};

pub fn daemon_status() -> Result<()> {
    use orcashell_protocol::messages::{ClientCommand, DaemonResponse};
    let response = crate::client::send_command(ClientCommand::DaemonStatus)?;

    match response {
        DaemonResponse::Status {
            ok,
            pid,
            endpoint,
            protocol_version,
        } => {
            println!("Daemon status: {}", if ok { "ok" } else { "unhealthy" });
            println!("  PID: {pid}");
            println!("  Endpoint: {endpoint}");
            println!(
                "  Protocol: v{}.{}",
                protocol_version.major, protocol_version.minor
            );
            Ok(())
        }
        DaemonResponse::Error { message } => {
            bail!("daemon error: {message}");
        }
        other => bail!("unexpected response: {other:?}"),
    }
}
