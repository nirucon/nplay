//! Bounded on-disk artwork cache (port of `nplay/artwork_cache.py`).
//!
//! The cache lives in `$XDG_DATA_HOME/nplay/artwork/<sha256>.art`, exactly as
//! in 1.x, so existing caches are reused. Navidrome `getCoverArt.view` URLs are
//! keyed by `scheme://host/path?id=<id>` only, so rotating Subsonic tokens
//! never duplicate a cover (and never end up in a file name).
//!
//! Public API:
//! - `ArtworkCache::new(max_mb)`: cache in the default directory; `max_mb` is
//!   the `artwork_cache_mb` setting (0 means the 500 MB default, minimum 32).
//! - `ArtworkCache::with_root(root, max_mb)`: same, explicit directory (tests).
//! - `key(url) -> String`: the sha256 hex file stem for a URL.
//! - `path(url) -> Option<PathBuf>`: cache file for a URL (None for "").
//! - `cached(url) -> Option<PathBuf>`: a local file path that exists, or a
//!   non-empty cached download (its atime/mtime are touched for LRU).
//! - `fetch(url, timeout_s, user_agent) -> Option<PathBuf>`: cached or download
//!   (http/https only, `Accept: image/*`, at most 12 MiB), then prune.
//!   `fetch_default(url)` uses 8 s and `NPLAY/<version>`.
//! - `stats() -> (files, bytes)`, `prune() -> removed`, `clear() -> removed`.
//!
//! The handle is a cheap `Clone + Send + Sync` value (no shared state).

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Largest image accepted from the network.
pub const MAX_DOWNLOAD: u64 = 12 * 1024 * 1024;

/// `NPLAY/<version>`, the user agent of every artwork request.
pub fn user_agent() -> String {
    format!("NPLAY/{}", crate::VERSION)
}

#[derive(Clone, Debug)]
pub struct ArtworkCache {
    pub root: PathBuf,
    pub max_bytes: u64,
}

fn is_http(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// `Path(s).expanduser()`.
fn expanduser(s: &str) -> PathBuf {
    if s == "~" {
        return crate::paths::home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return crate::paths::home_dir().join(rest);
    }
    PathBuf::from(s)
}

/// Minimal `urllib.parse.urlparse`: (scheme, netloc, path, query).
fn urlparse(url: &str) -> (String, String, String, String) {
    let no_frag = url.split('#').next().unwrap_or("");
    let (scheme, rest) = match no_frag.find(':') {
        Some(i)
            if i > 0
                && no_frag[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            (no_frag[..i].to_ascii_lowercase(), &no_frag[i + 1..])
        }
        _ => (String::new(), no_frag),
    };
    let (netloc, rest) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?']).unwrap_or(r.len());
            (r[..end].to_string(), &r[end..])
        }
        None => (String::new(), rest),
    };
    let (mut path, query) = match rest.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (rest.to_string(), String::new()),
    };
    // urlparse moves `;params` of the last path segment out of the path.
    if let Some(slash) = path.rfind('/').map(|i| i + 1).or(Some(0)) {
        if let Some(semi) = path[slash..].find(';') {
            path.truncate(slash + semi);
        }
    }
    (scheme, netloc, path, query)
}

impl ArtworkCache {
    pub fn new(max_mb: i64) -> ArtworkCache {
        ArtworkCache::with_root(crate::paths::data().join("artwork"), max_mb)
    }

    pub fn with_root(root: PathBuf, max_mb: i64) -> ArtworkCache {
        let _ = std::fs::create_dir_all(&root);
        let mb = if max_mb == 0 { 500 } else { max_mb };
        ArtworkCache {
            root,
            max_bytes: mb.max(32) as u64 * 1024 * 1024,
        }
    }

