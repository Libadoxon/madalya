use gpui_kit::*;

use crate::keybinds::{Action, Binding, KeyBind};

use super::{AppView, KeybindRecording};

impl AppView {
    /// Enter recording mode for the binding at `binding_index`: build a focus
    /// handle to receive key events, focus it, and wire a focus-out
    /// subscription so clicking away cancels recording without binding.
    pub(crate) fn start_recording(
        &mut self,
        binding_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        let weak = cx.weak_entity();
        let sub = window.on_focus_out(&focus, cx, move |_event, _window, cx| {
            _ = weak.update(cx, |this, ctx| {
                if this.recording.is_some() {
                    this.recording = None;
                    ctx.notify();
                }
            });
        });
        focus.focus(window, cx);
        self.recording = Some(KeybindRecording {
            binding_index,
            focus,
            _focus_out: sub,
        });
        cx.notify();
    }

    pub(super) fn stop_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            self.root_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Persist `bind` into the recording target slot, then exit recording.
    /// Silently bails if the slot was removed from the config while the
    /// recorder was open.
    pub(super) fn commit_binding(
        &mut self,
        idx: usize,
        bind: KeyBind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::config::update(cx, |c| {
            if let Some(b) = c.keybinds.bindings.get_mut(idx) {
                b.bind = bind;
            }
        });
        self.stop_recording(window, cx);
    }

    pub(crate) fn set_binding_action(
        &mut self,
        idx: usize,
        action: Action,
        cx: &mut Context<Self>,
    ) {
        crate::config::update(cx, |c| {
            if let Some(b) = c.keybinds.bindings.get_mut(idx) {
                b.action = action;
            }
        });
    }

    pub(crate) fn add_binding(&mut self, cx: &mut Context<Self>) {
        crate::config::update(cx, |c| c.keybinds.bindings.push(Binding::default()));
    }

    pub(crate) fn remove_binding(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Cancel recording if it targeted this slot (or any later one — those
        // indices would shift down and refer to the wrong binding).
        if let Some(rec) = self.recording.as_ref()
            && rec.binding_index >= idx
        {
            self.stop_recording(window, cx);
        }
        crate::config::update(cx, |c| {
            if idx < c.keybinds.bindings.len() {
                c.keybinds.bindings.remove(idx);
            }
        });
    }
}
