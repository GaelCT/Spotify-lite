//! Login screen: client ID/secret form that kicks off the OAuth flow.

use crate::app::SpotifyLiteApp;
use crate::types::UiCommand;
use crate::ui::theme;

/// Draws the "Connect to Spotify" form.
pub fn draw(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.heading("Connect to Spotify");
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "spotify-lite needs a Spotify Premium account and your own client ID\n\
                 from the Spotify Developer Dashboard. Your browser will open to approve\n\
                 access; the redirect goes to http://127.0.0.1:8888/login.",
            )
            .color(theme::TEXT_DIM),
        );
        ui.add_space(24.0);

        ui.set_width(380.0);
        ui.label(egui::RichText::new("Client ID").small().color(theme::TEXT_DIM));
        ui.text_edit_singleline(&mut app.login_client_id);
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("Client secret (optional)")
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.add(
            egui::TextEdit::singleline(&mut app.login_client_secret).password(true),
        );
        ui.add_space(20.0);

        if ui
            .add(egui::Button::new(
                egui::RichText::new("Connect with Spotify").size(16.0),
            ))
            .clicked()
        {
            connect(app);
        }

        if let Some(message) = &app.status_message {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(message).color(theme::TEXT_DIM));
        }
    });
}

/// Validates the form, persists the credentials and starts the OAuth flow.
fn connect(app: &mut SpotifyLiteApp) {
    let client_id = app.login_client_id.trim().to_string();
    if client_id.is_empty() {
        app.status_message = Some("Client ID is required.".to_string());
        return;
    }
    let client_secret = {
        let secret = app.login_client_secret.trim();
        if secret.is_empty() {
            None
        } else {
            Some(secret.to_string())
        }
    };

    app.config.client_id = Some(client_id.clone());
    app.config.client_secret = client_secret.clone();
    app.config.save();

    app.send(UiCommand::Login {
        client_id,
        client_secret,
    });
    app.status_message = Some("Waiting for browser…".to_string());
}
