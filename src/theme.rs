//! Palettes (port of 1.x `nplay/theme.py` and `nplay/theme_actions.py`).
//!
//! Public API:
//! - [`Palette`]: `label`, `native` (Niru Noir: terminal-native colours) and
//!   the six semantic slots `fg`, `muted`, `accent`, `select_fg`, `select_bg`,
//!   `bg` as optional RGB, plus optional exact xterm-256 indices (`*_idx`).
//! - [`builtin(key)`], [`BUILTIN_KEYS`]: niru-noir, satie, c-larsson,
//!   hackerman, commodore64, othala, ingwaz.
//! - [`omarchy_palette()`]: best-effort palette from the active Omarchy or
//!   kitty theme (never a runtime dependency).
//! - [`theme_dir()`]: `$XDG_CONFIG_HOME/nplay/themes`; [`load_custom(path)`],
//!   [`custom_themes()`] (<= 128 files, invalid ones logged and skipped),
//!   [`validate_theme(path)`] -> (ok, "OK: Label (id)" | "Invalid theme: ..."),
//!   [`available()`] -> (key, label) rows, [`list_themes_lines()`] for
//!   `--list-themes` (`{key:20} {label}`).
//! - [`resolve(name)`]: the palette `apply(name)` would use (unknown names
//!   fall back to Niru Noir; Omarchy without a palette gives
//!   "Omarchy · fallback to Niru Noir"). [`theme_alias(name)`]: the UI's
//!   short names (`c64`, `noir`, `auto`, ...).
//! - [`pairs(palette, colors)`] -> [`ThemePairs`]: the four 1.x curses colour
//!   pairs (normal, muted, accent, selection) as ratatui colours for a
//!   terminal with `colors` colours (256-colour cube, exact indices, or the
//!   readable 8-colour fallback). [`Palette::rgb_pairs`] for truecolor.
//! - [`open_theme_directory(dir)`]: create and open the theme directory with
//!   `xdg-open` (argument list, new session, no shell).

use ratatui::style::Color;
use regex_lite::Regex;
use std::path::{Path, PathBuf};

pub type Rgb = (u8, u8, u8);

/// Semantic colour slots in the 1.x order.
pub const FIELDS: &[&str] = &["fg", "muted", "accent", "select_fg", "select_bg", "bg"];

