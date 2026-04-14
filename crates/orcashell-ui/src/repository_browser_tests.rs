use std::collections::HashSet;
use std::path::PathBuf;

use orcashell_git::{
    BranchTrackingInfo, CommitFileSelection, CommitGraphNode, GitSnapshotSummary, GraphLaneKind,
    GraphLaneSegment, HeadState, LocalBranchEntry, Oid, RemoteBranchEntry, RepositoryGraphDocument,
};

use crate::repository_browser::{
    active_lane_by_row, branch_rail_cache_key, center_pane_mode, clamp_repository_diff_scroll,
    commit_row_highlight_state, commit_rows_cache_key, detail_pane_mode, flatten_branch_rail,
    format_timestamp, graph_has_lane_overflow, graph_lane_accent, graph_participating_lanes,
    lane_shows_node, max_repository_branch_pane_width, max_repository_detail_pane_width,
    max_repository_diff_scroll, repository_branch_toolbar_state, repository_checkout_action_state,
    repository_graph_meta_lines, repository_pull_action_state, select_visible_graph_lanes,
    visible_first_parent_spine, BranchRailBranchKind, BranchRailRow, CommitRowHighlightState,
    GraphLaneAccent, RepositoryBranchToolbarState, RepositoryCenterPaneMode,
    RepositoryDetailPaneMode, RepositoryPullActionState,
};
use crate::workspace::{AsyncDocumentState, RepositoryBranchSelection, RepositoryGraphTabState};

fn oid(value: u64) -> Oid {
    Oid::from_str(&format!("{value:040x}")).unwrap()
}

fn graph_document() -> RepositoryGraphDocument {
    let main_oid = oid(1);
    let feature_oid = oid(2);
    RepositoryGraphDocument {
        scope_root: PathBuf::from("/repo"),
        repo_root: PathBuf::from("/repo"),
        head: HeadState::Branch {
            name: "main".to_string(),
            oid: main_oid,
        },
        local_branches: vec![
            LocalBranchEntry {
                name: "main".to_string(),
                full_ref: "refs/heads/main".to_string(),
                target: main_oid,
                is_head: true,
                upstream: Some(BranchTrackingInfo {
                    remote_name: "origin".to_string(),
                    remote_ref: "main".to_string(),
                    ahead: 1,
                    behind: 0,
                }),
            },
            LocalBranchEntry {
                name: "feature/a".to_string(),
                full_ref: "refs/heads/feature/a".to_string(),
                target: feature_oid,
                is_head: false,
                upstream: None,
            },
        ],
        remote_branches: vec![
            RemoteBranchEntry {
                remote_name: "origin".to_string(),
                short_name: "feature/a".to_string(),
                full_ref: "refs/remotes/origin/feature/a".to_string(),
                target: feature_oid,
                tracked_by_local: Some("feature/a".to_string()),
            },
            RemoteBranchEntry {
                remote_name: "origin".to_string(),
                short_name: "main".to_string(),
                full_ref: "refs/remotes/origin/main".to_string(),
                target: main_oid,
                tracked_by_local: Some("main".to_string()),
            },
        ],
        commits: Vec::new(),
        truncated: false,
    }
}

fn graph_document_with_long_local_upstream() -> RepositoryGraphDocument {
    let head_oid = oid(11);
    RepositoryGraphDocument {
        scope_root: PathBuf::from("/repo"),
        repo_root: PathBuf::from("/repo"),
        head: HeadState::Branch {
            name: "feature/repository-browser-layout".to_string(),
            oid: head_oid,
        },
        local_branches: vec![LocalBranchEntry {
            name: "feature/repository-browser-layout".to_string(),
            full_ref: "refs/heads/feature/repository-browser-layout".to_string(),
            target: head_oid,
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "customer-mirror".to_string(),
                remote_ref: "release/2026-hardened".to_string(),
                ahead: 12,
                behind: 4,
            }),
        }],
        remote_branches: Vec::new(),
        commits: Vec::new(),
        truncated: false,
    }
}

