//! Source browsing and provider actions (port of 1.x `nplay/navigation.py`
//! plus the UI-side `nav_setup`). Owner: engine track.
//!
//! All methods are on [`App`] and take the [`UiRef`] to draw into. Network
//! work always runs through [`App::async_run`] (a worker thread whose result
//! is delivered with `Ui::post`), so the UI thread never blocks on a provider.
//!
//! Public API: `root_browse`, `local_home`, `radio_home`, `scan_async`,
//! `add_radio`, `playlists_home`, `add_selected_to_playlist`,
//! `spotify_add_playlist`, `playlist_remove`, `playlist_move`,
//! `playlist_rename`, `playlist_delete`, `source_switch_menu`,
//! `source_handoff`, `open_source(action)` (every menu action id of 1.x:
//! `stats:*`, `ctx:*`, `local:*`, `radio*`, `sr*`, `smart:*`, `spotify:*`,
//! `playlist:*`, `settings:*`, `visual-style:*`, ...), `open_spotify_home`,
//! `spotify_setup`, `spotify_load`, `spotify_search`, `spotify_local_menu`,
//! `spotify_local_diagnostics`, `spotify_devices`, `open_nav_home`,
//! `nav_action(action)` (`nav:*` ids), `nav_setup`, `nav_goto_artist`,
//! `nav_goto_album`, `open_item(track)`, `load_sr_programs`,
//! `search_sr_all`, `search_sr_programs`, `refresh_radio_async`,
//! `search_provider(nav|sr|yt)`, `search_async` (Quick Find).

use crate::app::{entry_rows, track_rows, App, Row, UiRef};
use crate::core::Result;
use crate::model::Track;
use crate::spotify::Spotify;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

fn meta_or_id(t: &Track, k: &str) -> String {
    let v = t.meta_str(k);
    if v.is_empty() {
        t.id.clone()
    } else {
        v
    }
}

fn show(ui: &UiRef, title: &str, xs: Vec<Track>, empty: &str) {
    ui.show_list(title, track_rows(xs), empty, None, true);
}

fn replace(ui: &UiRef, title: &str, xs: Vec<Track>, empty: &str) {
    ui.replace_list(title, track_rows(xs), empty, None);
}

fn prompt<F>(ui: &UiRef, label: &str, initial: &str, f: F)
where
    F: FnOnce(String) + Send + 'static,
{
    ui.prompt(
        label,
        initial,
        false,
        Box::new(move |v| f(v.unwrap_or_default())),
    );
}

/// Copy text with wl-copy, xclip or xsel (2 s timeout).
fn copy_to_clipboard(text: &str) -> Option<bool> {
    use std::io::Write;
    let w = crate::paths::which;
    let clip = w("wl-copy").or_else(|| w("xclip")).or_else(|| w("xsel"))?;
    let name = clip
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let args: Vec<&str> = match name.as_str() {
        "wl-copy" => vec![],
        "xclip" => vec!["-selection", "clipboard"],
        _ => vec!["--clipboard", "--input"],
    };
    let child = std::process::Command::new(&clip)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Some(false);
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Some(false);
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Some(st.success()),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(false);
            }
        }
    }
}

impl App {
    pub fn root_browse(&self, ui: &UiRef) {
        let mut items = Vec::new();
        let (cur, ctx_len, idx, qlen) = {
            let st = self.state();
            (
                st.current.clone(),
                st.play_context.len(),
                st.context_index,
                st.queue.len(),
            )
        };
        if let Some(c) = &cur {
            let mut detail = if c.artist.is_empty() {
                c.title.clone()
            } else {
                format!("{} · {}", c.artist, c.title)
            };
            if ctx_len > 0 && idx >= 0 {
                detail += &format!(" · {}/{}", idx + 1, ctx_len);
            }
            items.push(Row::header("CONTINUE"));
            items.push(Row::entry(c.title.clone(), "resume-current", detail));
        }
        items.push(Row::header("LIBRARY"));
        items.push(Row::entry(
            "LOCAL MUSIC",
            "local",
            format!("{} indexed tracks", self.db.count().unwrap_or(0)),
        ));
        if self.cfg.getbool("navidrome_enabled", true) {
            items.push(Row::entry(
                "NAVIDROME",
                "nav",
                if self.nav_configured() {
                    "Configured"
                } else {
                    "Set up server"
                },
            ));
        }
        if self.cfg.getbool("spotify_enabled", false) {
            items.push(Row::entry(
                "SPOTIFY",
                "spotify",
                if self.spotify_configured() {
                    "Connected"
                } else {
                    "Set up · Premium"
                },
            ));
        }
        items.extend([
            Row::entry("PLAYLISTS", "playlists", "Local cross-source playlists"),
            Row::entry("SMART PLAYLISTS", "smart", "Dynamic local collections"),
            Row::entry(
                "STATISTICS",
                "stats",
                "Listening time · charts & rankings · offline",
            ),
            Row::entry(
                "DISCOVERIES",
                "discoveries",
                "Songs saved from radio/discovery",
            ),
            Row::header("DISCOVER"),
            Row::entry("RADIO", "radiohome", "Live · podcasts · custom stations"),
        ]);
        if self.cfg.getbool("youtube_enabled", true) {
            items.push(Row::entry("YOUTUBE · BETA", "yt", "Audio via yt-dlp"));
        }
        items.extend([
            Row::header("PLAYBACK"),
            Row::entry("QUEUE", "queue", format!("{qlen} items")),
            Row::entry("FAVORITES", "fav", "Across sources"),
            Row::entry("HISTORY", "hist", "Recently played"),
            Row::entry(
                "BOOKMARKS",
                "bookmarks",
                "Saved positions in long-form audio",
            ),
            Row::header("NPLAY"),
            Row::entry("SETTINGS", "settings", "Appearance · sources · library"),
            Row::entry("ABOUT", "about", format!("NPLAY {}", crate::VERSION)),
        ]);
        ui.show_menu("BROWSE", items, true);
    }

    pub fn local_home(&self, ui: &UiRef) {
        let n = self.db.count().unwrap_or(0);
        ui.show_menu(
            "LOCAL MUSIC",
            vec![
                Row::entry("ALL TRACKS", "local:all", format!("{n} indexed tracks")),
                Row::entry("ARTISTS", "local:artists", "A–Z"),
                Row::entry("ALBUMS", "local:albums", "A–Z"),
                Row::entry("FOLDERS", "local:browsefolders", "Browse indexed folders"),
                Row::entry("RECENTLY ADDED", "local:recent", "Newest indexed files"),
                Row::entry(
                    "RANDOM 10 SONGS",
                    "local:random:10",
                    "Fresh shuffled selection",
                ),
                Row::entry(
                    "RANDOM 50 SONGS",
                    "local:random:50",
                    "Fresh shuffled selection",
                ),
                Row::entry(
                    "RANDOM 100 SONGS",
                    "local:random:100",
                    "Fresh shuffled selection",
                ),
                Row::entry(
                    "RANDOM ARTIST · SHUFFLE ALL",
                    "local:randomartistsearch",
                    "Search artist · shuffle all their songs",
                ),
                Row::entry(
                    "RANDOM ARTIST",
                    "local:randomartist",
                    "Pick one artist · albums/tracks in order",
                ),
                Row::entry(
                    "RANDOM ALBUM",
                    "local:randomalbum",
                    "Pick an album and play it in order",
                ),
                Row::entry("SEARCH", "local:search", "Tracks · artists · albums"),
                Row::entry("SCAN CHANGES", "local:scan", "Incremental metadata scan"),
                Row::entry(
                    "FULL RESCAN",
                    "local:fullscan",
                    "Re-read metadata for all files",
                ),
                Row::entry("MUSIC FOLDERS", "local:folders", self.roots().join(" · ")),
            ],
            true,
        );
    }

    pub fn radio_home(&self, ui: &UiRef) {
        let mut items = Vec::new();
        if self.cfg.getbool("sr_enabled", true) {
            items.push(Row::entry(
                "LIVE RADIO",
                "radio",
                "Sveriges Radio · P1 · P2 · P3 · P4 Väst",
            ));
            items.push(Row::entry(
                "PODCASTS & PROGRAMMES",
                "srpod",
                "Sveriges Radio · search · browse · discover",
            ));
        }
        if self.cfg.getbool("custom_radio_enabled", true) {
            let n = self.db.radios().map(|r| r.len()).unwrap_or(0);
            items.push(Row::entry(
                "CUSTOM STATIONS",
                "customradio",
                format!("{n} saved stations"),
            ));
            items.push(Row::entry("ADD STATION", "radio:add", "Name + stream URL"));
        }
        if items.is_empty() {
            items.push(Row::entry(
                "RADIO SOURCES DISABLED",
                "settings:sources",
                "Enable sources in Settings",
            ));
        }
        ui.show_menu("RADIO", items, true);
    }

