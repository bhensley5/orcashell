use super::*;
use orcashell_git::{list_stashes, HeadState, Oid};
use std::fs;
use tempfile::TempDir;

fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "user.name", "Orca"]);
    run_git(dir.path(), &["config", "user.email", "orca@example.com"]);
    fs::write(dir.path().join("tracked.txt"), "hello\n").unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    run_git(dir.path(), &["commit", "-m", "init"]);
    dir
}

fn setup_tracking_repo() -> (TempDir, PathBuf) {
    let tempdir = TempDir::new().unwrap();
    let bare_dir = tempdir.path().join("bare.git");
    run_git(
        tempdir.path(),
        &["init", "--bare", bare_dir.to_str().unwrap()],
    );

    let client_dir = tempdir.path().join("client");
    run_git(
        tempdir.path(),
        &[
            "clone",
            bare_dir.to_str().unwrap(),
            client_dir.to_str().unwrap(),
        ],
    );
    run_git(&client_dir, &["config", "user.name", "Orca"]);
    run_git(&client_dir, &["config", "user.email", "orca@example.com"]);
    fs::write(client_dir.join("file.txt"), "initial\n").unwrap();
    run_git(&client_dir, &["add", "file.txt"]);
    run_git(&client_dir, &["commit", "-m", "initial"]);
    let branch = run_git_capture(&client_dir, &["branch", "--show-current"]);
    run_git(
        &client_dir,
        &[
            "push",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    );

    (tempdir, client_dir)
}

fn run_git(cwd: &Path, args: &[&str]) {
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

fn run_git_capture(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn recv_event(rx: &AsyncReceiver<GitEvent>) -> GitEvent {
    rx.recv_blocking()
        .expect("git event channel closed unexpectedly")
}

fn head_oid(cwd: &Path) -> Oid {
    Oid::from_str(&run_git_capture(cwd, &["rev-parse", "HEAD"])).unwrap()
}

fn create_remote_branch(tempdir: &TempDir, bare_dir: &Path, branch_name: &str) {
    let other_dir = tempdir
        .path()
        .join(format!("other-{}", branch_name.replace('/', "-")));
    run_git(
        tempdir.path(),
        &[
            "clone",
            bare_dir.to_str().unwrap(),
            other_dir.to_str().unwrap(),
        ],
    );
    run_git(&other_dir, &["config", "user.name", "Orca"]);
    run_git(&other_dir, &["config", "user.email", "orca@example.com"]);
    run_git(&other_dir, &["checkout", "-b", branch_name]);
    fs::write(other_dir.join(format!("{branch_name}.txt")), "branch\n").unwrap();
    run_git(&other_dir, &["add", "."]);
    run_git(
        &other_dir,
        &["commit", "-m", &format!("{branch_name} commit")],
    );
    let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");
    run_git(&other_dir, &["push", "origin", &refspec]);
}

fn init_repo_with_contents(contents: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "user.name", "Orca"]);
    run_git(dir.path(), &["config", "user.email", "orca@example.com"]);
    fs::write(dir.path().join("tracked.txt"), contents).unwrap();
    run_git(dir.path(), &["add", "tracked.txt"]);
    run_git(dir.path(), &["commit", "-m", "init"]);
    dir
}

// ── Existing snapshot tests ──────────────────────────────────────

#[test]
fn snapshot_request_publishes_cached_summary() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));

    let event = recv_event(&events);
    match event {
        GitEvent::SnapshotUpdated {
            terminal_ids,
            scope_root,
            result,
            ..
        } => {
            assert_eq!(terminal_ids, vec!["term-1".to_string()]);
            let summary = result.unwrap();
            assert_eq!(scope_root.as_deref(), Some(summary.scope_root.as_path()));
            assert!(!summary.branch_name.is_empty());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn identical_snapshot_refresh_keeps_generation_stable() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let first_generation = match recv_event(&events) {
        GitEvent::SnapshotUpdated { result, .. } => result.unwrap().generation,
        other => panic!("unexpected event: {other:?}"),
    };

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let second_generation = match recv_event(&events) {
        GitEvent::SnapshotUpdated { result, .. } => result.unwrap().generation,
        other => panic!("unexpected event: {other:?}"),
    };

    assert_eq!(first_generation, second_generation);
}

#[test]
fn changed_snapshot_refresh_advances_generation() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let first_generation = match recv_event(&events) {
        GitEvent::SnapshotUpdated { result, .. } => result.unwrap().generation,
        other => panic!("unexpected event: {other:?}"),
    };

    fs::write(repo.path().join("tracked.txt"), "hello\nworld\n").unwrap();
    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let second_generation = match recv_event(&events) {
        GitEvent::SnapshotUpdated { result, .. } => result.unwrap().generation,
        other => panic!("unexpected event: {other:?}"),
    };

    assert!(second_generation > first_generation);
}

#[test]
fn watcher_subscriptions_reference_count() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let _ = recv_event(&events);

    coordinator.subscribe(repo.path());
    coordinator.subscribe(repo.path());
    assert_eq!(coordinator.debug_scope_ref_count(repo.path()), 2);

    coordinator.unsubscribe(repo.path());
    assert_eq!(coordinator.debug_scope_ref_count(repo.path()), 1);

    coordinator.unsubscribe(repo.path());
    assert_eq!(coordinator.debug_scope_ref_count(repo.path()), 0);
}

