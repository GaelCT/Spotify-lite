//! Thin async wrapper over the Spotify Web API (`rspotify` 0.16.1).
//!
//! `WebApi` binds an rspotify `AuthCodeSpotify` client to one access token
//! (obtained upstream via librespot OAuth) and maps `rspotify::model` types
//! into the plain structs in `crate::types`. The UI never sees rspotify types.
//!
//! Client construction: the token is injected manually with
//! [`AuthCodeSpotify::from_token_with_config`]. Two rspotify 0.16.1 details
//! matter here:
//! - A manually built `Token` always reports `is_expired() == true`
//!   (`expires_at: None`, and even `Token::default()` uses `Some(now)`). With
//!   the default `Config::token_refreshing` every request would then attempt
//!   a refresh and fail with `ClientError::InvalidToken` (no refresh token or
//!   client credentials are known), so refreshing is disabled in the `Config`
//!   built in [`WebApi::new`].
//! - The client's pagination helpers return a `futures` `Stream`, which this
//!   crate cannot consume (no direct `futures` dependency). Every list
//!   endpoint is instead paged manually through its `*_manual` variant with
//!   `limit = 50`, which is exactly what the helpers wrap; followed artists
//!   uses its cursor-based `after` parameter.
//!
//! Mapping conventions (see `crate::types` for the contract):
//! - `id` is the bare Spotify ID (`Id::id()`), never the URI; `Track.uri` is
//!   the full `spotify:track:...` URI (`Id::uri()`).
//! - Cover art picks the *smallest* image: rspotify lists images
//!   largest-first, so `images.last()`.
//! - Playlist items that are local files, episodes, or have no track/id are
//!   skipped (they carry no playable Spotify URI).
//! - `is_saved` is only resolved for [`WebApi::playlist_tracks`]; every other
//!   `Track` leaves it `false`.
//!
//! NOTE: `#![allow(dead_code)]` stays until the player worker wires all
//! methods up; remove it then.

#![allow(dead_code)]

use std::collections::HashSet;

use anyhow::Context;
use rspotify::{
    model::{
        AlbumId, ArtistId, FullAlbum, FullArtist, FullTrack, PlayableItem, PlaylistId,
        SearchResult, SearchType, SimplifiedAlbum, SimplifiedPlaylist, SimplifiedTrack, TrackId,
    },
    prelude::{BaseClient, Id, OAuthClient},
    AuthCodeSpotify, Config, Credentials, OAuth, Token,
};

use crate::types::{
    AlbumDetails, AlbumInfo, ArtistDetails, ArtistInfo, PlaylistInfo, SearchResults, Track,
};

/// Spotify returns at most 50 items per page for the endpoints used here.
const PAGE_LIMIT: u32 = 50;
/// The save/unsave/check-saved endpoints accept at most 50 ids per call.
const SAVED_CHUNK: usize = 50;

/// Message shown when Spotify answers HTTP 429 for an endpoint (endpoint
/// quota exceeded, e.g. every `/v1/me/playlists*` call while the
/// endpoint-family quota lasts): the limit resets with time, so the user
/// only needs to retry later.
pub const RATE_LIMITED_MESSAGE: &str =
    "Spotify is rate-limiting this endpoint (quota exceeded) — it resets with time; try again later.";

/// Web API client bound to one access token.
pub struct WebApi {
    /// Access token this client was built with (kept for diagnostics).
    access_token: String,
    /// rspotify client authenticated with the token above.
    client: AuthCodeSpotify,
}

impl WebApi {
    /// Creates a client that authenticates Web API calls with `access_token`.
    pub fn new(access_token: String) -> Self {
        let token = Token {
            access_token: access_token.clone(),
            // `Token::default()` gives `expires_in = 0` / `expires_at = now`;
            // the token cannot be refreshed, so expiry is tracked upstream
            // (the player re-creates `WebApi` with a fresh token).
            ..Default::default()
        };
        let config = Config {
            // The injected token has no refresh token and no client
            // credentials, so the automatic re-auth in `auto_reauth` would
            // fail every request with `ClientError::InvalidToken`.
            token_refreshing: false,
            ..Default::default()
        };
        let client = AuthCodeSpotify::from_token_with_config(
            token,
            Credentials::default(),
            OAuth::default(),
            config,
        );
        Self {
            access_token,
            client,
        }
    }

