//! The eframe application: owns all UI state, the command/event channels to
//! the backend worker, and the per-frame update loop.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::config::Config;
use crate::player::PlayerHandle;
use crate::types::*;
use crate::ui;

/// Root application state. Everything the UI renders lives here; the backend
/// lives behind `cmd_tx`/`event_rx` on the worker thread (kept alive by
/// `player_handle`).
pub struct SpotifyLiteApp {
    /// Loaded configuration (persisted via `Config::save`).
    pub config: Config,
    /// Command channel to the backend worker (UI -> backend).
    pub cmd_tx: Sender<UiCommand>,
    /// Event channel from the backend worker (backend -> UI).
    pub event_rx: Receiver<BackendEvent>,
    /// Keeps the backend worker alive for the lifetime of the app. Never read;
    /// dropping the handle would shut the worker down.
    #[allow(dead_code)]
    player_handle: PlayerHandle,
    /// Latest playback state snapshot, replaced on every
    /// `BackendEvent::PlayerSnapshot`.
    pub player: PlayerSnapshot,
    /// Currently displayed screen.
    pub screen: Screen,
    /// Display name of the authenticated user, if any.
    pub user_name: Option<String>,
    /// Whether a Spotify session is currently authenticated.
    pub authenticated: bool,
    /// Set at startup when a stored client ID plus a fresh cached token were
    /// found and a silent re-login was kicked off. The login screen can use
    /// this to show a "logging in…" state instead of the form.
    pub pending_auto_login: bool,
    /// User's playlists, filled by `BackendEvent::LibraryLoaded`.
    pub playlists: Vec<PlaylistInfo>,
    /// User's saved albums, filled by `BackendEvent::LibraryLoaded`.
    pub albums: Vec<AlbumInfo>,
    /// User's followed artists, filled by `BackendEvent::LibraryLoaded`.
    pub artists: Vec<ArtistInfo>,
    /// Tracks per playlist, filled by `BackendEvent::PlaylistTracksLoaded`.
    pub playlist_tracks: HashMap<String, Vec<Track>>,
    /// Details of the last loaded album (`Screen::Album`).
    pub album_details: Option<AlbumDetails>,
    /// Details of the last loaded artist (`Screen::Artist`).
    pub artist_details: Option<ArtistDetails>,
    /// Results of the last search (`Screen::Search`).
    pub search_results: Option<SearchResults>,
    /// Current text of the search box.
    pub search_query: String,
    /// Client ID field of the login form.
    pub login_client_id: String,
    /// Client secret field of the login form (currently unused by the flow).
    pub login_client_secret: String,
    /// Last status/error message to surface in the UI.
    pub status_message: Option<String>,
    /// Whether `status_message` currently holds an error. `BackendEvent::Info`
    /// must not clobber a pending error message.
    status_is_error: bool,
    /// One-shot flag: once the viewport's real outer rect is known, a
    /// window that hangs off the screen (small displays, DPI scaling)
    /// is shrunk and re-centered so the bottom bars are never off-screen.
    geometry_clamped: bool,
}

impl SpotifyLiteApp {
    /// Builds the app: applies the theme, loads config, opens the channels
    /// and spawns the backend worker.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::theme::apply(&cc.egui_ctx);

        let config = Config::load();
        let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>();
        let (event_tx, event_rx) = mpsc::channel::<BackendEvent>();
        let player_handle = PlayerHandle::spawn(cmd_rx, event_tx);

        // Auto-login: when a client ID is configured and a still-fresh cached
        // token exists, re-login silently at startup so the user lands on the
        // Home screen without seeing the browser flow again. The player worker
        // revalidates (and refreshes, once supported) the cached token; if it
        // turns out to be unusable the worker reports `AuthFailed` and the UI
        // falls back to the login form.
        let pending_auto_login =
            match (&config.client_id, crate::auth::AuthManager::new().cached_token()) {
                (Some(client_id), Some(_cached)) => {
                    let _ = cmd_tx.send(UiCommand::Login {
                        client_id: client_id.clone(),
                        client_secret: config.client_secret.clone(),
                    });
                    true
                }
                _ => false,
            };

