use orcashell_protocol::framing::{read_frame, write_frame};
use orcashell_protocol::*;
use std::io::Cursor;

#[test]
fn frame_roundtrip() {
    let data = b"hello world";
    let mut buf = Vec::new();
    write_frame(&mut buf, data).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_frame(&mut cursor).unwrap();
    assert_eq!(result, data);
}

#[test]
fn frame_empty_payload() {
    let data = b"";
    let mut buf = Vec::new();
    write_frame(&mut buf, data).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_frame(&mut cursor).unwrap();
    assert_eq!(result, data);
}

#[test]
fn frame_large_payload() {
    let data = vec![0xABu8; 64 * 1024];
    let mut buf = Vec::new();
    write_frame(&mut buf, &data).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = read_frame(&mut cursor).unwrap();
    assert_eq!(result, data);
}

#[test]
fn frame_json_envelope_roundtrip() {
    let envelope = Envelope {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        payload: ClientCommand::DaemonStatus,
    };
    let json = serde_json::to_string(&envelope).unwrap();

    let mut buf = Vec::new();
    write_frame(&mut buf, json.as_bytes()).unwrap();

    let mut cursor = Cursor::new(buf);
    let frame_bytes = read_frame(&mut cursor).unwrap();
    let frame_str = std::str::from_utf8(&frame_bytes).unwrap();
    let deserialized: Envelope<ClientCommand> = serde_json::from_str(frame_str).unwrap();

    assert_eq!(envelope, deserialized);
}

#[test]
fn frame_truncated_payload_returns_error() {
    // Length prefix says 100 bytes, but only 5 follow
    let mut buf = Vec::new();
    buf.extend_from_slice(&100u32.to_le_bytes());
    buf.extend_from_slice(b"short");

    let mut cursor = Cursor::new(buf);
    let result = read_frame(&mut cursor);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn frame_empty_input_returns_error() {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let result = read_frame(&mut cursor);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn frame_oversized_length_rejected() {
    // Length prefix claims 32 MiB (exceeds 16 MiB limit)
    let mut buf = Vec::new();
    buf.extend_from_slice(&(32 * 1024 * 1024u32).to_le_bytes());

    let mut cursor = Cursor::new(buf);
    let result = read_frame(&mut cursor);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
}
