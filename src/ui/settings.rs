use std::str::FromStr;

use gpui::*;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    popover::Popover,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    v_flex,
};
use strum::{EnumMessage, IntoEnumIterator};

use crate::app::AppView;
use crate::config::{self, Config, ConfigStatus};
use crate::keybinds::Action;

/// Build the settings pane. Each `.page(...)` maps to a section in the sidebar;
/// the General page shows one of every `SettingField` helper, and the Keybinds
/// page hosts the action/trigger editor.
pub fn render_settings(_app: &AppView, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let readonly = cx.global::<ConfigStatus>().readonly;
    let app_weak = cx.weak_entity();

    Settings::new("settings")
        .sidebar_width(px(135.))
        .page(
            SettingPage::new("General").default_open(true).group(
                SettingGroup::new()
                    .item(bool_item(
                        "Example toggle",
                        readonly,
                        |c| c.general.example_toggle,
                        |c, v| c.general.example_toggle = v,
                    ))
                    .item(dropdown_item(
                        "Example choice",
                        readonly,
                        |c| c.general.example_choice,
                        |c, v| c.general.example_choice = v,
                    ))
                    .item(number_item(
                        "Example number",
                        readonly,
                        NumberFieldOptions {
                            min: 0.0,
                            max: 1_000.0,
                            step: 1.0,
                        },
                        |c| c.general.example_number as f64,
                        |c, v| c.general.example_number = v.max(0.0) as u32,
                    ))
                    .item(input_item(
                        "Example text",
                        readonly,
                        |c| c.general.example_text.clone(),
                        |c, v| c.general.example_text = v,
                    ))
                    .item(
                        bool_item(
                            "Write log file",
                            readonly,
                            |c| c.general.log_to_file,
                            |c, v| c.general.log_to_file = v,
                        )
                        .description(
                            "Tee stderr logs into a per-launch file under the user state dir. \
                             Takes effect on next launch.",
                        ),
                    ),
            ),
        )
        .page(
            SettingPage::new("Keybinds")
                .description("Click a binding to record a key (mouse buttons work too). Ctrl+Esc deletes. Esc cancels.")
                .group({
                    let count = cx.global::<Config>().keybinds.bindings.len();
                    let group =
                        (0..count).fold(SettingGroup::new(), |group, idx| {
                            group.item(keybind_row_item(idx, app_weak.clone(), readonly, cx))
                        });
                    group.item(add_binding_item(app_weak.clone(), readonly))
                }),
        )
}

fn keybind_row_item(idx: usize, app: WeakEntity<AppView>, readonly: bool, cx: &App) -> SettingItem {
    // Build the keyword list once at construction time from the current
    // binding so the framework's search input matches on the action label,
    // the bind's display text ("Ctrl + Left"), and for RunCommand the
    // configured command string.
    let mut keywords: Vec<SharedString> = Vec::new();
    if let Some(binding) = cx.global::<Config>().keybinds.bindings.get(idx) {
        keywords.push(binding.action.label().into());
        if !binding.bind.is_unbound() {
            keywords.push(binding.bind.to_string().into());
            keywords.push(binding.bind.to_string().replace(" ", "").into());
        }
        if let Action::RunCommand(cmd) = &binding.action
            && !cmd.is_empty()
        {
            keywords.push(cmd.clone().into());
        }
    }

    SettingItem::render(move |_opts, window, cx: &mut App| {
        render_keybind_row(idx, app.clone(), readonly, window, cx)
    })
    .keywords(keywords)
    .disabled(readonly)
}

/// Owns the inline payload-input state for a `RunCommand` binding row. The
/// subscription handle keeps `cx.subscribe` alive for the lifetime of the
/// state entity; dropped when the row stops rendering.
struct PayloadInputState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// Owns the search-input state for a binding row's action picker plus the
/// keyboard-navigation cursor. The subscription forwards `InputEvent::Change`
/// notifications so the popover re-renders with the filtered list and resets
/// the cursor to `None` on every edit.
struct ActionPickerState {
    input: Entity<InputState>,
    /// Index into the currently-filtered items. `None` means no row is
    /// highlighted — only the currently-bound action shows a check icon.
    cursor: Option<usize>,
    /// Preserves scroll offset across re-renders and lets arrow-key nav
    /// keep the cursor row on screen via `scroll_to_item`.
    scroll: ScrollHandle,
    _subscription: Subscription,
}

