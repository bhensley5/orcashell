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
mod tests;
