use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use orcashell_git::{
    CommitChangedFile, CommitFileDiffDocument, CommitFileSelection, CommitFileStatus,
    CommitGraphNode, CommitRefKind, DiffLineKind, DiffLineView, DiffSectionKind, DiffSelectionKey,
    GitSnapshotSummary, GraphLaneKind, HeadState, Oid, RepositoryGraphDocument,
};

use crate::app_view::ContextMenuRequest;
use crate::context_menu::ContextMenuItem;
use crate::diff_explorer::{
    max_diff_content_width, measure_diff_char_width, plain_text_len, render_diff_line_element,
    CachedDiffLines, ScrollbarDrag, DEFAULT_CHAR_WIDTH, DIFF_FONT_FAMILY, SCROLLBAR_HIT_WIDTH,
    SCROLLBAR_THUMB_INSET, SCROLLBAR_THUMB_MIN, SCROLLBAR_THUMB_WIDTH,
};
use crate::prompt_dialog::{
    PromptDialogConfirmTone, PromptDialogInputSpec, PromptDialogRequest, PromptDialogSpec,
};
use crate::theme::{self, OrcaTheme};
use crate::workspace::{
    ActionBanner, ActionBannerKind, AsyncDocumentState, RepositoryBranchAction,
    RepositoryBranchSelection, RepositoryGraphTabState, WorkspaceState,
};

