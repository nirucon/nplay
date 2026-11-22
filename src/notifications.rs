//! Desktop "now playing" notifications (port of `nplay/notifications.py`).
//!
//! A small freedesktop notification adapter around `notify-send` (external
//! program, as in 1.x). Notifications are deduplicated by track identity
//! (`source:id|url|path`), replace each other via the
//! `x-canonical-private-synchronous:nplay-now-playing` hint and are sent from
//! a short-lived background thread that first resolves the cover through the
//! artwork cache (download at most 6 s).
//!
//! Config keys: `track_notifications` (default on), `notification_artwork`
//! (default on), `artwork_cache_mb` (cache size when no cache is given).
//!
//! Public API (`Notifier` is `Clone + Send + Sync`):
//! - `Notifier::new(cfg, cache)`; `Notifier::with_settings(getbool, cache)`
//!   takes a `getbool(key, default)` closure instead of a `Config`.
//! - `set_binary(Option<path>)`: override `notify-send` (tests).
//! - `available()`: notify-send found and a session (D-Bus/X11/Wayland).
//! - `enabled()`: `track_notifications` and `available()`.
//! - `track_changed(track, force, radio_info)`: notify once per track
//!   (`force` repeats); `radio_info` is the SR live map (`program`, `episode`).
//! - `test(track) -> bool`: send a test (current track or a fixed text).
//! - `message(track, radio_info) -> (title, body)`: the text (pure).
//! - `track_key(track)`: the dedupe identity.

use crate::artwork_cache::ArtworkCache;
use crate::model::Track;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type GetBool = Arc<dyn Fn(&str, bool) -> bool + Send + Sync>;

struct Inner {
    getbool: GetBool,
    cache: ArtworkCache,
    binary: Mutex<Option<PathBuf>>,
    last_key: Mutex<String>,
}