#[test]
fn watch_dispatch_debounces_refreshes_per_scope() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let _ = recv_event(&events);

    coordinator.debug_inject_watch_event(repo.path());
    coordinator.debug_inject_watch_event(repo.path());
    coordinator.debug_inject_watch_event(repo.path());

    let event = recv_event(&events);
    match event {
        GitEvent::SnapshotUpdated { terminal_ids, .. } => {
            assert!(terminal_ids.is_empty());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn snapshot_request_preserves_concurrent_terminal_ids_for_same_scope() {
    let repo = init_repo();
    let _delay = set_snapshot_test_delay(Duration::from_millis(100));
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    coordinator.request_snapshot(repo.path(), Some("term-2"));

    let event = recv_event(&events);
    match event {
        GitEvent::SnapshotUpdated {
            terminal_ids,
            result,
            ..
        } => {
            assert_eq!(
                terminal_ids,
                vec!["term-1".to_string(), "term-2".to_string()]
            );
            assert!(result.is_ok());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn snapshot_request_on_non_repo_is_classified_not_repository() {
    let tempdir = TempDir::new().unwrap();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(tempdir.path(), Some("term-1"));

    let event = recv_event(&events);
    match event {
        GitEvent::SnapshotUpdated {
            terminal_ids,
            scope_root,
            result,
            ..
        } => {
            assert_eq!(terminal_ids, vec!["term-1".to_string()]);
            assert!(scope_root.is_none());
            let error = result.unwrap_err();
            assert_eq!(
                error.kind(),
                orcashell_git::SnapshotLoadErrorKind::NotRepository
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn repository_graph_request_publishes_loaded_event() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_repository_graph(repo.path(), 17);

    let event = recv_event(&events);
    match event {
        GitEvent::RepositoryGraphLoaded {
            scope_root,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(request_revision, 17);
            let graph = result.unwrap();
            assert!(!graph.commits.is_empty());
            assert!(!graph.local_branches.is_empty());
        }
        other => panic!("expected RepositoryGraphLoaded, got {other:?}"),
    }
}

#[test]
fn commit_detail_and_file_diff_requests_publish_loaded_events() {
    let repo = init_repo();
    fs::write(repo.path().join("tracked.txt"), "hello\nworld\n").unwrap();
    run_git(repo.path(), &["add", "tracked.txt"]);
    run_git(repo.path(), &["commit", "-m", "expand tracked"]);
    let commit_oid = head_oid(repo.path());

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_commit_detail(repo.path(), commit_oid, 23);
    let event = recv_event(&events);
    match event {
        GitEvent::CommitDetailLoaded {
            scope_root,
            oid,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(oid, commit_oid);
            assert_eq!(request_revision, 23);
            let detail = result.unwrap();
            assert_eq!(detail.oid, commit_oid);
            assert_eq!(detail.summary, "expand tracked");
            assert!(detail
                .changed_files
                .iter()
                .any(|file| file.path == PathBuf::from("tracked.txt")));
        }
        other => panic!("expected CommitDetailLoaded, got {other:?}"),
    }

    coordinator.request_commit_file_diff(repo.path(), commit_oid, PathBuf::from("tracked.txt"), 24);
    let event = recv_event(&events);
    match event {
        GitEvent::CommitFileDiffLoaded {
            scope_root,
            selection,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(selection.commit_oid, commit_oid);
            assert_eq!(selection.relative_path, PathBuf::from("tracked.txt"));
            assert_eq!(request_revision, 24);
            let diff = result.unwrap();
            assert!(diff.lines.iter().any(|line| line.text.contains("world")));
        }
        other => panic!("expected CommitFileDiffLoaded, got {other:?}"),
    }
}

#[test]
fn repo_browser_errors_propagate_without_blocking_existing_loads() {
    let repo = init_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    let missing_scope = repo.path().join("missing");
    let missing_commit = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let commit_oid = head_oid(repo.path());

    coordinator.request_repository_graph(&missing_scope, 30);
    let event = recv_event(&events);
    match event {
        GitEvent::RepositoryGraphLoaded {
            scope_root,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(&missing_scope));
            assert_eq!(request_revision, 30);
            assert!(result.is_err());
        }
        other => panic!("expected RepositoryGraphLoaded error, got {other:?}"),
    }

    coordinator.request_commit_detail(repo.path(), missing_commit, 31);
    let event = recv_event(&events);
    match event {
        GitEvent::CommitDetailLoaded {
            scope_root,
            oid,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(oid, missing_commit);
            assert_eq!(request_revision, 31);
            assert!(result.is_err());
        }
        other => panic!("expected CommitDetailLoaded error, got {other:?}"),
    }

    coordinator.request_commit_file_diff(repo.path(), commit_oid, PathBuf::from("missing.txt"), 32);
    let event = recv_event(&events);
    match event {
        GitEvent::CommitFileDiffLoaded {
            scope_root,
            selection,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(selection.commit_oid, commit_oid);
            assert_eq!(selection.relative_path, PathBuf::from("missing.txt"));
            assert_eq!(request_revision, 32);
            assert!(result.is_err());
        }
        other => panic!("expected CommitFileDiffLoaded error, got {other:?}"),
    }

    coordinator.request_diff_index(repo.path());
    let event = recv_event(&events);
    match event {
        GitEvent::DiffIndexLoaded { result, .. } => {
            assert!(result.is_ok());
        }
        other => panic!("expected DiffIndexLoaded after repo-browser errors, got {other:?}"),
    }

    coordinator.request_snapshot(repo.path(), Some("term-1"));
    let event = recv_event(&events);
    match event {
        GitEvent::SnapshotUpdated { result, .. } => {
            assert!(result.is_ok());
        }
        other => panic!("expected SnapshotUpdated after repo-browser errors, got {other:?}"),
    }
}

#[test]
fn repo_browser_worker_does_not_block_diff_worker() {
    let repo = init_repo();
    let _delay = set_repo_browser_test_delay(Duration::from_millis(80));
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_repository_graph(repo.path(), 1);
    coordinator.request_diff_index(repo.path());

    let first = recv_event(&events);
    assert!(
        matches!(first, GitEvent::DiffIndexLoaded { .. }),
        "expected DiffIndexLoaded before repo-browser event, got {first:?}"
    );

    let second = recv_event(&events);
    assert!(
        matches!(second, GitEvent::RepositoryGraphLoaded { .. }),
        "expected RepositoryGraphLoaded after diff load, got {second:?}"
    );
}

#[test]
fn repo_browser_commit_detail_does_not_block_diff_worker() {
    let repo = init_repo();
    let _delay = set_repo_browser_test_delay(Duration::from_millis(80));
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    let commit_oid = head_oid(repo.path());

    coordinator.request_commit_detail(repo.path(), commit_oid, 1);
    coordinator.request_diff_index(repo.path());

    let first = recv_event(&events);
    assert!(
        matches!(first, GitEvent::DiffIndexLoaded { .. }),
        "expected DiffIndexLoaded before CommitDetailLoaded, got {first:?}"
    );

    let second = recv_event(&events);
    assert!(
        matches!(second, GitEvent::CommitDetailLoaded { .. }),
        "expected CommitDetailLoaded after diff load, got {second:?}"
    );
}

#[test]
fn repo_browser_commit_file_diff_does_not_block_diff_worker() {
    let repo = init_repo();
    let _delay = set_repo_browser_test_delay(Duration::from_millis(80));
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    let commit_oid = head_oid(repo.path());

    coordinator.request_commit_file_diff(repo.path(), commit_oid, PathBuf::from("tracked.txt"), 1);
    coordinator.request_diff_index(repo.path());

    let first = recv_event(&events);
    assert!(
        matches!(first, GitEvent::DiffIndexLoaded { .. }),
        "expected DiffIndexLoaded before CommitFileDiffLoaded, got {first:?}"
    );

    let second = recv_event(&events);
    assert!(
        matches!(second, GitEvent::CommitFileDiffLoaded { .. }),
        "expected CommitFileDiffLoaded after diff load, got {second:?}"
    );
}

#[test]
fn stash_read_requests_publish_loaded_events() {
    let repo = init_repo();
    fs::write(repo.path().join("tracked.txt"), "hello\nworld\n").unwrap();
    fs::write(repo.path().join("notes.txt"), "draft\n").unwrap();
    run_git(repo.path(), &["stash", "push", "-u", "-m", "preview stash"]);
    let stash_oid = list_stashes(repo.path()).unwrap().entries[0].stash_oid;

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_stash_list(repo.path(), 41);
    match recv_event(&events) {
        GitEvent::StashListLoaded {
            scope_root,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(request_revision, 41);
            let list = result.unwrap();
            assert_eq!(list.entries.len(), 1);
            assert_eq!(list.entries[0].stash_oid, stash_oid);
        }
        other => panic!("expected StashListLoaded, got {other:?}"),
    }

    coordinator.request_stash_detail(repo.path(), stash_oid, 42);
    match recv_event(&events) {
        GitEvent::StashDetailLoaded {
            scope_root,
            stash_oid: loaded_oid,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(loaded_oid, stash_oid);
            assert_eq!(request_revision, 42);
            let detail = result.unwrap();
            assert!(detail
                .files
                .iter()
                .any(|file| file.relative_path == PathBuf::from("tracked.txt")));
        }
        other => panic!("expected StashDetailLoaded, got {other:?}"),
    }

    coordinator.request_stash_file_diff(repo.path(), stash_oid, PathBuf::from("tracked.txt"), 43);
    match recv_event(&events) {
        GitEvent::StashFileDiffLoaded {
            scope_root,
            selection,
            request_revision,
            result,
        } => {
            assert_eq!(scope_root, normalize_path(repo.path()));
            assert_eq!(selection.stash_oid, stash_oid);
            assert_eq!(selection.relative_path, PathBuf::from("tracked.txt"));
            assert_eq!(request_revision, 43);
            let diff = result.unwrap();
            assert!(diff.lines.iter().any(|line| line.text.contains("world")));
        }
        other => panic!("expected StashFileDiffLoaded, got {other:?}"),
    }
}

#[test]
fn create_and_drop_stash_emit_completion_and_refresh_list() {
    let repo = init_repo();
    fs::write(repo.path().join("tracked.txt"), "hello\nworld\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.create_stash(repo.path(), Some("ui stash".to_string()), false, false);

    let mut saw_create_completion = false;
    let mut saw_list_refresh = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::CreateStash);
                assert!(result.is_ok());
                saw_create_completion = true;
            }
            GitEvent::StashListLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_STASH_LIST_REFRESH_REVISION);
                assert_eq!(result.unwrap().entries.len(), 1);
                saw_list_refresh = true;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected event after create_stash: {other:?}"),
        }
        if saw_create_completion && saw_list_refresh {
            break;
        }
    }
    assert!(saw_create_completion && saw_list_refresh);

    let stash_oid = list_stashes(repo.path()).unwrap().entries[0].stash_oid;
    coordinator.drop_stash(repo.path(), stash_oid);

    let mut saw_drop_completion = false;
    let mut saw_empty_refresh = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::DropStash);
                assert!(result.is_ok());
                saw_drop_completion = true;
            }
            GitEvent::StashListLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_STASH_LIST_REFRESH_REVISION);
                assert!(result.unwrap().entries.is_empty());
                saw_empty_refresh = true;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected event after drop_stash: {other:?}"),
        }
        if saw_drop_completion && saw_empty_refresh {
            break;
        }
    }
    assert!(saw_drop_completion && saw_empty_refresh);
}

