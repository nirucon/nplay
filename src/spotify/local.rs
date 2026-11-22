//! The NPLAY-managed local librespot receiver (port of 1.x
//! `LocalSpotifyEngine` and its helpers, lifecycle rules of 1.8.1).
//!
//! Lifecycle rules:
//! - A receiver NPLAY spawns dies with the NPLAY process that spawned it:
//!   it runs in its own session/process group and gets
//!   `PR_SET_PDEATHSIG=SIGKILL` in `pre_exec`. The death signal follows the
//!   *thread* that forked, so every spawn runs on one long-lived spawner
//!   thread (`nplay-librespot-spawner`, created on first use, blocked on a
//!   channel while idle), never on a short-lived worker.
//! - One receiver per cache directory: before spawning, a flock on
//!   `<private_runtime_dir>/librespot-<sha1(realpath(cache))[:12]>.lock` is
//!   taken and `/proc` is scanned for same-user librespot processes whose
//!   argv has the same `--cache` (verified in `/proc/<pid>/cmdline`). A
//!   receiver whose parent is another live NPLAY is adopted (shared, never
//!   killed on close); any other one (an orphan) is terminated. An OAuth
//!   start replaces every receiver. `librespot-<hash>.json` (0600) records
//!   `{pid, owner, cache, started}`.
//! - Without cached `credentials.json` a normal start fails at once with the
//!   not-authorized error (`super::NOT_AUTHORIZED_MESSAGE`,
//!   `ErrorKind::Authentication`; test with `super::is_not_authorized`).
//! - `close()` stops the owned receiver (SIGTERM, SIGKILL after 3 s) and
//!   refuses later starts. A C `atexit` hook closes engines left open.
//!
//! Public API (`LocalSpotifyEngine`, `Clone + Send + Sync`, `&self`):
//! `new(&Config)`, `with_options(EngineOptions)`, `binary()`, `available()`,
//! `running()`, `owned()`, `pid()`, `credential_files()`,
//! `credentials_path()`, `has_credentials()`, `authorized()`, `version()`,
//! `lock_path()`, `pid_path()`, `stale_receivers()`, `start(oauth)`,
//! `stop(release_only)`, `close()`, `restart()`,
//! `authorize(timeout, opener, on_url)`, `log_tail(n)`, and the paths
//! `root()`, `cache()`, `system_cache()`, `log_path()`.
//! Free functions: `find_receivers(cache, exclude)`, `alive(pid)`,
//! `terminate_pid(pid, timeout)`, `no_open_dir(runtime)`,
//! `oauth_url_from_log(path, offset)`.

use super::{not_authorized, sha1};
use crate::config::Config;
use crate::core::{Error, Result};
use crate::paths;
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Settings lookup `(key, default) -> value` (config.ini `[general]`).
pub type Settings = Arc<dyn Fn(&str, &str) -> String + Send + Sync>;

pub const AUTH_TIMEOUT: f64 = 180.0;
const NO_OPEN_TOOLS: &[&str] = &[
    "xdg-open",
    "gio",
    "gnome-open",
    "kde-open",
    "wslview",
    "open",
];

// --- /proc helpers -----------------------------------------------------------

fn my_pid() -> i32 {
    std::process::id() as i32
}

