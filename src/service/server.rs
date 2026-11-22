//! Unix-domain JSON-RPC server for the NPLAY headless control service
//! (port of `nplay/service/server.py`).
//!
//! Public API:
//! - `serve(path, idle_exit, backend) -> i32`: run the service in the
//!   foreground with SIGTERM/SIGINT/SIGHUP as graceful stops; prints
//!   `NPLAY 1 service listening on <path>` and `pid <pid> · protocol 1`, or
//!   `NPLAY service: <message>` (+ `Existing endpoint: <path>`) when the
//!   endpoint is taken (exit 0 for "already"/"lock" messages, else 1), or
//!   `NPLAY service failed: <error>` (exit 1).
//! - `ControlServer::new(path, idle_exit, backend)`, `.start()`, `.run()`
//!   (returns the stop reason), `.request_stop(reason)`, `.shutdown()`,
//!   `.path()`: the same pieces for embedding and tests.
//! - Constants `MAX_CLIENT_QUEUE` (256), `MAX_OUTSTANDING` (8),
//!   `MAX_FRAME_BACKLOG` (8), `SOCKET_TIMEOUT` (5 s, writes only),
//!   `MAX_WORKERS` (8).
//!
//! Behaviour: private 0700 runtime dir owned by the user, 0600 socket and
//! lock file, `flock` instance lock, stale-socket probe and cleanup. Each
//! connection has a blocking reader thread and a writer thread fed by a
//! bounded queue (256); requests run on a lazily grown pool of at most 8
//! workers with at most 8 outstanding per client (-32005 beyond). State
//! notifications are droppable; visualization frames take a non-blocking
//! direct-write fast path or a small backlog (8) and never block the frame
//! producer. Idle (no clients, or idle clients) costs no wakeups: reads,
//! accepts, queues and the idle-exit timer all block.

use super::protocol::{
    encode, error_response, lock_path, notification, parse_request, response, socket_path,
    RpcError, MAX_FRAME, PROTOCOL_MINOR, PROTOCOL_VERSION, TOO_MANY_OUTSTANDING,
};
use super::{Backend, FrameSink, NotifyFn};
use serde_json::{Map, Value};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

pub const MAX_CLIENT_QUEUE: usize = 256;
pub const MAX_OUTSTANDING: usize = 8;
/// Pushed visualization frames beyond this backlog are dropped, so a stalled
/// client can never fill the queue and lose responses.
pub const MAX_FRAME_BACKLOG: usize = 8;
/// Bounds a write to a stalled peer (reads block without a timeout).
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_WORKERS: usize = 8;
const SHUTDOWN_FLUSH: Duration = Duration::from_millis(500);
const IO_STACK: usize = 256 * 1024;
const WORKER_STACK: usize = 1024 * 1024;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

/// Python's `str(OSError)`: `[Errno N] strerror` (+ `: 'path'`).
fn py_oserror(e: &std::io::Error, path: Option<&Path>) -> String {
    let Some(code) = e.raw_os_error() else {
        return e.to_string();
    };
    let text = e.to_string();
    let suffix = format!(" (os error {code})");
    let msg = text.strip_suffix(&suffix).unwrap_or(&text).to_string();
    match path {
        Some(p) => format!("[Errno {code}] {msg}: '{}'", p.display()),
        None => format!("[Errno {code}] {msg}"),
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn sockaddr(path: &Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.len() > addr.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "AF_UNIX path too long",
        ));
    }
    for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    let base = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    Ok((addr, (base + bytes.len()) as libc::socklen_t))
}

/// Connect to a Unix stream socket within `timeout` (also bounding a full
/// listen backlog); the stream gets `timeout` as its read/write timeout.
pub fn connect_timeout(path: &Path, timeout: Duration) -> std::io::Result<UnixStream> {
    let (addr, len) = sockaddr(path)?;
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let deadline = Instant::now() + timeout;
    loop {
        let rc = unsafe { libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) };
        if rc == 0 {
            break;
        }
        match errno() {
            libc::EINTR => continue,
            libc::EAGAIN | libc::EINPROGRESS => {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            libc::EISCONN => break,
            _ => return Err(std::io::Error::last_os_error()),
        }
    }
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

// -- worker pool ----------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct PoolState {
    jobs: VecDeque<Job>,
    idle: usize,
    threads: usize,
    stopped: bool,
}

