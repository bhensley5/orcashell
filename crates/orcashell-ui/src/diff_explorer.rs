use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use orcashell_git::{
    ChangedFile, DiffLineKind, DiffLineView, DiffSectionKind, DiffSelectionKey, GitFileStatus,
    GitTrackingStatus, HighlightedSpan, OVERSIZE_DIFF_MESSAGE,
};
use orcashell_terminal_view::{Copy, TextInputState};

use crate::app_view::ContextMenuRequest;
use crate::context_menu::ContextMenuItem;
use crate::theme::{self, OrcaTheme};
use crate::workspace::{
    ActionBanner, ActionBannerKind, DiffTabState, ManagedWorktreeSummary, RemoveWorktreeConfirm,
    WorkspaceState,
};

const MIN_TREE_WIDTH: f32 = 180.0;
const MIN_DIFF_WIDTH: f32 = 320.0;

/// Height of each diff row: 20px min-height + 2×1px vertical padding.
const LINE_HEIGHT: f32 = 22.0;

/// Scrollbar hit-zone width (invisible track area that accepts clicks).
const SCROLLBAR_HIT_WIDTH: f32 = 12.0;
/// Visible scrollbar thumb width.
const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
/// Minimum scrollbar thumb height so it remains clickable.
const SCROLLBAR_THUMB_MIN: f32 = 20.0;
/// Inset from the right edge of the track to the thumb center.
const SCROLLBAR_THUMB_INSET: f32 = 3.0;

/// Width of each line-number gutter column.
const GUTTER_WIDTH: f32 = 52.0;
/// Gap between gutter columns and between the last gutter and the text.
const GUTTER_GAP: f32 = 8.0;
/// Horizontal padding on each diff row.
const LINE_PAD_X: f32 = 12.0;
/// X offset where the text column begins, relative to the row's leading edge.
const TEXT_COL_X: f32 = LINE_PAD_X + GUTTER_WIDTH + GUTTER_GAP + GUTTER_WIDTH + GUTTER_GAP;

/// Fallback advance width per character before font measurement completes.
const DEFAULT_CHAR_WIDTH: f32 = 6.6;

/// Font family used in the diff view.
const DIFF_FONT_FAMILY: &str = "JetBrains Mono";
/// Font size used in the diff view.
const DIFF_FONT_SIZE: f32 = 11.0;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DiffResizeDrag {
    initial_mouse_x: f32,
    initial_width: f32,
}

#[derive(Debug, Clone, Copy)]
struct ScrollbarDrag {
    start_y: f32,
    start_scroll_y: f32,
}

/// Cached filtered diff lines for the currently displayed file. Rebuilt only
/// when the selected diff document or git generation changes. Not on every
/// scroll/drag frame.
struct CachedDiffLines {
    selection: DiffSelectionKey,
    generation: u64,
    lines: Rc<Vec<DiffLineView>>,
    /// Longest line in characters (not pixels).  Pixel width is derived at
    /// render time via `measured_char_width` so font/DPI changes don't stale
    /// the cache.
    max_line_chars: usize,
}

/// Cached file tree for the current diff index generation. Rebuilt only when
/// the index generation changes, not on every drag/scroll frame.
struct CachedIndexTree {
    generation: Option<u64>,
    staged_tree: Vec<DiffTreeNode>,
    unstaged_tree: Vec<DiffTreeNode>,
    visible_file_order: Rc<Vec<DiffSelectionKey>>,
}

/// Lightweight snapshot of `DiffTabState` fields needed for rendering.
/// Extracted via a workspace borrow. No `FileDiffDocument` clone.
struct DiffRenderSnapshot {
    tree_width: f32,
    selected_file: Option<DiffSelectionKey>,
    // Index state.
    index_loading: bool,
    index_error: Option<String>,
    has_index: bool,
    index_file_count: usize,
    index_branch: Option<String>,
    index_generation: Option<u64>,
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
}

