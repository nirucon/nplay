//! Headless NPLAY service backend (port of 1.x `nplay/service/backend.py`).
//! Owner: backend track.
//!
//! The backend reuses the App engine instead of forking playback, queue,
//! provider or statistics behaviour. It only adds a bounded, serialisable
//! view of that state plus explicit commands for local graphical clients.
//!
//! Public API:
//! - [`run_service(path, idle_exit)`](run_service): `nplay --serve`. Builds
//!   the App (MPRIS per `mpris_enabled`), the backend and its [`ServiceUI`],
//!   runs [`crate::service::serve`] (SIGTERM/SIGINT/SIGHUP/`service.shutdown`/
//!   idle exit stop it) and closes the backend, which runs
//!   `App::shutdown()` (checkpoint, session save, librespot close before the
//!   playback stop, MPRIS stop). Returns the process exit code.
//! - [`ServiceBackend::new(app, idle_exit)`](ServiceBackend::new) ->
//!   `Arc<ServiceBackend>`: implements [`crate::service::Backend`] (every
//!   protocol method of docs/PROTOCOL.md, see `dispatch`). Construction drops
//!   legacy container rows (albums, artists, ...) from the restored queue,
//!   context, playlists and favorites, attaches the [`ServiceUI`] with
//!   `App::set_ui` (like 1.x the service does not run the start-up library
//!   refresh) and subscribes to the engine events, each of which becomes a
//!   `state.changed` notification whose `reason` is the event name.
//!   `set_cava_binary(path)` overrides CAVA (tests); `app()` gives the engine.
//! - [`ServiceUI`]: the `app::Ui` shim. `post` runs inline, `status` logs,
//!   `error(title, msg)` becomes a `service.error` notification (`title` 120
//!   and `message` 300 characters, redacted; `code` +
//!   `remedy: spotify.local_authorize` for the local-Spotify-not-authorized
//!   message), `visualizer_on` says whether CAVA exists.
//! - `track_wire(&Track)`: the wire form of a track (`key source id title
//!   artist album duration kind seekable has_art meta`); never a URL, path or
//!   cover; `meta` keeps only the [`META_ALLOW`] keys with scalar values
//!   (strings clipped to 512 characters). Every wired track joins the
//!   bounded reference catalogue (`MAX_TRACKS`, least recently wired out):
//!   requests refer to tracks by `key` (or `source`+`id`) and only catalogue
//!   members resolve.
//!
//! State: `revision` grows on every broadcast and every `playback.play`;
//! `state.changed` carries `revision reason playback sources capabilities
//! server_time`. Reasons: the engine events `playback-state-changed`,
//! `media-changed`, `integration-state-changed`, and `playback`, `position`,
//! `volume`, `repeat`, `shuffle`, `queue`, `settings`, `spotify`, `library`.
//! Other notifications: `library.scanned`, `spotify.auth`,
//! `spotify.local_auth`, `service.error`. The transport sample (mpv
//! position/pause/mute) is cached for 0.45 s except in forced snapshots.
//!
//! Visualization (CAVA lifecycle, pushed frames) lives in `backend::vis`;
//! browse and federated search in `backend::browse`; Python-compatible
//! parameter coercion in `backend::pyval`.

mod browse;
mod pyval;
#[cfg(test)]
mod tests;
mod vis;

use self::pyval::{
    backend_error, clip_chars, flag, float_or, int_or, internal, params_error, py_float,
    py_int, py_str, str_or,
};
use super::protocol::{RpcError, MAX_TRACKS, PROTOCOL_MINOR, PROTOCOL_VERSION};
use super::{Backend, FrameSink, NotifyFn};
use crate::app::{App, Task, Ui, UiRef};
use crate::core::jf;
use crate::model::Track;
use crate::paths::monotonic;
use crate::redact::redact_text;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};

pub use self::vis::{VisHub, VIS_DATA_IDLE, VIS_DATA_RECENT};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Track `meta` keys a client may see.
pub const META_ALLOW: &[&str] = &[
    "suffix",
    "bitrate",
    "codec",
    "track_no",
    "disc_no",
    "year",
    "genre",
    "album_artist",
    "sample_rate",
    "bit_depth",
    "channels",
    "song_count",
    "published_date",
    "published_ts",
    "explicit",
    "album_id",
    "artist_id",
    "playlist_id",
    "program_id",
    "homepage",
    "spotify_url",
    "webpage",
    "release_date",
    "item_count",
    "owner_id",
    "bookmark_pos",
    "local_artist",
];

/// Rows that are not playable media (they must be expanded first).
pub const CONTAINER_KINDS: &[&str] = &["album", "artist", "playlist", "program", "folder", "device"];

/// Settings writable through `settings.set` (sorted, as `settings.get` lists them).
pub const SAFE_SETTINGS: &[&str] = &[
    "custom_radio_enabled",
    "gapless",
    "mpris_enabled",
    "navidrome_enabled",
    "normalization",
    "normalization_mode",
    "notification_artwork",
    "preferred_source",
    "repeat_mode",
    "shuffle_mode",
    "spotify_enabled",
    "sr_enabled",
    "track_notifications",
    "youtube_enabled",
];
pub const BOOL_SETTINGS: &[&str] = &[
    "navidrome_enabled",
    "spotify_enabled",
    "sr_enabled",
    "youtube_enabled",
    "custom_radio_enabled",
    "normalization",
    "gapless",
    "track_notifications",
    "notification_artwork",
    "mpris_enabled",
];

/// Largest artwork file served by `artwork.get`.
pub const MAX_ARTWORK: u64 = 12 * 1024 * 1024;
/// In-memory artwork cache budget (the files stay in the on-disk cache).
const ART_CACHE_BYTES: usize = 4 * 1024 * 1024;
/// Entries above this size are served but not kept in memory.
const ART_CACHE_ENTRY: usize = 1024 * 1024;

