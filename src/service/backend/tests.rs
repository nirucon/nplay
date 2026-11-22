//! Backend tests over a real in-process control server (ports the backend
//! coverage of tests/test_service.py and tests/test_visualization_stream.py).
//! Playback uses tools/fake-mpv, visualization tests/fake_cava.py; tests that
//! need them are skipped when python3 is missing.

use super::*;
use crate::app::AppOptions;
use crate::service::protocol::encode;
use crate::service::{test_server_lock, ControlServer};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python() -> bool {
    crate::paths::which("python3").is_some()
}

fn write_wav(path: &Path, frames: usize) {
    let data = frames * 2;
    let mut b = Vec::with_capacity(44 + data);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&16000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data as u32).to_le_bytes());
    b.resize(44 + data, 0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b).unwrap();
}

/// One service per test: private XDG tree, App with fake mpv, backend with
/// fake CAVA, server on a private socket. The env is process-wide, so these
/// tests serialise with the engine tests.
struct Svc {
    root: PathBuf,
    backend: Arc<ServiceBackend>,
    server: Arc<ControlServer>,
    sock: PathBuf,
    handle: Option<JoinHandle<String>>,
    _serial: MutexGuard<'static, ()>,
    _srv: std::sync::RwLockReadGuard<'static, ()>,
}

impl Svc {
    fn start() -> Svc {
        let srv = test_server_lock().read().unwrap_or_else(|e| e.into_inner());
        let serial = crate::player::MPV_TESTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Short path: sun_path is limited to 108 bytes.
        let root = std::env::temp_dir().join(format!("npb-{:010x}", fastrand::u64(..) >> 24));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "data",
            "config/nplay",
            "state",
            "cache",
            "run",
            "home/Music",
            "bin",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for (k, d) in [
            ("XDG_DATA_HOME", "data"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "run"),
        ] {
            std::env::set_var(k, root.join(d));
        }
        let cava_log = root.join("cava.log");
        std::env::set_var("FAKE_CAVA_LOG", &cava_log);
        std::env::set_var("FAKE_CAVA_MUSIC_FRAMES", "15");
        std::fs::write(
            root.join("config/nplay/config.ini"),
            format!(
                "[general]\nmusic_dirs = {}\nyoutube_enabled = false\nsr_enabled = false\nnavidrome_enabled = false\nstatistics_enabled = false\ntrack_notifications = false\nmpris_enabled = false\nauto_library_refresh = false\n",
                root.join("home/Music").display()
            ),
        )
        .unwrap();
        let cava = root.join("bin/cava");
        std::fs::write(
            &cava,
            format!(
                "#!/bin/sh\nexec python3 {} \"$@\"\n",
                root_dir().join("tests/fake_cava.py").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cava, std::fs::Permissions::from_mode(0o755)).unwrap();
        let app = App::with_options(AppOptions {
            mpv_binary: Some(root_dir().join("tools/fake-mpv").display().to_string()),
            mpv_log: Some(root.join("state/mpv.log")),
            no_mpris: true,
            notify_binary: Some(None),
        })
        .unwrap();
        let backend = ServiceBackend::new(app, 60);
        backend.set_cava_binary(cava.display().to_string());
        let sock = root.join("run/nplay/control.sock");
        let server = Arc::new(ControlServer::new(
            Some(&sock),
            60,
            Arc::new(BackendHandle(backend.clone())),
        ));
        server.start().expect("server starts");
        let s = server.clone();
        let handle = std::thread::Builder::new()
            .name("nsvc-accept".into())
            .spawn(move || s.run(false))
            .unwrap();
        Svc {
            root,
            backend,
            server,
            sock,
            handle: Some(handle),
            _serial: serial,
            _srv: srv,
        }
    }

    fn wire(&self) -> Wire {
        Wire::new(&self.sock)
    }

    fn cava_starts(&self) -> Vec<(u32, PathBuf)> {
        std::fs::read_to_string(self.root.join("cava.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (p, c) = l.split_once(' ')?;
                Some((p.parse().ok()?, PathBuf::from(c)))
            })
            .collect()
    }
}

impl Drop for Svc {
    fn drop(&mut self) {
        self.server.request_stop("test end");
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.server.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
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

    fn read_timeout(&mut self, t: Duration) -> Option<Value> {
        self.sock.set_read_timeout(Some(t)).unwrap();
        let mut line = String::new();
        let r = match self.rd.read_line(&mut line) {
            Ok(0) => panic!("service closed the connection"),
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            Err(_) => None,
        };
        self.sock
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        r
    }

    fn read(&mut self) -> Value {
        self.read_timeout(Duration::from_secs(10))
            .expect("no message from the service")
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.sock.write_all(&encode(&req)).unwrap();
        loop {
            let msg = self.read();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
            self.notes.push(msg);
        }
    }

    fn result(&mut self, id: i64, method: &str, params: Value) -> Value {
        let r = self.request(id, method, params);
        assert!(r.get("error").is_none(), "{method} failed: {r}");
        r["result"].clone()
    }

    fn wait_note(&mut self, method: &str, secs: f64) -> Value {
        let deadline = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if let Some(i) = self.notes.iter().position(|m| m["method"] == method) {
                return self.notes.remove(i);
            }
            assert!(
                Instant::now() < deadline,
                "notification {method} not received"
            );
            if let Some(m) = self.read_timeout(Duration::from_millis(300)) {
                self.notes.push(m);
            }
        }
    }