const BRANCH_ROW_HEIGHT: f32 = 28.0;
const COMMIT_ROW_HEIGHT: f32 = 28.0;
const BRANCH_ROW_HORIZONTAL_PADDING: f32 = 24.0;
const BRANCH_ROW_CHECK_WIDTH: f32 = 14.0;
const BRANCH_ROW_GAP: f32 = 8.0;
const BRANCH_META_INDENT: f32 = 34.0;
const BRANCH_META_TEXT_INDENT: f32 = DEFAULT_CHAR_WIDTH * 2.0;
const BRANCH_BADGE_WIDTH_ESTIMATE: f32 = 32.0;
const BRANCH_LABEL_CHAR_WIDTH: f32 = DEFAULT_CHAR_WIDTH;
const BRANCH_META_CHAR_WIDTH: f32 = DEFAULT_CHAR_WIDTH * 0.85;
const BRANCH_INLINE_FIT_BUFFER: f32 = DEFAULT_CHAR_WIDTH * 4.0;
const GRAPH_LANE_SIZE: f32 = 14.0;
const GRAPH_LANE_GAP: f32 = 3.0;
const GRAPH_COLUMN_WIDTH: f32 = 116.0;
const MAX_VISIBLE_GRAPH_LANES: usize = 6;
const GRAPH_OVERFLOW_DOT_SIZE: f32 = 3.0;
const GRAPH_OVERFLOW_DOT_GAP: f32 = 3.0;
const DEFAULT_REPOSITORY_BRANCH_PANE_WIDTH: f32 = 280.0;
const DEFAULT_REPOSITORY_DETAIL_PANE_WIDTH: f32 = 360.0;
const MIN_REPOSITORY_BRANCH_PANE_WIDTH: f32 = 220.0;
const MIN_REPOSITORY_DETAIL_PANE_WIDTH: f32 = 300.0;
const MIN_REPOSITORY_CENTER_PANE_WIDTH: f32 = 320.0;
const REPOSITORY_PANE_DIVIDER_WIDTH: f32 = 4.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchRailRow {
    SectionHeader { label: String },
    RemoteGroup { remote_name: String, expanded: bool },
    Branch(BranchRailBranchRow),
    BranchMeta(BranchRailMetaRow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRailBranchRow {
    pub selection: RepositoryBranchSelection,
    pub name: String,
    pub kind: BranchRailBranchKind,
    pub inline_upstream: Option<BranchRailUpstreamSummary>,
    pub is_head: bool,
    pub is_worktree_occupied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRailMetaRow {
    pub selection: RepositoryBranchSelection,
    pub upstream: BranchRailUpstreamSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchRailBranchKind {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRailUpstreamSummary {
    pub remote_ref: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRailCacheKey {
    content_hash: u64,
    occupancy_count: usize,
    occupancy_hash: u64,
    expanded_remote_count: usize,
    expanded_remote_hash: u64,
    layout_width_bucket: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitRowsCacheKey {
    content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitFileCacheKey {
    commit_oid: Oid,
    path: PathBuf,
    line_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledCommitRow {
    commit: CommitGraphNode,
    active_lane: Option<u16>,
    is_checked_out_tip: bool,
    is_detached_head_tip: bool,
    visible_lane_ids: Vec<u16>,
    overflow_gutter_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphLaneAccent {
    Active,
    Green,
    Amber,
    Fog,
    Slate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitRowHighlightState {
    Selected,
    CurrentTip,
    HoverOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryCenterPaneMode {
    History,
    CommitFileDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryDetailPaneMode {
    Empty,
    Branch,
    Commit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepositoryPaneSide {
    Branch,
    Detail,
}

#[derive(Debug, Clone, Copy)]
struct RepositoryPaneResizeDrag {
    side: RepositoryPaneSide,
    initial_mouse_x: f32,
    initial_width: f32,
    opposite_width: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchDetailActionState {
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryBranchToolbarState {
    pub checkout_disabled: bool,
    pub create_disabled: bool,
    pub delete_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryPullActionState {
    pub disabled: bool,
    pub label: String,
}

pub struct RepositoryBrowserView {
    workspace: Entity<WorkspaceState>,
    project_id: String,
    menu_request: ContextMenuRequest,
    prompt_dialog_request: PromptDialogRequest,
    bounds: Rc<RefCell<Bounds<Pixels>>>,
    resize_drag: Option<RepositoryPaneResizeDrag>,
    branch_cache_key: Option<BranchRailCacheKey>,
    commit_cache_key: Option<CommitRowsCacheKey>,
    branch_rows: Rc<Vec<BranchRailRow>>,
    commit_rows: Rc<Vec<StyledCommitRow>>,
    diff_scroll_handle: UniformListScrollHandle,
    diff_scrollbar_drag: Option<ScrollbarDrag>,
    diff_list_bounds: Rc<RefCell<Bounds<Pixels>>>,
    diff_scroll_x: f32,
    measured_char_width: f32,
    branch_pane_width: f32,
    detail_pane_width: f32,
    commit_file_cache_key: Option<CommitFileCacheKey>,
    commit_file_line_cache: Option<CachedDiffLines>,
}

impl RepositoryBrowserView {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        project_id: String,
        menu_request: ContextMenuRequest,
        prompt_dialog_request: PromptDialogRequest,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            workspace,
            project_id,
            menu_request,
            prompt_dialog_request,
            bounds: Rc::new(RefCell::new(Bounds::default())),
            resize_drag: None,
            branch_cache_key: None,
            commit_cache_key: None,
            branch_rows: Rc::new(Vec::new()),
            commit_rows: Rc::new(Vec::new()),
            diff_scroll_handle: UniformListScrollHandle::new(),
            diff_scrollbar_drag: None,
            diff_list_bounds: Rc::new(RefCell::new(Bounds::default())),
            diff_scroll_x: 0.0,
            measured_char_width: DEFAULT_CHAR_WIDTH,
            branch_pane_width: DEFAULT_REPOSITORY_BRANCH_PANE_WIDTH,
            detail_pane_width: DEFAULT_REPOSITORY_DETAIL_PANE_WIDTH,
            commit_file_cache_key: None,
            commit_file_line_cache: None,
        };
        this.sync_graph_cache(cx);
        this.sync_commit_file_cache(cx);

        cx.observe(&this.workspace, |this, _workspace, cx| {
            this.sync_graph_cache(cx);
            this.sync_commit_file_cache(cx);
            cx.notify();
        })
        .detach();

        this
    }

    pub fn invalidate_theme_cache(&mut self, cx: &mut Context<Self>) {
        self.commit_file_cache_key = None;
        self.sync_commit_file_cache(cx);
        cx.notify();
    }

    fn clear_graph_cache(&mut self) {
        self.branch_cache_key = None;
        self.commit_cache_key = None;
        self.branch_rows = Rc::new(Vec::new());
        self.commit_rows = Rc::new(Vec::new());
    }

    fn clear_commit_file_cache(&mut self) {
        self.commit_file_cache_key = None;
        self.commit_file_line_cache = None;
        self.diff_scroll_x = 0.0;
    }

    fn sync_graph_cache(&mut self, cx: &App) {
        let ws = self.workspace.read(cx);
        let Some(graph_state) = ws.repository_graph_state(&self.project_id) else {
            self.clear_graph_cache();
            return;
        };
        let Some(graph) = graph_state.graph.document.as_ref() else {
            self.clear_graph_cache();
            return;
        };

        let next_branch_key = branch_rail_cache_key(
            graph,
            &graph_state.occupied_local_branches,
            &graph_state.expanded_remote_groups,
            self.branch_pane_width,
        );
        if self.branch_cache_key.as_ref() != Some(&next_branch_key) {
            let branch_rows = flatten_branch_rail(
                graph,
                &graph_state.occupied_local_branches,
                &graph_state.expanded_remote_groups,
                self.branch_pane_width,
            );
            self.branch_rows = Rc::new(branch_rows);
            self.branch_cache_key = Some(next_branch_key);
        }

        let next_commit_key = commit_rows_cache_key(graph);
        if self.commit_cache_key.as_ref() != Some(&next_commit_key) {
            let commit_rows = style_commit_rows(graph);
            self.commit_rows = Rc::new(commit_rows);
            self.commit_cache_key = Some(next_commit_key);
        }
    }

    fn sync_commit_file_cache(&mut self, cx: &App) {
        let ws = self.workspace.read(cx);
        let Some(state) = ws.repository_graph_state(&self.project_id) else {
            self.clear_commit_file_cache();
            return;
        };
        let Some(document) = state.commit_file_diff.document.as_ref() else {
            self.clear_commit_file_cache();
            return;
        };

        let next_key = CommitFileCacheKey {
            commit_oid: document.commit_oid,
            path: document.selection.relative_path.clone(),
            line_count: document.lines.len(),
        };
        if self.commit_file_cache_key.as_ref() == Some(&next_key) {
            return;
        }

        let lines: Vec<DiffLineView> = document
            .lines
            .iter()
            .filter(|line| line.kind != DiffLineKind::FileHeader)
            .cloned()
            .collect();
        let max_line_chars = lines
            .iter()
            .map(|line| plain_text_len(&line.text, line.highlights.as_deref()))
            .max()
            .unwrap_or(0);
        let relative_path = document.selection.relative_path.clone();

        self.commit_file_line_cache = Some(CachedDiffLines {
            selection: DiffSelectionKey {
                section: DiffSectionKind::Unstaged,
                relative_path: relative_path.clone(),
            },
            generation: 0,
            lines: Rc::new(lines),
            hunk_headers: Rc::new(HashMap::new()),
            max_line_chars,
        });
        self.diff_scroll_x = 0.0;
        self.commit_file_cache_key = Some(CommitFileCacheKey {
            path: relative_path,
            ..next_key
        });
    }

    fn measure_char_width(&mut self, window: &mut Window) {
        self.measured_char_width = measure_diff_char_width(window);
    }

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
            .id(ElementId::Name(
                format!("repository-diff-scrollbar-{}", self.project_id).into(),
            ))
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

                    if local_y >= thumb_y && local_y <= thumb_y + thumb_height {
                        let state = this.diff_scroll_handle.0.borrow();
                        let scroll_y = -f32::from(state.base_handle.offset().y);
                        drop(state);
                        this.diff_scrollbar_drag = Some(ScrollbarDrag {
                            start_y: y,
                            start_scroll_y: scroll_y,
                        });
                    } else {
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

                let Some(item_size) = item_size else {
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

    fn render_header(
        &self,
        project_name: &str,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        fetch_disabled: bool,
        toolbar_busy: bool,
        pull_state: &RepositoryPullActionState,
    ) -> Div {
        let (graph_meta, graph_meta_detail) =
            repository_graph_meta_lines(state.graph.document.as_ref());
        let toolbar_state = state
            .graph
            .document
            .as_ref()
            .map(|graph| repository_branch_toolbar_state(graph, state, toolbar_busy))
            .unwrap_or(RepositoryBranchToolbarState {
                checkout_disabled: true,
                create_disabled: true,
                delete_disabled: true,
            });
        let (checkout_label, create_label, delete_label) = match state.active_branch_action.as_ref()
        {
            Some(RepositoryBranchAction::Checkout { .. }) => {
                ("Checking Out…", "Create Branch", "Delete Branch")
            }
            Some(RepositoryBranchAction::Create { .. }) => {
                ("Checkout", "Creating…", "Delete Branch")
            }
            Some(RepositoryBranchAction::Delete { .. }) => {
                ("Checkout", "Create Branch", "Deleting…")
            }
            None => ("Checkout", "Create Branch", "Delete Branch"),
        };

        let mut actions = div().flex().items_center().gap(px(8.0));
        actions = actions.child(repository_action_button(
            palette,
            &format!("repository-fetch-action-{}", self.project_id),
            if state.fetch_in_flight {
                "Fetching…"
            } else {
                "Fetch"
            },
            fetch_disabled,
            false,
            {
                let ws = self.workspace.clone();
                let project_id = self.project_id.clone();
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws.update(cx, |ws, cx| {
                        ws.fetch_repository_graph(&project_id, cx);
                    });
                }
            },
        ));
        actions = actions.child(repository_action_button(
            palette,
            &format!("repository-pull-action-{}", self.project_id),
            &pull_state.label,
            pull_state.disabled,
            false,
            {
                let ws = self.workspace.clone();
                let project_id = self.project_id.clone();
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws.update(cx, |ws, cx| {
                        ws.pull_repository_current_branch(&project_id, cx);
                    });
                }
            },
        ));
        actions = actions.child(repository_action_button(
            palette,
            &format!("repository-checkout-action-{}", self.project_id),
            checkout_label,
            toolbar_state.checkout_disabled,
            false,
            {
                let ws = self.workspace.clone();
                let project_id = self.project_id.clone();
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    ws.update(cx, |ws, cx| {
                        ws.checkout_selected_repository_branch(&project_id, cx);
                    });
                }
            },
        ));
        actions = actions.child(repository_action_button(
            palette,
            &format!("repository-create-branch-action-{}", self.project_id),
            create_label,
            toolbar_state.create_disabled,
            false,
            {
                let ws = self.workspace.clone();
                let project_id = self.project_id.clone();
                let prompt_dialog_request = self.prompt_dialog_request.clone();
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    let spec = {
                        let ws = ws.read(cx);
                        ws.repository_graph_state(&project_id).and_then(|state| {
                            state.graph.document.as_ref().and_then(|graph| {
                                selected_local_branch_name(state).and_then(|branch_name| {
                                    create_branch_prompt_spec(
                                        &project_id,
                                        &state.scope_root,
                                        graph,
                                        &branch_name,
                                    )
                                })
                            })
                        })
                    };
                    if let Some(spec) = spec {
                        *prompt_dialog_request.borrow_mut() = Some(spec);
                        ws.update(cx, |_ws, cx| cx.notify());
                    }
                }
            },
        ));
        actions = actions.child(repository_action_button(
            palette,
            &format!("repository-delete-branch-action-{}", self.project_id),
            delete_label,
            toolbar_state.delete_disabled,
            true,
            {
                let ws = self.workspace.clone();
                let project_id = self.project_id.clone();
                let prompt_dialog_request = self.prompt_dialog_request.clone();
                move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                    let spec = {
                        let ws = ws.read(cx);
                        ws.repository_graph_state(&project_id).and_then(|state| {
                            state.graph.document.as_ref().and_then(|graph| {
                                selected_local_branch_name(state).map(|branch_name| {
                                    delete_branch_prompt_spec(
                                        &project_id,
                                        &state.scope_root,
                                        graph,
                                        &branch_name,
                                    )
                                })
                            })
                        })
                    };
                    if let Some(spec) = spec {
                        *prompt_dialog_request.borrow_mut() = Some(spec);
                        ws.update(cx, |_ws, cx| cx.notify());
                    }
                }
            },
        ));
        actions = actions.child(
            div().pl(px(10.0)).flex().items_center().child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.SLATE))
                            .child(graph_meta),
                    )
                    .when_some(graph_meta_detail, |meta, detail| {
                        meta.child(
                            div()
                                .text_size(px(10.0))
                                .font_family(DIFF_FONT_FAMILY)
                                .text_color(rgb(palette.STATUS_AMBER))
                                .child(detail),
                        )
                    }),
            ),
        );

        let mut header = div()
            .w_full()
            .bg(rgb(palette.DEEP))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .px(px(14.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.BONE))
                                    .child(project_name.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_family(DIFF_FONT_FAMILY)
                                    .text_color(rgb(palette.FOG))
                                    .child(state.scope_root.display().to_string()),
                            ),
                    )
                    .child(actions),
            );

        if let Some(banner) = state.action_banner.as_ref() {
            header = header.child(render_action_banner(
                "repository-header-banner",
                banner,
                palette,
                {
                    let ws = self.workspace.clone();
                    let project_id = self.project_id.clone();
                    move |_event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                        ws.update(cx, |ws, cx| {
                            ws.dismiss_repository_action_banner(&project_id, cx);
                        });
                    }
                },
            ));
        }

        header
    }

    fn render_branch_pane(
        &self,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        scope_busy: bool,
    ) -> Stateful<Div> {
        let rows = self.branch_rows.clone();
        let project_id = self.project_id.clone();
        let ws = self.workspace.clone();
        let menu_request = self.menu_request.clone();
        let prompt_dialog_request = self.prompt_dialog_request.clone();
        let palette = palette.clone();
        let selected_branch = state.selected_branch.clone();
        let graph = state.graph.document.clone();
        let scope_root = state.scope_root.clone();
        div()
            .id(ElementId::Name(
                format!("repository-branch-pane-{}", self.project_id).into(),
            ))
            .size_full()
            .bg(rgb(palette.DEEP))
            .child(
                uniform_list(
                    ElementId::Name(format!("repository-branches-{}", self.project_id).into()),
                    rows.len(),
                    move |range, _window, _cx| {
                        range
                            .map(|ix| match &rows[ix] {
                                BranchRailRow::SectionHeader { label } => div()
                                    .w_full()
                                    .h(px(BRANCH_ROW_HEIGHT))
                                    .px(px(12.0))
                                    .flex()
                                    .items_center()
                                    .text_size(px(10.0))
                                    .font_family(DIFF_FONT_FAMILY)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.FOG))
                                    .child(label.clone())
                                    .into_any_element(),
                                BranchRailRow::RemoteGroup {
                                    remote_name,
                                    expanded,
                                } => {
                                    let group_ws = ws.clone();
                                    let group_project_id = project_id.clone();
                                    let group_remote = remote_name.clone();
                                    let group_row = div()
                                        .w_full()
                                        .h(px(BRANCH_ROW_HEIGHT))
                                        .px(px(12.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.bg(rgba(theme::with_alpha(palette.CURRENT, 0x70)))
                                        });
                                    let group_row = group_row.on_mouse_down(
                                        MouseButton::Left,
                                        move |_event, _window, cx| {
                                            group_ws.update(cx, |ws, cx| {
                                                ws.toggle_repository_remote_group(
                                                    &group_project_id,
                                                    &group_remote,
                                                    cx,
                                                );
                                            });
                                        },
                                    );
                                    group_row
                                        .child(
                                            div()
                                                .w(px(10.0))
                                                .text_size(px(10.0))
                                                .font_family(DIFF_FONT_FAMILY)
                                                .text_color(rgb(palette.FOG))
                                                .child(if *expanded { "▾" } else { "▸" }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .font_family(DIFF_FONT_FAMILY)
                                                .text_color(rgb(palette.BONE))
                                                .child(remote_name.clone()),
                                        )
                                        .into_any_element()
                                }
                                BranchRailRow::Branch(row) => {
                                    let is_selected =
                                        selected_branch.as_ref() == Some(&row.selection);
                                    let click_selection = row.selection.clone();
                                    let row_ws = ws.clone();
                                    let row_project_id = project_id.clone();
                                    let context_selection = row.selection.clone();
                                    let menu_request = menu_request.clone();
                                    let prompt_dialog_request = prompt_dialog_request.clone();
                                    let graph = graph.clone();
                                    let context_scope_root = scope_root.clone();
                                    let mut branch_row = div()
                                        .id(ElementId::Name(
                                            format!("repo-branch-row-{}-{ix}", project_id).into(),
                                        ))
                                        .w_full()
                                        .h(px(BRANCH_ROW_HEIGHT))
                                        .px(px(12.0))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .cursor_pointer()
                                        .border_l_2();
                                    if is_selected {
                                        branch_row = branch_row
                                            .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x14)))
                                            .border_color(rgb(palette.ORCA_BLUE));
                                    } else {
                                        branch_row = branch_row
                                            .border_color(rgb(palette.DEEP))
                                            .hover(|style| {
                                                style.bg(rgba(theme::with_alpha(
                                                    palette.CURRENT,
                                                    0x70,
                                                )))
                                            });
                                    }

                                    let branch_label_color = if row.is_head {
                                        palette.ORCA_BLUE
                                    } else {
                                        palette.BONE
                                    };
                                    let check_label =
                                        if row.kind == BranchRailBranchKind::Local && row.is_head {
                                            "✓"
                                        } else {
                                            ""
                                        };
                                    let mut badges =
                                        div().flex_shrink_0().flex().items_center().gap(px(6.0));
                                    if row.is_worktree_occupied {
                                        badges = badges.child(pill_label(
                                            "WT",
                                            palette.STATUS_AMBER,
                                            palette.DEEP,
                                            &palette,
                                        ));
                                    }
                                    branch_row
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap(px(BRANCH_ROW_GAP))
                                                .child(
                                                    div()
                                                        .w(px(BRANCH_ROW_CHECK_WIDTH))
                                                        .text_size(px(11.0))
                                                        .font_family(DIFF_FONT_FAMILY)
                                                        .text_color(rgb(palette.ORCA_BLUE))
                                                        .child(check_label),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .w_full()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .text_size(px(12.0))
                                                        .font_family(DIFF_FONT_FAMILY)
                                                        .text_color(rgb(branch_label_color))
                                                        .child(row.name.clone()),
                                                )
                                                .when_some(
                                                    row.inline_upstream.clone(),
                                                    |branch_row, upstream| {
                                                        let counts_label =
                                                            branch_upstream_counts_label(&upstream);
                                                        branch_row.child(
                                                            div()
                                                                .flex_shrink_0()
                                                                .flex()
                                                                .items_center()
                                                                .gap(px(BRANCH_ROW_GAP))
                                                                .child(
                                                                    div()
                                                                        .text_size(px(10.0))
                                                                        .font_family(
                                                                            DIFF_FONT_FAMILY,
                                                                        )
                                                                        .text_color(
                                                                            rgb(palette.FOG),
                                                                        )
                                                                        .child(upstream.remote_ref),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_size(px(10.0))
                                                                        .font_family(
                                                                            DIFF_FONT_FAMILY,
                                                                        )
                                                                        .text_color(
                                                                            rgb(palette.FOG),
                                                                        )
                                                                        .child(counts_label),
                                                                ),
                                                        )
                                                    },
                                                ),
                                        )
                                        .child(badges)
                                        .on_click(move |_event, _window, cx| {
                                            row_ws.update(cx, |ws, cx| {
                                                ws.select_repository_branch(
                                                    &row_project_id,
                                                    click_selection.clone(),
                                                    cx,
                                                );
                                            });
                                        })
                                        .on_mouse_down(MouseButton::Right, {
                                            let row_ws = ws.clone();
                                            let project_id = project_id.clone();
                                            move |event: &MouseDownEvent, _window, cx| {
                                                let Some(graph) = graph.as_ref() else {
                                                    return;
                                                };
                                                row_ws.update(cx, |ws, cx| {
                                                    ws.select_repository_branch(
                                                        &project_id,
                                                        context_selection.clone(),
                                                        cx,
                                                    );
                                                });
                                                let items = repository_branch_context_menu_items(
                                                    project_id.clone(),
                                                    context_scope_root.clone(),
                                                    graph,
                                                    &context_selection,
                                                    scope_busy,
                                                    prompt_dialog_request.clone(),
                                                );
                                                if items.is_empty() {
                                                    return;
                                                }
                                                *menu_request.borrow_mut() =
                                                    Some((event.position, items));
                                                cx.stop_propagation();
                                            }
                                        })
                                        .into_any_element()
                                }
                                BranchRailRow::BranchMeta(row) => {
                                    let is_selected =
                                        selected_branch.as_ref() == Some(&row.selection);
                                    let click_selection = row.selection.clone();
                                    let row_ws = ws.clone();
                                    let row_project_id = project_id.clone();
                                    let context_selection = row.selection.clone();
                                    let menu_request = menu_request.clone();
                                    let prompt_dialog_request = prompt_dialog_request.clone();
                                    let graph = graph.clone();
                                    let context_scope_root = scope_root.clone();
                                    let mut meta_row = div()
                                        .id(ElementId::Name(
                                            format!("repo-branch-meta-row-{}-{ix}", project_id)
                                                .into(),
                                        ))
                                        .w_full()
                                        .h(px(BRANCH_ROW_HEIGHT))
                                        .pr(px(12.0))
                                        .pl(px(BRANCH_META_INDENT))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .border_l_2();
                                    if is_selected {
                                        meta_row = meta_row
                                            .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x14)))
                                            .border_color(rgb(palette.ORCA_BLUE));
                                    } else {
                                        meta_row = meta_row.border_color(rgb(palette.DEEP)).hover(
                                            |style| {
                                                style.bg(rgba(theme::with_alpha(
                                                    palette.CURRENT,
                                                    0x70,
                                                )))
                                            },
                                        );
                                    }

                                    meta_row
                                        .child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(BRANCH_ROW_GAP))
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(0.0))
                                                        .child(
                                                            div()
                                                                .w(px(BRANCH_META_TEXT_INDENT))
                                                                .flex_shrink_0(),
                                                        )
                                                        .child(
                                                            div()
                                                                .min_w_0()
                                                                .overflow_hidden()
                                                                .text_ellipsis()
                                                                .text_size(px(10.0))
                                                                .font_family(DIFF_FONT_FAMILY)
                                                                .text_color(rgb(palette.FOG))
                                                                .child(
                                                                    row.upstream.remote_ref.clone(),
                                                                ),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_size(px(10.0))
                                                        .font_family(DIFF_FONT_FAMILY)
                                                        .text_color(rgb(palette.FOG))
                                                        .child(branch_upstream_counts_label(
                                                            &row.upstream,
                                                        )),
                                                ),
                                        )
                                        .on_click(move |_event, _window, cx| {
                                            row_ws.update(cx, |ws, cx| {
                                                ws.select_repository_branch(
                                                    &row_project_id,
                                                    click_selection.clone(),
                                                    cx,
                                                );
                                            });
                                        })
                                        .on_mouse_down(MouseButton::Right, {
                                            let row_ws = ws.clone();
                                            let project_id = project_id.clone();
                                            move |event: &MouseDownEvent, _window, cx| {
                                                let Some(graph) = graph.as_ref() else {
                                                    return;
                                                };
                                                row_ws.update(cx, |ws, cx| {
                                                    ws.select_repository_branch(
                                                        &project_id,
                                                        context_selection.clone(),
                                                        cx,
                                                    );
                                                });
                                                let items = repository_branch_context_menu_items(
                                                    project_id.clone(),
                                                    context_scope_root.clone(),
                                                    graph,
                                                    &context_selection,
                                                    scope_busy,
                                                    prompt_dialog_request.clone(),
                                                );
                                                if items.is_empty() {
                                                    return;
                                                }
                                                *menu_request.borrow_mut() =
                                                    Some((event.position, items));
                                                cx.stop_propagation();
                                            }
                                        })
                                        .into_any_element()
                                }
                            })
                            .collect()
                    },
                )
                .size_full(),
            )
    }

    fn render_commit_pane(
        &self,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        if center_pane_mode(state) == RepositoryCenterPaneMode::CommitFileDiff {
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(palette.ABYSS))
                .child(self.render_selected_commit_file_diff(state, palette, cx));
        }

        let commits = self.commit_rows.clone();
        let project_id = self.project_id.clone();
        let ws = self.workspace.clone();
        let selected_commit = state.selected_commit;
        let current_head_branch =
            state
                .graph
                .document
                .as_ref()
                .and_then(|graph| match &graph.head {
                    HeadState::Branch { name, .. } => Some(name.clone()),
                    HeadState::Detached { .. } | HeadState::Unborn => None,
                });
        let palette = palette.clone();
        let pane = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col();

        if state.graph.loading && commits.is_empty() {
            return pane.child(panel_message(
                &palette,
                "Loading repository graph…",
                Some("The daemon is building the bounded topology view."),
            ));
        }
        if let Some(error) = state.graph.error.as_deref() {
            return pane.child(panel_message(
                &palette,
                "Could not load repository graph",
                Some(error),
            ));
        }
        if commits.is_empty() {
            return pane.child(panel_message(
                &palette,
                "No commit graph available",
                Some("Open a git-backed project to inspect history."),
            ));
        }

        pane.child(
            uniform_list(
                ElementId::Name(format!("repository-commits-{}", self.project_id).into()),
                commits.len(),
                move |range, _window, _cx| {
                    range
                        .map(|ix| {
                            render_commit_row(
                                &commits[ix],
                                selected_commit == Some(commits[ix].commit.oid),
                                current_head_branch.as_deref(),
                                &palette,
                                {
                                    let ws = ws.clone();
                                    let project_id = project_id.clone();
                                    let oid = commits[ix].commit.oid;
                                    move |_event: &ClickEvent,
                                          _window: &mut Window,
                                          cx: &mut App| {
                                        ws.update(cx, |ws, cx| {
                                            ws.select_repository_commit(&project_id, oid, cx);
                                        });
                                    }
                                },
                            )
                        })
                        .collect()
                },
            )
            .size_full(),
        )
    }

    fn render_detail_pane(
        &self,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        scope_busy: bool,
        _cx: &mut Context<Self>,
    ) -> Div {
        let pane = div().size_full().bg(rgb(palette.DEEP)).flex().flex_col();

        match detail_pane_mode(state) {
            RepositoryDetailPaneMode::Commit => {
                if let Some(oid) = state.selected_commit {
                    return pane.child(self.render_commit_detail(oid, state, palette));
                }
            }
            RepositoryDetailPaneMode::Branch => {
                if let Some(selection) = state.selected_branch.as_ref() {
                    return pane
                        .child(self.render_branch_detail(selection, state, palette, scope_busy));
                }
            }
            RepositoryDetailPaneMode::Empty => {}
        }

        pane.child(panel_message(
            palette,
            "No selection",
            Some("Pick a branch or commit to inspect details."),
        ))
    }

    fn render_pane_divider(
        &self,
        side: RepositoryPaneSide,
        palette: &OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .w(px(REPOSITORY_PANE_DIVIDER_WIDTH))
            .h_full()
            .bg(rgb(palette.CURRENT))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    let (initial_width, opposite_width) = match side {
                        RepositoryPaneSide::Branch => {
                            (this.branch_pane_width, this.detail_pane_width)
                        }
                        RepositoryPaneSide::Detail => {
                            (this.detail_pane_width, this.branch_pane_width)
                        }
                    };
                    this.resize_drag = Some(RepositoryPaneResizeDrag {
                        side,
                        initial_mouse_x: f32::from(event.position.x),
                        initial_width,
                        opposite_width,
                    });
                    cx.stop_propagation();
                }),
            )
    }

    fn render_branch_detail(
        &self,
        selection: &RepositoryBranchSelection,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        _scope_busy: bool,
    ) -> AnyElement {
        let Some(graph) = state.graph.document.as_ref() else {
            return div().into_any_element();
        };
        let detail = match selection {
            RepositoryBranchSelection::Local { name } => graph
                .local_branches
                .iter()
                .find(|branch| branch.name == *name)
                .map(|branch| {
                    let mut meta = vec![
                        format!("LOCAL"),
                        format!("Ref {}", branch.full_ref),
                        format!("Target {}", branch.target),
                    ];
                    if branch.is_head {
                        meta.push("Current branch".to_string());
                    }
                    if let Some(upstream) = branch.upstream.as_ref() {
                        meta.push(format!(
                            "Tracks {}/{} · +{} / -{}",
                            upstream.remote_name,
                            upstream.remote_ref,
                            upstream.ahead,
                            upstream.behind
                        ));
                    }
                    if state.occupied_local_branches.contains(&branch.name) {
                        meta.push("Occupied by an Orca-managed worktree".to_string());
                    }
                    (branch.name.clone(), meta)
                }),
            RepositoryBranchSelection::Remote { full_ref } => graph
                .remote_branches
                .iter()
                .find(|branch| branch.full_ref == *full_ref)
                .map(|branch| {
                    let mut meta = vec![
                        format!("REMOTE"),
                        format!("Ref {}", branch.full_ref),
                        format!("Target {}", branch.target),
                    ];
                    if let Some(local) = branch.tracked_by_local.as_ref() {
                        meta.push(format!("Tracked by local branch {local}"));
                    }
                    (
                        format!("{}/{}", branch.remote_name, branch.short_name),
                        meta,
                    )
                }),
        };

        let Some((title, meta)) = detail else {
            return panel_message(
                palette,
                "Branch no longer available",
                Some("Refresh the graph to inspect the latest repository state."),
            )
            .into_any_element();
        };

        div()
            .id(ElementId::Name(
                format!("repository-branch-detail-{}", self.project_id).into(),
            ))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(detail_header(&title, palette))
            .child(
                div()
                    .px(px(14.0))
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .children(meta.into_iter().map(|line| metadata_line(&line, palette))),
            )
            .into_any_element()
    }

    fn render_commit_detail(
        &self,
        oid: Oid,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
    ) -> AnyElement {
        if state.commit_detail.loading {
            return panel_message(
                palette,
                "Loading commit detail…",
                Some("Changed-file summaries are loaded lazily for the selected commit."),
            )
            .into_any_element();
        }
        if let Some(error) = state.commit_detail.error.as_deref() {
            return panel_message(palette, "Could not load commit detail", Some(error))
                .into_any_element();
        }
        let Some(detail) = state.commit_detail.document.as_ref() else {
            return panel_message(
                palette,
                "Select a commit",
                Some("Pick a commit from the graph to inspect its metadata."),
            )
            .into_any_element();
        };
        if detail.oid != oid {
            return panel_message(
                palette,
                "Loading commit detail…",
                Some("The right pane is waiting for the selected commit."),
            )
            .into_any_element();
        }

        let ws = self.workspace.clone();
        let project_id = self.project_id.clone();
        let selected_path = state
            .selected_commit_file
            .as_ref()
            .map(|selection| selection.relative_path.as_path());

        let mut body = div()
            .id(ElementId::Name(
                format!("repository-commit-detail-{}", detail.oid).into(),
            ))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(detail_header(&detail.summary, palette))
            .child(
                div()
                    .px(px(14.0))
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(metadata_line(
                        &format!("Commit {}", detail.short_oid),
                        palette,
                    ))
                    .child(metadata_line(
                        &format!("Author {} <{}>", detail.author_name, detail.author_email),
                        palette,
                    ))
                    .child(metadata_line(
                        &format!("Committed {}", format_timestamp(detail.committed_at_unix)),
                        palette,
                    ))
                    .when(!detail.message_body.is_empty(), |body_div| {
                        body_div.child(
                            div()
                                .mt(px(6.0))
                                .p(px(10.0))
                                .border_1()
                                .border_color(rgb(palette.SURFACE))
                                .bg(rgb(palette.ABYSS))
                                .rounded(px(4.0))
                                .text_size(px(12.0))
                                .text_color(rgb(palette.BONE))
                                .child(detail.message_body.clone()),
                        )
                    })
                    .child(
                        div()
                            .mt(px(8.0))
                            .text_size(px(10.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.FOG))
                            .child(format!("FILES ({})", detail.changed_files.len())),
                    ),
            );

        let changed_files = detail.changed_files.clone();
        for changed_file in changed_files {
            let is_selected = selected_path == Some(changed_file.path.as_path());
            let click_selection = CommitFileSelection {
                commit_oid: detail.oid,
                relative_path: changed_file.path.clone(),
            };
            body = body.child(
                render_commit_changed_file_row(&changed_file, is_selected, palette).on_click({
                    let ws = ws.clone();
                    let project_id = project_id.clone();
                    move |_event, _window, cx| {
                        ws.update(cx, |ws, cx| {
                            ws.select_repository_commit_file(
                                &project_id,
                                click_selection.clone(),
                                cx,
                            );
                        });
                    }
                }),
            );
        }

        body.into_any_element()
    }

    fn render_selected_commit_file_diff(
        &self,
        state: &RepositoryGraphTabState,
        palette: &OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(selection) = state.selected_commit_file.as_ref() else {
            return panel_message(
                palette,
                "Select a changed file",
                Some("Pick a file from the selected commit to inspect its historical patch."),
            );
        };

        self.render_commit_file_diff(selection, &state.commit_file_diff, palette, cx)
    }

    fn render_commit_file_diff(
        &self,
        selection: &CommitFileSelection,
        state: &AsyncDocumentState<CommitFileDiffDocument>,
        palette: &OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        if state.loading {
            return panel_message(
                palette,
                "Loading historical file diff…",
                Some("OrcaShell is rendering the selected file from commit history."),
            );
        }
        if let Some(error) = state.error.as_deref() {
            return panel_message(palette, "Could not load historical file diff", Some(error));
        }
        let Some(document) = state.document.as_ref() else {
            return panel_message(
                palette,
                "Select a changed file",
                Some("Pick a file from the selected commit to inspect its historical patch."),
            );
        };
        if document.selection != *selection {
            return panel_message(
                palette,
                "Loading historical file diff…",
                Some("The diff pane is waiting for the selected file."),
            );
        }

        let ws = self.workspace.clone();
        let project_id = self.project_id.clone();
        let palette = palette.clone();
        let line_palette = palette.clone();
        let Some(cached) = self.commit_file_line_cache.as_ref() else {
            return panel_message(
                &palette,
                "No rendered diff lines",
                Some("This historical diff did not produce a text body."),
            );
        };
        let lines = cached.lines.clone();
        let max_line_chars = cached.max_line_chars;
        let scroll_x = self.diff_scroll_x;
        let list_bounds = self.diff_list_bounds.clone();
        let scrollbar = self
            .diff_scrollbar_geometry()
            .map(|(thumb_y, thumb_height)| {
                let is_dragging = self.diff_scrollbar_drag.is_some();
                self.render_scrollbar(thumb_y, thumb_height, is_dragging, &palette, cx)
            });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .px(px(14.0))
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(rgb(palette.SURFACE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.BONE))
                                    .child(document.selection.relative_path.display().to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_family(DIFF_FONT_FAMILY)
                                    .text_color(rgb(palette.FOG))
                                    .child(format!("Commit {}", document.commit_oid)),
                            ),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("repository-back-to-commit-{}", self.project_id).into(),
                            ))
                            .px(px(10.0))
                            .py(px(6.0))
                            .border_1()
                            .border_color(rgb(palette.ORCA_BLUE))
                            .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x18)))
                            .rounded(px(4.0))
                            .text_size(px(11.0))
                            .font_family(DIFF_FONT_FAMILY)
                            .text_color(rgb(palette.PATCH))
                            .cursor_pointer()
                            .child("Back to History")
                            .on_click(move |_event, _window, cx| {
                                ws.update(cx, |ws, cx| {
                                    ws.back_to_repository_commit(&project_id, cx);
                                });
                            }),
                    ),
            )
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
                    .child(
                        uniform_list(
                            ElementId::Name(
                                format!("repository-commit-file-lines-{}", self.project_id).into(),
                            ),
                            lines.len(),
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
                                let delta_x = repository_scroll_delta_x(
                                    event.delta,
                                    this.measured_char_width,
                                );
                                if delta_x.abs() <= 0.5 {
                                    return;
                                }
                                let viewport_width =
                                    f32::from(this.diff_list_bounds.borrow().size.width);
                                let max_scroll = max_repository_diff_scroll(
                                    max_line_chars,
                                    this.measured_char_width,
                                    viewport_width,
                                );
                                let next_scroll = clamp_repository_diff_scroll(
                                    this.diff_scroll_x,
                                    delta_x,
                                    max_scroll,
                                );
                                if (next_scroll - this.diff_scroll_x).abs() > f32::EPSILON {
                                    this.diff_scroll_x = next_scroll;
                                    cx.notify();
                                    cx.stop_propagation();
                                }
                            },
                        )),
                    )
                    .children(scrollbar),
            )
    }
}

