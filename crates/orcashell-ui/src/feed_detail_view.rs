use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use orcashell_git::{DiffLineKind, DiffLineView, DiffSectionKind, DiffSelectionKey};
use orcashell_terminal_view::Copy;

use crate::diff_explorer::{
    self, build_diff_tree, collect_file_keys, extract_selected_text, is_copy_keystroke,
    is_oversize_document, measure_diff_char_width, plain_text_for_line, plain_text_len,
    render_diff_line_element, simple_hash, CachedDiffLines, DiffSelection, DiffTreeNode,
    DiffTreeNodeKind, ScrollbarDrag, DEFAULT_CHAR_WIDTH, DIFF_FONT_FAMILY, LINE_HEIGHT,
    SCROLLBAR_HIT_WIDTH, SCROLLBAR_THUMB_INSET, SCROLLBAR_THUMB_MIN, SCROLLBAR_THUMB_WIDTH,
    TEXT_COL_X,
};
use crate::theme::{self, OrcaTheme};
use crate::workspace::{CapturedDiffFailure, CapturedDiffFile, ChangeFeedEntry, FeedCaptureState};

const MIN_TREE_WIDTH: f32 = 180.0;
const MIN_DIFF_WIDTH: f32 = 320.0;
const DEFAULT_TREE_WIDTH: f32 = 240.0;

#[derive(Debug, Clone)]
struct DetailResizeDrag {
    initial_mouse_x: f32,
    initial_width: f32,
}

#[derive(Debug, Clone)]
struct CapturedTreeCache {
    staged_tree: Vec<DiffTreeNode>,
    unstaged_tree: Vec<DiffTreeNode>,
    visible_file_order: Rc<Vec<DiffSelectionKey>>,
    staged_count: usize,
    unstaged_count: usize,
}

pub struct FeedDetailView {
    entry: ChangeFeedEntry,
    focus_handle: FocusHandle,
    tree_cache: CapturedTreeCache,
    selected_file: Option<DiffSelectionKey>,
    diff_scroll_handle: UniformListScrollHandle,
    diff_scrollbar_drag: Option<ScrollbarDrag>,
    diff_list_bounds: Rc<RefCell<Bounds<Pixels>>>,
    diff_scroll_x: f32,
    selection: Option<DiffSelection>,
    measured_char_width: f32,
    line_cache: Option<CachedDiffLines>,
    tree_width: f32,
    resize_drag: Option<DetailResizeDrag>,
    bounds: Rc<RefCell<Bounds<Pixels>>>,
}

impl FeedDetailView {
    pub fn new(entry: ChangeFeedEntry, cx: &mut Context<Self>) -> Self {
        let tree_cache = build_captured_tree_cache(&entry);
        let selected_file = tree_cache.visible_file_order.first().cloned();
        let mut this = Self {
            entry,
            focus_handle: cx.focus_handle(),
            tree_cache,
            selected_file,
            diff_scroll_handle: UniformListScrollHandle::new(),
            diff_scrollbar_drag: None,
            diff_list_bounds: Rc::new(RefCell::new(Bounds::default())),
            diff_scroll_x: 0.0,
            selection: None,
            measured_char_width: DEFAULT_CHAR_WIDTH,
            line_cache: None,
            tree_width: DEFAULT_TREE_WIDTH,
            resize_drag: None,
            bounds: Rc::new(RefCell::new(Bounds::default())),
        };
        this.rebuild_line_cache();
        this
    }

    pub fn invalidate_theme_cache(&mut self, cx: &mut Context<Self>) {
        self.rebuild_line_cache();
        cx.notify();
    }

    pub fn update_entry(&mut self, entry: ChangeFeedEntry, cx: &mut Context<Self>) {
        let tree_cache = build_captured_tree_cache(&entry);
        let selected_still_visible = self.selected_file.as_ref().is_some_and(|selected| {
            tree_cache
                .visible_file_order
                .iter()
                .any(|candidate| candidate == selected)
        });

        self.entry = entry;
        self.tree_cache = tree_cache;

        if !selected_still_visible {
            self.selected_file = self.tree_cache.visible_file_order.first().cloned();
            self.selection = None;
            self.diff_scroll_x = 0.0;
            self.diff_scroll_handle = UniformListScrollHandle::new();
            self.diff_scrollbar_drag = None;
        }

        self.rebuild_line_cache();
        cx.notify();
    }

