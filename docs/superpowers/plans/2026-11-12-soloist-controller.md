# Soloist Controller Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** spotify-lite's GUI drives Spotify Soloist (official headless Connect client) as its audio engine over Soloist's local WebSocket API, so playback works for the user's blocked account while every existing screen stays unchanged.

**Architecture:** A new `src/soloist.rs` module implements the documented Soloist WebSocket protocol (JSON text frames, push events) as a client on the existing player-worker Tokio runtime: reconnect-with-backoff, a `SoloistState` mirror, and typed commands. The player worker routes the eight playback `UiCommand`s to Soloist whenever its state says `logged_in`; otherwise behavior is exactly today's librespot path (including the blocked-account banner). `SoloistState` converts into the existing `PlayerSnapshot` (plus a `PositionAnchor` for live position interpolation and `upcoming` for the Queue screen), so the UI never learns about Soloist.

**Tech Stack:** Rust 2021, tokio (existing), tokio-tungstenite 0.28 (promote from transitive to direct dep; already in Cargo.lock — no new transitive deps), serde/serde_json (existing), eframe/egui 0.31 (existing).

**Spec:** `docs/superpowers/specs/2026-11-12-soloist-controller-design.md` (read it first; this plan implements it, with one relocation noted below).

## Global Constraints

- Build/verify commands: `cmd //c vcbuild.bat check`, `cmd //c vcbuild.bat build --release`, `cmd //c vcbuild.bat run --example <name>` — cargo is NOT on PATH; always via the wrapper, from `C:\Users\gaelc\Downloads\dorian\spotify-lite`. Bash tool runs Git Bash: Unix syntax, `/dev/null`, forward slashes.
- **No automated tests exist in this project and none may be added** (documented debt; project convention is probe examples + launch checks). Every task's "test" step is therefore: compile clean (0 errors; the 5 pre-existing rspotify deprecation warnings are tolerated), the `soloist_probe` diagnostic where applicable, and GUI smoke checks.
- NOT a git repo: no commit steps.
- Zero new transitive dependencies.
- Soloist WS addr default `127.0.0.1:9090`; connect to 127.0.0.1 only.
- Match existing code style: doc comments on pub items, module-level `//!` docs, no new deps for serialization (serde_json), Windows host.
- Spec deviation (approved in design): command routing lives in the **player worker** (`player.rs`), not `app.rs::send()` — the worker owns the only Tokio runtime and the UiCommand channel already ends there; UX is identical to the spec.

## Review Focus

Input classes the spec implies but individual task steps don't all exercise; each is pinned to an owning task:

1. **Soloist process dies mid-playback** — reconnect hint appears, routing falls back to librespot, no panic or UI hang. (Task 4 step + Task 6.)
2. **Soloist session logged out/replaced underneath the app** — `auth_state` flips `logged_in` false; routing and snapshots revert; blocked-banner may legitimately return. (Task 4.)
3. **Play commands with empty/short queue lists** (search rows, 1-track contexts) — single-track `play`, zero `add_to_queue` frames, no errors. (Task 4 probe of routing logic + Task 6.)
4. **Rapid volume drags while Soloist drives** — many `set_volume` commands, `VOLUME_OVERRIDE` echo-clear still works, no flicker fight. (Task 6 GUI check.)
5. **Soloist logged in but not the active Connect device** — `activate` precedes `play`; activation failure surfaces as a Soloist error event on the status line. (Task 4 translation, Task 6.)
6. **Soloist absent at app start, started later** — permanent-with-backoff connect picks it up without restart. (Task 4/6.)

---

### Task 1: Install and start Spotify Soloist (setup)

**Files:**
- Create: `%LOCALAPPDATA%\soloist\` directory holding `soloist.exe` (downloaded; outside the repo)

**Interfaces:**
- Produces: Soloist listening on `127.0.0.1:9090`, logged in with the user's account — the precondition for every later verification step.

- [ ] **Step 1: Fetch the latest Soloist Windows release asset**
  Fetch `https://github.com/spotify/soloist/releases/latest` (redirects to the newest tag) and identify the Windows x86-64 zip asset name from the release page.
- [ ] **Step 2: Download and unpack**
  `mkdir -p "$LOCALAPPDATA/soloist"`, download the asset with `curl -L`, unzip into that directory.
- [ ] **Step 2b: Personal API key (user action, needed by Step 3)**
  The key is generated per-user in the Spotify for Developers dashboard: log in → open "Spotify Soloist API Key" → accept terms → Generate API Key. The user supplies it to the controller; it is never printed to logs or committed.
