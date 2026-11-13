//! Spotify authentication: OAuth authorization-code flow with PKCE, via
//! `librespot::oauth` (librespot-oauth 0.8).
//!
//! # Flow
//!
//! 1. [`AuthManager::authenticate`] first checks the on-disk cache
//!    ([`crate::config::auth_cache_path`]); a cached token is returned as-is
//!    while it is still fresh (see [`FRESHNESS_MARGIN_SECS`]).
//! 2. Otherwise the librespot-oauth client is built with
//!    [`OAuthClientBuilder::new`], our [`REDIRECT_URI`] and [`SCOPES`]. The
//!    builder validates the redirect URI; the actual callback listener is
//!    spawned by `get_access_token` itself (a std `TcpListener` on
//!    `127.0.0.1:8888`), so no listener code lives here.
//! 3. The token getter is blocking, so it runs on `tokio::task::spawn_blocking`.
//!    With `.open_in_browser()` the auth URL opens in the default browser via
//!    librespot-oauth's own `open` dependency (no extra crate needed); the URL
//!    is additionally printed to stdout.
//! 4. The returned [`librespot::oauth::OAuthToken`] carries
//!    `expires_at: std::time::Instant` (monotonic, per-process), so it is
//!    converted to seconds-from-now for [`AuthOutcome::expires_in`] and to an
//!    absolute Unix timestamp for the cache file.
//! 5. The token is persisted as JSON (`credentials.json`): access token,
//!    absolute expiry, and the refresh token (Spotify always issues one; the
//!    field is stored as `None` if empty).
//! 6. The user's display name is fetched from `GET /v1/me` with `reqwest`;
//!    failure leaves `AuthOutcome::user_name` as `None` and is not an error.
//!
//! # Dual identity: Web API vs playback
//!
//! Since 2026-08-10 Spotify's `login5` endpoint rejects playback credentials
//! minted from any third-party client ID (every track load fails with
//! `INVALID_CREDENTIALS`); only tokens issued to Spotify's own desktop
//! ("keymaster") client ID are accepted there. So the app uses TWO tokens:
//!
//! - [`AuthManager::authenticate`] — the user's own developer-app client ID,
//!   full [`SCOPES`], drives the Web API (rspotify) and user identity.
//! - [`AuthManager::authenticate_playback`] — [`KEYMASTER_CLIENT_ID`] with
//!   the `streaming` scope only, drives the librespot session (audio).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use librespot::oauth::{OAuthClient, OAuthClientBuilder};
use serde::{Deserialize, Serialize};

/// Redirect URI registered for the OAuth flow. Must match an entry in the
/// Spotify Developer Dashboard for the client ID.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/login";

/// OAuth scopes requested during login.
pub const SCOPES: &[&str] = &[
    "streaming",
    "user-read-email",
    "user-read-private",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-library-read",
    "user-library-modify",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-follow-read",
    "user-follow-modify",
];

/// A cached token is only reused while it has at least this much lifetime
/// left, so an in-flight session never starts with a nearly-dead token.
const FRESHNESS_MARGIN_SECS: u64 = 5 * 60;

/// Spotify's own desktop ("keymaster") client ID. Since 2026-08-10 the
/// login5 endpoint only accepts playback credentials minted with this
/// identity; a token from any third-party/developer client ID is rejected
/// with `INVALID_CREDENTIALS` on every track load. Used ONLY for the
/// playback token — the Web API keeps using the user's own developer app.
pub const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

/// Redirect URI for the playback OAuth flow. Separate port from the Web API
/// flow ([`REDIRECT_URI`]) so the two callback listeners never collide.
pub const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

/// Scopes for the playback token — streaming/login5 only.
pub const PLAYBACK_SCOPES: &[&str] = &["streaming"];

