use orcashell_ipc::{IpcEndpoint, IpcStream};
use std::time::Duration;

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
