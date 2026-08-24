use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, ElementExt as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    popover::Popover,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};

use crate::app::AppView;
use crate::keybinds::Action;
use crate::library::Library;
use crate::library::model::{Clip, TrackState};
use crate::media;
use crate::media::player::{Player, PlayerOptions};

pub struct Fullscreen {
    library: Entity<Library>,
    app: WeakEntity<AppView>,
    clip: Clip,
    player: Entity<Player>,
    dragging: bool,
    volumes: Vec<Entity<SliderState>>,
    tracks: Vec<TrackState>,
    master_muted: bool,
    tag_input: Entity<InputState>,
    clear_tag: bool,
    title_input: Entity<InputState>,
    game_input: Entity<InputState>,
    mark_start_input: Entity<InputState>,
    mark_end_input: Entity<InputState>,
    next_mark_is_start: bool,
    timeline_bounds: Rc<Cell<Bounds<Pixels>>>,
    sync_fields: bool,
    _subs: Vec<Subscription>,
}

impl Fullscreen {
    pub fn new(
        library: Entity<Library>,
        app: WeakEntity<AppView>,
        clip: Clip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add tag…"));
        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("Title…"));
        let game_input = cx.new(|cx| InputState::new(window, cx).placeholder("Game…"));
        let mark_start_input = cx.new(|cx| InputState::new(window, cx).placeholder("m:ss"));
        let mark_end_input = cx.new(|cx| InputState::new(window, cx).placeholder("m:ss"));
        let (player, volumes, tracks) = build_media(&clip, cx);
        let mut this = Self {
            library,
            app,
            clip,
            player,
            dragging: false,
            volumes,
            tracks,
            master_muted: false,
            tag_input,
            clear_tag: false,
            title_input,
            game_input,
            mark_start_input,
            mark_end_input,
            next_mark_is_start: true,
            timeline_bounds: Rc::new(Cell::new(Bounds::default())),
            sync_fields: true,
            _subs: Vec::new(),
        };
        this.wire(cx);
        this
    }

    pub fn load(&mut self, clip: Clip, _window: &mut Window, cx: &mut Context<Self>) {
        let (player, volumes, tracks) = build_media(&clip, cx);
        self.clip = clip;
        self.player = player;
        self.volumes = volumes;
        self.tracks = tracks;
        self.dragging = false;
        self.master_muted = false;
        self.next_mark_is_start = true;
        self._subs.clear();
        self.clear_tag = true;
        self.sync_fields = true;
        self.wire(cx);
        cx.notify();
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.player.update(cx, |p, cx| p.toggle_play(cx));
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.master_muted = !self.master_muted;
        self.player.read(cx).set_all_muted(self.master_muted);
        cx.notify();
    }

    fn wire(&mut self, cx: &mut Context<Self>) {
        let mut subs = vec![cx.observe(&self.player, |_, _, cx| cx.notify())];

        for (i, vol) in self.volumes.iter().enumerate() {
            subs.push(cx.subscribe(vol, move |this, slider, ev, cx| {
                let v = slider.read(cx).value().start() as f64;
                let Some(st) = this.tracks.get_mut(i) else {
                    return;
                };
                st.volume = v;
                let st = *st;
                this.player.read(cx).set_track(st.idx, st.volume, st.muted);
                if matches!(ev, SliderEvent::Release(_)) {
                    let path = this.clip.path.clone();
                    this.library
                        .update(cx, |l, cx| l.set_track_state(&path, st, cx));
                }
            }));
        }

        subs.push(
            cx.subscribe(&self.title_input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let value = input.read(cx).value().trim().to_string();
                    let title = (!value.is_empty()).then_some(value);
                    if title.as_deref() != this.clip.title_raw() {
                        let path = this.clip.path.clone();
                        this.library
                            .update(cx, |l, cx| l.set_title(&path, title.as_deref(), cx));
                        this.clip.title = title;
                        cx.notify();
                    }
                }
            }),
        );

        subs.push(
            cx.subscribe(&self.game_input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let value = input.read(cx).value().trim().to_string();
                    let game = (!value.is_empty()).then_some(value);
                    if game.as_deref() != this.clip.game() {
                        let path = this.clip.path.clone();
                        this.library
                            .update(cx, |l, cx| l.set_game(&path, game.as_deref(), cx));
                        this.clip.game = game;
                        cx.notify();
                    }
                }
            }),
        );

        subs.push(cx.subscribe(
            &self.mark_start_input,
            |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let ms = parse_time(&input.read(cx).value());
                    this.set_mark(true, ms, cx);
                }
            },
        ));

        subs.push(
            cx.subscribe(&self.mark_end_input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let ms = parse_time(&input.read(cx).value());
                    this.set_mark(false, ms, cx);
                }
            }),
        );

        subs.push(
            cx.subscribe(&self.tag_input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    let tag = input.read(cx).value().trim().to_string();
                    if !tag.is_empty() {
                        let path = this.clip.path.clone();
                        this.library.update(cx, |l, cx| l.add_tag(&path, &tag, cx));
                        if !this.clip.mtags.iter().any(|t| t == &tag) {
                            this.clip.mtags.push(tag);
                            this.clip.mtags.sort();
                        }
                        this.clear_tag = true;
                        cx.notify();
                    }
                }
            }),
        );

        self._subs = subs;
    }

    fn set_track_enabled(&mut self, i: usize, enabled: bool, cx: &mut Context<Self>) {
        let Some(st) = self.tracks.get_mut(i) else {
            return;
        };
        st.muted = !enabled;
        let st = *st;
        self.player.read(cx).set_track(st.idx, st.volume, st.muted);
        let path = self.clip.path.clone();
        self.library
            .update(cx, |l, cx| l.set_track_state(&path, st, cx));
        cx.notify();
    }

    fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let fav = !self.clip.favorite;
        self.clip.favorite = fav;
        let path = self.clip.path.clone();
        self.library
            .update(cx, |l, cx| l.set_favorite(&path, fav, cx));
        cx.notify();
    }

    fn remove_tag(&mut self, tag: String, cx: &mut Context<Self>) {
        self.clip.mtags.retain(|t| t != &tag);
        let path = self.clip.path.clone();
        self.library
            .update(cx, |l, cx| l.remove_tag(&path, &tag, cx));
        cx.notify();
    }

    /// Ctrl+click on the timeline: first click sets the start (and clears the
    /// end), the next sets the end; markers are re-ordered if crossed.
    fn ctrl_mark(&mut self, ms: u64, cx: &mut Context<Self>) {
        if self.next_mark_is_start {
            self.clip.mark_start = Some(ms);
            self.clip.mark_end = None;
            self.next_mark_is_start = false;
        } else {
            let start = self.clip.mark_start.unwrap_or(0);
            if ms >= start {
                self.clip.mark_end = Some(ms);
            } else {
                self.clip.mark_end = Some(start);
                self.clip.mark_start = Some(ms);
            }
            self.next_mark_is_start = true;
        }
        self.persist_marks(cx);
    }

    fn set_mark(&mut self, is_start: bool, ms: Option<u64>, cx: &mut Context<Self>) {
        if is_start {
            self.clip.mark_start = ms;
        } else {
            self.clip.mark_end = ms;
        }
        if let (Some(a), Some(b)) = (self.clip.mark_start, self.clip.mark_end)
            && a > b
        {
            self.clip.mark_start = Some(b);
            self.clip.mark_end = Some(a);
        }
        self.persist_marks(cx);
    }

    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        self.clip.mark_start = None;
        self.clip.mark_end = None;
        self.next_mark_is_start = true;
        self.persist_marks(cx);
    }

    fn persist_marks(&mut self, cx: &mut Context<Self>) {
        let path = self.clip.path.clone();
        let (start, end) = (self.clip.mark_start, self.clip.mark_end);
        self.library
            .update(cx, |l, cx| l.set_marks(&path, start, end, cx));
        self.sync_fields = true;
        cx.notify();
    }

    fn app_dispatch(&self, action: Action, window: &mut Window, cx: &mut App) {
        let app = self.app.clone();
        window.defer(cx, move |window, cx| {
            let _ = app.update(cx, |a, cx| a.dispatch(action, window, cx));
        });
    }
}

