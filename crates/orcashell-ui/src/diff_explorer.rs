use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use orcashell_git::{
    parse_conflict_file_text, ChangedFile, DiffLineKind, DiffLineView, DiffSectionKind,
    DiffSelectionKey, FileDiffHunk, GitFileStatus, GitTrackingStatus, HighlightedSpan, Oid,
    ParsedConflictBlock, StashDetailDocument, StashEntrySummary, StashFileDiffDocument,
    StashFileSelection, OVERSIZE_DIFF_MESSAGE,
};
use orcashell_syntax::{highlight_line_for_path, Highlighter, HighlighterCheckpoint};
use orcashell_terminal_view::{Copy, TextInputState};

use crate::app_view::ContextMenuRequest;
use crate::context_menu::ContextMenuItem;
use crate::prompt_dialog::{
    PromptDialogConfirmTone, PromptDialogInputSpec, PromptDialogRequest,
    PromptDialogSelectionOption, PromptDialogSelectionSpec, PromptDialogSpec,
    PromptDialogToggleSpec,
};
use crate::settings::ThemeId;
use crate::theme::{self, OrcaTheme};
use crate::workspace::{
    ActionBanner, ActionBannerKind, ConflictDocumentState, ConflictEditorDocument, DiffTabState,
    DiffTabViewMode, ManagedWorktreeSummary, RemoveWorktreeConfirm, WorkspaceState,
};

const MIN_TREE_WIDTH: f32 = 180.0;
const MIN_DIFF_WIDTH: f32 = 320.0;

/// Height of each diff row: 20px min-height + 2×1px vertical padding.
pub(crate) const LINE_HEIGHT: f32 = 22.0;

/// Scrollbar hit-zone width (invisible track area that accepts clicks).
pub(crate) const SCROLLBAR_HIT_WIDTH: f32 = 12.0;
/// Visible scrollbar thumb width.
pub(crate) const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
/// Minimum scrollbar thumb height so it remains clickable.
pub(crate) const SCROLLBAR_THUMB_MIN: f32 = 20.0;
/// Inset from the right edge of the track to the thumb center.
pub(crate) const SCROLLBAR_THUMB_INSET: f32 = 3.0;
const CONFLICT_HIGHLIGHT_ANCHOR_INTERVAL: usize = 128;
const CONFLICT_INDENT: &str = "    ";

/// Width of each line-number gutter column.
pub(crate) const GUTTER_WIDTH: f32 = 52.0;
/// Gap between gutter columns and between the last gutter and the text.
pub(crate) const GUTTER_GAP: f32 = 8.0;
/// Horizontal padding on each diff row.
pub(crate) const LINE_PAD_X: f32 = 12.0;
/// X offset where the text column begins, relative to the row's leading edge.
pub(crate) const TEXT_COL_X: f32 =
    LINE_PAD_X + GUTTER_WIDTH + GUTTER_GAP + GUTTER_WIDTH + GUTTER_GAP;

/// Fallback advance width per character before font measurement completes.
pub(crate) const DEFAULT_CHAR_WIDTH: f32 = 6.6;

/// Font family used in the diff view.
pub(crate) const DIFF_FONT_FAMILY: &str = "JetBrains Mono";
/// Font size used in the diff view.
pub(crate) const DIFF_FONT_SIZE: f32 = 11.0;