fn render_keybind_row(
    idx: usize,
    app: WeakEntity<AppView>,
    readonly: bool,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let Some(binding) = cx.global::<Config>().keybinds.bindings.get(idx).cloned() else {
        return div().into_any_element();
    };
    let recording_here = app.upgrade().and_then(|c| {
        c.read(cx)
            .recording
            .as_ref()
            .filter(|r| r.binding_index == idx)
            .map(|r| r.focus.clone())
    });

    let bind_label: SharedString = binding.bind.to_string().into();

    let recorder_id: SharedString = format!("keybind-recorder-{idx}").into();
    let bind_btn_id: SharedString = format!("keybind-set-{idx}").into();
    let delete_btn_id: SharedString = format!("keybind-delete-{idx}").into();

    let action_picker = action_picker_widget(
        idx,
        binding.action.clone(),
        app.clone(),
        readonly,
        window,
        cx,
    );

    // Payload-carrying actions render an extra inline field. `RunCommand` is
    // the template's example: editing this field rewrites the bound action's
    // payload. Add branches here for your own payload variants.
    let payload_input_widget: Option<AnyElement> = if let Action::RunCommand(cmd) = &binding.action
    {
        let initial: SharedString = cmd.clone().into();
        let app_for_input = app.clone();
        let state = window.use_keyed_state(
            SharedString::from(format!("kb-payload-{idx}")),
            cx,
            |window, cx| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(initial.clone())
                        .placeholder("command to run")
                });
                let sub = cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let value = input.read(cx).value().to_string();
                        _ = app_for_input.update(cx, |this, ctx| {
                            // Guard against races where the row has been
                            // switched away from RunCommand since this input
                            // was created.
                            let still_cmd = ctx
                                .global::<Config>()
                                .keybinds
                                .bindings
                                .get(idx)
                                .is_some_and(|b| matches!(b.action, Action::RunCommand(_)));
                            if still_cmd {
                                this.set_binding_action(idx, Action::RunCommand(value), ctx);
                            }
                        });
                    }
                });
                PayloadInputState {
                    input,
                    _subscription: sub,
                }
            },
        );
        // Sync the input when the config changed externally (file watcher,
        // config reset). Skip while focused so we don't yank text from
        // under the user mid-edit.
        let input_entity = state.read(cx).input.clone();
        let current = input_entity.read(cx).value().to_string();
        let target = cmd.clone();
        let focused = input_entity.read(cx).focus_handle(cx).is_focused(window);
        if current != target && !focused {
            input_entity.update(cx, |s, cx| s.set_value(target, window, cx));
        }
        Some(
            Input::new(&input_entity)
                .small()
                .disabled(readonly)
                .into_any_element(),
        )
    } else {
        None
    };

    let bind_widget: AnyElement = if let Some(focus) = recording_here {
        div()
            .id(recorder_id)
            .track_focus(&focus)
            .px_3()
            .py_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().primary)
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .child("Press a key…")
            .into_any_element()
    } else {
        let app = app.clone();
        Button::new(bind_btn_id)
            .ghost()
            .small()
            .label(bind_label)
            .disabled(readonly)
            .on_click(move |_, window, cx| {
                _ = app.update(cx, |this, ctx| {
                    this.start_recording(idx, window, ctx);
                });
            })
            .into_any_element()
    };

    let app_for_delete = app.clone();
    let delete_btn = Button::new(delete_btn_id)
        .ghost()
        .small()
        .icon(IconName::Delete)
        .tooltip("Remove binding")
        .disabled(readonly)
        .on_click(move |_, window, cx| {
            _ = app_for_delete.update(cx, |this, ctx| {
                this.remove_binding(idx, window, ctx);
            });
        });

    let mut row = h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(action_picker);
    if let Some(w) = payload_input_widget {
        row = row.child(w);
    }
    row.child(bind_widget)
        .child(div().flex_1())
        .child(delete_btn)
        .into_any_element()
}

