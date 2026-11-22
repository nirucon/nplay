//! Small shared runtime pieces (port of 1.x `nplay/core`): error taxonomy,
//! synchronous event bus, canonical playback snapshot, provider registry and
//! the atomic `session.json` store.

use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared error type. `kind` mirrors the 1.x taxonomy.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorKind {
    Generic,
    Provider,
    Playback,
    Authentication,
    Network,
    MediaUnavailable,
    Configuration,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
        }
    }
    pub fn msg(message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Generic, message)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::msg(e.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::msg(e.to_string())
    }
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Error::msg(e)
    }
}

impl From<&str> for Error {
    fn from(e: &str) -> Self {
        Error::msg(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Handler receives the event name and a JSON payload.
pub type Handler = Arc<dyn Fn(&str, &Map<String, Value>) + Send + Sync>;

/// Small synchronous event bus. `"*"` subscribes to every event. Handler
/// panics are contained.
#[derive(Clone, Default)]
pub struct EventBus {
    handlers: Arc<Mutex<Vec<(String, u64, Handler)>>>,
    next: Arc<Mutex<u64>>,
}

impl EventBus {
    pub fn new() -> EventBus {
        EventBus::default()
    }

    /// Returns a subscription id for `unsubscribe`.
    pub fn subscribe(&self, event: &str, h: Handler) -> u64 {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        self.handlers
            .lock()
            .unwrap()
            .push((event.to_string(), *n, h));
        *n
    }

    pub fn unsubscribe(&self, id: u64) {
        self.handlers.lock().unwrap().retain(|(_, i, _)| *i != id);
    }

    pub fn emit(&self, event: &str, payload: Map<String, Value>) {
        let hs: Vec<Handler> = {
            let g = self.handlers.lock().unwrap();
            g.iter()
                .filter(|(e, _, _)| e == event)
                .chain(g.iter().filter(|(e, _, _)| e == "*"))
                .map(|(_, _, h)| h.clone())
                .collect()
        };
        for h in hs {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(event, &payload)));
        }
    }
}

/// Canonical, serialisable playback snapshot exposed to integrations.
#[derive(Clone, Debug, Default)]
pub struct PlaybackState {
    pub status: String,
    pub transport: String,
    pub source: String,
    pub track_id: String,
    pub generation: u64,
    pub position: f64,
    pub duration: f64,
    pub seekable: bool,
    pub kind: String,
    pub metadata: Map<String, Value>,
}

impl PlaybackState {
    pub fn new() -> PlaybackState {
        PlaybackState {
            status: "idle".into(),
            ..Default::default()
        }
    }

    pub fn update_media(
        &mut self,
        track: &crate::model::Track,
        transport: &str,
        generation: u64,
        status: &str,
    ) {
        self.status = status.into();
        self.transport = transport.into();
        self.source = track.source.clone();
        self.track_id = track.id.clone();
        self.generation = generation;
        self.duration = track.duration;
        self.seekable = track.seekable;
        self.kind = track.kind.clone();
    }

    pub fn clear(&mut self) {
        *self = PlaybackState::new();
    }
}

#[derive(Clone, Debug)]
pub struct SourceSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub capabilities: &'static [&'static str],
}

/// Provider capability registry (order matters for the wire `sources` list).
pub const SOURCES: &[SourceSpec] = &[
    SourceSpec {
        id: "local",
        label: "Local Music",
        capabilities: &[
            "browse",
            "search",
            "random",
            "radio-seed",
            "seek",
            "artwork",
            "playlists",
            "ratings",
        ],
    },
    SourceSpec {
        id: "navidrome",
        label: "Navidrome",
        capabilities: &[
            "browse",
            "search",
            "random",
            "radio-seed",
            "seek",
            "artwork",
            "playlists",
        ],
    },
    SourceSpec {
        id: "spotify",
        label: "Spotify",
        capabilities: &[
            "browse",
            "search",
            "radio-seed",
            "seek",
            "artwork",
            "playlists",
            "remote-transport",
        ],
    },
    SourceSpec {
        id: "sr",
        label: "Sveriges Radio",
        capabilities: &["browse", "search", "live", "podcast", "artwork"],
    },
    SourceSpec {
        id: "youtube",
        label: "YouTube",
        capabilities: &["search", "seek", "artwork"],
    },
    SourceSpec {
        id: "radio",
        label: "Custom Radio",
        capabilities: &["live"],
    },
];

pub fn source(id: &str) -> Option<&'static SourceSpec> {
    SOURCES.iter().find(|s| s.id == id)
}

pub fn supports(id: &str, capability: &str) -> bool {
    source(id).is_some_and(|s| s.capabilities.contains(&capability))
}

/// Atomic `session.json` persistence (compact JSON, temp file + rename).
#[derive(Clone)]
pub struct SessionStore {
    pub path: PathBuf,
}

impl SessionStore {
    pub fn new(path: PathBuf) -> SessionStore {
        SessionStore { path }
    }

    pub fn load(&self) -> Map<String, Value> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub fn save(&self, data: &Value) -> Result<()> {
        let s = serde_json::to_vec(data).map_err(|e| Error::msg(e.to_string()))?;
        // Python's Path.write_text keeps the umask mode (0644 typically).
        crate::paths::write_atomic(&self.path, &s, 0o644)?;
        Ok(())
    }
}

/// Python-style JSON float: serde_json prints integral f64 as `1.0`, like
/// Python; this helper exists so callers can be explicit.
pub fn jf(v: f64) -> Value {
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}
