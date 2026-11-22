//! XDG locations shared by every part of NPLAY. They match the Python 1.x
//! layout exactly so existing libraries, logs, sessions and secrets open.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP: &str = "nplay";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

/// `$XDG_DATA_HOME/nplay` (library.db, statistics, spotify-local, artwork).
pub fn data() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join(APP)
}

/// `$XDG_STATE_HOME/nplay` (logs, session.json), mode 0700.
pub fn state() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join(APP)
}

/// `$XDG_CONFIG_HOME/nplay` (config.ini, secrets.ini, themes).
pub fn config() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join(APP)
}

/// `$XDG_CACHE_HOME/nplay`.
pub fn cache() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join(APP)
}

pub fn home_dir() -> PathBuf {
    home()
}

/// Create the data, state and config directories (state is private).
pub fn ensure() {
    for p in [data(), state(), config()] {
        let _ = std::fs::create_dir_all(&p);
    }
    let _ = std::fs::set_permissions(state(), std::fs::Permissions::from_mode(0o700));
}

static FALLBACK_RUNTIME: OnceLock<PathBuf> = OnceLock::new();

/// NPLAY's private (0700, owner-only) runtime directory for sockets, locks and
/// temporary configs: `$XDG_RUNTIME_DIR/nplay`, else `$STATE/run`, else a fresh
/// private temp directory (never a shared /tmp file).
pub fn private_runtime_dir() -> PathBuf {
    let d = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(b) if !b.is_empty() => PathBuf::from(b).join(APP),
        _ => state().join("run"),
    };
    if std::fs::create_dir_all(&d).is_ok() {
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        if let Ok(st) = std::fs::symlink_metadata(&d) {
            if st.is_dir() && st.uid() == unsafe { libc::getuid() } && d.as_os_str().len() <= 80 {
                return d;
            }
        }
    }
    FALLBACK_RUNTIME
        .get_or_init(|| {
            let base = std::env::temp_dir();
            for _ in 0..64 {
                let p = base.join(format!("nplay-{:08x}", fastrand::u32(..)));
                if std::fs::create_dir(&p).is_ok() {
                    let _ =
                        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
                    return p;
                }
            }
            base
        })
        .clone()
}

/// Write `data` to `path` atomically (temp file in the same directory + rename)
/// with the given mode.
pub fn write_atomic(path: &std::path::Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(data)?;
    }
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    std::fs::rename(&tmp, path)
}

/// `which(1)`: the first executable `name` on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_exec(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| is_exec(p))
}

fn is_exec(p: &std::path::Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Seconds since the Unix epoch as f64 (Python `time.time()`).
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Monotonic seconds (Python `time.monotonic()`), CLOCK_MONOTONIC.
pub fn monotonic() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}
