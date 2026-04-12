// Explicit imports to avoid pulling in GPUI types that blow the
// gpui_macros proc-macro stack during test compilation.
use super::WindowRegistry;

// We can't easily construct real AnyWindowHandle in tests without GPUI,
// so the registry logic is validated through the integration in main.rs.
// These tests cover the pure HashMap logic using a mock approach.

#[test]
fn test_count_empty() {
    let reg = WindowRegistry::new();
    assert_eq!(reg.count(), 0);
}