/// A fixed-size pool created lazily: threads appear only when work is
/// pending and none is idle, and then block on a condvar (no wakeups).
#[derive(Default)]
struct Pool {
    state: Mutex<PoolState>,
    cv: Condvar,
}

impl Pool {
    fn submit(self: &Arc<Self>, job: Job) -> bool {
        let mut st = lock(&self.state);
        if st.stopped {
            return false;
        }
        st.jobs.push_back(job);
        if st.jobs.len() > st.idle && st.threads < MAX_WORKERS {
            let me = self.clone();
            let ok = std::thread::Builder::new()
                .name("nplay-rpc".into())
                .stack_size(WORKER_STACK)
                .spawn(move || me.work())
                .is_ok();
            if ok {
                st.threads += 1;
            }
        }
        drop(st);
        self.cv.notify_one();
        true
    }

    fn work(&self) {
        loop {
            let job = {
                let mut st = lock(&self.state);
                loop {
                    if st.stopped {
                        st.threads -= 1;
                        return;
                    }
                    if let Some(j) = st.jobs.pop_front() {
                        break j;
                    }
                    st.idle += 1;
                    st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                    st.idle -= 1;
                }
            };
            job();
        }
    }

    fn shutdown(&self) {
        let jobs = {
            let mut st = lock(&self.state);
            st.stopped = true;
            std::mem::take(&mut st.jobs)
        };
        drop(jobs);
        self.cv.notify_all();
    }
}

// -- client ---------------------------------------------------------------------

type After = Box<dyn FnOnce() + Send>;

enum Item {
    Msg(Value, Option<After>),
    Close,
}

#[derive(Default)]
struct Outbox {
    items: VecDeque<Item>,
    /// Messages queued (the sentinel does not count, like Python's put_nowait
    /// skipping it only when full).
    len: usize,
    /// The writer holds a popped message that is not fully written yet.
    writing: bool,
}

struct Client {
    id: u64,
    stream: UnixStream,
    server: Weak<Inner>,
    outbox: Mutex<Outbox>,
    outbox_cv: Condvar,
    alive: AtomicBool,
    subscribed: AtomicBool,
    outstanding: Mutex<usize>,
    write_lock: Mutex<()>,
    writer_done: Mutex<bool>,
    writer_done_cv: Condvar,
}

