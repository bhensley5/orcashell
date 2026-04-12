use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Commit, Delta, Diff, DiffFindOptions, DiffLineType,
    DiffOptions, ErrorCode, MergeOptions, ObjectType, Patch, Reference, Repository,
    RepositoryState, ResetType, Sort, StashFlags, Status, StatusOptions, StatusShow,
    WorktreeAddOptions, WorktreePruneOptions,
};

pub use git2::Oid;
use orcashell_store::ThemeId;
pub use orcashell_syntax::HighlightedSpan;

pub const ORCASHELL_EXCLUDE_ENTRY: &str = "/.orcashell/";
pub const MAX_RENDERED_DIFF_LINES: usize = 10_000;
pub const MAX_RENDERED_DIFF_BYTES: usize = 1024 * 1024;
pub const OVERSIZE_DIFF_MESSAGE: &str = "Diff too large to render in OrcaShell";
pub const BINARY_DIFF_MESSAGE: &str = "Binary file; diff body unavailable";
pub const FEED_EVENT_FILE_CAP: usize = 16;
pub const FEED_EVENT_LINE_CAP: usize = 2_000;
pub const FEED_EVENT_BYTE_CAP: usize = 256 * 1024;
pub const MAX_GRAPH_COMMITS: usize = 1_500;

