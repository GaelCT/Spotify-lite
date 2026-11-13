//! Diagnostic probe (not part of the app): reads the cached OAuth token and
//! calls the failing Web API endpoint two ways — raw reqwest and the app's
//! `WebApi` — printing the exact HTTP status / error chain.
//!
//! Run: `cmd //c vcbuild.bat run --example webapi_probe`

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

    // 1. Raw call: GET /v1/me — proves token validity + product type.
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.spotify.com/v1/me")
        .bearer_auth(&token)
        .send()
        .await?;
    println!("\n[/v1/me] HTTP {}", resp.status());
    let body = resp.text().await?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        println!(
            "  user: {} | product: {} | country: {}",
            v["display_name"], v["product"], v["country"]
        );
    } else {
        println!("  body: {}", &body[..body.len().min(300)]);
    }

    // 2. Raw call: the exact failing endpoint.
    let resp = client
        .get("https://api.spotify.com/v1/me/playlists?offset=0&limit=50")
        .bearer_auth(&token)
        .send()
        .await?;
    println!("\n[/v1/me/playlists raw] HTTP {}", resp.status());
    let body = resp.text().await?;
    println!("  body: {}", &body[..body.len().min(500)]);

    // 3b. Search limit sweep: which limits does the migrated API accept?
    for limit in ["1", "5", "10", "20", "50"] {
        let url = format!("https://api.spotify.com/v1/search?q=kanye&limit={}&type=track", limit);
        let resp = client.get(&url).bearer_auth(&token).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        let snippet: String = body.chars().take(120).collect();
        println!("[raw search limit={}] HTTP {} — {}", limit, status, snippet.replace('\n', " "));
    }
    {
        let url = "https://api.spotify.com/v1/search?q=kanye&type=track".to_string();
        let resp = client.get(&url).bearer_auth(&token).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        let snippet: String = body.chars().take(120).collect();
        println!("[raw search no-limit] HTTP {} — {}", status, snippet.replace('\n', " "));
    }

    // 4. The app's WebApi path — exercise every endpoint we use.
    let api = webapi::WebApi::new(token.clone());
    match api.current_user_playlists().await {
        Ok(list) => println!("\n[WebApi] playlists OK: {}", list.len()),
        Err(e) => println!("\n[WebApi] playlists ERROR:\n  {:#}", e),
    }
    match api.saved_albums().await {
        Ok(list) => println!("[WebApi] saved_albums OK: {}", list.len()),
        Err(e) => println!("[WebApi] saved_albums ERROR:\n  {:#}", e),
    }
    match api.followed_artists().await {
        Ok(list) => println!("[WebApi] followed_artists OK: {}", list.len()),
        Err(e) => println!("[WebApi] followed_artists ERROR:\n  {:#}", e),
    }
    match api.search("kanye").await {
        Ok(r) => println!(
            "[WebApi] search OK: {} tracks, {} albums, {} artists, {} playlists",
            r.tracks.len(),
            r.albums.len(),
            r.artists.len(),
            r.playlists.len()
        ),
        Err(e) => println!("[WebApi] search ERROR:\n  {:#}", e),
    }
    if let Ok(playlists) = api.current_user_playlists().await {
        if let Some(first) = playlists.first() {
            match api.playlist_tracks(&first.id).await {
                Ok(tracks) => println!("[WebApi] playlist_tracks OK: {}", tracks.len()),
                Err(e) => println!("[WebApi] playlist_tracks ERROR:\n  {:#}", e),
            }
        }
        if let Some(albums) = api.saved_albums().await.ok().filter(|a| !a.is_empty()) {
            let id = albums[0].id.clone();
            match api.album(&id).await {
                Ok(d) => println!("[WebApi] album OK: {} tracks", d.tracks.len()),
                Err(e) => println!("[WebApi] album ERROR:\n  {:#}", e),
            }
        }
        let mut artist_id = api
            .followed_artists()
            .await
            .ok()
            .filter(|a| !a.is_empty())
            .map(|a| a[0].id.clone());
        if artist_id.is_none() {
            artist_id = api
                .search("kanye")
                .await
                .ok()
                .and_then(|r| r.artists.into_iter().next().map(|a| a.id));
        }
        if let Some(id) = artist_id {
            // Raw sweep: what does the migrated artist-albums endpoint accept?
            for (label, url) in [
                ("limit=20 no groups", format!("https://api.spotify.com/v1/artists/{}/albums?limit=20", id)),
                ("limit=10 no groups", format!("https://api.spotify.com/v1/artists/{}/albums?limit=10", id)),
                ("limit=50 no groups", format!("https://api.spotify.com/v1/artists/{}/albums?limit=50", id)),
                ("limit=20 groups=album,single", format!("https://api.spotify.com/v1/artists/{}/albums?limit=20&include_groups=album,single", id)),
                ("limit=20 empty groups", format!("https://api.spotify.com/v1/artists/{}/albums?limit=20&include_groups=", id)),
            ] {
                let resp = client.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                let body = resp.text().await?;
                let snippet: String = body.chars().take(100).collect();
                println!("[raw artist-albums {}] HTTP {} — {}", label, status, snippet.replace('\n', " "));
            }
            match api.artist(&id).await {
                Ok(d) => println!("[WebApi] artist OK: {} albums, {} top tracks", d.albums.len(), d.top_tracks.len()),
                Err(e) => println!("[WebApi] artist ERROR:\n  {:#}", e),
            }
        }
    }

    Ok(())
}
