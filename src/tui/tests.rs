//! TUI tests: rendering of every view into a ratatui buffer, view state
//! (selection, paging, filter, sort, back stack), the `Ui` implementation
//! (menus, prompts, theme switching incl. TOML custom themes), geometry and
//! key bindings. UI parts of tests/test_unicode_prompt.py (see prompt.rs),
//! test_theme_ux.py and test_statistics_ui.py are covered here.

use super::render::{self, AppView, Telemetry};
use super::state::TuiState;
use super::*;
use crate::app::{AppOptions, Row};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

struct Env {
    app: Arc<App>,
    sh: Arc<Shared>,
    ui: UiRef,
    _rx: Receiver<Msg>,
    root: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.app.set_ui(None);
        self.app.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn env() -> Env {
    let guard = crate::player::MPV_TESTS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "nplay-tui-test-{}-{}",
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
    std::env::remove_var("KITTY_WINDOW_ID");
    std::env::remove_var("TERM_PROGRAM");
    std::env::set_var("TERM", "xterm-256color");
    let app = App::with_options(AppOptions {
        mpv_binary: Some("/nonexistent/mpv".into()),
        mpv_log: None,
        no_mpris: true,
        notify_binary: Some(None),
    })
    .unwrap();
    let (tx, rx) = channel();
    let sh = Shared::new(&app, tx);
    let ui: UiRef = sh.clone();
    Env {
        app,
        sh,
        ui,
        _rx: rx,
        root,
        _guard: guard,
    }
}

fn track(id: &str) -> Track {
    let mut t = Track::new("local", id, &format!("Song {id}"));
    t.artist = format!("Artist {id}");
    t.album = "Album".into();
    t.duration = 180.0;
    t
}

/// Render the current state at `w`x`h`; returns the screen lines.
fn screen_with(e: &Env, w: u16, h: u16, tele: &Telemetry) -> (Vec<String>, render::ArtPlan) {
    let av = AppView::gather(&e.app);
    let keys = Keymap::from_config(&e.app.cfg);
    let mut st = e.sh.st();
    st.size = (w, h);
    let ctx = render::Ctx {
        app: &e.app,
        av: &av,
        tele,
        viz: None,
        kitty: false,
        keys: &keys,
        preview_ready: true,
    };
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    let plan = render::draw(&mut buf, &st, &ctx);
    let lines = (0..h)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < w {
                let sym = buf[(x, y)].symbol();
                line.push_str(sym);
                x += unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
            }
            line
        })
        .collect();
    (lines, plan)
}

