use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::model::{AudioTrack, Clip, ClipProbe, TrackState};

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    /// Path -> (mtime, size) for every stored clip, used to diff against disk.
    pub fn fingerprints(&self) -> Result<HashMap<PathBuf, (i64, u64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT path, mtime, size FROM clips")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)? as u64),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_clip(&self, path: &Path) -> Result<()> {
        self.lock()
            .execute("DELETE FROM clips WHERE path = ?1", params![path_str(path)])?;
        Ok(())
    }

    /// Insert or refresh a scanned clip. Preserves user-owned state: `favorite`,
    /// `added_at`, per-track mixer state, and manually-added tags. Replaces the
    /// script/probe-derived rows (meta, track listing, script tags).
    pub fn upsert_clip(&self, clip: &Clip) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let p = path_str(&clip.path);

        // Empty script output leaves the stored title/game untouched; a non-empty
        // one always overwrites (script and manual edits both flow through them).
        let title = clip.title.as_deref().filter(|s| !s.trim().is_empty());
        let game = clip.game.as_deref().filter(|s| !s.trim().is_empty());
        tx.execute(
            "INSERT INTO clips (path, mtime, size, duration_ms, width, height, vcodec, thumb_path, title, game, favorite, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)
             ON CONFLICT(path) DO UPDATE SET
               mtime=excluded.mtime, size=excluded.size, duration_ms=excluded.duration_ms,
               width=excluded.width, height=excluded.height, vcodec=excluded.vcodec,
               thumb_path=excluded.thumb_path,
               title=CASE WHEN excluded.title IS NOT NULL THEN excluded.title ELSE clips.title END,
               game=CASE WHEN excluded.game IS NOT NULL THEN excluded.game ELSE clips.game END",
            params![
                p,
                clip.mtime,
                clip.size as i64,
                clip.probe.duration_ms as i64,
                clip.probe.width,
                clip.probe.height,
                clip.probe.vcodec,
                clip.thumb_path.as_ref().map(|t| path_str(t)),
                title,
                game,
                clip.added_at,
            ],
        )?;

        tx.execute("DELETE FROM clip_tracks WHERE path = ?1", params![p])?;
        for t in &clip.probe.tracks {
            tx.execute(
                "INSERT INTO clip_tracks (path, idx, label, language) VALUES (?1, ?2, ?3, ?4)",
                params![p, t.idx, t.label, t.language],
            )?;
        }

        tx.execute(
            "DELETE FROM clip_default_tracks WHERE path = ?1",
            params![p],
        )?;
        for st in &clip.default_tracks {
            tx.execute(
                "INSERT INTO clip_default_tracks (path, idx, volume, muted) VALUES (?1, ?2, ?3, ?4)",
                params![p, st.idx, st.volume, st.muted as i64],
            )?;
        }

        tx.execute("DELETE FROM clip_meta WHERE path = ?1", params![p])?;
        for (ord, (k, v)) in clip.meta.iter().enumerate() {
            tx.execute(
                "INSERT INTO clip_meta (path, ord, key, value) VALUES (?1, ?2, ?3, ?4)",
                params![p, ord as i64, k, v],
            )?;
        }

        tx.execute(
            "DELETE FROM tags WHERE path = ?1 AND source = 'script'",
            params![p],
        )?;
        for tag in &clip.stags {
            tx.execute(
                "INSERT OR IGNORE INTO tags (path, tag, source) VALUES (?1, ?2, 'script')",
                params![p, tag],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<Clip>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CLIP_COLS} FROM clips ORDER BY mtime DESC, path ASC"
        ))?;
        let mut clips: Vec<Clip> = stmt
            .query_map([], row_to_clip)?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for clip in &mut clips {
            hydrate(&conn, clip)?;
        }
        Ok(clips)
    }

    pub fn load_clip(&self, path: &Path) -> Result<Option<Clip>> {
        let conn = self.lock();
        let mut clip = conn
            .query_row(
                &format!("SELECT {CLIP_COLS} FROM clips WHERE path = ?1"),
                params![path_str(path)],
                row_to_clip,
            )
            .optional()?;
        if let Some(clip) = &mut clip {
            hydrate(&conn, clip)?;
        }
        Ok(clip)
    }

    pub fn set_favorite(&self, path: &Path, favorite: bool) -> Result<()> {
        self.lock().execute(
            "UPDATE clips SET favorite = ?2 WHERE path = ?1",
            params![path_str(path), favorite as i64],
        )?;
        Ok(())
    }

    pub fn set_title(&self, path: &Path, title: Option<&str>) -> Result<()> {
        let t = title.map(str::trim).filter(|s| !s.is_empty());
        self.lock().execute(
            "UPDATE clips SET title = ?2 WHERE path = ?1",
            params![path_str(path), t],
        )?;
        Ok(())
    }

    pub fn set_game(&self, path: &Path, game: Option<&str>) -> Result<()> {
        let g = game.map(str::trim).filter(|s| !s.is_empty());
        self.lock().execute(
            "UPDATE clips SET game = ?2 WHERE path = ?1",
            params![path_str(path), g],
        )?;
        Ok(())
    }

    pub fn set_marks(&self, path: &Path, start: Option<u64>, end: Option<u64>) -> Result<()> {
        self.lock().execute(
            "UPDATE clips SET mark_start = ?2, mark_end = ?3 WHERE path = ?1",
            params![
                path_str(path),
                start.map(|v| v as i64),
                end.map(|v| v as i64)
            ],
        )?;
        Ok(())
    }

    pub fn add_tag(&self, path: &Path, tag: &str) -> Result<()> {
        self.lock().execute(
            "INSERT OR IGNORE INTO tags (path, tag, source) VALUES (?1, ?2, 'user')",
            params![path_str(path), tag],
        )?;
        Ok(())
    }

    pub fn remove_tag(&self, path: &Path, tag: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM tags WHERE path = ?1 AND tag = ?2 AND source = 'user'",
            params![path_str(path), tag],
        )?;
        Ok(())
    }

    pub fn set_track_state(&self, path: &Path, st: TrackState) -> Result<()> {
        self.lock().execute(
            "INSERT INTO track_state (path, idx, volume, muted) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path, idx) DO UPDATE SET volume=excluded.volume, muted=excluded.muted",
            params![path_str(path), st.idx, st.volume, st.muted as i64],
        )?;
        Ok(())
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT value FROM app_kv WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "INSERT OR REPLACE INTO app_kv (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn steam_get(&self, app_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT name FROM steam_cache WHERE app_id = ?1",
                params![app_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn steam_put(&self, app_id: &str, name: &str) -> Result<()> {
        self.lock().execute(
            "INSERT OR REPLACE INTO steam_cache (app_id, name, fetched_at) VALUES (?1, ?2, ?3)",
            params![app_id, name, now_secs()],
        )?;
        Ok(())
    }
}

const CLIP_COLS: &str = "path, mtime, size, duration_ms, width, height, vcodec, thumb_path, title, game, mark_start, mark_end, favorite, added_at";

fn row_to_clip(r: &rusqlite::Row) -> rusqlite::Result<Clip> {
    Ok(Clip {
        path: PathBuf::from(r.get::<_, String>(0)?),
        mtime: r.get(1)?,
        size: r.get::<_, i64>(2)? as u64,
        probe: ClipProbe {
            duration_ms: r.get::<_, i64>(3)? as u64,
            width: r.get(4)?,
            height: r.get(5)?,
            vcodec: r.get(6)?,
            tracks: Vec::new(),
            container_tags: Vec::new(),
        },
        thumb_path: r.get::<_, Option<String>>(7)?.map(PathBuf::from),
        title: r.get::<_, Option<String>>(8)?,
        game: r.get::<_, Option<String>>(9)?,
        mark_start: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        mark_end: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        favorite: r.get::<_, i64>(12)? != 0,
        stags: Vec::new(),
        mtags: Vec::new(),
        meta: Vec::new(),
        track_state: Vec::new(),
        default_tracks: Vec::new(),
        added_at: r.get(13)?,
    })
}

fn hydrate(conn: &Connection, clip: &mut Clip) -> Result<()> {
    let p = path_str(&clip.path);
    clip.probe.tracks = load_tracks(conn, &p)?;
    clip.track_state = load_track_state(conn, &p, "track_state")?;
    clip.default_tracks = load_track_state(conn, &p, "clip_default_tracks")?;
    clip.meta = load_meta(conn, &p)?;
    clip.stags = load_tags(conn, &p, "script")?;
    clip.mtags = load_tags(conn, &p, "user")?;
    Ok(())
}

fn load_tracks(conn: &Connection, p: &str) -> Result<Vec<AudioTrack>> {
    let mut stmt =
        conn.prepare("SELECT idx, label, language FROM clip_tracks WHERE path = ?1 ORDER BY idx")?;
    Ok(stmt
        .query_map(params![p], |r| {
            Ok(AudioTrack {
                idx: r.get(0)?,
                label: r.get(1)?,
                language: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn load_track_state(conn: &Connection, p: &str, table: &str) -> Result<Vec<TrackState>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT idx, volume, muted FROM {table} WHERE path = ?1 ORDER BY idx"
    ))?;
    Ok(stmt
        .query_map(params![p], |r| {
            Ok(TrackState {
                idx: r.get(0)?,
                volume: r.get(1)?,
                muted: r.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn load_meta(conn: &Connection, p: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM clip_meta WHERE path = ?1 ORDER BY ord")?;
    Ok(stmt
        .query_map(params![p], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn load_tags(conn: &Connection, p: &str, source: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag FROM tags WHERE path = ?1 AND source = ?2 ORDER BY tag")?;
    Ok(stmt
        .query_map(params![p, source], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS clips (
    path        TEXT PRIMARY KEY,
    mtime       INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    width       INTEGER NOT NULL,
    height      INTEGER NOT NULL,
    vcodec      TEXT NOT NULL,
    thumb_path  TEXT,
    title       TEXT,
    game        TEXT,
    mark_start  INTEGER,
    mark_end    INTEGER,
    favorite    INTEGER NOT NULL DEFAULT 0,
    added_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS clip_tracks (
    path     TEXT NOT NULL REFERENCES clips(path) ON DELETE CASCADE,
    idx      INTEGER NOT NULL,
    label    TEXT NOT NULL,
    language TEXT,
    PRIMARY KEY (path, idx)
);
CREATE TABLE IF NOT EXISTS track_state (
    path   TEXT NOT NULL REFERENCES clips(path) ON DELETE CASCADE,
    idx    INTEGER NOT NULL,
    volume REAL NOT NULL,
    muted  INTEGER NOT NULL,
    PRIMARY KEY (path, idx)
);
CREATE TABLE IF NOT EXISTS clip_default_tracks (
    path   TEXT NOT NULL REFERENCES clips(path) ON DELETE CASCADE,
    idx    INTEGER NOT NULL,
    volume REAL NOT NULL,
    muted  INTEGER NOT NULL,
    PRIMARY KEY (path, idx)
);
CREATE TABLE IF NOT EXISTS clip_meta (
    path  TEXT NOT NULL REFERENCES clips(path) ON DELETE CASCADE,
    ord   INTEGER NOT NULL,
    key   TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (path, key)
);
CREATE TABLE IF NOT EXISTS tags (
    path   TEXT NOT NULL REFERENCES clips(path) ON DELETE CASCADE,
    tag    TEXT NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (path, tag)
);
CREATE TABLE IF NOT EXISTS steam_cache (
    app_id     TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::model::AudioTrack;

    fn tmp_store() -> Store {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Store::open(&std::env::temp_dir().join(format!("madalya-test-{n}.db"))).unwrap()
    }

    fn sample() -> Clip {
        Clip {
            path: PathBuf::from("/clips/a.mkv"),
            mtime: 100,
            size: 2048,
            probe: ClipProbe {
                duration_ms: 5000,
                width: 1280,
                height: 720,
                vcodec: "H.264".into(),
                tracks: vec![
                    AudioTrack {
                        idx: 0,
                        label: "Game".into(),
                        language: None,
                    },
                    AudioTrack {
                        idx: 1,
                        label: "Track 2".into(),
                        language: None,
                    },
                ],
                container_tags: vec![],
            },
            favorite: false,
            thumb_path: Some(PathBuf::from("/thumbs/a.jpg")),
            title: Some("A".into()),
            game: Some("Dota 2".into()),
            mark_start: None,
            mark_end: None,
            stags: vec!["clip".into()],
            mtags: vec![],
            meta: vec![("kind".into(), "video".into())],
            track_state: vec![],
            default_tracks: vec![],
            added_at: 1,
        }
    }

    #[test]
    fn roundtrip_and_fingerprints() {
        let s = tmp_store();
        s.upsert_clip(&sample()).unwrap();
        let clips = s.load_all().unwrap();
        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        assert_eq!(c.probe.tracks.len(), 2);
        assert_eq!(c.title(), "A");
        assert_eq!(c.game(), Some("Dota 2"));
        assert_eq!(c.stags, vec!["clip".to_string()]);
        assert!(c.mtags.is_empty());

        let fp = s.fingerprints().unwrap();
        assert_eq!(fp.get(&PathBuf::from("/clips/a.mkv")), Some(&(100, 2048)));
    }

    #[test]
    fn rescan_preserves_user_state_and_replaces_script_data() {
        let s = tmp_store();
        let path = PathBuf::from("/clips/a.mkv");
        s.upsert_clip(&sample()).unwrap();

        // User edits.
        s.set_favorite(&path, true).unwrap();
        s.add_tag(&path, "funny").unwrap();
        s.set_marks(&path, Some(1000), Some(4000)).unwrap();
        s.set_track_state(
            &path,
            TrackState {
                idx: 1,
                volume: 0.4,
                muted: true,
            },
        )
        .unwrap();

        // Re-scan produces new script-derived title/tags.
        let mut changed = sample();
        changed.mtime = 200;
        changed.title = Some("A2".into());
        changed.stags = vec!["clip2".into()];
        s.upsert_clip(&changed).unwrap();

        let c = &s.load_all().unwrap()[0];
        assert!(c.favorite, "favorite preserved across rescan");
        assert_eq!(
            c.marks(),
            Some((1000, 4000)),
            "marks preserved across rescan"
        );
        assert!(
            c.mtags.contains(&"funny".to_string()),
            "manual tag preserved"
        );
        assert!(
            c.stags.contains(&"clip2".to_string()),
            "new script tag applied"
        );
        assert!(
            !c.stags.contains(&"clip".to_string()),
            "old script tag replaced"
        );
        assert_eq!(c.title(), "A2", "script title replaced");
        let st = c.state_for(1);
        assert_eq!(st.volume, 0.4);
        assert!(st.muted, "mixer state preserved");
    }

    #[test]
    fn rescan_overwrites_title_but_preserves_when_script_empty() {
        let s = tmp_store();
        let path = PathBuf::from("/clips/a.mkv");
        s.upsert_clip(&sample()).unwrap();
        let raw = |s: &Store| {
            s.load_clip(&path)
                .unwrap()
                .unwrap()
                .title_raw()
                .map(str::to_owned)
        };
        assert_eq!(raw(&s).as_deref(), Some("A"));

        // Script re-run yields no title -> keep the stored value.
        let mut empty = sample();
        empty.title = None;
        s.upsert_clip(&empty).unwrap();
        assert_eq!(
            raw(&s).as_deref(),
            Some("A"),
            "empty script title preserved"
        );

        // Manual override survives an empty re-run too.
        s.set_title(&path, Some("My Clip")).unwrap();
        s.upsert_clip(&empty).unwrap();
        assert_eq!(raw(&s).as_deref(), Some("My Clip"));

        // A non-empty script title always overwrites.
        let mut has_title = sample();
        has_title.title = Some("Scripted".into());
        s.upsert_clip(&has_title).unwrap();
        assert_eq!(raw(&s).as_deref(), Some("Scripted"));

        // Clearing the manual title falls back to the filename stem.
        s.set_title(&path, None).unwrap();
        assert_eq!(raw(&s), None);
        assert_eq!(s.load_clip(&path).unwrap().unwrap().title(), "a");
    }

    #[test]
    fn rescan_overwrites_game_but_preserves_when_script_empty() {
        let s = tmp_store();
        let path = PathBuf::from("/clips/a.mkv");
        s.upsert_clip(&sample()).unwrap();
        assert_eq!(s.load_clip(&path).unwrap().unwrap().game(), Some("Dota 2"));

        // Script re-run yields no game -> keep the stored value.
        let mut empty = sample();
        empty.game = None;
        s.upsert_clip(&empty).unwrap();
        assert_eq!(
            s.load_clip(&path).unwrap().unwrap().game(),
            Some("Dota 2"),
            "empty script game preserves stored value"
        );

        // Manual override survives an empty re-run too.
        s.set_game(&path, Some("Celeste")).unwrap();
        s.upsert_clip(&empty).unwrap();
        assert_eq!(s.load_clip(&path).unwrap().unwrap().game(), Some("Celeste"));

        // A non-empty script game always overwrites.
        let mut has_game = sample();
        has_game.game = Some("Portal 2".into());
        s.upsert_clip(&has_game).unwrap();
        assert_eq!(
            s.load_clip(&path).unwrap().unwrap().game(),
            Some("Portal 2")
        );

        // Clearing the manual game.
        s.set_game(&path, None).unwrap();
        assert_eq!(s.load_clip(&path).unwrap().unwrap().game(), None);
    }

    #[test]
    fn delete_cascades() {
        let s = tmp_store();
        let path = PathBuf::from("/clips/a.mkv");
        s.upsert_clip(&sample()).unwrap();
        s.add_tag(&path, "x").unwrap();
        s.delete_clip(&path).unwrap();
        assert!(s.load_all().unwrap().is_empty());
        assert!(s.fingerprints().unwrap().is_empty());
    }
}