- [ ] **Step 3: Start Soloist with the WebSocket API**
  Launch detached: `"$LOCALAPPDATA/soloist/soloist.exe" -n "spotify-lite" -k "<SOLOIST_API_KEY>" --ws 127.0.0.1:9090` (Soloist 1.3.x requires a device name `-n` and a personal developer API key `-k` — see Step 2b). Verify: `netstat -an | grep 9090` shows LISTENING, or bash `/dev/tcp/127.0.0.1/9090` connect succeeds.
- [ ] **Step 4: One-time Connect pairing (user, official app)**
  Soloist logs in via Spotify Connect, not a browser: with Soloist running, open the official Spotify app on the same network, pick the "spotify-lite" Soloist device from the device picker, and play a second of anything; Soloist stores the session in its data dir and restores it on later restarts. Wait for the user; do not proceed to Task 3 until playback state shows `logged_in: true`.

### Task 2: Soloist WebSocket client module (`src/soloist.rs`)

**Files:**
- Modify: `Cargo.toml` (add `tokio-tungstenite = "0.28"` under `[dependencies]`)
- Modify: `src/types.rs` (`PositionAnchor` struct + `PlayerSnapshot.position_anchor` + `PlayerSnapshot.upcoming`)
- Create: `src/soloist.rs`

**Interfaces:**
- Consumes: `crate::types::{Track, RepeatMode, PlayerSnapshot, BackendEvent, PLAYBACK_BLOCKED_MESSAGE}`.
- Produces (Task 4/5 rely on these exact names):
  - `pub struct PositionAnchor { pub position_ms: u32, pub timestamp_ms: i64, pub speed: f32 }` in `types.rs`, with `pub fn position_at(&self, now_ms: i64) -> u32` (saturating; advance only when `speed > 0.0`).
  - `PlayerSnapshot` gains `pub position_anchor: Option<PositionAnchor>` and `pub upcoming: Vec<Track>` (both fine with the existing `Default` derive).
  - `pub struct SoloistHandle { /* ... */ }` with `pub fn state(&self) -> SoloistState` and `pub fn command(&self, cmd: SoloistCommand)`.
  - `pub fn spawn(addr: String, events: std::sync::mpsc::Sender<BackendEvent>) -> SoloistHandle` — spawns the connect/backoff/read/write tasks on the current Tokio runtime.
  - `#[derive(Clone, Debug, Default)] pub struct SoloistState { pub connected: bool, pub logged_in: bool, pub is_active: bool, pub device_name: String, pub status: SoloistStatus, pub current: Option<Track>, pub context_name: Option<String>, pub position: Option<PositionAnchor>, pub volume_percent: u8, pub shuffle: bool, pub repeat: RepeatMode, pub upcoming: Vec<Track> }` (`SoloistStatus`: `Idle/Playing/Paused/Buffering`, default `Idle`).
  - `pub enum SoloistCommand { Play(Option<String>), Pause, Resume, SkipNext, SkipPrev, Seek(u32), SetVolume(u8), SetShuffle(bool), SetRepeat(RepeatMode), AddToQueue(String), Activate }`.
  - `pub fn snapshot_from_soloist(state: &SoloistState) -> PlayerSnapshot` — maps current→`current`, `Playing|Buffering`→`playing: true`, `volume_percent * 65535 / 100`→`volume`, `repeat` passthrough, `position`→`position_anchor` (and `position_ms` = anchor position when paused), `upcoming`→`upcoming`, `playback_blocked: false`.

- [ ] **Step 1: Add the dependency**
  `Cargo.toml`: `tokio-tungstenite = "0.28"`. Verify `cargo check`-equivalent via wrapper in Step 5.
- [ ] **Step 2: Write the wire types in `src/soloist.rs`**
  Private serde types matching the documented protocol: server events `auth_state`, `playback_state`, `track_changed`, `playback_changed`, `volume_changed`, `device_changed`, `context_changed`, `options_changed`, `position_sync`, `queue_changed`, `command_result`, `error`; the entity envelope (`uri`, `entity_type`, `decorations.identity.name`, `decorations.visual_identity.cover[].url`, `decorations.parent.entity…identity.name`, `decorations.creators[].entity…identity.name`, `decorations.playback.duration_ms`). Untagged/catch-all so unknown event types parse as `Unknown` instead of failing the frame.
- [ ] **Step 3: State mirror + entity→`Track` mapping**
  `SoloistState` per the Interfaces block; `fn entity_to_track(e: &Entity) -> Track` (uri→`id` by stripping `spotify:track:`, title, artists vec, album from parent, `album_art_url` from the first cover URL, `duration_ms`, zero/defaults elsewhere).
