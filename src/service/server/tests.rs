//! Transport tests with the fake backend (ports the transport-level coverage
//! of tests/test_service.py, test_visualization_stream.py and
//! test_idle_wakeups.py).

use super::*;
use crate::service::fake::FakeBackend;
use crate::service::protocol::dumps;
use crate::service::test_server_lock;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::thread::JoinHandle;

struct TmpDir(PathBuf);

impl TmpDir {
    fn new() -> Self {
        // Short path: sun_path is limited to 108 bytes.
        let p =
            std::env::temp_dir().join(format!("nps-{:012x}", fastrand::u64(..) & 0xffff_ffff_ffff));
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    fn sock(&self) -> PathBuf {
        self.0.join("run").join("nplay").join("control.sock")
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Running {
    backend: Arc<FakeBackend>,
    server: Arc<ControlServer>,
    path: PathBuf,
    handle: Option<JoinHandle<String>>,
}

impl Running {
    fn start(path: &Path, idle_exit: u64) -> Running {
        let backend = FakeBackend::new();
        let server = Arc::new(ControlServer::new(Some(path), idle_exit, backend.clone()));
        server.start().expect("server starts");
        let s = server.clone();
        let handle = std::thread::Builder::new()
            .name("nsvc-accept".into())
            .spawn(move || s.run(false))
            .unwrap();
        Running {
            backend,
            server,
            path: path.to_path_buf(),
            handle: Some(handle),
        }
    }

    fn join(&mut self) -> String {
        self.handle.take().unwrap().join().unwrap()
    }

    fn finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            self.server.request_stop("test end");
            let _ = h.join();
        }
    }
}

struct Wire {
    sock: UnixStream,
    rd: BufReader<UnixStream>,
    notes: Vec<Value>,
}

impl Wire {
    fn new(path: &Path) -> Wire {
        let sock = UnixStream::connect(path).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let rd = BufReader::new(sock.try_clone().unwrap());
        Wire {
            sock,
            rd,
            notes: vec![],
        }
    }

    fn send_raw(&mut self, data: &[u8]) {
        self.sock.write_all(data).unwrap();
    }

    fn read_raw(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.rd.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(e) => panic!("read failed: {e}"),
        }
    }

    fn read(&mut self) -> Value {
        let line = self.read_raw().expect("service closed the connection");
        serde_json::from_str(&line).unwrap()
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_raw(&encode(&req));
        loop {
            let msg = self.read();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
            self.notes.push(msg);
        }
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

#[test]
fn malformed_json_and_framing() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    w.send_raw(b"not json\n");
    assert_eq!(
        w.read_raw().unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600,\"message\":\"malformed JSON\"}}\n"
    );
    w.send_raw(&[b"[".repeat(100), b"\n".to_vec()].concat());
    assert_eq!(w.read()["error"]["code"], json!(-32600));
    // Blank lines are ignored; two requests in one write; one split request.
    w.send_raw(b"\n  \r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
    assert_eq!(
        w.read_raw().unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"pong\":1.5}}\n"
    );
    assert_eq!(w.read()["id"], json!(2));
    for b in r#"{"jsonrpc":"2.0","id":"x","method":"echo","params":{"s":"åäö"}}"#.as_bytes() {
        w.send_raw(&[*b]);
    }
    w.send_raw(b"\n");
    assert_eq!(
        w.read_raw().unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"result\":{\"s\":\"åäö\"}}\n"
    );
    w.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"x\",\"params\":[1]}\n");
    assert_eq!(
        w.read_raw().unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32602,\"message\":\"params must be an object\"}}\n"
    );
}

#[test]
fn oversized_frame_closes_after_error() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    let mut big = b"{".to_vec();
    big.extend(std::iter::repeat_n(b' ', 1024 * 1024 + 10));
    let _ = w.sock.write_all(&big);
    assert_eq!(
        w.read_raw().unwrap(),
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600,\"message\":\"frame too large\",\"data\":{\"limit\":1048576}}}\n"
    );
    assert!(w.read_raw().is_none(), "connection must close");
    // An over-long line ending in a newline is refused the same way.
    let mut w = Wire::new(&srv.path);
    let mut line = vec![b' '; 1024 * 1024 + 1];
    line.push(b'\n');
    let _ = w.sock.write_all(&line);
    assert!(w.read_raw().unwrap().contains("frame too large"));
    assert!(w.read_raw().is_none());
    // The server keeps serving others.
    let mut w = Wire::new(&srv.path);
    assert!(w.request(1, "ping", json!({})).get("result").is_some());
}