impl Render for Fullscreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_tag {
            self.clear_tag = false;
            self.tag_input
                .update(cx, |s, cx| s.set_value("", window, cx));
        }
        if self.sync_fields {
            self.sync_fields = false;
            let title = self.clip.title_raw().unwrap_or_default().to_string();
            self.title_input
                .update(cx, |s, cx| s.set_value(title, window, cx));
            let game = self.clip.game().unwrap_or_default().to_string();
            self.game_input
                .update(cx, |s, cx| s.set_value(game, window, cx));
            let start = self.clip.mark_start.map(fmt_time).unwrap_or_default();
            self.mark_start_input
                .update(cx, |s, cx| s.set_value(start, window, cx));
            let end = self.clip.mark_end.map(fmt_time).unwrap_or_default();
            self.mark_end_input
                .update(cx, |s, cx| s.set_value(end, window, cx));
        }

        let position = self.player.read(cx).position_ms();
        let duration = self.duration(cx);
        let playing = self.player.read(cx).playing();

        let video = div()
            .flex_1()
            .min_h(px(0.))
            .bg(rgb(0x000000))
            .child(self.player.clone());

        let transport = self.render_transport(position, duration, playing, cx);
        let panel = self.render_panel(cx);

        h_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(video)
                    .child(transport),
            )
            .child(panel)
    }
}

