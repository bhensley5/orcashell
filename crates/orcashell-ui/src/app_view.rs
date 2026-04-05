use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_channel::bounded;
use gpui::*;
use parking_lot::Mutex;

use crate::context_menu::{ContextMenuEvent, ContextMenuItem, ContextMenuOverlay};
use crate::diff_explorer::DiffExplorerView;
use crate::live_diff_stream::LiveDiffStreamView;
use crate::pane::resize::{self, ActiveDrag};
use crate::pane::LayoutContainer;
use crate::settings::AppSettings;
use crate::settings_view::SettingsView;
use crate::sidebar::Sidebar;
use crate::status_bar::StatusBar;
use crate::theme;
use crate::updater::{self, AvailableUpdate, UpdateCheckResult};
use crate::window_tab_bar::WindowTabBar;
use crate::workspace::actions::*;
use crate::workspace::layout::LayoutNode;
use crate::workspace::layout::SplitDirection;
use crate::workspace::{
    AuxiliaryTabKind, WorkspaceBanner, WorkspaceBannerKind, WorkspaceServices, WorkspaceState,
};
use orcashell_store::{Store, StoredProject, StoredWindow};

/// Shared state for requesting a context menu from child components.
/// Components write a menu request; OrcaAppView reads and renders it.
pub type ContextMenuRequest = Rc<RefCell<Option<(Point<Pixels>, Vec<ContextMenuItem>)>>>;

