use gpui::*;
use gpui_component::*;

use crate::app::AppView;
use crate::assets::Assets;

mod app;
mod assets;
mod config;
mod keybinds;
mod library;
mod logging;
mod media;
mod meta;
mod script;
mod ui;

fn main() {
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);

        if let Err(e) = media::init() {
            tracing::error!("failed to init gstreamer: {e:#}");
        }

        // Load or create config (with parent dirs), publish as a Global, and
        // start watching the file for external edits.
        let config_path = config::config_path();
        let config = config::load_or_create(&config_path);
        logging::init(config.general.log_to_file);
        tracing::info!(
            "starting {} v{} — {}",
            meta::APP_DISPLAY_NAME,
            meta::APP_VERSION,
            meta::APP_DESCRIPTION
        );
        let readonly = config::is_readonly(&config_path);
        if readonly {
            tracing::warn!(
                "config at {} is read-only; settings are locked",
                config_path.display()
            );
        }
        cx.set_global(config);
        cx.set_global(config::ConfigStatus { readonly });
        config::watch(config_path, cx);

        assets::init_themes("Gruvbox Light", cx);

        let store = match library::open_store() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to open library store: {e:#}");
                return;
            }
        };
        let lib = library::new_library(store, cx);

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.), px(480.))),
            app_id: Some(meta::APP_ID.into()),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| AppView::new(lib.clone(), window, cx));
                // This first level on the window, should be a Root.
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
