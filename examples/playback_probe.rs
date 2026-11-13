//! Diagnostic probe (not part of the app): obtains a fresh playback token
//! (cached one first, otherwise the same browser OAuth flow as
//! `src/auth.rs::authenticate_playback`) and then answers: does Spotify grant
//! audio keys for ANY file format listed in track metadata, or is every
//! format refused with "audio key error"? Requests a key for EVERY entry in
//! `track.files` on each test track.
//!
//! Run: `cmd //c vcbuild.bat run --example playback_probe`

use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use librespot::core::authentication::Credentials;
use librespot::core::{Session, SessionConfig, SpotifyUri};
use librespot::metadata::{Metadata, Track};
use librespot::oauth::OAuthClientBuilder;

// Same constants as `src/auth.rs`.
const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const PLAYBACK_SCOPES: &[&str] = &["streaming"];
const OAUTH_MESSAGE: &str = "Playback authorized — you can return to spotify-lite.";

// (primary uri, fallback uri if the primary does not resolve)
const TRACKS: [(&str, Option<&str>); 3] = [
    ("spotify:track:1udwFobQ1JoOdWPQrp2b6u", None),
    ("spotify:track:3LaHsdzzmvhx6TJedpomVy", None),
    (
        "spotify:track:7ouMYWpwJ422jRcDASb7zI",
        Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC"),
    ),
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const KEY_TIMEOUT: Duration = Duration::from_secs(15);
const OAUTH_TIMEOUT: Duration = Duration::from_secs(180);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cache_path = playback_cache_path()?;

    // 1. Try the cached token first.
    let mut session: Option<Session> = None;
    match load_cached_token(&cache_path) {
        Ok(token) => {
            println!("cached token loaded ({} chars), trying it", token.len());
            session = connect_session(Credentials::with_access_token(token), "cached playback token").await;
        }
        Err(e) => println!("cached token unusable: {:#}", e),
    }

    // 2. Cached token rejected/missing: same OAuth flow as the app.
    if session.is_none() {
        let client = OAuthClientBuilder::new(
            KEYMASTER_CLIENT_ID,
            PLAYBACK_REDIRECT_URI,
            PLAYBACK_SCOPES.to_vec(),
        )
        .open_in_browser()
        .with_custom_message(OAUTH_MESSAGE)
        .build()
        .context("failed to set up playback OAuth client")?;
        println!(
            "waiting for browser approval… ({}s; approve in the opened browser tab)",
            OAUTH_TIMEOUT.as_secs()
        );
        let token = match tokio::time::timeout(
            OAUTH_TIMEOUT,
            tokio::task::spawn_blocking(move || client.get_access_token()),
        )
        .await
        {
            Err(_) => {
                println!(
                    "OAUTH_NOT_COMPLETED no redirect within {}s",
                    OAUTH_TIMEOUT.as_secs()
                );
                return Ok(());
            }
            Ok(join) => join
                .context("OAuth task failed to complete")?
                .context("playback authorization failed")?,
        };
        let expires_in = token
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs();
        persist_playback_token(&cache_path, &token.access_token, expires_in)?;
        println!(
            "fresh token received ({} chars, ~{}s lifetime); playback-credentials.json updated for the app too",
            token.access_token.len(),
            expires_in
        );
        session = connect_session(Credentials::with_access_token(token.access_token), "fresh OAuth token").await;
    }
    let Some(session) = session else {
        println!("no token-authenticated session; stopping");
        return Ok(());
    };
    println!("session authenticated as {}", session.username());

    // 3. Per track: list every offered format, request a key for each.
    let mut granted = 0usize;
    let mut attempted = 0usize;
    let mut keyed_formats: Vec<String> = Vec::new();
    for (primary, fallback) in TRACKS {
        let outcome = tokio::time::timeout(
            Duration::from_secs(60),
            sweep_track(&session, primary, fallback),
        )
        .await;
        match outcome {
            Err(_) => println!("{} | track sweep: TIMEOUT after 60s", primary),
            Ok((g, a, mut formats)) => {
                granted += g;
                attempted += a;
                keyed_formats.append(&mut formats);
            }
        }
    }

    println!("KEYS_GRANTED: {}/{}", granted, attempted);
    if keyed_formats.is_empty() {
        println!("KEYED_FORMATS: none");
    } else {
        println!("KEYED_FORMATS: {}", keyed_formats.join(", "));
    }
    Ok(())
}

