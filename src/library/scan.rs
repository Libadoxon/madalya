use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gpui_kit::{App, WeakEntity};
use walkdir::WalkDir;

use super::Library;
use super::model::{Clip, TrackState};
use super::store::Store;
use crate::media;
use crate::script::{ClipInput, MdataScript, MixScript};

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "avi", "m4v", "flv", "wmv", "ts", "mpg", "mpeg",
];

const SCRIPT_HASH_KEY: &str = "script_hash";

/// How many clips are probed/thumbnailed/scripted concurrently.
fn concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Reconcile the store with the clip home: drop entries whose files vanished,
/// (re)process new or changed files, and stream results into the `Library`
/// entity as they land. The metadata script is re-run on every clip when
/// `force` is set or the script file changed since the last scan.
#[allow(clippy::too_many_arguments)]
pub fn run(
    library: WeakEntity<Library>,
    clips_dir: PathBuf,
    mdata_script_path: Option<PathBuf>,
    mix_script_path: Option<PathBuf>,
    store: Store,
    thumb_px: u32,
    max_depth: u32,
    force: bool,
    cx: &mut App,
) {
    let executor = cx.background_executor().clone();
    let thumbs_dir = super::thumbs_dir();

    cx.spawn(async move |cx| {
        let _ = library.update(cx, |l, cx| l.set_scanning(true, cx));

        let script_hash = format!(
            "{}-{}",
            script_fingerprint(mdata_script_path.as_deref()),
            script_fingerprint(mix_script_path.as_deref())
        );
        let script_changed = {
            let store = store.clone();
            let hash = script_hash.clone();
            executor
                .spawn(async move {
                    store.kv_get(SCRIPT_HASH_KEY).ok().flatten().as_deref() != Some(hash.as_str())
                })
                .await
        };
        let force = force || script_changed;

        // Diff disk against the store
        let plan = {
            let store = store.clone();
            let clips_dir = clips_dir.clone();
            executor
                .spawn(async move {
                    let disk = walk(&clips_dir, max_depth);
                    let existing = store.fingerprints().unwrap_or_default();
                    let removed: Vec<PathBuf> = existing
                        .keys()
                        .filter(|p| !disk.contains_key(*p))
                        .cloned()
                        .collect();
                    let todo: Vec<(PathBuf, (i64, u64))> = disk
                        .into_iter()
                        .filter(|(p, fp)| force || existing.get(p) != Some(fp))
                        .collect();
                    (removed, todo)
                })
                .await
        };
        let (removed, todo) = plan;

        for path in removed {
            let store = store.clone();
            let p = path.clone();
            executor
                .spawn(async move { store.delete_clip(&p) })
                .await
                .ok();
            let _ = library.update(cx, |l, cx| l.apply_removed(&path, cx));
        }

        let engine = {
            let store = store.clone();
            executor
                .spawn(async move { load_mdata_engine(mdata_script_path, store) })
                .await
        };
        let mix_engine = {
            let store = store.clone();
            executor
                .spawn(async move { load_mix_engine(mix_script_path, store) })
                .await
        };

        let (jobs_tx, jobs_rx) = smol::channel::unbounded::<(PathBuf, (i64, u64))>();
        let (results_tx, results_rx) = smol::channel::unbounded::<Clip>();
        for job in todo {
            let _ = jobs_tx.send(job).await;
        }
        jobs_tx.close();

        for _ in 0..concurrency() {
            let jobs_rx = jobs_rx.clone();
            let results_tx = results_tx.clone();
            let store = store.clone();
            let engine = engine.clone();
            let mix_engine = mix_engine.clone();
            let thumbs_dir = thumbs_dir.clone();
            executor
                .spawn(async move {
                    while let Ok((path, fp)) = jobs_rx.recv().await {
                        match build_clip(
                            &path,
                            fp,
                            thumb_px,
                            engine.as_deref(),
                            mix_engine.as_deref(),
                            &thumbs_dir,
                        )
                        .and_then(|clip| {
                            store.upsert_clip(&clip)?;
                            Ok(store.load_clip(&clip.path).ok().flatten().unwrap_or(clip))
                        }) {
                            Ok(clip) => {
                                let _ = results_tx.send(clip).await;
                            }
                            Err(e) => tracing::warn!("skipping {path:?}: {e:#}"),
                        }
                    }
                })
                .detach();
        }
        drop(results_tx);
        drop(jobs_rx);

        while let Ok(clip) = results_rx.recv().await {
            let _ = library.update(cx, |l, cx| l.apply_upserted(clip, cx));
        }

        {
            let store = store.clone();
            executor
                .spawn(async move { store.kv_set(SCRIPT_HASH_KEY, &script_hash) })
                .await
                .ok();
        }

        let _ = library.update(cx, |l, cx| l.set_scanning(false, cx));

        pregenerate_audio(&store, &executor).await;
    })
    .detach();
}

