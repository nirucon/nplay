//! Engine tests: ports of tests/test_core.py, test_playback_stability.py
//! (engine parts), test_idle_wakeups.py (adapted: no engine threads and no
//! wake-ups once stopped), test_unicode_prompt.py, test_statistics_ui.py and
//! test_statistics_polish.py, plus session, context and command coverage.
//! Real playback uses tools/fake-mpv (skipped when python3 is missing).

use super::*;
use crate::model::Track;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());

/// One App per test in its own XDG tree. The env is process-wide, so App
/// tests are serialized and keep the guard for their whole run.
struct Env {
    app: Arc<App>,
    root: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.app.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fake_mpv() -> Option<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/fake-mpv");
    (p.exists() && crate::paths::which("python3").is_some())
        .then(|| p.to_string_lossy().into_owned())
}

fn env() -> Env {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "nplay-engine-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["data", "config", "state", "cache"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::env::set_var("XDG_DATA_HOME", root.join("data"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
    let app = App::with_options(AppOptions {
        mpv_binary: Some(fake_mpv().unwrap_or_else(|| "/nonexistent/mpv".into())),
        mpv_log: Some(root.join("state/mpv.log")),
        no_mpris: true,
        notify_binary: Some(None),
    })
    .unwrap();
    app.cfg
        .set("music_dirs", root.join("music").display().to_string());
    Env {
        app,
        root,
        _guard: guard,
    }
}

fn track(id: &str, album: &str) -> Track {
    let mut t = Track::new("local", id, &format!("Song {id}"));
    t.artist = format!("Artist {id}");
    t.album = album.into();
    t.path = format!("/nonexistent/{id}.flac");
    t.duration = 180.0;
    t
}

fn wait_for(secs: f64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

/// A UI model like 1.x curses UI: view, back stack, selection, statuses.
#[derive(Default)]
struct RecUi {
    inner: Mutex<Rec>,
}

#[derive(Default)]
struct Rec {
    view: View,
    stack: Vec<View>,
    statuses: Vec<String>,
    errors: Vec<(String, String)>,
    invalidated: bool,
    answers: VecDeque<Option<String>>,
    layout: String,
    sort: String,
}

impl RecUi {
    fn r(&self) -> MutexGuard<'_, Rec> {
        self.inner.lock().unwrap()
    }
    fn last_status(&self) -> String {
        self.r().statuses.last().cloned().unwrap_or_default()
    }
    fn labels(&self) -> Vec<String> {
        self.r()
            .view
            .rows
            .iter()
            .map(|r| r.label().to_string())
            .collect()
    }
}

impl Ui for RecUi {
    fn status(&self, msg: &str) {
        self.r().statuses.push(msg.to_string());
    }
    fn error(&self, title: &str, msg: &str) {
        self.r().errors.push((title.into(), msg.into()));
    }
    fn invalidate(&self) {
        self.r().invalidated = true;
    }
    fn show_menu(&self, title: &str, rows: Vec<Row>, push: bool) {
        let mut r = self.r();
        if push {
            let v = r.view.clone();
            r.stack.push(v);
        }
        let sel = rows.iter().position(Row::selectable).unwrap_or(0);
        r.view = View {
            mode: "menu".into(),
            title: title.into(),
            sel,
            rows,
        };
    }
    fn show_list(
        &self,
        title: &str,
        rows: Vec<Row>,
        _empty: &str,
        _message: Option<&str>,
        push: bool,
    ) {
        let mut r = self.r();
        if push {
            let v = r.view.clone();
            r.stack.push(v);
        }
        r.view = View {
            mode: "list".into(),
            title: title.into(),
            sel: 0,
            rows,
        };
    }
    fn replace_list(&self, title: &str, rows: Vec<Row>, _empty: &str, _message: Option<&str>) {
        self.r().view = View {
            mode: "list".into(),
            title: title.into(),
            sel: 0,
            rows,
        };
    }
    fn loading(&self, title: &str, _msg: &str, rows: Vec<Row>) {
        self.replace_list(title, rows, "", None)
    }
    fn update_items(&self, rows: Vec<Row>, sel: usize) {
        let mut r = self.r();
        r.view.rows = rows;
        r.view.sel = sel;
    }
    fn prompt(&self, _label: &str, _initial: &str, _secret: bool, done: PromptDone) {
        let a = self.r().answers.pop_front().flatten();
        done(a)
    }
    fn push(&self) {
        let mut r = self.r();
        let v = r.view.clone();
        r.stack.push(v);
    }
    fn set_layout(&self, l: &str) {
        self.r().layout = l.into();
    }
    fn set_sort_mode(&self, m: &str) {
        self.r().sort = m.into();
    }
    fn view(&self) -> Option<View> {
        Some(self.r().view.clone())
    }
    fn set_selection(&self, i: usize) {
        self.r().view.sel = i;
    }
    fn edge_selection(&self, _last: bool) {
        let mut r = self.r();
        r.view.sel = r.view.rows.iter().position(Row::selectable).unwrap_or(0);
    }
}

