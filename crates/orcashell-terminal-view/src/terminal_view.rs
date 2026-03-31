use crate::actions;
use crate::colors::ColorPalette;
use crate::input::{
    key_input_to_bytes, modifier_key_to_bytes, wrap_bracketed_paste, KeyEventType, KeyInput,
    KITTY_CAPS_LOCK, KITTY_LEFT_ALT, KITTY_LEFT_CONTROL, KITTY_LEFT_SHIFT, KITTY_LEFT_SUPER,
};
use crate::links::{
    buffer_point_for_cell, build_visible_hovered_link, detect_link_at_point,
    hyperlink_activation_modifier_pressed, is_hyperlink_activation_click, open_hyperlink_uri,
    DetectedLink, PlaintextLinkMatcher,
};
use crate::mouse;
use crate::renderer::{scrollbar_geometry, TerminalRenderer, SCROLLBAR_HIT_WIDTH};
use crate::search::{
    build_visible_matches, SearchQueryChanged, SearchState, TextInputState, VisibleMatch,
};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::Selection as AlacSelection;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use gpui::*;
use orcashell_session::dimensions::TermDimensions;
use orcashell_session::engine::SessionEngine;
use orcashell_session::event::SessionEvent;
use orcashell_session::SemanticState;
use portable_pty::PtySize;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

/// Cursor shape for rendering. Mapped from the store's `CursorStyle` by the UI layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

#[derive(Clone, Debug)]
pub struct TerminalConfig {
    pub font_family: String,
    pub font_size: Pixels,
    pub line_height_multiplier: f32,
    pub padding: Edges<Pixels>,
    pub colors: ColorPalette,
    pub terminated_text_color: Hsla,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: "Menlo".into(),
            font_size: px(13.0),
            line_height_multiplier: 1.0,
            padding: Edges::all(px(0.0)),
            colors: ColorPalette::default(),
            // FOG from orcashell-ui tokens (0x9499A8)
            terminated_text_color: Rgba {
                r: 0x94 as f32 / 255.0,
                g: 0x99 as f32 / 255.0,
                b: 0xA8 as f32 / 255.0,
                a: 1.0,
            }
            .into(),
            cursor_shape: CursorShape::Bar,
            cursor_blink: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalRuntimeEvent {
    ActivityChanged {
        terminal_id: String,
    },
    LocalInput {
        terminal_id: String,
    },
    TitleChanged {
        terminal_id: String,
        title: Option<String>,
    },
    SemanticStateChanged {
        terminal_id: String,
        state: SemanticState,
    },
    Notification {
        terminal_id: String,
        title: String,
        body: String,
    },
    Bell {
        terminal_id: String,
    },
}

impl EventEmitter<TerminalRuntimeEvent> for TerminalView {}

pub struct TerminalView {
    terminal_id: String,
    shell_type: orcashell_session::ShellType,
    engine: SessionEngine,
    renderer: TerminalRenderer,
    focus_handle: FocusHandle,
    config: TerminalConfig,
    /// Base font size from settings (before per-terminal zoom offset).
    base_font_size: Pixels,
    // Cached cell dimensions. Re-measured only on font change
    cell_width: Pixels,
    cell_height: Pixels,
    cell_measured: bool,
    cursor_visible: bool,
    cursor_blink_paused: bool,
    last_input_time: std::time::Instant,
    terminated: bool,
    /// Accumulated sub-line scroll pixels from trackpad.
    scroll_px: f32,
    /// Canvas bounds in window coordinates. Updated every paint, used by mouse handlers
    /// to convert window-space event positions to terminal-relative coordinates.
    canvas_bounds: Rc<RefCell<Bounds<Pixels>>>,
    /// Active search state. None = search bar hidden.
    search: Option<SearchState>,
    #[allow(dead_code)]
    _search_subscription: Option<Subscription>,
    #[allow(dead_code)]
    _wakeup_task: Task<()>,
    #[allow(dead_code)]
    _blink_task: Task<()>,
    /// Previous modifier state for detecting modifier key press/release
    /// in `on_modifiers_changed`.
    prev_modifiers: Modifiers,
    /// Previous CapsLock state for detecting toggles.
    prev_capslock: bool,
    /// Whether a scrollbar thumb drag is in progress.
    scrollbar_dragging: bool,
    /// Drag start state: (start_y_px, start_display_offset).
    scrollbar_drag_start: Option<(f32, usize)>,
    /// Repeating timer for auto-scrolling during edge drag. Dropped = cancelled.
    auto_scroll_task: Option<Task<()>>,
    /// When the current auto-scroll began (for time-based acceleration).
    auto_scroll_start: Option<std::time::Instant>,
    /// Last mouse position during selection drag, for auto-scroll direction/speed.
    last_drag_position: Option<Point<Pixels>>,
    /// Last known mouse position inside the terminal viewport.
    last_mouse_position: Option<Point<Pixels>>,
    /// Hovered link while the activation modifier is held.
    hovered_link: Option<DetectedLink>,
    /// Reusable regex state for plain-text URL detection.
    plain_url_matcher: PlaintextLinkMatcher,
}

impl TerminalView {
    fn emit_local_input(&self, cx: &mut Context<Self>) {
        cx.emit(TerminalRuntimeEvent::LocalInput {
            terminal_id: self.terminal_id.clone(),
        });
    }

