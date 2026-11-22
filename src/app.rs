//! The NPLAY controller (port of 1.x `nplay/app.py`, `nplay/core/playback.py`
//! and the `:` command prompt). Navigation lives in `crate::navigation`,
//! search/settings/mpv start in `crate::services`. Owner: engine track.
//!
//! # Threading model
//! `App` is shared as `Arc<App>`. Every method takes `&self` (most take
//! `self: &Arc<Self>` so they can hand the app to worker threads). Mutable
//! session state lives in one `Mutex<State>` ([`App::state`]); the lock is
//! never held while the app calls the player, a provider, the network, the
//! database or the [`Ui`], so callbacks can re-enter the app freely. Callers
//! (TUI, service) may lock [`App::state`] to read fields, but must drop the
//! guard before calling any `App` method.
//!
//! Background threads exist only while needed and block (condvar waits, no
//! busy polling): the playback ticker (checkpoint + statistics every 2 s,
//! sleep timer, statistics sync every 60 s) runs only while something plays
//! or a timed sleep is armed; the Spotify state poller only while Spotify
//! plays (positions are interpolated locally between polls); the radio
//! metadata poller only while a radio stream plays. With nothing playing the
//! engine has no running threads of its own.
//!
//! # The UI boundary: [`Ui`]
//! The engine talks to "a UI" through the object-safe trait [`Ui`] (`Send +
//! Sync`, shared as [`UiRef`] = `Arc<dyn Ui>`). Every method has a default
//! (no-op, or a neutral value), so the headless service implements only what
//! it needs and [`NullUi`] is empty. A ratatui TUI implements all of it.
//!
//! Contract for implementors:
//! - `post(task)`: run `task` on the UI thread. The TUI queues it to its
//!   event loop and wakes the loop; the headless service runs it inline (the
//!   default). Worker threads always deliver results through `post`.
//! - `status(msg)`: the one-line message. `error(title, msg)`: an error view
//!   (TUI) or a client notification (service). `invalidate()`: redraw soon.
//! - Views (all take plain data, [`Row`]s are owned and cloneable):
//!   `show_menu(title, rows, push)` (1.x `show_menu`; `push` saves the
//!   current view on the back stack first), `show_list(title, rows, empty,
//!   message, push)` (`message` None means "`<n> items`"), `replace_list(title,
//!   rows, empty, message)` (replaces the current view without pushing; used
//!   after a `loading` placeholder), `loading(title, msg, rows)`,
//!   `show_empty(title, msg)`, `update_items(rows, sel)` (same view, new rows
//!   and selection, used by queue/playlist reordering), `set_title`, `push`
//!   (save current view), `now_playing` (go to the Now Playing home view),
//!   `show_info(track)` (1.x "context" information view), `show_about`,
//!   `show_diagnostics`, `show_help`.
//! - `prompt(label, initial, secret, done)`: ask for one line of text and call
//!   `done(Some(text))` (trimmed) or `done(None)` on Esc. It may call `done`
//!   synchronously (headless: `done(None)`) or later on the UI thread.
//!   Several engine flows chain prompts (Navidrome setup, add station).
//! - Appearance state the engine reads/changes: `theme()`, `theme_label()`,
//!   `theme_menu()`, `set_theme(name)` (aliases allowed, saves `theme`),
//!   `reload_theme()`, `layout()`/`set_layout(name)`/`cycle_layout()`
//!   (`normal|artwork|visualizer`), `visualizer_on()`/`toggle_visualizer()`,
//!   `visual_style()`/`set_visual_style()`/`visualizer_style_menu()`,
//!   `set_sort_mode(relevance|newest|oldest)`, `prefetch_art(url)`.
//! - `view()`: the visible view ([`View`]: mode, title, selection, rows) or
//!   None; with `set_selection(i)` and `edge_selection(last)` the engine uses
//!   it to refresh an open statistics menu in place
//!   ([`App::refresh_statistics_view`], call it every ~10 s while a
//!   `STATISTICS` menu is visible).
//! - `quit()`: leave the UI loop (MPRIS Quit).
//!
//! Rows: [`Row::Entry`] is a 1.x menu tuple `(label, action, detail)`; an
//! empty action makes it a non-selectable header. [`Row::Track`] is a media
//! item. Activating a row: call [`App::activate`] (it handles `theme:` via
//! `Ui::set_theme`, `nav:` via `nav_action`, any other action through
//! [`App::open_source`], and tracks like 1.x `UI.activate`). Other UI-side
//! helpers ported from 1.x `UI`: [`App::action_menu`] (the `.` menu),
//! [`App::random_play`] (`r`), [`App::nav_setup`] (Navidrome setup prompts),
//! [`App::command`] (the `:` prompt; [`COMMANDS`] lists the 29 completions,
//! [`command_suggestions`] the argument completions).
//!
//! # Public API (summary)
//! - Construction: [`App::new`] (real config/data dirs, starts MPRIS when
//!   `mpris_enabled`), [`App::with_options`] ([`AppOptions`]: mpv binary,
//!   no MPRIS, ... for tests). [`App::set_ui`]/[`App::ui`].
//! - Handles (pub fields): `cfg`, `db`, `statistics`, `statistics_tracker`,
//!   `statistics_sync`, `player`, `sr`, `yt`, `spotify`, `artwork_cache`,
//!   `notifier`, `events`, `session`, `mpris()`.
//! - Session: `load_state`, `save_state`, `checkpoint(pos, volume)`,
//!   `hydrate_resume_artwork`, `scan`, `background_library_refresh`, `roots`.
//! - Playback (port of `PlaybackController` too): `play`, `play_with`,
//!   `play_pause`, `stop_playback`, `stop_and_clear`, `next_track`,
//!   `previous_track`, `on_track_end`, `playback_active`, `playback_playing`,
//!   `playback_position`, `playback_duration`, `playback_seek_relative`,
//!   `playback_volume`, `playback_set_volume`, `playback_mute`, `up_next`,
//!   `set_play_context`, `cycle_shuffle`, `cycle_repeat`,
//!   `normalization_mode`, `sleep_set`, `sleep_check`, `snapshot`
//!   ([`PlaybackState`] with live position).
//! - Library actions: `queue_add`, `queue_next`, `queue_remove`, `queue_move`,
//!   `queue_play`, `queue_clear`, `queue_save_playlist`, `set_rating`,
//!   `save_discovery`, `favorite_toggle`.
//! - Statistics views: `stats_home`, `stats_details`, `stats_settings`,
//!   `statistics_toggle`, `statistics_sync_toggle`, `statistics_sync_now`,
//!   `refresh_statistics_view`, `stats_time`; `smart_home`.
//! - Spotify: `spotify_configured`, `spotify_poll_start` (internal polling).
//! - CLI: `doctor_lines(vis_line)`, `doctor(vis_line) -> exit code`,
//!   `shutdown()`, `scan_message(report)`; `main()` stays with the coordinator.
//!
//! Events emitted on `events` (payload keys): `playback-state-changed`
//! (`state`), `media-changed` (`track`, `state`), `integration-state-changed`
//! (`properties`). MPRIS `changed()` is called on every state change.

