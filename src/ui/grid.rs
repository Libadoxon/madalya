use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Icon, IconName,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::app::AppView;
use crate::config::Config;
use crate::library::model::Clip;
use crate::media::player::Player;

pub fn render_grid(
    app: &mut AppView,
    _window: &mut Window,
    cx: &mut Context<AppView>,
) -> AnyElement {
    let lib_cfg = cx.global::<Config>().library.clone();
    let library = app.library.read(cx);
    let clips: Vec<Clip> = library.clips().to_vec();
    let scanning = library.scanning();

    if lib_cfg.clips_dir.is_none() {
        return empty_state(
            "No clips directory set",
            "Choose a folder of clips in Settings to get started.",
            true,
            cx,
        );
    }
    if clips.is_empty() {
        let msg = if scanning {
            "Scanning for clips…"
        } else {
            "No video clips found in the configured directory."
        };
        return empty_state("Library empty", msg, false, cx);
    }

    let selected = app.selected;
    let thumb_px = lib_cfg.thumb_px as f32;
    let preview_on_hover = lib_cfg.preview_on_hover;

    let tiles: Vec<AnyElement> = clips
        .iter()
        .enumerate()
        .map(|(idx, clip)| {
            let preview = preview_on_hover
                .then(|| app.preview_for(&clip.path))
                .flatten();
            tile(
                idx,
                clip,
                idx == selected,
                thumb_px,
                preview_on_hover,
                preview,
                cx,
            )
        })
        .collect();

    v_flex()
        .id("grid-scroll")
        .size_full()
        .overflow_y_scroll()
        .child(h_flex().flex_wrap().gap_3().p_3().children(tiles))
        .into_any_element()
}

fn tile(
    idx: usize,
    clip: &Clip,
    selected: bool,
    thumb_px: f32,
    preview_on_hover: bool,
    preview: Option<Entity<Player>>,
    cx: &mut Context<AppView>,
) -> AnyElement {
    let thumb_h = thumb_px * 9.0 / 16.0;
    let path_hover = clip.path.clone();

    let media: AnyElement = if let Some(player) = preview {
        player.into_any_element()
    } else if let Some(thumb) = &clip.thumb_path {
        img(thumb.clone())
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Play).text_color(cx.theme().muted_foreground))
            .into_any_element()
    };

    v_flex()
        .id(("clip-tile", idx))
        .w(px(thumb_px))
        .gap_1()
        .p_1()
        .rounded(cx.theme().radius)
        .border_2()
        .border_color(if selected {
            cx.theme().primary
        } else {
            cx.theme().border.opacity(0.0)
        })
        .cursor_pointer()
        .child(
            div()
                .w_full()
                .h(px(thumb_h))
                .rounded(cx.theme().radius)
                .overflow_hidden()
                .bg(cx.theme().muted)
                .relative()
                .child(media)
                .when(clip.favorite, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_1()
                            .right_1()
                            .child(Icon::new(IconName::Heart).text_color(cx.theme().primary)),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .bottom_1()
                        .right_1()
                        .px_1()
                        .rounded_sm()
                        .bg(cx.theme().background.opacity(0.7))
                        .text_xs()
                        .child(fmt_duration(clip.probe.duration_ms)),
                ),
        )
        .child(div().w_full().truncate().text_sm().child(clip.title()))
        .on_click(cx.listener(move |this, _, window, cx| this.open_fullscreen(idx, window, cx)))
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            if *hovered {
                if preview_on_hover {
                    this.start_preview(path_hover.clone(), cx);
                }
                this.select(idx, cx);
            } else {
                this.stop_preview(window, cx);
            }
        }))
        .into_any_element()
}

fn empty_state(
    title: &str,
    msg: &str,
    show_settings: bool,
    cx: &mut Context<AppView>,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(div().text_lg().child(title.to_string()))
        .child(div().child(msg.to_string()))
        .when(show_settings, |el| {
            el.child(
                Button::new("open-settings-empty")
                    .primary()
                    .label("Open Settings")
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_settings(cx))),
            )
        })
        .into_any_element()
}

fn fmt_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