// ── Core data types ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitScope {
    pub repo_root: PathBuf,
    pub scope_root: PathBuf,
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSnapshotSummary {
    pub repo_root: PathBuf,
    pub scope_root: PathBuf,
    pub generation: u64,
    pub content_fingerprint: u64,
    pub branch_name: String,
    pub remotes: Vec<String>,
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
    pub changed_files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotLoadErrorKind {
    NotRepository,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLoadError {
    kind: SnapshotLoadErrorKind,
    message: String,
}

impl SnapshotLoadError {
    pub fn not_repository(message: impl Into<String>) -> Self {
        Self {
            kind: SnapshotLoadErrorKind::NotRepository,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: SnapshotLoadErrorKind::Unavailable,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> SnapshotLoadErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SnapshotLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SnapshotLoadError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Typechange,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub relative_path: PathBuf,
    pub status: GitFileStatus,
    pub is_binary: bool,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    FileHeader,
    HunkHeader,
    Context,
    Addition,
    Deletion,
    BinaryNotice,
    ConflictMarker,
    ConflictOurs,
    ConflictBase,
    ConflictTheirs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLineView {
    pub kind: DiffLineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub text: String,
    /// Syntax-highlighted spans. `None` for non-code lines (headers, binary notices)
    /// or if highlighting is unavailable for this file type.
    pub highlights: Option<Vec<orcashell_syntax::HighlightedSpan>>,
    /// Byte ranges within `text` marking inline word-level changes. Computed by
    /// diffing paired deletion/addition lines. `None` for unpaired or non-code lines.
    pub inline_changes: Option<Vec<Range<usize>>>,
}

// ── Phase 4.5 types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffSectionKind {
    Conflicted,
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffSelectionKey {
    pub section: DiffSectionKind,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTrackingStatus {
    pub upstream_ref: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    AlreadyMerged,
    FastForward {
        new_head: Oid,
    },
    MergeCommit {
        merge_oid: Oid,
    },
    Conflicted {
        affected_scope: PathBuf,
        conflicted_files: Vec<PathBuf>,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInfo {
    pub remote: String,
    pub upstream_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchCheckoutOutcome {
    SwitchedLocal {
        branch_name: String,
    },
    SwitchedTracking {
        local_branch_name: String,
        remote_full_ref: String,
        created: bool,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateLocalBranchOutcome {
    CreatedAndCheckedOut {
        source_branch_name: String,
        branch_name: String,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteLocalBranchOutcome {
    Deleted { branch_name: String },
    Blocked { reason: String },
}

// ── Repository graph documents ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryGraphDocument {
    pub scope_root: PathBuf,
    pub repo_root: PathBuf,
    pub head: HeadState,
    pub local_branches: Vec<LocalBranchEntry>,
    pub remote_branches: Vec<RemoteBranchEntry>,
    pub commits: Vec<CommitGraphNode>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    Branch { name: String, oid: Oid },
    Detached { oid: Oid },
    Unborn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranchEntry {
    pub name: String,
    pub full_ref: String,
    pub target: Oid,
    pub is_head: bool,
    pub upstream: Option<BranchTrackingInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTrackingInfo {
    pub remote_name: String,
    pub remote_ref: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranchEntry {
    pub remote_name: String,
    pub short_name: String,
    pub full_ref: String,
    pub target: Oid,
    pub tracked_by_local: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitGraphNode {
    pub oid: Oid,
    pub short_oid: String,
    pub summary: String,
    pub author_name: String,
    pub authored_at_unix: i64,
    pub parent_oids: Vec<Oid>,
    pub primary_lane: u16,
    pub row_lanes: Vec<GraphLaneSegment>,
    pub ref_labels: Vec<CommitRefLabel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLaneSegment {
    pub lane: u16,
    pub kind: GraphLaneKind,
    pub target_lane: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphLaneKind {
    Through,
    Start,
    End,
    MergeFromLeft,
    MergeFromRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRefLabel {
    pub name: String,
    pub kind: CommitRefKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitRefKind {
    Head,
    LocalBranch,
    RemoteBranch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetailDocument {
    pub oid: Oid,
    pub short_oid: String,
    pub summary: String,
    pub message_body: String,
    pub author_name: String,
    pub author_email: String,
    pub authored_at_unix: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub committed_at_unix: i64,
    pub parent_oids: Vec<Oid>,
    pub changed_files: Vec<CommitChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitChangedFile {
    pub path: PathBuf,
    pub status: CommitFileStatus,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
    Typechange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFileSelection {
    pub commit_oid: Oid,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFileDiffDocument {
    pub commit_oid: Oid,
    pub parent_oid: Option<Oid>,
    pub selection: CommitFileSelection,
    pub file: ChangedFile,
    pub lines: Vec<DiffLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashListDocument {
    pub scope_root: PathBuf,
    pub repo_root: PathBuf,
    pub entries: Vec<StashEntrySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntrySummary {
    pub stash_oid: Oid,
    pub stash_index: usize,
    pub label: String,
    pub message: String,
    pub committed_at_unix: i64,
    pub includes_untracked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashDetailDocument {
    pub stash_oid: Oid,
    pub label: String,
    pub message: String,
    pub committed_at_unix: i64,
    pub includes_untracked: bool,
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFileSelection {
    pub stash_oid: Oid,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashFileDiffDocument {
    pub stash_oid: Oid,
    pub selection: StashFileSelection,
    pub file: ChangedFile,
    pub lines: Vec<DiffLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashMutationOutcome {
    Applied {
        label: String,
    },
    Conflicted {
        affected_scope: PathBuf,
        conflicted_files: Vec<PathBuf>,
    },
}

// ── Diff documents ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffDocument {
    pub generation: u64,
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub lines: Vec<DiffLineView>,
    pub hunks: Vec<FileDiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffHunk {
    pub hunk_index: usize,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
    pub body_fingerprint: u64,
    /// Inclusive start index in the diff pane's filtered line space
    /// (file-header rows excluded).
    pub line_start: usize,
    /// Exclusive end index in the diff pane's filtered line space.
    pub line_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardHunkTarget {
    pub hunk_index: usize,
    pub body_fingerprint: u64,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscardMutationOutcome {
    Applied,
    Blocked { reason: String },
}

impl FileDiffDocument {
    pub fn discard_hunk_target(&self, hunk_index: usize) -> Option<DiscardHunkTarget> {
        discard_hunk_target_from_lines(&self.lines, &self.hunks, hunk_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeState {
    pub can_complete: bool,
    pub can_abort: bool,
    pub conflicted_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDocument {
    pub snapshot: GitSnapshotSummary,
    pub tracking: GitTrackingStatus,
    pub merge_state: Option<MergeState>,
    pub repo_state_warning: Option<String>,
    pub conflicted_files: Vec<ChangedFile>,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConflictFile {
    pub raw_text: String,
    pub blocks: Vec<ParsedConflictBlock>,
    pub has_base_sections: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConflictBlock {
    pub block_index: usize,
    pub ours: Range<usize>,
    pub base: Option<Range<usize>>,
    pub theirs: Range<usize>,
    pub whole_block: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedEventKind {
    BootstrapSnapshot,
    LiveDelta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEventFileSummary {
    pub relative_path: PathBuf,
    pub staged: bool,
    pub unstaged: bool,
    pub status: GitFileStatus,
    pub is_binary: bool,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEventFile {
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub document: FileDiffDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEventFailure {
    pub selection: DiffSelectionKey,
    pub relative_path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCapturedEvent {
    pub files: Vec<FeedEventFile>,
    pub failed_files: Vec<FeedEventFailure>,
    pub truncated: bool,
    pub total_rendered_lines: usize,
    pub total_rendered_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedLayerState {
    Ready(FileDiffDocument),
    Unavailable { message: String },
}

impl FeedLayerState {
    /// Compare two layer states by content only, ignoring the `generation` field
    /// on `FileDiffDocument`. This prevents every dirty file from appearing as
    /// "changed" in live delta events just because the snapshot generation advanced.
    fn content_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(a), Self::Ready(b)) => {
                a.selection == b.selection && a.file == b.file && a.lines == b.lines
            }
            (Self::Unavailable { message: a }, Self::Unavailable { message: b }) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLayerSnapshot {
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub state: FeedLayerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedScopeCapture {
    pub generation: u64,
    pub layers: Vec<FeedLayerSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEventData {
    pub kind: FeedEventKind,
    pub changed_file_count: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<FeedEventFileSummary>,
    pub capture: FeedCapturedEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCaptureResult {
    pub current_capture: FeedScopeCapture,
    pub event: Option<FeedEventData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWorktree {
    pub id: String,
    pub branch_name: String,
    pub worktree_name: String,
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub source_ref: String,
}

// ── Internal discovery ───────────────────────────────────────────────

struct DiscoveredRepo {
    repo: Repository,
    scope: GitScope,
    shared_admin_dir: PathBuf,
}

pub fn discover_scope(path: &Path) -> Result<GitScope> {
    Ok(discover_repo(path)?.scope)
}

// ── Snapshot and diff index loading ──────────────────────────────────

pub fn load_snapshot(
    path: &Path,
    generation: u64,
) -> std::result::Result<GitSnapshotSummary, SnapshotLoadError> {
    let discovered = discover_repo(path)
        .map_err(|error| SnapshotLoadError::not_repository(error.to_string()))?;
    let staged_diff = build_staged_diff(&discovered.repo)
        .map_err(|error| SnapshotLoadError::unavailable(error.to_string()))?;
    let unstaged_diff = build_unstaged_diff(&discovered.repo)
        .map_err(|error| SnapshotLoadError::unavailable(error.to_string()))?;
    let staged_files = collect_changed_files(&staged_diff, None)
        .map_err(|error| SnapshotLoadError::unavailable(error.to_string()))?;
    let unstaged_files = collect_changed_files(&unstaged_diff, None)
        .map_err(|error| SnapshotLoadError::unavailable(error.to_string()))?;
    snapshot_summary_from_split(
        &discovered.scope,
        &discovered.repo,
        &staged_diff,
        &unstaged_diff,
        &staged_files,
        &unstaged_files,
        generation,
    )
    .map_err(|error| SnapshotLoadError::unavailable(error.to_string()))
}

pub fn load_diff_index(path: &Path, generation: u64) -> Result<DiffDocument> {
    let discovered = discover_repo(path)?;
    let staged_diff = build_staged_diff(&discovered.repo)?;
    let unstaged_diff = build_unstaged_diff(&discovered.repo)?;
    let conflicted_paths = collect_conflicted_paths(&discovered.repo)?;
    let conflicted_path_set = conflicted_paths.iter().cloned().collect::<HashSet<_>>();
    let conflicted_files = conflicted_paths
        .into_iter()
        .map(conflicted_changed_file)
        .collect::<Vec<_>>();
    let staged_files = collect_changed_files(&staged_diff, Some(&conflicted_path_set))?;
    let unstaged_files = collect_changed_files(&unstaged_diff, Some(&conflicted_path_set))?;
    let tracking = compute_tracking_status(&discovered.repo)?;
    let repo_state = discovered.repo.state();
    let merge_state = match repo_state {
        RepositoryState::Merge => Some(MergeState {
            can_complete: conflicted_files.is_empty() && unstaged_files.is_empty(),
            can_abort: true,
            conflicted_file_count: conflicted_files.len(),
        }),
        _ => None,
    };
    let snapshot = snapshot_summary_from_split(
        &discovered.scope,
        &discovered.repo,
        &staged_diff,
        &unstaged_diff,
        &staged_files,
        &unstaged_files,
        generation,
    )?;

    Ok(DiffDocument {
        snapshot,
        tracking,
        merge_state,
        repo_state_warning: repo_state_warning(repo_state),
        conflicted_files,
        staged_files,
        unstaged_files,
    })
}

pub fn load_file_diff(
    path: &Path,
    generation: u64,
    selection: &DiffSelectionKey,
    theme_id: ThemeId,
) -> Result<FileDiffDocument> {
    let discovered = discover_repo(path)?;
    let diff = match selection.section {
        DiffSectionKind::Conflicted => {
            bail!("conflicted file diff view is not available via the normal diff loader yet");
        }
        DiffSectionKind::Staged => build_staged_diff(&discovered.repo)?,
        DiffSectionKind::Unstaged => build_unstaged_diff(&discovered.repo)?,
    };

    let include_hunks = selection.section == DiffSectionKind::Unstaged;
    for (idx, delta) in diff.deltas().enumerate() {
        let relative_path =
            delta_path(&delta).context("diff delta did not contain a path for file diff")?;
        if relative_path != selection.relative_path {
            continue;
        }

        let rendered = render_selected_diff_entry(&diff, idx, delta, include_hunks)
            .with_context(|| format!("failed to render file diff entry at diff index {idx}"))?;
        let mut lines = rendered.lines;
        let candidate = rendered.file;
        let hunks = rendered.hunks;

        if !candidate.is_binary {
            attach_syntax_highlights(&mut lines, &selection.relative_path, theme_id);
            attach_inline_changes(&mut lines);
        }

        return Ok(FileDiffDocument {
            generation,
            selection: selection.clone(),
            file: candidate,
            lines,
            hunks,
        });
    }

    Err(anyhow!(
        "diff entry not found for {:?} path {}",
        selection.section,
        selection.relative_path.display()
    ))
}

pub fn load_repository_graph(path: &Path) -> Result<RepositoryGraphDocument> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;
    let head = resolve_head_state(repo)?;
    let local_branches = load_local_branch_entries(repo)?;
    let remote_branches = load_remote_branch_entries(repo, &local_branches)?;
    let ref_labels = build_commit_ref_labels(&head, &local_branches, &remote_branches);
    let (commits, truncated) = load_commit_graph_nodes(repo, &head, &ref_labels)?;

    Ok(RepositoryGraphDocument {
        scope_root: discovered.scope.scope_root,
        repo_root: discovered.scope.repo_root,
        head,
        local_branches,
        remote_branches,
        commits,
        truncated,
    })
}

pub fn load_commit_detail(path: &Path, oid: Oid) -> Result<CommitDetailDocument> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;
    let commit = repo
        .find_commit(oid)
        .with_context(|| format!("failed to find commit {}", short_oid(oid)))?;
    let parent_oids = commit_parent_oids(&commit)?;

    let commit_tree = commit.tree().context("failed to read commit tree")?;
    let first_parent = if commit.parent_count() > 0 {
        Some(
            commit
                .parent(0)
                .context("failed to read first parent commit")?,
        )
    } else {
        None
    };
    let parent_tree = first_parent
        .as_ref()
        .map(|parent| parent.tree().context("failed to read parent commit tree"))
        .transpose()?;

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).ignore_submodules(true);

    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(&mut opts))
        .context("failed to diff commit against its first parent")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on commit detail diff")?;

    let mut changed_files = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        changed_files.push(
            commit_changed_file_from_delta(&diff, idx, delta).with_context(|| {
                format!(
                    "failed to build commit detail entry for {} at diff index {idx}",
                    short_oid(oid)
                )
            })?,
        );
    }
    changed_files.sort_by(|left, right| left.path.cmp(&right.path));

    let author = commit.author();
    let committer = commit.committer();
    Ok(CommitDetailDocument {
        oid,
        short_oid: short_oid(oid),
        summary: commit.summary().unwrap_or("").to_string(),
        message_body: commit.body().unwrap_or("").to_string(),
        author_name: author.name().unwrap_or("").to_string(),
        author_email: author.email().unwrap_or("").to_string(),
        authored_at_unix: author.when().seconds(),
        committer_name: committer.name().unwrap_or("").to_string(),
        committer_email: committer.email().unwrap_or("").to_string(),
        committed_at_unix: committer.when().seconds(),
        parent_oids,
        changed_files,
    })
}

pub fn load_commit_file_diff(
    path: &Path,
    commit_oid: Oid,
    relative_path: &Path,
    theme_id: ThemeId,
) -> Result<CommitFileDiffDocument> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;
    let commit = repo
        .find_commit(commit_oid)
        .with_context(|| format!("failed to find commit {}", short_oid(commit_oid)))?;

    let commit_tree = commit.tree().context("failed to read commit tree")?;
    let first_parent = if commit.parent_count() > 0 {
        Some(
            commit
                .parent(0)
                .context("failed to read first parent commit")?,
        )
    } else {
        None
    };
    let parent_oid = first_parent.as_ref().map(|parent| parent.id());
    let parent_tree = first_parent
        .as_ref()
        .map(|parent| parent.tree().context("failed to read parent commit tree"))
        .transpose()?;

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).ignore_submodules(true);

    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(&mut opts))
        .context("failed to diff commit against its first parent")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on commit file diff")?;

    for (idx, delta) in diff.deltas().enumerate() {
        let candidate = changed_file_from_delta(&diff, idx, delta)
            .with_context(|| format!("failed to build changed-file entry at diff index {idx}"))?;
        if candidate.relative_path != relative_path {
            continue;
        }

        let mut lines = render_diff_lines(&diff, idx, &candidate)?;
        if !candidate.is_binary {
            attach_syntax_highlights(&mut lines, &candidate.relative_path, theme_id);
            attach_inline_changes(&mut lines);
        }

        return Ok(CommitFileDiffDocument {
            commit_oid,
            parent_oid,
            selection: CommitFileSelection {
                commit_oid,
                relative_path: relative_path.to_path_buf(),
            },
            file: candidate,
            lines,
        });
    }

    Err(anyhow!(
        "commit diff entry not found for {} path {}",
        short_oid(commit_oid),
        relative_path.display()
    ))
}

pub fn list_stashes(path: &Path) -> Result<StashListDocument> {
    let mut discovered = discover_repo(path)?;
    let entries = load_stash_entry_summaries(&mut discovered.repo)?;
    Ok(StashListDocument {
        scope_root: discovered.scope.scope_root,
        repo_root: discovered.scope.repo_root,
        entries,
    })
}

pub fn load_stash_detail(path: &Path, stash_oid: Oid) -> Result<StashDetailDocument> {
    let mut discovered = discover_repo(path)?;
    let repo = &mut discovered.repo;
    let entry = resolve_stash_entry(repo, stash_oid)?;
    let stash_commit = repo
        .find_commit(stash_oid)
        .with_context(|| format!("failed to find stash {}", short_oid(stash_oid)))?;
    let files = load_stash_changed_files(repo, &stash_commit)?;

    Ok(StashDetailDocument {
        stash_oid,
        label: entry.label,
        message: entry.message,
        committed_at_unix: entry.committed_at_unix,
        includes_untracked: entry.includes_untracked,
        files,
    })
}

pub fn load_stash_file_diff(
    path: &Path,
    stash_oid: Oid,
    relative_path: &Path,
    theme_id: ThemeId,
) -> Result<StashFileDiffDocument> {
    let mut discovered = discover_repo(path)?;
    let repo = &mut discovered.repo;
    let stash_commit = repo
        .find_commit(stash_oid)
        .with_context(|| format!("failed to find stash {}", short_oid(stash_oid)))?;

    if let Some(rendered) = render_stash_tracked_target(repo, &stash_commit, relative_path)? {
        return Ok(build_stash_file_diff_document(
            stash_oid,
            relative_path,
            rendered,
            theme_id,
        ));
    }

    if let Some(rendered) = render_stash_untracked_target(repo, &stash_commit, relative_path)? {
        return Ok(build_stash_file_diff_document(
            stash_oid,
            relative_path,
            rendered,
            theme_id,
        ));
    }

    Err(anyhow!(
        "stash diff entry not found for {} path {}",
        short_oid(stash_oid),
        relative_path.display()
    ))
}

pub fn create_stash(
    path: &Path,
    message: Option<&str>,
    keep_index: bool,
    include_untracked: bool,
) -> Result<Oid> {
    let mut discovered = discover_repo(path)?;
    let repo = &mut discovered.repo;
    let signature = repo
        .signature()
        .context("failed to resolve repository signature for stash")?;
    let trimmed_message = message.map(str::trim).filter(|message| !message.is_empty());
    let mut flags = StashFlags::empty();
    if keep_index {
        flags |= StashFlags::KEEP_INDEX;
    }
    if include_untracked {
        flags |= StashFlags::INCLUDE_UNTRACKED;
    }
    repo.stash_save2(&signature, trimmed_message, Some(flags))
        .context("failed to create stash")
}

pub fn apply_stash(path: &Path, stash_oid: Oid) -> Result<StashMutationOutcome> {
    apply_stash_internal(path, stash_oid)
}

pub fn pop_stash(path: &Path, stash_oid: Oid) -> Result<StashMutationOutcome> {
    pop_stash_internal(path, stash_oid)
}

pub fn drop_stash(path: &Path, stash_oid: Oid) -> Result<String> {
    let mut discovered = discover_repo(path)?;
    let target = resolve_raw_stash_entry(&mut discovered.repo, stash_oid)?;
    discovered
        .repo
        .stash_drop(target.stash_index)
        .with_context(|| format!("failed to drop {}", target.label()))?;
    Ok(target.label())
}

pub fn capture_feed_event(
    path: &Path,
    generation: u64,
    previous: Option<&FeedScopeCapture>,
    theme_id: ThemeId,
) -> Result<FeedCaptureResult> {
    let discovered = discover_repo(path)?;
    let staged_diff = build_staged_diff(&discovered.repo)?;
    let unstaged_diff = build_unstaged_diff(&discovered.repo)?;
    let current_capture =
        capture_feed_scope_from_split(&staged_diff, &unstaged_diff, generation, theme_id)?;
    let event = derive_feed_event(previous, &current_capture, theme_id)?;
    Ok(FeedCaptureResult {
        current_capture,
        event,
    })
}

pub fn rehighlight_file_diff_document(document: &mut FileDiffDocument, theme_id: ThemeId) {
    for line in &mut document.lines {
        line.highlights = None;
    }
    attach_syntax_highlights(
        &mut document.lines,
        &document.selection.relative_path,
        theme_id,
    );
}

pub fn rehighlight_captured_feed_event(captured: &mut FeedCapturedEvent, theme_id: ThemeId) {
    for file in &mut captured.files {
        rehighlight_file_diff_document(&mut file.document, theme_id);
    }
}

pub fn parse_conflict_file_text(text: &str) -> Result<ParsedConflictFile> {
    let lines = split_lines_inclusive(text);
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    let mut line_index = 0usize;
    let mut has_base_sections = false;

    while line_index < lines.len() {
        let line = lines[line_index];
        if !line.starts_with("<<<<<<< ") {
            cursor += line.len();
            line_index += 1;
            continue;
        }

        let block_start = cursor;
        cursor += line.len();
        line_index += 1;

        let ours_start = cursor;
        while line_index < lines.len()
            && !lines[line_index].starts_with("||||||| ")
            && !lines[line_index].starts_with("=======")
        {
            cursor += lines[line_index].len();
            line_index += 1;
        }
        let ours_end = cursor;

        let mut base = None;
        if line_index < lines.len() && lines[line_index].starts_with("||||||| ") {
            has_base_sections = true;
            cursor += lines[line_index].len();
            line_index += 1;
            let base_start = cursor;
            while line_index < lines.len() && !lines[line_index].starts_with("=======") {
                cursor += lines[line_index].len();
                line_index += 1;
            }
            base = Some(base_start..cursor);
        }

        if line_index >= lines.len() || !lines[line_index].starts_with("=======") {
            bail!("invalid conflict marker set: missing separator");
        }
        cursor += lines[line_index].len();
        line_index += 1;

        let theirs_start = cursor;
        while line_index < lines.len() && !lines[line_index].starts_with(">>>>>>> ") {
            cursor += lines[line_index].len();
            line_index += 1;
        }
        let theirs_end = cursor;

        if line_index >= lines.len() || !lines[line_index].starts_with(">>>>>>> ") {
            bail!("invalid conflict marker set: missing end marker");
        }
        cursor += lines[line_index].len();
        line_index += 1;

        blocks.push(ParsedConflictBlock {
            block_index: blocks.len(),
            ours: ours_start..ours_end,
            base,
            theirs: theirs_start..theirs_end,
            whole_block: block_start..cursor,
        });
    }

    Ok(ParsedConflictFile {
        raw_text: text.to_string(),
        blocks,
        has_base_sections,
    })
}

// ── Repository graph helpers ────────────────────────────────────────

fn resolve_head_state(repo: &Repository) -> Result<HeadState> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(err) if matches!(err.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
            return Ok(HeadState::Unborn);
        }
        Err(err) => return Err(err.into()),
    };

    let Some(oid) = head.target() else {
        return Ok(HeadState::Unborn);
    };

    if repo
        .head_detached()
        .context("failed to inspect HEAD detached state")?
    {
        return Ok(HeadState::Detached { oid });
    }

    let name = head
        .shorthand()
        .map(str::to_owned)
        .or_else(|| head.name().map(str::to_owned))
        .unwrap_or_else(|| "HEAD".to_string());
    Ok(HeadState::Branch { name, oid })
}

fn load_local_branch_entries(repo: &Repository) -> Result<Vec<LocalBranchEntry>> {
    let mut entries = Vec::new();
    let branches = repo
        .branches(Some(BranchType::Local))
        .context("failed to enumerate local branches")?;

    for branch in branches {
        let (branch, _) = branch.context("failed to read local branch")?;
        let Some(target) = branch.get().target() else {
            continue;
        };

        let name = branch
            .name()
            .context("failed to read local branch name")?
            .context("local branch missing UTF-8 name")?
            .to_string();
        let full_ref = branch
            .get()
            .name()
            .context("local branch missing full ref name")?
            .to_string();
        let is_head = branch.is_head();
        let upstream = branch_tracking_info(repo, &branch, target)?;

        entries.push(LocalBranchEntry {
            name,
            full_ref,
            target,
            is_head,
            upstream,
        });
    }

    entries.sort_by(|left, right| {
        right
            .is_head
            .cmp(&left.is_head)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

fn branch_tracking_info(
    repo: &Repository,
    branch: &git2::Branch<'_>,
    local_oid: Oid,
) -> Result<Option<BranchTrackingInfo>> {
    let upstream = match branch.upstream() {
        Ok(upstream) => upstream,
        Err(err) if err.code() == ErrorCode::NotFound => return Ok(None),
        Err(err) => return Err(err).context("failed to resolve branch upstream"),
    };

    let remote_ref = upstream
        .get()
        .name()
        .context("upstream ref missing full ref name")?
        .to_string();
    let Some((remote_name, short_remote_ref)) = parse_remote_tracking_ref(&remote_ref) else {
        return Ok(None);
    };
    let upstream_oid = upstream
        .get()
        .target()
        .context("upstream tracking ref has no target")?;
    let (ahead, behind) = repo
        .graph_ahead_behind(local_oid, upstream_oid)
        .context("failed to compute ahead/behind counts")?;

    Ok(Some(BranchTrackingInfo {
        remote_name,
        remote_ref: short_remote_ref,
        ahead,
        behind,
    }))
}

fn load_remote_branch_entries(
    repo: &Repository,
    local_branches: &[LocalBranchEntry],
) -> Result<Vec<RemoteBranchEntry>> {
    let mut tracked_by_local = HashMap::<String, String>::new();
    for branch in local_branches {
        let Some(upstream) = branch.upstream.as_ref() else {
            continue;
        };
        let tracked_remote_ref = format!(
            "refs/remotes/{}/{}",
            upstream.remote_name, upstream.remote_ref
        );
        tracked_by_local
            .entry(tracked_remote_ref)
            .and_modify(|tracked| {
                if branch.name < *tracked {
                    *tracked = branch.name.clone();
                }
            })
            .or_insert_with(|| branch.name.clone());
    }

    let mut entries = Vec::new();
    let refs = repo
        .references_glob("refs/remotes/*")
        .context("failed to enumerate remote-tracking refs")?;

    for reference in refs {
        let reference = reference.context("failed to read remote-tracking ref")?;
        if reference.symbolic_target().is_some() {
            continue;
        }

        let Some(full_ref) = reference.name() else {
            continue;
        };
        let Some((remote_name, short_name)) = parse_remote_tracking_ref(full_ref) else {
            continue;
        };
        if short_name == "HEAD" {
            continue;
        }
        let Some(target) = reference.target() else {
            continue;
        };

        entries.push(RemoteBranchEntry {
            remote_name,
            short_name,
            full_ref: full_ref.to_string(),
            target,
            tracked_by_local: tracked_by_local.get(full_ref).cloned(),
        });
    }

    entries.sort_by(|left, right| {
        left.remote_name
            .cmp(&right.remote_name)
            .then_with(|| left.short_name.cmp(&right.short_name))
    });
    Ok(entries)
}

fn parse_remote_tracking_ref(full_ref: &str) -> Option<(String, String)> {
    let shorthand = full_ref.strip_prefix("refs/remotes/")?;
    let (remote_name, short_name) = shorthand.split_once('/')?;
    Some((remote_name.to_string(), short_name.to_string()))
}

fn build_commit_ref_labels(
    head: &HeadState,
    local_branches: &[LocalBranchEntry],
    remote_branches: &[RemoteBranchEntry],
) -> HashMap<Oid, Vec<CommitRefLabel>> {
    let mut labels = HashMap::<Oid, Vec<CommitRefLabel>>::new();

    match head {
        HeadState::Branch { oid, .. } | HeadState::Detached { oid } => insert_commit_ref_label(
            &mut labels,
            *oid,
            CommitRefLabel {
                name: "HEAD".to_string(),
                kind: CommitRefKind::Head,
            },
        ),
        HeadState::Unborn => {}
    }

    for branch in local_branches {
        insert_commit_ref_label(
            &mut labels,
            branch.target,
            CommitRefLabel {
                name: branch.name.clone(),
                kind: CommitRefKind::LocalBranch,
            },
        );
    }

    for branch in remote_branches {
        insert_commit_ref_label(
            &mut labels,
            branch.target,
            CommitRefLabel {
                name: format!("{}/{}", branch.remote_name, branch.short_name),
                kind: CommitRefKind::RemoteBranch,
            },
        );
    }

    for labels_for_commit in labels.values_mut() {
        labels_for_commit.sort_by(|left, right| {
            commit_ref_kind_sort_key(left.kind)
                .cmp(&commit_ref_kind_sort_key(right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
    }

    labels
}

fn insert_commit_ref_label(
    labels: &mut HashMap<Oid, Vec<CommitRefLabel>>,
    oid: Oid,
    label: CommitRefLabel,
) {
    labels.entry(oid).or_default().push(label);
}

fn commit_ref_kind_sort_key(kind: CommitRefKind) -> u8 {
    match kind {
        CommitRefKind::Head => 0,
        CommitRefKind::LocalBranch => 1,
        CommitRefKind::RemoteBranch => 2,
    }
}

fn load_commit_graph_nodes(
    repo: &Repository,
    head: &HeadState,
    ref_labels: &HashMap<Oid, Vec<CommitRefLabel>>,
) -> Result<(Vec<CommitGraphNode>, bool)> {
    let mut tip_oids = Vec::new();
    let mut seen = HashSet::new();

    if let HeadState::Detached { oid } = head {
        if seen.insert(*oid) {
            tip_oids.push(*oid);
        }
    }

    for oid in ref_labels.keys().copied() {
        if seen.insert(oid) {
            tip_oids.push(oid);
        }
    }

    if tip_oids.is_empty() {
        return Ok((Vec::new(), false));
    }

    let mut revwalk = repo.revwalk().context("failed to start git revwalk")?;
    revwalk
        .set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .context("failed to configure git revwalk sorting")?;
    for oid in tip_oids {
        revwalk
            .push(oid)
            .with_context(|| format!("failed to push revwalk tip {}", short_oid(oid)))?;
    }

    let mut commit_oids = Vec::new();
    let mut truncated = false;
    for next in revwalk {
        let oid = next.context("failed to walk repository history")?;
        commit_oids.push(oid);
        if commit_oids.len() > MAX_GRAPH_COMMITS {
            truncated = true;
            commit_oids.truncate(MAX_GRAPH_COMMITS);
            break;
        }
    }

    let mut commits = Vec::with_capacity(commit_oids.len());
    for oid in commit_oids {
        let commit = repo
            .find_commit(oid)
            .with_context(|| format!("failed to load commit {}", short_oid(oid)))?;
        let author = commit.author();
        commits.push(CommitGraphNode {
            oid,
            short_oid: short_oid(oid),
            summary: commit.summary().unwrap_or("").to_string(),
            author_name: author.name().unwrap_or("").to_string(),
            authored_at_unix: author.when().seconds(),
            parent_oids: commit_parent_oids(&commit)?,
            primary_lane: 0,
            row_lanes: Vec::new(),
            ref_labels: ref_labels.get(&oid).cloned().unwrap_or_default(),
        });
    }

    populate_graph_lane_segments(&mut commits);
    Ok((commits, truncated))
}

fn commit_parent_oids(commit: &Commit<'_>) -> Result<Vec<Oid>> {
    let mut parent_oids = Vec::with_capacity(commit.parent_count());
    for index in 0..commit.parent_count() {
        parent_oids.push(commit.parent_id(index).with_context(|| {
            format!(
                "failed to read parent oid {index} for commit {}",
                short_oid(commit.id())
            )
        })?);
    }
    Ok(parent_oids)
}

#[derive(Clone, Copy)]
struct PendingLane {
    lane: u16,
    oid: Oid,
}

fn populate_graph_lane_segments(nodes: &mut [CommitGraphNode]) {
    let visible_commits = nodes.iter().map(|node| node.oid).collect::<HashSet<_>>();
    let mut active = Vec::<PendingLane>::new();

    for node in nodes.iter_mut() {
        let first_parent_visible = node
            .parent_oids
            .first()
            .copied()
            .filter(|oid| visible_commits.contains(oid));
        let secondary_visible_parents = node
            .parent_oids
            .iter()
            .skip(1)
            .copied()
            .filter(|oid| visible_commits.contains(oid))
            .collect::<Vec<_>>();
        let has_visible_parents =
            first_parent_visible.is_some() || !secondary_visible_parents.is_empty();

        let mut owned_lanes = Vec::new();
        active.retain(|pending| {
            if pending.oid == node.oid {
                owned_lanes.push(pending.lane);
                false
            } else {
                true
            }
        });
        owned_lanes.sort_unstable();

        let mut current_from_active = false;
        let mut converging_lanes = Vec::new();
        let current_lane = if let Some((lane, rest)) = owned_lanes.split_first() {
            current_from_active = true;
            converging_lanes.extend_from_slice(rest);
            *lane
        } else {
            first_free_lane(&active)
        };

        let mut row_lanes = active
            .iter()
            .map(|pending| GraphLaneSegment {
                lane: pending.lane,
                kind: GraphLaneKind::Through,
                target_lane: None,
            })
            .collect::<Vec<_>>();

        let current_kind = if !has_visible_parents {
            GraphLaneKind::End
        } else if current_from_active {
            GraphLaneKind::Through
        } else {
            GraphLaneKind::Start
        };
        row_lanes.push(GraphLaneSegment {
            lane: current_lane,
            kind: current_kind,
            target_lane: None,
        });

        let mut reserved_lanes = active
            .iter()
            .map(|pending| pending.lane)
            .collect::<HashSet<_>>();
        reserved_lanes.insert(current_lane);

        for lane in converging_lanes {
            reserved_lanes.insert(lane);
            row_lanes.push(GraphLaneSegment {
                lane,
                kind: merge_lane_kind(lane, current_lane),
                target_lane: Some(current_lane),
            });
        }

        if let Some(first_parent) = first_parent_visible {
            active.push(PendingLane {
                lane: current_lane,
                oid: first_parent,
            });
        }

        for parent_oid in secondary_visible_parents {
            let parent_lane =
                if let Some(existing) = active.iter().find(|pending| pending.oid == parent_oid) {
                    existing.lane
                } else {
                    let lane = first_free_lane_with_reserved(&active, &reserved_lanes);
                    active.push(PendingLane {
                        lane,
                        oid: parent_oid,
                    });
                    lane
                };
            reserved_lanes.insert(parent_lane);

            row_lanes.push(GraphLaneSegment {
                lane: parent_lane,
                kind: merge_lane_kind(parent_lane, current_lane),
                target_lane: Some(current_lane),
            });
        }

        row_lanes.sort_by_key(|segment| segment.lane);
        active.sort_by_key(|pending| pending.lane);
        node.primary_lane = current_lane;
        node.row_lanes = row_lanes;
    }
}

fn first_free_lane(active: &[PendingLane]) -> u16 {
    first_free_lane_with_reserved(active, &HashSet::new())
}

fn first_free_lane_with_reserved(active: &[PendingLane], reserved: &HashSet<u16>) -> u16 {
    let mut lane = 0u16;
    loop {
        if !reserved.contains(&lane) && active.iter().all(|pending| pending.lane != lane) {
            return lane;
        }
        lane = lane.saturating_add(1);
    }
}

fn merge_lane_kind(parent_lane: u16, target_lane: u16) -> GraphLaneKind {
    if parent_lane < target_lane {
        GraphLaneKind::MergeFromLeft
    } else {
        GraphLaneKind::MergeFromRight
    }
}

fn commit_changed_file_from_delta(
    diff: &Diff<'_>,
    idx: usize,
    delta: git2::DiffDelta<'_>,
) -> Result<CommitChangedFile> {
    let path = commit_display_path_from_delta(&delta)?;
    let status = commit_file_status_from_delta(delta)?;
    let patch = Patch::from_diff(diff, idx).context("failed to create patch from diff")?;
    let (additions, deletions) = if let Some(ref patch) = patch {
        let (_, additions, deletions) =
            patch.line_stats().context("failed to collect line stats")?;
        (additions, deletions)
    } else {
        (0, 0)
    };

    Ok(CommitChangedFile {
        path,
        status,
        additions,
        deletions,
    })
}

fn commit_display_path_from_delta(delta: &git2::DiffDelta<'_>) -> Result<PathBuf> {
    match delta.status() {
        Delta::Deleted => delta
            .old_file()
            .path()
            .map(Path::to_path_buf)
            .context("deleted diff delta did not contain an old path"),
        _ => delta_path(delta).context("diff delta did not contain a path"),
    }
}

fn commit_file_status_from_delta(delta: git2::DiffDelta<'_>) -> Result<CommitFileStatus> {
    match delta.status() {
        Delta::Added => Ok(CommitFileStatus::Added),
        Delta::Modified | Delta::Copied | Delta::Unreadable => Ok(CommitFileStatus::Modified),
        Delta::Deleted => Ok(CommitFileStatus::Deleted),
        Delta::Renamed => Ok(CommitFileStatus::Renamed {
            from: delta
                .old_file()
                .path()
                .map(Path::to_path_buf)
                .context("renamed diff delta missing old path")?,
        }),
        Delta::Typechange => Ok(CommitFileStatus::Typechange),
        other => Err(anyhow!("unsupported commit diff status: {other:?}")),
    }
}

// ── Tracking status ──────────────────────────────────────────────────

fn compute_tracking_status(repo: &Repository) -> Result<GitTrackingStatus> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(_) => {
            return Ok(GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            });
        }
    };

    if repo.head_detached().unwrap_or(false) {
        return Ok(GitTrackingStatus {
            upstream_ref: None,
            ahead: 0,
            behind: 0,
        });
    }

    let branch_short = match head.shorthand() {
        Some(name) => name.to_string(),
        None => {
            return Ok(GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            });
        }
    };

    let local_branch = match repo.find_branch(&branch_short, BranchType::Local) {
        Ok(b) => b,
        Err(_) => {
            return Ok(GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            });
        }
    };

    let upstream = match local_branch.upstream() {
        Ok(u) => u,
        Err(_) => {
            return Ok(GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            });
        }
    };

    let upstream_ref = upstream.get().name().map(str::to_owned);
    let local_oid = match head.target() {
        Some(oid) => oid,
        None => {
            return Ok(GitTrackingStatus {
                upstream_ref,
                ahead: 0,
                behind: 0,
            });
        }
    };
    let upstream_oid = match upstream.get().target() {
        Some(oid) => oid,
        None => {
            return Ok(GitTrackingStatus {
                upstream_ref,
                ahead: 0,
                behind: 0,
            });
        }
    };

    let (ahead, behind) = repo
        .graph_ahead_behind(local_oid, upstream_oid)
        .context("failed to compute ahead/behind counts")?;

    Ok(GitTrackingStatus {
        upstream_ref,
        ahead,
        behind,
    })
}

// ── Local mutation APIs ──────────────────────────────────────────────

pub fn stage_paths(path: &Path, paths: &[PathBuf]) -> Result<()> {
    let discovered = discover_repo(path)?;
    let scope_root = &discovered.scope.scope_root;
    let validated = validate_and_normalize_paths(scope_root, paths)?;

    let mut index = discovered.repo.index().context("failed to read index")?;

    for relative in &validated {
        let full_path = scope_root.join(relative);
        if full_path.exists() {
            index
                .add_path(relative)
                .with_context(|| format!("failed to stage {}", relative.display()))?;
        } else {
            index
                .remove_path(relative)
                .with_context(|| format!("failed to stage deletion of {}", relative.display()))?;
        }
    }

    index.write().context("failed to write index")?;
    Ok(())
}

pub fn unstage_paths(path: &Path, paths: &[PathBuf]) -> Result<()> {
    let discovered = discover_repo(path)?;
    let scope_root = &discovered.scope.scope_root;
    let validated = validate_and_normalize_paths(scope_root, paths)?;

    let head_obj = match discovered.repo.head() {
        Ok(head) => {
            let commit = peel_head_commit(&head)?;
            Some(commit.into_object())
        }
        Err(err) if err.code() == ErrorCode::UnbornBranch => None,
        Err(err) => return Err(err.into()),
    };

    let path_strs: Vec<&str> = validated
        .iter()
        .map(|p| p.to_str().context("path contains invalid UTF-8"))
        .collect::<Result<Vec<_>>>()?;

    discovered
        .repo
        .reset_default(head_obj.as_ref(), path_strs.iter().copied())
        .context("failed to unstage paths")?;

    Ok(())
}

pub fn discard_all_unstaged(path: &Path) -> Result<DiscardMutationOutcome> {
    let discovered = discover_repo(path)?;
    ensure_discard_repo_state(&discovered.repo)?;
    let untracked_paths = collect_untracked_status_paths(&discovered.repo, &[])?;

    let diff = build_unstaged_apply_diff(&discovered.repo, &[], true)?;
    if diff.deltas().len() == 0 {
        return Ok(DiscardMutationOutcome::Applied);
    }

    discovered
        .repo
        .apply(&diff, ApplyLocation::WorkDir, None)
        .context("failed to discard unstaged changes")?;
    remove_workdir_paths(&discovered.scope.scope_root, &untracked_paths)?;
    Ok(DiscardMutationOutcome::Applied)
}

pub fn discard_unstaged_file(path: &Path, relative_path: &Path) -> Result<DiscardMutationOutcome> {
    let discovered = discover_repo(path)?;
    ensure_discard_repo_state(&discovered.repo)?;
    let validated =
        validate_and_normalize_paths(&discovered.scope.scope_root, &[relative_path.to_path_buf()])?;
    let Some(relative_path) = validated.first() else {
        bail!("no path provided to discard");
    };
    let untracked_paths = collect_untracked_status_paths(&discovered.repo, &validated)?;
    match inspect_unstaged_target(&discovered.repo, relative_path)? {
        UnstagedTargetInspection::Discardable => {}
        UnstagedTargetInspection::BlockedUnsupportedStatus => {
            return Ok(DiscardMutationOutcome::Blocked {
                reason: "Discard File is unavailable for renamed or typechanged files.".to_string(),
            });
        }
        UnstagedTargetInspection::Missing => {
            bail!("no unstaged changes found for {}", relative_path.display());
        }
    }

    let apply_diff = build_unstaged_apply_diff(&discovered.repo, &validated, true)?;
    if apply_diff.deltas().len() == 0 {
        bail!("no unstaged changes found for {}", relative_path.display());
    }

    discovered
        .repo
        .apply(&apply_diff, ApplyLocation::WorkDir, None)
        .with_context(|| format!("failed to discard {}", relative_path.display()))?;
    remove_workdir_paths(&discovered.scope.scope_root, &untracked_paths)?;
    Ok(DiscardMutationOutcome::Applied)
}

pub fn discard_unstaged_hunk(
    path: &Path,
    relative_path: &Path,
    target: &DiscardHunkTarget,
) -> Result<DiscardMutationOutcome> {
    let discovered = discover_repo(path)?;
    let scope_root = &discovered.scope.scope_root;
    ensure_discard_repo_state(&discovered.repo)?;
    let validated = validate_and_normalize_paths(scope_root, &[relative_path.to_path_buf()])?;
    let Some(relative_path) = validated.first() else {
        bail!("no path provided to discard hunk");
    };
    match inspect_unstaged_target(&discovered.repo, relative_path)? {
        UnstagedTargetInspection::Discardable => {}
        UnstagedTargetInspection::BlockedUnsupportedStatus => {
            return Ok(DiscardMutationOutcome::Blocked {
                reason: "Discard Hunk is unavailable for renamed or typechanged files.".to_string(),
            });
        }
        UnstagedTargetInspection::Missing => {
            bail!("no unstaged changes found for {}", relative_path.display());
        }
    }
    let Some(rendered) =
        render_unstaged_target_with_path_filter(&discovered.repo, relative_path, true)?
    else {
        bail!("no unstaged changes found for {}", relative_path.display());
    };
    let file = rendered.file.clone();

    let matched_hunk_index =
        match resolve_hunk_discard_index(&rendered.lines, &rendered.hunks, target) {
            Ok(index) => index,
            Err(reason) => return Ok(DiscardMutationOutcome::Blocked { reason }),
        };

    let apply_diff = build_unstaged_apply_diff(&discovered.repo, &validated, true)?;
    if apply_diff.deltas().len() == 0 {
        bail!("no unstaged changes found for {}", relative_path.display());
    }

    let mut seen_hunks = 0usize;
    let mut options = ApplyOptions::new();
    options.hunk_callback(|_hunk| {
        let should_apply = seen_hunks == matched_hunk_index;
        seen_hunks += 1;
        should_apply
    });

    discovered
        .repo
        .apply(&apply_diff, ApplyLocation::WorkDir, Some(&mut options))
        .with_context(|| {
            format!(
                "failed to discard hunk {} in {}",
                matched_hunk_index,
                file.relative_path.display()
            )
        })?;
    remove_empty_untracked_file(scope_root, &file.relative_path, file.status)?;
    Ok(DiscardMutationOutcome::Applied)
}

pub fn commit_staged(path: &Path, message: &str) -> Result<Oid> {
    let message = message.trim();
    if message.is_empty() {
        bail!("commit message cannot be empty");
    }

    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;

    let sig = repo
        .signature()
        .context("Git identity not configured. Set user.name and user.email in your git config.")?;

    let mut index = repo.index().context("failed to read index")?;
    let tree_id = index.write_tree().context("failed to write index tree")?;
    let tree = repo
        .find_tree(tree_id)
        .context("failed to find written tree")?;

    let parent = match repo.head() {
        Ok(head) => Some(peel_head_commit(&head)?),
        Err(err) if err.code() == ErrorCode::UnbornBranch => None,
        Err(err) => return Err(err.into()),
    };

    // Reject empty commits (no staged changes).
    if let Some(ref parent_commit) = parent {
        if parent_commit.tree_id() == tree_id {
            bail!("no staged changes to commit");
        }
    }

    let parents: Vec<&Commit> = parent.iter().collect();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .context("failed to create commit")?;

    Ok(oid)
}

// ── Scope clean check ────────────────────────────────────────────────

pub fn is_scope_clean(path: &Path) -> Result<bool> {
    let discovered = discover_repo(path)?;
    is_repo_clean(&discovered.repo)
}

fn is_repo_clean(repo: &Repository) -> Result<bool> {
    let mut options = StatusOptions::new();
    options
        .show(StatusShow::IndexAndWorkdir)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .context("failed to scan repository status")?;
    Ok(!statuses
        .iter()
        .any(|entry| status_is_uncommitted(entry.status())))
}

// ── Upstream info ────────────────────────────────────────────────────

pub fn resolve_upstream_info(path: &Path) -> Result<UpstreamInfo> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;

    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    if repo.head_detached().unwrap_or(false) {
        bail!("No upstream configured for this branch. Use the terminal to publish it.");
    }

    let branch_short = head
        .shorthand()
        .context("HEAD has no shorthand")?
        .to_string();

    let config = repo.config().context("failed to read git config")?;

    let remote = config
        .get_string(&format!("branch.{branch_short}.remote"))
        .map_err(|_| {
            anyhow!("No upstream configured for this branch. Use the terminal to publish it.")
        })?;

    let merge_ref = config
        .get_string(&format!("branch.{branch_short}.merge"))
        .map_err(|_| {
            anyhow!("No upstream configured for this branch. Use the terminal to publish it.")
        })?;

    // Parse merge refspec (e.g. "refs/heads/main" → "main")
    let upstream_branch = merge_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&merge_ref)
        .to_string();

    Ok(UpstreamInfo {
        remote,
        upstream_branch,
    })
}

pub fn checkout_local_branch(path: &Path, branch_name: &str) -> Result<BranchCheckoutOutcome> {
    let discovered = discover_repo(path)?;
    if let Some(reason) = branch_checkout_block_reason(&discovered.repo)? {
        return Ok(BranchCheckoutOutcome::Blocked { reason });
    }

    let branch = match discovered.repo.find_branch(branch_name, BranchType::Local) {
        Ok(branch) => branch,
        Err(err) if err.code() == ErrorCode::NotFound => {
            return Ok(BranchCheckoutOutcome::Blocked {
                reason: format!("Local branch {branch_name} was not found."),
            });
        }
        Err(err) => return Err(err).context("failed to resolve local branch"),
    };

    let full_ref = branch
        .get()
        .name()
        .context("local branch missing full ref name")?
        .to_string();

    if current_head_ref_name(&discovered.repo)?.as_deref() == Some(full_ref.as_str()) {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!("Local branch {branch_name} is already current."),
        });
    }

    if let Some(occupied_path) = find_checkout_holding_ref(
        &discovered.scope.repo_root,
        &discovered.scope.scope_root,
        &full_ref,
    )? {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!(
                "Local branch {branch_name} is already checked out at {}.",
                occupied_path.display()
            ),
        });
    }

    let target = branch
        .get()
        .target()
        .context("local branch has no target commit")?;
    let commit = discovered
        .repo
        .find_commit(target)
        .context("failed to resolve local branch target commit")?;

    switch_head_to_branch(&discovered.repo, &commit, &full_ref)?;

    Ok(BranchCheckoutOutcome::SwitchedLocal {
        branch_name: branch_name.to_string(),
    })
}

pub fn checkout_remote_branch(path: &Path, remote_full_ref: &str) -> Result<BranchCheckoutOutcome> {
    let discovered = discover_repo(path)?;
    if let Some(reason) = branch_checkout_block_reason(&discovered.repo)? {
        return Ok(BranchCheckoutOutcome::Blocked { reason });
    }

    let Some((remote_name, short_name)) = parse_remote_tracking_ref(remote_full_ref) else {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!(
                "Remote branch {remote_full_ref} is not a concrete remote-tracking ref."
            ),
        });
    };

    let remote_ref = match discovered.repo.find_reference(remote_full_ref) {
        Ok(reference) => reference,
        Err(err) if err.code() == ErrorCode::NotFound => {
            return Ok(BranchCheckoutOutcome::Blocked {
                reason: format!(
                    "Remote branch {remote_full_ref} was not found. Fetch to refresh remote-tracking refs."
                ),
            });
        }
        Err(err) => return Err(err).context("failed to resolve remote-tracking ref"),
    };

    if remote_ref.symbolic_target().is_some() {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!(
                "Remote branch {remote_full_ref} is not a concrete remote-tracking ref."
            ),
        });
    }

    let mut created = false;
    let local_branch_name = if let Some(existing) =
        select_tracking_branch_for_remote(&discovered.repo, remote_full_ref, &short_name)?
    {
        existing
    } else {
        match discovered.repo.find_branch(&short_name, BranchType::Local) {
            Ok(existing_branch) => {
                let existing_upstream = local_branch_upstream_full_ref(&existing_branch)?;
                if existing_upstream.as_deref() == Some(remote_full_ref) {
                    short_name.clone()
                } else {
                    return Ok(BranchCheckoutOutcome::Blocked {
                        reason: format!(
                            "Local branch {short_name} already exists and does not track {remote_full_ref}."
                        ),
                    });
                }
            }
            Err(err) if err.code() == ErrorCode::NotFound => {
                let remote_target = remote_ref
                    .target()
                    .context("remote-tracking ref has no target commit")?;
                let remote_commit = discovered
                    .repo
                    .find_commit(remote_target)
                    .context("failed to resolve remote-tracking branch target commit")?;
                let mut local_branch =
                    discovered
                        .repo
                        .branch(&short_name, &remote_commit, false)
                        .with_context(|| format!("failed to create local branch {short_name}"))?;
                local_branch
                    .set_upstream(Some(&format!("{remote_name}/{short_name}")))
                    .with_context(|| {
                        format!("failed to configure upstream for local branch {short_name}")
                    })?;
                created = true;
                short_name.clone()
            }
            Err(err) => return Err(err).context("failed to inspect local branch collision"),
        }
    };

    let local_branch = discovered
        .repo
        .find_branch(&local_branch_name, BranchType::Local)
        .with_context(|| format!("failed to resolve local branch {local_branch_name}"))?;
    let local_full_ref = local_branch
        .get()
        .name()
        .context("local branch missing full ref name")?
        .to_string();

    if current_head_ref_name(&discovered.repo)?.as_deref() == Some(local_full_ref.as_str()) {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!("Local branch {local_branch_name} is already current."),
        });
    }

    if let Some(occupied_path) = find_checkout_holding_ref(
        &discovered.scope.repo_root,
        &discovered.scope.scope_root,
        &local_full_ref,
    )? {
        return Ok(BranchCheckoutOutcome::Blocked {
            reason: format!(
                "Local branch {local_branch_name} is already checked out at {}.",
                occupied_path.display()
            ),
        });
    }

    let target = local_branch
        .get()
        .target()
        .context("local tracking branch has no target commit")?;
    let commit = discovered
        .repo
        .find_commit(target)
        .with_context(|| format!("failed to resolve local branch {local_branch_name} target"))?;

    switch_head_to_branch(&discovered.repo, &commit, &local_full_ref)?;

    Ok(BranchCheckoutOutcome::SwitchedTracking {
        local_branch_name,
        remote_full_ref: remote_full_ref.to_string(),
        created,
    })
}

pub fn create_local_branch_from_local(
    path: &Path,
    source_branch_name: &str,
    new_branch_name: &str,
) -> Result<CreateLocalBranchOutcome> {
    let discovered = discover_repo(path)?;
    if let Some(reason) = branch_checkout_block_reason(&discovered.repo)? {
        return Ok(CreateLocalBranchOutcome::Blocked { reason });
    }

    let new_branch_name = match validate_local_branch_name(new_branch_name) {
        Ok(branch_name) => branch_name,
        Err(reason) => {
            return Ok(CreateLocalBranchOutcome::Blocked { reason });
        }
    };

    let source_branch = match discovered
        .repo
        .find_branch(source_branch_name, BranchType::Local)
    {
        Ok(branch) => branch,
        Err(err) if err.code() == ErrorCode::NotFound => {
            return Ok(CreateLocalBranchOutcome::Blocked {
                reason: format!("Local branch {source_branch_name} was not found."),
            });
        }
        Err(err) => return Err(err).context("failed to resolve source local branch"),
    };

    if discovered
        .repo
        .find_branch(&new_branch_name, BranchType::Local)
        .is_ok()
    {
        return Ok(CreateLocalBranchOutcome::Blocked {
            reason: format!("Local branch {new_branch_name} already exists."),
        });
    }

    let target = source_branch
        .get()
        .target()
        .context("source local branch has no target commit")?;
    let commit = discovered
        .repo
        .find_commit(target)
        .context("failed to resolve source local branch target commit")?;

    let branch = discovered
        .repo
        .branch(&new_branch_name, &commit, false)
        .with_context(|| format!("failed to create local branch {new_branch_name}"))?;
    let full_ref = branch
        .get()
        .name()
        .context("created local branch missing full ref name")?
        .to_string();

    switch_head_to_branch(&discovered.repo, &commit, &full_ref)?;

    Ok(CreateLocalBranchOutcome::CreatedAndCheckedOut {
        source_branch_name: source_branch_name.to_string(),
        branch_name: new_branch_name,
    })
}

pub fn delete_local_branch(path: &Path, branch_name: &str) -> Result<DeleteLocalBranchOutcome> {
    let discovered = discover_repo(path)?;
    let mut branch = match discovered.repo.find_branch(branch_name, BranchType::Local) {
        Ok(branch) => branch,
        Err(err) if err.code() == ErrorCode::NotFound => {
            return Ok(DeleteLocalBranchOutcome::Blocked {
                reason: format!("Local branch {branch_name} was not found."),
            });
        }
        Err(err) => return Err(err).context("failed to resolve local branch"),
    };

    let full_ref = branch
        .get()
        .name()
        .context("local branch missing full ref name")?
        .to_string();

    if current_head_ref_name(&discovered.repo)?.as_deref() == Some(full_ref.as_str()) {
        return Ok(DeleteLocalBranchOutcome::Blocked {
            reason: format!("Cannot delete the current branch {branch_name}."),
        });
    }

    if let Some(occupied_path) = find_checkout_holding_ref(
        &discovered.scope.repo_root,
        &discovered.scope.scope_root,
        &full_ref,
    )? {
        return Ok(DeleteLocalBranchOutcome::Blocked {
            reason: format!(
                "Local branch {branch_name} is checked out at {}.",
                occupied_path.display()
            ),
        });
    }

    if local_branch_requires_force_delete(&discovered.repo, &branch)? {
        return Ok(DeleteLocalBranchOutcome::Blocked {
            reason: format!(
                "Local branch {branch_name} is not fully merged. Merge it or use the terminal to force-delete it."
            ),
        });
    }

    if let Err(err) = branch.delete() {
        return Err(err).with_context(|| format!("failed to delete local branch {branch_name}"));
    }

    Ok(DeleteLocalBranchOutcome::Deleted {
        branch_name: branch_name.to_string(),
    })
}

// ── Pull integration ─────────────────────────────────────────────────

pub fn pull_integrate(path: &Path) -> Result<MergeOutcome> {
    // Precondition: scope must be clean.
    if !is_scope_clean(path)? {
        return Ok(MergeOutcome::Blocked {
            reason: "Cannot pull with uncommitted changes. Commit or stash first.".into(),
        });
    }

    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;

    // Resolve HEAD commit
    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    let local_commit = peel_head_commit(&head)?;

    // Resolve upstream tracking ref commit
    let branch_short = head
        .shorthand()
        .context("HEAD has no shorthand")?
        .to_string();
    let local_branch = repo
        .find_branch(&branch_short, BranchType::Local)
        .context("cannot find local branch")?;
    let upstream = local_branch
        .upstream()
        .context("no upstream tracking branch configured")?;
    let upstream_oid = upstream
        .get()
        .target()
        .context("upstream ref has no target")?;
    let upstream_commit = repo
        .find_commit(upstream_oid)
        .context("cannot resolve upstream commit")?;

    // Already up to date?
    if local_commit.id() == upstream_commit.id()
        || repo.graph_descendant_of(local_commit.id(), upstream_commit.id())?
    {
        return Ok(MergeOutcome::AlreadyMerged);
    }

    // Merge base
    let merge_base = repo
        .merge_base(local_commit.id(), upstream_commit.id())
        .context("no common ancestor between local and upstream")?;

    // Fast-forward: local == merge_base means upstream is strictly ahead
    if local_commit.id() == merge_base {
        // Update HEAD ref to upstream commit
        let head_ref_name = head
            .name()
            .context("HEAD has no symbolic name for fast-forward")?;
        let mut head_ref = repo.find_reference(head_ref_name)?;
        head_ref.set_target(
            upstream_commit.id(),
            &format!(
                "orcashell pull: fast-forward to {}",
                short_oid(upstream_commit.id())
            ),
        )?;
        // Checkout the new HEAD
        checkout_head_for_scope(&discovered.scope.scope_root)?;
        return Ok(MergeOutcome::FastForward {
            new_head: upstream_commit.id(),
        });
    }

    // In-memory three-way merge for conflict preflight
    let merge_base_commit = repo.find_commit(merge_base)?;
    let ancestor_tree = merge_base_commit.tree()?;
    let local_tree = local_commit.tree()?;
    let upstream_tree = upstream_commit.tree()?;

    let mut merge_index = repo
        .merge_trees(&ancestor_tree, &local_tree, &upstream_tree, None)
        .context("merge analysis failed")?;

    if merge_index.has_conflicts() {
        let upstream_annotated = repo
            .find_annotated_commit(upstream_commit.id())
            .context("failed to prepare upstream annotated commit")?;
        enter_merge_state(repo, &upstream_annotated)?;
        return merge_conflicted_outcome(repo, &discovered.scope.scope_root);
    }

    // Clean merge: write tree and create merge commit
    let merged_tree_oid = merge_index
        .write_tree_to(repo)
        .context("failed to write merged tree")?;
    let merged_tree = repo.find_tree(merged_tree_oid)?;

    let sig = repo.signature().context("Git identity not configured")?;
    let upstream_short = upstream.get().shorthand().unwrap_or("upstream");
    let local_short = head.shorthand().unwrap_or("HEAD");
    let merge_message = format!("Merge branch '{upstream_short}' into {local_short}");

    let merge_oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            &merge_message,
            &merged_tree,
            &[&local_commit, &upstream_commit],
        )
        .context("failed to create merge commit")?;

    // Checkout the merge result
    checkout_head_for_scope(&discovered.scope.scope_root)?;

    Ok(MergeOutcome::MergeCommit { merge_oid })
}

pub fn complete_merge(path: &Path) -> Result<Oid> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;
    ensure_merge_state(repo)?;

    let conflicted_paths = collect_conflicted_paths(repo)?;
    if !conflicted_paths.is_empty() {
        bail!("cannot complete merge with unresolved conflicts");
    }

    let unstaged_diff = build_unstaged_diff(repo)?;
    let unstaged_files = collect_changed_files(&unstaged_diff, None)?;
    if !unstaged_files.is_empty() {
        bail!("cannot complete merge with unstaged changes");
    }

    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    let head_commit = peel_head_commit(&head)?;
    let merge_head_commit = read_merge_head_commit(repo)?;
    let merge_message = read_merge_message(repo)?;

    let sig = repo.signature().context("Git identity not configured")?;
    let mut index = repo.index().context("failed to read index")?;
    let tree_id = index
        .write_tree_to(repo)
        .context("failed to write index tree")?;
    let tree = repo
        .find_tree(tree_id)
        .context("failed to find merge tree")?;

    let merge_oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            &merge_message,
            &tree,
            &[&head_commit, &merge_head_commit],
        )
        .context("failed to create merge commit")?;
    repo.cleanup_state()
        .context("failed to clear merge state")?;
    Ok(merge_oid)
}

pub fn abort_merge(path: &Path) -> Result<()> {
    let discovered = discover_repo(path)?;
    let repo = &discovered.repo;
    ensure_merge_state(repo)?;

    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    let head_obj = head
        .peel(ObjectType::Commit)
        .context("failed to peel HEAD to commit for abort")?;
    repo.reset(&head_obj, ResetType::Hard, None)
        .context("failed to hard reset during merge abort")?;
    repo.cleanup_state()
        .context("failed to clear merge state")?;
    Ok(())
}

// ── Merge-back substrate ─────────────────────────────────────────────

pub fn merge_managed_branch(managed_scope: &Path, source_ref: &str) -> Result<MergeOutcome> {
    let discovered = discover_repo(managed_scope)?;
    let managed_repo = &discovered.repo;
    let repo_root = &discovered.scope.repo_root;

    // Precondition: managed scope must be clean.
    if !is_scope_clean(managed_scope)? {
        return Ok(MergeOutcome::Blocked {
            reason: "Managed worktree has uncommitted changes. Commit or stash before merging."
                .into(),
        });
    }

    // Resolve managed branch HEAD (what we are merging FROM)
    let managed_head = managed_repo
        .head()
        .map_err(map_unborn_head("managed worktree has no valid HEAD"))?;
    let managed_commit = peel_head_commit(&managed_head)?;
    let managed_commit_oid = managed_commit.id();

    // Resolve the source scope: find the worktree whose HEAD equals source_ref.
    let source_scope_path = resolve_source_scope(repo_root, source_ref)?;

    // Precondition: source scope must be clean.
    if !is_scope_clean(&source_scope_path)? {
        return Ok(MergeOutcome::Blocked {
            reason: "Source branch has uncommitted changes. Commit or stash before merging.".into(),
        });
    }

    // Open the source repo directly for all ref updates and commits.
    let source_repo = Repository::open(&source_scope_path).with_context(|| {
        format!(
            "failed to open source repo at {}",
            source_scope_path.display()
        )
    })?;

    // Resolve source HEAD commit from the source repo
    let source_head = source_repo
        .head()
        .map_err(map_unborn_head("source worktree has no valid HEAD"))?;
    let source_commit = peel_head_commit(&source_head)?;

    // Resolve managed commit in the source repo (shared object store)
    let managed_commit_in_source = source_repo
        .find_commit(managed_commit_oid)
        .context("cannot find managed commit in source repository")?;

    // Already merged? (source already contains managed, or they are the same commit)
    if source_commit.id() == managed_commit_oid
        || source_repo.graph_descendant_of(source_commit.id(), managed_commit_oid)?
    {
        return Ok(MergeOutcome::AlreadyMerged);
    }

    // Merge base
    let merge_base = source_repo
        .merge_base(source_commit.id(), managed_commit_oid)
        .context("no common ancestor between source and managed branch")?;

    // Fast-forward: source == merge_base means managed is strictly ahead
    if source_commit.id() == merge_base {
        let source_head_ref_name = source_head
            .name()
            .context("source HEAD has no symbolic name for fast-forward")?;
        let mut source_ref_mut = source_repo.find_reference(source_head_ref_name)?;
        source_ref_mut.set_target(
            managed_commit_oid,
            &format!(
                "orcashell merge-back: fast-forward to {}",
                short_oid(managed_commit_oid)
            ),
        )?;
        // Update the source worktree checkout to match the new ref.
        checkout_head_for_scope(&source_scope_path)?;
        return Ok(MergeOutcome::FastForward {
            new_head: managed_commit_oid,
        });
    }

    // In-memory three-way merge for conflict preflight (using source repo)
    let merge_base_commit = source_repo.find_commit(merge_base)?;
    let ancestor_tree = merge_base_commit.tree()?;
    let source_tree = source_commit.tree()?;
    let managed_tree = managed_commit_in_source.tree()?;

    let mut merge_index = source_repo
        .merge_trees(&ancestor_tree, &source_tree, &managed_tree, None)
        .context("merge analysis failed")?;

    if merge_index.has_conflicts() {
        let managed_annotated = source_repo
            .find_annotated_commit(managed_commit_oid)
            .context("failed to prepare managed annotated commit")?;
        enter_merge_state(&source_repo, &managed_annotated)?;
        return merge_conflicted_outcome(&source_repo, &source_scope_path);
    }

    // Clean merge: write tree and create merge commit IN the source repo
    let merged_tree_oid = merge_index
        .write_tree_to(&source_repo)
        .context("failed to write merged tree")?;
    let merged_tree = source_repo.find_tree(merged_tree_oid)?;

    let sig = source_repo
        .signature()
        .context("Git identity not configured")?;
    let managed_short = managed_head.shorthand().unwrap_or("managed");
    let source_short = source_head.shorthand().unwrap_or(source_ref);
    let merge_message = format!("Merge branch '{managed_short}' into {source_short}");

    let merge_oid = source_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            &merge_message,
            &merged_tree,
            &[&source_commit, &managed_commit_in_source],
        )
        .context("failed to create merge commit")?;

    // Update the source worktree checkout to match the new merge commit.
    checkout_head_for_scope(&source_scope_path)?;

    Ok(MergeOutcome::MergeCommit { merge_oid })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictMarkerStyle {
    Merge,
    Diff3,
}

fn enter_merge_state(repo: &Repository, commit: &git2::AnnotatedCommit<'_>) -> Result<()> {
    let mut merge_opts = MergeOptions::new();
    let mut checkout = git2::build::CheckoutBuilder::new();
    let conflict_style = configured_conflict_marker_style(repo)?;
    checkout.allow_conflicts(true).recreate_missing(true);
    match conflict_style {
        ConflictMarkerStyle::Merge => {
            checkout.conflict_style_merge(true);
        }
        ConflictMarkerStyle::Diff3 => {
            checkout.conflict_style_diff3(true);
        }
    }
    repo.merge(&[commit], Some(&mut merge_opts), Some(&mut checkout))
        .context("failed to enter merge state")?;
    Ok(())
}

fn configured_conflict_marker_style(repo: &Repository) -> Result<ConflictMarkerStyle> {
    let config = repo.config().context("failed to read git config")?;
    let style = config
        .get_string("merge.conflictstyle")
        .unwrap_or_else(|_| "merge".to_string());
    Ok(match style.trim().to_ascii_lowercase().as_str() {
        "diff3" | "zdiff3" => ConflictMarkerStyle::Diff3,
        _ => ConflictMarkerStyle::Merge,
    })
}

fn merge_conflicted_outcome(repo: &Repository, affected_scope: &Path) -> Result<MergeOutcome> {
    Ok(MergeOutcome::Conflicted {
        affected_scope: affected_scope.to_path_buf(),
        conflicted_files: collect_conflicted_paths(repo)?,
    })
}

fn branch_checkout_block_reason(repo: &Repository) -> Result<Option<String>> {
    if repo.state() == RepositoryState::Merge {
        return Ok(Some(
            "Cannot checkout branches while a merge is in progress. Complete or abort the merge first."
                .to_string(),
        ));
    }

    if !is_repo_clean(repo)? {
        return Ok(Some(
            "Cannot checkout branches with uncommitted changes. Commit or stash first.".to_string(),
        ));
    }

    Ok(None)
}

fn validate_local_branch_name(branch_name: &str) -> std::result::Result<String, String> {
    let trimmed = branch_name.trim();
    if trimmed.is_empty() {
        return Err("Branch name cannot be empty.".to_string());
    }

    let full_ref = format!("refs/heads/{trimmed}");
    if !Reference::is_valid_name(&full_ref) {
        return Err(format!("{trimmed} is not a valid local branch name."));
    }

    Ok(trimmed.to_string())
}

fn local_branch_requires_force_delete(
    repo: &Repository,
    branch: &git2::Branch<'_>,
) -> Result<bool> {
    let branch_target = branch
        .get()
        .target()
        .context("local branch has no target commit")?;

    let comparison_target = match branch.upstream() {
        Ok(upstream) => upstream.get().target(),
        Err(err) if err.code() == ErrorCode::NotFound => None,
        Err(err) => return Err(err).context("failed to resolve branch upstream"),
    }
    .or_else(|| repo.head().ok().and_then(|head| head.target()));

    let Some(comparison_target) = comparison_target else {
        return Ok(false);
    };

    Ok(comparison_target != branch_target
        && !repo.graph_descendant_of(comparison_target, branch_target)?)
}

fn status_is_uncommitted(status: Status) -> bool {
    status.intersects(
        Status::INDEX_NEW
            | Status::INDEX_MODIFIED
            | Status::INDEX_DELETED
            | Status::INDEX_RENAMED
            | Status::INDEX_TYPECHANGE
            | Status::WT_NEW
            | Status::WT_MODIFIED
            | Status::WT_DELETED
            | Status::WT_TYPECHANGE
            | Status::WT_RENAMED
            | Status::WT_UNREADABLE
            | Status::CONFLICTED,
    )
}

fn ensure_merge_state(repo: &Repository) -> Result<()> {
    if repo.state() != RepositoryState::Merge {
        bail!("repository is not in merge state");
    }
    Ok(())
}

fn read_merge_head_commit(repo: &Repository) -> Result<Commit<'_>> {
    let merge_head =
        fs::read_to_string(repo.path().join("MERGE_HEAD")).context("failed to read MERGE_HEAD")?;
    let merge_head_oid = merge_head
        .lines()
        .find(|line| !line.trim().is_empty())
        .context("MERGE_HEAD did not contain a commit id")?;
    let merge_head_oid = Oid::from_str(merge_head_oid.trim())
        .context("MERGE_HEAD contained an invalid commit id")?;
    repo.find_commit(merge_head_oid)
        .context("failed to resolve MERGE_HEAD commit")
}

fn read_merge_message(repo: &Repository) -> Result<String> {
    let message =
        fs::read_to_string(repo.path().join("MERGE_MSG")).context("failed to read MERGE_MSG")?;
    let trimmed = message.trim();
    if trimmed.is_empty() {
        bail!("MERGE_MSG was empty");
    }
    Ok(trimmed.to_string())
}

/// Resolve the worktree path whose HEAD matches `source_ref`.
///
/// Checks the main checkout at `repo_root` first, then all linked worktrees.
/// Returns an error if no checkout has `source_ref` as its current HEAD.
pub fn resolve_source_scope(repo_root: &Path, source_ref: &str) -> Result<PathBuf> {
    let admin_repo =
        Repository::open(repo_root).context("failed to open admin repo for source resolution")?;

    // Check main checkout first.
    if let Ok(head) = admin_repo.head() {
        if head.name() == Some(source_ref) {
            return Ok(repo_root.to_path_buf());
        }
    }

    // Check linked worktrees.
    if let Ok(worktrees) = admin_repo.worktrees() {
        for wt_name in worktrees.iter().flatten() {
            if let Ok(wt) = admin_repo.find_worktree(wt_name) {
                let wt_path = wt.path().to_path_buf();
                if let Ok(wt_repo) = Repository::open(&wt_path) {
                    if let Ok(wt_head) = wt_repo.head() {
                        if wt_head.name() == Some(source_ref) {
                            return Ok(wt_path);
                        }
                    }
                }
            }
        }
    }

    Err(anyhow!(
        "cannot resolve source ref {source_ref}: no checkout has this branch checked out"
    ))
}

/// Force the worktree at `scope_path` to match its HEAD ref after a ref update.
fn checkout_head_for_scope(scope_path: &Path) -> Result<()> {
    let repo = Repository::open(scope_path)
        .with_context(|| format!("failed to open repo at {}", scope_path.display()))?;
    let head_obj = repo
        .head()?
        .peel(ObjectType::Tree)
        .context("failed to peel HEAD to tree")?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(&head_obj, Some(&mut checkout))
        .context("failed to checkout HEAD tree")?;
    Ok(())
}

fn switch_head_to_branch(
    repo: &Repository,
    target_commit: &Commit<'_>,
    full_ref: &str,
) -> Result<()> {
    let target_object = target_commit.as_object();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe().recreate_missing(true);
    repo.checkout_tree(target_object, Some(&mut checkout))
        .with_context(|| format!("failed to checkout {full_ref}"))?;
    repo.set_head(full_ref)
        .with_context(|| format!("failed to set HEAD to {full_ref}"))?;
    Ok(())
}

fn current_head_ref_name(repo: &Repository) -> Result<Option<String>> {
    match repo.head() {
        Ok(head) => Ok(head.name().map(str::to_owned)),
        Err(err) if err.code() == ErrorCode::UnbornBranch => Ok(None),
        Err(err) => Err(err).context("failed to resolve HEAD"),
    }
}

fn current_local_branch_name(repo: &Repository) -> Result<Option<String>> {
    if repo
        .head_detached()
        .context("failed to inspect detached HEAD state")?
    {
        return Ok(None);
    }

    let Some(head_ref) = current_head_ref_name(repo)? else {
        return Ok(None);
    };
    Ok(head_ref.strip_prefix("refs/heads/").map(str::to_owned))
}

fn select_tracking_branch_for_remote(
    repo: &Repository,
    remote_full_ref: &str,
    short_name: &str,
) -> Result<Option<String>> {
    if let Some(current_branch) = current_local_branch_name(repo)? {
        let branch = repo
            .find_branch(&current_branch, BranchType::Local)
            .with_context(|| format!("failed to resolve current branch {current_branch}"))?;
        if local_branch_upstream_full_ref(&branch)?.as_deref() == Some(remote_full_ref) {
            return Ok(Some(current_branch));
        }
    }

    let mut matches = Vec::new();
    let branches = repo
        .branches(Some(BranchType::Local))
        .context("failed to enumerate local branches")?;
    for branch in branches {
        let (branch, _) = branch.context("failed to read local branch")?;
        if local_branch_upstream_full_ref(&branch)?.as_deref() != Some(remote_full_ref) {
            continue;
        }
        let Some(name) = branch.name().context("failed to read local branch name")? else {
            continue;
        };
        matches.push(name.to_string());
    }

    if matches.is_empty() {
        return Ok(None);
    }

    if matches.iter().any(|name| name == short_name) {
        return Ok(Some(short_name.to_string()));
    }

    matches.sort();
    Ok(matches.into_iter().next())
}

fn local_branch_upstream_full_ref(branch: &git2::Branch<'_>) -> Result<Option<String>> {
    match branch.upstream() {
        Ok(upstream) => Ok(upstream.get().name().map(str::to_owned)),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(err).context("failed to resolve branch upstream"),
    }
}

fn find_checkout_holding_ref(
    repo_root: &Path,
    current_scope_root: &Path,
    full_ref: &str,
) -> Result<Option<PathBuf>> {
    let admin_repo = Repository::open(repo_root).with_context(|| {
        format!(
            "failed to open repository at {} for worktree inspection",
            repo_root.display()
        )
    })?;

    let mut checkout_paths = vec![repo_root.to_path_buf()];
    if let Ok(worktrees) = admin_repo.worktrees() {
        for worktree_name in worktrees.iter().flatten() {
            let Ok(worktree) = admin_repo.find_worktree(worktree_name) else {
                continue;
            };
            checkout_paths.push(worktree.path().to_path_buf());
        }
    }

    for checkout_path in checkout_paths {
        let checkout_scope = match discover_scope(&checkout_path) {
            Ok(scope) => scope.scope_root,
            Err(_) => continue,
        };
        if checkout_scope == current_scope_root {
            continue;
        }

        let checkout_repo = match Repository::open(&checkout_scope) {
            Ok(repo) => repo,
            Err(_) => continue,
        };
        let Ok(head_ref) = current_head_ref_name(&checkout_repo) else {
            continue;
        };
        if head_ref.as_deref() == Some(full_ref) {
            return Ok(Some(checkout_scope));
        }
    }

    Ok(None)
}

// ── Worktree management ──────────────────────────────────────────────

pub fn create_managed_worktree(path: &Path, worktree_id: &str) -> Result<ManagedWorktree> {
    validate_worktree_id(worktree_id)?;

    let discovered = discover_repo(path)?;
    let admin_repo = Repository::open(&discovered.scope.repo_root).with_context(|| {
        format!(
            "failed to open repository at {}",
            discovered.scope.repo_root.display()
        )
    })?;

    let head = discovered
        .repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    let source_commit = peel_head_commit(&head)?;
    let admin_commit = admin_repo
        .find_commit(source_commit.id())
        .context("failed to resolve source commit in admin repository")?;
    let src_ref = source_ref(&head, source_commit.id());
    let worktree_name = worktree_id.to_string();
    let worktree_path = managed_worktree_path(&discovered.scope.repo_root, worktree_id);
    if worktree_path.exists() {
        bail!(
            "managed worktree path already exists: {}",
            worktree_path.display()
        );
    }
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create worktree parent {}", parent.display()))?;
    }

    ensure_orcashell_excluded(&discovered.scope.repo_root)?;

    let branch_name = managed_branch_name(worktree_id);
    if admin_repo
        .find_branch(&branch_name, BranchType::Local)
        .is_ok()
    {
        bail!("managed worktree branch {branch_name} already exists");
    }

    let mut branch = admin_repo
        .branch(&branch_name, &admin_commit, false)
        .with_context(|| format!("failed to create branch {branch_name}"))?;

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(branch.get()));
    if let Err(err) = admin_repo.worktree(&worktree_name, &worktree_path, Some(&opts)) {
        let cleanup_err = branch.delete().err();
        let mut error = anyhow!(err).context(format!(
            "failed to create worktree {}",
            worktree_path.display()
        ));
        if let Some(cleanup_err) = cleanup_err {
            error = error.context(format!(
                "failed to delete branch {branch_name} after worktree creation failure: {cleanup_err}"
            ));
        }
        return Err(error);
    }

    Ok(ManagedWorktree {
        id: worktree_id.to_string(),
        branch_name,
        worktree_name,
        repo_root: discovered.scope.repo_root,
        path: worktree_path,
        source_ref: src_ref,
    })
}

/// Remove a managed worktree with optional branch deletion.
/// `path` is the worktree scope root. The worktree must be clean.
pub fn remove_managed_worktree(path: &Path, delete_branch: bool) -> Result<()> {
    // Precondition: worktree must be clean before removal.
    if !is_scope_clean(path)? {
        bail!("worktree has uncommitted changes; commit or stash before removing");
    }

    let discovered = discover_repo(path)?;
    let worktree_name = discovered
        .scope
        .worktree_name
        .as_deref()
        .context("path is not inside a managed worktree")?;
    validate_worktree_id(worktree_name)
        .with_context(|| format!("worktree {worktree_name} is not Orca-managed"))?;

    let expected_path = managed_worktree_path(&discovered.scope.repo_root, worktree_name);
    if discovered.scope.scope_root != expected_path {
        bail!(
            "worktree {worktree_name} is not Orca-managed: expected path {}, got {}",
            expected_path.display(),
            discovered.scope.scope_root.display()
        );
    }

    let admin_repo = Repository::open(&discovered.scope.repo_root)?;
    let worktree = admin_repo
        .find_worktree(worktree_name)
        .with_context(|| format!("failed to find worktree {worktree_name}"))?;

    let mut prune_opts = WorktreePruneOptions::new();
    prune_opts.valid(true).locked(true).working_tree(true);
    worktree.prune(Some(&mut prune_opts)).with_context(|| {
        format!(
            "failed to prune worktree {worktree_name} from {}",
            discovered.scope.repo_root.display()
        )
    })?;

    if delete_branch {
        let branch_name = managed_branch_name(worktree_name);
        if let Ok(mut branch) = admin_repo.find_branch(&branch_name, BranchType::Local) {
            branch
                .delete()
                .with_context(|| format!("failed to delete branch {branch_name}"))?;
        }
    }

    Ok(())
}

pub fn ensure_orcashell_excluded(path: &Path) -> Result<()> {
    let discovered = discover_repo(path)?;
    let exclude_path = discovered.shared_admin_dir.join("info/exclude");
    ensure_orcashell_excluded_file(&exclude_path)
}

pub fn managed_worktree_path(repo_root: &Path, worktree_id: &str) -> PathBuf {
    repo_root.join(".orcashell/worktrees").join(worktree_id)
}

pub fn managed_branch_name(worktree_id: &str) -> String {
    format!("orca/{worktree_id}")
}

// ── Internal: repository discovery ───────────────────────────────────

fn discover_repo(path: &Path) -> Result<DiscoveredRepo> {
    let repo = Repository::discover(path)
        .with_context(|| format!("failed to discover git repository from {}", path.display()))?;
    if repo.is_bare() {
        bail!("bare repositories are not supported");
    }

    let scope_root = repo
        .workdir()
        .context("git repository has no working directory")?
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to canonicalize worktree {}",
                repo.workdir().unwrap().display()
            )
        })?;
    // Detect linked worktree BEFORE canonicalizing shared_admin_dir.
    // On Windows, canonicalize() adds a \\?\ prefix that git2's repo.path()
    // does not have, which would break the starts_with check.
    let is_worktree = repo.path().starts_with(repo.commondir().join("worktrees"));
    let shared_admin_dir = repo.commondir().canonicalize().with_context(|| {
        format!(
            "failed to canonicalize repository common dir {}",
            repo.commondir().display()
        )
    })?;
    let repo_root = if is_worktree {
        shared_admin_dir
            .parent()
            .context("repository common dir had no parent")?
            .canonicalize()
            .with_context(|| {
                format!(
                    "failed to canonicalize repo root from common dir {}",
                    shared_admin_dir.display()
                )
            })?
    } else {
        scope_root.clone()
    };
    let worktree_name = if is_worktree {
        scope_root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
    } else {
        None
    };

    Ok(DiscoveredRepo {
        repo,
        scope: GitScope {
            repo_root,
            scope_root,
            is_worktree,
            worktree_name,
        },
        shared_admin_dir,
    })
}

// ── Internal: diff building ──────────────────────────────────────────

fn resolve_head_tree(repo: &Repository) -> Result<git2::Tree<'_>> {
    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    let head_commit = peel_head_commit(&head)?;
    head_commit.tree().context("failed to load HEAD tree")
}

fn empty_tree(repo: &Repository) -> Result<git2::Tree<'_>> {
    let tree_id = repo
        .treebuilder(None)
        .context("failed to create empty tree builder")?
        .write()
        .context("failed to write empty tree")?;
    repo.find_tree(tree_id)
        .context("failed to load empty tree object")
}

fn build_tree_to_tree_diff<'repo>(
    repo: &'repo Repository,
    old_tree: Option<&git2::Tree<'repo>>,
    new_tree: Option<&git2::Tree<'repo>>,
) -> Result<Diff<'repo>> {
    let mut opts = DiffOptions::new();
    opts.include_typechange(true).ignore_submodules(true);

    let mut diff = repo
        .diff_tree_to_tree(old_tree, new_tree, Some(&mut opts))
        .context("failed to build tree diff")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on tree diff")?;
    Ok(diff)
}

fn build_staged_diff(repo: &Repository) -> Result<Diff<'_>> {
    let head_tree = resolve_head_tree(repo)?;
    let index = repo
        .index()
        .context("failed to read index for staged diff")?;

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).ignore_submodules(true);

    let mut diff = repo
        .diff_tree_to_index(Some(&head_tree), Some(&index), Some(&mut opts))
        .context("failed to diff HEAD against index")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on staged diff")?;

    Ok(diff)
}

fn build_unstaged_diff(repo: &Repository) -> Result<Diff<'_>> {
    let index = repo
        .index()
        .context("failed to read index for unstaged diff")?;

    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true)
        .include_unreadable_as_untracked(true)
        .ignore_submodules(true);

    let mut diff = repo
        .diff_index_to_workdir(Some(&index), Some(&mut opts))
        .context("failed to diff index against worktree")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true).for_untracked(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on unstaged diff")?;

    Ok(diff)
}

fn build_unstaged_apply_diff<'repo>(
    repo: &'repo Repository,
    paths: &[PathBuf],
    reverse: bool,
) -> Result<Diff<'repo>> {
    let index = repo
        .index()
        .context("failed to read index for unstaged discard diff")?;

    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true)
        .include_unreadable_as_untracked(true)
        .ignore_submodules(true)
        .show_binary(true)
        .reverse(reverse)
        .disable_pathspec_match(true);

    for path in paths {
        let path = path.to_str().context("path contains invalid UTF-8")?;
        opts.pathspec(path);
    }

    let mut diff = repo
        .diff_index_to_workdir(Some(&index), Some(&mut opts))
        .context("failed to diff index against worktree for discard")?;

    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true).for_untracked(true);
    diff.find_similar(Some(&mut find_opts))
        .context("failed to run rename detection on discard diff")?;

    Ok(diff)
}

fn collect_untracked_status_paths(repo: &Repository, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut opts = StatusOptions::new();
    opts.show(StatusShow::Workdir)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unreadable(true)
        .exclude_submodules(true)
        .disable_pathspec_match(true);

    for path in paths {
        let path = path.to_str().context("path contains invalid UTF-8")?;
        opts.pathspec(path);
    }

    let statuses = repo
        .statuses(Some(&mut opts))
        .context("failed to inspect untracked worktree paths for discard")?;
    let mut untracked = Vec::new();
    for entry in statuses.iter() {
        if !entry.status().contains(Status::WT_NEW) {
            continue;
        }
        let relative_path = entry
            .path()
            .map(PathBuf::from)
            .context("status entry missing path for untracked discard cleanup")?;
        untracked.push(relative_path);
    }
    untracked.sort();
    untracked.dedup();
    Ok(untracked)
}

enum UnstagedTargetInspection {
    Discardable,
    BlockedUnsupportedStatus,
    Missing,
}

enum CurrentUnstagedTargetStatus {
    Modified,
    Deleted,
    Untracked,
    BlockedUnsupportedStatus,
    Missing,
}

fn inspect_unstaged_target(
    repo: &Repository,
    relative_path: &Path,
) -> Result<UnstagedTargetInspection> {
    let current = inspect_current_unstaged_target_status(repo, relative_path)?;
    if matches!(
        current,
        CurrentUnstagedTargetStatus::BlockedUnsupportedStatus
    ) {
        return Ok(UnstagedTargetInspection::BlockedUnsupportedStatus);
    }
    if matches!(current, CurrentUnstagedTargetStatus::Missing) {
        return Ok(UnstagedTargetInspection::Missing);
    }
    if matches!(
        current,
        CurrentUnstagedTargetStatus::Modified | CurrentUnstagedTargetStatus::Deleted
    ) {
        return Ok(UnstagedTargetInspection::Discardable);
    }
    inspect_unstaged_target_with_full_diff(repo, relative_path)
}

fn inspect_current_unstaged_target_status(
    repo: &Repository,
    relative_path: &Path,
) -> Result<CurrentUnstagedTargetStatus> {
    let path = relative_path
        .to_str()
        .context("path contains invalid UTF-8")?;
    let mut opts = StatusOptions::new();
    opts.show(StatusShow::IndexAndWorkdir)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unreadable(true)
        .exclude_submodules(true)
        .disable_pathspec_match(true)
        .pathspec(path);

    let statuses = repo
        .statuses(Some(&mut opts))
        .context("failed to inspect discard target status")?;
    for entry in statuses.iter() {
        let status = entry.status();
        if status.intersects(Status::WT_RENAMED | Status::WT_TYPECHANGE) {
            return Ok(CurrentUnstagedTargetStatus::BlockedUnsupportedStatus);
        }
        if status.intersects(Status::WT_NEW) {
            return Ok(CurrentUnstagedTargetStatus::Untracked);
        }
        if status.intersects(Status::WT_MODIFIED | Status::WT_UNREADABLE) {
            return Ok(CurrentUnstagedTargetStatus::Modified);
        }
        if status.intersects(Status::WT_DELETED) {
            return Ok(CurrentUnstagedTargetStatus::Deleted);
        }
    }
    Ok(CurrentUnstagedTargetStatus::Missing)
}

fn inspect_unstaged_target_with_full_diff(
    repo: &Repository,
    relative_path: &Path,
) -> Result<UnstagedTargetInspection> {
    let diff = build_unstaged_diff(repo)?;
    let Some((_, delta)) = find_diff_delta_by_path(&diff, relative_path)? else {
        return Ok(UnstagedTargetInspection::Missing);
    };
    if is_unsupported_file_discard_status(map_status(delta.status())?) {
        return Ok(UnstagedTargetInspection::BlockedUnsupportedStatus);
    }
    Ok(UnstagedTargetInspection::Discardable)
}

fn remove_workdir_paths(scope_root: &Path, paths: &[PathBuf]) -> Result<()> {
    for relative_path in paths {
        let full_path = scope_root.join(relative_path);
        let metadata = match fs::symlink_metadata(&full_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", relative_path.display()));
            }
        };

        let file_type = metadata.file_type();
        if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(&full_path).with_context(|| {
                format!("failed to remove directory {}", relative_path.display())
            })?;
        } else if let Err(error) = fs::remove_file(&full_path) {
            return Err(error)
                .with_context(|| format!("failed to remove {}", relative_path.display()));
        }

        prune_empty_parent_dirs(scope_root, &full_path)?;
    }
    Ok(())
}

fn prune_empty_parent_dirs(scope_root: &Path, removed_path: &Path) -> Result<()> {
    let mut current = removed_path.parent();
    while let Some(directory) = current {
        if directory == scope_root {
            break;
        }

        match fs::remove_dir(directory) {
            Ok(()) => {
                current = directory.parent();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = directory.parent();
            }
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                break;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to prune {}", directory.display()));
            }
        }
    }
    Ok(())
}

fn remove_empty_untracked_file(
    scope_root: &Path,
    relative_path: &Path,
    status: GitFileStatus,
) -> Result<()> {
    if status != GitFileStatus::Untracked {
        return Ok(());
    }

    let full_path = scope_root.join(relative_path);
    let metadata = match fs::metadata(&full_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {}", relative_path.display()));
        }
    };

    if metadata.is_file() && metadata.len() == 0 {
        remove_workdir_paths(scope_root, &[relative_path.to_path_buf()])?;
    }

    Ok(())
}

fn snapshot_summary_from_split(
    scope: &GitScope,
    repo: &Repository,
    staged_diff: &Diff<'_>,
    unstaged_diff: &Diff<'_>,
    staged_files: &[ChangedFile],
    unstaged_files: &[ChangedFile],
    generation: u64,
) -> Result<GitSnapshotSummary> {
    let branch = branch_name(repo)?;
    let remotes = configured_remote_names(repo)?;
    let staged_stats = staged_diff
        .stats()
        .context("failed to compute staged diff statistics")?;
    let unstaged_stats = unstaged_diff
        .stats()
        .context("failed to compute unstaged diff statistics")?;

    // Aggregate stats: union of unique file paths across both diffs
    let mut unique_paths = HashSet::new();
    for f in staged_files {
        unique_paths.insert(&f.relative_path);
    }
    for f in unstaged_files {
        unique_paths.insert(&f.relative_path);
    }

    Ok(GitSnapshotSummary {
        repo_root: scope.repo_root.clone(),
        scope_root: scope.scope_root.clone(),
        generation,
        content_fingerprint: snapshot_content_fingerprint_split(
            scope,
            &branch,
            staged_diff,
            unstaged_diff,
            staged_files,
            unstaged_files,
        )?,
        branch_name: branch,
        remotes,
        is_worktree: scope.is_worktree,
        worktree_name: scope.worktree_name.clone(),
        changed_files: unique_paths.len(),
        insertions: staged_stats.insertions() + unstaged_stats.insertions(),
        deletions: staged_stats.deletions() + unstaged_stats.deletions(),
    })
}

fn configured_remote_names(repo: &Repository) -> Result<Vec<String>> {
    let remotes = repo.remotes().context("failed to read git remotes")?;
    let mut names = remotes
        .iter()
        .flatten()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}

fn collect_changed_files(
    diff: &Diff<'_>,
    excluded_paths: Option<&HashSet<PathBuf>>,
) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        let file = changed_file_from_delta(diff, idx, delta)
            .with_context(|| format!("failed to build changed-file entry at diff index {idx}"))?;
        if excluded_paths.is_some_and(|paths| paths.contains(&file.relative_path)) {
            continue;
        }
        files.push(file);
    }

    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn build_stash_file_diff_document(
    stash_oid: Oid,
    relative_path: &Path,
    mut rendered: SelectedDiffRender,
    theme_id: ThemeId,
) -> StashFileDiffDocument {
    if !rendered.file.is_binary {
        attach_syntax_highlights(&mut rendered.lines, &rendered.file.relative_path, theme_id);
        attach_inline_changes(&mut rendered.lines);
    }

    StashFileDiffDocument {
        stash_oid,
        selection: StashFileSelection {
            stash_oid,
            relative_path: relative_path.to_path_buf(),
        },
        file: rendered.file,
        lines: rendered.lines,
    }
}

fn stash_includes_untracked(commit: &Commit<'_>) -> bool {
    commit.parent_count() >= 3
}

#[derive(Debug, Clone)]
struct RawStashEntry {
    stash_index: usize,
    message: String,
    stash_oid: Oid,
}

impl RawStashEntry {
    fn label(&self) -> String {
        format!("stash@{{{}}}", self.stash_index)
    }
}

fn load_stash_entry_summaries(repo: &mut Repository) -> Result<Vec<StashEntrySummary>> {
    let raw_entries = collect_raw_stash_entries(repo)?;
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw_entry in raw_entries {
        let stash_commit = repo
            .find_commit(raw_entry.stash_oid)
            .with_context(|| format!("failed to find stash {}", short_oid(raw_entry.stash_oid)))?;
        entries.push(StashEntrySummary {
            stash_oid: raw_entry.stash_oid,
            stash_index: raw_entry.stash_index,
            label: format!("stash@{{{}}}", raw_entry.stash_index),
            message: raw_entry.message,
            committed_at_unix: stash_commit.committer().when().seconds(),
            includes_untracked: stash_includes_untracked(&stash_commit),
        });
    }
    Ok(entries)
}

fn collect_raw_stash_entries(repo: &mut Repository) -> Result<Vec<RawStashEntry>> {
    let mut entries = Vec::new();
    repo.stash_foreach(|stash_index, message, stash_oid| {
        entries.push(RawStashEntry {
            stash_index,
            message: message.to_string(),
            stash_oid: *stash_oid,
        });
        true
    })
    .context("failed to enumerate stash entries")?;
    Ok(entries)
}

fn resolve_stash_entry(repo: &mut Repository, stash_oid: Oid) -> Result<StashEntrySummary> {
    load_stash_entry_summaries(repo)?
        .into_iter()
        .find(|entry| entry.stash_oid == stash_oid)
        .with_context(|| format!("stash {} is no longer available", short_oid(stash_oid)))
}

fn resolve_raw_stash_entry(repo: &mut Repository, stash_oid: Oid) -> Result<RawStashEntry> {
    collect_raw_stash_entries(repo)?
        .into_iter()
        .find(|entry| entry.stash_oid == stash_oid)
        .with_context(|| format!("stash {} is no longer available", short_oid(stash_oid)))
}

fn load_stash_changed_files(
    repo: &Repository,
    stash_commit: &Commit<'_>,
) -> Result<Vec<ChangedFile>> {
    let tracked_files = load_stash_tracked_files(repo, stash_commit)?;
    let untracked_files = load_stash_untracked_files(repo, stash_commit)?;
    merge_stash_changed_files(tracked_files, untracked_files)
}

fn load_stash_tracked_files(
    repo: &Repository,
    stash_commit: &Commit<'_>,
) -> Result<Vec<ChangedFile>> {
    let stash_tree = stash_commit
        .tree()
        .context("failed to read stash commit tree")?;
    let base_commit = stash_commit
        .parent(0)
        .context("stash commit did not have a base parent")?;
    let base_tree = base_commit
        .tree()
        .context("failed to read stash base tree")?;
    let diff = build_tree_to_tree_diff(repo, Some(&base_tree), Some(&stash_tree))
        .context("failed to diff stash against its base parent")?;
    collect_changed_files(&diff, None)
}

fn load_stash_untracked_files(
    repo: &Repository,
    stash_commit: &Commit<'_>,
) -> Result<Vec<ChangedFile>> {
    if !stash_includes_untracked(stash_commit) {
        return Ok(Vec::new());
    }

    let untracked_commit = stash_commit
        .parent(2)
        .context("stash declared untracked content but was missing parent 2")?;
    let untracked_tree = untracked_commit
        .tree()
        .context("failed to read stash untracked tree")?;
    let empty_tree = empty_tree(repo)?;
    let diff = build_tree_to_tree_diff(repo, Some(&empty_tree), Some(&untracked_tree))
        .context("failed to diff stash untracked parent")?;
    let mut files = collect_changed_files(&diff, None)?;
    for file in &mut files {
        file.status = GitFileStatus::Untracked;
    }
    Ok(files)
}

fn merge_stash_changed_files(
    tracked_files: Vec<ChangedFile>,
    untracked_files: Vec<ChangedFile>,
) -> Result<Vec<ChangedFile>> {
    let mut merged = tracked_files
        .into_iter()
        .map(|file| (file.relative_path.clone(), file))
        .collect::<HashMap<_, _>>();
    for file in untracked_files {
        merged.entry(file.relative_path.clone()).or_insert(file);
    }

    let mut files = merged.into_values().collect::<Vec<_>>();
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn render_stash_tracked_target(
    repo: &Repository,
    stash_commit: &Commit<'_>,
    relative_path: &Path,
) -> Result<Option<SelectedDiffRender>> {
    let stash_tree = stash_commit
        .tree()
        .context("failed to read stash commit tree")?;
    let base_commit = stash_commit
        .parent(0)
        .context("stash commit did not have a base parent")?;
    let base_tree = base_commit
        .tree()
        .context("failed to read stash base tree")?;
    let diff = build_tree_to_tree_diff(repo, Some(&base_tree), Some(&stash_tree))
        .context("failed to diff stash against its base parent")?;
    render_target_from_diff(&diff, relative_path, false)
}

fn render_stash_untracked_target(
    repo: &Repository,
    stash_commit: &Commit<'_>,
    relative_path: &Path,
) -> Result<Option<SelectedDiffRender>> {
    if !stash_includes_untracked(stash_commit) {
        return Ok(None);
    }

    let untracked_commit = stash_commit
        .parent(2)
        .context("stash declared untracked content but was missing parent 2")?;
    let untracked_tree = untracked_commit
        .tree()
        .context("failed to read stash untracked tree")?;
    let empty_tree = empty_tree(repo)?;
    let diff = build_tree_to_tree_diff(repo, Some(&empty_tree), Some(&untracked_tree))
        .context("failed to diff stash untracked parent")?;
    let mut rendered = match render_target_from_diff(&diff, relative_path, false)? {
        Some(rendered) => rendered,
        None => return Ok(None),
    };
    rendered.file.status = GitFileStatus::Untracked;
    Ok(Some(rendered))
}

fn apply_stash_internal(path: &Path, stash_oid: Oid) -> Result<StashMutationOutcome> {
    let mut discovered = discover_repo(path)?;
    let repo = &mut discovered.repo;
    let target = resolve_raw_stash_entry(repo, stash_oid)?;
    let result = repo.stash_apply(target.stash_index, None);

    if repository_has_conflicts(repo)? {
        let conflicted_files = collect_conflicted_paths(repo)?;
        return Ok(StashMutationOutcome::Conflicted {
            affected_scope: discovered.scope.scope_root,
            conflicted_files,
        });
    }

    match result {
        Ok(()) => Ok(StashMutationOutcome::Applied {
            label: target.label(),
        }),
        Err(error) => Err(error).with_context(|| format!("failed to apply {}", target.label())),
    }
}

fn pop_stash_internal(path: &Path, stash_oid: Oid) -> Result<StashMutationOutcome> {
    let mut discovered = discover_repo(path)?;
    let repo = &mut discovered.repo;
    let target = resolve_raw_stash_entry(repo, stash_oid)?;
    let result = repo.stash_apply(target.stash_index, None);

    if repository_has_conflicts(repo)? {
        let conflicted_files = collect_conflicted_paths(repo)?;
        return Ok(StashMutationOutcome::Conflicted {
            affected_scope: discovered.scope.scope_root,
            conflicted_files,
        });
    }

    match result {
        Ok(()) => {
            let drop_target = resolve_raw_stash_entry(repo, stash_oid)?;
            repo.stash_drop(drop_target.stash_index)
                .with_context(|| format!("failed to drop {} after apply", drop_target.label()))?;
            Ok(StashMutationOutcome::Applied {
                label: drop_target.label(),
            })
        }
        Err(error) => Err(error).with_context(|| format!("failed to pop {}", target.label())),
    }
}

fn repository_has_conflicts(repo: &Repository) -> Result<bool> {
    let index = repo.index().context("failed to read index for conflicts")?;
    Ok(index.has_conflicts())
}

fn collect_conflicted_paths(repo: &Repository) -> Result<Vec<PathBuf>> {
    let index = repo.index().context("failed to read index for conflicts")?;
    let conflicts = index
        .conflicts()
        .context("failed to iterate index conflicts")?;

    let mut paths = Vec::new();
    for conflict in conflicts {
        let conflict = conflict.context("failed to read index conflict")?;
        let path = conflict
            .our
            .as_ref()
            .or(conflict.their.as_ref())
            .or(conflict.ancestor.as_ref())
            .map(|entry| PathBuf::from(String::from_utf8_lossy(&entry.path).into_owned()))
            .context("conflict entry had no path")?;
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn conflicted_changed_file(relative_path: PathBuf) -> ChangedFile {
    ChangedFile {
        relative_path,
        status: GitFileStatus::Conflicted,
        is_binary: false,
        insertions: 0,
        deletions: 0,
    }
}

// ── Internal: fingerprinting ─────────────────────────────────────────

fn snapshot_content_fingerprint_split(
    scope: &GitScope,
    branch_name: &str,
    staged_diff: &Diff<'_>,
    unstaged_diff: &Diff<'_>,
    staged_files: &[ChangedFile],
    unstaged_files: &[ChangedFile],
) -> Result<u64> {
    let mut fingerprint = 0xcbf29ce484222325u64;
    fingerprint_bytes(&mut fingerprint, branch_name.as_bytes());
    fingerprint_u8(&mut fingerprint, scope.is_worktree as u8);
    if let Some(worktree_name) = scope.worktree_name.as_deref() {
        fingerprint_bytes(&mut fingerprint, worktree_name.as_bytes());
    } else {
        fingerprint_u8(&mut fingerprint, 0xff);
    }
    // Staged section discriminant + files
    fingerprint_u8(&mut fingerprint, 0x01);
    fingerprint_usize(&mut fingerprint, staged_files.len());
    fingerprint_diff_content(&mut fingerprint, staged_diff)?;
    // Unstaged section discriminant + files
    fingerprint_u8(&mut fingerprint, 0x02);
    fingerprint_usize(&mut fingerprint, unstaged_files.len());
    fingerprint_diff_content(&mut fingerprint, unstaged_diff)?;
    Ok(fingerprint)
}

fn fingerprint_diff_content(fingerprint: &mut u64, diff: &Diff<'_>) -> Result<()> {
    for (idx, delta) in diff.deltas().enumerate() {
        let file = changed_file_from_delta(diff, idx, delta)
            .with_context(|| format!("failed to fingerprint changed file at diff index {idx}"))?;
        let relative_path = file.relative_path.to_string_lossy();
        fingerprint_bytes(fingerprint, relative_path.as_bytes());
        fingerprint_u8(fingerprint, git_file_status_code(file.status));
        fingerprint_u8(fingerprint, file.is_binary as u8);
        fingerprint_usize(fingerprint, file.insertions);
        fingerprint_usize(fingerprint, file.deletions);
        for line in render_diff_lines(diff, idx, &file)? {
            fingerprint_u8(fingerprint, diff_line_kind_code(line.kind));
            fingerprint_bytes(fingerprint, line.text.as_bytes());
        }
    }
    Ok(())
}

fn fingerprint_bytes(fingerprint: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *fingerprint ^= u64::from(*byte);
        *fingerprint = fingerprint.wrapping_mul(0x100000001b3);
    }
    *fingerprint ^= 0xff;
    *fingerprint = fingerprint.wrapping_mul(0x100000001b3);
}

fn fingerprint_u8(fingerprint: &mut u64, value: u8) {
    fingerprint_bytes(fingerprint, &[value]);
}

fn fingerprint_usize(fingerprint: &mut u64, value: usize) {
    fingerprint_bytes(fingerprint, &value.to_le_bytes());
}

fn git_file_status_code(status: GitFileStatus) -> u8 {
    match status {
        GitFileStatus::Added => 1,
        GitFileStatus::Modified => 2,
        GitFileStatus::Deleted => 3,
        GitFileStatus::Renamed => 4,
        GitFileStatus::Typechange => 5,
        GitFileStatus::Untracked => 6,
        GitFileStatus::Conflicted => 7,
    }
}

fn diff_line_kind_code(kind: DiffLineKind) -> u8 {
    match kind {
        DiffLineKind::FileHeader => 1,
        DiffLineKind::HunkHeader => 2,
        DiffLineKind::Context => 3,
        DiffLineKind::Addition => 4,
        DiffLineKind::Deletion => 5,
        DiffLineKind::BinaryNotice => 6,
        DiffLineKind::ConflictMarker => 7,
        DiffLineKind::ConflictOurs => 8,
        DiffLineKind::ConflictBase => 9,
        DiffLineKind::ConflictTheirs => 10,
    }
}

fn filtered_diff_lines(lines: &[DiffLineView]) -> Vec<&DiffLineView> {
    lines
        .iter()
        .filter(|line| line.kind != DiffLineKind::FileHeader)
        .collect()
}

fn normalized_hunk_body_from_filtered_lines(
    filtered_lines: &[&DiffLineView],
    hunk: &FileDiffHunk,
) -> Result<String> {
    let header = filtered_lines
        .get(hunk.line_start)
        .context("hunk start line missing from filtered diff lines")?;
    if header.kind != DiffLineKind::HunkHeader {
        bail!("hunk start did not point at a hunk header");
    }
    let body_lines = filtered_lines
        .get(hunk.line_start + 1..hunk.line_end)
        .context("hunk body range missing from filtered diff lines")?;
    Ok(serialize_hunk_body(body_lines))
}

fn serialize_hunk_body(lines: &[&DiffLineView]) -> String {
    let mut body = String::new();
    for line in lines {
        body.push_str(&diff_line_kind_code(line.kind).to_string());
        body.push('\0');
        body.push_str(&line.text.len().to_string());
        body.push('\0');
        body.push_str(&line.text);
        body.push('\u{1}');
    }
    body
}

fn fingerprint_hunk_body(body: &str) -> u64 {
    let mut fingerprint = 0xcbf29ce484222325u64;
    fingerprint_bytes(&mut fingerprint, body.as_bytes());
    fingerprint
}

fn discard_hunk_target_from_lines(
    lines: &[DiffLineView],
    hunks: &[FileDiffHunk],
    hunk_index: usize,
) -> Option<DiscardHunkTarget> {
    let filtered_lines = filtered_diff_lines(lines);
    discard_hunk_target_from_filtered_lines(&filtered_lines, hunks, hunk_index)
}

fn discard_hunk_target_from_filtered_lines(
    filtered_lines: &[&DiffLineView],
    hunks: &[FileDiffHunk],
    hunk_index: usize,
) -> Option<DiscardHunkTarget> {
    let hunk = hunks.iter().find(|hunk| hunk.hunk_index == hunk_index)?;
    let body = normalized_hunk_body_from_filtered_lines(filtered_lines, hunk).ok()?;
    Some(DiscardHunkTarget {
        hunk_index: hunk.hunk_index,
        body_fingerprint: hunk.body_fingerprint,
        old_start: hunk.old_start,
        old_lines: hunk.old_lines,
        new_start: hunk.new_start,
        new_lines: hunk.new_lines,
        body,
    })
}

fn same_hunk_range(hunk: &FileDiffHunk, target: &DiscardHunkTarget) -> bool {
    hunk.old_start == target.old_start
        && hunk.old_lines == target.old_lines
        && hunk.new_start == target.new_start
        && hunk.new_lines == target.new_lines
}

fn resolve_hunk_discard_index(
    lines: &[DiffLineView],
    hunks: &[FileDiffHunk],
    target: &DiscardHunkTarget,
) -> Result<usize, String> {
    let filtered_lines = filtered_diff_lines(lines);
    if let Some(candidate) =
        discard_hunk_target_from_filtered_lines(&filtered_lines, hunks, target.hunk_index)
    {
        if candidate.body_fingerprint == target.body_fingerprint
            && candidate.body == target.body
            && candidate.old_start == target.old_start
            && candidate.old_lines == target.old_lines
            && candidate.new_start == target.new_start
            && candidate.new_lines == target.new_lines
        {
            return Ok(target.hunk_index);
        }
    }

    let exact_matches = hunks
        .iter()
        .filter_map(|hunk| {
            let candidate =
                discard_hunk_target_from_filtered_lines(&filtered_lines, hunks, hunk.hunk_index)?;
            if candidate.body_fingerprint == target.body_fingerprint
                && candidate.body == target.body
            {
                Some((hunk.hunk_index, hunk))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    match exact_matches.as_slice() {
        [(matched_index, _)] => Ok(*matched_index),
        [] => Err("Selected hunk changed. Refresh and try again.".to_string()),
        _ => {
            let range_matches = exact_matches
                .iter()
                .filter(|(_, hunk)| same_hunk_range(hunk, target))
                .map(|(index, _)| *index)
                .collect::<Vec<_>>();
            if range_matches.len() == 1 {
                Ok(range_matches[0])
            } else {
                Err("Selected hunk changed. Refresh and try again.".to_string())
            }
        }
    }
}

fn is_unsupported_file_discard_status(status: GitFileStatus) -> bool {
    matches!(status, GitFileStatus::Renamed | GitFileStatus::Typechange)
}

fn repo_state_warning(state: RepositoryState) -> Option<String> {
    match state {
        RepositoryState::Clean | RepositoryState::Merge => None,
        RepositoryState::Revert => Some("Repository is in a revert state.".to_string()),
        RepositoryState::RevertSequence => {
            Some("Repository is in a revert-sequence state.".to_string())
        }
        RepositoryState::CherryPick => Some("Repository is in a cherry-pick state.".to_string()),
        RepositoryState::CherryPickSequence => {
            Some("Repository is in a cherry-pick-sequence state.".to_string())
        }
        RepositoryState::Bisect => Some("Repository is in a bisect state.".to_string()),
        RepositoryState::Rebase => Some("Repository is in a rebase state.".to_string()),
        RepositoryState::RebaseInteractive => {
            Some("Repository is in an interactive rebase state.".to_string())
        }
        RepositoryState::RebaseMerge => Some("Repository is in a rebase-merge state.".to_string()),
        RepositoryState::ApplyMailbox => {
            Some("Repository is applying a mailbox patch.".to_string())
        }
        RepositoryState::ApplyMailboxOrRebase => {
            Some("Repository is applying a mailbox patch or continuing a rebase.".to_string())
        }
    }
}

fn ensure_discard_repo_state(repo: &Repository) -> Result<()> {
    match repo.state() {
        RepositoryState::Clean => Ok(()),
        RepositoryState::Merge => bail!("discard actions are unavailable while a merge is active"),
        state => {
            let warning = repo_state_warning(state)
                .unwrap_or_else(|| format!("discard actions are unavailable in {state:?} state"));
            bail!("{warning}");
        }
    }
}

fn split_lines_inclusive(text: &str) -> Vec<&str> {
    let mut lines = text.split_inclusive('\n').collect::<Vec<_>>();
    if !text.is_empty() && !text.ends_with('\n') {
        let consumed = lines.iter().map(|line| line.len()).sum::<usize>();
        lines.push(&text[consumed..]);
    }
    lines
}

// ── Internal: diff rendering ─────────────────────────────────────────

fn changed_file_from_delta(
    diff: &Diff<'_>,
    idx: usize,
    delta: git2::DiffDelta<'_>,
) -> Result<ChangedFile> {
    let relative_path = delta_path(&delta).context("diff delta did not contain a path")?;
    let status = map_status(delta.status())?;
    let patch = Patch::from_diff(diff, idx).context("failed to create patch from diff")?;
    let (insertions, deletions) = if let Some(ref patch) = patch {
        let (_, insertions, deletions) =
            patch.line_stats().context("failed to collect line stats")?;
        (insertions, deletions)
    } else {
        (0, 0)
    };

    Ok(ChangedFile {
        relative_path,
        status,
        is_binary: delta.old_file().is_binary() || delta.new_file().is_binary(),
        insertions,
        deletions,
    })
}

struct SelectedDiffRender {
    file: ChangedFile,
    lines: Vec<DiffLineView>,
    hunks: Vec<FileDiffHunk>,
}

fn render_target_from_diff(
    diff: &Diff<'_>,
    relative_path: &Path,
    include_hunks: bool,
) -> Result<Option<SelectedDiffRender>> {
    let Some((idx, delta)) = find_diff_delta_by_path(diff, relative_path)? else {
        return Ok(None);
    };
    render_selected_diff_entry(diff, idx, delta, include_hunks).map(Some)
}

fn render_unstaged_target_with_path_filter(
    repo: &Repository,
    relative_path: &Path,
    include_hunks: bool,
) -> Result<Option<SelectedDiffRender>> {
    let diff = build_unstaged_apply_diff(repo, &[relative_path.to_path_buf()], false)?;
    render_target_from_diff(&diff, relative_path, include_hunks)
}

fn render_selected_diff_entry(
    diff: &Diff<'_>,
    idx: usize,
    delta: git2::DiffDelta<'_>,
    include_hunks: bool,
) -> Result<SelectedDiffRender> {
    let relative_path = delta_path(&delta).context("diff delta did not contain a path")?;
    let status = map_status(delta.status())?;
    let is_binary = delta.old_file().is_binary() || delta.new_file().is_binary();
    let patch = Patch::from_diff(diff, idx).context("failed to create patch from diff")?;
    let (insertions, deletions) = if let Some(ref patch) = patch {
        let (_, insertions, deletions) =
            patch.line_stats().context("failed to collect line stats")?;
        (insertions, deletions)
    } else {
        (0, 0)
    };
    let mut file = ChangedFile {
        relative_path,
        status,
        is_binary,
        insertions,
        deletions,
    };

    if file.is_binary {
        return Ok(SelectedDiffRender {
            file,
            lines: vec![binary_notice(BINARY_DIFF_MESSAGE)],
            hunks: Vec::new(),
        });
    }

    let Some(mut patch) = patch else {
        return Ok(SelectedDiffRender {
            file,
            lines: vec![no_text_diff_notice()],
            hunks: Vec::new(),
        });
    };

    if patch.size(true, true, true) > MAX_RENDERED_DIFF_BYTES
        || estimated_patch_line_count(&patch)? > MAX_RENDERED_DIFF_LINES
    {
        return Ok(SelectedDiffRender {
            file,
            lines: vec![binary_notice(OVERSIZE_DIFF_MESSAGE)],
            hunks: Vec::new(),
        });
    }

    let mut lines = Vec::new();
    let mut hunks = Vec::new();
    let mut open_hunk: Option<FileDiffHunk> = None;
    let mut filtered_line_index = 0usize;
    let mut render_error = None;
    patch
        .print(&mut |_delta, hunk, line| {
            let kind = map_line_kind(line.origin_value());
            if include_hunks && kind == DiffLineKind::HunkHeader {
                if let Some(mut previous) = open_hunk.take() {
                    previous.line_end = filtered_line_index;
                    hunks.push(previous);
                }
                let Some(hunk) = hunk else {
                    render_error = Some(anyhow!(
                        "libgit2 omitted hunk metadata for a rendered hunk header"
                    ));
                    return false;
                };
                open_hunk = Some(FileDiffHunk {
                    hunk_index: hunks.len(),
                    old_start: hunk.old_start(),
                    old_lines: hunk.old_lines(),
                    new_start: hunk.new_start(),
                    new_lines: hunk.new_lines(),
                    header: String::from_utf8_lossy(hunk.header())
                        .trim_end()
                        .to_string(),
                    body_fingerprint: 0,
                    line_start: filtered_line_index,
                    line_end: filtered_line_index,
                });
            }

            lines.push(DiffLineView {
                kind,
                old_lineno: line.old_lineno(),
                new_lineno: line.new_lineno(),
                text: String::from_utf8_lossy(line.content()).into_owned(),
                highlights: None,
                inline_changes: None,
            });
            if kind != DiffLineKind::FileHeader {
                filtered_line_index += 1;
            }
            true
        })
        .context("failed to render patch lines")?;
    if let Some(error) = render_error {
        return Err(error);
    }

    if let Some(mut last_hunk) = open_hunk.take() {
        last_hunk.line_end = filtered_line_index;
        hunks.push(last_hunk);
    }

    if lines
        .iter()
        .any(|line| line.kind == DiffLineKind::BinaryNotice)
    {
        file.is_binary = true;
        return Ok(SelectedDiffRender {
            file,
            lines: vec![binary_notice(BINARY_DIFF_MESSAGE)],
            hunks: Vec::new(),
        });
    }

    if include_hunks && !hunks.is_empty() {
        let filtered_lines = filtered_diff_lines(&lines);
        for hunk in &mut hunks {
            let body = normalized_hunk_body_from_filtered_lines(&filtered_lines, hunk)
                .context("failed to normalize rendered hunk body")?;
            hunk.body_fingerprint = fingerprint_hunk_body(&body);
        }
    } else {
        hunks.clear();
    }

    Ok(SelectedDiffRender { file, lines, hunks })
}

fn render_diff_lines(diff: &Diff<'_>, idx: usize, file: &ChangedFile) -> Result<Vec<DiffLineView>> {
    if file.is_binary {
        return Ok(vec![binary_notice(BINARY_DIFF_MESSAGE)]);
    }

    let patch = Patch::from_diff(diff, idx).context("failed to create patch from diff")?;
    let Some(mut patch) = patch else {
        return Ok(vec![no_text_diff_notice()]);
    };

    if patch.size(true, true, true) > MAX_RENDERED_DIFF_BYTES
        || estimated_patch_line_count(&patch)? > MAX_RENDERED_DIFF_LINES
    {
        return Ok(vec![binary_notice(OVERSIZE_DIFF_MESSAGE)]);
    }

    let mut lines = Vec::new();
    patch
        .print(&mut |_delta, _hunk, line| {
            lines.push(DiffLineView {
                kind: map_line_kind(line.origin_value()),
                old_lineno: line.old_lineno(),
                new_lineno: line.new_lineno(),
                text: String::from_utf8_lossy(line.content()).into_owned(),
                highlights: None,
                inline_changes: None,
            });
            true
        })
        .context("failed to render patch lines")?;

    if lines.is_empty() && file.is_binary {
        return Ok(vec![binary_notice(BINARY_DIFF_MESSAGE)]);
    }

    Ok(lines)
}

fn capture_feed_scope_from_split(
    staged_diff: &Diff<'_>,
    unstaged_diff: &Diff<'_>,
    generation: u64,
    theme_id: ThemeId,
) -> Result<FeedScopeCapture> {
    let mut layers =
        capture_feed_layers(staged_diff, DiffSectionKind::Staged, generation, theme_id)?;
    layers.extend(capture_feed_layers(
        unstaged_diff,
        DiffSectionKind::Unstaged,
        generation,
        theme_id,
    )?);
    layers.sort_by(|left, right| {
        feed_selection_sort_key(&left.selection).cmp(&feed_selection_sort_key(&right.selection))
    });
    Ok(FeedScopeCapture { generation, layers })
}

fn capture_feed_layers(
    diff: &Diff<'_>,
    section: DiffSectionKind,
    generation: u64,
    theme_id: ThemeId,
) -> Result<Vec<FeedLayerSnapshot>> {
    let mut layers = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        let file = changed_file_from_delta(diff, idx, delta)
            .with_context(|| format!("failed to capture feed layer at diff index {idx}"))?;
        let selection = DiffSelectionKey {
            section,
            relative_path: file.relative_path.clone(),
        };
        let state = match render_diff_lines(diff, idx, &file) {
            Ok(mut lines) => {
                if !file.is_binary {
                    attach_syntax_highlights(&mut lines, &selection.relative_path, theme_id);
                    attach_inline_changes(&mut lines);
                }
                FeedLayerState::Ready(FileDiffDocument {
                    generation,
                    selection: selection.clone(),
                    file: file.clone(),
                    lines,
                    hunks: Vec::new(),
                })
            }
            Err(error) => FeedLayerState::Unavailable {
                message: error.to_string(),
            },
        };
        layers.push(FeedLayerSnapshot {
            selection,
            file,
            state,
        });
    }
    Ok(layers)
}

fn derive_feed_event(
    previous: Option<&FeedScopeCapture>,
    current: &FeedScopeCapture,
    theme_id: ThemeId,
) -> Result<Option<FeedEventData>> {
    match previous {
        None => {
            if current.layers.is_empty() {
                Ok(None)
            } else {
                Ok(Some(build_bootstrap_feed_event(current)))
            }
        }
        Some(previous) => build_live_delta_event(previous, current, theme_id),
    }
}

fn build_bootstrap_feed_event(current: &FeedScopeCapture) -> FeedEventData {
    let summaries = summarize_feed_layers(current.layers.iter().collect());
    let (insertions, deletions) = count_feed_scope_changes(current);
    let capture = build_feed_capture_from_layers(
        current.layers.iter().collect(),
        FeedEventKind::BootstrapSnapshot,
    );
    FeedEventData {
        kind: FeedEventKind::BootstrapSnapshot,
        changed_file_count: summaries.len(),
        insertions,
        deletions,
        files: summaries.into_iter().take(FEED_EVENT_FILE_CAP).collect(),
        capture,
    }
}

fn build_live_delta_event(
    previous: &FeedScopeCapture,
    current: &FeedScopeCapture,
    theme_id: ThemeId,
) -> Result<Option<FeedEventData>> {
    let previous_layers = previous
        .layers
        .iter()
        .map(|layer| (layer.selection.clone(), layer))
        .collect::<HashMap<_, _>>();
    let current_layers = current
        .layers
        .iter()
        .map(|layer| (layer.selection.clone(), layer))
        .collect::<HashMap<_, _>>();

    let mut keys = previous_layers
        .keys()
        .chain(current_layers.keys())
        .cloned()
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| feed_selection_sort_key(left).cmp(&feed_selection_sort_key(right)));
    keys.dedup();

    let mut summaries = Vec::new();
    let mut event_files = Vec::new();
    let mut failed_files = Vec::new();
    let mut total_rendered_lines = 0usize;
    let mut total_rendered_bytes = 0usize;
    let mut truncated = false;
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    for selection in keys {
        let previous_layer = previous_layers.get(&selection).copied();
        let current_layer = current_layers.get(&selection).copied();
        if !feed_layer_changed(previous_layer, current_layer) {
            continue;
        }

        let file = current_layer
            .map(|layer| layer.file.clone())
            .or_else(|| previous_layer.map(|layer| layer.file.clone()))
            .expect("union key must resolve to at least one layer");
        summaries.push(feed_summary_from_layers(
            previous_layer,
            current_layer,
            &file,
        ));

        match build_delta_file_document(
            previous_layer,
            current_layer,
            current.generation,
            theme_id,
        )? {
            DeltaFileOutcome::Document {
                document,
                insertions: document_insertions,
                deletions: document_deletions,
            } => {
                insertions += document_insertions;
                deletions += document_deletions;
                if event_files.len() >= FEED_EVENT_FILE_CAP {
                    truncated = true;
                    continue;
                }
                let remaining_lines = FEED_EVENT_LINE_CAP.saturating_sub(total_rendered_lines);
                let remaining_bytes = FEED_EVENT_BYTE_CAP.saturating_sub(total_rendered_bytes);
                let (document, used_lines, used_bytes, document_truncated) =
                    truncate_feed_file_document(&document, remaining_lines, remaining_bytes);
                if !document.lines.is_empty() || document.file.is_binary {
                    total_rendered_lines += used_lines;
                    total_rendered_bytes += used_bytes;
                    truncated |= document_truncated;
                    event_files.push(FeedEventFile {
                        selection: selection.clone(),
                        file: document.file.clone(),
                        document,
                    });
                }
                if total_rendered_lines >= FEED_EVENT_LINE_CAP
                    || total_rendered_bytes >= FEED_EVENT_BYTE_CAP
                {
                    truncated = true;
                }
            }
            DeltaFileOutcome::Unavailable(message) => {
                failed_files.push(FeedEventFailure {
                    selection: selection.clone(),
                    relative_path: selection.relative_path.clone(),
                    message,
                });
            }
            DeltaFileOutcome::NoContent => {}
        }
    }

    if summaries.is_empty() {
        return Ok(None);
    }

    let capture = FeedCapturedEvent {
        files: event_files,
        failed_files,
        truncated,
        total_rendered_lines,
        total_rendered_bytes,
    };
    Ok(Some(FeedEventData {
        kind: FeedEventKind::LiveDelta,
        changed_file_count: summaries.len(),
        insertions,
        deletions,
        files: summaries.into_iter().take(FEED_EVENT_FILE_CAP).collect(),
        capture,
    }))
}

fn summarize_feed_layers(layers: Vec<&FeedLayerSnapshot>) -> Vec<FeedEventFileSummary> {
    let mut ordered = Vec::new();
    let mut positions = HashMap::<PathBuf, usize>::new();

    for layer in layers {
        if let Some(index) = positions.get(&layer.selection.relative_path).copied() {
            let summary: &mut FeedEventFileSummary = &mut ordered[index];
            match layer.selection.section {
                DiffSectionKind::Conflicted => {}
                DiffSectionKind::Staged => summary.staged = true,
                DiffSectionKind::Unstaged => summary.unstaged = true,
            }
            summary.status = layer.file.status;
            summary.is_binary = layer.file.is_binary;
            summary.insertions = layer.file.insertions;
            summary.deletions = layer.file.deletions;
        } else {
            let mut summary = FeedEventFileSummary {
                relative_path: layer.selection.relative_path.clone(),
                staged: false,
                unstaged: false,
                status: layer.file.status,
                is_binary: layer.file.is_binary,
                insertions: layer.file.insertions,
                deletions: layer.file.deletions,
            };
            match layer.selection.section {
                DiffSectionKind::Conflicted => {}
                DiffSectionKind::Staged => summary.staged = true,
                DiffSectionKind::Unstaged => summary.unstaged = true,
            }
            positions.insert(summary.relative_path.clone(), ordered.len());
            ordered.push(summary);
        }
    }

    ordered
}

fn feed_summary_from_layers(
    previous: Option<&FeedLayerSnapshot>,
    current: Option<&FeedLayerSnapshot>,
    file: &ChangedFile,
) -> FeedEventFileSummary {
    let mut summary = FeedEventFileSummary {
        relative_path: file.relative_path.clone(),
        staged: false,
        unstaged: false,
        status: file.status,
        is_binary: file.is_binary,
        insertions: file.insertions,
        deletions: file.deletions,
    };
    for layer in [previous, current].into_iter().flatten() {
        match layer.selection.section {
            DiffSectionKind::Conflicted => {}
            DiffSectionKind::Staged => summary.staged = true,
            DiffSectionKind::Unstaged => summary.unstaged = true,
        }
        summary.status = layer.file.status;
        summary.is_binary = layer.file.is_binary;
        summary.insertions = layer.file.insertions;
        summary.deletions = layer.file.deletions;
    }
    summary
}

fn build_feed_capture_from_layers(
    layers: Vec<&FeedLayerSnapshot>,
    _kind: FeedEventKind,
) -> FeedCapturedEvent {
    let mut files = Vec::new();
    let mut failed_files = Vec::new();
    let mut total_rendered_lines = 0usize;
    let mut total_rendered_bytes = 0usize;
    let mut truncated = false;

    for layer in layers {
        match &layer.state {
            FeedLayerState::Ready(document) => {
                if files.len() >= FEED_EVENT_FILE_CAP {
                    truncated = true;
                    continue;
                }
                let remaining_lines = FEED_EVENT_LINE_CAP.saturating_sub(total_rendered_lines);
                let remaining_bytes = FEED_EVENT_BYTE_CAP.saturating_sub(total_rendered_bytes);
                let (document, used_lines, used_bytes, document_truncated) =
                    truncate_feed_file_document(document, remaining_lines, remaining_bytes);
                total_rendered_lines += used_lines;
                total_rendered_bytes += used_bytes;
                truncated |= document_truncated;
                if !document.lines.is_empty() || document.file.is_binary {
                    files.push(FeedEventFile {
                        selection: layer.selection.clone(),
                        file: layer.file.clone(),
                        document,
                    });
                }
            }
            FeedLayerState::Unavailable { message } => failed_files.push(FeedEventFailure {
                selection: layer.selection.clone(),
                relative_path: layer.selection.relative_path.clone(),
                message: message.clone(),
            }),
        }
        if total_rendered_lines >= FEED_EVENT_LINE_CAP
            || total_rendered_bytes >= FEED_EVENT_BYTE_CAP
        {
            truncated = true;
        }
    }

    FeedCapturedEvent {
        files,
        failed_files,
        truncated,
        total_rendered_lines,
        total_rendered_bytes,
    }
}

fn feed_layer_changed(
    previous: Option<&FeedLayerSnapshot>,
    current: Option<&FeedLayerSnapshot>,
) -> bool {
    match (previous, current) {
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => true,
        (Some(previous), Some(current)) => {
            previous.file != current.file || !previous.state.content_eq(&current.state)
        }
    }
}

enum DeltaFileOutcome {
    Document {
        document: FileDiffDocument,
        insertions: usize,
        deletions: usize,
    },
    Unavailable(String),
    NoContent,
}

fn build_delta_file_document(
    previous: Option<&FeedLayerSnapshot>,
    current: Option<&FeedLayerSnapshot>,
    generation: u64,
    theme_id: ThemeId,
) -> Result<DeltaFileOutcome> {
    let selection = current
        .map(|layer| layer.selection.clone())
        .or_else(|| previous.map(|layer| layer.selection.clone()))
        .expect("delta layer must exist on one side");
    let file = current
        .map(|layer| layer.file.clone())
        .or_else(|| previous.map(|layer| layer.file.clone()))
        .expect("delta file metadata must exist on one side");

    let lines = match (
        previous.map(|layer| &layer.state),
        current.map(|layer| &layer.state),
    ) {
        (
            Some(FeedLayerState::Unavailable { message }),
            Some(FeedLayerState::Unavailable { .. }),
        )
        | (Some(FeedLayerState::Unavailable { message }), None)
        | (None, Some(FeedLayerState::Unavailable { message }))
        | (Some(FeedLayerState::Ready(_)), Some(FeedLayerState::Unavailable { message }))
        | (Some(FeedLayerState::Unavailable { message }), Some(FeedLayerState::Ready(_))) => {
            return Ok(DeltaFileOutcome::Unavailable(format!(
                "Historical event payload unavailable for {} [{}]: {message}",
                selection.relative_path.display(),
                feed_section_label(selection.section),
            )));
        }
        (None, Some(FeedLayerState::Ready(document))) => document
            .lines
            .iter()
            .map(delta_view_line_from_added)
            .collect::<Vec<_>>(),
        (Some(FeedLayerState::Ready(document)), None) => document
            .lines
            .iter()
            .map(delta_view_line_from_removed)
            .collect::<Vec<_>>(),
        (Some(FeedLayerState::Ready(previous)), Some(FeedLayerState::Ready(current))) => {
            diff_feed_documents(previous, current)
        }
        (None, None) => Vec::new(),
    };

    if lines.is_empty() {
        return Ok(DeltaFileOutcome::NoContent);
    }

    let (insertions, deletions) = count_diff_line_changes(&lines);
    let mut normalized_file = file;
    normalized_file.insertions = insertions;
    normalized_file.deletions = deletions;

    let mut document = FileDiffDocument {
        generation,
        selection: selection.clone(),
        file: normalized_file,
        lines,
        hunks: Vec::new(),
    };
    if !document.file.is_binary {
        attach_syntax_highlights(&mut document.lines, &selection.relative_path, theme_id);
        attach_inline_changes(&mut document.lines);
    }
    Ok(DeltaFileOutcome::Document {
        document,
        insertions,
        deletions,
    })
}

fn diff_feed_documents(
    previous: &FileDiffDocument,
    current: &FileDiffDocument,
) -> Vec<DiffLineView> {
    let mut input = imara_diff::intern::InternedInput::default();
    let before = previous
        .lines
        .iter()
        .map(|line| format!("{}\u{0}{}", diff_line_kind_code(line.kind), line.text))
        .collect::<Vec<_>>();
    let after = current
        .lines
        .iter()
        .map(|line| format!("{}\u{0}{}", diff_line_kind_code(line.kind), line.text))
        .collect::<Vec<_>>();

    input.update_before(before.iter().map(String::as_str));
    input.update_after(after.iter().map(String::as_str));

    let mut ranges = Vec::<(Range<u32>, Range<u32>)>::new();
    imara_diff::diff(
        imara_diff::Algorithm::Histogram,
        &input,
        |before: Range<u32>, after: Range<u32>| {
            ranges.push((before, after));
        },
    );

    let mut lines = Vec::new();
    for (before, after) in ranges {
        for idx in before.start..before.end {
            lines.push(delta_view_line_from_removed(&previous.lines[idx as usize]));
        }
        for idx in after.start..after.end {
            lines.push(delta_view_line_from_added(&current.lines[idx as usize]));
        }
    }
    lines
}

fn delta_view_line_from_added(line: &DiffLineView) -> DiffLineView {
    let (old_lineno, new_lineno) = event_line_numbers_for_added(line);
    DiffLineView {
        kind: added_event_kind(line.kind),
        old_lineno,
        new_lineno,
        text: line.text.clone(),
        highlights: None,
        inline_changes: None,
    }
}

fn delta_view_line_from_removed(line: &DiffLineView) -> DiffLineView {
    let (old_lineno, new_lineno) = event_line_numbers_for_removed(line);
    DiffLineView {
        kind: removed_event_kind(line.kind),
        old_lineno,
        new_lineno,
        text: line.text.clone(),
        highlights: None,
        inline_changes: None,
    }
}

fn added_event_kind(kind: DiffLineKind) -> DiffLineKind {
    kind
}

fn removed_event_kind(kind: DiffLineKind) -> DiffLineKind {
    match kind {
        DiffLineKind::Addition => DiffLineKind::Deletion,
        DiffLineKind::Deletion => DiffLineKind::Addition,
        DiffLineKind::FileHeader => DiffLineKind::FileHeader,
        DiffLineKind::HunkHeader => DiffLineKind::HunkHeader,
        DiffLineKind::Context => DiffLineKind::Context,
        DiffLineKind::BinaryNotice => DiffLineKind::BinaryNotice,
        DiffLineKind::ConflictMarker => DiffLineKind::ConflictMarker,
        DiffLineKind::ConflictOurs => DiffLineKind::ConflictOurs,
        DiffLineKind::ConflictBase => DiffLineKind::ConflictBase,
        DiffLineKind::ConflictTheirs => DiffLineKind::ConflictTheirs,
    }
}

fn event_line_numbers_for_added(line: &DiffLineView) -> (Option<u32>, Option<u32>) {
    match line.kind {
        DiffLineKind::Addition => (None, line.new_lineno.or(line.old_lineno)),
        DiffLineKind::Deletion => (line.old_lineno.or(line.new_lineno), None),
        DiffLineKind::Context => (line.old_lineno, line.new_lineno),
        DiffLineKind::FileHeader
        | DiffLineKind::HunkHeader
        | DiffLineKind::BinaryNotice
        | DiffLineKind::ConflictMarker
        | DiffLineKind::ConflictOurs
        | DiffLineKind::ConflictBase
        | DiffLineKind::ConflictTheirs => (line.old_lineno, line.new_lineno),
    }
}

fn event_line_numbers_for_removed(line: &DiffLineView) -> (Option<u32>, Option<u32>) {
    match line.kind {
        DiffLineKind::Addition => (line.new_lineno.or(line.old_lineno), None),
        DiffLineKind::Deletion => (None, line.old_lineno.or(line.new_lineno)),
        DiffLineKind::Context => (line.old_lineno, line.new_lineno),
        DiffLineKind::FileHeader
        | DiffLineKind::HunkHeader
        | DiffLineKind::BinaryNotice
        | DiffLineKind::ConflictMarker
        | DiffLineKind::ConflictOurs
        | DiffLineKind::ConflictBase
        | DiffLineKind::ConflictTheirs => (line.old_lineno, line.new_lineno),
    }
}

fn truncate_feed_file_document(
    document: &FileDiffDocument,
    remaining_lines: usize,
    remaining_bytes: usize,
) -> (FileDiffDocument, usize, usize, bool) {
    if document.file.is_binary {
        return (document.clone(), 0, 0, false);
    }

    let mut used_lines = 0usize;
    let mut used_bytes = 0usize;
    let mut kept_lines = Vec::new();
    let mut truncated = false;

    for line in &document.lines {
        let line_bytes = line.text.len();
        if used_lines >= remaining_lines || used_bytes + line_bytes > remaining_bytes {
            truncated = true;
            break;
        }
        kept_lines.push(line.clone());
        used_lines += 1;
        used_bytes += line_bytes;
    }

    if kept_lines.len() < document.lines.len() {
        truncated = true;
    }

    (
        FileDiffDocument {
            generation: document.generation,
            selection: document.selection.clone(),
            file: document.file.clone(),
            lines: kept_lines,
            hunks: Vec::new(),
        },
        used_lines,
        used_bytes,
        truncated,
    )
}

#[cfg(test)]
fn count_capture_changes(capture: &FeedCapturedEvent) -> (usize, usize) {
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for file in &capture.files {
        let (file_insertions, file_deletions) = count_document_changes(&file.document);
        insertions += file_insertions;
        deletions += file_deletions;
    }
    (insertions, deletions)
}

fn count_feed_scope_changes(capture: &FeedScopeCapture) -> (usize, usize) {
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for layer in &capture.layers {
        let FeedLayerState::Ready(document) = &layer.state else {
            continue;
        };
        let (document_insertions, document_deletions) = count_document_changes(document);
        insertions += document_insertions;
        deletions += document_deletions;
    }
    (insertions, deletions)
}

fn count_document_changes(document: &FileDiffDocument) -> (usize, usize) {
    count_diff_line_changes(&document.lines)
}

fn count_diff_line_changes(lines: &[DiffLineView]) -> (usize, usize) {
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for line in lines {
        match line.kind {
            DiffLineKind::Addition => insertions += 1,
            DiffLineKind::Deletion => deletions += 1,
            DiffLineKind::FileHeader
            | DiffLineKind::HunkHeader
            | DiffLineKind::Context
            | DiffLineKind::BinaryNotice
            | DiffLineKind::ConflictMarker
            | DiffLineKind::ConflictOurs
            | DiffLineKind::ConflictBase
            | DiffLineKind::ConflictTheirs => {}
        }
    }
    (insertions, deletions)
}

fn feed_selection_sort_key(selection: &DiffSelectionKey) -> (u8, &Path) {
    (
        match selection.section {
            DiffSectionKind::Conflicted => 0,
            DiffSectionKind::Staged => 1,
            DiffSectionKind::Unstaged => 2,
        },
        selection.relative_path.as_path(),
    )
}

fn feed_section_label(section: DiffSectionKind) -> &'static str {
    match section {
        DiffSectionKind::Conflicted => "conflicted",
        DiffSectionKind::Staged => "staged",
        DiffSectionKind::Unstaged => "unstaged",
    }
}

fn estimated_patch_line_count(patch: &Patch<'_>) -> Result<usize> {
    let mut line_count = 0usize;
    for hunk_idx in 0..patch.num_hunks() {
        line_count += 1;
        line_count += patch
            .num_lines_in_hunk(hunk_idx)
            .context("failed to count lines in patch hunk")?;
    }
    Ok(line_count + 2)
}

fn delta_path(delta: &git2::DiffDelta<'_>) -> Option<PathBuf> {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(Path::to_path_buf)
}

fn find_diff_delta_by_path<'diff>(
    diff: &'diff Diff<'_>,
    relative_path: &Path,
) -> Result<Option<(usize, git2::DiffDelta<'diff>)>> {
    for (idx, delta) in diff.deltas().enumerate() {
        let delta_path = delta_path(&delta).context("diff delta did not contain a path")?;
        if delta_path == relative_path {
            return Ok(Some((idx, delta)));
        }
    }
    Ok(None)
}

fn map_status(delta: Delta) -> Result<GitFileStatus> {
    match delta {
        Delta::Added => Ok(GitFileStatus::Added),
        Delta::Modified | Delta::Copied | Delta::Unreadable => Ok(GitFileStatus::Modified),
        Delta::Deleted => Ok(GitFileStatus::Deleted),
        Delta::Renamed => Ok(GitFileStatus::Renamed),
        Delta::Typechange => Ok(GitFileStatus::Typechange),
        Delta::Untracked => Ok(GitFileStatus::Untracked),
        Delta::Conflicted => Ok(GitFileStatus::Conflicted),
        other => Err(anyhow!("unsupported diff status: {other:?}")),
    }
}

fn map_line_kind(kind: DiffLineType) -> DiffLineKind {
    match kind {
        DiffLineType::FileHeader => DiffLineKind::FileHeader,
        DiffLineType::HunkHeader => DiffLineKind::HunkHeader,
        DiffLineType::Addition | DiffLineType::AddEOFNL => DiffLineKind::Addition,
        DiffLineType::Deletion | DiffLineType::DeleteEOFNL => DiffLineKind::Deletion,
        DiffLineType::Binary => DiffLineKind::BinaryNotice,
        DiffLineType::Context | DiffLineType::ContextEOFNL => DiffLineKind::Context,
    }
}

// ── Internal: branch and ref helpers ─────────────────────────────────

fn branch_name(repo: &Repository) -> Result<String> {
    let head = repo
        .head()
        .map_err(map_unborn_head("repository has no valid HEAD commit"))?;
    if repo
        .head_detached()
        .context("failed to inspect HEAD state")?
    {
        let commit = peel_head_commit(&head)?;
        return Ok(format!("detached@{}", short_oid(commit.id())));
    }

    Ok(head
        .shorthand()
        .map(str::to_owned)
        .or_else(|| head.name().map(str::to_owned))
        .unwrap_or_else(|| "HEAD".to_string()))
}

fn peel_head_commit<'repo>(head: &Reference<'repo>) -> Result<Commit<'repo>> {
    head.peel(ObjectType::Commit)
        .context("failed to peel HEAD to a commit")?
        .into_commit()
        .map_err(|_| anyhow!("HEAD did not resolve to a commit"))
}

