use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(test)]
use std::{
    sync::atomic::{AtomicU64, AtomicUsize},
    thread::sleep,
};

use async_channel::{
    unbounded as async_unbounded, Receiver as AsyncReceiver, Sender as AsyncSender,
};
use crossbeam_channel::{
    unbounded as crossbeam_unbounded, Receiver as CrossbeamReceiver, RecvTimeoutError,
    Sender as CrossbeamSender,
};
use notify::{recommended_watcher, Event as NotifyEvent, RecursiveMode, Watcher};
use orcashell_git::{
    abort_merge, apply_stash, capture_feed_event, checkout_local_branch, checkout_remote_branch,
    commit_staged, complete_merge, create_local_branch_from_local, create_managed_worktree,
    create_stash, delete_local_branch, discard_all_unstaged, discard_unstaged_file,
    discard_unstaged_hunk, drop_stash, list_stashes, load_commit_detail, load_commit_file_diff,
    load_diff_index, load_file_diff, load_repository_graph, load_snapshot, load_stash_detail,
    load_stash_file_diff, merge_managed_branch, pop_stash, pull_integrate, remove_managed_worktree,
    resolve_source_scope, resolve_upstream_info, stage_paths, unstage_paths, BranchCheckoutOutcome,
    CommitDetailDocument, CommitFileDiffDocument, CommitFileSelection, CreateLocalBranchOutcome,
    DeleteLocalBranchOutcome, DiffDocument, DiffSelectionKey, DiscardHunkTarget,
    DiscardMutationOutcome, FeedCaptureResult, FeedScopeCapture, FileDiffDocument,
    GitSnapshotSummary, ManagedWorktree, Oid, RepositoryGraphDocument, SnapshotLoadError,
    StashDetailDocument, StashFileDiffDocument, StashFileSelection, StashListDocument,
    StashMutationOutcome,
};
use orcashell_store::ThemeId;
use parking_lot::Mutex;
use tracing::warn;
use uuid::Uuid;

const WATCHER_DEBOUNCE: Duration = Duration::from_millis(250);
const WATCHER_POLL: Duration = Duration::from_millis(25);
const SNAPSHOT_WORKERS: usize = 2;
const AUTO_REPOSITORY_GRAPH_REFRESH_REVISION: u64 = 0;
const AUTO_STASH_LIST_REFRESH_REVISION: u64 = 0;