pub fn playable_track(t: &Track) -> bool {
    let k = if t.kind.is_empty() { "track" } else { &t.kind };
    !CONTAINER_KINDS.contains(&k)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `{**a, **b}` with `a`'s keys first.
fn merge(a: Value, b: Value) -> Value {
    let mut m = obj(a);
    for (k, v) in obj(b) {
        m.insert(k, v);
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------
// ServiceUI
// ---------------------------------------------------------------------------

/// Small UI shim so the engine runs without a terminal. Callbacks posted from
/// worker threads run in that worker; they only mutate engine state.
pub struct ServiceUI {
    backend: Weak<ServiceBackend>,
}

impl Ui for ServiceUI {
    fn post(&self, task: Task) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err() {
            error!("service UI callback failed");
        }
    }

    fn status(&self, msg: &str) {
        info!("service status: {}", redact_text(msg));
    }

    fn error(&self, title: &str, msg: &str) {
        let title = redact_text(title);
        let message = redact_text(msg);
        warn!("service error: {title} · {message}");
        // Protocol 1.2: asynchronous failures (e.g. a Spotify play that runs
        // in a worker) reach clients instead of only the log.
        let mut params = Map::new();
        params.insert("title".into(), Value::from(clip_chars(&title, 120)));
        params.insert("message".into(), Value::from(clip_chars(&message, 300)));
        if message.contains(crate::spotify::NOT_AUTHORIZED_MESSAGE) {
            params.insert(
                "code".into(),
                Value::from(crate::spotify::NOT_AUTHORIZED_CODE),
            );
            params.insert("remedy".into(), Value::from("spotify.local_authorize"));
        }
        if let Some(b) = self.backend.upgrade() {
            b.notify("service.error", Value::Object(params));
        }
    }

    fn visualizer_on(&self) -> bool {
        self.backend.upgrade().is_some_and(|b| b.vis.available())
    }
}

// ---------------------------------------------------------------------------
// Backend state
// ---------------------------------------------------------------------------

/// Bounded LRU of wired tracks (1.x `_catalog` OrderedDict).
#[derive(Default)]
struct Catalog {
    map: HashMap<String, (u64, Track)>,
    order: BTreeMap<u64, String>,
    seq: u64,
}

impl Catalog {
    fn remember(&mut self, t: &Track) {
        let key = t.key();
        if let Some((s, _)) = self.map.remove(&key) {
            self.order.remove(&s);
        }
        self.seq += 1;
        self.order.insert(self.seq, key.clone());
        self.map.insert(key, (self.seq, t.clone()));
        while self.map.len() > MAX_TRACKS {
            match self.order.pop_first() {
                Some((_, k)) => {
                    self.map.remove(&k);
                }
                None => break,
            }
        }
    }

    fn get(&self, key: &str) -> Option<Track> {
        self.map.get(key).map(|(_, t)| t.clone())
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Sample {
    position: f64,
    paused: bool,
    active: bool,
    muted: bool,
}

#[derive(Default)]
struct Core {
    catalog: Catalog,
    sample_at: f64,
    sample: Sample,
    art: Vec<(String, String, Arc<Vec<u8>>)>,
    art_bytes: usize,
    socket: String,
    /// Python kept `app.volume` an int after a clamp (`min(150, x)`); the
    /// value it was clamped to, while the engine still reports it.
    volume_int: Option<f64>,
}

pub struct ServiceBackend {
    app: Arc<App>,
    ui: UiRef,
    notify_fn: RwLock<Option<NotifyFn>>,
    idle_exit: u64,
    started: f64,
    /// Serialises commands (1.x `self.lock` around engine calls).
    ops: Mutex<()>,
    /// The revision; held while a broadcast payload or snapshot is built so
    /// revisions and contents stay in order.
    rev: Mutex<u64>,
    core: Mutex<Core>,
    vis: VisHub,
    scanning: AtomicBool,
    local_auth_running: AtomicBool,
    closed: AtomicBool,
    events: Mutex<Vec<u64>>,
}

impl ServiceBackend {
    pub fn new(app: Arc<App>, idle_exit: u64) -> Arc<ServiceBackend> {
        let b = Arc::new_cyclic(|me: &Weak<ServiceBackend>| ServiceBackend {
            app,
            ui: Arc::new(ServiceUI {
                backend: me.clone(),
            }),
            notify_fn: RwLock::new(None),
            idle_exit,
            started: monotonic(),
            ops: Mutex::new(()),
            rev: Mutex::new(0),
            core: Mutex::new(Core::default()),
            vis: VisHub::new(),
            scanning: AtomicBool::new(false),
            local_auth_running: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
        });
        b.sanitize_queue_state();
        b.sanitize_saved_containers();
        b.app.set_ui(Some(b.ui.clone()));
        let mut ids = Vec::new();
        for ev in [
            "playback-state-changed",
            "media-changed",
            "integration-state-changed",
        ] {
            let w = Arc::downgrade(&b);
            ids.push(b.app.events.subscribe(
                ev,
                Arc::new(move |event: &str, _payload: &Map<String, Value>| {
                    if let Some(b) = w.upgrade() {
                        if !b.closed.load(Ordering::SeqCst) {
                            b.bump(event);
                        }
                    }
                }),
            ));
        }
        *lock(&b.events) = ids;
        b
    }

    pub fn app(&self) -> &Arc<App> {
        &self.app
    }

    pub fn ui(&self) -> UiRef {
        self.ui.clone()
    }

    /// CAVA executable for visualization (default `cava` on `$PATH`).
    pub fn set_cava_binary(&self, path: impl Into<String>) {
        self.vis.set_binary(path);
    }

    pub(crate) fn notify(&self, method: &str, params: Value) {
        let n = self
            .notify_fn
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(n) = n {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| n(method, params)))
                .is_err()
            {
                error!("service notification failed");
            }
        }
    }

    /// Drop legacy container rows that older frontends queued as tracks.
    fn sanitize_queue_state(&self) {
        let changed = {
            let mut st = self.app.state();
            let queue: Vec<Track> = st.queue.iter().filter(|t| playable_track(t)).cloned().collect();
            let later: Vec<Track> = st
                .later_queue
                .iter()
                .filter(|t| playable_track(t))
                .cloned()
                .collect();
            let mut changed = queue.len() != st.queue.len() || later.len() != st.later_queue.len();
            if !st.play_context.is_empty() {
                let context: Vec<Track> = st
                    .play_context
                    .iter()
                    .filter(|t| playable_track(t))
                    .cloned()
                    .collect();
                if context.len() != st.play_context.len() {
                    changed = true;
                    st.context_index = match st.current.as_ref().filter(|c| playable_track(c)) {
                        Some(c) => context
                            .iter()
                            .position(|t| t.source == c.source && t.id == c.id)
                            .map(|i| i as i64)
                            .unwrap_or(-1),
                        None => -1,
                    };
                    st.play_context = context;
                }
            }
            if changed {
                st.queue = queue;
                st.later_queue = later;
            }
            changed
        };
        if changed {
            self.app.save_state();
            warn!("service removed legacy non-playable queue/context rows");
        }
    }

    /// Remove legacy container rows from playlists and favorites (older
    /// NIRUAMP builds could save album/artist/playlist rows).
    fn sanitize_saved_containers(&self) {
        let res = (|| -> rusqlite::Result<usize> {
            let c = rusqlite::Connection::open(&self.app.db.path)?;
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            let kind_of = |data: &str| -> Option<String> {
                let v: Value = serde_json::from_str(data).ok()?;
                Some(
                    v.get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("track")
                        .to_string(),
                )
            };
            let mut removed = 0;
            for (table, idcol) in [("playlist_items", "id"), ("favorites", "id")] {
                let rows: Vec<(Value, String)> = {
                    let mut st = c.prepare(&format!("SELECT {idcol},data FROM {table}"))?;
                    let v = st
                        .query_map([], |r| {
                            let id = match r.get_ref(0)? {
                                rusqlite::types::ValueRef::Integer(i) => Value::from(i),
                                rusqlite::types::ValueRef::Text(t) => {
                                    Value::from(String::from_utf8_lossy(t).into_owned())
                                }
                                _ => Value::Null,
                            };
                            let data: Option<String> = r.get(1).ok();
                            Ok((id, data.unwrap_or_default()))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    v
                };
                for (id, data) in rows {
                    let Some(kind) = kind_of(&data) else { continue };
                    if CONTAINER_KINDS.contains(&kind.as_str()) {
                        let sql = format!("DELETE FROM {table} WHERE {idcol}=?");
                        match &id {
                            Value::Number(n) => c.execute(&sql, [n.as_i64().unwrap_or(0)])?,
                            Value::String(s) => c.execute(&sql, [s])?,
                            _ => continue,
                        };
                        removed += 1;
                    }
                }
            }
            Ok(removed)
        })();
        match res {
            Ok(n) if n > 0 => {
                warn!("service removed {n} legacy non-playable playlist/favorite rows")
            }
            Ok(_) => {}
            Err(e) => debug!("saved container cleanup failed · {e}"),
        }
    }

    // ---- state ------------------------------------------------------------

    fn bump(&self, reason: &str) {
        let payload = {
            let mut rev = lock(&self.rev);
            *rev += 1;
            json!({
                "revision": *rev,
                "reason": reason,
                "playback": self.playback(true),
                "sources": self.sources(),
                "capabilities": self.capabilities(),
                "server_time": crate::paths::now(),
            })
        };
        self.notify("state.changed", payload);
    }

    pub fn touch(&self, reason: &str) {
        self.bump(reason);
    }

    fn sample(&self, force: bool) -> Sample {
        let now = monotonic();
        {
            let c = lock(&self.core);
            if !force && now - c.sample_at < 0.45 {
                return c.sample;
            }
        }
        let app = &self.app;
        let (spotify, playing, resume, volume, has_current) = {
            let st = app.state();
            (
                st.is_spotify(),
                st.spotify_state.is_playing,
                st.resume_position,
                st.volume,
                st.current.is_some(),
            )
        };
        let mut v = Sample::default();
        if spotify {
            v.position = app.playback_position();
            v.active = playing;
            v.paused = has_current && !v.active;
            v.muted = volume <= 0.001;
        } else {
            let running = app.player.running();
            v.active = running;
            v.paused = running && app.player.prop_bool("pause") == Some(true);
            v.muted = running && app.player.prop_bool("mute") == Some(true);
            let raw = if running {
                app.player.prop_f64("time-pos")
            } else {
                None
            };
            v.position = raw.unwrap_or(resume);
        }
        let mut c = lock(&self.core);
        c.sample_at = now;
        c.sample = v;
        v
    }

    fn status_of(&self, sample: &Sample) -> &'static str {
        let (state, current, spotify, playing) = {
            let st = self.app.state();
            (
                st.playback_state.state.clone(),
                st.current.is_some(),
                st.is_spotify(),
                st.spotify_state.is_playing,
            )
        };
        if state == "starting" {
            return "loading";
        }
        if state == "loading" && !sample.active {
            return "loading";
        }
        if !current {
            return "idle";
        }
        if spotify {
            return if playing { "playing" } else { "paused" };
        }
        if state == "stopped" {
            return "stopped";
        }
        if sample.active {
            return if sample.paused { "paused" } else { "playing" };
        }
        "paused"
    }

    fn playback(&self, force: bool) -> Value {
        let sample = self.sample(force);
        let status = self.status_of(&sample);
        let duration = self.app.playback_duration();
        let st = self.app.state().clone();
        let t = st.current.as_ref();
        let wire_all = |xs: &[Track]| -> Vec<Value> { xs.iter().map(|x| self.track_wire(x)).collect() };
        let radio = match t {
            Some(t) if t.kind == "radio" => {
                Value::Object(st.radio_info.get(&t.id).cloned().unwrap_or_default())
            }
            _ => json!({}),
        };
        let duration = if duration != 0.0 {
            duration
        } else {
            t.map(|t| t.duration).unwrap_or(0.0)
        };
        json!({
            "status": status,
            "active": matches!(status, "playing" | "paused" | "loading"),
            "transport": match t { Some(t) if t.source == "spotify" => "spotify", Some(_) => "mpv", None => "" },
            "source": t.map(|t| t.source.clone()).unwrap_or_default(),
            "position": jf(sample.position.max(0.0)),
            "duration": jf(duration),
            "seekable": t.is_some_and(|t| t.seekable),
            "volume": jf(st.volume),
            "muted": sample.muted,
            "shuffle": st.shuffle_mode,
            "repeat": st.repeat_mode,
            "current": t.map(|t| self.track_wire(t)).unwrap_or(Value::Null),
            "context_index": st.context_index,
            "context": wire_all(&st.play_context),
            "queue": wire_all(&st.queue),
            "later": wire_all(&st.later_queue),
            "radio": radio,
            "sampled_at": jf(monotonic()),
        })
    }

    fn sources(&self) -> Value {
        let app = &self.app;
        let cfg = &app.cfg;
        let out: Vec<Value> = crate::core::SOURCES
            .iter()
            .map(|spec| {
                let (enabled, configured) = match spec.id {
                    "local" => (true, app.db.count().unwrap_or(0) > 0),
                    "navidrome" => (
                        cfg.getbool("navidrome_enabled", true),
                        app.nav_configured(),
                    ),
                    "spotify" => (
                        cfg.getbool("spotify_enabled", false),
                        app.spotify_configured(),
                    ),
                    "sr" => (cfg.getbool("sr_enabled", true), true),
                    "youtube" => (cfg.getbool("youtube_enabled", true), app.yt.available()),
                    "radio" => (
                        cfg.getbool("custom_radio_enabled", true),
                        app.db.radios().map(|r| !r.is_empty()).unwrap_or(false),
                    ),
                    _ => (true, true),
                };
                let mut caps: Vec<&str> = spec.capabilities.to_vec();
                caps.sort_unstable();
                json!({
                    "id": spec.id,
                    "label": spec.label,
                    "capabilities": caps,
                    "enabled": enabled,
                    "configured": configured,
                })
            })
            .collect();
        Value::Array(out)
    }

    fn capabilities(&self) -> Value {
        let app = &self.app;
        let cava = self.vis.available();
        json!({
            "seek": true,
            "equalizer": false,
            "visualization": cava,
            "artwork": true,
            "drag_drop": true,
            "queue_edit": true,
            "playlists": true,
            "favorites": true,
            "ratings": true,
            "spotify": app.spotify_configured(),
            "navidrome": app.nav_configured(),
            "youtube": app.yt.available(),
            "cava": cava,
            "mpv": crate::paths::which("mpv").is_some(),
            "backend_version": VERSION,
            "protocol_version": PROTOCOL_VERSION,
            "protocol_minor": PROTOCOL_MINOR,
            // Protocol 1.1: visualization.subscribe pushes frames to this
            // connection. "visualization"/"cava" still say whether CAVA exists.
            "visualization_stream": true,
            "visualization_encodings": ["json", "b64"],
            "visualization_max_fps": 30,
            // Protocol 1.2: local librespot receiver state and its OAuth command.
            "spotify_local_authorize": true,
            "spotify_local": app.spotify.local.available(),
            "spotify_local_authorized": app.spotify.local.authorized(),
        })
    }

    fn socket(&self) -> String {
        lock(&self.core).socket.clone()
    }

    fn snapshot_value(&self) -> Value {
        let rev = lock(&self.rev);
        let pid = std::process::id();
        json!({
            "protocol_version": PROTOCOL_VERSION,
            "protocol_minor": PROTOCOL_MINOR,
            "backend": {
                "name": "nplay",
                "version": VERSION,
                "pid": pid,
                "socket": self.socket(),
                "started": jf(self.started),
                "instance": format!("{pid}-{}", self.started as i64),
            },
            "revision": *rev,
            "server_time": crate::paths::now(),
            "playback": self.playback(true),
            "sources": self.sources(),
            "capabilities": self.capabilities(),
        })
    }

    // ---- tracks -------------------------------------------------------------

    fn safe_meta(meta: &Map<String, Value>) -> Value {
        let mut out = Map::new();
        for (k, v) in meta {
            if !META_ALLOW.contains(&k.as_str()) {
                continue;
            }
            match v {
                Value::String(s) => {
                    out.insert(k.clone(), Value::String(clip_chars(s, 512)));
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => {
                    out.insert(k.clone(), v.clone());
                }
                _ => {}
            }
        }
        Value::Object(out)
    }

    /// The wire form of a track; also remembers it in the catalogue.
    pub fn track_wire(&self, t: &Track) -> Value {
        self.remember(t);
        json!({
            "key": t.key(),
            "source": t.source,
            "id": t.id,
            "title": t.title,
            "artist": t.artist,
            "album": t.album,
            "duration": jf(t.duration),
            "kind": if t.kind.is_empty() { "track" } else { t.kind.as_str() },
            "seekable": t.seekable,
            "has_art": !t.cover.is_empty(),
            "meta": Self::safe_meta(&t.meta),
        })
    }

    fn wires(&self, xs: &[Track]) -> Value {
        Value::Array(xs.iter().map(|t| self.track_wire(t)).collect())
    }

    fn remember(&self, t: &Track) {
        lock(&self.core).catalog.remember(t);
    }

    /// Number of tracks in the reference catalogue (tests, diagnostics).
    pub fn catalog_len(&self) -> usize {
        lock(&self.core).catalog.len()
    }

    fn resolve(&self, r: Option<&Value>) -> Result<Track, RpcError> {
        let key = match r {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Object(o)) => match o.get("key").filter(|v| pyval::truthy(v)) {
                Some(k) => py_str(k),
                None => {
                    let s = o.get("source").filter(|v| pyval::truthy(v));
                    let i = o.get("id").filter(|v| pyval::truthy(v));
                    match (s, i) {
                        (Some(s), Some(i)) => format!("{}:{}", py_str(s), py_str(i)),
                        _ => String::new(),
                    }
                }
            },
            _ => String::new(),
        };
        if key.is_empty() {
            return Err(params_error(
                "track reference requires key or source+id",
                None,
            ));
        }
        lock(&self.core).catalog.get(&key).ok_or_else(|| {
            params_error(
                "unknown track reference; browse or search it first",
                Some(json!({"track": clip_chars(&key, 200)})),
            )
        })
    }

    fn resolve_many(&self, refs: Option<&Value>) -> Result<Vec<Track>, RpcError> {
        let Some(Value::Array(a)) = refs else {
            return Err(params_error(
                "context must be a list of track references",
                None,
            ));
        };
        if a.len() > 5000 {
            return Err(params_error("context is too large", None));
        }
        a.iter().map(|x| self.resolve(Some(x))).collect()
    }

    fn require_playable(t: &Track, what: &str) -> Result<(), RpcError> {
        if playable_track(t) {
            Ok(())
        } else {
            Err(params_error(what, Some(json!({"kind": t.kind}))))
        }
    }

    fn ui_ref(&self) -> Option<&UiRef> {
        Some(&self.ui)
    }

    /// `app.volume` as 1.x reported it raw (an int after a clamp).
    fn volume_value(&self) -> Value {
        let v = self.app.volume();
        match lock(&self.core).volume_int {
            Some(x) if x == v => Value::from(v as i64),
            _ => jf(v),
        }
    }

    fn apply_volume_delta(&self, delta: f64) {
        let (before, spotify) = {
            let st = self.app.state();
            (st.volume, st.is_spotify())
        };
        self.app.playback_volume(delta);
        let cap = if spotify { 100.0 } else { 150.0 };
        let raw = before + delta;
        let after = self.app.volume();
        lock(&self.core).volume_int = (raw >= cap || raw <= 0.0).then_some(after);
    }

    // ---- playback -----------------------------------------------------------

    fn play(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        Self::require_playable(
            &t,
            "container items must be expanded into tracks before playback",
        )?;
        let _g = lock(&self.ops);
        if let Some(context) = p.get("context").filter(|v| !v.is_null()) {
            let xs = self.resolve_many(Some(context))?;
            if xs.is_empty() {
                return Err(params_error("context is empty", None));
            }
            let idx = match p.get("index").filter(|v| !v.is_null()) {
                Some(i) => py_int(Some(i))?,
                None => xs
                    .iter()
                    .position(|x| x.source == t.source && x.id == t.id)
                    .unwrap_or(0) as i64,
            };
            self.app.set_play_context(xs, idx);
        }
        let resumed = float_or(p, "resume_position", 0.0)?;
        self.app.play_with(
            t,
            self.ui_ref(),
            flag(p, "preserve_context"),
            resumed,
            false,
        );
        *lock(&self.rev) += 1;
        Ok(self.playback(false))
    }

    fn transport(&self, reason: &str, f: impl FnOnce(&App, Option<&UiRef>)) -> Value {
        {
            let _g = lock(&self.ops);
            f(&self.app, self.ui_ref());
        }
        self.touch(reason);
        self.playback(false)
    }

    fn seek(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        {
            let _g = lock(&self.ops);
            let delta = if p.contains_key("position") {
                let target = float_or(p, "position", 0.0)?.max(0.0);
                target - self.app.playback_position()
            } else {
                float_or(p, "delta", 0.0)?
            };
            if delta.abs() > 0.001 {
                self.app.playback_seek_relative(delta);
            }
        }
        self.touch("position");
        Ok(json!({"position": jf(self.app.playback_position())}))
    }

    fn volume(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        {
            let _g = lock(&self.ops);
            let delta = if p.contains_key("value") {
                let target = float_or(p, "value", 0.0)?.clamp(0.0, 150.0);
                target - self.app.volume()
            } else {
                float_or(p, "delta", 0.0)?
            };
            if delta.abs() > 0.001 {
                self.apply_volume_delta(delta);
            }
        }
        self.touch("volume");
        Ok(json!({"volume": self.volume_value()}))
    }

    fn mute(&self) -> Value {
        {
            let _g = lock(&self.ops);
            let spotify = self.app.state().is_spotify();
            self.app.playback_mute();
            if spotify {
                lock(&self.core).volume_int = Some(0.0);
            }
        }
        self.touch("volume");
        json!({"muted": self.sample(true).muted, "volume": self.volume_value()})
    }

    fn repeat(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let mode = p.get("mode").filter(|v| !v.is_null());
        {
            let _g = lock(&self.ops);
            match mode {
                Some(m) => {
                    let m = m.as_str().unwrap_or("\u{0}");
                    if !["off", "track", "context"].contains(&m) {
                        return Err(params_error(
                            "repeat mode must be off, track or context",
                            None,
                        ));
                    }
                    while self.app.repeat_mode() != m {
                        let before = self.app.repeat_mode();
                        self.app.cycle_repeat(self.ui_ref());
                        if self.app.repeat_mode() == before {
                            break;
                        }
                    }
                    self.app.cfg.set("repeat_mode", m);
                }
                None => self.app.cycle_repeat(self.ui_ref()),
            }
            self.app.save_state();
        }
        self.touch("repeat");
        Ok(json!({"repeat": self.app.repeat_mode()}))
    }

    fn shuffle(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let mode = p.get("mode").filter(|v| !v.is_null());
        {
            let _g = lock(&self.ops);
            match mode {
                Some(m) => {
                    let m = m.as_str().unwrap_or("\u{0}");
                    if !["off", "shuffle", "smart"].contains(&m) {
                        return Err(params_error(
                            "shuffle mode must be off, shuffle or smart",
                            None,
                        ));
                    }
                    while self.app.shuffle_mode() != m {
                        let before = self.app.shuffle_mode();
                        self.app.cycle_shuffle(self.ui_ref());
                        if self.app.shuffle_mode() == before {
                            break;
                        }
                    }
                }
                None => self.app.cycle_shuffle(self.ui_ref()),
            }
            self.app.save_state();
        }
        self.touch("shuffle");
        Ok(json!({"shuffle": self.app.shuffle_mode()}))
    }

    // ---- queue --------------------------------------------------------------

    fn queue_list(&self) -> Value {
        let (q, later) = {
            let st = self.app.state();
            (st.queue.clone(), st.later_queue.clone())
        };
        json!({"items": self.wires(&q), "later": self.wires(&later)})
    }

    fn queue_len(&self) -> i64 {
        self.app.state().queue.len() as i64
    }

    fn queue_add(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        Self::require_playable(
            &t,
            "container items must be expanded into tracks before queueing",
        )?;
        {
            let _g = lock(&self.ops);
            if flag(p, "next") {
                self.app.queue_next(t, self.ui_ref());
            } else {
                self.app.queue_add(t, self.ui_ref());
            }
        }
        self.touch("queue");
        Ok(self.queue_list())
    }

    fn non_empty_list<'a>(
        p: &'a Map<String, Value>,
        key: &str,
        max: usize,
    ) -> Result<&'a Value, RpcError> {
        match p.get(key) {
            Some(v @ Value::Array(a)) if !a.is_empty() => {
                if a.len() > max {
                    Err(params_error("too many tracks", None))
                } else {
                    Ok(v)
                }
            }
            _ => Err(params_error("tracks must be a non-empty list", None)),
        }
    }

    fn queue_add_many(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let refs = Self::non_empty_list(p, "tracks", 1000)?;
        let xs = self.resolve_many(Some(refs))?;
        if let Some(bad) = xs.iter().find(|t| !playable_track(t)) {
            Self::require_playable(
                bad,
                "container items must be expanded into tracks before queueing",
            )?;
        }
        {
            let _g = lock(&self.ops);
            {
                let mut st = self.app.state();
                if flag(p, "next") {
                    st.queue.splice(0..0, xs.iter().cloned());
                } else {
                    st.queue.extend(xs.iter().cloned());
                }
            }
            for t in &xs {
                self.remember(t);
            }
            self.app.save_state();
        }
        self.touch("queue");
        Ok(self.queue_list())
    }

    fn queue_index(&self, p: &Map<String, Value>, key: &str, msg: &str) -> Result<i64, RpcError> {
        let i = py_int(p.get(key))?;
        if i < 0 || i >= self.queue_len() {
            return Err(params_error(msg, None));
        }
        Ok(i)
    }

    fn queue_play(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let index = py_int(p.get("index"))?;
        {
            let _g = lock(&self.ops);
            if index < 0 || index >= self.queue_len() {
                return Err(params_error("queue index out of range", None));
            }
            self.app.queue_play(index as usize, self.ui_ref());
        }
        self.touch("queue");
        Ok(self.playback(false))
    }

    fn queue_remove(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        {
            let _g = lock(&self.ops);
            let index = self.queue_index(p, "index", "queue index out of range")?;
            self.app.queue_remove(index as usize, self.ui_ref());
        }
        self.touch("queue");
        Ok(self.queue_list())
    }

    fn queue_reorder(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let src = py_int(p.get("from"))?;
        let dst = py_int(p.get("to"))?;
        let dst = {
            let _g = lock(&self.ops);
            let dst = {
                let mut st = self.app.state();
                let n = st.queue.len() as i64;
                if src < 0 || src >= n {
                    return Err(params_error("queue source index out of range", None));
                }
                let dst = dst.clamp(0, n - 1);
                let item = st.queue.remove(src as usize);
                st.queue.insert(dst as usize, item);
                dst
            };
            self.app.save_state();
            dst
        };
        self.touch("queue");
        Ok(merge(json!({"index": dst}), self.queue_list()))
    }

    fn queue_move(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let index = py_int(p.get("index"))?;
        let delta = match p.get("delta") {
            Some(v) => py_int(Some(v))?,
            None => 0,
        };
        let new_index = {
            let _g = lock(&self.ops);
            if index < 0 || index >= self.queue_len() {
                return Err(params_error("queue index out of range", None));
            }
            self.app.queue_move(index as usize, delta, self.ui_ref())
        };
        self.touch("queue");
        Ok(merge(json!({"index": new_index}), self.queue_list()))
    }

    fn queue_clear(&self) -> Value {
        {
            let _g = lock(&self.ops);
            self.app.queue_clear(self.ui_ref());
            self.app.state().later_queue.clear();
            self.app.save_state();
        }
        self.touch("queue");
        self.queue_list()
    }

    fn queue_save(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = str_or(p, "name", "").trim().to_string();
        if name.is_empty() {
            return Err(params_error("playlist name is required", None));
        }
        let _g = lock(&self.ops);
        let q = self.app.queue();
        if q.is_empty() {
            return Err(backend_error("queue is empty", None));
        }
        let pid = self.app.db.playlist_create(&name).map_err(internal)?;
        if let Some(pid) = pid {
            for t in &q {
                self.app.db.playlist_add(pid, t).map_err(internal)?;
            }
        }
        Ok(json!({"playlist_id": pid, "name": name}))
    }

    // ---- playlists / favorites / ratings ----------------------------------------

    fn playlists_list(&self) -> Result<Value, RpcError> {
        let rows = self.app.db.playlists().map_err(internal)?;
        let mut out = Vec::new();
        for (name, action, subtitle) in rows {
            let id = py_int(Some(&Value::String(
                action.rsplit(':').next().unwrap_or("").to_string(),
            )))?;
            out.push(json!({"id": id, "name": name, "subtitle": subtitle}));
        }
        Ok(json!({"items": out}))
    }

    fn playlist_tracks_of(&self, pid: i64) -> Result<Value, RpcError> {
        let xs = self.app.db.playlist_tracks(pid).map_err(internal)?;
        let name = self.app.db.playlist_name(pid).map_err(internal)?;
        Ok(json!({"id": pid, "name": name, "items": self.wires(&xs)}))
    }

    fn playlist_tracks(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        self.playlist_tracks_of(py_int(p.get("id"))?)
    }

    fn playlist_create(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = str_or(p, "name", "").trim().to_string();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(params_error("playlist name is required", None));
        }
        let pid = self.app.db.playlist_create(&name).map_err(internal)?;
        Ok(json!({"id": pid, "name": name}))
    }

    fn playlist_add(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        let t = self.resolve(p.get("track"))?;
        Self::require_playable(
            &t,
            "container items must be expanded into tracks before adding to a playlist",
        )?;
        self.app.db.playlist_add(pid, &t).map_err(internal)?;
        self.playlist_tracks_of(pid)
    }

    fn playlist_add_many(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        let refs = Self::non_empty_list(p, "tracks", 5000)?;
        let xs = self.resolve_many(Some(refs))?;
        if let Some(bad) = xs.iter().find(|t| !playable_track(t)) {
            Self::require_playable(
                bad,
                "container items must be expanded into tracks before adding to a playlist",
            )?;
        }
        {
            let _g = lock(&self.ops);
            for t in &xs {
                self.app.db.playlist_add(pid, t).map_err(internal)?;
                self.remember(t);
            }
        }
        self.playlist_tracks_of(pid)
    }

    fn playlist_remove(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        let index = py_int(p.get("index"))?;
        self.app.db.playlist_remove(pid, index).map_err(internal)?;
        self.playlist_tracks_of(pid)
    }

    fn playlist_move(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        let index = py_int(p.get("index"))?;
        let delta = match p.get("delta") {
            Some(v) => py_int(Some(v))?,
            None => 0,
        };
        let new_index = self
            .app
            .db
            .playlist_move(pid, index, delta)
            .map_err(internal)?;
        Ok(merge(
            json!({"index": new_index}),
            self.playlist_tracks_of(pid)?,
        ))
    }

    fn playlist_rename(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        let name = str_or(p, "name", "").trim().to_string();
        if name.is_empty() {
            return Err(params_error("playlist name is required", None));
        }
        self.app.db.playlist_rename(pid, &name).map_err(internal)?;
        Ok(json!({"id": pid, "name": name}))
    }

    fn playlist_delete(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let pid = py_int(p.get("id"))?;
        self.app.db.playlist_delete(pid).map_err(internal)?;
        Ok(json!({"deleted": pid}))
    }

    fn favorites_list(&self) -> Result<Value, RpcError> {
        let xs = self.app.db.favorites().map_err(internal)?;
        Ok(json!({"items": self.wires(&xs)}))
    }

    fn favorite_toggle(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        Self::require_playable(&t, "favorites are supported for playable tracks only")?;
        let added = self.app.db.favorite(&t).map_err(internal)?;
        Ok(json!({"added": added, "track": self.track_wire(&t)}))
    }

    fn rating_get(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        let r = self.app.db.rating_get(&t.id, &t.source).map_err(internal)?;
        Ok(json!({"rating": r, "track": self.track_wire(&t)}))
    }

    fn rating_set(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        Self::require_playable(&t, "ratings are supported for playable tracks only")?;
        let rating = int_or(p, "rating", 0)?.clamp(0, 5);
        self.app
            .db
            .rating_set(&t.id, rating, &t.source)
            .map_err(internal)?;
        Ok(json!({"rating": rating, "track": self.track_wire(&t)}))
    }

    // ---- radio ------------------------------------------------------------------

    fn radio_add(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = str_or(p, "name", "").trim().to_string();
        let url = str_or(p, "url", "").trim().to_string();
        if name.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(params_error(
                "station needs a name and an http(s) stream URL",
                None,
            ));
        }
        self.app
            .db
            .radio_add(&name, &url, "", "")
            .map_err(internal)?;
        Ok(json!({"ok": true}))
    }

    fn radio_delete(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let id = py_int(p.get("id"))?;
        self.app.db.radio_delete(id).map_err(internal)?;
        Ok(json!({"deleted": id}))
    }

    // ---- artwork ----------------------------------------------------------------

    fn artwork(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let t = self.resolve(p.get("track"))?;
        let empty = || json!({"mime": "", "data": "", "empty": true});
        if t.cover.is_empty() {
            return Ok(empty());
        }
        let b64 = |d: &[u8]| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(d)
        };
        let key = format!("{}:{}:{}", t.source, t.id, t.cover);
        {
            let mut c = lock(&self.core);
            if let Some(i) = c.art.iter().position(|(k, _, _)| *k == key) {
                let hit = c.art.remove(i);
                let out = json!({"mime": hit.1, "data": b64(&hit.2)});
                c.art.push(hit);
                return Ok(out);
            }
        }
        let cover = t.cover.clone();
        let path: Option<PathBuf> = if cover.starts_with("http://") || cover.starts_with("https://")
        {
            self.app
                .artwork_cache
                .fetch(&cover, 8.0, &crate::artwork_cache::user_agent())
        } else {
            let p = super::protocol::expanduser(&cover);
            p.is_file().then_some(p)
        };
        let Some(path) = path else {
            return Ok(empty());
        };
        let unavailable = |e: std::io::Error| {
            backend_error(
                "artwork unavailable",
                Some(json!({"detail": clip_chars(&redact_text(&e.to_string()), 200)})),
            )
        };
        let size = std::fs::metadata(&path).map_err(unavailable)?.len();
        if size > MAX_ARTWORK {
            return Err(backend_error("artwork is too large", None));
        }
        let data = std::fs::read(&path).map_err(unavailable)?;
        if data.is_empty() {
            return Ok(empty());
        }
        let mime = guess_mime(&path);
        let out = json!({"mime": mime, "data": b64(&data)});
        if data.len() <= ART_CACHE_ENTRY {
            let mut c = lock(&self.core);
            c.art_bytes += data.len();
            c.art.push((key, mime.to_string(), Arc::new(data)));
            while c.art_bytes > ART_CACHE_BYTES && c.art.len() > 1 {
                let old = c.art.remove(0);
                c.art_bytes -= old.2.len();
            }
        }
        Ok(out)
    }

    // ---- settings / sources -------------------------------------------------------

    fn spotify_status(&self) -> Value {
        let c = &self.app.cfg;
        let local = &self.app.spotify.local;
        let playback = c.get("spotify_playback", "local");
        json!({
            "configured": self.app.spotify_configured(),
            "enabled": c.getbool("spotify_enabled", false),
            // Protocol 1.2 additions.
            "playback": if playback.is_empty() { "local".to_string() } else { playback },
            "local_available": local.available(),
            "local_authorized": local.authorized(),
            "local_running": local.running(),
            "local_authorizing": self.local_auth_running.load(Ordering::SeqCst),
        })
    }

    fn settings_get(&self) -> Value {
        let c = &self.app.cfg;
        let mut out = Map::new();
        for key in SAFE_SETTINGS {
            let v = if BOOL_SETTINGS.contains(key) {
                Value::Bool(c.getbool(key, false))
            } else {
                Value::String(c.get(key, ""))
            };
            out.insert((*key).into(), v);
        }
        json!({
            "settings": out,
            "navidrome": {
                "configured": self.app.nav_configured(),
                "url": c.get("navidrome_url", ""),
                "user": c.get("navidrome_user", ""),
            },
            "spotify": self.spotify_status(),
            "library": {
                "roots": self.app.roots(),
                "tracks": self.app.db.count().unwrap_or(0),
                "last_scan": self.app.db.last_scan(),
            },
            "system": {
                "version": VERSION,
                "protocol": PROTOCOL_VERSION,
                "protocol_minor": PROTOCOL_MINOR,
                "log": crate::log::log_path().display().to_string(),
            },
        })
    }

    fn settings_set(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let Some(Value::Object(values)) = p.get("settings") else {
            return Err(params_error("settings must be an object", None));
        };
        let mut changed = Vec::new();
        {
            let _g = lock(&self.ops);
            for (key, value) in values {
                if !SAFE_SETTINGS.contains(&key.as_str()) {
                    return Err(params_error(
                        "setting is not writable through the control protocol",
                        Some(json!({"key": key})),
                    ));
                }
                if BOOL_SETTINGS.contains(&key.as_str()) {
                    let b = match value {
                        Value::String(s) if s.eq_ignore_ascii_case("true") => true,
                        Value::String(s) if s.eq_ignore_ascii_case("false") => false,
                        Value::Bool(b) => *b,
                        _ => {
                            return Err(params_error(
                                "boolean setting requires true or false",
                                Some(json!({"key": key})),
                            ))
                        }
                    };
                    self.app.cfg.set(key, if b { "true" } else { "false" });
                } else {
                    let s = value.as_str();
                    if key == "preferred_source"
                        && !s.is_some_and(|s| ["local", "navidrome", "spotify", "youtube"].contains(&s))
                    {
                        return Err(params_error("unknown preferred source", None));
                    }
                    if key == "normalization_mode"
                        && !s.is_some_and(|s| ["auto", "track", "album"].contains(&s))
                    {
                        return Err(params_error("unknown normalization mode", None));
                    }
                    self.app.cfg.set(key, py_str(value));
                }
                changed.push(key.clone());
            }
        }
        self.touch("settings");
        Ok(merge(json!({"changed": changed}), self.settings_get()))
    }

    fn navidrome_setup(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let url = str_or(p, "url", "").trim().trim_end_matches('/').to_string();
        let user = str_or(p, "user", "").trim().to_string();
        let password = str_or(p, "password", "");
        if !(url.starts_with("http://") || url.starts_with("https://"))
            || user.is_empty()
            || password.is_empty()
        {
            return Err(params_error(
                "server URL, user and password are required",
                None,
            ));
        }
        let probe = crate::providers::Navidrome::new(&url, &user, &password, 0);
        if let Err(e) = probe.ping() {
            return Err(backend_error(
                "Navidrome connection failed",
                Some(json!({
                    "detail": clip_chars(&redact_text(&e.to_string()), 200),
                    "remedy": "check the URL and password in Settings → Navidrome",
                })),
            ));
        }
        self.app.cfg.save_nav(&url, &user, &password);
        self.touch("settings");
        Ok(json!({"configured": true, "url": url, "user": user}))
    }

    fn navidrome_clear(&self) -> Value {
        self.app.cfg.clear_nav();
        self.touch("settings");
        json!({"configured": false})
    }

    fn spotify_connect(self: &Arc<Self>) -> Result<Value, RpcError> {
        if self.app.cfg.get("spotify_client_id", "").trim().is_empty() {
            return Err(backend_error(
                "Spotify client ID is not configured",
                Some(json!({"remedy": "Set spotify_client_id in NPLAY settings or run `nplay` and use Spotify setup"})),
            ));
        }
        if self.app.spotify_configured() {
            return Ok(json!({"connected": true}));
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("nplay-spotify-auth".into())
            .spawn(move || match me.app.spotify.connect(true, 180.0) {
                Ok(_) => me.notify("spotify.auth", json!({"connected": true})),
                Err(e) => me.notify(
                    "spotify.auth",
                    json!({"connected": false, "error": clip_chars(&e.to_string(), 300)}),
                ),
            });
        Ok(json!({"started": true}))
    }

    /// Protocol 1.2: the terminal's AUTHORIZE flow for librespot; the result
    /// arrives as a `spotify.local_auth` notification.
    fn spotify_local_authorize(
        self: &Arc<Self>,
        p: &Map<String, Value>,
    ) -> Result<Value, RpcError> {
        if !self.app.spotify.local.available() {
            return Err(backend_error(
                "librespot is not installed",
                Some(json!({"remedy": "Install librespot for local Spotify playback, or use an external Spotify Connect device"})),
            ));
        }
        let timeout = match p.get("timeout").filter(|v| !v.is_null()) {
            Some(v) => py_float(Some(v))
                .map_err(|_| params_error("timeout must be a number of seconds", None))?
                .clamp(10.0, 600.0),
            None => 180.0,
        };
        if self.local_auth_running.swap(true, Ordering::SeqCst) {
            return Ok(json!({"started": false, "running": true}));
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("nplay-spotify-local-auth".into())
            .spawn(move || {
                let n = me.clone();
                let opened = move |_url: &str| {
                    n.notify(
                        "spotify.local_auth",
                        json!({"phase": "browser", "authorized": false}),
                    )
                };
                match me.app.spotify.authorize_local(timeout, Some(&opened), 30.0) {
                    Ok(d) => {
                        let name = d.get("name").map(py_str_nonempty).unwrap_or_default();
                        if !d.is_empty() {
                            me.app.state().spotify_state.device = d;
                        }
                        me.app.cfg.set("spotify_playback", "local");
                        me.notify(
                            "spotify.local_auth",
                            json!({"phase": "done", "authorized": true, "device": name}),
                        );
                    }
                    Err(e) => {
                        let msg = redact_text(&e.to_string());
                        warn!("local Spotify authorization failed: {msg}");
                        me.notify(
                            "spotify.local_auth",
                            json!({"phase": "done", "authorized": false, "error": clip_chars(&msg, 300)}),
                        );
                    }
                }
                me.local_auth_running.store(false, Ordering::SeqCst);
                me.touch("spotify");
            });
        if spawned.is_err() {
            self.local_auth_running.store(false, Ordering::SeqCst);
            return Err(internal("could not start the authorization worker"));
        }
        self.touch("spotify");
        Ok(json!({"started": true, "timeout": jf(timeout)}))
    }

    fn spotify_disconnect(&self) -> Value {
        self.app.spotify.disconnect();
        json!({"connected": false})
    }

    fn spotify_devices(&self) -> Result<Value, RpcError> {
        if !self.app.spotify_configured() {
            return Err(backend_error("Spotify is not connected", None));
        }
        let (items, _) = self.browse_spotify("devices", "", 50, &Map::new())?;
        Ok(json!({"items": self.wires(&items)}))
    }

    fn spotify_transfer(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let did = str_or(p, "device_id", "");
        if did.is_empty() {
            return Err(params_error("device_id is required", None));
        }
        self.app
            .spotify
            .transfer(&did, flag(p, "play"))
            .map_err(internal)?;
        Ok(json!({"ok": true}))
    }

    // ---- external sources / scan ----------------------------------------------------

    fn play_external(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let path = str_or(p, "path", "").trim().to_string();
        let url = str_or(p, "url", "").trim().to_string();
        let title = str_or(p, "title", "").trim().to_string();
        let (t, source) = if !url.is_empty() {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(params_error(
                    "only http(s) URLs can be opened directly",
                    None,
                ));
            }
            let low = url.to_lowercase();
            if !title.is_empty() && [".m3u", ".m3u8", ".pls"].iter().any(|e| low.ends_with(e)) {
                return Err(params_error(
                    "playlist URLs are not supported; use NPLAY radio setup",
                    None,
                ));
            }
            let mut t = Track::new("radio", &url, if title.is_empty() { &url } else { &title });
            t.artist = "External stream".into();
            t.url = url.clone();
            t.kind = "radio".into();
            t.seekable = false;
            (t, "radio")
        } else if !path.is_empty() {
            let q = super::protocol::expanduser(&path);
            if !q.is_file() {
                return Err(params_error(
                    "file does not exist",
                    Some(json!({"path": clip_chars(&path, 300)})),
                ));
            }
            let ext = q
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            if ![".mp3", ".flac", ".ogg", ".opus", ".m4a", ".aac", ".wav", ".webm"]
                .contains(&ext.as_str())
            {
                return Err(params_error("unsupported media file", None));
            }
            let qs = q.display().to_string();
            let stem = q
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut t = Track::new("local", &qs, if title.is_empty() { &stem } else { &title });
            t.artist = "Dropped file".into();
            t.album = q
                .parent()
                .and_then(Path::file_name)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            t.path = qs.clone();
            t.url = qs;
            t.seekable = true;
            (t, "local")
        } else {
            return Err(params_error("path or url is required", None));
        };
        self.remember(&t);
        if flag(p, "queue") {
            {
                let _g = lock(&self.ops);
                {
                    let mut st = self.app.state();
                    if flag(p, "next") {
                        st.queue.insert(0, t.clone());
                    } else {
                        st.queue.push(t.clone());
                    }
                }
                self.app.save_state();
            }
            self.touch("queue");
            return Ok(json!({"track": self.track_wire(&t), "source": source, "queued": true}));
        }
        {
            let _g = lock(&self.ops);
            self.app.set_play_context(vec![t.clone()], 0);
            self.app.play(t.clone(), self.ui_ref(), false);
        }
        self.touch("playback");
        Ok(json!({"track": self.track_wire(&t), "source": source, "queued": false}))
    }

    fn library_scan(self: &Arc<Self>, p: &Map<String, Value>) -> Result<Value, RpcError> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err(backend_error("a library scan is already running", None));
        }
        let force = flag(p, "force");
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("nplay-library-scan".into())
            .spawn(move || {
                match me.app.scan(None, force) {
                    Ok(r) => me.notify("library.scanned", json!({"result": r.to_map()})),
                    Err(e) => me.notify(
                        "library.scanned",
                        json!({"error": clip_chars(&e.to_string(), 300)}),
                    ),
                }
                me.scanning.store(false, Ordering::SeqCst);
                me.touch("library");
            });
        if spawned.is_err() {
            self.scanning.store(false, Ordering::SeqCst);
            return Err(internal("could not start the library scan"));
        }
        Ok(json!({"started": true}))
    }

    fn library_stats(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "tracks": self.app.db.count().map_err(internal)?,
            "roots": self.app.roots(),
            "schema": self.app.db.schema_version().map_err(internal)?,
            "last_scan": self.app.db.last_scan(),
        }))
    }

    // ---- lifecycle ------------------------------------------------------------------

    fn info(&self) -> Value {
        json!({
            "protocol_version": PROTOCOL_VERSION,
            "protocol_minor": PROTOCOL_MINOR,
            "backend_version": VERSION,
            "pid": std::process::id(),
            "socket": self.socket(),
            "uptime": jf((monotonic() - self.started).max(0.0)),
            "idle_exit": self.idle_exit,
            "visualization": self.vis.status(),
        })
    }

    fn initialize(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let client = match p.get("client") {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        if let Some(Value::Array(versions)) = p.get("protocol_versions") {
            let ok = versions.iter().any(|v| match v {
                Value::Number(n) => n.as_f64() == Some(PROTOCOL_VERSION as f64),
                Value::Bool(b) => (*b as i64) == PROTOCOL_VERSION,
                _ => false,
            });
            if !ok {
                return Err(RpcError::with_data(
                    super::protocol::INCOMPATIBLE_PROTOCOL,
                    "incompatible protocol version",
                    json!({"server": PROTOCOL_VERSION, "client": versions}),
                ));
            }
        }
        let mut snap = obj(self.snapshot_value());
        snap.insert("protocol_version".into(), Value::from(PROTOCOL_VERSION));
        snap.insert("protocol_minor".into(), Value::from(PROTOCOL_MINOR));
        snap.insert(
            "client".into(),
            json!({
                "name": clip_chars(&str_or(&client, "name", "unknown"), 80),
                "version": clip_chars(&str_or(&client, "version", ""), 40),
            }),
        );
        if let Some(Value::Object(caps)) = snap.get_mut("capabilities") {
            caps.insert(
                "messages".into(),
                json!([
                    "state.changed",
                    "library.scanned",
                    "spotify.auth",
                    "spotify.local_auth",
                    "service.error"
                ]),
            );
            // Non-JSON-RPC push lines a subscribed connection may receive.
            caps.insert("push".into(), json!(["vis"]));
        }
        Ok(Value::Object(snap))
    }

    fn dispatch_inner(
        self: &Arc<Self>,
        method: &str,
        p: &Map<String, Value>,
    ) -> Result<Value, RpcError> {
        Ok(match method {
            "ping" => json!({"pong": crate::paths::now()}),
            "initialize" => self.initialize(p)?,
            "state.snapshot" | "state.subscribe" => self.snapshot_value(),
            "state.unsubscribe" => json!({"ok": true}),
            "playback.play" => self.play(p)?,
            "playback.toggle" => self.transport("playback", |a, u| a.play_pause(u)),
            "playback.stop" => self.transport("playback", |a, u| a.stop_and_clear(u)),
            "playback.next" => self.transport("playback", |a, u| a.next_track(u, false)),
            "playback.previous" => self.transport("playback", |a, u| a.previous_track(u)),
            "playback.seek" => self.seek(p)?,
            "playback.volume" => self.volume(p)?,
            "playback.mute" => self.mute(),
            "playback.repeat" => self.repeat(p)?,
            "playback.shuffle" => self.shuffle(p)?,
            "playback.play_external" => self.play_external(p)?,
            "queue.list" => self.queue_list(),
            "queue.add" => self.queue_add(p)?,
            "queue.add_many" => self.queue_add_many(p)?,
            "queue.play" => self.queue_play(p)?,
            "queue.remove" => self.queue_remove(p)?,
            "queue.move" => self.queue_move(p)?,
            "queue.reorder" => self.queue_reorder(p)?,
            "queue.clear" => self.queue_clear(),
            "queue.save" => self.queue_save(p)?,
            "browse" => self.browse(p)?,
            "search" => self.search(p)?,
            "playlists.list" => self.playlists_list()?,
            "playlists.tracks" => self.playlist_tracks(p)?,
            "playlists.create" => self.playlist_create(p)?,
            "playlists.add" => self.playlist_add(p)?,
            "playlists.add_many" => self.playlist_add_many(p)?,
            "playlists.remove" => self.playlist_remove(p)?,
            "playlists.move" => self.playlist_move(p)?,
            "playlists.rename" => self.playlist_rename(p)?,
            "playlists.delete" => self.playlist_delete(p)?,
            "favorites.list" => self.favorites_list()?,
            "favorites.toggle" => self.favorite_toggle(p)?,
            "ratings.get" => self.rating_get(p)?,
            "ratings.set" => self.rating_set(p)?,
            "radio.add" => self.radio_add(p)?,
            "radio.delete" => self.radio_delete(p)?,
            "artwork.get" => self.artwork(p)?,
            "visualization.start" => self.vis.start(p)?,
            "visualization.stop" => self.vis.stop(p)?,
            "visualization.data" => self.vis.data(p)?,
            "visualization.subscribe" => {
                return Err(backend_error(
                    "visualization.subscribe needs a streaming connection",
                    None,
                ))
            }
            "visualization.unsubscribe" => {
                json!({"subscribed": false, "was_subscribed": false})
            }
            "visualization.status" => self.vis.status(),
            "settings.get" => self.settings_get(),
            "settings.set" => self.settings_set(p)?,
            "navidrome.setup" => self.navidrome_setup(p)?,
            "navidrome.clear" => self.navidrome_clear(),
            "spotify.connect" => self.spotify_connect()?,
            "spotify.disconnect" => self.spotify_disconnect(),
            "spotify.local_authorize" => self.spotify_local_authorize(p)?,
            "spotify.devices" => self.spotify_devices()?,
            "spotify.transfer" => self.spotify_transfer(p)?,
            "library.scan" => self.library_scan(p)?,
            "library.stats" => self.library_stats()?,
            "service.info" => self.info(),
            _ => return Err(RpcError::method("method not found")),
        })
    }
}