fn screen(e: &Env) -> Vec<String> {
    screen_with(e, 100, 30, &Telemetry::default()).0
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

use ratatui::buffer::Buffer;

#[test]
fn idle_home_and_footer() {
    let e = env();
    let s = screen(&e);
    assert!(s[1].contains("NPLAY") && s[1].contains(render::VERSION));
    let t = text(&s);
    assert!(t.contains("Nothing playing"));
    assert!(t.contains("b  Browse your music"));
    assert!(t.contains("^P Universal Quick Find"));
    assert!(s[27].contains("─"));
    assert!(s[28].contains("Ready"));
    assert!(s[29].contains("SPACE pause   z stop"));
    // Narrow terminals get the short footer.
    let (n, _) = screen_with(&e, 60, 20, &Telemetry::default());
    assert!(n[19].contains("? help  ^N now  n/p track  b browse  Q quit"));
    // Too small.
    let (small, _) = screen_with(&e, 40, 10, &Telemetry::default());
    assert!(small[0].starts_with("NPLAY · terminal too small"));
}

#[test]
fn now_playing_normal_view() {
    let e = env();
    let a = track("1");
    let b = track("2");
    {
        let mut st = e.app.state();
        st.current = Some(a.clone());
        st.play_context = vec![a.clone(), b.clone()];
        st.context_index = 0;
    }
    let tele = Telemetry {
        active: true,
        playing: true,
        pos: 90.0,
        dur: 180.0,
    };
    let (s, plan) = screen_with(&e, 100, 30, &tele);
    assert_eq!(plan, render::ArtPlan::Keep);
    assert!(s[4].contains("Song 1"));
    assert!(s[5].contains("Artist 1"));
    assert!(s[6].contains("Album"));
    assert!(s[8].contains("LOCAL"));
    assert!(s[10].contains("━") && s[10].contains("●") && s[10].contains("─"));
    assert!(s[11].contains("1:30   3:00"));
    let t = text(&s);
    assert!(t.contains("1 of 2 · Album"));
    assert!(t.contains("UP NEXT  Artist 2 · Song 2"));
    assert!(t.contains("RG ALBUM"));
    assert!(s[29].contains("SPACE pause"));
    // Other views show the transport line instead of the status message.
    e.ui.show_menu("BROWSE", vec![Row::entry("LOCAL MUSIC", "local", "")], true);
    let (s, _) = screen_with(&e, 100, 30, &tele);
    assert!(s[28].contains("▶ Artist 1 · Song 1   1:30 / 3:00"));
}

#[test]
fn resume_and_radio_views() {
    let e = env();
    {
        let mut st = e.app.state();
        st.current = Some(track("1"));
        st.resume_candidate = true;
        st.resume_position = 75.0;
    }
    let t = text(&screen(&e));
    assert!(t.contains("Resume with SPACE · 1:15"));
    assert!(t.contains("SPACE resume"));
    let mut r = Track::new("sr", "132", "P1");
    r.kind = "radio".into();
    e.app.state().current = Some(r);
    e.app.state().resume_candidate = false;
    let t = text(&screen(&e));
    assert!(t.contains("● LIVE · P1"));
    e.ui.show_menu("X", vec![Row::entry("A", "a", "")], true);
    assert!(text(&screen(&e)).contains("● P1 · LIVE"));
}

#[test]
fn menus_headers_and_selection() {
    let e = env();
    e.ui.show_menu(
        "BROWSE",
        vec![
            Row::header("LIBRARY"),
            Row::entry("LOCAL MUSIC", "local", "3 indexed tracks"),
            Row::entry("PLAYLISTS", "playlists", "Local cross-source playlists"),
            Row::header("NPLAY"),
            Row::entry("ABOUT", "about", "NPLAY 2"),
        ],
        true,
    );
    // The first selectable row is chosen, headers are skipped.
    assert_eq!(e.sh.st().sel, 1);
    let s = screen(&e);
    assert!(s[3].contains("BROWSE"));
    assert!(s[5].trim_start().starts_with("LIBRARY"));
    assert!(s[6].contains("› LOCAL MUSIC") && s[6].contains("3 indexed tracks"));
    // Detail column is aligned: label padded to the longest label + 4.
    let col = |l: &str, d: &str| l[..l.find(d).unwrap()].chars().count();
    assert_eq!(col(&s[6], "3 indexed"), col(&s[7], "Local cross"));
    e.sh.st().move_selection(1);
    assert_eq!(e.sh.st().sel, 2);
    e.sh.st().move_selection(1);
    assert_eq!(e.sh.st().sel, 4, "header skipped");
    e.sh.st().move_selection(1);
    assert_eq!(e.sh.st().sel, 4, "stays on the last row");
    e.sh.st().edge_selection(false);
    assert_eq!(e.sh.st().sel, 1);
    e.sh.st().edge_selection(true);
    assert_eq!(e.sh.st().sel, 4);
    // Esc goes back exactly one view, then home.
    e.ui.show_menu("SUB", vec![Row::entry("X", "x", "")], true);
    e.sh.st().back();
    assert_eq!(e.sh.st().title, "BROWSE");
    assert_eq!(e.sh.st().sel, 4);
    e.sh.st().back();
    assert_eq!(e.sh.st().mode, "home");
}

#[test]
fn long_lists_page_and_scroll() {
    let e = env();
    let rows: Vec<Row> = (0..100)
        .map(|i| Row::Track(track(&i.to_string())))
        .collect();
    e.ui.show_list("LOCAL MUSIC · ALL TRACKS", rows, "No items", None, true);
    assert_eq!(e.sh.st().message, "100 items");
    e.sh.st().size = (100, 30);
    e.sh.st().page_selection(1);
    assert_eq!(e.sh.st().sel, 22);
    e.sh.st().set_selection(80);
    let s = screen(&e);
    assert!(text(&s).contains("› Song 80  ·  Artist 80"));
    assert!(!text(&s).contains("Song 1  ·"));
    e.sh.st().edge_selection(true);
    let s = screen(&e);
    assert!(text(&s).contains("› Song 99"));
    // Empty and loading states.
    e.ui.loading("QUICK FIND · x", "Searching…", Vec::new());
    assert!(text(&screen(&e)).contains("Searching…"));
    e.ui.show_empty("NAVIDROME", "Navidrome is not configured");
    let s = screen(&e);
    assert!(s[6].contains("Navidrome is not configured"));
}

#[test]
fn preview_pane_placeholder_and_details() {
    let e = env();
    let mut alb = Track::new("navidrome", "al1", "Below the Lights");
    alb.kind = "album".into();
    alb.artist = "Enslaved".into();
    alb.meta.insert("year".into(), 2003.into());
    alb.meta.insert("song_count".into(), 7.into());
    e.ui.show_list("ALBUMS", vec![Row::Track(alb)], "", None, true);
    let (s, _) = screen_with(&e, 120, 40, &Telemetry::default());
    let t = text(&s);
    assert!(t.contains("│  ALBUM   │"), "placeholder without kitty");
    assert!(t.contains("2003 · 7 tracks"));
    assert!(render::preview_geometry(17, 75).is_none());
    assert!(render::preview_geometry(16, 120).is_none());
    let g = render::preview_geometry(40, 120).unwrap();
    assert_eq!((g.cols, g.rows, g.col, g.right), (24, 13, 92, 89));
}

#[test]
fn filter_and_sort() {
    let e = env();
    let mut rows = Vec::new();
    for (i, ts) in [(1, 300.0), (2, 100.0), (3, 200.0)] {
        let mut t = track(&i.to_string());
        t.kind = "podcast".into();
        t.meta.insert("published_ts".into(), ts.into());
        rows.push(Row::Track(t));
    }
    e.ui.show_list("PODCAST", rows, "", None, true);
    let titles = |e: &Env| -> Vec<String> {
        e.sh.st()
            .items
            .iter()
            .map(|r| r.label().to_string())
            .collect()
    };
    assert!(text(&screen(&e)).contains("PODCAST  ·  SORT RELEVANCE"));
    e.sh.st().cycle_sort();
    assert_eq!(titles(&e), ["Song 1", "Song 3", "Song 2"]);
    assert_eq!(e.sh.st().message, "Sort · Newest");
    e.sh.st().cycle_sort();
    assert_eq!(titles(&e), ["Song 2", "Song 3", "Song 1"]);
    e.sh.st().cycle_sort();
    assert_eq!(titles(&e), ["Song 1", "Song 2", "Song 3"]);
    e.sh.st().filter_current(Some("SONG 3"));
    assert_eq!(titles(&e), ["Song 3"]);
    assert_eq!(e.sh.st().message, "Filter “song 3” · 1 matches");
    e.sh.st().filter_current(Some(""));
    assert_eq!(titles(&e).len(), 3);
    assert_eq!(e.sh.st().message, "Filter cleared · 3 items");
    e.sh.st().filter_current(None);
    assert_eq!(e.sh.st().message, "Filter cancelled");
    e.ui.show_list("PLAIN", vec![Row::Track(track("9"))], "", None, true);
    e.sh.st().cycle_sort();
    assert_eq!(
        e.sh.st().message,
        "Sort · no publication dates in this view"
    );
}

#[test]
fn wide_and_combining_titles_render() {
    let e = env();
    let mut t = track("x");
    t.title = "漢字のうた".into();
    t.artist = "Bjo\u{308}rk".into();
    e.ui.show_list("LIST", vec![Row::Track(t)], "", None, true);
    let t = text(&screen(&e));
    assert!(t.contains("漢字のうた"));
    assert!(t.contains("Björk") || t.contains("Bjo\u{308}rk"));
}

#[test]
fn prompt_rendering_and_secret() {
    let e = env();
    let got = Arc::new(Mutex::new(None::<Option<String>>));
    let g = got.clone();
    e.ui.prompt(
        "PASSWORD › ",
        "",
        true,
        Box::new(move |v| *g.lock().unwrap() = Some(v)),
    );
    {
        let mut st = e.sh.st();
        let p = st.prompt.as_mut().unwrap();
        for c in "abc".chars() {
            p.key(
                &KeyEvent::new(
                    crossterm::event::KeyCode::Char(c),
                    crossterm::event::KeyModifiers::NONE,
                ),
                &[],
                100,
            );
        }
    }
    let s = screen(&e);
    assert!(s[28].contains("PASSWORD › •••"));
    assert!(!text(&s).contains("abc"));
    // A second prompt replaces the first, which is cancelled.
    e.ui.prompt("NEXT › ", "x", false, Box::new(|_| {}));
    assert_eq!(*got.lock().unwrap(), Some(None));
    assert!(screen(&e)[28].contains("NEXT › x"));
    // Command prompt hint.
    e.sh.st().prompt = None;
    e.sh.start_prompt(":", "the", false, true, Box::new(|_| {}));
    let s = screen(&e);
    assert!(s[28].contains(":the"));
    assert!(s[29].contains("[theme]"));
}

#[test]
fn help_about_context_error_diagnostics() {
    let e = env();
    e.ui.show_menu("QUEUE", vec![Row::Track(track("1"))], true);
    e.ui.show_help();
    let s = screen(&e);
    let t = text(&s);
    assert!(t.contains("HELP · KEYBINDINGS"));
    assert!(t.contains("NAVIGATION"));
    assert!(t.contains("This view · Enter play   d remove"));
    assert!(t.contains("Esc              Back one view"));
    e.sh.st().back();
    assert_eq!(e.sh.st().title, "QUEUE");
    e.ui.show_about();
    let t = text(&screen(&e));
    assert!(t.contains(&format!("NPLAY {}", render::VERSION)));
    assert!(t.contains("Kitty artwork fallback"));
    let mut t1 = track("1");
    t1.meta.insert("sample_rate".into(), 44100.into());
    t1.meta.insert("bit_depth".into(), 24.into());
    t1.meta.insert("suffix".into(), "flac".into());
    t1.meta.insert("play_count".into(), 0.into());
    e.ui.show_info(&t1);
    let t = text(&screen(&e));
    assert!(t.contains("CONTEXT"));
    assert!(t.contains("Sample rate  44.1 kHz"));
    assert!(t.contains("Bit depth    24 bit"));
    assert!(t.contains("Format       FLAC"));
    assert!(t.contains("NPLAY rating —"));
    assert!(!t.contains("Play count"));
    e.ui.error("Navidrome unavailable", "Connection refused");
    let t = text(&screen(&e));
    assert!(t.contains("Navidrome unavailable") && t.contains("Esc  Back"));
    // Diagnostics with the service probe's hidden VIS line.
    e.sh.st().open_mode("diagnostics", "");
    e.sh.st().diag_vis = "VIS: CAVA, terminal off, 2 subscribers".into();
    let t = text(&screen(&e));
    assert!(t.contains("DIAGNOSTICS"));
    assert!(t.contains("Visualizer     VIS: CAVA, terminal off, 2 subscribers"));
    assert!(t.contains("Queue          0 items"));
}

#[test]
fn help_scrolls_and_shows_custom_keys() {
    let e = env();
    e.app.cfg.set_in("keys", "next", "N");
    let keys = Keymap::from_config(&e.app.cfg);
    let lines = render::help_lines(&keys);
    assert!(lines.iter().any(|(l, _)| l.contains("CUSTOM KEYS")));
    assert!(lines
        .iter()
        .any(|(l, _)| l.starts_with("N ") && l.contains("next")));
    assert!(render::help_max_scroll(&keys, 30) > 0);
    assert_eq!(keys.action("N"), Some("next"));
}

#[test]
fn theme_switching_live_and_custom_toml() {
    let e = env();
    e.ui.theme_menu();
    assert_eq!(e.sh.st().title, "THEME");
    let labels: Vec<String> =
        e.sh.st()
            .items
            .iter()
            .map(|r| r.label().to_string())
            .collect();
    assert!(labels[0].starts_with("✓ "));
    assert!(labels.iter().any(|l| l == "NO CUSTOM THEMES INSTALLED"));
    let depth = e.sh.st().stack.len();
    e.ui.set_theme("c64");
    assert_eq!(e.ui.theme(), "commodore64");
    assert_eq!(e.app.cfg.get("theme", ""), "commodore64");
    assert!(e.sh.st().message.starts_with("Theme · "));
    e.ui.theme_menu();
    assert_eq!(e.sh.st().stack.len(), depth, "refreshed in place");
    assert!(e
        .sh
        .st()
        .items
        .iter()
        .any(|r| r.label().starts_with("✓ ") && r.action() == "theme:commodore64"));
    e.ui.set_theme("nonexistent");
    assert_eq!(
        e.sh.st().message,
        "Theme not found · Theme → How to install themes"
    );
    assert_eq!(e.ui.theme(), "commodore64");
    // Built-in palettes use their muted colour, not A_DIM.
    let pairs = e.sh.st().pairs;
    assert_ne!(pairs.normal, pairs.muted);
    // A TOML custom theme appears after reload and can be selected.
    let dir = crate::theme::theme_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("my-theme.toml"),
        "[theme]\nid = \"my-theme\"\nname = \"My Theme\"\n\n[colors]\nfg = \"#E0E0E0\"\nbg = \"#151515\"\n",
    )
    .unwrap();
    e.app.open_source("settings:theme-reload", &e.ui);
    assert!(e.sh.st().message.starts_with("Themes reloaded"));
    assert!(e
        .sh
        .st()
        .items
        .iter()
        .any(|r| r.label().contains("My Theme · Custom")));
    e.ui.set_theme("my-theme");
    assert_eq!(e.ui.theme(), "my-theme");
    let st = e.sh.st();
    assert_eq!(st.palette.label, "My Theme");
    drop(st);
    // Activating a theme row goes through App::activate.
    let rows = e.sh.st().items.clone();
    let i = rows
        .iter()
        .position(|r| r.action() == "theme:satie")
        .unwrap();
    e.app.activate(&e.ui, "THEME", &rows, i);
    assert_eq!(e.ui.theme(), "satie");
    // Omarchy follow mode falls back cleanly without an Omarchy install.
    e.ui.set_theme("auto");
    assert_eq!(e.ui.theme(), "omarchy");
}

