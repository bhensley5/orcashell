//! Theme variants for OrcaShell syntax highlighting.

use std::sync::OnceLock;

use orcashell_store::ThemeId;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

static DARK_THEME: OnceLock<Theme> = OnceLock::new();
static BLACK_THEME: OnceLock<Theme> = OnceLock::new();
static LIGHT_THEME: OnceLock<Theme> = OnceLock::new();
static SEPIA_THEME: OnceLock<Theme> = OnceLock::new();

#[derive(Clone, Copy)]
struct SyntaxPalette {
    foreground: u32,
    background: u32,
    muted: u32,
    subtle: u32,
    keyword: u32,
    attribute: u32,
    string: u32,
    number: u32,
    type_name: u32,
    function: u32,
    macro_name: u32,
}

pub fn orca_syntax_theme(theme_id: ThemeId) -> &'static Theme {
    match theme_id {
        ThemeId::Dark => DARK_THEME.get_or_init(|| build_theme(dark_palette(), "Orca Brutalism Dark")),
        ThemeId::Black => {
            BLACK_THEME.get_or_init(|| build_theme(black_palette(), "Orca Brutalism Black"))
        }
        ThemeId::Light => {
            LIGHT_THEME.get_or_init(|| build_theme(light_palette(), "Orca Brutalism Light"))
        }
        ThemeId::Sepia => {
            SEPIA_THEME.get_or_init(|| build_theme(sepia_palette(), "Orca Brutalism Sepia"))
        }
    }
}

fn dark_palette() -> SyntaxPalette {
    SyntaxPalette {
        foreground: 0xD8DAE0,
        background: 0x12151C,
        muted: 0x9499A8,
        subtle: 0x5C6070,
        keyword: 0x5E9BFF,
        attribute: 0x6FDCCF,
        string: 0x7EFFC1,
        number: 0xFFD97E,
        type_name: 0xB87EFF,
        function: 0x7EE8FA,
        macro_name: 0xFF7E9D,
    }
}

fn light_palette() -> SyntaxPalette {
    SyntaxPalette {
        foreground: 0x263241,
        background: 0xF7F9FC,
        muted: 0x5B687C,
        subtle: 0x8B96A7,
        keyword: 0x3F78DE,
        attribute: 0x257E78,
        string: 0x2D8F69,
        number: 0xA67621,
        type_name: 0x8C67D4,
        function: 0x257E78,
        macro_name: 0xC95E79,
    }
}

fn black_palette() -> SyntaxPalette {
    SyntaxPalette {
        foreground: 0xE1E6EF,
        background: 0x000000,
        muted: 0x9AA3B3,
        subtle: 0x596274,
        keyword: 0x6AA7FF,
        attribute: 0x78E5D8,
        string: 0x89FFC8,
        number: 0xFFE08F,
        type_name: 0xC08AFF,
        function: 0x88EDFF,
        macro_name: 0xFF8AA5,
    }
}

fn sepia_palette() -> SyntaxPalette {
    SyntaxPalette {
        foreground: 0x47382A,
        background: 0xFBF5EA,
        muted: 0x72604D,
        subtle: 0xA18D76,
        keyword: 0x5A82AD,
        attribute: 0x4A8D80,
        string: 0x64895A,
        number: 0xB48636,
        type_name: 0x8B6FAF,
        function: 0x4A8D80,
        macro_name: 0xBF715E,
    }
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

fn build_theme(palette: SyntaxPalette, name: &str) -> Theme {
    let settings = ThemeSettings {
        foreground: Some(color_from_hex(palette.foreground)),
        background: Some(color_from_hex(palette.background)),
        ..Default::default()
    };

    let scopes = vec![
        item("keyword", palette.keyword),
        item("storage.type", palette.keyword),
        item("storage.modifier", palette.keyword),
        item("string", palette.string),
        item("comment", palette.muted),
        item("comment.documentation", palette.subtle),
        item("entity.name.type", palette.type_name),
        item("support.type", palette.type_name),
        item("storage.type.primitive", palette.type_name),
        item("constant.numeric", palette.number),
        item("constant.language", palette.number),
        item("constant.character", palette.number),
        item("entity.name.function", palette.function),
        item("support.function", palette.function),
        item("meta.function-call", palette.function),
        item("entity.name.tag", palette.keyword),
        item("entity.other.attribute-name", palette.attribute),
        item("punctuation", palette.muted),
        item("keyword.operator", palette.muted),
        item("variable", palette.foreground),
        item("variable.parameter", palette.foreground),
        item("entity.name.function.macro", palette.macro_name),
        item("meta.annotation", palette.attribute),
        item("punctuation.definition.annotation", palette.attribute),
        item("constant.character.escape", palette.number),
        item("entity.name.namespace", palette.type_name),
        item("entity.name.module", palette.type_name),
    ];

    Theme {
        name: Some(name.to_string()),
        author: Some("OrcaShell".to_string()),
        settings,
        scopes,
    }
}

#[cfg(test)]
mod tests {
    use super::orca_syntax_theme;
    use orcashell_store::ThemeId;

    #[test]
    fn all_theme_variants_build() {
        for theme_id in [ThemeId::Dark, ThemeId::Black, ThemeId::Light, ThemeId::Sepia] {
            let theme = orca_syntax_theme(theme_id);
            assert!(theme.settings.foreground.is_some());
            assert!(theme.settings.background.is_some());
            assert!(!theme.scopes.is_empty());
        }
    }
}
