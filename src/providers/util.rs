//! Helpers shared by the providers: HTTP (ureq + rustls), Python-compatible
//! text helpers (`html.unescape`, `unquote`, `casefold`, `capitalize`), local
//! time formatting, ISO-8601 parsing and a subprocess runner with a timeout.

use serde_json::{Map, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// User agent of the Navidrome/SR JSON requests (Python `_json`, `SR._request`).
pub fn user_agent() -> String {
    format!("NPLAY/{} (+terminal audio player)", crate::VERSION)
}

/// Maximum response body read from any provider endpoint.
pub const MAX_BODY: u64 = 32 * 1024 * 1024;

/// Transport-level failure of one HTTP request. Never contains the URL.
#[derive(Debug, Clone, PartialEq)]
pub enum HttpError {
    /// The server answered with an HTTP error status (>= 400).
    Status(u16),
    /// DNS, connect, TLS or read failure; the detail names the reason only.
    Unreachable(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Status(c) => write!(f, "HTTP Error {c}"),
            HttpError::Unreachable(d) => write!(f, "unreachable: {d}"),
        }
    }
}

/// `fetch(url, timeout_seconds) -> body bytes`. Injected in tests.
pub type Fetcher = Arc<dyn Fn(&str, u64) -> Result<Vec<u8>, HttpError> + Send + Sync>;

fn agent(timeout: u64, ua: &str) -> ureq::Agent {
    let d = Duration::from_secs(timeout.max(1));
    // Python's urlopen timeout applies per socket operation, not in total.
    ureq::AgentBuilder::new()
        .timeout_connect(d)
        .timeout_read(d)
        .timeout_write(d)
        .user_agent(ua)
        .build()
}

fn transport_detail(t: &ureq::Transport) -> String {
    use std::error::Error as _;
    let mut src = t.source();
    while let Some(e) = src {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                return "TimeoutError".into();
            }
        }
        src = e.source();
    }
    match t.message() {
        Some(m) if !m.is_empty() => crate::redact::redact_text(m),
        _ => t.kind().to_string(),
    }
}

