//! Orca Brutalism theme token system.
//!
//! All colors in the app flow through this module. UI code references these
//! constants directly (`theme::BONE`, `theme::ORCA_BLUE`, etc.). The `OrcaTheme`
//! struct collects them for future theme switching.

// ── Surfaces ──────────────────────────────────────────────────────────────

/// Window background, deepest layer.
pub const ABYSS: u32 = 0x1C1F26;
/// Terminal background, pane fill.
pub const DEEP: u32 = 0x12151C;
/// Active pane, hover states.
pub const CURRENT: u32 = 0x1C2028;
/// Selected states, floating panels.
pub const SURFACE: u32 = 0x262A34;

// ── Text ──────────────────────────────────────────────────────────────────

/// Brightest - active indicators, focused element text.
pub const PATCH: u32 = 0xE8EAF0;
/// Primary text - headlines, body, terminal foreground default.
pub const BONE: u32 = 0xD8DAE0;
/// Secondary text - metadata, timestamps, inactive tabs.
pub const FOG: u32 = 0x9499A8;
/// Tertiary - placeholders, disabled text, subtle dividers.
pub const SLATE: u32 = 0x5C6070;

// ── Accents ───────────────────────────────────────────────────────────────

/// Primary accent - focused pane, active task, primary actions.
pub const ORCA_BLUE: u32 = 0x5E9BFF;
/// Ocean attention accent - informational/attention states.
pub const SEAFOAM: u32 = 0x6FDCCF;
/// Success - task complete, agent running.
pub const STATUS_GREEN: u32 = 0x7EFFC1;
/// Error - task failed, agent crashed.
pub const STATUS_CORAL: u32 = 0xFF7E9D;
/// Warning - merge conflict, agent blocked.
pub const STATUS_AMBER: u32 = 0xFFD97E;

// ── Borders ───────────────────────────────────────────────────────────────

/// Default border - muted, structural.
pub const BORDER_DEFAULT: u32 = 0x2A2E3A;
/// Emphasis border - visible but not dominant.
pub const BORDER_EMPHASIS: u32 = 0x3A4050;

// ── Shadows ───────────────────────────────────────────────────────────────

/// Hard shadow color (brutalist directional shadows).
pub const SHADOW: u32 = 0x06080C;

// ── Platform: Windows window controls ────────────────────────────────────

/// Windows close button hover background (Windows-standard red).
pub const WIN_CLOSE_HOVER: u32 = 0xE81123;
/// Windows close button hover text (PATCH - brightest orca white, not pure #FFFFFF).
pub const WIN_CLOSE_HOVER_TEXT: u32 = PATCH;

// ── ANSI Terminal Palette (Pastel Neons) ──────────────────────────────────

/// 16-color ANSI palette: indices 0–7 normal, 8–15 bright.
pub const ANSI: [u32; 16] = [
    0x0A0C10, // 0  Black (darker than UI ABYSS for correct terminal rendering)
    0xFF7E9D, // 1  Neon Coral
    0x7EFFC1, // 2  Neon Mint
    0xFFD97E, // 3  Neon Amber
    0x5E9BFF, // 4  Orca Blue
    0xB87EFF, // 5  Neon Lavender
    0x7EE8FA, // 6  Neon Cyan
    0xD8DAE0, // 7  Bone
    0x5C6070, // 8  Slate (bright black)
    0xFFA0B8, // 9  Bright Coral
    0xA0FFD6, // 10 Bright Mint
    0xFFE5A0, // 11 Bright Amber
    0x82B4FF, // 12 Bright Blue
    0xCCA0FF, // 13 Bright Lavender
    0xA0F0FF, // 14 Bright Cyan
    0xE8EAF0, // 15 Patch (bright white)
];

// ── Helpers ───────────────────────────────────────────────────────────────