    /// Fetches the current user's playlists. `PlaylistInfo::description` is
    /// always `None` because `current_user_playlists` returns
    /// `SimplifiedPlaylist`, which carries no description (fetching each
    /// playlist's `FullPlaylist` would cost one request per playlist).
    pub async fn current_user_playlists(&self) -> anyhow::Result<Vec<PlaylistInfo>> {
        let mut playlists = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .current_user_playlists_manual(Some(PAGE_LIMIT), Some(offset))
                .await
                .context("failed to fetch the current user's playlists")?;
            let total = page.total;
            let fetched = page.items.len() as u32;
            playlists.extend(page.items.iter().map(playlist_info));
            offset += fetched;
            if fetched == 0 || offset >= total {
                break;
            }
        }
        Ok(playlists)
    }

    /// Fetches the current user's saved albums.
    pub async fn saved_albums(&self) -> anyhow::Result<Vec<AlbumInfo>> {
        let mut albums = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .current_user_saved_albums_manual(None, Some(PAGE_LIMIT), Some(offset))
                .await
                .context("failed to fetch the current user's saved albums")?;
            let total = page.total;
            let fetched = page.items.len() as u32;
            albums.extend(page.items.iter().map(|saved| album_info_full(&saved.album)));
            offset += fetched;
            if fetched == 0 || offset >= total {
                break;
            }
        }
        Ok(albums)
    }

    /// Fetches the artists the current user follows (cursor-paginated).
    pub async fn followed_artists(&self) -> anyhow::Result<Vec<ArtistInfo>> {
        let mut artists = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .client
                .current_user_followed_artists(after.as_deref(), Some(PAGE_LIMIT))
                .await
                .context("failed to fetch the current user's followed artists")?;
            let has_next = page.next.is_some();
            let next_after = page.cursors.and_then(|cursor| cursor.after);
            let fetched = page.items.len();
            artists.extend(page.items.iter().map(artist_info));
            // Continue only while Spotify reports a next page and hands us
            // the `after` cursor for it; also bail if a page is empty so a
            // malformed response can't loop forever.
            if fetched == 0 {
                break;
            }
            match (has_next, next_after) {
                (true, Some(cursor)) => after = Some(cursor),
                _ => break,
            }
        }
        Ok(artists)
    }

    /// Fetches all tracks of the playlist with the given bare ID.
    ///
    /// Local files, podcast episodes, and items without a track id are
    /// skipped. `is_saved` is resolved in chunks of 50 via the check-saved
    /// endpoint; if that check fails for any chunk, the playlist is still
    /// returned with every `is_saved` left as `false` rather than failing
    /// the whole load.
    pub async fn playlist_tracks(&self, id: &str) -> anyhow::Result<Vec<Track>> {
        let playlist_id = PlaylistId::from_id(id).context("invalid playlist id")?;

        let mut raw_items = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .playlist_items_manual(
                    playlist_id.as_ref(),
                    None,
                    None,
                    Some(PAGE_LIMIT),
                    Some(offset),
                )
                .await
                .context("failed to fetch playlist tracks")?;
            let total = page.total;
            let fetched = page.items.len() as u32;
            raw_items.extend(page.items);
            offset += fetched;
            if fetched == 0 || offset >= total {
                break;
            }
        }

        let items: Vec<FullTrack> = raw_items
            .into_iter()
            .filter(|item| !item.is_local)
            .filter_map(|item| match item.item {
                Some(PlayableItem::Track(track)) if track.id.is_some() => Some(track),
                _ => None,
            })
            .collect();

        let mut tracks: Vec<Track> = items.iter().map(track_from_full).collect();

        let ids = tracks
            .iter()
            .map(|track| TrackId::from_id(track.id.as_str()))
            .collect::<Result<Vec<_>, _>>()
            .context("invalid track id in playlist")?;

        let mut saved_flags: Vec<bool> = Vec::with_capacity(ids.len());
        let mut check_ok = true;
        for chunk in ids.chunks(SAVED_CHUNK) {
            match self
                .client
                .current_user_saved_tracks_contains(chunk.to_vec())
                .await
            {
                Ok(flags) => saved_flags.extend_from_slice(&flags),
                Err(_) => {
                    check_ok = false;
                    break;
                }
            }
        }
        if check_ok {
            for (track, saved) in tracks.iter_mut().zip(saved_flags) {
                track.is_saved = saved;
            }
        }
        Ok(tracks)
    }

    /// Fetches one album (info + full track list) by bare ID.
    pub async fn album(&self, id: &str) -> anyhow::Result<AlbumDetails> {
        let album_id = AlbumId::from_id(id).context("invalid album id")?;
        let full = self
            .client
            .album(album_id.as_ref(), None)
            .await
            .context("failed to fetch album")?;
        let info = album_info_full(&full);
        let art = full.images.last().map(|image| image.url.as_str());

        // Page past the track page embedded in `FullAlbum` (which may be
        // truncated at 50 items for large albums).
        let mut tracks = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .album_track_manual(album_id.as_ref(), None, Some(PAGE_LIMIT), Some(offset))
                .await
                .context("failed to fetch album tracks")?;
            let total = page.total;
            let fetched = page.items.len() as u32;
            tracks.extend(
                page.items
                    .iter()
                    .map(|track| track_from_simplified(track, &full.name, art)),
            );
            offset += fetched;
            if fetched == 0 || offset >= total {
                break;
            }
        }

        Ok(AlbumDetails { info, tracks })
    }

    /// Fetches one artist (info + up to 20 albums + top tracks) by bare ID.
    ///
    /// The `market` parameter of `artist_top_tracks` is optional in rspotify
    /// and in the Spotify endpoint; `None` returns top tracks without market
    /// filtering.
    pub async fn artist(&self, id: &str) -> anyhow::Result<ArtistDetails> {
        let artist_id = ArtistId::from_id(id).context("invalid artist id")?;

        let full = self
            .client
            .artist(artist_id.as_ref())
            .await
            .context("failed to fetch artist")?;
        let info = artist_info(&full);

        let albums_page = self
            .client
            .artist_albums_manual(artist_id.as_ref(), [], None, Some(10), Some(0))
            .await
            .context("failed to fetch artist albums")?;
        let mut seen_names: HashSet<String> = HashSet::new();
        let mut albums = Vec::new();
        for album in &albums_page.items {
            if let Some(info) = album_info_simplified(album) {
                // The same album can appear once per album group; keep the
                // first occurrence of each (case-insensitive) name.
                if seen_names.insert(info.name.to_lowercase()) {
                    albums.push(info);
                }
            }
        }

        // Spotify removed the artist-top-tracks endpoint in the Feb 2026 API
        // migration (it now answers 403), so this is best-effort: the artist
        // view still renders the header and discography without it.
        let top_tracks = match self.client.artist_top_tracks(artist_id.as_ref(), None).await {
            Ok(top) => top.iter().map(track_from_full).collect(),
            Err(e) => {
                tracing::warn!("artist top tracks unavailable: {e}");
                Vec::new()
            }
        };

        Ok(ArtistDetails {
            info,
            albums,
            top_tracks,
        })
    }

    /// Searches tracks, albums, artists and playlists for `query` (10 hits
    /// per type — the Feb 2026 API migration caps search `limit` at 10).
    /// rspotify's `search` takes a single `SearchType`, so one request per type.
    pub async fn search(&self, query: &str) -> anyhow::Result<SearchResults> {
        let mut results = SearchResults::default();

        let res = self.search_typed(query, SearchType::Track).await?;
        if let SearchResult::Tracks(page) = res {
            results.tracks = page.items.iter().map(track_from_full).collect();
        }
        let res = self.search_typed(query, SearchType::Album).await?;
        if let SearchResult::Albums(page) = res {
            results.albums = page
                .items
                .iter()
                .filter_map(album_info_simplified)
                .collect();
        }
        let res = self.search_typed(query, SearchType::Artist).await?;
        if let SearchResult::Artists(page) = res {
            results.artists = page.items.iter().map(artist_info).collect();
        }
        let res = self.search_typed(query, SearchType::Playlist).await?;
        if let SearchResult::Playlists(page) = res {
            results.playlists = page.items.iter().map(playlist_info).collect();
        }

        Ok(results)
    }

    /// Runs one typed search request, returning the raw rspotify result.
    async fn search_typed(&self, query: &str, kind: SearchType) -> anyhow::Result<SearchResult> {
        self.client
            .search(query, kind, None, None, Some(10), None)
            .await
            .context("search request failed")
    }

    /// Adds the tracks with the given bare IDs to "Liked Songs" (chunked:
    /// the endpoint accepts at most 50 ids per call).
    pub async fn save_tracks(&self, ids: &[String]) -> anyhow::Result<()> {
        for chunk in ids.chunks(SAVED_CHUNK) {
            let track_ids = chunk
                .iter()
                .map(|id| TrackId::from_id(id.as_str()))
                .collect::<Result<Vec<_>, _>>()
                .context("invalid track id")?;
            self.client
                .current_user_saved_tracks_add(track_ids)
                .await
                .context("failed to save tracks")?;
        }
        Ok(())
    }

    /// Removes the tracks with the given bare IDs from "Liked Songs"
    /// (chunked: the endpoint accepts at most 50 ids per call).
    pub async fn remove_saved_tracks(&self, ids: &[String]) -> anyhow::Result<()> {
        for chunk in ids.chunks(SAVED_CHUNK) {
            let track_ids = chunk
                .iter()
                .map(|id| TrackId::from_id(id.as_str()))
                .collect::<Result<Vec<_>, _>>()
                .context("invalid track id")?;
            self.client
                .current_user_saved_tracks_delete(track_ids)
                .await
                .context("failed to remove saved tracks")?;
        }
        Ok(())
    }
}

