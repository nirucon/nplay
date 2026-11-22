//! The provider-neutral `Track` (also used for container rows such as
//! albums, artists, playlists, programs, folders and devices).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Meta = Map<String, Value>;

/// Field order matches Python's dataclass, so `session.json` written by
/// either version reads in both.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Track {
    pub source: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub url: String,
    pub cover: String,
    pub path: String,
    pub kind: String,
    pub seekable: bool,
    pub meta: Meta,
}

impl Default for Track {
    fn default() -> Self {
        Track {
            source: "local".into(),
            id: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            duration: 0.0,
            url: String::new(),
            cover: String::new(),
            path: String::new(),
            kind: "track".into(),
            seekable: true,
            meta: Meta::new(),
        }
    }
}

impl Track {
    pub fn new(source: &str, id: &str, title: &str) -> Track {
        Track {
            source: source.into(),
            id: id.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    /// `source:id`, the stable identity used by history, ratings and resume.
    pub fn key(&self) -> String {
        format!("{}:{}", self.source, self.id)
    }

    pub fn is_track(&self) -> bool {
        self.kind == "track"
    }

    /// Python `Track.dict()`.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Python `Track.from_dict()`: unknown keys are ignored, missing ones
    /// default; tolerant of ints where floats are expected and vice versa.
    pub fn from_value(v: &Value) -> Option<Track> {
        let o = v.as_object()?;
        let mut t = Track::default();
        let s = |k: &str| o.get(k).map(value_str);
        if let Some(x) = s("source") {
            t.source = x;
        }
        if let Some(x) = s("id") {
            t.id = x;
        }
        if let Some(x) = s("title") {
            t.title = x;
        }
        if let Some(x) = s("artist") {
            t.artist = x;
        }
        if let Some(x) = s("album") {
            t.album = x;
        }
        if let Some(x) = s("url") {
            t.url = x;
        }
        if let Some(x) = s("cover") {
            t.cover = x;
        }
        if let Some(x) = s("path") {
            t.path = x;
        }
        if let Some(x) = s("kind") {
            t.kind = x;
        }
        if let Some(d) = o.get("duration") {
            t.duration = value_f64(d);
        }
        if let Some(b) = o.get("seekable") {
            t.seekable = value_bool(b);
        }
        if let Some(Value::Object(m)) = o.get("meta") {
            t.meta = m.clone();
        }
        Some(t)
    }

    pub fn meta_str(&self, k: &str) -> String {
        self.meta.get(k).map(value_str).unwrap_or_default()
    }

    pub fn meta_f64(&self, k: &str) -> f64 {
        self.meta.get(k).map(value_f64).unwrap_or(0.0)
    }

    pub fn meta_bool(&self, k: &str) -> bool {
        self.meta.get(k).map(value_bool).unwrap_or(false)
    }

    pub fn set_meta(&mut self, k: &str, v: impl Into<Value>) {
        self.meta.insert(k.to_string(), v.into());
    }
}

/// Python `str(x)` for JSON scalars (None -> "").
pub fn value_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

pub fn value_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        Value::Bool(b) => *b as i32 as f64,
        _ => 0.0,
    }
}

pub fn value_i64(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().map(|f| f as i64))
            .unwrap_or(0),
        Value::Bool(b) => *b as i64,
        _ => 0,
    }
}

/// Python truthiness.
pub fn value_bool(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_order_and_roundtrip() {
        let t = Track::new("local", "a", "Song");
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.starts_with("{\"source\":\"local\",\"id\":\"a\",\"title\":\"Song\""));
        let back = Track::from_value(&serde_json::from_str(&s).unwrap()).unwrap();
        assert_eq!(back, t);
        let partial: Value = serde_json::json!({"source":"sr","id":"1","duration":3,"x":1});
        let p = Track::from_value(&partial).unwrap();
        assert_eq!(p.duration, 3.0);
        assert_eq!(p.kind, "track");
    }
}
