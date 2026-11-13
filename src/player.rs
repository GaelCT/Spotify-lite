//! Playback backend: owns the librespot session, audio pipeline and Web API
//! fan-out on a dedicated worker thread with its own Tokio runtime, consuming
//! [`UiCommand`]s and emitting [`BackendEvent`]s.
//!
//! # Architecture
//!
//! `PlayerHandle::spawn` starts one std thread. That thread builds a
//! multi-thread Tokio runtime and runs a blocking `recv_timeout` command loop;
//! each command is executed with `runtime.block_on(...)` (the runtime context
//! is entered, so `tokio::spawn` works inside commands — the player event pump
//! is one such task and keeps running between commands). The loop checks the
//! shutdown flag every 100 ms; `Drop` on `PlayerHandle` sets the flag and
//! joins the thread, which drops the runtime and aborts the spawned tasks.
//!
//! Mutable state shared between the command loop and the event-pump task lives
//! in [`WorkerState`] behind an `Arc<Mutex<_>>`; a [`PlayerSnapshot`] is built
//! from it and emitted on every relevant `PlayerEvent`.
//!
//! # Soloist routing
//!
//! This worker also owns the [`crate::soloist`] client — the WebSocket client
//! for Spotify Soloist, Spotify's official headless Connect client (see that
//! module's docs for why it exists). While Soloist reports `logged_in`, the
//! playback commands are translated to [`crate::soloist::SoloistCommand`]s
//! and never touch the librespot state, and librespot-derived snapshot
//! emissions are suppressed so Soloist-derived snapshots win (`Error`/`Info`
//! still flow). The moment Soloist is absent or logged out, behavior is
//! exactly the pure-librespot path described in this module. A takeover
//! guard pauses a still-audible librespot player on the transition so the
//! two engines never overlap (see `PlayerWorker::check_soloist_takeover`).
//!
//! # Verified librespot 0.8.0 call sequence
//!
//! All of the below was checked against docs.rs / crate sources for the exact
//! 0.8.0 release (see each module):
//!
//! ```ignore
//! use librespot::core::authentication::Credentials;
//! use librespot::core::{Session, SessionConfig, SpotifyUri};
//! use librespot::playback::audio_backend;
//! use librespot::playback::config::{AudioFormat, PlayerConfig};
//! use librespot::playback::mixer::{self, MixerConfig};
//! use librespot::playback::player::Player;
//!
//! // 1. Session — two-step in 0.8: `Session::new` then `&connect`.
//! //    `connect(credentials, store_credentials)`: the bool only controls
//! //    whether the credentials are cached in the session `Cache` — we pass
//! //    `None` for the cache and manage the token ourselves, so `false`.
//! let session = Session::new(SessionConfig::default(), None);
//! session
//!     .connect(Credentials::with_access_token(token), false)
//!     .await?;
//!
//! // 2. Audio pipeline, built once per session:
//! //    `MixerFn = fn(MixerConfig) -> Result<Arc<dyn Mixer>, Error>`;
//! //    the volume curve lives in `MixerConfig.volume_ctrl` (there is NO
//! //    `volume_ctrl` field on `PlayerConfig` in 0.8).
//! let mixer = mixer::find(None).expect("no mixer")(MixerConfig::default())?;
//! let volume_getter = mixer.get_soft_volume(); // Box<dyn VolumeGetter + Send>
//!
//! //    `SinkBuilder = fn(Option<String>, AudioFormat) -> Box<dyn Sink>`;
//! //    `find(None)` picks the first compiled backend — rodio with the
//! //    default feature set.
//! let backend = audio_backend::find(None).expect("no backend");
//!
//! let mut player_config = PlayerConfig::default();
//! player_config.position_update_interval = Some(Duration::from_millis(250));
//!
//! //    `F: FnOnce() -> Box<dyn Sink> + Send + 'static`
//! let player: Arc<Player> = Player::new(
//!     player_config,
//!     session.clone(),
//!     volume_getter,
//!     move || backend(None, AudioFormat::default()),
//! );
//!
//! // 3. Load + play. 0.8 parses URIs with `SpotifyUri::from_uri` (an
//! //    associated fn returning `Result<_, librespot_core::Error>`); the enum
//! //    has a `Track { id: SpotifyId }` variant, and `is_playable()` accepts
//! //    Track/Episode/Local.
//! let uri = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC")?;
//! player.load(uri, true, 0); // start_playing, position_ms
//!
//! // 4. Events: `PlayerEventChannel = tokio UnboundedReceiver<PlayerEvent>`.
//! let mut events = player.get_player_event_channel();
//! tokio::spawn(async move { while let Some(ev) = events.recv().await { /* ... */ } });
//!
//! // 5. Volume: `mixer.set_volume(u16)` / `mixer.volume()`; the softmixer
//! //    reports changes back as `PlayerEvent::VolumeChanged`.
//! // 6. Track metadata (best effort):
//! //    `librespot::metadata::Metadata::get(&session, &uri).await`.
//! ```
//!
//! # v1 limitations (documented, by design)
//!
//! - **New-account playback block.** Since ~2024/2025 Spotify refuses to
//!   issue audio keys (AP `AesKeyError`, upstream log "error audio key 0 1",
//!   code 0x0001) for accounts created after a server-side cutoff, which
//!   makes playback impossible for those accounts in every librespot-based
//!   client (upstream: librespot-org/librespot#1649, #1735; no client-side
//!   workaround exists — not client ID, scopes, zeroconf, nor stored
//!   credentials). After login we probe for this by requesting an audio key
//!   for a well-known track; a confirmed refusal sets
//!   [`WorkerState::playback_blocked`] (surfaced in the UI) and play
//!   commands fail fast with an explanation instead of loading tracks that
//!   can never start.
//! - **Context queue.** Clicking a track inside a playlist/album (or an
//!   "Play" header button) builds a flat queue of the context's track URIs in
//!   this worker, starting at the clicked/first track. `EndOfTrack`
//!   auto-advances (repeat-all wraps, repeat-one reloads, repeat-off stops at
//!   the end); `Next`/`Previous` walk the queue; shuffle physically reorders
//!   the not-yet-played suffix and the original order is restored when it is
//!   toggled off. Search-result rows play as single tracks with no queue.
//!   The librespot-side queue has no UI mirror; the Queue screen renders the
//!   up-next list that Spotify Soloist reports (see
//!   [`PlayerSnapshot::upcoming`]).
//! - **Track metadata in the snapshot is best effort.** A freshly loaded track
//!   is represented by a placeholder [`Track`] built from the URI alone (ID +
//!   URI known; title/artists/album/duration zero/empty). A spawned
//!   `metadata::Track::get` enriches it asynchronously; `album_art_url` stays
//!   `None` (librespot serves covers as CDN file IDs, not URLs — v2 should map
//!   them through the image CDN or the Web API).
//! - **Shuffle/repeat** drive the queue traversal described above. The
//!   librespot-side flags only mirror our toggles; the queue logic lives in
//!   this worker, not in librespot's `Player`.
//! - **Seek coalescing:** `SeekMs` sets a `seek_pending` flag that is cleared
//!   by the `Seeked`/`PositionCorrection`/`Stopped`/`EndOfTrack` events;
//!   further seeks are dropped while it is set. `SetVolume` passes straight
//!   through to the mixer.
//! - **`ToggleSaveTrack` heuristic:** `webapi::WebApi` exposes no
//!   saved-state query, so the worker keeps a `HashSet` of IDs known to be
//!   saved (siphoned from `Track.is_saved` in routed Web API results) and
//!   adds-or-removes based on that. Unknown IDs are treated as unsaved.

