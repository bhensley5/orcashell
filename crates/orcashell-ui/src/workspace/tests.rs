use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use orcashell_daemon_core::git_coordinator::{
    GitActionKind, GitFetchOrigin, GitRemoteKind, MergeConflictTrigger,
};
use parking_lot::Mutex;

// Import only the types tests actually need. Avoid `use super::*` which
// pulls in GPUI types and blows the gpui_macros proc-macro stack budget
// during test compilation (same pattern as orcashell-terminal-view/search.rs).
use super::{
    classify_notification, merge_conflict_document_from_text, ActionBannerKind, AuxiliaryTabKind,
    AuxiliaryTabState, CapturedDiffFailure, CapturedDiffFile, CapturedEventDiff, ChangeFeedEntry,
    ChangeFeedState, ConflictDocumentState, ConflictEditorDocument, ConflictEditorState,
    DiffTabState, FeedCaptureState, FeedEntryOrigin, FeedScopeKind, FocusTarget, GitEvent,
    GitSnapshotSummary, LayoutNode, NotificationTier, ProjectData, RepositoryBranchAction,
    RepositoryBranchSelection, ResumableAgentKind, ResumeInjectionTrigger, TerminalRuntimeState,
    WorkspaceBannerKind, WorkspaceServices, WorkspaceState, FEED_PREVIEW_FILE_CAP,
    FEED_PREVIEW_LINE_BUDGET, SETTINGS_TAB_ID,
};
use crate::settings::ThemeId;
use orcashell_git::{
    BranchTrackingInfo, ChangedFile, CommitGraphNode, DiffDocument, DiffSectionKind,
    DiffSelectionKey, FeedCaptureResult, FeedEventData, FeedEventFileSummary, FeedEventKind,
    FeedLayerSnapshot, FeedLayerState, FeedScopeCapture, FileDiffDocument, FileDiffHunk,
    GitFileStatus, GitTrackingStatus, GraphLaneKind, GraphLaneSegment, HeadState, LocalBranchEntry,
    Oid, RemoteBranchEntry, RepositoryGraphDocument, SnapshotLoadError, StashListDocument,
    FEED_EVENT_LINE_CAP,
};
use orcashell_session::semantic_zone::SemanticState;
use orcashell_store::{Store, StoredWorktree};
use uuid::Uuid;

fn term(id: &str) -> LayoutNode {
    LayoutNode::Terminal {
        terminal_id: Some(id.to_string()),
        working_directory: None,
        zoom_level: None,
    }
}

fn tabs(children: Vec<LayoutNode>, active_tab: usize) -> LayoutNode {
    LayoutNode::Tabs {
        children,
        active_tab,
    }
}

fn project(id: &str, layout: LayoutNode) -> ProjectData {
    ProjectData {
        id: id.to_string(),
        name: id.to_string(),
        path: PathBuf::from(format!("/tmp/{id}")),
        layout,
        terminal_names: HashMap::new(),
    }
}

fn runtime_state(notification_tier: Option<NotificationTier>) -> TerminalRuntimeState {
    TerminalRuntimeState {
        shell_label: "zsh".into(),
        live_title: None,
        semantic_state: SemanticState::Prompt,
        last_activity_at: None,
        last_local_input_at: None,
        notification_tier,
        resumable_agent: None,
        pending_agent_detection: false,
    }
}

fn workspace_with_store() -> WorkspaceState {
    let services = WorkspaceServices {
        git: orcashell_daemon_core::git_coordinator::GitCoordinator::new(),
        store: Arc::new(Mutex::new(Some(Store::open_in_memory().unwrap()))),
    };
    WorkspaceState::new_with_services(services)
}

fn snapshot(scope_root: &str) -> GitSnapshotSummary {
    snapshot_with(scope_root, 1, "main")
}

fn snapshot_with(scope_root: &str, generation: u64, branch_name: &str) -> GitSnapshotSummary {
    let scope_root = PathBuf::from(scope_root);
    GitSnapshotSummary {
        repo_root: scope_root.clone(),
        scope_root,
        generation,
        content_fingerprint: generation,
        branch_name: branch_name.into(),
        remotes: vec!["origin".into()],
        is_worktree: false,
        worktree_name: None,
        changed_files: 1,
        insertions: 2,
        deletions: 1,
    }
}

fn changed_file(path: &str, status: GitFileStatus) -> ChangedFile {
    ChangedFile {
        relative_path: PathBuf::from(path),
        status,
        is_binary: false,
        insertions: 3,
        deletions: 1,
    }
}

fn diff_document(
    scope_root: &str,
    generation: u64,
    branch_name: &str,
    files: Vec<ChangedFile>,
) -> DiffDocument {
    DiffDocument {
        snapshot: snapshot_with(scope_root, generation, branch_name),
        tracking: GitTrackingStatus {
            upstream_ref: None,
            ahead: 0,
            behind: 0,
        },
        merge_state: None,
        repo_state_warning: None,
        conflicted_files: Vec::new(),
        staged_files: Vec::new(),
        unstaged_files: files,
    }
}

fn diff_document_with_conflicts(
    scope_root: &str,
    generation: u64,
    branch_name: &str,
    conflicted_files: Vec<ChangedFile>,
    staged_files: Vec<ChangedFile>,
    unstaged_files: Vec<ChangedFile>,
) -> DiffDocument {
    DiffDocument {
        snapshot: snapshot_with(scope_root, generation, branch_name),
        tracking: GitTrackingStatus {
            upstream_ref: None,
            ahead: 0,
            behind: 0,
        },
        merge_state: Some(orcashell_git::MergeState {
            can_complete: conflicted_files.is_empty() && unstaged_files.is_empty(),
            can_abort: true,
            conflicted_file_count: conflicted_files.len(),
        }),
        repo_state_warning: None,
        conflicted_files,
        staged_files,
        unstaged_files,
    }
}

fn oid(value: u64) -> Oid {
    Oid::from_str(&format!("{value:040x}")).unwrap()
}

fn repository_graph_document(scope_root: &str, head_branch: &str) -> RepositoryGraphDocument {
    let head_oid = oid(1);
    RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head: HeadState::Branch {
            name: head_branch.to_string(),
            oid: head_oid,
        },
        local_branches: vec![LocalBranchEntry {
            name: head_branch.to_string(),
            full_ref: format!("refs/heads/{head_branch}"),
            target: head_oid,
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: head_branch.to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: vec![RemoteBranchEntry {
            remote_name: "origin".to_string(),
            short_name: head_branch.to_string(),
            full_ref: format!("refs/remotes/origin/{head_branch}"),
            target: head_oid,
            tracked_by_local: Some(head_branch.to_string()),
        }],
        commits: vec![CommitGraphNode {
            oid: head_oid,
            short_oid: head_oid.to_string()[..8].to_string(),
            summary: "head".to_string(),
            author_name: "Orca".to_string(),
            authored_at_unix: 1_700_000_000,
            parent_oids: Vec::new(),
            primary_lane: 0,
            row_lanes: vec![GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::Start,
                target_lane: None,
            }],
            ref_labels: Vec::new(),
        }],
        truncated: false,
    }
}

fn repository_workspace(scope_root: &str) -> WorkspaceState {
    let mut workspace = workspace_with_store();
    let project_id = "repo-project".to_string();
    workspace.projects = vec![ProjectData {
        id: project_id.clone(),
        name: "Repo Project".to_string(),
        path: PathBuf::from(scope_root),
        layout: tabs(vec![term("term-1")], 0),
        terminal_names: HashMap::new(),
    }];
    workspace.active_project_id = Some(project_id.clone());
    let scope_root = PathBuf::from(scope_root);
    let tab_id = WorkspaceState::repository_graph_tab_id(&project_id);
    workspace.auxiliary_tabs.push(AuxiliaryTabState {
        id: tab_id.clone(),
        title: WorkspaceState::repository_graph_title("Repo Project"),
        kind: AuxiliaryTabKind::RepositoryGraph {
            project_id: project_id.clone(),
        },
    });
    workspace.active_auxiliary_tab_id = Some(tab_id);
    workspace.repository_graph_tabs.insert(
        project_id.clone(),
        super::RepositoryGraphTabState::new(project_id.clone(), scope_root),
    );
    workspace
}

fn loaded_conflict_document(
    generation: u64,
    path: &str,
    raw_text: &str,
    is_dirty: bool,
) -> ConflictEditorDocument {
    ConflictEditorDocument {
        generation,
        version: 0,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Conflicted),
        initial_raw_text: raw_text.to_string(),
        raw_text: raw_text.to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty,
        cursor_pos: 0,
        selection_range: None,
        active_block_index: None,
        scroll_x: 0.0,
        scroll_y: 0.0,
    }
}

fn file_document(scope_root: &str, generation: u64, path: &str) -> FileDiffDocument {
    FileDiffDocument {
        generation,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Modified),
        lines: vec![orcashell_git::DiffLineView {
            kind: orcashell_git::DiffLineKind::Context,
            old_lineno: Some(1),
            new_lineno: Some(1),
            text: format!("{scope_root}:{path}"),
            highlights: None,
            inline_changes: None,
        }],
        hunks: Vec::new(),
    }
}

fn file_document_with_lines(
    scope_root: &str,
    generation: u64,
    path: &str,
    line_count: usize,
) -> FileDiffDocument {
    FileDiffDocument {
        generation,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Modified),
        lines: (0..line_count)
            .map(|index| orcashell_git::DiffLineView {
                kind: orcashell_git::DiffLineKind::Context,
                old_lineno: Some((index + 1) as u32),
                new_lineno: Some((index + 1) as u32),
                text: format!("{scope_root}:{path}:{index}"),
                highlights: None,
                inline_changes: None,
            })
            .collect(),
        hunks: Vec::new(),
    }
}

fn file_document_with_kinds(
    generation: u64,
    path: &str,
    lines: Vec<(orcashell_git::DiffLineKind, &str)>,
) -> FileDiffDocument {
    FileDiffDocument {
        generation,
        selection: DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Modified),
        lines: lines
            .into_iter()
            .enumerate()
            .map(|(index, (kind, text))| orcashell_git::DiffLineView {
                kind,
                old_lineno: Some((index + 1) as u32),
                new_lineno: Some((index + 1) as u32),
                text: text.to_string(),
                highlights: None,
                inline_changes: None,
            })
            .collect(),
        hunks: Vec::new(),
    }
}

fn feed_file_summary(
    path: &str,
    section: DiffSectionKind,
    status: GitFileStatus,
) -> FeedEventFileSummary {
    FeedEventFileSummary {
        relative_path: PathBuf::from(path),
        staged: matches!(section, DiffSectionKind::Staged),
        unstaged: matches!(section, DiffSectionKind::Unstaged),
        status,
        is_binary: false,
        insertions: 3,
        deletions: 1,
    }
}

fn feed_event_file(
    scope_root: &str,
    generation: u64,
    section: DiffSectionKind,
    path: &str,
    line_count: usize,
) -> CapturedDiffFile {
    CapturedDiffFile {
        selection: DiffSelectionKey {
            section,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Modified),
        document: file_document_with_lines(scope_root, generation, path, line_count),
    }
}

fn feed_failure(path: &str, section: DiffSectionKind, message: &str) -> CapturedDiffFailure {
    CapturedDiffFailure {
        selection: DiffSelectionKey {
            section,
            relative_path: PathBuf::from(path),
        },
        relative_path: PathBuf::from(path),
        message: message.into(),
    }
}

fn captured_event(
    files: Vec<CapturedDiffFile>,
    failed_files: Vec<CapturedDiffFailure>,
    truncated: bool,
) -> CapturedEventDiff {
    let total_rendered_lines = files.iter().map(|file| file.document.lines.len()).sum();
    let total_rendered_bytes = files
        .iter()
        .flat_map(|file| file.document.lines.iter())
        .map(|line| line.text.len())
        .sum();
    CapturedEventDiff {
        files,
        failed_files,
        truncated,
        total_rendered_lines,
        total_rendered_bytes,
    }
}