#[derive(Clone)]
pub struct Notifier {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// `source:(id or url or path)`.
pub fn track_key(t: &Track) -> String {
    let ident = [&t.id, &t.url, &t.path]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default();
    format!("{}:{}", t.source, ident)
}

fn source_label(src: &str) -> String {
    // Same chained replacements as 1.x.
    src.replace("sr", "Sveriges Radio")
        .replace("navidrome", "Navidrome")
        .replace("spotify", "Spotify")
        .replace("youtube", "YouTube")
        .replace("local", "Local")
}

fn info_str(info: Option<&Map<String, Value>>, k: &str) -> String {
    info.and_then(|m| m.get(k))
        .map(crate::model::value_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Notification title and body for a track.
pub fn message(t: &Track, radio_info: Option<&Map<String, Value>>) -> (String, String) {
    let artist = t.artist.trim();
    let album = t.album.trim();
    let src = if t.source.is_empty() { "NPLAY" } else { &t.source };
    // Live SR is programme-first: avoid P1 / Sveriges Radio duplication.
    if t.source == "sr" && t.kind == "radio" {
        let mut program = info_str(radio_info, "program");
        if program.is_empty() {
            program = info_str(radio_info, "episode");
        }
        let channel = if t.title.is_empty() {
            "Sveriges Radio".to_string()
        } else {
            t.title.trim().to_string()
        };
        let title = if program.is_empty() {
            channel.clone()
        } else {
            program.clone()
        };
        let parts: Vec<&str> = [
            if program.is_empty() { "" } else { channel.as_str() },
            "Sveriges Radio",
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect();
        return (title, parts.join(" · "));
    }
    let source = source_label(src);
    let mut lines = Vec::new();
    if !artist.is_empty() {
        lines.push(artist.to_string());
    }
    let detail: Vec<&str> = [album, source.as_str()]
        .into_iter()
        .filter(|x| !x.is_empty() && x.to_lowercase() != artist.to_lowercase())
        .collect();
    if !detail.is_empty() {
        lines.push(detail.join(" · "));
    }
    let title = if t.title.is_empty() {
        "Now playing".to_string()
    } else {
        t.title.clone()
    };
    (title, lines.join("\n"))
}

fn env_set(k: &str) -> bool {
    std::env::var_os(k).is_some_and(|v| !v.is_empty())
}

impl Notifier {
    pub fn new(cfg: crate::config::Config, cache: Option<ArtworkCache>) -> Notifier {
        let cache = cache.unwrap_or_else(|| {
            let mb = cfg.get("artwork_cache_mb", "500").trim().parse().unwrap_or(500);
            ArtworkCache::new(mb)
        });
        Notifier::with_settings(Arc::new(move |k: &str, d: bool| cfg.getbool(k, d)), cache)
    }

    pub fn with_settings(getbool: GetBool, cache: ArtworkCache) -> Notifier {
        Notifier {
            inner: Arc::new(Inner {
                getbool,
                cache,
                binary: Mutex::new(crate::paths::which("notify-send")),
                last_key: Mutex::new(String::new()),
            }),
        }
    }

    pub fn set_binary(&self, p: Option<PathBuf>) {
        *lock(&self.inner.binary) = p;
    }

    pub fn available(&self) -> bool {
        lock(&self.inner.binary).is_some()
            && (env_set("DBUS_SESSION_BUS_ADDRESS") || env_set("DISPLAY") || env_set("WAYLAND_DISPLAY"))
    }

    pub fn enabled(&self) -> bool {
        (self.inner.getbool)("track_notifications", true) && self.available()
    }

    fn artwork(&self, cover: &str) -> Option<PathBuf> {
        if !(self.inner.getbool)("notification_artwork", true) || cover.is_empty() {
            return None;
        }
        self.inner
            .cache
            .fetch(cover, 6.0, &crate::artwork_cache::user_agent())
    }

    fn send(&self, title: &str, body: &str, icon: Option<PathBuf>) -> bool {
        if !self.available() {
            return false;
        }
        let Some(bin) = lock(&self.inner.binary).clone() else {
            return false;
        };
        let mut cmd = Command::new(bin);
        cmd.args([
            "--app-name=NPLAY",
            "--urgency=low",
            "--expire-time=5000",
            "--hint=string:x-canonical-private-synchronous:nplay-now-playing",
        ]);
        if let Some(i) = icon {
            cmd.arg("--icon").arg(i);
        }
        cmd.arg(if title.is_empty() { "NPLAY" } else { title })
            .arg(body)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        // 4 s limit like 1.x; this runs on a background thread.
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            match child.try_wait() {
                Ok(Some(st)) => return st.success(),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    pub fn track_changed(&self, t: &Track, force: bool, radio_info: Option<&Map<String, Value>>) {
        if !self.enabled() {
            return;
        }
        let key = track_key(t);
        {
            let mut last = lock(&self.inner.last_key);
            if !force && key == *last {
                return;
            }
            *last = key;
        }
        let me = self.clone();
        let t = t.clone();
        let info = radio_info.cloned();
        let _ = std::thread::Builder::new()
            .name("nplay-notify".into())
            .spawn(move || {
                let art = me.artwork(&t.cover);
                let (title, body) = message(&t, info.as_ref());
                me.send(&title, &body, art);
            });
    }

    pub fn test(&self, t: Option<&Track>) -> bool {
        if !self.available() {
            return false;
        }
        if let Some(t) = t {
            self.track_changed(t, true, None);
            return true;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("nplay-notify".into())
            .spawn(move || {
                me.send("NPLAY notifications", "System notifications are working.", None);
            });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str, id: &str, title: &str, artist: &str, album: &str) -> Track {
        Track {
            artist: artist.into(),
            album: album.into(),
            ..Track::new(source, id, title)
        }
    }

    #[test]
    fn messages_match_1x() {
        let t = track("navidrome", "1", "Song", "Artist", "Album");
        assert_eq!(
            message(&t, None),
            ("Song".into(), "Artist\nAlbum · Navidrome".into())
        );
        let t = track("local", "1", "", "", "");
        assert_eq!(message(&t, None), ("Now playing".into(), "Local".into()));
        // Album equal to the artist (case-insensitive) is not repeated.
        let t = track("youtube", "x", "Clip", "Band", "band");
        assert_eq!(message(&t, None), ("Clip".into(), "Band\nYouTube".into()));
        let mut r = track("sr", "132", "P1", "", "");
        r.kind = "radio".into();
        assert_eq!(
            message(&r, None),
            ("P1".into(), "Sveriges Radio".into())
        );
        let mut info = Map::new();
        info.insert("program".into(), Value::from(" Morgonpasset "));
        assert_eq!(
            message(&r, Some(&info)),
            ("Morgonpasset".into(), "P1 · Sveriges Radio".into())
        );
        let mut info = Map::new();
        info.insert("episode".into(), Value::from("Ep"));
        assert_eq!(message(&r, Some(&info)).0, "Ep");
        // SR podcast episodes (not radio) use the normal layout.
        let p = track("sr", "9", "Episode", "", "Show");
        assert_eq!(message(&p, None).1, "Show · Sveriges Radio");
    }

    #[test]
    fn identity_prefers_id_then_url_then_path() {
        let mut t = track("local", "", "x", "", "");
        t.path = "/m/a.flac".into();
        assert_eq!(track_key(&t), "local:/m/a.flac");
        t.url = "http://u".into();
        assert_eq!(track_key(&t), "local:http://u");
        t.id = "7".into();
        assert_eq!(track_key(&t), "local:7");
    }

    #[test]
    fn sends_once_per_track_with_fake_notify_send() {
        if !env_set("DBUS_SESSION_BUS_ADDRESS") && !env_set("DISPLAY") && !env_set("WAYLAND_DISPLAY")
        {
            // available() needs a session; the environment is not changed here.
            return;
        }
        let d = std::env::temp_dir().join(format!(
            "nplay-notify-{}-{:x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&d).unwrap();
        let out = d.join("calls");
        let count = d.join("count");
        let bin = d.join("notify-send");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s|' \"$@\" >> {}\necho x >> {}\n",
                out.display(),
                count.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cover = d.join("c.png");
        std::fs::write(&cover, b"x").unwrap();
        let n = Notifier::with_settings(
            Arc::new(|_k: &str, d: bool| d),
            ArtworkCache::with_root(d.join("art"), 500),
        );
        n.set_binary(Some(bin));
        let mut t = track("local", "1", "Song", "Artist", "Album");
        t.cover = cover.to_string_lossy().into_owned();
        n.track_changed(&t, false, None);
        n.track_changed(&t, false, None);
        let lines = || {
            std::fs::read_to_string(&count)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let end = Instant::now() + Duration::from_secs(5);
        while lines() < 1 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(lines(), 1, "deduplicated");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("--app-name=NPLAY|"));
        assert!(text.contains(&format!("--icon|{}|", cover.display())));
        assert!(text.contains("Song|Artist\nAlbum · Local|"));
        n.track_changed(&t, true, None);
        let end = Instant::now() + Duration::from_secs(5);
        while lines() < 2 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(lines(), 2, "force repeats");
        let _ = std::fs::remove_dir_all(d);
    }
}