fn rec() -> (Arc<RecUi>, UiRef) {
    let r = Arc::new(RecUi::default());
    let u: UiRef = r.clone();
    (r, u)
}

fn engine_threads() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc/self/task") {
        for e in rd.flatten() {
            if let Ok(c) = std::fs::read_to_string(e.path().join("comm")) {
                let c = c.trim().to_string();
                if c.starts_with("nplay-ticker")
                    || c.starts_with("nplay-spotify")
                    || c.starts_with("nplay-radio")
                {
                    out.push(c);
                }
            }
        }
    }
    out
}

// ---- test_core.py ------------------------------------------------------------

#[test]
fn event_bus_state_registry_session() {
    let bus = crate::core::EventBus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    bus.subscribe(
        "x",
        Arc::new(move |e: &str, p: &Map<String, Value>| {
            s2.lock().unwrap().push((e.to_string(), p["value"].clone()))
        }),
    );
    let mut m = Map::new();
    m.insert("value".into(), json!(7));
    bus.emit("x", m);
    assert_eq!(*seen.lock().unwrap(), vec![("x".to_string(), json!(7))]);

    let mut t = Track::new("sr", "p1", "P1");
    t.kind = "radio".into();
    t.seekable = false;
    let mut s = PlaybackState::new();
    s.update_media(&t, "mpv", 3, "playing");
    assert_eq!(s.status, "playing");
    assert!(!s.seekable);
    assert_eq!(s.kind, "radio");
    assert_eq!(playback_state_value(&s)["generation"], json!(3));
    s.clear();
    assert_eq!(s.status, "idle");

    assert!(crate::core::supports("local", "search"));
    assert!(crate::core::supports("sr", "live"));
    assert!(!crate::core::supports("sr", "random"));
}

#[test]
fn playback_controller_stop_clears() {
    let e = env();
    let a = &e.app;
    a.state().current = Some(track("a", "X"));
    a.playback_stop(None, true);
    assert!(a.current().is_none());
    assert_eq!(a.state().state.status, "idle");
}

// ---- session -----------------------------------------------------------------

#[test]
fn session_roundtrip_scrubs_navidrome_urls() {
    let e = env();
    let a = &e.app;
    let mut nav = Track::new("navidrome", "n1", "Remote");
    nav.url = "https://nd.example/rest/stream.view?u=me&t=secret&s=salt&id=n1".into();
    nav.cover = "https://nd.example/rest/getCoverArt.view?u=me&t=secret&s=salt&id=al-9".into();
    {
        let mut st = a.state();
        st.current = Some(nav.clone());
        st.queue = vec![track("q", "Q")];
        st.later_queue = vec![track("l", "L")];
        st.play_context = vec![nav.clone(), track("b", "B")];
        st.context_index = 0;
        st.volume = 80.0;
    }
    a.checkpoint(Some(42.5), Some(200.0));
    let raw = std::fs::read_to_string(&a.session.path).unwrap();
    assert!(!raw.contains("secret"), "{raw}");
    assert!(!raw.contains("stream.view"));
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["current"]["meta"]["cover_id"], json!("al-9"));
    assert_eq!(v["current"]["url"], json!(""));
    assert_eq!(v["position"], json!(42.5));
    assert_eq!(v["volume"], json!(150.0));
    for k in [
        "current",
        "queue",
        "later_queue",
        "play_context",
        "context_index",
        "position",
        "volume",
    ] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(a.db.resume_get("navidrome:n1", None).unwrap(), 42.5);

    // A fresh load rebuilds the cover from the stable id with current creds.
    a.cfg.save_nav("https://nd.example", "me", "pw");
    a.state().current = None;
    a.load_state();
    let st = a.state();
    let cur = st.current.clone().unwrap();
    assert!(st.resume_candidate);
    assert_eq!(st.resume_position, 42.5);
    assert_eq!(st.queue.len(), 1);
    assert_eq!(st.later_queue.len(), 1);
    assert_eq!(st.play_context.len(), 2);
    assert!(
        cur.cover.contains("getCoverArt.view") && cur.cover.contains("id=al-9"),
        "{}",
        cur.cover
    );
    assert!(cur.url.is_empty());
}

