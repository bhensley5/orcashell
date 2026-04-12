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
mod tests;