    fn captured_files(&self) -> &[CapturedDiffFile] {
        match &self.entry.capture_state {
            FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) => {
                &captured.files
            }
            _ => &[],
        }
    }

    fn captured_failures(&self) -> &[CapturedDiffFailure] {
        match &self.entry.capture_state {
            FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) => {
                &captured.failed_files
            }
            _ => &[],
        }
    }

    fn captured_file_for_selection(
        &self,
        selection: &DiffSelectionKey,
    ) -> Option<&CapturedDiffFile> {
        self.captured_files()
            .iter()
            .find(|captured| captured.selection == *selection)
    }

    fn select_file(&mut self, selection: DiffSelectionKey) {
        if self.selected_file.as_ref() == Some(&selection) {
            return;
        }
        self.selected_file = Some(selection);
        self.selection = None;
        self.diff_scroll_x = 0.0;
        self.diff_scroll_handle = UniformListScrollHandle::new();
        self.diff_scrollbar_drag = None;
        self.rebuild_line_cache();
    }

    fn rebuild_line_cache(&mut self) {
        let Some(selection) = self.selected_file.as_ref() else {
            self.line_cache = None;
            return;
        };
        let Some(captured) = self.captured_file_for_selection(selection) else {
            self.line_cache = None;
            return;
        };

        let lines: Vec<DiffLineView> = captured
            .document
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
        self.line_cache = Some(CachedDiffLines {
            selection: selection.clone(),
            generation: captured.document.generation,
            lines: Rc::new(lines),
            max_line_chars,
        });
    }

    fn measure_char_width(&mut self, window: &mut Window) {
        self.measured_char_width = measure_diff_char_width(window);
    }

    fn render_tree_pane(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let palette = theme::active(cx);
        let pane = div()
            .id("feed-detail-tree")
            .w(px(self.tree_width))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(palette.DEEP))
            .border_r_1()
            .border_color(rgb(palette.SURFACE))
            .overflow_y_scroll();

        if self.tree_cache.visible_file_order.is_empty() {
            return pane.child(diff_explorer::DiffExplorerView::empty_panel_message(
                &palette,
                "No files captured",
                Some("This event has no captured file diffs."),
            ));
        }

        let mut list = div().w_full().flex().flex_col();

        if self.tree_cache.staged_count > 0 {
            list = list.child(diff_explorer::DiffExplorerView::section_header(
                &palette,
                "STAGED CHANGES",
                self.tree_cache.staged_count,
            ));
            list = list.children(self.render_section_tree(
                &self.tree_cache.staged_tree,
                0,
                DiffSectionKind::Staged,
                cx,
            ));
        }

        list = list.child(diff_explorer::DiffExplorerView::section_header(
            &palette,
            "CHANGES",
            self.tree_cache.unstaged_count,
        ));
        list = list.children(self.render_section_tree(
            &self.tree_cache.unstaged_tree,
            0,
            DiffSectionKind::Unstaged,
            cx,
        ));

        if matches!(self.entry.capture_state, FeedCaptureState::Truncated(_)) {
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
                    .child("Capture truncated at limit"),
            );
        }

        for failure in self.captured_failures() {
            list = list.child(
                div()
                    .m(px(8.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.STATUS_AMBER))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .font_family(DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(failure.relative_path.display().to_string())
                    .child(div().text_color(rgb(palette.FOG)).child("unavailable")),
            );
        }

        pane.child(list)
    }

    fn render_section_tree(
        &self,
        nodes: &[DiffTreeNode],
        depth: usize,
        section: DiffSectionKind,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let palette = theme::active(cx);
        let mut rows = Vec::new();
        for node in nodes {
            match &node.kind {
                DiffTreeNodeKind::Directory => {
                    rows.push(
                        diff_explorer::DiffExplorerView::directory_tree_row(
                            &palette, &node.name, depth,
                        )
                        .into_any_element(),
                    );
                    rows.extend(self.render_section_tree(&node.children, depth + 1, section, cx));
                }
                DiffTreeNodeKind::File(file) => {
                    let key = DiffSelectionKey {
                        section,
                        relative_path: file.relative_path.clone(),
                    };
                    let path_hash = simple_hash(file.relative_path.to_string_lossy().as_ref());
                    let element_id =
                        ElementId::Name(format!("feed-detail-{:?}-{}", section, path_hash).into());
                    let mut row = diff_explorer::DiffExplorerView::file_tree_row(
                        &palette,
                        element_id,
                        &node.name,
                        depth,
                        file,
                        self.selected_file.as_ref() == Some(&key),
                        false,
                    );
                    row =
                        row.on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.select_file(key.clone());
                            cx.notify();
                        }));
                    rows.push(row.into_any_element());
                }
            }
        }
        rows
    }

    fn render_diff_pane(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let palette = theme::active(cx);
        let pane_id = ElementId::Name("feed-detail-diff-body".into());

        let bare_pane = || {
            div()
                .id(pane_id.clone())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(rgb(palette.ABYSS))
        };

        let Some(selected_file) = self.selected_file.as_ref() else {
            return bare_pane().child(diff_explorer::DiffExplorerView::empty_panel_message(
                &palette,
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        let Some(captured) = self.captured_file_for_selection(selected_file) else {
            return bare_pane().child(diff_explorer::DiffExplorerView::empty_panel_message(
                &palette,
                "File no longer available",
                None,
            ));
        };

        if is_oversize_document(&captured.document) {
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
                            .child(captured.file.relative_path.display().to_string()),
                    )
                    .child(diff_explorer::DiffExplorerView::status_summary(
                        &palette,
                        &captured.file,
                    ))
                    .child(diff_explorer::DiffExplorerView::empty_panel_message(
                        &palette,
                        "Diff too large",
                        Some("Open the file in another tool if you need the full patch."),
                    )),
            );
        }

        let Some(cached) = self.line_cache.as_ref() else {
            return bare_pane().child(diff_explorer::DiffExplorerView::empty_panel_message(
                &palette,
                "Select a file",
                Some("Pick a changed file from the left pane."),
            ));
        };

        let file_header =
            diff_explorer::DiffExplorerView::diff_file_header(&palette, &captured.file, None);
        let lines = cached.lines.clone();
        let line_count = lines.len();
        let selection = self.selection;
        let scroll_x = self.diff_scroll_x;
        let line_palette = palette.clone();

        let diff_list = uniform_list(
            "feed-detail-diff-list",
            line_count,
            move |range, _window, _cx| {
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
            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                let (line, col) = this.hit_test_diff(event.position);
                if event.click_count >= 3 {
                    this.selection = this.line_length(line).map(|len| DiffSelection {
                        start: (line, 0),
                        end: (line, len),
                        is_selecting: false,
                    });
                } else if event.click_count == 2 {
                    this.selection =
                        this.word_bounds_at(line, col)
                            .map(|(start, end)| DiffSelection {
                                start: (line, start),
                                end: (line, end),
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
            let delta_x: f32 = match event.delta {
                ScrollDelta::Pixels(d) => f32::from(d.x),
                ScrollDelta::Lines(d) => d.x * this.measured_char_width * 3.0,
            };
            if delta_x.abs() > 0.5 {
                let viewport_w = f32::from(this.diff_list_bounds.borrow().size.width);
                let max_line_w = this
                    .line_cache
                    .as_ref()
                    .map(|cache| {
                        cache.max_line_chars as f32 * this.measured_char_width + TEXT_COL_X
                    })
                    .unwrap_or(0.0);
                let max_scroll = (max_line_w - viewport_w).max(0.0);
                this.diff_scroll_x = (this.diff_scroll_x - delta_x).clamp(0.0, max_scroll);
                cx.notify();
            }
        }));

        let list_bounds = self.diff_list_bounds.clone();
        let scrollbar = self
            .diff_scrollbar_geometry()
            .map(|(thumb_y, thumb_height)| {
                let is_dragging = self.diff_scrollbar_drag.is_some();
                self.render_scrollbar(thumb_y, thumb_height, is_dragging, &palette, cx)
            });

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
            .id("feed-detail-scrollbar")
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

    fn line_length(&self, line_index: usize) -> Option<usize> {
        self.with_filtered_line(line_index, |line| {
            plain_text_len(&line.text, line.highlights.as_deref())
        })
    }

    fn word_bounds_at(&self, line_index: usize, col: usize) -> Option<(usize, usize)> {
        self.with_filtered_line(line_index, |line| {
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

    fn with_filtered_line<R>(
        &self,
        line_index: usize,
        f: impl FnOnce(&DiffLineView) -> R,
    ) -> Option<R> {
        let cache = self.line_cache.as_ref()?;
        let line = cache.lines.get(line_index)?;
        Some(f(line))
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(sel) = self.selection else {
            return;
        };
        let ((start_line, start_col), (end_line, end_col)) = sel.normalized();
        let text = self.line_cache.as_ref().and_then(|cache| {
            let lines: Vec<&DiffLineView> = cache.lines.iter().collect();
            extract_selected_text(&lines, start_line, start_col, end_line, end_col)
        });
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
}

impl Render for FeedDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        self.measure_char_width(window);

        let bounds_ref = self.bounds.clone();

        div()
            .id("feed-detail-view")
            .size_full()
            .bg(rgb(palette.SURFACE))
            .p(px(1.0))
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
                if let Some(drag) = this.resize_drag.as_ref() {
                    let total_width = f32::from(this.bounds.borrow().size.width);
                    let max_width = (total_width - MIN_DIFF_WIDTH).max(MIN_TREE_WIDTH);
                    let next_width = (drag.initial_width
                        + (f32::from(event.position.x) - drag.initial_mouse_x))
                        .clamp(MIN_TREE_WIDTH, max_width);
                    this.tree_width = next_width;
                    cx.notify();
                    return;
                }

                if this
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.is_selecting)
                {
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
                    if let Some(selection) = this.selection.as_mut() {
                        if selection.is_selecting {
                            selection.is_selecting = false;
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .bg(rgb(palette.DEEP))
                    .p(px(3.0))
                    .child(
                        div()
                            .size_full()
                            .bg(rgb(palette.ABYSS))
                            .flex()
                            .flex_row()
                            .child(self.render_tree_pane(cx))
                            .child(
                                div()
                                    .w(px(4.0))
                                    .h_full()
                                    .bg(rgb(palette.CURRENT))
                                    .cursor_col_resize()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                            this.resize_drag = Some(DetailResizeDrag {
                                                initial_mouse_x: f32::from(event.position.x),
                                                initial_width: this.tree_width,
                                            });
                                            cx.stop_propagation();
                                        }),
                                    ),
                            )
                            .child(self.render_diff_pane(cx)),
                    ),
            )
    }
}

fn build_captured_tree_cache(entry: &ChangeFeedEntry) -> CapturedTreeCache {
    let mut staged_files = Vec::new();
    let mut unstaged_files = Vec::new();

    if let FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) =
        &entry.capture_state
    {
        for file in &captured.files {
            match file.selection.section {
                DiffSectionKind::Staged => staged_files.push(file.file.clone()),
                DiffSectionKind::Unstaged => unstaged_files.push(file.file.clone()),
            }
        }
    }

    let staged_tree = build_diff_tree(&staged_files);
    let unstaged_tree = build_diff_tree(&unstaged_files);
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

    CapturedTreeCache {
        staged_count: staged_files.len(),
        unstaged_count: unstaged_files.len(),
        staged_tree,
        unstaged_tree,
        visible_file_order: Rc::new(visible_file_order),
    }
}