fn source_ref(head: &Reference<'_>, source_commit: Oid) -> String {
    head.name()
        .map(str::to_owned)
        .unwrap_or_else(|| source_commit.to_string())
}

fn short_oid(oid: Oid) -> String {
    oid.to_string().chars().take(8).collect()
}

// ── Internal: path validation ────────────────────────────────────────

fn validate_and_normalize_paths(scope_root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for path in paths {
        // Reject any path containing parent-directory traversal components.
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!(
                "path {} contains parent directory traversal",
                path.display()
            );
        }

        let relative = if path.is_absolute() {
            path.strip_prefix(scope_root)
                .with_context(|| {
                    format!(
                        "{} is not under scope root {}",
                        path.display(),
                        scope_root.display()
                    )
                })?
                .to_path_buf()
        } else {
            path.to_path_buf()
        };

        if seen.insert(relative.clone()) {
            result.push(relative);
        }
    }

    result.sort();
    Ok(result)
}

// ── Internal: worktree and exclude helpers ───────────────────────────

fn validate_worktree_id(worktree_id: &str) -> Result<()> {
    if worktree_id.len() != 11
        || !worktree_id.starts_with("wt-")
        || worktree_id.contains('/')
        || worktree_id.contains('\\')
        || !worktree_id["wt-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid managed worktree id: {worktree_id}");
    }
    Ok(())
}

