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
mod tests;
