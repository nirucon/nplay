use super::sync::{Transport, TransportError};
use super::*;
use crate::model::Track;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("nplay-stats-test-{:016x}", fastrand::u64(..)));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn store(&self) -> StatisticsStore {
        StatisticsStore::open(&self.0.join("statistics.sqlite3")).unwrap()
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now_int() -> f64 {
    crate::paths::now().floor()
}

fn track(source: &str, id: &str) -> Track {
    let mut t = Track::new(source, id, "Song");
    t.artist = "Artist".into();
    t.album = "Album".into();
    t.duration = 300.0;
    t
}

type Shared = Arc<Mutex<f64>>;
fn clock(v: &Shared) -> tracker::Clock {
    let v = v.clone();
    Arc::new(move || *v.lock().unwrap())
}
fn add(v: &Shared, d: f64) {
    *v.lock().unwrap() += d;
}

// ---- store / tracker (test_statistics.py) ------------------------------

#[test]
fn identity_stable_and_private_db() {
    let t = Tmp::new();
    let s = t.store();
    let id = s.installation_id().unwrap();
    assert_eq!(id, t.store().installation_id().unwrap());
    let mode = std::fs::metadata(&s.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn idempotent_event_and_outbox() {
    let t = Tmp::new();
    let s = t.store();
    for _ in 0..3 {
        s.add(
            "session",
            1000.0,
            2.0,
            &track("spotify", "track1"),
            Some("event-1"),
        )
        .unwrap();
    }
    assert_eq!(s.summary("all", None).unwrap().seconds, 2.0);
    assert_eq!(s.pending_events(100).unwrap().len(), 1);
    s.acknowledge(&["event-1".into()]).unwrap();
    assert_eq!(s.pending_events(100).unwrap().len(), 0);
}

#[test]
fn add_validation() {
    let t = Tmp::new();
    let s = t.store();
    let tr = track("local", "a");
    for bad in [0.0, -1.0, 10.5, f64::NAN, f64::INFINITY] {
        assert_eq!(
            s.add("s", 1000.0, bad, &tr, None).unwrap_err().message,
            "Invalid listening interval"
        );
    }
    assert_eq!(
        s.add("s", crate::paths::now() + 1000.0, 1.0, &tr, None)
            .unwrap_err()
            .message,
        "Listening timestamp is in the future"
    );
    // A zero start uses the wall clock.
    s.add("s", 0.0, 1.0, &tr, Some("z")).unwrap();
    let e = &s.pending_events(10).unwrap()[0];
    assert!(e.start_utc > 1577836800.0);
    let mut long = Track::new("", &"x".repeat(600), "t");
    long.artist = "ä".repeat(600);
    s.add("s", 1000.0, 1.0, &long, Some("long")).unwrap();
    let e = s
        .pending_events(10)
        .unwrap()
        .into_iter()
        .find(|e| e.event_id == "long")
        .unwrap();
    assert_eq!(e.source, "unknown");
    assert_eq!(e.track_id.chars().count(), 512);
    assert_eq!(e.artist.chars().count(), 512);
    assert!(s.summary("bogus", None).is_err());
}

#[test]
fn real_playback_pause_and_track_switch() {
    let t = Tmp::new();
    let s = t.store();
    let (mono, wall) = (Arc::new(Mutex::new(0.0)), Arc::new(Mutex::new(now_int())));
    let tr = ListeningTracker::with_clocks(s.clone(), true, clock(&mono), clock(&wall));
    let a = track("spotify", "track1");
    tr.tick(Some(&a), true);
    add(&mono, 2.0);
    add(&wall, 2.0);
    tr.tick(Some(&a), true);
    add(&mono, 2.0);
    add(&wall, 2.0);
    tr.tick(Some(&a), false);
    add(&mono, 4.0);
    add(&wall, 4.0);
    tr.tick(Some(&a), true);
    add(&mono, 2.0);
    add(&wall, 2.0);
    tr.tick(Some(&a), true);
    tr.flush(true);
    let sum = s.summary("all", None).unwrap();
    assert_eq!(sum.seconds, 4.0);
    assert_eq!(sum.sessions, 2);
}

#[test]
fn suspend_gap_ignored() {
    let t = Tmp::new();
    let s = t.store();
    let mono = Arc::new(Mutex::new(1.0));
    let base = now_int();
    let m2 = mono.clone();
    let wall: tracker::Clock = Arc::new(move || base + *m2.lock().unwrap());
    let tr = ListeningTracker::with_clocks(s.clone(), true, clock(&mono), wall);
    let a = track("spotify", "track1");
    tr.tick(Some(&a), true);
    *mono.lock().unwrap() = 900.0;
    tr.tick(Some(&a), true);
    assert_eq!(s.summary("all", None).unwrap().seconds, 0.0);
}

#[test]
fn disabled_does_not_record() {
    let t = Tmp::new();
    let s = t.store();
    let mono = Arc::new(Mutex::new(0.0));
    let tr =
        ListeningTracker::with_clocks(s.clone(), false, clock(&mono), Arc::new(crate::paths::now));
    let a = track("spotify", "track1");
    tr.tick(Some(&a), true);
    add(&mono, 2.0);
    tr.tick(Some(&a), true);
    tr.flush(true);
    assert_eq!(s.summary("all", None).unwrap().seconds, 0.0);
    assert!(!tr.enabled());
    tr.set_enabled(true);
    assert!(tr.enabled());
}

#[test]
fn source_separation_and_periods() {
    let t = Tmp::new();
    let s = t.store();
    let now = now_int();
    for src in ["local", "navidrome", "spotify", "youtube", "radio"] {
        s.add(src, now, 2.0, &track(src, "same"), None).unwrap();
    }
    assert_eq!(s.summary("all", None).unwrap().sources.len(), 5);
    let t2 = Tmp::new();
    let s = t2.store();
    let a = track("spotify", "track1");
    s.add("old", 1000.0, 2.0, &a, None).unwrap();
    s.add("new", now, 2.0, &a, None).unwrap();
    assert_eq!(s.summary("all", None).unwrap().seconds, 4.0);
    assert_eq!(s.summary("day", Some(now + 1.0)).unwrap().seconds, 2.0);
    for p in ["week", "month", "year"] {
        assert_eq!(s.summary(p, Some(now + 1.0)).unwrap().seconds, 2.0, "{p}");
    }
    let sum = s.summary("all", None).unwrap();
    assert_eq!(sum.artists, vec![("Artist".into(), 4.0, 2)]);
    assert_eq!(sum.albums, vec![("Artist".into(), "Album".into(), 4.0, 2)]);
    assert_eq!(sum.tracks, vec![("Artist".into(), "Song".into(), 4.0, 2)]);
    assert_eq!(sum.pending, 2);
}

// ---- aggregation / boundaries (test_statistics_sync.py) ------------------

struct Agg {
    _t: Tmp,
    s: StatisticsStore,
    mono: Shared,
    wall: Shared,
    tr: ListeningTracker,
    a: Track,
}
impl Agg {
    fn new() -> Agg {
        let t = Tmp::new();
        let s = t.store();
        let mono = Arc::new(Mutex::new(0.0));
        let wall = Arc::new(Mutex::new(now_int() - 86400.0));
        let tr = ListeningTracker::with_clocks(s.clone(), true, clock(&mono), clock(&wall));
        Agg {
            _t: t,
            s,
            mono,
            wall,
            tr,
            a: track("spotify", "track"),
        }
    }
    fn step(&self, n: f64) {
        add(&self.mono, n);
        add(&self.wall, n);
        self.tr.tick(Some(&self.a), true);
    }
}

#[test]
fn five_second_aggregation() {
    let g = Agg::new();
    g.tr.tick(Some(&g.a), true);
    for _ in 0..15 {
        g.step(2.0);
    }
    let sum = g.s.summary("all", None).unwrap();
    assert_eq!(sum.seconds, 30.0);
    assert_eq!(sum.pending, 6);
    assert!(g
        .s
        .pending_events(100)
        .unwrap()
        .iter()
        .all(|e| e.seconds <= 5.0));
}

#[test]
fn stop_flush_pause_new_track_and_suspend() {
    let g = Agg::new();
    g.tr.tick(Some(&g.a), true);
    g.step(2.0);
    g.tr.flush(true);
    assert_eq!(g.s.summary("all", None).unwrap().seconds, 2.0);

    let g = Agg::new();
    g.tr.tick(Some(&g.a), true);
    g.step(2.0);
    g.tr.tick(Some(&g.a), false);
    let other = track("spotify", "other");
    add(&g.mono, 1.0);
    add(&g.wall, 1.0);
    g.tr.tick(Some(&other), true);
    add(&g.mono, 2.0);
    add(&g.wall, 2.0);
    g.tr.tick(Some(&other), true);
    g.tr.flush(false);
    let sum = g.s.summary("all", None).unwrap();
    assert_eq!(sum.seconds, 4.0);
    assert_eq!(sum.sessions, 2);

    let g = Agg::new();
    g.tr.tick(Some(&g.a), true);
    g.step(2.0);
    g.step(100.0);
    g.tr.flush(false);
    assert_eq!(g.s.summary("all", None).unwrap().seconds, 2.0);

    // No track flushes like a stop.
    let g = Agg::new();
    g.tr.tick(Some(&g.a), true);
    g.step(3.0);
    g.tr.tick(None, true);
    assert_eq!(g.s.summary("all", None).unwrap().seconds, 3.0);
}

#[test]
fn midnight_split_local() {
    let t = Tmp::new();
    let s = t.store();
    // Two seconds before a local midnight, a few days ago.
    let stamp = time::next_local_midnight(now_int() - 3.0 * 86400.0) - 2.0;
    let (mono, wall) = (Arc::new(Mutex::new(0.0)), Arc::new(Mutex::new(stamp)));
    let tr = ListeningTracker::with_clocks(s.clone(), true, clock(&mono), clock(&wall));
    let a = track("local", "a");
    tr.tick(Some(&a), true);
    add(&mono, 4.0);
    add(&wall, 4.0);
    tr.tick(Some(&a), true);
    tr.flush(false);
    let sum = s.summary("all", None).unwrap();
    assert!((sum.seconds - 4.0).abs() < 1e-9);
    assert_eq!(sum.days, 2);
    assert_eq!(s.pending_events(100).unwrap().len(), 2);
}

#[test]
fn existing_152_database_survives_open_and_ack() {
    let t = Tmp::new();
    let s = t.store();
    let id = s.installation_id().unwrap();
    let a = track("spotify", "track");
    let base = now_int() - 86400.0;
    for i in 0..700 {
        s.add("legacy-session", base + i as f64 * 2.0, 2.0, &a, None)
            .unwrap();
    }
    let re = t.store();
    assert_eq!(re.installation_id().unwrap(), id);
    let sum = re.summary("all", None).unwrap();
    assert_eq!((sum.pending, sum.seconds), (700, 1400.0));
    assert_eq!(re.pending_events(50).unwrap().len(), 50);
    assert_eq!(re.pending_events(700).unwrap().len(), 500);
}

/// A statistics file as Python 1.8.2 writes it (executescript schema,
/// rollback journal) opens unchanged.
#[test]
fn python_era_statistics_file_opens() {
    let t = Tmp::new();
    let path = t.0.join("statistics.sqlite3");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS identity(key TEXT PRIMARY KEY,value TEXT NOT NULL);
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
   INSERT INTO identity VALUES('installation_id','11111111-2222-4333-8444-555555555555');
   INSERT INTO identity VALUES('last_success','1763800000.25');
   INSERT INTO listening VALUES('e1','s',1700000000,5,'local','x','T','A','B');
   INSERT INTO sync_outbox VALUES('e1','pending',1700000000);",
        )
        .unwrap();
    }
    let s = t.store();
    assert_eq!(
        s.installation_id().unwrap(),
        "11111111-2222-4333-8444-555555555555"
    );
    assert_eq!(s.pending_events(10).unwrap()[0].seconds, 5.0);
    let sync = StatisticsSync::with_options(s, Arc::new(Cfg::new()), None, None);
    assert_eq!(sync.last_success(), 1763800000.25);
}

#[test]
fn envelope_read_only_and_idempotent() {
    let t = Tmp::new();
    let s = t.store();
    let now = now_int();
    let mut tr = Track::new("local", "x", "Track");
    tr.artist = "Artist".into();
    tr.album = "Album".into();
    s.add("session", now, 2.0, &tr, Some("fixed")).unwrap();
    let b = pending_envelope(&s, 100).unwrap();
    assert_eq!(b["format"], json!("nplay.listening-events"));
    assert_eq!(b["format_version"], json!(1));
    assert_eq!(b["installation_id"], json!(s.installation_id().unwrap()));
    let e = &b["events"][0];
    assert_eq!(e["event_id"], json!("fixed"));
    assert_eq!(e["started_at"], json!(time::iso_utc(now)));
    assert!(e["started_at"].as_str().unwrap().ends_with('Z'));
    let keys: Vec<&String> = e.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "event_id",
            "session_id",
            "started_at",
            "listened_seconds",
            "source",
            "source_track_id",
            "title",
            "artist",
            "album"
        ]
    );
    let top: Vec<&String> = b.as_object().unwrap().keys().collect();
    assert_eq!(
        top,
        ["format", "format_version", "installation_id", "events"]
    );
    assert_eq!(s.pending_events(100).unwrap().len(), 1);
    assert_eq!(b, pending_envelope(&s, 100).unwrap());
}

