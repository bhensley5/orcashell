use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use gpui::*;

use crate::app_view::ContextMenuRequest;
use crate::context_menu::ContextMenuItem;
use crate::settings::{AppSettings, ThemeId};
use crate::theme;
use crate::workspace::layout::LayoutNode;
use crate::workspace::{RenameLocation, WorkspaceState};

const DOUBLE_CLICK_MS: u128 = 400;
const MIN_SIDEBAR_WIDTH: f32 = 240.0;
const MAX_SIDEBAR_WIDTH: f32 = 600.0;

// Scrollbar styling constants (matching diff explorer)
const SCROLLBAR_HIT_WIDTH: f32 = 12.0;
const SCROLLBAR_THUMB_WIDTH: f32 = 5.0;
const SCROLLBAR_THUMB_MIN: f32 = 20.0;
const SCROLLBAR_THUMB_INSET: f32 = 3.0;

#[derive(Clone, Copy)]
struct ScrollbarDrag {
    start_y: f32,
    start_scroll_y: f32,
}

#[derive(Clone)]
struct SidebarResizeDrag {
    initial_mouse_x: f32,
    initial_width: f32,
}

/// Drag payload for reordering projects in the sidebar.
#[derive(Clone)]
pub struct ProjectDragPayload {
    pub project_id: String,
    pub source_index: usize,
}

/// Drag payload for reordering terminals (tabs) within a project.
#[derive(Clone)]
pub struct SidebarTerminalDragPayload {
    pub project_id: String,
    pub tab_index: usize,
}

/// Ghost view for sidebar drags.
struct SidebarDragView {
    label: String,
}

impl Render for SidebarDragView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current();
        div()
            .px(px(10.0))
            .py(px(4.0))
            .bg(rgb(palette.CURRENT))
            .border_1()
            .border_color(rgb(palette.ORCA_BLUE))
            .rounded(px(4.0))
            .shadow_md()
            .text_size(px(12.0))
            .text_color(rgb(palette.BONE))
            .child(self.label.clone())
    }
}

/// Transfer GPUI keyboard focus to the currently focused terminal.
fn focus_active_terminal(ws: &Entity<WorkspaceState>, window: &mut Window, cx: &mut App) {
    let ws_read = ws.read(cx);
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
}

/// Shared double-click state for terminal rows in the sidebar.
type TermClickState = Rc<RefCell<Option<(String, Instant)>>>;

/// Check if this click is a double-click on the same terminal.
fn check_term_double_click(state: &TermClickState, terminal_id: &str) -> bool {
    let now = Instant::now();
    let mut guard = state.borrow_mut();
    if let Some((ref last_tid, last_time)) = *guard {
        if last_tid == terminal_id && now.duration_since(last_time).as_millis() < DOUBLE_CLICK_MS {
            *guard = None;
            return true;
        }
    }
    *guard = Some((terminal_id.to_string(), now));
    false
}

pub struct Sidebar {
    workspace: Entity<WorkspaceState>,
    visible: bool,
    menu_request: ContextMenuRequest,
    term_click_state: TermClickState,
    pulse_phase: f32,
    pulse_task: Option<Task<()>>,
    resize_drag: Option<SidebarResizeDrag>,
    scroll_handle: ScrollHandle,
    scrollbar_drag: Option<ScrollbarDrag>,
}