    /// Collect pushed frames for `secs`; JSON-RPC messages go to `notes`.
    fn frames(&mut self, secs: f64) -> Vec<Value> {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return out;
            }
            match self.read_timeout(left) {
                Some(m) if m.get("vis").is_some() => out.push(m),
                Some(m) => self.notes.push(m),
                None => return out,
            }
        }
    }
}

fn wait_until(secs: f64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

fn pid_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            s.rsplit_once(')')
                .and_then(|(_, r)| r.split_whitespace().next().map(|st| st != "Z"))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn init(w: &mut Wire) {
    w.result(1, "initialize", json!({"protocol_versions": [1]}));
}

// ---- tests/test_service.py ---------------------------------------------------

#[test]
fn handshake_snapshot_and_capabilities() {
    let svc = Svc::start();
    let mut w = svc.wire();
    let r = w.result(
        1,
        "initialize",
        json!({"client": {"name": "test", "version": "0.1"}, "protocol_versions": [1]}),
    );
    assert_eq!(r["protocol_version"], 1);
    assert_eq!(r["protocol_minor"], 2);
    assert_eq!(r["backend"]["name"], "nplay");
    assert_eq!(
        r["backend"]["socket"],
        json!(svc.sock.display().to_string())
    );
    assert_eq!(r["playback"]["status"], "idle");
    assert!(r["playback"]["current"].is_null());
    assert_eq!(r["playback"]["volume"], json!(100.0));
    assert!(r["playback"]["volume"].is_f64());
    assert_eq!(r["capabilities"]["equalizer"], false);
    assert_eq!(r["client"], json!({"name": "test", "version": "0.1"}));
    let msgs = r["capabilities"]["messages"].as_array().unwrap();
    assert!(msgs.contains(&json!("state.changed")));
    assert_eq!(r["capabilities"]["push"], json!(["vis"]));
    let sources = r["sources"].as_array().unwrap();
    let ids: Vec<&str> = sources.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["local", "navidrome", "spotify", "sr", "youtube", "radio"]
    );
    assert_eq!(sources[0]["enabled"], true);
    assert!(sources[0]["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("search")));
    let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "protocol_version",
            "protocol_minor",
            "backend",
            "revision",
            "server_time",
            "playback",
            "sources",
            "capabilities",
            "client"
        ]
    );
    // Incompatible protocol.
    let r = w.request(2, "initialize", json!({"protocol_versions": [999]}));
    assert_eq!(r["error"]["code"], -32010);
    assert_eq!(r["error"]["data"], json!({"server": 1, "client": [999]}));
}

