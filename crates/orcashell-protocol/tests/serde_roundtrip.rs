use orcashell_protocol::*;

#[test]
fn client_command_daemon_status_roundtrip() {
    let cmd = ClientCommand::DaemonStatus;
    let json = serde_json::to_string(&cmd).unwrap();
    let deserialized: ClientCommand = serde_json::from_str(&json).unwrap();
    assert_eq!(cmd, deserialized);
}

#[test]
fn daemon_response_status_roundtrip() {
    let resp = DaemonResponse::Status {
        ok: true,
        pid: 1234,
        endpoint: "/tmp/orcashell.sock".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    };
    let json = serde_json::to_string(&resp).unwrap();
    let deserialized: DaemonResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, deserialized);
}

#[test]
fn daemon_response_error_roundtrip() {
    let resp = DaemonResponse::Error {
        message: "test error".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    let deserialized: DaemonResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, deserialized);
}

#[test]
fn envelope_client_command_roundtrip() {
    let envelope = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::DaemonStatus,
    };
    let json = serde_json::to_string(&envelope).unwrap();
    let deserialized: Envelope<ClientCommand> = serde_json::from_str(&json).unwrap();
    assert_eq!(envelope, deserialized);
}

#[test]
fn envelope_daemon_response_roundtrip() {
    let envelope = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: DaemonResponse::Status {
            ok: true,
            pid: 42,
            endpoint: "/tmp/test.sock".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
        },
    };
    let json = serde_json::to_string(&envelope).unwrap();
    let deserialized: Envelope<DaemonResponse> = serde_json::from_str(&json).unwrap();
    assert_eq!(envelope, deserialized);
}

#[test]
fn daemon_response_status_accepts_old_socket_path_field() {
    // The `endpoint` field has `#[serde(alias = "socket_path")]` for
    // backward compatibility with old JSON payloads.
    let json = r#"{"Status":{"ok":true,"pid":99,"socket_path":"/tmp/old.sock","protocol_version":{"major":1,"minor":0}}}"#;
    let resp: DaemonResponse = serde_json::from_str(json).unwrap();
    match resp {
        DaemonResponse::Status { endpoint, .. } => {
            assert_eq!(endpoint, "/tmp/old.sock");
        }
        _ => panic!("expected Status"),
    }
}

#[test]
fn protocol_version_is_compatible_same_major() {
    let v1 = ProtocolVersion { major: 1, minor: 0 };
    let v2 = ProtocolVersion { major: 1, minor: 5 };
    assert!(v1.is_compatible(&v2));
}

#[test]
fn protocol_version_incompatible_different_major() {
    let v1 = ProtocolVersion { major: 1, minor: 0 };
    let v2 = ProtocolVersion { major: 2, minor: 0 };
    assert!(!v1.is_compatible(&v2));
}