    pub fn new(
        terminal_id: String,
        shell_type: orcashell_session::ShellType,
        mut engine: SessionEngine,
        config: TerminalConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let renderer = TerminalRenderer::new(
            config.font_family.clone(),
            config.font_size,
            config.line_height_multiplier,
            config.colors.clone(),
        );

        let focus_handle = cx.focus_handle();

        let wake_rx = engine
            .take_wake_rx()
            .expect("wake_rx already taken from engine");

        let wakeup_task = cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                if wake_rx.recv().await.is_err() {
                    // Wake channel closed. Reader thread exited (shell EOF).
                    // Mark terminal as terminated and trigger a final render.
                    let _ = this.update(cx, |view, cx| {
                        view.terminated = true;
                        cx.notify();
                    });
                    break;
                }

                loop {
                    let result = this.update(cx, |view, cx| {
                        if view.terminated {
                            return false;
                        }

                        let processed_pending_bytes = view.engine.process_pending_bytes();
                        if processed_pending_bytes {
                            view.hovered_link = None;
                            cx.emit(TerminalRuntimeEvent::ActivityChanged {
                                terminal_id: view.terminal_id.clone(),
                            });
                            cx.notify();
                        }

                        // Drain session events (notifications, bell, title, etc.)
                        // even if the view isn't being rendered (e.g., unfocused tab).
                        while let Some(event) = view.engine.try_recv_event() {
                            view.handle_session_event(event, cx);
                        }

                        if view.engine.has_pending_bytes() {
                            return true;
                        }

                        view.engine.dirty().store(false, Ordering::Release);

                        if view.engine.has_pending_bytes() {
                            view.engine.dirty().store(true, Ordering::Release);
                            return true;
                        }

                        false
                    });

                    match result {
                        Ok(true) => {
                            Timer::after(std::time::Duration::from_millis(8)).await;
                        }
                        Ok(false) => break,
                        Err(_) => return,
                    }
                }
            },
        );