// ── Public event types ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitActionKind {
    Stage,
    Unstage,
    DiscardAll,
    DiscardFile,
    DiscardHunk,
    Commit,
    CreateStash,
    ApplyStash,
    PopStash,
    DropStash,
    MergeBack,
    CompleteMerge,
    AbortMerge,
    CheckoutLocalBranch,
    CheckoutRemoteBranch,
    CreateLocalBranch,
    DeleteLocalBranch,
    RemoveWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRemoteKind {
    Push,
    Publish,
    Pull,
    Fetch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFetchOrigin {
    Manual,
    Automatic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvent {
    SnapshotUpdated {
        terminal_ids: Vec<String>,
        request_path: PathBuf,
        scope_root: Option<PathBuf>,
        result: Result<GitSnapshotSummary, SnapshotLoadError>,
    },
    DiffIndexLoaded {
        scope_root: PathBuf,
        generation: u64,
        result: Result<DiffDocument, String>,
    },
    FileDiffLoaded {
        scope_root: PathBuf,
        generation: u64,
        selection: DiffSelectionKey,
        result: Result<FileDiffDocument, String>,
    },
    RepositoryGraphLoaded {
        scope_root: PathBuf,
        request_revision: u64,
        result: Result<RepositoryGraphDocument, String>,
    },
    CommitDetailLoaded {
        scope_root: PathBuf,
        oid: Oid,
        request_revision: u64,
        result: Result<CommitDetailDocument, String>,
    },
    CommitFileDiffLoaded {
        scope_root: PathBuf,
        selection: CommitFileSelection,
        request_revision: u64,
        result: Result<CommitFileDiffDocument, String>,
    },
    StashListLoaded {
        scope_root: PathBuf,
        request_revision: u64,
        result: Result<StashListDocument, String>,
    },
    StashDetailLoaded {
        scope_root: PathBuf,
        stash_oid: Oid,
        request_revision: u64,
        result: Result<StashDetailDocument, String>,
    },
    StashFileDiffLoaded {
        scope_root: PathBuf,
        selection: StashFileSelection,
        request_revision: u64,
        result: Result<StashFileDiffDocument, String>,
    },
    FeedCaptureCompleted {
        project_id: String,
        scope_root: PathBuf,
        generation: u64,
        request_revision: u64,
        result: Result<FeedCaptureResult, String>,
    },
    ManagedWorktreeCreated {
        project_id: String,
        origin_terminal_id: Option<String>,
        source_path: PathBuf,
        result: Result<ManagedWorktree, String>,
    },
    ManagedWorktreeRollbackComplete {
        project_id: String,
        worktree: ManagedWorktree,
        original_error: String,
        result: Result<(), String>,
    },
    LocalActionCompleted {
        scope_root: PathBuf,
        action: GitActionKind,
        result: Result<String, String>,
    },
    MergeConflictEntered {
        request_scope: PathBuf,
        affected_scope: PathBuf,
        conflicted_files: Vec<PathBuf>,
        trigger: MergeConflictTrigger,
    },
    RemoteOpCompleted {
        scope_root: PathBuf,
        kind: GitRemoteKind,
        fetch_origin: Option<GitFetchOrigin>,
        refresh_graph: bool,
        result: Result<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeConflictTrigger {
    Pull,
    MergeBack,
    StashApply,
    StashPop,
}

// ── GitCoordinator public API ────────────────────────────────────────

#[derive(Clone)]
pub struct GitCoordinator {
    inner: Arc<GitCoordinatorInner>,
}

impl GitCoordinator {
    pub fn new() -> Self {
        let (snapshot_tx, snapshot_rx) = crossbeam_unbounded();
        let (diff_tx, diff_rx) = crossbeam_unbounded();
        let (repo_browser_tx, repo_browser_rx) = crossbeam_unbounded();
        let (feed_capture_tx, feed_capture_rx) = crossbeam_unbounded();
        let (local_mutation_tx, local_mutation_rx) = crossbeam_unbounded();
        let (remote_op_tx, remote_op_rx) = crossbeam_unbounded();
        let (watch_event_tx, watch_event_rx) = crossbeam_unbounded();

        let inner = Arc::new(GitCoordinatorInner {
            state: Mutex::new(CoordinatorState::default()),
            diff_theme: Mutex::new(ThemeId::Dark),
            subscribers: Mutex::new(Vec::new()),
            snapshot_tx,
            diff_tx,
            repo_browser_tx,
            feed_capture_tx,
            local_mutation_tx,
            remote_op_tx,
            watch_event_tx,
            shutdown: AtomicBool::new(false),
            worker_handles: Mutex::new(Vec::new()),
        });

        let mut handles = Vec::new();
        for worker_idx in 0..SNAPSHOT_WORKERS {
            handles.push(spawn_snapshot_worker(
                inner.clone(),
                snapshot_rx.clone(),
                worker_idx,
            ));
        }
        handles.push(spawn_diff_worker(inner.clone(), diff_rx));
        handles.push(spawn_repo_browser_worker(inner.clone(), repo_browser_rx));
        handles.push(spawn_feed_capture_worker(inner.clone(), feed_capture_rx));
        handles.push(spawn_local_mutation_worker(
            inner.clone(),
            local_mutation_rx,
        ));
        handles.push(spawn_remote_op_worker(inner.clone(), remote_op_rx));
        handles.push(spawn_watch_dispatcher(inner.clone(), watch_event_rx));

        *inner.worker_handles.lock() = handles;

        Self { inner }
    }

    pub fn subscribe_events(&self) -> AsyncReceiver<GitEvent> {
        let (tx, rx) = async_unbounded();
        self.inner.subscribers.lock().push(tx);
        rx
    }

    pub fn set_diff_theme(&self, theme_id: ThemeId) {
        *self.inner.diff_theme.lock() = theme_id;
    }

    // ── Snapshot / diff requests (unchanged shape) ───────────────────

    pub fn request_snapshot(&self, path: &Path, terminal_id: Option<&str>) {
        let request_path = normalize_path(path);
        let key = {
            let state = self.inner.state.lock();
            state
                .aliases
                .get(&request_path)
                .cloned()
                .unwrap_or_else(|| request_path.clone())
        };
        self.inner.enqueue_snapshot(SnapshotJob {
            key,
            request_path,
            terminal_ids: terminal_id.into_iter().map(ToOwned::to_owned).collect(),
        });
    }

    pub fn request_diff_index(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        let generation = self.inner.current_generation(&scope_root);
        let _ = self.inner.diff_tx.send(DiffJob::Index {
            scope_root,
            generation,
        });
    }

    pub fn request_file_diff(
        &self,
        scope_root: &Path,
        generation: u64,
        selection: &DiffSelectionKey,
    ) {
        let _ = self.inner.diff_tx.send(DiffJob::File {
            scope_root: self.resolve_scope_root(scope_root),
            generation,
            selection: selection.clone(),
        });
    }

    pub fn request_repository_graph(&self, scope_root: &Path, request_revision: u64) {
        let _ = self.inner.repo_browser_tx.send(RepoBrowserJob::Graph {
            scope_root: self.resolve_scope_root(scope_root),
            request_revision,
        });
    }

    pub fn request_commit_detail(&self, scope_root: &Path, oid: Oid, request_revision: u64) {
        let _ = self
            .inner
            .repo_browser_tx
            .send(RepoBrowserJob::CommitDetail {
                scope_root: self.resolve_scope_root(scope_root),
                oid,
                request_revision,
            });
    }

    pub fn request_commit_file_diff(
        &self,
        scope_root: &Path,
        commit_oid: Oid,
        relative_path: PathBuf,
        request_revision: u64,
    ) {
        let _ = self
            .inner
            .repo_browser_tx
            .send(RepoBrowserJob::CommitFileDiff {
                scope_root: self.resolve_scope_root(scope_root),
                selection: CommitFileSelection {
                    commit_oid,
                    relative_path,
                },
                request_revision,
            });
    }

    pub fn request_stash_list(&self, scope_root: &Path, request_revision: u64) {
        let _ = self.inner.repo_browser_tx.send(RepoBrowserJob::StashList {
            scope_root: self.resolve_scope_root(scope_root),
            request_revision,
        });
    }

    pub fn request_stash_detail(&self, scope_root: &Path, stash_oid: Oid, request_revision: u64) {
        let _ = self
            .inner
            .repo_browser_tx
            .send(RepoBrowserJob::StashDetail {
                scope_root: self.resolve_scope_root(scope_root),
                stash_oid,
                request_revision,
            });
    }

    pub fn request_stash_file_diff(
        &self,
        scope_root: &Path,
        stash_oid: Oid,
        relative_path: PathBuf,
        request_revision: u64,
    ) {
        let _ = self
            .inner
            .repo_browser_tx
            .send(RepoBrowserJob::StashFileDiff {
                scope_root: self.resolve_scope_root(scope_root),
                selection: StashFileSelection {
                    stash_oid,
                    relative_path,
                },
                request_revision,
            });
    }

    pub fn request_feed_capture(
        &self,
        project_id: &str,
        scope_root: &Path,
        generation: u64,
        request_revision: u64,
        previous: Option<Arc<FeedScopeCapture>>,
    ) {
        let _ = self.inner.feed_capture_tx.send(FeedCaptureJob::Capture {
            project_id: project_id.to_string(),
            scope_root: self.resolve_scope_root(scope_root),
            generation,
            request_revision,
            previous,
        });
    }

    // ── Worktree lifecycle (project-scoped, no suppression) ──────────

    pub fn create_managed_worktree(
        &self,
        project_id: &str,
        source_path: &Path,
        origin_terminal_id: Option<&str>,
    ) {
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::CreateWorktree {
                project_id: project_id.to_string(),
                source_path: normalize_path(source_path),
                origin_terminal_id: origin_terminal_id.map(ToOwned::to_owned),
            });
    }

    pub fn rollback_managed_worktree(
        &self,
        project_id: &str,
        worktree: ManagedWorktree,
        original_error: impl Into<String>,
    ) {
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::RollbackWorktree {
                project_id: project_id.to_string(),
                worktree,
                original_error: original_error.into(),
            });
    }

    // ── Local mutation actions (scope-suppressed) ────────────────────

    pub fn stage_paths(&self, scope_root: &Path, paths: Vec<PathBuf>) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed stage_paths: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::Stage { scope_root, paths });
    }

    pub fn unstage_paths(&self, scope_root: &Path, paths: Vec<PathBuf>) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed unstage_paths: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::Unstage { scope_root, paths });
    }

    pub fn commit_staged(&self, scope_root: &Path, message: String) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed commit_staged: local action in flight"
            );
            return;
        }
        let _ = self.inner.local_mutation_tx.send(LocalMutationJob::Commit {
            scope_root,
            message,
        });
    }

    pub fn create_stash(
        &self,
        scope_root: &Path,
        message: Option<String>,
        keep_index: bool,
        include_untracked: bool,
    ) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed create_stash: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::CreateStash {
                scope_root,
                message,
                keep_index,
                include_untracked,
            });
    }

    pub fn apply_stash(&self, scope_root: &Path, stash_oid: Oid) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed apply_stash: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::ApplyStash {
                scope_root,
                stash_oid,
            });
    }

    pub fn pop_stash(&self, scope_root: &Path, stash_oid: Oid) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(?scope_root, "suppressed pop_stash: local action in flight");
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::PopStash {
                scope_root,
                stash_oid,
            });
    }

    pub fn drop_stash(&self, scope_root: &Path, stash_oid: Oid) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(?scope_root, "suppressed drop_stash: local action in flight");
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::DropStash {
                scope_root,
                stash_oid,
            });
    }

    pub fn discard_all_unstaged(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed discard_all_unstaged: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::DiscardAll { scope_root });
    }

    pub fn discard_unstaged_file(&self, scope_root: &Path, relative_path: PathBuf) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed discard_unstaged_file: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::DiscardFile {
                scope_root,
                relative_path,
            });
    }

    pub fn discard_unstaged_hunk(
        &self,
        scope_root: &Path,
        relative_path: PathBuf,
        target: DiscardHunkTarget,
    ) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed discard_unstaged_hunk: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::DiscardHunk {
                scope_root,
                relative_path,
                target,
            });
    }

    pub fn merge_managed_branch(&self, scope_root: &Path, source_ref: String) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed merge_managed_branch: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::MergeBack {
                scope_root,
                source_ref,
            });
    }

    pub fn complete_merge(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed complete_merge: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::CompleteMerge { scope_root });
    }

    pub fn abort_merge(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed abort_merge: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::AbortMerge { scope_root });
    }

    pub fn checkout_local_branch(&self, scope_root: &Path, branch_name: String) -> bool {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed checkout_local_branch: local action in flight"
            );
            return false;
        }
        self.inner
            .local_mutation_tx
            .send(LocalMutationJob::CheckoutLocalBranch {
                scope_root,
                branch_name,
            })
            .is_ok()
    }

    pub fn checkout_remote_branch(&self, scope_root: &Path, remote_full_ref: String) -> bool {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed checkout_remote_branch: local action in flight"
            );
            return false;
        }
        self.inner
            .local_mutation_tx
            .send(LocalMutationJob::CheckoutRemoteBranch {
                scope_root,
                remote_full_ref,
            })
            .is_ok()
    }

    pub fn create_local_branch(
        &self,
        scope_root: &Path,
        source_branch_name: String,
        new_branch_name: String,
    ) -> bool {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed create_local_branch: local action in flight"
            );
            return false;
        }
        self.inner
            .local_mutation_tx
            .send(LocalMutationJob::CreateLocalBranch {
                scope_root,
                source_branch_name,
                new_branch_name,
            })
            .is_ok()
    }

    pub fn delete_local_branch(&self, scope_root: &Path, branch_name: String) -> bool {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed delete_local_branch: local action in flight"
            );
            return false;
        }
        self.inner
            .local_mutation_tx
            .send(LocalMutationJob::DeleteLocalBranch {
                scope_root,
                branch_name,
            })
            .is_ok()
    }

    pub fn remove_managed_worktree_action(&self, scope_root: &Path, delete_branch: bool) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_local_action(&scope_root) {
            warn!(
                ?scope_root,
                "suppressed remove_managed_worktree: local action in flight"
            );
            return;
        }
        let _ = self
            .inner
            .local_mutation_tx
            .send(LocalMutationJob::RemoveWorktree {
                scope_root,
                delete_branch,
            });
    }

    // ── Remote operations (scope-suppressed) ─────────────────────────

    pub fn push_current_branch(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self
            .inner
            .try_start_remote_op(&scope_root, GitRemoteKind::Push, None)
        {
            warn!(?scope_root, "suppressed push: remote op already in flight");
            return;
        }
        let _ = self
            .inner
            .remote_op_tx
            .send(RemoteOpJob::Push { scope_root });
    }

    pub fn publish_current_branch(&self, scope_root: &Path, remote_name: String) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self
            .inner
            .try_start_remote_op(&scope_root, GitRemoteKind::Publish, None)
        {
            warn!(
                ?scope_root,
                "suppressed publish: remote op already in flight"
            );
            return;
        }
        let _ = self.inner.remote_op_tx.send(RemoteOpJob::Publish {
            scope_root,
            remote_name,
        });
    }

    pub fn pull_current_branch(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self
            .inner
            .try_start_remote_op(&scope_root, GitRemoteKind::Pull, None)
        {
            warn!(?scope_root, "suppressed pull: remote op already in flight");
            return;
        }
        let _ = self
            .inner
            .remote_op_tx
            .send(RemoteOpJob::Pull { scope_root });
    }

    pub fn fetch_repo(&self, scope_root: &Path, origin: GitFetchOrigin) -> bool {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self
            .inner
            .try_start_remote_op(&scope_root, GitRemoteKind::Fetch, Some(origin))
        {
            warn!(?scope_root, "suppressed fetch: remote op already in flight");
            return false;
        }
        self.inner
            .remote_op_tx
            .send(RemoteOpJob::Fetch { scope_root, origin })
            .is_ok()
    }

    // ── Watcher management ───────────────────────────────────────────

    pub fn subscribe(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        let mut state = self.inner.state.lock();
        let scope = state.scopes.entry(scope_root.clone()).or_default();
        scope.watcher_refs += 1;
        if scope.watcher.is_none() {
            match build_watcher(&scope_root, self.inner.watch_event_tx.clone()) {
                Ok(watcher) => {
                    scope.watcher = Some(WatcherHandle { _watcher: watcher });
                }
                Err(error) => {
                    warn!(
                        ?scope_root,
                        "failed to start git watcher for scope: {error}"
                    );
                }
            }
        }
    }

    pub fn unsubscribe(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        let mut state = self.inner.state.lock();
        if let Some(scope) = state.scopes.get_mut(&scope_root) {
            scope.watcher_refs = scope.watcher_refs.saturating_sub(1);
            if scope.watcher_refs == 0 {
                scope.watcher = None;
            }
        }
    }

    fn resolve_scope_root(&self, scope_root: &Path) -> PathBuf {
        let normalized = normalize_path(scope_root);
        let state = self.inner.state.lock();
        state
            .aliases
            .get(&normalized)
            .cloned()
            .unwrap_or(normalized)
    }

    #[cfg(test)]
    fn debug_scope_ref_count(&self, scope_root: &Path) -> usize {
        let scope_root = self.resolve_scope_root(scope_root);
        self.inner
            .state
            .lock()
            .scopes
            .get(&scope_root)
            .map(|scope| scope.watcher_refs)
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn debug_inject_watch_event(&self, scope_root: &Path) {
        let _ = self
            .inner
            .watch_event_tx
            .send(self.resolve_scope_root(scope_root));
    }
}

