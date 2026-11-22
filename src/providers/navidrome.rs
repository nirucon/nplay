//! Navidrome / Subsonic client. See the parent module doc for the API.
//!
//! Every request carries `u`, `t` = md5(password + salt), `s` = 12 random hex
//! digits, `v=1.16.1`, `c=nplay`, `f=json`. URLs built here contain
//! credentials: log them only through `crate::redact`.

use super::util::{self, arr, obj, raw, s, Fetcher, HttpError};
use crate::model::{value_i64, Meta, Track};
use crate::redact::{redact_text, urlencode};
use md5::{Digest, Md5};
use serde_json::{Map, Value};
use std::sync::Arc;

pub const NAVIDROME_REMEDY: &str = "check the URL and password in Settings → Navidrome";

/// Subsonic error codes that mean "fix the configuration" rather than "retry later".
const SUBSONIC_AUTH: &[i64] = &[10, 20, 30, 40, 41, 42, 43, 44, 50];

/// A user-facing Navidrome failure. Its text never contains a URL or credential.
#[derive(Debug, Clone, PartialEq)]
pub struct NavidromeError {
    pub what: String,
    pub status: Option<u16>,
    pub remedy: String,
    message: String,
}

impl NavidromeError {
    /// `what (HTTP status) · detail · remedy`.
    pub fn new(what: &str, status: Option<u16>, detail: &str, remedy: &str) -> NavidromeError {
        let mut parts = vec![match status {
            Some(c) if c != 0 => format!("{what} (HTTP {c})"),
            _ => what.to_string(),
        }];
        if !detail.is_empty() {
            parts.push(util::take_chars(&redact_text(detail), 160));
        }
        if !remedy.is_empty() {
            parts.push(remedy.to_string());
        }
        NavidromeError {
            what: what.to_string(),
            status,
            remedy: remedy.to_string(),
            message: parts.join(" · "),
        }
    }

    /// With the default remedy and no detail.
    pub fn simple(what: &str, status: Option<u16>) -> NavidromeError {
        NavidromeError::new(what, status, "", NAVIDROME_REMEDY)
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for NavidromeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NavidromeError {}

impl From<NavidromeError> for crate::core::Error {
    fn from(e: NavidromeError) -> Self {
        use crate::core::ErrorKind;
        let kind = if e.what.contains("authentication") || matches!(e.status, Some(401 | 403)) {
            ErrorKind::Authentication
        } else if e.what.contains("unreachable") {
            ErrorKind::Network
        } else {
            ErrorKind::Provider
        };
        crate::core::Error::new(kind, e.message)
    }
}

pub type NavResult<T> = std::result::Result<T, NavidromeError>;

/// Turn an mpv load failure of a Navidrome stream into an actionable,
/// credential-free error (1.x `MpvStartCoordinator._navidrome_stream_failed`).
/// `status` is the result of [`Navidrome::stream_status`].
pub fn navidrome_stream_failure(status: Option<u16>) -> NavidromeError {
    match status {
        Some(c @ (401 | 403)) => NavidromeError::simple("Navidrome rejected the stream", Some(c)),
        Some(c) if c >= 400 => NavidromeError::simple("Navidrome stream failed", Some(c)),
        None | Some(0) => NavidromeError::simple("Navidrome is unreachable", None),
        Some(c) => NavidromeError::new(
            "Navidrome stream could not be decoded",
            Some(c),
            "",
            "see mpv.log for the decoder error",
        ),
    }
}

fn default_fetcher() -> Fetcher {
    Arc::new(|url: &str, timeout: u64| util::http_get(url, timeout, &util::user_agent(), &[]))
}

#[derive(Clone)]
pub struct Navidrome {
    pub url: String,
    pub user: String,
    pub password: String,
    pub bitrate: i64,
    fetch: Fetcher,
}

impl std::fmt::Debug for Navidrome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Navidrome")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("bitrate", &self.bitrate)
            .finish_non_exhaustive()
    }
}