    pub fn key(&self, url: &str) -> String {
        let mut key = url.to_string();
        let (scheme, netloc, path, query) = urlparse(url);
        if path.ends_with("/rest/getCoverArt.view") {
            let id = crate::redact::parse_qsl(&query)
                .into_iter()
                .find(|(k, v)| k == "id" && !v.is_empty())
                .map(|(_, v)| v);
            if let Some(id) = id {
                key = format!("{scheme}://{netloc}{path}?id={id}");
            }
        }
        let digest = Sha256::digest(key.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn path(&self, url: &str) -> Option<PathBuf> {
        if url.is_empty() {
            return None;
        }
        Some(self.root.join(format!("{}.art", self.key(url))))
    }

    pub fn cached(&self, url: &str) -> Option<PathBuf> {
        if url.is_empty() {
            return None;
        }
        if !is_http(url) {
            let p = expanduser(url);
            return p.is_file().then_some(p);
        }
        let p = self.path(url)?;
        match std::fs::metadata(&p) {
            Ok(m) if m.is_file() && m.len() > 0 => {
                touch(&p);
                Some(p)
            }
            _ => None,
        }
    }

    pub fn fetch_default(&self, url: &str) -> Option<PathBuf> {
        self.fetch(url, 8.0, &user_agent())
    }

    pub fn fetch(&self, url: &str, timeout: f64, user_agent: &str) -> Option<PathBuf> {
        if let Some(hit) = self.cached(url) {
            return Some(hit);
        }
        if !is_http(url) {
            return None;
        }
        let p = self.path(url)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs_f64(timeout.max(0.1)))
            .user_agent(user_agent)
            .build();
        let resp = agent.get(url).set("Accept", "image/*").call().ok()?;
        let mut data = Vec::new();
        resp.into_reader()
            .take(MAX_DOWNLOAD + 1)
            .read_to_end(&mut data)
            .ok()?;
        if data.is_empty() || data.len() as u64 > MAX_DOWNLOAD {
            return None;
        }
        let _ = std::fs::create_dir_all(&self.root);
        let tmp = p.with_extension("tmp");
        if write_file(&tmp, &data).is_err() || std::fs::rename(&tmp, &p).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        self.prune();
        Some(p)
    }

    fn files(&self) -> Vec<(f64, u64, PathBuf)> {
        let mut out = Vec::new();
        walk(&self.root, &mut |p, m| {
            if !p.to_string_lossy().ends_with(".tmp") {
                let t = m
                    .accessed()
                    .or_else(|_| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                out.push((t, m.len(), p.to_path_buf()));
            }
        });
        out
    }

    pub fn stats(&self) -> (usize, u64) {
        let f = self.files();
        (f.len(), f.iter().map(|x| x.1).sum())
    }

    /// Remove least recently used files until the cache is at 90 % of its cap
    /// (only when it is over the cap). Returns the number removed.
    pub fn prune(&self) -> usize {
        let mut files = self.files();
        let mut total: u64 = files.iter().map(|x| x.1).sum();
        if total <= self.max_bytes {
            return 0;
        }
        files.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        let floor = (self.max_bytes as f64 * 0.9) as u64;
        let mut removed = 0;
        for (_, size, p) in files {
            if total <= floor {
                break;
            }
            if std::fs::remove_file(&p).is_ok() {
                total = total.saturating_sub(size);
                removed += 1;
            }
        }
        removed
    }

    pub fn clear(&self) -> usize {
        let mut n = 0;
        walk(&self.root, &mut |p, _| {
            if std::fs::remove_file(p).is_ok() {
                n += 1;
            }
        });
        n
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path, &std::fs::Metadata)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(m) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk(&p, f);
        } else if m.is_file() {
            f(&p, &m);
        }
    }
}

fn touch(p: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().append(true).open(p) {
        let now = std::time::SystemTime::now();
        let _ = f.set_times(
            std::fs::FileTimes::new()
                .set_accessed(now)
                .set_modified(now),
        );
    }
}

