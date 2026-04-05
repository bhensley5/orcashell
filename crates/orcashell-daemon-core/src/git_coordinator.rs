use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(test)]
use std::{sync::atomic::AtomicU64, thread::sleep};

use async_channel::{
    unbounded as async_unbounded, Receiver as AsyncReceiver, Sender as AsyncSender,
};
use crossbeam_channel::{
    unbounded as crossbeam_unbounded, Receiver as CrossbeamReceiver, RecvTimeoutError,
    Sender as CrossbeamSender,
};
use notify::{recommended_watcher, Event as NotifyEvent, RecursiveMode, Watcher};
use orcashell_git::{
    capture_feed_event, commit_staged, create_managed_worktree, load_diff_index, load_file_diff,
    load_snapshot, merge_managed_branch, pull_integrate, remove_managed_worktree,
    resolve_source_scope, resolve_upstream_info, stage_paths, unstage_paths, DiffDocument,
    DiffSelectionKey, FeedCaptureResult, FeedScopeCapture, FileDiffDocument, GitSnapshotSummary,
    ManagedWorktree,
};
use orcashell_store::ThemeId;
use parking_lot::Mutex;
use tracing::warn;
use uuid::Uuid;

const WATCHER_DEBOUNCE: Duration = Duration::from_millis(250);
const WATCHER_POLL: Duration = Duration::from_millis(25);
const SNAPSHOT_WORKERS: usize = 2;

// ── Public event types ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitActionKind {
    Stage,
    Unstage,
    Commit,
    MergeBack,
    RemoveWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRemoteKind {
    Push,
    Pull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvent {
    SnapshotUpdated {
        terminal_ids: Vec<String>,
        request_path: PathBuf,
        scope_root: Option<PathBuf>,
        result: Result<GitSnapshotSummary, String>,
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
    RemoteOpCompleted {
        scope_root: PathBuf,
        kind: GitRemoteKind,
        result: Result<String, String>,
    },
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
        if !self.inner.try_start_remote_op(&scope_root) {
            warn!(?scope_root, "suppressed push: remote op already in flight");
            return;
        }
        let _ = self
            .inner
            .remote_op_tx
            .send(RemoteOpJob::Push { scope_root });
    }

    pub fn pull_current_branch(&self, scope_root: &Path) {
        let scope_root = self.resolve_scope_root(scope_root);
        if !self.inner.try_start_remote_op(&scope_root) {
            warn!(?scope_root, "suppressed pull: remote op already in flight");
            return;
        }
        let _ = self
            .inner
            .remote_op_tx
            .send(RemoteOpJob::Pull { scope_root });
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

    /// Try to start a local action for the given scope. Returns false if any
    /// action (local or remote) is already in flight (scope-wide exclusion).
    fn try_start_local_action(&self, scope_root: &Path) -> bool {
        let mut state = self.state.lock();
        let scope = state.scopes.entry(scope_root.to_path_buf()).or_default();
        if scope.local_action_in_flight || scope.remote_op_in_flight {
            return false;
        }
        scope.local_action_in_flight = true;
        true
    }

    /// Try to start a remote op for the given scope. Returns false if any
    /// action (local or remote) is already in flight (scope-wide exclusion).
    fn try_start_remote_op(&self, scope_root: &Path) -> bool {
        let mut state = self.state.lock();
        let scope = state.scopes.entry(scope_root.to_path_buf()).or_default();
        if scope.local_action_in_flight || scope.remote_op_in_flight {
            return false;
        }
        scope.remote_op_in_flight = true;
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
        // Trigger snapshot refresh so UI sees updated file lists.
        self.enqueue_snapshot(SnapshotJob {
            key: scope_root.clone(),
            request_path: scope_root.clone(),
            terminal_ids: Vec::new(),
        });
    }

    /// Clear the remote-op in-flight flag, broadcast the completion event,
    /// and enqueue a snapshot refresh for the scope.
    fn finish_remote_op(
        &self,
        scope_root: &PathBuf,
        kind: GitRemoteKind,
        result: Result<String, String>,
    ) {
        {
            let mut state = self.state.lock();
            if let Some(scope) = state.scopes.get_mut(scope_root) {
                scope.remote_op_in_flight = false;
            }
        }
        self.broadcast(GitEvent::RemoteOpCompleted {
            scope_root: scope_root.clone(),
            kind,
            result,
        });
        self.enqueue_snapshot(SnapshotJob {
            key: scope_root.clone(),
            request_path: scope_root.clone(),
            terminal_ids: Vec::new(),
        });
    }

    fn finish_snapshot(&self, job: SnapshotJob, result: Result<GitSnapshotSummary, String>) {
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
                Err(message) => {
                    scope_state.last_error = Some(message.clone());
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
                            result: Err(message),
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
    MergeBack {
        scope_root: PathBuf,
        source_ref: String,
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
    Push { scope_root: PathBuf },
    Pull { scope_root: PathBuf },
    Stop,
}

// ── Workers ──────────────────────────────────────────────────────────

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
                        let result =
                            load_snapshot(&job.request_path, 0).map_err(|error| error.to_string());
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
                            Ok(MergeOutcome::Blocked { reason }) => {
                                Ok(format!("BLOCKED: {reason}"))
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        inner.finish_local_action(&scope_root, GitActionKind::MergeBack, result);
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
                        inner.finish_remote_op(&scope_root, GitRemoteKind::Push, result);
                    }
                    RemoteOpJob::Pull { scope_root } => {
                        // Phase 1: resolve upstream info for targeted fetch.
                        let upstream = match resolve_upstream_info(&scope_root) {
                            Ok(info) => info,
                            Err(e) => {
                                inner.finish_remote_op(
                                    &scope_root,
                                    GitRemoteKind::Pull,
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
                                Err(classify_remote_error(&stderr)),
                            );
                            continue;
                        }

                        // Phase 3: local integration (fast-forward or merge).
                        use orcashell_git::MergeOutcome;
                        let result = match pull_integrate(&scope_root) {
                            Ok(MergeOutcome::AlreadyMerged) => {
                                Ok("Already up to date".to_string())
                            }
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
                            Ok(MergeOutcome::Blocked { reason }) => {
                                Ok(format!("BLOCKED: {reason}"))
                            }
                            Err(e) => Err(e.to_string()),
                        };
                        inner.finish_remote_op(&scope_root, GitRemoteKind::Pull, result);
                    }
                    RemoteOpJob::Stop => break,
                }
            }
        })
        .expect("failed to spawn git remote-op worker")
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
    } else if lower.contains("permission denied")
        || lower.contains("authentication failed")
        || lower.contains("could not read from remote")
        || lower.contains("terminal prompts disabled")
    {
        format!("Authentication failed. Check SSH keys or credential helper. Git said: {stderr}")
    } else if lower.contains("rejected") || lower.contains("non-fast-forward") {
        format!("Push rejected: remote has changes you don't have locally. Pull first. Git said: {stderr}")
    } else if lower.contains("does not appear to be a git repository") {
        format!("Remote not found: {stderr}")
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
mod tests {
    use super::*;
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

    fn recv_event(rx: &AsyncReceiver<GitEvent>) -> GitEvent {
        rx.recv_blocking()
            .expect("git event channel closed unexpectedly")
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
}