#[test]
fn queue_events_and_playlists() {
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    w.result(2, "state.subscribe", json!({}));
    let added = w.result(
        3,
        "radio.add",
        json!({"name": "Test Radio", "url": "http://127.0.0.1:9/live"}),
    );
    assert_eq!(added["ok"], true);
    let items =
        w.result(4, "browse", json!({"source": "radio", "section": "all"}))["items"].clone();
    assert_eq!(items.as_array().unwrap().len(), 1);
    let track = items[0].clone();
    assert_eq!(track["source"], "radio");
    assert!(track.get("url").is_none());
    assert!(!track.to_string().contains("127.0.0.1"));
    let q = w.result(5, "queue.add", json!({"track": track}));
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
    let note = w.wait_note("state.changed", 5.0);
    assert!(note["params"]["revision"].as_u64().unwrap() >= 1);
    assert_eq!(note["params"]["reason"], "queue");
    assert_eq!(
        note["params"]["playback"]["queue"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    w.result(6, "state.unsubscribe", json!({}));
    // Local playlists can hold cross-source tracks without copying audio.
    let pid = w.result(7, "playlists.create", json!({"name": "Service list"}))["id"].clone();
    w.result(8, "playlists.add", json!({"id": pid, "track": track}));
    let tracks = w.result(9, "playlists.tracks", json!({"id": pid}));
    assert_eq!(tracks["items"][0]["key"], track["key"]);
    let saved = w.result(10, "queue.save", json!({"name": "Saved queue"}));
    assert_eq!(saved["name"], "Saved queue");
    let names: Vec<Value> = w.result(11, "playlists.list", json!({}))["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].clone())
        .collect();
    assert!(names.contains(&json!("Saved queue")));
    assert_eq!(
        w.result(12, "playlists.delete", json!({"id": pid}))["deleted"],
        pid
    );
}

#[test]
fn play_external_queue_and_bulk_queue() {
    let svc = Svc::start();
    let media = svc.root.join("drop.wav");
    std::fs::write(&media, b"RIFF\x24\x00\x00\x00WAVEfmt ").unwrap();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(
        2,
        "playback.play_external",
        json!({"path": media.display().to_string(), "title": "Dropped", "queue": true}),
    );
    assert_eq!(r["queued"], true);
    assert_eq!(r["track"]["title"], "Dropped");
    assert_eq!(r["track"]["artist"], "Dropped file");
    let q = w.result(3, "queue.list", json!({}));
    assert_eq!(q["items"][0]["title"], "Dropped");
    w.result(
        4,
        "radio.add",
        json!({"name": "Bulk", "url": "http://127.0.0.1:9/bulk"}),
    );
    let track =
        w.result(5, "browse", json!({"source": "radio", "section": "all"}))["items"][0].clone();
    let r = w.result(
        6,
        "queue.add_many",
        json!({"tracks": [track, track, track]}),
    );
    assert_eq!(r["items"].as_array().unwrap().len(), 4);
    let first = r["items"][0].clone();
    w.result(7, "queue.clear", json!({}));
    assert_eq!(w.result(8, "queue.list", json!({}))["items"], json!([]));
    let art = w.result(9, "artwork.get", json!({"track": first}));
    assert_eq!(art["empty"], true);
    // Bad external inputs.
    let r = w.request(10, "playback.play_external", json!({"url": "ftp://x"}));
    assert_eq!(r["error"]["code"], -32602);
    let r = w.request(
        11,
        "playback.play_external",
        json!({"path": "/nonexistent.wav"}),
    );
    assert_eq!(r["error"]["message"], "file does not exist");
    let r = w.request(12, "playback.play_external", json!({}));
    assert_eq!(r["error"]["message"], "path or url is required");
}

#[test]
fn local_playback_through_fake_mpv() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let media = svc.root.join("tone.wav");
    write_wav(&media, 8000 * 30);
    let mut w = svc.wire();
    init(&mut w);
    w.result(2, "state.subscribe", json!({}));
    let r = w.result(
        3,
        "playback.play_external",
        json!({"path": media.display().to_string(), "title": "Tone"}),
    );
    assert_eq!(r["queued"], false);
    let mut playing = Value::Null;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let snap = w.result(20, "state.snapshot", json!({}));
        if snap["playback"]["status"] == "playing" && !snap["playback"]["current"].is_null() {
            playing = snap;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(!playing.is_null(), "local playback never reached playing");
    assert_eq!(playing["playback"]["current"]["title"], "Tone");
    assert_eq!(playing["playback"]["transport"], "mpv");
    assert_eq!(playing["playback"]["seekable"], true);
    let first = playing["playback"]["position"].as_f64().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let later = w.result(21, "state.snapshot", json!({}))["playback"]["position"]
        .as_f64()
        .unwrap();
    assert!(later > first + 0.2, "{first} -> {later}");
    w.result(4, "playback.toggle", json!({}));
    std::thread::sleep(Duration::from_millis(200));
    let paused = w.result(22, "state.snapshot", json!({}))["playback"].clone();
    assert_eq!(paused["status"], "paused");
    w.result(5, "playback.seek", json!({"delta": 10}));
    std::thread::sleep(Duration::from_millis(200));
    let pos = w.result(23, "state.snapshot", json!({}))["playback"]["position"]
        .as_f64()
        .unwrap();
    assert!(pos > later, "{pos} <= {later}");
    // Volume: floats normally, Python's int after a clamp.
    assert_eq!(
        w.result(6, "playback.volume", json!({"delta": 5}))["volume"],
        json!(105.0)
    );
    let v = w.result(7, "playback.volume", json!({"value": 900}))["volume"].clone();
    assert_eq!(v, json!(150));
    assert!(v.is_i64());
    assert_eq!(
        w.result(24, "state.snapshot", json!({}))["playback"]["volume"],
        json!(150.0)
    );
    let m = w.result(8, "playback.mute", json!({}));
    assert_eq!(m["muted"], true);
    let m = w.result(9, "playback.mute", json!({}));
    assert_eq!(m["muted"], false);
    // Reasons of the broadcasts this produced.
    let reasons: Vec<String> = w
        .notes
        .iter()
        .filter(|n| n["method"] == "state.changed")
        .map(|n| n["params"]["reason"].as_str().unwrap().to_string())
        .collect();
    for r in ["playback", "position", "volume"] {
        assert!(reasons.iter().any(|x| x == r), "{r} not in {reasons:?}");
    }
    w.result(10, "playback.stop", json!({}));
    std::thread::sleep(Duration::from_millis(300));
    let idle = w.result(25, "state.snapshot", json!({}))["playback"].clone();
    assert_eq!(idle["status"], "idle");
    assert!(idle["current"].is_null());
}

#[test]
fn queue_reorder_moves_absolute_index() {
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    for i in 0..3 {
        w.result(
            10 + i,
            "radio.add",
            json!({"name": format!("Station {i}"), "url": format!("http://127.0.0.1:9/r{i}")}),
        );
    }
    let tracks = w.result(3, "browse", json!({"source": "radio", "section": "all"}))["items"]
        .as_array()
        .unwrap()
        .clone();
    let titles = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["title"].as_str().unwrap().to_string())
            .collect()
    };
    for (i, t) in tracks.iter().enumerate() {
        w.result(4 + i as i64, "queue.add", json!({"track": t}));
    }
    let r = w.result(20, "queue.reorder", json!({"from": 0, "to": 2}));
    assert_eq!(titles(&r), ["Station 1", "Station 2", "Station 0"]);
    assert_eq!(r["index"], 2);
    let r = w.result(21, "queue.reorder", json!({"from": 2, "to": 0}));
    assert_eq!(titles(&r), ["Station 0", "Station 1", "Station 2"]);
    let r = w.result(22, "queue.move", json!({"index": 0, "delta": 1}));
    assert_eq!(r["index"], 1);
    assert_eq!(titles(&r), ["Station 1", "Station 0", "Station 2"]);
    let r = w.result(23, "queue.remove", json!({"index": 1}));
    assert_eq!(titles(&r), ["Station 1", "Station 2"]);
    let r = w.request(24, "queue.remove", json!({"index": 5}));
    assert_eq!(r["error"]["message"], "queue index out of range");
    let r = w.request(25, "queue.reorder", json!({"from": 9, "to": 0}));
    assert_eq!(r["error"]["message"], "queue source index out of range");
    let r = w.request(26, "queue.play", json!({}));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["message"], "invalid parameters");
}

