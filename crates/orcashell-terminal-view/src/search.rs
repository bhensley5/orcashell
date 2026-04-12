use std::ops::{Range, RangeInclusive};
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Line, Point as AlacPoint};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::Term;
use gpui::*;

/// Escape regex metacharacters for plain-text search.
/// Preserves alacritty's smart case-insensitivity (case-insensitive unless
/// the query contains uppercase characters).
pub fn escape_regex(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        if "\\.*+?()[]{}^$|".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// A search match filtered to the visible viewport, ready for the renderer.
#[derive(Clone, Debug)]
pub struct VisibleMatch {
    pub range: RangeInclusive<AlacPoint>,
    pub is_current: bool,
}

/// Collect all matches in the terminal buffer using alacritty's RegexIter.
fn collect_all_matches<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let grid = term.grid();
    let topmost = AlacPoint::new(Line(-(grid.history_size() as i32)), Column(0));
    let bottommost = AlacPoint::new(
        Line(grid.screen_lines() as i32 - 1),
        Column(grid.last_column().0),
    );

    RegexIter::new(topmost, bottommost, Direction::Right, term, regex).collect()
}

/// Search state held by TerminalView.
pub struct SearchState {
    pub input: Entity<TextInputState>,
    pub all_matches: Vec<Match>,
    pub current_index: Option<usize>,
    query: String,
}

impl SearchState {
    pub fn new(input: Entity<TextInputState>) -> Self {
        Self {
            input,
            all_matches: Vec::new(),
            current_index: None,
            query: String::new(),
        }
    }

    /// Re-run the search with the current query against the terminal buffer.
    pub fn update_search<T>(&mut self, term: &Term<T>, query: &str) {
        self.query = query.to_string();

        if query.is_empty() {
            self.all_matches.clear();
            self.current_index = None;
            return;
        }

        let escaped = escape_regex(query);
        match RegexSearch::new(&escaped) {
            Ok(mut regex) => {
                self.all_matches = collect_all_matches(term, &mut regex);
                self.current_index = if self.all_matches.is_empty() {
                    None
                } else {
                    Some(0)
                };
            }
            Err(_) => {
                self.all_matches.clear();
                self.current_index = None;
            }
        }
    }

    /// Move to next match with wraparound.
    pub fn next_match(&mut self) {
        if let Some(idx) = self.current_index {
            self.current_index = Some((idx + 1) % self.all_matches.len());
        }
    }

    /// Move to previous match with wraparound.
    pub fn prev_match(&mut self) {
        if let Some(idx) = self.current_index {
            self.current_index = Some(if idx == 0 {
                self.all_matches.len() - 1
            } else {
                idx - 1
            });
        }
    }

    /// Get the start point of the current match (for scroll_to_point).
    pub fn current_match_point(&self) -> Option<AlacPoint> {
        self.current_index
            .and_then(|idx| self.all_matches.get(idx))
            .map(|m| *m.start())
    }

    /// Build visible match list for the renderer.
    pub fn visible_matches(&self, display_offset: usize, screen_lines: usize) -> Vec<VisibleMatch> {
        build_visible_matches(
            &self.all_matches,
            self.current_index,
            display_offset,
            screen_lines,
        )
    }
}

pub fn build_visible_matches(
    all_matches: &[Match],
    current_index: Option<usize>,
    display_offset: usize,
    screen_lines: usize,
) -> Vec<VisibleMatch> {
    let top_buffer_line = -(display_offset as i32);
    let bottom_buffer_line = screen_lines as i32 - 1 - display_offset as i32;

    all_matches
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| {
            let start_line = m.start().line.0;
            let end_line = m.end().line.0;
            // Match overlaps viewport if start <= bottom AND end >= top
            if start_line <= bottom_buffer_line && end_line >= top_buffer_line {
                Some(VisibleMatch {
                    range: m.clone(),
                    is_current: current_index == Some(idx),
                })
            } else {
                None
            }
        })
        .collect()
}

impl SearchState {
    /// Match count text for UI display.
    pub fn match_count_text(&self) -> String {
        if self.all_matches.is_empty() {
            if self.query.is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            }
        } else {
            let idx = self.current_index.map(|i| i + 1).unwrap_or(0);
            format!("{} of {}", idx, self.all_matches.len())
        }
    }
}

/// Event emitted when search input text changes.
pub struct SearchQueryChanged;

