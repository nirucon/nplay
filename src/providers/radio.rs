//! NPLAY Radio (1.x `services/radio.py`) and custom radio station helpers.
//!
//! [`NPlayRadio::build`] mixes tracks related to a seed: local library search
//! on the seed's artist/album/genre, Navidrome `search_library(artist, 40, 5,
//! 5)`, and (when the app enables it) Spotify search; shuffled, without the
//! seed, without non-track rows and avoiding the same artist twice in a row.

use super::Navidrome;
use crate::model::Track;
use std::collections::HashSet;
use std::sync::Arc;

/// `search(query, limit) -> tracks`, e.g. the local DB or Spotify search.
pub type SearchFn = Arc<dyn Fn(&str, usize) -> crate::core::Result<Vec<Track>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct NPlayRadio {
    /// Local library search (`db.search(q, 120)`).
    pub db_search: Option<SearchFn>,
    /// Navidrome, when configured.
    pub navidrome: Option<Navidrome>,
    /// Spotify search; set only when Spotify is enabled and configured.
    pub spotify_search: Option<SearchFn>,
}

impl NPlayRadio {
    pub fn build(&self, seed: &Track, limit: usize) -> Vec<Track> {
        let mut pool: Vec<Track> = Vec::new();
        let genre = seed.meta_str("genre");
        let terms: Vec<&str> = [seed.artist.as_str(), seed.album.as_str(), genre.as_str()]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect();
        let mut seen: HashSet<String> = HashSet::from([seed.key()]);
        if let Some(db) = &self.db_search {
            for q in terms.iter().take(2) {
                if let Ok(xs) = db(q, 120) {
                    pool.extend(xs);
                }
            }
        }
        if let Some(n) = &self.navidrome {
            if !seed.artist.is_empty() {
                if let Ok(xs) = n.search_library(&seed.artist, 40, 5, 5) {
                    pool.extend(xs);
                }
            }
        }
        // Spotify contributes discovery tracks but never blocks radio creation.
        if let Some(sp) = &self.spotify_search {
            if !seed.artist.is_empty() {
                if let Ok(xs) = sp(&seed.artist, 10) {
                    pool.extend(xs.into_iter().filter(|x| x.kind == "track"));
                }
            }
        }
        fastrand::shuffle(&mut pool);
        let big = pool.len() > 10;
        let mut out = Vec::new();
        let mut last_artist = seed.artist.to_lowercase();
        for t in pool {
            let key = t.key();
            let artist = t.artist.to_lowercase();
            if seen.contains(&key) || t.kind != "track" {
                continue;
            }
            // Avoid immediate artist repetition where possible.
            if !artist.is_empty() && artist == last_artist && big {
                continue;
            }
            seen.insert(key);
            out.push(t);
            last_artist = artist;
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

/// Validate a custom station (1.x service `radio.add`): trimmed name and
/// http(s) URL, or the 1.x error text.
pub fn check_custom_station(name: &str, url: &str) -> Result<(String, String), String> {
    let name = name.trim();
    let url = url.trim();
    if name.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("station needs a name and an http(s) stream URL".into());
    }
    Ok((name.to_string(), url.to_string()))
}

/// A saved custom station as a playable row (1.x `DB.radios()` mapping).
pub fn custom_radio_track(id: i64, name: &str, url: &str, homepage: &str, artwork: &str) -> Track {
    let mut t = Track {
        source: "radio".into(),
        id: format!("custom:{id}"),
        title: name.into(),
        artist: "Custom radio".into(),
        url: url.into(),
        cover: artwork.into(),
        kind: "radio".into(),
        seekable: false,
        ..Default::default()
    };
    t.set_meta("homepage", homepage);
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(id: &str, artist: &str, kind: &str) -> Track {
        Track {
            id: id.into(),
            artist: artist.into(),
            kind: kind.into(),
            ..Default::default()
        }
    }

    #[test]
    fn build_mixes_and_filters() {
        let db: SearchFn = Arc::new(|q: &str, n: usize| {
            assert_eq!(n, 120);
            Ok(match q {
                "A" => vec![
                    tr("seed", "A", "track"),
                    tr("1", "A", "track"),
                    tr("al", "A", "album"),
                ],
                "Alb" => vec![tr("2", "B", "track"), tr("1", "A", "track")],
                _ => vec![],
            })
        });
        let sp: SearchFn = Arc::new(|_q: &str, _n: usize| {
            let mut t = tr("s1", "C", "track");
            t.source = "spotify".into();
            Ok(vec![t, tr("s2", "C", "artist")])
        });
        let r = NPlayRadio {
            db_search: Some(db),
            navidrome: None,
            spotify_search: Some(sp),
        };
        let mut seed = tr("seed", "A", "track");
        seed.album = "Alb".into();
        seed.set_meta("genre", "Pop");
        let out = r.build(&seed, 60);
        let mut keys: Vec<String> = out.iter().map(Track::key).collect();
        keys.sort();
        assert_eq!(keys, ["local:1", "local:2", "spotify:s1"]);
        assert_eq!(r.build(&seed, 1).len(), 1);
        assert!(NPlayRadio::default().build(&seed, 5).is_empty());
    }

    #[test]
    fn custom_station_helpers() {
        assert_eq!(
            check_custom_station(" Jazz ", " https://x/s ").unwrap(),
            ("Jazz".into(), "https://x/s".into())
        );
        assert!(check_custom_station("", "https://x").is_err());
        assert_eq!(
            check_custom_station("a", "ftp://x").unwrap_err(),
            "station needs a name and an http(s) stream URL"
        );
        let t = custom_radio_track(3, "Jazz", "https://x/s", "https://h", "");
        assert_eq!(
            (
                t.id.as_str(),
                t.artist.as_str(),
                t.kind.as_str(),
                t.seekable
            ),
            ("custom:3", "Custom radio", "radio", false)
        );
        assert_eq!(t.meta_str("homepage"), "https://h");
    }
}