#[test]
fn apply_stash_conflict_emits_merge_conflict_event() {
    let repo = init_repo_with_contents("a\nb\nc\n");
    fs::write(repo.path().join("tracked.txt"), "a\nstash\nc\n").unwrap();
    run_git(repo.path(), &["stash", "push", "-m", "conflict"]);
    fs::write(repo.path().join("tracked.txt"), "a\nhead\nc\n").unwrap();
    run_git(repo.path(), &["add", "tracked.txt"]);
    run_git(repo.path(), &["commit", "-m", "head change"]);
    let stash_oid = list_stashes(repo.path()).unwrap().entries[0].stash_oid;

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.apply_stash(repo.path(), stash_oid);

    let mut saw_conflict = false;
    let mut saw_completion = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::MergeConflictEntered {
                request_scope,
                affected_scope,
                conflicted_files,
                trigger,
            } => {
                assert_eq!(request_scope, normalize_path(repo.path()));
                assert_eq!(affected_scope, normalize_path(repo.path()));
                assert_eq!(conflicted_files, vec![PathBuf::from("tracked.txt")]);
                assert_eq!(trigger, MergeConflictTrigger::StashApply);
                saw_conflict = true;
            }
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::ApplyStash);
                let message = result.unwrap();
                assert!(message.starts_with("CONFLICT: "));
                saw_completion = true;
            }
            GitEvent::StashListLoaded { .. } | GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected apply_stash event: {other:?}"),
        }
        if saw_conflict && saw_completion {
            break;
        }
    }
    assert!(saw_conflict && saw_completion);
}

