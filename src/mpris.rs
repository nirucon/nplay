//! Optional MPRIS2 bridge (port of `nplay/mpris.py`), zbus blocking API.
//!
//! Bus name `org.mpris.MediaPlayer2.nplay`, object `/org/mpris/MediaPlayer2`,
//! interfaces `org.mpris.MediaPlayer2` (Identity `NPLAY`, DesktopEntry
//! `nplay`, CanQuit, no Raise, no track list, URI schemes file/http/https)
//! and `org.mpris.MediaPlayer2.Player` (PlayPause, Play, Pause, Next,
//! Previous, Stop, Seek, SetPosition; PlaybackStatus, LoopStatus, Rate,
//! Shuffle, Metadata, Volume (settable), Position, Minimum/MaximumRate,
//! CanGoNext/Previous/Play/Pause/Seek/Control). It does not depend on the app:
//! the engine implements `MprisControl`.
//!
//! A credential-bearing cover URL (Navidrome) is never published on D-Bus: it
//! is fetched into the private artwork cache and announced as `file://` (the
//! Metadata change is emitted once the download finishes).
//!
//! No session bus never prevents start: `start()` returns false and
//! `status()` explains. Another NPLAY owning the name is reported as a healthy
//! external instance. Threads: zbus's executor (plus async-io's reactor);
//! signals are emitted as tasks on that executor, so callers never block.
//!
//! Public API:
//! - trait `MprisControl: Send + Sync`: `play_pause`, `next`, `previous`,
//!   `stop`, `seek_relative(secs)`, `volume_relative(delta_percent)`, `quit`,
//!   `snapshot() -> MprisState`.
//! - `MprisState`: what the properties are computed from (current track,
//!   its SR radio info, playing/stopped, repeat/shuffle modes, volume percent,
//!   position/duration seconds, can_go_next/previous).
//! - `Mpris::new(control, cache)`, `start() -> bool`, `stop()`,
//!   `changed(&[names])` (empty = PlaybackStatus, Metadata, Volume,
//!   LoopStatus, Shuffle), `available()`, `external()`, `status()`.
//! - `build_metadata(state, cache) -> (Option<Metadata>, Option<fetch_url>)`
//!   and `playback_status(state)`, `loop_status(mode)`: pure helpers.

use crate::artwork_cache::ArtworkCache;
use crate::model::Track;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zbus::zvariant::{ObjectPath, OwnedValue, Value as ZValue};

pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.nplay";
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const DEFAULT_CHANGED: &[&str] = &["PlaybackStatus", "Metadata", "Volume", "LoopStatus", "Shuffle"];

/// What the engine exposes to MPRIS.
pub trait MprisControl: Send + Sync {
    fn play_pause(&self);
    fn next(&self);
    fn previous(&self);
    fn stop(&self);
    fn seek_relative(&self, secs: f64);
    fn volume_relative(&self, delta_percent: f64);
    fn quit(&self);
    fn snapshot(&self) -> MprisState;
}

/// Snapshot of the player state (1.x read these from the app directly).
#[derive(Clone, Debug, Default)]
pub struct MprisState {
    pub track: Option<Track>,
    /// `app.radio_info[track.id]` (`program`, `song`, `episode`...).
    pub radio_info: Map<String, Value>,
    /// `playback.active()`.
    pub playing: bool,
    /// `playback_state["state"] == "stopped"`.
    pub stopped: bool,
    /// `off`, `track` or `context`.
    pub repeat_mode: String,
    /// `off`, `shuffle` or `smart`.
    pub shuffle_mode: String,
    /// App volume in percent (0..150).
    pub volume: f64,
    pub position: f64,
    /// `playback.duration()`; 0 falls back to the track's duration.
    pub duration: f64,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

/// The MPRIS metadata map before D-Bus encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub trackid: String,
    pub title: String,
    pub artist: Vec<String>,
    pub album: String,
    pub length: Option<i64>,
    pub art_url: Option<String>,
}

pub fn playback_status(st: &MprisState) -> &'static str {
    if st.playing {
        "Playing"
    } else if st.track.is_some() && !st.stopped {
        "Paused"
    } else {
        "Stopped"
    }
}

pub fn loop_status(mode: &str) -> &'static str {
    match mode {
        "track" => "Track",
        "context" => "Playlist",
        _ => "None",
    }
}

