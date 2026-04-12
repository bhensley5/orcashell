use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use super::layout::LayoutNode;
use orcashell_store::StoredProject;

pub struct ProjectData {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub layout: LayoutNode,
    pub terminal_names: HashMap<String, String>,
}

impl ProjectData {
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        Self {
            id: format!("proj-{}", Uuid::new_v4()),
            name,
            path,
            layout: LayoutNode::Terminal {
                terminal_id: None,
                working_directory: None,
                zoom_level: None,
            },
            terminal_names: HashMap::new(),
        }
    }

    pub fn new_with_name(name: String, path: PathBuf) -> Self {
        Self {
            id: format!("proj-{}", Uuid::new_v4()),
            name,
            path,
            layout: LayoutNode::Terminal {
                terminal_id: None,
                working_directory: None,
                zoom_level: None,
            },
            terminal_names: HashMap::new(),
        }
    }

    pub fn custom_terminal_name(&self, terminal_id: &str) -> Option<&str> {
        self.terminal_names.get(terminal_id).map(|s| s.as_str())
    }

    /// Convert to a persistence-layer `StoredProject` by serializing layout and terminal_names.
    pub fn to_stored(&self, sort_order: i32) -> StoredProject {
        self.to_stored_for_window(sort_order, 1)
    }

    /// Convert to a persistence-layer `StoredProject` tagged with a specific window_id.
    pub fn to_stored_for_window(&self, sort_order: i32, window_id: i64) -> StoredProject {
        let layout_json =
            serde_json::to_string(&self.layout).expect("LayoutNode serialization cannot fail");
        let terminal_names_json = serde_json::to_string(&self.terminal_names)
            .expect("terminal_names serialization cannot fail");

        StoredProject {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            layout_json,
            terminal_names_json,
            sort_order,
            window_id,
        }
    }

    /// Restore from a persistence-layer `StoredProject`.
    /// Terminal IDs are preserved from storage (caller should clear them and
    /// spawn fresh sessions during layout restoration).
    pub fn from_stored(stored: StoredProject) -> anyhow::Result<Self> {
        let layout: LayoutNode = serde_json::from_str(&stored.layout_json)?;
        let terminal_names: HashMap<String, String> =
            serde_json::from_str(&stored.terminal_names_json)?;

        Ok(Self {
            id: stored.id,
            name: stored.name,
            path: stored.path,
            layout,
            terminal_names,
        })
    }

    /// Walk the layout tree and clear all terminal IDs (set to None).
    /// Used after deserialization to prepare for fresh session spawning.
    pub fn clear_terminal_ids(&mut self) {
        Self::clear_ids_recursive(&mut self.layout);
    }

    fn clear_ids_recursive(node: &mut LayoutNode) {
        match node {
            LayoutNode::Terminal { terminal_id, .. } => {
                *terminal_id = None;
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    Self::clear_ids_recursive(child);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