impl Default for GitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal coordinator state ───────────────────────────────────────

struct GitCoordinatorInner {
    state: Mutex<CoordinatorState>,
    diff_theme: Mutex<ThemeId>,
    subscribers: Mutex<Vec<AsyncSender<GitEvent>>>,
    snapshot_tx: CrossbeamSender<SnapshotJobEnvelope>,
    diff_tx: CrossbeamSender<DiffJob>,
    repo_browser_tx: CrossbeamSender<RepoBrowserJob>,
    feed_capture_tx: CrossbeamSender<FeedCaptureJob>,
    local_mutation_tx: CrossbeamSender<LocalMutationJob>,
    remote_op_tx: CrossbeamSender<RemoteOpJob>,
    watch_event_tx: CrossbeamSender<PathBuf>,
    shutdown: AtomicBool,
    worker_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl GitCoordinatorInner {
    fn enqueue_snapshot(&self, job: SnapshotJob) {
        let mut state = self.state.lock();
        let scope = state.scopes.entry(job.key.clone()).or_default();
        if scope.snapshot_in_flight {
            push_unique_ids(&mut scope.pending_terminal_ids, job.terminal_ids);
            scope.snapshot_pending = true;
            return;
        }
        scope.snapshot_in_flight = true;
        let _ = self.snapshot_tx.send(SnapshotJobEnvelope::Refresh(job));
    }

    fn current_generation(&self, scope_root: &Path) -> u64 {
        self.state
            .lock()
            .scopes
            .get(scope_root)
            .map(|scope| scope.generation)
            .unwrap_or(0)
    }

    /// Try to start a local action for the given scope. Returns false if a
    /// blocking action is already in flight. Automatic background fetches are
    /// treated as non-blocking.
    fn try_start_local_action(&self, scope_root: &Path) -> bool {
        let mut state = self.state.lock();
        let scope = state.scopes.entry(scope_root.to_path_buf()).or_default();
        let background_fetch_in_flight = matches!(
            (
                scope.current_remote_op_kind.as_ref(),
                scope.current_fetch_origin.as_ref(),
            ),
            (Some(GitRemoteKind::Fetch), Some(GitFetchOrigin::Automatic))
        );
        if scope.local_action_in_flight
            || (scope.remote_op_in_flight && !background_fetch_in_flight)
        {
            return false;
        }
        scope.local_action_in_flight = true;
        true
    }

    /// Try to start a remote op for the given scope. Returns false if any
    /// action (local or remote) is already in flight (scope-wide exclusion).
    fn try_start_remote_op(
        &self,
        scope_root: &Path,
        kind: GitRemoteKind,
        fetch_origin: Option<GitFetchOrigin>,
    ) -> bool {
        let mut state = self.state.lock();
        let scope = state.scopes.entry(scope_root.to_path_buf()).or_default();
        if scope.local_action_in_flight || scope.remote_op_in_flight {
            return false;
        }
        scope.remote_op_in_flight = true;
        scope.current_remote_op_kind = Some(kind);
        scope.current_fetch_origin = fetch_origin;
        true
    }

    /// Clear the local-action in-flight flag, broadcast the completion event,
    /// and enqueue a snapshot refresh for the scope.
    fn finish_local_action(
        &self,
        scope_root: &PathBuf,
        action: GitActionKind,
        result: Result<String, String>,
    ) {
        let refresh_graph =
            action_triggers_graph_refresh(&action) && result_triggers_graph_refresh(&result);
        let refresh_stash_list = action_triggers_stash_refresh(&action);
        {
            let mut state = self.state.lock();
            if let Some(scope) = state.scopes.get_mut(scope_root) {
                scope.local_action_in_flight = false;
            }
        }
        self.broadcast(GitEvent::LocalActionCompleted {
            scope_root: scope_root.clone(),
            action,
            result,
        });
        if refresh_graph {
            let _ = self.repo_browser_tx.send(RepoBrowserJob::Graph {
                scope_root: scope_root.clone(),
                request_revision: AUTO_REPOSITORY_GRAPH_REFRESH_REVISION,
            });
        }
        if refresh_stash_list {
            let _ = self.repo_browser_tx.send(RepoBrowserJob::StashList {
                scope_root: scope_root.clone(),
                request_revision: AUTO_STASH_LIST_REFRESH_REVISION,
            });
        }
        // Trigger snapshot refresh so UI sees updated file lists.
        self.enqueue_snapshot(SnapshotJob {
            key: scope_root.clone(),
            request_path: scope_root.clone(),
            terminal_ids: Vec::new(),
        });
    }

    /// Clear the remote-op in-flight flag, broadcast the completion event,
    /// and enqueue follow-up refresh work for the scope when requested.
    fn finish_remote_op(
        &self,
        scope_root: &PathBuf,
        kind: GitRemoteKind,
        fetch_origin: Option<GitFetchOrigin>,
        refresh_graph: bool,
        refresh_snapshot: bool,
        result: Result<String, String>,
    ) {
        {
            let mut state = self.state.lock();
            if let Some(scope) = state.scopes.get_mut(scope_root) {
                scope.remote_op_in_flight = false;
                scope.current_remote_op_kind = None;
                scope.current_fetch_origin = None;
            }
        }
        self.broadcast(GitEvent::RemoteOpCompleted {
            scope_root: scope_root.clone(),
            kind,
            fetch_origin,
            refresh_graph,
            result,
        });
        if refresh_graph {
            let _ = self.repo_browser_tx.send(RepoBrowserJob::Graph {
                scope_root: scope_root.clone(),
                request_revision: AUTO_REPOSITORY_GRAPH_REFRESH_REVISION,
            });
        }
        if refresh_snapshot {
            self.enqueue_snapshot(SnapshotJob {
                key: scope_root.clone(),
                request_path: scope_root.clone(),
                terminal_ids: Vec::new(),
            });
        }
    }

    fn emit_merge_conflict(
        &self,
        request_scope: &Path,
        affected_scope: &Path,
        conflicted_files: Vec<PathBuf>,
        trigger: MergeConflictTrigger,
    ) {
        self.broadcast(GitEvent::MergeConflictEntered {
            request_scope: request_scope.to_path_buf(),
            affected_scope: affected_scope.to_path_buf(),
            conflicted_files,
            trigger,
        });
    }

    fn finish_snapshot(
        &self,
        job: SnapshotJob,
        result: Result<GitSnapshotSummary, SnapshotLoadError>,
    ) {
        let (event, requeue) = {
            let mut state = self.state.lock();
            let mut scope_state = state.scopes.remove(&job.key).unwrap_or_default();
            scope_state.snapshot_in_flight = false;
            let had_pending = scope_state.snapshot_pending;
            scope_state.snapshot_pending = false;
            let mut terminal_ids = job.terminal_ids;
            push_unique_ids(
                &mut terminal_ids,
                std::mem::take(&mut scope_state.pending_terminal_ids),
            );

            match result {
                Ok(mut snapshot) => {
                    let scope_root = normalize_path(&snapshot.scope_root);
                    let repo_root = normalize_path(&snapshot.repo_root);
                    snapshot.scope_root = scope_root.clone();
                    snapshot.repo_root = repo_root;

                    let prior_scope_state = state.scopes.remove(&scope_root).unwrap_or_default();
                    let mut merged = merge_scope_state(prior_scope_state, scope_state);
                    let content_changed = merged.last_snapshot.as_ref().is_none_or(|previous| {
                        previous.content_fingerprint != snapshot.content_fingerprint
                    });
                    if content_changed {
                        merged.generation += 1;
                    }
                    snapshot.generation = merged.generation;
                    merged.last_snapshot = Some(snapshot.clone());
                    merged.last_error = None;

                    if had_pending {
                        merged.snapshot_in_flight = true;
                    }

                    state
                        .aliases
                        .insert(job.request_path.clone(), scope_root.clone());
                    if job.key != scope_root {
                        state.aliases.insert(job.key.clone(), scope_root.clone());
                    }
                    state.scopes.insert(scope_root.clone(), merged);

                    let requeue = if had_pending {
                        Some(SnapshotJob {
                            key: scope_root.clone(),
                            request_path: scope_root.clone(),
                            terminal_ids: Vec::new(),
                        })
                    } else {
                        None
                    };

                    (
                        GitEvent::SnapshotUpdated {
                            terminal_ids,
                            request_path: job.request_path,
                            scope_root: Some(scope_root),
                            result: Ok(snapshot),
                        },
                        requeue,
                    )
                }
                Err(error) => {
                    scope_state.last_error = Some(error.message().to_string());
                    if had_pending {
                        scope_state.snapshot_in_flight = true;
                    }
                    let scope_root = state.aliases.get(&job.request_path).cloned().or_else(|| {
                        if job.key != job.request_path {
                            Some(job.key.clone())
                        } else {
                            None
                        }
                    });
                    let requeue = if had_pending {
                        Some(SnapshotJob {
                            key: job.key.clone(),
                            request_path: job.request_path.clone(),
                            terminal_ids: Vec::new(),
                        })
                    } else {
                        None
                    };
                    state.scopes.insert(job.key.clone(), scope_state);

                    (
                        GitEvent::SnapshotUpdated {
                            terminal_ids,
                            request_path: job.request_path,
                            scope_root,
                            result: Err(error),
                        },
                        requeue,
                    )
                }
            }
        };

        self.broadcast(event);
        if let Some(job) = requeue {
            let _ = self.snapshot_tx.send(SnapshotJobEnvelope::Refresh(job));
        }
    }

    fn broadcast(&self, event: GitEvent) {
        let mut subscribers = self.subscribers.lock();
        subscribers.retain(|sender| sender.try_send(event.clone()).is_ok());
    }
}

impl Drop for GitCoordinatorInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for _ in 0..SNAPSHOT_WORKERS {
            let _ = self.snapshot_tx.send(SnapshotJobEnvelope::Stop);
        }
        let _ = self.diff_tx.send(DiffJob::Stop);
        let _ = self.repo_browser_tx.send(RepoBrowserJob::Stop);
        let _ = self.feed_capture_tx.send(FeedCaptureJob::Stop);
        let _ = self.local_mutation_tx.send(LocalMutationJob::Stop);
        let _ = self.remote_op_tx.send(RemoteOpJob::Stop);