impl Client {
    fn new(stream: UnixStream, server: Weak<Inner>) -> Arc<Self> {
        Arc::new(Client {
            id: NEXT_CLIENT.fetch_add(1, Ordering::Relaxed),
            stream,
            server,
            outbox: Mutex::new(Outbox::default()),
            outbox_cv: Condvar::new(),
            alive: AtomicBool::new(true),
            subscribed: AtomicBool::new(false),
            outstanding: Mutex::new(0),
            write_lock: Mutex::new(()),
            writer_done: Mutex::new(false),
            writer_done_cv: Condvar::new(),
        })
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn start(self: &Arc<Self>) -> bool {
        // Only writes to a stalled peer are bounded; reads block (no wakeups
        // for an idle connection).
        let _ = self.stream.set_write_timeout(Some(SOCKET_TIMEOUT));
        let w = self.clone();
        let writer = std::thread::Builder::new()
            .name("nplay-wr".into())
            .stack_size(IO_STACK)
            .spawn(move || w.write_loop());
        if writer.is_err() {
            self.mark_writer_done();
            self.close();
            return false;
        }
        let r = self.clone();
        let reader = std::thread::Builder::new()
            .name("nplay-rd".into())
            .stack_size(IO_STACK)
            .spawn(move || r.read_loop());
        if reader.is_err() {
            self.close();
            return false;
        }
        true
    }

    /// Queue one message; `after` runs on the writer once it is on the wire.
    /// A full queue drops a droppable message and closes the client otherwise.
    fn send(&self, obj: Value, droppable: bool, after: Option<After>) -> bool {
        if !self.alive() {
            return false;
        }
        let mut ob = lock(&self.outbox);
        if ob.len >= MAX_CLIENT_QUEUE {
            drop(ob);
            if droppable {
                return false;
            }
            self.close();
            return false;
        }
        ob.items.push_back(Item::Msg(obj, after));
        ob.len += 1;
        drop(ob);
        self.outbox_cv.notify_one();
        true
    }

    /// Send one pushed visualization frame; dropped (never fatal, never
    /// blocking) when the client lags.
    fn send_frame(&self, obj: Value) -> bool {
        if !self.alive() {
            return false;
        }
        let ob = lock(&self.outbox);
        if ob.len == 0 && !ob.writing {
            if let Ok(guard) = self.write_lock.try_lock() {
                drop(ob);
                let data = encode(&obj);
                let fd = self.stream.as_raw_fd();
                let n = unsafe {
                    libc::send(
                        fd,
                        data.as_ptr() as *const libc::c_void,
                        data.len(),
                        libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                    )
                };
                if n < 0 {
                    let e = errno();
                    drop(guard);
                    if e == libc::EAGAIN || e == libc::EWOULDBLOCK || e == libc::EINTR {
                        return false;
                    }
                    self.fail();
                    return false;
                }
                let n = n as usize;
                if n < data.len() {
                    // Rare: socket buffer nearly full. Finish the line so
                    // framing stays intact (bounded by SOCKET_TIMEOUT).
                    if (&self.stream).write_all(&data[n..]).is_err() {
                        drop(guard);
                        self.fail();
                        return false;
                    }
                }
                return true;
            }
        }
        let mut ob = ob;
        if ob.len >= MAX_FRAME_BACKLOG {
            return false;
        }
        ob.items.push_back(Item::Msg(obj, None));
        ob.len += 1;
        drop(ob);
        self.outbox_cv.notify_one();
        true
    }

    /// A failure on a producer's thread: end the connection without calling
    /// back into the backend here (the reader sees EOF and closes).
    fn fail(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    fn write_loop(self: Arc<Self>) {
        loop {
            let item = {
                let mut ob = lock(&self.outbox);
                loop {
                    if let Some(it) = ob.items.pop_front() {
                        if matches!(it, Item::Msg(..)) {
                            ob.len -= 1;
                            ob.writing = true;
                        }
                        break it;
                    }
                    if !self.alive() {
                        break Item::Close;
                    }
                    ob = self.outbox_cv.wait(ob).unwrap_or_else(|e| e.into_inner());
                }
            };
            let Item::Msg(obj, after) = item else {
                break;
            };
            if !self.alive() {
                break;
            }
            let data = encode(&obj);
            let res = {
                let _g = lock(&self.write_lock);
                (&self.stream).write_all(&data)
            };
            lock(&self.outbox).writing = false;
            if res.is_err() {
                self.close();
                break;
            }
            if let Some(f) = after {
                if catch_unwind(AssertUnwindSafe(f)).is_err() {
                    debug!("post-send hook failed");
                }
            }
        }
        lock(&self.outbox).writing = false;
        self.mark_writer_done();
    }

    fn mark_writer_done(&self) {
        *lock(&self.writer_done) = true;
        self.writer_done_cv.notify_all();
    }

    fn wait_writer(&self, deadline: Instant) {
        let mut done = lock(&self.writer_done);
        while !*done {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            done = self
                .writer_done_cv
                .wait_timeout(done, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn send_sync(&self, obj: &Value) -> bool {
        let _g = lock(&self.write_lock);
        (&self.stream).write_all(&encode(obj)).is_ok()
    }

    fn frame_too_large(&self) {
        let e = RpcError::protocol("frame too large").data("limit", MAX_FRAME as u64);
        self.send_sync(&error_response(Value::Null, &e));
        self.close();
    }

    fn read_loop(self: Arc<Self>) {
        // close() (shutdown) or the peer ends the loop; no polling.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 65536];
        while self.alive() {
            let n = match (&self.stream).read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let scan_from = buf.len();
            buf.extend_from_slice(&chunk[..n]);
            let mut start = 0;
            let mut i = scan_from;
            while let Some(off) = buf[i..].iter().position(|b| *b == b'\n') {
                let end = i + off;
                let line = &buf[start..end];
                if line.len() > MAX_FRAME {
                    self.frame_too_large();
                    return;
                }
                if !is_blank(line) {
                    let line = line.to_vec();
                    self.submit(&line);
                }
                start = end + 1;
                i = start;
            }
            buf.drain(..start);
            if buf.len() > MAX_FRAME {
                self.frame_too_large();
                return;
            }
        }
        self.close();
    }

    fn submit(self: &Arc<Self>, line: &[u8]) {
        let (req_id, method, params) = match parse_request(line) {
            Ok(r) => r,
            Err(e) => {
                self.send(error_response(Value::Null, &e), false, None);
                return;
            }
        };
        {
            let mut n = lock(&self.outstanding);
            if *n >= MAX_OUTSTANDING {
                drop(n);
                let e = RpcError::new(TOO_MANY_OUTSTANDING, "too many outstanding requests");
                self.send(error_response(req_id, &e), false, None);
                return;
            }
            *n += 1;
        }
        let Some(server) = self.server.upgrade() else {
            *lock(&self.outstanding) -= 1;
            return;
        };
        let me = self.clone();
        let srv = server.clone();
        if !server
            .pool
            .submit(Box::new(move || me.run(&srv, req_id, method, params)))
        {
            *lock(&self.outstanding) -= 1;
        }
    }

    fn run(
        self: &Arc<Self>,
        server: &Arc<Inner>,
        req_id: Value,
        method: String,
        params: Map<String, Value>,
    ) {
        let backend = server.backend.clone();
        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<Value, RpcError> {
            match method.as_str() {
                "state.subscribe" => {
                    self.subscribed.store(true, Ordering::SeqCst);
                    Ok(backend.snapshot())
                }
                "state.unsubscribe" => {
                    self.subscribed.store(false, Ordering::SeqCst);
                    Ok(serde_json::json!({"ok": true}))
                }
                "service.shutdown" => {
                    Ok(serde_json::json!({"ok": true, "pid": std::process::id()}))
                }
                "visualization.subscribe" => {
                    let weak = Arc::downgrade(self);
                    let sink: FrameSink = Arc::new(move |frame: Value| {
                        weak.upgrade().is_some_and(|c| c.send_frame(frame))
                    });
                    backend.visualization_subscribe(&params, self.id, sink)
                }
                "visualization.unsubscribe" => backend.visualization_unsubscribe(&params, self.id),
                _ => backend.dispatch(&method, &params),
            }
        }));
        match outcome {
            Ok(Ok(result)) => {
                if method == "visualization.subscribe" {
                    // Frames start only after the subscribe response is on the wire.
                    let b = backend.clone();
                    let id = self.id;
                    self.send(
                        response(req_id, result),
                        false,
                        Some(Box::new(move || b.visualization_activate(id))),
                    );
                } else {
                    self.send(response(req_id, result), false, None);
                }
            }
            Ok(Err(e)) => {
                self.send(error_response(req_id, &e), false, None);
            }
            Err(p) => {
                let detail = panic_text(p.as_ref());
                error!(
                    "service request failed method={method}: {}",
                    crate::redact::redact_text(&detail)
                );
                self.send(
                    error_response(req_id, &RpcError::internal(&detail)),
                    false,
                    None,
                );
            }
        }
        *lock(&self.outstanding) -= 1;
        if method == "service.shutdown" {
            server.request_stop("shutdown requested");
        }
    }

    fn close(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        lock(&self.outbox).items.push_back(Item::Close);
        self.outbox_cv.notify_all();
        if let Some(server) = self.server.upgrade() {
            server.client_gone(self.id);
        }
    }
}

/// Python `bytes.strip()` emptiness (ASCII whitespace incl. \x0b, \x0c).
fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
}

// -- server -----------------------------------------------------------------------

struct Clients {
    map: HashMap<u64, Arc<Client>>,
    last_client: Instant,
    had_client: bool,
}

struct Inner {
    path: PathBuf,
    idle_exit: u64,
    backend: Arc<dyn Backend>,
    pool: Arc<Pool>,
    clients: Mutex<Clients>,
    clients_changed: Condvar,
    stop: AtomicBool,
    reason: Mutex<String>,
    listener: Mutex<Option<UnixListener>>,
    lock_file: Mutex<Option<File>>,
    shutdown_done: AtomicBool,
    started: Instant,
}

impl Inner {
    fn broadcast(&self, method: &str, params: Value) {
        let clients: Vec<Arc<Client>> = lock(&self.clients)
            .map
            .values()
            .filter(|c| c.subscribed.load(Ordering::SeqCst) && c.alive())
            .cloned()
            .collect();
        if clients.is_empty() {
            return;
        }
        let msg = notification(method, params);
        for c in clients {
            c.send(msg.clone(), true, None);
        }
    }

    fn client_gone(&self, id: u64) {
        if catch_unwind(AssertUnwindSafe(|| {
            self.backend.visualization_client_gone(id)
        }))
        .is_err()
        {
            debug!("visualization unsubscribe on disconnect failed");
        }
        let mut st = lock(&self.clients);
        st.map.remove(&id);
        if st.map.is_empty() {
            st.last_client = Instant::now();
        }
        drop(st);
        self.clients_changed.notify_all();
    }

    fn request_stop(&self, reason: &str) {
        *lock(&self.reason) = reason.to_string();
        self.stop.store(true, Ordering::SeqCst);
        drop(lock(&self.clients));
        self.clients_changed.notify_all();
        // Wake the blocking accept() (Linux returns EINVAL after shutdown).
        if let Some(l) = lock(&self.listener).as_ref() {
            unsafe { libc::shutdown(l.as_raw_fd(), libc::SHUT_RDWR) };
        }
    }

    fn idle_loop(&self) {
        // Sleeps until a client leaves or the idle deadline: no periodic wakeups.
        let limit = Duration::from_secs(self.idle_exit);
        let mut st = lock(&self.clients);
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            if !st.map.is_empty() {
                st = self
                    .clients_changed
                    .wait(st)
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let elapsed = st.last_client.elapsed();
            if elapsed >= limit {
                break;
            }
            st = self
                .clients_changed
                .wait_timeout(st, limit - elapsed)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(st);
        self.request_stop("idle exit");
    }
}

/// Why `start()` failed: an endpoint problem (printed as
/// `NPLAY service: ...`) or any other OS error (`NPLAY service failed: ...`).
#[derive(Debug)]
pub enum StartError {
    Endpoint(RpcError),
    Os(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Endpoint(e) => f.write_str(&e.message),
            StartError::Os(s) => f.write_str(s),
        }
    }
}

pub struct ControlServer {
    inner: Arc<Inner>,
}

// Signal plumbing for serve(): the handler only stores the signal number and
// shuts the listening socket down (async-signal-safe), which wakes accept().
static SIGNAL_NUM: AtomicI32 = AtomicI32::new(0);
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL_NUM.store(sig, Ordering::SeqCst);
    let fd = SIGNAL_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
    }
}

