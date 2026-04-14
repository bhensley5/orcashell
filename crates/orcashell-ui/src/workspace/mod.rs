pub mod actions;
pub mod focus;
pub mod layout;
pub mod project;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::*;
use orcashell_daemon_core::git_coordinator::{
    GitActionKind, GitCoordinator, GitEvent, GitFetchOrigin, GitRemoteKind, MergeConflictTrigger,
};
use orcashell_git::{
    discover_scope, parse_conflict_file_text, rehighlight_captured_feed_event, ChangedFile,
    CommitDetailDocument, CommitFileDiffDocument, CommitFileSelection, DiffDocument, DiffLineKind,
    DiffSectionKind, DiffSelectionKey, DiscardHunkTarget, FeedCaptureResult, FeedCapturedEvent,
    FeedEventData, FeedEventFailure, FeedEventFile, FeedEventFileSummary, FeedEventKind,
    FeedScopeCapture, FileDiffDocument, GitFileStatus, GitSnapshotSummary, HeadState,
    ManagedWorktree, Oid, ParsedConflictBlock, RepositoryGraphDocument, SnapshotLoadError,
    SnapshotLoadErrorKind, StashDetailDocument, StashFileDiffDocument, StashFileSelection,
    StashListDocument, MAX_RENDERED_DIFF_BYTES, MAX_RENDERED_DIFF_LINES,
};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::settings::{AppSettings, ThemeId};
use crate::theme;
use crate::theme::OrcaTheme;
use focus::{FocusManager, FocusTarget};
use layout::{LayoutNode, SplitDirection};
use orcashell_session::event::TerminalColors;
use orcashell_session::SemanticState;
use orcashell_session::SessionEngine;
use orcashell_store::{
    ResumableAgentKind, Store, StoredAgentTerminal, StoredProject, StoredWorktree,
};
use orcashell_terminal_view::{
    ColorPalette, CursorShape, TerminalConfig, TerminalRuntimeEvent, TerminalView, TextInputState,
};
use project::ProjectData;

const REPOSITORY_AUTO_FETCH_INTERVAL: Duration = Duration::from_secs(180);
const REPOSITORY_AUTO_FETCH_FAILURE_COOLDOWN: Duration = Duration::from_secs(300);

/// Where the rename input should appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameLocation {
    TabBar,
    Sidebar,
}

/// State for an active inline rename of a terminal.
pub struct RenameState {
    pub project_id: String,
    pub terminal_id: String,
    pub input: Entity<TextInputState>,
    pub location: RenameLocation,
    /// Whether the input has been focused at least once (for blur-to-commit).
    pub focused_once: bool,
}