// ---- sync (test_statistics_sync.py) --------------------------------------

struct Cfg {
    enabled: AtomicBool,
    key: Mutex<String>,
    url: Mutex<Option<String>>,
}
impl Cfg {
    fn new() -> Cfg {
        Cfg {
            enabled: AtomicBool::new(true),
            key: Mutex::new("dummy".into()),
            url: Mutex::new(None),
        }
    }
}
impl SyncConfig for Cfg {
    fn getbool(&self, _k: &str, _d: bool) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
    fn get(&self, _k: &str, d: &str) -> String {
        self.url
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| d.to_string())
    }
    fn statistics_token(&self) -> String {
        self.key.lock().unwrap().clone()
    }
}

struct Fix {
    _t: Tmp,
    s: StatisticsStore,
    cfg: Arc<Cfg>,
}
impl Fix {
    fn new() -> Fix {
        let t = Tmp::new();
        let s = t.store();
        let base = now_int() - 86400.0;
        let tr = track("navidrome", "id");
        for i in 0..5 {
            s.add("session", base + i as f64 * 2.0, 2.0, &tr, None)
                .unwrap();
        }
        Fix {
            _t: t,
            s,
            cfg: Arc::new(Cfg::new()),
        }
    }
    fn sync(
        &self,
        f: impl Fn(&str, &str, &Value) -> Result<Value, TransportError> + Send + Sync + 'static,
    ) -> StatisticsSync {
        let tr: Transport = Arc::new(f);
        StatisticsSync::with_options(self.s.clone(), self.cfg.clone(), Some(tr), None)
    }
    fn pending(&self) -> i64 {
        self.s.summary("all", None).unwrap().pending
    }
}

