#[derive(Clone, Debug)]
pub struct FocusTarget {
    pub project_id: String,
    pub layout_path: Vec<usize>,
}

pub struct FocusManager {
    current: Option<FocusTarget>,
}

impl FocusManager {
    pub fn new() -> Self {
        Self { current: None }
    }

    pub fn set_current(&mut self, target: FocusTarget) {
        self.current = Some(target);
    }

    pub fn current_target(&self) -> Option<&FocusTarget> {
        self.current.as_ref()
    }

    pub fn clear(&mut self) {
        self.current = None;
    }

    pub fn is_focused(&self, project_id: &str, layout_path: &[usize]) -> bool {
        match &self.current {
            Some(target) => target.project_id == project_id && target.layout_path == layout_path,
            None => false,
        }
    }
}

impl Default for FocusManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
