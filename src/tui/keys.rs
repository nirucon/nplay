//! Key bindings: crossterm key events to NPLAY actions.
//!
//! The configurable actions come from `config.ini [keys]` (defaults in
//! `crate::config::DEFAULT_KEYS`, read through `Config::keys()`); the other
//! 1.x keys (`z`, `i`, `t`, `Ctrl+P`, `S`, `s`, `x`, `.`, ...) are fixed. A
//! configured key wins over a fixed one, and rebinding an action removes its
//! default key. Fixed aliases (arrow keys for up/down, `=` for volume up,
//! `Home`/`End`, `Ctrl+C` quit) always stay.
//!
//! API: [`spec`] (event to a key name such as `SPACE`, `CTRL+N`, `a`),
//! [`normalize`] (config spelling to the same name), [`Keymap::new`] /
//! [`Keymap::from_config`], [`Keymap::action`], [`Keymap::key_for`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// Every action the main loop handles (configurable ones use their
/// `[keys]` names).
pub const FIXED: &[(&str, &str)] = &[
    ("CTRL+C", "quit"),
    ("CTRL+N", "now"),
    ("CTRL+P", "quick_find"),
    ("TAB", "tab"),
    ("DOWN", "down"),
    ("UP", "up"),
    ("HOME", "home"),
    ("END", "last"),
    ("G", "last"),
    ("PGDN", "page_down"),
    ("PGUP", "page_up"),
    ("z", "stop"),
    ("C", "visual_style"),
    ("=", "volume_up"),
    ("s", "shuffle"),
    ("S", "sort"),
    ("x", "repeat"),
    ("a", "queue_add"),
    ("A", "playlist_add"),
    ("c", "queue_clear"),
    ("W", "queue_save"),
    ("r", "random"),
    ("e", "play_next"),
    ("d", "remove"),
    ("J", "move_down"),
    ("K", "move_up"),
    ("R", "rename"),
    ("D", "delete"),
    ("V", "toggle_visualizer"),
    (".", "actions"),
    ("t", "theme"),
    ("i", "info"),
];

/// A key event as a key name; None for releases and keys NPLAY ignores.
pub fn spec(ev: &KeyEvent) -> Option<String> {
    use crossterm::event::KeyEventKind;
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let s = match ev.code {
        KeyCode::Char(' ') => "SPACE".to_string(),
        KeyCode::Char(c) if ctrl => format!("CTRL+{}", c.to_ascii_uppercase()),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "ENTER".into(),
        KeyCode::Esc => "ESC".into(),
        KeyCode::Tab => "TAB".into(),
        KeyCode::BackTab => "BACKTAB".into(),
        KeyCode::Backspace => "BACKSPACE".into(),
        KeyCode::Delete => "DELETE".into(),
        KeyCode::Insert => "INSERT".into(),
        KeyCode::Left => "LEFT".into(),
        KeyCode::Right => "RIGHT".into(),
        KeyCode::Up => "UP".into(),
        KeyCode::Down => "DOWN".into(),
        KeyCode::Home => "HOME".into(),
        KeyCode::End => "END".into(),
        KeyCode::PageUp => "PGUP".into(),
        KeyCode::PageDown => "PGDN".into(),
        KeyCode::F(n) => format!("F{n}"),
        _ => return None,
    };
    Some(s)
}

/// A `[keys]` value as a key name (`space`, `KEY_LEFT`, `^N`, `Ctrl-n`,
/// `PageDown`, ... are accepted). Single characters keep their case.
pub fn normalize(v: &str) -> String {
    if v == " " {
        return "SPACE".into();
    }
    let v = v.trim();
    if v.chars().count() == 1 {
        return v.to_string();
    }
    let up = v.to_uppercase();
    let up = up.strip_prefix("KEY_").unwrap_or(&up).to_string();
    let ctrl = ["CTRL+", "CTRL-", "C-", "^"]
        .iter()
        .find_map(|p| up.strip_prefix(p).map(str::to_string));
    if let Some(rest) = ctrl {
        if rest.chars().count() == 1 {
            return format!("CTRL+{rest}");
        }
    }
    match up.as_str() {
        "SPC" => "SPACE",
        "RETURN" | "ENT" => "ENTER",
        "ESCAPE" => "ESC",
        "PAGEUP" | "PPAGE" | "PRIOR" => "PGUP",
        "PAGEDOWN" | "NPAGE" | "NEXT" => "PGDN",
        "DC" | "DEL" => "DELETE",
        "BS" => "BACKSPACE",
        "ARROWLEFT" => "LEFT",
        "ARROWRIGHT" => "RIGHT",
        "ARROWUP" => "UP",
        "ARROWDOWN" => "DOWN",
        other => other,
    }
    .to_string()
}

