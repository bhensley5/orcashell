use orcashell_ipc::{IpcEndpoint, IpcListener, IpcStream};
use std::io::{Read, Write};
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

#[test]
fn ipc_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "roundtrip");

    let mut listener = IpcListener::bind(&endpoint).unwrap();

    let ep_clone = endpoint.clone();
    let client = std::thread::spawn(move || {
        let mut stream = IpcStream::connect(&ep_clone, Duration::from_secs(5)).unwrap();
        stream.write_all(b"hello").unwrap();
        stream.flush().unwrap();
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    });

    let mut stream = listener.accept().unwrap();
    let mut buf = [0u8; 5];
    stream.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    stream.write_all(b"world").unwrap();
    stream.flush().unwrap();

    client.join().unwrap();

    #[cfg(unix)]
    {
        let peer = stream
            .peer_identity()
            .expect("accepted unix stream should expose peer identity");
        assert_eq!(peer.uid, unsafe { libc::geteuid() as u32 });
    }
}

#[test]
fn bind_conflict_detection() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "conflict");

    let _listener = IpcListener::bind(&endpoint).unwrap();

    match IpcListener::bind(&endpoint) {
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse),
        Ok(_) => panic!("second bind should have failed with AddrInUse"),
    }
}

#[test]
fn connect_to_nonexistent() {
    let endpoint = IpcEndpoint::new(
        "nonexistent",
        if cfg!(unix) {
            "/tmp/orcashell-ipc-test-nonexistent-99999.sock"
        } else {
            r"\\.\pipe\orcashell-ipc-test-nonexistent-99999"
        },
    );
    let result = IpcStream::connect(&endpoint, Duration::from_secs(1));
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn unix_listener_sets_owner_only_socket_permissions() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "socket-mode");
    let _listener = IpcListener::bind(&endpoint).unwrap();

    let metadata = std::fs::symlink_metadata(&endpoint.display_name).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn unix_listener_rejects_symlink_socket_path() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(dir.path(), "socket-symlink");
    let socket_path = std::path::Path::new(&endpoint.display_name);
    let target = dir.path().join("missing-target.sock");
    symlink(&target, socket_path).unwrap();

    match IpcListener::bind(&endpoint) {
        Ok(_) => panic!("expected symlink endpoint to be rejected"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
    }
    assert!(std::fs::symlink_metadata(socket_path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn default_unix_endpoint_lives_in_private_runtime_dir() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    let endpoint = orcashell_ipc::default_endpoint().unwrap();
    assert_ne!(endpoint.display_name, "/tmp/orcashell.sock");

    let socket_path = Path::new(&endpoint.display_name);
    let dir = socket_path
        .parent()
        .expect("endpoint should have parent dir");
    let metadata = std::fs::symlink_metadata(dir).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert!(endpoint.capability_path().is_some());
}