pub(crate) fn max_diff_content_width(max_line_chars: usize, measured_char_width: f32) -> f32 {
    TEXT_COL_X + (max_line_chars as f32 * measured_char_width) + LINE_PAD_X
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DiffResizeDrag {
    initial_mouse_x: f32,
    initial_width: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollbarDrag {
    pub(crate) start_y: f32,
    pub(crate) start_scroll_y: f32,
}

/// Cached filtered diff lines for the currently displayed file. Rebuilt only
/// when the selected diff document or git generation changes. Not on every
/// scroll/drag frame.
pub(crate) struct CachedDiffLines {
    pub(crate) selection: DiffSelectionKey,
    pub(crate) generation: u64,
    pub(crate) lines: Rc<Vec<DiffLineView>>,
    pub(crate) hunk_headers: Rc<HashMap<usize, FileDiffHunk>>,
    /// Longest line in characters (not pixels).  Pixel width is derived at
    /// render time via `measured_char_width` so font/DPI changes don't stale
    /// the cache.
    pub(crate) max_line_chars: usize,
}

/// Cached filtered stash diff lines for the currently displayed stash file.
/// Rebuilt only when the selected stash file or theme changes. Not on every
/// scroll/drag frame.
pub(crate) struct CachedStashDiffLines {
    pub(crate) selection: StashFileSelection,
    pub(crate) theme_id: ThemeId,
    pub(crate) lines: Rc<Vec<DiffLineView>>,
    pub(crate) max_line_chars: usize,
    pub(crate) is_oversize: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictRenderLine {
    kind: DiffLineKind,
    raw_text: String,
    highlights: Option<Rc<[HighlightedSpan]>>,
    highlight_mode: ConflictHighlightMode,
    line_number: u32,
    start: usize,
    end: usize,
    block_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictHighlightMode {
    Stateful,
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictHighlightAnchor {
    line_index: usize,
    mode: ConflictHighlightMode,
    checkpoint: Option<HighlighterCheckpoint>,
}

struct CachedConflictLines {
    selection: DiffSelectionKey,
    generation: u64,
    version: u64,
    theme_id: ThemeId,
    raw_text: String,
    lines: Rc<Vec<ConflictRenderLine>>,
    anchors: Rc<Vec<ConflictHighlightAnchor>>,
    max_line_chars: usize,
}

/// Cached file tree for the current diff index generation. Rebuilt only when
/// the index generation changes, not on every drag/scroll frame.
struct CachedIndexTree {
    generation: Option<u64>,
    conflicted_tree: Vec<DiffTreeNode>,
    staged_tree: Vec<DiffTreeNode>,
    unstaged_tree: Vec<DiffTreeNode>,
    visible_file_order: Rc<Vec<DiffSelectionKey>>,
}

/// Lightweight snapshot of `DiffTabState` fields needed for rendering.
/// Extracted via a workspace borrow. No `FileDiffDocument` clone.
struct DiffRenderSnapshot {
    tree_width: f32,
    view_mode: DiffTabViewMode,
    selected_file: Option<DiffSelectionKey>,
    // Index state.
    index_loading: bool,
    index_error: Option<String>,
    has_index: bool,
    index_file_count: usize,
    conflicted_file_count: usize,
    index_branch: Option<String>,
    index_generation: Option<u64>,
    merge_state: Option<orcashell_git::MergeState>,
    repo_state_warning: Option<String>,
    // File state.
    file_loading: bool,
    file_error: Option<String>,
    /// Metadata for the currently loaded file (path, status, +/-).
    file_meta: Option<ChangedFile>,
    file_selection: Option<DiffSelectionKey>,
    file_generation: Option<u64>,
    is_oversize: bool,
    // Phase 4.5 CP3 additions.
    tracking_behind: usize,
    #[allow(dead_code)]
    // Push is always shown per spec; has_upstream is available for future error pre-check.
    has_upstream: bool,
    staged_file_count: usize,
    unstaged_file_count: usize,
    local_action_in_flight: bool,
    remote_op_in_flight: bool,
    commit_message: String,
    last_action_banner: Option<ActionBanner>,
    managed_worktree: Option<ManagedWorktreeSummary>,
    multi_select: HashSet<DiffSelectionKey>,
    remove_worktree_confirm: Option<RemoveWorktreeConfirm>,
    stash_entries: Vec<StashEntrySummary>,
    selected_stash: Option<Oid>,
    expanded_stash: Option<Oid>,
    stash_list_loading: bool,
    stash_list_error: Option<String>,
    stash_detail_loading: bool,
    stash_detail_error: Option<String>,
    stash_detail: Option<StashDetailDocument>,
    stash_file_loading: bool,
    stash_file_error: Option<String>,
    stash_file_selection: Option<StashFileSelection>,
    stash_file_meta: Option<ChangedFile>,
}

impl DiffRenderSnapshot {
    fn from_tab(tab: &DiffTabState) -> Self {
        let index_doc = tab.index.document.as_ref();
        let file_doc = tab.file.document.as_ref();
        let selected_file_meta = index_doc.and_then(|document| {
            let selected = tab.selected_file.as_ref()?;
            let files = match selected.section {
                DiffSectionKind::Conflicted => &document.conflicted_files,
                DiffSectionKind::Staged => &document.staged_files,
                DiffSectionKind::Unstaged => &document.unstaged_files,
            };
            files
                .iter()
                .find(|file| file.relative_path == selected.relative_path)
                .cloned()
        });
        let tracking = index_doc
            .map(|d| &d.tracking)
            .cloned()
            .unwrap_or(GitTrackingStatus {
                upstream_ref: None,
                ahead: 0,
                behind: 0,
            });
        Self {
            tree_width: tab.tree_width,
            view_mode: tab.view_mode,
            selected_file: tab.selected_file.clone(),
            index_loading: tab.index.loading,
            index_error: tab.index.error.clone(),
            has_index: index_doc.is_some(),
            index_file_count: index_doc.map_or(0, |d| {
                d.conflicted_files.len() + d.staged_files.len() + d.unstaged_files.len()
            }),
            conflicted_file_count: index_doc.map_or(0, |d| d.conflicted_files.len()),
            index_branch: index_doc.map(|d| d.snapshot.branch_name.clone()),
            index_generation: index_doc.map(|d| d.snapshot.generation),
            merge_state: index_doc.and_then(|d| d.merge_state.clone()),
            repo_state_warning: index_doc.and_then(|d| d.repo_state_warning.clone()),
            file_loading: tab.file.loading,
            file_error: tab.file.error.clone(),
            file_meta: file_doc.map(|d| d.file.clone()).or(selected_file_meta),
            file_selection: file_doc.map(|d| d.selection.clone()),
            file_generation: file_doc.map(|d| d.generation),
            is_oversize: file_doc.is_some_and(is_oversize_document),
            tracking_behind: tracking.behind,
            has_upstream: tracking.upstream_ref.is_some(),
            staged_file_count: index_doc.map_or(0, |d| d.staged_files.len()),
            unstaged_file_count: index_doc.map_or(0, |d| d.unstaged_files.len()),
            local_action_in_flight: tab.local_action_in_flight,
            remote_op_in_flight: tab.remote_op_in_flight,
            commit_message: tab.commit_message.clone(),
            last_action_banner: tab.last_action_banner.clone(),
            managed_worktree: tab.managed_worktree.clone(),
            multi_select: tab.multi_select.clone(),
            remove_worktree_confirm: tab.remove_worktree_confirm.clone(),
            stash_entries: tab
                .stash
                .list
                .document
                .as_ref()
                .map(|document| document.entries.clone())
                .unwrap_or_default(),
            selected_stash: tab.stash.selected_stash,
            expanded_stash: tab.stash.expanded_stash,
            stash_list_loading: tab.stash.list.loading,
            stash_list_error: tab.stash.list.error.clone(),
            stash_detail_loading: tab.stash.detail.loading,
            stash_detail_error: tab.stash.detail.error.clone(),
            stash_detail: tab.stash.detail.document.clone(),
            stash_file_loading: tab.stash.file_diff.loading,
            stash_file_error: tab.stash.file_diff.error.clone(),
            stash_file_selection: tab.stash.selected_file.clone(),
            stash_file_meta: tab
                .stash
                .file_diff
                .document
                .as_ref()
                .map(|document| document.file.clone()),
        }
    }

    fn any_action_in_flight(&self) -> bool {
        self.local_action_in_flight || self.remote_op_in_flight
    }

    fn show_commit_input(&self) -> bool {
        self.merge_state.is_none()
            && self.repo_state_warning.is_none()
            && self.staged_file_count > 0
    }

    fn show_staged_section(&self) -> bool {
        self.repo_state_warning.is_none() && self.staged_file_count > 0
    }
}

/// A text selection inside the diff pane, expressed in (line_index, char_index)
/// coordinates.  `start` is the anchor (where the mouse went down) and `end`
/// tracks the current pointer position.  The two may be in any order; call
/// [`DiffSelection::normalized`] to get (min, max).
#[derive(Debug, Clone, Copy)]
pub(crate) struct DiffSelection {
    pub(crate) start: (usize, usize),
    pub(crate) end: (usize, usize),
    pub(crate) is_selecting: bool,
}

impl DiffSelection {
    /// Return (start, end) in ascending (line, col) order.
    pub(crate) fn normalized(self) -> ((usize, usize), (usize, usize)) {
        let (a, b) = (self.start, self.end);
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            (a, b)
        } else {
            (b, a)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffTreeNode {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: DiffTreeNodeKind,
    pub(crate) children: Vec<DiffTreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffTreeNodeKind {
    Directory,
    File(ChangedFile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictAcceptChoice {
    Ours,
    Theirs,
    Both,
    Base,
}

#[derive(Default)]
struct DirectoryBuilder {
    directories: BTreeMap<String, DirectoryBuilder>,
    files: Vec<ChangedFile>,
}

struct DiffTooltipView {
    label: String,
}

impl Render for DiffTooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        div()
            .px(px(8.0))
            .py(px(4.0))
            .bg(rgb(palette.SURFACE))
            .border_1()
            .border_color(rgb(palette.BORDER_EMPHASIS))
            .rounded(px(6.0))
            .text_size(px(10.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(palette.BONE))
            .child(self.label.clone())
    }
}

#[derive(Clone, Copy)]
enum ActionButtonTone {
    Default,
    Primary,
    Destructive,
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub struct DiffExplorerView {
    workspace: Entity<WorkspaceState>,
    scope_root: PathBuf,
    focus_handle: FocusHandle,

    // Resize state (existing).
    bounds: Rc<RefCell<Bounds<Pixels>>>,
    resize_drag: Option<DiffResizeDrag>,

    // Diff pane virtualized scroll.
    diff_scroll_handle: UniformListScrollHandle,
    diff_scrollbar_drag: Option<ScrollbarDrag>,
    /// Bounds of the uniform-list area (excludes the file header bar).
    diff_list_bounds: Rc<RefCell<Bounds<Pixels>>>,

    // Horizontal scroll offset for long diff lines.
    diff_scroll_x: f32,

    // Text selection.
    selection: Option<DiffSelection>,

    /// Measured character advance width for JetBrains Mono at the current font
    /// size.  Computed once via the GPUI text system and cached.  Falls back to
    /// the constant `DEFAULT_CHAR_WIDTH` until the first measurement.
    measured_char_width: f32,

    /// Generation-keyed cache of filtered diff lines. Rebuilt only when the
    /// file path or git generation changes. Not on every scroll/drag frame.
    line_cache: Option<CachedDiffLines>,
    stash_line_cache: Option<CachedStashDiffLines>,
    conflict_line_cache: Option<CachedConflictLines>,
    /// Generation-keyed cache of the diff index tree. Rebuilt only when the
    /// index generation changes. Not on every drag/scroll frame.
    tree_cache: Option<CachedIndexTree>,

    // CP3: context menu and commit input.
    menu_request: ContextMenuRequest,
    prompt_dialog_request: PromptDialogRequest,
    commit_input: Option<Entity<TextInputState>>,
    conflict_select_anchor: Option<usize>,
    conflict_is_selecting: bool,
}

impl DiffExplorerView {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        scope_root: PathBuf,
        menu_request: ContextMenuRequest,
        prompt_dialog_request: PromptDialogRequest,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&workspace, |_this, _ws, cx| cx.notify())
            .detach();
        Self {
            workspace,
            scope_root,
            focus_handle: cx.focus_handle(),
            bounds: Rc::new(RefCell::new(Bounds::default())),
            resize_drag: None,
            diff_scroll_handle: UniformListScrollHandle::new(),
            diff_scrollbar_drag: None,
            diff_list_bounds: Rc::new(RefCell::new(Bounds::default())),
            diff_scroll_x: 0.0,
            selection: None,
            measured_char_width: DEFAULT_CHAR_WIDTH,
            line_cache: None,
            stash_line_cache: None,
            conflict_line_cache: None,
            tree_cache: None,
            menu_request,
            prompt_dialog_request,
            commit_input: None,
            conflict_select_anchor: None,
            conflict_is_selecting: false,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn invalidate_theme_cache(&mut self, cx: &mut Context<Self>) {
        self.line_cache = None;
        self.stash_line_cache = None;
        self.conflict_line_cache = None;
        cx.notify();
    }

    /// Measure the actual character advance width using the GPUI text system.
    /// Called once per render; the result is cached until the next render.
    fn measure_char_width(&mut self, window: &mut Window) {
        self.measured_char_width = measure_diff_char_width(window);
    }

    /// Extract a lightweight render snapshot from workspace state.
    /// Returns `None` if there is no diff tab for this scope.
    fn extract_render_snapshot(&self, cx: &App) -> Option<DiffRenderSnapshot> {
        let ws = self.workspace.read(cx);
        let tab = ws.diff_tab_state(&self.scope_root)?;
        Some(DiffRenderSnapshot::from_tab(tab))
    }

    /// Rebuild the line cache if the file document changed (new section/path or
    /// new generation). On cache hit this is O(1). Just a comparison.
    fn update_line_cache(&mut self, cx: &App, snap: &DiffRenderSnapshot) {
        if snap.view_mode != DiffTabViewMode::WorkingTree {
            self.line_cache = None;
            return;
        }
        let Some(selection) = snap.file_selection.as_ref() else {
            self.line_cache = None;
            return;
        };
        let Some(gen) = snap.file_generation else {
            self.line_cache = None;
            return;
        };

        // Cache hit. Nothing to do.
        if let Some(cached) = &self.line_cache {
            if line_cache_matches(&cached.selection, cached.generation, selection, gen) {
                return;
            }
        }

        // Cache miss. Reset per-file UI state.
        self.selection = None;
        self.diff_scroll_x = 0.0;

        // Borrow workspace to read the actual lines (one-time clone).
        let ws = self.workspace.read(cx);
        let Some(tab) = ws.diff_tab_state(&self.scope_root) else {
            return;
        };
        let Some(doc) = tab.file.document.as_ref() else {
            return;
        };

        let lines: Vec<DiffLineView> = doc
            .lines
            .iter()
            .filter(|l| l.kind != DiffLineKind::FileHeader)
            .cloned()
            .collect();
        let hunk_headers = doc
            .hunks
            .iter()
            .map(|hunk| (hunk.line_start, hunk.clone()))
            .collect::<HashMap<_, _>>();

        let max_line_chars = lines
            .iter()
            .map(|l| plain_text_len(&l.text, l.highlights.as_deref()))
            .max()
            .unwrap_or(0);

        self.line_cache = Some(CachedDiffLines {
            selection: selection.clone(),
            generation: gen,
            lines: Rc::new(lines),
            hunk_headers: Rc::new(hunk_headers),
            max_line_chars,
        });
    }

    fn update_stash_line_cache(&mut self, cx: &App, snap: &DiffRenderSnapshot) {
        let theme_id = theme::active_selection(cx).resolved_id;
        let Some(selection) =
            stash_line_cache_key(snap.view_mode, snap.stash_file_selection.as_ref())
        else {
            self.stash_line_cache = None;
            return;
        };

        if let Some(cached) = &self.stash_line_cache {
            if stash_line_cache_matches(&cached.selection, cached.theme_id, &selection, theme_id) {
                return;
            }
        }

        self.selection = None;
        self.diff_scroll_x = 0.0;

        let ws = self.workspace.read(cx);
        let Some(tab) = ws.diff_tab_state(&self.scope_root) else {
            self.stash_line_cache = None;
            return;
        };
        let Some(document) = tab
            .stash
            .file_diff
            .document
            .as_ref()
            .filter(|document| document.selection == selection)
        else {
            self.stash_line_cache = None;
            return;
        };

        self.stash_line_cache = Some(build_stash_line_cache(document, theme_id));
    }

    fn update_conflict_cache(&mut self, cx: &App, snap: &DiffRenderSnapshot) {
        if snap.view_mode != DiffTabViewMode::WorkingTree {
            self.conflict_line_cache = None;
            return;
        }
        let Some(selection) = snap.selected_file.as_ref() else {
            self.conflict_line_cache = None;
            return;
        };
        if selection.section != DiffSectionKind::Conflicted {
            self.conflict_line_cache = None;
            return;
        }

        let ws = self.workspace.read(cx);
        let Some(tab) = ws.diff_tab_state(&self.scope_root) else {
            self.conflict_line_cache = None;
            return;
        };
        let Some(ConflictDocumentState::Loaded(document)) =
            tab.conflict_editor.documents.get(&selection.relative_path)
        else {
            self.conflict_line_cache = None;
            return;
        };

        if self.conflict_line_cache.as_ref().is_some_and(|cached| {
            cached.selection == *selection
                && cached.generation == document.generation
                && cached.version == document.version
                && cached.theme_id == theme::active_selection(cx).resolved_id
        }) {
            return;
        }

        let theme_id = theme::active_selection(cx).resolved_id;
        let prior_cache = self.conflict_line_cache.as_ref().filter(|cached| {
            cached.selection == *selection
                && cached.generation == document.generation
                && cached.theme_id == theme_id
        });
        let (lines, anchors) = build_conflict_render_lines_with_cache(
            &document.raw_text,
            &document.file.relative_path,
            theme_id,
            prior_cache,
        );
        let max_line_chars = lines
            .iter()
            .map(|line| plain_text_len(&line.raw_text, line.highlights.as_deref()))
            .max()
            .unwrap_or(0);

        self.conflict_line_cache = Some(CachedConflictLines {
            selection: selection.clone(),
            generation: document.generation,
            version: document.version,
            theme_id,
            raw_text: document.raw_text.clone(),
            lines: Rc::new(lines),
            anchors: Rc::new(anchors),
            max_line_chars,
        });
    }

    /// Rebuild the cached diff tree if the index generation changed.
    fn update_tree_cache(&mut self, cx: &App, snap: &DiffRenderSnapshot) {
        if snap.view_mode != DiffTabViewMode::WorkingTree {
            self.tree_cache = None;
            return;
        }
        if self
            .tree_cache
            .as_ref()
            .is_some_and(|cached| cached.generation == snap.index_generation)
        {
            return;
        }

        let ws = self.workspace.read(cx);
        let Some(tab) = ws.diff_tab_state(&self.scope_root) else {
            self.tree_cache = None;
            return;
        };

        let (conflicted_tree, staged_tree, unstaged_tree) = tab
            .index
            .document
            .as_ref()
            .map(|document| {
                let conflicted = build_diff_tree(&document.conflicted_files);
                let staged = build_diff_tree(&document.staged_files);
                let unstaged = build_diff_tree(&document.unstaged_files);
                (conflicted, staged, unstaged)
            })
            .unwrap_or_default();

        // Build visible file order: conflicted files first, then staged, then unstaged.
        let mut visible_file_order = Vec::new();
        collect_file_keys(
            &conflicted_tree,
            DiffSectionKind::Conflicted,
            &mut visible_file_order,
        );
        collect_file_keys(
            &staged_tree,
            DiffSectionKind::Staged,
            &mut visible_file_order,
        );
        collect_file_keys(
            &unstaged_tree,
            DiffSectionKind::Unstaged,
            &mut visible_file_order,
        );

        self.tree_cache = Some(CachedIndexTree {
            generation: snap.index_generation,
            conflicted_tree,
            staged_tree,
            unstaged_tree,
            visible_file_order: Rc::new(visible_file_order),
        });
    }

    /// Ensure the commit input entity exists and return it.
    fn ensure_commit_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInputState> {
        if let Some(ref input) = self.commit_input {
            return input.clone();
        }
        let palette = theme::active(cx);
        let text_color: Hsla = rgb(palette.BONE).into();
        let placeholder_color: Hsla = rgb(palette.SLATE).into();
        let cursor_color: Hsla = rgb(palette.ORCA_BLUE).into();
        let selection_bg: Hsla = rgba(theme::with_alpha(palette.ORCA_BLUE, 0x40)).into();
        let input = cx.new(|cx| {
            let mut state = TextInputState::new(
                text_color,
                placeholder_color,
                cursor_color,
                selection_bg,
                cx,
            );
            state.set_placeholder("Commit message...");
            state
        });
        self.commit_input = Some(input.clone());
        input
    }

    fn with_selected_conflict_document<R>(
        &self,
        cx: &App,
        read: impl FnOnce(&ConflictEditorDocument) -> R,
    ) -> Option<R> {
        let ws = self.workspace.read(cx);
        match ws.selected_conflict_document(&self.scope_root)? {
            ConflictDocumentState::Loaded(document) => Some(read(document)),
            ConflictDocumentState::Unavailable(_) => None,
        }
    }

    fn update_selected_conflict_document(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut ConflictEditorDocument),
    ) -> bool {
        let scope_root = self.scope_root.clone();
        let mut changed = false;
        self.workspace.update(cx, |ws, cx| {
            let Some(document) = ws.selected_conflict_document_mut(&scope_root) else {
                return;
            };
            update(document);
            changed = true;
            cx.notify();
        });
        changed
    }

    fn move_conflict_active_block(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some((block_total, active_block_index, block, scroll_y)) = self
            .with_selected_conflict_document(cx, |document| {
                let active = document.active_block_index;
                let total = document.blocks.len();
                let next_index = navigated_conflict_block_index(total, active, forward)?;
                let block = document.blocks[next_index].clone();
                let scroll_y =
                    line_index_for_offset_in_text(&document.raw_text, block.whole_block.start)
                        as f32
                        * LINE_HEIGHT;
                Some((total, active, block, scroll_y))
            })
            .flatten()
        else {
            return;
        };
        let _ = (block_total, active_block_index);

        self.update_selected_conflict_document(cx, move |document| {
            document.active_block_index = Some(block.block_index);
            document.cursor_pos = block.whole_block.start;
            document.selection_range = None;
            document.scroll_y = scroll_y;
        });
    }

    fn replace_conflict_selection(&mut self, replacement: &str, cx: &mut Context<Self>) -> bool {
        self.update_selected_conflict_document(cx, |document| {
            replace_document_selection(document, replacement);
        })
    }

    fn delete_conflict_backward(&mut self, cx: &mut Context<Self>) -> bool {
        self.update_selected_conflict_document(cx, |document| {
            delete_document_backward(document);
        })
    }

    fn delete_conflict_forward(&mut self, cx: &mut Context<Self>) -> bool {
        self.update_selected_conflict_document(cx, |document| {
            delete_document_forward(document);
        })
    }

    fn indent_conflict_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.update_selected_conflict_document(cx, |document| {
            indent_document_selection(document);
        })
    }

    fn outdent_conflict_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.update_selected_conflict_document(cx, |document| {
            outdent_document_selection(document);
        })
    }

    fn save_conflict_document(&mut self, cx: &mut Context<Self>) {
        let scope_root = self.scope_root.clone();
        self.workspace.update(cx, |ws, cx| {
            ws.save_conflict_document(&scope_root, cx);
        });
    }

    fn reset_conflict_document(&mut self, cx: &mut Context<Self>) {
        let scope_root = self.scope_root.clone();
        self.workspace.update(cx, |ws, cx| {
            ws.reset_conflict_document(&scope_root, cx);
        });
    }

    fn mark_conflicts_resolved(&mut self, cx: &mut Context<Self>) {
        let scope_root = self.scope_root.clone();
        self.workspace.update(cx, |ws, cx| {
            ws.mark_conflicts_resolved(&scope_root, cx);
        });
    }

    fn apply_active_conflict_choice(
        &mut self,
        choice: ConflictAcceptChoice,
        cx: &mut Context<Self>,
    ) {
        self.update_selected_conflict_document(cx, |document| {
            let Some(block_index) = document.active_block_index else {
                return;
            };
            let Some(block) = document.blocks.get(block_index).cloned() else {
                return;
            };
            let replacement = match choice {
                ConflictAcceptChoice::Ours => document.raw_text[block.ours.clone()].to_string(),
                ConflictAcceptChoice::Theirs => document.raw_text[block.theirs.clone()].to_string(),
                ConflictAcceptChoice::Both => {
                    let mut text = document.raw_text[block.ours.clone()].to_string();
                    text.push_str(&document.raw_text[block.theirs.clone()]);
                    text
                }
                ConflictAcceptChoice::Base => block
                    .base
                    .as_ref()
                    .map(|range| document.raw_text[range.clone()].to_string())
                    .unwrap_or_default(),
            };
            replace_document_range(document, block.whole_block, &replacement);
        });
    }

    fn copy_conflict_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self
            .with_selected_conflict_document(cx, |document| {
                let selection = document.selection_range.as_ref()?;
                if selection.is_empty() {
                    return None;
                }
                Some(document.raw_text[selection.clone()].to_string())
            })
            .flatten()
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn paste_conflict_selection(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        let _ = self.replace_conflict_selection(&text, cx);
    }

    fn set_conflict_cursor(
        &mut self,
        offset: usize,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.conflict_select_anchor.unwrap_or(offset);
        self.update_selected_conflict_document(cx, move |document| {
            let clamped = clamp_offset_to_char_boundary(&document.raw_text, offset);
            if extend_selection {
                let start = anchor.min(clamped);
                let end = anchor.max(clamped);
                document.selection_range = (start < end).then_some(start..end);
            } else {
                document.selection_range = None;
            }
            document.cursor_pos = clamped;
            document.active_block_index =
                active_block_index_for_cursor(&document.blocks, document.cursor_pos);
        });
    }

    fn move_conflict_cursor_horizontal(
        &mut self,
        forward: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((selection_range, cursor_pos, raw_text)) =
            self.with_selected_conflict_document(cx, |document| {
                (
                    document.selection_range.clone(),
                    document.cursor_pos,
                    document.raw_text.clone(),
                )
            })
        else {
            return;
        };
        if !extend_selection {
            if let Some(selection) = selection_range.as_ref() {
                let target = if forward {
                    selection.end
                } else {
                    selection.start
                };
                self.conflict_select_anchor = Some(target);
                self.set_conflict_cursor(target, false, cx);
                return;
            }
        }
        let target = if forward {
            next_char_boundary(&raw_text, cursor_pos)
        } else {
            previous_char_boundary(&raw_text, cursor_pos)
        };
        if extend_selection && self.conflict_select_anchor.is_none() {
            self.conflict_select_anchor = Some(cursor_pos);
        } else if !extend_selection {
            self.conflict_select_anchor = Some(target);
        }
        self.set_conflict_cursor(target, extend_selection, cx);
    }

    fn move_conflict_cursor_vertical(
        &mut self,
        direction: i32,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(cursor_pos) =
            self.with_selected_conflict_document(cx, |document| document.cursor_pos)
        else {
            return;
        };
        let Some(lines) = self
            .conflict_line_cache
            .as_ref()
            .map(|cached| cached.lines.clone())
            .or_else(|| {
                self.with_selected_conflict_document(cx, |document| {
                    Rc::new(build_conflict_render_lines(
                        &document.raw_text,
                        &document.file.relative_path,
                        theme::active_selection(cx).resolved_id,
                    ))
                })
            })
        else {
            return;
        };
        let (line_index, column) = line_and_column_for_offset(lines.as_ref(), cursor_pos);
        let target_line = if direction < 0 {
            line_index.saturating_sub(1)
        } else {
            (line_index + 1).min(lines.len().saturating_sub(1))
        };
        let target_offset = byte_offset_for_line_column(lines.as_ref(), target_line, column);
        if extend_selection && self.conflict_select_anchor.is_none() {
            self.conflict_select_anchor = Some(cursor_pos);
        } else if !extend_selection {
            self.conflict_select_anchor = Some(target_offset);
        }
        self.set_conflict_cursor(target_offset, extend_selection, cx);
    }

    fn move_conflict_cursor_line_edge(
        &mut self,
        to_end: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(cursor_pos) =
            self.with_selected_conflict_document(cx, |document| document.cursor_pos)
        else {
            return;
        };
        let Some(lines) = self
            .conflict_line_cache
            .as_ref()
            .map(|cached| cached.lines.clone())
            .or_else(|| {
                self.with_selected_conflict_document(cx, |document| {
                    Rc::new(build_conflict_render_lines(
                        &document.raw_text,
                        &document.file.relative_path,
                        theme::active_selection(cx).resolved_id,
                    ))
                })
            })
        else {
            return;
        };
        let (line_index, _) = line_and_column_for_offset(lines.as_ref(), cursor_pos);
        let line = &lines[line_index];
        let target = if to_end {
            trim_newline_end(line)
        } else {
            line.start
        };
        if extend_selection && self.conflict_select_anchor.is_none() {
            self.conflict_select_anchor = Some(cursor_pos);
        } else if !extend_selection {
            self.conflict_select_anchor = Some(target);
        }
        self.set_conflict_cursor(target, extend_selection, cx);
    }

    fn select_all_conflict_text(&mut self, cx: &mut Context<Self>) {
        self.update_selected_conflict_document(cx, |document| {
            document.cursor_pos = document.raw_text.len();
            document.selection_range =
                (!document.raw_text.is_empty()).then_some(0..document.raw_text.len());
            document.active_block_index =
                active_block_index_for_cursor(&document.blocks, document.cursor_pos);
        });
        self.conflict_select_anchor = Some(0);
    }

    fn handle_conflict_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cached) = self.conflict_line_cache.as_ref() else {
            return false;
        };

        let (delta_x, delta_y) = scroll_delta_components(event.delta, self.measured_char_width);
        if delta_x.abs() <= 0.5 && delta_y.abs() <= 0.5 {
            return false;
        }

        let viewport_w = f32::from(self.diff_list_bounds.borrow().size.width);
        let viewport_h = f32::from(self.diff_list_bounds.borrow().size.height);
        let max_scroll_x =
            (max_diff_content_width(cached.max_line_chars, self.measured_char_width) - viewport_w)
                .max(0.0);
        let max_scroll_y = (cached.lines.len() as f32 * LINE_HEIGHT - viewport_h).max(0.0);

        let current_scroll_y = {
            let state = self.diff_scroll_handle.0.borrow();
            -f32::from(state.base_handle.offset().y)
        };
        let next_scroll_y = (current_scroll_y - delta_y).clamp(0.0, max_scroll_y);

        self.update_selected_conflict_document(cx, move |document| {
            document.scroll_x = (document.scroll_x - delta_x).clamp(0.0, max_scroll_x);
            document.scroll_y = next_scroll_y;
        });

        let state = self.diff_scroll_handle.0.borrow();
        state
            .base_handle
            .set_offset(point(px(0.0), px(-next_scroll_y)));
        true
    }

    fn handle_conflict_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;
        let shift = mods.shift;

        match key {
            _ if key_acts_as_backward_delete(key) => self.delete_conflict_backward(cx),
            _ if key_acts_as_forward_delete(key) => self.delete_conflict_forward(cx),
            "tab" if shift => self.outdent_conflict_selection(cx),
            "tab" => self.indent_conflict_selection(cx),
            "left" => {
                self.move_conflict_cursor_horizontal(false, shift, cx);
                true
            }
            "right" => {
                self.move_conflict_cursor_horizontal(true, shift, cx);
                true
            }
            "up" => {
                self.move_conflict_cursor_vertical(-1, shift, cx);
                true
            }
            "down" => {
                self.move_conflict_cursor_vertical(1, shift, cx);
                true
            }
            "home" => {
                self.move_conflict_cursor_line_edge(false, shift, cx);
                true
            }
            "end" => {
                self.move_conflict_cursor_line_edge(true, shift, cx);
                true
            }
            "a" if mods.platform || mods.control => {
                self.select_all_conflict_text(cx);
                true
            }
            "c" if mods.platform || mods.control => {
                self.copy_conflict_selection(cx);
                true
            }
            "x" if mods.platform || mods.control => {
                let copied = self
                    .with_selected_conflict_document(cx, |document| {
                        document
                            .selection_range
                            .as_ref()
                            .map(|range| document.raw_text[range.clone()].to_string())
                    })
                    .flatten();
                if let Some(text) = copied {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.replace_conflict_selection("", cx)
                } else {
                    true
                }
            }
            "v" if mods.platform || mods.control => {
                self.paste_conflict_selection(cx);
                true
            }
            "s" if mods.platform || mods.control => {
                self.save_conflict_document(cx);
                true
            }
            "enter" => self.replace_conflict_selection("\n", cx),
            "escape" | "shift" | "control" | "alt" | "meta" | "capslock" | "f1" | "f2" | "f3"
            | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" | "pageup"
            | "pagedown" => false,
            _ => {
                if let Some(ref text) = event.keystroke.key_char {
                    if !text.is_empty() && !text.chars().next().is_none_or(|ch| ch.is_control()) {
                        return self.replace_conflict_selection(text, cx);
                    }
                }
                false
            }
        }
    }

    // ------------------------------------------------------------------
    // Header
    // ------------------------------------------------------------------

    fn render_header(&self, scope_root: &Path, snap: &DiffRenderSnapshot, cx: &App) -> Div {
        let palette = theme::active(cx);
        let ws = self.workspace.read(cx);
        let live_snapshot = ws.git_scope_snapshot(scope_root);
        let branch_name = live_snapshot
            .map(|s| s.branch_name.clone())
            .or_else(|| snap.index_branch.clone())
            .unwrap_or_else(|| "detached".to_string());
        let remote_names = live_snapshot.map(|s| s.remotes.clone()).unwrap_or_default();
        let latest_generation = live_snapshot.map(|s| s.generation);
        let displayed_generation = snap.file_generation.or(snap.index_generation);
        let is_stale = matches!(
            (latest_generation, displayed_generation),
            (Some(latest), Some(displayed)) if displayed < latest
        );
        let is_loading = snap.index_loading || snap.file_loading;

        // Left side: branch + scope path.
        let left = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.PATCH))
                    .child(branch_name.clone()),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.FOG))
                    .child(scope_root.display().to_string()),
            );

        // Center badges.
        let mut badges = div().flex().items_center().gap(px(4.0));
        if is_loading {
            badges = badges.child(Self::header_status_text("loading", palette.ORCA_BLUE));
        }
        if is_stale {
            badges = badges.child(Self::header_status_text("stale", palette.STATUS_AMBER));
        }
        if let Some(generation) = latest_generation {
            badges = badges.child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.FOG))
                    .child(format!("gen {generation}")),
            );
        }

        // Right side: action buttons OR remove-worktree confirmation.
        let any_in_flight = snap.any_action_in_flight();
        let unsupported_repo_state =
            snap.merge_state.is_none() && snap.repo_state_warning.as_ref().is_some();
        let mut actions = div().flex().items_center().gap(px(4.0));

        if let Some(merge_state) = snap.merge_state.as_ref() {
            let ws_complete = self.workspace.clone();
            let scope_complete = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-complete-merge",
                "Complete Merge",
                any_in_flight || !merge_state.can_complete,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_complete.update(cx, |ws, cx| {
                        ws.dispatch_complete_merge(&scope_complete, cx);
                    });
                },
            ));

            let ws_abort = self.workspace.clone();
            let scope_abort = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-abort-merge",
                "Abort Merge",
                any_in_flight || !merge_state.can_abort,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_abort.update(cx, |ws, cx| {
                        ws.dispatch_abort_merge(&scope_abort, cx);
                    });
                },
            ));
        } else if let Some(confirm) = &snap.remove_worktree_confirm {
            // Inline confirmation UI replaces all action buttons.
            let branch_label = snap
                .managed_worktree
                .as_ref()
                .map(|m| m.branch_name.clone())
                .unwrap_or_else(|| "branch".to_string());
            let delete_branch = confirm.delete_branch;

            let ws_toggle = self.workspace.clone();
            let scope_toggle = scope_root.to_path_buf();
            let ws_cancel = self.workspace.clone();
            let scope_cancel = scope_root.to_path_buf();
            let ws_confirm = self.workspace.clone();
            let scope_confirm = scope_root.to_path_buf();

            actions = actions
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.BONE))
                        .child(format!("Remove worktree {branch_label}?")),
                )
                .child(
                    div()
                        .id("diff-remove-toggle-branch")
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(2.0))
                        .border_1()
                        .border_color(rgb(palette.SURFACE))
                        .hover(|s| s.bg(rgba(theme::with_alpha(palette.SURFACE, 0x40))))
                        .on_click(
                            move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                                ws_toggle.update(cx, |ws, cx| {
                                    ws.toggle_remove_delete_branch(&scope_toggle, cx);
                                });
                            },
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_family(DIFF_FONT_FAMILY)
                                .text_color(rgb(palette.FOG))
                                .child(if delete_branch {
                                    "\u{2611} Delete branch"
                                } else {
                                    "\u{2610} Delete branch"
                                }),
                        ),
                )
                .child(Self::action_bar_button(
                    &palette,
                    "diff-remove-cancel",
                    "Cancel",
                    false,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_cancel.update(cx, |ws, cx| {
                            ws.cancel_remove_worktree(&scope_cancel, cx);
                        });
                    },
                ))
                .child(
                    // Destructive "Remove Worktree" button with coral styling.
                    div()
                        .id(ElementId::Name("diff-remove-confirm".into()))
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(rgb(palette.STATUS_CORAL))
                        .text_size(px(10.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.STATUS_CORAL))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(theme::with_alpha(palette.STATUS_CORAL, 0x18))))
                        .child("Remove Worktree".to_string())
                        .on_click(
                            move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                                ws_confirm.update(cx, |ws, cx| {
                                    ws.confirm_remove_worktree(&scope_confirm, cx);
                                });
                            },
                        ),
                );
        } else {
            // Header buttons: Pull, Push/Publish, Discard All, Merge, Remove
            // - always visible, dimmed when not applicable.
            if snap.view_mode == DiffTabViewMode::Stashes {
                let ws_back_to_diff = self.workspace.clone();
                let scope_back_to_diff = scope_root.to_path_buf();
                actions = actions.child(Self::action_bar_primary_button(
                    &palette,
                    "diff-action-back-to-diff",
                    "Back to Diff",
                    any_in_flight,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_back_to_diff.update(cx, |ws, cx| {
                            ws.exit_stash_mode(&scope_back_to_diff, cx);
                        });
                    },
                ));
            }

            // Pull button. Disabled when not behind.
            let pull_disabled =
                unsupported_repo_state || any_in_flight || snap.tracking_behind == 0;
            let pull_label = if snap.tracking_behind > 0 {
                format!("Pull ({})", snap.tracking_behind)
            } else {
                "Pull".to_string()
            };
            let ws_pull = self.workspace.clone();
            let scope_pull = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-pull",
                &pull_label,
                pull_disabled,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_pull.update(cx, |ws, cx| {
                        ws.dispatch_pull(&scope_pull, cx);
                    });
                },
            ));

            if snap.has_upstream {
                let ws_push = self.workspace.clone();
                let scope_push = scope_root.to_path_buf();
                actions = actions.child(Self::action_bar_button(
                    &palette,
                    "diff-action-push",
                    "Push",
                    unsupported_repo_state || any_in_flight,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_push.update(cx, |ws, cx| {
                            ws.dispatch_push(&scope_push, cx);
                        });
                    },
                ));
            } else {
                let ws_publish = self.workspace.clone();
                let scope_publish = scope_root.to_path_buf();
                let branch_name = branch_name.clone();
                let remotes = remote_names.clone();
                let prompt_dialog_request = self.prompt_dialog_request.clone();
                actions = actions.child(Self::action_bar_button(
                    &palette,
                    "diff-action-publish",
                    "Publish",
                    unsupported_repo_state || any_in_flight || live_snapshot.is_none(),
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        if remotes.is_empty() {
                            ws_publish.update(cx, |ws, cx| {
                                ws.show_diff_action_warning(
                                    &scope_publish,
                                    "No git remotes are configured for this repository."
                                        .to_string(),
                                    cx,
                                );
                            });
                            return;
                        }

                        if remotes.len() == 1 {
                            let remote_name = remotes[0].clone();
                            ws_publish.update(cx, |ws, cx| {
                                ws.dispatch_publish(&scope_publish, remote_name, cx);
                            });
                            return;
                        }

                        *prompt_dialog_request.borrow_mut() = Some(publish_remote_prompt_spec(
                            scope_publish.clone(),
                            branch_name.clone(),
                            remotes.clone(),
                        ));
                        ws_publish.update(cx, |_ws, cx| cx.notify());
                    },
                ));
            }

            let discard_all_disabled =
                unsupported_repo_state || any_in_flight || snap.unstaged_file_count == 0;
            let ws_discard_all = self.workspace.clone();
            let scope_discard_all = scope_root.to_path_buf();
            let prompt_dialog_request = self.prompt_dialog_request.clone();
            actions = actions.child(
                Self::action_bar_destructive_button(
                    &palette,
                    "diff-action-discard-all",
                    "Discard All",
                    discard_all_disabled,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        *prompt_dialog_request.borrow_mut() =
                            Some(discard_all_prompt_spec(scope_discard_all.clone()));
                        ws_discard_all.update(cx, |_ws, cx| cx.notify());
                    },
                )
                .tooltip(move |_window, cx| {
                    cx.new(|_| DiffTooltipView {
                        label: "Discard all unstaged changes. Staged changes stay intact.".into(),
                    })
                    .into()
                }),
            );

            let stash_create_disabled =
                unsupported_repo_state || any_in_flight || snap.index_file_count == 0;
            let ws_stash_create = self.workspace.clone();
            let scope_stash_create = scope_root.to_path_buf();
            let prompt_dialog_request = self.prompt_dialog_request.clone();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-create-stash",
                "Stash…",
                stash_create_disabled,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    *prompt_dialog_request.borrow_mut() =
                        Some(create_stash_prompt_spec(scope_stash_create.clone()));
                    ws_stash_create.update(cx, |_ws, cx| cx.notify());
                },
            ));

            if snap.view_mode == DiffTabViewMode::WorkingTree {
                let stash_label =
                    stash_header_button_label(snap.view_mode, snap.stash_entries.len());
                let ws_stash_mode = self.workspace.clone();
                let scope_stash_mode = scope_root.to_path_buf();
                actions = actions.child(Self::action_bar_button(
                    &palette,
                    "diff-action-open-stashes",
                    &stash_label,
                    false,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_stash_mode.update(cx, |ws, cx| {
                            ws.enter_stash_mode(&scope_stash_mode, cx);
                        });
                    },
                ));
            }

            // Merge button. Disabled when no managed worktree.
            let merge_disabled =
                unsupported_repo_state || any_in_flight || snap.managed_worktree.is_none();
            let ws_merge = self.workspace.clone();
            let scope_merge = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-merge",
                "Merge",
                merge_disabled,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_merge.update(cx, |ws, cx| {
                        ws.dispatch_merge_back(&scope_merge, cx);
                    });
                },
            ));

            // Remove button. Disabled when no managed worktree.
            let remove_disabled =
                unsupported_repo_state || any_in_flight || snap.managed_worktree.is_none();
            let ws_remove = self.workspace.clone();
            let scope_remove = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                &palette,
                "diff-action-remove",
                "Remove",
                remove_disabled,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_remove.update(cx, |ws, cx| {
                        ws.begin_remove_worktree_confirm(&scope_remove, cx);
                    });
                },
            ));
        }

        div()
            .w_full()
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .bg(rgb(palette.DEEP))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .child(left)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(badges)
                    .child(actions),
            )
    }

    fn header_status_text(label: &str, color: u32) -> Div {
        div()
            .text_size(px(9.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(color))
            .child(label.to_string())
    }

    // ------------------------------------------------------------------
    // Tree-pane action bar (Stage All, Unstage All, Commit)
    // ------------------------------------------------------------------

    /// Render the always-visible action bar at the top of the tree pane.
    /// Height matches the diff pane's file header bar.
    fn render_action_bar(&self, scope_root: &Path, snap: &DiffRenderSnapshot, cx: &App) -> Div {
        let palette = theme::active(cx);
        let any_in_flight = snap.any_action_in_flight();

        // Stage All. Enabled when unstaged files exist.
        let stage_all_disabled = any_in_flight || snap.unstaged_file_count == 0;
        let ws_stage = self.workspace.clone();
        let scope_stage = scope_root.to_path_buf();

        // Unstage All. Enabled when staged files exist.
        let unstage_all_disabled = any_in_flight || snap.staged_file_count == 0;
        let ws_unstage = self.workspace.clone();
        let scope_unstage = scope_root.to_path_buf();

        // Commit. Enabled when staged files exist and message non-empty.
        let commit_disabled =
            any_in_flight || snap.staged_file_count == 0 || snap.commit_message.trim().is_empty();
        let ws_commit = self.workspace.clone();
        let scope_commit = scope_root.to_path_buf();

        div()
            .w_full()
            .flex_shrink_0()
            .px(px(12.0))
            .py(px(7.5))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .bg(rgb(palette.DEEP))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(
                Self::action_bar_button(
                    &palette,
                    "action-stage-all",
                    "Stage All",
                    stage_all_disabled,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_stage.update(cx, |ws, cx| {
                            ws.stage_all(&scope_stage, cx);
                        });
                    },
                )
                .flex_1(),
            )
            .child(
                Self::action_bar_button(
                    &palette,
                    "action-unstage-all",
                    "Unstage All",
                    unstage_all_disabled,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_unstage.update(cx, |ws, cx| {
                            ws.unstage_all(&scope_unstage, cx);
                        });
                    },
                )
                .flex_1(),
            )
            .child(
                Self::action_bar_button(
                    &palette,
                    "action-commit",
                    "Commit",
                    commit_disabled,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_commit.update(cx, |ws, cx| {
                            ws.commit_staged(&scope_commit, cx);
                        });
                    },
                )
                .flex_1(),
            )
    }

    fn render_stash_action_bar(
        &self,
        scope_root: &Path,
        snap: &DiffRenderSnapshot,
        cx: &App,
    ) -> Div {
        let palette = theme::active(cx);
        let any_in_flight = snap.any_action_in_flight();
        let selected_entry = snap.selected_stash.and_then(|stash_oid| {
            snap.stash_entries
                .iter()
                .find(|entry| entry.stash_oid == stash_oid)
        });
        let selected_label = selected_entry
            .map(|entry| entry.label.clone())
            .unwrap_or_else(|| "stash".to_string());
        let pop_label = selected_label.clone();
        let drop_label = selected_label.clone();

        let ws_apply = self.workspace.clone();
        let scope_apply = scope_root.to_path_buf();
        let ws_pop = self.workspace.clone();
        let scope_pop = scope_root.to_path_buf();
        let ws_drop = self.workspace.clone();
        let scope_drop = scope_root.to_path_buf();
        let pop_prompt_dialog_request = self.prompt_dialog_request.clone();
        let drop_prompt_dialog_request = self.prompt_dialog_request.clone();

        div()
            .w_full()
            .flex_shrink_0()
            .px(px(12.0))
            .py(px(7.5))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .bg(rgb(palette.DEEP))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(
                Self::action_bar_button(
                    &palette,
                    "stash-apply",
                    "Apply",
                    any_in_flight || snap.selected_stash.is_none(),
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_apply.update(cx, |ws, cx| {
                            ws.apply_selected_stash(&scope_apply, cx);
                        });
                    },
                )
                .flex_1(),
            )
            .child(
                Self::action_bar_button(
                    &palette,
                    "stash-pop",
                    "Pop",
                    any_in_flight || snap.selected_stash.is_none(),
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        *pop_prompt_dialog_request.borrow_mut() =
                            Some(pop_stash_prompt_spec(scope_pop.clone(), pop_label.clone()));
                        ws_pop.update(cx, |_ws, cx| cx.notify());
                    },
                )
                .flex_1(),
            )
            .child(
                Self::action_bar_destructive_button(
                    &palette,
                    "stash-drop",
                    "Drop",
                    any_in_flight || snap.selected_stash.is_none(),
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        *drop_prompt_dialog_request.borrow_mut() = Some(drop_stash_prompt_spec(
                            scope_drop.clone(),
                            drop_label.clone(),
                        ));
                        ws_drop.update(cx, |_ws, cx| cx.notify());
                    },
                )
                .flex_1(),
            )
    }

    fn render_stash_tree_pane(
        &mut self,
        snap: &DiffRenderSnapshot,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = theme::active(cx);
        let pane = div()
            .id(ElementId::Name(
                format!("diff-stash-tree-{}", self.scope_root.display()).into(),
            ))
            .w(px(snap.tree_width))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(palette.DEEP))
            .border_r_1()
            .border_color(rgb(palette.SURFACE))
            .overflow_y_scroll();

        let mut list = div().w_full().flex().flex_col();
        list = list.child(self.render_stash_action_bar(&self.scope_root, snap, cx));

        if snap.stash_list_loading && snap.stash_entries.is_empty() {
            return pane.child(list.child(Self::empty_panel_message(
                &palette,
                "Loading stashes...",
                Some("Git is collecting stash entries for this scope."),
            )));
        }

        if let Some(error) = snap.stash_list_error.as_ref() {
            if snap.stash_entries.is_empty() {
                return pane.child(list.child(Self::empty_panel_message(
                    &palette,
                    "Could not load stashes",
                    Some(error.as_str()),
                )));
            }
        }

        if snap.stash_entries.is_empty() {
            return pane.child(list.child(Self::empty_panel_message(
                &palette,
                "No stashes",
                Some("Create a stash from the header to browse it here."),
            )));
        }

        list = list.child(Self::section_header(
            &palette,
            "STASHES",
            snap.stash_entries.len(),
        ));

        for entry in &snap.stash_entries {
            let is_selected = snap.selected_stash == Some(entry.stash_oid);
            let is_expanded = snap.expanded_stash == Some(entry.stash_oid);
            let ws_select = self.workspace.clone();
            let scope_select = self.scope_root.clone();
            let stash_oid = entry.stash_oid;
            let entry_id = ElementId::Name(
                format!("stash-row-{}-{}", self.scope_root.display(), entry.label).into(),
            );
            let title = stash_display_title(&entry.message, &entry.label);
            let relative_time = format_relative_time(entry.committed_at_unix);

            let mut row = div()
                .id(entry_id)
                .w_full()
                .px(px(8.0))
                .py(px(6.0))
                .border_l_2()
                .cursor_pointer()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .when(is_selected, |row| {
                    row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x16)))
                        .border_color(rgb(palette.ORCA_BLUE))
                })
                .when(!is_selected, |row| {
                    row.border_color(rgb(palette.DEEP))
                        .hover(|style| style.bg(rgba(theme::with_alpha(palette.CURRENT, 0x80))))
                })
                .on_click(
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws_select.update(cx, |ws, cx| {
                            ws.select_stash(&scope_select, stash_oid, cx);
                        });
                    },
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.0))
                                .font_family(DIFF_FONT_FAMILY)
                                .text_color(rgb(if is_selected {
                                    palette.PATCH
                                } else {
                                    palette.BONE
                                }))
                                .child(format!(
                                    "{} {}",
                                    if is_expanded { "\u{25BE}" } else { "\u{25B8}" },
                                    title
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_family(DIFF_FONT_FAMILY)
                                .text_color(rgb(palette.FOG))
                                .child(relative_time),
                        ),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.FOG))
                        .child(entry.label.clone()),
                );
            if entry.includes_untracked {
                row = row.child(
                    div()
                        .text_size(px(10.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.SEAFOAM))
                        .child("includes untracked".to_string()),
                );
            }
            list = list.child(row);

            if !is_expanded {
                continue;
            }

            match (
                snap.stash_detail
                    .as_ref()
                    .filter(|detail| detail.stash_oid == entry.stash_oid),
                snap.stash_detail_loading && snap.selected_stash == Some(entry.stash_oid),
                snap.stash_detail_error.as_ref(),
            ) {
                (Some(detail), _, _) => {
                    let tree = build_diff_tree(&detail.files);
                    for node in &tree {
                        list = list.children(self.render_stash_tree_nodes(
                            std::slice::from_ref(node),
                            1,
                            entry.stash_oid,
                            snap,
                            &palette,
                        ));
                    }
                }
                (None, true, _) => {
                    list = list.child(
                        div()
                            .w_full()
                            .px(px(8.0))
                            .py(px(6.0))
                            .pl(px(22.0))
                            .text_size(px(10.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.FOG))
                            .child("Loading stash contents...".to_string()),
                    );
                }
                (None, false, Some(error)) if snap.selected_stash == Some(entry.stash_oid) => {
                    list = list.child(
                        div()
                            .w_full()
                            .px(px(8.0))
                            .py(px(6.0))
                            .pl(px(22.0))
                            .text_size(px(10.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.STATUS_AMBER))
                            .child(error.clone()),
                    );
                }
                _ => {}
            }
        }

        if let Some(error) = snap.stash_list_error.as_ref() {
            list = list.child(
                div()
                    .m(px(8.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.STATUS_AMBER))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                    .text_size(px(11.0))
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(error.clone()),
            );
        }

        pane.child(list)
    }

    fn render_stash_tree_nodes(
        &self,
        nodes: &[DiffTreeNode],
        depth: usize,
        stash_oid: Oid,
        snap: &DiffRenderSnapshot,
        palette: &OrcaTheme,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for node in nodes {
            match &node.kind {
                DiffTreeNodeKind::Directory => {
                    rows.push(
                        Self::directory_tree_row(palette, &node.name, depth).into_any_element(),
                    );
                    rows.extend(self.render_stash_tree_nodes(
                        &node.children,
                        depth + 1,
                        stash_oid,
                        snap,
                        palette,
                    ));
                }
                DiffTreeNodeKind::File(file) => {
                    let selection = StashFileSelection {
                        stash_oid,
                        relative_path: file.relative_path.clone(),
                    };
                    let is_primary = snap.stash_file_selection.as_ref() == Some(&selection);
                    let ws_select = self.workspace.clone();
                    let scope_select = self.scope_root.clone();
                    let selection_for_click = selection.clone();
                    let element_id = ElementId::Name(
                        format!("stash-file-{}-{}", stash_oid, file.relative_path.display()).into(),
                    );
                    rows.push(
                        Self::file_tree_row(
                            palette, element_id, &node.name, depth, file, is_primary, false,
                        )
                        .on_click(
                            move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                                ws_select.update(cx, |ws, cx| {
                                    ws.select_stash_file(
                                        &scope_select,
                                        selection_for_click.clone(),
                                        cx,
                                    );
                                });
                            },
                        )
                        .into_any_element(),
                    );
                }
            }
        }
        rows
    }

    /// A muted, professional toolbar button with subtle blue tint.
    /// Used in both the tree-pane action bar and header action buttons.
    fn action_bar_button(
        palette: &OrcaTheme,
        id: &str,
        label: &str,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        Self::action_bar_button_toned(
            palette,
            id,
            label,
            disabled,
            ActionButtonTone::Default,
            on_click,
        )
    }

    fn action_bar_primary_button(
        palette: &OrcaTheme,
        id: &str,
        label: &str,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        Self::action_bar_button_toned(
            palette,
            id,
            label,
            disabled,
            ActionButtonTone::Primary,
            on_click,
        )
    }

    fn action_bar_destructive_button(
        palette: &OrcaTheme,
        id: &str,
        label: &str,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        Self::action_bar_button_toned(
            palette,
            id,
            label,
            disabled,
            ActionButtonTone::Destructive,
            on_click,
        )
    }

    fn action_bar_button_toned(
        palette: &OrcaTheme,
        id: &str,
        label: &str,
        disabled: bool,
        tone: ActionButtonTone,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let (hover_bg, enabled_bg, enabled_border, enabled_text) = match tone {
            ActionButtonTone::Default => (
                theme::with_alpha(palette.ORCA_BLUE, 0x1A),
                theme::with_alpha(palette.ORCA_BLUE, 0x0C),
                palette.BORDER_EMPHASIS,
                palette.FOG,
            ),
            ActionButtonTone::Primary => (
                theme::with_alpha(palette.ORCA_BLUE, 0x34),
                theme::with_alpha(palette.ORCA_BLUE, 0x18),
                palette.ORCA_BLUE,
                palette.PATCH,
            ),
            ActionButtonTone::Destructive => (
                theme::with_alpha(palette.STATUS_CORAL, 0x1F),
                theme::with_alpha(palette.CURRENT, 0xFF),
                palette.BORDER_EMPHASIS,
                palette.STATUS_CORAL,
            ),
        };
        let hover_text = palette.BONE;
        let mut btn = div()
            .id(ElementId::Name(id.to_string().into()))
            .px(px(8.0))
            .py(px(3.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .border_1()
            .text_size(px(10.0))
            .font_family(DIFF_FONT_FAMILY);

        if disabled {
            btn = btn
                .bg(rgb(palette.CURRENT))
                .border_color(rgb(palette.BORDER_DEFAULT))
                .text_color(rgb(palette.SLATE))
                .cursor(CursorStyle::Arrow)
                .opacity(0.5);
        } else {
            btn = btn
                .bg(rgba(enabled_bg))
                .border_color(rgb(enabled_border))
                .text_color(rgb(enabled_text))
                .cursor_pointer()
                .hover(move |s| s.bg(rgba(hover_bg)).text_color(rgb(hover_text)))
                .on_click(on_click);
        }

        btn.child(label.to_string())
    }

    // ------------------------------------------------------------------
    // Action banner
    // ------------------------------------------------------------------

    fn render_action_banner(&self, palette: &OrcaTheme, snap: &DiffRenderSnapshot) -> Option<Div> {
        let banner = snap.last_action_banner.as_ref()?;
        let dismiss_hover = palette.PATCH;
        let (bg_color, border_color, text_color) = match banner.kind {
            ActionBannerKind::Success => (
                theme::with_alpha(palette.STATUS_GREEN, 0x12),
                theme::with_alpha(palette.STATUS_GREEN, 0x30),
                palette.STATUS_GREEN,
            ),
            ActionBannerKind::Warning => (
                theme::with_alpha(palette.STATUS_AMBER, 0x12),
                theme::with_alpha(palette.STATUS_AMBER, 0x30),
                palette.STATUS_AMBER,
            ),
            ActionBannerKind::Error => (
                theme::with_alpha(palette.STATUS_CORAL, 0x12),
                theme::with_alpha(palette.STATUS_CORAL, 0x30),
                palette.STATUS_CORAL,
            ),
        };

        let ws_dismiss = self.workspace.clone();
        let scope_dismiss = self.scope_root.clone();

        Some(
            div()
                .w_full()
                .px(px(12.0))
                .py(px(6.0))
                .bg(rgba(bg_color))
                .border_b_1()
                .border_color(rgba(border_color))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(text_color))
                        .child(banner.message.clone()),
                )
                .child(
                    div()
                        .id("diff-banner-dismiss")
                        .cursor_pointer()
                        .text_size(px(11.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.FOG))
                        .hover(move |s| s.text_color(rgb(dismiss_hover)))
                        .child("\u{2715}")
                        .on_click(move |_event, _window, cx| {
                            ws_dismiss.update(cx, |ws, cx| {
                                ws.dismiss_action_banner(&scope_dismiss, cx);
                            });
                        }),
                ),
        )
    }

    // ------------------------------------------------------------------
    // Tree pane (left)
    // ------------------------------------------------------------------

    fn render_tree_pane(
        &mut self,
        snap: &DiffRenderSnapshot,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        if snap.view_mode == DiffTabViewMode::Stashes {
            return self.render_stash_tree_pane(snap, cx);
        }
        let palette = theme::active(cx);
        let pane = div()
            .id(ElementId::Name(
                format!("diff-tree-{}", self.scope_root.display()).into(),
            ))
            .w(px(snap.tree_width))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(palette.DEEP))
            .border_r_1()
            .border_color(rgb(palette.SURFACE))
            .overflow_y_scroll();

        if snap.index_loading && !snap.has_index {
            return pane.child(Self::empty_panel_message(
                &palette,
                "Loading diff index...",
                Some("Git is collecting the changed-file tree for this scope."),
            ));
        }
        if let Some(error) = snap.index_error.as_ref() {
            if !snap.has_index {
                return pane.child(Self::empty_panel_message(
                    &palette,
                    "Could not load diff index",
                    Some(error.as_str()),
                ));
            }
        }

        if !snap.has_index {
            return pane.child(Self::empty_panel_message(
                &palette,
                "Diff index unavailable",
                Some("Open the diff tab from a git-backed terminal row."),
            ));
        }

        if snap.index_file_count == 0 {
            return pane.child(Self::empty_panel_message(
                &palette,
                "Working tree clean",
                Some("No changed files are available for this scope."),
            ));
        }

        // Extract visible_order up-front (Rc, so closures share cheaply).
        let visible_order = self
            .tree_cache
            .as_ref()
            .map(|c| Rc::clone(&c.visible_file_order))
            .unwrap_or_else(|| Rc::new(Vec::new()));
        let scope_hash = simple_hash(self.scope_root.to_string_lossy().as_ref());

        let mut list = div().w_full().flex().flex_col();

        if snap.merge_state.is_none() && snap.repo_state_warning.is_none() {
            let scope = self.scope_root.clone();
            list = list.child(self.render_action_bar(&scope, snap, cx));
        }

        if snap.conflicted_file_count > 0 {
            list = list.child(Self::section_header(
                &palette,
                "CONFLICTS",
                snap.conflicted_file_count,
            ));

            let conflicted_tree = self
                .tree_cache
                .as_ref()
                .map(|c| c.conflicted_tree.as_slice())
                .unwrap_or(&[]);
            list = list.children(self.render_section_tree(
                conflicted_tree,
                0,
                DiffSectionKind::Conflicted,
                snap,
                &visible_order,
                scope_hash,
                &palette,
            ));
        }

        if snap.show_commit_input() {
            // Commit input row. ensure_commit_input borrows &mut self, so
            // we do it before borrowing the tree cache.
            let input_entity = self.ensure_commit_input(cx);

            // Sync workspace → input: only push workspace state back to
            // the input when the workspace message was cleared externally
            // (e.g. after a successful commit). Avoids reverting in-flight
            // keystrokes during typing.
            {
                let current_input_value = input_entity.read(cx).value().to_string();
                if snap.commit_message.is_empty() && !current_input_value.is_empty() {
                    input_entity.update(cx, |input, _cx| {
                        input.set_value("");
                    });
                }
            }

            let input_for_keys = input_entity.clone();
            let ws_for_msg = self.workspace.clone();
            let scope_for_msg = self.scope_root.clone();

            list = list.child(
                div()
                    .id("diff-commit-input-container")
                    .w_full()
                    .px(px(8.0))
                    .py(px(6.0))
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .bg(rgb(palette.DEEP))
                    .border_b_1()
                    .border_color(rgb(palette.SURFACE))
                    .child(
                        div()
                            .id("diff-commit-input-wrapper")
                            .flex_1()
                            .h(px(24.0))
                            .bg(rgb(palette.CURRENT))
                            .border_1()
                            .border_color(rgb(palette.SURFACE))
                            .rounded(px(2.0))
                            .px(px(4.0))
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .text_size(px(11.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .child(input_entity.clone())
                            .on_mouse_down(MouseButton::Left, {
                                let input = input_entity.clone();
                                move |_, window, cx| {
                                    input.read(cx).focus(window);
                                    cx.stop_propagation();
                                }
                            })
                            .on_key_down({
                                move |event: &KeyDownEvent, _window, cx| {
                                    cx.stop_propagation();
                                    match event.keystroke.key.as_str() {
                                        "enter" | "escape" => {
                                            // enter/escape: no special handling for now
                                        }
                                        _ => {
                                            input_for_keys.update(cx, |input, cx| {
                                                let changed = input.handle_key_down(event, cx);
                                                if changed {
                                                    let new_val = input.value().to_string();
                                                    let ws = ws_for_msg.clone();
                                                    let scope = scope_for_msg.clone();
                                                    cx.defer(move |cx| {
                                                        ws.update(cx, |ws, cx| {
                                                            ws.update_commit_message(
                                                                &scope, new_val, cx,
                                                            );
                                                        });
                                                    });
                                                }
                                            });
                                        }
                                    }
                                }
                            }),
                    ),
            );
        }

        if snap.show_staged_section() {
            list = list.child(Self::section_header(
                &palette,
                "STAGED CHANGES",
                snap.staged_file_count,
            ));

            let staged_tree = self
                .tree_cache
                .as_ref()
                .map(|c| c.staged_tree.as_slice())
                .unwrap_or(&[]);
            list = list.children(self.render_section_tree(
                staged_tree,
                0,
                DiffSectionKind::Staged,
                snap,
                &visible_order,
                scope_hash,
                &palette,
            ));
        }

        if snap.unstaged_file_count > 0 {
            list = list.child(Self::section_header(
                &palette,
                "CHANGES",
                snap.unstaged_file_count,
            ));

            let unstaged_tree = self
                .tree_cache
                .as_ref()
                .map(|c| c.unstaged_tree.as_slice())
                .unwrap_or(&[]);
            list = list.children(self.render_section_tree(
                unstaged_tree,
                0,
                DiffSectionKind::Unstaged,
                snap,
                &visible_order,
                scope_hash,
                &palette,
            ));
        }

        if let Some(error) = snap.index_error.as_ref() {
            list = list.child(
                div()
                    .m(px(8.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.STATUS_AMBER))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                    .text_size(px(11.0))
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(error.clone()),
            );
        }

        pane.child(list)
    }

    fn render_mode_banner(&self, palette: &OrcaTheme, snap: &DiffRenderSnapshot) -> Option<Div> {
        if let Some(message) = snap.repo_state_warning.as_ref() {
            return Some(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(6.0))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                    .border_b_1()
                    .border_color(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x30)))
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(message.clone()),
            );
        }

        snap.merge_state.as_ref().map(|_| {
            div()
                .w_full()
                .px(px(12.0))
                .py(px(6.0))
                .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                .border_b_1()
                .border_color(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x24)))
                .text_size(px(11.0))
                .font_family(DIFF_FONT_FAMILY)
                .text_color(rgb(palette.STATUS_AMBER))
                .child(
                    "Open each C file, resolve it in the conflict editor, save it, then mark it resolved.",
                )
        })
    }

    fn render_conflict_pane(
        &self,
        _snap: &DiffRenderSnapshot,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = theme::active(cx);
        let pane_id = ElementId::Name(format!("diff-body-{}", self.scope_root.display()).into());
        let bare_pane = || {
            div()
                .id(pane_id.clone())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(rgb(palette.ABYSS))
        };

        let state = {
            let ws = self.workspace.read(cx);
            ws.selected_conflict_document(&self.scope_root).cloned()
        };
        let Some(state) = state else {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Conflict unavailable",
                Some("The selected conflict file is no longer available."),
            ));
        };

        match state {
            ConflictDocumentState::Unavailable(unavailable) => bare_pane().child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(Self::diff_file_header(
                        &palette,
                        &unavailable.file,
                        Some(unavailable.message.as_str()),
                        None,
                    ))
                    .child(Self::empty_panel_message(
                        &palette,
                        "Conflict editor unavailable",
                        Some(unavailable.message.as_str()),
                    )),
            ),
            ConflictDocumentState::Loaded(document) => {
                let Some(cached) = self.conflict_line_cache.as_ref() else {
                    return bare_pane().child(Self::empty_panel_message(
                        &palette,
                        "Loading conflict editor...",
                        Some("The conflicted file is still hydrating."),
                    ));
                };

                let lines = cached.lines.clone();
                let line_count = lines.len();
                let scroll_x = document.scroll_x;
                let selection = document.selection_range.clone();
                let cursor_pos = document.cursor_pos;
                let active_block_index = document.active_block_index;
                let char_width = self.measured_char_width;

                {
                    let state = self.diff_scroll_handle.0.borrow();
                    state
                        .base_handle
                        .set_offset(point(px(0.0), px(-document.scroll_y.max(0.0))));
                }

                let line_palette = palette.clone();
                let diff_list = uniform_list(
                    "conflict-editor-lines",
                    line_count,
                    move |range, _window, _cx| {
                        range
                            .map(|index| {
                                render_conflict_line_element(
                                    &line_palette,
                                    &lines[index],
                                    selection.as_ref(),
                                    cursor_pos,
                                    active_block_index,
                                    scroll_x,
                                    char_width,
                                )
                            })
                            .collect()
                    },
                )
                .size_full()
                .bg(rgb(palette.ABYSS))
                .track_scroll(self.diff_scroll_handle.clone())
                .map(|mut list| {
                    list.style().restrict_scroll_to_axis = Some(true);
                    list
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus_handle);
                        let offset = this.hit_test_conflict_offset(event.position, cx);
                        this.conflict_select_anchor = Some(offset);
                        this.conflict_is_selecting = true;
                        this.set_conflict_cursor(offset, false, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    if !this.conflict_is_selecting {
                        return;
                    }
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.conflict_is_selecting = false;
                        return;
                    }
                    let offset = this.hit_test_conflict_offset(event.position, cx);
                    this.set_conflict_cursor(offset, true, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                        this.conflict_is_selecting = false;
                    }),
                )
                .on_scroll_wheel(cx.listener(
                    |this, event: &ScrollWheelEvent, _window, cx| {
                        if this.handle_conflict_scroll_wheel(event, cx) {
                            cx.stop_propagation();
                        }
                    },
                ));

                let list_bounds = self.diff_list_bounds.clone();
                let scrollbar = self
                    .diff_scrollbar_geometry()
                    .map(|(thumb_y, thumb_height)| {
                        let is_dragging = self.diff_scrollbar_drag.is_some();
                        self.render_scrollbar(thumb_y, thumb_height, is_dragging, &palette, cx)
                    });

                bare_pane().child(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                            this.copy_conflict_selection(cx);
                        }))
                        .on_action(cx.listener(
                            |this, _: &orcashell_terminal_view::Paste, _window, cx| {
                                this.paste_conflict_selection(cx);
                            },
                        ))
                        .child(self.render_conflict_status_row(&palette, &document, cx))
                        .child(self.render_conflict_helper_strip(&palette, &document, cx))
                        .child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    canvas(
                                        move |bounds, _window, _cx| {
                                            *list_bounds.borrow_mut() = bounds;
                                        },
                                        |_bounds, _prepaint, _window, _cx| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                                .child(diff_list)
                                .children(scrollbar),
                        ),
                )
            }
        }
    }

    fn render_stash_diff_pane(
        &self,
        snap: &DiffRenderSnapshot,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = theme::active(cx);
        let pane_id = ElementId::Name(format!("stash-body-{}", self.scope_root.display()).into());
        let bare_pane = || {
            div()
                .id(pane_id.clone())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(rgb(palette.ABYSS))
        };

        if snap.stash_list_loading && snap.stash_entries.is_empty() {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Loading stashes...",
                Some("The stash list is still loading."),
            ));
        }

        if snap.stash_entries.is_empty() {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "No stashes",
                Some("Create a stash from the diff header to preview it here."),
            ));
        }

        let Some(selected_stash) = snap.selected_stash else {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Select a stash",
                Some("Pick a stash from the left pane."),
            ));
        };

        if snap.stash_file_selection.is_some() {
            if snap.stash_file_loading && snap.stash_file_meta.is_none() {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Loading stash file...",
                    Some("The selected stash file patch is still hydrating."),
                ));
            }
            if let Some(error) = snap.stash_file_error.as_ref() {
                if snap.stash_file_meta.is_none() {
                    return bare_pane().child(Self::empty_panel_message(
                        &palette,
                        "Could not load stash file",
                        Some(error.as_str()),
                    ));
                }
            }

            let Some(file_meta) = snap.stash_file_meta.as_ref() else {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Select a file",
                    Some("Pick a file from the selected stash."),
                ));
            };

            let Some(cached) = self.stash_line_cache.as_ref() else {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Select a file",
                    Some("Pick a file from the selected stash."),
                ));
            };
            let lines = cached.lines.clone();
            let line_count = lines.len();
            let scroll_x = self.diff_scroll_x;
            let max_line_chars = cached.max_line_chars;
            let is_oversize = cached.is_oversize;
            let file_header =
                Self::diff_file_header(&palette, file_meta, snap.stash_file_error.as_deref(), None);

            if is_oversize {
                return bare_pane().child(
                    div()
                        .p(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(file_header)
                        .child(Self::empty_panel_message(
                            &palette,
                            OVERSIZE_DIFF_MESSAGE,
                            Some("Open the file in another tool if you need the full patch."),
                        )),
                );
            }

            let line_palette = palette.clone();
            let diff_list = uniform_list(
                "stash-diff-lines",
                line_count,
                move |range, _window, _cx| {
                    range
                        .map(|ix| {
                            render_diff_line_element(
                                &line_palette,
                                &lines[ix],
                                ix,
                                None,
                                scroll_x,
                                None,
                            )
                        })
                        .collect()
                },
            )
            .size_full()
            .bg(rgb(palette.ABYSS))
            .track_scroll(self.diff_scroll_handle.clone())
            .map(|mut list| {
                list.style().restrict_scroll_to_axis = Some(true);
                list
            })
            .on_scroll_wheel(cx.listener(
                move |this, event: &ScrollWheelEvent, _window, cx| {
                    let delta_x: f32 = match event.delta {
                        ScrollDelta::Pixels(d) => f32::from(d.x),
                        ScrollDelta::Lines(d) => d.x * this.measured_char_width * 3.0,
                    };
                    if delta_x.abs() > 0.5 {
                        let viewport_w = f32::from(this.diff_list_bounds.borrow().size.width);
                        let max_scroll =
                            (max_diff_content_width(max_line_chars, this.measured_char_width)
                                - viewport_w)
                                .max(0.0);
                        this.diff_scroll_x = (this.diff_scroll_x - delta_x).clamp(0.0, max_scroll);
                        cx.notify();
                    }
                },
            ));

            let list_bounds = self.diff_list_bounds.clone();
            let scrollbar = self
                .diff_scrollbar_geometry()
                .map(|(thumb_y, thumb_height)| {
                    let is_dragging = self.diff_scrollbar_drag.is_some();
                    self.render_scrollbar(thumb_y, thumb_height, is_dragging, &palette, cx)
                });

            return bare_pane().child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(file_header)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(
                                canvas(
                                    move |bounds, _window, _cx| {
                                        *list_bounds.borrow_mut() = bounds;
                                    },
                                    |_bounds, _prepaint, _window, _cx| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(diff_list)
                            .children(scrollbar),
                    ),
            );
        }

        if snap.stash_detail_loading && snap.stash_detail.is_none() {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Loading stash...",
                Some("The selected stash summary is still hydrating."),
            ));
        }

        if let Some(error) = snap.stash_detail_error.as_ref() {
            if snap.stash_detail.is_none() {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Could not load stash",
                    Some(error.as_str()),
                ));
            }
        }

        let entry = snap
            .stash_entries
            .iter()
            .find(|entry| entry.stash_oid == selected_stash);
        let detail = snap
            .stash_detail
            .as_ref()
            .filter(|detail| detail.stash_oid == selected_stash);
        let title = entry
            .map(|entry| entry.label.clone())
            .unwrap_or_else(|| "stash".to_string());
        let message = detail
            .map(|detail| detail.message.clone())
            .or_else(|| entry.map(|entry| entry.message.clone()))
            .unwrap_or_else(|| "No stash detail available.".to_string());
        let body = detail.map(|detail| {
            format!(
                "{} file{}{}",
                detail.files.len(),
                if detail.files.len() == 1 { "" } else { "s" },
                if detail.includes_untracked {
                    ", includes untracked files"
                } else {
                    ""
                }
            )
        });

        bare_pane().child(
            div()
                .size_full()
                .p(px(20.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(palette.BONE))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.FOG))
                        .child(message),
                )
                .children(body.map(|body| {
                    div()
                        .text_size(px(11.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.SEAFOAM))
                        .child(body)
                        .into_any_element()
                }))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.FOG))
                        .child("Select a file on the left to preview its patch.".to_string()),
                ),
        )
    }

    fn render_conflict_status_row(
        &self,
        palette: &OrcaTheme,
        document: &ConflictEditorDocument,
        cx: &mut Context<Self>,
    ) -> Div {
        let block_total = document.blocks.len();
        let active_position = document
            .active_block_index
            .map(|index| index + 1)
            .unwrap_or(0);
        let prev_disabled =
            conflict_block_navigation_disabled(block_total, document.active_block_index, false);
        let next_disabled =
            conflict_block_navigation_disabled(block_total, document.active_block_index, true);

        let entity_prev = cx.entity().clone();
        let entity_next = cx.entity().clone();

        div()
            .w_full()
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .bg(rgb(palette.DEEP))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                if this.handle_conflict_scroll_wheel(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.PATCH))
                    .child(document.file.relative_path.display().to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(Self::action_bar_button(
                        palette,
                        "conflict-prev",
                        "Prev",
                        prev_disabled,
                        move |_event, _window, cx| {
                            entity_prev.update(cx, |this, cx| {
                                this.move_conflict_active_block(false, cx);
                            });
                        },
                    ))
                    .child(Self::action_bar_button(
                        palette,
                        "conflict-next",
                        "Next",
                        next_disabled,
                        move |_event, _window, cx| {
                            entity_next.update(cx, |this, cx| {
                                this.move_conflict_active_block(true, cx);
                            });
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.FOG))
                            .child(format!("{active_position} of {block_total} conflicts")),
                    ),
            )
    }

    fn render_conflict_helper_strip(
        &self,
        palette: &OrcaTheme,
        document: &ConflictEditorDocument,
        cx: &mut Context<Self>,
    ) -> Div {
        let workspace = self.workspace.read(cx);
        let any_in_flight = workspace
            .diff_tab_state(&self.scope_root)
            .is_some_and(|tab| tab.local_action_in_flight || tab.remote_op_in_flight);
        let can_mark_resolved = workspace.can_mark_conflicts_resolved(&self.scope_root);
        let active_block = document
            .active_block_index
            .and_then(|index| document.blocks.get(index));
        let helper_disabled = active_block.is_none() || any_in_flight;
        let save_disabled = any_in_flight || !document.is_dirty;
        let reset_disabled = any_in_flight || document.raw_text == document.initial_raw_text;
        let resolve_disabled = !can_mark_resolved;

        let entity_ours = cx.entity().clone();
        let entity_theirs = cx.entity().clone();
        let entity_both = cx.entity().clone();
        let entity_base = cx.entity().clone();
        let entity_save = cx.entity().clone();
        let entity_reset = cx.entity().clone();
        let entity_resolve = cx.entity().clone();

        let mut row = div()
            .w_full()
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .bg(rgb(palette.DEEP))
            .flex()
            .items_center()
            .gap(px(6.0))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                if this.handle_conflict_scroll_wheel(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(Self::action_bar_button(
                palette,
                "conflict-accept-ours",
                "Accept Ours",
                helper_disabled,
                move |_event, _window, cx| {
                    entity_ours.update(cx, |this, cx| {
                        this.apply_active_conflict_choice(ConflictAcceptChoice::Ours, cx);
                    });
                },
            ))
            .child(Self::action_bar_button(
                palette,
                "conflict-accept-theirs",
                "Accept Theirs",
                helper_disabled,
                move |_event, _window, cx| {
                    entity_theirs.update(cx, |this, cx| {
                        this.apply_active_conflict_choice(ConflictAcceptChoice::Theirs, cx);
                    });
                },
            ))
            .child(Self::action_bar_button(
                palette,
                "conflict-accept-both",
                "Accept Both",
                helper_disabled,
                move |_event, _window, cx| {
                    entity_both.update(cx, |this, cx| {
                        this.apply_active_conflict_choice(ConflictAcceptChoice::Both, cx);
                    });
                },
            ));

        if active_block.and_then(|block| block.base.as_ref()).is_some() {
            row = row.child(Self::action_bar_button(
                palette,
                "conflict-accept-base",
                "Accept Base",
                helper_disabled,
                move |_event, _window, cx| {
                    entity_base.update(cx, |this, cx| {
                        this.apply_active_conflict_choice(ConflictAcceptChoice::Base, cx);
                    });
                },
            ));
        }

        row.child(Self::action_bar_button(
            palette,
            "conflict-save",
            "Save",
            save_disabled,
            move |_event, _window, cx| {
                entity_save.update(cx, |this, cx| {
                    this.save_conflict_document(cx);
                });
            },
        ))
        .child(Self::action_bar_button(
            palette,
            "conflict-reset",
            "Reset",
            reset_disabled,
            move |_event, _window, cx| {
                entity_reset.update(cx, |this, cx| {
                    this.reset_conflict_document(cx);
                });
            },
        ))
        .child(Self::action_bar_button(
            palette,
            "conflict-mark-resolved",
            "Mark Resolved",
            resolve_disabled,
            move |_event, _window, cx| {
                entity_resolve.update(cx, |this, cx| {
                    this.mark_conflicts_resolved(cx);
                });
            },
        ))
    }

    /// Render a section header row (e.g. "STAGED CHANGES (3)").
    pub(crate) fn section_header(palette: &OrcaTheme, label: &str, count: usize) -> Div {
        div()
            .w_full()
            .px(px(8.0))
            .py(px(4.0))
            .bg(rgb(palette.CURRENT))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.FOG))
                    .child(format!("{label} ({count})")),
            )
    }

    pub(crate) fn directory_tree_row(palette: &OrcaTheme, name: &str, depth: usize) -> Div {
        div()
            .w_full()
            .px(px(8.0))
            .py(px(4.0))
            .pl(px(8.0 + depth as f32 * 14.0))
            .text_size(px(11.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(palette.FOG))
            .child(format!("\u{25BE} {name}"))
    }

    pub(crate) fn file_tree_row(
        palette: &OrcaTheme,
        element_id: ElementId,
        name: &str,
        depth: usize,
        file: &ChangedFile,
        is_primary: bool,
        is_multi: bool,
    ) -> Stateful<Div> {
        let hover_bg = rgba(theme::with_alpha(palette.CURRENT, 0x88));
        let mut row = div()
            .id(element_id)
            .w_full()
            .px(px(8.0))
            .py(px(4.0))
            .pl(px(8.0 + depth as f32 * 14.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .border_l_2()
            .when(is_primary, |row| {
                row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                    .border_color(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x66)))
            })
            .when(!is_primary && is_multi, |row| {
                row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x12)))
                    .border_color(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x60)))
            })
            .when(!is_primary && !is_multi, |row| {
                row.border_color(rgba(0x00000000))
                    .hover(move |row| row.bg(hover_bg))
            })
            .child(Self::status_pill(palette, file.status))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(if is_primary {
                        palette.PATCH
                    } else {
                        palette.BONE
                    }))
                    .child(name.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .child(
                        div()
                            .text_color(rgb(palette.STATUS_GREEN))
                            .child(format!("+{}", file.insertions)),
                    )
                    .child(
                        div()
                            .text_color(rgb(palette.STATUS_CORAL))
                            .child(format!("-{}", file.deletions)),
                    ),
            );

        if file.is_binary {
            row = row.child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.SEAFOAM))
                    .child("bin"),
            );
        }

        row
    }

    pub(crate) fn diff_file_header(
        palette: &OrcaTheme,
        file_meta: &ChangedFile,
        error: Option<&str>,
        action: Option<AnyElement>,
    ) -> Div {
        let mut file_header = div()
            .w_full()
            .flex_shrink_0()
            .px(px(12.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .bg(rgb(palette.DEEP))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.PATCH))
                    .child(file_meta.relative_path.display().to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .children(action)
                    .child(Self::status_summary(palette, file_meta)),
            );

        if let Some(error) = error {
            file_header = file_header.child(
                div()
                    .w_full()
                    .px(px(12.0))
                    .py(px(6.0))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x10)))
                    .border_b_1()
                    .border_color(rgb(palette.SURFACE))
                    .text_size(px(11.0))
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(error.to_string()),
            );
        }

        file_header
    }

    fn inline_destructive_button(
        palette: &OrcaTheme,
        id: impl Into<String>,
        label: &str,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let mut button = div()
            .id(ElementId::Name(id.into().into()))
            .px(px(7.0))
            .py(px(2.0))
            .rounded(px(3.0))
            .border_1()
            .text_size(px(10.0))
            .font_family(DIFF_FONT_FAMILY);

        if disabled {
            button = button
                .bg(rgb(palette.CURRENT))
                .border_color(rgb(palette.BORDER_DEFAULT))
                .text_color(rgb(palette.SLATE))
                .cursor(CursorStyle::Arrow)
                .opacity(0.5);
        } else {
            let hover_bg = theme::with_alpha(palette.STATUS_CORAL, 0x18);
            let border = palette.BORDER_EMPHASIS;
            let hover_text = palette.BONE;
            button = button
                .bg(rgba(theme::with_alpha(palette.CURRENT, 0xFF)))
                .border_color(rgb(border))
                .text_color(rgb(palette.STATUS_CORAL))
                .cursor_pointer()
                .hover(move |style| style.bg(rgba(hover_bg)).text_color(rgb(hover_text)))
                .on_click(on_click);
        }

        button.child(label.to_string())
    }

    fn inline_hunk_discard_button(
        palette: &OrcaTheme,
        scope_root: &Path,
        workspace: Entity<WorkspaceState>,
        line_index: usize,
        hunk: FileDiffHunk,
    ) -> AnyElement {
        let scope_root = scope_root.to_path_buf();
        div()
            .id(ElementId::Name(
                format!("diff-discard-hunk-{}", hunk.hunk_index).into(),
            ))
            .px(px(2.0))
            .py(px(1.0))
            .text_size(px(10.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(palette.STATUS_CORAL))
            .opacity(0.70)
            .cursor_pointer()
            .hover(|style| style.opacity(1.0))
            .child("Discard Hunk")
            .on_click(
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    cx.stop_propagation();
                    workspace.update(cx, |ws, cx| {
                        ws.discard_selected_hunk(&scope_root, hunk.hunk_index, cx);
                    });
                },
            )
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                cx.stop_propagation();
            })
            .debug_selector(move || format!("diff-discard-hunk-action-{line_index}"))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_section_tree(
        &self,
        nodes: &[DiffTreeNode],
        depth: usize,
        section: DiffSectionKind,
        snap: &DiffRenderSnapshot,
        visible_order: &Rc<Vec<DiffSelectionKey>>,
        scope_hash: u64,
        palette: &OrcaTheme,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for node in nodes {
            match &node.kind {
                DiffTreeNodeKind::Directory => {
                    rows.push(
                        Self::directory_tree_row(palette, &node.name, depth).into_any_element(),
                    );
                    rows.extend(self.render_section_tree(
                        &node.children,
                        depth + 1,
                        section,
                        snap,
                        visible_order,
                        scope_hash,
                        palette,
                    ));
                }
                DiffTreeNodeKind::File(file) => {
                    let key = DiffSelectionKey {
                        section,
                        relative_path: file.relative_path.clone(),
                    };
                    let is_primary = snap.selected_file.as_ref() == Some(&key);
                    let is_multi = snap.multi_select.contains(&key);
                    let scope_root = self.scope_root.clone();
                    let ws_handle = self.workspace.clone();
                    let key_click = key.clone();
                    let key_select = key.clone();
                    let scope_for_click = scope_root.clone();
                    let scope_for_select = scope_root.clone();
                    let ws_for_click = ws_handle.clone();
                    let ws_for_select = ws_handle.clone();
                    let visible_for_shift = Rc::clone(visible_order);

                    let path_hash = simple_hash(file.relative_path.to_string_lossy().as_ref());
                    let element_id = ElementId::Name(
                        format!("diff-{:?}-{}-{}", section, scope_hash, path_hash).into(),
                    );
                    // Context menu items for right-click.
                    let menu_request = self.menu_request.clone();
                    let key_ctx = key.clone();
                    let ws_for_ctx = ws_handle.clone();
                    let scope_for_ctx = scope_root.clone();

                    let row = Self::file_tree_row(
                        palette, element_id, &node.name, depth, file, is_primary, is_multi,
                    )
                    .on_click(move |event: &ClickEvent, _window, cx| {
                        let mods = event.modifiers();
                        if mods.platform {
                            // Cmd+click: toggle multi-select.
                            ws_for_click.update(cx, |ws, cx| {
                                ws.diff_toggle_multi_select(
                                    &scope_for_click,
                                    key_click.clone(),
                                    cx,
                                );
                            });
                        } else if mods.shift {
                            // Shift+click: range select.
                            ws_for_click.update(cx, |ws, cx| {
                                ws.diff_range_select(
                                    &scope_for_click,
                                    key_click.clone(),
                                    &visible_for_shift,
                                    cx,
                                );
                            });
                        } else {
                            // Plain click: replace select + load diff.
                            ws_for_select.update(cx, |ws, cx| {
                                ws.diff_replace_select(&scope_for_select, key_select.clone(), cx);
                                ws.select_diff_file(&scope_for_select, key_select.clone(), cx);
                            });
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        move |event: &MouseDownEvent, _window, cx| {
                            // Collapse selection to this row if not already selected.
                            let is_in_selection = ws_for_ctx
                                .read(cx)
                                .diff_tab_state(&scope_for_ctx)
                                .is_some_and(|tab| tab.multi_select.contains(&key_ctx));
                            if !is_in_selection {
                                ws_for_ctx.update(cx, |ws, cx| {
                                    ws.diff_replace_select(&scope_for_ctx, key_ctx.clone(), cx);
                                });
                            }
                            let ctx_section = key_ctx.section;
                            let scope = scope_for_ctx.clone();
                            let in_flight = ws_for_ctx
                                .read(cx)
                                .diff_tab_state(&scope_for_ctx)
                                .is_some_and(|tab| {
                                    tab.local_action_in_flight || tab.remote_op_in_flight
                                });
                            let unsupported_state = ws_for_ctx
                                .read(cx)
                                .diff_tab_state(&scope_for_ctx)
                                .and_then(|tab| tab.index.document.as_ref())
                                .is_some_and(|document| {
                                    document.merge_state.is_none()
                                        && document.repo_state_warning.is_some()
                                });
                            let items = match ctx_section {
                                DiffSectionKind::Conflicted => {
                                    let scope = scope.clone();
                                    let can_mark_resolved =
                                        ws_for_ctx.read(cx).can_mark_conflicts_resolved(&scope);
                                    vec![ContextMenuItem {
                                        label: "Mark Resolved".to_string(),
                                        shortcut: None,
                                        enabled: can_mark_resolved,
                                        action: Box::new(move |ws_state, cx| {
                                            ws_state.mark_conflicts_resolved(&scope, cx);
                                        }),
                                    }]
                                }
                                DiffSectionKind::Staged => {
                                    let scope = scope.clone();
                                    vec![ContextMenuItem {
                                        label: "Unstage".to_string(),
                                        shortcut: None,
                                        enabled: !in_flight && !unsupported_state,
                                        action: Box::new(move |ws_state, cx| {
                                            ws_state.unstage_selected(&scope, cx);
                                        }),
                                    }]
                                }
                                DiffSectionKind::Unstaged => {
                                    let scope = scope.clone();
                                    vec![ContextMenuItem {
                                        label: "Stage".to_string(),
                                        shortcut: None,
                                        enabled: !in_flight && !unsupported_state,
                                        action: Box::new(move |ws_state, cx| {
                                            ws_state.stage_selected(&scope, cx);
                                        }),
                                    }]
                                }
                            };
                            if items.is_empty() {
                                return;
                            }
                            *menu_request.borrow_mut() = Some((event.position, items));
                            cx.stop_propagation();
                        },
                    );

                    rows.push(row.into_any_element());
                }
            }
        }
        rows
    }

    // ------------------------------------------------------------------
    // Diff pane (right) - virtualized via uniform_list
    // ------------------------------------------------------------------

    fn render_diff_pane(&self, snap: &DiffRenderSnapshot, cx: &mut Context<Self>) -> Stateful<Div> {
        if snap.view_mode == DiffTabViewMode::Stashes {
            return self.render_stash_diff_pane(snap, cx);
        }
        let palette = theme::active(cx);
        let pane_id = ElementId::Name(format!("diff-body-{}", self.scope_root.display()).into());

        // A bare pane used for empty/loading/error states (no uniform_list).
        let bare_pane = || {
            div()
                .id(pane_id.clone())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(rgb(palette.ABYSS))
        };

        // ---- Early-return states ----

        if snap.index_loading && !snap.has_index {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Loading diff...",
                Some("The changed-file tree is still loading."),
            ));
        }
        if let Some(error) = snap.index_error.as_ref() {
            if !snap.has_index {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Could not load diff",
                    Some(error.as_str()),
                ));
            }
        }

        if !snap.has_index {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "No diff loaded",
                Some("Select a git-backed terminal row and open its diff explorer."),
            ));
        }

        if snap.index_file_count == 0 {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Working tree clean",
                Some("There are no file changes to render."),
            ));
        }

        let selected_path = snap.selected_file.as_ref().map(|k| &k.relative_path);

        let Some(selected_path) = selected_path else {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        if snap
            .selected_file
            .as_ref()
            .is_some_and(|selection| selection.section == DiffSectionKind::Conflicted)
        {
            return self.render_conflict_pane(snap, cx);
        }

        if snap.file_loading
            && snap
                .file_meta
                .as_ref()
                .is_none_or(|meta| meta.relative_path != *selected_path)
        {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Loading file diff...",
                Some("The selected file patch is still hydrating."),
            ));
        }

        if let Some(error) = snap.file_error.as_ref() {
            if snap.file_meta.is_none() {
                return bare_pane().child(Self::empty_panel_message(
                    &palette,
                    "Could not load file diff",
                    Some(error.as_str()),
                ));
            }
        }

        let Some(file_meta) = snap.file_meta.as_ref() else {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        let latest_generation = self
            .workspace
            .read(cx)
            .git_scope_snapshot(&self.scope_root)
            .map(|snapshot| snapshot.generation);
        let file_is_stale = is_displayed_file_stale(snap.file_generation, latest_generation);
        let discard_file_disabled = snap.any_action_in_flight()
            || snap.merge_state.is_some()
            || snap.repo_state_warning.is_some()
            || file_is_stale
            || snap.file_meta.as_ref().is_some_and(|file| {
                matches!(
                    file.status,
                    GitFileStatus::Renamed | GitFileStatus::Typechange
                )
            });
        let discard_file_action = if snap
            .selected_file
            .as_ref()
            .is_some_and(|selection| selection.section == DiffSectionKind::Unstaged)
            && snap.file_generation.is_some()
            && snap.file_selection == snap.selected_file
        {
            let ws_discard_file = self.workspace.clone();
            let scope_discard_file = self.scope_root.clone();
            Some(
                Self::inline_destructive_button(
                    &palette,
                    "diff-action-discard-file",
                    "Discard File",
                    discard_file_disabled,
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        cx.stop_propagation();
                        ws_discard_file.update(cx, |ws, cx| {
                            ws.discard_selected_file(&scope_discard_file, cx);
                        });
                    },
                )
                .into_any_element(),
            )
        } else {
            None
        };
        let file_header = Self::diff_file_header(
            &palette,
            file_meta,
            snap.file_error.as_deref(),
            discard_file_action,
        );

        if snap.is_oversize {
            return bare_pane().child(
                div()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(file_header)
                    .child(Self::empty_panel_message(
                        &palette,
                        OVERSIZE_DIFF_MESSAGE,
                        Some("Open the file in another tool if you need the full patch."),
                    )),
            );
        }

        // ---- Normal content: file header + virtualized line list ----

        // Use cached lines (Rc clone = refcount bump on cache hit).
        let Some(cached) = self.line_cache.as_ref() else {
            return bare_pane().child(Self::empty_panel_message(
                &palette,
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };
        let lines = cached.lines.clone();
        let line_count = lines.len();
        let selection = self.selection;
        let scroll_x = self.diff_scroll_x;
        let line_palette = palette.clone();
        let hunk_headers = cached.hunk_headers.clone();
        let hunk_actions_enabled = snap
            .selected_file
            .as_ref()
            .is_some_and(|selection| selection.section == DiffSectionKind::Unstaged)
            && !discard_file_disabled
            && !snap.is_oversize
            && snap.file_error.is_none()
            && snap.file_meta.as_ref().is_some_and(|file| {
                !matches!(
                    file.status,
                    GitFileStatus::Renamed | GitFileStatus::Typechange
                )
            });
        let ws_discard_hunk = self.workspace.clone();
        let scope_discard_hunk = self.scope_root.clone();

        // The virtualized line list.
        let diff_list = uniform_list("diff-lines", line_count, move |range, _window, _cx| {
            range
                .map(|ix| {
                    let hunk_action = if hunk_actions_enabled {
                        hunk_headers.get(&ix).cloned().map(|hunk| {
                            Self::inline_hunk_discard_button(
                                &line_palette,
                                &scope_discard_hunk,
                                ws_discard_hunk.clone(),
                                ix,
                                hunk,
                            )
                        })
                    } else {
                        None
                    };
                    render_diff_line_element(
                        &line_palette,
                        &lines[ix],
                        ix,
                        selection.as_ref(),
                        scroll_x,
                        hunk_action,
                    )
                })
                .collect()
        })
        .size_full()
        .bg(rgb(palette.ABYSS))
        .track_scroll(self.diff_scroll_handle.clone())
        .map(|mut list| {
            list.style().restrict_scroll_to_axis = Some(true);
            list
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                let (line, col) = this.hit_test_diff(event.position);
                if event.click_count >= 3 {
                    // Triple-click: select entire line.
                    this.selection = this.line_length(line, cx).map(|len| DiffSelection {
                        start: (line, 0),
                        end: (line, len),
                        is_selecting: false,
                    });
                } else if event.click_count == 2 {
                    // Double-click: select word.
                    this.selection =
                        this.word_bounds_at(line, col, cx)
                            .map(|(s, e)| DiffSelection {
                                start: (line, s),
                                end: (line, e),
                                is_selecting: false,
                            });
                } else {
                    this.selection = Some(DiffSelection {
                        start: (line, col),
                        end: (line, col),
                        is_selecting: true,
                    });
                }
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
            // Handle horizontal scroll (shift+wheel or trackpad horizontal).
            let delta_x: f32 = match event.delta {
                ScrollDelta::Pixels(d) => f32::from(d.x),
                ScrollDelta::Lines(d) => d.x * this.measured_char_width * 3.0,
            };
            if delta_x.abs() > 0.5 {
                // Clamp to [0, max_scrollable] so you can't scroll past the longest line.
                let viewport_w = f32::from(this.diff_list_bounds.borrow().size.width);
                let max_line_w = this
                    .line_cache
                    .as_ref()
                    .map(|c| max_diff_content_width(c.max_line_chars, this.measured_char_width))
                    .unwrap_or(0.0);
                let max_scroll = (max_line_w - viewport_w).max(0.0);
                this.diff_scroll_x = (this.diff_scroll_x - delta_x).clamp(0.0, max_scroll);
                cx.notify();
            }
        }));

        // Bounds-tracking canvas for the list area.
        let list_bounds = self.diff_list_bounds.clone();

        // Scrollbar overlay (conditionally rendered).
        let scrollbar = self
            .diff_scrollbar_geometry()
            .map(|(thumb_y, thumb_height)| {
                let is_dragging = self.diff_scrollbar_drag.is_some();
                self.render_scrollbar(thumb_y, thumb_height, is_dragging, &palette, cx)
            });

        // Assemble: file header + (list area with scrollbar overlay).
        div()
            .id(pane_id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                this.copy_selection(cx);
            }))
            .child(file_header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        canvas(
                            move |bounds, _window, _cx| {
                                *list_bounds.borrow_mut() = bounds;
                            },
                            |_bounds, _prepaint, _window, _cx| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(diff_list)
                    .children(scrollbar),
            )
    }

    // ------------------------------------------------------------------
    // Scrollbar
    // ------------------------------------------------------------------

    /// Compute (thumb_y, thumb_height) for the diff-pane scrollbar, or `None`
    /// if the content fits within the viewport.
    fn diff_scrollbar_geometry(&self) -> Option<(f32, f32)> {
        let state = self.diff_scroll_handle.0.borrow();
        let item_size = state.last_item_size?;

        let viewport_h = f32::from(item_size.item.height);
        let content_h = f32::from(item_size.contents.height);
        if content_h <= viewport_h {
            return None;
        }

        let scroll_y = -f32::from(state.base_handle.offset().y);
        let thumb_h = (viewport_h / content_h * viewport_h).max(SCROLLBAR_THUMB_MIN);
        let scrollable_content = content_h - viewport_h;
        let scrollable_track = viewport_h - thumb_h;
        let ratio = (scroll_y / scrollable_content).clamp(0.0, 1.0);
        Some((ratio * scrollable_track, thumb_h))
    }

    fn render_scrollbar(
        &self,
        thumb_y: f32,
        thumb_height: f32,
        is_dragging: bool,
        palette: &OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hover_color = theme::with_alpha(palette.ORCA_BLUE, 0xA0);
        let thumb_color = if is_dragging {
            theme::with_alpha(palette.ORCA_BLUE, 0x60)
        } else {
            theme::with_alpha(palette.ORCA_BLUE, 0x4D)
        };

        div()
            .id("diff-scrollbar")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(SCROLLBAR_HIT_WIDTH))
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let y = f32::from(event.position.y);
                    let bounds = *this.diff_list_bounds.borrow();
                    let local_y = y - f32::from(bounds.origin.y);

                    // If clicking on the thumb, start a drag.
                    if local_y >= thumb_y && local_y <= thumb_y + thumb_height {
                        let state = this.diff_scroll_handle.0.borrow();
                        let scroll_y = -f32::from(state.base_handle.offset().y);
                        drop(state);
                        this.diff_scrollbar_drag = Some(ScrollbarDrag {
                            start_y: y,
                            start_scroll_y: scroll_y,
                        });
                    } else {
                        // Click on track: jump to that position.
                        this.scrollbar_jump_to(local_y);
                    }
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let Some(drag) = this.diff_scrollbar_drag else {
                    return;
                };
                let Some((_, _)) = this.diff_scrollbar_geometry() else {
                    return;
                };
                let state = this.diff_scroll_handle.0.borrow();
                let item_size = state.last_item_size;
                drop(state);

                let Some(item_size) = item_size else { return };
                let viewport_h = f32::from(item_size.item.height);
                let content_h = f32::from(item_size.contents.height);
                let scrollable_content = content_h - viewport_h;
                let thumb_h = (viewport_h / content_h * viewport_h).max(SCROLLBAR_THUMB_MIN);
                let scrollable_track = viewport_h - thumb_h;
                if scrollable_track <= 0.0 {
                    return;
                }

                let delta_y = f32::from(event.position.y) - drag.start_y;
                let delta_scroll = delta_y * scrollable_content / scrollable_track;
                let new_scroll =
                    (drag.start_scroll_y + delta_scroll).clamp(0.0, scrollable_content);

                let state = this.diff_scroll_handle.0.borrow();
                state
                    .base_handle
                    .set_offset(point(px(0.0), px(-new_scroll)));
                drop(state);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.diff_scrollbar_drag = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(thumb_y))
                    .right(px(SCROLLBAR_THUMB_INSET))
                    .w(px(SCROLLBAR_THUMB_WIDTH))
                    .h(px(thumb_height))
                    .rounded(px(3.0))
                    .bg(rgba(thumb_color))
                    .hover(move |s| s.bg(rgba(hover_color))),
            )
    }

    /// Jump the scroll position so that the given track-local Y is centred
    /// under the scrollbar thumb.
    fn scrollbar_jump_to(&self, local_y: f32) {
        let state = self.diff_scroll_handle.0.borrow();
        let Some(item_size) = state.last_item_size else {
            return;
        };
        let viewport_h = f32::from(item_size.item.height);
        let content_h = f32::from(item_size.contents.height);
        let scrollable_content = content_h - viewport_h;
        let thumb_h = (viewport_h / content_h * viewport_h).max(SCROLLBAR_THUMB_MIN);
        let scrollable_track = viewport_h - thumb_h;
        if scrollable_track <= 0.0 {
            return;
        }
        let ratio = ((local_y - thumb_h * 0.5) / scrollable_track).clamp(0.0, 1.0);
        let new_scroll = ratio * scrollable_content;
        state
            .base_handle
            .set_offset(point(px(0.0), px(-new_scroll)));
    }

    // ------------------------------------------------------------------
    // Selection helpers
    // ------------------------------------------------------------------

    /// Convert a window-coordinate position to a (line_index, char_index) in
    /// the diff list.
    fn hit_test_diff(&self, position: Point<Pixels>) -> (usize, usize) {
        let bounds = *self.diff_list_bounds.borrow();
        let local_y = f32::from(position.y) - f32::from(bounds.origin.y);
        let local_x = f32::from(position.x) - f32::from(bounds.origin.x);

        let scroll_y = {
            let state = self.diff_scroll_handle.0.borrow();
            -f32::from(state.base_handle.offset().y)
        };

        let line = ((local_y + scroll_y) / LINE_HEIGHT).floor().max(0.0) as usize;
        let col = ((local_x - TEXT_COL_X + self.diff_scroll_x) / self.measured_char_width)
            .floor()
            .max(0.0) as usize;
        (line, col)
    }

    fn hit_test_conflict_offset(&self, position: Point<Pixels>, _cx: &App) -> usize {
        let (line_index, col) = self.hit_test_diff(position);
        let Some(cached) = self.conflict_line_cache.as_ref() else {
            return 0;
        };
        let line_index = line_index.min(cached.lines.len().saturating_sub(1));
        byte_offset_for_line_column(&cached.lines, line_index, col)
    }

    /// Get the plain-text length for a diff line, reading from workspace state.
    fn line_length(&self, line_index: usize, cx: &App) -> Option<usize> {
        self.with_filtered_line(line_index, cx, |l| {
            plain_text_len(&l.text, l.highlights.as_deref())
        })
    }

    /// Find word boundaries around the given column in a diff line.
    fn word_bounds_at(&self, line_index: usize, col: usize, cx: &App) -> Option<(usize, usize)> {
        self.with_filtered_line(line_index, cx, |line| {
            let text = plain_text_for_line(&line.text, line.highlights.as_deref());
            let chars: Vec<char> = text.chars().collect();
            if chars.is_empty() {
                return (0, 0);
            }
            let col = col.min(chars.len().saturating_sub(1));
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let anchor = chars[col];
            if !is_word(anchor) {
                return (col, col + 1);
            }
            let start = (0..=col)
                .rev()
                .take_while(|&i| is_word(chars[i]))
                .last()
                .unwrap_or(col);
            let end = (col..chars.len())
                .take_while(|&i| is_word(chars[i]))
                .last()
                .map(|i| i + 1)
                .unwrap_or(col + 1);
            (start, end)
        })
    }

    /// Read a single filtered diff line from workspace state and run a closure
    /// on it.  Returns `None` if no file diff is loaded or the index is out of
    /// range.
    fn with_filtered_line<R>(
        &self,
        line_index: usize,
        cx: &App,
        f: impl FnOnce(&DiffLineView) -> R,
    ) -> Option<R> {
        let ws = self.workspace.read(cx);
        let tab = ws.diff_tab_state(&self.scope_root)?;
        let doc = tab.file.document.as_ref()?;
        let line = doc
            .lines
            .iter()
            .filter(|l| l.kind != DiffLineKind::FileHeader)
            .nth(line_index)?;
        Some(f(line))
    }

    /// Copy the current text selection to the system clipboard.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(sel) = self.selection else { return };
        let ((start_line, start_col), (end_line, end_col)) = sel.normalized();

        let scope = self.scope_root.clone();
        let text = self
            .workspace
            .read(cx)
            .diff_tab_state(&scope)
            .and_then(|tab| {
                let doc = tab.file.document.as_ref()?;
                let lines: Vec<&DiffLineView> = doc
                    .lines
                    .iter()
                    .filter(|l| l.kind != DiffLineKind::FileHeader)
                    .collect();
                extract_selected_text(&lines, start_line, start_col, end_line, end_col)
            });

        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    // ------------------------------------------------------------------
    // Shared UI helpers
    // ------------------------------------------------------------------

    pub(crate) fn status_summary(palette: &OrcaTheme, file: &ChangedFile) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(Self::status_pill(palette, file.status))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_GREEN))
                    .child(format!("+{}", file.insertions)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_CORAL))
                    .child(format!("-{}", file.deletions)),
            )
            .when(file.is_binary, |summary| {
                summary.child(
                    div()
                        .text_size(px(10.0))
                        .font_family(DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.SEAFOAM))
                        .child("binary"),
                )
            })
    }

    pub(crate) fn status_pill(palette: &OrcaTheme, status: GitFileStatus) -> Div {
        let (label, border, text) = match status {
            GitFileStatus::Added => ("A", palette.STATUS_GREEN, palette.STATUS_GREEN),
            GitFileStatus::Modified => ("M", palette.ORCA_BLUE, palette.ORCA_BLUE),
            GitFileStatus::Deleted => ("D", palette.STATUS_CORAL, palette.STATUS_CORAL),
            GitFileStatus::Renamed => ("R", palette.SEAFOAM, palette.SEAFOAM),
            GitFileStatus::Typechange => ("T", palette.FOG, palette.FOG),
            GitFileStatus::Untracked => ("?", palette.STATUS_AMBER, palette.STATUS_AMBER),
            GitFileStatus::Conflicted => ("C", palette.STATUS_CORAL, palette.STATUS_CORAL),
        };

        div()
            .w(px(20.0))
            .h(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(border))
            .bg(rgba(theme::with_alpha(border, 0x12)))
            .text_size(px(10.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(text))
            .child(label)
    }

    pub(crate) fn empty_panel_message(palette: &OrcaTheme, title: &str, body: Option<&str>) -> Div {
        let mut panel = div()
            .size_full()
            .p(px(16.0))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.PATCH))
                    .child(title.to_string()),
            );
        if let Some(body) = body {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.FOG))
                    .child(body.to_string()),
            );
        }
        panel
    }
}