#[test]
fn containers_are_rejected_and_bulk_playlist_add_works() {
    let svc = Svc::start();
    write_wav(&svc.root.join("home/Music/Test Album/one.wav"), 800);
    let mut w = svc.wire();
    init(&mut w);
    assert_eq!(
        w.result(2, "library.scan", json!({"force": true}))["started"],
        true
    );
    w.result(30, "state.subscribe", json!({}));
    let note = w.wait_note("library.scanned", 10.0);
    assert!(note["params"]["result"]["total"].as_i64().unwrap() >= 1);
    let albums = w.result(
        10,
        "browse",
        json!({"source": "local", "section": "albums"}),
    )["items"]
        .clone();
    let album = albums[0].clone();
    assert_eq!(album["kind"], "album");
    assert_eq!(album["title"], "Test Album");
    let r = w.request(3, "queue.add", json!({"track": album}));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["data"]["kind"], "album");
    let r = w.request(4, "playback.play", json!({"track": album}));
    assert_eq!(r["error"]["code"], -32602);
    let tracks =
        w.result(5, "browse", json!({"source": "local", "section": "all"}))["items"].clone();
    assert!(!tracks.as_array().unwrap().is_empty());
    let pid = w.result(6, "playlists.create", json!({"name": "Bulk"}))["id"].clone();
    let r = w.result(
        7,
        "playlists.add_many",
        json!({"id": pid, "tracks": tracks}),
    );
    assert_eq!(
        r["items"].as_array().unwrap().len(),
        tracks.as_array().unwrap().len()
    );
    let r = w.request(8, "playlists.add", json!({"id": pid, "track": album}));
    assert_eq!(r["error"]["code"], -32602);
    let stats = w.result(9, "library.stats", json!({}));
    assert_eq!(stats["tracks"], 1);
    assert_eq!(stats["schema"], 5);
    // Folders and the folder section.
    let folders = w.result(
        11,
        "browse",
        json!({"source": "local", "section": "folders"}),
    );
    assert_eq!(folders["items"][0]["kind"], "folder");
    let r = w.request(12, "browse", json!({"source": "local", "section": "nope"}));
    assert_eq!(r["error"]["data"], json!({"section": "nope"}));
}

