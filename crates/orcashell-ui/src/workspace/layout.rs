use std::mem;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SplitDirection {
    Horizontal, // Children stacked top-to-bottom (divider is horizontal)
    Vertical,   // Children stacked left-to-right (divider is vertical)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LayoutNode {
    Terminal {
        terminal_id: Option<String>,
        /// Last known working directory (captured via process cwd query on save).
        #[serde(default)]
        working_directory: Option<PathBuf>,
        /// Per-terminal zoom offset (e.g., +2.0 means 2pt larger than base).
        #[serde(default)]
        zoom_level: Option<f32>,
    },
    Split {
        direction: SplitDirection,
        sizes: Vec<f32>,
        children: Vec<LayoutNode>,
    },
    Tabs {
        children: Vec<LayoutNode>,
        #[serde(default)]
        active_tab: usize,
    },
}

impl LayoutNode {
    pub fn new_terminal() -> Self {
        LayoutNode::Terminal {
            terminal_id: None,
            working_directory: None,
            zoom_level: None,
        }
    }

    /// Find the first terminal ID in this subtree (depth-first).
    pub fn first_terminal_id(&self) -> Option<&String> {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => terminal_id.as_ref(),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().find_map(|c| c.first_terminal_id())
            }
        }
    }

    /// Return the active_tab index if this is a Tabs node.
    pub fn active_tab_index(&self) -> Option<usize> {
        match self {
            LayoutNode::Tabs { active_tab, .. } => Some(*active_tab),
            _ => None,
        }
    }

    /// Return the number of children if this is a Tabs node.
    pub fn tab_count(&self) -> Option<usize> {
        match self {
            LayoutNode::Tabs { children, .. } => Some(children.len()),
            _ => None,
        }
    }

    pub fn get_at_path(&self, path: &[usize]) -> Option<&LayoutNode> {
        if path.is_empty() {
            return Some(self);
        }
        match self {
            LayoutNode::Terminal { .. } => None,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => children
                .get(path[0])
                .and_then(|child| child.get_at_path(&path[1..])),
        }
    }

    pub fn get_at_path_mut(&mut self, path: &[usize]) -> Option<&mut LayoutNode> {
        if path.is_empty() {
            return Some(self);
        }
        match self {
            LayoutNode::Terminal { .. } => None,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => children
                .get_mut(path[0])
                .and_then(|child| child.get_at_path_mut(&path[1..])),
        }
    }

    /// Remove the node at the given path. After removal, if the parent container
    /// has only one child remaining, collapse it (replace parent with that child).
    /// For Tabs, adjusts active_tab if needed.
    /// Returns the removed node, or None if the path is invalid.
    pub fn remove_at_path(&mut self, path: &[usize]) -> Option<LayoutNode> {
        if path.is_empty() {
            return None; // Can't remove root
        }

        if path.len() == 1 {
            let idx = path[0];
            match self {
                LayoutNode::Terminal { .. } => None,
                LayoutNode::Split {
                    children, sizes, ..
                } => {
                    if idx >= children.len() {
                        return None;
                    }
                    let removed = children.remove(idx);
                    if idx < sizes.len() {
                        sizes.remove(idx);
                    }
                    // Collapse single-child container
                    if children.len() == 1 {
                        let only_child = children.remove(0);
                        *self = only_child;
                    }
                    Some(removed)
                }
                LayoutNode::Tabs {
                    children,
                    active_tab,
                } => {
                    if idx >= children.len() {
                        return None;
                    }
                    let removed = children.remove(idx);
                    // Adjust active_tab
                    if children.is_empty() {
                        *active_tab = 0;
                    } else if idx <= *active_tab {
                        *active_tab = active_tab.saturating_sub(1);
                    }
                    // Collapse single-child container
                    if children.len() == 1 {
                        let only_child = children.remove(0);
                        *self = only_child;
                    }
                    Some(removed)
                }
            }
        } else {
            // Recurse to the parent of the target
            let first = path[0];
            match self {
                LayoutNode::Terminal { .. } => None,
                LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => children
                    .get_mut(first)
                    .and_then(|child| child.remove_at_path(&path[1..])),
            }
        }
    }

    pub fn find_terminal_path(&self, target_id: &str) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if terminal_id.as_deref() == Some(target_id) {
                    Some(vec![])
                } else {
                    None
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    if let Some(mut path) = child.find_terminal_path(target_id) {
                        path.insert(0, i);
                        return Some(path);
                    }
                }
                None
            }
        }
    }

    pub fn collect_terminal_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.collect_terminal_ids_inner(&mut ids);
        ids
    }

    fn collect_terminal_ids_inner(&self, ids: &mut Vec<String>) {
        match self {
            LayoutNode::Terminal { terminal_id, .. } => {
                if let Some(id) = terminal_id {
                    ids.push(id.clone());
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    child.collect_terminal_ids_inner(ids);
                }
            }
        }
    }

    pub fn find_first_terminal_path(&self) -> Option<Vec<usize>> {
        match self {
            LayoutNode::Terminal { .. } => Some(vec![]),
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    if let Some(mut path) = child.find_first_terminal_path() {
                        path.insert(0, i);
                        return Some(path);
                    }
                }
                None
            }
        }
    }

    /// Collect paths to all Terminal leaves in depth-first order.
    pub fn collect_terminal_paths(&self) -> Vec<Vec<usize>> {
        let mut paths = Vec::new();
        self.collect_terminal_paths_inner(&mut paths, &mut vec![]);
        paths
    }

    fn collect_terminal_paths_inner(&self, paths: &mut Vec<Vec<usize>>, current: &mut Vec<usize>) {
        match self {
            LayoutNode::Terminal { .. } => {
                paths.push(current.clone());
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    current.push(i);
                    child.collect_terminal_paths_inner(paths, current);
                    current.pop();
                }
            }
        }
    }

    pub fn terminal_count(&self) -> usize {
        match self {
            LayoutNode::Terminal { .. } => 1,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.iter().map(|c| c.terminal_count()).sum()
            }
        }
    }

    /// Normalize the tree to maintain invariants:
    /// - Collapse single-child Split/Tabs to their only child
    /// - Flatten nested same-direction splits with proportional size merging
    /// - Fix sizes/children count mismatches
    /// - Replace invalid sizes (zero, negative, NaN, infinite)
    pub fn normalize(&mut self) {
        // First, recurse into children
        match self {
            LayoutNode::Terminal { .. } => return,
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children.iter_mut() {
                    child.normalize();
                }
            }
        }

        // Collapse empty or single-child containers
        let child_count = match self {
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                children.len()
            }
            _ => unreachable!(),
        };
        if child_count == 0 {
            *self = LayoutNode::new_terminal();
            return;
        }
        if child_count == 1 {
            let only_child = match self {
                LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                    children.remove(0)
                }
                _ => unreachable!(),
            };
            *self = only_child;
            return;
        }

        // Fix sizes and flatten same-direction splits
        if let LayoutNode::Split {
            direction,
            sizes,
            children,
        } = self
        {
            // Fix sizes/children count mismatch
            if sizes.len() != children.len() {
                let count = children.len();
                let equal = 1.0;
                *sizes = vec![equal; count];
            }

            // Replace invalid sizes
            let has_invalid = sizes.iter().any(|s| !s.is_finite() || *s <= 0.0);
            if has_invalid {
                let count = children.len();
                let equal = 1.0;
                *sizes = vec![equal; count];
            }

            // Flatten nested same-direction splits (skip if none need flattening)
            let needs_flatten = children
                .iter()
                .any(|c| matches!(c, LayoutNode::Split { direction: d, .. } if d == direction));
            if !needs_flatten {
                return;
            }

            let mut new_children = Vec::new();
            let mut new_sizes = Vec::new();
            let total_size: f32 = sizes.iter().sum();

            for (i, child) in children.iter().enumerate() {
                let child_weight = if total_size > 0.0 {
                    sizes[i] / total_size
                } else {
                    1.0 / children.len() as f32
                };

                if let LayoutNode::Split {
                    direction: child_dir,
                    sizes: child_sizes,
                    children: child_children,
                } = child
                {
                    if child_dir == direction {
                        let child_total: f32 = child_sizes.iter().sum();
                        for (j, grandchild) in child_children.iter().enumerate() {
                            new_children.push(grandchild.clone());
                            let grandchild_proportion = if child_total > 0.0 {
                                child_sizes[j] / child_total
                            } else {
                                1.0 / child_children.len() as f32
                            };
                            new_sizes.push(child_weight * grandchild_proportion * total_size);
                        }
                        continue;
                    }
                }
                new_children.push(child.clone());
                new_sizes.push(sizes[i]);
            }

            if new_children.len() != children.len() {
                let _ = mem::replace(children, new_children);
                let _ = mem::replace(sizes, new_sizes);
            }
        }

        // Fix Tabs active_tab bounds
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = self
        {
            if !children.is_empty() && *active_tab >= children.len() {
                *active_tab = children.len() - 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports to avoid pulling in GPUI types that blow the
    // gpui_macros proc-macro stack during test compilation.
    use super::{LayoutNode, SplitDirection};
    use std::mem;
    use std::path::PathBuf;

    fn term(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            terminal_id: Some(id.to_string()),
            working_directory: None,
            zoom_level: None,
        }
    }

    fn term_none() -> LayoutNode {
        LayoutNode::Terminal {
            terminal_id: None,
            working_directory: None,
            zoom_level: None,
        }
    }

    fn split_v(children: Vec<LayoutNode>) -> LayoutNode {
        let count = children.len();
        LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![1.0; count],
            children,
        }
    }

    fn split_h(children: Vec<LayoutNode>) -> LayoutNode {
        let count = children.len();
        LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![1.0; count],
            children,
        }
    }

    fn tabs(children: Vec<LayoutNode>, active: usize) -> LayoutNode {
        LayoutNode::Tabs {
            children,
            active_tab: active,
        }
    }

    // --- get_at_path tests ---

    #[test]
    fn test_get_at_path_empty_returns_root() {
        let node = term("t1");
        assert!(node.get_at_path(&[]).is_some());
    }

    #[test]
    fn test_get_at_path_single_index() {
        let node = split_v(vec![term("t1"), term("t2")]);
        let child = node.get_at_path(&[1]).unwrap();
        match child {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("expected Terminal"),
        }
    }

    #[test]
    fn test_get_at_path_nested() {
        let node = split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]);
        let child = node.get_at_path(&[0, 1]).unwrap();
        match child {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("expected Terminal"),
        }
    }

    #[test]
    fn test_get_at_path_out_of_bounds() {
        let node = split_v(vec![term("t1")]);
        assert!(node.get_at_path(&[5]).is_none());
    }

    // --- get_at_path_mut tests ---

    #[test]
    fn test_get_at_path_mut_can_modify() {
        let mut node = split_v(vec![term("t1"), term("t2")]);
        if let Some(LayoutNode::Terminal { terminal_id, .. }) = node.get_at_path_mut(&[0]) {
            *terminal_id = Some("modified".into());
        }
        match node.get_at_path(&[0]).unwrap() {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("modified"));
            }
            _ => panic!("expected Terminal"),
        }
    }

    // --- remove_at_path tests ---

    #[test]
    fn test_remove_from_split_collapses() {
        let mut node = split_v(vec![term("t1"), term("t2")]);
        let removed = node.remove_at_path(&[0]);
        assert!(removed.is_some());
        // Should collapse to the remaining child
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("expected single Terminal after collapse, got {:?}", node),
        }
    }

    #[test]
    fn test_remove_from_tabs_adjusts_active_tab() {
        // Remove tab before active
        let mut node = tabs(vec![term("t1"), term("t2"), term("t3")], 2);
        node.remove_at_path(&[0]);
        if let LayoutNode::Tabs { active_tab, .. } = &node {
            assert_eq!(*active_tab, 1);
        } else {
            panic!("expected Tabs");
        }

        // Remove active tab
        let mut node = tabs(vec![term("t1"), term("t2"), term("t3")], 1);
        node.remove_at_path(&[1]);
        if let LayoutNode::Tabs { active_tab, .. } = &node {
            assert!(*active_tab <= 1);
        } else {
            panic!("expected Tabs");
        }

        // Remove tab after active. active_tab unchanged
        let mut node = tabs(vec![term("t1"), term("t2"), term("t3")], 0);
        node.remove_at_path(&[2]);
        if let LayoutNode::Tabs { active_tab, .. } = &node {
            assert_eq!(*active_tab, 0);
        } else {
            panic!("expected Tabs");
        }
    }

    // --- find_terminal_path tests ---

    #[test]
    fn test_find_terminal_path_found() {
        let node = split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]);
        assert_eq!(node.find_terminal_path("t2"), Some(vec![0, 1]));
        assert_eq!(node.find_terminal_path("t3"), Some(vec![1]));
    }

    #[test]
    fn test_find_terminal_path_not_found() {
        let node = split_v(vec![term("t1")]);
        assert_eq!(node.find_terminal_path("missing"), None);
    }

    // --- collect_terminal_ids tests ---

    #[test]
    fn test_collect_terminal_ids_skips_none() {
        let node = split_v(vec![term("t1"), term_none(), term("t3")]);
        let ids = node.collect_terminal_ids();
        assert_eq!(ids, vec!["t1", "t3"]);
    }

    #[test]
    fn test_collect_terminal_ids_nested() {
        let node = split_v(vec![
            split_h(vec![term("t1"), term("t2")]),
            tabs(vec![term("t3"), term("t4")], 0),
        ]);
        let ids = node.collect_terminal_ids();
        assert_eq!(ids, vec!["t1", "t2", "t3", "t4"]);
    }

    // --- terminal_count tests ---

    #[test]
    fn test_terminal_count() {
        assert_eq!(term("t1").terminal_count(), 1);
        assert_eq!(split_v(vec![term("t1"), term("t2")]).terminal_count(), 2);
        assert_eq!(
            split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]).terminal_count(),
            3
        );
    }

    // --- find_first_terminal_path tests ---

    #[test]
    fn test_find_first_terminal_path() {
        let node = split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]);
        assert_eq!(node.find_first_terminal_path(), Some(vec![0, 0]));
    }

    #[test]
    fn test_find_first_terminal_path_single() {
        let node = term("t1");
        assert_eq!(node.find_first_terminal_path(), Some(vec![]));
    }

    // --- normalize tests ---

    #[test]
    fn test_normalize_collapses_single_child_split() {
        let mut node = split_v(vec![term("t1")]);
        node.normalize();
        match &node {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t1"));
            }
            _ => panic!("should collapse to Terminal"),
        }
    }

    #[test]
    fn test_normalize_flattens_same_direction() {
        let mut node = split_v(vec![split_v(vec![term("t1"), term("t2")]), term("t3")]);
        node.normalize();
        match &node {
            LayoutNode::Split {
                direction,
                children,
                sizes,
            } => {
                assert_eq!(*direction, SplitDirection::Vertical);
                assert_eq!(children.len(), 3);
                assert_eq!(sizes.len(), 3);
            }
            _ => panic!("should be a flat Split"),
        }
    }

    #[test]
    fn test_normalize_does_not_flatten_different_direction() {
        let mut node = split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]);
        node.normalize();
        match &node {
            LayoutNode::Split { children, .. } => {
                assert_eq!(children.len(), 2); // Not flattened
                assert!(matches!(children[0], LayoutNode::Split { .. }));
            }
            _ => panic!("should remain nested Split"),
        }
    }

    #[test]
    fn test_normalize_fixes_size_mismatch() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![1.0], // Wrong count
            children: vec![term("t1"), term("t2"), term("t3")],
        };
        node.normalize();
        if let LayoutNode::Split {
            sizes, children, ..
        } = &node
        {
            assert_eq!(sizes.len(), children.len());
        }
    }

    #[test]
    fn test_normalize_replaces_invalid_sizes() {
        let mut node = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![0.0, f32::NAN],
            children: vec![term("t1"), term("t2")],
        };
        node.normalize();
        if let LayoutNode::Split { sizes, .. } = &node {
            assert_eq!(sizes.len(), 2);
            assert!(sizes.iter().all(|s| s.is_finite() && *s > 0.0));
        }
    }

    // --- split operation test ---

    #[test]
    fn test_split_creates_correct_shape() {
        let old = term("t1");
        let new_term = term("t2");
        let split = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            sizes: vec![50.0, 50.0],
            children: vec![old, new_term],
        };
        assert_eq!(split.terminal_count(), 2);
        assert_eq!(split.find_terminal_path("t1"), Some(vec![0]));
        assert_eq!(split.find_terminal_path("t2"), Some(vec![1]));
    }

    // --- close last terminal is no-op ---

    #[test]
    fn test_cannot_remove_root() {
        let mut node = term("t1");
        assert!(node.remove_at_path(&[]).is_none());
    }

    // --- collect_terminal_paths tests ---

    #[test]
    fn test_collect_terminal_paths() {
        let node = split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]);
        let paths = node.collect_terminal_paths();
        assert_eq!(paths, vec![vec![0, 0], vec![0, 1], vec![1]]);
    }

    // --- Split operation simulation ---

    #[test]
    fn test_split_inside_nested_tree() {
        // Simulate splitting t2 inside an existing split
        let mut layout = split_v(vec![term("t1"), term("t2")]);
        // Navigate to t2 at path [1] and replace with a horizontal split
        let node = layout.get_at_path_mut(&[1]).unwrap();
        let old = mem::replace(node, LayoutNode::new_terminal());
        *node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            sizes: vec![50.0, 50.0],
            children: vec![old, term("t3")],
        };
        layout.normalize();

        assert_eq!(layout.terminal_count(), 3);
        assert_eq!(layout.find_terminal_path("t1"), Some(vec![0]));
        assert_eq!(layout.find_terminal_path("t2"), Some(vec![1, 0]));
        assert_eq!(layout.find_terminal_path("t3"), Some(vec![1, 1]));
    }

    #[test]
    fn test_close_last_terminal_guard() {
        let layout = term("t1");
        // terminal_count() == 1, so close operation should be skipped
        assert_eq!(layout.terminal_count(), 1);
    }

    #[test]
    fn test_close_moves_to_sibling() {
        let mut layout = split_v(vec![term("t1"), term("t2")]);
        // Remove t1 at path [0]
        layout.remove_at_path(&[0]);
        layout.normalize();
        // After collapse, should be just t2
        let first_path = layout.find_first_terminal_path().unwrap();
        assert_eq!(first_path, Vec::<usize>::new());
        match &layout {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t2"));
            }
            _ => panic!("expected terminal after close + collapse"),
        }
    }

    // --- Tabs active_tab bounds ---

    #[test]
    fn test_normalize_fixes_active_tab_bounds() {
        let mut node = LayoutNode::Tabs {
            children: vec![term("t1"), term("t2")],
            active_tab: 5, // Out of bounds
        };
        node.normalize();
        if let LayoutNode::Tabs { active_tab, .. } = &node {
            assert_eq!(*active_tab, 1);
        }
    }

    // --- first_terminal_id tests ---

    #[test]
    fn test_first_terminal_id_on_terminal() {
        let node = term("t1");
        assert_eq!(node.first_terminal_id(), Some(&"t1".to_string()));
    }

    #[test]
    fn test_first_terminal_id_on_split() {
        let node = split_v(vec![term("t1"), term("t2")]);
        assert_eq!(node.first_terminal_id(), Some(&"t1".to_string()));
    }

    #[test]
    fn test_first_terminal_id_on_nested_split() {
        let node = split_v(vec![split_h(vec![term("t3"), term("t2")]), term("t1")]);
        assert_eq!(node.first_terminal_id(), Some(&"t3".to_string()));
    }

    #[test]
    fn test_first_terminal_id_on_empty_tabs() {
        let node = LayoutNode::Tabs {
            children: vec![],
            active_tab: 0,
        };
        assert_eq!(node.first_terminal_id(), None);
    }

    #[test]
    fn test_first_terminal_id_on_terminal_none() {
        let node = term_none();
        assert_eq!(node.first_terminal_id(), None);
    }

    // --- active_tab_index tests ---

    #[test]
    fn test_active_tab_index_on_tabs() {
        let node = tabs(vec![term("t1"), term("t2")], 1);
        assert_eq!(node.active_tab_index(), Some(1));
    }

    #[test]
    fn test_active_tab_index_on_non_tabs() {
        let node = term("t1");
        assert_eq!(node.active_tab_index(), None);
    }

    // --- tab_count tests ---

    #[test]
    fn test_tab_count_on_tabs() {
        let node = tabs(vec![term("t1"), term("t2"), term("t3")], 0);
        assert_eq!(node.tab_count(), Some(3));
    }

    #[test]
    fn test_tab_count_on_non_tabs() {
        let node = split_v(vec![term("t1"), term("t2")]);
        assert_eq!(node.tab_count(), None);
    }

    // --- Root-always-Tabs invariant tests ---

    #[test]
    fn test_root_tabs_new_tab_at_root() {
        // Simulate new_tab_focused: add child to root Tabs
        let mut root = tabs(vec![term("t1")], 0);
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut root
        {
            children.push(term("t2"));
            *active_tab = children.len() - 1;
        }
        assert_eq!(root.tab_count(), Some(2));
        assert_eq!(root.active_tab_index(), Some(1));
    }

    #[test]
    fn test_root_tabs_close_needs_rewrap_after_collapse() {
        // remove_at_path on root Tabs with 2 children will collapse to a single child.
        // The caller (WorkspaceState) must re-wrap with enforce_root_tabs().
        let mut root = tabs(vec![term("t1"), term("t2")], 0);
        root.remove_at_path(&[0]);
        // LayoutNode::remove_at_path collapses single-child Tabs. This is expected.
        // WorkspaceState::enforce_root_tabs re-wraps it.
        assert!(matches!(root, LayoutNode::Terminal { .. }));

        // Simulate enforce_root_tabs
        if !matches!(root, LayoutNode::Tabs { .. }) {
            let child = std::mem::replace(&mut root, LayoutNode::new_terminal());
            root = LayoutNode::Tabs {
                children: vec![child],
                active_tab: 0,
            };
        }
        // Now root is Tabs again with the remaining terminal
        assert!(matches!(root, LayoutNode::Tabs { .. }));
        assert_eq!(root.tab_count(), Some(1));
        assert_eq!(root.first_terminal_id(), Some(&"t2".to_string()));
    }

    #[test]
    fn test_root_tabs_close_tab_with_split_content() {
        // Tab contains a split. Closing the tab removes the whole subtree
        let mut root = tabs(vec![split_v(vec![term("t1"), term("t2")]), term("t3")], 0);
        // Remove tab 0 (the split)
        root.remove_at_path(&[0]);
        // Collapses to t3
        match &root {
            LayoutNode::Terminal { terminal_id, .. } => {
                assert_eq!(terminal_id.as_deref(), Some("t3"));
            }
            _ => panic!("expected collapse to Terminal"),
        }
    }

    #[test]
    fn test_focus_path_includes_tab_index() {
        // With root Tabs, terminal in tab 0 is at path [0]
        // Terminal in tab 0's split child 1 is at path [0, 1]
        let root = tabs(vec![split_v(vec![term("t1"), term("t2")]), term("t3")], 0);
        assert_eq!(root.find_terminal_path("t1"), Some(vec![0, 0]));
        assert_eq!(root.find_terminal_path("t2"), Some(vec![0, 1]));
        assert_eq!(root.find_terminal_path("t3"), Some(vec![1]));
    }

    #[test]
    fn test_goto_tab_bounds() {
        let root = tabs(vec![term("t1"), term("t2")], 0);
        // tab_count is 2, so index 2 would be out of bounds
        assert_eq!(root.tab_count(), Some(2));
        assert!(root.get_at_path(&[2]).is_none());
    }

    // --- Reorder tab tests (simulating WorkspaceState::reorder_tab logic) ---

    #[test]
    fn test_reorder_tab_swap() {
        let mut root = tabs(vec![term("t1"), term("t2"), term("t3")], 0);
        // Move tab 0 to position 2
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut root
        {
            let child = children.remove(0);
            children.insert(2, child);
            // active_tab was 0, moved to 2
            *active_tab = 2;
        }
        // Verify order: t2, t3, t1
        assert_eq!(root.first_terminal_id(), Some(&"t2".to_string()));
        assert_eq!(
            root.get_at_path(&[2])
                .and_then(|n| n.first_terminal_id())
                .map(|s| s.as_str()),
            Some("t1")
        );
    }

    #[test]
    fn test_reorder_tab_active_tab_tracking() {
        let mut root = tabs(vec![term("t1"), term("t2"), term("t3")], 1);
        // Move tab 0 to position 2. active_tab (1) should shift down to 0
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut root
        {
            let from = 0;
            let to = 2;
            let child = children.remove(from);
            children.insert(to, child);
            // Update tracking: from < active_tab && to >= active_tab
            if *active_tab == from {
                *active_tab = to;
            } else if from < *active_tab && to >= *active_tab {
                *active_tab = active_tab.saturating_sub(1);
            }
        }
        if let LayoutNode::Tabs { active_tab, .. } = &root {
            assert_eq!(*active_tab, 0); // was 1, shifted down because tab 0 moved past it
        }
    }

    #[test]
    fn test_reorder_tab_noop_same_index() {
        let root = tabs(vec![term("t1"), term("t2")], 0);
        // from == to is a no-op, order unchanged
        assert_eq!(root.first_terminal_id(), Some(&"t1".to_string()));
    }

    // --- close_other_tabs logic tests ---

    #[test]
    fn test_close_other_tabs_keeps_only_specified() {
        let mut root = tabs(vec![term("t1"), term("t2"), term("t3")], 1);
        // Simulate close_other_tabs: keep index 1 (t2)
        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut root
        {
            let keep = 1;
            let kept = children.remove(keep);
            children.clear();
            children.push(kept);
            *active_tab = 0;
        }
        assert_eq!(root.tab_count(), Some(1));
        assert_eq!(root.first_terminal_id(), Some(&"t2".to_string()));
        assert_eq!(root.active_tab_index(), Some(0));
    }

    #[test]
    fn test_close_other_tabs_collects_removed_ids() {
        let root = tabs(vec![term("t1"), term("t2"), term("t3")], 0);
        // Collecting IDs that would be destroyed (everything except index 1)
        if let LayoutNode::Tabs { children, .. } = &root {
            let keep = 1;
            let mut ids = Vec::new();
            for (i, child) in children.iter().enumerate() {
                if i != keep {
                    ids.extend(child.collect_terminal_ids());
                }
            }
            assert_eq!(ids, vec!["t1", "t3"]);
        }
    }

    // --- remove_project logic tests ---

    #[test]
    fn test_remove_project_collects_all_terminal_ids() {
        let root = tabs(vec![split_v(vec![term("t1"), term("t2")]), term("t3")], 0);
        let ids = root.collect_terminal_ids();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    // --- rename_terminal logic test ---

    #[test]
    fn test_terminal_names_map() {
        use std::collections::HashMap;
        let mut names: HashMap<String, String> = HashMap::new();
        // Empty name removes entry
        names.insert("t1".into(), "My Terminal".into());
        assert_eq!(names.get("t1").map(|s| s.as_str()), Some("My Terminal"));
        names.remove("t1");
        assert_eq!(names.get("t1"), None);
    }

    // --- serde serialization round-trip tests ---

    #[test]
    fn test_serialize_terminal() {
        let node = term("t1");
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.first_terminal_id(), Some(&"t1".to_string()));
    }

    #[test]
    fn test_serialize_split() {
        let node = split_v(vec![term("t1"), term("t2")]);
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.terminal_count(), 2);
        assert_eq!(loaded.find_terminal_path("t1"), Some(vec![0]));
        assert_eq!(loaded.find_terminal_path("t2"), Some(vec![1]));
    }

    #[test]
    fn test_serialize_tabs() {
        let node = tabs(vec![term("t1"), term("t2")], 1);
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.active_tab_index(), Some(1));
        assert_eq!(loaded.terminal_count(), 2);
    }

    #[test]
    fn test_serialize_nested() {
        let node = tabs(
            vec![
                split_v(vec![split_h(vec![term("t1"), term("t2")]), term("t3")]),
                term("t4"),
            ],
            0,
        );
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.terminal_count(), 4);
        assert_eq!(loaded.find_terminal_path("t1"), Some(vec![0, 0, 0]));
        assert_eq!(loaded.find_terminal_path("t4"), Some(vec![1]));
    }

    #[test]
    fn test_deserialize_clears_terminal_ids() {
        let node = term("t1");
        let json = serde_json::to_string(&node).unwrap();
        let mut loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        // Clear terminal IDs (simulating restore)
        if let LayoutNode::Terminal { terminal_id, .. } = &mut loaded {
            *terminal_id = None;
        }
        assert_eq!(loaded.first_terminal_id(), None);
    }

    #[test]
    fn test_serialize_working_directory() {
        let node = LayoutNode::Terminal {
            terminal_id: Some("t1".into()),
            working_directory: Some(PathBuf::from("/home/user/project")),
            zoom_level: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        if let LayoutNode::Terminal {
            working_directory, ..
        } = &loaded
        {
            assert_eq!(
                working_directory.as_deref(),
                Some(std::path::Path::new("/home/user/project"))
            );
        } else {
            panic!("expected Terminal");
        }
    }

    #[test]
    fn test_serialize_zoom_level() {
        let node = LayoutNode::Terminal {
            terminal_id: Some("t1".into()),
            working_directory: None,
            zoom_level: Some(2.0),
        };
        let json = serde_json::to_string(&node).unwrap();
        let loaded: LayoutNode = serde_json::from_str(&json).unwrap();
        if let LayoutNode::Terminal { zoom_level, .. } = &loaded {
            assert_eq!(*zoom_level, Some(2.0));
        } else {
            panic!("expected Terminal");
        }
    }

    #[test]
    fn test_deserialize_missing_new_fields() {
        // Old JSON without working_directory/zoom_level fields
        let json = r#"{"Terminal":{"terminal_id":"t1"}}"#;
        let loaded: LayoutNode = serde_json::from_str(json).unwrap();
        if let LayoutNode::Terminal {
            terminal_id,
            working_directory,
            zoom_level,
        } = &loaded
        {
            assert_eq!(terminal_id.as_deref(), Some("t1"));
            assert!(working_directory.is_none());
            assert!(zoom_level.is_none());
        } else {
            panic!("expected Terminal");
        }
    }

    // ── Auxiliary tab defaults ──────────────────────────────────────────

    #[test]
    fn test_settings_state_defaults() {
        use super::super::WorkspaceState;
        let ws = WorkspaceState::new();
        assert!(ws.auxiliary_tabs().is_empty());
        assert!(!ws.is_settings_focused());
    }

    #[test]
    fn test_build_terminal_config_uses_settings() {
        use super::super::WorkspaceState;
        use crate::settings::AppSettings;
        use crate::theme::OrcaTheme;
        let mut settings = AppSettings(orcashell_store::AppSettings::default());
        settings.font_size = 18.0;
        settings.font_family = "Fira Code".to_string();

        let config = WorkspaceState::build_terminal_config(&settings, &OrcaTheme::dark());
        assert_eq!(f32::from(config.font_size), 18.0);
        assert_eq!(config.font_family, "Fira Code");
    }

    #[test]
    fn test_terminal_display_name_prefers_custom_override() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        let mut project = super::super::project::ProjectData::new(PathBuf::from("/tmp/project"));
        project.id = "proj-1".into();
        project.terminal_names.insert("t1".into(), "Pinned".into());
        ws.projects.push(project);
        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: Some("cargo test".into()),
                semantic_state: SemanticState::Prompt,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );

        assert_eq!(ws.terminal_display_name("proj-1", "t1"), "Pinned");
    }

    #[test]
    fn test_terminal_display_name_falls_back_to_live_title_then_shell() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        let mut project = super::super::project::ProjectData::new(PathBuf::from("/tmp/project"));
        project.id = "proj-1".into();
        ws.projects.push(project);

        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: Some("nvim".into()),
                semantic_state: SemanticState::Prompt,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );
        assert_eq!(ws.terminal_display_name("proj-1", "t1"), "nvim");

        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: None,
                semantic_state: SemanticState::Unknown,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );
        assert_eq!(ws.terminal_display_name("proj-1", "t1"), "terminal");
    }

    #[test]
    fn test_terminal_display_name_uses_terminal_for_idle_project_root_title() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        let mut project = super::super::project::ProjectData::new(PathBuf::from("/tmp/orcashell"));
        project.id = "proj-1".into();
        ws.projects.push(project);

        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: Some("orcashell".into()),
                semantic_state: SemanticState::Prompt,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );

        assert_eq!(ws.terminal_display_name("proj-1", "t1"), "terminal");
    }

    #[test]
    fn test_terminal_display_name_keeps_executing_title_even_if_it_matches_project() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        let mut project = super::super::project::ProjectData::new(PathBuf::from("/tmp/orcashell"));
        project.id = "proj-1".into();
        ws.projects.push(project);

        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: Some("orcashell".into()),
                semantic_state: SemanticState::Executing,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );

        assert_eq!(ws.terminal_display_name("proj-1", "t1"), "orcashell");
    }

    #[test]
    fn test_terminal_executing_helper_tracks_semantic_state() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: None,
                semantic_state: SemanticState::Executing,
                last_activity_at: Some(std::time::Instant::now()),
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );
        ws.set_terminal_runtime_state(
            "t2".into(),
            TerminalRuntimeState {
                shell_label: "bash".into(),
                live_title: None,
                semantic_state: SemanticState::CommandComplete { exit_code: Some(0) },
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );

        assert!(ws.terminal_is_executing("t1"));
        assert!(!ws.terminal_is_executing("t2"));
        assert!(ws.terminal_should_pulse("t1"));
        assert!(!ws.terminal_should_pulse("t2"));
        assert!(ws.has_pulsing_terminals());
    }

    #[test]
    fn test_recent_local_input_suppresses_pulse() {
        use super::super::{TerminalRuntimeState, WorkspaceState};
        use orcashell_session::SemanticState;

        let mut ws = WorkspaceState::new();
        ws.set_terminal_runtime_state(
            "t1".into(),
            TerminalRuntimeState {
                shell_label: "zsh".into(),
                live_title: None,
                semantic_state: SemanticState::Executing,
                last_activity_at: Some(std::time::Instant::now()),
                last_local_input_at: Some(std::time::Instant::now()),
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );

        assert!(!ws.terminal_should_pulse("t1"));
        assert!(!ws.has_pulsing_terminals());
    }
}