        let mut handles = self.worker_handles.lock();
        for handle in handles.drain(..) {
            let _ = handle.join();
        }
    }
}

// ── Coordinator state ────────────────────────────────────────────────

#[derive(Default)]
struct CoordinatorState {
    scopes: HashMap<PathBuf, ScopeState>,
    aliases: HashMap<PathBuf, PathBuf>,
}

#[derive(Default)]
struct ScopeState {
    generation: u64,
    snapshot_in_flight: bool,
    snapshot_pending: bool,
    pending_terminal_ids: Vec<String>,
    last_snapshot: Option<GitSnapshotSummary>,
    last_error: Option<String>,
    watcher_refs: usize,
    watcher: Option<WatcherHandle>,
    local_action_in_flight: bool,
    remote_op_in_flight: bool,
    current_remote_op_kind: Option<GitRemoteKind>,
    current_fetch_origin: Option<GitFetchOrigin>,
}

struct WatcherHandle {
    _watcher: notify::RecommendedWatcher,
}

fn merge_scope_state(mut current: ScopeState, incoming: ScopeState) -> ScopeState {
    current.generation = current.generation.max(incoming.generation);
    current.snapshot_in_flight |= incoming.snapshot_in_flight;
    current.snapshot_pending |= incoming.snapshot_pending;
    push_unique_ids(
        &mut current.pending_terminal_ids,
        incoming.pending_terminal_ids,
    );
    current.watcher_refs += incoming.watcher_refs;
    current.local_action_in_flight |= incoming.local_action_in_flight;
    if !current.remote_op_in_flight && incoming.remote_op_in_flight {
        current.current_remote_op_kind = incoming.current_remote_op_kind;
        current.current_fetch_origin = incoming.current_fetch_origin;
    } else if current.remote_op_in_flight && incoming.remote_op_in_flight {
        if current.current_remote_op_kind.is_none() {
            current.current_remote_op_kind = incoming.current_remote_op_kind;
        }
        if current.current_fetch_origin.is_none() {
            current.current_fetch_origin = incoming.current_fetch_origin;
        }
    }
    current.remote_op_in_flight |= incoming.remote_op_in_flight;
    if current.last_snapshot.is_none() {
        current.last_snapshot = incoming.last_snapshot;
    }
    if current.last_error.is_none() {
        current.last_error = incoming.last_error;
    }
    if current.watcher.is_none() {
        current.watcher = incoming.watcher;
    }
    current
}