#[test]
fn unknown_method_and_bad_params_are_structured() {
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.request(2, "does.not.exist", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    let r = w.request(
        3,
        "playback.play",
        json!({"track": {"key": "local:missing"}}),
    );
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["data"], json!({"track": "local:missing"}));
    let r = w.request(4, "playback.play", json!({"track": {}}));
    assert_eq!(
        r["error"]["message"],
        "track reference requires key or source+id"
    );
    let r = w.request(
        5,
        "settings.set",
        json!({"settings": {"secret_token": "x"}}),
    );
    assert_eq!(r["error"]["code"], -32602);
    let r = w.request(6, "settings.set", json!({"settings": {"gapless": "maybe"}}));
    assert_eq!(
        r["error"]["message"],
        "boolean setting requires true or false"
    );
    let r = w.request(7, "playback.repeat", json!({"mode": "bogus"}));
    assert_eq!(
        r["error"]["message"],
        "repeat mode must be off, track or context"
    );
    let r = w.request(8, "search", json!({"query": "x".repeat(201)}));
    assert_eq!(r["error"]["message"], "query is too long");
    assert_eq!(
        w.result(9, "search", json!({"query": "  "})),
        json!({"query": "", "groups": []})
    );
    let r = w.request(10, "browse", json!({"source": "youtube"}));
    assert_eq!(r["error"]["code"], -32000);
}