async fn pregenerate_audio(store: &Store, executor: &gpui_kit::BackgroundExecutor) {
    let clips = {
        let store = store.clone();
        executor
            .spawn(async move { store.load_all().unwrap_or_default() })
            .await
    };

    let (tx, rx) = smol::channel::unbounded::<(String, Vec<TrackState>, PathBuf)>();
    for clip in clips {
        if clip.probe.tracks.is_empty() {
            continue;
        }
        let states: Vec<TrackState> = clip
            .probe
            .tracks
            .iter()
            .map(|t| clip.state_for(t.idx))
            .collect();
        let dest = media::mix::cache_path(&clip.path, clip.mtime, &states);
        if dest.exists() {
            continue;
        }
        if let Ok(uri) = media::path_to_uri(&clip.path) {
            let _ = tx.send((uri, states, dest)).await;
        }
    }
    tx.close();

    let mut workers = Vec::new();
    for _ in 0..concurrency() {
        let rx = rx.clone();
        workers.push(executor.spawn(async move {
            while let Ok((uri, states, dest)) = rx.recv().await {
                if let Err(e) = media::mix::render_mix(&uri, &states, &dest) {
                    tracing::warn!("audio pre-render failed for {dest:?}: {e:#}");
                }
            }
        }));
    }
    for w in workers {
        w.await;
    }
}

fn script_fingerprint(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "none".into();
    };
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            format!("{:016x}", h.finish())
        }
        Err(_) => "unreadable".into(),
    }
}

fn load_mdata_engine(script_path: Option<PathBuf>, store: Store) -> Option<Arc<MdataScript>> {
    let path = script_path?;
    match MdataScript::load(&path, store) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::error!("metadata script failed to load: {e:#}");
            None
        }
    }
}

fn load_mix_engine(script_path: Option<PathBuf>, store: Store) -> Option<Arc<MixScript>> {
    let path = script_path?;
    match MixScript::load(&path, store) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::error!("mix script failed to load: {e:#}");
            None
        }
    }
}

fn build_clip(
    path: &Path,
    (mtime, size): (i64, u64),
    thumb_px: u32,
    engine: Option<&MdataScript>,
    mix_engine: Option<&MixScript>,
    thumbs_dir: &Path,
) -> Result<Clip> {
    let uri = media::path_to_uri(path)?;
    let probe = media::probe::probe(&uri)?;

    let thumb = thumbs_dir.join(format!("{}.jpg", fingerprint_hash(path, mtime)));
    let thumb_path = if thumb.exists() {
        Some(thumb)
    } else {
        match media::thumbnail::generate(&uri, &thumb, thumb_px) {
            Ok(()) => Some(thumb),
            Err(e) => {
                tracing::warn!("thumbnail failed for {path:?}: {e:#}");
                None
            }
        }
    };

    let (title, game, meta, stags) = match engine {
        Some(e) => match e.run(ClipInput {
            path,
            mtime,
            size,
            probe: &probe,
        }) {
            Ok(r) => (r.title, r.game, r.meta, r.tags),
            Err(err) => {
                tracing::warn!("script error for {path:?}: {err:#}");
                (None, None, Vec::new(), Vec::new())
            }
        },
        None => (None, None, Vec::new(), Vec::new()),
    };

    let default_tracks = default_tracks(&probe, mix_engine, path, mtime, size);

    Ok(Clip {
        path: path.to_path_buf(),
        mtime,
        size,
        probe,
        favorite: false,
        thumb_path,
        title,
        game,
        mark_start: None,
        mark_end: None,
        stags,
        mtags: Vec::new(),
        meta,
        track_state: Vec::new(),
        default_tracks,
        added_at: now_secs(),
    })
}

fn default_tracks(
    probe: &crate::library::model::ClipProbe,
    mix_engine: Option<&MixScript>,
    path: &Path,
    mtime: i64,
    size: u64,
) -> Vec<TrackState> {
    if probe.tracks.is_empty() {
        return Vec::new();
    }
    let choices = mix_engine.and_then(|e| {
        e.run(ClipInput {
            path,
            mtime,
            size,
            probe,
        })
        .map_err(|err| tracing::warn!("mix script error for {path:?}: {err:#}"))
        .ok()
    });
    let choices = choices.unwrap_or_else(|| {
        vec![crate::script::mix::TrackChoice {
            idx: 0,
            volume: 1.0,
        }]
    });
    probe
        .tracks
        .iter()
        .map(|t| match choices.iter().find(|c| c.idx == t.idx) {
            Some(c) => TrackState {
                idx: t.idx,
                volume: c.volume,
                muted: false,
            },
            None => TrackState {
                idx: t.idx,
                volume: 1.0,
                muted: true,
            },
        })
        .collect()
}

fn walk(dir: &Path, max_depth: u32) -> HashMap<PathBuf, (i64, u64)> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(dir)
        .max_depth(max_depth.max(1) as usize)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() || !is_video(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(entry.path().to_path_buf(), (mtime, meta.len()));
    }
    out
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn fingerprint_hash(path: &Path, mtime: i64) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    mtime.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{is_video, walk};

    #[test]
    fn walk_respects_max_depth() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("madalya-walk-{n}"));
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.mp4"), b"").unwrap();
        std::fs::write(root.join("sub/b.mp4"), b"").unwrap();
        std::fs::write(root.join("sub/deep/c.mp4"), b"").unwrap();
        std::fs::write(root.join("notes.txt"), b"").unwrap();

        assert_eq!(walk(&root, 1).len(), 1); // a.mp4 only
        assert_eq!(walk(&root, 2).len(), 2); // + sub/b.mp4
        assert_eq!(walk(&root, 8).len(), 3); // + sub/deep/c.mp4, .txt ignored

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_video_extensions() {
        assert!(is_video(std::path::Path::new("x.MKV")));
        assert!(is_video(std::path::Path::new("x.mp4")));
        assert!(!is_video(std::path::Path::new("x.txt")));
        assert!(!is_video(std::path::Path::new("x")));
    }
}
