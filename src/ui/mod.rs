//! egui layout: sidebar on the left, content in the center, now-playing bar
//! at the bottom and a status line at the bottom of the content area.

pub mod theme;

mod browser;
mod login;
mod now_playing;
mod settings;
mod sidebar;

use crate::app::SpotifyLiteApp;
use crate::types::Screen;

/// Draws the full window for one frame.
pub fn draw(ctx: &egui::Context, app: &mut SpotifyLiteApp) {
    egui::SidePanel::left("sidebar")
        .resizable(false)
        .exact_width(200.0)
        .show(ctx, |ui| sidebar::draw(ui, app));

    egui::CentralPanel::default().show(ctx, |ui| {
        // Both bottom bars live INSIDE the central panel: a ctx-level bottom
        // panel does not paint on some display stacks (observed on a
        // 150%-scaled VMware display), while in-panel bottom panels do.
        egui::TopBottomPanel::bottom("now_playing")
            .resizable(false)
            .exact_height(72.0)
            .show_inside(ui, |ui| now_playing::draw(ui, ctx, app));

        egui::TopBottomPanel::bottom("status_line")
            .resizable(false)
            .exact_height(22.0)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    // The playback-block banner outranks transient status
                    // messages: it means no track can ever start for this
                    // account, so it stays visible until logout.
                    if app.player.playback_blocked {
                        ui.label(
                            egui::RichText::new(crate::types::PLAYBACK_BLOCKED_MESSAGE)
                                .small()
                                .color(theme::WARNING),
                        );
                    } else if let Some(message) = &app.status_message {
                        ui.label(egui::RichText::new(message).small().color(theme::TEXT_DIM));
                    }
                });
            });

        match app.screen.clone() {
            Screen::Login => login::draw(ui, app),
            Screen::Settings => settings::draw(ui, app),
            _ => browser::draw(ui, app),
        }
    });
}