fn graph_document_with_long_local_name_and_short_upstream() -> RepositoryGraphDocument {
    let head_oid = oid(12);
    RepositoryGraphDocument {
        scope_root: PathBuf::from("/repo"),
        repo_root: PathBuf::from("/repo"),
        head: HeadState::Branch {
            name: "feature/repository-browser-layout".to_string(),
            oid: head_oid,
        },
        local_branches: vec![LocalBranchEntry {
            name: "feature/repository-browser-layout".to_string(),
            full_ref: "refs/heads/feature/repository-browser-layout".to_string(),
            target: head_oid,
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "main".to_string(),
                ahead: 2,
                behind: 1,
            }),
        }],
        remote_branches: Vec::new(),
        commits: Vec::new(),
        truncated: false,
    }
}

fn graph_document_with_medium_local_name_and_medium_upstream() -> RepositoryGraphDocument {
    let head_oid = oid(13);
    RepositoryGraphDocument {
        scope_root: PathBuf::from("/repo"),
        repo_root: PathBuf::from("/repo"),
        head: HeadState::Branch {
            name: "release/v0.3.0".to_string(),
            oid: head_oid,
        },
        local_branches: vec![LocalBranchEntry {
            name: "release/v0.3.0".to_string(),
            full_ref: "refs/heads/release/v0.3.0".to_string(),
            target: head_oid,
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "release/v0.3.0".to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: Vec::new(),
        commits: Vec::new(),
        truncated: false,
    }
}

fn commit(oid: Oid, primary_lane: u16, parent_oids: Vec<Oid>) -> CommitGraphNode {
    CommitGraphNode {
        oid,
        short_oid: oid.to_string()[..8].to_string(),
        summary: format!("commit-{primary_lane}"),
        author_name: "Orca".to_string(),
        authored_at_unix: 1_700_000_000,
        parent_oids,
        primary_lane,
        row_lanes: vec![GraphLaneSegment {
            lane: primary_lane,
            kind: GraphLaneKind::Through,
            target_lane: None,
        }],
        ref_labels: Vec::new(),
    }
}

fn graph_with_commits(head: HeadState, commits: Vec<CommitGraphNode>) -> RepositoryGraphDocument {
    RepositoryGraphDocument {
        scope_root: PathBuf::from("/repo"),
        repo_root: PathBuf::from("/repo"),
        head,
        local_branches: Vec::new(),
        remote_branches: Vec::new(),
        commits,
        truncated: false,
    }
}

fn tab_state() -> RepositoryGraphTabState {
    RepositoryGraphTabState {
        project_id: "project-1".to_string(),
        scope_root: PathBuf::from("/repo"),
        graph: AsyncDocumentState::default(),
        selected_branch: None,
        selected_commit: None,
        commit_detail: AsyncDocumentState::default(),
        selected_commit_file: None,
        commit_file_diff: AsyncDocumentState::default(),
        fetch_in_flight: false,
        pull_in_flight: false,
        active_fetch_origin: None,
        last_remote_check_at: None,
        last_automatic_fetch_failure_at: None,
        active_branch_action: None,
        action_banner: None,
        occupied_local_branches: HashSet::new(),
        expanded_remote_groups: HashSet::new(),
        remote_groups_seeded: false,
    }
}

fn snapshot(changed_files: usize) -> GitSnapshotSummary {
    GitSnapshotSummary {
        repo_root: PathBuf::from("/repo"),
        scope_root: PathBuf::from("/repo"),
        generation: 1,
        content_fingerprint: 1,
        branch_name: "main".to_string(),
        remotes: vec!["origin".to_string()],
        is_worktree: false,
        worktree_name: None,
        changed_files,
        insertions: 0,
        deletions: 0,
    }
}

#[test]
fn flatten_branch_rail_groups_sections_and_marks_worktree_rows() {
    let mut occupied = HashSet::new();
    occupied.insert("feature/a".to_string());
    let expanded = HashSet::from(["origin".to_string()]);

    let rows = flatten_branch_rail(&graph_document(), &occupied, &expanded, 320.0);
    assert_eq!(rows.len(), 7);

    assert!(matches!(
        &rows[0],
        BranchRailRow::SectionHeader { label } if label == "LOCAL"
    ));
    assert!(matches!(
        &rows[1],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "main".to_string(),
            }
            && row.kind == BranchRailBranchKind::Local
            && row.is_head
            && !row.is_worktree_occupied
            && row.inline_upstream.as_ref().is_some_and(|upstream|
                upstream.remote_ref == "origin/main" && upstream.ahead == 1 && upstream.behind == 0)
    ));
    assert!(matches!(
        &rows[2],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "feature/a".to_string(),
            }
            && row.kind == BranchRailBranchKind::Local
            && !row.is_head
            && row.is_worktree_occupied
            && row.inline_upstream.is_none()
    ));
    assert!(matches!(
        &rows[3],
        BranchRailRow::SectionHeader { label } if label == "REMOTE"
    ));
    assert!(matches!(
        &rows[4],
        BranchRailRow::RemoteGroup { remote_name, expanded } if remote_name == "origin" && *expanded
    ));
    assert!(matches!(
        &rows[5],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Remote {
                full_ref: "refs/remotes/origin/feature/a".to_string(),
            }
            && row.name == "feature/a"
            && row.kind == BranchRailBranchKind::Remote
            && row.inline_upstream.is_none()
    ));
    assert!(matches!(
        &rows[6],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Remote {
                full_ref: "refs/remotes/origin/main".to_string(),
            }
            && row.name == "main"
            && row.kind == BranchRailBranchKind::Remote
            && row.inline_upstream.is_none()
    ));
}

