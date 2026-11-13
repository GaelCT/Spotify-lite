//! Diagnostic probe (not part of the app): connects to Spotify Soloist's
//! local WebSocket API via the app's `soloist` client and watches the
//! mirrored state: prints every `BackendEvent` for 8 s, dumps
//! `SoloistHandle::state()`, then — only if Soloist is logged in and
//! actually playing — pauses and resumes playback to prove commands work.
//! Harmless by design: never starts playback, never activates the device.
//!
//! Run: `cmd //c vcbuild.bat run --example soloist_probe`
//! Address override: `SOLOIST_ADDR=127.0.0.1:9090` (that is the default).

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

#[path = "../src/soloist.rs"]
mod soloist;
#[path = "../src/types.rs"]
mod types;

use soloist::{SoloistCommand, SoloistStatus};
use types::{BackendEvent, Track};

const EVENT_WINDOW: Duration = Duration::from_secs(8);
const SETTLE: Duration = Duration::from_secs(1);

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let addr = std::env::var("SOLOIST_ADDR").unwrap_or_else(|_| "127.0.0.1:9090".to_string());
    println!("probe: soloist client connecting to ws://{addr}");

    let (events_tx, events_rx) = channel::<BackendEvent>();
    let handle = soloist::spawn(addr, events_tx);

    println!("-- events for {}s --", EVENT_WINDOW.as_secs());
    drain_events(&events_rx, EVENT_WINDOW);

    let state = handle.state();
    println!(
        "state logged_in={} is_active={} status={:?} device_name={:?} track={:?} \
         volume_percent={} shuffle={} repeat={:?} upcoming={}",
        state.logged_in,
        state.is_active,
        state.status,
        state.device_name,
        title(&state.current),
        state.volume_percent,
        state.shuffle,
        state.repeat,
        state.upcoming.len()
    );

    if state.logged_in && state.status == SoloistStatus::Playing {
        println!("-- playing: Pause -> observe -> Resume --");
        let before = state.status;
        handle.command(SoloistCommand::Pause);
        drain_events(&events_rx, SETTLE);
        let paused = handle.state();
        println!("after Pause:  status {:?} -> {:?}", before, paused.status);

        handle.command(SoloistCommand::Resume);
        drain_events(&events_rx, SETTLE);
        let resumed = handle.state();
        println!("after Resume: status {:?} -> {:?}", paused.status, resumed.status);
    } else {
        println!("-- not playing (logged_in={}, status={:?}): no commands sent --",
            state.logged_in, state.status);
    }

    println!("probe done");
}

/// Title of the current track, or a placeholder when nothing is loaded.
fn title(track: &Option<Track>) -> String {
    track.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "<none>".into())
}

/// Prints every event that arrives within `window`, one compact line each.
fn drain_events(rx: &Receiver<BackendEvent>, window: Duration) {
    let deadline = Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        // Err covers both timeout (window elapsed) and sender disconnect.
        match rx.recv_timeout(remaining) {
            Ok(event) => print_event(&event),
            Err(_) => return,
        }
    }
}

/// One line per event: variant name plus the fields that matter for a probe.
fn print_event(event: &BackendEvent) {
    match event {
        BackendEvent::Authenticated { user_name } => println!("event Authenticated user={user_name}"),
        BackendEvent::AuthFailed(reason) => println!("event AuthFailed reason={reason}"),
        BackendEvent::LoggedOut => println!("event LoggedOut"),
        BackendEvent::PlayerSnapshot(s) => println!(
            "event PlayerSnapshot track={:?} playing={} position_ms={} volume={} upcoming={}",
            title(&s.current),
            s.playing,
            s.position_ms,
            s.volume,
            s.upcoming.len()
        ),
        BackendEvent::LibraryLoaded { playlists, albums, artists } => println!(
            "event LibraryLoaded playlists={} albums={} artists={}",
            playlists.len(),
            albums.len(),
            artists.len()
        ),
        BackendEvent::PlaylistTracksLoaded { playlist_id, tracks } => {
            println!("event PlaylistTracksLoaded playlist={playlist_id} tracks={}", tracks.len())
        }
        BackendEvent::AlbumLoaded(d) => println!("event AlbumLoaded {} tracks={}", d.info.name, d.tracks.len()),
        BackendEvent::ArtistLoaded(d) => println!(
            "event ArtistLoaded {} albums={} top_tracks={}",
            d.info.name,
            d.albums.len(),
            d.top_tracks.len()
        ),
        BackendEvent::SearchLoaded(r) => println!(
            "event SearchLoaded tracks={} albums={} artists={} playlists={}",
            r.tracks.len(),
            r.albums.len(),
            r.artists.len(),
            r.playlists.len()
        ),
        BackendEvent::TrackSaveUpdated { track_id, saved } => {
            println!("event TrackSaveUpdated track={track_id} saved={saved}")
        }
        BackendEvent::Error(message) => println!("event Error {message}"),
        BackendEvent::Info(message) => println!("event Info {message}"),
    }
}
