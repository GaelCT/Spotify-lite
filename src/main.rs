//! Entry point: logging setup, then the native eframe window.

mod app;
mod auth;
mod config;
mod player;
mod soloist;
mod types;
mod ui;
mod webapi;

use eframe::egui;

fn main() -> eframe::Result<()> {
    init_tracing();

    let options = eframe::NativeOptions {
        // No window-geometry persistence: a stale saved size (e.g. after
        // dragging the window beyond a small screen's edge) must never bury
        // the bottom now-playing bar off-screen — start sane every launch.
        // The size itself stays modest because DPI scaling multiplies it
        // physically; `update()` clamps to the real monitor metrics anyway.
        // The minimum keeps the now-playing bar's right section from
        // silently clipping below ~870 logical px of width.
        persist_window: false,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 560.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "spotify-lite",
        options,
        Box::new(|cc| Ok(Box::new(app::SpotifyLiteApp::new(cc)))),
    )
}

/// Initializes `tracing` logging to `config::log_path()`, falling back to
/// stderr when the log file cannot be created.
fn init_tracing() {
    match config::log_path() {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::File::create(&path) {
                Ok(file) => tracing_subscriber::fmt()
                    .with_writer(move || {
                        file.try_clone()
                            .expect("failed to clone log file handle for tracing")
                    })
                    .init(),
                Err(e) => {
                    eprintln!("failed to open log file {}: {e}; logging to stderr", path.display());
                    tracing_subscriber::fmt().init();
                }
            }
        }
        None => tracing_subscriber::fmt().init(),
    }
}