use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

use librespot::core::authentication::Credentials;
use librespot::core::audio_key::AudioKeyError;
use librespot::core::{Session, SessionConfig, SpotifyUri};
use librespot::metadata::audio::AudioFileFormat;
use librespot::metadata::{Metadata, Track as MetadataTrack};
use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::{self, Mixer, MixerConfig};
use librespot::playback::player::{Player, PlayerEvent};
use tokio::task::AbortHandle;

use crate::auth::AuthManager;
use crate::soloist::{
    self, SoloistCommand, SoloistHandle, SoloistStatus, DISCONNECTED_INFO,
};
use crate::types::{BackendEvent, PlayerSnapshot, RepeatMode, Track, UiCommand};
use crate::webapi::{friendly_error, WebApi};

/// Handle to the backend worker thread.
///
/// Dropping it signals the worker to shut down and joins it. The worker exits
/// on its own earlier if the command channel disconnects.
pub struct PlayerHandle {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Playback state shared between the command loop and the player event-pump
/// task. Every mutation here is followed by a [`BackendEvent::PlayerSnapshot`]
/// built from it.
struct WorkerState {
    playing: bool,
    position_ms: u32,
    volume: u16,
    shuffle: bool,
    repeat: RepeatMode,
    /// Currently loaded track, best effort (see module docs).
    current: Option<Track>,
    /// Raw URI of the loaded track, kept so repeat-one can reload it.
    current_uri: Option<String>,
    /// Set while a seek is being processed by librespot; further `SeekMs`
    /// commands are dropped until the in-flight one is acknowledged.
    seek_pending: bool,
    /// Flat queue of track URIs for context playback (see module docs).
    queue: Vec<String>,
    /// Index into `queue` of the currently playing track (`None` = no queue).
    queue_pos: Option<usize>,
    /// Original queue order, kept while shuffle is on so toggling it off
    /// restores the context's order.
    queue_backup: Option<Vec<String>>,
    /// When the current track was loaded; an `EndOfTrack` arriving within
    /// `LOAD_GRACE` of this is a load side-effect, not a natural end.
    loaded_at: Option<std::time::Instant>,
    /// True once the post-login probe has confirmed Spotify refuses audio
    /// keys for this account (see module docs): play commands fail fast.
    playback_blocked: bool,
    /// Login generation, bumped on every session teardown so a stale
    /// playback-block probe cannot apply its result to a newer session.
    generation: u64,
}

impl WorkerState {
    fn new() -> Self {
        Self {
            playing: false,
            position_ms: 0,
            volume: u16::MAX,
            shuffle: false,
            repeat: RepeatMode::Off,
            current: None,
            current_uri: None,
            seek_pending: false,
            queue: Vec::new(),
            queue_pos: None,
            queue_backup: None,
            loaded_at: None,
            playback_blocked: false,
            generation: 0,
        }
    }

    fn snapshot(&self) -> PlayerSnapshot {
        PlayerSnapshot {
            current: self.current.clone(),
            playing: self.playing,
            position_ms: self.position_ms,
            volume: self.volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
            playback_blocked: self.playback_blocked,
            position_anchor: None,
            upcoming: Vec::new(),
        }
    }

    fn reset(&mut self) {
        // Keep the login generation monotonic across teardowns so a probe
        // spawned for the outgoing session is recognized as stale.
        let generation = self.generation.wrapping_add(1);
        *self = Self::new();
        self.generation = generation;
    }

    fn clear_queue(&mut self) {
        self.queue.clear();
        self.queue_pos = None;
        self.queue_backup = None;
    }

    /// Builds a fresh queue from `uris`, positioned at `start_uri` (inserted
    /// at the front when absent). Any previous shuffle backup is discarded.
    fn set_queue(&mut self, uris: Vec<String>, start_uri: &str) {
        let mut queue = uris;
        let pos = queue.iter().position(|u| u == start_uri).unwrap_or(0);
        if queue.get(pos).map(|u| u.as_str()) != Some(start_uri) {
            queue.insert(0, start_uri.to_string());
        }
        self.queue_pos = Some(pos);
        self.queue = queue;
        self.queue_backup = None;
    }

