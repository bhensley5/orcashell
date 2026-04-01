use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use git2::{
    BranchType, Commit, Delta, Diff, DiffFindOptions, DiffLineType, DiffOptions, ErrorCode,
    ObjectType, Oid, Patch, Reference, Repository, WorktreeAddOptions, WorktreePruneOptions,
};

use orcashell_store::ThemeId;
pub use orcashell_syntax::HighlightedSpan;

pub const ORCASHELL_EXCLUDE_ENTRY: &str = "/.orcashell/";
pub const MAX_RENDERED_DIFF_LINES: usize = 10_000;
pub const MAX_RENDERED_DIFF_BYTES: usize = 1024 * 1024;
pub const OVERSIZE_DIFF_MESSAGE: &str = "Diff too large to render in OrcaShell";
pub const BINARY_DIFF_MESSAGE: &str = "Binary file; diff body unavailable";

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
    pub is_worktree: bool,
    pub worktree_name: Option<String>,
    pub changed_files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

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
    FastForward { new_head: Oid },
    MergeCommit { merge_oid: Oid },
    Blocked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamInfo {
    pub remote: String,
    pub upstream_branch: String,
}

// ── Diff documents ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffDocument {
    pub generation: u64,
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub lines: Vec<DiffLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDocument {
    pub snapshot: GitSnapshotSummary,
    pub tracking: GitTrackingStatus,
    pub staged_files: Vec<ChangedFile>,
    pub unstaged_files: Vec<ChangedFile>,
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

pub fn load_snapshot(path: &Path, generation: u64) -> Result<GitSnapshotSummary> {
    let discovered = discover_repo(path)?;
    let staged_diff = build_staged_diff(&discovered.repo)?;
    let unstaged_diff = build_unstaged_diff(&discovered.repo)?;
    let staged_files = collect_changed_files(&staged_diff)?;
    let unstaged_files = collect_changed_files(&unstaged_diff)?;
    snapshot_summary_from_split(
        &discovered.scope,
        &discovered.repo,
        &staged_diff,
        &unstaged_diff,
        &staged_files,
        &unstaged_files,
        generation,
    )
}

pub fn load_diff_index(path: &Path, generation: u64) -> Result<DiffDocument> {
    let discovered = discover_repo(path)?;
    let staged_diff = build_staged_diff(&discovered.repo)?;
    let unstaged_diff = build_unstaged_diff(&discovered.repo)?;
    let staged_files = collect_changed_files(&staged_diff)?;
    let unstaged_files = collect_changed_files(&unstaged_diff)?;
    let tracking = compute_tracking_status(&discovered.repo)?;
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
        DiffSectionKind::Staged => build_staged_diff(&discovered.repo)?,
        DiffSectionKind::Unstaged => build_unstaged_diff(&discovered.repo)?,
    };

    for (idx, delta) in diff.deltas().enumerate() {
        let candidate = changed_file_from_delta(&diff, idx, delta)
            .with_context(|| format!("failed to build changed-file entry at diff index {idx}"))?;
        if candidate.relative_path != selection.relative_path {
            continue;
        }

        let mut lines = render_diff_lines(&diff, idx, &candidate)?;

        if !candidate.is_binary {
            attach_syntax_highlights(&mut lines, &selection.relative_path, theme_id);
            attach_inline_changes(&mut lines);
        }

        return Ok(FileDiffDocument {
            generation,
            selection: selection.clone(),
            file: candidate,
            lines,
        });
    }

    Err(anyhow!(
        "diff entry not found for {:?} path {}",
        selection.section,
        selection.relative_path.display()
    ))
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
    let staged = build_staged_diff(&discovered.repo)?;
    let unstaged = build_unstaged_diff(&discovered.repo)?;
    Ok(staged.deltas().count() == 0 && unstaged.deltas().count() == 0)
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
        return Ok(MergeOutcome::Blocked {
            reason: "Pull would produce merge conflicts. Resolve in the terminal.".into(),
        });
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
        return Ok(MergeOutcome::Blocked {
            reason: "Merge would produce conflicts. Resolve in the terminal.".into(),
        });
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
            staged_files,
            unstaged_files,
        ),
        branch_name: branch,
        is_worktree: scope.is_worktree,
        worktree_name: scope.worktree_name.clone(),
        changed_files: unique_paths.len(),
        insertions: staged_stats.insertions() + unstaged_stats.insertions(),
        deletions: staged_stats.deletions() + unstaged_stats.deletions(),
    })
}

fn collect_changed_files(diff: &Diff<'_>) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        files.push(
            changed_file_from_delta(diff, idx, delta).with_context(|| {
                format!("failed to build changed-file entry at diff index {idx}")
            })?,
        );
    }

    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

// ── Internal: fingerprinting ─────────────────────────────────────────

fn snapshot_content_fingerprint_split(
    scope: &GitScope,
    branch_name: &str,
    staged_files: &[ChangedFile],
    unstaged_files: &[ChangedFile],
) -> u64 {
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
    for file in staged_files {
        fingerprint_changed_file(&mut fingerprint, file);
    }
    // Unstaged section discriminant + files
    fingerprint_u8(&mut fingerprint, 0x02);
    fingerprint_usize(&mut fingerprint, unstaged_files.len());
    for file in unstaged_files {
        fingerprint_changed_file(&mut fingerprint, file);
    }
    fingerprint
}

fn fingerprint_changed_file(fingerprint: &mut u64, file: &ChangedFile) {
    let relative_path = file.relative_path.to_string_lossy();
    fingerprint_bytes(fingerprint, relative_path.as_bytes());
    fingerprint_u8(fingerprint, git_file_status_code(file.status));
    fingerprint_u8(fingerprint, file.is_binary as u8);
    fingerprint_usize(fingerprint, file.insertions);
    fingerprint_usize(fingerprint, file.deletions);
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
            DiffLineKind::FileHeader | DiffLineKind::HunkHeader | DiffLineKind::BinaryNotice => {}
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
mod tests {
    use super::*;
    use git2::{IndexAddOption, Signature};
    use tempfile::TempDir;

    fn repo_fixture() -> (TempDir, Repository) {
        let tempdir = TempDir::new().unwrap();
        let repo = Repository::init(tempdir.path()).unwrap();
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
        assert!(matches!(outcome, MergeOutcome::Blocked { .. }));

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
        let content = fs::read_to_string(tempdir.path().join("new.txt")).unwrap();
        // Normalize line endings for Windows (autocrlf may convert \n → \r\n)
        assert_eq!(content.replace("\r\n", "\n"), "from managed\n");
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
        assert!(matches!(result, MergeOutcome::Blocked { .. }));

        // Verify HEAD unchanged.
        let head_after = repo.head().unwrap().target().unwrap();
        assert_eq!(head_before, head_after);
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
        let source_content = fs::read_to_string(tempdir.path().join("new.txt")).unwrap();
        // Normalize line endings for Windows (autocrlf may convert \n → \r\n)
        assert_eq!(source_content.replace("\r\n", "\n"), "from managed\n");

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
}