#[test]
fn error_mapping() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    let r = w.request(1, "does.not.exist", json!({}));
    assert_eq!(
        dumps(&r),
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#
    );
    let r = w.request(2, "bad", json!({}));
    assert_eq!(r["error"]["code"], json!(-32602));
    assert_eq!(r["error"]["message"], json!("invalid parameters"));
    assert!(!dumps(&r).contains("hunter2"));
    let r = w.request(3, "panic", json!({}));
    assert_eq!(r["error"]["code"], json!(-32603));
    assert_eq!(r["error"]["message"], json!("internal error"));
    assert_eq!(r["error"]["data"]["detail"], json!("fake backend exploded"));
    assert!(w.request(4, "ping", json!({})).get("result").is_some());
    // Python accepts a bool id (bool is an int) and echoes it.
    w.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":true,\"method\":\"ping\"}\n");
    assert_eq!(w.read()["id"], json!(true));
}

#[test]
fn outstanding_limit() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    let mut batch = Vec::new();
    for id in 1..=10 {
        batch.extend(encode(
            &json!({"jsonrpc": "2.0", "id": id, "method": "block"}),
        ));
    }
    w.send_raw(&batch);
    srv.backend.wait_blocked(8);
    for want in [9, 10] {
        let r = w.read();
        assert_eq!(r["id"], json!(want));
        assert_eq!(
            r["error"],
            json!({"code": -32005, "message": "too many outstanding requests"})
        );
    }
    srv.backend.open_gate();
    let mut ok: Vec<i64> = (0..8).map(|_| w.read()["id"].as_i64().unwrap()).collect();
    ok.sort();
    assert_eq!(ok, (1..=8).collect::<Vec<_>>());
    // Capacity is back.
    assert!(w.request(11, "ping", json!({})).get("result").is_some());
}

#[test]
fn subscribe_and_notifications() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut a = Wire::new(&srv.path);
    let mut b = Wire::new(&srv.path);
    let r = a.request(1, "state.subscribe", json!({}));
    assert_eq!(r["result"]["socket"], json!(srv.path.display().to_string()));
    b.request(1, "ping", json!({}));
    a.request(2, "notify", json!({"revision": 7}));
    assert_eq!(
        dumps(a.notes.last().expect("notification before the response")),
        r#"{"jsonrpc":"2.0","method":"state.changed","params":{"revision":7}}"#
    );
    // The unsubscribed client gets nothing: its next line is its response.
    b.request(2, "notify", json!({"revision": 8}));
    assert!(b.notes.is_empty());
    assert_eq!(a.read()["params"]["revision"], json!(8));
    let r = a.request(3, "state.unsubscribe", json!({}));
    assert_eq!(r["result"], json!({"ok": true}));
    a.notes.clear();
    a.request(4, "notify", json!({"revision": 9}));
    assert!(a.notes.is_empty());
}

#[test]
fn frames_follow_the_subscribe_response() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    for i in 0..50 {
        let id = 2 * i + 1;
        let r = w.request(id, "visualization.subscribe", json!({"bars": 3}));
        assert!(
            w.notes.is_empty(),
            "a frame preceded the subscribe response"
        );
        assert_eq!(r["result"]["subscribed"], json!(true));
        assert_eq!(w.read_raw().unwrap(), "{\"vis\":[1,2,3]}\n");
        let r = w.request(id + 1, "visualization.unsubscribe", json!({}));
        assert_eq!(r["result"]["was_subscribed"], json!(true));
    }
    let gone_before = srv.backend.state.lock().unwrap().gone.len();
    drop(w);
    assert!(wait_until(Duration::from_secs(3), || srv
        .backend
        .state
        .lock()
        .unwrap()
        .gone
        .len()
        > gone_before));
}

