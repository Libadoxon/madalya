use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, AxisExt as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    ThemeRegistry, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    popover::Popover,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    setting::{
        NumberFieldOptions, RenderOptions, SettingField, SettingGroup, SettingItem, SettingPage,
        Settings,
    },
    v_flex,
};
use strum::IntoEnumIterator;

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
            SettingPage::new("Library").default_open(true).group(
                SettingGroup::new()
                    .item(
                        path_item(
                            "Clips directory",
                            readonly,
                            get_clips_dir,
                            set_clips_dir,
                            PathKind::Dir,
                        )
                        .description("Folder scanned recursively for video clips."),
                    )
                    .item(
                        path_item(
                            "Metadata script",
                            readonly,
                            get_script_path,
                            set_script_path,
                            PathKind::File,
                        )
                        .description("Optional Rhai script that derives per-clip metadata and tags."),
                    )
                    .item(bool_item(
                        "Preview on hover",
                        readonly,
                        |c| c.library.preview_on_hover,
                        |c, v| c.library.preview_on_hover = v,
                    ))
                    .item(number_item(
                        "Thumbnail width (px)",
                        readonly,
                        NumberFieldOptions {
                            min: 80.0,
                            max: 1_000.0,
                            step: 20.0,
                        },
                        |c| c.library.thumb_px as f64,
                        |c, v| c.library.thumb_px = v.clamp(80.0, 1_000.0) as u32,
                    ))
                    .item(
                        number_item(
                            "Max scan depth",
                            readonly,
                            NumberFieldOptions {
                                min: 1.0,
                                max: 64.0,
                                step: 1.0,
                            },
                            |c| c.library.max_scan_depth as f64,
                            |c, v| c.library.max_scan_depth = v.clamp(1.0, 64.0) as u32,
                        )
                        .description("How many folder levels below the clip home to scan."),
                    ),
            ),
        )
        .page(
            SettingPage::new("Script")
                .description("Rhai metadata script, also editable on disk. Save re-runs it across the library.")
                .group(SettingGroup::new().item(script_editor_item(app_weak.clone(), readonly))),
        )
        .page(
            SettingPage::new("General").group(
                SettingGroup::new()
                    .item(theme_item(readonly))
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
    }

    SettingItem::render(move |_opts, window, cx: &mut App| {
        render_keybind_row(idx, app.clone(), readonly, window, cx)
    })
    .keywords(keywords)
    .disabled(readonly)
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

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(action_picker)
        .child(bind_widget)
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

#[derive(Clone, Copy, PartialEq)]
enum PathKind {
    Dir,
    File,
}

struct PathInputState {
    input: Entity<InputState>,
    _sub: Subscription,
}

/// A free-form path field. Accepts any text, commits on Enter/Blur (so it
/// never fights per-keystroke), and shows an error notification if the path
/// doesn't exist.
fn path_item(
    label: &'static str,
    readonly: bool,
    get: fn(&Config) -> String,
    set: fn(&mut Config, String),
    kind: PathKind,
) -> SettingItem {
    SettingItem::new(
        label,
        SettingField::element(
            move |opts: &RenderOptions, window: &mut Window, cx: &mut App| {
                render_path_input(label, get, set, kind, readonly, opts.layout, window, cx)
            },
        ),
    )
    .disabled(readonly)
}

#[allow(clippy::too_many_arguments)]
fn render_path_input(
    key: &'static str,
    get: fn(&Config) -> String,
    set: fn(&mut Config, String),
    kind: PathKind,
    readonly: bool,
    layout: Axis,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let current = get(cx.global::<Config>());
    let state = window.use_keyed_state(
        SharedString::from(format!("path-input-{key}")),
        cx,
        |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(current.clone()));
            let sub = cx.subscribe_in(
                &input,
                window,
                move |_this, input, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        commit_path(input.read(cx).value().to_string(), set, kind, window, cx);
                    }
                },
            );
            PathInputState { input, _sub: sub }
        },
    );

    let input = state.read(cx).input.clone();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    if !focused && input.read(cx).value() != current.as_str() {
        input.update(cx, |s, cx| s.set_value(current.clone(), window, cx));
    }

    Input::new(&input)
        .disabled(readonly)
        .map(|this| {
            if layout.is_horizontal() {
                this.w_96()
            } else {
                this.w_full()
            }
        })
        .into_any_element()
}

