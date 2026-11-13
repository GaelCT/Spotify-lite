//! Application configuration: client credentials and UI preferences,
//! persisted as TOML via the `directories` crate.
//!
//! Path layout (via `directories::ProjectDirs::from("", "", "spotify-lite")`):
//! - Windows: config under `%APPDATA%\spotify-lite`, cache under
//!   `%LOCALAPPDATA%\spotify-lite`.
//! - Linux: config under `$XDG_CONFIG_HOME/spotify-lite`, cache under
//!   `$XDG_CACHE_HOME/spotify-lite`.
//! - macOS: config under `~/Library/Application Support/spotify-lite`, cache
//!   under `~/Library/Caches/spotify-lite`.

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// On-disk application configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Spotify Developer Dashboard client ID, remembered between sessions.
    pub client_id: Option<String>,
    /// Optional client secret. Unused by the PKCE login flow; kept for future
    /// Authorization-Code-flow support.
    pub client_secret: Option<String>,
    /// UI volume preference in percent (`0..=100`), applied at login.
    /// Note: this is a UI-level percent; librespot mixer units are
    /// `0..=65535` — the conversion happens in the player worker.
    pub volume: Option<u16>,
    /// WebSocket address of Spotify Soloist (e.g. `127.0.0.1:9090`), used by
    /// the player worker as its playback engine. Loopback hosts only; a
    /// missing port defaults to 9090. `None` = the built-in default.
    /// Takes effect after restart.
    pub soloist_ws_addr: Option<String>,
}

impl Config {
    /// Loads the config from `config_dir()/config.toml`.
    ///
    /// Returns `Config::default()` (all `None`) when the file is missing or
    /// cannot be parsed; the failure is logged, never fatal.
    pub fn load() -> Self {
        let Some(path) = config_file_path() else {
            tracing::warn!("no project directories available; using default config");
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!("failed to parse config at {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("failed to read config at {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Saves the config to `config_dir()/config.toml`, creating the directory
    /// if needed. Failures are logged, never fatal.
    pub fn save(&self) {
        let Some(path) = config_file_path() else {
            tracing::warn!("no project directories available; config not saved");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("failed to create config dir {}: {e}", parent.display());
                return;
            }
        }
        match toml::to_string_pretty(self) {
            Ok(contents) => {
                if let Err(e) = std::fs::write(&path, contents) {
                    tracing::warn!("failed to write config to {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("failed to serialize config: {e}"),
        }
    }
}

/// Absolute path of the config file (`config_dir()/config.toml`), or `None`
/// when no project directory could be determined.
pub fn config_file_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

/// The platform config directory, e.g. `%APPDATA%\spotify-lite` on Windows.
pub fn config_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "spotify-lite")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// The platform cache directory, e.g. `%LOCALAPPDATA%\spotify-lite` on
/// Windows. Used for the auth token cache and (later) audio/image caches.
pub fn cache_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "spotify-lite")
        .map(|dirs| dirs.cache_dir().to_path_buf())
}

/// Path of the cached OAuth token (`cache_dir()/credentials.json`), or `None`
/// when no cache directory could be determined.
///
/// IMPLEMENTER NOTE: `serde_json` is NOT a project dependency. Either cache
/// the token in TOML format (via the `toml` crate) in this same file, or
/// serialize the token fields manually with `format!`/`serde`.
pub fn auth_cache_path() -> Option<PathBuf> {
    cache_dir().map(|dir| dir.join("credentials.json"))
}

/// Path of the cached playback token (`cache_dir()/playback-credentials.json`),
/// minted with Spotify's desktop client identity; separate from the Web API
/// token cache. See `auth` module docs.
pub fn playback_cache_path() -> Option<PathBuf> {
    cache_dir().map(|dir| dir.join("playback-credentials.json"))
}

/// Path of the log file (`config_dir()/spotify-lite.log`), or `None` when no
/// config directory could be determined. Falls back to stderr in `main`.
pub fn log_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("spotify-lite.log"))
}