#[test]
fn layout_visualizer_and_styles() {
    let e = env();
    assert_eq!(e.ui.layout(), "normal");
    e.ui.cycle_layout();
    assert_eq!(e.ui.layout(), "artwork");
    assert_eq!(e.app.cfg.get("now_playing_view", ""), "artwork");
    assert_eq!(e.sh.st().message, "View · Artwork");
    e.ui.set_layout("visualizer");
    assert_eq!(e.ui.layout(), "visualizer");
    let on = e.ui.visualizer_on();
    e.ui.toggle_visualizer();
    assert_eq!(e.ui.visualizer_on(), !on);
    assert_eq!(e.app.cfg.getbool("visualizer", on), !on);
    e.ui.visualizer_style_menu();
    assert_eq!(e.sh.st().title, "CAVA STYLE");
    e.app.open_source("visual-style:dots", &e.ui);
    assert_eq!(e.ui.visual_style(), "dots");
    assert_eq!(e.sh.st().title, "CAVA STYLE");
    assert!(e.sh.st().items.iter().any(|r| r.label() == "✓ DOTS"));
    assert_eq!(e.sh.st().cycle_visual_style(), "classic");
}

#[test]
fn visualizer_glyphs() {
    let v = render::visual_lines(&[0, 255, 128], 2, "classic");
    assert_eq!(v, vec![" █ ".to_string(), " █▏".replace('▏', "█")]);
    let v = render::visual_lines(&[0, 255, 96], 4, "classic");
    assert_eq!(v[3].chars().nth(1), Some('█'));
    assert_eq!(v[0].chars().nth(2), Some(' '));
    assert_eq!(v[2].chars().nth(2), Some('▄'));
    let b = render::visual_lines(&[255, 0], 2, "blocks");
    assert_eq!(b, vec!["█ ".to_string(), "█ ".to_string()]);
    let d = render::visual_lines(&[255, 0], 1, "dots");
    assert_eq!(d, vec!["• ".to_string()]);
}