impl Render for RepositoryBrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_graph_cache(cx);
        self.sync_commit_file_cache(cx);
        self.measure_char_width(window);
        let palette = theme::active(cx);
        let (project_name, tab_state, fetch_disabled, toolbar_busy, pull_state) = {
            let ws = self.workspace.read(cx);
            let project_name = ws
                .project(&self.project_id)
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "Missing Project".to_string());
            let Some(tab_state) = ws.repository_graph_state(&self.project_id).cloned() else {
                return panel_message(
                    &palette,
                    "Repository browser unavailable",
                    Some("Open the repository browser from a git-backed project."),
                );
            };
            let scope_busy = ws.scope_git_action_in_flight(&tab_state.scope_root);
            let fetch_disabled = scope_busy;
            let toolbar_busy = ws.repository_toolbar_action_in_flight(&tab_state.scope_root);
            let pull_state = repository_pull_action_state(
                tab_state.graph.document.as_ref(),
                ws.git_scope_snapshot(&tab_state.scope_root),
                scope_busy,
                tab_state.pull_in_flight,
            );
            (
                project_name,
                tab_state,
                fetch_disabled,
                toolbar_busy,
                pull_state,
            )
        };
        let bounds_ref = self.bounds.clone();

        div()
            .size_full()
            .relative()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let Some(drag) = this.resize_drag else {
                    return;
                };
                let total_width = f32::from(this.bounds.borrow().size.width);
                if total_width <= 0.0 {
                    return;
                }
                match drag.side {
                    RepositoryPaneSide::Branch => {
                        let max_width =
                            max_repository_branch_pane_width(total_width, drag.opposite_width);
                        let next_width = (drag.initial_width
                            + (f32::from(event.position.x) - drag.initial_mouse_x))
                            .clamp(MIN_REPOSITORY_BRANCH_PANE_WIDTH, max_width);
                        if (next_width - this.branch_pane_width).abs() > f32::EPSILON {
                            this.branch_pane_width = next_width;
                            cx.notify();
                        }
                    }
                    RepositoryPaneSide::Detail => {
                        let max_width =
                            max_repository_detail_pane_width(total_width, drag.opposite_width);
                        let next_width = (drag.initial_width
                            - (f32::from(event.position.x) - drag.initial_mouse_x))
                            .clamp(MIN_REPOSITORY_DETAIL_PANE_WIDTH, max_width);
                        if (next_width - this.detail_pane_width).abs() > f32::EPSILON {
                            this.detail_pane_width = next_width;
                            cx.notify();
                        }
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    let mut changed = false;
                    if this.resize_drag.take().is_some() {
                        changed = true;
                    }
                    if this.diff_scrollbar_drag.take().is_some() {
                        changed = true;
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
            .child(self.render_header(
                &project_name,
                &tab_state,
                &palette,
                fetch_disabled,
                toolbar_busy,
                &pull_state,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(
                        div()
                            .w(px(self.branch_pane_width))
                            .min_w(px(MIN_REPOSITORY_BRANCH_PANE_WIDTH))
                            .h_full()
                            .flex_shrink_0()
                            .child(self.render_branch_pane(&tab_state, &palette, toolbar_busy)),
                    )
                    .child(self.render_pane_divider(RepositoryPaneSide::Branch, &palette, cx))
                    .child(self.render_commit_pane(&tab_state, &palette, cx))
                    .child(self.render_pane_divider(RepositoryPaneSide::Detail, &palette, cx))
                    .child(
                        div()
                            .w(px(self.detail_pane_width))
                            .min_w(px(MIN_REPOSITORY_DETAIL_PANE_WIDTH))
                            .h_full()
                            .flex_shrink_0()
                            .child(self.render_detail_pane(&tab_state, &palette, toolbar_busy, cx)),
                    ),
            )
    }
}

pub(crate) fn flatten_branch_rail(
    graph: &RepositoryGraphDocument,
    occupied_local_branches: &HashSet<String>,
    expanded_remote_groups: &HashSet<String>,
    branch_pane_width: f32,
) -> Vec<BranchRailRow> {
    let mut rows = Vec::new();
    rows.push(BranchRailRow::SectionHeader {
        label: "LOCAL".to_string(),
    });
    for branch in &graph.local_branches {
        let selection = RepositoryBranchSelection::Local {
            name: branch.name.clone(),
        };
        let upstream = branch
            .upstream
            .as_ref()
            .map(|upstream| BranchRailUpstreamSummary {
                remote_ref: format!("{}/{}", upstream.remote_name, upstream.remote_ref),
                ahead: upstream.ahead,
                behind: upstream.behind,
            });
        let show_inline_upstream = upstream.as_ref().is_some_and(|upstream| {
            local_branch_upstream_fits_inline(
                &branch.name,
                upstream,
                branch_pane_width,
                occupied_local_branches.contains(&branch.name),
            )
        });
        rows.push(BranchRailRow::Branch(BranchRailBranchRow {
            selection: selection.clone(),
            name: branch.name.clone(),
            kind: BranchRailBranchKind::Local,
            inline_upstream: if show_inline_upstream {
                upstream.clone()
            } else {
                None
            },
            is_head: branch.is_head,
            is_worktree_occupied: occupied_local_branches.contains(&branch.name),
        }));
        if let Some(upstream) = upstream.filter(|_| !show_inline_upstream) {
            rows.push(BranchRailRow::BranchMeta(BranchRailMetaRow {
                selection,
                upstream,
            }));
        }
    }

    rows.push(BranchRailRow::SectionHeader {
        label: "REMOTE".to_string(),
    });
    let mut current_remote = None::<String>;
    for branch in &graph.remote_branches {
        if current_remote.as_deref() != Some(branch.remote_name.as_str()) {
            current_remote = Some(branch.remote_name.clone());
            rows.push(BranchRailRow::RemoteGroup {
                remote_name: branch.remote_name.clone(),
                expanded: expanded_remote_groups.contains(&branch.remote_name),
            });
        }
        if !expanded_remote_groups.contains(&branch.remote_name) {
            continue;
        }
        rows.push(BranchRailRow::Branch(BranchRailBranchRow {
            selection: RepositoryBranchSelection::Remote {
                full_ref: branch.full_ref.clone(),
            },
            name: branch.short_name.clone(),
            kind: BranchRailBranchKind::Remote,
            inline_upstream: None,
            is_head: false,
            is_worktree_occupied: false,
        }));
    }

    rows
}

pub(crate) fn branch_rail_cache_key(
    graph: &RepositoryGraphDocument,
    occupied_local_branches: &HashSet<String>,
    expanded_remote_groups: &HashSet<String>,
    branch_pane_width: f32,
) -> BranchRailCacheKey {
    let mut hasher = DefaultHasher::new();
    for branch in &graph.local_branches {
        branch.name.hash(&mut hasher);
        branch.full_ref.hash(&mut hasher);
        branch.target.hash(&mut hasher);
        branch.is_head.hash(&mut hasher);
        if let Some(upstream) = branch.upstream.as_ref() {
            upstream.remote_name.hash(&mut hasher);
            upstream.remote_ref.hash(&mut hasher);
            upstream.ahead.hash(&mut hasher);
            upstream.behind.hash(&mut hasher);
        }
    }
    for branch in &graph.remote_branches {
        branch.remote_name.hash(&mut hasher);
        branch.short_name.hash(&mut hasher);
        branch.full_ref.hash(&mut hasher);
        branch.target.hash(&mut hasher);
        branch.tracked_by_local.hash(&mut hasher);
    }
    BranchRailCacheKey {
        content_hash: hasher.finish(),
        occupancy_count: occupied_local_branches.len(),
        occupancy_hash: hash_occupied_branches(occupied_local_branches),
        expanded_remote_count: expanded_remote_groups.len(),
        expanded_remote_hash: hash_expanded_remote_groups(expanded_remote_groups),
        layout_width_bucket: branch_pane_width.round().clamp(0.0, f32::from(u16::MAX)) as u16,
    }
}

fn branch_upstream_counts_label(upstream: &BranchRailUpstreamSummary) -> String {
    format!("+{} -{}", upstream.ahead, upstream.behind)
}

fn local_branch_upstream_fits_inline(
    branch_name: &str,
    upstream: &BranchRailUpstreamSummary,
    branch_pane_width: f32,
    is_worktree_occupied: bool,
) -> bool {
    let badge_width = if is_worktree_occupied {
        BRANCH_BADGE_WIDTH_ESTIMATE
    } else {
        0.0
    };
    let available_width = (branch_pane_width
        - BRANCH_ROW_HORIZONTAL_PADDING
        - BRANCH_ROW_CHECK_WIDTH
        - (BRANCH_ROW_GAP * 2.0)
        - badge_width)
        .max(0.0);
    let branch_width = branch_name.chars().count() as f32 * BRANCH_LABEL_CHAR_WIDTH;
    let upstream_width = upstream.remote_ref.chars().count() as f32 * BRANCH_META_CHAR_WIDTH
        + branch_upstream_counts_label(upstream).chars().count() as f32 * BRANCH_META_CHAR_WIDTH
        + BRANCH_ROW_GAP;
    branch_width + upstream_width + BRANCH_INLINE_FIT_BUFFER <= available_width
}

pub(crate) fn commit_rows_cache_key(graph: &RepositoryGraphDocument) -> CommitRowsCacheKey {
    let mut hasher = DefaultHasher::new();
    match &graph.head {
        orcashell_git::HeadState::Branch { name, oid } => {
            "branch".hash(&mut hasher);
            name.hash(&mut hasher);
            oid.hash(&mut hasher);
        }
        orcashell_git::HeadState::Detached { oid } => {
            "detached".hash(&mut hasher);
            oid.hash(&mut hasher);
        }
        orcashell_git::HeadState::Unborn => {
            "unborn".hash(&mut hasher);
        }
    }
    for commit in &graph.commits {
        commit.oid.hash(&mut hasher);
        commit.summary.hash(&mut hasher);
        commit.authored_at_unix.hash(&mut hasher);
        commit.primary_lane.hash(&mut hasher);
        commit.parent_oids.hash(&mut hasher);
        for segment in &commit.row_lanes {
            segment.lane.hash(&mut hasher);
            std::mem::discriminant(&segment.kind).hash(&mut hasher);
            segment.target_lane.hash(&mut hasher);
        }
        for label in &commit.ref_labels {
            label.name.hash(&mut hasher);
            label.kind.hash(&mut hasher);
        }
    }
    CommitRowsCacheKey {
        content_hash: hasher.finish(),
    }
}

fn hash_occupied_branches(branches: &HashSet<String>) -> u64 {
    let mut names: Vec<&String> = branches.iter().collect();
    names.sort();
    let mut hasher = DefaultHasher::new();
    for name in names {
        name.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_expanded_remote_groups(remote_groups: &HashSet<String>) -> u64 {
    let mut names: Vec<&String> = remote_groups.iter().collect();
    names.sort();
    let mut hasher = DefaultHasher::new();
    for name in names {
        name.hash(&mut hasher);
    }
    hasher.finish()
}

pub(crate) fn repository_graph_meta_lines(
    graph: Option<&RepositoryGraphDocument>,
) -> (String, Option<String>) {
    let Some(graph) = graph else {
        return ("No graph".to_string(), None);
    };

    let primary = format!("{} commits", graph.commits.len());
    let detail = graph.truncated.then_some("bounded view".to_string());
    (primary, detail)
}

fn style_commit_rows(graph: &RepositoryGraphDocument) -> Vec<StyledCommitRow> {
    let active_rows = active_lane_by_row(&graph.commits, &graph.head);
    let checked_out_tip = match graph.head {
        HeadState::Branch { oid, .. } => Some(oid),
        HeadState::Detached { .. } | HeadState::Unborn => None,
    };
    let detached_tip = match graph.head {
        HeadState::Detached { oid } => Some(oid),
        HeadState::Branch { .. } | HeadState::Unborn => None,
    };

    graph
        .commits
        .iter()
        .enumerate()
        .map(|(ix, commit)| StyledCommitRow {
            commit: commit.clone(),
            active_lane: active_rows[ix],
            is_checked_out_tip: checked_out_tip == Some(commit.oid),
            is_detached_head_tip: detached_tip == Some(commit.oid),
            visible_lane_ids: select_visible_graph_lanes(commit, active_rows[ix]),
            overflow_gutter_visible: graph_has_lane_overflow(commit),
        })
        .collect()
}

pub(crate) fn graph_participating_lanes(commit: &CommitGraphNode) -> Vec<u16> {
    let mut lanes = Vec::new();
    for segment in &commit.row_lanes {
        lanes.push(segment.lane);
        if let Some(target_lane) = segment.target_lane {
            lanes.push(target_lane);
        }
    }
    lanes.sort_unstable();
    lanes.dedup();
    lanes
}

pub(crate) fn select_visible_graph_lanes(
    commit: &CommitGraphNode,
    _active_lane: Option<u16>,
) -> Vec<u16> {
    graph_participating_lanes(commit)
        .into_iter()
        .filter(|lane| usize::from(*lane) < MAX_VISIBLE_GRAPH_LANES)
        .collect()
}

pub(crate) fn graph_has_lane_overflow(commit: &CommitGraphNode) -> bool {
    graph_participating_lanes(commit)
        .into_iter()
        .any(|lane| usize::from(lane) >= MAX_VISIBLE_GRAPH_LANES)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn visible_first_parent_spine(graph: &RepositoryGraphDocument) -> Vec<Oid> {
    let HeadState::Branch { oid: head_oid, .. } = graph.head else {
        return Vec::new();
    };
    let visible = graph
        .commits
        .iter()
        .map(|commit| (commit.oid, commit))
        .collect::<HashMap<_, _>>();
    let mut spine = Vec::new();
    let mut next = Some(head_oid);

    while let Some(oid) = next {
        let Some(commit) = visible.get(&oid) else {
            break;
        };
        spine.push(oid);
        next = commit
            .parent_oids
            .first()
            .copied()
            .filter(|parent| visible.contains_key(parent));
    }

    spine
}

pub(crate) fn active_lane_by_row(
    commits: &[CommitGraphNode],
    head: &HeadState,
) -> Vec<Option<u16>> {
    let mut active_lanes = vec![None; commits.len()];
    let HeadState::Branch { oid: head_oid, .. } = head else {
        return active_lanes;
    };

    let index_by_oid = commits
        .iter()
        .enumerate()
        .map(|(ix, commit)| (commit.oid, ix))
        .collect::<HashMap<_, _>>();
    let mut spine_positions = Vec::new();
    let mut next = Some(*head_oid);

    while let Some(oid) = next {
        let Some(&ix) = index_by_oid.get(&oid) else {
            break;
        };
        let commit = &commits[ix];
        spine_positions.push((ix, commit.primary_lane));
        next = commit
            .parent_oids
            .first()
            .copied()
            .filter(|parent| index_by_oid.contains_key(parent));
    }

    for window in spine_positions.windows(2) {
        let (start_ix, start_lane) = window[0];
        let (end_ix, _) = window[1];
        for lane in active_lanes.iter_mut().take(end_ix).skip(start_ix) {
            *lane = Some(start_lane);
        }
    }
    if let Some((last_ix, last_lane)) = spine_positions.last().copied() {
        active_lanes[last_ix] = Some(last_lane);
    }

    active_lanes
}

pub(crate) fn graph_lane_accent(
    lane: u16,
    active_lane: Option<u16>,
    detached_primary_lane: Option<u16>,
) -> GraphLaneAccent {
    if active_lane == Some(lane) || detached_primary_lane == Some(lane) {
        return GraphLaneAccent::Active;
    }
    match lane % 4 {
        0 => GraphLaneAccent::Green,
        1 => GraphLaneAccent::Amber,
        2 => GraphLaneAccent::Fog,
        _ => GraphLaneAccent::Slate,
    }
}

pub(crate) fn commit_row_highlight_state(
    is_selected: bool,
    is_current_tip: bool,
) -> CommitRowHighlightState {
    if is_selected {
        CommitRowHighlightState::Selected
    } else if is_current_tip {
        CommitRowHighlightState::CurrentTip
    } else {
        CommitRowHighlightState::HoverOnly
    }
}

pub(crate) fn lane_shows_node(primary_lane: u16, segment: orcashell_git::GraphLaneSegment) -> bool {
    !matches!(
        segment.kind,
        GraphLaneKind::MergeFromLeft | GraphLaneKind::MergeFromRight
    ) && segment.lane == primary_lane
}

fn graph_lane_accent_color(accent: GraphLaneAccent, palette: &OrcaTheme) -> u32 {
    match accent {
        GraphLaneAccent::Active => palette.ORCA_BLUE,
        GraphLaneAccent::Green => palette.STATUS_GREEN,
        GraphLaneAccent::Amber => palette.STATUS_AMBER,
        GraphLaneAccent::Fog => palette.FOG,
        GraphLaneAccent::Slate => palette.SLATE,
    }
}

fn detail_header(title: &str, palette: &OrcaTheme) -> Div {
    div()
        .w_full()
        .px(px(14.0))
        .py(px(12.0))
        .border_b_1()
        .border_color(rgb(palette.SURFACE))
        .text_size(px(13.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(palette.BONE))
        .child(title.to_string())
}

fn metadata_line(text: &str, palette: &OrcaTheme) -> Div {
    div()
        .text_size(px(11.0))
        .font_family(DIFF_FONT_FAMILY)
        .text_color(rgb(palette.FOG))
        .child(text.to_string())
}

fn panel_message(palette: &OrcaTheme, title: &str, detail: Option<&str>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(palette.BONE))
                .child(title.to_string()),
        )
        .when_some(detail, |panel_div, detail| {
            panel_div.child(
                div()
                    .max_w(px(420.0))
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.FOG))
                    .child(detail.to_string()),
            )
        })
}

pub(crate) fn repository_checkout_action_state(
    graph: &RepositoryGraphDocument,
    selection: &RepositoryBranchSelection,
    scope_busy: bool,
) -> BranchDetailActionState {
    match selection {
        RepositoryBranchSelection::Local { name } => {
            let is_current = local_branch_is_current(graph, name);
            BranchDetailActionState {
                disabled: scope_busy || is_current,
            }
        }
        RepositoryBranchSelection::Remote { full_ref } => {
            let is_current_tracking = remote_branch_is_current_tracking_target(graph, full_ref);
            BranchDetailActionState {
                disabled: scope_busy || is_current_tracking,
            }
        }
    }
}

pub(crate) fn repository_branch_toolbar_state(
    graph: &RepositoryGraphDocument,
    state: &RepositoryGraphTabState,
    scope_busy: bool,
) -> RepositoryBranchToolbarState {
    let selected_branch = state.selected_branch.as_ref();
    let checkout_disabled = selected_branch
        .map(|selection| repository_checkout_action_state(graph, selection, scope_busy).disabled)
        .unwrap_or(true);
    let create_disabled = scope_busy
        || !matches!(
            selected_branch,
            Some(RepositoryBranchSelection::Local { .. })
        );
    let delete_disabled = scope_busy
        || match selected_branch {
            Some(RepositoryBranchSelection::Local { name }) => local_branch_is_current(graph, name),
            Some(RepositoryBranchSelection::Remote { .. }) | None => true,
        };

    RepositoryBranchToolbarState {
        checkout_disabled,
        create_disabled,
        delete_disabled,
    }
}

pub(crate) fn repository_pull_action_state(
    graph: Option<&RepositoryGraphDocument>,
    snapshot: Option<&GitSnapshotSummary>,
    scope_busy: bool,
    pull_in_flight: bool,
) -> RepositoryPullActionState {
    if pull_in_flight {
        return RepositoryPullActionState {
            disabled: true,
            label: "Pulling…".to_string(),
        };
    }

    let Some(graph) = graph else {
        return RepositoryPullActionState {
            disabled: true,
            label: "Pull".to_string(),
        };
    };

    let Some(head_branch_name) = (match &graph.head {
        HeadState::Branch { name, .. } => Some(name.as_str()),
        HeadState::Detached { .. } | HeadState::Unborn => None,
    }) else {
        return RepositoryPullActionState {
            disabled: true,
            label: "Pull".to_string(),
        };
    };

    let Some(current_branch) = graph
        .local_branches
        .iter()
        .find(|branch| branch.name == head_branch_name)
    else {
        return RepositoryPullActionState {
            disabled: true,
            label: "Pull".to_string(),
        };
    };

    let behind = current_branch
        .upstream
        .as_ref()
        .map_or(0, |upstream| upstream.behind);
    let label = if behind > 0 {
        format!("Pull ({behind})")
    } else {
        "Pull".to_string()
    };
    let dirty = snapshot.is_some_and(|snapshot| snapshot.changed_files > 0);

    RepositoryPullActionState {
        disabled: scope_busy || dirty || current_branch.upstream.is_none() || behind == 0,
        label,
    }
}

fn local_branch_is_current(graph: &RepositoryGraphDocument, branch_name: &str) -> bool {
    matches!(
        &graph.head,
        orcashell_git::HeadState::Branch {
            name: head_name, ..
        } if head_name == branch_name
    )
}

fn selected_local_branch_name(state: &RepositoryGraphTabState) -> Option<String> {
    match state.selected_branch.as_ref() {
        Some(RepositoryBranchSelection::Local { name }) => Some(name.clone()),
        Some(RepositoryBranchSelection::Remote { .. }) | None => None,
    }
}

fn local_branch_target(graph: &RepositoryGraphDocument, branch_name: &str) -> Option<Oid> {
    graph
        .local_branches
        .iter()
        .find(|branch| branch.name == branch_name)
        .map(|branch| branch.target)
}

fn create_branch_prompt_spec(
    project_id: &str,
    scope_root: &Path,
    graph: &RepositoryGraphDocument,
    source_branch_name: &str,
) -> Option<PromptDialogSpec> {
    let target = local_branch_target(graph, source_branch_name)?;
    let short_target = target.to_string();
    let short_target = short_target[..short_target.len().min(8)].to_string();
    let project_id = project_id.to_string();
    let source_branch_name = source_branch_name.to_string();
    let scope_root_display = scope_root.display().to_string();

    Some(PromptDialogSpec {
        title: format!("Create Branch From {source_branch_name}"),
        detail: Some(format!(
            "Target commit {short_target}. The new branch will be checked out in {scope_root_display}."
        )),
        confirm_label: "Create & Checkout".to_string(),
        confirm_tone: PromptDialogConfirmTone::Primary,
        input: Some(PromptDialogInputSpec {
            placeholder: "new branch name".to_string(),
            initial_value: String::new(),
            allow_empty: false,
            validate: None,
        }),
        selection: None,
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, result| {
            if let Some(new_branch_name) = result.input {
                ws.create_repository_branch(
                    &project_id,
                    source_branch_name.clone(),
                    new_branch_name,
                    cx,
                );
            }
        }),
    })
}

fn delete_branch_prompt_spec(
    project_id: &str,
    scope_root: &Path,
    graph: &RepositoryGraphDocument,
    branch_name: &str,
) -> PromptDialogSpec {
    let project_id = project_id.to_string();
    let branch_name_owned = branch_name.to_string();
    let detail = local_branch_target(graph, branch_name)
        .map(|target| {
            let short_target = target.to_string();
            let short_target = short_target[..short_target.len().min(8)].to_string();
            format!(
                "Delete local branch {branch_name} at {short_target} from the repository rooted at {}. Git may still refuse if the branch is checked out elsewhere or not fully merged.",
                scope_root.display()
            )
        })
        .unwrap_or_else(|| {
            format!(
                "Delete local branch {branch_name} from the repository rooted at {}.",
                scope_root.display()
            )
        });

    PromptDialogSpec {
        title: format!("Delete Branch {branch_name}"),
        detail: Some(detail),
        confirm_label: "Delete Branch".to_string(),
        confirm_tone: PromptDialogConfirmTone::Destructive,
        input: None,
        selection: None,
        toggles: Vec::new(),
        on_confirm: Box::new(move |ws, cx, _result| {
            ws.delete_repository_branch(&project_id, branch_name_owned.clone(), cx);
        }),
    }
}

fn repository_branch_context_menu_items(
    project_id: String,
    scope_root: PathBuf,
    graph: &RepositoryGraphDocument,
    selection: &RepositoryBranchSelection,
    scope_busy: bool,
    prompt_dialog_request: PromptDialogRequest,
) -> Vec<ContextMenuItem> {
    let checkout_state = repository_checkout_action_state(graph, selection, scope_busy);
    let checkout_selection = selection.clone();
    let checkout_project_id = project_id.clone();
    let mut items = vec![ContextMenuItem {
        label: "Checkout".to_string(),
        shortcut: None,
        enabled: !checkout_state.disabled,
        action: Box::new(move |ws, cx| {
            ws.select_repository_branch(&checkout_project_id, checkout_selection.clone(), cx);
            ws.checkout_selected_repository_branch(&checkout_project_id, cx);
        }),
    }];

    if let RepositoryBranchSelection::Local { name } = selection {
        if local_branch_target(graph, name).is_some() {
            let prompt_dialog_request = prompt_dialog_request.clone();
            let create_project_id = project_id.clone();
            let create_scope_root = scope_root.clone();
            let create_graph = graph.clone();
            let create_branch_name = name.clone();
            items.push(ContextMenuItem {
                label: "Create Branch From Here…".to_string(),
                shortcut: None,
                enabled: !scope_busy,
                action: Box::new(move |_ws, cx| {
                    if let Some(spec) = create_branch_prompt_spec(
                        &create_project_id,
                        &create_scope_root,
                        &create_graph,
                        &create_branch_name,
                    ) {
                        *prompt_dialog_request.borrow_mut() = Some(spec);
                        cx.notify();
                    }
                }),
            });
        }

        let delete_disabled = scope_busy || local_branch_is_current(graph, name);
        let prompt_dialog_request = prompt_dialog_request.clone();
        let delete_project_id = project_id.clone();
        let delete_scope_root = scope_root.clone();
        let delete_graph = graph.clone();
        let delete_branch_name = name.clone();
        items.push(ContextMenuItem {
            label: "Delete Branch…".to_string(),
            shortcut: None,
            enabled: !delete_disabled,
            action: Box::new(move |_ws, cx| {
                *prompt_dialog_request.borrow_mut() = Some(delete_branch_prompt_spec(
                    &delete_project_id,
                    &delete_scope_root,
                    &delete_graph,
                    &delete_branch_name,
                ));
                cx.notify();
            }),
        });
    }

    items
}

fn remote_branch_is_current_tracking_target(
    graph: &RepositoryGraphDocument,
    remote_full_ref: &str,
) -> bool {
    let head_branch_name = match &graph.head {
        orcashell_git::HeadState::Branch { name, .. } => name,
        orcashell_git::HeadState::Detached { .. } | orcashell_git::HeadState::Unborn => {
            return false;
        }
    };

    graph
        .local_branches
        .iter()
        .find(|branch| branch.name == *head_branch_name)
        .and_then(|branch| branch.upstream.as_ref())
        .is_some_and(|upstream| {
            format!(
                "refs/remotes/{}/{}",
                upstream.remote_name, upstream.remote_ref
            ) == remote_full_ref
        })
}

fn repository_action_button(
    palette: &OrcaTheme,
    id: &str,
    label: &str,
    disabled: bool,
    destructive: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let accent = if destructive {
        palette.STATUS_CORAL
    } else {
        palette.ORCA_BLUE
    };
    let hover_bg = theme::with_alpha(accent, 0x1A);
    let hover_text = palette.BONE;
    let mut button = div()
        .id(ElementId::Name(id.to_string().into()))
        .px(px(10.0))
        .py(px(7.0))
        .border_1()
        .rounded(px(4.0))
        .text_size(px(11.0))
        .font_family(DIFF_FONT_FAMILY);

    if disabled {
        button = button
            .bg(rgb(palette.CURRENT))
            .border_color(rgb(palette.BORDER_DEFAULT))
            .text_color(rgb(palette.SLATE))
            .cursor(CursorStyle::Arrow)
            .opacity(0.5);
    } else {
        button = button
            .bg(rgba(theme::with_alpha(accent, 0x0C)))
            .border_color(rgb(if destructive {
                palette.STATUS_CORAL
            } else {
                palette.BORDER_EMPHASIS
            }))
            .text_color(rgb(if destructive {
                palette.STATUS_CORAL
            } else {
                palette.FOG
            }))
            .cursor_pointer()
            .hover(move |style| style.bg(rgba(hover_bg)).text_color(rgb(hover_text)))
            .on_click(on_click);
    }

    button.child(label.to_string())
}

fn render_action_banner(
    id: &str,
    banner: &ActionBanner,
    palette: &OrcaTheme,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (text_color, border_color, bg_color) = match banner.kind {
        ActionBannerKind::Success => (
            palette.STATUS_GREEN,
            palette.STATUS_GREEN,
            theme::with_alpha(palette.STATUS_GREEN, 0x12),
        ),
        ActionBannerKind::Warning => (
            palette.STATUS_AMBER,
            palette.STATUS_AMBER,
            theme::with_alpha(palette.STATUS_AMBER, 0x12),
        ),
        ActionBannerKind::Error => (
            palette.STATUS_CORAL,
            palette.STATUS_CORAL,
            theme::with_alpha(palette.STATUS_CORAL, 0x12),
        ),
    };

    div()
        .id(ElementId::Name(id.to_string().into()))
        .mt(px(10.0))
        .w_full()
        .px(px(10.0))
        .py(px(8.0))
        .border_1()
        .border_color(rgb(border_color))
        .bg(rgba(bg_color))
        .rounded(px(4.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.0))
                .font_family(DIFF_FONT_FAMILY)
                .text_color(rgb(text_color))
                .child(banner.message.clone()),
        )
        .child(
            div()
                .id(ElementId::Name(format!("{id}-dismiss").into()))
                .cursor_pointer()
                .text_size(px(11.0))
                .font_family(DIFF_FONT_FAMILY)
                .text_color(rgb(palette.FOG))
                .child("Dismiss")
                .on_click(dismiss),
        )
}