/// Successful authentication result handed back to the player worker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthOutcome {
    /// Bearer access token usable with the Spotify Web API and
    /// `librespot::core::authentication::Credentials::with_access_token`.
    pub access_token: String,
    /// Seconds until the access token expires, measured from when it was
    /// obtained. When caching, convert this to an absolute expiry instant.
    pub expires_in: u64,
    /// Display name of the logged-in user, if already fetched.
    pub user_name: Option<String>,
}

/// On-disk representation of the cached credentials (`credentials.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCredentials {
    /// Bearer access token.
    access_token: String,
    /// Absolute expiry as a Unix timestamp in seconds.
    expires_at: u64,
    /// Long-lived refresh token; present unless Spotify omitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

/// Owns the interactive OAuth login flow and the on-disk token cache.
pub struct AuthManager;

impl AuthManager {
    /// Creates a new manager (currently stateless; per-call state lives in
    /// the methods).
    pub fn new() -> Self {
        Self
    }

    /// Returns a valid access token, reusing the on-disk cache when possible
    /// and otherwise running the interactive browser-based OAuth flow.
    ///
    /// Blocks (via `spawn_blocking`) until the user completes login in the
    /// browser, or returns an error on failure.
    pub async fn authenticate(
        &self,
        client_id: &str,
        _client_secret: Option<&str>,
    ) -> anyhow::Result<AuthOutcome> {
        // 1. Fresh cache hit: skip the browser entirely.
        if let Some(cached) = self.cached_token() {
            tracing::debug!("reusing cached Spotify token");
            return Ok(cached);
        }

        // 2. Build the OAuth client (validates the redirect URI).
        let client: OAuthClient = OAuthClientBuilder::new(client_id, REDIRECT_URI, SCOPES.to_vec())
            .open_in_browser()
            .with_custom_message("Login successful — you can return to spotify-lite.")
            .build()
            .map_err(|e| anyhow!("failed to set up OAuth client: {e}"))?;

        // 3. The getter blocks on a std TcpListener accepting the OAuth
        //    redirect; keep it off the async executor.
        let token = tokio::task::spawn_blocking(move || client.get_access_token())
            .await
            .map_err(|e| anyhow!("OAuth login task failed to complete: {e}"))?
            .map_err(|e| anyhow!("OAuth login failed: {e}"))?;

        // 4. OAuthToken.expires_at is a monotonic Instant; convert it to
        //    seconds-from-now for AuthOutcome.
        let expires_in = token
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs();

        // 5. Persist for later launches. Failure is not fatal: login already
        //    succeeded, the next launch just repeats the browser step.
        self.persist_cache(&token.access_token, expires_in, &token.refresh_token);

        // 6. Best-effort display-name lookup.
        let user_name = fetch_display_name(&token.access_token).await;

        Ok(AuthOutcome {
            access_token: token.access_token,
            expires_in,
            user_name,
        })
    }

    /// Returns a previously cached, still-fresh token, or `None` when there is
    /// no cache or it has expired (or is within [`FRESHNESS_MARGIN_SECS`] of
    /// expiring).
    pub fn cached_token(&self) -> Option<AuthOutcome> {
        let path = crate::config::auth_cache_path()?;
        let contents = std::fs::read_to_string(&path).ok()?;
        let cached: CachedCredentials = match serde_json::from_str(&contents) {
            Ok(cached) => cached,
            Err(e) => {
                tracing::warn!(
                    "ignoring unparsable auth cache at {}: {e}",
                    path.display()
                );
                return None;
            }
        };

        let now = unix_now_secs();
        if cached.expires_at <= now.saturating_add(FRESHNESS_MARGIN_SECS) {
            tracing::debug!("cached Spotify token is expired or nearly expired");
            return None;
        }

        Some(AuthOutcome {
            access_token: cached.access_token,
            expires_in: cached.expires_at - now,
            user_name: None,
        })
    }