fn ensure_orcashell_excluded_file(exclude_path: &Path) -> Result<()> {
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let existing = match fs::read_to_string(exclude_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", exclude_path.display()));
        }
    };

    let already_present = existing
        .lines()
        .map(str::trim)
        .any(|line| line == ORCASHELL_EXCLUDE_ENTRY);
    if already_present {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(ORCASHELL_EXCLUDE_ENTRY);
    updated.push('\n');

    fs::write(exclude_path, updated)
        .with_context(|| format!("failed to write {}", exclude_path.display()))
}

fn map_unborn_head(message: &'static str) -> impl FnOnce(git2::Error) -> anyhow::Error {
    move |err| match err.code() {
        ErrorCode::UnbornBranch | ErrorCode::NotFound => anyhow!(message),
        _ => anyhow!(err).context(message),
    }
}

// ── Syntax highlighting ──────────────────────────────────────────────

/// Highlight diff lines in-place using two parse states (old-file, new-file).
///
/// Context lines advance both states. Additions advance the new-file state.
/// Deletions advance the old-file state. Headers are skipped.
fn attach_syntax_highlights(lines: &mut [DiffLineView], relative_path: &Path, theme_id: ThemeId) {
    let (mut old_hl, mut new_hl) = match (
        orcashell_syntax::Highlighter::for_path(relative_path, theme_id),
        orcashell_syntax::Highlighter::for_path(relative_path, theme_id),
    ) {
        (Some(old), Some(new)) => (old, new),
        _ => return, // plain text. Skip highlighting.
    };

    for line in lines.iter_mut() {
        match line.kind {
            DiffLineKind::Context => {
                old_hl.advance_state(&line.text);
                line.highlights = Some(new_hl.highlight_line(&line.text));
            }
            DiffLineKind::Addition => {
                line.highlights = Some(new_hl.highlight_line(&line.text));
            }
            DiffLineKind::Deletion => {
                line.highlights = Some(old_hl.highlight_line(&line.text));
            }
            DiffLineKind::FileHeader
            | DiffLineKind::HunkHeader
            | DiffLineKind::BinaryNotice
            | DiffLineKind::ConflictMarker
            | DiffLineKind::ConflictOurs
            | DiffLineKind::ConflictBase
            | DiffLineKind::ConflictTheirs => {}
        }
    }
}

