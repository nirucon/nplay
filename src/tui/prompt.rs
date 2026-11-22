//! The one-line text prompt (1.x `UI.prompt`): Unicode input with cell-width
//! accounting (wide CJK characters, combining marks), secret input shown as
//! `•`, cursor movement, Backspace/Delete, and for the `:` command prompt Tab
//! completion (repeated Tab cycles) and Up/Down history.
//!
//! API: [`Prompt::new`], [`Prompt::key`] -> [`Outcome`], [`Prompt::shown`],
//! [`Prompt::input_x`], [`Prompt::cursor_x`], [`Prompt::hint`],
//! [`remember`] (history update on submit).

use crate::app::{cell_width, command_suggestions, printable_input, PromptDone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Prompt {
    pub label: String,
    pub buf: Vec<char>,
    pub pos: usize,
    pub secret: bool,
    /// Command prompt: completion and history.
    pub complete: bool,
    pub histpos: usize,
    pub matches: Vec<String>,
    pub match_i: usize,
    pub done: Option<PromptDone>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Continue,
    Cancel,
    Submit(String),
}

impl Prompt {
    pub fn new(
        label: &str,
        initial: &str,
        secret: bool,
        complete: bool,
        history_len: usize,
        done: Option<PromptDone>,
    ) -> Prompt {
        let buf: Vec<char> = if secret {
            Vec::new()
        } else {
            initial.chars().collect()
        };
        Prompt {
            label: label.to_string(),
            pos: buf.len(),
            buf,
            secret,
            complete,
            histpos: history_len,
            matches: Vec::new(),
            match_i: 0,
            done,
        }
    }

    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    /// Column where the input starts for a terminal `w` columns wide.
    pub fn input_x(&self, w: usize) -> usize {
        (2 + cell_width(&self.label)).min(w.saturating_sub(3))
    }

    /// What the input row shows.
    pub fn shown(&self) -> String {
        if self.secret {
            "•".repeat(self.buf.len())
        } else {
            self.text()
        }
    }

    /// Cursor column.
    pub fn cursor_x(&self, w: usize) -> usize {
        let before: String = if self.secret {
            "•".repeat(self.pos)
        } else {
            self.buf[..self.pos].iter().collect()
        };
        (self.input_x(w) + cell_width(&before)).min(w.saturating_sub(2))
    }

    /// The completion hint (`  [a · b · c · d]`), command prompt only.
    pub fn hint(&self) -> String {
        if !self.complete || self.secret {
            return String::new();
        }
        let ss = command_suggestions(&self.text());
        if ss.is_empty() {
            return String::new();
        }
        format!(
            "  [{}]",
            ss.iter()
                .take(4)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" · ")
        )
    }

    /// Handle one key for a terminal `w` columns wide.
    pub fn key(&mut self, ev: &KeyEvent, history: &[String], w: usize) -> Outcome {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        match ev.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('c') if ctrl => return Outcome::Cancel,
            KeyCode::Enter => return Outcome::Submit(self.text().trim().to_string()),
            KeyCode::Char('j') | KeyCode::Char('m') if ctrl => {
                return Outcome::Submit(self.text().trim().to_string())
            }
            KeyCode::Backspace => {
                if self.pos > 0 {
                    self.buf.remove(self.pos - 1);
                    self.pos -= 1;
                }
            }
            KeyCode::Char('h') if ctrl => {
                if self.pos > 0 {
                    self.buf.remove(self.pos - 1);
                    self.pos -= 1;
                }
            }
            KeyCode::Delete => {
                if self.pos < self.buf.len() {
                    self.buf.remove(self.pos);
                }
            }
            KeyCode::Left => self.pos = self.pos.saturating_sub(1),
            KeyCode::Right => self.pos = (self.pos + 1).min(self.buf.len()),
            KeyCode::Home => self.pos = 0,
            KeyCode::End => self.pos = self.buf.len(),
            KeyCode::Char('a') if ctrl => self.pos = 0,
            KeyCode::Char('e') if ctrl => self.pos = self.buf.len(),
            KeyCode::Char('u') if ctrl => {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            KeyCode::Up if self.complete && !history.is_empty() => {
                self.histpos = self.histpos.saturating_sub(1).min(history.len() - 1);
                self.buf = history[self.histpos].chars().collect();
                self.pos = self.buf.len();
            }
            KeyCode::Down if self.complete && !history.is_empty() => {
                self.histpos = (self.histpos + 1).min(history.len());
                self.buf = history
                    .get(self.histpos)
                    .map(|s| s.chars().collect())
                    .unwrap_or_default();
                self.pos = self.buf.len();
            }
            KeyCode::Tab if self.complete => {
                let current = self.text();
                let ss = command_suggestions(&current);
                if !ss.is_empty() {
                    if ss != self.matches {
                        self.matches = ss;
                    }
                    self.match_i = if self.matches.contains(&current) {
                        (self.match_i + 1) % self.matches.len()
                    } else {
                        0
                    };
                    self.buf = self.matches[self.match_i].chars().collect();
                    self.pos = self.buf.len();
                }
            }
            KeyCode::Char(c) if !ctrl && printable_input(c) => {
                let mut next = self.text();
                next.push(c);
                let room = w.saturating_sub(self.input_x(w) + 3).max(1);
                if cell_width(&next) <= room {
                    self.buf.insert(self.pos, c);
                    self.pos += 1;
                }
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Insert pasted text (bracketed paste), one printable char at a time.
    pub fn paste(&mut self, text: &str, w: usize) {
        for c in text.chars() {
            let ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            if printable_input(c) {
                self.key(&ev, &[], w);
            }
        }
    }
}

/// Command history after submitting `out` (deduplicated, last 50).
pub fn remember(history: &mut Vec<String>, out: &str) {
    if out.is_empty() {
        return;
    }
    history.retain(|x| x != out);
    history.push(out.to_string());
    let n = history.len();
    if n > 50 {
        history.drain(..n - 50);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(p: &mut Prompt, s: &str) {
        for c in s.chars() {
            p.key(&k(KeyCode::Char(c)), &[], 80);
        }
    }

    // tests/test_unicode_prompt.py
    #[test]
    fn swedish_and_widths() {
        for c in "åäöÅÄÖ".chars() {
            assert!(printable_input(c));
            assert_eq!(cell_width(&c.to_string()), 1);
        }
        for c in ['\x1b', '\t', '\r', '\n', '\x7f'] {
            assert!(!printable_input(c));
        }
        assert_eq!(cell_width("Örebro"), 6);
        assert_eq!(cell_width("漢字"), 4);
        assert_eq!(cell_width("e\u{301}"), 1);
    }

    #[test]
    fn unicode_editing_and_cursor() {
        let mut p = Prompt::new("FIND › ", "", false, false, 0, None);
        typed(&mut p, "Örebro 漢字");
        assert_eq!(p.text(), "Örebro 漢字");
        let x = p.input_x(80);
        assert_eq!(x, 2 + cell_width("FIND › "));
        assert_eq!(p.cursor_x(80), x + 11);
        p.key(&k(KeyCode::Left), &[], 80);
        assert_eq!(p.cursor_x(80), x + 9);
        p.key(&k(KeyCode::Backspace), &[], 80);
        assert_eq!(p.text(), "Örebro 字");
        p.key(&k(KeyCode::Delete), &[], 80);
        assert_eq!(p.text(), "Örebro ");
        assert_eq!(
            p.key(&k(KeyCode::Enter), &[], 80),
            Outcome::Submit("Örebro".into())
        );
        assert_eq!(p.key(&k(KeyCode::Esc), &[], 80), Outcome::Cancel);
    }

    #[test]
    fn width_limit_counts_cells() {
        let mut p = Prompt::new(":", "", false, false, 0, None);
        // w=12: x=3, room = 12-3-3 = 6 cells: three wide chars fit, not four.
        for _ in 0..5 {
            p.key(&k(KeyCode::Char('漢')), &[], 12);
        }
        assert_eq!(p.buf.len(), 3);
    }

    #[test]
    fn secret_input_is_masked() {
        let mut p = Prompt::new("PASSWORD › ", "ignored", true, false, 0, None);
        assert!(p.buf.is_empty());
        typed(&mut p, "hemlig");
        assert_eq!(p.shown(), "••••••");
        assert_eq!(p.hint(), "");
        assert_eq!(
            p.key(&k(KeyCode::Enter), &[], 80),
            Outcome::Submit("hemlig".into())
        );
    }

    #[test]
    fn tab_completion_cycles() {
        let mut p = Prompt::new(":", "", false, true, 0, None);
        typed(&mut p, "th");
        assert_eq!(p.hint(), "  [theme]");
        p.key(&k(KeyCode::Tab), &[], 80);
        assert_eq!(p.text(), "theme");
        // 1.x: a trailing space alone still completes the command name.
        typed(&mut p, " ");
        p.key(&k(KeyCode::Tab), &[], 80);
        assert_eq!(p.text(), "theme");
        typed(&mut p, " n");
        p.key(&k(KeyCode::Tab), &[], 80);
        assert_eq!(p.text(), "theme noir");
        p.key(&k(KeyCode::Tab), &[], 80);
        assert_eq!(p.text(), "theme satie");
        let mut q = Prompt::new(":", "", false, true, 0, None);
        typed(&mut q, "s");
        assert!(q.hint().contains("scan · settings · shuffle · sleep]"));
    }

    #[test]
    fn history_up_down() {
        let mut h = Vec::new();
        remember(&mut h, "queue");
        remember(&mut h, "stats");
        remember(&mut h, "queue");
        assert_eq!(h, vec!["stats".to_string(), "queue".to_string()]);
        let mut p = Prompt::new(":", "", false, true, h.len(), None);
        p.key(&k(KeyCode::Up), &h, 80);
        assert_eq!(p.text(), "queue");
        p.key(&k(KeyCode::Up), &h, 80);
        assert_eq!(p.text(), "stats");
        p.key(&k(KeyCode::Down), &h, 80);
        assert_eq!(p.text(), "queue");
        p.key(&k(KeyCode::Down), &h, 80);
        assert_eq!(p.text(), "");
        for i in 0..60 {
            remember(&mut h, &format!("c{i}"));
        }
        assert_eq!(h.len(), 50);
    }
}