use crate::artwork_cache::ArtworkCache;
use crate::config::Config;
use crate::core::{EventBus, PlaybackState, Result, SessionStore};
use crate::db::DB;
use crate::model::{value_str, Meta, Track};
use crate::mpris::{Mpris, MprisControl, MprisState};
use crate::notifications::Notifier;
use crate::player::Player;
use crate::providers::{Navidrome, YouTube, SR};
use crate::services::playback_start::MpvStartCoordinator;
use crate::spotify::Spotify;
use crate::statistics::{ListeningTracker, StatisticsStore, StatisticsSync};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

mod command;
mod playback;
mod spotify;
mod stats;
#[cfg(test)]
mod tests;

pub use command::{command_suggestions, COMMANDS, UNKNOWN_COMMAND};
pub use stats::thousands;

// ---------------------------------------------------------------------------
// UI boundary
// ---------------------------------------------------------------------------

/// A closure to run on the UI thread.
pub type Task = Box<dyn FnOnce() + Send + 'static>;
/// Prompt completion: `Some(text)` (trimmed) or `None` when cancelled.
pub type PromptDone = Box<dyn FnOnce(Option<String>) + Send + 'static>;
/// Shared UI handle.
pub type UiRef = Arc<dyn Ui>;

/// A 1.x menu tuple `(label, action, detail)`. Empty `action` = header.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MenuEntry {
    pub label: String,
    pub action: String,
    pub detail: String,
}

/// One row of a list or menu view.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Row {
    Entry(MenuEntry),
    Track(Track),
}

impl Row {
    pub fn entry(
        label: impl Into<String>,
        action: impl Into<String>,
        detail: impl Into<String>,
    ) -> Row {
        Row::Entry(MenuEntry {
            label: label.into(),
            action: action.into(),
            detail: detail.into(),
        })
    }

    /// A non-selectable section header.
    pub fn header(label: impl Into<String>) -> Row {
        Row::entry(label, "", "")
    }

    /// 1.x `UI.selectable`: tracks always, entries only with an action.
    pub fn selectable(&self) -> bool {
        match self {
            Row::Entry(e) => !e.action.is_empty(),
            Row::Track(_) => true,
        }
    }

    /// The entry's action ("" for tracks and headers).
    pub fn action(&self) -> &str {
        match self {
            Row::Entry(e) => &e.action,
            Row::Track(_) => "",
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Row::Entry(e) => &e.label,
            Row::Track(t) => &t.title,
        }
    }

    pub fn track(&self) -> Option<&Track> {
        match self {
            Row::Track(t) => Some(t),
            Row::Entry(_) => None,
        }
    }

    /// The text 1.x `filter_current` matches against.
    pub fn filter_text(&self) -> String {
        match self {
            Row::Entry(e) => format!("{} {} {}", e.label, e.action, e.detail),
            Row::Track(t) => format!(
                "{} {} {} {}",
                t.title,
                t.artist,
                t.album,
                t.meta_str("published_date")
            ),
        }
    }

