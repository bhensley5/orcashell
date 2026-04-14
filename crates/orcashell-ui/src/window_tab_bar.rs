use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
#[allow(unused_imports)]
use gpui::*;

#[cfg(target_os = "windows")]
use std::cell::RefCell;

use crate::app_view::ContextMenuRequest;
use crate::context_menu::{platform_shortcut, ContextMenuItem};
use crate::settings::AppSettings;
use crate::theme;
use crate::workspace::actions::ToggleSidebar;
use crate::workspace::layout::{LayoutNode, SplitDirection};
use crate::workspace::{AuxiliaryTabKind, RenameLocation, WorkspaceState};

/// Window-level tab bar rendered at the top of the window, spanning full width.
/// Ghostty-style: equal-width tabs, close X, "+" button, shortcut badges.
/// On Windows, also serves as the custom title bar with drag and window controls.
pub struct WindowTabBar {
    workspace: Entity<WorkspaceState>,
    menu_request: ContextMenuRequest,
    /// Double-click detection: (tab_index, timestamp)
    last_click: Option<(usize, Instant)>,
    /// Cached HWND for Win32 drag operations (Windows only).
    #[cfg(target_os = "windows")]
    hwnd: Option<isize>,
}

const DOUBLE_CLICK_MS: u128 = 400;

enum RenameAction {
    Focus,
    Commit,
}

impl WindowTabBar {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        menu_request: ContextMenuRequest,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&workspace, |_this, _ws, cx| cx.notify())
            .detach();
        Self {
            workspace,
            menu_request,
            last_click: None,
            #[cfg(target_os = "windows")]
            hwnd: None,
        }
    }

    /// Returns true if this click is a double-click on the same tab.
    fn check_double_click(&mut self, tab_index: usize) -> bool {
        let now = Instant::now();
        if let Some((last_idx, last_time)) = self.last_click.take() {
            if last_idx == tab_index && now.duration_since(last_time).as_millis() < DOUBLE_CLICK_MS
            {
                return true;
            }
        }
        self.last_click = Some((tab_index, now));
        false
    }
}

fn tab_split_button_enabled(has_terminal: bool, is_active: bool) -> Option<bool> {
    has_terminal.then_some(is_active)
}

fn build_tab_split_menu_items() -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem {
            label: "Split Right".into(),
            shortcut: Some(platform_shortcut("\u{2318}D", "Ctrl+Shift+D")),
            action: Box::new(|ws, cx| {
                ws.split_focused(SplitDirection::Vertical, cx);
            }),
            enabled: true,
        },
        ContextMenuItem {
            label: "Split Down".into(),
            shortcut: Some(platform_shortcut("\u{2318}\u{21E7}D", "Ctrl+Shift+E")),
            action: Box::new(|ws, cx| {
                ws.split_focused(SplitDirection::Horizontal, cx);
            }),
            enabled: true,
        },
    ]
}

// ── Win32 FFI for window drag (Windows only) ─────────────────────────────
//
// Uses a Win32 timer to poll cursor position and move the window. This runs
// outside GPUI's event dispatch to avoid RefCell re-entrancy issues. The
// pattern is taken directly from the Okena reference implementation.

#[cfg(target_os = "windows")]
fn get_hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::HasWindowHandle;
    // Use trait method explicitly since Window has its own window_handle() method.
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
        -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn IsZoomed(hwnd: isize) -> i32;
    fn GetCursorPos(point: *mut WinPoint) -> i32;
    fn GetAsyncKeyState(key: i32) -> i16;
    fn SetTimer(
        hwnd: isize,
        id: usize,
        elapse: u32,
        func: Option<unsafe extern "system" fn(isize, u32, usize, u32)>,
    ) -> usize;
    fn KillTimer(hwnd: isize, id: usize) -> i32;
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[cfg(target_os = "windows")]
const DRAG_TIMER_ID: usize = 0xD8A6;