impl Fullscreen {
    fn render_transport(
        &mut self,
        position: u64,
        duration: u64,
        playing: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let play_icon = if playing {
            IconName::Pause
        } else {
            IconName::Play
        };

        let weak = cx.entity().downgrade();
        let tracks: Vec<(String, bool, Option<Entity<SliderState>>)> = self
            .clip
            .probe
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let enabled = !self.tracks.get(i).map(|s| s.muted).unwrap_or(false);
                (t.label.clone(), enabled, self.volumes.get(i).cloned())
            })
            .collect();
        let audio = Popover::new("audio-mixer")
            .trigger(
                Button::new("audio")
                    .ghost()
                    .icon(IconName::Settings2)
                    .tooltip("Audio tracks"),
            )
            .content(move |_, _, _| audio_mixer_content(&weak, &tracks));

        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("prev")
                    .ghost()
                    .icon(IconName::ChevronLeft)
                    .tooltip("Previous")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.app_dispatch(Action::PrevClip, window, cx)
                    })),
            )
            .child(
                Button::new("play")
                    .ghost()
                    .icon(play_icon)
                    .on_click(cx.listener(|this, _, _w, cx| this.toggle_play(cx))),
            )
            .child(
                Button::new("next")
                    .ghost()
                    .icon(IconName::ChevronRight)
                    .tooltip("Next")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.app_dispatch(Action::NextClip, window, cx)
                    })),
            )
            .child(div().text_sm().child(fmt_time(position)))
            .child(self.render_timeline(position, duration, cx))
            .child(div().text_sm().child(fmt_time(duration)))
            .child(audio)
            .child(
                Button::new("mute")
                    .ghost()
                    .label(if self.master_muted { "Unmute" } else { "Mute" })
                    .on_click(cx.listener(|this, _, _w, cx| this.toggle_mute(cx))),
            )
            .child(
                Button::new("fav")
                    .ghost()
                    .icon(if self.clip.favorite {
                        IconName::Heart
                    } else {
                        IconName::HeartOff
                    })
                    .tooltip("Favorite")
                    .on_click(cx.listener(|this, _, _w, cx| this.toggle_favorite(cx))),
            )
            .into_any_element()
    }

    fn duration(&self, cx: &App) -> u64 {
        let d = self.player.read(cx).duration_ms();
        if d > 0 {
            d
        } else {
            self.clip.probe.duration_ms
        }
    }

    fn timeline_ms(&self, x: Pixels, cx: &App) -> Option<u64> {
        let b = self.timeline_bounds.get();
        let w = f32::from(b.size.width);
        if w <= 0.0 {
            return None;
        }
        let frac = (f32::from(x - b.left()) / w).clamp(0.0, 1.0);
        Some((frac as f64 * self.duration(cx) as f64) as u64)
    }

    fn render_timeline(&self, position: u64, duration: u64, cx: &mut Context<Self>) -> AnyElement {
        let dur = duration.max(1);
        let frac = |ms: u64| (ms as f32 / dur as f32).clamp(0.0, 1.0);
        let pos = frac(position);
        let start_frac = self.clip.mark_start.map(frac);
        let end_frac = self.clip.mark_end.map(frac);
        let accent = cx.theme().accent;
        let bounds_cell = self.timeline_bounds.clone();

        let line = move |at: f32, color: Hsla| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(at))
                .w(px(2.))
                .bg(color)
        };

        let track = div()
            .relative()
            .w_full()
            .h(px(6.))
            .rounded_full()
            .bg(cx.theme().secondary)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(pos))
                    .rounded_full()
                    .bg(cx.theme().primary),
            )
            .when_some(
                start_frac.zip(end_frac).filter(|(a, b)| b > a),
                |el, (a, b)| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(a))
                            .w(relative(b - a))
                            .rounded_full()
                            .bg(accent.opacity(0.45)),
                    )
                },
            );

        div()
            .id("timeline")
            .flex_1()
            .h_6()
            .relative()
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    let Some(ms) = this.timeline_ms(ev.position.x, cx) else {
                        return;
                    };
                    if ev.modifiers.control {
                        this.ctrl_mark(ms, cx);
                    } else {
                        this.dragging = true;
                        this.player.update(cx, |p, cx| p.user_seek(ms, cx));
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                if this.dragging
                    && ev.pressed_button == Some(MouseButton::Left)
                    && let Some(ms) = this.timeline_ms(ev.position.x, cx)
                {
                    this.player.update(cx, |p, cx| p.user_seek(ms, cx));
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.dragging = false;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _window, _cx| {
                    this.dragging = false;
                }),
            )
            .child(track)
            .when_some(start_frac, |el, a| el.child(line(a, accent)))
            .when_some(end_frac, |el, b| el.child(line(b, accent)))
            .child(line(pos, cx.theme().foreground))
            .on_prepaint(move |b, _, _| bounds_cell.set(b))
            .into_any_element()
    }

    fn render_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let clip = &self.clip;

        let mtags = h_flex().w_full().flex_wrap().gap_1().children(
            clip.mtags
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, tag)| {
                    let tag_for_click = tag.clone();
                    h_flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py(px(2.))
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().secondary)
                        .text_xs()
                        .child(tag.clone())
                        .child(
                            Button::new(("rm-tag", i))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _, _w, cx| {
                                    this.remove_tag(tag_for_click.clone(), cx)
                                })),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
        );

        let stags = h_flex().w_full().flex_wrap().gap_1().children(
            clip.stags
                .iter()
                .map(|tag| {
                    h_flex()
                        .items_center()
                        .px_2()
                        .py(px(2.))
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tag.clone())
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
        );
        let has_stags = !clip.stags.is_empty();

        let meta = v_flex().w_full().gap_1().children(
            clip.meta
                .iter()
                .map(|(k, v)| {
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(k.clone()),
                        )
                        .child(div().truncate().child(v.clone()))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
        );

        v_flex()
            .id("fs-panel")
            .w(px(320.))
            .h_full()
            .p_3()
            .gap_3()
            .border_l_1()
            .border_color(cx.theme().border)
            .overflow_y_scroll()
            .child(div().text_lg().font_semibold().child(clip.title()))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}×{} · {} · {}",
                        clip.probe.width,
                        clip.probe.height,
                        fmt_time(clip.probe.duration_ms),
                        if clip.probe.vcodec.is_empty() {
                            "unknown".into()
                        } else {
                            clip.probe.vcodec.clone()
                        }
                    )),
            )
            .child(section_title("Title", cx))
            .child(Input::new(&self.title_input).small())
            .child(section_title("Game", cx))
            .child(Input::new(&self.game_input).small())
            .child(section_title("Highlight", cx))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Ctrl+click the timeline to set start, then end."),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.mark_start_input).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.mark_end_input).small()),
                    )
                    .child(
                        Button::new("clear-marks")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Clear highlight")
                            .on_click(cx.listener(|this, _, _w, cx| this.clear_marks(cx))),
                    ),
            )
            .child(section_title("Tags", cx))
            .child(mtags)
            .child(Input::new(&self.tag_input).small())
            .when(has_stags, |el| {
                el.child(section_title("Script tags", cx)).child(stags)
            })
            .child(section_title("Metadata", cx))
            .child(meta)
            .into_any_element()
    }
}