type Params = Vec<(String, String)>;

fn p(k: &str, v: impl ToString) -> (String, String) {
    (k.to_string(), v.to_string())
}

impl Navidrome {
    pub fn new(url: &str, user: &str, password: &str, bitrate: i64) -> Navidrome {
        Navidrome {
            url: url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            password: password.to_string(),
            bitrate,
            fetch: default_fetcher(),
        }
    }

    /// 1.x `App.nav()`: `None` when disabled or not fully configured.
    pub fn from_config(cfg: &crate::config::Config) -> Option<Navidrome> {
        if !cfg.getbool("navidrome_enabled", true) {
            return None;
        }
        let u = cfg.get("navidrome_url", "");
        let user = cfg.get("navidrome_user", "");
        let pw = cfg.nav_password();
        if u.is_empty() || user.is_empty() || pw.is_empty() {
            return None;
        }
        let br = cfg.get("navidrome_bitrate", "0");
        let br = br.trim().parse::<i64>().unwrap_or(0);
        Some(Navidrome::new(&u, &user, &pw, br))
    }

    /// Replace the HTTP transport (tests).
    pub fn with_fetcher(mut self, fetch: Fetcher) -> Navidrome {
        self.fetch = fetch;
        self
    }

    /// Fresh authentication parameters (new salt every call).
    pub fn params(&self) -> Params {
        let salt = util::token_hex(6);
        let mut h = Md5::new();
        h.update(self.password.as_bytes());
        h.update(salt.as_bytes());
        let token: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        vec![
            p("u", &self.user),
            p("t", token),
            p("s", salt),
            p("v", "1.16.1"),
            p("c", "nplay"),
            p("f", "json"),
        ]
    }

    fn url_for(&self, endpoint: &str, extra: Params) -> String {
        let mut q = self.params();
        for (k, v) in extra {
            match q.iter_mut().find(|(qk, _)| *qk == k) {
                Some(slot) => slot.1 = v,
                None => q.push((k, v)),
            }
        }
        format!("{}/rest/{}.view?{}", self.url, endpoint, urlencode(&q, ""))
    }

