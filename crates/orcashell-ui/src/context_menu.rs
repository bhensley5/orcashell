use gpui::*;

use crate::theme;
use crate::workspace::WorkspaceState;

actions!(orcashell, [DismissMenu]);

/// Return a platform-aware shortcut label (macOS symbol vs Ctrl-based text).
pub fn platform_shortcut(mac: &str, other: &str) -> String {
    if cfg!(target_os = "macos") {
        mac.to_string()
    } else {
        other.to_string()
    }
}

/// Type alias for context menu action closures.
pub type MenuAction = Box<dyn Fn(&mut WorkspaceState, &mut Context<WorkspaceState>) + 'static>;

/// A single item in a context menu.
pub struct ContextMenuItem {
    pub label: String,
    pub shortcut: Option<String>,
    pub action: MenuAction,
    pub enabled: bool,
}

/// Event emitted when the context menu should be dismissed.
pub enum ContextMenuEvent {
    Dismiss,
}

impl EventEmitter<ContextMenuEvent> for ContextMenuOverlay {}

/// Overlay context menu entity.
pub struct ContextMenuOverlay {
    position: Point<Pixels>,
    items: Vec<ContextMenuItem>,
    workspace: Entity<WorkspaceState>,
    focus_handle: FocusHandle,
}

impl ContextMenuOverlay {
    pub fn new(
        position: Point<Pixels>,
        items: Vec<ContextMenuItem>,
        workspace: Entity<WorkspaceState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            position,
            items,
            workspace,
            focus_handle,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ContextMenuEvent::Dismiss);
    }
}

impl Render for ContextMenuOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        // Focus the menu so Escape works
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle);
        }

        let position = self.position;

        // Build menu items
        let mut menu_children: Vec<AnyElement> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let enabled = item.enabled;
            let label = item.label.clone();
            let shortcut = item.shortcut.clone();

            let mut item_div = div()
                .id(ElementId::Name(format!("ctx-item-{}", i).into()))
                .px(px(12.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .justify_between()
                .min_w(px(160.0))
                .text_size(px(12.0));

            if enabled {
                item_div = item_div
                    .cursor_pointer()
                    .text_color(rgb(palette.BONE))
                    .hover(|s| s.bg(rgb(palette.ORCA_BLUE)));
            } else {
                item_div = item_div.text_color(rgb(palette.SLATE));
            }

            item_div = item_div.child(div().flex_1().child(label));

            if let Some(sc) = shortcut {
                item_div = item_div.child(
                    div()
                        .ml(px(16.0))
                        .text_size(px(10.0))
                        .text_color(rgb(palette.SLATE))
                        .flex_shrink_0()
                        .child(sc),
                );
            }

            if enabled {
                let item_index = i;
                item_div = item_div.on_click(cx.listener(move |this, _event, _window, cx| {
                    let ws = this.workspace.clone();
                    if item_index < this.items.len() {
                        let action = &this.items[item_index].action;
                        ws.update(cx, |ws_state, ws_cx| {
                            action(ws_state, ws_cx);
                        });
                    }
                    this.dismiss(cx);
                }));
            }

            menu_children.push(item_div.into_any_element());
        }

        // Full-window backdrop + positioned menu panel
        div()
            .track_focus(&self.focus_handle)
            .key_context("ContextMenu")
            .on_action(cx.listener(|this, _: &DismissMenu, _window, cx| {
                this.dismiss(cx);
            }))
            .absolute()
            .inset_0()
            .size_full()
            .id("ctx-backdrop")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.dismiss(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _window, cx| {
                    this.dismiss(cx);
                }),
            )
            .child(deferred(
                anchored().position(position).snap_to_window().child(
                    div()
                        .id("ctx-panel")
                        .bg(rgb(palette.DEEP))
                        .border_1()
                        .border_color(rgb(palette.SURFACE))
                        .rounded(px(4.0))
                        .shadow_xl()
                        .py(px(4.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .children(menu_children),
                ),
            ))
    }
}

/// Build a separator element for use between menu items.
pub fn menu_separator() -> Div {
    let palette = theme::OrcaTheme::default();
    div()
        .h(px(1.0))
        .mx(px(8.0))
        .my(px(4.0))
        .bg(rgb(palette.SURFACE))
}
