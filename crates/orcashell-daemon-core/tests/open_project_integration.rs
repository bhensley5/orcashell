use orcashell_daemon_core::server::{
    DaemonServer, OpenProjectEnqueueError, OPEN_PROJECT_QUEUE_CAPACITY,
};
use orcashell_ipc::{IpcEndpoint, IpcStream};
use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::messages::{
    ClientCommand, CommandAuth, DaemonResponse, Envelope, OpenDisposition,
};
use orcashell_protocol::version::CURRENT_PROTOCOL_VERSION;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

fn test_endpoint(dir: &std::path::Path, name: &str) -> IpcEndpoint {
    #[cfg(unix)]
    {
        let path = dir.join(format!("{name}.sock"));
        let token = dir.join(format!("{name}.token"));
        let s = path.to_string_lossy().into_owned();
        IpcEndpoint::new_with_capability(s.clone(), s, token)
    }
    #[cfg(windows)]
    {
        let unique = dir.file_name().unwrap().to_string_lossy();
        let pipe = format!(r"\\.\pipe\orcashell-test-{unique}-{name}");
        let token = dir.join(format!("{name}.token"));
        IpcEndpoint::new_with_capability(pipe.clone(), pipe, token)
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

fn command_auth(endpoint: &IpcEndpoint) -> Option<CommandAuth> {
    endpoint
        .read_capability_token()
        .unwrap()
        .map(|capability| CommandAuth { capability })
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
        auth: command_auth(&endpoint),
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
        auth: command_auth(&endpoint),
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

    let target_path = std::fs::canonicalize(dir.path()).unwrap();
    let target_dir = target_path.to_string_lossy().into_owned();

    // Test NewTab disposition
    let request_tab = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        auth: command_auth(&endpoint),
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
        auth: command_auth(&endpoint),
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

#[test]
fn open_project_enqueues_canonical_path() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-canonical");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let request_path = dir.path().join(".");
    let canonical_path = std::fs::canonicalize(&request_path).unwrap();
    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        auth: command_auth(&endpoint),
        payload: ClientCommand::OpenProject {
            path: request_path.to_string_lossy().into_owned(),
            disposition: OpenDisposition::NewTab,
        },
    };

    let response = send_envelope(&endpoint, &request);
    match response.payload {
        DaemonResponse::ProjectOpened { path } => {
            assert_eq!(PathBuf::from(path), canonical_path);
        }
        other => panic!("expected ProjectOpened, got {other:?}"),
    }

    let rx = daemon.open_project_receiver();
    let (queued_path, queued_disposition) = rx.try_recv().expect("expected enqueued item");
    assert_eq!(queued_path, canonical_path);
    assert_eq!(queued_disposition, OpenDisposition::NewTab);
    assert!(rx.is_empty());
}

#[test]
fn open_project_without_capability_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-unauthorized");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(100));

    let request = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        auth: None,
        payload: ClientCommand::OpenProject {
            path: dir.path().to_string_lossy().into_owned(),
            disposition: OpenDisposition::NewTab,
        },
    };

    let response = send_envelope(&endpoint, &request);
    match response.payload {
        DaemonResponse::Error { message } => {
            assert!(
                message.contains("unauthorized"),
                "expected unauthorized error in: {message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    assert!(daemon.open_project_receiver().is_empty());
}

/// enqueue_open_project directly → validated canonical item appears in receiver without IPC
#[test]
fn enqueue_open_project_validates_and_canonicalizes_without_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-direct");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    let path = dir.path().join(".");
    let canonical = std::fs::canonicalize(&path).unwrap();
    daemon
        .enqueue_open_project(path, OpenDisposition::NewWindow)
        .unwrap();

    let rx = daemon.open_project_receiver();
    let (received_path, received_disp) = rx.try_recv().expect("item should be in receiver");
    assert_eq!(received_path, canonical);
    assert_eq!(received_disp, OpenDisposition::NewWindow);
    assert!(rx.is_empty());
}

#[test]
fn enqueue_open_project_rejects_invalid_direct_path() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-direct-invalid");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    let missing = dir.path().join("missing");
    let err = daemon
        .enqueue_open_project(missing, OpenDisposition::NewWindow)
        .expect_err("missing project directory should be rejected");

    assert!(
        err.to_string().contains("not a directory"),
        "unexpected error: {err}"
    );
    assert!(daemon.open_project_receiver().is_empty());
}

#[test]
fn enqueue_open_project_rejects_relative_direct_path() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-direct-relative");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    let err = daemon
        .enqueue_open_project(PathBuf::from("relative"), OpenDisposition::NewWindow)
        .expect_err("relative project path should be rejected");

    assert!(
        err.to_string().contains("absolute"),
        "unexpected error: {err}"
    );
    assert!(daemon.open_project_receiver().is_empty());
}

#[test]
fn enqueue_open_project_reports_full_direct_queue() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "op-direct-full");

    let Some(daemon) = start_daemon_or_skip(&endpoint) else {
        return;
    };

    for _ in 0..OPEN_PROJECT_QUEUE_CAPACITY {
        daemon
            .enqueue_open_project(dir.path().to_path_buf(), OpenDisposition::NewWindow)
            .unwrap();
    }

    let err = daemon
        .enqueue_open_project(dir.path().to_path_buf(), OpenDisposition::NewWindow)
        .expect_err("bounded open-project queue should report full");

    assert!(matches!(err, OpenProjectEnqueueError::QueueFull));
}
