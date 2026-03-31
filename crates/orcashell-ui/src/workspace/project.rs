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
mod tests {
    // Explicit imports to avoid pulling in GPUI types that blow the
    // gpui_macros proc-macro stack during test compilation.
    use super::{LayoutNode, ProjectData, StoredProject};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_new_creates_single_terminal() {
        let project = ProjectData::new(PathBuf::from("/tmp/my-project"));
        assert_eq!(project.layout.terminal_count(), 1);
        assert!(matches!(project.layout, LayoutNode::Terminal { .. }));
    }

    #[test]
    fn test_id_format() {
        let project = ProjectData::new(PathBuf::from("/tmp/test"));
        assert!(project.id.starts_with("proj-"));
    }

    #[test]
    fn test_name_from_path_basename() {
        let project = ProjectData::new(PathBuf::from("/home/user/my-project"));
        assert_eq!(project.name, "my-project");
    }

    #[test]
    fn test_name_from_root() {
        let project = ProjectData::new(PathBuf::from("/"));
        // Root has no file_name, falls back to full path
        assert_eq!(project.name, "/");
    }

    #[test]
    fn test_custom_terminal_name_returns_override_only() {
        let mut project = ProjectData::new(PathBuf::from("/tmp/project"));
        assert_eq!(project.custom_terminal_name("missing"), None);

        project.terminal_names.insert("t1".into(), "Pinned".into());
        assert_eq!(project.custom_terminal_name("t1"), Some("Pinned"));
    }

    #[test]
    fn test_project_to_stored() {
        let mut project = ProjectData::new(PathBuf::from("/home/user/project"));
        project.id = "proj-test-1".to_string();
        project
            .terminal_names
            .insert("t1".into(), "my shell".into());

        let stored = project.to_stored(3);
        assert_eq!(stored.id, "proj-test-1");
        assert_eq!(stored.name, "project");
        assert_eq!(stored.path, PathBuf::from("/home/user/project"));
        assert_eq!(stored.sort_order, 3);
        // layout_json should be valid JSON
        let _: LayoutNode = serde_json::from_str(&stored.layout_json).unwrap();
        // terminal_names_json should contain our entry
        let names: HashMap<String, String> =
            serde_json::from_str(&stored.terminal_names_json).unwrap();
        assert_eq!(names.get("t1").unwrap(), "my shell");
    }

    #[test]
    fn test_stored_to_project() {
        // Create a stored project with a known layout
        let layout = LayoutNode::Tabs {
            children: vec![
                LayoutNode::Terminal {
                    terminal_id: Some("old-id-1".into()),
                    working_directory: Some(PathBuf::from("/src")),
                    zoom_level: Some(2.0),
                },
                LayoutNode::Terminal {
                    terminal_id: Some("old-id-2".into()),
                    working_directory: None,
                    zoom_level: None,
                },
            ],
            active_tab: 0,
        };
        let layout_json = serde_json::to_string(&layout).unwrap();

        let stored = StoredProject {
            id: "proj-test".into(),
            name: "my-project".into(),
            path: PathBuf::from("/home/user/my-project"),
            layout_json,
            terminal_names_json: r#"{"old-id-1":"custom name"}"#.into(),
            sort_order: 0,
            window_id: 1,
        };

        let mut project = ProjectData::from_stored(stored).unwrap();
        assert_eq!(project.id, "proj-test");
        assert_eq!(project.name, "my-project");
        assert_eq!(project.layout.terminal_count(), 2);

        // working_directory and zoom_level should be preserved
        if let LayoutNode::Tabs { children, .. } = &project.layout {
            if let LayoutNode::Terminal {
                working_directory,
                zoom_level,
                ..
            } = &children[0]
            {
                assert_eq!(
                    working_directory.as_deref(),
                    Some(std::path::Path::new("/src"))
                );
                assert_eq!(*zoom_level, Some(2.0));
            } else {
                panic!("expected Terminal");
            }
        }

        // clear_terminal_ids should set all IDs to None
        project.clear_terminal_ids();
        let ids = project.layout.collect_terminal_ids();
        assert!(ids.is_empty());

        // But working_directory should still be preserved after clearing IDs
        if let LayoutNode::Tabs { children, .. } = &project.layout {
            if let LayoutNode::Terminal {
                working_directory, ..
            } = &children[0]
            {
                assert_eq!(
                    working_directory.as_deref(),
                    Some(std::path::Path::new("/src"))
                );
            }
        }
    }

    #[test]
    fn test_from_stored_malformed_json() {
        let stored = StoredProject {
            id: "proj-bad".into(),
            name: "bad".into(),
            path: PathBuf::from("/tmp"),
            layout_json: "not valid json{{{".into(),
            terminal_names_json: "{}".into(),
            sort_order: 0,
            window_id: 1,
        };
        assert!(ProjectData::from_stored(stored).is_err());

        let stored2 = StoredProject {
            id: "proj-bad2".into(),
            name: "bad2".into(),
            path: PathBuf::from("/tmp"),
            layout_json: r#"{"Terminal":{"terminal_id":null}}"#.into(),
            terminal_names_json: "not valid json".into(),
            sort_order: 0,
            window_id: 1,
        };
        assert!(ProjectData::from_stored(stored2).is_err());
    }
}