fn ack_all(p: &Value) -> Value {
    let ids: Vec<Value> = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_id"].clone())
        .collect();
    json!({"ok": true, "format_version": 1, "acknowledged_event_ids": ids})
}

#[test]
fn complete_ack() {
    let f = Fix::new();
    let sync = f.sync(|url, token, p| {
        assert!(url.starts_with("https://"));
        assert_eq!(token, "dummy");
        Ok(ack_all(p))
    });
    assert_eq!(sync.once().unwrap(), 5);
    assert_eq!(f.pending(), 0);
    assert_eq!(f.s.summary("all", None).unwrap().seconds, 10.0);
    assert_eq!(sync.status(), "IDLE");
    assert!(sync.last_success() > 0.0);
    let stored: f64 = f.s.get_meta("last_success", "0").unwrap().parse().unwrap();
    assert!((stored - sync.last_success()).abs() < 1e-3);
    // Nothing left: IDLE, 0.
    assert_eq!(sync.once().unwrap(), 0);
}

#[test]
fn partial_ack() {
    let f = Fix::new();
    let sync = f.sync(|_, _, p| {
        Ok(json!({"ok": true, "format_version": 1, "acknowledged_event_ids": [p["events"][0]["event_id"]]}))
    });
    assert_eq!(sync.once().unwrap(), 1);
    assert_eq!(f.pending(), 4);
}