#[test]
fn flatten_branch_rail_collapses_remote_groups_when_not_expanded() {
    let rows = flatten_branch_rail(&graph_document(), &HashSet::new(), &HashSet::new(), 320.0);

    assert_eq!(rows.len(), 5);
    assert!(matches!(
        &rows[4],
        BranchRailRow::RemoteGroup { remote_name, expanded } if remote_name == "origin" && !expanded
    ));
}

#[test]
fn flatten_branch_rail_moves_long_local_upstream_to_metadata_row_when_narrow() {
    let rows = flatten_branch_rail(
        &graph_document_with_long_local_upstream(),
        &HashSet::new(),
        &HashSet::new(),
        220.0,
    );

    assert_eq!(rows.len(), 4);
    assert!(matches!(
        &rows[1],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "feature/repository-browser-layout".to_string(),
            }
            && row.inline_upstream.is_none()
    ));
    assert!(matches!(
        &rows[2],
        BranchRailRow::BranchMeta(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "feature/repository-browser-layout".to_string(),
            }
            && row.upstream.remote_ref == "customer-mirror/release/2026-hardened"
            && row.upstream.ahead == 12
            && row.upstream.behind == 4
    ));
}

#[test]
fn flatten_branch_rail_moves_upstream_below_when_it_would_ellipsize_branch_name() {
    let rows = flatten_branch_rail(
        &graph_document_with_long_local_name_and_short_upstream(),
        &HashSet::new(),
        &HashSet::new(),
        320.0,
    );

    assert_eq!(rows.len(), 4);
    assert!(matches!(
        &rows[1],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "feature/repository-browser-layout".to_string(),
            }
            && row.inline_upstream.is_none()
    ));
    assert!(matches!(
        &rows[2],
        BranchRailRow::BranchMeta(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "feature/repository-browser-layout".to_string(),
            }
            && row.upstream.remote_ref == "origin/main"
            && row.upstream.ahead == 2
            && row.upstream.behind == 1
    ));
}

#[test]
fn flatten_branch_rail_moves_medium_upstream_below_before_name_starts_truncating() {
    let rows = flatten_branch_rail(
        &graph_document_with_medium_local_name_and_medium_upstream(),
        &HashSet::new(),
        &HashSet::new(),
        320.0,
    );

    assert_eq!(rows.len(), 4);
    assert!(matches!(
        &rows[1],
        BranchRailRow::Branch(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "release/v0.3.0".to_string(),
            }
            && row.inline_upstream.is_none()
    ));
    assert!(matches!(
        &rows[2],
        BranchRailRow::BranchMeta(row)
            if row.selection == RepositoryBranchSelection::Local {
                name: "release/v0.3.0".to_string(),
            }
            && row.upstream.remote_ref == "origin/release/v0.3.0"
            && row.upstream.ahead == 0
            && row.upstream.behind == 0
    ));
}