/// Key name to action.
#[derive(Clone, Debug)]
pub struct Keymap {
    map: HashMap<String, String>,
    configured: Vec<(String, String)>,
}

impl Keymap {
    /// Fixed keys plus the configurable `(action, key)` pairs.
    pub fn new(configured: &[(String, String)]) -> Keymap {
        let mut map: HashMap<String, String> = FIXED
            .iter()
            .map(|(k, a)| (k.to_string(), a.to_string()))
            .collect();
        let mut conf = Vec::new();
        for (action, key) in configured {
            let k = normalize(key);
            if k.is_empty() {
                continue;
            }
            map.insert(k.clone(), action.clone());
            conf.push((action.clone(), k));
        }
        // 1.x aliases that stay whatever [keys] says (unless rebound above).
        for (k, a) in [("DOWN", "down"), ("UP", "up")] {
            map.entry(k.into()).or_insert_with(|| a.into());
        }
        Keymap {
            map,
            configured: conf,
        }
    }

    pub fn from_config(cfg: &crate::config::Config) -> Keymap {
        Keymap::new(&cfg.keys())
    }

    /// The action for a key name.
    pub fn action(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// The configured key of a configurable action.
    pub fn key_for(&self, action: &str) -> Option<&str> {
        self.configured
            .iter()
            .rev()
            .find(|(a, _)| a == action)
            .map(|(_, k)| k.as_str())
    }

    /// `(action, key)` pairs that differ from `DEFAULT_KEYS`.
    pub fn custom(&self) -> Vec<(String, String)> {
        self.configured
            .iter()
            .filter(|(a, k)| {
                !crate::config::DEFAULT_KEYS
                    .iter()
                    .any(|(da, dk)| da == a && normalize(dk) == *k)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: m,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn defaults() -> Vec<(String, String)> {
        crate::config::DEFAULT_KEYS
            .iter()
            .map(|(a, k)| (a.to_string(), k.to_string()))
            .collect()
    }

    #[test]
    fn specs() {
        let n = KeyModifiers::NONE;
        assert_eq!(spec(&ev(KeyCode::Char(' '), n)).unwrap(), "SPACE");
        assert_eq!(
            spec(&ev(KeyCode::Char('n'), KeyModifiers::CONTROL)).unwrap(),
            "CTRL+N"
        );
        assert_eq!(
            spec(&ev(KeyCode::Char('Q'), KeyModifiers::SHIFT)).unwrap(),
            "Q"
        );
        assert_eq!(spec(&ev(KeyCode::PageDown, n)).unwrap(), "PGDN");
        let mut r = ev(KeyCode::Char('a'), n);
        r.kind = KeyEventKind::Release;
        assert!(spec(&r).is_none());
    }

    #[test]
    fn normalize_names() {
        assert_eq!(normalize("space"), "SPACE");
        assert_eq!(normalize("KEY_LEFT"), "LEFT");
        assert_eq!(normalize("^n"), "CTRL+N");
        assert_eq!(normalize("Ctrl-p"), "CTRL+P");
        assert_eq!(normalize("PageDown"), "PGDN");
        assert_eq!(normalize("Q"), "Q");
        assert_eq!(normalize("q"), "q");
    }

    #[test]
    fn default_map_matches_1x() {
        let k = Keymap::new(&defaults());
        for (key, action) in [
            ("SPACE", "play_pause"),
            ("n", "next"),
            ("p", "previous"),
            ("LEFT", "seek_back"),
            ("H", "seek_back_long"),
            ("+", "volume_up"),
            ("=", "volume_up"),
            ("/", "search"),
            ("ESC", "back"),
            ("Q", "quit"),
            ("CTRL+C", "quit"),
            ("j", "down"),
            ("DOWN", "down"),
            ("g", "home"),
            ("G", "last"),
            ("u", "refresh"),
            ("v", "visualizer"),
            ("V", "toggle_visualizer"),
            ("z", "stop"),
            ("CTRL+P", "quick_find"),
            (":", "command"),
            ("?", "help"),
            ("ENTER", "activate"),
        ] {
            assert_eq!(k.action(key), Some(action), "{key}");
        }
        assert!(k.custom().is_empty());
    }

    #[test]
    fn rebinding_replaces_default_and_beats_fixed() {
        let mut c = defaults();
        for e in c.iter_mut() {
            if e.0 == "next" {
                e.1 = "N".into();
            }
            if e.0 == "quit" {
                e.1 = "z".into();
            }
        }
        let k = Keymap::new(&c);
        assert_eq!(k.action("N"), Some("next"));
        assert_eq!(k.action("n"), None);
        assert_eq!(k.action("z"), Some("quit"));
        assert_eq!(k.action("Q"), None);
        assert_eq!(k.key_for("next"), Some("N"));
        assert_eq!(k.custom().len(), 2);
    }
}