impl ControlServer {
    pub fn new(path: Option<&Path>, idle_exit: u64, backend: Arc<dyn Backend>) -> Self {
        let path = socket_path(path);
        backend.set_socket_path(&path);
        let now = Instant::now();
        let inner = Arc::new(Inner {
            path,
            idle_exit,
            backend: backend.clone(),
            pool: Arc::new(Pool::default()),
            clients: Mutex::new(Clients {
                map: HashMap::new(),
                last_client: now,
                had_client: false,
            }),
            clients_changed: Condvar::new(),
            stop: AtomicBool::new(false),
            reason: Mutex::new(String::new()),
            listener: Mutex::new(None),
            lock_file: Mutex::new(None),
            shutdown_done: AtomicBool::new(false),
            started: now,
        });
        let weak = Arc::downgrade(&inner);
        let notify: NotifyFn = Arc::new(move |method: &str, params: Value| {
            if let Some(i) = weak.upgrade() {
                i.broadcast(method, params);
            }
        });
        backend.set_notify(notify);
        ControlServer { inner }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn reason(&self) -> String {
        lock(&self.inner.reason).clone()
    }

    pub fn request_stop(&self, reason: &str) {
        self.inner.request_stop(reason);
    }

    fn prepare(&self) -> Result<bool, StartError> {
        let path = &self.inner.path;
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let parent = if parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            parent
        };
        if let Err(e) = std::fs::create_dir_all(&parent) {
            return Err(StartError::Os(py_oserror(&e, Some(&parent))));
        }
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
        let st = std::fs::symlink_metadata(&parent).map_err(|e| {
            StartError::Endpoint(
                RpcError::backend("cannot access runtime directory")
                    .data("detail", py_oserror(&e, Some(&parent))),
            )
        })?;
        let shown = parent.display().to_string();
        if st.uid() != unsafe { libc::getuid() } {
            return Err(StartError::Endpoint(
                RpcError::backend("runtime directory is not owned by this user")
                    .data("path", shown),
            ));
        }
        if !st.is_dir() {
            return Err(StartError::Endpoint(
                RpcError::backend("runtime path is not a directory").data("path", shown),
            ));
        }
        let lp = lock_path(Some(path));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lp)
            .map_err(|e| StartError::Os(py_oserror(&e, Some(&lp))))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Ok(false);
            }
            return Err(StartError::Os(py_oserror(&e, None)));
        }
        *lock(&self.inner.lock_file) = Some(f);
        Ok(true)
    }

    fn clean_stale(&self) -> Result<(), StartError> {
        let path = &self.inner.path;
        let st = match std::fs::symlink_metadata(path) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(StartError::Os(py_oserror(&e, Some(path)))),
        };
        let shown = path.display().to_string();
        if st.uid() != unsafe { libc::getuid() } {
            return Err(StartError::Endpoint(
                RpcError::backend("socket path is not owned by this user").data("path", shown),
            ));
        }
        if !st.file_type().is_socket() {
            return Err(StartError::Endpoint(
                RpcError::backend("socket path exists and is not a socket").data("path", shown),
            ));
        }
        match connect_timeout(path, Duration::from_millis(250)) {
            Ok(_) => Err(StartError::Endpoint(
                RpcError::backend("another NPLAY service is already listening").data("path", shown),
            )),
            Err(_) => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(StartError::Os(py_oserror(&e, Some(path)))),
            },
        }
    }

    fn bind(&self) -> Result<UnixListener, StartError> {
        let path = &self.inner.path;
        let (addr, len) =
            sockaddr(path).map_err(|_| StartError::Os("AF_UNIX path too long".into()))?;
        let fd: RawFd =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(StartError::Os(py_oserror(
                &std::io::Error::last_os_error(),
                None,
            )));
        }
        let listener = unsafe { UnixListener::from_raw_fd(fd) };
        if unsafe { libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len) } != 0 {
            return Err(StartError::Os(py_oserror(
                &std::io::Error::last_os_error(),
                None,
            )));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| StartError::Os(py_oserror(&e, Some(path))))?;
        if unsafe { libc::listen(fd, 16) } != 0 {
            return Err(StartError::Os(py_oserror(
                &std::io::Error::last_os_error(),
                None,
            )));
        }
        Ok(listener)
    }

    /// Take the instance lock, clean a stale socket and listen.
    pub fn start(&self) -> Result<(), StartError> {
        if !self.prepare()? {
            return Err(StartError::Endpoint(
                RpcError::backend("another NPLAY service holds the instance lock")
                    .data("path", self.inner.path.display().to_string()),
            ));
        }
        self.clean_stale()?;
        let listener = self.bind()?;
        *lock(&self.inner.listener) = Some(listener);
        info!(
            "nplay service listening path={} protocol={}.{} pid={}",
            self.inner.path.display(),
            PROTOCOL_VERSION,
            PROTOCOL_MINOR,
            std::process::id()
        );
        Ok(())
    }

    /// Accept clients until stopped, then shut down. Returns the reason.
    pub fn run(&self, signals: bool) -> String {
        let inner = &self.inner;
        if inner.idle_exit > 0 {
            let i = inner.clone();
            let _ = std::thread::Builder::new()
                .name("nplay-idle".into())
                .stack_size(64 * 1024)
                .spawn(move || i.idle_loop());
        }
        let accept = lock(&inner.listener)
            .as_ref()
            .and_then(|l| l.try_clone().ok());
        if let Some(a) = &accept {
            if signals {
                SIGNAL_FD.store(a.as_raw_fd(), Ordering::SeqCst);
            }
        }
        while let Some(listener) = accept.as_ref() {
            if signals {
                let sig = SIGNAL_NUM.load(Ordering::SeqCst);
                if sig != 0 {
                    inner.request_stop(&format!("signal {sig}"));
                }
            }
            if inner.stop.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((conn, _)) => {
                    if inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let client = Client::new(conn, Arc::downgrade(inner));
                    {
                        let mut st = lock(&inner.clients);
                        st.map.insert(client.id, client.clone());
                        st.had_client = true;
                    }
                    inner.clients_changed.notify_all();
                    client.start();
                }
                Err(_) => {
                    if signals && SIGNAL_NUM.load(Ordering::SeqCst) != 0 {
                        continue;
                    }
                    if inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if signals {
            SIGNAL_FD.store(-1, Ordering::SeqCst);
        }
        drop(accept);
        self.shutdown();
        self.reason()
    }

    /// Stop listening, flush queued responses (bounded 0.5 s), close every
    /// client, close the backend, remove the socket and release the lock.
    pub fn shutdown(&self) {
        let inner = &self.inner;
        if inner.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        inner.stop.store(true, Ordering::SeqCst);
        drop(lock(&inner.clients));
        inner.clients_changed.notify_all();
        let listener = lock(&inner.listener).take();
        drop(listener);
        let clients: Vec<Arc<Client>> = lock(&inner.clients).map.values().cloned().collect();
        // Let queued responses (e.g. the service.shutdown reply) flush: each
        // writer drains its queue up to the sentinel.
        let deadline = Instant::now() + SHUTDOWN_FLUSH;
        for c in &clients {
            lock(&c.outbox).items.push_back(Item::Close);
            c.outbox_cv.notify_all();
        }
        for c in &clients {
            c.wait_writer(deadline);
        }
        for c in &clients {
            c.close();
        }
        inner.pool.shutdown();
        if catch_unwind(AssertUnwindSafe(|| inner.backend.close())).is_err() {
            error!("backend close failed");
        }
        if let Ok(st) = std::fs::symlink_metadata(&inner.path) {
            if st.file_type().is_socket() {
                let _ = std::fs::remove_file(&inner.path);
            }
        }
        if let Some(f) = lock(&inner.lock_file).take() {
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        }
        info!("nplay service stopped reason={}", self.reason());
    }

    /// Seconds since construction.
    pub fn uptime(&self) -> f64 {
        self.inner.started.elapsed().as_secs_f64()
    }
}

struct SignalGuard {
    old: Vec<(libc::c_int, libc::sigaction)>,
}

impl SignalGuard {
    fn install() -> Self {
        SIGNAL_NUM.store(0, Ordering::SeqCst);
        let mut old = Vec::new();
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                let mut prev: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &sa, &mut prev) == 0 {
                    old.push((sig, prev));
                }
            }
        }
        SignalGuard { old }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        for (sig, prev) in &self.old {
            unsafe { libc::sigaction(*sig, prev, std::ptr::null_mut()) };
        }
    }
}

