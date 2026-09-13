mod keybinds;

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, button::*, *};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::config::Config;
use crate::keybinds::{Action, KeyBind, is_cancel_gesture, is_unbind_gesture};
use crate::library::Library;
use crate::library::model::Clip;
use crate::media::player::{Player, PlayerOptions};
use crate::ui::filter::{FilterEvent, FilterState};
use crate::ui::fullscreen::Fullscreen;
use crate::ui::grid::render_grid;
use crate::ui::settings::render_settings;

const PREVIEW_WIDTH: u32 = 480;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Grid,
    Fullscreen,
}

pub(crate) struct PreviewState {
    pub path: PathBuf,
    pub player: Entity<Player>,
}

type LibCfgKey = (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>, u32);

pub struct AppView {
    pub(crate) settings_open: bool,
    pub(crate) recording: Option<KeybindRecording>,
    pub(crate) root_focus: FocusHandle,
    pub(crate) library: Entity<Library>,
    pub(crate) mode: Mode,
    pub(crate) selected: usize,
    pub(crate) fullscreen: Option<Entity<Fullscreen>>,
    pub(crate) preview: Option<PreviewState>,
    pub(crate) filter: Entity<FilterState>,
    pub(crate) grid_scroll: ScrollHandle,
    hover_target: Option<PathBuf>,
    preview_debounce: Option<Task<()>>,
    last_lib_cfg: LibCfgKey,
    pending_rescan: bool,
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
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root_focus = cx.focus_handle();
        root_focus.focus(window, cx);

        let config_sub = cx.observe_global::<Config>(|this, cx| {
            this.on_config_changed(cx);
            cx.notify();
        });
        let lib_sub = cx.observe(&library, |_this, _lib, cx| cx.notify());

        let filter = cx.new(|cx| FilterState::new(window, cx));
        let filter_sub = cx.subscribe(&filter, |this, _f, _ev: &FilterEvent, cx| {
            this.selected = 0;
            cx.notify();
        });

        let last_lib_cfg = lib_cfg_key(cx);
        library.update(cx, |l, cx| l.rescan(false, cx));

