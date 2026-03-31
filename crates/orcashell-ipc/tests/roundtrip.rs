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
