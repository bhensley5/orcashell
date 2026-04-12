use std::collections::HashMap;

use gpui::{AnyWindowHandle, Global};

/// Tracks all open OrcaShell windows by their deterministic ID.
/// Registered as a GPUI Global so any code with `&App` can access it.
pub struct WindowRegistry {
    windows: HashMap<i64, AnyWindowHandle>,
}

impl Global for WindowRegistry {}

impl Default for WindowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    /// Register a window after `cx.open_window()`.
    pub fn register(&mut self, window_id: i64, handle: AnyWindowHandle) {
        self.windows.insert(window_id, handle);
    }

    /// Unregister a window when it is closed.
    pub fn unregister(&mut self, window_id: i64) {
        self.windows.remove(&window_id);
    }

    /// Number of tracked windows.
    pub fn count(&self) -> usize {
        self.windows.len()
    }

    /// Iterate over all (window_id, handle) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (i64, AnyWindowHandle)> + '_ {
        self.windows.iter().map(|(&id, &handle)| (id, handle))
    }

    /// Find closed windows by diffing against the live set from `cx.windows()`.
    pub fn find_closed(&self, live: &[AnyWindowHandle]) -> Vec<i64> {
        self.windows
            .iter()
            .filter(|(_, &h)| !live.contains(&h))
            .map(|(&id, _)| id)
            .collect()
    }
}

#[cfg(test)]
mod tests;
