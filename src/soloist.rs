//! Spotify Soloist WebSocket client.
//!
//! [Spotify Soloist](https://github.com/spotify/soloist) is Spotify's
//! official headless Spotify Connect client. Started with
//! `soloist --ws 127.0.0.1:9090` it exposes a local WebSocket API with no
//! client authentication (a local integration surface; connect to 127.0.0.1
//! only): JSON text frames in both directions, server-push events, and a
//! documented command set — see Spotify's "Soloist WebSocket API reference".
//!
//! spotify-lite's own playback engine (librespot) cannot play audio for this
//! user's account — Spotify refuses to issue audio keys for it server-side
//! (see `src/player.rs`) — so Soloist acts as the audio engine while this
//! app remains the GUI. This module is the client for Soloist's WS API: it
//! keeps a permanent connection with capped exponential backoff, mirrors
//! Soloist's playback state into [`SoloistState`], sends typed
//! [`SoloistCommand`]s, and converts the mirror into the shared
//! [`PlayerSnapshot`] so the UI never learns about Soloist.
//!
//! # Connection lifecycle
//!
//! [`spawn`] must be called from within an existing Tokio runtime (in this
//! app, the player-worker thread). It spawns one manager task on that
//! runtime which connects to `ws://{addr}` via
//! `tokio_tungstenite::connect_async` and, whenever the connection fails or
//! drops, sleeps 0.5 s and retries with the delay doubling up to a 15 s cap
//! — forever, so Soloist started later is picked up without an app restart.
//! On every successful connect the manager sends `get_auth_state`,
//! `get_state` and `get_queue {limit: 0}` (broadcast `queue_changed` events
//! only carry 10 entries; `limit: 0` requests all of them), splits the
//! socket, and spawns a writer task draining the command queue onto the
//! sink while the manager itself reads events in a loop.
//!
//! # Wire protocol
//!
//! Server events deserialize into [`SoloistEvent`], an internally-tagged
//! enum keyed on the `type` field with an `Unknown` catch-all so a frame of
//! an unexpected type never fails the stream. All fields are optional
//! (serde defaults) for tolerance against schema drift — Soloist's API is
//! documented but young (v0.x) — and an unknown `status`/`repeat` string
//! leaves the previous mirrored value untouched instead of dropping the
//! event. Frames that do not parse at all are skipped; the connection stays
//! open and the next frame is a fresh attempt.
//!
//! Commands are sent as `{"type":"command",...}` JSON frames, one frame per
//! [`SoloistCommand`] — except `SetRepeat`, which expands to Soloist's two
//! boolean repeat commands in the documented order (see [`command_frames`]).

use std::sync::{mpsc::Sender as StdSender, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::types::{BackendEvent, PlayerSnapshot, PositionAnchor, RepeatMode, Track};

/// Reconnect delay after the first failed attempt; doubles per failure up
/// to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Cap for the reconnect backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// Info shown when the connection to Soloist is (re-)established.
pub const CONNECTED_INFO: &str = "Soloist found.";
/// Info shown when Soloist cannot be reached; names the exact start command.
pub const DISCONNECTED_INFO: &str = "Soloist not detected — start it with: soloist --ws 127.0.0.1:9090 (audio playback needs Spotify's official client for this account)";
/// Info shown when Soloist reports a logged-in Spotify session.
pub const READY_INFO: &str = "Soloist ready — playback goes through Spotify Soloist.";

/// Playback status of the Soloist session, mirroring the `status` field of
/// Soloist's `playback_state` / `playback_changed` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoloistStatus {
    /// Nothing loaded or playback stopped.
    #[default]
    Idle,
    /// Actively playing audio.
    Playing,
    /// Paused.
    Paused,
    /// Buffering; treated as "playing" by the snapshot conversion.
    Buffering,
}

impl SoloistStatus {
    /// Maps a wire `status` string to a status. Returns `None` for unknown
    /// values so callers leave the mirrored state untouched (schema drift).
    fn from_wire(status: &str) -> Option<Self> {
        match status {
            "idle" => Some(Self::Idle),
            "playing" => Some(Self::Playing),
            "paused" => Some(Self::Paused),
            "buffering" => Some(Self::Buffering),
            _ => None,
        }
    }
}

