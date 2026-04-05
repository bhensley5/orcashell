use std::time::{Duration, SystemTime};

use gpui::*;
use orcashell_git::{DiffLineKind, DiffSelectionKey};

use crate::feed_detail_view::FeedDetailView;
use crate::theme;
use crate::workspace::{
    ChangeFeedEntry, FeedCaptureState, FeedEntryOrigin, FeedScopeKind, WorkspaceState,
};

const FEED_CARD_PREVIEW_LINE_HEIGHT: f32 = 16.0;
const FEED_LIST_OVERDRAW: f32 = 320.0;
const FEED_ROW_GAP: f32 = 10.0;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FeedListSnapshot {
    entry_ids: Vec<u64>,
}

pub struct LiveDiffStreamView {
    workspace: Entity<WorkspaceState>,
    project_id: String,
    feed_list_state: ListState,
    last_feed_snapshot: FeedListSnapshot,
    /// Optional detail view entity for showing a captured feed entry's diff.
    detail_view: Option<Entity<FeedDetailView>>,
    /// The entry ID that the current detail_view was built for.
    detail_entry_id: Option<u64>,
}

impl LiveDiffStreamView {
    pub fn new(
        workspace: Entity<WorkspaceState>,
        project_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_for_list = workspace.clone();
        let project_id_for_list = project_id.clone();
        let initial_snapshot = Self::capture_feed_snapshot(&workspace, &project_id, cx);
        let this = Self {
            workspace,
            project_id,
            feed_list_state: Self::build_feed_list_state(
                initial_snapshot.entry_ids.len(),
                workspace_for_list,
                project_id_for_list,
            ),
            last_feed_snapshot: initial_snapshot,
            detail_view: None,
            detail_entry_id: None,
        };

        cx.observe(&this.workspace, |this, _workspace, cx| {
            this.sync_feed_list_state(cx);
            cx.notify();
        })
        .detach();

        this
    }

    fn build_feed_list_state(
        item_count: usize,
        workspace: Entity<WorkspaceState>,
        project_id: String,
    ) -> ListState {
        let list_state = ListState::new(item_count, ListAlignment::Bottom, px(FEED_LIST_OVERDRAW));
        list_state.set_scroll_handler(move |event, _window, cx| {
            workspace.update(cx, |workspace, _cx| {
                workspace.set_live_diff_feed_follow_state(&project_id, !event.is_scrolled);
            });
        });
        list_state
    }

