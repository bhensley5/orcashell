use std::ops::{Deref, DerefMut};

use gpui::{App, Global};
pub use orcashell_store::{AppSettings as AppSettingsInner, CursorStyle};

/// Wrapper around `AppSettings` so we can implement `Global` in this crate.
#[derive(Debug, Clone)]
pub struct AppSettings(pub AppSettingsInner);

impl Global for AppSettings {}

impl Deref for AppSettings {
    type Target = AppSettingsInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for AppSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl AppSettings {
    pub fn into_inner(self) -> AppSettingsInner {
        self.0
    }
}

/// Load settings from disk and register as a GPUI global.
pub fn register_settings(cx: &mut App) {
    let settings = AppSettingsInner::load();
    cx.set_global(AppSettings(settings));
}
