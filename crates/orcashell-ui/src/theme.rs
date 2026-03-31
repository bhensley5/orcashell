//! OrcaShell runtime theme system.
//!
//! Theme selection is resolved from persisted settings plus the current
//! platform `WindowAppearance`. UI code reads the active tokens from the
//! `ResolvedTheme` global via [`active`].

use gpui::{App, Global, WindowAppearance};
use std::sync::{OnceLock, RwLock};

use crate::settings::{AppSettings, ThemeId, ThemeMode};

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

#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrcaTheme {
    pub ABYSS: u32,
    pub DEEP: u32,
    pub CURRENT: u32,
    pub SURFACE: u32,
    pub PATCH: u32,
    pub BONE: u32,
    pub FOG: u32,
    pub SLATE: u32,
    pub ORCA_BLUE: u32,
    pub SEAFOAM: u32,
    pub STATUS_GREEN: u32,
    pub STATUS_CORAL: u32,
    pub STATUS_AMBER: u32,
    pub BORDER_DEFAULT: u32,
    pub BORDER_EMPHASIS: u32,
    pub SHADOW: u32,
    pub ANSI: [u32; 16],
    pub WIN_CLOSE_HOVER: u32,
    pub WIN_CLOSE_HOVER_TEXT: u32,
    pub TERMINAL_FOREGROUND: u32,
    pub TERMINAL_BACKGROUND: u32,
    pub TERMINAL_CURSOR: u32,
    pub TERMINAL_SELECTION: u32,
}

impl OrcaTheme {
    pub fn dark() -> Self {
        Self {
            ABYSS: 0x1C1F26,
            DEEP: 0x12151C,
            CURRENT: 0x1C2028,
            SURFACE: 0x262A34,
            PATCH: 0xE8EAF0,
            BONE: 0xD8DAE0,
            FOG: 0x9499A8,
            SLATE: 0x5C6070,
            ORCA_BLUE: 0x5E9BFF,
            SEAFOAM: 0x6FDCCF,
            STATUS_GREEN: 0x7EFFC1,
            STATUS_CORAL: 0xFF7E9D,
            STATUS_AMBER: 0xFFD97E,
            BORDER_DEFAULT: 0x2A2E3A,
            BORDER_EMPHASIS: 0x3A4050,
            SHADOW: 0x06080C,
            ANSI: [
                0x0A0C10, 0xFF7E9D, 0x7EFFC1, 0xFFD97E, 0x5E9BFF, 0xB87EFF, 0x7EE8FA,
                0xD8DAE0, 0x5C6070, 0xFFA0B8, 0xA0FFD6, 0xFFE5A0, 0x82B4FF, 0xCCA0FF,
                0xA0F0FF, 0xE8EAF0,
            ],
            WIN_CLOSE_HOVER: 0xE81123,
            WIN_CLOSE_HOVER_TEXT: 0xE8EAF0,
            TERMINAL_FOREGROUND: 0xD8DAE0,
            TERMINAL_BACKGROUND: 0x12151C,
            TERMINAL_CURSOR: 0x5E9BFF,
            TERMINAL_SELECTION: 0x5E9BFF,
        }
    }

    pub fn black() -> Self {
        Self {
            ABYSS: 0x000000,
            DEEP: 0x000000,
            CURRENT: 0x090B0F,
            SURFACE: 0x11151B,
            PATCH: 0xF1F4FA,
            BONE: 0xE1E6EF,
            FOG: 0x9AA3B3,
            SLATE: 0x596274,
            ORCA_BLUE: 0x6AA7FF,
            SEAFOAM: 0x78E5D8,
            STATUS_GREEN: 0x89FFC8,
            STATUS_CORAL: 0xFF8AA5,
            STATUS_AMBER: 0xFFE08F,
            BORDER_DEFAULT: 0x191F28,
            BORDER_EMPHASIS: 0x252D39,
            SHADOW: 0x000000,
            ANSI: [
                0x000000, 0xFF8AA5, 0x89FFC8, 0xFFE08F, 0x6AA7FF, 0xC08AFF, 0x88EDFF,
                0xE1E6EF, 0x596274, 0xFFACC0, 0xADFFD9, 0xFFEAB1, 0x90BDFF, 0xD2AEFF,
                0xAEF3FF, 0xF1F4FA,
            ],
            WIN_CLOSE_HOVER: 0xE81123,
            WIN_CLOSE_HOVER_TEXT: 0xF1F4FA,
            TERMINAL_FOREGROUND: 0xE1E6EF,
            TERMINAL_BACKGROUND: 0x000000,
            TERMINAL_CURSOR: 0x6AA7FF,
            TERMINAL_SELECTION: 0x6AA7FF,
        }
    }