#[test]
fn settings_never_expose_secrets() {
    let svc = Svc::start();
    svc.backend
        .app()
        .cfg
        .save_nav("http://127.0.0.1:9", "me", "hunter2");
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(2, "settings.get", json!({}));
    let text = r.to_string().to_lowercase();
    assert!(!text.contains("password") && !text.contains("hunter2"));
    assert!(!text.contains("refresh_token") && !text.contains("access_token"));
    assert_eq!(r["navidrome"]["user"], "me");
    let keys: Vec<&String> = r["settings"].as_object().unwrap().keys().collect();
    assert_eq!(keys, SAFE_SETTINGS);
    let r = w.result(
        3,
        "settings.set",
        json!({"settings": {"gapless": true, "preferred_source": "local"}}),
    );
    assert_eq!(r["changed"], json!(["gapless", "preferred_source"]));
    assert_eq!(r["settings"]["gapless"], true);
    let r = w.request(
        4,
        "settings.set",
        json!({"settings": {"preferred_source": "ftp"}}),
    );
    assert_eq!(r["error"]["message"], "unknown preferred source");
}

#[test]
fn service_error_notification_and_wire_redaction() {
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    w.result(2, "state.subscribe", json!({}));
    svc.backend
        .ui()
        .error("Spotify", crate::spotify::NOT_AUTHORIZED_MESSAGE);
    let n = w.wait_note("service.error", 5.0);
    assert_eq!(n["params"]["code"], crate::spotify::NOT_AUTHORIZED_CODE);
    assert_eq!(n["params"]["remedy"], "spotify.local_authorize");
    svc.backend.ui().error(
        "Navidrome",
        &format!("failed http://h/rest?u=me&p=hunter2 {}", "x".repeat(400)),
    );
    let n = w.wait_note("service.error", 5.0);
    let msg = n["params"]["message"].as_str().unwrap();
    assert!(!msg.contains("hunter2"));
    assert_eq!(msg.chars().count(), 300);
    assert!(n["params"].get("code").is_none());

    // track_wire: no url/path/cover, meta allowlist, 512-char strings.
    let mut t = Track::new("navidrome", "s1", "Song");
    t.url = "http://h/stream?u=me&p=hunter2".into();
    t.cover = "http://h/cover?id=1".into();
    t.path = "/secret/path".into();
    t.meta.insert("suffix".into(), json!("flac"));
    t.meta.insert("genre".into(), json!("g".repeat(600)));
    t.meta.insert("cover_id".into(), json!("1"));
    t.meta.insert("year".into(), json!([1]));
    let v = svc.backend.track_wire(&t);
    let s = v.to_string();
    assert!(!s.contains("hunter2") && !s.contains("/secret/path") && !s.contains("cover?id"));
    assert_eq!(v["has_art"], true);
    assert_eq!(v["meta"]["genre"].as_str().unwrap().len(), 512);
    let meta_keys: Vec<&String> = v["meta"].as_object().unwrap().keys().collect();
    assert_eq!(meta_keys, ["suffix", "genre"]);
    // The wired track resolves by key and by source+id.
    let r = w.result(
        3,
        "ratings.get",
        json!({"track": {"source": "navidrome", "id": "s1"}}),
    );
    assert_eq!(r["rating"], 0);
}

#[test]
fn catalog_is_bounded() {
    let mut c = Catalog::default();
    for i in 0..(MAX_TRACKS + 10) {
        c.remember(&Track::new("local", &i.to_string(), "t"));
    }
    assert_eq!(c.len(), MAX_TRACKS);
    assert!(c.get("local:0").is_none());
    assert!(c.get("local:9").is_none());
    assert!(c.get("local:10").is_some());
    // Re-wiring moves a track to the young end.
    c.remember(&Track::new("local", "10", "t"));
    c.remember(&Track::new("local", "new", "t"));
    assert!(c.get("local:10").is_some());
    assert!(c.get("local:11").is_none());
}