impl DiffRenderSnapshot {
    fn from_tab(tab: &DiffTabState) -> Self {
        let index_doc = tab.index.document.as_ref();
        let file_doc = tab.file.document.as_ref();
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
            selected_file: tab.selected_file.clone(),
            index_loading: tab.index.loading,
            index_error: tab.index.error.clone(),
            has_index: index_doc.is_some(),
            index_file_count: index_doc
                .map_or(0, |d| d.staged_files.len() + d.unstaged_files.len()),
            index_branch: index_doc.map(|d| d.snapshot.branch_name.clone()),
            index_generation: index_doc.map(|d| d.snapshot.generation),
            file_loading: tab.file.loading,
            file_error: tab.file.error.clone(),
            file_meta: file_doc.map(|d| d.file.clone()),
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
        }
    }

    fn any_action_in_flight(&self) -> bool {
        self.local_action_in_flight || self.remote_op_in_flight
    }
}

/// A text selection inside the diff pane, expressed in (line_index, char_index)
/// coordinates.  `start` is the anchor (where the mouse went down) and `end`
/// tracks the current pointer position.  The two may be in any order; call
/// [`DiffSelection::normalized`] to get (min, max).
#[derive(Debug, Clone, Copy)]
struct DiffSelection {
    start: (usize, usize),
    end: (usize, usize),
    is_selecting: bool,
}

