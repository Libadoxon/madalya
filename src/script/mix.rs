use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rhai::{AST, Array, Engine, Map, Scope};

use super::engine::{ClipInput, build_input_map};
use super::steam;
use crate::library::store::Store;

pub struct MixScript {
    engine: Engine,
    ast: AST,
}

/// An enabled track in the default mix.
pub struct TrackChoice {
    pub idx: u32,
    pub volume: f64,
}

impl MixScript {
    pub fn load(script_path: &Path, store: Store) -> Result<Self> {
        let mut engine = Engine::new();
        engine.set_max_expr_depths(128, 64);
        engine.register_fn("steam_app_name", move |id: i64| -> String {
            steam::resolve(&store, id).unwrap_or_default()
        });
        let ast = engine
            .compile_file(PathBuf::from(script_path))
            .with_context(|| format!("compiling mix script {script_path:?}"))?;
        Ok(Self { engine, ast })
    }

    pub fn run(&self, input: ClipInput) -> Result<Vec<TrackChoice>> {
        let mut scope = Scope::new();
        scope.push_constant("clip", build_input_map(&input));
        let out: Array = self
            .engine
            .eval_ast_with_scope(&mut scope, &self.ast)
            .context("running mix script")?;
        Ok(interpret(out))
    }
}

fn interpret(out: Array) -> Vec<TrackChoice> {
    out.into_iter()
        .filter_map(|item| {
            let m = item.try_cast::<Map>()?;
            let idx = m.get("track")?.as_int().ok()? as u32;
            let volume = m
                .get("volume")
                .and_then(|v| v.as_float().ok())
                .unwrap_or(1.0);
            Some(TrackChoice { idx, volume })
        })
        .collect()
}
