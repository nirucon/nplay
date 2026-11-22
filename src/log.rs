//! Private, redacting, rotating log (`$STATE/nplay.log`, 2 MiB x 3, mode
//! 0600), same line format as 1.x: `2026-11-22 10:00:00,123 INFO nplay: msg`.
//!
//! Use the `info!`, `warn!`, `error!` and `debug!` macros at the crate root.

use crate::paths;
use crate::redact::{contains_secret, redact_text};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const BACKUPS: usize = 3;

struct Sink {
    file: Option<File>,
    size: u64,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
static DEBUG: OnceLock<bool> = OnceLock::new();

pub fn log_path() -> PathBuf {
    paths::state().join("nplay.log")
}

pub fn mpv_log_path() -> PathBuf {
    paths::state().join("mpv.log")
}

fn open() -> Option<File> {
    let p = log_path();
    let _ = std::fs::create_dir_all(paths::state());
    let f = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&p)
        .ok()?;
    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    Some(f)
}

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| {
        let (files, lines) = migrate_logs();
        let file = open();
        let size = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let m = Mutex::new(Sink { file, size });
        if files > 0 {
            write_line(
                &m,
                "INFO",
                &format!(
                    "log redaction migration: removed credentials from {lines} lines in {files} files"
                ),
            );
        }
        m
    })
}

fn rotate(s: &mut Sink) {
    s.file = None;
    let base = log_path();
    for i in (1..BACKUPS).rev() {
        let from = PathBuf::from(format!("{}.{}", base.display(), i));
        let to = PathBuf::from(format!("{}.{}", base.display(), i + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let first = PathBuf::from(format!("{}.1", base.display()));
    let _ = std::fs::rename(&base, &first);
    for i in 1..=BACKUPS {
        let p = PathBuf::from(format!("{}.{}", base.display(), i));
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    s.file = open();
    s.size = 0;
}

/// Local time as `YYYY-MM-DD HH:MM:SS,mmm` (Python logging's asctime).
pub fn asctime() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let ms = now.subsec_millis();
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02},{:03}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        ms
    )
}

fn write_line(m: &Mutex<Sink>, level: &str, msg: &str) {
    let line = format!("{} {} nplay: {}\n", asctime(), level, redact_text(msg));
    let mut s = match m.lock() {
        Ok(s) => s,
        Err(p) => p.into_inner(),
    };
    if s.size + line.len() as u64 > MAX_BYTES && s.size > 0 {
        rotate(&mut s);
    }
    if let Some(f) = s.file.as_mut() {
        if f.write_all(line.as_bytes()).is_ok() {
            s.size += line.len() as u64;
        }
    }
}

pub fn debug_enabled() -> bool {
    *DEBUG.get_or_init(|| std::env::var_os("NPLAY_DEBUG").is_some_and(|v| !v.is_empty()))
}

pub fn write(level: &str, msg: &str) {
    if level == "DEBUG" && !debug_enabled() {
        return;
    }
    write_line(sink(), level, msg);
}

/// Every NPLAY-owned log that may have been written by an older version.
pub fn log_files() -> Vec<PathBuf> {
    let base = log_path();
    let mut out = vec![base.clone()];
    for i in 1..10 {
        out.push(PathBuf::from(format!("{}.{}", base.display(), i)));
    }
    out.push(mpv_log_path());
    out.into_iter().filter(|p| p.exists()).collect()
}

/// Redact credentials in one file in place (temp file + rename). Returns
/// the number of changed lines.
pub fn redact_file(path: &std::path::Path) -> usize {
    let Ok(raw) = std::fs::read(path) else {
        return 0;
    };
    let text = String::from_utf8_lossy(&raw);
    if !contains_secret(&text) {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        return 0;
    }
    let mut count = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let clean = redact_text(line);
        if clean != line {
            count += 1;
        }
        out.push_str(&clean);
    }
    if paths::write_atomic(path, out.as_bytes(), 0o600).is_err() {
        return 0;
    }
    count
}

/// Idempotent upgrade migration: redact credential-bearing logs written by
/// NPLAY <= 1.7 and remove this user's stale `/tmp/nplay-cava-*.conf`.
/// Returns (files_changed, lines_redacted).
pub fn migrate_logs() -> (usize, usize) {
    let (mut files, mut lines) = (0, 0);
    for p in log_files() {
        let n = redact_file(&p);
        if n > 0 {
            files += 1;
            lines += n;
        }
    }
    cleanup_tmp_cava();
    (files, lines)
}

fn cleanup_tmp_cava() {
    use std::os::unix::fs::MetadataExt;
    let tmp = std::env::temp_dir();
    let Ok(rd) = std::fs::read_dir(&tmp) else {
        return;
    };
    let uid = unsafe { libc::getuid() };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("nplay-cava-") && name.ends_with(".conf") {
            if let Ok(st) = std::fs::symlink_metadata(e.path()) {
                if st.uid() == uid && st.is_file() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

/// Number of ERROR and WARNING lines in the current log (doctor).
pub fn recent_error_count() -> usize {
    std::fs::read(log_path())
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .filter(|l| l.contains(" ERROR ") || l.contains(" WARNING "))
                .count()
        })
        .unwrap_or(0)
}

#[macro_export]
macro_rules! info {
    ($($a:tt)*) => { $crate::log::write("INFO", &format!($($a)*)) };
}
#[macro_export]
macro_rules! warn {
    ($($a:tt)*) => { $crate::log::write("WARNING", &format!($($a)*)) };
}
#[macro_export]
macro_rules! error {
    ($($a:tt)*) => { $crate::log::write("ERROR", &format!($($a)*)) };
}
#[macro_export]
macro_rules! debug {
    ($($a:tt)*) => { $crate::log::write("DEBUG", &format!($($a)*)) };
}
