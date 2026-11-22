//! SQLite library (`library.db` in `paths::data()`), scanner, playlists,
//! favorites, ratings, history, discoveries, bookmarks, play events and custom
//! radio stations. Port of 1.x `nplay/db.py`; the file format is identical, so
//! a 1.x library opens unchanged (and 1.x can still open a 2.x library).
//!
//! # Public API
//! `DB` is a cheap `Clone + Send + Sync` handle (one WAL connection behind a
//! mutex, busy timeout 10 s, bounded SQLITE_BUSY retry for writes). Every
//! method takes `&self`; methods that 1.x let raise return `Result`.
//!
//! - `DB::new()` opens `<data>/library.db`; `DB::open(path)` any path. Both run
//!   the additive migrations (`init`): WAL + synchronous=NORMAL, CREATE TABLE IF
//!   NOT EXISTS, `tracks.added` (backup `library.pre-0.4.3.db`), the 1.1 columns
//!   (backup `library.pre-1.1.0.db` before the first schema-4 open), the
//!   `play_events` table, backup `library.pre-1.2.0.db` before the first
//!   schema-5 open, and `PRAGMA user_version=5`.
//! - Scanning: `scan(roots, progress, excludes, batch_size, force) -> ScanReport`
//!   (incremental mtime compare, `force` = full rescan, offline roots are kept
//!   and never cause deletion, nested symlinked dirs are not followed, batches
//!   of `max(25, batch_size)` rows). `scan_config(&Config, progress, force)` is
//!   1.x `App.scan` (`music_dirs`, `music_excludes`, `library_scan_batch`);
//!   `roots(&Config)` is `App.roots`. `last_scan()` returns the last report as
//!   a JSON map. `ScanReport::to_map()` has the keys `checked new updated
//!   removed total offline roots` (+ `done` in the final progress call).
//!   `ProgressFn = Arc<dyn Fn(&Map<String, Value>) + Send + Sync>` gets
//!   `{checked,new,updated,offline}` every 500 files and the final report with
//!   `done: true`.
//! - Library: `count`, `schema_version`, `search(q, limit)`, `random_tracks`,
//!   `find_artists(q, limit) -> Vec<(artist, n)>`, `random_artist_tracks ->
//!   (artist, tracks)`, `shuffled_artist_tracks`, `recently_added`,
//!   `random_album_tracks`, `local_artists`, `local_albums`, `local_folders`
//!   (menu rows `(label, id, subtitle)`), `tracks_by_artist`, `tracks_by_album`,
//!   `tracks_by_folder`, `never_played`, `unplayed_albums`, `rated`.
//!   Local tracks map like 1.x `_track` (folder cover wins, else extracted
//!   embedded cover; meta keys suffix album_artist track_no disc_no year genre
//!   codec sample_rate bit_depth channels bitrate).
//! - Resume: `resume_set(item_id, pos)`, `resume_get(item_id, legacy_id)`.
//! - Favorites/history: `favorite(&Track) -> bool` (toggle, true = added),
//!   `favorites`, `history_add`, `history(limit)`, `most_played(limit)`.
//! - Playlists: `playlists`, `playlist_create(name) -> Option<id>`,
//!   `playlist_add`, `playlist_tracks`, `playlist_name`, `playlist_delete`,
//!   `playlist_rename`, `playlist_remove(pid, index)`,
//!   `playlist_move(pid, index, delta) -> new index`.
//! - Ratings: `rating_get(item_id, source)`, `rating_set(item_id, rating, source)`.
//! - Discoveries: `discovery_add(title, artist, source, data) -> bool`, `discoveries`.
//! - Bookmarks: `bookmark_add(&Track, pos) -> id`, `bookmarks(limit)`, `bookmark_delete`.
//! - Play events: `play_started(&Track) -> Option<id>`, `play_finished(id,
//!   seconds, completed)`, `listening_stats(days) -> ListeningStats`.
//! - Radio: `radios`, `radio_add(name, url, homepage, artwork)`, `radio_delete`.

mod fnmatch;
mod sha1;
pub mod tags;

use crate::config::Config;
use crate::core::{Error, Result};
use crate::model::Track;
use crate::paths;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const AUDIO: &[&str] = &[
    "mp3", "flac", "opus", "ogg", "oga", "m4a", "aac", "wav", "webm", "mp4", "wma", "ape", "aiff",
    "aif",
];
pub const COVERS: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.png",
];
const BUILTIN_EXCLUDES: &[&str] = &[
    ".git",
    ".svn",
    "node_modules",
    "__pycache__",
    ".Trash-1000",
    ".Trash",
];
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tracks(id TEXT PRIMARY KEY,path TEXT UNIQUE,title TEXT,artist TEXT,album TEXT,mtime REAL,added REAL); CREATE TABLE IF NOT EXISTS favorites(id TEXT PRIMARY KEY,data TEXT,added REAL); CREATE TABLE IF NOT EXISTS history(id INTEGER PRIMARY KEY AUTOINCREMENT,item_id TEXT,data TEXT,played REAL); CREATE TABLE IF NOT EXISTS resume(item_id TEXT PRIMARY KEY,pos REAL,updated REAL); CREATE TABLE IF NOT EXISTS playlists(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE COLLATE NOCASE,created REAL); CREATE TABLE IF NOT EXISTS playlist_items(id INTEGER PRIMARY KEY AUTOINCREMENT,playlist_id INTEGER NOT NULL,data TEXT NOT NULL,position INTEGER NOT NULL,added REAL,FOREIGN KEY(playlist_id) REFERENCES playlists(id) ON DELETE CASCADE); CREATE TABLE IF NOT EXISTS radio_stations(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE COLLATE NOCASE,url TEXT NOT NULL,homepage TEXT,artwork TEXT,added REAL); CREATE TABLE IF NOT EXISTS ratings(item_id TEXT PRIMARY KEY,rating INTEGER NOT NULL,updated REAL); CREATE TABLE IF NOT EXISTS discoveries(id INTEGER PRIMARY KEY AUTOINCREMENT,title TEXT,artist TEXT,source TEXT,data TEXT,added REAL); CREATE TABLE IF NOT EXISTS bookmarks(id INTEGER PRIMARY KEY AUTOINCREMENT,item_key TEXT,data TEXT,pos REAL,added REAL);";
const PLAY_EVENTS: &str = "CREATE TABLE IF NOT EXISTS play_events(id INTEGER PRIMARY KEY AUTOINCREMENT,item_key TEXT,data TEXT,started REAL,ended REAL,seconds REAL DEFAULT 0,completed INTEGER DEFAULT 0)";
const NEW_COLUMNS: &[(&str, &str)] = &[
    ("album_artist", "TEXT"),
    ("track_no", "INTEGER"),
    ("disc_no", "INTEGER"),
    ("year", "INTEGER"),
    ("genre", "TEXT"),
    ("duration", "REAL"),
    ("codec", "TEXT"),
    ("sample_rate", "INTEGER"),
    ("bit_depth", "INTEGER"),
    ("channels", "INTEGER"),
    ("bitrate", "INTEGER"),
];
const ORDER_LIBRARY: &str = "ORDER BY coalesce(nullif(album_artist,''),artist) COLLATE NOCASE,year,album COLLATE NOCASE,disc_no,track_no,title COLLATE NOCASE";
const ARTIST: &str = "coalesce(nullif(album_artist,''),artist)";