impl DiffSelection {
    /// Return (start, end) in ascending (line, col) order.
    fn normalized(self) -> ((usize, usize), (usize, usize)) {
        let (a, b) = (self.start, self.end);
        if a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1) {
            (a, b)
        } else {
            (b, a)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffTreeNode {
    name: String,
    path: PathBuf,
    kind: DiffTreeNodeKind,
    children: Vec<DiffTreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffTreeNodeKind {
    Directory,
    File(ChangedFile),
}

#[derive(Default)]
struct DirectoryBuilder {
    directories: BTreeMap<String, DirectoryBuilder>,
    files: Vec<ChangedFile>,
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
    /// Generation-keyed cache of the diff index tree. Rebuilt only when the
    /// index generation changes. Not on every drag/scroll frame.
    tree_cache: Option<CachedIndexTree>,

    // CP3: context menu and commit input.
    menu_request: ContextMenuRequest,
    commit_input: Option<Entity<TextInputState>>,
}

impl DiffExplorerView {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        scope_root: PathBuf,
        menu_request: ContextMenuRequest,
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
            tree_cache: None,
            menu_request,
            commit_input: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Measure the actual character advance width using the GPUI text system.
    /// Called once per render; the result is cached until the next render.
    fn measure_char_width(&mut self, window: &mut Window) {
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
        let shaped =
            window
                .text_system()
                .shape_line("M".into(), px(DIFF_FONT_SIZE), &[text_run], None);
        let w: f32 = shaped.width.into();
        if w > 0.0 {
            self.measured_char_width = w;
        }
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

        let max_line_chars = lines
            .iter()
            .map(|l| plain_text_len(&l.text, l.highlights.as_deref()))
            .max()
            .unwrap_or(0);

        self.line_cache = Some(CachedDiffLines {
            selection: selection.clone(),
            generation: gen,
            lines: Rc::new(lines),
            max_line_chars,
        });
    }

    /// Rebuild the cached diff tree if the index generation changed.
    fn update_tree_cache(&mut self, cx: &App, snap: &DiffRenderSnapshot) {
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

        let (staged_tree, unstaged_tree) = tab
            .index
            .document
            .as_ref()
            .map(|document| {
                let staged = build_diff_tree(&document.staged_files);
                let unstaged = build_diff_tree(&document.unstaged_files);
                (staged, unstaged)
            })
            .unwrap_or_default();

        // Build visible file order: staged files first, then unstaged.
        let mut visible_file_order = Vec::new();
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
                    .child(branch_name),
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
        let mut actions = div().flex().items_center().gap(px(4.0));

        if let Some(confirm) = &snap.remove_worktree_confirm {
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
            // Header buttons: Pull, Push, Merge, Remove - always visible,
            // dimmed when not applicable.

            // Pull button. Disabled when not behind.
            let pull_disabled = any_in_flight || snap.tracking_behind == 0;
            let pull_label = if snap.tracking_behind > 0 {
                format!("Pull ({})", snap.tracking_behind)
            } else {
                "Pull".to_string()
            };
            let ws_pull = self.workspace.clone();
            let scope_pull = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                "diff-action-pull",
                &pull_label,
                pull_disabled,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_pull.update(cx, |ws, cx| {
                        ws.dispatch_pull(&scope_pull, cx);
                    });
                },
            ));

            // Push button. Disabled when in-flight.
            let ws_push = self.workspace.clone();
            let scope_push = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
                "diff-action-push",
                "Push",
                any_in_flight,
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws_push.update(cx, |ws, cx| {
                        ws.dispatch_push(&scope_push, cx);
                    });
                },
            ));

            // Merge button. Disabled when no managed worktree.
            let merge_disabled = any_in_flight || snap.managed_worktree.is_none();
            let ws_merge = self.workspace.clone();
            let scope_merge = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
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
            let remove_disabled = any_in_flight || snap.managed_worktree.is_none();
            let ws_remove = self.workspace.clone();
            let scope_remove = scope_root.to_path_buf();
            actions = actions.child(Self::action_bar_button(
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

        let _ = cx; // used only through workspace.clone() above

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

    /// A muted, professional toolbar button with subtle blue tint.
    /// Used in both the tree-pane action bar and header action buttons.
    fn action_bar_button(
        id: &str,
        label: &str,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let palette = theme::current();
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
                .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x0C)))
                .border_color(rgb(palette.BORDER_EMPHASIS))
                .text_color(rgb(palette.FOG))
                .cursor_pointer()
                .hover(|s| {
                    s.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                        .text_color(rgb(palette.BONE))
                })
                .on_click(on_click);
        }

        btn.child(label.to_string())
    }

    // ------------------------------------------------------------------
    // Action banner
    // ------------------------------------------------------------------

    fn render_action_banner(&self, snap: &DiffRenderSnapshot) -> Option<Div> {
        let palette = theme::current();
        let banner = snap.last_action_banner.as_ref()?;
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
                        .hover(|s| s.text_color(rgb(palette.PATCH)))
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
                "Loading diff index...",
                Some("Git is collecting the changed-file tree for this scope."),
            ));
        }
        if let Some(error) = snap.index_error.as_ref() {
            if !snap.has_index {
                return pane.child(Self::empty_panel_message(
                    "Could not load diff index",
                    Some(error.as_str()),
                ));
            }
        }

        if !snap.has_index {
            return pane.child(Self::empty_panel_message(
                "Diff index unavailable",
                Some("Open the diff tab from a git-backed terminal row."),
            ));
        }

        if snap.index_file_count == 0 {
            return pane.child(Self::empty_panel_message(
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

        // Action bar: Stage All, Commit, Push, Merge - always visible.
        {
            let scope = self.scope_root.clone();
            list = list.child(self.render_action_bar(&scope, snap, cx));
        }

        // Commit input + "STAGED CHANGES" section.
        if snap.staged_file_count > 0 {
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

            // "STAGED CHANGES" section header.
            list = list.child(Self::section_header(
                "STAGED CHANGES",
                snap.staged_file_count,
            ));

            // Staged tree.
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
            ));
        }

        // "CHANGES" section header (always shown).
        list = list.child(Self::section_header("CHANGES", snap.unstaged_file_count));

        // Unstaged tree.
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
        ));

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

    /// Render a section header row (e.g. "STAGED CHANGES (3)").
    fn section_header(label: &str, count: usize) -> Div {
        let palette = theme::current();
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

    fn render_section_tree(
        &self,
        nodes: &[DiffTreeNode],
        depth: usize,
        section: DiffSectionKind,
        snap: &DiffRenderSnapshot,
        visible_order: &Rc<Vec<DiffSelectionKey>>,
        scope_hash: u64,
    ) -> Vec<AnyElement> {
        let palette = theme::current();
        let mut rows = Vec::new();
        for node in nodes {
            match &node.kind {
                DiffTreeNodeKind::Directory => {
                    rows.push(
                        div()
                            .w_full()
                            .px(px(8.0))
                            .py(px(4.0))
                            .pl(px(8.0 + depth as f32 * 14.0))
                            .text_size(px(11.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.FOG))
                            .child(format!("\u{25BE} {}", node.name))
                            .into_any_element(),
                    );
                    rows.extend(self.render_section_tree(
                        &node.children,
                        depth + 1,
                        section,
                        snap,
                        visible_order,
                        scope_hash,
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
                                .hover(|row| row.bg(rgba(theme::with_alpha(palette.CURRENT, 0x88))))
                        })
                        .child(Self::status_pill(file.status))
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
                                .child(node.name.clone()),
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
                                    ws.diff_replace_select(
                                        &scope_for_select,
                                        key_select.clone(),
                                        cx,
                                    );
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
                                let (label, action_section) = match ctx_section {
                                    DiffSectionKind::Staged => ("Unstage", DiffSectionKind::Staged),
                                    DiffSectionKind::Unstaged => {
                                        ("Stage", DiffSectionKind::Unstaged)
                                    }
                                };
                                let items = vec![ContextMenuItem {
                                    label: label.to_string(),
                                    shortcut: None,
                                    enabled: !in_flight,
                                    action: Box::new(move |ws_state, cx| match action_section {
                                        DiffSectionKind::Staged => {
                                            ws_state.unstage_selected(&scope, cx);
                                        }
                                        DiffSectionKind::Unstaged => {
                                            ws_state.stage_selected(&scope, cx);
                                        }
                                    }),
                                }];
                                *menu_request.borrow_mut() = Some((event.position, items));
                                cx.stop_propagation();
                            },
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
                "Loading diff...",
                Some("The changed-file tree is still loading."),
            ));
        }
        if let Some(error) = snap.index_error.as_ref() {
            if !snap.has_index {
                return bare_pane().child(Self::empty_panel_message(
                    "Could not load diff",
                    Some(error.as_str()),
                ));
            }
        }

        if !snap.has_index {
            return bare_pane().child(Self::empty_panel_message(
                "No diff loaded",
                Some("Select a git-backed terminal row and open its diff explorer."),
            ));
        }

        if snap.index_file_count == 0 {
            return bare_pane().child(Self::empty_panel_message(
                "Working tree clean",
                Some("There are no file changes to render."),
            ));
        }

        let selected_path = snap.selected_file.as_ref().map(|k| &k.relative_path);

        let Some(selected_path) = selected_path else {
            return bare_pane().child(Self::empty_panel_message(
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        if snap.file_loading
            && snap
                .file_meta
                .as_ref()
                .is_none_or(|meta| meta.relative_path != *selected_path)
        {
            return bare_pane().child(Self::empty_panel_message(
                "Loading file diff...",
                Some("The selected file patch is still hydrating."),
            ));
        }

        if let Some(error) = snap.file_error.as_ref() {
            if snap.file_meta.is_none() {
                return bare_pane().child(Self::empty_panel_message(
                    "Could not load file diff",
                    Some(error.as_str()),
                ));
            }
        }

        let Some(file_meta) = snap.file_meta.as_ref() else {
            return bare_pane().child(Self::empty_panel_message(
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        if snap.is_oversize {
            return bare_pane().child(
                div()
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.PATCH))
                            .child(file_meta.relative_path.display().to_string()),
                    )
                    .child(Self::status_summary(file_meta))
                    .child(Self::empty_panel_message(
                        OVERSIZE_DIFF_MESSAGE,
                        Some("Open the file in another tool if you need the full patch."),
                    )),
            );
        }

        // ---- Normal content: file header + virtualized line list ----

        // Use cached lines (Rc clone = refcount bump on cache hit).
        let Some(cached) = self.line_cache.as_ref() else {
            return bare_pane().child(Self::empty_panel_message(
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };
        let lines = cached.lines.clone();
        let line_count = lines.len();
        let selection = self.selection;
        let scroll_x = self.diff_scroll_x;
        let line_palette = palette.clone();

        // Fixed file header bar above the line list.
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
            .child(Self::status_summary(file_meta));

        if let Some(error) = snap.file_error.as_ref() {
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
                    .child(error.clone()),
            );
        }

        // The virtualized line list.
        let diff_list = uniform_list("diff-lines", line_count, move |range, _window, _cx| {
            range
                .map(|ix| {
                    render_diff_line_element(
                        &line_palette,
                        &lines[ix],
                        ix,
                        selection.as_ref(),
                        scroll_x,
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
                    .map(|c| c.max_line_chars as f32 * this.measured_char_width + TEXT_COL_X)
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
                self.render_scrollbar(thumb_y, thumb_height, is_dragging, cx)
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
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let palette = theme::current();
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
                    .hover(|s| s.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0xA0)))),
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

    fn status_summary(file: &ChangedFile) -> Div {
        let palette = theme::current();
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(Self::status_pill(file.status))
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

    fn status_pill(status: GitFileStatus) -> Div {
        let palette = theme::current();
        let (label, border, text) = match status {
            GitFileStatus::Added => ("A", palette.STATUS_GREEN, palette.STATUS_GREEN),
            GitFileStatus::Modified => ("M", palette.ORCA_BLUE, palette.ORCA_BLUE),
            GitFileStatus::Deleted => ("D", palette.STATUS_CORAL, palette.STATUS_CORAL),
            GitFileStatus::Renamed => ("R", palette.SEAFOAM, palette.SEAFOAM),
            GitFileStatus::Typechange => ("T", palette.FOG, palette.FOG),
            GitFileStatus::Untracked => ("?", palette.STATUS_AMBER, palette.STATUS_AMBER),
            GitFileStatus::Conflicted => ("U", palette.STATUS_AMBER, palette.STATUS_AMBER),
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

    fn empty_panel_message(title: &str, body: Option<&str>) -> Div {
        let palette = theme::current();
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
                    "Diff tab closed",
                    Some("Re-open the diff explorer from a git-backed terminal row."),
                ));
        };

        // Update caches (O(1) on hit, O(n) on miss).
        self.update_tree_cache(cx, &snap);
        self.update_line_cache(cx, &snap);

        // Drop commit input if no staged files.
        if snap.staged_file_count == 0 {
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
                if is_copy_keystroke(&event.keystroke.key, &event.keystroke.modifiers) {
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
        if let Some(banner) = self.render_action_banner(&snap) {
            root = root.child(banner);
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
fn render_diff_line_element(
    palette: &OrcaTheme,
    line: &DiffLineView,
    line_index: usize,
    selection: Option<&DiffSelection>,
    scroll_x: f32,
) -> AnyElement {
    let (background, text_color, gutter) = diff_line_colors(palette, line.kind);

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

    row = row
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
        .child(
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
            ),
        );

    row.into_any_element()
}

/// Return (background, text_color, gutter_color) for a given diff-line kind.
fn diff_line_colors(palette: &OrcaTheme, kind: DiffLineKind) -> (Option<u32>, u32, u32) {
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
        DiffLineKind::Context => (None, palette.BONE, palette.FOG),
    }
}

/// Return the inline change background color for a given line kind, if applicable.
fn inline_change_bg(palette: &OrcaTheme, kind: DiffLineKind) -> Option<Hsla> {
    match kind {
        DiffLineKind::Addition => Some(rgba(theme::with_alpha(palette.STATUS_GREEN, 0x2E)).into()),
        DiffLineKind::Deletion => Some(rgba(theme::with_alpha(palette.STATUS_CORAL, 0x2E)).into()),
        _ => None,
    }
}

/// Compute the byte range within the line's display text that is selected,
/// or `None` if this line is outside the selection.
fn selection_range_for_line(
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
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Get the display text length in characters for a diff line.
fn plain_text_len(raw: &str, highlights: Option<&[HighlightedSpan]>) -> usize {
    plain_text_for_line(raw, highlights).chars().count()
}

/// Get the plain text content for a diff line (joining highlighted spans if
/// present, otherwise using the raw text with whitespace normalization).
fn plain_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
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
fn display_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
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
fn clipboard_text_for_line(raw: &str, highlights: Option<&[HighlightedSpan]>) -> String {
    plain_text_for_line(raw, highlights).replace('\u{00A0}', " ")
}

/// Extract the selected text from diff lines.
fn extract_selected_text(
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

fn render_diff_text(text: &str) -> SharedString {
    SharedString::from(render_diff_text_string(text))
}

/// Like `render_diff_text` but returns an owned `String`.
fn render_diff_text_string(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    text.replace('\t', "\u{00A0}\u{00A0}\u{00A0}\u{00A0}")
        .replace(' ', "\u{00A0}")
}

/// Map byte ranges from raw diff text to display text byte offsets.
///
/// Raw text has single-byte spaces and tabs; display text replaces them with
/// multi-byte NBSP (`\u{00A0}`, 2 bytes each) and tab→4 NBSPs (8 bytes).
/// Trailing `\r`/`\n` are stripped.
fn map_raw_to_display_ranges(raw: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
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

    for (raw_pos, byte) in trimmed.bytes().enumerate() {
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
        let display_len = match byte {
            b'\t' => 8, // 4 NBSPs × 2 bytes each
            b' ' => 2,  // 1 NBSP × 2 bytes
            _ => 1,
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

/// Check if a display byte offset falls inside any of the given display ranges.
fn in_any_range(pos: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|r| pos >= r.start && pos < r.end)
}

/// Render diff line text with optional syntax highlighting and selection.
///
/// GPUI's `StyledText::with_highlights` requires **non-overlapping, sorted**
/// ranges.  When a selection intersects syntax spans, we split each span at
/// the selection boundaries so that every sub-range carries the correct
/// composite style (foreground from syntax + background from selection) and
/// no two ranges overlap.
fn render_highlighted_text(
    palette: &OrcaTheme,
    text: &str,
    highlights: Option<&[HighlightedSpan]>,
    selection_range: Option<Range<usize>>,
    inline_changes: Option<&[Range<usize>]>,
    inline_bg: Option<Hsla>,
) -> AnyElement {
    let sel_bg: Hsla = rgba(theme::with_alpha(palette.ORCA_BLUE, 0x40)).into();

    // Map inline change ranges from raw text bytes to display text bytes.
    let display_inline: Vec<Range<usize>> = match (inline_changes, inline_bg) {
        (Some(ranges), Some(_)) if !ranges.is_empty() => map_raw_to_display_ranges(text, ranges),
        _ => vec![],
    };

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
            let display = render_diff_text(text);
            let mut ranges: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

            // Inline change ranges.
            if inline_bg.is_some() && !display_inline.is_empty() {
                for r in &display_inline {
                    let clamped = r.start.min(display.len())..r.end.min(display.len());
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
                let clamped = sel.start.min(display.len())..sel.end.min(display.len());
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
                display.into_any_element()
            } else {
                StyledText::new(display)
                    .with_highlights(ranges)
                    .into_any_element()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tree builder
// ---------------------------------------------------------------------------

fn build_diff_tree(files: &[ChangedFile]) -> Vec<DiffTreeNode> {
    let mut builder = DirectoryBuilder::default();
    for file in files {
        builder.insert(file.clone());
    }
    builder.build(Path::new(""))
}

/// Recursively collect all file-level DiffSelectionKey entries from a tree.
fn collect_file_keys(
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

fn line_cache_matches(
    cached_selection: &DiffSelectionKey,
    cached_generation: u64,
    selection: &DiffSelectionKey,
    generation: u64,
) -> bool {
    cached_generation == generation && cached_selection == selection
}

/// Simple non-cryptographic hash for generating stable element IDs.
fn simple_hash(s: &str) -> u64 {
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

fn is_oversize_document(document: &orcashell_git::FileDiffDocument) -> bool {
    document.lines.len() == 1
        && document.lines[0].kind == DiffLineKind::BinaryNotice
        && document.lines[0].text == OVERSIZE_DIFF_MESSAGE
}

fn is_copy_keystroke(key: &str, modifiers: &Modifiers) -> bool {
    key == "c" && (modifiers.platform || modifiers.control)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        build_diff_tree, collect_file_keys, extract_selected_text, is_copy_keystroke,
        is_oversize_document, line_cache_matches, map_raw_to_display_ranges, plain_text_for_line,
        render_diff_text, selection_range_for_line, DiffSelection, DiffTreeNodeKind,
    };
    use gpui::Modifiers;
    use orcashell_git::{
        ChangedFile, DiffLineKind, DiffLineView, DiffSectionKind, DiffSelectionKey,
        FileDiffDocument, GitFileStatus, OVERSIZE_DIFF_MESSAGE,
    };

    fn file(path: &str, status: GitFileStatus) -> ChangedFile {
        ChangedFile {
            relative_path: PathBuf::from(path),
            status,
            is_binary: false,
            insertions: 1,
            deletions: 0,
        }
    }

    fn diff_line(text: &str, kind: DiffLineKind) -> DiffLineView {
        DiffLineView {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.to_string(),
            highlights: None,
            inline_changes: None,
        }
    }

    #[test]
    fn build_diff_tree_groups_directories_before_files() {
        let tree = build_diff_tree(&[
            file("src/lib.rs", GitFileStatus::Modified),
            file("src/app/mod.rs", GitFileStatus::Added),
            file("README.md", GitFileStatus::Modified),
        ]);

        assert_eq!(tree.len(), 2);
        assert!(matches!(tree[0].kind, DiffTreeNodeKind::Directory));
        assert!(matches!(tree[1].kind, DiffTreeNodeKind::File(_)));
        assert_eq!(tree[0].name, "src");
        assert_eq!(tree[1].name, "README.md");
    }

    #[test]
    fn render_diff_text_preserves_spaces() {
        assert_eq!(
            render_diff_text("a b\tc").as_ref(),
            "a\u{00A0}b\u{00A0}\u{00A0}\u{00A0}\u{00A0}c"
        );
    }

    #[test]
    fn render_diff_text_trims_trailing_newlines() {
        assert_eq!(render_diff_text("line\n").as_ref(), "line");
        assert_eq!(render_diff_text("line\r\n").as_ref(), "line");
    }

    #[test]
    fn map_raw_to_display_ranges_with_spaces_and_tabs() {
        // "a b\tc\n" → display: "a" NBSP "b" NBSP×4 "c"
        // raw bytes:  a=0, ' '=1, b=2, '\t'=3, c=4, '\n'=5
        // display:    a=0, NBSP=1..3, b=3, NBSP×4=4..12, c=12
        let raw = "a b\tc\n";
        let ranges = vec![2..5]; // raw "b\tc"
        let mapped = map_raw_to_display_ranges(raw, &ranges);
        // display "b" starts at 3, display "c" ends at 13
        assert_eq!(mapped, vec![3..13]);
    }

    #[test]
    fn map_raw_to_display_ranges_empty() {
        let mapped = map_raw_to_display_ranges("hello\n", &[]);
        assert!(mapped.is_empty());
    }

    #[test]
    fn oversize_detection_matches_guard_document() {
        let document = FileDiffDocument {
            generation: 3,
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("Cargo.lock"),
            },
            file: file("Cargo.lock", GitFileStatus::Modified),
            lines: vec![DiffLineView {
                kind: DiffLineKind::BinaryNotice,
                old_lineno: None,
                new_lineno: None,
                text: OVERSIZE_DIFF_MESSAGE.to_string(),
                highlights: None,
                inline_changes: None,
            }],
        };

        assert!(is_oversize_document(&document));
    }

    #[test]
    fn plain_text_for_line_joins_spans() {
        use orcashell_git::HighlightedSpan;
        let spans = vec![
            HighlightedSpan {
                text: "fn ".to_string(),
                color: 0xFF0000,
            },
            HighlightedSpan {
                text: "main".to_string(),
                color: 0x00FF00,
            },
        ];
        assert_eq!(plain_text_for_line("fn main", Some(&spans)), "fn main");
    }

    #[test]
    fn plain_text_for_line_normalizes_whitespace() {
        assert_eq!(plain_text_for_line("a\tb\n", None), "a    b");
    }

    #[test]
    fn selection_range_single_line() {
        let line = diff_line("hello world", DiffLineKind::Addition);
        let sel = DiffSelection {
            start: (0, 0),
            end: (0, 5),
            is_selecting: false,
        };
        let range = selection_range_for_line(Some(&sel), 0, &line);
        assert!(range.is_some());
    }

    #[test]
    fn selection_range_outside_line() {
        let line = diff_line("hello", DiffLineKind::Context);
        let sel = DiffSelection {
            start: (2, 0),
            end: (3, 5),
            is_selecting: false,
        };
        assert!(selection_range_for_line(Some(&sel), 0, &line).is_none());
    }

    #[test]
    fn extract_selected_text_single_line() {
        let line = diff_line("hello world", DiffLineKind::Addition);
        let lines: Vec<&DiffLineView> = vec![&line];
        let text = extract_selected_text(&lines, 0, 6, 0, 11);
        assert_eq!(text, Some("world".to_string()));
    }

    #[test]
    fn extract_selected_text_multi_line() {
        let l0 = diff_line("first line", DiffLineKind::Context);
        let l1 = diff_line("second line", DiffLineKind::Addition);
        let l2 = diff_line("third line", DiffLineKind::Context);
        let lines: Vec<&DiffLineView> = vec![&l0, &l1, &l2];
        let text = extract_selected_text(&lines, 0, 6, 2, 5);
        assert_eq!(text, Some("line\nsecond line\nthird".to_string()));
    }

    #[test]
    fn extract_selected_text_normalizes_nbsp_in_highlighted_spans() {
        use orcashell_git::HighlightedSpan;
        let line = DiffLineView {
            kind: DiffLineKind::Context,
            old_lineno: Some(1),
            new_lineno: Some(1),
            text: "fn main() {".to_string(),
            highlights: Some(vec![
                HighlightedSpan {
                    text: "fn\u{00A0}".to_string(),
                    color: 0xFF0000,
                },
                HighlightedSpan {
                    text: "main()\u{00A0}{".to_string(),
                    color: 0x00FF00,
                },
            ]),
            inline_changes: None,
        };
        let lines: Vec<&DiffLineView> = vec![&line];
        let text = extract_selected_text(&lines, 0, 0, 0, 11);
        // Clipboard text must have regular spaces, not NBSPs.
        assert_eq!(text, Some("fn main() {".to_string()));
    }

    #[test]
    fn collect_file_keys_from_tree() {
        let tree = build_diff_tree(&[
            file("src/lib.rs", GitFileStatus::Modified),
            file("src/app/mod.rs", GitFileStatus::Added),
            file("README.md", GitFileStatus::Modified),
        ]);
        let mut keys = Vec::new();
        collect_file_keys(&tree, DiffSectionKind::Unstaged, &mut keys);
        assert_eq!(keys.len(), 3);
        assert_eq!(
            keys[0],
            DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("src/app/mod.rs"),
            }
        );
        assert_eq!(
            keys[1],
            DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("src/lib.rs"),
            }
        );
        assert_eq!(
            keys[2],
            DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: PathBuf::from("README.md"),
            }
        );
    }

    #[test]
    fn line_cache_match_requires_section_aware_selection() {
        let staged = DiffSelectionKey {
            section: DiffSectionKind::Staged,
            relative_path: PathBuf::from("src/lib.rs"),
        };
        let unstaged = DiffSelectionKey {
            section: DiffSectionKind::Unstaged,
            relative_path: PathBuf::from("src/lib.rs"),
        };

        assert!(line_cache_matches(&staged, 7, &staged, 7));
        assert!(!line_cache_matches(&staged, 7, &unstaged, 7));
    }

    #[test]
    fn copy_keystroke_matches_platform_and_control_c() {
        assert!(is_copy_keystroke(
            "c",
            &Modifiers {
                platform: true,
                ..Default::default()
            }
        ));
        assert!(is_copy_keystroke(
            "c",
            &Modifiers {
                control: true,
                ..Default::default()
            }
        ));
        assert!(!is_copy_keystroke("c", &Modifiers::default()));
        assert!(!is_copy_keystroke(
            "x",
            &Modifiers {
                platform: true,
                ..Default::default()
            }
        ));
    }
}