#[test]
fn timeout_preserves_all() {
    let f = Fix::new();
    let sync = f.sync(|_, _, _| Err(TransportError::Network("timeout".into())));
    assert_eq!(sync.once().unwrap_err().0, "Network unavailable or timeout");
    assert_eq!(f.pending(), 5);
    assert!(sync.next_retry() > sync.clock());
    assert_eq!(sync.status(), "ERROR");
    assert_eq!(sync.failures(), 1);
}

#[test]
fn duplicate_ack_unknown_ack_and_bad_responses_rejected() {
    let f = Fix::new();
    type Resp = fn(&Value) -> Value;
    let cases: [Resp; 6] = [
        |p| json!({"ok": true, "format_version": 1, "acknowledged_event_ids": [p["events"][0]["event_id"], p["events"][0]["event_id"]]}),
        |_| json!({"ok": true, "format_version": 1, "acknowledged_event_ids": ["not-a-real-event"]}),
        |_| json!({"ok": true, "format_version": 1, "acknowledged_event_ids": []}),
        |_| json!({"ok": true, "format_version": 2, "acknowledged_event_ids": []}),
        |_| json!({"ok": 1, "format_version": 1}),
        |_| json!([1, 2]),
    ];
    for c in cases {
        let sync = f.sync(move |_, _, p| Ok(c(p)));
        assert!(sync.once().is_err());
        assert_eq!(f.pending(), 5);
    }
}