fn publish_remote_prompt_spec(
    scope_root: PathBuf,
    branch_name: String,
    remotes: Vec<String>,
) -> PromptDialogSpec {
    PromptDialogSpec {
        title: format!("Publish {branch_name}"),
        detail: Some(
            "Choose a remote for the first publish. OrcaShell will run `git push -u <remote> HEAD`."
                .to_string(),
        ),
        confirm_label: "Publish".to_string(),
        confirm_tone: PromptDialogConfirmTone::Primary,
        input: None,
        selection: Some(PromptDialogSelectionSpec {
            options: remotes
                .into_iter()
                .map(|remote| PromptDialogSelectionOption {
                    label: remote.clone(),
                    value: remote,
                })
                .collect(),
            initial_selected: 0,
        }),
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, result| {
            if let Some(remote_name) = result.selection {
                ws.dispatch_publish(&scope_root, remote_name, cx);
            }
        }),
    }
}

fn create_stash_prompt_spec(scope_root: PathBuf) -> PromptDialogSpec {
    PromptDialogSpec {
        title: "Create Stash".to_string(),
        detail: Some("Save the current working tree into a stash for this diff scope.".to_string()),
        confirm_label: "Create Stash".to_string(),
        confirm_tone: PromptDialogConfirmTone::Primary,
        input: Some(PromptDialogInputSpec {
            placeholder: "optional stash message".to_string(),
            initial_value: String::new(),
            allow_empty: true,
            validate: None,
        }),
        selection: None,
        toggles: vec![
            PromptDialogToggleSpec {
                id: "keep_index".to_string(),
                label: "Keep index".to_string(),
                initial_value: false,
            },
            PromptDialogToggleSpec {
                id: "include_untracked".to_string(),
                label: "Include untracked".to_string(),
                initial_value: false,
            },
        ],
        on_confirm: Box::new(move |ws, cx, result| {
            let message = result.input.and_then(|value| {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            });
            let keep_index = result.toggles.get("keep_index").copied().unwrap_or(false);
            let include_untracked = result
                .toggles
                .get("include_untracked")
                .copied()
                .unwrap_or(false);
            ws.create_stash(&scope_root, message, keep_index, include_untracked, cx);
        }),
    }
}

