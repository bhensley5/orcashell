use super::*;
use git2::{IndexAddOption, Signature};
use std::process::Command;
use tempfile::TempDir;

fn repo_fixture() -> (TempDir, Repository) {
    let tempdir = TempDir::new().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    configure_line_endings(&repo);
    (tempdir, repo)
}

fn signature() -> Signature<'static> {
    Signature::now("OrcaShell", "orca@example.com").unwrap()
}

fn commit_all(repo: &Repository, message: &str) -> Oid {
    let sig = signature();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();

    let parent_commit = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok());

    match parent_commit {
        Some(parent) => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
            .unwrap(),
        None => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
            .unwrap(),
    }
}

fn write_file(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn read_text_file_normalized(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn hunk_target(document: &FileDiffDocument, hunk_index: usize) -> DiscardHunkTarget {
    document
        .discard_hunk_target(hunk_index)
        .expect("expected hunk target to exist")
}

fn numbered_lines(count: usize) -> String {
    (1..=count).map(|index| format!("{index}\n")).collect()
}

fn split_gitdir_repo_fixture() -> (TempDir, PathBuf, PathBuf, Repository) {
    let tempdir = TempDir::new().unwrap();
    let worktree_path = tempdir.path().join("checkout");
    let admin_dir = tempdir.path().join("admin.git");
    let repo = Repository::init(&worktree_path).unwrap();
    drop(repo);

    fs::rename(worktree_path.join(".git"), &admin_dir).unwrap();
    fs::write(
        worktree_path.join(".git"),
        format!("gitdir: {}\n", admin_dir.display()),
    )
    .unwrap();

    let admin_repo = Repository::open(&admin_dir).unwrap();
    admin_repo
        .config()
        .unwrap()
        .set_str(
            "core.worktree",
            worktree_path.canonicalize().unwrap().to_str().unwrap(),
        )
        .unwrap();
    configure_line_endings(&admin_repo);
    let repo = Repository::open(&worktree_path).unwrap();
    (tempdir, worktree_path, admin_dir, repo)
}

fn current_branch_name(repo: &Repository) -> String {
    repo.head().unwrap().shorthand().map(str::to_owned).unwrap()
}

fn current_head_ref(repo: &Repository) -> String {
    repo.head().unwrap().name().map(str::to_owned).unwrap()
}

/// Helper to configure git identity for test repos.
fn configure_identity(repo: &Repository) {
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "OrcaShell").unwrap();
    config.set_str("user.email", "orca@example.com").unwrap();
}

fn configure_line_endings(repo: &Repository) {
    let mut config = repo.config().unwrap();
    config.set_str("core.autocrlf", "false").unwrap();
    config.set_str("core.eol", "lf").unwrap();
}

fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn head_oid(repo: &Repository) -> Oid {
    repo.head().unwrap().target().unwrap()
}

fn synthetic_oid(value: u64) -> Oid {
    Oid::from_str(&format!("{value:040x}")).unwrap()
}

fn graph_node(oid: Oid, parent_oids: Vec<Oid>) -> CommitGraphNode {
    CommitGraphNode {
        oid,
        short_oid: short_oid(oid),
        summary: String::new(),
        author_name: String::new(),
        authored_at_unix: 0,
        parent_oids,
        primary_lane: 0,
        row_lanes: Vec::new(),
        ref_labels: Vec::new(),
    }
}

fn merge_repo_fixture() -> (TempDir, Repository, Oid) {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);

    write_file(&tempdir.path().join("base.txt"), "base\n");
    commit_all(&repo, "initial");
    let main_branch = current_branch_name(&repo);

    let checkout_feature = run_git(tempdir.path(), &["checkout", "-b", "feature"]);
    assert!(
        checkout_feature.status.success(),
        "git checkout -b feature failed: {}",
        String::from_utf8_lossy(&checkout_feature.stderr)
    );
    write_file(&tempdir.path().join("feature.txt"), "feature\n");
    commit_all(&repo, "feature commit");

    let checkout_main = run_git(tempdir.path(), &["checkout", &main_branch]);
    assert!(
        checkout_main.status.success(),
        "git checkout {main_branch} failed: {}",
        String::from_utf8_lossy(&checkout_main.stderr)
    );
    write_file(&tempdir.path().join("main.txt"), "main\n");
    commit_all(&repo, "main commit");

    let merge = run_git(
        tempdir.path(),
        &["merge", "--no-ff", "feature", "-m", "merge feature"],
    );
    assert!(
        merge.status.success(),
        "git merge failed: {}",
        String::from_utf8_lossy(&merge.stderr)
    );

    let merge_oid = head_oid(&repo);
    (tempdir, repo, merge_oid)
}

fn stash_oid_at(path: &Path, index: usize) -> Oid {
    list_stashes(path).unwrap().entries[index].stash_oid
}

// ── Discovery tests ──────────────────────────────────────────────

#[test]
fn discovers_repo_from_nested_directory() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("src/lib.rs"), "fn main() {}\n");
    commit_all(&repo, "initial");

    let nested = tempdir.path().join("src");
    let scope = discover_scope(&nested).unwrap();
    assert_eq!(scope.repo_root, tempdir.path().canonicalize().unwrap());
    assert_eq!(scope.scope_root, tempdir.path().canonicalize().unwrap());
    assert!(!scope.is_worktree);
    assert_eq!(scope.worktree_name, None);
}

#[test]
fn discovers_split_gitdir_repo_from_checkout_root() {
    let (_tempdir, worktree_path, _admin_dir, repo) = split_gitdir_repo_fixture();
    write_file(&worktree_path.join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let scope = discover_scope(&worktree_path).unwrap();
    assert_eq!(scope.repo_root, worktree_path.canonicalize().unwrap());
    assert_eq!(scope.scope_root, worktree_path.canonicalize().unwrap());
    assert!(!scope.is_worktree);
    assert_eq!(scope.worktree_name, None);
}

// ── Snapshot tests ───────────────────────────────────────────────

#[test]
fn snapshot_counts_staged_unstaged_and_untracked_changes() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "one\ntwo\n");
    write_file(&tempdir.path().join("notes.txt"), "draft\n");

    let summary = load_snapshot(tempdir.path(), 7).unwrap();
    assert_eq!(summary.generation, 7);
    assert_eq!(summary.branch_name, current_branch_name(&repo));
    assert_eq!(summary.changed_files, 2);
    assert_eq!(summary.insertions, 2);
    assert_eq!(summary.deletions, 0);
}

#[test]
fn snapshot_uses_detached_branch_label() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    let commit_id = commit_all(&repo, "initial");
    repo.set_head_detached(commit_id).unwrap();

    let summary = load_snapshot(tempdir.path(), 1).unwrap();
    assert!(summary.branch_name.starts_with("detached@"));
}

#[test]
fn snapshot_fingerprint_is_stable_for_unchanged_content() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let first = load_snapshot(tempdir.path(), 1).unwrap();
    let second = load_snapshot(tempdir.path(), 2).unwrap();

    assert_eq!(first.content_fingerprint, second.content_fingerprint);
}

#[test]
fn snapshot_fingerprint_changes_when_diff_content_changes() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let first = load_snapshot(tempdir.path(), 1).unwrap();
    write_file(&tempdir.path().join("tracked.txt"), "one\ntwo\n");
    let second = load_snapshot(tempdir.path(), 2).unwrap();

    assert_ne!(first.content_fingerprint, second.content_fingerprint);
}

#[test]
fn snapshot_non_repo_returns_not_repository_error() {
    let tempdir = TempDir::new().unwrap();

    let error = load_snapshot(tempdir.path(), 1).unwrap_err();

    assert_eq!(error.kind(), SnapshotLoadErrorKind::NotRepository);
}

// ── Stash tests ───────────────────────────────────────────────────────

#[test]
fn list_stashes_reports_metadata_and_untracked_presence() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nstashed\n");
    write_file(&tempdir.path().join("notes.txt"), "draft\n");
    let stash_oid = create_stash(tempdir.path(), Some("tracked and untracked"), false, true)
        .expect("stash should be created");

    let list = list_stashes(tempdir.path()).unwrap();
    assert_eq!(list.scope_root, tempdir.path().canonicalize().unwrap());
    assert_eq!(list.repo_root, tempdir.path().canonicalize().unwrap());
    assert_eq!(list.entries.len(), 1);
    let entry = &list.entries[0];
    assert_eq!(entry.stash_oid, stash_oid);
    assert_eq!(entry.stash_index, 0);
    assert_eq!(entry.label, "stash@{0}");
    assert!(entry.message.contains("tracked and untracked"));
    assert!(entry.committed_at_unix > 0);
    assert!(entry.includes_untracked);
}

#[test]
fn load_stash_detail_merges_tracked_and_untracked_files() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nstashed\n");
    write_file(&tempdir.path().join("notes.txt"), "draft\n");
    let stash_oid = create_stash(tempdir.path(), Some("detail preview"), false, true).unwrap();

    let detail = load_stash_detail(tempdir.path(), stash_oid).unwrap();
    assert_eq!(detail.stash_oid, stash_oid);
    assert_eq!(detail.label, "stash@{0}");
    assert!(detail.message.contains("detail preview"));
    assert!(detail.includes_untracked);
    assert!(detail
        .files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("tracked.txt")
            && file.status == GitFileStatus::Modified));
    assert!(detail
        .files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("notes.txt")
            && file.status == GitFileStatus::Untracked));
}

#[test]
fn load_stash_file_diff_renders_tracked_and_untracked_files() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nstashed\n");
    write_file(&tempdir.path().join("notes.txt"), "draft\n");
    let stash_oid = create_stash(tempdir.path(), Some("file preview"), false, true).unwrap();

    let tracked = load_stash_file_diff(
        tempdir.path(),
        stash_oid,
        Path::new("tracked.txt"),
        ThemeId::Dark,
    )
    .unwrap();
    assert_eq!(tracked.file.status, GitFileStatus::Modified);
    assert!(tracked
        .lines
        .iter()
        .any(|line| line.text.contains("stashed")));

    let untracked = load_stash_file_diff(
        tempdir.path(),
        stash_oid,
        Path::new("notes.txt"),
        ThemeId::Dark,
    )
    .unwrap();
    assert_eq!(untracked.file.status, GitFileStatus::Untracked);
    assert!(untracked
        .lines
        .iter()
        .any(|line| line.text.contains("draft")));
}

#[test]
fn create_stash_with_keep_index_preserves_staged_changes_in_worktree() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    write_file(&tempdir.path().join("unstaged.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nstaged\n");
    run_git(tempdir.path(), &["add", "tracked.txt"]);
    write_file(&tempdir.path().join("unstaged.txt"), "base\nunstaged\n");

    let stash_oid = create_stash(tempdir.path(), Some("keep index"), true, false).unwrap();
    let index = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(index
        .staged_files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("tracked.txt")));
    assert!(index
        .unstaged_files
        .iter()
        .all(|file| file.relative_path != PathBuf::from("unstaged.txt")));

    let detail = load_stash_detail(tempdir.path(), stash_oid).unwrap();
    assert!(detail
        .files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("unstaged.txt")));
    assert!(detail
        .files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("tracked.txt")));
}

#[test]
fn apply_and_drop_stash_succeed() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\napplied\n");
    let stash_oid = create_stash(tempdir.path(), Some("apply me"), false, false).unwrap();
    assert_eq!(stash_oid_at(tempdir.path(), 0), stash_oid);

    let outcome = apply_stash(tempdir.path(), stash_oid).unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Applied {
            label: "stash@{0}".to_string(),
        }
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("tracked.txt")),
        "base\napplied\n"
    );
    assert_eq!(list_stashes(tempdir.path()).unwrap().entries.len(), 1);

    run_git(tempdir.path(), &["reset", "--hard", "HEAD"]);
    let dropped = drop_stash(tempdir.path(), stash_oid).unwrap();
    assert_eq!(dropped, "stash@{0}");
    assert!(list_stashes(tempdir.path()).unwrap().entries.is_empty());
}

#[test]
fn pop_stash_removes_entry_and_restores_changes() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\npopped\n");
    let stash_oid = create_stash(tempdir.path(), Some("pop me"), false, false).unwrap();

    let outcome = pop_stash(tempdir.path(), stash_oid).unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Applied {
            label: "stash@{0}".to_string(),
        }
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("tracked.txt")),
        "base\npopped\n"
    );
    assert!(list_stashes(tempdir.path()).unwrap().entries.is_empty());
}

#[test]
fn stash_apply_conflict_reports_conflicted_files() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "a\nb\nc\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "a\nstash\nc\n");
    let stash_oid = create_stash(tempdir.path(), Some("conflict"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "a\nhead\nc\n");
    run_git(tempdir.path(), &["add", "tracked.txt"]);
    run_git(tempdir.path(), &["commit", "-m", "head change"]);

    let outcome = apply_stash(tempdir.path(), stash_oid).unwrap();
    let expected_scope = tempdir.path().canonicalize().unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Conflicted {
            affected_scope: expected_scope,
            conflicted_files: vec![PathBuf::from("tracked.txt")],
        }
    );
}