fn pill_label(label: &str, border_color: u32, bg_color: u32, palette: &OrcaTheme) -> Div {
    div()
        .px(px(6.0))
        .py(px(3.0))
        .border_1()
        .border_color(rgb(border_color))
        .bg(rgba(theme::with_alpha(bg_color, 0x28)))
        .rounded(px(4.0))
        .text_size(px(10.0))
        .font_family(DIFF_FONT_FAMILY)
        .text_color(rgb(palette.PATCH))
        .child(label.to_string())
}

fn render_commit_row(
    row_state: &StyledCommitRow,
    is_selected: bool,
    current_head_branch: Option<&str>,
    palette: &OrcaTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let commit = &row_state.commit;
    let highlight_state = commit_row_highlight_state(
        is_selected,
        row_state.is_checked_out_tip || row_state.is_detached_head_tip,
    );
    let mut row = div()
        .id(ElementId::Name(
            format!("repo-commit-{}", commit.oid).into(),
        ))
        .w_full()
        .h(px(COMMIT_ROW_HEIGHT))
        .px(px(12.0))
        .border_l_2()
        .border_color(match highlight_state {
            CommitRowHighlightState::Selected => rgb(palette.SURFACE),
            CommitRowHighlightState::CurrentTip => rgb(palette.ORCA_BLUE),
            CommitRowHighlightState::HoverOnly => rgb(palette.ABYSS),
        })
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between();
    match highlight_state {
        CommitRowHighlightState::Selected => {
            row = row.bg(rgba(theme::with_alpha(palette.SURFACE, 0x80)));
        }
        CommitRowHighlightState::CurrentTip => {
            row = row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x12)));
        }
        CommitRowHighlightState::HoverOnly => {
            row = row.hover(|style| style.bg(rgba(theme::with_alpha(palette.CURRENT, 0x70))));
        }
    }

    row.child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(render_graph_lanes(row_state, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .children(commit.ref_labels.iter().map(|label| {
                        render_ref_pill(label.kind, &label.name, current_head_branch, palette)
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.BONE))
                            .child(commit.summary.clone()),
                    ),
            ),
    )
    .child(
        div()
            .w(px(76.0))
            .text_size(px(11.0))
            .font_family(DIFF_FONT_FAMILY)
            .text_color(rgb(palette.FOG))
            .text_align(TextAlign::Right)
            .child(format_relative_time(commit.authored_at_unix)),
    )
    .on_click(on_click)
    .into_any_element()
}