pub type ProgressFn = Arc<dyn Fn(&Map<String, Value>) + Send + Sync>;
/// Menu row `(label, id, subtitle)` as 1.x returned tuples.
pub type Row3 = (String, String, String);

/// Result of `DB::scan` (1.x `last_scan` dict).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanReport {
    pub checked: u64,
    pub new: u64,
    pub updated: u64,
    pub removed: u64,
    pub total: i64,
    pub offline: Vec<String>,
    pub roots: Vec<String>,
}

impl ScanReport {
    pub fn to_map(&self) -> Map<String, Value> {
        let v = json!({
            "checked": self.checked, "new": self.new, "updated": self.updated,
            "removed": self.removed, "total": self.total, "offline": self.offline,
            "roots": self.roots,
        });
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    }
}

/// Result of `DB::listening_stats`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListeningStats {
    pub plays: i64,
    pub substantial: i64,
    pub completed: i64,
    pub minutes: i64,
    /// Top 20 `(artist, substantial plays)`.
    pub artists: Vec<(String, i64)>,
    pub sources: Vec<(String, i64)>,
}

impl ListeningStats {
    pub fn to_value(&self) -> Value {
        json!({
            "plays": self.plays, "substantial": self.substantial, "completed": self.completed,
            "minutes": self.minutes,
            "artists": self.artists.iter().map(|(a, n)| json!([a, n])).collect::<Vec<_>>(),
            "sources": self.sources.iter().map(|(a, n)| json!([a, n])).collect::<Vec<_>>(),
        })
    }
}

/// A `tracks` row as stored.
#[derive(Clone, Debug, Default)]
struct TrackRow {
    id: String,
    path: String,
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    track_no: i64,
    disc_no: i64,
    year: i64,
    genre: String,
    duration: f64,
    codec: String,
    sample_rate: i64,
    bit_depth: i64,
    channels: i64,
    bitrate: i64,
}

fn v_str(r: &Row, col: &str) -> String {
    match r.get_ref(col) {
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).to_string(),
        Ok(ValueRef::Integer(i)) => i.to_string(),
        Ok(ValueRef::Real(f)) => f.to_string(),
        _ => String::new(),
    }
}

