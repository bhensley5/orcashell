use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::*;
use orcashell_terminal_view::{Copy, Paste, TextInputState};

use crate::theme::{self, OrcaTheme};
use crate::workspace::WorkspaceState;

pub type PromptDialogRequest = Rc<RefCell<Option<PromptDialogSpec>>>;
pub type PromptDialogAction =
    Box<dyn Fn(&mut WorkspaceState, &mut Context<WorkspaceState>, PromptDialogResult) + 'static>;
pub type PromptDialogValidate = Box<dyn Fn(&str) -> Option<String> + 'static>;

pub enum PromptDialogEvent {
    Dismiss,
}

impl EventEmitter<PromptDialogEvent> for PromptDialogOverlay {}

pub enum PromptDialogConfirmTone {
    Primary,
    Destructive,
}

pub struct PromptDialogInputSpec {
    pub placeholder: String,
    pub initial_value: String,
    pub allow_empty: bool,
    pub validate: Option<PromptDialogValidate>,
}

pub struct PromptDialogSelectionOption {
    pub label: String,
    pub value: String,
}

pub struct PromptDialogSelectionSpec {
    pub options: Vec<PromptDialogSelectionOption>,
    pub initial_selected: usize,
}

pub struct PromptDialogToggleSpec {
    pub id: String,
    pub label: String,
    pub initial_value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDialogResult {
    pub input: Option<String>,
    pub selection: Option<String>,
    pub toggles: HashMap<String, bool>,
}

pub struct PromptDialogSpec {
    pub title: String,
    pub detail: Option<String>,
    pub confirm_label: String,
    pub confirm_tone: PromptDialogConfirmTone,
    pub input: Option<PromptDialogInputSpec>,
    pub selection: Option<PromptDialogSelectionSpec>,
    pub toggles: Vec<PromptDialogToggleSpec>,
    pub on_confirm: PromptDialogAction,
}

pub struct PromptDialogOverlay {
    spec: PromptDialogSpec,
    workspace: Entity<WorkspaceState>,
    input: Option<Entity<TextInputState>>,
    selected_option: usize,
    toggle_values: HashMap<String, bool>,
    focus_handle: FocusHandle,
    input_focused_once: bool,
}

impl PromptDialogOverlay {
    pub fn new(
        spec: PromptDialogSpec,
        workspace: Entity<WorkspaceState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let input = spec.input.as_ref().map(|input_spec| {
            let palette = theme::active(cx);
            let text_color: Hsla = rgb(palette.BONE).into();
            let placeholder_color: Hsla = rgb(palette.SLATE).into();
            let cursor_color: Hsla = rgb(palette.ORCA_BLUE).into();
            let selection_bg: Hsla = rgba(theme::with_alpha(palette.ORCA_BLUE, 0x40)).into();
            let placeholder = input_spec.placeholder.clone();
            let initial_value = input_spec.initial_value.clone();
            let input = cx.new(|cx| {
                let mut state = TextInputState::new(
                    text_color,
                    placeholder_color,
                    cursor_color,
                    selection_bg,
                    cx,
                );
                state.set_placeholder(&placeholder);
                state.set_value(&initial_value);
                state
            });
            cx.observe(&input, |_this, _input, cx| cx.notify()).detach();
            input
        });

        Self {
            selected_option: spec
                .selection
                .as_ref()
                .map(|selection| {
                    selection
                        .initial_selected
                        .min(selection.options.len().saturating_sub(1))
                })
                .unwrap_or(0),
            toggle_values: spec
                .toggles
                .iter()
                .map(|toggle| (toggle.id.clone(), toggle.initial_value))
                .collect(),
            spec,
            workspace,
            input,
            focus_handle,
            input_focused_once: false,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(PromptDialogEvent::Dismiss);
    }

    fn current_input_value(&self, cx: &App) -> Option<String> {
        self.input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
    }

    fn current_selection_value(&self) -> Option<String> {
        self.spec
            .selection
            .as_ref()
            .and_then(|selection| selection.options.get(self.selected_option))
            .map(|option| option.value.clone())
    }

    fn current_result(&self, cx: &App) -> PromptDialogResult {
        PromptDialogResult {
            input: self.current_input_value(cx),
            selection: self.current_selection_value(),
            toggles: self.toggle_values.clone(),
        }
    }

    fn validation_message(&self, cx: &App) -> Option<String> {
        let value = self.current_input_value(cx)?;
        if value.is_empty()
            && self
                .spec
                .input
                .as_ref()
                .is_none_or(|input| input.allow_empty)
        {
            return None;
        }
        self.spec
            .input
            .as_ref()
            .and_then(|input| input.validate.as_ref())
            .and_then(|validate| validate(&value))
    }

    fn confirm_disabled(&self, cx: &App) -> bool {
        if let Some(value) = self.current_input_value(cx) {
            let allow_empty = self
                .spec
                .input
                .as_ref()
                .is_some_and(|input| input.allow_empty);
            return (!allow_empty && value.is_empty()) || self.validation_message(cx).is_some();
        }
        if let Some(selection) = self.spec.selection.as_ref() {
            return selection.options.is_empty();
        }
        false
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(selection) = self.spec.selection.as_ref() else {
            return;
        };
        if selection.options.is_empty() {
            return;
        }
        let len = selection.options.len();
        let next = if delta.is_negative() {
            self.selected_option.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected_option + delta as usize).min(len.saturating_sub(1))
        };
        if next != self.selected_option {
            self.selected_option = next;
            cx.notify();
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.confirm_disabled(cx) {
            return;
        }
        let value = self.current_result(cx);
        let action = &self.spec.on_confirm;
        let workspace = self.workspace.clone();
        workspace.update(cx, |ws, ws_cx| {
            action(ws, ws_cx, value);
        });
        self.dismiss(cx);
    }
}

impl Render for PromptDialogOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::active(cx);
        if let Some(input) = self.input.as_ref() {
            if !self.input_focused_once {
                input.read(cx).focus(window);
                self.input_focused_once = true;
            }
        } else if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle);
        }

        let validation_message = self.validation_message(cx);
        let confirm_disabled = self.confirm_disabled(cx);
        let confirm_button = prompt_dialog_button(
            &palette,
            "prompt-dialog-confirm",
            &self.spec.confirm_label,
            confirm_disabled,
            match self.spec.confirm_tone {
                PromptDialogConfirmTone::Primary => palette.ORCA_BLUE,
                PromptDialogConfirmTone::Destructive => palette.STATUS_CORAL,
            },
            match self.spec.confirm_tone {
                PromptDialogConfirmTone::Primary => theme::with_alpha(palette.ORCA_BLUE, 0x18),
                PromptDialogConfirmTone::Destructive => {
                    theme::with_alpha(palette.STATUS_CORAL, 0x18)
                }
            },
            cx.listener(|this, _event: &ClickEvent, _window, cx| {
                this.submit(cx);
            }),
        );

        let mut panel = div()
            .track_focus(&self.focus_handle)
            .w(px(460.0))
            .max_w(px(460.0))
            .bg(rgb(palette.DEEP))
            .border_1()
            .border_color(rgb(palette.BORDER_EMPHASIS))
            .rounded(px(4.0))
            .shadow_xl()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.dismiss(cx);
                        cx.stop_propagation();
                    }
                    "up" => {
                        this.move_selection(-1, cx);
                        cx.stop_propagation();
                    }
                    "down" => {
                        this.move_selection(1, cx);
                        cx.stop_propagation();
                    }
                    "enter" => {
                        this.submit(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.BONE))
                    .child(self.spec.title.clone()),
            );

        if let Some(detail) = self.spec.detail.as_ref() {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .font_family(crate::diff_explorer::DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.FOG))
                    .child(detail.clone()),
            );
        }

        if let Some(input) = self.input.as_ref() {
            let input_focus = input.clone();
            let input_paste = input.clone();
            let input_copy = input.clone();
            let input_keys = input.clone();
            panel = panel.child(
                div()
                    .id("prompt-dialog-input-wrapper")
                    .key_context("PromptDialogInput")
                    .w_full()
                    .h(px(38.0))
                    .bg(rgb(palette.CURRENT))
                    .border_1()
                    .border_color(rgb(palette.BORDER_EMPHASIS))
                    .rounded(px(4.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .text_size(px(12.0))
                    .font_family(crate::diff_explorer::DIFF_FONT_FAMILY)
                    .child(input.clone())
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        input_focus.read(cx).focus(window);
                        cx.stop_propagation();
                    })
                    .on_action(cx.listener(move |_this, _: &Paste, _window, cx| {
                        if let Some(item) = cx.read_from_clipboard() {
                            if let Some(text) = item.text() {
                                let line = text.lines().next().unwrap_or("");
                                if !line.is_empty() {
                                    input_paste.update(cx, |input, cx| {
                                        input.insert_text(line);
                                        cx.notify();
                                    });
                                }
                            }
                        }
                    }))
                    .on_action(cx.listener(move |_this, _: &Copy, _window, cx| {
                        input_copy.update(cx, |input, cx| {
                            input.copy_selection(cx);
                        });
                    }))
                    .on_key_down(
                        cx.listener(move |_this, event: &KeyDownEvent, _window, cx| {
                            let handled =
                                input_keys.update(cx, |input, cx| input.handle_key_down(event, cx));
                            if handled {
                                cx.stop_propagation();
                            }
                        }),
                    ),
            );
        }

        if !self.spec.toggles.is_empty() {
            panel = panel.child(div().w_full().flex().flex_col().gap(px(6.0)).children(
                self.spec.toggles.iter().map(|toggle| {
                    let value = *self
                        .toggle_values
                        .get(&toggle.id)
                        .unwrap_or(&toggle.initial_value);
                    let toggle_id = toggle.id.clone();
                    let label = toggle.label.clone();
                    let mut row = div()
                        .id(ElementId::Name(
                            format!("prompt-dialog-toggle-{}", toggle.id).into(),
                        ))
                        .w_full()
                        .px(px(10.0))
                        .py(px(8.0))
                        .border_1()
                        .border_color(rgb(palette.BORDER_EMPHASIS))
                        .rounded(px(4.0))
                        .bg(rgb(palette.CURRENT))
                        .text_size(px(12.0))
                        .font_family(crate::diff_explorer::DIFF_FONT_FAMILY)
                        .text_color(rgb(palette.BONE))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgba(theme::with_alpha(palette.SURFACE, 0x60))))
                        .flex()
                        .items_center()
                        .gap(px(8.0));
                    row = row.child(
                        div()
                            .text_color(rgb(if value {
                                palette.ORCA_BLUE
                            } else {
                                palette.FOG
                            }))
                            .child(if value { "\u{2611}" } else { "\u{2610}" }),
                    );
                    row.child(label).on_click(cx.listener(
                        move |this, _event: &ClickEvent, _window, cx| {
                            let current =
                                this.toggle_values.get(&toggle_id).copied().unwrap_or(false);
                            this.toggle_values.insert(toggle_id.clone(), !current);
                            cx.notify();
                        },
                    ))
                }),
            ));
        }

        if let Some(selection) = self.spec.selection.as_ref() {
            panel = panel.child(div().w_full().flex().flex_col().gap(px(6.0)).children(
                selection.options.iter().enumerate().map(|(index, option)| {
                    let selected = index == self.selected_option;
                    let mut row = div()
                        .id(ElementId::Name(
                            format!("prompt-dialog-option-{index}").into(),
                        ))
                        .w_full()
                        .px(px(10.0))
                        .py(px(8.0))
                        .border_1()
                        .rounded(px(4.0))
                        .text_size(px(12.0))
                        .font_family(crate::diff_explorer::DIFF_FONT_FAMILY)
                        .cursor_pointer();

                    row = if selected {
                        row.bg(rgba(theme::with_alpha(palette.ORCA_BLUE, 0x18)))
                            .border_color(rgb(palette.ORCA_BLUE))
                            .text_color(rgb(palette.BONE))
                    } else {
                        row.bg(rgb(palette.CURRENT))
                            .border_color(rgb(palette.BORDER_EMPHASIS))
                            .text_color(rgb(palette.FOG))
                            .hover(|style| {
                                style
                                    .bg(rgba(theme::with_alpha(palette.SURFACE, 0x60)))
                                    .text_color(rgb(palette.BONE))
                            })
                    };

                    row.child(option.label.clone()).on_click(cx.listener(
                        move |this, _event: &ClickEvent, _window, cx| {
                            if this.selected_option != index {
                                this.selected_option = index;
                                cx.notify();
                            }
                        },
                    ))
                }),
            ));
        }

        if let Some(message) = validation_message {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .font_family(crate::diff_explorer::DIFF_FONT_FAMILY)
                    .text_color(rgb(palette.STATUS_CORAL))
                    .child(message),
            );
        }

        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .child(prompt_dialog_button(
                    &palette,
                    "prompt-dialog-cancel",
                    "Cancel",
                    false,
                    palette.BORDER_EMPHASIS,
                    theme::with_alpha(palette.CURRENT, 0x70),
                    cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.dismiss(cx);
                    }),
                ))
                .child(confirm_button),
        );

        div()
            .track_focus(&self.focus_handle)
            .absolute()
            .inset_0()
            .size_full()
            .bg(rgba(theme::with_alpha(palette.ABYSS, 0xD0)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.dismiss(cx);
                }),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(panel),
            )
    }
}

fn prompt_dialog_button(
    palette: &OrcaTheme,
    id: &str,
    label: &str,
    disabled: bool,
    border_color: u32,
    bg_color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let mut button = div()
        .id(ElementId::Name(id.to_string().into()))
        .px(px(10.0))
        .py(px(7.0))
        .border_1()
        .rounded(px(4.0))
        .text_size(px(11.0))
        .font_family(crate::diff_explorer::DIFF_FONT_FAMILY);

    if disabled {
        button = button
            .bg(rgb(palette.CURRENT))
            .border_color(rgb(palette.BORDER_DEFAULT))
            .text_color(rgb(palette.SLATE))
            .cursor(CursorStyle::Arrow)
            .opacity(0.5);
    } else {
        button = button
            .bg(rgba(bg_color))
            .border_color(rgb(border_color))
            .text_color(rgb(palette.PATCH))
            .cursor_pointer()
            .hover({
                let text = palette.BONE;
                move |style| style.text_color(rgb(text))
            })
            .on_click(on_click);
    }

    button.child(label.to_string())
}
