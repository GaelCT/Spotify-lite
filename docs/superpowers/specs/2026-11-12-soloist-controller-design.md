# spotify-lite ↔ Spotify Soloist integration — design

Date: 2026-11-12. Status: awaiting approval.

## Intent and why

spotify-lite's own playback engine (librespot) cannot play audio for the
user's Spotify account: Spotify refuses audio decryption keys for accounts in
the post-cutoff cohort, permanently and with no client-side workaround
(verified 2026-11-12: live probe 0/2 keys on both token and stored-credential
sessions; upstream librespot#1649 locked with "no workaround known").
Everything else in the app works (library, search, playlists, queue logic,
UI).

Spotify Soloist is Spotify's official headless Connect client (Linux/Windows,
`github.com/spotify/soloist`). Started with `--ws 127.0.0.1:9090` it exposes a
local JSON WebSocket API: playback commands (play/pause/skip/seek/volume/
shuffle/repeat/add_to_queue/activate), real-time events (playback_state,
track_changed, position_sync, queue_changed), and no polling or API rate
limits. Because Soloist is an official client, audio works for any account,
including the user's.

**Goal: keep spotify-lite as the GUI, let Soloist be the audio engine.** The
app becomes the controller; Soloist becomes the renderer. All existing UI and
library work stays relevant.

## Goals

- Clicking any track/playlist/album in the existing UI plays audio through
  Soloist on this PC, with the account that librespot cannot serve.
- Now-playing bar shows real state: track (with cover art — Soloist provides
  image URLs, which librespot never did), playing/paused, interpolated
  position, volume, shuffle, repeat.
- Play/pause/next/prev/seek/volume/shuffle/repeat controls in the existing UI
  drive Soloist.
- The Queue screen placeholder finally shows the real up-next queue.
- Browsing/library/search stay exactly as they are (Web API, already working).

## Non-goals (v1)

- Removing or degrading the librespot backend (it still works for unaffected
  accounts and auto-heals if Spotify reopens key issuance — the block-detection
  probe stays).
- Transferring playback to other Spotify devices or building a device picker
  (the `activate` command is used only to make Soloist itself active; full
  transfer UI is later work).
- Jam, podcasts/audiobooks-specific UI, playback speed, crossfade/normalization
  controls (Soloist owns these; not exposed in v1).
- Auto-launching or bundling Soloist (it is Spotify's binary under Spotify's
  license; the app only connects to it).

## Anti-goals

- Anything that breaks browsing when Soloist is not running. Soloist-less
  behavior must be identical to today.
- New transitive dependencies. `tokio-tungstenite 0.28` is already in
  Cargo.lock; it becomes a direct dependency and nothing new enters the tree.
- Polling loops against the Spotify Web API for playback state. Soloist pushes
  events over the WebSocket; the app never polls Spotify for playback.
- Designing around undocumented Soloist fields (e.g. no play-offset parameter
  is documented; see "Play semantics" below).

## Constraints

