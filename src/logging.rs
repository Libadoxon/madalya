use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::Context as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::meta::APP_ID;

/// Cap on total log-dir size. On startup the oldest `<APP_ID>-*.log` files are
/// removed until the total drops below this threshold.
const MAX_LOG_DIR_BYTES: u64 = 100 * 1024 * 1024;

/// Initialize tracing. Always writes coloured output to stderr; if
/// `log_to_file` is true, also tees into
/// `<state_dir>/<APP_ID>/logs/<APP_ID>-<ts>-<pid>.log`.
/// Filter level is `info` by default and respects `RUST_LOG`.
pub fn init(log_to_file: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_ansi(true);

    let file_layer = if log_to_file {
        match open_log_file() {
            Ok(file) => Some(
                fmt::layer()
                    .with_writer(Mutex::new(file))
                    .with_ansi(false)
                    .boxed(),
            ),
            Err(e) => {
                eprintln!("clerk: file logging disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
}

fn open_log_file() -> anyhow::Result<File> {
    let dir = log_dir().context("locating log directory")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating log dir {}", dir.display()))?;
    prune_log_dir(&dir, MAX_LOG_DIR_BYTES);
    let ts = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S");
    let pid = std::process::id();
    let path = dir.join(format!("{APP_ID}-{ts}-{pid}.log"));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening log file {}", path.display()))
}

/// Best-effort: delete the oldest `<APP_ID>-*.log` files until the dir's total
/// size is under `max_bytes`. Skips unrecognised files so we don't nuke
/// anything the user may have dropped in. Errors go to stderr; tracing isn't
/// initialised yet at this point.
fn prune_log_dir(dir: &Path, max_bytes: u64) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("clerk: log dir prune skipped: {e}");
            return;
        }
    };
    let mut entries: Vec<(PathBuf, u64, SystemTime)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("clerk-") || !name.ends_with(".log") {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let mtime = meta.modified().ok()?;
            Some((path, meta.len(), mtime))
        })
        .collect();

    let mut total: u64 = entries.iter().map(|(_, len, _)| *len).sum();
    if total <= max_bytes {
        return;
    }
    entries.sort_by_key(|(_, _, mtime)| *mtime);
    for (path, len, _) in entries {
        if total <= max_bytes {
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(len),
            Err(e) => eprintln!("clerk: failed to prune {}: {e}", path.display()),
        }
    }
}

fn log_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::state_dir()
        .or_else(dirs::cache_dir)
        .context("no state or cache dir on this platform")?;
    Ok(base.join("clerk").join("logs"))
}
