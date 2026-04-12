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

#[test]
fn unrelated_settings_changes_do_not_change_resolved_theme() {
    let settings = AppSettings(crate::settings::AppSettingsInner::default());
    let original = resolve_theme(&settings, WindowAppearance::Dark);

    let mut updated = settings.clone();
    updated.font_size += 1.0;
    updated.sidebar_visible = !updated.sidebar_visible;

    let resolved = resolve_theme(&updated, WindowAppearance::Dark);
    assert_eq!(original, resolved);
}