/// Fetch metadata, print every offered format, request a key for each.
/// Returns (keys granted, formats attempted, formats that got a key).
async fn sweep_track(
    session: &Session,
    uri_str: &str,
    fallback: Option<&str>,
) -> (usize, usize, Vec<String>) {
    let uri = match SpotifyUri::from_uri(uri_str) {
        Ok(uri) => uri,
        Err(e) => {
            println!("{} | metadata: FAIL {:#}", uri_str, e);
            return (0, 0, Vec::new());
        }
    };
    let track = match Track::get(session, &uri).await {
        Ok(track) => track,
        Err(e) => {
            println!("{} | metadata: FAIL {:#}", uri_str, e);
            if let Some(fb) = fallback {
                println!("{} does not resolve; swapping in fallback {}", uri_str, fb);
                return Box::pin(sweep_track(session, fb, None)).await;
            }
            return (0, 0, Vec::new());
        }
    };
    println!("{} | metadata: OK \"{}\"", uri_str, track.name);

    let SpotifyUri::Track { id } = &uri else {
        println!("{} | not a track URI, skipping", uri_str);
        return (0, 0, Vec::new());
    };

    // Every offered format, sorted for deterministic output.
    let mut files: Vec<_> = track.files.iter().map(|(f, id)| (*f, *id)).collect();
    files.sort_by_key(|(f, _)| format!("{:?}", f));
    if files.is_empty() {
        println!("  no audio files in metadata");
        return (0, 0, Vec::new());
    }
    let offered: Vec<String> = files
        .iter()
        .map(|(f, id)| format!("{:?} ({}B file)", f, id.0.len()))
        .collect();
    println!("  formats offered ({}): {}", files.len(), offered.join(", "));

    let mut granted = 0usize;
    let attempted = files.len();
    let mut keyed_formats = Vec::new();
    for (format, file_id) in files {
        let label = format!("{:?}", format);
        match tokio::time::timeout(
            KEY_TIMEOUT,
            session.audio_key().request(*id, file_id),
        )
        .await
        {
            Err(_) => println!("  {}: key TIMEOUT after {}s", label, KEY_TIMEOUT.as_secs()),
            Ok(Ok(_)) => {
                println!("  {}: key OK", label);
                granted += 1;
                keyed_formats.push(label);
            }
            Ok(Err(e)) => println!("  {}: key FAIL {:#}", label, e),
        }
    }
    (granted, attempted, keyed_formats)
}

async fn connect_session(creds: Credentials, label: &str) -> Option<Session> {
    let session = Session::new(SessionConfig::default(), None);
    match tokio::time::timeout(CONNECT_TIMEOUT, session.connect(creds, false)).await {
        Err(_) => {
            println!(
                "session ({}): CONNECT FAILED timed out after {}s",
                label,
                CONNECT_TIMEOUT.as_secs()
            );
            None
        }
        Ok(Err(e)) => {
            println!("session ({}): CONNECT FAILED {:#}", label, e);
            None
        }
        Ok(Ok(())) => {
            println!("session ({}): AUTHENTICATED {}", label, session.username());
            Some(session)
        }
    }
}

fn playback_cache_path() -> Result<PathBuf> {
    let base = PathBuf::from(std::env::var("LOCALAPPDATA").context("LOCALAPPDATA not set")?);
    Ok(base.join("spotify-lite").join("cache").join("playback-credentials.json"))
}

fn load_cached_token(cache_path: &std::path::Path) -> Result<String> {
    let raw = std::fs::read_to_string(cache_path)
        .with_context(|| format!("cannot read {}", cache_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    parsed["access_token"]
        .as_str()
        .context("cached playback credentials have no access_token")
        .map(str::to_string)
}

/// Same on-disk format as `src/auth.rs` (`refresh_token` omitted when absent).
fn persist_playback_token(cache_path: &std::path::Path, token: &str, expires_in: u64) -> Result<()> {
    let value = serde_json::json!({
        "access_token": token,
        "expires_at": unix_now_secs() + expires_in,
    });
    std::fs::write(cache_path, serde_json::to_string(&value)?)?;
    Ok(())
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