// ── Job types ────────────────────────────────────────────────────────

#[derive(Debug)]
enum SnapshotJobEnvelope {
    Refresh(SnapshotJob),
    Stop,
}

#[derive(Debug, Clone)]
struct SnapshotJob {
    key: PathBuf,
    request_path: PathBuf,
    terminal_ids: Vec<String>,
}

#[derive(Debug)]
enum DiffJob {
    Index {
        scope_root: PathBuf,
        generation: u64,
    },
    File {
        scope_root: PathBuf,
        generation: u64,
        selection: DiffSelectionKey,
    },
    Stop,
}

#[derive(Debug)]
enum RepoBrowserJob {
    Graph {
        scope_root: PathBuf,
        request_revision: u64,
    },
    CommitDetail {
        scope_root: PathBuf,
        oid: Oid,
        request_revision: u64,
    },
    CommitFileDiff {
        scope_root: PathBuf,
        selection: CommitFileSelection,
        request_revision: u64,
    },
    StashList {
        scope_root: PathBuf,
        request_revision: u64,
    },
    StashDetail {
        scope_root: PathBuf,
        stash_oid: Oid,
        request_revision: u64,
    },
    StashFileDiff {
        scope_root: PathBuf,
        selection: StashFileSelection,
        request_revision: u64,
    },
    Stop,
}

#[derive(Debug)]
enum LocalMutationJob {
    CreateWorktree {
        project_id: String,
        source_path: PathBuf,
        origin_terminal_id: Option<String>,
    },
    RollbackWorktree {
        project_id: String,
        worktree: ManagedWorktree,
        original_error: String,
    },
    Stage {
        scope_root: PathBuf,
        paths: Vec<PathBuf>,
    },
    Unstage {
        scope_root: PathBuf,
        paths: Vec<PathBuf>,
    },
    Commit {
        scope_root: PathBuf,
        message: String,
    },
    CreateStash {
        scope_root: PathBuf,
        message: Option<String>,
        keep_index: bool,
        include_untracked: bool,
    },
    ApplyStash {
        scope_root: PathBuf,
        stash_oid: Oid,
    },
    PopStash {
        scope_root: PathBuf,
        stash_oid: Oid,
    },
    DropStash {
        scope_root: PathBuf,
        stash_oid: Oid,
    },
    DiscardAll {
        scope_root: PathBuf,
    },
    DiscardFile {
        scope_root: PathBuf,
        relative_path: PathBuf,
    },
    DiscardHunk {
        scope_root: PathBuf,
        relative_path: PathBuf,
        target: DiscardHunkTarget,
    },
    MergeBack {
        scope_root: PathBuf,
        source_ref: String,
    },
    CompleteMerge {
        scope_root: PathBuf,
    },
    AbortMerge {
        scope_root: PathBuf,
    },
    CheckoutLocalBranch {
        scope_root: PathBuf,
        branch_name: String,
    },
    CheckoutRemoteBranch {
        scope_root: PathBuf,
        remote_full_ref: String,
    },
    CreateLocalBranch {
        scope_root: PathBuf,
        source_branch_name: String,
        new_branch_name: String,
    },
    DeleteLocalBranch {
        scope_root: PathBuf,
        branch_name: String,
    },
    IntegratePull {
        scope_root: PathBuf,
    },
    RemoveWorktree {
        scope_root: PathBuf,
        delete_branch: bool,
    },
    Stop,
}

#[derive(Debug)]
enum FeedCaptureJob {
    Capture {
        project_id: String,
        scope_root: PathBuf,
        generation: u64,
        request_revision: u64,
        previous: Option<Arc<FeedScopeCapture>>,
    },
    Stop,
}

#[derive(Debug)]
enum RemoteOpJob {
    Push {
        scope_root: PathBuf,
    },
    Publish {
        scope_root: PathBuf,
        remote_name: String,
    },
    Pull {
        scope_root: PathBuf,
    },
    Fetch {
        scope_root: PathBuf,
        origin: GitFetchOrigin,
    },
    Stop,
}

// ── Workers ──────────────────────────────────────────────────────────

fn map_discard_outcome(outcome: DiscardMutationOutcome, success_message: String) -> String {
    match outcome {
        DiscardMutationOutcome::Applied => success_message,
        DiscardMutationOutcome::Blocked { reason } => format!("BLOCKED: {reason}"),
    }
}

fn spawn_snapshot_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<SnapshotJobEnvelope>,
    worker_idx: usize,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name(format!("orca-git-snapshot-{worker_idx}"))
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    SnapshotJobEnvelope::Refresh(job) => {
                        #[cfg(test)]
                        {
                            let delay = snapshot_worker_test_delay();
                            if !delay.is_zero() {
                                sleep(delay);
                            }
                        }
                        let result = load_snapshot(&job.request_path, 0);
                        inner.finish_snapshot(job, result);
                    }
                    SnapshotJobEnvelope::Stop => break,
                }
            }
        })
        .expect("failed to spawn git snapshot worker")
}

fn spawn_diff_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<DiffJob>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-file-diff".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    DiffJob::Index {
                        scope_root,
                        generation,
                    } => {
                        let result = load_diff_index(&scope_root, generation)
                            .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::DiffIndexLoaded {
                            scope_root,
                            generation,
                            result,
                        });
                    }
                    DiffJob::File {
                        scope_root,
                        generation,
                        selection,
                    } => {
                        let theme_id = *inner.diff_theme.lock();
                        let result = load_file_diff(&scope_root, generation, &selection, theme_id)
                            .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::FileDiffLoaded {
                            scope_root,
                            generation,
                            selection,
                            result,
                        });
                    }
                    DiffJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git file-diff worker")
}

