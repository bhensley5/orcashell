use orcashell_daemon_core::server::DaemonServer;
use orcashell_ipc::{IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope};
use orcashell_protocol::version::ProtocolVersion;
use std::io;
use std::time::Duration;

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
    std::thread::sleep(Duration::from_millis(100));

    // Send with incompatible version
    let mut stream = IpcStream::connect(&endpoint, Duration::from_secs(5)).unwrap();
    let envelope = Envelope {
        protocol_version: ProtocolVersion {
            major: 99,
            minor: 0,
        },
        payload: ClientCommand::DaemonStatus,
    };
    let json = serde_json::to_string(&envelope).unwrap();
    write_frame(&mut stream, json.as_bytes()).unwrap();

    let response_bytes = read_frame(&mut stream).unwrap();
    let response_str = std::str::from_utf8(&response_bytes).unwrap();
    let response: Envelope<DaemonResponse> = serde_json::from_str(response_str).unwrap();

    // Daemon should return an error payload
    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(message.contains("mismatch"));
        }
        _ => panic!("expected error response for version mismatch"),
    }
}
