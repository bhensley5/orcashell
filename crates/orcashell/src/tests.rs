use super::is_visible_on_displays;

#[test]
fn test_window_visible_on_primary_display() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0)];
    assert!(is_visible_on_displays(
        (100.0, 100.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_window_completely_offscreen() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0)];
    assert!(!is_visible_on_displays(
        (5000.0, 3000.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_window_partially_visible() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0)];
    assert!(is_visible_on_displays(
        (1800.0, 500.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_window_on_secondary_display() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0), (1920.0, 0.0, 2560.0, 1440.0)];
    assert!(is_visible_on_displays(
        (2000.0, 100.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_window_between_displays_no_overlap() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0), (3000.0, 0.0, 2560.0, 1440.0)];
    assert!(!is_visible_on_displays(
        (2000.0, 100.0, 500.0, 400.0),
        &displays
    ));
}

#[test]
fn test_no_displays() {
    let displays: Vec<(f32, f32, f32, f32)> = vec![];
    assert!(!is_visible_on_displays(
        (100.0, 100.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_window_at_display_edge() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0)];
    assert!(!is_visible_on_displays(
        (1920.0, 0.0, 1200.0, 800.0),
        &displays
    ));
}

#[test]
fn test_negative_position() {
    let displays = vec![(0.0, 0.0, 1920.0, 1080.0)];
    assert!(is_visible_on_displays(
        (-100.0, 500.0, 1200.0, 800.0),
        &displays
    ));
}