#[test]
fn stash_pop_conflict_reports_conflicted_files_and_keeps_stash() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "a\nb\nc\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "a\nstash\nc\n");
    let stash_oid = create_stash(tempdir.path(), Some("pop conflict"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "a\nhead\nc\n");
    run_git(tempdir.path(), &["add", "tracked.txt"]);
    run_git(tempdir.path(), &["commit", "-m", "head change"]);

    let outcome = pop_stash(tempdir.path(), stash_oid).unwrap();
    let expected_scope = tempdir.path().canonicalize().unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Conflicted {
            affected_scope: expected_scope,
            conflicted_files: vec![PathBuf::from("tracked.txt")],
        }
    );
    assert_eq!(stash_oid_at(tempdir.path(), 0), stash_oid);
}

#[test]
fn stash_selection_identity_is_stable_when_indices_reorder() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nfirst\n");
    let first_oid = create_stash(tempdir.path(), Some("first"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nsecond\n");
    let second_oid = create_stash(tempdir.path(), Some("second"), false, false).unwrap();

    let before = list_stashes(tempdir.path()).unwrap();
    assert_eq!(before.entries[0].stash_oid, second_oid);
    assert_eq!(before.entries[1].stash_oid, first_oid);
    assert_eq!(before.entries[1].stash_index, 1);

    drop_stash(tempdir.path(), second_oid).unwrap();

    let after = list_stashes(tempdir.path()).unwrap();
    assert_eq!(after.entries.len(), 1);
    assert_eq!(after.entries[0].stash_oid, first_oid);
    assert_eq!(after.entries[0].stash_index, 0);
}

#[test]
fn apply_stash_targets_oid_after_indices_shift() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nfirst\n");
    let first_oid = create_stash(tempdir.path(), Some("first"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nsecond\n");
    create_stash(tempdir.path(), Some("second"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nthird\n");
    create_stash(tempdir.path(), Some("third"), false, false).unwrap();

    let outcome = apply_stash(tempdir.path(), first_oid).unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Applied {
            label: "stash@{2}".to_string(),
        }
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("tracked.txt")),
        "base\nfirst\n"
    );
}

#[test]
fn drop_stash_targets_oid_after_indices_shift() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nfirst\n");
    let first_oid = create_stash(tempdir.path(), Some("first"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nsecond\n");
    let second_oid = create_stash(tempdir.path(), Some("second"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nthird\n");
    let third_oid = create_stash(tempdir.path(), Some("third"), false, false).unwrap();

    let dropped = drop_stash(tempdir.path(), second_oid).unwrap();
    assert_eq!(dropped, "stash@{1}");

    let remaining = list_stashes(tempdir.path()).unwrap();
    assert_eq!(remaining.entries.len(), 2);
    assert_eq!(remaining.entries[0].stash_oid, third_oid);
    assert_eq!(remaining.entries[1].stash_oid, first_oid);
}

#[test]
fn pop_stash_targets_oid_after_indices_shift() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nfirst\n");
    create_stash(tempdir.path(), Some("first"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nsecond\n");
    let second_oid = create_stash(tempdir.path(), Some("second"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nthird\n");
    let third_oid = create_stash(tempdir.path(), Some("third"), false, false).unwrap();

    let outcome = pop_stash(tempdir.path(), second_oid).unwrap();
    assert_eq!(
        outcome,
        StashMutationOutcome::Applied {
            label: "stash@{1}".to_string(),
        }
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("tracked.txt")),
        "base\nsecond\n"
    );

    let remaining = list_stashes(tempdir.path()).unwrap();
    assert_eq!(remaining.entries.len(), 2);
    assert_eq!(remaining.entries[0].stash_oid, third_oid);
}

#[test]
fn missing_stash_oid_returns_error_without_mutating_other_entries() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "base\nfirst\n");
    let first_oid = create_stash(tempdir.path(), Some("first"), false, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "base\nsecond\n");
    let second_oid = create_stash(tempdir.path(), Some("second"), false, false).unwrap();

    drop_stash(tempdir.path(), second_oid).unwrap();

    let error = apply_stash(tempdir.path(), second_oid).unwrap_err();
    assert!(error.to_string().contains("is no longer available"));

    let remaining = list_stashes(tempdir.path()).unwrap();
    assert_eq!(remaining.entries.len(), 1);
    assert_eq!(remaining.entries[0].stash_oid, first_oid);
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("tracked.txt")),
        "base\n"
    );
}

#[test]
fn snapshot_repo_without_head_returns_unavailable_error() {
    let tempdir = TempDir::new().unwrap();
    Repository::init(tempdir.path()).unwrap();

    let error = load_snapshot(tempdir.path(), 1).unwrap_err();

    assert_eq!(error.kind(), SnapshotLoadErrorKind::Unavailable);
}

// ── Split diff index tests ───────────────────────────────────────

#[test]
fn diff_index_detects_renames_in_unstaged() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("before.txt"), "hello\n");
    commit_all(&repo, "initial");

    fs::rename(
        tempdir.path().join("before.txt"),
        tempdir.path().join("after.txt"),
    )
    .unwrap();

    let diff = load_diff_index(tempdir.path(), 3).unwrap();
    assert!(diff.staged_files.is_empty());
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(
        diff.unstaged_files[0].relative_path,
        PathBuf::from("after.txt")
    );
    assert_eq!(diff.unstaged_files[0].status, GitFileStatus::Renamed);
}

#[test]
fn staged_only_files_appear_in_staged_section() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    // Modify and stage
    write_file(&tempdir.path().join("a.txt"), "v2\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert_eq!(diff.staged_files[0].relative_path, PathBuf::from("a.txt"));
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn unstaged_only_files_appear_in_unstaged_section() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    // Modify without staging
    write_file(&tempdir.path().join("a.txt"), "v2\n");

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.staged_files.is_empty());
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(diff.unstaged_files[0].relative_path, PathBuf::from("a.txt"));
}

#[test]
fn same_path_appears_in_both_sections() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    // Stage v2
    write_file(&tempdir.path().join("a.txt"), "v2\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();

    // Modify to v3 in worktree (after staging v2)
    write_file(&tempdir.path().join("a.txt"), "v3\n");

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert_eq!(diff.staged_files[0].relative_path, PathBuf::from("a.txt"));
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(diff.unstaged_files[0].relative_path, PathBuf::from("a.txt"));
}

#[test]
fn untracked_file_in_unstaged_section() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("new.txt"), "untracked\n");

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.staged_files.is_empty());
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(
        diff.unstaged_files[0].relative_path,
        PathBuf::from("new.txt")
    );
    assert_eq!(diff.unstaged_files[0].status, GitFileStatus::Untracked);
}

#[test]
fn staged_file_diff_shows_head_vs_index() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "line1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "line1\nline2\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Staged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 1, &selection, ThemeId::Dark).unwrap();
    assert!(file_diff
        .lines
        .iter()
        .any(|l| l.kind == DiffLineKind::Addition && l.text.contains("line2")));
}

#[test]
fn unstaged_file_diff_shows_index_vs_worktree() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "line1\n");
    commit_all(&repo, "initial");

    // Stage v2
    write_file(&tempdir.path().join("a.txt"), "line1\nline2\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();

    // Write v3 to worktree
    write_file(&tempdir.path().join("a.txt"), "line1\nline2\nline3\n");

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 1, &selection, ThemeId::Dark).unwrap();
    // Should show addition of line3 (index has line1+line2, worktree has line1+line2+line3)
    assert!(file_diff
        .lines
        .iter()
        .any(|l| l.kind == DiffLineKind::Addition && l.text.contains("line3")));
    // Should NOT show line2 as added (it's already in index)
    assert!(!file_diff
        .lines
        .iter()
        .any(|l| l.kind == DiffLineKind::Addition && l.text.contains("line2")));
}

// ── Stage / unstage tests ────────────────────────────────────────

#[test]
fn stage_paths_stages_modified_file() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "v2\n");
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn stage_paths_stages_new_file() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("new.txt"), "new\n");
    stage_paths(tempdir.path(), &[PathBuf::from("new.txt")]).unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert_eq!(diff.staged_files[0].status, GitFileStatus::Added);
}

#[test]
fn stage_paths_stages_deletion() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    fs::remove_file(tempdir.path().join("a.txt")).unwrap();
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert_eq!(diff.staged_files[0].status, GitFileStatus::Deleted);
}

#[test]
fn unstage_paths_moves_to_unstaged() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "v2\n");
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    // Verify staged
    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert_eq!(diff.staged_files.len(), 1);

    unstage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.staged_files.is_empty());
    assert_eq!(diff.unstaged_files.len(), 1);
}

#[test]
fn unstage_new_file_becomes_untracked() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("new.txt"), "new\n");
    stage_paths(tempdir.path(), &[PathBuf::from("new.txt")]).unwrap();
    unstage_paths(tempdir.path(), &[PathBuf::from("new.txt")]).unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.staged_files.is_empty());
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(diff.unstaged_files[0].status, GitFileStatus::Untracked);
    // File still exists in worktree
    assert!(tempdir.path().join("new.txt").exists());
}

#[test]
fn stage_unstage_round_trip() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "v2\n");

    let before = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(before.staged_files.is_empty());
    assert_eq!(before.unstaged_files.len(), 1);

    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();
    unstage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let after = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(after.staged_files.is_empty());
    assert_eq!(after.unstaged_files.len(), 1);
}

#[test]
fn path_validation_rejects_traversal() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let result = stage_paths(tempdir.path(), &[PathBuf::from("../outside.txt")]);
    assert!(result.is_err());
}

#[test]
fn discard_all_unstaged_restores_worktree_and_preserves_staged_changes() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "a1\n");
    write_file(&tempdir.path().join("b.txt"), "b1\n");
    write_file(&tempdir.path().join("staged.txt"), "s1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "a2\n");
    fs::remove_file(tempdir.path().join("b.txt")).unwrap();
    write_file(tempdir.path().join("new.bin").as_path(), b"\0orca\0");
    write_file(&tempdir.path().join("staged.txt"), "s2\n");
    stage_paths(tempdir.path(), &[PathBuf::from("staged.txt")]).unwrap();

    discard_all_unstaged(tempdir.path()).unwrap();

    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "a1\n"
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("b.txt")),
        "b1\n"
    );
    assert!(!tempdir.path().join("new.bin").exists());
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("staged.txt")),
        "s2\n"
    );

    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
    assert_eq!(diff.staged_files.len(), 1);
    assert_eq!(
        diff.staged_files[0].relative_path,
        PathBuf::from("staged.txt")
    );
}

#[test]
fn discard_unstaged_file_only_reverts_target_path() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "a1\n");
    write_file(&tempdir.path().join("b.txt"), "b1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "a2\n");
    write_file(&tempdir.path().join("b.txt"), "b2\n");

    discard_unstaged_file(tempdir.path(), Path::new("a.txt")).unwrap();

    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "a1\n"
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("b.txt")),
        "b2\n"
    );

    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert_eq!(diff.unstaged_files.len(), 1);
    assert_eq!(diff.unstaged_files[0].relative_path, PathBuf::from("b.txt"));
}

#[test]
fn discard_unstaged_file_reverts_binary_content() {
    let (tempdir, repo) = repo_fixture();
    write_file(tempdir.path().join("blob.bin").as_path(), b"\0orca\0one");
    commit_all(&repo, "initial");

    write_file(tempdir.path().join("blob.bin").as_path(), b"\0orca\0two");

    discard_unstaged_file(tempdir.path(), Path::new("blob.bin")).unwrap();

    assert_eq!(
        fs::read(tempdir.path().join("blob.bin")).unwrap(),
        b"\0orca\0one"
    );
    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn discard_all_unstaged_removes_untracked_binary_files() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");

    write_file(
        tempdir.path().join("scratch.bin").as_path(),
        b"\0orca\0scratch",
    );

    discard_all_unstaged(tempdir.path()).unwrap();

    assert!(!tempdir.path().join("scratch.bin").exists());
    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn discard_all_unstaged_removes_untracked_directory_trees() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("scratch/nested/file.txt"),
        "temporary work\n",
    );

    discard_all_unstaged(tempdir.path()).unwrap();

    assert!(!tempdir.path().join("scratch").exists());
    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn load_file_diff_emits_structured_hunks_for_live_unstaged_files() {
    let (tempdir, repo) = repo_fixture();
    write_file(
        &tempdir.path().join("a.txt"),
        "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
    );
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    let filtered_lines = file_diff
        .lines
        .iter()
        .filter(|line| line.kind != DiffLineKind::FileHeader)
        .collect::<Vec<_>>();

    assert_eq!(file_diff.hunks.len(), 2);
    assert_eq!(file_diff.hunks[0].hunk_index, 0);
    assert_eq!(file_diff.hunks[1].hunk_index, 1);
    assert!(file_diff.hunks[0].header.starts_with("@@"));
    assert!(file_diff.hunks[1].header.starts_with("@@"));
    assert_ne!(file_diff.hunks[0].body_fingerprint, 0);
    assert_ne!(file_diff.hunks[1].body_fingerprint, 0);
    assert_eq!(
        filtered_lines[file_diff.hunks[0].line_start].kind,
        DiffLineKind::HunkHeader
    );
    assert_eq!(
        filtered_lines[file_diff.hunks[1].line_start].kind,
        DiffLineKind::HunkHeader
    );
    assert!(file_diff.hunks[0].line_end <= file_diff.hunks[1].line_start);
}