#[test]
fn fetch_success_triggers_graph_refresh() {
    let (tempdir, client_dir) = setup_tracking_repo();
    let bare_dir = tempdir.path().join("bare.git");
    create_remote_branch(&tempdir, &bare_dir, "feature");

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.fetch_repo(&client_dir, GitFetchOrigin::Manual));

    let mut saw_completion = false;
    let mut saw_graph_refresh = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::RemoteOpCompleted {
                kind,
                fetch_origin,
                refresh_graph,
                result,
                ..
            } => {
                assert_eq!(kind, GitRemoteKind::Fetch);
                assert_eq!(fetch_origin, Some(GitFetchOrigin::Manual));
                assert!(refresh_graph);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(graph
                    .remote_branches
                    .iter()
                    .any(|branch| branch.full_ref == "refs/remotes/origin/feature"));
                saw_graph_refresh = true;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected event while waiting for fetch refresh: {other:?}"),
        }

        if saw_completion && saw_graph_refresh {
            return;
        }
    }

    panic!("did not observe fetch completion plus graph refresh");
}

#[test]
fn fetch_failure_surfaces_error() {
    let (_tempdir, client_dir) = setup_tracking_repo();
    let bad_remote_path = client_dir.join("../missing-remote.git");
    run_git(
        &client_dir,
        &[
            "remote",
            "set-url",
            "origin",
            bad_remote_path.to_str().unwrap(),
        ],
    );

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.fetch_repo(&client_dir, GitFetchOrigin::Manual));
    loop {
        match recv_event(&events) {
            GitEvent::RemoteOpCompleted {
                kind,
                fetch_origin,
                refresh_graph,
                result,
                ..
            } => {
                assert_eq!(kind, GitRemoteKind::Fetch);
                assert_eq!(fetch_origin, Some(GitFetchOrigin::Manual));
                assert!(!refresh_graph);
                assert!(result.is_err());
                let message = result.unwrap_err();
                assert!(message.contains("Remote not found"));
                return;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected fetch-failure event: {other:?}"),
        }
    }
}

#[test]
fn pull_success_triggers_graph_refresh() {
    let (tempdir, client_dir) = setup_tracking_repo();
    let bare_dir = tempdir.path().join("bare.git");
    let other_dir = tempdir.path().join("other");

    run_git(
        tempdir.path(),
        &[
            "clone",
            bare_dir.to_str().unwrap(),
            other_dir.to_str().unwrap(),
        ],
    );
    run_git(&other_dir, &["config", "user.name", "Orca"]);
    run_git(&other_dir, &["config", "user.email", "orca@example.com"]);
    fs::write(other_dir.join("fresh.txt"), "remote update\n").unwrap();
    run_git(&other_dir, &["add", "fresh.txt"]);
    run_git(&other_dir, &["commit", "-m", "remote update"]);
    let branch = run_git_capture(&other_dir, &["branch", "--show-current"]);
    run_git(
        &other_dir,
        &[
            "push",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    );

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    coordinator.pull_current_branch(&client_dir);

    let mut saw_completion = false;
    let mut saw_graph_refresh = false;
    for _ in 0..8 {
        match recv_event(&events) {
            GitEvent::RemoteOpCompleted {
                kind,
                refresh_graph,
                result,
                ..
            } => {
                assert_eq!(kind, GitRemoteKind::Pull);
                assert!(refresh_graph);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(matches!(graph.head, HeadState::Branch { .. }));
                saw_graph_refresh = true;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected event while waiting for pull refresh: {other:?}"),
        }

        if saw_completion && saw_graph_refresh {
            return;
        }
    }

    panic!("did not observe pull completion plus graph refresh");
}

#[test]
fn second_fetch_is_rejected_while_first_is_starting() {
    let (_tempdir, client_dir) = setup_tracking_repo();
    let coordinator = GitCoordinator::new();

    assert!(coordinator.fetch_repo(&client_dir, GitFetchOrigin::Manual));
    assert!(!coordinator.fetch_repo(&client_dir, GitFetchOrigin::Manual));
}

#[test]
fn up_to_date_fetch_skips_graph_refresh() {
    let (_tempdir, client_dir) = setup_tracking_repo();
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.fetch_repo(&client_dir, GitFetchOrigin::Automatic));

    match recv_event(&events) {
        GitEvent::RemoteOpCompleted {
            kind,
            fetch_origin,
            refresh_graph,
            result,
            ..
        } => {
            assert_eq!(kind, GitRemoteKind::Fetch);
            assert_eq!(fetch_origin, Some(GitFetchOrigin::Automatic));
            assert!(!refresh_graph);
            assert_eq!(result.unwrap(), "Remote refs already up to date");
        }
        other => panic!("expected RemoteOpCompleted, got {other:?}"),
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        events.try_recv().is_err(),
        "expected no graph refresh or snapshot event after up-to-date fetch"
    );
}

// ── CP2: Local action tests ──────────────────────────────────────

#[test]
fn stage_paths_emits_local_action_completed() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "new\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime scope via snapshot
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.stage_paths(repo.path(), vec![PathBuf::from("new.txt")]);

    // Should get LocalActionCompleted then SnapshotUpdated (from refresh)
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, result, .. } => {
            assert_eq!(action, GitActionKind::Stage);
            assert!(result.is_ok());
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }
}

