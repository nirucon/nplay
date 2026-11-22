//! mpv JSON-IPC player (port of `nplay/player.py`).
//!
//! One mpv process per track, started idle (`--idle=yes --pause=yes`) with a
//! per-generation IPC socket in NPLAY's private 0700 runtime directory. The
//! media URL is never part of argv (readable via `/proc/<pid>/cmdline`): it is
//! sent as `loadfile <url> replace` over IPC. mpv's log is routed through a
//! pipe, redacted (Subsonic tokens) and written to `mpv.log` (0600, truncated
//! per start). One event thread per generation blocks on the IPC socket (no
//! polling, no timers): when stopped, the player has no threads and no CPU use.
//!
//! Public API (all `&self`; `Player` is `Clone + Send + Sync`):
//! - `Player::new(on_end: Option<EndCallback>)`; `set_on_end(cb)`.
//!   `on_end()` runs on the event thread when the track reaches end-of-file.
//! - `set_binary(path)` (default `mpv`), `set_log_path(path)` (default
//!   `log::mpv_log_path()`): overrides for tests / fake-mpv.
//! - `start(url, &StartOptions) -> Result<()>`: stop the previous mpv, spawn,
//!   load and wait until mpv reports the media loaded (`file-loaded` event, or
//!   the IPC state `path == url && idle-active == false`). Errors are redacted
//!   and end with ` · see <mpv.log>`. Playback stays paused: the caller unpauses
//!   (`cmd(["set_property","pause",false])`) once it commits the track.
//! - `StartOptions` expresses every mpv option 1.x passes: `replaygain`
//!   (`track`/`album`/`no`), `preamp` dB, `clip`, `gapless`, `ytdl_path`,
//!   `timeout` s, plus `volume` (initial, 100), `volume_max` and `extra_args`.
//!   `StartOptions::from_config(cfg, replaygain, ytdl_path)` reads
//!   `replaygain_preamp`, `replaygain_clip` and `gapless` like 1.x.
//! - `normalization_mode(enabled, mode, track_album, context_albums)`: the 1.x
//!   `App.normalization_mode` rule (`no`, `track`, `album`, `auto`).
//! - `cmd(args) -> Option<Value>`: one IPC command on the current socket;
//!   `Some(data)` on mpv `success` (data may be `Null`), else `None`.
//!   `prop(name)`, `prop_f64(name)`, `prop_bool(name)`.
//! - `active()`: mpv alive, socket present and not idle.
//! - `toggle()`, `set_pause(bool)`, `set_volume(v)` (0..150), `set_position(s)`,
//!   `seek(delta_s)`, `volume(delta)`, `mute()`, `position()`, `duration()`.
//! - `stop()`: SIGTERM, reap (1.5 s), SIGKILL, reap (2 s), remove own socket.
//! - `last_error()`, `is_ready()`, `pid()`, `socket_path()`, `generation()`.
//! - `playback_status(sock) -> Option<(Value, Value, Value)>`: `path`,
//!   `idle-active`, `core-idle` of a specific socket (readiness probe).
//! - `mpv_args(sock, log_fd, opts) -> Vec<String>`: the exact argv after `mpv`.

use crate::core::{Error, ErrorKind, Result};
use crate::redact::{redact_text, redact_url};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub type EndCallback = Arc<dyn Fn() + Send + Sync>;

/// Every mpv option NPLAY sets per start.
#[derive(Clone, Debug, PartialEq)]
pub struct StartOptions {
    /// `--replaygain`: `track`, `album` or `no`.
    pub replaygain: String,
    /// `--replaygain-preamp` in dB.
    pub preamp: f64,
    /// `--replaygain-clip`.
    pub clip: bool,
    /// `--gapless-audio`.
    pub gapless: bool,
    /// yt-dlp binary for mpv's ytdl hook (YouTube only).
    pub ytdl_path: Option<String>,
    /// Seconds to wait for the media to load (minimum 2).
    pub timeout: f64,
    /// `--volume` at start (1.x always 100; the app sets its volume after).
    pub volume: f64,
    /// `--volume-max` (mpv's default is 130); `None` leaves it to mpv.
    pub volume_max: Option<f64>,
    /// Further `--option=value` arguments (never media URLs).
    pub extra_args: Vec<String>,
}

