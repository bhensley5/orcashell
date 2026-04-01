use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use parking_lot::Mutex;

use crate::context_menu::{ContextMenuEvent, ContextMenuItem, ContextMenuOverlay};
use crate::diff_explorer::DiffExplorerView;
use crate::pane::resize::{self, ActiveDrag};
use crate::pane::LayoutContainer;
use crate::settings::AppSettings;
use crate::settings_view::SettingsView;
use crate::sidebar::Sidebar;
use crate::status_bar::StatusBar;
use crate::theme;
use crate::window_tab_bar::WindowTabBar;
use crate::workspace::actions::*;
use crate::workspace::layout::LayoutNode;
use crate::workspace::layout::SplitDirection;
use crate::workspace::{AuxiliaryTabKind, WorkspaceServices, WorkspaceState};
use orcashell_store::{Store, StoredProject, StoredWindow};

/// Shared state for requesting a context menu from child components.
/// Components write a menu request; OrcaAppView reads and renders it.
pub type ContextMenuRequest = Rc<RefCell<Option<(Point<Pixels>, Vec<ContextMenuItem>)>>>;

pub struct OrcaAppView {
    window_id: i64,
    window_handle: Option<AnyWindowHandle>,
    workspace: Entity<WorkspaceState>,
    layout_container: Entity<LayoutContainer>,
    window_tab_bar: Entity<WindowTabBar>,
    sidebar: Entity<Sidebar>,
    #[allow(dead_code)]
    status_bar: Entity<StatusBar>,
    focus_handle: FocusHandle,
    daemon_error: Option<String>,
    active_drag: ActiveDrag,
    context_menu: Option<Entity<ContextMenuOverlay>>,
    menu_request: ContextMenuRequest,
    store: Arc<Mutex<Option<Store>>>,
    #[allow(dead_code)]
    save_task: Option<Task<()>>,
    #[allow(dead_code)]
    git_event_task: Option<Task<()>>,
    settings_view: Option<Entity<SettingsView>>,
    #[allow(dead_code)]
    settings_save_task: Option<Task<()>>,
    diff_views: std::collections::HashMap<std::path::PathBuf, Entity<DiffExplorerView>>,
}

/// Transfer GPUI keyboard focus to the currently focused terminal in the workspace.
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