fn spawn_repo_browser_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<RepoBrowserJob>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-repo-browser".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    RepoBrowserJob::Graph {
                        scope_root,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let result =
                            load_repository_graph(&scope_root).map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::RepositoryGraphLoaded {
                            scope_root,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::CommitDetail {
                        scope_root,
                        oid,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let result =
                            load_commit_detail(&scope_root, oid).map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::CommitDetailLoaded {
                            scope_root,
                            oid,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::CommitFileDiff {
                        scope_root,
                        selection,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let theme_id = *inner.diff_theme.lock();
                        let result = load_commit_file_diff(
                            &scope_root,
                            selection.commit_oid,
                            &selection.relative_path,
                            theme_id,
                        )
                        .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::CommitFileDiffLoaded {
                            scope_root,
                            selection,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::StashList {
                        scope_root,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let result = list_stashes(&scope_root).map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::StashListLoaded {
                            scope_root,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::StashDetail {
                        scope_root,
                        stash_oid,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let result = load_stash_detail(&scope_root, stash_oid)
                            .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::StashDetailLoaded {
                            scope_root,
                            stash_oid,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::StashFileDiff {
                        scope_root,
                        selection,
                        request_revision,
                    } => {
                        #[cfg(test)]
                        {
                            wait_for_repo_browser_test_unblock();
                        }
                        let theme_id = *inner.diff_theme.lock();
                        let result = load_stash_file_diff(
                            &scope_root,
                            selection.stash_oid,
                            &selection.relative_path,
                            theme_id,
                        )
                        .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::StashFileDiffLoaded {
                            scope_root,
                            selection,
                            request_revision,
                            result,
                        });
                    }
                    RepoBrowserJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git repo-browser worker")
}

fn spawn_feed_capture_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<FeedCaptureJob>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-feed-capture".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    FeedCaptureJob::Capture {
                        project_id,
                        scope_root,
                        generation,
                        request_revision,
                        previous,
                    } => {
                        let theme_id = *inner.diff_theme.lock();
                        let result = capture_feed_event(
                            &scope_root,
                            generation,
                            previous.as_deref(),
                            theme_id,
                        )
                        .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::FeedCaptureCompleted {
                            project_id,
                            scope_root,
                            generation,
                            request_revision,
                            result,
                        });
                    }
                    FeedCaptureJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git feed-capture worker")
}

/// Single-threaded local mutation worker. Project-scoped jobs (CreateWorktree,
/// RollbackWorktree) share this channel with scope-suppressed jobs (Stage, Unstage,
/// etc.). A slow worktree creation will delay pending scope-suppressed actions -
/// this is an intentional trade-off for single-worker serialization simplicity.
fn spawn_local_mutation_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<LocalMutationJob>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-local-mutation".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    LocalMutationJob::CreateWorktree {
                        project_id,
                        source_path,
                        origin_terminal_id,
                    } => {
                        let worktree_id = next_worktree_id();
                        let result = create_managed_worktree(&source_path, &worktree_id)
                            .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::ManagedWorktreeCreated {
                            project_id,
                            origin_terminal_id,
                            source_path,
                            result,
                        });
                    }
                    LocalMutationJob::RollbackWorktree {
                        project_id,
                        worktree,
                        original_error,
                    } => {
                        // Rollback always deletes the branch since it was just created.
                        let result = remove_managed_worktree(&worktree.path, true)
                            .map_err(|error| error.to_string());
                        inner.broadcast(GitEvent::ManagedWorktreeRollbackComplete {
                            project_id,
                            worktree,
                            original_error,
                            result,
                        });
                    }
                    LocalMutationJob::Stage { scope_root, paths } => {
                        let result = stage_paths(&scope_root, &paths)
                            .map(|()| "Staged successfully".to_string())
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::Stage, result);
                    }
                    LocalMutationJob::Unstage { scope_root, paths } => {
                        let result = unstage_paths(&scope_root, &paths)
                            .map(|()| "Unstaged successfully".to_string())
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::Unstage, result);
                    }
                    LocalMutationJob::Commit {
                        scope_root,
                        message,
                    } => {
                        let result = commit_staged(&scope_root, &message)
                            .map(|oid| {
                                let short = oid.to_string();
                                let short = &short[..short.len().min(8)];
                                format!("Committed {short}")
                            })
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::Commit, result);
                    }
                    LocalMutationJob::CreateStash {
                        scope_root,
                        message,
                        keep_index,
                        include_untracked,
                    } => {
                        let result = create_stash(
                            &scope_root,
                            message.as_deref(),
                            keep_index,
                            include_untracked,
                        )
                        .map(|_| "Created stash@{0}".to_string())
                        .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::CreateStash, result);
                    }
                    LocalMutationJob::ApplyStash {
                        scope_root,
                        stash_oid,
                    } => {
                        let result = match apply_stash(&scope_root, stash_oid) {
                            Ok(StashMutationOutcome::Applied { label }) => {
                                Ok(format!("Applied {label}"))
                            }
                            Ok(StashMutationOutcome::Conflicted {
                                affected_scope,
                                conflicted_files,
                            }) => {
                                inner.emit_merge_conflict(
                                    &scope_root,
                                    &affected_scope,
                                    conflicted_files.clone(),
                                    MergeConflictTrigger::StashApply,
                                );
                                inner.enqueue_snapshot(SnapshotJob {
                                    key: affected_scope.clone(),
                                    request_path: affected_scope,
                                    terminal_ids: Vec::new(),
                                });
                                Ok("CONFLICT: Resolve stash apply conflicts in the diff tab."
                                    .to_string())
                            }
                            Err(error) => Err(error.to_string()),
                        };
                        inner.finish_local_action(&scope_root, GitActionKind::ApplyStash, result);
                    }
                    LocalMutationJob::PopStash {
                        scope_root,
                        stash_oid,
                    } => {
                        let result = match pop_stash(&scope_root, stash_oid) {
                            Ok(StashMutationOutcome::Applied { label }) => {
                                Ok(format!("Popped {label}"))
                            }
                            Ok(StashMutationOutcome::Conflicted {
                                affected_scope,
                                conflicted_files,
                            }) => {
                                inner.emit_merge_conflict(
                                    &scope_root,
                                    &affected_scope,
                                    conflicted_files.clone(),
                                    MergeConflictTrigger::StashPop,
                                );
                                inner.enqueue_snapshot(SnapshotJob {
                                    key: affected_scope.clone(),
                                    request_path: affected_scope,
                                    terminal_ids: Vec::new(),
                                });
                                Ok("CONFLICT: Resolve stash pop conflicts in the diff tab."
                                    .to_string())
                            }
                            Err(error) => Err(error.to_string()),
                        };
                        inner.finish_local_action(&scope_root, GitActionKind::PopStash, result);
                    }
                    LocalMutationJob::DropStash {
                        scope_root,
                        stash_oid,
                    } => {
                        let result = drop_stash(&scope_root, stash_oid)
                            .map(|label| format!("Dropped {label}"))
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::DropStash, result);
                    }
                    LocalMutationJob::DiscardAll { scope_root } => {
                        let result = discard_all_unstaged(&scope_root)
                            .map(|outcome| {
                                map_discard_outcome(
                                    outcome,
                                    "Discarded all unstaged changes".to_string(),
                                )
                            })
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::DiscardAll, result);
                    }
                    LocalMutationJob::DiscardFile {
                        scope_root,
                        relative_path,
                    } => {
                        let result = discard_unstaged_file(&scope_root, &relative_path)
                            .map(|outcome| {
                                map_discard_outcome(
                                    outcome,
                                    format!("Discarded {}", relative_path.display()),
                                )
                            })
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::DiscardFile, result);
                    }
                    LocalMutationJob::DiscardHunk {
                        scope_root,
                        relative_path,
                        target,
                    } => {
                        let result = discard_unstaged_hunk(&scope_root, &relative_path, &target)
                            .map(|outcome| {
                                map_discard_outcome(
                                    outcome,
                                    format!(
                                        "Discarded hunk {} in {}",
                                        target.hunk_index,
                                        relative_path.display()
                                    ),
                                )
                            })
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::DiscardHunk, result);
                    }
                    LocalMutationJob::MergeBack {
                        scope_root,
                        source_ref,
                    } => {
                        use orcashell_git::MergeOutcome;
                        let merge_result = merge_managed_branch(&scope_root, &source_ref);

                        // On success, refresh the source scope so its diff tab and sidebar update.
                        if let Ok(
                            MergeOutcome::FastForward { .. } | MergeOutcome::MergeCommit { .. },
                        ) = &merge_result
                        {
                            // Discover repo_root from the managed scope to resolve source scope.
                            if let Ok(scope) = orcashell_git::discover_scope(&scope_root) {
                                if let Ok(source_path) =
                                    resolve_source_scope(&scope.repo_root, &source_ref)
                                {
                                    inner.enqueue_snapshot(SnapshotJob {
                                        key: source_path.clone(),
                                        request_path: source_path,
                                        terminal_ids: Vec::new(),
                                    });
                                }
                            }
                        }

                        let result = match merge_result {
                            Ok(MergeOutcome::AlreadyMerged) => Ok("Already up to date".to_string()),
                            Ok(MergeOutcome::FastForward { new_head }) => {
                                let short = new_head.to_string();
                                let short = &short[..short.len().min(8)];
                                Ok(format!("Fast-forwarded to {short}"))
                            }
                            Ok(MergeOutcome::MergeCommit { merge_oid }) => {
                                let short = merge_oid.to_string();
                                let short = &short[..short.len().min(8)];
                                Ok(format!("Merge commit {short}"))
                            }
                            Ok(MergeOutcome::Conflicted {
                                affected_scope,
                                conflicted_files,
                            }) => {
                                inner.emit_merge_conflict(
                                    &scope_root,
                                    &affected_scope,
                                    conflicted_files.clone(),
                                    MergeConflictTrigger::MergeBack,
                                );
                                inner.enqueue_snapshot(SnapshotJob {
                                    key: affected_scope.clone(),
                                    request_path: affected_scope,
                                    terminal_ids: Vec::new(),
                                });
                                Ok("CONFLICT: Resolve conflicts in the source scope diff tab."
                                    .to_string())
                            }
                            Ok(MergeOutcome::Blocked { reason }) => {
                                Ok(format!("BLOCKED: {reason}"))
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        inner.finish_local_action(&scope_root, GitActionKind::MergeBack, result);
                    }
                    LocalMutationJob::CompleteMerge { scope_root } => {
                        let result = complete_merge(&scope_root)
                            .map(|merge_oid| {
                                let short = merge_oid.to_string();
                                let short = &short[..short.len().min(8)];
                                format!("Merge commit {short}")
                            })
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::CompleteMerge,
                            result,
                        );
                    }
                    LocalMutationJob::AbortMerge { scope_root } => {
                        let result = abort_merge(&scope_root)
                            .map(|()| "Merge aborted".to_string())
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(&scope_root, GitActionKind::AbortMerge, result);
                    }
                    LocalMutationJob::CheckoutLocalBranch {
                        scope_root,
                        branch_name,
                    } => {
                        let result = checkout_local_branch(&scope_root, &branch_name)
                            .map(format_checkout_outcome)
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::CheckoutLocalBranch,
                            result,
                        );
                    }
                    LocalMutationJob::CheckoutRemoteBranch {
                        scope_root,
                        remote_full_ref,
                    } => {
                        let result = checkout_remote_branch(&scope_root, &remote_full_ref)
                            .map(format_checkout_outcome)
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::CheckoutRemoteBranch,
                            result,
                        );
                    }
                    LocalMutationJob::CreateLocalBranch {
                        scope_root,
                        source_branch_name,
                        new_branch_name,
                    } => {
                        let result = create_local_branch_from_local(
                            &scope_root,
                            &source_branch_name,
                            &new_branch_name,
                        )
                        .map(format_create_local_branch_outcome)
                        .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::CreateLocalBranch,
                            result,
                        );
                    }
                    LocalMutationJob::DeleteLocalBranch {
                        scope_root,
                        branch_name,
                    } => {
                        let result = delete_local_branch(&scope_root, &branch_name)
                            .map(format_delete_local_branch_outcome)
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::DeleteLocalBranch,
                            result,
                        );
                    }
                    LocalMutationJob::IntegratePull { scope_root } => {
                        use orcashell_git::MergeOutcome;
                        let result = match pull_integrate(&scope_root) {
                            Ok(MergeOutcome::AlreadyMerged) => Ok("Already up to date".to_string()),
                            Ok(MergeOutcome::FastForward { new_head }) => {
                                let short = new_head.to_string();
                                let short = &short[..short.len().min(8)];
                                Ok(format!("Pulled (fast-forward to {short})"))
                            }
                            Ok(MergeOutcome::MergeCommit { merge_oid }) => {
                                let short = merge_oid.to_string();
                                let short = &short[..short.len().min(8)];
                                Ok(format!("Pulled (merge commit {short})"))
                            }
                            Ok(MergeOutcome::Conflicted {
                                affected_scope,
                                conflicted_files,
                            }) => {
                                inner.emit_merge_conflict(
                                    &scope_root,
                                    &affected_scope,
                                    conflicted_files.clone(),
                                    MergeConflictTrigger::Pull,
                                );
                                inner.enqueue_snapshot(SnapshotJob {
                                    key: affected_scope.clone(),
                                    request_path: affected_scope,
                                    terminal_ids: Vec::new(),
                                });
                                Ok("CONFLICT: Resolve conflicts in the diff tab.".to_string())
                            }
                            Ok(MergeOutcome::Blocked { reason }) => {
                                Ok(format!("BLOCKED: {reason}"))
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        inner.finish_remote_op(
                            &scope_root,
                            GitRemoteKind::Pull,
                            None,
                            true,
                            true,
                            result,
                        );
                    }
                    LocalMutationJob::RemoveWorktree {
                        scope_root,
                        delete_branch,
                    } => {
                        let result = remove_managed_worktree(&scope_root, delete_branch)
                            .map(|()| "Worktree removed".to_string())
                            .map_err(|e| e.to_string());
                        inner.finish_local_action(
                            &scope_root,
                            GitActionKind::RemoveWorktree,
                            result,
                        );
                    }
                    LocalMutationJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git local-mutation worker")
}

fn spawn_remote_op_worker(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<RemoteOpJob>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-remote-op".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                match job {
                    RemoteOpJob::Push { scope_root } => {
                        // Pre-check: upstream must exist.
                        let result = match resolve_upstream_info(&scope_root) {
                            Err(_) => Err("No upstream configured for this branch. Use the terminal to publish it.".to_string()),
                            Ok(_) => {
                                run_git_remote_command(&scope_root, &["push"])
                                    .map_err(|e| classify_remote_error(&e))
                            }
                        };
                        inner.finish_remote_op(
                            &scope_root,
                            GitRemoteKind::Push,
                            None,
                            false,
                            true,
                            result,
                        );
                    }
                    RemoteOpJob::Publish {
                        scope_root,
                        remote_name,
                    } => {
                        let result = run_git_remote_command(
                            &scope_root,
                            &["push", "-u", &remote_name, "HEAD"],
                        )
                        .map(|_| format!("Published current branch to {remote_name}"))
                        .map_err(|e| classify_remote_error(&e));
                        let refresh_graph = result_triggers_graph_refresh(&result);
                        inner.finish_remote_op(
                            &scope_root,
                            GitRemoteKind::Publish,
                            None,
                            refresh_graph,
                            true,
                            result,
                        );
                    }
                    RemoteOpJob::Pull { scope_root } => {
                        // Phase 1: resolve upstream info for targeted fetch.
                        let upstream = match resolve_upstream_info(&scope_root) {
                            Ok(info) => info,
                            Err(e) => {
                                inner.finish_remote_op(
                                    &scope_root,
                                    GitRemoteKind::Pull,
                                    None,
                                    false,
                                    true,
                                    Err(e.to_string()),
                                );
                                continue;
                            }
                        };

                        // Phase 2: targeted fetch.
                        let fetch_result = run_git_remote_command(
                            &scope_root,
                            &[
                                "fetch",
                                "--quiet",
                                "--no-tags",
                                &upstream.remote,
                                &upstream.upstream_branch,
                            ],
                        );
                        if let Err(stderr) = fetch_result {
                            inner.finish_remote_op(
                                &scope_root,
                                GitRemoteKind::Pull,
                                None,
                                false,
                                true,
                                Err(classify_remote_error(&stderr)),
                            );
                            continue;
                        }

                        let _ = inner
                            .local_mutation_tx
                            .send(LocalMutationJob::IntegratePull { scope_root });
                    }
                    RemoteOpJob::Fetch { scope_root, origin } => {
                        let fetch_result = run_git_fetch_command(&scope_root)
                            .map_err(|e| classify_remote_error(&e));
                        let (refresh_graph, refresh_snapshot, result) = match fetch_result {
                            Ok(fetch_result) => (
                                fetch_result.refs_changed,
                                fetch_result.refs_changed,
                                Ok(fetch_result.message),
                            ),
                            Err(message) => (false, false, Err(message)),
                        };
                        inner.finish_remote_op(
                            &scope_root,
                            GitRemoteKind::Fetch,
                            Some(origin),
                            refresh_graph,
                            refresh_snapshot,
                            result,
                        );
                    }
                    RemoteOpJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git remote-op worker")
}

fn format_checkout_outcome(outcome: BranchCheckoutOutcome) -> String {
    match outcome {
        BranchCheckoutOutcome::SwitchedLocal { branch_name } => {
            format!("Checked out local branch {branch_name}")
        }
        BranchCheckoutOutcome::SwitchedTracking {
            local_branch_name,
            remote_full_ref,
            created,
        } => {
            if created {
                format!(
                    "Created and checked out tracking branch {local_branch_name} for {remote_full_ref}"
                )
            } else {
                format!("Checked out tracking branch {local_branch_name} for {remote_full_ref}")
            }
        }
        BranchCheckoutOutcome::Blocked { reason } => format!("BLOCKED: {reason}"),
    }
}

fn format_create_local_branch_outcome(outcome: CreateLocalBranchOutcome) -> String {
    match outcome {
        CreateLocalBranchOutcome::CreatedAndCheckedOut {
            source_branch_name,
            branch_name,
        } => {
            format!("Created and checked out local branch {branch_name} from {source_branch_name}")
        }
        CreateLocalBranchOutcome::Blocked { reason } => format!("BLOCKED: {reason}"),
    }
}

fn format_delete_local_branch_outcome(outcome: DeleteLocalBranchOutcome) -> String {
    match outcome {
        DeleteLocalBranchOutcome::Deleted { branch_name } => {
            format!("Deleted local branch {branch_name}")
        }
        DeleteLocalBranchOutcome::Blocked { reason } => format!("BLOCKED: {reason}"),
    }
}

fn action_triggers_graph_refresh(action: &GitActionKind) -> bool {
    matches!(
        action,
        GitActionKind::CheckoutLocalBranch
            | GitActionKind::CheckoutRemoteBranch
            | GitActionKind::CreateLocalBranch
            | GitActionKind::DeleteLocalBranch
    )
}

fn action_triggers_stash_refresh(action: &GitActionKind) -> bool {
    matches!(
        action,
        GitActionKind::CreateStash
            | GitActionKind::ApplyStash
            | GitActionKind::PopStash
            | GitActionKind::DropStash
    )
}

fn result_triggers_graph_refresh(result: &Result<String, String>) -> bool {
    matches!(result, Ok(message) if !message.starts_with("BLOCKED: ") && !message.starts_with("CONFLICT: "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchCommandResult {
    message: String,
    refs_changed: bool,
}

/// Classify git remote operation stderr into a user-friendly error message.
fn classify_remote_error(stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("could not resolve host")
        || lower.contains("unable to access")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
    {
        format!("Network error: {stderr}")
    } else if lower.contains("does not appear to be a git repository")
        || lower.contains("repository not found")
    {
        format!("Remote not found: {stderr}")
    } else if lower.contains("permission denied")
        || lower.contains("authentication failed")
        || lower.contains("could not read from remote")
        || lower.contains("terminal prompts disabled")
    {
        format!("Authentication failed. Check SSH keys or credential helper. Git said: {stderr}")
    } else if lower.contains("rejected") || lower.contains("non-fast-forward") {
        format!("Push rejected: remote has changes you don't have locally. Pull first. Git said: {stderr}")
    } else {
        stderr.to_string() // Fallback: raw message
    }
}

fn spawn_watch_dispatcher(
    inner: Arc<GitCoordinatorInner>,
    rx: CrossbeamReceiver<PathBuf>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("orca-git-watch-dispatch".into())
        .spawn(move || {
            let mut due_by_scope = HashMap::<PathBuf, Instant>::new();

            loop {
                match rx.recv_timeout(WATCHER_POLL) {
                    Ok(scope_root) => {
                        due_by_scope.insert(scope_root, Instant::now() + WATCHER_DEBOUNCE);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }

                if inner.shutdown.load(Ordering::Acquire) {
                    break;
                }

                let now = Instant::now();
                let ready: Vec<PathBuf> = due_by_scope
                    .iter()
                    .filter(|(_, due)| **due <= now)
                    .map(|(scope_root, _)| scope_root.clone())
                    .collect();

                for scope_root in ready {
                    due_by_scope.remove(&scope_root);
                    inner.enqueue_snapshot(SnapshotJob {
                        key: scope_root.clone(),
                        request_path: scope_root,
                        terminal_ids: Vec::new(),
                    });
                }
            }
        })
        .expect("failed to spawn git watch dispatcher")
}

// ── Remote shell-out helper ──────────────────────────────────────────

/// Run a git command non-interactively against the given scope root.
/// Sets GIT_TERMINAL_PROMPT=0 and GCM_INTERACTIVE=Never, closes stdin.
fn run_git_remote_command(scope_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = orcashell_platform::command("git")
        .args(["-C", &scope_root.to_string_lossy()])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        // Git convention: progress and status messages (e.g. "Everything up-to-date")
        // are written to stderr even on success. Fall back to stderr when stdout is empty.
        let msg = if stdout.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Ok(if msg.is_empty() {
            "Done".to_string()
        } else {
            msg
        })
    } else {
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Err(if msg.is_empty() {
            format!(
                "git {} failed with exit code {:?}",
                args.join(" "),
                output.status.code()
            )
        } else {
            msg
        })
    }
}

/// Run a non-interactive fetch and report whether any remote-tracking refs changed.
fn run_git_fetch_command(scope_root: &Path) -> Result<FetchCommandResult, String> {
    let output = orcashell_platform::command("git")
        .args(["-C", &scope_root.to_string_lossy()])
        .args(["fetch", "--porcelain", "--prune", "--no-tags", "--all"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        if let Some(rejected_line) = stdout
            .lines()
            .find(|line| matches!(fetch_porcelain_flag(line), Some('!')))
        {
            return Err(rejected_line.trim().to_string());
        }

        let refs_changed = stdout
            .lines()
            .filter_map(fetch_porcelain_flag)
            .any(|flag| flag != '=');
        let message = if refs_changed {
            "Fetched remote refs".to_string()
        } else {
            "Remote refs already up to date".to_string()
        };
        Ok(FetchCommandResult {
            message,
            refs_changed,
        })
    } else {
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Err(if msg.is_empty() {
            "git fetch failed".to_string()
        } else {
            msg
        })
    }
}

fn fetch_porcelain_flag(line: &str) -> Option<char> {
    let flag = line.chars().next()?;
    if line.trim().is_empty() {
        None
    } else {
        Some(flag)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn push_unique_ids(destination: &mut Vec<String>, incoming: impl IntoIterator<Item = String>) {
    for id in incoming {
        if !destination.iter().any(|existing| existing == &id) {
            destination.push(id);
        }
    }
}

fn build_watcher(
    scope_root: &Path,
    watch_event_tx: CrossbeamSender<PathBuf>,
) -> notify::Result<notify::RecommendedWatcher> {
    let scope_root = normalize_path(scope_root);
    let ignored_root = scope_root.join(".orcashell");
    let callback_scope_root = scope_root.clone();

    let mut watcher =
        recommended_watcher(move |result: notify::Result<NotifyEvent>| match result {
            Ok(event) => {
                if !event.paths.is_empty()
                    && event
                        .paths
                        .iter()
                        .all(|path| path.starts_with(&ignored_root))
                {
                    return;
                }
                let _ = watch_event_tx.send(callback_scope_root.clone());
            }
            Err(error) => {
                warn!(?callback_scope_root, "git watcher callback error: {error}");
            }
        })?;

    watcher.watch(&scope_root, RecursiveMode::Recursive)?;

    let git_metadata_path = scope_root.join(".git");
    if git_metadata_path.exists() {
        let mode = if git_metadata_path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher.watch(&git_metadata_path, mode)?;
    }

    Ok(watcher)
}

fn next_worktree_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("wt-{}", &uuid[..8])
}

fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// ── Test infrastructure ──────────────────────────────────────────────

#[cfg(test)]
static SNAPSHOT_TEST_DELAY_MS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn snapshot_worker_test_delay() -> Duration {
    Duration::from_millis(SNAPSHOT_TEST_DELAY_MS.load(Ordering::Relaxed))
}

#[cfg(test)]
struct SnapshotDelayGuard;

#[cfg(test)]
impl Drop for SnapshotDelayGuard {
    fn drop(&mut self) {
        SNAPSHOT_TEST_DELAY_MS.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
fn set_snapshot_test_delay(delay: Duration) -> SnapshotDelayGuard {
    SNAPSHOT_TEST_DELAY_MS.store(delay.as_millis() as u64, Ordering::Relaxed);
    SnapshotDelayGuard
}

#[cfg(test)]
static REPO_BROWSER_TEST_BLOCKERS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
fn wait_for_repo_browser_test_unblock() {
    while REPO_BROWSER_TEST_BLOCKERS.load(Ordering::Relaxed) > 0 {
        sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
struct RepoBrowserBlockGuard;

#[cfg(test)]
impl Drop for RepoBrowserBlockGuard {
    fn drop(&mut self) {
        REPO_BROWSER_TEST_BLOCKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
fn block_repo_browser_worker() -> RepoBrowserBlockGuard {
    REPO_BROWSER_TEST_BLOCKERS.fetch_add(1, Ordering::Relaxed);
    RepoBrowserBlockGuard
}

#[cfg(test)]
mod tests;