impl Default for StartOptions {
    fn default() -> Self {
        StartOptions {
            replaygain: "track".into(),
            preamp: 0.0,
            clip: true,
            gapless: false,
            ytdl_path: None,
            timeout: 20.0,
            volume: 100.0,
            volume_max: None,
            extra_args: Vec::new(),
        }
    }
}

impl StartOptions {
    /// 1.x: `replaygain=normalization_mode(t)`, `preamp=replaygain_preamp`,
    /// `clip=replaygain_clip`, `gapless=gapless`, `ytdl_path` for YouTube.
    pub fn from_config(
        cfg: &crate::config::Config,
        replaygain: &str,
        ytdl_path: Option<String>,
    ) -> StartOptions {
        let preamp = cfg.get("replaygain_preamp", "0");
        StartOptions {
            replaygain: replaygain.to_string(),
            preamp: preamp.trim().parse::<f64>().unwrap_or(0.0),
            clip: cfg.getbool("replaygain_clip", true),
            gapless: cfg.getbool("gapless", false),
            ytdl_path,
            ..Default::default()
        }
    }
}

/// 1.x `App.normalization_mode`: `no` when disabled; `track`/`album` when
/// forced; `auto` picks `album` only when the context (more than one entry)
/// is a single album and the track has an album.
pub fn normalization_mode(
    enabled: bool,
    mode: &str,
    track_album: &str,
    context_albums: &[&str],
) -> &'static str {
    if !enabled {
        return "no";
    }
    match mode.to_lowercase().as_str() {
        "track" => return "track",
        "album" => return "album",
        _ => {}
    }
    let mut albums: Vec<&str> = context_albums
        .iter()
        .copied()
        .filter(|a| !a.is_empty())
        .collect();
    albums.sort_unstable();
    albums.dedup();
    if !track_album.is_empty() && albums.len() == 1 && context_albums.len() > 1 {
        "album"
    } else {
        "track"
    }
}

