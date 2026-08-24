pub mod model;
pub mod scan;
pub mod store;

use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, Context, Entity};

use crate::config::Config;
use crate::meta::APP_ID;
use model::{Clip, TrackState};
use store::Store;

pub fn data_dir() -> PathBuf {
    dirs::data_dir().expect("no data dir").join(APP_ID)
}

pub fn db_path() -> PathBuf {
    data_dir().join("library.db")
}

pub fn thumbs_dir() -> PathBuf {
    data_dir().join("thumbs")
}

pub struct Library {
    store: Store,
    clips: Vec<Clip>,
    scanning: bool,
}

impl Library {
    pub fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let clips = store.load_all().unwrap_or_else(|e| {
            tracing::error!("failed to load library: {e:#}");
            Vec::new()
        });
        let this = Self {
            store,
            clips,
            scanning: false,
        };
        cx.notify();
        this
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn clip(&self, idx: usize) -> Option<&Clip> {
        self.clips.get(idx)
    }

    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// (Re)scan the configured clip home. No-op if no clip home is set. When
    /// `force` is set, the script is re-run on every clip regardless of changes.
    pub fn rescan(&mut self, force: bool, cx: &mut Context<Self>) {
        let cfg = cx.global::<Config>().library.clone();
        let Some(clips_dir) = cfg.clips_dir.clone() else {
            return;
        };
        scan::run(
            cx.weak_entity(),
            clips_dir,
            cfg.script_path.clone(),
            self.store.clone(),
            cfg.thumb_px.max(1),
            cfg.max_scan_depth,
            force,
            cx,
        );
    }

    pub fn set_favorite(&mut self, path: &Path, favorite: bool, cx: &mut Context<Self>) {
        let _ = self.store.set_favorite(path, favorite);
        if let Some(c) = self.clip_mut(path) {
            c.favorite = favorite;
            cx.notify();
        }
    }

    pub fn set_title(&mut self, path: &Path, title: Option<&str>, cx: &mut Context<Self>) {
        let _ = self.store.set_title(path, title);
        let normalized = title.map(str::trim).filter(|s| !s.is_empty());
        if let Some(c) = self.clip_mut(path) {
            c.title = normalized.map(str::to_owned);
            cx.notify();
        }
    }

    pub fn set_game(&mut self, path: &Path, game: Option<&str>, cx: &mut Context<Self>) {
        let _ = self.store.set_game(path, game);
        let normalized = game.map(str::trim).filter(|s| !s.is_empty());
        if let Some(c) = self.clip_mut(path) {
            c.game = normalized.map(str::to_owned);
            cx.notify();
        }
    }

    pub fn add_tag(&mut self, path: &Path, tag: &str, cx: &mut Context<Self>) {
        let _ = self.store.add_tag(path, tag);
        if let Some(c) = self.clip_mut(path)
            && !c.mtags.iter().any(|t| t == tag)
        {
            c.mtags.push(tag.to_string());
            c.mtags.sort();
            cx.notify();
        }
    }

    pub fn remove_tag(&mut self, path: &Path, tag: &str, cx: &mut Context<Self>) {
        let _ = self.store.remove_tag(path, tag);
        if let Some(c) = self.clip_mut(path) {
            c.mtags.retain(|t| t != tag);
            cx.notify();
        }
    }

    pub fn set_track_state(&mut self, path: &Path, st: TrackState, cx: &mut Context<Self>) {
        let _ = self.store.set_track_state(path, st);
        if let Some(c) = self.clip_mut(path) {
            match c.track_state.iter_mut().find(|s| s.idx == st.idx) {
                Some(existing) => *existing = st,
                None => c.track_state.push(st),
            }
            cx.notify();
        }
    }

    pub(crate) fn set_scanning(&mut self, scanning: bool, cx: &mut Context<Self>) {
        self.scanning = scanning;
        cx.notify();
    }

    pub(crate) fn apply_removed(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.clips.retain(|c| c.path != path);
        cx.notify();
    }

    pub(crate) fn apply_upserted(&mut self, clip: Clip, cx: &mut Context<Self>) {
        match self.clips.iter_mut().find(|c| c.path == clip.path) {
            Some(existing) => *existing = clip,
            None => self.clips.push(clip),
        }
        self.clips
            .sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
        cx.notify();
    }

    fn clip_mut(&mut self, path: &Path) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.path == path)
    }
}

pub fn open_store() -> anyhow::Result<Store> {
    Store::open(&db_path())
}

pub fn new_library(store: Store, cx: &mut App) -> Entity<Library> {
    cx.new(|cx| Library::new(store, cx))
}
