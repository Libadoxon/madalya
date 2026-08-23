use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gpui::{App, WeakEntity};
use walkdir::WalkDir;

use super::Library;
use super::model::Clip;
use super::store::Store;
use crate::media;
use crate::script::{ClipInput, ScriptEngine};

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "avi", "m4v", "flv", "wmv", "ts", "mpg", "mpeg",
];

/// How many clips are probed/thumbnailed/scripted concurrently.
const CONCURRENCY: usize = 4;

/// Reconcile the store with the clip home: drop entries whose files vanished,
/// (re)process new or changed files (script re-runs when mtime/size differ), and
/// stream results into the `Library` entity as they land.
///
/// All disk/DB/GStreamer/Rhai work runs on the background executor; the main
/// thread only applies cheap in-memory updates to the entity.
pub fn run(
    library: WeakEntity<Library>,
    clips_dir: PathBuf,
    script_path: Option<PathBuf>,
    store: Store,
    thumb_px: u32,
    cx: &mut App,
) {
    let executor = cx.background_executor().clone();
    let thumbs_dir = super::thumbs_dir();

    cx.spawn(async move |cx| {
        let _ = library.update(cx, |l, cx| l.set_scanning(true, cx));

        // Diff disk against the store — off the main thread.
        let plan = {
            let store = store.clone();
            let clips_dir = clips_dir.clone();
            executor
                .spawn(async move {
                    let disk = walk(&clips_dir);
                    let existing = store.fingerprints().unwrap_or_default();
                    let removed: Vec<PathBuf> = existing
                        .keys()
                        .filter(|p| !disk.contains_key(*p))
                        .cloned()
                        .collect();
                    let todo: Vec<(PathBuf, (i64, u64))> = disk
                        .into_iter()
                        .filter(|(p, fp)| existing.get(p) != Some(fp))
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
                .spawn(async move { load_engine(script_path, store) })
                .await
        };

        for chunk in todo.chunks(CONCURRENCY) {
            let tasks: Vec<_> = chunk
                .iter()
                .cloned()
                .map(|(path, fp)| {
                    let store = store.clone();
                    let engine = engine.clone();
                    let thumbs_dir = thumbs_dir.clone();
                    executor.spawn(async move {
                        let clip = build_clip(&path, fp, thumb_px, engine.as_deref(), &thumbs_dir)?;
                        store.upsert_clip(&clip)?;
                        Ok::<Clip, anyhow::Error>(
                            store.load_clip(&clip.path).ok().flatten().unwrap_or(clip),
                        )
                    })
                })
                .collect();

            for task in tasks {
                match task.await {
                    Ok(clip) => {
                        let _ = library.update(cx, |l, cx| l.apply_upserted(clip, cx));
                    }
                    Err(e) => tracing::warn!("skipping clip: {e:#}"),
                }
            }
        }

        let _ = library.update(cx, |l, cx| l.set_scanning(false, cx));
    })
    .detach();
}

fn load_engine(script_path: Option<PathBuf>, store: Store) -> Option<Arc<ScriptEngine>> {
    let path = script_path?;
    match ScriptEngine::load(&path, store) {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::error!("metadata script failed to load: {e:#}");
            None
        }
    }
}

fn build_clip(
    path: &Path,
    (mtime, size): (i64, u64),
    thumb_px: u32,
    engine: Option<&ScriptEngine>,
    thumbs_dir: &Path,
) -> Result<Clip> {
    let uri = media::path_to_uri(path)?;
    let probe = media::probe::probe(&uri)?;

    let thumb = thumbs_dir.join(format!("{}.jpg", fingerprint_hash(path, mtime)));
    let thumb_path = match media::thumbnail::generate(&uri, &thumb, thumb_px) {
        Ok(()) => Some(thumb),
        Err(e) => {
            tracing::warn!("thumbnail failed for {path:?}: {e:#}");
            None
        }
    };

    let (meta, tags) = match engine {
        Some(e) => match e.run(ClipInput {
            path,
            mtime,
            size,
            probe: &probe,
        }) {
            Ok(r) => (r.meta, r.tags),
            Err(err) => {
                tracing::warn!("script error for {path:?}: {err:#}");
                (Vec::new(), Vec::new())
            }
        },
        None => (Vec::new(), Vec::new()),
    };

    Ok(Clip {
        path: path.to_path_buf(),
        mtime,
        size,
        probe,
        favorite: false,
        thumb_path,
        tags,
        meta,
        track_state: Vec::new(),
        added_at: now_secs(),
    })
}

fn walk(dir: &Path) -> HashMap<PathBuf, (i64, u64)> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
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