#[test]
fn visualizer_drawn_on_home_while_on() {
    let e = env();
    e.app.state().current = Some(track("1"));
    e.ui.set_layout("visualizer");
    if !e.ui.visualizer_on() {
        e.ui.toggle_visualizer();
    }
    // With no CAVA running the box is drawn flat (blank), below the text.
    let (s, _) = screen_with(&e, 100, 40, &Telemetry::default());
    assert!(s[16].trim().is_empty() && s[35].trim().is_empty());
    assert!(s[37].contains("─"));
}

#[test]
fn home_geometry_responsive() {
    let g = render::home_geometry(40, 120, true, "normal");
    assert!(g.art);
    assert_eq!((g.cols, g.rows, g.textx), (34, 20, 42));
    let a = render::home_geometry(40, 120, true, "artwork");
    assert_eq!((a.cols, a.rows), (52, 27));
    let v = render::home_geometry(40, 120, true, "visualizer");
    assert_eq!((v.cols, v.rows), (24, 10));
    let c = render::home_geometry(18, 60, true, "normal");
    assert!(c.art);
    assert_eq!((c.cols, c.rows, c.textx), (18, 7, 25));
    assert!(!render::home_geometry(15, 120, true, "normal").art);
    assert!(!render::home_geometry(40, 120, false, "normal").art);
    assert!(!render::home_geometry(40, 41, true, "normal").art);
}

