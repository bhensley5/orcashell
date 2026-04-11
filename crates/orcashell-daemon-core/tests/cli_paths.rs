use orcashell_daemon_core::server::DaemonServer;
use orcashell_ipc::{IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope};
use orcashell_protocol::version::ProtocolVersion;
use std::io;
use std::thread;
use std::time::Duration;
use std::time::Instant;

fn test_endpoint(dir: &std::path::Path, name: &str) -> IpcEndpoint {
    #[cfg(unix)]
    {
        let path = dir.join(format!("{name}.sock"));
        let s = path.to_string_lossy().into_owned();
        IpcEndpoint::new(s.clone(), s)
    }
    #[cfg(windows)]
    {
        let unique = dir.file_name().unwrap().to_string_lossy();
        let pipe = format!(r"\\.\pipe\orcashell-test-{unique}-{name}");
        IpcEndpoint::new(pipe.clone(), pipe)
    }
}

fn start_daemon_or_skip(endpoint: &IpcEndpoint) -> Option<DaemonServer> {
    match DaemonServer::start(endpoint) {
        Ok(daemon) => Some(daemon),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!(
                "skipping IPC test: permission denied for {}: {e}",
                endpoint.display_name
            );
            None
        }
        Err(e) => panic!("failed to start daemon at {}: {e}", endpoint.display_name),
    }
}

fn is_transient_ipc_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::TimedOut
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
    )
}

fn send_envelope_with_retry(
    endpoint: &IpcEndpoint,
    envelope: &Envelope<ClientCommand>,
) -> io::Result<Envelope<DaemonResponse>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let json = serde_json::to_string(envelope).unwrap();

    loop {
        match IpcStream::connect(endpoint, Duration::from_secs(5)) {
            Ok(mut stream) => {
                if let Err(e) = write_frame(&mut stream, json.as_bytes()) {
                    if is_transient_ipc_error(&e) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    return Err(e);
                }

                let response_bytes = match read_frame(&mut stream) {
                    Ok(bytes) => bytes,
                    Err(e) if is_transient_ipc_error(&e) && Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let response_str = std::str::from_utf8(&response_bytes).unwrap();
                let response = serde_json::from_str(response_str).unwrap();
                return Ok(response);
            }
            Err(e) if is_transient_ipc_error(&e) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}

#[test]
fn connect_failure_on_missing_endpoint() {
    let endpoint = IpcEndpoint::new(
        "nonexistent",
        if cfg!(unix) {
            "/tmp/orcashell-test-nonexistent-12345.sock"
        } else {
            r"\\.\pipe\orcashell-test-nonexistent-12345"
        },
    );
    let result = IpcStream::connect(&endpoint, Duration::from_secs(1));
    assert!(result.is_err());
}

#[test]
fn version_mismatch_from_client_perspective() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "vermismatch");

    let Some(_daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    let envelope = Envelope {
        protocol_version: ProtocolVersion {
            major: 99,
            minor: 0,
        },
        payload: ClientCommand::DaemonStatus,
    };
    let response = send_envelope_with_retry(&endpoint, &envelope).unwrap();

    // Daemon should return an error payload
    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(message.contains("mismatch"));
        }
        _ => panic!("expected error response for version mismatch"),
    }
}
