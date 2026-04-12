use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext, TestAppContext};
use orcashell_daemon_core::git_coordinator::GitEvent;
use orcashell_git::{
    BranchTrackingInfo, CommitChangedFile, CommitDetailDocument, CommitFileDiffDocument,
    CommitFileSelection, CommitFileStatus, CommitGraphNode, CommitRefKind, CommitRefLabel,
    DiffLineKind, DiffLineView, GitFileStatus, GitSnapshotSummary, GraphLaneKind, GraphLaneSegment,
    HeadState, LocalBranchEntry, Oid, RemoteBranchEntry, RepositoryGraphDocument,
};
use orcashell_store::Store;
use orcashell_ui::app_view::ContextMenuRequest;
use orcashell_ui::settings::AppSettings;
use orcashell_ui::sidebar::Sidebar;
use orcashell_ui::theme::{resolve_theme, SystemAppearance};
use orcashell_ui::workspace::{RepositoryBranchSelection, WorkspaceServices, WorkspaceState};
use parking_lot::Mutex;
use tempfile::tempdir;

fn services() -> WorkspaceServices {
    WorkspaceServices {
        git: orcashell_daemon_core::git_coordinator::GitCoordinator::new(),
        store: Arc::new(Mutex::new(Some(Store::open_in_memory().unwrap()))),
    }
}

fn oid(value: u64) -> Oid {
    Oid::from_str(&format!("{value:040x}")).unwrap()
}

fn local_branch(name: &str, target: Oid, is_head: bool) -> LocalBranchEntry {
    LocalBranchEntry {
        name: name.to_string(),
        full_ref: format!("refs/heads/{name}"),
        target,
        is_head,
        upstream: Some(BranchTrackingInfo {
            remote_name: "origin".to_string(),
            remote_ref: name.to_string(),
            ahead: 0,
            behind: 0,
        }),
    }
}

fn remote_branch(remote_name: &str, short_name: &str, target: Oid) -> RemoteBranchEntry {
    RemoteBranchEntry {
        remote_name: remote_name.to_string(),
        short_name: short_name.to_string(),
        full_ref: format!("refs/remotes/{remote_name}/{short_name}"),
        target,
        tracked_by_local: None,
    }
}

fn commit_node(oid: Oid, summary: &str, refs: Vec<CommitRefLabel>) -> CommitGraphNode {
    CommitGraphNode {
        oid,
        short_oid: oid.to_string()[..8].to_string(),
        summary: summary.to_string(),
        author_name: "Orca".to_string(),
        authored_at_unix: 1_700_000_000,
        parent_oids: Vec::new(),
        primary_lane: 0,
        row_lanes: vec![GraphLaneSegment {
            lane: 0,
            kind: GraphLaneKind::Start,
            target_lane: None,
        }],
        ref_labels: refs,
    }
}

fn graph_document(
    scope_root: &str,
    local_branches: Vec<LocalBranchEntry>,
    remote_branches: Vec<RemoteBranchEntry>,
    commits: Vec<CommitGraphNode>,
) -> RepositoryGraphDocument {
    graph_document_with_truncated(scope_root, local_branches, remote_branches, commits, false)
}

fn graph_document_with_truncated(
    scope_root: &str,
    local_branches: Vec<LocalBranchEntry>,
    remote_branches: Vec<RemoteBranchEntry>,
    commits: Vec<CommitGraphNode>,
    truncated: bool,
) -> RepositoryGraphDocument {
    let head = local_branches
        .iter()
        .find(|branch| branch.is_head)
        .map(|branch| HeadState::Branch {
            name: branch.name.clone(),
            oid: branch.target,
        })
        .unwrap_or(HeadState::Unborn);
    RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head,
        local_branches,
        remote_branches,
        commits,
        truncated,
    }
}

