//! View state of the terminal UI (1.x `UI` fields and its pure view
//! methods): mode/title/rows/selection, the back stack, filter and sort,
//! appearance (theme, layout, visualizer), the active prompt and the
//! artwork bookkeeping. No terminal I/O and no `App` calls here, so the
//! engine can change views from any thread through the `Ui` impl.

use super::prompt::Prompt;
use crate::app::{Row, View};
use crate::artwork::Artwork;
use crate::model::Track;
use crate::theme::{Palette, ThemePairs};
use std::time::{Duration, Instant};

/// 1.x `snapshot()`: what `Esc` restores.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub mode: String,
    pub title: String,
    pub items: Vec<Row>,
    pub sel: usize,
    pub empty: String,
    pub message: String,
    pub context_target: Option<Track>,
}

pub const LAYOUTS: &[&str] = &["normal", "artwork", "visualizer"];
pub const STYLES: &[&str] = &["classic", "gradient", "blocks", "dots"];

pub struct TuiState {
    pub mode: String,
    pub title: String,
    pub items: Vec<Row>,
    pub sel: usize,
    pub empty: String,
    pub message: String,
    pub stack: Vec<Snapshot>,
    pub dirty: bool,
    pub running: bool,
    pub context_target: Option<Track>,
    pub unfiltered: Option<Vec<Row>>,
    pub sort_mode: String,
    pub preview_due: Option<Instant>,
    pub size: (u16, u16),
    // appearance
    pub theme: String,
    pub palette: Palette,
    pub pairs: ThemePairs,
    pub colors: u16,
    pub truecolor: bool,
    pub visual: bool,
    pub visual_style: String,
    pub layout: String,
    // prompt
    pub prompt: Option<Prompt>,
    pub command_history: Vec<String>,
    // artwork
    pub art: Artwork,
    pub art_visible: bool,
    /// Delete the image before the next frame (view change, theme, resize).
    pub art_clear: bool,
    // help / diagnostics
    pub help_scroll: usize,
    pub help_context: String,
    pub diag_vis: String,
    pub diag_yt: Option<String>,
    pub diag_log: Option<usize>,
}

impl TuiState {
    pub fn new(theme: &str, visual: bool, visual_style: &str, layout: &str, art: Artwork) -> Self {
        let (colors, truecolor) = terminal_colors();
        let layout = if LAYOUTS.contains(&layout) {
            layout
        } else {
            "normal"
        };
        let mut st = TuiState {
            mode: "home".into(),
            title: String::new(),
            items: Vec::new(),
            sel: 0,
            empty: String::new(),
            message: "Ready".into(),
            stack: Vec::new(),
            dirty: true,
            running: true,
            context_target: None,
            unfiltered: None,
            sort_mode: "relevance".into(),
            preview_due: None,
            size: (80, 24),
            theme: theme.to_string(),
            palette: Palette::default(),
            pairs: crate::theme::pairs(&Palette::default(), colors),
            colors,
            truecolor,
            visual,
            visual_style: visual_style.to_string(),
            layout: layout.to_string(),
            prompt: None,
            command_history: Vec::new(),
            art,
            art_visible: false,
            art_clear: false,
            help_scroll: 0,
            help_context: String::new(),
            diag_vis: String::new(),
            diag_yt: None,
            diag_log: None,
        };
        st.apply_theme();
        st
    }

    /// 1.x `theme.apply`: resolve the palette and its colour pairs.
    pub fn apply_theme(&mut self) {
        self.palette = crate::theme::resolve(&self.theme);
        self.pairs = if self.truecolor {
            self.palette.rgb_pairs()
        } else {
            crate::theme::pairs(&self.palette, self.colors)
        };
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn status(&mut self, s: &str) {
        self.message = s.to_string();
        self.invalidate();
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            mode: self.mode.clone(),
            title: self.title.clone(),
            items: self.items.clone(),
            sel: self.sel,
            empty: self.empty.clone(),
            message: self.message.clone(),
            context_target: self.context_target.clone(),
        }
    }

    pub fn restore(&mut self, s: Snapshot) {
        self.mode = s.mode;
        self.title = s.title;
        self.items = s.items;
        self.sel = s.sel;
        self.empty = s.empty;
        self.message = s.message;
        self.context_target = s.context_target;
        self.unfiltered = None;
        self.view_changed();
    }

