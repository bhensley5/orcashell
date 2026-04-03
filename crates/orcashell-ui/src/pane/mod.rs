pub mod resize;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::app_view::ContextMenuRequest;
use crate::context_menu::{platform_shortcut, ContextMenuItem};
use crate::theme::{self, OrcaTheme};
use crate::workspace::layout::{LayoutNode, SplitDirection};
use crate::workspace::WorkspaceState;
use resize::{ActiveDrag, DragState};

/// Recursive GPUI view that renders the pane tree.
/// Each LayoutContainer corresponds to a node in the LayoutNode tree.
/// Entity caching ensures TerminalView and child LayoutContainer entities
/// are created once and reused across renders.
pub struct LayoutContainer {
    workspace: Entity<WorkspaceState>,
    layout_path: Vec<usize>,
    child_containers: HashMap<Vec<usize>, Entity<LayoutContainer>>,
    container_bounds: Rc<RefCell<Bounds<Pixels>>>,
    active_drag: ActiveDrag,
    menu_request: ContextMenuRequest,
}

impl LayoutContainer {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        layout_path: Vec<usize>,
        active_drag: ActiveDrag,
        menu_request: ContextMenuRequest,
    ) -> Self {
        Self {
            workspace,
            layout_path,
            child_containers: HashMap::new(),
            container_bounds: Rc::new(RefCell::new(Bounds::default())),
            active_drag,
            menu_request,
        }
    }

    /// Update the layout path. Clears cached children since their paths are stale.
    pub fn set_layout_path(&mut self, path: Vec<usize>) {
        if self.layout_path != path {
            self.layout_path = path;
            self.child_containers.clear();
        }
    }

    /// Get or create a child LayoutContainer entity for the given path.
    fn ensure_child_container(
        &mut self,
        child_path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> Entity<LayoutContainer> {
        self.child_containers
            .entry(child_path.clone())
            .or_insert_with(|| {
                let ws = self.workspace.clone();
                let drag = self.active_drag.clone();
                let menu = self.menu_request.clone();
                cx.new(|_| LayoutContainer::new(ws, child_path, drag, menu))
            })
            .clone()
    }

    /// Clean up child containers that no longer exist in the layout.
    fn retain_valid_children(&mut self, child_count: usize) {
        let base_path = &self.layout_path;
        self.child_containers.retain(|path, _| {
            if path.len() != base_path.len() + 1 {
                return false;
            }
            if !path.starts_with(base_path) {
                return false;
            }
            let idx = path[base_path.len()];
            idx < child_count
        });
    }

    fn render_terminal(
        &self,
        terminal_id: Option<&String>,
        project_id: &str,
        cx: &App,
    ) -> impl IntoElement {
        let palette = theme::active(cx);
        let content = if let Some(tid) = terminal_id {
            let ws = self.workspace.read(cx);
            if let Some(view) = ws.terminal_view(tid) {
                div().size_full().min_h_0().min_w_0().child(view.clone())
            } else {
                self.render_placeholder(&palette)
            }
        } else {
            self.render_placeholder(&palette)
        };

        // Wrap with click-to-focus
        let workspace = self.workspace.clone();
        let pid = project_id.to_string();
        let path = self.layout_path.clone();
        let element_id = format!("pane-{}-{:?}", project_id, self.layout_path);

        div()
            .id(ElementId::Name(element_id.into()))
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(content)
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                workspace.update(cx, |ws, cx| {
                    ws.focus_pane(pid.clone(), path.clone(), cx);
                });
            })
            .on_mouse_down(MouseButton::Right, {
                let menu_request = self.menu_request.clone();
                let ws_notify = self.workspace.clone();
                let ws_focus = self.workspace.clone();
                let focus_pid = project_id.to_string();
                let focus_path = self.layout_path.clone();
                move |event: &MouseDownEvent, _window, cx| {
                    // Focus this pane first so context menu actions target the right pane
                    ws_focus.update(cx, |ws, cx| {
                        ws.focus_pane(focus_pid.clone(), focus_path.clone(), cx);
                    });
                    let items = vec![
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
                        ContextMenuItem {
                            label: "New Tab".into(),
                            shortcut: Some(platform_shortcut("\u{2318}T", "Ctrl+Shift+T")),
                            action: Box::new(|ws, cx| {
                                ws.new_tab_focused(cx);
                            }),
                            enabled: true,
                        },
                        ContextMenuItem {
                            label: "Close Pane".into(),
                            shortcut: Some(platform_shortcut("\u{2318}W", "Ctrl+Shift+W")),
                            action: Box::new(|ws, cx| {
                                ws.close_focused(cx);
                            }),
                            enabled: true,
                        },
                    ];
                    *menu_request.borrow_mut() = Some((event.position, items));
                    ws_notify.update(cx, |_ws, cx| cx.notify());
                }
            })
    }

    fn render_placeholder(&self, palette: &OrcaTheme) -> Div {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(palette.FOG))
            .text_size(px(13.0))
            .child("Starting terminal...")
    }

    fn render_split(
        &mut self,
        direction: &SplitDirection,
        sizes: &[f32],
        children: &[LayoutNode],
        project_id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_horizontal = *direction == SplitDirection::Horizontal;

        // Clean up stale child containers
        self.retain_valid_children(children.len());

        // Normalize sizes to percentages
        let palette = theme::active(cx);
        let total_size: f32 = sizes.iter().sum();
        let normalized: Vec<f32> = if total_size > 0.0 {
            sizes.iter().map(|s| s / total_size * 100.0).collect()
        } else {
            let equal = 100.0 / children.len().max(1) as f32;
            vec![equal; children.len()]
        };

        // Build interleaved children and dividers
        let element_count = children.len() * 2 - children.len().min(1);
        let mut elements: Vec<AnyElement> = Vec::with_capacity(element_count);

        for (i, _child) in children.iter().enumerate() {
            // Add divider before this child (if not first)
            if i > 0 {
                let divider = self.render_divider(
                    &palette,
                    is_horizontal,
                    i - 1,
                    i,
                    direction.clone(),
                    project_id,
                    sizes,
                );
                elements.push(divider.into_any_element());
            }

            // Get or create child container
            let mut child_path = self.layout_path.clone();
            child_path.push(i);
            let container = self.ensure_child_container(child_path, cx);

            let size_pct = normalized[i];
            let child_el = div()
                .flex_basis(relative(size_pct / 100.0))
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(container)
                .into_any_element();

            elements.push(child_el);
        }

        // Container with canvas for bounds capture
        let bounds_ref = self.container_bounds.clone();
        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .when(is_horizontal, |d| d.flex_col())
            .flex_nowrap()
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
            .children(elements)
    }

    /// Render a draggable divider between split children.
    #[allow(clippy::too_many_arguments)]
    fn render_divider(
        &self,
        palette: &OrcaTheme,
        is_horizontal: bool,
        left_index: usize,
        right_index: usize,
        direction: SplitDirection,
        project_id: &str,
        sizes: &[f32],
    ) -> impl IntoElement {
        let active_drag = self.active_drag.clone();
        let split_path = self.layout_path.clone();
        let pid = project_id.to_string();
        let initial_sizes = sizes.to_vec();
        let container_bounds = self.container_bounds.clone();
        let divider_id = format!(
            "divider-{}-{:?}-{}-{}",
            project_id, self.layout_path, left_index, right_index
        );

        // The visible divider line: 1px
        // The hit target: 4px (centered on the 1px line)
        div()
            .id(ElementId::Name(divider_id.into()))
            .flex_shrink_0()
            .when(is_horizontal, |d| d.w_full().h(px(4.0)))
            .when(!is_horizontal, |d| d.h_full().w(px(4.0)))
            .flex()
            .items_center()
            .justify_center()
            .when(is_horizontal, |d| d.cursor_row_resize())
            .when(!is_horizontal, |d| d.cursor_col_resize())
            .child(
                // The visible 1px line
                div()
                    .when(is_horizontal, |d| d.w_full().h(px(1.0)))
                    .when(!is_horizontal, |d| d.h_full().w(px(1.0)))
                    .bg(rgb(palette.SURFACE)),
            )
            .on_mouse_down(MouseButton::Left, {
                move |event: &MouseDownEvent, _window, _cx| {
                    let bounds = *container_bounds.borrow();
                    *active_drag.borrow_mut() = Some(DragState {
                        project_id: pid.clone(),
                        split_path: split_path.clone(),
                        left_index,
                        right_index,
                        direction: direction.clone(),
                        container_bounds: bounds,
                        initial_mouse: event.position,
                        initial_sizes: initial_sizes.clone(),
                    });
                }
            })
    }

    fn render_tabs(
        &mut self,
        children: &[LayoutNode],
        active_tab: usize,
        _project_id: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Nested Tabs are disallowed. Window-level tab bar handles all tabs.
        // This is only reached if the layout tree is in an unexpected state.
        tracing::warn!(
            "LayoutContainer encountered nested Tabs at path {:?}. This should not happen.",
            self.layout_path
        );

        // Gracefully render the active tab's content without an inline tab bar
        self.retain_valid_children(children.len());
        let active_idx = active_tab.min(children.len().saturating_sub(1));
        let mut child_path = self.layout_path.clone();
        child_path.push(active_idx);
        let container = self.ensure_child_container(child_path, cx);

        div().size_full().min_h_0().child(container)
    }
}