#[test]
fn repository_graph_meta_reports_truncation_in_header_detail_only() {
    let mut graph = graph_document();
    graph.truncated = true;

    assert_eq!(
        repository_graph_meta_lines(Some(&graph)),
        ("0 commits".to_string(), Some("bounded view".to_string()))
    );
    assert_eq!(
        repository_graph_meta_lines(None),
        ("No graph".to_string(), None)
    );
}

#[test]
fn commit_rows_cache_key_ignores_expanded_remote_groups() {
    let graph = graph_document();
    let collapsed = HashSet::new();
    let expanded = HashSet::from(["origin".to_string()]);

    assert_ne!(
        branch_rail_cache_key(&graph, &HashSet::new(), &collapsed, 320.0),
        branch_rail_cache_key(&graph, &HashSet::new(), &expanded, 320.0)
    );
    assert_ne!(
        branch_rail_cache_key(&graph, &HashSet::new(), &collapsed, 220.0),
        branch_rail_cache_key(&graph, &HashSet::new(), &collapsed, 320.0)
    );
    assert_eq!(commit_rows_cache_key(&graph), commit_rows_cache_key(&graph));
}

#[test]
fn format_timestamp_uses_human_readable_utc_output() {
    assert_eq!(format_timestamp(1_700_000_100), "2023-11-14 22:15 UTC");
}

#[test]
fn checkout_action_disables_current_local_branch() {
    let graph = graph_document();
    let action = repository_checkout_action_state(
        &graph,
        &RepositoryBranchSelection::Local {
            name: "main".to_string(),
        },
        false,
    );

    assert!(action.disabled);
}

#[test]
fn checkout_action_disables_current_remote_tracking_branch() {
    let graph = graph_document();
    let action = repository_checkout_action_state(
        &graph,
        &RepositoryBranchSelection::Remote {
            full_ref: "refs/remotes/origin/main".to_string(),
        },
        false,
    );

    assert!(action.disabled);
}

#[test]
fn checkout_action_respects_scope_busy_for_checkoutable_branch() {
    let graph = graph_document();
    let action = repository_checkout_action_state(
        &graph,
        &RepositoryBranchSelection::Remote {
            full_ref: "refs/remotes/origin/feature/a".to_string(),
        },
        true,
    );

    assert!(action.disabled);
}

#[test]
fn toolbar_state_enables_local_branch_actions_for_non_current_branch() {
    let mut state = tab_state();
    state.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "feature/a".to_string(),
    });

    let toolbar = repository_branch_toolbar_state(&graph_document(), &state, false);
    assert_eq!(
        toolbar,
        RepositoryBranchToolbarState {
            checkout_disabled: false,
            create_disabled: false,
            delete_disabled: false,
        }
    );
}

#[test]
fn toolbar_state_disables_delete_for_current_branch() {
    let mut state = tab_state();
    state.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "main".to_string(),
    });

    let toolbar = repository_branch_toolbar_state(&graph_document(), &state, false);
    assert_eq!(
        toolbar,
        RepositoryBranchToolbarState {
            checkout_disabled: true,
            create_disabled: false,
            delete_disabled: true,
        }
    );
}

#[test]
fn pull_action_uses_current_head_branch_tracking_state() {
    let mut graph = graph_document();
    graph.local_branches[0].upstream.as_mut().unwrap().behind = 3;

    let pull = repository_pull_action_state(Some(&graph), Some(&snapshot(0)), false, false);
    assert_eq!(
        pull,
        RepositoryPullActionState {
            disabled: false,
            label: "Pull (3)".to_string(),
        }
    );
}