// tests/test_statistics_ui.py, through the real UI.
#[test]
fn statistics_menu_renders_time_and_rankings() {
    let e = env();
    let mut t = Track::new("navidrome", "1", "Havenless");
    t.artist = "Enslaved".into();
    t.album = "Below the Lights".into();
    e.app
        .statistics
        .add("session", crate::paths::now(), 3.0, &t, None)
        .unwrap();
    e.app.stats_home(&e.ui, "all", true);
    let st = e.sh.st();
    assert!(st.title.starts_with("STATISTICS"));
    // The first selectable row is a period, so the overview stays visible.
    assert!(st.items[st.sel].action().starts_with("stats:period:"));
    drop(st);
    let (s, _) = screen_with(&e, 120, 40, &Telemetry::default());
    let t = text(&s);
    assert!(t.contains("ACTUAL LISTENING · 0m 03s"));
    assert!(t.contains("Enslaved · 0m 03s"));
    assert!(t.contains("Havenless · 0m 03s"));
    assert!(!t.contains("INSTALLATION ID"));
    // Period change replaces instead of stacking.
    let depth = e.sh.st().stack.len();
    e.app.stats_home(&e.ui, "day", false);
    assert_eq!(e.sh.st().stack.len(), depth);
    assert!(e.app.refresh_statistics_view(&e.ui));
    e.app.cfg.set("statistics_enabled", "false");
    e.app.stats_home(&e.ui, "week", true);
    assert!(text(&screen(&e)).contains("ENABLE LOCAL STATISTICS"));
}