/// Decompose a 0xRRGGBB token into (r, g, b) channel bytes.
pub const fn rgb_channels(hex: u32) -> (u8, u8, u8) {
    (
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// Combine a 0xRRGGBB token with an alpha byte (0–255) into a 0xRRGGBBAA value
/// suitable for `gpui::rgba()`.
pub const fn with_alpha(hex: u32, alpha: u8) -> u32 {
    (hex << 8) | (alpha as u32)
}

// ── OrcaTheme struct (for future theme switching) ─────────────────────────

/// Collects all theme tokens into a single struct. `Default` returns the
/// canonical Orca Brutalism theme. Future phases can swap this for
/// user-selectable themes.
#[derive(Debug, Clone)]
pub struct OrcaTheme {
    // Surfaces
    pub abyss: u32,
    pub deep: u32,
    pub current: u32,
    pub surface: u32,

    // Text
    pub patch: u32,
    pub bone: u32,
    pub fog: u32,
    pub slate: u32,

    // Accents
    pub orca_blue: u32,
    pub seafoam: u32,
    pub status_green: u32,
    pub status_coral: u32,
    pub status_amber: u32,

    // Borders
    pub border_default: u32,
    pub border_emphasis: u32,

    // Shadows
    pub shadow: u32,

    // ANSI terminal palette (normal 0–7, bright 8–15)
    pub ansi: [u32; 16],

    // Platform: Windows window controls
    pub win_close_hover: u32,
    pub win_close_hover_text: u32,

    // Terminal specific
    pub terminal_foreground: u32,
    pub terminal_background: u32,
    pub terminal_cursor: u32,
    pub terminal_selection: u32,
}

impl Default for OrcaTheme {
    fn default() -> Self {
        Self {
            abyss: ABYSS,
            deep: DEEP,
            current: CURRENT,
            surface: SURFACE,

            patch: PATCH,
            bone: BONE,
            fog: FOG,
            slate: SLATE,

            orca_blue: ORCA_BLUE,
            seafoam: SEAFOAM,
            status_green: STATUS_GREEN,
            status_coral: STATUS_CORAL,
            status_amber: STATUS_AMBER,

            border_default: BORDER_DEFAULT,
            border_emphasis: BORDER_EMPHASIS,

            shadow: SHADOW,

            win_close_hover: WIN_CLOSE_HOVER,
            win_close_hover_text: WIN_CLOSE_HOVER_TEXT,

            ansi: ANSI,

            terminal_foreground: BONE,
            terminal_background: DEEP,
            terminal_cursor: ORCA_BLUE,
            terminal_selection: ORCA_BLUE,
        }
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports to avoid pulling in GPUI types that blow the
    // gpui_macros proc-macro stack during test compilation.
    use super::{
        rgb_channels, with_alpha, OrcaTheme, ABYSS, ANSI, BONE, BORDER_DEFAULT, BORDER_EMPHASIS,
        CURRENT, DEEP, FOG, ORCA_BLUE, PATCH, SEAFOAM, SHADOW, SLATE, STATUS_AMBER, STATUS_CORAL,
        STATUS_GREEN, SURFACE, WIN_CLOSE_HOVER, WIN_CLOSE_HOVER_TEXT,
    };

    #[test]
    fn theme_default_matches_constants() {
        let t = OrcaTheme::default();
        assert_eq!(t.abyss, ABYSS);
        assert_eq!(t.deep, DEEP);
        assert_eq!(t.current, CURRENT);
        assert_eq!(t.surface, SURFACE);
        assert_eq!(t.bone, BONE);
        assert_eq!(t.fog, FOG);
        assert_eq!(t.slate, SLATE);
        assert_eq!(t.patch, PATCH);
        assert_eq!(t.orca_blue, ORCA_BLUE);
        assert_eq!(t.seafoam, SEAFOAM);
        assert_eq!(t.ansi, ANSI);
    }

    #[test]
    fn no_pure_black_or_white_in_tokens() {
        // The orca spectrum rule: no 0x000000 or 0xFFFFFF
        let all_colors = [
            ABYSS,
            DEEP,
            CURRENT,
            SURFACE,
            PATCH,
            BONE,
            FOG,
            SLATE,
            ORCA_BLUE,
            SEAFOAM,
            STATUS_GREEN,
            STATUS_CORAL,
            STATUS_AMBER,
            BORDER_DEFAULT,
            BORDER_EMPHASIS,
            SHADOW,
            WIN_CLOSE_HOVER,
            WIN_CLOSE_HOVER_TEXT,
        ];
        for color in &all_colors {
            assert_ne!(*color, 0x000000, "Found pure black in theme tokens");
            assert_ne!(*color, 0xFFFFFF, "Found pure white in theme tokens");
        }
        for (i, color) in ANSI.iter().enumerate() {
            assert_ne!(*color, 0x000000, "ANSI[{}] is pure black", i);
            assert_ne!(*color, 0xFFFFFF, "ANSI[{}] is pure white", i);
        }
    }

    #[test]
    fn rgb_channels_correctness() {
        assert_eq!(rgb_channels(0xFF8040), (0xFF, 0x80, 0x40));
        assert_eq!(rgb_channels(0x000000), (0, 0, 0));
        assert_eq!(rgb_channels(BONE), (0xD8, 0xDA, 0xE0));
    }

    #[test]
    fn with_alpha_correctness() {
        assert_eq!(with_alpha(0xD8DAE0, 0xFF), 0xD8DAE0FF);
        assert_eq!(with_alpha(0x5E9BFF, 0x40), 0x5E9BFF40);
    }

    #[test]
    fn syntax_theme_colors_match_canonical() {
        use orcashell_syntax::theme as syn;
        assert_eq!(syn::BONE, BONE);
        assert_eq!(syn::FOG, FOG);
        assert_eq!(syn::SLATE, SLATE);
        assert_eq!(syn::ORCA_BLUE, ORCA_BLUE);
        assert_eq!(syn::SEAFOAM, SEAFOAM);
        assert_eq!(syn::STATUS_GREEN, STATUS_GREEN);
        assert_eq!(syn::STATUS_AMBER, STATUS_AMBER);
        assert_eq!(syn::DEEP, DEEP);
        assert_eq!(syn::NEON_LAVENDER, ANSI[5]);
        assert_eq!(syn::NEON_CYAN, ANSI[6]);
        assert_eq!(syn::NEON_CORAL, ANSI[1]);
    }
}
