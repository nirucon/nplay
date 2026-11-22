//! Drawing (port of 1.x `ui_render.py`): every view into a ratatui
//! [`Buffer`] at the 1.x coordinates, plus the geometry of the kitty cover
//! artwork. Rendering is pure: [`draw`] reads the [`TuiState`] and an
//! [`AppView`] snapshot and returns the [`ArtPlan`] the caller executes after
//! the text frame is flushed. ratatui's diff makes the 1.x
//! "visualizer-rectangle only" optimisation automatic.
//!
//! API: [`AppView::gather`], [`Telemetry`], [`draw`], [`footer_help`],
//! [`home_geometry`], [`preview_geometry`], [`help_lines`], [`ACTIONS`].

use super::keys::Keymap;
use super::state::TuiState;
use crate::app::{clip, App, Row};
use crate::model::{value_str, Track};
use crate::visualizer::Visualizer;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 1.x `ui_common.ACTIONS`: the help screen (empty description = heading).
pub const ACTIONS: &[(&str, &str)] = &[
    ("NAVIGATION", ""),
    ("Esc", "Back one view"),
    ("Ctrl+N", "Now Playing"),
    ("b", "Browse"),
    ("/", "Filter current list"),
    ("Ctrl+P", "Universal Quick Find"),
    ("q", "Queue"),
    ("i", "Information / context"),
    ("Tab", "Now Playing → Queue → Context"),
    ("PLAYBACK", ""),
    ("SPACE", "Play / pause"),
    ("z", "Stop and clear Now Playing"),
    ("n / p", "Next / previous track"),
    ("s", "Cycle shuffle: Off / Shuffle / Smart"),
    ("S", "Sort dated results: Relevance / Newest / Oldest"),
    ("x", "Cycle repeat: Off / Track / Context"),
    ("Enter", "Open / play"),
    ("← / →", "Seek ±5 s"),
    ("H / L", "Seek ±30 s"),
    ("+ / -", "Volume ±5"),
    ("m", "Mute"),
    ("QUEUE & LIBRARY", ""),
    ("a", "Add selected to queue"),
    ("A", "Add selected to playlist"),
    ("r", "Random in current source/list"),
    ("e", "Play selected next"),
    ("d", "Remove selected from queue/playlist"),
    ("J / K", "Move queue/playlist item down / up"),
    ("c", "Clear queue (while viewing Queue)"),
    ("W", "Save queue as playlist"),
    ("R", "Rename current playlist"),
    ("D", "Delete current playlist (confirmed)"),
    ("f", "Favorite current"),
    ("u", "Rescan local library"),
    ("VIEW", ""),
    (".", "Context actions for selected/current item"),
    ("v", "Cycle Normal / Artwork / Visualizer"),
    ("V", "Visualizer on / off"),
    ("C", "Cycle CAVA style"),
    ("j/k · ↑/↓", "Move selection"),
    ("g / G", "First / last"),
    ("OTHER", ""),
    (":", "Command palette / commands"),
    ("?", "Help"),
    ("Q", "Quit"),
];

/// Playback telemetry, refreshed by the loop (at most every 200 ms).
#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    pub active: bool,
    pub playing: bool,
    pub pos: f64,
    pub dur: f64,
}

/// The engine state one frame needs, read with one short lock.
#[derive(Clone, Debug, Default)]
pub struct AppView {
    pub current: Option<Track>,
    pub resume_candidate: bool,
    pub resume_position: f64,
    pub shuffle: String,
    pub repeat: String,
    pub ctx_len: usize,
    pub ctx_index: i64,
    pub ctx_same_album: bool,
    pub ctx_next_title: Option<String>,
    pub queue_len: usize,
    pub spotify_device: String,
    pub radio: Map<String, Value>,
    pub up_next: Option<Track>,
    pub playlist_pid: Option<i64>,
}

impl AppView {
    pub fn gather(app: &App) -> AppView {
        let mut v = {
            let st = app.state();
            let cur = st.current.clone();
            let albums: std::collections::HashSet<&str> = st
                .play_context
                .iter()
                .map(|z| z.album.as_str())
                .filter(|a| !a.is_empty())
                .collect();
            let idx = st.context_index;
            AppView {
                ctx_same_album: cur.as_ref().map(|t| !t.album.is_empty()).unwrap_or(false)
                    && albums.len() == 1,
                ctx_next_title: usize::try_from(idx + 1)
                    .ok()
                    .and_then(|i| st.play_context.get(i))
                    .map(|t| t.title.clone()),
                current: cur,
                resume_candidate: st.resume_candidate,
                resume_position: st.resume_position,
                shuffle: st.shuffle_mode.clone(),
                repeat: st.repeat_mode.clone(),
                ctx_len: st.play_context.len(),
                ctx_index: idx,
                queue_len: st.queue.len(),
                spotify_device: st
                    .spotify_state
                    .device
                    .get("name")
                    .map(value_str)
                    .unwrap_or_default(),
                radio: Map::new(),
                up_next: None,
                playlist_pid: st.current_playlist_pid,
            }
        };
        if let Some(t) = &v.current {
            if t.kind == "radio" {
                v.radio = app.radio_info(&t.id);
            } else {
                v.up_next = app.up_next();
            }
        }
        v
    }
}

/// Everything [`draw`] reads besides the view state.
pub struct Ctx<'a> {
    pub app: &'a App,
    pub av: &'a AppView,
    pub tele: &'a Telemetry,
    pub viz: Option<&'a Visualizer>,
    /// Kitty graphics available and `kitty_artwork` on.
    pub kitty: bool,
    pub keys: &'a Keymap,
    pub preview_ready: bool,
}