    pub fn push(&mut self) {
        self.stack.push(self.snapshot());
        // Bounded: a long session of drilling down must not grow forever.
        if self.stack.len() > 200 {
            self.stack.remove(0);
        }
    }

    pub fn back(&mut self) {
        match self.stack.pop() {
            Some(s) => self.restore(s),
            None => self.go_home(true),
        }
    }

    /// 1.x `_view_changed`.
    pub fn view_changed(&mut self) {
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
        self.art_clear = true;
        self.art_visible = false;
        self.help_scroll = 0;
        self.preview_due = if self.mode == "list" || self.mode == "menu" {
            Some(Instant::now() + Duration::from_millis(100))
        } else {
            None
        };
        self.prefetch_selected();
        self.invalidate();
    }

    pub fn go_home(&mut self, clear_stack: bool) {
        self.mode = "home".into();
        self.items.clear();
        self.sel = 0;
        self.title.clear();
        if clear_stack {
            self.stack.clear();
        }
        self.view_changed();
    }

    pub fn now_playing(&mut self) {
        if self.mode != "home" {
            self.push();
        }
        self.go_home(false);
    }

    pub fn show_menu(&mut self, title: &str, rows: Vec<Row>, push: bool) {
        self.unfiltered = None;
        if push {
            self.push();
        }
        self.mode = "menu".into();
        self.title = title.to_string();
        self.items = rows;
        self.sel = 0;
        self.empty.clear();
        self.view_changed();
        self.edge_selection(false);
    }

    pub fn show_list(
        &mut self,
        title: &str,
        rows: Vec<Row>,
        empty: &str,
        message: Option<&str>,
        push: bool,
    ) {
        if push {
            self.push();
        }
        self.replace_list(title, rows, empty, message);
    }

    pub fn replace_list(
        &mut self,
        title: &str,
        rows: Vec<Row>,
        empty: &str,
        message: Option<&str>,
    ) {
        self.unfiltered = None;
        self.mode = "list".into();
        self.title = title.to_string();
        self.items = rows;
        self.sel = 0;
        self.empty = empty.to_string();
        self.message = match message {
            Some(m) => m.to_string(),
            None => format!("{} items", self.items.len()),
        };
        self.view_changed();
    }

    pub fn loading(&mut self, title: &str, msg: &str, rows: Vec<Row>) {
        self.mode = "list".into();
        self.title = title.to_string();
        self.items = rows;
        self.sel = 0;
        self.empty = msg.to_string();
        self.message = msg.to_string();
        self.view_changed();
    }

    pub fn error(&mut self, title: &str, msg: &str) {
        self.mode = "error".into();
        self.title = title.to_string();
        self.empty = msg.to_string();
        self.message = msg.to_string();
        self.view_changed();
    }

    pub fn open_mode(&mut self, mode: &str, title: &str) {
        let ctx = if self.mode == "help" {
            self.help_context.clone()
        } else {
            super::render::footer_help(self, 200, None)
        };
        self.push();
        self.mode = mode.to_string();
        if !title.is_empty() || mode == "about" {
            self.title = title.to_string();
        }
        if mode == "about" {
            self.items.clear();
            self.sel = 0;
        }
        self.help_context = ctx;
        self.view_changed();
    }

    pub fn show_context(&mut self, t: Option<Track>) {
        self.push();
        self.context_target = t;
        self.mode = "context".into();
        self.view_changed();
    }

    pub fn update_items(&mut self, rows: Vec<Row>, sel: usize) {
        self.items = rows;
        self.sel = sel.min(self.items.len().saturating_sub(1));
        self.unfiltered = None;
        self.invalidate();
    }

    pub fn view(&self) -> View {
        View {
            mode: self.mode.clone(),
            title: self.title.clone(),
            sel: self.sel,
            rows: self.items.clone(),
        }
    }

    /// The selected media item (headers and menu entries are not).
    pub fn selected(&self) -> Option<Track> {
        self.items.get(self.sel).and_then(Row::track).cloned()
    }

    pub fn selectable(&self, i: usize) -> bool {
        self.items.get(i).map(Row::selectable).unwrap_or(false)
    }

    fn touch_preview(&mut self) {
        self.preview_due = Some(Instant::now() + Duration::from_millis(100));
        self.prefetch_selected();
        self.invalidate();
    }