#[test]
fn discard_unstaged_hunk_reverts_only_the_target_hunk() {
    let (tempdir, repo) = repo_fixture();
    write_file(
        &tempdir.path().join("a.txt"),
        "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n",
    );
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.hunks.len(), 2);
    let target = hunk_target(&file_diff, file_diff.hunks[0].hunk_index);

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("a.txt"), &target).unwrap();
    assert_eq!(outcome, DiscardMutationOutcome::Applied);

    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "1\n2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n"
    );

    let after = load_file_diff(tempdir.path(), 3, &selection, ThemeId::Dark).unwrap();
    assert_eq!(after.hunks.len(), 1);
    assert!(after
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Addition && line.text.contains("changed-10")));
    assert!(!after
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Addition && line.text.contains("changed-2")));
}

#[test]
fn discard_unstaged_hunk_rejects_invalid_hunk_index() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "a1\n");
    commit_all(&repo, "initial");
    write_file(&tempdir.path().join("a.txt"), "a2\n");

    let target = DiscardHunkTarget {
        hunk_index: 9,
        body_fingerprint: 42,
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
        body: "bogus".to_string(),
    };
    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("a.txt"), &target).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Selected hunk changed. Refresh and try again.".to_string()
        }
    );
}

#[test]
fn discard_unstaged_hunk_removes_untracked_text_file() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("notes.txt"),
        "line one\nline two\nline three\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("notes.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.hunks.len(), 1);
    let target = hunk_target(&file_diff, 0);

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("notes.txt"), &target).unwrap();
    assert_eq!(outcome, DiscardMutationOutcome::Applied);

    assert!(!tempdir.path().join("notes.txt").exists());
    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn discard_unstaged_file_removes_untracked_text_file() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("notes.txt"), "line one\nline two\n");

    let outcome = discard_unstaged_file(tempdir.path(), Path::new("notes.txt")).unwrap();
    assert_eq!(outcome, DiscardMutationOutcome::Applied);

    assert!(!tempdir.path().join("notes.txt").exists());
    let diff = load_diff_index(tempdir.path(), 2).unwrap();
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn load_file_diff_for_live_staged_files_skips_hunk_metadata() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), &numbered_lines(12));
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Staged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();

    assert!(!file_diff.lines.is_empty());
    assert!(file_diff.hunks.is_empty());
}

#[test]
fn discard_unstaged_hunk_matches_same_body_after_hunk_positions_shift() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), &numbered_lines(30));
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\n2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\nchanged-25\n26\n27\n28\n29\n30\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    let target = hunk_target(&file_diff, 1);

    write_file(
        &tempdir.path().join("a.txt"),
        "1\ninserted-top\n2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\nchanged-25\n26\n27\n28\n29\n30\n",
    );

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("a.txt"), &target).unwrap();
    assert_eq!(outcome, DiscardMutationOutcome::Applied);
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "1\ninserted-top\n2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n25\n26\n27\n28\n29\n30\n"
    );
}

#[test]
fn discard_unstaged_hunk_blocks_when_target_body_changes() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), &numbered_lines(12));
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    let target = hunk_target(&file_diff, 0);

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2-again\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("a.txt"), &target).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Selected hunk changed. Refresh and try again.".to_string()
        }
    );
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "1\nchanged-2-again\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n"
    );
}

#[test]
fn discard_unstaged_hunk_blocks_when_duplicate_body_candidates_become_ambiguous() {
    let (tempdir, repo) = repo_fixture();
    write_file(
        &tempdir.path().join("repeat.txt"),
        "start-1\nstart-2\nstart-3\nstart-4\nstart-5\nstart-6\nstart-7\nx1\nx2\nx3\nold\ny1\ny2\ny3\nmid-1\nmid-2\nmid-3\nmid-4\nmid-5\nmid-6\nmid-7\nx1\nx2\nx3\nold\ny1\ny2\ny3\nend-1\nend-2\nend-3\n",
    );
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("repeat.txt"),
        "start-1\nstart-2\nstart-3\nstart-4\nstart-5\nstart-6\nstart-7\nx1\nx2\nx3\nnew\ny1\ny2\ny3\nmid-1\nmid-2\nmid-3\nmid-4\nmid-5\nmid-6\nmid-7\nx1\nx2\nx3\nnew\ny1\ny2\ny3\nend-1\nend-2\nend-3\n",
    );

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("repeat.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    let target = hunk_target(&file_diff, 1);

    write_file(
        &tempdir.path().join("repeat.txt"),
        "prefix\nstart-1\nstart-2\nstart-3\nstart-4\nstart-5\nstart-6\nstart-7\nx1\nx2\nx3\nnew\ny1\ny2\ny3\nmid-1\nmid-2\nmid-3\nmid-4\nmid-5\nmid-6\nmid-7\nx1\nx2\nx3\nnew\ny1\ny2\ny3\nend-1\nend-2\nend-3\n",
    );

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("repeat.txt"), &target).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Selected hunk changed. Refresh and try again.".to_string()
        }
    );
}

#[test]
fn discard_unstaged_hunk_preserves_same_file_partial_staging() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), &numbered_lines(12));
    commit_all(&repo, "initial");

    write_file(
        &tempdir.path().join("a.txt"),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\nchanged-10\n11\n12\n",
    );

    let diff = String::from_utf8(run_git(tempdir.path(), &["diff", "--", "a.txt"]).stdout).unwrap();
    let mut first_hunk_patch = String::new();
    let mut seen_hunks = 0usize;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            seen_hunks += 1;
        }
        if seen_hunks <= 1 {
            first_hunk_patch.push_str(line);
        }
    }
    let patch_path = tempdir.path().join("first-hunk.patch");
    write_file(&patch_path, first_hunk_patch);
    let apply = run_git(
        tempdir.path(),
        &["apply", "--cached", patch_path.to_str().unwrap()],
    );
    assert!(
        apply.status.success(),
        "git apply --cached failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    fs::remove_file(&patch_path).unwrap();

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("a.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 2, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.hunks.len(), 1);
    let target = hunk_target(&file_diff, 0);

    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("a.txt"), &target).unwrap();
    assert_eq!(outcome, DiscardMutationOutcome::Applied);
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("a.txt")),
        "1\nchanged-2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n"
    );

    let diff_index = load_diff_index(tempdir.path(), 3).unwrap();
    assert_eq!(diff_index.staged_files.len(), 1);
    assert!(diff_index
        .staged_files
        .iter()
        .any(|file| file.relative_path == PathBuf::from("a.txt")));
    assert!(diff_index.unstaged_files.is_empty());
}

#[test]
fn discard_unstaged_file_blocks_on_renames() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");
    fs::rename(
        tempdir.path().join("tracked.txt"),
        tempdir.path().join("renamed.txt"),
    )
    .unwrap();

    let outcome = discard_unstaged_file(tempdir.path(), Path::new("renamed.txt")).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Discard File is unavailable for renamed or typechanged files.".to_string()
        }
    );
}

#[test]
fn discard_unstaged_hunk_blocks_on_renames() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");
    fs::rename(
        tempdir.path().join("tracked.txt"),
        tempdir.path().join("renamed.txt"),
    )
    .unwrap();

    let target = DiscardHunkTarget {
        hunk_index: 0,
        body_fingerprint: 0,
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        body: String::new(),
    };
    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("renamed.txt"), &target).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Discard Hunk is unavailable for renamed or typechanged files.".to_string()
        }
    );
}

#[cfg(unix)]
#[test]
fn discard_unstaged_file_blocks_on_typechanges() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");
    fs::remove_file(tempdir.path().join("tracked.txt")).unwrap();
    std::os::unix::fs::symlink("other-target", tempdir.path().join("tracked.txt")).unwrap();

    let outcome = discard_unstaged_file(tempdir.path(), Path::new("tracked.txt")).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Discard File is unavailable for renamed or typechanged files.".to_string()
        }
    );
}

#[cfg(unix)]
#[test]
fn discard_unstaged_hunk_blocks_on_typechanges() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "tracked\n");
    commit_all(&repo, "initial");
    fs::remove_file(tempdir.path().join("tracked.txt")).unwrap();
    std::os::unix::fs::symlink("other-target", tempdir.path().join("tracked.txt")).unwrap();

    let target = DiscardHunkTarget {
        hunk_index: 0,
        body_fingerprint: 0,
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        body: String::new(),
    };
    let outcome = discard_unstaged_hunk(tempdir.path(), Path::new("tracked.txt"), &target).unwrap();
    assert_eq!(
        outcome,
        DiscardMutationOutcome::Blocked {
            reason: "Discard Hunk is unavailable for renamed or typechanged files.".to_string()
        }
    );
}

#[test]
fn discard_unstaged_file_rejects_path_traversal() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "a1\n");
    commit_all(&repo, "initial");
    write_file(&tempdir.path().join("a.txt"), "a2\n");

    let result = discard_unstaged_file(tempdir.path(), Path::new("../outside.txt"));
    assert!(result.is_err());
}

// ── Commit tests ─────────────────────────────────────────────────

#[test]
fn commit_staged_creates_commit() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "v2\n");
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let oid = commit_staged(tempdir.path(), "test commit").unwrap();
    let commit = repo.find_commit(oid).unwrap();
    assert_eq!(commit.message(), Some("test commit"));

    // Staged section should now be empty
    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.staged_files.is_empty());
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn commit_staged_rejects_empty_message() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let result = commit_staged(tempdir.path(), "");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty"));
}

#[test]
fn commit_staged_trims_whitespace() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("a.txt"), "v2\n");
    stage_paths(tempdir.path(), &[PathBuf::from("a.txt")]).unwrap();

    let oid = commit_staged(tempdir.path(), "  hello  ").unwrap();
    let commit = repo.find_commit(oid).unwrap();
    assert_eq!(commit.message(), Some("hello"));
}

#[test]
fn commit_staged_rejects_empty_staged_area() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    // Nothing staged. Should fail.
    let result = commit_staged(tempdir.path(), "empty commit");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("no staged changes"));
}

// ── Tracking status tests ────────────────────────────────────────

#[test]
fn tracking_no_upstream_returns_none() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.tracking.upstream_ref.is_none());
    assert_eq!(diff.tracking.ahead, 0);
    assert_eq!(diff.tracking.behind, 0);
}

#[test]
fn tracking_detached_head_returns_none() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    let oid = commit_all(&repo, "initial");
    repo.set_head_detached(oid).unwrap();

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.tracking.upstream_ref.is_none());
}

#[test]
fn tracking_ahead_of_upstream() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    // Create a bare remote and push
    let remote_dir = tempdir.path().join("remote.git");
    Repository::init_bare(&remote_dir).unwrap();
    let mut remote = repo.remote("origin", remote_dir.to_str().unwrap()).unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    // Set up tracking
    let mut branch = repo.find_branch("master", BranchType::Local).unwrap();
    branch.set_upstream(Some("origin/master")).unwrap();

    // Make additional local commit
    write_file(&tempdir.path().join("a.txt"), "v2\n");
    commit_all(&repo, "local only");

    let diff = load_diff_index(tempdir.path(), 1).unwrap();
    assert!(diff.tracking.upstream_ref.is_some());
    assert_eq!(diff.tracking.ahead, 1);
    assert_eq!(diff.tracking.behind, 0);
}

// ── Repository graph tests ───────────────────────────────────────

#[test]
fn repository_graph_supports_unborn_head() {
    let (tempdir, _repo) = repo_fixture();

    let graph = load_repository_graph(tempdir.path()).unwrap();
    assert_eq!(graph.head, HeadState::Unborn);
    assert!(graph.local_branches.is_empty());
    assert!(graph.remote_branches.is_empty());
    assert!(graph.commits.is_empty());
    assert!(!graph.truncated);
}

#[test]
fn repository_graph_supports_detached_head() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    let oid = commit_all(&repo, "initial");
    repo.set_head_detached(oid).unwrap();

    let graph = load_repository_graph(tempdir.path()).unwrap();
    assert_eq!(graph.head, HeadState::Detached { oid });
    assert_eq!(graph.commits[0].oid, oid);
    assert!(graph.commits[0]
        .ref_labels
        .iter()
        .any(|label| label.kind == CommitRefKind::Head && label.name == "HEAD"));
}

#[test]
fn repository_graph_loads_tracking_and_remote_branches() {
    let (_tempdir, repo, _bare_dir) = setup_tracking_repo();
    let branch_name = current_branch_name(&repo);

    let graph = load_repository_graph(repo.workdir().unwrap()).unwrap();
    assert_eq!(graph.local_branches.len(), 1);
    assert!(graph.local_branches[0].is_head);
    assert_eq!(
        graph.local_branches[0]
            .upstream
            .as_ref()
            .map(|tracking| tracking.remote_name.as_str()),
        Some("origin")
    );
    assert!(graph
        .remote_branches
        .iter()
        .any(|branch| branch.remote_name == "origin"
            && branch.short_name == branch_name
            && branch.tracked_by_local.as_deref() == Some(branch_name.as_str())));
    assert!(graph
        .remote_branches
        .iter()
        .all(|branch| branch.short_name != "HEAD"));
}

