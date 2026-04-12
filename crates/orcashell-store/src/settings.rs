use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

/// Platform-aware config directory: `~/Library/Application Support/orcashell/` on macOS,
/// `~/.config/orcashell/` on Linux.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .expect("no config directory found")
        .join("orcashell")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn database_path() -> PathBuf {
    config_dir().join("orcashell.db")
}

fn default_font_size() -> f32 {
    13.0
}

fn default_font_family() -> String {
    "JetBrains Mono".to_string()
}

fn default_cursor_style() -> CursorStyle {
    CursorStyle::Bar
}

fn default_cursor_blink() -> bool {
    true
}

fn default_scrollback() -> u32 {
    10_000
}

fn default_theme_mode() -> ThemeMode {
    ThemeMode::Manual
}

fn default_manual_theme() -> ThemeId {
    ThemeId::Dark
}

fn default_system_light_theme() -> ThemeId {
    ThemeId::Light
}

fn default_system_dark_theme() -> ThemeId {
    ThemeId::Dark
}

fn default_true() -> bool {
    true
}

fn default_activity_pulse() -> bool {
    true
}

fn default_sidebar_width() -> f32 {
    240.0
}

fn default_notification_urgent_patterns() -> Vec<String> {
    vec![
        "approv".to_string(),
        "permission".to_string(),
        "edit".to_string(),
    ]
}

/// Terminal cursor style.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
}

/// Concrete theme identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeId {
    Dark,
    Black,
    Light,
    Sepia,
}

/// Theme selection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeMode {
    Manual,
    System,
}

/// User preferences persisted to `settings.json`.
/// All fields have serde defaults so a partial or empty JSON file loads without error.
#[derive(Debug, Clone, Serialize)]
pub struct AppSettings {
    /// Terminal font size in points. Default: 13.0
    #[serde(default = "default_font_size")]
    pub font_size: f32,

    /// Terminal font family. Default: "JetBrains Mono"
    #[serde(default = "default_font_family")]
    pub font_family: String,

    /// Cursor style. Default: bar
    #[serde(default = "default_cursor_style")]
    pub cursor_style: CursorStyle,

    /// Whether the cursor blinks. Default: true
    #[serde(default = "default_cursor_blink")]
    pub cursor_blink: bool,

    /// Terminal scrollback buffer size in lines. Default: 10000
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: u32,

    /// Default shell command. None = system default ($SHELL).
    #[serde(default)]
    pub default_shell: Option<String>,

    /// Theme mode. Default: manual
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeMode,

    /// Theme used when theme mode is manual. Default: dark
    #[serde(default = "default_manual_theme")]
    pub manual_theme: ThemeId,

    /// Theme used when theme mode is system and the OS is in light appearance.
    /// Default: light
    #[serde(default = "default_system_light_theme")]
    pub system_light_theme: ThemeId,

    /// Theme used when theme mode is system and the OS is in dark appearance.
    /// Default: dark
    #[serde(default = "default_system_dark_theme")]
    pub system_dark_theme: ThemeId,

    /// Whether sidebar is visible. Default: true
    #[serde(default = "default_true")]
    pub sidebar_visible: bool,

    /// Whether terminal activity pulse is enabled. Default: true
    #[serde(default = "default_activity_pulse")]
    pub activity_pulse: bool,

    /// Sidebar width in pixels. Default: 240.0
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,

    /// Enable agent notification indicators in the sidebar. Default: true
    #[serde(default = "default_true")]
    pub agent_notifications: bool,

    /// Substring patterns that classify a notification as urgent (case-insensitive).
    /// Default: ["approv", "permission", "edit"]
    #[serde(default = "default_notification_urgent_patterns")]
    pub notification_urgent_patterns: Vec<String>,

