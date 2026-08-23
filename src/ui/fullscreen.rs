use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
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
    scrubber: Entity<SliderState>,
    dragging: bool,
    volumes: Vec<Entity<SliderState>>,
    tracks: Vec<TrackState>,
    master_muted: bool,
    tag_input: Entity<InputState>,
    clear_tag: bool,
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
        let (player, scrubber, volumes, tracks) = build_media(&clip, cx);
        let mut this = Self {
            library,
            app,
            clip,
            player,
            scrubber,
            dragging: false,
            volumes,
            tracks,
            master_muted: false,
            tag_input,
            clear_tag: false,
            _subs: Vec::new(),
        };
        this.wire(cx);
        this
    }

    pub fn load(&mut self, clip: Clip, _window: &mut Window, cx: &mut Context<Self>) {
        let (player, scrubber, volumes, tracks) = build_media(&clip, cx);
        self.clip = clip;
        self.player = player;
        self.scrubber = scrubber;
        self.volumes = volumes;
        self.tracks = tracks;
        self.dragging = false;
        self.master_muted = false;
        self._subs.clear();
        self.clear_tag = true;
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

        subs.push(
            cx.subscribe(&self.scrubber, |this, slider, ev, cx| match ev {
                SliderEvent::Change(_) => this.dragging = true,
                SliderEvent::Release(_) => {
                    let secs = slider.read(cx).value().start();
                    this.player
                        .update(cx, |p, cx| p.seek_ms((secs * 1000.0) as u64, cx));
                    this.dragging = false;
                }
            }),
        );

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
            cx.subscribe(&self.tag_input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    let tag = input.read(cx).value().trim().to_string();
                    if !tag.is_empty() {
                        let path = this.clip.path.clone();
                        this.library.update(cx, |l, cx| l.add_tag(&path, &tag, cx));
                        if !this.clip.tags.iter().any(|t| t == &tag) {
                            this.clip.tags.push(tag);
                            this.clip.tags.sort();
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
        self.clip.tags.retain(|t| t != &tag);
        let path = self.clip.path.clone();
        self.library
            .update(cx, |l, cx| l.remove_tag(&path, &tag, cx));
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

        let position = self.player.read(cx).position_ms();
        let duration = self
            .player
            .read(cx)
            .duration_ms()
            .max(self.clip.probe.duration_ms);
        let playing = self.player.read(cx).playing();

        if !self.dragging {
            self.scrubber.update(cx, |s, cx| {
                s.set_value(position as f32 / 1000.0, window, cx)
            });
        }

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
            .child(div().flex_1().child(Slider::new(&self.scrubber)))
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

    fn render_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let clip = &self.clip;

        let tags = h_flex().w_full().flex_wrap().gap_1().children(
            clip.tags
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

        let meta = v_flex().w_full().gap_1().children(
            clip.meta
                .iter()
                .filter(|(k, _)| k != "title")
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
            .child(section_title("Tags", cx))
            .child(tags)
            .child(Input::new(&self.tag_input).small())
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
) -> (
    Entity<Player>,
    Entity<SliderState>,
    Vec<Entity<SliderState>>,
    Vec<TrackState>,
) {
    let uri = media::path_to_uri(&clip.path).unwrap_or_default();
    let states: Vec<TrackState> = clip
        .probe
        .tracks
        .iter()
        .map(|t| clip.state_for(t.idx))
        .collect();
    let states_for_player = states.clone();
    let player = cx.new(|cx| {
        Player::new(
            &uri,
            PlayerOptions {
                muted: false,
                looping: false,
                preview_width: None,
            },
            states_for_player,
            cx,
        )
    });

    let dur = (clip.probe.duration_ms as f32 / 1000.0).max(1.0);
    let scrubber = cx.new(|_| {
        SliderState::new()
            .min(0.0)
            .max(dur)
            .step(0.1)
            .default_value(0.0)
    });

    let volumes = states
        .iter()
        .map(|st| {
            let v = st.volume as f32;
            cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(1.5)
                    .step(0.01)
                    .default_value(v)
            })
        })
        .collect();

    (player, scrubber, volumes, states)
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