    /// A track that can join a play context (1.x `playable` in `UI.activate`).
    pub fn playable(&self) -> Option<&Track> {
        self.track()
            .filter(|t| !matches!(t.kind.as_str(), "album" | "artist" | "playlist" | "program"))
    }
}

impl From<Track> for Row {
    fn from(t: Track) -> Row {
        Row::Track(t)
    }
}

/// Tracks as rows.
pub fn track_rows(xs: Vec<Track>) -> Vec<Row> {
    xs.into_iter().map(Row::Track).collect()
}

/// DB menu tuples as rows.
pub fn entry_rows(xs: Vec<crate::db::Row3>) -> Vec<Row> {
    xs.into_iter()
        .map(|(a, b, c)| Row::entry(a, b, c))
        .collect()
}

/// What the UI currently shows (for in-place refreshes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// `home`, `list`, `menu`, `error`, `help`, `about`, `diagnostics`, `context`.
    pub mode: String,
    pub title: String,
    pub sel: usize,
    pub rows: Vec<Row>,
}

/// Everything the engine asks of a user interface. See the module docs.
pub trait Ui: Send + Sync {
    fn post(&self, task: Task) {
        task()
    }
    fn status(&self, _msg: &str) {}
    fn error(&self, _title: &str, _msg: &str) {}
    fn invalidate(&self) {}
    fn now_playing(&self) {}
    fn show_menu(&self, _title: &str, _rows: Vec<Row>, _push: bool) {}
    fn show_list(
        &self,
        _title: &str,
        _rows: Vec<Row>,
        _empty: &str,
        _message: Option<&str>,
        _push: bool,
    ) {
    }
    fn replace_list(&self, _title: &str, _rows: Vec<Row>, _empty: &str, _message: Option<&str>) {}
    fn loading(&self, _title: &str, _msg: &str, _rows: Vec<Row>) {}
    fn show_empty(&self, title: &str, msg: &str) {
        self.replace_list(title, Vec::new(), msg, Some(msg))
    }
    fn update_items(&self, _rows: Vec<Row>, _sel: usize) {}
    fn set_title(&self, _title: &str) {}
    fn prompt(&self, _label: &str, _initial: &str, _secret: bool, done: PromptDone) {
        done(None)
    }
    fn push(&self) {}
    fn show_info(&self, _track: &Track) {}
    fn show_about(&self) {}
    fn show_diagnostics(&self) {}
    fn show_help(&self) {}
    fn theme(&self) -> String {
        "niru-noir".into()
    }
    fn theme_label(&self) -> String {
        let t = self.theme();
        crate::theme::available()
            .into_iter()
            .find(|(k, _)| *k == t)
            .map(|(_, l)| l)
            .unwrap_or(t)
    }
    fn theme_menu(&self) {}
    fn set_theme(&self, _name: &str) {}
    fn reload_theme(&self) {}
    fn layout(&self) -> String {
        "normal".into()
    }
    fn set_layout(&self, _layout: &str) {}
    fn cycle_layout(&self) {}
    fn visualizer_on(&self) -> bool {
        false
    }
    fn toggle_visualizer(&self) {}
    fn visual_style(&self) -> String {
        "classic".into()
    }
    fn set_visual_style(&self, _style: &str) {}
    fn visualizer_style_menu(&self) {}
    fn set_sort_mode(&self, _mode: &str) {}
    fn prefetch_art(&self, _url: &str) {}
    fn view(&self) -> Option<View> {
        None
    }
    fn set_selection(&self, _index: usize) {}
    fn edge_selection(&self, _last: bool) {}
    fn quit(&self) {}
}

/// A UI that does nothing (tests, one-shot CLI commands).
#[derive(Default)]
pub struct NullUi;

impl Ui for NullUi {}

pub(crate) fn say(ui: Option<&UiRef>, msg: &str) {
    if let Some(u) = ui {
        u.status(msg);
    }
}

/// Python `str.capitalize()`.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// `source:id` of an optional track ("" for none).
pub fn track_key(t: Option<&Track>) -> String {
    t.map(Track::key).unwrap_or_default()
}

/// 1.x `ui_common.clip`.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// 1.x `spotify_state` (one `/me/player` snapshot plus local bookkeeping).
#[derive(Clone, Debug, Default)]
pub struct SpotifyState {
    pub track: Option<Track>,
    pub is_playing: bool,
    /// None = key absent (falls back to `resume_position`).
    pub progress: Option<f64>,
    pub duration: f64,
    pub device: Meta,
    /// Monotonic time of the snapshot (`_seen`), for interpolation.
    pub seen: Option<f64>,
    pub pending: bool,
    pub context: Meta,
}

impl SpotifyState {
    pub fn from_player(st: &crate::spotify::PlayerState) -> SpotifyState {
        SpotifyState {
            track: st.track.clone(),
            is_playing: st.is_playing,
            progress: Some(st.progress),
            duration: st.duration,
            device: st.device.clone(),
            seen: Some(crate::paths::monotonic()),
            pending: false,
            context: st.context.clone(),
        }
    }