fn v_i64(r: &Row, col: &str) -> i64 {
    match r.get_ref(col) {
        Ok(ValueRef::Integer(i)) => i,
        Ok(ValueRef::Real(f)) => f as i64,
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn v_f64(r: &Row, col: &str) -> f64 {
    match r.get_ref(col) {
        Ok(ValueRef::Integer(i)) => i as f64,
        Ok(ValueRef::Real(f)) => f,
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn v_json(r: &Row, col: &str) -> Value {
    match r.get_ref(col) {
        Ok(ValueRef::Integer(i)) => json!(i),
        Ok(ValueRef::Real(f)) => json!(f),
        Ok(ValueRef::Text(t)) => json!(String::from_utf8_lossy(t)),
        _ => Value::Null,
    }
}

fn track_row(r: &Row) -> rusqlite::Result<TrackRow> {
    Ok(TrackRow {
        id: v_str(r, "id"),
        path: v_str(r, "path"),
        title: v_str(r, "title"),
        artist: v_str(r, "artist"),
        album: v_str(r, "album"),
        album_artist: v_str(r, "album_artist"),
        track_no: v_i64(r, "track_no"),
        disc_no: v_i64(r, "disc_no"),
        year: v_i64(r, "year"),
        genre: v_str(r, "genre"),
        duration: v_f64(r, "duration"),
        codec: v_str(r, "codec"),
        sample_rate: v_i64(r, "sample_rate"),
        bit_depth: v_i64(r, "bit_depth"),
        channels: v_i64(r, "channels"),
        bitrate: v_i64(r, "bitrate"),
    })
}

fn track_json(t: &Track) -> String {
    serde_json::to_string(&t.to_value()).unwrap_or_else(|_| "{}".into())
}

fn parse_track(data: &str) -> Option<Track> {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| Track::from_value(&v))
}

fn is_busy(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(f, msg) => {
            matches!(f.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                || msg
                    .as_deref()
                    .map(|m| {
                        let m = m.to_lowercase();
                        m.contains("locked") || m.contains("busy")
                    })
                    .unwrap_or(false)
        }
        _ => false,
    }
}

/// `~` / `~/x` expansion (Python `Path.expanduser`).
pub fn expanduser(p: &str) -> PathBuf {
    if p == "~" {
        return paths::home_dir();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return paths::home_dir().join(rest);
    }
    PathBuf::from(p)
}

/// Python `str(Path(p))`: collapse duplicate and trailing slashes and `.`.
fn norm_path(p: &str) -> String {
    let pb: PathBuf = Path::new(p).components().collect();
    let s = pb.to_string_lossy().to_string();
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

fn casefold(s: &str) -> String {
    s.to_lowercase()
}

/// Copy `src` to `dst` keeping the modification time (shutil.copy2-like).
fn copy2(src: &Path, dst: &Path) {
    if std::fs::copy(src, dst).is_ok() {
        if let Ok(m) = std::fs::metadata(src).and_then(|m| m.modified()) {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(dst) {
                let _ = f.set_modified(m);
            }
        }
    }
}

#[derive(Clone)]
pub struct DB {
    pub path: PathBuf,
    data_dir: PathBuf,
    conn: Arc<Mutex<Connection>>,
    last_scan: Arc<Mutex<Map<String, Value>>>,
}

impl DB {
    /// Open `<data>/library.db` (created and migrated as needed).
    pub fn new() -> Result<DB> {
        let data = paths::data();
        DB::open_in(&data.join("library.db"), &data)
    }

    /// Open a library at `path`; embedded covers go below `paths::data()`.
    pub fn open(path: &Path) -> Result<DB> {
        DB::open_in(path, &paths::data())
    }

    /// Open a library at `path` with embedded covers cached in
    /// `<data_dir>/artwork/embedded`.
    pub fn open_in(path: &Path, data_dir: &Path) -> Result<DB> {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let existed = path.exists();
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(10))?;
        c.execute_batch("PRAGMA foreign_keys=ON")?;
        let db = DB {
            path: path.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            conn: Arc::new(Mutex::new(c)),
            last_scan: Arc::new(Mutex::new(Map::new())),
        };
        db.init(existed)?;
        Ok(db)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn backup(&self, name: &str) {
        let b = self.path.with_file_name(name);
        if !b.exists() {
            copy2(&self.path, &b);
        }
    }

    /// Additive schema migrations. User libraries/playlists are never rebuilt.
    /// Backups are only made of a library that existed before this open.
    fn init(&self, existed: bool) -> Result<()> {
        let c = self.lock();
        c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        c.execute_batch("PRAGMA synchronous=NORMAL")?;
        let version = |c: &Connection| -> Result<i64> {
            Ok(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?)
        };
        let pre = version(&c)?;
        if pre < 5 && existed {
            // Make the copy self-contained (no committed rows left in -wal).
            let _ = c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            self.backup("library.pre-1.2.0.db");
        }
        c.execute_batch(SCHEMA)?;
        let cols = |c: &Connection| -> Result<HashSet<String>> {
            let mut st = c.prepare("PRAGMA table_info(tracks)")?;
            let v = st
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<HashSet<String>>>()?;
            Ok(v)
        };
        let tx = c.unchecked_transaction()?;
        if !cols(&tx)?.contains("added") {
            if existed {
                self.backup("library.pre-0.4.3.db");
            }
            tx.execute_batch("ALTER TABLE tracks ADD COLUMN added REAL")?;
            tx.execute(
                "UPDATE tracks SET added=coalesce(mtime,?) WHERE added IS NULL",
                params![paths::now()],
            )?;
        }
        if version(&tx)? < 4 && existed {
            self.backup("library.pre-1.1.0.db");
        }
        for (col, typ) in NEW_COLUMNS {
            if !cols(&tx)?.contains(*col) {
                tx.execute_batch(&format!("ALTER TABLE tracks ADD COLUMN {col} {typ}"))?;
            }
        }
        tx.execute_batch(PLAY_EVENTS)?;
        tx.execute_batch("PRAGMA user_version=5")?;
        tx.commit()?;
        Ok(())
    }

    /// Run a short write transaction with bounded SQLITE_BUSY retry.
    fn write<T>(&self, f: impl Fn(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut delay = 0.04f64;
        let retries = 6;
        for attempt in 0..retries {
            let res = {
                let c = self.lock();
                (|| {
                    let tx = c.unchecked_transaction()?;
                    let v = f(&tx)?;
                    tx.commit()?;
                    Ok(v)
                })()
            };
            match res {
                Ok(v) => return Ok(v),
                Err(e) if is_busy(&e) && attempt + 1 < retries => {
                    std::thread::sleep(std::time::Duration::from_secs_f64(delay));
                    delay = (delay * 2.0).min(0.8);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::msg("database is locked"))
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let c = self.lock();
        Ok(f(&c)?)
    }

    fn rows(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<TrackRow>> {
        self.read(|c| {
            let mut st = c.prepare(sql)?;
            let v = st.query_map(p, track_row)?.collect();
            v
        })
    }

    fn tracks(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Track>> {
        Ok(self.rows(sql, p)?.iter().map(|r| self.track(r)).collect())
    }

    fn data_tracks(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Track>> {
        let data: Vec<String> = self.read(|c| {
            let mut st = c.prepare(sql)?;
            let v = st.query_map(p, |r| Ok(v_str(r, "data")))?.collect();
            v
        })?;
        Ok(data.iter().filter_map(|d| parse_track(d)).collect())
    }

    /// 1.x `_track`: map a library row to a local `Track`.
    fn track(&self, r: &TrackRow) -> Track {
        let f = Path::new(&r.path);
        let mut cover = String::new();
        // An explicit folder cover is intentional and therefore wins over embedded art.
        if let Some(parent) = f.parent() {
            for n in COVERS {
                let q = parent.join(n);
                if q.is_file() {
                    cover = q.to_string_lossy().to_string();
                    break;
                }
            }
        }
        if cover.is_empty() {
            cover = tags::embedded_cover(f, &self.data_dir);
        }
        let mut meta = Map::new();
        meta.insert("suffix".into(), json!(tags::suffix(f)));
        meta.insert("album_artist".into(), json!(r.album_artist));
        meta.insert("track_no".into(), json!(r.track_no));
        meta.insert("disc_no".into(), json!(r.disc_no));
        meta.insert("year".into(), json!(r.year));
        meta.insert("genre".into(), json!(r.genre));
        meta.insert("codec".into(), json!(r.codec));
        meta.insert("sample_rate".into(), json!(r.sample_rate));
        meta.insert("bit_depth".into(), json!(r.bit_depth));
        meta.insert("channels".into(), json!(r.channels));
        meta.insert("bitrate".into(), json!(r.bitrate));
        Track {
            source: "local".into(),
            id: r.id.clone(),
            title: r.title.clone(),
            artist: r.artist.clone(),
            album: r.album.clone(),
            duration: r.duration,
            path: r.path.clone(),
            url: r.path.clone(),
            cover,
            meta,
            ..Default::default()
        }
    }

    // ---- library -------------------------------------------------------

    pub fn count(&self) -> Result<i64> {
        self.read(|c| c.query_row("SELECT count(*) FROM tracks", [], |r| r.get(0)))
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.read(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
    }

    /// The last scan report (empty before the first scan).
    pub fn last_scan(&self) -> Map<String, Value> {
        self.last_scan.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// 1.x `App.roots()`: `music_dirs` split on `:` with `~` expanded.
    pub fn roots(cfg: &Config) -> Vec<String> {
        let def = paths::home_dir()
            .join("Music")
            .to_string_lossy()
            .to_string();
        cfg.get("music_dirs", &def)
            .split(':')
            .filter(|x| !x.is_empty())
            .map(|x| norm_path(&expanduser(x).to_string_lossy()))
            .collect()
    }

    /// 1.x `App.scan()`: scan with the configured roots, excludes and batch size.
    pub fn scan_config(
        &self,
        cfg: &Config,
        progress: Option<ProgressFn>,
        force: bool,
    ) -> Result<ScanReport> {
        let excludes: Vec<String> = cfg
            .get("music_excludes", "Reaper-projects")
            .split(':')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        let raw = cfg.get("library_scan_batch", "250");
        let raw = if raw.is_empty() {
            "250".to_string()
        } else {
            raw
        };
        let batch = raw
            .trim()
            .parse::<i64>()
            .map(|b| b.clamp(25, 2000))
            .unwrap_or(250);
        self.scan(&DB::roots(cfg), progress, &excludes, batch as usize, force)
    }

    /// Incrementally scan large libraries without holding SQLite write locks.
    ///
    /// Configured root symlinks are resolved and followed. Symlinks discovered
    /// below a root are not followed. Missing/offline roots are retained and
    /// never treated as mass deletion. Database writes are committed in small
    /// batches so playback checkpoints, history and playlists remain
    /// responsive during long scans.
    pub fn scan(
        &self,
        roots: &[String],
        progress: Option<ProgressFn>,
        excludes: &[String],
        batch_size: usize,
        force: bool,
    ) -> Result<ScanReport> {
        let mut online: Vec<PathBuf> = Vec::new();
        let mut offline: Vec<String> = Vec::new();
        for x in roots.iter().filter(|x| !x.trim().is_empty()) {
            let r = expanduser(x);
            match std::fs::canonicalize(&r) {
                Ok(rr) => {
                    if rr.is_dir() && !online.contains(&rr) {
                        online.push(rr);
                    }
                }
                Err(_) => offline.push(norm_path(&r.to_string_lossy())),
            }
        }
        let patterns: Vec<String> = excludes
            .iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        let excluded = |path: &Path, name: &str| -> bool {
            if BUILTIN_EXCLUDES.contains(&name) {
                return true;
            }
            let rel = path.to_string_lossy();
            patterns.iter().any(|p| {
                fnmatch::fnmatch(name, p)
                    || fnmatch::fnmatch(&rel, p)
                    || fnmatch::fnmatch(&rel, &format!("*/{p}"))
            })
        };
        let existing: HashMap<String, (String, f64)> = self.read(|c| {
            let mut st = c.prepare("SELECT id,path,mtime FROM tracks")?;
            let v = st
                .query_map([], |r| {
                    Ok((v_str(r, "path"), (v_str(r, "id"), v_f64(r, "mtime"))))
                })?
                .collect();
            v
        })?;
        let mut seen: HashSet<String> = HashSet::new();
        let (mut checked, mut new_count, mut updated, mut last_report) = (0u64, 0u64, 0u64, 0u64);
        let mut pending: Vec<(String, String, tags::Metadata, f64, f64)> = Vec::new();
        let batch = batch_size.max(25);
        let report = |checked: u64, new: u64, updated: u64| {
            if let Some(p) = &progress {
                let mut m = Map::new();
                m.insert("checked".into(), json!(checked));
                m.insert("new".into(), json!(new));
                m.insert("updated".into(), json!(updated));
                m.insert("offline".into(), json!(offline));
                p(&m);
            }
        };
        for root in &online {
            let mut stack = vec![root.clone()];
            while let Some(base) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&base) else {
                    continue;
                };
                let mut dirs = Vec::new();
                let mut files = Vec::new();
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let p = e.path();
                    let Ok(ft) = e.file_type() else { continue };
                    let is_dir = if ft.is_symlink() {
                        std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false)
                    } else {
                        ft.is_dir()
                    };
                    if is_dir {
                        // Never recurse into hidden/excluded dirs or nested symlinks.
                        if name.starts_with('.') || excluded(&p, &name) || ft.is_symlink() {
                            continue;
                        }
                        dirs.push(p);
                    } else {
                        files.push((p, name));
                    }
                }
                // Visit subdirectories in listing order (depth first, like os.walk).
                stack.extend(dirs.into_iter().rev());
                for (f, name) in files {
                    if !AUDIO.contains(&tags::suffix(&f).as_str()) || excluded(&f, &name) {
                        continue;
                    }
                    let (Ok(real), Ok(st)) = (std::fs::canonicalize(&f), std::fs::metadata(&f))
                    else {
                        continue;
                    };
                    let real = real.to_string_lossy().to_string();
                    if seen.contains(&real) {
                        continue;
                    }
                    seen.insert(real.clone());
                    checked += 1;
                    let mtime = st.mtime() as f64 + st.mtime_nsec() as f64 / 1e9;
                    let old = existing.get(&real);
                    if !force {
                        if let Some((_, om)) = old {
                            if (om - mtime).abs() < 0.001 {
                                if checked - last_report >= 500 {
                                    report(checked, new_count, updated);
                                    last_report = checked;
                                }
                                continue;
                            }
                        }
                    }
                    let rid = match old {
                        Some((id, _)) => id.clone(),
                        None => sha1::sha1_hex(real.as_bytes()),
                    };
                    let m = tags::metadata(&f);
                    pending.push((rid, real, m, mtime, paths::now()));
                    if old.is_some() {
                        updated += 1;
                    } else {
                        new_count += 1;
                    }
                    if pending.len() >= batch {
                        self.flush_tracks(&mut pending)?;
                    }
                    if checked - last_report >= 500 {
                        report(checked, new_count, updated);
                        last_report = checked;
                    }
                }
            }
        }
        self.flush_tracks(&mut pending)?;
        // Remove missing tracks only for roots that were actually online and fully
        // traversed. An unplugged disk or broken root symlink must never empty a library.
        let remove: Vec<String> = existing
            .iter()
            .filter(|(path, _)| {
                online.iter().any(|r| Path::new(path).starts_with(r)) && !seen.contains(*path)
            })
            .map(|(_, (rid, _))| rid.clone())
            .collect();
        if !remove.is_empty() {
            self.write(|c| {
                let mut st = c.prepare("DELETE FROM tracks WHERE id=?")?;
                for id in &remove {
                    st.execute([id])?;
                }
                Ok(())
            })?;
        }
        let rep = ScanReport {
            checked,
            new: new_count,
            updated,
            removed: remove.len() as u64,
            total: self.count()?,
            offline,
            roots: online
                .iter()
                .map(|r| r.to_string_lossy().to_string())
                .collect(),
        };
        let map = rep.to_map();
        if let Ok(mut g) = self.last_scan.lock() {
            *g = map.clone();
        }
        if let Some(p) = &progress {
            let mut m = map;
            m.insert("done".into(), json!(true));
            p(&m);
        }
        Ok(rep)
    }

    fn flush_tracks(
        &self,
        pending: &mut Vec<(String, String, tags::Metadata, f64, f64)>,
    ) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let sql = "INSERT OR REPLACE INTO tracks(id,path,title,artist,album,mtime,added,album_artist,track_no,disc_no,year,genre,duration,codec,sample_rate,bit_depth,channels,bitrate) VALUES(?,?,?,?,?,?,coalesce((SELECT added FROM tracks WHERE id=?),?),?,?,?,?,?,?,?,?,?,?,?)";
        let rows = std::mem::take(pending);
        self.write(|c| {
            let mut st = c.prepare(sql)?;
            for (rid, real, m, mtime, now) in &rows {
                st.execute(params![
                    rid,
                    real,
                    m.title,
                    m.artist,
                    m.album,
                    mtime,
                    rid,
                    now,
                    m.album_artist,
                    m.track_no,
                    m.disc_no,
                    m.year,
                    m.genre,
                    m.duration,
                    m.codec,
                    m.sample_rate,
                    m.bit_depth,
                    m.channels,
                    m.bitrate
                ])?;
            }
            Ok(())
        })
    }

    pub fn search(&self, q: &str, limit: i64) -> Result<Vec<Track>> {
        if q.is_empty() {
            return self.tracks(
                &format!("SELECT * FROM tracks {ORDER_LIBRARY} LIMIT ?"),
                params![limit],
            );
        }
        let like = format!("%{q}%");
        self.tracks(
            &format!("SELECT * FROM tracks WHERE title LIKE ? OR artist LIKE ? OR album_artist LIKE ? OR album LIKE ? {ORDER_LIBRARY} LIMIT ?"),
            params![like, like, like, like, limit],
        )
    }

    pub fn random_tracks(&self, limit: i64) -> Result<Vec<Track>> {
        self.tracks(
            "SELECT * FROM tracks ORDER BY RANDOM() LIMIT ?",
            params![limit],
        )
    }

    pub fn find_artists(&self, q: &str, limit: i64) -> Result<Vec<(String, i64)>> {
        let q = q.trim();
        let base =
            format!("SELECT {ARTIST} artist,count(*) n FROM tracks WHERE trim({ARTIST})<>''");
        self.read(|c| {
            let map = |r: &Row| Ok((v_str(r, "artist"), v_i64(r, "n")));
            if q.is_empty() {
                let mut st = c.prepare(&format!("{base} GROUP BY {ARTIST} ORDER BY artist COLLATE NOCASE LIMIT ?"))?;
                let v = st.query_map(params![limit], map)?.collect();
                v
            } else {
                let mut st = c.prepare(&format!("{base} AND {ARTIST} LIKE ? COLLATE NOCASE GROUP BY {ARTIST} ORDER BY artist COLLATE NOCASE LIMIT ?"))?;
                let v = st.query_map(params![format!("%{q}%"), limit], map)?.collect();
                v
            }
        })
    }

    pub fn random_artist_tracks(&self) -> Result<(String, Vec<Track>)> {
        let a: Option<String> = self.read(|c| {
            c.query_row(
                &format!("SELECT {ARTIST} artist FROM tracks WHERE trim({ARTIST})<>'' GROUP BY {ARTIST} ORDER BY RANDOM() LIMIT 1"),
                [],
                |r| Ok(v_str(r, "artist")),
            )
            .optional()
        })?;
        match a {
            Some(a) => {
                let t = self.tracks_by_artist(&a)?;
                Ok((a, t))
            }
            None => Ok((String::new(), Vec::new())),
        }
    }

    pub fn shuffled_artist_tracks(&self, name: &str) -> Result<Vec<Track>> {
        let mut xs = self.tracks_by_artist(name)?;
        fastrand::shuffle(&mut xs);
        Ok(xs)
    }

    pub fn recently_added(&self, limit: i64) -> Result<Vec<Track>> {
        self.tracks(
            "SELECT * FROM tracks ORDER BY coalesce(added,mtime) DESC, artist,album,title LIMIT ?",
            params![limit],
        )
    }

    pub fn random_album_tracks(&self) -> Result<Vec<Track>> {
        let a: Option<(String, String)> = self.read(|c| {
            c.query_row(
                &format!("SELECT {ARTIST} artist,album FROM tracks WHERE trim(album)<>'' GROUP BY {ARTIST},album ORDER BY RANDOM() LIMIT 1"),
                [],
                |r| Ok((v_str(r, "artist"), v_str(r, "album"))),
            )
            .optional()
        })?;
        match a {
            Some((artist, album)) => self.tracks_by_album(&artist, &album),
            None => Ok(Vec::new()),
        }
    }

    pub fn local_artists(&self) -> Result<Vec<Row3>> {
        self.read(|c| {
            let mut st = c.prepare(&format!("SELECT {ARTIST} artist,count(*) n FROM tracks WHERE trim({ARTIST})<>'' GROUP BY {ARTIST} ORDER BY artist COLLATE NOCASE"))?;
            let v = st
                .query_map([], |r| {
                    let a = v_str(r, "artist");
                    let n = v_i64(r, "n");
                    Ok((a.clone(), format!("local:artist:{a}"), format!("{n} tracks")))
                })?
                .collect();
            v
        })
    }

    pub fn local_albums(&self) -> Result<Vec<Row3>> {
        self.read(|c| {
            let mut st = c.prepare(&format!("SELECT album,{ARTIST} artist,count(*) n,min(year) year FROM tracks WHERE trim(album)<>'' GROUP BY album,{ARTIST} ORDER BY artist COLLATE NOCASE,year,album COLLATE NOCASE"))?;
            let v = st
                .query_map([], |r| {
                    let (al, ar, n) = (v_str(r, "album"), v_str(r, "artist"), v_i64(r, "n"));
                    Ok((
                        al.clone(),
                        format!("local:album:{ar}\t{al}"),
                        format!("{ar} · {n} tracks"),
                    ))
                })?
                .collect();
            v
        })
    }

    pub fn local_folders(&self) -> Result<Vec<Row3>> {
        let paths_: Vec<String> = self.read(|c| {
            let mut st = c.prepare("SELECT path FROM tracks ORDER BY path")?;
            let v = st.query_map([], |r| Ok(v_str(r, "path")))?.collect();
            v
        })?;
        let mut d: Vec<(String, i64)> = Vec::new();
        let mut idx: HashMap<String, usize> = HashMap::new();
        for p in paths_ {
            let parent = Path::new(&p)
                .parent()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent = if parent.is_empty() {
                ".".to_string()
            } else {
                parent
            };
            match idx.get(&parent) {
                Some(&i) => d[i].1 += 1,
                None => {
                    idx.insert(parent.clone(), d.len());
                    d.push((parent, 1));
                }
            }
        }
        d.sort_by_key(|(k, _)| casefold(k));
        Ok(d.into_iter()
            .map(|(k, v)| {
                let name = Path::new(&k)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| k.clone());
                (
                    name,
                    format!("local:folder:{k}"),
                    format!("{v} tracks · {k}"),
                )
            })
            .collect())
    }

    pub fn tracks_by_artist(&self, name: &str) -> Result<Vec<Track>> {
        self.tracks(
            "SELECT * FROM tracks WHERE artist=? OR album_artist=? ORDER BY year,album,disc_no,track_no,title,path",
            params![name, name],
        )
    }

    pub fn tracks_by_album(&self, artist: &str, album: &str) -> Result<Vec<Track>> {
        self.tracks(
            "SELECT * FROM tracks WHERE album=? AND (artist=? OR album_artist=?) ORDER BY disc_no,track_no,title,path",
            params![album, artist, artist],
        )
    }

    pub fn tracks_by_folder(&self, path: &str) -> Result<Vec<Track>> {
        let pat = format!("{}/%", norm_path(path));
        self.tracks(
            "SELECT * FROM tracks WHERE path LIKE ? ORDER BY artist,album,title",
            params![pat],
        )
    }

    // ---- resume --------------------------------------------------------

    pub fn resume_set(&self, item_id: &str, pos: f64) -> Result<()> {
        if item_id.is_empty() {
            return Ok(());
        }
        let pos = if pos.is_finite() { pos.max(0.0) } else { 0.0 };
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO resume(item_id,pos,updated) VALUES(?,?,?)",
                params![item_id, pos, paths::now()],
            )
        })?;
        Ok(())
    }

    pub fn resume_get(&self, item_id: &str, legacy_id: Option<&str>) -> Result<f64> {
        if item_id.is_empty() {
            return Ok(0.0);
        }
        self.read(|c| {
            let q = |id: &str| {
                c.query_row("SELECT pos FROM resume WHERE item_id=?", [id], |r| {
                    Ok(v_f64(r, "pos"))
                })
                .optional()
            };
            let mut r = q(item_id)?;
            if r.is_none() {
                if let Some(l) = legacy_id.filter(|l| !l.is_empty()) {
                    r = q(l)?;
                }
            }
            Ok(r.unwrap_or(0.0))
        })
    }

    // ---- favorites / history -------------------------------------------

    /// Toggle a favorite; true when it was added.
    pub fn favorite(&self, t: &Track) -> Result<bool> {
        let key = t.key();
        let data = track_json(t);
        self.write(|c| {
            let row: Option<String> = c
                .query_row(
                    "SELECT id FROM favorites WHERE id IN (?,?) ORDER BY CASE WHEN id=? THEN 0 ELSE 1 END LIMIT 1",
                    params![key, t.id, key],
                    |r| Ok(v_str(r, "id")),
                )
                .optional()?;
            if let Some(id) = row {
                c.execute("DELETE FROM favorites WHERE id=?", [id])?;
                return Ok(false);
            }
            c.execute(
                "INSERT INTO favorites VALUES(?,?,?)",
                params![key, data, paths::now()],
            )?;
            Ok(true)
        })
    }

    pub fn favorites(&self) -> Result<Vec<Track>> {
        self.data_tracks("SELECT data FROM favorites ORDER BY added DESC", [])
    }

    pub fn history_add(&self, t: &Track) -> Result<()> {
        let data = track_json(t);
        self.write(|c| {
            c.execute(
                "INSERT INTO history(item_id,data,played) VALUES(?,?,?)",
                params![t.key(), data, paths::now()],
            )
        })?;
        Ok(())
    }

    pub fn history(&self, limit: i64) -> Result<Vec<Track>> {
        self.data_tracks(
            "SELECT data FROM history ORDER BY id DESC LIMIT ?",
            params![limit],
        )
    }

    pub fn most_played(&self, limit: i64) -> Result<Vec<Track>> {
        let rows: Vec<(String, i64)> = self.read(|c| {
            let mut st = c.prepare("SELECT data,count(*) n,max(played) last FROM history GROUP BY item_id ORDER BY n DESC,last DESC LIMIT ?")?;
            let v = st
                .query_map(params![limit], |r| Ok((v_str(r, "data"), v_i64(r, "n"))))?
                .collect();
            v
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(d, n)| {
                let mut t = parse_track(&d)?;
                t.set_meta("play_count", n);
                Some(t)
            })
            .collect())
    }

    pub fn never_played(&self, limit: i64) -> Result<Vec<Track>> {
        self.tracks(
            &format!("SELECT * FROM tracks WHERE ('local:'||id) NOT IN (SELECT DISTINCT item_id FROM history) AND id NOT IN (SELECT DISTINCT item_id FROM history) {ORDER_LIBRARY} LIMIT ?"),
            params![limit],
        )
    }

    pub fn unplayed_albums(&self, limit: i64) -> Result<Vec<Row3>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT artist,album,count(*) n FROM tracks WHERE trim(album)<>'' GROUP BY artist,album HAVING sum(CASE WHEN (('local:'||id) IN (SELECT DISTINCT item_id FROM history) OR id IN (SELECT DISTINCT item_id FROM history)) THEN 1 ELSE 0 END)=0 ORDER BY artist,album LIMIT ?")?;
            let v = st
                .query_map(params![limit], |r| {
                    let (ar, al, n) = (v_str(r, "artist"), v_str(r, "album"), v_i64(r, "n"));
                    Ok((
                        al.clone(),
                        format!("local:album:{ar}\t{al}"),
                        format!("{ar} · {n} tracks · unplayed"),
                    ))
                })?
                .collect();
            v
        })
    }

    pub fn rated(&self, min_rating: i64, limit: i64) -> Result<Vec<Track>> {
        self.tracks(
            "SELECT t.* FROM tracks t JOIN ratings r ON r.item_id IN (t.id,'local:'||t.id) WHERE r.rating>=? ORDER BY r.rating DESC,t.artist,t.album,t.title LIMIT ?",
            params![min_rating, limit],
        )
    }

    // ---- playlists -----------------------------------------------------

    pub fn playlists(&self) -> Result<Vec<Row3>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT p.id,p.name,count(i.id) n FROM playlists p LEFT JOIN playlist_items i ON i.playlist_id=p.id GROUP BY p.id ORDER BY p.name")?;
            let v = st
                .query_map([], |r| {
                    Ok((
                        v_str(r, "name"),
                        format!("playlist:{}", v_i64(r, "id")),
                        format!("{} tracks", v_i64(r, "n")),
                    ))
                })?
                .collect();
            v
        })
    }

    pub fn playlist_create(&self, name: &str) -> Result<Option<i64>> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        self.write(|c| {
            c.execute(
                "INSERT OR IGNORE INTO playlists(name,created) VALUES(?,?)",
                params![name, paths::now()],
            )?;
            c.query_row("SELECT id FROM playlists WHERE name=?", [name], |r| {
                r.get(0)
            })
            .optional()
        })
    }

    pub fn playlist_add(&self, pid: i64, t: &Track) -> Result<()> {
        let data = track_json(t);
        self.write(|c| {
            let pos: i64 = c.query_row(
                "SELECT coalesce(max(position),-1)+1 FROM playlist_items WHERE playlist_id=?",
                [pid],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO playlist_items(playlist_id,data,position,added) VALUES(?,?,?,?)",
                params![pid, data, pos, paths::now()],
            )
        })?;
        Ok(())
    }

    pub fn playlist_tracks(&self, pid: i64) -> Result<Vec<Track>> {
        self.data_tracks(
            "SELECT data FROM playlist_items WHERE playlist_id=? ORDER BY position,id",
            params![pid],
        )
    }

    pub fn playlist_name(&self, pid: i64) -> Result<String> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT name FROM playlists WHERE id=?", [pid], |r| {
                    Ok(v_str(r, "name"))
                })
                .optional()?
                .unwrap_or_else(|| "Playlist".into()),
            )
        })
    }

    pub fn playlist_delete(&self, pid: i64) -> Result<()> {
        self.write(|c| {
            c.execute("DELETE FROM playlist_items WHERE playlist_id=?", [pid])?;
            c.execute("DELETE FROM playlists WHERE id=?", [pid])
        })?;
        Ok(())
    }

    /// Fails (like 1.x) when another playlist already has the name.
    pub fn playlist_rename(&self, pid: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if !name.is_empty() {
            self.write(|c| {
                c.execute("UPDATE playlists SET name=? WHERE id=?", params![name, pid])
            })?;
        }
        Ok(())
    }

    fn playlist_ids(c: &Connection, pid: i64) -> rusqlite::Result<Vec<i64>> {
        let mut st =
            c.prepare("SELECT id FROM playlist_items WHERE playlist_id=? ORDER BY position,id")?;
        let v = st.query_map([pid], |r| r.get(0))?.collect();
        v
    }

    pub fn playlist_remove(&self, pid: i64, index: i64) -> Result<()> {
        self.write(|c| {
            let rows = DB::playlist_ids(c, pid)?;
            if index >= 0 && (index as usize) < rows.len() {
                c.execute(
                    "DELETE FROM playlist_items WHERE id=?",
                    [rows[index as usize]],
                )?;
            }
            Ok(())
        })
    }

    /// Swap item `index` with `index + delta` (clamped); returns the new index.
    pub fn playlist_move(&self, pid: i64, index: i64, delta: i64) -> Result<i64> {
        self.write(|c| {
            let rows = DB::playlist_ids(c, pid)?;
            let j = (index + delta).min(rows.len() as i64 - 1).max(0);
            if rows.is_empty() || j == index || index < 0 || index as usize >= rows.len() {
                return Ok(index);
            }
            let (a, b) = (rows[index as usize], rows[j as usize]);
            c.execute("UPDATE playlist_items SET position=-1 WHERE id=?", [a])?;
            c.execute(
                "UPDATE playlist_items SET position=? WHERE id=?",
                params![index, b],
            )?;
            c.execute(
                "UPDATE playlist_items SET position=? WHERE id=?",
                params![j, a],
            )?;
            Ok(j)
        })
    }

    // ---- ratings -------------------------------------------------------

    fn rating_key(item_id: &str, source: &str) -> String {
        if item_id.contains(':') {
            item_id.to_string()
        } else {
            format!("{source}:{item_id}")
        }
    }

    pub fn rating_get(&self, item_id: &str, source: &str) -> Result<i64> {
        let key = DB::rating_key(item_id, source);
        self.read(|c| {
            Ok(c.query_row(
                "SELECT rating FROM ratings WHERE item_id IN (?,?) ORDER BY CASE WHEN item_id=? THEN 0 ELSE 1 END LIMIT 1",
                params![key, item_id, key],
                |r| Ok(v_i64(r, "rating")),
            )
            .optional()?
            .unwrap_or(0))
        })
    }

    pub fn rating_set(&self, item_id: &str, rating: i64, source: &str) -> Result<()> {
        let rating = rating.clamp(0, 5);
        if item_id.is_empty() {
            return Ok(());
        }
        let key = DB::rating_key(item_id, source);
        if rating != 0 {
            self.write(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO ratings(item_id,rating,updated) VALUES(?,?,?)",
                    params![key, rating, paths::now()],
                )
            })?;
        } else {
            self.write(|c| c.execute("DELETE FROM ratings WHERE item_id=?", [&key]))?;
        }
        Ok(())
    }

    // ---- discoveries ---------------------------------------------------

    pub fn discovery_add(
        &self,
        title: &str,
        artist: &str,
        source: &str,
        data: Option<&Map<String, Value>>,
    ) -> Result<bool> {
        let (title, artist) = (title.trim(), artist.trim());
        if title.is_empty() {
            return Ok(false);
        }
        let payload = serde_json::to_string(&data.cloned().unwrap_or_default())
            .unwrap_or_else(|_| "{}".into());
        self.write(|c| {
            c.execute(
                "INSERT INTO discoveries(title,artist,source,data,added) VALUES(?,?,?,?,?)",
                params![title, artist, source, payload, paths::now()],
            )
        })?;
        Ok(true)
    }

    pub fn discoveries(&self, limit: i64) -> Result<Vec<Track>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT * FROM discoveries ORDER BY id DESC LIMIT ?")?;
            let v = st
                .query_map(params![limit], |r| {
                    let mut meta = Map::new();
                    meta.insert("origin".into(), v_json(r, "source"));
                    meta.insert("added".into(), v_json(r, "added"));
                    let raw = v_str(r, "data");
                    let raw = if raw.is_empty() {
                        "{}".to_string()
                    } else {
                        raw
                    };
                    if let Ok(Value::Object(extra)) = serde_json::from_str::<Value>(&raw) {
                        for (k, v) in extra {
                            meta.insert(k, v);
                        }
                    }
                    Ok(Track {
                        source: "discovery".into(),
                        id: format!("discovery:{}", v_i64(r, "id")),
                        title: v_str(r, "title"),
                        artist: v_str(r, "artist"),
                        kind: "discovery".into(),
                        seekable: false,
                        meta,
                        ..Default::default()
                    })
                })?
                .collect();
            v
        })
    }

    // ---- bookmarks -----------------------------------------------------

    pub fn bookmark_add(&self, t: &Track, pos: f64) -> Result<i64> {
        let data = track_json(t);
        let pos = if pos.is_finite() { pos.max(0.0) } else { 0.0 };
        self.write(|c| {
            c.execute(
                "INSERT INTO bookmarks(item_key,data,pos,added) VALUES(?,?,?,?)",
                params![t.key(), data, pos, paths::now()],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn bookmarks(&self, limit: i64) -> Result<Vec<Track>> {
        let rows: Vec<(i64, String, f64, Value)> = self.read(|c| {
            let mut st =
                c.prepare("SELECT id,data,pos,added FROM bookmarks ORDER BY added DESC LIMIT ?")?;
            let v = st
                .query_map(params![limit], |r| {
                    Ok((
                        v_i64(r, "id"),
                        v_str(r, "data"),
                        v_f64(r, "pos"),
                        v_json(r, "added"),
                    ))
                })?
                .collect();
            v
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, d, pos, added)| {
                let mut t = parse_track(&d)?;
                t.set_meta("bookmark_id", id);
                t.set_meta("bookmark_pos", pos);
                t.set_meta("bookmark_added", added);
                Some(t)
            })
            .collect())
    }

    pub fn bookmark_delete(&self, bid: i64) -> Result<()> {
        self.write(|c| c.execute("DELETE FROM bookmarks WHERE id=?", [bid]))?;
        Ok(())
    }

    // ---- play events ---------------------------------------------------

    pub fn play_started(&self, t: &Track) -> Result<Option<i64>> {
        let data = track_json(t);
        self.write(|c| {
            c.execute(
                "INSERT INTO play_events(item_key,data,started,seconds,completed) VALUES(?,?,?,?,0)",
                params![t.key(), data, paths::now(), 0],
            )?;
            Ok(Some(c.last_insert_rowid()))
        })
    }

    pub fn play_finished(&self, event_id: i64, seconds: f64, completed: bool) -> Result<()> {
        if event_id == 0 {
            return Ok(());
        }
        let s = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        self.write(|c| {
            c.execute(
                "UPDATE play_events SET ended=?,seconds=max(seconds,?),completed=? WHERE id=?",
                params![paths::now(), s, completed as i64, event_id],
            )
        })?;
        Ok(())
    }

    pub fn listening_stats(&self, days: f64) -> Result<ListeningStats> {
        let cutoff = paths::now() - days * 86400.0;
        let rows: Vec<(String, f64, i64)> = self.read(|c| {
            let mut st = c.prepare(
                "SELECT data,seconds,completed FROM play_events WHERE started>=? ORDER BY started DESC",
            )?;
            let v = st
                .query_map(params![cutoff], |r| {
                    Ok((v_str(r, "data"), v_f64(r, "seconds"), v_i64(r, "completed")))
                })?
                .collect();
            v
        })?;
        let mut out = ListeningStats::default();
        let mut artists: Vec<(String, i64)> = Vec::new();
        let mut sources: Vec<(String, i64)> = Vec::new();
        let bump = |v: &mut Vec<(String, i64)>, k: &str| match v.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 += 1,
            None => v.push((k.to_string(), 1)),
        };
        let mut seconds = 0.0;
        for (d, heard, completed) in rows {
            let Some(t) = parse_track(&d) else { continue };
            out.plays += 1;
            seconds += heard;
            out.completed += completed;
            let is_sub = heard >= 30.0 || (t.duration > 0.0 && heard >= t.duration * 0.5);
            out.substantial += is_sub as i64;
            if is_sub && !t.artist.is_empty() {
                bump(&mut artists, &t.artist);
            }
            if is_sub {
                bump(&mut sources, &t.source);
            }
        }
        out.minutes = (seconds / 60.0) as i64;
        artists.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| casefold(&a.0).cmp(&casefold(&b.0)))
        });
        artists.truncate(20);
        sources.sort_by_key(|a| std::cmp::Reverse(a.1));
        out.artists = artists;
        out.sources = sources;
        Ok(out)
    }

    // ---- radio ---------------------------------------------------------

    pub fn radios(&self) -> Result<Vec<Track>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT * FROM radio_stations ORDER BY name")?;
            let v = st
                .query_map([], |r| {
                    let mut meta = Map::new();
                    meta.insert("homepage".into(), json!(v_str(r, "homepage")));
                    Ok(Track {
                        source: "radio".into(),
                        id: format!("custom:{}", v_i64(r, "id")),
                        title: v_str(r, "name"),
                        artist: "Custom radio".into(),
                        url: v_str(r, "url"),
                        cover: v_str(r, "artwork"),
                        kind: "radio".into(),
                        seekable: false,
                        meta,
                        ..Default::default()
                    })
                })?
                .collect();
            v
        })
    }

    pub fn radio_add(&self, name: &str, url: &str, homepage: &str, artwork: &str) -> Result<()> {
        if name.trim().is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(Error::msg("Station needs a name and an http(s) stream URL"));
        }
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO radio_stations(name,url,homepage,artwork,added) VALUES(?,?,?,?,?)",
                params![name.trim(), url.trim(), homepage.trim(), artwork.trim(), paths::now()],
            )
        })?;
        Ok(())
    }

    pub fn radio_delete(&self, station_id: i64) -> Result<()> {
        self.write(|c| c.execute("DELETE FROM radio_stations WHERE id=?", [station_id]))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