/// Run the service in the foreground. Returns a process exit code.
pub fn serve(path: Option<PathBuf>, idle_exit: u64, backend: Arc<dyn Backend>) -> i32 {
    serve_with(
        path.as_deref(),
        idle_exit,
        backend,
        &mut std::io::stdout(),
        true,
    )
}

pub(crate) fn serve_with(
    path: Option<&Path>,
    idle_exit: u64,
    backend: Arc<dyn Backend>,
    out: &mut dyn Write,
    signals: bool,
) -> i32 {
    let mut say = |line: String| {
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    let server = ControlServer::new(path, idle_exit, backend);
    // A service started from a terminal must not outlive it with an orphaned
    // librespot: SIGHUP is a graceful stop too.
    let _guard = signals.then(SignalGuard::install);
    match server.start() {
        Ok(()) => {}
        Err(StartError::Endpoint(e)) => {
            say(format!("NPLAY service: {}", e.message));
            if let Some(p) = e.data_path() {
                say(format!("Existing endpoint: {p}"));
            }
            return if e.message.contains("already") || e.message.contains("lock") {
                0
            } else {
                1
            };
        }
        Err(StartError::Os(msg)) => {
            say(format!("NPLAY service failed: {msg}"));
            error!("service failed: {msg}");
            return 1;
        }
    }
    say(format!(
        "NPLAY {PROTOCOL_VERSION} service listening on {}",
        server.path().display()
    ));
    say(format!(
        "pid {} · protocol {PROTOCOL_VERSION}",
        std::process::id()
    ));
    match catch_unwind(AssertUnwindSafe(|| server.run(signals))) {
        Ok(_) => 0,
        Err(p) => {
            let msg = panic_text(p.as_ref());
            say(format!("NPLAY service failed: {msg}"));
            error!("service failed: {msg}");
            server.shutdown();
            1
        }
    }
}

#[cfg(test)]
mod tests;