    pub fn move_selection(&mut self, delta: i64) {
        if self.items.is_empty() {
            return;
        }
        let mut i = self.sel as i64;
        loop {
            let j = i + delta;
            if j < 0 || j >= self.items.len() as i64 {
                break;
            }
            i = j;
            if self.selectable(i as usize) {
                break;
            }
        }
        if self.selectable(i as usize) {
            self.sel = i as usize;
        }
        self.touch_preview();
    }

    pub fn edge_selection(&mut self, last: bool) {
        let n = self.items.len();
        let found = if last {
            (0..n).rev().find(|&i| self.selectable(i))
        } else {
            (0..n).find(|&i| self.selectable(i))
        };
        if let Some(i) = found {
            self.sel = i;
        }
        self.touch_preview();
    }

    pub fn set_selection(&mut self, i: usize) {
        self.sel = i.min(self.items.len().saturating_sub(1));
        self.touch_preview();
    }

    pub fn page_selection(&mut self, delta: i64) {
        let step = (self.size.1 as i64 - 8).max(1);
        for _ in 0..step {
            self.move_selection(delta);
        }
    }

    pub fn prefetch_selected(&self) {
        if let Some(t) = self.items.get(self.sel).and_then(Row::track) {
            if !t.cover.is_empty() {
                self.art.prefetch(&t.cover);
            }
        }
    }