// ── Inline word-level diff (imara-diff) ──────────────────────────────

/// Upper bound for a single delete/add replacement block that receives
/// token-level inline diffing. Larger blocks fall back to full-line add/remove.
const MAX_INLINE_DIFF_BLOCK_BYTES: usize = 16 * 1024;
const MAX_INLINE_DIFF_BLOCK_LINES: usize = 256;
const MAX_INLINE_ALIGNMENT_CELLS: usize = 4096;
const MAX_LOCAL_REPLACE_LINES_PER_SIDE: usize = 8;
const MAX_LOCAL_REPLACE_TOTAL_LINES: usize = 12;
const MAX_LOCAL_SIDE_RATIO: usize = 4;
const MIN_LINE_SIMILARITY: f32 = 0.35;
const ALIGN_MATCH: u8 = 1;
const ALIGN_DELETE: u8 = 2;
const ALIGN_INSERT: u8 = 3;

#[derive(Debug, Clone)]
struct InlineToken<'a> {
    text: &'a str,
    line_index: usize,
    byte_range: Range<usize>,
}

#[derive(Debug, Clone)]
struct PreparedLine<'a> {
    text: &'a str,
    trimmed: &'a str,
    identifier_tokens: Vec<&'a str>,
    content_tokens: Vec<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignmentOp {
    Match(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Character class for word-boundary tokenization.
fn char_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0 // identifier
    } else if c.is_whitespace() {
        1
    } else {
        2 // punctuation / other
    }
}