fn render_graph_lanes(row_state: &StyledCommitRow, palette: &OrcaTheme) -> Div {
    let row_lanes = &row_state.commit.row_lanes;
    let display_lane_count = row_state
        .visible_lane_ids
        .iter()
        .copied()
        .max()
        .map_or(0, |lane| usize::from(lane) + 1);
    let mut lane_cells: Vec<AnyElement> = Vec::with_capacity(
        display_lane_count
            + if row_state.overflow_gutter_visible {
                1
            } else {
                0
            },
    );

    for source_display_lane in 0..display_lane_count {
        let visible_lane = source_display_lane as u16;
        let segment = row_lanes
            .iter()
            .find(|segment| segment.lane == visible_lane)
            .copied();
        let target_display_lane = segment.and_then(|segment| {
            segment.target_lane.and_then(|target_lane| {
                (usize::from(target_lane) < MAX_VISIBLE_GRAPH_LANES)
                    .then_some(usize::from(target_lane))
            })
        });
        lane_cells.push(
            div()
                .w(px(GRAPH_LANE_SIZE))
                .h(px(COMMIT_ROW_HEIGHT - 6.0))
                .flex_shrink_0()
                .relative()
                .child(render_lane_segment(
                    segment,
                    row_state.commit.primary_lane,
                    row_state.active_lane,
                    if row_state.is_detached_head_tip {
                        Some(row_state.commit.primary_lane)
                    } else {
                        None
                    },
                    source_display_lane,
                    target_display_lane,
                    palette,
                ))
                .into_any_element(),
        );
    }

    if row_state.overflow_gutter_visible {
        lane_cells.push(render_graph_overflow_gutter(palette));
    }

    div()
        .w(px(GRAPH_COLUMN_WIDTH))
        .min_w(px(GRAPH_COLUMN_WIDTH))
        .flex()
        .items_center()
        .gap(px(GRAPH_LANE_GAP))
        .overflow_hidden()
        .children(lane_cells)
}

