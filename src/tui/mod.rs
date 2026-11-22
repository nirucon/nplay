//! ratatui terminal UI (port of 1.x `ui.py`, `ui_render.py`, `ui_common.py`
//! and the UI side of `theme_actions.py`). Owner: tui track.
//!
//! # Structure
//! - [`run(app, query)`]: the entry point `main` calls; returns the exit code
//!   (0 after `Q`/`Ctrl+C`, SIGTERM, SIGHUP or SIGINT) and leaves the
//!   terminal restored, so `main` can run `App::shutdown`.
//! - [`Shared`] implements [`crate::app::Ui`]. Its view state
//!   ([`state::TuiState`]) sits behind one mutex that is never held while
//!   the TUI calls into the [`App`], so the engine may call any `Ui` method
//!   from any thread (including re-entrantly from the UI thread). Mutating
//!   calls mark the state dirty and wake the UI thread through its channel;
//!   `post(task)` queues the task to the UI thread. Prompts are
//!   asynchronous: `prompt(.., done)` shows the prompt row and `done` runs on
//!   the UI thread on Enter/Esc.
//! - The UI thread blocks on one channel fed by an input thread (blocking
//!   `crossterm::event::read`, which also delivers SIGWINCH resizes), a
//!   signal thread (self-pipe for SIGTERM/SIGHUP/SIGINT), CAVA frames and
//!   engine wake-ups. Timeouts exist only while needed: the next whole second
//!   of playback position while playing (progress bar), the 100 ms list
//!   preview delay, and the 10 s statistics refresh while a STATISTICS menu
//!   is open. CAVA runs only while the visualizer is on and audio plays, and
//!   its frames redraw only while Now Playing is visible. Idle = no wakeups.
//! - The engine keeps its own ticker (checkpoints, statistics, sleep timer,
//!   sync), so this loop never calls those.
//! - Rendering: [`render`] (pure, ratatui `Buffer`), key bindings: [`keys`],
//!   text prompt: [`prompt`].

pub mod keys;
pub mod prompt;
pub mod render;
pub mod state;
#[cfg(test)]
mod tests;

use crate::app::{App, PromptDone, Row, Task, Ui, UiRef, View};
use crate::artwork::Artwork;
use crate::config::Config;
use crate::model::Track;
use crate::visualizer::Visualizer;
use crossterm::event::{Event, KeyEvent};
use keys::Keymap;
use prompt::{Outcome, Prompt};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use render::{AppView, ArtPlan, Telemetry};
use state::TuiState;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

enum Msg {
    Input(Event),
    Task(Task),
    Wake,
    Frame,
    Signal(i32),
}

/// The `Ui` the engine talks to.
pub struct Shared {
    st: Mutex<TuiState>,
    tx: Sender<Msg>,
    wake_pending: AtomicBool,
    frame_pending: AtomicBool,
    viz_visible: AtomicBool,
    tele_force: AtomicBool,
    me: Weak<Shared>,
    app: Weak<App>,
    cfg: Config,
}

impl Shared {
    fn new(app: &Arc<App>, tx: Sender<Msg>) -> Arc<Shared> {
        let cfg = app.cfg.clone();
        Arc::new_cyclic(|me: &Weak<Shared>| {
            let w = me.clone();
            let on_ready: crate::artwork::ReadyCallback = Arc::new(move |_url: &str| {
                if let Some(sh) = w.upgrade() {
                    sh.invalidate();
                }
            });
            let art = Artwork::new(Some(on_ready), Some(app.artwork_cache.clone()));
            let st = TuiState::new(
                &cfg.get("theme", "niru-noir"),
                cfg.getbool("visualizer", true),
                &cfg.get("visualizer_style", "classic"),
                &cfg.get("now_playing_view", "normal"),
                art,
            );
            Shared {
                st: Mutex::new(st),
                tx,
                wake_pending: AtomicBool::new(false),
                frame_pending: AtomicBool::new(false),
                viz_visible: AtomicBool::new(false),
                tele_force: AtomicBool::new(true),
                me: me.clone(),
                app: Arc::downgrade(app),
                cfg: cfg.clone(),
            }
        })
    }

