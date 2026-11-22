//! Optional Spotify Web API + Spotify Connect integration (port of 1.x
//! `nplay/spotify.py`).
//!
//! Authorization Code with PKCE for the Web API (the user's own client id,
//! `spotify_client_id`; no client secret). The callback listener binds only
//! `127.0.0.1:43821` and exists only during `connect()`. Tokens live in
//! `secrets.ini [spotify]` (0600) through `Config::save_spotify_tokens`.
//! Local playback is the optional managed librespot receiver in
//! `spotify::local` (see its doc comment for the lifecycle rules).
//!
//! Public API
//! - Constants: `API`, `AUTH`, `TOKEN`, `SCOPES`, `CALLBACK_PORT`,
//!   `NOT_AUTHORIZED_CODE`, `NOT_AUTHORIZED_MESSAGE`.
//! - Error helpers: `not_authorized()`, `is_not_authorized(&Error)` (the
//!   `LocalSpotifyNotAuthorized` gate), `is_registration_timeout(&Error)`
//!   (`LocalDeviceRegistrationTimeout`), `is_stale_device_error(&Error)`
//!   ("Device not found": rebind the device once and retry).
//! - Mapping to `Track` (same kinds and meta keys as 1.x): `track_from`,
//!   `album_from`, `artist_from`, `playlist_from` (JSON object -> `Track`),
//!   `device_track` (a device row as the service lists it, kind "device").
//! - `SpotifyConfig`: the settings/token store (implemented for `Config`).
//! - `Spotify` (`Clone + Send + Sync`, all `&self`):
//!   - `new(&Config)`, `with_parts(cfg, local, Endpoints)`; `local` is the
//!     public `LocalSpotifyEngine` (`spotify.local.start(false)`,
//!     `.close()`, `.authorize(..)`, `.authorized()`, `.running()`,
//!     `.available()`, `.stale_receivers()`, ...).
//!   - setup: `client_id`, `enabled`, `token_data`, `connected`,
//!     `access_token` (refreshes), `connect(open_browser, timeout)` (PKCE,
//!     returns the `/me` profile), `disconnect`, `profile`.
//!   - raw: `request(method, path, params, body, timeout)`, `get`, `put`,
//!     `post` (JSON `Value`, `Null` for 204/empty).
//!   - catalog: `search(q, limit)`, `playlists`, `playlist_items(id)`,
//!     `album_tracks(id)`, `top_tracks`, `top_artists`, `recent`.
//!   - devices: `devices` (raw objects), `playback_device`,
//!     `preferred_device_id`, `wait_for_device(name, timeout, poll)`,
//!     `ensure_local_device(name, timeout)` (one rate-limited restart),
//!     `authorize_local(timeout, on_url, device_timeout)`,
//!     `activate_device(id, timeout)`, `transfer(id, play)`.
//!   - transport: `play(track, device_id, context_uri)` (context URI +
//!     offset, one 403 retry with the plain track URI), `play_track`,
//!     `resume`, `pause`, `next`, `previous`, `seek`, `volume`, `shuffle`,
//!     `repeat`, `queue_add`, `save`, `add_to_playlist`.
//!   - `state()` -> `PlayerState` (one `/me/player` poll; the engine owns the
//!     polling thread and runs it only while Spotify plays).

pub mod browser;
pub mod local;
mod sha1;

pub use browser::{browser_commands, open_authorization_url};
pub use local::{find_receivers, terminate_pid, EngineOptions, LocalSpotifyEngine};

use crate::config::Config;
use crate::core::{Error, ErrorKind, Result};
use crate::model::{value_bool, value_f64, value_i64, value_str, Meta, Track};
use crate::redact::{parse_qsl, urlencode};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const API: &str = "https://api.spotify.com/v1";
pub const AUTH: &str = "https://accounts.spotify.com/authorize";
pub const TOKEN: &str = "https://accounts.spotify.com/api/token";
pub const CALLBACK_PORT: u16 = 43821;
pub const SCOPES: &str = "user-read-private user-read-playback-state user-read-currently-playing user-modify-playback-state user-read-recently-played user-top-read user-library-read user-library-modify playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public";

pub const NOT_AUTHORIZED_CODE: &str = "spotify_local_unauthorized";
pub const NOT_AUTHORIZED_MESSAGE: &str =
    "Local Spotify playback is not authorized yet · choose Spotify → Local playback → AUTHORIZE";
const REGISTRATION_PREFIX: &str = "NPLAY local Spotify device did not register in time";
const MAX_BODY: u64 = 16 * 1024 * 1024;

/// `LocalSpotifyNotAuthorized`: no reusable librespot credentials cached.
pub fn not_authorized() -> Error {
    Error::new(ErrorKind::Authentication, NOT_AUTHORIZED_MESSAGE)
}

pub fn is_not_authorized(e: &Error) -> bool {
    e.message == NOT_AUTHORIZED_MESSAGE
}

/// `LocalDeviceRegistrationTimeout`: receiver alive but not visible.
pub fn is_registration_timeout(e: &Error) -> bool {
    e.message.starts_with(REGISTRATION_PREFIX)
}

/// A transport call hit a stale Connect device id.
pub fn is_stale_device_error(e: &Error) -> bool {
    e.message.contains("Device not found")
}

// --- mapping -------------------------------------------------------------------

fn obj(v: &Value) -> &serde_json::Map<String, Value> {
    static EMPTY: OnceLock<serde_json::Map<String, Value>> = OnceLock::new();
    v.as_object()
        .unwrap_or_else(|| EMPTY.get_or_init(serde_json::Map::new))
}

fn s(v: &Value, k: &str) -> String {
    obj(v).get(k).map(value_str).unwrap_or_default()
}

