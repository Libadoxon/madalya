use std::collections::BTreeSet;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    calendar::Date,
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    h_flex,
    input::InputState,
    v_flex,
};
use gpui_kit::*;

use crate::library::model::Clip;

#[derive(Clone, Default)]
pub struct Filters {
    pub query: String,
    pub game: Option<String>,
    pub include: BTreeSet<String>,
    pub exclude: BTreeSet<String>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
}

impl Filters {
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
            || self.game.is_some()
            || !self.include.is_empty()
            || !self.exclude.is_empty()
            || self.date_from.is_some()
            || self.date_to.is_some()
    }

    pub fn matches(&self, clip: &Clip) -> bool {
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() && !clip.title().to_lowercase().contains(&q) {
            return false;
        }
        if let Some(game) = &self.game
            && clip.game() != Some(game.as_str())
        {
            return false;
        }
        if let Some(from) = self.date_from
            && clip.mtime < from
        {
            return false;
        }
        if let Some(to) = self.date_to
            && clip.mtime > to
        {
            return false;
        }
        if !self.include.is_empty() || !self.exclude.is_empty() {
            let tags: BTreeSet<&str> = clip
                .stags
                .iter()
                .chain(clip.mtags.iter())
                .map(String::as_str)
                .collect();
            if !self.include.iter().all(|t| tags.contains(t.as_str())) {
                return false;
            }
            if self.exclude.iter().any(|t| tags.contains(t.as_str())) {
                return false;
            }
        }
        true
    }
}

pub enum FilterEvent {
    Changed,
}

pub struct FilterState {
    pub search: Entity<InputState>,
    date: Entity<DatePickerState>,
    filters: Filters,
    _subs: Vec<Subscription>,
}

impl EventEmitter<FilterEvent> for FilterState {}

impl FilterState {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search title…"));
        let date = cx.new(|cx| DatePickerState::range(window, cx));

        let subs = vec![
            cx.subscribe(
                &search,
                |this, input, ev: &gpui_kit::component::input::InputEvent, cx| {
                    if matches!(ev, gpui_kit::component::input::InputEvent::Change) {
                        this.filters.query = input.read(cx).value().to_string();
                        this.changed(cx);
                    }
                },
            ),
            cx.subscribe(&date, |this, _picker, ev: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(date) = ev;
                this.filters.date_from = date
                    .start()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|dt| dt.and_utc().timestamp());
                this.filters.date_to = date
                    .end()
                    .and_then(|d| d.and_hms_opt(23, 59, 59))
                    .map(|dt| dt.and_utc().timestamp());
                this.changed(cx);
            }),
        ];

        Self {
            search,
            date,
            filters: Filters::default(),
            _subs: subs,
        }
    }

    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(FilterEvent::Changed);
        cx.notify();
    }

    fn cycle_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        if self.filters.include.remove(tag) {
            self.filters.exclude.insert(tag.to_string());
        } else if !self.filters.exclude.remove(tag) {
            self.filters.include.insert(tag.to_string());
        }
        self.changed(cx);
    }

    fn toggle_game(&mut self, game: &str, cx: &mut Context<Self>) {
        if self.filters.game.as_deref() == Some(game) {
            self.filters.game = None;
        } else {
            self.filters.game = Some(game.to_string());
        }
        self.changed(cx);
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filters = Filters::default();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.date
            .update(cx, |d, cx| d.set_date(Date::Range(None, None), window, cx));
        self.changed(cx);
    }
}

/// Popover body: whitelist/blacklist tag chips, a game picker, and a recording
/// date-range picker, all reading from and writing back to `filter`.
pub fn panel(filter: &Entity<FilterState>, clips: &[Clip], cx: &mut App) -> AnyElement {
    let f = filter.read(cx).filters().clone();
    let date = filter.read(cx).date.clone();

    let mut tags: BTreeSet<&str> = BTreeSet::new();
    let mut games: BTreeSet<&str> = BTreeSet::new();
    for c in clips {
        for t in c.stags.iter().chain(c.mtags.iter()) {
            tags.insert(t);
        }
        if let Some(g) = c.game() {
            games.insert(g);
        }
    }

    let tag_chips = tags.into_iter().map(|tag| {
        let (bg, fg) = if f.include.contains(tag) {
            (cx.theme().primary, cx.theme().primary_foreground)
        } else if f.exclude.contains(tag) {
            (cx.theme().danger, cx.theme().danger_foreground)
        } else {
            (cx.theme().secondary, cx.theme().secondary_foreground)
        };
        let filter = filter.clone();
        let tag_owned = tag.to_string();
        chip(tag, bg, fg).on_click(move |_, _window, cx| {
            filter.update(cx, |s, cx| s.cycle_tag(&tag_owned, cx));
        })
    });

    let game_chips = games.into_iter().map(|game| {
        let selected = f.game.as_deref() == Some(game);
        let (bg, fg) = if selected {
            (cx.theme().primary, cx.theme().primary_foreground)
        } else {
            (cx.theme().secondary, cx.theme().secondary_foreground)
        };
        let filter = filter.clone();
        let game_owned = game.to_string();
        chip(game, bg, fg).on_click(move |_, _window, cx| {
            filter.update(cx, |s, cx| s.toggle_game(&game_owned, cx));
        })
    });

    let filter_clear = filter.clone();
    v_flex()
        .w(px(320.))
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(section("Tags", cx))
                .child(
                    Button::new("clear-filters")
                        .ghost()
                        .xsmall()
                        .label("Clear")
                        .on_click(move |_, window, cx| {
                            filter_clear.update(cx, |s, cx| s.clear(window, cx));
                        }),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Click a tag: once to require it, twice to exclude it."),
        )
        .child(h_flex().flex_wrap().gap_1().children(tag_chips))
        .child(section("Game", cx))
        .child(h_flex().flex_wrap().gap_1().children(game_chips))
        .child(section("Recording date", cx))
        .child(
            DatePicker::new(&date)
                .small()
                .cleanable(true)
                .placeholder("Any date"),
        )
        .into_any_element()
}

fn chip(label: &str, bg: Hsla, fg: Hsla) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("chip-{label}")))
        .px_2()
        .py(px(2.))
        .rounded_md()
        .bg(bg)
        .text_xs()
        .text_color(fg)
        .cursor_pointer()
        .child(label.to_string())
}

fn section(label: &str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(label.to_string())
}