fn commit_path(
    raw: String,
    set: fn(&mut Config, String),
    kind: PathKind,
    window: &mut Window,
    cx: &mut App,
) {
    let path = raw.trim().to_string();
    config::update(cx, |c| set(c, path.clone()));
    if path.is_empty() {
        return;
    }
    let p = std::path::Path::new(&path);
    let ok = match kind {
        PathKind::Dir => p.is_dir(),
        PathKind::File => p.is_file(),
    };
    if !ok {
        let what = match kind {
            PathKind::Dir => "directory",
            PathKind::File => "file",
        };
        window.push_notification(Notification::error(format!("No such {what}: {path}")), cx);
    }
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

struct ScriptEditorState {
    input: Entity<InputState>,
}

fn script_file_path(cx: &App) -> PathBuf {
    cx.global::<Config>()
        .library
        .script_path
        .clone()
        .unwrap_or_else(config::default_script_path)
}

fn script_editor_item(app: WeakEntity<AppView>, readonly: bool) -> SettingItem {
    SettingItem::render(move |_opts, window, cx: &mut App| {
        render_script_editor(app.clone(), readonly, window, cx)
    })
    .disabled(readonly)
}

fn render_script_editor(
    app: WeakEntity<AppView>,
    readonly: bool,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let path = script_file_path(cx);
    let state = window.use_keyed_state(SharedString::from("script-editor"), cx, |window, cx| {
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("rust")
                .line_number(true)
                .default_value(contents)
        });
        ScriptEditorState { input }
    });
    let input = state.read(cx).input.clone();
    let save_path = path.clone();
    let save_input = input.clone();

    v_flex()
        .gap_2()
        .child(
            div()
                .h(px(440.))
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(Input::new(&input).h_full().disabled(readonly)),
        )
        .child(
            Button::new("script-save")
                .primary()
                .label("Save & Rescan")
                .disabled(readonly)
                .on_click(move |_, _window, cx| {
                    let text = save_input.read(cx).value().to_string();
                    if let Err(e) = std::fs::write(&save_path, &text) {
                        tracing::error!("failed to write script: {e:#}");
                        return;
                    }
                    let p = save_path.clone();
                    config::update(cx, |c| c.library.script_path = Some(p.clone()));
                    let _ = app.update(cx, |a, cx| a.rescan_library(cx));
                }),
        )
}

struct ThemeSelectState {
    select: Entity<SelectState<SearchableVec<SharedString>>>,
    _sub: Subscription,
}

fn theme_item(readonly: bool) -> SettingItem {
    SettingItem::new(
        "Theme",
        SettingField::element(
            move |_opts: &RenderOptions, window: &mut Window, cx: &mut App| {
                render_theme_select(readonly, window, cx)
            },
        ),
    )
    .disabled(readonly)
}

fn render_theme_select(readonly: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let names: Vec<SharedString> = ThemeRegistry::global(cx)
        .sorted_themes()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    let current = SharedString::from(cx.global::<Config>().appearance.theme.clone());
    let selected = names.iter().position(|n| n == &current).map(IndexPath::new);

    let state =
        window.use_keyed_state(SharedString::from("theme-select"), cx, move |window, cx| {
            let select = cx.new(|cx| {
                SelectState::new(SearchableVec::new(names), selected, window, cx).searchable(true)
            });
            let sub = cx.subscribe(
                &select,
                |_this, _select, ev: &SelectEvent<SearchableVec<SharedString>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = ev {
                        let value = value.to_string();
                        config::update(cx, |c| c.appearance.theme = value.clone());
                    }
                },
            );
            ThemeSelectState { select, _sub: sub }
        });

    let select = state.read(cx).select.clone();
    Select::new(&select)
        .small()
        .menu_width(px(240.))
        .menu_max_h(px(360.))
        .search_placeholder("Search themes…")
        .disabled(readonly)
        .into_any_element()
}

fn get_clips_dir(c: &Config) -> String {
    c.library
        .clips_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn set_clips_dir(c: &mut Config, v: String) {
    c.library.clips_dir = (!v.trim().is_empty()).then(|| PathBuf::from(v.trim()));
}

fn get_script_path(c: &Config) -> String {
    c.library
        .script_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn set_script_path(c: &mut Config, v: String) {
    c.library.script_path = (!v.trim().is_empty()).then(|| PathBuf::from(v.trim()));
}