    /// Call `method` and return the `subsonic-response` object.
    pub fn call(&self, method: &str, extra: Params) -> NavResult<Value> {
        let u = self.url_for(method, extra);
        let body = match (self.fetch)(&u, 8) {
            Ok(b) => b,
            Err(HttpError::Status(c)) => {
                return Err(NavidromeError::simple("Navidrome request failed", Some(c)))
            }
            Err(HttpError::Unreachable(d)) => {
                return Err(NavidromeError::new(
                    "Navidrome is unreachable",
                    None,
                    &d,
                    NAVIDROME_REMEDY,
                ))
            }
        };
        let v: Value = std::str::from_utf8(&body)
            .ok()
            .and_then(|t| serde_json::from_str(t).ok())
            .ok_or_else(|| {
                NavidromeError::simple("Navidrome returned an invalid response", None)
            })?;
        let d = v
            .get("subsonic-response")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Map::new()));
        if s(&d, "status") != "ok" || !matches!(d.get("status"), Some(Value::String(_))) {
            let err = Value::Object(obj(&d, "error").clone());
            let code = err.get("code").filter(|c| c.is_number()).map(value_i64);
            let mut msg = util::first_str(&err, &["message"]);
            if msg.is_empty() {
                msg = "Navidrome/Subsonic error".into();
            }
            if code.is_some_and(|c| SUBSONIC_AUTH.contains(&c)) {
                return Err(NavidromeError::new(
                    "Navidrome authentication failed",
                    None,
                    &msg,
                    NAVIDROME_REMEDY,
                ));
            }
            return Err(NavidromeError::new("Navidrome error", None, &msg, ""));
        }
        Ok(d)
    }

    /// Probe a failed stream URL and return its HTTP status (None if unreachable).
    pub fn stream_status(&self, url: &str, timeout: u64) -> Option<u16> {
        util::http_status(
            url,
            timeout,
            &format!("NPLAY/{}", crate::VERSION),
            &[("Range", "bytes=0-0")],
        )
    }

    pub fn ping(&self) -> NavResult<bool> {
        self.call("ping", vec![])?;
        Ok(true)
    }

    /// `{"version": ...}` of the server.
    pub fn stats(&self) -> NavResult<Meta> {
        let d = self.call("getIndexes", vec![])?;
        let mut m = Meta::new();
        m.insert(
            "version".into(),
            d.get("version")
                .cloned()
                .unwrap_or_else(|| "Subsonic-compatible".into()),
        );
        Ok(m)
    }

    fn song_track(&self, x: &Value, cover_fallback: &str) -> Track {
        let mut meta = Meta::new();
        meta.insert("suffix".into(), raw(x, "suffix", "".into()));
        meta.insert("bitrate".into(), raw(x, "bitRate", 0.into()));
        let cover = util::first_str(x, &["coverArt"]);
        Track {
            source: "navidrome".into(),
            id: s(x, "id"),
            title: s(x, "title"),
            artist: s(x, "artist"),
            album: s(x, "album"),
            duration: x
                .get("duration")
                .map(crate::model::value_f64)
                .unwrap_or(0.0),
            url: self.stream(&s(x, "id")),
            cover: self.cover(if cover.is_empty() {
                cover_fallback
            } else {
                &cover
            }),
            kind: "track".into(),
            seekable: true,
            meta,
            ..Default::default()
        }
    }

    fn album_track(&self, a: &Value, artist: Option<&str>) -> Track {
        let mut meta = Meta::new();
        meta.insert("album_id".into(), Value::String(s(a, "id")));
        meta.insert("song_count".into(), raw(a, "songCount", 0.into()));
        meta.insert("year".into(), raw(a, "year", Value::Null));
        Track {
            source: "navidrome".into(),
            id: s(a, "id"),
            title: s(a, "name"),
            artist: artist.map(str::to_string).unwrap_or_else(|| s(a, "artist")),
            album: s(a, "name"),
            cover: self.cover(&s(a, "coverArt")),
            kind: "album".into(),
            seekable: false,
            meta,
            ..Default::default()
        }
    }

    fn artist_track(&self, a: &Value, with_cover: bool) -> Track {
        let mut meta = Meta::new();
        meta.insert("artist_id".into(), Value::String(s(a, "id")));
        Track {
            source: "navidrome".into(),
            id: s(a, "id"),
            title: s(a, "name"),
            artist: "Artist".into(),
            kind: "artist".into(),
            seekable: false,
            cover: if with_cover {
                self.cover(&s(a, "coverArt"))
            } else {
                String::new()
            },
            meta,
            ..Default::default()
        }
    }

    pub fn recent(&self, limit: usize) -> NavResult<Vec<Track>> {
        let d = self.call("getAlbumList2", vec![p("type", "newest"), p("size", limit)])?;
        let mut out = Vec::new();
        for a in arr(&Value::Object(obj(&d, "albumList2").clone()), "album") {
            let Ok(dir) = self.call("getMusicDirectory", vec![p("id", s(a, "id"))]) else {
                continue;
            };
            let dir = Value::Object(obj(&dir, "directory").clone());
            out.extend(
                arr(&dir, "child")
                    .iter()
                    .filter(|x| !util::truthy(x, "isDir"))
                    .map(|x| self.song_track(x, "")),
            );
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Stream URL (carries credentials; `maxBitRate` when a bitrate is set).
    pub fn stream(&self, id: &str) -> String {
        let mut extra = vec![p("id", id)];
        if self.bitrate != 0 {
            extra.push(p("maxBitRate", self.bitrate));
        }
        self.url_for("stream", extra)
    }

    pub fn song(&self, id: &str) -> NavResult<Track> {
        let d = self.call("getSong", vec![p("id", id)])?;
        match d.get("song").filter(|x| crate::model::value_bool(x)) {
            Some(x) => Ok(self.song_track(x, "")),
            None => Err(NavidromeError::new(
                "Navidrome track no longer exists",
                None,
                "",
                "",
            )),
        }
    }

    /// Cover-art URL ("" for an empty id).
    pub fn cover(&self, id: &str) -> String {
        if id.is_empty() {
            return String::new();
        }
        self.url_for("getCoverArt", vec![p("id", id)])
    }

    pub fn albums(&self, kind: &str, limit: usize) -> NavResult<Vec<Track>> {
        let d = self.call("getAlbumList2", vec![p("type", kind), p("size", limit)])?;
        let l = Value::Object(obj(&d, "albumList2").clone());
        Ok(arr(&l, "album")
            .iter()
            .map(|a| self.album_track(a, None))
            .collect())
    }

    pub fn album_tracks(&self, album_id: &str) -> NavResult<Vec<Track>> {
        let d = self.call("getAlbum", vec![p("id", album_id)])?;
        let a = Value::Object(obj(&d, "album").clone());
        let fallback = s(&a, "coverArt");
        Ok(arr(&a, "song")
            .iter()
            .map(|x| self.song_track(x, &fallback))
            .collect())
    }

    /// Artists, then albums, then songs.
    pub fn search_library(
        &self,
        q: &str,
        songs: usize,
        albums: usize,
        artists: usize,
    ) -> NavResult<Vec<Track>> {
        let d = self.call(
            "search3",
            vec![
                p("query", q),
                p("songCount", songs),
                p("albumCount", albums),
                p("artistCount", artists),
            ],
        )?;
        let r = Value::Object(obj(&d, "searchResult3").clone());
        let mut out: Vec<Track> = arr(&r, "artist")
            .iter()
            .map(|a| self.artist_track(a, true))
            .collect();
        out.extend(arr(&r, "album").iter().map(|a| self.album_track(a, None)));
        out.extend(arr(&r, "song").iter().map(|x| self.song_track(x, "")));
        Ok(out)
    }

    pub fn artists(&self) -> NavResult<Vec<Track>> {
        let d = self.call("getArtists", vec![])?;
        let a = Value::Object(obj(&d, "artists").clone());
        Ok(arr(&a, "index")
            .iter()
            .flat_map(|idx| arr(idx, "artist").iter())
            .map(|x| self.artist_track(x, false))
            .collect())
    }

    pub fn artist_albums(&self, artist_id: &str) -> NavResult<Vec<Track>> {
        let d = self.call("getArtist", vec![p("id", artist_id)])?;
        let a = Value::Object(obj(&d, "artist").clone());
        let name = s(&a, "name");
        Ok(arr(&a, "album")
            .iter()
            .map(|x| self.album_track(x, Some(&name)))
            .collect())
    }

    pub fn playlists(&self) -> NavResult<Vec<Track>> {
        let d = self.call("getPlaylists", vec![])?;
        let l = Value::Object(obj(&d, "playlists").clone());
        Ok(arr(&l, "playlist")
            .iter()
            .map(|x| {
                let mut meta = Meta::new();
                meta.insert("playlist_id".into(), Value::String(s(x, "id")));
                meta.insert("song_count".into(), raw(x, "songCount", 0.into()));
                Track {
                    source: "navidrome".into(),
                    id: s(x, "id"),
                    title: s(x, "name"),
                    artist: "Playlist".into(),
                    kind: "playlist".into(),
                    seekable: false,
                    meta,
                    ..Default::default()
                }
            })
            .collect())
    }

    pub fn playlist_tracks(&self, pid: &str) -> NavResult<Vec<Track>> {
        let d = self.call("getPlaylist", vec![p("id", pid)])?;
        let pl = Value::Object(obj(&d, "playlist").clone());
        Ok(arr(&pl, "entry")
            .iter()
            .map(|x| self.song_track(x, ""))
            .collect())
    }

    /// Songs only.
    pub fn search(&self, q: &str, limit: usize) -> NavResult<Vec<Track>> {
        let d = self.call(
            "search3",
            vec![
                p("query", q),
                p("songCount", limit),
                p("albumCount", 0),
                p("artistCount", 0),
            ],
        )?;
        let r = Value::Object(obj(&d, "searchResult3").clone());
        Ok(arr(&r, "song")
            .iter()
            .map(|x| self.song_track(x, ""))
            .collect())
    }

    pub fn random_tracks(&self, limit: usize) -> NavResult<Vec<Track>> {
        let d = self.call("getRandomSongs", vec![p("size", limit)])?;
        let r = Value::Object(obj(&d, "randomSongs").clone());
        Ok(arr(&r, "song")
            .iter()
            .map(|x| self.song_track(x, ""))
            .collect())
    }

    /// Every track of an artist, albums ordered by (year, name).
    pub fn artist_tracks(&self, artist_id: &str, shuffle: bool) -> NavResult<Vec<Track>> {
        let d = self.call("getArtist", vec![p("id", artist_id)])?;
        let a = Value::Object(obj(&d, "artist").clone());
        let mut albums: Vec<&Value> = arr(&a, "album").iter().collect();
        albums.sort_by_key(|x| {
            (
                x.get("year").map(value_i64).unwrap_or(0),
                util::casefold(&s(x, "name")),
            )
        });
        let mut out = Vec::new();
        for album in albums {
            out.extend(self.album_tracks(&s(album, "id"))?);
        }
        if shuffle {
            fastrand::shuffle(&mut out);
        }
        Ok(out)
    }

    /// A random artist's name and tracks (`("", [])` without artists).
    pub fn random_artist_tracks(&self) -> NavResult<(String, Vec<Track>)> {
        let artists = self.artists()?;
        if artists.is_empty() {
            return Ok((String::new(), vec![]));
        }
        let artist = &artists[fastrand::usize(..artists.len())];
        let id = artist.meta_str("artist_id");
        let id = if id.is_empty() { artist.id.clone() } else { id };
        Ok((artist.title.clone(), self.artist_tracks(&id, false)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::{contains_secret, parse_qsl, redact_url, SECRET_KEYS};
    use serde_json::json;
    use std::sync::Mutex;

    const PASSWORD: &str = "correct horse battery staple";

    fn secrets_of(url: &str) -> Vec<String> {
        let q = url.split_once('?').map(|x| x.1).unwrap_or("");
        parse_qsl(q)
            .into_iter()
            .filter(|(k, v)| SECRET_KEYS.contains(&k.as_str()) && !v.is_empty())
            .map(|x| x.1)
            .collect()
    }

    fn fixed(reply: Value) -> (Fetcher, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let f: Fetcher = Arc::new(move |u: &str, _t: u64| {
            s2.lock().unwrap().push(u.to_string());
            Ok(serde_json::to_vec(&reply).unwrap())
        });
        (f, seen)
    }

    #[test]
    fn token_auth_is_salted_md5() {
        let n = Navidrome::new("https://m.example/", "niru", PASSWORD, 0);
        assert_eq!(n.url, "https://m.example");
        let ps = n.params();
        let get = |k: &str| ps.iter().find(|x| x.0 == k).unwrap().1.clone();
        let mut h = Md5::new();
        h.update(format!("{PASSWORD}{}", get("s")).as_bytes());
        let want: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(get("t"), want);
        assert_eq!(get("s").len(), 12);
        assert_eq!(
            ps.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["u", "t", "s", "v", "c", "f"]
        );
        assert_eq!(get("v"), "1.16.1");
        assert_ne!(n.params()[2].1, get("s"));
        assert!(!format!("{n:?}").contains(PASSWORD));
    }

    #[test]
    fn provider_urls_are_redacted() {
        let nav = Navidrome::new("https://music.example", "niru", PASSWORD, 0);
        let br = Navidrome::new("https://m.example/", "u", PASSWORD, 192).stream("x");
        assert!(br.contains("maxBitRate=192"));
        assert!(br.starts_with("https://m.example/rest/stream.view?u=u&t="));
        for url in [nav.stream("tr-1"), nav.cover("al-1"), br] {
            let values = secrets_of(&url);
            assert!(!values.is_empty());
            for out in [
                redact_url(&url),
                redact_text(&format!("start source={url} done")),
            ] {
                for v in &values {
                    assert!(!out.contains(v.as_str()));
                }
                assert!(!contains_secret(&out));
                assert!(out.contains("REDACTED"));
            }
            assert!(redact_url(&url).contains(url.split('?').next().unwrap()));
            assert!(!redact_url(&url).contains("?u="));
            assert!(!redact_text(&url).contains("&u="));
        }
        assert_eq!(nav.cover(""), "");
        let once = redact_text(&nav.stream("1"));
        assert_eq!(redact_text(&once), once);
    }

    #[test]
    fn http_error_shows_status_and_remedy_without_url() {
        let f: Fetcher = Arc::new(|_u: &str, _t: u64| Err(HttpError::Status(401)));
        let nav = Navidrome::new("https://music.example", "niru", PASSWORD, 0).with_fetcher(f);
        let e = nav.ping().unwrap_err();
        let text = e.to_string();
        assert!(text.contains("HTTP 401"));
        assert!(text.contains("Settings → Navidrome"));
        assert!(!text.contains("music.example"));
        assert!(!text.contains("t="));
        assert_eq!(e.status, Some(401));
        assert_eq!(text, "Navidrome request failed (HTTP 401) · check the URL and password in Settings → Navidrome");
    }

    #[test]
    fn subsonic_auth_failure_and_plain_error() {
        let (f, _) = fixed(
            json!({"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "Wrong username or password"}}}),
        );
        let nav = Navidrome::new("https://music.example", "niru", "wrong", 0).with_fetcher(f);
        let t = nav.ping().unwrap_err().to_string();
        assert_eq!(t, "Navidrome authentication failed · Wrong username or password · check the URL and password in Settings → Navidrome");
        let ce: crate::core::Error = nav.ping().unwrap_err().into();
        assert_eq!(ce.kind, crate::core::ErrorKind::Authentication);
        let (f, _) =
            fixed(json!({"subsonic-response": {"status": "failed", "error": {"code": 70}}}));
        let nav = nav.with_fetcher(f);
        assert_eq!(
            nav.ping().unwrap_err().to_string(),
            "Navidrome error · Navidrome/Subsonic error"
        );
        let bad: Fetcher = Arc::new(|_u: &str, _t: u64| Ok(b"<html>".to_vec()));
        let nav = nav.with_fetcher(bad);
        assert!(nav
            .ping()
            .unwrap_err()
            .to_string()
            .starts_with("Navidrome returned an invalid response"));
    }

    #[test]
    fn unreachable_is_credential_free() {
        let f: Fetcher = Arc::new(|u: &str, _t: u64| {
            Err(HttpError::Unreachable(format!(
                "Name or service not known {u}"
            )))
        });
        let nav = Navidrome::new("https://music.example", "niru", PASSWORD, 0).with_fetcher(f);
        let t = nav.ping().unwrap_err().to_string();
        assert!(t.contains("unreachable"));
        assert!(!contains_secret(&t));
        assert!(!t.contains("correct"));
    }

    #[test]
    fn real_http_status_maps_to_error() {
        use std::io::{Read as _, Write as _};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let _ = c.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let nav = Navidrome::new(&format!("http://127.0.0.1:{port}"), "niru", PASSWORD, 0);
        let e = nav.ping().unwrap_err();
        assert_eq!(e.status, Some(503));
        assert_eq!(nav.stream_status(&nav.stream("1"), 3), Some(503));
        h.join().unwrap();
        let dead = Navidrome::new("http://127.0.0.1:9", "niru", PASSWORD, 0);
        assert!(dead.ping().unwrap_err().to_string().contains("unreachable"));
        assert_eq!(dead.stream_status("http://127.0.0.1:9/x", 2), None);
    }

    #[test]
    fn stream_failure_messages() {
        let t = navidrome_stream_failure(Some(401)).to_string();
        assert!(t.contains("HTTP 401"));
        assert!(t.contains("check the URL and password in Settings → Navidrome"));
        assert!(navidrome_stream_failure(Some(503))
            .to_string()
            .contains("HTTP 503"));
        assert!(navidrome_stream_failure(None)
            .to_string()
            .contains("unreachable"));
        assert!(navidrome_stream_failure(Some(200))
            .to_string()
            .contains("mpv.log"));
    }

    #[test]
    fn track_mapping() {
        let reply = json!({"subsonic-response": {"status": "ok", "searchResult3": {
            "artist": [{"id": "ar1", "name": "Art", "coverArt": "ca"}],
            "album": [{"id": 7, "name": "Alb", "artist": "Art", "songCount": 3, "year": 1999, "coverArt": "al-7"}],
            "song": [{"id": "s1", "title": "Song", "artist": "Art", "album": "Alb", "duration": 201, "suffix": "flac", "bitRate": 900, "coverArt": "al-7"}]
        }}});
        let (f, seen) = fixed(reply);
        let nav = Navidrome::new("https://m.example", "niru", PASSWORD, 0).with_fetcher(f);
        let xs = nav.search_library("art", 30, 15, 10).unwrap();
        assert!(seen.lock().unwrap()[0].contains("/rest/search3.view?u=niru&t="));
        assert!(seen.lock().unwrap()[0]
            .ends_with("&query=art&songCount=30&albumCount=15&artistCount=10"));
        assert_eq!(
            (xs[0].kind.as_str(), xs[0].artist.as_str()),
            ("artist", "Artist")
        );
        assert_eq!(xs[0].meta["artist_id"], "ar1");
        assert!(!xs[0].seekable);
        assert_eq!(xs[1].id, "7");
        assert_eq!(xs[1].meta["album_id"], "7");
        assert_eq!(xs[1].meta["song_count"], 3);
        assert_eq!(xs[1].meta["year"], 1999);
        let s1 = &xs[2];
        assert_eq!(
            (s1.kind.as_str(), s1.duration, s1.seekable),
            ("track", 201.0, true)
        );
        assert_eq!(s1.meta["suffix"], "flac");
        assert_eq!(s1.meta["bitrate"], 900);
        assert!(s1.url.contains("stream.view") && s1.url.ends_with("&id=s1"));
        assert!(s1.cover.contains("getCoverArt.view") && s1.cover.ends_with("&id=al-7"));
    }

    #[test]
    fn artist_tracks_order_and_album_cover_fallback() {
        let f: Fetcher = Arc::new(|u: &str, _t: u64| {
            let v = if u.contains("/getArtist.view") {
                json!({"subsonic-response": {"status": "ok", "artist": {"name": "A", "album": [
                    {"id": "b", "name": "beta", "year": 2001}, {"id": "a", "name": "Alpha", "year": 2001}, {"id": "c", "name": "old", "year": "1990"}]}}})
            } else {
                let id = u.rsplit("id=").next().unwrap().to_string();
                json!({"subsonic-response": {"status": "ok", "album": {"coverArt": format!("cov-{id}"), "song": [{"id": format!("{id}1"), "title": id}]}}})
            };
            Ok(serde_json::to_vec(&v).unwrap())
        });
        let nav = Navidrome::new("https://m.example", "u", PASSWORD, 0).with_fetcher(f);
        let xs = nav.artist_tracks("ar", false).unwrap();
        assert_eq!(
            xs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["c1", "a1", "b1"]
        );
        assert!(xs[0].cover.ends_with("id=cov-c"));
        let albums = nav.artist_albums("ar").unwrap();
        assert_eq!(albums[0].artist, "A");
    }
}