fn commit_detail(oid: Oid, path: &str) -> CommitDetailDocument {
    CommitDetailDocument {
        oid,
        short_oid: oid.to_string()[..8].to_string(),
        summary: "Selected commit".to_string(),
        message_body: "Body".to_string(),
        author_name: "Orca".to_string(),
        author_email: "orca@example.com".to_string(),
        authored_at_unix: 1_700_000_000,
        committer_name: "Orca".to_string(),
        committer_email: "orca@example.com".to_string(),
        committed_at_unix: 1_700_000_100,
        parent_oids: Vec::new(),
        changed_files: vec![CommitChangedFile {
            path: PathBuf::from(path),
            status: CommitFileStatus::Modified,
            additions: 3,
            deletions: 1,
        }],
    }
}

fn commit_file_diff(commit_oid: Oid, path: &str) -> CommitFileDiffDocument {
    CommitFileDiffDocument {
        commit_oid,
        parent_oid: None,
        selection: CommitFileSelection {
            commit_oid,
            relative_path: PathBuf::from(path),
        },
        file: orcashell_git::ChangedFile {
            relative_path: PathBuf::from(path),
            status: GitFileStatus::Modified,
            is_binary: false,
            insertions: 3,
            deletions: 1,
        },
        lines: vec![DiffLineView {
            kind: DiffLineKind::Context,
            old_lineno: Some(1),
            new_lineno: Some(1),
            text: "context".to_string(),
            highlights: None,
            inline_changes: None,
        }],
    }
}

fn open_workspace(cx: &mut TestAppContext) -> (gpui::Entity<WorkspaceState>, String, PathBuf) {
    let settings = AppSettings(orcashell_store::AppSettings::default());
    let appearance = gpui::WindowAppearance::Dark;
    let resolved = resolve_theme(&settings, appearance);
    cx.set_global(settings);
    cx.set_global(SystemAppearance(appearance));
    cx.set_global(resolved);

    let dir = tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let ws = cx.new(|_| WorkspaceState::new_with_services(services()));
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.init_with_project(path.clone(), cx);
    });
    let (project_id, project_path) = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let project = ws.active_project().unwrap();
        (project.id.clone(), project.path.clone())
    });
    std::mem::forget(dir);
    (ws, project_id, project_path)
}

#[gpui::test]
fn repository_graph_tab_is_project_singleton_and_closes_cleanly(cx: &mut TestAppContext) {
    let (ws, project_id, _) = open_workspace(cx);

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.open_repository_graph_for_project(&project_id, cx);
        ws.open_repository_graph_for_project(&project_id, cx);
    });

    let tab_id = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let tab = ws.active_auxiliary_tab().unwrap();
        assert_eq!(
            tab.title,
            format!("Repository: {}", ws.active_project().unwrap().name)
        );
        tab.id.clone()
    });
    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        assert!(ws.repository_graph_state(&project_id).is_some());
        assert!(matches!(
            ws.active_auxiliary_tab().map(|tab| &tab.kind),
            Some(orcashell_ui::workspace::AuxiliaryTabKind::RepositoryGraph { project_id: tab_pid })
                if tab_pid == &project_id
        ));
    });

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.close_auxiliary_tab(&tab_id, cx)
    });
    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        assert!(ws.repository_graph_state(&project_id).is_none());
        assert!(ws.active_auxiliary_tab().is_none());
    });
}

#[gpui::test]
fn sidebar_project_actions_render_in_spec_order(cx: &mut TestAppContext) {
    let (ws, _project_id, scope_root) = open_workspace(cx);
    let terminal_id = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.active_project()
            .unwrap()
            .layout
            .collect_terminal_ids()
            .into_iter()
            .next()
            .unwrap()
    });

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::SnapshotUpdated {
                terminal_ids: vec![terminal_id.clone()],
                request_path: scope_root.clone(),
                scope_root: Some(scope_root.clone()),
                result: Ok(GitSnapshotSummary {
                    repo_root: scope_root.clone(),
                    scope_root: scope_root.clone(),
                    generation: 1,
                    content_fingerprint: 1,
                    branch_name: "main".to_string(),
                    remotes: vec!["origin".to_string()],
                    is_worktree: false,
                    worktree_name: None,
                    changed_files: 0,
                    insertions: 0,
                    deletions: 0,
                }),
            },
            cx,
        );
    });

    let menu_request: ContextMenuRequest = Rc::new(RefCell::new(None));
    let (_sidebar, cx) =
        cx.add_window_view(|_window, cx| Sidebar::new(ws.clone(), menu_request.clone(), cx));

    let live_diff = cx.debug_bounds("sidebar-live-diff-action").unwrap();
    let repository = cx.debug_bounds("sidebar-repository-action").unwrap();
    let add_terminal = cx.debug_bounds("sidebar-add-term-action").unwrap();

    assert!(live_diff.origin.x < repository.origin.x);
    assert!(repository.origin.x < add_terminal.origin.x);
}