fn pop_stash_prompt_spec(scope_root: PathBuf, label: String) -> PromptDialogSpec {
    PromptDialogSpec {
        title: format!("Pop {label}"),
        detail: Some("Apply this stash and remove it if the apply succeeds.".to_string()),
        confirm_label: "Pop".to_string(),
        confirm_tone: PromptDialogConfirmTone::Primary,
        input: None,
        selection: None,
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, _result| {
            ws.pop_selected_stash(&scope_root, cx);
        }),
    }
}

fn drop_stash_prompt_spec(scope_root: PathBuf, label: String) -> PromptDialogSpec {
    PromptDialogSpec {
        title: format!("Drop {label}"),
        detail: Some("Delete this stash entry permanently.".to_string()),
        confirm_label: "Drop".to_string(),
        confirm_tone: PromptDialogConfirmTone::Destructive,
        input: None,
        selection: None,
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, _result| {
            ws.drop_selected_stash(&scope_root, cx);
        }),
    }
}

fn discard_all_prompt_spec(scope_root: PathBuf) -> PromptDialogSpec {
    PromptDialogSpec {
        title: "Discard All".to_string(),
        detail: Some(
            "Discard all unstaged changes in this diff scope? Staged changes will be left intact."
                .to_string(),
        ),
        confirm_label: "Discard All".to_string(),
        confirm_tone: PromptDialogConfirmTone::Destructive,
        input: None,
        selection: None,
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, _result| {
            ws.discard_all_unstaged(&scope_root, cx);
        }),
    }
}