#[test]
fn slow_client_frames_are_dropped_never_blocking() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let mut w = Wire::new(&srv.path);
    w.request(1, "visualization.subscribe", json!({"bars": 96}));
    assert_eq!(w.read_raw().unwrap(), "{\"vis\":[1,2,3]}\n");
    let sink = srv.backend.active_sink().unwrap();
    let frame = json!({"vis": (0..96).map(|i| 100 + i).collect::<Vec<_>>()});
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    let mut sent = 0;
    let total = 20000;
    for _ in 0..total {
        let t = Instant::now();
        if sink(frame.clone()) {
            sent += 1;
        }
        worst = worst.max(t.elapsed());
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "producer blocked: {:?}",
        started.elapsed()
    );
    assert!(
        worst < Duration::from_millis(500),
        "one push took {worst:?}"
    );
    assert!(sent < total, "a client that does not read must lose frames");
    // Responses are never displaced by frames and framing stays intact.
    w.send_raw(&encode(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    ));
    let mut frames = 0;
    loop {
        let line = w.read_raw().expect("connection stays open");
        let v: Value = serde_json::from_str(&line).expect("every line is whole JSON");
        if v.get("vis").is_some() {
            assert_eq!(v, frame);
            frames += 1;
        } else {
            assert_eq!(v["id"], json!(2));
            break;
        }
    }
    assert!(frames <= sent);
}

#[test]
fn shutdown_flushes_response_and_cleans_up() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let mut srv = Running::start(&tmp.sock(), 0);
    let lock = lock_path(Some(&srv.path));
    let meta = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(meta(&srv.path), 0o600);
    assert_eq!(meta(srv.path.parent().unwrap()), 0o700);
    assert_eq!(meta(&lock), 0o600);
    assert_eq!(std::fs::metadata(&srv.path).unwrap().uid(), unsafe {
        libc::getuid()
    });
    let mut w = Wire::new(&srv.path);
    let r = w.request(1, "service.shutdown", json!({}));
    assert_eq!(r["result"], json!({"ok": true, "pid": std::process::id()}));
    assert_eq!(srv.join(), "shutdown requested");
    assert!(!srv.path.exists());
    assert!(srv.backend.state.lock().unwrap().closed);
    assert!(w.read_raw().is_none());
    // The lock is released: a new service can take the endpoint.
    let srv2 = Running::start(&tmp.sock(), 0);
    assert!(Wire::new(&srv2.path)
        .request(1, "ping", json!({}))
        .get("result")
        .is_some());
}

#[test]
fn idle_exit_without_and_after_clients() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let mut srv = Running::start(&tmp.sock(), 1);
    let t = Instant::now();
    assert_eq!(srv.join(), "idle exit");
    assert!(t.elapsed() < Duration::from_secs(4));
    assert!(!srv.path.exists());

    let mut srv = Running::start(&tmp.sock(), 1);
    let w = Wire::new(&srv.path);
    std::thread::sleep(Duration::from_millis(1600));
    assert!(
        !srv.finished(),
        "a connected client keeps the service alive"
    );
    drop(w);
    let t = Instant::now();
    assert_eq!(srv.join(), "idle exit");
    assert!(t.elapsed() < Duration::from_secs(4));
}