#[test]
fn repository_graph_ignores_local_only_upstreams() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");
    let main_branch = current_branch_name(&repo);

    let checkout_feature = run_git(tempdir.path(), &["checkout", "-b", "feature"]);
    assert!(
        checkout_feature.status.success(),
        "git checkout -b feature failed: {}",
        String::from_utf8_lossy(&checkout_feature.stderr)
    );

    let set_upstream = run_git(
        tempdir.path(),
        &["branch", "--set-upstream-to", &main_branch],
    );
    assert!(
        set_upstream.status.success(),
        "git branch --set-upstream-to {main_branch} failed: {}",
        String::from_utf8_lossy(&set_upstream.stderr)
    );

    let graph = load_repository_graph(tempdir.path()).unwrap();
    let feature = graph
        .local_branches
        .iter()
        .find(|branch| branch.name == "feature")
        .unwrap();
    assert!(feature.upstream.is_none());
}

#[test]
fn repository_graph_marks_truncated_when_history_exceeds_bound() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "0\n");
    commit_all(&repo, "seed");

    for index in 1..=(MAX_GRAPH_COMMITS + 1) {
        write_file(&tempdir.path().join("tracked.txt"), format!("{index}\n"));
        commit_all(&repo, &format!("commit {index}"));
    }

    let graph = load_repository_graph(tempdir.path()).unwrap();
    assert!(graph.truncated);
    assert_eq!(graph.commits.len(), MAX_GRAPH_COMMITS);
}

#[test]
fn repository_graph_generates_merge_lane_segments() {
    let (tempdir, _repo, merge_oid) = merge_repo_fixture();

    let graph = load_repository_graph(tempdir.path()).unwrap();
    let merge_commit = graph
        .commits
        .iter()
        .find(|node| node.oid == merge_oid)
        .unwrap();
    assert_eq!(merge_commit.parent_oids.len(), 2);
    assert!(merge_commit.row_lanes.iter().any(|segment| {
        matches!(
            segment.kind,
            GraphLaneKind::MergeFromLeft | GraphLaneKind::MergeFromRight
        )
    }));
    assert!(graph.commits.iter().any(|node| {
        node.row_lanes
            .iter()
            .any(|segment| segment.kind == GraphLaneKind::Start)
    }));
}

#[test]
fn lane_generator_preserves_divergent_children_on_distinct_lanes() {
    let branch_point = synthetic_oid(1);
    let main_tip = synthetic_oid(2);
    let feature_tip = synthetic_oid(3);
    let mut nodes = vec![
        graph_node(main_tip, vec![branch_point]),
        graph_node(feature_tip, vec![branch_point]),
        graph_node(branch_point, Vec::new()),
    ];

    populate_graph_lane_segments(&mut nodes);

    assert_eq!(nodes[0].primary_lane, 0);
    assert_eq!(nodes[1].primary_lane, 1);
    assert_eq!(nodes[2].primary_lane, 0);

    assert_eq!(
        nodes[0].row_lanes,
        vec![GraphLaneSegment {
            lane: 0,
            kind: GraphLaneKind::Start,
            target_lane: None,
        }]
    );
    assert_eq!(
        nodes[1].row_lanes,
        vec![
            GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::Through,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 1,
                kind: GraphLaneKind::Start,
                target_lane: None,
            },
        ]
    );
    assert_eq!(
        nodes[2].row_lanes,
        vec![
            GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::End,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 1,
                kind: GraphLaneKind::MergeFromRight,
                target_lane: Some(0),
            },
        ]
    );
}

#[test]
fn lane_generator_keeps_non_first_visible_parent_off_current_lane() {
    let truncated_first_parent = synthetic_oid(1);
    let visible_second_parent = synthetic_oid(2);
    let merge_commit = synthetic_oid(3);
    let mut nodes = vec![
        graph_node(
            merge_commit,
            vec![truncated_first_parent, visible_second_parent],
        ),
        graph_node(visible_second_parent, Vec::new()),
    ];

    populate_graph_lane_segments(&mut nodes);

    assert_eq!(nodes[0].primary_lane, 0);
    assert_eq!(nodes[1].primary_lane, 1);

    assert_eq!(
        nodes[0].row_lanes,
        vec![
            GraphLaneSegment {
                lane: 0,
                kind: GraphLaneKind::Start,
                target_lane: None,
            },
            GraphLaneSegment {
                lane: 1,
                kind: GraphLaneKind::MergeFromRight,
                target_lane: Some(0),
            },
        ]
    );
    assert_eq!(
        nodes[1].row_lanes,
        vec![GraphLaneSegment {
            lane: 1,
            kind: GraphLaneKind::End,
            target_lane: None,
        }]
    );
}

#[test]
fn commit_detail_for_root_commit_diffs_against_empty_history() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "root\n");
    let oid = commit_all(&repo, "initial");

    let detail = load_commit_detail(tempdir.path(), oid).unwrap();
    assert!(detail.parent_oids.is_empty());
    assert!(detail.changed_files.iter().any(|file| {
        file.path == PathBuf::from("tracked.txt") && file.status == CommitFileStatus::Added
    }));
}

#[test]
fn commit_detail_for_normal_commit_reports_modified_file() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "hello\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "hello\nworld\n");
    let oid = commit_all(&repo, "expand tracked");

    let detail = load_commit_detail(tempdir.path(), oid).unwrap();
    assert_eq!(detail.summary, "expand tracked");
    assert!(detail.changed_files.iter().any(|file| {
        file.path == PathBuf::from("tracked.txt")
            && file.status == CommitFileStatus::Modified
            && file.additions > 0
    }));
}

#[test]
fn commit_detail_for_rename_reports_old_path() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("before.txt"), "hello\n");
    commit_all(&repo, "initial");

    fs::rename(
        tempdir.path().join("before.txt"),
        tempdir.path().join("after.txt"),
    )
    .unwrap();
    let rename_oid = commit_all(&repo, "rename file");

    let detail = load_commit_detail(tempdir.path(), rename_oid).unwrap();
    assert!(detail.changed_files.iter().any(|file| {
        file.path == PathBuf::from("after.txt")
            && file.status
                == CommitFileStatus::Renamed {
                    from: PathBuf::from("before.txt"),
                }
    }));
}

#[test]
fn commit_detail_for_merge_uses_first_parent_semantics() {
    let (tempdir, _repo, merge_oid) = merge_repo_fixture();

    let detail = load_commit_detail(tempdir.path(), merge_oid).unwrap();
    assert!(detail
        .changed_files
        .iter()
        .any(|file| file.path == PathBuf::from("feature.txt")));
    assert!(detail
        .changed_files
        .iter()
        .all(|file| file.path != PathBuf::from("main.txt")));
}

#[test]
fn commit_file_diff_for_root_commit_uses_empty_base() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "hello\n");
    let oid = commit_all(&repo, "initial");

    let diff = load_commit_file_diff(tempdir.path(), oid, Path::new("tracked.txt"), ThemeId::Dark)
        .unwrap();
    assert!(diff.parent_oid.is_none());
    assert!(diff
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Addition && line.text.contains("hello")));
}

#[test]
fn commit_file_diff_for_normal_commit_renders_modified_lines() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "hello\n");
    commit_all(&repo, "initial");

    write_file(&tempdir.path().join("tracked.txt"), "hello\nworld\n");
    let oid = commit_all(&repo, "expand tracked");

    let diff = load_commit_file_diff(tempdir.path(), oid, Path::new("tracked.txt"), ThemeId::Dark)
        .unwrap();
    assert_eq!(diff.file.relative_path, PathBuf::from("tracked.txt"));
    assert!(diff
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Addition && line.text.contains("world")));
}

#[test]
fn commit_file_diff_for_rename_reuses_diff_renderer() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("before.txt"), "hello\n");
    commit_all(&repo, "initial");

    fs::rename(
        tempdir.path().join("before.txt"),
        tempdir.path().join("after.txt"),
    )
    .unwrap();
    let rename_oid = commit_all(&repo, "rename file");

    let diff = load_commit_file_diff(
        tempdir.path(),
        rename_oid,
        Path::new("after.txt"),
        ThemeId::Dark,
    )
    .unwrap();
    assert_eq!(diff.file.relative_path, PathBuf::from("after.txt"));
    assert_eq!(diff.file.status, GitFileStatus::Renamed);
}

#[test]
fn commit_file_diff_for_merge_uses_first_parent_semantics() {
    let (tempdir, _repo, merge_oid) = merge_repo_fixture();

    let diff = load_commit_file_diff(
        tempdir.path(),
        merge_oid,
        Path::new("feature.txt"),
        ThemeId::Dark,
    )
    .unwrap();
    assert!(diff.parent_oid.is_some());
    assert!(diff
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Addition && line.text.contains("feature")));
    assert!(load_commit_file_diff(
        tempdir.path(),
        merge_oid,
        Path::new("main.txt"),
        ThemeId::Dark
    )
    .is_err());
}

#[test]
fn commit_file_diff_for_binary_commit_returns_binary_notice() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("blob.bin"), [0_u8, 159, 146, 150]);
    let oid = commit_all(&repo, "binary");

    let diff =
        load_commit_file_diff(tempdir.path(), oid, Path::new("blob.bin"), ThemeId::Dark).unwrap();
    assert_eq!(diff.lines, vec![binary_notice(BINARY_DIFF_MESSAGE)]);
}

// ── Scope clean check test ───────────────────────────────────────

#[test]
fn is_scope_clean_reports_correctly() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    assert!(is_scope_clean(tempdir.path()).unwrap());

    write_file(&tempdir.path().join("a.txt"), "v2\n");
    assert!(!is_scope_clean(tempdir.path()).unwrap());
}

// ── Merge-back tests ─────────────────────────────────────────────

#[test]
fn merge_back_fast_forward() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make commits on managed branch
    write_file(&managed.path.join("a.txt"), "v2\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed commit");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::FastForward { .. }));

    // Verify source ref advanced
    let source_commit = repo
        .find_reference(&source_ref)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(source_commit.message(), Some("managed commit"));
}

#[test]
fn merge_back_already_merged() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make commits on managed branch
    write_file(&managed.path.join("a.txt"), "v2\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed commit");

    // First merge (fast-forward)
    merge_managed_branch(&managed.path, &source_ref).unwrap();

    // Second merge should be already merged
    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::AlreadyMerged));
}

#[test]
fn merge_back_clean_merge_commit() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make commit on managed branch (different file)
    write_file(&managed.path.join("b.txt"), "managed\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed commit");

    // Make commit on source branch (different file)
    write_file(&tempdir.path().join("c.txt"), "source\n");
    commit_all(&repo, "source commit");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::MergeCommit { .. }));

    // Verify merge commit has two parents
    if let MergeOutcome::MergeCommit { merge_oid } = outcome {
        let merge_commit = repo.find_commit(merge_oid).unwrap();
        assert_eq!(merge_commit.parent_count(), 2);
    }
}

#[test]
fn merge_back_conflict_blocks() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Conflicting changes to same file
    write_file(&managed.path.join("a.txt"), "managed version\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed conflict");

    write_file(&tempdir.path().join("a.txt"), "source version\n");
    commit_all(&repo, "source conflict");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    match outcome {
        MergeOutcome::Conflicted {
            affected_scope,
            conflicted_files,
        } => {
            assert_eq!(affected_scope, tempdir.path().canonicalize().unwrap());
            assert_eq!(conflicted_files, vec![PathBuf::from("a.txt")]);
        }
        other => panic!("expected conflicted outcome, got {other:?}"),
    }
    assert_eq!(repo.state(), RepositoryState::Merge);
    assert_eq!(
        Repository::open(&managed.path).unwrap().state(),
        RepositoryState::Clean
    );

    let diff = load_diff_index(tempdir.path(), 7).unwrap();
    assert!(diff.merge_state.is_some());
    assert_eq!(diff.conflicted_files.len(), 1);
    assert_eq!(
        diff.conflicted_files[0].relative_path,
        PathBuf::from("a.txt")
    );
    assert!(diff
        .staged_files
        .iter()
        .all(|f| f.relative_path != PathBuf::from("a.txt")));
    assert!(diff
        .unstaged_files
        .iter()
        .all(|f| f.relative_path != PathBuf::from("a.txt")));

    // Verify source ref is unchanged (still at "source conflict" commit)
    let source_commit = repo
        .find_reference(&source_ref)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(source_commit.message(), Some("source conflict"));
}

#[test]
fn merge_back_blocks_when_managed_scope_is_dirty() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make committed change on managed branch
    write_file(&managed.path.join("a.txt"), "v2\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed commit");

    // Leave dirty file in managed worktree
    write_file(&managed.path.join("dirty.txt"), "uncommitted\n");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::Blocked { .. }));
    if let MergeOutcome::Blocked { reason } = outcome {
        assert!(reason.contains("Managed worktree"));
    }
}

#[test]
fn merge_back_blocks_when_source_scope_is_dirty() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make committed change on managed branch
    write_file(&managed.path.join("b.txt"), "managed\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "managed commit");

    // Leave dirty file in source (main checkout)
    write_file(&tempdir.path().join("dirty.txt"), "uncommitted\n");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::Blocked { .. }));
    if let MergeOutcome::Blocked { reason } = outcome {
        assert!(reason.contains("Source branch"));
    }
}

