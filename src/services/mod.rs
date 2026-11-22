//! Engine services (port of 1.x `nplay/services/*`). Owner: engine track.
//!
//! - [`playback_start`]: `MpvStartCoordinator` (serialized, non-blocking mpv
//!   start with request guards), `prepare(app, track)` (resolve a playable
//!   URL: YouTube page, SR episode, fresh Navidrome stream/cover), `commit(..)`
//!   (take over Now Playing once mpv has the media loaded).
//! - [`search`]: `search(app, q, ui)`, the federated Quick Find (local at
//!   once, then Navidrome/Spotify/SR/YouTube in parallel, cross-source matches
//!   merged first), and `merge_matches(groups, preferred)`.
//! - [`settings`]: `open(app, action, ui)`, every `settings:*` menu action.
//! - [`radio_engine`]: the NPLAY Radio builder wired to this app's sources.

pub mod playback_start;
pub mod search;
pub mod settings;

use crate::app::App;
use crate::providers::NPlayRadio;
use std::sync::Arc;

/// NPLAY Radio for `app` (1.x `services/radio.py`): local search, Navidrome
/// when configured, Spotify search when enabled and connected.
pub fn radio_engine(app: &Arc<App>) -> NPlayRadio {
    let db = app.db.clone();
    let spotify_search: Option<crate::providers::SearchFn> =
        if app.cfg.getbool("spotify_enabled", false) && app.spotify_configured() {
            let sp = app.spotify.clone();
            Some(Arc::new(move |q: &str, n: usize| sp.search(q, n)))
        } else {
            None
        };
    NPlayRadio {
        db_search: Some(Arc::new(move |q: &str, n: usize| db.search(q, n as i64))),
        navidrome: app.nav(),
        spotify_search,
    }
}
