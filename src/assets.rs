use std::path::PathBuf;

use gpui_kit::component::*;
use gpui_kit::*;
use rust_embed::RustEmbed;

use crate::config::Config;
use crate::meta::APP_ID;

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