impl OrcaAppView {
    pub fn new(
        window_id: i64,
        cx: &mut Context<Self>,
        daemon_error: Option<String>,
        stored_projects: Vec<StoredProject>,
        active_project_id: Option<String>,
        services: WorkspaceServices,
        initial_dir: Option<PathBuf>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let has_stored = !stored_projects.is_empty();
        let git_events = services.git.subscribe_events();
        let store = services.store.clone();

        let workspace = cx.new(|cx| {
            let mut ws = WorkspaceState::new_with_services(services);
            if has_stored {
                ws.restore_projects(stored_projects, active_project_id, cx);
            } else {
                // Use caller-provided initial_dir when present (e.g., "open here"); fall back
                // to the process working directory for normal cold starts.
                let cwd = initial_dir.unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
                });
                ws.init_with_project(cwd, cx);
            }
            ws
        });

        let active_drag: ActiveDrag = Rc::new(RefCell::new(None));
        let menu_request: ContextMenuRequest = Rc::new(RefCell::new(None));

        let ws_clone = workspace.clone();
        let drag_clone = active_drag.clone();
        let menu_clone = menu_request.clone();
        let layout_container =
            cx.new(|_| LayoutContainer::new(ws_clone, vec![0], drag_clone, menu_clone));

        let window_tab_bar =
            cx.new(|cx| WindowTabBar::new(workspace.clone(), menu_request.clone(), cx));
        let sidebar = cx.new(|cx| Sidebar::new(workspace.clone(), menu_request.clone(), cx));
        let status_bar = cx.new(|cx| StatusBar::new(workspace.clone(), cx));

        cx.observe(&workspace, |_this, _ws, cx| cx.notify())
            .detach();

        let git_event_task = Some(cx.spawn({
            let workspace = workspace.clone();
            async move |_this, cx: &mut AsyncApp| {
                while let Ok(event) = git_events.recv().await {
                    let _ = cx.update(|cx| {
                        workspace.update(cx, |ws, cx| ws.handle_git_event(event, cx));
                    });
                }
            }
        }));

        // Debounced auto-save: save on workspace changes after 500ms delay
        Self::setup_debounced_save(window_id, workspace.clone(), store.clone(), cx);

        // Observe settings changes and propagate to all terminal views + save settings.json
        cx.observe_global::<AppSettings>(|this, cx| {
            let theme_changed = theme::sync_from_settings(cx);
            if !theme_changed {
                let settings = cx.global::<AppSettings>();
                let palette = theme::active(cx);
                let config = WorkspaceState::build_terminal_config(settings, &palette);
                this.workspace.update(cx, |ws, cx| {
                    for view in ws.terminal_views.values() {
                        view.update(cx, |v, _cx| v.apply_config(config.clone()));
                    }
                    cx.notify();
                });
            }
            // Debounced save of settings.json
            this.settings_save_task = Some(cx.spawn(async move |_this, cx: &mut AsyncApp| {
                Timer::after(std::time::Duration::from_millis(500)).await;
                let _ = cx.update(|cx| {
                    let s = cx.global::<AppSettings>();
                    if let Err(e) = s.0.save() {
                        tracing::error!("Failed to save settings.json: {e}");
                    }
                });
            }));
        })
        .detach();

        cx.observe_global::<theme::ResolvedTheme>(|this, cx| {
            let settings = cx.global::<AppSettings>();
            let palette = theme::active(cx);
            let config = WorkspaceState::build_terminal_config(settings, &palette);
            this.workspace.update(cx, |ws, cx| {
                ws.refresh_diff_theme(cx);
                for view in ws.terminal_views.values() {
                    view.update(cx, |v, _cx| v.apply_config(config.clone()));
                }
                cx.notify();
            });
            cx.notify();
        })
        .detach();

        Self {
            window_id,
            window_handle: None,
            workspace,
            layout_container,
            window_tab_bar,
            sidebar,
            status_bar,
            focus_handle,
            daemon_error,
            active_drag,
            context_menu: None,
            menu_request,
            store,
            save_task: None,
            git_event_task,
            settings_view: None,
            settings_save_task: None,
            diff_views: std::collections::HashMap::new(),
        }
    }

    /// Set the window handle after `cx.open_window()` returns. Needed for per-window
    /// bounds capture in debounced saves.
    pub fn set_window_handle(&mut self, handle: AnyWindowHandle) {
        self.window_handle = Some(handle);
    }

    /// Set up debounced auto-save: on workspace changes, schedule a 500ms delayed save.
    /// Captures and saves this window's bounds alongside workspace state.
    fn setup_debounced_save(
        window_id: i64,
        workspace: Entity<WorkspaceState>,
        store: Arc<Mutex<Option<Store>>>,
        cx: &mut Context<Self>,
    ) {
        cx.observe(&workspace, {
            let store = store.clone();
            let workspace = workspace.clone();
            move |this, _ws, cx| {
                let store = store.clone();
                let ws = workspace.clone();
                let wid = window_id;
                let wh = this.window_handle;
                this.save_task = Some(cx.spawn(async move |_this, cx: &mut AsyncApp| {
                    Timer::after(std::time::Duration::from_millis(500)).await;
                    let _ = cx.update(|cx| {
                        // Read bounds from this specific window
                        let bounds = wh.and_then(|h| {
                            h.update(cx, |_view, window, _cx| {
                                let b = window.bounds();
                                (
                                    f32::from(b.origin.x),
                                    f32::from(b.origin.y),
                                    f32::from(b.size.width),
                                    f32::from(b.size.height),
                                )
                            })
                            .ok()
                        });
                        Self::save_state_to_db(wid, &ws, &store, bounds, cx);
                    });
                }));
            }
        })
        .detach();
    }

    /// Save workspace state to the database for a specific window.
    /// Uses only `workspace.read()` to avoid triggering the observer cycle.
    pub fn save_state_to_db(
        window_id: i64,
        workspace: &Entity<WorkspaceState>,
        store: &Arc<Mutex<Option<Store>>>,
        window_bounds: Option<(f32, f32, f32, f32)>,
        cx: &App,
    ) {
        let mut store_guard = store.lock();
        let store = match store_guard.as_mut() {
            Some(s) => s,
            None => return,
        };

        let ws = workspace.read(cx);
        let stored = ws.to_stored_projects_for_window(window_id, cx);

        let window = StoredWindow {
            window_id,
            bounds_x: window_bounds.map(|(x, _, _, _)| x),
            bounds_y: window_bounds.map(|(_, y, _, _)| y),
            bounds_width: window_bounds.map(|(_, _, w, _)| w).unwrap_or(1200.0),
            bounds_height: window_bounds.map(|(_, _, _, h)| h).unwrap_or(800.0),
            active_project_id: ws.active_project_id.clone(),
            sort_order: window_id as i32 - 1,
            is_open: true,
        };

        if let Err(e) = store.save_window_state(&window, &stored) {
            tracing::error!("Failed to save window {window_id} state: {e}");
        }
    }

    /// Access the window_id for this view.
    pub fn window_id(&self) -> i64 {
        self.window_id
    }

    /// Access the workspace entity (for use in quit fallback path).
    pub fn workspace(&self) -> &Entity<WorkspaceState> {
        &self.workspace
    }

    /// Access the store (for use in quit fallback path).
    pub fn store_ref(&self) -> &Arc<Mutex<Option<Store>>> {
        &self.store
    }

    /// Perform a synchronous save of all state for this window. Called during app quit.
    pub fn save_on_quit(&self, window_bounds: Option<(f32, f32, f32, f32)>, cx: &App) {
        Self::save_state_to_db(
            self.window_id,
            &self.workspace,
            &self.store,
            window_bounds,
            cx,
        );
    }

    /// Get or create the settings view entity.
    fn ensure_settings_view(&mut self, cx: &mut Context<Self>) -> Entity<SettingsView> {
        if let Some(ref view) = self.settings_view {
            view.clone()
        } else {
            let ws = self.workspace.clone();
            let view = cx.new(|cx| SettingsView::new(ws, cx));
            self.settings_view = Some(view.clone());
            view
        }
    }

    fn ensure_diff_view(
        &mut self,
        scope_root: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Entity<DiffExplorerView> {
        let menu_req = self.menu_request.clone();
        self.diff_views
            .entry(scope_root.to_path_buf())
            .or_insert_with(|| {
                let ws = self.workspace.clone();
                let scope_root = scope_root.to_path_buf();
                let mr = menu_req.clone();
                cx.new(|cx| DiffExplorerView::new(ws, scope_root, mr, cx))
            })
            .clone()
    }

    fn prune_closed_diff_views(&mut self, cx: &App) {
        let open_diff_scopes: std::collections::HashSet<_> = self
            .workspace
            .read(cx)
            .auxiliary_tabs()
            .iter()
            .filter_map(|tab| match &tab.kind {
                AuxiliaryTabKind::Diff { scope_root } => Some(scope_root.clone()),
                AuxiliaryTabKind::Settings => None,
            })
            .collect();
        self.diff_views
            .retain(|scope_root, _| open_diff_scopes.contains(scope_root));
    }

    /// Returns a focus handle for the focused terminal in the workspace.
    pub fn terminal_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let ws = self.workspace.read(cx);

        if let Some(target) = ws.focus.current_target() {
            if let Some(project) = ws.project(&target.project_id) {
                if let Some(LayoutNode::Terminal {
                    terminal_id: Some(tid),
                    ..
                }) = project.layout.get_at_path(&target.layout_path)
                {
                    if let Some(view) = ws.terminal_view(tid) {
                        return Some(view.read(cx).focus_handle().clone());
                    }
                }
            }
        }

        let project = ws.active_project()?;
        let path = project.layout.find_first_terminal_path()?;
        let node = project.layout.get_at_path(&path)?;
        if let LayoutNode::Terminal {
            terminal_id: Some(tid),
            ..
        } = node
        {
            let view = ws.terminal_view(tid)?;
            Some(view.read(cx).focus_handle().clone())
        } else {
            None
        }
    }
}

