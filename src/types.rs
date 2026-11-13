//! Core shared types: THE CONTRACT between the UI layer and the backend layer.
//!
//! UI code (`crate::app`, `crate::ui`) and backend code (`crate::player`,
//! `crate::webapi`, `crate::auth`) communicate exclusively through
//! [`UiCommand`] (UI -> backend) and [`BackendEvent`] (backend -> UI).
//!
//! All data crossing that boundary is expressed in terms of the plain structs
//! below — never in terms of `rspotify` or `librespot` types. The backend
//! (`webapi.rs`) is responsible for mapping `rspotify` model types into these
//! structs; the UI never depends on `rspotify` or `librespot` types directly.
//!
//! SKELETON NOTE: `allow(dead_code)` is set module-wide because the backend
//! stubs do not construct these types yet; it should be removed once the
//! backend implementation lands.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// A single playable track, normalized from `rspotify::model::FullTrack` /
/// `rspotify::model::SimplifiedTrack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    /// Spotify track ID: the bare ID, not a URI (no `spotify:track:` prefix).
    pub id: String,
    /// Full Spotify URI, e.g. `spotify:track:4uLU6hMCjMI75M1A2tKUQC`.
    /// Pass this verbatim to [`UiCommand::PlayTrack`] / [`UiCommand::PlayContext`].
    pub uri: String,
    /// Track title.
    pub title: String,
    /// Display names of all artists. The UI joins these for display.
    pub artists: Vec<String>,
    /// Album name the track belongs to.
    pub album: String,
    /// URL of an album art image. The backend picks the *smallest* available
    /// image (last element of rspotify's `images` array). `None` when the
    /// track/album has no art.
    pub album_art_url: Option<String>,
    /// Track length in milliseconds.
    pub duration_ms: u32,
    /// 1-based track number within its disc.
    pub track_number: u32,
    /// 1-based disc number within the album.
    pub disc_number: u32,
    /// Whether the track is in the user's "Liked Songs" library.
    pub is_saved: bool,
}

impl Track {
    /// All artist names as a single display string, e.g. `"Artist A, Artist B"`.
    pub fn artists_display(&self) -> String {
        self.artists.join(", ")
    }

    /// Duration formatted as `m:ss`, e.g. `"3:45"`.
    pub fn duration_display(&self) -> String {
        format_duration_ms(self.duration_ms)
    }
}

/// Formats a millisecond duration as `m:ss`, e.g. `225000` -> `"3:45"`.
pub fn format_duration_ms(ms: u32) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1_000) % 60)
}

/// A playlist as listed in the user's library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistInfo {
    /// Spotify playlist ID (bare ID, not a URI).
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Playlist description, if any.
    pub description: Option<String>,
    /// Display name of the playlist owner.
    pub owner: String,
    /// Total number of tracks in the playlist.
    pub tracks_total: u32,
    /// URL of a playlist cover image, if any (smallest variant preferred).
    pub image_url: Option<String>,
}

/// An album as listed in the user's "Your Library".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumInfo {
    /// Spotify album ID (bare ID, not a URI).
    pub id: String,
    /// Album name.
    pub name: String,
    /// Album artists.
    pub artists: Vec<String>,
    /// Total number of tracks on the album.
    pub total_tracks: u32,
    /// URL of cover art, if any (smallest variant preferred).
    pub image_url: Option<String>,
    /// Release date as an ISO string (`"2024-05-17"` or `"2024"`/`"2024-05"`).
    pub release_date: String,
}

/// An artist as listed in the user's followed artists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistInfo {
    /// Spotify artist ID (bare ID, not a URI).
    pub id: String,
    /// Artist name.
    pub name: String,
    /// URL of an artist image, if any (smallest variant preferred).
    pub image_url: Option<String>,
    /// Approximate follower count.
    pub followers: u32,
}

/// Full album view: basic info plus the album's track list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumDetails {
    /// Basic album info.
    pub info: AlbumInfo,
    /// All tracks on the album, in track order.
    pub tracks: Vec<Track>,
}

/// Full artist view: basic info plus the artist's albums and top tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistDetails {
    /// Basic artist info.
    pub info: ArtistInfo,
    /// The artist's albums.
    pub albums: Vec<AlbumInfo>,
    /// The artist's top tracks.
    pub top_tracks: Vec<Track>,
}

/// Repeat mode for the player. Drives the `CycleRepeat` command order:
/// `Off` -> `All` -> `One` -> `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RepeatMode {
    /// Do not repeat.
    #[default]
    Off,
    /// Repeat the current context (playlist/album).
    All,
    /// Repeat the current track.
    One,
}

