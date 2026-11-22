//! Non-blocking mpv startup and source handoff coordination (1.x
//! `services/playback_start.py`).
//!
//! Network resolution and mpv's file-loaded wait never run on the UI thread.
//! The previous authoritative media stays current until the new transport is
//! ready; a newer request (or Stop) makes an in-flight start stale.

use crate::app::{App, UiRef};
use crate::core::{Error, ErrorKind, Result};
use crate::model::Track;
use crate::providers::{navidrome_stream_failure, NavidromeError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Serializes mpv starts (one `player.start` at a time).
#[derive(Default)]
pub struct MpvStartCoordinator {
    start_lock: Mutex<()>,
}

/// Resolve the playable URL for `t`; returns the (possibly refreshed) track.
pub fn prepare(app: &App, mut t: Track) -> Result<(Track, String, Instant)> {
    let started = Instant::now();
    let url = if t.source == "youtube" {
        let w = t.meta_str("webpage");
        if !w.is_empty() {
            w
        } else if !t.id.is_empty() {
            format!("https://www.youtube.com/watch?v={}", t.id)
        } else {
            t.url.clone()
        }
    } else if t.source == "sr" && t.url.is_empty() {
        t = app.sr.resolve(t)?;
        t.url.clone()
    } else if t.source == "navidrome" {
        let n = app.nav().ok_or_else(|| {
            Error::from(NavidromeError::new(
                "Navidrome is not configured or is disabled",
                None,
                "",
                "set it up in Settings → Navidrome",
            ))
        })?;
        if t.url.is_empty() || t.cover.is_empty() {
            let fresh = n.song(&t.id)?;
            t.url = fresh.url;
            t.cover = fresh.cover;
            if fresh.duration > 0.0 {
                t.duration = fresh.duration;
            }
            if !fresh.meta.is_empty() {
                t.meta = fresh.meta;
            }
        }
        t.url.clone()
    } else if !t.url.is_empty() {
        t.url.clone()
    } else {
        t.path.clone()
    };
    if url.is_empty() {
        return Err(Error::new(
            ErrorKind::MediaUnavailable,
            "No playable audio URL",
        ));
    }
    info!(
        "mpv prepare request source={} track={} resolve_ms={}",
        t.source,
        t.id,
        started.elapsed().as_millis()
    );
    Ok((t, url, started))
}

/// An mpv load failure as an actionable, credential-free Navidrome error.
fn navidrome_stream_failed(app: &App, url: &str) -> Error {
    let status = app.nav().and_then(|n| n.stream_status(url, 6));
    navidrome_stream_failure(status).into()
}

impl MpvStartCoordinator {
    pub fn start(
        &self,
        app: &Arc<App>,
        t: Track,
        ui: &UiRef,
        request: u64,
        resume_pos: f64,
        was_spotify: bool,
    ) {
        ui.status(&format!("Loading · {}", t.title));
        ui.invalidate();
        let app = app.clone();
        let ui = ui.clone();
        let _ = std::thread::Builder::new().name(format!("nplay-mpv-{request}")).spawn(move || {
            let (src, id) = (t.source.clone(), t.id.clone());
            let res = (|| -> Result<Option<(Track, u128)>> {
                let (track, url, started) = prepare(&app, t)?;
                let load_started = Instant::now();
                {
                    let _g = app.mpv_start.start_lock.lock().unwrap_or_else(|e| e.into_inner());
                    if request != app.state().playback_request {
                        return Ok(None);
                    }
                    let rg = app.normalization_mode(Some(&track));
                    let ytdl = (track.source == "youtube").then(|| app.yt.binary.clone());
                    let opts = crate::player::StartOptions::from_config(&app.cfg, rg, ytdl);
                    if let Err(e) = app.player.start(&url, &opts) {
                        if track.source != "navidrome" {
                            return Err(e);
                        }
                        return Err(navidrome_stream_failed(&app, &url));
                    }
                }
                let load_ms = load_started.elapsed().as_millis();
                let total_ms = started.elapsed().as_millis();
                info!(
                    "mpv ready request={request} source={} track={} load_ms={load_ms} total_ms={total_ms}",
                    track.source, track.id
                );
                let cur = app.state().playback_request;
                if request != cur {
                    info!("mpv ready ignored stale request={request} current_request={cur}");
                    return Ok(None);
                }
                Ok(Some((track, total_ms)))
            })();
            match res {
                Ok(Some((track, total_ms))) => {
                    let u = ui.clone();
                    ui.post(Box::new(move || {
                        commit(&app, track, Some(&u), request, resume_pos, was_spotify, total_ms as u64, true)
                    }));
                }
                Ok(None) => {}
                Err(e) => {
                    let msg = crate::redact::redact_text(&e.to_string());
                    error!("mpv start failed request={request} source={src} track={id} · {msg}");
                    if request == app.state().playback_request {
                        let u = ui.clone();
                        ui.post(Box::new(move || u.status(&format!("Playback failed · {msg}"))));
                    }
                }
            }
        });
    }
}

/// Take over Now Playing for a loaded mpv track.
#[allow(clippy::too_many_arguments)]
pub fn commit(
    app: &App,
    t: Track,
    ui: Option<&UiRef>,
    request: u64,
    resume_pos: f64,
    was_spotify: bool,
    total_ms: u64,
    check_request: bool,
) {
    if check_request && request != app.state().playback_request {
        return;
    }
    app.begin_playback_session(&t, "mpv");
    if was_spotify {
        app.stop_spotify_session();
    }
    app.player.set_pause(false);
    app.commit_playback_session(&t, "mpv", "playing");
    let vol = {
        let mut st = app.state();
        st.resume_candidate = false;
        st.resume_position = 0.0;
        st.volume
    };
    app.player.set_volume(vol);
    if resume_pos > 0.0 && t.seekable {
        app.player.set_position(resume_pos);
        app.state().resume_position = resume_pos;
    }
    let _ = app.db.history_add(&t);
    app.save_state();
    if !(t.kind == "radio" && t.source == "sr") {
        app.notifier.track_changed(&t, false, None);
    }
    if let Some(u) = ui {
        app.set_ui(Some(u.clone()));
        u.now_playing();
        u.status(&if total_ms >= 2000 {
            format!(
                "Playing · {} · {:.1}s start",
                t.title,
                total_ms as f64 / 1000.0
            )
        } else {
            format!("Playing · {}", t.title)
        });
    }
    if t.kind == "radio" {
        if t.source == "sr" {
            app.refresh_radio_async(vec![t.clone()], ui);
        }
        app.radio_poll_start(&t, ui);
    }
    app.ensure_ticker();
}