/// Split `text` into word-boundary tokens, returning byte ranges.
/// Each contiguous run of the same character class is one token.
fn tokenize_words(text: &str) -> Vec<Range<usize>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        let cls = char_class(ch);
        let mut end = start + ch.len_utf8();
        while let Some(&(_, next_ch)) = chars.peek() {
            if char_class(next_ch) != cls {
                break;
            }
            end += next_ch.len_utf8();
            chars.next();
        }
        tokens.push(start..end);
    }
    tokens
}

fn tokenize_inline_tokens<'a>(
    line_text: &'a str,
    line_index: usize,
    identifierish_only: bool,
    out: &mut Vec<InlineToken<'a>>,
) {
    let trimmed = line_text.trim_end_matches(['\r', '\n']);
    out.extend(
        tokenize_words(trimmed)
            .into_iter()
            .filter_map(|byte_range| {
                let text = &trimmed[byte_range.clone()];
                if identifierish_only && !range_contains_identifierish(trimmed, &byte_range) {
                    return None;
                }
                Some(InlineToken {
                    text,
                    line_index,
                    byte_range,
                })
            }),
    );
}

fn prepare_line(text: &str) -> PreparedLine<'_> {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let mut identifier_tokens = Vec::new();
    let mut content_tokens = Vec::new();

    for byte_range in tokenize_words(trimmed) {
        let token = &trimmed[byte_range.clone()];
        if token.chars().all(char::is_whitespace) {
            continue;
        }
        content_tokens.push(token);
        if range_contains_identifierish(trimmed, &byte_range) {
            identifier_tokens.push(token);
        }
    }

    PreparedLine {
        text,
        trimmed,
        identifier_tokens,
        content_tokens,
    }
}