fn is_displayed_file_stale(file_generation: Option<u64>, latest_generation: Option<u64>) -> bool {
    matches!(
        (file_generation, latest_generation),
        (Some(displayed), Some(latest)) if displayed < latest
    )
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for DiffExplorerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        self.measure_char_width(window);

        // Extract a lightweight snapshot (no document clone).
        let Some(snap) = self.extract_render_snapshot(cx) else {
            return div()
                .id("diff-explorer-missing")
                .size_full()
                .bg(rgb(palette.ABYSS))
                .child(Self::empty_panel_message(
                    &palette,
                    "Diff tab closed",
                    Some("Re-open the diff explorer from a git-backed terminal row."),
                ));
        };

        // Update caches (O(1) on hit, O(n) on miss).
        self.update_tree_cache(cx, &snap);
        self.update_line_cache(cx, &snap);
        self.update_stash_line_cache(cx, &snap);
        self.update_conflict_cache(cx, &snap);

        // Drop commit input if no staged files.
        if snap.view_mode != DiffTabViewMode::WorkingTree || snap.staged_file_count == 0 {
            self.commit_input = None;
        }

        let bounds_ref = self.bounds.clone();
        let scope_root = self.scope_root.clone();

        let mut root = div()
            .id(ElementId::Name(
                format!("diff-explorer-{}", scope_root.display()).into(),
            ))
            .size_full()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, _cx| {
                    window.focus(&this.focus_handle);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let conflict_selected = this
                    .extract_render_snapshot(cx)
                    .filter(|snap| snap.view_mode == DiffTabViewMode::WorkingTree)
                    .and_then(|snap| snap.selected_file)
                    .is_some_and(|selection| selection.section == DiffSectionKind::Conflicted);
                if conflict_selected {
                    if this.handle_conflict_key_down(event, cx) {
                        cx.stop_propagation();
                    }
                } else if is_copy_keystroke(&event.keystroke.key, &event.keystroke.modifiers) {
                    this.copy_selection(cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                // Resize drag.
                if let Some(drag) = this.resize_drag.as_ref() {
                    let total_width = f32::from(this.bounds.borrow().size.width);
                    let max_width = (total_width - MIN_DIFF_WIDTH).max(MIN_TREE_WIDTH);
                    let next_width = (drag.initial_width
                        + (f32::from(event.position.x) - drag.initial_mouse_x))
                        .clamp(MIN_TREE_WIDTH, max_width);
                    let scope_root = this.scope_root.clone();
                    this.workspace.update(cx, |ws, cx| {
                        ws.update_diff_tree_width(&scope_root, next_width, cx);
                    });
                    return;
                }

                if this.conflict_is_selecting {
                    let offset = this.hit_test_conflict_offset(event.position, cx);
                    this.set_conflict_cursor(offset, true, cx);
                    return;
                }

                // Selection drag. Handled at the top level so the selection
                // extends even when the mouse leaves the diff list bounds.
                let is_selecting = this.selection.as_ref().is_some_and(|s| s.is_selecting);
                if is_selecting {
                    let pos = this.hit_test_diff(event.position);
                    this.selection.as_mut().unwrap().end = pos;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    let mut changed = false;
                    if this.resize_drag.is_some() {
                        this.resize_drag = None;
                        changed = true;
                    }
                    if this.diff_scrollbar_drag.is_some() {
                        this.diff_scrollbar_drag = None;
                        changed = true;
                    }
                    if this.conflict_is_selecting {
                        this.conflict_is_selecting = false;
                        changed = true;
                    }
                    if let Some(sel) = this.selection.as_mut() {
                        if sel.is_selecting {
                            sel.is_selecting = false;
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        *bounds_ref.borrow_mut() = bounds;
                    },
                    |_bounds, _prepaint, _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .child(self.render_header(&self.scope_root, &snap, cx));

        // Action banner (between header and body).
        if let Some(banner) = self.render_action_banner(&palette, &snap) {
            root = root.child(banner);
        }
        if let Some(mode_banner) = self.render_mode_banner(&palette, &snap) {
            root = root.child(mode_banner);
        }

        root.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(self.render_tree_pane(&snap, cx))
                .child(
                    div()
                        .w(px(4.0))
                        .h_full()
                        .bg(rgb(palette.CURRENT))
                        .cursor_col_resize()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                let current_width = this
                                    .workspace
                                    .read(cx)
                                    .diff_tab_state(&this.scope_root)
                                    .map(|tab| tab.tree_width)
                                    .unwrap_or(300.0);
                                this.resize_drag = Some(DiffResizeDrag {
                                    initial_mouse_x: f32::from(event.position.x),
                                    initial_width: current_width,
                                });
                                cx.stop_propagation();
                            }),
                        ),
                )
                .child(self.render_diff_pane(&snap, cx)),
        )
    }
}

