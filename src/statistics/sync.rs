//! Opt-in HTTPS adapter for the verified n.rudolfsson.net 0.6.4 API v3.
//! Transport is isolated from playback. No token is logged or shown.
//! Port of 1.x `statistics/sync.py`.

use super::adapter::pending_envelope_rows;
use super::store::StatisticsStore;
use super::time;
use crate::config::Config;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const DEFAULT_URL: &str = "https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3";
pub const MAX_PAYLOAD: usize = 262144;
pub const TIMEOUT_SECS: u64 = 12;

/// The sync error; its message is what `nplay --stats-sync-now` prints
/// after "Sync failed: ".
#[derive(Clone, Debug, PartialEq)]
pub struct SyncError(pub String);

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SyncError {}

/// What a transport can fail with (1.x HTTPError / URLError+OSError /
/// ValueError+SyncError).
#[derive(Clone, Debug, PartialEq)]
pub enum TransportError {
    Http(u16),
    Network(String),
    Invalid(String),
}

/// `(url, token, payload) -> parsed JSON response`.
pub type Transport = Arc<dyn Fn(&str, &str, &Value) -> Result<Value, TransportError> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Callback = Arc<dyn Fn() + Send + Sync>;

/// The configuration the sync reads (implemented by `Config`).
pub trait SyncConfig: Send + Sync {
    fn getbool(&self, k: &str, d: bool) -> bool;
    fn get(&self, k: &str, d: &str) -> String;
    fn statistics_token(&self) -> String;
}

impl SyncConfig for Config {
    fn getbool(&self, k: &str, d: bool) -> bool {
        Config::getbool(self, k, d)
    }
    fn get(&self, k: &str, d: &str) -> String {
        Config::get(self, k, d)
    }
    fn statistics_token(&self) -> String {
        Config::statistics_token(self)
    }
}

/// Minimal `urlsplit` for the checks 1.x makes.
struct Split {
    scheme: String,
    hostname: String,
    userinfo: bool,
    fragment: bool,
}

fn urlsplit(url: &str) -> Split {
    let (scheme, rest) = match url.find(':') {
        Some(i)
            if i > 0
                && url[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                && url[..1].chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            (url[..i].to_lowercase(), &url[i + 1..])
        }
        _ => (String::new(), url),
    };
    let (before_frag, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], !rest[i + 1..].is_empty()),
        None => (rest, false),
    };
    let mut hostname = String::new();
    let mut userinfo = false;
    if let Some(after) = before_frag.strip_prefix("//") {
        let end = after.find(['/', '?']).unwrap_or(after.len());
        let netloc = &after[..end];
        let host = match netloc.rfind('@') {
            Some(i) => {
                userinfo = true;
                &netloc[i + 1..]
            }
            None => netloc,
        };
        hostname = if let Some(h) = host.strip_prefix('[') {
            h.split(']').next().unwrap_or("").to_lowercase()
        } else {
            host.split(':').next().unwrap_or("").to_lowercase()
        };
    }
    Split {
        scheme,
        hostname,
        userinfo,
        fragment,
    }
}

