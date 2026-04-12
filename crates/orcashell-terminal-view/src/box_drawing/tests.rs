use super::*;

#[test]
fn test_is_box_drawing_char() {
    assert!(is_box_drawing_char('─')); // U+2500
    assert!(is_box_drawing_char('│')); // U+2502
    assert!(is_box_drawing_char('┌')); // U+250C
    assert!(is_box_drawing_char('┼')); // U+253C
    assert!(is_box_drawing_char('╬')); // U+256C
    assert!(is_box_drawing_char('╿')); // U+257F (last)
    assert!(!is_box_drawing_char('A'));
    assert!(!is_box_drawing_char(' '));
    assert!(!is_box_drawing_char('█')); // Block element, not box drawing
}

#[test]
fn test_is_block_element() {
    assert!(is_block_element('█')); // U+2588 full block
    assert!(is_block_element('▀')); // U+2580 upper half
    assert!(is_block_element('▄')); // U+2584 lower half
    assert!(is_block_element('▌')); // U+258C left half
    assert!(is_block_element('░')); // U+2591 light shade
    assert!(is_block_element('▒')); // U+2592 medium shade
    assert!(is_block_element('▓')); // U+2593 dark shade
    assert!(is_block_element('▟')); // U+259F (last)
    assert!(!is_block_element('─')); // Box drawing, not block
    assert!(!is_block_element('A'));
}

#[test]
fn test_get_box_segments_horizontal() {
    let seg = get_box_segments('─').unwrap();
    assert_eq!(seg.left, Some(Light));
    assert_eq!(seg.right, Some(Light));
    assert_eq!(seg.top, None);
    assert_eq!(seg.bottom, None);
}

#[test]
fn test_get_box_segments_vertical() {
    let seg = get_box_segments('│').unwrap();
    assert_eq!(seg.left, None);
    assert_eq!(seg.right, None);
    assert_eq!(seg.top, Some(Light));
    assert_eq!(seg.bottom, Some(Light));
}

#[test]
fn test_get_box_segments_corner() {
    let seg = get_box_segments('┌').unwrap();
    assert_eq!(seg.left, None);
    assert_eq!(seg.right, Some(Light));
    assert_eq!(seg.top, None);
    assert_eq!(seg.bottom, Some(Light));
}

#[test]
fn test_get_box_segments_cross() {
    let seg = get_box_segments('┼').unwrap();
    assert_eq!(seg.left, Some(Light));
    assert_eq!(seg.right, Some(Light));
    assert_eq!(seg.top, Some(Light));
    assert_eq!(seg.bottom, Some(Light));
}

#[test]
fn test_get_box_segments_double() {
    let seg = get_box_segments('═').unwrap();
    assert_eq!(seg.left, Some(Double));
    assert_eq!(seg.right, Some(Double));
    assert_eq!(seg.top, None);
    assert_eq!(seg.bottom, None);
}

#[test]
fn test_get_box_segments_invalid() {
    assert!(get_box_segments('A').is_none());
    assert!(get_box_segments(' ').is_none());
}