    fn st(&self) -> MutexGuard<'_, TuiState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(Msg::Wake);
        }
    }

    /// Run `f` on the view state, then wake the UI thread.
    fn with<R>(&self, f: impl FnOnce(&mut TuiState) -> R) -> R {
        let r = f(&mut self.st());
        self.wake();
        r
    }

    /// Show a prompt; `complete` enables `:` completion and history.
    fn start_prompt(
        &self,
        label: &str,
        initial: &str,
        secret: bool,
        complete: bool,
        done: PromptDone,
    ) {
        let old = self.with(|st| {
            let n = st.command_history.len();
            let p = Prompt::new(label, initial, secret, complete, n, Some(done));
            st.dirty = true;
            st.prompt.replace(p)
        });
        if let Some(mut p) = old {
            if let Some(d) = p.done.take() {
                d(None);
            }
        }
    }

    /// Show or refresh a chooser menu in place (theme, CAVA style) so live
    /// switching does not stack one menu per choice.
    fn chooser(&self, title: &str, rows: Vec<Row>) {
        self.with(|st| {
            if st.mode == "menu" && st.title == title {
                let sel = st.sel;
                st.update_items(rows, sel);
            } else {
                st.show_menu(title, rows, true);
            }
        });
    }

    fn show_diag(&self) {
        let visual = self.with(|st| {
            st.diag_vis.clear();
            st.diag_yt = None;
            st.diag_log = None;
            st.open_mode("diagnostics", "");
            st.visual
        });
        // One read-only query when the view opens (no polling); the service
        // probe and `yt-dlp --version` must not block the UI thread.
        let (Some(app), me) = (self.app.upgrade(), self.me.clone()) else {
            return;
        };
        std::thread::spawn(move || {
            let running = visual && app.playback_playing();
            let vis = crate::service::probe::vis_line(running, None);
            let yt = app.yt.version();
            let log = crate::log::recent_error_count();
            if let Some(sh) = me.upgrade() {
                sh.with(|st| {
                    st.diag_vis = vis;
                    st.diag_yt = Some(yt);
                    st.diag_log = Some(log);
                    st.dirty = true;
                });
            }
        });
    }
}

