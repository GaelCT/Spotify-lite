//! Left navigation sidebar: app title, primary navigation, playlist shortcuts
//! and the connection identity pinned to the bottom.

use crate::app::SpotifyLiteApp;
use crate::types::{Screen, UiCommand};
use crate::ui::theme;

/// Draws the sidebar navigation.
pub fn draw(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("spotify-lite")
            .strong()
            .size(18.0)
            .color(theme::ACCENT),
    );
    ui.add_space(6.0);
    ui.separator();

    nav_button(ui, app, "Home", Screen::Home);
    nav_button(ui, app, "Search", Screen::Search);
    nav_button(ui, app, "Your Playlists", Screen::Playlists);
    nav_button(ui, app, "Albums", Screen::Albums);
    nav_button(ui, app, "Artists", Screen::Artists);
    nav_button(ui, app, "Queue", Screen::Queue);
    nav_button(ui, app, "Settings", Screen::Settings);

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("PLAYLISTS")
            .small()
            .color(theme::TEXT_DIM),
    );

    let playlists = app.playlists.clone();
    egui::ScrollArea::vertical()
        .id_salt("sidebar_playlists")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if playlists.is_empty() {
                ui.label(
                    egui::RichText::new("No playlists loaded.")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
            for playlist in &playlists {
                let selected = matches!(&app.screen, Screen::Playlist(id) if id == &playlist.id);
                if ui
                    .selectable_label(selected, egui::RichText::new(&playlist.name).size(14.0))
                    .clicked()
                {
                    let id = playlist.id.clone();
                    app.screen = Screen::Playlist(id.clone());
                    app.send(UiCommand::LoadPlaylistTracks(id));
                }
            }
        });

    // Pin the account identity to the bottom of the panel.
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add_space(6.0);
        match current_user(app) {
            Some(name) => {
                ui.label(egui::RichText::new(name).small().color(theme::TEXT));
            }
            None => {
                ui.label(
                    egui::RichText::new("Not connected")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
        }
    });
}

/// One navigation row: highlights while its screen — or a detail screen below
/// it — is active, and switches to it on click.
fn nav_button(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, label: &str, target: Screen) {
    let selected = is_active(&app.screen, &target);
    if ui
        .selectable_label(selected, egui::RichText::new(label).size(15.0))
        .clicked()
    {
        app.screen = target;
    }
}

/// Detail screens highlight their parent list entry in the nav.
fn is_active(current: &Screen, target: &Screen) -> bool {
    match (current, target) {
        (Screen::Playlist(_), Screen::Playlists)
        | (Screen::Album(_), Screen::Albums)
        | (Screen::Artist(_), Screen::Artists) => true,
        (a, b) => a == b,
    }
}

/// Display name of the currently logged-in user, if any.
pub(crate) fn current_user(app: &SpotifyLiteApp) -> Option<String> {
    app.user_name.clone()
}