/// `str(x or "")` for a device name.
fn py_str_nonempty(v: &Value) -> String {
    if pyval::truthy(v) {
        py_str(v)
    } else {
        String::new()
    }
}

/// 1.x `mimetypes.guess_type` for the image types artwork uses.
fn guess_mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/vnd.microsoft.icon",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
}

/// Adapter so the server (which holds `Arc<dyn Backend>`) reaches the
/// `Arc<ServiceBackend>` methods that spawn workers.
pub struct BackendHandle(pub Arc<ServiceBackend>);

impl Backend for BackendHandle {
    fn snapshot(&self) -> Value {
        self.0.snapshot_value()
    }

    fn dispatch(&self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
        self.0.dispatch_inner(method, params)
    }

    fn visualization_subscribe(
        &self,
        params: &Map<String, Value>,
        client_id: u64,
        sink: FrameSink,
    ) -> Result<Value, RpcError> {
        self.0.vis.subscribe(params, client_id, sink)
    }

    fn visualization_activate(&self, client_id: u64) {
        self.0.vis.activate(client_id)
    }

    fn visualization_unsubscribe(
        &self,
        _params: &Map<String, Value>,
        client_id: u64,
    ) -> Result<Value, RpcError> {
        Ok(self.0.vis.unsubscribe(client_id))
    }