- [ ] **Step 4: Connection task with backoff + event pump**
  `spawn()` builds the handle, then `tokio::spawn`s: a writer task draining an internal `mpsc::SoloistCommand` queue onto the WS sink (each frame `serde_json::json!({"type":"command","command":…})`; repeat expands to the two documented boolean commands in the documented order); a connector loop `connect_with_retry`-style with capped exponential backoff 0.5 s→15 s updating `SoloistState.connected`; on each accepted connection send `get_auth_state`, `get_state`, `get_queue {limit: 0}`. A reader loop deserializes events, updates `SoloistState` under its mutex, emits `BackendEvent::PlayerSnapshot(snapshot_from_soloist(&state))` **only while `logged_in`** (on logout also re-emit the last librespot snapshot is NOT possible here — instead emit `PlayerSnapshot::default()`; Task 4 restores librespot snapshots on the next librespot event), forwards `error` events as `BackendEvent::Error(message)`, and emits `BackendEvent::Info` on transitions: connected, disconnected, logged in ("Soloist ready — playback goes through Spotify Soloist.").
- [ ] **Step 5: Compile check**
  Run: `cmd //c vcbuild.bat check`. Expected: 0 errors.

### Task 3: `soloist_probe` diagnostic example

**Files:**
- Create: `examples/soloist_probe.rs`

**Interfaces:**
- Consumes: `soloist::spawn` / `SoloistState` from Task 2.
- Produces: human-readable protocol dump used by all later verification.

- [ ] **Step 1: Write the probe (~120 lines, conventions of `examples/webapi_probe.rs`)**
  `#[tokio::main]`; read addr from `SOLOIST_ADDR` env or default `127.0.0.1:9090`; `spawn()`; loop printing each `BackendEvent` for ~8 s; then, if `state.logged_in`, send `SoloistCommand::Play(None)`… no — harmless round-trip only: send `get_state` equivalent is automatic; if `status == Playing`, send `Pause` then `Resume`, else just `Activate` is too invasive — only pause/resume when playing. Print final `SoloistState` debug.
- [ ] **Step 2: Run against live Soloist**
  Run: `cmd //c vcbuild.bat run --example soloist_probe`. Expected: `auth_state logged_in=true`, a `playback_state` dump, queue dump; no panics; exit 0. (Requires Task 1 complete.)

### Task 4: Worker routing + plumbing (`player.rs`, `types.rs` command fields)

**Files:**
- Modify: `src/types.rs` (`UiCommand::PlayTrack`/`PlayContext` gain `queue: Vec<String>`)
- Modify: `src/player.rs` (worker owns `SoloistHandle`, routes playback commands, takeover pause)
- Modify: `src/main.rs` (register `mod soloist;`)

**Interfaces:**
- Consumes: Task 2's `SoloistHandle`/`SoloistCommand`/`snapshot_from_soloist`; Task 3's probe output as behavioral reference.
- Produces: behavior — while `soloist.state().logged_in`, these `UiCommand`s never reach the librespot code paths: `PlayTrack`, `PlayContext`, `TogglePlay`, `Next`, `Previous`, `SeekMs`, `SetVolume`, `ToggleShuffle`, `CycleRepeat`. All other commands unchanged.

- [ ] **Step 1: Extend the play commands in `types.rs`**
  `PlayTrack { track_uri: String, context_uri: Option<String>, queue: Vec<String> }`; `PlayContext { context_uri: String, track_uri: Option<String>, queue: Vec<String> }`. Update the two doc comments. Fix every construction site (grep `PlayTrack`/`PlayContext`; UI sites get the real queue in Task 5 — temporarily pass `Vec::new()`).
- [ ] **Step 2: Worker spawns the Soloist client**
  In `PlayerHandle::spawn`'s thread, after building the runtime: read the addr via `crate::config::Config::load().soloist_ws_addr` if the field exists yet, else the literal default `"127.0.0.1:9090".to_string()` (Task 5 adds the config field and switches this read to it), then `let soloist = soloist::spawn(addr, events.clone());` and pass it into `PlayerWorker::new`.
- [ ] **Step 3: Route playback commands in `handle_command`**
  At the top of `handle_command`: `let soloist_live = self.soloist.state().logged_in;` — for the eight playback commands, when `soloist_live`, translate and return instead of running the librespot path: `PlayTrack{track_uri, queue,..}` → `Activate` (only when `!state.is_active`), `Play(Some(track_uri))`, then `AddToQueue(uri)` for each of `queue`; `PlayContext{context_uri, queue, track_uri}` → same but `Play(Some(track_uri.unwrap_or(context_uri)))`; `TogglePlay` → `Pause`/`Resume` by `state.status`; `Next`/`Previous` → `SkipNext`/`SkipPrev`; `SeekMs(p)` → `Seek(p)`; `SetVolume(v)` → `SetVolume((v as u32 * 100 / 65535) as u8)`; `ToggleShuffle` → `SetShuffle(!state.shuffle)`; `CycleRepeat` → `SetRepeat(next)`. Soloist `command()` is fire-and-forget.