// ---------------------------------------------------------------------------
// Free functions - diff line rendering
// ---------------------------------------------------------------------------

/// Render a single diff line element for the virtualized list.
///
/// This is a free function (not a method) so it can be captured by the
/// `uniform_list` closure without borrowing `self`.
pub(crate) fn render_diff_line_element(
    palette: &OrcaTheme,
    line: &DiffLineView,
    line_index: usize,
    selection: Option<&DiffSelection>,
    scroll_x: f32,
    trailing_action: Option<AnyElement>,
) -> AnyElement {
    let (background, text_color, gutter) = diff_line_colors(palette, line.kind);
    let text_cell = || {
        div().flex_1().min_w_0().overflow_hidden().child(
            div()
                .whitespace_nowrap()
                .when(scroll_x > 0.0, |d| d.ml(px(-scroll_x)))
                .child(render_highlighted_text(
                    palette,
                    &line.text,
                    line.highlights.as_deref(),
                    selection_range_for_line(selection, line_index, line),
                    line.inline_changes.as_deref(),
                    inline_change_bg(palette, line.kind),
                )),
        )
    };

    let mut row = div()
        .w_full()
        .h(px(LINE_HEIGHT))
        .px(px(LINE_PAD_X))
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap(px(GUTTER_GAP))
        .font_family(DIFF_FONT_FAMILY)
        .text_size(px(11.0))
        .text_color(rgb(text_color))
        .when_some(background, |row, bg| row.bg(rgba(bg)));

    row = match trailing_action {
        Some(action) if line.kind == DiffLineKind::HunkHeader => row
            .child(
                div()
                    .w(px(GUTTER_WIDTH * 2.0 + GUTTER_GAP))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .child(action),
            )
            .child(text_cell()),
        other_action => row
            .child(
                div()
                    .w(px(GUTTER_WIDTH))
                    .flex_shrink_0()
                    .text_right()
                    .text_color(rgb(gutter))
                    .child(line.old_lineno.map_or_else(String::new, |n| n.to_string())),
            )
            .child(
                div()
                    .w(px(GUTTER_WIDTH))
                    .flex_shrink_0()
                    .text_right()
                    .text_color(rgb(gutter))
                    .child(line.new_lineno.map_or_else(String::new, |n| n.to_string())),
            )
            .child(text_cell())
            .when_some(other_action, |row, action| {
                row.child(div().flex_none().ml(px(8.0)).child(action))
            }),
    };

    row.into_any_element()
}

fn render_conflict_line_element(
    palette: &OrcaTheme,
    line: &ConflictRenderLine,
    selection: Option<&Range<usize>>,
    cursor_pos: usize,
    active_block_index: Option<usize>,
    scroll_x: f32,
    char_width: f32,
) -> AnyElement {
    let (background, text_color, gutter) = diff_line_colors(palette, line.kind);
    let is_cursor_line = cursor_pos >= line.start && cursor_pos <= trim_newline_end(line);
    let cursor_column = display_char_count(
        &line.raw_text[..cursor_pos
            .saturating_sub(line.start)
            .min(line.raw_text.len())],
    );
    let selection_range = selection_range_for_conflict_line(selection, line);
    let is_active_conflict_line = line
        .block_index
        .zip(active_block_index)
        .is_some_and(|(line_block, active)| line_block == active);
    let active_border = if is_active_conflict_line {
        rgba(theme::with_alpha(palette.ORCA_BLUE, 0x73))
    } else {
        rgba(theme::with_alpha(palette.ORCA_BLUE, 0x00))
    };

    let mut row = div()
        .w_full()
        .h(px(LINE_HEIGHT))
        .px(px(LINE_PAD_X))
        .border_l_2()
        .border_color(active_border)
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap(px(GUTTER_GAP))
        .font_family(DIFF_FONT_FAMILY)
        .text_size(px(11.0))
        .text_color(rgb(text_color))
        .when_some(background, |row, bg| row.bg(rgba(bg)));

    row = row
        .child(div().w(px(GUTTER_WIDTH)).flex_shrink_0())
        .child(
            div()
                .w(px(GUTTER_WIDTH))
                .flex_shrink_0()
                .text_right()
                .text_color(rgb(gutter))
                .child(line.line_number.to_string()),
        )
        .child(
            div().relative().flex_1().min_w_0().overflow_hidden().child(
                div()
                    .relative()
                    .whitespace_nowrap()
                    .when(scroll_x > 0.0, |d| d.ml(px(-scroll_x)))
                    .child(render_highlighted_text(
                        palette,
                        &line.raw_text,
                        line.highlights.as_deref(),
                        selection_range,
                        None,
                        None,
                    ))
                    .when(is_cursor_line, |row| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(cursor_column as f32 * char_width))
                                .top(px(2.0))
                                .w(px(1.0))
                                .h(px(LINE_HEIGHT - 4.0))
                                .bg(rgb(palette.ORCA_BLUE)),
                        )
                    }),
            ),
        );

    row.into_any_element()
}

/// Return (background, text_color, gutter_color) for a given diff-line kind.
pub(crate) fn diff_line_colors(palette: &OrcaTheme, kind: DiffLineKind) -> (Option<u32>, u32, u32) {
    match kind {
        DiffLineKind::Addition => (
            Some(theme::with_alpha(palette.STATUS_GREEN, 0x1F)),
            palette.BONE,
            palette.FOG,
        ),
        DiffLineKind::Deletion => (
            Some(theme::with_alpha(palette.STATUS_CORAL, 0x1F)),
            palette.BONE,
            palette.FOG,
        ),
        DiffLineKind::HunkHeader => (
            Some(theme::with_alpha(palette.ORCA_BLUE, 0x10)),
            palette.SLATE,
            palette.SLATE,
        ),
        DiffLineKind::FileHeader => (
            Some(theme::with_alpha(palette.CURRENT, 0xE0)),
            palette.PATCH,
            palette.SLATE,
        ),
        DiffLineKind::BinaryNotice => (
            Some(theme::with_alpha(palette.SEAFOAM, 0x10)),
            palette.SEAFOAM,
            palette.FOG,
        ),
        DiffLineKind::ConflictMarker => (
            Some(theme::with_alpha(palette.STATUS_AMBER, 0x1A)),
            palette.STATUS_AMBER,
            palette.STATUS_AMBER,
        ),
        DiffLineKind::ConflictOurs => (
            Some(theme::with_alpha(palette.STATUS_GREEN, 0x16)),
            palette.BONE,
            palette.FOG,
        ),
        DiffLineKind::ConflictBase => (
            Some(theme::with_alpha(palette.STATUS_AMBER, 0x12)),
            palette.BONE,
            palette.FOG,
        ),
        DiffLineKind::ConflictTheirs => (
            Some(theme::with_alpha(palette.STATUS_CORAL, 0x16)),
            palette.BONE,
            palette.FOG,
        ),
        DiffLineKind::Context => (None, palette.BONE, palette.FOG),
    }
}

/// Return the inline change background color for a given line kind, if applicable.
pub(crate) fn inline_change_bg(palette: &OrcaTheme, kind: DiffLineKind) -> Option<Hsla> {
    match kind {
        DiffLineKind::Addition => Some(rgba(theme::with_alpha(palette.STATUS_GREEN, 0x2E)).into()),
        DiffLineKind::Deletion => Some(rgba(theme::with_alpha(palette.STATUS_CORAL, 0x2E)).into()),
        _ => None,
    }
}

fn build_conflict_render_lines(
    text: &str,
    relative_path: &Path,
    theme_id: ThemeId,
) -> Vec<ConflictRenderLine> {
    build_conflict_render_lines_with_cache(text, relative_path, theme_id, None).0
}

fn build_conflict_render_lines_with_cache(
    text: &str,
    relative_path: &Path,
    theme_id: ThemeId,
    prior_cache: Option<&CachedConflictLines>,
) -> (Vec<ConflictRenderLine>, Vec<ConflictHighlightAnchor>) {
    let mut lines = classify_conflict_render_lines(text);
    let Some(initial_highlighter) = Highlighter::for_path(relative_path, theme_id) else {
        return (lines, Vec::new());
    };

    let reuse_plan =
        prior_cache.and_then(|prior| build_conflict_highlight_reuse_plan(text, &lines, prior));
    let mut anchors = Vec::new();
    let mut shared_highlighter = Some(initial_highlighter);
    let mut current_mode = ConflictHighlightMode::Stateful;
    let mut line_index = 0usize;
    let mut consensus_lines_since_anchor = 0usize;

    if let Some(prior) = prior_cache.zip(reuse_plan.as_ref()) {
        let (prior_cache, reuse_plan) = prior;
        for (line, prior_line) in lines
            .iter_mut()
            .zip(prior_cache.lines.iter())
            .take(reuse_plan.restart_line)
        {
            line.highlights = prior_line.highlights.clone();
            line.highlight_mode = prior_line.highlight_mode;
        }
        anchors.extend(
            prior_cache
                .anchors
                .iter()
                .filter(|anchor| anchor.line_index <= reuse_plan.restart_line)
                .cloned(),
        );
        if let Some(anchor) =
            conflict_anchor_before_or_at(prior_cache.anchors.as_ref(), reuse_plan.restart_line)
        {
            current_mode = anchor.mode;
            shared_highlighter = anchor.checkpoint.clone().and_then(|checkpoint| {
                Highlighter::from_checkpoint(relative_path, theme_id, checkpoint)
            });
        }
        line_index = reuse_plan.restart_line;
    }

    if line_index == 0 {
        if try_reuse_conflict_highlight_suffix(
            &mut lines,
            0,
            current_mode,
            shared_highlighter.as_ref(),
            prior_cache,
            reuse_plan.as_ref(),
            &mut anchors,
        ) {
            return (lines, anchors);
        }
        push_conflict_highlight_anchor(&mut anchors, 0, current_mode, shared_highlighter.as_ref());
    }

    while line_index < lines.len() {
        if lines[line_index].kind == DiffLineKind::ConflictMarker
            && lines[line_index].raw_text.starts_with("<<<<<<< ")
        {
            let block = highlight_conflict_block(
                &mut lines,
                line_index,
                relative_path,
                theme_id,
                &mut current_mode,
                &mut shared_highlighter,
            );
            line_index = block.next_line_index;
            consensus_lines_since_anchor = 0;
            if block.push_anchor {
                if try_reuse_conflict_highlight_suffix(
                    &mut lines,
                    line_index,
                    current_mode,
                    shared_highlighter.as_ref(),
                    prior_cache,
                    reuse_plan.as_ref(),
                    &mut anchors,
                ) {
                    return (lines, anchors);
                }
                push_conflict_highlight_anchor(
                    &mut anchors,
                    line_index,
                    current_mode,
                    shared_highlighter.as_ref(),
                );
            }
            continue;
        }

        lines[line_index].highlight_mode = current_mode;
        match current_mode {
            ConflictHighlightMode::Stateful => {
                let highlighter = shared_highlighter
                    .as_mut()
                    .expect("stateful conflict highlighting requires a highlighter");
                lines[line_index].highlights = Some(Rc::<[HighlightedSpan]>::from(
                    highlighter.highlight_line(&lines[line_index].raw_text),
                ));
                consensus_lines_since_anchor += 1;
                line_index += 1;
                if consensus_lines_since_anchor >= CONFLICT_HIGHLIGHT_ANCHOR_INTERVAL {
                    if try_reuse_conflict_highlight_suffix(
                        &mut lines,
                        line_index,
                        current_mode,
                        shared_highlighter.as_ref(),
                        prior_cache,
                        reuse_plan.as_ref(),
                        &mut anchors,
                    ) {
                        return (lines, anchors);
                    }
                    push_conflict_highlight_anchor(
                        &mut anchors,
                        line_index,
                        current_mode,
                        shared_highlighter.as_ref(),
                    );
                    consensus_lines_since_anchor = 0;
                }
            }
            ConflictHighlightMode::Fallback => {
                lines[line_index].highlights =
                    shared_conflict_highlights(conflict_line_stateless_highlights(
                        relative_path,
                        theme_id,
                        lines[line_index].kind,
                        &lines[line_index].raw_text,
                    ));
                line_index += 1;
            }
        }
    }

    (lines, anchors)
}

