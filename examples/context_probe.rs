//! Diagnostic probe (not part of the app): reproduces the "context is empty"
//! path in `player.rs::context_track_uris` for one album and one playlist,
//! printing the raw `id`/`uri` values the Web API mapping produces.
//!
//! Run: `cmd //c vcbuild.bat run --example context_probe`

#[path = "../src/config.rs"]
mod config;
#[path = "../src/types.rs"]
mod types;
#[path = "../src/webapi.rs"]
mod webapi;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cache_path = config::auth_cache_path().expect("no cache dir");
    let raw = std::fs::read_to_string(&cache_path)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let token = parsed["access_token"]
        .as_str()
        .expect("cached credentials have no access_token")
        .to_string();
    println!("token loaded ({} chars), expiry field: {}", token.len(), parsed["expires_at"]);

    // 1. The failing album, exactly as context_track_uris does.
    let api = webapi::WebApi::new(token.clone());
    match api.album("38KDcpMrViyGMwOuyXT3VS").await {
        Ok(details) => {
            println!("\n[album 38KDcpMrViyGMwOuyXT3VS] OK: {} tracks", details.tracks.len());
            for (i, track) in details.tracks.iter().take(5).enumerate() {
                println!("  [{}] id: {:?} | uri: {:?}", i, track.id, track.uri);
            }
            let playable: Vec<_> = details
                .tracks
                .iter()
                .filter(|t| t.uri.starts_with("spotify:track:"))
                .collect();
            println!(
                "  -> after the app's `spotify:track:` filter: {} of {}",
                playable.len(),
                details.tracks.len()
            );
            if let Some(first_unplayable) = details
                .tracks
                .iter()
                .find(|t| !t.uri.starts_with("spotify:track:"))
            {
                println!("  first filtered-out track: {:?}", first_unplayable);
            }
        }
        Err(e) => println!("\n[album 38KDcpMrViyGMwOuyXT3VS] ERROR:\n  {:#}", e),
    }

    // 1b. Raw calls: what does the migrated API actually answer for the
    // album and its track page? (status + verbatim body snippet)
    let client = reqwest::Client::new();
    for (label, url) in [
        ("album", "https://api.spotify.com/v1/albums/38KDcpMrViyGMwOuyXT3VS".to_string()),
        (
            "album tracks limit=50 offset=0",
            "https://api.spotify.com/v1/albums/38KDcpMrViyGMwOuyXT3VS/tracks?limit=50&offset=0".to_string(),
        ),
        (
            "album tracks market=from_token",
            "https://api.spotify.com/v1/albums/38KDcpMrViyGMwOuyXT3VS/tracks?limit=50&offset=0&market=from_token".to_string(),
        ),
    ] {
        match client.get(&url).bearer_auth(&token).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let snippet: String = body.chars().take(600).collect();
                println!("\n[raw {}] HTTP {}\n  {}", label, status, snippet.replace('\n', " "));
                if label == "album" {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        println!(
                            "  parsed: name={} total_tracks={}",
                            v["name"], v["total_tracks"]
                        );
                    }
                }
            }
            Err(e) => println!("\n[raw {}] REQUEST ERROR: {:#}", label, e),
        }
    }

    // 2. The user's first playlist, for comparison.
    let api2 = webapi::WebApi::new(token.clone());
    match api2.current_user_playlists().await {
        Ok(playlists) => {
            println!("\n[current_user_playlists] OK: {} playlists", playlists.len());
            match playlists.first() {
                Some(first) => {
                    println!(
                        "  first playlist: id={:?} name={:?} tracks_total={}",
                        first.id, first.name, first.tracks_total
                    );
                    match api2.playlist_tracks(&first.id).await {
                        Ok(tracks) => {
                            println!("[playlist {}] OK: {} tracks", first.id, tracks.len());
                            for (i, track) in tracks.iter().take(5).enumerate() {
                                println!("  [{}] id: {:?} | uri: {:?}", i, track.id, track.uri);
                            }
                            let playable: Vec<_> = tracks
                                .iter()
                                .filter(|t| t.uri.starts_with("spotify:track:"))
                                .collect();
                            println!(
                                "  -> after the app's `spotify:track:` filter: {} of {}",
                                playable.len(),
                                tracks.len()
                            );
                        }
                        Err(e) => println!("[playlist {}] ERROR:\n  {:#}", first.id, e),
                    }
                }
                None => println!("  no playlists to probe"),
            }
        }
        Err(e) => println!("\n[current_user_playlists] ERROR:\n  {:#}", e),
    }

    Ok(())
}
