//! Bottom now-playing bar: track info + album art, transport controls, seek
//! and volume, plus a tiny async album-art texture cache.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};

use crate::app::SpotifyLiteApp;
use crate::types::{format_duration_ms, RepeatMode, UiCommand};
use crate::ui::theme;

const ART_SIZE: f32 = 52.0;

/// Raw decoded image shipped from the fetcher thread to the UI thread.
struct ArtImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

enum ArtState {
    Loading(Receiver<Result<ArtImage, String>>),
    Ready(egui::TextureHandle),
    Failed,
}

/// Album art textures by image URL. `egui::TextureHandle` must be created on
/// the UI thread, so the worker only sends raw pixels back through the channel.
static ART_CACHE: OnceLock<Mutex<HashMap<String, ArtState>>> = OnceLock::new();

/// While the user drags a slider, the snapshot value would snap the handle
/// back; these overrides keep the dragged value until the backend echoes it.
static SEEK_OVERRIDE: Mutex<Option<u32>> = Mutex::new(None);
static VOLUME_OVERRIDE: Mutex<Option<u16>> = Mutex::new(None);

/// Draws the 72px now-playing bar from the latest [`crate::types::PlayerSnapshot`].
pub fn draw(ui: &mut egui::Ui, ctx: &egui::Context, app: &mut SpotifyLiteApp) {
    ui.set_min_height(ART_SIZE);

    // The three sections must live in ONE horizontal row: allocating each
    // with the panel's full height in the default top-down layout stacks
    // them (3 × 72 px), overflowing the panel off the bottom of the window.
    ui.horizontal(|ui| {
        let available = ui.available_size();
        let side_width = 250.0f32;
        let right_width = 240.0f32;
        let center_width = (available.x - side_width - right_width).max(180.0);
        let row_height = ART_SIZE;

        // Left: album art + track info.
        ui.allocate_ui_with_layout(
            egui::vec2(side_width, row_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                match &app.player.current {
                    Some(track) => {
                        album_art(ui, ctx, track.album_art_url.as_deref());
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&track.title).strong().size(14.0));
                            ui.label(
                                egui::RichText::new(track.artists_display())
                                    .small()
                                    .color(theme::TEXT_DIM),
                            );
                        });
                    }
                    None => {
                        art_placeholder(ui);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("—")
                                    .strong()
                                    .size(14.0)
                                    .color(theme::TEXT_DIM),
                            );
                        });
                    }
                }
            },
        );

        // Center: transport controls + seek bar.
        ui.allocate_ui_with_layout(
            egui::vec2(center_width, row_height),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                ui.horizontal(|ui| {
                    transport_controls(ui, app);
                });
                ui.horizontal(|ui| {
                    seek_bar(ui, app);
                });
            },
        );

        // Right: volume + position/duration.
        ui.allocate_ui_with_layout(
            egui::vec2(right_width, row_height),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                position_display(ui, app);
                ui.add_space(8.0);
                volume_control(ui, app);
            },
        );
    });
}