/// Mirror of Soloist's playback state, updated on every accepted event.
///
/// This is the single source of truth for "is Soloist usable right now"
/// (`connected && logged_in`) that the player worker routes commands on.
#[derive(Clone, Debug, Default)]
pub struct SoloistState {
    /// Whether the WebSocket connection to Soloist is currently open.
    pub connected: bool,
    /// Whether Soloist has an authenticated Spotify Connect session.
    pub logged_in: bool,
    /// Whether Soloist is the active Spotify Connect device.
    pub is_active: bool,
    /// Spotify Connect device name reported by Soloist.
    pub device_name: String,
    /// Current playback status.
    pub status: SoloistStatus,
    /// The currently loaded track, mapped from Soloist's entity envelope
    /// (includes cover art URLs, which librespot never provided).
    pub current: Option<Track>,
    /// Display name of the current playback context (playlist/album/...),
    /// if known.
    pub context_name: Option<String>,
    /// Last reported position anchor, used by the UI to interpolate the
    /// live position between `position_sync` events.
    pub position: Option<PositionAnchor>,
    /// Output volume in percent: `0..=100`.
    pub volume_percent: u8,
    /// Whether shuffle is enabled.
    pub shuffle: bool,
    /// Current repeat mode.
    pub repeat: RepeatMode,
    /// Upcoming queue entries, next to play onward (full queue, requested
    /// with `get_queue {limit: 0}` because broadcast `queue_changed` events
    /// carry at most 10 entries).
    pub upcoming: Vec<Track>,
}

/// A playback command sent to Soloist. Fire-and-forget from the caller's
/// perspective: outcomes arrive asynchronously as Soloist events (and as
/// `command_result` / `error` frames, the latter forwarded to the UI).
// The dead_code allow is scoped to this enum, not the module: the player
// worker (the lib crate) constructs every variant, but `examples/soloist_probe.rs`
// includes this file via `#[path]` and — being a harmless probe — sends only
// Pause/Resume, so the per-crate analysis flags the rest as never constructed
// there. The full command set is protocol surface, not dead code.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum SoloistCommand {
    /// Resume playback (`play` without a URI) or start playing the given
    /// Spotify URI (track, album, playlist, or episode).
    Play(Option<String>),
    /// Pause playback.
    Pause,
    /// Resume the current track. Soloist has no dedicated "resume" command:
    /// this sends `play` without a URI.
    Resume,
    /// Skip to the next track.
    SkipNext,
    /// Skip to the previous track (or restart the current one).
    SkipPrev,
    /// Seek to an absolute position within the track, in milliseconds.
    Seek(u32),
    /// Set output volume: `0..=100` (percent).
    SetVolume(u8),
    /// Enable or disable shuffle.
    SetShuffle(bool),
    /// Set repeat mode. Expands to two Soloist frames in the documented
    /// order (e.g. `One` = `set_repeat_context(false)` then
    /// `set_repeat_track(true)`).
    SetRepeat(RepeatMode),
    /// Append a Spotify *track* URI to the play queue.
    AddToQueue(String),
    /// Make Soloist the active Spotify Connect device for this account.
    Activate,
}

/// Handle to the running Soloist client: read the mirrored state and queue
/// commands. Owned by the player worker, which routes playback commands to
/// it while Soloist is logged in (see `src/player.rs`).
#[derive(Clone)]
pub struct SoloistHandle {
    state: Arc<Mutex<SoloistState>>,
    commands: UnboundedSender<SoloistCommand>,
}

impl SoloistHandle {
    /// Snapshot of the currently mirrored Soloist state.
    pub fn state(&self) -> SoloistState {
        self.state.lock().unwrap().clone()
    }

    /// Queues a command for the writer task. Fire-and-forget: while Soloist
    /// is unreachable the command stays queued and is sent on the next
    /// connect, and delivery failures surface as Soloist `error` events.
    pub fn command(&self, cmd: SoloistCommand) {
        let _ = self.commands.send(cmd);
    }
}

/// Starts the Soloist WebSocket client and returns its handle.
///
/// Must be called from within an existing Tokio runtime: the manager task
/// is spawned on it. `events` is the app's backend-event channel;
/// [`BackendEvent`]s emitted here follow the same replace-wholesale rules
/// as the librespot backend's.
pub fn spawn(addr: String, events: StdSender<BackendEvent>) -> SoloistHandle {
    let state = Arc::new(Mutex::new(SoloistState::default()));
    let (commands, command_rx) = unbounded_channel::<SoloistCommand>();
    tokio::spawn(connection_manager(addr, Arc::clone(&state), command_rx, events));
    SoloistHandle { state, commands }
}