impl Render for LayoutContainer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        // Read active project and snapshot the layout node at our path.
        // Clone releases the workspace borrow before mutable render methods.
        let (project_id, layout_snapshot) = {
            let ws = self.workspace.read(cx);
            let pid = ws.active_project_id.clone();
            let layout = pid.as_ref().and_then(|id| {
                let project = ws.project(id)?;
                if self.layout_path.is_empty() {
                    Some(project.layout.clone())
                } else {
                    project.layout.get_at_path(&self.layout_path).cloned()
                }
            });
            (pid.unwrap_or_default(), layout)
        };

        // Clean up caches when layout type changes
        match &layout_snapshot {
            Some(LayoutNode::Terminal { .. }) => {
                if !self.child_containers.is_empty() {
                    self.child_containers.clear();
                }
            }
            Some(LayoutNode::Split { .. }) | Some(LayoutNode::Tabs { .. }) => {}
            None => {
                self.child_containers.clear();
            }
        }

        match layout_snapshot {
            Some(LayoutNode::Terminal {
                ref terminal_id, ..
            }) => self
                .render_terminal(terminal_id.as_ref(), &project_id, cx)
                .into_any_element(),
            Some(LayoutNode::Split {
                ref direction,
                ref sizes,
                ref children,
            }) => self
                .render_split(direction, sizes, children, &project_id, cx)
                .into_any_element(),
            Some(LayoutNode::Tabs {
                ref children,
                active_tab,
            }) => self
                .render_tabs(children, active_tab, &project_id, cx)
                .into_any_element(),
            None => div()
                .text_color(rgb(palette.FOG))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.0))
                .child("No layout")
                .into_any_element(),
        }
    }
}
