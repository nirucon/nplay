//! Streaming providers (port of 1.x `nplay/providers.py` and
//! `nplay/services/radio.py`).
//!
//! Public API (all handles are `Clone + Send + Sync`, methods take `&self`):
//!
//! - [`Navidrome`] (Subsonic API 1.16.1, salted-MD5 token auth, client `nplay`):
//!   `new(url, user, password, bitrate)`, `from_config(&Config) -> Option<_>`
//!   (same rule as 1.x `App.nav()`), `with_fetcher(fetcher)` (tests),
//!   `params()`, `call(method, extra)`, `stream_status(url, timeout)`,
//!   `ping`, `stats`, `recent`, `stream(id)`, `song(id)`, `cover(id)`,
//!   `albums(kind, limit)`, `album_tracks`, `search_library(q, songs, albums,
//!   artists)`, `artists`, `artist_albums`, `playlists`, `playlist_tracks`,
//!   `search(q, limit)`, `random_tracks`, `artist_tracks(id, shuffle)`,
//!   `random_artist_tracks() -> (artist name, tracks)`. Failures are
//!   [`NavidromeError`] (HTTP status + remedy text, never a URL or credential).
//!   [`navidrome_stream_failure`] turns a failed stream probe into the 1.x
//!   user-facing error (from `services/playback_start.py`).
//! - [`SR`] (Sveriges Radio Open API + public web search):
//!   `new()` (cache under `$DATA/sr`), `with(cache_dir, fetcher)`, `live()`
//!   (P1, P2, P3, P4 Väst), `get(path, params)`, `search(q, limit)`,
//!   `search_all(q, limit)` = `search_programmes_and_episodes`, `programs(q,
//!   limit)` (6 h cached catalogue `programs.json`, `last_cache()` tells when a
//!   stale cache was served), `program_episodes(id, limit)`, `live_info(id) ->
//!   Option<LiveInfo>`, `resolve(track)` (episode page -> audio URL).
//! - [`YouTube`] (yt-dlp): `new()` (prefers `~/.local/bin/yt-dlp`), `binary`,
//!   `available()`, `version()`, `search(q, limit)` (relevance re-rank by query
//!   tokens), `resolve(track) -> direct audio URL`.
//! - [`NPlayRadio`]: provider-neutral artist radio built from local search,
//!   Navidrome and (optionally) Spotify search callbacks.
//! - Custom radio helpers: [`check_custom_station`], [`custom_radio_track`].
//! - [`sort_by_published`]: the `relevance|newest|oldest` list sort that the
//!   UI applies to SR/YouTube rows (`youtube_sort` config key).
//! - [`published_meta`], [`sr_date`]: `published_ts`/`published_date` meta.
//!
//! Track meta keys (the engine and UI rely on them): Navidrome songs carry
//! `suffix`, `bitrate`; albums `album_id`, `song_count`, `year`; artists
//! `artist_id`; playlists `playlist_id`, `song_count`. SR episodes carry
//! `description`, `needs_resolve`, `published_ts`, `published_date`; programs
//! `program_id`, `description`; web rows `webpage`, `needs_resolve`,
//! `web_discovery`. YouTube rows carry `webpage`, `published_ts`,
//! `published_date`. Custom radios carry `homepage`.

pub mod navidrome;
pub mod radio;
pub mod sr;
pub mod util;
pub mod youtube;

pub use navidrome::{navidrome_stream_failure, Navidrome, NavidromeError, NAVIDROME_REMEDY};
pub use radio::{check_custom_station, custom_radio_track, NPlayRadio, SearchFn};
pub use sr::{LiveInfo, SR};
pub use util::{Fetcher, HttpError};
pub use youtube::YouTube;

use crate::core::jf;
use crate::model::{Meta, Track};
use serde_json::Value;

/// Stable publication metadata from a yt-dlp entry, without an extra request.
pub fn published_meta(x: &Value) -> Meta {
    let raw = [x.get("timestamp"), x.get("release_timestamp")]
        .into_iter()
        .flatten()
        .find(|v| crate::model::value_bool(v));
    let ts = raw.map(crate::model::value_f64).unwrap_or(0.0);
    let mut date = util::first_str(x, &["upload_date", "release_date"]);
    if date.is_empty() && ts != 0.0 {
        date = util::local_date(ts);
    } else if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) {
        date = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8]);
    }
    let mut m = Meta::new();
    m.insert("published_ts".into(), jf(ts));
    m.insert("published_date".into(), Value::String(date));
    m
}