    pub fn device_id(&self) -> String {
        self.device
            .get("id")
            .map(value_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

/// 1.x `playback_state` dict: who owns playback right now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionInfo {
    pub generation: u64,
    pub source: String,
    pub transport: String,
    /// `starting`, `playing`, `paused`, `stopped`, or "" (cleared).
    pub state: String,
    pub track_id: String,
    pub artwork: String,
    pub context_uri: String,
    /// Spotify only: `pending` until the Web API confirms, then `confirmed`.
    pub spotify_phase: String,
}

impl SessionInfo {
    pub fn to_value(&self) -> Value {
        if *self == SessionInfo::default() {
            return json!({});
        }
        json!({
            "generation": self.generation, "source": self.source,
            "transport": self.transport, "state": self.state,
            "track_id": self.track_id, "artwork": self.artwork,
            "context_uri": self.context_uri, "spotify_phase": self.spotify_phase,
        })
    }
}

/// The context frozen for one Spotify play request.
#[derive(Clone, Debug, Default)]
pub struct SpotifyContext {
    pub tracks: Vec<Track>,
    pub original: Vec<Track>,
    pub index: i64,
    pub context_uri: String,
}

/// All mutable session state (lock with [`App::state`]).
#[derive(Clone, Debug)]
pub struct State {
    pub current: Option<Track>,
    pub resume_candidate: bool,
    pub resume_position: f64,
    pub volume: f64,
    pub queue: Vec<Track>,
    pub later_queue: Vec<Track>,
    pub play_context: Vec<Track>,
    pub original_context: Vec<Track>,
    pub context_index: i64,
    pub back_stack: Vec<Track>,
    pub shuffle_mode: String,
    pub repeat_mode: String,
    pub spotify_state: SpotifyState,
    pub spotify_polling: bool,
    pub spotify_pending_track_id: String,
    pub spotify_track_generation: u64,
    pub playback_generation: u64,
    pub playback_request: u64,
    pub playback_state: SessionInfo,
    /// Live radio metadata by track id (`program`, `episode`, `song`, ...).
    pub radio_info: HashMap<String, Map<String, Value>>,
    /// Wall-clock deadline of a timed sleep (0 = none).
    pub sleep_deadline: f64,
    /// `off`, `time`, `track` or `album`.
    pub sleep_mode: String,
    pub play_event_id: Option<i64>,
    pub play_event_started: f64,
    pub play_event_listened: f64,
    pub play_event_tick: f64,
    pub radio_poll_generation: u64,
    pub radio_polling: bool,
    /// Canonical snapshot for integrations.
    pub state: PlaybackState,
    pub stats_period: String,
    /// Target of the Actions menu (1.x `ui.action_target`).
    pub action_target: Option<Track>,
    pub source_switch_tracks: HashMap<String, Track>,
    pub spotify_device_names: HashMap<String, String>,
    pub spotify_search_groups: HashMap<String, Vec<Track>>,
    /// The local playlist shown in the current list (1.x `ui.current_playlist_pid`).
    pub current_playlist_pid: Option<i64>,
    pub ticker_running: bool,
}

impl State {
    fn new(shuffle: String, repeat: String) -> State {
        State {
            current: None,
            resume_candidate: false,
            resume_position: 0.0,
            volume: 100.0,
            queue: Vec::new(),
            later_queue: Vec::new(),
            play_context: Vec::new(),
            original_context: Vec::new(),
            context_index: -1,
            back_stack: Vec::new(),
            shuffle_mode: shuffle,
            repeat_mode: repeat,
            spotify_state: SpotifyState::default(),
            spotify_polling: false,
            spotify_pending_track_id: String::new(),
            spotify_track_generation: 0,
            playback_generation: 0,
            playback_request: 0,
            playback_state: SessionInfo::default(),
            radio_info: HashMap::new(),
            sleep_deadline: 0.0,
            sleep_mode: "off".into(),
            play_event_id: None,
            play_event_started: 0.0,
            play_event_listened: 0.0,
            play_event_tick: 0.0,
            radio_poll_generation: 0,
            radio_polling: false,
            state: PlaybackState::new(),
            stats_period: "week".into(),
            action_target: None,
            source_switch_tracks: HashMap::new(),
            spotify_device_names: HashMap::new(),
            spotify_search_groups: HashMap::new(),
            current_playlist_pid: None,
            ticker_running: false,
        }
    }

    pub fn is_spotify(&self) -> bool {
        self.current.as_ref().is_some_and(|t| t.source == "spotify")
    }
}

/// A wake-up signal for the engine's background loops: they sleep on it and
/// re-check their ownership conditions when it fires (stop, new playback).
#[derive(Default)]
pub(crate) struct Waker {
    gen: Mutex<u64>,
    cv: Condvar,
}

impl Waker {
    pub fn wake(&self) {
        let mut g = self.gen.lock().unwrap_or_else(|e| e.into_inner());
        *g = g.wrapping_add(1);
        self.cv.notify_all();
    }

    /// The current wake count; pass it to `sleep_since`.
    pub fn token(&self) -> u64 {
        *self.gen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sleep up to `secs`, returning early when woken after `token` was taken
    /// (a wake between the caller's checks and this call is never lost).
    pub fn sleep_since(&self, token: u64, secs: f64) {
        let g = self.gen.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .cv
            .wait_timeout_while(g, Duration::from_secs_f64(secs.max(0.0)), |x| *x == token);
    }
}

/// Construction options (tests and special front-ends).
#[derive(Clone, Default)]
pub struct AppOptions {
    /// mpv executable (default `mpv`; tests use tools/fake-mpv).
    pub mpv_binary: Option<String>,
    /// mpv log path override.
    pub mpv_log: Option<std::path::PathBuf>,
    /// Never start MPRIS, whatever `mpris_enabled` says.
    pub no_mpris: bool,
    /// `notify-send` override (Some(None) disables notifications).
    pub notify_binary: Option<Option<std::path::PathBuf>>,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub cfg: Config,
    pub db: DB,
    pub statistics: StatisticsStore,
    pub statistics_tracker: ListeningTracker,
    pub statistics_sync: StatisticsSync,
    pub player: Player,
    pub sr: SR,
    pub yt: YouTube,
    pub spotify: Spotify,
    pub artwork_cache: ArtworkCache,
    pub notifier: Notifier,
    pub events: EventBus,
    pub session: SessionStore,
    pub(crate) mpv_start: MpvStartCoordinator,
    mpris: OnceLock<Mpris>,
    state: Mutex<State>,
    ui: Mutex<Option<UiRef>>,
    pub(crate) waker: Waker,
    me: Weak<App>,
}

struct MprisBridge(Weak<App>);

impl MprisControl for MprisBridge {
    fn play_pause(&self) {
        if let Some(a) = self.0.upgrade() {
            a.play_pause(a.ui().as_ref());
        }
    }
    fn next(&self) {
        if let Some(a) = self.0.upgrade() {
            a.next_track(a.ui().as_ref(), false);
        }
    }
    fn previous(&self) {
        if let Some(a) = self.0.upgrade() {
            a.previous_track(a.ui().as_ref());
        }
    }
    fn stop(&self) {
        if let Some(a) = self.0.upgrade() {
            a.stop_and_clear(a.ui().as_ref());
        }
    }
    fn seek_relative(&self, secs: f64) {
        if let Some(a) = self.0.upgrade() {
            a.playback_seek_relative(secs);
        }
    }
    fn volume_relative(&self, delta: f64) {
        if let Some(a) = self.0.upgrade() {
            a.playback_volume(delta);
        }
    }
    fn quit(&self) {
        if let Some(a) = self.0.upgrade() {
            if let Some(u) = a.ui() {
                u.quit();
            }
        }
    }
    fn snapshot(&self) -> MprisState {
        match self.0.upgrade() {
            Some(a) => a.mpris_state(),
            None => MprisState::default(),
        }
    }
}

impl App {
    /// The real application: XDG config, library, statistics, session; starts
    /// MPRIS when `mpris_enabled` (default on).
    pub fn new() -> Result<Arc<App>> {
        App::with_options(AppOptions::default())
    }

    pub fn with_options(opts: AppOptions) -> Result<Arc<App>> {
        let cfg = Config::load();
        let db = DB::new()?;
        let statistics = StatisticsStore::new()?;
        let tracker =
            ListeningTracker::new(statistics.clone(), cfg.getbool("statistics_enabled", true));
        let sync = StatisticsSync::new(statistics.clone(), cfg.clone());
        let player = Player::new(None);
        if let Some(b) = &opts.mpv_binary {
            player.set_binary(b.clone());
        }
        if let Some(p) = &opts.mpv_log {
            player.set_log_path(p.clone());
        }
        let cache_mb = cfg
            .get("artwork_cache_mb", "500")
            .trim()
            .parse::<i64>()
            .unwrap_or(500);
        let artwork_cache = ArtworkCache::new(if cache_mb == 0 { 500 } else { cache_mb });
        let notifier = Notifier::new(cfg.clone(), Some(artwork_cache.clone()));
        if let Some(b) = &opts.notify_binary {
            notifier.set_binary(b.clone());
        }
        let spotify = Spotify::new(&cfg);
        let st = State::new(
            cfg.get("shuffle_mode", "off"),
            cfg.get("repeat_mode", "off"),
        );
        let session = SessionStore::new(crate::paths::state().join("session.json"));
        let app = Arc::new_cyclic(|me: &Weak<App>| App {
            cfg,
            db,
            statistics,
            statistics_tracker: tracker,
            statistics_sync: sync,
            player,
            sr: SR::new(),
            yt: YouTube::new(),
            spotify,
            artwork_cache,
            notifier,
            events: EventBus::new(),
            session,
            mpv_start: MpvStartCoordinator::default(),
            mpris: OnceLock::new(),
            state: Mutex::new(st),
            ui: Mutex::new(None),
            waker: Waker::default(),
            me: me.clone(),
        });
        let weak = Arc::downgrade(&app);
        app.player.set_on_end(Some(Arc::new(move || {
            if let Some(a) = weak.upgrade() {
                a.on_track_end();
            }
        })));
        let mpris = Mpris::new(
            Arc::new(MprisBridge(Arc::downgrade(&app))),
            app.artwork_cache.clone(),
        );
        let _ = app.mpris.set(mpris);
        app.load_state();
        if !opts.no_mpris && app.cfg.getbool("mpris_enabled", true) {
            app.mpris().start();
        }
        Ok(app)
    }

    pub(crate) fn arc(&self) -> Arc<App> {
        self.me.upgrade().expect("App used after drop")
    }

    /// The session state. Never hold the guard while calling an `App` method.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn mpris(&self) -> &Mpris {
        self.mpris.get().expect("mpris initialised in constructor")
    }

    /// The UI most recently attached (1.x `self.ui`).
    pub fn ui(&self) -> Option<UiRef> {
        self.ui.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_ui(&self, ui: Option<UiRef>) {
        *self.ui.lock().unwrap_or_else(|e| e.into_inner()) = ui;
    }

    /// `ui` when given, else the attached UI.
    pub(crate) fn ui_or(&self, ui: Option<&UiRef>) -> Option<UiRef> {
        ui.cloned().or_else(|| self.ui())
    }

    // ---- convenience readers ------------------------------------------------

    pub fn current(&self) -> Option<Track> {
        self.state().current.clone()
    }

    pub fn queue(&self) -> Vec<Track> {
        self.state().queue.clone()
    }

    pub fn volume(&self) -> f64 {
        self.state().volume
    }

    pub fn shuffle_mode(&self) -> String {
        self.state().shuffle_mode.clone()
    }

    pub fn repeat_mode(&self) -> String {
        self.state().repeat_mode.clone()
    }

    pub fn current_playlist_pid(&self) -> Option<i64> {
        self.state().current_playlist_pid
    }

    pub fn set_action_target(&self, t: Option<Track>) {
        self.state().action_target = t;
    }

    pub fn radio_info(&self, id: &str) -> Map<String, Value> {
        self.state().radio_info.get(id).cloned().unwrap_or_default()
    }

    /// 1.x `fmt`: `m:ss`.
    pub fn fmt(&self, s: f64) -> String {
        let s = if s.is_finite() { s.max(0.0) as i64 } else { 0 };
        format!("{}:{:02}", s / 60, s % 60)
    }

    pub fn log_debug(&self, msg: &str, e: Option<&dyn std::fmt::Display>) {
        match e {
            Some(e) => debug!("{msg} · {e}"),
            None => debug!("{msg}"),
        }
    }

    pub fn roots(&self) -> Vec<String> {
        DB::roots(&self.cfg)
    }

    pub fn nav(&self) -> Option<Navidrome> {
        Navidrome::from_config(&self.cfg)
    }

    pub fn nav_configured(&self) -> bool {
        !self.cfg.get("navidrome_url", "").is_empty()
            && !self.cfg.get("navidrome_user", "").is_empty()
            && !self.cfg.nav_password().is_empty()
    }

    pub fn spotify_configured(&self) -> bool {
        self.spotify.connected()
    }

    // ---- session persistence -------------------------------------------------

    /// Restore `session.json` (same keys as 1.x).
    pub fn load_state(&self) {
        let d = self.session.load();
        let tracks = |k: &str| -> Vec<Track> {
            d.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Track::from_value).collect())
                .unwrap_or_default()
        };
        let mut current = d
            .get("current")
            .filter(|v| crate::model::value_bool(v))
            .and_then(Track::from_value);
        let mut queue = tracks("queue");
        let later = tracks("later_queue");
        let mut ctx = tracks("play_context");
        // Rebuild ephemeral Navidrome cover URLs from stable coverArt ids.
        let needs = current
            .iter()
            .chain(queue.iter())
            .chain(ctx.iter())
            .any(|t| {
                t.source == "navidrome" && t.cover.is_empty() && !t.meta_str("cover_id").is_empty()
            });
        if needs {
            let n = self.nav();
            for t in current
                .iter_mut()
                .chain(queue.iter_mut())
                .chain(ctx.iter_mut())
            {
                if t.source == "navidrome"
                    && t.cover.is_empty()
                    && !t.meta_str("cover_id").is_empty()
                {
                    t.cover = n
                        .as_ref()
                        .map(|n| n.cover(&t.meta_str("cover_id")))
                        .unwrap_or_default();
                }
            }
        }
        let mut st = self.state();
        st.resume_candidate = current.is_some();
        st.current = current.take();
        st.queue = queue;
        st.later_queue = later;
        st.play_context = ctx;
        st.context_index = d
            .get("context_index")
            .map(crate::model::value_i64)
            .unwrap_or(-1);
        let vol = d
            .get("volume")
            .map(crate::model::value_f64)
            .unwrap_or(100.0);
        st.volume = if vol == 0.0 { 100.0 } else { vol };
        st.resume_position = d
            .get("position")
            .map(crate::model::value_f64)
            .unwrap_or(0.0);
    }

    /// Session-state form of a track: Navidrome rows keep only stable identity
    /// (the auth-bearing `url`/`cover` are dropped, `meta.cover_id` kept).
    pub fn state_track(t: Option<&Track>) -> Value {
        let Some(t) = t else { return Value::Null };
        let mut d = t.to_value();
        if t.source == "navidrome" {
            if let Some(o) = d.as_object_mut() {
                if !t.cover.is_empty() {
                    let cid = cover_id(&t.cover);
                    if !cid.is_empty() {
                        if let Some(Value::Object(m)) = o.get_mut("meta") {
                            m.insert("cover_id".into(), Value::String(cid));
                        }
                    }
                }
                o.insert("url".into(), Value::String(String::new()));
                o.insert("cover".into(), Value::String(String::new()));
            }
        }
        d
    }

    pub fn save_state(&self) {
        let v = {
            let st = self.state();
            json!({
                "current": App::state_track(st.current.as_ref()),
                "queue": st.queue.iter().map(|t| App::state_track(Some(t))).collect::<Vec<_>>(),
                "later_queue": st.later_queue.iter().map(|t| App::state_track(Some(t))).collect::<Vec<_>>(),
                "play_context": st.play_context.iter().map(|t| App::state_track(Some(t))).collect::<Vec<_>>(),
                "context_index": st.context_index,
                "position": st.resume_position,
                "volume": st.volume,
            })
        };
        if let Err(e) = self.session.save(&v) {
            debug!("session save failed · {e}");
        }
    }

    /// Record listening progress, the resume position and volume (best effort).
    pub fn checkpoint(&self, pos: Option<f64>, volume: Option<f64>) {
        let now = crate::paths::monotonic();
        let active = self.playback_active();
        let key = {
            let mut st = self.state();
            if st.play_event_id.is_some() && active && st.play_event_tick > 0.0 {
                st.play_event_listened += (now - st.play_event_tick).clamp(0.0, 5.0);
            }
            if st.play_event_id.is_some() {
                st.play_event_tick = now;
            }
            let mut key = None;
            if let Some(p) = pos {
                st.resume_position = if p.is_finite() { p.max(0.0) } else { 0.0 };
                key = st.current.as_ref().map(|t| (t.key(), st.resume_position));
            }
            if let Some(v) = volume {
                st.volume = v.clamp(0.0, 150.0);
            }
            key
        };
        if let Some((k, p)) = key {
            // Playback checkpoints are best-effort and must never kill the UI.
            let _ = self.db.resume_set(&k, p);
        }
        self.save_state();
    }

    /// Refresh a restored Navidrome track's artwork in the background.
    pub fn hydrate_resume_artwork(&self, ui: Option<&UiRef>) {
        let Some(t) = self.current() else { return };
        if t.source != "navidrome" || !t.cover.is_empty() {
            return;
        }
        let app = self.arc();
        let ui = ui.cloned();
        std::thread::spawn(move || {
            let Some(n) = app.nav() else { return };
            let Ok(fresh) = n.song(&t.id) else { return };
            let cover = {
                let mut st = app.state();
                let Some(cur) = st.current.as_mut().filter(|c| c.key() == t.key()) else {
                    return;
                };
                if !fresh.cover.is_empty() {
                    cur.cover = fresh.cover.clone();
                }
                for (k, v) in fresh.meta.iter() {
                    cur.meta.insert(k.clone(), v.clone());
                }
                if fresh.cover.is_empty() {
                    return;
                }
                let cid = cover_id(&fresh.cover);
                if !cid.is_empty() {
                    cur.meta.insert("cover_id".into(), Value::String(cid));
                }
                cur.cover.clone()
            };
            app.save_state();
            if let Some(ui) = ui {
                let u = ui.clone();
                ui.post(Box::new(move || {
                    u.prefetch_art(&cover);
                    u.invalidate();
                }));
            }
        });
    }

    // ---- library ------------------------------------------------------------

    /// 1.x `App.scan` (configured roots, excludes and batch size).
    pub fn scan(
        &self,
        progress: Option<crate::db::ProgressFn>,
        force: bool,
    ) -> Result<crate::db::ScanReport> {
        self.db.scan_config(&self.cfg, progress, force)
    }

    /// The `--scan` / `:scan` summary line.
    pub fn scan_message(r: &crate::db::ScanReport) -> String {
        format!(
            "Scan · {} checked · {} new · {} updated · {} removed · {} indexed",
            r.checked, r.new, r.updated, r.removed, r.total
        )
    }

    /// Incremental scan on start-up (`auto_library_refresh`, default on).
    pub fn background_library_refresh(&self, ui: Option<&UiRef>) {
        if !self.cfg.getbool("auto_library_refresh", true) {
            return;
        }
        let app = self.arc();
        let ui = ui.cloned();
        std::thread::spawn(move || {
            if let Ok(r) = app.scan(None, false) {
                if let Some(ui) = ui {
                    if r.new > 0 || r.updated > 0 || r.removed > 0 {
                        let u = ui.clone();
                        ui.post(Box::new(move || {
                            u.status(&format!(
                                "Library refreshed · {} new · {} updated · {} removed",
                                r.new, r.updated, r.removed
                            ))
                        }));
                    }
                }
            }
        });
    }

    /// Run `work` on a worker thread; deliver `ok(result)` or
    /// `ui.error(fail, message)` through `ui.post`.
    pub fn async_run<R, W, O>(&self, ui: &UiRef, work: W, ok: O, fail: &str)
    where
        R: Send + 'static,
        W: FnOnce() -> Result<R> + Send + 'static,
        O: FnOnce(R) + Send + 'static,
    {
        let ui = ui.clone();
        let fail = fail.to_string();
        std::thread::spawn(move || match work() {
            Ok(r) => ui.post(Box::new(move || ok(r))),
            Err(e) => {
                let msg = crate::redact::redact_text(&e.to_string());
                let u = ui.clone();
                ui.post(Box::new(move || u.error(&fail, &msg)));
            }
        });
    }

    /// Test a Navidrome connection and save it only when it works.
    pub fn test_and_save_nav(&self, url: &str, user: &str, pw: &str, ui: &UiRef) {
        let (url, user, pw) = (url.to_string(), user.to_string(), pw.to_string());
        let app = self.arc();
        let u = ui.clone();
        self.async_run(
            ui,
            move || {
                Navidrome::new(&url, &user, &pw, 0).ping()?;
                app.cfg.save_nav(&url, &user, &pw);
                Ok(true)
            },
            {
                let app = self.arc();
                move |_| {
                    u.status("Navidrome connected");
                    app.open_nav_home(&u);
                }
            },
            "Connection failed · nothing saved",
        );
    }

    // ---- events / integrations -----------------------------------------------

    /// Emit `integration-state-changed` and tell MPRIS (empty = the defaults).
    pub fn state_changed(&self, props: &[&str]) {
        let props: Vec<&str> = if props.is_empty() {
            vec!["PlaybackStatus", "Metadata", "Volume"]
        } else {
            props.to_vec()
        };
        let mut m = Map::new();
        m.insert("properties".into(), json!(props));
        self.events.emit("integration-state-changed", m);
        self.mpris().changed(&props);
        self.ensure_ticker();
    }

    pub(crate) fn emit_state(&self, event: &str, track: Option<&Track>) {
        let s = self.state().state.clone();
        let mut m = Map::new();
        if let Some(t) = track {
            m.insert("track".into(), t.to_value());
        }
        m.insert("state".into(), playback_state_value(&s));
        self.events.emit(event, m);
    }

    /// What MPRIS publishes.
    pub fn mpris_state(&self) -> MprisState {
        let playing = self.playback_playing();
        let position = self.playback_position();
        let duration = self.playback_duration();
        let st = self.state();
        let t = st.current.clone();
        let can_next = !st.queue.is_empty()
            || (t.as_ref().is_some_and(|t| t.kind != "radio")
                && !st.play_context.is_empty()
                && (st.context_index + 1 < st.play_context.len() as i64
                    || st.repeat_mode == "context"));
        let can_prev =
            t.is_some() && (position > 3.0 || !st.back_stack.is_empty() || st.context_index > 0);
        MprisState {
            radio_info: t
                .as_ref()
                .and_then(|t| st.radio_info.get(&t.id).cloned())
                .unwrap_or_default(),
            track: t,
            playing,
            stopped: st.playback_state.state == "stopped",
            repeat_mode: st.repeat_mode.clone(),
            shuffle_mode: st.shuffle_mode.clone(),
            volume: st.volume,
            position,
            duration,
            can_go_next: can_next,
            can_go_previous: can_prev,
        }
    }

    /// The canonical snapshot with live position/duration.
    pub fn snapshot(&self) -> PlaybackState {
        let pos = self.playback_position();
        let dur = self.playback_duration();
        let mut s = self.state().state.clone();
        s.position = pos;
        if dur > 0.0 {
            s.duration = dur;
        }
        s
    }
}

/// `PlaybackState` as JSON (the event payload form).
pub fn playback_state_value(s: &PlaybackState) -> Value {
    json!({
        "status": s.status, "transport": s.transport, "source": s.source,
        "track_id": s.track_id, "generation": s.generation, "position": s.position,
        "duration": s.duration, "seekable": s.seekable, "kind": s.kind,
        "metadata": s.metadata,
    })
}

/// The `id` query parameter of a (Navidrome getCoverArt) URL.
pub fn cover_id(url: &str) -> String {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let q = q.split('#').next().unwrap_or("");
    crate::redact::parse_qsl(q)
        .into_iter()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// Terminal columns of `text` (combining and format characters take none,
/// East Asian wide/full-width characters two): 1.x `ui._cell_width`, for
/// prompt implementations.
pub fn cell_width(text: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    text.chars()
        .map(|c| {
            if is_format_char(c) {
                0
            } else {
                c.width().unwrap_or(1)
            }
        })
        .sum()
}

fn is_format_char(c: char) -> bool {
    matches!(c as u32, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E | 0x200B..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
}

/// A key that a text prompt inserts (1.x `ui._printable_input`): printable,
/// and not Esc, Tab, CR or LF.
pub fn printable_input(c: char) -> bool {
    !c.is_control()
        && !is_format_char(c)
        && !matches!(c, '\u{2028}' | '\u{2029}')
        && (c == ' ' || !c.is_whitespace())
}
