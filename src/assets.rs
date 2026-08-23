use std::borrow::Cow;

use gpui::*;
use gpui_component::*;
use rust_embed::RustEmbed;

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
}

impl IconNamed for IconName {
    fn path(self) -> SharedString {
        match self {
            IconName::CircleDot => "icons/circle-dot.svg",
        }
        .into()
    }
}

/// Register the theme registry against the user's themes dir.
/// The directory is created if missing. Any theme matching
/// `theme_name` (re)applies on every directory change.
pub fn init_themes(theme_name: impl Into<SharedString>, cx: &mut App) {
    let dir = dirs::config_dir()
        .expect("no config dir")
        .join(APP_ID)
        .join("themes");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create themes dir at {}: {e}", dir.display());
    }
    let theme_name = theme_name.into();
    if let Err(err) = ThemeRegistry::watch_dir(dir, cx, move |cx| {
        if let Some(theme) = ThemeRegistry::global(cx).themes().get(&theme_name).cloned() {
            let theme_mut = Theme::global_mut(cx);
            theme_mut.apply_config(&theme);
            theme_mut.font_family = SharedString::from("Inter");
        }
    }) {
        tracing::warn!("failed to watch themes dir: {err}");
    }
}