#[test]
fn shutdown_closes_backend_once() {
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(2, "service.shutdown", json!({}));
    assert_eq!(r["ok"], true);
    assert!(wait_until(5.0, || !svc.sock.exists()));
    assert!(svc.backend.closed.load(Ordering::SeqCst));
    svc.backend.close();
}

// ---- tests/test_visualization_stream.py ---------------------------------------

#[test]
fn capabilities_advertise_stream() {
    let svc = Svc::start();
    let mut w = svc.wire();
    let snap = w.result(1, "initialize", json!({"protocol_versions": [1]}));
    let caps = &snap["capabilities"];
    assert_eq!(caps["visualization_stream"], true);
    assert_eq!(caps["visualization_encodings"], json!(["json", "b64"]));
    assert_eq!(caps["visualization_max_fps"], 30);
    assert_eq!(caps["cava"], true);
    let info = w.result(2, "service.info", json!({}));
    assert_eq!(info["protocol_minor"], 2);
    assert_eq!(info["idle_exit"], 60);
    assert_eq!(info["visualization"]["subscribers"], 0);
    // An idle service has no visualizer thread.
    assert!(!svc.backend.vis.monitor_running());
}

#[test]
fn subscribe_push_silence_and_unsubscribe() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(2, "visualization.subscribe", json!({"bars": 16, "fps": 30}));
    assert_eq!(
        r,
        json!({"subscribed": true, "bars": 16, "fps": 30, "encoding": "json", "source": "cava"})
    );
    assert!(w.notes.iter().all(|n| n.get("vis").is_none()));
    let got = w.frames(1.5);
    // ~15 distinct music frames, then exactly one silent frame, then nothing.
    assert!(got.len() >= 8, "{}", got.len());
    assert!(got.len() <= 17, "{}", got.len());
    assert!(got
        .iter()
        .all(|f| f["vis"].as_array().is_some_and(|a| a.len() == 16)));
    let zero = json!(vec![0; 16]);
    assert_eq!(got.last().unwrap()["vis"], zero);
    assert_eq!(got.iter().filter(|f| f["vis"] == zero).count(), 1);
    assert!(w.frames(0.6).is_empty(), "silence must not be pushed");
    let st = w.result(3, "visualization.status", json!({}));
    assert_eq!(st["running"], true);
    assert_eq!(st["subscribers"], 1);
    assert!(st["suppressed"].as_u64().unwrap() > 0);
    let (pid, _) = *svc.cava_starts().last().unwrap();
    let r = w.result(4, "visualization.unsubscribe", json!({}));
    assert_eq!(r, json!({"subscribed": false, "was_subscribed": true}));
    assert!(
        wait_until(4.0, || !pid_alive(pid)),
        "CAVA must stop with 0 subscribers"
    );
    let st = w.result(5, "visualization.status", json!({}));
    assert_eq!(st["running"], false);
    assert_eq!(st["subscribers"], 0);
    assert!(w.frames(0.3).is_empty());
    assert!(wait_until(2.0, || !svc.backend.vis.monitor_running()));
}

#[test]
fn b64_encoding_and_fps_limit() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(
        2,
        "visualization.subscribe",
        json!({"bars": 24, "fps": 10, "encoding": "b64"}),
    );
    assert_eq!(r["encoding"], "b64");
    let got = w.frames(0.45);
    assert!(!got.is_empty());
    for f in &got {
        assert_eq!(f["enc"], "b64");
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(f["vis"].as_str().unwrap())
            .unwrap();
        assert_eq!(raw.len(), 24);
    }
    // 10 fps for 0.45 s of music: ~5 frames, never CAVA's full 30 fps.
    assert!(got.len() <= 7, "{}", got.len());
    let bad = w.request(3, "visualization.subscribe", json!({"encoding": "xml"}));
    assert_eq!(bad["error"]["code"], -32602);
}