    /// Computes the next track to play and advances `queue_pos`. Wraps around
    /// on `RepeatMode::All`, returns `None` at the end otherwise (a natural
    /// stop). `RepeatMode::One` is handled by the caller (reload, no
    /// advance) — manual `Next` treats it like `Off` here, as Spotify does.
    fn queue_advance(&mut self) -> Option<String> {
        let pos = self.queue_pos?;
        if self.queue.is_empty() {
            return None;
        }
        let next = if self.shuffle && self.queue.len() > pos + 1 {
            pos + 1 + rand_below(self.queue.len() - pos - 1)
        } else {
            pos + 1
        };
        if next >= self.queue.len() {
            if self.repeat == RepeatMode::All {
                let wrapped = if self.shuffle { rand_below(self.queue.len()) } else { 0 };
                self.queue_pos = Some(wrapped);
                Some(self.queue[wrapped].clone())
            } else {
                self.queue_pos = None;
                None
            }
        } else {
            self.queue_pos = Some(next);
            Some(self.queue[next].clone())
        }
    }
}

/// An `EndOfTrack` within this period after a load is a stale side-effect of
/// replacing a playing track, not a natural end — the queue must not advance.
const LOAD_GRACE: Duration = Duration::from_millis(300);

/// A well-known, globally available track used by the post-login probe to
/// test whether Spotify will issue an audio key for this account at all (see
/// module docs). If it ever leaves the catalog or ships without audio files
/// the probe fails open (no block detected).
const PROBE_TRACK_URI: &str = "spotify:track:1udwFobQ1JoOdWPQrp2b6u";

/// How long the post-login audio-key probe may take before it gives up
/// (inconclusive → fail open).
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Soloist WebSocket address used when the config does not set one (see
/// [`resolve_soloist_addr`]).
const DEFAULT_SOLOIST_ADDR: &str = "127.0.0.1:9090";

/// Resolves the configured Soloist WebSocket address to the one the client
/// connects to. Soloist's WS API has no client authentication, so the
/// loopback-only policy applies: only `127.0.0.1`, `localhost` and `[::1]`
/// are accepted, each with an optional `:port` (a missing port defaults to
/// 9090). Anything else is rejected with a warning and falls back to
/// [`DEFAULT_SOLOIST_ADDR`].
fn resolve_soloist_addr(configured: &str) -> String {
    let (host, port) = match configured.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (configured, None),
    };
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        tracing::warn!(
            "ignoring non-loopback soloist_ws_addr {configured:?}; using {DEFAULT_SOLOIST_ADDR}"
        );
        return DEFAULT_SOLOIST_ADDR.to_string();
    }
    format!("{host}:{}", port.unwrap_or("9090"))
}

/// Tiny xorshift64* PRNG. Shuffle is its only consumer and crypto-quality
/// randomness is irrelevant, so we avoid pulling in `rand`.
fn rand_below(bound: usize) -> usize {
    if bound <= 1 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut x = nanos ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (((x as u128) * (bound as u128)) >> 64) as usize
}

/// Fisher-Yates shuffle of `queue[start + 1 ..]`, keeping the current track
/// (and everything before it) in place.
fn shuffle_suffix(queue: &mut [String], start: usize) {
    for i in (start + 1..queue.len()).rev() {
        let j = start + 1 + rand_below(i - start);
        queue.swap(i, j);
    }
}

/// Outcome of the post-login audio-key probe.
enum ProbeResult {
    /// Spotify issued the key: playback should work.
    Granted,
    /// Spotify explicitly refused the key: this account is blocked.
    Denied,
    /// Inconclusive (metadata/network/timeout): don't change the flag.
    Unknown,
}

/// Fetches `uri`'s metadata and requests an audio key for one of its
/// files — the exact step Spotify denies for accounts affected by the
/// new-account restriction (see module docs).
async fn probe_audio_key(session: &Session, uri: &SpotifyUri) -> ProbeResult {
    let SpotifyUri::Track { id } = uri else {
        return ProbeResult::Unknown;
    };
    let Ok(track) = MetadataTrack::get(session, uri).await else {
        return ProbeResult::Unknown;
    };
    // Any file works for a probe; mirror the player's format preference.
    let file_id = [
        AudioFileFormat::OGG_VORBIS_320,
        AudioFileFormat::OGG_VORBIS_160,
        AudioFileFormat::OGG_VORBIS_96,
    ]
    .into_iter()
    .find_map(|f| track.files.get(&f).copied())
    .or_else(|| track.files.values().next().copied());
    let Some(file_id) = file_id else {
        return ProbeResult::Unknown;
    };
    match session.audio_key().request(*id, file_id).await {
        Ok(_) => ProbeResult::Granted,
        Err(e) if is_audio_key_denied(&e) => ProbeResult::Denied,
        Err(_) => ProbeResult::Unknown,
    }
}

/// True when `e` is the AP's explicit audio-key refusal (`AesKeyError`,
/// logged upstream as "error audio key 0 1", code 0x0001).
fn is_audio_key_denied(e: &librespot::core::Error) -> bool {
    matches!(
        e.error.downcast_ref::<AudioKeyError>(),
        Some(AudioKeyError::AesKey)
    )
}

/// Builds the best-effort placeholder [`Track`] for a URI we have no
/// metadata for yet: ID and URI are known, everything else is empty/zero
/// (see module docs).
fn placeholder_track(track_uri: &str) -> Track {
    Track {
        id: track_uri
            .strip_prefix("spotify:track:")
            .unwrap_or_default()
            .to_string(),
        uri: track_uri.to_string(),
        title: String::new(),
        artists: Vec::new(),
        album: String::new(),
        album_art_url: None,
        duration_ms: 0,
        track_number: 0,
        disc_number: 0,
        is_saved: false,
    }
}

/// Spawns the best-effort metadata enrichment for a freshly loaded track
/// (see module docs): real title/artists/album/duration from librespot
/// metadata, adopted only if the track is still current when it arrives.
/// The adoption snapshot is suppressed while Soloist drives playback (its
/// own snapshots win — see the module docs).
fn spawn_enrichment(
    state: &Arc<Mutex<WorkerState>>,
    events: &Sender<BackendEvent>,
    session: &Session,
    uri: SpotifyUri,
    track_uri: String,
    soloist: &SoloistHandle,
) {
    let state = Arc::clone(state);
    let events = events.clone();
    let session = session.clone();
    let soloist = soloist.clone();
    tokio::spawn(async move {
        if let Ok(meta) = MetadataTrack::get(&session, &uri).await {
            let track = Track {
                id: meta.id.to_id().unwrap_or_default(),
                uri: track_uri,
                title: meta.name,
                artists: meta.artists.iter().map(|a| a.name.clone()).collect(),
                album: meta.album.name,
                album_art_url: None,
                duration_ms: meta.duration.max(0) as u32,
                track_number: meta.number.max(0) as u32,
                disc_number: meta.disc_number.max(0) as u32,
                is_saved: false,
            };
            let mut st = state.lock().unwrap();
            // Only adopt the metadata if the track is still current.
            if st.current_uri.as_deref() == Some(track.uri.as_str()) {
                st.current = Some(track);
                if !soloist.state().logged_in {
                    let snapshot = st.snapshot();
                    let _ = events.send(BackendEvent::PlayerSnapshot(snapshot));
                }
            }
        }
    });
}