    fn capture_feed_snapshot(
        workspace: &Entity<WorkspaceState>,
        project_id: &str,
        cx: &App,
    ) -> FeedListSnapshot {
        let entry_ids = workspace
            .read(cx)
            .live_diff_feed_state(project_id)
            .map(|feed| feed.entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
        FeedListSnapshot { entry_ids }
    }

    fn sync_feed_list_state(&mut self, cx: &mut Context<Self>) {
        let snapshot = Self::capture_feed_snapshot(&self.workspace, &self.project_id, cx);
        if snapshot == self.last_feed_snapshot {
            return;
        }

        let old_count = self.last_feed_snapshot.entry_ids.len();
        let new_count = snapshot.entry_ids.len();
        if new_count >= old_count
            && snapshot.entry_ids[..old_count] == self.last_feed_snapshot.entry_ids[..]
        {
            self.feed_list_state
                .splice(old_count..old_count, new_count - old_count);
        } else {
            self.feed_list_state.reset(new_count);
        }
        self.last_feed_snapshot = snapshot;
    }

    fn scroll_to_live(&mut self, cx: &mut Context<Self>) {
        let item_count = self.last_feed_snapshot.entry_ids.len();
        self.feed_list_state.reset(item_count);
        self.workspace.update(cx, |workspace, _cx| {
            workspace.resume_live_diff_feed(&self.project_id);
        });
        cx.notify();
    }

    pub fn invalidate_theme_cache(&mut self, cx: &mut Context<Self>) {
        if let Some(entry_id) = self.detail_entry_id {
            let entry = self
                .workspace
                .read(cx)
                .live_diff_feed_state(&self.project_id)
                .and_then(|feed| {
                    feed.entries
                        .iter()
                        .find(|entry| entry.id == entry_id)
                        .cloned()
                });
            if let (Some(detail_view), Some(entry)) = (self.detail_view.as_ref(), entry) {
                detail_view.update(cx, |view, cx| {
                    view.update_entry(entry, cx);
                });
            }
        }
        cx.notify();
    }
}

impl Render for LiveDiffStreamView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_feed_list_state(cx);
        let palette = theme::active(cx);
        let (
            project_name,
            tracked_scope_count,
            latest_scope_error,
            unread_count,
            live_follow,
            item_count,
            detail_pane_open,
            selected_entry_id,
        ) = {
            let workspace = self.workspace.read(cx);
            let project_name = workspace
                .project(&self.project_id)
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "Missing Project".to_string());
            let (
                tracked_scope_count,
                latest_scope_error,
                unread_count,
                live_follow,
                item_count,
                detail_pane_open,
                selected_entry_id,
            ) = workspace
                .live_diff_feed_state(&self.project_id)
                .map(|feed| {
                    (
                        feed.tracked_scope_count(),
                        feed.latest_scope_error().map(str::to_string),
                        feed.unread_count,
                        feed.live_follow,
                        feed.entries.len(),
                        feed.detail_pane_open,
                        feed.selected_entry_id,
                    )
                })
                .unwrap_or((0, None, 0, true, 0, false, None));
            (
                project_name,
                tracked_scope_count,
                latest_scope_error,
                unread_count,
                live_follow,
                item_count,
                detail_pane_open,
                selected_entry_id,
            )
        };

        let mut header_actions = div().flex().items_center().gap(px(10.0));
        if !live_follow || unread_count > 0 {
            let label = if unread_count > 0 {
                format!("Back to Live ({unread_count})")
            } else {
                "Back to Live".to_string()
            };
            header_actions = header_actions.child(
                div()
                    .id("live-diff-back-to-live")
                    .px(px(10.0))
                    .py(px(6.0))
                    .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x20)))
                    .border_1()
                    .border_color(rgb(palette.ORCA_BLUE))
                    .rounded(px(999.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.PATCH))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.scroll_to_live(cx);
                    })),
            );
        }
        header_actions = header_actions.child(
            div()
                .px(px(8.0))
                .py(px(5.0))
                .bg(rgb(palette.DEEP))
                .border_1()
                .border_color(rgb(palette.SURFACE))
                .rounded(px(6.0))
                .text_size(px(11.0))
                .text_color(rgb(palette.FOG))
                .child(format!("{item_count} event(s)")),
        );

        // Close button when in detail mode.
        if detail_pane_open {
            header_actions = header_actions.child(
                div()
                    .id("live-diff-close-detail")
                    .px(px(10.0))
                    .py(px(5.0))
                    .bg(rgba(theme::with_alpha(palette.STATUS_CORAL, 0x18)))
                    .border_1()
                    .border_color(rgb(palette.STATUS_CORAL))
                    .rounded(px(6.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.PATCH))
                    .cursor_pointer()
                    .child("Close")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.detail_view = None;
                        this.detail_entry_id = None;
                        this.workspace.update(cx, |workspace, _cx| {
                            workspace.close_feed_detail_pane(&this.project_id);
                        });
                        cx.notify();
                    })),
            );
        }

        // ---------- Body: detail view or feed list ----------

        // Manage detail view entity lifecycle.
        if detail_pane_open {
            if let Some(entry_id) = selected_entry_id {
                // Create or re-create the detail view if the entry changed.
                if self.detail_entry_id != Some(entry_id) {
                    let entry = self
                        .workspace
                        .read(cx)
                        .live_diff_feed_state(&self.project_id)
                        .and_then(|feed| feed.entries.iter().find(|e| e.id == entry_id).cloned());
                    if let Some(entry) = entry {
                        self.detail_view = Some(cx.new(|cx| FeedDetailView::new(entry, cx)));
                        self.detail_entry_id = Some(entry_id);
                    } else {
                        // Entry not found — close detail pane.
                        self.detail_view = None;
                        self.detail_entry_id = None;
                        self.workspace.update(cx, |workspace, _cx| {
                            workspace.close_feed_detail_pane(&self.project_id);
                        });
                    }
                }
            }
        } else {
            // Clean up if detail pane is closed.
            if self.detail_view.is_some() {
                self.detail_view = None;
                self.detail_entry_id = None;
            }
        }

        let content_body = if detail_pane_open && self.detail_view.is_some() {
            // Render the detail view instead of the feed list.
            let detail_view = self.detail_view.as_ref().unwrap().clone();
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(detail_view)
                .into_any_element()
        } else {
            // Normal feed list rendering.
            let list_body = if item_count == 0 {
                let (title, body) = if tracked_scope_count == 0 {
                    (
                        "No git scopes are active",
                        "Open a git-backed terminal in this project to start the live diff feed.",
                    )
                } else {
                    (
                        "No feed events yet",
                        "Tracked scopes are armed. Events will appear here after diff indexes load or repo generations change.",
                    )
                };
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .child(
                        div()
                            .w_full()
                            .px(px(16.0))
                            .py(px(18.0))
                            .bg(rgb(palette.DEEP))
                            .border_1()
                            .border_color(rgb(palette.SURFACE))
                            .rounded(px(4.0))
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.PATCH))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(palette.BONE))
                                    .child(body),
                            ),
                    )
                    .into_any_element()
            } else {
                let workspace = self.workspace.clone();
                let project_id = self.project_id.clone();
                let palette = palette.clone();
                let feed_list = list(self.feed_list_state.clone(), move |ix, _window, cx| {
                    let Some(entry) = workspace
                        .read(cx)
                        .live_diff_feed_state(&project_id)
                        .and_then(|feed| feed.entries.get(ix))
                        .cloned()
                    else {
                        return div().w_full().into_any_element();
                    };

                    let mut entry_actions = div().flex().items_center().gap(px(8.0));

                    let show_view_details = matches!(
                        entry.capture_state,
                        FeedCaptureState::Ready(_) | FeedCaptureState::Truncated(_)
                    ) && entry.changed_file_count > 0;
                    if show_view_details {
                        let entry_id = entry.id;
                        let project_id = project_id.clone();
                        let workspace = workspace.clone();
                        entry_actions = entry_actions.child(
                            div()
                                .id(ElementId::Name(
                                    format!("live-diff-row-view-details-{}", entry.id).into(),
                                ))
                                .flex_shrink_0()
                                .px(px(8.0))
                                .py(px(5.0))
                                .bg(rgb(palette.ABYSS))
                                .border_1()
                                .border_color(rgb(palette.SURFACE))
                                .rounded(px(4.0))
                                .text_size(px(10.0))
                                .text_color(rgb(palette.BONE))
                                .cursor_pointer()
                                .child("View Details")
                                .on_click(move |_event, _window, cx| {
                                    workspace.update(cx, |workspace, _cx| {
                                        workspace.select_feed_entry(&project_id, entry_id);
                                    });
                                }),
                        );
                    }

                    let scope_root = entry.scope_root.clone();
                    let preferred_file = preferred_open_diff_selection(&entry);
                    let workspace_for_diff = workspace.clone();
                    entry_actions = entry_actions.child(
                        div()
                            .id(ElementId::Name(
                                format!("live-diff-row-open-diff-{}", entry.id).into(),
                            ))
                            .flex_shrink_0()
                            .px(px(8.0))
                            .py(px(5.0))
                            .bg(rgb(palette.ABYSS))
                            .border_1()
                            .border_color(rgb(palette.SURFACE))
                            .rounded(px(4.0))
                            .text_size(px(10.0))
                            .text_color(rgb(palette.BONE))
                            .cursor_pointer()
                            .child("Open Diff")
                            .on_click(move |_event, _window, cx| {
                                workspace_for_diff.update(cx, |workspace, _cx| {
                                    workspace.open_diff_tab_for_scope_and_file(
                                        &scope_root,
                                        preferred_file.clone(),
                                    );
                                });
                            }),
                    );

                    let open_terminal_available =
                        entry
                            .source_terminal_id
                            .as_deref()
                            .is_some_and(|terminal_id| {
                                workspace
                                    .read(cx)
                                    .live_diff_source_terminal_available(terminal_id)
                            });

                    if open_terminal_available {
                        let source_terminal_id = entry.source_terminal_id.clone().unwrap();
                        let workspace = workspace.clone();
                        entry_actions = entry_actions.child(
                            div()
                                .id(ElementId::Name(
                                    format!("live-diff-row-open-terminal-{}", entry.id).into(),
                                ))
                                .flex_shrink_0()
                                .px(px(8.0))
                                .py(px(5.0))
                                .bg(rgb(palette.ABYSS))
                                .border_1()
                                .border_color(rgb(palette.SURFACE))
                                .rounded(px(4.0))
                                .text_size(px(10.0))
                                .text_color(rgb(palette.BONE))
                                .cursor_pointer()
                                .child("Open Terminal")
                                .on_click(move |_event, _window, cx| {
                                    workspace.update(cx, |workspace, _cx| {
                                        workspace.focus_terminal_by_id(&source_terminal_id);
                                    });
                                }),
                        );
                    }

                    div()
                        .w_full()
                        .pt(px(if ix == 0 { 0.0 } else { FEED_ROW_GAP }))
                        .child(render_feed_row(
                            &entry,
                            &palette,
                            entry_actions.into_any_element(),
                        ))
                        .into_any_element()
                })
                .size_full();

                div()
                    .id("live-diff-feed-scroll")
                    .size_full()
                    .child(feed_list)
                    .into_any_element()
            };

            let mut body = div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .gap(px(10.0));
            if let Some(error) = latest_scope_error {
                body = body.child(
                    div()
                        .w_full()
                        .px(px(12.0))
                        .py(px(10.0))
                        .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x18)))
                        .border_1()
                        .border_color(rgb(palette.STATUS_AMBER))
                        .rounded(px(4.0))
                        .text_size(px(11.0))
                        .text_color(rgb(palette.STATUS_AMBER))
                        .child(error),
                );
            }
            body = body.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(list_body),
            );
            body.into_any_element()
        };

        div()
            .id(ElementId::Name(
                format!("live-diff-stream-{}", self.project_id).into(),
            ))
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(palette.ABYSS))
            .p(px(18.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .px(px(14.0))
                    .py(px(12.0))
                    .bg(rgb(palette.DEEP))
                    .border_1()
                    .border_color(rgb(palette.SURFACE))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(palette.FOG))
                                    .child("LIVE DIFF FEED"),
                            )
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.BONE))
                                    .child(project_name),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(5.0))
                                    .bg(rgba(theme::with_alpha(
                                        if live_follow {
                                            palette.STATUS_GREEN
                                        } else {
                                            palette.STATUS_AMBER
                                        },
                                        0x18,
                                    )))
                                    .border_1()
                                    .border_color(rgb(if live_follow {
                                        palette.STATUS_GREEN
                                    } else {
                                        palette.STATUS_AMBER
                                    }))
                                    .rounded(px(6.0))
                                    .text_size(px(11.0))
                                    .text_color(rgb(palette.PATCH))
                                    .child(if live_follow { "LIVE" } else { "PAUSED" }),
                            )
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(5.0))
                                    .bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x18)))
                                    .border_1()
                                    .border_color(rgb(palette.ORCA_BLUE))
                                    .rounded(px(6.0))
                                    .text_size(px(11.0))
                                    .text_color(rgb(palette.PATCH))
                                    .child(format!("{tracked_scope_count} tracked scope(s)")),
                            )
                            .child(header_actions),
                    ),
            )
            .child(content_body)
    }
}