fn image(x: &Value) -> String {
    match obj(x)
        .get("images")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        Some(Value::Object(m)) => m.get("url").map(value_str).unwrap_or_default(),
        _ => String::new(),
    }
}

fn artists(x: &Value) -> String {
    obj(x)
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|v| v.is_object())
                .map(|v| s(v, "name"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn ext_url(x: &Value) -> String {
    obj(x)
        .get("external_urls")
        .map(|e| s(e, "spotify"))
        .unwrap_or_default()
}

fn new_track(kind: &str, id: String, title: String) -> Track {
    Track {
        source: "spotify".into(),
        id,
        title,
        kind: kind.into(),
        seekable: kind == "track",
        ..Default::default()
    }
}

/// A Web API track object.
pub fn track_from(x: &Value) -> Track {
    let album = obj(x).get("album").cloned().unwrap_or(Value::Null);
    let id = s(x, "id");
    let mut uri = s(x, "uri");
    if uri.is_empty() && !id.is_empty() {
        uri = format!("spotify:track:{id}");
    }
    let mut t = new_track("track", id, s(x, "name"));
    t.artist = artists(x);
    t.album = s(&album, "name");
    t.duration = obj(x).get("duration_ms").map(value_f64).unwrap_or(0.0) / 1000.0;
    t.cover = image(&album);
    t.set_meta("spotify_uri", uri);
    t.set_meta("spotify_url", ext_url(x));
    t.set_meta(
        "explicit",
        obj(x).get("explicit").map(value_bool).unwrap_or(false),
    );
    t
}

pub fn album_from(x: &Value) -> Track {
    let mut t = new_track("album", s(x, "id"), s(x, "name"));
    t.artist = artists(x);
    t.album = s(x, "name");
    t.cover = image(x);
    t.set_meta("spotify_uri", s(x, "uri"));
    t.set_meta("spotify_url", ext_url(x));
    t.set_meta("release_date", s(x, "release_date"));
    t
}

pub fn artist_from(x: &Value) -> Track {
    let mut t = new_track("artist", s(x, "id"), s(x, "name"));
    t.artist = "Artist".into();
    t.cover = image(x);
    t.set_meta("spotify_uri", s(x, "uri"));
    t.set_meta("spotify_url", ext_url(x));
    t
}

pub fn playlist_from(x: &Value) -> Track {
    let o = obj(x);
    let truthy = |k: &str| o.get(k).filter(|v| value_bool(v));
    let cnt = match truthy("items").or_else(|| truthy("tracks")) {
        Some(Value::Object(m)) => m.get("total").map(value_i64).unwrap_or(0),
        _ => 0,
    };
    let owner = o.get("owner").cloned().unwrap_or(Value::Null);
    let mut t = new_track("playlist", s(x, "id"), s(x, "name"));
    let dn = s(&owner, "display_name");
    t.artist = if dn.is_empty() { "Spotify".into() } else { dn };
    t.cover = image(x);
    t.set_meta("spotify_uri", s(x, "uri"));
    t.set_meta("spotify_url", ext_url(x));
    t.set_meta("item_count", cnt);
    t.set_meta("owner_id", s(&owner, "id"));
    t
}

/// A Connect device as a list row (kind "device", `meta.explicit` = active),
/// exactly as the 1.x service lists Spotify → Devices.
pub fn device_track(d: &Meta) -> Track {
    let g = |k: &str| d.get(k).map(value_str).unwrap_or_default();
    let mut t = new_track("device", g("id"), g("name"));
    t.artist = g("type");
    t.seekable = false;
    t.set_meta(
        "explicit",
        d.get("is_active").map(value_bool).unwrap_or(false),
    );
    t
}

fn uri_of(t: &Track) -> String {
    let u = t.meta_str("spotify_uri");
    if u.is_empty() {
        format!("spotify:track:{}", t.id)
    } else {
        u
    }
}

// --- config ----------------------------------------------------------------------

/// Settings and token storage used by `Spotify` (implemented for `Config`).
pub trait SpotifyConfig: Send + Sync {
    fn get(&self, k: &str, d: &str) -> String;
    fn getbool(&self, k: &str, d: bool) -> bool;
    fn tokens(&self) -> Vec<(String, String)>;
    fn save_tokens(&self, t: &[(&str, String)]);
    fn clear_tokens(&self);
}

impl SpotifyConfig for Config {
    fn get(&self, k: &str, d: &str) -> String {
        Config::get(self, k, d)
    }
    fn getbool(&self, k: &str, d: bool) -> bool {
        Config::getbool(self, k, d)
    }
    fn tokens(&self) -> Vec<(String, String)> {
        self.spotify_tokens()
    }
    fn save_tokens(&self, t: &[(&str, String)]) {
        self.save_spotify_tokens(t)
    }
    fn clear_tokens(&self) {
        self.clear_spotify_tokens()
    }
}

/// Web API / accounts endpoints (overridable for tests).
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub api: String,
    pub auth: String,
    pub token: String,
    pub callback_port: u16,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            api: API.into(),
            auth: AUTH.into(),
            token: TOKEN.into(),
            callback_port: CALLBACK_PORT,
        }
    }
}

/// One `/me/player` snapshot.
#[derive(Clone, Debug, Default)]
pub struct PlayerState {
    pub track: Option<Track>,
    pub is_playing: bool,
    pub progress: f64,
    pub duration: f64,
    pub device: Meta,
    pub shuffle: bool,
    pub repeat: String,
    pub context: Meta,
}

impl PlayerState {
    pub fn to_value(&self) -> Value {
        json!({
            "track": self.track.as_ref().map(Track::to_value),
            "is_playing": self.is_playing,
            "progress": self.progress,
            "duration": self.duration,
            "device": self.device,
            "shuffle": self.shuffle,
            "repeat": self.repeat,
            "context": self.context,
        })
    }
}