#[test]
fn action_menu_and_info_through_ui() {
    let e = env();
    let t = track("7");
    e.app.action_menu(&e.ui, Some(t.clone()));
    let st = e.sh.st();
    assert!(st.title.starts_with("ACTIONS · Song 7"));
    assert!(st.items.iter().any(|r| r.action() == "ctx:nplay-radio"));
    drop(st);
    e.app.open_source("ctx:info", &e.ui);
    assert_eq!(e.sh.st().mode, "context");
    assert!(text(&screen(&e)).contains("Title        Song 7"));
    e.app.command("about", &e.ui);
    assert_eq!(e.sh.st().mode, "about");
    e.app.command("bogus", &e.ui);
    assert!(e.sh.st().message.starts_with("Unknown command"));
}

#[test]
fn ui_view_and_selection_for_engine() {
    let e = env();
    e.ui.show_menu(
        "STATISTICS · WEEK",
        vec![
            Row::header("H"),
            Row::entry("A", "a", ""),
            Row::entry("B", "b", ""),
        ],
        true,
    );
    let v = e.ui.view().unwrap();
    assert_eq!((v.mode.as_str(), v.sel, v.rows.len()), ("menu", 1, 3));
    e.ui.set_selection(2);
    assert_eq!(e.ui.view().unwrap().sel, 2);
    e.ui.edge_selection(false);
    assert_eq!(e.ui.view().unwrap().sel, 1);
    e.ui.update_items(vec![Row::entry("Z", "z", "")], 5);
    assert_eq!(e.ui.view().unwrap().sel, 0);
    e.ui.quit();
    assert!(!e.sh.st().running);
}

#[test]
fn posted_tasks_and_wakeups_are_queued() {
    let e = env();
    let hit = Arc::new(AtomicBool::new(false));
    let h = hit.clone();
    e.ui.post(Box::new(move || h.store(true, Ordering::SeqCst)));
    e.ui.status("a");
    e.ui.status("b");
    let mut tasks = 0;
    let mut wakes = 0;
    while let Ok(m) = e._rx.try_recv() {
        match m {
            Msg::Task(t) => {
                t();
                tasks += 1;
            }
            Msg::Wake => wakes += 1,
            _ => {}
        }
    }
    assert_eq!(tasks, 1);
    assert_eq!(wakes, 1, "wake-ups are coalesced");
    assert!(hit.load(Ordering::SeqCst));
    assert_eq!(e.sh.st().message, "b");
}

#[test]
fn state_colors_and_noir_selection() {
    let e = env();
    let st = e.sh.st();
    assert_eq!(st.theme, "niru-noir");
    assert!(st.palette.native);
    drop(st);
    let _ = TuiState::new;
}