#[gpui::test]
fn repository_graph_events_select_head_and_reconcile_missing_branch(cx: &mut TestAppContext) {
    let (ws, project_id, scope_root) = open_workspace(cx);

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.open_repository_graph_for_project(&project_id, cx)
    });
    let request_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .graph
            .requested_revision
    });

    let main_oid = oid(1);
    let feature_oid = oid(2);
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::RepositoryGraphLoaded {
                scope_root: scope_root.clone(),
                request_revision,
                result: Ok(graph_document(
                    scope_root.to_string_lossy().as_ref(),
                    vec![
                        local_branch("main", main_oid, true),
                        local_branch("feature/a", feature_oid, false),
                    ],
                    vec![remote_branch("origin", "feature/a", feature_oid)],
                    vec![commit_node(
                        main_oid,
                        "Initial",
                        vec![CommitRefLabel {
                            name: "main".to_string(),
                            kind: CommitRefKind::Head,
                        }],
                    )],
                )),
            },
            cx,
        );
        ws.select_repository_branch(
            &project_id,
            RepositoryBranchSelection::Local {
                name: "feature/a".to_string(),
            },
            cx,
        );
    });

    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        assert_eq!(
            ws.repository_graph_state(&project_id)
                .unwrap()
                .selected_branch,
            Some(RepositoryBranchSelection::Local {
                name: "feature/a".to_string()
            })
        );
    });

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::RepositoryGraphLoaded {
                scope_root: scope_root.clone(),
                request_revision: request_revision + 1,
                result: Ok(graph_document(
                    scope_root.to_string_lossy().as_ref(),
                    vec![local_branch("main", main_oid, true)],
                    Vec::new(),
                    vec![commit_node(main_oid, "Initial", Vec::new())],
                )),
            },
            cx,
        );
    });

    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let tab = ws.repository_graph_state(&project_id).unwrap();
        assert_eq!(
            tab.selected_branch,
            Some(RepositoryBranchSelection::Local {
                name: "main".to_string()
            })
        );
        assert_eq!(tab.selected_commit, None);
    });
}

#[gpui::test]
fn repository_commit_file_selection_round_trips_back_to_commit(cx: &mut TestAppContext) {
    let (ws, project_id, scope_root) = open_workspace(cx);

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.open_repository_graph_for_project(&project_id, cx)
    });
    let graph_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .graph
            .requested_revision
    });
    let commit_oid = oid(9);
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::RepositoryGraphLoaded {
                scope_root: scope_root.clone(),
                request_revision: graph_revision,
                result: Ok(graph_document(
                    scope_root.to_string_lossy().as_ref(),
                    vec![local_branch("main", commit_oid, true)],
                    Vec::new(),
                    vec![commit_node(commit_oid, "Selected commit", Vec::new())],
                )),
            },
            cx,
        );
        ws.select_repository_commit(&project_id, commit_oid, cx);
    });

    let detail_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .commit_detail
            .requested_revision
    });
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::CommitDetailLoaded {
                scope_root: scope_root.clone(),
                oid: commit_oid,
                request_revision: detail_revision,
                result: Ok(commit_detail(commit_oid, "src/lib.rs")),
            },
            cx,
        );
        ws.select_repository_commit_file(
            &project_id,
            CommitFileSelection {
                commit_oid,
                relative_path: PathBuf::from("src/lib.rs"),
            },
            cx,
        );
    });

    let file_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .commit_file_diff
            .requested_revision
    });
    let selection = CommitFileSelection {
        commit_oid,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::CommitFileDiffLoaded {
                scope_root: scope_root.clone(),
                selection: selection.clone(),
                request_revision: file_revision,
                result: Ok(commit_file_diff(commit_oid, "src/lib.rs")),
            },
            cx,
        );
        ws.back_to_repository_commit(&project_id, cx);
    });

    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let tab = ws.repository_graph_state(&project_id).unwrap();
        assert_eq!(tab.selected_commit, Some(commit_oid));
        assert!(tab.selected_commit_file.is_none());
    });
}

