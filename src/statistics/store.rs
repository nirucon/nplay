//! Listening statistics and the durable, backwards-compatible sync outbox
//! (`<data>/statistics.sqlite3`, mode 0600). Port of 1.x `statistics/store.py`.

use super::time;
use crate::core::{Error, Result};
use crate::model::Track;
use crate::paths;
use rusqlite::{params, Connection, OptionalExtension};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
   CREATE TABLE IF NOT EXISTS identity(key TEXT PRIMARY KEY,value TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS listening(
    event_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, start_utc REAL NOT NULL,
    seconds REAL NOT NULL CHECK(seconds>0 AND seconds<=10),
    source TEXT NOT NULL, track_id TEXT NOT NULL, title TEXT NOT NULL,
    artist TEXT NOT NULL, album TEXT NOT NULL);
   CREATE INDEX IF NOT EXISTS listening_period ON listening(start_utc);
   CREATE INDEX IF NOT EXISTS listening_artist ON listening(artist);
   CREATE TABLE IF NOT EXISTS sync_outbox(
    event_id TEXT PRIMARY KEY REFERENCES listening(event_id),
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','acked')),
    created_utc REAL NOT NULL);
   ";

/// One row of `listening` waiting in the outbox.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingEvent {
    pub event_id: String,
    pub session_id: String,
    pub start_utc: f64,
    pub seconds: f64,
    pub source: String,
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// `StatisticsStore::summary` result. Ranking rows are
/// `(name, seconds, distinct sessions)`; albums and tracks are
/// `(artist, album|title, seconds, sessions)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub seconds: f64,
    pub sessions: i64,
    pub days: i64,
    pub artists: Vec<(String, f64, i64)>,
    pub albums: Vec<(String, String, f64, i64)>,
    pub tracks: Vec<(String, String, f64, i64)>,
    pub sources: Vec<(String, f64, i64)>,
    pub pending: i64,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[derive(Clone)]
pub struct StatisticsStore {
    pub path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl StatisticsStore {
    /// Open `<data>/statistics.sqlite3`.
    pub fn new() -> Result<StatisticsStore> {
        StatisticsStore::open(&paths::data().join("statistics.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<StatisticsStore> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let c = Connection::open(path)?;
        // Never block playback on a busy statistics database.
        c.busy_timeout(std::time::Duration::from_millis(50))?;
        c.execute_batch("PRAGMA foreign_keys=ON")?;
        let tx = c.unchecked_transaction()?;
        tx.execute_batch(SCHEMA)?;
        tx.execute(
            "INSERT OR IGNORE INTO identity(key,value) VALUES('installation_id',?)",
            [time::uuid4()],
        )?;
        tx.commit()?;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        Ok(StatisticsStore {
            path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(c)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn installation_id(&self) -> Result<String> {
        Ok(self.lock().query_row(
            "SELECT value FROM identity WHERE key='installation_id'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn get_meta(&self, key: &str, default: &str) -> Result<String> {
        Ok(self
            .lock()
            .query_row("SELECT value FROM identity WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_else(|| default.to_string()))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO identity(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Record `seconds` (0 < s <= 10) of listening that started at
    /// `start_utc`. Idempotent per `event_id`; returns the event id.
    pub fn add(
        &self,
        session_id: &str,
        start_utc: f64,
        seconds: f64,
        track: &Track,
        event_id: Option<&str>,
    ) -> Result<String> {
        if !seconds.is_finite() || !(seconds > 0.0 && seconds <= 10.0) {
            return Err(Error::msg("Invalid listening interval"));
        }
        // Playback may briefly report a zero/uninitialized start timestamp.
        // Use the actual wall clock rather than persisting an unsyncable 1970 event.
        let now = paths::now();
        let start = if !start_utc.is_finite() || start_utc <= 0.0 {
            now
        } else if start_utc > now + 300.0 {
            return Err(Error::msg("Listening timestamp is in the future"));
        } else {
            start_utc
        };
        let eid = event_id
            .filter(|e| !e.is_empty())
            .map(|e| e.to_string())
            .unwrap_or_else(time::uuid4);
        let source = if track.source.is_empty() {
            "unknown"
        } else {
            &track.source
        };
        let c = self.lock();
        let tx = c.unchecked_transaction()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO listening VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                eid,
                session_id,
                start,
                seconds,
                cut(source, 40),
                cut(&track.id, 512),
                cut(&track.title, 512),
                cut(&track.artist, 512),
                cut(&track.album, 512)
            ],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT OR IGNORE INTO sync_outbox(event_id,created_utc) VALUES(?,?)",
                params![eid, paths::now()],
            )?;
        }
        tx.commit()?;
        Ok(eid)
    }

    /// Totals and top-10 rankings for `day|week|month|year|all`.
    pub fn summary(&self, period: &str, now: Option<f64>) -> Result<Summary> {
        let now = now.unwrap_or_else(paths::now);
        let start = time::period_start(period, now).ok_or_else(|| Error::msg("Unknown period"))?;
        let cutoff = start.unwrap_or(0.0);
        let c = self.lock();
        let (seconds, sessions, days) = c.query_row(
            "SELECT COALESCE(SUM(seconds),0),COUNT(DISTINCT session_id),COUNT(DISTINCT date(start_utc,'unixepoch','localtime')) FROM listening WHERE start_utc>=?",
            [cutoff],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )?;
        let top = |field: &str| -> rusqlite::Result<Vec<(String, f64, i64)>> {
            let mut st = c.prepare(&format!("SELECT {field},SUM(seconds) AS secs,COUNT(DISTINCT session_id) AS plays FROM listening WHERE start_utc>=? AND {field} != '' GROUP BY {field} ORDER BY secs DESC,{field} COLLATE NOCASE LIMIT 10"))?;
            let v = st
                .query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect();
            v
        };
        let pairs = |sql: &str| -> rusqlite::Result<Vec<(String, String, f64, i64)>> {
            let mut st = c.prepare(sql)?;
            let v = st
                .query_map([cutoff], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect();
            v
        };
        let artists = top("artist")?;
        let albums = pairs("SELECT artist,album,SUM(seconds),COUNT(DISTINCT session_id) FROM listening WHERE start_utc>=? AND album != '' GROUP BY artist,album ORDER BY SUM(seconds) DESC LIMIT 10")?;
        let tracks = pairs("SELECT artist,title,SUM(seconds),COUNT(DISTINCT session_id) FROM listening WHERE start_utc>=? AND title != '' GROUP BY artist,title ORDER BY SUM(seconds) DESC LIMIT 10")?;
        let sources = top("source")?;
        let pending = c.query_row(
            "SELECT COUNT(*) FROM sync_outbox WHERE status='pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(Summary {
            seconds,
            sessions,
            days,
            artists,
            albums,
            tracks,
            sources,
            pending,
        })
    }

    /// Read-only export of pending events (oldest first, 1..=500 rows).
    pub fn pending_events(&self, limit: i64) -> Result<Vec<PendingEvent>> {
        let c = self.lock();
        let mut st = c.prepare(
            "SELECT l.* FROM listening l JOIN sync_outbox q USING(event_id)
    WHERE q.status='pending' ORDER BY l.start_utc,l.event_id LIMIT ?",
        )?;
        let v = st
            .query_map([limit.clamp(1, 500)], |r| {
                Ok(PendingEvent {
                    event_id: r.get("event_id")?,
                    session_id: r.get("session_id")?,
                    start_utc: r.get("start_utc")?,
                    seconds: r.get("seconds")?,
                    source: r.get("source")?,
                    track_id: r.get("track_id")?,
                    title: r.get("title")?,
                    artist: r.get("artist")?,
                    album: r.get("album")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    /// Only call after a verified server-side idempotent acknowledgment.
    pub fn acknowledge(&self, ids: &[String]) -> Result<()> {
        let c = self.lock();
        let tx = c.unchecked_transaction()?;
        {
            let mut st = tx.prepare("UPDATE sync_outbox SET status='acked' WHERE event_id=?")?;
            for id in ids {
                st.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