fn render_feed_row(
    entry: &ChangeFeedEntry,
    palette: &theme::OrcaTheme,
    header_actions: AnyElement,
) -> Stateful<Div> {
    let scope_chip = match entry.scope_kind {
        FeedScopeKind::ManagedWorktree => "WT",
        FeedScopeKind::ProjectRoot => "BR",
    };
    let scope_label = entry
        .worktree_name
        .clone()
        .unwrap_or_else(|| entry.branch_name.clone());
    let age_label = feed_age_label(entry.observed_at);
    let summary_paths = summarize_paths(entry);
    let (event_label, event_border) = match entry.origin {
        FeedEntryOrigin::BootstrapSnapshot => ("CURRENT STATE", palette.ORCA_BLUE),
        FeedEntryOrigin::LiveDelta => ("LIVE DELTA", palette.STATUS_GREEN),
    };

    let mut card = div()
        .id(ElementId::Name(
            format!("live-diff-entry-{}", entry.id).into(),
        ))
        .w_full()
        .flex_shrink_0()
        .px(px(12.0))
        .py(px(12.0))
        .bg(rgb(palette.DEEP))
        .border_1()
        .border_color(rgb(palette.SURFACE))
        .rounded(px(4.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .px(px(6.0))
                                .py(px(3.0))
                                .bg(rgba(theme::with_alpha(
                                    if scope_chip == "WT" {
                                        palette.STATUS_GREEN
                                    } else {
                                        palette.ORCA_BLUE
                                    },
                                    0x18,
                                )))
                                .border_1()
                                .border_color(rgb(if scope_chip == "WT" {
                                    palette.STATUS_GREEN
                                } else {
                                    palette.ORCA_BLUE
                                }))
                                .rounded(px(4.0))
                                .text_size(px(10.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.PATCH))
                                .child(scope_chip),
                        )
                        .child(
                            div()
                                .px(px(6.0))
                                .py(px(3.0))
                                .bg(rgba(theme::with_alpha(event_border, 0x18)))
                                .border_1()
                                .border_color(rgb(event_border))
                                .rounded(px(4.0))
                                .text_size(px(10.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.PATCH))
                                .child(event_label),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.PATCH))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(scope_label),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(10.0))
                                .text_color(rgb(palette.FOG))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(entry.branch_name.clone()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(header_actions)
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(palette.FOG))
                                .child(age_label),
                        ),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .text_size(px(11.0))
                .text_color(rgb(palette.BONE))
                .child(format!("gen {}", entry.generation))
                .child(format!("{} files", entry.changed_file_count))
                .child(
                    div()
                        .text_color(rgb(palette.STATUS_GREEN))
                        .child(format!("+{}", entry.insertions)),
                )
                .child(
                    div()
                        .text_color(rgb(palette.STATUS_CORAL))
                        .child(format!("-{}", entry.deletions)),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .text_size(px(11.0))
                .text_color(rgb(palette.BONE))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(summary_paths),
        );

    match &entry.capture_state {
        FeedCaptureState::Pending => {
            card = card.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.FOG))
                    .child("Capturing bounded historical preview..."),
            );
        }
        FeedCaptureState::Failed { message } => {
            card = card.child(
                div()
                    .flex_shrink_0()
                    .px(px(10.0))
                    .py(px(8.0))
                    .bg(rgba(theme::with_alpha(palette.STATUS_AMBER, 0x12)))
                    .rounded(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(palette.STATUS_AMBER))
                    .child(message.clone()),
            );
        }
        FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) => {
            if entry.changed_file_count == 0 {
                card = card.child(
                    div()
                        .flex_shrink_0()
                        .px(px(10.0))
                        .py(px(8.0))
                        .bg(rgba(theme::with_alpha(palette.STATUS_GREEN, 0x12)))
                        .rounded(px(4.0))
                        .text_size(px(11.0))
                        .text_color(rgb(palette.STATUS_GREEN))
                        .child(match entry.origin {
                            FeedEntryOrigin::BootstrapSnapshot => "No dirty files captured",
                            FeedEntryOrigin::LiveDelta => "Returned to clean",
                        }),
                );
            } else {
                let preview = WorkspaceState::build_feed_preview_layout(captured);
                let mut preview_body = div()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .gap(px(8.0));

                for preview_file in &preview.files {
                    preview_body = preview_body.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex_shrink_0()
                            .px(px(10.0))
                            .py(px(8.0))
                            .bg(rgb(palette.ABYSS))
                            .border_1()
                            .border_color(rgb(palette.SURFACE))
                            .rounded(px(4.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.FOG))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(preview_file.relative_path.display().to_string()),
                            )
                            .children(preview_file.lines.iter().map(|line| {
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .h(px(FEED_CARD_PREVIEW_LINE_HEIGHT))
                                    .text_size(px(11.0))
                                    .font_family("Monaco")
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_color(rgb(match line.kind {
                                        DiffLineKind::Addition => palette.STATUS_GREEN,
                                        DiffLineKind::Deletion => palette.STATUS_CORAL,
                                        DiffLineKind::HunkHeader | DiffLineKind::FileHeader => {
                                            palette.ORCA_BLUE
                                        }
                                        DiffLineKind::BinaryNotice => palette.STATUS_AMBER,
                                        DiffLineKind::Context => palette.BONE,
                                    }))
                                    .child(format!("{} {}", diff_line_prefix(line.kind), line.text))
                            })),
                    );
                }
                card = card.child(preview_body);

                if preview.hidden_file_count > 0 {
                    let hidden_names = preview
                        .hidden_file_names
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let suffix = if hidden_names.is_empty() {
                        String::new()
                    } else {
                        format!(" ({hidden_names})")
                    };
                    card = card.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.0))
                            .text_color(rgb(palette.FOG))
                            .child(format!(
                                "+{} more file(s){suffix}",
                                preview.hidden_file_count
                            )),
                    );
                }

                if matches!(entry.capture_state, FeedCaptureState::Truncated(_)) {
                    card = card.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.0))
                            .text_color(rgb(palette.STATUS_AMBER))
                            .child("Preview truncated at the event capture limit."),
                    );
                }
            }
        }
    }

    card
}