fn trim_range_to_non_whitespace(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let trimmed = text.trim_end_matches(['\r', '\n']);
    let mut start = range.start.min(trimmed.len());
    let mut end = range.end.min(trimmed.len());

    while start < end {
        let ch = trimmed[start..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        start += ch.len_utf8();
    }

    while start < end {
        let (idx, ch) = trimmed[..end].char_indices().next_back().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        end = idx;
    }

    (start < end).then_some(start..end)
}

fn range_contains_identifierish(text: &str, range: &Range<usize>) -> bool {
    text[range.clone()]
        .chars()
        .any(|ch| ch.is_alphanumeric() || ch == '_')
}

fn mergeable_inline_gap(text: &str, start: usize, end: usize) -> bool {
    if start >= end {
        return true;
    }
    text[start..end].chars().all(char::is_whitespace)
}

fn merge_changed_tokens(
    line_texts: &[&str],
    tokens: &[InlineToken<'_>],
    changed: &[bool],
) -> Vec<Vec<Range<usize>>> {
    let mut ranges_by_line = vec![Vec::new(); line_texts.len()];
    let mut i = 0;

    while i < tokens.len() {
        if !changed[i] {
            i += 1;
            continue;
        }

        let line_index = tokens[i].line_index;
        let start = tokens[i].byte_range.start;
        let mut end = tokens[i].byte_range.end;
        i += 1;

        while i < tokens.len() && changed[i] && tokens[i].line_index == line_index {
            if !mergeable_inline_gap(line_texts[line_index], end, tokens[i].byte_range.start) {
                break;
            }
            end = tokens[i].byte_range.end;
            i += 1;
        }

        if let Some(range) = trim_range_to_non_whitespace(line_texts[line_index], start..end) {
            ranges_by_line[line_index].push(range);
        }
    }

    ranges_by_line
}

fn dice_similarity(tokens_a: &[&str], tokens_b: &[&str]) -> f32 {
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }

    let mut used = vec![false; tokens_b.len()];
    let mut shared = 0usize;

    for token in tokens_a {
        if let Some((idx, _)) = tokens_b
            .iter()
            .enumerate()
            .find(|(idx, candidate)| !used[*idx] && **candidate == *token)
        {
            used[idx] = true;
            shared += 1;
        }
    }

    (2 * shared) as f32 / (tokens_a.len() + tokens_b.len()) as f32
}

fn line_similarity(old_line: &PreparedLine<'_>, new_line: &PreparedLine<'_>) -> f32 {
    if old_line.trimmed.is_empty() || new_line.trimmed.is_empty() {
        return 0.0;
    }

    let use_identifier_tokens =
        !old_line.identifier_tokens.is_empty() || !new_line.identifier_tokens.is_empty();
    let old_tokens = if use_identifier_tokens {
        &old_line.identifier_tokens
    } else {
        &old_line.content_tokens
    };
    let new_tokens = if use_identifier_tokens {
        &new_line.identifier_tokens
    } else {
        &new_line.content_tokens
    };

    dice_similarity(old_tokens, new_tokens)
}

fn longest_increasing_anchor_pairs(candidates: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut best_len = vec![1usize; candidates.len()];
    let mut prev = vec![None; candidates.len()];
    let mut best_end = 0usize;

    for i in 0..candidates.len() {
        for j in 0..i {
            if candidates[j].1 < candidates[i].1 && best_len[j] + 1 > best_len[i] {
                best_len[i] = best_len[j] + 1;
                prev[i] = Some(j);
            }
        }
        if best_len[i] > best_len[best_end] {
            best_end = i;
        }
    }

    let mut anchors = Vec::new();
    let mut current = Some(best_end);
    while let Some(idx) = current {
        anchors.push(candidates[idx]);
        current = prev[idx];
    }
    anchors.reverse();
    anchors
}

fn unique_exact_anchor_pairs<'a>(
    old_lines: &[PreparedLine<'a>],
    new_lines: &[PreparedLine<'a>],
) -> Vec<(usize, usize)> {
    let mut old_positions: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut new_positions: HashMap<&str, Vec<usize>> = HashMap::new();

    for (idx, line) in old_lines.iter().enumerate() {
        if !line.trimmed.is_empty() {
            old_positions.entry(line.trimmed).or_default().push(idx);
        }
    }
    for (idx, line) in new_lines.iter().enumerate() {
        if !line.trimmed.is_empty() {
            new_positions.entry(line.trimmed).or_default().push(idx);
        }
    }

    let mut candidates = Vec::new();
    for (text, old_idxs) in old_positions {
        if old_idxs.len() != 1 {
            continue;
        }
        let Some(new_idxs) = new_positions.get(text) else {
            continue;
        };
        if new_idxs.len() != 1 {
            continue;
        }
        candidates.push((old_idxs[0], new_idxs[0]));
    }
    candidates.sort_unstable();
    longest_increasing_anchor_pairs(&candidates)
}

fn align_prepared_lines(
    old_lines: &[PreparedLine<'_>],
    new_lines: &[PreparedLine<'_>],
) -> Option<Vec<AlignmentOp>> {
    if old_lines.is_empty() && new_lines.is_empty() {
        return Some(Vec::new());
    }
    if old_lines.is_empty() {
        return Some((0..new_lines.len()).map(AlignmentOp::Insert).collect());
    }
    if new_lines.is_empty() {
        return Some((0..old_lines.len()).map(AlignmentOp::Delete).collect());
    }
    if old_lines.len() * new_lines.len() > MAX_INLINE_ALIGNMENT_CELLS {
        return None;
    }

    let rows = old_lines.len() + 1;
    let cols = new_lines.len() + 1;
    let gap_penalty = 0.45f32;
    let mut scores = vec![0.0f32; rows * cols];
    let mut trace = vec![0u8; rows * cols];

    let idx = |row: usize, col: usize| row * cols + col;

    for row in 1..rows {
        scores[idx(row, 0)] = scores[idx(row - 1, 0)] - gap_penalty;
        trace[idx(row, 0)] = ALIGN_DELETE;
    }
    for col in 1..cols {
        scores[idx(0, col)] = scores[idx(0, col - 1)] - gap_penalty;
        trace[idx(0, col)] = ALIGN_INSERT;
    }

    for row in 1..rows {
        for col in 1..cols {
            let similarity = line_similarity(&old_lines[row - 1], &new_lines[col - 1]);
            let delete_score = scores[idx(row - 1, col)] - gap_penalty;
            let insert_score = scores[idx(row, col - 1)] - gap_penalty;
            let mut best_score = delete_score;
            let mut best_trace = ALIGN_DELETE;

            if insert_score > best_score {
                best_score = insert_score;
                best_trace = ALIGN_INSERT;
            }

            if similarity >= MIN_LINE_SIMILARITY {
                let match_score = scores[idx(row - 1, col - 1)] + similarity;
                if match_score >= best_score {
                    best_score = match_score;
                    best_trace = ALIGN_MATCH;
                }
            }

            scores[idx(row, col)] = best_score;
            trace[idx(row, col)] = best_trace;
        }
    }

    let mut row = old_lines.len();
    let mut col = new_lines.len();
    let mut ops = Vec::with_capacity(old_lines.len() + new_lines.len());
    while row > 0 || col > 0 {
        match trace[idx(row, col)] {
            ALIGN_MATCH => {
                row -= 1;
                col -= 1;
                ops.push(AlignmentOp::Match(row, col));
            }
            ALIGN_DELETE => {
                row -= 1;
                ops.push(AlignmentOp::Delete(row));
            }
            ALIGN_INSERT => {
                col -= 1;
                ops.push(AlignmentOp::Insert(col));
            }
            _ => unreachable!("alignment traceback entered invalid state"),
        }
    }
    ops.reverse();
    Some(ops)
}

fn can_group_as_local_replace(old_count: usize, new_count: usize) -> bool {
    if old_count == 0 || new_count == 0 {
        return false;
    }
    if old_count > MAX_LOCAL_REPLACE_LINES_PER_SIDE || new_count > MAX_LOCAL_REPLACE_LINES_PER_SIDE
    {
        return false;
    }
    if old_count + new_count > MAX_LOCAL_REPLACE_TOTAL_LINES {
        return false;
    }

    let larger = old_count.max(new_count);
    let smaller = old_count.min(new_count);
    larger <= smaller * MAX_LOCAL_SIDE_RATIO
}

fn apply_local_replace_group_updates(
    old_updates: &mut [Option<Vec<Range<usize>>>],
    new_updates: &mut [Option<Vec<Range<usize>>>],
    old_base: usize,
    new_base: usize,
    old_lines: &[PreparedLine<'_>],
    new_lines: &[PreparedLine<'_>],
) {
    if !can_group_as_local_replace(old_lines.len(), new_lines.len()) {
        return;
    }

    let old_group: Vec<&str> = old_lines.iter().map(|line| line.text).collect();
    let new_group: Vec<&str> = new_lines.iter().map(|line| line.text).collect();
    let (old_ranges_by_line, new_ranges_by_line) =
        compute_block_inline_diff(&old_group, &new_group, true);

    for (offset, ranges) in old_ranges_by_line.into_iter().enumerate() {
        if !ranges.is_empty() {
            old_updates[old_base + offset] = Some(ranges);
        }
    }
    for (offset, ranges) in new_ranges_by_line.into_iter().enumerate() {
        if !ranges.is_empty() {
            new_updates[new_base + offset] = Some(ranges);
        }
    }
}

fn apply_alignment_ops(
    old_updates: &mut [Option<Vec<Range<usize>>>],
    new_updates: &mut [Option<Vec<Range<usize>>>],
    old_base: usize,
    new_base: usize,
    old_lines: &[PreparedLine<'_>],
    new_lines: &[PreparedLine<'_>],
    ops: &[AlignmentOp],
) {
    let mut cursor = 0;
    while cursor < ops.len() {
        match ops[cursor] {
            AlignmentOp::Match(old_idx, new_idx) => {
                let (old_ranges, new_ranges) =
                    compute_line_inline_diff(old_lines[old_idx].text, new_lines[new_idx].text);
                if !old_ranges.is_empty() {
                    old_updates[old_base + old_idx] = Some(old_ranges);
                }
                if !new_ranges.is_empty() {
                    new_updates[new_base + new_idx] = Some(new_ranges);
                }
                cursor += 1;
            }
            AlignmentOp::Delete(_) | AlignmentOp::Insert(_) => {
                let mut old_indices = Vec::new();
                let mut new_indices = Vec::new();

                while cursor < ops.len() {
                    match ops[cursor] {
                        AlignmentOp::Match(_, _) => break,
                        AlignmentOp::Delete(old_idx) => old_indices.push(old_idx),
                        AlignmentOp::Insert(new_idx) => new_indices.push(new_idx),
                    }
                    cursor += 1;
                }

                if !can_group_as_local_replace(old_indices.len(), new_indices.len()) {
                    continue;
                }

                let old_group: Vec<PreparedLine<'_>> = old_indices
                    .iter()
                    .map(|&idx| old_lines[idx].clone())
                    .collect();
                let new_group: Vec<PreparedLine<'_>> = new_indices
                    .iter()
                    .map(|&idx| new_lines[idx].clone())
                    .collect();
                apply_local_replace_group_updates(
                    old_updates,
                    new_updates,
                    old_base + old_indices[0],
                    new_base + new_indices[0],
                    &old_group,
                    &new_group,
                );
            }
        }
    }
}

fn apply_line_aware_inline_changes(
    lines: &mut [DiffLineView],
    del_start: usize,
    del_end: usize,
    add_start: usize,
    add_end: usize,
) {
    let old_texts: Vec<&str> = lines[del_start..del_end]
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    let new_texts: Vec<&str> = lines[add_start..add_end]
        .iter()
        .map(|line| line.text.as_str())
        .collect();

    if old_texts.len() + new_texts.len() > MAX_INLINE_DIFF_BLOCK_LINES {
        return;
    }

    let mut old_updates = vec![None; old_texts.len()];
    let mut new_updates = vec![None; new_texts.len()];

    {
        let prepared_old: Vec<PreparedLine<'_>> =
            old_texts.iter().map(|&text| prepare_line(text)).collect();
        let prepared_new: Vec<PreparedLine<'_>> =
            new_texts.iter().map(|&text| prepare_line(text)).collect();
        let anchors = unique_exact_anchor_pairs(&prepared_old, &prepared_new);

        let mut old_cursor = 0usize;
        let mut new_cursor = 0usize;
        for (anchor_old, anchor_new) in anchors {
            let old_window = &prepared_old[old_cursor..anchor_old];
            let new_window = &prepared_new[new_cursor..anchor_new];
            if old_window.len() != new_window.len()
                && can_group_as_local_replace(old_window.len(), new_window.len())
            {
                apply_local_replace_group_updates(
                    &mut old_updates,
                    &mut new_updates,
                    old_cursor,
                    new_cursor,
                    old_window,
                    new_window,
                );
            } else if let Some(ops) = align_prepared_lines(old_window, new_window) {
                apply_alignment_ops(
                    &mut old_updates,
                    &mut new_updates,
                    old_cursor,
                    new_cursor,
                    old_window,
                    new_window,
                    &ops,
                );
            }
            old_cursor = anchor_old + 1;
            new_cursor = anchor_new + 1;
        }

        let old_window = &prepared_old[old_cursor..];
        let new_window = &prepared_new[new_cursor..];
        if old_window.len() != new_window.len()
            && can_group_as_local_replace(old_window.len(), new_window.len())
        {
            apply_local_replace_group_updates(
                &mut old_updates,
                &mut new_updates,
                old_cursor,
                new_cursor,
                old_window,
                new_window,
            );
        } else if let Some(ops) = align_prepared_lines(old_window, new_window) {
            apply_alignment_ops(
                &mut old_updates,
                &mut new_updates,
                old_cursor,
                new_cursor,
                old_window,
                new_window,
                &ops,
            );
        }
    }

    for (offset, ranges) in old_updates.into_iter().enumerate() {
        if let Some(ranges) = ranges {
            lines[del_start + offset].inline_changes = Some(ranges);
        }
    }
    for (offset, ranges) in new_updates.into_iter().enumerate() {
        if let Some(ranges) = ranges {
            lines[add_start + offset].inline_changes = Some(ranges);
        }
    }
}

#[allow(clippy::type_complexity)]
fn compute_block_inline_diff(
    old_lines: &[&str],
    new_lines: &[&str],
    identifierish_only: bool,
) -> (Vec<Vec<Range<usize>>>, Vec<Vec<Range<usize>>>) {
    let mut old_tokens = Vec::new();
    let mut new_tokens = Vec::new();

    for (line_index, line) in old_lines.iter().copied().enumerate() {
        tokenize_inline_tokens(line, line_index, identifierish_only, &mut old_tokens);
    }
    for (line_index, line) in new_lines.iter().copied().enumerate() {
        tokenize_inline_tokens(line, line_index, identifierish_only, &mut new_tokens);
    }

    let mut old_ranges = vec![Vec::new(); old_lines.len()];
    let mut new_ranges = vec![Vec::new(); new_lines.len()];
    if old_tokens.is_empty() && new_tokens.is_empty() {
        return (old_ranges, new_ranges);
    }

    let mut input = imara_diff::intern::InternedInput::default();
    input.update_before(old_tokens.iter().map(|token| token.text));
    input.update_after(new_tokens.iter().map(|token| token.text));

    let mut old_changed = vec![false; old_tokens.len()];
    let mut new_changed = vec![false; new_tokens.len()];

    imara_diff::diff(
        imara_diff::Algorithm::Myers,
        &input,
        |before: Range<u32>, after: Range<u32>| {
            for i in before.start..before.end {
                old_changed[i as usize] = true;
            }
            for i in after.start..after.end {
                new_changed[i as usize] = true;
            }
        },
    );

    old_ranges = merge_changed_tokens(old_lines, &old_tokens, &old_changed);
    new_ranges = merge_changed_tokens(new_lines, &new_tokens, &new_changed);
    (old_ranges, new_ranges)
}

/// Run a word-level diff between two lines and return byte ranges of changed
/// regions in each line.
fn compute_line_inline_diff(
    old_text: &str,
    new_text: &str,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (mut old_ranges, mut new_ranges) =
        compute_block_inline_diff(&[old_text], &[new_text], false);
    (
        old_ranges.pop().unwrap_or_default(),
        new_ranges.pop().unwrap_or_default(),
    )
}

/// Walk diff lines, pair adjacent deletion/addition blocks, and compute
/// word-level inline change spans for each paired line.
fn attach_inline_changes(lines: &mut [DiffLineView]) {
    let mut i = 0;
    while i < lines.len() {
        // Find contiguous Deletion block.
        let del_start = i;
        while i < lines.len() && lines[i].kind == DiffLineKind::Deletion {
            i += 1;
        }
        let del_end = i;

        // Find contiguous Addition block immediately after.
        let add_start = i;
        while i < lines.len() && lines[i].kind == DiffLineKind::Addition {
            i += 1;
        }
        let add_end = i;

        // No block found. Advance past non-del/add line.
        if del_start == del_end && add_start == add_end {
            i += 1;
            continue;
        }

        if del_start == del_end || add_start == add_end {
            continue;
        }

        let block_bytes: usize = lines[del_start..del_end]
            .iter()
            .chain(lines[add_start..add_end].iter())
            .map(|line| line.text.len())
            .sum();
        if block_bytes > MAX_INLINE_DIFF_BLOCK_BYTES {
            continue;
        }

        apply_line_aware_inline_changes(lines, del_start, del_end, add_start, add_end);
    }
}

fn binary_notice(message: &str) -> DiffLineView {
    DiffLineView {
        kind: DiffLineKind::BinaryNotice,
        old_lineno: None,
        new_lineno: None,
        text: format!("{message}\n"),
        highlights: None,
        inline_changes: None,
    }
}

fn no_text_diff_notice() -> DiffLineView {
    DiffLineView {
        kind: DiffLineKind::Context,
        old_lineno: None,
        new_lineno: None,
        text: "No textual diff available\n".to_string(),
        highlights: None,
        inline_changes: None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
