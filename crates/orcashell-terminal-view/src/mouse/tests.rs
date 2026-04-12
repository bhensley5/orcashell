use super::*;
use gpui::{point, px};

#[test]
fn test_pixel_to_cell() {
    let position = point(px(100.0), px(50.0));
    let origin = point(px(10.0), px(10.0));
    let cell_width = px(10.0);
    let cell_height = px(20.0);

    let point = pixel_to_cell(position, origin, cell_width, cell_height);
    assert_eq!(point.column.0, 9);
    assert_eq!(point.line.0, 2);
}

#[test]
fn test_pixel_to_cell_at_origin() {
    let position = point(px(10.0), px(10.0));
    let origin = point(px(10.0), px(10.0));
    let cell_width = px(10.0);
    let cell_height = px(20.0);

    let point = pixel_to_cell(position, origin, cell_width, cell_height);
    assert_eq!(point.column.0, 0);
    assert_eq!(point.line.0, 0);
}

#[test]
fn test_pixel_to_cell_negative_coordinates() {
    // Coordinates before origin should clamp to 0
    let position = point(px(5.0), px(5.0));
    let origin = point(px(10.0), px(10.0));
    let cell_width = px(10.0);
    let cell_height = px(20.0);

    let point = pixel_to_cell(position, origin, cell_width, cell_height);
    assert_eq!(point.column.0, 0);
    assert_eq!(point.line.0, 0);
}

#[test]
fn test_selection_type_from_clicks() {
    use alacritty_terminal::selection::SelectionType;
    assert_eq!(selection_type_from_clicks(1), SelectionType::Simple);
    assert_eq!(selection_type_from_clicks(2), SelectionType::Semantic);
    assert_eq!(selection_type_from_clicks(3), SelectionType::Lines);
    assert_eq!(selection_type_from_clicks(4), SelectionType::Lines);
    assert_eq!(selection_type_from_clicks(10), SelectionType::Lines);
}

#[test]
fn test_mouse_button_report_left_click() {
    let point = AlacPoint::new(Line(5), Column(10));
    let mode = TermMode::MOUSE_REPORT_CLICK;

    let bytes = mouse_button_report(MouseButton::Left, true, point, 0, mode);
    assert!(bytes.is_some());

    let sequence = String::from_utf8(bytes.unwrap()).unwrap();
    // SGR format: ESC[<button;col;row M
    // Line 5 (0-indexed) = row 6 (1-indexed)
    // Column 10 (0-indexed) = col 11 (1-indexed)
    assert_eq!(sequence, "\x1b[<0;11;6M");
}

#[test]
fn test_mouse_button_report_right_release() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_REPORT_CLICK;

    let bytes = mouse_button_report(MouseButton::Right, false, point, 0, mode);
    assert!(bytes.is_some());

    let sequence = String::from_utf8(bytes.unwrap()).unwrap();
    // Right button = 2, released = 'm'
    assert_eq!(sequence, "\x1b[<2;1;1m");
}

#[test]
fn test_mouse_button_report_with_modifiers() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_REPORT_CLICK;
    let modifiers = encode_modifiers(true, true, true); // Shift + Alt + Ctrl = 28

    let bytes = mouse_button_report(MouseButton::Left, true, point, modifiers, mode);
    assert!(bytes.is_some());

    let sequence = String::from_utf8(bytes.unwrap()).unwrap();
    // Button 0 + modifiers 28 = 28
    assert_eq!(sequence, "\x1b[<28;1;1M");
}

#[test]
fn test_mouse_button_report_disabled() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::empty();

    let bytes = mouse_button_report(MouseButton::Left, true, point, 0, mode);
    assert!(bytes.is_none());
}

#[test]
fn test_scroll_report_mouse_mode() {
    let point = AlacPoint::new(Line(5), Column(10));
    let mode = TermMode::MOUSE_REPORT_CLICK;

    // Scroll up
    let bytes = scroll_report(3, point, 0, mode);
    assert!(bytes.is_some());
    let sequence = String::from_utf8(bytes.unwrap()).unwrap();
    // Wheel up = button 64
    assert_eq!(sequence, "\x1b[<64;11;6M");

    // Scroll down
    let bytes = scroll_report(-2, point, 0, mode);
    assert!(bytes.is_some());
    let sequence = String::from_utf8(bytes.unwrap()).unwrap();
    // Wheel down = button 65
    assert_eq!(sequence, "\x1b[<65;11;6M");
}