/// Which screen the central panel is currently showing.
///
/// Variants carrying a `String` hold a Spotify object ID (bare ID, not URI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Screen {
    /// Home overview.
    Home,
    /// Search box + results.
    Search,
    /// List of the user's playlists.
    Playlists,
    /// Track list of one playlist; the `String` is the playlist ID.
    Playlist(String),
    /// List of the user's saved albums.
    Albums,
    /// Details of one album; the `String` is the album ID.
    Album(String),
    /// List of the user's followed artists.
    Artists,
    /// Details of one artist; the `String` is the artist ID.
    Artist(String),
    /// Play queue (rendered from backend state in a later iteration).
    Queue,
    /// Application settings.
    Settings,
    /// Spotify login form.
    Login,
}

/// Aggregated search results for one query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResults {
    /// Matching tracks.
    pub tracks: Vec<Track>,
    /// Matching albums.
    pub albums: Vec<AlbumInfo>,
    /// Matching artists.
    pub artists: Vec<ArtistInfo>,
    /// Matching playlists.
    pub playlists: Vec<PlaylistInfo>,
}

/// A playback position anchor: a server-reported position paired with the
/// wall-clock time it was measured at and the playback speed, so the UI can
/// interpolate a smoothly advancing position between backend updates without
/// polling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PositionAnchor {
    /// Playback position at `timestamp_ms`, in milliseconds.
    pub position_ms: u32,
    /// Server wall-clock time (Unix epoch milliseconds) at which
    /// `position_ms` was measured.
    pub timestamp_ms: i64,
    /// Playback speed multiplier; `0.0` means the position is not advancing
    /// (paused or stopped).
    pub speed: f32,
}

impl PositionAnchor {
    /// The interpolated position at `now_ms`, in milliseconds.
    ///
    /// The position only advances when `speed > 0.0`; a non-positive speed
    /// returns `position_ms` unchanged. All arithmetic is saturating (the
    /// result never wraps and never exceeds `u32::MAX`), and negative clock
    /// skew (a local `now_ms` behind the server's `timestamp_ms`) is treated
    /// as zero elapsed time.
    pub fn position_at(&self, now_ms: i64) -> u32 {
        if self.speed <= 0.0 {
            return self.position_ms;
        }
        let elapsed_ms = (now_ms - self.timestamp_ms).max(0) as f64;
        self.position_ms
            .saturating_add((elapsed_ms * self.speed as f64) as u32)
    }
}

/// Point-in-time view of the playback state.
///
/// The backend emits [`BackendEvent::PlayerSnapshot`] on ANY player state
/// change (track change, play/pause, seek, volume, shuffle, repeat). The UI
/// replaces its whole snapshot on each event; it never mutates this state
/// itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    /// The currently loaded track, if any.
    pub current: Option<Track>,
    /// Whether playback is currently running (false = paused or stopped).
    pub playing: bool,
    /// Current playback position within the track, in milliseconds.
    pub position_ms: u32,
    /// Output volume in librespot mixer units: `0..=65535`.
    pub volume: u16,
    /// Whether shuffle is enabled.
    pub shuffle: bool,
    /// Current repeat mode.
    pub repeat: RepeatMode,
    /// True once the post-login probe has confirmed that Spotify refuses to
    /// issue audio keys for this account (the new-account restriction, see
    /// `src/player.rs` module docs): playback can never start; browsing
    /// still works.
    pub playback_blocked: bool,
    /// Anchor for interpolating the live position between backend updates
    /// (`None` = no position known). Populated when playback is driven by
    /// Spotify Soloist; the UI evaluates it at draw time (see
    /// [`PositionAnchor::position_at`]).
    pub position_anchor: Option<PositionAnchor>,
    /// Upcoming queue entries, next to play onward (empty when unknown).
    /// Populated when playback is driven by Spotify Soloist.
    pub upcoming: Vec<Track>,
}

/// Emitted as a backend error and shown in the status line once the
/// post-login probe confirms Spotify refuses audio keys for this account
/// (the new-account restriction; see `src/player.rs` module docs).
pub const PLAYBACK_BLOCKED_MESSAGE: &str = "Spotify blocks audio playback for this account (new-account restriction) — playback is unavailable; browsing still works.";