/// Formats an error from a [`WebApi`] call for the UI. A failure carrying
/// HTTP status 429 (Spotify rate-limiting the endpoint / quota exceeded)
/// maps to [`RATE_LIMITED_MESSAGE`]; any other error keeps its current text.
pub fn friendly_error(e: &anyhow::Error) -> String {
    if is_rate_limited(e) {
        RATE_LIMITED_MESSAGE.to_string()
    } else {
        e.to_string()
    }
}

/// True when the error chain carries an rspotify HTTP failure answered with
/// status 429: `rspotify::ClientError::Http` wrapping the HTTP client's
/// `StatusCode` variant.
fn is_rate_limited(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rspotify::ClientError>(),
            Some(rspotify::ClientError::Http(http))
                if matches!(
                    &**http,
                    rspotify::http::HttpError::StatusCode(response)
                        if response.status().as_u16() == 429
                )
        )
    })
}

/// `(bare id, full URI)` for an optional track id; empty strings when absent.
fn track_id_and_uri(id: &Option<TrackId<'_>>) -> (String, String) {
    match id {
        Some(id) => (id.id().to_string(), id.uri()),
        None => (String::new(), String::new()),
    }
}

/// Maps a `FullTrack` (playlist item, search hit, top track) into a [`Track`].
/// `is_saved` is left `false`; resolve it via the check-saved endpoint where
/// needed.
fn track_from_full(track: &FullTrack) -> Track {
    let (id, uri) = track_id_and_uri(&track.id);
    Track {
        id,
        uri,
        title: track.name.clone(),
        artists: track.artists.iter().map(|artist| artist.name.clone()).collect(),
        album: track.album.name.clone(),
        album_art_url: track.album.images.last().map(|image| image.url.clone()),
        duration_ms: track.duration.num_milliseconds().max(0) as u32,
        track_number: track.track_number,
        disc_number: track.disc_number.max(0) as u32,
        is_saved: false,
    }
}