#[test]
fn merge_back_fast_forward_updates_source_worktree() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("a.txt"), "v1\n");
    commit_all(&repo, "initial");

    let source_ref = current_head_ref(&repo);
    let managed = create_managed_worktree(tempdir.path(), "wt-ab123456").unwrap();

    // Make a new file on managed branch
    write_file(&managed.path.join("new.txt"), "from managed\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    configure_identity(&wt_repo);
    commit_all(&wt_repo, "add new.txt");

    let outcome = merge_managed_branch(&managed.path, &source_ref).unwrap();
    assert!(matches!(outcome, MergeOutcome::FastForward { .. }));

    // The source worktree should now have the new file on disk
    assert!(tempdir.path().join("new.txt").exists());
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("new.txt")),
        "from managed\n"
    );
}

// ── Worktree management tests ────────────────────────────────────

#[test]
fn discovers_managed_worktree_scope_and_admin_root() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    let scope = discover_scope(&managed.path).unwrap();
    assert_eq!(scope.repo_root, tempdir.path().canonicalize().unwrap());
    assert_eq!(scope.scope_root, managed.path);
    assert!(scope.is_worktree);
    assert_eq!(scope.worktree_name.as_deref(), Some("wt-abc12345"));
}

#[test]
fn remove_managed_worktree_without_branch() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    remove_managed_worktree(&managed.path, false).unwrap();

    // Worktree directory should be gone
    assert!(!managed.path.exists());
    // Branch should still exist
    assert!(repo
        .find_branch("orca/wt-abc12345", BranchType::Local)
        .is_ok());
}

#[test]
fn remove_managed_worktree_with_branch() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    remove_managed_worktree(&managed.path, true).unwrap();

    assert!(!managed.path.exists());
    assert!(repo
        .find_branch("orca/wt-abc12345", BranchType::Local)
        .is_err());
}

#[test]
fn remove_managed_worktree_blocks_when_dirty() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    // Dirty the worktree
    write_file(&managed.path.join("dirty.txt"), "uncommitted\n");

    let result = remove_managed_worktree(&managed.path, false);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("uncommitted changes"));
    // Worktree should still exist
    assert!(managed.path.exists());
}

#[test]
fn remove_managed_worktree_rejects_external_linked_worktree() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let head = repo.head().unwrap();
    let head_commit = peel_head_commit(&head).unwrap();
    repo.branch("external-branch", &head_commit, false).unwrap();

    let external_path = tempdir.path().join("external-worktree");
    let branch = repo
        .find_branch("external-branch", BranchType::Local)
        .unwrap();
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(branch.get()));
    repo.worktree("external-worktree", &external_path, Some(&opts))
        .unwrap();

    let result = remove_managed_worktree(&external_path, false);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("is not Orca-managed"));
    assert!(external_path.exists());
}

// ── Binary and oversize diff tests ───────────────────────────────

#[test]
fn binary_files_return_binary_notice() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("asset.bin"), [0_u8, 1, 2, 3, 4]);
    commit_all(&repo, "initial");
    write_file(&tempdir.path().join("asset.bin"), [0_u8, 1, 2, 9, 10]);

    let diff = load_diff_index(tempdir.path(), 5).unwrap();
    assert_eq!(diff.unstaged_files.len(), 1);
    assert!(diff.unstaged_files[0].is_binary);

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("asset.bin"),
    };
    let file_diff = load_file_diff(tempdir.path(), 5, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.lines, vec![binary_notice(BINARY_DIFF_MESSAGE)]);
}

#[cfg(unix)]
#[test]
fn non_binary_mode_only_change_returns_non_binary_notice() {
    use std::os::unix::fs::PermissionsExt;

    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("script.sh"), "#!/bin/sh\necho hi\n");
    commit_all(&repo, "initial");
    repo.config()
        .unwrap()
        .set_bool("core.filemode", true)
        .unwrap();

    let script_path = tempdir.path().join("script.sh");
    let mut permissions = fs::metadata(&script_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).unwrap();

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("script.sh"),
    };
    let file_diff = load_file_diff(tempdir.path(), 5, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.file.relative_path, PathBuf::from("script.sh"));
    assert!(!file_diff.file.is_binary);
}

#[test]
fn oversized_diff_returns_oversize_notice() {
    let (tempdir, repo) = repo_fixture();
    let baseline = (0..10_500)
        .map(|i| format!("old-{i}\n"))
        .collect::<String>();
    write_file(&tempdir.path().join("big.txt"), &baseline);
    commit_all(&repo, "initial");

    let updated = (0..10_500)
        .map(|i| format!("new-{i}\n"))
        .collect::<String>();
    write_file(&tempdir.path().join("big.txt"), updated);

    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("big.txt"),
    };
    let file_diff = load_file_diff(tempdir.path(), 9, &selection, ThemeId::Dark).unwrap();
    assert_eq!(file_diff.lines, vec![binary_notice(OVERSIZE_DIFF_MESSAGE)]);
}

// ── Worktree creation tests ──────────────────────────────────────

#[test]
fn managed_worktree_creation_is_deterministic_and_updates_excludes() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    let commit_id = commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    assert_eq!(managed.id, "wt-abc12345");
    assert_eq!(managed.branch_name, "orca/wt-abc12345");
    assert_eq!(managed.worktree_name, "wt-abc12345");
    assert_eq!(
        managed.path,
        tempdir
            .path()
            .canonicalize()
            .unwrap()
            .join(".orcashell/worktrees")
            .join("wt-abc12345")
    );
    assert_eq!(managed.source_ref, current_head_ref(&repo));

    let worktree_repo = Repository::open(&managed.path).unwrap();
    assert!(worktree_repo.is_worktree());
    let head = worktree_repo.head().unwrap();
    assert_eq!(head.shorthand(), Some(managed.branch_name.as_str()));
    assert_eq!(head.peel_to_commit().unwrap().id(), commit_id);

    let exclude = fs::read_to_string(tempdir.path().join(".git/info/exclude")).unwrap();
    let occurrences = exclude
        .lines()
        .filter(|line| line.trim() == ORCASHELL_EXCLUDE_ENTRY)
        .count();
    assert_eq!(occurrences, 1);

    ensure_orcashell_excluded(tempdir.path()).unwrap();
    let exclude = fs::read_to_string(tempdir.path().join(".git/info/exclude")).unwrap();
    let occurrences = exclude
        .lines()
        .filter(|line| line.trim() == ORCASHELL_EXCLUDE_ENTRY)
        .count();
    assert_eq!(occurrences, 1);
}

#[test]
fn split_gitdir_repos_update_their_actual_shared_exclude_file() {
    let (_tempdir, worktree_path, admin_dir, repo) = split_gitdir_repo_fixture();
    write_file(&worktree_path.join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    ensure_orcashell_excluded(&worktree_path).unwrap();

    let exclude = fs::read_to_string(admin_dir.join("info/exclude")).unwrap();
    assert!(exclude
        .lines()
        .any(|line| line.trim() == ORCASHELL_EXCLUDE_ENTRY));
    assert!(fs::metadata(worktree_path.join(".git")).unwrap().is_file());
}

#[test]
fn managed_worktree_requires_valid_head_commit() {
    let (tempdir, _repo) = repo_fixture();
    let err = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap_err();
    assert!(err.to_string().contains("HEAD"));
}

#[test]
fn managed_worktree_id_validation_requires_uuid_like_suffix() {
    assert!(validate_worktree_id("wt-abc12345").is_ok());
    assert!(validate_worktree_id("wt-ABC12345").is_err());
    assert!(validate_worktree_id("wt-abc1234").is_err());
    assert!(validate_worktree_id("wt-abc12345-extra").is_err());
    assert!(validate_worktree_id("feature-abc1").is_err());
}

#[test]
fn managed_worktree_path_collision_does_not_leave_branch_behind() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let collision_path =
        managed_worktree_path(&tempdir.path().canonicalize().unwrap(), "wt-abc12345");
    fs::create_dir_all(&collision_path).unwrap();

    let err = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap_err();
    assert!(err.to_string().contains("already exists"));
    assert!(repo
        .find_branch("orca/wt-abc12345", BranchType::Local)
        .is_err());
}

// ── Upstream info tests ────────────────────────────────────────────

#[test]
fn resolve_upstream_info_no_upstream() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let result = resolve_upstream_info(tempdir.path());
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("No upstream configured"));
}

#[test]
fn resolve_upstream_info_with_tracking() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    // Create a bare remote and configure tracking.
    let bare_dir = tempdir.path().join("bare.git");
    Repository::init_bare(&bare_dir).unwrap();
    let mut remote = repo.remote("origin", bare_dir.to_str().unwrap()).unwrap();
    remote.push(&["refs/heads/master"], None).ok();
    let mut config = repo.config().unwrap();
    let branch = current_branch_name(&repo);
    config
        .set_str(&format!("branch.{branch}.remote"), "origin")
        .unwrap();
    config
        .set_str(
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        )
        .unwrap();

    let info = resolve_upstream_info(tempdir.path()).unwrap();
    assert_eq!(info.remote, "origin");
    assert_eq!(info.upstream_branch, branch);
}

fn create_remote_branch(
    tempdir: &TempDir,
    bare_dir: &Path,
    branch_name: &str,
    file_name: &str,
    contents: &str,
) {
    let other_dir = tempdir
        .path()
        .join(format!("other-{}", branch_name.replace('/', "-")));
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);

    let checkout = run_git(&other_dir, &["checkout", "-b", branch_name]);
    assert!(
        checkout.status.success(),
        "git checkout -b {branch_name} failed: {}",
        String::from_utf8_lossy(&checkout.stderr)
    );

    write_file(&other_dir.join(file_name), contents);
    commit_all(&other, &format!("{branch_name} commit"));

    let refspec = format!("refs/heads/{branch_name}:refs/heads/{branch_name}");
    other
        .find_remote("origin")
        .unwrap()
        .push(&[&refspec], None)
        .unwrap();
}

// ── Branch checkout tests ───────────────────────────────────────────

#[test]
fn checkout_local_branch_switches_head() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let outcome = checkout_local_branch(tempdir.path(), "feature").unwrap();
    assert_eq!(
        outcome,
        BranchCheckoutOutcome::SwitchedLocal {
            branch_name: "feature".to_string(),
        }
    );

    let reopened = Repository::open(tempdir.path()).unwrap();
    assert_eq!(current_branch_name(&reopened), "feature");
}

#[test]
fn create_local_branch_from_local_uses_selected_target_and_checks_out() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let initial_commit = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &initial_commit, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "main change\n");
    let main_head = commit_all(&repo, "main change");

    let outcome =
        create_local_branch_from_local(tempdir.path(), "feature", "review/feature").unwrap();
    assert_eq!(
        outcome,
        CreateLocalBranchOutcome::CreatedAndCheckedOut {
            source_branch_name: "feature".to_string(),
            branch_name: "review/feature".to_string(),
        }
    );

    let reopened = Repository::open(tempdir.path()).unwrap();
    assert_eq!(current_branch_name(&reopened), "review/feature");
    let review_target = reopened
        .find_branch("review/feature", BranchType::Local)
        .unwrap()
        .get()
        .target()
        .unwrap();
    let feature_target = reopened
        .find_branch("feature", BranchType::Local)
        .unwrap()
        .get()
        .target()
        .unwrap();
    assert_eq!(review_target, feature_target);
    assert_ne!(review_target, main_head);
}

#[test]
fn checkout_local_branch_blocks_when_dirty() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");
    let main_branch = current_branch_name(&repo);

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();
    write_file(&tempdir.path().join("dirty.txt"), "dirty\n");

    let outcome = checkout_local_branch(tempdir.path(), "feature").unwrap();
    match outcome {
        BranchCheckoutOutcome::Blocked { reason } => {
            assert!(reason.contains("uncommitted changes"));
        }
        other => panic!("expected blocked checkout, got {other:?}"),
    }

    let reopened = Repository::open(tempdir.path()).unwrap();
    assert_eq!(current_branch_name(&reopened), main_branch);
}

#[test]
fn delete_local_branch_removes_non_current_branch() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let outcome = delete_local_branch(tempdir.path(), "feature").unwrap();
    assert_eq!(
        outcome,
        DeleteLocalBranchOutcome::Deleted {
            branch_name: "feature".to_string(),
        }
    );

    let reopened = Repository::open(tempdir.path()).unwrap();
    assert!(reopened.find_branch("feature", BranchType::Local).is_err());
}

#[test]
fn delete_local_branch_blocks_when_current() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let main_branch = current_branch_name(&repo);
    let outcome = delete_local_branch(tempdir.path(), &main_branch).unwrap();
    match outcome {
        DeleteLocalBranchOutcome::Blocked { reason } => {
            assert!(reason.contains("Cannot delete the current branch"));
        }
        other => panic!("expected blocked delete, got {other:?}"),
    }
}

#[test]
fn delete_local_branch_blocks_when_not_fully_merged() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let main_branch = current_branch_name(&repo);
    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let checkout_feature = run_git(tempdir.path(), &["checkout", "feature"]);
    assert!(checkout_feature.status.success());
    write_file(&tempdir.path().join("feature.txt"), "feature\n");
    commit_all(&repo, "feature change");

    let checkout_main = run_git(tempdir.path(), &["checkout", &main_branch]);
    assert!(checkout_main.status.success());

    let outcome = delete_local_branch(tempdir.path(), "feature").unwrap();
    match outcome {
        DeleteLocalBranchOutcome::Blocked { reason } => {
            assert!(reason.contains("not fully merged"));
        }
        other => panic!("expected blocked delete, got {other:?}"),
    }
}