impl Sidebar {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        menu_request: ContextMenuRequest,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&workspace, |_this, _ws, cx| cx.notify())
            .detach();
        let visible = cx.global::<AppSettings>().sidebar_visible;
        Self {
            workspace,
            visible,
            menu_request,
            term_click_state: Rc::new(RefCell::new(None)),
            pulse_phase: 0.0,
            pulse_task: None,
            resize_drag: None,
            scroll_handle: ScrollHandle::new(),
            scrollbar_drag: None,
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.visible = !self.visible;
        cx.update_global::<AppSettings, _>(|settings, _| {
            settings.sidebar_visible = self.visible;
        });
        cx.notify();
    }

    pub fn workspace(&self) -> &Entity<WorkspaceState> {
        &self.workspace
    }

    fn ensure_pulse_task(&mut self, cx: &mut Context<Self>) {
        let settings = cx.global::<AppSettings>();
        let activity_pulse = settings.activity_pulse;
        let ws = self.workspace.read(cx);
        let has_pulsing = activity_pulse && ws.has_pulsing_terminals();
        let has_notifying = ws.has_notifying_terminals();
        let needs_animation = has_pulsing || has_notifying;

        if !needs_animation {
            if self.pulse_task.is_some() || self.pulse_phase != 0.0 {
                self.pulse_phase = 0.0;
                self.pulse_task = None;
                cx.notify();
            }
            return;
        }

        if self.pulse_task.is_some() || !self.visible {
            return;
        }

        self.pulse_task =
            Some(cx.spawn(
                async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                    Timer::after(Duration::from_millis(120)).await;

                    let should_continue = this.update(cx, |sidebar, cx| {
                        let settings = cx.global::<AppSettings>();
                        let ws = sidebar.workspace.read(cx);
                        let has_pulsing = settings.activity_pulse && ws.has_pulsing_terminals();
                        let has_notifying = ws.has_notifying_terminals();

                        if !sidebar.visible || (!has_pulsing && !has_notifying) {
                            sidebar.pulse_phase = 0.0;
                            sidebar.pulse_task = None;
                            cx.notify();
                            return false;
                        }

                        sidebar.pulse_phase = (sidebar.pulse_phase + 0.085) % 1.0;
                        cx.notify();
                        true
                    });

                    match should_continue {
                        Ok(true) => {}
                        Ok(false) | Err(_) => break,
                    }
                },
            ));
    }

    fn pulse_opacity(&self) -> f32 {
        let pulse = (self.pulse_phase * std::f32::consts::TAU).sin();
        0.72 + ((pulse + 1.0) * 0.14)
    }

    pub fn open_folder_picker(workspace: Entity<WorkspaceState>, cx: &mut App) {
        let future = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select Project Folder".into()),
        });
        cx.spawn(async move |cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = future.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = cx.update(|cx| {
                        workspace.update(cx, |ws, cx| {
                            ws.add_project(path, cx);
                        });
                    });
                }
            }
        })
        .detach();
    }

    fn render_project_list(&self, cx: &App) -> Stateful<Div> {
        let palette = theme::active(cx);
        let ws = self.workspace.read(cx);
        let activity_pulse = cx.global::<AppSettings>().activity_pulse;
        let mut list = div()
            .id("sidebar-project-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .flex()
            .flex_col();

        for (proj_idx, project) in ws.projects.iter().enumerate() {
            let _is_active = ws.active_project_id.as_deref() == Some(&project.id);
            let pid = project.id.clone();
            let project_name = project.name.clone();
            let ws_proj_drop = self.workspace.clone();

            // Project header row
            let mut project_row = div()
                .id(ElementId::Name(
                    format!("sidebar-proj-{}", project.id).into(),
                ))
                .w_full()
                .px(px(12.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer();

            // Project row has no highlight. Terminal selection is the indicator

            let drag_name = project_name.clone();
            let ws_add = self.workspace.clone();
            let pid_for_add = pid.clone();
            project_row = project_row
                .justify_between()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(if cfg!(target_os = "windows") {
                            px(16.0)
                        } else {
                            px(13.0)
                        })
                        .text_color(rgb(palette.BONE))
                        .child(if cfg!(target_os = "windows") {
                            format!("\u{1F5C1} {}", project.name)
                        } else {
                            format!("\u{1F4C1} {}", project.name)
                        }),
                )
                .child(
                    div()
                        .id(ElementId::Name(
                            format!("sidebar-add-term-{}", project.id).into(),
                        ))
                        .cursor_pointer()
                        .flex()
                        .justify_center()
                        .w(px(20.0))
                        .text_size(px(13.0))
                        .text_color(rgb(palette.FOG))
                        .child("+")
                        .on_click(move |_event, window, cx| {
                            ws_add.update(cx, |ws, cx| {
                                ws.add_terminal_to_project(&pid_for_add, cx);
                            });
                            focus_active_terminal(&ws_add, window, cx);
                        }),
                )
                .on_click({
                    let ws = self.workspace.clone();
                    let pid = pid.clone();
                    move |_event, window, cx| {
                        ws.update(cx, |ws, cx| {
                            ws.set_active_project(&pid, cx);
                        });
                        focus_active_terminal(&ws, window, cx);
                    }
                })
                // Drag-and-drop for project reordering
                .on_drag(
                    ProjectDragPayload {
                        project_id: pid.clone(),
                        source_index: proj_idx,
                    },
                    move |_payload, _position, _window, cx| {
                        let label = drag_name.clone();
                        cx.new(|_| SidebarDragView { label })
                    },
                )
                .drag_over::<ProjectDragPayload>(move |style, _, _, _| {
                    style.border_t(px(2.0)).border_color(rgb(palette.ORCA_BLUE))
                })
                .on_drop({
                    let target_idx = proj_idx;
                    move |payload: &ProjectDragPayload, _window, cx| {
                        ws_proj_drop.update(cx, |ws, cx| {
                            ws.reorder_project(payload.source_index, target_idx, cx);
                        });
                    }
                })
                // Right-click context menu on project header
                .on_mouse_down(MouseButton::Right, {
                    let menu_request = self.menu_request.clone();
                    let pid = pid.clone();
                    let ws_notify = self.workspace.clone();
                    move |event: &MouseDownEvent, _window, cx| {
                        let pid_worktree = pid.clone();
                        let pid_remove = pid.clone();
                        let items = vec![
                            ContextMenuItem {
                                label: "Create Worktree".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    ws.create_project_worktree(&pid_worktree, cx);
                                }),
                                enabled: true,
                            },
                            ContextMenuItem {
                                label: "Remove Project".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    ws.remove_project(&pid_remove, cx);
                                }),
                                enabled: true,
                            },
                        ];
                        *menu_request.borrow_mut() = Some((event.position, items));
                        ws_notify.update(cx, |_ws, cx| cx.notify());
                    }
                });

            list = list.child(project_row);

            // Terminal rows - one per tab (each tab's first terminal)
            let terminal_ids = project.layout.collect_terminal_ids();
            for tid in &terminal_ids {
                let term_name = ws.terminal_display_name(&project.id, tid);
                let git_snapshot = ws.terminal_git_snapshot(tid).cloned();
                let term_path = project.layout.find_terminal_path(tid);
                let tab_index = term_path.as_ref().and_then(|p| p.first().copied());
                let is_term_focused = ws
                    .focus
                    .is_focused(&project.id, &term_path.clone().unwrap_or_default());
                let is_pulsing = activity_pulse && ws.terminal_should_pulse(tid);

                let workspace = self.workspace.clone();
                let ws_term_drop = self.workspace.clone();
                let pid = pid.clone();
                let pid_for_ctx = pid.clone();
                let pid_for_drag = pid.clone();
                let pid_for_drop = pid.clone();
                let tid_clone = tid.clone();

                let mut term_row = div()
                    .id(ElementId::Name(format!("sidebar-term-{}", tid).into()))
                    .w_full()
                    .pl(px(24.0))
                    .pr(px(12.0))
                    .py(px(5.0))
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .text_size(px(13.0));

                term_row = term_row.border_l_2();
                if is_term_focused {
                    term_row = term_row
                        .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                        .text_color(rgb(palette.PATCH))
                        .border_color(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x66)));
                } else {
                    term_row = term_row
                        .text_color(rgb(palette.FOG))
                        .border_color(transparent_black());
                }

                let drag_name = term_name.clone();
                let is_renaming = ws.renaming.as_ref().is_some_and(|r| {
                    r.terminal_id == *tid && r.location == RenameLocation::Sidebar
                });

                let notification_tier = ws.terminal_notification_tier(tid);
                let is_notifying = notification_tier.is_some();

                let (icon_text, icon_color, icon_opacity, icon_weight) = if is_notifying {
                    let _ = notification_tier;
                    ("!!", palette.SEAFOAM, self.pulse_opacity(), FontWeight::BOLD)
                } else if is_pulsing {
                    (
                        ">_",
                        palette.ORCA_BLUE,
                        self.pulse_opacity(),
                        FontWeight::NORMAL,
                    )
                } else if is_term_focused {
                    (">_", palette.PATCH, 1.0, FontWeight::NORMAL)
                } else {
                    (">_", palette.FOG, 1.0, FontWeight::NORMAL)
                };

                term_row = term_row.child(
                    div()
                        .w(px(20.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_weight(icon_weight)
                        .text_color(rgb(icon_color))
                        .opacity(icon_opacity)
                        .child(icon_text),
                );

                if is_renaming {
                    // Show inline rename input
                    let input_entity = ws.renaming.as_ref().unwrap().input.clone();
                    let input_for_keys = input_entity.clone();
                    let ws_for_rename = self.workspace.clone();
                    term_row = term_row.child(
                        div()
                            .id(ElementId::Name(format!("sidebar-rename-{}", tid).into()))
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
                            .on_key_down({
                                let ws = ws_for_rename.clone();
                                move |event: &KeyDownEvent, _window, cx| {
                                    cx.stop_propagation();
                                    match event.keystroke.key.as_str() {
                                        "enter" => {
                                            ws.update(cx, |ws, cx| ws.commit_rename(cx));
                                        }
                                        "escape" => {
                                            ws.update(cx, |ws, cx| ws.cancel_rename(cx));
                                        }
                                        _ => {
                                            input_for_keys.update(cx, |input, cx| {
                                                input.handle_key_down(event, cx);
                                            });
                                        }
                                    }
                                }
                            }),
                    );
                } else {
                    let title_color = if is_term_focused {
                        rgb(palette.PATCH)
                    } else {
                        rgb(palette.BONE)
                    };
                    let mut term_details = div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div().w_full().flex().items_center().child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(title_color)
                                    .child(term_name.clone()),
                            ),
                        );

                    if let Some(snapshot) = git_snapshot.clone() {
                        let ws_diff = self.workspace.clone();
                        let tid_for_diff = tid.clone();
                        let branch_color = if is_term_focused {
                            palette.PATCH
                        } else {
                            palette.FOG
                        };
                        let diff_button = div()
                            .id(ElementId::Name(format!("sidebar-diff-{}", tid).into()))
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(2.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgb(palette.BORDER_EMPHASIS))
                            .rounded(px(3.0))
                            .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x0C)))
                            .text_size(px(10.0))
                            .font_family("JetBrains Mono")
                            .text_color(rgb(palette.FOG))
                            .hover(|s| {
                                s.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x1A)))
                                    .text_color(rgb(palette.BONE))
                            })
                            .child("Diff")
                            .on_click(move |_event, _window, cx| {
                                cx.stop_propagation();
                                ws_diff.update(cx, |ws, cx| {
                                    ws.open_diff_tab_for_terminal(&tid_for_diff, cx)
                                });
                            });

                        let mut badge_cluster = div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(4.0))
                            .child(diff_button);
                        if snapshot.is_worktree {
                            badge_cluster = badge_cluster.child(
                                div()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .border_1()
                                    .border_color(rgb(palette.ORCA_BLUE))
                                    .rounded(px(6.0))
                                    .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x18)))
                                    .text_size(px(10.0))
                                    .font_family("JetBrains Mono")
                                    .text_color(rgb(palette.ORCA_BLUE))
                                    .child("WT"),
                            );
                        }

                        term_details = term_details
                            .child(
                                div().w_full().flex().items_center().child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_size(px(11.0))
                                        .font_family("JetBrains Mono")
                                        .text_color(rgb(branch_color))
                                        .child(snapshot.branch_name),
                                ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .text_size(px(10.0))
                                    .font_family("JetBrains Mono")
                                    .child(badge_cluster.flex_shrink_0())
                                    .child(
                                        div()
                                            .text_color(rgb(palette.FOG))
                                            .child(format!("{} files", snapshot.changed_files)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(palette.STATUS_GREEN))
                                            .child(format!("+{}", snapshot.insertions)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(palette.STATUS_CORAL))
                                            .child(format!("-{}", snapshot.deletions)),
                                    ),
                            );
                    }
                    term_row = term_row.child(term_details);
                }

                let tid_for_dblclick = tid.clone();
                let pid_for_dblclick = pid.clone();
                let click_state = self.term_click_state.clone();
                term_row = term_row.on_click(move |_event, window, cx| {
                    // Double-click → start rename
                    if check_term_double_click(&click_state, &tid_for_dblclick) {
                        workspace.update(cx, |ws, cx| {
                            ws.start_rename(
                                pid_for_dblclick.clone(),
                                tid_for_dblclick.clone(),
                                RenameLocation::Sidebar,
                                cx,
                            );
                        });
                        return;
                    }
                    // Single click. Focus terminal
                    let path = {
                        let ws_read = workspace.read(cx);
                        ws_read
                            .project(&pid)
                            .and_then(|p| p.layout.find_terminal_path(&tid_clone))
                    };
                    if let Some(path) = path {
                        workspace.update(cx, |ws, cx| {
                            ws.select_terminal(pid.clone(), path, cx);
                        });
                        focus_active_terminal(&workspace, window, cx);
                    }
                });

                // Right-click context menu on terminal row
                {
                    let menu_request = self.menu_request.clone();
                    let pid_ctx = pid_for_ctx.clone();
                    let tid_ctx = tid.clone();
                    let ws_ctx = self.workspace.clone();
                    term_row = term_row.on_mouse_down(MouseButton::Right, {
                        move |event: &MouseDownEvent, _window, cx| {
                            let pid_focus = pid_ctx.clone();
                            let pid_worktree = pid_ctx.clone();
                            let pid_close = pid_ctx.clone();
                            let pid_rename = pid_ctx.clone();
                            let tid_focus = tid_ctx.clone();
                            let tid_worktree = tid_ctx.clone();
                            let tid_close = tid_ctx.clone();
                            let tid_rename = tid_ctx.clone();
                            let mut items = vec![ContextMenuItem {
                                label: "Focus".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    let path = ws
                                        .project(&pid_focus)
                                        .and_then(|p| p.layout.find_terminal_path(&tid_focus));
                                    if let Some(path) = path {
                                        ws.select_terminal(pid_focus.clone(), path, cx);
                                    }
                                }),
                                enabled: true,
                            }];
                            let show_worktree = {
                                let ws_read = ws_ctx.read(cx);
                                ws_read.terminal_is_git_backed(&tid_ctx)
                            };
                            if show_worktree {
                                items.push(ContextMenuItem {
                                    label: "Create Worktree From This Terminal".into(),
                                    shortcut: None,
                                    action: Box::new(move |ws, cx| {
                                        ws.create_terminal_worktree(
                                            &pid_worktree,
                                            &tid_worktree,
                                            cx,
                                        );
                                    }),
                                    enabled: true,
                                });
                            }
                            items.push(ContextMenuItem {
                                label: "Close".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    let path = ws
                                        .project(&pid_close)
                                        .and_then(|p| p.layout.find_terminal_path(&tid_close));
                                    if let Some(path) = path {
                                        ws.focus_pane(pid_close.clone(), path, cx);
                                        ws.close_focused(cx);
                                    }
                                }),
                                enabled: true,
                            });
                            items.push(ContextMenuItem {
                                label: "Rename".into(),
                                shortcut: None,
                                action: Box::new(move |ws, cx| {
                                    ws.start_rename(
                                        pid_rename.clone(),
                                        tid_rename.clone(),
                                        RenameLocation::Sidebar,
                                        cx,
                                    );
                                }),
                                enabled: true,
                            });
                            *menu_request.borrow_mut() = Some((event.position, items));
                            ws_ctx.update(cx, |_ws, cx| cx.notify());
                        }
                    });
                }

                // Terminal drag-and-drop (reorder tabs)
                if let Some(src_tab) = tab_index {
                    let drag_label = drag_name.clone();
                    term_row = term_row
                        .on_drag(
                            SidebarTerminalDragPayload {
                                project_id: pid_for_drag,
                                tab_index: src_tab,
                            },
                            move |_payload, _position, _window, cx| {
                                let label = drag_label.clone();
                                cx.new(|_| SidebarDragView { label })
                            },
                        )
                        .drag_over::<SidebarTerminalDragPayload>(move |style, _, _, _| {
                            style.border_t(px(2.0)).border_color(rgb(palette.ORCA_BLUE))
                        })
                        .on_drop({
                            let target_tab = src_tab;
                            move |payload: &SidebarTerminalDragPayload, _window, cx| {
                                if payload.project_id == pid_for_drop {
                                    ws_term_drop.update(cx, |ws, cx| {
                                        ws.reorder_tab(
                                            &pid_for_drop,
                                            payload.tab_index,
                                            target_tab,
                                            cx,
                                        );
                                    });
                                }
                            }
                        });
                }

                list = list.child(term_row);
            }
        }

        list
    }

    /// Compute (thumb_y, thumb_height) for the sidebar scrollbar, or `None`
    /// if the content fits within the viewport.
    fn scrollbar_geometry(&self) -> Option<(f32, f32)> {
        let bounds = self.scroll_handle.bounds();
        let viewport_h = f32::from(bounds.size.height);
        if viewport_h <= 0.0 {
            return None;
        }
        let max_offset = self.scroll_handle.max_offset();
        let content_h = viewport_h + f32::from(max_offset.height);
        if content_h <= viewport_h {
            return None;
        }
        let scroll_y = -f32::from(self.scroll_handle.offset().y);
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
            theme::with_alpha(palette.ORCA_BLUE, 0x40)
        } else {
            theme::with_alpha(palette.ORCA_BLUE, 0x25)
        };

        div()
            .id("sidebar-scrollbar")
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
                    let bounds = this.scroll_handle.bounds();
                    let local_y = y - f32::from(bounds.origin.y);

                    if let Some((ty, th)) = this.scrollbar_geometry() {
                        if local_y >= ty && local_y <= ty + th {
                            // Clicking on the thumb. Start drag.
                            let scroll_y = -f32::from(this.scroll_handle.offset().y);
                            this.scrollbar_drag = Some(ScrollbarDrag {
                                start_y: y,
                                start_scroll_y: scroll_y,
                            });
                        } else {
                            // Click on track. Jump to that position.
                            this.scrollbar_jump_to(local_y);
                        }
                    }
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let Some(drag) = this.scrollbar_drag else {
                    return;
                };
                let bounds = this.scroll_handle.bounds();
                let viewport_h = f32::from(bounds.size.height);
                let max_offset = this.scroll_handle.max_offset();
                let content_h = viewport_h + f32::from(max_offset.height);
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
                this.scroll_handle
                    .set_offset(point(px(0.0), px(-new_scroll)));
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.scrollbar_drag = None;
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
        let bounds = self.scroll_handle.bounds();
        let viewport_h = f32::from(bounds.size.height);
        let max_offset = self.scroll_handle.max_offset();
        let content_h = viewport_h + f32::from(max_offset.height);
        let scrollable_content = content_h - viewport_h;
        let thumb_h = (viewport_h / content_h * viewport_h).max(SCROLLBAR_THUMB_MIN);
        let scrollable_track = viewport_h - thumb_h;
        if scrollable_track <= 0.0 {
            return;
        }
        let ratio = ((local_y - thumb_h / 2.0) / scrollable_track).clamp(0.0, 1.0);
        let new_scroll = ratio * scrollable_content;
        self.scroll_handle
            .set_offset(point(px(0.0), px(-new_scroll)));
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        let selection = theme::active_selection(cx);
        let (logo_asset, logo_opacity) = match selection.resolved_id {
            ThemeId::Light | ThemeId::Sepia => ("images/OrcaShellLogoTrimmedBlack.png", 1.0),
            ThemeId::Dark | ThemeId::Black => ("images/OrcaShellLogoTrimmed.png", 0.75),
        };
        if !self.visible {
            return div().id("sidebar-root").w(px(0.0)).h_full().flex_shrink_0();
        }

        self.ensure_pulse_task(cx);

        // Rename blur-to-commit for sidebar
        let sidebar_rename_action = {
            let ws = self.workspace.read(cx);
            ws.renaming.as_ref().and_then(|r| {
                if r.location != RenameLocation::Sidebar {
                    return None;
                }
                let is_focused = r.input.read(cx).focus_handle().is_focused(window);
                if r.focused_once && !is_focused {
                    Some(true) // commit
                } else if !is_focused {
                    Some(false) // focus
                } else {
                    None
                }
            })
        };
        match sidebar_rename_action {
            Some(true) => {
                self.workspace.update(cx, |ws, cx| ws.commit_rename(cx));
            }
            Some(false) => {
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

        let workspace = self.workspace.clone();
        let sidebar_width = cx.global::<AppSettings>().sidebar_width;

        // Shared drag state for window-level event handlers.
        let drag_state: Rc<RefCell<Option<SidebarResizeDrag>>> =
            Rc::new(RefCell::new(self.resize_drag.clone()));

        div()
            .id("sidebar-root")
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_row()
            // Zero-size canvas to register window-level mouse events during paint.
            .child(
                canvas(|_bounds, _window, _cx| {}, {
                    let drag_for_move = drag_state.clone();
                    let drag_for_up = drag_state.clone();
                    let entity = cx.entity().downgrade();
                    let entity_up = cx.entity().downgrade();
                    move |_bounds, _prepaint, window, _cx| {
                        // Window-level mouse move. Fires even outside sidebar bounds.
                        window.on_mouse_event({
                            let drag = drag_for_move.clone();
                            let entity = entity.clone();
                            move |event: &MouseMoveEvent, phase, _window, cx| {
                                if phase != DispatchPhase::Bubble {
                                    return;
                                }
                                let current = drag.borrow();
                                if let Some(ref d) = *current {
                                    let next_width = (d.initial_width
                                        + (f32::from(event.position.x) - d.initial_mouse_x))
                                        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
                                    cx.update_global::<AppSettings, _>(|settings, _| {
                                        settings.sidebar_width = next_width;
                                    });
                                    if let Some(e) = entity.upgrade() {
                                        e.update(cx, |s, cx| {
                                            s.resize_drag = Some(SidebarResizeDrag {
                                                initial_mouse_x: d.initial_mouse_x,
                                                initial_width: d.initial_width,
                                            });
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                        });
                        // Window-level mouse up. End drag.
                        window.on_mouse_event({
                            let drag = drag_for_up.clone();
                            let entity = entity_up.clone();
                            move |e: &MouseUpEvent, phase, _window, cx| {
                                if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                                    return;
                                }
                                if drag.borrow().is_some() {
                                    *drag.borrow_mut() = None;
                                    let settings = cx.global::<AppSettings>();
                                    let _ = settings.save();
                                    if let Some(e) = entity.upgrade() {
                                        e.update(cx, |s, cx| {
                                            s.resize_drag = None;
                                            cx.notify();
                                        });
                                    }
                                }
                            }
                        });
                    }
                })
                .absolute()
                .size_0(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .w(px(sidebar_width - 3.0))
                    .h_full()
                    .bg(rgb(palette.DEEP))
                    .flex()
                    .flex_col()
                    // Header
                    .child(
                        div()
                            .w_full()
                            .h(px(32.0))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_end()
                            .px(px(12.0))
                            .child(
                                div()
                                    .id("sidebar-add-project")
                                    .cursor_pointer()
                                    .flex()
                                    .justify_center()
                                    .w(px(20.0))
                                    .text_size(if cfg!(target_os = "windows") {
                                        px(16.0)
                                    } else {
                                        px(13.0)
                                    })
                                    .text_color(rgb(palette.FOG))
                                    .child(if cfg!(target_os = "windows") {
                                        "\u{1F5C1}"
                                    } else {
                                        "\u{1F4C1}"
                                    })
                                    .on_click(move |_event, _window, cx| {
                                        Sidebar::open_folder_picker(workspace.clone(), cx);
                                    }),
                            ),
                    )
                    // Divider
                    .child(
                        div()
                            .w_full()
                            .h(px(1.0))
                            .bg(rgb(palette.SURFACE))
                            .flex_shrink_0(),
                    )
                    // Project list with scrollbar overlay
                    .child({
                        let scrollbar = self.scrollbar_geometry().map(|(thumb_y, thumb_height)| {
                            let is_dragging = self.scrollbar_drag.is_some();
                            self.render_scrollbar(thumb_y, thumb_height, is_dragging, cx)
                        });
                        // Notify sidebar on scroll so the scrollbar thumb re-renders
                        let project_list = self.render_project_list(cx).on_scroll_wheel(
                            cx.listener(|_this, _event: &ScrollWheelEvent, _window, cx| {
                                cx.notify();
                            }),
                        );
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(project_list)
                            .children(scrollbar)
                    })
                    // Logo pinned to bottom
                    .child(
                        div()
                            .mt_auto()
                            .flex_shrink_0()
                            .w_full()
                            .flex()
                            .justify_center()
                            .pt(px(16.0))
                            .pb(px(20.0))
                            .child(
                                img(logo_asset)
                                    .w(px(160.0))
                                    .h(px(120.0))
                                    .opacity(logo_opacity),
                            ),
                    ),
            )
            // Resize handle
            .child(
                div()
                    .id("sidebar-resize-handle")
                    .w(px(3.0))
                    .h_full()
                    .flex_shrink_0()
                    .bg(rgb(palette.SURFACE))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let drag_state = drag_state.clone();
                            move |this, event: &MouseDownEvent, _window, cx| {
                                let current_width = cx.global::<AppSettings>().sidebar_width;
                                let drag = SidebarResizeDrag {
                                    initial_mouse_x: f32::from(event.position.x),
                                    initial_width: current_width,
                                };
                                this.resize_drag = Some(drag.clone());
                                *drag_state.borrow_mut() = Some(drag);
                                cx.stop_propagation();
                            }
                        }),
                    ),
            )
    }
}