/// Publication time of an SR episode (`publishdateutc` `/Date(ms)/`, ISO
/// text, or the broadcast start).
pub fn sr_date(e: &Value) -> Meta {
    let bt = e.get("broadcasttime");
    let raw = if util::truthy(e, "publishdateutc") {
        util::s(e, "publishdateutc")
    } else if util::truthy(e, "publishdate") {
        util::s(e, "publishdate")
    } else if let Some(b) = bt.filter(|b| b.is_object()) {
        util::s(b, "starttimeutc")
    } else {
        String::new()
    };
    static R: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex_lite::Regex::new(r"/Date\((\d+)").unwrap());
    let mut ts = r
        .captures(&raw)
        .and_then(|c| c[1].parse::<f64>().ok())
        .map(|ms| ms / 1000.0)
        .unwrap_or(0.0);
    if ts == 0.0 && !raw.is_empty() {
        ts = util::iso_timestamp(&raw.replace('Z', "+00:00")).unwrap_or(0.0);
    }
    let mut m = Meta::new();
    m.insert("published_ts".into(), jf(ts));
    m.insert(
        "published_date".into(),
        Value::String(if ts != 0.0 {
            util::local_date(ts)
        } else {
            String::new()
        }),
    );
    m
}

/// The UI list sort for rows with `published_ts`: `newest`, `oldest`, or
/// anything else (relevance) to keep `items` unchanged. Stable.
pub fn sort_by_published(items: &mut [Track], mode: &str) {
    match mode {
        "newest" => items.sort_by(|a, b| {
            b.meta_f64("published_ts")
                .partial_cmp(&a.meta_f64("published_ts"))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "oldest" => items.sort_by(|a, b| {
            a.meta_f64("published_ts")
                .partial_cmp(&b.meta_f64("published_ts"))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => {}
    }
}

/// The next sort mode after `mode` (`relevance -> newest -> oldest -> ...`).
pub fn next_sort_mode(mode: &str) -> &'static str {
    match mode {
        "relevance" => "newest",
        "newest" => "oldest",
        _ => "relevance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn published_meta_formats() {
        let m = published_meta(&json!({"upload_date": "20240131"}));
        assert_eq!(m["published_date"], "2024-01-31");
        assert_eq!(m["published_ts"], json!(0.0));
        let m = published_meta(&json!({"timestamp": 1706700000}));
        assert_eq!(m["published_ts"], json!(1706700000.0));
        assert_eq!(m["published_date"], util::local_date(1706700000.0));
        let m = published_meta(&json!({}));
        assert_eq!(m["published_date"], "");
    }

    #[test]
    fn sr_date_variants() {
        let m = sr_date(&json!({"publishdateutc": "/Date(1700000000000)/"}));
        assert_eq!(m["published_ts"], json!(1700000000.0));
        let m = sr_date(&json!({"publishdate": "2023-11-14T22:13:20Z"}));
        assert_eq!(m["published_ts"], json!(1700000000.0));
        let m = sr_date(&json!({"broadcasttime": {"starttimeutc": "/Date(1700000000000)/"}}));
        assert_eq!(m["published_ts"], json!(1700000000.0));
        let m = sr_date(&json!({"publishdateutc": "nonsense"}));
        assert_eq!(m["published_date"], "");
    }

    #[test]
    fn sorting() {
        let mk = |id: &str, ts: f64| {
            let mut t = Track::new("sr", id, id);
            t.set_meta("published_ts", ts);
            t
        };
        let mut xs = vec![mk("a", 2.0), mk("b", 3.0), mk("c", 1.0)];
        sort_by_published(&mut xs, "newest");
        assert_eq!(xs.iter().map(|t| t.id.as_str()).collect::<String>(), "bac");
        sort_by_published(&mut xs, "oldest");
        assert_eq!(xs.iter().map(|t| t.id.as_str()).collect::<String>(), "cab");
        assert_eq!(next_sort_mode("oldest"), "relevance");
    }
}