    pub fn light() -> Self {
        Self {
            ABYSS: 0xEEF2F7,
            DEEP: 0xF7F9FC,
            CURRENT: 0xE2E8F1,
            SURFACE: 0xD6DFEA,
            PATCH: 0x17202C,
            BONE: 0x263241,
            FOG: 0x5B687C,
            SLATE: 0x8B96A7,
            ORCA_BLUE: 0x3F78DE,
            SEAFOAM: 0x257E78,
            STATUS_GREEN: 0x2D8F69,
            STATUS_CORAL: 0xC95E79,
            STATUS_AMBER: 0xA67621,
            BORDER_DEFAULT: 0xC2CBD8,
            BORDER_EMPHASIS: 0xA7B5C8,
            SHADOW: 0xB8C2CF,
            ANSI: [
                0x263241, 0xC95E79, 0x2D8F69, 0xA67621, 0x3F78DE, 0x8C67D4, 0x257E78,
                0xBBC5D3, 0x5B687C, 0xDE7F97, 0x53AD86, 0xC59745, 0x6897EB, 0xAB84E3,
                0x4CABBA, 0xE2E8F1,
            ],
            WIN_CLOSE_HOVER: 0xE81123,
            WIN_CLOSE_HOVER_TEXT: 0xF7F9FC,
            TERMINAL_FOREGROUND: 0x263241,
            TERMINAL_BACKGROUND: 0xF7F9FC,
            TERMINAL_CURSOR: 0x3F78DE,
            TERMINAL_SELECTION: 0x3F78DE,
        }
    }

    pub fn sepia() -> Self {
        Self {
            ABYSS: 0xF3EBDD,
            DEEP: 0xFBF5EA,
            CURRENT: 0xE8DCC5,
            SURFACE: 0xDCC9AD,
            PATCH: 0x2F241B,
            BONE: 0x47382A,
            FOG: 0x72604D,
            SLATE: 0xA18D76,
            ORCA_BLUE: 0x5A82AD,
            SEAFOAM: 0x4A8D80,
            STATUS_GREEN: 0x64895A,
            STATUS_CORAL: 0xBF715E,
            STATUS_AMBER: 0xB48636,
            BORDER_DEFAULT: 0xC7B59B,
            BORDER_EMPHASIS: 0xB29C7C,
            SHADOW: 0xA79378,
            ANSI: [
                0x47382A, 0xBF715E, 0x64895A, 0xB48636, 0x5A82AD, 0x8B6FAF, 0x4A8D80,
                0xC8B79F, 0x72604D, 0xD28B78, 0x87A27D, 0xC9A158, 0x7A9BC1, 0xA58AC7,
                0x6AA79A, 0xE8DCC5,
            ],
            WIN_CLOSE_HOVER: 0xE81123,
            WIN_CLOSE_HOVER_TEXT: 0xFBF5EA,
            TERMINAL_FOREGROUND: 0x47382A,
            TERMINAL_BACKGROUND: 0xFBF5EA,
            TERMINAL_CURSOR: 0x5A82AD,
            TERMINAL_SELECTION: 0x5A82AD,
        }
    }
}

impl Default for OrcaTheme {
    fn default() -> Self {
        Self::dark()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeSelection {
    pub mode: ThemeMode,
    pub resolved_id: ThemeId,
    pub appearance: WindowAppearance,
}

#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    pub selection: ThemeSelection,
    pub theme: OrcaTheme,
}

impl Global for ResolvedTheme {}

#[derive(Debug, Clone, Copy)]
pub struct SystemAppearance(pub WindowAppearance);

impl Global for SystemAppearance {}

static CURRENT_THEME: OnceLock<RwLock<OrcaTheme>> = OnceLock::new();

fn current_theme_store() -> &'static RwLock<OrcaTheme> {
    CURRENT_THEME.get_or_init(|| RwLock::new(OrcaTheme::default()))
}

pub fn register_theme(cx: &mut App) {
    let appearance = cx.window_appearance();
    cx.set_global(SystemAppearance(appearance));

    let settings = cx.global::<AppSettings>().clone();
    let resolved = resolve_theme(&settings, appearance);
    *current_theme_store().write().expect("theme lock poisoned") = resolved.theme.clone();
    cx.set_global(resolved);
}

pub fn update_window_appearance(appearance: WindowAppearance, cx: &mut App) {
    cx.set_global(SystemAppearance(appearance));
    sync_from_settings(cx);
}

pub fn sync_from_settings(cx: &mut App) {
    let settings = cx.global::<AppSettings>().clone();
    let appearance = cx.global::<SystemAppearance>().0;
    let resolved = resolve_theme(&settings, appearance);
    *current_theme_store().write().expect("theme lock poisoned") = resolved.theme.clone();
    cx.set_global(resolved);
}