/// Shuffle / previous / play-pause / next / repeat.
fn transport_controls(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    let shuffle_color = if app.player.shuffle {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    if ui
        .add(
            egui::Button::new(egui::RichText::new("🔀").size(15.0).color(shuffle_color))
                .frame(false),
        )
        .on_hover_text("Shuffle")
        .clicked()
    {
        app.send(UiCommand::ToggleShuffle);
    }

    if ui
        .add(egui::Button::new(egui::RichText::new("⏮").size(16.0)).frame(false))
        .on_hover_text("Previous")
        .clicked()
    {
        app.send(UiCommand::Previous);
    }

    let play_glyph = if app.player.playing { "⏸" } else { "▶" };
    if ui
        .add(egui::Button::new(egui::RichText::new(play_glyph).size(18.0)).frame(false))
        .on_hover_text(if app.player.playing { "Pause" } else { "Play" })
        .clicked()
    {
        app.send(UiCommand::TogglePlay);
    }

    if ui
        .add(egui::Button::new(egui::RichText::new("⏭").size(16.0)).frame(false))
        .on_hover_text("Next")
        .clicked()
    {
        app.send(UiCommand::Next);
    }

    let (repeat_glyph, repeat_color) = match app.player.repeat {
        RepeatMode::Off => ("🔁", theme::TEXT_DIM),
        RepeatMode::All => ("🔁", theme::ACCENT),
        RepeatMode::One => ("🔂", theme::ACCENT),
    };
    let hover = match app.player.repeat {
        RepeatMode::Off => "Repeat: off",
        RepeatMode::All => "Repeat: context",
        RepeatMode::One => "Repeat: track",
    };
    if ui
        .add(
            egui::Button::new(egui::RichText::new(repeat_glyph).size(15.0).color(repeat_color))
                .frame(false),
        )
        .on_hover_text(hover)
        .clicked()
    {
        app.send(UiCommand::CycleRepeat);
    }
}

/// Seek slider with current-position and duration labels; seeks on release.
fn seek_bar(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    let Some(track) = &app.player.current else {
        return;
    };
    let duration_ms = track.duration_ms;

    let mut position = locked_copy(&SEEK_OVERRIDE).unwrap_or_else(|| live_position_ms(app));
    ui.label(
        egui::RichText::new(format_duration_ms(position))
            .small()
            .color(theme::TEXT_DIM),
    );
    let slider_width = (ui.available_width() - 56.0).max(60.0);
    let response = ui.add_sized(
        [slider_width, 16.0],
        egui::Slider::new(&mut position, 0..=duration_ms)
            .show_value(false)
            .trailing_fill(true),
    );
    ui.label(
        egui::RichText::new(format_duration_ms(duration_ms))
            .small()
            .color(theme::TEXT_DIM),
    );

    if response.dragged() {
        if let Ok(mut guard) = SEEK_OVERRIDE.lock() {
            *guard = Some(position);
        }
    }
    if response.drag_stopped() || (response.changed() && !response.dragged()) {
        app.send(UiCommand::SeekMs(position));
        if let Ok(mut guard) = SEEK_OVERRIDE.lock() {
            *guard = None;
        }
    }
}

/// Volume slider in percent, mapped to librespot mixer units.
fn volume_control(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    let snapshot_percent = ((app.player.volume as u32) * 100 / 65_535) as u16;
    let mut percent = locked_copy(&VOLUME_OVERRIDE).unwrap_or(snapshot_percent);

    ui.label(
        egui::RichText::new(format!("{percent}%"))
            .small()
            .color(theme::TEXT_DIM),
    );
    let response = ui.add_sized(
        [90.0, 16.0],
        egui::Slider::new(&mut percent, 0..=100)
            .show_value(false)
            .trailing_fill(true),
    );
    ui.label(
        egui::RichText::new(if percent == 0 { "🔇" } else { "🔊" })
            .size(14.0)
            .color(theme::TEXT_DIM),
    );

    if response.changed() {
        if let Ok(mut guard) = VOLUME_OVERRIDE.lock() {
            *guard = Some(percent);
        }
        app.send(UiCommand::SetVolume(
            ((percent as u32) * 65_535 / 100) as u16,
        ));
    }
    // Clear the override once the backend echoes back a value within ±1:
    // both sides floor (51% -> 50 echo -> 49 displayed), so exact equality
    // never fires and external volume changes would stop being reflected.
    if !response.dragged() && snapshot_percent.abs_diff(percent) <= 1 {
        if let Ok(mut guard) = VOLUME_OVERRIDE.lock() {
            *guard = None;
        }
    }
}

/// `current / duration` text on the right side of the bar.
fn position_display(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    let (position, duration) = match &app.player.current {
        Some(track) => (live_position_ms(app), track.duration_ms),
        None => (0, 0),
    };
    ui.label(
        egui::RichText::new(format!(
            "{} / {}",
            format_duration_ms(position),
            format_duration_ms(duration)
        ))
        .small()
        .color(theme::TEXT_DIM),
    );
}

fn locked_copy<T: Copy>(lock: &Mutex<Option<T>>) -> Option<T> {
    lock.lock().ok().and_then(|guard| *guard)
}

/// Effective playback position in milliseconds: interpolated from the live
/// position anchor when one is advancing, otherwise the snapshot value.
fn live_position_ms(app: &SpotifyLiteApp) -> u32 {
    match &app.player.position_anchor {
        Some(anchor) if anchor.speed > 0.0 => anchor.position_at(now_unix_ms()),
        _ => app.player.position_ms,
    }
}

/// Current Unix time in milliseconds (0 when the system clock predates the
/// epoch).
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Album art
// ---------------------------------------------------------------------------

/// Draws the current track's album art, fetching and caching it on first use.
fn album_art(ui: &mut egui::Ui, ctx: &egui::Context, url: Option<&str>) {
    match url.filter(|url| !url.is_empty()) {
        Some(url) => match art_texture(ctx, url) {
            Some(texture) => {
                ui.image((texture.id(), egui::vec2(ART_SIZE, ART_SIZE)));
            }
            None => art_placeholder(ui),
        },
        None => art_placeholder(ui),
    }
}

fn art_placeholder(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ART_SIZE, ART_SIZE), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme::PANEL);
    ui.painter().rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x2A, 0x2A, 0x2A)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "♪",
        egui::FontId::proportional(16.0),
        theme::TEXT_DIM,
    );
}

/// Returns the cached texture for `url`, starting a fetch when needed.
fn art_texture(ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
    use std::collections::hash_map::Entry;

    let mut cache = ART_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().ok()?;
    match cache.entry(url.to_string()) {
        Entry::Occupied(entry) => match entry.get() {
            ArtState::Ready(texture) => Some(texture.clone()),
            ArtState::Loading(rx) => match rx.try_recv() {
                Ok(Ok(image)) => {
                    let texture = ctx.load_texture(
                        format!("album_art:{url}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [image.width as usize, image.height as usize],
                            &image.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    *entry.into_mut() = ArtState::Ready(texture.clone());
                    Some(texture)
                }
                Ok(Err(_)) => {
                    *entry.into_mut() = ArtState::Failed;
                    None
                }
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    *entry.into_mut() = ArtState::Failed;
                    None
                }
            },
            ArtState::Failed => None,
        },
        Entry::Vacant(entry) => {
            let (tx, rx) = mpsc::channel();
            spawn_art_fetch(url.to_string(), tx);
            entry.insert(ArtState::Loading(rx));
            None
        }
    }
}

/// Spawns a worker thread that downloads and decodes the cover image; the
/// pixel data comes back through the channel (texture creation must happen on
/// the UI thread).
fn spawn_art_fetch(url: String, tx: mpsc::Sender<Result<ArtImage, String>>) {
    std::thread::spawn(move || {
        let _ = tx.send(fetch_art(&url));
    });
}

fn fetch_art(url: &str) -> Result<ArtImage, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bytes = reqwest::get(url)
            .await
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let dyn_img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let rgba = dyn_img.thumbnail(64, 64).to_rgba8();
        Ok(ArtImage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    })
}