impl Ui for Shared {
    fn post(&self, task: Task) {
        if let Err(e) = self.tx.send(Msg::Task(task)) {
            // UI loop gone (shutting down): run it here.
            if let Msg::Task(t) = e.0 {
                t()
            }
        }
    }
    fn status(&self, msg: &str) {
        self.tele_force.store(true, Ordering::Release);
        self.with(|st| st.status(msg));
    }
    fn error(&self, title: &str, msg: &str) {
        self.with(|st| st.error(title, msg));
    }
    fn invalidate(&self) {
        self.tele_force.store(true, Ordering::Release);
        self.with(|st| st.invalidate());
    }
    fn now_playing(&self) {
        self.with(|st| st.now_playing());
    }
    fn show_menu(&self, title: &str, rows: Vec<Row>, push: bool) {
        self.with(|st| st.show_menu(title, rows, push));
    }
    fn show_list(
        &self,
        title: &str,
        rows: Vec<Row>,
        empty: &str,
        message: Option<&str>,
        push: bool,
    ) {
        self.with(|st| st.show_list(title, rows, empty, message, push));
    }
    fn replace_list(&self, title: &str, rows: Vec<Row>, empty: &str, message: Option<&str>) {
        self.with(|st| st.replace_list(title, rows, empty, message));
    }
    fn loading(&self, title: &str, msg: &str, rows: Vec<Row>) {
        self.with(|st| st.loading(title, msg, rows));
    }
    fn update_items(&self, rows: Vec<Row>, sel: usize) {
        self.with(|st| st.update_items(rows, sel));
    }
    fn set_title(&self, title: &str) {
        self.with(|st| {
            st.title = title.to_string();
            st.invalidate();
        });
    }
    fn prompt(&self, label: &str, initial: &str, secret: bool, done: PromptDone) {
        self.start_prompt(label, initial, secret, false, done);
    }
    fn push(&self) {
        self.with(|st| st.push());
    }
    fn show_info(&self, track: &Track) {
        let t = track.clone();
        self.with(|st| st.show_context(Some(t)));
    }
    fn show_about(&self) {
        self.with(|st| st.open_mode("about", "ABOUT"));
    }
    fn show_diagnostics(&self) {
        self.show_diag();
    }
    fn show_help(&self) {
        self.with(|st| st.open_mode("help", ""));
    }
    fn theme(&self) -> String {
        self.st().theme.clone()
    }
    fn theme_label(&self) -> String {
        self.st().theme_label()
    }
    fn theme_menu(&self) {
        let rows = self.st().theme_rows();
        self.chooser("THEME", rows);
    }
    fn set_theme(&self, name: &str) {
        if let Some(key) = self.with(|st| st.set_theme(name)) {
            self.cfg.set("theme", key);
        }
    }
    fn reload_theme(&self) {
        self.with(|st| {
            st.apply_theme();
            st.art_clear = true;
            st.art_visible = false;
        });
    }
    fn layout(&self) -> String {
        self.st().layout.clone()
    }
    fn set_layout(&self, layout: &str) {
        let l = self.with(|st| {
            st.set_layout(layout);
            st.layout.clone()
        });
        self.cfg.set("now_playing_view", l);
    }
    fn cycle_layout(&self) {
        let l = self.with(|st| st.cycle_layout());
        self.cfg.set("now_playing_view", l);
    }
    fn visualizer_on(&self) -> bool {
        self.st().visual
    }
    fn toggle_visualizer(&self) {
        let on = self.with(|st| {
            st.visual = !st.visual;
            let on = st.visual;
            st.status(if on {
                "Visualizer on"
            } else {
                "Visualizer off"
            });
            on
        });
        self.cfg
            .set("visualizer", if on { "true" } else { "false" });
    }
    fn visual_style(&self) -> String {
        self.st().visual_style.clone()
    }
    fn set_visual_style(&self, style: &str) {
        self.with(|st| {
            if state::STYLES.contains(&style) {
                st.visual_style = style.to_string();
            }
            st.invalidate();
        });
    }
    fn visualizer_style_menu(&self) {
        let rows = self.st().visualizer_style_rows();
        self.chooser("CAVA STYLE", rows);
    }
    fn set_sort_mode(&self, mode: &str) {
        self.with(|st| st.set_sort_mode(mode));
    }
    fn prefetch_art(&self, url: &str) {
        if !url.is_empty() {
            self.st().art.prefetch(url);
        }
    }
    fn view(&self) -> Option<View> {
        Some(self.st().view())
    }
    fn set_selection(&self, index: usize) {
        self.with(|st| st.set_selection(index));
    }
    fn edge_selection(&self, last: bool) {
        self.with(|st| st.edge_selection(last));
    }
    fn quit(&self) {
        self.with(|st| st.running = false);
    }
}

// ---------------------------------------------------------------------------
// Signals: SIGTERM / SIGHUP / SIGINT -> orderly exit through a self-pipe.
// ---------------------------------------------------------------------------

static SIG_FD: AtomicI32 = AtomicI32::new(-1);
const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIG_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        // SAFETY: write(2) is async-signal-safe; the buffer is a local byte.
        unsafe {
            libc::write(fd, &b as *const u8 as *const libc::c_void, 1);
        }
    }
}

