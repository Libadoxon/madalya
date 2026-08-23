use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Clip {
    pub path: PathBuf,
    pub mtime: i64,
    pub size: u64,
    pub probe: ClipProbe,
    pub favorite: bool,
    pub thumb_path: Option<PathBuf>,
    pub tags: Vec<String>,
    pub meta: Vec<(String, String)>,
    pub track_state: Vec<TrackState>,
    pub added_at: i64,
}

impl Clip {
    pub fn title(&self) -> String {
        self.meta_get("title")
            .map(str::to_owned)
            .unwrap_or_else(|| {
                self.path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    }

    pub fn meta_get(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn state_for(&self, idx: u32) -> TrackState {
        self.track_state
            .iter()
            .find(|s| s.idx == idx)
            .copied()
            .unwrap_or(TrackState::default_for(idx))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClipProbe {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub vcodec: String,
    pub tracks: Vec<AudioTrack>,
    pub container_tags: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct AudioTrack {
    pub idx: u32,
    pub label: String,
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct TrackState {
    pub idx: u32,
    pub volume: f64,
    pub muted: bool,
}

impl TrackState {
    pub fn default_for(idx: u32) -> Self {
        Self {
            idx,
            volume: 1.0,
            muted: false,
        }
    }
}