/// The real transport: HTTPS POST with a 12 s timeout, CA and hostname
/// verification (rustls), no redirects (the token is never forwarded) and
/// bounded request and response sizes.
pub fn https_send(url: &str, token: &str, payload: &Value) -> Result<Value, TransportError> {
    let raw = serde_json::to_vec(payload).map_err(|e| TransportError::Invalid(e.to_string()))?;
    if raw.len() > MAX_PAYLOAD {
        return Err(TransportError::Invalid(
            "Batch exceeds server payload limit".into(),
        ));
    }
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build();
    let res = agent
        .post(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .set(
            "User-Agent",
            &format!("NPLAY statistics/{}", crate::VERSION),
        )
        .send_bytes(&raw);
    match res {
        Ok(r) => {
            if (300..400).contains(&r.status()) {
                return Err(TransportError::Invalid(
                    "Unexpected redirect; token not forwarded".into(),
                ));
            }
            if r.status() != 200 {
                return Err(TransportError::Invalid("Unexpected server response".into()));
            }
            let mut body = Vec::new();
            r.into_reader()
                .take(MAX_PAYLOAD as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| TransportError::Network("read failed".into()))?;
            serde_json::from_slice(&body).map_err(|e| TransportError::Invalid(e.to_string()))
        }
        Err(ureq::Error::Status(code, _)) => Err(TransportError::Http(code)),
        Err(ureq::Error::Transport(t)) => Err(TransportError::Network(t.kind().to_string())),
    }
}

struct State {
    status: String,
    last_error: String,
    last_success: f64,
    next_retry: f64,
    failures: u32,
}

struct Inner {
    store: StatisticsStore,
    cfg: Arc<dyn SyncConfig>,
    transport: Transport,
    clock: Clock,
    state: Mutex<State>,
    busy: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// Clone + Send + Sync handle; status strings are `DISABLED`, `IDLE`,
/// `SYNCING` and `ERROR` like 1.x.
#[derive(Clone)]
pub struct StatisticsSync {
    inner: Arc<Inner>,
}

fn http_message(code: u16) -> String {
    match code {
        401 => "Authorization required".into(),
        403 => "Invalid token, scope or installation binding".into(),
        413 => "Batch too large".into(),
        422 => "Server rejected an event".into(),
        429 => "Server rate limit".into(),
        503 => "Server storage unavailable".into(),
        c => format!("HTTP {c}"),
    }
}

impl StatisticsSync {
    pub fn new(store: StatisticsStore, cfg: Config) -> StatisticsSync {
        StatisticsSync::with_options(store, Arc::new(cfg), None, None)
    }

    pub fn with_options(
        store: StatisticsStore,
        cfg: Arc<dyn SyncConfig>,
        transport: Option<Transport>,
        clock: Option<Clock>,
    ) -> StatisticsSync {
        let last_success = store
            .get_meta("last_success", "0")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        StatisticsSync {
            inner: Arc::new(Inner {
                store,
                cfg,
                transport: transport.unwrap_or_else(|| Arc::new(https_send)),
                clock: clock.unwrap_or_else(|| Arc::new(crate::paths::now)),
                state: Mutex::new(State {
                    status: "DISABLED".into(),
                    last_error: String::new(),
                    last_success,
                    next_retry: 0.0,
                    failures: 0,
                }),
                busy: AtomicBool::new(false),
                thread: Mutex::new(None),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> String {
        self.st().status.clone()
    }
    pub fn set_status(&self, s: &str) {
        self.st().status = s.to_string();
    }
    pub fn last_error(&self) -> String {
        self.st().last_error.clone()
    }
    pub fn last_success(&self) -> f64 {
        self.st().last_success
    }
    pub fn next_retry(&self) -> f64 {
        self.st().next_retry
    }
    pub fn failures(&self) -> u32 {
        self.st().failures
    }
    pub fn clock(&self) -> f64 {
        (self.inner.clock)()
    }

    pub fn enabled(&self) -> bool {
        self.inner.cfg.getbool("statistics_sync_enabled", false)
    }
    pub fn token(&self) -> String {
        self.inner.cfg.statistics_token()
    }
    pub fn url(&self) -> String {
        let u = self.inner.cfg.get("statistics_sync_url", DEFAULT_URL);
        if u.is_empty() {
            DEFAULT_URL.into()
        } else {
            u
        }
    }

    fn failed(&self, msg: &str, retry: bool, token: &str) -> SyncError {
        let mut clean = crate::redact::redact_text(msg);
        if !token.is_empty() {
            clean = clean.replace(token, "[redacted]");
        }
        let now = self.clock();
        let mut s = self.st();
        s.status = "ERROR".into();
        s.last_error = clean.chars().take(160).collect();
        s.failures += 1;
        s.next_retry = if retry {
            let backoff =
                (10.0 * 2f64.powi(s.failures.saturating_sub(1).min(8) as i32)).min(3600.0);
            now + backoff + fastrand::f64() * 3.0
        } else {
            f64::INFINITY
        };
        SyncError(clean)
    }

    /// Send one batch (at most 50 events); returns how many the server
    /// acknowledged. `nplay --stats-sync-now` prints `Acknowledged: <n>`.
    pub fn once(&self) -> Result<usize, SyncError> {
        if !self.enabled() {
            self.set_status("DISABLED");
            return Ok(0);
        }
        let token = self.token();
        if token.is_empty() {
            return Err(self.failed("No statistics API token configured", false, ""));
        }
        let url = self.url();
        let p = urlsplit(&url);
        if p.scheme != "https" || p.hostname.is_empty() || p.userinfo || p.fragment {
            return Err(self.failed(
                "Statistics server must use HTTPS without embedded credentials",
                false,
                &token,
            ));
        }
        let (payload, rows) = match pending_envelope_rows(&self.inner.store, 50) {
            Ok(x) => x,
            Err(e) => return Err(self.failed(&e.message, true, &token)),
        };
        if rows.is_empty() {
            self.set_status("IDLE");
            return Ok(0);
        }
        if rows.iter().any(|r| !(r.seconds > 0.0 && r.seconds <= 5.5)) {
            return Err(self.failed(
                "Legacy interval exceeds server maximum; preserved locally",
                false,
                &token,
            ));
        }
        let limit = self.clock() + 300.0;
        if rows
            .iter()
            .any(|r| !(r.start_utc >= 1577836800.0 && r.start_utc <= limit))
        {
            return Err(self.failed(
                "Invalid listening timestamp in outbox; preserved locally",
                false,
                &token,
            ));
        }
        let ids: HashSet<&str> = rows.iter().map(|r| r.event_id.as_str()).collect();
        self.set_status("SYNCING");
        let response = match (self.inner.transport)(&url, &token, &payload) {
            Ok(v) => v,
            Err(TransportError::Http(code)) => {
                let retry = matches!(code, 429 | 500 | 502 | 503 | 504);
                return Err(self.failed(&http_message(code), retry, &token));
            }
            Err(TransportError::Network(_)) => {
                return Err(self.failed("Network unavailable or timeout", true, &token));
            }
            Err(TransportError::Invalid(m)) => return Err(self.failed(&m, true, &token)),
        };
        let valid = response.get("ok") == Some(&Value::Bool(true))
            && response
                .get("format_version")
                .and_then(|v| v.as_f64())
                .map(|v| v == 1.0)
                .unwrap_or(false);
        if !response.is_object() || !valid {
            return Err(self.failed(
                "Server response was not a valid acknowledgment",
                true,
                &token,
            ));
        }
        let ack: Option<Vec<String>> = response
            .get("acknowledged_event_ids")
            .and_then(|v| v.as_array())
            .and_then(|a| {
                a.iter()
                    .map(|x| x.as_str().filter(|s| ids.contains(s)).map(String::from))
                    .collect()
            });
        let ack = match ack {
            Some(a) if a.iter().collect::<HashSet<_>>().len() == a.len() => a,
            _ => {
                return Err(self.failed(
                    "Server acknowledgment contains invalid event IDs",
                    true,
                    &token,
                ))
            }
        };
        if ack.is_empty() {
            return Err(self.failed("Server acknowledged no events", true, &token));
        }
        if let Err(e) = self.inner.store.acknowledge(&ack) {
            return Err(self.failed(&e.message, true, &token));
        }
        let now = self.clock();
        let _ = self
            .inner
            .store
            .set_meta("last_success", &time::py_float(now));
        let mut s = self.st();
        s.last_success = now;
        s.failures = 0;
        s.next_retry = 0.0;
        s.last_error.clear();
        s.status = "IDLE".into();
        Ok(ack.len())
    }

    /// Run `once()` on a background thread. Automatic runs respect the retry
    /// back-off; `manual` ignores it. Returns false when disabled, backing
    /// off or already running. `callback` runs when the attempt ends.
    pub fn run_async(&self, manual: bool, callback: Option<Callback>) -> bool {
        if !self.enabled() {
            self.set_status("DISABLED");
            return false;
        }
        if !manual && self.clock() < self.next_retry() {
            return false;
        }
        if self
            .inner
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("nplay-statistics-sync".into())
            .spawn(move || {
                let _ = me.once();
                me.inner.busy.store(false, Ordering::SeqCst);
                if let Some(cb) = callback {
                    cb();
                }
            });
        match spawned {
            Ok(h) => {
                if let Ok(mut t) = self.inner.thread.lock() {
                    *t = Some(h);
                }
                true
            }
            Err(_) => {
                self.inner.busy.store(false, Ordering::SeqCst);
                false
            }
        }
    }

    /// Wait for a running `run_async` attempt (tests, shutdown).
    pub fn join(&self) {
        let h = self.inner.thread.lock().ok().and_then(|mut t| t.take());
        if let Some(h) = h {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::urlsplit;

    #[test]
    fn url_checks() {
        let p = urlsplit("https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3");
        assert_eq!(
            (p.scheme.as_str(), p.hostname.as_str()),
            ("https", "n.rudolfsson.net")
        );
        assert!(!p.userinfo && !p.fragment);
        assert!(urlsplit("https://u:p@h/").userinfo);
        assert!(urlsplit("https://h/#frag").fragment);
        assert!(!urlsplit("https://h/#").fragment);
        assert_eq!(urlsplit("HTTP://insecure.test/").scheme, "http");
        assert_eq!(urlsplit("https:///x").hostname, "");
        assert_eq!(urlsplit("https://[::1]:8443/").hostname, "::1");
    }
}
