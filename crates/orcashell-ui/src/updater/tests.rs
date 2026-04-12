use super::*;

#[test]
fn version_compare_handles_semver_like_numbers() {
    assert!(compare_versions("0.1.3", "0.1.2").is_gt());
    assert!(compare_versions("1.0.0", "0.9.9").is_gt());
    assert!(compare_versions("0.1.2", "0.1.2").is_eq());
    assert!(compare_versions("0.1.2", "0.1.10").is_lt());
}