#[test]
fn commit_emits_local_action_completed() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "new\n").unwrap();
    run_git(repo.path(), &["add", "new.txt"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.commit_staged(repo.path(), "test commit".to_string());

    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, result, .. } => {
            assert_eq!(action, GitActionKind::Commit);
            assert!(result.is_ok());
            assert!(result.unwrap().starts_with("Committed"));
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }
}

#[test]
fn checkout_local_branch_refreshes_snapshot_and_graph() {
    let repo = init_repo();
    let main_branch = run_git_capture(repo.path(), &["branch", "--show-current"]);
    run_git(repo.path(), &["branch", "feature"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.checkout_local_branch(repo.path(), "feature".to_string()));

    let mut saw_completion = false;
    let mut saw_snapshot = false;
    let mut saw_graph = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::CheckoutLocalBranch);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::SnapshotUpdated { result, .. } => {
                let snapshot = result.unwrap();
                assert_eq!(snapshot.branch_name, "feature");
                saw_snapshot = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(
                    matches!(graph.head, orcashell_git::HeadState::Branch { name, .. } if name == "feature")
                );
                saw_graph = true;
            }
            other => panic!("unexpected checkout-local event: {other:?}"),
        }
        if saw_completion && saw_snapshot && saw_graph {
            break;
        }
    }

    assert!(saw_completion && saw_snapshot && saw_graph);
    assert_eq!(
        run_git_capture(repo.path(), &["branch", "--show-current"]),
        "feature"
    );
    assert_ne!(main_branch, "feature");
}

#[test]
fn second_checkout_is_rejected_while_first_is_starting() {
    let repo = init_repo();
    run_git(repo.path(), &["branch", "feature"]);
    let coordinator = GitCoordinator::new();

    assert!(coordinator.checkout_local_branch(repo.path(), "feature".to_string()));
    assert!(!coordinator.checkout_local_branch(repo.path(), "feature".to_string()));
}

#[test]
fn checkout_local_branch_blocked_result_preserves_head() {
    let repo = init_repo();
    let main_branch = run_git_capture(repo.path(), &["branch", "--show-current"]);
    run_git(repo.path(), &["branch", "feature"]);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.checkout_local_branch(repo.path(), "feature".to_string()));

    loop {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::CheckoutLocalBranch);
                assert!(result.is_ok());
                assert!(result.unwrap().contains("BLOCKED:"));
                break;
            }
            GitEvent::SnapshotUpdated { .. } => {}
            other => panic!("unexpected blocked-checkout event: {other:?}"),
        }
    }

    assert_eq!(
        run_git_capture(repo.path(), &["branch", "--show-current"]),
        main_branch
    );
}

#[test]
fn checkout_remote_branch_refreshes_snapshot_and_graph() {
    let (tempdir, client_dir) = setup_tracking_repo();
    let bare_dir = tempdir.path().join("bare.git");
    create_remote_branch(&tempdir, &bare_dir, "feature");
    run_git(&client_dir, &["fetch", "origin", "feature"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    assert!(
        coordinator.checkout_remote_branch(&client_dir, "refs/remotes/origin/feature".to_string())
    );

    let mut saw_completion = false;
    let mut saw_snapshot = false;
    let mut saw_graph = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::CheckoutRemoteBranch);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::SnapshotUpdated { result, .. } => {
                let snapshot = result.unwrap();
                assert_eq!(snapshot.branch_name, "feature");
                saw_snapshot = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(graph
                    .local_branches
                    .iter()
                    .any(|branch| branch.name == "feature" && branch.is_head));
                saw_graph = true;
            }
            other => panic!("unexpected checkout-remote event: {other:?}"),
        }

        if saw_completion && saw_snapshot && saw_graph {
            break;
        }
    }

    assert!(saw_completion && saw_snapshot && saw_graph);
    assert_eq!(
        run_git_capture(&client_dir, &["branch", "--show-current"]),
        "feature"
    );
}