/// Permanent connection manager: connects (and reconnects forever) to
/// `ws://{addr}`, mirrors events into `state`, and emits UI events.
async fn connection_manager(
    addr: String,
    state: Arc<Mutex<SoloistState>>,
    commands: UnboundedReceiver<SoloistCommand>,
    events: StdSender<BackendEvent>,
) {
    // Shared between the per-connection writer tasks (only one writer exists
    // at a time; a dying connection's writer is aborted before the next one
    // is spawned).
    let commands = Arc::new(tokio::sync::Mutex::new(commands));
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let url = format!("ws://{addr}");
        match connect_async(&url).await {
            Ok((socket, _response)) => {
                backoff = INITIAL_BACKOFF;
                set_connected(&state, &events, true);
                let (mut sink, mut stream) = socket.split();
                // Refresh the full state after every (re)connect: login
                // state, the playback snapshot, and the *complete* queue.
                for query in [
                    serde_json::json!({ "type": "command", "command": "get_auth_state" }),
                    serde_json::json!({ "type": "command", "command": "get_state" }),
                    serde_json::json!({ "type": "command", "command": "get_queue", "limit": 0 }),
                ] {
                    if sink.send(Message::Text(query.to_string().into())).await.is_err() {
                        break;
                    }
                }
                // Writer task: drains the command queue onto this
                // connection's sink (one frame per command; `SetRepeat`
                // produces two frames in the documented order).
                let writer = tokio::spawn({
                    let commands = Arc::clone(&commands);
                    async move {
                        loop {
                            let cmd = commands.lock().await.recv().await;
                            let Some(cmd) = cmd else { return };
                            for frame in command_frames(&cmd) {
                                if sink.send(Message::Text(frame.to_string().into())).await.is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                });
                // Read events until EOF or error; then tear the connection
                // down and fall through to the reconnect backoff below.
                while let Some(msg) = stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => apply_frame(&text, &state, &events),
                        Ok(Message::Close(_)) => break,
                        // Ping/pong is answered by the protocol layer;
                        // anything else is ignored.
                        Ok(_) => {}
                        Err(err) => {
                            tracing::debug!("soloist connection ended: {err}");
                            break;
                        }
                    }
                }
                writer.abort();
                set_connected(&state, &events, false);
            }
            Err(err) => {
                tracing::debug!("soloist connect to {url} failed: {err}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Sets the `connected` flag. On disconnect the session-derived fields are
/// reset immediately so command routing falls back to the librespot path,
/// and a default [`BackendEvent::PlayerSnapshot`] is emitted to clear the
/// now-playing bar (mirroring the logout path in [`apply_frame`]); the Info
/// message is emitted exactly on false->true / true->false transitions,
/// never per event.
fn set_connected(state: &Mutex<SoloistState>, events: &StdSender<BackendEvent>, connected: bool) {
    let info = {
        let mut s = state.lock().unwrap();
        if s.connected == connected {
            return;
        }
        s.connected = connected;
        if connected {
            CONNECTED_INFO
        } else {
            s.logged_in = false;
            s.is_active = false;
            s.status = SoloistStatus::Idle;
            DISCONNECTED_INFO
        }
    };
    if !connected {
        let _ = events.send(BackendEvent::PlayerSnapshot(PlayerSnapshot::default()));
    }
    let _ = events.send(BackendEvent::Info(info.to_string()));
}

/// Serializes one [`SoloistCommand`] into one or two Soloist command frames
/// (`{"type":"command",...}`). `SetRepeat` expands to the two documented
/// boolean repeat commands in the documented order.
fn command_frames(cmd: &SoloistCommand) -> Vec<serde_json::Value> {
    use SoloistCommand::*;
    let frame = |command: &str| serde_json::json!({ "type": "command", "command": command });
    match cmd {
        Play(None) => vec![frame("play")],
        Play(Some(uri)) => {
            vec![serde_json::json!({ "type": "command", "command": "play", "uri": uri })]
        }
        Pause => vec![frame("pause")],
        Resume => vec![frame("play")],
        SkipNext => vec![frame("skip_next")],
        SkipPrev => vec![frame("skip_prev")],
        Seek(position_ms) => {
            vec![serde_json::json!({ "type": "command", "command": "seek", "position_ms": position_ms })]
        }
        SetVolume(volume) => {
            vec![serde_json::json!({ "type": "command", "command": "set_volume", "volume": volume })]
        }
        SetShuffle(enabled) => {
            vec![serde_json::json!({ "type": "command", "command": "set_shuffle", "enabled": enabled })]
        }
        SetRepeat(RepeatMode::Off) => vec![
            serde_json::json!({ "type": "command", "command": "set_repeat_track", "enabled": false }),
            serde_json::json!({ "type": "command", "command": "set_repeat_context", "enabled": false }),
        ],
        SetRepeat(RepeatMode::All) => vec![
            serde_json::json!({ "type": "command", "command": "set_repeat_track", "enabled": false }),
            serde_json::json!({ "type": "command", "command": "set_repeat_context", "enabled": true }),
        ],
        SetRepeat(RepeatMode::One) => vec![
            serde_json::json!({ "type": "command", "command": "set_repeat_context", "enabled": false }),
            serde_json::json!({ "type": "command", "command": "set_repeat_track", "enabled": true }),
        ],
        AddToQueue(uri) => {
            vec![serde_json::json!({ "type": "command", "command": "add_to_queue", "uri": uri })]
        }
        Activate => vec![frame("activate")],
    }
}

/// Deserializes one text frame and folds it into the state mirror, emitting
/// the resulting [`BackendEvent`]s after releasing the lock. Emits a
/// [`BackendEvent::PlayerSnapshot`] on every event that changes
/// playback-relevant state while logged in; on the logged-in -> logged-out
/// transition it emits `PlayerSnapshot::default()` to clear the now-playing
/// bar (the librespot backend re-emits on its next event). `error` events
/// are forwarded as [`BackendEvent::Error`]; the login Info is emitted only
/// on the false->true transition.
fn apply_frame(text: &str, state: &Mutex<SoloistState>, events: &StdSender<BackendEvent>) {
    let Ok(event) = serde_json::from_str::<SoloistEvent>(text) else {
        return;
    };
    let mut out: Vec<BackendEvent> = Vec::new();
    let mut snapshot = false;
    {
        let mut s = state.lock().unwrap();
        match event {
            SoloistEvent::AuthState {
                logged_in,
                is_active,
                device_name,
            } => {
                let was_logged_in = s.logged_in;
                s.logged_in = logged_in;
                s.is_active = is_active;
                s.device_name = device_name;
                if logged_in && !was_logged_in {
                    out.push(BackendEvent::Info(READY_INFO.to_string()));
                }
                if !logged_in && was_logged_in {
                    out.push(BackendEvent::PlayerSnapshot(PlayerSnapshot::default()));
                }
                snapshot = logged_in;
            }
            SoloistEvent::PlaybackState {
                status,
                item,
                context,
                position,
                volume,
                is_active,
                options,
            } => {
                if let Some(status) = SoloistStatus::from_wire(&status) {
                    s.status = status;
                }
                s.current = item.as_ref().map(entity_to_track);
                s.context_name = entity_name(&context);
                if let Some(position) = position {
                    s.position = Some(position.to_anchor());
                }
                if let Some(volume) = volume {
                    s.volume_percent = clamp_volume(volume);
                }
                s.is_active = is_active;
                if let Some(options) = options {
                    s.shuffle = options.shuffle;
                    if let Some(repeat) = parse_repeat(&options.repeat) {
                        s.repeat = repeat;
                    }
                }
                snapshot = true;
            }
            SoloistEvent::TrackChanged { item } => {
                if let Some(item) = &item {
                    s.current = Some(entity_to_track(item));
                }
                snapshot = true;
            }
            SoloistEvent::PlaybackChanged { status } => {
                if let Some(status) = SoloistStatus::from_wire(&status) {
                    s.status = status;
                }
                snapshot = true;
            }
            SoloistEvent::VolumeChanged { volume } => {
                if let Some(volume) = volume {
                    s.volume_percent = clamp_volume(volume);
                }
                snapshot = true;
            }
            SoloistEvent::DeviceChanged {
                is_active,
                device_name,
            } => {
                s.is_active = is_active;
                s.device_name = device_name;
                snapshot = true;
            }
            SoloistEvent::ContextChanged { context } => {
                s.context_name = entity_name(&context);
                snapshot = true;
            }
            SoloistEvent::OptionsChanged { options } => {
                if let Some(options) = options {
                    s.shuffle = options.shuffle;
                    if let Some(repeat) = parse_repeat(&options.repeat) {
                        s.repeat = repeat;
                    }
                }
                snapshot = true;
            }
            SoloistEvent::PositionSync { position } => {
                if let Some(position) = position {
                    s.position = Some(position.to_anchor());
                }
                snapshot = true;
            }
            SoloistEvent::QueueChanged { previous, upcoming } => {
                // Play history is not displayed anywhere; the upcoming
                // entries feed the Queue screen.
                let _ = previous;
                s.upcoming = upcoming
                    .iter()
                    .filter_map(|entry| entry.item.as_ref())
                    .map(entity_to_track)
                    .collect();
                snapshot = true;
            }
            SoloistEvent::Error { message } => {
                out.push(BackendEvent::Error(message));
            }
            // `command_result` only acknowledges acceptance; `Unknown`
            // covers event types this client does not know about.
            SoloistEvent::CommandResult | SoloistEvent::Unknown => {}
        }
        if snapshot && s.logged_in {
            out.push(BackendEvent::PlayerSnapshot(snapshot_from_soloist(&s)));
        }
    }
    for event in out {
        let _ = events.send(event);
    }
}

/// Converts the mirrored [`SoloistState`] into the shared [`PlayerSnapshot`]
/// (see `src/types.rs`). `Playing`/`Buffering` map to `playing: true`;
/// volume maps linearly from percent to librespot's `0..=65535` full scale;
/// the position is interpolated from the anchor while it is advancing and
/// frozen at the anchor's position when paused. `playback_blocked` is
/// always `false`: once Soloist drives playback, audio works for this
/// account.
pub fn snapshot_from_soloist(state: &SoloistState) -> PlayerSnapshot {
    let position_anchor = state.position;
    let position_ms = match position_anchor {
        Some(anchor) if anchor.speed > 0.0 => anchor.position_at(now_ms()),
        Some(anchor) => anchor.position_ms,
        None => 0,
    };
    PlayerSnapshot {
        current: state.current.clone(),
        playing: matches!(state.status, SoloistStatus::Playing | SoloistStatus::Buffering),
        position_ms,
        volume: (state.volume_percent as u32 * 65535 / 100) as u16,
        shuffle: state.shuffle,
        repeat: state.repeat,
        playback_blocked: false,
        position_anchor,
        upcoming: state.upcoming.clone(),
    }
}

/// Current Unix time in milliseconds (the fallback `0` only occurs when the
/// system clock predates the epoch).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Maps a Soloist entity envelope to the shared [`Track`] type. Unknown
/// fields fall back to zero values (same best-effort convention as the
/// librespot backend's placeholder tracks).
fn entity_to_track(entity: &Entity) -> Track {
    Track {
        id: entity
            .uri
            .strip_prefix("spotify:track:")
            .unwrap_or(&entity.uri)
            .to_string(),
        uri: entity.uri.clone(),
        title: entity.decorations.identity.name.clone(),
        artists: entity
            .decorations
            .creators
            .iter()
            .filter_map(|creator| creator.entity.as_ref())
            .map(|artist| artist.decorations.identity.name.clone())
            .filter(|name| !name.is_empty())
            .collect(),
        album: entity
            .decorations
            .parent
            .entity
            .as_ref()
            .map(|parent| parent.decorations.identity.name.clone())
            .unwrap_or_default(),
        album_art_url: entity
            .decorations
            .visual_identity
            .cover
            .iter()
            .map(|cover| cover.url.clone())
            .find(|url| !url.is_empty()),
        duration_ms: entity.decorations.playback.duration_ms,
        track_number: 0,
        disc_number: 0,
        is_saved: false,
    }
}

/// Display name of an optional entity envelope (`None` when absent or
/// unnamed).
fn entity_name(entity: &Option<Entity>) -> Option<String> {
    entity
        .as_ref()
        .map(|e| e.decorations.identity.name.clone())
        .filter(|name| !name.is_empty())
}

/// Maps a wire `repeat` option to [`RepeatMode`]; `None` = unknown, leave
/// the mirrored state untouched.
fn parse_repeat(wire: &str) -> Option<RepeatMode> {
    match wire {
        "off" => Some(RepeatMode::Off),
        "context" => Some(RepeatMode::All),
        "track" => Some(RepeatMode::One),
        _ => None,
    }
}

/// Clamps a wire volume percentage (Soloist documents `0..=100` as an
/// integer but sends it as a JSON number) into range.
fn clamp_volume(volume: f64) -> u8 {
    volume.clamp(0.0, 100.0) as u8
}

/// Server event frame, keyed on the `type` field. The `Unknown` catch-all
/// (`#[serde(other)]`) makes any event type this client does not know about
/// parse successfully, so schema drift never fails a frame.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SoloistEvent {
    /// `auth_state`: login/active-device/device-name state.
    AuthState {
        #[serde(default)]
        logged_in: bool,
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        device_name: String,
    },
    /// `playback_state`: full playback snapshot.
    PlaybackState {
        #[serde(default)]
        status: String,
        #[serde(default)]
        item: Option<Entity>,
        #[serde(default)]
        context: Option<Entity>,
        #[serde(default)]
        position: Option<Position>,
        #[serde(default)]
        volume: Option<f64>,
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        options: Option<Options>,
    },
    /// `track_changed`: new current item.
    TrackChanged {
        #[serde(default)]
        item: Option<Entity>,
    },
    /// `playback_changed`: new status (`idle`/`playing`/`paused`/`buffering`).
    PlaybackChanged {
        #[serde(default)]
        status: String,
    },
    /// `volume_changed`: new volume percentage.
    VolumeChanged {
        #[serde(default)]
        volume: Option<f64>,
    },
    /// `device_changed`: active-device flag and/or device name changed.
    DeviceChanged {
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        device_name: String,
    },
    /// `context_changed`: new playback context entity.
    ContextChanged {
        #[serde(default)]
        context: Option<Entity>,
    },
    /// `options_changed`: new shuffle/repeat options.
    OptionsChanged {
        #[serde(default)]
        options: Option<Options>,
    },
    /// `position_sync`: new position anchor.
    PositionSync {
        #[serde(default)]
        position: Option<Position>,
    },
    /// `queue_changed`: new play queue; also the response to `get_queue`.
    QueueChanged {
        #[serde(default)]
        previous: Vec<QueueEntry>,
        #[serde(default)]
        upcoming: Vec<QueueEntry>,
    },
    /// `command_result`: a control command was accepted and dispatched. The
    /// acknowledged command name is not captured — the frame only marks
    /// acceptance and carries no state by itself.
    CommandResult,
    /// `error`: the server rejected a command or reported a dispatch error;
    /// forwarded to the UI status line.
    Error {
        #[serde(default)]
        message: String,
    },
    /// Any other event type.
    #[serde(other)]
    Unknown,
}

/// Position anchor wire shape (`playback_state.position`,
/// `position_sync.position`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Position {
    position_ms: u32,
    timestamp_ms: i64,
    speed: f32,
}

impl Position {
    fn to_anchor(&self) -> PositionAnchor {
        PositionAnchor {
            position_ms: self.position_ms,
            timestamp_ms: self.timestamp_ms,
            speed: self.speed,
        }
    }
}

/// `options` wire shape (`playback_state.options`, `options_changed.options`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Options {
    shuffle: bool,
    repeat: String,
}

/// Queue entry wire shape (`queue_changed.previous[]`/`upcoming[]`); only
/// the item entity is used.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct QueueEntry {
    item: Option<Entity>,
}

/// Entity envelope shared by playback items, contexts, parents, creators
/// and queue entries. Only the decorations spotify-lite displays are
/// captured; everything else is ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Entity {
    uri: String,
    #[allow(dead_code)]
    entity_type: String,
    decorations: Decorations,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Decorations {
    identity: Identity,
    visual_identity: VisualIdentity,
    parent: Parent,
    creators: Vec<Creator>,
    playback: PlaybackInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Identity {
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct VisualIdentity {
    cover: Vec<Cover>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Cover {
    url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Parent {
    // `Box` breaks the Entity -> Decorations -> Parent -> Entity cycle.
    entity: Option<Box<Entity>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Creator {
    entity: Option<Entity>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PlaybackInfo {
    duration_ms: u32,
}
