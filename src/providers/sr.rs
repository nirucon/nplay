//! Sveriges Radio: Open API v2 (`https://api.sr.se/api/v2`) plus best-effort
//! discovery from the public search page `https://www.sverigesradio.se/sok`.
//! See the parent module doc for the API.

use super::sr_date;
use super::util::{self, arr, obj, s, truthy, Fetcher, HttpError};
use crate::core::{Error, ErrorKind, Result};
use crate::model::{Meta, Track};
use crate::redact::urlencode;
use regex_lite::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

pub const API: &str = "https://api.sr.se/api/v2";
pub const WEB: &str = "https://www.sverigesradio.se";

/// Live channels: (name, channel id).
pub const ST: &[(&str, u32)] = &[("P1", 132), ("P2", 163), ("P3", 164), ("P4 Väst", 212)];

/// Programme catalogue cache lifetime (6 hours).
const CATALOGUE_TTL: f64 = 21600.0;

/// What is on a live channel now (`/scheduledepisodes/rightnow` and
/// `/playlists/rightnow`); times are local `HH:MM`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LiveInfo {
    pub program: String,
    pub episode: String,
    pub song: String,
    pub next_program: String,
    pub start: String,
    pub end: String,
    pub next_start: String,
    pub next_end: String,
}

#[derive(Clone)]
pub struct SR {
    pub cache: PathBuf,
    last_cache: Arc<AtomicBool>,
    fetch: Fetcher,
}

fn default_fetcher() -> Fetcher {
    Arc::new(|url: &str, timeout: u64| {
        util::http_get(
            url,
            timeout,
            &util::user_agent(),
            &[("Accept", "application/json,text/html;q=0.9,*/*;q=0.8")],
        )
    })
}

fn net(e: HttpError) -> Error {
    Error::new(ErrorKind::Network, format!("Sveriges Radio {e}"))
}

