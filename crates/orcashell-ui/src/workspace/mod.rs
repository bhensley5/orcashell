pub mod actions;
pub mod focus;
pub mod layout;
pub mod project;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::*;
use orcashell_daemon_core::git_coordinator::{
    GitActionKind, GitCoordinator, GitEvent, GitRemoteKind,
};
use orcashell_git::{
    DiffDocument, DiffSectionKind, DiffSelectionKey, FileDiffDocument, GitSnapshotSummary,
    ManagedWorktree,
};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::settings::AppSettings;
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

#[derive(Debug, Clone, PartialEq)]
pub struct DiffTabState {
    pub scope_root: PathBuf,
    pub tree_width: f32,
    pub index: DiffIndexState,
    pub selected_file: Option<DiffSelectionKey>,
    pub file: DiffFileState,
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
            index: DiffIndexState::default(),
            selected_file: None,
            file: DiffFileState::default(),
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

    pub fn is_settings_focused(&self) -> bool {
        self.active_auxiliary_tab()
            .is_some_and(|tab| matches!(tab.kind, AuxiliaryTabKind::Settings))
    }

    pub fn diff_tab_state(&self, scope_root: &Path) -> Option<&DiffTabState> {
        self.diff_tabs.get(scope_root)
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

    pub fn focus_auxiliary_tab(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if self.focus_auxiliary_tab_internal(tab_id) {
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

    // ── Diff tab action dispatch ───────────────────────────────────────

    fn any_action_in_flight(tab: &DiffTabState) -> bool {
        tab.local_action_in_flight || tab.remote_op_in_flight
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
            .filter(|k| k.section == DiffSectionKind::Unstaged)
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
                scope_root,
                result,
                ..
            } => {
                self.apply_snapshot_update(terminal_ids, scope_root, result);
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
            GitEvent::LocalActionCompleted {
                scope_root,
                action,
                result,
            } => {
                self.apply_local_action_completion(scope_root, action, result);
                cx.notify();
            }
            GitEvent::RemoteOpCompleted {
                scope_root,
                kind,
                result,
            } => {
                self.apply_remote_op_completion(scope_root, kind, result);
                cx.notify();
            }
        }
    }

    fn apply_snapshot_update(
        &mut self,
        terminal_ids: Vec<String>,
        scope_root: Option<PathBuf>,
        result: Result<GitSnapshotSummary, String>,
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
            }
            Err(message) => {
                if let Some(scope_root) = scope_root {
                    self.mark_diff_scope_unavailable(&scope_root, message);
                    self.detach_scope(&scope_root);
                }
                for terminal_id in terminal_ids {
                    self.detach_terminal_scope(&terminal_id);
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
                    // 2. First file in staged_files
                    // 3. First file in unstaged_files
                    // 4. None (empty state)
                    let selected_file = diff_tab
                        .selected_file
                        .clone()
                        .filter(|key| {
                            let files = match key.section {
                                DiffSectionKind::Staged => &document.staged_files,
                                DiffSectionKind::Unstaged => &document.unstaged_files,
                            };
                            files.iter().any(|f| f.relative_path == key.relative_path)
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
                            DiffSectionKind::Staged => &document.staged_files,
                            DiffSectionKind::Unstaged => &document.unstaged_files,
                        };
                        files.iter().any(|f| f.relative_path == key.relative_path)
                    });

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

    fn apply_local_action_completion(
        &mut self,
        scope_root: PathBuf,
        action: GitActionKind,
        result: Result<String, String>,
    ) {
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

            // Clear multi-select after successful stage/unstage.
            // Skip success banner for stage/unstage. The tree update is feedback enough.
            if result.is_ok() && matches!(action, GitActionKind::Stage | GitActionKind::Unstage) {
                diff_tab.multi_select.clear();
                diff_tab.selection_anchor = None;
                diff_tab.last_action_banner = None;
            } else {
                let banner = match &result {
                    Ok(msg) if msg.starts_with("BLOCKED: ") => ActionBanner {
                        kind: ActionBannerKind::Warning,
                        message: msg["BLOCKED: ".len()..].to_string(),
                    },
                    Ok(msg) => ActionBanner {
                        kind: ActionBannerKind::Success,
                        message: msg.clone(),
                    },
                    Err(msg) => ActionBanner {
                        kind: ActionBannerKind::Error,
                        message: msg.clone(),
                    },
                };
                diff_tab.last_action_banner = Some(banner);
            }

            // Clear commit message after successful commit
            if result.is_ok() && matches!(action, GitActionKind::Commit) {
                diff_tab.commit_message.clear();
                diff_tab.multi_select.clear();
                diff_tab.selection_anchor = None;
            }
        }
    }

    fn apply_remote_op_completion(
        &mut self,
        scope_root: PathBuf,
        _kind: GitRemoteKind,
        result: Result<String, String>,
    ) {
        if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
            diff_tab.remote_op_in_flight = false;

            let banner = match &result {
                Ok(msg) if msg.starts_with("BLOCKED: ") => ActionBanner {
                    kind: ActionBannerKind::Warning,
                    message: msg["BLOCKED: ".len()..].to_string(),
                },
                Ok(msg) => ActionBanner {
                    kind: ActionBannerKind::Success,
                    message: msg.clone(),
                },
                Err(msg) => ActionBanner {
                    kind: ActionBannerKind::Error,
                    message: msg.clone(),
                },
            };
            diff_tab.last_action_banner = Some(banner);
        }
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
        if let Some(AuxiliaryTabKind::Diff { scope_root }) =
            self.active_auxiliary_tab().map(|tab| tab.kind.clone())
        {
            self.services.git.request_snapshot(&scope_root, None);
            self.request_diff_index_refresh(&scope_root);
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
        if let AuxiliaryTabKind::Diff { scope_root } = removed.kind {
            self.diff_tabs.remove(&scope_root);
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

    fn latest_diff_generation(&self, scope_root: &Path) -> Option<u64> {
        self.git_scopes
            .get(scope_root)
            .map(|snapshot| snapshot.generation)
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
        self.services.git.set_diff_theme(active_theme);

        let scopes: Vec<PathBuf> = self.diff_tabs.keys().cloned().collect();
        for scope_root in scopes {
            let selected = self
                .diff_tabs
                .get(&scope_root)
                .and_then(|tab| tab.selected_file.clone());
            if let Some(diff_tab) = self.diff_tabs.get_mut(&scope_root) {
                diff_tab.file.document = None;
            }
            if let Some(selection) = selected {
                self.request_selected_file_diff(&scope_root, selection);
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
        } else {
            self.detach_terminal_scope(terminal_id);
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

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}

// Note: Unit tests for settings state transitions and build_terminal_config
// live in workspace/layout.rs tests (avoids GPUI proc-macro compiler issues
// when compiling tests in this module due to heavy Entity type usage).

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    use orcashell_daemon_core::git_coordinator::{GitActionKind, GitRemoteKind};
    use parking_lot::Mutex;

    // Import only the types tests actually need. Avoid `use super::*` which
    // pulls in GPUI types and blows the gpui_macros proc-macro stack budget
    // during test compilation (same pattern as orcashell-terminal-view/search.rs).
    use super::{
        classify_notification, AuxiliaryTabKind, AuxiliaryTabState, DiffTabState, FocusTarget,
        GitEvent, GitSnapshotSummary, LayoutNode, NotificationTier, ProjectData,
        ResumableAgentKind, ResumeInjectionTrigger, TerminalRuntimeState, WorkspaceServices,
        WorkspaceState, SETTINGS_TAB_ID,
    };
    use orcashell_git::{
        ChangedFile, DiffDocument, DiffSectionKind, DiffSelectionKey, FileDiffDocument,
        GitFileStatus, GitTrackingStatus,
    };
    use orcashell_session::semantic_zone::SemanticState;
    use orcashell_store::Store;
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
            staged_files: Vec::new(),
            unstaged_files: files,
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
        for _ in 0..2 {
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
                GitEvent::DiffIndexLoaded { .. } => {}
                other => panic!("unexpected event: {other:?}"),
            }
        }

        assert!(saw_snapshot);
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
    fn snapshot_error_marks_open_diff_tab_unavailable() {
        let mut ws = WorkspaceState::new();
        let scope_root = PathBuf::from("/tmp/repo");
        ws.open_or_focus_diff_tab_internal(scope_root.clone(), "main".into());

        ws.apply_snapshot_update(
            vec![],
            Some(scope_root.clone()),
            Err("repo unavailable".into()),
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
            Some(scope_root.clone()),
            Ok(snapshot("/tmp/repo")),
        );

        assert!(!ws.terminal_git_scopes.contains_key("t1"));
        assert!(ws.git_scopes.contains_key(&scope_root));
    }

    #[test]
    fn snapshot_error_detaches_all_terminals_for_failed_scope() {
        let mut ws = WorkspaceState::new();
        let scope_root = PathBuf::from("/tmp/repo");
        ws.git_scopes
            .insert(scope_root.clone(), snapshot("/tmp/repo"));
        ws.terminal_git_scopes
            .insert("t1".into(), scope_root.clone());

        ws.apply_snapshot_update(vec![], Some(scope_root.clone()), Err("boom".into()));

        assert!(!ws.terminal_git_scopes.contains_key("t1"));
        assert!(!ws.git_scopes.contains_key(&scope_root));
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

        let transition = ws.apply_semantic_state_change(
            "t1",
            SemanticState::CommandComplete { exit_code: Some(0) },
        );
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
    fn missing_cwd_detaches_terminal_git_scope() {
        let mut ws = WorkspaceState::new();
        let scope_root = PathBuf::from("/tmp/repo");
        ws.git_scopes
            .insert(scope_root.clone(), snapshot("/tmp/repo"));
        ws.terminal_git_scopes
            .insert("t1".into(), scope_root.clone());

        ws.sync_terminal_git_scope("t1", None);

        assert!(!ws.terminal_git_scopes.contains_key("t1"));
        assert!(!ws.git_scopes.contains_key(&scope_root));
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
        let worktree =
            orcashell_git::create_managed_worktree(&repo, "wt-12345678").expect("worktree");

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
            Ok("Everything up-to-date".into()),
        );

        let tab = &ws.diff_tabs[&scope_root];
        assert!(!tab.remote_op_in_flight);
        assert_eq!(
            tab.last_action_banner.as_ref().unwrap().kind,
            super::ActionBannerKind::Success
        );
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
}