#[test]
fn pull_action_disables_when_scope_is_dirty() {
    let mut graph = graph_document();
    graph.local_branches[0].upstream.as_mut().unwrap().behind = 2;

    let pull = repository_pull_action_state(Some(&graph), Some(&snapshot(1)), false, false);
    assert!(pull.disabled);
    assert_eq!(pull.label, "Pull (2)");
}

#[test]
fn pull_action_disables_when_current_branch_is_not_behind() {
    let pull =
        repository_pull_action_state(Some(&graph_document()), Some(&snapshot(0)), false, false);
    assert_eq!(
        pull,
        RepositoryPullActionState {
            disabled: true,
            label: "Pull".to_string(),
        }
    );
}

#[test]
fn pull_action_shows_in_flight_label() {
    let mut graph = graph_document();
    graph.local_branches[0].upstream.as_mut().unwrap().behind = 4;

    let pull = repository_pull_action_state(Some(&graph), Some(&snapshot(0)), false, true);
    assert_eq!(
        pull,
        RepositoryPullActionState {
            disabled: true,
            label: "Pulling…".to_string(),
        }
    );
}

#[test]
fn pane_modes_show_history_and_branch_detail_for_branch_selection() {
    let mut state = tab_state();
    state.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "main".to_string(),
    });

    assert_eq!(center_pane_mode(&state), RepositoryCenterPaneMode::History);
    assert_eq!(detail_pane_mode(&state), RepositoryDetailPaneMode::Branch);
}

#[test]
fn pane_modes_keep_commit_detail_visible_while_middle_pane_shows_file_diff() {
    let mut state = tab_state();
    let commit_oid = oid(7);
    state.selected_commit = Some(commit_oid);
    state.selected_commit_file = Some(CommitFileSelection {
        commit_oid,
        relative_path: PathBuf::from("src/lib.rs"),
    });

    assert_eq!(
        center_pane_mode(&state),
        RepositoryCenterPaneMode::CommitFileDiff
    );
    assert_eq!(detail_pane_mode(&state), RepositoryDetailPaneMode::Commit);
}

#[test]
fn visible_first_parent_spine_follows_head_branch_only() {
    let head = oid(10);
    let parent = oid(9);
    let grandparent = oid(8);
    let graph = graph_with_commits(
        HeadState::Branch {
            name: "main".to_string(),
            oid: head,
        },
        vec![
            commit(head, 0, vec![parent]),
            commit(oid(12), 1, vec![parent]),
            commit(parent, 0, vec![grandparent]),
            commit(grandparent, 0, Vec::new()),
        ],
    );

    assert_eq!(
        visible_first_parent_spine(&graph),
        vec![head, parent, grandparent]
    );
}

#[test]
fn active_lane_by_row_marks_rows_between_spine_commits() {
    let head = oid(10);
    let parent = oid(9);
    let grandparent = oid(8);
    let commits = vec![
        commit(head, 1, vec![parent]),
        commit(oid(12), 0, vec![parent]),
        commit(parent, 0, vec![grandparent]),
        commit(grandparent, 0, Vec::new()),
    ];

    assert_eq!(
        active_lane_by_row(
            &commits,
            &HeadState::Branch {
                name: "main".to_string(),
                oid: head,
            }
        ),
        vec![Some(1), Some(1), Some(0), Some(0)]
    );
}

#[test]
fn active_lane_by_row_is_empty_for_detached_head() {
    let commits = vec![commit(oid(1), 0, Vec::new())];

    assert_eq!(
        active_lane_by_row(&commits, &HeadState::Detached { oid: oid(1) }),
        vec![None]
    );
}

#[test]
fn graph_lane_accent_prioritizes_active_lane() {
    assert_eq!(graph_lane_accent(2, Some(2), None), GraphLaneAccent::Active);
    assert_eq!(graph_lane_accent(3, None, Some(3)), GraphLaneAccent::Active);
    assert_eq!(graph_lane_accent(0, None, None), GraphLaneAccent::Green);
    assert_eq!(graph_lane_accent(1, None, None), GraphLaneAccent::Amber);
    assert_eq!(graph_lane_accent(2, None, None), GraphLaneAccent::Fog);
    assert_eq!(graph_lane_accent(3, None, None), GraphLaneAccent::Slate);
}