fn render_lane_segment(
    segment: Option<orcashell_git::GraphLaneSegment>,
    primary_lane: u16,
    active_lane: Option<u16>,
    detached_primary_lane: Option<u16>,
    source_display_lane: usize,
    target_display_lane: Option<usize>,
    palette: &OrcaTheme,
) -> AnyElement {
    let mut lane = div().size_full().relative();

    if let Some(segment) = segment {
        let accent = graph_lane_accent_color(
            graph_lane_accent(segment.lane, active_lane, detached_primary_lane),
            palette,
        );
        let center_x = (GRAPH_LANE_SIZE * 0.5) - 1.0;
        let center_y = (COMMIT_ROW_HEIGHT - 6.0) * 0.5;
        let vertical = match segment.kind {
            GraphLaneKind::End => div()
                .absolute()
                .top(px(0.0))
                .h(px(center_y))
                .left(px(center_x))
                .w(px(2.0))
                .bg(rgba(theme::with_alpha(accent, 0xB0))),
            _ => div()
                .absolute()
                .top(px(0.0))
                .bottom(px(0.0))
                .left(px(center_x))
                .w(px(2.0))
                .bg(rgba(theme::with_alpha(accent, 0xB0))),
        };
        lane = lane.child(vertical);
        if let Some(target_display_lane) = target_display_lane {
            let lane_span = (target_display_lane as f32 - source_display_lane as f32)
                * (GRAPH_LANE_SIZE + GRAPH_LANE_GAP);
            let connector_width = lane_span.abs().max(0.0);
            let connector_left = if lane_span.is_sign_negative() {
                center_x + lane_span
            } else {
                center_x
            };
            lane = lane.child(
                div()
                    .absolute()
                    .top(px(center_y - 1.0))
                    .left(px(connector_left))
                    .w(px(connector_width + 2.0))
                    .h(px(2.0))
                    .bg(rgba(theme::with_alpha(accent, 0xB8))),
            );
        }
        if lane_shows_node(primary_lane, segment) {
            lane = lane.child(
                div()
                    .absolute()
                    .top(px(center_y - 4.0))
                    .left(px((GRAPH_LANE_SIZE * 0.5) - 4.0))
                    .w(px(8.0))
                    .h(px(8.0))
                    .bg(rgb(accent))
                    .rounded(px(4.0)),
            );
        }
    }

    lane.into_any_element()
}