#[test]
fn create_local_branch_refreshes_snapshot_and_graph() {
    let repo = init_repo();
    run_git(repo.path(), &["branch", "feature"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.create_local_branch(
        repo.path(),
        "feature".to_string(),
        "review/feature".to_string(),
    ));

    let mut saw_completion = false;
    let mut saw_snapshot = false;
    let mut saw_graph = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::CreateLocalBranch);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::SnapshotUpdated { result, .. } => {
                let snapshot = result.unwrap();
                assert_eq!(snapshot.branch_name, "review/feature");
                saw_snapshot = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(matches!(
                    graph.head,
                    orcashell_git::HeadState::Branch { name, .. } if name == "review/feature"
                ));
                saw_graph = true;
            }
            other => panic!("unexpected create-local-branch event: {other:?}"),
        }
        if saw_completion && saw_snapshot && saw_graph {
            break;
        }
    }

    assert!(saw_completion && saw_snapshot && saw_graph);
    assert_eq!(
        run_git_capture(repo.path(), &["branch", "--show-current"]),
        "review/feature"
    );
}

#[test]
fn delete_local_branch_refreshes_snapshot_and_graph() {
    let repo = init_repo();
    let main_branch = run_git_capture(repo.path(), &["branch", "--show-current"]);
    run_git(repo.path(), &["branch", "feature"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    assert!(coordinator.delete_local_branch(repo.path(), "feature".to_string()));

    let mut saw_completion = false;
    let mut saw_snapshot = false;
    let mut saw_graph = false;
    for _ in 0..6 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::DeleteLocalBranch);
                assert!(result.is_ok());
                saw_completion = true;
            }
            GitEvent::SnapshotUpdated { result, .. } => {
                let snapshot = result.unwrap();
                assert_eq!(snapshot.branch_name, main_branch);
                saw_snapshot = true;
            }
            GitEvent::RepositoryGraphLoaded {
                request_revision,
                result,
                ..
            } => {
                assert_eq!(request_revision, AUTO_REPOSITORY_GRAPH_REFRESH_REVISION);
                let graph = result.unwrap();
                assert!(!graph
                    .local_branches
                    .iter()
                    .any(|branch| branch.name == "feature"));
                saw_graph = true;
            }
            other => panic!("unexpected delete-local-branch event: {other:?}"),
        }
        if saw_completion && saw_snapshot && saw_graph {
            break;
        }
    }

    assert!(saw_completion && saw_snapshot && saw_graph);
}