- Soloist must be installed and started by the user. Current builds (1.3.x) require a
  device name and a personal developer API key:
  `soloist -n "spotify-lite" -k <SOLOIST_API_KEY> --ws 127.0.0.1:9090`. The key is
  generated per-user in the Spotify for Developers dashboard ("Spotify Soloist API
  Key"). Login is Connect-based one-time pairing from the official Spotify app on the
  same network (no browser OAuth); the session persists in Soloist's data dir.
  Premium is required for setup. Builds expire every 90 days (re-download). The app
  surfaces a clear status when Soloist is absent.
- The WebSocket API has no authentication (local integration surface); connect
  to 127.0.0.1 only.
- Windows x86-64 host (Soloist desktop build available since 2026-10).

## Architecture

New module `src/soloist.rs` — a Soloist WebSocket client living on the
existing Tokio runtime in the player worker thread (same pattern as the
librespot session):

- **Connection task**: connect to `ws://127.0.0.1:9090` (configurable), read
  JSON text frames, deserialize into an internal `SoloistEvent` enum (serde,
  shapes per the official WebSocket API reference). Reconnect with capped
  exponential backoff (0.5 s → 15 s) whenever Soloist is not reachable; each
  transition updates a `connected` flag so the UI always knows. After connect
  and on every `queue_changed`, send `get_queue {limit: 0}` — broadcast
  `queue_changed` only carries 10 entries; the explicit query returns all.
- **State mirror** `SoloistState`: connected, logged_in, is_active, status
  (idle/playing/paused/buffering), current item (mapped to the existing
  `Track` type, including `album_art_url` from `decorations.visual_identity`),
  context name, position anchor (`position_ms` + `timestamp_ms` + `speed`),
  volume 0-100, shuffle, repeat, upcoming/previous queue entries.
- **Position interpolation**: the UI already re-renders continuously; position
  is computed from the anchor at draw time (server wall-clock skew ignored in
  v1; error is sub-second on localhost).
- **Commands**: an async fn per command (`play_uri(Option<uri>)`, `pause()`,
  `resume()`, `skip_next()`, `skip_prev()`, `seek(ms)`, `set_volume(0-100)`,
  `set_shuffle(bool)`, `set_repeat(RepeatMode)`, `add_to_queue(uri)`,
  `activate()`) sending `{"type":"command",...}` frames over a split sink.
  Repeat mapping per docs: off = both false; context = context only;
  track = track only.
- **Error events** from Soloist are forwarded to the UI status line through
  the existing `BackendEvent::Error` path.

`SoloistState` converts losslessly into the existing `PlayerSnapshot`
(volume % → 0..=65535; status → playing flag; item → `Track`). The UI does not
learn about Soloist — it keeps consuming `app.player`.

**Command routing** lives in `app.rs::send()`: while `SoloistState.logged_in`
is true, playback `UiCommand`s (PlayTrack/PlayContext/TogglePlay/Next/
Previous/SeekMs/SetVolume/ToggleShuffle/CycleRepeat) are translated to Soloist
commands and the librespot worker never sees them; all other commands flow to
the worker unchanged. When Soloist is absent, routing is exactly today's
behavior (librespot path, including the blocked-detection banner). Preference
order when both are usable: Soloist (it makes sound for this account).

**Takeover**: the first time Soloist becomes the active playback source while
the librespot backend has an audible session (only possible for unaffected
accounts), a pause is sent to the librespot worker first so the two engines
never play simultaneously.

**Play semantics** (documented API has no start-offset): clicking a track in a
playlist/album plays that track's URI via `play`, then appends the remaining
context URIs in order via `add_to_queue`. The URIs travel inside the existing
`UiCommand`s — the UI already holds the full visible list at click time
(track table / loaded playlist / album details), so "the rest of the context"
means exactly what the user sees, with no new fetch plumbing. Artist and
search rows send no context list and play as single tracks (same fallback the
librespot path uses today for artist contexts, whose URIs Soloist's `play`
does not accept). Playing a ▶ playlist/album header plays the context URI
directly (Soloist fills the queue from the context). If Soloist is logged in
but not the active Connect device when a play command arrives, `activate` is
sent first (fire-and-forget; failures surface as Soloist error events).

## Files

- `src/soloist.rs` (new): WS client, event/state mirror, commands, backoff,
  `PlayerSnapshot` conversion.
- `Cargo.toml`: add `tokio-tungstenite = "0.28"` (already in the lockfile; no
  new transitive deps).
- `src/types.rs`: playback `UiCommand` variants carry `queue: Vec<String>`
  (the remaining visible context URIs); new `BackendEvent` variant shipping
  `SoloistState` to the app; `PLAYBACK_BLOCKED_MESSAGE` stays as-is.
- `src/config.rs`: `soloist_ws_addr` (default `127.0.0.1:9090`), read at
  startup.
- `src/app.rs`: own the `SoloistHandle`, route playback commands per above,
  merge Soloist snapshots into `app.player` (Soloist snapshot wins while
  logged_in; otherwise librespot snapshot as today), pause the librespot
  worker on takeover.
- `src/ui/settings.rs`: Soloist address field (label notes it takes effect on
  restart), saved to config.toml.
- `src/ui/browser.rs`: pass the visible context URIs into play commands; the
  Queue placeholder (`browser.rs` today) shows the upcoming queue from
  Soloist state.
- `examples/soloist_probe.rs` (new): diagnostic — connect, print auth_state /
  playback_state / full queue, one harmless command round-trip. Same
  conventions as `webapi_probe.rs` / `playback_probe.rs`.

## UX

- Soloist absent: status line shows "Soloist not detected — start it with:
  `soloist --ws 127.0.0.1:9090` (audio playback needs Spotify's official
  client for this account)". App otherwise behaves exactly as today.
- Soloist present but logged out: "Soloist is running — log in to Soloist once
  (Premium) to enable playback."
- Soloist live: normal playback UI; now-playing shows cover art.
- The librespot blocked-banner (from the 2026-11-12 fix) only shows when the
  librespot backend is the active one; when Soloist drives, it is suppressed.

## Verification

1. `cmd //c vcbuild.bat check` — 0 errors (5 pre-existing rspotify
   deprecation warnings tolerated).
2. `soloist_probe` against a running Soloist: auth_state, playback_state,
   full queue, command round-trip (play + pause on a test URI).
3. Release build; launch with Soloist running: click a track in a playlist →
   audio out of the PC speakers within ~2 s; now-playing bar updates on
   track_changed; seek/volume/shuffle/repeat all take effect; queue screen
   lists upcoming tracks.
4. Kill Soloist mid-session: app degrades to today's behavior with the
   "start Soloist" hint (no crash, no hang).

## Risks

- Soloist WebSocket schema drift (it is documented but young; v0.x surface).
  Mitigation: serde defaults + unknown-variant catch-all; probe example for
  quick re-checks.
- 90-day build expiry will look like an app bug to the user. Mitigation: the
  "Soloist not detected" hint names re-download.
- Server clock skew in position_sync is ignored in v1 (localhost impact is
  sub-second).

## Calls made here (reviewer round, 2026-11-12)

- Queue URIs at click time come from the UI's visible list, carried inside
  the existing `UiCommand`s — no new fetch path; freshness equals what the
  user sees on screen.
- `activate` is sent automatically before a play command whenever Soloist is
  logged in but not the active device (hitting play in our GUI is expected to
  produce sound from this PC).
- On Soloist takeover, the librespot worker is paused first — never double
  audio.
- Soloist address edited in Settings takes effect on the next app start
  (label says so; no hot-reconfigure plumbing in v1).
- Broadcast `queue_changed` carries only 10 entries; the client issues
  `get_queue {limit: 0}` after connect and on each `queue_changed`.
- The Queue screen lives in `src/ui/browser.rs` (no separate queue.rs).

## Calls made here (original, still standing)

- Soloist is the preferred playback source whenever it is logged in, even if
  librespot could also play (old accounts).
- Soloist connection is permanent-with-backoff from app start, not on-demand.
- Queue screen gets filled as part of this feature (it was placeholder debt).
- Volume mapping % → u16 full-scale, linear.