    fn visualization_client_gone(&self, client_id: u64) {
        self.0.vis.client_gone(client_id)
    }

    fn set_notify(&self, notify: NotifyFn) {
        *self.0.notify_fn.write().unwrap_or_else(|e| e.into_inner()) = Some(notify);
    }

    fn set_socket_path(&self, path: &Path) {
        lock(&self.0.core).socket = path.display().to_string();
    }

    fn close(&self) {
        self.0.close();
    }
}

impl ServiceBackend {
    /// Stop CAVA, then the engine (`App::shutdown`: checkpoint, session save,
    /// librespot close before the playback stop, MPRIS stop). Runs once.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.vis.close();
        for id in lock(&self.events).drain(..) {
            self.app.events.unsubscribe(id);
        }
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.app.shutdown())).is_err()
        {
            error!("service shutdown failed");
        }
    }
}

/// `nplay --serve`: run the control service in the foreground; returns the
/// exit code.
pub fn run_service(path: Option<PathBuf>, idle_exit: u64) -> i32 {
    let app = match App::new() {
        Ok(a) => a,
        Err(e) => {
            println!("NPLAY service failed: {e}");
            error!("service failed: {e}");
            return 1;
        }
    };
    let backend = ServiceBackend::new(app, idle_exit);
    let code = super::serve(path, idle_exit, Arc::new(BackendHandle(backend.clone())));
    backend.close();
    code
}