#[test]
fn http_403_and_503() {
    let f = Fix::new();
    let sync = f.sync(|_, _, _| Err(TransportError::Http(403)));
    let e = sync.once().unwrap_err();
    assert!(e.0.contains("Invalid token"), "{e}");
    assert_eq!(f.pending(), 5);
    assert_eq!(sync.next_retry(), f64::INFINITY);
    let sync = f.sync(|_, _, _| Err(TransportError::Http(503)));
    assert!(sync.once().unwrap_err().0.contains("storage"));
    assert!(sync.next_retry().is_finite());
    assert_eq!(f.pending(), 5);
    let sync = f.sync(|_, _, _| Err(TransportError::Http(418)));
    assert_eq!(sync.once().unwrap_err().0, "HTTP 418");
}

#[test]
fn offline_only_and_https_only_and_no_token() {
    let f = Fix::new();
    f.cfg.enabled.store(false, Ordering::SeqCst);
    let sync = f.sync(|_, _, _| panic!("must not send"));
    assert_eq!(sync.once().unwrap(), 0);
    assert_eq!(sync.status(), "DISABLED");
    assert!(!sync.run_async(true, None));
    assert_eq!(f.pending(), 5);
    f.cfg.enabled.store(true, Ordering::SeqCst);
    for bad in [
        "http://insecure.test/",
        "https://user:pw@h.test/",
        "https://h.test/#x",
        "https:///",
    ] {
        *f.cfg.url.lock().unwrap() = Some(bad.into());
        let e = sync.once().unwrap_err();
        assert!(e.0.contains("HTTPS"), "{bad}: {e}");
    }
    *f.cfg.url.lock().unwrap() = Some(String::new());
    assert_eq!(sync.url(), DEFAULT_URL);
    *f.cfg.key.lock().unwrap() = String::new();
    assert_eq!(
        sync.once().unwrap_err().0,
        "No statistics API token configured"
    );
}