fn my_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub(crate) fn read_cmdline(pid: i32) -> Vec<String> {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(raw) => raw
            .split(|b| *b == 0)
            .filter(|x| !x.is_empty())
            .map(|x| String::from_utf8_lossy(x).into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn proc_uid(pid: i32) -> Option<u32> {
    std::fs::metadata(format!("/proc/{pid}"))
        .ok()
        .map(|m| m.uid())
}

/// `(state, ppid)` from `/proc/<pid>/stat`.
pub(crate) fn proc_stat(pid: i32) -> Option<(String, i32)> {
    if pid <= 0 {
        return None;
    }
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = raw.rsplit_once(')')?.1;
    let mut it = rest.split_whitespace();
    let state = it.next()?.to_string();
    let ppid = it.next()?.parse().ok()?;
    Some((state, ppid))
}

/// A live (not zombie/dead) process.
pub fn alive(pid: i32) -> bool {
    matches!(proc_stat(pid), Some((s, _)) if s != "Z" && s != "X")
}

fn basename(a: &str) -> &str {
    a.rsplit('/').next().unwrap_or(a)
}

fn is_librespot_argv(argv: &[String]) -> bool {
    // Native binary: argv[0]; script wrappers: interpreter + script path.
    argv.iter()
        .take(3)
        .any(|a| basename(a).starts_with("librespot"))
}

fn argv_cache(argv: &[String]) -> Option<String> {
    for (i, a) in argv.iter().enumerate() {
        if (a == "--cache" || a == "-c") && i + 1 < argv.len() {
            return Some(argv[i + 1].clone());
        }
        if let Some(v) = a.strip_prefix("--cache=") {
            return Some(v.to_string());
        }
    }
    None
}

/// `os.path.realpath`: resolves what exists, keeps the rest.
pub(crate) fn realpath(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => realpath(parent).join(name),
        _ => abs,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    !a.as_os_str().is_empty() && realpath(a) == realpath(b)
}

/// PIDs of live librespot processes of this user that use this `--cache`.
pub fn find_receivers(cache: &Path, exclude: &[i32]) -> Vec<i32> {
    let mut out = Vec::new();
    let uid = my_uid();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        if exclude.contains(&pid) || pid == my_pid() || proc_uid(pid) != Some(uid) {
            continue;
        }
        let argv = read_cmdline(pid);
        if argv.is_empty() || !is_librespot_argv(&argv) {
            continue;
        }
        if let Some(c) = argv_cache(&argv) {
            if same_path(Path::new(&c), cache) && alive(pid) {
                out.push(pid);
            }
        }
    }
    out.sort_unstable();
    out
}

/// A live NPLAY (terminal or service) process of this user.
fn is_nplay_process(pid: i32) -> bool {
    if pid <= 1 || proc_uid(pid) != Some(my_uid()) || !alive(pid) {
        return false;
    }
    if pid == my_pid() {
        return true;
    }
    read_cmdline(pid)
        .iter()
        .any(|a| basename(a).contains("nplay") || a.starts_with("nplay.") || a.contains("/nplay/"))
}

fn send_sig(pid: i32, sig: i32) {
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::killpg(pid, sig);
        } else {
            libc::kill(pid, sig);
        }
    }
}

/// SIGTERM a receiver (its process group if it leads one), then SIGKILL
/// after `timeout` seconds. True when it is gone.
pub fn terminate_pid(pid: i32, timeout: f64) -> bool {
    if pid <= 1 {
        return false;
    }
    if !alive(pid) {
        return true;
    }
    send_sig(pid, libc::SIGTERM);
    for (limit, sig) in [(timeout, Some(libc::SIGKILL)), (2.0, None)] {
        let deadline = Instant::now() + Duration::from_secs_f64(limit.max(0.0));
        while Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            // Reap if it happens to be our child.
            unsafe {
                libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(s) = sig {
            send_sig(pid, s);
        }
    }
    !alive(pid)
}

/// The consent URL librespot prints ("Browse to: https://…") after `offset`.
pub fn oauth_url_from_log(path: &Path, offset: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(256 * 1024).read_to_end(&mut buf);
    let data = String::from_utf8_lossy(&buf);
    for line in data.lines() {
        if let Some((_, rest)) = line.split_once("Browse to:") {
            let url = rest.trim();
            if url.starts_with("https://") {
                return url.to_string();
            }
        }
    }
    String::new()
}

/// A private PATH prefix whose browser openers do nothing: NPLAY opens the
/// consent page itself, so exactly one tab opens.
pub fn no_open_dir(runtime: &Path) -> std::io::Result<PathBuf> {
    let d = runtime.join("librespot-noopen");
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&d)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    for tool in NO_OPEN_TOOLS {
        let p = d.join(tool);
        if !p.exists() {
            // Written under a temporary name and renamed: the executable is
            // never open for writing (no ETXTBSY when a fork races).
            let tmp = d.join(format!(".{tool}.tmp"));
            std::fs::write(
                &tmp,
                "#!/bin/sh\n# NPLAY: the browser is opened by NPLAY, not by librespot.\nexit 0\n",
            )?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
            std::fs::rename(&tmp, &p)?;
        }
    }
    Ok(d)
}

// --- spawner thread ----------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;
static SPAWNER: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

fn start_spawner() -> Option<mpsc::Sender<Job>> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("nplay-librespot-spawner".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
        })
        .ok()?;
    Some(tx)
}