/// The backend worker: owns the session/player/mixer and dispatches
/// [`UiCommand`]s one at a time (via `runtime.block_on` from the owning
/// thread).
struct PlayerWorker {
    events: Sender<BackendEvent>,
    auth: AuthManager,
    state: Arc<Mutex<WorkerState>>,
    session: Option<Session>,
    player: Option<Arc<Player>>,
    mixer: Option<Arc<dyn Mixer>>,
    access_token: Option<String>,
    /// The Soloist client: audio-engine routing target while it reports
    /// `logged_in` (see the module docs).
    soloist: SoloistHandle,
    /// Last observed Soloist `logged_in`, tracked for the takeover pause
    /// (see `check_soloist_takeover`).
    last_soloist_live: bool,
    /// Track IDs known to be in "Liked Songs" (see module docs).
    saved_ids: HashSet<String>,
    /// Abort handle of the player event-pump task (replaced per session).
    pump_abort: Option<AbortHandle>,
}

impl PlayerWorker {
    fn new(events: Sender<BackendEvent>, soloist: SoloistHandle) -> Self {
        Self {
            events,
            auth: AuthManager::new(),
            state: Arc::new(Mutex::new(WorkerState::new())),
            session: None,
            player: None,
            mixer: None,
            access_token: None,
            soloist,
            last_soloist_live: false,
            saved_ids: HashSet::new(),
            pump_abort: None,
        }
    }

    fn emit(&self, event: BackendEvent) {
        let _ = self.events.send(event);
    }

    fn emit_snapshot(&self) {
        // While Soloist drives playback, Soloist-derived snapshots win and
        // librespot-side emissions are skipped (the state updates still
        // happen, so the librespot path is current again the moment Soloist
        // drops out). Error/Info are never gated.
        if self.soloist.state().logged_in {
            return;
        }
        let snapshot = self.state.lock().unwrap().snapshot();
        self.emit(BackendEvent::PlayerSnapshot(snapshot));
    }

    /// Returns a `WebApi` client when logged in; emits `Error` otherwise.
    fn webapi(&self) -> Option<WebApi> {
        match &self.access_token {
            Some(token) => Some(WebApi::new(token.clone())),
            None => {
                self.emit(BackendEvent::Error("not logged in".to_string()));
                None
            }
        }
    }

    /// Player + "is something loaded" guard for playback commands.
    fn player(&self) -> Option<Arc<Player>> {
        match &self.player {
            Some(p) => Some(Arc::clone(p)),
            None => {
                self.emit(BackendEvent::Error("not logged in".to_string()));
                None
            }
        }
    }

    async fn handle_command(&mut self, cmd: UiCommand) {
        // While Soloist is logged in it owns playback: the commands below
        // are translated to Soloist commands and never touch the librespot
        // state. Everything else flows to the existing paths unchanged.
        if self.route_soloist(&cmd) {
            return;
        }
        match cmd {
            UiCommand::Login {
                client_id,
                client_secret,
            } => self.login(&client_id, client_secret.as_deref()).await,
            UiCommand::Logout => self.logout(),
            UiCommand::PlayTrack {
                track_uri,
                context_uri,
                ..
            } => match context_uri {
                Some(context_uri) => self.play_context(&context_uri, Some(&track_uri)).await,
                None => self.play_track(&track_uri).await,
            },
            UiCommand::PlayContext {
                context_uri,
                track_uri,
                ..
            } => self.play_context(&context_uri, track_uri.as_deref()).await,
            UiCommand::TogglePlay => self.toggle_play(),
            UiCommand::Next => self.manual_next(),
            UiCommand::Previous => self.manual_previous(),
            UiCommand::SeekMs(position_ms) => self.seek(position_ms),
            UiCommand::SetVolume(volume) => self.set_volume(volume),
            UiCommand::ToggleShuffle => self.toggle_shuffle(),
            UiCommand::CycleRepeat => self.cycle_repeat(),
            UiCommand::LoadLibrary => self.load_library().await,
            UiCommand::LoadPlaylistTracks(id) => self.load_playlist_tracks(id).await,
            UiCommand::LoadAlbum(id) => self.load_album(id).await,
            UiCommand::LoadArtist(id) => self.load_artist(id).await,
            UiCommand::Search(query) => self.search(query).await,
            UiCommand::ToggleSaveTrack(id) => self.toggle_save_track(id).await,
            UiCommand::QueueTrack(uri) => self.queue_track(&uri),
        }
    }