        let blink_task = cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                Timer::after(std::time::Duration::from_millis(800)).await;
                let result = this.update(cx, |view, cx| {
                    if view.terminated {
                        return;
                    }
                    // Resume blinking after 0.5 seconds of no input
                    if view.cursor_blink_paused
                        && view.last_input_time.elapsed() > std::time::Duration::from_millis(500)
                    {
                        view.cursor_blink_paused = false;
                    }
                    if !view.cursor_blink_paused && view.config.cursor_blink {
                        view.cursor_visible = !view.cursor_visible;
                        cx.notify();
                    }
                });
                if result.is_err() {
                    break;
                }
            },
        );

        let base_font_size = config.font_size;
        Self {
            terminal_id,
            shell_type,
            engine,
            renderer,
            focus_handle,
            config,
            base_font_size,
            cell_width: px(0.0),
            cell_height: px(0.0),
            cell_measured: false,
            cursor_visible: true,
            cursor_blink_paused: false,
            last_input_time: std::time::Instant::now(),
            terminated: false,
            scroll_px: 0.0,
            canvas_bounds: Rc::new(RefCell::new(Bounds::default())),
            search: None,
            _search_subscription: None,
            _wakeup_task: wakeup_task,
            _blink_task: blink_task,
            prev_modifiers: Modifiers::default(),
            prev_capslock: false,
            scrollbar_dragging: false,
            scrollbar_drag_start: None,
            auto_scroll_task: None,
            auto_scroll_start: None,
            last_drag_position: None,
            last_mouse_position: None,
            hovered_link: None,
            plain_url_matcher: PlaintextLinkMatcher::new(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Access the underlying session engine.
    pub fn engine(&self) -> &SessionEngine {
        &self.engine
    }

    /// The current font size (base + any zoom offset).
    pub fn current_font_size(&self) -> Pixels {
        self.config.font_size
    }

    /// Apply new terminal configuration (e.g., from settings changes).
    /// Preserves per-terminal zoom offset relative to the new base font size.
    pub fn apply_config(&mut self, config: TerminalConfig) {
        let zoom_offset = f32::from(self.config.font_size) - f32::from(self.base_font_size);
        self.base_font_size = config.font_size;
        let zoomed_size = config.font_size + px(zoom_offset);
        self.renderer.font_family = config.font_family.clone();
        self.renderer.font_family_shared = config.font_family.clone().into();
        self.renderer.font_size = zoomed_size;
        self.config = config;
        self.config.font_size = zoomed_size;
        self.cell_measured = false;
        if !self.config.cursor_blink {
            self.cursor_visible = true;
        }
    }

    /// Origin for mouse coordinate mapping: canvas bounds origin + padding.
    fn mouse_origin(&self) -> Point<Pixels> {
        let bounds = *self.canvas_bounds.borrow();
        point(
            bounds.origin.x + self.config.padding.left,
            bounds.origin.y + self.config.padding.top,
        )
    }

    fn detect_link_at_position(&mut self, position: Point<Pixels>) -> Option<DetectedLink> {
        let cell = mouse::pixel_to_cell(
            position,
            self.mouse_origin(),
            self.cell_width,
            self.cell_height,
        );
        let term_arc = self.engine.term_arc();
        let term = term_arc.lock();
        let point = buffer_point_for_cell(&term, cell);
        let mut link = detect_link_at_point(&term, point, &mut self.plain_url_matcher)?;
        let cell = &term.grid()[point];
        let mut fg = self.renderer.palette.resolve(cell.fg, term.colors());
        let mut bg = self.renderer.palette.resolve(cell.bg, term.colors());
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg.l *= 0.66;
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        link.underline_color = Some(fg);
        Some(link)
    }

    fn set_hovered_link(&mut self, link: Option<DetectedLink>, cx: &mut Context<Self>) {
        if self.hovered_link != link {
            self.hovered_link = link;
            cx.notify();
        }
    }

    fn refresh_hovered_link(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        if !hyperlink_activation_modifier_pressed(modifiers) {
            self.set_hovered_link(None, cx);
            return;
        }

        let Some(position) = self.last_mouse_position else {
            self.set_hovered_link(None, cx);
            return;
        };

        let link = self.detect_link_at_position(position);
        self.set_hovered_link(link, cx);
    }

    /// Returns true if the given window-space position is within the scrollbar hit region.
    fn is_in_scrollbar_region(&self, position: Point<Pixels>) -> bool {
        let bounds = *self.canvas_bounds.borrow();
        let right_edge: f32 = (bounds.origin.x + bounds.size.width).into();
        let x: f32 = position.x.into();
        let y: f32 = position.y.into();
        let top: f32 = bounds.origin.y.into();
        let bottom: f32 = (bounds.origin.y + bounds.size.height).into();
        x >= right_edge - SCROLLBAR_HIT_WIDTH && x <= right_edge && y >= top && y <= bottom
    }

    /// Start or stop the auto-scroll timer based on whether the drag position
    /// is near/past the top/bottom edge of the terminal viewport.
    ///
    /// Uses a margin of half a cell height so the timer starts while the mouse
    /// is still inside the element (GPUI may not deliver move events once the
    /// cursor leaves element bounds).
    fn check_auto_scroll_edge(&mut self, cx: &mut Context<Self>) {
        let bounds = *self.canvas_bounds.borrow();
        let Some(mouse_pos) = self.last_drag_position else {
            self.auto_scroll_task = None;
            self.auto_scroll_start = None;
            return;
        };

        let cell_h: f32 = self.cell_height.into();
        let margin = (cell_h * 0.5).max(4.0);
        let top: f32 = bounds.origin.y.into();
        let bottom: f32 = (bounds.origin.y + bounds.size.height).into();
        let y: f32 = mouse_pos.y.into();

        let near_edge = y < top + margin || y > bottom - margin;
        if !near_edge {
            // Mouse is well inside viewport. Cancel any running timer
            self.auto_scroll_task = None;
            self.auto_scroll_start = None;
            return;
        }

        // Already running. The timer reads last_drag_position each tick,
        // so it will pick up the updated position automatically.
        if self.auto_scroll_task.is_some() {
            return;
        }

        // Record when auto-scroll began for time-based acceleration
        let scroll_start = std::time::Instant::now();
        self.auto_scroll_start = Some(scroll_start);

        // Spawn a repeating 50ms timer to scroll + extend selection
        let canvas_bounds = self.canvas_bounds.clone();
        let task = cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                Timer::after(std::time::Duration::from_millis(50)).await;
                let result = this.update(cx, |view, cx| {
                    let bounds = *canvas_bounds.borrow();
                    let Some(mouse_pos) = view.last_drag_position else {
                        return;
                    };

                    let cell_h: f32 = view.cell_height.into();
                    if cell_h <= 0.0 {
                        return;
                    }
                    let margin = (cell_h * 0.5).max(4.0);
                    let top: f32 = bounds.origin.y.into();
                    let bottom: f32 = (bounds.origin.y + bounds.size.height).into();
                    let y: f32 = mouse_pos.y.into();

                    // Time-based acceleration: ramp from 1 to 3 lines/tick
                    // 0–1s: 1 line, 1–2s: 2 lines, 2s+: 3 lines
                    let elapsed = scroll_start.elapsed().as_secs_f32();
                    let lines = if elapsed < 1.0 {
                        1
                    } else if elapsed < 2.0 {
                        2
                    } else {
                        3
                    };

                    // Determine direction
                    let (scroll_delta, edge_line_is_top) = if y < top + margin {
                        (lines, true) // scroll up (toward history)
                    } else if y > bottom - margin {
                        (-lines, false) // scroll down (toward present)
                    } else {
                        // Mouse back well inside. Will be cancelled on next move
                        return;
                    };

                    let term_arc = view.engine.term_arc();
                    let mut term = term_arc.lock();

                    // Stop if selection was cleared (e.g. Cmd+C during drag)
                    if term.selection.is_none() {
                        return;
                    }

                    term.scroll_display(Scroll::Delta(scroll_delta));

                    // Extend selection to the edge row
                    let display_offset = term.grid().display_offset() as i32;
                    let num_lines = term.grid().screen_lines();
                    let num_cols = term.grid().columns();
                    let edge_line = if edge_line_is_top {
                        0
                    } else {
                        (num_lines as i32) - 1
                    };
                    let edge_col = if edge_line_is_top {
                        0
                    } else {
                        num_cols.saturating_sub(1)
                    };
                    let buffer_point = alacritty_terminal::index::Point::new(
                        alacritty_terminal::index::Line(edge_line - display_offset),
                        alacritty_terminal::index::Column(edge_col),
                    );
                    if let Some(ref mut selection) = term.selection {
                        selection.update(buffer_point, Side::Right);
                    }
                    drop(term);
                    cx.notify();
                });
                if result.is_err() {
                    break;
                }
            },
        );
        self.auto_scroll_task = Some(task);
    }

    fn handle_session_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Exit => {
                self.terminated = true;
            }
            SessionEvent::ClipboardStore(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            SessionEvent::SemanticPrompt(cmd) => {
                let term_arc = self.engine.term_arc();
                let term = term_arc.lock();
                let cursor_pos = term.grid().cursor.point;
                drop(term);
                self.engine
                    .zone_tracker_mut()
                    .handle_command(cmd, cursor_pos);
                cx.emit(TerminalRuntimeEvent::SemanticStateChanged {
                    terminal_id: self.terminal_id.clone(),
                    state: self.engine.zone_tracker().state(),
                });
            }
            SessionEvent::Title(title) => {
                cx.emit(TerminalRuntimeEvent::TitleChanged {
                    terminal_id: self.terminal_id.clone(),
                    title: (!title.is_empty()).then_some(title),
                });
            }
            SessionEvent::Bell => {
                cx.emit(TerminalRuntimeEvent::Bell {
                    terminal_id: self.terminal_id.clone(),
                });
            }
            SessionEvent::Notification { title, body } => {
                cx.emit(TerminalRuntimeEvent::Notification {
                    terminal_id: self.terminal_id.clone(),
                    title,
                    body,
                });
            }
            SessionEvent::ClipboardLoad | SessionEvent::Wakeup => {}
        }
    }

    fn process_events(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        while let Some(event) = self.engine.try_recv_event() {
            self.handle_session_event(event, cx);
        }
    }

    /// Measure cell dimensions from the font. Only called once or on font change.
    fn ensure_cell_measured(&mut self, window: &mut Window) {
        if !self.cell_measured {
            self.renderer.measure_cell(window);
            self.cell_width = self.renderer.cell_width;
            self.cell_height = self.renderer.cell_height;
            self.cell_measured = true;
            // Push initial cell dimensions to EventProxy for TextAreaSizeRequest.
            let term_arc = self.engine.term_arc();
            let term = term_arc.lock();
            self.engine.window_size().update(
                term.screen_lines() as u16,
                term.columns() as u16,
                f32::from(self.cell_width) as u16,
                f32::from(self.cell_height) as u16,
            );
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.terminated {
            return;
        }

        // Selection-aware delete: backspace/delete with a selection in the input region.
        let key = event.keystroke.key.as_str();
        if key == "backspace" || key == "delete" {
            if let Some(delete_bytes) = self.try_selection_delete() {
                self.cursor_visible = true;
                self.cursor_blink_paused = true;
                self.last_input_time = std::time::Instant::now();
                self.engine.write(&delete_bytes);
                self.emit_local_input(cx);
                return;
            }
        }

        let input = KeyInput {
            keystroke: &event.keystroke,
            event_type: if event.is_held {
                KeyEventType::Repeat
            } else {
                KeyEventType::Press
            },
            associated_text: event.keystroke.key_char.as_deref(),
        };

        if let Some(bytes) = key_input_to_bytes(&input, self.engine.mode()) {
            // Clear selection and hold cursor steady while typing
            let term_arc = self.engine.term_arc();
            let mut term = term_arc.lock();
            term.selection = None;
            drop(term);
            self.cursor_visible = true;
            self.cursor_blink_paused = true;
            self.last_input_time = std::time::Instant::now();
            self.engine.write(&bytes);
            self.emit_local_input(cx);
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.terminated {
            return;
        }
        let mode = self.engine.mode();
        if !mode.contains(TermMode::REPORT_EVENT_TYPES) {
            return;
        }
        let input = KeyInput {
            keystroke: &event.keystroke,
            event_type: KeyEventType::Release,
            associated_text: None,
        };
        if let Some(bytes) = key_input_to_bytes(&input, mode) {
            self.engine.write(&bytes);
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminated {
            return;
        }
        let mode = self.engine.mode();
        // Modifier-only key events only matter when both ALL_KEYS and EVENT_TYPES are active.
        if !mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
            || !mode.contains(TermMode::REPORT_EVENT_TYPES)
        {
            self.prev_modifiers = event.modifiers;
            self.refresh_hovered_link(event.modifiers, cx);
            return;
        }

        let prev = self.prev_modifiers;
        let curr = event.modifiers;

        // Build modifier mask from the NEW state.
        let curr_mods = crate::input::kitty_modifier_mask(&curr);

        let changes: [(bool, bool, u32); 4] = [
            (prev.shift, curr.shift, KITTY_LEFT_SHIFT),
            (prev.control, curr.control, KITTY_LEFT_CONTROL),
            (prev.alt, curr.alt, KITTY_LEFT_ALT),
            (prev.platform, curr.platform, KITTY_LEFT_SUPER),
        ];

        for (was_set, is_set, codepoint) in changes {
            if was_set == is_set {
                continue;
            }
            let event_type = if is_set {
                KeyEventType::Press
            } else {
                KeyEventType::Release
            };
            if let Some(bytes) = modifier_key_to_bytes(codepoint, curr_mods, event_type, mode) {
                self.engine.write(&bytes);
            }
        }

        // CapsLock toggle: GPUI reports capslock.on as a boolean state.
        // Treat a transition as press (toggled on) or release (toggled off).
        let caps_now = event.capslock.on;
        if caps_now != self.prev_capslock {
            let event_type = if caps_now {
                KeyEventType::Press
            } else {
                KeyEventType::Release
            };
            if let Some(bytes) = modifier_key_to_bytes(KITTY_CAPS_LOCK, curr_mods, event_type, mode)
            {
                self.engine.write(&bytes);
            }
            self.prev_capslock = caps_now;
        }

        self.prev_modifiers = curr;
        self.refresh_hovered_link(curr, cx);
    }

    /// Attempt selection-aware delete. Returns synthesized keystrokes if
    /// the selection is within the shell input region, otherwise None.
    fn try_selection_delete(&mut self) -> Option<Vec<u8>> {
        if !self.engine.zone_tracker().is_inputting() {
            return None;
        }
        let input_region = *self.engine.zone_tracker().input_region()?;

        let term_arc = self.engine.term_arc();
        let mut term = term_arc.lock();

        let sel_range = term.selection.as_ref()?.to_range(&*term)?;
        let cursor_pos = term.grid().cursor.point;

        let delete_bytes = crate::selection_delete::compute_delete_keystrokes(
            &sel_range,
            &input_region,
            cursor_pos,
        )?;

        term.selection = None;
        drop(term);

        Some(delete_bytes)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        cx.notify();

        if self.terminated {
            return;
        }
        self.last_mouse_position = Some(event.position);

        // Scrollbar interaction: intercept before selection or mouse reporting
        if event.button == MouseButton::Left && self.is_in_scrollbar_region(event.position) {
            let bounds = *self.canvas_bounds.borrow();
            let track_height: f32 = bounds.size.height.into();
            let relative_y: f32 = (event.position.y - bounds.origin.y).into();

            let term_arc = self.engine.term_arc();
            let mut term = term_arc.lock();
            let num_lines = term.grid().screen_lines();
            let history_size = term.grid().history_size();
            let display_offset = term.grid().display_offset();

            if let Some(geom) =
                scrollbar_geometry(num_lines, history_size, display_offset, track_height)
            {
                if relative_y >= geom.thumb_y && relative_y <= geom.thumb_y + geom.thumb_height {
                    // Click on thumb: start drag
                    self.scrollbar_dragging = true;
                    self.scrollbar_drag_start = Some((f32::from(event.position.y), display_offset));
                } else {
                    // Click on track: jump so thumb centers at click position
                    let max_travel = (track_height - geom.thumb_height).max(1.0);
                    let ratio = 1.0 - (relative_y / max_travel).clamp(0.0, 1.0);
                    let new_offset = (ratio * history_size as f32).round() as i32;
                    let delta = new_offset - display_offset as i32;
                    if delta != 0 {
                        term.scroll_display(Scroll::Delta(delta));
                    }
                }
                drop(term);
                cx.notify();
                return; // consume event. no selection or mouse report
            }
        }

        // Open OSC 8 hyperlinks on modifier-click without interfering with normal selection.
        if is_hyperlink_activation_click(event.modifiers, event.button) {
            if let Some(link) = self.detect_link_at_position(event.position) {
                if open_hyperlink_uri(&link.uri) {
                    return;
                }
                // Opener failed. Fall through to normal click handling
            }
            // Fall through to normal click handling if no hyperlink
        }

        let mode = self.engine.mode();
        let cell = mouse::pixel_to_cell(
            event.position,
            self.mouse_origin(),
            self.cell_width,
            self.cell_height,
        );

        if mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG,
        ) {
            let modifiers = mouse::encode_modifiers(
                event.modifiers.shift,
                event.modifiers.alt,
                event.modifiers.control,
            );
            if let Some(bytes) =
                mouse::mouse_button_report(event.button, true, cell, modifiers, mode)
            {
                self.engine.write(&bytes);
            }
        } else {
            // Convert visual → buffer coordinates
            let term_arc = self.engine.term_arc();
            let mut term = term_arc.lock();
            let display_offset = term.grid().display_offset() as i32;
            let buffer_point = alacritty_terminal::index::Point::new(
                alacritty_terminal::index::Line(cell.line.0 - display_offset),
                cell.column,
            );

            if event.modifiers.shift && term.selection.is_some() {
                // Shift+Click: extend existing selection to the clicked point
                if let Some(ref mut selection) = term.selection {
                    selection.update(buffer_point, Side::Right);
                }
            } else {
                // Normal click: start a new selection
                let sel_type = mouse::selection_type_from_clicks(event.click_count);
                term.selection = Some(AlacSelection::new(sel_type, buffer_point, Side::Left));
            }
            drop(term);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.terminated {
            return;
        }

        // Cancel auto-scroll
        self.auto_scroll_task = None;
        self.auto_scroll_start = None;
        self.last_drag_position = None;

        // End scrollbar drag
        if self.scrollbar_dragging {
            self.scrollbar_dragging = false;
            self.scrollbar_drag_start = None;
            return;
        }

        let mode = self.engine.mode();
        if mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG,
        ) {
            let cell = mouse::pixel_to_cell(
                event.position,
                self.mouse_origin(),
                self.cell_width,
                self.cell_height,
            );
            let modifiers = mouse::encode_modifiers(
                event.modifiers.shift,
                event.modifiers.alt,
                event.modifiers.control,
            );
            if let Some(bytes) =
                mouse::mouse_button_report(event.button, false, cell, modifiers, mode)
            {
                self.engine.write(&bytes);
            }
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminated {
            return;
        }
        self.last_mouse_position = Some(event.position);

        // Catch button releases that happened outside element bounds
        if event.pressed_button.is_none() {
            if self.scrollbar_dragging {
                self.scrollbar_dragging = false;
                self.scrollbar_drag_start = None;
            }
            if self.auto_scroll_task.is_some() {
                self.auto_scroll_task = None;
                self.auto_scroll_start = None;
                self.last_drag_position = None;
            }
        }

        if event.pressed_button.is_some() {
            self.set_hovered_link(None, cx);
        }

        // Handle scrollbar thumb drag
        if self.scrollbar_dragging {
            if let Some((start_y, start_offset)) = self.scrollbar_drag_start {
                let bounds = *self.canvas_bounds.borrow();
                let track_height: f32 = bounds.size.height.into();
                let delta_y: f32 = f32::from(event.position.y) - start_y;

                let term_arc = self.engine.term_arc();
                let mut term = term_arc.lock();
                let history_size = term.grid().history_size();
                let num_lines = term.grid().screen_lines();

                if history_size > 0 && track_height > 0.0 {
                    // Use max_travel (track minus thumb) for 1:1 thumb tracking
                    let total = num_lines + history_size;
                    let visible_frac = num_lines as f32 / total as f32;
                    let thumb_h = (visible_frac * track_height).max(20.0);
                    let max_travel = (track_height - thumb_h).max(1.0);
                    let lines_per_pixel = history_size as f32 / max_travel;
                    let new_offset = (start_offset as i32
                        + (-delta_y * lines_per_pixel).round() as i32)
                        .clamp(0, history_size as i32);
                    let current_offset = term.grid().display_offset() as i32;
                    let scroll_delta = new_offset - current_offset;
                    if scroll_delta != 0 {
                        term.scroll_display(Scroll::Delta(scroll_delta));
                        drop(term);
                        cx.notify();
                    }
                }
            }
            return;
        }

        let mode = self.engine.mode();
        let cell = mouse::pixel_to_cell(
            event.position,
            self.mouse_origin(),
            self.cell_width,
            self.cell_height,
        );
        let modifiers = mouse::encode_modifiers(
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
        );

        if let Some(pressed) = event.pressed_button {
            // Button held: check if terminal app wants drag reports
            if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                if let Some(bytes) =
                    mouse::mouse_motion_report(Some(pressed), cell, modifiers, mode)
                {
                    self.engine.write(&bytes);
                }
            } else {
                // No mouse reporting: handle OrcaShell selection drag
                self.last_drag_position = Some(event.position);
                let term_arc = self.engine.term_arc();
                let mut term = term_arc.lock();
                if term.selection.is_some() {
                    let display_offset = term.grid().display_offset() as i32;
                    let buffer_point = alacritty_terminal::index::Point::new(
                        alacritty_terminal::index::Line(cell.line.0 - display_offset),
                        cell.column,
                    );
                    if let Some(ref mut selection) = term.selection {
                        selection.update(buffer_point, Side::Right);
                    }
                }
                drop(term);
                cx.notify();
                // Start/stop auto-scroll timer if dragging near viewport edges
                self.check_auto_scroll_edge(cx);
            }
        } else {
            // No button held: report hover motion if terminal requested it
            if let Some(bytes) = mouse::mouse_motion_report(None, cell, modifiers, mode) {
                self.engine.write(&bytes);
            }

            self.refresh_hovered_link(event.modifiers, cx);
        }
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminated {
            return;
        }
        self.last_mouse_position = Some(event.position);

        let cell_h: f32 = self.cell_height.into();
        if cell_h <= 0.0 {
            return;
        }

        let px_delta: f32 = match event.delta {
            ScrollDelta::Pixels(delta) => delta.y.into(),
            ScrollDelta::Lines(delta) => delta.y * cell_h,
        };

        // Accumulate sub-line pixel deltas (macOS trackpad sends tiny values)
        self.scroll_px += px_delta;
        let lines = (self.scroll_px / cell_h).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * cell_h;

        let mode = self.engine.mode();
        let cell = mouse::pixel_to_cell(
            event.position,
            self.mouse_origin(),
            self.cell_width,
            self.cell_height,
        );
        let modifiers = mouse::encode_modifiers(
            event.modifiers.shift,
            event.modifiers.alt,
            event.modifiers.control,
        );

        if let Some(bytes) = mouse::scroll_report(lines, cell, modifiers, mode) {
            self.engine.write(&bytes);
        } else {
            let term_arc = self.engine.term_arc();
            let mut term = term_arc.lock();
            term.scroll_display(Scroll::Delta(lines));
            cx.notify();
            drop(term);
            self.refresh_hovered_link(event.modifiers, cx);
        }
    }

    fn copy_selection_text(&mut self, cx: &mut Context<Self>) {
        let term_arc = self.engine.term_arc();
        let mut term = term_arc.lock();
        if let Some(text) = term.selection_to_string() {
            term.selection = None;
            drop(term);
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if self.terminated {
            return;
        }
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                let mode = self.engine.mode();
                let bytes = wrap_bracketed_paste(text.as_bytes(), mode);
                self.last_input_time = std::time::Instant::now();
                self.engine.write(&bytes);
                self.emit_local_input(cx);
            }
        }
    }

    fn handle_file_drop(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        if self.terminated {
            return;
        }
        for path in paths.paths() {
            let escaped = Self::shell_escape_path(path, self.shell_type);
            self.engine.write(format!("{} ", escaped).as_bytes());
        }
        self.emit_local_input(cx);
    }

    fn shell_escape_path(
        path: &std::path::Path,
        shell_type: orcashell_session::ShellType,
    ) -> String {
        orcashell_session::shell_integration::quote_path_for_shell(path, shell_type)
    }

    fn clear_scrollback(&mut self, cx: &mut Context<Self>) {
        let term_arc = self.engine.term_arc();
        let mut term = term_arc.lock();
        term.grid_mut().clear_history();
        term.scroll_display(Scroll::Bottom);
        drop(term);
        // Send Ctrl+L (form feed) to the shell. Bash/zsh/fish clear and redraw the prompt
        self.engine.write(b"\x0c");
        cx.notify();
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let new_size = self.config.font_size + px(1.0);
        if new_size <= px(32.0) {
            self.config.font_size = new_size;
            self.renderer.font_size = new_size;
            self.cell_measured = false;
            cx.notify();
        }
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let new_size = self.config.font_size - px(1.0);
        if new_size >= px(8.0) {
            self.config.font_size = new_size;
            self.renderer.font_size = new_size;
            self.cell_measured = false;
            cx.notify();
        }
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.config.font_size = self.base_font_size;
        self.renderer.font_size = self.base_font_size;
        self.cell_measured = false;
        cx.notify();
    }

    // ── Search ──

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref search) = self.search {
            // Already open. Re-focus input
            search.input.read(cx).focus(window);
            return;
        }

        let palette = &self.config.colors;
        let text_color = palette.search_input_text;
        let placeholder_color = palette.search_input_placeholder;
        let cursor_color = palette.search_input_cursor;
        let selection_bg = palette.search_input_selection;
        let input = cx.new(|cx| {
            TextInputState::new(
                text_color,
                placeholder_color,
                cursor_color,
                selection_bg,
                cx,
            )
        });

        let sub = cx.subscribe(
            &input,
            |this: &mut Self, _input, _event: &SearchQueryChanged, cx| {
                this.run_search(cx);
            },
        );

        let search_state = SearchState::new(input.clone());
        self.search = Some(search_state);
        self._search_subscription = Some(sub);

        input.read(cx).focus(window);
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        self._search_subscription = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut search) = self.search {
            let query = search.input.read(cx).value().to_string();
            let term_arc = self.engine.term_arc();
            let term = term_arc.lock();
            search.update_search(&*term, &query);
            drop(term);
            cx.notify();
        }
    }

    fn search_next(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut search) = self.search {
            search.next_match();
            if let Some(point) = search.current_match_point() {
                let term_arc = self.engine.term_arc();
                let mut term = term_arc.lock();
                term.scroll_to_point(point);
            }
            cx.notify();
        }
    }

    fn search_prev(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut search) = self.search {
            search.prev_match();
            if let Some(point) = search.current_match_point() {
                let term_arc = self.engine.term_arc();
                let mut term = term_arc.lock();
                term.scroll_to_point(point);
            }
            cx.notify();
        }
    }

    fn render_search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.search.as_ref().unwrap();
        let match_text: SharedString = search.match_count_text().into();
        let has_no_matches = !search.all_matches.is_empty();
        let _ = has_no_matches; // used implicitly by match_text

        let palette = &self.config.colors;
        let bar_bg = palette.search_bar_bg;
        let border_color = palette.search_bar_border;
        let text_color = palette.search_bar_text;
        let input_bg = palette.search_input_bg;

        div()
            .id("search-bar")
            .h(px(36.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .bg(bar_bg)
            .border_b_1()
            .border_color(border_color)
            // Search input container
            .child(
                div()
                    .id("search-input-wrapper")
                    .key_context("SearchBar")
                    .flex_1()
                    .min_w(px(80.0))
                    .max_w(px(300.0))
                    .h(px(24.0))
                    .bg(input_bg)
                    .rounded(px(4.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .child(search.input.clone())
                    .on_mouse_down(MouseButton::Left, {
                        let input = search.input.clone();
                        move |_, window, cx| {
                            input.read(cx).focus(window);
                            cx.stop_propagation();
                        }
                    })
                    .on_action(cx.listener(|this, _: &actions::Paste, _window, cx| {
                        if let Some(ref search) = this.search {
                            if let Some(item) = cx.read_from_clipboard() {
                                if let Some(text) = item.text() {
                                    let line = text.lines().next().unwrap_or("");
                                    if !line.is_empty() {
                                        search.input.update(cx, |input, cx| {
                                            input.insert_text(line);
                                            cx.notify();
                                            cx.emit(SearchQueryChanged);
                                        });
                                    }
                                }
                            }
                        }
                    }))
                    .on_action(cx.listener(|this, _: &actions::Copy, _window, cx| {
                        if let Some(ref search) = this.search {
                            search.input.update(cx, |input, cx| {
                                input.copy_selection(cx);
                            });
                        }
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        cx.stop_propagation();
                        let key = event.keystroke.key.as_str();
                        let shift = event.keystroke.modifiers.shift;
                        match key {
                            "escape" => this.close_search(window, cx),
                            "enter" if shift => this.search_prev(cx),
                            "enter" => this.search_next(cx),
                            _ => {
                                if let Some(ref search) = this.search {
                                    search.input.update(cx, |input, cx| {
                                        if input.handle_key_down(event, cx) {
                                            cx.emit(SearchQueryChanged);
                                        }
                                    });
                                }
                            }
                        }
                    })),
            )
            // Match count
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(text_color)
                    .min_w(px(60.0))
                    .child(match_text),
            )
            // Previous match button
            .child(
                div()
                    .id("search-prev")
                    .cursor_pointer()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .hover(|s| s.bg(palette.hover_overlay))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _window, cx| this.search_prev(cx)))
                    .child(div().text_size(px(14.0)).text_color(text_color).child("▲")),
            )
            // Next match button
            .child(
                div()
                    .id("search-next")
                    .cursor_pointer()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .hover(|s| s.bg(palette.hover_overlay))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _window, cx| this.search_next(cx)))
                    .child(div().text_size(px(14.0)).text_color(text_color).child("▼")),
            )
            // Close button
            .child(
                div()
                    .id("search-close")
                    .cursor_pointer()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .hover(|s| s.bg(palette.close_hover_overlay))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx)))
                    .child(div().text_size(px(14.0)).text_color(text_color).child("✕")),
            )
    }

    fn render_terminated(&self) -> impl IntoElement {
        div()
            .size_full()
            .bg(self.renderer.palette.background())
            .flex()
            .justify_center()
            .items_center()
            .child(
                div()
                    .text_color(self.config.terminated_text_color)
                    .text_size(px(16.0))
                    .child("[Session terminated]"),
            )
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.process_events(window, cx);

        // Update input region end while user is typing.
        if self.engine.zone_tracker().is_inputting() {
            let term_arc = self.engine.term_arc();
            let term = term_arc.lock();
            let cursor_pos = term.grid().cursor.point;
            drop(term);
            self.engine.zone_tracker_mut().update_input_end(cursor_pos);
        }

        // Measure cell dimensions once (or on font change)
        self.ensure_cell_measured(window);

        if self.terminated {
            return self.render_terminated().into_any_element();
        }

        let search_match_source = self
            .search
            .as_ref()
            .map(|search| (search.all_matches.clone(), search.current_index));

        let term_arc = self.engine.term_arc();
        let pty_master = self.engine.pty_master_arc();
        let shared_window_size = self.engine.window_size().clone();
        // Clone renderer once to move into closure. No inner clone needed
        let renderer = self.renderer.clone();
        let padding = self.config.padding;
        let cell_width = self.cell_width;
        let cell_height = self.cell_height;
        let cursor_visible = self.cursor_visible && self.focus_handle.is_focused(window);
        let cursor_shape = self.config.cursor_shape;
        let hovered_link = self.hovered_link.clone();

        let terminal_content = div().flex_1().min_h_0().child({
            let canvas_bounds = self.canvas_bounds.clone();
            canvas(
                move |bounds, _window, _cx| {
                    *canvas_bounds.borrow_mut() = bounds;
                    bounds
                },
                move |bounds, _, window, cx| {
                    let avail_w: f32 = (bounds.size.width - padding.left - padding.right).into();
                    let avail_h: f32 = (bounds.size.height - padding.top - padding.bottom).into();
                    let cw: f32 = cell_width.into();
                    let ch: f32 = cell_height.into();
                    let cols = (avail_w / cw).max(1.0) as usize;
                    let rows = (avail_h / ch).max(1.0) as usize;

                    let mut term = term_arc.lock();
                    let current_cols = term.columns();
                    let current_rows = term.screen_lines();
                    if cols != current_cols || rows != current_rows {
                        // M4 fix: use blocking lock(). PTY master is never contended
                        // during rendering (reader cloned at construction time)
                        let master = pty_master.lock();
                        let _ = master.resize(PtySize {
                            cols: cols as u16,
                            rows: rows as u16,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        drop(master);
                        term.resize(TermDimensions::new(cols, rows));
                        shared_window_size.update(rows as u16, cols as u16, cw as u16, ch as u16);
                    }

                    let snapshot = renderer.snapshot_frame(&mut term);
                    drop(term);
                    let visible_matches: Vec<VisibleMatch> =
                        if let Some((all_matches, current_index)) = &search_match_source {
                            build_visible_matches(
                                all_matches,
                                *current_index,
                                snapshot.display_offset(),
                                snapshot.num_lines(),
                            )
                        } else {
                            Vec::new()
                        };
                    let visible_hovered_link = build_visible_hovered_link(
                        hovered_link.as_ref(),
                        snapshot.display_offset(),
                        snapshot.num_lines(),
                    );
                    renderer.paint_from_snapshot(
                        bounds,
                        padding,
                        &snapshot,
                        cursor_visible,
                        cursor_shape,
                        &visible_matches,
                        visible_hovered_link.as_ref(),
                        window,
                        cx,
                    );
                },
            )
            .size_full()
        });

        let search_active = self.search.is_some();

        let container = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(self.renderer.palette.background())
            .cursor(if self.hovered_link.is_some() {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            })
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_action(cx.listener(|this, _: &actions::Copy, _window, cx| {
                this.copy_selection_text(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::Paste, _window, cx| {
                this.paste_from_clipboard(cx);
            }))
            .on_action(
                cx.listener(|this, _: &actions::ClearScrollback, _window, cx| {
                    this.clear_scrollback(cx);
                }),
            )
            .on_action(cx.listener(|this, _: &actions::ZoomIn, _window, cx| {
                this.zoom_in(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::ZoomOut, _window, cx| {
                this.zoom_out(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::ResetZoom, _window, cx| {
                this.reset_zoom(cx);
            }))
            .on_action(cx.listener(|this, _: &actions::SearchFind, window, cx| {
                this.open_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &actions::SearchDismiss, window, cx| {
                this.close_search(window, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            // File drag-and-drop: visual feedback + path insertion
            .drag_over::<ExternalPaths>(|style, _, _, _| {
                style
                    .border_2()
                    .border_color(rgba(0x5E9BFFCC_u32))
                    .bg(rgba(0x5E9BFF1A_u32))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.handle_file_drop(paths, cx);
            }));

        let container = if search_active {
            container
                .child(self.render_search_bar(cx))
                .child(terminal_content)
        } else {
            container.child(terminal_content)
        };

        container.into_any_element()
    }
}