/// GPUI entity for the search text input.
/// Uses StyledText + TextLayout for pixel-perfect cursor positioning,
/// click-to-position, and drag-to-select (modeled on Okena's SimpleInputState).
pub struct TextInputState {
    focus_handle: FocusHandle,
    value: String,
    cursor_pos: usize,
    selection: Option<Range<usize>>,
    cursor_visible: bool,
    /// TextLayout from the last render. Used for click-to-position.
    text_layout: Option<TextLayout>,
    /// Bounds of the input container. Used for mouse coordinate mapping.
    input_bounds: Option<Bounds<Pixels>>,
    /// Horizontal scroll offset to keep cursor visible in the clipped container.
    scroll_offset: Pixels,
    /// Whether the user is currently dragging to select text.
    is_selecting: bool,
    /// Anchor position (char offset) for drag selection.
    select_anchor: usize,
    // Colors from the palette
    text_color: Hsla,
    placeholder_color: Hsla,
    cursor_color: Hsla,
    selection_bg: Hsla,
    /// Placeholder text shown when value is empty.
    placeholder_text: String,
    #[allow(dead_code)]
    _blink_task: Task<()>,
}

impl EventEmitter<SearchQueryChanged> for TextInputState {}

impl TextInputState {
    pub fn new(
        text_color: Hsla,
        placeholder_color: Hsla,
        cursor_color: Hsla,
        selection_bg: Hsla,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        let blink_task = cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                Timer::after(Duration::from_millis(530)).await;
                let result = this.update(cx, |state, cx| {
                    state.cursor_visible = !state.cursor_visible;
                    cx.notify();
                });
                if result.is_err() {
                    break;
                }
            },
        );

        Self {
            focus_handle,
            value: String::new(),
            cursor_pos: 0,
            selection: None,
            cursor_visible: true,
            text_layout: None,
            input_bounds: None,
            scroll_offset: px(0.0),
            is_selecting: false,
            select_anchor: 0,
            text_color,
            placeholder_color,
            cursor_color,
            selection_bg,
            placeholder_text: "Search...".to_string(),
            _blink_task: blink_task,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// Set the placeholder text shown when the input is empty.
    pub fn set_placeholder(&mut self, text: &str) {
        self.placeholder_text = text.to_string();
    }

    /// Set the input value and move cursor to end. Select all text.
    pub fn set_value(&mut self, text: &str) {
        self.value = text.to_string();
        let len = self.char_count();
        self.cursor_pos = len;
        self.selection = Some(0..len);
        self.reset_blink();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    fn reset_blink(&mut self) {
        self.cursor_visible = true;
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_offset(&self, char_pos: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    fn byte_range_for_chars(&self, range: &Range<usize>) -> Range<usize> {
        self.byte_offset(range.start)..self.byte_offset(range.end)
    }

    pub fn insert_text(&mut self, text: &str) {
        // Delete selection first if any
        if let Some(range) = self.selection.take() {
            self.value
                .replace_range(self.byte_range_for_chars(&range), "");
            self.cursor_pos = range.start;
        }
        let byte_pos = self.byte_offset(self.cursor_pos);
        self.value.insert_str(byte_pos, text);
        self.cursor_pos += text.chars().count();
        self.reset_blink();
    }

    fn delete_backward(&mut self) {
        if let Some(range) = self.selection.take() {
            self.value
                .replace_range(self.byte_range_for_chars(&range), "");
            self.cursor_pos = range.start;
        } else if self.cursor_pos > 0 {
            let prev = self.cursor_pos - 1;
            let start = self.byte_offset(prev);
            let end = self.byte_offset(self.cursor_pos);
            self.value.replace_range(start..end, "");
            self.cursor_pos = prev;
        }
        self.reset_blink();
    }

    fn delete_forward(&mut self) {
        if let Some(range) = self.selection.take() {
            self.value
                .replace_range(self.byte_range_for_chars(&range), "");
            self.cursor_pos = range.start;
        } else if self.cursor_pos < self.char_count() {
            let start = self.byte_offset(self.cursor_pos);
            let end = self.byte_offset(self.cursor_pos + 1);
            self.value.replace_range(start..end, "");
        }
        self.reset_blink();
    }

    fn move_cursor_left(&mut self, extend_selection: bool) {
        if self.cursor_pos > 0 {
            let old = self.cursor_pos;
            self.cursor_pos -= 1;
            if extend_selection {
                self.extend_selection(old, self.cursor_pos);
            } else if self.selection.is_some() {
                self.cursor_pos = self.selection.as_ref().unwrap().start;
                self.selection = None;
            } else {
                self.selection = None;
            }
        } else if !extend_selection {
            self.selection = None;
        }
        self.reset_blink();
    }

    fn move_cursor_right(&mut self, extend_selection: bool) {
        let len = self.char_count();
        if self.cursor_pos < len {
            let old = self.cursor_pos;
            self.cursor_pos += 1;
            if extend_selection {
                self.extend_selection(old, self.cursor_pos);
            } else if self.selection.is_some() {
                self.cursor_pos = self.selection.as_ref().unwrap().end;
                self.selection = None;
            } else {
                self.selection = None;
            }
        } else if !extend_selection {
            self.selection = None;
        }
        self.reset_blink();
    }

    fn extend_selection(&mut self, anchor: usize, new_pos: usize) {
        let (start, end) = if let Some(ref sel) = self.selection {
            if anchor == sel.end {
                if new_pos < sel.start {
                    (new_pos, sel.start)
                } else {
                    (sel.start, new_pos)
                }
            } else if new_pos > sel.end {
                (sel.end, new_pos)
            } else {
                (new_pos, sel.end)
            }
        } else {
            (anchor.min(new_pos), anchor.max(new_pos))
        };
        if start != end {
            self.selection = Some(start..end);
        } else {
            self.selection = None;
        }
    }

    fn select_all(&mut self) {
        let len = self.char_count();
        if len > 0 {
            self.selection = Some(0..len);
            self.cursor_pos = len;
        }
        self.reset_blink();
    }

    /// Resolve a mouse position to a char offset using stored TextLayout.
    fn char_position_for_mouse(&self, position: Point<Pixels>) -> usize {
        let char_count = self.char_count();
        if let Some(ref layout) = self.text_layout {
            layout
                .index_for_position(position)
                .unwrap_or_else(|ix| ix)
                .min(char_count)
        } else {
            char_count
        }
    }

    /// Select the word around a given char position.
    fn select_word_at(&mut self, pos: usize) {
        let chars: Vec<char> = self.value.chars().collect();
        let len = chars.len();
        if len == 0 {
            return;
        }
        let p = pos.min(len.saturating_sub(1));
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = p;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = p;
        while end < len && is_word(chars[end]) {
            end += 1;
        }
        if start != end {
            self.selection = Some(start..end);
            self.cursor_pos = end;
        }
        self.reset_blink();
    }

    /// Copy the current selection to the clipboard.
    pub fn copy_selection(&self, cx: &mut Context<Self>) {
        if let Some(ref sel) = self.selection {
            let byte_range = self.byte_range_for_chars(sel);
            let text = &self.value[byte_range];
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    /// Handle key down. Returns true if text changed (caller should emit SearchQueryChanged).
    pub fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;
        let shift = mods.shift;

        match key {
            "backspace" => {
                self.delete_backward();
                cx.notify();
                true
            }
            "delete" => {
                self.delete_forward();
                cx.notify();
                true
            }
            "left" => {
                if mods.platform {
                    let old = self.cursor_pos;
                    self.cursor_pos = 0;
                    if shift {
                        self.extend_selection(old, 0);
                    } else {
                        self.selection = None;
                    }
                    self.reset_blink();
                } else {
                    self.move_cursor_left(shift);
                }
                cx.notify();
                false
            }
            "right" => {
                if mods.platform {
                    let old = self.cursor_pos;
                    let len = self.char_count();
                    self.cursor_pos = len;
                    if shift {
                        self.extend_selection(old, len);
                    } else {
                        self.selection = None;
                    }
                    self.reset_blink();
                } else {
                    self.move_cursor_right(shift);
                }
                cx.notify();
                false
            }
            "a" if mods.platform || mods.control => {
                self.select_all();
                cx.notify();
                false
            }
            "c" if mods.platform || mods.control => {
                if let Some(ref sel) = self.selection {
                    let byte_range = self.byte_range_for_chars(sel);
                    let text = &self.value[byte_range];
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                }
                false
            }
            "x" if mods.platform || mods.control => {
                if let Some(ref sel) = self.selection {
                    let byte_range = self.byte_range_for_chars(sel);
                    let text = self.value[byte_range].to_string();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    // delete selection
                    self.delete_backward();
                    cx.notify();
                    return true;
                }
                false
            }
            "v" if mods.platform || mods.control => {
                if let Some(item) = cx.read_from_clipboard() {
                    if let Some(text) = item.text() {
                        let line = text.lines().next().unwrap_or("");
                        if !line.is_empty() {
                            self.insert_text(line);
                            cx.notify();
                            return true;
                        }
                    }
                }
                false
            }
            "enter" | "escape" => false,
            "shift" | "control" | "alt" | "meta" | "capslock" | "f1" | "f2" | "f3" | "f4"
            | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" | "up" | "down"
            | "pageup" | "pagedown" | "home" | "end" | "tab" => false,
            _ => {
                if let Some(ref s) = event.keystroke.key_char {
                    if !s.is_empty() && !s.chars().next().is_none_or(|c| c.is_control()) {
                        self.insert_text(s);
                        cx.notify();
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// Canvas element that paints a cursor line at the position from a TextLayout.
fn cursor_canvas(
    layout: TextLayout,
    cursor_byte: usize,
    visible: bool,
    color: Hsla,
) -> impl IntoElement {
    canvas(
        move |_bounds, _window, _cx| {
            let pos = layout.position_for_index(cursor_byte);
            let line_h = layout.line_height();
            (pos, line_h)
        },
        move |_bounds, (cursor_pos, line_h), window, _cx| {
            if visible {
                if let Some(pos) = cursor_pos {
                    let cursor_h = px(14.0).min(line_h);
                    let y_offset = (line_h - cursor_h) * 0.5;
                    let adjusted = point(pos.x, pos.y + y_offset);
                    window.paint_quad(fill(Bounds::new(adjusted, size(px(1.0), cursor_h)), color));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

impl Render for TextInputState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_focused = self.focus_handle.is_focused(window);
        let text_color = self.text_color;
        let placeholder_color: Hsla = self.placeholder_color;
        let cursor_color = self.cursor_color;
        let selection_bg = self.selection_bg;
        let cursor_visible = self.cursor_visible && is_focused;
        let cursor_byte = self.byte_offset(self.cursor_pos);

        // Compute scroll offset using the PREVIOUS frame's text layout.
        // On first render text_layout is None, so scroll_offset stays 0.
        if let (Some(ref layout), Some(ref bounds)) = (&self.text_layout, &self.input_bounds) {
            if let Some(cursor_pos) = layout.position_for_index(cursor_byte) {
                let container_width = bounds.size.width - px(16.0); // account for px padding
                let cursor_x = cursor_pos.x - bounds.origin.x + self.scroll_offset;

                // If cursor is past the right edge, scroll right
                if cursor_x > container_width {
                    self.scroll_offset -= cursor_x - container_width + px(8.0);
                }
                // If cursor is before the left edge, scroll left
                if cursor_x < px(0.0) {
                    self.scroll_offset -= cursor_x - px(8.0);
                }
                // Don't scroll past the start
                if self.scroll_offset > px(0.0) {
                    self.scroll_offset = px(0.0);
                }
            }
        }
        if self.value.is_empty() {
            self.scroll_offset = px(0.0);
        }

        let scroll_offset = self.scroll_offset;

        // Build text content. StyledText must be a child so GPUI lays it out
        // before the sibling cursor_canvas reads position_for_index in prepaint.
        let content: AnyElement = if self.value.is_empty() {
            let placeholder = StyledText::new(self.placeholder_text.clone());
            let layout = placeholder.layout().clone();
            self.text_layout = None;

            div()
                .relative()
                .text_color(placeholder_color)
                .child(placeholder)
                .child(cursor_canvas(layout, 0, cursor_visible, cursor_color))
                .into_any_element()
        } else {
            let styled = if let Some(ref sel) = self.selection {
                let sel_start_byte = self.byte_offset(sel.start);
                let sel_end_byte = self.byte_offset(sel.end);
                StyledText::new(self.value.clone()).with_highlights(vec![(
                    sel_start_byte..sel_end_byte,
                    HighlightStyle {
                        background_color: Some(selection_bg),
                        ..Default::default()
                    },
                )])
            } else {
                StyledText::new(self.value.clone())
            };

            let layout = styled.layout().clone();
            self.text_layout = Some(layout.clone());

            div()
                .relative()
                .left(scroll_offset)
                .text_color(text_color)
                .child(styled)
                .child(cursor_canvas(
                    layout,
                    cursor_byte,
                    cursor_visible,
                    cursor_color,
                ))
                .into_any_element()
        };

        div()
            .id("search-input")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .items_center()
            .w_full()
            .h(px(24.0))
            .text_size(px(13.0))
            .cursor_text()
            .overflow_x_hidden()
            // Bounds tracking canvas
            .child(
                canvas(
                    {
                        let entity = cx.entity().downgrade();
                        move |bounds, _, cx: &mut App| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, _| {
                                    this.input_bounds = Some(bounds);
                                });
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // Mouse handlers
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.focus(window);
                    let pos = this.char_position_for_mouse(event.position);
                    if event.click_count >= 2 {
                        this.is_selecting = false;
                        this.select_word_at(pos);
                    } else {
                        this.cursor_pos = pos;
                        this.selection = None;
                        this.is_selecting = true;
                        this.select_anchor = pos;
                        this.reset_blink();
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.is_selecting {
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.is_selecting = false;
                        return;
                    }
                    let pos = this.char_position_for_mouse(event.position);
                    this.cursor_pos = pos;
                    let anchor = this.select_anchor;
                    if pos != anchor {
                        this.selection = Some(anchor.min(pos)..anchor.max(pos));
                    } else {
                        this.selection = None;
                    }
                    this.reset_blink();
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.is_selecting = false;
                }),
            )
            .child(content)
    }
}

#[cfg(test)]
mod tests;
