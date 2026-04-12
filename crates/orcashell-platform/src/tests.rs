use super::*;

#[test]
fn replace_file_basic() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("data.tmp");
    let dest = dir.path().join("data.json");

    std::fs::write(&temp, b"hello").unwrap();
    replace_file(&temp, &dest).unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    assert!(!temp.exists(), "temp file should be removed after replace");
}

#[test]
fn replace_file_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("data.tmp");
    let dest = dir.path().join("data.json");

    std::fs::write(&dest, b"old content").unwrap();
    std::fs::write(&temp, b"new content").unwrap();
    replace_file(&temp, &dest).unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new content");
    assert!(!temp.exists(), "temp file should be removed after replace");
}