#[test]
fn checkout_local_branch_blocks_during_merge() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "base\n");
    commit_all(&repo, "initial");
    let main_branch = current_branch_name(&repo);

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("other", &head, false).unwrap();

    write_file(&tempdir.path().join("tracked.txt"), "main change\n");
    commit_all(&repo, "main change");

    let checkout_other = run_git(tempdir.path(), &["checkout", "other"]);
    assert!(checkout_other.status.success());
    write_file(&tempdir.path().join("tracked.txt"), "other change\n");
    commit_all(&repo, "other change");

    let checkout_main = run_git(tempdir.path(), &["checkout", &main_branch]);
    assert!(checkout_main.status.success());
    let merge = run_git(tempdir.path(), &["merge", "other"]);
    assert!(!merge.status.success());
    assert_eq!(repo.state(), RepositoryState::Merge);

    let outcome = checkout_local_branch(tempdir.path(), "other").unwrap();
    match outcome {
        BranchCheckoutOutcome::Blocked { reason } => {
            assert!(reason.contains("merge is in progress"));
        }
        other => panic!("expected blocked checkout, got {other:?}"),
    }
}

#[test]
fn checkout_remote_branch_creates_tracking_branch() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();
    create_remote_branch(&tempdir, &bare_dir, "feature", "feature.txt", "feature\n");
    let fetch = run_git(repo.workdir().unwrap(), &["fetch", "origin", "feature"]);
    assert!(fetch.status.success());

    let outcome =
        checkout_remote_branch(repo.workdir().unwrap(), "refs/remotes/origin/feature").unwrap();
    assert_eq!(
        outcome,
        BranchCheckoutOutcome::SwitchedTracking {
            local_branch_name: "feature".to_string(),
            remote_full_ref: "refs/remotes/origin/feature".to_string(),
            created: true,
        }
    );

    let reopened = Repository::open(repo.workdir().unwrap()).unwrap();
    assert_eq!(current_branch_name(&reopened), "feature");
    let branch = reopened.find_branch("feature", BranchType::Local).unwrap();
    assert_eq!(
        local_branch_upstream_full_ref(&branch).unwrap().as_deref(),
        Some("refs/remotes/origin/feature")
    );
}

#[test]
fn checkout_remote_branch_reuses_existing_tracking_branch() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();
    create_remote_branch(&tempdir, &bare_dir, "feature", "feature.txt", "feature\n");
    let fetch = run_git(repo.workdir().unwrap(), &["fetch", "origin", "feature"]);
    assert!(fetch.status.success());

    let remote_target = repo
        .find_reference("refs/remotes/origin/feature")
        .unwrap()
        .target()
        .unwrap();
    let remote_commit = repo.find_commit(remote_target).unwrap();
    let mut branch = repo
        .branch("review-feature", &remote_commit, false)
        .unwrap();
    branch.set_upstream(Some("origin/feature")).unwrap();

    let outcome =
        checkout_remote_branch(repo.workdir().unwrap(), "refs/remotes/origin/feature").unwrap();
    assert_eq!(
        outcome,
        BranchCheckoutOutcome::SwitchedTracking {
            local_branch_name: "review-feature".to_string(),
            remote_full_ref: "refs/remotes/origin/feature".to_string(),
            created: false,
        }
    );

    let reopened = Repository::open(repo.workdir().unwrap()).unwrap();
    assert_eq!(current_branch_name(&reopened), "review-feature");
    assert!(reopened.find_branch("feature", BranchType::Local).is_err());
}

#[test]
fn checkout_remote_branch_blocks_on_short_name_collision() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();
    let main_branch = current_branch_name(&repo);
    create_remote_branch(&tempdir, &bare_dir, "feature", "feature.txt", "feature\n");
    let fetch = run_git(repo.workdir().unwrap(), &["fetch", "origin", "feature"]);
    assert!(fetch.status.success());

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let outcome =
        checkout_remote_branch(repo.workdir().unwrap(), "refs/remotes/origin/feature").unwrap();
    match outcome {
        BranchCheckoutOutcome::Blocked { reason } => {
            assert!(reason.contains("already exists"));
            assert!(reason.contains("does not track"));
        }
        other => panic!("expected blocked remote checkout, got {other:?}"),
    }

    let reopened = Repository::open(repo.workdir().unwrap()).unwrap();
    assert_eq!(current_branch_name(&reopened), main_branch);
}

#[test]
fn checkout_remote_branch_from_unborn_repo_succeeds() {
    let tempdir = TempDir::new().unwrap();
    let bare_dir = tempdir.path().join("bare.git");
    Repository::init_bare(&bare_dir).unwrap();
    create_remote_branch(&tempdir, &bare_dir, "feature", "feature.txt", "feature\n");

    let client_dir = tempdir.path().join("client");
    let repo = Repository::init(&client_dir).unwrap();
    configure_identity(&repo);

    let add_remote = run_git(
        &client_dir,
        &["remote", "add", "origin", bare_dir.to_str().unwrap()],
    );
    assert!(
        add_remote.status.success(),
        "git remote add failed: {}",
        String::from_utf8_lossy(&add_remote.stderr)
    );

    let fetch = run_git(&client_dir, &["fetch", "origin", "feature"]);
    assert!(
        fetch.status.success(),
        "git fetch origin feature failed: {}",
        String::from_utf8_lossy(&fetch.stderr)
    );

    let outcome = checkout_remote_branch(&client_dir, "refs/remotes/origin/feature").unwrap();
    assert_eq!(
        outcome,
        BranchCheckoutOutcome::SwitchedTracking {
            local_branch_name: "feature".to_string(),
            remote_full_ref: "refs/remotes/origin/feature".to_string(),
            created: true,
        }
    );

    let reopened = Repository::open(&client_dir).unwrap();
    assert_eq!(current_branch_name(&reopened), "feature");
    let branch = reopened.find_branch("feature", BranchType::Local).unwrap();
    assert_eq!(
        local_branch_upstream_full_ref(&branch).unwrap().as_deref(),
        Some("refs/remotes/origin/feature")
    );
}

#[test]
fn checkout_local_branch_blocks_when_checked_out_in_other_worktree() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let worktree_parent = TempDir::new().unwrap();
    let worktree_path = worktree_parent.path().join("feature-worktree");
    let add = run_git(
        tempdir.path(),
        &[
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            "feature",
        ],
    );
    assert!(
        add.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let outcome = checkout_local_branch(tempdir.path(), "feature").unwrap();
    match outcome {
        BranchCheckoutOutcome::Blocked { reason } => {
            assert!(reason.contains("already checked out"));
        }
        other => panic!("expected blocked worktree checkout, got {other:?}"),
    }
}

#[test]
fn delete_local_branch_blocks_when_checked_out_in_other_worktree() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let head = repo
        .find_commit(repo.head().unwrap().target().unwrap())
        .unwrap();
    repo.branch("feature", &head, false).unwrap();

    let worktree_parent = TempDir::new().unwrap();
    let worktree_path = worktree_parent.path().join("feature-worktree");
    let add = run_git(
        tempdir.path(),
        &[
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            "feature",
        ],
    );
    assert!(
        add.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let outcome = delete_local_branch(tempdir.path(), "feature").unwrap();
    match outcome {
        DeleteLocalBranchOutcome::Blocked { reason } => {
            assert!(reason.contains("checked out"));
        }
        other => panic!("expected blocked delete, got {other:?}"),
    }
}

// ── Pull integration tests ──────────────────────────────────────

fn setup_tracking_repo() -> (TempDir, Repository, PathBuf) {
    let tempdir = TempDir::new().unwrap();
    let bare_dir = tempdir.path().join("bare.git");
    Repository::init_bare(&bare_dir).unwrap();

    let client_dir = tempdir.path().join("client");
    let repo = Repository::clone(bare_dir.to_str().unwrap(), &client_dir).unwrap();
    configure_identity(&repo);

    write_file(&client_dir.join("file.txt"), "initial\n");
    commit_all(&repo, "initial");

    // Push to bare remote so tracking is set up.
    {
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    (tempdir, repo, bare_dir)
}

#[test]
fn pull_integrate_already_up_to_date() {
    let (_tempdir, repo, _bare_dir) = setup_tracking_repo();
    let result = pull_integrate(repo.workdir().unwrap()).unwrap();
    assert_eq!(result, MergeOutcome::AlreadyMerged);
}

#[test]
fn pull_integrate_fast_forward() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();

    // Create a second clone, make a commit, push.
    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    write_file(&other_dir.join("file.txt"), "initial\nnew line\n");
    commit_all(&other, "other commit");
    let mut remote = other.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    // Fetch in client to update tracking ref.
    let mut client_remote = repo.find_remote("origin").unwrap();
    client_remote.fetch(&["master"], None, None).unwrap();

    let result = pull_integrate(repo.workdir().unwrap()).unwrap();
    assert!(matches!(result, MergeOutcome::FastForward { .. }));

    // Verify file updated.
    let content = fs::read_to_string(repo.workdir().unwrap().join("file.txt")).unwrap();
    assert!(content.contains("new line"));
}

#[test]
fn pull_integrate_conflict_blocks() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();

    // Create a second clone, make a conflicting commit, push.
    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    write_file(&other_dir.join("file.txt"), "conflict content\n");
    commit_all(&other, "conflicting commit");
    let mut remote = other.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    // Make a local commit with different content.
    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "local conflict content\n",
    );
    commit_all(&repo, "local commit");

    // Fetch in client.
    let mut client_remote = repo.find_remote("origin").unwrap();
    client_remote.fetch(&["master"], None, None).unwrap();

    let head_before = repo.head().unwrap().target().unwrap();
    let result = pull_integrate(repo.workdir().unwrap()).unwrap();
    match result {
        MergeOutcome::Conflicted {
            affected_scope,
            conflicted_files,
        } => {
            assert_eq!(
                affected_scope,
                repo.workdir().unwrap().canonicalize().unwrap()
            );
            assert_eq!(conflicted_files, vec![PathBuf::from("file.txt")]);
        }
        other => panic!("expected conflicted outcome, got {other:?}"),
    }
    assert_eq!(repo.state(), RepositoryState::Merge);

    // Verify HEAD unchanged.
    let head_after = repo.head().unwrap().target().unwrap();
    assert_eq!(head_before, head_after);

    let diff = load_diff_index(repo.workdir().unwrap(), 3).unwrap();
    assert!(diff.merge_state.is_some());
    assert_eq!(diff.conflicted_files.len(), 1);
    assert_eq!(
        diff.conflicted_files[0].relative_path,
        PathBuf::from("file.txt")
    );
    assert!(diff
        .staged_files
        .iter()
        .all(|f| f.relative_path != PathBuf::from("file.txt")));
    assert!(diff
        .unstaged_files
        .iter()
        .all(|f| f.relative_path != PathBuf::from("file.txt")));
}

#[test]
fn pull_integrate_dirty_scope_blocks() {
    let (_tempdir, repo, _bare_dir) = setup_tracking_repo();

    // Dirty the scope.
    write_file(&repo.workdir().unwrap().join("dirty.txt"), "uncommitted\n");

    let result = pull_integrate(repo.workdir().unwrap()).unwrap();
    assert!(matches!(result, MergeOutcome::Blocked { .. }));
    if let MergeOutcome::Blocked { reason } = result {
        assert!(reason.contains("uncommitted changes"));
    }
}

#[test]
fn parse_conflict_file_text_handles_diff3_markers() {
    let parsed = parse_conflict_file_text(
        "<<<<<<< ours\nleft\n||||||| base\nbase\n=======\nright\n>>>>>>> theirs\n",
    )
    .unwrap();

    assert_eq!(parsed.blocks.len(), 1);
    assert!(parsed.has_base_sections);
    let block = &parsed.blocks[0];
    assert_eq!(&parsed.raw_text[block.ours.clone()], "left\n");
    assert_eq!(&parsed.raw_text[block.base.clone().unwrap()], "base\n");
    assert_eq!(&parsed.raw_text[block.theirs.clone()], "right\n");
}

#[test]
fn pull_integrate_honors_diff3_conflictstyle() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();
    repo.config()
        .unwrap()
        .set_str("merge.conflictstyle", "diff3")
        .unwrap();

    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    other
        .config()
        .unwrap()
        .set_str("merge.conflictstyle", "diff3")
        .unwrap();
    write_file(&other_dir.join("file.txt"), "conflict content\n");
    commit_all(&other, "conflicting commit");
    other
        .find_remote("origin")
        .unwrap()
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "local conflict content\n",
    );
    commit_all(&repo, "local commit");

    let mut client_remote = repo.find_remote("origin").unwrap();
    client_remote.fetch(&["master"], None, None).unwrap();

    let result = pull_integrate(repo.workdir().unwrap()).unwrap();
    assert!(matches!(result, MergeOutcome::Conflicted { .. }));

    let conflicted_text = fs::read_to_string(repo.workdir().unwrap().join("file.txt")).unwrap();
    assert!(conflicted_text.contains("||||||| "));
}