    /// Whether agent session auto-resume runs during workspace restore. Default: true
    #[serde(default = "default_true")]
    pub resume_agent_sessions: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            font_family: default_font_family(),
            cursor_style: default_cursor_style(),
            cursor_blink: default_cursor_blink(),
            scrollback_lines: default_scrollback(),
            default_shell: None,
            theme_mode: default_theme_mode(),
            manual_theme: default_manual_theme(),
            system_light_theme: default_system_light_theme(),
            system_dark_theme: default_system_dark_theme(),
            sidebar_visible: default_true(),
            activity_pulse: default_activity_pulse(),
            sidebar_width: default_sidebar_width(),
            agent_notifications: default_true(),
            notification_urgent_patterns: default_notification_urgent_patterns(),
            resume_agent_sessions: default_true(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AppSettingsSerde {
    #[serde(default = "default_font_size")]
    font_size: f32,
    #[serde(default = "default_font_family")]
    font_family: String,
    #[serde(default = "default_cursor_style")]
    cursor_style: CursorStyle,
    #[serde(default = "default_cursor_blink")]
    cursor_blink: bool,
    #[serde(default = "default_scrollback")]
    scrollback_lines: u32,
    #[serde(default)]
    default_shell: Option<String>,
    #[serde(default = "default_theme_mode")]
    theme_mode: ThemeMode,
    #[serde(default = "default_manual_theme")]
    manual_theme: ThemeId,
    #[serde(default = "default_system_light_theme")]
    system_light_theme: ThemeId,
    #[serde(default = "default_system_dark_theme")]
    system_dark_theme: ThemeId,
    #[serde(default = "default_true")]
    sidebar_visible: bool,
    #[serde(default = "default_activity_pulse")]
    activity_pulse: bool,
    #[serde(default = "default_sidebar_width")]
    sidebar_width: f32,
    #[serde(default = "default_true")]
    agent_notifications: bool,
    #[serde(default = "default_notification_urgent_patterns")]
    notification_urgent_patterns: Vec<String>,
    #[serde(default = "default_true")]
    resume_agent_sessions: bool,
    #[serde(default)]
    theme: Option<String>,
}

impl From<AppSettingsSerde> for AppSettings {
    fn from(raw: AppSettingsSerde) -> Self {
        let legacy_theme = raw
            .theme
            .as_deref()
            .and_then(legacy_theme_to_theme_id)
            .unwrap_or_else(default_manual_theme);

        let manual_theme = if raw.theme.is_some() {
            legacy_theme
        } else {
            raw.manual_theme
        };

        let theme_mode = if raw.theme.is_some() {
            ThemeMode::Manual
        } else {
            raw.theme_mode
        };

        Self {
            font_size: raw.font_size,
            font_family: raw.font_family,
            cursor_style: raw.cursor_style,
            cursor_blink: raw.cursor_blink,
            scrollback_lines: raw.scrollback_lines,
            default_shell: raw.default_shell,
            theme_mode,
            manual_theme,
            system_light_theme: raw.system_light_theme,
            system_dark_theme: raw.system_dark_theme,
            sidebar_visible: raw.sidebar_visible,
            activity_pulse: raw.activity_pulse,
            sidebar_width: raw.sidebar_width,
            agent_notifications: raw.agent_notifications,
            notification_urgent_patterns: raw.notification_urgent_patterns,
            resume_agent_sessions: raw.resume_agent_sessions,
        }
    }
}

impl<'de> Deserialize<'de> for AppSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AppSettingsSerde::deserialize(deserializer).map(Into::into)
    }
}

fn legacy_theme_to_theme_id(value: &str) -> Option<ThemeId> {
    match value.trim().to_ascii_lowercase().as_str() {
        "orca-dark" | "dark" => Some(ThemeId::Dark),
        "black" | "oled" | "orca-black" | "orca-oled" => Some(ThemeId::Black),
        "orca-light" | "light" | "white" => Some(ThemeId::Light),
        "sepia" | "orca-sepia" => Some(ThemeId::Sepia),
        _ => None,
    }
}

impl AppSettings {
    /// Load settings from disk. Returns defaults on any error (missing, malformed, etc.).
    pub fn load() -> Self {
        let path = settings_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Load settings from a specific path (for testing).
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to disk with an atomic write (temp file + rename).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_path();
        self.save_to(&path)
    }

    /// Save settings to a specific path (for testing).
    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        orcashell_platform::replace_file(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