fn info(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).map(crate::model::value_str).unwrap_or_default()
}

fn first_nonempty(xs: &[&str]) -> String {
    xs.iter().find(|s| !s.is_empty()).copied().unwrap_or("").to_string()
}

/// Metadata for the current track and, when the cover carries credentials
/// and is not cached yet, the URL to fetch into the cache.
pub fn build_metadata(st: &MprisState, cache: &ArtworkCache) -> (Option<Metadata>, Option<String>) {
    let Some(t) = &st.track else {
        return (None, None);
    };
    let (mut title, mut artist, mut album) = (t.title.clone(), t.artist.clone(), t.album.clone());
    if t.kind == "radio" {
        let i = &st.radio_info;
        title = first_nonempty(&[&info(i, "program"), &info(i, "song"), &t.title, "Live radio"]);
        artist = first_nonempty(&[&t.title, &t.artist]);
        album = if t.source == "sr" {
            "Sveriges Radio · LIVE".into()
        } else {
            "Live radio".into()
        };
    }
    // D-Bus object paths allow only [A-Za-z0-9_] per element.
    let ident: String = format!("{}_{}", t.source, t.id)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trackid = format!(
        "/org/nplay/track/{}",
        if ident.is_empty() { "current" } else { &ident }
    );
    let length = (t.kind != "radio").then(|| {
        let d = if st.duration > 0.0 { st.duration } else { t.duration };
        (d.max(0.0) * 1_000_000.0) as i64
    });
    let mut fetch = None;
    let mut art = String::new();
    if !t.cover.is_empty() {
        art = match cache.cached(&t.cover) {
            Some(p) => format!("file://{}", p.display()),
            None => t.cover.clone(),
        };
    }
    if !art.is_empty() && crate::redact::contains_secret(&art) {
        art.clear();
        fetch = Some(t.cover.clone());
    }
    let art_url = (!art.is_empty()).then(|| {
        if art.starts_with("file://") || art.starts_with("http://") || art.starts_with("https://") {
            art
        } else {
            format!("file://{art}")
        }
    });
    (
        Some(Metadata {
            trackid,
            title,
            artist: if artist.is_empty() { vec![] } else { vec![artist] },
            album,
            length,
            art_url,
        }),
        fetch,
    )
}

fn ov<'a>(v: impl Into<ZValue<'a>>) -> Option<OwnedValue> {
    v.into().try_to_owned().ok()
}

fn encode_metadata(m: &Option<Metadata>) -> HashMap<String, OwnedValue> {
    let mut out = HashMap::new();
    let Some(m) = m else {
        return out;
    };
    let mut put = |k: &str, v: Option<OwnedValue>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v);
        }
    };
    put(
        "mpris:trackid",
        ObjectPath::try_from(m.trackid.clone()).ok().and_then(ov),
    );
    put("xesam:title", ov(m.title.clone()));
    put("xesam:artist", ov(m.artist.clone()));
    put("xesam:album", ov(m.album.clone()));
    if let Some(l) = m.length {
        put("mpris:length", ov(l));
    }
    if let Some(a) = &m.art_url {
        put("mpris:artUrl", ov(a.clone()));
    }
    out
}

struct Shared {
    control: Arc<dyn MprisControl>,
    cache: ArtworkCache,
    conn: Mutex<Option<zbus::blocking::Connection>>,
    available: AtomicBool,
    external: AtomicBool,
    reason: Mutex<String>,
    art_pending: Mutex<String>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    fn metadata(self: &Arc<Self>, st: &MprisState) -> HashMap<String, OwnedValue> {
        let (m, fetch) = build_metadata(st, &self.cache);
        if let Some(url) = fetch {
            self.fetch_art(url);
        }
        encode_metadata(&m)
    }

