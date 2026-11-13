//! Settings screen: shows the current account and playback state and edits
//! `app.config` in place, persisting it via `Config::save`.

use crate::app::SpotifyLiteApp;
use crate::types::{Screen, UiCommand};
use crate::ui::sidebar;
use crate::ui::theme;

/// Draws the settings form.
pub fn draw(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading("Settings");
    ui.add_space(12.0);
    ui.set_max_width(460.0);

    // Account ----------------------------------------------------------------
    ui.label(egui::RichText::new("ACCOUNT").small().color(theme::TEXT_DIM));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Current user:");
        match sidebar::current_user(app) {
            Some(name) => {
                ui.label(egui::RichText::new(name).strong().color(theme::ACCENT));
            }
            None => {
                ui.label(egui::RichText::new("Not connected").color(theme::TEXT_DIM));
            }
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Refresh library").clicked() {
            app.send(UiCommand::LoadLibrary);
        }
        if sidebar::current_user(app).is_some() {
            if ui.button("Log out").clicked() {
                app.send(UiCommand::Logout);
            }
        } else if ui.button("Connect…").clicked() {
            app.screen = Screen::Login;
        }
    });

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    // Developer credentials ----------------------------------------------------
    ui.label(
        egui::RichText::new("DEVELOPER CREDENTIALS")
            .small()
            .color(theme::TEXT_DIM),
    );
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Client ID").small().color(theme::TEXT_DIM));
    let mut client_id = app.config.client_id.clone().unwrap_or_default();
    if ui.text_edit_singleline(&mut client_id).changed() {
        app.config.client_id = if client_id.is_empty() {
            None
        } else {
            Some(client_id)
        };
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Client secret (optional, unused by the PKCE flow)")
            .small()
            .color(theme::TEXT_DIM),
    );
    let mut client_secret = app.config.client_secret.clone().unwrap_or_default();
    if ui
        .add(egui::TextEdit::singleline(&mut client_secret).password(true))
        .changed()
    {
        app.config.client_secret = if client_secret.is_empty() {
            None
        } else {
            Some(client_secret)
        };
    }

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    // Soloist ------------------------------------------------------------------
    ui.label(egui::RichText::new("SOLOIST").small().color(theme::TEXT_DIM));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("WebSocket address")
            .small()
            .color(theme::TEXT_DIM),
    );
    let mut soloist_ws_addr = app.config.soloist_ws_addr.clone().unwrap_or_default();
    if ui
        .add(
            egui::TextEdit::singleline(&mut soloist_ws_addr)
                .hint_text("WebSocket address of Spotify Soloist (takes effect after restart)"),
        )
        .changed()
    {
        app.config.soloist_ws_addr = if soloist_ws_addr.is_empty() {
            None
        } else {
            Some(soloist_ws_addr)
        };
    }

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    // Playback ---------------------------------------------------------------
    ui.label(egui::RichText::new("PLAYBACK").small().color(theme::TEXT_DIM));
    ui.add_space(4.0);
    let volume_percent = ((app.player.volume as u32) * 100 / 65_535) as u16;
    ui.label(
        egui::RichText::new(format!("Current volume: {volume_percent}%"))
            .color(theme::TEXT_DIM),
    );
    ui.label(
        egui::RichText::new("Volume is adjusted with the slider in the now-playing bar.")
            .small()
            .color(theme::TEXT_DIM),
    );

    ui.add_space(20.0);
    if ui.button("Save").clicked() {
        app.config.save();
        app.status_message = Some("Settings saved.".to_string());
    }
}