fn render_graph_overflow_gutter(palette: &OrcaTheme) -> AnyElement {
    let total_height = (GRAPH_OVERFLOW_DOT_SIZE * 3.0) + (GRAPH_OVERFLOW_DOT_GAP * 2.0);
    let start_y = (((COMMIT_ROW_HEIGHT - 6.0) - total_height) * 0.5).max(0.0);
    let center_x = ((GRAPH_LANE_SIZE - GRAPH_OVERFLOW_DOT_SIZE) * 0.5).max(0.0);

    div()
        .w(px(GRAPH_LANE_SIZE))
        .h(px(COMMIT_ROW_HEIGHT - 6.0))
        .flex_shrink_0()
        .relative()
        .child(
            div()
                .absolute()
                .left(px(center_x))
                .top(px(start_y))
                .w(px(GRAPH_OVERFLOW_DOT_SIZE))
                .h(px(GRAPH_OVERFLOW_DOT_SIZE))
                .rounded(px(GRAPH_OVERFLOW_DOT_SIZE * 0.5))
                .bg(rgba(theme::with_alpha(palette.SLATE, 0xC0))),
        )
        .child(
            div()
                .absolute()
                .left(px(center_x))
                .top(px(start_y
                    + GRAPH_OVERFLOW_DOT_SIZE
                    + GRAPH_OVERFLOW_DOT_GAP))
                .w(px(GRAPH_OVERFLOW_DOT_SIZE))
                .h(px(GRAPH_OVERFLOW_DOT_SIZE))
                .rounded(px(GRAPH_OVERFLOW_DOT_SIZE * 0.5))
                .bg(rgba(theme::with_alpha(palette.SLATE, 0xC0))),
        )
        .child(
            div()
                .absolute()
                .left(px(center_x))
                .top(px(
                    start_y + (GRAPH_OVERFLOW_DOT_SIZE + GRAPH_OVERFLOW_DOT_GAP) * 2.0
                ))
                .w(px(GRAPH_OVERFLOW_DOT_SIZE))
                .h(px(GRAPH_OVERFLOW_DOT_SIZE))
                .rounded(px(GRAPH_OVERFLOW_DOT_SIZE * 0.5))
                .bg(rgba(theme::with_alpha(palette.SLATE, 0xC0))),
        )
        .into_any_element()
}

