use std::ops::RangeInclusive;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point as AlacPoint};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::Term;
use gpui::{Hsla, Modifiers, MouseButton};

const PLAIN_URL_REGEX: &str = r"https?://[A-Za-z0-9._~:/?#\[\]@!$&'()*+,;=%-]+";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DetectedLinkKind {
    Osc8,
    Plaintext,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DetectedLink {
    pub range: RangeInclusive<AlacPoint>,
    pub uri: String,
    pub kind: DetectedLinkKind,
    pub underline_color: Option<Hsla>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleHoveredLink {
    pub range: RangeInclusive<AlacPoint>,
    pub underline_color: Option<Hsla>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlaintextLinkMatcher {
    regex: RegexSearch,
}

impl PlaintextLinkMatcher {
    pub(crate) fn new() -> Self {
        Self {
            regex: RegexSearch::new(PLAIN_URL_REGEX).expect("plain URL regex should compile"),
        }
    }

    pub(crate) fn detect_at_point<T>(
        &mut self,
        term: &Term<T>,
        point: AlacPoint,
    ) -> Option<DetectedLink> {
        let start = term.line_search_left(point);
        let end = term.line_search_right(point);

        RegexIter::new(start, end, Direction::Right, term, &mut self.regex)
            .filter_map(|candidate| trim_plaintext_match(term, &candidate))
            .find(|link| range_contains_point(&link.range, point))
    }
}

pub(crate) fn hyperlink_activation_modifier_pressed(modifiers: Modifiers) -> bool {
    #[cfg(target_os = "macos")]
    {
        modifiers.platform
    }

    #[cfg(target_os = "linux")]
    {
        modifiers.control
    }

    #[cfg(target_os = "windows")]
    {
        modifiers.control
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        modifiers.platform
    }
}

pub(crate) fn is_hyperlink_activation_click(modifiers: Modifiers, button: MouseButton) -> bool {
    button == MouseButton::Left && hyperlink_activation_modifier_pressed(modifiers)
}

pub(crate) fn is_supported_hyperlink_uri(uri: &str) -> bool {
    uri.starts_with("http://") || uri.starts_with("https://")
}

pub(crate) fn open_hyperlink_uri(uri: &str) -> bool {
    if !is_supported_hyperlink_uri(uri) {
        return false;
    }
    orcashell_platform::open_url(uri)
}

pub(crate) fn build_visible_hovered_link(
    link: Option<&DetectedLink>,
    display_offset: usize,
    screen_lines: usize,
) -> Option<VisibleHoveredLink> {
    let link = link?;
    if link.kind != DetectedLinkKind::Plaintext {
        return None;
    }

    let top_buffer_line = -(display_offset as i32);
    let bottom_buffer_line = screen_lines as i32 - 1 - display_offset as i32;
    let start_line = link.range.start().line.0;
    let end_line = link.range.end().line.0;

    if start_line <= bottom_buffer_line && end_line >= top_buffer_line {
        Some(VisibleHoveredLink {
            range: link.range.clone(),
            underline_color: link.underline_color,
        })
    } else {
        None
    }
}

pub(crate) fn detect_link_at_point<T>(
    term: &Term<T>,
    point: AlacPoint,
    matcher: &mut PlaintextLinkMatcher,
) -> Option<DetectedLink> {
    if let Some(link) = term.grid()[point].hyperlink() {
        let uri = link.uri();
        if is_supported_hyperlink_uri(uri) {
            return Some(DetectedLink {
                range: point..=point,
                uri: uri.to_string(),
                kind: DetectedLinkKind::Osc8,
                underline_color: None,
            });
        }
    }

    matcher.detect_at_point(term, point)
}

pub(crate) fn buffer_point_for_cell<T>(term: &Term<T>, cell: AlacPoint) -> AlacPoint {
    let cols = term.grid().columns();
    let lines = term.grid().screen_lines();
    let display_offset = term.grid().display_offset() as i32;

    let col = cell.column.0.min(cols.saturating_sub(1));
    let line = cell.line.0.max(0) as usize;
    let clamped_line = line.min(lines.saturating_sub(1)) as i32;

    AlacPoint::new(Line(clamped_line - display_offset), Column(col))
}

pub(crate) fn range_contains_point(range: &RangeInclusive<AlacPoint>, point: AlacPoint) -> bool {
    *range.start() <= point && point <= *range.end()
}

fn trim_plaintext_match<T>(term: &Term<T>, candidate: &Match) -> Option<DetectedLink> {
    let mut uri = term.bounds_to_string(*candidate.start(), *candidate.end());
    let trim_count = trailing_trim_count(&uri);
    let end = if trim_count == 0 {
        *candidate.end()
    } else {
        if trim_count >= uri.len() {
            return None;
        }
        uri.truncate(uri.len() - trim_count);
        candidate.end().sub(term, Boundary::None, trim_count)
    };

    if uri.is_empty() || !is_supported_hyperlink_uri(&uri) {
        return None;
    }

    Some(DetectedLink {
        range: *candidate.start()..=end,
        uri,
        kind: DetectedLinkKind::Plaintext,
        underline_color: None,
    })
}

fn trailing_trim_count(uri: &str) -> usize {
    let mut trim_count = 0;
    let mut trimmed = uri;

    while let Some(last) = trimmed.as_bytes().last().copied() {
        let should_trim = match last {
            b'.' | b',' | b';' | b':' | b'!' | b'?' => true,
            b')' => unmatched_closing(trimmed, b'(', b')'),
            b']' => unmatched_closing(trimmed, b'[', b']'),
            b'}' => unmatched_closing(trimmed, b'{', b'}'),
            _ => false,
        };

        if !should_trim {
            break;
        }

        trim_count += 1;
        trimmed = &trimmed[..trimmed.len() - 1];
    }

    trim_count
}

fn unmatched_closing(uri: &str, open: u8, close: u8) -> bool {
    let opens = uri.as_bytes().iter().filter(|&&b| b == open).count();
    let closes = uri.as_bytes().iter().filter(|&&b| b == close).count();
    closes > opens
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::term::{Config, Term};
    use orcashell_session::dimensions::TermDimensions;

    fn mock_term(content: &str) -> Term<VoidListener> {
        let lines: Vec<&str> = content.split('\n').collect();
        let num_cols = lines
            .iter()
            .map(|line| line.chars().take_while(|c| *c != '\r').count())
            .max()
            .unwrap_or(0)
            .max(1);

        let size = TermDimensions::new(num_cols, lines.len().max(1));
        let mut term = Term::new(Config::default(), &size, VoidListener);

        for (line_idx, text) in lines.iter().enumerate() {
            let line = Line(line_idx as i32);
            if !text.ends_with('\r') && line_idx + 1 != lines.len() {
                term.grid_mut()[line][Column(num_cols - 1)]
                    .flags
                    .insert(Flags::WRAPLINE);
            }

            for (col_idx, ch) in text.chars().take_while(|c| *c != '\r').enumerate() {
                term.grid_mut()[line][Column(col_idx)].c = ch;
            }
        }

        term
    }

    #[test]
    fn only_left_click_activates_hyperlinks() {
        let modifiers = Modifiers {
            control: true,
            platform: true,
            ..Modifiers::default()
        };

        assert!(is_hyperlink_activation_click(modifiers, MouseButton::Left));
        assert!(!is_hyperlink_activation_click(
            modifiers,
            MouseButton::Right
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_command_click_and_open() {
        let command = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        let control = Modifiers {
            control: true,
            ..Modifiers::default()
        };

        assert!(hyperlink_activation_modifier_pressed(command));
        assert!(!hyperlink_activation_modifier_pressed(control));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_control_click_and_xdg_open() {
        let control = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let command = Modifiers {
            platform: true,
            ..Modifiers::default()
        };

        assert!(hyperlink_activation_modifier_pressed(control));
        assert!(!hyperlink_activation_modifier_pressed(command));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_control_click() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let platform = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert!(hyperlink_activation_modifier_pressed(ctrl));
        assert!(!hyperlink_activation_modifier_pressed(platform));
    }

    #[test]
    fn only_http_and_https_uris_are_supported() {
        assert!(is_supported_hyperlink_uri("http://example.com"));
        assert!(is_supported_hyperlink_uri("https://example.com"));
        assert!(!is_supported_hyperlink_uri("file:///tmp/test.txt"));
        assert!(!is_supported_hyperlink_uri("mailto:test@example.com"));
        assert!(!is_supported_hyperlink_uri("javascript:alert(1)"));
    }

    #[test]
    fn trims_trailing_punctuation_from_plaintext_links() {
        assert_eq!(trailing_trim_count("https://example.com/path."), 1);
        assert_eq!(trailing_trim_count("https://example.com/path?)"), 2);
        assert_eq!(trailing_trim_count("https://example.com/path))"), 2);
        assert_eq!(trailing_trim_count("https://example.com/path(test)"), 0);
    }

    #[test]
    fn detects_plaintext_link_and_trims_suffix() {
        let mut matcher = PlaintextLinkMatcher::new();
        let term = mock_term("check https://localhost:3000/test). now");
        let point = AlacPoint::new(Line(0), Column(20));
        let link = matcher
            .detect_at_point(&term, point)
            .expect("link should be detected");

        assert_eq!(link.uri, "https://localhost:3000/test");
        assert_eq!(link.kind, DetectedLinkKind::Plaintext);
        assert!(range_contains_point(&link.range, point));
        assert_eq!(link.range.end(), &AlacPoint::new(Line(0), Column(32)));
        assert!(!range_contains_point(
            &link.range,
            AlacPoint::new(Line(0), Column(33))
        ));
    }

    #[test]
    fn detects_plaintext_link_across_wrapped_lines() {
        let mut matcher = PlaintextLinkMatcher::new();
        let term = mock_term("https://localhost:3000/ab\nc?x=1");
        let point = AlacPoint::new(Line(1), Column(2));
        let link = matcher
            .detect_at_point(&term, point)
            .expect("wrapped link should be found");

        assert_eq!(link.uri, "https://localhost:3000/abc?x=1");
        assert_eq!(link.range.start(), &AlacPoint::new(Line(0), Column(0)));
        assert_eq!(link.range.end(), &AlacPoint::new(Line(1), Column(4)));
    }

    #[test]
    fn visible_hovered_link_only_emits_for_plaintext_links() {
        let plain = DetectedLink {
            range: AlacPoint::new(Line(0), Column(2))..=AlacPoint::new(Line(0), Column(5)),
            uri: "https://example.com".to_string(),
            kind: DetectedLinkKind::Plaintext,
            underline_color: None,
        };
        let osc8 = DetectedLink {
            range: AlacPoint::new(Line(0), Column(2))..=AlacPoint::new(Line(0), Column(2)),
            uri: "https://example.com".to_string(),
            kind: DetectedLinkKind::Osc8,
            underline_color: None,
        };

        assert!(build_visible_hovered_link(Some(&plain), 0, 24).is_some());
        assert!(build_visible_hovered_link(Some(&osc8), 0, 24).is_none());
    }
}
