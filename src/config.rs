//! `config.ini` and `secrets.ini`, read and written like Python's
//! `configparser` so 1.x files open unchanged and stay readable by 1.x.

use crate::paths;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// An ordered INI document with configparser semantics: keys are lowercased,
/// `=` or `:` delimit, `#`/`;` start comment lines, indented lines continue
/// the previous value, and writing produces `key = value` lines with a blank
/// line after each section.
#[derive(Clone, Debug, Default)]
pub struct Ini {
    pub sections: Vec<(String, Vec<(String, String)>)>,
}

impl Ini {
    pub fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut cur: Option<usize> = None;
        let mut last_key: Option<usize> = None;
        for raw in text.lines() {
            let line = raw.trim_end();
            let stripped = line.trim_start();
            if stripped.is_empty() {
                last_key = None;
                continue;
            }
            if stripped.starts_with('#') || stripped.starts_with(';') {
                continue;
            }
            let indented = line.len() != stripped.len();
            if indented {
                if let (Some(s), Some(k)) = (cur, last_key) {
                    let v = &mut ini.sections[s].1[k].1;
                    v.push('\n');
                    v.push_str(stripped);
                    continue;
                }
            }
            if stripped.starts_with('[') && stripped.ends_with(']') {
                let name = stripped[1..stripped.len() - 1].to_string();
                cur = Some(match ini.sections.iter().position(|(n, _)| *n == name) {
                    Some(i) => i,
                    None => {
                        ini.sections.push((name, Vec::new()));
                        ini.sections.len() - 1
                    }
                });
                last_key = None;
                continue;
            }
            let Some(s) = cur else { continue };
            let pos = stripped.find(['=', ':']);
            let (k, v) = match pos {
                Some(p) => (stripped[..p].trim(), stripped[p + 1..].trim()),
                None => (stripped, ""),
            };
            let key = k.to_lowercase();
            let items = &mut ini.sections[s].1;
            match items.iter().position(|(n, _)| *n == key) {
                Some(i) => {
                    items[i].1 = v.to_string();
                    last_key = Some(i);
                }
                None => {
                    items.push((key, v.to_string()));
                    last_key = Some(items.len() - 1);
                }
            }
        }
        ini
    }

    pub fn read(path: &Path) -> Ini {
        std::fs::read_to_string(path)
            .map(|t| Ini::parse(&t))
            .unwrap_or_default()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (name, items) in &self.sections {
            out.push('[');
            out.push_str(name);
            out.push_str("]\n");
            for (k, v) in items {
                out.push_str(k);
                out.push_str(" = ");
                out.push_str(&v.replace('\n', "\n\t"));
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    pub fn has_section(&self, s: &str) -> bool {
        self.sections.iter().any(|(n, _)| n == s)
    }

    pub fn section(&self, s: &str) -> Option<&Vec<(String, String)>> {
        self.sections.iter().find(|(n, _)| n == s).map(|(_, v)| v)
    }

    fn section_mut(&mut self, s: &str) -> &mut Vec<(String, String)> {
        if !self.has_section(s) {
            self.sections.push((s.to_string(), Vec::new()));
        }
        let i = self.sections.iter().position(|(n, _)| n == s).unwrap();
        &mut self.sections[i].1
    }

    pub fn get(&self, s: &str, k: &str) -> Option<&str> {
        let k = k.to_lowercase();
        self.section(s)?
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, s: &str, k: &str, v: &str) {
        let k = k.to_lowercase();
        let items = self.section_mut(s);
        match items.iter_mut().find(|(n, _)| *n == k) {
            Some(e) => e.1 = v.to_string(),
            None => items.push((k, v.to_string())),
        }
    }

    pub fn remove_section(&mut self, s: &str) {
        self.sections.retain(|(n, _)| n != s);
    }

    pub fn ensure_section(&mut self, s: &str) {
        self.section_mut(s);
    }
}

/// configparser's boolean words; anything else is `None`.
pub fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "1" | "yes" | "true" | "on" => Some(true),
        "0" | "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

pub const DEFAULT_KEYS: &[(&str, &str)] = &[
    ("play_pause", "SPACE"),
    ("next", "n"),
    ("previous", "p"),
    ("seek_back", "LEFT"),
    ("seek_forward", "RIGHT"),
    ("seek_back_long", "H"),
    ("seek_forward_long", "L"),
    ("volume_up", "+"),
    ("volume_down", "-"),
    ("mute", "m"),
    ("search", "/"),
    ("browse", "b"),
    ("queue", "q"),
    ("favorite", "f"),
    ("visualizer", "v"),
    ("help", "?"),
    ("back", "ESC"),
    ("quit", "Q"),
    ("down", "j"),
    ("up", "k"),
    ("activate", "ENTER"),
    ("command", ":"),
    ("refresh", "u"),
    ("home", "g"),
];

pub const DEFAULT_STATS_URL: &str = "https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3";

fn defaults() -> Vec<(&'static str, String)> {
    let music = paths::home_dir()
        .join("Music")
        .to_string_lossy()
        .into_owned();
    let d: &[(&str, &str)] = &[
        ("statistics_enabled", "true"),
        ("statistics_sync_enabled", "false"),
        ("statistics_sync_url", DEFAULT_STATS_URL),
        ("music_dirs", "\u{0}"),
        ("youtube_enabled", "true"),
        ("navidrome_enabled", "true"),
        ("navidrome_url", ""),
        ("navidrome_user", ""),
        ("navidrome_bitrate", "0"),
        ("visualizer", "true"),
        ("visualizer_height", "10"),
        ("kitty_artwork", "true"),
        ("now_playing_view", "normal"),
        ("theme", "niru-noir"),
        ("sr_enabled", "true"),
        ("custom_radio_enabled", "true"),
        ("auto_library_refresh", "true"),
        ("normalization", "true"),
        ("normalization_mode", "auto"),
        ("replaygain_preamp", "0"),
        ("replaygain_clip", "true"),
        ("gapless", "false"),
        ("shuffle_mode", "off"),
        ("repeat_mode", "off"),
        ("youtube_sort", "relevance"),
        ("spotify_enabled", "false"),
        ("spotify_client_id", ""),
        ("spotify_device_id", ""),
        ("spotify_device_name", ""),
        ("spotify_playback", "local"),
        ("spotify_local_name", "NPLAY"),
        ("spotify_local_backend", "pulseaudio"),
        ("spotify_local_bitrate", "320"),
        ("spotify_local_volume", "70"),
        ("music_excludes", "Reaper-projects"),
        ("library_scan_batch", "250"),
        ("track_notifications", "true"),
        ("notification_artwork", "true"),
        ("mpris_enabled", "true"),
        ("preferred_source", "local"),
        ("visualizer_style", "classic"),
        ("visualizer_gradient", "theme"),
    ];
    d.iter()
        .map(|(k, v)| {
            (
                *k,
                if *v == "\u{0}" {
                    music.clone()
                } else {
                    v.to_string()
                },
            )
        })
        .collect()
}

struct Inner {
    c: Ini,
    s: Ini,
}

/// NPLAY's configuration (`config.ini`) and secrets (`secrets.ini`, 0600).
/// Cheap to clone; all clones share one state. Every `set` saves at once,
/// like 1.x.
#[derive(Clone)]
pub struct Config {
    inner: Arc<Mutex<Inner>>,
    pub path: PathBuf,
    pub secret_path: PathBuf,
}

impl Config {
    pub fn load() -> Config {
        paths::ensure();
        let dir = paths::config();
        let path = dir.join("config.ini");
        let secret_path = dir.join("secrets.ini");
        let mut c = Ini::read(&path);
        let s = Ini::read(&secret_path);
        let mut changed = false;
        if !c.has_section("general") {
            c.ensure_section("general");
            changed = true;
        }
        for (k, v) in defaults() {
            if c.get("general", k).is_none() {
                c.set("general", k, &v);
                changed = true;
            }
        }
        if !c.has_section("keys") {
            for (k, v) in DEFAULT_KEYS {
                c.set("keys", k, v);
            }
            changed = true;
        }
        let cfg = Config {
            inner: Arc::new(Mutex::new(Inner { c, s })),
            path,
            secret_path,
        };
        if changed {
            cfg.save();
        }
        cfg
    }

    /// `[general] k`, or `d` when absent.
    pub fn get(&self, k: &str, d: &str) -> String {
        let g = self.inner.lock().unwrap();
        g.c.get("general", k).unwrap_or(d).to_string()
    }

    pub fn getbool(&self, k: &str, d: bool) -> bool {
        let g = self.inner.lock().unwrap();
        g.c.get("general", k).and_then(parse_bool).unwrap_or(d)
    }

    pub fn getint(&self, k: &str, d: i64) -> i64 {
        self.get(k, "").trim().parse().unwrap_or(d)
    }

    pub fn getfloat(&self, k: &str, d: f64) -> f64 {
        self.get(k, "").trim().parse().unwrap_or(d)
    }

    pub fn has(&self, k: &str) -> bool {
        self.inner.lock().unwrap().c.get("general", k).is_some()
    }

    pub fn set(&self, k: &str, v: impl ToString) {
        {
            let mut g = self.inner.lock().unwrap();
            g.c.set("general", k, &v.to_string());
        }
        self.save();
    }

    /// Any section of config.ini (themes, keys, ...).
    pub fn get_in(&self, section: &str, k: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .c
            .get(section, k)
            .map(str::to_string)
    }

    pub fn set_in(&self, section: &str, k: &str, v: &str) {
        self.inner.lock().unwrap().c.set(section, k, v);
        self.save();
    }

    /// All `[general]` items in file order.
    pub fn general_items(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap()
            .c
            .section("general")
            .cloned()
            .unwrap_or_default()
    }

    /// Key bindings: defaults overlaid by `[keys]`.
    pub fn keys(&self) -> Vec<(String, String)> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<(String, String)> = DEFAULT_KEYS
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(items) = g.c.section("keys") {
            for (k, v) in items {
                match out.iter_mut().find(|(n, _)| n == k) {
                    Some(e) => e.1 = v.clone(),
                    None => out.push((k.clone(), v.clone())),
                }
            }
        }
        out
    }

    pub fn save(&self) {
        let text = self.inner.lock().unwrap().c.render();
        let _ = std::fs::write(&self.path, text);
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
    }

    fn save_secrets(&self, g: &Inner) {
        let text = g.s.render();
        let _ = write_private(&self.secret_path, text.as_bytes());
    }

    pub fn secret(&self, section: &str, k: &str) -> String {
        self.inner
            .lock()
            .unwrap()
            .s
            .get(section, k)
            .unwrap_or("")
            .to_string()
    }

    pub fn nav_password(&self) -> String {
        match std::env::var("NPLAY_NAVIDROME_PASSWORD") {
            Ok(p) if !p.is_empty() => p,
            _ => self.secret("navidrome", "password"),
        }
    }

    pub fn save_nav(&self, url: &str, user: &str, password: &str) {
        let mut g = self.inner.lock().unwrap();
        g.c.set("general", "navidrome_url", url.trim_end_matches('/'));
        g.c.set("general", "navidrome_user", user);
        let text = g.c.render();
        let _ = std::fs::write(&self.path, text);
        g.s.set("navidrome", "password", password);
        self.save_secrets(&g);
    }

    pub fn clear_nav(&self) {
        let mut g = self.inner.lock().unwrap();
        g.c.set("general", "navidrome_url", "");
        g.c.set("general", "navidrome_user", "");
        let text = g.c.render();
        let _ = std::fs::write(&self.path, text);
        g.s.remove_section("navidrome");
        if self.secret_path.exists() {
            self.save_secrets(&g);
        }
    }

    pub fn statistics_token(&self) -> String {
        self.secret("statistics", "token")
    }

    pub fn save_statistics_token(&self, token: &str) -> Result<(), String> {
        if token.is_empty() || token.contains('\n') || token.contains('\r') || token.len() > 1024 {
            return Err("Invalid token".into());
        }
        let mut g = self.inner.lock().unwrap();
        g.s.set("statistics", "token", token);
        self.save_secrets(&g);
        Ok(())
    }

    /// The `[spotify]` secrets section (OAuth tokens).
    pub fn spotify_tokens(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap()
            .s
            .section("spotify")
            .cloned()
            .unwrap_or_default()
    }

    pub fn save_spotify_tokens(&self, tokens: &[(&str, String)]) {
        let mut g = self.inner.lock().unwrap();
        g.s.ensure_section("spotify");
        for (k, v) in tokens {
            if matches!(
                *k,
                "access_token" | "refresh_token" | "expires_at" | "scope" | "token_type"
            ) {
                g.s.set("spotify", k, v);
            }
        }
        self.save_secrets(&g);
    }

    pub fn clear_spotify_tokens(&self) {
        let mut g = self.inner.lock().unwrap();
        g.s.remove_section("spotify");
        self.save_secrets(&g);
    }

    /// Re-read both files from disk (after another process changed them).
    pub fn reload(&self) {
        let mut g = self.inner.lock().unwrap();
        g.c = Ini::read(&self.path);
        g.s = Ini::read(&self.secret_path);
        g.c.ensure_section("general");
    }
}

/// Write a file created 0600 (O_CREAT|O_TRUNC), then chmod 0600.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_roundtrip_like_configparser() {
        let t =
            "[general]\nMusic_Dirs = /a\nx: y\n# c\n; d\nempty =\n\n[keys]\nplay_pause = SPACE\n";
        let i = Ini::parse(t);
        assert_eq!(i.get("general", "music_dirs"), Some("/a"));
        assert_eq!(i.get("general", "x"), Some("y"));
        assert_eq!(i.get("general", "empty"), Some(""));
        assert_eq!(
            i.render(),
            "[general]\nmusic_dirs = /a\nx = y\nempty = \n\n[keys]\nplay_pause = SPACE\n\n"
        );
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool("Yes"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }
}