enum WaitError {
    Timeout(Error),
    Other(Error),
}

impl From<WaitError> for Error {
    fn from(w: WaitError) -> Error {
        match w {
            WaitError::Timeout(e) | WaitError::Other(e) => e,
        }
    }
}

/// The operations `ensure_local_device` composes (mockable in tests).
trait RecoveryOps {
    fn start(&self) -> Result<()>;
    fn wait(&self, name: &str, timeout: f64) -> std::result::Result<Meta, WaitError>;
    fn devices(&self) -> Result<Vec<Meta>>;
    fn restart(&self) -> Result<()>;
    fn now(&self) -> f64;
}

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::AgentBuilder::new().build())
}

fn user_agent() -> String {
    format!("NPLAY/{}", crate::VERSION)
}

fn read_body(r: ureq::Response) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = r.into_reader().take(MAX_BODY).read_to_end(&mut buf);
    buf
}

/// `_http_error`: Spotify's error message, else the HTTP reason.
fn http_error_detail(reason: &str, body: &[u8]) -> String {
    let Ok(d) = serde_json::from_slice::<Value>(body) else {
        return reason.to_string();
    };
    let Some(o) = d.as_object() else {
        return reason.to_string();
    };
    match o.get("error") {
        Some(Value::Object(e)) => e
            .get("message")
            .map(value_str)
            .unwrap_or_else(|| "None".into()),
        other => {
            for v in [o.get("error_description"), other] {
                if let Some(v) = v.filter(|v| value_bool(v)) {
                    return value_str(v);
                }
            }
            reason.to_string()
        }
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        let r = unsafe {
            libc::getrandom(
                buf[filled..].as_mut_ptr() as *mut libc::c_void,
                n - filled,
                0,
            )
        };
        if r <= 0 {
            // Extremely unlikely; fall back to the urandom device.
            if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
                let _ = f.read_exact(&mut buf[filled..]);
            }
            break;
        }
        filled += r as usize;
    }
    buf
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Spotify Web API + Connect handle.
#[derive(Clone)]
pub struct Spotify {
    cfg: Arc<dyn SpotifyConfig>,
    pub local: LocalSpotifyEngine,
    last_recovery: Arc<Mutex<f64>>,
    refresh: Arc<Mutex<()>>,
    endpoints: Arc<Endpoints>,
}

impl Spotify {
    pub fn new(cfg: &Config) -> Spotify {
        let local = LocalSpotifyEngine::new(cfg);
        Spotify::with_parts(Arc::new(cfg.clone()), local, Endpoints::default())
    }

    pub fn with_parts(
        cfg: Arc<dyn SpotifyConfig>,
        local: LocalSpotifyEngine,
        endpoints: Endpoints,
    ) -> Spotify {
        Spotify {
            cfg,
            local,
            last_recovery: Arc::new(Mutex::new(f64::NEG_INFINITY)),
            refresh: Arc::new(Mutex::new(())),
            endpoints: Arc::new(endpoints),
        }
    }

    fn local_name(&self) -> String {
        let n = self.cfg.get("spotify_local_name", "NPLAY");
        if n.is_empty() {
            "NPLAY".into()
        } else {
            n
        }
    }

    pub fn client_id(&self) -> String {
        self.cfg.get("spotify_client_id", "").trim().to_string()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.getbool("spotify_enabled", false)
    }

    pub fn token_data(&self) -> Meta {
        self.cfg
            .tokens()
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect()
    }

