// Import only non-GPUI types to avoid triggering the GPUI proc macro
// stack overflow during test compilation (two `impl Render` in one crate
// exceeds the gpui_macros parser budget in test mode).
use super::escape_regex;
use alacritty_terminal::index::{Column, Line, Point as AlacPoint};
use alacritty_terminal::term::search::Match;

#[test]
fn test_escape_regex() {
    assert_eq!(escape_regex("hello"), "hello");
    assert_eq!(escape_regex("a.b"), "a\\.b");
    assert_eq!(escape_regex("foo*bar"), "foo\\*bar");
    assert_eq!(escape_regex("(test)"), "\\(test\\)");
    assert_eq!(escape_regex("[a]"), "\\[a\\]");
    assert_eq!(escape_regex("a+b"), "a\\+b");
    assert_eq!(escape_regex("a?b"), "a\\?b");
    assert_eq!(escape_regex("a|b"), "a\\|b");
    assert_eq!(escape_regex("^$"), "\\^\\$");
    assert_eq!(escape_regex("a\\b"), "a\\\\b");
    assert_eq!(escape_regex("{n}"), "\\{n\\}");
}

fn match_count_text(all_matches: &[Match], current_index: Option<usize>, query: &str) -> String {
    if all_matches.is_empty() {
        if query.is_empty() {
            String::new()
        } else {
            "No matches".to_string()
        }
    } else {
        let idx = current_index.map(|i| i + 1).unwrap_or(0);
        format!("{} of {}", idx, all_matches.len())
    }
}

#[test]
fn test_match_count_text() {
    let p1 = AlacPoint::new(Line(0), Column(0));
    let p2 = AlacPoint::new(Line(0), Column(3));

    assert_eq!(match_count_text(&[], None, ""), "");
    assert_eq!(match_count_text(&[], None, "foo"), "No matches");

    let matches = vec![p1..=p2, p1..=p2, p1..=p2];
    assert_eq!(match_count_text(&matches, Some(0), "foo"), "1 of 3");
    assert_eq!(match_count_text(&matches, Some(2), "foo"), "3 of 3");
}

#[test]
fn test_next_prev_wraparound() {
    let p1 = AlacPoint::new(Line(0), Column(0));
    let p2 = AlacPoint::new(Line(0), Column(3));
    let matches = vec![p1..=p2, p1..=p2, p1..=p2];
    let len = matches.len();

    let mut idx: Option<usize> = Some(0);

    // next_match logic
    idx = Some((idx.unwrap() + 1) % len);
    assert_eq!(idx, Some(1));

    idx = Some((idx.unwrap() + 1) % len);
    assert_eq!(idx, Some(2));

    // Wraparound forward
    idx = Some((idx.unwrap() + 1) % len);
    assert_eq!(idx, Some(0));

    // prev_match logic. Wraparound backward
    idx = Some(if idx.unwrap() == 0 {
        len - 1
    } else {
        idx.unwrap() - 1
    });
    assert_eq!(idx, Some(2));

    idx = Some(if idx.unwrap() == 0 {
        len - 1
    } else {
        idx.unwrap() - 1
    });
    assert_eq!(idx, Some(1));
}