    fn fetch_art(self: &Arc<Self>, url: String) {
        {
            let mut p = lock(&self.art_pending);
            if *p == url {
                return;
            }
            *p = url.clone();
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("nplay-mpris-art".into())
            .spawn(move || {
                if me.cache.fetch(&url, 8.0, &crate::artwork_cache::user_agent()).is_some() {
                    Mpris { shared: me }.changed(&["Metadata"]);
                }
            });
    }

    fn prop(self: &Arc<Self>, name: &str, st: &MprisState) -> Option<OwnedValue> {
        let t = st.track.as_ref();
        match name {
            "PlaybackStatus" => ov(playback_status(st)),
            "LoopStatus" => ov(loop_status(&st.repeat_mode)),
            "Rate" | "MinimumRate" | "MaximumRate" => ov(1.0f64),
            "Shuffle" => ov(st.shuffle_mode != "off"),
            "Metadata" => ov(ZValue::from(self.metadata(st))),
            "Volume" => ov((st.volume / 100.0).clamp(0.0, 1.5)),
            "Position" => ov((st.position * 1_000_000.0) as i64),
            "CanGoNext" => ov(st.can_go_next),
            "CanGoPrevious" => ov(st.can_go_previous),
            "CanPlay" | "CanPause" | "CanControl" => ov(true),
            "CanSeek" => ov(t.is_some_and(|t| t.seekable)),
            _ => None,
        }
    }
}

struct Root {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    fn quit(&self) {
        self.shared.control.quit();
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        "NPLAY".into()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "nplay".into()
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["file".into(), "http".into(), "https".into()]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}

struct PlayerIface {
    shared: Arc<Shared>,
}

impl PlayerIface {
    fn st(&self) -> MprisState {
        self.shared.control.snapshot()
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerIface {
    fn play_pause(&self) {
        self.shared.control.play_pause();
    }
    fn play(&self) {
        if !self.st().playing {
            self.shared.control.play_pause();
        }
    }
    fn pause(&self) {
        if self.st().playing {
            self.shared.control.play_pause();
        }
    }
    fn next(&self) {
        self.shared.control.next();
    }
    fn previous(&self) {
        self.shared.control.previous();
    }
    fn stop(&self) {
        self.shared.control.stop();
    }
    fn seek(&self, offset: i64) {
        self.shared.control.seek_relative(offset as f64 / 1_000_000.0);
    }
    fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
        let st = self.st();
        if st.track.as_ref().is_some_and(|t| t.seekable) {
            let target = (position as f64 / 1_000_000.0).max(0.0);
            self.shared.control.seek_relative(target - st.position);
        }
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        playback_status(&self.st()).into()
    }
    #[zbus(property)]
    fn loop_status(&self) -> String {
        loop_status(&self.st().repeat_mode).into()
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.st().shuffle_mode != "off"
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.shared.metadata(&self.st())
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        (self.st().volume / 100.0).clamp(0.0, 1.5)
    }
    #[zbus(property)]
    fn set_volume(&self, value: f64) {
        if value.is_finite() {
            let cur = self.st().volume;
            self.shared.control.volume_relative(value * 100.0 - cur);
        }
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        (self.st().position * 1_000_000.0) as i64
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.st().can_go_next
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.st().can_go_previous
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.st().track.as_ref().is_some_and(|t| t.seekable)
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// The MPRIS service handle (`Clone + Send + Sync`).
#[derive(Clone)]
pub struct Mpris {
    shared: Arc<Shared>,
}

impl Mpris {
    pub fn new(control: Arc<dyn MprisControl>, cache: ArtworkCache) -> Mpris {
        Mpris {
            shared: Arc::new(Shared {
                control,
                cache,
                conn: Mutex::new(None),
                available: AtomicBool::new(false),
                external: AtomicBool::new(false),
                reason: Mutex::new(String::new()),
                art_pending: Mutex::new(String::new()),
            }),
        }
    }

    pub fn available(&self) -> bool {
        self.shared.available.load(Ordering::SeqCst)
    }

    pub fn external(&self) -> bool {
        self.shared.external.load(Ordering::SeqCst)
    }

    pub fn reason(&self) -> String {
        lock(&self.shared.reason).clone()
    }

    pub fn status(&self) -> String {
        if self.available() {
            return if self.external() {
                "READY · external instance".into()
            } else {
                "READY".into()
            };
        }
        let r = self.reason();
        if r.is_empty() {
            "unavailable".into()
        } else {
            r
        }
    }

    fn fail(&self, reason: String) -> bool {
        *lock(&self.shared.reason) = reason;
        self.shared.available.store(false, Ordering::SeqCst);
        false
    }

    /// Connect to the session bus and serve the MPRIS objects. Never panics
    /// and never blocks long; without a session bus it returns false.
    pub fn start(&self) -> bool {
        if self.available() {
            return true;
        }
        let conn = match zbus::blocking::connection::Builder::session().and_then(|b| b.build()) {
            Ok(c) => c,
            Err(e) => return self.fail(format!("D-Bus session unavailable: {e}")),
        };
        let owned = zbus::blocking::fdo::DBusProxy::new(&conn).and_then(|p| {
            p.name_has_owner(zbus::names::BusName::try_from(BUS_NAME).map_err(zbus::Error::from)?)
                .map_err(zbus::Error::from)
        });
        match owned {
            Ok(true) => {
                // Another NPLAY owns the single MPRIS name: healthy, external.
                self.shared.available.store(true, Ordering::SeqCst);
                self.shared.external.store(true, Ordering::SeqCst);
                *lock(&self.shared.reason) = "another NPLAY instance owns MPRIS".into();
                return true;
            }
            Ok(false) => {}
            Err(e) => return self.fail(format!("D-Bus session unavailable: {e}")),
        }
        let served = (|| -> zbus::Result<()> {
            let os = conn.object_server();
            os.at(
                OBJECT_PATH,
                Root {
                    shared: self.shared.clone(),
                },
            )?;
            os.at(
                OBJECT_PATH,
                PlayerIface {
                    shared: self.shared.clone(),
                },
            )?;
            drop(os);
            use zbus::fdo::{RequestNameFlags, RequestNameReply};
            match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())? {
                RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(()),
                _ => Err(zbus::Error::NameTaken),
            }
        })();
        if let Err(e) = served {
            return self.fail(format!("MPRIS service failed: {e}"));
        }
        *lock(&self.shared.conn) = Some(conn);
        *lock(&self.shared.reason) = String::new();
        self.shared.external.store(false, Ordering::SeqCst);
        self.shared.available.store(true, Ordering::SeqCst);
        info!("MPRIS ready as {BUS_NAME}");
        true
    }

    /// Emit `PropertiesChanged` for the given Player properties (none: the
    /// 1.x default set). Non-blocking: the signal is sent by zbus's executor.
    pub fn changed(&self, names: &[&str]) {
        if !self.available() || self.external() {
            return;
        }
        let Some(conn) = lock(&self.shared.conn).clone() else {
            return;
        };
        let names = if names.is_empty() { DEFAULT_CHANGED } else { names };
        let st = self.shared.control.snapshot();
        let mut changed: HashMap<String, OwnedValue> = HashMap::new();
        for n in names {
            if let Some(v) = self.shared.prop(n, &st) {
                changed.insert((*n).to_string(), v);
            }
        }
        if changed.is_empty() {
            return;
        }
        let c = conn.inner().clone();
        let c2 = c.clone();
        c.executor()
            .spawn(
                async move {
                    let _ = c2
                        .emit_signal(
                            None::<zbus::names::BusName<'_>>,
                            OBJECT_PATH,
                            "org.freedesktop.DBus.Properties",
                            "PropertiesChanged",
                            &(PLAYER_IFACE, changed, Vec::<String>::new()),
                        )
                        .await;
                },
                "nplay-mpris-changed",
            )
            .detach();
    }

    /// Release the name and drop the connection (breaks the object cycle).
    pub fn stop(&self) {
        let conn = lock(&self.shared.conn).take();
        if let Some(c) = conn {
            let _ = c.release_name(BUS_NAME);
            let os = c.object_server();
            let _ = os.remove::<PlayerIface, _>(OBJECT_PATH);
            let _ = os.remove::<Root, _>(OBJECT_PATH);
        }
        self.shared.available.store(false, Ordering::SeqCst);
        self.shared.external.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn cache(name: &str) -> ArtworkCache {
        ArtworkCache::with_root(
            std::env::temp_dir().join(format!(
                "nplay-mpris-{name}-{}-{:x}",
                std::process::id(),
                fastrand::u64(..)
            )),
            500,
        )
    }

    fn song() -> Track {
        Track {
            artist: "Artist".into(),
            album: "Album".into(),
            duration: 200.0,
            ..Track::new("navidrome", "a-1/ö", "Song")
        }
    }

    #[test]
    fn status_and_loop() {
        let mut st = MprisState::default();
        assert_eq!(playback_status(&st), "Stopped");
        st.track = Some(song());
        assert_eq!(playback_status(&st), "Paused");
        st.stopped = true;
        assert_eq!(playback_status(&st), "Stopped");
        st.playing = true;
        assert_eq!(playback_status(&st), "Playing");
        assert_eq!(loop_status("track"), "Track");
        assert_eq!(loop_status("context"), "Playlist");
        assert_eq!(loop_status("off"), "None");
    }

    #[test]
    fn metadata_for_tracks_and_radio() {
        let c = cache("meta");
        let mut st = MprisState {
            track: Some(song()),
            duration: 0.0,
            ..Default::default()
        };
        let (m, fetch) = build_metadata(&st, &c);
        let m = m.unwrap();
        assert_eq!(m.trackid, "/org/nplay/track/navidrome_a_1__");
        assert!(ObjectPath::try_from(m.trackid.clone()).is_ok());
        assert_eq!(m.title, "Song");
        assert_eq!(m.artist, vec!["Artist".to_string()]);
        assert_eq!(m.length, Some(200_000_000));
        assert_eq!(m.art_url, None);
        assert!(fetch.is_none());
        st.duration = 12.5;
        assert_eq!(build_metadata(&st, &c).0.unwrap().length, Some(12_500_000));
        let mut r = Track::new("sr", "132", "P1");
        r.kind = "radio".into();
        st.track = Some(r);
        st.radio_info.insert("song".into(), Value::from("Tune"));
        let m = build_metadata(&st, &c).0.unwrap();
        assert_eq!((m.title.as_str(), m.album.as_str()), ("Tune", "Sveriges Radio · LIVE"));
        assert_eq!(m.artist, vec!["P1".to_string()]);
        assert_eq!(m.length, None);
        st.radio_info.insert("program".into(), Value::from("Morgon"));
        assert_eq!(build_metadata(&st, &c).0.unwrap().title, "Morgon");
        let mut x = Track::new("radio", "r", "");
        x.kind = "radio".into();
        st.track = Some(x);
        st.radio_info.clear();
        let m = build_metadata(&st, &c).0.unwrap();
        assert_eq!((m.title.as_str(), m.album.as_str()), ("Live radio", "Live radio"));
        assert!(m.artist.is_empty());
        assert!(build_metadata(&MprisState::default(), &c).0.is_none());
        assert!(encode_metadata(&None).is_empty());
    }

    #[test]
    fn secret_cover_is_never_published() {
        let c = cache("secret");
        let mut t = song();
        t.cover = "https://nav.example/rest/getCoverArt.view?id=al-1&u=me&t=abcdef123456&s=salt".into();
        let st = MprisState {
            track: Some(t.clone()),
            ..Default::default()
        };
        let (m, fetch) = build_metadata(&st, &c);
        assert_eq!(m.unwrap().art_url, None);
        assert_eq!(fetch.as_deref(), Some(t.cover.as_str()));
        // Once cached it is announced as file://.
        let p = c.path(&t.cover).unwrap();
        std::fs::write(&p, b"img").unwrap();
        let (m, fetch) = build_metadata(&st, &c);
        assert_eq!(m.unwrap().art_url, Some(format!("file://{}", p.display())));
        assert!(fetch.is_none());
        // Plain URLs pass through; bare paths become file://.
        let mut t2 = song();
        t2.cover = "https://img.example/c.jpg".into();
        let st2 = MprisState {
            track: Some(t2),
            ..Default::default()
        };
        assert_eq!(
            build_metadata(&st2, &c).0.unwrap().art_url.as_deref(),
            Some("https://img.example/c.jpg")
        );
        let enc = encode_metadata(&build_metadata(&st, &c).0);
        for k in ["mpris:trackid", "xesam:title", "xesam:artist", "xesam:album", "mpris:length", "mpris:artUrl"] {
            assert!(enc.contains_key(k), "{k}");
        }
        let _ = std::fs::remove_dir_all(&c.root);
    }

    struct Ctl {
        calls: AtomicUsize,
        st: Mutex<MprisState>,
    }
    impl MprisControl for Ctl {
        fn play_pause(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut s = lock(&self.st);
            s.playing = !s.playing;
        }
        fn next(&self) {}
        fn previous(&self) {}
        fn stop(&self) {}
        fn seek_relative(&self, secs: f64) {
            lock(&self.st).position += secs;
        }
        fn volume_relative(&self, d: f64) {
            lock(&self.st).volume += d;
        }
        fn quit(&self) {}
        fn snapshot(&self) -> MprisState {
            lock(&self.st).clone()
        }
    }

    /// Full D-Bus round trip when a session bus exists (skipped otherwise).
    /// A private dbus-daemon is used when available so the user's session is
    /// never touched.
    #[test]
    fn serves_on_a_private_bus() {
        if crate::paths::which("dbus-daemon").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("nplay-mpris-bus-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("bus");
        let addr = format!("unix:path={}", sock.display());
        let Ok(mut daemon) = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", &format!("--address={addr}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !sock.exists() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let ctl = Arc::new(Ctl {
            calls: AtomicUsize::new(0),
            st: Mutex::new(MprisState {
                track: Some(song()),
                volume: 80.0,
                position: 10.0,
                ..Default::default()
            }),
        });
        // Build the service connection explicitly on the private bus.
        let m = Mpris::new(ctl.clone(), cache("bus"));
        let conn = zbus::blocking::connection::Builder::address(addr.as_str())
            .unwrap()
            .build()
            .unwrap();
        conn.object_server()
            .at(OBJECT_PATH, Root { shared: m.shared.clone() })
            .unwrap();
        conn.object_server()
            .at(OBJECT_PATH, PlayerIface { shared: m.shared.clone() })
            .unwrap();
        conn.request_name(BUS_NAME).unwrap();
        *lock(&m.shared.conn) = Some(conn);
        m.shared.available.store(true, Ordering::SeqCst);

        let client = zbus::blocking::connection::Builder::address(addr.as_str())
            .unwrap()
            .build()
            .unwrap();
        let root = zbus::blocking::Proxy::new(&client, BUS_NAME, OBJECT_PATH, "org.mpris.MediaPlayer2").unwrap();
        assert_eq!(root.get_property::<String>("Identity").unwrap(), "NPLAY");
        assert_eq!(root.get_property::<String>("DesktopEntry").unwrap(), "nplay");
        let player = zbus::blocking::Proxy::new(&client, BUS_NAME, OBJECT_PATH, PLAYER_IFACE).unwrap();
        assert_eq!(player.get_property::<String>("PlaybackStatus").unwrap(), "Paused");
        player.call_method("PlayPause", &()).unwrap();
        assert_eq!(player.get_property::<String>("PlaybackStatus").unwrap(), "Playing");
        player.call_method("Play", &()).unwrap();
        assert_eq!(ctl.calls.load(Ordering::SeqCst), 1, "Play while playing is a no-op");
        player.call_method("Seek", &(5_000_000i64)).unwrap();
        assert_eq!(lock(&ctl.st).position, 15.0);
        let tid = ObjectPath::try_from("/org/nplay/track/x").unwrap();
        player.call_method("SetPosition", &(tid, 2_000_000i64)).unwrap();
        assert_eq!(lock(&ctl.st).position, 2.0);
        assert_eq!(player.get_property::<f64>("Volume").unwrap(), 0.8);
        player.set_property("Volume", 0.5f64).unwrap();
        assert!((lock(&ctl.st).volume - 50.0).abs() < 1e-9);
        assert_eq!(player.get_property::<i64>("Position").unwrap(), 2_000_000);
        let meta: HashMap<String, OwnedValue> = player.get_property("Metadata").unwrap();
        assert!(meta.contains_key("xesam:title"));
        // PropertiesChanged reaches a listener.
        let props = zbus::blocking::fdo::PropertiesProxy::builder(&client)
            .destination(BUS_NAME)
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .build()
            .unwrap();
        let mut stream = props.receive_properties_changed().unwrap();
        m.changed(&["PlaybackStatus"]);
        let sig = stream.next().unwrap();
        let args = sig.args().unwrap();
        assert_eq!(args.interface_name().as_str(), PLAYER_IFACE);
        assert!(args.changed_properties().contains_key("PlaybackStatus"));
        // A second instance sees the name taken and reports external.
        m.stop();
        assert!(!m.available());
        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_session_bus_does_not_prevent_start() {
        // Only meaningful when no bus is reachable; never alters the env.
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("XDG_RUNTIME_DIR").is_some()
        {
            return;
        }
        let ctl = Arc::new(Ctl {
            calls: AtomicUsize::new(0),
            st: Mutex::new(MprisState::default()),
        });
        let m = Mpris::new(ctl, cache("nobus"));
        assert!(!m.start());
        assert!(m.status().starts_with("D-Bus session unavailable"));
    }
}