    /// Returns an access token usable for librespot playback, reusing the
    /// on-disk playback cache when fresh and otherwise running a browser
    /// OAuth flow against Spotify's desktop client ID (see module docs).
    pub async fn authenticate_playback(&self) -> anyhow::Result<String> {
        if let Some(cached) = self.cached_playback_token() {
            tracing::debug!("reusing cached playback token");
            return Ok(cached);
        }

        let client: OAuthClient = OAuthClientBuilder::new(
            KEYMASTER_CLIENT_ID,
            PLAYBACK_REDIRECT_URI,
            PLAYBACK_SCOPES.to_vec(),
        )
        .open_in_browser()
        .with_custom_message("Playback authorized — you can return to spotify-lite.")
        .build()
        .map_err(|e| anyhow!("failed to set up playback OAuth client: {e}"))?;

        let token = tokio::task::spawn_blocking(move || client.get_access_token())
            .await
            .map_err(|e| anyhow!("playback OAuth task failed to complete: {e}"))?
            .map_err(|e| anyhow!("playback authorization failed: {e}"))?;

        let expires_in = token
            .expires_at
            .saturating_duration_since(Instant::now())
            .as_secs();

        self.persist_playback_cache(&token.access_token, expires_in);
        Ok(token.access_token)
    }

    /// Returns a fresh cached playback token, or `None` when missing/expired.
    fn cached_playback_token(&self) -> Option<String> {
        let path = crate::config::playback_cache_path()?;
        let contents = std::fs::read_to_string(&path).ok()?;
        let cached: CachedCredentials = serde_json::from_str(&contents).ok()?;
        let now = unix_now_secs();
        if cached.expires_at <= now.saturating_add(FRESHNESS_MARGIN_SECS) {
            return None;
        }
        Some(cached.access_token)
    }

    /// Writes the playback token to its cache file; failures are logged only.
    fn persist_playback_cache(&self, access_token: &str, expires_in: u64) {
        let Some(path) = crate::config::playback_cache_path() else {
            tracing::warn!("no cache directory available; playback token not cached");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("failed to create cache dir {}: {e}", parent.display());
                return;
            }
        }
        let cached = CachedCredentials {
            access_token: access_token.to_string(),
            expires_at: unix_now_secs().saturating_add(expires_in),
            refresh_token: None,
        };
        if let Ok(json) = serde_json::to_string_pretty(&cached) {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("failed to write playback cache to {}: {e}", path.display());
            }
        }
    }

    /// Deletes the cached tokens (Web API + playback), if any. Errors ignored.
    pub fn clear_cache(&self) {
        if let Some(path) = crate::config::auth_cache_path() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(path) = crate::config::playback_cache_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Writes the token to the cache file as JSON, creating the cache
    /// directory if needed. All failures are logged, never propagated.
    fn persist_cache(&self, access_token: &str, expires_in: u64, refresh_token: &str) {
        let Some(path) = crate::config::auth_cache_path() else {
            tracing::warn!("no cache directory available; token not cached");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("failed to create cache dir {}: {e}", parent.display());
                return;
            }
        }

        // Spotify always issues a refresh token; librespot substitutes an
        // empty string if the response omits it. Don't cache that placeholder.
        let refresh_token = if refresh_token.is_empty() {
            None
        } else {
            Some(refresh_token.to_string())
        };

        let cached = CachedCredentials {
            access_token: access_token.to_string(),
            expires_at: unix_now_secs().saturating_add(expires_in),
            refresh_token,
        };

        match serde_json::to_string_pretty(&cached) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("failed to write auth cache to {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("failed to serialize auth cache: {e}"),
        }
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Current time as a Unix timestamp in seconds (`0` only if the system clock
/// predates the Unix epoch).
fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fetches the user's `display_name` from `GET /v1/me`. Any failure —
/// network, HTTP status, missing field — yields `None`.
async fn fetch_display_name(access_token: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let response = client
        .get("https://api.spotify.com/v1/me")
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::warn!(
            "failed to fetch user profile: HTTP {}",
            response.status()
        );
        return None;
    }
    let profile: serde_json::Value = response.json().await.ok()?;
    profile
        .get("display_name")
        .and_then(|name| name.as_str())
        .map(str::to_string)
}