    /// Soloist-side command routing (see the module docs). While the
    /// mirrored [`SoloistState`](crate::soloist::SoloistState) reports
    /// `logged_in`, the playback commands are translated to
    /// [`SoloistCommand`]s — fire-and-forget; outcomes arrive asynchronously
    /// as Soloist events — and `true` is returned so the librespot path never
    /// runs. When Soloist is not logged in, returns `false` and the caller
    /// falls through to the librespot path.
    fn route_soloist(&self, cmd: &UiCommand) -> bool {
        let state = self.soloist.state();
        if !state.logged_in {
            return false;
        }
        match cmd {
            UiCommand::PlayTrack { track_uri, queue, .. } => {
                // Soloist's `play` with a track URI replaces the queue on its
                // side; the UI-passed queue is re-appended below.
                if !state.is_active {
                    self.soloist.command(SoloistCommand::Activate);
                }
                self.soloist.command(SoloistCommand::Play(Some(track_uri.clone())));
                for uri in queue {
                    self.soloist.command(SoloistCommand::AddToQueue(uri.clone()));
                }
                true
            }
            UiCommand::PlayContext {
                context_uri,
                track_uri,
                queue,
            } => {
                if !state.is_active {
                    self.soloist.command(SoloistCommand::Activate);
                }
                self.soloist.command(SoloistCommand::Play(Some(
                    track_uri.clone().unwrap_or_else(|| context_uri.clone()),
                )));
                for uri in queue {
                    self.soloist.command(SoloistCommand::AddToQueue(uri.clone()));
                }
                true
            }
            UiCommand::TogglePlay => {
                self.soloist.command(match state.status {
                    SoloistStatus::Playing | SoloistStatus::Buffering => SoloistCommand::Pause,
                    _ => SoloistCommand::Resume,
                });
                true
            }
            UiCommand::Next => {
                self.soloist.command(SoloistCommand::SkipNext);
                true
            }
            UiCommand::Previous => {
                self.soloist.command(SoloistCommand::SkipPrev);
                true
            }
            UiCommand::SeekMs(position_ms) => {
                self.soloist.command(SoloistCommand::Seek(*position_ms));
                true
            }
            UiCommand::SetVolume(volume) => {
                self.soloist
                    .command(SoloistCommand::SetVolume((*volume as u32 * 100 / 65535) as u8));
                true
            }
            UiCommand::ToggleShuffle => {
                self.soloist.command(SoloistCommand::SetShuffle(!state.shuffle));
                true
            }
            UiCommand::CycleRepeat => {
                let next = match state.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                };
                self.soloist.command(SoloistCommand::SetRepeat(next));
                true
            }
            UiCommand::QueueTrack(uri) => {
                self.soloist.command(SoloistCommand::AddToQueue(uri.clone()));
                self.emit(BackendEvent::Info("Added to queue".to_string()));
                true
            }
            _ => false,
        }
    }

    /// Called from the command loop on every `recv_timeout` (~100 ms). On the
    /// Soloist `logged_in` false→true transition, pauses the librespot player
    /// when the librespot side still reports `playing`, so a librespot-audible
    /// session never overlaps Soloist-driven audio.
    fn check_soloist_takeover(&mut self) {
        let live = self.soloist.state().logged_in;
        if live && !self.last_soloist_live && self.state.lock().unwrap().playing {
            if let Some(player) = &self.player {
                player.pause();
            }
        }
        self.last_soloist_live = live;
    }

    // ---------------------------------------------------------------- login

    async fn login(&mut self, client_id: &str, client_secret: Option<&str>) {
        // Re-login: tear down any existing session first.
        self.teardown_session();

        let outcome = match self.auth.authenticate(client_id, client_secret).await {
            Ok(outcome) => outcome,
            Err(e) => {
                self.emit(BackendEvent::AuthFailed(e.to_string()));
                return;
            }
        };
        self.access_token = Some(outcome.access_token.clone());

        // Playback token: a SEPARATE OAuth against Spotify's desktop client
        // identity. Since 2026-08-10 the login5 endpoint rejects playback
        // credentials minted from third-party client IDs (every track load
        // dies with INVALID_CREDENTIALS), so the librespot session must use
        // this token; the Web API keeps the user's dev-app token above.
        let playback_token = match self.auth.authenticate_playback().await {
            Ok(token) => token,
            Err(e) => {
                self.access_token = None;
                self.emit(BackendEvent::AuthFailed(format!(
                    "playback authorization failed: {e}"
                )));
                return;
            }
        };

        // Two-step 0.8 API: `Session::new` then `&connect`. `store_credentials`
        // only controls caching credentials in the session cache; we pass
        // `None` for the cache and keep our own token cache in `AuthManager`.
        let session = Session::new(SessionConfig::default(), None);
        if let Err(e) = session
            .connect(Credentials::with_access_token(playback_token), false)
            .await
        {
            self.access_token = None;
            self.emit(BackendEvent::AuthFailed(format!(
                "session connect failed: {e}"
            )));
            return;
        }

        // Audio pipeline, built once per session: softvolume mixer + the
        // default (rodio) backend.
        let mixer = match mixer::find(None) {
            Some(create) => match create(MixerConfig::default()) {
                Ok(mixer) => mixer,
                Err(e) => {
                    self.access_token = None;
                    self.emit(BackendEvent::AuthFailed(format!("mixer init failed: {e}")));
                    return;
                }
            },
            None => {
                self.access_token = None;
                self.emit(BackendEvent::AuthFailed("no audio mixer available".into()));
                return;
            }
        };
        let backend = match audio_backend::find(None) {
            Some(backend) => backend,
            None => {
                self.access_token = None;
                self.emit(BackendEvent::AuthFailed("no audio backend available".into()));
                return;
            }
        };
        let volume_getter = mixer.get_soft_volume();

        let mut player_config = PlayerConfig::default();
        player_config.position_update_interval = Some(Duration::from_millis(250));

        let player = Player::new(
            player_config,
            session.clone(),
            volume_getter,
            move || backend(None, AudioFormat::default()),
        );

        {
            let mut state = self.state.lock().unwrap();
            state.volume = mixer.volume();
        }

        self.session = Some(session.clone());
        self.mixer = Some(mixer);
        self.player = Some(player.clone());
        self.spawn_block_probe(&session);
        self.spawn_event_pump(player, session);

        self.emit(BackendEvent::Authenticated {
            user_name: outcome
                .user_name
                .unwrap_or_else(|| "Spotify user".to_string()),
        });
        self.emit_snapshot();
    }

    /// Drops the player, mixer and session and aborts the event pump.
    /// The token cache is left alone (only `Logout` clears it).
    fn teardown_session(&mut self) {
        if let Some(abort) = self.pump_abort.take() {
            abort.abort();
        }
        // Dropping the last `Arc<Player>` stops its internal thread; the pump
        // task (already aborted) was the only other strong reference.
        self.player = None;
        self.mixer = None;
        if let Some(session) = self.session.take() {
            session.shutdown();
        }
        self.access_token = None;
        self.state.lock().unwrap().reset();
    }

    fn logout(&mut self) {
        self.teardown_session();
        self.auth.clear_cache();
        self.emit(BackendEvent::LoggedOut);
        self.emit_snapshot();
    }

    // ------------------------------------------------------------ event pump

    /// Spawns the task translating `PlayerEvent`s into state updates +
    /// snapshots. The task also owns a strong `Arc<Player>` so that repeat-one
    /// can reload the current track on `EndOfTrack`; `teardown_session` aborts
    /// it before the player is dropped, and the channel closing on player
    /// drop ends it regardless.
    fn spawn_event_pump(&mut self, player: Arc<Player>, session: Session) {
        let mut rx = player.get_player_event_channel();
        let events = self.events.clone();
        let state = Arc::clone(&self.state);
        let soloist = self.soloist.clone();
        let session = session;

        let handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let mut emit_snapshot = false;
                {
                    let mut st = state.lock().unwrap();
                    match event {
                        PlayerEvent::Playing { position_ms, .. } => {
                            st.playing = true;
                            st.position_ms = position_ms;
                            emit_snapshot = true;
                        }
                        PlayerEvent::Paused { position_ms, .. } => {
                            st.playing = false;
                            st.position_ms = position_ms;
                            emit_snapshot = true;
                        }
                        PlayerEvent::Stopped { .. } => {
                            st.playing = false;
                            st.position_ms = 0;
                            st.seek_pending = false;
                            emit_snapshot = true;
                        }
                        PlayerEvent::EndOfTrack { .. } => {
                            st.playing = false;
                            st.position_ms = 0;
                            st.seek_pending = false;
                            // An EndOfTrack within LOAD_GRACE of a load is a
                            // stale side-effect of replacing a playing track,
                            // not a natural end — do not advance the queue.
                            let stale = st
                                .loaded_at
                                .map_or(false, |t| t.elapsed() < LOAD_GRACE);
                            if !stale {
                                let next = if st.repeat == RepeatMode::One {
                                    st.current_uri.clone()
                                } else {
                                    st.queue_advance()
                                };
                                if let Some(uri_str) = next {
                                    if let Ok(uri) = SpotifyUri::from_uri(&uri_str) {
                                        if uri.is_playable() {
                                            if st.repeat != RepeatMode::One {
                                                // Advance: new placeholder +
                                                // enrichment. Repeat-one keeps
                                                // the already-enriched current.
                                                st.current = Some(placeholder_track(&uri_str));
                                                spawn_enrichment(
                                                    &state, &events, &session, uri.clone(), uri_str.clone(),
                                                    &soloist,
                                                );
                                            }
                                            player.load(uri, true, 0);
                                            st.current_uri = Some(uri_str);
                                            st.playing = true;
                                            st.loaded_at = Some(std::time::Instant::now());
                                        }
                                    }
                                }
                            }
                            emit_snapshot = true;
                        }
                        PlayerEvent::PositionChanged { position_ms, .. }
                        | PlayerEvent::PositionCorrection { position_ms, .. }
                        | PlayerEvent::Seeked { position_ms, .. } => {
                            st.position_ms = position_ms;
                            st.seek_pending = false;
                            emit_snapshot = true;
                        }
                        PlayerEvent::VolumeChanged { volume } => {
                            st.volume = volume;
                            emit_snapshot = true;
                        }
                        PlayerEvent::ShuffleChanged { shuffle } => {
                            st.shuffle = shuffle;
                            emit_snapshot = true;
                        }
                        PlayerEvent::RepeatChanged { context, track } => {
                            st.repeat = if track {
                                RepeatMode::One
                            } else if context {
                                RepeatMode::All
                            } else {
                                RepeatMode::Off
                            };
                            emit_snapshot = true;
                        }
                        // Loading/Preloading/PlayRequestIdChanged/
                        // TimeToPreloadNextTrack/Unavailable/TrackChanged/
                        // Session*/AutoPlay*/FilterExplicit* — no snapshot
                        // state to update (track metadata is fetched
                        // separately, see `play_track`).
                        _ => {}
                    }
                }
                // While Soloist drives playback its own snapshots win;
                // librespot-side snapshots are suppressed (state updates
                // above still happen).
                if emit_snapshot && !soloist.state().logged_in {
                    let snapshot = state.lock().unwrap().snapshot();
                    let _ = events.send(BackendEvent::PlayerSnapshot(snapshot));
                }
            }
        });
        self.pump_abort = Some(handle.abort_handle());
    }

    /// Spawns the post-login probe that detects whether Spotify refuses to
    /// issue audio keys for this account (the new-account restriction, see
    /// the module docs). It fetches a well-known track's metadata and asks
    /// for its audio key — the exact step the restriction denies. Only an
    /// explicit `AesKeyError` refusal counts as "blocked"; every other
    /// outcome (metadata/network/timeout) fails open, since play attempts
    /// surface their own errors anyway. Runs in the background; a result
    /// that arrives after a logout/re-login (stale generation) is dropped.
    fn spawn_block_probe(&self, session: &Session) {
        let Ok(uri) = SpotifyUri::from_uri(PROBE_TRACK_URI) else {
            return;
        };
        let session = session.clone();
        let state = Arc::clone(&self.state);
        let events = self.events.clone();
        let soloist = self.soloist.clone();
        let generation = state.lock().unwrap().generation;

        tokio::spawn(async move {
            let result = tokio::time::timeout(PROBE_TIMEOUT, probe_audio_key(&session, &uri)).await;
            let blocked = match result {
                Ok(ProbeResult::Denied) => true,
                Ok(ProbeResult::Granted) => false,
                // Inconclusive: leave the flag untouched.
                _ => return,
            };
            let snapshot = {
                let mut st = state.lock().unwrap();
                if st.generation != generation {
                    return;
                }
                st.playback_blocked = blocked;
                st.snapshot()
            };
            // The flag is recorded above regardless; the snapshot send is
            // suppressed while Soloist drives playback (its own win).
            if !soloist.state().logged_in {
                let _ = events.send(BackendEvent::PlayerSnapshot(snapshot));
            }
            if blocked {
                tracing::warn!(
                    "playback blocked: Spotify refused the audio key for this account"
                );
                let _ = events.send(BackendEvent::Error(
                    crate::types::PLAYBACK_BLOCKED_MESSAGE.to_string(),
                ));
            }
        });
    }

    // -------------------------------------------------------------- playback

    /// Plays a single track with no context: clears any active queue, then
    /// loads.
    async fn play_track(&mut self, track_uri: &str) {
        self.state.lock().unwrap().clear_queue();
        let (player, session) = match (&self.player, &self.session) {
            (Some(player), Some(session)) => (Arc::clone(player), session.clone()),
            _ => {
                self.emit(BackendEvent::Error("not logged in".to_string()));
                return;
            }
        };
        self.load_and_play(&player, &session, track_uri);
    }

    /// Parses and loads `track_uri`, updates the current-track state and
    /// kicks off best-effort metadata enrichment. Shared by every play path.
    fn load_and_play(&self, player: &Arc<Player>, session: &Session, track_uri: &str) {
        if self.state.lock().unwrap().playback_blocked {
            // Confirmed by the post-login probe (see module docs): every
            // load would hit the same audio-key refusal, so fail fast with
            // an explanation instead of flailing through the decoder.
            self.emit(BackendEvent::Error(
                crate::types::PLAYBACK_BLOCKED_MESSAGE.to_string(),
            ));
            return;
        }
        let uri = match SpotifyUri::from_uri(track_uri) {
            Ok(uri) if uri.is_playable() => uri,
            Ok(_) => {
                self.emit(BackendEvent::Error(format!(
                    "not a playable track URI: {track_uri}"
                )));
                return;
            }
            Err(e) => {
                self.emit(BackendEvent::Error(format!("invalid track URI: {e}")));
                return;
            }
        };

        {
            let mut st = self.state.lock().unwrap();
            st.current = Some(placeholder_track(track_uri));
            st.current_uri = Some(track_uri.to_string());
            st.playing = true;
            st.position_ms = 0;
            st.seek_pending = false;
            st.loaded_at = Some(std::time::Instant::now());
        }
        player.load(uri.clone(), true, 0);
        spawn_enrichment(&self.state, &self.events, session, uri, track_uri.to_string(), &self.soloist);
        self.emit_snapshot();
    }

    /// Plays a context (playlist/album), starting at `start_uri` (or its
    /// first track). Fetches the context's track list to build the queue; on
    /// failure the clicked track still plays as a single track when given.
    async fn play_context(&mut self, context_uri: &str, start_uri: Option<&str>) {
        match self.context_track_uris(context_uri).await {
            Ok(uris) if uris.is_empty() => {
                self.emit(BackendEvent::Info(format!("context is empty: {context_uri}")));
            }
            Ok(uris) => {
                let start = start_uri.unwrap_or(&uris[0]).to_string();
                self.state.lock().unwrap().set_queue(uris, &start);
                let (player, session) = match (&self.player, &self.session) {
                    (Some(player), Some(session)) => (Arc::clone(player), session.clone()),
                    _ => {
                        self.emit(BackendEvent::Error("not logged in".to_string()));
                        return;
                    }
                };
                self.load_and_play(&player, &session, &start);
            }
            Err(e) => match start_uri {
                Some(track_uri) => {
                    self.emit(BackendEvent::Info(format!(
                        "queue unavailable ({}); playing the track only",
                        friendly_error(&e)
                    )));
                    self.play_track(track_uri).await;
                }
                None => self.emit(BackendEvent::Error(format!(
                    "cannot play context: {}",
                    friendly_error(&e)
                ))),
            },
        }
    }

    /// Fetches the track URIs of a `spotify:playlist:` / `spotify:album:`
    /// context, keeping only playable track URIs.
    async fn context_track_uris(&self, context_uri: &str) -> anyhow::Result<Vec<String>> {
        let Some(api) = self.webapi() else {
            anyhow::bail!("not logged in");
        };
        let uris: Vec<String> = if let Some(id) = context_uri.strip_prefix("spotify:playlist:") {
            api.playlist_tracks(id)
                .await?
                .into_iter()
                .map(|track| track.uri)
                .collect()
        } else if let Some(id) = context_uri.strip_prefix("spotify:album:") {
            api.album(id)
                .await?
                .tracks
                .into_iter()
                .map(|track| track.uri)
                .collect()
        } else {
            anyhow::bail!("unsupported context: {context_uri}");
        };
        Ok(uris
            .into_iter()
            .filter(|uri| uri.starts_with("spotify:track:"))
            .collect())
    }

    /// Manual `Next`: advances the queue (see `WorkerState::queue_advance`).
    fn manual_next(&mut self) {
        let next = self.state.lock().unwrap().queue_advance();
        let Some(uri) = next else {
            self.emit(BackendEvent::Info("end of the queue".to_string()));
            return;
        };
        let (player, session) = match (&self.player, &self.session) {
            (Some(player), Some(session)) => (Arc::clone(player), session.clone()),
            _ => {
                self.emit(BackendEvent::Error("not logged in".to_string()));
                return;
            }
        };
        self.load_and_play(&player, &session, &uri);
    }

    /// Manual `Previous`: restarts the current track when more than 3 s in or
    /// at the head of the queue, otherwise steps back to the previous track.
    fn manual_previous(&mut self) {
        let target = {
            let st = self.state.lock().unwrap();
            if st.current_uri.is_none() {
                None
            } else if st.position_ms > 3_000 {
                Some(None) // restart current
            } else {
                match st.queue_pos {
                    Some(pos) if pos > 0 => Some(Some(st.queue[pos - 1].clone())),
                    _ => Some(None),
                }
            }
        };
        match target {
            None => {}
            Some(None) => self.seek(0),
            Some(Some(uri)) => {
                let (player, session) = match (&self.player, &self.session) {
                    (Some(player), Some(session)) => (Arc::clone(player), session.clone()),
                    _ => {
                        self.emit(BackendEvent::Error("not logged in".to_string()));
                        return;
                    }
                };
                self.load_and_play(&player, &session, &uri);
            }
        }
    }

    /// `QueueTrack` on the librespot path: appends the URI to the end of the
    /// active queue. With no queue playing, only informs the user.
    fn queue_track(&mut self, uri: &str) {
        let queued = {
            let mut st = self.state.lock().unwrap();
            if st.queue_pos.is_some() {
                st.queue.push(uri.to_string());
                true
            } else {
                false
            }
        };
        self.emit(BackendEvent::Info(
            if queued {
                "Added to queue"
            } else {
                "Nothing playing to queue after"
            }
            .to_string(),
        ));
    }

    fn toggle_play(&mut self) {
        let playing = self.state.lock().unwrap().playing;
        if let Some(player) = self.player() {
            if playing {
                player.pause();
            } else {
                player.play();
            }
        }
    }

    fn seek(&mut self, position_ms: u32) {
        let (pending, loaded) = {
            let st = self.state.lock().unwrap();
            (st.seek_pending, st.current_uri.is_some())
        };
        if !loaded {
            // Nothing to seek within; stay silent rather than noisy.
            return;
        }
        if pending {
            // Coalesce: drop seeks while one is in flight (module docs).
            return;
        }
        if let Some(player) = self.player() {
            player.seek(position_ms);
            self.state.lock().unwrap().seek_pending = true;
        }
    }

    fn set_volume(&mut self, volume: u16) {
        if let Some(mixer) = &self.mixer {
            mixer.set_volume(volume);
            // VolumeChanged flows back through the event pump as a snapshot;
            // update eagerly too so rapid drags feel responsive.
            self.state.lock().unwrap().volume = volume;
            self.emit_snapshot();
        } else {
            self.emit(BackendEvent::Error("not logged in".to_string()));
        }
    }

    fn toggle_shuffle(&mut self) {
        let shuffle = {
            let mut st = self.state.lock().unwrap();
            st.shuffle = !st.shuffle;
            let shuffle = st.shuffle;
            if shuffle {
                if let Some(pos) = st.queue_pos {
                    st.queue_backup = Some(st.queue.clone());
                    shuffle_suffix(&mut st.queue, pos);
                }
            } else if let Some(backup) = st.queue_backup.take() {
                // Restore the context's original order, re-positioning on the
                // track that is currently playing.
                let current = st.current_uri.clone();
                st.queue = backup;
                if let Some(cur) = current {
                    if let Some(pos) = st.queue.iter().position(|u| *u == cur) {
                        st.queue_pos = Some(pos);
                    }
                }
            }
            shuffle
        };
        if let Some(player) = self.player() {
            // Flows back as PlayerEvent::ShuffleChanged -> snapshot.
            player.emit_shuffle_changed_event(shuffle);
        }
    }

    fn cycle_repeat(&mut self) {
        let next = {
            let mut st = self.state.lock().unwrap();
            st.repeat = match st.repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            st.repeat
        };
        if let Some(player) = self.player() {
            // Flows back as PlayerEvent::RepeatChanged -> snapshot.
            player.emit_repeat_changed_event(next != RepeatMode::Off, next == RepeatMode::One);
        }
    }

    // --------------------------------------------------------------- web api

    fn siphon_saved(&mut self, tracks: &[Track]) {
        for track in tracks {
            if track.is_saved {
                self.saved_ids.insert(track.id.clone());
            }
        }
    }

    /// `LoadLibrary`: fetches the three sections independently so one failing
    /// endpoint (e.g. an endpoint-family quota answering HTTP 429) cannot
    /// wipe the whole view. Every section that succeeds lands in
    /// [`BackendEvent::LibraryLoaded`] with an empty `Vec` for the failed
    /// ones; when only some fail, an additional `Error` names them. Only when
    /// all three fail is the load treated as a hard failure (no
    /// `LibraryLoaded`).
    async fn load_library(&mut self) {
        let Some(api) = self.webapi() else { return };
        let playlists = api.current_user_playlists().await;
        let albums = api.saved_albums().await;
        let artists = api.followed_artists().await;
        let mut failures = Vec::new();
        let playlists = playlists.unwrap_or_else(|e| {
            failures.push(format!("playlists failed ({})", friendly_error(&e)));
            Vec::new()
        });
        let albums = albums.unwrap_or_else(|e| {
            failures.push(format!("albums failed ({})", friendly_error(&e)));
            Vec::new()
        });
        let artists = artists.unwrap_or_else(|e| {
            failures.push(format!("artists failed ({})", friendly_error(&e)));
            Vec::new()
        });
        if failures.len() < 3 {
            self.emit(BackendEvent::LibraryLoaded {
                playlists,
                albums,
                artists,
            });
            if !failures.is_empty() {
                self.emit(BackendEvent::Error(format!(
                    "Library partially loaded: {}",
                    failures.join("; ")
                )));
            }
        } else {
            self.emit(BackendEvent::Error(format!(
                "failed to load library: {}",
                failures.join("; ")
            )));
        }
    }

    async fn load_playlist_tracks(&mut self, playlist_id: String) {
        let Some(api) = self.webapi() else { return };
        match api.playlist_tracks(&playlist_id).await {
            Ok(tracks) => {
                self.siphon_saved(&tracks);
                self.emit(BackendEvent::PlaylistTracksLoaded {
                    playlist_id,
                    tracks,
                });
            }
            Err(e) => self.emit(BackendEvent::Error(friendly_error(&e))),
        }
    }

    async fn load_album(&mut self, album_id: String) {
        let Some(api) = self.webapi() else { return };
        match api.album(&album_id).await {
            Ok(details) => {
                self.siphon_saved(&details.tracks);
                self.emit(BackendEvent::AlbumLoaded(details));
            }
            Err(e) => self.emit(BackendEvent::Error(friendly_error(&e))),
        }
    }

    async fn load_artist(&mut self, artist_id: String) {
        let Some(api) = self.webapi() else { return };
        match api.artist(&artist_id).await {
            Ok(details) => {
                self.siphon_saved(&details.top_tracks);
                self.emit(BackendEvent::ArtistLoaded(details));
            }
            Err(e) => self.emit(BackendEvent::Error(friendly_error(&e))),
        }
    }

    async fn search(&mut self, query: String) {
        let Some(api) = self.webapi() else { return };
        match api.search(&query).await {
            Ok(results) => {
                self.siphon_saved(&results.tracks);
                self.emit(BackendEvent::SearchLoaded(results));
            }
            Err(e) => self.emit(BackendEvent::Error(friendly_error(&e))),
        }
    }

    async fn toggle_save_track(&mut self, track_id: String) {
        let Some(api) = self.webapi() else { return };
        // See module docs: saved-state is a best-effort heuristic because
        // WebApi exposes no saved-state query.
        let currently_saved = self.saved_ids.contains(&track_id);
        let result = if currently_saved {
            api.remove_saved_tracks(std::slice::from_ref(&track_id))
                .await
                .map(|()| false)
        } else {
            api.save_tracks(std::slice::from_ref(&track_id))
                .await
                .map(|()| true)
        };
        match result {
            Ok(saved) => {
                if saved {
                    self.saved_ids.insert(track_id.clone());
                } else {
                    self.saved_ids.remove(&track_id);
                }
                self.emit(BackendEvent::TrackSaveUpdated { track_id, saved });
            }
            Err(e) => self.emit(BackendEvent::Error(friendly_error(&e))),
        }
    }
}