/// A dropdown-style action picker with an inline search input and a
/// scrollable list of left-aligned rows. Nothing is auto-highlighted while
/// typing; Up/Down arrows drive an explicit cursor and Enter picks it.
/// Esc dismisses cleanly without mutating the binding.
fn action_picker_widget(
    idx: usize,
    current_action: Action,
    app: WeakEntity<AppView>,
    readonly: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let picker_state = window.use_keyed_state(
        SharedString::from(format!("kb-action-{idx}")),
        cx,
        |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search actions"));
            let sub = cx.subscribe(
                &input,
                |this: &mut ActionPickerState, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        // Typing invalidates any keyboard cursor — force the
                        // user to re-enter navigation from the top of the new
                        // list.
                        this.cursor = None;
                        cx.notify();
                    }
                },
            );
            ActionPickerState {
                input,
                cursor: None,
                scroll: ScrollHandle::new(),
                _subscription: sub,
            }
        },
    );

    let input = picker_state.read(cx).input.clone();
    let input_focus = input.read(cx).focus_handle(cx);
    let current_label: SharedString = current_action.label().into();
    let action_btn_id: SharedString = format!("keybind-action-{idx}").into();
    let popover_id: SharedString = format!("kb-action-popover-{idx}").into();

    Popover::new(popover_id)
        .track_focus(&input_focus)
        .trigger(
            Button::new(action_btn_id)
                .outline()
                .small()
                .label(current_label)
                .dropdown_caret(true)
                .disabled(readonly),
        )
        .on_open_change({
            let input = input.clone();
            let picker_state = picker_state.clone();
            move |new_open, window, cx| {
                if !*new_open {
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                    picker_state.update(cx, |s, cx| {
                        s.cursor = None;
                        cx.notify();
                    });
                }
            }
        })
        .content(move |_state, _window, cx| {
            let popover_entity = cx.entity();
            let query = input.read(cx).value().to_lowercase();
            let items: Vec<Action> = Action::iter()
                .filter(|a| query.is_empty() || a.label().to_lowercase().contains(&query))
                .collect();
            let cursor = picker_state.read(cx).cursor;
            let scroll = picker_state.read(cx).scroll.clone();

            let commit = {
                let app = app.clone();
                let current = current_action.clone();
                let popover_entity = popover_entity.clone();
                move |action: Action, window: &mut Window, cx: &mut App| {
                    if !action.same_kind(&current) {
                        let action = action.clone();
                        _ = app.update(cx, |this, ctx| {
                            this.set_binding_action(idx, action, ctx);
                        });
                    }
                    popover_entity.update(cx, |s, cx| s.dismiss(window, cx));
                }
            };

            let mut list = v_flex()
                .id("kb-action-list")
                .max_h(px(240.))
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .gap_0p5();
            for (i, action) in items.iter().enumerate() {
                let checked = action.same_kind(&current_action);
                let is_cursor = cursor == Some(i);
                let label: SharedString = action.label().into();
                let item_id = SharedString::from(format!("kb-action-item-{idx}-{i}"));
                let action_for_click = action.clone();
                let commit_for_click = commit.clone();

                let mut row = h_flex()
                    .id(item_id)
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .hover(|s| {
                        s.bg(cx.theme().accent)
                            .text_color(cx.theme().accent_foreground)
                    })
                    .child(label);
                if is_cursor {
                    row = row
                        .bg(cx.theme().accent)
                        .text_color(cx.theme().accent_foreground);
                }
                if checked {
                    row = row.child(Icon::new(IconName::Check).xsmall());
                }
                list = list.child(row.on_click(move |_, window, cx| {
                    commit_for_click(action_for_click.clone(), window, cx);
                }));
            }

            v_flex()
                .w(px(260.))
                .gap_2()
                .capture_key_down({
                    let picker_state = picker_state.clone();
                    let popover_entity = popover_entity.clone();
                    let items = items.clone();
                    let commit = commit.clone();
                    move |event, window, cx| {
                        let key = event.keystroke.key.as_str();
                        match key {
                            "down" => {
                                if items.is_empty() {
                                    return;
                                }
                                let last = items.len() - 1;
                                picker_state.update(cx, |s, cx| {
                                    let next = match s.cursor {
                                        None => 0,
                                        Some(c) => (c + 1).min(last),
                                    };
                                    s.cursor = Some(next);
                                    s.scroll.scroll_to_item(next);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            }
                            "up" => {
                                if items.is_empty() {
                                    return;
                                }
                                let last = items.len() - 1;
                                picker_state.update(cx, |s, cx| {
                                    let next = match s.cursor {
                                        None => last,
                                        Some(c) => c.saturating_sub(1),
                                    };
                                    s.cursor = Some(next);
                                    s.scroll.scroll_to_item(next);
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            }
                            "enter" => {
                                let Some(c) = picker_state.read(cx).cursor else {
                                    return;
                                };
                                let Some(action) = items.get(c).cloned() else {
                                    return;
                                };
                                commit(action, window, cx);
                                cx.stop_propagation();
                            }
                            "escape" => {
                                popover_entity.update(cx, |s, cx| s.dismiss(window, cx));
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    }
                })
                .child(Input::new(&input).small())
                .child(list)
        })
        .into_any_element()
}

fn add_binding_item(app: WeakEntity<AppView>, readonly: bool) -> SettingItem {
    SettingItem::render(move |_opts, _window, _cx: &mut App| {
        let app = app.clone();
        v_flex().child(
            Button::new("keybind-add")
                .outline()
                .small()
                .icon(IconName::Plus)
                .label("Add binding")
                .disabled(readonly)
                .on_click(move |_, _, cx| {
                    _ = app.update(cx, |this, ctx| {
                        this.add_binding(ctx);
                    });
                }),
        )
    })
    .disabled(readonly)
}

fn bool_item(
    label: &'static str,
    disabled: bool,
    get: fn(&Config) -> bool,
    set: fn(&mut Config, bool),
) -> SettingItem {
    SettingItem::new(
        label,
        SettingField::switch(
            move |cx: &App| get(cx.global::<Config>()),
            move |val: bool, cx: &mut App| config::update(cx, |c| set(c, val)),
        ),
    )
    .disabled(disabled)
}

fn input_item(
    label: &'static str,
    disabled: bool,
    get: fn(&Config) -> String,
    set: fn(&mut Config, String),
) -> SettingItem {
    SettingItem::new(
        label,
        SettingField::input(
            move |cx: &App| SharedString::from(get(cx.global::<Config>())),
            move |val: SharedString, cx: &mut App| config::update(cx, |c| set(c, val.to_string())),
        ),
    )
    .disabled(disabled)
}

fn number_item(
    label: &'static str,
    disabled: bool,
    options: NumberFieldOptions,
    get: fn(&Config) -> f64,
    set: fn(&mut Config, f64),
) -> SettingItem {
    SettingItem::new(
        label,
        SettingField::number_input(
            options,
            move |cx: &App| get(cx.global::<Config>()),
            move |val: f64, cx: &mut App| config::update(cx, |c| set(c, val)),
        ),
    )
    .disabled(disabled)
}

/// Render a dropdown bound to a config enum. Variant identifiers serve as keys
/// (matching their serde representation in the TOML file) and `#[strum(message
/// = ...)]` provides the human-readable label.
fn dropdown_item<T>(
    label: &'static str,
    disabled: bool,
    get: fn(&Config) -> T,
    set: fn(&mut Config, T),
) -> SettingItem
where
    T: 'static + Copy + IntoEnumIterator + Into<&'static str> + FromStr + EnumMessage,
{
    let options: Vec<(SharedString, SharedString)> = T::iter()
        .map(|v| {
            let key: &'static str = v.into();
            let label = v.get_message().unwrap_or(key);
            (SharedString::from(key), SharedString::from(label))
        })
        .collect();
    SettingItem::new(
        label,
        SettingField::dropdown(
            options,
            move |cx: &App| {
                let key: &'static str = get(cx.global::<Config>()).into();
                SharedString::from(key)
            },
            move |val: SharedString, cx: &mut App| {
                if let Ok(v) = T::from_str(val.as_ref()) {
                    config::update(cx, |c| set(c, v));
                }
            },
        ),
    )
    .disabled(disabled)
}