    fn token(&self, k: &str) -> String {
        self.cfg
            .tokens()
            .into_iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v)
            .unwrap_or_default()
    }

    pub fn connected(&self) -> bool {
        !self.client_id().is_empty() && !self.token("refresh_token").is_empty()
    }

    fn save_tokens(&self, d: &Value) {
        let mut merged: Vec<(&str, String)> = Vec::new();
        let old = self.cfg.tokens();
        for k in ["access_token", "refresh_token", "scope", "token_type"] {
            let v = match obj(d).get(k) {
                Some(v) if !v.is_null() => value_str(v),
                _ => match old.iter().find(|(n, _)| n == k) {
                    Some((_, v)) => v.clone(),
                    None => continue,
                },
            };
            merged.push((k, v));
        }
        let expires_in = obj(d).get("expires_in").map(value_f64).unwrap_or(3600.0);
        merged.push((
            "expires_at",
            format!("{}", crate::paths::now() + expires_in - 60.0),
        ));
        self.cfg.save_tokens(&merged);
    }

    fn post_form(&self, url: &str, pairs: &[(String, String)], what: &str) -> Result<Value> {
        let body = urlencode(pairs, "");
        let r = agent()
            .post(url)
            .timeout(Duration::from_secs(12))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("User-Agent", &user_agent())
            .send_string(&body);
        match r {
            Ok(resp) => {
                let raw = read_body(resp);
                serde_json::from_slice(&raw).map_err(|_| {
                    Error::new(
                        ErrorKind::Authentication,
                        format!("{what} · invalid response"),
                    )
                })
            }
            Err(ureq::Error::Status(_, resp)) => {
                let reason = resp.status_text().to_string();
                let raw = read_body(resp);
                Err(Error::new(
                    ErrorKind::Authentication,
                    format!("{what} · {}", http_error_detail(&reason, &raw)),
                ))
            }
            Err(e) => Err(Error::new(ErrorKind::Network, format!("{what} · {e}"))),
        }
    }

    /// A valid access token, refreshed with the refresh token when expired.
    pub fn access_token(&self) -> Result<String> {
        let _g = self.refresh.lock().unwrap_or_else(|p| p.into_inner());
        let at = self.token("access_token");
        let exp: f64 = self.token("expires_at").trim().parse().unwrap_or(0.0);
        if !at.is_empty() && exp > crate::paths::now() {
            return Ok(at);
        }
        let refresh = self.token("refresh_token");
        if refresh.is_empty() {
            return Err(Error::new(
                ErrorKind::Authentication,
                "Spotify is not connected",
            ));
        }
        let out = self.post_form(
            &self.endpoints.token,
            &[
                ("grant_type".into(), "refresh_token".into()),
                ("refresh_token".into(), refresh),
                ("client_id".into(), self.client_id()),
            ],
            "Spotify token refresh failed",
        )?;
        let token = s(&out, "access_token");
        if token.is_empty() {
            return Err(Error::new(
                ErrorKind::Authentication,
                "Spotify token refresh failed · no access token",
            ));
        }
        self.save_tokens(&out);
        Ok(token)
    }

    /// One Web API call. `Null` for 204/empty, a string for non-JSON.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
        timeout: f64,
    ) -> Result<Value> {
        let mut url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.endpoints.api, path)
        };
        if !params.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            let pairs: Vec<(String, String)> = params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            url.push_str(&urlencode(&pairs, ""));
        }
        let token = self.access_token()?;
        let req = agent()
            .request(method, &url)
            .timeout(Duration::from_secs_f64(timeout.max(0.1)))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json")
            .set("User-Agent", &user_agent());
        let r = match body {
            Some(b) => req.send_string(&b.to_string()),
            None if method == "GET" => req.call(),
            None => req.send_bytes(&[]),
        };
        match r {
            Ok(resp) => {
                let status = resp.status();
                let ctype = resp.header("Content-Type").unwrap_or("").to_lowercase();
                let raw = read_body(resp);
                // Player commands commonly return 204 No Content; an empty
                // successful response is success, never a JSON error.
                if status == 204 || raw.iter().all(|b| b.is_ascii_whitespace()) {
                    return Ok(Value::Null);
                }
                if !ctype.contains("json") {
                    return Ok(Value::String(String::from_utf8_lossy(&raw).into_owned()));
                }
                serde_json::from_slice(&raw).map_err(|_| {
                    Error::new(
                        ErrorKind::Provider,
                        format!(
                            "Spotify returned invalid JSON for {method} {path} · HTTP {status}"
                        ),
                    )
                })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let reason = resp.status_text().to_string();
                let raw = read_body(resp);
                let detail = http_error_detail(&reason, &raw);
                if code == 429 {
                    return Err(Error::new(
                        ErrorKind::Network,
                        "Spotify rate limit · try again shortly",
                    ));
                }
                if code == 403 && detail.contains("Restriction violated") {
                    return Err(Error::new(ErrorKind::Playback, "Spotify 403 · playback restricted by Spotify · verify Premium, account/device and Spotify Connect in Diagnostics; retry from Spotify app. This is not a librespot registration timeout"));
                }
                let kind = if code == 401 {
                    ErrorKind::Authentication
                } else {
                    ErrorKind::Provider
                };
                Err(Error::new(kind, format!("Spotify {code} · {detail}")))
            }
            Err(e) => Err(Error::new(
                ErrorKind::Network,
                format!("Spotify network error · {e}"),
            )),
        }
    }

    pub fn get(&self, p: &str, params: &[(&str, String)]) -> Result<Value> {
        self.request("GET", p, params, None, 12.0)
    }

    pub fn put(&self, p: &str, params: &[(&str, String)], body: Option<&Value>) -> Result<Value> {
        self.request("PUT", p, params, body, 12.0)
    }

    pub fn post(&self, p: &str, params: &[(&str, String)], body: Option<&Value>) -> Result<Value> {
        self.request("POST", p, params, body, 12.0)
    }

    pub fn profile(&self) -> Result<Value> {
        self.get("/me", &[])
    }

    /// Authorization Code with PKCE; returns the `/me` profile.
    pub fn connect(&self, open_browser: bool, timeout: f64) -> Result<Value> {
        self.connect_with(open_browser, timeout, &open_authorization_url)
    }

    fn connect_with(
        &self,
        open_browser: bool,
        timeout: f64,
        opener: &dyn Fn(&str) -> bool,
    ) -> Result<Value> {
        let cid = self.client_id();
        if cid.is_empty() {
            return Err(Error::new(
                ErrorKind::Configuration,
                "Spotify Client ID is not configured",
            ));
        }
        let verifier = b64url(&random_bytes(64));
        let challenge = b64url(&sha2::Sha256::digest(verifier.as_bytes()));
        let state = b64url(&random_bytes(24));
        let port = self.endpoints.callback_port;
        let server = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            Error::new(
                ErrorKind::Configuration,
                format!("Spotify callback port {port} is unavailable · {e}"),
            )
        })?;
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let pairs: Vec<(String, String)> = [
            ("client_id", cid.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("scope", SCOPES),
            ("state", state.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let url = format!("{}?{}", self.endpoints.auth, urlencode(&pairs, ""));
        // The listener is dropped (port released) on every path out of here.
        if open_browser && !opener(&url) {
            drop(server);
            return Err(Error::new(
                ErrorKind::Authentication,
                format!("Spotify authorization URL could not be opened in a browser. Open this URL manually: {url}"),
            ));
        }
        let result = serve_callback(&server, timeout);
        drop(server);
        if result.is_empty() {
            return Err(Error::new(
                ErrorKind::Authentication,
                format!(
                    "Spotify authorization timed out after {}s · retry and complete authorization in the browser",
                    py_num(timeout)
                ),
            ));
        }
        let get = |k: &str| {
            result
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        if get("state") != state {
            return Err(Error::new(
                ErrorKind::Authentication,
                "Spotify authorization state mismatch · retry and complete authorization in the browser",
            ));
        }
        if !get("error").is_empty() {
            return Err(Error::new(
                ErrorKind::Authentication,
                format!("Spotify authorization denied · {}", get("error")),
            ));
        }
        let code = get("code");
        if code.is_empty() {
            return Err(Error::new(
                ErrorKind::Authentication,
                "Spotify authorization returned no code · retry authorization",
            ));
        }
        let tokens = self.post_form(
            &self.endpoints.token,
            &[
                ("client_id".into(), cid),
                ("grant_type".into(), "authorization_code".into()),
                ("code".into(), code),
                ("redirect_uri".into(), redirect),
                ("code_verifier".into(), verifier),
            ],
            "Spotify token exchange failed",
        )?;
        self.save_tokens(&tokens);
        self.profile()
    }

    pub fn disconnect(&self) {
        self.cfg.clear_tokens();
    }

    /// Search artists, albums, tracks and playlists; each group ranked
    /// locally so exact and name-token matches come first.
    pub fn search(&self, q: &str, limit: usize) -> Result<Vec<Track>> {
        // Dev Mode search max is 10/request as of 2026.
        let d = self.get(
            "/search",
            &[
                ("q", q.to_string()),
                ("type", "artist,album,track,playlist".into()),
                ("limit", limit.clamp(1, 10).to_string()),
            ],
        )?;
        Ok(rank_search(q, &d))
    }

    fn items(v: &Value) -> Vec<Value> {
        obj(v)
            .get("items")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|x| value_bool(x)).cloned().collect())
            .unwrap_or_default()
    }

    pub fn playlists(&self) -> Result<Vec<Track>> {
        let d = self.get("/me/playlists", &[("limit", "50".into())])?;
        Ok(Self::items(&d).iter().map(playlist_from).collect())
    }

    /// Every track of a playlist (pages followed, at most 500), with the
    /// playlist context in `meta.context_uri/context_kind/context_index`.
    pub fn playlist_items(&self, pid: &str) -> Result<Vec<Track>> {
        let mut out: Vec<Track> = Vec::new();
        let context_uri = format!("spotify:playlist:{pid}");
        let mut url = format!("/playlists/{pid}/items");
        let mut first = true;
        while !url.is_empty() && out.len() < 500 {
            let params: Vec<(&str, String)> = if first {
                vec![("limit", "50".into())]
            } else {
                vec![]
            };
            first = false;
            let d = self.get(&url, &params)?;
            for row in Self::items(&d) {
                let x = if row.is_object() {
                    let o = obj(&row);
                    o.get("item")
                        .filter(|v| value_bool(v))
                        .or_else(|| o.get("track").filter(|v| value_bool(v)))
                        .cloned()
                        .unwrap_or_else(|| row.clone())
                } else {
                    continue;
                };
                if x.is_object() && s(&x, "type") == "track" {
                    let mut t = track_from(&x);
                    t.set_meta("context_uri", context_uri.clone());
                    t.set_meta("context_kind", "playlist");
                    t.set_meta("context_index", out.len());
                    out.push(t);
                }
            }
            url = s(&d, "next");
        }
        Ok(out)
    }

    /// An album's tracks, enriched with the album name and artwork.
    pub fn album_tracks(&self, aid: &str) -> Result<Vec<Track>> {
        let album = self.get(&format!("/albums/{aid}"), &[])?;
        let d = self.get(&format!("/albums/{aid}/tracks"), &[("limit", "50".into())])?;
        let parent = json!({
            "id": aid,
            "name": s(&album, "name"),
            "images": obj(&album).get("images").filter(|v| value_bool(v)).cloned().unwrap_or(json!([])),
        });
        let mut out = Vec::new();
        for x in Self::items(&d) {
            let mut m = obj(&x).clone();
            m.insert("album".into(), parent.clone());
            let mut t = track_from(&Value::Object(m));
            t.set_meta("context_uri", format!("spotify:album:{aid}"));
            t.set_meta("context_kind", "album");
            t.set_meta("context_index", out.len());
            out.push(t);
        }
        Ok(out)
    }

    pub fn top_tracks(&self) -> Result<Vec<Track>> {
        let d = self.get(
            "/me/top/tracks",
            &[("limit", "50".into()), ("time_range", "medium_term".into())],
        )?;
        Ok(Self::items(&d).iter().map(track_from).collect())
    }

    pub fn top_artists(&self) -> Result<Vec<Track>> {
        let d = self.get(
            "/me/top/artists",
            &[("limit", "50".into()), ("time_range", "medium_term".into())],
        )?;
        Ok(Self::items(&d).iter().map(artist_from).collect())
    }

    pub fn recent(&self) -> Result<Vec<Track>> {
        let d = self.get("/me/player/recently-played", &[("limit", "50".into())])?;
        Ok(Self::items(&d)
            .iter()
            .filter_map(|row| {
                obj(row)
                    .get("track")
                    .filter(|t| value_bool(t))
                    .map(track_from)
            })
            .collect())
    }

    /// Raw Connect device objects (`id`, `name`, `type`, `is_active`,
    /// `is_restricted`, `volume_percent`, ...).
    pub fn devices(&self) -> Result<Vec<Meta>> {
        let d = self.get("/me/player/devices", &[])?;
        Ok(obj(&d)
            .get("devices")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
            .unwrap_or_default())
    }

    /// The best usable Connect device, without guessing across many.
    pub fn playback_device(&self) -> Result<Meta> {
        let preferred = self.preferred_device_id();
        pick_playback_device(self.devices()?, &preferred)
    }

    pub fn preferred_device_id(&self) -> String {
        self.cfg.get("spotify_device_id", "").trim().to_string()
    }

    pub fn play_track(&self, t: &Track) -> Result<Meta> {
        let d = self.playback_device()?;
        let id = d.get("id").map(value_str).unwrap_or_default();
        self.play(Some(t), Some(&id), None)?;
        Ok(d)
    }

    /// One playback-state poll (`GET /me/player`).
    pub fn state(&self) -> Result<PlayerState> {
        Ok(parse_state(&self.get("/me/player", &[])?))
    }

    /// Wait until the local receiver is visible to the Web API and return
    /// its device. Registration is eventually consistent: a running process
    /// is never proof that Spotify accepts Player commands.
    pub fn wait_for_device(&self, name: Option<&str>, timeout: f64, poll: f64) -> Result<Meta> {
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| self.local_name());
        self.wait_inner(&name, timeout, poll).map_err(Error::from)
    }

    fn wait_inner(
        &self,
        name: &str,
        timeout: f64,
        poll: f64,
    ) -> std::result::Result<Meta, WaitError> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(1.0));
        let mut last: Vec<Meta> = Vec::new();
        while Instant::now() < deadline {
            if !self.local.running() {
                return Err(WaitError::Other(Error::new(
                    ErrorKind::Playback,
                    "Local Spotify engine stopped · open Spotify → Local playback → Diagnostics",
                )));
            }
            match self.devices() {
                Ok(ds) => {
                    last = ds;
                    if let Some(hit) = last.iter().find(|d| usable(d) && dname(d) == name) {
                        return Ok(hit.clone());
                    }
                }
                Err(e) if e.kind == ErrorKind::Network && !e.message.contains("rate limit") => {
                    return Err(WaitError::Other(e))
                }
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_secs_f64(poll.max(0.05)));
        }
        let names: Vec<String> = last.iter().map(dname).filter(|n| !n.is_empty()).collect();
        let names = if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        };
        warn!(
            "librespot device registration timeout · visible_devices={names} · engine_running={}",
            if self.local.running() {
                "True"
            } else {
                "False"
            }
        );
        Err(WaitError::Timeout(Error::new(
            ErrorKind::Playback,
            format!("{REGISTRATION_PREFIX} · visible devices: {names} · use Spotify → Local playback → Diagnostics; if authorization is required choose AUTHORIZE / REAUTHORIZE"),
        )))
    }

    /// Start (or adopt) the local receiver and wait for its device; recover
    /// once from a running-but-unregistered receiver (rate limited to one
    /// restart per 90 s). External devices are never restarted.
    pub fn ensure_local_device(&self, name: Option<&str>, timeout: f64) -> Result<Meta> {
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| self.local_name());
        struct Ops<'a>(&'a Spotify);
        impl RecoveryOps for Ops<'_> {
            fn start(&self) -> Result<()> {
                self.0.local.start(false).map(|_| ())
            }
            fn wait(&self, name: &str, timeout: f64) -> std::result::Result<Meta, WaitError> {
                self.0.wait_inner(name, timeout, 0.5)
            }
            fn devices(&self) -> Result<Vec<Meta>> {
                self.0.devices()
            }
            fn restart(&self) -> Result<()> {
                self.0.local.restart().map(|_| ())
            }
            fn now(&self) -> f64 {
                crate::paths::monotonic()
            }
        }
        ensure_with(&Ops(self), &self.last_recovery, &name, timeout)
    }

    /// Shared terminal/service OAuth flow for the local receiver. Success
    /// means librespot cached fresh credentials; with the Web API connected
    /// too, wait (bounded) for the device and return it, else `{}`.
    pub fn authorize_local(
        &self,
        timeout: f64,
        on_url: Option<&dyn Fn(&str)>,
        device_timeout: f64,
    ) -> Result<Meta> {
        self.local.authorize(Some(timeout), None, on_url)?;
        if !self.connected() {
            return Ok(Meta::new());
        }
        let name = self.local_name();
        match self.wait_for_device(Some(&name), device_timeout, 0.5) {
            Ok(d) => Ok(d),
            Err(e) => {
                warn!("local Spotify authorized but device not visible yet · {e}");
                Ok(Meta::new())
            }
        }
    }

    /// Transfer playback and wait until Spotify reports the device active.
    pub fn activate_device(&self, device_id: &str, timeout: f64) -> Result<Meta> {
        if device_id.is_empty() {
            return Err(Error::new(
                ErrorKind::Playback,
                "Spotify device has no device ID",
            ));
        }
        self.transfer(device_id, false)?;
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(1.0));
        while Instant::now() < deadline {
            match self.devices() {
                Ok(ds) => {
                    if let Some(hit) = ds.iter().find(|d| did(d) == device_id) {
                        if hit.get("is_active").map(value_bool).unwrap_or(false) {
                            return Ok(hit.clone());
                        }
                    }
                }
                Err(e) if e.kind == ErrorKind::Network && !e.message.contains("rate limit") => {
                    return Err(e)
                }
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_millis(350));
        }
        // Some backends only become active on the first explicit play; the
        // concrete device is still safe because play() targets it.
        Ok(self
            .devices()?
            .into_iter()
            .find(|d| did(d) == device_id)
            .unwrap_or_else(|| {
                let mut m = Meta::new();
                m.insert("id".into(), Value::String(device_id.into()));
                m
            }))
    }

    /// Play a track (inside its album/playlist context when it has one, with
    /// the track as offset), or a context alone.
    pub fn play(
        &self,
        t: Option<&Track>,
        device_id: Option<&str>,
        context_uri: Option<&str>,
    ) -> Result<Value> {
        let dp = dev(device_id);
        let mut body = serde_json::Map::new();
        let ctx = context_uri
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .or_else(|| {
                t.map(|t| t.meta_str("context_uri"))
                    .filter(|c| !c.is_empty())
            });
        if let Some(c) = &ctx {
            body.insert("context_uri".into(), json!(c));
            if let Some(t) = t {
                body.insert("offset".into(), json!({"uri": uri_of(t)}));
            }
        } else if let Some(t) = t {
            body.insert("uris".into(), json!([uri_of(t)]));
        }
        let b = (!body.is_empty()).then(|| Value::Object(body));
        match self.put("/me/player/play", &dp, b.as_ref()) {
            Ok(v) => Ok(v),
            Err(e) => {
                // Context resolution can fail in librespot after reconnect:
                // retry exactly once with the plain track URI.
                if let (Some(_), Some(t)) = (&ctx, t) {
                    if e.message.contains("Spotify 403") {
                        let uri = uri_of(t);
                        if !uri.starts_with("spotify:track:") {
                            return Err(e);
                        }
                        warn!("Spotify rejected context playback (403); retrying track URI only");
                        return self.put("/me/player/play", &dp, Some(&json!({"uris": [uri]})));
                    }
                }
                Err(e)
            }
        }
    }

    /// Resume without a URI or context (sending one restarts at 0:00).
    pub fn resume(&self, device_id: Option<&str>) -> Result<Value> {
        self.put("/me/player/play", &dev(device_id), None)
    }

    pub fn pause(&self, device_id: Option<&str>) -> Result<Value> {
        self.put("/me/player/pause", &dev(device_id), None)
    }

    pub fn next(&self, device_id: Option<&str>) -> Result<Value> {
        self.post("/me/player/next", &dev(device_id), None)
    }

    pub fn previous(&self, device_id: Option<&str>) -> Result<Value> {
        self.post("/me/player/previous", &dev(device_id), None)
    }

    pub fn seek(&self, seconds: f64, device_id: Option<&str>) -> Result<Value> {
        let mut p = vec![(
            "position_ms",
            ((seconds * 1000.0) as i64).max(0).to_string(),
        )];
        p.extend(dev(device_id));
        self.put("/me/player/seek", &p, None)
    }

    pub fn volume(&self, pct: f64, device_id: Option<&str>) -> Result<Value> {
        let mut p = vec![("volume_percent", (pct as i64).clamp(0, 100).to_string())];
        p.extend(dev(device_id));
        self.put("/me/player/volume", &p, None)
    }

    pub fn shuffle(&self, on: bool, device_id: Option<&str>) -> Result<Value> {
        let mut p = vec![("state", on.to_string())];
        p.extend(dev(device_id));
        self.put("/me/player/shuffle", &p, None)
    }

    /// `mode`: "track", "context" or "off".
    pub fn repeat(&self, mode: &str, device_id: Option<&str>) -> Result<Value> {
        let mode = if matches!(mode, "track" | "context") {
            mode
        } else {
            "off"
        };
        let mut p = vec![("state", mode.to_string())];
        p.extend(dev(device_id));
        self.put("/me/player/repeat", &p, None)
    }

    pub fn transfer(&self, device_id: &str, play: bool) -> Result<Value> {
        self.put(
            "/me/player",
            &[],
            Some(&json!({"device_ids": [device_id], "play": play})),
        )
    }

    pub fn queue_add(&self, t: &Track) -> Result<Value> {
        self.post("/me/player/queue", &[("uri", uri_of(t))], None)
    }

    pub fn save(&self, t: &Track) -> Result<Value> {
        let u = t.meta_str("spotify_uri");
        let uri = if u.is_empty() {
            format!("spotify:{}:{}", t.kind, t.id)
        } else {
            u
        };
        self.put("/me/library", &[("uris", uri)], None)
    }

    pub fn add_to_playlist(&self, pid: &str, t: &Track) -> Result<Value> {
        self.post(
            &format!("/playlists/{pid}/items"),
            &[("uris", uri_of(t))],
            None,
        )
    }
}

