use orcashell_daemon_core::server::DaemonServer;
use orcashell_ipc::{IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope};
use orcashell_protocol::version::{ProtocolVersion, CURRENT_PROTOCOL_VERSION};
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

fn send_envelope(
    endpoint: &IpcEndpoint,
    envelope: &Envelope<ClientCommand>,
) -> Envelope<DaemonResponse> {
    let mut stream =
        IpcStream::connect(endpoint, Duration::from_secs(5)).expect("failed to connect");
    let json = serde_json::to_string(envelope).unwrap();
    write_frame(&mut stream, json.as_bytes()).unwrap();
    let response_bytes = read_frame(&mut stream).unwrap();
    let response_str = std::str::from_utf8(&response_bytes).unwrap();
    serde_json::from_str(response_str).unwrap()
}

#[test]
fn daemon_status_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "status");

    let Some(_daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    // Give the listener thread a moment to start
    std::thread::sleep(Duration::from_millis(100));

    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::DaemonStatus,
    };

    let response = send_envelope(&endpoint, &request);

    assert_eq!(response.protocol_version, CURRENT_PROTOCOL_VERSION);
    match response.payload {
        DaemonResponse::Status {
            ok,
            pid,
            endpoint: ep_name,
            protocol_version,
        } => {
            assert!(ok);
            assert_eq!(pid, std::process::id());
            assert_eq!(ep_name, endpoint.display_name);
            assert_eq!(protocol_version, CURRENT_PROTOCOL_VERSION);
        }
        DaemonResponse::Error { message } => {
            panic!("expected Status, got Error: {message}");
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn protocol_version_mismatch_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "mismatch");

    let Some(_daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let request = Envelope {
        protocol_version: ProtocolVersion {
            major: 99,
            minor: 0,
        },
        payload: ClientCommand::DaemonStatus,
    };

    let response = send_envelope(&endpoint, &request);

    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(message.contains("mismatch"));
        }
        other => panic!("expected Error for version mismatch, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn stale_socket_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "stale");
    let socket_path = std::path::Path::new(&endpoint.display_name);

    // Create a stale file at the socket path
    std::fs::write(socket_path, "stale").unwrap();
    assert!(socket_path.exists());

    // Daemon should remove stale file and start successfully
    let Some(_daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    // Verify the daemon works
    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::DaemonStatus,
    };

    let response = send_envelope(&endpoint, &request);
    match response.payload {
        DaemonResponse::Status { ok, .. } => assert!(ok),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn daemon_double_start_detection() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "double");

    let Some(_daemon1) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    // Second start should fail with AddrInUse
    match DaemonServer::start(&endpoint) {
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::AddrInUse),
        Ok(_) => panic!("second daemon start should have failed with AddrInUse"),
    }
}