fn feed_event(
    kind: FeedEventKind,
    files: Vec<FeedEventFileSummary>,
    capture: CapturedEventDiff,
) -> FeedEventData {
    FeedEventData {
        kind,
        changed_file_count: files.len(),
        insertions: files.iter().map(|file| file.insertions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
        files,
        capture,
    }
}

fn feed_layer(
    scope_root: &str,
    generation: u64,
    section: DiffSectionKind,
    path: &str,
    line_count: usize,
) -> FeedLayerSnapshot {
    FeedLayerSnapshot {
        selection: DiffSelectionKey {
            section,
            relative_path: PathBuf::from(path),
        },
        file: changed_file(path, GitFileStatus::Modified),
        state: FeedLayerState::Ready(file_document_with_lines(
            scope_root, generation, path, line_count,
        )),
    }
}

fn feed_capture_result(
    generation: u64,
    layers: Vec<FeedLayerSnapshot>,
    event: Option<FeedEventData>,
) -> FeedCaptureResult {
    FeedCaptureResult {
        current_capture: FeedScopeCapture { generation, layers },
        event,
    }
}

fn init_repo() -> PathBuf {
    let path = std::env::temp_dir().join(format!("orcashell-ui-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    run_git(&path, &["init"]);
    run_git(&path, &["config", "user.name", "Orca"]);
    run_git(&path, &["config", "user.email", "orca@example.com"]);
    fs::write(path.join("tracked.txt"), "hello\n").unwrap();
    run_git(&path, &["add", "tracked.txt"]);
    run_git(&path, &["commit", "-m", "init"]);
    path
}

fn run_git(cwd: &PathBuf, args: &[&str]) {
    let output = orcashell_platform::command("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── normalize_project_path ──────────────────────────────────────────────

#[test]
fn normalize_project_path_resolves_existing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let result = WorkspaceState::normalize_project_path(dir.path());
    // Canonical path must be absolute and the directory must exist.
    assert!(result.is_absolute());
    assert!(result.exists());
}

#[test]
fn normalize_project_path_fallback_for_nonexistent() {
    let path = PathBuf::from("/this/does/not/exist/orcashell-unit-test-xyz");
    let result = WorkspaceState::normalize_project_path(&path);
    // Falls back to the original path unchanged.
    assert_eq!(result, path);
}

#[cfg(unix)]
#[test]
fn normalize_project_path_resolves_symlink_to_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let canonical_real = WorkspaceState::normalize_project_path(&real);
    let canonical_link = WorkspaceState::normalize_project_path(&link);
    // Both the real directory and the symlink should resolve to the same canonical path.
    assert_eq!(canonical_real, canonical_link);
}

#[test]
fn select_terminal_clears_only_selected_terminal_notification() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.terminal_runtime
        .insert("t1".into(), runtime_state(Some(NotificationTier::Urgent)));
    ws.terminal_runtime.insert(
        "t2".into(),
        runtime_state(Some(NotificationTier::Informational)),
    );

    assert!(ws.select_terminal_internal("proj-1", &[1]));

    assert_eq!(
        ws.terminal_notification_tier("t1"),
        Some(NotificationTier::Urgent)
    );
    assert_eq!(ws.terminal_notification_tier("t2"), None);
    assert!(ws.focus.is_focused("proj-1", &[1]));
}

#[test]
fn select_terminal_updates_active_project_and_root_tab() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.auxiliary_tabs.push(AuxiliaryTabState {
        id: SETTINGS_TAB_ID.into(),
        title: "Settings".into(),
        kind: AuxiliaryTabKind::Settings,
    });
    ws.active_auxiliary_tab_id = Some(SETTINGS_TAB_ID.into());

    assert!(ws.select_terminal_internal("proj-1", &[1]));

    assert_eq!(ws.active_project_id.as_deref(), Some("proj-1"));
    assert!(ws.active_auxiliary_tab_id.is_none());
    assert!(ws.focus.is_focused("proj-1", &[1]));
    assert_eq!(
        ws.project("proj-1")
            .and_then(|project| project.layout.active_tab_index()),
        Some(1)
    );
}

#[test]
fn diff_tab_open_dedupes_by_scope_and_updates_title() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with("/tmp/repo", 2, "feature/a"),
    );

    assert!(ws.open_or_focus_diff_tab_internal(scope_root.clone(), "feature/a".into()));
    assert!(ws.open_or_focus_diff_tab_internal(scope_root.clone(), "feature/b".into()));

    assert_eq!(ws.auxiliary_tabs.len(), 1);
    assert_eq!(ws.diff_tabs.len(), 1);
    assert_eq!(ws.active_diff_scope_root(), Some(scope_root.as_path()));
    assert_eq!(ws.auxiliary_tabs[0].title, "Diff: feature/b");
    assert!(ws.diff_tabs[&scope_root].index.loading);
    assert_eq!(
        ws.diff_tabs[&scope_root].index.requested_generation,
        Some(2)
    );
}

#[test]
fn live_diff_feed_open_dedupes_by_project_and_tracks_current_scopes() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), PathBuf::from("/tmp/repo-a"));
    ws.terminal_git_scopes
        .insert("t2".into(), PathBuf::from("/tmp/repo-b"));

    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    assert_eq!(ws.auxiliary_tabs.len(), 1);
    assert_eq!(
        ws.active_auxiliary_tab_id.as_deref(),
        Some("aux-live-diff-proj-1")
    );
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.tracked_scope_count()),
        Some(2)
    );
    assert_eq!(ws.auxiliary_tabs[0].title, "Live Feed: proj-1");
}

#[test]
fn closing_live_diff_auxiliary_tab_discards_feed_state() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));

    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert!(ws.live_diff_feed_state("proj-1").is_some());

    assert!(ws.close_auxiliary_tab_internal("aux-live-diff-proj-1"));
    assert!(ws.live_diff_feed_state("proj-1").is_none());
    assert!(ws.auxiliary_tabs.is_empty());
}

#[test]
fn live_diff_feed_scope_membership_updates_on_scope_attach_and_detach() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));

    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.tracked_scope_count()),
        Some(0)
    );

    ws.attach_terminal_scope("t1", PathBuf::from("/tmp/repo-a"));
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.tracked_scope_count()),
        Some(1)
    );

    ws.detach_terminal_scope("t1");
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.tracked_scope_count()),
        Some(0)
    );
}

#[test]
fn live_diff_feed_bootstraps_dirty_scope_only() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 1, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root.clone(),
        1,
        1,
        Ok(feed_capture_result(1, Vec::new(), None)),
    );
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.entries.len()),
        Some(0)
    );

    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    ws.refresh_live_feeds_for_scope_if_stale(&scope_root, 2);
    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        2,
        2,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                2,
            )],
            Some(feed_event(
                FeedEventKind::LiveDelta,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        2,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(feed.entries[0].origin, FeedEntryOrigin::LiveDelta);
    assert!(matches!(
        feed.entries[0].capture_state,
        FeedCaptureState::Ready(CapturedEventDiff { ref files, .. }) if files.len() == 1
    ));
}

#[test]
fn live_diff_feed_ignores_stale_request_revisions() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    {
        let scope_state = ws
            .live_diff_feeds
            .get_mut("proj-1")
            .and_then(|feed| feed.tracked_scopes.get_mut(&scope_root))
            .unwrap();
        scope_state.latest_request_revision = 2;
    }

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        1,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );

    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .map(|feed| feed.entries.len()),
        Some(0)
    );
}

#[test]
fn live_diff_feed_dirty_to_clean_appends_ready_clean_event() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root.clone(),
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                2,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        2,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 3, "main"));
    ws.refresh_live_feeds_for_scope_if_stale(&scope_root, 3);
    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        3,
        2,
        Ok(feed_capture_result(
            3,
            Vec::new(),
            Some(feed_event(
                FeedEventKind::LiveDelta,
                Vec::new(),
                captured_event(Vec::new(), Vec::new(), false),
            )),
        )),
    );

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.entries.len(), 2);
    let clean_entry = &feed.entries[1];
    assert_eq!(clean_entry.origin, FeedEntryOrigin::LiveDelta);
    assert_eq!(clean_entry.changed_file_count, 0);
    assert!(matches!(
        clean_entry.capture_state,
        FeedCaptureState::Ready(CapturedEventDiff {
            ref files,
            ref failed_files,
            truncated: false,
            total_rendered_lines: 0,
            total_rendered_bytes: 0,
        }) if files.is_empty() && failed_files.is_empty()
    ));
}

#[test]
fn live_diff_feed_retention_prunes_oldest_entries() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 1, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    let event = feed_event(
        FeedEventKind::LiveDelta,
        vec![feed_file_summary(
            "a.rs",
            DiffSectionKind::Unstaged,
            GitFileStatus::Modified,
        )],
        captured_event(
            vec![feed_event_file(
                "/tmp/repo",
                1,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Vec::new(),
            false,
        ),
    );

    for generation in 1..=2_001 {
        ws.append_live_feed_entry("proj-1", &scope_root, generation, &event);
    }

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.entries.len(), 2_000);
    assert_eq!(feed.entries.front().map(|entry| entry.id), Some(2));
    assert_eq!(feed.entries.back().map(|entry| entry.id), Some(2_001));
}

#[test]
fn live_diff_feed_capture_result_maps_truncated_events() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                FEED_EVENT_LINE_CAP,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        FEED_EVENT_LINE_CAP,
                    )],
                    Vec::new(),
                    true,
                ),
            )),
        )),
    );

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert!(matches!(
        &feed.entries[0].capture_state,
        FeedCaptureState::Truncated(CapturedEventDiff { files, total_rendered_lines, .. })
            if files.len() == 1 && *total_rendered_lines == FEED_EVENT_LINE_CAP
    ));
}

#[test]
fn live_diff_feed_capture_result_maps_all_failures_to_failed_state() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![FeedLayerSnapshot {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("a.rs"),
                },
                file: changed_file("a.rs", GitFileStatus::Modified),
                state: FeedLayerState::Unavailable {
                    message: "capture failed".into(),
                },
            }],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    Vec::new(),
                    vec![feed_failure(
                        "a.rs",
                        DiffSectionKind::Unstaged,
                        "capture failed",
                    )],
                    false,
                ),
            )),
        )),
    );

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert!(matches!(
        feed.entries[0].capture_state,
        FeedCaptureState::Failed { ref message } if message.contains("1 file")
    ));
}

#[test]
fn live_diff_feed_scope_error_persists_until_failing_scope_recovers() {
    let mut ws = WorkspaceState::new();
    let scope_root_a = PathBuf::from("/tmp/repo-a");
    let scope_root_b = PathBuf::from("/tmp/repo-b");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root_a.clone());
    ws.terminal_git_scopes
        .insert("t2".into(), scope_root_b.clone());
    ws.git_scopes.insert(
        scope_root_a.clone(),
        snapshot_with("/tmp/repo-a", 2, "main"),
    );
    ws.git_scopes.insert(
        scope_root_b.clone(),
        snapshot_with("/tmp/repo-b", 2, "main"),
    );
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.apply_live_feed_capture_update("proj-1", scope_root_a.clone(), 2, 1, Err("boom".into()));
    assert!(ws
        .live_diff_feed_state("proj-1")
        .and_then(|feed| feed.latest_scope_error())
        .is_some_and(|error| error.contains("/tmp/repo-a") && error.contains("boom")));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root_b,
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo-b",
                2,
                DiffSectionKind::Unstaged,
                "b.rs",
                1,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "b.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo-b",
                        2,
                        DiffSectionKind::Unstaged,
                        "b.rs",
                        1,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );
    assert!(ws
        .live_diff_feed_state("proj-1")
        .and_then(|feed| feed.latest_scope_error())
        .is_some_and(|error| error.contains("/tmp/repo-a") && error.contains("boom")));

    ws.git_scopes.insert(
        scope_root_a.clone(),
        snapshot_with("/tmp/repo-a", 3, "main"),
    );
    ws.refresh_live_feeds_for_scope_if_stale(&scope_root_a, 3);
    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root_a,
        3,
        2,
        Ok(feed_capture_result(
            3,
            vec![feed_layer(
                "/tmp/repo-a",
                3,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo-a",
                        3,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        1,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );
    assert_eq!(
        ws.live_diff_feed_state("proj-1")
            .and_then(|feed| feed.latest_scope_error()),
        None
    );
}

#[test]
fn live_diff_feed_requeues_newer_generation_after_older_response() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 3, "main"));
    ws.refresh_live_feeds_for_scope_if_stale(&scope_root, 3);

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root.clone(),
        2,
        1,
        Ok(feed_capture_result(
            2,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        1,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );
    assert!(ws
        .live_diff_feeds
        .get("proj-1")
        .and_then(|feed| feed.tracked_scopes.get(&scope_root))
        .is_some_and(|scope_state| {
            scope_state.pending_refresh && scope_state.latest_request_revision == 2
        }));

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        3,
        2,
        Ok(feed_capture_result(
            3,
            Vec::new(),
            Some(feed_event(
                FeedEventKind::LiveDelta,
                Vec::new(),
                captured_event(Vec::new(), Vec::new(), false),
            )),
        )),
    );
    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.entries.len(), 2);
    assert_eq!(feed.entries[0].generation, 2);
    assert_eq!(feed.entries[1].generation, 3);
}

#[test]
fn live_diff_feed_paused_follow_accumulates_unread_entries() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 1, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert!(ws.set_live_diff_feed_follow_state("proj-1", false));

    let event = feed_event(
        FeedEventKind::LiveDelta,
        vec![feed_file_summary(
            "a.rs",
            DiffSectionKind::Unstaged,
            GitFileStatus::Modified,
        )],
        captured_event(
            vec![feed_event_file(
                "/tmp/repo",
                1,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Vec::new(),
            false,
        ),
    );
    ws.append_live_feed_entry("proj-1", &scope_root, 1, &event);
    ws.append_live_feed_entry("proj-1", &scope_root, 2, &event);

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert!(!feed.live_follow);
    assert_eq!(feed.unread_count, 2);
}

#[test]
fn live_diff_feed_resume_clears_unread_entries() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 1, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert!(ws.set_live_diff_feed_follow_state("proj-1", false));

    ws.append_live_feed_entry(
        "proj-1",
        &scope_root,
        1,
        &feed_event(
            FeedEventKind::LiveDelta,
            vec![feed_file_summary(
                "a.rs",
                DiffSectionKind::Unstaged,
                GitFileStatus::Modified,
            )],
            captured_event(
                vec![feed_event_file(
                    "/tmp/repo",
                    1,
                    DiffSectionKind::Unstaged,
                    "a.rs",
                    1,
                )],
                Vec::new(),
                false,
            ),
        ),
    );
    assert!(ws.resume_live_diff_feed("proj-1"));

    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert!(feed.live_follow);
    assert_eq!(feed.unread_count, 0);
}