#[test]
fn pull_conflict_emits_merge_conflict_entered_before_remote_completion() {
    let (tempdir, client_dir) = setup_tracking_repo();
    let bare_dir = tempdir.path().join("bare.git");
    let other_dir = tempdir.path().join("other");

    run_git(
        tempdir.path(),
        &[
            "clone",
            bare_dir.to_str().unwrap(),
            other_dir.to_str().unwrap(),
        ],
    );
    run_git(&other_dir, &["config", "user.name", "Orca"]);
    run_git(&other_dir, &["config", "user.email", "orca@example.com"]);
    fs::write(other_dir.join("file.txt"), "remote conflict\n").unwrap();
    run_git(&other_dir, &["add", "file.txt"]);
    run_git(&other_dir, &["commit", "-m", "remote conflict"]);
    let branch = run_git_capture(&other_dir, &["branch", "--show-current"]);
    run_git(
        &other_dir,
        &[
            "push",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    );

    fs::write(client_dir.join("file.txt"), "local conflict\n").unwrap();
    run_git(&client_dir, &["add", "file.txt"]);
    run_git(&client_dir, &["commit", "-m", "local conflict"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    coordinator.pull_current_branch(&client_dir);

    match recv_event(&events) {
        GitEvent::MergeConflictEntered {
            request_scope,
            affected_scope,
            conflicted_files,
            trigger,
        } => {
            let expected = fs::canonicalize(&client_dir).unwrap();
            assert_eq!(request_scope, expected);
            assert_eq!(affected_scope, expected);
            assert_eq!(conflicted_files, vec![PathBuf::from("file.txt")]);
            assert_eq!(trigger, MergeConflictTrigger::Pull);
        }
        other => panic!("expected MergeConflictEntered, got {other:?}"),
    }

    match recv_event(&events) {
        GitEvent::RemoteOpCompleted { kind, result, .. } => {
            assert_eq!(kind, GitRemoteKind::Pull);
            assert!(result.is_ok());
            assert!(result.unwrap().contains("CONFLICT:"));
        }
        other => panic!("expected RemoteOpCompleted, got {other:?}"),
    }
}

#[test]
fn merge_back_conflict_emits_source_scope_handoff_event() {
    let repo_dir = init_repo();
    let source_ref = run_git_capture(repo_dir.path(), &["symbolic-ref", "HEAD"]);
    let managed = create_managed_worktree(repo_dir.path(), "wt-ab123456").unwrap();

    fs::write(managed.path.join("tracked.txt"), "managed conflict\n").unwrap();
    run_git(&managed.path, &["add", "tracked.txt"]);
    run_git(&managed.path, &["commit", "-m", "managed conflict"]);

    fs::write(repo_dir.path().join("tracked.txt"), "source conflict\n").unwrap();
    run_git(repo_dir.path(), &["add", "tracked.txt"]);
    run_git(repo_dir.path(), &["commit", "-m", "source conflict"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&managed.path, Some("t1"));
    let _ = recv_event(&events);

    coordinator.merge_managed_branch(&managed.path, source_ref);

    match recv_event(&events) {
        GitEvent::MergeConflictEntered {
            request_scope,
            affected_scope,
            conflicted_files,
            trigger,
        } => {
            assert_eq!(request_scope, fs::canonicalize(&managed.path).unwrap());
            assert_eq!(affected_scope, fs::canonicalize(repo_dir.path()).unwrap());
            assert_eq!(conflicted_files, vec![PathBuf::from("tracked.txt")]);
            assert_eq!(trigger, MergeConflictTrigger::MergeBack);
        }
        other => panic!("expected MergeConflictEntered, got {other:?}"),
    }

    match recv_event(&events) {
        GitEvent::LocalActionCompleted { action, result, .. } => {
            assert_eq!(action, GitActionKind::MergeBack);
            assert!(result.is_ok());
            assert!(result.unwrap().contains("CONFLICT:"));
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }
}

#[test]
fn abort_merge_action_completes_after_pull_conflict() {
    let (tempdir, client_dir) = setup_tracking_repo();
    let bare_dir = tempdir.path().join("bare.git");
    let other_dir = tempdir.path().join("other");

    run_git(
        tempdir.path(),
        &[
            "clone",
            bare_dir.to_str().unwrap(),
            other_dir.to_str().unwrap(),
        ],
    );
    run_git(&other_dir, &["config", "user.name", "Orca"]);
    run_git(&other_dir, &["config", "user.email", "orca@example.com"]);
    fs::write(other_dir.join("file.txt"), "remote conflict\n").unwrap();
    run_git(&other_dir, &["add", "file.txt"]);
    run_git(&other_dir, &["commit", "-m", "remote conflict"]);
    let branch = run_git_capture(&other_dir, &["branch", "--show-current"]);
    run_git(
        &other_dir,
        &[
            "push",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    );

    fs::write(client_dir.join("file.txt"), "local conflict\n").unwrap();
    run_git(&client_dir, &["add", "file.txt"]);
    run_git(&client_dir, &["commit", "-m", "local conflict"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();
    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    coordinator.pull_current_branch(&client_dir);
    let _ = recv_event(&events); // MergeConflictEntered
    let _ = recv_event(&events); // RemoteOpCompleted

    coordinator.abort_merge(&client_dir);

    for _ in 0..4 {
        match recv_event(&events) {
            GitEvent::LocalActionCompleted { action, result, .. } => {
                assert_eq!(action, GitActionKind::AbortMerge);
                assert_eq!(result.unwrap(), "Merge aborted");
                return;
            }
            GitEvent::SnapshotUpdated { .. } | GitEvent::RepositoryGraphLoaded { .. } => {}
            other => panic!("expected abort-related event, got {other:?}"),
        }
    }
    panic!("did not receive AbortMerge completion event");
}

#[test]
fn local_action_triggers_snapshot_refresh() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "new\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.stage_paths(repo.path(), vec![PathBuf::from("new.txt")]);

    // First: LocalActionCompleted
    let event = recv_event(&events);
    assert!(matches!(event, GitEvent::LocalActionCompleted { .. }));

    // Second: SnapshotUpdated (from the auto-refresh)
    let event = recv_event(&events);
    assert!(matches!(event, GitEvent::SnapshotUpdated { .. }));
}

#[test]
fn duplicate_local_action_is_suppressed() {
    let repo = init_repo();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    fs::write(repo.path().join("b.txt"), "b\n").unwrap();

    let _delay = set_snapshot_test_delay(Duration::from_millis(50));
    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime scope
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    // Send two stage requests rapidly. Second should be suppressed.
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("a.txt")]);
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("b.txt")]);

    // Should get exactly one LocalActionCompleted (for a.txt)
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, result, .. } => {
            assert_eq!(action, GitActionKind::Stage);
            assert!(result.is_ok());
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }

    // The snapshot refresh follows. No second LocalActionCompleted.
    let event = recv_event(&events);
    assert!(
        matches!(event, GitEvent::SnapshotUpdated { .. }),
        "expected SnapshotUpdated after action, got {event:?}"
    );
}

#[test]
fn local_action_suppression_clears_after_completion() {
    let repo = init_repo();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    fs::write(repo.path().join("b.txt"), "b\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    // First action
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("a.txt")]);
    let _ = recv_event(&events); // LocalActionCompleted
    let _ = recv_event(&events); // SnapshotUpdated

    // Second action should succeed (suppression cleared)
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("b.txt")]);
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, result, .. } => {
            assert_eq!(action, GitActionKind::Stage);
            assert!(result.is_ok());
        }
        other => panic!("expected second LocalActionCompleted, got {other:?}"),
    }
}

// ── CP4: Scope-wide exclusion tests ──────────────────────────────

#[test]
fn scope_wide_exclusion_local_blocks_when_remote_in_flight() {
    let repo = init_repo();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime scope.
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    // Manually set remote_op_in_flight for the scope.
    {
        let mut state = coordinator.inner.state.lock();
        let scope = state.scopes.entry(normalize_path(repo.path())).or_default();
        scope.remote_op_in_flight = true;
    }

    // Local action should be suppressed.
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("a.txt")]);
    // No event should come from stage (it was suppressed). Clear the flag.
    {
        let mut state = coordinator.inner.state.lock();
        let scope = state.scopes.get_mut(&normalize_path(repo.path())).unwrap();
        scope.remote_op_in_flight = false;
    }

    // Now local action should succeed.
    coordinator.stage_paths(repo.path(), vec![PathBuf::from("a.txt")]);
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, .. } => {
            assert_eq!(action, GitActionKind::Stage);
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }
}