/// Run `f` on the one long-lived spawner thread and return its result.
fn spawner_run<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    let (tx, rx) = mpsc::channel();
    let mut job: Job = Box::new(move || {
        let _ = tx.send(f());
    });
    for _ in 0..2 {
        let sender = {
            let mut g = SPAWNER.lock().unwrap_or_else(|p| p.into_inner());
            if g.is_none() {
                *g = start_spawner();
            }
            g.clone()
        };
        let Some(sender) = sender else { break };
        match sender.send(job) {
            Ok(()) => {
                return rx
                    .recv()
                    .unwrap_or_else(|_| Err(std::io::Error::other("librespot spawner failed")));
            }
            Err(mpsc::SendError(j)) => {
                job = j;
                *SPAWNER.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
        }
    }
    Err(std::io::Error::other("librespot spawner unavailable"))
}

// --- atexit -----------------------------------------------------------------

static ENGINES: Mutex<Vec<Weak<Inner>>> = Mutex::new(Vec::new());
static ATEXIT: std::sync::Once = std::sync::Once::new();

extern "C" fn close_all_engines() {
    let list: Vec<Weak<Inner>> = match ENGINES.try_lock() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    for w in list {
        let Some(inner) = w.upgrade() else { continue };
        let guard = inner.state.try_lock();
        match guard {
            Ok(mut st) => {
                st.closed = true;
                st.adopted = None;
                inner.stop_owned_locked(&mut st);
            }
            Err(_) => {
                let pid = inner.owned_pid.load(Ordering::SeqCst);
                if pid > 1 {
                    unsafe {
                        libc::killpg(pid, libc::SIGTERM);
                    }
                }
            }
        }
    }
}

fn register(inner: &Arc<Inner>) {
    let mut g = ENGINES.lock().unwrap_or_else(|p| p.into_inner());
    g.retain(|w| w.strong_count() > 0);
    g.push(Arc::downgrade(inner));
    drop(g);
    ATEXIT.call_once(|| unsafe {
        libc::atexit(close_all_engines);
    });
}

// --- engine -------------------------------------------------------------------

/// Where an engine lives; `new(&Config)` fills it from the XDG paths.
#[derive(Clone)]
pub struct EngineOptions {
    pub settings: Settings,
    /// `$DATA/nplay` (the engine uses `<data>/spotify-local`).
    pub data_dir: PathBuf,
    /// `$STATE/nplay` (the engine logs to `<state>/librespot.log`).
    pub state_dir: PathBuf,
    /// Lock/pid/no-open directory; `None` = `paths::private_runtime_dir()`.
    pub runtime_dir: Option<PathBuf>,
    /// librespot binary; `None` = `which librespot`.
    pub binary: Option<PathBuf>,
    /// Extra environment for the receiver (tests).
    pub env: Vec<(String, String)>,
}

struct State {
    proc: Option<Child>,
    adopted: Option<i32>,
    closed: bool,
}

struct Inner {
    opts: EngineOptions,
    root: PathBuf,
    cache: PathBuf,
    system_cache: PathBuf,
    log_path: PathBuf,
    state: Mutex<State>,
    owned_pid: AtomicI32,
}

/// Lifecycle wrapper for the optional local librespot receiver.
#[derive(Clone)]
pub struct LocalSpotifyEngine {
    inner: Arc<Inner>,
}

impl LocalSpotifyEngine {
    pub fn new(cfg: &Config) -> LocalSpotifyEngine {
        let c = cfg.clone();
        LocalSpotifyEngine::with_options(EngineOptions {
            settings: Arc::new(move |k, d| c.get(k, d)),
            data_dir: paths::data(),
            state_dir: paths::state(),
            runtime_dir: None,
            binary: None,
            env: Vec::new(),
        })
    }

