use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::settings::{AppSettings, ThemeId, ThemeMode};
use crate::theme::{self, OrcaTheme};
use crate::workspace::WorkspaceState;
use orcashell_store::CursorStyle;

/// Which text field is currently being edited inline.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EditingField {
    FontFamily,
    DefaultShell,
    NotificationUrgentPatterns,
}

pub struct SettingsView {
    workspace: Entity<WorkspaceState>,
    focus_handle: FocusHandle,
    editing: Option<EditingField>,
    edit_buffer: String,
}

impl SettingsView {
    pub fn new(workspace: Entity<WorkspaceState>, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            editing: None,
            edit_buffer: String::new(),
        }
    }

    /// Clear any in-progress text edit. Called when settings tab is closed.
    pub fn reset_edit_state(&mut self) {
        self.editing = None;
        self.edit_buffer.clear();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn start_edit(&mut self, field: EditingField, current_value: &str, cx: &mut Context<Self>) {
        self.editing = Some(field);
        self.edit_buffer = current_value.to_string();
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(field) = self.editing.take() {
            let value = self.edit_buffer.clone();
            let mut settings = cx.global::<AppSettings>().clone();
            match field {
                EditingField::FontFamily => settings.font_family = value,
                EditingField::DefaultShell => {
                    settings.default_shell = if value.is_empty() { None } else { Some(value) };
                }
                EditingField::NotificationUrgentPatterns => {
                    settings.notification_urgent_patterns = value
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .collect();
                }
            }
            cx.set_global(settings);
            cx.notify();
        }
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.edit_buffer.clear();
        cx.notify();
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke.key;

        if self.editing.is_some() {
            match key.as_str() {
                "backspace" => {
                    self.edit_buffer.pop();
                    cx.notify();
                }
                "enter" => self.commit_edit(cx),
                "escape" => self.cancel_edit(cx),
                "space" => {
                    self.edit_buffer.push(' ');
                    cx.notify();
                }
                _ => {
                    let mods = &event.keystroke.modifiers;
                    // Use key_char for composed character, filtering out modified keys
                    if let Some(ref kc) = event.keystroke.key_char {
                        if !kc.is_empty() && !mods.platform && !mods.control {
                            self.edit_buffer.push_str(kc);
                            cx.notify();
                        }
                    } else if key.len() == 1 && !mods.control && !mods.alt && !mods.platform {
                        self.edit_buffer.push_str(key);
                        cx.notify();
                    }
                }
            }
        } else if key == "escape" {
            // Close settings tab when Escape is pressed and not editing
            self.workspace.update(cx, |ws, cx| ws.close_settings(cx));
        }
    }

    // ── Layout helpers ──────────────────────────────────────────────────

    fn section_header(palette: &OrcaTheme, title: &str) -> Div {
        div()
            .w_full()
            .pb(px(8.0))
            .mb(px(12.0))
            .border_b_1()
            .border_color(rgb(palette.SURFACE))
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgb(palette.PATCH))
                    .child(title.to_string()),
            )
    }

    fn setting_row() -> Div {
        div()
            .w_full()
            .mb(px(10.0))
            .flex()
            .items_center()
            .gap(px(16.0))
    }

    fn label(palette: &OrcaTheme, text: &str) -> Div {
        div()
            .w(px(140.0))
            .flex_shrink_0()
            .text_size(px(13.0))
            .text_color(rgb(palette.BONE))
            .child(text.to_string())
    }

    fn stepper_button(
        palette: &OrcaTheme,
        id: impl Into<SharedString>,
        label: &str,
    ) -> Stateful<Div> {
        let hover_bg = palette.CURRENT;
        div()
            .id(ElementId::Name(id.into()))
            .w(px(28.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .bg(rgb(palette.SURFACE))
            .text_size(px(14.0))
            .text_color(rgb(palette.BONE))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(hover_bg)))
            .child(label.to_string())
    }

    fn value_display(palette: &OrcaTheme, value: String) -> Div {
        div()
            .w(px(80.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .bg(rgb(palette.DEEP))
            .border_1()
            .border_color(rgb(palette.SURFACE))
            .text_size(px(13.0))
            .text_color(rgb(palette.BONE))
            .child(value)
    }

    fn radio_option(
        palette: &OrcaTheme,
        id: impl Into<SharedString>,
        text: &str,
        selected: bool,
    ) -> Stateful<Div> {
        div()
            .id(ElementId::Name(id.into()))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .child(
                div()
                    .w(px(14.0))
                    .h(px(14.0))
                    .rounded(px(7.0))
                    .border_1()
                    .when(selected, |s| {
                        s.border_color(rgb(palette.FOG)).bg(rgb(palette.FOG))
                    })
                    .when(!selected, |s| {
                        s.border_color(rgb(palette.SLATE)).bg(rgb(palette.DEEP))
                    }),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(if selected {
                        rgb(palette.BONE)
                    } else {
                        rgb(palette.FOG)
                    })
                    .child(text.to_string()),
            )
    }

    fn text_field_display(
        palette: &OrcaTheme,
        id: impl Into<SharedString>,
        current_value: &str,
        placeholder: &str,
        is_editing: bool,
        edit_buffer: &str,
    ) -> Stateful<Div> {
        let display = if is_editing {
            if edit_buffer.is_empty() {
                "\u{258F}".to_string()
            } else {
                format!("{}\u{258F}", edit_buffer)
            }
        } else if current_value.is_empty() {
            placeholder.to_string()
        } else {
            current_value.to_string()
        };

        let text_color = if is_editing {
            rgb(palette.BONE)
        } else if current_value.is_empty() {
            rgb(palette.SLATE)
        } else {
            rgb(palette.BONE)
        };

        div()
            .id(ElementId::Name(id.into()))
            .min_w(px(200.0))
            .h(px(28.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(4.0))
            .bg(rgb(palette.DEEP))
            .border_1()
            .when(is_editing, |s| s.border_color(rgb(palette.ORCA_BLUE)))
            .when(!is_editing, |s| s.border_color(rgb(palette.SURFACE)))
            .cursor_pointer()
            .text_size(px(13.0))
            .text_color(text_color)
            .child(display)
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<AppSettings>().clone();
        let palette = theme::active(cx);
        let editing = self.editing;
        let edit_buffer = self.edit_buffer.clone();

        let root = div()
            .id("settings-view-root")
            .size_full()
            .bg(rgb(palette.ABYSS))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_key_down(event, cx);
            }));

        // ── Font Size ──
        let font_size = settings.font_size;
        let font_size_row = Self::setting_row()
            .child(Self::label(&palette, "Font Size"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        Self::stepper_button(&palette, "font-size-dec", "\u{2212}").on_click(
                            cx.listener(move |_this, _, _, cx| {
                                let mut s = cx.global::<AppSettings>().clone();
                                s.font_size = (s.font_size - 1.0).max(8.0);
                                cx.set_global(s);
                            }),
                        ),
                    )
                    .child(Self::value_display(&palette, format!("{:.1}", font_size)))
                    .child(
                        Self::stepper_button(&palette, "font-size-inc", "+").on_click(cx.listener(
                            move |_this, _, _, cx| {
                                let mut s = cx.global::<AppSettings>().clone();
                                s.font_size = (s.font_size + 1.0).min(32.0);
                                cx.set_global(s);
                            },
                        )),
                    ),
            );

        // ── Font Family ──
        let is_editing_font = editing == Some(EditingField::FontFamily);
        let font_family_display = if is_editing_font {
            edit_buffer.clone()
        } else {
            settings.font_family.clone()
        };
        let font_family_row = Self::setting_row()
            .child(Self::label(&palette, "Font Family"))
            .child(
                Self::text_field_display(
                    &palette,
                    "font-family-input",
                    &settings.font_family,
                    "JetBrains Mono",
                    is_editing_font,
                    &font_family_display,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let current = cx.global::<AppSettings>().font_family.clone();
                    this.start_edit(EditingField::FontFamily, &current, cx);
                })),
            );

        // ── Scrollback ──
        let scrollback = settings.scrollback_lines;
        let scrollback_row = Self::setting_row()
            .child(Self::label(&palette, "Scrollback"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        Self::stepper_button(&palette, "scrollback-dec", "\u{2212}").on_click(
                            cx.listener(move |_this, _, _, cx| {
                                let mut s = cx.global::<AppSettings>().clone();
                                s.scrollback_lines =
                                    s.scrollback_lines.saturating_sub(1000).max(100);
                                cx.set_global(s);
                            }),
                        ),
                    )
                    .child(Self::value_display(&palette, format!("{}", scrollback)))
                    .child(
                        Self::stepper_button(&palette, "scrollback-inc", "+").on_click(
                            cx.listener(move |_this, _, _, cx| {
                                let mut s = cx.global::<AppSettings>().clone();
                                s.scrollback_lines = (s.scrollback_lines + 1000).min(100_000);
                                cx.set_global(s);
                            }),
                        ),
                    ),
            );

        // ── Cursor Style ──
        let cursor_style = settings.cursor_style;
        let cursor_style_row = Self::setting_row()
            .child(Self::label(&palette, "Style"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        Self::radio_option(
                            &palette,
                            "cursor-block",
                            "Block",
                            cursor_style == CursorStyle::Block,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.cursor_style = CursorStyle::Block;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "cursor-bar",
                            "Bar",
                            cursor_style == CursorStyle::Bar,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.cursor_style = CursorStyle::Bar;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "cursor-underline",
                            "Underline",
                            cursor_style == CursorStyle::Underline,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.cursor_style = CursorStyle::Underline;
                            cx.set_global(s);
                        })),
                    ),
            );

        // ── Cursor Blink ──
        let cursor_blink = settings.cursor_blink;
        let cursor_blink_row = Self::setting_row()
            .child(Self::label(&palette, "Blink"))
            .child(
                div()
                    .id("cursor-blink-toggle")
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .border_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(cursor_blink, |s| {
                        s.border_color(rgb(palette.FOG)).bg(rgb(palette.FOG))
                    })
                    .when(!cursor_blink, |s| {
                        s.border_color(rgb(palette.SLATE)).bg(rgb(palette.DEEP))
                    })
                    .when(cursor_blink, |s| {
                        s.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.DEEP))
                                .child("\u{2713}"),
                        )
                    })
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let mut s = cx.global::<AppSettings>().clone();
                        s.cursor_blink = !s.cursor_blink;
                        cx.set_global(s);
                    })),
            );

        // ── Activity Pulse ──
        let activity_pulse = settings.activity_pulse;
        let activity_pulse_row = Self::setting_row()
            .child(Self::label(&palette, "Activity Pulse"))
            .child(
                div()
                    .id("activity-pulse-toggle")
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .border_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(activity_pulse, |s| {
                        s.border_color(rgb(palette.FOG)).bg(rgb(palette.FOG))
                    })
                    .when(!activity_pulse, |s| {
                        s.border_color(rgb(palette.SLATE)).bg(rgb(palette.DEEP))
                    })
                    .when(activity_pulse, |s| {
                        s.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.DEEP))
                                .child("\u{2713}"),
                        )
                    })
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let mut s = cx.global::<AppSettings>().clone();
                        s.activity_pulse = !s.activity_pulse;
                        cx.set_global(s);
                    })),
            );

        // ── Agent Notifications ──
        let agent_notifications = settings.agent_notifications;
        let agent_notifications_row = Self::setting_row()
            .child(Self::label(&palette, "Agent Notifications"))
            .child(
                div()
                    .id("agent-notifications-toggle")
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .border_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(agent_notifications, |s| {
                        s.border_color(rgb(palette.FOG)).bg(rgb(palette.FOG))
                    })
                    .when(!agent_notifications, |s| {
                        s.border_color(rgb(palette.SLATE)).bg(rgb(palette.DEEP))
                    })
                    .when(agent_notifications, |s| {
                        s.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.DEEP))
                                .child("\u{2713}"),
                        )
                    })
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let mut s = cx.global::<AppSettings>().clone();
                        s.agent_notifications = !s.agent_notifications;
                        cx.set_global(s);
                    })),
            );

        // ── Urgent Patterns ──
        let is_editing_patterns = editing == Some(EditingField::NotificationUrgentPatterns);
        let patterns_value = settings.notification_urgent_patterns.join(", ");
        let patterns_display = if is_editing_patterns {
            edit_buffer.clone()
        } else {
            patterns_value.clone()
        };
        let urgent_patterns_row = Self::setting_row()
            .child(Self::label(&palette, "Urgent Patterns"))
            .child(
                Self::text_field_display(
                    &palette,
                    "urgent-patterns-input",
                    &patterns_value,
                    "approv, permission, edit",
                    is_editing_patterns,
                    &patterns_display,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let current = cx
                        .global::<AppSettings>()
                        .notification_urgent_patterns
                        .join(", ");
                    this.start_edit(EditingField::NotificationUrgentPatterns, &current, cx);
                })),
            );

        // ── Resume Agent Sessions ──
        let resume_agent_sessions = settings.resume_agent_sessions;
        let resume_agent_sessions_row = Self::setting_row()
            .child(Self::label(&palette, "Resume Sessions"))
            .child(
                div()
                    .id("resume-agent-sessions-toggle")
                    .w(px(18.0))
                    .h(px(18.0))
                    .rounded(px(4.0))
                    .border_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(resume_agent_sessions, |s| {
                        s.border_color(rgb(palette.FOG)).bg(rgb(palette.FOG))
                    })
                    .when(!resume_agent_sessions, |s| {
                        s.border_color(rgb(palette.SLATE)).bg(rgb(palette.DEEP))
                    })
                    .when(resume_agent_sessions, |s| {
                        s.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.DEEP))
                                .child("\u{2713}"),
                        )
                    })
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let mut s = cx.global::<AppSettings>().clone();
                        s.resume_agent_sessions = !s.resume_agent_sessions;
                        cx.set_global(s);
                    })),
            );

        // ── Default Shell ──
        let is_editing_shell = editing == Some(EditingField::DefaultShell);
        let shell_value = settings.default_shell.as_deref().unwrap_or("").to_string();
        let shell_display = if is_editing_shell {
            edit_buffer.clone()
        } else {
            shell_value.clone()
        };
        let shell_row = Self::setting_row()
            .child(Self::label(&palette, "Default Shell"))
            .child(
                Self::text_field_display(
                    &palette,
                    "shell-input",
                    &shell_value,
                    if cfg!(windows) {
                        "System default (pwsh / powershell / cmd)"
                    } else {
                        "System default ($SHELL)"
                    },
                    is_editing_shell,
                    &shell_display,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let current = cx
                        .global::<AppSettings>()
                        .default_shell
                        .clone()
                        .unwrap_or_default();
                    this.start_edit(EditingField::DefaultShell, &current, cx);
                })),
            );

        let theme_row = Self::setting_row()
            .child(Self::label(&palette, "Theme"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        Self::radio_option(
                            &palette,
                            "theme-system",
                            "System",
                            settings.theme_mode == ThemeMode::System,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.theme_mode = ThemeMode::System;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "theme-dark",
                            "Dark",
                            settings.theme_mode == ThemeMode::Manual
                                && settings.manual_theme == ThemeId::Dark,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.theme_mode = ThemeMode::Manual;
                            s.manual_theme = ThemeId::Dark;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "theme-black",
                            "Black",
                            settings.theme_mode == ThemeMode::Manual
                                && settings.manual_theme == ThemeId::Black,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.theme_mode = ThemeMode::Manual;
                            s.manual_theme = ThemeId::Black;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "theme-light",
                            "Light",
                            settings.theme_mode == ThemeMode::Manual
                                && settings.manual_theme == ThemeId::Light,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.theme_mode = ThemeMode::Manual;
                            s.manual_theme = ThemeId::Light;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "theme-sepia",
                            "Sepia",
                            settings.theme_mode == ThemeMode::Manual
                                && settings.manual_theme == ThemeId::Sepia,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.theme_mode = ThemeMode::Manual;
                            s.manual_theme = ThemeId::Sepia;
                            cx.set_global(s);
                        })),
                    ),
            );

        let system_light_row = Self::setting_row()
            .child(Self::label(&palette, "Light Mode"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        Self::radio_option(
                            &palette,
                            "system-light-light",
                            "Light",
                            settings.system_light_theme == ThemeId::Light,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.system_light_theme = ThemeId::Light;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "system-light-sepia",
                            "Sepia",
                            settings.system_light_theme == ThemeId::Sepia,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.system_light_theme = ThemeId::Sepia;
                            cx.set_global(s);
                        })),
                    ),
            );

        let system_dark_row = Self::setting_row()
            .child(Self::label(&palette, "Dark Mode"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        Self::radio_option(
                            &palette,
                            "system-dark-dark",
                            "Dark",
                            settings.system_dark_theme == ThemeId::Dark,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.system_dark_theme = ThemeId::Dark;
                            cx.set_global(s);
                        })),
                    )
                    .child(
                        Self::radio_option(
                            &palette,
                            "system-dark-black",
                            "Black",
                            settings.system_dark_theme == ThemeId::Black,
                        )
                        .on_click(cx.listener(|_this, _, _, cx| {
                            let mut s = cx.global::<AppSettings>().clone();
                            s.system_dark_theme = ThemeId::Black;
                            cx.set_global(s);
                        })),
                    ),
            );

        // ── Assemble ──
        root.child(
            div()
                .w_full()
                .max_w(px(600.0))
                .mx_auto()
                .p(px(32.0))
                .flex()
                .flex_col()
                // Title
                .child(
                    div()
                        .mb(px(24.0))
                        .text_size(px(16.0))
                        .text_color(rgb(palette.PATCH))
                        .child("Settings"),
                )
                // Terminal
                .child(Self::section_header(&palette, "Terminal"))
                .child(font_size_row)
                .child(font_family_row)
                .child(scrollback_row)
                // Cursor
                .child(div().h(px(12.0)))
                .child(Self::section_header(&palette, "Cursor"))
                .child(cursor_style_row)
                .child(cursor_blink_row)
                // Attention
                .child(div().h(px(12.0)))
                .child(Self::section_header(&palette, "Attention"))
                .child(activity_pulse_row)
                .child(agent_notifications_row)
                .child(urgent_patterns_row)
                // Agents
                .child(div().h(px(12.0)))
                .child(Self::section_header(&palette, "Agents"))
                .child(resume_agent_sessions_row)
                // Shell
                .child(div().h(px(12.0)))
                .child(Self::section_header(&palette, "Shell"))
                .child(shell_row)
                // Appearance
                .child(div().h(px(12.0)))
                .child(Self::section_header(&palette, "Appearance"))
                .child(theme_row)
                .when(settings.theme_mode == ThemeMode::System, |this| {
                    this.child(system_light_row)
                        .child(system_dark_row)
                })
                // Footer
                .child(
                    div()
                        .mt(px(32.0))
                        .text_size(px(11.0))
                        .text_color(rgb(palette.SLATE))
                        .child(
                            "Settings are saved automatically. You can also edit settings.json directly.",
                        ),
                ),
        )
    }
}
