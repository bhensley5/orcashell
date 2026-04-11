use orcashell_daemon_core::server::DaemonServer;
use orcashell_ipc::{IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{ClientCommand, DaemonResponse, Envelope, OpenDisposition};
use orcashell_protocol::version::CURRENT_PROTOCOL_VERSION;
use std::io;
use std::path::PathBuf;
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

/// Non-absolute path → DaemonResponse::Error
#[test]
fn open_project_relative_path_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-relative");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::OpenProject {
            path: "relative/path".to_string(),
            disposition: OpenDisposition::NewTab,
        },
    };

    let response = send_envelope(&endpoint, &request);
    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(
                message.contains("absolute"),
                "expected 'absolute' in: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // Receiver should be empty. Nothing was enqueued.
    assert!(daemon.open_project_receiver().is_empty());
}

/// Absolute path that does not exist → DaemonResponse::Error
#[test]
fn open_project_nonexistent_path_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-nonexist");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::OpenProject {
            path: if cfg!(windows) {
                r"C:\this\path\definitely\does\not\exist\orcashell-test-x9z".to_string()
            } else {
                "/this/path/definitely/does/not/exist/orcashell-test-x9z".to_string()
            },
            disposition: OpenDisposition::NewTab,
        },
    };

    let response = send_envelope(&endpoint, &request);
    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(
                message.contains("not a directory") || message.contains("directory"),
                "expected directory error in: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    assert!(daemon.open_project_receiver().is_empty());
}

/// Valid absolute directory → ProjectOpened + item in receiver with correct disposition
#[test]
fn open_project_valid_dir_enqueues_with_disposition() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-valid");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let target_dir = dir.path().to_str().unwrap().to_string();

    // Test NewTab disposition
    let request_tab = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::OpenProject {
            path: target_dir.clone(),
            disposition: OpenDisposition::NewTab,
        },
    };
    let resp = send_envelope(&endpoint, &request_tab);
    match resp.payload {
        DaemonResponse::ProjectOpened { path } => assert_eq!(path, target_dir),
        other => panic!("expected ProjectOpened, got {other:?}"),
    }

    // Test NewWindow disposition
    let request_win = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::OpenProject {
            path: target_dir.clone(),
            disposition: OpenDisposition::NewWindow,
        },
    };
    let resp2 = send_envelope(&endpoint, &request_win);
    match resp2.payload {
        DaemonResponse::ProjectOpened { path } => assert_eq!(path, target_dir),
        other => panic!("expected ProjectOpened, got {other:?}"),
    }

    // Poll for both items to appear. Avoid a fixed sleep that may be too short on slow CI.
    let rx = daemon.open_project_receiver();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if rx.len() >= 2 || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (path1, disp1) = rx.try_recv().expect("expected first enqueued item");
    let (path2, disp2) = rx.try_recv().expect("expected second enqueued item");

    assert_eq!(path1, PathBuf::from(&target_dir));
    assert_eq!(disp1, OpenDisposition::NewTab);
    assert_eq!(path2, PathBuf::from(&target_dir));
    assert_eq!(disp2, OpenDisposition::NewWindow);
    assert!(rx.is_empty(), "no extra items should be enqueued");
}

/// enqueue_open_project directly → item appears in receiver without IPC
#[test]
fn enqueue_open_project_bypasses_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-direct");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    let path = dir.path().to_path_buf();
    daemon.enqueue_open_project(path.clone(), OpenDisposition::NewWindow);

    let rx = daemon.open_project_receiver();
    let (received_path, received_disp) = rx.try_recv().expect("item should be in receiver");
    assert_eq!(received_path, path);
    assert_eq!(received_disp, OpenDisposition::NewWindow);
    assert!(rx.is_empty());
}