#[test]
fn second_instance_and_stale_socket() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let path = tmp.sock();
    let srv = Running::start(&path, 0);
    let mut out = Vec::new();
    let code = serve_with(Some(&path), 0, FakeBackend::new(), &mut out, false);
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "NPLAY service: another NPLAY service holds the instance lock\nExisting endpoint: {}\n",
            path.display()
        )
    );
    assert!(Wire::new(&path)
        .request(1, "ping", json!({}))
        .get("result")
        .is_some());
    drop(srv);
    assert!(!path.exists());

    // Someone listens without holding the lock.
    let foreign = UnixListener::bind(&path).unwrap();
    let mut out = Vec::new();
    assert_eq!(
        serve_with(Some(&path), 0, FakeBackend::new(), &mut out, false),
        0
    );
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "NPLAY service: another NPLAY service is already listening\nExisting endpoint: {}\n",
            path.display()
        )
    );
    // A stale socket (nobody listening) is removed and replaced.
    drop(foreign);
    assert!(path.exists());
    let srv = Running::start(&path, 0);
    assert!(Wire::new(&srv.path)
        .request(1, "ping", json!({}))
        .get("result")
        .is_some());
    drop(srv);

    // A regular file in the way is an error (exit 1).
    std::fs::write(&path, b"x").unwrap();
    let mut out = Vec::new();
    assert_eq!(
        serve_with(Some(&path), 0, FakeBackend::new(), &mut out, false),
        1
    );
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "NPLAY service: socket path exists and is not a socket\nExisting endpoint: {}\n",
            path.display()
        )
    );
}

#[test]
fn serve_prints_endpoint_and_stops_on_sigterm() {
    let _l = test_server_lock().write().unwrap();
    let tmp = TmpDir::new();
    let path = tmp.sock();
    let p2 = path.clone();
    let h = std::thread::spawn(move || {
        let mut out = Vec::new();
        let code = serve_with(Some(&p2), 0, FakeBackend::new(), &mut out, true);
        (code, String::from_utf8(out).unwrap())
    });
    assert!(wait_until(Duration::from_secs(5), || UnixStream::connect(
        &path
    )
    .is_ok()));
    unsafe { libc::raise(libc::SIGTERM) };
    let (code, out) = h.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "NPLAY 1 service listening on {}\npid {} · protocol 1\n",
            path.display(),
            std::process::id()
        )
    );
    assert!(!path.exists());
}

fn service_switches() -> u64 {
    let names = [
        "nplay-rd",
        "nplay-wr",
        "nplay-rpc",
        "nplay-idle",
        "nsvc-accept",
    ];
    let mut total = 0;
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    for t in dir.flatten() {
        let comm = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        if !names.contains(&comm.trim()) {
            continue;
        }
        let status = std::fs::read_to_string(t.path().join("status")).unwrap_or_default();
        for line in status.lines() {
            if line.starts_with("voluntary_ctxt_switches")
                || line.starts_with("nonvoluntary_ctxt_switches")
            {
                total += line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
            }
        }
    }
    total
}

#[test]
fn idle_service_has_no_wakeups() {
    if !Path::new("/proc/self/task").exists() {
        return;
    }
    let _l = test_server_lock().write().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 600);
    std::thread::sleep(Duration::from_millis(300));
    let a = service_switches();
    std::thread::sleep(Duration::from_secs(2));
    let idle = service_switches() - a;
    assert!(idle <= 2, "{idle} wakeups in 2 s without clients");

    let mut w = Wire::new(&srv.path);
    w.request(1, "state.subscribe", json!({}));
    std::thread::sleep(Duration::from_millis(300));
    let a = service_switches();
    std::thread::sleep(Duration::from_secs(2));
    let idle = service_switches() - a;
    assert!(
        idle <= 2,
        "{idle} wakeups in 2 s with an idle subscribed client"
    );
    assert!(w.request(2, "ping", json!({})).get("result").is_some());
}

#[test]
fn probe_against_running_service() {
    let _l = test_server_lock().read().unwrap();
    let tmp = TmpDir::new();
    let srv = Running::start(&tmp.sock(), 0);
    let line = crate::service::probe::vis_line(false, Some(&srv.path));
    assert!(line.starts_with("VIS: CAVA, 0 subscribers"), "{line}");
    let mut w = Wire::new(&srv.path);
    w.request(1, "visualization.subscribe", json!({"bars": 8}));
    let line = crate::service::probe::vis_line(true, Some(&srv.path));
    assert!(line.starts_with("VIS: CAVA, 1 subscriber ·"), "{line}");
    assert!(line.contains("service running"), "{line}");
    let info =
        crate::service::probe::service_info(Duration::from_millis(300), Some(&srv.path)).unwrap();
    assert_eq!(info["protocol_minor"], json!(2));
}