#[derive(Debug, Clone, Copy)]
struct ConflictHighlightReusePlan {
    restart_line: usize,
    reuse_start_new_line: usize,
    line_delta: isize,
}

#[derive(Debug, Clone, Copy)]
struct ConflictBlockHighlightOutcome {
    next_line_index: usize,
    push_anchor: bool,
}

fn classify_conflict_render_lines(text: &str) -> Vec<ConflictRenderLine> {
    if text.is_empty() {
        return vec![ConflictRenderLine {
            kind: DiffLineKind::Context,
            raw_text: String::new(),
            highlights: None,
            highlight_mode: ConflictHighlightMode::Fallback,
            line_number: 1,
            start: 0,
            end: 0,
            block_index: None,
        }];
    }

    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1u32;
    let mut state = DiffLineKind::Context;
    let mut block_index = 0usize;
    let mut active_block = None;

    for (index, byte) in text.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let end = index + 1;
        let raw = &text[start..end];
        let prior_block = active_block;
        let kind =
            classify_conflict_render_line(raw, &mut state, &mut active_block, &mut block_index);
        lines.push(ConflictRenderLine {
            kind,
            raw_text: raw.to_string(),
            highlights: None,
            highlight_mode: ConflictHighlightMode::Fallback,
            line_number,
            start,
            end,
            block_index: if raw.starts_with(">>>>>>> ") {
                prior_block
            } else {
                active_block
            },
        });
        start = end;
        line_number += 1;
    }

    if start < text.len() {
        let raw = &text[start..];
        let prior_block = active_block;
        let kind =
            classify_conflict_render_line(raw, &mut state, &mut active_block, &mut block_index);
        lines.push(ConflictRenderLine {
            kind,
            raw_text: raw.to_string(),
            highlights: None,
            highlight_mode: ConflictHighlightMode::Fallback,
            line_number,
            start,
            end: text.len(),
            block_index: if raw.starts_with(">>>>>>> ") {
                prior_block
            } else {
                active_block
            },
        });
    }

    lines
}

fn shared_conflict_highlights(
    highlights: Option<Vec<HighlightedSpan>>,
) -> Option<Rc<[HighlightedSpan]>> {
    highlights.map(Rc::<[HighlightedSpan]>::from)
}

fn highlight_fallback_conflict_block(
    lines: &mut [ConflictRenderLine],
    start_line_index: usize,
    relative_path: &Path,
    theme_id: ThemeId,
) -> ConflictBlockHighlightOutcome {
    let mut line_index = start_line_index;
    while line_index < lines.len() {
        lines[line_index].highlight_mode = ConflictHighlightMode::Fallback;
        lines[line_index].highlights =
            shared_conflict_highlights(conflict_line_stateless_highlights(
                relative_path,
                theme_id,
                lines[line_index].kind,
                &lines[line_index].raw_text,
            ));
        if lines[line_index].kind == DiffLineKind::ConflictMarker
            && lines[line_index].raw_text.starts_with(">>>>>>> ")
        {
            return ConflictBlockHighlightOutcome {
                next_line_index: line_index + 1,
                push_anchor: false,
            };
        }
        line_index += 1;
    }

    ConflictBlockHighlightOutcome {
        next_line_index: line_index,
        push_anchor: false,
    }
}

fn highlight_conflict_block(
    lines: &mut [ConflictRenderLine],
    start_line_index: usize,
    relative_path: &Path,
    theme_id: ThemeId,
    current_mode: &mut ConflictHighlightMode,
    shared_highlighter: &mut Option<Highlighter>,
) -> ConflictBlockHighlightOutcome {
    if *current_mode == ConflictHighlightMode::Fallback {
        return highlight_fallback_conflict_block(lines, start_line_index, relative_path, theme_id);
    }

    let entry_checkpoint = shared_highlighter
        .as_ref()
        .expect("stateful conflict highlighting requires a highlighter")
        .checkpoint();
    let mut ours = Highlighter::from_checkpoint(relative_path, theme_id, entry_checkpoint.clone())
        .expect("conflict block should reuse the current file highlighter");
    let mut theirs =
        Highlighter::from_checkpoint(relative_path, theme_id, entry_checkpoint.clone())
            .expect("conflict block should reuse the current file highlighter");
    let mut base =
        Highlighter::from_checkpoint(relative_path, theme_id, entry_checkpoint).filter(|_| {
            lines[start_line_index..]
                .iter()
                .take_while(|line| {
                    !(line.kind == DiffLineKind::ConflictMarker
                        && line.raw_text.starts_with(">>>>>>> "))
                })
                .any(|line| line.kind == DiffLineKind::ConflictBase)
        });

    let mut line_index = start_line_index;
    while line_index < lines.len() {
        let line = &mut lines[line_index];
        line.highlight_mode = ConflictHighlightMode::Stateful;
        line.highlights = match line.kind {
            DiffLineKind::ConflictMarker => None,
            DiffLineKind::ConflictOurs => Some(Rc::<[HighlightedSpan]>::from(
                ours.highlight_line(&line.raw_text),
            )),
            DiffLineKind::ConflictBase => base
                .as_mut()
                .map(|highlighter| {
                    Rc::<[HighlightedSpan]>::from(highlighter.highlight_line(&line.raw_text))
                })
                .or_else(|| {
                    shared_conflict_highlights(conflict_line_stateless_highlights(
                        relative_path,
                        theme_id,
                        line.kind,
                        &line.raw_text,
                    ))
                }),
            DiffLineKind::ConflictTheirs => Some(Rc::<[HighlightedSpan]>::from(
                theirs.highlight_line(&line.raw_text),
            )),
            DiffLineKind::Context
            | DiffLineKind::FileHeader
            | DiffLineKind::HunkHeader
            | DiffLineKind::Addition
            | DiffLineKind::Deletion
            | DiffLineKind::BinaryNotice => {
                shared_conflict_highlights(conflict_line_stateless_highlights(
                    relative_path,
                    theme_id,
                    line.kind,
                    &line.raw_text,
                ))
            }
        };
        if line.kind == DiffLineKind::ConflictMarker && line.raw_text.starts_with(">>>>>>> ") {
            let mut exits = vec![ours.checkpoint(), theirs.checkpoint()];
            if let Some(base_highlighter) = base {
                exits.push(base_highlighter.checkpoint());
            }
            if exits.windows(2).all(|window| window[0] == window[1]) {
                *shared_highlighter = Highlighter::from_checkpoint(
                    relative_path,
                    theme_id,
                    exits.into_iter().next().expect("at least one exit state"),
                );
                *current_mode = ConflictHighlightMode::Stateful;
                return ConflictBlockHighlightOutcome {
                    next_line_index: line_index + 1,
                    push_anchor: true,
                };
            }
            *shared_highlighter = None;
            *current_mode = ConflictHighlightMode::Fallback;
            return ConflictBlockHighlightOutcome {
                next_line_index: line_index + 1,
                push_anchor: false,
            };
        }
        line_index += 1;
    }

    *shared_highlighter = None;
    *current_mode = ConflictHighlightMode::Fallback;
    ConflictBlockHighlightOutcome {
        next_line_index: line_index,
        push_anchor: false,
    }
}

fn conflict_line_stateless_highlights(
    relative_path: &Path,
    theme_id: ThemeId,
    kind: DiffLineKind,
    raw: &str,
) -> Option<Vec<HighlightedSpan>> {
    match kind {
        DiffLineKind::ConflictMarker => None,
        DiffLineKind::ConflictOurs
        | DiffLineKind::ConflictBase
        | DiffLineKind::ConflictTheirs
        | DiffLineKind::Context => highlight_line_for_path(relative_path, theme_id, raw),
        DiffLineKind::FileHeader
        | DiffLineKind::HunkHeader
        | DiffLineKind::Addition
        | DiffLineKind::Deletion
        | DiffLineKind::BinaryNotice => None,
    }
}

fn build_conflict_highlight_reuse_plan(
    text: &str,
    lines: &[ConflictRenderLine],
    prior_cache: &CachedConflictLines,
) -> Option<ConflictHighlightReusePlan> {
    let prefix_len = common_prefix_len(prior_cache.raw_text.as_bytes(), text.as_bytes());
    let suffix_len =
        common_suffix_len(prior_cache.raw_text.as_bytes(), text.as_bytes(), prefix_len);
    let first_dirty_line = line_index_for_offset_in_text(text, prefix_len);
    let restart_line = conflict_anchor_before_or_at(prior_cache.anchors.as_ref(), first_dirty_line)
        .map(|anchor| anchor.line_index)
        .unwrap_or(0);

    let new_suffix_start = text.len().saturating_sub(suffix_len);
    let old_suffix_start = prior_cache.raw_text.len().saturating_sub(suffix_len);
    let reuse_start_new_line = first_line_starting_at_or_after(lines, new_suffix_start);
    let reuse_start_old_line =
        first_line_starting_at_or_after(prior_cache.lines.as_ref(), old_suffix_start);
    (reuse_start_new_line < lines.len() && reuse_start_old_line < prior_cache.lines.len()).then(
        || ConflictHighlightReusePlan {
            restart_line,
            reuse_start_new_line,
            line_delta: reuse_start_old_line as isize - reuse_start_new_line as isize,
        },
    )
}

fn try_reuse_conflict_highlight_suffix(
    lines: &mut [ConflictRenderLine],
    anchor_line_index: usize,
    current_mode: ConflictHighlightMode,
    shared_highlighter: Option<&Highlighter>,
    prior_cache: Option<&CachedConflictLines>,
    reuse_plan: Option<&ConflictHighlightReusePlan>,
    anchors: &mut Vec<ConflictHighlightAnchor>,
) -> bool {
    let Some(prior_cache) = prior_cache else {
        return false;
    };
    let Some(reuse_plan) = reuse_plan else {
        return false;
    };
    if anchor_line_index < reuse_plan.reuse_start_new_line || anchor_line_index >= lines.len() {
        return false;
    }
    let old_anchor_line = anchor_line_index as isize + reuse_plan.line_delta;
    if old_anchor_line < 0 {
        return false;
    }
    let old_anchor_line = old_anchor_line as usize;
    let Some(old_anchor) = conflict_anchor_at_line(prior_cache.anchors.as_ref(), old_anchor_line)
    else {
        return false;
    };
    if old_anchor.mode != current_mode {
        return false;
    }
    let current_checkpoint = shared_highlighter.map(Highlighter::checkpoint);
    if current_mode == ConflictHighlightMode::Stateful
        && old_anchor.checkpoint != current_checkpoint
    {
        return false;
    }

    for (new_index, line) in lines.iter_mut().enumerate().skip(anchor_line_index) {
        let old_index = new_index as isize + reuse_plan.line_delta;
        if old_index < 0 || old_index as usize >= prior_cache.lines.len() {
            return false;
        }
        let prior_line = &prior_cache.lines[old_index as usize];
        if prior_line.kind != line.kind || prior_line.raw_text != line.raw_text {
            return false;
        }
        line.highlights = prior_line.highlights.clone();
        line.highlight_mode = prior_line.highlight_mode;
    }

    for anchor in prior_cache
        .anchors
        .iter()
        .filter(|anchor| anchor.line_index >= old_anchor_line)
    {
        let new_line_index = anchor.line_index as isize - reuse_plan.line_delta;
        if new_line_index < anchor_line_index as isize || new_line_index < 0 {
            continue;
        }
        anchors.push(ConflictHighlightAnchor {
            line_index: new_line_index as usize,
            mode: anchor.mode,
            checkpoint: anchor.checkpoint.clone(),
        });
    }
    true
}

fn push_conflict_highlight_anchor(
    anchors: &mut Vec<ConflictHighlightAnchor>,
    line_index: usize,
    mode: ConflictHighlightMode,
    shared_highlighter: Option<&Highlighter>,
) {
    if anchors
        .last()
        .is_some_and(|anchor| anchor.line_index == line_index)
    {
        anchors.pop();
    }
    anchors.push(ConflictHighlightAnchor {
        line_index,
        mode,
        checkpoint: shared_highlighter.map(Highlighter::checkpoint),
    });
}

fn conflict_anchor_before_or_at(
    anchors: &[ConflictHighlightAnchor],
    line_index: usize,
) -> Option<&ConflictHighlightAnchor> {
    anchors
        .iter()
        .rev()
        .find(|anchor| anchor.line_index <= line_index)
}

fn conflict_anchor_at_line(
    anchors: &[ConflictHighlightAnchor],
    line_index: usize,
) -> Option<&ConflictHighlightAnchor> {
    anchors
        .iter()
        .find(|anchor| anchor.line_index == line_index)
}

fn common_prefix_len(old: &[u8], new: &[u8]) -> usize {
    old.iter()
        .zip(new.iter())
        .take_while(|(old, new)| old == new)
        .count()
}