/// Commands sent from the UI to the backend player worker.
///
/// Delivered over an `std::sync::mpsc` channel; `PlayerHandle` consumes them.
/// All commands are fire-and-forget from the UI's perspective; the backend
/// reports outcomes via [`BackendEvent`]s.
#[derive(Debug, Clone)]
pub enum UiCommand {
    /// Start the OAuth login flow with the given Spotify client ID.
    /// `client_secret` is optional (PKCE flow does not need it) and currently
    /// unused; kept for future Authorization-Code-flow compatibility.
    Login {
        /// Spotify Developer Dashboard client ID.
        client_id: String,
        /// Optional client secret (unused by the PKCE flow).
        client_secret: Option<String>,
    },
    /// Log out: disconnect the session, drop the cached token, clear state.
    Logout,
    /// Play a context (playlist/album/artist URI), optionally starting at a
    /// specific track within it. `queue` carries the track URIs that should
    /// follow the initial item (empty = leave the queue to the backend).
    PlayContext {
        /// Context URI, e.g. `spotify:playlist:37i9dQZF1DXcBWIGoYBM5M`.
        context_uri: String,
        /// Optional track URI within the context to start at.
        track_uri: Option<String>,
        /// Track URIs to enqueue after the initial item.
        queue: Vec<String>,
    },
    /// Play a single track, optionally within a context. `queue` carries the
    /// track URIs that should follow it (empty = play just this track).
    PlayTrack {
        /// Track URI, e.g. `spotify:track:...`.
        track_uri: String,
        /// Optional context URI the track belongs to.
        context_uri: Option<String>,
        /// Track URIs to enqueue after this track.
        queue: Vec<String>,
    },
    /// Toggle between playing and paused.
    TogglePlay,
    /// Skip to the next track.
    Next,
    /// Go back to the previous track.
    Previous,
    /// Seek to an absolute position within the current track.
    SeekMs(u32),
    /// Set output volume: `0..=65535` (librespot mixer units).
    SetVolume(u16),
    /// Toggle shuffle mode.
    ToggleShuffle,
    /// Cycle repeat mode: Off -> All -> One -> Off.
    CycleRepeat,
    /// Load the user's library (playlists, saved albums, followed artists)
    /// from the Web API. Results arrive as [`BackendEvent::LibraryLoaded`].
    LoadLibrary,
    /// Load the tracks of one playlist. Results arrive as
    /// [`BackendEvent::PlaylistTracksLoaded`].
    LoadPlaylistTracks(String),
    /// Load one album's details. Results arrive as [`BackendEvent::AlbumLoaded`].
    LoadAlbum(String),
    /// Load one artist's details. Results arrive as [`BackendEvent::ArtistLoaded`].
    LoadArtist(String),
    /// Run a search query. Results arrive as [`BackendEvent::SearchLoaded`].
    Search(String),
    /// Add/remove a track from "Liked Songs". Result arrives as
    /// [`BackendEvent::TrackSaveUpdated`].
    ToggleSaveTrack(String),
    /// Append a track to the play queue; the `String` is a track URI
    /// (`spotify:track:...`). Routed to Soloist while it is logged in,
    /// otherwise appended to the librespot queue when one is active.
    QueueTrack(String),
}

/// Events sent from the backend worker to the UI.
///
/// The UI drains these each frame with `try_recv` and applies them to its
/// state (see `SpotifyLiteApp::apply_event`).
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// Login succeeded; `user_name` is the display name of the logged-in user.
    Authenticated {
        /// Display name of the authenticated user.
        user_name: String,
    },
    /// Login failed; the `String` is a human-readable reason.
    AuthFailed(String),
    /// The user was logged out and the session disconnected.
    LoggedOut,
    /// New playback state snapshot. Emitted on ANY player state change;
    /// replaces the UI's previous [`PlayerSnapshot`] wholesale.
    PlayerSnapshot(PlayerSnapshot),
    /// Result of [`UiCommand::LoadLibrary`].
    LibraryLoaded {
        /// The user's playlists.
        playlists: Vec<PlaylistInfo>,
        /// The user's saved albums.
        albums: Vec<AlbumInfo>,
        /// The user's followed artists.
        artists: Vec<ArtistInfo>,
    },
    /// Result of [`UiCommand::LoadPlaylistTracks`].
    PlaylistTracksLoaded {
        /// ID of the playlist these tracks belong to.
        playlist_id: String,
        /// The playlist's tracks.
        tracks: Vec<Track>,
    },
    /// Result of [`UiCommand::LoadAlbum`].
    AlbumLoaded(AlbumDetails),
    /// Result of [`UiCommand::LoadArtist`].
    ArtistLoaded(ArtistDetails),
    /// Result of [`UiCommand::Search`].
    SearchLoaded(SearchResults),
    /// Result of [`UiCommand::ToggleSaveTrack`]: the track's new saved state.
    TrackSaveUpdated {
        /// ID of the affected track.
        track_id: String,
        /// New saved state.
        saved: bool,
    },
    /// A backend error worth surfacing to the user.
    Error(String),
    /// A low-priority informational message worth surfacing to the user.
    Info(String),
}