#[test]
fn graph_participating_lanes_include_merge_targets() {
    let merge_commit = CommitGraphNode {
        oid: oid(21),
        short_oid: "00000015".to_string(),
        summary: "merge".to_string(),
        author_name: "Orca".to_string(),
        authored_at_unix: 1_700_000_000,
        parent_oids: vec![oid(20)],
        primary_lane: 2,
        row_lanes: vec![
            GraphLaneSegment {
                lane: 2,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 6,
                kind: GraphLaneKind::MergeFromRight,
                target_lane: Some(2),
            },
        ],
        ref_labels: Vec::new(),
    };

    assert_eq!(graph_participating_lanes(&merge_commit), vec![2, 6]);
}

#[test]
fn visible_graph_lanes_clip_to_first_six_actual_lanes() {
    let commit = CommitGraphNode {
        oid: oid(30),
        short_oid: "0000001e".to_string(),
        summary: "dense".to_string(),
        author_name: "Orca".to_string(),
        authored_at_unix: 1_700_000_000,
        parent_oids: vec![oid(29)],
        primary_lane: 7,
        row_lanes: vec![
            GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 1,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 2,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 3,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 6,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 7,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 8,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
        ],
        ref_labels: Vec::new(),
    };

    assert_eq!(
        select_visible_graph_lanes(&commit, Some(6)),
        vec![0, 1, 2, 3]
    );
}

#[test]
fn lane_overflow_detects_sparse_hidden_high_lane() {
    let commit = CommitGraphNode {
        oid: oid(31),
        short_oid: "0000001f".to_string(),
        summary: "sparse-dense".to_string(),
        author_name: "Orca".to_string(),
        authored_at_unix: 1_700_000_000,
        parent_oids: vec![oid(30)],
        primary_lane: 2,
        row_lanes: vec![
            GraphLaneSegment {
                lane: 2,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 6,
                kind: GraphLaneKind::MergeFromRight,
                target_lane: Some(2),
            },
        ],
        ref_labels: Vec::new(),
    };

    assert!(graph_has_lane_overflow(&commit));
    assert_eq!(select_visible_graph_lanes(&commit, Some(2)), vec![2]);
}

#[test]
fn highlight_state_keeps_selection_neutral() {
    assert_eq!(
        commit_row_highlight_state(true, true),
        CommitRowHighlightState::Selected
    );
    assert_eq!(
        commit_row_highlight_state(false, true),
        CommitRowHighlightState::CurrentTip
    );
    assert_eq!(
        commit_row_highlight_state(false, false),
        CommitRowHighlightState::HoverOnly
    );
}

#[test]
fn node_renders_only_on_primary_non_merge_lane() {
    assert!(lane_shows_node(
        1,
        GraphLaneSegment {
            lane: 1,
            kind: GraphLaneKind::Through,
            target_lane: None,
        }
    ));
    assert!(!lane_shows_node(
        0,
        GraphLaneSegment {
            lane: 1,
            kind: GraphLaneKind::Through,
            target_lane: None,
        }
    ));
    assert!(!lane_shows_node(
        1,
        GraphLaneSegment {
            lane: 1,
            kind: GraphLaneKind::MergeFromRight,
            target_lane: Some(0),
        }
    ));
}

#[test]
fn repository_pane_width_limits_reserve_center_space() {
    assert_eq!(max_repository_branch_pane_width(1400.0, 360.0), 712.0);
    assert_eq!(max_repository_detail_pane_width(1400.0, 280.0), 792.0);
}

#[test]
fn repository_diff_scroll_helpers_clamp_to_visible_range() {
    let max_scroll = max_repository_diff_scroll(200, 6.5, 640.0);
    assert!(max_scroll > 0.0);
    assert_eq!(clamp_repository_diff_scroll(0.0, -120.0, max_scroll), 120.0);
    assert_eq!(
        clamp_repository_diff_scroll(max_scroll, -120.0, max_scroll),
        max_scroll
    );
    assert_eq!(clamp_repository_diff_scroll(40.0, 80.0, max_scroll), 0.0);
}