/// Python `str()` of an int or float seconds value (`180`, `1.5`).
fn py_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn dev(device_id: Option<&str>) -> Vec<(&'static str, String)> {
    match device_id {
        Some(d) if !d.is_empty() => vec![("device_id", d.to_string())],
        _ => vec![],
    }
}

fn did(d: &Meta) -> String {
    d.get("id").map(value_str).unwrap_or_default()
}

fn dname(d: &Meta) -> String {
    d.get("name").map(value_str).unwrap_or_default()
}

fn usable(d: &Meta) -> bool {
    !did(d).is_empty() && !d.get("is_restricted").map(value_bool).unwrap_or(false)
}

fn pick_playback_device(all: Vec<Meta>, preferred: &str) -> Result<Meta> {
    let ds: Vec<Meta> = all.into_iter().filter(usable).collect();
    if let Some(a) = ds
        .iter()
        .find(|d| d.get("is_active").map(value_bool).unwrap_or(false))
    {
        return Ok(a.clone());
    }
    if !preferred.is_empty() {
        if let Some(hit) = ds.iter().find(|d| did(d) == preferred) {
            return Ok(hit.clone());
        }
    }
    match ds.len() {
        1 => Ok(ds[0].clone()),
        0 => Err(Error::new(ErrorKind::Playback, "No Spotify Connect device available · open Spotify on a phone, desktop or web player, then use Spotify → Devices")),
        _ => Err(Error::new(ErrorKind::Playback, "No active Spotify Connect device · choose one in Spotify → Devices")),
    }
}