#[gpui::test]
fn repository_commit_file_diff_failure_returns_to_commit_mode_with_banner(cx: &mut TestAppContext) {
    let (ws, project_id, scope_root) = open_workspace(cx);

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.open_repository_graph_for_project(&project_id, cx)
    });
    let graph_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .graph
            .requested_revision
    });
    let commit_oid = oid(11);
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::RepositoryGraphLoaded {
                scope_root: scope_root.clone(),
                request_revision: graph_revision,
                result: Ok(graph_document(
                    scope_root.to_string_lossy().as_ref(),
                    vec![local_branch("main", commit_oid, true)],
                    Vec::new(),
                    vec![commit_node(commit_oid, "Selected commit", Vec::new())],
                )),
            },
            cx,
        );
        ws.select_repository_commit(&project_id, commit_oid, cx);
    });

    let detail_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .commit_detail
            .requested_revision
    });
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::CommitDetailLoaded {
                scope_root: scope_root.clone(),
                oid: commit_oid,
                request_revision: detail_revision,
                result: Ok(commit_detail(commit_oid, "src/lib.rs")),
            },
            cx,
        );
        ws.select_repository_commit_file(
            &project_id,
            CommitFileSelection {
                commit_oid,
                relative_path: PathBuf::from("src/lib.rs"),
            },
            cx,
        );
    });
    let file_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .commit_file_diff
            .requested_revision
    });
    let selection = CommitFileSelection {
        commit_oid,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::CommitFileDiffLoaded {
                scope_root: scope_root.clone(),
                selection,
                request_revision: file_revision,
                result: Err("diff unavailable".to_string()),
            },
            cx,
        );
    });

    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let tab = ws.repository_graph_state(&project_id).unwrap();
        assert_eq!(tab.selected_commit, Some(commit_oid));
        assert!(tab.selected_commit_file.is_none());
        assert_eq!(
            tab.action_banner.as_ref().map(|banner| banner.kind.clone()),
            Some(orcashell_ui::workspace::ActionBannerKind::Error)
        );
    });

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.dismiss_repository_action_banner(&project_id, cx);
    });
    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        assert!(ws
            .repository_graph_state(&project_id)
            .unwrap()
            .action_banner
            .is_none());
    });
}

#[gpui::test]
fn repository_graph_loaded_retains_truncated_state(cx: &mut TestAppContext) {
    let (ws, project_id, scope_root) = open_workspace(cx);

    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.open_repository_graph_for_project(&project_id, cx)
    });
    let graph_revision = cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        ws.repository_graph_state(&project_id)
            .unwrap()
            .graph
            .requested_revision
    });
    let commit_oid = oid(12);
    cx.update_entity(&ws, |ws: &mut WorkspaceState, cx| {
        ws.handle_git_event(
            GitEvent::RepositoryGraphLoaded {
                scope_root: scope_root.clone(),
                request_revision: graph_revision,
                result: Ok(graph_document_with_truncated(
                    scope_root.to_string_lossy().as_ref(),
                    vec![local_branch("main", commit_oid, true)],
                    Vec::new(),
                    vec![commit_node(commit_oid, "Selected commit", Vec::new())],
                    true,
                )),
            },
            cx,
        );
    });

    cx.read_entity(&ws, |ws: &WorkspaceState, _| {
        let tab = ws.repository_graph_state(&project_id).unwrap();
        assert!(tab.graph.document.as_ref().unwrap().truncated);
        assert_eq!(
            tab.selected_branch,
            Some(RepositoryBranchSelection::Local {
                name: "main".to_string()
            })
        );
    });
}
