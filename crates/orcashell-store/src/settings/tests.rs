use super::*;

#[test]
fn test_default_settings() {
    let s = AppSettings::default();
    assert!((s.font_size - 13.0).abs() < f32::EPSILON);
    assert_eq!(s.font_family, "JetBrains Mono");
    assert_eq!(s.cursor_style, CursorStyle::Bar);
    assert!(s.cursor_blink);
    assert_eq!(s.scrollback_lines, 10_000);
    assert!(s.default_shell.is_none());
    assert_eq!(s.theme_mode, ThemeMode::Manual);
    assert_eq!(s.manual_theme, ThemeId::Dark);
    assert_eq!(s.system_light_theme, ThemeId::Light);
    assert_eq!(s.system_dark_theme, ThemeId::Dark);
    assert!(s.sidebar_visible);
    assert!(s.activity_pulse);
    assert!(s.agent_notifications);
    assert!(s.resume_agent_sessions);
    assert_eq!(
        s.notification_urgent_patterns,
        vec![
            "approv".to_string(),
            "permission".to_string(),
            "edit".to_string()
        ]
    );
    assert!((s.sidebar_width - 240.0).abs() < f32::EPSILON);
}

#[test]
fn test_load_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let s = AppSettings::load_from(&path);
    assert!((s.font_size - 13.0).abs() < f32::EPSILON);
}

#[test]
fn test_load_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{}").unwrap();
    let s = AppSettings::load_from(&path);
    assert!((s.font_size - 13.0).abs() < f32::EPSILON);
    assert_eq!(s.font_family, "JetBrains Mono");
}

#[test]
fn test_load_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"font_size": 16.0}"#).unwrap();
    let s = AppSettings::load_from(&path);
    assert!((s.font_size - 16.0).abs() < f32::EPSILON);
    assert_eq!(s.font_family, "JetBrains Mono"); // default
}

#[test]
fn test_load_malformed_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "not json at all {{{").unwrap();
    let s = AppSettings::load_from(&path);
    // Should fall back to all defaults
    assert!((s.font_size - 13.0).abs() < f32::EPSILON);
}

#[test]
fn test_save_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let mut original = AppSettings::default();
    original.font_size = 18.0;
    original.font_family = "Fira Code".to_string();
    original.cursor_style = CursorStyle::Bar;
    original.cursor_blink = false;
    original.scrollback_lines = 5000;
    original.default_shell = Some("/bin/fish".to_string());
    original.theme_mode = ThemeMode::System;
    original.manual_theme = ThemeId::Sepia;
    original.system_light_theme = ThemeId::Sepia;
    original.system_dark_theme = ThemeId::Dark;
    original.sidebar_visible = false;
    original.activity_pulse = false;
    original.sidebar_width = 300.0;
    original.resume_agent_sessions = false;

    original.save_to(&path).unwrap();
    let loaded = AppSettings::load_from(&path);

    assert!((loaded.font_size - 18.0).abs() < f32::EPSILON);
    assert_eq!(loaded.font_family, "Fira Code");
    assert_eq!(loaded.cursor_style, CursorStyle::Bar);
    assert!(!loaded.cursor_blink);
    assert_eq!(loaded.scrollback_lines, 5000);
    assert_eq!(loaded.default_shell, Some("/bin/fish".to_string()));
    assert_eq!(loaded.theme_mode, ThemeMode::System);
    assert_eq!(loaded.manual_theme, ThemeId::Sepia);
    assert_eq!(loaded.system_light_theme, ThemeId::Sepia);
    assert_eq!(loaded.system_dark_theme, ThemeId::Dark);
    assert!(!loaded.sidebar_visible);
    assert!(!loaded.activity_pulse);
    assert!(!loaded.resume_agent_sessions);
    assert!((loaded.sidebar_width - 300.0).abs() < f32::EPSILON);
}

#[test]
fn test_atomic_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let tmp_path = dir.path().join("settings.json.tmp");

    let s = AppSettings::default();
    s.save_to(&path).unwrap();

    // Final file should exist, temp file should NOT exist
    assert!(path.exists());
    assert!(!tmp_path.exists());

    // Verify it's valid JSON
    let contents = std::fs::read_to_string(&path).unwrap();
    let _: AppSettings = serde_json::from_str(&contents).unwrap();
}

#[test]
fn test_cursor_style_serde() {
    let json = serde_json::to_string(&CursorStyle::Block).unwrap();
    assert_eq!(json, r#""block""#);

    let json = serde_json::to_string(&CursorStyle::Bar).unwrap();
    assert_eq!(json, r#""bar""#);

    let json = serde_json::to_string(&CursorStyle::Underline).unwrap();
    assert_eq!(json, r#""underline""#);

    let loaded: CursorStyle = serde_json::from_str(r#""bar""#).unwrap();
    assert_eq!(loaded, CursorStyle::Bar);
}

#[test]
fn test_theme_id_serde() {
    let json = serde_json::to_string(&ThemeId::Light).unwrap();
    assert_eq!(json, r#""light""#);

    let loaded: ThemeId = serde_json::from_str(r#""sepia""#).unwrap();
    assert_eq!(loaded, ThemeId::Sepia);

    let loaded: ThemeId = serde_json::from_str(r#""black""#).unwrap();
    assert_eq!(loaded, ThemeId::Black);
}

#[test]
fn test_theme_mode_serde() {
    let json = serde_json::to_string(&ThemeMode::System).unwrap();
    assert_eq!(json, r#""system""#);

    let loaded: ThemeMode = serde_json::from_str(r#""manual""#).unwrap();
    assert_eq!(loaded, ThemeMode::Manual);
}

#[test]
fn test_legacy_theme_string_migrates_to_manual_dark() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"theme":"orca-dark"}"#).unwrap();

    let loaded = AppSettings::load_from(&path);
    assert_eq!(loaded.theme_mode, ThemeMode::Manual);
    assert_eq!(loaded.manual_theme, ThemeId::Dark);
}

#[test]
fn test_unknown_legacy_theme_falls_back_to_dark() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"theme":"custom-theme"}"#).unwrap();

    let loaded = AppSettings::load_from(&path);
    assert_eq!(loaded.theme_mode, ThemeMode::Manual);
    assert_eq!(loaded.manual_theme, ThemeId::Dark);
}