/// Maps a `SimplifiedTrack` (album track list) into a [`Track`], taking the
/// album name and art from the parent `FullAlbum` (simplified tracks carry
/// neither).
fn track_from_simplified(track: &SimplifiedTrack, album_name: &str, art: Option<&str>) -> Track {
    let (id, uri) = track_id_and_uri(&track.id);
    Track {
        id,
        uri,
        title: track.name.clone(),
        artists: track.artists.iter().map(|artist| artist.name.clone()).collect(),
        album: album_name.to_string(),
        album_art_url: art.map(str::to_string),
        duration_ms: track.duration.num_milliseconds().max(0) as u32,
        track_number: track.track_number,
        disc_number: track.disc_number.max(0) as u32,
        is_saved: false,
    }
}

/// Maps a `SimplifiedPlaylist` (user's playlists, search hits) into a
/// [`PlaylistInfo`]. `description` is unavailable at this level; see
/// [`WebApi::current_user_playlists`].
fn playlist_info(playlist: &SimplifiedPlaylist) -> PlaylistInfo {
    PlaylistInfo {
        id: playlist.id.id().to_string(),
        name: playlist.name.clone(),
        description: None,
        owner: playlist.owner.display_name.clone().unwrap_or_default(),
        tracks_total: playlist.items.total,
        image_url: playlist.images.last().map(|image| image.url.clone()),
    }
}

/// Maps a `FullAlbum` (saved albums, album details) into an [`AlbumInfo`].
fn album_info_full(album: &FullAlbum) -> AlbumInfo {
    AlbumInfo {
        id: album.id.id().to_string(),
        name: album.name.clone(),
        artists: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        total_tracks: album.tracks.total,
        image_url: album.images.last().map(|image| image.url.clone()),
        release_date: album.release_date.clone(),
    }
}

/// Maps a `SimplifiedAlbum` (artist albums, search hits) into an [`AlbumInfo`].
/// Returns `None` when the album has no id. `total_tracks` is `0` because
/// `SimplifiedAlbum` in rspotify 0.13 does not expose a track count.
fn album_info_simplified(album: &SimplifiedAlbum) -> Option<AlbumInfo> {
    let id = album.id.as_ref()?;
    Some(AlbumInfo {
        id: id.id().to_string(),
        name: album.name.clone(),
        artists: album.artists.iter().map(|artist| artist.name.clone()).collect(),
        total_tracks: 0,
        image_url: album.images.last().map(|image| image.url.clone()),
        release_date: album.release_date.clone().unwrap_or_default(),
    })
}

/// Maps a `FullArtist` (followed artists, search hits, artist details) into
/// an [`ArtistInfo`].
fn artist_info(artist: &FullArtist) -> ArtistInfo {
    ArtistInfo {
        id: artist.id.id().to_string(),
        name: artist.name.clone(),
        image_url: artist.images.last().map(|image| image.url.clone()),
        followers: artist.followers.total,
    }
}