fn parse_state(d: &Value) -> PlayerState {
    let o = obj(d);
    let x = o
        .get("item")
        .filter(|v| value_bool(v))
        .cloned()
        .unwrap_or(Value::Null);
    let track = (x.is_object() && s(&x, "type") == "track").then(|| track_from(&x));
    PlayerState {
        track,
        is_playing: o.get("is_playing").map(value_bool).unwrap_or(false),
        progress: o.get("progress_ms").map(value_f64).unwrap_or(0.0) / 1000.0,
        duration: obj(&x).get("duration_ms").map(value_f64).unwrap_or(0.0) / 1000.0,
        device: o
            .get("device")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        shuffle: o.get("shuffle_state").map(value_bool).unwrap_or(false),
        repeat: match o.get("repeat_state") {
            Some(v) => value_str(v),
            None => "off".into(),
        },
        context: o
            .get("context")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

type Mapper = fn(&Value) -> Track;

fn rank_search(q: &str, d: &Value) -> Vec<Track> {
    let query = norm(q);
    let qt: std::collections::HashSet<&str> = query.split(' ').filter(|w| !w.is_empty()).collect();
    let score = |t: &Track| -> i64 {
        let title = norm(&t.title);
        let artist = norm(&t.artist);
        let both = format!("{title} {artist}");
        let words: std::collections::HashSet<&str> = both.split_whitespace().collect();
        let overlap = qt.intersection(&words).count() as i64;
        (if title == query { 1000 } else { 0 })
            + (if title.starts_with(&query) { 500 } else { 0 })
            + 120 * overlap
            + (if !query.is_empty() && title.contains(&query) {
                30
            } else {
                0
            })
    };
    let mut out = Vec::new();
    let groups: [(&str, Mapper); 4] = [
        ("artists", artist_from),
        ("albums", album_from),
        ("tracks", track_from),
        ("playlists", playlist_from),
    ];
    for (key, f) in groups {
        let part: Vec<Track> = obj(d)
            .get(key)
            .map(Spotify::items)
            .unwrap_or_default()
            .iter()
            .map(f)
            .collect();
        let mut scored: Vec<(i64, usize, Track)> = part
            .into_iter()
            .enumerate()
            .map(|(i, t)| (score(&t), i, t))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        // Artist search is noisy: drop zero-token matches when at least one
        // genuine name match exists. Other groups keep everything.
        let any_hit = scored.iter().any(|z| z.0 > 0);
        out.extend(
            scored
                .into_iter()
                .filter(|z| key != "artists" || !any_hit || z.0 > 0)
                .map(|z| z.2),
        );
    }
    out
}

fn ensure_with(ops: &dyn RecoveryOps, last: &Mutex<f64>, name: &str, timeout: f64) -> Result<Meta> {
    ops.start()?;
    let first = match ops.wait(name, timeout) {
        Ok(d) => return Ok(d),
        Err(WaitError::Other(e)) => return Err(e),
        Err(WaitError::Timeout(e)) => e,
    };
    let mut g = last.lock().unwrap_or_else(|p| p.into_inner());
    // Another request may have recovered the receiver while we waited.
    if let Ok(ds) = ops.devices() {
        if let Some(d) = ds.into_iter().find(|d| usable(d) && dname(d) == name) {
            return Ok(d);
        }
    }
    let now = ops.now();
    if now - *g < 90.0 {
        return Err(Error::new(
            first.kind,
            format!("{} · automatic recovery cooldown active", first.message),
        ));
    }
    *g = now;
    warn!("Spotify Connect missing: restarting owned librespot once");
    ops.restart()?;
    match ops.wait(name, timeout) {
        Ok(d) => Ok(d),
        Err(WaitError::Timeout(e)) => Err(Error::new(
            ErrorKind::Playback,
            format!(
                "Spotify Connect unavailable after one automatic local receiver restart · {}",
                e.message
            ),
        )),
        Err(WaitError::Other(e)) => Err(e),
    }
}

/// Serve the loopback callback until a `/callback` with a query arrives or
/// the timeout passes. Returns the first value of each query key (blank
/// values dropped, like `parse_qs`).
fn serve_callback(server: &std::net::TcpListener, timeout: f64) -> Vec<(String, String)> {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    let mut result: Vec<(String, String)> = Vec::new();
    while Instant::now() < deadline && result.is_empty() {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(1));
        let mut pfd = libc::pollfd {
            fd: server.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as libc::c_int) };
        if r <= 0 {
            continue;
        }
        let Ok((mut stream, _)) = server.accept() else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        while buf.len() < 16 * 1024 && !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let line = head.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = path.split('#').next().unwrap_or("");
        let respond = |stream: &mut std::net::TcpStream, code: &str, ctype: &str, body: &[u8]| {
            let h = format!(
                "HTTP/1.0 {code}\r\nServer: NPLAY\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(h.as_bytes());
            let _ = stream.write_all(body);
        };
        if method != "GET" {
            respond(
                &mut stream,
                "501 Unsupported method",
                "text/plain; charset=utf-8",
                b"Unsupported method",
            );
            continue;
        }
        // Only the registered callback may set the result; browsers also
        // request /favicon.ico, which must never alter OAuth state.
        if path != "/callback" {
            respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Not found",
            );
            continue;
        }
        let query = query.split('#').next().unwrap_or("");
        for (k, v) in parse_qsl(query) {
            if !v.is_empty() && !result.iter().any(|(n, _)| *n == k) {
                result.push((k, v));
            }
        }
        let ok = result.iter().any(|(k, _)| k == "code");
        let msg = if ok {
            "NPLAY Spotify connected. You can close this tab and return to the terminal."
        } else {
            "NPLAY Spotify authorization was not completed."
        };
        let body = format!(
            "<html><body style=\"background:#111;color:#eee;font:16px sans-serif;padding:3rem\"><h2>{msg}</h2></body></html>"
        );
        respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            body.as_bytes(),
        );
    }
    result
}

#[cfg(test)]
mod tests;