- [ ] **Step 4: Takeover pause (no double audio)**
  In the worker's command loop (the `recv_timeout` match arm on `Timeout`), check a stored `last_soloist_logged_in: bool`: on transition false→true while the librespot side reports `playing` (from `WorkerState`), call `player.pause()` and update status via the normal snapshot path.
- [ ] **Step 5: Snapshot ownership while Soloist drives**
  `spawn_block_probe` stays as-is. When Soloist is live, librespot-side snapshot emissions (`emit_snapshot`, event pump) would fight Soloist-derived snapshots — gate them: in `emit_snapshot` and the pump's snapshot sends, skip when `soloist.state().logged_in`. When Soloist logs off, the next librespot event re-emits normally (acceptable one-frame stale).
- [ ] **Step 6: Compile + degraded-mode smoke**
  Run: `cmd //c vcbuild.bat check` (0 errors). Launch the release exe WITHOUT Soloist running (if running, stop it): verify status line shows the "Soloist not detected — start it with: soloist --ws 127.0.0.1:9090 …" Info and the app otherwise behaves exactly as today. Kill the app.

### Task 5: UI — settings field, queue passing, Queue screen, live position

**Files:**
- Modify: `src/config.rs` (`pub soloist_ws_addr: Option<String>` on `Config`)
- Modify: `src/ui/settings.rs` (Soloist section)
- Modify: `src/ui/browser.rs` (pass `queue` at both play emission sites; Queue screen renders `app.player.upcoming`)
- Modify: `src/ui/now_playing.rs` (position from `position_anchor` when present)

**Interfaces:**
- Consumes: Task 1–4 behavior.
- Produces: complete user-facing feature.

- [ ] **Step 1: Config field**
  Add `pub soloist_ws_addr: Option<String>` to `Config` (serde default keeps old config.toml files parsing).
- [ ] **Step 2: Settings section**
  In `settings.rs::draw`, new "SOLOIST" section between DEVELOPER CREDENTIALS and PLAYBACK: single-line text field bound to `app.config.soloist_ws_addr` (same pattern as client_id), hint text "WebSocket address of Spotify Soloist (takes effect after restart)". Save via existing Save button.
- [ ] **Step 3: Pass the visible queue at play sites in `browser.rs`**
  In `track_table`'s emission block (currently ~line 642): `queue = tracks[index + 1..].iter().map(|t| t.uri.clone()).collect()` for `PlayTrack`. For `PlayContext` (playlist header button — find via grep `PlayContext`): `queue` = the full visible track list of that screen (playlist: `app.playlist_tracks` order; album: `details.tracks`); header plays `context_uri` with `track_uri: None`.
- [ ] **Step 4: Queue screen**
  Replace the placeholder at `browser.rs` `Screen::Queue` arm: heading "Queue"; if `app.player.upcoming` is empty → existing empty-state text; else `track_table`-style simple rows (`#`, title, artists) of `app.player.upcoming` (read-only; reuse the table's column layout without the heart/duration columns — simplest: a `TableBuilder` with 3 columns, no interactions).
- [ ] **Step 5: Live position in the now-playing bar**
  In `now_playing.rs::seek_bar` and `position_display`: effective position = `app.player.position_anchor` when `Some` and `speed > 0.0` → `anchor.position_at(current_unix_ms())`, else `app.player.position_ms`. Add tiny helper `fn now_ms() -> i64` (SystemTime). Keep `SEEK_OVERRIDE` behavior unchanged.
- [ ] **Step 6: Compile**
  `cmd //c vcbuild.bat check` → 0 errors.

### Task 6: End-to-end verification

**Files:** none (verification only; Soloist running per Task 1)

- [ ] **Step 1: Release build** — `cmd //c vcbuild.bat build --release` → 0 errors.
- [ ] **Step 2: Soloist-live smoke (with the user)** — Launch `target/release/spotify-lite.exe`; user logs in to the app (Connect + browser). Click a mid-playlist track: audio from PC speakers within ~2 s; now-playing shows title/artists/cover; progress bar advances smoothly; play/pause/next/prev/seek/volume/shuffle/repeat all take effect; Queue screen lists upcoming tracks.
- [ ] **Step 3: Review Focus sweep** — (1) kill Soloist mid-playback → hint appears, no hang, app usable; restart Soloist → reconnects without app restart. (2) Soloist logged out underneath → graceful fallback. (3) search-row click → single track plays. (4) volume drag spam → smooth. (5) phone was last active device → play still lands on Soloist.
- [ ] **Step 4: Regression** — librespot-only mode (Soloist stopped): yesterday's blocked-banner flow intact; browsing unchanged.
- [ ] **Step 5: Handoff update** — Append a "Soloist integration shipped" section to `SESSION-2026-11-11.md` (what changed, how to run Soloist, the 90-day expiry caveat, new debt: position clock skew ignored, queue snapshot ownership simplification).