#[test]
fn live_diff_feed_select_entry_opens_and_closes_detail_pane() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 1, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    ws.append_live_feed_entry(
        "proj-1",
        &scope_root,
        1,
        &feed_event(
            FeedEventKind::LiveDelta,
            vec![feed_file_summary(
                "a.rs",
                DiffSectionKind::Unstaged,
                GitFileStatus::Modified,
            )],
            captured_event(
                vec![feed_event_file(
                    "/tmp/repo",
                    1,
                    DiffSectionKind::Unstaged,
                    "a.rs",
                    1,
                )],
                Vec::new(),
                false,
            ),
        ),
    );

    assert!(ws.select_feed_entry("proj-1", 1));
    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.selected_entry_id, Some(1));
    assert!(feed.detail_pane_open);

    assert!(ws.close_feed_detail_pane("proj-1"));
    let feed = ws.live_diff_feed_state("proj-1").unwrap();
    assert_eq!(feed.selected_entry_id, None);
    assert!(!feed.detail_pane_open);
}

#[test]
fn live_diff_feed_open_diff_routes_scope_and_preferred_file() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with("/tmp/repo", 4, "feature/live"),
    );

    let preferred_file = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/live.rs"),
    };
    assert!(ws.open_diff_tab_for_scope_and_file(&scope_root, Some(preferred_file.clone())));

    let diff_tab = ws.diff_tab_state(&scope_root).unwrap();
    assert_eq!(diff_tab.selected_file.as_ref(), Some(&preferred_file));
    assert!(diff_tab.index.loading);
    assert_eq!(diff_tab.index.requested_generation, Some(4));
}

#[test]
fn live_diff_feed_open_diff_missing_scope_reports_error() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/missing");

    assert!(!ws.open_diff_tab_for_scope_and_file(&scope_root, None));
    assert!(ws.diff_tabs.get(&scope_root).is_none());
    assert_eq!(
        ws.workspace_banner().map(|banner| banner.message.as_str()),
        Some("The diff scope /tmp/missing is no longer available.")
    );
}

#[test]
fn merge_conflict_event_opens_affected_scope_and_warns_request_scope() {
    let mut ws = WorkspaceState::new();
    let request_scope = PathBuf::from("/tmp/managed");
    let affected_scope = PathBuf::from("/tmp/source");

    ws.git_scopes.insert(
        affected_scope.clone(),
        snapshot_with("/tmp/source", 8, "main"),
    );
    ws.diff_tabs.insert(
        request_scope.clone(),
        DiffTabState::new(request_scope.clone()),
    );

    ws.handle_merge_conflict_entered(
        request_scope.clone(),
        affected_scope.clone(),
        vec![
            PathBuf::from("src/conflict.rs"),
            PathBuf::from("src/other.rs"),
        ],
        MergeConflictTrigger::MergeBack,
    );

    let affected_tab = ws.diff_tab_state(&affected_scope).unwrap();
    assert_eq!(
        affected_tab.selected_file,
        Some(DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("src/conflict.rs"),
        })
    );
    assert!(affected_tab.index.loading);
    assert_eq!(affected_tab.index.requested_generation, Some(8));
    assert_eq!(
        ws.diff_tabs[&request_scope]
            .last_action_banner
            .as_ref()
            .unwrap()
            .kind,
        ActionBannerKind::Warning
    );
}

#[test]
fn merge_conflict_event_uses_optimistic_open_for_missing_scope_snapshot() {
    let mut ws = WorkspaceState::new();
    let request_scope = PathBuf::from("/tmp/managed");
    let affected_scope = PathBuf::from("/tmp/missing-source");

    ws.diff_tabs.insert(
        request_scope.clone(),
        DiffTabState::new(request_scope.clone()),
    );

    ws.handle_merge_conflict_entered(
        request_scope,
        affected_scope.clone(),
        vec![PathBuf::from("src/conflict.rs")],
        MergeConflictTrigger::MergeBack,
    );

    let affected_tab = ws.diff_tab_state(&affected_scope).unwrap();
    assert_eq!(
        affected_tab.selected_file,
        Some(DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("src/conflict.rs"),
        })
    );
    assert!(affected_tab.index.loading);
    assert_eq!(ws.auxiliary_tabs[0].title, "Diff: missing-source");
}

#[test]
fn diff_index_update_prioritizes_conflicted_selection_and_skips_file_diff_request() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());

    ws.apply_diff_index_update(
        scope_root.clone(),
        6,
        Ok(diff_document_with_conflicts(
            "/tmp/repo",
            6,
            "main",
            vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
            vec![changed_file("staged.rs", GitFileStatus::Modified)],
            vec![changed_file("unstaged.rs", GitFileStatus::Modified)],
        )),
    );

    let diff_tab = ws.diff_tab_state(&scope_root).unwrap();
    assert_eq!(
        diff_tab.selected_file,
        Some(DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        })
    );
    assert!(!diff_tab.file.loading);
    assert!(diff_tab.file.document.is_none());
    assert_eq!(
        diff_tab.file.requested_selection,
        Some(DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        })
    );
}

#[test]
fn request_selected_file_diff_loads_conflict_document_from_worktree() {
    let repo = init_repo();
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    let conflict_text = "\
<<<<<<< ours
left
=======
right
>>>>>>> theirs
";
    fs::write(scope_root.join("conflict.rs"), conflict_text).unwrap();

    let mut ws = WorkspaceState::new();
    ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());
    ws.diff_tabs.get_mut(&scope_root).unwrap().index.document = Some(diff_document_with_conflicts(
        &scope_root_str,
        2,
        "main",
        vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
        Vec::new(),
        Vec::new(),
    ));

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("conflict.rs"),
    };
    ws.request_selected_file_diff(&scope_root, selection.clone());

    let diff_tab = ws.diff_tab_state(&scope_root).unwrap();
    assert!(!diff_tab.file.loading);
    assert!(diff_tab.file.document.is_none());
    assert_eq!(diff_tab.file.requested_generation, Some(2));
    assert_eq!(diff_tab.file.requested_selection.as_ref(), Some(&selection));
    let Some(ConflictDocumentState::Loaded(document)) = diff_tab
        .conflict_editor
        .documents
        .get(&selection.relative_path)
    else {
        panic!("expected loaded conflict document");
    };
    assert_eq!(document.raw_text, conflict_text);
    assert_eq!(document.blocks.len(), 1);
    assert_eq!(document.active_block_index, Some(0));
    assert!(!document.has_base_sections);
    assert!(document.parse_error.is_none());
    assert!(!document.is_dirty);
}

#[test]
fn diff_index_update_prunes_stale_conflict_documents() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.conflict_editor = ConflictEditorState {
        documents: HashMap::from([
            (
                PathBuf::from("keep.rs"),
                ConflictDocumentState::Loaded(loaded_conflict_document(
                    1,
                    "keep.rs",
                    "resolved\n",
                    false,
                )),
            ),
            (
                PathBuf::from("drop.rs"),
                ConflictDocumentState::Loaded(loaded_conflict_document(
                    1,
                    "drop.rs",
                    "resolved\n",
                    false,
                )),
            ),
        ]),
    };
    ws.diff_tabs.insert(scope_root.clone(), tab);

    ws.apply_diff_index_update(
        scope_root.clone(),
        3,
        Ok(diff_document_with_conflicts(
            "/tmp/repo",
            3,
            "main",
            vec![changed_file("keep.rs", GitFileStatus::Conflicted)],
            Vec::new(),
            Vec::new(),
        )),
    );

    let documents = &ws.diff_tabs[&scope_root].conflict_editor.documents;
    assert!(documents.contains_key(&PathBuf::from("keep.rs")));
    assert!(!documents.contains_key(&PathBuf::from("drop.rs")));
}

#[test]
fn validate_conflict_path_for_resolution_rejects_dirty_cached_document() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.index.document = Some(diff_document_with_conflicts(
        "/tmp/repo",
        1,
        "main",
        vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
        Vec::new(),
        Vec::new(),
    ));
    tab.conflict_editor = ConflictEditorState {
        documents: HashMap::from([(
            PathBuf::from("conflict.rs"),
            ConflictDocumentState::Loaded(loaded_conflict_document(
                1,
                "conflict.rs",
                "resolved\n",
                true,
            )),
        )]),
    };
    ws.diff_tabs.insert(scope_root.clone(), tab);

    let error = ws
        .validate_conflict_path_for_resolution(
            &scope_root,
            &DiffSelectionKey {
                section: DiffSectionKind::Conflicted,
                relative_path: PathBuf::from("conflict.rs"),
            },
        )
        .unwrap_err();

    assert_eq!(error, "Save conflict.rs before marking it resolved.");
}

#[test]
fn conflict_resolution_targets_fall_back_to_selected_conflict_file() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.selected_file = Some(DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("conflict.rs"),
    });
    ws.diff_tabs.insert(scope_root.clone(), tab);

    assert_eq!(
        ws.conflict_resolution_targets(&scope_root),
        vec![DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        }]
    );
}

#[test]
fn can_mark_conflicts_resolved_uses_selected_conflict_fallback() {
    let repo = init_repo();
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    fs::write(scope_root.join("conflict.rs"), "fn resolved() {}\n").unwrap();

    let mut ws = WorkspaceState::new();
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.selected_file = Some(DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("conflict.rs"),
    });
    tab.index.document = Some(diff_document_with_conflicts(
        &scope_root_str,
        1,
        "main",
        vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
        Vec::new(),
        Vec::new(),
    ));
    ws.diff_tabs.insert(scope_root.clone(), tab);

    assert!(ws.can_mark_conflicts_resolved(&scope_root));
}

#[test]
fn can_mark_conflicts_resolved_rejects_dirty_conflicted_selection() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("conflict.rs"),
    };
    tab.selected_file = Some(selection.clone());
    tab.index.document = Some(diff_document_with_conflicts(
        "/tmp/repo",
        1,
        "main",
        vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
        Vec::new(),
        Vec::new(),
    ));
    tab.conflict_editor = ConflictEditorState {
        documents: HashMap::from([(
            PathBuf::from("conflict.rs"),
            ConflictDocumentState::Loaded(loaded_conflict_document(
                1,
                "conflict.rs",
                "resolved\n",
                true,
            )),
        )]),
    };
    ws.diff_tabs.insert(scope_root.clone(), tab);

    assert!(!ws.can_mark_conflicts_resolved(&scope_root));
}

#[test]
fn validate_conflict_path_for_resolution_accepts_saved_conflict_free_file() {
    let repo = init_repo();
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    fs::write(scope_root.join("conflict.rs"), "fn resolved() {}\n").unwrap();

    let mut ws = WorkspaceState::new();
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.index.document = Some(diff_document_with_conflicts(
        &scope_root_str,
        1,
        "main",
        vec![changed_file("conflict.rs", GitFileStatus::Conflicted)],
        Vec::new(),
        Vec::new(),
    ));
    ws.diff_tabs.insert(scope_root.clone(), tab);

    assert!(ws
        .validate_conflict_path_for_resolution(
            &scope_root,
            &DiffSelectionKey {
                section: DiffSectionKind::Conflicted,
                relative_path: PathBuf::from("conflict.rs"),
            },
        )
        .is_ok());
}

#[test]
fn merge_conflict_document_reset_reload_restores_initial_conflict_view() {
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Conflicted,
        relative_path: PathBuf::from("conflict.rs"),
    };
    let original_text = "\
<<<<<<< ours
left
=======
right
>>>>>>> theirs
";

    let prior_document = ConflictEditorDocument {
        generation: 2,
        version: 1,
        selection: selection.clone(),
        file: changed_file("conflict.rs", GitFileStatus::Conflicted),
        initial_raw_text: original_text.to_string(),
        raw_text: "fn resolved() {}\n".to_string(),
        blocks: Vec::new(),
        has_base_sections: false,
        parse_error: None,
        is_dirty: false,
        cursor_pos: 5,
        selection_range: Some(0..2),
        active_block_index: None,
        scroll_x: 24.0,
        scroll_y: 96.0,
    };

    let document = merge_conflict_document_from_text(
        3,
        selection,
        changed_file("conflict.rs", GitFileStatus::Conflicted),
        original_text.to_string(),
        None,
        false,
    );

    assert_eq!(document.version, 0);
    assert_eq!(document.raw_text, original_text);
    assert_eq!(document.initial_raw_text, original_text);
    assert_eq!(document.blocks.len(), 1);
    assert_eq!(document.active_block_index, Some(0));
    assert_eq!(document.cursor_pos, document.blocks[0].whole_block.start);
    assert!(document.selection_range.is_none());
    assert_eq!(document.scroll_x, 0.0);
    assert_eq!(document.scroll_y, 0.0);
    assert!(!document.is_dirty);

    let preserved = merge_conflict_document_from_text(
        3,
        DiffSelectionKey {
            section: DiffSectionKind::Conflicted,
            relative_path: PathBuf::from("conflict.rs"),
        },
        changed_file("conflict.rs", GitFileStatus::Conflicted),
        "fn resolved() {}\n".to_string(),
        Some(&prior_document),
        false,
    );
    assert_eq!(preserved.initial_raw_text, original_text);
}

#[test]
fn focus_terminal_by_id_selects_exact_terminal() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.terminal_runtime
        .insert("t1".into(), runtime_state(Some(NotificationTier::Urgent)));
    ws.terminal_runtime.insert(
        "t2".into(),
        runtime_state(Some(NotificationTier::Informational)),
    );

    assert!(ws.focus_terminal_by_id("t2"));
    assert_eq!(ws.active_project_id.as_deref(), Some("proj-1"));
    assert!(ws.focus.is_focused("proj-1", &[1]));
    assert_eq!(ws.pending_focus_terminal_id.as_deref(), Some("t2"));
    assert_eq!(
        ws.terminal_notification_tier("t1"),
        Some(NotificationTier::Urgent)
    );
    assert_eq!(ws.terminal_notification_tier("t2"), None);
}

