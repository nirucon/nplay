//! YouTube through yt-dlp (never a shell). See the parent module doc.
//!
//! Commands, as in 1.x: `yt-dlp --version` (5 s), `yt-dlp --dump-single-json
//! --flat-playlist --no-warnings ytsearch<N>:<query>` (25 s) and `yt-dlp -f
//! bestaudio -g --no-warnings <webpage>` (30 s).

use super::published_meta;
use super::util::{self, run_capture};
use crate::core::{Error, ErrorKind, Result};
use crate::model::{Meta, Track};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct YouTube {
    /// Path of yt-dlp ("" when missing).
    pub binary: String,
}

impl YouTube {
    /// Prefer an executable `~/.local/bin/yt-dlp`, else `yt-dlp` on PATH.
    pub fn new() -> YouTube {
        let local = crate::paths::home_dir().join(".local/bin/yt-dlp");
        let ok = std::fs::metadata(&local)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        let binary = if ok {
            local.to_string_lossy().into_owned()
        } else {
            crate::paths::which("yt-dlp")
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        YouTube { binary }
    }

    pub fn with_binary(binary: &str) -> YouTube {
        YouTube {
            binary: binary.to_string(),
        }
    }

    pub fn available(&self) -> bool {
        !self.binary.is_empty()
    }

    /// `yt-dlp --version` ("" when missing or failing).
    pub fn version(&self) -> String {
        if self.binary.is_empty() {
            return String::new();
        }
        run_capture(&self.binary, &["--version".into()], Duration::from_secs(5))
            .map(|c| c.stdout.trim().to_string())
            .unwrap_or_default()
    }

    /// Search YouTube. A failing yt-dlp run gives an empty list; a timeout or
    /// unparsable output is an error.
    pub fn search(&self, q: &str, limit: usize) -> Result<Vec<Track>> {
        if !self.available() {
            return Ok(vec![]);
        }
        let args = vec![
            "--dump-single-json".to_string(),
            "--flat-playlist".into(),
            "--no-warnings".into(),
            format!("ytsearch{limit}:{q}"),
        ];
        let p = run_capture(&self.binary, &args, Duration::from_secs(25))
            .map_err(|e| Error::new(ErrorKind::Provider, format!("yt-dlp {e}")))?;
        if p.code != Some(0) {
            return Ok(vec![]);
        }
        let d: Value = serde_json::from_str(&p.stdout).map_err(|e| {
            Error::new(
                ErrorKind::Provider,
                format!("yt-dlp returned invalid JSON: {e}"),
            )
        })?;
        let out = util::arr(&d, "entries")
            .iter()
            .filter(|x| x.is_object())
            .map(entry_track)
            .collect();
        Ok(rank(q, out))
    }

    /// Direct audio URL of a search row (`meta.webpage`, else `url`).
    pub fn resolve(&self, t: &Track) -> Result<String> {
        let page = match t.meta.get("webpage") {
            Some(v) => crate::model::value_str(v),
            None => t.url.clone(),
        };
        let args = vec![
            "-f".to_string(),
            "bestaudio".into(),
            "-g".into(),
            "--no-warnings".into(),
            page,
        ];
        let p = run_capture(&self.binary, &args, Duration::from_secs(30))
            .map_err(|e| Error::new(ErrorKind::MediaUnavailable, format!("yt-dlp {e}")))?;
        if p.code != Some(0) {
            let msg = p
                .stderr
                .trim()
                .lines()
                .last()
                .filter(|l| !l.is_empty())
                .unwrap_or("yt-dlp failed")
                .to_string();
            return Err(Error::new(ErrorKind::MediaUnavailable, msg));
        }
        p.stdout
            .trim()
            .lines()
            .next()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Error::new(ErrorKind::MediaUnavailable, "yt-dlp failed"))
    }
}

/// One flat-playlist entry as a track.
pub fn entry_track(x: &Value) -> Track {
    let vid = util::s(x, "id");
    let mut meta = Meta::new();
    let page = util::first_str(x, &["webpage_url"]);
    meta.insert(
        "webpage".into(),
        if page.is_empty() {
            format!("https://www.youtube.com/watch?v={vid}")
        } else {
            page
        }
        .into(),
    );
    meta.extend(published_meta(x));
    let mut cover = util::first_str(x, &["thumbnail"]);
    if cover.is_empty() && !vid.is_empty() {
        cover = format!("https://i.ytimg.com/vi/{vid}/hqdefault.jpg");
    }
    let artist = if util::truthy(x, "channel") {
        util::s(x, "channel")
    } else {
        util::s(x, "uploader")
    };
    Track {
        source: "youtube".into(),
        id: vid,
        title: util::s(x, "title"),
        artist,
        duration: x
            .get("duration")
            .map(crate::model::value_f64)
            .unwrap_or(0.0),
        url: util::s(x, "url"),
        cover,
        meta,
        ..Default::default()
    }
}

/// Query tokens: runs of Unicode letters/digits, longer than one character.
fn tokens(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|z| z.chars().count() > 1)
        .map(util::casefold)
        .collect()
}