/// What to do with the cover image after the frame.
#[derive(Clone, Debug, PartialEq)]
pub enum ArtPlan {
    Keep,
    Clear,
    Draw {
        url: String,
        row: u32,
        col: u32,
        cols: u32,
        rows: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HomeGeo {
    pub art: bool,
    pub cols: u16,
    pub rows: u16,
    pub row: u16,
    pub col: u16,
    pub textx: u16,
}

/// 1.x `home_geometry`: the Now Playing cover box and text column.
pub fn home_geometry(h: i32, w: i32, kitty: bool, layout: &str) -> HomeGeo {
    let none = HomeGeo {
        art: false,
        cols: 0,
        rows: 0,
        row: 4,
        col: 4,
        textx: 4,
    };
    if w < 42 || h < 16 || !kitty {
        return none;
    }
    let compact = w < 70 || h < 20;
    let (cols, rows, textx);
    if compact {
        cols = (w - 32).clamp(10, 18);
        rows = 4.max(7.min(h - 10).min((cols as f64 * 0.55) as i32));
        textx = cols + 7;
    } else if layout == "artwork" {
        cols = 52.min(24.max(w / 2 - 8));
        rows = (9.max(h - 13)).min(12.max((cols as f64 * 0.62) as i32));
        textx = (w - 28).min(cols + 8);
    } else if layout == "visualizer" {
        cols = 24.min(14.max(w / 5));
        rows = (7.max(h / 4)).min(8.max((cols as f64 * 0.55) as i32));
        textx = (w - 28).min(cols + 8);
    } else {
        cols = 34.min(16.max(w / 3));
        rows = (8.max(h - 14)).min(9.max((cols as f64 * 0.60) as i32));
        textx = (w - 28).min(cols + 8);
    }
    if rows < 4 || w - textx < 20 {
        return none;
    }
    HomeGeo {
        art: true,
        cols: cols as u16,
        rows: rows as u16,
        row: 4,
        col: 4,
        textx: textx as u16,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewGeo {
    pub cols: u16,
    pub rows: u16,
    pub row: u16,
    pub col: u16,
    pub right: u16,
}

/// 1.x `preview_geometry`: the list preview pane (None below 76x17).
pub fn preview_geometry(h: i32, w: i32) -> Option<PreviewGeo> {
    if w < 76 || h < 17 {
        return None;
    }
    let cols = (w / 5).clamp(10, 24);
    let rows = 4.max(14.min(h - 10).min((cols as f64 * 0.55) as i32));
    let col = w - cols - 4;
    let right = col - 3;
    if right < 44 {
        return None;
    }
    Some(PreviewGeo {
        cols: cols as u16,
        rows: rows as u16,
        row: 5,
        col: col as u16,
        right: right as u16,
    })
}

struct Styles {
    normal: Style,
    muted: Style,
    accent: Style,
    select: Style,
    bold: Style,
}

fn styles(st: &TuiState) -> Styles {
    let p = &st.pairs;
    let pair =
        |(fg, bg): (ratatui::style::Color, ratatui::style::Color)| Style::new().fg(fg).bg(bg);
    let normal = pair(p.normal);
    // A_DIM made light themes illegible: built-in palettes use their muted
    // colour, terminal-native themes keep the terminal's own dim.
    let muted = if st.theme == "niru-noir" || st.theme == "omarchy" {
        normal.add_modifier(Modifier::DIM)
    } else {
        pair(p.muted)
    };
    let select = if st.theme == "niru-noir" {
        pair(p.select).add_modifier(Modifier::REVERSED)
    } else {
        pair(p.select)
    };
    Styles {
        normal,
        muted,
        accent: pair(p.accent),
        select,
        bold: normal.add_modifier(Modifier::BOLD),
    }
}

/// curses `addnstr`: at most `n` cells, clipped to the buffer.
fn put(buf: &mut Buffer, x: i32, y: i32, s: &str, n: i32, style: Style) {
    let a = buf.area;
    if x < 0 || y < 0 || n <= 0 || y >= a.height as i32 || x >= a.width as i32 {
        return;
    }
    let n = (n as usize).min((a.width as i32 - x) as usize);
    buf.set_stringn(x as u16, y as u16, s, n, style);
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Left-justify to `n` cells (Python `f'{s:<n}'`).
fn pad(s: &str, n: usize) -> String {
    let w = width(s);
    if w >= n {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(n - w))
    }
}

/// Center in `n` cells (Python `f'{s:^n}'`).
fn center(s: &str, n: usize) -> String {
    let w = width(s);
    if w >= n {
        return s.to_string();
    }
    let left = (n - w) / 2;
    format!("{}{s}{}", " ".repeat(left), " ".repeat(n - w - left))
}

/// A meta value when Python would find it truthy.
fn truthy(m: &Map<String, Value>, k: &str) -> Option<String> {
    match m.get(k)? {
        Value::Null => None,
        Value::Bool(false) => None,
        Value::String(s) if s.is_empty() => None,
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Array(a) if a.is_empty() => None,
        v => Some(value_str(v)),
    }
}

fn mstr(m: &Map<String, Value>, k: &str) -> String {
    m.get(k)
        .map(value_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn fmt(s: f64) -> String {
    let s = if s.is_finite() { s.max(0.0) as i64 } else { 0 };
    format!("{}:{:02}", s / 60, s % 60)
}

/// The transport line shown under every view except Now Playing.
pub fn transport_line(t: &Track, radio: &Map<String, Value>, tele: &Telemetry) -> String {
    if t.kind == "radio" {
        let prog = if t.source == "sr" {
            let p = mstr(radio, "program");
            if p.is_empty() {
                "LIVE".into()
            } else {
                p
            }
        } else {
            "LIVE".into()
        };
        format!("● {} · {prog}", t.title)
    } else {
        let dur = if tele.dur > 0.0 { tele.dur } else { t.duration };
        format!(
            "▶ {} · {}   {} / {}",
            t.artist,
            t.title,
            fmt(tele.pos),
            fmt(dur)
        )
    }
}

/// 1.x `footer_help`. `av`/`tele` None = describe a playing session.
pub fn footer_help(st: &TuiState, w: i32, play: Option<(&AppView, &Telemetry)>) -> String {
    if w < 76 {
        return "? help  ^N now  n/p track  b browse  Q quit".into();
    }
    if st.title == "QUEUE" {
        return "Enter play   d remove   J/K move   n/p track   Esc back   ? help".into();
    }
    if st.title.starts_with("PLAYLIST") {
        return "Enter play   r random   d remove   J/K move   R rename   D delete   Esc back   ? help"
            .into();
    }
    if st.mode == "list" || st.mode == "menu" {
        return "Enter open/play   / filter   S sort   . actions   a queue   A playlist   r random   Esc back   ? help".into();
    }
    let resume = play
        .map(|(av, tele)| {
            av.current
                .as_ref()
                .map(|c| !tele.active && (av.resume_candidate || c.source == "spotify"))
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if resume {
        "SPACE resume   z stop   n/p track   Enter resume   b browse   ? help".into()
    } else {
        "SPACE pause   z stop   n/p track   ←/→ seek   +/- volume   v view   i info   b browse   t theme   ? help".into()
    }
}

/// Draw one frame. Returns what to do with the cover image.
pub fn draw(buf: &mut Buffer, st: &TuiState, cx: &Ctx) -> ArtPlan {
    let area: Rect = buf.area;
    let s = styles(st);
    buf.set_style(area, s.normal);
    let (w, h) = (area.width as i32, area.height as i32);
    if h < 12 || w < 42 {
        put(buf, 0, 0, "NPLAY · terminal too small", w, s.normal);
        return if st.art_visible {
            ArtPlan::Clear
        } else {
            ArtPlan::Keep
        };
    }
    put(
        buf,
        2,
        1,
        "NPLAY",
        w - 4,
        s.accent.add_modifier(Modifier::BOLD),
    );
    put(
        buf,
        8.max(w - VERSION.len() as i32 - 2),
        1,
        VERSION,
        VERSION.len() as i32,
        s.muted,
    );
    match st.mode.as_str() {
        "home" => draw_home(buf, st, cx, &s, h, w),
        "menu" | "list" => draw_list(buf, st, cx, &s, h, w),
        "help" => draw_help(buf, st, cx, &s, h, w),
        "about" => draw_about(buf, cx, &s, w),
        "context" => draw_context(buf, st, cx, &s, h, w),
        "error" => draw_error(buf, st, &s, w),
        "diagnostics" => draw_diagnostics(buf, st, cx, &s, w),
        _ => {}
    }
    put(
        buf,
        2,
        h - 3,
        &"─".repeat((w - 4).max(1) as usize),
        w - 4,
        s.normal,
    );
    let mut footer = st.message.clone();
    if st.mode != "home" {
        if let Some(t) = &cx.av.current {
            footer = transport_line(t, &cx.av.radio, cx.tele);
        }
    }
    put(
        buf,
        2,
        h - 2,
        &clip(&footer, (w - 4) as usize),
        w - 4,
        s.muted,
    );
    put(
        buf,
        2,
        h - 1,
        &footer_help(st, w, Some((cx.av, cx.tele))),
        w - 4,
        s.muted,
    );
    if let Some(p) = &st.prompt {
        draw_prompt(buf, p, &s, h, w);
    }
    match st.mode.as_str() {
        "home" => art_home(st, cx, h, w),
        "list" | "menu" => {
            if cx.preview_ready {
                art_preview(st, cx, h, w)
            } else {
                ArtPlan::Keep
            }
        }
        _ if st.art_visible => ArtPlan::Clear,
        _ => ArtPlan::Keep,
    }
}

fn draw_prompt(buf: &mut Buffer, p: &super::prompt::Prompt, s: &Styles, h: i32, w: i32) {
    let blank = " ".repeat(w as usize);
    put(buf, 0, h - 2, &blank, w, s.normal);
    put(buf, 2, h - 2, &p.label, w - 4, s.bold);
    let x = p.input_x(w as usize) as i32;
    put(buf, x, h - 2, &p.shown(), (w - x - 2).max(1), s.normal);
    let hint = p.hint();
    if !hint.is_empty() {
        put(buf, 0, h - 1, &blank, w, s.normal);
        put(buf, 2, h - 1, &hint, w - 4, s.muted);
    }
}

fn art_home(st: &TuiState, cx: &Ctx, h: i32, w: i32) -> ArtPlan {
    let g = home_geometry(h, w, cx.kitty, &st.layout);
    match &cx.av.current {
        Some(t) if !t.cover.is_empty() && g.art && cx.kitty => ArtPlan::Draw {
            url: t.cover.clone(),
            row: g.row as u32,
            col: g.col as u32,
            cols: g.cols as u32,
            rows: g.rows as u32,
        },
        _ if st.art_visible => ArtPlan::Clear,
        _ => ArtPlan::Keep,
    }
}

fn art_preview(st: &TuiState, cx: &Ctx, h: i32, w: i32) -> ArtPlan {
    let g = preview_geometry(h, w);
    match (st.selected(), g) {
        (Some(x), Some(g)) if !x.cover.is_empty() && cx.kitty => ArtPlan::Draw {
            url: x.cover,
            row: g.row as u32,
            col: g.col as u32,
            cols: g.cols as u32,
            rows: g.rows as u32,
        },
        _ if st.art_visible => ArtPlan::Clear,
        _ => ArtPlan::Keep,
    }
}

fn placeholder(buf: &mut Buffer, x: i32, y: i32, w: i32, source: &str, label: &str, s: &Styles) {
    let sr_label = format!("│ {} │", center(label, 8));
    let lines: Vec<&str> = match source {
        "sr" => vec![
            "┌──────────┐",
            "│    SR    │",
            &sr_label,
            "│   LIVE   │",
            "└──────────┘",
        ],
        "youtube" => vec![
            "┌──────────┐",
            "│ YOUTUBE  │",
            "│  AUDIO   │",
            "└──────────┘",
        ],
        "local" => vec![
            "┌──────────┐",
            "│  NPLAY   │",
            "│  LOCAL   │",
            "└──────────┘",
        ],
        "navidrome" => vec![
            "┌──────────┐",
            "│  NPLAY   │",
            "│  ALBUM   │",
            "└──────────┘",
        ],
        "spotify" => vec![
            "┌──────────┐",
            "│ SPOTIFY  │",
            "│  NPLAY   │",
            "└──────────┘",
        ],
        _ => vec!["┌──────────┐", "│  NPLAY   │", "└──────────┘"],
    };
    for (i, l) in lines.iter().enumerate() {
        put(buf, x, y + i as i32, l, w.min(width(l) as i32), s.muted);
    }
}

fn key_label(cx: &Ctx, action: &str, default: &str) -> String {
    match cx.keys.key_for(action) {
        Some("SPACE") => "SPACE".into(),
        Some(k) if k.starts_with("CTRL+") => format!("^{}", &k[5..]),
        Some(k) => k.to_string(),
        None => default.to_string(),
    }
}

fn strip_track_no(title: &str) -> String {
    // 1.x: re.sub(r'^\s*\d{1,3}\s*[-._]\s*', '', title)
    let t = title.trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) {
        let rest = t[digits..].trim_start();
        if let Some(c) = rest.chars().next() {
            if matches!(c, '-' | '.' | '_') {
                return rest[1..].trim_start().trim().to_string();
            }
        }
    }
    title.trim().to_string()
}

fn draw_home(buf: &mut Buffer, st: &TuiState, cx: &Ctx, s: &Styles, h: i32, w: i32) {
    let app = cx.app;
    let Some(t) = &cx.av.current else {
        let y = 5.max(h / 3);
        put(buf, 4, y, "Nothing playing", w - 8, s.bold);
        let b = key_label(cx, "browse", "b");
        let c = key_label(cx, "command", ":");
        put(
            buf,
            4,
            y + 2,
            &format!("{}  Browse your music", pad(&b, 1)),
            w - 8,
            s.normal,
        );
        put(buf, 4, y + 3, "^P Universal Quick Find", w - 8, s.normal);
        put(
            buf,
            4,
            y + 4,
            &format!("{}  Command palette", pad(&c, 1)),
            w - 8,
            s.normal,
        );
        return;
    };
    let tele = cx.tele;
    let g = home_geometry(h, w, cx.kitty, &st.layout);
    let x = if g.art { g.textx as i32 } else { 4 };
    if g.art && t.cover.is_empty() {
        let label = if t.kind == "radio" {
            t.title.as_str()
        } else {
            ""
        };
        let label: String = label.chars().take(8).collect();
        placeholder(buf, 5, 5, g.cols as i32, &t.source, &label, s);
    }
    let tw = (w - x - 4).max(0) as usize;
    put(buf, x, 4, &clip(&t.title, tw), tw as i32, s.bold);
    put(buf, x, 5, &clip(&t.artist, tw), tw as i32, s.normal);
    put(buf, x, 6, &clip(&t.album, tw), tw as i32, s.muted);
    let mut meta = t.source.to_uppercase();
    if t.source == "spotify" {
        let local = app.cfg.get("spotify_playback", "local") == "local";
        meta += if local {
            " · LIBRESPOT"
        } else {
            " · CONNECT"
        };
        if local {
            if !tele.active {
                meta += " · PAUSED";
            } else {
                meta += &format!(" · {} KBPS", app.cfg.get("spotify_local_bitrate", "320"));
            }
        } else if !cx.av.spotify_device.is_empty() {
            meta += &format!(" · {}", cx.av.spotify_device);
        }
    }
    if let Some(suf) = truthy(&t.meta, "suffix") {
        meta += &format!(" · {}", suf.to_uppercase());
    }
    if let Some(br) = truthy(&t.meta, "bitrate") {
        meta += &format!(" · {br} kbps");
    }
    put(buf, x, 8, &meta, tw as i32, s.muted);
    let barw = 10.max(w - x - 5);
    if cx.av.resume_candidate && !tele.active {
        let hint = format!("Resume with SPACE · {}", fmt(cx.av.resume_position));
        put(buf, x, 9, &hint, barw, s.muted);
    }
    let bw = barw as usize;
    let mut cb: i32;
    if t.kind == "radio" {
        let info = &cx.av.radio;
        put(buf, x, 10, &format!("● LIVE · {}", t.title), barw, s.bold);
        cb = 11;
        let prog = mstr(info, "program");
        let ep = mstr(info, "episode");
        let song = mstr(info, "song");
        if !prog.is_empty() {
            put(buf, x, cb, &clip(&prog, bw), barw, s.normal);
            cb += 1;
        }
        if !ep.is_empty() && ep.to_lowercase() != prog.to_lowercase() {
            put(buf, x, cb, &clip(&ep, bw), barw, s.muted);
            cb += 1;
        }
        let times: Vec<String> = [mstr(info, "start"), mstr(info, "end")]
            .into_iter()
            .filter(|z| !z.is_empty())
            .collect();
        if !times.is_empty() {
            put(buf, x, cb, &clip(&times.join("–"), bw), barw, s.muted);
            cb += 1;
        }
        if !song.is_empty() {
            put(buf, x, cb, &clip(&format!("♪ {song}"), bw), barw, s.muted);
            cb += 1;
        }
        let nxt = mstr(info, "next_program");
        if !nxt.is_empty() && cb < h - 7 {
            let nt = mstr(info, "next_start");
            let line = format!(
                "NEXT  {nxt}{}",
                if nt.is_empty() {
                    String::new()
                } else {
                    format!(" · {nt}")
                }
            );
            put(buf, x, cb, &clip(&line, bw), barw, s.muted);
            cb += 1;
        }
    } else {
        let pos = tele.pos;
        let dur = if tele.dur > 0.0 { tele.dur } else { t.duration };
        let ratio = if dur > 0.0 {
            (pos / dur).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let n = ((barw - 1) as f64 * ratio) as i32;
        let n = n.min(barw - 1).max(0);
        let bar = format!(
            "{}●{}",
            "━".repeat(n as usize),
            "─".repeat((barw - n - 1).max(0) as usize)
        );
        put(buf, x, 10, &bar, barw, s.normal);
        put(
            buf,
            x,
            11,
            &format!("{}   {}", fmt(pos), fmt(dur)),
            barw,
            s.muted,
        );
        cb = 12;
    }
    let mut flags: Vec<String> = Vec::new();
    let not_radio_spotify = t.kind != "radio" && t.source != "spotify";
    if not_radio_spotify && app.cfg.getbool("normalization", true) {
        flags.push(format!(
            "RG {}",
            app.normalization_mode(Some(t)).to_uppercase()
        ));
    }
    if not_radio_spotify && app.cfg.getbool("gapless", false) {
        flags.push("GAPLESS".into());
    }
    if cx.av.shuffle != "off" && !cx.av.shuffle.is_empty() {
        flags.push(format!("SHUFFLE {}", cx.av.shuffle.to_uppercase()));
    }
    if cx.av.repeat != "off" && !cx.av.repeat.is_empty() {
        flags.push(format!("REPEAT {}", cx.av.repeat.to_uppercase()));
    }
    let av = cx.av;
    if t.kind != "radio"
        && av.ctx_len > 0
        && av.ctx_index >= 0
        && (av.ctx_index as usize) < av.ctx_len
        && cb < h - 7
    {
        let name = if av.ctx_same_album {
            t.album.clone()
        } else {
            let n = t.meta_str("context_name");
            if !n.is_empty() {
                n
            } else if t.source == "spotify" {
                "Spotify context".into()
            } else {
                "Playback context".into()
            }
        };
        let line = format!("{} of {} · {name}", av.ctx_index + 1, av.ctx_len);
        put(buf, x, cb, &clip(&line, bw), barw, s.muted);
        cb += 1;
    }
    if let Some(nxt) = av.up_next.as_ref().filter(|_| t.kind != "radio") {
        if cb < h - 7 {
            let mut nt = strip_track_no(&nxt.title);
            let artist = nxt.artist.trim();
            // Avoid "Artist - Title" duplication from filenames/tags.
            let pre = format!("{} - ", artist.to_lowercase());
            if !artist.is_empty() && nt.to_lowercase().starts_with(&pre) {
                nt = nt
                    .chars()
                    .skip(artist.chars().count() + 3)
                    .collect::<String>()
                    .trim()
                    .to_string();
            }
            let line = format!(
                "UP NEXT  {}{nt}",
                if artist.is_empty() {
                    String::new()
                } else {
                    format!("{artist} · ")
                }
            );
            put(buf, x, cb, &clip(&line, bw), barw, s.muted);
            cb += 1;
        }
    }
    if !flags.is_empty() && cb < h - 6 {
        put(buf, x, cb, &flags.join(" · "), barw, s.muted);
        cb += 1;
    }
    if st.visual {
        let art_bottom = if g.art && st.layout == "artwork" {
            (g.row + g.rows) as i32
        } else {
            cb
        };
        let available = (h - 4 - cb.max(art_bottom) - 1).max(0);
        let vh = match st.layout.as_str() {
            "visualizer" => 3.max(available.min(8.max(h / 2))),
            "artwork" => 2.max(available.min(6)),
            _ => 3.max(available.min(5.max(h / 4))),
        };
        // 1.x drew over the text when there was no room; stay below it.
        let vh = vh.min(available);
        if vh >= 2 {
            draw_visual(buf, st, cx, h - 4 - vh, h - 4, w, 3, s);
        }
    }
}

/// The spectrum rows for `vals` (0..255) in a box `rows` high (1.x
/// `draw_visual` glyph rules). Returns one string per row, top first.
pub fn visual_lines(vals: &[u8], rows: i32, style: &str) -> Vec<String> {
    const FRAC: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    (0..rows)
        .map(|r| {
            let th = (rows - r - 1) as f64;
            vals.iter()
                .map(|&v| {
                    let level = v as f64 / 255.0 * rows as f64;
                    match style {
                        "classic" => {
                            let f = (level - th).clamp(0.0, 1.0);
                            FRAC[((f * 8.0).round() as usize).min(8)]
                        }
                        "dots" => {
                            if level > th + 0.35 {
                                '•'
                            } else {
                                ' '
                            }
                        }
                        _ => {
                            if level > th {
                                '█'
                            } else {
                                ' '
                            }
                        }
                    }
                })
                .collect()
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_visual(
    buf: &mut Buffer,
    st: &TuiState,
    cx: &Ctx,
    y: i32,
    bottom: i32,
    w: i32,
    x: i32,
    s: &Styles,
) {
    let rows = 1.max(bottom - y);
    let width = 8.max(w - x - 4);
    let bars = 12.max(width) as usize;
    let vals = match cx.viz {
        Some(v) if v.running() => v.get(bars),
        _ => vec![0; bars],
    };
    let style = match st.visual_style.as_str() {
        s @ ("classic" | "gradient" | "blocks" | "dots") => s,
        _ => "classic",
    };
    for (r, line) in visual_lines(&vals, rows, style).iter().enumerate() {
        let r = r as i32;
        let attr = match style {
            "gradient" => {
                let ratio = (rows - r - 1) as f64 / 1.max(rows - 1) as f64;
                if ratio > 0.66 {
                    s.accent
                } else if ratio > 0.30 {
                    s.normal
                } else {
                    s.muted
                }
            }
            "dots" => s.accent,
            _ => s.muted,
        };
        put(buf, x, y + r, line, width, attr);
    }
}

/// One list row's label (1.x `draw_list` formatting).
pub fn row_label(x: &Row, items: &[Row], right: i32, radio: &dyn Fn(&str) -> String) -> String {
    match x {
        Row::Entry(e) => {
            let longest = items
                .iter()
                .filter_map(|z| match z {
                    Row::Entry(e) if !e.action.is_empty() => Some(width(&e.label)),
                    _ => None,
                })
                .max()
                .unwrap_or(18);
            let namew = (longest + 4).min(22.max(right / 2) as usize);
            format!("{}{}", pad(&e.label, namew), e.detail)
        }
        Row::Track(t) => {
            let maxline = 16.max(right - 8) as usize;
            let dated = |t: &Track| {
                let date = t.meta_str("published_date");
                if right >= 96 {
                    format!("{} {} {date}", pad(&t.title, 46), pad(&t.artist, 22))
                } else {
                    format!(
                        "{}  ·  {}{}",
                        t.title,
                        t.artist,
                        if date.is_empty() {
                            String::new()
                        } else {
                            format!("  ·  {date}")
                        }
                    )
                }
            };
            let label = match t.kind.as_str() {
                "radio" => {
                    let p = radio(&t.id);
                    format!(
                        "{} {}",
                        pad(&t.title, 10),
                        if p.is_empty() { "LIVE".into() } else { p }
                    )
                }
                "program" => format!("{}  ·  {}", t.title, t.artist),
                "album" => format!("{} {}", pad(&t.title, 38), t.artist),
                "artist" => t.title.clone(),
                "podcast" => dated(t),
                _ if t.source == "youtube" => dated(t),
                _ => format!("{}  ·  {}", t.title, t.artist),
            };
            clip(&label, maxline)
        }
    }
}

fn draw_list(buf: &mut Buffer, st: &TuiState, cx: &Ctx, s: &Styles, h: i32, w: i32) {
    let heading = format!("{}{}", st.title, st.sort_heading());
    put(buf, 4, 3, &heading, w - 8, s.bold);
    let pg = preview_geometry(h, w);
    let right = pg.map(|g| g.right as i32).unwrap_or(w - 4);
    let visible = 1.max(h - 8) as usize;
    let n = st.items.len();
    let start = (st.sel as i64 - (visible / 2) as i64)
        .min(n.saturating_sub(visible) as i64)
        .max(0) as usize;
    let radio = |id: &str| -> String { mstr(&cx.app.radio_info(id), "program") };
    for (row, i) in (start..n.min(start + visible)).enumerate() {
        let y = 5 + row as i32;
        let x = &st.items[i];
        if let Row::Entry(e) = x {
            if e.action.is_empty() {
                put(
                    buf,
                    4,
                    y,
                    &e.label,
                    1.max(right - 6),
                    s.accent.add_modifier(Modifier::BOLD),
                );
                continue;
            }
        }
        let label = row_label(x, &st.items, right, &radio);
        let mark = if i == st.sel { "› " } else { "  " };
        let style = if i == st.sel { s.select } else { s.normal };
        put(
            buf,
            4,
            y,
            &format!("{mark}{label}"),
            1.max(right - 6),
            style,
        );
    }
    if st.items.is_empty() {
        put(
            buf,
            4,
            6,
            &clip(&st.empty, (w - 10).max(0) as usize),
            w - 10,
            s.muted,
        );
    }
    let (Some(x), Some(g)) = (st.selected(), pg) else {
        return;
    };
    let cxx = right + 2;
    let cw = 12.max(w - cxx - 4);
    let hasart = !x.cover.is_empty() && cx.kitty;
    if !hasart {
        let label = if x.kind == "radio" {
            x.title.clone()
        } else {
            x.source.to_uppercase()
        };
        let label: String = label.chars().take(8).collect();
        placeholder(buf, cxx, 6, cw, &x.source, &label, s);
    }
    let mut y = (h - 8).min(if hasart {
        (g.row + g.rows + 1) as i32
    } else {
        12
    });
    if y < h - 5 {
        let cwu = cw as usize;
        put(buf, cxx, y, &clip(&x.title, cwu), cw, s.bold);
        y += 1;
        if !x.artist.is_empty() {
            put(buf, cxx, y, &clip(&x.artist, cwu), cw, s.normal);
            y += 1;
        }
        let details: Vec<String> = match x.kind.as_str() {
            "album" => vec![
                truthy(&x.meta, "year").unwrap_or_default(),
                truthy(&x.meta, "song_count")
                    .map(|c| format!("{c} tracks"))
                    .unwrap_or_default(),
            ],
            "program" => vec!["SR programme".into()],
            "podcast" => vec![
                x.meta_str("published_date"),
                if x.duration > 0.0 {
                    fmt(x.duration)
                } else {
                    "SR episode".into()
                },
            ],
            _ if x.source == "youtube" => vec![
                x.meta_str("published_date"),
                if x.duration > 0.0 {
                    fmt(x.duration)
                } else {
                    String::new()
                },
            ],
            _ => vec![],
        };
        let d: Vec<&str> = details
            .iter()
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .collect();
        if !d.is_empty() {
            put(buf, cxx, y, &clip(&d.join(" · "), cwu), cw, s.muted);
        }
    }
}

/// Help body lines: (text, is_heading).
pub fn help_lines(keys: &Keymap) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (key, desc) in ACTIONS {
        if desc.is_empty() {
            out.push((key.to_string(), true));
        } else {
            out.push((format!("{} {desc}", pad(key, 16)), false));
        }
    }
    let custom = keys.custom();
    if !custom.is_empty() {
        out.push(("CUSTOM KEYS · config.ini [keys]".into(), true));
        for (a, k) in custom {
            out.push((format!("{} {}", pad(&k, 16), a.replace('_', " ")), false));
        }
    }
    out
}

fn draw_help(buf: &mut Buffer, st: &TuiState, cx: &Ctx, s: &Styles, h: i32, w: i32) {
    put(buf, 4, 3, "HELP · KEYBINDINGS", w - 8, s.bold);
    put(buf, 4, 4, "Esc goes back one view · Ctrl+N opens Now Playing · Ctrl+P searches all sources · / filters the current view · n/p changes track · t changes theme · v changes layout", w - 8, s.muted);
    if !st.help_context.is_empty() {
        put(
            buf,
            4,
            5,
            &format!("This view · {}", st.help_context),
            w - 8,
            s.muted,
        );
    }
    let lines = help_lines(cx.keys);
    for (y, (text, heading)) in (6..).zip(lines.iter().skip(st.help_scroll)) {
        if y >= h - 4 {
            break;
        }
        if *heading {
            put(buf, 4, y, text, w - 8, s.bold);
        } else {
            put(buf, 5, y, text, w - 10, s.normal);
        }
    }
}

/// Lines the help view can scroll by in a terminal `h` rows high.
pub fn help_max_scroll(keys: &Keymap, h: i32) -> usize {
    let room = (h - 4 - 6).max(1) as usize;
    help_lines(keys).len().saturating_sub(room)
}

fn found(name: &str) -> bool {
    crate::paths::which(name).is_some()
}

fn draw_about(buf: &mut Buffer, cx: &Ctx, s: &Styles, w: i32) {
    let term = std::env::var("TERM").unwrap_or_else(|_| "unknown".into());
    let lines = [
        format!("NPLAY {VERSION}"),
        "Terminal-native music, radio & podcast player".into(),
        String::new(),
        "Custom-built for Nicklas Rudolfsson.".into(),
        "Fully functional and configurable for other users too.".into(),
        String::new(),
        "Rust · ratatui terminal UI".into(),
        format!("Terminal {term}"),
        format!(
            "Kitty artwork {}",
            if cx.kitty { "available" } else { "fallback" }
        ),
        format!("mpv {}", if found("mpv") { "available" } else { "missing" }),
        format!(
            "CAVA {}",
            if found("cava") {
                "available"
            } else {
                "optional / missing"
            }
        ),
        format!(
            "yt-dlp {}",
            if found("yt-dlp") {
                "available"
            } else {
                "optional / missing"
            }
        ),
        String::new(),
        "Local · Navidrome · Spotify · Sveriges Radio · YouTube".into(),
        "Normal · Artwork · Visualizer views".into(),
        "Keyboard first · responsive · provider resilient".into(),
    ];
    for (i, l) in lines.iter().enumerate() {
        let st = match i {
            0 => s.bold,
            1 | 13 | 14 => s.muted,
            _ => s.normal,
        };
        put(buf, 5, 4 + i as i32, l, w - 10, st);
    }
}

fn sample_rate(v: &str) -> String {
    let khz = v.parse::<f64>().map(|f| f.trunc()).unwrap_or(0.0) / 1000.0;
    // Python `{:g}`.
    let s = format!("{khz:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s} kHz")
}

/// Rows of the information view (1.x `draw_context`).
pub fn context_rows(app: &App, t: &Track, av: &AppView, w: i32) -> Vec<(String, String)> {
    let rating = app.db.rating_get(&t.id, &t.source).unwrap_or(0).max(0) as usize;
    let mut rows: Vec<(String, String)> = vec![
        ("Title".into(), t.title.clone()),
        ("Artist".into(), t.artist.clone()),
        ("Album".into(), t.album.clone()),
        ("Source".into(), t.source.to_uppercase()),
        ("Type".into(), t.kind.clone()),
        (
            "Duration".into(),
            if t.duration > 0.0 {
                fmt(t.duration)
            } else {
                "Live / unknown".into()
            },
        ),
        (
            "NPLAY rating".into(),
            if rating > 0 {
                "★".repeat(rating.min(5))
            } else {
                "—".into()
            },
        ),
    ];
    let wide = 20.max(w - 22) as usize;
    if !t.path.is_empty() {
        rows.push(("File".into(), clip(&t.path, wide)));
    }
    let is_current = av.current.as_ref().map(|c| c == t).unwrap_or(false);
    if is_current && av.ctx_len > 0 {
        rows.push((
            "Playing from".into(),
            format!("{} / {}", av.ctx_index + 1, av.ctx_len),
        ));
        if ((av.ctx_index + 1) as usize) < av.ctx_len {
            if let Some(n) = &av.ctx_next_title {
                rows.push(("Up next".into(), n.clone()));
            }
        }
    }
    let m = &t.meta;
    if let Some(Value::Array(a)) = m.get("available_sources") {
        if !a.is_empty() {
            let v: Vec<String> = a
                .iter()
                .map(|x| crate::app::capitalize(&value_str(x)))
                .collect();
            rows.push(("Available".into(), v.join(" · ")));
        }
    }
    let mut add = |label: &str, key: &str, f: &dyn Fn(String) -> String| {
        if let Some(v) = truthy(m, key) {
            rows.push((label.into(), f(v)));
        }
    };
    add("Play count", "play_count", &|v| v);
    add("Format", "suffix", &|v| v.to_uppercase());
    add("Bitrate", "bitrate", &|v| format!("{v} kbps"));
    add("Year", "year", &|v| v);
    add("Genre", "genre", &|v| v);
    add("Track", "track_no", &|v| v);
    add("Disc", "disc_no", &|v| v);
    add("Codec", "codec", &|v| v.to_uppercase());
    add("Sample rate", "sample_rate", &|v| sample_rate(&v));
    add("Bit depth", "bit_depth", &|v| format!("{v} bit"));
    add("Channels", "channels", &|v| v);
    add("About", "description", &|v| clip(&v, wide));
    rows
}

fn draw_context(buf: &mut Buffer, st: &TuiState, cx: &Ctx, s: &Styles, h: i32, w: i32) {
    put(buf, 4, 3, "CONTEXT", w - 8, s.bold);
    let Some(t) = st.context_target.as_ref().or(cx.av.current.as_ref()) else {
        put(buf, 5, 5, "Nothing selected or playing", w - 10, s.muted);
        return;
    };
    for (y, (a, b)) in (5..).zip(context_rows(cx.app, t, cx.av, w)) {
        if y >= h - 4 {
            break;
        }
        put(buf, 5, y, &format!("{} {b}", pad(&a, 12)), w - 10, s.normal);
    }
}

fn draw_error(buf: &mut Buffer, st: &TuiState, s: &Styles, w: i32) {
    put(buf, 4, 3, &st.title, w - 8, s.bold);
    put(
        buf,
        4,
        6,
        &clip(&st.empty, (w - 8).max(0) as usize),
        w - 8,
        s.normal,
    );
    put(buf, 4, 8, "Esc  Back", w - 8, s.muted);
    put(buf, 4, 9, "NPLAY remains available.", w - 8, s.muted);
}

/// The diagnostics lines (1.x `draw_diagnostics`).
pub fn diagnostics_lines(st: &TuiState, cx: &Ctx) -> Vec<String> {
    let a = cx.app;
    let c = &a.cfg;
    let onoff = |b: bool| if b { "on" } else { "off" };
    let cava = if !st.visual {
        "off"
    } else if cx.viz.map(|v| v.running()).unwrap_or(false) {
        "running"
    } else {
        "on · starts with playback"
    };
    let mpris = if a.mpris().available() {
        "ready"
    } else if c.getbool("mpris_enabled", true) {
        "enabled · unavailable"
    } else {
        "off"
    };
    let notes = if c.getbool("track_notifications", true) && a.notifier.available() {
        if c.getbool("notification_artwork", true) {
            "on · artwork"
        } else {
            "on · text only"
        }
    } else if c.getbool("track_notifications", true) {
        "enabled · unavailable"
    } else {
        "off"
    };
    let spotify = if c.getbool("spotify_enabled", false) {
        if a.spotify_configured() {
            "enabled / connected"
        } else {
            "enabled / setup required"
        }
    } else {
        "disabled"
    };
    let nav = if c.getbool("navidrome_enabled", true) {
        if a.nav_configured() {
            "enabled / configured"
        } else {
            "enabled / not configured"
        }
    } else {
        "disabled"
    };
    let sr = if c.getbool("sr_enabled", true) {
        if a.sr.last_cache() {
            "enabled · cached fallback"
        } else {
            "enabled · live/cache"
        }
    } else {
        "disabled"
    };
    let yt = if c.getbool("youtube_enabled", true) {
        if a.yt.available() {
            let v = match &st.diag_yt {
                None => "…".to_string(),
                Some(v) if v.is_empty() => "unknown".into(),
                Some(v) => v.clone(),
            };
            format!("enabled · yt-dlp {v}")
        } else {
            "enabled · yt-dlp missing".into()
        }
    } else {
        "disabled".into()
    };
    let norm = if c.getbool("normalization", true) {
        format!("on · {}", c.get("normalization_mode", "auto"))
    } else {
        "off".into()
    };
    let vis = if st.diag_vis.is_empty() {
        "VIS: CAVA".to_string()
    } else {
        st.diag_vis.clone()
    };
    let log = match st.diag_log {
        Some(n) => n.to_string(),
        None => "…".into(),
    };
    vec![
        format!("NPLAY          {VERSION}"),
        "Input          event-driven · 0 idle wakeups".into(),
        format!("Now view       {}", st.layout),
        "Render         responsive / dirty / 30 fps visual".into(),
        format!("CAVA           {cava}"),
        format!("Visualizer     {vis}"),
        format!(
            "Artwork        {}",
            if cx.kitty {
                "Kitty"
            } else {
                "Unicode fallback"
            }
        ),
        format!("MPRIS          {mpris}"),
        format!("Notifications  {notes}"),
        format!("Spotify        {spotify}"),
        format!("Navidrome      {nav}"),
        format!("SR             {sr}"),
        format!("YouTube        {yt}"),
        format!("Normalization  {norm}"),
        format!("Gapless        {}", onoff(c.getbool("gapless", false))),
        format!("Shuffle        {}", cx.av.shuffle),
        format!("Repeat         {}", cx.av.repeat),
        format!("Queue          {} items", cx.av.queue_len),
        format!(
            "Log            {log} recent warnings/errors · {}",
            crate::log::log_path().display()
        ),
    ]
}

fn draw_diagnostics(buf: &mut Buffer, st: &TuiState, cx: &Ctx, s: &Styles, w: i32) {
    put(buf, 4, 3, "DIAGNOSTICS", w - 8, s.bold);
    for (i, l) in diagnostics_lines(st, cx).iter().enumerate() {
        put(buf, 5, 5 + i as i32, l, w - 10, s.normal);
    }
}