#[test]
fn scope_wide_exclusion_remote_blocks_when_local_in_flight() {
    let repo = init_repo();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime scope.
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    // Manually set local_action_in_flight.
    {
        let mut state = coordinator.inner.state.lock();
        let scope = state.scopes.entry(normalize_path(repo.path())).or_default();
        scope.local_action_in_flight = true;
    }

    // Remote op should be suppressed.
    coordinator.push_current_branch(repo.path());
    // No RemoteOpCompleted should come. Clear the flag.
    {
        let mut state = coordinator.inner.state.lock();
        let scope = state.scopes.get_mut(&normalize_path(repo.path())).unwrap();
        scope.local_action_in_flight = false;
    }

    // Remote op should now succeed (will error because no upstream, but it should start).
    coordinator.push_current_branch(repo.path());
    let event = recv_event(&events);
    match event {
        GitEvent::RemoteOpCompleted { kind, .. } => {
            assert_eq!(kind, GitRemoteKind::Push);
        }
        other => panic!("expected RemoteOpCompleted, got {other:?}"),
    }
}

#[test]
fn automatic_fetch_allows_local_action_to_start() {
    let repo = init_repo();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    {
        let mut state = coordinator.inner.state.lock();
        let scope = state.scopes.entry(normalize_path(repo.path())).or_default();
        scope.remote_op_in_flight = true;
        scope.current_remote_op_kind = Some(GitRemoteKind::Fetch);
        scope.current_fetch_origin = Some(GitFetchOrigin::Automatic);
    }

    coordinator.stage_paths(repo.path(), vec![PathBuf::from("a.txt")]);
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted { action, .. } => {
            assert_eq!(action, GitActionKind::Stage);
        }
        other => panic!("expected LocalActionCompleted, got {other:?}"),
    }
}

#[test]
fn cross_scope_concurrency_allowed() {
    let repo_a = init_repo();
    let repo_b = init_repo();
    fs::write(repo_a.path().join("a.txt"), "a\n").unwrap();
    fs::write(repo_b.path().join("b.txt"), "b\n").unwrap();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime both scopes.
    coordinator.request_snapshot(repo_a.path(), Some("t1"));
    let _ = recv_event(&events);
    coordinator.request_snapshot(repo_b.path(), Some("t2"));
    let _ = recv_event(&events);

    // Set remote in flight for scope A.
    {
        let mut state = coordinator.inner.state.lock();
        let scope_a = state
            .scopes
            .entry(normalize_path(repo_a.path()))
            .or_default();
        scope_a.remote_op_in_flight = true;
    }

    // Local action on scope B should still succeed.
    coordinator.stage_paths(repo_b.path(), vec![PathBuf::from("b.txt")]);
    let event = recv_event(&events);
    match event {
        GitEvent::LocalActionCompleted {
            action, scope_root, ..
        } => {
            assert_eq!(action, GitActionKind::Stage);
            assert_eq!(scope_root, normalize_path(repo_b.path()));
        }
        other => panic!("expected LocalActionCompleted for scope B, got {other:?}"),
    }
}

#[test]
fn push_no_upstream_returns_error() {
    let repo = init_repo();

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    // Prime scope.
    coordinator.request_snapshot(repo.path(), Some("t1"));
    let _ = recv_event(&events);

    coordinator.push_current_branch(repo.path());
    let event = recv_event(&events);
    match event {
        GitEvent::RemoteOpCompleted { kind, result, .. } => {
            assert_eq!(kind, GitRemoteKind::Push);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("No upstream configured"));
        }
        other => panic!("expected RemoteOpCompleted, got {other:?}"),
    }
}

#[test]
fn publish_current_branch_sets_upstream_and_remote_branch() {
    let (_tempdir, client_dir) = setup_tracking_repo();
    run_git(&client_dir, &["checkout", "-b", "feature/publish"]);
    fs::write(client_dir.join("publish.txt"), "publish\n").unwrap();
    run_git(&client_dir, &["add", "publish.txt"]);
    run_git(&client_dir, &["commit", "-m", "publish branch"]);

    let coordinator = GitCoordinator::new();
    let events = coordinator.subscribe_events();

    coordinator.request_snapshot(&client_dir, Some("t1"));
    let _ = recv_event(&events);

    coordinator.publish_current_branch(&client_dir, "origin".to_string());
    let event = recv_event(&events);
    match event {
        GitEvent::RemoteOpCompleted { kind, result, .. } => {
            assert_eq!(kind, GitRemoteKind::Publish);
            assert!(result.is_ok(), "publish failed: {:?}", result);
        }
        other => panic!("expected RemoteOpCompleted, got {other:?}"),
    }

    assert_eq!(
        run_git_capture(
            &client_dir,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}"
            ],
        ),
        "origin/feature/publish"
    );

    let heads = run_git_capture(
        &client_dir,
        &["ls-remote", "--heads", "origin", "feature/publish"],
    );
    assert!(heads.contains("refs/heads/feature/publish"));
}

#[test]
fn classify_remote_error_identifies_auth_failure() {
    let msg = classify_remote_error("fatal: could not read from remote repository.");
    assert!(msg.contains("Authentication failed"));
}

#[test]
fn classify_remote_error_identifies_network_error() {
    let msg = classify_remote_error("fatal: unable to access 'https://example.com/repo.git/'");
    assert!(msg.contains("Network error"));
}

#[test]
fn classify_remote_error_identifies_rejected_push() {
    let msg = classify_remote_error("! [rejected]        main -> main (non-fast-forward)");
    assert!(msg.contains("Push rejected"));
}

#[test]
fn classify_remote_error_fallback() {
    let msg = classify_remote_error("some unknown git error");
    assert_eq!(msg, "some unknown git error");
}