fn render_ref_pill(
    kind: CommitRefKind,
    label: &str,
    current_head_branch: Option<&str>,
    palette: &OrcaTheme,
) -> AnyElement {
    let border = match kind {
        CommitRefKind::Head => palette.ORCA_BLUE,
        CommitRefKind::LocalBranch if current_head_branch == Some(label) => palette.ORCA_BLUE,
        CommitRefKind::LocalBranch => palette.STATUS_GREEN,
        CommitRefKind::RemoteBranch => palette.STATUS_AMBER,
    };
    div()
        .px(px(6.0))
        .py(px(2.0))
        .border_1()
        .border_color(rgb(border))
        .bg(rgba(theme::with_alpha(border, 0x14)))
        .rounded(px(4.0))
        .text_size(px(10.0))
        .font_family(DIFF_FONT_FAMILY)
        .text_color(rgb(palette.PATCH))
        .child(label.to_string())
        .into_any_element()
}

fn render_commit_changed_file_row(
    file: &CommitChangedFile,
    is_selected: bool,
    palette: &OrcaTheme,
) -> Stateful<Div> {
    let mut row = div()
        .id(ElementId::Name(
            format!("repository-commit-file-{}", file.path.display()).into(),
        ))
        .w_full()
        .px(px(14.0))
        .py(px(8.0))
        .border_t_1()
        .border_color(rgb(palette.SURFACE))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between();

    if is_selected {
        row = row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x12)));
    } else {
        row = row.hover(|style| style.bg(rgba(theme::with_alpha(palette.CURRENT, 0x70))));
    }

    row.child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(status_color(file.status.clone(), palette)))
                    .child(status_label(&file.status)),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(12.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.BONE))
                    .child(file.path.display().to_string()),
            ),
    )
    .child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_GREEN))
                    .child(format!("+{}", file.additions)),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_CORAL))
                    .child(format!("-{}", file.deletions)),
            ),
    )
}

pub(crate) fn center_pane_mode(state: &RepositoryGraphTabState) -> RepositoryCenterPaneMode {
    if state.selected_commit_file.is_some() {
        RepositoryCenterPaneMode::CommitFileDiff
    } else {
        RepositoryCenterPaneMode::History
    }
}

pub(crate) fn detail_pane_mode(state: &RepositoryGraphTabState) -> RepositoryDetailPaneMode {
    if state.selected_commit.is_some() {
        RepositoryDetailPaneMode::Commit
    } else if state.selected_branch.is_some() {
        RepositoryDetailPaneMode::Branch
    } else {
        RepositoryDetailPaneMode::Empty
    }
}

pub(crate) fn max_repository_branch_pane_width(total_width: f32, detail_width: f32) -> f32 {
    (total_width
        - detail_width
        - (REPOSITORY_PANE_DIVIDER_WIDTH * 2.0)
        - MIN_REPOSITORY_CENTER_PANE_WIDTH)
        .max(MIN_REPOSITORY_BRANCH_PANE_WIDTH)
}

pub(crate) fn max_repository_detail_pane_width(total_width: f32, branch_width: f32) -> f32 {
    (total_width
        - branch_width
        - (REPOSITORY_PANE_DIVIDER_WIDTH * 2.0)
        - MIN_REPOSITORY_CENTER_PANE_WIDTH)
        .max(MIN_REPOSITORY_DETAIL_PANE_WIDTH)
}

pub(crate) fn max_repository_diff_scroll(
    max_line_chars: usize,
    measured_char_width: f32,
    viewport_width: f32,
) -> f32 {
    (max_diff_content_width(max_line_chars, measured_char_width) - viewport_width).max(0.0)
}

pub(crate) fn clamp_repository_diff_scroll(current: f32, delta_x: f32, max_scroll: f32) -> f32 {
    (current - delta_x).clamp(0.0, max_scroll.max(0.0))
}

fn repository_scroll_delta_x(delta: ScrollDelta, measured_char_width: f32) -> f32 {
    match delta {
        ScrollDelta::Pixels(delta) => f32::from(delta.x),
        ScrollDelta::Lines(delta) => delta.x * measured_char_width * 3.0,
    }
}

fn status_color(status: CommitFileStatus, palette: &OrcaTheme) -> u32 {
    match status {
        CommitFileStatus::Added => palette.STATUS_GREEN,
        CommitFileStatus::Modified => palette.ORCA_BLUE,
        CommitFileStatus::Deleted => palette.STATUS_CORAL,
        CommitFileStatus::Renamed { .. } | CommitFileStatus::Typechange => palette.STATUS_AMBER,
    }
}

fn status_label(status: &CommitFileStatus) -> &'static str {
    match status {
        CommitFileStatus::Added => "ADD",
        CommitFileStatus::Modified => "MOD",
        CommitFileStatus::Deleted => "DEL",
        CommitFileStatus::Renamed { .. } => "REN",
        CommitFileStatus::Typechange => "TYPE",
    }
}

fn format_relative_time(timestamp_unix: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let delta = (now - timestamp_unix).max(0);
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

fn format_timestamp(timestamp_unix: i64) -> String {
    let (year, month, day, hour, minute) = timestamp_to_utc(timestamp_unix);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

fn timestamp_to_utc(timestamp_unix: i64) -> (i64, u32, u32, u32, u32) {
    let days = timestamp_unix.div_euclid(86_400);
    let seconds_of_day = timestamp_unix.rem_euclid(86_400);
    let hour = (seconds_of_day / 3_600) as u32;
    let minute = ((seconds_of_day % 3_600) / 60) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - (era * 146_097);
    let year_of_era =
        (day_of_era - (day_of_era / 1_460) + (day_of_era / 36_524) - (day_of_era / 146_096)) / 365;
    let year = year_of_era + (era * 400);
    let day_of_year = day_of_era - ((365 * year_of_era) + (year_of_era / 4) - (year_of_era / 100));
    let month_prime = ((5 * day_of_year) + 2) / 153;
    let day = day_of_year - ((153 * month_prime) + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };

    (year, month as u32, day as u32, hour, minute)
}

#[cfg(test)]
#[path = "repository_browser_tests.rs"]
mod tests;
