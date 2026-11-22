//! Web API, mapping, OAuth and recovery tests (ports of
//! tests/test_spotify_recovery.py and tests/test_spotify_browser.py plus
//! request/mapping coverage against a local fake HTTP server).

use super::*;
use std::collections::HashMap;
use std::io::BufRead;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

// --- in-memory config -------------------------------------------------------

#[derive(Default)]
pub(crate) struct MemCfg {
    pub general: Mutex<HashMap<String, String>>,
    pub tokens: Mutex<Vec<(String, String)>>,
}

impl MemCfg {
    pub fn with(pairs: &[(&str, &str)]) -> Arc<MemCfg> {
        let c = MemCfg::default();
        for (k, v) in pairs {
            c.general
                .lock()
                .unwrap()
                .insert(k.to_string(), v.to_string());
        }
        Arc::new(c)
    }
    fn set_tokens(&self, t: &[(&str, &str)]) {
        *self.tokens.lock().unwrap() = t
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    }
    fn tok(&self, k: &str) -> String {
        self.tokens
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

impl SpotifyConfig for MemCfg {
    fn get(&self, k: &str, d: &str) -> String {
        self.general
            .lock()
            .unwrap()
            .get(k)
            .cloned()
            .unwrap_or_else(|| d.to_string())
    }
    fn getbool(&self, k: &str, d: bool) -> bool {
        self.general
            .lock()
            .unwrap()
            .get(k)
            .and_then(|v| crate::config::parse_bool(v))
            .unwrap_or(d)
    }
    fn tokens(&self) -> Vec<(String, String)> {
        self.tokens.lock().unwrap().clone()
    }
    fn save_tokens(&self, t: &[(&str, String)]) {
        let mut g = self.tokens.lock().unwrap();
        for (k, v) in t {
            match g.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = v.clone(),
                None => g.push((k.to_string(), v.clone())),
            }
        }
    }
    fn clear_tokens(&self) {
        self.tokens.lock().unwrap().clear();
    }
}

pub(crate) fn temp_root(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "nplay-sp-{tag}-{}-{:x}",
        std::process::id(),
        fastrand::u64(..)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn engine(root: &std::path::Path) -> LocalSpotifyEngine {
    LocalSpotifyEngine::with_options(EngineOptions {
        settings: Arc::new(|_, d| d.to_string()),
        data_dir: root.join("data"),
        state_dir: root.join("state"),
        runtime_dir: Some(root.join("run")),
        binary: Some(root.join("no-librespot")),
        env: vec![],
    })
}

// --- fake HTTP server -------------------------------------------------------

#[derive(Clone, Debug)]
struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn path(&self) -> &str {
        self.target.split('?').next().unwrap()
    }
    fn query(&self) -> Vec<(String, String)> {
        parse_qsl(self.target.split_once('?').map(|x| x.1).unwrap_or(""))
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

type Handler = Arc<dyn Fn(&Req) -> (u16, String, String) + Send + Sync>;

struct Fake {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
}

fn fake_server(h: Handler) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let reqs: Arc<Mutex<Vec<Req>>> = Arc::default();
    let r2 = reqs.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut rd = std::io::BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if rd.read_line(&mut line).is_err() {
                continue;
            }
            let mut it = line.split_whitespace();
            let method = it.next().unwrap_or("").to_string();
            let target = it.next().unwrap_or("").to_string();
            let mut headers = Vec::new();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if rd.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.trim_end().split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    headers.push((k.to_lowercase(), v.trim().to_string()));
                }
            }
            let mut body = vec![0u8; len];
            let _ = std::io::Read::read_exact(&mut rd, &mut body);
            let req = Req {
                method,
                target,
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            r2.lock().unwrap().push(req.clone());
            let (code, ctype, body) = h(&req);
            let reason = match code {
                200 => "OK",
                204 => "No Content",
                403 => "Forbidden",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let resp = format!(
                "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    Fake { base, reqs }
}

fn ok(v: Value) -> (u16, String, String) {
    (200, "application/json; charset=utf-8".into(), v.to_string())
}

fn client(fake: &Fake, root: &std::path::Path) -> (Spotify, Arc<MemCfg>) {
    let cfg = MemCfg::with(&[("spotify_client_id", "cid")]);
    cfg.set_tokens(&[
        ("access_token", "AT"),
        ("refresh_token", "RT"),
        ("expires_at", &format!("{}", crate::paths::now() + 3000.0)),
    ]);
    let sp = Spotify::with_parts(
        cfg.clone(),
        engine(root),
        Endpoints {
            api: format!("{}/v1", fake.base),
            auth: format!("{}/authorize", fake.base),
            token: format!("{}/api/token", fake.base),
            callback_port: 0,
        },
    );
    (sp, cfg)
}

// --- mapping ----------------------------------------------------------------

#[test]
fn track_mapping_matches_python_keys() {
    let x = json!({"id":"t1","name":"Song","artists":[{"name":"A"},{"name":"B"},"x"],
        "album":{"name":"Al","images":[{"url":"http://img/1"}]},"duration_ms":185000,
        "external_urls":{"spotify":"https://open.spotify.com/track/t1"},"explicit":true});
    let t = track_from(&x);
    assert_eq!(
        (t.source.as_str(), t.id.as_str(), t.title.as_str()),
        ("spotify", "t1", "Song")
    );
    assert_eq!(t.artist, "A, B");
    assert_eq!(t.album, "Al");
    assert_eq!(t.duration, 185.0);
    assert_eq!(t.cover, "http://img/1");
    assert!(t.seekable && t.kind == "track");
    assert_eq!(t.meta_str("spotify_uri"), "spotify:track:t1");
    assert_eq!(
        t.meta_str("spotify_url"),
        "https://open.spotify.com/track/t1"
    );
    assert_eq!(t.meta.get("explicit"), Some(&json!(true)));
    let keys: Vec<&String> = t.meta.keys().collect();
    assert_eq!(keys, ["spotify_uri", "spotify_url", "explicit"]);
    // No id: empty uri; explicit uri wins.
    assert_eq!(track_from(&json!({})).meta_str("spotify_uri"), "");
    assert_eq!(
        track_from(&json!({"id":"a","uri":"spotify:episode:a"})).meta_str("spotify_uri"),
        "spotify:episode:a"
    );
}

#[test]
fn container_mappings() {
    let a = album_from(
        &json!({"id":"al","name":"Alb","artists":[{"name":"X"}],"uri":"spotify:album:al","release_date":"2020"}),
    );
    assert_eq!(
        (a.kind.as_str(), a.album.as_str(), a.artist.as_str()),
        ("album", "Alb", "X")
    );
    assert!(!a.seekable);
    assert_eq!(a.meta_str("release_date"), "2020");
    let ar = artist_from(&json!({"id":"ar","name":"Art","images":[{"url":"u"}]}));
    assert_eq!(
        (ar.kind.as_str(), ar.artist.as_str(), ar.cover.as_str()),
        ("artist", "Artist", "u")
    );
    let p = playlist_from(
        &json!({"id":"p","name":"PL","owner":{"display_name":"Me","id":"me1"},"items":{"total":7}}),
    );
    assert_eq!((p.kind.as_str(), p.artist.as_str()), ("playlist", "Me"));
    assert_eq!(p.meta.get("item_count"), Some(&json!(7)));
    assert_eq!(p.meta_str("owner_id"), "me1");
    let p2 = playlist_from(&json!({"id":"p","name":"PL","items":{},"tracks":{"total":3}}));
    assert_eq!(p2.artist, "Spotify");
    assert_eq!(p2.meta.get("item_count"), Some(&json!(3)));
    assert_eq!(
        playlist_from(&json!({"tracks":5})).meta.get("item_count"),
        Some(&json!(0))
    );
    let mut d = Meta::new();
    d.insert("id".into(), json!("d1"));
    d.insert("name".into(), json!("Kitchen"));
    d.insert("type".into(), json!("Speaker"));
    d.insert("is_active".into(), json!(true));
    let dt = device_track(&d);
    assert_eq!(
        (dt.kind.as_str(), dt.title.as_str(), dt.artist.as_str()),
        ("device", "Kitchen", "Speaker")
    );
    assert!(dt.meta_bool("explicit") && !dt.seekable);
}

#[test]
fn search_ranking_and_artist_filter() {
    let d = json!({
        "artists":{"items":[{"id":"1","name":"Something Else"},{"id":"2","name":"Abba"},null]},
        "tracks":{"items":[{"id":"a","name":"Dancing Queen Live"},{"id":"b","name":"Waterloo"},{"id":"c","name":"Dancing Queen"}]},
        "albums":{"items":[]},
    });
    let out = rank_search("dancing  QUEEN", &d);
    let ids: Vec<&str> = out.iter().map(|t| t.id.as_str()).collect();
    // No artist matches a token: all artists are kept in original order.
    assert_eq!(ids, ["1", "2", "c", "a", "b"]);
    let out = rank_search("abba", &d);
    let ids: Vec<&str> = out.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["2", "a", "b", "c"]);
}

#[test]
fn player_state_and_device_choice() {
    let st = parse_state(
        &json!({"item":{"type":"track","id":"t","duration_ms":2000},"is_playing":true,
        "progress_ms":1500,"device":{"id":"d"},"shuffle_state":true,"repeat_state":"context","context":{"uri":"spotify:album:x"}}),
    );
    assert_eq!(st.track.as_ref().unwrap().id, "t");
    assert!(st.is_playing && st.shuffle);
    assert_eq!((st.progress, st.duration), (1.5, 2.0));
    assert_eq!(st.repeat, "context");
    assert_eq!(st.context.get("uri"), Some(&json!("spotify:album:x")));
    let empty = parse_state(&Value::Null);
    assert!(empty.track.is_none() && empty.repeat == "off" && !empty.is_playing);
    assert!(parse_state(&json!({"item":{"type":"episode"}}))
        .track
        .is_none());

    let dv = |v: Value| v.as_object().unwrap().clone();
    let a = dv(json!({"id":"a"}));
    let b = dv(json!({"id":"b","is_active":true}));
    let r = dv(json!({"id":"r","is_restricted":true,"is_active":true}));
    assert_eq!(
        did(&pick_playback_device(vec![a.clone(), b.clone(), r.clone()], "").unwrap()),
        "b"
    );
    assert_eq!(
        did(&pick_playback_device(vec![a.clone(), dv(json!({"id":"c"}))], "c").unwrap()),
        "c"
    );
    assert_eq!(
        did(&pick_playback_device(vec![a.clone(), r.clone()], "").unwrap()),
        "a"
    );
    let e = pick_playback_device(vec![r.clone()], "").unwrap_err();
    assert!(e.message.starts_with("No Spotify Connect device available"));
    let e = pick_playback_device(vec![a, dv(json!({"id":"c"}))], "zz").unwrap_err();
    assert!(e.message.starts_with("No active Spotify Connect device"));
}

#[test]
fn http_error_detail_like_python() {
    assert_eq!(
        http_error_detail(
            "Not Found",
            br#"{"error":{"status":404,"message":"Device not found"}}"#
        ),
        "Device not found"
    );
    assert_eq!(
        http_error_detail(
            "Bad",
            br#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#
        ),
        "Refresh token revoked"
    );
    assert_eq!(
        http_error_detail("Bad", br#"{"error":"invalid_client"}"#),
        "invalid_client"
    );
    assert_eq!(http_error_detail("Bad Request", b"<html>"), "Bad Request");
}

// --- recovery (tests/test_spotify_recovery.py) -------------------------------

struct MockOps {
    waits: Mutex<Vec<std::result::Result<Meta, bool>>>,
    devices: Vec<Meta>,
    restarts: AtomicUsize,
    wait_calls: AtomicUsize,
    now: f64,
    start_err: Option<Error>,
}

impl MockOps {
    fn new(waits: Vec<std::result::Result<Meta, bool>>, devices: Vec<Meta>) -> MockOps {
        MockOps {
            waits: Mutex::new(waits),
            devices,
            restarts: AtomicUsize::new(0),
            wait_calls: AtomicUsize::new(0),
            now: 1000.0,
            start_err: None,
        }
    }
}

impl RecoveryOps for MockOps {
    fn start(&self) -> Result<()> {
        match &self.start_err {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
    fn wait(&self, _: &str, _: f64) -> std::result::Result<Meta, WaitError> {
        self.wait_calls.fetch_add(1, Ordering::SeqCst);
        let mut w = self.waits.lock().unwrap();
        let r = if w.len() > 1 {
            w.remove(0)
        } else {
            w[0].clone()
        };
        r.map_err(|_| WaitError::Timeout(Error::new(ErrorKind::Playback, "missing")))
    }
    fn devices(&self) -> Result<Vec<Meta>> {
        Ok(self.devices.clone())
    }
    fn restart(&self) -> Result<()> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn now(&self) -> f64 {
        self.now
    }
}

fn dev_named(id: &str) -> Meta {
    json!({"id": id, "name": "NPLAY", "is_restricted": false})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn registered_device_does_not_restart() {
    let ops = MockOps::new(vec![Ok(dev_named("123"))], vec![]);
    let last = Mutex::new(f64::NEG_INFINITY);
    assert_eq!(
        did(&ensure_with(&ops, &last, "NPLAY", 20.0).unwrap()),
        "123"
    );
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_device_restarts_once_and_retries() {
    let ops = MockOps::new(vec![Err(true), Ok(dev_named("new"))], vec![]);
    let last = Mutex::new(f64::NEG_INFINITY);
    assert_eq!(
        did(&ensure_with(&ops, &last, "NPLAY", 20.0).unwrap()),
        "new"
    );
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(ops.wait_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn second_failure_stops_after_one_restart() {
    let ops = MockOps::new(vec![Err(true)], vec![]);
    let last = Mutex::new(f64::NEG_INFINITY);
    let e = ensure_with(&ops, &last, "NPLAY", 20.0).unwrap_err();
    assert!(e.message.contains("after one automatic"), "{e}");
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 1);
}

#[test]
fn cooldown_blocks_restart_loop() {
    let mut ops = MockOps::new(vec![Err(true)], vec![]);
    ops.now = 200.0;
    let last = Mutex::new(180.0);
    let e = ensure_with(&ops, &last, "NPLAY", 20.0).unwrap_err();
    assert!(e.message.contains("cooldown"), "{e}");
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 0);
}

#[test]
fn other_request_recovered_before_restart() {
    let ops = MockOps::new(vec![Err(true)], vec![dev_named("123")]);
    let last = Mutex::new(f64::NEG_INFINITY);
    assert_eq!(
        did(&ensure_with(&ops, &last, "NPLAY", 20.0).unwrap()),
        "123"
    );
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 0);
}

#[test]
fn not_authorized_start_spends_nothing() {
    let mut ops = MockOps::new(vec![Ok(dev_named("x"))], vec![]);
    ops.start_err = Some(not_authorized());
    let last = Mutex::new(f64::NEG_INFINITY);
    let e = ensure_with(&ops, &last, "NPLAY", 20.0).unwrap_err();
    assert!(is_not_authorized(&e));
    assert_eq!(ops.wait_calls.load(Ordering::SeqCst), 0);
    assert_eq!(ops.restarts.load(Ordering::SeqCst), 0);
    assert_eq!(*last.lock().unwrap(), f64::NEG_INFINITY);
}

#[test]
fn wait_for_device_fails_fast_when_engine_stopped() {
    let root = temp_root("wait");
    let fake = fake_server(Arc::new(|_| ok(json!({"devices": []}))));
    let (sp, _) = client(&fake, &root);
    let t = Instant::now();
    let e = sp.wait_for_device(None, 5.0, 0.1).unwrap_err();
    assert!(e.message.starts_with("Local Spotify engine stopped"));
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(!is_registration_timeout(&e));
    let _ = std::fs::remove_dir_all(root);
}

// --- Web API over a fake server ----------------------------------------------

#[test]
fn transport_requests_match_python() {
    let root = temp_root("api");
    let fake = fake_server(Arc::new(|r: &Req| match r.path() {
        "/v1/me/player/play"
        | "/v1/me/player/pause"
        | "/v1/me/player/seek"
        | "/v1/me/player/volume"
        | "/v1/me/player"
        | "/v1/me/player/next"
        | "/v1/me/player/queue" => (204, "text/plain".into(), String::new()),
        "/v1/me/player/shuffle" => (200, "text/plain".into(), "  \n".into()),
        _ => (
            404,
            "application/json".into(),
            r#"{"error":{"status":404,"message":"Device not found"}}"#.into(),
        ),
    }));
    let (sp, _) = client(&fake, &root);
    let mut t = Track::new("spotify", "t1", "Song");
    t.set_meta("spotify_uri", "spotify:track:t1");
    t.set_meta("context_uri", "spotify:album:al");
    assert_eq!(sp.play(Some(&t), Some("dev1"), None).unwrap(), Value::Null);
    let mut plain = Track::new("spotify", "t2", "S2");
    plain.set_meta("context_uri", "");
    sp.play(Some(&plain), None, None).unwrap();
    sp.resume(Some("dev1")).unwrap();
    sp.pause(Some("")).unwrap();
    sp.seek(-3.0, Some("d")).unwrap();
    sp.volume(150.0, None).unwrap();
    sp.transfer("d9", false).unwrap();
    sp.next(None).unwrap();
    sp.queue_add(&plain).unwrap();
    assert_eq!(sp.shuffle(true, None).unwrap(), Value::Null);
    let e = sp.previous(Some("gone")).unwrap_err();
    assert!(is_stale_device_error(&e), "{e}");
    assert_eq!(e.message, "Spotify 404 · Device not found");

    let reqs = fake.reqs.lock().unwrap().clone();
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.target.as_str()),
        ("PUT", "/v1/me/player/play?device_id=dev1")
    );
    assert_eq!(
        r.json(),
        json!({"context_uri":"spotify:album:al","offset":{"uri":"spotify:track:t1"}})
    );
    assert!(r
        .headers
        .iter()
        .any(|(k, v)| k == "authorization" && v == "Bearer AT"));
    assert!(r
        .headers
        .iter()
        .any(|(k, v)| k == "user-agent" && v.starts_with("NPLAY/")));
    assert_eq!(reqs[1].json(), json!({"uris":["spotify:track:t2"]}));
    assert_eq!(reqs[1].target, "/v1/me/player/play");
    assert_eq!(
        (reqs[2].target.as_str(), reqs[2].body.as_str()),
        ("/v1/me/player/play?device_id=dev1", "")
    );
    assert_eq!(reqs[3].target, "/v1/me/player/pause");
    assert_eq!(
        reqs[4].target,
        "/v1/me/player/seek?position_ms=0&device_id=d"
    );
    assert_eq!(reqs[5].target, "/v1/me/player/volume?volume_percent=100");
    assert_eq!(
        (reqs[6].method.as_str(), reqs[6].json()),
        ("PUT", json!({"device_ids":["d9"],"play":false}))
    );
    assert_eq!(
        (reqs[7].method.as_str(), reqs[7].target.as_str()),
        ("POST", "/v1/me/player/next")
    );
    assert_eq!(
        reqs[8].query(),
        vec![("uri".to_string(), "spotify:track:t2".to_string())]
    );
    assert_eq!(reqs[9].target, "/v1/me/player/shuffle?state=true");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn context_403_retries_once_with_track_uri() {
    let root = temp_root("403");
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let fake = fake_server(Arc::new(move |_r: &Req| {
        if n2.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                403,
                "application/json".into(),
                r#"{"error":{"status":403,"message":"Player command failed"}}"#.into(),
            )
        } else {
            (204, "text/plain".into(), String::new())
        }
    }));
    let (sp, _) = client(&fake, &root);
    let mut t = Track::new("spotify", "t1", "Song");
    t.set_meta("context_uri", "spotify:playlist:p");
    sp.play(Some(&t), Some("d"), None).unwrap();
    let reqs = fake.reqs.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].json(), json!({"uris":["spotify:track:t1"]}));
    assert_eq!(reqs[1].target, "/v1/me/player/play?device_id=d");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn restriction_and_rate_limit_messages() {
    let root = temp_root("err");
    let fake = fake_server(Arc::new(|r: &Req| {
        if r.path().ends_with("pause") {
            (403, "application/json".into(), r#"{"error":{"status":403,"message":"Player command failed: Restriction violated"}}"#.into())
        } else {
            (429, "application/json".into(), "{}".into())
        }
    }));
    let (sp, _) = client(&fake, &root);
    let e = sp.pause(None).unwrap_err();
    assert!(
        e.message
            .starts_with("Spotify 403 · playback restricted by Spotify"),
        "{e}"
    );
    let e = sp.next(None).unwrap_err();
    assert_eq!(e.message, "Spotify rate limit · try again shortly");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn catalog_calls_and_paging() {
    let root = temp_root("cat");
    let fake = fake_server(Arc::new(|r: &Req| {
        let page2 = format!(
            "http://{}/v1/playlists/p1/items?offset=2&limit=50",
            r.headers
                .iter()
                .find(|(k, _)| k == "host")
                .map(|x| x.1.clone())
                .unwrap_or_default()
        );
        match r.path() {
            "/v1/playlists/p1/items" if r.target.contains("offset=2") => {
                ok(json!({"items":[{"item":{"type":"track","id":"c"}}],"next":null}))
            }
            "/v1/playlists/p1/items" => ok(
                json!({"items":[{"track":{"type":"track","id":"a"}},{"item":{"type":"episode","id":"e"}},{"item":{"type":"track","id":"b"}},null],"next":page2}),
            ),
            "/v1/albums/al" => ok(json!({"name":"Album","images":[{"url":"cover"}]})),
            "/v1/albums/al/tracks" => {
                ok(json!({"items":[{"id":"x","name":"X"},{"id":"y","name":"Y"}]}))
            }
            "/v1/me/player/recently-played" => {
                ok(json!({"items":[{"track":{"id":"r1"}},{"track":null}]}))
            }
            "/v1/me/player/devices" => ok(json!({"devices":[{"id":"d1","name":"NPLAY"},5]})),
            "/v1/me/playlists" => ok(json!({"items":[{"id":"p1","name":"Mine"}]})),
            "/v1/search" => ok(json!({"tracks":{"items":[{"id":"t"}]}})),
            "/v1/me/player" => (204, "text/plain".into(), String::new()),
            _ => ok(json!({"items":[{"id":"z"}]})),
        }
    }));
    let (sp, _) = client(&fake, &root);
    let items = sp.playlist_items("p1").unwrap();
    let ids: Vec<&str> = items.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(items[2].meta.get("context_index"), Some(&json!(2)));
    assert_eq!(items[0].meta_str("context_uri"), "spotify:playlist:p1");
    assert_eq!(items[0].meta_str("context_kind"), "playlist");
    let al = sp.album_tracks("al").unwrap();
    assert_eq!(
        (al[1].album.as_str(), al[1].cover.as_str()),
        ("Album", "cover")
    );
    assert_eq!(al[1].meta.get("context_index"), Some(&json!(1)));
    assert_eq!(al[0].meta_str("context_uri"), "spotify:album:al");
    assert_eq!(sp.recent().unwrap().len(), 1);
    assert_eq!(sp.devices().unwrap().len(), 1);
    assert_eq!(sp.playlists().unwrap()[0].kind, "playlist");
    assert_eq!(sp.top_artists().unwrap()[0].kind, "artist");
    assert_eq!(sp.top_tracks().unwrap()[0].kind, "track");
    assert_eq!(sp.search("q x", 50).unwrap().len(), 1);
    assert!(sp.state().unwrap().track.is_none());
    let reqs = fake.reqs.lock().unwrap().clone();
    assert_eq!(reqs[0].target, "/v1/playlists/p1/items?limit=50");
    assert!(reqs[1].target.contains("offset=2"));
    let search = reqs.iter().find(|r| r.path() == "/v1/search").unwrap();
    assert_eq!(
        search.target,
        "/v1/search?q=q+x&type=artist%2Calbum%2Ctrack%2Cplaylist&limit=10"
    );
    let top = reqs
        .iter()
        .find(|r| r.path() == "/v1/me/top/tracks")
        .unwrap();
    assert_eq!(
        top.target,
        "/v1/me/top/tracks?limit=50&time_range=medium_term"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn expired_token_is_refreshed_and_saved() {
    let root = temp_root("tok");
    let fake = fake_server(Arc::new(|r: &Req| match r.path() {
        "/api/token" => {
            ok(json!({"access_token":"NEW","token_type":"Bearer","expires_in":3600,"scope":"s"}))
        }
        _ => ok(json!({"id":"me"})),
    }));
    let (sp, cfg) = client(&fake, &root);
    cfg.set_tokens(&[
        ("access_token", "OLD"),
        ("refresh_token", "RT"),
        ("expires_at", "1"),
    ]);
    assert!(sp.connected());
    assert_eq!(sp.profile().unwrap(), json!({"id":"me"}));
    let reqs = fake.reqs.lock().unwrap().clone();
    assert_eq!(
        (reqs[0].method.as_str(), reqs[0].path()),
        ("POST", "/api/token")
    );
    assert_eq!(
        reqs[0].query_body(),
        vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".into(), "RT".into()),
            ("client_id".into(), "cid".into())
        ]
    );
    assert!(reqs[1]
        .headers
        .iter()
        .any(|(k, v)| k == "authorization" && v == "Bearer NEW"));
    assert_eq!(cfg.tok("access_token"), "NEW");
    assert_eq!(
        cfg.tok("refresh_token"),
        "RT",
        "an omitted refresh token is kept"
    );
    let exp: f64 = cfg.tok("expires_at").parse().unwrap();
    assert!((exp - (crate::paths::now() + 3540.0)).abs() < 30.0);
    // Valid token: no second refresh.
    sp.profile().unwrap();
    assert_eq!(
        fake.reqs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path() == "/api/token")
            .count(),
        1
    );
    // Disconnected.
    sp.disconnect();
    assert!(!sp.connected());
    assert_eq!(
        sp.profile().unwrap_err().message,
        "Spotify is not connected"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn refresh_failure_message() {
    let root = temp_root("tokf");
    let fake = fake_server(Arc::new(|_| {
        (
            400,
            "application/json".into(),
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#.into(),
        )
    }));
    let (sp, cfg) = client(&fake, &root);
    cfg.set_tokens(&[("refresh_token", "RT")]);
    let e = sp.access_token().unwrap_err();
    assert_eq!(
        e.message,
        "Spotify token refresh failed · Refresh token revoked"
    );
    let _ = std::fs::remove_dir_all(root);
}

impl Req {
    fn query_body(&self) -> Vec<(String, String)> {
        parse_qsl(&self.body)
    }
}

// --- OAuth PKCE (tests/test_spotify_browser.py OAuthServerLifecycleTests) -----

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn oauth_client(fake: &Fake, root: &std::path::Path, port: u16) -> (Spotify, Arc<MemCfg>) {
    let cfg = MemCfg::with(&[("spotify_client_id", "client-id")]);
    let sp = Spotify::with_parts(
        cfg.clone(),
        engine(root),
        Endpoints {
            api: format!("{}/v1", fake.base),
            auth: format!("{}/authorize", fake.base),
            token: format!("{}/api/token", fake.base),
            callback_port: port,
        },
    );
    (sp, cfg)
}

#[test]
fn callback_port_is_closed_when_browser_cannot_open() {
    let root = temp_root("oauth1");
    let fake = fake_server(Arc::new(|_| ok(json!({}))));
    let port = free_port();
    let (sp, _) = oauth_client(&fake, &root, port);
    let e = sp.connect_with(true, 5.0, &|_| false).unwrap_err();
    assert!(e.message.contains("could not be opened"), "{e}");
    assert!(e.message.contains("code_challenge_method=S256"));
    // The port is free again at once.
    TcpListener::bind(("127.0.0.1", port)).expect("callback port released");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn callback_port_conflict_has_actionable_error() {
    let root = temp_root("oauth2");
    let fake = fake_server(Arc::new(|_| ok(json!({}))));
    let busy = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let (sp, _) = oauth_client(&fake, &root, port);
    let e = sp.connect_with(false, 1.0, &|_| true).unwrap_err();
    assert!(
        e.message.contains(&format!("port {port} is unavailable")),
        "{e}"
    );
    let (sp2, _) = oauth_client(&fake, &root, port);
    let cfg = MemCfg::with(&[]);
    let sp3 = Spotify::with_parts(cfg, sp2.local.clone(), (*sp2.endpoints).clone());
    assert_eq!(
        sp3.connect_with(false, 1.0, &|_| true).unwrap_err().message,
        "Spotify Client ID is not configured"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn browser_visit(url: &str, port: u16, path_and_query: &str) -> String {
    let _ = url;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn full_pkce_flow_saves_tokens_and_returns_profile() {
    let root = temp_root("oauth3");
    let fake = fake_server(Arc::new(|r: &Req| match r.path() {
        "/api/token" => ok(
            json!({"access_token":"A1","refresh_token":"R1","expires_in":3600,"token_type":"Bearer","scope":"x"}),
        ),
        "/v1/me" => ok(json!({"id":"user","display_name":"U"})),
        _ => (404, "text/plain".into(), String::new()),
    }));
    let port = free_port();
    let (sp, cfg) = oauth_client(&fake, &root, port);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    let opener = move |url: &str| {
        let url = url.to_string();
        seen2.lock().unwrap().push(url.clone());
        std::thread::spawn(move || {
            let q = parse_qsl(url.split_once('?').unwrap().1);
            let state = q.iter().find(|(k, _)| k == "state").unwrap().1.clone();
            let fav = browser_visit(&url, port, "/favicon.ico");
            assert!(fav.starts_with("HTTP/1.0 404"), "{fav}");
            let page = browser_visit(
                &url,
                port,
                &format!("/callback?code=C0DE&state={state}&error="),
            );
            assert!(page.contains("NPLAY Spotify connected"), "{page}");
        });
        true
    };
    let profile = sp.connect_with(true, 10.0, &opener).unwrap();
    assert_eq!(profile["id"], "user");
    assert_eq!(cfg.tok("access_token"), "A1");
    assert_eq!(cfg.tok("refresh_token"), "R1");
    let url = seen.lock().unwrap()[0].clone();
    let q: HashMap<String, String> = parse_qsl(url.split_once('?').unwrap().1)
        .into_iter()
        .collect();
    assert_eq!(q["client_id"], "client-id");
    assert_eq!(q["response_type"], "code");
    assert_eq!(
        q["redirect_uri"],
        format!("http://127.0.0.1:{port}/callback")
    );
    assert_eq!(q["scope"], SCOPES);
    assert_eq!(q["code_challenge_method"], "S256");
    let reqs = fake.reqs.lock().unwrap().clone();
    let tok: HashMap<String, String> = reqs[0].query_body().into_iter().collect();
    assert_eq!(tok["grant_type"], "authorization_code");
    assert_eq!(tok["code"], "C0DE");
    // The challenge is SHA-256 of the verifier (RFC 7636 S256).
    assert_eq!(
        b64url(&sha2::Sha256::digest(tok["code_verifier"].as_bytes())),
        q["code_challenge"]
    );
    assert_eq!(tok["code_verifier"].len(), 86);
    TcpListener::bind(("127.0.0.1", port)).expect("callback port released after success");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn state_mismatch_and_denial() {
    let root = temp_root("oauth4");
    let fake = fake_server(Arc::new(|_| ok(json!({}))));
    for (query, want) in [
        ("/callback?code=x&state=WRONG", "state mismatch"),
        (
            "/callback?error=access_denied&state={S}",
            "authorization denied · access_denied",
        ),
        ("/callback?state={S}", "returned no code"),
    ] {
        let port = free_port();
        let (sp, _) = oauth_client(&fake, &root, port);
        let opener = move |url: &str| {
            let url = url.to_string();
            let query = query.to_string();
            std::thread::spawn(move || {
                let q = parse_qsl(url.split_once('?').unwrap().1);
                let state = q.iter().find(|(k, _)| k == "state").unwrap().1.clone();
                browser_visit(&url, port, &query.replace("{S}", &state));
            });
            true
        };
        let e = sp.connect_with(true, 10.0, &opener).unwrap_err();
        assert!(e.message.contains(want), "{e} / {want}");
    }
    // No callback at all: timeout message with the Python number format.
    let port = free_port();
    let (sp, _) = oauth_client(&fake, &root, port);
    let e = sp.connect_with(false, 1.0, &|_| true).unwrap_err();
    assert_eq!(e.message, "Spotify authorization timed out after 1s · retry and complete authorization in the browser");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scopes_are_unchanged() {
    assert_eq!(SCOPES.split(' ').count(), 12);
    assert!(SCOPES.starts_with("user-read-private user-read-playback-state"));
    assert!(SCOPES.ends_with("playlist-modify-public"));
}
