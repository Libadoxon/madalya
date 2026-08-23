use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use gpui::{App, Global};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumMessage, EnumString, IntoStaticStr};

use crate::keybinds::Keybinds;
use crate::meta::APP_ID;

/// Top-level config. Each field is a nested struct that maps 1:1 to a page in
/// the settings menu and a `[section]` table in the TOML file.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub keybinds: Keybinds,
}

/// Demo settings section. Each field shows a different `SettingField` widget in
/// `ui/settings.rs`: a switch, a dropdown, a number input, and a text input.
/// Replace these with your own; `log_to_file` is the one field the framework
/// actually reads (see `logging::init`).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub example_toggle: bool,
    pub example_choice: ExampleChoice,
    pub example_number: u32,
    pub example_text: String,
    pub log_to_file: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            example_toggle: true,
            example_choice: ExampleChoice::default(),
            example_number: 42,
            example_text: "hello".into(),
            log_to_file: false,
        }
    }
}

/// Example enum-backed setting rendered as a dropdown. Copy this pattern for
/// any multiple-choice value: the variant identifier is the key stored in TOML
/// and `#[strum(message = ...)]` supplies the human-readable label.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    Default,
    EnumIter,
    EnumString,
    IntoStaticStr,
    EnumMessage,
)]
pub enum ExampleChoice {
    #[default]
    #[strum(message = "First option")]
    First,
    #[strum(message = "Second option")]
    Second,
    #[strum(message = "Third option")]
    Third,
}

impl Global for Config {}

/// Runtime metadata about the config file
#[derive(Clone, Default, PartialEq)]
pub struct ConfigStatus {
    pub readonly: bool,
}

impl Global for ConfigStatus {}

/// Absolute path to the config file: `$XDG_CONFIG_HOME/<APP_ID>/config.toml`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .expect("no config dir")
        .join(APP_ID)
        .join("config.toml")
}

pub fn is_readonly(path: &Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_err()
}

/// Apply `mutate` to a fresh clone of the global config and persist if it
/// actually changed. Saves us from forgetting to call `cx.set_global` or
/// hit disk after each tweak.
pub fn update(cx: &mut App, mutate: impl FnOnce(&mut Config)) {
    let mut next = cx.global::<Config>().clone();
    mutate(&mut next);
    if &next == cx.global::<Config>() {
        return;
    }
    cx.set_global(next.clone());
    if let Err(e) = save(&next, &config_path()) {
        tracing::error!("failed to save config: {e}");
    }
}

/// Read the config from disk. If the file is missing, write defaults silently
/// and return them. If the file exists but is unparseable, move it aside as
/// `non-parsable-<unix_ts>.toml`
pub fn load_or_create(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<Config>(&content) {
            Ok(cfg) => return cfg,
            Err(e) => {
                tracing::warn!("config at {} is unparseable: {e}", path.display());
                let backup = backup_path_for(path);
                match std::fs::rename(path, &backup) {
                    Ok(_) => tracing::warn!("moved unparseable config to {}", backup.display()),
                    Err(e) => tracing::error!(
                        "failed to move unparseable config to {}: {e}",
                        backup.display()
                    ),
                }
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("config file not found, creating")
        }
        Err(e) => {
            tracing::error!("failed to read config at {}: {e}", path.display());
        }
    }

    let cfg = Config::default();
    if let Err(e) = save(&cfg, path) {
        tracing::error!("failed to write default config to {}: {e}", path.display());
    }
    cfg
}

fn backup_path_for(config_path: &Path) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("non-parsable-{ts}.toml"))
}

pub fn save(cfg: &Config, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(cfg).context("serializing config")?;
    std::fs::write(path, body)?;
    Ok(())
}

/// Watch the config file for external edits. When the file's content changes,
/// update the `Config` global so observers (the settings menu, anything reading
/// `cx.global::<Config>()`) see the new values. Editors that rename-replace the
/// file would orphan a direct watch, so we watch the parent dir instead.
pub fn watch(path: PathBuf, cx: &mut App) {
    let (tx, rx) = smol::channel::bounded::<()>(8);
    let Ok(mut watcher) = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if matches!(
            event.kind,
            notify::EventKind::Modify(_) | notify::EventKind::Create(_)
        ) {
            let _ = tx.send_blocking(());
        }
    }) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
        tracing::warn!("failed to watch config dir: {e}");
        return;
    }

    cx.spawn(async move |cx| {
        // Keep the watcher alive for the life of the task.
        let _watcher = watcher;
        while rx.recv().await.is_ok() {
            // Conservative on this path: an in-flight editor save may briefly
            // produce an unparseable file. Log and skip rather than backing it
            // up — the user's next save likely fixes it.
            let cfg = match std::fs::read_to_string(&path) {
                Ok(s) => match toml::from_str::<Config>(&s) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        tracing::warn!("config reload skipped, unparseable: {e}");
                        continue;
                    }
                },
                Err(_) => continue,
            };
            let status = ConfigStatus {
                readonly: is_readonly(&path),
            };
            cx.update(|cx| {
                if cx.try_global::<Config>() != Some(&cfg) {
                    cx.set_global(cfg);
                }
                if cx.try_global::<ConfigStatus>() != Some(&status) {
                    if status.readonly {
                        tracing::warn!("config file is read-only; settings are locked");
                    }
                    cx.set_global(status);
                }
            });
        }
    })
    .detach();
}
