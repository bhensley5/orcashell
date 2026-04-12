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