impl PlayerHandle {
    /// Spawns the backend worker thread.
    ///
    /// The worker owns `cmd_rx` and `events`; it processes [`UiCommand`]s and
    /// pushes [`BackendEvent`]s back to the UI.
    pub fn spawn(cmd_rx: Receiver<UiCommand>, events: Sender<BackendEvent>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name("player-worker".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        let _ = events.send(BackendEvent::Error(format!(
                            "failed to start Tokio runtime: {e}"
                        )));
                        return;
                    }
                };
                let mut worker = {
                    // Enter the runtime so `tokio::spawn` works for the
                    // Soloist client and its startup-hint task below.
                    let _guard = runtime.enter();
                    let addr = crate::config::Config::load()
                        .soloist_ws_addr
                        .map(|configured| resolve_soloist_addr(&configured))
                        .unwrap_or_else(|| DEFAULT_SOLOIST_ADDR.to_string());
                    let soloist = soloist::spawn(addr, events.clone());
                    // Startup hint: the Soloist client only emits Infos on
                    // connection transitions, so with Soloist absent at
                    // startup nothing would be said — after a short grace
                    // period, say it once here.
                    tokio::spawn({
                        let soloist = soloist.clone();
                        let events = events.clone();
                        async move {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            if !soloist.state().connected {
                                let _ = events.send(BackendEvent::Info(
                                    DISCONNECTED_INFO.to_string(),
                                ));
                            }
                        }
                    });
                    PlayerWorker::new(events, soloist)
                };
                while !shutdown_clone.load(Ordering::SeqCst) {
                    match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                        // Each command runs to completion on the runtime; the
                        // event pump spawned inside keeps running on the
                        // runtime's worker threads between commands.
                        Ok(cmd) => runtime.block_on(worker.handle_command(cmd)),
                        Err(mpsc::RecvTimeoutError::Timeout) => worker.check_soloist_takeover(),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Dropping the runtime here shuts down the worker threads and
                // aborts the spawned tasks (event pump, metadata fetches).
            })
            .expect("failed to spawn player worker thread");
        Self {
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for PlayerHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