#[test]
fn zero_volume_session_restores_full_volume() {
    let e = env();
    std::fs::write(&e.app.session.path, r#"{"volume":0,"position":3}"#).unwrap();
    e.app.load_state();
    assert_eq!(e.app.volume(), 100.0);
    assert!(e.app.current().is_none());
}

// ---- contexts, shuffle, repeat --------------------------------------------------------

#[test]
fn up_next_queue_repeat_and_later_queue() {
    let e = env();
    let a = &e.app;
    let (x, y) = (track("x", "A"), track("y", "A"));
    a.set_play_context(vec![x.clone(), y.clone()], 0);
    a.state().current = Some(x.clone());
    assert_eq!(a.up_next().unwrap().id, "y");
    a.queue_add(track("q", "Q"), None);
    assert_eq!(a.up_next().unwrap().id, "q");
    a.state().queue.clear();
    a.state().context_index = 1;
    assert!(a.up_next().is_none());
    a.cycle_repeat(None); // track
    assert_eq!(a.up_next().unwrap().id, "x");
    a.cycle_repeat(None); // context
    assert_eq!(a.repeat_mode(), "context");
    assert_eq!(a.up_next().unwrap().id, "x");
    a.cycle_repeat(None);
    assert_eq!(a.repeat_mode(), "off");
    assert_eq!(a.cfg.get("repeat_mode", ""), "off");
    assert_eq!(a.normalization_mode(Some(&x)), "album");
    a.cfg.set("normalization_mode", "track");
    assert_eq!(a.normalization_mode(Some(&x)), "track");
    a.cfg.set("normalization", "false");
    assert_eq!(a.normalization_mode(Some(&x)), "no");
}

#[test]
fn shuffle_keeps_chosen_first_and_off_restores_order() {
    let e = env();
    let a = &e.app;
    let xs: Vec<Track> = (0..12).map(|i| track(&i.to_string(), "A")).collect();
    a.cycle_shuffle(None);
    assert_eq!(a.shuffle_mode(), "shuffle");
    a.set_play_context(xs.clone(), 5);
    {
        let st = a.state();
        assert_eq!(st.play_context[0].id, "5");
        assert_eq!(st.context_index, 0);
        let mut ids: Vec<String> = st.play_context.iter().map(|t| t.id.clone()).collect();
        ids.sort();
        let mut want: Vec<String> = xs.iter().map(|t| t.id.clone()).collect();
        want.sort();
        assert_eq!(ids, want);
    }
    a.state().current = Some(xs[5].clone());
    a.cycle_shuffle(None); // smart
    assert_eq!(a.shuffle_mode(), "smart");
    assert_eq!(a.state().play_context[0].id, "5");
    a.cycle_shuffle(None); // off
    let st = a.state();
    assert_eq!(st.play_context, xs);
    assert_eq!(st.context_index, 5);
}

#[test]
fn spotify_context_snapshot_rules() {
    let e = env();
    let a = &e.app;
    let mut s1 = Track::new("spotify", "s1", "One");
    let s2 = Track::new("spotify", "s2", "Two");
    a.set_play_context(vec![s1.clone(), s2.clone()], 0);
    // Search results without context_uri play as 1/1.
    let snap = a.spotify_context_snapshot(&s1, true);
    assert_eq!(snap.tracks.len(), 1);
    s1.set_meta("context_uri", "spotify:album:x");
    let mut ctx = vec![s1.clone(), s2.clone()];
    ctx[1].set_meta("context_uri", "spotify:album:x");
    a.set_play_context(ctx, 0);
    let snap = a.spotify_context_snapshot(&s1, true);
    assert_eq!(snap.tracks.len(), 2);
    assert_eq!(snap.index, 0);
    assert_eq!(snap.context_uri, "spotify:album:x");
    // A track outside the context never inherits it.
    let mut other = Track::new("spotify", "zz", "Other");
    other.set_meta("context_uri", "spotify:album:y");
    let snap = a.spotify_context_snapshot(&other, true);
    assert_eq!(snap.tracks.len(), 1);
    a.apply_spotify_context(&snap, &other);
    assert_eq!(a.state().play_context.len(), 1);
    assert_eq!(a.state().context_index, 0);
}

// ---- real playback through fake-mpv ------------------------------------------------------

#[test]
fn sync_play_next_previous_and_events() {
    if fake_mpv().is_none() {
        return eprintln!("skipping: fake-mpv or python3 missing");
    }
    let e = env();
    let a = &e.app;
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ev = events.clone();
    a.events.subscribe(
        "*",
        Arc::new(move |name: &str, _p: &Map<String, Value>| ev.lock().unwrap().push(name.into())),
    );
    let xs = vec![track("a", "Al"), track("b", "Al"), track("c", "Al")];
    a.set_play_context(xs.clone(), 0);
    a.play(xs[0].clone(), None, true);
    assert_eq!(a.current().unwrap().id, "a");
    {
        let st = a.state();
        assert_eq!(st.playback_state.state, "playing");
        assert_eq!(st.playback_state.transport, "mpv");
        assert_eq!(st.state.status, "playing");
        assert!(st.play_event_id.is_some());
    }
    assert!(a.playback_active());
    assert!(a.playback_playing());
    let names = events.lock().unwrap().clone();
    assert!(names.contains(&"playback-state-changed".to_string()));
    assert!(names.contains(&"media-changed".to_string()));
    assert!(names.contains(&"integration-state-changed".to_string()));
    // The ticker runs while playing.
    assert!(wait_for(1.0, || engine_threads()
        .iter()
        .any(|t| t.starts_with("nplay-ticker"))));

    a.next_track(None, false);
    assert_eq!(a.current().unwrap().id, "b");
    assert_eq!(a.state().context_index, 1);
    a.queue_next(track("q", "Q"), None);
    a.next_track(None, false);
    assert_eq!(a.current().unwrap().id, "q");
    assert!(a.queue().is_empty());
    a.previous_track(None); // back stack
    assert_eq!(a.current().unwrap().id, "b");
    let hist: Vec<String> =
        a.db.history(10)
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect();
    assert_eq!(&hist[..3], &["b", "q", "b"]);

    // Volume 0..150 and the MPRIS snapshot.
    a.playback_volume(80.0);
    assert_eq!(a.volume(), 150.0);
    let snap = a.mpris_state();
    assert!(snap.playing);
    assert!(snap.can_go_next);

    // Pausing: still active (1.x semantics), not playing; ticker exits.
    a.play_pause(None);
    assert!(wait_for(1.0, || !a.playback_playing()));
    assert!(a.playback_active());
    assert!(!a.mpris_state().playing);
    assert!(
        wait_for(3.0, || engine_threads().is_empty()),
        "{:?}",
        engine_threads()
    );

    a.stop_and_clear(None);
    assert!(a.current().is_none());
    assert!(!a.playback_active());
    let st = a.state();
    assert_eq!(st.context_index, -1);
    assert!(st.play_context.is_empty());
    assert_eq!(st.state.status, "idle");
}

#[test]
fn track_end_advances_and_sleep_track_stops() {
    if fake_mpv().is_none() {
        return;
    }
    let e = env();
    let a = &e.app;
    let xs = vec![track("a", "Al"), track("b", "Al")];
    a.set_play_context(xs.clone(), 0);
    a.play(xs[0].clone(), None, true);
    a.on_track_end();
    assert!(wait_for(5.0, || a.current().is_some_and(|t| t.id == "b")));
    a.sleep_set("track", None);
    assert_eq!(a.state().sleep_mode, "track");
    a.on_track_end();
    assert!(wait_for(5.0, || a.state().playback_state.state == "stopped"));
    assert_eq!(a.current().unwrap().id, "b");
    assert_eq!(a.state().sleep_mode, "off");
    // A timed sleep fires through the ticker.
    a.play(xs[0].clone(), None, true);
    a.sleep_set("1", None);
    a.state().sleep_deadline = crate::paths::now() + 0.3;
    assert!(wait_for(4.0, || a.state().playback_state.state == "stopped"));
    assert_eq!(a.state().sleep_mode, "off");
}

#[test]
fn async_start_generation_guard() {
    if fake_mpv().is_none() {
        return;
    }
    let e = env();
    let a = &e.app;
    let (r, ui) = rec();
    // Two quick requests: only the newest commits.
    a.play(track("old", "A"), Some(&ui), false);
    a.play(track("new", "A"), Some(&ui), false);
    assert!(wait_for(5.0, || a.current().is_some_and(|t| t.id == "new")));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(a.current().unwrap().id, "new");
    let hist: Vec<String> =
        a.db.history(10)
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect();
    assert_eq!(hist, vec!["new"]);
    assert!(r
        .r()
        .statuses
        .iter()
        .any(|s| s.starts_with("Playing · Song new")));
    assert!(a.ui().is_some());
    // Stop cancels an in-flight start: the old media stays authoritative.
    a.play(track("late", "A"), Some(&ui), false);
    a.stop_playback(Some(&ui));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(a.current().unwrap().id, "new");
    assert!(!a.playback_active());
}

#[test]
fn idle_engine_has_no_threads_after_stop() {
    if fake_mpv().is_none() {
        return;
    }
    let e = env();
    let a = &e.app;
    let mut radio = Track::new("radio", "7", "Stream");
    radio.kind = "radio".into();
    radio.seekable = false;
    radio.url = "http://radio.invalid/stream".into();
    a.play(radio, None, false);
    assert!(wait_for(2.0, || engine_threads()
        .iter()
        .any(|t| t.starts_with("nplay-radio"))));
    assert!(wait_for(2.0, || a.state().radio_info.contains_key("7")));
    a.stop_and_clear(None);
    // Adapted test_idle_wakeups: once stopped the engine keeps no threads
    // and nothing wakes up periodically.
    assert!(
        wait_for(3.0, || engine_threads().is_empty()),
        "{:?}",
        engine_threads()
    );
    let gen = *a.waker.gen.lock().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(engine_threads().is_empty());
    assert_eq!(*a.waker.gen.lock().unwrap(), gen);
    assert!(!a.state().ticker_running);
}

// ---- commands and UI helpers ----------------------------------------------------------

#[test]
fn command_prompt() {
    let e = env();
    let a = &e.app;
    let (r, ui) = rec();
    assert_eq!(COMMANDS.len(), 29);
    assert_eq!(
        command_suggestions("qu"),
        vec!["queue", "queue-clear", "queue-save"]
    );
    assert_eq!(
        command_suggestions("gapless o"),
        vec!["gapless on", "gapless off"]
    );
    a.command("bogus", &ui);
    assert_eq!(r.last_status(), UNKNOWN_COMMAND);
    a.command("view artwork", &ui);
    assert_eq!(r.r().layout, "artwork");
    assert_eq!(a.cfg.get("now_playing_view", ""), "artwork");
    a.command("view sideways", &ui);
    assert_eq!(r.last_status(), "View · normal | artwork | visualizer");
    a.command("normalize album", &ui);
    assert_eq!(a.cfg.get("normalization_mode", ""), "album");
    a.command("normalize off", &ui);
    assert!(!a.cfg.getbool("normalization", true));
    a.command("gapless on", &ui);
    assert_eq!(r.last_status(), "Gapless · on · applies from next track");
    a.command("sleep 30", &ui);
    assert_eq!(r.last_status(), "Sleep · 30 minutes");
    assert_eq!(a.state().sleep_mode, "time");
    a.command("sleep nonsense", &ui);
    assert_eq!(r.last_status(), "Sleep · minutes | track | album | off");
    a.command("sleep off", &ui);
    assert_eq!(a.state().sleep_mode, "off");
    a.command("shuffle", &ui);
    assert_eq!(r.last_status(), "Shuffle · Shuffle");
    a.command("repeat", &ui);
    assert_eq!(r.last_status(), "Repeat · Track");
    a.command("rating x", &ui);
    assert_eq!(r.last_status(), "Rating · 0–5");
    a.state().current = Some(track("r", "R"));
    a.command("rating 4", &ui);
    assert_eq!(r.last_status(), "Rating · 4/5 · Song r");
    assert_eq!(a.db.rating_get("r", "local").unwrap(), 4);
    a.command("browse", &ui);
    assert_eq!(r.r().view.title, "BROWSE");
    assert!(r.labels().contains(&"CONTINUE".to_string()));
    a.command("queue", &ui);
    assert_eq!(r.r().view.title, "QUEUE");
    a.command("scan", &ui);
    assert!(
        r.last_status().starts_with("Scan · 0 checked"),
        "{}",
        r.last_status()
    );
    a.command("settings", &ui);
    assert_eq!(r.r().view.title, "SETTINGS");
    a.open_source("settings:playback", &ui);
    assert!(r.labels().contains(&"GAPLESS".to_string()));
    a.open_source("settings:toggle-gapless", &ui);
    assert!(!a.cfg.getbool("gapless", true));
}

#[test]
fn queue_prompts_and_playlists() {
    let e = env();
    let a = &e.app;
    let (r, ui) = rec();
    a.queue_add(track("1", "A"), Some(&ui));
    a.queue_add(track("2", "A"), Some(&ui));
    assert_eq!(r.last_status(), "Queued · Song 2");
    assert_eq!(a.queue_move(0, 1, Some(&ui)), 1);
    assert_eq!(a.queue()[0].id, "2");
    r.r().answers.push_back(Some("Mix".into()));
    a.queue_save_playlist(Some(&ui));
    assert_eq!(r.last_status(), "Saved queue · Mix · 2 tracks");
    let pls = a.db.playlists().unwrap();
    assert_eq!(pls.len(), 1);
    a.queue_remove(0, Some(&ui));
    assert_eq!(a.queue().len(), 1);
    a.queue_clear(Some(&ui));
    assert!(a.queue().is_empty());
    assert_eq!(r.r().view.title, "QUEUE");
    // Add station: name, then URL.
    r.r().answers.extend([
        Some("Jazz".into()),
        Some("https://radio.example/jazz".into()),
    ]);
    a.add_radio(&ui);
    assert_eq!(r.last_status(), "Station saved · Jazz");
    assert_eq!(r.r().view.title, "RADIO");
    // Cancelled prompts.
    a.add_radio(&ui);
    assert_eq!(r.last_status(), "Add station cancelled");
    // Actions menu target and ctx actions.
    a.action_menu(&ui, Some(track("t", "T")));
    assert!(r.r().view.title.starts_with("ACTIONS · Song t"));
    assert!(r.labels().contains(&"START NPLAY RADIO".to_string()));
    a.open_source("ctx:queue", &ui);
    assert_eq!(a.queue()[0].id, "t");
    a.open_source("ctx:after-context", &ui);
    assert_eq!(a.state().later_queue[0].id, "t");
    a.open_source("ctx:rate5", &ui);
    assert_eq!(a.db.rating_get("t", "local").unwrap(), 5);
}

#[test]
fn discovery_from_radio_metadata() {
    let e = env();
    let a = &e.app;
    let (r, ui) = rec();
    a.save_discovery(Some(&ui));
    assert_eq!(r.last_status(), "Discovery · nothing playing");
    let mut radio = Track::new("sr", "132", "P1");
    radio.kind = "radio".into();
    a.state().current = Some(radio);
    let mut info = Map::new();
    info.insert("song".into(), json!("Kraftwerk - Radioactivity"));
    a.state().radio_info.insert("132".into(), info);
    a.save_discovery(Some(&ui));
    assert_eq!(
        r.last_status(),
        "Saved discovery · Kraftwerk · Radioactivity"
    );
    let d = a.db.discoveries(10).unwrap();
    assert_eq!(d.len(), 1);
}

#[test]
fn activate_sets_context_and_random_play_shuffles() {
    if fake_mpv().is_none() {
        return;
    }
    let e = env();
    let a = &e.app;
    let (_r, ui) = rec();
    let mut album = Track::new("navidrome", "al", "Album");
    album.kind = "album".into();
    let rows = vec![
        Row::header("TRACKS"),
        Row::Track(track("a", "X")),
        Row::Track(album),
        Row::Track(track("b", "X")),
    ];
    a.activate(&ui, "LIST", &rows, 3);
    {
        let st = a.state();
        assert_eq!(st.play_context.len(), 2);
        assert_eq!(st.context_index, 1);
    }
    assert!(wait_for(5.0, || a.current().is_some_and(|t| t.id == "b")));
    a.random_play(&ui, "LIST", &rows);
    assert_eq!(a.state().play_context.len(), 2);
}

#[test]
fn unicode_prompt_helpers() {
    for ch in "åäöÅÄÖ".chars() {
        assert!(printable_input(ch));
        assert_eq!(cell_width(&ch.to_string()), 1);
    }
    for ch in ['\x1b', '\t', '\r', '\n', '\x7f'] {
        assert!(!printable_input(ch));
    }
    assert_eq!(cell_width("Örebro"), 6);
    assert_eq!(cell_width("漢字"), 4);
    assert_eq!(cell_width("e\u{301}"), 1);
}

#[test]
fn quick_find_merges_cross_source_matches() {
    let mut groups = HashMap::new();
    let mut l = Track::new("local", "1", "Havenless");
    l.artist = "Enslaved".into();
    let mut n = Track::new("navidrome", "9", "Havenless!");
    n.artist = "ENSLAVED".into();
    let mut solo = Track::new("navidrome", "8", "Other");
    solo.artist = "X".into();
    groups.insert("LOCAL".to_string(), vec![l]);
    groups.insert("NAVIDROME".to_string(), vec![n, solo]);
    let m = crate::services::search::merge_matches(&groups, "navidrome");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].source, "navidrome");
    assert_eq!(
        m[0].meta["available_sources"],
        json!(["local", "navidrome"])
    );
    assert_eq!(
        m[0].meta["source_alternatives"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn quick_find_publishes_local_results() {
    let e = env();
    let a = &e.app;
    a.cfg.set("sr_enabled", "false");
    a.cfg.set("youtube_enabled", "false");
    a.cfg.set("navidrome_enabled", "false");
    let (r, ui) = rec();
    a.search_async("nothing-here", &ui);
    let v = r.r().view.clone();
    assert_eq!(v.title, "QUICK FIND · nothing-here · 0 RESULTS");
    assert_eq!(r.r().sort, "relevance");
}

#[test]
fn doctor_report_lines() {
    let e = env();
    let lines = e
        .app
        .doctor_lines("VIS: CAVA, 0 subscribers · terminal off · service not running");
    assert!(lines[0].starts_with("NPLAY "));
    assert!(lines.iter().any(|l| l.starts_with("mpv          ")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with("library      0 indexed tracks · schema 5")));
    assert!(lines
        .iter()
        .any(|l| l == "visualizer   VIS: CAVA, 0 subscribers · service not running"));
    assert!(lines.iter().any(|l| l.starts_with("privacy ")));
}

// ---- test_statistics_ui.py / test_statistics_polish.py ---------------------------------------

fn stats_env() -> (Env, Arc<RecUi>, UiRef) {
    let e = env();
    let mut t = Track::new("navidrome", "1", "Havenless");
    t.artist = "Enslaved".into();
    t.album = "Below the Lights".into();
    e.app
        .statistics
        .add("session", crate::paths::now(), 3.0, &t, None)
        .unwrap();
    let (r, u) = rec();
    (e, r, u)
}

#[test]
fn stats_overview_details_settings_and_disabled() {
    let (e, r, ui) = stats_env();
    let a = &e.app;
    a.stats_home(&ui, "all", true);
    let labels = r.labels();
    assert!(
        labels.contains(&"ACTUAL LISTENING · 0m 03s".to_string()),
        "{labels:?}"
    );
    assert!(labels.iter().any(|l| l.contains("Enslaved · 0m 03s")));
    assert!(labels.iter().any(|l| l.contains("Havenless · 0m 03s")));
    assert!(!labels.contains(&"INSTALLATION ID".to_string()));
    let depth = r.r().stack.len();
    a.stats_home(&ui, "day", false);
    assert_eq!(r.r().stack.len(), depth);
    a.stats_details(&ui, "artists", Some("all"), true);
    assert!(r.labels().iter().any(|l| l.contains("Enslaved")));
    a.stats_settings(&ui, true);
    let labels = r.labels();
    assert!(labels.contains(&"SYNC · DISABLED".to_string()));
    assert!(labels.contains(&"COPY INSTALLATION ID".to_string()));
    a.open_source("stats:sync-toggle", &ui);
    assert_eq!(
        r.last_status(),
        "Configure a dedicated token first: nplay --stats-set-token"
    );
    a.cfg.set("statistics_enabled", "false");
    a.stats_home(&ui, "week", true);
    assert!(r.labels().contains(&"ENABLE LOCAL STATISTICS".to_string()));
    a.open_source("stats:toggle", &ui);
    assert!(a.cfg.getbool("statistics_enabled", false));
    assert_eq!(r.last_status(), "Local statistics enabled · sync disabled");
    assert_eq!(App::stats_time(3725.0), "1h 02m");
    assert_eq!(App::stats_time(3600.0 * 1234.0), "1,234h 00m");
}

#[test]
fn stats_refresh_preserves_selection_and_stack() {
    let (e, r, ui) = stats_env();
    let a = &e.app;
    a.stats_home(&ui, "week", true);
    let sel = r
        .r()
        .view
        .rows
        .iter()
        .position(|x| x.action() == "stats:period:year")
        .unwrap();
    r.r().view.sel = sel;
    let depth = r.r().stack.len();
    r.r().invalidated = false;
    assert!(a.refresh_statistics_view(&ui));
    let v = r.r().view.clone();
    assert_eq!(v.rows[v.sel].action(), "stats:period:year");
    assert_eq!(r.r().stack.len(), depth);
    assert!(r.r().invalidated);
    // Details keep navigation history.
    a.stats_details(&ui, "artists", Some("all"), true);
    let depth = r.r().stack.len();
    assert!(a.refresh_statistics_view(&ui));
    assert_eq!(r.r().stack.len(), depth);
    assert!(r.r().view.title.contains("ARTISTS"));
    // Period rows carry no repeated helper text.
    a.stats_home(&ui, "day", true);
    assert!(r
        .r()
        .view
        .rows
        .iter()
        .filter(|x| x.action().starts_with("stats:period:"))
        .all(|x| matches!(x, Row::Entry(e) if e.detail.is_empty())));
    // Other menus are left alone.
    r.r().view.title = "PLAYLISTS".into();
    assert!(!a.refresh_statistics_view(&ui));
}

#[test]
fn spotify_play_failure_restores_session() {
    let e = env();
    let a = &e.app;
    let prev = track("keep", "K");
    {
        let mut st = a.state();
        st.current = Some(prev.clone());
        st.resume_candidate = true;
    }
    let (r, ui) = rec();
    // Not connected (and no cached librespot credentials): fails fast,
    // never touches Now Playing, and reports through ui.error.
    a.play(Track::new("spotify", "sp1", "Remote"), Some(&ui), false);
    assert!(wait_for(20.0, || !r.r().errors.is_empty()));
    let (title, _msg) = r.r().errors[0].clone();
    assert_eq!(title, "Spotify playback unavailable");
    let st = a.state();
    assert_eq!(st.current.as_ref().unwrap().id, "keep");
    assert!(st.spotify_pending_track_id.is_empty());
    assert!(st.resume_candidate);
    assert!(!r.r().statuses.is_empty());
}