fn install_signals(tx: Sender<Msg>) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: plain pipe(2)/signal(2) calls with valid pointers.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return;
        }
        SIG_FD.store(fds[1], Ordering::Relaxed);
        for s in SIGNALS {
            libc::signal(s, on_signal as *const () as libc::sighandler_t);
        }
    }
    let r = fds[0];
    std::thread::Builder::new()
        .name("nplay-signals".into())
        .spawn(move || loop {
            let mut b = 0u8;
            // SAFETY: reading one byte into a local.
            let n = unsafe { libc::read(r, &mut b as *mut u8 as *mut libc::c_void, 1) };
            if n == 1 {
                if tx.send(Msg::Signal(b as i32)).is_err() {
                    break;
                }
            } else if n < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            } else {
                break;
            }
        })
        .ok();
}

fn restore_signals() {
    // SAFETY: resetting dispositions to the default.
    unsafe {
        for s in SIGNALS {
            libc::signal(s, libc::SIG_DFL);
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn setup_terminal() -> std::io::Result<Term> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    crossterm::execute!(
        out,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;
    Terminal::new(CrosstermBackend::new(std::io::stdout()))
}

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = crossterm::execute!(
        out,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
}

fn write_out(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// The UI thread
// ---------------------------------------------------------------------------

struct Tui {
    sh: Arc<Shared>,
    ui: UiRef,
    app: Arc<App>,
    rx: Receiver<Msg>,
    keys: Keymap,
    viz: Visualizer,
    viz_started: Option<Instant>,
    tele: Telemetry,
    tele_at: Option<Instant>,
    stats_at: Option<Instant>,
    term: Term,
}

/// Run the terminal UI until quit; `query` (may be empty) starts a search.
/// Returns the process exit code. The caller runs `App::shutdown` after it.
pub fn run(app: Arc<App>, query: &str) -> i32 {
    let (tx, rx) = channel::<Msg>();
    let sh = Shared::new(&app, tx.clone());
    let ui: UiRef = sh.clone();

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev_hook(info);
    }));
    let term = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            restore_terminal();
            eprintln!("NPLAY needs an interactive terminal: {e}");
            return 1;
        }
    };
    install_signals(tx.clone());
    let itx = tx.clone();
    std::thread::Builder::new()
        .name("nplay-input".into())
        .spawn(move || {
            while let Ok(ev) = crossterm::event::read() {
                if itx.send(Msg::Input(ev)).is_err() {
                    break;
                }
            }
        })
        .ok();

    let fsh = Arc::downgrade(&sh);
    let viz = Visualizer::new(
        256,
        Some(Arc::new(move |_v: &Visualizer, _f: &[u8]| {
            if let Some(sh) = fsh.upgrade() {
                if sh.viz_visible.load(Ordering::Acquire)
                    && !sh.frame_pending.swap(true, Ordering::AcqRel)
                {
                    let _ = sh.tx.send(Msg::Frame);
                }
            }
        })),
        Some(Arc::new({
            let w = Arc::downgrade(&sh);
            move |_v: &Visualizer| {
                if let Some(sh) = w.upgrade() {
                    sh.invalidate();
                }
            }
        })),
    );

    let mut tui = Tui {
        sh: sh.clone(),
        ui: ui.clone(),
        app: app.clone(),
        rx,
        keys: Keymap::from_config(&app.cfg),
        viz,
        viz_started: None,
        tele: Telemetry::default(),
        tele_at: None,
        stats_at: None,
        term,
    };
    if let Ok(sz) = tui.term.size() {
        sh.st().size = (sz.width, sz.height);
    }
    tui.refresh_telemetry();
    tui.draw();

    // Like 1.x: the UI is drawn first, then the engine gets its UI.
    app.attach_ui(&ui);
    let ew = Arc::downgrade(&sh);
    let sub = app.events.subscribe(
        "*",
        Arc::new(
            move |_e: &str, _p: &serde_json::Map<String, serde_json::Value>| {
                if let Some(sh) = ew.upgrade() {
                    sh.invalidate();
                }
            },
        ),
    );
    if !query.trim().is_empty() {
        app.search_async(query.trim(), &ui);
    }

    tui.main_loop();

    app.events.unsubscribe(sub);
    tui.viz.stop();
    let clear = tui.sh.st().art.clear();
    write_out(&clear);
    restore_terminal();
    restore_signals();
    app.set_ui(None);
    0
}