fn summarize_paths(entry: &ChangeFeedEntry) -> String {
    let shown = entry
        .files
        .iter()
        .take(2)
        .map(|file| file.relative_path.display().to_string())
        .collect::<Vec<_>>();
    let hidden = entry.changed_file_count.saturating_sub(shown.len());
    let mut parts = shown;
    if hidden > 0 {
        parts.push(format!("+{hidden} more"));
    }
    if parts.is_empty() {
        "No changed files".to_string()
    } else {
        parts.join("  ")
    }
}

fn diff_line_prefix(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Addition => "+",
        DiffLineKind::Deletion => "-",
        DiffLineKind::HunkHeader => "@@",
        DiffLineKind::FileHeader => "diff",
        DiffLineKind::BinaryNotice => "bin",
        DiffLineKind::Context => " ",
    }
}

fn preferred_open_diff_selection(entry: &ChangeFeedEntry) -> Option<DiffSelectionKey> {
    match &entry.capture_state {
        FeedCaptureState::Ready(captured) | FeedCaptureState::Truncated(captured) => captured
            .files
            .first()
            .map(|file| file.selection.clone())
            .or_else(|| {
                captured
                    .failed_files
                    .first()
                    .map(|file| file.selection.clone())
            }),
        FeedCaptureState::Pending | FeedCaptureState::Failed { .. } => None,
    }
}

fn feed_age_label(observed_at: SystemTime) -> String {
    let age = SystemTime::now()
        .duration_since(observed_at)
        .unwrap_or(Duration::from_secs(0));
    if age < Duration::from_secs(60) {
        format!("{}s ago", age.as_secs())
    } else if age < Duration::from_secs(60 * 60) {
        format!("{}m ago", age.as_secs() / 60)
    } else {
        format!("{}h ago", age.as_secs() / (60 * 60))
    }
}