    /// 1.x `filter_current`; `None` = cancelled.
    pub fn filter_current(&mut self, q: Option<&str>) {
        let Some(q) = q else {
            self.status("Filter cancelled");
            return;
        };
        let base = self
            .unfiltered
            .get_or_insert_with(|| self.items.clone())
            .clone();
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            self.items = base;
            self.sel = 0;
            let n = self.items.len();
            self.status(&format!("Filter cleared · {n} items"));
            self.view_changed();
            return;
        }
        self.items = base
            .into_iter()
            .filter(|x| x.filter_text().to_lowercase().contains(&q))
            .collect();
        self.sel = 0;
        let n = self.items.len();
        self.status(&format!("Filter “{q}” · {n} matches"));
        self.view_changed();
    }

    fn has_dates(&self) -> bool {
        self.items
            .iter()
            .filter_map(Row::track)
            .any(|t| published_ts(t) != 0.0)
    }

    /// The `SORT …` suffix of a list heading.
    pub fn sort_heading(&self) -> String {
        if self.has_dates() {
            format!("  ·  SORT {}", self.sort_mode.to_uppercase())
        } else {
            String::new()
        }
    }

    /// 1.x `cycle_sort` (`S`).
    pub fn cycle_sort(&mut self) {
        if !self.has_dates() {
            self.status("Sort · no publication dates in this view");
            return;
        }
        if self.unfiltered.is_none() {
            self.unfiltered = Some(self.items.clone());
        }
        let modes = ["relevance", "newest", "oldest"];
        let i = modes.iter().position(|m| *m == self.sort_mode).unwrap_or(0);
        self.sort_mode = modes[(i + 1) % 3].to_string();
        self.apply_sort();
        self.sel = 0;
        let label = crate::app::capitalize(&self.sort_mode);
        self.status(&format!("Sort · {label}"));
    }

    fn apply_sort(&mut self) {
        if self.sort_mode == "relevance" {
            if let Some(u) = &self.unfiltered {
                self.items = u.clone();
            }
        } else {
            let newest = self.sort_mode == "newest";
            // Stable like Python's sorted(); headers sort as timestamp 0.
            let key = |r: &Row| r.track().map(published_ts).unwrap_or(0.0);
            self.items.sort_by(|a, b| {
                let o = key(a)
                    .partial_cmp(&key(b))
                    .unwrap_or(std::cmp::Ordering::Equal);
                if newest {
                    o.reverse()
                } else {
                    o
                }
            });
        }
    }

    pub fn set_sort_mode(&mut self, mode: &str) {
        if ["relevance", "newest", "oldest"].contains(&mode) {
            self.sort_mode = mode.to_string();
        }
    }

    pub fn theme_label(&self) -> String {
        crate::theme::available()
            .into_iter()
            .find(|(k, _)| *k == self.theme)
            .map(|(_, l)| l)
            .unwrap_or_else(|| self.theme.clone())
    }

    /// Rows of the theme chooser (1.x `theme_menu`).
    pub fn theme_rows(&self) -> Vec<Row> {
        let mut items: Vec<Row> = crate::theme::available()
            .into_iter()
            .map(|(k, label)| {
                let mark = if k == self.theme { "✓ " } else { "  " };
                Row::entry(
                    format!("{mark}{label}"),
                    format!("theme:{k}"),
                    "Select theme",
                )
            })
            .collect();
        if crate::theme::custom_themes().is_empty() {
            items.push(Row::entry(
                "NO CUSTOM THEMES INSTALLED",
                "",
                "Add .toml files to enable custom themes",
            ));
        }
        items.push(Row::entry(
            "HOW TO INSTALL THEMES",
            "settings:theme-help",
            "Step-by-step installation guide",
        ));
        items.push(Row::entry(
            "RELOAD CUSTOM THEMES",
            "settings:theme-reload",
            "Rescan installed TOML files",
        ));
        items
    }

    /// 1.x `set_theme`; returns the theme key to save, None when unknown.
    pub fn set_theme(&mut self, name: &str) -> Option<String> {
        let name = crate::theme::theme_alias(name);
        if !crate::theme::available().iter().any(|(k, _)| *k == name) {
            self.status("Theme not found · Theme → How to install themes");
            return None;
        }
        self.theme = name.clone();
        self.apply_theme();
        self.art_clear = true;
        self.art_visible = false;
        let label = self.theme_label();
        self.status(&format!("Theme · {label}"));
        Some(name)
    }

    pub fn visualizer_style_rows(&self) -> Vec<Row> {
        [
            ("CLASSIC", "classic", "Current NPLAY style · default"),
            ("GRADIENT", "gradient", "Theme-aware vertical gradient"),
            ("BLOCKS", "blocks", "Solid terminal bars"),
            ("DOTS", "dots", "Minimal dotted spectrum"),
        ]
        .iter()
        .map(|(label, key, desc)| {
            let mark = if *key == self.visual_style {
                "✓ "
            } else {
                "  "
            };
            Row::entry(
                format!("{mark}{label}"),
                format!("visual-style:{key}"),
                *desc,
            )
        })
        .collect()
    }

    pub fn cycle_visual_style(&mut self) -> String {
        let i = STYLES
            .iter()
            .position(|s| *s == self.visual_style)
            .unwrap_or(0);
        self.visual_style = STYLES[(i + 1) % STYLES.len()].to_string();
        let label = crate::app::capitalize(&self.visual_style);
        self.status(&format!("CAVA style · {label}"));
        self.visual_style.clone()
    }

    pub fn cycle_layout(&mut self) -> String {
        let i = LAYOUTS.iter().position(|s| *s == self.layout).unwrap_or(0);
        let next = LAYOUTS[(i + 1) % LAYOUTS.len()];
        self.set_layout(next);
        self.layout.clone()
    }

    pub fn set_layout(&mut self, layout: &str) {
        if LAYOUTS.contains(&layout) {
            self.layout = layout.to_string();
        }
        self.art_clear = true;
        self.art_visible = false;
        let label = crate::app::capitalize(&self.layout);
        self.status(&format!("View · {label}"));
    }
}

/// `meta.published_ts` as a number (0 when missing).
pub fn published_ts(t: &Track) -> f64 {
    t.meta
        .get("published_ts")
        .map(crate::model::value_f64)
        .unwrap_or(0.0)
}

/// Colours the terminal offers: (curses-like colour count, truecolor).
pub fn terminal_colors() -> (u16, bool) {
    let env = |k: &str| std::env::var(k).unwrap_or_default().to_lowercase();
    let ct = env("COLORTERM");
    let term = env("TERM");
    let truecolor = ct == "truecolor" || ct == "24bit";
    let colors = if truecolor
        || term.contains("256")
        || term.contains("kitty")
        || term.contains("direct")
        || env("TERM_PROGRAM") == "niruterm"
    {
        256
    } else if term.is_empty() || term == "dumb" || term == "vt100" {
        2
    } else if term == "linux" || term.starts_with("xterm") || term.starts_with("screen") {
        8
    } else {
        256
    };
    (colors, truecolor)
}