#[test]
fn complete_merge_rejects_unresolved_and_unstaged_changes() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();

    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    write_file(&other_dir.join("file.txt"), "conflict content\n");
    commit_all(&other, "conflicting commit");
    other
        .find_remote("origin")
        .unwrap()
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "local conflict content\n",
    );
    commit_all(&repo, "local commit");
    repo.find_remote("origin")
        .unwrap()
        .fetch(&["master"], None, None)
        .unwrap();

    assert!(matches!(
        pull_integrate(repo.workdir().unwrap()).unwrap(),
        MergeOutcome::Conflicted { .. }
    ));
    assert!(complete_merge(repo.workdir().unwrap())
        .unwrap_err()
        .to_string()
        .contains("unresolved conflicts"));

    write_file(&repo.workdir().unwrap().join("file.txt"), "resolved\n");
    stage_paths(repo.workdir().unwrap(), &[PathBuf::from("file.txt")]).unwrap();
    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "resolved but unstaged\n",
    );
    assert!(complete_merge(repo.workdir().unwrap())
        .unwrap_err()
        .to_string()
        .contains("unstaged changes"));
}

#[test]
fn complete_merge_creates_two_parent_commit_and_clears_state() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();

    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    write_file(&other_dir.join("file.txt"), "conflict content\n");
    commit_all(&other, "conflicting commit");
    other
        .find_remote("origin")
        .unwrap()
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "local conflict content\n",
    );
    commit_all(&repo, "local commit");
    repo.find_remote("origin")
        .unwrap()
        .fetch(&["master"], None, None)
        .unwrap();

    assert!(matches!(
        pull_integrate(repo.workdir().unwrap()).unwrap(),
        MergeOutcome::Conflicted { .. }
    ));

    write_file(&repo.workdir().unwrap().join("file.txt"), "resolved\n");
    stage_paths(repo.workdir().unwrap(), &[PathBuf::from("file.txt")]).unwrap();

    let diff = load_diff_index(repo.workdir().unwrap(), 5).unwrap();
    assert!(diff.conflicted_files.is_empty());
    assert!(diff
        .merge_state
        .as_ref()
        .is_some_and(|state| state.can_complete));

    let merge_oid = complete_merge(repo.workdir().unwrap()).unwrap();
    let merge_commit = repo.find_commit(merge_oid).unwrap();
    assert_eq!(merge_commit.parent_count(), 2);
    assert_eq!(repo.state(), RepositoryState::Clean);
    assert!(!repo.path().join("MERGE_HEAD").exists());
}

#[test]
fn abort_merge_restores_pre_merge_state() {
    let (tempdir, repo, bare_dir) = setup_tracking_repo();

    let other_dir = tempdir.path().join("other");
    let other = Repository::clone(bare_dir.to_str().unwrap(), &other_dir).unwrap();
    configure_identity(&other);
    write_file(&other_dir.join("file.txt"), "conflict content\n");
    commit_all(&other, "conflicting commit");
    other
        .find_remote("origin")
        .unwrap()
        .push(&["refs/heads/master:refs/heads/master"], None)
        .unwrap();

    write_file(
        &repo.workdir().unwrap().join("file.txt"),
        "local conflict content\n",
    );
    commit_all(&repo, "local commit");
    repo.find_remote("origin")
        .unwrap()
        .fetch(&["master"], None, None)
        .unwrap();

    let head_before = repo.head().unwrap().target().unwrap();
    assert!(matches!(
        pull_integrate(repo.workdir().unwrap()).unwrap(),
        MergeOutcome::Conflicted { .. }
    ));

    abort_merge(repo.workdir().unwrap()).unwrap();

    assert_eq!(repo.state(), RepositoryState::Clean);
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);
    assert_eq!(
        read_text_file_normalized(&repo.workdir().unwrap().join("file.txt")),
        "local conflict content\n"
    );
    assert!(!repo.path().join("MERGE_HEAD").exists());
}

#[test]
fn load_diff_index_detects_external_merge_state() {
    let (tempdir, repo) = repo_fixture();
    configure_identity(&repo);
    write_file(&tempdir.path().join("file.txt"), "initial\n");
    commit_all(&repo, "initial");

    let main_branch = current_branch_name(&repo);
    repo.branch(
        "other",
        &repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap(),
        false,
    )
    .unwrap();
    write_file(&tempdir.path().join("file.txt"), "main change\n");
    commit_all(&repo, "main change");

    let checkout_other = run_git(tempdir.path(), &["checkout", "other"]);
    assert!(checkout_other.status.success());
    write_file(&tempdir.path().join("file.txt"), "other change\n");
    commit_all(&repo, "other change");

    let checkout_main = run_git(tempdir.path(), &["checkout", &main_branch]);
    assert!(checkout_main.status.success());
    let merge_output = run_git(tempdir.path(), &["merge", "other"]);
    assert!(!merge_output.status.success());

    let diff = load_diff_index(tempdir.path(), 9).unwrap();
    assert!(diff.merge_state.is_some());
    assert_eq!(diff.conflicted_files.len(), 1);
    assert_eq!(
        diff.conflicted_files[0].relative_path,
        PathBuf::from("file.txt")
    );
    assert!(diff.repo_state_warning.is_none());
}

// ── Merge-back tests (source repo) ──────────────────────────────

#[test]
fn merge_back_opens_source_repo_directly() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    configure_identity(&Repository::open(&managed.path).unwrap());

    // Make a commit in managed worktree.
    write_file(&managed.path.join("new.txt"), "from managed\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    commit_all(&wt_repo, "managed commit");

    let result = merge_managed_branch(&managed.path, &managed.source_ref).unwrap();
    assert!(
        matches!(result, MergeOutcome::FastForward { .. }),
        "expected fast-forward, got {result:?}"
    );

    // Verify merge result visible in source repo.
    let source_repo = Repository::open(tempdir.path()).unwrap();
    assert_eq!(
        read_text_file_normalized(&tempdir.path().join("new.txt")),
        "from managed\n"
    );

    // Verify source HEAD advanced.
    let source_head = source_repo.head().unwrap().peel_to_commit().unwrap().id();
    let managed_head = wt_repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(source_head, managed_head);
}

#[test]
fn merge_back_source_scope_dirty_blocks() {
    let (tempdir, repo) = repo_fixture();
    write_file(&tempdir.path().join("tracked.txt"), "one\n");
    commit_all(&repo, "initial");

    let managed = create_managed_worktree(tempdir.path(), "wt-abc12345").unwrap();
    configure_identity(&Repository::open(&managed.path).unwrap());

    // Make a commit in managed worktree.
    write_file(&managed.path.join("new.txt"), "from managed\n");
    let wt_repo = Repository::open(&managed.path).unwrap();
    commit_all(&wt_repo, "managed commit");

    // Dirty the source scope.
    write_file(&tempdir.path().join("dirty.txt"), "uncommitted\n");

    let result = merge_managed_branch(&managed.path, &managed.source_ref).unwrap();
    assert!(matches!(result, MergeOutcome::Blocked { .. }));
    if let MergeOutcome::Blocked { reason } = result {
        assert!(reason.contains("Source branch has uncommitted changes"));
    }
}

// ── Inline diff tests ────────────────────────────────────────────

#[test]
fn tokenize_words_splits_on_boundaries() {
    let tokens = tokenize_words("fn main() {");
    let words: Vec<&str> = tokens.iter().map(|r| &"fn main() {"[r.clone()]).collect();
    assert_eq!(words, vec!["fn", " ", "main", "()", " ", "{"]);
}

#[test]
fn tokenize_words_empty_input() {
    assert!(tokenize_words("").is_empty());
}

#[test]
fn tokenize_words_unicode() {
    let tokens = tokenize_words("café += 1");
    let words: Vec<&str> = tokens.iter().map(|r| &"café += 1"[r.clone()]).collect();
    assert_eq!(words, vec!["café", " ", "+=", " ", "1"]);
}

#[test]
fn compute_line_inline_diff_word_change() {
    let (old_ranges, new_ranges) =
        compute_line_inline_diff("let width = 240.0;", "let width = 300.0;");
    assert_eq!(old_ranges.len(), 1);
    assert_eq!(new_ranges.len(), 1);
    assert_eq!(&"let width = 240.0;"[old_ranges[0].clone()], "240");
    assert_eq!(&"let width = 300.0;"[new_ranges[0].clone()], "300");
}

#[test]
fn compute_line_inline_diff_identical_lines() {
    let (old_ranges, new_ranges) = compute_line_inline_diff("no change here", "no change here");
    assert!(old_ranges.is_empty());
    assert!(new_ranges.is_empty());
}

#[test]
fn compute_line_inline_diff_completely_different() {
    let (old_ranges, new_ranges) = compute_line_inline_diff("aaa", "zzz");
    assert_eq!(&"aaa"[old_ranges[0].clone()], "aaa");
    assert_eq!(&"zzz"[new_ranges[0].clone()], "zzz");
}

#[test]
fn attach_inline_changes_pairs_blocks() {
    let mut lines = vec![
        DiffLineView {
            kind: DiffLineKind::Context,
            old_lineno: Some(1),
            new_lineno: Some(1),
            text: "context\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Deletion,
            old_lineno: Some(2),
            new_lineno: None,
            text: "let x = 10;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(2),
            text: "let x = 20;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Context,
            old_lineno: Some(3),
            new_lineno: Some(3),
            text: "more context\n".into(),
            highlights: None,
            inline_changes: None,
        },
    ];

    attach_inline_changes(&mut lines);

    assert!(lines[0].inline_changes.is_none());
    assert!(lines[3].inline_changes.is_none());

    let del_changes = lines[1].inline_changes.as_ref().unwrap();
    let add_changes = lines[2].inline_changes.as_ref().unwrap();
    assert_eq!(&"let x = 10;\n"[del_changes[0].clone()], "10");
    assert_eq!(&"let x = 20;\n"[add_changes[0].clone()], "20");
}

#[test]
fn attach_inline_changes_unpaired_stays_none() {
    let mut lines = vec![DiffLineView {
        kind: DiffLineKind::Addition,
        old_lineno: None,
        new_lineno: Some(1),
        text: "brand new line\n".into(),
        highlights: None,
        inline_changes: None,
    }];
    attach_inline_changes(&mut lines);
    assert!(lines[0].inline_changes.is_none());
}

#[test]
fn added_event_kind_preserves_non_body_lines() {
    assert_eq!(
        added_event_kind(DiffLineKind::FileHeader),
        DiffLineKind::FileHeader
    );
    assert_eq!(
        added_event_kind(DiffLineKind::HunkHeader),
        DiffLineKind::HunkHeader
    );
    assert_eq!(
        added_event_kind(DiffLineKind::Context),
        DiffLineKind::Context
    );
    assert_eq!(
        added_event_kind(DiffLineKind::Addition),
        DiffLineKind::Addition
    );
    assert_eq!(
        added_event_kind(DiffLineKind::Deletion),
        DiffLineKind::Deletion
    );
}

#[test]
fn removed_event_kind_inverts_only_patch_body_lines() {
    assert_eq!(
        removed_event_kind(DiffLineKind::Addition),
        DiffLineKind::Deletion
    );
    assert_eq!(
        removed_event_kind(DiffLineKind::Deletion),
        DiffLineKind::Addition
    );
    assert_eq!(
        removed_event_kind(DiffLineKind::FileHeader),
        DiffLineKind::FileHeader
    );
    assert_eq!(
        removed_event_kind(DiffLineKind::HunkHeader),
        DiffLineKind::HunkHeader
    );
    assert_eq!(
        removed_event_kind(DiffLineKind::Context),
        DiffLineKind::Context
    );
}

#[test]
fn delta_view_line_from_added_preserves_relevant_line_numbers() {
    let added = DiffLineView {
        kind: DiffLineKind::Addition,
        old_lineno: None,
        new_lineno: Some(12),
        text: "+alpha".into(),
        highlights: None,
        inline_changes: None,
    };
    let deletion = DiffLineView {
        kind: DiffLineKind::Deletion,
        old_lineno: Some(8),
        new_lineno: None,
        text: "-beta".into(),
        highlights: None,
        inline_changes: None,
    };

    let added_event = delta_view_line_from_added(&added);
    assert_eq!(added_event.old_lineno, None);
    assert_eq!(added_event.new_lineno, Some(12));

    let deletion_event = delta_view_line_from_added(&deletion);
    assert_eq!(deletion_event.old_lineno, Some(8));
    assert_eq!(deletion_event.new_lineno, None);
}

#[test]
fn delta_view_line_from_removed_remaps_line_numbers_to_output_side() {
    let prior_addition = DiffLineView {
        kind: DiffLineKind::Addition,
        old_lineno: None,
        new_lineno: Some(21),
        text: "+gamma".into(),
        highlights: None,
        inline_changes: None,
    };
    let prior_deletion = DiffLineView {
        kind: DiffLineKind::Deletion,
        old_lineno: Some(34),
        new_lineno: None,
        text: "-delta".into(),
        highlights: None,
        inline_changes: None,
    };

    let removed_added_event = delta_view_line_from_removed(&prior_addition);
    assert_eq!(removed_added_event.kind, DiffLineKind::Deletion);
    assert_eq!(removed_added_event.old_lineno, Some(21));
    assert_eq!(removed_added_event.new_lineno, None);

    let removed_deletion_event = delta_view_line_from_removed(&prior_deletion);
    assert_eq!(removed_deletion_event.kind, DiffLineKind::Addition);
    assert_eq!(removed_deletion_event.old_lineno, None);
    assert_eq!(removed_deletion_event.new_lineno, Some(34));
}

