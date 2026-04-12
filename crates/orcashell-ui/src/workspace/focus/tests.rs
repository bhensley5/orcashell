// Explicit imports to avoid pulling in GPUI types that blow the
// gpui_macros proc-macro stack during test compilation.
use super::{FocusManager, FocusTarget};

#[test]
fn test_set_and_get_current() {
    let mut fm = FocusManager::new();
    fm.set_current(FocusTarget {
        project_id: "proj-1".into(),
        layout_path: vec![0, 1],
    });
    let target = fm.current_target().unwrap();
    assert_eq!(target.project_id, "proj-1");
    assert_eq!(target.layout_path, vec![0, 1]);
}

#[test]
fn test_is_focused_matching() {
    let mut fm = FocusManager::new();
    fm.set_current(FocusTarget {
        project_id: "proj-1".into(),
        layout_path: vec![0],
    });
    assert!(fm.is_focused("proj-1", &[0]));
    assert!(!fm.is_focused("proj-1", &[1]));
    assert!(!fm.is_focused("proj-2", &[0]));
}

#[test]
fn test_is_focused_when_none() {
    let fm = FocusManager::new();
    assert!(!fm.is_focused("proj-1", &[0]));
}

#[test]
fn test_clear() {
    let mut fm = FocusManager::new();
    fm.set_current(FocusTarget {
        project_id: "proj-1".into(),
        layout_path: vec![],
    });
    assert!(fm.current_target().is_some());
    fm.clear();
    assert!(fm.current_target().is_none());
}

/// Simulate focus next/prev cycle using collect_terminal_paths.
/// This tests the logic that WorkspaceState::cycle_focus uses.
#[test]
fn test_focus_cycle_wrap_around() {
    use crate::workspace::layout::LayoutNode;

    fn term(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            terminal_id: Some(id.to_string()),
            working_directory: None,
            zoom_level: None,
        }
    }
    fn split_v(children: Vec<LayoutNode>) -> LayoutNode {
        LayoutNode::Split {
            direction: crate::workspace::layout::SplitDirection::Vertical,
            sizes: vec![1.0; children.len()],
            children,
        }
    }

    let layout = split_v(vec![term("t1"), term("t2"), term("t3")]);
    let paths = layout.collect_terminal_paths();
    assert_eq!(paths.len(), 3);

    let mut fm = FocusManager::new();
    fm.set_current(FocusTarget {
        project_id: "p".into(),
        layout_path: paths[2].clone(), // last pane
    });

    // Simulate focus_next: should wrap to first
    let current_idx = paths
        .iter()
        .position(|p| *p == fm.current_target().unwrap().layout_path)
        .unwrap();
    let next_idx = (current_idx as isize + 1).rem_euclid(paths.len() as isize) as usize;
    assert_eq!(next_idx, 0);

    // Simulate focus_prev from first: should wrap to last
    fm.set_current(FocusTarget {
        project_id: "p".into(),
        layout_path: paths[0].clone(),
    });
    let current_idx = paths
        .iter()
        .position(|p| *p == fm.current_target().unwrap().layout_path)
        .unwrap();
    let prev_idx = (current_idx as isize - 1).rem_euclid(paths.len() as isize) as usize;
    assert_eq!(prev_idx, 2);
}

#[test]
fn test_focus_cycle_single_pane_noop() {
    use crate::workspace::layout::LayoutNode;

    let layout = LayoutNode::Terminal {
        terminal_id: Some("t1".into()),
        working_directory: None,
        zoom_level: None,
    };
    let paths = layout.collect_terminal_paths();
    assert_eq!(paths.len(), 1);

    let current_idx = 0;
    let next_idx = (current_idx as isize + 1).rem_euclid(paths.len() as isize) as usize;
    assert_eq!(next_idx, 0); // stays on same pane
}

#[test]
fn test_focus_next_in_nested_splits_dfs_order() {
    use crate::workspace::layout::{LayoutNode, SplitDirection};

    fn term(id: &str) -> LayoutNode {
        LayoutNode::Terminal {
            terminal_id: Some(id.to_string()),
            working_directory: None,
            zoom_level: None,
        }
    }

    // Tree: Split(V, [Split(H, [t1, t2]), t3])
    let layout = LayoutNode::Split {
        direction: SplitDirection::Vertical,
        sizes: vec![1.0, 1.0],
        children: vec![
            LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                sizes: vec![1.0, 1.0],
                children: vec![term("t1"), term("t2")],
            },
            term("t3"),
        ],
    };

    let paths = layout.collect_terminal_paths();
    // DFS order: t1 at [0,0], t2 at [0,1], t3 at [1]
    assert_eq!(paths, vec![vec![0, 0], vec![0, 1], vec![1]]);
}
