use super::orca_syntax_theme;
use orcashell_store::ThemeId;

#[test]
fn all_theme_variants_build() {
    for theme_id in [
        ThemeId::Dark,
        ThemeId::Black,
        ThemeId::Light,
        ThemeId::Sepia,
    ] {
        let theme = orca_syntax_theme(theme_id);
        assert!(theme.settings.foreground.is_some());
        assert!(theme.settings.background.is_some());
        assert!(!theme.scopes.is_empty());
    }
}