#[test]
fn disconnect_ends_stream_and_stops_cava() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let pid = {
        let mut w = svc.wire();
        init(&mut w);
        w.result(2, "visualization.subscribe", json!({"bars": 16}));
        assert!(!w.frames(0.3).is_empty());
        let (pid, _) = *svc.cava_starts().last().unwrap();
        assert!(pid_alive(pid));
        pid
    };
    assert!(
        wait_until(4.0, || !pid_alive(pid)),
        "CAVA must stop after the subscriber disconnects"
    );
    let mut w = svc.wire();
    let st = w.result(1, "visualization.status", json!({}));
    assert_eq!(st["subscribers"], 0);
    assert_eq!(st["running"], false);
}

#[test]
fn legacy_data_request_still_works() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    let r = w.result(2, "visualization.data", json!({"bars": 20}));
    assert_eq!(r["bars"].as_array().unwrap().len(), 20);
    std::thread::sleep(Duration::from_millis(200));
    let r = w.result(3, "visualization.data", json!({"bars": 20}));
    assert_eq!(r["bars"].as_array().unwrap().len(), 20);
    assert!(r["bars"].as_array().unwrap().iter().any(|x| x != 0));
    // A poller never receives pushed frames.
    assert!(w.frames(0.3).is_empty());
    // A subscriber leaving does not cut CAVA for a recent poller.
    w.result(4, "visualization.subscribe", json!({"bars": 8}));
    w.result(5, "visualization.unsubscribe", json!({}));
    w.frames(0.1);
    let st = w.result(6, "visualization.status", json!({}));
    assert_eq!(st["running"], true);
    assert_eq!(st["polled_recently"], true);
    // visualization.stop stops a poll-only CAVA; start reports 1.x's 48 bars.
    w.result(7, "visualization.stop", json!({}));
    assert_eq!(
        w.result(8, "visualization.status", json!({}))["running"],
        false
    );
    assert_eq!(
        w.result(9, "visualization.start", json!({})),
        json!({"available": true, "bars": 48})
    );
    w.result(10, "visualization.stop", json!({}));
}

#[test]
fn cava_config_is_private_and_removed() {
    if !python() {
        return;
    }
    let svc = Svc::start();
    let mut w = svc.wire();
    init(&mut w);
    w.result(2, "visualization.subscribe", json!({"bars": 16}));
    w.frames(0.3);
    let (pid, cfg) = svc.cava_starts().last().unwrap().clone();
    assert_eq!(cfg.parent().unwrap(), svc.root.join("run/nplay"));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(cfg.parent().unwrap()), 0o700);
    assert_eq!(mode(&cfg), 0o600);
    w.result(3, "visualization.unsubscribe", json!({}));
    assert!(wait_until(4.0, || !pid_alive(pid)));
    assert!(
        wait_until(2.0, || !cfg.exists()),
        "CAVA config must be removed when CAVA stops"
    );
}

#[test]
fn cava_restarts_with_backoff_while_subscribed() {
    let svc = Svc::start();
    // A CAVA that writes two frames and dies.
    let cava = svc.root.join("bin/cava-dies");
    let log = svc.root.join("dies.log");
    std::fs::write(
        &cava,
        format!(
            "#!/bin/sh\necho start >> {}\nprintf '\\001\\002\\003\\004\\005\\006\\007\\010\\001\\002\\003\\004\\005\\006\\007\\010'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cava, std::fs::Permissions::from_mode(0o755)).unwrap();
    svc.backend.set_cava_binary(cava.display().to_string());
    let starts = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let mut w = svc.wire();
    init(&mut w);
    w.result(2, "visualization.subscribe", json!({"bars": 8}));
    // First retry after 1 s, the second 2 s later.
    assert!(
        wait_until(3.0, || starts() >= 2),
        "no restart: {}",
        starts()
    );
    let t = Instant::now();
    assert!(wait_until(4.0, || starts() >= 3));
    assert!(
        t.elapsed() >= Duration::from_millis(1200),
        "backoff too short"
    );
    w.result(3, "visualization.unsubscribe", json!({}));
    let n = starts();
    std::thread::sleep(Duration::from_millis(2500));
    assert!(starts() <= n + 1, "restarted without subscribers");
    assert!(wait_until(5.0, || !svc.backend.vis.monitor_running()));
}
