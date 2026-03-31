//! Custom Orca Brutalism syntax highlighting theme for syntect.
//!
//! Maps TextMate scopes to the Orca pastel neon palette. Color constants are
//! duplicated from `orcashell-ui::theme` to avoid a dependency on the UI crate.
//! Canonical source: `crates/orcashell-ui/src/theme.rs`.

use std::sync::OnceLock;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

static ORCA_THEME: OnceLock<Theme> = OnceLock::new();

// ── Orca color constants (canonical source: orcashell-ui/src/theme.rs) ──────

pub const BONE: u32 = 0xD8DAE0;
pub const FOG: u32 = 0x9499A8;
pub const SLATE: u32 = 0x5C6070;
pub const ORCA_BLUE: u32 = 0x5E9BFF;
pub const SEAFOAM: u32 = 0x6FDCCF;
pub const STATUS_GREEN: u32 = 0x7EFFC1;
pub const STATUS_AMBER: u32 = 0xFFD97E;
pub const NEON_LAVENDER: u32 = 0xB87EFF;
pub const NEON_CYAN: u32 = 0x7EE8FA;
pub const NEON_CORAL: u32 = 0xFF7E9D;
pub const DEEP: u32 = 0x12151C;

/// Returns the cached Orca syntax theme.
pub fn orca_syntax_theme() -> &'static Theme {
    ORCA_THEME.get_or_init(build_orca_theme)
}

fn color_from_hex(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xFF) as u8,
        g: ((hex >> 8) & 0xFF) as u8,
        b: (hex & 0xFF) as u8,
        a: 0xFF,
    }
}

fn scope(s: &str) -> ScopeSelectors {
    s.parse().expect("valid scope selector")
}

fn item(selector: &str, color: u32) -> ThemeItem {
    ThemeItem {
        scope: scope(selector),
        style: StyleModifier {
            foreground: Some(color_from_hex(color)),
            background: None,
            font_style: None,
        },
    }
}

fn build_orca_theme() -> Theme {
    let settings = ThemeSettings {
        foreground: Some(color_from_hex(BONE)),
        background: Some(color_from_hex(DEEP)),
        ..Default::default()
    };

    let scopes = vec![
        // Keywords & storage
        item("keyword", ORCA_BLUE),
        item("storage.type", ORCA_BLUE),
        item("storage.modifier", ORCA_BLUE),
        // Strings
        item("string", STATUS_GREEN),
        // Comments
        item("comment", FOG),
        item("comment.documentation", SLATE),
        // Types
        item("entity.name.type", NEON_LAVENDER),
        item("support.type", NEON_LAVENDER),
        item("storage.type.primitive", NEON_LAVENDER),
        // Numbers & language constants
        item("constant.numeric", STATUS_AMBER),
        item("constant.language", STATUS_AMBER),
        item("constant.character", STATUS_AMBER),
        // Functions
        item("entity.name.function", NEON_CYAN),
        item("support.function", NEON_CYAN),
        item("meta.function-call", NEON_CYAN),
        // HTML/JSX tags
        item("entity.name.tag", ORCA_BLUE),
        // Attributes
        item("entity.other.attribute-name", SEAFOAM),
        // Punctuation
        item("punctuation", FOG),
        // Operators
        item("keyword.operator", FOG),
        // Variables (default-ish, but explicit for clarity)
        item("variable", BONE),
        item("variable.parameter", BONE),
        // Macros / annotations / decorators
        item("entity.name.function.macro", NEON_CORAL),
        item("meta.annotation", SEAFOAM),
        item("punctuation.definition.annotation", SEAFOAM),
        // Escape sequences in strings
        item("constant.character.escape", STATUS_AMBER),
        // Module / namespace
        item("entity.name.namespace", NEON_LAVENDER),
        item("entity.name.module", NEON_LAVENDER),
    ];

    Theme {
        name: Some("Orca Brutalism".to_string()),
        author: Some("OrcaShell".to_string()),
        settings,
        scopes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_builds_without_panic() {
        let theme = orca_syntax_theme();
        assert_eq!(theme.name.as_deref(), Some("Orca Brutalism"));
        assert!(!theme.scopes.is_empty());
    }

    #[test]
    fn foreground_is_bone() {
        let theme = orca_syntax_theme();
        let fg = theme.settings.foreground.unwrap();
        assert_eq!(fg, color_from_hex(BONE));
    }

    #[test]
    fn color_from_hex_correctness() {
        let c = color_from_hex(0xFF8040);
        assert_eq!(c.r, 0xFF);
        assert_eq!(c.g, 0x80);
        assert_eq!(c.b, 0x40);
        assert_eq!(c.a, 0xFF);
    }
}