        Self {
            login_client_id: config.client_id.clone().unwrap_or_default(),
            login_client_secret: config.client_secret.clone().unwrap_or_default(),
            config,
            cmd_tx,
            event_rx,
            player_handle,
            player: PlayerSnapshot::default(),
            screen: Screen::Login,
            user_name: None,
            authenticated: false,
            pending_auto_login,
            playlists: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
            playlist_tracks: HashMap::new(),
            album_details: None,
            artist_details: None,
            search_results: None,
            search_query: String::new(),
            status_message: Some("Not logged in".to_string()),
            status_is_error: false,
            geometry_clamped: false,
        }
    }

    /// Queues a command for the backend worker. Never blocks the UI.
    pub fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Applies one backend event to the UI state.
    fn apply_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::Authenticated { user_name } => {
                self.user_name = Some(user_name.clone());
                self.authenticated = true;
                self.pending_auto_login = false;
                self.status_is_error = false;
                self.status_message = Some(format!("Logged in as {user_name}"));
                self.screen = Screen::Home;
                self.send(UiCommand::LoadLibrary);
            }
            BackendEvent::AuthFailed(reason) => {
                self.authenticated = false;
                self.pending_auto_login = false;
                self.status_is_error = true;
                self.status_message = Some(format!("Login failed: {reason}"));
                // Screen stays on Login so the user can retry.
            }
            BackendEvent::LoggedOut => {
                // Make sure a logged-out session cannot silently resume.
                crate::auth::AuthManager::new().clear_cache();
                self.user_name = None;
                self.authenticated = false;
                self.playlists.clear();
                self.albums.clear();
                self.artists.clear();
                self.playlist_tracks.clear();
                self.album_details = None;
                self.artist_details = None;
                self.search_results = None;
                self.player = PlayerSnapshot::default();
                self.screen = Screen::Login;
                self.status_is_error = false;
                self.status_message = Some("Logged out".to_string());
            }
            BackendEvent::PlayerSnapshot(snapshot) => {
                self.player = snapshot;
            }
            BackendEvent::LibraryLoaded {
                playlists,
                albums,
                artists,
            } => {
                self.playlists = playlists;
                self.albums = albums;
                self.artists = artists;
            }
            BackendEvent::PlaylistTracksLoaded {
                playlist_id,
                tracks,
            } => {
                self.playlist_tracks.insert(playlist_id, tracks);
            }
            BackendEvent::AlbumLoaded(details) => {
                self.album_details = Some(details);
            }
            BackendEvent::ArtistLoaded(details) => {
                self.artist_details = Some(details);
            }
            BackendEvent::SearchLoaded(results) => {
                self.search_results = Some(results);
            }
            BackendEvent::TrackSaveUpdated { track_id, saved } => {
                let mut patch = |track: &mut Track| {
                    if track.id == track_id {
                        track.is_saved = saved;
                    }
                };
                for tracks in self.playlist_tracks.values_mut() {
                    tracks.iter_mut().for_each(&mut patch);
                }
                if let Some(details) = &mut self.album_details {
                    details.tracks.iter_mut().for_each(&mut patch);
                }
                if let Some(details) = &mut self.artist_details {
                    details.top_tracks.iter_mut().for_each(&mut patch);
                }
                if let Some(results) = &mut self.search_results {
                    results.tracks.iter_mut().for_each(&mut patch);
                }
                if let Some(track) = &mut self.player.current {
                    patch(track);
                }
            }
            BackendEvent::Error(message) => {
                self.status_is_error = true;
                self.status_message = Some(message);
            }
            BackendEvent::Info(message) => {
                // Low priority: never clobber an error that is still pending.
                if !self.status_is_error {
                    self.status_message = Some(message);
                }
            }
        }
    }
}

impl eframe::App for SpotifyLiteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain every pending backend event before drawing.
        while let Ok(event) = self.event_rx.try_recv() {
            self.apply_event(event);
        }

        // Login gate: an unauthenticated session may only show the login
        // screen (a login in progress already sits there).
        if !self.authenticated && self.screen != Screen::Login {
            self.screen = Screen::Login;
        }

        ui::draw(ctx, self);

        // Startup window-geometry rescue (runs once, when the real outer
        // rect is first reported). egui mixes units here: `outer_rect` is in
        // logical points, but `monitor_size` is in PHYSICAL pixels and
        // `native_pixels_per_point` is the display scale. On a small display
        // with scaling >100% (e.g. 1280×800 at 150%) a 1024×700 logical
        // request becomes a 1536×1050 physical window — the bottom bars get
        // cropped off the screen. Compare in PHYSICAL terms and shrink +
        // re-center when the window doesn't fit. No gratuitous resize:
        // post-creation resizes can desync the paint surface on some drivers.
        if !self.geometry_clamped {
            let (monitor_px, ppp, outer) = ctx.input(|i| {
                (
                    i.viewport().monitor_size,
                    i.raw.viewport().native_pixels_per_point,
                    i.viewport().outer_rect,
                )
            });
            if let Some(outer) = outer {
                self.geometry_clamped = true;
                let monitor_px = monitor_px.unwrap_or(egui::vec2(1280.0, 800.0));
                let ppp = ppp.unwrap_or(1.0).max(0.5);
                // Usable desktop in logical units (minus a taskbar margin).
                let usable = egui::vec2(monitor_px.x / ppp, monitor_px.y / ppp - 32.0);
                let fits = outer.left() >= 0.0
                    && outer.top() >= 0.0
                    && outer.width() * ppp <= monitor_px.x
                    && outer.height() * ppp <= monitor_px.y
                    && outer.bottom() <= usable.y;
                if !fits {
                    let size = egui::vec2(
                        outer.width().min(usable.x * 0.94),
                        outer.height().min(usable.y * 0.94),
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        egui::pos2(
                            (usable.x - size.x) / 2.0,
                            (usable.y - size.y) / 2.0,
                        ),
                    ));
                }
            }
        }

        // While playing, repaint periodically so the now-playing bar advances
        // between player snapshot events.
        if self.player.playing {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