    /// Scan the configured folders in the background with progress messages.
    pub fn scan_async(&self, ui: &UiRef, force: bool) {
        ui.loading(
            "LOCAL MUSIC",
            "Scanning configured music folders recursively… playback remains available.",
            Vec::new(),
        );
        let u = ui.clone();
        let progress: crate::db::ProgressFn = Arc::new(move |r: &Map<String, Value>| {
            let g = |k: &str| r.get(k).map(crate::model::value_i64).unwrap_or(0);
            if r.get("done").is_some_and(crate::model::value_bool) {
                return;
            }
            let th = crate::app::thousands;
            let mut msg = format!(
                "Scanning · {} audio files · {} new · {} updated",
                th(g("checked")),
                th(g("new")),
                th(g("updated"))
            );
            if let Some(off) = r
                .get("offline")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                msg += &format!(" · {} root offline (retained)", off.len());
            }
            let u2 = u.clone();
            u.post(Box::new(move || u2.status(&msg)));
        });
        let app = self.arc();
        let app2 = self.arc();
        let u = ui.clone();
        self.async_run(
            ui,
            move || app.scan(Some(progress), force),
            move |r| {
                let th = |n: u64| crate::app::thousands(n as i64);
                let mut msg = format!(
                    "Scan · {} checked · {} new · {} updated · {} removed · {} indexed",
                    th(r.checked),
                    th(r.new),
                    th(r.updated),
                    th(r.removed),
                    crate::app::thousands(r.total)
                );
                if !r.offline.is_empty() {
                    msg += &format!(" · {} offline root retained", r.offline.len());
                }
                u.replace_list(
                    "LOCAL MUSIC",
                    track_rows(app2.db.search("", 500).unwrap_or_default()),
                    "No supported audio found",
                    Some(&msg),
                );
            },
            "Library scan failed",
        );
    }

    /// Prompt for a custom station (name, then stream URL) and save it.
    pub fn add_radio(&self, ui: &UiRef) {
        let app = self.arc();
        let u = ui.clone();
        prompt(ui, "STATION NAME › ", "", move |name| {
            let u2 = u.clone();
            prompt(&u, "STREAM URL › ", "", move |url| {
                if name.is_empty() || url.is_empty() {
                    u2.status("Add station cancelled");
                    return;
                }
                match app.db.radio_add(&name, &url, "", "") {
                    Ok(()) => {
                        u2.status(&format!("Station saved · {name}"));
                        app.radio_home(&u2);
                    }
                    Err(e) => u2.error("Invalid station", &e.to_string()),
                }
            });
        });
    }

    pub fn playlists_home(&self, ui: &UiRef) {
        let mut items = entry_rows(self.db.playlists().unwrap_or_default());
        items.push(Row::entry(
            "+ NEW PLAYLIST",
            "playlist:new",
            "Create a local playlist",
        ));
        ui.show_menu("PLAYLISTS", items, true);
    }

    /// `A`: add a track to a local playlist (by name; new names create one).
    pub fn add_selected_to_playlist(&self, t: Track, ui: &UiRef) {
        let pls = self.db.playlists().unwrap_or_default();
        let (label, initial) = match pls.first() {
            None => ("NEW PLAYLIST › ", String::new()),
            Some(p) => ("PLAYLIST NAME › ", p.0.clone()),
        };
        let app = self.arc();
        let u = ui.clone();
        prompt(ui, label, &initial, move |name| {
            if name.is_empty() {
                return;
            }
            if let Ok(Some(pid)) = app.db.playlist_create(&name) {
                let _ = app.db.playlist_add(pid, &t);
                u.status(&format!("Added to playlist · {name}"));
            }
        });
    }

    pub fn spotify_add_playlist(&self, t: Track, ui: &UiRef) {
        if t.source != "spotify" {
            ui.status("Spotify playlists accept Spotify items only");
            return;
        }
        self.set_action_target(Some(t));
        ui.loading(
            "SPOTIFY · ADD TO PLAYLIST",
            "Loading your playlists…",
            Vec::new(),
        );
        let sp = self.spotify.clone();
        let u = ui.clone();
        self.async_run(
            ui,
            move || sp.playlists(),
            move |pls| {
                let mut rows: Vec<Row> = pls
                    .iter()
                    .map(|p| {
                        Row::entry(
                            p.title.clone(),
                            format!("spotify:add:{}", p.id),
                            format!(
                                "{} items",
                                p.meta
                                    .get("item_count")
                                    .map(crate::model::value_i64)
                                    .unwrap_or(0)
                            ),
                        )
                    })
                    .collect();
                if rows.is_empty() {
                    rows.push(Row::entry(
                        "NO PLAYLISTS",
                        "",
                        "No Spotify playlists available",
                    ));
                }
                u.show_menu("SPOTIFY · ADD TO PLAYLIST", rows, true);
            },
            "Spotify playlists unavailable",
        );
    }

    pub fn playlist_remove(&self, pid: i64, index: usize, ui: &UiRef) {
        let _ = self.db.playlist_remove(pid, index as i64);
        ui.replace_list(
            &format!(
                "PLAYLIST · {}",
                self.db.playlist_name(pid).unwrap_or_default()
            ),
            track_rows(self.db.playlist_tracks(pid).unwrap_or_default()),
            "Playlist is empty",
            None,
        );
        ui.status("Removed from playlist");
    }

    /// Move a playlist entry; returns its new index.
    pub fn playlist_move(&self, pid: i64, index: usize, delta: i64, ui: &UiRef) -> usize {
        let j = self
            .db
            .playlist_move(pid, index as i64, delta)
            .unwrap_or(index as i64)
            .max(0) as usize;
        ui.update_items(
            track_rows(self.db.playlist_tracks(pid).unwrap_or_default()),
            j,
        );
        ui.invalidate();
        j
    }

    pub fn playlist_rename(&self, pid: i64, ui: &UiRef) {
        let old = self.db.playlist_name(pid).unwrap_or_default();
        let app = self.arc();
        let u = ui.clone();
        prompt(ui, "RENAME PLAYLIST › ", &old.clone(), move |name| {
            if !name.is_empty() && name != old {
                let _ = app.db.playlist_rename(pid, &name);
                u.set_title(&format!("PLAYLIST · {name}"));
                u.status("Playlist renamed");
            }
        });
    }

    pub fn playlist_delete(&self, pid: i64, ui: &UiRef) {
        let name = self.db.playlist_name(pid).unwrap_or_default();
        let app = self.arc();
        let u = ui.clone();
        prompt(
            ui,
            &format!("DELETE {name}? type YES › "),
            "",
            move |answer| {
                if answer == "YES" {
                    let _ = app.db.playlist_delete(pid);
                    app.state().current_playlist_pid = None;
                    app.playlists_home(&u);
                    u.status("Playlist deleted");
                } else {
                    u.status("Delete cancelled");
                }
            },
        );
    }

    pub fn source_switch_menu(&self, t: Track, ui: &UiRef) {
        let alts: Vec<Track> = t
            .meta
            .get("source_alternatives")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|x| x.is_object())
                    .filter_map(Track::from_value)
                    .collect()
            })
            .unwrap_or_default();
        if alts.is_empty() {
            ui.status("No matched alternate sources for this track");
            return;
        }
        {
            let mut st = self.state();
            st.source_switch_tracks = alts.iter().map(|x| (x.source.clone(), x.clone())).collect();
            st.action_target = Some(t.clone());
        }
        let label = |s: &str| match s {
            "local" => "Local".to_string(),
            "navidrome" => "Navidrome".into(),
            "spotify" => "Spotify".into(),
            "youtube" => "YouTube".into(),
            other => crate::app::capitalize(other),
        };
        let rows = alts
            .iter()
            .map(|x| {
                Row::entry(
                    label(&x.source),
                    format!("source-switch:{}", x.source),
                    if x.source == t.source {
                        "current"
                    } else {
                        "continue near current position"
                    },
                )
            })
            .collect();
        ui.show_menu(&format!("SWITCH SOURCE · {}", t.title), rows, true);
    }

    /// Continue the same song from another source near the same position.
    pub fn source_handoff(&self, t: Track, ui: &UiRef) {
        let pos = self.playback_position();
        let rp = if t.seekable { pos } else { 0.0 };
        self.play_with(t, Some(ui), true, rp, false);
    }

    /// Run a menu action id.
    pub fn open_source(&self, s: &str, ui: &UiRef) {
        let u = Some(ui);
        match s {
            "stats:sync-toggle" => return self.statistics_sync_toggle(ui),
            "stats:sync-now" => return self.statistics_sync_now(ui),
            "stats:toggle" => return self.statistics_toggle(ui),
            "stats:overview" => {
                let p = self.state().stats_period.clone();
                return self.stats_home(ui, &p, false);
            }
            "stats:settings" => return self.stats_settings(ui, true),
            "stats:artists" | "stats:albums" | "stats:tracks" | "stats:sources" => {
                return self.stats_details(ui, &s[6..], None, true)
            }
            "stats:copy-id" => {
                let id = self.statistics.installation_id().unwrap_or_default();
                match copy_to_clipboard(&id) {
                    None => ui
                        .status("Clipboard tool unavailable · install wl-clipboard, xclip or xsel"),
                    Some(true) => ui.status("Installation ID copied"),
                    Some(false) => ui.status("Unable to copy installation ID"),
                }
                return;
            }
            "resume-current" => return self.play_pause(u),
            "spotify:open-web" => {
                crate::spotify::browser::open_authorization_url("https://open.spotify.com/");
                ui.status("Spotify Web Player opened · activate playback, then refresh Devices");
                return;
            }
            "spotify:devices-refresh" => return self.spotify_devices(ui),
            _ => {}
        }
        if let Some(p) = s.strip_prefix("stats:period:") {
            return self.stats_home(ui, p, false);
        }
        if let Some(src) = s.strip_prefix("source-switch:") {
            let t = self.state().source_switch_tracks.get(src).cloned();
            if let Some(t) = t {
                self.source_handoff(t, ui);
            }
            return;
        }
        if let Some(pid) = s.strip_prefix("spotify:add:") {
            let t = self.state().action_target.clone();
            if let Some(t) = t {
                let sp = self.spotify.clone();
                let pid = pid.to_string();
                let u2 = ui.clone();
                self.async_run(
                    ui,
                    move || sp.add_to_playlist(&pid, &t),
                    move |_| u2.status("Added to Spotify playlist"),
                    "Spotify playlist update failed",
                );
            }
            return;
        }
        if let Some(did) = s.strip_prefix("spotify:device:") {
            let did = did.to_string();
            let name = self
                .state()
                .spotify_device_names
                .get(&did)
                .cloned()
                .unwrap_or_else(|| "Spotify device".into());
            let sp = self.spotify.clone();
            let app = self.arc();
            let u2 = ui.clone();
            let d2 = did.clone();
            self.async_run(
                ui,
                move || sp.transfer(&d2, false),
                move |_| {
                    app.cfg.set("spotify_device_id", &did);
                    app.cfg.set("spotify_device_name", &name);
                    u2.status(&format!("Spotify device · {name}"));
                    app.open_spotify_home(&u2);
                },
                "Device transfer failed",
            );
            return;
        }
        if let Some(kind) = s.strip_prefix("spotify:searchall:") {
            let items = self
                .state()
                .spotify_search_groups
                .get(kind)
                .cloned()
                .unwrap_or_default();
            let label = match kind {
                "artist" => "ARTISTS".to_string(),
                "album" => "ALBUMS".into(),
                "track" => "TRACKS".into(),
                "playlist" => "PLAYLISTS".into(),
                k => k.to_uppercase(),
            };
            show(
                ui,
                &format!("SPOTIFY · {label}"),
                items,
                "No Spotify matches",
            );
            return;
        }
        if let Some(action) = s.strip_prefix("ctx:") {
            return self.context_action(action, ui);
        }
        let db = &self.db;
        match s {
            "local" => self.local_home(ui),
            "local:all" => show(
                ui,
                "LOCAL MUSIC",
                db.search("", 500).unwrap_or_default(),
                &format!(
                    "No indexed music · folder: {} · press u to scan",
                    self.roots().join(" · ")
                ),
            ),
            "local:randomartistsearch" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "ARTIST › ", "", move |q| {
                    if q.is_empty() {
                        return;
                    }
                    let hits = app.db.find_artists(&q, 50).unwrap_or_default();
                    if hits.len() == 1 {
                        let name = &hits[0].0;
                        show(
                            &u2,
                            &format!("LOCAL MUSIC · {name} · SHUFFLED"),
                            app.db.shuffled_artist_tracks(name).unwrap_or_default(),
                            "No items",
                        );
                    } else if !hits.is_empty() {
                        let rows = hits
                            .iter()
                            .map(|(name, n)| {
                                Row::entry(
                                    name.clone(),
                                    format!("local:randomartisttracks:{name}"),
                                    format!("{n} tracks · shuffle all"),
                                )
                            })
                            .collect();
                        u2.show_menu("LOCAL MUSIC · CHOOSE ARTIST", rows, true);
                    } else {
                        u2.status("No matching local artist");
                    }
                });
            }
            "local:randomartist" => {
                let (name, xs) = db.random_artist_tracks().unwrap_or_default();
                show(
                    ui,
                    &format!("LOCAL MUSIC · RANDOM ARTIST · {name}"),
                    xs,
                    "No indexed artists · press u to scan",
                );
            }
            "local:recent" => show(
                ui,
                "LOCAL MUSIC · RECENTLY ADDED",
                db.recently_added(100).unwrap_or_default(),
                "No indexed music · press u to scan",
            ),
            "local:randomalbum" => show(
                ui,
                "LOCAL MUSIC · RANDOM ALBUM",
                db.random_album_tracks().unwrap_or_default(),
                "No indexed albums · press u to scan",
            ),
            "local:search" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "LOCAL MUSIC SEARCH › ", "", move |q| {
                    if !q.is_empty() {
                        show(
                            &u2,
                            &format!("LOCAL MUSIC · SEARCH · {q}"),
                            app.db.search(&q, 200).unwrap_or_default(),
                            "No local matches",
                        );
                    }
                });
            }
            "local:artists" => ui.show_menu(
                "LOCAL MUSIC · ARTISTS",
                entry_rows(db.local_artists().unwrap_or_default()),
                true,
            ),
            "local:albums" => ui.show_menu(
                "LOCAL MUSIC · ALBUMS",
                entry_rows(db.local_albums().unwrap_or_default()),
                true,
            ),
            "local:browsefolders" => ui.show_menu(
                "LOCAL MUSIC · FOLDERS",
                entry_rows(db.local_folders().unwrap_or_default()),
                true,
            ),
            "local:scan" => self.scan_async(ui, false),
            "local:fullscan" => self.scan_async(ui, true),
            "local:folders" => {
                let def = crate::paths::home_dir()
                    .join("Music")
                    .to_string_lossy()
                    .to_string();
                let cfg = self.cfg.clone();
                let u2 = ui.clone();
                prompt(
                    ui,
                    "MUSIC FOLDERS (: separated) › ",
                    &self.cfg.get("music_dirs", &def),
                    move |v| {
                        if !v.is_empty() {
                            cfg.set("music_dirs", &v);
                            u2.status("Music folders updated · press u to scan");
                        }
                    },
                );
            }
            "radiohome" => self.radio_home(ui),
            "radio" => {
                if !self.cfg.getbool("sr_enabled", true) {
                    return ui.status("Sveriges Radio is disabled · Settings → Sources");
                }
                let xs = self.sr.live();
                ui.show_list(
                    "SVERIGES RADIO · LIVE",
                    track_rows(xs.clone()),
                    "No items",
                    Some("Fetching current programmes…"),
                    true,
                );
                self.refresh_radio_async(xs, u);
            }
            "customradio" => {
                if !self.cfg.getbool("custom_radio_enabled", true) {
                    return ui.status("Custom radio is disabled · Settings → Sources");
                }
                show(
                    ui,
                    "RADIO · CUSTOM STATIONS",
                    db.radios().unwrap_or_default(),
                    "No custom stations · choose Add Station in Radio",
                );
            }
            "radio:add" => self.add_radio(ui),
            "radio:toggle-sr" => {
                let enabled = !self.cfg.getbool("sr_enabled", true);
                self.cfg
                    .set("sr_enabled", if enabled { "true" } else { "false" });
                ui.status(&format!(
                    "Sveriges Radio {}",
                    if enabled { "enabled" } else { "disabled" }
                ));
                self.open_source("settings:sources", ui);
            }
            "playlists" => self.playlists_home(ui),
            "settings" => crate::services::settings::open(self, s, ui),
            "playlist:new" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "NEW PLAYLIST › ", "", move |name| {
                    if !name.is_empty() {
                        let _ = app.db.playlist_create(&name);
                        app.playlists_home(&u2);
                    }
                });
            }
            "srpod" => {
                if !self.cfg.getbool("sr_enabled", true) {
                    return ui.status("Sveriges Radio is disabled · Settings → Sources");
                }
                ui.show_menu(
                    "SR · PODCASTS & PROGRAMMES",
                    vec![
                        Row::entry("SEARCH SR", "srsearch", "Programmes + episodes together"),
                        Row::entry(
                            "PROGRAMMES A–Ö",
                            "srprograms",
                            "Browse the complete programme catalogue",
                        ),
                        Row::entry(
                            "SEARCH PROGRAMMES",
                            "srprogsearch",
                            "Programme titles and descriptions",
                        ),
                        Row::entry("SEARCH EPISODES", "srepisearch", "Individual episodes"),
                        Row::entry(
                            "DOCUMENTARY",
                            "srcat:documentary",
                            "Documentaries and related programmes",
                        ),
                        Row::entry(
                            "HISTORY",
                            "srcat:history",
                            "History programmes and episodes",
                        ),
                        Row::entry(
                            "NEWS & SOCIETY",
                            "srcat:news",
                            "News, society and current affairs",
                        ),
                        Row::entry("MUSIC", "srcat:music", "Music programmes and episodes"),
                        Row::entry("RECENTLY PLAYED", "hist", "Your NPLAY history"),
                    ],
                    true,
                );
            }
            "srsearch" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "SEARCH SVERIGES RADIO › ", "", move |q| {
                    app.search_sr_all(&q, &u2)
                });
            }
            "srprogsearch" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "SR PROGRAMME SEARCH › ", "", move |q| {
                    app.search_sr_programs(&q, &u2)
                });
            }
            "srepisearch" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "SR EPISODE SEARCH › ", "", move |q| {
                    app.search_provider("sr", &q, &u2)
                });
            }
            "srprograms" => self.load_sr_programs("", ui),
            "smart" => self.smart_home(ui),
            "smart:recent" => show(
                ui,
                "SMART · RECENTLY ADDED",
                db.recently_added(200).unwrap_or_default(),
                "No items",
            ),
            "smart:most" => show(
                ui,
                "SMART · MOST PLAYED",
                db.most_played(200).unwrap_or_default(),
                "No listening history yet",
            ),
            "smart:never" => show(
                ui,
                "SMART · NEVER PLAYED",
                db.never_played(200).unwrap_or_default(),
                "Everything has been played",
            ),
            "smart:albums" => ui.show_menu(
                "SMART · UNPLAYED ALBUMS",
                entry_rows(db.unplayed_albums(200).unwrap_or_default()),
                true,
            ),
            "smart:rated" => show(
                ui,
                "SMART · HIGHLY RATED",
                db.rated(4, 200).unwrap_or_default(),
                "No 4–5 star local tracks",
            ),
            "stats" => self.stats_home(ui, "week", true),
            "discoveries" => show(
                ui,
                "DISCOVERIES",
                db.discoveries(300).unwrap_or_default(),
                "No discoveries yet · save a radio song from Actions",
            ),
            "fav" => show(
                ui,
                "FAVORITES",
                db.favorites().unwrap_or_default(),
                "No favorites yet",
            ),
            "hist" => show(
                ui,
                "HISTORY",
                db.history(200).unwrap_or_default(),
                "Nothing played yet",
            ),
            "bookmarks" => show(
                ui,
                "BOOKMARKS",
                db.bookmarks(200).unwrap_or_default(),
                "No bookmarks yet · use Actions on long-form audio",
            ),
            "queue" => show(ui, "QUEUE", self.queue(), "Queue is empty"),
            "nav" => {
                if !self.cfg.getbool("navidrome_enabled", true) {
                    return ui.status("Navidrome is disabled · Settings → Sources");
                }
                if self.nav().is_some() {
                    self.open_nav_home(ui)
                } else {
                    self.nav_setup(ui)
                }
            }
            "yt" => {
                if !self.cfg.getbool("youtube_enabled", true) {
                    return ui.status("YouTube is disabled · Settings → Sources");
                }
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "YOUTUBE SEARCH › ", "", move |q| {
                    app.search_provider("yt", &q, &u2)
                });
            }
            "spotify" => self.open_spotify_home(ui),
            "spotify:setup" => self.spotify_setup(ui),
            "spotify:search" => {
                let app = self.arc();
                let u2 = ui.clone();
                prompt(ui, "SPOTIFY SEARCH › ", "", move |q| {
                    app.spotify_search(&q, &u2)
                });
            }
            "spotify:playlists" => self.spotify_load("SPOTIFY · PLAYLISTS", Spotify::playlists, ui),
            "spotify:recent" => self.spotify_load("SPOTIFY · RECENTLY PLAYED", Spotify::recent, ui),
            "spotify:toptracks" => {
                self.spotify_load("SPOTIFY · TOP TRACKS", Spotify::top_tracks, ui)
            }
            "spotify:topartists" => {
                self.spotify_load("SPOTIFY · TOP ARTISTS", Spotify::top_artists, ui)
            }
            "spotify:local" => self.spotify_local_menu(ui),
            "spotify:local-start" => {
                ui.loading(
                    "SPOTIFY · LOCAL PLAYBACK",
                    "Starting NPLAY Spotify engine without browser authorization…",
                    Vec::new(),
                );
                let sp = self.spotify.clone();
                let app = self.arc();
                let u2 = ui.clone();
                self.async_run(
                    ui,
                    move || sp.local.start(false),
                    move |_| {
                        u2.status("Local Spotify engine started · NPLAY will use this computer");
                        app.cfg.set("spotify_playback", "local");
                        app.open_spotify_home(&u2);
                    },
                    "Local Spotify engine failed",
                );
            }
            "spotify:local-authorize" => {
                ui.loading(
                    "SPOTIFY · LOCAL PLAYBACK",
                    "Starting explicit librespot authorization · browser may open…",
                    Vec::new(),
                );
                let app = self.arc();
                let app2 = self.arc();
                let u1 = ui.clone();
                let u2 = ui.clone();
                self.async_run(
                    ui,
                    move || {
                        // Same flow as the service's spotify.local_authorize.
                        let on_url = move |_url: &str| {
                            let u3 = u1.clone();
                            u1.post(Box::new(move || {
                                u3.status("Spotify authorization opened in your browser · complete it there")
                            }));
                        };
                        let d = app.spotify.authorize_local(180.0, Some(&on_url), 30.0)?;
                        if !d.is_empty() {
                            app.state().spotify_state.device = d.clone();
                        }
                        Ok(d)
                    },
                    move |_| {
                        u2.status("Local Spotify authorized · credentials cached for future starts");
                        app2.cfg.set("spotify_playback", "local");
                        app2.open_spotify_home(&u2);
                    },
                    "Local Spotify authorization failed",
                );
            }
            "spotify:local-stop" => {
                self.spotify.local.stop(false);
                ui.status("Local Spotify engine stopped");
                self.spotify_local_menu(ui);
            }
            "spotify:mode-local" => {
                self.cfg.set("spotify_playback", "local");
                ui.status("Spotify playback · this computer");
                self.open_spotify_home(ui);
            }
            "spotify:mode-connect" => {
                self.cfg.set("spotify_playback", "connect");
                ui.status("Spotify playback · external Connect device");
                self.spotify_devices(ui);
            }
            "spotify:local-diag" => self.spotify_local_diagnostics(ui),
            "spotify:devices" => self.spotify_devices(ui),
            "spotify:disconnect" => {
                self.spotify.disconnect();
                ui.status("Spotify disconnected");
                self.open_source("settings:sources", ui);
            }
            "about" => ui.show_about(),
            _ => {
                if let Some(n) = s.strip_prefix("local:random:") {
                    let limit = n.parse::<i64>().unwrap_or(50);
                    show(
                        ui,
                        &format!("LOCAL MUSIC · RANDOM {limit}"),
                        db.random_tracks(limit).unwrap_or_default(),
                        "No indexed music · press u to scan",
                    );
                } else if let Some(name) = s.strip_prefix("local:randomartisttracks:") {
                    show(
                        ui,
                        &format!("LOCAL MUSIC · {name} · SHUFFLED"),
                        db.shuffled_artist_tracks(name).unwrap_or_default(),
                        "No items",
                    );
                } else if let Some(name) = s.strip_prefix("local:artist:") {
                    show(
                        ui,
                        &format!("LOCAL MUSIC · {name}"),
                        db.tracks_by_artist(name).unwrap_or_default(),
                        "No items",
                    );
                } else if let Some(rest) = s.strip_prefix("local:album:") {
                    let (artist, album) = rest.split_once('\t').unwrap_or(("", rest));
                    show(
                        ui,
                        &format!("LOCAL MUSIC · {album}"),
                        db.tracks_by_album(artist, album).unwrap_or_default(),
                        "No items",
                    );
                } else if let Some(path) = s.strip_prefix("local:folder:") {
                    let name = Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    show(
                        ui,
                        &format!("LOCAL MUSIC · {name}"),
                        db.tracks_by_folder(path).unwrap_or_default(),
                        "No items",
                    );
                } else if let Some(style) = s.strip_prefix("visual-style:") {
                    ui.set_visual_style(style);
                    self.cfg.set("visualizer_style", style);
                    ui.status(&format!("CAVA style · {}", crate::app::capitalize(style)));
                    ui.visualizer_style_menu();
                } else if s.starts_with("settings:") {
                    crate::services::settings::open(self, s, ui);
                } else if let Some(pid) = s.strip_prefix("playlist:") {
                    if let Ok(pid) = pid.parse::<i64>() {
                        self.state().current_playlist_pid = Some(pid);
                        show(
                            ui,
                            &format!("PLAYLIST · {}", db.playlist_name(pid).unwrap_or_default()),
                            db.playlist_tracks(pid).unwrap_or_default(),
                            "Playlist is empty",
                        );
                    }
                } else if let Some(cat) = s.strip_prefix("srcat:") {
                    let term = match cat {
                        "documentary" => "dokumentär",
                        "history" => "historia",
                        "news" => "nyheter",
                        "music" => "musik",
                        other => other,
                    };
                    self.search_sr_all(term, ui);
                }
            }
        }
    }

    /// `ctx:*` actions on the Actions menu target (or the current track).
    fn context_action(&self, action: &str, ui: &UiRef) {
        let u = Some(ui);
        let target = self.state().action_target.clone();
        let Some(t) = target.or_else(|| self.current()) else {
            ui.status("Actions · no target");
            return;
        };
        match action {
            "play" => self.open_item(t, ui),
            "next" => self.queue_next(t, u),
            "queue" => self.queue_add(t, u),
            "after-context" => self.queue_after_context(t, u),
            "playlist" => self.add_selected_to_playlist(t, ui),
            "info" => ui.show_info(&t),
            "artist" => self.nav_goto_artist(t, ui),
            "album" => self.nav_goto_album(t, ui),
            "spotify-save" => {
                let sp = self.spotify.clone();
                let u2 = ui.clone();
                self.async_run(
                    ui,
                    move || sp.save(&t),
                    move |_| u2.status("Saved to Spotify library"),
                    "Spotify save failed",
                );
            }
            "spotify-playlist" => self.spotify_add_playlist(t, ui),
            "discover" => self.save_discovery(u),
            "switch-source" => self.source_switch_menu(t, ui),
            "bookmark" => {
                let pos = if self.current().is_some_and(|c| c.key() == t.key()) {
                    self.playback_position()
                } else {
                    self.db.resume_get(&t.key(), Some(&t.id)).unwrap_or(0.0)
                };
                let _ = self.db.bookmark_add(&t, pos);
                ui.status(&format!("Bookmark saved · {}", self.fmt(pos)));
            }
            "nplay-radio" => {
                let title = format!("NPLAY RADIO · {}", t.title);
                ui.loading(&title, "Building a cross-source mix…", Vec::new());
                let engine = crate::services::radio_engine(&self.arc());
                let u2 = ui.clone();
                self.async_run(
                    ui,
                    move || Ok(engine.build(&t, 60)),
                    move |xs| {
                        let n = xs.len();
                        replace(
                            &u2,
                            &title,
                            xs,
                            "Not enough matching music across enabled sources",
                        );
                        u2.status(&format!("NPLAY Radio · {n} tracks"));
                    },
                    "NPLAY Radio unavailable",
                );
            }
            a if a.starts_with("rate") => {
                let r = a.chars().last().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
                self.set_rating(Some(&t), r, u);
            }
            _ => {}
        }
    }

    pub fn open_spotify_home(&self, ui: &UiRef) {
        if !self.cfg.getbool("spotify_enabled", false) {
            ui.status("Spotify is disabled · Settings → Sources");
            return;
        }
        if !self.spotify_configured() {
            return self.spotify_setup(ui);
        }
        let dev = self.cfg.get("spotify_device_name", "");
        ui.show_menu(
            "SPOTIFY",
            vec![
                Row::entry(
                    "SEARCH",
                    "spotify:search",
                    "Artists · albums · tracks · playlists",
                ),
                Row::entry("PLAYLISTS", "spotify:playlists", "Your Spotify playlists"),
                Row::entry(
                    "RECENTLY PLAYED",
                    "spotify:recent",
                    "Spotify listening history",
                ),
                Row::entry(
                    "TOP TRACKS",
                    "spotify:toptracks",
                    "Your medium-term favorites",
                ),
                Row::entry(
                    "TOP ARTISTS",
                    "spotify:topartists",
                    "Your medium-term favorites",
                ),
                Row::entry(
                    "LOCAL PLAYBACK",
                    "spotify:local",
                    format!(
                        "This computer · {}",
                        if self.spotify.local.available() {
                            "ready"
                        } else {
                            "librespot required"
                        }
                    ),
                ),
                Row::entry(
                    "DEVICES",
                    "spotify:devices",
                    if dev.is_empty() {
                        "Optional external Connect device".to_string()
                    } else {
                        format!("Preferred · {dev}")
                    },
                ),
                Row::entry(
                    "DISCONNECT",
                    "spotify:disconnect",
                    "Remove local authorization tokens",
                ),
            ],
            true,
        );
    }

    /// Ask for the client id and run the PKCE browser authorization.
    pub fn spotify_setup(&self, ui: &UiRef) {
        let app = self.arc();
        let u = ui.clone();
        prompt(
            ui,
            "SPOTIFY CLIENT ID › ",
            &self.cfg.get("spotify_client_id", ""),
            move |cid| {
                if cid.is_empty() {
                    u.status("Spotify setup cancelled");
                    return;
                }
                app.cfg.set("spotify_client_id", cid.trim());
                app.cfg.set("spotify_enabled", "true");
                u.loading(
                    "SPOTIFY · CONNECT",
                    "Opening browser · authorize NPLAY · callback 127.0.0.1:43821…",
                    Vec::new(),
                );
                let sp = app.spotify.clone();
                let app2 = app.clone();
                let u2 = u.clone();
                app.async_run(
                    &u,
                    move || sp.connect(true, 180.0),
                    move |p| {
                        let who = ["display_name", "id"]
                            .iter()
                            .map(|k| p.get(*k).map(crate::model::value_str).unwrap_or_default())
                            .find(|s| !s.is_empty())
                            .unwrap_or_else(|| "account".into());
                        u2.status(&format!("Spotify connected · {who}"));
                        app2.open_spotify_home(&u2);
                    },
                    "Spotify connection failed",
                );
            },
        );
    }

    pub fn spotify_load(&self, title: &str, work: fn(&Spotify) -> Result<Vec<Track>>, ui: &UiRef) {
        if !self.spotify_configured() {
            return self.spotify_setup(ui);
        }
        ui.push();
        ui.loading(title, "Loading from Spotify…", Vec::new());
        let sp = self.spotify.clone();
        let u = ui.clone();
        let title = title.to_string();
        self.async_run(
            ui,
            move || work(&sp),
            move |xs| replace(&u, &title, xs, "No Spotify items found"),
            "Spotify unavailable",
        );
    }

    pub fn spotify_search(&self, q: &str, ui: &UiRef) {
        if q.is_empty() {
            return;
        }
        ui.push();
        ui.loading("SPOTIFY · SEARCH", &format!("Searching “{q}”…"), Vec::new());
        let sp = self.spotify.clone();
        let app = self.arc();
        let u = ui.clone();
        let (q1, q2) = (q.to_string(), q.to_string());
        self.async_run(
            ui,
            move || sp.search(&q1, 10),
            move |xs: Vec<Track>| {
                // Fair-share the first screen: no single result type starves
                // the others. Full groups stay one Enter away.
                let mut groups: HashMap<String, Vec<Track>> = HashMap::new();
                let mut grouped: Vec<Row> = Vec::new();
                for (kind, label, cap) in [
                    ("artist", "ARTISTS", 5),
                    ("album", "ALBUMS", 6),
                    ("track", "TRACKS", 8),
                    ("playlist", "PLAYLISTS", 4),
                ] {
                    let part: Vec<Track> = xs.iter().filter(|x| x.kind == kind).cloned().collect();
                    groups.insert(kind.to_string(), part.clone());
                    if part.is_empty() {
                        continue;
                    }
                    grouped.push(Row::header(label));
                    grouped.extend(part.iter().take(cap).cloned().map(Row::Track));
                    if part.len() > cap {
                        grouped.push(Row::entry(
                            format!("SHOW ALL {label} ({})", part.len()),
                            format!("spotify:searchall:{kind}"),
                            format!("{} more", part.len() - cap),
                        ));
                    }
                }
                app.state().spotify_search_groups = groups;
                u.replace_list(
                    &format!("SPOTIFY · {q2} · {} RESULTS", xs.len()),
                    grouped,
                    "No Spotify matches",
                    None,
                );
            },
            "Spotify search unavailable",
        );
    }

    pub fn spotify_local_menu(&self, ui: &UiRef) {
        let e = &self.spotify.local;
        let mode = self.cfg.get("spotify_playback", "local");
        let state = if e.running() {
            "RUNNING"
        } else if e.available() {
            "READY"
        } else {
            "NOT INSTALLED"
        };
        let auth = if e.authorized() {
            "authorized"
        } else {
            "NOT AUTHORIZED · choose AUTHORIZE"
        };
        ui.show_menu(
            "SPOTIFY · LOCAL PLAYBACK",
            vec![
                Row::entry(
                    "STATUS",
                    "",
                    format!(
                        "{state} · {} · {auth}",
                        if mode == "local" {
                            "selected"
                        } else {
                            "not selected"
                        }
                    ),
                ),
                Row::entry(
                    "USE THIS COMPUTER",
                    "spotify:mode-local",
                    "Default · NPLAY manages local Spotify audio",
                ),
                Row::entry(
                    "START LOCAL ENGINE",
                    "spotify:local-start",
                    "Start librespot silently · never opens browser",
                ),
                Row::entry(
                    "AUTHORIZE / REAUTHORIZE",
                    "spotify:local-authorize",
                    "Explicit OAuth only when local playback actually needs it",
                ),
                Row::entry(
                    "STOP LOCAL ENGINE",
                    "spotify:local-stop",
                    "Stop local Spotify receiver",
                ),
                Row::entry(
                    "USE EXTERNAL CONNECT",
                    "spotify:mode-connect",
                    "Phone · speaker · another Spotify device",
                ),
                Row::entry(
                    "DIAGNOSTICS",
                    "spotify:local-diag",
                    "Engine · backend · cache · recent log",
                ),
            ],
            true,
        );
    }

    pub fn spotify_local_diagnostics(&self, ui: &UiRef) {
        let e = &self.spotify.local;
        let c = &self.cfg;
        let v = e.version();
        let mut rows = vec![
            Row::entry(
                "LIBRESPOT",
                "",
                if v.is_empty() {
                    "not installed".into()
                } else {
                    v
                },
            ),
            Row::entry(
                "ENGINE",
                "",
                if e.running() { "running" } else { "stopped" },
            ),
            Row::entry("BACKEND", "", c.get("spotify_local_backend", "pulseaudio")),
            Row::entry(
                "BITRATE",
                "",
                format!("{} kbps", c.get("spotify_local_bitrate", "320")),
            ),
            Row::entry("DEVICE NAME", "", c.get("spotify_local_name", "NPLAY")),
            Row::entry("PLAYBACK MODE", "", c.get("spotify_playback", "local")),
            Row::entry("CACHE", "", e.system_cache().display().to_string()),
            Row::entry("ENGINE LOG", "", e.log_path().display().to_string()),
            Row::entry("CACHED FILES", "", e.credential_files().len().to_string()),
            Row::entry(
                "AUTHORIZED",
                "",
                if e.authorized() {
                    "yes · credentials cached"
                } else {
                    "no · choose AUTHORIZE"
                },
            ),
            Row::entry(
                "RECEIVER PID",
                "",
                format!(
                    "{}{}",
                    e.pid().map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                    if e.running() && !e.owned() {
                        " (shared with another NPLAY)"
                    } else {
                        ""
                    }
                ),
            ),
            Row::header("RECENT ENGINE LOG"),
        ];
        let tail = e.log_tail(40);
        let lines: Vec<String> = if tail.trim().is_empty() {
            vec!["No local engine log yet.".into()]
        } else {
            tail.lines().map(str::to_string).collect()
        };
        for line in lines {
            let head: String = line.chars().take(34).collect();
            let rest: String = line.chars().skip(34).collect();
            rows.push(Row::entry(head, "", rest));
        }
        ui.show_menu("SPOTIFY · LOCAL DIAGNOSTICS", rows, true);
    }

    pub fn spotify_devices(&self, ui: &UiRef) {
        ui.push();
        ui.loading(
            "SPOTIFY · DEVICES",
            "Finding Spotify Connect devices…",
            Vec::new(),
        );
        let sp = self.spotify.clone();
        let app = self.arc();
        let u = ui.clone();
        self.async_run(
            ui,
            move || sp.devices(),
            move |ds| {
                let preferred = app.cfg.get("spotify_device_id", "");
                let s = |d: &crate::model::Meta, k: &str| {
                    d.get(k).map(crate::model::value_str).unwrap_or_default()
                };
                let b = |d: &crate::model::Meta, k: &str| {
                    d.get(k).is_some_and(crate::model::value_bool)
                };
                app.state().spotify_device_names = ds
                    .iter()
                    .filter(|d| !s(d, "id").is_empty())
                    .map(|d| {
                        let n = s(d, "name");
                        (
                            s(d, "id"),
                            if n.is_empty() {
                                "Spotify device".into()
                            } else {
                                n
                            },
                        )
                    })
                    .collect();
                let mut items = Vec::new();
                for d in &ds {
                    let id = s(d, "id");
                    if id.is_empty() {
                        continue;
                    }
                    let active = b(d, "is_active");
                    let mark = if active {
                        "● "
                    } else if id == preferred {
                        "◆ "
                    } else {
                        ""
                    };
                    let ty = s(d, "type");
                    let vol = match d.get("volume_percent") {
                        Some(v) if !v.is_null() => crate::model::value_str(v),
                        _ => "?".into(),
                    };
                    let mut detail = format!(
                        "{} · {vol}%",
                        if ty.is_empty() { "Device".into() } else { ty }
                    );
                    if active {
                        detail += " · ACTIVE";
                    } else if id == preferred {
                        detail += " · PREFERRED";
                    }
                    let name = s(d, "name");
                    items.push(Row::entry(
                        format!(
                            "{mark}{}",
                            if name.is_empty() {
                                "Spotify device".into()
                            } else {
                                name
                            }
                        ),
                        format!("spotify:device:{id}"),
                        detail,
                    ));
                }
                items.push(Row::header("ACTIONS"));
                items.push(Row::entry(
                    "REFRESH DEVICES",
                    "spotify:devices-refresh",
                    "Rescan Spotify Connect",
                ));
                items.push(Row::entry(
                    "OPEN SPOTIFY WEB PLAYER",
                    "spotify:open-web",
                    "Use when no Connect device is available",
                ));
                u.show_menu("SPOTIFY · DEVICES", items, true);
            },
            "Spotify devices unavailable",
        );
    }

    pub fn open_nav_home(&self, ui: &UiRef) {
        ui.show_menu(
            "NAVIDROME",
            vec![
                Row::entry("RECENTLY ADDED", "nav:newest", "Albums"),
                Row::entry(
                    "RANDOM 10 SONGS",
                    "nav:randomtracks:10",
                    "Fresh server selection",
                ),
                Row::entry(
                    "RANDOM 50 SONGS",
                    "nav:randomtracks:50",
                    "Fresh server selection",
                ),
                Row::entry(
                    "RANDOM 100 SONGS",
                    "nav:randomtracks:100",
                    "Fresh server selection",
                ),
                Row::entry(
                    "RANDOM ARTIST · SHUFFLE ALL",
                    "nav:randomartistsearch",
                    "Search artist · shuffle all their songs",
                ),
                Row::entry(
                    "RANDOM ARTIST",
                    "nav:randomartist",
                    "Pick one artist · albums/tracks in order",
                ),
                Row::entry("RANDOM ALBUMS", "nav:random", "Albums"),
                Row::entry("ARTISTS", "nav:artists", "A–Z"),
                Row::entry("ALBUMS", "nav:albums", "A–Z"),
                Row::entry("PLAYLISTS", "nav:playlists", "Server playlists"),
                Row::entry("SEARCH", "nav:search", "Artists · albums · songs"),
                Row::entry("SETUP SERVER", "nav:setup", "Change connection"),
            ],
            true,
        );
    }

    /// Prompt for URL, user and password, then test and save (1.x `UI.nav_setup`).
    pub fn nav_setup(&self, ui: &UiRef) {
        let app = self.arc();
        let u = ui.clone();
        let initial = self.cfg.get("navidrome_url", "https://");
        prompt(ui, "NAVIDROME URL › ", &initial, move |url| {
            if url.is_empty() {
                return u.status("Navidrome setup cancelled");
            }
            let url = if url.starts_with("http://") || url.starts_with("https://") {
                url
            } else {
                format!("https://{url}")
            };
            let u2 = u.clone();
            let user0 = app.cfg.get("navidrome_user", "");
            prompt(&u, "USERNAME › ", &user0, move |user| {
                if user.is_empty() {
                    return u2.status("Navidrome setup cancelled");
                }
                let u3 = u2.clone();
                u2.prompt(
                    "PASSWORD › ",
                    "",
                    true,
                    Box::new(move |pw| {
                        let pw = pw.unwrap_or_default();
                        if pw.is_empty() {
                            return u3.status("Navidrome setup cancelled");
                        }
                        u3.loading("NAVIDROME · SETUP", "Testing connection…", Vec::new());
                        app.test_and_save_nav(&url, &user, &pw, &u3);
                    }),
                );
            });
        });
    }

    /// `nav:*` actions.
    pub fn nav_action(&self, action: &str, ui: &UiRef) {
        ui.push();
        let Some(n) = self.nav() else {
            return self.nav_setup(ui);
        };
        if action == "setup" {
            return self.nav_setup(ui);
        }
        if action == "search" {
            let app = self.arc();
            let u = ui.clone();
            return prompt(ui, "NAVIDROME SEARCH › ", "", move |q| {
                app.search_provider("nav", &q, &u)
            });
        }
        ui.loading("NAVIDROME", "Loading…", Vec::new());
        let u = ui.clone();
        if let Some(lim) = action.strip_prefix("randomtracks:") {
            let limit = lim.parse::<usize>().unwrap_or(50);
            self.async_run(
                ui,
                move || Ok(n.random_tracks(limit)?),
                move |xs| replace(&u, &format!("NAVIDROME · RANDOM {limit}"), xs, "No items"),
                "Navidrome unavailable",
            );
        } else if action == "randomartistsearch" {
            let app = self.arc();
            prompt(ui, "ARTIST › ", "", move |q| {
                if q.is_empty() {
                    return;
                }
                u.loading("NAVIDROME · ARTIST", "Searching…", Vec::new());
                let u2 = u.clone();
                app.async_run(
                    &u,
                    move || Ok(n.search_library(&q, 0, 0, 50)?),
                    move |xs| {
                        let artists: Vec<Track> =
                            xs.into_iter().filter(|x| x.kind == "artist").collect();
                        if artists.is_empty() {
                            return u2.show_empty("NAVIDROME · ARTIST", "No matching artist");
                        }
                        let rows = artists
                            .iter()
                            .map(|x| {
                                let aid = x.meta_str("artist_id");
                                let aid = if aid.is_empty() { x.id.clone() } else { aid };
                                Row::entry(
                                    x.title.clone(),
                                    format!("nav:randomartisttracks:{aid}"),
                                    "shuffle all songs",
                                )
                            })
                            .collect();
                        u2.show_menu("NAVIDROME · CHOOSE ARTIST", rows, true);
                    },
                    "Navidrome artist search unavailable",
                );
            });
        } else if let Some(aid) = action.strip_prefix("randomartisttracks:") {
            let aid = aid.to_string();
            ui.loading(
                "NAVIDROME · ARTIST",
                "Loading and shuffling all songs…",
                Vec::new(),
            );
            self.async_run(
                ui,
                move || Ok(n.artist_tracks(&aid, true)?),
                move |xs| replace(&u, "NAVIDROME · ARTIST · SHUFFLED", xs, "No items"),
                "Navidrome artist unavailable",
            );
        } else if action == "randomartist" {
            self.async_run(
                ui,
                move || Ok(n.random_artist_tracks()?),
                move |(name, xs)| {
                    replace(
                        &u,
                        &format!("NAVIDROME · RANDOM ARTIST · {name}"),
                        xs,
                        "No Navidrome artists",
                    )
                },
                "Navidrome random artist unavailable",
            );
        } else if matches!(action, "newest" | "random" | "albums") {
            let kind = match action {
                "newest" => "newest",
                "random" => "random",
                _ => "alphabeticalByName",
            };
            let title = format!("NAVIDROME · {}", action.to_uppercase());
            self.async_run(
                ui,
                move || Ok(n.albums(kind, 60)?),
                move |xs| replace(&u, &title, xs, "No items"),
                "Navidrome unavailable",
            );
        } else if action == "artists" {
            self.async_run(
                ui,
                move || Ok(n.artists()?),
                move |xs| replace(&u, "NAVIDROME · ARTISTS", xs, "No items"),
                "Navidrome unavailable",
            );
        } else if action == "playlists" {
            self.async_run(
                ui,
                move || Ok(n.playlists()?),
                move |xs| replace(&u, "NAVIDROME · PLAYLISTS", xs, "No items"),
                "Navidrome unavailable",
            );
        }
    }

    pub fn nav_goto_artist(&self, t: Track, ui: &UiRef) {
        if t.artist.is_empty() {
            return ui.status("No artist metadata");
        }
        ui.loading(
            &format!("NAVIDROME · {}", t.artist),
            "Finding artist…",
            Vec::new(),
        );
        let app = self.arc();
        let app2 = self.arc();
        let u = ui.clone();
        let artist = t.artist.clone();
        self.async_run(
            ui,
            move || {
                let n = app.nav().ok_or("Navidrome is not configured")?;
                let xs = n.search_library(&t.artist, 20, 8, 0)?;
                let cf = t.artist.to_lowercase();
                Ok(xs
                    .iter()
                    .find(|x| x.kind == "artist" && x.title.to_lowercase() == cf)
                    .or_else(|| xs.iter().find(|x| x.kind == "artist"))
                    .cloned())
            },
            move |x: Option<Track>| {
                let Some(x) = x else {
                    return u.show_empty(&format!("NAVIDROME · {artist}"), "Artist not found");
                };
                u.loading(&x.title, "Loading artist…", Vec::new());
                let u2 = u.clone();
                let app3 = app2.clone();
                let aid = {
                    let a = x.meta_str("artist_id");
                    if a.is_empty() {
                        x.id.clone()
                    } else {
                        a
                    }
                };
                let title = x.title.clone();
                app2.async_run(
                    &u,
                    move || {
                        Ok(app3
                            .nav()
                            .ok_or("Navidrome is not configured")?
                            .artist_albums(&aid)?)
                    },
                    move |xs| replace(&u2, &title, xs, "No items"),
                    "Artist unavailable",
                );
            },
            "Artist lookup unavailable",
        );
    }

    pub fn nav_goto_album(&self, t: Track, ui: &UiRef) {
        if t.album.is_empty() {
            return ui.status("No album metadata");
        }
        let q = format!("{} {}", t.artist, t.album).trim().to_string();
        ui.loading(
            &format!("NAVIDROME · {}", t.album),
            "Finding album…",
            Vec::new(),
        );
        let app = self.arc();
        let app2 = self.arc();
        let u = ui.clone();
        let album = t.album.clone();
        self.async_run(
            ui,
            move || {
                let n = app.nav().ok_or("Navidrome is not configured")?;
                let xs = n.search_library(&q, 0, 20, 0)?;
                let cf = t.album.to_lowercase();
                Ok(xs
                    .iter()
                    .find(|x| x.kind == "album" && x.title.to_lowercase() == cf)
                    .or_else(|| xs.iter().find(|x| x.kind == "album"))
                    .cloned())
            },
            move |x: Option<Track>| {
                let Some(x) = x else {
                    return u.show_empty(&format!("NAVIDROME · {album}"), "Album not found");
                };
                u.loading(&x.title, "Loading album…", Vec::new());
                let u2 = u.clone();
                let app3 = app2.clone();
                let aid = {
                    let a = x.meta_str("album_id");
                    if a.is_empty() {
                        x.id.clone()
                    } else {
                        a
                    }
                };
                let title = format!("{} · {}", x.artist, x.title);
                app2.async_run(
                    &u,
                    move || {
                        Ok(app3
                            .nav()
                            .ok_or("Navidrome is not configured")?
                            .album_tracks(&aid)?)
                    },
                    move |xs| replace(&u2, &title, xs, "No items"),
                    "Album unavailable",
                );
            },
            "Album lookup unavailable",
        );
    }

    /// Open a container (album/artist/playlist/program) or play an item.
    pub fn open_item(&self, t: Track, ui: &UiRef) {
        let u = ui.clone();
        let meta_or_id = |k: &str| meta_or_id(&t, k);
        let nav = || {
            self.nav()
                .ok_or_else(|| crate::core::Error::msg("Navidrome is not configured"))
        };
        match (t.source.as_str(), t.kind.as_str()) {
            ("spotify", "playlist") => {
                ui.loading(&t.title, "Loading Spotify playlist…", Vec::new());
                let sp = self.spotify.clone();
                let title = format!("SPOTIFY · {}", t.title);
                self.async_run(
                    ui,
                    {
                        let id = t.id.clone();
                        move || sp.playlist_items(&id)
                    },
                    move |xs| replace(&u, &title, xs, "Playlist has no accessible items"),
                    "Spotify playlist unavailable",
                );
            }
            ("spotify", "album") => {
                ui.loading(&t.title, "Loading Spotify album…", Vec::new());
                let sp = self.spotify.clone();
                let title = format!("SPOTIFY · {}", t.title);
                self.async_run(
                    ui,
                    {
                        let id = t.id.clone();
                        move || sp.album_tracks(&id)
                    },
                    move |xs| replace(&u, &title, xs, "Album is empty"),
                    "Spotify album unavailable",
                );
            }
            ("spotify", "artist") => self.spotify_search(&t.title, ui),
            ("navidrome", "album") => {
                ui.loading(&t.title, "Loading album…", Vec::new());
                let n = nav();
                let id = meta_or_id("album_id");
                let title = format!("{} · {}", t.artist, t.title);
                self.async_run(
                    ui,
                    move || Ok(n?.album_tracks(&id)?),
                    move |xs| replace(&u, &title, xs, "No items"),
                    "Album unavailable",
                );
            }
            ("navidrome", "artist") => {
                ui.loading(&t.title, "Loading artist…", Vec::new());
                let n = nav();
                let id = meta_or_id("artist_id");
                let title = t.title.clone();
                self.async_run(
                    ui,
                    move || Ok(n?.artist_albums(&id)?),
                    move |xs| replace(&u, &title, xs, "No items"),
                    "Artist unavailable",
                );
            }
            ("navidrome", "playlist") => {
                ui.loading(&t.title, "Loading playlist…", Vec::new());
                let n = nav();
                let id = meta_or_id("playlist_id");
                let title = t.title.clone();
                self.async_run(
                    ui,
                    move || Ok(n?.playlist_tracks(&id)?),
                    move |xs| replace(&u, &title, xs, "No items"),
                    "Playlist unavailable",
                );
            }
            ("sr", "program") => {
                ui.loading(&t.title, "Loading episodes…", Vec::new());
                let sr = self.sr.clone();
                let pid = t.meta_str("program_id");
                let title = t.title.clone();
                let t2 = t.title.clone();
                self.async_run(
                    ui,
                    move || {
                        if !pid.is_empty() {
                            sr.program_episodes(&pid, 60)
                        } else {
                            Ok(sr
                                .search_all(&t2, 60)
                                .into_iter()
                                .filter(|x| x.kind == "podcast")
                                .collect())
                        }
                    },
                    move |xs: Vec<Track>| {
                        u.set_sort_mode("newest");
                        let n = xs.len();
                        replace(
                            &u,
                            &format!("SR · {title} · {n} EPISODES"),
                            xs,
                            "No playable episodes found",
                        );
                    },
                    "Programme unavailable",
                );
            }
            _ => {
                let resume = t.meta_f64("bookmark_pos");
                self.play_with(t, Some(ui), false, resume, false);
            }
        }
    }

    pub fn load_sr_programs(&self, q: &str, ui: &UiRef) {
        ui.loading("SR · PROGRAMMES", "Loading programmes…", Vec::new());
        let sr = self.sr.clone();
        let q = q.to_string();
        let u = ui.clone();
        self.async_run(
            ui,
            move || sr.programs(&q, 200),
            move |xs| replace(&u, "SR · PROGRAMMES", xs, "No items"),
            "SR unavailable",
        );
    }

    pub fn search_sr_all(&self, q: &str, ui: &UiRef) {
        if q.is_empty() {
            return;
        }
        if q.trim().chars().count() < 2 {
            return ui.status("SR search · type at least 2 characters");
        }
        ui.push();
        ui.loading(
            "SR · SEARCH",
            &format!("Searching programmes and episodes for “{q}”…"),
            Vec::new(),
        );
        let sr = self.sr.clone();
        let (q1, q2) = (q.to_string(), q.to_string());
        let u = ui.clone();
        self.async_run(
            ui,
            move || Ok(sr.search_all(&q1, 50)),
            move |xs| {
                let n = xs.len();
                u.replace_list(
                    &format!("SR · SEARCH · {q2}"),
                    track_rows(xs),
                    "No programmes or episodes found",
                    Some(&format!("{n} SR results")),
                );
            },
            "SR search unavailable",
        );
    }

    pub fn search_sr_programs(&self, q: &str, ui: &UiRef) {
        if q.is_empty() {
            return;
        }
        if q.trim().chars().count() < 2 {
            return ui.status("SR search · type at least 2 characters");
        }
        ui.push();
        ui.loading("SR · PROGRAMMES", &format!("Searching “{q}”…"), Vec::new());
        let sr = self.sr.clone();
        let (q1, q2) = (q.to_string(), q.to_string());
        let u = ui.clone();
        self.async_run(
            ui,
            move || sr.programs(&q1, 50),
            move |xs| replace(&u, &format!("SR PROGRAMMES · {q2}"), xs, "No items"),
            "SR unavailable",
        );
    }

    /// Fetch "now on air" for SR live channels; notify for the playing one.
    pub fn refresh_radio_async(&self, xs: Vec<Track>, ui: Option<&UiRef>) {
        let app = self.arc();
        let work = move || -> Result<HashMap<String, Map<String, Value>>> {
            let mut info = HashMap::new();
            for t in &xs {
                let m = app
                    .sr
                    .live_info(&t.id)
                    .and_then(|i| serde_json::to_value(i).ok())
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                info.insert(t.id.clone(), m);
            }
            app.state().radio_info.extend(info.clone());
            Ok(info)
        };
        let app = self.arc();
        let ui2 = ui.cloned();
        let done = move |info: HashMap<String, Map<String, Value>>| {
            let cur = app.current();
            if let Some(c) = &cur {
                if c.source == "sr" && c.kind == "radio" {
                    if let Some(i) = info.get(&c.id) {
                        app.notifier.track_changed(c, true, Some(i));
                    }
                }
            }
            if let Some(u) = &ui2 {
                let name = cur
                    .filter(|c| c.kind == "radio")
                    .map(|c| c.title)
                    .unwrap_or_else(|| "Sveriges Radio".into());
                u.status(&format!("Playing live · {name}"));
                u.invalidate();
            }
        };
        match ui {
            Some(u) => self.async_run(u, work, done, "SR metadata unavailable"),
            None => {
                std::thread::spawn(move || {
                    if let Ok(i) = work() {
                        done(i)
                    }
                });
            }
        }
    }

    /// Search one provider: `nav`, `sr` (episodes) or `yt`.
    pub fn search_provider(&self, source: &str, q: &str, ui: &UiRef) {
        if q.is_empty() {
            return;
        }
        if source == "sr" && q.trim().chars().count() < 2 {
            return ui.status("SR search · type at least 2 characters");
        }
        let title = match source {
            "nav" => "NAVIDROME",
            "sr" => "SR EPISODES",
            "yt" => "YOUTUBE",
            _ => return,
        };
        ui.push();
        ui.loading(title, &format!("Searching “{q}”…"), Vec::new());
        let app = self.arc();
        let app2 = self.arc();
        let (src, q1, q2) = (source.to_string(), q.to_string(), q.to_string());
        let src2 = src.clone();
        let u = ui.clone();
        self.async_run(
            ui,
            move || -> Result<Vec<Track>> {
                let mut xs = match src.as_str() {
                    "nav" => app
                        .nav()
                        .ok_or("Navidrome is not configured")?
                        .search_library(&q1, 30, 15, 10)?,
                    "sr" => app
                        .sr
                        .search(&q1, 40)
                        .into_iter()
                        .filter(|x| x.kind == "podcast")
                        .collect(),
                    _ => app.yt.search(&q1, 25)?,
                };
                if src == "sr" {
                    crate::providers::sort_by_published(&mut xs, "newest");
                }
                Ok(xs)
            },
            move |mut xs: Vec<Track>| {
                let mode = match src2.as_str() {
                    "sr" => "newest".to_string(),
                    "yt" => app2.cfg.get("youtube_sort", "relevance"),
                    _ => "relevance".into(),
                };
                u.set_sort_mode(&mode);
                if (mode == "newest" || mode == "oldest")
                    && xs.iter().any(|x| x.meta_f64("published_ts") != 0.0)
                {
                    crate::providers::sort_by_published(&mut xs, &mode);
                }
                let n = xs.len();
                u.replace_list(
                    &format!("{title} · {q2} · {n} RESULTS"),
                    track_rows(xs),
                    "No items",
                    Some(&format!("{n} results")),
                );
            },
            &format!("{title} unavailable"),
        );
    }

    /// Quick Find across every enabled source.
    pub fn search_async(&self, q: &str, ui: &UiRef) {
        crate::services::search::search(&self.arc(), q, ui)
    }
}