/// Urgency tier for a terminal's pending notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationTier {
    /// Agent needs user action (permission prompt).
    Urgent,
    /// Agent is done or informational (turn complete, bell).
    Informational,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuxiliaryTabKind {
    Settings,
    Diff { scope_root: PathBuf },
    LiveDiffStream { project_id: String },
    RepositoryGraph { project_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxiliaryTabState {
    pub id: String,
    pub title: String,
    pub kind: AuxiliaryTabKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffIndexState {
    pub document: Option<DiffDocument>,
    pub error: Option<String>,
    pub loading: bool,
    pub requested_generation: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffFileState {
    pub document: Option<FileDiffDocument>,
    pub error: Option<String>,
    pub loading: bool,
    pub requested_generation: Option<u64>,
    pub requested_selection: Option<DiffSelectionKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTabViewMode {
    WorkingTree,
    Stashes,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffTabStashState {
    pub list: AsyncDocumentState<StashListDocument>,
    pub selected_stash: Option<Oid>,
    pub expanded_stash: Option<Oid>,
    pub detail: AsyncDocumentState<StashDetailDocument>,
    pub selected_file: Option<StashFileSelection>,
    pub file_diff: AsyncDocumentState<StashFileDiffDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncDocumentState<T> {
    pub document: Option<T>,
    pub error: Option<String>,
    pub loading: bool,
    pub requested_revision: u64,
}

impl<T> Default for AsyncDocumentState<T> {
    fn default() -> Self {
        Self {
            document: None,
            error: None,
            loading: false,
            requested_revision: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConflictEditorDocument {
    pub generation: u64,
    pub version: u64,
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub initial_raw_text: String,
    pub raw_text: String,
    pub blocks: Vec<ParsedConflictBlock>,
    pub has_base_sections: bool,
    pub parse_error: Option<String>,
    pub is_dirty: bool,
    pub cursor_pos: usize,
    pub selection_range: Option<Range<usize>>,
    pub active_block_index: Option<usize>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictUnavailableState {
    pub generation: u64,
    pub selection: DiffSelectionKey,
    pub file: ChangedFile,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictDocumentState {
    Loaded(ConflictEditorDocument),
    Unavailable(ConflictUnavailableState),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConflictEditorState {
    pub documents: HashMap<PathBuf, ConflictDocumentState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedEntryOrigin {
    BootstrapSnapshot,
    LiveDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedScopeKind {
    ProjectRoot,
    ManagedWorktree,
}

pub type ChangeFeedFileSummary = FeedEventFileSummary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedCaptureState {
    Pending,
    Ready(CapturedEventDiff),
    Truncated(CapturedEventDiff),
    Failed { message: String },
}

pub type CapturedEventDiff = FeedCapturedEvent;
pub type CapturedDiffFile = FeedEventFile;
pub type CapturedDiffFailure = FeedEventFailure;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPreviewFile {
    pub selection: DiffSelectionKey,
    pub relative_path: PathBuf,
    pub lines: Vec<orcashell_git::DiffLineView>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPreviewLayout {
    pub files: Vec<FeedPreviewFile>,
    pub hidden_file_count: usize,
    pub hidden_file_names: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeFeedEntry {
    pub id: u64,
    pub observed_at: SystemTime,
    pub origin: FeedEntryOrigin,
    pub scope_root: PathBuf,
    pub branch_name: String,
    pub scope_kind: FeedScopeKind,
    pub worktree_name: Option<String>,
    pub source_terminal_id: Option<String>,
    pub generation: u64,
    pub changed_file_count: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<ChangeFeedFileSummary>,
    pub capture_state: FeedCaptureState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedScopeState {
    pub last_generation: Option<u64>,
    pub emitted_baseline: Option<Arc<FeedScopeCapture>>,
    pub pending_refresh: bool,
    pub bootstrap_emitted: bool,
    pub latest_snapshot_generation: Option<u64>,
    pub latest_request_revision: u64,
    pub latest_error: Option<String>,
    pub error_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeFeedState {
    pub project_id: String,
    pub entries: VecDeque<ChangeFeedEntry>,
    pub selected_entry_id: Option<u64>,
    pub detail_pane_open: bool,
    pub unread_count: usize,
    pub live_follow: bool,
    next_entry_id: u64,
    next_error_revision: u64,
    tracked_scopes: HashMap<PathBuf, FeedScopeState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionBannerKind {
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBanner {
    pub kind: ActionBannerKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBannerKind {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBanner {
    pub kind: WorkspaceBannerKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWorktreeConfirm {
    pub delete_branch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWorktreeSummary {
    pub id: String,
    pub branch_name: String,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryBranchSelection {
    Local { name: String },
    Remote { full_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryBranchAction {
    Checkout {
        selection: RepositoryBranchSelection,
    },
    Create {
        source_branch_name: String,
        new_branch_name: String,
    },
    Delete {
        branch_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryGraphTabState {
    pub project_id: String,
    pub scope_root: PathBuf,
    pub graph: AsyncDocumentState<RepositoryGraphDocument>,
    pub selected_branch: Option<RepositoryBranchSelection>,
    pub selected_commit: Option<Oid>,
    pub commit_detail: AsyncDocumentState<CommitDetailDocument>,
    pub selected_commit_file: Option<CommitFileSelection>,
    pub commit_file_diff: AsyncDocumentState<CommitFileDiffDocument>,
    pub fetch_in_flight: bool,
    pub pull_in_flight: bool,
    pub active_fetch_origin: Option<GitFetchOrigin>,
    pub last_remote_check_at: Option<SystemTime>,
    pub last_automatic_fetch_failure_at: Option<SystemTime>,
    pub active_branch_action: Option<RepositoryBranchAction>,
    pub action_banner: Option<ActionBanner>,
    pub occupied_local_branches: HashSet<String>,
    pub expanded_remote_groups: HashSet<String>,
    pub remote_groups_seeded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffTabState {
    pub scope_root: PathBuf,
    pub tree_width: f32,
    pub view_mode: DiffTabViewMode,
    pub index: DiffIndexState,
    pub selected_file: Option<DiffSelectionKey>,
    pub file: DiffFileState,
    pub stash: DiffTabStashState,
    pub conflict_editor: ConflictEditorState,
    // Phase 4.5 CP2 fields
    pub multi_select: HashSet<DiffSelectionKey>,
    pub selection_anchor: Option<DiffSelectionKey>,
    pub commit_message: String,
    pub local_action_in_flight: bool,
    pub remote_op_in_flight: bool,
    pub last_action_banner: Option<ActionBanner>,
    pub remove_worktree_confirm: Option<RemoveWorktreeConfirm>,
    pub managed_worktree: Option<ManagedWorktreeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalRuntimeState {
    pub(crate) shell_label: String,
    live_title: Option<String>,
    semantic_state: SemanticState,
    last_activity_at: Option<Instant>,
    last_local_input_at: Option<Instant>,
    /// Active notification tier, if any. Cleared on focus.
    pub(crate) notification_tier: Option<NotificationTier>,
    resumable_agent: Option<ResumableAgentKind>,
    pending_agent_detection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeInjectionTrigger {
    PromptReady,
    TimeoutFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingResumeInjection {
    terminal_id: String,
    agent_kind: ResumableAgentKind,
    command: &'static str,
    resume_attempted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedResumeInjection {
    terminal_id: String,
    agent_kind: ResumableAgentKind,
    command: &'static str,
    trigger: ResumeInjectionTrigger,
}

#[derive(Debug, Default)]
struct ProjectResumeRestorePlan {
    rows_by_terminal_id: HashMap<String, StoredAgentTerminal>,
    winning_terminal_ids: HashSet<String>,
    suppressed_duplicates: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticTransition {
    changed: bool,
    refresh_git_snapshot: bool,
    entered_executing: bool,
    left_executing: bool,
    prompt_ready: bool,
}

const ACTIVITY_PULSE_WINDOW: Duration = Duration::from_millis(1000);
const LOCAL_INPUT_SUPPRESS_WINDOW: Duration = Duration::from_millis(1000);
const SETTINGS_TAB_ID: &str = "aux-settings";
const DEFAULT_DIFF_TREE_WIDTH: f32 = 300.0;
const FEED_ENTRY_RETENTION_CAP: usize = 2_000;
#[allow(dead_code)]
const FEED_PREVIEW_LINE_BUDGET: usize = 18;
#[allow(dead_code)]
const FEED_PREVIEW_FILE_CAP: usize = 3;
#[allow(dead_code)]
const FEED_PREVIEW_MIN_LINES_PER_FILE: usize = 3;
#[allow(dead_code)]
const FEED_PREVIEW_HIDDEN_NAME_CAP: usize = 3;
const REPOSITORY_GRAPH_AUTO_REFRESH_REVISION: u64 = 0;

fn classify_notification(title: &str, body: &str, patterns: &[String]) -> NotificationTier {
    let lower = format!("{title} {body}").to_lowercase();
    if patterns.iter().any(|p| lower.contains(&p.to_lowercase())) {
        NotificationTier::Urgent
    } else {
        NotificationTier::Informational
    }
}

pub struct WorkspaceState {
    services: WorkspaceServices,
    pub projects: Vec<ProjectData>,
    pub active_project_id: Option<String>,
    pub terminal_views: HashMap<String, Entity<TerminalView>>,
    pub(crate) terminal_runtime: HashMap<String, TerminalRuntimeState>,
    git_scopes: HashMap<PathBuf, GitSnapshotSummary>,
    terminal_git_scopes: HashMap<String, PathBuf>,
    pub focus: FocusManager,
    auxiliary_tabs: Vec<AuxiliaryTabState>,
    active_auxiliary_tab_id: Option<String>,
    diff_tabs: HashMap<PathBuf, DiffTabState>,
    live_diff_feeds: HashMap<String, ChangeFeedState>,
    repository_graph_tabs: HashMap<String, RepositoryGraphTabState>,
    /// Active inline rename, if any.
    pub renaming: Option<RenameState>,
    pub workspace_banner: Option<WorkspaceBanner>,
    pending_focus_terminal_id: Option<String>,
    pending_resume_injections: HashMap<String, PendingResumeInjection>,
    pending_resume_timeout_tasks: HashMap<String, Task<()>>,
}

#[derive(Clone)]
pub struct WorkspaceServices {
    pub git: GitCoordinator,
    pub store: Arc<Mutex<Option<Store>>>,
}

impl Default for WorkspaceServices {
    fn default() -> Self {
        Self {
            git: GitCoordinator::new(),
            store: Arc::new(Mutex::new(None)),
        }
    }
}

impl DiffTabState {
    fn new(scope_root: PathBuf) -> Self {
        Self {
            scope_root,
            tree_width: DEFAULT_DIFF_TREE_WIDTH,
            view_mode: DiffTabViewMode::WorkingTree,
            index: DiffIndexState::default(),
            selected_file: None,
            file: DiffFileState::default(),
            stash: DiffTabStashState::default(),
            conflict_editor: ConflictEditorState::default(),
            multi_select: HashSet::new(),
            selection_anchor: None,
            commit_message: String::new(),
            local_action_in_flight: false,
            remote_op_in_flight: false,
            last_action_banner: None,
            remove_worktree_confirm: None,
            managed_worktree: None,
        }
    }
}

impl RepositoryGraphTabState {
    fn new(project_id: String, scope_root: PathBuf) -> Self {
        Self {
            project_id,
            scope_root,
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
}

impl ChangeFeedState {
    fn new(project_id: String) -> Self {
        Self {
            project_id,
            entries: VecDeque::new(),
            selected_entry_id: None,
            detail_pane_open: false,
            unread_count: 0,
            live_follow: true,
            next_entry_id: 1,
            next_error_revision: 1,
            tracked_scopes: HashMap::new(),
        }
    }

    fn replace_tracked_scopes(&mut self, scope_roots: impl IntoIterator<Item = PathBuf>) {
        let mut next_scopes = HashMap::new();
        for scope_root in scope_roots {
            let state = self.tracked_scopes.remove(&scope_root).unwrap_or_default();
            next_scopes.insert(scope_root, state);
        }
        self.tracked_scopes = next_scopes;
    }

    pub fn tracked_scope_count(&self) -> usize {
        self.tracked_scopes.len()
    }

    pub fn latest_scope_error(&self) -> Option<&str> {
        self.tracked_scopes
            .values()
            .filter_map(|scope| {
                scope
                    .latest_error
                    .as_deref()
                    .map(|error| (scope.error_revision, error))
            })
            .max_by_key(|(revision, _)| *revision)
            .map(|(_, error)| error)
    }
}

fn count_text_lines(text: &str) -> usize {
    if text.is_empty() {
        1
    } else {
        text.bytes().filter(|byte| *byte == b'\n').count() + 1
    }
}

fn clamp_to_char_boundary(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len();
    }
    let mut offset = offset;
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn clamp_selection_range(text: &str, selection: Option<Range<usize>>) -> Option<Range<usize>> {
    let selection = selection?;
    let start = clamp_to_char_boundary(text, selection.start.min(text.len()));
    let end = clamp_to_char_boundary(text, selection.end.min(text.len()));
    (start < end).then_some(start..end)
}

fn active_block_for_cursor(blocks: &[ParsedConflictBlock], cursor_pos: usize) -> Option<usize> {
    blocks
        .iter()
        .find(|block| cursor_pos >= block.whole_block.start && cursor_pos <= block.whole_block.end)
        .map(|block| block.block_index)
}

fn merge_conflict_document_from_text(
    generation: u64,
    selection: DiffSelectionKey,
    file: ChangedFile,
    raw_text: String,
    prior: Option<&ConflictEditorDocument>,
    is_dirty: bool,
) -> ConflictEditorDocument {
    let (blocks, has_base_sections, parse_error) = match parse_conflict_file_text(&raw_text) {
        Ok(parsed) => (parsed.blocks, parsed.has_base_sections, None),
        Err(error) => (Vec::new(), false, Some(error.to_string())),
    };

    let cursor_pos = prior
        .map(|document| clamp_to_char_boundary(&raw_text, document.cursor_pos))
        .unwrap_or_else(|| {
            blocks
                .first()
                .map(|block| block.whole_block.start)
                .unwrap_or(0)
        });
    let selection_range = prior
        .and_then(|document| clamp_selection_range(&raw_text, document.selection_range.clone()));
    let active_block_index = active_block_for_cursor(&blocks, cursor_pos)
        .or_else(|| blocks.first().map(|block| block.block_index));

    ConflictEditorDocument {
        generation,
        version: prior.map_or(0, |document| document.version.saturating_add(1)),
        selection,
        file,
        initial_raw_text: prior
            .map(|document| document.initial_raw_text.clone())
            .unwrap_or_else(|| raw_text.clone()),
        raw_text,
        blocks,
        has_base_sections,
        parse_error,
        is_dirty,
        cursor_pos,
        selection_range,
        active_block_index,
        scroll_x: prior.map_or(0.0, |document| document.scroll_x),
        scroll_y: prior.map_or(0.0, |document| document.scroll_y),
    }
}

fn unreadable_conflict_message(error: &std::io::Error) -> String {
    format!("Could not read conflicted file: {error}")
}

impl WorkspaceState {
    pub fn new() -> Self {
        Self::new_with_services(WorkspaceServices::default())
    }

    pub fn new_with_services(services: WorkspaceServices) -> Self {
        Self {
            services,
            projects: Vec::new(),
            active_project_id: None,
            terminal_views: HashMap::new(),
            terminal_runtime: HashMap::new(),
            git_scopes: HashMap::new(),
            terminal_git_scopes: HashMap::new(),
            focus: FocusManager::new(),
            auxiliary_tabs: Vec::new(),
            active_auxiliary_tab_id: None,
            diff_tabs: HashMap::new(),
            live_diff_feeds: HashMap::new(),
            repository_graph_tabs: HashMap::new(),
            renaming: None,
            workspace_banner: None,
            pending_focus_terminal_id: None,
            pending_resume_injections: HashMap::new(),
            pending_resume_timeout_tasks: HashMap::new(),
        }
    }

    pub fn generate_terminal_id() -> String {
        format!("term-{}", Uuid::new_v4())
    }

    /// Spawn a new terminal session. Returns `true` on success, `false` on failure.
    pub fn spawn_session(
        &mut self,
        terminal_id: &str,
        cwd: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> bool {
        let settings = cx.global::<AppSettings>();
        let palette = theme::active(cx);
        let scrollback = settings.scrollback_lines as usize;
        let shell_override = settings.default_shell.as_deref();
        let config = Self::build_terminal_config(settings, &palette);
        // Resolve once to avoid duplicate work (Windows probe-spawns are expensive).
        let resolved_shell =
            orcashell_session::shell_integration::resolve_shell_path(shell_override);
        let shell_label = Self::extract_shell_label(&resolved_shell);

        let colors = TerminalColors::new(
            theme::rgb_channels(palette.TERMINAL_FOREGROUND),
            theme::rgb_channels(palette.TERMINAL_BACKGROUND),
            theme::rgb_channels(palette.TERMINAL_CURSOR),
        );
        match SessionEngine::new_with_shell(80, 24, scrollback, cwd, colors, Some(&resolved_shell))
        {
            Ok(engine) => {
                let shell_type = engine.shell_type();
                self.attach_terminal_view(
                    terminal_id.to_string(),
                    shell_label,
                    shell_type,
                    engine,
                    config,
                    cx,
                );
                if let Some(cwd) = cwd {
                    self.services.git.request_snapshot(cwd, Some(terminal_id));
                }
                true
            }
            Err(e) => {
                tracing::error!("Failed to spawn session {terminal_id}: {e}");
                false
            }
        }
    }

    pub fn destroy_session(&mut self, terminal_id: &str) {
        self.clear_pending_resume_injection(terminal_id);
        if let Some(store) = self.services.store.lock().as_mut() {
            if let Err(err) = store.delete_agent_terminal(terminal_id) {
                tracing::warn!(
                    "Failed to delete resumable terminal {}: {}",
                    terminal_id,
                    err
                );
            }
        }
        self.terminal_views.remove(terminal_id);
        self.terminal_runtime.remove(terminal_id);
        self.detach_terminal_scope(terminal_id);
        for project in &mut self.projects {
            project.terminal_names.remove(terminal_id);
        }
    }

    pub fn terminal_view(&self, terminal_id: &str) -> Option<&Entity<TerminalView>> {
        self.terminal_views.get(terminal_id)
    }

    pub fn project(&self, id: &str) -> Option<&ProjectData> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn project_mut(&mut self, id: &str) -> Option<&mut ProjectData> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    pub fn active_project(&self) -> Option<&ProjectData> {
        self.active_project_id
            .as_ref()
            .and_then(|id| self.project(id))
    }

    pub fn active_project_mut(&mut self) -> Option<&mut ProjectData> {
        let id = self.active_project_id.clone();
        id.and_then(move |id| self.project_mut(&id))
    }

    pub fn workspace_banner(&self) -> Option<&WorkspaceBanner> {
        self.workspace_banner.as_ref()
    }

    pub fn clear_workspace_banner(&mut self, cx: &mut Context<Self>) {
        if self.workspace_banner.take().is_some() {
            cx.notify();
        }
    }

    fn report_live_diff_action_error(&mut self, message: String) {
        self.workspace_banner = Some(WorkspaceBanner {
            kind: WorkspaceBannerKind::Error,
            message,
        });
    }

    pub fn terminal_git_snapshot(&self, terminal_id: &str) -> Option<&GitSnapshotSummary> {
        self.terminal_git_scopes
            .get(terminal_id)
            .and_then(|scope_root| self.git_scopes.get(scope_root))
    }

    fn set_workspace_banner(
        &mut self,
        kind: WorkspaceBannerKind,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.workspace_banner = Some(WorkspaceBanner {
            kind,
            message: message.into(),
        });
        cx.notify();
    }

    fn set_workspace_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.set_workspace_banner(WorkspaceBannerKind::Error, message, cx);
    }

    fn set_workspace_warning(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.set_workspace_banner(WorkspaceBannerKind::Warning, message, cx);
    }

    pub fn terminal_is_git_backed(&self, terminal_id: &str) -> bool {
        self.terminal_git_snapshot(terminal_id).is_some()
    }

    pub fn git_scope_snapshot(&self, scope_root: &Path) -> Option<&GitSnapshotSummary> {
        self.git_scopes.get(scope_root)
    }

    pub fn auxiliary_tabs(&self) -> &[AuxiliaryTabState] {
        &self.auxiliary_tabs
    }

    pub fn active_auxiliary_tab(&self) -> Option<&AuxiliaryTabState> {
        self.active_auxiliary_tab_id
            .as_deref()
            .and_then(|active_id| {
                self.auxiliary_tabs
                    .iter()
                    .find(|tab| tab.id.as_str() == active_id)
            })
    }

    pub fn active_diff_scope_root(&self) -> Option<&Path> {
        match self.active_auxiliary_tab().map(|tab| &tab.kind) {
            Some(AuxiliaryTabKind::Diff { scope_root }) => Some(scope_root.as_path()),
            _ => None,
        }
    }

    pub fn live_diff_feed_state(&self, project_id: &str) -> Option<&ChangeFeedState> {
        self.live_diff_feeds.get(project_id)
    }

    pub fn repository_graph_state(&self, project_id: &str) -> Option<&RepositoryGraphTabState> {
        self.repository_graph_tabs.get(project_id)
    }

    pub fn live_diff_source_terminal_available(&self, terminal_id: &str) -> bool {
        self.terminal_exists(terminal_id)
    }

    pub fn select_feed_entry(&mut self, project_id: &str, entry_id: u64) -> bool {
        let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
            return false;
        };
        if !feed.entries.iter().any(|entry| entry.id == entry_id) {
            return false;
        }

        let changed = feed.selected_entry_id != Some(entry_id) || !feed.detail_pane_open;
        feed.selected_entry_id = Some(entry_id);
        feed.detail_pane_open = true;
        changed
    }

    pub fn close_feed_detail_pane(&mut self, project_id: &str) -> bool {
        let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
            return false;
        };
        if !feed.detail_pane_open {
            return false;
        }

        feed.detail_pane_open = false;
        feed.selected_entry_id = None;
        true
    }

    pub fn set_live_diff_feed_follow_state(&mut self, project_id: &str, live_follow: bool) -> bool {
        let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
            return false;
        };

        let unread_count = if live_follow { 0 } else { feed.unread_count };
        let changed = feed.live_follow != live_follow || feed.unread_count != unread_count;
        feed.live_follow = live_follow;
        feed.unread_count = unread_count;
        changed
    }

    pub fn resume_live_diff_feed(&mut self, project_id: &str) -> bool {
        self.set_live_diff_feed_follow_state(project_id, true)
    }

    pub fn open_diff_tab_for_scope_and_file(
        &mut self,
        scope_root: &Path,
        preferred_file: Option<DiffSelectionKey>,
    ) -> bool {
        let Some(branch_name) = self
            .git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.branch_name.clone())
        else {
            self.report_live_diff_action_error(format!(
                "The diff scope {} is no longer available.",
                scope_root.display()
            ));
            return false;
        };

        self.open_or_focus_diff_tab_internal(scope_root.to_path_buf(), branch_name);
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.selected_file = preferred_file;
            diff_tab.file.document = None;
            diff_tab.file.error = None;
        }
        self.request_diff_index_refresh(scope_root);
        true
    }

    fn open_diff_tab_for_scope_and_file_optimistic(
        &mut self,
        scope_root: &Path,
        preferred_file: Option<DiffSelectionKey>,
    ) -> bool {
        let branch_name = self
            .git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.branch_name.clone())
            .unwrap_or_else(|| {
                scope_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("scope")
                    .to_string()
            });

        self.open_or_focus_diff_tab_internal(scope_root.to_path_buf(), branch_name);
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.selected_file = preferred_file;
            diff_tab.file.document = None;
            diff_tab.file.error = None;
        }
        self.request_diff_index_refresh(scope_root);
        true
    }

    pub fn focus_terminal_by_id(&mut self, terminal_id: &str) -> bool {
        let Some(project_id) = self
            .project_id_for_terminal(terminal_id)
            .map(str::to_string)
        else {
            self.report_live_diff_action_error(format!(
                "The source terminal {terminal_id} is no longer available."
            ));
            return false;
        };
        let Some(layout_path) = self
            .project(&project_id)
            .and_then(|project| project.layout.find_terminal_path(terminal_id))
        else {
            self.report_live_diff_action_error(format!(
                "The source terminal {terminal_id} is no longer available."
            ));
            return false;
        };
        if !self.select_terminal_internal(&project_id, &layout_path) {
            self.report_live_diff_action_error(format!(
                "The source terminal {terminal_id} is no longer available."
            ));
            return false;
        }

        self.pending_focus_terminal_id = Some(terminal_id.to_string());
        true
    }

    pub fn is_settings_focused(&self) -> bool {
        self.active_auxiliary_tab()
            .is_some_and(|tab| matches!(tab.kind, AuxiliaryTabKind::Settings))
    }

    pub fn diff_tab_state(&self, scope_root: &Path) -> Option<&DiffTabState> {
        self.diff_tabs.get(scope_root)
    }

    pub(crate) fn selected_conflict_document(
        &self,
        scope_root: &Path,
    ) -> Option<&ConflictDocumentState> {
        let tab = self.diff_tabs.get(scope_root)?;
        let relative_path = {
            let selection = tab.selected_file.as_ref()?;
            if selection.section != DiffSectionKind::Conflicted {
                return None;
            }
            selection.relative_path.clone()
        };
        tab.conflict_editor.documents.get(&relative_path)
    }

    pub(crate) fn selected_conflict_document_mut(
        &mut self,
        scope_root: &Path,
    ) -> Option<&mut ConflictEditorDocument> {
        let tab = self.diff_tabs.get_mut(scope_root)?;
        let relative_path = {
            let selection = tab.selected_file.as_ref()?;
            if selection.section != DiffSectionKind::Conflicted {
                return None;
            }
            selection.relative_path.clone()
        };
        match tab.conflict_editor.documents.get_mut(&relative_path)? {
            ConflictDocumentState::Loaded(document) => Some(document),
            ConflictDocumentState::Unavailable(_) => None,
        }
    }

    pub fn take_pending_focus_terminal_id(&mut self) -> Option<String> {
        self.pending_focus_terminal_id.take()
    }

    pub fn open_diff_tab_for_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        let Some(snapshot) = self.terminal_git_snapshot(terminal_id).cloned() else {
            self.set_workspace_error("No git scope is available for this terminal.", cx);
            return;
        };

        if self.open_or_focus_diff_tab_internal(snapshot.scope_root, snapshot.branch_name) {
            cx.notify();
        }
    }

    pub fn open_live_diff_stream_for_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.open_or_focus_live_diff_stream_tab_internal(project_id) {
            cx.notify();
        }
    }

    pub fn open_repository_graph_for_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.open_or_focus_repository_graph_tab_internal(project_id) {
            self.tick_repository_auto_fetch(cx);
            cx.notify();
        }
    }

    pub fn select_repository_branch(
        &mut self,
        project_id: &str,
        selection: RepositoryBranchSelection,
        cx: &mut Context<Self>,
    ) {
        if self.select_repository_branch_internal(project_id, selection) {
            cx.notify();
        }
    }

    pub fn toggle_repository_remote_group(
        &mut self,
        project_id: &str,
        remote_name: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return;
        };

        tab.remote_groups_seeded = true;
        if !tab.expanded_remote_groups.remove(remote_name) {
            tab.expanded_remote_groups.insert(remote_name.to_string());
        }
        cx.notify();
    }

    pub fn select_repository_commit(&mut self, project_id: &str, oid: Oid, cx: &mut Context<Self>) {
        let already_selected = self
            .repository_graph_tabs
            .get(project_id)
            .is_some_and(|tab| {
                tab.selected_commit == Some(oid) && tab.selected_commit_file.is_none()
            });
        if already_selected {
            return;
        }
        self.request_repository_commit_detail(project_id, oid);
        cx.notify();
    }

    pub fn select_repository_commit_file(
        &mut self,
        project_id: &str,
        selection: CommitFileSelection,
        cx: &mut Context<Self>,
    ) {
        self.request_repository_commit_file_diff(project_id, selection);
        cx.notify();
    }

    pub fn back_to_repository_commit(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.back_to_repository_commit_internal(project_id) {
            cx.notify();
        }
    }

    pub fn fetch_repository_graph(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.start_repository_fetch(project_id, GitFetchOrigin::Manual) {
            cx.notify();
        }
    }

    pub fn pull_repository_current_branch(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some((scope_root, can_pull)) = self.repository_graph_tabs.get(project_id).map(|tab| {
            let can_pull = tab.graph.document.as_ref().is_some_and(|graph| {
                let Some(head_branch_name) = (match &graph.head {
                    HeadState::Branch { name, .. } => Some(name.as_str()),
                    HeadState::Detached { .. } | HeadState::Unborn => None,
                }) else {
                    return false;
                };
                graph.local_branches.iter().any(|branch| {
                    branch.name == head_branch_name
                        && branch.upstream.is_some()
                        && branch
                            .upstream
                            .as_ref()
                            .is_some_and(|upstream| upstream.behind > 0)
                })
            }) && self
                .git_scopes
                .get(&tab.scope_root)
                .is_none_or(|snapshot| snapshot.changed_files == 0);
            (tab.scope_root.clone(), can_pull)
        }) else {
            return;
        };

        if self.scope_git_action_in_flight(&scope_root) {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if !can_pull {
            return;
        }

        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            tab.pull_in_flight = true;
            tab.action_banner = None;
        }
        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.remote_op_in_flight = true;
        }
        self.services.git.pull_current_branch(&scope_root);
        cx.notify();
    }

    pub fn tick_repository_auto_fetch(&mut self, cx: &mut Context<Self>) {
        let now = SystemTime::now();
        let Some(project_id) = self.active_auxiliary_tab().and_then(|tab| match &tab.kind {
            AuxiliaryTabKind::RepositoryGraph { project_id } => Some(project_id.clone()),
            AuxiliaryTabKind::Settings
            | AuxiliaryTabKind::Diff { .. }
            | AuxiliaryTabKind::LiveDiffStream { .. } => None,
        }) else {
            return;
        };

        let mut should_notify = true;
        if self.repository_auto_fetch_due(&project_id, now) {
            should_notify = self.start_repository_fetch(&project_id, GitFetchOrigin::Automatic);
        }

        if should_notify {
            cx.notify();
        }
    }

    pub fn checkout_selected_repository_branch(
        &mut self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some((scope_root, selection)) = self
            .repository_graph_tabs
            .get(project_id)
            .map(|tab| (tab.scope_root.clone(), tab.selected_branch.clone()))
        else {
            return;
        };
        let Some(selection) = selection else {
            return;
        };
        if self.repository_toolbar_action_in_flight(&scope_root) {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        let started = match &selection {
            RepositoryBranchSelection::Local { name } => self
                .services
                .git
                .checkout_local_branch(&scope_root, name.clone()),
            RepositoryBranchSelection::Remote { full_ref } => self
                .services
                .git
                .checkout_remote_branch(&scope_root, full_ref.clone()),
        };
        if !started {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            tab.active_branch_action = Some(RepositoryBranchAction::Checkout {
                selection: selection.clone(),
            });
            tab.action_banner = None;
        }
        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.local_action_in_flight = true;
        }
        cx.notify();
    }

    pub fn create_repository_branch(
        &mut self,
        project_id: &str,
        source_branch_name: String,
        new_branch_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(scope_root) = self
            .repository_graph_tabs
            .get(project_id)
            .map(|tab| tab.scope_root.clone())
        else {
            return;
        };
        if self.repository_toolbar_action_in_flight(&scope_root) {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if !self.services.git.create_local_branch(
            &scope_root,
            source_branch_name.clone(),
            new_branch_name.clone(),
        ) {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            tab.active_branch_action = Some(RepositoryBranchAction::Create {
                source_branch_name,
                new_branch_name,
            });
            tab.action_banner = None;
        }
        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.local_action_in_flight = true;
        }
        cx.notify();
    }

    pub fn delete_repository_branch(
        &mut self,
        project_id: &str,
        branch_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(scope_root) = self
            .repository_graph_tabs
            .get(project_id)
            .map(|tab| tab.scope_root.clone())
        else {
            return;
        };
        if self.repository_toolbar_action_in_flight(&scope_root) {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if !self
            .services
            .git
            .delete_local_branch(&scope_root, branch_name.clone())
        {
            if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                tab.action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "Another git action is already running for this checkout.".to_string(),
                });
            }
            cx.notify();
            return;
        }

        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            tab.active_branch_action = Some(RepositoryBranchAction::Delete { branch_name });
            tab.action_banner = None;
        }
        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.local_action_in_flight = true;
        }
        cx.notify();
    }

    pub fn dismiss_repository_action_banner(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            if tab.action_banner.take().is_some() {
                cx.notify();
            }
        }
    }

    fn start_repository_fetch(&mut self, project_id: &str, origin: GitFetchOrigin) -> bool {
        let Some(scope_root) = self
            .repository_graph_tabs
            .get(project_id)
            .map(|tab| tab.scope_root.clone())
        else {
            return false;
        };

        if self.scope_git_action_in_flight(&scope_root) {
            if origin == GitFetchOrigin::Manual {
                if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                    tab.action_banner = Some(ActionBanner {
                        kind: ActionBannerKind::Warning,
                        message: "Another git action is already running for this checkout."
                            .to_string(),
                    });
                    return true;
                }
            }
            return false;
        }

        if !self.services.git.fetch_repo(&scope_root, origin) {
            if origin == GitFetchOrigin::Manual {
                if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
                    tab.action_banner = Some(ActionBanner {
                        kind: ActionBannerKind::Warning,
                        message: "Another git action is already running for this checkout."
                            .to_string(),
                    });
                    return true;
                }
            }
            return false;
        }

        if let Some(tab) = self.repository_graph_tabs.get_mut(project_id) {
            tab.fetch_in_flight = true;
            tab.active_fetch_origin = Some(origin);
            if origin == GitFetchOrigin::Manual {
                tab.action_banner = None;
            }
        }
        if origin == GitFetchOrigin::Manual {
            if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
                diff_tab.remote_op_in_flight = true;
            }
        }
        true
    }

    fn repository_auto_fetch_due(&self, project_id: &str, now: SystemTime) -> bool {
        let Some(tab) = self.repository_graph_tabs.get(project_id) else {
            return false;
        };
        if tab.fetch_in_flight || tab.active_branch_action.is_some() {
            return false;
        }
        if !self.repository_scope_may_have_remotes(tab) {
            return false;
        }
        if let Some(last_failure) = tab.last_automatic_fetch_failure_at {
            if now
                .duration_since(last_failure)
                .is_ok_and(|elapsed| elapsed < REPOSITORY_AUTO_FETCH_FAILURE_COOLDOWN)
            {
                return false;
            }
        }
        tab.last_remote_check_at.is_none_or(|last_check| {
            now.duration_since(last_check)
                .is_ok_and(|elapsed| elapsed >= REPOSITORY_AUTO_FETCH_INTERVAL)
        })
    }

    fn repository_scope_may_have_remotes(&self, tab: &RepositoryGraphTabState) -> bool {
        let graph_has_remote_signal = tab.graph.document.as_ref().is_some_and(|graph| {
            !graph.remote_branches.is_empty()
                || graph
                    .local_branches
                    .iter()
                    .any(|branch| branch.upstream.is_some())
        });

        match self.git_scopes.get(&tab.scope_root) {
            Some(snapshot) if !snapshot.remotes.is_empty() => true,
            Some(_) => graph_has_remote_signal,
            None => true,
        }
    }

    pub fn focus_auxiliary_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if self.focus_auxiliary_tab_internal(tab_id) {
            self.tick_repository_auto_fetch(cx);
            cx.notify();
        }
    }

    pub fn close_auxiliary_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if self.close_auxiliary_tab_internal(tab_id) {
            cx.notify();
        }
    }

    pub fn close_active_auxiliary_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_auxiliary_tab_id.clone() else {
            return;
        };
        if self.close_auxiliary_tab_internal(&tab_id) {
            cx.notify();
        }
    }

    pub fn update_diff_tree_width(
        &mut self,
        scope_root: &Path,
        width: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };

        let clamped = width.max(180.0);
        if (diff_tab.tree_width - clamped).abs() > f32::EPSILON {
            diff_tab.tree_width = clamped;
            cx.notify();
        }
    }

    pub fn select_diff_file(
        &mut self,
        scope_root: &Path,
        selection: DiffSelectionKey,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        let Some(index_document) = diff_tab.index.document.as_ref() else {
            return;
        };
        // Validate the selection exists in the correct section.
        let files = match selection.section {
            DiffSectionKind::Conflicted => &index_document.conflicted_files,
            DiffSectionKind::Staged => &index_document.staged_files,
            DiffSectionKind::Unstaged => &index_document.unstaged_files,
        };
        if !files
            .iter()
            .any(|file| file.relative_path == selection.relative_path)
        {
            return;
        }

        if diff_tab.selected_file.as_ref() == Some(&selection)
            && diff_tab.file.document.as_ref().is_some_and(|document| {
                document.selection == selection
                    && document.generation == index_document.snapshot.generation
            })
        {
            return;
        }

        diff_tab.selected_file = Some(selection.clone());
        self.request_selected_file_diff(scope_root, selection);
        cx.notify();
    }

    fn reload_conflict_document(
        &mut self,
        scope_root: &Path,
        generation: u64,
        selection: &DiffSelectionKey,
        preserve_editor_state: bool,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        let Some(index_document) = diff_tab.index.document.as_ref() else {
            return;
        };
        let Some(file) = index_document
            .conflicted_files
            .iter()
            .find(|file| file.relative_path == selection.relative_path)
            .cloned()
        else {
            diff_tab
                .conflict_editor
                .documents
                .remove(&selection.relative_path);
            return;
        };

        let prior_document = diff_tab
            .conflict_editor
            .documents
            .get(&selection.relative_path)
            .and_then(|state| match state {
                ConflictDocumentState::Loaded(document) => Some(document.clone()),
                ConflictDocumentState::Unavailable(_) => None,
            })
            .filter(|_| preserve_editor_state);

        let next_state = if file.is_binary {
            ConflictDocumentState::Unavailable(ConflictUnavailableState {
                generation,
                selection: selection.clone(),
                file,
                message: "Binary conflicted files cannot be edited in OrcaShell.".to_string(),
            })
        } else {
            let file_path = scope_root.join(&selection.relative_path);
            match fs::read(&file_path) {
                Ok(bytes) if bytes.len() > MAX_RENDERED_DIFF_BYTES => {
                    ConflictDocumentState::Unavailable(ConflictUnavailableState {
                        generation,
                        selection: selection.clone(),
                        file,
                        message: "This conflicted file is too large to edit in OrcaShell."
                            .to_string(),
                    })
                }
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) if count_text_lines(&text) > MAX_RENDERED_DIFF_LINES => {
                        ConflictDocumentState::Unavailable(ConflictUnavailableState {
                            generation,
                            selection: selection.clone(),
                            file,
                            message: "This conflicted file is too large to edit in OrcaShell."
                                .to_string(),
                        })
                    }
                    Ok(text) => ConflictDocumentState::Loaded(merge_conflict_document_from_text(
                        generation,
                        selection.clone(),
                        file,
                        text,
                        prior_document.as_ref(),
                        false,
                    )),
                    Err(_) => ConflictDocumentState::Unavailable(ConflictUnavailableState {
                        generation,
                        selection: selection.clone(),
                        file,
                        message: "This conflicted file is not UTF-8 text.".to_string(),
                    }),
                },
                Err(error) => ConflictDocumentState::Unavailable(ConflictUnavailableState {
                    generation,
                    selection: selection.clone(),
                    file,
                    message: unreadable_conflict_message(&error),
                }),
            }
        };

        diff_tab
            .conflict_editor
            .documents
            .insert(selection.relative_path.clone(), next_state);
    }

    fn validate_conflict_path_for_resolution(
        &self,
        scope_root: &Path,
        selection: &DiffSelectionKey,
    ) -> Result<(), String> {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return Err("The diff tab is no longer available.".to_string());
        };
        let Some(index_document) = diff_tab.index.document.as_ref() else {
            return Err("The conflicted file list is unavailable.".to_string());
        };
        let Some(file) = index_document
            .conflicted_files
            .iter()
            .find(|file| file.relative_path == selection.relative_path)
        else {
            return Err(format!(
                "{} is no longer conflicted.",
                selection.relative_path.display()
            ));
        };

        if let Some(state) = diff_tab
            .conflict_editor
            .documents
            .get(&selection.relative_path)
        {
            match state {
                ConflictDocumentState::Loaded(document) => {
                    if document.is_dirty {
                        return Err(format!(
                            "Save {} before marking it resolved.",
                            selection.relative_path.display()
                        ));
                    }
                    if document.parse_error.is_some() {
                        return Err(format!(
                            "{} still contains malformed conflict markers.",
                            selection.relative_path.display()
                        ));
                    }
                    if !document.blocks.is_empty() {
                        return Err(format!(
                            "{} still contains conflict markers.",
                            selection.relative_path.display()
                        ));
                    }
                    return Ok(());
                }
                ConflictDocumentState::Unavailable(unavailable) => {
                    return Err(format!(
                        "{} cannot be marked resolved from OrcaShell: {}",
                        selection.relative_path.display(),
                        unavailable.message
                    ));
                }
            }
        }

        if file.is_binary {
            return Err(format!(
                "{} cannot be marked resolved from OrcaShell because it is binary.",
                selection.relative_path.display()
            ));
        }

        let file_path = scope_root.join(&selection.relative_path);
        let bytes = fs::read(&file_path).map_err(|error| unreadable_conflict_message(&error))?;
        if bytes.len() > MAX_RENDERED_DIFF_BYTES {
            return Err(format!(
                "{} is too large to validate in OrcaShell.",
                selection.relative_path.display()
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not UTF-8 text.", selection.relative_path.display()))?;
        if count_text_lines(&text) > MAX_RENDERED_DIFF_LINES {
            return Err(format!(
                "{} is too large to validate in OrcaShell.",
                selection.relative_path.display()
            ));
        }
        let parsed = parse_conflict_file_text(&text).map_err(|_| {
            format!(
                "{} still contains malformed conflict markers.",
                selection.relative_path.display()
            )
        })?;
        if !parsed.blocks.is_empty() {
            return Err(format!(
                "{} still contains conflict markers.",
                selection.relative_path.display()
            ));
        }

        Ok(())
    }

    fn conflict_resolution_targets(&self, scope_root: &Path) -> Vec<DiffSelectionKey> {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return Vec::new();
        };

        let mut selections: Vec<DiffSelectionKey> = diff_tab
            .multi_select
            .iter()
            .filter(|selection| selection.section == DiffSectionKind::Conflicted)
            .cloned()
            .collect();
        if !selections.is_empty() {
            selections.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            return selections;
        }

        diff_tab
            .selected_file
            .as_ref()
            .filter(|selection| selection.section == DiffSectionKind::Conflicted)
            .cloned()
            .into_iter()
            .collect()
    }

    pub(crate) fn can_mark_conflicts_resolved(&self, scope_root: &Path) -> bool {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return false;
        };
        if Self::any_action_in_flight(diff_tab) {
            return false;
        }
        if diff_tab.index.document.as_ref().is_some_and(|document| {
            document.merge_state.is_none() && document.repo_state_warning.is_some()
        }) {
            return false;
        }

        let selections = self.conflict_resolution_targets(scope_root);
        !selections.is_empty()
            && selections.iter().all(|selection| {
                self.validate_conflict_path_for_resolution(scope_root, selection)
                    .is_ok()
            })
    }

    // ── Diff tab action dispatch ───────────────────────────────────────

    fn any_action_in_flight(tab: &DiffTabState) -> bool {
        tab.local_action_in_flight || tab.remote_op_in_flight
    }

    fn is_unsupported_discard_status(status: GitFileStatus) -> bool {
        matches!(status, GitFileStatus::Renamed | GitFileStatus::Typechange)
    }

    pub fn stage_all(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let paths: Vec<PathBuf> = diff_tab
            .index
            .document
            .as_ref()
            .map(|d| {
                d.unstaged_files
                    .iter()
                    .map(|f| f.relative_path.clone())
                    .collect()
            })
            .unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.stage_paths(scope_root, paths);
        cx.notify();
    }

    pub fn stage_selected(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let paths: Vec<PathBuf> = diff_tab
            .multi_select
            .iter()
            .filter(|k| matches!(k.section, DiffSectionKind::Unstaged))
            .map(|k| k.relative_path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.stage_paths(scope_root, paths);
        cx.notify();
    }

    pub fn unstage_all(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let paths: Vec<PathBuf> = diff_tab
            .index
            .document
            .as_ref()
            .map(|d| {
                d.staged_files
                    .iter()
                    .map(|f| f.relative_path.clone())
                    .collect()
            })
            .unwrap_or_default();
        if paths.is_empty() {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.unstage_paths(scope_root, paths);
        cx.notify();
    }

    pub fn unstage_selected(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let paths: Vec<PathBuf> = diff_tab
            .multi_select
            .iter()
            .filter(|k| k.section == DiffSectionKind::Staged)
            .map(|k| k.relative_path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.unstage_paths(scope_root, paths);
        cx.notify();
    }

    pub fn discard_all_unstaged(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let outcome = {
            let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
                return;
            };
            if Self::any_action_in_flight(diff_tab) {
                return;
            }
            let Some(index_document) = diff_tab.index.document.as_ref() else {
                return;
            };
            if index_document.merge_state.is_some() {
                Err("Discard actions are unavailable while a merge is active.".to_string())
            } else if let Some(warning) = index_document.repo_state_warning.as_ref() {
                Err(warning.clone())
            } else if index_document.unstaged_files.is_empty() {
                Ok(false)
            } else {
                Ok(true)
            }
        };

        match outcome {
            Ok(true) => {
                if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                    diff_tab.local_action_in_flight = true;
                }
                self.services.git.discard_all_unstaged(scope_root);
                cx.notify();
            }
            Ok(false) => {}
            Err(message) => self.show_diff_action_warning(scope_root, message, cx),
        }
    }

    pub fn discard_selected_file(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let outcome = self.validate_discard_selected_file(scope_root);
        match outcome {
            Ok(relative_path) => {
                if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                    diff_tab.local_action_in_flight = true;
                }
                self.services
                    .git
                    .discard_unstaged_file(scope_root, relative_path);
                cx.notify();
            }
            Err(message) => self.show_diff_action_warning(scope_root, message, cx),
        }
    }

    pub fn discard_selected_hunk(
        &mut self,
        scope_root: &Path,
        hunk_index: usize,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.validate_discard_selected_hunk(scope_root, hunk_index);
        match outcome {
            Ok((relative_path, target)) => {
                if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                    diff_tab.local_action_in_flight = true;
                }
                self.services
                    .git
                    .discard_unstaged_hunk(scope_root, relative_path, target);
                cx.notify();
            }
            Err(message) => self.show_diff_action_warning(scope_root, message, cx),
        }
    }

    pub fn commit_staged(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let message = diff_tab.commit_message.trim().to_string();
        if message.is_empty() {
            return;
        }
        if diff_tab
            .index
            .document
            .as_ref()
            .is_none_or(|d| d.staged_files.is_empty())
        {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.commit_staged(scope_root, message);
        cx.notify();
    }

    pub fn enter_stash_mode(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.view_mode = DiffTabViewMode::Stashes;
        cx.notify();
        self.request_stash_list(scope_root);
    }

    pub fn exit_stash_mode(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.view_mode = DiffTabViewMode::WorkingTree;
        cx.notify();
    }

    pub fn select_stash(&mut self, scope_root: &Path, stash_oid: Oid, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.view_mode = DiffTabViewMode::Stashes;
        diff_tab.stash.expanded_stash = Some(stash_oid);
        cx.notify();
        self.request_stash_detail(scope_root, stash_oid);
    }

    pub fn select_stash_file(
        &mut self,
        scope_root: &Path,
        selection: StashFileSelection,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.view_mode = DiffTabViewMode::Stashes;
        diff_tab.stash.expanded_stash = Some(selection.stash_oid);
        diff_tab.stash.selected_stash = Some(selection.stash_oid);
        cx.notify();
        self.request_stash_file_diff(scope_root, selection);
    }

    pub fn create_stash(
        &mut self,
        scope_root: &Path,
        message: Option<String>,
        keep_index: bool,
        include_untracked: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services
            .git
            .create_stash(scope_root, message, keep_index, include_untracked);
        cx.notify();
    }

    pub fn apply_selected_stash(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(stash_oid) = self.selected_stash_oid(scope_root) else {
            return;
        };
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.apply_stash(scope_root, stash_oid);
        cx.notify();
    }

    pub fn pop_selected_stash(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(stash_oid) = self.selected_stash_oid(scope_root) else {
            return;
        };
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.pop_stash(scope_root, stash_oid);
        cx.notify();
    }

    pub fn drop_selected_stash(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(stash_oid) = self.selected_stash_oid(scope_root) else {
            return;
        };
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.drop_stash(scope_root, stash_oid);
        cx.notify();
    }

    pub fn dispatch_pull(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.remote_op_in_flight = true;
        self.services.git.pull_current_branch(scope_root);
        cx.notify();
    }

    pub fn dispatch_push(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.remote_op_in_flight = true;
        self.services.git.push_current_branch(scope_root);
        cx.notify();
    }

    pub fn dispatch_publish(
        &mut self,
        scope_root: &Path,
        remote_name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.remote_op_in_flight = true;
        self.services
            .git
            .publish_current_branch(scope_root, remote_name);
        cx.notify();
    }

    pub fn dispatch_merge_back(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab) {
            return;
        }
        let Some(managed) = &diff_tab.managed_worktree else {
            return;
        };
        let source_ref = managed.source_ref.clone();
        let diff_tab = self.diff_tabs.get_mut(scope_root).unwrap();
        diff_tab.local_action_in_flight = true;
        self.services
            .git
            .merge_managed_branch(scope_root, source_ref);
        cx.notify();
    }

    pub fn dispatch_complete_merge(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab)
            || diff_tab
                .index
                .document
                .as_ref()
                .and_then(|document| document.merge_state.as_ref())
                .is_none_or(|merge_state| !merge_state.can_complete)
        {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.complete_merge(scope_root);
        cx.notify();
    }

    pub fn dispatch_abort_merge(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if Self::any_action_in_flight(diff_tab)
            || diff_tab
                .index
                .document
                .as_ref()
                .and_then(|document| document.merge_state.as_ref())
                .is_none_or(|merge_state| !merge_state.can_abort)
        {
            return;
        }
        diff_tab.local_action_in_flight = true;
        self.services.git.abort_merge(scope_root);
        cx.notify();
    }

    pub fn save_conflict_document(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(selection) = self
            .diff_tabs
            .get(scope_root)
            .and_then(|tab| tab.selected_file.clone())
        else {
            return;
        };
        if selection.section != DiffSectionKind::Conflicted {
            return;
        }

        let (current_generation, state) = {
            let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
                return;
            };
            if Self::any_action_in_flight(diff_tab) {
                return;
            }
            (
                diff_tab
                    .index
                    .document
                    .as_ref()
                    .map(|document| document.snapshot.generation),
                diff_tab
                    .conflict_editor
                    .documents
                    .get(&selection.relative_path)
                    .cloned(),
            )
        };

        let Some(ConflictDocumentState::Loaded(document)) = state else {
            if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                diff_tab.last_action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: "This conflicted file cannot be edited in OrcaShell.".to_string(),
                });
                cx.notify();
            }
            return;
        };

        let file_path = scope_root.join(&selection.relative_path);
        if let Err(error) = fs::write(&file_path, document.raw_text.as_bytes()) {
            if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                diff_tab.last_action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Error,
                    message: format!(
                        "Could not save {}: {error}",
                        selection.relative_path.display()
                    ),
                });
                cx.notify();
            }
            return;
        }

        self.reload_conflict_document(
            scope_root,
            current_generation.unwrap_or(document.generation),
            &selection,
            true,
        );
        self.services.git.request_snapshot(scope_root, None);

        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            let banner = match diff_tab
                .conflict_editor
                .documents
                .get(&selection.relative_path)
            {
                Some(ConflictDocumentState::Loaded(saved)) if saved.parse_error.is_some() => {
                    ActionBanner {
                        kind: ActionBannerKind::Warning,
                        message: format!(
                            "Saved {}, but the conflict markers are malformed.",
                            selection.relative_path.display()
                        ),
                    }
                }
                Some(ConflictDocumentState::Loaded(saved)) if saved.blocks.is_empty() => {
                    ActionBanner {
                        kind: ActionBannerKind::Success,
                        message: format!(
                            "Saved {}. It can now be marked resolved.",
                            selection.relative_path.display()
                        ),
                    }
                }
                Some(ConflictDocumentState::Loaded(_)) => ActionBanner {
                    kind: ActionBannerKind::Success,
                    message: format!("Saved {}.", selection.relative_path.display()),
                },
                Some(ConflictDocumentState::Unavailable(unavailable)) => ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: unavailable.message.clone(),
                },
                None => ActionBanner {
                    kind: ActionBannerKind::Success,
                    message: format!("Saved {}.", selection.relative_path.display()),
                },
            };
            diff_tab.last_action_banner = Some(banner);
            cx.notify();
        }
    }

    pub fn reset_conflict_document(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(selection) = self
            .diff_tabs
            .get(scope_root)
            .and_then(|tab| tab.selected_file.clone())
        else {
            return;
        };
        if selection.section != DiffSectionKind::Conflicted {
            return;
        }

        let (current_generation, state) = {
            let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
                return;
            };
            if Self::any_action_in_flight(diff_tab) {
                return;
            }
            (
                diff_tab
                    .index
                    .document
                    .as_ref()
                    .map(|document| document.snapshot.generation),
                diff_tab
                    .conflict_editor
                    .documents
                    .get(&selection.relative_path)
                    .cloned(),
            )
        };

        let Some(ConflictDocumentState::Loaded(document)) = state else {
            return;
        };
        if document.raw_text == document.initial_raw_text {
            return;
        }

        let file_path = scope_root.join(&selection.relative_path);
        if let Err(error) = fs::write(&file_path, document.initial_raw_text.as_bytes()) {
            if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                diff_tab.last_action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Error,
                    message: format!(
                        "Could not reset {}: {error}",
                        selection.relative_path.display()
                    ),
                });
                cx.notify();
            }
            return;
        }

        self.reload_conflict_document(
            scope_root,
            current_generation.unwrap_or(document.generation),
            &selection,
            false,
        );
        self.services.git.request_snapshot(scope_root, None);

        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.last_action_banner = Some(ActionBanner {
                kind: ActionBannerKind::Success,
                message: format!(
                    "Reset {} to its original conflict state.",
                    selection.relative_path.display()
                ),
            });
            cx.notify();
        }
    }

    pub fn mark_conflicts_resolved(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let selections: Vec<DiffSelectionKey> = {
            let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
                return;
            };
            if Self::any_action_in_flight(diff_tab) {
                return;
            }
            if diff_tab.index.document.as_ref().is_some_and(|document| {
                document.merge_state.is_none() && document.repo_state_warning.is_some()
            }) {
                return;
            }
            self.conflict_resolution_targets(scope_root)
        };
        if selections.is_empty() {
            return;
        }

        for selection in &selections {
            if let Err(message) = self.validate_conflict_path_for_resolution(scope_root, selection)
            {
                if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
                    diff_tab.last_action_banner = Some(ActionBanner {
                        kind: ActionBannerKind::Warning,
                        message,
                    });
                    cx.notify();
                }
                return;
            }
        }

        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.local_action_in_flight = true;
        }
        self.services.git.stage_paths(
            scope_root,
            selections
                .into_iter()
                .map(|selection| selection.relative_path)
                .collect(),
        );
        cx.notify();
    }

    pub fn show_diff_action_warning(
        &mut self,
        scope_root: &Path,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.last_action_banner = Some(ActionBanner {
            kind: ActionBannerKind::Warning,
            message,
        });
        cx.notify();
    }

    fn validate_discard_selected_file(&self, scope_root: &Path) -> Result<PathBuf, String> {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return Err("Diff tab is no longer available.".to_string());
        };
        if Self::any_action_in_flight(diff_tab) {
            return Err("Another git action is already in progress.".to_string());
        }
        let Some(index_document) = diff_tab.index.document.as_ref() else {
            return Err("The diff index is not loaded yet.".to_string());
        };
        if index_document.merge_state.is_some() {
            return Err("Discard actions are unavailable while a merge is active.".to_string());
        }
        if let Some(warning) = index_document.repo_state_warning.as_ref() {
            return Err(warning.clone());
        }
        let Some(selection) = diff_tab.selected_file.as_ref() else {
            return Err("No file is selected.".to_string());
        };
        if selection.section != DiffSectionKind::Unstaged {
            return Err("Discard actions only apply to unstaged file diffs.".to_string());
        }
        let Some(file_document) = diff_tab.file.document.as_ref() else {
            return Err("The selected file diff is not ready yet.".to_string());
        };
        if file_document.selection != *selection {
            return Err("The selected file diff is being refreshed.".to_string());
        }
        let latest_generation = self
            .git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.generation);
        if latest_generation.is_some_and(|generation| file_document.generation < generation) {
            return Err(
                "Displayed diff is stale. Wait for refresh to finish and try again.".to_string(),
            );
        }
        if Self::is_unsupported_discard_status(file_document.file.status) {
            return Err(
                "Discard File is unavailable for renamed or typechanged files.".to_string(),
            );
        }
        Ok(selection.relative_path.clone())
    }

    fn validate_discard_selected_hunk(
        &self,
        scope_root: &Path,
        hunk_index: usize,
    ) -> Result<(PathBuf, DiscardHunkTarget), String> {
        let relative_path = self.validate_discard_selected_file(scope_root)?;
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return Err("Diff tab is no longer available.".to_string());
        };
        let Some(file_document) = diff_tab.file.document.as_ref() else {
            return Err("The selected file diff is not ready yet.".to_string());
        };
        if Self::is_unsupported_discard_status(file_document.file.status) {
            return Err(
                "Discard Hunk is unavailable for renamed or typechanged files.".to_string(),
            );
        }
        let Some(target) = file_document.discard_hunk_target(hunk_index) else {
            return Err(format!(
                "Hunk {} is no longer available in {}.",
                hunk_index,
                relative_path.display()
            ));
        };
        Ok((relative_path, target))
    }

    pub fn begin_remove_worktree_confirm(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.managed_worktree.is_none() || Self::any_action_in_flight(diff_tab) {
            return;
        }
        diff_tab.remove_worktree_confirm = Some(RemoveWorktreeConfirm {
            delete_branch: false,
        });
        cx.notify();
    }

    pub fn toggle_remove_delete_branch(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            if let Some(confirm) = &mut diff_tab.remove_worktree_confirm {
                confirm.delete_branch = !confirm.delete_branch;
                cx.notify();
            }
        }
    }

    pub fn cancel_remove_worktree(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.remove_worktree_confirm = None;
            cx.notify();
        }
    }

    pub fn confirm_remove_worktree(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return;
        };
        // Guard: must have an active confirmation state.
        let Some(confirm) = &diff_tab.remove_worktree_confirm else {
            return;
        };

        // 1. Capture everything we need before mutating state.
        let delete_branch = confirm.delete_branch;

        // 2. Collect terminal IDs to close: all terminals whose git scope equals this worktree.
        let terminal_ids: Vec<String> = self
            .terminal_git_scopes
            .iter()
            .filter(|(_, scope)| scope.as_path() == scope_root)
            .map(|(tid, _)| tid.clone())
            .collect();

        // 3. Close terminals (may invalidate some workspace state).
        self.close_terminals_by_id_internal(&terminal_ids);

        // 4. Now update diff tab state and dispatch.
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.remove_worktree_confirm = None;
            diff_tab.local_action_in_flight = true;
        }

        self.services
            .git
            .remove_managed_worktree_action(scope_root, delete_branch);
        cx.notify();
    }

    pub fn update_commit_message(
        &mut self,
        scope_root: &Path,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.commit_message = message;
            cx.notify();
        }
    }

    pub fn dismiss_action_banner(&mut self, scope_root: &Path, cx: &mut Context<Self>) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) {
            diff_tab.last_action_banner = None;
            cx.notify();
        }
    }

    fn selected_stash_oid(&self, scope_root: &Path) -> Option<Oid> {
        self.diff_tabs.get(scope_root)?.stash.selected_stash
    }

    // ── Diff tab multi-select ───────────────────────────────────────────

    pub fn diff_toggle_multi_select(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
        cx: &mut Context<Self>,
    ) {
        self.diff_toggle_multi_select_internal(scope_root, key);
        cx.notify();
    }

    pub(crate) fn diff_toggle_multi_select_internal(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.multi_select.contains(&key) {
            diff_tab.multi_select.remove(&key);
        } else {
            // Section-local: clear keys from other section
            diff_tab.multi_select.retain(|k| k.section == key.section);
            diff_tab.multi_select.insert(key.clone());
        }
        diff_tab.selection_anchor = Some(key);
    }

    pub fn diff_range_select(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
        visible_order: &[DiffSelectionKey],
        cx: &mut Context<Self>,
    ) {
        self.diff_range_select_internal(scope_root, key, visible_order);
        cx.notify();
    }

    pub(crate) fn diff_range_select_internal(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
        visible_order: &[DiffSelectionKey],
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        let anchor = diff_tab
            .selection_anchor
            .clone()
            .unwrap_or_else(|| key.clone());
        // Cross-section: fall back to replace
        if anchor.section != key.section {
            diff_tab.multi_select.clear();
            diff_tab.multi_select.insert(key.clone());
            diff_tab.selection_anchor = Some(key);
            return;
        }
        let section_order: Vec<&DiffSelectionKey> = visible_order
            .iter()
            .filter(|k| k.section == key.section)
            .collect();
        let anchor_pos = section_order.iter().position(|k| **k == anchor);
        let target_pos = section_order.iter().position(|k| **k == key);
        if let (Some(a), Some(t)) = (anchor_pos, target_pos) {
            let (lo, hi) = if a <= t { (a, t) } else { (t, a) };
            diff_tab.multi_select.clear();
            for k in &section_order[lo..=hi] {
                diff_tab.multi_select.insert((*k).clone());
            }
        }
    }

    pub fn diff_replace_select(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
        cx: &mut Context<Self>,
    ) {
        self.diff_replace_select_internal(scope_root, key);
        cx.notify();
    }

    pub(crate) fn diff_replace_select_internal(
        &mut self,
        scope_root: &Path,
        key: DiffSelectionKey,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.multi_select.clear();
        diff_tab.multi_select.insert(key.clone());
        diff_tab.selection_anchor = Some(key);
    }

    pub fn create_project_worktree(&mut self, project_id: &str, _cx: &mut Context<Self>) {
        let Some(project) = self.project(project_id) else {
            return;
        };
        self.services
            .git
            .create_managed_worktree(project_id, &project.path, None);
    }

    fn open_or_focus_live_diff_stream_tab_internal(&mut self, project_id: &str) -> bool {
        let Some(project_name) = self.project(project_id).map(|project| project.name.clone())
        else {
            return false;
        };

        let tab_id = Self::live_diff_stream_tab_id(project_id);
        let title = Self::live_diff_stream_title(&project_name);
        if let Some(tab) = self.auxiliary_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = title;
        } else {
            self.auxiliary_tabs.push(AuxiliaryTabState {
                id: tab_id.clone(),
                title,
                kind: AuxiliaryTabKind::LiveDiffStream {
                    project_id: project_id.to_string(),
                },
            });
        }

        self.ensure_live_diff_feed_state(project_id);
        self.active_auxiliary_tab_id = Some(tab_id);
        true
    }

    fn open_or_focus_repository_graph_tab_internal(&mut self, project_id: &str) -> bool {
        let Some(project_name) = self.project(project_id).map(|project| project.name.clone())
        else {
            return false;
        };

        let tab_id = Self::repository_graph_tab_id(project_id);
        let title = Self::repository_graph_title(&project_name);
        if let Some(tab) = self.auxiliary_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.title = title;
        } else {
            self.auxiliary_tabs.push(AuxiliaryTabState {
                id: tab_id.clone(),
                title,
                kind: AuxiliaryTabKind::RepositoryGraph {
                    project_id: project_id.to_string(),
                },
            });
        }

        self.ensure_repository_graph_state(project_id);
        self.active_auxiliary_tab_id = Some(tab_id);
        true
    }

    fn open_or_focus_diff_tab_internal(
        &mut self,
        scope_root: PathBuf,
        branch_name: String,
    ) -> bool {
        let tab_id = Self::diff_tab_id(&scope_root);
        if !self.auxiliary_tabs.iter().any(|tab| tab.id == tab_id) {
            self.auxiliary_tabs.push(AuxiliaryTabState {
                id: tab_id.clone(),
                title: format!("Diff: {branch_name}"),
                kind: AuxiliaryTabKind::Diff {
                    scope_root: scope_root.clone(),
                },
            });
        } else {
            self.update_diff_tab_title(&scope_root, &branch_name);
        }

        let diff_tab = self
            .diff_tabs
            .entry(scope_root.clone())
            .or_insert_with(|| DiffTabState::new(scope_root.clone()));

        // Populate managed worktree info from store if not already set.
        if diff_tab.managed_worktree.is_none() {
            if let Some(stored) = self
                .services
                .store
                .lock()
                .as_ref()
                .and_then(|store| store.find_worktree_by_path(&scope_root).ok().flatten())
            {
                diff_tab.managed_worktree = Some(ManagedWorktreeSummary {
                    id: stored.id,
                    branch_name: stored.branch_name,
                    source_ref: stored.source_ref,
                });
            }
        }

        self.active_auxiliary_tab_id = Some(tab_id);
        self.services.git.request_snapshot(&scope_root, None);
        self.request_diff_index_refresh(&scope_root);
        self.request_stash_list(&scope_root);
        true
    }

    pub fn create_terminal_worktree(
        &mut self,
        project_id: &str,
        terminal_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.terminal_working_directory(project_id, terminal_id, cx) else {
            self.set_workspace_error(
                "Could not resolve a working directory for this terminal.",
                cx,
            );
            return;
        };

        self.services
            .git
            .create_managed_worktree(project_id, &cwd, Some(terminal_id));
    }

    pub fn handle_git_event(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        match event {
            GitEvent::SnapshotUpdated {
                terminal_ids,
                request_path,
                scope_root,
                result,
                ..
            } => {
                self.apply_snapshot_update(terminal_ids, request_path, scope_root, result);
                cx.notify();
            }
            GitEvent::ManagedWorktreeCreated {
                project_id, result, ..
            } => {
                if self.project(&project_id).is_none() {
                    return;
                }
                match result {
                    Ok(worktree) => {
                        match self.open_terminal_tab_in_project(
                            &project_id,
                            worktree.path.clone(),
                            cx,
                        ) {
                            Ok(terminal_id) => {
                                if let Err(error) =
                                    self.persist_worktree(&project_id, &worktree, Some(terminal_id))
                                {
                                    self.set_workspace_error(
                                        format!(
                                            "Worktree opened, but SQLite persistence failed: {error}"
                                        ),
                                        cx,
                                    );
                                } else {
                                    self.workspace_banner = None;
                                    cx.notify();
                                }
                            }
                            Err(error) => {
                                self.services.git.rollback_managed_worktree(
                                    &project_id,
                                    worktree,
                                    error,
                                );
                            }
                        }
                    }
                    Err(message) => {
                        self.set_workspace_error(message, cx);
                    }
                }
            }
            GitEvent::ManagedWorktreeRollbackComplete {
                project_id,
                worktree,
                original_error,
                result,
                ..
            } => {
                if self.project(&project_id).is_none() {
                    return;
                }
                match result {
                    Ok(()) => {
                        self.set_workspace_error(original_error, cx);
                    }
                    Err(message) => {
                        self.set_workspace_error(
                            format!(
                                "{original_error} Rollback also failed for branch {} at {}: {message}",
                                worktree.branch_name,
                                worktree.path.display()
                            ),
                            cx,
                        );
                    }
                }
            }
            GitEvent::DiffIndexLoaded {
                scope_root,
                generation,
                result,
            } => {
                self.apply_diff_index_update(scope_root, generation, result);
                cx.notify();
            }
            GitEvent::FileDiffLoaded {
                scope_root,
                generation,
                selection,
                result,
            } => {
                self.apply_file_diff_update(scope_root, generation, selection, result);
                cx.notify();
            }
            GitEvent::RepositoryGraphLoaded {
                scope_root,
                request_revision,
                result,
            } => {
                self.apply_repository_graph_update(scope_root, request_revision, result);
                cx.notify();
            }
            GitEvent::CommitDetailLoaded {
                scope_root,
                oid,
                request_revision,
                result,
            } => {
                self.apply_repository_commit_detail_update(
                    scope_root,
                    oid,
                    request_revision,
                    result,
                );
                cx.notify();
            }
            GitEvent::CommitFileDiffLoaded {
                scope_root,
                selection,
                request_revision,
                result,
            } => {
                self.apply_repository_commit_file_diff_update(
                    scope_root,
                    selection,
                    request_revision,
                    result,
                );
                cx.notify();
            }
            GitEvent::StashListLoaded {
                scope_root,
                request_revision,
                result,
            } => {
                self.apply_stash_list_update(scope_root, request_revision, result);
                cx.notify();
            }
            GitEvent::StashDetailLoaded {
                scope_root,
                stash_oid,
                request_revision,
                result,
            } => {
                self.apply_stash_detail_update(scope_root, stash_oid, request_revision, result);
                cx.notify();
            }
            GitEvent::StashFileDiffLoaded {
                scope_root,
                selection,
                request_revision,
                result,
            } => {
                self.apply_stash_file_diff_update(scope_root, selection, request_revision, result);
                cx.notify();
            }
            GitEvent::FeedCaptureCompleted {
                project_id,
                scope_root,
                generation,
                request_revision,
                result,
            } => {
                self.apply_live_feed_capture_update(
                    &project_id,
                    scope_root,
                    generation,
                    request_revision,
                    result,
                );
                cx.notify();
            }
            GitEvent::LocalActionCompleted {
                scope_root,
                action,
                result,
            } => {
                self.apply_local_action_completion(scope_root, action, result);
                cx.notify();
            }
            GitEvent::MergeConflictEntered {
                request_scope,
                affected_scope,
                conflicted_files,
                trigger,
            } => {
                self.handle_merge_conflict_entered(
                    request_scope,
                    affected_scope,
                    conflicted_files,
                    trigger,
                );
                cx.notify();
            }
            GitEvent::RemoteOpCompleted {
                scope_root,
                kind,
                fetch_origin,
                refresh_graph,
                result,
            } => {
                self.apply_remote_op_completion(
                    scope_root,
                    kind,
                    fetch_origin,
                    refresh_graph,
                    result,
                );
                cx.notify();
            }
        }
    }

    fn handle_merge_conflict_entered(
        &mut self,
        request_scope: PathBuf,
        affected_scope: PathBuf,
        conflicted_files: Vec<PathBuf>,
        trigger: MergeConflictTrigger,
    ) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(&affected_scope) {
            diff_tab.view_mode = DiffTabViewMode::WorkingTree;
        }
        let preferred_file =
            conflicted_files
                .first()
                .cloned()
                .map(|relative_path| DiffSelectionKey {
                    section: DiffSectionKind::Conflicted,
                    relative_path,
                });

        self.open_diff_tab_for_scope_and_file_optimistic(&affected_scope, preferred_file);
        self.services.git.request_snapshot(&affected_scope, None);
        self.request_diff_index_refresh(&affected_scope);

        if request_scope != affected_scope {
            if let Some(diff_tab) = self.diff_tabs.get_mut(&request_scope) {
                let message = match trigger {
                    MergeConflictTrigger::Pull => {
                        "Pull conflicts opened in the affected diff tab.".to_string()
                    }
                    MergeConflictTrigger::MergeBack => {
                        "Merge conflicts must be resolved in the source scope diff tab.".to_string()
                    }
                    MergeConflictTrigger::StashApply => {
                        "Stash apply conflicts opened in the affected diff tab.".to_string()
                    }
                    MergeConflictTrigger::StashPop => {
                        "Stash pop conflicts opened in the affected diff tab.".to_string()
                    }
                };
                diff_tab.last_action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message,
                });
            }
        }
    }

    fn apply_snapshot_update(
        &mut self,
        terminal_ids: Vec<String>,
        request_path: PathBuf,
        scope_root: Option<PathBuf>,
        result: Result<GitSnapshotSummary, SnapshotLoadError>,
    ) {
        match result {
            Ok(snapshot) => {
                let scope_root = scope_root.unwrap_or_else(|| snapshot.scope_root.clone());
                self.update_diff_tab_title(&scope_root, &snapshot.branch_name);
                self.git_scopes.insert(scope_root.clone(), snapshot.clone());
                for terminal_id in terminal_ids {
                    if self.terminal_exists(&terminal_id) {
                        self.attach_terminal_scope(&terminal_id, scope_root.clone());
                    }
                }
                self.refresh_diff_tab_if_stale(&scope_root, snapshot.generation);
                self.refresh_live_feeds_for_scope_if_stale(&scope_root, snapshot.generation);
            }
            Err(error) => {
                let is_scope_refresh = terminal_ids.is_empty();
                let scope_refresh_root = if is_scope_refresh {
                    scope_root.clone().or_else(|| Some(request_path.clone()))
                } else {
                    None
                };

                match error.kind() {
                    SnapshotLoadErrorKind::Unavailable => {
                        if let Some(scope_root) = scope_refresh_root {
                            let message = error.message().to_string();
                            self.mark_diff_scope_unavailable(&scope_root, message.clone());
                            self.record_live_feed_scope_error(&scope_root, message);
                        }
                    }
                    SnapshotLoadErrorKind::NotRepository if is_scope_refresh => {
                        if let Some(scope_root) = scope_refresh_root {
                            let message = error.message().to_string();
                            self.mark_diff_scope_unavailable(&scope_root, message.clone());
                            self.record_live_feed_scope_error(&scope_root, message);
                            if scope_root == request_path {
                                self.detach_scope(&scope_root);
                            }
                        }
                    }
                    SnapshotLoadErrorKind::NotRepository => {
                        for terminal_id in terminal_ids {
                            self.detach_terminal_scope(&terminal_id);
                        }
                    }
                }
            }
        }
    }

    fn apply_diff_index_update(
        &mut self,
        scope_root: PathBuf,
        generation: u64,
        result: Result<DiffDocument, String>,
    ) {
        match result {
            Ok(document) => {
                let response_generation = generation;
                let branch_name = document.snapshot.branch_name.clone();
                let selected_file = {
                    let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
                        return;
                    };
                    if diff_tab
                        .index
                        .requested_generation
                        .is_some_and(|requested| response_generation < requested)
                    {
                        return;
                    }

                    diff_tab.index.loading = false;
                    diff_tab.index.error = None;

                    // Selection fallback chain per spec Section 6:
                    // 1. Same (section, path) if still exists
                    // 2. First file in conflicted_files
                    // 3. First file in staged_files
                    // 4. First file in unstaged_files
                    // 4. None (empty state)
                    let selected_file = diff_tab
                        .selected_file
                        .clone()
                        .filter(|key| {
                            let files = match key.section {
                                DiffSectionKind::Conflicted => &document.conflicted_files,
                                DiffSectionKind::Staged => &document.staged_files,
                                DiffSectionKind::Unstaged => &document.unstaged_files,
                            };
                            files.iter().any(|f| f.relative_path == key.relative_path)
                        })
                        .or_else(|| {
                            document.conflicted_files.first().map(|f| DiffSelectionKey {
                                section: DiffSectionKind::Conflicted,
                                relative_path: f.relative_path.clone(),
                            })
                        })
                        .or_else(|| {
                            document.staged_files.first().map(|f| DiffSelectionKey {
                                section: DiffSectionKind::Staged,
                                relative_path: f.relative_path.clone(),
                            })
                        })
                        .or_else(|| {
                            document.unstaged_files.first().map(|f| DiffSelectionKey {
                                section: DiffSectionKind::Unstaged,
                                relative_path: f.relative_path.clone(),
                            })
                        });

                    // Prune stale keys from multi_select
                    diff_tab.multi_select.retain(|key| {
                        let files = match key.section {
                            DiffSectionKind::Conflicted => &document.conflicted_files,
                            DiffSectionKind::Staged => &document.staged_files,
                            DiffSectionKind::Unstaged => &document.unstaged_files,
                        };
                        files.iter().any(|f| f.relative_path == key.relative_path)
                    });
                    let conflicted_paths = document
                        .conflicted_files
                        .iter()
                        .map(|file| file.relative_path.clone())
                        .collect::<HashSet<_>>();
                    diff_tab
                        .conflict_editor
                        .documents
                        .retain(|path, _| conflicted_paths.contains(path));

                    diff_tab.index.document = Some(document);
                    diff_tab.selected_file = selected_file.clone();
                    if selected_file.is_none() {
                        diff_tab.file = DiffFileState::default();
                    }
                    selected_file
                };

                self.update_diff_tab_title(&scope_root, &branch_name);
                if let Some(selection) = selected_file {
                    self.request_selected_file_diff(&scope_root, selection);
                }
            }
            Err(message) => {
                let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
                    return;
                };
                if diff_tab
                    .index
                    .requested_generation
                    .is_some_and(|requested| generation < requested)
                {
                    return;
                }
                diff_tab.index.loading = false;
                diff_tab.index.error = Some(message);
            }
        }
    }

    fn apply_file_diff_update(
        &mut self,
        scope_root: PathBuf,
        generation: u64,
        selection: DiffSelectionKey,
        result: Result<FileDiffDocument, String>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
            return;
        };
        let matches_current_request = diff_tab.file.requested_selection.as_ref()
            == Some(&selection)
            && diff_tab.file.requested_generation == Some(generation);
        if !matches_current_request || diff_tab.selected_file.as_ref() != Some(&selection) {
            return;
        }

        match result {
            Ok(document) => {
                diff_tab.file.document = Some(document);
                diff_tab.file.error = None;
                diff_tab.file.loading = false;
            }
            Err(message) => {
                diff_tab.file.loading = false;
                diff_tab.file.error = Some(message);
            }
        }
    }

    fn apply_repository_graph_update(
        &mut self,
        scope_root: PathBuf,
        request_revision: u64,
        result: Result<RepositoryGraphDocument, String>,
    ) {
        let project_ids: Vec<String> = self
            .repository_graph_tabs
            .iter()
            .filter(|(_, tab)| tab.scope_root == scope_root)
            .map(|(project_id, _)| project_id.clone())
            .collect();
        if project_ids.is_empty() {
            return;
        }

        let mut detail_requests = Vec::new();
        match result {
            Ok(document) => {
                let occupied_local_branches =
                    self.load_repository_branch_occupancy(&document.repo_root);
                for project_id in project_ids {
                    let Some(tab) = self.repository_graph_tabs.get_mut(&project_id) else {
                        continue;
                    };
                    if request_revision != REPOSITORY_GRAPH_AUTO_REFRESH_REVISION
                        && request_revision < tab.graph.requested_revision
                    {
                        continue;
                    }

                    tab.scope_root = document.scope_root.clone();
                    tab.graph.document = Some(document.clone());
                    tab.graph.error = None;
                    tab.graph.loading = false;
                    tab.occupied_local_branches = occupied_local_branches.clone();

                    if let Some(oid) = Self::reconcile_repository_graph_selection(tab) {
                        detail_requests.push((project_id.clone(), oid));
                    }
                    Self::sync_repository_remote_group_state(tab);
                }
            }
            Err(message) => {
                for project_id in project_ids {
                    let Some(tab) = self.repository_graph_tabs.get_mut(&project_id) else {
                        continue;
                    };
                    if request_revision != REPOSITORY_GRAPH_AUTO_REFRESH_REVISION
                        && request_revision < tab.graph.requested_revision
                    {
                        continue;
                    }
                    tab.graph.loading = false;
                    tab.graph.error = Some(message.clone());
                }
            }
        }

        for (project_id, oid) in detail_requests {
            self.request_repository_commit_detail(&project_id, oid);
        }
    }

    fn apply_repository_commit_detail_update(
        &mut self,
        scope_root: PathBuf,
        oid: Oid,
        request_revision: u64,
        result: Result<CommitDetailDocument, String>,
    ) {
        for tab in self
            .repository_graph_tabs
            .values_mut()
            .filter(|tab| tab.scope_root == scope_root && tab.selected_commit == Some(oid))
        {
            if request_revision < tab.commit_detail.requested_revision {
                continue;
            }

            tab.commit_detail.loading = false;
            match &result {
                Ok(detail) => {
                    tab.commit_detail.document = Some(detail.clone());
                    tab.commit_detail.error = None;
                    if let Some(selection) = tab.selected_commit_file.as_ref() {
                        let still_present = detail
                            .changed_files
                            .iter()
                            .any(|file| file.path == selection.relative_path);
                        if !still_present {
                            tab.selected_commit_file = None;
                            tab.commit_file_diff = AsyncDocumentState::default();
                        }
                    }
                }
                Err(message) => {
                    tab.commit_detail.error = Some(message.clone());
                }
            }
        }
    }

    fn apply_repository_commit_file_diff_update(
        &mut self,
        scope_root: PathBuf,
        selection: CommitFileSelection,
        request_revision: u64,
        result: Result<CommitFileDiffDocument, String>,
    ) {
        for tab in self
            .repository_graph_tabs
            .values_mut()
            .filter(|tab| tab.scope_root == scope_root)
        {
            if request_revision < tab.commit_file_diff.requested_revision {
                continue;
            }
            if tab.selected_commit_file.as_ref() != Some(&selection) {
                continue;
            }

            tab.commit_file_diff.loading = false;
            match &result {
                Ok(document) => {
                    tab.commit_file_diff.document = Some(document.clone());
                    tab.commit_file_diff.error = None;
                }
                Err(message) => {
                    tab.selected_commit_file = None;
                    tab.commit_file_diff.document = None;
                    tab.commit_file_diff.error = Some(message.clone());
                    tab.action_banner = Some(ActionBanner {
                        kind: ActionBannerKind::Error,
                        message: message.clone(),
                    });
                }
            }
        }
    }

    fn apply_stash_list_update(
        &mut self,
        scope_root: PathBuf,
        request_revision: u64,
        result: Result<StashListDocument, String>,
    ) {
        let mut detail_request = None;
        let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
            return;
        };
        if request_revision != 0 && request_revision < diff_tab.stash.list.requested_revision {
            return;
        }

        diff_tab.stash.list.loading = false;
        match result {
            Ok(document) => {
                diff_tab.stash.list.document = Some(document.clone());
                diff_tab.stash.list.error = None;

                let has_entry =
                    |oid: Oid| document.entries.iter().any(|entry| entry.stash_oid == oid);
                let next_selected = diff_tab
                    .stash
                    .selected_stash
                    .filter(|oid| has_entry(*oid))
                    .or_else(|| document.entries.first().map(|entry| entry.stash_oid));
                let next_expanded = diff_tab
                    .stash
                    .expanded_stash
                    .filter(|oid| has_entry(*oid))
                    .or(next_selected);

                let selection_changed = diff_tab.stash.selected_stash != next_selected;
                diff_tab.stash.selected_stash = next_selected;
                diff_tab.stash.expanded_stash = next_expanded;

                if selection_changed {
                    diff_tab.stash.detail = AsyncDocumentState::default();
                    diff_tab.stash.selected_file = None;
                    diff_tab.stash.file_diff = AsyncDocumentState::default();
                }

                if diff_tab.stash.selected_stash.is_none() {
                    diff_tab.stash.detail = AsyncDocumentState::default();
                    diff_tab.stash.selected_file = None;
                    diff_tab.stash.file_diff = AsyncDocumentState::default();
                } else if let Some(stash_oid) = diff_tab.stash.selected_stash {
                    if diff_tab
                        .stash
                        .detail
                        .document
                        .as_ref()
                        .is_none_or(|detail| detail.stash_oid != stash_oid)
                        && !diff_tab.stash.detail.loading
                    {
                        detail_request = Some(stash_oid);
                    }
                }
            }
            Err(message) => {
                diff_tab.stash.list.error = Some(message);
            }
        }

        if let Some(stash_oid) = detail_request {
            self.request_stash_detail(&scope_root, stash_oid);
        }
    }

    fn apply_stash_detail_update(
        &mut self,
        scope_root: PathBuf,
        stash_oid: Oid,
        request_revision: u64,
        result: Result<StashDetailDocument, String>,
    ) {
        let mut file_request = None;
        let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
            return;
        };
        if request_revision < diff_tab.stash.detail.requested_revision
            || diff_tab.stash.selected_stash != Some(stash_oid)
        {
            return;
        }

        diff_tab.stash.detail.loading = false;
        match result {
            Ok(detail) => {
                diff_tab.stash.detail.document = Some(detail.clone());
                diff_tab.stash.detail.error = None;
                if let Some(selection) = diff_tab.stash.selected_file.as_ref() {
                    let still_present = detail
                        .files
                        .iter()
                        .any(|file| file.relative_path == selection.relative_path);
                    if !still_present {
                        diff_tab.stash.selected_file = None;
                        diff_tab.stash.file_diff = AsyncDocumentState::default();
                    } else if diff_tab
                        .stash
                        .file_diff
                        .document
                        .as_ref()
                        .is_none_or(|document| document.selection != *selection)
                        && !diff_tab.stash.file_diff.loading
                    {
                        file_request = Some(selection.clone());
                    }
                }
            }
            Err(message) => {
                diff_tab.stash.detail.document = None;
                diff_tab.stash.detail.error = Some(message);
                diff_tab.stash.selected_file = None;
                diff_tab.stash.file_diff = AsyncDocumentState::default();
            }
        }

        if let Some(selection) = file_request {
            self.request_stash_file_diff(&scope_root, selection);
        }
    }

    fn apply_stash_file_diff_update(
        &mut self,
        scope_root: PathBuf,
        selection: StashFileSelection,
        request_revision: u64,
        result: Result<StashFileDiffDocument, String>,
    ) {
        let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) else {
            return;
        };
        if request_revision < diff_tab.stash.file_diff.requested_revision
            || diff_tab.stash.selected_file.as_ref() != Some(&selection)
        {
            return;
        }

        diff_tab.stash.file_diff.loading = false;
        match result {
            Ok(document) => {
                diff_tab.stash.file_diff.document = Some(document);
                diff_tab.stash.file_diff.error = None;
            }
            Err(message) => {
                diff_tab.stash.file_diff.document = None;
                diff_tab.stash.file_diff.error = Some(message.clone());
                diff_tab.last_action_banner = Some(ActionBanner {
                    kind: ActionBannerKind::Error,
                    message,
                });
            }
        }
    }

    fn reconcile_repository_graph_selection(tab: &mut RepositoryGraphTabState) -> Option<Oid> {
        let graph = tab.graph.document.as_ref()?;

        if let Some(selected_commit) = tab.selected_commit {
            let commit_still_present = graph
                .commits
                .iter()
                .any(|commit| commit.oid == selected_commit);
            if commit_still_present {
                tab.selected_branch = None;
                if tab
                    .commit_detail
                    .document
                    .as_ref()
                    .is_none_or(|detail| detail.oid != selected_commit)
                    && !tab.commit_detail.loading
                {
                    return Some(selected_commit);
                }
                return None;
            }

            tab.selected_commit = None;
            tab.commit_detail = AsyncDocumentState::default();
            tab.selected_commit_file = None;
            tab.commit_file_diff = AsyncDocumentState::default();
        }

        let branch_is_valid = tab
            .selected_branch
            .as_ref()
            .and_then(|selection| match selection {
                RepositoryBranchSelection::Local { name } => graph
                    .local_branches
                    .iter()
                    .find(|branch| branch.name == *name)
                    .map(|branch| RepositoryBranchSelection::Local {
                        name: branch.name.clone(),
                    }),
                RepositoryBranchSelection::Remote { full_ref } => graph
                    .remote_branches
                    .iter()
                    .find(|branch| branch.full_ref == *full_ref)
                    .map(|branch| RepositoryBranchSelection::Remote {
                        full_ref: branch.full_ref.clone(),
                    }),
            });

        tab.selected_branch =
            branch_is_valid.or_else(|| Self::default_repository_branch_selection(graph));
        None
    }

    fn sync_repository_remote_group_state(tab: &mut RepositoryGraphTabState) {
        let Some(graph) = tab.graph.document.as_ref() else {
            return;
        };

        let available_remotes = graph
            .remote_branches
            .iter()
            .map(|branch| branch.remote_name.clone())
            .collect::<HashSet<_>>();

        if !tab.remote_groups_seeded {
            tab.expanded_remote_groups = Self::default_repository_expanded_remote_groups(
                graph,
                tab.selected_branch.as_ref(),
            );
            tab.remote_groups_seeded = true;
        } else {
            tab.expanded_remote_groups
                .retain(|remote| available_remotes.contains(remote));
        }
    }

    fn default_repository_expanded_remote_groups(
        graph: &RepositoryGraphDocument,
        selection: Option<&RepositoryBranchSelection>,
    ) -> HashSet<String> {
        let mut expanded = HashSet::new();
        if let HeadState::Branch { name, .. } = &graph.head {
            if let Some(remote_name) = graph
                .local_branches
                .iter()
                .find(|branch| branch.name == *name)
                .and_then(|branch| branch.upstream.as_ref())
                .map(|upstream| upstream.remote_name.clone())
            {
                expanded.insert(remote_name);
            }
        }
        if let Some(remote_name) = selection.and_then(|selection| {
            Self::repository_remote_name_for_remote_selection(graph, selection)
        }) {
            expanded.insert(remote_name);
        }
        expanded
    }

    fn expand_repository_remote_group_for_remote_selection(
        tab: &mut RepositoryGraphTabState,
        selection: &RepositoryBranchSelection,
    ) {
        let Some(graph) = tab.graph.document.as_ref() else {
            return;
        };
        if let Some(remote_name) =
            Self::repository_remote_name_for_remote_selection(graph, selection)
        {
            tab.expanded_remote_groups.insert(remote_name);
            tab.remote_groups_seeded = true;
        }
    }

    fn repository_remote_name_for_remote_selection(
        graph: &RepositoryGraphDocument,
        selection: &RepositoryBranchSelection,
    ) -> Option<String> {
        match selection {
            RepositoryBranchSelection::Remote { full_ref } => graph
                .remote_branches
                .iter()
                .find(|branch| branch.full_ref == *full_ref)
                .map(|branch| branch.remote_name.clone()),
            RepositoryBranchSelection::Local { .. } => None,
        }
    }

    fn default_repository_branch_selection(
        graph: &RepositoryGraphDocument,
    ) -> Option<RepositoryBranchSelection> {
        if let Some(branch_name) = match &graph.head {
            orcashell_git::HeadState::Branch { name, .. } => Some(name),
            orcashell_git::HeadState::Detached { .. } | orcashell_git::HeadState::Unborn => None,
        } {
            if graph
                .local_branches
                .iter()
                .any(|branch| branch.name == *branch_name)
            {
                return Some(RepositoryBranchSelection::Local {
                    name: branch_name.clone(),
                });
            }
        }

        graph
            .local_branches
            .first()
            .map(|branch| RepositoryBranchSelection::Local {
                name: branch.name.clone(),
            })
            .or_else(|| {
                graph
                    .remote_branches
                    .first()
                    .map(|branch| RepositoryBranchSelection::Remote {
                        full_ref: branch.full_ref.clone(),
                    })
            })
    }

    fn load_repository_branch_occupancy(&self, repo_root: &Path) -> HashSet<String> {
        self.services
            .store
            .lock()
            .as_ref()
            .and_then(|store| store.load_worktrees_for_repo_root(repo_root).ok())
            .map(|worktrees| {
                worktrees
                    .into_iter()
                    .map(|worktree| worktree.branch_name)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn scope_git_action_in_flight(&self, scope_root: &Path) -> bool {
        self.diff_tabs
            .get(scope_root)
            .is_some_and(|tab| tab.local_action_in_flight || tab.remote_op_in_flight)
            || self.repository_graph_tabs.values().any(|tab| {
                tab.scope_root == scope_root
                    && (tab.fetch_in_flight
                        || tab.pull_in_flight
                        || tab.active_branch_action.is_some())
            })
    }

    pub(crate) fn repository_toolbar_action_in_flight(&self, scope_root: &Path) -> bool {
        self.diff_tabs
            .get(scope_root)
            .is_some_and(|tab| tab.local_action_in_flight || tab.remote_op_in_flight)
            || self.repository_graph_tabs.values().any(|tab| {
                tab.scope_root == scope_root
                    && (tab.pull_in_flight
                        || tab.active_branch_action.is_some()
                        || (tab.fetch_in_flight
                            && tab.active_fetch_origin != Some(GitFetchOrigin::Automatic)))
            })
    }

    fn apply_local_action_completion(
        &mut self,
        scope_root: PathBuf,
        action: GitActionKind,
        result: Result<String, String>,
    ) {
        let banner = Self::action_banner_from_result(&result);

        // Handle successful RemoveWorktree specially: delete SQLite row + close diff tab.
        if result.is_ok() && matches!(action, GitActionKind::RemoveWorktree) {
            // Capture managed worktree ID before any state changes.
            let managed_id = self
                .diff_tabs
                .get(&scope_root)
                .and_then(|tab| tab.managed_worktree.as_ref())
                .map(|m| m.id.clone());

            // Delete SQLite worktree row.
            if let Some(id) = managed_id {
                if let Some(store) = self.services.store.lock().as_mut() {
                    if let Err(e) = store.delete_worktree(&id) {
                        tracing::warn!("Failed to delete worktree row from SQLite: {e}");
                    }
                }
            }

            // Close the diff tab (no-op if already closed).
            let tab_id = Self::diff_tab_id(&scope_root);
            self.close_auxiliary_tab_internal(&tab_id);

            // Return early. Don't set banner on a tab that no longer exists.
            return;
        }

        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.local_action_in_flight = false;

            if Self::diff_tab_handles_local_action(&action) {
                // Skip success banners for the actions where the refreshed diff
                // is the primary feedback.
                if matches!(&result, Ok(message) if !message.starts_with("BLOCKED: "))
                    && matches!(
                        action,
                        GitActionKind::Stage
                            | GitActionKind::Unstage
                            | GitActionKind::DiscardFile
                            | GitActionKind::DiscardHunk
                    )
                {
                    diff_tab.multi_select.clear();
                    diff_tab.selection_anchor = None;
                    diff_tab.last_action_banner = None;
                } else {
                    diff_tab.last_action_banner = Some(banner.clone());
                }

                // Clear commit message after successful commit
                if result.is_ok() && matches!(action, GitActionKind::Commit) {
                    diff_tab.commit_message.clear();
                    diff_tab.multi_select.clear();
                    diff_tab.selection_anchor = None;
                }
                if result.is_ok() && matches!(action, GitActionKind::CreateStash) {
                    diff_tab.view_mode = DiffTabViewMode::Stashes;
                    diff_tab.stash.selected_stash = None;
                    diff_tab.stash.expanded_stash = None;
                    diff_tab.stash.detail = AsyncDocumentState::default();
                    diff_tab.stash.selected_file = None;
                    diff_tab.stash.file_diff = AsyncDocumentState::default();
                }
                if Self::result_requests_graph_refresh(&result)
                    && matches!(action, GitActionKind::ApplyStash | GitActionKind::PopStash)
                {
                    diff_tab.view_mode = DiffTabViewMode::WorkingTree;
                }
            }
        }

        if matches!(
            action,
            GitActionKind::CheckoutLocalBranch
                | GitActionKind::CheckoutRemoteBranch
                | GitActionKind::CreateLocalBranch
                | GitActionKind::DeleteLocalBranch
        ) {
            for tab in self
                .repository_graph_tabs
                .values_mut()
                .filter(|tab| tab.scope_root == scope_root)
            {
                let completed_action = tab.active_branch_action.take();
                if Self::result_requests_graph_refresh(&result) {
                    match completed_action {
                        Some(RepositoryBranchAction::Create {
                            new_branch_name, ..
                        }) => {
                            tab.selected_branch = Some(RepositoryBranchSelection::Local {
                                name: new_branch_name,
                            });
                        }
                        Some(RepositoryBranchAction::Delete { branch_name }) => {
                            if tab.selected_branch.as_ref()
                                == Some(&RepositoryBranchSelection::Local { name: branch_name })
                            {
                                tab.selected_branch = None;
                            }
                        }
                        Some(RepositoryBranchAction::Checkout { .. }) | None => {}
                    }
                }
                tab.action_banner = Some(banner.clone());
                if Self::result_requests_graph_refresh(&result) {
                    tab.graph.loading = true;
                    tab.graph.error = None;
                }
            }
        }
    }

    fn apply_remote_op_completion(
        &mut self,
        scope_root: PathBuf,
        kind: GitRemoteKind,
        fetch_origin: Option<GitFetchOrigin>,
        refresh_graph: bool,
        result: Result<String, String>,
    ) {
        let banner = Self::action_banner_from_result(&result);
        let completed_at = SystemTime::now();

        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            let tracks_diff_busy =
                kind != GitRemoteKind::Fetch || fetch_origin != Some(GitFetchOrigin::Automatic);
            if tracks_diff_busy {
                diff_tab.remote_op_in_flight = false;
                if kind != GitRemoteKind::Fetch {
                    diff_tab.last_action_banner = Some(banner.clone());
                }
            }
        }

        if matches!(
            kind,
            GitRemoteKind::Fetch | GitRemoteKind::Publish | GitRemoteKind::Pull
        ) {
            for tab in self
                .repository_graph_tabs
                .values_mut()
                .filter(|tab| tab.scope_root == scope_root)
            {
                if kind == GitRemoteKind::Fetch {
                    tab.fetch_in_flight = false;
                    tab.active_fetch_origin = None;
                    match &result {
                        Ok(_) => {
                            tab.last_remote_check_at = Some(completed_at);
                            tab.last_automatic_fetch_failure_at = None;
                            if fetch_origin == Some(GitFetchOrigin::Manual) {
                                tab.action_banner = Some(banner.clone());
                            }
                        }
                        Err(_) => {
                            if fetch_origin == Some(GitFetchOrigin::Manual) {
                                tab.action_banner = Some(banner.clone());
                            } else if fetch_origin == Some(GitFetchOrigin::Automatic) {
                                tab.last_automatic_fetch_failure_at = Some(completed_at);
                            }
                        }
                    }
                } else {
                    if kind == GitRemoteKind::Pull {
                        tab.pull_in_flight = false;
                    }
                    tab.action_banner = Some(banner.clone());
                }
                if refresh_graph {
                    tab.graph.loading = true;
                    tab.graph.error = None;
                }
            }
        }

        if kind == GitRemoteKind::Publish && refresh_graph {
            self.request_diff_index_refresh(&scope_root);
        }
    }

    fn action_banner_from_result(result: &Result<String, String>) -> ActionBanner {
        match result {
            Ok(msg) if msg.starts_with("BLOCKED: ") => ActionBanner {
                kind: ActionBannerKind::Warning,
                message: msg["BLOCKED: ".len()..].to_string(),
            },
            Ok(msg) if msg.starts_with("CONFLICT: ") => ActionBanner {
                kind: ActionBannerKind::Warning,
                message: msg["CONFLICT: ".len()..].to_string(),
            },
            Ok(msg) => ActionBanner {
                kind: ActionBannerKind::Success,
                message: msg.clone(),
            },
            Err(msg) => ActionBanner {
                kind: ActionBannerKind::Error,
                message: msg.clone(),
            },
        }
    }

    fn diff_tab_handles_local_action(action: &GitActionKind) -> bool {
        matches!(
            action,
            GitActionKind::Stage
                | GitActionKind::Unstage
                | GitActionKind::DiscardAll
                | GitActionKind::DiscardFile
                | GitActionKind::DiscardHunk
                | GitActionKind::Commit
                | GitActionKind::CreateStash
                | GitActionKind::ApplyStash
                | GitActionKind::PopStash
                | GitActionKind::DropStash
                | GitActionKind::MergeBack
                | GitActionKind::CompleteMerge
                | GitActionKind::AbortMerge
                | GitActionKind::CreateLocalBranch
                | GitActionKind::DeleteLocalBranch
                | GitActionKind::RemoveWorktree
        )
    }

    fn result_requests_graph_refresh(result: &Result<String, String>) -> bool {
        matches!(result, Ok(message) if !message.starts_with("BLOCKED: ") && !message.starts_with("CONFLICT: "))
    }

    fn persist_worktree(
        &self,
        project_id: &str,
        worktree: &ManagedWorktree,
        primary_terminal_id: Option<String>,
    ) -> anyhow::Result<()> {
        let mut store = self.services.store.lock();
        let Some(store) = store.as_mut() else {
            anyhow::bail!("database is unavailable");
        };

        store.save_worktree(&StoredWorktree {
            id: worktree.id.clone(),
            project_id: project_id.to_string(),
            repo_root: worktree.repo_root.clone(),
            path: worktree.path.clone(),
            worktree_name: worktree.worktree_name.clone(),
            branch_name: worktree.branch_name.clone(),
            source_ref: worktree.source_ref.clone(),
            primary_terminal_id,
        })
    }

    fn settings_tab() -> AuxiliaryTabState {
        AuxiliaryTabState {
            id: SETTINGS_TAB_ID.to_string(),
            title: "Settings".to_string(),
            kind: AuxiliaryTabKind::Settings,
        }
    }

    fn live_diff_stream_tab_id(project_id: &str) -> String {
        format!("aux-live-diff-{project_id}")
    }

    fn live_diff_stream_title(project_name: &str) -> String {
        format!("Live Feed: {project_name}")
    }

    fn repository_graph_tab_id(project_id: &str) -> String {
        format!("aux-repository-{project_id}")
    }

    fn repository_graph_title(project_name: &str) -> String {
        format!("Repository: {project_name}")
    }

    fn diff_tab_id(scope_root: &Path) -> String {
        let scope = scope_root.to_string_lossy();
        format!(
            "aux-diff-{}",
            Uuid::new_v5(&Uuid::NAMESPACE_URL, scope.as_bytes())
        )
    }

    fn ensure_settings_tab(&mut self) {
        if self
            .auxiliary_tabs
            .iter()
            .any(|tab| matches!(tab.kind, AuxiliaryTabKind::Settings))
        {
            return;
        }

        self.auxiliary_tabs.insert(0, Self::settings_tab());
    }

    fn focus_auxiliary_tab_internal(&mut self, tab_id: &str) -> bool {
        if !self
            .auxiliary_tabs
            .iter()
            .any(|tab| tab.id.as_str() == tab_id)
        {
            return false;
        }

        self.active_auxiliary_tab_id = Some(tab_id.to_string());
        if let Some(tab_kind) = self.active_auxiliary_tab().map(|tab| tab.kind.clone()) {
            match tab_kind {
                AuxiliaryTabKind::Diff { scope_root } => {
                    self.services.git.request_snapshot(&scope_root, None);
                    self.request_diff_index_refresh(&scope_root);
                    self.request_stash_list(&scope_root);
                }
                AuxiliaryTabKind::LiveDiffStream { project_id } => {
                    self.ensure_live_diff_feed_state(&project_id);
                }
                AuxiliaryTabKind::RepositoryGraph { project_id } => {
                    self.ensure_repository_graph_state(&project_id);
                }
                AuxiliaryTabKind::Settings => {}
            }
        }
        true
    }

    fn close_auxiliary_tab_internal(&mut self, tab_id: &str) -> bool {
        let Some(index) = self.auxiliary_tabs.iter().position(|tab| tab.id == tab_id) else {
            return false;
        };

        let removed = self.auxiliary_tabs.remove(index);
        if self.active_auxiliary_tab_id.as_deref() == Some(tab_id) {
            self.active_auxiliary_tab_id = None;
        }
        match removed.kind {
            AuxiliaryTabKind::Diff { scope_root } => {
                self.diff_tabs.remove(&scope_root);
            }
            AuxiliaryTabKind::LiveDiffStream { project_id } => {
                self.cancel_feed_captures_for_project(&project_id);
                self.live_diff_feeds.remove(&project_id);
            }
            AuxiliaryTabKind::RepositoryGraph { project_id } => {
                self.repository_graph_tabs.remove(&project_id);
            }
            AuxiliaryTabKind::Settings => {}
        }
        true
    }

    fn update_diff_tab_title(&mut self, scope_root: &Path, branch_name: &str) {
        if let Some(tab) = self.auxiliary_tabs.iter_mut().find(|tab| {
            matches!(
                &tab.kind,
                AuxiliaryTabKind::Diff {
                    scope_root: tab_scope_root
                } if tab_scope_root == scope_root
            )
        }) {
            tab.title = format!("Diff: {branch_name}");
        }
    }

    fn ensure_live_diff_feed_state(&mut self, project_id: &str) {
        let tracked_scopes = self.tracked_scope_roots_for_project(project_id);
        let feed_was_present = self.live_diff_feeds.contains_key(project_id);
        let scopes_to_refresh = {
            let feed = self
                .live_diff_feeds
                .entry(project_id.to_string())
                .or_insert_with(|| ChangeFeedState::new(project_id.to_string()));
            let previous_scopes = feed.tracked_scopes.keys().cloned().collect::<HashSet<_>>();
            feed.replace_tracked_scopes(tracked_scopes);
            if !feed_was_present {
                feed.tracked_scopes.keys().cloned().collect::<Vec<_>>()
            } else {
                feed.tracked_scopes
                    .keys()
                    .filter(|scope_root| !previous_scopes.contains(*scope_root))
                    .cloned()
                    .collect::<Vec<_>>()
            }
        };

        for scope_root in scopes_to_refresh {
            self.request_live_feed_capture(project_id, &scope_root);
        }
    }

    fn ensure_repository_graph_state(&mut self, project_id: &str) {
        let Some(scope_root) = self.resolve_repository_scope_root(project_id) else {
            return;
        };
        let graph_tab = self
            .repository_graph_tabs
            .entry(project_id.to_string())
            .or_insert_with(|| {
                RepositoryGraphTabState::new(project_id.to_string(), scope_root.clone())
            });
        if graph_tab.scope_root != scope_root {
            graph_tab.scope_root = scope_root;
        }
        self.request_repository_graph_refresh(project_id);
    }

    fn resolve_repository_scope_root(&self, project_id: &str) -> Option<PathBuf> {
        let project = self.project(project_id)?;
        match discover_scope(&project.path) {
            Ok(scope) => Some(scope.scope_root),
            Err(_) => Some(project.path.clone()),
        }
    }

    fn tracked_scope_roots_for_project(&self, project_id: &str) -> Vec<PathBuf> {
        let Some(project) = self.project(project_id) else {
            return Vec::new();
        };

        let mut seen = HashSet::new();
        let mut scopes = Vec::new();
        for terminal_id in project.layout.collect_terminal_ids() {
            let Some(scope_root) = self.terminal_git_scopes.get(&terminal_id) else {
                continue;
            };
            if seen.insert(scope_root.clone()) {
                scopes.push(scope_root.clone());
            }
        }
        scopes
    }

    fn refresh_live_diff_feed_scope_membership(&mut self, project_id: &str) {
        if !self.live_diff_feeds.contains_key(project_id) {
            return;
        }
        self.ensure_live_diff_feed_state(project_id);
    }

    fn cancel_feed_captures_for_project(&mut self, project_id: &str) {
        let _ = project_id;
    }

    fn cancel_feed_capture_target(&mut self, project_id: &str, entry_id: u64) {
        let _ = (project_id, entry_id);
    }

    fn request_live_feed_capture(&mut self, project_id: &str, scope_root: &Path) {
        let latest_generation = self.latest_diff_generation(scope_root);
        let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
            return;
        };
        let Some(scope_state) = feed.tracked_scopes.get_mut(scope_root) else {
            return;
        };
        if let Some(latest_generation) = latest_generation {
            scope_state.latest_snapshot_generation = Some(
                scope_state
                    .latest_snapshot_generation
                    .map_or(latest_generation, |current| current.max(latest_generation)),
            );
        }
        if scope_state.pending_refresh {
            return;
        }
        let Some(generation) = latest_generation else {
            return;
        };
        let previous = scope_state.emitted_baseline.clone();
        scope_state.latest_request_revision += 1;
        let request_revision = scope_state.latest_request_revision;
        scope_state.pending_refresh = true;
        self.services.git.request_feed_capture(
            project_id,
            scope_root,
            generation,
            request_revision,
            previous,
        );
    }

    fn refresh_live_feeds_for_scope_if_stale(&mut self, scope_root: &Path, latest_generation: u64) {
        let project_ids: Vec<String> = self
            .live_diff_feeds
            .iter()
            .filter(|(_, feed)| feed.tracked_scopes.contains_key(scope_root))
            .filter_map(|(project_id, feed)| {
                let scope_state = feed.tracked_scopes.get(scope_root)?;
                let stale = scope_state
                    .last_generation
                    .is_none_or(|generation| generation < latest_generation);
                if stale {
                    Some(project_id.clone())
                } else {
                    None
                }
            })
            .collect();

        for project_id in project_ids {
            if let Some(feed) = self.live_diff_feeds.get_mut(&project_id) {
                if let Some(scope_state) = feed.tracked_scopes.get_mut(scope_root) {
                    scope_state.latest_snapshot_generation = Some(
                        scope_state
                            .latest_snapshot_generation
                            .map_or(latest_generation, |current| current.max(latest_generation)),
                    );
                }
            }
            self.request_live_feed_capture(&project_id, scope_root);
        }
    }

    fn latest_diff_generation(&self, scope_root: &Path) -> Option<u64> {
        self.git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.generation)
    }

    fn record_live_feed_scope_error(&mut self, scope_root: &Path, message: String) {
        for feed in self
            .live_diff_feeds
            .values_mut()
            .filter(|feed| feed.tracked_scopes.contains_key(scope_root))
        {
            let next_revision = feed.next_error_revision;
            feed.next_error_revision += 1;
            if let Some(scope_state) = feed.tracked_scopes.get_mut(scope_root) {
                scope_state.pending_refresh = false;
                scope_state.latest_error = Some(format!("{}: {}", scope_root.display(), message));
                scope_state.error_revision = next_revision;
            }
        }
    }

    fn request_diff_index_refresh(&mut self, scope_root: &Path) {
        let requested_generation = self.latest_diff_generation(scope_root);
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.index.loading && diff_tab.index.requested_generation == requested_generation {
            return;
        }

        diff_tab.index.loading = true;
        diff_tab.index.error = None;
        diff_tab.index.requested_generation = requested_generation;
        self.services.git.request_diff_index(scope_root);
    }

    fn request_repository_graph_refresh(&mut self, project_id: &str) {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return;
        };
        if tab.graph.loading {
            return;
        }

        tab.graph.loading = true;
        tab.graph.error = None;
        tab.graph.requested_revision += 1;
        self.services
            .git
            .request_repository_graph(&tab.scope_root, tab.graph.requested_revision);
    }

    fn select_repository_branch_internal(
        &mut self,
        project_id: &str,
        selection: RepositoryBranchSelection,
    ) -> bool {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return false;
        };
        if tab.selected_branch.as_ref() == Some(&selection)
            && tab.selected_commit.is_none()
            && tab.selected_commit_file.is_none()
        {
            return false;
        }

        tab.selected_branch = Some(selection.clone());
        Self::expand_repository_remote_group_for_remote_selection(tab, &selection);
        tab.selected_commit = None;
        tab.commit_detail = AsyncDocumentState::default();
        tab.selected_commit_file = None;
        tab.commit_file_diff = AsyncDocumentState::default();
        true
    }

    fn back_to_repository_commit_internal(&mut self, project_id: &str) -> bool {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return false;
        };
        if tab.selected_commit_file.take().is_some() {
            tab.commit_file_diff.error = None;
            return true;
        }
        false
    }

    fn request_repository_commit_detail(&mut self, project_id: &str, oid: Oid) {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return;
        };
        if tab.commit_detail.loading
            && tab
                .commit_detail
                .document
                .as_ref()
                .is_some_and(|detail| detail.oid == oid)
        {
            return;
        }

        tab.selected_branch = None;
        tab.selected_commit = Some(oid);
        tab.selected_commit_file = None;
        tab.commit_file_diff = AsyncDocumentState::default();
        tab.commit_detail.document = None;
        tab.commit_detail.loading = true;
        tab.commit_detail.error = None;
        tab.commit_detail.requested_revision += 1;
        self.services.git.request_commit_detail(
            &tab.scope_root,
            oid,
            tab.commit_detail.requested_revision,
        );
    }

    fn request_repository_commit_file_diff(
        &mut self,
        project_id: &str,
        selection: CommitFileSelection,
    ) {
        let Some(tab) = self.repository_graph_tabs.get_mut(project_id) else {
            return;
        };
        if tab.selected_commit != Some(selection.commit_oid) {
            return;
        }
        if tab.commit_file_diff.loading && tab.selected_commit_file.as_ref() == Some(&selection) {
            return;
        }
        if tab
            .commit_file_diff
            .document
            .as_ref()
            .is_some_and(|document| document.selection == selection)
        {
            tab.selected_commit_file = Some(selection);
            tab.commit_file_diff.error = None;
            return;
        }

        tab.selected_commit_file = Some(selection.clone());
        tab.commit_file_diff.document = None;
        tab.commit_file_diff.loading = true;
        tab.commit_file_diff.error = None;
        tab.commit_file_diff.requested_revision += 1;
        self.services.git.request_commit_file_diff(
            &tab.scope_root,
            selection.commit_oid,
            selection.relative_path.clone(),
            tab.commit_file_diff.requested_revision,
        );
    }

    fn request_stash_list(&mut self, scope_root: &Path) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.stash.list.loading {
            return;
        }

        diff_tab.stash.list.loading = true;
        diff_tab.stash.list.error = None;
        diff_tab.stash.list.requested_revision += 1;
        self.services
            .git
            .request_stash_list(scope_root, diff_tab.stash.list.requested_revision);
    }

    fn request_stash_detail(&mut self, scope_root: &Path, stash_oid: Oid) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.stash.detail.loading
            && diff_tab
                .stash
                .detail
                .document
                .as_ref()
                .is_some_and(|detail| detail.stash_oid == stash_oid)
        {
            return;
        }

        diff_tab.stash.selected_stash = Some(stash_oid);
        diff_tab.stash.expanded_stash = Some(stash_oid);
        diff_tab.stash.selected_file = None;
        diff_tab.stash.file_diff = AsyncDocumentState::default();
        diff_tab.stash.detail.document = None;
        diff_tab.stash.detail.loading = true;
        diff_tab.stash.detail.error = None;
        diff_tab.stash.detail.requested_revision += 1;
        self.services.git.request_stash_detail(
            scope_root,
            stash_oid,
            diff_tab.stash.detail.requested_revision,
        );
    }

    fn request_stash_file_diff(&mut self, scope_root: &Path, selection: StashFileSelection) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        if diff_tab.stash.selected_stash != Some(selection.stash_oid) {
            return;
        }
        if diff_tab.stash.file_diff.loading
            && diff_tab.stash.selected_file.as_ref() == Some(&selection)
        {
            return;
        }
        if diff_tab
            .stash
            .file_diff
            .document
            .as_ref()
            .is_some_and(|document| document.selection == selection)
        {
            diff_tab.stash.selected_file = Some(selection);
            diff_tab.stash.file_diff.error = None;
            return;
        }

        diff_tab.stash.selected_file = Some(selection.clone());
        diff_tab.stash.file_diff.document = None;
        diff_tab.stash.file_diff.loading = true;
        diff_tab.stash.file_diff.error = None;
        diff_tab.stash.file_diff.requested_revision += 1;
        self.services.git.request_stash_file_diff(
            scope_root,
            selection.stash_oid,
            selection.relative_path.clone(),
            diff_tab.stash.file_diff.requested_revision,
        );
    }

    fn request_selected_file_diff(&mut self, scope_root: &Path, selection: DiffSelectionKey) {
        let latest_generation = self.latest_diff_generation(scope_root);
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        let generation = diff_tab
            .index
            .document
            .as_ref()
            .map(|document| document.snapshot.generation)
            .or(latest_generation);

        let Some(generation) = generation else {
            return;
        };

        if selection.section == DiffSectionKind::Conflicted {
            let should_reload = match diff_tab
                .conflict_editor
                .documents
                .get(&selection.relative_path)
            {
                Some(ConflictDocumentState::Loaded(document)) => {
                    !document.is_dirty && document.generation != generation
                }
                Some(ConflictDocumentState::Unavailable(document)) => {
                    document.generation != generation
                }
                None => true,
            };
            diff_tab.file = DiffFileState {
                document: None,
                error: None,
                loading: false,
                requested_generation: Some(generation),
                requested_selection: Some(selection.clone()),
            };
            if should_reload {
                self.reload_conflict_document(scope_root, generation, &selection, true);
            }
            return;
        }

        if diff_tab.file.loading
            && diff_tab.file.requested_generation == Some(generation)
            && diff_tab.file.requested_selection.as_ref() == Some(&selection)
        {
            return;
        }
        if diff_tab.file.document.as_ref().is_some_and(|document| {
            document.generation == generation && document.selection == selection
        }) {
            diff_tab.file.error = None;
            diff_tab.file.loading = false;
            diff_tab.file.requested_generation = Some(generation);
            diff_tab.file.requested_selection = Some(selection);
            return;
        }

        diff_tab.file.loading = true;
        diff_tab.file.error = None;
        diff_tab.file.requested_generation = Some(generation);
        diff_tab.file.requested_selection = Some(selection.clone());
        self.services
            .git
            .request_file_diff(scope_root, generation, &selection);
    }

    fn refresh_diff_tab_if_stale(&mut self, scope_root: &Path, latest_generation: u64) {
        let Some(diff_tab) = self.diff_tabs.get(scope_root) else {
            return;
        };

        let index_generation = diff_tab
            .index
            .document
            .as_ref()
            .map(|document| document.snapshot.generation);
        let file_generation = diff_tab
            .file
            .document
            .as_ref()
            .map(|document| document.generation);
        let index_stale = index_generation.is_some_and(|generation| generation < latest_generation);
        let file_stale = diff_tab.selected_file.is_some()
            && file_generation.is_some_and(|generation| generation < latest_generation);
        let in_flight_current = diff_tab.index.loading
            && diff_tab.index.requested_generation == Some(latest_generation)
            && (diff_tab.selected_file.is_none()
                || (diff_tab.file.loading
                    && diff_tab.file.requested_generation == Some(latest_generation)));
        if !(index_stale || file_stale) || in_flight_current {
            return;
        }

        self.request_diff_index_refresh(scope_root);
    }

    fn apply_live_feed_capture_update(
        &mut self,
        project_id: &str,
        scope_root: PathBuf,
        generation: u64,
        request_revision: u64,
        result: Result<FeedCaptureResult, String>,
    ) {
        let mut entry_to_append = None;
        let mut should_request_refresh = false;

        {
            let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
                return;
            };
            let Some(scope_state) = feed.tracked_scopes.get_mut(&scope_root) else {
                return;
            };
            if request_revision < scope_state.latest_request_revision {
                return;
            }

            scope_state.pending_refresh = false;

            match result {
                Ok(result) => {
                    let FeedCaptureResult {
                        current_capture,
                        event,
                    } = result;
                    scope_state.latest_error = None;
                    scope_state.error_revision = 0;
                    scope_state.last_generation = Some(generation);
                    scope_state.emitted_baseline = Some(Arc::new(current_capture));
                    if let Some(event) = event {
                        if matches!(event.kind, FeedEventKind::BootstrapSnapshot) {
                            scope_state.bootstrap_emitted = true;
                        }
                        entry_to_append = Some(event);
                    }

                    should_request_refresh = scope_state
                        .latest_snapshot_generation
                        .is_some_and(|latest| latest > generation);
                }
                Err(message) => {
                    scope_state.latest_error =
                        Some(format!("{}: {}", scope_root.display(), message));
                    scope_state.error_revision = feed.next_error_revision;
                    feed.next_error_revision += 1;
                }
            }
        }

        if let Some(event) = entry_to_append.as_ref() {
            self.append_live_feed_entry(project_id, &scope_root, generation, event);
        }
        if should_request_refresh {
            self.request_live_feed_capture(project_id, &scope_root);
        }
    }

    fn append_live_feed_entry(
        &mut self,
        project_id: &str,
        scope_root: &Path,
        generation: u64,
        event: &FeedEventData,
    ) {
        let Some(branch_name) = self
            .git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.branch_name.clone())
        else {
            return;
        };
        let (scope_kind, worktree_name, source_terminal_id) =
            self.resolve_feed_scope_metadata(scope_root);
        let mut pruned_ids = Vec::new();
        let Some(feed) = self.live_diff_feeds.get_mut(project_id) else {
            return;
        };
        let entry_id = feed.next_entry_id;
        feed.next_entry_id += 1;
        feed.entries.push_back(ChangeFeedEntry {
            id: entry_id,
            observed_at: SystemTime::now(),
            origin: Self::feed_origin_from_kind(event.kind),
            scope_root: scope_root.to_path_buf(),
            branch_name,
            scope_kind,
            worktree_name,
            source_terminal_id,
            generation,
            changed_file_count: event.changed_file_count,
            insertions: event.insertions,
            deletions: event.deletions,
            files: event.files.clone(),
            capture_state: Self::capture_state_from_event(event.capture.clone()),
        });
        if !feed.live_follow {
            feed.unread_count += 1;
        }

        while feed.entries.len() > FEED_ENTRY_RETENTION_CAP {
            if let Some(pruned) = feed.entries.pop_front() {
                pruned_ids.push(pruned.id);
                if feed.selected_entry_id == Some(pruned.id) {
                    feed.selected_entry_id = None;
                    feed.detail_pane_open = false;
                }
            }
        }

        for pruned_id in pruned_ids {
            self.cancel_feed_capture_target(project_id, pruned_id);
        }
    }

    fn resolve_feed_scope_metadata(
        &self,
        scope_root: &Path,
    ) -> (FeedScopeKind, Option<String>, Option<String>) {
        let stored = self
            .services
            .store
            .lock()
            .as_ref()
            .and_then(|store| store.find_worktree_by_path(scope_root).ok().flatten());

        if let Some(stored) = stored {
            (
                FeedScopeKind::ManagedWorktree,
                Some(stored.worktree_name),
                stored.primary_terminal_id,
            )
        } else {
            (FeedScopeKind::ProjectRoot, None, None)
        }
    }

    fn feed_origin_from_kind(kind: FeedEventKind) -> FeedEntryOrigin {
        match kind {
            FeedEventKind::BootstrapSnapshot => FeedEntryOrigin::BootstrapSnapshot,
            FeedEventKind::LiveDelta => FeedEntryOrigin::LiveDelta,
        }
    }

    fn capture_state_from_event(captured: CapturedEventDiff) -> FeedCaptureState {
        if captured.files.is_empty() && !captured.failed_files.is_empty() {
            FeedCaptureState::Failed {
                message: format!(
                    "Could not capture diffs for {} file(s).",
                    captured.failed_files.len()
                ),
            }
        } else if captured.truncated {
            FeedCaptureState::Truncated(captured)
        } else {
            FeedCaptureState::Ready(captured)
        }
    }

    pub(crate) fn build_feed_preview_layout(captured: &CapturedEventDiff) -> FeedPreviewLayout {
        let previewable_files = captured
            .files
            .iter()
            .filter_map(|file| {
                if file.file.is_binary {
                    return None;
                }

                let preview_lines = file
                    .document
                    .lines
                    .iter()
                    .filter(|line| include_preview_line(line.kind))
                    .cloned()
                    .collect::<Vec<_>>();

                if preview_lines.is_empty() {
                    None
                } else {
                    Some((file, preview_lines))
                }
            })
            .collect::<Vec<_>>();
        let shown = previewable_files
            .iter()
            .take(FEED_PREVIEW_FILE_CAP)
            .collect::<Vec<_>>();
        if shown.is_empty() {
            return FeedPreviewLayout {
                files: Vec::new(),
                hidden_file_count: 0,
                hidden_file_names: Vec::new(),
            };
        }

        let hidden_files = previewable_files
            .iter()
            .skip(FEED_PREVIEW_FILE_CAP)
            .collect::<Vec<_>>();
        let available_per_file = shown
            .iter()
            .map(|(_, lines)| lines.len())
            .collect::<Vec<_>>();
        let line_budget = available_per_file
            .iter()
            .sum::<usize>()
            .min(FEED_PREVIEW_LINE_BUDGET);
        let allocations = Self::allocate_feed_preview_lines(&available_per_file, line_budget);

        FeedPreviewLayout {
            files: shown
                .into_iter()
                .zip(allocations)
                .map(|((file, lines), line_count)| FeedPreviewFile {
                    selection: file.selection.clone(),
                    relative_path: file.file.relative_path.clone(),
                    lines: lines.iter().take(line_count).cloned().collect(),
                })
                .collect(),
            hidden_file_count: hidden_files.len(),
            hidden_file_names: hidden_files
                .into_iter()
                .take(FEED_PREVIEW_HIDDEN_NAME_CAP)
                .map(|(file, _)| file.file.relative_path.clone())
                .collect(),
        }
    }

    #[allow(dead_code)]
    fn allocate_feed_preview_lines(
        available_per_file: &[usize],
        total_budget: usize,
    ) -> Vec<usize> {
        if available_per_file.is_empty() || total_budget == 0 {
            return vec![0; available_per_file.len()];
        }

        let mut allocations = vec![0; available_per_file.len()];
        let mut remaining_budget = total_budget;

        if total_budget >= available_per_file.len() * FEED_PREVIEW_MIN_LINES_PER_FILE {
            for (allocation, available) in allocations.iter_mut().zip(available_per_file) {
                let granted = (*available).min(FEED_PREVIEW_MIN_LINES_PER_FILE);
                *allocation = granted;
                remaining_budget = remaining_budget.saturating_sub(granted);
            }
        }

        while remaining_budget > 0 {
            let mut granted_any = false;
            for (allocation, available) in allocations.iter_mut().zip(available_per_file) {
                if remaining_budget == 0 {
                    break;
                }
                if *allocation < *available {
                    *allocation += 1;
                    remaining_budget -= 1;
                    granted_any = true;
                }
            }

            if !granted_any {
                break;
            }
        }

        allocations
    }

    fn mark_diff_scope_unavailable(&mut self, scope_root: &Path, message: String) {
        let Some(diff_tab) = self.diff_tabs.get_mut(scope_root) else {
            return;
        };
        diff_tab.index.loading = false;
        diff_tab.index.error = Some(message);
        diff_tab.file.loading = false;
    }

    fn activate_terminal_content(&mut self) {
        self.active_auxiliary_tab_id = None;
    }

    fn attach_terminal_scope(&mut self, terminal_id: &str, scope_root: PathBuf) {
        let project_id = self
            .project_id_for_terminal(terminal_id)
            .map(str::to_string);
        let already_attached = self
            .terminal_git_scopes
            .get(terminal_id)
            .is_some_and(|existing| *existing == scope_root);
        if already_attached {
            return;
        }

        if let Some(previous_scope) = self
            .terminal_git_scopes
            .insert(terminal_id.to_string(), scope_root.clone())
        {
            self.services.git.unsubscribe(&previous_scope);
            if !self
                .terminal_git_scopes
                .values()
                .any(|candidate| *candidate == previous_scope)
            {
                self.git_scopes.remove(&previous_scope);
            }
        }

        self.services.git.subscribe(&scope_root);
        if let Some(project_id) = project_id {
            self.refresh_live_diff_feed_scope_membership(&project_id);
        }
    }

    fn terminal_exists(&self, terminal_id: &str) -> bool {
        self.terminal_views.contains_key(terminal_id)
            && self
                .projects
                .iter()
                .any(|project| project.layout.find_terminal_path(terminal_id).is_some())
    }

    fn detach_scope(&mut self, scope_root: &Path) {
        let scope_root = scope_root.to_path_buf();
        let terminal_ids: Vec<String> = self
            .terminal_git_scopes
            .iter()
            .filter(|(_, candidate_scope)| candidate_scope.as_path() == scope_root.as_path())
            .map(|(terminal_id, _)| terminal_id.clone())
            .collect();

        for terminal_id in terminal_ids {
            self.detach_terminal_scope(&terminal_id);
        }
        self.git_scopes.remove(&scope_root);
    }

    fn detach_terminal_scope(&mut self, terminal_id: &str) {
        let project_id = self
            .project_id_for_terminal(terminal_id)
            .map(str::to_string);
        let Some(scope_root) = self.terminal_git_scopes.remove(terminal_id) else {
            return;
        };

        self.services.git.unsubscribe(&scope_root);
        if !self
            .terminal_git_scopes
            .values()
            .any(|candidate| *candidate == scope_root)
        {
            self.git_scopes.remove(&scope_root);
        }
        if let Some(project_id) = project_id {
            self.refresh_live_diff_feed_scope_membership(&project_id);
        }
    }

    fn terminal_working_directory(
        &self,
        project_id: &str,
        terminal_id: &str,
        cx: &App,
    ) -> Option<PathBuf> {
        if let Some(view) = self.terminal_view(terminal_id) {
            if let Some(cwd) = view.read(cx).engine().cwd() {
                return Some(cwd);
            }
        }

        let project = self.project(project_id)?;
        let layout_path = project.layout.find_terminal_path(terminal_id)?;
        if let Some(LayoutNode::Terminal {
            working_directory, ..
        }) = project.layout.get_at_path(&layout_path)
        {
            if let Some(cwd) = working_directory.clone() {
                return Some(cwd);
            }
        }

        Some(project.path.clone())
    }

    fn project_id_for_terminal(&self, terminal_id: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|project| project.layout.find_terminal_path(terminal_id).is_some())
            .map(|project| project.id.as_str())
    }

    fn detect_resumable_agent(title: Option<&str>) -> Option<ResumableAgentKind> {
        let first = title?.split_whitespace().next()?;
        match first {
            "codex" => Some(ResumableAgentKind::Codex),
            "claude" => Some(ResumableAgentKind::ClaudeCode),
            _ => None,
        }
    }

    fn resume_command(agent_kind: ResumableAgentKind) -> &'static str {
        match agent_kind {
            ResumableAgentKind::Codex => "codex resume --last\n",
            ResumableAgentKind::ClaudeCode => "claude --continue\n",
        }
    }

    fn semantic_state_is_prompt_ready(state: SemanticState) -> bool {
        matches!(
            state,
            SemanticState::Prompt | SemanticState::Input | SemanticState::CommandComplete { .. }
        )
    }

    fn arm_resumable_agent(&mut self, terminal_id: &str, agent_kind: ResumableAgentKind, cx: &App) {
        let Some(project_id) = self
            .project_id_for_terminal(terminal_id)
            .map(str::to_string)
        else {
            return;
        };
        let Some(runtime) = self.terminal_runtime.get_mut(terminal_id) else {
            return;
        };
        if runtime.resumable_agent.is_some() {
            runtime.pending_agent_detection = false;
            return;
        }
        runtime.resumable_agent = Some(agent_kind);
        runtime.pending_agent_detection = false;

        let Some(cwd) = self.terminal_working_directory(&project_id, terminal_id, cx) else {
            tracing::warn!(
                "Failed to determine cwd for resumable terminal {} in project {}",
                terminal_id,
                project_id
            );
            return;
        };

        let row = StoredAgentTerminal {
            terminal_id: terminal_id.to_string(),
            project_id,
            agent_kind,
            cwd,
            updated_at: String::new(),
        };
        if let Some(store) = self.services.store.lock().as_mut() {
            if let Err(err) = store.upsert_agent_terminal(&row) {
                tracing::warn!(
                    "Failed to persist resumable terminal {}: {}",
                    terminal_id,
                    err
                );
            }
        }
    }

    fn disarm_resumable_agent(&mut self, terminal_id: &str) {
        self.clear_pending_resume_injection(terminal_id);
        let Some(runtime) = self.terminal_runtime.get_mut(terminal_id) else {
            return;
        };
        let had_agent = runtime.resumable_agent.take().is_some();
        runtime.pending_agent_detection = false;
        if had_agent {
            if let Some(store) = self.services.store.lock().as_mut() {
                if let Err(err) = store.delete_agent_terminal(terminal_id) {
                    tracing::warn!(
                        "Failed to clear resumable terminal {}: {}",
                        terminal_id,
                        err
                    );
                }
            }
        }
    }

    fn maybe_arm_resumable_agent_from_title(&mut self, terminal_id: &str, cx: &App) {
        let candidate = self
            .terminal_runtime
            .get(terminal_id)
            .filter(|runtime| {
                runtime.semantic_state == SemanticState::Executing
                    && runtime.pending_agent_detection
                    && runtime.resumable_agent.is_none()
            })
            .and_then(|runtime| Self::detect_resumable_agent(runtime.live_title.as_deref()));
        if let Some(agent_kind) = candidate {
            self.arm_resumable_agent(terminal_id, agent_kind, cx);
        }
    }

    fn clear_pending_resume_injection(&mut self, terminal_id: &str) {
        self.pending_resume_injections.remove(terminal_id);
        self.pending_resume_timeout_tasks.remove(terminal_id);
    }

    fn cancel_resume_injection(
        &mut self,
        terminal_id: &str,
        warning: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_resume_injections
            .get(terminal_id)
            .is_some_and(|pending| !pending.resume_attempted)
        {
            self.clear_pending_resume_injection(terminal_id);
            self.set_workspace_warning(warning, cx);
        }
    }

    fn queue_resume_injection(
        &mut self,
        terminal_id: &str,
        agent_kind: ResumableAgentKind,
        cx: &mut Context<Self>,
    ) {
        let pending = PendingResumeInjection {
            terminal_id: terminal_id.to_string(),
            agent_kind,
            command: Self::resume_command(agent_kind),
            resume_attempted: false,
        };
        self.pending_resume_injections
            .insert(terminal_id.to_string(), pending);

        let terminal_id = terminal_id.to_string();
        let timeout_terminal_id = terminal_id.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            Timer::after(Duration::from_secs(5)).await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.attempt_resume_injection(
                    &timeout_terminal_id,
                    ResumeInjectionTrigger::TimeoutFallback,
                    cx,
                );
            });
        });
        self.pending_resume_timeout_tasks.insert(terminal_id, task);
    }

    fn should_queue_resume_injection(
        row: &StoredAgentTerminal,
        restored_cwd: &Path,
        winning_terminal_ids: &HashSet<String>,
    ) -> bool {
        winning_terminal_ids.contains(&row.terminal_id) && restored_cwd == row.cwd
    }

    fn prepare_resume_injection_attempt(
        &mut self,
        terminal_id: &str,
        trigger: ResumeInjectionTrigger,
    ) -> Option<PreparedResumeInjection> {
        let pending = self.pending_resume_injections.get_mut(terminal_id)?;
        if pending.resume_attempted {
            return None;
        }
        pending.resume_attempted = true;
        Some(PreparedResumeInjection {
            terminal_id: pending.terminal_id.clone(),
            agent_kind: pending.agent_kind,
            command: pending.command,
            trigger,
        })
    }

    fn finish_resume_injection_attempt(
        &mut self,
        prepared: PreparedResumeInjection,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        self.clear_pending_resume_injection(&prepared.terminal_id);
        match result {
            Ok(()) => {
                self.mark_resume_injection_succeeded(&prepared);
                if prepared.trigger == ResumeInjectionTrigger::TimeoutFallback {
                    self.set_workspace_warning(
                        "Agent resume used the timeout fallback before shell prompt markers arrived.",
                        cx,
                    );
                }
            }
            Err(err) => {
                self.set_workspace_error(
                    format!(
                        "Failed to inject agent resume command for terminal {}: {}",
                        prepared.terminal_id, err
                    ),
                    cx,
                );
            }
        }
    }

    fn attempt_resume_injection(
        &mut self,
        terminal_id: &str,
        trigger: ResumeInjectionTrigger,
        cx: &mut Context<Self>,
    ) {
        let Some(prepared) = self.prepare_resume_injection_attempt(terminal_id, trigger) else {
            return;
        };

        let result = self
            .terminal_view(&prepared.terminal_id)
            .map(|view| {
                view.read(cx)
                    .engine()
                    .try_write(prepared.command.as_bytes())
            })
            .unwrap_or_else(|| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "terminal session is no longer available",
                ))
            });

        self.finish_resume_injection_attempt(prepared, result, cx);
    }

    fn mark_resume_injection_succeeded(&mut self, prepared: &PreparedResumeInjection) {
        if let Some(runtime) = self.terminal_runtime.get_mut(&prepared.terminal_id) {
            runtime.resumable_agent = Some(prepared.agent_kind);
            runtime.pending_agent_detection = false;
        }
    }

    fn build_project_resume_restore_plan(&self, project_id: &str) -> ProjectResumeRestorePlan {
        let store_guard = self.services.store.lock();
        let Some(store) = store_guard.as_ref() else {
            return ProjectResumeRestorePlan::default();
        };
        let rows = match store.load_agent_terminals_for_project(project_id) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(
                    "Failed to load resumable terminals for project {}: {}",
                    project_id,
                    err
                );
                return ProjectResumeRestorePlan::default();
            }
        };

        let mut rows_by_terminal_id = HashMap::new();
        let mut winning_terminal_ids = HashSet::new();
        let mut seen_groups = HashSet::new();
        let mut suppressed_duplicates = 0;

        for row in rows {
            let group_key = (row.agent_kind, row.cwd.clone());
            if seen_groups.insert(group_key) {
                winning_terminal_ids.insert(row.terminal_id.clone());
            } else {
                suppressed_duplicates += 1;
            }
            rows_by_terminal_id.insert(row.terminal_id.clone(), row);
        }

        ProjectResumeRestorePlan {
            rows_by_terminal_id,
            winning_terminal_ids,
            suppressed_duplicates,
        }
    }

    fn restored_terminal_cwd(project_path: &Path, working_directory: Option<&PathBuf>) -> PathBuf {
        working_directory
            .filter(|path| path.exists())
            .cloned()
            .unwrap_or_else(|| project_path.to_path_buf())
    }

    fn open_terminal_tab_in_project(
        &mut self,
        project_id: &str,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        self.activate_terminal_content();

        let new_terminal_id = Self::generate_terminal_id();
        let insert_index = {
            let project = self
                .project_mut(project_id)
                .ok_or_else(|| format!("unknown project {project_id}"))?;
            let LayoutNode::Tabs {
                children,
                active_tab,
            } = &mut project.layout
            else {
                return Err(format!("project {project_id} has no root tab container"));
            };

            children.push(LayoutNode::Terminal {
                terminal_id: Some(new_terminal_id.clone()),
                working_directory: Some(cwd.clone()),
                zoom_level: None,
            });
            *active_tab = children.len() - 1;
            children.len() - 1
        };

        if !self.spawn_session(&new_terminal_id, Some(&cwd), cx) {
            if let Some(project) = self.project_mut(project_id) {
                project.layout.remove_at_path(&[insert_index]);
                project.layout.normalize();
                Self::enforce_root_tabs(project);
            }
            return Err(format!(
                "Failed to open a new terminal tab in {}.",
                cwd.display()
            ));
        }

        self.active_project_id = Some(project_id.to_string());
        self.pending_focus_terminal_id = Some(new_terminal_id.clone());
        self.set_focus(FocusTarget {
            project_id: project_id.to_string(),
            layout_path: vec![insert_index],
        });
        cx.notify();
        Ok(new_terminal_id)
    }

    /// Build terminal configuration from settings + theme tokens.
    pub fn build_terminal_config(settings: &AppSettings, theme: &OrcaTheme) -> TerminalConfig {
        let (bg_r, bg_g, bg_b) = theme::rgb_channels(theme.TERMINAL_BACKGROUND);
        let (fg_r, fg_g, fg_b) = theme::rgb_channels(theme.TERMINAL_FOREGROUND);
        let (cursor_r, cursor_g, cursor_b) = theme::rgb_channels(theme.TERMINAL_CURSOR);
        let (terminated_r, terminated_g, terminated_b) = theme::rgb_channels(theme.FOG);

        let mut palette = ColorPalette::builder()
            .background(bg_r, bg_g, bg_b)
            .foreground(fg_r, fg_g, fg_b)
            .cursor(cursor_r, cursor_g, cursor_b)
            .black_channels(theme.ANSI[0])
            .red_channels(theme.ANSI[1])
            .green_channels(theme.ANSI[2])
            .yellow_channels(theme.ANSI[3])
            .blue_channels(theme.ANSI[4])
            .magenta_channels(theme.ANSI[5])
            .cyan_channels(theme.ANSI[6])
            .white_channels(theme.ANSI[7])
            .bright_black_channels(theme.ANSI[8])
            .bright_red_channels(theme.ANSI[9])
            .bright_green_channels(theme.ANSI[10])
            .bright_yellow_channels(theme.ANSI[11])
            .bright_blue_channels(theme.ANSI[12])
            .bright_magenta_channels(theme.ANSI[13])
            .bright_cyan_channels(theme.ANSI[14])
            .bright_white_channels(theme.ANSI[15])
            .build();
        palette.search_match_active = rgba(theme::with_alpha(theme.ORCA_BLUE, 0x66)).into();
        palette.search_match_other = rgba(theme::with_alpha(theme.ORCA_BLUE, 0x2E)).into();
        palette.scrollbar = rgba(theme::with_alpha(theme.ORCA_BLUE, 0x4D)).into();
        palette.search_bar_bg = rgb(theme.SURFACE).into();
        palette.search_bar_border = rgba(theme::with_alpha(theme.ORCA_BLUE, 0x40)).into();
        palette.search_bar_text = rgb(theme.FOG).into();
        palette.search_input_bg = rgb(theme.ABYSS).into();
        palette.search_input_text = rgb(theme.BONE).into();
        palette.search_input_placeholder = rgba(theme::with_alpha(theme.SLATE, 0x80)).into();
        palette.search_input_cursor = rgb(theme.ORCA_BLUE).into();
        palette.search_input_selection =
            rgba(theme::with_alpha(theme.TERMINAL_SELECTION, 0x40)).into();
        palette.terminal_selection = rgba(theme::with_alpha(theme.TERMINAL_SELECTION, 0x40)).into();
        let hover_base = if theme.TERMINAL_BACKGROUND == theme.DEEP {
            theme.PATCH
        } else {
            theme.SLATE
        };
        palette.hover_overlay = rgba(theme::with_alpha(hover_base, 0x10)).into();
        palette.close_hover_overlay = rgba(theme::with_alpha(theme.STATUS_CORAL, 0x40)).into();
        palette.link = rgb(theme.ORCA_BLUE).into();

        let cursor_shape = match settings.cursor_style {
            orcashell_store::CursorStyle::Block => CursorShape::Block,
            orcashell_store::CursorStyle::Bar => CursorShape::Bar,
            orcashell_store::CursorStyle::Underline => CursorShape::Underline,
        };

        TerminalConfig {
            font_family: settings.font_family.clone(),
            font_size: px(settings.font_size),
            line_height_multiplier: 1.0,
            padding: Edges::all(px(4.0)),
            colors: palette,
            terminated_text_color: Rgba {
                r: terminated_r as f32 / 255.0,
                g: terminated_g as f32 / 255.0,
                b: terminated_b as f32 / 255.0,
                a: 1.0,
            }
            .into(),
            cursor_shape,
            cursor_blink: settings.cursor_blink,
        }
    }

    pub fn refresh_diff_theme(&mut self, _cx: &mut Context<Self>) {
        let active_theme = theme::active_selection(_cx).resolved_id;
        self.refresh_diff_theme_for(active_theme);
    }

    fn refresh_diff_theme_for(&mut self, active_theme: ThemeId) {
        self.services.git.set_diff_theme(active_theme);
        self.refresh_live_feed_theme(active_theme);

        let scopes: Vec<PathBuf> = self.diff_tabs.keys().cloned().collect();
        for scope_root in scopes {
            let selected = self
                .diff_tabs
                .get(&scope_root)
                .and_then(|tab| tab.selected_file.clone());
            let selected_stash_file = self
                .diff_tabs
                .get(&scope_root)
                .and_then(|tab| tab.stash.selected_file.clone());
            if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
                diff_tab.file.document = None;
                diff_tab.file.error = None;
                diff_tab.file.loading = false;
                diff_tab.file.requested_generation = None;
                diff_tab.file.requested_selection = None;
                diff_tab.stash.file_diff.document = None;
                diff_tab.stash.file_diff.error = None;
                diff_tab.stash.file_diff.loading = false;
            }
            if let Some(selection) = selected {
                self.request_selected_file_diff(&scope_root, selection);
            }
            if let Some(selection) = selected_stash_file {
                self.request_stash_file_diff(&scope_root, selection);
            }
        }

        let repository_refreshes: Vec<(String, CommitFileSelection)> = self
            .repository_graph_tabs
            .iter()
            .filter_map(|(project_id, tab)| {
                tab.selected_commit_file
                    .clone()
                    .map(|selection| (project_id.clone(), selection))
            })
            .collect();
        for (project_id, selection) in repository_refreshes {
            if let Some(tab) = self.repository_graph_tabs.get_mut(&project_id) {
                tab.commit_file_diff.document = None;
                tab.commit_file_diff.error = None;
                tab.commit_file_diff.loading = false;
            }
            self.request_repository_commit_file_diff(&project_id, selection);
        }
    }

    fn refresh_live_feed_theme(&mut self, active_theme: ThemeId) {
        for feed in self.live_diff_feeds.values_mut() {
            for entry in &mut feed.entries {
                match &mut entry.capture_state {
                    FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) => {
                        rehighlight_captured_feed_event(captured, active_theme);
                    }
                    FeedCaptureState::Pending | FeedCaptureState::Failed { .. } => {}
                }
            }
        }
    }

    fn attach_terminal_view(
        &mut self,
        terminal_id: String,
        shell_label: String,
        shell_type: orcashell_session::ShellType,
        engine: SessionEngine,
        config: TerminalConfig,
        cx: &mut Context<Self>,
    ) {
        let view_terminal_id = terminal_id.clone();
        let view = cx.new(|cx| TerminalView::new(view_terminal_id, shell_type, engine, config, cx));
        self.set_terminal_runtime_state(
            terminal_id.clone(),
            TerminalRuntimeState {
                shell_label,
                live_title: None,
                semantic_state: SemanticState::Unknown,
                last_activity_at: None,
                last_local_input_at: None,
                notification_tier: None,
                resumable_agent: None,
                pending_agent_detection: false,
            },
        );
        cx.subscribe(&view, |this, _view, event: &TerminalRuntimeEvent, cx| {
            this.handle_terminal_runtime_event(event, cx);
        })
        .detach();
        self.terminal_views.insert(terminal_id, view);
    }

    fn sync_terminal_git_scope(&mut self, terminal_id: &str, cwd: Option<PathBuf>) {
        if let Some(cwd) = cwd {
            self.services.git.request_snapshot(&cwd, Some(terminal_id));
        }
    }

    fn refresh_terminal_git_snapshot(&mut self, terminal_id: &str, cx: &App) {
        let cwd = self
            .terminal_view(terminal_id)
            .and_then(|view| view.read(cx).engine().cwd());
        self.sync_terminal_git_scope(terminal_id, cwd);
    }

    fn apply_semantic_state_change(
        &mut self,
        terminal_id: &str,
        state: SemanticState,
    ) -> SemanticTransition {
        let Some(runtime) = self.terminal_runtime.get_mut(terminal_id) else {
            return SemanticTransition {
                changed: false,
                refresh_git_snapshot: false,
                entered_executing: false,
                left_executing: false,
                prompt_ready: false,
            };
        };

        if runtime.semantic_state == state {
            return SemanticTransition {
                changed: false,
                refresh_git_snapshot: false,
                entered_executing: false,
                left_executing: false,
                prompt_ready: false,
            };
        }

        let previous_state = runtime.semantic_state;
        runtime.semantic_state = state;
        runtime.last_activity_at = match state {
            SemanticState::Executing => Some(Instant::now()),
            _ => None,
        };
        if state != SemanticState::Executing {
            runtime.last_local_input_at = None;
        }
        let entered_executing =
            previous_state != SemanticState::Executing && state == SemanticState::Executing;
        let left_executing =
            previous_state == SemanticState::Executing && state != SemanticState::Executing;
        if entered_executing {
            runtime.pending_agent_detection = true;
            runtime.resumable_agent = None;
        }

        SemanticTransition {
            changed: true,
            refresh_git_snapshot: left_executing,
            entered_executing,
            left_executing,
            prompt_ready: Self::semantic_state_is_prompt_ready(state),
        }
    }

    fn handle_terminal_runtime_event(
        &mut self,
        event: &TerminalRuntimeEvent,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.global::<AppSettings>();
        let activity_pulse = settings.activity_pulse;
        let agent_notifications = settings.agent_notifications;
        let changed = match event {
            // Activity/LocalInput gated by activity_pulse setting.
            TerminalRuntimeEvent::ActivityChanged { .. }
            | TerminalRuntimeEvent::LocalInput { .. }
                if !activity_pulse =>
            {
                false
            }
            TerminalRuntimeEvent::ActivityChanged { terminal_id } => self
                .terminal_runtime
                .get_mut(terminal_id)
                .is_some_and(|runtime| {
                    let now = Instant::now();
                    let was_recent = runtime
                        .last_activity_at
                        .is_some_and(|at| now.duration_since(at) < ACTIVITY_PULSE_WINDOW);
                    runtime.last_activity_at = Some(now);
                    !was_recent && runtime.semantic_state == SemanticState::Executing
                }),
            TerminalRuntimeEvent::LocalInput { terminal_id } => self
                .terminal_runtime
                .get_mut(terminal_id)
                .is_some_and(|runtime| {
                    let now = Instant::now();
                    let was_recent = runtime
                        .last_local_input_at
                        .is_some_and(|at| now.duration_since(at) < LOCAL_INPUT_SUPPRESS_WINDOW);
                    runtime.last_local_input_at = Some(now);
                    !was_recent
                }),
            TerminalRuntimeEvent::TitleChanged { terminal_id, title } => self
                .terminal_runtime
                .get_mut(terminal_id)
                .is_some_and(|runtime| {
                    if runtime.live_title != *title {
                        runtime.live_title = title.clone();
                        true
                    } else {
                        false
                    }
                }),
            TerminalRuntimeEvent::SemanticStateChanged { terminal_id, state } => {
                let transition = self.apply_semantic_state_change(terminal_id, *state);
                if transition.refresh_git_snapshot {
                    self.refresh_terminal_git_snapshot(terminal_id, cx);
                }
                if transition.entered_executing {
                    self.maybe_arm_resumable_agent_from_title(terminal_id, cx);
                }
                if transition.left_executing {
                    self.disarm_resumable_agent(terminal_id);
                } else if transition.prompt_ready {
                    self.attempt_resume_injection(
                        terminal_id,
                        ResumeInjectionTrigger::PromptReady,
                        cx,
                    );
                }
                transition.changed
            }
            // Notification/Bell: skip if disabled or terminal is already focused.
            TerminalRuntimeEvent::Notification { terminal_id, .. }
            | TerminalRuntimeEvent::Bell { terminal_id }
                if !agent_notifications || self.is_terminal_focused(terminal_id) =>
            {
                false
            }
            TerminalRuntimeEvent::Notification {
                terminal_id,
                title,
                body,
            } => {
                let patterns = cx
                    .global::<AppSettings>()
                    .notification_urgent_patterns
                    .clone();
                self.terminal_runtime
                    .get_mut(terminal_id)
                    .is_some_and(|runtime| {
                        let tier = classify_notification(title, body, &patterns);
                        // Never downgrade Urgent → Informational.
                        if runtime.notification_tier == Some(NotificationTier::Urgent)
                            && tier == NotificationTier::Informational
                        {
                            return false;
                        }
                        let changed = runtime.notification_tier != Some(tier);
                        runtime.notification_tier = Some(tier);
                        changed
                    })
            }
            TerminalRuntimeEvent::Bell { terminal_id } => self
                .terminal_runtime
                .get_mut(terminal_id)
                .is_some_and(|runtime| {
                    if runtime.notification_tier.is_none() {
                        runtime.notification_tier = Some(NotificationTier::Informational);
                        true
                    } else {
                        false
                    }
                }),
        };

        if let TerminalRuntimeEvent::LocalInput { terminal_id } = event {
            self.cancel_resume_injection(
                terminal_id,
                "Agent resume was canceled because input arrived before restore could safely attach.",
                cx,
            );
        } else if let TerminalRuntimeEvent::TitleChanged { terminal_id, .. } = event {
            self.maybe_arm_resumable_agent_from_title(terminal_id, cx);
        }

        if changed {
            cx.notify();
        }
    }

    /// Extract a short shell label from a resolved shell path.
    fn extract_shell_label(resolved_shell: &str) -> String {
        resolved_shell
            .rsplit(['/', '\\'])
            .next()
            .map(|name| {
                let lower = name.to_ascii_lowercase();
                lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
            })
            .unwrap_or_else(|| "shell".to_string())
    }

    pub(crate) fn set_terminal_runtime_state(
        &mut self,
        terminal_id: String,
        state: TerminalRuntimeState,
    ) {
        self.terminal_runtime.insert(terminal_id, state);
    }

    fn automatic_terminal_label(&self, project: Option<&ProjectData>, terminal_id: &str) -> String {
        let Some(runtime) = self.terminal_runtime.get(terminal_id) else {
            return "terminal".to_string();
        };

        let Some(title) = runtime.live_title.as_deref() else {
            return "terminal".to_string();
        };

        if runtime.semantic_state == SemanticState::Executing {
            return title.to_string();
        }

        let matches_project_name = project.is_some_and(|project| {
            title == project.name
                || project
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| title == name)
        });

        if matches_project_name {
            "terminal".to_string()
        } else {
            title.to_string()
        }
    }

    pub fn terminal_display_name(&self, project_id: &str, terminal_id: &str) -> String {
        let project = self.project(project_id);
        project
            .and_then(|project| project.custom_terminal_name(terminal_id))
            .map(str::to_string)
            .unwrap_or_else(|| self.automatic_terminal_label(project, terminal_id))
    }

    pub fn terminal_is_executing(&self, terminal_id: &str) -> bool {
        self.terminal_runtime
            .get(terminal_id)
            .is_some_and(|runtime| runtime.semantic_state == SemanticState::Executing)
    }

    pub fn terminal_should_pulse(&self, terminal_id: &str) -> bool {
        let now = Instant::now();
        self.terminal_runtime
            .get(terminal_id)
            .is_some_and(|runtime| {
                runtime.semantic_state == SemanticState::Executing
                    && runtime
                        .last_activity_at
                        .is_some_and(|at| now.duration_since(at) < ACTIVITY_PULSE_WINDOW)
                    && runtime
                        .last_local_input_at
                        .is_none_or(|at| now.duration_since(at) >= LOCAL_INPUT_SUPPRESS_WINDOW)
            })
    }

    pub fn has_pulsing_terminals(&self) -> bool {
        let now = Instant::now();
        self.terminal_runtime.values().any(|runtime| {
            runtime.semantic_state == SemanticState::Executing
                && runtime
                    .last_activity_at
                    .is_some_and(|at| now.duration_since(at) < ACTIVITY_PULSE_WINDOW)
                && runtime
                    .last_local_input_at
                    .is_none_or(|at| now.duration_since(at) >= LOCAL_INPUT_SUPPRESS_WINDOW)
        })
    }

    fn is_terminal_focused(&self, terminal_id: &str) -> bool {
        self.focus.current_target().is_some_and(|target| {
            self.project(&target.project_id)
                .and_then(|p| p.layout.get_at_path(&target.layout_path))
                .is_some_and(|node| {
                    matches!(node, LayoutNode::Terminal { terminal_id: Some(tid), .. } if tid == terminal_id)
                })
        })
    }

    pub fn terminal_notification_tier(&self, terminal_id: &str) -> Option<NotificationTier> {
        self.terminal_runtime
            .get(terminal_id)
            .and_then(|r| r.notification_tier)
    }

    pub fn has_notifying_terminals(&self) -> bool {
        self.terminal_runtime
            .values()
            .any(|r| r.notification_tier.is_some())
    }

    /// Set focus and clear notification for the focused terminal.
    /// All focus changes should go through this method.
    fn set_focus(&mut self, target: FocusTarget) {
        // Extract terminal_id first (immutable borrow), then mutate runtime.
        let tid = self
            .project(&target.project_id)
            .and_then(|project| project.layout.get_at_path(&target.layout_path))
            .and_then(|node| match node {
                LayoutNode::Terminal {
                    terminal_id: Some(tid),
                    ..
                } => Some(tid.clone()),
                _ => None,
            });
        if let Some(tid) = tid {
            if let Some(runtime) = self.terminal_runtime.get_mut(&tid) {
                runtime.notification_tier = None;
            }
            if let Some(scope_root) = self.terminal_git_scopes.get(&tid).cloned() {
                self.services.git.request_snapshot(&scope_root, Some(&tid));
            }
        }
        self.focus.set_current(target);
    }

    /// Set the active tab index on a Tabs node at the given path.
    pub fn set_active_tab(
        &mut self,
        project_id: &str,
        layout_path: &[usize],
        tab_index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project_mut(project_id) {
            if let Some(LayoutNode::Tabs {
                active_tab,
                children,
            }) = project.layout.get_at_path_mut(layout_path)
            {
                if tab_index < children.len() {
                    *active_tab = tab_index;
                }
            }
        }
        cx.notify();
    }

    /// Split the focused pane in the given direction.
    pub fn split_focused(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        self.activate_terminal_content();
        let (project_id, focus_path) = match self.focus.current_target() {
            Some(t) => (t.project_id.clone(), t.layout_path.clone()),
            None => return,
        };
        let project_cwd = self.project(&project_id).map(|p| p.path.clone());

        let project = match self.project_mut(&project_id) {
            Some(p) => p,
            None => return,
        };

        let node = match project.layout.get_at_path_mut(&focus_path) {
            Some(n) => n,
            None => return,
        };

        // Only split Terminal nodes
        if !matches!(node, LayoutNode::Terminal { .. }) {
            return;
        }

        let new_id = Self::generate_terminal_id();
        let old_node = std::mem::replace(node, LayoutNode::new_terminal());
        let new_terminal = LayoutNode::Terminal {
            terminal_id: Some(new_id.clone()),
            working_directory: None,
            zoom_level: None,
        };

        *node = LayoutNode::Split {
            direction,
            sizes: vec![50.0, 50.0],
            children: vec![old_node, new_terminal],
        };
        project.layout.normalize();
        Self::enforce_root_tabs(project);

        // Spawn session for new terminal
        self.spawn_session(&new_id, project_cwd.as_deref(), cx);

        // Find actual path after normalize (may differ due to flattening)
        let new_path = self
            .project(&project_id)
            .and_then(|p| p.layout.find_terminal_path(&new_id))
            .unwrap_or_default();
        self.set_focus(FocusTarget {
            project_id,
            layout_path: new_path,
        });
        cx.notify();
    }

    /// Add a new tab at the root Tabs level for the active project.
    pub fn new_tab_focused(&mut self, cx: &mut Context<Self>) {
        self.activate_terminal_content();
        let project_id = match &self.active_project_id {
            Some(id) => id.clone(),
            None => return,
        };
        let Some(project_cwd) = self.project(&project_id).map(|p| p.path.clone()) else {
            return;
        };
        let _ = self.open_terminal_tab_in_project(&project_id, project_cwd, cx);
    }

    /// Re-enforce the root-always-Tabs invariant after layout mutations.
    /// If `remove_at_path` + `normalize` collapsed a 2-child Tabs into a single child,
    /// re-wrap it in Tabs.
    fn enforce_root_tabs(project: &mut ProjectData) {
        if !matches!(project.layout, LayoutNode::Tabs { .. }) {
            let child = std::mem::replace(&mut project.layout, LayoutNode::new_terminal());
            project.layout = LayoutNode::Tabs {
                children: vec![child],
                active_tab: 0,
            };
        }
    }

    /// Compute the focus path for the active tab in a project.
    fn focus_path_for_active_tab(project: &ProjectData) -> Vec<usize> {
        let active = project.layout.active_tab_index().unwrap_or(0);
        let mut path = vec![active];
        if let Some(subtree) = project.layout.get_at_path(&[active]) {
            if let Some(sub) = subtree.find_first_terminal_path() {
                path.extend(sub);
            }
        }
        path
    }

    fn focus_target_has_live_terminal(&self, target: &FocusTarget) -> bool {
        self.project(&target.project_id)
            .and_then(|project| project.layout.get_at_path(&target.layout_path))
            .and_then(|node| match node {
                LayoutNode::Terminal {
                    terminal_id: Some(tid),
                    ..
                } => Some(tid),
                _ => None,
            })
            .is_some_and(|tid| self.terminal_views.contains_key(tid))
    }

    fn first_focusable_target(&self) -> Option<FocusTarget> {
        if let Some(active_id) = self.active_project_id.as_deref() {
            if let Some(project) = self.project(active_id) {
                if let Some(layout_path) = project.layout.find_first_terminal_path() {
                    return Some(FocusTarget {
                        project_id: active_id.to_string(),
                        layout_path,
                    });
                }
            }
        }

        self.projects.iter().find_map(|project| {
            project
                .layout
                .find_first_terminal_path()
                .map(|layout_path| FocusTarget {
                    project_id: project.id.clone(),
                    layout_path,
                })
        })
    }

    fn remove_terminal_path_from_project(project: &mut ProjectData, focus_path: &[usize]) {
        if focus_path.is_empty() {
            return;
        }

        let tab_index = focus_path[0];
        let root_tab_count = match &project.layout {
            LayoutNode::Tabs { children, .. } => children.len(),
            _ => return,
        };

        let remove_whole_tab = focus_path.len() == 1
            || project
                .layout
                .get_at_path(&[tab_index])
                .is_some_and(|node| node.terminal_count() <= 1);

        if remove_whole_tab {
            if root_tab_count <= 1 {
                if let LayoutNode::Tabs {
                    children,
                    active_tab,
                } = &mut project.layout
                {
                    if tab_index < children.len() {
                        children.remove(tab_index);
                        if children.is_empty() {
                            *active_tab = 0;
                        } else if tab_index <= *active_tab {
                            *active_tab = active_tab.saturating_sub(1);
                        }
                    }
                }
            } else {
                let _ = project.layout.remove_at_path(&[tab_index]);
            }
        } else {
            let _ = project.layout.remove_at_path(focus_path);
        }

        project.layout.normalize();
        Self::enforce_root_tabs(project);
    }

    fn close_terminals_by_id_internal(&mut self, terminal_ids: &[String]) {
        if terminal_ids.is_empty() {
            return;
        }

        let targets: HashSet<&str> = terminal_ids.iter().map(String::as_str).collect();
        for project in &mut self.projects {
            loop {
                let Some(path) = targets
                    .iter()
                    .find_map(|terminal_id| project.layout.find_terminal_path(terminal_id))
                else {
                    break;
                };
                Self::remove_terminal_path_from_project(project, &path);
            }
        }

        for terminal_id in terminal_ids {
            self.destroy_session(terminal_id);
        }

        if self
            .focus
            .current_target()
            .is_some_and(|target| !self.focus_target_has_live_terminal(target))
        {
            if let Some(target) = self.first_focusable_target() {
                self.set_focus(target);
            } else {
                self.focus.clear();
            }
        }
    }

    /// Remove a tab by index, enforce root Tabs invariant, destroy sessions,
    /// and return the new focus path. Returns None if the removal is invalid
    /// (last tab, out of bounds, etc.).
    fn remove_tab(
        &mut self,
        project_id: &str,
        tab_index: usize,
    ) -> Option<(Vec<String>, Vec<usize>)> {
        let project = self.project_mut(project_id)?;

        let root_tab_count = match &project.layout {
            LayoutNode::Tabs { children, .. } => children.len(),
            _ => return None,
        };

        if root_tab_count <= 1 || tab_index >= root_tab_count {
            return None;
        }

        let ids = project
            .layout
            .get_at_path(&[tab_index])
            .map(|n| n.collect_terminal_ids())
            .unwrap_or_default();

        project.layout.remove_at_path(&[tab_index]);
        project.layout.normalize();
        Self::enforce_root_tabs(project);

        let new_path = Self::focus_path_for_active_tab(project);
        Some((ids, new_path))
    }

    /// Close the focused pane. If it's the only content in a tab, remove the tab.
    /// No-op if it would remove the last tab (enforce minimum 1 tab).
    pub fn close_focused(&mut self, cx: &mut Context<Self>) {
        let (project_id, focus_path) = match self.focus.current_target() {
            Some(t) => (t.project_id.clone(), t.layout_path.clone()),
            None => return,
        };

        if focus_path.is_empty() {
            return;
        }

        let tab_index = focus_path[0];

        if focus_path.len() == 1 {
            // Focused on a direct child of root Tabs. Remove the tab
            if let Some((ids, new_path)) = self.remove_tab(&project_id, tab_index) {
                for tid in &ids {
                    self.destroy_session(tid);
                }
                self.set_focus(FocusTarget {
                    project_id,
                    layout_path: new_path,
                });
                cx.notify();
            }
        } else {
            // Focused inside a split within a tab.
            // First, read terminal_id and tab_terminal_count without mut borrow.
            let (terminal_id, tab_terminal_count) = {
                let project = match self.project(&project_id) {
                    Some(p) => p,
                    None => return,
                };
                let tid = match project.layout.get_at_path(&focus_path) {
                    Some(LayoutNode::Terminal { terminal_id, .. }) => terminal_id.clone(),
                    _ => return,
                };
                let count = project
                    .layout
                    .get_at_path(&[tab_index])
                    .map(|n| n.terminal_count())
                    .unwrap_or(0);
                (tid, count)
            };

            if tab_terminal_count <= 1 {
                // Last terminal in this tab. Remove the entire tab
                if let Some((ids, new_path)) = self.remove_tab(&project_id, tab_index) {
                    for tid in &ids {
                        self.destroy_session(tid);
                    }
                    self.set_focus(FocusTarget {
                        project_id,
                        layout_path: new_path,
                    });
                    cx.notify();
                }
            } else {
                // More terminals in this tab. Just remove this pane
                let ids: Vec<String> = terminal_id.into_iter().collect();
                let new_path = {
                    let project = match self.project_mut(&project_id) {
                        Some(p) => p,
                        None => return,
                    };
                    project.layout.remove_at_path(&focus_path);
                    project.layout.normalize();
                    Self::enforce_root_tabs(project);

                    let tab_subtree = project.layout.get_at_path(&[tab_index]);
                    if let Some(subtree) = tab_subtree {
                        let mut path = vec![tab_index];
                        if let Some(sub) = subtree.find_first_terminal_path() {
                            path.extend(sub);
                        }
                        path
                    } else {
                        Self::focus_path_for_active_tab(project)
                    }
                };

                for tid in &ids {
                    self.destroy_session(tid);
                }
                self.set_focus(FocusTarget {
                    project_id,
                    layout_path: new_path,
                });
                cx.notify();
            }
        }
    }

    /// Close the current tab (the tab containing the focused pane).
    /// Removes the entire tab and all its terminals. No-op on last tab.
    pub fn close_tab(&mut self, cx: &mut Context<Self>) {
        let (project_id, focus_path) = match self.focus.current_target() {
            Some(t) => (t.project_id.clone(), t.layout_path.clone()),
            None => return,
        };

        if focus_path.is_empty() {
            return;
        }

        if let Some((ids, new_path)) = self.remove_tab(&project_id, focus_path[0]) {
            for tid in &ids {
                self.destroy_session(tid);
            }
            self.set_focus(FocusTarget {
                project_id,
                layout_path: new_path,
            });
            cx.notify();
        }
    }

    /// Switch directly to tab N (0-indexed). No-op if out of bounds.
    pub fn goto_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.activate_terminal_content();

        let project_id = match &self.active_project_id {
            Some(id) => id.clone(),
            None => return,
        };

        let project = match self.project_mut(&project_id) {
            Some(p) => p,
            None => return,
        };

        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut project.layout
        {
            if index >= children.len() {
                return;
            }
            *active_tab = index;

            // Focus the first terminal in the new tab
            let mut new_focus = vec![index];
            if let Some(sub) = children[index].find_first_terminal_path() {
                new_focus.extend(sub);
            }
            self.set_focus(FocusTarget {
                project_id,
                layout_path: new_focus,
            });
        }
        cx.notify();
    }

    /// Close a specific tab by project ID and tab index. No-op on last tab.
    pub fn close_specific_tab(
        &mut self,
        project_id: &str,
        tab_index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some((ids, new_path)) = self.remove_tab(project_id, tab_index) {
            for tid in &ids {
                self.destroy_session(tid);
            }
            self.set_focus(FocusTarget {
                project_id: project_id.to_string(),
                layout_path: new_path,
            });
            cx.notify();
        }
    }

    /// Focus the next pane in depth-first order.
    pub fn focus_next_pane(&mut self, cx: &mut Context<Self>) {
        self.cycle_focus(1, cx);
    }

    /// Focus the previous pane in depth-first order.
    pub fn focus_prev_pane(&mut self, cx: &mut Context<Self>) {
        self.cycle_focus(-1, cx);
    }

    fn cycle_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        let (project_id, focus_path) = match self.focus.current_target() {
            Some(t) => (t.project_id.clone(), t.layout_path.clone()),
            None => return,
        };
        let project = match self.project(&project_id) {
            Some(p) => p,
            None => return,
        };
        let paths = project.layout.collect_terminal_paths();
        if paths.is_empty() {
            return;
        }
        let current_idx = paths.iter().position(|p| *p == focus_path).unwrap_or(0);
        let new_idx = (current_idx as isize + delta).rem_euclid(paths.len() as isize) as usize;
        self.set_focus(FocusTarget {
            project_id,
            layout_path: paths[new_idx].clone(),
        });
        cx.notify();
    }

    /// Switch to the next tab in the nearest Tabs ancestor.
    pub fn next_tab(&mut self, cx: &mut Context<Self>) {
        self.cycle_tab(1, cx);
    }

    /// Switch to the previous tab in the nearest Tabs ancestor.
    pub fn prev_tab(&mut self, cx: &mut Context<Self>) {
        self.cycle_tab(-1, cx);
    }

    fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let project_id = match &self.active_project_id {
            Some(id) => id.clone(),
            None => return,
        };

        let terminal_tab_count = self
            .project(&project_id)
            .and_then(|project| match &project.layout {
                LayoutNode::Tabs { children, .. } => Some(children.len()),
                _ => None,
            })
            .unwrap_or(0);
        let auxiliary_count = self.auxiliary_tabs.len();
        let total_tabs = auxiliary_count + terminal_tab_count;
        if total_tabs == 0 {
            return;
        }

        let current_index = if let Some(active_tab_id) = self.active_auxiliary_tab_id.as_deref() {
            self.auxiliary_tabs
                .iter()
                .position(|tab| tab.id == active_tab_id)
                .unwrap_or(0)
        } else {
            let active_terminal_tab = self
                .project(&project_id)
                .and_then(|project| project.layout.active_tab_index())
                .unwrap_or(0);
            auxiliary_count + active_terminal_tab
        };

        let new_index = (current_index as isize + delta).rem_euclid(total_tabs as isize) as usize;
        if new_index < auxiliary_count {
            let tab_id = self.auxiliary_tabs[new_index].id.clone();
            self.focus_auxiliary_tab(&tab_id, cx);
            return;
        }

        self.goto_tab(new_index - auxiliary_count, cx);
    }

    /// Reorder a tab within the root Tabs node.
    /// Moves the tab at `from_index` to `to_index`. Updates `active_tab` to follow
    /// the moved tab if it was the active one.
    pub fn reorder_tab(
        &mut self,
        project_id: &str,
        from_index: usize,
        to_index: usize,
        cx: &mut Context<Self>,
    ) {
        if from_index == to_index {
            return;
        }
        let project = match self.project_mut(project_id) {
            Some(p) => p,
            None => return,
        };

        if let LayoutNode::Tabs {
            children,
            active_tab,
        } = &mut project.layout
        {
            if from_index >= children.len() || to_index >= children.len() {
                return;
            }
            let child = children.remove(from_index);
            children.insert(to_index, child);

            // Update active_tab to track the moved tab
            if *active_tab == from_index {
                *active_tab = to_index;
            } else if from_index < *active_tab && to_index >= *active_tab {
                *active_tab = active_tab.saturating_sub(1);
            } else if from_index > *active_tab && to_index <= *active_tab {
                *active_tab = (*active_tab + 1).min(children.len() - 1);
            }
        }

        // Update focus path. First element is the tab index
        if let Some(target) = self.focus.current_target() {
            if target.project_id == project_id && !target.layout_path.is_empty() {
                let mut new_path = target.layout_path.clone();
                let focused_tab = new_path[0];
                if focused_tab == from_index {
                    new_path[0] = to_index;
                } else if from_index < focused_tab && to_index >= focused_tab {
                    new_path[0] = focused_tab.saturating_sub(1);
                } else if from_index > focused_tab && to_index <= focused_tab {
                    new_path[0] = focused_tab + 1;
                }
                self.set_focus(FocusTarget {
                    project_id: project_id.to_string(),
                    layout_path: new_path,
                });
            }
        }
        cx.notify();
    }

    /// Reorder projects in the workspace. Moves project at `from_index` to `to_index`.
    pub fn reorder_project(&mut self, from_index: usize, to_index: usize, cx: &mut Context<Self>) {
        if from_index == to_index
            || from_index >= self.projects.len()
            || to_index >= self.projects.len()
        {
            return;
        }
        let project = self.projects.remove(from_index);
        self.projects.insert(to_index, project);
        cx.notify();
    }

    /// Remove a project and all its terminal sessions.
    /// Switches active project if the removed one was active.
    pub fn remove_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let live_diff_tab_id = Self::live_diff_stream_tab_id(project_id);
        let _ = self.close_auxiliary_tab_internal(&live_diff_tab_id);
        self.live_diff_feeds.remove(project_id);
        let repository_tab_id = Self::repository_graph_tab_id(project_id);
        let _ = self.close_auxiliary_tab_internal(&repository_tab_id);
        self.repository_graph_tabs.remove(project_id);

        // Collect terminal IDs to destroy
        let ids_to_destroy: Vec<String> = self
            .project(project_id)
            .map(|p| p.layout.collect_terminal_ids())
            .unwrap_or_default();

        // Remove the project
        let was_active = self.active_project_id.as_deref() == Some(project_id);
        self.projects.retain(|p| p.id != project_id);

        // Destroy all sessions
        for tid in &ids_to_destroy {
            self.destroy_session(tid);
        }
        if let Some(store) = self.services.store.lock().as_mut() {
            if let Err(err) = store.delete_agent_terminals_for_project(project_id) {
                tracing::warn!(
                    "Failed to clear resumable terminals for removed project {}: {}",
                    project_id,
                    err
                );
            }
        }

        // If that was the last project, create a fresh one at cwd
        if self.projects.is_empty() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            self.init_with_project(cwd, cx);
        } else if was_active {
            let next_id = self.projects[0].id.clone();
            self.set_active_project(&next_id, cx);
        }
        cx.notify();
    }

    /// Close all tabs except the one at the given index.
    pub fn close_other_tabs(
        &mut self,
        project_id: &str,
        keep_index: usize,
        cx: &mut Context<Self>,
    ) {
        let ids_to_destroy = {
            let project = match self.project_mut(project_id) {
                Some(p) => p,
                None => return,
            };

            if let LayoutNode::Tabs { children, .. } = &project.layout {
                if keep_index >= children.len() {
                    return;
                }
                // Collect IDs from tabs we're removing
                let mut ids = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    if i != keep_index {
                        ids.extend(child.collect_terminal_ids());
                    }
                }
                ids
            } else {
                return;
            }
        };

        // Keep only the specified tab
        if let Some(project) = self.project_mut(project_id) {
            if let LayoutNode::Tabs {
                children,
                active_tab,
            } = &mut project.layout
            {
                if keep_index < children.len() {
                    let kept = children.remove(keep_index);
                    children.clear();
                    children.push(kept);
                    *active_tab = 0;
                }
            }
        }

        for tid in &ids_to_destroy {
            self.destroy_session(tid);
        }

        // Update focus to the kept tab
        let new_path = self
            .project(project_id)
            .map(Self::focus_path_for_active_tab)
            .unwrap_or(vec![0]);
        self.set_focus(FocusTarget {
            project_id: project_id.to_string(),
            layout_path: new_path,
        });
        cx.notify();
    }

    /// Rename a terminal. Updates the terminal_names map in ProjectData.
    pub fn rename_terminal(
        &mut self,
        project_id: &str,
        terminal_id: &str,
        new_name: String,
        cx: &mut Context<Self>,
    ) {
        let automatic_label = self.automatic_terminal_label(self.project(project_id), terminal_id);
        if let Some(project) = self.project_mut(project_id) {
            if new_name.is_empty() || new_name == automatic_label {
                project.terminal_names.remove(terminal_id);
            } else {
                project
                    .terminal_names
                    .insert(terminal_id.to_string(), new_name);
            }
        }
        cx.notify();
    }

    // ── Inline rename ──

    /// Start an inline rename for a terminal. Creates a text input pre-filled
    /// with the current name. The input is focused by the rendering component
    /// (tab bar or sidebar) in the next render cycle.
    pub fn start_rename(
        &mut self,
        project_id: String,
        terminal_id: String,
        location: RenameLocation,
        cx: &mut Context<Self>,
    ) {
        let current_name = self.terminal_display_name(&project_id, &terminal_id);
        let palette = theme::active(cx);

        let input = cx.new(|cx| {
            let (tr, tg, tb) = theme::rgb_channels(palette.BONE);
            let (pr, pg, pb) = theme::rgb_channels(palette.FOG);
            let (cr, cg, cb) = theme::rgb_channels(palette.ORCA_BLUE);
            let text_color = Hsla::from(Rgba {
                r: tr as f32 / 255.0,
                g: tg as f32 / 255.0,
                b: tb as f32 / 255.0,
                a: 1.0,
            });
            let placeholder_color = Hsla::from(Rgba {
                r: pr as f32 / 255.0,
                g: pg as f32 / 255.0,
                b: pb as f32 / 255.0,
                a: 0.5,
            });
            let cursor_color = Hsla::from(Rgba {
                r: cr as f32 / 255.0,
                g: cg as f32 / 255.0,
                b: cb as f32 / 255.0,
                a: 1.0,
            });
            let selection_bg = Hsla::from(Rgba {
                r: cr as f32 / 255.0,
                g: cg as f32 / 255.0,
                b: cb as f32 / 255.0,
                a: 0.25,
            });
            let mut input = TextInputState::new(
                text_color,
                placeholder_color,
                cursor_color,
                selection_bg,
                cx,
            );
            input.set_placeholder("Terminal name...");
            input.set_value(&current_name);
            input
        });

        self.renaming = Some(RenameState {
            project_id,
            terminal_id,
            input,
            location,
            focused_once: false,
        });
        cx.notify();
    }

    /// Commit the current inline rename. Applies the text input value.
    pub fn commit_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.renaming.take() {
            let new_name = state.input.read(cx).value().trim().to_string();
            self.rename_terminal(&state.project_id, &state.terminal_id, new_name, cx);
        }
        cx.notify();
    }

    /// Cancel the current inline rename. Discards changes.
    pub fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = None;
        cx.notify();
    }

    /// Check if a specific terminal is currently being renamed.
    pub fn is_renaming(&self, terminal_id: &str) -> bool {
        self.renaming
            .as_ref()
            .is_some_and(|r| r.terminal_id == terminal_id)
    }

    // ── Settings tab management ──

    /// Toggle the settings tab. If closed: open and focus. If open and focused:
    /// close. If open but not focused: focus it.
    pub fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.ensure_settings_tab();
        if self.is_settings_focused() {
            self.close_settings(cx);
            return;
        }
        self.active_auxiliary_tab_id = Some(SETTINGS_TAB_ID.to_string());
        cx.notify();
    }

    /// Close the settings tab and switch focus back to the active terminal tab.
    pub fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.close_auxiliary_tab(SETTINGS_TAB_ID, cx);
    }

    /// Update split sizes at the given path.
    pub fn update_split_sizes(
        &mut self,
        project_id: &str,
        path: &[usize],
        new_sizes: Vec<f32>,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project_mut(project_id) {
            if let Some(LayoutNode::Split {
                sizes, children, ..
            }) = project.layout.get_at_path_mut(path)
            {
                if new_sizes.len() == children.len() {
                    *sizes = new_sizes;
                }
            }
        }
        cx.notify();
    }

    /// Focus a specific terminal by project_id and layout_path.
    pub fn focus_pane(
        &mut self,
        project_id: String,
        layout_path: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        self.activate_terminal_content();
        self.set_focus(FocusTarget {
            project_id,
            layout_path,
        });
        cx.notify();
    }

    /// Select an exact terminal target without introducing intermediate focus hops.
    pub fn select_terminal(
        &mut self,
        project_id: String,
        layout_path: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.select_terminal_internal(&project_id, &layout_path) {
            cx.notify();
        }
    }

    fn select_terminal_internal(&mut self, project_id: &str, layout_path: &[usize]) -> bool {
        let target_is_terminal = self
            .project(project_id)
            .and_then(|project| project.layout.get_at_path(layout_path))
            .is_some_and(|node| {
                matches!(
                    node,
                    LayoutNode::Terminal {
                        terminal_id: Some(_),
                        ..
                    }
                )
            });
        if !target_is_terminal {
            return false;
        }

        self.activate_terminal_content();
        self.active_project_id = Some(project_id.to_string());

        if let Some(&tab_index) = layout_path.first() {
            if let Some(project) = self.project_mut(project_id) {
                if let LayoutNode::Tabs {
                    children,
                    active_tab,
                } = &mut project.layout
                {
                    if tab_index >= children.len() {
                        return false;
                    }
                    *active_tab = tab_index;
                }
            }
        }

        self.set_focus(FocusTarget {
            project_id: project_id.to_string(),
            layout_path: layout_path.to_vec(),
        });
        true
    }

    /// Set the active project and focus the active tab's first terminal.
    pub fn set_active_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.project(project_id).is_none() {
            return;
        }
        self.active_project_id = Some(project_id.to_string());
        // Focus the active tab's first terminal
        if let Some(project) = self.project(project_id) {
            let active_tab = project.layout.active_tab_index().unwrap_or(0);
            let mut focus_path = vec![active_tab];
            if let Some(subtree) = project.layout.get_at_path(&[active_tab]) {
                if let Some(sub) = subtree.find_first_terminal_path() {
                    focus_path.extend(sub);
                }
            }
            self.set_focus(FocusTarget {
                project_id: project_id.to_string(),
                layout_path: focus_path,
            });
        }
        cx.notify();
    }

    /// Resolve `path` to its canonical form so that symlinks and `..` components are
    /// eliminated before storing or comparing project paths.  Falls back to the original
    /// path if canonicalization fails (e.g. the directory does not yet exist).
    fn normalize_project_path(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    /// Open a terminal at `path`.
    ///
    /// - If a project already exists at the canonical path: switch to it and add a new
    ///   terminal tab so the user lands in a fresh shell there.
    /// - Otherwise: create a new project (which also opens its first terminal).
    pub fn open_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let canonical = Self::normalize_project_path(&path);
        if let Some(pid) = self
            .projects
            .iter()
            .find(|p| p.path == canonical)
            .map(|p| p.id.clone())
        {
            self.set_active_project(&pid, cx);
            self.add_terminal_to_project(&pid, cx);
        } else {
            self.add_project(canonical, cx);
        }
    }

    /// Add a new project from a directory path.
    pub fn add_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = Self::normalize_project_path(&path);
        // Prevent duplicate projects
        if self.projects.iter().any(|p| p.path == path) {
            // Switch to existing project instead
            if let Some(pid) = self
                .projects
                .iter()
                .find(|p| p.path == path)
                .map(|p| p.id.clone())
            {
                self.set_active_project(&pid, cx);
            }
            return;
        }

        let mut project = ProjectData::new(path.clone());
        let terminal_id = Self::generate_terminal_id();

        if !self.spawn_session(&terminal_id, Some(&path), cx) {
            tracing::error!(
                "Failed to add project at {}: PTY spawn failed",
                path.display()
            );
            return;
        }

        // Root is always Tabs
        project.layout = LayoutNode::Tabs {
            children: vec![LayoutNode::Terminal {
                terminal_id: Some(terminal_id),
                working_directory: None,
                zoom_level: None,
            }],
            active_tab: 0,
        };

        let project_id = project.id.clone();
        self.projects.push(project);
        self.active_project_id = Some(project_id.clone());
        self.set_focus(FocusTarget {
            project_id,
            layout_path: vec![0],
        });
        cx.notify();
    }

    /// Add a new terminal to an existing project as a new tab at the root level.
    pub fn add_terminal_to_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some(project_cwd) = self.project(project_id).map(|p| p.path.clone()) else {
            return;
        };
        let _ = self.open_terminal_tab_in_project(project_id, project_cwd, cx);
    }

    /// Initialize workspace with a single project at the given path.
    /// Root layout is always Tabs with one Terminal child.
    pub fn init_with_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = Self::normalize_project_path(&path);
        let mut project = ProjectData::new(path.clone());
        let terminal_id = Self::generate_terminal_id();

        let spawned = self.spawn_session(&terminal_id, Some(&path), cx);
        let terminal_node = if spawned {
            LayoutNode::Terminal {
                terminal_id: Some(terminal_id),
                working_directory: None,
                zoom_level: None,
            }
        } else {
            LayoutNode::Terminal {
                terminal_id: None,
                working_directory: None,
                zoom_level: None,
            }
        };

        // Root is always Tabs
        project.layout = LayoutNode::Tabs {
            children: vec![terminal_node],
            active_tab: 0,
        };

        let project_id = project.id.clone();
        self.projects.push(project);
        self.active_project_id = Some(project_id.clone());
        self.set_focus(FocusTarget {
            project_id,
            layout_path: vec![0],
        });
    }

    /// Restore projects from stored data, spawning sessions for each terminal.
    pub fn restore_projects(
        &mut self,
        stored: Vec<StoredProject>,
        active_project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        for sp in stored {
            let mut project = match ProjectData::from_stored(sp) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("Failed to restore project: {e}");
                    continue;
                }
            };

            // Normalize the restored path so that open_directory lookups match
            // regardless of when the path was originally saved (pre- or post-CP1).
            project.path = Self::normalize_project_path(&project.path);
            let project_path = project.path.clone();
            let resume_plan = if cx.global::<AppSettings>().resume_agent_sessions {
                self.build_project_resume_restore_plan(&project.id)
            } else {
                ProjectResumeRestorePlan::default()
            };

            // Walk layout tree and spawn sessions for each terminal
            self.restore_layout_terminals(&mut project.layout, &project_path, &resume_plan, cx);

            if resume_plan.suppressed_duplicates > 0 {
                self.set_workspace_warning(
                    "Suppressed duplicate same-directory agent resume and restored those terminals as plain shells.",
                    cx,
                );
            }

            project.layout.normalize();
            Self::enforce_root_tabs(&mut project);

            self.projects.push(project);
        }

        // Restore active project
        if let Some(ref id) = active_project_id {
            if self.project(id).is_some() {
                self.active_project_id = Some(id.clone());
            }
        }
        if self.active_project_id.is_none() {
            self.active_project_id = self.projects.first().map(|p| p.id.clone());
        }

        // Set focus to first terminal in active project
        if let Some(project) = self.active_project() {
            let focus_path = Self::focus_path_for_active_tab(project);
            if let Some(ref id) = self.active_project_id {
                self.set_focus(FocusTarget {
                    project_id: id.clone(),
                    layout_path: focus_path,
                });
            }
        }
    }

    /// Walk a layout tree, generating new terminal IDs and spawning sessions.
    fn restore_layout_terminals(
        &mut self,
        node: &mut LayoutNode,
        project_path: &Path,
        resume_plan: &ProjectResumeRestorePlan,
        cx: &mut Context<Self>,
    ) {
        match node {
            LayoutNode::Terminal {
                terminal_id,
                working_directory,
                zoom_level,
            } => {
                // Reuse persisted ID for stable terminal identity across restarts.
                // Only generate a new ID if the saved layout had None (corrupt data).
                let id = terminal_id
                    .clone()
                    .unwrap_or_else(Self::generate_terminal_id);
                *terminal_id = Some(id.clone());
                let new_id = id;

                // Resolve cwd: use saved working_directory if it still exists, else project root
                let cwd = Self::restored_terminal_cwd(project_path, working_directory.as_ref());

                let settings = cx.global::<AppSettings>();
                let scrollback = settings.scrollback_lines as usize;
                let shell_override = settings.default_shell.as_deref();
                // Resolve once to avoid duplicate work.
                let resolved_shell =
                    orcashell_session::shell_integration::resolve_shell_path(shell_override);
                let shell_label = Self::extract_shell_label(&resolved_shell);
                let palette = theme::active(cx);
                let mut config = Self::build_terminal_config(settings, &palette);

                // Apply per-terminal zoom offset
                if let Some(offset) = zoom_level {
                    config.font_size += px(*offset);
                }

                let colors = TerminalColors::new(
                    theme::rgb_channels(palette.TERMINAL_FOREGROUND),
                    theme::rgb_channels(palette.TERMINAL_BACKGROUND),
                    theme::rgb_channels(palette.TERMINAL_CURSOR),
                );
                match SessionEngine::new_with_shell(
                    80,
                    24,
                    scrollback,
                    Some(&cwd),
                    colors,
                    Some(&resolved_shell),
                ) {
                    Ok(engine) => {
                        let shell_type = engine.shell_type();
                        self.attach_terminal_view(
                            new_id,
                            shell_label,
                            shell_type,
                            engine,
                            config,
                            cx,
                        );
                        self.services
                            .git
                            .request_snapshot(&cwd, terminal_id.as_deref());
                        if let Some(row) = resume_plan
                            .rows_by_terminal_id
                            .get(terminal_id.as_deref().unwrap_or_default())
                        {
                            if Self::should_queue_resume_injection(
                                row,
                                &cwd,
                                &resume_plan.winning_terminal_ids,
                            ) {
                                self.queue_resume_injection(&row.terminal_id, row.agent_kind, cx);
                            } else if resume_plan.winning_terminal_ids.contains(&row.terminal_id) {
                                self.set_workspace_warning(
                                    format!(
                                        "Skipped agent resume for {} because its saved working directory could not be restored.",
                                        row.terminal_id
                                    ),
                                    cx,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to restore terminal session: {e}");
                    }
                }
            }
            LayoutNode::Split { children, .. } | LayoutNode::Tabs { children, .. } => {
                for child in children {
                    self.restore_layout_terminals(child, project_path, resume_plan, cx);
                }
            }
        }
    }

    /// Convert all projects to StoredProject for persistence, capturing live
    /// terminal cwd and zoom state inline (no layout tree mutation).
    pub fn to_stored_projects(&self, cx: &App) -> Vec<StoredProject> {
        self.to_stored_projects_for_window(1, cx)
    }

    /// Convert all projects to StoredProject tagged with a specific window_id.
    pub fn to_stored_projects_for_window(&self, window_id: i64, cx: &App) -> Vec<StoredProject> {
        let settings = cx.global::<AppSettings>();
        let base_font_size = px(settings.font_size);

        self.projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let layout = Self::layout_with_live_state(
                    &p.layout,
                    &self.terminal_views,
                    base_font_size,
                    cx,
                );
                let layout_json =
                    serde_json::to_string(&layout).expect("LayoutNode serialization cannot fail");
                let terminal_names_json = serde_json::to_string(&p.terminal_names)
                    .expect("terminal_names serialization cannot fail");
                StoredProject {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    path: p.path.clone(),
                    layout_json,
                    terminal_names_json,
                    sort_order: i as i32,
                    window_id,
                }
            })
            .collect()
    }

    /// Clone a layout node tree, injecting live cwd and zoom from terminal views.
    fn layout_with_live_state(
        node: &LayoutNode,
        terminal_views: &HashMap<String, Entity<TerminalView>>,
        base_font_size: Pixels,
        cx: &App,
    ) -> LayoutNode {
        match node {
            LayoutNode::Terminal {
                terminal_id,
                working_directory,
                zoom_level,
            } => {
                let (live_cwd, live_zoom) = terminal_id
                    .as_ref()
                    .and_then(|tid| terminal_views.get(tid))
                    .map(|view| {
                        let view_ref = view.read(cx);
                        let cwd = view_ref.engine().cwd();
                        let current = view_ref.current_font_size();
                        let offset = f32::from(current) - f32::from(base_font_size);
                        let zoom = if offset.abs() > 0.01 {
                            Some(offset)
                        } else {
                            None
                        };
                        (cwd, zoom)
                    })
                    .unwrap_or((working_directory.clone(), *zoom_level));
                LayoutNode::Terminal {
                    terminal_id: terminal_id.clone(),
                    working_directory: live_cwd,
                    zoom_level: live_zoom,
                }
            }
            LayoutNode::Split {
                direction,
                sizes,
                children,
            } => LayoutNode::Split {
                direction: direction.clone(),
                sizes: sizes.clone(),
                children: children
                    .iter()
                    .map(|c| Self::layout_with_live_state(c, terminal_views, base_font_size, cx))
                    .collect(),
            },
            LayoutNode::Tabs {
                children,
                active_tab,
            } => LayoutNode::Tabs {
                children: children
                    .iter()
                    .map(|c| Self::layout_with_live_state(c, terminal_views, base_font_size, cx))
                    .collect(),
                active_tab: *active_tab,
            },
        }
    }
}

fn include_preview_line(kind: DiffLineKind) -> bool {
    !matches!(kind, DiffLineKind::FileHeader | DiffLineKind::HunkHeader)
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}

// Note: Unit tests for settings state transitions and build_terminal_config
// live in workspace/layout.rs tests (avoids GPUI proc-macro compiler issues
// when compiling tests in this module due to heavy Entity type usage).

#[cfg(test)]
mod tests;