#[test]
fn focus_terminal_by_id_missing_terminal_sets_workspace_error() {
    let mut ws = WorkspaceState::new();

    assert!(!ws.focus_terminal_by_id("missing"));
    assert_eq!(
        ws.workspace_banner()
            .map(|banner| (&banner.kind, banner.message.as_str())),
        Some((
            &WorkspaceBannerKind::Error,
            "The source terminal missing is no longer available."
        ))
    );
}

#[test]
fn live_diff_feed_resolves_managed_worktree_provenance() {
    let mut ws = workspace_with_store();
    let scope_root = PathBuf::from("/tmp/repo-worktree");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with("/tmp/repo-worktree", 1, "feature/live"),
    );
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));

    {
        let mut store = ws.services.store.lock();
        store
            .as_mut()
            .unwrap()
            .save_worktree(&StoredWorktree {
                id: "wt-1".into(),
                project_id: "proj-1".into(),
                repo_root: PathBuf::from("/tmp/repo-root"),
                path: scope_root.clone(),
                worktree_name: "agent-a".into(),
                branch_name: "feature/live".into(),
                source_ref: "main".into(),
                primary_terminal_id: Some("t1".into()),
            })
            .unwrap();
    }

    ws.append_live_feed_entry(
        "proj-1",
        &scope_root,
        1,
        &feed_event(
            FeedEventKind::LiveDelta,
            vec![feed_file_summary(
                "a.rs",
                DiffSectionKind::Unstaged,
                GitFileStatus::Modified,
            )],
            captured_event(
                vec![feed_event_file(
                    "/tmp/repo-worktree",
                    1,
                    DiffSectionKind::Unstaged,
                    "a.rs",
                    1,
                )],
                Vec::new(),
                false,
            ),
        ),
    );

    let entry = &ws.live_diff_feed_state("proj-1").unwrap().entries[0];
    assert_eq!(entry.scope_kind, FeedScopeKind::ManagedWorktree);
    assert_eq!(entry.worktree_name.as_deref(), Some("agent-a"));
    assert_eq!(entry.source_terminal_id.as_deref(), Some("t1"));
}

#[test]
fn closing_live_diff_feed_discards_pending_refresh_and_ignores_late_results() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1")], 0)));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 2, "main"));
    assert!(ws.open_or_focus_live_diff_stream_tab_internal("proj-1"));
    assert!(ws
        .live_diff_feeds
        .get("proj-1")
        .and_then(|feed| feed.tracked_scopes.get(&scope_root))
        .is_some_and(|scope_state| scope_state.pending_refresh));

    assert!(ws.close_auxiliary_tab_internal("aux-live-diff-proj-1"));
    assert!(ws.live_diff_feed_state("proj-1").is_none());

    ws.apply_live_feed_capture_update(
        "proj-1",
        scope_root,
        2,
        1,
        Ok(feed_capture_result(
            1,
            vec![feed_layer(
                "/tmp/repo",
                2,
                DiffSectionKind::Unstaged,
                "a.rs",
                1,
            )],
            Some(feed_event(
                FeedEventKind::BootstrapSnapshot,
                vec![feed_file_summary(
                    "a.rs",
                    DiffSectionKind::Unstaged,
                    GitFileStatus::Modified,
                )],
                captured_event(
                    vec![feed_event_file(
                        "/tmp/repo",
                        2,
                        DiffSectionKind::Unstaged,
                        "a.rs",
                        1,
                    )],
                    Vec::new(),
                    false,
                ),
            )),
        )),
    );
    assert!(ws.live_diff_feed_state("proj-1").is_none());
}

#[test]
fn feed_preview_layout_single_file_can_use_full_budget() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: vec![CapturedDiffFile {
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("a.rs"),
            },
            file: changed_file("a.rs", GitFileStatus::Modified),
            document: file_document_with_lines("/tmp/repo", 2, "a.rs", 40),
        }],
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 40,
        total_rendered_bytes: 400,
    });

    assert_eq!(preview.files.len(), 1);
    assert_eq!(preview.files[0].lines.len(), FEED_PREVIEW_LINE_BUDGET);
}

#[test]
fn feed_preview_layout_skips_file_and_hunk_headers() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: vec![CapturedDiffFile {
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("a.rs"),
            },
            file: changed_file("a.rs", GitFileStatus::Modified),
            document: file_document_with_kinds(
                2,
                "a.rs",
                vec![
                    (
                        orcashell_git::DiffLineKind::FileHeader,
                        "diff --git a/a.rs b/a.rs",
                    ),
                    (orcashell_git::DiffLineKind::HunkHeader, "@@ -1,2 +1,2 @@"),
                    (orcashell_git::DiffLineKind::Deletion, "old line"),
                    (orcashell_git::DiffLineKind::Addition, "new line"),
                ],
            ),
        }],
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 4,
        total_rendered_bytes: 64,
    });

    assert_eq!(preview.files.len(), 1);
    assert_eq!(
        preview.files[0]
            .lines
            .iter()
            .map(|line| line.kind)
            .collect::<Vec<_>>(),
        vec![
            orcashell_git::DiffLineKind::Deletion,
            orcashell_git::DiffLineKind::Addition,
        ]
    );
}

#[test]
fn feed_preview_layout_evenly_distributes_budget_across_visible_files() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: ["a.rs", "b.rs", "c.rs", "d.rs"]
            .into_iter()
            .map(|path| CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from(path),
                },
                file: changed_file(path, GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, path, 20),
            })
            .collect(),
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 80,
        total_rendered_bytes: 800,
    });

    assert_eq!(
        preview
            .files
            .iter()
            .map(|file| file.lines.len())
            .collect::<Vec<_>>(),
        vec![6, 6, 6]
    );
}

#[test]
fn feed_preview_layout_evenly_distributes_budget_across_two_files() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: ["a.rs", "b.rs"]
            .into_iter()
            .map(|path| CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from(path),
                },
                file: changed_file(path, GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, path, 20),
            })
            .collect(),
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 40,
        total_rendered_bytes: 400,
    });

    assert_eq!(
        preview
            .files
            .iter()
            .map(|file| file.lines.len())
            .collect::<Vec<_>>(),
        vec![9, 9]
    );
}

#[test]
fn feed_preview_layout_evenly_distributes_budget_across_three_files() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: ["a.rs", "b.rs", "c.rs"]
            .into_iter()
            .map(|path| CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from(path),
                },
                file: changed_file(path, GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, path, 20),
            })
            .collect(),
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 60,
        total_rendered_bytes: 600,
    });

    assert_eq!(
        preview
            .files
            .iter()
            .map(|file| file.lines.len())
            .collect::<Vec<_>>(),
        vec![6, 6, 6]
    );
}

#[test]
fn feed_preview_layout_redistributes_budget_from_short_files() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: vec![
            CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("short.rs"),
                },
                file: changed_file("short.rs", GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, "short.rs", 1),
            },
            CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("long-a.rs"),
                },
                file: changed_file("long-a.rs", GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, "long-a.rs", 20),
            },
            CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("long-b.rs"),
                },
                file: changed_file("long-b.rs", GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, "long-b.rs", 20),
            },
        ],
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 41,
        total_rendered_bytes: 410,
    });

    assert_eq!(
        preview
            .files
            .iter()
            .map(|file| file.lines.len())
            .collect::<Vec<_>>(),
        vec![1, 9, 8]
    );
}

#[test]
fn feed_preview_layout_limits_visible_files_and_tracks_hidden_names() {
    let preview = WorkspaceState::build_feed_preview_layout(&CapturedEventDiff {
        files: ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]
            .into_iter()
            .map(|path| CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from(path),
                },
                file: changed_file(path, GitFileStatus::Modified),
                document: file_document_with_lines("/tmp/repo", 2, path, 10),
            })
            .collect(),
        failed_files: Vec::new(),
        truncated: false,
        total_rendered_lines: 60,
        total_rendered_bytes: 600,
    });

    assert_eq!(preview.files.len(), FEED_PREVIEW_FILE_CAP);
    assert_eq!(preview.hidden_file_count, 3);
    assert_eq!(
        preview.hidden_file_names,
        vec![
            PathBuf::from("d.rs"),
            PathBuf::from("e.rs"),
            PathBuf::from("f.rs")
        ]
    );
}

#[test]
fn closing_diff_auxiliary_tab_clears_cached_state() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let tab_id = WorkspaceState::diff_tab_id(&scope_root);
    ws.auxiliary_tabs.push(AuxiliaryTabState {
        id: tab_id.clone(),
        title: "Diff: main".into(),
        kind: AuxiliaryTabKind::Diff {
            scope_root: scope_root.clone(),
        },
    });
    ws.active_auxiliary_tab_id = Some(tab_id.clone());
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));

    assert!(ws.close_auxiliary_tab_internal(&tab_id));
    assert!(ws.active_auxiliary_tab_id.is_none());
    assert!(!ws.diff_tabs.contains_key(&scope_root));
    assert!(ws.auxiliary_tabs.is_empty());
}

#[test]
fn diff_index_load_selects_first_file_and_refreshes_title() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());

    ws.apply_diff_index_update(
        scope_root.clone(),
        3,
        Ok(diff_document(
            "/tmp/repo",
            3,
            "feature/diff",
            vec![
                changed_file("b.txt", GitFileStatus::Modified),
                changed_file("z.txt", GitFileStatus::Added),
            ],
        )),
    );

    assert_eq!(
        ws.diff_tabs[&scope_root]
            .selected_file
            .as_ref()
            .map(|k| k.relative_path.as_path()),
        Some(PathBuf::from("b.txt").as_path())
    );
    assert_eq!(ws.auxiliary_tabs[0].title, "Diff: feature/diff");
    assert!(ws.diff_tabs[&scope_root].file.loading);
    assert_eq!(
        ws.diff_tabs[&scope_root]
            .file
            .requested_selection
            .as_ref()
            .map(|k| k.relative_path.as_path()),
        Some(PathBuf::from("b.txt").as_path())
    );
    assert_eq!(ws.diff_tabs[&scope_root].file.requested_generation, Some(3));
}

#[test]
fn diff_index_update_ignores_stale_generation() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let tab_id = WorkspaceState::diff_tab_id(&scope_root);
    ws.auxiliary_tabs.push(AuxiliaryTabState {
        id: tab_id.clone(),
        title: "Diff: feature/new".into(),
        kind: AuxiliaryTabKind::Diff {
            scope_root: scope_root.clone(),
        },
    });
    ws.active_auxiliary_tab_id = Some(tab_id);
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    3,
                    "feature/new",
                    vec![changed_file("new.rs", GitFileStatus::Modified)],
                )),
                error: None,
                loading: true,
                requested_generation: Some(3),
            },
            selected_file: Some(DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("new.rs"),
            }),
            file: Default::default(),
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.apply_diff_index_update(
        scope_root.clone(),
        2,
        Ok(diff_document(
            "/tmp/repo",
            2,
            "feature/old",
            vec![changed_file("old.rs", GitFileStatus::Modified)],
        )),
    );

    let diff_tab = &ws.diff_tabs[&scope_root];
    assert_eq!(
        diff_tab
            .index
            .document
            .as_ref()
            .map(|document| document.snapshot.generation),
        Some(3)
    );
    assert_eq!(
        diff_tab
            .selected_file
            .as_ref()
            .map(|k| k.relative_path.as_path()),
        Some(PathBuf::from("new.rs").as_path())
    );
    assert!(diff_tab.index.loading);
    assert_eq!(ws.auxiliary_tabs[0].title, "Diff: feature/new");
}

#[test]
fn opening_diff_tab_requests_scope_snapshot_refresh() {
    let repo = init_repo();
    let services = WorkspaceServices::default();
    let events = services.git.subscribe_events();
    let mut ws = WorkspaceState::new_with_services(services);
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with(&scope_root_str, 1, "main"),
    );

    assert!(ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into()));

    let mut saw_snapshot = false;
    for _ in 0..3 {
        match events.recv_blocking().unwrap() {
            GitEvent::SnapshotUpdated {
                terminal_ids,
                scope_root,
                result,
                ..
            } => {
                assert!(terminal_ids.is_empty());
                let snapshot = result.unwrap();
                assert_eq!(scope_root.as_deref(), Some(snapshot.scope_root.as_path()));
                saw_snapshot = true;
                break;
            }
            GitEvent::DiffIndexLoaded { .. } | GitEvent::StashListLoaded { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }

    assert!(saw_snapshot);
}

#[test]
fn opening_diff_tab_starts_background_stash_list_refresh() {
    let repo = init_repo();
    let services = WorkspaceServices::default();
    let mut ws = WorkspaceState::new_with_services(services);
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with(&scope_root_str, 1, "main"),
    );

    assert!(ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into()));

    let diff_tab = ws.diff_tabs.get(&scope_root).unwrap();
    assert!(diff_tab.stash.list.loading);
    assert_eq!(diff_tab.stash.list.requested_revision, 1);
}

#[test]
fn focusing_diff_tab_refreshes_stash_list_again() {
    let repo = init_repo();
    let services = WorkspaceServices::default();
    let mut ws = WorkspaceState::new_with_services(services);
    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with(&scope_root_str, 1, "main"),
    );

    assert!(ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into()));
    ws.apply_stash_list_update(
        scope_root.clone(),
        1,
        Ok(StashListDocument {
            scope_root: scope_root.clone(),
            repo_root: scope_root.clone(),
            entries: Vec::new(),
        }),
    );

    let tab_id = WorkspaceState::diff_tab_id(&scope_root);
    assert!(ws.focus_auxiliary_tab_internal(&tab_id));

    let diff_tab = ws.diff_tabs.get(&scope_root).unwrap();
    assert!(diff_tab.stash.list.loading);
    assert_eq!(diff_tab.stash.list.requested_revision, 2);
}