fn common_suffix_len(old: &[u8], new: &[u8], prefix_len: usize) -> usize {
    let old_remaining = old.len().saturating_sub(prefix_len);
    let new_remaining = new.len().saturating_sub(prefix_len);
    old[old.len().saturating_sub(old_remaining)..]
        .iter()
        .rev()
        .zip(new[new.len().saturating_sub(new_remaining)..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count()
}

fn first_line_starting_at_or_after(lines: &[ConflictRenderLine], offset: usize) -> usize {
    lines
        .iter()
        .position(|line| line.start >= offset)
        .unwrap_or(lines.len())
}

fn classify_conflict_render_line(
    raw: &str,
    state: &mut DiffLineKind,
    active_block: &mut Option<usize>,
    block_index: &mut usize,
) -> DiffLineKind {
    if raw.starts_with("<<<<<<< ") {
        *active_block = Some(*block_index);
        *state = DiffLineKind::ConflictOurs;
        return DiffLineKind::ConflictMarker;
    }
    if raw.starts_with("||||||| ") {
        *state = DiffLineKind::ConflictBase;
        return DiffLineKind::ConflictMarker;
    }
    if raw.starts_with("=======") {
        *state = DiffLineKind::ConflictTheirs;
        return DiffLineKind::ConflictMarker;
    }
    if raw.starts_with(">>>>>>> ") {
        *state = DiffLineKind::Context;
        let current = *active_block;
        *active_block = None;
        *block_index += 1;
        return if current.is_some() {
            DiffLineKind::ConflictMarker
        } else {
            DiffLineKind::Context
        };
    }

    match *state {
        DiffLineKind::ConflictOurs => DiffLineKind::ConflictOurs,
        DiffLineKind::ConflictBase => DiffLineKind::ConflictBase,
        DiffLineKind::ConflictTheirs => DiffLineKind::ConflictTheirs,
        _ => DiffLineKind::Context,
    }
}

fn selection_range_for_conflict_line(
    selection: Option<&Range<usize>>,
    line: &ConflictRenderLine,
) -> Option<Range<usize>> {
    let selection = selection?;
    let line_start = line.start;
    let line_end = trim_newline_end(line);
    if selection.end <= line_start || selection.start >= line_end {
        return None;
    }
    let raw_start = selection.start.max(line_start) - line_start;
    let raw_end = selection.end.min(line_end) - line_start;
    let raw_range = raw_start..raw_end;
    let mapped = map_raw_to_display_ranges(&line.raw_text, std::slice::from_ref(&raw_range));
    mapped.into_iter().next()
}

fn trim_newline_end(line: &ConflictRenderLine) -> usize {
    line.start + line.raw_text.trim_end_matches(['\r', '\n']).len()
}

fn display_char_count(text: &str) -> usize {
    render_diff_text_string(text).chars().count()
}

fn line_index_for_offset_in_text(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
}

fn line_start_for_offset(text: &str, offset: usize) -> usize {
    let offset = clamp_offset_to_char_boundary(text, offset.min(text.len()));
    text[..offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn touched_line_starts(
    text: &str,
    selection: Option<&Range<usize>>,
    cursor_pos: usize,
) -> Vec<usize> {
    let (first_start, last_start) = if let Some(selection) = selection {
        let start = clamp_offset_to_char_boundary(text, selection.start.min(text.len()));
        let end = clamp_offset_to_char_boundary(text, selection.end.min(text.len()));
        let first_start = line_start_for_offset(text, start);
        let last_offset =
            if end > start && text.as_bytes().get(end.saturating_sub(1)) == Some(&b'\n') {
                end.saturating_sub(1)
            } else {
                end
            };
        (first_start, line_start_for_offset(text, last_offset))
    } else {
        let line_start = line_start_for_offset(text, cursor_pos);
        (line_start, line_start)
    };

    let mut starts = vec![first_start];
    let mut current = first_start;
    while current < last_start {
        let Some(relative_newline) = text[current..].find('\n') else {
            break;
        };
        current += relative_newline + 1;
        if current <= last_start {
            starts.push(current);
        }
    }
    starts
}

fn selection_spans_multiple_lines(text: &str, selection: &Range<usize>) -> bool {
    text[selection.clone()].contains('\n')
}

fn line_and_column_for_offset(lines: &[ConflictRenderLine], offset: usize) -> (usize, usize) {
    let line_index = lines
        .iter()
        .position(|line| offset >= line.start && offset <= line.end)
        .unwrap_or_else(|| lines.len().saturating_sub(1));
    let line = &lines[line_index];
    let local = offset.saturating_sub(line.start).min(line.raw_text.len());
    let local = clamp_offset_to_char_boundary(&line.raw_text, local);
    (line_index, display_char_count(&line.raw_text[..local]))
}

fn byte_offset_for_line_column(
    lines: &[ConflictRenderLine],
    line_index: usize,
    column: usize,
) -> usize {
    let Some(line) = lines.get(line_index) else {
        return 0;
    };
    let raw = line.raw_text.trim_end_matches(['\r', '\n']);
    let byte = char_to_byte(raw, column.min(raw.chars().count()));
    line.start + byte
}

fn clamp_offset_to_char_boundary(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len();
    }
    let mut offset = offset;
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn ceil_offset_to_char_boundary(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len();
    }
    let mut offset = offset;
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

fn previous_char_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_offset_to_char_boundary(text, offset);
    if offset == 0 {
        0
    } else {
        text[..offset]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    let offset = clamp_offset_to_char_boundary(text, offset);
    if offset >= text.len() {
        text.len()
    } else {
        let mut iter = text[offset..].char_indices();
        let _ = iter.next();
        iter.next()
            .map(|(index, _)| offset + index)
            .unwrap_or(text.len())
    }
}

fn scroll_delta_components(delta: ScrollDelta, measured_char_width: f32) -> (f32, f32) {
    match delta {
        ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
        ScrollDelta::Lines(delta) => (
            delta.x * measured_char_width * 3.0,
            delta.y * LINE_HEIGHT * 3.0,
        ),
    }
}

fn key_acts_as_backward_delete(key: &str) -> bool {
    key == "backspace" || (cfg!(target_os = "macos") && key == "delete")
}

fn key_acts_as_forward_delete(key: &str) -> bool {
    key == "delete" && !cfg!(target_os = "macos")
}

fn delete_document_backward(document: &mut ConflictEditorDocument) -> bool {
    if document.selection_range.is_some() {
        replace_document_selection(document, "");
        return true;
    }

    let cursor = clamp_offset_to_char_boundary(&document.raw_text, document.cursor_pos);
    let previous = previous_char_boundary(&document.raw_text, cursor);
    if previous == cursor {
        return false;
    }

    document.selection_range = Some(previous..cursor);
    replace_document_selection(document, "");
    true
}

fn delete_document_forward(document: &mut ConflictEditorDocument) -> bool {
    if document.selection_range.is_some() {
        replace_document_selection(document, "");
        return true;
    }

    let cursor = clamp_offset_to_char_boundary(&document.raw_text, document.cursor_pos);
    let next = next_char_boundary(&document.raw_text, cursor);
    if next == cursor {
        return false;
    }

    document.selection_range = Some(cursor..next);
    replace_document_selection(document, "");
    true
}

fn active_block_index_for_cursor(
    blocks: &[ParsedConflictBlock],
    cursor_pos: usize,
) -> Option<usize> {
    blocks
        .iter()
        .find(|block| cursor_pos >= block.whole_block.start && cursor_pos <= block.whole_block.end)
        .map(|block| block.block_index)
}

fn navigated_conflict_block_index(
    block_total: usize,
    active_block_index: Option<usize>,
    forward: bool,
) -> Option<usize> {
    if block_total == 0 {
        return None;
    }

    match active_block_index {
        Some(index) if forward => Some((index + 1).min(block_total.saturating_sub(1))),
        Some(index) => Some(index.saturating_sub(1)),
        None if forward => Some(0),
        None => Some(block_total.saturating_sub(1)),
    }
}

fn conflict_block_navigation_disabled(
    block_total: usize,
    active_block_index: Option<usize>,
    forward: bool,
) -> bool {
    if block_total == 0 {
        return true;
    }

    match active_block_index {
        Some(index) if forward => index + 1 >= block_total,
        Some(index) => index == 0,
        None => false,
    }
}

fn reparse_conflict_document(document: &mut ConflictEditorDocument) {
    let parsed = parse_conflict_file_text(&document.raw_text);
    let previous_active = document.active_block_index;
    let previous_cursor = clamp_offset_to_char_boundary(&document.raw_text, document.cursor_pos);
    document.version = document.version.saturating_add(1);
    document.cursor_pos = previous_cursor;
    document.selection_range = document.selection_range.take().and_then(|range| {
        let start = clamp_offset_to_char_boundary(
            &document.raw_text,
            range.start.min(document.raw_text.len()),
        );
        let end = clamp_offset_to_char_boundary(
            &document.raw_text,
            range.end.min(document.raw_text.len()),
        );
        (start < end).then_some(start..end)
    });
    match parsed {
        Ok(parsed) => {
            document.blocks = parsed.blocks;
            document.has_base_sections = parsed.has_base_sections;
            document.parse_error = None;
        }
        Err(error) => {
            document.blocks.clear();
            document.has_base_sections = false;
            document.parse_error = Some(error.to_string());
        }
    }
    document.active_block_index =
        active_block_index_for_cursor(&document.blocks, document.cursor_pos)
            .or(previous_active.filter(|index| document.blocks.get(*index).is_some()))
            .or_else(|| document.blocks.first().map(|block| block.block_index));
}

fn replace_document_range(
    document: &mut ConflictEditorDocument,
    range: Range<usize>,
    replacement: &str,
) {
    document.raw_text.replace_range(range.clone(), replacement);
    document.cursor_pos = range.start + replacement.len();
    document.selection_range = None;
    document.is_dirty = true;
    reparse_conflict_document(document);
}

fn replace_document_selection(document: &mut ConflictEditorDocument, replacement: &str) {
    if let Some(selection) = document.selection_range.clone() {
        replace_document_range(document, selection, replacement);
        return;
    }
    let cursor = clamp_offset_to_char_boundary(&document.raw_text, document.cursor_pos);
    document.raw_text.insert_str(cursor, replacement);
    document.cursor_pos = cursor + replacement.len();
    document.selection_range = None;
    document.is_dirty = true;
    reparse_conflict_document(document);
}

fn indent_document_selection(document: &mut ConflictEditorDocument) -> bool {
    if let Some(selection) = document.selection_range.clone() {
        if !selection_spans_multiple_lines(&document.raw_text, &selection) {
            replace_document_selection(document, CONFLICT_INDENT);
            return true;
        }

        let line_starts =
            touched_line_starts(&document.raw_text, Some(&selection), document.cursor_pos);
        let mut raw_text = document.raw_text.clone();
        let mut inserted = 0usize;
        for start in &line_starts {
            let insertion_at = start + inserted;
            raw_text.insert_str(insertion_at, CONFLICT_INDENT);
            inserted += CONFLICT_INDENT.len();
        }

        let start_shift = line_starts
            .iter()
            .filter(|&&start| start <= selection.start)
            .count()
            * CONFLICT_INDENT.len();
        let end_shift = line_starts
            .iter()
            .filter(|&&start| start <= selection.end)
            .count()
            * CONFLICT_INDENT.len();

        document.raw_text = raw_text;
        document.cursor_pos = selection.end + end_shift;
        document.selection_range =
            Some((selection.start + start_shift)..(selection.end + end_shift));
        document.is_dirty = true;
        reparse_conflict_document(document);
        return true;
    }

    replace_document_selection(document, CONFLICT_INDENT);
    true
}

fn leading_outdent_len(text: &str, line_start: usize) -> usize {
    let remaining = &text[line_start..];
    if remaining.starts_with('\t') {
        return 1;
    }
    remaining
        .bytes()
        .take(CONFLICT_INDENT.len())
        .take_while(|byte| *byte == b' ')
        .count()
}

fn adjust_offset_for_removals(offset: usize, removals: &[(usize, usize)]) -> usize {
    let mut adjusted = offset;
    for (start, len) in removals {
        let end = start + len;
        if offset >= end {
            adjusted = adjusted.saturating_sub(*len);
        } else if offset > *start {
            adjusted = *start;
            break;
        }
    }
    adjusted
}

fn outdent_document_selection(document: &mut ConflictEditorDocument) -> bool {
    let selection = document.selection_range.clone();
    let line_starts =
        touched_line_starts(&document.raw_text, selection.as_ref(), document.cursor_pos);
    let removals: Vec<(usize, usize)> = line_starts
        .into_iter()
        .filter_map(|start| {
            let len = leading_outdent_len(&document.raw_text, start);
            (len > 0).then_some((start, len))
        })
        .collect();
    if removals.is_empty() {
        return false;
    }

    let mut raw_text = document.raw_text.clone();
    for (start, len) in removals.iter().rev() {
        raw_text.replace_range(*start..(*start + *len), "");
    }

    document.raw_text = raw_text;
    if let Some(selection) = selection {
        let start = adjust_offset_for_removals(selection.start, &removals);
        let end = adjust_offset_for_removals(selection.end, &removals);
        document.selection_range = (start < end).then_some(start..end);
        document.cursor_pos = end;
    } else {
        document.cursor_pos = adjust_offset_for_removals(document.cursor_pos, &removals);
        document.selection_range = None;
    }
    document.is_dirty = true;
    reparse_conflict_document(document);
    true
}

/// Compute the byte range within the line's display text that is selected,
/// or `None` if this line is outside the selection.
pub(crate) fn selection_range_for_line(
    selection: Option<&DiffSelection>,
    line_index: usize,
    line: &DiffLineView,
) -> Option<Range<usize>> {
    let sel = selection?;
    let ((start_line, start_col), (end_line, end_col)) = sel.normalized();
    if line_index < start_line || line_index > end_line {
        return None;
    }

    let text_len = plain_text_len(&line.text, line.highlights.as_deref());
    if text_len == 0 {
        return None;
    }

    let sel_start = if line_index == start_line {
        start_col.min(text_len)
    } else {
        0
    };
    let sel_end = if line_index == end_line {
        end_col.min(text_len)
    } else {
        text_len
    };

    if sel_start < sel_end {
        // Convert char indices to byte offsets in the display text.
        let display = display_text_for_line(&line.text, line.highlights.as_deref());
        let byte_start = char_to_byte(&display, sel_start);
        let byte_end = char_to_byte(&display, sel_end);
        Some(byte_start..byte_end)
    } else {
        None
    }
}

/// Convert a character index to a byte offset in a string.
pub(crate) fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Get the display text length in characters for a diff line.
pub(crate) fn plain_text_len(raw: &str, highlights: Option<&[HighlightedSpan]>) -> usize {
    plain_text_for_line(raw, highlights).chars().count()
}

/// Get the plain text content for a diff line (joining highlighted spans if
/// present, otherwise using the raw text with whitespace normalization).
pub(crate) fn plain_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
    match highlights {
        Some(spans) if !spans.is_empty() => {
            let mut s = String::new();
            for span in spans {
                s.push_str(&span.text);
            }
            s
        }
        _ => {
            let text = raw.trim_end_matches(['\r', '\n']);
            text.replace('\t', "    ").to_string()
        }
    }
}

/// Build the display-ready text for a line (with whitespace normalization),
/// matching what `render_highlighted_text` renders.
pub(crate) fn display_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
    match highlights {
        Some(spans) if !spans.is_empty() => {
            let mut s = String::new();
            for span in spans {
                s.push_str(&span.text);
            }
            s
        }
        _ => render_diff_text_string(raw),
    }
}

/// Get the plain text for a line, normalized for the clipboard (NBSPs → spaces).
pub(crate) fn clipboard_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
    plain_text_for_line(raw, highlights).replace('\u{00A0}', " ")
}

/// Extract the selected text from diff lines.
pub(crate) fn extract_selected_text(
    lines: &[&DiffLineView],
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
) -> Option<String> {
    if start_line > end_line || lines.is_empty() {
        return None;
    }

    let mut result = String::new();
    for line_idx in start_line..=end_line {
        if line_idx >= lines.len() {
            break;
        }
        let line = lines[line_idx];
        let text = clipboard_text_for_line(&line.text, line.highlights.as_deref());
        let chars: Vec<char> = text.chars().collect();

        if start_line == end_line {
            let s = start_col.min(chars.len());
            let e = end_col.min(chars.len());
            result.extend(&chars[s..e]);
        } else if line_idx == start_line {
            let s = start_col.min(chars.len());
            result.extend(&chars[s..]);
            result.push('\n');
        } else if line_idx == end_line {
            let e = end_col.min(chars.len());
            result.extend(&chars[..e]);
        } else {
            result.extend(&chars[..]);
            result.push('\n');
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

// ---------------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------------

pub(crate) fn measure_diff_char_width(window: &mut Window) -> f32 {
    let font = Font {
        family: SharedString::from(DIFF_FONT_FAMILY),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let text_run = TextRun {
        len: "M".len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line("M".into(), px(DIFF_FONT_SIZE), &[text_run], None);
    let width: f32 = shaped.width.into();
    if width > 0.0 {
        width
    } else {
        DEFAULT_CHAR_WIDTH
    }
}

/// Like `render_diff_text` but returns an owned `String`.
pub(crate) fn render_diff_text_string(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    text.replace('\t', "\u{00A0}\u{00A0}\u{00A0}\u{00A0}")
        .replace(' ', "\u{00A0}")
}

/// Map byte ranges from raw diff text to display text byte offsets.
///
/// Raw text has single-byte spaces and tabs; display text replaces them with
/// multi-byte NBSP (`\u{00A0}`, 2 bytes each) and tab→4 NBSPs (8 bytes).
/// Trailing `\r`/`\n` are stripped.
pub(crate) fn map_raw_to_display_ranges(raw: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    if ranges.is_empty() {
        return vec![];
    }
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    // Build a cumulative offset map: for each raw byte position, how much
    // extra display bytes have accumulated before it.
    // We only need to translate the boundary values in `ranges`, so we walk
    // the raw text once and translate on the fly.
    let mut result = Vec::with_capacity(ranges.len());
    // Sort range endpoints we need to translate.
    let mut endpoints: Vec<(usize, usize, bool)> = Vec::with_capacity(ranges.len() * 2);
    for (i, r) in ranges.iter().enumerate() {
        endpoints.push((r.start, i, true));
        endpoints.push((r.end, i, false));
    }
    endpoints.sort_unstable();
    result.resize(ranges.len(), 0usize..0usize);

    let mut display_pos = 0usize;
    let mut ep_idx = 0;

    for (raw_pos, ch) in trimmed.char_indices() {
        // Resolve any endpoints at this raw position.
        while ep_idx < endpoints.len() && endpoints[ep_idx].0 == raw_pos {
            let (_, ri, is_start) = endpoints[ep_idx];
            if is_start {
                result[ri].start = display_pos;
            } else {
                result[ri].end = display_pos;
            }
            ep_idx += 1;
        }
        let display_len = match ch {
            '\t' => 8, // 4 NBSPs × 2 bytes each
            ' ' => 2,  // 1 NBSP × 2 bytes
            _ => ch.len_utf8(),
        };
        display_pos += display_len;
    }
    // Resolve any endpoints at the end.
    while ep_idx < endpoints.len() {
        let (_, ri, is_start) = endpoints[ep_idx];
        if is_start {
            result[ri].start = display_pos;
        } else {
            result[ri].end = display_pos;
        }
        ep_idx += 1;
    }

    result
}

fn normalize_display_ranges(text: &str, mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if ranges.is_empty() {
        return ranges;
    }

    let mut normalized = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        let start = clamp_offset_to_char_boundary(text, range.start.min(text.len()));
        let end = ceil_offset_to_char_boundary(text, range.end.min(text.len()));
        if start < end {
            normalized.push(start..end);
        }
    }

    normalized.sort_by_key(|range| (range.start, range.end));

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(normalized.len());
    for range in normalized {
        if let Some(last) = merged.last_mut() {
            if range.start < last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }

    merged
}

/// Check if a display byte offset falls inside any of the given display ranges.
pub(crate) fn in_any_range(pos: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|r| pos >= r.start && pos < r.end)
}

/// Render diff line text with optional syntax highlighting and selection.
///
/// GPUI's `StyledText::with_highlights` requires **non-overlapping, sorted**
/// ranges.  When a selection intersects syntax spans, we split each span at
/// the selection boundaries so that every sub-range carries the correct
/// composite style (foreground from syntax + background from selection) and
/// no two ranges overlap.
pub(crate) fn render_highlighted_text(
    palette: &OrcaTheme,
    text: &str,
    highlights: Option<&[HighlightedSpan]>,
    selection_range: Option<Range<usize>>,
    inline_changes: Option<&[Range<usize>]>,
    inline_bg: Option<Hsla>,
) -> AnyElement {
    let sel_bg: Hsla = rgba(theme::with_alpha(palette.ORCA_BLUE, 0x40)).into();

    // Map inline change ranges from raw text bytes to display text bytes.
    let display_text = display_text_for_line(text, highlights);
    let display_inline: Vec<Range<usize>> = match (inline_changes, inline_bg) {
        (Some(ranges), Some(_)) if !ranges.is_empty() => {
            normalize_display_ranges(&display_text, map_raw_to_display_ranges(text, ranges))
        }
        _ => vec![],
    };
    let selection_range = selection_range.and_then(|range| {
        normalize_display_ranges(&display_text, vec![range])
            .into_iter()
            .next()
    });

    match highlights {
        Some(spans) if !spans.is_empty() => {
            let mut full_text = String::new();
            let mut highlight_ranges: Vec<(Range<usize>, HighlightStyle)> =
                Vec::with_capacity(spans.len() * 5);

            for span in spans {
                let span_start = full_text.len();
                full_text.push_str(&span.text);
                let span_end = full_text.len();
                if span_start >= span_end {
                    continue;
                }

                let fg_color: Hsla = rgb(span.color).into();

                // Collect all boundary points within this span.
                let mut boundaries = Vec::with_capacity(8);
                boundaries.push(span_start);
                boundaries.push(span_end);

                if let Some(sel) = &selection_range {
                    if sel.start > span_start && sel.start < span_end {
                        boundaries.push(sel.start);
                    }
                    if sel.end > span_start && sel.end < span_end {
                        boundaries.push(sel.end);
                    }
                }
                for r in &display_inline {
                    if r.start > span_start && r.start < span_end {
                        boundaries.push(r.start);
                    }
                    if r.end > span_start && r.end < span_end {
                        boundaries.push(r.end);
                    }
                }

                boundaries.sort_unstable();
                boundaries.dedup();

                for pair in boundaries.windows(2) {
                    let (lo, hi) = (pair[0], pair[1]);
                    let in_sel = selection_range
                        .as_ref()
                        .is_some_and(|s| lo >= s.start && lo < s.end);
                    let in_inline = in_any_range(lo, &display_inline);

                    let bg = if in_sel {
                        Some(sel_bg)
                    } else if in_inline {
                        inline_bg
                    } else {
                        None
                    };

                    highlight_ranges.push((
                        lo..hi,
                        HighlightStyle {
                            color: Some(fg_color),
                            background_color: bg,
                            ..Default::default()
                        },
                    ));
                }
            }

            StyledText::new(SharedString::from(full_text))
                .with_highlights(highlight_ranges)
                .into_any_element()
        }
        _ => {
            // No syntax highlights. Build highlight ranges from selection
            // and inline changes.
            let mut ranges: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

            // Inline change ranges.
            if inline_bg.is_some() && !display_inline.is_empty() {
                for r in &display_inline {
                    let clamped = r.start.min(display_text.len())..r.end.min(display_text.len());
                    if !clamped.is_empty() {
                        ranges.push((
                            clamped,
                            HighlightStyle {
                                background_color: inline_bg,
                                ..Default::default()
                            },
                        ));
                    }
                }
            }

            // Selection range (takes priority. Overwrite overlapping inline).
            if let Some(sel) = selection_range {
                let clamped = sel.start.min(display_text.len())..sel.end.min(display_text.len());
                if !clamped.is_empty() {
                    // Remove any inline ranges that overlap and re-add split.
                    let sel_style = HighlightStyle {
                        background_color: Some(sel_bg),
                        ..Default::default()
                    };
                    let mut merged = Vec::new();
                    for (r, style) in ranges.drain(..) {
                        if r.end <= clamped.start || r.start >= clamped.end {
                            // No overlap.
                            merged.push((r, style));
                        } else {
                            // Split around selection.
                            if r.start < clamped.start {
                                merged.push((r.start..clamped.start, style));
                            }
                            if r.end > clamped.end {
                                merged.push((clamped.end..r.end, style));
                            }
                        }
                    }
                    merged.push((clamped, sel_style));
                    merged.sort_by_key(|(r, _)| r.start);
                    ranges = merged;
                }
            }

            if ranges.is_empty() {
                SharedString::from(display_text).into_any_element()
            } else {
                StyledText::new(SharedString::from(display_text))
                    .with_highlights(ranges)
                    .into_any_element()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tree builder
// ---------------------------------------------------------------------------

pub(crate) fn build_diff_tree(files: &[ChangedFile]) -> Vec<DiffTreeNode> {
    let mut builder = DirectoryBuilder::default();
    for file in files {
        builder.insert(file.clone());
    }
    builder.build(Path::new(""))
}

/// Recursively collect all file-level DiffSelectionKey entries from a tree.
pub(crate) fn collect_file_keys(
    nodes: &[DiffTreeNode],
    section: DiffSectionKind,
    out: &mut Vec<DiffSelectionKey>,
) {
    for node in nodes {
        match &node.kind {
            DiffTreeNodeKind::Directory => {
                collect_file_keys(&node.children, section, out);
            }
            DiffTreeNodeKind::File(file) => {
                out.push(DiffSelectionKey {
                    section,
                    relative_path: file.relative_path.clone(),
                });
            }
        }
    }
}

pub(crate) fn line_cache_matches(
    cached_selection: &DiffSelectionKey,
    cached_generation: u64,
    selection: &DiffSelectionKey,
    generation: u64,
) -> bool {
    cached_generation == generation && cached_selection == selection
}

pub(crate) fn stash_line_cache_key(
    view_mode: DiffTabViewMode,
    selection: Option<&StashFileSelection>,
) -> Option<StashFileSelection> {
    if view_mode != DiffTabViewMode::Stashes {
        return None;
    }
    selection.cloned()
}

pub(crate) fn stash_line_cache_matches(
    cached_selection: &StashFileSelection,
    cached_theme_id: ThemeId,
    selection: &StashFileSelection,
    theme_id: ThemeId,
) -> bool {
    cached_theme_id == theme_id && cached_selection == selection
}

pub(crate) fn stash_header_button_label(view_mode: DiffTabViewMode, stash_count: usize) -> String {
    match view_mode {
        DiffTabViewMode::WorkingTree => format!("Stashes ({stash_count})"),
        DiffTabViewMode::Stashes => "Back to Diff".to_string(),
    }
}

pub(crate) fn stash_display_title(message: &str, label: &str) -> String {
    let trimmed = message.trim();
    let cleaned = trimmed
        .split_once(':')
        .and_then(|(prefix, remainder)| {
            if prefix.starts_with("WIP on ") || prefix.starts_with("On ") {
                Some(remainder.trim())
            } else {
                None
            }
        })
        .unwrap_or(trimmed);
    if cleaned.is_empty() {
        label.to_string()
    } else {
        cleaned.to_string()
    }
}

pub(crate) fn format_relative_time(timestamp_unix: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    format_relative_time_from(now, timestamp_unix)
}

pub(crate) fn format_relative_time_from(now_unix: i64, timestamp_unix: i64) -> String {
    let delta = (now_unix - timestamp_unix).max(0);
    if delta < 60 {
        "now".to_string()
    } else if delta < 60 * 60 {
        format!("{}m", delta / 60)
    } else if delta < 60 * 60 * 24 {
        format!("{}h", delta / (60 * 60))
    } else if delta < 60 * 60 * 24 * 30 {
        format!("{}d", delta / (60 * 60 * 24))
    } else if delta < 60 * 60 * 24 * 365 {
        format!("{}mo", delta / (60 * 60 * 24 * 30))
    } else {
        format!("{}y", delta / (60 * 60 * 24 * 365))
    }
}

pub(crate) fn build_stash_line_cache(
    document: &StashFileDiffDocument,
    theme_id: ThemeId,
) -> CachedStashDiffLines {
    let lines = document
        .lines
        .iter()
        .filter(|line| line.kind != DiffLineKind::FileHeader)
        .cloned()
        .collect::<Vec<_>>();
    let max_line_chars = lines
        .iter()
        .map(|line| plain_text_len(&line.text, line.highlights.as_deref()))
        .max()
        .unwrap_or(0);
    let is_oversize = lines.len() == 1
        && lines[0].kind == DiffLineKind::BinaryNotice
        && lines[0].text == OVERSIZE_DIFF_MESSAGE;

    CachedStashDiffLines {
        selection: document.selection.clone(),
        theme_id,
        lines: Rc::new(lines),
        max_line_chars,
        is_oversize,
    }
}

/// Simple non-cryptographic hash for generating stable element IDs.
pub(crate) fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h = h.wrapping_mul(0x100000001b3);
        h ^= b as u64;
    }
    h
}

impl DirectoryBuilder {
    fn insert(&mut self, file: ChangedFile) {
        let mut components = file.relative_path.components().peekable();
        let mut current = self;
        while let Some(component) = components.next() {
            let name = component.as_os_str().to_string_lossy().to_string();
            if components.peek().is_some() {
                current = current.directories.entry(name).or_default();
            } else {
                current.files.push(file.clone());
            }
        }
    }

    fn build(self, base: &Path) -> Vec<DiffTreeNode> {
        let mut nodes = Vec::new();
        for (name, directory) in self.directories {
            let path = base.join(&name);
            nodes.push(DiffTreeNode {
                name,
                path: path.clone(),
                kind: DiffTreeNodeKind::Directory,
                children: directory.build(&path),
            });
        }

        let mut files = self.files;
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        for file in files {
            let name = file
                .relative_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| file.relative_path.display().to_string());
            nodes.push(DiffTreeNode {
                name,
                path: file.relative_path.clone(),
                kind: DiffTreeNodeKind::File(file),
                children: Vec::new(),
            });
        }
        nodes
    }
}

pub(crate) fn is_oversize_document(document: &orcashell_git::FileDiffDocument) -> bool {
    document.lines.len() == 1
        && document.lines[0].kind == DiffLineKind::BinaryNotice
        && document.lines[0].text == OVERSIZE_DIFF_MESSAGE
}

pub(crate) fn is_copy_keystroke(key: &str, modifiers: &Modifiers) -> bool {
    key == "c" && (modifiers.platform || modifiers.control)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
