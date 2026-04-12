#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gpui::*;
use orcashell_daemon_core::git_coordinator::GitCoordinator;
use orcashell_daemon_core::server::DaemonServer;
use orcashell_ipc::default_endpoint;
use orcashell_protocol::messages::OpenDisposition;
use orcashell_store::{config_dir, database_path, Store, StoredProject};
use orcashell_terminal_view::{
    ClearScrollback, Copy, Paste, ResetZoom, SearchDismiss, SearchFind, ZoomIn, ZoomOut,
};
use orcashell_ui::app_view::OrcaAppView;
use orcashell_ui::context_menu::DismissMenu;
use orcashell_ui::settings::{register_settings, AppSettings};
use orcashell_ui::theme::{register_theme, update_window_appearance};
use orcashell_ui::window_registry::WindowRegistry;
use orcashell_ui::workspace::actions::*;
use orcashell_ui::workspace::WorkspaceServices;
use parking_lot::Mutex;

type OpenWindowFn = dyn Fn(Option<PathBuf>, &mut App) + Send + Sync;

/// GPUI Global that allows the app-scoped channel poll task to open fresh windows
/// from an async context without holding a specific window handle.
struct WindowOpener {
    create_fn: Arc<OpenWindowFn>,
}

impl Global for WindowOpener {}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Parse --open-dir / --open-new-window args injected by `orcash open` on cold launch.
    // We do this before starting GPUI so the values are available inside app.run().
    let args: Vec<String> = std::env::args().collect();
    let cli_open_dir: Option<PathBuf> = args
        .windows(2)
        .find(|w| w[0] == "--open-dir")
        .map(|w| PathBuf::from(&w[1]));
    let cli_new_window = args.contains(&"--open-new-window".to_string());

    // Start embedded daemon before GPUI event loop.
    // IMPORTANT: _daemon must NOT be captured by the `move` closure passed to app.run(),
    // otherwise it gets dropped when the closure returns (killing the listener thread)
    // while the app is still running.  We extract what we need here and keep _daemon
    // alive in main() for the entire app lifetime.
    let (_daemon, daemon_error) = match default_endpoint() {
        Ok(endpoint) => match DaemonServer::start(&endpoint) {
            Ok(server) => (Some(server), None),
            Err(e) => {
                tracing::error!(
                    "DAEMON FAILED TO START: {e}. `orcash daemon status` will not work"
                );
                (None, Some(format!("Daemon unavailable: {e}")))
            }
        },
        Err(e) => {
            tracing::error!("Failed to resolve IPC endpoint: {e}. Daemon will not start");
            (None, Some(format!("Daemon unavailable: {e}")))
        }
    };

    // Ensure config directory exists
    let cfg_dir = config_dir();
    if let Err(e) = std::fs::create_dir_all(&cfg_dir) {
        tracing::error!("Failed to create config dir {}: {e}", cfg_dir.display());
    }

    // Open SQLite database
    let store = match Store::open(&database_path()) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!("Failed to open database: {e}. Starting with empty workspace");
            None
        }
    };
    let store = Arc::new(Mutex::new(store));
    let workspace_services = WorkspaceServices {
        git: GitCoordinator::new(),
        store: store.clone(),
    };

    // Extract the open-project receiver before entering app.run() so _daemon is
    // never captured by the move closure.  The channel itself is Arc-backed so the
    // receiver stays valid even though we only borrow _daemon here.
    let open_project_rx = _daemon.as_ref().map(|d| d.open_project_receiver());

    // Enqueue cold-launch open-dir request before app.run().  The poll task won't
    // drain the channel until the GPUI event loop starts, so ordering is safe.
    if let (Some(ref dir), Some(ref daemon)) = (&cli_open_dir, &_daemon) {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        let disp = if cli_new_window {
            OpenDisposition::NewWindow
        } else {
            OpenDisposition::NewTab
        };
        daemon.enqueue_open_project(canonical, disp);
    }

    // Copy bundled Quick Action workflows to ~/Library/Services/ so macOS discovers them.
    // Contents/Library/Services/ inside the app bundle is NOT auto-scanned by pbs;
    // ~/Library/Services/ is the only reliable user-level discovery path.
    #[cfg(target_os = "macos")]
    {
        if let Some(services_dst) = dirs::home_dir().map(|h| h.join("Library/Services")) {
            // Locate our app bundle's Services directory via the executable path:
            // .../OrcaShell.app/Contents/MacOS/orcashell → .../OrcaShell.app/Contents/Library/Services
            if let Ok(exe) = std::env::current_exe() {
                if let Some(macos_dir) = exe.parent() {
                    let bundled_services = macos_dir
                        .parent() // Contents
                        .map(|contents| contents.join("Library/Services"));
                    if let Some(src) = bundled_services {
                        if src.is_dir() {
                            if let Err(e) = std::fs::create_dir_all(&services_dst) {
                                tracing::warn!("Failed to create ~/Library/Services: {e}");
                            }
                            for name in &[
                                "New OrcaShell Tab Here.workflow",
                                "New OrcaShell Window Here.workflow",
                            ] {
                                let from = src.join(name);
                                let to = services_dst.join(name);
                                if from.is_dir() {
                                    // Remove stale copy and replace
                                    if let Err(e) = std::fs::remove_dir_all(&to) {
                                        if e.kind() != std::io::ErrorKind::NotFound {
                                            tracing::warn!(
                                                "Failed to remove old Quick Action {name}: {e}"
                                            );
                                        }
                                    }
                                    if let Err(e) = copy_dir_recursive(&from, &to) {
                                        tracing::warn!(
                                            "Failed to install Quick Action {name}: {e}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let app = Application::new().with_assets(assets::Assets);
    app.run(move |cx| {
        // Register bundled JetBrains Mono font
        cx.text_system()
            .add_fonts(assets::embedded_fonts())
            .expect("Failed to register embedded fonts");

        // Register AppSettings as a GPUI Global
        register_settings(cx);
        register_theme(cx);

        // Register WindowRegistry as a GPUI Global
        cx.set_global(WindowRegistry::new());

        // Register keybindings - dual bindings for macOS (cmd) and Linux (ctrl/ctrl-shift)
        cx.bind_keys([
            // ── Clipboard ──
            KeyBinding::new("cmd-c", Copy, None),
            KeyBinding::new("ctrl-shift-c", Copy, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("ctrl-shift-v", Paste, None),
            // ── Window operations ──
            KeyBinding::new("cmd-shift-n", NewWindow, None),
            KeyBinding::new("ctrl-shift-n", NewWindow, None),
            // ── Pane operations ──
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("ctrl-shift-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("ctrl-shift-e", SplitDown, None),
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("ctrl-shift-t", NewTab, None),
            KeyBinding::new("cmd-w", ClosePane, None),
            KeyBinding::new("ctrl-shift-w", ClosePane, None),
            KeyBinding::new("cmd-shift-w", CloseTab, None),
            KeyBinding::new("ctrl-shift-q", CloseTab, None),
            // ── Focus navigation ──
            KeyBinding::new("cmd-]", FocusNextPane, None),
            KeyBinding::new("ctrl-shift-]", FocusNextPane, None),
            KeyBinding::new("cmd-[", FocusPrevPane, None),
            KeyBinding::new("ctrl-shift-[", FocusPrevPane, None),
            // ── Tab navigation ──
            KeyBinding::new("cmd-shift-]", NextTab, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("cmd-shift-[", PrevTab, None),
            KeyBinding::new("ctrl-shift-tab", PrevTab, None),
            // ── Direct tab switching ──
            KeyBinding::new("cmd-1", GotoTab1, None),
            KeyBinding::new("ctrl-1", GotoTab1, None),
            KeyBinding::new("cmd-2", GotoTab2, None),
            KeyBinding::new("ctrl-2", GotoTab2, None),
            KeyBinding::new("cmd-3", GotoTab3, None),
            KeyBinding::new("ctrl-3", GotoTab3, None),
            KeyBinding::new("cmd-4", GotoTab4, None),
            KeyBinding::new("ctrl-4", GotoTab4, None),
            KeyBinding::new("cmd-5", GotoTab5, None),
            KeyBinding::new("ctrl-5", GotoTab5, None),
            KeyBinding::new("cmd-6", GotoTab6, None),
            KeyBinding::new("ctrl-6", GotoTab6, None),
            KeyBinding::new("cmd-7", GotoTab7, None),
            KeyBinding::new("ctrl-7", GotoTab7, None),
            KeyBinding::new("cmd-8", GotoTab8, None),
            KeyBinding::new("ctrl-8", GotoTab8, None),
            KeyBinding::new("cmd-9", GotoTab9, None),
            KeyBinding::new("ctrl-9", GotoTab9, None),
            // ── Terminal operations ──
            KeyBinding::new("cmd-k", ClearScrollback, None),
            KeyBinding::new("ctrl-shift-k", ClearScrollback, None),
            KeyBinding::new("cmd-=", ZoomIn, None),
            KeyBinding::new("ctrl-=", ZoomIn, None),
            KeyBinding::new("cmd--", ZoomOut, None),
            KeyBinding::new("ctrl--", ZoomOut, None),
            KeyBinding::new("cmd-0", ResetZoom, None),
            KeyBinding::new("ctrl-0", ResetZoom, None),
            // ── Search ──
            KeyBinding::new("cmd-f", SearchFind, None),
            KeyBinding::new("ctrl-shift-f", SearchFind, None),
            KeyBinding::new("escape", SearchDismiss, Some("SearchBar")),
            // ── Context menu ──
            KeyBinding::new("escape", DismissMenu, Some("ContextMenu")),
            // ── Sidebar ──
            KeyBinding::new("cmd-shift-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-shift-b", ToggleSidebar, None),
            KeyBinding::new("cmd-shift-o", AddProject, None),
            KeyBinding::new("ctrl-shift-o", AddProject, None),
            // ── Settings ──
            KeyBinding::new("cmd-,", ToggleSettings, None),
            KeyBinding::new("ctrl-,", ToggleSettings, None),
        ]);

        // ── Application menu bar (macOS native only) ──
        #[cfg(target_os = "macos")]
        set_app_menus(cx);

        // ── QuitApp action handler ──
        cx.on_action(|_: &QuitApp, cx| {
            cx.quit();
        });

        // ── Load saved windows from DB ──
        let windows_to_restore = {
            let guard = store.lock();
            match guard.as_ref() {
                Some(s) => s.load_windows().unwrap_or_default(),
                None => vec![],
            }
        };

        if windows_to_restore.is_empty() {
            // Fresh install or no DB. Open a single default window
            let projects = {
                let guard = store.lock();
                match guard.as_ref() {
                    Some(s) => s.load_projects_for_window(1).unwrap_or_default(),
                    None => vec![],
                }
            };
            open_orca_window(
                cx,
                1,
                daemon_error.clone(),
                projects,
                None,
                workspace_services.clone(),
                None,
                None,
            );
        } else {
            // Restore all saved windows
            for sw in &windows_to_restore {
                let projects = {
                    let guard = store.lock();
                    match guard.as_ref() {
                        Some(s) => s.load_projects_for_window(sw.window_id).unwrap_or_default(),
                        None => vec![],
                    }
                };
                let bounds = sw.bounds_x.map(|x| {
                    (
                        x,
                        sw.bounds_y.unwrap_or(0.0),
                        sw.bounds_width,
                        sw.bounds_height,
                    )
                });
                open_orca_window(
                    cx,
                    sw.window_id,
                    daemon_error.clone(),
                    projects,
                    sw.active_project_id.clone(),
                    workspace_services.clone(),
                    bounds,
                    None,
                );
            }
        }

        // ── WindowOpener global ──
        // Captured once here so the poll task can open fresh windows from async context.
        {
            let daemon_error_for_opener = daemon_error.clone();
            let services_for_opener = workspace_services.clone();
            let store_for_opener = store.clone();
            cx.set_global(WindowOpener {
                create_fn: Arc::new(move |initial_dir, cx| {
                    create_fresh_window(
                        cx,
                        daemon_error_for_opener.clone(),
                        services_for_opener.clone(),
                        store_for_opener.clone(),
                        initial_dir,
                    );
                }),
            });
        }

        // ── App-scoped open-project poll task ──
        // Drains the daemon's open-project channel and routes each request to the right
        // window. App::spawn gives the task app-scoped lifetime. It survives individual
        // window closures, so routing is always available as long as the app is running.
        // NOTE: open_project_rx was extracted before app.run() to avoid capturing _daemon.
        if let Some(rx) = open_project_rx {
            cx.spawn(async move |cx: &mut AsyncApp| {
                while let Ok((path, disposition)) = rx.recv().await {
                    let _ = cx.update(|cx| {
                        match disposition {
                            OpenDisposition::NewTab => {
                                // Route to the most-recently opened window (highest ID).
                                // Window IDs are assigned in monotonically increasing order,
                                // so sorting descending by ID gives us registration order.
                                let mut entries: Vec<(i64, AnyWindowHandle)> =
                                    cx.global::<WindowRegistry>().iter().collect();
                                entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));
                                for (_, handle) in entries.iter() {
                                    if let Some(typed) = handle.downcast::<OrcaAppView>() {
                                        let routed = typed
                                            .update(cx, |app_view, window, cx| {
                                                app_view.workspace().update(cx, |ws: &mut orcashell_ui::workspace::WorkspaceState, cx| {
                                                    ws.open_directory(path.clone(), cx);
                                                });
                                                window.activate_window();
                                            })
                                            .is_ok();
                                        if routed {
                                            cx.activate(true);
                                            break;
                                        }
                                    }
                                }
                            }
                            OpenDisposition::NewWindow => {
                                let create_fn =
                                    cx.global::<WindowOpener>().create_fn.clone();
                                create_fn(Some(path), cx);
                            }
                        }
                    });
                }
            })
            .detach();
        }

        // ── NewWindow action handler (global, applies to all windows) ──
        {
            let store = store.clone();
            let daemon_error = daemon_error.clone();
            cx.on_action(move |_: &NewWindow, cx| {
                // Check for a hibernated window to restore first
                let hibernated = {
                    let guard = store.lock();
                    guard
                        .as_ref()
                        .and_then(|s| s.load_hibernated_window().ok().flatten())
                };

                if let Some(sw) = hibernated {
                    // Restore the hibernated window with its saved state
                    let projects = {
                        let guard = store.lock();
                        match guard.as_ref() {
                            Some(s) => s.load_projects_for_window(sw.window_id).unwrap_or_default(),
                            None => vec![],
                        }
                    };
                    let bounds = sw.bounds_x.map(|x| {
                        (
                            x,
                            sw.bounds_y.unwrap_or(0.0),
                            sw.bounds_width,
                            sw.bounds_height,
                        )
                    });
                    open_orca_window(
                        cx,
                        sw.window_id,
                        daemon_error.clone(),
                        projects,
                        sw.active_project_id.clone(),
                        workspace_services.clone(),
                        bounds,
                        None,
                    );
                } else {
                    // No hibernated windows. Open a fresh one at cwd.
                    create_fresh_window(
                        cx,
                        daemon_error.clone(),
                        workspace_services.clone(),
                        store.clone(),
                        None,
                    );
                }
            });
        }

        // ── Quit the app when the last window is closed ──
        cx.on_window_closed({
            let store = store.clone();
            move |cx| {
                // Find which windows were closed by diffing registry against live set
                let live = cx.windows();
                let closed_ids = cx.global::<WindowRegistry>().find_closed(&live);
                let registry = cx.global_mut::<WindowRegistry>();
                for id in &closed_ids {
                    registry.unregister(*id);
                }

                if live.is_empty() {
                    // Last window. Don't hibernate it, keep is_open = true
                    // so it restores on next launch. Just save settings and quit.
                    if let Some(s) = cx.try_global::<AppSettings>() {
                        let _ = s.0.save();
                    }
                    cx.quit();
                } else {
                    // Not the last window. Hibernate it (preserve state for reactivation)
                    let guard = store.lock();
                    if let Some(s) = guard.as_ref() {
                        for id in &closed_ids {
                            if let Err(e) = s.hibernate_window(*id) {
                                tracing::error!("Failed to hibernate window {id}: {e}");
                            }
                        }
                    }
                }
            }
        })
        .detach();

        // ── Final save on app quit. Fires for all quit paths (Cmd+Q, etc.) ──
        cx.on_app_quit({
            move |cx| {
                // Save all open windows via typed WindowHandle
                let entries: Vec<(i64, AnyWindowHandle)> =
                    cx.global::<WindowRegistry>().iter().collect();
                for (window_id, handle) in entries {
                    if let Some(typed) = handle.downcast::<OrcaAppView>() {
                        let _ = typed.update(cx, |app_view, window, cx| {
                            let b = window.bounds();
                            let bounds = Some((
                                f32::from(b.origin.x),
                                f32::from(b.origin.y),
                                f32::from(b.size.width),
                                f32::from(b.size.height),
                            ));
                            app_view.save_on_quit(bounds, cx);
                        });
                    } else {
                        tracing::warn!("Window {window_id} could not be accessed during quit");
                    }
                }

                // Save settings once
                if let Some(s) = cx.try_global::<AppSettings>() {
                    let _ = s.0.save();
                }

                async {}
            }
        })
        .detach();
    });

    Ok(())
}

/// Open an OrcaShell window with the given ID, projects, and optional bounds.
///
/// `initial_dir` is only meaningful when `stored_projects` is empty. It sets the
/// working directory for the first terminal instead of the process cwd.
#[allow(clippy::too_many_arguments)]
fn open_orca_window(
    cx: &mut App,
    window_id: i64,
    daemon_error: Option<String>,
    stored_projects: Vec<StoredProject>,
    active_project_id: Option<String>,
    services: WorkspaceServices,
    bounds: Option<(f32, f32, f32, f32)>,
    initial_dir: Option<PathBuf>,
) {
    let validated = validate_window_state(bounds, cx);

    let title = if window_id == 1 {
        "OrcaShell".to_string()
    } else {
        format!("OrcaShell - {window_id}")
    };

    let mut window_opts = WindowOptions {
        titlebar: if cfg!(target_os = "windows") {
            None
        } else {
            Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                traffic_light_position: if cfg!(target_os = "macos") {
                    Some(point(px(8.0), px(8.0)))
                } else {
                    None
                },
            })
        },
        window_decorations: if cfg!(target_os = "windows") {
            Some(WindowDecorations::Client)
        } else {
            None
        },
        ..Default::default()
    };

    if let Some((x, y, w, h)) = validated {
        window_opts.window_bounds = Some(WindowBounds::Windowed(Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }));
    }

    let window_handle = cx
        .open_window(window_opts, |window, cx| {
            update_window_appearance(window.appearance(), cx);
            let app_view = cx.new(|cx| {
                cx.observe_window_appearance(window, |_this, window, cx| {
                    update_window_appearance(window.appearance(), cx);
                    cx.notify();
                })
                .detach();
                OrcaAppView::new(
                    window_id,
                    cx,
                    daemon_error,
                    stored_projects,
                    active_project_id,
                    services,
                    initial_dir,
                )
            });
            if let Some(focus) = app_view.read(cx).terminal_focus_handle(cx) {
                focus.focus(window);
            }
            app_view
        })
        .expect("Failed to create window");

    // Set the window handle on the OrcaAppView for per-window bounds saving
    let any_handle: AnyWindowHandle = window_handle.into();
    window_handle
        .update(cx, |app_view, _window, _cx| {
            app_view.set_window_handle(any_handle);
        })
        .ok();

    // Register in WindowRegistry
    cx.global_mut::<WindowRegistry>()
        .register(window_id, any_handle);
}

/// Open a fresh (no saved state) OrcaShell window, optionally rooted at `initial_dir`.
///
/// Used by the `WindowOpener` global and the `NewWindow` "no hibernated state" branch.
/// Distinct from the `NewWindow` handler's hibernation-restore path.
///
/// When `initial_dir` is `Some`, `OrcaAppView::new` will call `init_with_project(initial_dir)`
/// so only one terminal is created. At the requested directory.
/// When `initial_dir` is `None`, the window starts at the process cwd as usual.
fn create_fresh_window(
    cx: &mut App,
    daemon_error: Option<String>,
    services: WorkspaceServices,
    store: Arc<Mutex<Option<Store>>>,
    initial_dir: Option<PathBuf>,
) {
    let next_id = {
        let db_max = {
            let guard = store.lock();
            guard
                .as_ref()
                .and_then(|s| s.next_window_id().ok())
                .unwrap_or(2)
        };
        let reg_max = cx
            .global::<WindowRegistry>()
            .iter()
            .map(|(id, _)| id)
            .max()
            .map(|m| m + 1)
            .unwrap_or(2);
        db_max.max(reg_max)
    };

    let bounds = cascade_bounds(cx);
    open_orca_window(
        cx,
        next_id,
        daemon_error,
        vec![],
        None,
        services,
        bounds,
        initial_dir,
    );
}

/// Get cascaded bounds for a new window, offset +30,+30 from the last open window.
fn cascade_bounds(cx: &mut App) -> Option<(f32, f32, f32, f32)> {
    let registry = cx.global::<WindowRegistry>();
    // Find the highest-numbered window to cascade from
    let mut best: Option<(i64, AnyWindowHandle)> = None;
    for (id, handle) in registry.iter() {
        if best.is_none() || id > best.unwrap().0 {
            best = Some((id, handle));
        }
    }
    let (_, handle) = best?;
    handle
        .update(cx, |_any_view, window, _cx| {
            let b = window.bounds();
            (
                f32::from(b.origin.x) + 30.0,
                f32::from(b.origin.y) + 30.0,
                f32::from(b.size.width),
                f32::from(b.size.height),
            )
        })
        .ok()
}

/// Set up the native application menu bar.
#[cfg(target_os = "macos")]
fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu {
            name: "OrcaShell".into(),
            items: vec![
                MenuItem::action("Check for Updates...", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Settings...", ToggleSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit OrcaShell", QuitApp),
            ],
        },
        Menu {
            name: "Shell".into(),
            items: vec![
                MenuItem::action("New Window", NewWindow),
                MenuItem::action("New Tab", NewTab),
                MenuItem::separator(),
                MenuItem::action("Close Pane", ClosePane),
                MenuItem::action("Close Tab", CloseTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::action("Find...", SearchFind),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Reset Zoom", ResetZoom),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
            ],
        },
    ]);
}

/// Validate saved window position against connected displays.
/// Returns `None` if the window would be completely off-screen.
fn validate_window_state(
    state: Option<(f32, f32, f32, f32)>,
    cx: &App,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = state?;

    let displays = cx.displays();
    if displays.is_empty() {
        return Some((x, y, w, h));
    }

    let display_rects: Vec<(f32, f32, f32, f32)> = displays
        .iter()
        .map(|d| {
            let db = d.bounds();
            (
                f32::from(db.origin.x),
                f32::from(db.origin.y),
                f32::from(db.size.width),
                f32::from(db.size.height),
            )
        })
        .collect();

    if is_visible_on_displays((x, y, w, h), &display_rects) {
        Some((x, y, w, h))
    } else {
        tracing::info!("Saved window position ({x}, {y}, {w}x{h}) is off-screen, using default");
        None
    }
}

/// Recursively copy a directory tree. Used to install Quick Action workflows.
#[cfg(target_os = "macos")]
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

/// Check if a window rectangle overlaps any display rectangle.
fn is_visible_on_displays(window: (f32, f32, f32, f32), displays: &[(f32, f32, f32, f32)]) -> bool {
    let (wx, wy, ww, wh) = window;
    displays
        .iter()
        .any(|&(dx, dy, dw, dh)| wx < dx + dw && wx + ww > dx && wy < dy + dh && wy + wh > dy)
}

#[cfg(test)]
mod tests;