#[test]
fn batch_max_50_and_existing_db() {
    let f = Fix::new();
    let base = now_int() - 86400.0 + 10.0;
    let tr = track("navidrome", "id");
    for i in 0..60 {
        f.s.add("session", base + i as f64 * 2.0, 2.0, &tr, None)
            .unwrap();
    }
    let sync = f.sync(|_, _, p| {
        assert!(p["events"].as_array().unwrap().len() <= 50);
        Ok(ack_all(p))
    });
    let id = f.s.installation_id().unwrap();
    assert_eq!(sync.once().unwrap(), 50);
    assert_eq!(f.pending(), 15);
    assert_eq!(
        StatisticsStore::open(&f.s.path)
            .unwrap()
            .installation_id()
            .unwrap(),
        id
    );
}

#[test]
fn legacy_oversized_event_not_deleted() {
    let f = Fix::new();
    f.s.add(
        "legacy",
        now_int() - 86400.0,
        9.0,
        &track("navidrome", "id"),
        None,
    )
    .unwrap();
    let sync = f.sync(|_, _, _| panic!("must not send"));
    assert!(sync.once().unwrap_err().0.contains("Legacy interval"));
    assert_eq!(f.pending(), 6);
    assert_eq!(sync.next_retry(), f64::INFINITY);
}

#[test]
fn old_timestamp_preserved_locally() {
    let t = Tmp::new();
    let s = t.store();
    s.add("old", 1000.0, 2.0, &track("local", "a"), None)
        .unwrap();
    let tr: Transport = Arc::new(|_: &str, _: &str, _: &Value| panic!("must not send"));
    let sync = StatisticsSync::with_options(s.clone(), Arc::new(Cfg::new()), Some(tr), None);
    assert!(sync
        .once()
        .unwrap_err()
        .0
        .contains("Invalid listening timestamp"));
    assert_eq!(s.summary("all", None).unwrap().pending, 1);
}

#[test]
fn errors_never_contain_the_token() {
    let f = Fix::new();
    *f.cfg.key.lock().unwrap() = "s3cr3t-token-value".into();
    let sync = f.sync(|_, token, _| Err(TransportError::Invalid(format!("bad {token}"))));
    let e = sync.once().unwrap_err();
    assert!(!e.0.contains("s3cr3t-token-value"), "{e}");
    assert!(!sync.last_error().contains("s3cr3t-token-value"));
}

#[test]
fn run_async_backoff_and_callback() {
    let f = Fix::new();
    let sync = f.sync(|_, _, _| Err(TransportError::Network("down".into())));
    let hit = Arc::new(AtomicBool::new(false));
    let h2 = hit.clone();
    assert!(sync.run_async(
        false,
        Some(Arc::new(move || h2.store(true, Ordering::SeqCst)))
    ));
    sync.join();
    assert!(hit.load(Ordering::SeqCst));
    assert_eq!(sync.status(), "ERROR");
    // Backing off: automatic runs wait, manual runs go.
    assert!(!sync.run_async(false, None));
    assert!(sync.run_async(true, None));
    sync.join();
    assert_eq!(sync.failures(), 2);
}

#[test]
fn real_transport_limits_and_network_errors() {
    let big = json!({"events": "x".repeat(300_000)});
    assert_eq!(
        sync::https_send("https://127.0.0.1:9/", "dummy", &big).unwrap_err(),
        TransportError::Invalid("Batch exceeds server payload limit".into())
    );
    match sync::https_send("https://127.0.0.1:9/", "dummy", &json!({"events": []})) {
        Err(TransportError::Network(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn handles_are_send_sync_clone() {
    fn ok<T: Send + Sync + Clone>() {}
    ok::<StatisticsStore>();
    ok::<ListeningTracker>();
    ok::<StatisticsSync>();
}