fn audio_mixer_content(
    weak: &WeakEntity<Fullscreen>,
    tracks: &[(String, bool, Option<Entity<SliderState>>)],
) -> AnyElement {
    let rows = tracks
        .iter()
        .enumerate()
        .map(|(i, (label, enabled, slider))| {
            let enabled = *enabled;
            let weak = weak.clone();
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    Checkbox::new(("track-enabled", i))
                        .checked(enabled)
                        .on_click(move |checked, _window, cx| {
                            let checked = *checked;
                            let _ =
                                weak.update(cx, |this, cx| this.set_track_enabled(i, checked, cx));
                        }),
                )
                .child(div().w(px(96.)).truncate().text_sm().child(label.clone()))
                .when_some(slider.clone(), |el, s| {
                    el.child(div().flex_1().child(Slider::new(&s).disabled(!enabled)))
                })
                .into_any_element()
        })
        .collect::<Vec<_>>();
    v_flex()
        .w(px(300.))
        .gap_2()
        .children(rows)
        .into_any_element()
}

fn section_title(label: &str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(label.to_string())
}

fn build_media(
    clip: &Clip,
    cx: &mut Context<Fullscreen>,
) -> (Entity<Player>, Vec<Entity<SliderState>>, Vec<TrackState>) {
    let uri = media::path_to_uri(&clip.path).unwrap_or_default();
    let states: Vec<TrackState> = clip
        .probe
        .tracks
        .iter()
        .map(|t| clip.state_for(t.idx))
        .collect();
    let states_for_player = states.clone();
    let start_ms = clip.mark_start.unwrap_or(0);
    let stop_ms = clip.marks().map(|(_, end)| end);
    let player = cx.new(|cx| {
        Player::new(
            &uri,
            PlayerOptions {
                muted: false,
                looping: false,
                preview_width: None,
                start_ms,
                stop_ms,
            },
            states_for_player,
            cx,
        )
    });

    let volumes = states
        .iter()
        .map(|st| {
            let v = st.volume as f32;
            cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(4.0)
                    .step(0.05)
                    .default_value(v)
            })
        })
        .collect();

    (player, volumes, states)
}

fn fmt_time(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Parse "m:ss", "h:mm:ss", or plain seconds into milliseconds. Empty -> None.
fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut secs = 0f64;
    for part in s.split(':') {
        secs = secs * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some((secs * 1000.0) as u64)
}