    pub fn with_options(opts: EngineOptions) -> LocalSpotifyEngine {
        let root = opts.data_dir.join("spotify-local");
        let cache = root.join("cache");
        let system_cache = root.join("system-cache");
        let log_path = opts.state_dir.join("librespot.log");
        for d in [&root, &cache, &system_cache] {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        let inner = Arc::new(Inner {
            opts,
            root,
            cache,
            system_cache,
            log_path,
            state: Mutex::new(State {
                proc: None,
                adopted: None,
                closed: false,
            }),
            owned_pid: AtomicI32::new(0),
        });
        register(&inner);
        LocalSpotifyEngine { inner }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }
    pub fn cache(&self) -> &Path {
        &self.inner.cache
    }
    pub fn system_cache(&self) -> &Path {
        &self.inner.system_cache
    }
    pub fn log_path(&self) -> &Path {
        &self.inner.log_path
    }

    pub fn binary(&self) -> Option<PathBuf> {
        match &self.inner.opts.binary {
            Some(b) => Some(b.clone()),
            None => paths::which("librespot"),
        }
    }

    pub fn available(&self) -> bool {
        self.binary().is_some()
    }

    pub fn running(&self) -> bool {
        let mut st = self.lock();
        self.inner.running_locked(&mut st)
    }

    pub fn owned(&self) -> bool {
        let mut st = self.lock();
        Inner::owned_locked(&mut st)
    }

    pub fn pid(&self) -> Option<i32> {
        let mut st = self.lock();
        if Inner::owned_locked(&mut st) {
            return st.proc.as_ref().map(|p| p.id() as i32);
        }
        if self.inner.adopted_alive(&mut st) {
            st.adopted
        } else {
            None
        }
    }

    /// Non-empty files in the system cache and cache (diagnostics).
    pub fn credential_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for base in [&self.inner.system_cache, &self.inner.cache] {
            walk(base, &mut out, 0);
        }
        out
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.inner.credentials_path()
    }

    /// True when librespot has reusable credentials (credentials.json)
    /// cached: in `--system-cache`, or in `--cache` (older NPLAY builds).
    pub fn has_credentials(&self) -> bool {
        self.inner.has_credentials()
    }

    pub fn authorized(&self) -> bool {
        self.has_credentials()
    }