#[test]
fn count_capture_changes_ignores_metadata_lines_in_event_deltas() {
    let capture = FeedCapturedEvent {
            files: vec![FeedEventFile {
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("docs/internal/delivery/Working-Set.md"),
                },
                file: ChangedFile {
                    relative_path: PathBuf::from("docs/internal/delivery/Working-Set.md"),
                    status: GitFileStatus::Modified,
                    is_binary: false,
                    insertions: 1,
                    deletions: 0,
                },
                document: FileDiffDocument {
                    generation: 2,
                    selection: DiffSelectionKey {
                        section: DiffSectionKind::Unstaged,
                        relative_path: PathBuf::from("docs/internal/delivery/Working-Set.md"),
                    },
                    file: ChangedFile {
                        relative_path: PathBuf::from("docs/internal/delivery/Working-Set.md"),
                        status: GitFileStatus::Modified,
                        is_binary: false,
                        insertions: 1,
                        deletions: 0,
                    },
                    lines: vec![
                        DiffLineView {
                            kind: DiffLineKind::FileHeader,
                            old_lineno: None,
                            new_lineno: None,
                            text: "diff --git a/docs/internal/delivery/Working-Set.md b/docs/internal/delivery/Working-Set.md".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                        DiffLineView {
                            kind: DiffLineKind::HunkHeader,
                            old_lineno: None,
                            new_lineno: None,
                            text: "@@ -8,4 +8,5 @@".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                        DiffLineView {
                            kind: DiffLineKind::Context,
                            old_lineno: Some(8),
                            new_lineno: Some(8),
                            text: "- **Tracked Between-Phase Work:** existing text".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                        DiffLineView {
                            kind: DiffLineKind::Addition,
                            old_lineno: None,
                            new_lineno: Some(9),
                            text: "+ **Live Feed Test Note:** Temporary text edit added on April 3, 2026 to exercise the delta-based live feed.".into(),
                            highlights: None,
                            inline_changes: None,
                        },
                    ],
                    hunks: Vec::new(),
                },
            }],
            failed_files: Vec::new(),
            truncated: false,
            total_rendered_lines: 4,
            total_rendered_bytes: 0,
        };

    assert_eq!(count_capture_changes(&capture), (1, 0));
}

#[test]
fn bootstrap_feed_event_counts_full_changes_even_when_capture_truncates() {
    let total_lines = FEED_EVENT_LINE_CAP + 5;
    let current = FeedScopeCapture {
        generation: 7,
        layers: vec![FeedLayerSnapshot {
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("src/live.rs"),
            },
            file: ChangedFile {
                relative_path: PathBuf::from("src/live.rs"),
                status: GitFileStatus::Modified,
                is_binary: false,
                insertions: total_lines,
                deletions: 0,
            },
            state: FeedLayerState::Ready(FileDiffDocument {
                generation: 7,
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/live.rs"),
                },
                file: ChangedFile {
                    relative_path: PathBuf::from("src/live.rs"),
                    status: GitFileStatus::Modified,
                    is_binary: false,
                    insertions: total_lines,
                    deletions: 0,
                },
                lines: (0..total_lines)
                    .map(|ix| DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some((ix + 1) as u32),
                        text: format!("added line {ix}\n"),
                        highlights: None,
                        inline_changes: None,
                    })
                    .collect(),
                hunks: Vec::new(),
            }),
        }],
    };

    let event = build_bootstrap_feed_event(&current);
    assert_eq!(event.insertions, total_lines);
    assert_eq!(event.deletions, 0);
    assert!(event.capture.truncated);
    assert_eq!(event.capture.total_rendered_lines, FEED_EVENT_LINE_CAP);
}

#[test]
fn live_delta_event_counts_full_changes_even_when_capture_truncates() {
    let total_lines = FEED_EVENT_LINE_CAP + 5;
    let previous = FeedScopeCapture {
        generation: 6,
        layers: Vec::new(),
    };
    let current = FeedScopeCapture {
        generation: 7,
        layers: vec![FeedLayerSnapshot {
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("src/live.rs"),
            },
            file: ChangedFile {
                relative_path: PathBuf::from("src/live.rs"),
                status: GitFileStatus::Modified,
                is_binary: false,
                insertions: total_lines,
                deletions: 0,
            },
            state: FeedLayerState::Ready(FileDiffDocument {
                generation: 7,
                selection: DiffSelectionKey {
                    section: DiffSectionKind::Unstaged,
                    relative_path: PathBuf::from("src/live.rs"),
                },
                file: ChangedFile {
                    relative_path: PathBuf::from("src/live.rs"),
                    status: GitFileStatus::Modified,
                    is_binary: false,
                    insertions: total_lines,
                    deletions: 0,
                },
                lines: (0..total_lines)
                    .map(|ix| DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some((ix + 1) as u32),
                        text: format!("added line {ix}\n"),
                        highlights: None,
                        inline_changes: None,
                    })
                    .collect(),
                hunks: Vec::new(),
            }),
        }],
    };

    let event = build_live_delta_event(&previous, &current, ThemeId::Dark)
        .unwrap()
        .unwrap();
    assert_eq!(event.insertions, total_lines);
    assert_eq!(event.deletions, 0);
    assert!(event.capture.truncated);
    assert_eq!(event.capture.total_rendered_lines, FEED_EVENT_LINE_CAP);
}

#[test]
fn live_delta_capture_file_counts_match_event_delta_not_whole_file() {
    let selection = DiffSelectionKey {
        section: DiffSectionKind::Unstaged,
        relative_path: PathBuf::from("src/live.rs"),
    };
    let previous = FeedScopeCapture {
        generation: 6,
        layers: vec![FeedLayerSnapshot {
            selection: selection.clone(),
            file: ChangedFile {
                relative_path: PathBuf::from("src/live.rs"),
                status: GitFileStatus::Modified,
                is_binary: false,
                insertions: 2,
                deletions: 0,
            },
            state: FeedLayerState::Ready(FileDiffDocument {
                generation: 6,
                selection: selection.clone(),
                file: ChangedFile {
                    relative_path: PathBuf::from("src/live.rs"),
                    status: GitFileStatus::Modified,
                    is_binary: false,
                    insertions: 2,
                    deletions: 0,
                },
                lines: vec![
                    DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(1),
                        text: "kept line 1\n".into(),
                        highlights: None,
                        inline_changes: None,
                    },
                    DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(2),
                        text: "kept line 2\n".into(),
                        highlights: None,
                        inline_changes: None,
                    },
                ],
                hunks: Vec::new(),
            }),
        }],
    };
    let current = FeedScopeCapture {
        generation: 7,
        layers: vec![FeedLayerSnapshot {
            selection: selection.clone(),
            file: ChangedFile {
                relative_path: PathBuf::from("src/live.rs"),
                status: GitFileStatus::Modified,
                is_binary: false,
                insertions: 3,
                deletions: 0,
            },
            state: FeedLayerState::Ready(FileDiffDocument {
                generation: 7,
                selection: selection.clone(),
                file: ChangedFile {
                    relative_path: PathBuf::from("src/live.rs"),
                    status: GitFileStatus::Modified,
                    is_binary: false,
                    insertions: 3,
                    deletions: 0,
                },
                lines: vec![
                    DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(1),
                        text: "kept line 1\n".into(),
                        highlights: None,
                        inline_changes: None,
                    },
                    DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(2),
                        text: "kept line 2\n".into(),
                        highlights: None,
                        inline_changes: None,
                    },
                    DiffLineView {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(3),
                        text: "new event line\n".into(),
                        highlights: None,
                        inline_changes: None,
                    },
                ],
                hunks: Vec::new(),
            }),
        }],
    };

    let event = build_live_delta_event(&previous, &current, ThemeId::Dark)
        .unwrap()
        .unwrap();
    let captured = event.capture.files.first().expect("captured file");

    assert_eq!(event.insertions, 1);
    assert_eq!(event.deletions, 0);
    assert_eq!(captured.file.insertions, 1);
    assert_eq!(captured.file.deletions, 0);
    assert_eq!(captured.document.file.insertions, 1);
    assert_eq!(captured.document.file.deletions, 0);
}

#[test]
fn attach_inline_changes_handles_reflowed_multiline_block() {
    let mut lines = vec![
            DiffLineView {
                kind: DiffLineKind::Deletion,
                old_lineno: Some(1),
                new_lineno: None,
                text: "        build_diff_tree, extract_selected_text, is_oversize_document, plain_text_for_line,\n"
                    .into(),
                highlights: None,
                inline_changes: None,
            },
            DiffLineView {
                kind: DiffLineKind::Deletion,
                old_lineno: Some(2),
                new_lineno: None,
                text: "        render_diff_text, selection_range_for_line, DiffSelection, DiffTreeNodeKind,\n"
                    .into(),
                highlights: None,
                inline_changes: None,
            },
            DiffLineView {
                kind: DiffLineKind::Addition,
                old_lineno: None,
                new_lineno: Some(1),
                text: "        build_diff_tree, extract_selected_text, is_oversize_document, map_raw_to_display_ranges,\n"
                    .into(),
                highlights: None,
                inline_changes: None,
            },
            DiffLineView {
                kind: DiffLineKind::Addition,
                old_lineno: None,
                new_lineno: Some(2),
                text: "        plain_text_for_line, render_diff_text, selection_range_for_line, DiffSelection,\n"
                    .into(),
                highlights: None,
                inline_changes: None,
            },
            DiffLineView {
                kind: DiffLineKind::Addition,
                old_lineno: None,
                new_lineno: Some(3),
                text: "        DiffTreeNodeKind,\n".into(),
                highlights: None,
                inline_changes: None,
            },
        ];

    attach_inline_changes(&mut lines);

    assert!(lines[0].inline_changes.is_none());
    assert!(lines[1].inline_changes.is_none());

    let inserted = lines[2].inline_changes.as_ref().unwrap();
    assert_eq!(inserted.len(), 1);
    assert!(
        lines[2].text[inserted[0].clone()].contains("map_raw_to_display_ranges"),
        "expected inserted token highlight, got {:?}",
        &lines[2].text[inserted[0].clone()]
    );

    assert!(lines[3].inline_changes.is_none());
    assert!(lines[4].inline_changes.is_none());
}

#[test]
fn attach_inline_changes_aligns_ordered_multiline_pairs() {
    let mut lines = vec![
        DiffLineView {
            kind: DiffLineKind::Deletion,
            old_lineno: Some(1),
            new_lineno: None,
            text: "let alpha = 10;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Deletion,
            old_lineno: Some(2),
            new_lineno: None,
            text: "let beta = 20;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Deletion,
            old_lineno: Some(3),
            new_lineno: None,
            text: "let gamma = 30;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(1),
            text: "let alpha = 11;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(2),
            text: "let beta = 21;\n".into(),
            highlights: None,
            inline_changes: None,
        },
        DiffLineView {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(3),
            text: "let gamma = 31;\n".into(),
            highlights: None,
            inline_changes: None,
        },
    ];

    attach_inline_changes(&mut lines);

    assert_eq!(
        &lines[0].text[lines[0].inline_changes.as_ref().unwrap()[0].clone()],
        "10"
    );
    assert_eq!(
        &lines[1].text[lines[1].inline_changes.as_ref().unwrap()[0].clone()],
        "20"
    );
    assert_eq!(
        &lines[2].text[lines[2].inline_changes.as_ref().unwrap()[0].clone()],
        "30"
    );
    assert_eq!(
        &lines[3].text[lines[3].inline_changes.as_ref().unwrap()[0].clone()],
        "11"
    );
    assert_eq!(
        &lines[4].text[lines[4].inline_changes.as_ref().unwrap()[0].clone()],
        "21"
    );
    assert_eq!(
        &lines[5].text[lines[5].inline_changes.as_ref().unwrap()[0].clone()],
        "31"
    );
}

#[test]
fn attach_inline_changes_skips_large_unbalanced_rewrite() {
    let mut lines = vec![DiffLineView {
        kind: DiffLineKind::Deletion,
        old_lineno: Some(1),
        new_lineno: None,
        text: "legacy compact paragraph\n".into(),
        highlights: None,
        inline_changes: None,
    }];

    for (idx, text) in [
        "new introduction line\n",
        "expanded rationale line\n",
        "usage details line\n",
        "installation details line\n",
        "configuration details line\n",
        "workflow example line\n",
        "review guidance line\n",
        "testing guidance line\n",
        "deployment guidance line\n",
        "closing note line\n",
    ]
    .into_iter()
    .enumerate()
    {
        lines.push(DiffLineView {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some((idx + 1) as u32),
            text: text.into(),
            highlights: None,
            inline_changes: None,
        });
    }

    attach_inline_changes(&mut lines);

    assert!(lines[0].inline_changes.is_none());
    for line in &lines[1..] {
        assert!(
            line.inline_changes.is_none(),
            "expected unmatched rewrite line to have no inline changes: {:?}",
            line.text
        );
    }
}

#[test]
fn compute_block_inline_diff_keeps_punctuation_separated_ranges_distinct() {
    let (old_ranges, new_ranges) =
        compute_block_inline_diff(&["alpha, beta, gamma\n"], &["delta, epsilon, zeta\n"], true);

    assert_eq!(
        old_ranges,
        vec![vec![0..5, 7..11, 13..18]],
        "old-side ranges should not bridge commas"
    );
    assert_eq!(
        new_ranges,
        vec![vec![0..5, 7..14, 16..20]],
        "new-side ranges should not bridge commas"
    );
}