#[test]
fn successful_stash_apply_and_pop_return_diff_tab_to_working_tree_mode() {
    for action in [GitActionKind::ApplyStash, GitActionKind::PopStash] {
        let mut ws = WorkspaceState::new();
        let scope_root = PathBuf::from("/tmp/repo");
        ws.diff_tabs.insert(
            scope_root.clone(),
            DiffTabState {
                scope_root: scope_root.clone(),
                tree_width: 300.0,
                view_mode: super::DiffTabViewMode::Stashes,
                local_action_in_flight: true,
                ..DiffTabState::new(scope_root.clone())
            },
        );

        ws.apply_local_action_completion(scope_root.clone(), action, Ok("stash applied".into()));

        let diff_tab = ws.diff_tabs.get(&scope_root).unwrap();
        assert_eq!(diff_tab.view_mode, super::DiffTabViewMode::WorkingTree);
        assert!(!diff_tab.local_action_in_flight);
    }
}

#[test]
fn stash_drop_success_stays_in_stash_mode() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            view_mode: super::DiffTabViewMode::Stashes,
            local_action_in_flight: true,
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::DropStash,
        Ok("stash dropped".into()),
    );

    let diff_tab = ws.diff_tabs.get(&scope_root).unwrap();
    assert_eq!(diff_tab.view_mode, super::DiffTabViewMode::Stashes);
    assert!(!diff_tab.local_action_in_flight);
}

#[test]
fn file_diff_update_ignores_non_selected_paths() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: Default::default(),
            selected_file: Some(DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("selected.rs"),
            }),
            file: Default::default(),
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.apply_file_diff_update(
        scope_root.clone(),
        4,
        DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("other.rs"),
        },
        Ok(file_document("/tmp/repo", 4, "other.rs")),
    );

    assert!(ws.diff_tabs[&scope_root].file.document.is_none());
}

#[test]
fn file_diff_update_ignores_stale_generation_for_selected_path() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: Default::default(),
            selected_file: Some(DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("selected.rs"),
            }),
            file: super::DiffFileState {
                document: Some(file_document("/tmp/repo", 5, "selected.rs")),
                error: None,
                loading: true,
                requested_generation: Some(5),
                requested_selection: Some(DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("selected.rs"),
                }),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.apply_file_diff_update(
        scope_root.clone(),
        4,
        DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("selected.rs"),
        },
        Ok(file_document("/tmp/repo", 4, "selected.rs")),
    );

    let file_state = &ws.diff_tabs[&scope_root].file;
    assert_eq!(
        file_state
            .document
            .as_ref()
            .map(|document| document.generation),
        Some(5)
    );
    assert!(file_state.loading);
    assert!(file_state.error.is_none());
}

#[test]
fn refresh_diff_theme_requeues_selected_file_when_old_request_is_in_flight() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 7, "main"));
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    7,
                    "main",
                    vec![changed_file("src/lib.rs", GitFileStatus::Modified)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(7),
            },
            selected_file: Some(selection.clone()),
            file: super::DiffFileState {
                document: Some(file_document("/tmp/repo", 7, "src/lib.rs")),
                error: Some("stale".into()),
                loading: true,
                requested_generation: Some(7),
                requested_selection: Some(selection.clone()),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.refresh_diff_theme_for(ThemeId::Light);

    let file = &ws.diff_tabs[&scope_root].file;
    assert!(file.document.is_none());
    assert!(file.error.is_none());
    assert!(file.loading);
    assert_eq!(file.requested_generation, Some(7));
    assert_eq!(file.requested_selection.as_ref(), Some(&selection));
}

#[test]
fn refresh_diff_theme_rehighlights_saved_live_feed_captures() {
    let mut ws = WorkspaceState::new();
    let project_id = "proj-1".to_string();
    let scope_root = PathBuf::from("/tmp/repo");

    let mut feed = ChangeFeedState::new(project_id.clone());
    feed.entries.push_back(ChangeFeedEntry {
        id: 1,
        observed_at: SystemTime::UNIX_EPOCH,
        origin: FeedEntryOrigin::LiveDelta,
        scope_root: scope_root.clone(),
        branch_name: "main".into(),
        scope_kind: FeedScopeKind::ProjectRoot,
        worktree_name: None,
        source_terminal_id: None,
        generation: 7,
        changed_file_count: 1,
        insertions: 1,
        deletions: 0,
        files: vec![feed_file_summary(
            "src/lib.rs",
            DiffSectionKind::Unstaged,
            GitFileStatus::Modified,
        )],
        capture_state: FeedCaptureState::Ready(CapturedEventDiff {
            files: vec![CapturedDiffFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/lib.rs"),
                },
                file: changed_file("src/lib.rs", GitFileStatus::Modified),
                document: file_document_with_kinds(
                    7,
                    "src/lib.rs",
                    vec![(
                        orcashell_git::DiffLineKind::Context,
                        "fn themed() { let value = 1; }",
                    )],
                ),
            }],
            failed_files: Vec::new(),
            truncated: false,
            total_rendered_lines: 1,
            total_rendered_bytes: 29,
        }),
    });
    ws.live_diff_feeds.insert(project_id, feed);

    ws.refresh_diff_theme_for(ThemeId::Light);

    let entry = &ws.live_diff_feeds["proj-1"].entries[0];
    let FeedCaptureState::Ready(captured) = &entry.capture_state else {
        panic!("expected ready capture");
    };
    assert!(captured.files[0].document.lines[0].highlights.is_some());
}

#[test]
fn snapshot_error_marks_open_diff_tab_unavailable() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());

    ws.apply_snapshot_update(
        vec![],
        scope_root.clone(),
        Some(scope_root.clone()),
        Err(SnapshotLoadError::unavailable("repo unavailable")),
    );

    let diff_tab = &ws.diff_tabs[&scope_root];
    assert_eq!(diff_tab.index.error.as_deref(), Some("repo unavailable"));
    assert!(!diff_tab.index.loading);
}

#[test]
fn newer_snapshot_marks_diff_tab_stale_and_requests_reload() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    1,
                    "main",
                    vec![changed_file("src/lib.rs", GitFileStatus::Modified)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(1),
            },
            selected_file: Some(DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("src/lib.rs"),
            }),
            file: super::DiffFileState {
                document: Some(file_document("/tmp/repo", 1, "src/lib.rs")),
                error: None,
                loading: false,
                requested_generation: Some(1),
                requested_selection: Some(DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/lib.rs"),
                }),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    ws.apply_snapshot_update(
        vec![],
        scope_root.clone(),
        Some(scope_root.clone()),
        Ok(snapshot_with("/tmp/repo", 2, "feature/reload")),
    );

    assert!(ws.diff_tabs[&scope_root].index.loading);
    assert_eq!(
        ws.diff_tabs[&scope_root].index.requested_generation,
        Some(2)
    );
    assert_eq!(ws.auxiliary_tabs[0].title, "Diff: feature/reload");
}

#[test]
fn select_terminal_across_projects_preserves_other_notifications() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 0)));
    ws.projects
        .push(project("proj-2", tabs(vec![term("t3")], 0)));
    ws.active_project_id = Some("proj-1".into());
    ws.focus.set_current(FocusTarget {
        project_id: "proj-1".into(),
        layout_path: vec![0],
    });
    ws.terminal_runtime
        .insert("t1".into(), runtime_state(Some(NotificationTier::Urgent)));
    ws.terminal_runtime.insert(
        "t3".into(),
        runtime_state(Some(NotificationTier::Informational)),
    );

    assert!(ws.select_terminal_internal("proj-2", &[0]));

    assert_eq!(ws.active_project_id.as_deref(), Some("proj-2"));
    assert_eq!(
        ws.terminal_notification_tier("t1"),
        Some(NotificationTier::Urgent)
    );
    assert_eq!(ws.terminal_notification_tier("t3"), None);
    assert!(ws.focus.is_focused("proj-2", &[0]));
}

#[test]
fn classify_notification_matches_title_and_body() {
    let patterns = vec!["permission".to_string()];

    assert_eq!(
        classify_notification("Permission required", "", &patterns),
        NotificationTier::Urgent
    );
    assert_eq!(
        classify_notification("", "permission required", &patterns),
        NotificationTier::Urgent
    );
    assert_eq!(
        classify_notification("Done", "all clear", &patterns),
        NotificationTier::Informational
    );
}

#[test]
fn late_snapshot_does_not_reattach_closed_terminal() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t2")], 0)));

    let scope_root = PathBuf::from("/tmp/repo");
    ws.apply_snapshot_update(
        vec!["t1".into()],
        scope_root.clone(),
        Some(scope_root.clone()),
        Ok(snapshot("/tmp/repo")),
    );

    assert!(!ws.terminal_git_scopes.contains_key("t1"));
    assert!(ws.git_scopes.contains_key(&scope_root));
}

#[test]
fn unavailable_snapshot_error_preserves_attached_scope() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes
        .insert(scope_root.clone(), snapshot("/tmp/repo"));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());

    ws.apply_snapshot_update(
        vec![],
        scope_root.clone(),
        Some(scope_root.clone()),
        Err(SnapshotLoadError::unavailable("boom")),
    );

    assert_eq!(ws.terminal_git_scopes.get("t1"), Some(&scope_root));
    assert!(ws.git_scopes.contains_key(&scope_root));
}

#[test]
fn not_repository_scope_refresh_detaches_all_terminals_for_failed_scope() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes
        .insert(scope_root.clone(), snapshot("/tmp/repo"));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());

    ws.apply_snapshot_update(
        vec![],
        scope_root.clone(),
        Some(scope_root.clone()),
        Err(SnapshotLoadError::not_repository("boom")),
    );

    assert!(!ws.terminal_git_scopes.contains_key("t1"));
    assert!(!ws.git_scopes.contains_key(&scope_root));
}

#[test]
fn terminal_scoped_not_repository_detaches_only_that_terminal() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes
        .insert(scope_root.clone(), snapshot("/tmp/repo"));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());
    ws.terminal_git_scopes
        .insert("t2".into(), scope_root.clone());

    ws.apply_snapshot_update(
        vec!["t1".into()],
        PathBuf::from("/tmp/outside"),
        Some(scope_root.clone()),
        Err(SnapshotLoadError::not_repository("boom")),
    );

    assert!(!ws.terminal_git_scopes.contains_key("t1"));
    assert_eq!(ws.terminal_git_scopes.get("t2"), Some(&scope_root));
    assert!(ws.git_scopes.contains_key(&scope_root));
}