impl Tui {
    fn running(&self) -> bool {
        self.sh.st().running
    }

    fn main_loop(&mut self) {
        while self.running() {
            let msg = match self.next_timeout() {
                None => match self.rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                },
                Some(d) => match self.rx.recv_timeout(d) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
            };
            if msg.is_none() {
                // A tick: progress bar, preview delay or statistics refresh.
                self.sh.st().dirty = true;
            }
            if let Some(m) = msg {
                self.handle(m);
            }
            while let Ok(m) = self.rx.try_recv() {
                self.handle(m);
                if !self.running() {
                    break;
                }
            }
            if !self.running() {
                break;
            }
            self.housekeeping();
            let (dirty, preview) = {
                let st = self.sh.st();
                let preview = (st.mode == "list" || st.mode == "menu")
                    && st.preview_due.map(|d| Instant::now() >= d).unwrap_or(false);
                (st.dirty, preview)
            };
            if dirty || preview {
                self.draw();
            }
        }
    }

    /// How long the loop may block; None = until something happens.
    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut t: Option<Duration> = None;
        let mut min = |d: Duration| t = Some(t.map_or(d, |x: Duration| x.min(d)));
        if self.tele.playing && self.app.current().is_some() {
            // Wake when the displayed second changes, never faster than 20/s.
            let frac = 1.0 - self.tele.pos.fract();
            min(Duration::from_secs_f64((frac + 0.02).clamp(0.05, 1.0)));
        }
        let st = self.sh.st();
        if let Some(d) = st.preview_due {
            min(d.saturating_duration_since(now));
        }
        if st.mode == "menu" && st.title.starts_with("STATISTICS") {
            let due = self
                .stats_at
                .map(|s| s + Duration::from_secs(10))
                .unwrap_or(now);
            min(due
                .saturating_duration_since(now)
                .max(Duration::from_millis(50)));
        }
        t
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Wake => self.sh.wake_pending.store(false, Ordering::Release),
            Msg::Frame => {
                self.sh.frame_pending.store(false, Ordering::Release);
                if self.sh.viz_visible.load(Ordering::Acquire) {
                    self.sh.st().dirty = true;
                }
            }
            Msg::Task(t) => {
                t();
                self.sh.st().dirty = true;
            }
            Msg::Signal(sig) => {
                crate::debug!("tui: signal {sig}, exiting");
                self.sh.st().running = false
            }
            Msg::Input(ev) => {
                self.sh.tele_force.store(true, Ordering::Release);
                self.input(ev);
                self.sh.st().dirty = true;
            }
        }
    }

    fn refresh_telemetry(&mut self) {
        let app = &self.app;
        let (has, resume) = {
            let s = app.state();
            (s.current.is_some(), s.resume_position)
        };
        if !has {
            self.tele = Telemetry::default();
        } else {
            let active = app.playback_active();
            self.tele = Telemetry {
                active,
                playing: active && app.playback_playing(),
                pos: if active {
                    app.playback_position()
                } else {
                    resume
                },
                dur: app.playback_duration(),
            };
        }
        self.tele_at = Some(Instant::now());
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        let stale = self
            .tele_at
            .map(|t| now.duration_since(t) >= Duration::from_millis(200))
            .unwrap_or(true);
        if self.sh.tele_force.swap(false, Ordering::AcqRel) || stale {
            self.refresh_telemetry();
        }
        let (visual, mode, title) = {
            let st = self.sh.st();
            (st.visual, st.mode.clone(), st.title.clone())
        };
        let has_current = self.app.current().is_some();
        // CAVA only while it can show something.
        let want = visual && self.tele.playing;
        if want && !self.viz.running() {
            let retry = self
                .viz_started
                .map(|t| now.duration_since(t) >= Duration::from_secs(5))
                .unwrap_or(true);
            if retry && self.viz.available() {
                self.viz_started = Some(now);
                self.viz.start();
            }
        } else if !want && self.viz.running() {
            self.viz.stop();
            self.viz_started = None;
            self.sh.st().dirty = true;
        }
        self.sh.viz_visible.store(
            mode == "home" && visual && has_current && self.viz.running(),
            Ordering::Release,
        );
        // Refresh a visible statistics menu every 10 s (1.x).
        if mode == "menu" && title.starts_with("STATISTICS") {
            match self.stats_at {
                None => self.stats_at = Some(now),
                Some(t) if now.duration_since(t) >= Duration::from_secs(10) => {
                    self.stats_at = Some(now);
                    self.app.refresh_statistics_view(&self.ui);
                }
                _ => {}
            }
        } else {
            self.stats_at = None;
        }
    }

    fn draw(&mut self) {
        let av = AppView::gather(&self.app);
        let kitty_cfg = self.app.cfg.getbool("kitty_artwork", true);
        let sh = self.sh.clone();
        let mut st = sh.st();
        let kitty = st.art.kitty() && kitty_cfg;
        if st.art_clear {
            st.art_clear = false;
            let b = st.art.clear();
            if kitty {
                write_out(&b);
            }
        }
        let now = Instant::now();
        let preview_ready = st.preview_due.map(|d| now >= d).unwrap_or(true);
        if preview_ready {
            st.preview_due = None;
        }
        let ctx = render::Ctx {
            app: &self.app,
            av: &av,
            tele: &self.tele,
            viz: Some(&self.viz),
            kitty,
            keys: &self.keys,
            preview_ready,
        };
        let mut plan = ArtPlan::Keep;
        let r = self.term.draw(|f| {
            let area = f.area();
            plan = render::draw(f.buffer_mut(), &st, &ctx);
            if let Some(p) = &st.prompt {
                if area.height >= 12 && area.width >= 42 {
                    let x = p.cursor_x(area.width as usize) as u16;
                    f.set_cursor_position((x, area.height - 2));
                }
            }
        });
        if let Ok(done) = r {
            st.size = (done.area.width, done.area.height);
        }
        st.dirty = false;
        match plan {
            ArtPlan::Keep => {}
            ArtPlan::Clear => {
                if st.art_visible {
                    let b = st.art.clear();
                    write_out(&b);
                    st.art_visible = false;
                }
            }
            ArtPlan::Draw {
                url,
                row,
                col,
                cols,
                rows,
            } => {
                let d = st.art.draw(&url, row, col, cols, rows);
                write_out(&d.bytes);
                if d.shown {
                    st.art_visible = true;
                }
            }
        }
    }

    fn input(&mut self, ev: Event) {
        match ev {
            Event::Resize(w, h) => {
                let mut st = self.sh.st();
                st.size = (w, h);
                st.art_clear = true;
                st.art_visible = false;
                if st.mode == "list" || st.mode == "menu" {
                    st.preview_due = Some(Instant::now() + Duration::from_millis(120));
                }
                st.dirty = true;
            }
            Event::Key(k) => {
                if self.sh.st().prompt.is_some() {
                    self.prompt_key(&k);
                    return;
                }
                let Some(spec) = keys::spec(&k) else { return };
                let Some(action) = self.keys.action(&spec).map(str::to_string) else {
                    return;
                };
                self.act(&action);
            }
            Event::Paste(s) => {
                let mut st = self.sh.st();
                let w = st.size.0 as usize;
                if let Some(p) = st.prompt.as_mut() {
                    p.paste(&s, w);
                }
            }
            _ => {}
        }
    }

    fn prompt_key(&mut self, k: &KeyEvent) {
        if k.kind == crossterm::event::KeyEventKind::Release {
            return;
        }
        let (outcome, prompt) = {
            let mut st = self.sh.st();
            let w = st.size.0 as usize;
            let hist = st.command_history.clone();
            let Some(p) = st.prompt.as_mut() else { return };
            let o = p.key(k, &hist, w);
            st.dirty = true;
            if o == Outcome::Continue {
                return;
            }
            let p = st.prompt.take();
            if let (Outcome::Submit(s), Some(p)) = (&o, &p) {
                if p.complete && !s.is_empty() {
                    prompt::remember(&mut st.command_history, s);
                }
            }
            (o, p)
        };
        let Some(mut p) = prompt else { return };
        let Some(done) = p.done.take() else { return };
        match outcome {
            Outcome::Submit(s) => done(Some(s)),
            _ => done(None),
        }
    }

    fn selected(&self) -> Option<Track> {
        self.sh.st().selected()
    }

    fn quick_find(&self) {
        let app = self.app.clone();
        let ui = self.ui.clone();
        self.sh.start_prompt(
            "UNIVERSAL QUICK FIND › ",
            "",
            false,
            false,
            Box::new(move |q| app.search_async(q.as_deref().unwrap_or(""), &ui)),
        );
    }

    /// One key action (1.x `UI.key`).
    fn act(&mut self, action: &str) {
        let app = self.app.clone();
        let ui = self.ui.clone();
        let sh = self.sh.clone();
        let (mode, title, sel, nitems) = {
            let st = sh.st();
            (st.mode.clone(), st.title.clone(), st.sel, st.items.len())
        };
        // Help scrolls instead of moving a selection.
        if mode == "help" {
            let h = sh.st().size.1 as i32;
            let max = render::help_max_scroll(&self.keys, h);
            let page = (h - 10).max(1) as usize;
            let mut st = sh.st();
            let s = st.help_scroll;
            let handled = match action {
                "down" => Some((s + 1).min(max)),
                "up" => Some(s.saturating_sub(1)),
                "page_down" => Some((s + page).min(max)),
                "page_up" => Some(s.saturating_sub(page)),
                "home" => Some(0),
                "last" => Some(max),
                _ => None,
            };
            if let Some(v) = handled {
                st.help_scroll = v;
                st.dirty = true;
                return;
            }
        }
        let playlist = app
            .current_playlist_pid()
            .filter(|_| title.starts_with("PLAYLIST ·"));
        let queue = title == "QUEUE";
        match action {
            "quit" => sh.st().running = false,
            "help" => ui.show_help(),
            "back" => sh.with(|st| st.back()),
            "now" => ui.now_playing(),
            "next" => app.next_track(Some(&ui), false),
            "previous" => app.previous_track(Some(&ui)),
            "browse" => app.root_browse(&ui),
            "queue" => app.open_source("queue", &ui),
            "tab" => {
                if mode == "home" {
                    app.open_source("queue", &ui)
                } else if queue {
                    let cur = app.current();
                    sh.with(|st| st.show_context(cur));
                } else {
                    ui.now_playing()
                }
            }
            "down" => sh.with(|st| st.move_selection(1)),
            "up" => sh.with(|st| st.move_selection(-1)),
            "home" => sh.with(|st| st.edge_selection(false)),
            "last" => sh.with(|st| st.edge_selection(true)),
            "page_down" => sh.with(|st| st.page_selection(1)),
            "page_up" => sh.with(|st| st.page_selection(-1)),
            "play_pause" => app.play_pause(Some(&ui)),
            "stop" => app.stop_and_clear(Some(&ui)),
            "visual_style" => {
                let s = sh.with(|st| st.cycle_visual_style());
                app.cfg.set("visualizer_style", s);
            }
            "seek_back" => app.playback_seek_relative(-5.0),
            "seek_forward" => app.playback_seek_relative(5.0),
            "seek_back_long" => app.playback_seek_relative(-30.0),
            "seek_forward_long" => app.playback_seek_relative(30.0),
            "volume_up" => app.playback_volume(5.0),
            "volume_down" => app.playback_volume(-5.0),
            "mute" => app.playback_mute(),
            "shuffle" => app.cycle_shuffle(Some(&ui)),
            "repeat" => app.cycle_repeat(Some(&ui)),
            "queue_add" => {
                if let Some(x) = self.selected() {
                    app.queue_add(x, Some(&ui));
                }
            }
            "playlist_add" => {
                if let Some(x) = self.selected() {
                    app.add_selected_to_playlist(x, &ui);
                }
            }
            "queue_clear" if queue => app.queue_clear(Some(&ui)),
            "queue_save" if queue => app.queue_save_playlist(Some(&ui)),
            "random" => {
                let rows = sh.st().items.clone();
                app.random_play(&ui, &title, &rows);
            }
            "play_next" => {
                if let Some(x) = self.selected() {
                    app.queue_next(x, Some(&ui));
                }
            }
            "remove" => {
                if queue {
                    app.queue_remove(sel, Some(&ui));
                } else if let Some(pid) = playlist {
                    app.playlist_remove(pid, sel, &ui);
                }
            }
            "move_down" | "move_up" => {
                let d = if action == "move_down" { 1 } else { -1 };
                let new = if queue {
                    Some(app.queue_move(sel, d, Some(&ui)))
                } else {
                    playlist.map(|pid| app.playlist_move(pid, sel, d, &ui))
                };
                if let Some(i) = new {
                    sh.with(|st| {
                        st.sel = i.min(st.items.len().saturating_sub(1));
                        st.invalidate();
                    });
                }
            }
            "rename" => {
                if let Some(pid) = playlist {
                    app.playlist_rename(pid, &ui);
                }
            }
            "delete" => {
                if let Some(pid) = playlist {
                    app.playlist_delete(pid, &ui);
                }
            }
            "quick_find" => self.quick_find(),
            "search" => {
                if (mode == "list" || mode == "menu") && nitems > 0 {
                    let s2 = sh.clone();
                    sh.start_prompt(
                        "FILTER CURRENT LIST › ",
                        "",
                        false,
                        false,
                        Box::new(move |q| s2.with(|st| st.filter_current(q.as_deref()))),
                    );
                } else {
                    self.quick_find();
                }
            }
            "sort" => sh.with(|st| st.cycle_sort()),
            "command" => {
                let (a2, u2) = (app.clone(), ui.clone());
                sh.start_prompt(
                    ":",
                    "",
                    false,
                    true,
                    Box::new(move |q| match q {
                        Some(q) => a2.command(&q, &u2),
                        None => u2.status("Command cancelled"),
                    }),
                );
            }
            "refresh" => app.scan_async(&ui, false),
            "favorite" => {
                if app.current().is_some() {
                    app.favorite_toggle(Some(&ui));
                }
            }
            "visualizer" => ui.cycle_layout(),
            "toggle_visualizer" => ui.toggle_visualizer(),
            "actions" => {
                let x = self.selected();
                app.action_menu(&ui, x);
            }
            "theme" => ui.theme_menu(),
            "info" => {
                let t = self.selected().or_else(|| app.current());
                sh.with(|st| st.show_context(t));
            }
            "activate" => {
                if nitems > 0 {
                    let rows = sh.st().items.clone();
                    app.activate(&ui, &title, &rows, sel);
                } else if mode == "home" && app.current().is_some() {
                    app.play_pause(Some(&ui));
                }
            }
            _ => {}
        }
    }
}
