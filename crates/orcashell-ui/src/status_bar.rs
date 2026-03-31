use gpui::*;

use crate::theme;
use crate::workspace::layout::LayoutNode;
use crate::workspace::WorkspaceState;

pub struct StatusBar {
    workspace: Entity<WorkspaceState>,
}

impl StatusBar {
    pub fn new(workspace: Entity<WorkspaceState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&workspace, |_this, _ws, cx| cx.notify())
            .detach();
        Self { workspace }
    }
}

/// Contract a path with the home directory prefix replaced by ~.
fn tilde_contract(path: &std::path::Path) -> String {
    if let Some(home) = orcashell_platform::user_home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ws = self.workspace.read(cx);

        let project_name = ws
            .active_project()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        let project_path = ws
            .active_project()
            .map(|p| tilde_contract(&p.path))
            .unwrap_or_default();

        // Pane position: current / total
        let (pane_pos, pane_total) = ws
            .active_project()
            .map(|p| {
                let paths = p.layout.collect_terminal_paths();
                let total = paths.len();
                let current = ws
                    .focus
                    .current_target()
                    .and_then(|t| paths.iter().position(|path| *path == t.layout_path))
                    .map(|i| i + 1)
                    .unwrap_or(1);
                (current, total)
            })
            .unwrap_or((0, 0));

        let left_text = if project_path.is_empty() {
            project_name.clone()
        } else {
            format!("{} \u{00B7} {}", project_name, project_path)
        };

        let shell_label = ws
            .focus
            .current_target()
            .and_then(|target| {
                let project = ws.project(&target.project_id)?;
                match project.layout.get_at_path(&target.layout_path)? {
                    LayoutNode::Terminal {
                        terminal_id: Some(tid),
                        ..
                    } => ws.terminal_runtime.get(tid),
                    _ => None,
                }
            })
            .map(|rt| rt.shell_label.as_str())
            .unwrap_or("shell");

        let right_text = if pane_total > 0 {
            format!("{}  {}/{}", shell_label, pane_pos, pane_total)
        } else {
            String::new()
        };

        div()
            .w_full()
            .h(px(24.0))
            .flex_shrink_0()
            .bg(rgb(theme::ABYSS))
            .border_t_1()
            .border_color(rgb(theme::SURFACE))
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .text_size(px(11.0))
            .text_color(rgb(theme::FOG))
            .child(div().child(left_text))
            .child(div().child(right_text))
    }
}