/// Built-in theme keys in `--list-themes` order.
pub const BUILTIN_KEYS: &[&str] = &[
    "niru-noir",
    "satie",
    "c-larsson",
    "hackerman",
    "commodore64",
    "othala",
    "ingwaz",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Palette {
    pub label: String,
    /// Niru Noir: keep the terminal's own colours.
    pub native: bool,
    pub fg: Option<Rgb>,
    pub muted: Option<Rgb>,
    pub accent: Option<Rgb>,
    pub select_fg: Option<Rgb>,
    pub select_bg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub fg_idx: Option<u8>,
    pub muted_idx: Option<u8>,
    pub accent_idx: Option<u8>,
    pub select_fg_idx: Option<u8>,
    pub select_bg_idx: Option<u8>,
    pub bg_idx: Option<u8>,
}

impl Palette {
    /// RGB of a slot by name.
    pub fn get(&self, field: &str) -> Option<Rgb> {
        match field {
            "fg" => self.fg,
            "muted" => self.muted,
            "accent" => self.accent,
            "select_fg" => self.select_fg,
            "select_bg" => self.select_bg,
            "bg" => self.bg,
            _ => None,
        }
    }

    fn set(&mut self, field: &str, v: Option<Rgb>) {
        match field {
            "fg" => self.fg = v,
            "muted" => self.muted = v,
            "accent" => self.accent = v,
            "select_fg" => self.select_fg = v,
            "select_bg" => self.select_bg = v,
            "bg" => self.bg = v,
            _ => {}
        }
    }

    fn clear_indices(&mut self) {
        self.fg_idx = None;
        self.muted_idx = None;
        self.accent_idx = None;
        self.select_fg_idx = None;
        self.select_bg_idx = None;
        self.bg_idx = None;
    }

    /// The four pairs in truecolor (native palettes keep terminal defaults).
    pub fn rgb_pairs(&self) -> ThemePairs {
        if self.native {
            return pairs(self, 256);
        }
        let c = |x: Option<Rgb>| {
            x.map(|(r, g, b)| Color::Rgb(r, g, b))
                .unwrap_or(Color::Reset)
        };
        let bg = c(self.bg);
        ThemePairs {
            normal: (c(self.fg), bg),
            muted: (c(self.muted), bg),
            accent: (c(self.accent), bg),
            select: (c(self.select_fg), c(self.select_bg)),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rgbp(label: &str, c: [Rgb; 6], idx: [u8; 6]) -> Palette {
    Palette {
        label: label.into(),
        native: false,
        fg: Some(c[0]),
        muted: Some(c[1]),
        accent: Some(c[2]),
        select_fg: Some(c[3]),
        select_bg: Some(c[4]),
        bg: Some(c[5]),
        fg_idx: Some(idx[0]),
        muted_idx: Some(idx[1]),
        accent_idx: Some(idx[2]),
        select_fg_idx: Some(idx[3]),
        select_bg_idx: Some(idx[4]),
        bg_idx: Some(idx[5]),
    }
}

/// A built-in palette. Light themes use explicit xterm-256 indices as well as
/// RGB: rounding RGB to the 6x6x6 cube made C. Larsson's paper colour pink.
pub fn builtin(key: &str) -> Option<Palette> {
    // Slot order: fg, muted, accent, select_fg, select_bg, bg.
    Some(match key {
        "niru-noir" => Palette {
            label: "Niru Noir".into(),
            native: true,
            ..Default::default()
        },
        // Aged score/manuscript paper: warm-neutral field, ink-black graphite, sepia accent.
        "satie" => rgbp(
            "Satie",
            [
                (28, 28, 28),
                (68, 68, 68),
                (135, 95, 0),
                (238, 238, 215),
                (135, 95, 0),
                (208, 208, 208),
            ],
            [234, 238, 94, 230, 94, 252],
        ),
        // Sundborn watercolour: aged linen/ochre paper, forest green, olive, falun red.
        "c-larsson" => rgbp(
            "C. Larsson",
            [
                (0, 95, 0),
                (95, 95, 0),
                (175, 0, 0),
                (238, 238, 215),
                (95, 135, 95),
                (215, 215, 175),
            ],
            [22, 58, 124, 230, 65, 187],
        ),
        // High-contrast phosphor terminal.
        "hackerman" => rgbp(
            "Hackerman",
            [
                (0, 215, 0),
                (0, 135, 0),
                (95, 255, 0),
                (255, 255, 255),
                (0, 95, 0),
                (8, 8, 8),
            ],
            [40, 28, 82, 231, 22, 232],
        ),
        // Nordic stone / iron / winter sky.
        "othala" => rgbp(
            "Othala",
            [
                (215, 215, 205),
                (135, 135, 135),
                (95, 135, 175),
                (18, 18, 18),
                (175, 175, 175),
                (18, 18, 18),
            ],
            [252, 245, 67, 233, 250, 233],
        ),
        // Earth / seed / growth.
        "ingwaz" => rgbp(
            "Ingwaz",
            [
                (215, 205, 175),
                (135, 135, 95),
                (135, 175, 95),
                (18, 18, 18),
                (175, 175, 95),
                (28, 28, 18),
            ],
            [187, 101, 107, 233, 143, 234],
        ),
        // C64 blue field / light-blue type.
        "commodore64" => rgbp(
            "Commodore 64",
            [
                (175, 175, 255),
                (135, 135, 215),
                (215, 215, 255),
                (38, 38, 95),
                (175, 175, 255),
                (38, 38, 95),
            ],
            [147, 110, 189, 17, 147, 17],
        ),
        _ => return None,
    })
}

fn rgb_hex(h: &str) -> Option<Rgb> {
    let h = h.trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

fn walk(root: &Path, out: &mut Vec<PathBuf>, cap: usize) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        if out.len() >= cap {
            return;
        }
        let p = e.path();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(p.clone());
        }
        out.push(p);
    }
    for d in dirs {
        if out.len() >= cap {
            return;
        }
        walk(&d, out, cap);
    }
}

/// Best-effort palette discovery from Omarchy or kitty configuration.
pub fn omarchy_palette() -> Option<Palette> {
    omarchy_palette_in(&crate::paths::home_dir())
}

/// [`omarchy_palette`] under an explicit home directory.
pub fn omarchy_palette_in(home: &Path) -> Option<Palette> {
    let roots = [
        home.join(".config/omarchy/current/theme"),
        home.join(".config/omarchy/themes/current"),
        home.join(".config/kitty/current-theme.conf"),
        home.join(".config/kitty/kitty.conf"),
    ];
    let keys = ["background", "foreground", "color7", "color8", "color4"];
    let res: Vec<Regex> = keys
        .iter()
        .map(|k| {
            Regex::new(&format!(
                r"(?mi)^\s*{}\s*[=: ]\s*(#[0-9a-f]{{6}})",
                regex_lite::escape(k)
            ))
            .unwrap()
        })
        .collect();
    let mut vals: [Option<Rgb>; 5] = [None; 5];
    for root in &roots {
        let files = if root.is_file() {
            vec![root.clone()]
        } else if root.is_dir() {
            let mut v = Vec::new();
            walk(root, &mut v, 80);
            v
        } else {
            vec![]
        };
        for f in files {
            if !f.is_file() {
                continue;
            }
            let Ok(raw) = read_capped(&f, 1024 * 1024) else {
                continue;
            };
            let text = String::from_utf8_lossy(&raw);
            for (i, r) in res.iter().enumerate() {
                if vals[i].is_some() {
                    continue;
                }
                if let Some(c) = r.captures(&text) {
                    vals[i] = rgb_hex(&c[1]);
                }
            }
        }
    }
    let [bg, fg, c7, muted, c4] = vals;
    let (fg, bg) = (fg?, bg?);
    let accent = c4.or(c7);
    Some(Palette {
        label: "Omarchy".into(),
        fg: Some(fg),
        bg: Some(bg),
        muted: Some(muted.unwrap_or(fg)),
        accent: Some(accent.unwrap_or(fg)),
        select_fg: Some(bg),
        select_bg: Some(accent.unwrap_or(fg)),
        ..Default::default()
    })
}

fn read_capped(p: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(p)?.take(cap).read_to_end(&mut buf)?;
    Ok(buf)
}

/// `$XDG_CONFIG_HOME/nplay/themes` (declarative TOML, never executable code).
pub fn theme_dir() -> PathBuf {
    crate::paths::config().join("themes")
}

fn id_ok(key: &str) -> bool {
    let mut c = key.chars();
    matches!(c.next(), Some('a'..='z'))
        && key.len() <= 48
        && c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn hex_ok(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn os_err(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
                .to_string_lossy()
                .into_owned();
            format!("[Errno {code}] {msg}")
        }
        None => e.to_string(),
    }
}

/// Load and validate one custom theme file. Errors are the 1.x messages.
pub fn load_custom(path: &Path) -> Result<(String, Palette), String> {
    let is_toml = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase() == "toml")
        .unwrap_or(false);
    let is_link = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if !is_toml || is_link || !path.is_file() {
        return Err("Expected a regular .toml file (not a symlink)".into());
    }
    let size = std::fs::metadata(path).map_err(|e| os_err(&e))?.len();
    if size > 16384 {
        return Err("Theme exceeds 16 KiB".into());
    }
    let bytes = std::fs::read(path).map_err(|e| os_err(&e))?;
    let text = String::from_utf8(bytes).map_err(|_| "Invalid UTF-8 in theme file".to_string())?;
    let data: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| e.message().to_string())?;
    if data.keys().any(|k| k != "theme" && k != "colors") {
        return Err("Unknown section".into());
    }
    let (Some(toml::Value::Table(meta)), Some(toml::Value::Table(colors))) =
        (data.get("theme"), data.get("colors"))
    else {
        return Err("Missing [theme] or [colors]".into());
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let key = match meta.get("id") {
        Some(toml::Value::String(k)) if id_ok(k) && *k == stem => k.clone(),
        _ => return Err("id must match filename".into()),
    };
    if builtin(&key).is_some() || key == "omarchy" {
        return Err("Reserved theme id".into());
    }
    if meta
        .keys()
        .any(|k| !["id", "name", "author", "version"].contains(&k.as_str()))
        || colors.keys().any(|k| !FIELDS.contains(&k.as_str()))
    {
        return Err("Unknown theme field".into());
    }
    let label = match meta.get("name") {
        None => key.clone(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => return Err("Invalid name".into()),
    };
    let n = label.chars().count();
    if !(1..=48).contains(&n) || label.chars().any(|c| (c as u32) < 32) {
        return Err("Invalid name".into());
    }
    if colors.is_empty() {
        return Err("At least one color required".into());
    }
    let mut p = builtin("othala").unwrap_or_default();
    p.clear_indices();
    p.label = label;
    for (field, value) in colors {
        match value {
            toml::Value::String(v) if hex_ok(v) => p.set(field, rgb_hex(v)),
            _ => return Err(format!("{field}: expected #RRGGBB")),
        }
    }
    Ok((key, p))
}

/// Valid custom themes in `dir`, sorted by file name (at most 128 files).
pub fn custom_themes_in(dir: &Path) -> Vec<(String, Palette)> {
    let mut found: Vec<(String, Palette)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    for path in paths.into_iter().take(128) {
        match load_custom(&path) {
            Ok((key, palette)) => {
                found.retain(|(k, _)| *k != key);
                found.push((key, palette));
            }
            Err(exc) => {
                crate::warn!("Invalid theme {}: {}", path.display(), exc);
            }
        }
    }
    found
}

/// Valid custom themes in [`theme_dir`].
pub fn custom_themes() -> Vec<(String, Palette)> {
    custom_themes_in(&theme_dir())
}

fn expanduser(p: &str) -> PathBuf {
    if p == "~" {
        return crate::paths::home_dir();
    }
    match p.strip_prefix("~/") {
        Some(rest) => crate::paths::home_dir().join(rest),
        None => PathBuf::from(p),
    }
}

/// `--check-theme FILE`: `(true, "OK: Label (id)")` or `(false, "Invalid theme: why")`.
pub fn validate_theme(path: &str) -> (bool, String) {
    match load_custom(&expanduser(path)) {
        Ok((key, p)) => (true, format!("OK: {} ({key})", p.label)),
        Err(e) => (false, format!("Invalid theme: {e}")),
    }
}

/// Built-ins, Omarchy follow mode, then custom themes (`label · Custom`).
pub fn available() -> Vec<(String, String)> {
    available_with(&custom_themes())
}

fn available_with(custom: &[(String, Palette)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUILTIN_KEYS
        .iter()
        .map(|k| {
            (
                k.to_string(),
                builtin(k).map(|p| p.label).unwrap_or_default(),
            )
        })
        .collect();
    out.push(("omarchy".into(), "Omarchy · follow active theme".into()));
    out.extend(
        custom
            .iter()
            .map(|(k, p)| (k.clone(), format!("{} · Custom", p.label))),
    );
    out
}

/// `--list-themes` output lines (`{key:20} {label}`).
pub fn list_themes_lines() -> Vec<String> {
    available()
        .into_iter()
        .map(|(k, l)| format!("{k:20} {l}"))
        .collect()
}

/// The UI's theme aliases (lower-cased, trimmed).
pub fn theme_alias(name: &str) -> String {
    let n = name.trim().to_lowercase();
    match n.as_str() {
        "noir" | "niru" => "niru-noir",
        "larsson" | "c.larsson" => "c-larsson",
        "hack" | "hacker" => "hackerman",
        "c64" | "commodore" | "commodore-64" => "commodore64",
        "oth" => "othala",
        "ing" => "ingwaz",
        "auto" => "omarchy",
        _ => return n,
    }
    .to_string()
}

/// The palette applied for `name`.
pub fn resolve(name: &str) -> Palette {
    if name == "omarchy" {
        return omarchy_palette().unwrap_or_else(|| {
            let mut p = builtin("niru-noir").unwrap_or_default();
            p.label = "Omarchy · fallback to Niru Noir".into();
            p
        });
    }
    if let Some(p) = builtin(name) {
        return p;
    }
    custom_themes()
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|x| x.1)
        .unwrap_or_else(|| builtin("niru-noir").unwrap_or_default())
}

/// The four 1.x colour pairs: (foreground, background) each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemePairs {
    /// Pair 1: normal text.
    pub normal: (Color, Color),
    /// Pair 2: muted text.
    pub muted: (Color, Color),
    /// Pair 3: accent.
    pub accent: (Color, Color),
    /// Pair 4: selection.
    pub select: (Color, Color),
}

/// xterm-256 colour for an RGB value (6x6x6 cube).
pub fn cube_index(rgb: Rgb) -> u8 {
    let q = |v: u8| (v as f64 / 255.0 * 5.0).round() as u16;
    (16 + 36 * q(rgb.0) + 6 * q(rgb.1) + q(rgb.2)) as u8
}

fn color(rgb: Option<Rgb>, exact: Option<u8>, colors: u16) -> Color {
    if let (Some(i), true) = (exact, colors >= 256) {
        return Color::Indexed(i);
    }
    let Some(rgb) = rgb else {
        return Color::Reset;
    };
    if colors >= 256 {
        return Color::Indexed(cube_index(rgb));
    }
    // Basic terminals: light colours become white, dark ones black.
    let avg = (rgb.0 as f64 + rgb.1 as f64 + rgb.2 as f64) / 3.0;
    if avg < 128.0 {
        Color::Black
    } else {
        Color::White
    }
}

/// Colour pairs for a terminal with `colors` colours (curses `COLORS`).
pub fn pairs(p: &Palette, colors: u16) -> ThemePairs {
    let r = Color::Reset;
    if p.native {
        // Niru Noir stays terminal-native; muted is bright black when available.
        let muted = if colors >= 16 { Color::Indexed(8) } else { r };
        return ThemePairs {
            normal: (r, r),
            muted: (muted, r),
            accent: (r, r),
            select: (r, r),
        };
    }
    if colors < 256 {
        // Portable readable fallback when only the basic ANSI palette is available.
        let b = p.bg.unwrap_or((0, 0, 0));
        let light_bg = (b.0 as f64 + b.1 as f64 + b.2 as f64) / 3.0 >= 128.0;
        let (bg, fg) = if light_bg {
            (Color::White, Color::Black)
        } else {
            (Color::Black, Color::White)
        };
        let acc = if light_bg { Color::Red } else { Color::Green };
        return ThemePairs {
            normal: (fg, bg),
            muted: (fg, bg),
            accent: (acc, bg),
            select: (bg, fg),
        };
    }
    let bg = if p.bg.is_some() {
        color(p.bg, p.bg_idx, colors)
    } else {
        r
    };
    ThemePairs {
        normal: (color(p.fg, p.fg_idx, colors), bg),
        muted: (color(p.muted, p.muted_idx, colors), bg),
        accent: (color(p.accent, p.accent_idx, colors), bg),
        select: (
            color(p.select_fg, p.select_fg_idx, colors),
            color(p.select_bg, p.select_bg_idx, colors),
        ),
    }
}

/// Open the theme directory in the desktop file manager (no shell).
pub fn open_theme_directory(dir: &Path) -> (bool, String) {
    open_theme_directory_with(dir, crate::paths::which("xdg-open"))
}

/// [`open_theme_directory`] with an explicit opener (tests).
pub fn open_theme_directory_with(dir: &Path, opener: Option<PathBuf>) -> (bool, String) {
    use std::os::unix::process::CommandExt;
    if let Err(e) = std::fs::create_dir_all(dir) {
        return (
            false,
            format!("Cannot open theme directory · {}", os_err(&e)),
        );
    }
    let is_link = std::fs::symlink_metadata(dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_link || !dir.is_dir() {
        return (false, "Theme directory is not a regular directory".into());
    }
    let Some(opener) = opener else {
        return (
            false,
            format!("No desktop file opener found · {}", dir.display()),
        );
    };
    let mut cmd = std::process::Command::new(&opener);
    cmd.arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap the opener without blocking the caller.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            (true, "Theme directory opened".into())
        }
        Err(e) => (
            false,
            format!("Cannot open theme directory · {}", os_err(&e)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("nplay-theme-test-{:x}", fastrand::u64(..)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const GOOD: &str = "[theme]\nid = \"my-theme\"\nname = \"My Theme\"\nauthor = \"X\"\nversion = \"1.0\"\n\n[colors]\nfg = \"#E0E0E0\"\nbg = \"#151515\"\n";

    #[test]
    fn builtins_and_available() {
        let rows = available_with(&[]);
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[0], ("niru-noir".into(), "Niru Noir".into()));
        assert_eq!(rows[4], ("commodore64".into(), "Commodore 64".into()));
        assert_eq!(
            rows[7],
            ("omarchy".into(), "Omarchy · follow active theme".into())
        );
        assert_eq!(
            format!("{:20} {}", rows[2].0, rows[2].1),
            "c-larsson            C. Larsson"
        );
        let c = builtin("c-larsson").unwrap();
        assert_eq!((c.bg, c.bg_idx), (Some((215, 215, 175)), Some(187)));
        assert!(builtin("niru-noir").unwrap().native);
        assert!(builtin("nope").is_none());
        assert_eq!(theme_alias(" C64 "), "commodore64");
        assert_eq!(theme_alias("auto"), "omarchy");
        assert_eq!(theme_alias("Satie"), "satie");
    }

    #[test]
    fn custom_theme_validation() {
        let d = tmp();
        let ok = d.join("my-theme.toml");
        std::fs::write(&ok, GOOD).unwrap();
        let (k, p) = load_custom(&ok).unwrap();
        assert_eq!(k, "my-theme");
        assert_eq!(p.label, "My Theme");
        assert_eq!(p.fg, Some((0xE0, 0xE0, 0xE0)));
        assert_eq!(p.accent, builtin("othala").unwrap().accent);
        assert_eq!(p.fg_idx, None);
        assert_eq!(
            validate_theme(ok.to_str().unwrap()),
            (true, "OK: My Theme (my-theme)".into())
        );
        let cases: &[(&str, &str, &str)] = &[
            ("other.toml", GOOD, "id must match filename"),
            (
                "satie.toml",
                "[theme]\nid=\"satie\"\n[colors]\nfg=\"#000000\"\n",
                "Reserved theme id",
            ),
            (
                "omarchy.toml",
                "[theme]\nid=\"omarchy\"\n[colors]\nfg=\"#000000\"\n",
                "Reserved theme id",
            ),
            (
                "a.toml",
                "[theme]\nid=\"a\"\n[colors]\nfg=\"#000000\"\n[x]\n",
                "Unknown section",
            ),
            (
                "b.toml",
                "[theme]\nid=\"b\"\n",
                "Missing [theme] or [colors]",
            ),
            (
                "c.toml",
                "[theme]\nid=\"c\"\nfoo=1\n[colors]\nfg=\"#000000\"\n",
                "Unknown theme field",
            ),
            (
                "d.toml",
                "[theme]\nid=\"d\"\n[colors]\nlink=\"#000000\"\n",
                "Unknown theme field",
            ),
            (
                "e.toml",
                "[theme]\nid=\"e\"\n[colors]\nfg=\"red\"\n",
                "fg: expected #RRGGBB",
            ),
            (
                "f.toml",
                "[theme]\nid=\"f\"\n[colors]\n",
                "At least one color required",
            ),
            (
                "g.toml",
                "[theme]\nid=\"g\"\nname=\"\"\n[colors]\nfg=\"#000000\"\n",
                "Invalid name",
            ),
            (
                "h.toml",
                "[theme]\nid=\"h\"\nname=\"a\\tb\"\n[colors]\nfg=\"#000000\"\n",
                "Invalid name",
            ),
            (
                "Upper.toml",
                "[theme]\nid=\"Upper\"\n[colors]\nfg=\"#000000\"\n",
                "id must match filename",
            ),
            (
                "x.json",
                GOOD,
                "Expected a regular .toml file (not a symlink)",
            ),
        ];
        for (name, body, want) in cases {
            let p = d.join(name);
            std::fs::write(&p, body).unwrap();
            assert_eq!(load_custom(&p).unwrap_err(), *want, "{name}");
            assert_eq!(
                validate_theme(p.to_str().unwrap()),
                (false, format!("Invalid theme: {want}"))
            );
        }
        let big = d.join("big.toml");
        std::fs::write(&big, format!("{GOOD}#{}", "x".repeat(17000))).unwrap();
        assert_eq!(load_custom(&big).unwrap_err(), "Theme exceeds 16 KiB");
        let link = d.join("link.toml");
        std::os::unix::fs::symlink(&ok, &link).unwrap();
        assert_eq!(
            load_custom(&link).unwrap_err(),
            "Expected a regular .toml file (not a symlink)"
        );
        let broken = d.join("broken.toml");
        std::fs::write(&broken, "[theme\n").unwrap();
        assert!(validate_theme(broken.to_str().unwrap())
            .1
            .starts_with("Invalid theme: "));
        // Only valid ones are listed.
        let found = custom_themes_in(&d);
        assert_eq!(
            found.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["my-theme"]
        );
        let rows = available_with(&found);
        assert_eq!(rows[8], ("my-theme".into(), "My Theme · Custom".into()));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn omarchy_discovery() {
        let home = tmp();
        assert!(omarchy_palette_in(&home).is_none());
        let theme = home.join(".config/omarchy/current/theme");
        std::fs::create_dir_all(theme.join("sub")).unwrap();
        std::fs::write(
            theme.join("alacritty.toml"),
            "[colors]\nbackground = \"#202020\"\nbackground #101010\n",
        )
        .unwrap();
        std::fs::write(
            theme.join("sub/kitty.conf"),
            "foreground #E0E0E0\ncolor8 #808080\ncolor4: #3366cc\n",
        )
        .unwrap();
        let p = omarchy_palette_in(&home).unwrap();
        assert_eq!(p.label, "Omarchy");
        assert_eq!(p.bg, Some((16, 16, 16)));
        assert_eq!(p.fg, Some((224, 224, 224)));
        assert_eq!(p.muted, Some((128, 128, 128)));
        assert_eq!(p.accent, Some((0x33, 0x66, 0xcc)));
        assert_eq!((p.select_fg, p.select_bg), (p.bg, p.accent));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn colour_pairs() {
        let noir = builtin("niru-noir").unwrap();
        assert_eq!(pairs(&noir, 256).muted.0, Color::Indexed(8));
        assert_eq!(pairs(&noir, 8).muted.0, Color::Reset);
        let c = builtin("c-larsson").unwrap();
        let p = pairs(&c, 256);
        assert_eq!(p.normal, (Color::Indexed(22), Color::Indexed(187)));
        assert_eq!(p.select, (Color::Indexed(230), Color::Indexed(65)));
        let basic = pairs(&c, 8);
        assert_eq!(basic.normal, (Color::Black, Color::White));
        assert_eq!(basic.accent.0, Color::Red);
        assert_eq!(basic.select, (Color::White, Color::Black));
        let dark = pairs(&builtin("hackerman").unwrap(), 8);
        assert_eq!(dark.accent, (Color::Green, Color::Black));
        assert_eq!(cube_index((255, 215, 215)), 224);
        let mut custom = builtin("othala").unwrap();
        custom.clear_indices();
        custom.fg = Some((0, 0, 0));
        assert_eq!(pairs(&custom, 256).normal.0, Color::Indexed(16));
        assert_eq!(
            custom.rgb_pairs().normal,
            (Color::Rgb(0, 0, 0), Color::Rgb(18, 18, 18))
        );
        assert_eq!(resolve("definitely-not-a-theme").label, "Niru Noir");
    }

    #[test]
    fn theme_directory_actions() {
        let home = tmp();
        let target = home.join("nplay/themes");
        let log = home.join("args");
        let opener = home.join("xdg-open");
        std::fs::write(
            &opener,
            format!(
                "#!/bin/sh\nprintf '%s|\\n' \"$@\" > {}\nps -o sid= -p $$ >> {}\n",
                log.display(),
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (ok, msg) = open_theme_directory_with(&target, Some(opener.clone()));
        assert!(ok, "{msg}");
        assert_eq!(msg, "Theme directory opened");
        assert!(target.is_dir());
        for _ in 0..100 {
            if std::fs::read_to_string(&log)
                .map(|s| s.lines().count() >= 2)
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let got = std::fs::read_to_string(&log).unwrap();
        assert!(got.starts_with(&format!("{}|", target.display())), "{got}");
        let sid: i32 = got.lines().nth(1).unwrap().trim().parse().unwrap();
        assert_ne!(
            sid,
            unsafe { libc::getsid(0) },
            "opener runs in a new session"
        );
        let (ok, msg) = open_theme_directory_with(&home.join("themes"), None);
        assert!(!ok);
        assert!(msg.contains("No desktop file opener"));
        let real = home.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = home.join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (ok, msg) = open_theme_directory_with(&link, Some(opener));
        assert!(!ok);
        assert!(msg.contains("not a regular directory"));
        let _ = std::fs::remove_dir_all(home);
    }
}