        Self {
            settings_open: false,
            recording: None,
            root_focus,
            library,
            mode: Mode::Grid,
            selected: 0,
            fullscreen: None,
            preview: None,
            filter,
            grid_scroll: ScrollHandle::new(),
            hover_target: None,
            preview_debounce: None,
            last_lib_cfg,
            pending_rescan: false,
            _subscriptions: vec![config_sub, lib_sub, filter_sub],
        }
    }

    fn on_config_changed(&mut self, cx: &mut Context<Self>) {
        let key = lib_cfg_key(cx);
        if key != self.last_lib_cfg {
            self.last_lib_cfg = key;
            // Defer while settings are open so typing a path doesn't scan.
            if self.settings_open {
                self.pending_rescan = true;
            } else {
                self.library.update(cx, |l, cx| l.rescan(false, cx));
            }
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

    pub(crate) fn dispatch(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::ToggleSettings => self.toggle_settings(cx),
            Action::Close => cx.quit(),
            Action::OpenSelected => self.open_fullscreen(self.selected, window, cx),
            Action::ExitFullscreen => self.exit_fullscreen(window, cx),
            Action::NextClip => self.navigate(1, window, cx),
            Action::PrevClip => self.navigate(-1, window, cx),
            Action::PlayPause => {
                if let Some(fs) = &self.fullscreen {
                    fs.update(cx, |f, cx| f.toggle_play(cx));
                }
            }
            Action::Replay => {
                if let Some(fs) = &self.fullscreen {
                    fs.update(cx, |f, cx| f.replay(cx));
                }
            }
            Action::ToggleMute => {
                if let Some(fs) = &self.fullscreen {
                    fs.update(cx, |f, cx| f.toggle_mute(cx));
                }
            }
            Action::ToggleFavorite => self.toggle_favorite(cx),
        }
    }

    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        let closing = self.settings_open;
        self.settings_open = !self.settings_open;
        if closing && self.pending_rescan {
            self.pending_rescan = false;
            self.rescan_library(false, cx);
        }
        cx.notify();
    }

    pub(crate) fn rescan_library(&mut self, force: bool, cx: &mut Context<Self>) {
        self.library.update(cx, |l, cx| l.rescan(force, cx));
    }

    /// Clips passing the active filters, in library order.
    pub(crate) fn visible_clips(&self, cx: &App) -> Vec<Clip> {
        let filters = self.filter.read(cx).filters();
        self.library
            .read(cx)
            .clips()
            .iter()
            .filter(|c| filters.matches(c))
            .cloned()
            .collect()
    }

    pub(crate) fn clip_count(&self, cx: &App) -> usize {
        self.visible_clips(cx).len()
    }

    pub(crate) fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.selected = idx;
        cx.notify();
    }

    fn navigate(&mut self, delta: i64, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.clip_count(cx);
        if count == 0 {
            return;
        }
        let next = (self.selected as i64 + delta).rem_euclid(count as i64) as usize;
        self.selected = next;
        if self.mode == Mode::Fullscreen {
            self.stop_preview(cx);
            let clip = self.visible_clips(cx).get(next).cloned();
            if let (Some(fs), Some(clip)) = (&self.fullscreen, clip) {
                fs.update(cx, |f, cx| f.load(clip, window, cx));
            }
        }
        cx.notify();
    }

    pub(crate) fn open_fullscreen(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(clip) = self.visible_clips(cx).get(idx).cloned() else {
            return;
        };
        self.selected = idx;
        self.hover_target = None;
        self.preview_debounce = None;
        self.stop_preview(cx);
        let library = self.library.clone();
        let app = cx.weak_entity();
        let fs = cx.new(|cx| Fullscreen::new(library, app, clip, window, cx));
        self.fullscreen = Some(fs);
        self.mode = Mode::Fullscreen;
        self.root_focus.focus(window, cx);
        cx.notify();
    }

    fn exit_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != Mode::Fullscreen {
            return;
        }
        self.fullscreen = None;
        self.mode = Mode::Grid;
        self.root_focus.focus(window, cx);
        cx.notify();
    }

    fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let idx = self.selected;
        let Some(clip) = self.visible_clips(cx).get(idx).cloned() else {
            return;
        };
        self.library
            .update(cx, |l, cx| l.set_favorite(&clip.path, !clip.favorite, cx));
    }

    /// Cursor entered a tile. Debounced so scrolling past tiles doesn't spin up
    /// a GStreamer pipeline for every one — only start after the cursor rests.
    pub(crate) fn hover_clip(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.hover_target.as_ref() == Some(&path) {
            return;
        }
        self.hover_target = Some(path.clone());
        self.preview_debounce = Some(cx.spawn(async move |this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = this.update(cx, |this, cx| {
                if this.hover_target.as_ref() == Some(&path) {
                    this.start_preview(path.clone(), cx);
                }
            });
        }));
    }

    /// Cursor left a tile. Only clears if it's still the current hover target so
    /// a stale leave (after we've moved to a new tile) doesn't cancel it.
    pub(crate) fn unhover_clip(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.hover_target.as_deref() == Some(path) {
            self.hover_target = None;
            self.preview_debounce = None;
            self.stop_preview(cx);
        }
    }

    fn start_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.preview.as_ref().is_some_and(|p| p.path == path) {
            return;
        }
        let Ok(uri) = crate::media::path_to_uri(&path) else {
            return;
        };
        let clip = self
            .library
            .read(cx)
            .clips()
            .iter()
            .find(|c| c.path == path);
        let start_ms = clip.and_then(|c| c.mark_start).unwrap_or(0);
        let stop_ms = clip.and_then(|c| c.marks()).map(|(_, end)| end);
        let opts = PlayerOptions {
            muted: true,
            looping: true,
            preview_width: Some(PREVIEW_WIDTH),
            start_ms,
            stop_ms,
            ..Default::default()
        };
        let player = cx.new(|cx| Player::new(&uri, opts, cx));
        self.preview = Some(PreviewState { path, player });
        cx.notify();
    }

    fn stop_preview(&mut self, cx: &mut Context<Self>) {
        if self.preview.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn preview_for(&self, path: &std::path::Path) -> Option<Entity<Player>> {
        self.preview
            .as_ref()
            .filter(|p| p.path == path)
            .map(|p| p.player.clone())
    }

    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.filter.read(cx).search.clone();
        div()
            .w(px(460.))
            .text_base()
            .child(gpui_kit::component::input::Input::new(&search).large())
    }

    fn render_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.filter.read(cx).filters().is_active();
        let filter = self.filter.clone();
        let clips = self.visible_clips_unfiltered(cx);
        gpui_kit::component::popover::Popover::new("filters")
            .overlay_closable(false)
            .trigger(
                Button::new("filters")
                    .ghost()
                    .large()
                    .selected(active)
                    .icon(IconName::Funnel)
                    .tooltip("Filter"),
            )
            .content(move |_, _, cx| crate::ui::filter::panel(&filter, &clips, cx))
    }

    /// All clips ignoring filters — used to populate the filter popover's tag and
    /// game choices, so hidden options stay selectable.
    fn visible_clips_unfiltered(&self, cx: &App) -> Vec<Clip> {
        self.library.read(cx).clips().to_vec()
    }
}

fn lib_cfg_key(cx: &App) -> LibCfgKey {
    let lib = &cx.global::<Config>().library;
    (
        lib.clips_dir.clone(),
        lib.mdata_script_path.clone(),
        lib.mix_script_path.clone(),
        lib.max_scan_depth,
    )
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fullscreen_mode = self.mode == Mode::Fullscreen && !self.settings_open;
        let grid_mode = !fullscreen_mode && !self.settings_open;

        let top_bar = h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_1()
                    .gap_2()
                    .items_center()
                    .when(fullscreen_mode, |el| {
                        el.child(
                            Button::new("back-to-grid")
                                .ghost()
                                .large()
                                .icon(IconName::ArrowLeft)
                                .tooltip("Back to grid")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.dispatch(Action::ExitFullscreen, window, cx)
                                })),
                        )
                    })
                    .child(div().font_semibold().child(crate::meta::APP_DISPLAY_NAME)),
            )
            .when(grid_mode, |el| {
                el.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(self.render_search(cx))
                        .child(self.render_filter(cx)),
                )
            })
            .child(
                h_flex().flex_1().justify_end().child(
                    Button::new("toggle-settings")
                        .ghost()
                        .large()
                        .icon(IconName::Settings)
                        .tooltip("Settings")
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_settings(cx))),
                ),
            );

        let body: AnyElement = if self.settings_open {
            render_settings(self, cx).into_any_element()
        } else if fullscreen_mode {
            match &self.fullscreen {
                Some(fs) => fs.clone().into_any_element(),
                None => div().into_any_element(),
            }
        } else {
            render_grid(self, window, cx).into_any_element()
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