/// Python's `f'{float(x):g}'` for the preamp.
fn fmt_g(v: f64) -> String {
    if v.is_finite() && v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The argv after the program name. No media URL is ever included.
pub fn mpv_args(sock: &Path, log_fd: i32, opts: &StartOptions) -> Vec<String> {
    let yn = |b: bool| if b { "yes" } else { "no" };
    let mut a = vec![
        "--no-video".to_string(),
        "--idle=yes".into(),
        "--really-quiet".into(),
        format!("--log-file=/dev/fd/{log_fd}"),
        format!("--input-ipc-server={}", sock.display()),
        format!("--volume={}", fmt_g(opts.volume)),
        "--pause=yes".into(),
        "--keep-open=no".into(),
        format!("--replaygain={}", opts.replaygain),
        format!("--replaygain-preamp={}", fmt_g(opts.preamp)),
        format!("--replaygain-clip={}", yn(opts.clip)),
        format!("--gapless-audio={}", yn(opts.gapless)),
    ];
    if let Some(m) = opts.volume_max {
        a.push(format!("--volume-max={}", fmt_g(m)));
    }
    if let Some(y) = opts.ytdl_path.as_deref().filter(|y| !y.is_empty()) {
        a.push(format!("--script-opts=ytdl_hook-ytdl_path={y}"));
    }
    a.extend(opts.extra_args.iter().cloned());
    a
}

#[derive(Default)]
struct State {
    proc: Option<Child>,
    sock: Option<PathBuf>,
    evsock: Option<UnixStream>,
}

#[derive(Default)]
struct Flags {
    ready: bool,
    failed: bool,
}

struct Inner {
    start_lock: Mutex<()>,
    state: Mutex<State>,
    generation: AtomicU64,
    flags: Mutex<Flags>,
    cv: Condvar,
    last_error: Mutex<String>,
    on_end: Mutex<Option<EndCallback>>,
    binary: Mutex<String>,
    log_path: Mutex<Option<PathBuf>>,
}

#[derive(Clone)]
pub struct Player {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// One request on a fresh connection to `sock`; the reply line (events that
/// mpv broadcasts on the same connection are skipped). 0.35 s timeouts.
fn ipc(sock: &Path, args: &[Value]) -> Option<Value> {
    if !sock.exists() {
        return None;
    }
    let mut s = UnixStream::connect(sock).ok()?;
    let t = Some(Duration::from_millis(350));
    s.set_read_timeout(t).ok()?;
    s.set_write_timeout(t).ok()?;
    let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut line = json!({"command": args, "request_id": id}).to_string();
    line.push('\n');
    s.write_all(line.as_bytes()).ok()?;
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        while let Some(i) = data.iter().position(|b| *b == b'\n') {
            let rest = data.split_off(i + 1);
            let l = std::mem::replace(&mut data, rest);
            if let Ok(v) = serde_json::from_slice::<Value>(&l) {
                if v.get("event").is_none() {
                    return Some(v);
                }
            } else {
                return None;
            }
        }
        let n = s.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() > 1_048_576 {
            return None;
        }
    }
}

fn success(r: &Option<Value>) -> bool {
    r.as_ref().and_then(|v| v.get("error")).and_then(Value::as_str) == Some("success")
}

/// `path`, `idle-active` and `core-idle` of a specific (generation) socket.
pub fn playback_status(sock: &Path) -> Option<(Value, Value, Value)> {
    let mut vals = Vec::with_capacity(3);
    for name in ["path", "idle-active", "core-idle"] {
        let r = ipc(sock, &[json!("get_property"), json!(name)]);
        if !success(&r) {
            return None;
        }
        vals.push(r?.get("data").cloned().unwrap_or(Value::Null));
    }
    let c = vals.pop()?;
    let b = vals.pop()?;
    let a = vals.pop()?;
    Some((a, b, c))
}

/// Copy one mpv's log from the pipe into `path` (0600, truncated), redacting
/// credentials line by line. Blocks on read; ends at EOF when mpv exits.
fn copy_redacted_log(mut r: std::fs::File, path: PathBuf) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .ok();
    if out.is_some() {
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let mut emit = |bytes: &[u8]| {
        if let Some(f) = out.as_mut() {
            let _ = f.write_all(redact_text(&String::from_utf8_lossy(bytes)).as_bytes());
        }
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        match buf.iter().rposition(|b| *b == b'\n') {
            Some(i) => {
                let rest = buf.split_off(i + 1);
                emit(&buf);
                buf = rest;
            }
            None if buf.len() >= 65536 => {
                emit(&buf);
                buf.clear();
            }
            None => {}
        }
    }
    if !buf.is_empty() {
        emit(&buf);
    }
}

fn pipe_cloexec() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn alive(c: &mut Child) -> bool {
    matches!(c.try_wait(), Ok(None))
}

/// Wait up to `secs` for the child to exit (only used while stopping).
fn wait_timeout(c: &mut Child, secs: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        match c.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => {}
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn random_hex(n: usize) -> String {
    (0..n).map(|_| format!("{:02x}", fastrand::u8(..))).collect()
}

impl Default for Player {
    fn default() -> Self {
        Player::new(None)
    }
}

impl Player {
    pub fn new(on_end: Option<EndCallback>) -> Player {
        Player {
            inner: Arc::new(Inner {
                start_lock: Mutex::new(()),
                state: Mutex::new(State::default()),
                generation: AtomicU64::new(0),
                flags: Mutex::new(Flags::default()),
                cv: Condvar::new(),
                last_error: Mutex::new(String::new()),
                on_end: Mutex::new(on_end),
                binary: Mutex::new("mpv".into()),
                log_path: Mutex::new(None),
            }),
        }
    }

    pub fn set_on_end(&self, cb: Option<EndCallback>) {
        *lock(&self.inner.on_end) = cb;
    }

    pub fn set_binary(&self, path: impl Into<String>) {
        *lock(&self.inner.binary) = path.into();
    }

    pub fn set_log_path(&self, path: impl Into<PathBuf>) {
        *lock(&self.inner.log_path) = Some(path.into());
    }

    fn mpv_log(&self) -> PathBuf {
        lock(&self.inner.log_path)
            .clone()
            .unwrap_or_else(crate::log::mpv_log_path)
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub fn last_error(&self) -> String {
        lock(&self.inner.last_error).clone()
    }

    pub fn is_ready(&self) -> bool {
        lock(&self.inner.flags).ready
    }

    pub fn pid(&self) -> Option<u32> {
        lock(&self.inner.state).proc.as_ref().map(|c| c.id())
    }

    pub fn socket_path(&self) -> Option<PathBuf> {
        lock(&self.inner.state).sock.clone()
    }

    fn set_flag(&self, ready: bool, failed: bool) {
        let mut f = lock(&self.inner.flags);
        f.ready |= ready;
        f.failed |= failed;
        self.inner.cv.notify_all();
    }

    fn set_error(&self, e: impl Into<String>) {
        *lock(&self.inner.last_error) = e.into();
    }

    pub fn start(&self, url: &str, opts: &StartOptions) -> Result<()> {
        let _g = lock(&self.inner.start_lock);
        self.start_locked(url, opts)
    }

    fn start_locked(&self, url: &str, opts: &StartOptions) -> Result<()> {
        self.stop_locked();
        let gen = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.inner.flags) = Flags::default();
        self.set_error("");
        // Per-generation socket in the private 0700 runtime directory.
        let sock = crate::paths::private_runtime_dir().join(format!(
            "mpv-{}-{}.sock",
            std::process::id(),
            random_hex(5)
        ));
        let mpvlog = self.mpv_log();
        let fail = |msg: String| {
            Error::new(
                ErrorKind::Playback,
                format!("{} · see {}", redact_text(&msg), mpvlog.display()),
            )
        };
        let (log_r, log_w) = pipe_cloexec().map_err(|e| fail(format!("mpv log pipe: {e}")))?;
        let log_fd = log_w.as_raw_fd();
        let args = mpv_args(&sock, log_fd, opts);
        let bin = lock(&self.inner.binary).clone();
        info!("mpv start source={}", trunc(&redact_url(url), 240));
        let mut cmd = Command::new(&bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                // start_new_session=True, and pass the log pipe's write end.
                libc::setsid();
                let fl = libc::fcntl(log_fd, libc::F_GETFD);
                if fl >= 0 {
                    libc::fcntl(log_fd, libc::F_SETFD, fl & !libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
        let spawned = cmd.spawn();
        drop(log_w);
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::NotFound {
                    "mpv is not installed".to_string()
                } else {
                    format!("mpv could not start: {e}")
                };
                error!("mpv playback failed gen={gen}: {msg}");
                return Err(fail(msg));
            }
        };
        let logfile = std::fs::File::from(log_r);
        let lp = mpvlog.clone();
        let _ = std::thread::Builder::new()
            .name("nplay-mpv-log".into())
            .spawn(move || copy_redacted_log(logfile, lp));
        let pid = child.id();
        info!(
            "mpv spawned pid={pid} generation={gen} socket={}",
            sock.display()
        );
        // Wait (up to 5 s) for the IPC socket to accept connections.
        let mut evsock = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && alive(&mut child) {
            if sock.exists() {
                if let Ok(s) = UnixStream::connect(&sock) {
                    evsock = Some(s);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        {
            let mut st = lock(&self.inner.state);
            st.proc = Some(child);
            st.sock = Some(sock.clone());
            st.evsock = evsock.as_ref().and_then(|s| s.try_clone().ok());
        }
        match evsock {
            Some(s) => {
                let me = self.clone();
                let _ = std::thread::Builder::new()
                    .name("nplay-mpv-events".into())
                    .spawn(move || me.events(gen, s));
                let r = ipc(&sock, &[json!("loadfile"), json!(url), json!("replace")]);
                if !success(&r) {
                    let detail = r
                        .as_ref()
                        .and_then(|v| v.get("error"))
                        .map(|e| format!(" ({})", crate::model::value_str(e)))
                        .unwrap_or_default();
                    self.set_error(format!("mpv rejected the media{detail}"));
                    self.set_flag(false, true);
                }
            }
            None => {
                if self.last_error().is_empty() {
                    self.set_error("mpv IPC socket did not appear");
                }
                self.set_flag(false, true);
            }
        }
        let deadline = Instant::now() + Duration::from_secs_f64(opts.timeout.max(2.0));
        let mut next_probe = Instant::now() + Duration::from_millis(150);
        let mut last_status = None;
        while Instant::now() < deadline {
            {
                let f = lock(&self.inner.flags);
                let (f, _) = self
                    .inner
                    .cv
                    .wait_timeout_while(f, Duration::from_millis(50), |f| !f.ready && !f.failed)
                    .unwrap_or_else(|p| p.into_inner());
                if f.ready {
                    return Ok(());
                }
                if f.failed {
                    break;
                }
            }
            if !self.proc_alive() {
                break;
            }
            // file-loaded is edge-triggered; reconcile if the event was missed.
            // core-idle may stay true while paused, so it is not required.
            if Instant::now() >= next_probe {
                next_probe = Instant::now() + Duration::from_millis(250);
                if let Some(status) = playback_status(&sock) {
                    if status.0.as_str() == Some(url) && status.1 == Value::Bool(false) {
                        info!(
                            "mpv ready via IPC state pid={pid} gen={gen} idle={} core_idle={}",
                            status.1, status.2
                        );
                        self.set_flag(true, false);
                        return Ok(());
                    }
                    last_status = Some(status);
                }
            }
        }
        if let Some(s) = &last_status {
            warn!(
                "mpv readiness timeout pid={pid} gen={gen} expected={} status={}",
                redact_url(url),
                redact_text(&format!("{s:?}"))
            );
        }
        let mut err = self.last_error();
        if err.is_empty() {
            err = if self.proc_alive() {
                "Timed out waiting for mpv to load media".into()
            } else {
                "mpv exited before playback started".into()
            };
        }
        let err = redact_text(&err);
        error!("mpv playback failed pid={pid} gen={gen}: {err}");
        self.stop_locked();
        Err(fail(err))
    }

    fn proc_alive(&self) -> bool {
        lock(&self.inner.state).proc.as_mut().is_some_and(alive)
    }

    /// Event reader: blocks on the socket; mpv's exit or `stop()` (socket
    /// shutdown) ends it.
    fn events(&self, gen: u64, s: UnixStream) {
        let current = || self.inner.generation.load(Ordering::SeqCst) == gen;
        let mut r = BufReader::new(s);
        let mut line = Vec::new();
        while current() {
            line.clear();
            match r.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if current() && !self.is_ready() {
                        self.set_error(format!("mpv IPC: {}", redact_text(&e.to_string())));
                        self.set_flag(false, true);
                        error!("mpv IPC failure: {}", e);
                    }
                    break;
                }
            }
            let Ok(m) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            match m.get("event").and_then(Value::as_str) {
                Some("file-loaded") if current() => self.set_flag(true, false),
                Some("end-file") => {
                    let reason = m.get("reason").and_then(Value::as_str).unwrap_or("");
                    if reason == "error" && current() {
                        let err = ["file_error", "error"]
                            .iter()
                            .filter_map(|k| m.get(*k))
                            .map(crate::model::value_str)
                            .find(|s| !s.is_empty())
                            .unwrap_or_else(|| reason.to_string());
                        let msg = format!("mpv: {}", redact_text(&err));
                        error!("{msg}");
                        self.set_error(msg);
                        self.set_flag(false, true);
                        return;
                    }
                    if reason == "eof" && current() {
                        let cb = lock(&self.inner.on_end).clone();
                        if let Some(cb) = cb {
                            cb();
                        }
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn cmd(&self, args: &[Value]) -> Option<Value> {
        let sock = self.socket_path()?;
        let r = ipc(&sock, args);
        if success(&r) {
            Some(r?.get("data").cloned().unwrap_or(Value::Null))
        } else {
            None
        }
    }

    pub fn prop(&self, name: &str) -> Option<Value> {
        self.cmd(&[json!("get_property"), json!(name)])
    }

    pub fn prop_f64(&self, name: &str) -> Option<f64> {
        self.prop(name).and_then(|v| v.as_f64())
    }

    pub fn prop_bool(&self, name: &str) -> Option<bool> {
        self.prop(name).and_then(|v| v.as_bool())
    }

    pub fn active(&self) -> bool {
        {
            let mut st = lock(&self.inner.state);
            let ok = st.proc.as_mut().is_some_and(alive)
                && st.sock.as_ref().is_some_and(|s| s.exists());
            if !ok {
                return false;
            }
        }
        self.prop("idle-active") != Some(Value::Bool(true))
    }

    pub fn toggle(&self) {
        self.cmd(&[json!("cycle"), json!("pause")]);
    }

    pub fn set_pause(&self, paused: bool) {
        self.cmd(&[json!("set_property"), json!("pause"), json!(paused)]);
    }

    pub fn set_volume(&self, v: f64) {
        let v = if v.is_finite() { v.clamp(0.0, 150.0) } else { 0.0 };
        self.cmd(&[json!("set_property"), json!("volume"), json!(v)]);
    }

    pub fn set_position(&self, v: f64) {
        let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
        self.cmd(&[json!("set_property"), json!("time-pos"), json!(v)]);
    }

    pub fn seek(&self, d: f64) {
        self.cmd(&[json!("seek"), json!(d), json!("relative")]);
    }

    pub fn volume(&self, d: f64) {
        self.cmd(&[json!("add"), json!("volume"), json!(d)]);
    }

    pub fn mute(&self) {
        self.cmd(&[json!("cycle"), json!("mute")]);
    }

    pub fn position(&self) -> Option<f64> {
        self.prop_f64("time-pos")
    }

    pub fn duration(&self) -> Option<f64> {
        self.prop_f64("duration")
    }

    pub fn stop(&self) {
        let _g = lock(&self.inner.start_lock);
        self.stop_locked();
    }

    fn stop_locked(&self) {
        // A terminated child is reaped before a replacement is spawned.
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        *lock(&self.inner.flags) = Flags::default();
        let (proc, sock, ev) = {
            let mut st = lock(&self.inner.state);
            (st.proc.take(), st.sock.take(), st.evsock.take())
        };
        if let Some(ev) = ev {
            let _ = ev.shutdown(std::net::Shutdown::Both);
        }
        if let Some(mut p) = proc {
            let pid = p.id();
            if alive(&mut p) {
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if !wait_timeout(&mut p, 1.5) {
                    warn!("mpv pid={pid} did not terminate; killing");
                    let _ = p.kill();
                    if !wait_timeout(&mut p, 2.0) {
                        error!("mpv pid={pid} could not be reaped");
                    }
                }
            }
            let rc = p
                .try_wait()
                .ok()
                .flatten()
                .map(|s| {
                    use std::os::unix::process::ExitStatusExt;
                    s.code().or(s.signal().map(|g| -g)).unwrap_or(0).to_string()
                })
                .unwrap_or_else(|| "None".into());
            info!("mpv stopped pid={pid} returncode={rc}");
        }
        if let Some(s) = sock {
            let _ = std::fs::remove_file(s);
        }
    }

    #[cfg(test)]
    fn adopt(&self, child: Child, sock: PathBuf) {
        let mut st = lock(&self.inner.state);
        st.proc = Some(child);
        st.sock = Some(sock);
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "nplay-player-{name}-{}-{:x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// tools/fake-mpv relative to the crate, when python3 is available.
    fn fake_mpv() -> Option<String> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/fake-mpv");
        (p.exists() && crate::paths::which("python3").is_some())
            .then(|| p.to_string_lossy().into_owned())
    }

    #[test]
    fn argv_has_options_but_never_the_url() {
        let sock = crate::paths::private_runtime_dir().join("mpv-1-abc.sock");
        let opts = StartOptions {
            replaygain: "album".into(),
            preamp: -3.5,
            clip: false,
            gapless: true,
            ytdl_path: Some("/usr/bin/yt-dlp".into()),
            volume_max: Some(150.0),
            ..Default::default()
        };
        let a = mpv_args(&sock, 7, &opts);
        assert!(a.contains(&"--idle=yes".to_string()));
        assert!(a.contains(&"--pause=yes".to_string()));
        assert!(a.contains(&"--log-file=/dev/fd/7".to_string()));
        assert!(a.contains(&"--replaygain=album".to_string()));
        assert!(a.contains(&"--replaygain-preamp=-3.5".to_string()));
        assert!(a.contains(&"--replaygain-clip=no".to_string()));
        assert!(a.contains(&"--gapless-audio=yes".to_string()));
        assert!(a.contains(&"--volume=100".to_string()));
        assert!(a.contains(&"--volume-max=150".to_string()));
        assert!(a.contains(&"--script-opts=ytdl_hook-ytdl_path=/usr/bin/yt-dlp".to_string()));
        let d = mpv_args(&sock, 3, &StartOptions::default());
        assert!(d.contains(&"--replaygain-preamp=0".to_string()));
        assert!(d.contains(&"--replaygain-clip=yes".to_string()));
        assert!(d.contains(&"--gapless-audio=no".to_string()));
        assert!(!d.iter().any(|x| x.starts_with("--volume-max")));
        assert!(!d.iter().any(|x| x.starts_with("--script-opts")));
    }

    #[test]
    fn socket_dir_is_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = crate::paths::private_runtime_dir();
        let m = std::fs::metadata(&d).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o700);
        assert_eq!(m.uid(), unsafe { libc::getuid() });
        assert!(!d.starts_with("/tmp/nplay-mpv-"));
    }

    #[test]
    fn normalization_rules() {
        assert_eq!(normalization_mode(false, "album", "A", &["A", "A"]), "no");
        assert_eq!(normalization_mode(true, "Track", "A", &["A", "A"]), "track");
        assert_eq!(normalization_mode(true, "album", "", &[]), "album");
        assert_eq!(normalization_mode(true, "auto", "A", &["A", "A", ""]), "album");
        assert_eq!(normalization_mode(true, "auto", "A", &["A"]), "track");
        assert_eq!(normalization_mode(true, "auto", "A", &["A", "B"]), "track");
        assert_eq!(normalization_mode(true, "auto", "", &["A", "A"]), "track");
    }

    #[test]
    fn status_matches_loaded_media() {
        let d = tmpdir("status");
        let sock = d.join("mpv.sock");
        let server = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            for (expected, value) in [
                ("path", json!("/tmp/song.mp3")),
                ("idle-active", json!(false)),
                ("core-idle", json!(false)),
            ] {
                let (c, _) = server.accept().unwrap();
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                let cmd: Value = serde_json::from_str(&l).unwrap();
                assert_eq!(cmd["command"], json!(["get_property", expected]));
                let mut c = c;
                // An unrelated broadcast event first: must be skipped.
                c.write_all(b"{\"event\":\"audio-reconfig\"}\n").unwrap();
                c.write_all(
                    format!("{}\n", json!({"error":"success","data":value})).as_bytes(),
                )
                .unwrap();
            }
        });
        assert_eq!(
            playback_status(&sock),
            Some((json!("/tmp/song.mp3"), json!(false), json!(false)))
        );
        h.join().unwrap();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn status_missing_socket() {
        assert_eq!(
            playback_status(Path::new("/tmp/nplay-test-nonexistent-socket")),
            None
        );
    }

    #[test]
    fn stop_reaps_and_removes_only_own_socket() {
        let d = tmpdir("stop");
        let sock = d.join("owned.sock");
        let other = d.join("other.sock");
        std::fs::write(&sock, b"").unwrap();
        std::fs::write(&other, b"").unwrap();
        let p = Player::new(None);
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        p.adopt(child, sock.clone());
        p.stop();
        assert!(!sock.exists());
        assert!(other.exists());
        assert!(p.pid().is_none());
        // Reaped: no zombie left behind.
        assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn stuck_child_is_killed_and_reaped() {
        let p = Player::new(None);
        let child = Command::new("sh")
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(100));
        p.adopt(child, tmpdir("stuck").join("x.sock"));
        let t = Instant::now();
        p.stop();
        assert!(t.elapsed() >= Duration::from_millis(1400));
        assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
    }

    #[test]
    fn missing_binary_is_an_error() {
        let p = Player::new(None);
        p.set_binary("/nonexistent/mpv-binary");
        p.set_log_path(tmpdir("missing").join("mpv.log"));
        let e = p.start("/tmp/x.mp3", &StartOptions::default()).unwrap_err();
        assert!(e.message.contains("mpv is not installed"), "{}", e.message);
        assert!(e.message.contains(" · see "));
    }

    #[test]
    fn fake_mpv_lifecycle_and_redacted_log() {
        let Some(fake) = fake_mpv() else {
            eprintln!("skipping: fake-mpv or python3 missing");
            return;
        };
        let d = tmpdir("fake");
        let wav = d.join("tone.wav");
        write_wav(&wav, 1.0);
        let ended = Arc::new((Mutex::new(0u32), Condvar::new()));
        let e2 = ended.clone();
        let p = Player::new(Some(Arc::new(move || {
            *e2.0.lock().unwrap() += 1;
            e2.1.notify_all();
        })));
        p.set_binary(fake);
        let log = d.join("mpv.log");
        p.set_log_path(&log);
        let url = "https://music.example/rest/stream.view?u=me&t=0123456789abcdef&s=a1b2c3&v=1.16.1&c=nplay&id=42";
        p.start(url, &StartOptions::default()).unwrap();
        assert!(p.is_ready());
        let pid = p.pid().unwrap();
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        let cmdline = String::from_utf8_lossy(&cmdline);
        assert!(!cmdline.contains("0123456789abcdef"));
        assert!(!cmdline.contains("stream.view"));
        assert!(p.active());
        assert_eq!(p.prop("path"), Some(json!(url)));
        p.set_volume(200.0);
        assert_eq!(p.prop_f64("volume"), Some(150.0));
        p.volume(-10.0);
        assert_eq!(p.prop_f64("volume"), Some(140.0));
        p.set_pause(false);
        assert_eq!(p.prop_bool("pause"), Some(false));
        p.toggle();
        assert_eq!(p.prop_bool("pause"), Some(true));
        p.mute();
        assert_eq!(p.prop_bool("mute"), Some(true));
        p.set_position(5.0);
        assert!(p.position().unwrap() >= 5.0);
        let sock = p.socket_path().unwrap();
        assert!(sock.starts_with(crate::paths::private_runtime_dir()));
        p.stop();
        assert!(!sock.exists());
        assert!(!p.active());
        assert_eq!(p.prop("path"), None);
        // The log reached disk redacted, 0600.
        std::thread::sleep(Duration::from_millis(200));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("loadfile"), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Restart with a local file works and a second start replaces the first.
        p.start(wav.to_str().unwrap(), &StartOptions::default())
            .unwrap();
        let first = p.pid().unwrap();
        p.start(wav.to_str().unwrap(), &StartOptions::default())
            .unwrap();
        assert_ne!(p.pid().unwrap(), first);
        assert_ne!(unsafe { libc::kill(first as i32, 0) }, 0);
        p.stop();
        assert_eq!(*ended.0.lock().unwrap(), 0);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Real mpv smoke test: a short generated WAV plays to the end and fires
    /// on_end. Skipped when mpv is not installed.
    #[test]
    fn real_mpv_plays_to_end() {
        if crate::paths::which("mpv").is_none() {
            eprintln!("skipping: mpv missing");
            return;
        }
        let d = tmpdir("real");
        let wav = d.join("tone.wav");
        write_wav(&wav, 0.6);
        let ended = Arc::new((Mutex::new(0u32), Condvar::new()));
        let e2 = ended.clone();
        let p = Player::new(Some(Arc::new(move || {
            *e2.0.lock().unwrap() += 1;
            e2.1.notify_all();
        })));
        p.set_log_path(d.join("mpv.log"));
        let opts = StartOptions {
            extra_args: vec!["--ao=null".into()],
            ..Default::default()
        };
        p.start(wav.to_str().unwrap(), &opts).unwrap();
        assert!(p.duration().unwrap_or(0.0) > 0.5);
        p.set_pause(false);
        let g = ended.0.lock().unwrap();
        let (g, _) = ended
            .1
            .wait_timeout_while(g, Duration::from_secs(10), |n| *n == 0)
            .unwrap();
        assert_eq!(*g, 1, "on_end must fire at end-of-file");
        drop(g);
        p.stop();
        // Bad media is a clean, redacted error.
        let e = p
            .start(d.join("missing.wav").to_str().unwrap(), &opts)
            .unwrap_err();
        assert!(e.message.contains("see"), "{}", e.message);
        assert!(p.pid().is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    fn write_wav(path: &Path, secs: f64) {
        let rate = 8000u32;
        let n = (rate as f64 * secs) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            let v = ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 8000.0)
                as i16;
            b.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }
}
