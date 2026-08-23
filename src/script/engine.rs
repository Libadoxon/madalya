use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rhai::{AST, Array, Dynamic, Engine, Map, Scope};

use super::steam;
use crate::library::model::ClipProbe;
use crate::library::store::Store;

pub struct ScriptEngine {
    engine: Engine,
    ast: AST,
}

pub struct ScriptResult {
    pub game: Option<String>,
    pub meta: Vec<(String, String)>,
    pub tags: Vec<String>,
}

pub struct ClipInput<'a> {
    pub path: &'a Path,
    pub mtime: i64,
    pub size: u64,
    pub probe: &'a ClipProbe,
}

impl ScriptEngine {
    pub fn load(script_path: &Path, store: Store) -> Result<Self> {
        let mut engine = Engine::new();
        engine.set_max_expr_depths(128, 64);

        engine.register_fn("steam_app_name", move |id: i64| -> String {
            steam::resolve(&store, id).unwrap_or_default()
        });

        let ast = engine
            .compile_file(PathBuf::from(script_path))
            .with_context(|| format!("compiling script {script_path:?}"))?;
        Ok(Self { engine, ast })
    }

    pub fn run(&self, input: ClipInput) -> Result<ScriptResult> {
        let mut scope = Scope::new();
        scope.push_constant("clip", build_input_map(&input));
        let out: Map = self
            .engine
            .eval_ast_with_scope(&mut scope, &self.ast)
            .context("running metadata script")?;
        Ok(interpret(out))
    }
}

fn build_input_map(input: &ClipInput) -> Map {
    let path = input.path;
    let mut m = Map::new();
    m.insert("path".into(), path.to_string_lossy().to_string().into());
    m.insert("filename".into(), file_part(path, |p| p.file_name()).into());
    m.insert("stem".into(), file_part(path, |p| p.file_stem()).into());
    m.insert(
        "ext".into(),
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
            .into(),
    );
    m.insert(
        "dir".into(),
        path.parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
            .into(),
    );
    m.insert("size".into(), (input.size as i64).into());
    m.insert("mtime".into(), input.mtime.into());
    m.insert(
        "duration_ms".into(),
        (input.probe.duration_ms as i64).into(),
    );
    m.insert("width".into(), (input.probe.width as i64).into());
    m.insert("height".into(), (input.probe.height as i64).into());
    m.insert("vcodec".into(), input.probe.vcodec.clone().into());

    let tracks: Array = input
        .probe
        .tracks
        .iter()
        .map(|t| t.label.clone().into())
        .collect();
    m.insert("audio_tracks".into(), tracks.into());

    let mut tags = Map::new();
    for (k, v) in &input.probe.container_tags {
        tags.insert(k.as_str().into(), v.clone().into());
    }
    m.insert("container_tags".into(), tags.into());
    m
}

fn interpret(out: Map) -> ScriptResult {
    let mut game = None;
    let mut meta = Vec::new();
    let mut tags = Vec::new();
    for (k, v) in out {
        if k == "tags" {
            if let Some(arr) = v.try_cast::<Array>() {
                tags = arr.into_iter().map(dyn_to_string).collect();
            }
            continue;
        }
        if k == "game" {
            let g = dyn_to_string(v);
            game = (!g.trim().is_empty()).then_some(g);
            continue;
        }
        meta.push((k.to_string(), dyn_to_string(v)));
    }
    ScriptResult { game, meta, tags }
}

fn dyn_to_string(v: Dynamic) -> String {
    if v.is_string() {
        v.into_string().unwrap_or_default()
    } else {
        v.to_string()
    }
}

fn file_part(path: &Path, f: impl Fn(&Path) -> Option<&std::ffi::OsStr>) -> String {
    f(path)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> Store {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Store::open(&std::env::temp_dir().join(format!("madalya-script-{n}.db"))).unwrap()
    }

    fn load(src: &str) -> ScriptEngine {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("m-{n}.rhai"));
        std::fs::write(&path, src).unwrap();
        ScriptEngine::load(&path, tmp_store()).unwrap()
    }

    #[test]
    fn maps_meta_and_lifts_tags() {
        let e = load(r#"#{ title: clip.stem, kind: "video", tags: ["a", "b"] }"#);
        let probe = ClipProbe::default();
        let r = e
            .run(ClipInput {
                path: Path::new("/x/foo bar.mp4"),
                mtime: 0,
                size: 0,
                probe: &probe,
            })
            .unwrap();
        assert_eq!(r.tags, vec!["a".to_string(), "b".to_string()]);
        assert!(r.meta.contains(&("title".into(), "foo bar".into())));
        assert!(r.meta.contains(&("kind".into(), "video".into())));
        assert!(!r.meta.iter().any(|(k, _)| k == "tags"));
    }

    #[test]
    fn exposes_probe_fields() {
        let e = load(r#"#{ w: clip.width, tracks: clip.audio_tracks.len() }"#);
        let probe = ClipProbe {
            width: 1920,
            tracks: vec![
                crate::library::model::AudioTrack {
                    idx: 0,
                    label: "Game".into(),
                    language: None,
                },
                crate::library::model::AudioTrack {
                    idx: 1,
                    label: "Track 2".into(),
                    language: None,
                },
            ],
            ..Default::default()
        };
        let r = e
            .run(ClipInput {
                path: Path::new("/x/y.mp4"),
                mtime: 0,
                size: 0,
                probe: &probe,
            })
            .unwrap();
        assert_eq!(r.meta.iter().find(|(k, _)| k == "w").unwrap().1, "1920");
        assert_eq!(r.meta.iter().find(|(k, _)| k == "tracks").unwrap().1, "2");
    }
}