/// GET `url` with the given headers; the body is capped at [`MAX_BODY`].
/// gzip responses are decoded by ureq.
pub fn http_get(
    url: &str,
    timeout: u64,
    ua: &str,
    headers: &[(&str, &str)],
) -> Result<Vec<u8>, HttpError> {
    let mut req = agent(timeout, ua).get(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.call() {
        Ok(resp) => {
            let mut buf = Vec::new();
            resp.into_reader()
                .take(MAX_BODY)
                .read_to_end(&mut buf)
                .map_err(|e| {
                    HttpError::Unreachable(if e.kind() == std::io::ErrorKind::TimedOut {
                        "TimeoutError".into()
                    } else {
                        e.kind().to_string()
                    })
                })?;
            Ok(buf)
        }
        Err(ureq::Error::Status(code, _)) => Err(HttpError::Status(code)),
        Err(ureq::Error::Transport(t)) => Err(HttpError::Unreachable(transport_detail(&t))),
    }
}

/// HTTP status of `url` (`None` when unreachable), without reading the body.
pub fn http_status(url: &str, timeout: u64, ua: &str, headers: &[(&str, &str)]) -> Option<u16> {
    let mut req = agent(timeout, ua).get(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.call() {
        Ok(resp) => Some(resp.status()),
        Err(ureq::Error::Status(code, _)) => Some(code),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------- JSON access

/// `d.get(k)` as an object (empty when missing or not an object).
pub fn obj<'a>(v: &'a Value, k: &str) -> &'a Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.get(k)
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// `d.get(k) or []` as a slice of values.
pub fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `str(d.get(k, ''))` (null -> "").
pub fn s(v: &Value, k: &str) -> String {
    v.get(k).map(crate::model::value_str).unwrap_or_default()
}

/// `d.get(k)` truthy?
pub fn truthy(v: &Value, k: &str) -> bool {
    v.get(k).map(crate::model::value_bool).unwrap_or(false)
}

/// `d.get(k) or d.get(k2) or ...` as a string ("" when all are falsy).
pub fn first_str(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if truthy(v, k) {
            return s(v, k);
        }
    }
    String::new()
}

/// `d.get(k, default)` as a raw JSON value.
pub fn raw(v: &Value, k: &str, default: Value) -> Value {
    v.get(k).cloned().unwrap_or(default)
}

// ---------------------------------------------------------------- text

/// Python `str.casefold()` (close enough: full lowercase).
pub fn casefold(s: &str) -> String {
    s.to_lowercase()
}

/// Python `str.capitalize()`.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// First `n` characters (Python `s[:n]`).
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python `urllib.parse.unquote` (percent-decoding, `+` kept).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|x| u8::from_str_radix(x, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path of an absolute or root-relative URL (`urlparse(u).path`).
pub fn url_path(u: &str) -> String {
    let rest = match u.find("://") {
        Some(i) => {
            let after = &u[i + 3..];
            match after.find('/') {
                Some(j) => &after[j..],
                None => "",
            }
        }
        None => u,
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Last path segment of a URL, percent-decoded (SR slugs).
pub fn url_slug(u: &str) -> String {
    let p = url_path(u);
    unquote(p.trim_end_matches('/').rsplit('/').next().unwrap_or(""))
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "ndash" => "\u{2013}",
        "mdash" => "\u{2014}",
        "hellip" => "\u{2026}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "deg" => "\u{b0}",
        "middot" => "\u{b7}",
        "bull" => "\u{2022}",
        "aring" => "å",
        "Aring" => "Å",
        "auml" => "ä",
        "Auml" => "Ä",
        "ouml" => "ö",
        "Ouml" => "Ö",
        "uuml" => "ü",
        "Uuml" => "Ü",
        "eacute" => "é",
        "Eacute" => "É",
        "egrave" => "è",
        "aacute" => "á",
        "oslash" => "ø",
        "Oslash" => "Ø",
        "aelig" => "æ",
        "AElig" => "Æ",
        "szlig" => "ß",
        "ccedil" => "ç",
        _ => return None,
    })
}

/// Python `html.unescape` for numeric references and the common named ones.
pub fn html_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let semi = rest[1..]
            .char_indices()
            .take(32)
            .find(|(_, c)| *c == ';' || *c == '&' || c.is_whitespace())
            .map(|(j, c)| (j + 1, c));
        if let Some((j, ';')) = semi {
            let ent = &rest[1..j];
            let rep = if let Some(num) = ent.strip_prefix('#') {
                let n = if let Some(h) = num.strip_prefix(['x', 'X']) {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    num.parse::<u32>().ok()
                };
                n.map(|n| {
                    char::from_u32(n)
                        .filter(|_| n != 0)
                        .unwrap_or('\u{fffd}')
                        .to_string()
                })
            } else {
                named_entity(ent).map(str::to_string)
            };
            if let Some(r) = rep {
                out.push_str(&r);
                rest = &rest[j + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

fn re(pat: &str) -> regex_lite::Regex {
    regex_lite::Regex::new(pat).expect("static regex")
}

/// `re.sub('<[^>]+>', repl, s)`.
pub fn strip_tags(s: &str, repl: &str) -> String {
    static R: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| re(r"<[^>]+>"))
        .replace_all(s, repl)
        .into_owned()
}

/// `re.sub(r'\s+', ' ', s)`.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !ws {
                out.push(' ');
            }
            ws = true;
        } else {
            out.push(c);
            ws = false;
        }
    }
    out
}

/// Python `\w` for one character.
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `re.search(r'(^|\W)' + re.escape(needle) + r'($|\W)', hay)` with Unicode `\W`.
pub fn word_match(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut start = 0;
    while let Some(i) = hay[start..].find(needle) {
        let at = start + i;
        let before_ok = hay[..at].chars().next_back().is_none_or(|c| !is_word(c));
        let end = at + needle.len();
        let after_ok = hay[end..].chars().next().is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            return true;
        }
        start = at + hay[at..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    false
}

/// Python `text[max(0, pos-before) : pos+after]` where `pos` is a byte offset
/// and the window is measured in characters.
pub fn char_window(text: &str, pos: usize, before: usize, after: usize) -> &str {
    let pos = (0..=pos.min(text.len()))
        .rev()
        .find(|&p| text.is_char_boundary(p))
        .unwrap_or(0);
    let start = text[..pos]
        .char_indices()
        .rev()
        .nth(before.saturating_sub(1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let start = if before == 0 { pos } else { start };
    let end = text[pos..]
        .char_indices()
        .nth(after)
        .map(|(i, _)| pos + i)
        .unwrap_or(text.len());
    &text[start..end]
}

// ---------------------------------------------------------------- time

fn local_tm(ts: f64) -> Option<libc::tm> {
    if !ts.is_finite() {
        return None;
    }
    let t = ts.floor() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::localtime_r(&t, &mut tm) };
    (!r.is_null()).then_some(tm)
}

/// `time.strftime('%Y-%m-%d', time.localtime(ts))`.
pub fn local_date(ts: f64) -> String {
    local_tm(ts)
        .map(|tm| {
            format!(
                "{:04}-{:02}-{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday
            )
        })
        .unwrap_or_default()
}

/// `datetime.fromtimestamp(ts).strftime('%H:%M')`.
pub fn local_hm(ts: f64) -> String {
    local_tm(ts)
        .map(|tm| format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
        .unwrap_or_default()
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `datetime.fromisoformat(text).timestamp()`: naive values are local time.
pub fn iso_timestamp(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.len() < 10 || !t.is_char_boundary(10) {
        return None;
    }
    let (date, rest) = t.split_at(10);
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 || dp[0].len() != 4 || dp[1].len() != 2 || dp[2].len() != 2 {
        return None;
    }
    let (y, mo, d) = (num(dp[0])?, num(dp[1])?, num(dp[2])?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (mut h, mut mi, mut sec, mut frac) = (0i64, 0i64, 0i64, 0f64);
    let mut offset: Option<i64> = None;
    if !rest.is_empty() {
        let rest = rest.strip_prefix(['T', ' '])?;
        let tz_at = rest.find(['+', '-']);
        let (time, tz) = match tz_at {
            Some(i) => (&rest[..i], Some(&rest[i..])),
            None => (rest, None),
        };
        let (hms, fr) = match time.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (time, None),
        };
        let tp: Vec<&str> = hms.split(':').collect();
        if tp.is_empty() || tp.len() > 3 || tp.iter().any(|x| x.len() != 2) {
            return None;
        }
        h = num(tp[0])?;
        mi = tp.get(1).map(|x| num(x)).unwrap_or(Some(0))?;
        sec = tp.get(2).map(|x| num(x)).unwrap_or(Some(0))?;
        if let Some(f) = fr {
            num(f)?;
            frac = format!("0.{f}").parse().ok()?;
        }
        if h > 23 || mi > 59 || sec > 59 {
            return None;
        }
        if let Some(tz) = tz {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let body = tz[1..].replace(':', "");
            if body.len() < 4 {
                return None;
            }
            let oh = num(&body[..2])?;
            let om = num(&body[2..4])?;
            let os = if body.len() >= 6 {
                num(&body[4..6])?
            } else {
                0
            };
            offset = Some(sign * (oh * 3600 + om * 60 + os));
        }
    }
    match offset {
        Some(off) => {
            let days = days_from_civil(y, mo, d);
            Some((days * 86400 + h * 3600 + mi * 60 + sec - off) as f64 + frac)
        }
        None => {
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            tm.tm_year = (y - 1900) as i32;
            tm.tm_mon = (mo - 1) as i32;
            tm.tm_mday = d as i32;
            tm.tm_hour = h as i32;
            tm.tm_min = mi as i32;
            tm.tm_sec = sec as i32;
            tm.tm_isdst = -1;
            let r = unsafe { libc::mktime(&mut tm) };
            (r != -1).then_some(r as f64 + frac)
        }
    }
}

// ---------------------------------------------------------------- subprocess

/// Result of [`run_capture`].
#[derive(Debug, Clone)]
pub struct Captured {
    /// Exit code (`None` when killed by a signal).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

const MAX_CAPTURE: u64 = 64 * 1024 * 1024;

/// Run `prog args...` with stdin closed and stdout/stderr captured, killing it
/// after `timeout` (then `ErrorKind::TimedOut`). No shell is involved.
pub fn run_capture(prog: &str, args: &[String], timeout: Duration) -> std::io::Result<Captured> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id() as libc::pid_t;
    let reader = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = p {
                let _ = (&mut p).take(MAX_CAPTURE).read_to_end(&mut buf);
                let _ = std::io::copy(&mut p, &mut std::io::sink());
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = reader(
        child
            .stdout
            .take()
            .map(|x| Box::new(x) as Box<dyn Read + Send>),
    );
    let err = reader(
        child
            .stderr
            .take()
            .map(|x| Box::new(x) as Box<dyn Read + Send>),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let r = child.wait();
        let _ = tx.send(());
        r
    });
    let timed_out = rx.recv_timeout(timeout).is_err();
    if timed_out {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let status = waiter
        .join()
        .map_err(|_| std::io::Error::other("wait failed"))??;
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if timed_out {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("timed out after {} seconds", timeout.as_secs()),
        ));
    }
    Ok(Captured {
        code: status.code(),
        stdout,
        stderr,
    })
}

/// Six random bytes as 12 hex digits (Python `secrets.token_hex(6)`).
pub fn token_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        for b in buf.iter_mut() {
            *b = fastrand::u8(..);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_and_unquote() {
        assert_eq!(
            html_unescape("a &amp; b &#229; &#xE5; &aring; &bogus; &"),
            "a & b å å å &bogus; &"
        );
        assert_eq!(unquote("p%C3%A5-x%2"), "på-x%2");
        assert_eq!(
            url_slug("https://www.sverigesradio.se/avsnitt/abc-d%C3%A5/"),
            "abc-då"
        );
        assert_eq!(url_path("/avsnitt/x?y=1"), "/avsnitt/x");
        assert_eq!(capitalize("hej DÅ"), "Hej då");
    }

    #[test]
    fn words_and_windows() {
        assert!(word_match("p3 dokumentär", "dokumentär"));
        assert!(!word_match("ödokumentär", "dokumentär"));
        assert!(word_match("x-ray, y", "ray"));
        assert_eq!(char_window("åäöabc", 6, 2, 2), "äöab");
        assert_eq!(char_window("abc", 1, 500, 900), "abc");
    }

    #[test]
    fn iso_and_dates() {
        assert_eq!(
            iso_timestamp("2024-01-02T03:04:05+00:00"),
            Some(1704164645.0)
        );
        assert_eq!(
            iso_timestamp("2024-01-02T03:04:05.5+01:00"),
            Some(1704164645.5 - 3600.0)
        );
        assert!(iso_timestamp("2024-01-02").is_some());
        assert!(iso_timestamp("garbage").is_none());
        let ts = iso_timestamp("2024-05-06T12:30:00").unwrap();
        assert_eq!(local_date(ts), "2024-05-06");
        assert_eq!(local_hm(ts), "12:30");
    }

    #[test]
    fn subprocess_timeout() {
        let r = run_capture(
            "sh",
            &["-c".into(), "echo hi; echo e >&2; exit 3".into()],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            (r.code, r.stdout.as_str(), r.stderr.as_str()),
            (Some(3), "hi\n", "e\n")
        );
        let e = run_capture("sleep", &["5".into()], Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(token_hex(6).len(), 12);
    }
}
