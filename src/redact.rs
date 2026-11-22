//! Credential redaction for URLs and free text (port of 1.x `redact.py`).
//!
//! A query that carries a credential is rewritten as its non-secret
//! parameters followed by `auth=REDACTED`: the secret values (`t`, `s`, `p`,
//! `token`, `apiKey`, `access_token`) and the Subsonic user name `u` are
//! dropped, `id`, `v`, `c` and the like stay for diagnosis.

use regex_lite::Regex;
use std::sync::OnceLock;

pub const SECRET_KEYS: &[&str] = &["t", "s", "p", "token", "apiKey", "access_token"];
pub const MASK: &str = "REDACTED";
pub const MARKER: &str = "auth=REDACTED";

fn is_secret(k: &str) -> bool {
    let l = k.to_lowercase();
    SECRET_KEYS.iter().any(|s| s.to_lowercase() == l)
}

fn is_drop(k: &str) -> bool {
    is_secret(k) || k.eq_ignore_ascii_case("u")
}

fn query_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\?([^\s"'<>#\]\)]*)"#).unwrap())
}

fn param_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<lead>[?&]|%3F|%26)(?P<key>t|s|p|token|apiKey|access_token)(?P<eq>=|%3D)(?P<val>[^&\s"'<>#,;)\]}]*)"#,
        )
        .unwrap()
    })
}

/// Python `urllib.parse.unquote_plus`.
pub fn unquote_plus(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|x| u8::from_str_radix(x, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Python `urllib.parse.quote_plus(s, safe)`.
pub fn quote_plus(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"_.-~".contains(&c) || safe.as_bytes().contains(&c) {
            out.push(c as char);
        } else if c == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Python `urllib.parse.quote(s, safe)` (spaces become %20).
pub fn quote(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"_.-~".contains(&c) || safe.as_bytes().contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Python `parse_qsl(q, keep_blank_values=True)`.
pub fn parse_qsl(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (unquote_plus(k), unquote_plus(v)),
            None => (unquote_plus(p), String::new()),
        })
        .collect()
}

/// Python `urlencode(pairs, safe=...)`.
pub fn urlencode(pairs: &[(String, String)], safe: &str) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k, safe), quote_plus(v, safe)))
        .collect::<Vec<_>>()
        .join("&")
}

fn clean_query(query: &str) -> Option<String> {
    let pairs = parse_qsl(query);
    if !pairs
        .iter()
        .any(|(k, v)| is_secret(k) && !v.is_empty() && v != MASK)
    {
        return None;
    }
    let kept: Vec<(String, String)> = pairs.into_iter().filter(|(k, _)| !is_drop(k)).collect();
    let head = urlencode(&kept, ":/");
    Some(if head.is_empty() {
        MARKER.to_string()
    } else {
        format!("{head}&{MARKER}")
    })
}

/// `text` with every credential-bearing query redacted.
pub fn redact_text(text: &str) -> String {
    let lower = text.to_lowercase();
    if !text.contains('=') && !lower.contains("%3") {
        return text.to_string();
    }
    let s = query_re().replace_all(text, |c: &regex_lite::Captures| match clean_query(&c[1]) {
        Some(clean) => format!("?{clean}"),
        None => c[0].to_string(),
    });
    param_re()
        .replace_all(&s, |c: &regex_lite::Captures| {
            let val = &c["val"];
            if val.is_empty() || val == MASK {
                c[0].to_string()
            } else {
                format!("{}{}{}{}", &c["lead"], &c["key"], &c["eq"], MASK)
            }
        })
        .into_owned()
}

/// `url` without credentials.
pub fn redact_url(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let (base, frag) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let Some((head, query)) = base.split_once('?') else {
        return redact_text(url);
    };
    match clean_query(query) {
        None => redact_text(url),
        Some(clean) => {
            let mut out = format!("{head}?{clean}");
            if let Some(f) = frag {
                if !f.is_empty() {
                    out.push('#');
                    out.push_str(f);
                }
            }
            out
        }
    }
}

/// True if `text` still contains an unredacted credential query value.
pub fn contains_secret(text: &str) -> bool {
    param_re()
        .captures_iter(text)
        .any(|c| !c["val"].is_empty() && &c["val"] != MASK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsonic_url() {
        let u = "https://n.example/rest/stream.view?u=me&t=abc&s=xyz&v=1.16.1&c=nplay&id=42";
        assert_eq!(
            redact_url(u),
            "https://n.example/rest/stream.view?v=1.16.1&c=nplay&id=42&auth=REDACTED"
        );
        let t = format!("mpv start source={u} ok");
        let r = redact_text(&t);
        assert!(!contains_secret(&r), "{r}");
        assert!(r.contains("id=42&auth=REDACTED ok"));
    }

    #[test]
    fn encoded_and_plain() {
        assert_eq!(redact_text("no secrets here"), "no secrets here");
        let e = "x%3Fu%3Dme%26t%3Dabc%26s%3Ddef";
        assert!(!contains_secret(&redact_text(e)));
        assert!(contains_secret("a?token=zz"));
        assert!(!contains_secret("a?token=REDACTED"));
    }
}