#[test]
fn leaving_executing_state_requests_git_refresh() {
    let repo = init_repo();
    let services = WorkspaceServices::default();
    let events = services.git.subscribe_events();
    let mut ws = WorkspaceState::new_with_services(services);
    ws.terminal_runtime.insert(
        "t1".into(),
        TerminalRuntimeState {
            shell_label: "zsh".into(),
            live_title: None,
            semantic_state: SemanticState::Executing,
            last_activity_at: None,
            last_local_input_at: Some(Instant::now()),
            notification_tier: None,
            resumable_agent: None,
            pending_agent_detection: false,
        },
    );

    let transition =
        ws.apply_semantic_state_change("t1", SemanticState::CommandComplete { exit_code: Some(0) });
    assert!(transition.changed);
    assert!(transition.refresh_git_snapshot);

    ws.sync_terminal_git_scope("t1", Some(repo.clone()));

    match events.recv_blocking().unwrap() {
        GitEvent::SnapshotUpdated {
            terminal_ids,
            scope_root,
            result,
            ..
        } => {
            assert_eq!(terminal_ids, vec!["t1".to_string()]);
            let snapshot = result.unwrap();
            assert_eq!(scope_root.as_deref(), Some(snapshot.scope_root.as_path()));
            assert_eq!(snapshot.scope_root, fs::canonicalize(repo).unwrap());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn missing_cwd_preserves_terminal_git_scope() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes
        .insert(scope_root.clone(), snapshot("/tmp/repo"));
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());

    ws.sync_terminal_git_scope("t1", None);

    assert_eq!(ws.terminal_git_scopes.get("t1"), Some(&scope_root));
    assert!(ws.git_scopes.contains_key(&scope_root));
}

#[test]
fn selecting_git_backed_terminal_requests_snapshot_refresh() {
    let repo = init_repo();
    let services = WorkspaceServices::default();
    let events = services.git.subscribe_events();
    let mut ws = WorkspaceState::new_with_services(services);
    let mut restored_project = project("proj-1", tabs(vec![term("t1")], 0));
    restored_project.path = repo.clone();
    ws.projects.push(restored_project);

    let scope_root = fs::canonicalize(&repo).unwrap();
    let scope_root_str = scope_root.display().to_string();
    ws.git_scopes.insert(
        scope_root.clone(),
        snapshot_with(&scope_root_str, 1, "main"),
    );
    ws.terminal_git_scopes
        .insert("t1".into(), scope_root.clone());

    assert!(ws.select_terminal_internal("proj-1", &[0]));

    match events.recv_blocking().unwrap() {
        GitEvent::SnapshotUpdated {
            terminal_ids,
            scope_root,
            result,
            ..
        } => {
            assert_eq!(terminal_ids, vec!["t1".to_string()]);
            let snapshot = result.unwrap();
            assert_eq!(scope_root.as_deref(), Some(snapshot.scope_root.as_path()));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn restored_terminal_cwd_prefers_existing_worktree_directory() {
    let repo = init_repo();
    let worktree = orcashell_git::create_managed_worktree(&repo, "wt-12345678").expect("worktree");

    assert_eq!(
        WorkspaceState::restored_terminal_cwd(&repo, Some(&worktree.path)),
        worktree.path
    );
}

#[test]
fn restored_terminal_cwd_falls_back_to_project_path_when_missing() {
    let project_path = PathBuf::from("/tmp/project-root");
    let missing = project_path.join("missing-worktree");

    assert_eq!(
        WorkspaceState::restored_terminal_cwd(&project_path, Some(&missing)),
        project_path
    );
}

#[test]
fn detect_resumable_agent_matches_supported_commands_only() {
    assert_eq!(
        WorkspaceState::detect_resumable_agent(Some("codex --last")),
        Some(ResumableAgentKind::Codex)
    );
    assert_eq!(
        WorkspaceState::detect_resumable_agent(Some("   claude --continue")),
        Some(ResumableAgentKind::ClaudeCode)
    );
    assert_eq!(
        WorkspaceState::detect_resumable_agent(Some("echo codex")),
        None
    );
    assert_eq!(
        WorkspaceState::detect_resumable_agent(Some("env FOO=1 codex")),
        None
    );
}

#[test]
fn entering_executing_enables_pending_agent_detection() {
    let mut ws = WorkspaceState::new();
    ws.terminal_runtime.insert("t1".into(), runtime_state(None));

    let transition = ws.apply_semantic_state_change("t1", SemanticState::Executing);

    assert!(transition.changed);
    assert!(transition.entered_executing);
    assert!(
        ws.terminal_runtime
            .get("t1")
            .unwrap()
            .pending_agent_detection
    );
}

#[test]
fn later_title_changes_do_not_retarget_armed_agent() {
    let mut ws = WorkspaceState::new();
    let mut runtime = runtime_state(None);
    runtime.semantic_state = SemanticState::Executing;
    runtime.pending_agent_detection = true;
    runtime.live_title = Some("codex --last".into());
    ws.terminal_runtime.insert("t1".into(), runtime);

    assert_eq!(
        WorkspaceState::detect_resumable_agent(ws.terminal_runtime["t1"].live_title.as_deref()),
        Some(ResumableAgentKind::Codex)
    );

    {
        let runtime = ws.terminal_runtime.get_mut("t1").unwrap();
        runtime.resumable_agent = Some(ResumableAgentKind::Codex);
        runtime.pending_agent_detection = false;
        runtime.live_title = Some("claude --continue".into());
    }

    assert_eq!(
        ws.terminal_runtime["t1"].resumable_agent,
        Some(ResumableAgentKind::Codex)
    );
}

#[test]
fn build_project_resume_restore_plan_suppresses_duplicate_same_cwd_rows() {
    let mut ws = workspace_with_store();
    ws.projects.push(project(
        "proj-1",
        tabs(vec![term("term-a"), term("term-b")], 0),
    ));

    {
        let mut store = ws.services.store.lock();
        let store = store.as_mut().unwrap();
        store
            .upsert_agent_terminal(&orcashell_store::StoredAgentTerminal {
                terminal_id: "term-a".into(),
                project_id: "proj-1".into(),
                agent_kind: ResumableAgentKind::Codex,
                cwd: PathBuf::from("/repo/wt"),
                updated_at: String::new(),
            })
            .unwrap();
        store
            .upsert_agent_terminal(&orcashell_store::StoredAgentTerminal {
                terminal_id: "term-b".into(),
                project_id: "proj-1".into(),
                agent_kind: ResumableAgentKind::Codex,
                cwd: PathBuf::from("/repo/wt"),
                updated_at: String::new(),
            })
            .unwrap();
        store
            .upsert_agent_terminal(&orcashell_store::StoredAgentTerminal {
                terminal_id: "term-c".into(),
                project_id: "proj-1".into(),
                agent_kind: ResumableAgentKind::ClaudeCode,
                cwd: PathBuf::from("/repo/wt-2"),
                updated_at: String::new(),
            })
            .unwrap();
        store.delete_agent_terminal("term-a").unwrap();
        store
            .upsert_agent_terminal(&orcashell_store::StoredAgentTerminal {
                terminal_id: "term-a".into(),
                project_id: "proj-1".into(),
                agent_kind: ResumableAgentKind::Codex,
                cwd: PathBuf::from("/repo/wt"),
                updated_at: String::new(),
            })
            .unwrap();
    }

    let plan = ws.build_project_resume_restore_plan("proj-1");

    assert_eq!(plan.rows_by_terminal_id.len(), 3);
    assert!(plan.winning_terminal_ids.contains("term-c"));
    assert_ne!(
        plan.winning_terminal_ids.contains("term-a"),
        plan.winning_terminal_ids.contains("term-b")
    );
    assert_eq!(plan.suppressed_duplicates, 1);
}

#[test]
fn prepare_resume_injection_marks_attempted_before_write() {
    let mut ws = WorkspaceState::new();
    ws.pending_resume_injections.insert(
        "term-1".into(),
        super::PendingResumeInjection {
            terminal_id: "term-1".into(),
            agent_kind: ResumableAgentKind::Codex,
            command: WorkspaceState::resume_command(ResumableAgentKind::Codex),
            resume_attempted: false,
        },
    );

    let prepared =
        ws.prepare_resume_injection_attempt("term-1", ResumeInjectionTrigger::PromptReady);
    assert!(prepared.is_some());
    assert!(
        ws.pending_resume_injections["term-1"].resume_attempted,
        "resume attempt should be marked before the write occurs"
    );
    assert!(ws
        .prepare_resume_injection_attempt("term-1", ResumeInjectionTrigger::TimeoutFallback)
        .is_none());
}

#[test]
fn disarm_resumable_agent_clears_persisted_row() {
    let mut ws = workspace_with_store();
    ws.projects
        .push(project("proj-1", tabs(vec![term("term-1")], 0)));
    let mut runtime = runtime_state(None);
    runtime.semantic_state = SemanticState::Executing;
    runtime.resumable_agent = Some(ResumableAgentKind::Codex);
    runtime.pending_agent_detection = false;
    ws.terminal_runtime.insert("term-1".into(), runtime);

    {
        let mut store = ws.services.store.lock();
        store
            .as_mut()
            .unwrap()
            .upsert_agent_terminal(&orcashell_store::StoredAgentTerminal {
                terminal_id: "term-1".into(),
                project_id: "proj-1".into(),
                agent_kind: ResumableAgentKind::Codex,
                cwd: PathBuf::from("/repo/wt"),
                updated_at: String::new(),
            })
            .unwrap();
    }

    ws.disarm_resumable_agent("term-1");

    assert!(ws
        .services
        .store
        .lock()
        .as_ref()
        .unwrap()
        .load_agent_terminals_for_project("proj-1")
        .unwrap()
        .is_empty());
    assert!(ws.terminal_runtime["term-1"].resumable_agent.is_none());
}

#[test]
fn should_queue_resume_injection_requires_winner_and_matching_cwd() {
    let row = orcashell_store::StoredAgentTerminal {
        terminal_id: "term-1".into(),
        project_id: "proj-1".into(),
        agent_kind: ResumableAgentKind::Codex,
        cwd: PathBuf::from("/repo/wt"),
        updated_at: String::new(),
    };
    let winners = HashSet::from([String::from("term-1")]);

    assert!(WorkspaceState::should_queue_resume_injection(
        &row,
        PathBuf::from("/repo/wt").as_path(),
        &winners,
    ));
    assert!(!WorkspaceState::should_queue_resume_injection(
        &row,
        PathBuf::from("/repo/project-root").as_path(),
        &winners,
    ));
    assert!(!WorkspaceState::should_queue_resume_injection(
        &row,
        PathBuf::from("/repo/wt").as_path(),
        &HashSet::new(),
    ));
}

#[test]
fn successful_resume_injection_marks_terminal_for_cleanup() {
    let mut ws = WorkspaceState::new();
    ws.terminal_runtime
        .insert("term-1".into(), runtime_state(None));
    let prepared = super::PreparedResumeInjection {
        terminal_id: "term-1".into(),
        agent_kind: ResumableAgentKind::ClaudeCode,
        command: WorkspaceState::resume_command(ResumableAgentKind::ClaudeCode),
        trigger: ResumeInjectionTrigger::PromptReady,
    };

    ws.mark_resume_injection_succeeded(&prepared);

    assert_eq!(
        ws.terminal_runtime["term-1"].resumable_agent,
        Some(ResumableAgentKind::ClaudeCode)
    );
    assert!(!ws.terminal_runtime["term-1"].pending_agent_detection);
}

// ── CP2: Action state tests ──────────────────────────────────────

#[test]
fn local_action_completed_stage_clears_flag_no_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));
    ws.diff_tabs
        .get_mut(&scope_root)
        .unwrap()
        .local_action_in_flight = true;

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::Stage,
        Ok("Staged successfully".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    // Stage/unstage success skips banner. The tree update is feedback enough.
    assert!(tab.last_action_banner.is_none());
}

#[test]
fn local_action_completed_discard_file_clears_flag_no_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));
    ws.diff_tabs
        .get_mut(&scope_root)
        .unwrap()
        .local_action_in_flight = true;

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::DiscardFile,
        Ok("Discarded src/lib.rs".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    assert!(tab.last_action_banner.is_none());
}

#[test]
fn local_action_completed_discard_all_sets_success_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));
    ws.diff_tabs
        .get_mut(&scope_root)
        .unwrap()
        .local_action_in_flight = true;

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::DiscardAll,
        Ok("Discarded all unstaged changes".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Success
    );
}

#[test]
fn local_action_completed_discard_hunk_blocked_sets_warning_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));
    ws.diff_tabs
        .get_mut(&scope_root)
        .unwrap()
        .local_action_in_flight = true;

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::DiscardHunk,
        Ok("BLOCKED: Selected hunk changed. Refresh and try again.".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Warning
    );
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().message,
        "Selected hunk changed. Refresh and try again."
    );
}

#[test]
fn local_action_completed_commit_sets_success_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope_root.clone(), DiffTabState::new(scope_root.clone()));
    ws.diff_tabs
        .get_mut(&scope_root)
        .unwrap()
        .local_action_in_flight = true;

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::Commit,
        Ok("Committed abc12345".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Success
    );
}

#[test]
fn local_action_completed_error_sets_error_banner_and_keeps_multi_select() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.local_action_in_flight = true;
    tab.multi_select.insert(DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    });
    ws.diff_tabs.insert(scope_root.clone(), tab);

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::Stage,
        Err("index locked".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.local_action_in_flight);
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Error
    );
    // Multi-select should NOT be cleared on error
    assert!(!tab.multi_select.is_empty());
}

#[test]
fn successful_commit_clears_commit_message_and_multi_select() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.local_action_in_flight = true;
    tab.commit_message = "my commit".into();
    tab.multi_select.insert(DiffSelectionKey {
        section: DiffSectionKind::Staged,
        relative_path: PathBuf::from("a.txt"),
    });
    ws.diff_tabs.insert(scope_root.clone(), tab);

    ws.apply_local_action_completion(
        scope_root.clone(),
        GitActionKind::Commit,
        Ok("Committed abc12345".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(tab.commit_message.is_empty());
    assert!(tab.multi_select.is_empty());
}

#[test]
fn remote_op_completed_clears_flag_and_sets_banner() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope_root.clone());
    tab.remote_op_in_flight = true;
    ws.diff_tabs.insert(scope_root.clone(), tab);

    ws.apply_remote_op_completion(
        scope_root.clone(),
        GitRemoteKind::Push,
        None,
        false,
        Ok("Everything up-to-date".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.remote_op_in_flight);
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Success
    );
}

#[test]
fn publish_remote_op_requests_diff_index_refresh() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 7, "feature"));

    let mut tab = DiffTabState::new(scope_root.clone());
    tab.remote_op_in_flight = true;
    tab.index.loading = false;
    ws.diff_tabs.insert(scope_root.clone(), tab);

    ws.apply_remote_op_completion(
        scope_root.clone(),
        GitRemoteKind::Publish,
        None,
        true,
        Ok("Published current branch to origin".into()),
    );

    let tab = &ws.diff_tabs[&scope_root];
    assert!(!tab.remote_op_in_flight);
    assert!(tab.index.loading);
    assert_eq!(tab.index.requested_generation, Some(7));
    assert_eq!(
        tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Success
    );
}

#[test]
fn validate_discard_selected_file_rejects_stale_displayed_file() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 9, "main"));
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    9,
                    "main",
                    vec![changed_file("src/lib.rs", GitFileStatus::Modified)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(9),
            },
            selected_file: Some(selection.clone()),
            file: super::DiffFileState {
                document: Some(file_document("/tmp/repo", 8, "src/lib.rs")),
                error: None,
                loading: false,
                requested_generation: Some(8),
                requested_selection: Some(selection),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    let result = ws.validate_discard_selected_file(&scope_root);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("stale"));
}