fn rx(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn cover_512(c: &str) -> String {
    c.replace("{format}", "512x512")
        .replace("{width}", "512")
        .replace("{height}", "512")
}

fn sr_track(id: String, title: String, kind: &str, seekable: bool) -> Track {
    Track {
        source: "sr".into(),
        id,
        title,
        artist: "Sveriges Radio".into(),
        kind: kind.into(),
        seekable,
        ..Default::default()
    }
}

fn decode_web(raw: &str) -> String {
    util::html_unescape(raw)
        .replace("\\/", "/")
        .replace("\\u002F", "/")
        .replace("\\u002f", "/")
}

fn absolute(href: &str) -> String {
    if href.starts_with("http") {
        href.to_string()
    } else {
        format!("{WEB}{href}")
    }
}

fn pairs(xs: &[(&str, String)]) -> Vec<(String, String)> {
    xs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

impl Default for SR {
    fn default() -> Self {
        SR::new()
    }
}

impl SR {
    pub fn new() -> SR {
        SR::with(crate::paths::data().join("sr"), default_fetcher())
    }

    /// Explicit cache directory and transport (tests).
    pub fn with(cache: PathBuf, fetch: Fetcher) -> SR {
        let _ = std::fs::create_dir_all(&cache);
        SR {
            cache,
            last_cache: Arc::new(AtomicBool::new(false)),
            fetch,
        }
    }

    /// True when the last catalogue load fell back to a stale cache.
    pub fn last_cache(&self) -> bool {
        self.last_cache.load(Ordering::Relaxed)
    }

    fn request(&self, url: &str, timeout: u64) -> Result<String> {
        let raw = (self.fetch)(url, timeout).map_err(net)?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    /// GET an Open API path with `format=json&size=100` plus `params`.
    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let mut q = vec![
            ("format".to_string(), "json".to_string()),
            ("size".into(), "100".into()),
        ];
        for (k, v) in params {
            match q.iter_mut().find(|(qk, _)| qk == k) {
                Some(slot) => slot.1 = v.clone(),
                None => q.push((k.to_string(), v.clone())),
            }
        }
        let text = self.request(&format!("{API}{path}?{}", urlencode(&q, "")), 8)?;
        serde_json::from_str(&text).map_err(|e| {
            Error::new(
                ErrorKind::Provider,
                format!("Sveriges Radio returned invalid JSON: {e}"),
            )
        })
    }

    /// The live channels as radio rows (`sr:<channel id>`).
    pub fn live(&self) -> Vec<Track> {
        ST.iter()
            .map(|(name, i)| Track {
                url: format!("https://sverigesradio.se/topsy/direkt/{i}-hi-mp3.pls"),
                ..sr_track(format!("sr:{i}"), name.to_string(), "radio", false)
            })
            .collect()
    }

    fn web_row(slug: &str, title: String, href: String) -> Track {
        let mut t = sr_track(format!("web:{slug}"), title, "podcast", true);
        t.set_meta("webpage", href);
        t.set_meta("needs_resolve", true);
        t
    }

    /// Episode links scraped from the public search page.
    pub fn web_search(&self, q: &str, limit: usize) -> Result<Vec<Track>> {
        let raw = self.request(
            &format!(
                "{WEB}/sok?{}",
                urlencode(&pairs(&[("query", q.to_string())]), "")
            ),
            6,
        )?;
        let decoded = decode_web(&raw);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        static A: OnceLock<Regex> = OnceLock::new();
        let a = rx(
            &A,
            r##"(?is)<a\b[^>]*href=['"](?P<href>(?:https://www\.sverigesradio\.se)?/avsnitt/[^'"#?]+)['"][^>]*>(?P<body>.*?)</a>"##,
        );
        for m in a.captures_iter(&decoded) {
            let href = absolute(&m["href"]);
            if seen.contains(&href) {
                continue;
            }
            let title = util::html_unescape(&util::collapse_ws(&util::strip_tags(&m["body"], " ")))
                .trim()
                .to_string();
            if title.chars().count() < 3 {
                continue;
            }
            seen.insert(href.clone());
            let slug = util::url_slug(&href);
            out.push(SR::web_row(&slug, title, href));
            if out.len() >= limit {
                return Ok(out);
            }
        }
        if out.len() < limit {
            static B: OnceLock<Regex> = OnceLock::new();
            let b = rx(
                &B,
                r"(?i)(?:https://www\.sverigesradio\.se)?/avsnitt/[a-zA-Z0-9_%\-]+",
            );
            static SEP: OnceLock<Regex> = OnceLock::new();
            let sep = rx(&SEP, r"[-_]+");
            for m in b.find_iter(&decoded) {
                let href = absolute(m.as_str());
                if seen.contains(&href) {
                    continue;
                }
                seen.insert(href.clone());
                let slug = util::url_slug(&href);
                let title = util::capitalize(sep.replace_all(&slug, " ").trim());
                if !title.is_empty() {
                    out.push(SR::web_row(&slug, title, href));
                }
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Best-effort enrichment from SR's public search page: programmes and
    /// episodes SR exposes on sverigesradio.se but that are missing from API
    /// search results. Never fails (empty on any error).
    pub fn web_discovery(&self, q: &str, limit: usize) -> Vec<Track> {
        if q.trim().is_empty() {
            return vec![];
        }
        let Ok(raw) = self.request(
            &format!(
                "{WEB}/sok?{}",
                urlencode(&pairs(&[("query", q.to_string())]), "")
            ),
            7,
        ) else {
            return vec![];
        };
        let text = util::html_unescape(&raw)
            .replace("\\u002F", "/")
            .replace("\\u002f", "/")
            .replace("\\/", "/");
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        static EP: OnceLock<Regex> = OnceLock::new();
        static PR: OnceLock<Regex> = OnceLock::new();
        static CAND: OnceLock<Regex> = OnceLock::new();
        static SEP: OnceLock<Regex> = OnceLock::new();
        let ep = rx(
            &EP,
            r#"(?i)href=["']((?:https://www\.sverigesradio\.se)?/avsnitt/[^"'#?]+)["']"#,
        );
        let pr = rx(
            &PR,
            r#"(?i)href=["']((?:https://www\.sverigesradio\.se)?/(?:program|podd)/[^"'#?]+)["']"#,
        );
        let cand = rx(&CAND, r#"(?i)(?:aria-label|title)=["']([^"']{3,180})["']"#);
        let sep = rx(&SEP, r"[-_]+");
        for (kind, pat) in [("podcast", ep), ("program", pr)] {
            for m in pat.captures_iter(&text) {
                let href = absolute(&m[1]);
                if seen.contains(&href) {
                    continue;
                }
                seen.insert(href.clone());
                let pos = m.get(0).map(|x| x.start()).unwrap_or(0);
                let window = util::char_window(&text, pos, 500, 900);
                let clean = util::collapse_ws(&util::strip_tags(window, " "));
                let slug = util::url_slug(&href);
                let mut title = sep.replace_all(&slug, " ").trim().to_string();
                // Prefer nearby aria-label/title text if available, otherwise readable slug.
                let attrs = util::char_window(&text, pos, 350, 350);
                if let Some(c) = cand.captures_iter(attrs).last() {
                    title = util::html_unescape(&c[1]).trim().to_string();
                }
                if title.is_empty() {
                    continue;
                }
                if kind == "program" {
                    let mut t = sr_track(format!("sr:webprogram:{slug}"), title, "program", false);
                    t.album = "Program".into();
                    t.set_meta("webpage", href);
                    t.set_meta("web_discovery", true);
                    t.set_meta("description", util::take_chars(&clean, 400));
                    out.push(t);
                } else {
                    let mut t = sr_track(format!("sr:webepisode:{slug}"), title, "podcast", true);
                    t.album = "SR Podcast".into();
                    t.set_meta("webpage", href);
                    t.set_meta("needs_resolve", true);
                    t.set_meta("web_discovery", true);
                    out.push(t);
                }
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// Programmes, API episode search and web discovery, de-duplicated by
    /// (kind, title, artist). Queries shorter than 2 characters return nothing.
    pub fn search_all(&self, q: &str, limit: usize) -> Vec<Track> {
        let q = q.trim();
        if q.chars().count() < 2 {
            return vec![];
        }
        let groups = [
            self.programs(q, limit.min(12)).unwrap_or_default(),
            self.legacy_episode_search(q, limit.min(24)),
            self.web_discovery(q, limit.min(20)),
        ];
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for group in groups {
            for t in group {
                let key = (
                    t.kind.clone(),
                    util::casefold(t.title.trim()),
                    util::casefold(t.artist.trim()),
                );
                if !seen.insert(key) {
                    continue;
                }
                out.push(t);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// Same as [`SR::search_all`] (the name the 1.x UI and service use).
    pub fn search_programmes_and_episodes(&self, q: &str, limit: usize) -> Vec<Track> {
        self.search_all(q, limit)
    }

    /// Latest episodes of every programme whose name or description matches.
    pub fn program_search(&self, q: &str, limit: usize) -> Vec<Track> {
        let Ok(data) = self.get("/programs", &[("size", "500".into())]) else {
            return vec![];
        };
        let needle = util::casefold(q);
        let mut out = Vec::new();
        for p in arr(&data, "programs") {
            let text = util::casefold(&format!(
                "{} {}",
                util::first_str(p, &["name"]),
                util::first_str(p, &["description"])
            ));
            if !needle.is_empty() && !text.contains(&needle) {
                continue;
            }
            let pid = s(p, "id");
            let name = p
                .get("name")
                .map(crate::model::value_str)
                .unwrap_or_else(|| "Sveriges Radio".into());
            let eps = self
                .get(
                    "/episodes/index",
                    &[
                        ("programid", pid),
                        ("pagesize", "4".into()),
                        ("size", "4".into()),
                        ("audioquality", "hi".into()),
                    ],
                )
                .map(|d| arr(&d, "episodes").to_vec())
                .unwrap_or_default();
            for e in &eps {
                let audio = [e.get("listenpodfile"), e.get("downloadpodfile")]
                    .into_iter()
                    .flatten()
                    .find(|v| crate::model::value_bool(v))
                    .cloned()
                    .unwrap_or(Value::Null);
                let url = if audio.is_object() {
                    s(&audio, "url")
                } else {
                    String::new()
                };
                let cover = [e.get("imageurltemplate"), e.get("imageurl")]
                    .into_iter()
                    .flatten()
                    .find(|v| crate::model::value_bool(v))
                    .map(crate::model::value_str)
                    .unwrap_or_else(|| s(p, "programimage"));
                let ad = if audio.is_object() && truthy(&audio, "duration") {
                    audio.get("duration")
                } else {
                    None
                };
                let duration = ad
                    .or(e.get("duration").filter(|v| crate::model::value_bool(v)))
                    .map(crate::model::value_f64)
                    .unwrap_or(0.0);
                let title = util::first_str(e, &["title", "name"]);
                let mut meta = Meta::new();
                meta.insert(
                    "description".into(),
                    util::strip_tags(&s(e, "description"), "").into(),
                );
                meta.insert("needs_resolve".into(), url.is_empty().into());
                meta.extend(sr_date(e));
                out.push(Track {
                    source: "sr".into(),
                    id: format!("sr:episode:{}", s(e, "id")),
                    title: if title.is_empty() {
                        "Avsnitt".into()
                    } else {
                        title
                    },
                    artist: name.clone(),
                    album: "SR Podcast".into(),
                    duration,
                    url,
                    cover,
                    kind: "podcast".into(),
                    seekable: true,
                    meta,
                    ..Default::default()
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// One Open API episode object as a podcast row.
    pub fn episode_track(e: &Value) -> Option<Track> {
        if !e.is_object() {
            return None;
        }
        let empty = Value::Object(Default::default());
        let mut pod = e
            .get("listenpodfile")
            .filter(|v| v.is_object())
            .unwrap_or(&empty);
        if pod.as_object().is_some_and(|o| o.is_empty()) {
            if let Some(d) = e.get("downloadpodfile").filter(|v| v.is_object()) {
                pod = d;
            }
        }
        let program = e.get("program").filter(|v| v.is_object()).unwrap_or(&empty);
        let cover = cover_512(&util::first_str(
            e,
            &["imageurltemplate", "imageurl", "programimage"],
        ));
        let url = util::first_str(pod, &["url"]);
        let title = util::first_str(e, &["title", "name"]);
        let artist = util::first_str(program, &["name"]);
        let duration = [pod.get("duration"), e.get("duration")]
            .into_iter()
            .flatten()
            .find(|v| crate::model::value_bool(v))
            .map(crate::model::value_f64)
            .unwrap_or(0.0);
        let mut meta = Meta::new();
        meta.insert(
            "description".into(),
            util::strip_tags(&util::first_str(e, &["description"]), "").into(),
        );
        meta.insert("needs_resolve".into(), url.is_empty().into());
        meta.extend(sr_date(e));
        Some(Track {
            source: "sr".into(),
            id: format!("sr:episode:{}", s(e, "id")),
            title: if title.is_empty() {
                "Avsnitt".into()
            } else {
                title
            },
            artist: if artist.is_empty() {
                "Sveriges Radio".into()
            } else {
                artist
            },
            album: "SR Podcast".into(),
            duration,
            url,
            cover,
            kind: "podcast".into(),
            seekable: true,
            meta,
            ..Default::default()
        })
    }

    /// `/episodes/search` (empty on any error).
    pub fn legacy_episode_search(&self, q: &str, limit: usize) -> Vec<Track> {
        let Ok(data) = self.get(
            "/episodes/search",
            &[
                ("query", q.to_string()),
                ("size", limit.min(25).to_string()),
                ("audioquality", "hi".into()),
            ],
        ) else {
            return vec![];
        };
        let mut out: Vec<Track> = arr(&data, "episodes")
            .iter()
            .filter_map(SR::episode_track)
            .collect();
        out.truncate(limit);
        out
    }

    /// Resilient search: every SR endpoint is independent, so a failing web
    /// search never discards working Open API/programme results.
    pub fn search(&self, q: &str, limit: usize) -> Vec<Track> {
        let sources = [
            self.legacy_episode_search(q, limit.min(20)),
            self.program_search(q, limit.min(12)),
            self.web_search(q, limit.min(12)).unwrap_or_default(),
        ];
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for source in sources {
            for t in source {
                let key = if t.id.is_empty() {
                    format!(
                        "\u{0}{}\u{0}{}",
                        util::casefold(&t.title),
                        util::casefold(&t.artist)
                    )
                } else {
                    t.id.clone()
                };
                if !seen.insert(key) {
                    continue;
                }
                out.push(t);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    fn catalogue_path(&self) -> PathBuf {
        self.cache.join("programs.json")
    }

    fn read_cache(path: &Path) -> Result<Value> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| Error::msg(e.to_string()))
    }

    /// `/programs?size=500`, cached for 6 hours in `programs.json`; a stale
    /// cache is served when SR is unreachable (then `last_cache()` is true).
    pub fn program_catalogue(&self) -> Result<Value> {
        let cache = self.catalogue_path();
        self.last_cache.store(false, Ordering::Relaxed);
        let fresh = std::fs::metadata(&cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs_f64() < CATALOGUE_TTL);
        if fresh {
            if let Ok(d) = SR::read_cache(&cache) {
                return Ok(d);
            }
        }
        let fetched = self
            .get("/programs", &[("size", "500".into())])
            .and_then(|data| {
                let tmp = cache.with_extension("tmp");
                std::fs::write(&tmp, serde_json::to_vec(&data).unwrap_or_default())?;
                std::fs::rename(&tmp, &cache)?;
                Ok(data)
            });
        match fetched {
            Ok(d) => Ok(d),
            Err(e) => {
                if cache.exists() {
                    let d = SR::read_cache(&cache)?;
                    self.last_cache.store(true, Ordering::Relaxed);
                    Ok(d)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Programme rows ranked by match quality (exact, prefix, word, substring,
    /// description), then name.
    pub fn programs(&self, q: &str, limit: usize) -> Result<Vec<Track>> {
        let data = self.program_catalogue()?;
        let needle = util::casefold(q.trim());
        let mut ranked: Vec<(u8, String, Track)> = Vec::new();
        for p in arr(&data, "programs") {
            let name = util::first_str(p, &["name"]);
            let desc = util::strip_tags(&util::first_str(p, &["description"]), "");
            let n = util::casefold(&name);
            let d = util::casefold(&desc);
            let score = if needle.is_empty() || n == needle {
                0
            } else if n.starts_with(&needle) {
                1
            } else if util::word_match(&n, &needle) {
                2
            } else if n.contains(&needle) {
                3
            } else if d.contains(&needle) {
                5
            } else {
                continue;
            };
            let cover = cover_512(&util::first_str(
                p,
                &["programimage", "programimagetemplate"],
            ));
            let mut t = sr_track(format!("sr:program:{}", s(p, "id")), name, "program", false);
            t.album = "Program".into();
            t.cover = cover;
            t.set_meta("program_id", s(p, "id"));
            t.set_meta("description", desc);
            ranked.push((score, n, t));
        }
        ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Ok(ranked.into_iter().take(limit).map(|x| x.2).collect())
    }

    /// Episodes of one programme, newest first.
    pub fn program_episodes(&self, pid: &str, limit: usize) -> Result<Vec<Track>> {
        let data = self.get(
            "/episodes/index",
            &[
                ("programid", pid.to_string()),
                ("size", limit.to_string()),
                ("audioquality", "hi".into()),
            ],
        )?;
        let mut out: Vec<Track> = arr(&data, "episodes")
            .iter()
            .filter_map(SR::episode_track)
            .collect();
        out.sort_by(|a, b| {
            b.meta_f64("published_ts")
                .partial_cmp(&a.meta_f64("published_ts"))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    /// Now/next on a live channel (`sr:132` or `132`); `None` for a bad id.
    /// Each half fails independently and leaves its fields empty.
    pub fn live_info(&self, station_id: &str) -> Option<LiveInfo> {
        let cid: u64 = station_id.rsplit(':').next()?.trim().parse().ok()?;
        if cid == 0 {
            return None;
        }
        let mut out = LiveInfo::default();
        if let Ok(data) = self.get(
            "/scheduledepisodes/rightnow",
            &[("channelid", cid.to_string()), ("size", "10".into())],
        ) {
            let ch = Value::Object(obj(&data, "channel").clone());
            let cur = Value::Object(obj(&ch, "currentscheduledepisode").clone());
            let prog = Value::Object(obj(&cur, "program").clone());
            let nxt = Value::Object(obj(&ch, "nextscheduledepisode").clone());
            let nprog = Value::Object(obj(&nxt, "program").clone());
            out.program = util::first_str(&prog, &["name"]);
            out.episode = util::first_str(&cur, &["title"]);
            out.next_program = util::first_str(&nprog, &["name"]);
            if out.next_program.is_empty() {
                out.next_program = util::first_str(&nxt, &["title"]);
            }
            out.start = clock(&util::first_str(&cur, &["starttimeutc", "starttime"]));
            out.end = clock(&util::first_str(&cur, &["endtimeutc", "endtime"]));
            out.next_start = clock(&util::first_str(&nxt, &["starttimeutc", "starttime"]));
            out.next_end = clock(&util::first_str(&nxt, &["endtimeutc", "endtime"]));
        }
        if let Ok(data) = self.get(
            "/playlists/rightnow",
            &[("channelid", cid.to_string()), ("size", "10".into())],
        ) {
            let pl = Value::Object(obj(&data, "playlist").clone());
            let song = Value::Object(obj(&pl, "song").clone());
            let artist = util::first_str(&song, &["artist"]).trim().to_string();
            let title = util::first_str(&song, &["title"]).trim().to_string();
            out.song = [artist, title]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(" – ");
        }
        Some(out)
    }

    /// Fill in the audio URL (and og:title/og:image) of a web-discovered
    /// episode from its page. Tracks with a URL or without a page are returned
    /// unchanged, as is a page without an audio link.
    pub fn resolve(&self, mut t: Track) -> Result<Track> {
        if !t.url.is_empty() {
            return Ok(t);
        }
        let page = t.meta_str("webpage");
        if page.is_empty() {
            return Ok(t);
        }
        let raw = self.request(&page, 12)?;
        let normalized = util::html_unescape(&raw)
            .replace("\\u0026", "&")
            .replace("\\/", "/");
        static AUDIO: OnceLock<Regex> = OnceLock::new();
        let audio = rx(
            &AUDIO,
            r#"(?i)https://[^'"<>\s]+?\.(?:mp3|m4a|aac)(?:\?[^'"<>\s]*)?"#,
        );
        let Some(u) = audio.find(&normalized) else {
            return Ok(t);
        };
        t.url = u.as_str().to_string();
        let title = meta_prop(&raw, "og:title");
        if !title.is_empty() {
            t.title = title;
        }
        let image = meta_prop(&raw, "og:image");
        if !image.is_empty() {
            t.cover = image;
        }
        t.set_meta("needs_resolve", false);
        Ok(t)
    }
}

/// `<meta property|name=PROP content=...>` in either attribute order.
fn meta_prop(raw: &str, prop: &str) -> String {
    let p = regex_lite::escape(prop);
    for pat in [
        format!(r#"(?i)<meta[^>]+(?:property|name)=['"]{p}['"][^>]+content=['"]([^'"]+)"#),
        format!(r#"(?i)<meta[^>]+content=['"]([^'"]+)['"][^>]+(?:property|name)=['"]{p}['"]"#),
    ] {
        if let Some(c) = Regex::new(&pat).ok().and_then(|r| r.captures(raw)) {
            return util::html_unescape(&c[1]).trim().to_string();
        }
    }
    String::new()
}

/// First 10-13 digit run as a local `HH:MM` (milliseconds when > 10 digits).
pub fn clock(v: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let Some(m) = rx(&R, r"\d{10,13}").find(v) else {
        return String::new();
    };
    let Ok(ts) = m.as_str().parse::<i64>() else {
        return String::new();
    };
    let ts = if ts > 9_999_999_999 {
        ts as f64 / 1000.0
    } else {
        ts as f64
    };
    util::local_hm(ts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    type Routes = Arc<Mutex<Vec<String>>>;

    fn sr_with(
        dir: &Path,
        f: impl Fn(&str) -> std::result::Result<String, HttpError> + Send + Sync + 'static,
    ) -> (SR, Routes) {
        let seen: Routes = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let fetch: Fetcher = Arc::new(move |u: &str, _t: u64| {
            s2.lock().unwrap().push(u.to_string());
            f(u).map(String::into_bytes)
        });
        (SR::with(dir.join("sr"), fetch), seen)
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("nplay-sr-test-{:x}", fastrand::u64(..)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn live_channels() {
        let d = tmp();
        let (sr, _) = sr_with(&d, |_| Err(HttpError::Status(500)));
        let xs = sr.live();
        assert_eq!(
            xs.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["P1", "P2", "P3", "P4 Väst"]
        );
        assert_eq!(xs[0].id, "sr:132");
        assert_eq!(
            xs[0].url,
            "https://sverigesradio.se/topsy/direkt/132-hi-mp3.pls"
        );
        assert_eq!(
            (xs[0].kind.as_str(), xs[0].seekable, xs[0].artist.as_str()),
            ("radio", false, "Sveriges Radio")
        );
        assert!(d.join("sr").is_dir());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn programs_ranked_and_cached_with_stale_fallback() {
        let d = tmp();
        let catalogue = json!({"programs": [
            {"id": 1, "name": "Ekot dokumentär", "description": "x"},
            {"id": 2, "name": "Dokumentär", "description": "", "programimage": "https://i/{width}x{height}.jpg"},
            {"id": 3, "name": "P3 Dokumentär", "description": ""},
            {"id": 4, "name": "Vetenskap", "description": "<b>dokumentär</b> om allt"},
            {"id": 5, "name": "Kultur", "description": "nej"},
            {"id": 6, "name": "Superdokumentären", "description": ""}
        ]});
        let fail = Arc::new(AtomicBool::new(false));
        let f2 = fail.clone();
        let (sr, seen) = sr_with(&d, move |u| {
            if f2.load(Ordering::Relaxed) {
                return Err(HttpError::Unreachable("down".into()));
            }
            assert!(u.starts_with("https://api.sr.se/api/v2/programs?format=json&size=500"));
            Ok(catalogue.to_string())
        });
        let xs = sr.programs("Dokumentär", 100).unwrap();
        let ids: Vec<String> = xs.iter().map(|t| t.meta_str("program_id")).collect();
        assert_eq!(ids, ["2", "1", "3", "6", "4"]);
        assert_eq!(xs[0].cover, "https://i/512x512.jpg");
        assert_eq!(xs[0].id, "sr:program:2");
        assert_eq!(xs[4].meta_str("description"), "dokumentär om allt");
        assert!(!sr.last_cache());
        // Second call comes from the fresh cache.
        sr.programs("", 2).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Expire the cache and fail the network: stale cache served.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30000);
        let f = std::fs::File::options()
            .write(true)
            .open(sr.catalogue_path())
            .unwrap();
        f.set_modified(old).unwrap();
        fail.store(true, Ordering::Relaxed);
        assert_eq!(sr.programs("kultur", 10).unwrap().len(), 1);
        assert!(sr.last_cache());
        std::fs::remove_file(sr.catalogue_path()).unwrap();
        assert!(sr.programs("x", 10).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn episodes_mapping_and_order() {
        let d = tmp();
        let (sr, seen) = sr_with(&d, |_| {
            Ok(json!({"episodes": [
                {"id": 10, "title": "Old", "description": "<p>a</p>", "publishdateutc": "/Date(1600000000000)/",
                 "listenpodfile": {"url": "https://x/a.mp3", "duration": 60}, "program": {"name": "Prog"},
                 "imageurltemplate": "https://img/{format}.jpg"},
                {"id": 11, "name": "New", "publishdateutc": "/Date(1700000000000)/", "downloadpodfile": {"url": "", "duration": 0}, "duration": 30},
                "junk"
            ]}).to_string())
        });
        let xs = sr.program_episodes("42", 50).unwrap();
        assert!(seen.lock().unwrap()[0]
            .ends_with("/episodes/index?format=json&size=50&programid=42&audioquality=hi"));
        assert_eq!(xs.len(), 2);
        assert_eq!(xs[0].title, "New");
        assert_eq!(xs[0].artist, "Sveriges Radio");
        assert_eq!(xs[0].duration, 30.0);
        assert!(xs[0].meta_bool("needs_resolve"));
        let old = &xs[1];
        assert_eq!(old.id, "sr:episode:10");
        assert_eq!(
            (old.artist.as_str(), old.album.as_str(), old.url.as_str()),
            ("Prog", "SR Podcast", "https://x/a.mp3")
        );
        assert_eq!(old.cover, "https://img/512x512.jpg");
        assert_eq!(old.meta_str("description"), "a");
        assert_eq!(old.meta_f64("published_ts"), 1600000000.0);
        assert!(!old.meta_bool("needs_resolve"));
        assert_eq!(
            old.meta.keys().cloned().collect::<Vec<_>>(),
            [
                "description",
                "needs_resolve",
                "published_ts",
                "published_date"
            ]
        );
        let _ = std::fs::remove_dir_all(d);
    }

    const PAGE: &str = r#"<html><a class="x" href="/avsnitt/vem-var-du-nu">Vem &amp; <b>var</b>  du</a>
        <a href="https://www.sverigesradio.se/avsnitt/ab">AB</a>
        <div aria-label="Mitt program">x</div><a href="/program/mitt-program">p</a>
        <script>{"u":"\/avsnitt\/json-avsnitt-1"}</script></html>"#;

    #[test]
    fn web_search_and_discovery() {
        let d = tmp();
        let (sr, _) = sr_with(&d, |u| {
            assert!(u.starts_with("https://www.sverigesradio.se/sok?query="));
            Ok(PAGE.to_string())
        });
        let xs = sr.web_search("vem", 20).unwrap();
        let titles: Vec<&str> = xs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Vem & var du", "Ab", "Json avsnitt 1"]);
        assert_eq!(xs[0].id, "web:vem-var-du-nu");
        assert_eq!(
            xs[0].meta_str("webpage"),
            "https://www.sverigesradio.se/avsnitt/vem-var-du-nu"
        );
        assert!(xs[0].meta_bool("needs_resolve"));
        let ds = sr.web_discovery("vem", 30);
        let prog = ds.iter().find(|t| t.kind == "program").unwrap();
        assert_eq!(prog.id, "sr:webprogram:mitt-program");
        assert_eq!(prog.title, "Mitt program");
        assert!(prog.meta_bool("web_discovery"));
        assert!(ds.iter().any(|t| t.id == "sr:webepisode:vem-var-du-nu"));
        assert!(sr.web_discovery("  ", 30).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn search_survives_failing_endpoints() {
        let d = tmp();
        let (sr, _) = sr_with(&d, |u| {
            if u.contains("/episodes/search") {
                Ok(json!({"episodes": [{"id": 1, "title": "Hit", "listenpodfile": {"url": "https://a/1.mp3"}}]}).to_string())
            } else {
                Err(HttpError::Status(403))
            }
        });
        let xs = sr.search("hit", 30);
        assert_eq!(xs.len(), 1);
        assert_eq!(xs[0].id, "sr:episode:1");
        assert_eq!(sr.search_all("hit", 40).len(), 1);
        assert!(sr.search_all("h", 40).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn live_info_and_resolve() {
        let d = tmp();
        let (sr, _) = sr_with(&d, |u| {
            if u.contains("scheduledepisodes") {
                Ok(json!({"channel": {"currentscheduledepisode": {"title": "Ep", "program": {"name": "Morgon"}, "starttimeutc": "/Date(1700000000000)/"},
                    "nextscheduledepisode": {"title": "Nästa"}}}).to_string())
            } else if u.contains("playlists/rightnow") {
                Ok(json!({"playlist": {"song": {"artist": " Abba ", "title": "SOS"}}}).to_string())
            } else {
                Ok(r#"<meta property="og:title" content="Rätt &amp; titel"><meta content="https://img/x.jpg" name="og:image">
                     <script>"https:\/\/media.sr.se\/a.mp3?x=1&y=2"</script>"#.to_string())
            }
        });
        let li = sr.live_info("sr:132").unwrap();
        assert_eq!(
            (
                li.program.as_str(),
                li.episode.as_str(),
                li.next_program.as_str()
            ),
            ("Morgon", "Ep", "Nästa")
        );
        assert_eq!(li.song, "Abba – SOS");
        assert_eq!(li.start, util::local_hm(1700000000.0));
        assert_eq!(li.end, "");
        assert!(sr.live_info("sr:x").is_none());
        let mut t = Track::new("sr", "web:x", "slug");
        t.set_meta("webpage", "https://www.sverigesradio.se/avsnitt/x");
        t.set_meta("needs_resolve", true);
        let r = sr.resolve(t).unwrap();
        assert_eq!(r.url, "https://media.sr.se/a.mp3?x=1&y=2");
        assert_eq!(r.title, "Rätt & titel");
        assert_eq!(r.cover, "https://img/x.jpg");
        assert!(!r.meta_bool("needs_resolve"));
        let _ = std::fs::remove_dir_all(d);
    }

    /// Live network smoke test: `cargo test -- --ignored live_sr`.
    #[test]
    #[ignore]
    fn live_sr_network() {
        let d = tmp();
        let sr = SR::with(d.join("sr"), default_fetcher());
        let ps = sr.programs("p3 dokumentär", 3).unwrap();
        eprintln!(
            "programs: {:?}",
            ps.iter().map(|t| &t.title).collect::<Vec<_>>()
        );
        assert!(!ps.is_empty());
        let pid = ps[0].meta_str("program_id");
        let eps = sr.program_episodes(&pid, 5).unwrap();
        eprintln!(
            "episodes: {:?}",
            eps.iter()
                .map(|t| (&t.title, t.meta_str("published_date"), t.url.is_empty()))
                .collect::<Vec<_>>()
        );
        assert!(!eps.is_empty());
        eprintln!("live: {:?}", sr.live_info("sr:164"));
        let all = sr.search_all("sommar", 10);
        eprintln!(
            "search_all: {:?}",
            all.iter().map(|t| (&t.kind, &t.title)).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