pub fn active(cx: &App) -> OrcaTheme {
    cx.global::<ResolvedTheme>().theme.clone()
}

pub fn current() -> OrcaTheme {
    current_theme_store()
        .read()
        .expect("theme lock poisoned")
        .clone()
}

pub fn active_selection(cx: &App) -> ThemeSelection {
    cx.global::<ResolvedTheme>().selection
}

pub fn resolve_theme(settings: &AppSettings, appearance: WindowAppearance) -> ResolvedTheme {
    let resolved_id = match settings.theme_mode {
        ThemeMode::Manual => settings.manual_theme,
        ThemeMode::System => match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => settings.system_light_theme,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => settings.system_dark_theme,
        },
    };

    let theme = match resolved_id {
        ThemeId::Dark => OrcaTheme::dark(),
        ThemeId::Black => OrcaTheme::black(),
        ThemeId::Light => OrcaTheme::light(),
        ThemeId::Sepia => OrcaTheme::sepia(),
    };

    ResolvedTheme {
        selection: ThemeSelection {
            mode: settings.theme_mode,
            resolved_id,
            appearance,
        },
        theme,
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_theme, rgb_channels, with_alpha, OrcaTheme};
    use crate::settings::{AppSettings, ThemeId, ThemeMode};
    use gpui::WindowAppearance;

    fn no_pure_black_or_white(theme: &OrcaTheme) {
        let all = [
            theme.ABYSS,
            theme.DEEP,
            theme.CURRENT,
            theme.SURFACE,
            theme.PATCH,
            theme.BONE,
            theme.FOG,
            theme.SLATE,
            theme.ORCA_BLUE,
            theme.SEAFOAM,
            theme.STATUS_GREEN,
            theme.STATUS_CORAL,
            theme.STATUS_AMBER,
            theme.BORDER_DEFAULT,
            theme.BORDER_EMPHASIS,
            theme.SHADOW,
            theme.WIN_CLOSE_HOVER_TEXT,
            theme.TERMINAL_FOREGROUND,
            theme.TERMINAL_BACKGROUND,
        ];
        for color in all {
            assert_ne!(color, 0x000000);
            assert_ne!(color, 0xFFFFFF);
        }
        for color in theme.ANSI {
            assert_ne!(color, 0x000000);
            assert_ne!(color, 0xFFFFFF);
        }
    }

    #[test]
    fn helper_functions_work() {
        assert_eq!(rgb_channels(0xFF8040), (0xFF, 0x80, 0x40));
        assert_eq!(with_alpha(0xD8DAE0, 0x40), 0xD8DAE040);
    }

    #[test]
    fn built_in_themes_follow_orca_spectrum_rule() {
        no_pure_black_or_white(&OrcaTheme::dark());
        no_pure_black_or_white(&OrcaTheme::light());
        no_pure_black_or_white(&OrcaTheme::sepia());
    }

    #[test]
    fn black_theme_uses_true_black_background() {
        let theme = OrcaTheme::black();
        assert_eq!(theme.ABYSS, 0x000000);
        assert_eq!(theme.DEEP, 0x000000);
        assert_eq!(theme.TERMINAL_BACKGROUND, 0x000000);
    }

    #[test]
    fn manual_mode_ignores_window_appearance() {
        let mut settings = AppSettings(crate::settings::AppSettingsInner::default());
        settings.theme_mode = ThemeMode::Manual;
        settings.manual_theme = ThemeId::Black;
        let light = resolve_theme(&settings, WindowAppearance::Light);
        let dark = resolve_theme(&settings, WindowAppearance::Dark);

        assert_eq!(light.selection.resolved_id, ThemeId::Black);
        assert_eq!(dark.selection.resolved_id, ThemeId::Black);
    }

    #[test]
    fn system_mode_uses_light_mapping_for_light_appearances() {
        let mut settings = AppSettings(crate::settings::AppSettingsInner::default());
        settings.theme_mode = ThemeMode::System;
        settings.system_light_theme = ThemeId::Sepia;
        let resolved = resolve_theme(&settings, WindowAppearance::Light);
        assert_eq!(resolved.selection.resolved_id, ThemeId::Sepia);
    }

    #[test]
    fn system_mode_uses_dark_mapping_for_dark_appearances() {
        let mut settings = AppSettings(crate::settings::AppSettingsInner::default());
        settings.theme_mode = ThemeMode::System;
        settings.system_dark_theme = ThemeId::Black;
        let resolved = resolve_theme(&settings, WindowAppearance::Dark);
        assert_eq!(resolved.selection.resolved_id, ThemeId::Black);
    }
}