#[test]
fn validate_discard_selected_hunk_rejects_missing_hunk() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/lib.rs"),
    };
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 9, "main"));
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    9,
                    "main",
                    vec![changed_file("src/lib.rs", GitFileStatus::Modified)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(9),
            },
            selected_file: Some(selection.clone()),
            file: super::DiffFileState {
                document: Some(FileDiffDocument {
                    generation: 9,
                    selection,
                    file: changed_file("src/lib.rs", GitFileStatus::Modified),
                    lines: vec![
                        orcashell_git::DiffLineView {
                            kind: orcashell_git::DiffLineKind::HunkHeader,
                            old_lineno: None,
                            new_lineno: None,
                            text: "@@ -1 +1 @@".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                        orcashell_git::DiffLineView {
                            kind: orcashell_git::DiffLineKind::Deletion,
                            old_lineno: Some(1),
                            new_lineno: None,
                            text: "-old\n".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                    ],
                    hunks: vec![FileDiffHunk {
                        hunk_index: 2,
                        old_start: 1,
                        old_lines: 1,
                        new_start: 1,
                        new_lines: 1,
                        header: "@@ -1 +1 @@".into(),
                        body_fingerprint: 7,
                        line_start: 0,
                        line_end: 2,
                    }],
                }),
                error: None,
                loading: false,
                requested_generation: Some(9),
                requested_selection: Some(DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/lib.rs"),
                }),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    let result = ws.validate_discard_selected_hunk(&scope_root, 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("no longer available"));
}

#[test]
fn validate_discard_selected_file_rejects_renamed_file() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/renamed.rs"),
    };
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 9, "main"));
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    9,
                    "main",
                    vec![changed_file("src/renamed.rs", GitFileStatus::Renamed)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(9),
            },
            selected_file: Some(selection.clone()),
            file: super::DiffFileState {
                document: Some(FileDiffDocument {
                    generation: 9,
                    selection,
                    file: changed_file("src/renamed.rs", GitFileStatus::Renamed),
                    lines: Vec::new(),
                    hunks: Vec::new(),
                }),
                error: None,
                loading: false,
                requested_generation: Some(9),
                requested_selection: Some(DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/renamed.rs"),
                }),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    let result = ws.validate_discard_selected_file(&scope_root);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("renamed or typechanged"));
}

#[test]
fn validate_discard_selected_hunk_rejects_renamed_file() {
    let mut ws = WorkspaceState::new();
    let scope_root = PathBuf::from("/tmp/repo");
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/renamed.rs"),
    };
    ws.git_scopes
        .insert(scope_root.clone(), snapshot_with("/tmp/repo", 9, "main"));
    ws.diff_tabs.insert(
        scope_root.clone(),
        DiffTabState {
            scope_root: scope_root.clone(),
            tree_width: 300.0,
            index: super::DiffIndexState {
                document: Some(diff_document(
                    "/tmp/repo",
                    9,
                    "main",
                    vec![changed_file("src/renamed.rs", GitFileStatus::Renamed)],
                )),
                error: None,
                loading: false,
                requested_generation: Some(9),
            },
            selected_file: Some(selection.clone()),
            file: super::DiffFileState {
                document: Some(FileDiffDocument {
                    generation: 9,
                    selection,
                    file: changed_file("src/renamed.rs", GitFileStatus::Renamed),
                    lines: Vec::new(),
                    hunks: Vec::new(),
                }),
                error: None,
                loading: false,
                requested_generation: Some(9),
                requested_selection: Some(DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/renamed.rs"),
                }),
            },
            ..DiffTabState::new(scope_root.clone())
        },
    );

    let result = ws.validate_discard_selected_hunk(&scope_root, 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("renamed or typechanged"));
}

// ── Multi-select tests ──────────────────────────────────────────

fn make_key(section: DiffSectionKind, path: &str) -> DiffSelectionKey {
    DiffSelectionKey {
        section,
        relative_path: PathBuf::from(path),
    }
}

#[test]
fn diff_replace_select_clears_and_sets_single() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    tab.multi_select
        .insert(make_key(DiffSectionKind::Unstaged, "a.rs"));
    tab.multi_select
        .insert(make_key(DiffSectionKind::Unstaged, "b.rs"));
    ws.diff_tabs.insert(scope.clone(), tab);

    let key = make_key(DiffSectionKind::Staged, "c.rs");
    ws.diff_replace_select_internal(&scope, key.clone());

    let tab = &ws.diff_tabs[&scope];
    assert_eq!(tab.multi_select.len(), 1);
    assert!(tab.multi_select.contains(&key));
    assert_eq!(tab.selection_anchor, Some(key));
}

#[test]
fn diff_toggle_multi_select_adds_and_removes() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope.clone(), DiffTabState::new(scope.clone()));

    let key_a = make_key(DiffSectionKind::Unstaged, "a.rs");
    let key_b = make_key(DiffSectionKind::Unstaged, "b.rs");

    // Toggle on a
    ws.diff_toggle_multi_select_internal(&scope, key_a.clone());
    assert!(ws.diff_tabs[&scope].multi_select.contains(&key_a));

    // Toggle on b (same section)
    ws.diff_toggle_multi_select_internal(&scope, key_b.clone());
    assert!(ws.diff_tabs[&scope].multi_select.contains(&key_a));
    assert!(ws.diff_tabs[&scope].multi_select.contains(&key_b));

    // Toggle off a
    ws.diff_toggle_multi_select_internal(&scope, key_a.clone());
    assert!(!ws.diff_tabs[&scope].multi_select.contains(&key_a));
    assert!(ws.diff_tabs[&scope].multi_select.contains(&key_b));
}

#[test]
fn diff_toggle_multi_select_cross_section_clears() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope.clone(), DiffTabState::new(scope.clone()));

    let unstaged_key = make_key(DiffSectionKind::Unstaged, "a.rs");
    let staged_key = make_key(DiffSectionKind::Staged, "b.rs");

    ws.diff_toggle_multi_select_internal(&scope, unstaged_key.clone());
    assert_eq!(ws.diff_tabs[&scope].multi_select.len(), 1);

    // Toggling a staged key should clear unstaged keys
    ws.diff_toggle_multi_select_internal(&scope, staged_key.clone());
    assert_eq!(ws.diff_tabs[&scope].multi_select.len(), 1);
    assert!(ws.diff_tabs[&scope].multi_select.contains(&staged_key));
    assert!(!ws.diff_tabs[&scope].multi_select.contains(&unstaged_key));
}

#[test]
fn diff_range_select_within_section() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope.clone(), DiffTabState::new(scope.clone()));

    let a = make_key(DiffSectionKind::Unstaged, "a.rs");
    let b = make_key(DiffSectionKind::Unstaged, "b.rs");
    let c = make_key(DiffSectionKind::Unstaged, "c.rs");
    let visible = vec![a.clone(), b.clone(), c.clone()];

    // Set anchor at a
    ws.diff_replace_select_internal(&scope, a.clone());

    // Range select to c
    ws.diff_range_select_internal(&scope, c.clone(), &visible);

    let tab = &ws.diff_tabs[&scope];
    assert_eq!(tab.multi_select.len(), 3);
    assert!(tab.multi_select.contains(&a));
    assert!(tab.multi_select.contains(&b));
    assert!(tab.multi_select.contains(&c));
}

#[test]
fn diff_range_select_cross_section_falls_back() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    ws.diff_tabs
        .insert(scope.clone(), DiffTabState::new(scope.clone()));

    let staged = make_key(DiffSectionKind::Staged, "a.rs");
    let unstaged = make_key(DiffSectionKind::Unstaged, "b.rs");
    let visible = vec![staged.clone(), unstaged.clone()];

    // Set anchor at staged
    ws.diff_replace_select_internal(&scope, staged.clone());

    // Range select to unstaged (different section) → falls back to replace
    ws.diff_range_select_internal(&scope, unstaged.clone(), &visible);

    let tab = &ws.diff_tabs[&scope];
    assert_eq!(tab.multi_select.len(), 1);
    assert!(tab.multi_select.contains(&unstaged));
}

// ── CP4: Remove-worktree + scope exclusion tests ─────────────────

#[test]
fn remove_confirmation_cancel_restores_normal_state() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    tab.managed_worktree = Some(super::ManagedWorktreeSummary {
        id: "wt-abc12345".into(),
        branch_name: "orca/wt-abc12345".into(),
        source_ref: "refs/heads/main".into(),
    });
    ws.diff_tabs.insert(scope.clone(), tab);

    // Begin confirmation.
    {
        let tab = ws.diff_tabs.get_mut(&scope).unwrap();
        tab.remove_worktree_confirm = Some(super::RemoveWorktreeConfirm {
            delete_branch: false,
        });
    }
    assert!(ws.diff_tabs[&scope].remove_worktree_confirm.is_some());

    // Cancel.
    {
        let tab = ws.diff_tabs.get_mut(&scope).unwrap();
        tab.remove_worktree_confirm = None;
    }
    assert!(ws.diff_tabs[&scope].remove_worktree_confirm.is_none());
}

#[test]
fn remove_confirmation_toggle_delete_branch() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    tab.managed_worktree = Some(super::ManagedWorktreeSummary {
        id: "wt-abc12345".into(),
        branch_name: "orca/wt-abc12345".into(),
        source_ref: "refs/heads/main".into(),
    });
    tab.remove_worktree_confirm = Some(super::RemoveWorktreeConfirm {
        delete_branch: false,
    });
    ws.diff_tabs.insert(scope.clone(), tab);

    assert!(
        !ws.diff_tabs[&scope]
            .remove_worktree_confirm
            .as_ref()
            .unwrap()
            .delete_branch
    );

    // Toggle.
    {
        let tab = ws.diff_tabs.get_mut(&scope).unwrap();
        if let Some(confirm) = &mut tab.remove_worktree_confirm {
            confirm.delete_branch = !confirm.delete_branch;
        }
    }
    assert!(
        ws.diff_tabs[&scope]
            .remove_worktree_confirm
            .as_ref()
            .unwrap()
            .delete_branch
    );
}

#[test]
fn any_action_in_flight_blocks_all_dispatch() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    // Set remote op in flight.
    tab.remote_op_in_flight = true;
    // Add staged + unstaged files so stage/unstage/commit can potentially fire.
    tab.multi_select
        .insert(make_key(DiffSectionKind::Unstaged, "a.rs"));
    tab.commit_message = "test".to_string();
    tab.index.document = Some(diff_document(
        "/tmp/repo",
        1,
        "main",
        vec![changed_file("a.rs", GitFileStatus::Modified)],
    ));
    // Add staged files to the document.
    if let Some(doc) = &mut tab.index.document {
        doc.staged_files = vec![changed_file("a.rs", GitFileStatus::Modified)];
    }
    ws.diff_tabs.insert(scope.clone(), tab);

    // stage_selected should no-op (guard on any_action_in_flight).
    let before_flag = ws.diff_tabs[&scope].local_action_in_flight;
    // We can't call the cx-requiring methods in a unit test, so verify the
    // guard directly.
    assert!(WorkspaceState::any_action_in_flight(
        ws.diff_tabs.get(&scope).unwrap()
    ));
    assert!(!before_flag); // local_action_in_flight is not set.
}

#[test]
fn successful_remove_closes_tab_and_deletes_sqlite() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    tab.managed_worktree = Some(super::ManagedWorktreeSummary {
        id: "wt-abc12345".into(),
        branch_name: "orca/wt-abc12345".into(),
        source_ref: "refs/heads/main".into(),
    });
    tab.local_action_in_flight = true;
    ws.diff_tabs.insert(scope.clone(), tab);

    // Add the auxiliary tab.
    let tab_id = WorkspaceState::diff_tab_id(&scope);
    ws.auxiliary_tabs.push(super::AuxiliaryTabState {
        id: tab_id.clone(),
        title: "Diff: orca/wt-abc12345".into(),
        kind: AuxiliaryTabKind::Diff {
            scope_root: scope.clone(),
        },
    });

    // Simulate successful RemoveWorktree completion.
    ws.apply_local_action_completion(
        scope.clone(),
        GitActionKind::RemoveWorktree,
        Ok("Worktree removed".to_string()),
    );

    // Tab should be removed.
    assert!(!ws.diff_tabs.contains_key(&scope));
    assert!(!ws.auxiliary_tabs.iter().any(|t| t.id == tab_id));
}

#[test]
fn close_terminals_by_id_removes_layout_nodes_not_just_sessions() {
    let mut ws = WorkspaceState::new();
    ws.projects
        .push(project("proj-1", tabs(vec![term("t1"), term("t2")], 1)));
    ws.active_project_id = Some("proj-1".into());
    ws.focus.set_current(FocusTarget {
        project_id: "proj-1".into(),
        layout_path: vec![1],
    });
    ws.terminal_runtime.insert("t1".into(), runtime_state(None));
    ws.terminal_runtime.insert("t2".into(), runtime_state(None));

    ws.close_terminals_by_id_internal(&["t2".to_string()]);

    let project = ws.project("proj-1").unwrap();
    assert!(project.layout.find_terminal_path("t2").is_none());
    assert!(project.layout.find_terminal_path("t1").is_some());
    assert!(!ws.terminal_runtime.contains_key("t2"));
}

#[test]
fn failed_remove_preserves_sqlite_row() {
    let mut ws = WorkspaceState::new();
    let scope = PathBuf::from("/tmp/repo");
    let mut tab = DiffTabState::new(scope.clone());
    tab.managed_worktree = Some(super::ManagedWorktreeSummary {
        id: "wt-abc12345".into(),
        branch_name: "orca/wt-abc12345".into(),
        source_ref: "refs/heads/main".into(),
    });
    tab.local_action_in_flight = true;
    ws.diff_tabs.insert(scope.clone(), tab);

    // Add the auxiliary tab.
    let tab_id = WorkspaceState::diff_tab_id(&scope);
    ws.auxiliary_tabs.push(super::AuxiliaryTabState {
        id: tab_id.clone(),
        title: "Diff: orca/wt-abc12345".into(),
        kind: AuxiliaryTabKind::Diff {
            scope_root: scope.clone(),
        },
    });

    // Simulate failed RemoveWorktree completion.
    ws.apply_local_action_completion(
        scope.clone(),
        GitActionKind::RemoveWorktree,
        Err("worktree removal failed".to_string()),
    );

    // Tab should still exist with error banner.
    assert!(ws.diff_tabs.contains_key(&scope));
    assert!(ws.auxiliary_tabs.iter().any(|t| t.id == tab_id));
    let diff_tab = &ws.diff_tabs[&scope];
    assert!(!diff_tab.local_action_in_flight);
    assert_eq!(
        diff_tab.last_action_banner.as_ref().unwrap().kind,
        super::ActionBannerKind::Error
    );
}

