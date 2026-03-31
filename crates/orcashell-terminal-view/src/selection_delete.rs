//! Selection-aware delete for shell prompts.
//!
//! When the user has selected text within the shell input region and presses
//! Delete or Backspace, this module computes the PTY keystrokes needed to
//! delete the selected text by moving the cursor and sending backspaces.

use alacritty_terminal::index::Point;
use alacritty_terminal::selection::SelectionRange;
use orcashell_session::InputRegion;

/// Compute the byte sequence to delete selected text in the shell input line.
///
/// Returns `None` if:
/// - The selection spans multiple lines (too complex for keystroke synthesis)
/// - The selection is not fully within the input region
/// - The cursor is on a different line than the selection
pub fn compute_delete_keystrokes(
    selection: &SelectionRange,
    input_region: &InputRegion,
    cursor_position: Point,
) -> Option<Vec<u8>> {
    // Only handle single-line selections.
    if selection.start.line != selection.end.line {
        return None;
    }

    if !is_within_input_region(selection, input_region) {
        return None;
    }

    // Cursor must be on the same line as the selection.
    if cursor_position.line != selection.start.line {
        return None;
    }

    let sel_start_col = selection.start.column.0;
    let sel_end_col = selection.end.column.0;
    let cursor_col = cursor_position.column.0;

    // Target: position cursor just past the end of the selection.
    let target_col = sel_end_col + 1;

    let mut bytes = Vec::new();

    // Move cursor to target_col.
    if cursor_col < target_col {
        for _ in 0..(target_col - cursor_col) {
            bytes.extend_from_slice(b"\x1b[C"); // Right arrow
        }
    } else if cursor_col > target_col {
        for _ in 0..(cursor_col - target_col) {
            bytes.extend_from_slice(b"\x1b[D"); // Left arrow
        }
    }

    // Send backspace for each character in selection (inclusive range).
    let num_chars = sel_end_col - sel_start_col + 1;
    bytes.extend(std::iter::repeat_n(0x7fu8, num_chars));

    Some(bytes)
}

/// Check if a selection range is fully contained within the input region.
fn is_within_input_region(selection: &SelectionRange, input_region: &InputRegion) -> bool {
    // Selection start must be >= input region start.
    let start_ok = selection.start.line > input_region.start.line
        || (selection.start.line == input_region.start.line
            && selection.start.column >= input_region.start.column);

    // Selection end must be <= input region end.
    let end_ok = selection.end.line < input_region.end.line
        || (selection.end.line == input_region.end.line
            && selection.end.column <= input_region.end.column);

    start_ok && end_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

    fn pt(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    fn sel(start_line: i32, start_col: usize, end_line: i32, end_col: usize) -> SelectionRange {
        SelectionRange::new(pt(start_line, start_col), pt(end_line, end_col), false)
    }

    fn region(start_line: i32, start_col: usize, end_line: i32, end_col: usize) -> InputRegion {
        InputRegion {
            start: pt(start_line, start_col),
            end: pt(end_line, end_col),
        }
    }

    #[test]
    fn delete_selection_cursor_at_end() {
        // Selection: cols 2-5, cursor at col 6 (just past selection end).
        let selection = sel(0, 2, 0, 5);
        let input = region(0, 0, 0, 20);
        let cursor = pt(0, 6);

        let bytes = compute_delete_keystrokes(&selection, &input, cursor).unwrap();
        // No cursor movement needed (already at col 6 = end+1).
        // 4 backspaces (cols 2,3,4,5).
        assert_eq!(bytes, vec![0x7f, 0x7f, 0x7f, 0x7f]);
    }

    #[test]
    fn delete_selection_cursor_before() {
        // Selection: cols 5-8, cursor at col 3.
        let selection = sel(0, 5, 0, 8);
        let input = region(0, 0, 0, 20);
        let cursor = pt(0, 3);

        let bytes = compute_delete_keystrokes(&selection, &input, cursor).unwrap();
        // Move right 6 positions (col 3 → col 9).
        let mut expected = Vec::new();
        for _ in 0..6 {
            expected.extend_from_slice(b"\x1b[C");
        }
        // 4 backspaces (cols 5,6,7,8).
        for _ in 0..4 {
            expected.push(0x7f);
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn delete_selection_cursor_past_end() {
        // Selection: cols 2-5, cursor at col 10 (well past selection).
        let selection = sel(0, 2, 0, 5);
        let input = region(0, 0, 0, 20);
        let cursor = pt(0, 10);

        let bytes = compute_delete_keystrokes(&selection, &input, cursor).unwrap();
        // Move left 4 positions (col 10 → col 6).
        let mut expected = Vec::new();
        for _ in 0..4 {
            expected.extend_from_slice(b"\x1b[D");
        }
        // 4 backspaces.
        for _ in 0..4 {
            expected.push(0x7f);
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn delete_selection_cursor_in_middle() {
        // Selection: cols 3-7, cursor at col 5 (inside selection).
        let selection = sel(0, 3, 0, 7);
        let input = region(0, 0, 0, 20);
        let cursor = pt(0, 5);

        let bytes = compute_delete_keystrokes(&selection, &input, cursor).unwrap();
        // Move right 3 positions (col 5 → col 8).
        let mut expected = Vec::new();
        for _ in 0..3 {
            expected.extend_from_slice(b"\x1b[C");
        }
        // 5 backspaces (cols 3,4,5,6,7).
        for _ in 0..5 {
            expected.push(0x7f);
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn reject_multiline_selection() {
        let selection = sel(0, 5, 1, 3);
        let input = region(0, 0, 1, 20);
        let cursor = pt(1, 4);

        assert!(compute_delete_keystrokes(&selection, &input, cursor).is_none());
    }

    #[test]
    fn reject_selection_outside_input_region() {
        // Selection starts before input region.
        let selection = sel(0, 0, 0, 5);
        let input = region(0, 3, 0, 20);
        let cursor = pt(0, 6);

        assert!(compute_delete_keystrokes(&selection, &input, cursor).is_none());
    }

    #[test]
    fn reject_selection_past_input_region() {
        let selection = sel(0, 5, 0, 25);
        let input = region(0, 0, 0, 20);
        let cursor = pt(0, 10);

        assert!(compute_delete_keystrokes(&selection, &input, cursor).is_none());
    }

    #[test]
    fn reject_cursor_on_different_line() {
        let selection = sel(0, 2, 0, 5);
        let input = region(0, 0, 0, 20);
        let cursor = pt(1, 3);

        assert!(compute_delete_keystrokes(&selection, &input, cursor).is_none());
    }

    #[test]
    fn is_within_input_region_at_boundaries() {
        let input = region(0, 5, 0, 20);

        // Exactly at boundaries.
        assert!(is_within_input_region(&sel(0, 5, 0, 20), &input));
        // Fully inside.
        assert!(is_within_input_region(&sel(0, 6, 0, 10), &input));
        // Start before.
        assert!(!is_within_input_region(&sel(0, 3, 0, 10), &input));
        // End after.
        assert!(!is_within_input_region(&sel(0, 6, 0, 25), &input));
    }
}
