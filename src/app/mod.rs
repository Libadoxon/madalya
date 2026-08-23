mod keybinds;

use gpui::*;
use gpui_component::{ActiveTheme as _, WindowExt as _, button::*, *};

use crate::config::Config;
use crate::keybinds::{Action, KeyBind, is_cancel_gesture, is_unbind_gesture};
use crate::meta::{APP_DESCRIPTION, APP_DISPLAY_NAME};
use crate::ui::settings::render_settings;

/// Root view of the app. This template keeps only the framework scaffolding:
/// global keybind dispatch, an in-progress keybind recording, and a settings
/// pane toggle. Build your real UI in `render` and add behavior in `dispatch`.
pub struct AppView {
    pub(crate) settings_open: bool,
    pub(crate) recording: Option<KeybindRecording>,
    /// Focus target that receives the window's global key/mouse events so
    /// keybinds dispatch regardless of what's focused inside the app.
    pub(crate) root_focus: FocusHandle,
    pub(crate) _subscriptions: Vec<Subscription>,
}

/// While the user is prompted for a new binding, hold the focus handle that
/// receives key events and the focus-out subscription that exits recording
/// when focus leaves the prompt. `binding_index` points into
/// `Config.keybinds.bindings`; if the binding gets removed mid-recording,
/// the next event check finds the slot empty and bails.
pub(crate) struct KeybindRecording {
    pub(crate) binding_index: usize,
    pub(crate) focus: FocusHandle,
    pub(crate) _focus_out: Subscription,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root_focus = cx.focus_handle();
        root_focus.focus(window, cx);

        // Re-render whenever the config changes — settings edits and external
        // file-watcher reloads both publish a new `Config` global.
        let config_sub = cx.observe_global::<Config>(|_this, cx| cx.notify());

        Self {
            settings_open: false,
            recording: None,
            root_focus,
            _subscriptions: vec![config_sub],
        }
    }

    pub(crate) fn handle_global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Deactivate global keybinds when a dialog is open
        if window.has_active_dialog(cx) {
            if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                window.close_dialog(cx);
                cx.stop_propagation();
            }
            return;
        }
        if let Some(rec) = self.recording.as_ref() {
            let idx = rec.binding_index;
            if is_unbind_gesture(event) {
                // remove_binding also stops recording for this idx
                self.remove_binding(idx, window, cx);
                cx.stop_propagation();
                return;
            }
            if is_cancel_gesture(event) {
                self.stop_recording(window, cx);
                cx.stop_propagation();
                return;
            }
            // Modifier-only keystrokes make poor bindings.
            // They fire repeatedly while held and can't be distinguished
            // from a chord still being assembled. Wait for a real key.
            if is_modifier_key(&event.keystroke.key) {
                return;
            }
            self.commit_binding(idx, KeyBind::from_key(event), window, cx);
            cx.stop_propagation();
            return;
        }

        // Suppress all non modifier bindings while any text input has focus
        if window
            .context_stack()
            .iter()
            .any(|ctx| ctx.contains("Input") || ctx.contains("NumberInput"))
            && !event.keystroke.modifiers.modified()
        {
            return;
        }

        if let Some(action) = cx.global::<Config>().keybinds.match_key(event) {
            self.dispatch(action, window, cx);
            cx.stop_propagation();
        }
    }

    pub(crate) fn handle_global_mouse(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rec) = self.recording.as_ref() {
            // Left click without modifiers is reserved as the "interact
            // normally / cancel" gesture so the user can always click outside
            // the recorder. Modifier+Left and other buttons are bindable.
            if event.button == MouseButton::Left && !event.modifiers.modified() {
                return;
            }
            let idx = rec.binding_index;
            self.commit_binding(idx, KeyBind::from_mouse(event), window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(action) = cx.global::<Config>().keybinds.match_mouse(event) {
            self.dispatch(action, window, cx);
            cx.stop_propagation();
        }
    }

    /// Turn a matched `Action` into behavior. Add an arm here for every
    /// variant you add to `keybinds::Action`.
    fn dispatch(&mut self, action: Action, _window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::ToggleSettings => self.toggle_settings(cx),
            Action::Close => cx.quit(),
            Action::RunCommand(cmd) => {
                // Demo payload action. The command string is editable inline in
                // the keybind settings row — swap this for real behavior.
                tracing::info!("run command: {cmd}");
            }
        }
    }

    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        cx.notify();
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let top_bar = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().font_semibold().child(APP_DISPLAY_NAME))
            .child(
                Button::new("toggle-settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_settings(cx))),
            );

        let body: AnyElement = if self.settings_open {
            render_settings(self, cx).into_any_element()
        } else {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(div().text_lg().child(APP_DISPLAY_NAME))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(APP_DESCRIPTION),
                )
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("Press Ctrl+, or the gear to open settings."),
                )
                .into_any_element()
        };

        // Root holds dialogs/notifications in its own state, but the app's root
        // view has to paint those layers itself — Root::render doesn't.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        div()
            .track_focus(&self.root_focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_1()
            .border_color(cx.theme().border)
            .capture_key_down(cx.listener(|this, event, window, cx| {
                this.handle_global_key(event, window, cx);
            }))
            .capture_any_mouse_down(cx.listener(|this, event, window, cx| {
                this.handle_global_mouse(event, window, cx);
            }))
            .child(top_bar)
            .child(body)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "shift" | "control" | "ctrl" | "alt" | "platform" | "cmd" | "super" | "function" | "fn"
    )
}