#[test]
fn test_scroll_report_alternate_screen() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::ALT_SCREEN;

    // Scroll up in alternate screen = arrow up keys
    let bytes = scroll_report(3, point, 0, mode);
    assert!(bytes.is_some());
    let sequence = bytes.unwrap();
    // Should be 3 arrow up sequences
    assert_eq!(sequence, b"\x1b[A\x1b[A\x1b[A");
}

#[test]
fn test_scroll_report_alternate_screen_app_cursor() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::ALT_SCREEN | TermMode::APP_CURSOR;

    // Scroll down with app cursor mode
    let bytes = scroll_report(-2, point, 0, mode);
    assert!(bytes.is_some());
    let sequence = bytes.unwrap();
    // Should be 2 arrow down sequences in app cursor format
    assert_eq!(sequence, b"\x1bOB\x1bOB");
}

#[test]
fn test_scroll_report_normal_screen() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::empty();

    // In normal screen mode, scrolling should be handled locally
    let bytes = scroll_report(3, point, 0, mode);
    assert!(bytes.is_none());
}

#[test]
fn test_encode_modifiers() {
    assert_eq!(encode_modifiers(false, false, false), 0);
    assert_eq!(encode_modifiers(true, false, false), 4);
    assert_eq!(encode_modifiers(false, true, false), 8);
    assert_eq!(encode_modifiers(false, false, true), 16);
    assert_eq!(encode_modifiers(true, true, false), 12);
    assert_eq!(encode_modifiers(true, false, true), 20);
    assert_eq!(encode_modifiers(false, true, true), 24);
    assert_eq!(encode_modifiers(true, true, true), 28);
}

#[test]
fn test_pixels_to_scroll_lines() {
    let cell_height = px(20.0);

    assert_eq!(pixels_to_scroll_lines(px(60.0), cell_height), 3);
    assert_eq!(pixels_to_scroll_lines(px(-40.0), cell_height), -2);
    assert_eq!(pixels_to_scroll_lines(px(10.0), cell_height), 1);
    assert_eq!(pixels_to_scroll_lines(px(-10.0), cell_height), -1);

    // Test clamping
    assert_eq!(pixels_to_scroll_lines(px(300.0), cell_height), 10);
    assert_eq!(pixels_to_scroll_lines(px(-300.0), cell_height), -10);
}

#[test]
fn test_scroll_to_arrow_keys_limit() {
    let mode = TermMode::empty();

    // Very large scroll should be limited to 5 lines
    let bytes = scroll_to_arrow_keys(100, mode);
    let expected = b"\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A";
    assert_eq!(bytes, expected);

    let bytes = scroll_to_arrow_keys(-100, mode);
    let expected = b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B";
    assert_eq!(bytes, expected);
}

#[test]
fn test_mouse_motion_report_drag() {
    let point = AlacPoint::new(Line(5), Column(10));
    let mode = TermMode::MOUSE_DRAG;

    let bytes = mouse_motion_report(Some(MouseButton::Left), point, 0, mode);
    assert!(bytes.is_some());
    let seq = String::from_utf8(bytes.unwrap()).unwrap();
    // Left button (0) + 32 (motion flag) = 32, col 11, row 6
    assert_eq!(seq, "\x1b[<32;11;6M");
}

#[test]
fn test_mouse_motion_report_hover() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_MOTION;

    let bytes = mouse_motion_report(None, point, 0, mode);
    assert!(bytes.is_some());
    let seq = String::from_utf8(bytes.unwrap()).unwrap();
    // No button (3) + 32 (motion flag) = 35, col 1, row 1
    assert_eq!(seq, "\x1b[<35;1;1M");
}

#[test]
fn test_mouse_motion_report_hover_not_in_drag_mode() {
    // MOUSE_DRAG (1002) does NOT report hover-only motion
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_DRAG;

    let bytes = mouse_motion_report(None, point, 0, mode);
    assert!(bytes.is_none());
}

#[test]
fn test_mouse_motion_report_click_mode_no_motion() {
    // MOUSE_REPORT_CLICK alone does NOT report any motion
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_REPORT_CLICK;

    let bytes = mouse_motion_report(Some(MouseButton::Left), point, 0, mode);
    assert!(bytes.is_none());
}

#[test]
fn test_mouse_motion_report_with_modifiers() {
    let point = AlacPoint::new(Line(0), Column(0));
    let mode = TermMode::MOUSE_DRAG;
    let modifiers = encode_modifiers(true, false, false); // Shift = 4

    let bytes = mouse_motion_report(Some(MouseButton::Left), point, modifiers, mode);
    assert!(bytes.is_some());
    let seq = String::from_utf8(bytes.unwrap()).unwrap();
    // Left (0) + 32 (motion) + 4 (shift) = 36
    assert_eq!(seq, "\x1b[<36;1;1M");
}