fn write_file(p: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)?;
    f.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "nplay-artcache-{name}-{}-{:x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn key_matches_python_and_ignores_subsonic_tokens() {
        let c = ArtworkCache::with_root(tmpdir("key"), 500);
        let a = c.key("https://nav.example/rest/getCoverArt.view?u=me&t=aaa&s=1&id=al-1&size=600");
        let b = c.key("https://nav.example/rest/getCoverArt.view?id=al-1&u=me&t=bbb&s=2");
        assert_eq!(a, b);
        let direct = c.key("https://nav.example/rest/getCoverArt.view?id=al-1");
        assert_eq!(a, direct);
        assert_ne!(a, c.key("https://nav.example/rest/getCoverArt.view?id=al-2"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn known_digest() {
        let c = ArtworkCache::with_root(tmpdir("dig"), 500);
        // Values computed with the Python 1.x ArtworkCache.key().
        assert_eq!(
            c.key("https://nav.example:4533/rest/getCoverArt.view?u=me&t=aaa&s=1&id=al%2D1+x&size=600"),
            "b48355e67c46ec97655f734754eb2a772c3f5e90ece878312c650a9bf5418f9c"
        );
        assert_eq!(
            c.key("https://example.com/a.jpg"),
            "276a1ac00ba4f0ea47eeeafca24284f41bc78dc593af1f048615aceba44ab9d9"
        );
        assert_eq!(
            c.key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cached_local_files_and_downloads() {
        let root = tmpdir("cached");
        let c = ArtworkCache::with_root(root.join("art"), 500);
        let local = root.join("cover.jpg");
        std::fs::write(&local, b"x").unwrap();
        assert_eq!(c.cached(local.to_str().unwrap()), Some(local.clone()));
        assert_eq!(c.cached(root.join("missing.jpg").to_str().unwrap()), None);
        assert_eq!(c.cached(""), None);
        let url = "https://example.invalid/c.jpg";
        assert_eq!(c.cached(url), None);
        let p = c.path(url).unwrap();
        assert!(p.to_string_lossy().ends_with(".art"));
        std::fs::write(&p, b"").unwrap();
        assert_eq!(c.cached(url), None, "empty files are not hits");
        std::fs::write(&p, b"img").unwrap();
        assert_eq!(c.cached(url), Some(p));
        assert_eq!(c.fetch("ftp://x/y", 1.0, "t"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prune_removes_oldest_to_ninety_percent() {
        let root = tmpdir("prune");
        let mut c = ArtworkCache::with_root(root.clone(), 32);
        c.max_bytes = 1000;
        for i in 0..6 {
            let p = root.join(format!("{i}.art"));
            std::fs::write(&p, vec![0u8; 300]).unwrap();
            let t = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000 + i * 100);
            let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
                .unwrap();
        }
        std::fs::write(root.join("x.tmp"), vec![0u8; 5000]).unwrap();
        assert_eq!(c.stats(), (6, 1800));
        assert_eq!(c.prune(), 3);
        assert!(!root.join("0.art").exists());
        assert!(!root.join("2.art").exists());
        assert!(root.join("3.art").exists());
        assert_eq!(c.prune(), 0);
        assert_eq!(c.clear(), 4);
        assert_eq!(c.stats(), (0, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn minimum_size_cap() {
        let c = ArtworkCache::with_root(tmpdir("cap"), 1);
        assert_eq!(c.max_bytes, 32 * 1024 * 1024);
        let c = ArtworkCache::with_root(tmpdir("cap2"), 0);
        assert_eq!(c.max_bytes, 500 * 1024 * 1024);
    }

    #[test]
    fn fetch_over_http_with_user_agent() {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut ua = String::new();
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line.to_lowercase().starts_with("user-agent:") {
                    ua = line.trim().to_string();
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let mut s = s;
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nPNG!")
                .unwrap();
            ua
        });
        let root = tmpdir("fetch");
        let c = ArtworkCache::with_root(root.clone(), 500);
        let url = format!("http://127.0.0.1:{port}/rest/getCoverArt.view?id=7&t=secret");
        let p = c.fetch(&url, 5.0, &user_agent()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"PNG!");
        assert!(!p.to_string_lossy().contains("secret"));
        assert_eq!(h.join().unwrap(), format!("User-Agent: NPLAY/{}", crate::VERSION));
        // Second call is a cache hit (no server any more).
        assert_eq!(c.fetch(&url, 1.0, "x"), Some(p));
        let _ = std::fs::remove_dir_all(root);
    }
}