static STARTUP_UPDATE_CHECK_STARTED: AtomicBool = AtomicBool::new(false);
const DISMISSED_UPDATE_VERSION_KEY: &str = "dismissed_update_version";

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateBannerKind {
    Available,
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdateBannerState {
    kind: UpdateBannerKind,
    message: String,
    latest_version: Option<String>,
    download_url: Option<String>,
    release_notes_url: Option<String>,
}

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
    live_diff_views: std::collections::HashMap<String, Entity<LiveDiffStreamView>>,
    update_banner: Option<UpdateBannerState>,
    update_check_in_flight: bool,
    #[allow(dead_code)]
    update_check_task: Option<Task<()>>,
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
            if let Some(message) = daemon_error.clone() {
                ws.workspace_banner = Some(WorkspaceBanner {
                    kind: WorkspaceBannerKind::Error,
                    message,
                });
            }
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
            for view in this.diff_views.values() {
                view.update(cx, |v, cx| v.invalidate_theme_cache(cx));
            }
            for view in this.live_diff_views.values() {
                view.update(cx, |v, cx| v.invalidate_theme_cache(cx));
            }
            cx.notify();
        })
        .detach();

        let mut this = Self {
            window_id,
            window_handle: None,
            workspace,
            layout_container,
            window_tab_bar,
            sidebar,
            status_bar,
            focus_handle,
            active_drag,
            context_menu: None,
            menu_request,
            store,
            save_task: None,
            git_event_task,
            settings_view: None,
            settings_save_task: None,
            diff_views: std::collections::HashMap::new(),
            live_diff_views: std::collections::HashMap::new(),
            update_banner: None,
            update_check_in_flight: false,
            update_check_task: None,
        };
        this.start_startup_update_check(cx);
        this
    }

    fn start_startup_update_check(&mut self, cx: &mut Context<Self>) {
        if STARTUP_UPDATE_CHECK_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        self.trigger_update_check(false, cx);
    }

    fn trigger_update_check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update_check_in_flight {
            if manual {
                self.update_banner = Some(UpdateBannerState {
                    kind: UpdateBannerKind::Info,
                    message: "Already checking for updates…".to_string(),
                    latest_version: None,
                    download_url: None,
                    release_notes_url: None,
                });
                cx.notify();
            }
            return;
        }

        self.update_check_in_flight = true;
        if manual {
            self.update_banner = Some(UpdateBannerState {
                kind: UpdateBannerKind::Info,
                message: "Checking for updates…".to_string(),
                latest_version: None,
                download_url: None,
                release_notes_url: None,
            });
            cx.notify();
        }

        let (tx, rx) = bounded::<UpdateCheckResult>(1);
        std::thread::spawn(move || {
            let result = updater::check_for_updates();
            let _ = tx.send_blocking(result);
        });

        self.update_check_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(result) = rx.recv().await else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    this.apply_update_check_result(result, manual, cx);
                });
            },
        ));
    }

    fn apply_update_check_result(
        &mut self,
        result: UpdateCheckResult,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        self.update_check_in_flight = false;
        self.update_check_task = None;

        match result {
            UpdateCheckResult::UpdateAvailable(update) => {
                if !manual && self.is_update_dismissed(&update.latest_version) {
                    self.update_banner = None;
                    return;
                }
                self.update_banner = Some(Self::available_update_banner(update));
                cx.notify();
            }
            UpdateCheckResult::UpToDate { current_version } => {
                if manual {
                    self.update_banner = Some(UpdateBannerState {
                        kind: UpdateBannerKind::Info,
                        message: format!("OrcaShell {current_version} is up to date."),
                        latest_version: None,
                        download_url: None,
                        release_notes_url: None,
                    });
                    cx.notify();
                }
            }
            UpdateCheckResult::Failed { message } => {
                if manual {
                    self.update_banner = Some(UpdateBannerState {
                        kind: UpdateBannerKind::Error,
                        message,
                        latest_version: None,
                        download_url: None,
                        release_notes_url: None,
                    });
                    cx.notify();
                }
            }
        }
    }

    fn available_update_banner(update: AvailableUpdate) -> UpdateBannerState {
        UpdateBannerState {
            kind: UpdateBannerKind::Available,
            message: format!(
                "OrcaShell {} is available. You’re running {}.",
                update.latest_version, update.current_version
            ),
            latest_version: Some(update.latest_version),
            download_url: Some(update.download_url),
            release_notes_url: update.release_notes_url,
        }
    }

    fn dismiss_update_banner(&mut self, cx: &mut Context<Self>) {
        if let Some(latest_version) = self
            .update_banner
            .as_ref()
            .and_then(|banner| banner.latest_version.as_deref())
        {
            self.persist_dismissed_update_version(latest_version);
        }
        if self.update_banner.take().is_some() {
            cx.notify();
        }
    }

    fn is_update_dismissed(&self, latest_version: &str) -> bool {
        let store_guard = self.store.lock();
        let Some(store) = store_guard.as_ref() else {
            return false;
        };
        match store.get_state(DISMISSED_UPDATE_VERSION_KEY) {
            Ok(Some(value)) => value == latest_version,
            Ok(None) => false,
            Err(error) => {
                tracing::warn!("Failed to read dismissed update version: {error}");
                false
            }
        }
    }

    fn persist_dismissed_update_version(&self, latest_version: &str) {
        let store_guard = self.store.lock();
        let Some(store) = store_guard.as_ref() else {
            return;
        };
        if let Err(error) = store.set_state(DISMISSED_UPDATE_VERSION_KEY, latest_version) {
            tracing::warn!("Failed to persist dismissed update version: {error}");
        }
    }

    fn render_update_banner(
        &self,
        palette: &theme::OrcaTheme,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let banner = self.update_banner.clone()?;
        let (bg_color, border_color, text_color, button_border, button_bg) = match banner.kind {
            UpdateBannerKind::Available => (
                theme::with_alpha(palette.ORCA_BLUE, 0x12),
                theme::with_alpha(palette.ORCA_BLUE, 0x30),
                palette.PATCH,
                palette.ORCA_BLUE,
                theme::with_alpha(palette.ORCA_BLUE, 0x18),
            ),
            UpdateBannerKind::Info => (
                theme::with_alpha(palette.STATUS_AMBER, 0x12),
                theme::with_alpha(palette.STATUS_AMBER, 0x30),
                palette.STATUS_AMBER,
                palette.STATUS_AMBER,
                theme::with_alpha(palette.STATUS_AMBER, 0x12),
            ),
            UpdateBannerKind::Error => (
                theme::with_alpha(palette.STATUS_CORAL, 0x12),
                theme::with_alpha(palette.STATUS_CORAL, 0x30),
                palette.STATUS_CORAL,
                palette.STATUS_CORAL,
                theme::with_alpha(palette.STATUS_CORAL, 0x12),
            ),
        };

        let download_url = banner.download_url.clone();
        let release_notes_url = banner.release_notes_url.clone();

        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .px(px(8.0))
                .py(px(3.0))
                .border_1()
                .border_color(rgb(button_border))
                .bg(rgba(button_bg))
                .text_color(rgb(palette.PATCH))
                .text_size(px(11.0))
                .child(label)
        };

        let mut bar = div()
            .w_full()
            .px(px(8.0))
            .py(px(6.0))
            .bg(rgba(bg_color))
            .border_b_1()
            .border_color(rgba(border_color))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(rgb(text_color))
                    .text_size(px(11.0))
                    .child(banner.message),
            );

        if let Some(url) = download_url {
            bar = bar.child(button("update-download", "Download").on_click(
                move |_event, _window, _cx| {
                    let _ = orcashell_platform::open_url(&url);
                },
            ));
        }

        if let Some(url) = release_notes_url {
            bar = bar.child(button("update-release-notes", "Release Notes").on_click(
                move |_event, _window, _cx| {
                    let _ = orcashell_platform::open_url(&url);
                },
            ));
        }

        Some(
            bar.child(
                div()
                    .id("update-banner-close")
                    .cursor_pointer()
                    .text_color(rgb(palette.FOG))
                    .text_size(px(11.0))
                    .child("\u{2715}")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.dismiss_update_banner(cx);
                    })),
            ),
        )
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
                AuxiliaryTabKind::Settings | AuxiliaryTabKind::LiveDiffStream { .. } => None,
            })
            .collect();
        self.diff_views
            .retain(|scope_root, _| open_diff_scopes.contains(scope_root));
    }

    fn ensure_live_diff_view(
        &mut self,
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> Entity<LiveDiffStreamView> {
        self.live_diff_views
            .entry(project_id.to_string())
            .or_insert_with(|| {
                let ws = self.workspace.clone();
                let project_id = project_id.to_string();
                cx.new(|cx| LiveDiffStreamView::new(ws, project_id, cx))
            })
            .clone()
    }

    fn prune_closed_live_diff_views(&mut self, cx: &App) {
        let open_project_ids: std::collections::HashSet<_> = self
            .workspace
            .read(cx)
            .auxiliary_tabs()
            .iter()
            .filter_map(|tab| match &tab.kind {
                AuxiliaryTabKind::LiveDiffStream { project_id } => Some(project_id.clone()),
                AuxiliaryTabKind::Diff { .. } | AuxiliaryTabKind::Settings => None,
            })
            .collect();
        self.live_diff_views
            .retain(|project_id, _| open_project_ids.contains(project_id));
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
        self.prune_closed_live_diff_views(cx);
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
            })
            .on_action(cx.listener(|this, _: &CheckForUpdates, _window, cx| {
                this.trigger_update_check(true, cx);
            }));

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

        let content_area = match active_auxiliary_tab.as_ref().map(|tab| &tab.kind) {
            Some(crate::workspace::AuxiliaryTabKind::Settings) => {
                let view = self.ensure_settings_view(cx);
                div().flex_1().min_h_0().child(view)
            }
            Some(crate::workspace::AuxiliaryTabKind::Diff { scope_root }) => {
                let view = self.ensure_diff_view(scope_root, cx);
                div().flex_1().min_h_0().child(view)
            }
            Some(crate::workspace::AuxiliaryTabKind::LiveDiffStream { project_id }) => {
                let view = self.ensure_live_diff_view(project_id, cx);
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(view)
            }
            None => div()
                .flex_1()
                .min_h_0()
                .child(self.layout_container.clone()),
        };

        let workspace_banner = self.workspace.read(cx).workspace_banner().cloned();
        let update_banner = self.render_update_banner(&palette, cx);

        let content_area = if workspace_banner.is_some() || update_banner.is_some() {
            let mut wrapped = div().flex_1().min_h_0().flex().flex_col();
            if let Some(banner) = workspace_banner {
                let ws_clear = self.workspace.clone();
                let text_color = match banner.kind {
                    crate::workspace::WorkspaceBannerKind::Warning => rgb(palette.STATUS_AMBER),
                    crate::workspace::WorkspaceBannerKind::Error => rgb(palette.STATUS_CORAL),
                };
                wrapped = wrapped.child(
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
                                .text_color(text_color)
                                .text_size(px(11.0))
                                .child(banner.message),
                        )
                        .child(
                            div()
                                .id("workspace-error-close")
                                .cursor_pointer()
                                .text_color(rgb(palette.FOG))
                                .text_size(px(11.0))
                                .child("\u{2715}")
                                .on_click(move |_event, _window, cx| {
                                    ws_clear.update(cx, |ws, cx| ws.clear_workspace_banner(cx));
                                }),
                        ),
                );
            }
            if let Some(banner) = update_banner {
                wrapped = wrapped.child(banner);
            }
            wrapped.child(content_area)
        } else {
            content_area
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