impl Render for OrcaAppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prune_closed_diff_views(cx);
        let palette = theme::active(cx);

        let workspace = self.workspace.clone();
        let active_drag = self.active_drag.clone();
        let (active_auxiliary_tab, settings_focused) = {
            let workspace = self.workspace.read(cx);
            (
                workspace.active_auxiliary_tab().cloned(),
                workspace.is_settings_focused(),
            )
        };
        let pending_focus_terminal_id: Option<String> = self
            .workspace
            .update(cx, |ws, _cx| ws.take_pending_focus_terminal_id());

        if let Some(terminal_id) = pending_focus_terminal_id {
            if let Some(view) = self.workspace.read(cx).terminal_view(&terminal_id) {
                view.read(cx).focus_handle().focus(window);
            }
        }

        // Transfer GPUI keyboard focus to settings view when settings tab is active
        if settings_focused {
            let view = self.ensure_settings_view(cx);
            view.read(cx).focus_handle().focus(window);
        } else if let Some(ref view) = self.settings_view {
            // Clear any in-progress text edit when settings tab loses focus
            view.update(cx, |v, _cx| v.reset_edit_state());
        }

        // Update LayoutContainer root path to [active_tab]
        let active_tab = {
            let ws = self.workspace.read(cx);
            ws.active_project()
                .and_then(|p| p.layout.active_tab_index())
                .unwrap_or(0)
        };
        self.layout_container.update(cx, |lc, _cx| {
            lc.set_layout_path(vec![active_tab]);
        });

        let mut container = div()
            .id("orca-app-root")
            .size_full()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .on_action({
                let ws = workspace.clone();
                move |_: &SplitRight, window, cx| {
                    ws.update(cx, |ws, cx| ws.split_focused(SplitDirection::Vertical, cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &SplitDown, window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.split_focused(SplitDirection::Horizontal, cx)
                    });
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &NewTab, window, cx| {
                    ws.update(cx, |ws, cx| ws.new_tab_focused(cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &ClosePane, window, cx| {
                    let has_auxiliary = ws.read(cx).active_auxiliary_tab().is_some();
                    if has_auxiliary {
                        ws.update(cx, |ws, cx| ws.close_active_auxiliary_tab(cx));
                        focus_active_terminal(&ws, window, cx);
                    } else {
                        ws.update(cx, |ws, cx| ws.close_focused(cx));
                        focus_active_terminal(&ws, window, cx);
                    }
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &FocusNextPane, window, cx| {
                    ws.update(cx, |ws, cx| ws.focus_next_pane(cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &FocusPrevPane, window, cx| {
                    ws.update(cx, |ws, cx| ws.focus_prev_pane(cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &NextTab, window, cx| {
                    ws.update(cx, |ws, cx| ws.next_tab(cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &PrevTab, window, cx| {
                    ws.update(cx, |ws, cx| ws.prev_tab(cx));
                    focus_active_terminal(&ws, window, cx);
                }
            })
            .on_action({
                let sidebar = self.sidebar.clone();
                move |_: &ToggleSidebar, _window, cx| {
                    sidebar.update(cx, |s, cx| s.toggle(cx));
                }
            })
            .on_action({
                let sidebar = self.sidebar.clone();
                move |_: &AddProject, _window, cx| {
                    let ws = sidebar.read(cx).workspace().clone();
                    Sidebar::open_folder_picker(ws, cx);
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &CloseTab, window, cx| {
                    let has_auxiliary = ws.read(cx).active_auxiliary_tab().is_some();
                    if has_auxiliary {
                        ws.update(cx, |ws, cx| ws.close_active_auxiliary_tab(cx));
                        focus_active_terminal(&ws, window, cx);
                    } else {
                        ws.update(cx, |ws, cx| ws.close_tab(cx));
                        focus_active_terminal(&ws, window, cx);
                    }
                }
            })
            .on_action({
                let ws = workspace.clone();
                move |_: &ToggleSettings, window, cx| {
                    ws.update(cx, |ws, cx| ws.toggle_settings(cx));
                    if !ws.read(cx).is_settings_focused() {
                        focus_active_terminal(&ws, window, cx);
                    }
                }
            });

        // Register Cmd+1-9 direct tab switching
        macro_rules! register_goto_tab {
            ($container:expr, $ws:expr, $action:ty, $idx:expr) => {{
                let ws = $ws.clone();
                $container = $container.on_action(move |_: &$action, window, cx| {
                    ws.update(cx, |ws, cx| ws.goto_tab($idx, cx));
                    focus_active_terminal(&ws, window, cx);
                });
            }};
        }
        register_goto_tab!(container, workspace, GotoTab1, 0);
        register_goto_tab!(container, workspace, GotoTab2, 1);
        register_goto_tab!(container, workspace, GotoTab3, 2);
        register_goto_tab!(container, workspace, GotoTab4, 3);
        register_goto_tab!(container, workspace, GotoTab5, 4);
        register_goto_tab!(container, workspace, GotoTab6, 5);
        register_goto_tab!(container, workspace, GotoTab7, 6);
        register_goto_tab!(container, workspace, GotoTab8, 7);
        register_goto_tab!(container, workspace, GotoTab9, 8);

        container = container
            .on_mouse_up(MouseButton::Left, move |_event, _window, _cx| {
                *active_drag.borrow_mut() = None;
            })
            .on_mouse_move({
                let active_drag = self.active_drag.clone();
                let ws = workspace.clone();
                move |event: &MouseMoveEvent, _window, cx| {
                    let drag_ref = active_drag.clone();
                    let drag = drag_ref.borrow();
                    if let Some(ref drag_state) = *drag {
                        if let Some(new_sizes) = resize::compute_resize(drag_state, event.position)
                        {
                            let pid = drag_state.project_id.clone();
                            let path = drag_state.split_path.clone();
                            drop(drag);
                            ws.update(cx, |ws, cx| {
                                ws.update_split_sizes(&pid, &path, new_sizes, cx);
                            });
                        }
                    }
                }
            });

        // Show daemon error bar if daemon failed to start
        if let Some(ref err) = self.daemon_error {
            container = container.child(
                div()
                    .w_full()
                    .px(px(8.0))
                    .py(px(2.0))
                    .bg(rgb(palette.DEEP))
                    .flex_shrink_0()
                    .child(
                        div()
                            .text_color(rgb(palette.FOG))
                            .text_size(px(11.0))
                            .child(err.clone()),
                    ),
            );
        }

        if let Some(err) = self.workspace.read(cx).action_error() {
            let ws_clear = self.workspace.clone();
            container = container.child(
                div()
                    .w_full()
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(rgb(palette.DEEP))
                    .border_b_1()
                    .border_color(rgb(palette.SURFACE))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(palette.STATUS_AMBER))
                            .text_size(px(11.0))
                            .child(err.to_string()),
                    )
                    .child(
                        div()
                            .id("workspace-error-close")
                            .cursor_pointer()
                            .text_color(rgb(palette.FOG))
                            .text_size(px(11.0))
                            .child("\u{2715}")
                            .on_click(move |_event, _window, cx| {
                                ws_clear.update(cx, |ws, cx| ws.clear_action_error(cx));
                            }),
                    ),
            );
        }

        let content_area = match active_auxiliary_tab.as_ref().map(|tab| &tab.kind) {
            Some(crate::workspace::AuxiliaryTabKind::Settings) => {
                let view = self.ensure_settings_view(cx);
                div().flex_1().min_h_0().child(view)
            }
            Some(crate::workspace::AuxiliaryTabKind::Diff { scope_root }) => {
                let view = self.ensure_diff_view(scope_root, cx);
                div().flex_1().min_h_0().child(view)
            }
            None => div()
                .flex_1()
                .min_h_0()
                .child(self.layout_container.clone()),
        };

        // Main content: sidebar (full height) + tab bar + layout/settings area
        container = container.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .child(self.sidebar.clone())
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(self.window_tab_bar.clone())
                        .child(content_area),
                ),
        );

        // Process context menu requests from child components
        if let Some((position, items)) = self.menu_request.borrow_mut().take() {
            let ws = self.workspace.clone();
            let menu = cx.new(|cx| ContextMenuOverlay::new(position, items, ws, cx));
            cx.subscribe(
                &menu,
                |this, _menu, event: &ContextMenuEvent, cx| match event {
                    ContextMenuEvent::Dismiss => {
                        this.context_menu = None;
                        cx.notify();
                    }
                },
            )
            .detach();
            self.context_menu = Some(menu);
        }

        // Render context menu overlay if present
        if let Some(ref menu) = self.context_menu {
            container = container.child(menu.clone());
        }

        container
    }
}
