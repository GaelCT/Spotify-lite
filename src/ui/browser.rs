//! Central panel content for every browser-style screen (home, search,
//! library lists and detail views).
//!
//! All rendering is driven by state the backend has already delivered; when
//! no data is available yet the screens show explicit empty-state labels.

use std::sync::Mutex;

use crate::app::SpotifyLiteApp;
use crate::types::{AlbumDetails, ArtistDetails, PlaylistInfo, Screen, Track, UiCommand};
use crate::ui::theme;

/// Currently highlighted row of a track list, keyed by list so each screen
/// keeps its own selection ("search", "playlist:{id}", …).
static SELECTED_ROW: Mutex<Option<(String, usize)>> = Mutex::new(None);

/// Routes to the screen renderer matching `app.screen`.
pub fn draw(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    match app.screen.clone() {
        Screen::Home => home(ui, app),
        Screen::Search => search(ui, app),
        Screen::Playlists => playlist_list(ui, app),
        Screen::Playlist(id) => playlist_detail(ui, app, &id),
        Screen::Albums => album_list(ui, app),
        Screen::Album(id) => album_detail(ui, app, &id),
        Screen::Artists => artist_list(ui, app),
        Screen::Artist(id) => artist_detail(ui, app, &id),
        Screen::Queue => queue_screen(ui, app),
        // Login and Settings are rendered by their own modules.
        Screen::Login | Screen::Settings => {}
    }
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

fn home(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading(greeting());
    ui.add_space(8.0);

    let playlists = app.playlists.clone();
    section(ui, "Your Playlists", |ui| {
        if playlists.is_empty() {
            empty_state(ui, "Connect to load your library.");
            return;
        }
        card_grid(ui, "home_playlists", &playlists, 4, |ui, playlist| {
            let response = card(ui, 150.0, 168.0, |ui| {
                cover_tile(ui, 120.0);
                ui.label(egui::RichText::new(&playlist.name).strong().size(14.0));
                ui.label(
                    egui::RichText::new(format!("{} tracks", playlist.tracks_total))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            });
            if response.clicked() {
                open_playlist(app, &playlist.id);
            }
        });
    });

    let albums = app.albums.clone();
    section(ui, "Recently saved albums", |ui| {
        if albums.is_empty() {
            empty_state(ui, "No saved albums yet.");
            return;
        }
        card_grid(ui, "home_albums", &albums, 4, |ui, album| {
            let response = card(ui, 150.0, 168.0, |ui| {
                cover_tile(ui, 120.0);
                ui.label(egui::RichText::new(&album.name).strong().size(14.0));
                ui.label(
                    egui::RichText::new(album.artists.join(", "))
                        .small()
                        .color(theme::TEXT_DIM),
                );
            });
            if response.clicked() {
                open_album(app, &album.id);
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn search(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading("Search");
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut submitted = false;
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.search_query)
                .hint_text("Search tracks, albums, artists, playlists")
                .desired_width(360.0),
        );
        submitted = ui.button("Search").clicked() || (response.lost_focus() && enter);
    });
    ui.add_space(8.0);

    let query = app.search_query.trim().to_string();
    if submitted && !query.is_empty() {
        app.send(UiCommand::Search(query.clone()));
    }

    let Some(results) = app.search_results.clone() else {
        empty_state(ui, "Search for tracks, albums, artists and playlists.");
        return;
    };
    if results.tracks.is_empty()
        && results.albums.is_empty()
        && results.artists.is_empty()
        && results.playlists.is_empty()
    {
        empty_state(ui, &format!("No results for “{query}”."));
        return;
    }

    egui::ScrollArea::vertical()
        .id_salt("search_results")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            section(ui, "Tracks", |ui| {
                if results.tracks.is_empty() {
                    empty_state(ui, "No matching tracks.");
                    return;
                }
                track_table(
                    ui,
                    app,
                    "search",
                    &results.tracks,
                    TableOpts {
                        show_album: true,
                        click_plays: false,
                        context_uri: None,
                    },
                );
            });

            section(ui, "Albums", |ui| {
                if results.albums.is_empty() {
                    empty_state(ui, "No matching albums.");
                    return;
                }
                card_grid(ui, "search_albums", &results.albums, 4, |ui, album| {
                    let response = card(ui, 150.0, 168.0, |ui| {
                        cover_tile(ui, 120.0);
                        ui.label(egui::RichText::new(&album.name).strong().size(14.0));
                        ui.label(
                            egui::RichText::new(album.artists.join(", "))
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                    if response.clicked() {
                        open_album(app, &album.id);
                    }
                });
            });

            section(ui, "Artists", |ui| {
                if results.artists.is_empty() {
                    empty_state(ui, "No matching artists.");
                    return;
                }
                card_grid(ui, "search_artists", &results.artists, 4, |ui, artist| {
                    let response = card(ui, 150.0, 168.0, |ui| {
                        cover_tile(ui, 120.0);
                        ui.label(egui::RichText::new(&artist.name).strong().size(14.0));
                        ui.label(
                            egui::RichText::new(format!("{} followers", artist.followers))
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                    if response.clicked() {
                        open_artist(app, &artist.id);
                    }
                });
            });

            section(ui, "Playlists", |ui| {
                if results.playlists.is_empty() {
                    empty_state(ui, "No matching playlists.");
                    return;
                }
                card_grid(ui, "search_playlists", &results.playlists, 4, |ui, playlist| {
                    let response = card(ui, 150.0, 168.0, |ui| {
                        cover_tile(ui, 120.0);
                        ui.label(egui::RichText::new(&playlist.name).strong().size(14.0));
                        ui.label(
                            egui::RichText::new(format!("{} tracks", playlist.tracks_total))
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                    if response.clicked() {
                        open_playlist(app, &playlist.id);
                    }
                });
            });
        });

    // Enter with an empty search box plays the highlighted search result.
    if enter && query.is_empty() {
        if let Some(index) = selected_row("search") {
            if let Some(track) = results.tracks.get(index) {
                app.send(UiCommand::PlayTrack {
                    track_uri: track.uri.clone(),
                    context_uri: None,
                    queue: results.tracks[index + 1..]
                        .iter()
                        .map(|t| t.uri.clone())
                        .collect(),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Library lists
// ---------------------------------------------------------------------------

fn playlist_list(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading("Your Playlists");
    let playlists = app.playlists.clone();
    if playlists.is_empty() {
        empty_state(ui, "No playlists loaded yet. Connect to load your library.");
        if ui.button("Refresh library").clicked() {
            app.send(UiCommand::LoadLibrary);
        }
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("playlist_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for playlist in &playlists {
                if library_row(ui, &playlist.name, &playlist_subtitle(playlist), false).clicked() {
                    open_playlist(app, &playlist.id);
                }
            }
        });
}

fn album_list(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading("Albums");
    let albums = app.albums.clone();
    if albums.is_empty() {
        empty_state(ui, "No albums loaded yet. Connect to load your library.");
        if ui.button("Refresh library").clicked() {
            app.send(UiCommand::LoadLibrary);
        }
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("album_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for album in &albums {
                let year = album.release_date.chars().take(4).collect::<String>();
                let subtitle = format!("{} • {}", album.artists.join(", "), year);
                if library_row(ui, &album.name, &subtitle, false).clicked() {
                    open_album(app, &album.id);
                }
            }
        });
}

fn artist_list(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    ui.heading("Artists");
    let artists = app.artists.clone();
    if artists.is_empty() {
        empty_state(ui, "No artists loaded yet. Connect to load your library.");
        if ui.button("Refresh library").clicked() {
            app.send(UiCommand::LoadLibrary);
        }
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("artist_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for artist in &artists {
                let subtitle = format!("{} followers", artist.followers);
                if library_row(ui, &artist.name, &subtitle, false).clicked() {
                    open_artist(app, &artist.id);
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Detail screens
// ---------------------------------------------------------------------------

fn playlist_detail(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, id: &str) {
    let info = app.playlists.iter().find(|p| p.id == id).cloned();
    let title = info
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Playlist".to_string());
    let tracks = app.playlist_tracks.get(id).cloned().unwrap_or_default();

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("PLAYLIST").small().color(theme::ACCENT));
            ui.heading(&title);
            if let Some(info) = &info {
                ui.label(
                    egui::RichText::new(format!("{} • {} tracks", info.owner, info.tracks_total))
                        .color(theme::TEXT_DIM),
                );
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(egui::RichText::new("▶ Play").size(15.0)))
                .clicked()
            {
                // Empty queue: Soloist expands the whole context server-side;
                // re-enqueueing the visible list would double every track.
                app.send(UiCommand::PlayContext {
                    context_uri: format!("spotify:playlist:{id}"),
                    track_uri: None,
                    queue: Vec::new(),
                });
            }
        });
    });
    ui.add_space(6.0);

    if tracks.is_empty() {
        empty_state(ui, "No tracks loaded yet.");
        if ui.button("Reload").clicked() {
            app.send(UiCommand::LoadPlaylistTracks(id.to_string()));
        }
        return;
    }
    track_table(
        ui,
        app,
        &format!("playlist:{id}"),
        &tracks,
        TableOpts {
            show_album: true,
            click_plays: false,
            context_uri: Some(format!("spotify:playlist:{id}")),
        },
    );
}

fn album_detail(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, id: &str) {
    let details = app
        .album_details
        .clone()
        .filter(|details| details.info.id == id);

    let Some(details) = details else {
        ui.heading("Album");
        empty_state(ui, "No album loaded yet.");
        if ui.button("Reload").clicked() {
            app.send(UiCommand::LoadAlbum(id.to_string()));
        }
        return;
    };
    album_view(ui, app, &details);
}

fn album_view(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, details: &AlbumDetails) {
    ui.horizontal(|ui| {
        cover_tile(ui, 96.0);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("ALBUM").small().color(theme::ACCENT));
            ui.heading(&details.info.name);
            ui.label(
                egui::RichText::new(details.info.artists.join(", ")).color(theme::TEXT_DIM),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} • {} tracks",
                    details.info.release_date, details.info.total_tracks
                ))
                .small()
                .color(theme::TEXT_DIM),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(egui::RichText::new("▶ Play").size(15.0)))
                .clicked()
            {
                // Same as the playlist header: the context expands
                // server-side, so the visible list must not be re-enqueued.
                app.send(UiCommand::PlayContext {
                    context_uri: format!("spotify:album:{}", details.info.id),
                    track_uri: None,
                    queue: Vec::new(),
                });
            }
        });
    });
    ui.add_space(6.0);

    if details.tracks.is_empty() {
        empty_state(ui, "No tracks on this album.");
        return;
    }
    track_table(
        ui,
        app,
        &format!("album:{}", details.info.id),
        &details.tracks,
        TableOpts {
            show_album: false,
            click_plays: false,
            context_uri: Some(format!("spotify:album:{}", details.info.id)),
        },
    );
}

fn artist_detail(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, id: &str) {
    let details = app
        .artist_details
        .clone()
        .filter(|details| details.info.id == id);

    let Some(details) = details else {
        ui.heading("Artist");
        empty_state(ui, "No artist loaded yet.");
        if ui.button("Reload").clicked() {
            app.send(UiCommand::LoadArtist(id.to_string()));
        }
        return;
    };
    artist_view(ui, app, &details);
}

fn artist_view(ui: &mut egui::Ui, app: &mut SpotifyLiteApp, details: &ArtistDetails) {
    ui.horizontal(|ui| {
        cover_tile(ui, 96.0);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("ARTIST").small().color(theme::ACCENT));
            ui.heading(&details.info.name);
            ui.label(
                egui::RichText::new(format!("{} followers", details.info.followers))
                    .color(theme::TEXT_DIM),
            );
        });
    });
    ui.add_space(6.0);

    egui::ScrollArea::vertical()
        .id_salt(format!("artist:{}", details.info.id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            section(ui, "Top Tracks", |ui| {
                if details.top_tracks.is_empty() {
                    empty_state(ui, "No top tracks here yet.");
                    return;
                }
                track_table(
                    ui,
                    app,
                    &format!("artist:{}", details.info.id),
                    &details.top_tracks,
                    TableOpts {
                        show_album: true,
                        click_plays: true,
                        context_uri: Some(format!("spotify:artist:{}", details.info.id)),
                    },
                );
            });
            section(ui, "Albums", |ui| {
                if details.albums.is_empty() {
                    empty_state(ui, "No albums here yet.");
                    return;
                }
                card_grid(ui, "artist_albums", &details.albums, 4, |ui, album| {
                    let response = card(ui, 150.0, 168.0, |ui| {
                        cover_tile(ui, 120.0);
                        ui.label(egui::RichText::new(&album.name).strong().size(14.0));
                        ui.label(
                            egui::RichText::new(&album.release_date)
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                    if response.clicked() {
                        open_album(app, &album.id);
                    }
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// The up-next queue reported by the backend (populated when Spotify Soloist
/// drives playback), rendered as a read-only track table.
fn queue_screen(ui: &mut egui::Ui, app: &mut SpotifyLiteApp) {
    use egui_extras::{Column, TableBuilder};

    ui.heading("Queue");
    let upcoming = app.player.upcoming.clone();
    if upcoming.is_empty() {
        empty_state(ui, "Nothing queued — start playback to see what's up next.");
        return;
    }

    TableBuilder::new(ui)
        .column(Column::initial(30.0).at_least(26.0))
        .column(Column::remainder().at_least(150.0).clip(true))
        .column(Column::initial(130.0).at_least(90.0).clip(true))
        .header(22.0, |mut header| {
            header.col(|ui| {
                dim_heading(ui, "#");
            });
            header.col(|ui| {
                dim_heading(ui, "Title");
            });
            header.col(|ui| {
                dim_heading(ui, "Artist");
            });
        })
        .body(|body| {
            body.rows(26.0, upcoming.len(), |mut row| {
                let index = row.index();
                let Some(track) = upcoming.get(index) else {
                    return;
                };

                row.col(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}", index + 1))
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                });
                row.col(|ui| {
                    ui.label(egui::RichText::new(&track.title).size(14.0));
                });
                row.col(|ui| {
                    ui.label(
                        egui::RichText::new(track.artists_display())
                            .size(14.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Track table
// ---------------------------------------------------------------------------

/// Behavior switches for [`track_table`].
struct TableOpts {
    show_album: bool,
    /// Artist top tracks play on a single click instead of only on double-click.
    click_plays: bool,
    context_uri: Option<String>,
}

/// A Spotify-style track table: `# | Title | Artist | [Album] | ♡ | Time`.
///
/// Double-clicking a row plays it (single-click when `opts.click_plays`),
/// right-clicking toggles Liked Songs, and clicking highlights the row.
fn track_table(
    ui: &mut egui::Ui,
    app: &mut SpotifyLiteApp,
    key: &str,
    tracks: &[Track],
    opts: TableOpts,
) {
    use egui_extras::{Column, TableBuilder};

    let selected = selected_row(key);
    let mut play_index: Option<usize> = None;
    let mut toggle_save: Option<String> = None;
    let mut queue_uri: Option<String> = None;

    let builder = TableBuilder::new(ui)
        .column(Column::initial(30.0).at_least(26.0))
        .column(Column::remainder().at_least(150.0).clip(true))
        .column(Column::initial(130.0).at_least(90.0).clip(true));
    let builder = if opts.show_album {
        builder
            .column(Column::initial(150.0).at_least(100.0).clip(true))
            .column(Column::initial(36.0).at_least(32.0))
            .column(Column::initial(26.0).at_least(22.0))
            .column(Column::initial(58.0).at_least(50.0))
    } else {
        builder
            .column(Column::initial(36.0).at_least(32.0))
            .column(Column::initial(26.0).at_least(22.0))
            .column(Column::initial(58.0).at_least(50.0))
    };

    builder
        .header(22.0, |mut header| {
            header.col(|ui| {
                dim_heading(ui, "#");
            });
            header.col(|ui| {
                dim_heading(ui, "Title");
            });
            header.col(|ui| {
                dim_heading(ui, "Artist");
            });
            if opts.show_album {
                header.col(|ui| {
                    dim_heading(ui, "Album");
                });
            }
            header.col(|_| {});
            header.col(|_| {});
            header.col(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    dim_heading(ui, "Time");
                });
            });
        })
        .body(|body| {
            body.rows(26.0, tracks.len(), |mut row| {
                let index = row.index();
                let Some(track) = tracks.get(index) else {
                    return;
                };
                row.set_selected(selected == Some(index));

                row.col(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}", index + 1))
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                });
                row.col(|ui| {
                    ui.label(egui::RichText::new(&track.title).size(14.0));
                });
                row.col(|ui| {
                    ui.label(
                        egui::RichText::new(track.artists_display())
                            .size(14.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                if opts.show_album {
                    row.col(|ui| {
                        ui.label(
                            egui::RichText::new(&track.album)
                                .size(14.0)
                                .color(theme::TEXT_DIM),
                        );
                    });
                }
                row.col(|ui| {
                    let (glyph, color) = if track.is_saved {
                        ("♥", theme::ACCENT)
                    } else {
                        ("♡", theme::TEXT_DIM)
                    };
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(glyph).color(color))
                                .frame(false),
                        )
                        .on_hover_text("Toggle Liked Songs")
                        .clicked()
                    {
                        toggle_save = Some(track.id.clone());
                    }
                });
                row.col(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("+").size(14.0).color(theme::TEXT_DIM),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Add to queue")
                        .clicked()
                    {
                        queue_uri = Some(track.uri.clone());
                    }
                });
                row.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(track.duration_display())
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    });
                });

                let response = row.response();
                if response.clicked() {
                    select_row(key, index);
                    if opts.click_plays {
                        play_index = Some(index);
                    }
                }
                if response.double_clicked() {
                    play_index = Some(index);
                }
                if response.secondary_clicked() {
                    toggle_save = Some(track.id.clone());
                }
            });
        });

    if let Some(index) = play_index {
        if let Some(track) = tracks.get(index) {
            app.send(UiCommand::PlayTrack {
                track_uri: track.uri.clone(),
                context_uri: opts.context_uri.clone(),
                queue: tracks[index + 1..]
                    .iter()
                    .map(|t| t.uri.clone())
                    .collect(),
            });
        }
    }
    if let Some(track_id) = toggle_save {
        app.send(UiCommand::ToggleSaveTrack(track_id));
    }
    if let Some(uri) = queue_uri {
        app.send(UiCommand::QueueTrack(uri));
    }
}

// ---------------------------------------------------------------------------
// Shared widgets
// ---------------------------------------------------------------------------

/// A titled group.
fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(title).strong().size(17.0));
    ui.add_space(4.0);
    add_contents(ui);
}

/// Dim helper text used for empty states.
fn empty_state(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(theme::TEXT_DIM));
}

/// Small grey column heading for the track table.
fn dim_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme::TEXT_DIM));
}

/// A grid of clickable cards.
fn card_grid<T>(
    ui: &mut egui::Ui,
    id: &str,
    items: &[T],
    columns: usize,
    mut card_contents: impl FnMut(&mut egui::Ui, &T),
) {
    egui::Grid::new(id)
        .num_columns(columns)
        .spacing(egui::vec2(12.0, 12.0))
        .show(ui, |ui| {
            for item in items {
                card_contents(ui, item);
            }
        });
}

/// One clickable card of a fixed size; draws a raised background and returns
/// the click response.
fn card(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if response.hovered() {
            egui::Color32::from_rgb(0x24, 0x24, 0x24)
        } else {
            egui::Color32::from_rgb(0x1A, 0x1A, 0x1A)
        };
        ui.painter().rect_filled(rect, 6.0, fill);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(rect.shrink(10.0)),
            contents,
        );
    }
    response
}

/// A square cover placeholder with a musical note glyph.
fn cover_tile(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
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
        egui::FontId::proportional(size * 0.3),
        theme::TEXT_DIM,
    );
}

/// One row of a library list: title on the left, subtitle on the right.
fn library_row(ui: &mut egui::Ui, title: &str, subtitle: &str, selected: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        let response = ui.selectable_label(selected, egui::RichText::new(title).size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(subtitle).small().color(theme::TEXT_DIM));
        });
        response
    })
    .inner
}

/// The subtitle shown under a playlist in list views.
fn playlist_subtitle(playlist: &PlaylistInfo) -> String {
    match &playlist.description {
        Some(description) if !description.is_empty() => {
            format!("{} • {} tracks", description, playlist.tracks_total)
        }
        _ => format!("{} tracks", playlist.tracks_total),
    }
}

/// Hour of the day (UTC) for the home greeting.
fn hour_of_day() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    ((secs / 3600) % 24) as u32
}

fn greeting() -> String {
    let part = match hour_of_day() {
        5..=11 => "morning",
        12..=17 => "afternoon",
        _ => "evening",
    };
    format!("Good {part}")
}

// ---------------------------------------------------------------------------
// Navigation helpers
// ---------------------------------------------------------------------------

fn open_playlist(app: &mut SpotifyLiteApp, id: &str) {
    app.screen = Screen::Playlist(id.to_string());
    app.send(UiCommand::LoadPlaylistTracks(id.to_string()));
}

fn open_album(app: &mut SpotifyLiteApp, id: &str) {
    app.screen = Screen::Album(id.to_string());
    app.send(UiCommand::LoadAlbum(id.to_string()));
}

fn open_artist(app: &mut SpotifyLiteApp, id: &str) {
    app.screen = Screen::Artist(id.to_string());
    app.send(UiCommand::LoadArtist(id.to_string()));
}

fn selected_row(key: &str) -> Option<usize> {
    SELECTED_ROW
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .and_then(|(k, index)| (k == key).then_some(index))
}

fn select_row(key: &str, index: usize) {
    if let Ok(mut guard) = SELECTED_ROW.lock() {
        *guard = Some((key.to_string(), index));
    }
}