#[test]
fn repository_fetch_completion_sets_banner_and_keeps_graph_document() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = repository_graph_document(scope_root, "main");

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.graph.document = Some(graph.clone());
    tab.graph.loading = false;
    tab.fetch_in_flight = true;

    ws.apply_remote_op_completion(
        PathBuf::from(scope_root),
        GitRemoteKind::Fetch,
        Some(GitFetchOrigin::Manual),
        false,
        Err("Network error".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.fetch_in_flight);
    assert_eq!(tab.graph.document.as_ref(), Some(&graph));
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Error
    );
    assert!(tab.last_remote_check_at.is_none());
    assert!(tab.last_automatic_fetch_failure_at.is_none());
}

#[test]
fn automatic_repository_fetch_completion_updates_freshness_without_banner() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = repository_graph_document(scope_root, "main");

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.graph.document = Some(graph.clone());
    tab.graph.loading = false;
    tab.fetch_in_flight = true;
    tab.active_fetch_origin = Some(GitFetchOrigin::Automatic);

    ws.apply_remote_op_completion(
        PathBuf::from(scope_root),
        GitRemoteKind::Fetch,
        Some(GitFetchOrigin::Automatic),
        false,
        Ok("Remote refs already up to date".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.fetch_in_flight);
    assert!(tab.active_fetch_origin.is_none());
    assert!(tab.last_remote_check_at.is_some());
    assert!(tab.last_automatic_fetch_failure_at.is_none());
    assert_eq!(tab.graph.document.as_ref(), Some(&graph));
    assert!(!tab.graph.loading);
    assert!(tab.action_banner.is_none());
}

#[test]
fn automatic_repository_fetch_failure_sets_cooldown_without_banner() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = repository_graph_document(scope_root, "main");

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.graph.document = Some(graph.clone());
    tab.graph.loading = false;
    tab.fetch_in_flight = true;
    tab.active_fetch_origin = Some(GitFetchOrigin::Automatic);

    ws.apply_remote_op_completion(
        PathBuf::from(scope_root),
        GitRemoteKind::Fetch,
        Some(GitFetchOrigin::Automatic),
        false,
        Err("Network error".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.fetch_in_flight);
    assert!(tab.active_fetch_origin.is_none());
    assert!(tab.last_remote_check_at.is_none());
    assert!(tab.last_automatic_fetch_failure_at.is_some());
    assert_eq!(tab.graph.document.as_ref(), Some(&graph));
    assert!(!tab.graph.loading);
    assert!(tab.action_banner.is_none());
}

#[test]
fn repository_graph_update_seeds_expanded_remote_groups_from_head_upstream() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let head_oid = oid(1);
    let other_oid = oid(2);
    let graph = RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head: HeadState::Branch {
            name: "main".to_string(),
            oid: head_oid,
        },
        local_branches: vec![LocalBranchEntry {
            name: "main".to_string(),
            full_ref: "refs/heads/main".to_string(),
            target: head_oid,
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "main".to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: vec![
            RemoteBranchEntry {
                remote_name: "origin".to_string(),
                short_name: "main".to_string(),
                full_ref: "refs/remotes/origin/main".to_string(),
                target: head_oid,
                tracked_by_local: Some("main".to_string()),
            },
            RemoteBranchEntry {
                remote_name: "backup".to_string(),
                short_name: "main".to_string(),
                full_ref: "refs/remotes/backup/main".to_string(),
                target: other_oid,
                tracked_by_local: None,
            },
        ],
        commits: vec![CommitGraphNode {
            oid: head_oid,
            short_oid: head_oid.to_string()[..8].to_string(),
            summary: "head".to_string(),
            author_name: "Orca".to_string(),
            authored_at_unix: 1_700_000_000,
            parent_oids: Vec::new(),
            primary_lane: 0,
            row_lanes: vec![GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::Start,
                target_lane: None,
            }],
            ref_labels: Vec::new(),
        }],
        truncated: false,
    };

    ws.apply_repository_graph_update(PathBuf::from(scope_root), 0, Ok(graph));

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.remote_groups_seeded);
    assert!(tab.expanded_remote_groups.contains("origin"));
    assert!(!tab.expanded_remote_groups.contains("backup"));
}

#[test]
fn selecting_remote_branch_auto_expands_its_group() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head: HeadState::Branch {
            name: "main".to_string(),
            oid: oid(1),
        },
        local_branches: vec![LocalBranchEntry {
            name: "main".to_string(),
            full_ref: "refs/heads/main".to_string(),
            target: oid(1),
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "main".to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: vec![
            RemoteBranchEntry {
                remote_name: "origin".to_string(),
                short_name: "main".to_string(),
                full_ref: "refs/remotes/origin/main".to_string(),
                target: oid(1),
                tracked_by_local: Some("main".to_string()),
            },
            RemoteBranchEntry {
                remote_name: "backup".to_string(),
                short_name: "feature".to_string(),
                full_ref: "refs/remotes/backup/feature".to_string(),
                target: oid(2),
                tracked_by_local: None,
            },
        ],
        commits: Vec::new(),
        truncated: false,
    };

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.graph.document = Some(graph);
        tab.expanded_remote_groups.insert("origin".to_string());
        tab.remote_groups_seeded = true;
    }

    assert!(ws.select_repository_branch_internal(
        &project_id,
        RepositoryBranchSelection::Remote {
            full_ref: "refs/remotes/backup/feature".to_string(),
        },
    ));

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.expanded_remote_groups.contains("origin"));
    assert!(tab.expanded_remote_groups.contains("backup"));
}

#[test]
fn repository_graph_refresh_preserves_user_collapsed_selected_remote_group() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head: HeadState::Branch {
            name: "main".to_string(),
            oid: oid(1),
        },
        local_branches: vec![LocalBranchEntry {
            name: "main".to_string(),
            full_ref: "refs/heads/main".to_string(),
            target: oid(1),
            is_head: true,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "main".to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: vec![RemoteBranchEntry {
            remote_name: "backup".to_string(),
            short_name: "feature".to_string(),
            full_ref: "refs/remotes/backup/feature".to_string(),
            target: oid(2),
            tracked_by_local: None,
        }],
        commits: Vec::new(),
        truncated: false,
    };

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.selected_branch = Some(RepositoryBranchSelection::Remote {
            full_ref: "refs/remotes/backup/feature".to_string(),
        });
        tab.remote_groups_seeded = true;
        tab.expanded_remote_groups.clear();
    }

    ws.apply_repository_graph_update(PathBuf::from(scope_root), 0, Ok(graph));

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.expanded_remote_groups.contains("backup"));
}

#[test]
fn repository_graph_refresh_preserves_user_collapsed_local_upstream_group() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = RepositoryGraphDocument {
        scope_root: PathBuf::from(scope_root),
        repo_root: PathBuf::from(scope_root),
        head: HeadState::Detached { oid: oid(9) },
        local_branches: vec![LocalBranchEntry {
            name: "feature".to_string(),
            full_ref: "refs/heads/feature".to_string(),
            target: oid(2),
            is_head: false,
            upstream: Some(BranchTrackingInfo {
                remote_name: "origin".to_string(),
                remote_ref: "feature".to_string(),
                ahead: 0,
                behind: 0,
            }),
        }],
        remote_branches: vec![RemoteBranchEntry {
            remote_name: "origin".to_string(),
            short_name: "feature".to_string(),
            full_ref: "refs/remotes/origin/feature".to_string(),
            target: oid(2),
            tracked_by_local: Some("feature".to_string()),
        }],
        commits: Vec::new(),
        truncated: false,
    };

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.selected_branch = Some(RepositoryBranchSelection::Local {
            name: "feature".to_string(),
        });
        tab.remote_groups_seeded = true;
        tab.expanded_remote_groups.clear();
    }

    ws.apply_repository_graph_update(PathBuf::from(scope_root), 0, Ok(graph));

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.expanded_remote_groups.is_empty());
}

#[test]
fn repository_auto_fetch_due_respects_recent_checks_and_failure_cooldown() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let now = SystemTime::now();

    assert!(ws.repository_auto_fetch_due(&project_id, now));

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.last_remote_check_at = Some(now);
    }
    assert!(!ws.repository_auto_fetch_due(&project_id, now));

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.last_remote_check_at = Some(now - std::time::Duration::from_secs(181));
        tab.last_automatic_fetch_failure_at = Some(now);
    }
    assert!(!ws.repository_auto_fetch_due(&project_id, now));

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.last_automatic_fetch_failure_at = Some(now - std::time::Duration::from_secs(301));
    }
    assert!(ws.repository_auto_fetch_due(&project_id, now));
}

#[test]
fn repository_toolbar_busy_ignores_automatic_fetch_only() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let scope = PathBuf::from(scope_root);

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.fetch_in_flight = true;
        tab.active_fetch_origin = Some(GitFetchOrigin::Automatic);
    }
    assert!(!ws.repository_toolbar_action_in_flight(&scope));
    assert!(ws.scope_git_action_in_flight(&scope));

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.active_fetch_origin = Some(GitFetchOrigin::Manual);
    }
    assert!(ws.repository_toolbar_action_in_flight(&scope));
}

#[test]
fn repository_toolbar_busy_when_pull_is_in_flight() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let scope = PathBuf::from(scope_root);

    {
        let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
        tab.pull_in_flight = true;
    }

    assert!(ws.scope_git_action_in_flight(&scope));
    assert!(ws.repository_toolbar_action_in_flight(&scope));
}

#[test]
fn repository_pull_completion_sets_banner_and_marks_graph_loading() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();
    let graph = repository_graph_document(scope_root, "main");

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.graph.document = Some(graph.clone());
    tab.graph.loading = false;
    tab.pull_in_flight = true;

    ws.apply_remote_op_completion(
        PathBuf::from(scope_root),
        GitRemoteKind::Pull,
        None,
        true,
        Ok("Pulled (fast-forward to 12345678)".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.pull_in_flight);
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Success
    );
    assert!(tab.graph.loading);
    assert_eq!(tab.graph.document.as_ref(), Some(&graph));
}

#[test]
fn repository_pull_conflict_completion_sets_warning_banner() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.pull_in_flight = true;

    ws.apply_remote_op_completion(
        PathBuf::from(scope_root),
        GitRemoteKind::Pull,
        None,
        true,
        Ok("CONFLICT: Resolve conflicts in the diff tab.".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.pull_in_flight);
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Warning
    );
    assert_eq!(
        tab.action_banner.as_ref().unwrap().message,
        "Resolve conflicts in the diff tab."
    );
    assert!(tab.graph.loading);
}

#[test]
fn repository_checkout_blocked_completion_sets_warning_banner() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.active_branch_action = Some(RepositoryBranchAction::Checkout {
        selection: RepositoryBranchSelection::Local {
            name: "feature".to_string(),
        },
    });
    tab.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "feature".to_string(),
    });

    ws.apply_local_action_completion(
        PathBuf::from(scope_root),
        GitActionKind::CheckoutLocalBranch,
        Ok("BLOCKED: Cannot checkout branches with uncommitted changes.".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.active_branch_action.is_none());
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Warning
    );
}

#[test]
fn repository_create_branch_completion_selects_new_branch_and_requests_refresh() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.active_branch_action = Some(RepositoryBranchAction::Create {
        source_branch_name: "main".to_string(),
        new_branch_name: "feature".to_string(),
    });
    tab.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "main".to_string(),
    });
    tab.graph.loading = false;

    ws.apply_local_action_completion(
        PathBuf::from(scope_root),
        GitActionKind::CreateLocalBranch,
        Ok("Created and checked out local branch feature from main".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.active_branch_action.is_none());
    assert!(tab.graph.loading);
    assert_eq!(
        tab.selected_branch,
        Some(RepositoryBranchSelection::Local {
            name: "feature".to_string()
        })
    );
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Success
    );
}

#[test]
fn repository_delete_branch_completion_clears_deleted_selection_and_requests_refresh() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.active_branch_action = Some(RepositoryBranchAction::Delete {
        branch_name: "feature".to_string(),
    });
    tab.selected_branch = Some(RepositoryBranchSelection::Local {
        name: "feature".to_string(),
    });
    tab.graph.loading = false;

    ws.apply_local_action_completion(
        PathBuf::from(scope_root),
        GitActionKind::DeleteLocalBranch,
        Ok("Deleted local branch feature".to_string()),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(tab.active_branch_action.is_none());
    assert!(tab.graph.loading);
    assert!(tab.selected_branch.is_none());
    assert_eq!(
        tab.action_banner.as_ref().unwrap().kind,
        ActionBannerKind::Success
    );
}

#[test]
fn repository_auto_refresh_graph_update_accepts_revision_zero() {
    let scope_root = "/tmp/repo";
    let mut ws = repository_workspace(scope_root);
    let project_id = "repo-project".to_string();

    let tab = ws.repository_graph_tabs.get_mut(&project_id).unwrap();
    tab.graph.requested_revision = 9;
    tab.graph.document = Some(repository_graph_document(scope_root, "main"));
    tab.graph.loading = true;

    ws.apply_repository_graph_update(
        PathBuf::from(scope_root),
        0,
        Ok(repository_graph_document(scope_root, "feature")),
    );

    let tab = ws.repository_graph_tabs.get(&project_id).unwrap();
    assert!(!tab.graph.loading);
    assert!(matches!(
        tab.graph.document.as_ref().unwrap().head,
        HeadState::Branch { ref name, .. } if name == "feature"
    ));
    assert_eq!(
        tab.selected_branch,
        Some(RepositoryBranchSelection::Local {
            name: "feature".to_string()
        })
    );
}