#[cfg(target_os = "windows")]
struct DragState {
    hwnd: isize,
    start_cursor: WinPoint,
    start_window: WinPoint,
}

#[cfg(target_os = "windows")]
thread_local! {
    static DRAG_STATE: RefCell<Option<DragState>> = const { RefCell::new(None) };
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn drag_timer_proc(_hwnd: isize, _msg: u32, _id: usize, _time: u32) {
    const VK_LBUTTON: i32 = 0x01;
    if GetAsyncKeyState(VK_LBUTTON) >= 0 {
        stop_drag();
        return;
    }
    DRAG_STATE.with(|state| {
        let state = state.borrow();
        if let Some(ds) = state.as_ref() {
            let mut cursor = WinPoint { x: 0, y: 0 };
            if GetCursorPos(&mut cursor) != 0 {
                let dx = cursor.x - ds.start_cursor.x;
                let dy = cursor.y - ds.start_cursor.y;
                const SWP_NOSIZE: u32 = 0x0001;
                const SWP_NOZORDER: u32 = 0x0004;
                const SWP_NOACTIVATE: u32 = 0x0010;
                SetWindowPos(
                    ds.hwnd,
                    0,
                    ds.start_window.x + dx,
                    ds.start_window.y + dy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    });
}

#[cfg(target_os = "windows")]
fn start_drag(hwnd: isize) {
    unsafe {
        let mut cursor = WinPoint { x: 0, y: 0 };
        let mut rect = WinRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetCursorPos(&mut cursor) == 0 || GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }
        DRAG_STATE.with(|state| {
            *state.borrow_mut() = Some(DragState {
                hwnd,
                start_cursor: cursor,
                start_window: WinPoint {
                    x: rect.left,
                    y: rect.top,
                },
            });
        });
        SetTimer(hwnd, DRAG_TIMER_ID, 16, Some(drag_timer_proc));
    }
}

#[cfg(target_os = "windows")]
fn stop_drag() {
    DRAG_STATE.with(|state| {
        if let Some(ds) = state.borrow().as_ref() {
            unsafe {
                KillTimer(ds.hwnd, DRAG_TIMER_ID);
            }
        }
        *state.borrow_mut() = None;
    });
}

#[cfg(target_os = "windows")]
fn toggle_maximize_hwnd(hwnd: isize) {
    const SW_MAXIMIZE: i32 = 3;
    const SW_RESTORE: i32 = 9;
    unsafe {
        let cmd = if IsZoomed(hwnd) != 0 {
            SW_RESTORE
        } else {
            SW_MAXIMIZE
        };
        ShowWindow(hwnd, cmd);
    }
}

// ── Windows drag + double-click maximize helper ──────────────────────────

/// Attach Win32 timer-based drag and double-click maximize to a bar div.
#[cfg(target_os = "windows")]
fn apply_windows_drag(bar: Div, cx: &mut Context<WindowTabBar>) -> Div {
    bar.on_mouse_down(
        MouseButton::Left,
        cx.listener(|this, _, window, _cx| {
            if this.hwnd.is_none() {
                this.hwnd = get_hwnd(window);
            }
            if let Some(hwnd) = this.hwnd {
                // Double-click on title bar → toggle maximize
                if this.check_double_click(usize::MAX) {
                    stop_drag();
                    toggle_maximize_hwnd(hwnd);
                    return;
                }
                start_drag(hwnd);
            }
        }),
    )
}

// ── Windows window controls helper ────────────────────────────────────���──

/// Append minimize / maximize-or-restore / close buttons to a bar div.
/// Each button is 46px wide x 32px tall (Windows standard caption button size).
#[cfg(target_os = "windows")]
fn append_window_controls(bar: Div, window: &mut Window, palette: &theme::OrcaTheme) -> Div {
    let is_maximized = window.is_maximized();
    let surface = palette.SURFACE;
    let fog = palette.FOG;
    let close_hover = palette.WIN_CLOSE_HOVER;
    let close_hover_text = palette.WIN_CLOSE_HOVER_TEXT;

    bar
        // Separator before window controls
        .child(
            div()
                .h_full()
                .w(px(1.0))
                .flex_shrink_0()
                .bg(rgb(palette.SURFACE)),
        )
        // Minimize
        .child(
            div()
                .id("wtab-win-minimize")
                .w(px(46.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(px(18.0))
                .text_color(rgb(fog))
                .hover(move |s| s.bg(rgb(surface)))
                .child(div().mt(px(-4.0)).child("\u{2500}")) // ─
                .occlude()
                .window_control_area(WindowControlArea::Min),
        )
        // Maximize / Restore
        .child(
            div()
                .id("wtab-win-maximize")
                .w(px(46.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(px(14.0))
                .text_color(rgb(fog))
                .hover(move |s| s.bg(rgb(surface)))
                .child(
                    div()
                        .mt(px(-2.0))
                        .child(if is_maximized { "\u{2750}" } else { "\u{2610}" }),
                )
                .occlude()
                .window_control_area(WindowControlArea::Max),
        )
        // Close (Windows-standard red hover, using theme tokens)
        .child(
            div()
                .id("wtab-win-close")
                .w(px(46.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(px(12.0))
                .text_color(rgb(fog))
                .hover(move |s| s.bg(rgb(close_hover)).text_color(rgb(close_hover_text)))
                .child("\u{2715}") // ✕
                .occlude()
                .window_control_area(WindowControlArea::Close),
        )
}

impl Render for WindowTabBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        // Check rename state before taking the long-lived ws borrow
        let tab_rename_action = {
            let ws = self.workspace.read(cx);
            ws.renaming.as_ref().and_then(|r| {
                if r.location != RenameLocation::TabBar {
                    return None;
                }
                let is_focused = r.input.read(cx).focus_handle().is_focused(window);
                if r.focused_once && !is_focused {
                    Some(RenameAction::Commit)
                } else if !is_focused {
                    Some(RenameAction::Focus)
                } else {
                    None
                }
            })
        };
        // Act on rename (ws borrow is dropped, safe to mutate)
        match tab_rename_action {
            Some(RenameAction::Commit) => {
                self.workspace.update(cx, |ws, cx| ws.commit_rename(cx));
            }
            Some(RenameAction::Focus) => {
                self.workspace.update(cx, |ws, _cx| {
                    if let Some(ref r) = ws.renaming {
                        r.input.read(_cx).focus(window);
                    }
                    if let Some(ref mut r) = ws.renaming {
                        r.focused_once = true;
                    }
                });
            }
            None => {}
        }

        let ws = self.workspace.read(cx);
        let workspace = self.workspace.clone();
        let auxiliary_tabs = ws.auxiliary_tabs().to_vec();
        let active_auxiliary_tab_id = ws.active_auxiliary_tab().map(|tab| tab.id.clone());

        // Check if a rename is active in the tab bar (extract before dropping ws borrow)
        let rename_input = ws
            .renaming
            .as_ref()
            .filter(|r| r.location == RenameLocation::TabBar)
            .map(|r| (r.terminal_id.clone(), r.input.clone()));

        // Get active project's root Tabs data
        let (tab_count, active_tab, tab_info, project_id) = {
            if let Some(project) = ws.active_project() {
                if let LayoutNode::Tabs {
                    children,
                    active_tab,
                } = &project.layout
                {
                    let info: Vec<(String, Option<String>)> = children
                        .iter()
                        .enumerate()
                        .map(|(i, child)| {
                            if let Some(tid) = child.first_terminal_id() {
                                let label = ws.terminal_display_name(&project.id, tid);
                                (label, Some(tid.to_string()))
                            } else {
                                (format!("Tab {}", i + 1), None)
                            }
                        })
                        .collect();
                    (children.len(), *active_tab, info, project.id.clone())
                } else {
                    (0, 0, vec![], String::new())
                }
            } else {
                (0, 0, vec![], String::new())
            }
        };

        // When the sidebar is hidden, add left padding to clear macOS traffic lights.
        let sidebar_hidden = !cx.global::<AppSettings>().sidebar_visible;
        let traffic_light_pad = if cfg!(target_os = "macos") && sidebar_hidden {
            80.0
        } else {
            0.0
        };

        // Don't render content if no project. Still show window chrome on Windows.
        if tab_count == 0 && auxiliary_tabs.is_empty() {
            #[allow(unused_mut)]
            let mut empty_bar = div()
                .w_full()
                .h(px(32.0))
                .flex()
                .flex_shrink_0()
                .items_center()
                .bg(rgb(palette.DEEP))
                .border_b_1()
                .border_color(rgb(palette.SURFACE))
                .pl(px(traffic_light_pad));

            #[cfg(target_os = "windows")]
            {
                empty_bar = apply_windows_drag(empty_bar, cx);
                empty_bar = empty_bar.child(div().flex_1()); // Spacer pushes controls right
                empty_bar = append_window_controls(empty_bar, window, &palette);
            }

            return empty_bar;
        }

        let mut bar = div()
            .w_full()
            .h(px(32.0))
            .flex()
            .flex_shrink_0()
            .items_center()
            .bg(rgb(palette.DEEP))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .pl(px(traffic_light_pad));

        // Windows: attach drag handler to the bar
        #[cfg(target_os = "windows")]
        {
            bar = apply_windows_drag(bar, cx);
        }

        // Sidebar toggle button
        bar = bar.child(
            div()
                .id("wtab-sidebar-toggle")
                .w(px(36.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(px(16.0))
                .text_color(rgb(palette.FOG))
                .hover(|s| s.text_color(rgb(palette.BONE)))
                .child("\u{25E7}")
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(|_event, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(Box::new(ToggleSidebar), cx);
                }),
        );

        // Separator after sidebar toggle
        bar = bar.child(
            div()
                .h_full()
                .w(px(1.0))
                .flex_shrink_0()
                .bg(rgb(palette.SURFACE)),
        );

        for auxiliary_tab in &auxiliary_tabs {
            let tab_id = auxiliary_tab.id.clone();
            let tab_id_for_close = tab_id.clone();
            let title = auxiliary_tab.title.clone();
            let is_active = active_auxiliary_tab_id.as_deref() == Some(tab_id.as_str());
            let icon = match auxiliary_tab.kind {
                AuxiliaryTabKind::Settings => "\u{2699}",
                AuxiliaryTabKind::Diff { .. } => "\u{2260}",
                AuxiliaryTabKind::LiveDiffStream { .. } => "\u{223f}",
                AuxiliaryTabKind::RepositoryGraph { .. } => "⎇",
            };

            let ws_focus = workspace.clone();
            let ws_close = workspace.clone();

            let mut aux_tab = div()
                .id(ElementId::Name(format!("wtab-aux-{tab_id}").into()))
                .flex_1()
                .h_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(px(12.0))
                .px(px(8.0))
                .overflow_hidden()
                .border_r_1()
                .border_color(rgb(palette.SURFACE));

            if is_active {
                aux_tab = aux_tab
                    .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                    .text_color(rgb(palette.BONE));
            } else {
                aux_tab = aux_tab
                    .text_color(rgb(palette.FOG))
                    .hover(|s| s.bg(rgba(theme::with_alpha(palette.CURRENT, 0x80))));
            }

            aux_tab = aux_tab
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(div().text_size(px(12.0)).child(icon))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .id(ElementId::Name(
                            format!("wtab-aux-close-{tab_id_for_close}").into(),
                        ))
                        .ml(px(4.0))
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .rounded(px(3.0))
                        .text_size(px(12.0))
                        .text_color(rgb(palette.FOG))
                        .hover(|s| s.text_color(rgb(palette.BONE)).bg(rgb(palette.SURFACE)))
                        .child("\u{2715}")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            ws_close.update(cx, |ws, cx| {
                                ws.close_auxiliary_tab(&tab_id_for_close, cx);
                            });
                        }),
                )
                .when(!cfg!(target_os = "windows"), |d| {
                    d.on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                })
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    ws_focus.update(cx, |ws, cx| {
                        ws.focus_auxiliary_tab(&tab_id, cx);
                    });
                });

            bar = bar.child(aux_tab);
        }

        // Tab elements - equal flex basis
        for (i, (tab_label, tab_tid)) in tab_info.iter().enumerate() {
            let is_active = i == active_tab && active_auxiliary_tab_id.is_none();
            let label = tab_label.clone();
            let tid = tab_tid.clone();
            let pid_for_close = project_id.clone();
            let ws_close = workspace.clone();

            let mut tab = div()
                .id(ElementId::Name(format!("wtab-{}", i).into()))
                .flex_1()
                .h_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(px(12.0))
                .px(px(8.0))
                .overflow_hidden()
                .border_r_1()
                .border_color(rgb(palette.SURFACE));

            if is_active {
                tab = tab
                    .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                    .text_color(rgb(palette.BONE));
            } else {
                tab = tab
                    .text_color(rgb(palette.FOG))
                    .hover(|s| s.bg(rgba(theme::with_alpha(palette.CURRENT, 0x80))));
            }

            // Tab content: inline rename input OR label + badge + close button
            let tab_index = i;
            let show_close = tab_count > 1 || !auxiliary_tabs.is_empty();
            let split_button_enabled = tab_split_button_enabled(tid.is_some(), is_active);
            let split_button_menu_request = self.menu_request.clone();
            let split_button_workspace = workspace.clone();

            // Check if this tab's terminal is being renamed
            let is_renaming = tid
                .as_ref()
                .is_some_and(|t| rename_input.as_ref().is_some_and(|(rid, _)| rid == t));

            if is_renaming {
                let input_entity = rename_input.as_ref().unwrap().1.clone();
                let input_for_keys = input_entity.clone();
                tab = tab.child(
                    div()
                        .id(ElementId::Name(format!("wtab-rename-input-{}", i).into()))
                        .flex_1()
                        .min_w_0()
                        .h(px(20.0))
                        .bg(rgb(palette.SURFACE))
                        .rounded(px(3.0))
                        .px(px(4.0))
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .child(input_entity.clone())
                        .on_mouse_down(MouseButton::Left, {
                            let input = input_entity.clone();
                            move |_, window, cx| {
                                input.read(cx).focus(window);
                                cx.stop_propagation();
                            }
                        })
                        .on_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, _window, cx| {
                                cx.stop_propagation();
                                match event.keystroke.key.as_str() {
                                    "enter" => {
                                        this.workspace.update(cx, |ws, cx| ws.commit_rename(cx));
                                    }
                                    "escape" => {
                                        this.workspace.update(cx, |ws, cx| ws.cancel_rename(cx));
                                    }
                                    _ => {
                                        input_for_keys.update(cx, |input, cx| {
                                            input.handle_key_down(event, cx);
                                        });
                                    }
                                }
                            },
                        )),
                );
            } else {
                tab = tab
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label),
                    )
                    .when(split_button_enabled.is_some(), move |el| {
                        let split_enabled = split_button_enabled.unwrap_or(false);
                        let menu_request = split_button_menu_request.clone();
                        let ws_menu = split_button_workspace.clone();
                        let mut split_button = div()
                            .id(ElementId::Name(format!("wtab-split-{}", tab_index).into()))
                            .ml(px(4.0))
                            .w(px(16.0))
                            .h(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .rounded(px(3.0))
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .mt(px(-1.0))
                            .child("\u{25EB}");

                        if split_enabled {
                            split_button = split_button
                                .cursor_pointer()
                                .text_color(rgb(palette.FOG))
                                .hover(|s| s.text_color(rgb(palette.BONE)).bg(rgb(palette.SURFACE)))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(move |event: &ClickEvent, _window, cx| {
                                    cx.stop_propagation();
                                    *menu_request.borrow_mut() =
                                        Some((event.position(), build_tab_split_menu_items()));
                                    ws_menu.update(cx, |_ws, cx| cx.notify());
                                });
                        } else {
                            split_button = split_button
                                .text_color(rgb(palette.SLATE))
                                .opacity(0.5)
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(|_, _, cx| {
                                    cx.stop_propagation();
                                });
                        }

                        el.child(split_button)
                    })
                    // Shortcut badge for tabs 1-9
                    .when(i < 9, |el| {
                        el.child(
                            div()
                                .ml(px(4.0))
                                .mt(px(1.0))
                                .text_size(px(10.0))
                                .text_color(rgb(palette.SLATE))
                                .flex_shrink_0()
                                .child(if cfg!(target_os = "macos") {
                                    format!("\u{2318}{}", i + 1)
                                } else {
                                    format!("Ctrl+{}", i + 1)
                                }),
                        )
                    })
                    // Close button
                    .when(show_close, move |el| {
                        el.child(
                            div()
                                .id(ElementId::Name(format!("wtab-close-{}", tab_index).into()))
                                .ml(px(4.0))
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_shrink_0()
                                .rounded(px(3.0))
                                .text_size(px(12.0))
                                .text_color(rgb(palette.FOG))
                                .hover(|s| s.text_color(rgb(palette.BONE)).bg(rgb(palette.SURFACE)))
                                .child("\u{2715}")
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(move |_event, _window, cx| {
                                    cx.stop_propagation();
                                    ws_close.update(cx, |ws, cx| {
                                        ws.close_specific_tab(&pid_for_close, tab_index, cx);
                                    });
                                }),
                        )
                    });
            }

            tab = tab
                .when(!cfg!(target_os = "windows"), |d| {
                    d.on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                })
                .on_click({
                    let tid_for_click = tid.clone();
                    let pid_for_rename = project_id.clone();
                    cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        // Check for double-click -> start rename.
                        let is_double = if cfg!(target_os = "windows") {
                            // Use platform click_count since the bar's drag
                            // handler clobbers the shared last_click state.
                            // Still call check_double_click for its .take()
                            // side-effect which prevents the bar from seeing
                            // a false title-bar double-click maximize.
                            this.check_double_click(tab_index);
                            match event {
                                ClickEvent::Mouse(m) => m.down.click_count == 2,
                                _ => false,
                            }
                        } else {
                            this.check_double_click(tab_index)
                        };
                        if is_double {
                            if let Some(ref tid) = tid_for_click {
                                this.workspace.update(cx, |ws, cx| {
                                    ws.start_rename(
                                        pid_for_rename.clone(),
                                        tid.clone(),
                                        RenameLocation::TabBar,
                                        cx,
                                    );
                                });
                            }
                            return;
                        }
                        // Single click - switch to tab
                        this.workspace.update(cx, |ws, cx| {
                            ws.goto_tab(tab_index, cx);
                        });
                        // Transfer GPUI keyboard focus to the terminal
                        let ws_read = this.workspace.read(cx);
                        if let Some(target) = ws_read.focus.current_target() {
                            if let Some(project) = ws_read.project(&target.project_id) {
                                if let Some(LayoutNode::Terminal {
                                    terminal_id: Some(tid),
                                    ..
                                }) = project.layout.get_at_path(&target.layout_path)
                                {
                                    if let Some(view) = ws_read.terminal_view(tid) {
                                        view.read(cx).focus_handle().focus(window);
                                    }
                                }
                            }
                        }
                    })
                })
                // Right-click context menu
                .on_mouse_down(MouseButton::Right, {
                    let menu_request = self.menu_request.clone();
                    let pid = project_id.clone();
                    let ws_ctx = workspace.clone();
                    let tid_for_menu = tid.clone();
                    move |event: &MouseDownEvent, _window, cx| {
                        let pid_close = pid.clone();
                        let pid_close_others = pid.clone();
                        let pid_rename = pid.clone();
                        let tid_rename = tid_for_menu.clone();
                        let mut items = build_tab_split_menu_items();
                        items.extend([
                            ContextMenuItem {
                                label: "Close Tab".into(),
                                shortcut: Some(platform_shortcut(
                                    "\u{2318}\u{21E7}W",
                                    "Ctrl+Shift+Q",
                                )),
                                action: Box::new(move |ws, cx| {
                                    ws.close_specific_tab(&pid_close, tab_index, cx);
                                }),
                                enabled: tab_count > 1,
                            },
                            ContextMenuItem {
                                label: "Close Other Tabs".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    ws.close_other_tabs(&pid_close_others, tab_index, cx);
                                }),
                                enabled: tab_count > 1,
                            },
                        ]);
                        // Add Rename option if this tab has a terminal
                        if let Some(tid) = tid_rename {
                            items.push(ContextMenuItem {
                                label: "Rename".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    ws.start_rename(
                                        pid_rename.clone(),
                                        tid.clone(),
                                        RenameLocation::TabBar,
                                        cx,
                                    );
                                }),
                                enabled: true,
                            });
                        }
                        *menu_request.borrow_mut() = Some((event.position, items));
                        // Trigger re-render via workspace entity
                        ws_ctx.update(cx, |_ws, cx| cx.notify());
                    }
                });

            bar = bar.child(tab);
        }

        // Separator between tabs and action buttons
        bar = bar.child(
            div()
                .h_full()
                .w(px(1.0))
                .flex_shrink_0()
                .bg(rgb(palette.SURFACE)),
        );

        // "+" new tab button
        let ws_new = workspace.clone();
        bar = bar.child(
            div()
                .id("wtab-add")
                .w(px(36.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(if cfg!(target_os = "windows") {
                    px(20.0)
                } else {
                    px(16.0)
                })
                .text_color(rgb(palette.FOG))
                .hover(|s| s.text_color(rgb(palette.BONE)))
                .child(
                    div()
                        .when(cfg!(target_os = "windows"), |d| d.mt(px(-4.0)))
                        .child("+"),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    ws_new.update(cx, |ws, cx| {
                        ws.new_tab_focused(cx);
                    });
                }),
        );

        // Separator between + and settings
        bar = bar.child(
            div()
                .h_full()
                .w(px(1.0))
                .flex_shrink_0()
                .bg(rgb(palette.SURFACE)),
        );

        // Settings gear button
        let ws_settings_btn = workspace.clone();
        bar = bar.child(
            div()
                .id("wtab-settings-btn")
                .w(px(36.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .cursor_pointer()
                .text_size(if cfg!(target_os = "windows") {
                    px(12.0)
                } else {
                    px(18.0)
                })
                .text_color(rgb(palette.FOG))
                .hover(|s| s.text_color(rgb(palette.BONE)))
                .child("\u{2699}")
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    ws_settings_btn.update(cx, |ws, cx| ws.toggle_settings(cx));
                }),
        );

        // Windows: append minimize / maximize-or-restore / close controls
        #[cfg(target_os = "windows")]
        {
            bar = append_window_controls(bar, window, &palette);
        }

        bar
    }
}

#[cfg(test)]
mod tests {
    use super::{build_tab_split_menu_items, tab_split_button_enabled};

    #[test]
    fn split_button_only_renders_for_terminal_tabs() {
        assert_eq!(tab_split_button_enabled(true, true), Some(true));
        assert_eq!(tab_split_button_enabled(true, false), Some(false));
        assert_eq!(tab_split_button_enabled(false, true), None);
        assert_eq!(tab_split_button_enabled(false, false), None);
    }

    #[test]
    fn split_menu_contains_only_split_actions() {
        let items = build_tab_split_menu_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "Split Right");
        assert_eq!(items[1].label, "Split Down");
        assert!(items.iter().all(|item| item.enabled));
        assert!(items.iter().all(|item| item.shortcut.is_some()));
    }
}
