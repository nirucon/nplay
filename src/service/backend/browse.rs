//! `browse` for every source/section and the federated `search` (1.x
//! `ServiceBackend.browse`, `_browse_*`, `search`).

use super::pyval::{backend_error, int_or, internal, params_error, py_int, py_str, str_or};
use super::ServiceBackend;
use crate::model::Track;
use crate::redact::redact_text;
use serde_json::{json, Map, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

type Items = (Vec<Track>, String);

fn container(source: &str, id: &str, title: &str, kind: &str) -> Track {
    let mut t = Track::new(source, id, title);
    t.kind = kind.into();
    t.seekable = false;
    t
}

/// `str(args.get(key) or query)`.
fn arg_or(args: &Map<String, Value>, key: &str, query: &str) -> String {
    super::pyval::get_or(args, key)
        .map(py_str)
        .unwrap_or_else(|| query.to_string())
}

impl ServiceBackend {
    pub(super) fn browse(&self, p: &Map<String, Value>) -> Result<Value, crate::service::RpcError> {
        let source = str_or(p, "source", "local");
        let section = str_or(p, "section", "all");
        let query = str_or(p, "query", "").trim().to_string();
        let limit = int_or(p, "limit", 200)?.clamp(1, 1000);
        let offset = int_or(p, "offset", 0)?.max(0);
        let args = match p.get("args") {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let app = &self.app;
        let (items, title): Items = match source.as_str() {
            "local" => self.browse_local(&section, &query, limit, offset, &args)?,
            "navidrome" => self.browse_navidrome(&section, &query, limit, &args)?,
            "library" => self.browse_library(&section, &query, limit, &args)?,
            "sr" => self.browse_sr(&section, &query, limit, &args)?,
            "youtube" => {
                if !app.cfg.getbool("youtube_enabled", true) || !app.yt.available() {
                    return Err(backend_error(
                        "YouTube is disabled or yt-dlp is not installed",
                        None,
                    ));
                }
                let items = if query.is_empty() {
                    Vec::new()
                } else {
                    app.yt.search(&query, limit as usize).map_err(internal)?
                };
                let q = if query.is_empty() { "search" } else { &query };
                (items, format!("YouTube · {q}"))
            }
            "spotify" => self.browse_spotify(&section, &query, limit, &args)?,
            "radio" => (
                app.db.radios().map_err(internal)?,
                "Custom radio".to_string(),
            ),
            _ => {
                return Err(params_error(
                    "unknown browse source",
                    Some(json!({"source": source})),
                ))
            }
        };
        Ok(json!({
            "source": source,
            "section": section,
            "title": title,
            "items": self.wires(&items),
        }))
    }

    fn browse_local(
        &self,
        section: &str,
        query: &str,
        limit: i64,
        offset: i64,
        args: &Map<String, Value>,
    ) -> Result<Items, crate::service::RpcError> {
        let db = &self.app.db;
        let lim = limit as usize;
        Ok(match section {
            "all" | "search" => {
                let xs = db.search(query, limit + offset).map_err(internal)?;
                let xs: Vec<Track> = xs.into_iter().skip(offset as usize).take(lim).collect();
                let q = if query.is_empty() {
                    "all tracks"
                } else {
                    query
                };
                (xs, format!("Local music · {q}"))
            }
            "artists" => {
                let rows = db.local_artists().map_err(internal)?;
                let xs = rows
                    .into_iter()
                    .take(lim)
                    .map(|(name, _, _)| {
                        let mut t = container("local", &name, &name, "artist");
                        t.artist = "Artist".into();
                        t.meta.insert("local_artist".into(), Value::String(name));
                        t
                    })
                    .collect();
                (xs, "Local artists".into())
            }
            "albums" => {
                let rows = db.local_albums().map_err(internal)?;
                let xs = rows
                    .into_iter()
                    .take(lim)
                    .map(|(title, action, _)| {
                        let raw = match action.split_once("local:album:") {
                            Some((_, r)) => r.to_string(),
                            None => action.clone(),
                        };
                        let (artist, album) = match raw.split_once('\t') {
                            Some((a, b)) => (a.to_string(), b.to_string()),
                            None => (raw.clone(), String::new()),
                        };
                        let name = if album.is_empty() { title } else { album };
                        let mut t = container("local", &raw, &name, "album");
                        t.artist = artist.clone();
                        t.album = name;
                        t.meta.insert("local_artist".into(), Value::String(artist));
                        t
                    })
                    .collect();
                (xs, "Local albums".into())
            }
            "folders" => {
                let rows = db.local_folders().map_err(internal)?;
                let xs = rows
                    .into_iter()
                    .take(lim)
                    .map(|(title, action, _)| {
                        let id = match action.split_once("local:folder:") {
                            Some((_, r)) => r.to_string(),
                            None => action.clone(),
                        };
                        container("local", &id, &title, "folder")
                    })
                    .collect();
                (xs, "Local folders".into())
            }
            "recent" => (
                db.recently_added(limit).map_err(internal)?,
                "Recently added".into(),
            ),
            "random" => (
                db.random_tracks(limit).map_err(internal)?,
                "Random tracks".into(),
            ),
            "artist" => {
                let name = arg_or(args, "name", query);
                if name.is_empty() {
                    return Err(params_error("artist name is required", None));
                }
                (
                    db.tracks_by_artist(&name).map_err(internal)?,
                    format!("Local · {name}"),
                )
            }
            "album" => {
                let artist = arg_or(args, "artist", "");
                let album = arg_or(args, "album", query);
                if album.is_empty() {
                    return Err(params_error("album name is required", None));
                }
                (
                    db.tracks_by_album(&artist, &album).map_err(internal)?,
                    format!("Local · {album}"),
                )
            }
            "folder" => {
                let path = arg_or(args, "path", query);
                if path.is_empty() {
                    return Err(params_error("folder path is required", None));
                }
                (
                    db.tracks_by_folder(&path).map_err(internal)?,
                    "Local folder".into(),
                )
            }
            _ => {
                return Err(params_error(
                    "unknown local section",
                    Some(json!({"section": section})),
                ))
            }
        })
    }

    fn browse_navidrome(
        &self,
        section: &str,
        query: &str,
        limit: i64,
        args: &Map<String, Value>,
    ) -> Result<Items, crate::service::RpcError> {
        let n = self
            .app
            .nav()
            .ok_or_else(|| backend_error("Navidrome is disabled or not configured", None))?;
        let lim = limit as usize;
        let id = || arg_or(args, "id", query);
        let (r, title) = match section {
            "newest" => (
                n.albums("newest", lim),
                "Navidrome · Recently added".to_string(),
            ),
            "random" => (n.albums("random", lim), "Navidrome · Random albums".into()),
            "random_tracks" => (n.random_tracks(lim), "Navidrome · Random tracks".into()),
            "artists" => (n.artists(), "Navidrome · Artists".into()),
            "albums" => (
                n.albums("alphabeticalByName", lim),
                "Navidrome · Albums".into(),
            ),
            "playlists" => (n.playlists(), "Navidrome · Playlists".into()),
            "search" => (
                n.search_library(query, lim, 20, 10),
                format!("Navidrome · {query}"),
            ),
            "album" => (n.album_tracks(&id()), "Navidrome album".into()),
            "artist" => (n.artist_albums(&id()), "Navidrome artist".into()),
            "playlist" => (n.playlist_tracks(&id()), "Navidrome playlist".into()),
            _ => {
                return Err(params_error(
                    "unknown Navidrome section",
                    Some(json!({"section": section})),
                ))
            }
        };
        Ok((r.map_err(internal)?, title))
    }

    fn browse_library(
        &self,
        section: &str,
        query: &str,
        limit: i64,
        args: &Map<String, Value>,
    ) -> Result<Items, crate::service::RpcError> {
        let db = &self.app.db;
        Ok(match section {
            "playlists" => {
                let rows = db.playlists().map_err(internal)?;
                let xs = rows
                    .into_iter()
                    .take(limit as usize)
                    .map(|(name, action, subtitle)| {
                        let pid = action.split_once(':').map(|(_, b)| b).unwrap_or(&action);
                        let mut t = container("library", pid, &name, "playlist");
                        t.artist = "Playlist".into();
                        t.meta.insert("item_count".into(), Value::String(subtitle));
                        t
                    })
                    .collect();
                (xs, "Playlists".into())
            }
            "playlist" => {
                let raw = super::pyval::get_or(args, "id")
                    .cloned()
                    .unwrap_or_else(|| Value::String(query.to_string()));
                let pid = py_int(Some(&raw))?;
                (
                    db.playlist_tracks(pid).map_err(internal)?,
                    db.playlist_name(pid).map_err(internal)?,
                )
            }
            "favorites" => {
                let mut xs = db.favorites().map_err(internal)?;
                xs.truncate(limit as usize);
                (xs, "Favorites".into())
            }
            "history" => (
                db.history(limit).map_err(internal)?,
                "Recently played".into(),
            ),
            "discoveries" => (
                db.discoveries(limit).map_err(internal)?,
                "Discoveries".into(),
            ),
            "bookmarks" => (db.bookmarks(limit).map_err(internal)?, "Bookmarks".into()),
            _ => {
                return Err(params_error(
                    "unknown library section",
                    Some(json!({"section": section})),
                ))
            }
        })
    }

    fn browse_sr(
        &self,
        section: &str,
        query: &str,
        limit: i64,
        args: &Map<String, Value>,
    ) -> Result<Items, crate::service::RpcError> {
        let app = &self.app;
        if !app.cfg.getbool("sr_enabled", true) {
            return Err(backend_error("Sveriges Radio is disabled", None));
        }
        let lim = limit as usize;
        Ok(match section {
            "live" => (app.sr.live(), "Sveriges Radio · Live".into()),
            "programs" => (
                app.sr.programs(query, lim).map_err(internal)?,
                "Sveriges Radio · Programmes".into(),
            ),
            "search" => (
                app.sr.search_programmes_and_episodes(query, lim),
                format!("Sveriges Radio · {query}"),
            ),
            "program" => (
                app.sr
                    .program_episodes(&arg_or(args, "id", query), lim)
                    .map_err(internal)?,
                "Sveriges Radio · Episodes".into(),
            ),
            _ => {
                return Err(params_error(
                    "unknown Sveriges Radio section",
                    Some(json!({"section": section})),
                ))
            }
        })
    }

    pub(super) fn browse_spotify(
        &self,
        section: &str,
        query: &str,
        limit: i64,
        args: &Map<String, Value>,
    ) -> Result<Items, crate::service::RpcError> {
        let app = &self.app;
        if !app.cfg.getbool("spotify_enabled", false) || !app.spotify_configured() {
            return Err(backend_error("Spotify is disabled or not connected", None));
        }
        let sp = &app.spotify;
        let id = || arg_or(args, "id", query);
        let (r, title) = match section {
            "search" => (
                sp.search(query, limit as usize),
                format!("Spotify · {query}"),
            ),
            "playlists" => (sp.playlists(), "Spotify · Playlists".into()),
            "playlist" => (sp.playlist_items(&id()), "Spotify playlist".into()),
            "album" => (sp.album_tracks(&id()), "Spotify album".into()),
            "top_tracks" => (sp.top_tracks(), "Spotify · Top tracks".into()),
            "top_artists" => (sp.top_artists(), "Spotify · Top artists".into()),
            "recent" => (sp.recent(), "Spotify · Recently played".into()),
            "devices" => (
                sp.devices()
                    .map(|ds| ds.iter().map(crate::spotify::device_track).collect()),
                "Spotify · Devices".into(),
            ),
            _ => {
                return Err(params_error(
                    "unknown Spotify section",
                    Some(json!({"section": section})),
                ))
            }
        };
        Ok((r.map_err(internal)?, title))
    }

    /// Federated search: every enabled, configured source in parallel (30 s
    /// budget per source); groups in the fixed order local, navidrome,
    /// spotify, sr, youtube, each with its own `error`.
    pub(super) fn search(&self, p: &Map<String, Value>) -> Result<Value, crate::service::RpcError> {
        let query = str_or(p, "query", "").trim().to_string();
        if query.is_empty() {
            return Ok(json!({"query": "", "groups": []}));
        }
        if query.chars().count() > 200 {
            return Err(params_error("query is too long", None));
        }
        let limit = int_or(p, "limit", 20)?.clamp(1, 100) as usize;
        let allowed: Option<Vec<String>> = match p.get("sources") {
            Some(Value::Array(a)) => Some(a.iter().map(py_str).collect()),
            _ => None,
        };
        let wants = |s: &str| allowed.as_ref().is_none_or(|a| a.iter().any(|x| x == s));
        let app = &self.app;
        type Job = Box<dyn FnOnce() -> Result<Vec<Track>, String> + Send>;
        let mut jobs: Vec<(&str, &str, Job)> = Vec::new();
        if wants("local") {
            let (a, q) = (app.clone(), query.clone());
            jobs.push((
                "local",
                "Local",
                Box::new(move || a.db.search(&q, limit as i64).map_err(|e| e.to_string())),
            ));
        }
        if wants("navidrome") && app.nav_configured() && app.cfg.getbool("navidrome_enabled", true)
        {
            let (a, q) = (app.clone(), query.clone());
            jobs.push((
                "navidrome",
                "Navidrome",
                Box::new(move || match a.nav() {
                    Some(n) => n
                        .search_library(&q, limit, 10, 5)
                        .map_err(|e| e.to_string()),
                    None => Err("'NoneType' object has no attribute 'search_library'".into()),
                }),
            ));
        }
        if wants("spotify") && app.spotify_configured() && app.cfg.getbool("spotify_enabled", false)
        {
            let (a, q) = (app.clone(), query.clone());
            jobs.push((
                "spotify",
                "Spotify",
                Box::new(move || a.spotify.search(&q, limit).map_err(|e| e.to_string())),
            ));
        }
        if wants("sr") && app.cfg.getbool("sr_enabled", true) {
            let (a, q) = (app.clone(), query.clone());
            jobs.push((
                "sr",
                "Sveriges Radio",
                Box::new(move || Ok(a.sr.search_programmes_and_episodes(&q, limit))),
            ));
        }
        if wants("youtube") && app.yt.available() && app.cfg.getbool("youtube_enabled", true) {
            let (a, q) = (app.clone(), query.clone());
            jobs.push((
                "youtube",
                "YouTube",
                Box::new(move || a.yt.search(&q, limit).map_err(|e| e.to_string())),
            ));
        }
        let mut pending = Vec::new();
        for (key, label, job) in jobs {
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name(format!("nplay-search-{key}"))
                .spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                        .unwrap_or_else(|_| Err("search failed".into()));
                    let _ = tx.send(r);
                });
            pending.push((key, label, spawned.ok().map(|_| rx)));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut groups = Vec::new();
        for (key, label, rx) in pending {
            let r = match rx {
                Some(rx) => rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(|_| Err(String::new())),
                None => Err("could not start the search worker".into()),
            };
            let (items, error) = match r {
                Ok(xs) => (xs, String::new()),
                Err(e) => {
                    let e = redact_text(&e);
                    warn!("service search {key} failed: {e}");
                    (Vec::new(), super::pyval::clip_chars(&e, 200))
                }
            };
            groups.push(json!({
                "source": key,
                "label": label,
                "items": self.wires(&items),
                "error": error,
            }));
        }
        Ok(json!({"query": query, "groups": groups}))
    }
}