    pub fn version(&self) -> String {
        let Some(b) = self.binary() else {
            return String::new();
        };
        super::browser::run_capture(&b, &["--version"], Duration::from_secs(2))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    pub fn lock_path(&self) -> PathBuf {
        self.inner.lock_path()
    }

    pub fn pid_path(&self) -> PathBuf {
        self.inner.pid_path()
    }

    /// Receivers for this cache that no live NPLAY owns (doctor).
    pub fn stale_receivers(&self) -> Vec<i32> {
        let own = {
            let mut st = self.lock();
            if Inner::owned_locked(&mut st) {
                st.proc.as_ref().map(|p| p.id() as i32)
            } else {
                None
            }
        };
        find_receivers(&self.inner.cache, &own.into_iter().collect::<Vec<_>>())
            .into_iter()
            .filter(|pid| {
                !proc_stat(*pid)
                    .map(|(_, pp)| is_nplay_process(pp))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Start (or adopt) the receiver. `oauth` replaces every receiver and
    /// runs librespot's OAuth flow.
    pub fn start(&self, oauth: bool) -> Result<bool> {
        let mut st = self.lock();
        self.inner.start_locked(&mut st, oauth)
    }

    /// Stop the receiver this engine uses. An owned receiver is terminated;
    /// an adopted one too unless `release_only`, which just forgets it.
    pub fn stop(&self, release_only: bool) {
        let mut st = self.lock();
        self.inner.stop_locked(&mut st, release_only);
    }

    /// Final shutdown: stop the owned receiver and refuse later starts.
    pub fn close(&self) {
        let mut st = self.lock();
        st.closed = true;
        self.inner.stop_locked(&mut st, true);
    }

    /// Restart the local receiver once; preserve OAuth caches.
    pub fn restart(&self) -> Result<bool> {
        let mut st = self.lock();
        self.inner.stop_locked(&mut st, false);
        self.inner.start_locked(&mut st, false)
    }

    /// Run librespot's OAuth flow; return once fresh credentials are cached.
    ///
    /// Any receiver for this cache is replaced. librespot ignores
    /// `--enable-oauth` while credentials.json exists, so an existing blob is
    /// moved aside and put back if the flow fails. The consent URL that
    /// librespot prints is opened with `opener` (default: NPLAY's browser
    /// candidates); `on_url` is told the URL first.
    pub fn authorize(
        &self,
        timeout: Option<f64>,
        opener: Option<&dyn Fn(&str) -> bool>,
        on_url: Option<&dyn Fn(&str)>,
    ) -> Result<bool> {
        let timeout = timeout.filter(|t| *t > 0.0).unwrap_or(AUTH_TIMEOUT);
        let creds = self.inner.credentials_path();
        let backup = creds.with_file_name("credentials.json.nplay-previous");
        let offset;
        {
            let mut st = self.lock();
            self.inner.stop_locked(&mut st, false);
            for old in [creds.clone(), self.inner.cache.join("credentials.json")] {
                if old.exists() {
                    let _ = std::fs::rename(&old, &backup);
                }
            }
            offset = std::fs::metadata(&self.inner.log_path)
                .map(|m| m.len())
                .unwrap_or(0);
            if let Err(e) = self.inner.start_locked(&mut st, true) {
                self.inner.restore_credentials(&backup);
                return Err(e);
            }
        }
        let res = self.authorize_wait(timeout, offset, &backup, opener, on_url);
        if res.is_err() {
            let mut st = self.lock();
            self.inner.stop_locked(&mut st, false);
            self.inner.restore_credentials(&backup);
        }
        res
    }

    fn authorize_wait(
        &self,
        timeout: f64,
        offset: u64,
        backup: &Path,
        opener: Option<&dyn Fn(&str) -> bool>,
        on_url: Option<&dyn Fn(&str)>,
    ) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let mut opened = false;
        while Instant::now() < deadline {
            if self.has_credentials() {
                let _ = std::fs::remove_file(backup);
                info!("librespot OAuth completed · credentials cached");
                return Ok(true);
            }
            if !self.running() {
                return Err(Error::new(
                    crate::core::ErrorKind::Playback,
                    "librespot exited during authorization · open Spotify → Local playback → Diagnostics",
                ));
            }
            if !opened {
                let url = oauth_url_from_log(&self.inner.log_path, offset);
                if !url.is_empty() {
                    opened = true;
                    if let Some(cb) = on_url {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&url)));
                    }
                    let ok = match opener {
                        Some(o) => {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o(&url)))
                                .unwrap_or(false)
                        }
                        None => super::browser::open_authorization_url(&url),
                    };
                    if !ok {
                        warn!(
                            "librespot OAuth URL could not be opened in a browser · see {}",
                            self.inner.log_path.display()
                        );
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(Error::new(
            crate::core::ErrorKind::Authentication,
            format!(
                "Local Spotify authorization timed out after {}s · retry and complete authorization in the browser",
                timeout as i64
            ),
        ))
    }

    pub fn log_tail(&self, n: usize) -> String {
        match std::fs::read(&self.inner.log_path) {
            Ok(b) => {
                let s = String::from_utf8_lossy(&b);
                let lines: Vec<&str> = s.lines().collect();
                lines[lines.len().saturating_sub(n)..].join("\n")
            }
            Err(_) => String::new(),
        }
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 10_000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(m) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk(&p, out, depth + 1);
        } else if std::fs::metadata(&p)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

impl Inner {
    fn setting(&self, k: &str, d: &str) -> String {
        let v = (self.opts.settings)(k, d);
        if v.is_empty() {
            d.to_string()
        } else {
            v
        }
    }

    fn credentials_path(&self) -> PathBuf {
        self.system_cache.join("credentials.json")
    }

    fn has_credentials(&self) -> bool {
        [self.credentials_path(), self.cache.join("credentials.json")]
            .iter()
            .any(|p| {
                std::fs::metadata(p)
                    .map(|m| m.is_file() && m.len() > 0)
                    .unwrap_or(false)
            })
    }

    fn runtime(&self) -> PathBuf {
        match &self.opts.runtime_dir {
            Some(d) => {
                let _ = std::fs::create_dir_all(d);
                d.clone()
            }
            None => paths::private_runtime_dir(),
        }
    }

    fn key(&self) -> String {
        let rp = realpath(&self.cache);
        sha1::sha1_hex(rp.as_os_str().as_encoded_bytes())[..12].to_string()
    }

    fn lock_path(&self) -> PathBuf {
        self.runtime()
            .join(format!("librespot-{}.lock", self.key()))
    }

    fn pid_path(&self) -> PathBuf {
        self.runtime()
            .join(format!("librespot-{}.json", self.key()))
    }

    fn flock(&self) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsRawFd;
        let p = self.lock_path();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&p)?;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        loop {
            let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if r == 0 {
                return Ok(f);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    fn owned_locked(st: &mut State) -> bool {
        match st.proc.as_mut() {
            Some(p) => matches!(p.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn adopted_alive(&self, st: &mut State) -> bool {
        let Some(pid) = st.adopted else { return false };
        let argv = read_cmdline(pid);
        if !argv.is_empty()
            && is_librespot_argv(&argv)
            && same_path(
                Path::new(&argv_cache(&argv).unwrap_or_default()),
                &self.cache,
            )
            && alive(pid)
        {
            return true;
        }
        st.adopted = None;
        false
    }

    fn running_locked(&self, st: &mut State) -> bool {
        Inner::owned_locked(st) || self.adopted_alive(st)
    }

    fn write_pidfile(&self, pid: i32) {
        let d = json!({
            "pid": pid,
            "owner": my_pid(),
            "cache": self.cache.to_string_lossy(),
            "started": paths::now(),
        });
        let target = self.pid_path();
        let tmp = target.with_extension("tmp");
        let res = std::fs::write(&tmp, d.to_string())
            .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)))
            .and_then(|_| std::fs::rename(&tmp, &target));
        if res.is_err() {
            debug!("librespot pid file not written");
        }
    }

    fn pidfile_pid(&self) -> Option<i32> {
        let raw = std::fs::read_to_string(self.pid_path()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let pid = crate::model::value_i64(v.get("pid")?) as i32;
        (pid != 0).then_some(pid)
    }

    fn clear_pidfile(&self, pid: i32) {
        if self.pidfile_pid() == Some(pid) {
            let _ = std::fs::remove_file(self.pid_path());
        }
    }

    /// Adopt a receiver another live NPLAY owns, or terminate stale ones.
    /// With `replace` every other receiver for this cache is terminated.
    fn reconcile_others_locked(&self, st: &mut State, replace: bool) -> Option<i32> {
        let own = if Inner::owned_locked(st) {
            st.proc.as_ref().map(|p| p.id() as i32)
        } else {
            None
        };
        let hint = self.pidfile_pid();
        let mut pids = find_receivers(&self.cache, &own.into_iter().collect::<Vec<_>>());
        if let Some(h) = hint {
            if let Some(i) = pids.iter().position(|p| *p == h) {
                pids.remove(i);
                pids.insert(0, h);
            }
        }
        let mut adopted = None;
        for pid in pids {
            let ppid = proc_stat(pid).map(|(_, p)| p).unwrap_or(0);
            if !replace && adopted.is_none() && is_nplay_process(ppid) && ppid != my_pid() {
                adopted = Some(pid);
                info!("librespot adopting receiver pid={pid} owned by NPLAY pid={ppid}");
                continue;
            }
            warn!(
                "librespot terminating {} receiver pid={} parent={} (same cache)",
                if adopted.is_some() {
                    "duplicate"
                } else {
                    "stale"
                },
                pid,
                ppid
            );
            terminate_pid(pid, 3.0);
        }
        adopted
    }

    fn start_locked(&self, st: &mut State, oauth: bool) -> Result<bool> {
        if st.closed {
            return Err(Error::msg(
                "NPLAY is shutting down · local Spotify engine not started",
            ));
        }
        if !oauth && self.running_locked(st) {
            return Ok(true);
        }
        let b = match &self.opts.binary {
            Some(b) => Some(b.clone()),
            None => paths::which("librespot"),
        };
        let Some(b) = b else {
            return Err(Error::new(
                crate::core::ErrorKind::Configuration,
                "librespot is not installed · install it for local playback, or use an external Spotify Connect device",
            ));
        };
        let _lock = self.flock()?;
        if oauth && Inner::owned_locked(st) {
            self.stop_owned_locked(st);
        }
        st.adopted = None;
        if let Some(pid) = self.reconcile_others_locked(st, oauth) {
            st.adopted = Some(pid);
            return Ok(true);
        }
        if !oauth && !self.has_credentials() {
            let n = {
                let mut v = Vec::new();
                walk(&self.system_cache, &mut v, 0);
                walk(&self.cache, &mut v, 0);
                v.len()
            };
            warn!("librespot not started · no cached credentials (cached_files={n}) · authorization required");
            return Err(not_authorized());
        }
        self.spawn_locked(st, &b, oauth)
    }

    fn spawn_locked(&self, st: &mut State, b: &Path, oauth: bool) -> Result<bool> {
        let bitrate = self.setting("spotify_local_bitrate", "320");
        let name = self.setting("spotify_local_name", "NPLAY");
        let backend = self.setting("spotify_local_backend", "pulseaudio");
        let vol_raw = self.setting("spotify_local_volume", "70");
        let volume = vol_raw
            .trim()
            .parse::<f64>()
            .map(|v| v as i64)
            .unwrap_or(70);
        let args: Vec<String> = vec![
            "--name".into(),
            name,
            "--device-type".into(),
            "computer".into(),
            "--bitrate".into(),
            bitrate,
            "--cache".into(),
            self.cache.to_string_lossy().into_owned(),
            "--system-cache".into(),
            self.system_cache.to_string_lossy().into_owned(),
            "--backend".into(),
            backend,
            "--initial-volume".into(),
            volume.to_string(),
            "--quiet".into(),
        ];
        let mut args = args;
        let mut env: Vec<(String, String)> = Vec::new();
        let mut remove_browser = false;
        for (k, v) in [
            ("PULSE_PROP_application.name", "NPLAY · Spotify"),
            ("PULSE_PROP_media.role", "music"),
        ] {
            if std::env::var_os(k).is_none() {
                env.push((k.into(), v.into()));
            }
        }
        // First run: librespot runs its OAuth flow and persists its
        // credential blob. Later runs reuse that blob without a browser.
        if oauth {
            info!("librespot OAuth explicitly requested by setup flow");
            args.push("--enable-oauth".into());
            match no_open_dir(&self.runtime()) {
                Ok(d) => {
                    let path = std::env::var("PATH").unwrap_or_default();
                    env.push(("PATH".into(), format!("{}:{}", d.display(), path)));
                    remove_browser = true;
                }
                Err(_) => warn!(
                    "librespot no-open PATH prefix unavailable; librespot may open its own browser tab"
                ),
            }
        } else {
            let mut v = Vec::new();
            walk(&self.system_cache, &mut v, 0);
            walk(&self.cache, &mut v, 0);
            info!(
                "librespot start without OAuth · credentials cached · cached_files={}",
                v.len()
            );
        }
        env.extend(self.opts.env.iter().cloned());
        let parent = my_pid();
        let log_path = self.log_path.clone();
        let bin = b.to_path_buf();
        let child = spawner_run(move || {
            if let Some(p) = log_path.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let log = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(&log_path)?;
            let _ = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600));
            let mut cmd = Command::new(&bin);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            if remove_browser {
                cmd.env_remove("BROWSER");
            }
            unsafe {
                cmd.pre_exec(move || {
                    libc::setsid();
                    libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0,
                        0,
                        0,
                    );
                    // The parent may have died between fork() and prctl().
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
            cmd.spawn()
        })?;
        let pid = child.id() as i32;
        st.proc = Some(child);
        self.owned_pid.store(pid, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(250));
        if let Some(p) = st.proc.as_mut() {
            if let Ok(Some(rc)) = p.try_wait() {
                error!(
                    "librespot exited during startup rc={} · log={}",
                    rc.code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    self.log_path.display()
                );
                st.proc = None;
                self.owned_pid.store(0, Ordering::SeqCst);
                return Err(Error::new(
                    crate::core::ErrorKind::Playback,
                    "librespot failed to start · open Spotify → Local playback → Diagnostics",
                ));
            }
        }
        self.write_pidfile(pid);
        info!(
            "librespot process started pid={pid} oauth={} log={}",
            if oauth { "True" } else { "False" },
            self.log_path.display()
        );
        Ok(true)
    }

    fn stop_owned_locked(&self, st: &mut State) {
        let Some(mut p) = st.proc.take() else { return };
        let pid = p.id() as i32;
        if matches!(p.try_wait(), Ok(None)) {
            unsafe {
                libc::killpg(pid, libc::SIGTERM);
            }
            if !wait_child(&mut p, 3.0) {
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
                if !wait_child(&mut p, 3.0) {
                    let _ = p.kill();
                    let _ = p.wait();
                }
            }
        } else {
            let _ = p.wait();
        }
        self.owned_pid.store(0, Ordering::SeqCst);
        self.clear_pidfile(pid);
    }

    fn stop_locked(&self, st: &mut State, release_only: bool) {
        let adopted = st.adopted.take();
        if let Some(pid) = adopted {
            if !release_only && alive(pid) {
                info!("librespot stopping adopted receiver pid={pid}");
                terminate_pid(pid, 3.0);
            }
        }
        self.stop_owned_locked(st);
    }

    fn restore_credentials(&self, backup: &Path) {
        if backup.exists()
            && !self.credentials_path().exists()
            && std::fs::rename(backup, self.credentials_path()).is_ok()
        {
            info!("librespot previous credentials restored after failed authorization");
        }
    }
}

fn wait_child(p: &mut Child, secs: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        match p.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
