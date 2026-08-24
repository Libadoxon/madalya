use std::borrow::Cow;
use std::path::PathBuf;

use gpui::*;
use gpui_component::*;
use rust_embed::RustEmbed;

use crate::config::Config;
use crate::meta::APP_ID;

/// Asset source for the app. Bundles icons from `./assets` at compile time
/// and falls back to `gpui_component_assets` for anything we don't ship.
/// Icons come from https://lucide.dev/icons
#[derive(RustEmbed)]
#[folder = "./assets"]
#[include = "icons/**/*.svg"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(f) = Self::get(path) {
            return Ok(Some(f.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = Self::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        for entry in gpui_component_assets::Assets.list(path)? {
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum IconName {
    CircleDot,
    Funnel,
    RotateCcw,
}

impl IconNamed for IconName {
    fn path(self) -> SharedString {
        match self {
            IconName::CircleDot => "icons/circle-dot.svg",
            IconName::Funnel => "icons/funnel.svg",
            IconName::RotateCcw => "icons/rotate-ccw.svg",
        }
        .into()
    }
}

#[derive(RustEmbed)]
#[folder = "./themes"]
#[include = "*.json"]
struct BundledThemes;

pub fn themes_dir() -> PathBuf {
    dirs::config_dir()
        .expect("no config dir")
        .join(APP_ID)
        .join("themes")
}

/// Copy bundled themes into the user's themes dir, without clobbering files the
/// user has added or edited.
fn install_bundled_themes(dir: &std::path::Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("failed to create themes dir at {}: {e}", dir.display());
        return;
    }
    for name in BundledThemes::iter() {
        let dest = dir.join(name.as_ref());
        if dest.exists() {
            continue;
        }
        if let Some(f) = BundledThemes::get(&name)
            && let Err(e) = std::fs::write(&dest, f.data)
        {
            tracing::warn!("failed to write theme {}: {e}", dest.display());
        }
    }
}

/// Install bundled themes, then watch the themes dir and keep the configured
/// theme applied — live across theme-file edits and selection changes.
pub fn init_themes(cx: &mut App) {
    let dir = themes_dir();
    install_bundled_themes(&dir);

    if let Err(err) = ThemeRegistry::watch_dir(dir, cx, apply_selected_theme) {
        tracing::warn!("failed to watch themes dir: {err}");
    }
    cx.observe_global::<Config>(apply_selected_theme).detach();
    cx.observe_global::<ThemeRegistry>(apply_selected_theme)
        .detach();
}

fn apply_selected_theme(cx: &mut App) {
    let name = cx.global::<Config>().appearance.theme.clone();
    let Some(theme) = ThemeRegistry::global(cx)
        .themes()
        .get(name.as_str())
        .cloned()
    else {
        tracing::warn!("selected theme {name:?} not found");
        return;
    };
    let theme_mut = Theme::global_mut(cx);
    theme_mut.apply_config(&theme);
    theme_mut.font_family = SharedString::from("Inter");
}