/// Keep YouTube's relevance as a signal, but strongly prefer results that
/// contain the query tokens across title + creator.
pub fn rank(q: &str, out: Vec<Track>) -> Vec<Track> {
    let toks = tokens(q);
    if toks.is_empty() {
        return out;
    }
    let phrase = util::casefold(q.trim());
    let mut ranked: Vec<(i64, usize, Track)> = out
        .into_iter()
        .enumerate()
        .map(|(pos, t)| {
            let title = util::casefold(&t.title);
            let creator = util::casefold(&t.artist);
            let combined = format!("{title} {creator}");
            let hits = toks
                .iter()
                .filter(|z| combined.contains(z.as_str()))
                .count() as i64;
            let title_hits = toks.iter().filter(|z| title.contains(z.as_str())).count() as i64;
            let phrase_hit = (!phrase.is_empty() && combined.contains(&phrase)) as i64;
            (-(phrase_hit * 20 + hits * 4 + title_hits * 2), pos, t)
        })
        .collect();
    ranked.sort_by_key(|z| (z.0, z.1));
    ranked.into_iter().map(|z| z.2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entry_mapping() {
        let t = entry_track(
            &json!({"id": "abc", "title": "T", "uploader": "U", "duration": 61.5, "url": "https://www.youtube.com/watch?v=abc", "upload_date": "20240102"}),
        );
        assert_eq!(
            (t.source.as_str(), t.artist.as_str(), t.duration),
            ("youtube", "U", 61.5)
        );
        assert_eq!(t.cover, "https://i.ytimg.com/vi/abc/hqdefault.jpg");
        assert_eq!(t.meta_str("webpage"), "https://www.youtube.com/watch?v=abc");
        assert_eq!(t.meta_str("published_date"), "2024-01-02");
        assert_eq!(t.kind, "track");
        let t = entry_track(
            &json!({"id": "x", "channel": "C", "uploader": "U", "thumbnail": "https://t", "webpage_url": "https://w"}),
        );
        assert_eq!(
            (
                t.artist.as_str(),
                t.cover.as_str(),
                t.meta_str("webpage").as_str()
            ),
            ("C", "https://t", "https://w")
        );
    }

    #[test]
    fn ranking_prefers_query_tokens() {
        let mk = |title: &str, artist: &str| Track {
            title: title.into(),
            artist: artist.into(),
            ..Default::default()
        };
        let xs = vec![
            mk("Unrelated", "Other"),
            mk("Song live", "Band"),
            mk("Band - Song", "Band VEVO"),
        ];
        let r = rank("Band Song", xs);
        assert_eq!(r[0].title, "Band - Song");
        assert_eq!(r[1].title, "Song live");
        assert_eq!(r[2].title, "Unrelated");
        let same = rank("a", vec![mk("b", ""), mk("a", "")]);
        assert_eq!(same[0].title, "b");
        assert_eq!(tokens("Hej på dig_x"), ["hej", "på", "dig"]);
    }

    #[test]
    fn fake_ytdlp_search_and_resolve() {
        let d = std::env::temp_dir().join(format!("nplay-yt-test-{:x}", fastrand::u64(..)));
        std::fs::create_dir_all(&d).unwrap();
        let bin = d.join("yt-dlp");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
case "$1" in
 --version) echo 2026.01.01;;
 --dump-single-json) [ "$4" = "ytsearch2:fail me" ] && exit 1; echo '{"entries":[{"id":"a","title":"Zzz","uploader":"x"},{"id":"b","title":"Wanted","uploader":"y"}]}';;
 -f) [ "$5" = "bad" ] && { echo "ERROR: first" >&2; echo "ERROR: video gone" >&2; exit 1; }; echo "https://cdn/audio"; echo second;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let yt = YouTube::with_binary(bin.to_str().unwrap());
        assert_eq!(yt.version(), "2026.01.01");
        let xs = yt.search("wanted", 2).unwrap();
        assert_eq!(xs[0].id, "b");
        assert!(yt.search("fail me", 2).unwrap().is_empty());
        let mut t = Track::new("youtube", "b", "Wanted");
        t.set_meta("webpage", "https://www.youtube.com/watch?v=b");
        assert_eq!(yt.resolve(&t).unwrap(), "https://cdn/audio");
        t.set_meta("webpage", "bad");
        assert_eq!(yt.resolve(&t).unwrap_err().message, "ERROR: video gone");
        assert!(YouTube::with_binary("").search("x", 1).unwrap().is_empty());
        assert_eq!(YouTube::with_binary("").version(), "");
        let _ = std::fs::remove_dir_all(d);
    }

    /// Live yt-dlp smoke test: `cargo test -- --ignored live_youtube`.
    #[test]
    #[ignore]
    fn live_youtube_search() {
        let yt = YouTube::new();
        assert!(yt.available());
        eprintln!("yt-dlp {}", yt.version());
        let xs = yt.search("hello adele", 3).unwrap();
        eprintln!(
            "{:?}",
            xs.iter()
                .map(|t| (&t.title, &t.artist, t.meta_str("published_date")))
                .collect::<Vec<_>>()
        );
        assert!(!xs.is_empty());
    }
}
