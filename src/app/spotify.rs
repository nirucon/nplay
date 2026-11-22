//! Spotify transport dispatch: device binding (rebind once on a stale
//! device), play requests with frozen contexts, Web API reconciliation and the
//! state poller that runs only while Spotify plays.

use super::{App, SpotifyContext, SpotifyState, UiRef};
use crate::core::{Error, Result};
use crate::model::{Meta, Track};
use crate::paths::monotonic;
use crate::spotify::is_stale_device_error;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

fn merge_context_meta(into: &mut Track, from: &Track) {
    for (k, v) in from.meta.iter() {
        if k.starts_with("context_") {
            into.meta.insert(k.clone(), v.clone());
        }
    }
}

fn device_id(d: &Meta) -> String {
    d.get("id").map(crate::model::value_str).unwrap_or_default()
}

impl App {
    /// The runtime Connect device id (never a persisted librespot id).
    pub(crate) fn spotify_device_id(&self) -> String {
        self.state().spotify_state.device_id()
    }

    fn local_mode(&self) -> bool {
        self.cfg.get("spotify_playback", "local") == "local" && self.spotify.local.available()
    }

    fn spotify_local_device(&self, timeout: f64) -> Result<Meta> {
        self.spotify.local.start(false)?;
        let name = self.cfg.get("spotify_local_name", "NPLAY");
        let name = if name.is_empty() {
            "NPLAY".to_string()
        } else {
            name
        };
        let d = self.spotify.wait_for_device(Some(&name), timeout, 0.5)?;
        self.state().spotify_state.device = d.clone();
        Ok(d)
    }

    /// Bind a live device again: the local receiver (restarting it if
    /// needed) or the active external Connect device.
    pub(crate) fn spotify_rebind(&self, timeout: f64) -> Result<Meta> {
        let d = if self.local_mode() {
            let d = self.spotify_local_device(timeout)?;
            self.spotify.activate_device(&device_id(&d), 8.0)?;
            d
        } else {
            self.spotify.playback_device()?
        };
        self.state().spotify_state.device = d.clone();
        Ok(d)
    }

    /// Run a transport action against a live device, rebinding once if stale.
    pub(crate) fn spotify_with_device<F>(&self, action: F) -> Result<Value>
    where
        F: Fn(&str) -> Result<Value>,
    {
        let local = self.local_mode();
        let mut did = self.spotify_device_id();
        if did.is_empty() {
            let d = if local {
                self.spotify_rebind(20.0)?
            } else {
                self.spotify.playback_device()?
            };
            did = device_id(&d);
            self.state().spotify_state.device = d;
        }
        match action(&did) {
            Err(e) if local && is_stale_device_error(&e) => {
                let d = self.spotify_rebind(20.0)?;
                action(&device_id(&d))
            }
            r => r,
        }
    }

    /// Run a Spotify control on a worker; report through the UI, never panic.
    pub(crate) fn spotify_control<R, W, O>(
        &self,
        ui: Option<&UiRef>,
        label: &str,
        work: W,
        on_ok: Option<O>,
    ) where
        R: Send + 'static,
        W: FnOnce() -> Result<R> + Send + 'static,
        O: FnOnce(R) + Send + 'static,
    {
        let ui = ui.cloned();
        let label = label.to_string();
        std::thread::spawn(move || match work() {
            Ok(r) => match ui {
                Some(ui) => {
                    let u = ui.clone();
                    ui.post(Box::new(move || {
                        if let Some(f) = on_ok {
                            f(r);
                        }
                        u.status(&format!("Spotify · {label}"));
                        u.invalidate();
                    }));
                }
                None => {
                    if let Some(f) = on_ok {
                        f(r);
                    }
                }
            },
            Err(e) => {
                let msg = crate::redact::redact_text(&e.to_string());
                warn!("Spotify control {label} failed · {msg}");
                if let Some(ui) = ui {
                    let u = ui.clone();
                    ui.post(Box::new(move || {
                        u.error("Spotify control unavailable", &msg)
                    }));
                }
            }
        });
    }

    fn pause_or_resume_with_rebind(&self, did: &mut String, resume: bool) -> Result<Value> {
        if did.is_empty() {
            *did = device_id(&self.spotify_rebind(20.0)?);
        }
        let run = |d: &str| {
            if resume {
                self.spotify.resume(Some(d))
            } else {
                self.spotify.pause(Some(d))
            }
        };
        match run(did) {
            Err(e) if is_stale_device_error(&e) => {
                *did = device_id(&self.spotify_rebind(20.0)?);
                run(did)
            }
            r => r,
        }
    }

    pub(crate) fn spotify_play_pause(&self, ui: Option<&UiRef>) {
        let did = self.spotify_device_id();
        let playing = self.state().spotify_state.is_playing;
        let app = self.arc();
        if playing {
            let pos = self.playback_position();
            let app2 = self.arc();
            self.spotify_control(
                ui,
                "paused",
                move || {
                    let mut did = did;
                    app.pause_or_resume_with_rebind(&mut did, false)
                },
                Some(move |_| {
                    {
                        let mut st = app2.state();
                        st.spotify_state.is_playing = false;
                        st.spotify_state.progress = Some(pos);
                        st.spotify_state.seen = Some(monotonic());
                        st.resume_position = pos;
                    }
                    app2.save_state();
                    app2.state_changed(&["PlaybackStatus"]);
                }),
            );
        } else {
            let pos = {
                let st = self.state();
                match st.spotify_state.progress {
                    Some(p) if p != 0.0 => p,
                    _ => st.resume_position,
                }
            };
            let current = self.current();
            let app2 = self.arc();
            let ui2 = self.ui_or(ui);
            self.spotify_control(
                ui,
                "playing",
                move || {
                    let mut did = did;
                    app.pause_or_resume_with_rebind(&mut did, true)?;
                    // Recovery: if Spotify lost the paused session, restore track+position.
                    std::thread::sleep(Duration::from_millis(150));
                    let st = app.spotify.state()?;
                    if let Some(cur) = &current {
                        if st.track.as_ref().map(|t| t.id.as_str()) != Some(cur.id.as_str()) {
                            app.spotify.play(Some(cur), Some(&did), None)?;
                            if pos > 0.0 {
                                app.spotify.seek(pos, Some(&did))?;
                            }
                        }
                    }
                    Ok(true)
                },
                Some(move |_| {
                    {
                        let mut st = app2.state();
                        st.spotify_state.is_playing = true;
                        st.spotify_state.progress = Some(pos);
                        st.spotify_state.seen = Some(monotonic());
                    }
                    app2.state_changed(&["PlaybackStatus"]);
                    // Polling runs only while Spotify plays: resume it.
                    app2.spotify_poll_start(ui2.as_ref());
                }),
            );
        }
    }

    pub(crate) fn stop_spotify_session(&self) {
        self.state().spotify_polling = false;
        self.waker.wake();
        let _ = self.spotify.pause(None);
    }

    /// Freeze the context that belongs to a Spotify play request. Only tracks
    /// carrying `context_uri` (album/playlist) may own a multi-track context;
    /// a track that is not in the current context gets a 1/1 context.
    pub(crate) fn spotify_context_snapshot(
        &self,
        t: &Track,
        preserve_context: bool,
    ) -> SpotifyContext {
        let st = self.state();
        let key = t.key();
        let context_uri = t.meta_str("context_uri");
        let mut ctx =
            if !context_uri.is_empty() && (preserve_context || !st.play_context.is_empty()) {
                st.play_context.clone()
            } else {
                vec![t.clone()]
            };
        if ctx.is_empty() || !ctx.iter().any(|x| x.key() == key) {
            ctx = vec![t.clone()];
        }
        let original = if !st.original_context.is_empty()
            && st.original_context.iter().any(|x| x.key() == key)
        {
            st.original_context.clone()
        } else {
            ctx.clone()
        };
        let index = ctx.iter().position(|x| x.key() == key).unwrap_or(0) as i64;
        SpotifyContext {
            tracks: ctx,
            original,
            index,
            context_uri,
        }
    }

    pub(crate) fn apply_spotify_context(&self, snap: &SpotifyContext, t: &Track) {
        let key = t.key();
        let mut ctx = if snap.tracks.is_empty() {
            vec![t.clone()]
        } else {
            snap.tracks.clone()
        };
        if !ctx.iter().any(|x| x.key() == key) {
            ctx = vec![t.clone()];
        }
        let mut st = self.state();
        st.original_context = if snap.original.is_empty() {
            ctx.clone()
        } else {
            snap.original.clone()
        };
        st.context_index = ctx.iter().position(|x| x.key() == key).unwrap_or(0) as i64;
        st.play_context = ctx;
    }

    /// Best-effort observation of the Web API (eventually consistent): a
    /// mismatch is never a playback failure.
    pub fn spotify_observe_track(
        &self,
        track_id: &str,
        timeout: f64,
        device: &str,
    ) -> Option<crate::spotify::PlayerState> {
        let deadline = monotonic() + timeout.max(0.1);
        let mut last_observed: Option<String> = None;
        while monotonic() < deadline {
            if let Ok(st) = self.spotify.state() {
                let observed = st.track.as_ref().map(|t| t.id.clone()).unwrap_or_default();
                if last_observed.as_deref() != Some(observed.as_str()) {
                    info!(
                        "spotify observe requested={track_id} observed={} playing={} device={}",
                        if observed.is_empty() {
                            "none"
                        } else {
                            &observed
                        },
                        st.is_playing,
                        st.device
                            .get("name")
                            .map(crate::model::value_str)
                            .unwrap_or_default()
                    );
                    last_observed = Some(observed.clone());
                }
                let sdid = device_id(&st.device);
                if observed == track_id && (device.is_empty() || sdid.is_empty() || sdid == device)
                {
                    return Some(st);
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    pub(crate) fn play_spotify(
        &self,
        t: Track,
        ui: Option<&UiRef>,
        preserve: bool,
        resume_pos: f64,
        request: u64,
    ) {
        let (stg, snap, previous_resume_candidate) = {
            let mut st = self.state();
            st.spotify_track_generation += 1;
            st.spotify_polling = false;
            let stg = st.spotify_track_generation;
            drop(st);
            let snap = self.spotify_context_snapshot(&t, preserve);
            let mut st = self.state();
            let prev = st.resume_candidate;
            st.resume_candidate = false;
            st.spotify_pending_track_id = t.id.clone();
            (stg, snap, prev)
        };
        self.waker.wake();
        info!(
            "spotify play request request={request} track_gen={stg} track={} context_len={} context_uri={}",
            t.id,
            snap.tracks.len(),
            snap.context_uri
        );
        let mut local = self.cfg.get("spotify_playback", "local") == "local";
        // A missing local engine must not strand playback when an external
        // Connect device exists: fall back for this request only.
        let fallback = local && !self.spotify.local.available();
        if fallback {
            warn!(
                "librespot unavailable · trying external Spotify Connect for request {request} track={}",
                t.id
            );
            local = false;
        }
        if let Some(u) = ui {
            u.status(if fallback {
                "Spotify · librespot missing · trying Connect device…"
            } else if local {
                "Spotify · starting locally…"
            } else {
                "Spotify · starting on Connect device…"
            });
        }
        let app = self.arc();
        let tw = t.clone();
        let work = move || -> Result<Meta> {
            if local {
                // Normal playback never opens OAuth: without cached credentials
                // the engine fails at once with the AUTHORIZE remedy.
                let name = app.cfg.get("spotify_local_name", "NPLAY");
                let name = if name.is_empty() {
                    "NPLAY".to_string()
                } else {
                    name
                };
                let d = app.spotify.ensure_local_device(Some(&name), 20.0)?;
                let did = device_id(&d);
                app.spotify.activate_device(&did, 8.0)?;
                let mut last_error: Option<Error> = None;
                for delay in [0.0, 0.35, 0.75, 1.25] {
                    if delay > 0.0 {
                        std::thread::sleep(Duration::from_secs_f64(delay));
                    }
                    match app.spotify.play(Some(&tw), Some(&did), None) {
                        Ok(_) => {
                            last_error = None;
                            break;
                        }
                        Err(e) => {
                            if !e.message.contains("No active device") {
                                return Err(e);
                            }
                            last_error = Some(e);
                            let _ = app.spotify.transfer(&did, false);
                        }
                    }
                }
                if let Some(e) = last_error {
                    return Err(Error::new(
                        e.kind.clone(),
                        format!(
                            "Local Spotify device registered but did not accept playback · {e}"
                        ),
                    ));
                }
                if resume_pos > 0.0 && tw.seekable {
                    app.spotify.seek(resume_pos, Some(&did))?;
                }
                app.cfg.set("spotify_device_name", &name);
                let d = app
                    .spotify
                    .devices()
                    .ok()
                    .and_then(|ds| ds.into_iter().find(|x| device_id(x) == did))
                    .unwrap_or(d);
                return Ok(d);
            }
            let d = app.spotify.play_track(&tw).map_err(|e| {
                if fallback {
                    Error::new(
                        e.kind.clone(),
                        format!("Local Spotify engine (librespot) is not installed and no external Spotify Connect device is available · {e}"),
                    )
                } else {
                    e
                }
            })?;
            if resume_pos > 0.0 && tw.seekable {
                app.spotify.seek(resume_pos, Some(&device_id(&d)))?;
            }
            Ok(d)
        };
        let app = self.arc();
        let ui_c = ui.cloned();
        let ts = t.clone();
        let started = move |device: Meta| {
            app.spotify_started(ts, device, ui_c.as_ref(), request, stg, snap, resume_pos)
        };
        let app = self.arc();
        let ui_f = ui.cloned();
        let failed = move |err: String| {
            {
                let mut st = app.state();
                if request == st.playback_request && stg == st.spotify_track_generation {
                    st.spotify_pending_track_id.clear();
                    st.resume_candidate = previous_resume_candidate;
                }
            }
            match &ui_f {
                Some(u) => u.error("Spotify playback unavailable", &err),
                None => warn!("Spotify playback unavailable · {err}"),
            }
        };
        match ui {
            Some(ui) => {
                let ui = ui.clone();
                std::thread::spawn(move || match work() {
                    Ok(d) => ui.post(Box::new(move || started(d))),
                    Err(e) => {
                        let err = crate::redact::redact_text(&e.to_string());
                        if crate::spotify::is_not_authorized(&e) {
                            warn!("Spotify play request failed · {err}");
                        } else {
                            error!("Spotify play request failed · {err}");
                        }
                        ui.post(Box::new(move || failed(err)));
                    }
                });
            }
            None => match work() {
                Ok(d) => started(d),
                Err(e) => failed(crate::redact::redact_text(&e.to_string())),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spotify_started(
        &self,
        t: Track,
        device: Meta,
        ui: Option<&UiRef>,
        request: u64,
        stg: u64,
        snap: SpotifyContext,
        resume_pos: f64,
    ) {
        {
            let st = self.state();
            if request != st.playback_request || stg != st.spotify_track_generation {
                info!(
                    "spotify start ignored stale request={request}/{} track_gen={stg}/{} track={}",
                    st.playback_request, st.spotify_track_generation, t.id
                );
                return;
            }
        }
        // The play command succeeded: commit the requested metadata as PENDING
        // now, never waiting for eventually-consistent /me/player metadata.
        self.begin_playback_session(&t, "spotify");
        self.player.stop();
        self.apply_spotify_context(&snap, &t);
        self.commit_playback_session(&t, "spotify", "playing");
        {
            let mut st = self.state();
            st.playback_state.context_uri = snap.context_uri.clone();
            st.playback_state.spotify_phase = "pending".into();
            st.spotify_pending_track_id = t.id.clone();
            st.resume_position = resume_pos;
            st.spotify_state = SpotifyState {
                track: Some(t.clone()),
                is_playing: true,
                progress: Some(resume_pos),
                duration: t.duration,
                device: device.clone(),
                seen: Some(monotonic()),
                pending: true,
                context: Meta::new(),
            };
        }
        let _ = self.db.history_add(&t);
        self.save_state();
        self.notifier.track_changed(&t, false, None);
        if let Some(u) = ui {
            self.set_ui(Some(u.clone()));
        }
        let (gen, idx, len) = {
            let st = self.state();
            (
                st.playback_generation,
                st.context_index,
                st.play_context.len(),
            )
        };
        info!(
            "spotify pending commit playback_gen={gen} request={request} track_gen={stg} track={} context_index={idx} context_len={len}",
            t.id
        );
        if let Some(u) = ui {
            u.now_playing();
            u.status("Spotify · playing · syncing metadata…");
            // Keep the sync hint transient; reconciliation continues in the
            // background and stays visible in the log.
            let app = self.arc();
            let u = u.clone();
            let tid = t.id.clone();
            let title = t.title.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(2500));
                let ok = {
                    let st = app.state();
                    request == st.playback_request
                        && stg == st.spotify_track_generation
                        && st.current.as_ref().is_some_and(|c| c.id == tid)
                };
                if ok {
                    let u2 = u.clone();
                    u.post(Box::new(move || {
                        u2.status(&format!("Playing · {title}"));
                        u2.invalidate();
                    }));
                }
            });
        }
        self.spotify_reconcile_start(ui, request, stg, t, snap, device_id(&device));
    }

    /// Wait for the Web API to report the requested track; stale state is
    /// diagnostic only and never replaces current/context/artwork.
    pub(crate) fn spotify_reconcile_start(
        &self,
        ui: Option<&UiRef>,
        request: u64,
        stg: u64,
        requested: Track,
        snap: SpotifyContext,
        _device: String,
    ) {
        let pg = self.state().playback_generation;
        let app = self.arc();
        let ui = ui.cloned();
        let _ = std::thread::Builder::new().name("nplay-spotify-reconcile".into()).spawn(move || {
            let mut last_observed: Option<String> = None;
            let started = monotonic();
            let rid = requested.id.clone();
            loop {
                {
                    let st = app.state();
                    if !(request == st.playback_request
                        && stg == st.spotify_track_generation
                        && pg == st.playback_generation
                        && st.spotify_pending_track_id == rid)
                    {
                        break;
                    }
                }
                match app.spotify.state() {
                    Ok(ps) => {
                        let observed = ps.track.as_ref().map(|t| t.id.clone()).unwrap_or_default();
                        if last_observed.as_deref() != Some(observed.as_str()) {
                            info!(
                                "spotify reconcile request={request} track_gen={stg} requested={rid} observed={} playing={} age={:.1}s",
                                if observed.is_empty() { "none" } else { &observed },
                                ps.is_playing,
                                monotonic() - started
                            );
                            last_observed = Some(observed.clone());
                        }
                        if observed != rid {
                            app.waker.sleep(if monotonic() - started < 20.0 { 0.5 } else { 2.0 });
                            continue;
                        }
                        let mut incoming = ps.track.clone().unwrap_or_default();
                        merge_context_meta(&mut incoming, &requested);
                        {
                            let mut st = app.state();
                            // Re-check ownership right before committing.
                            if !(request == st.playback_request
                                && stg == st.spotify_track_generation
                                && pg == st.playback_generation
                                && st.spotify_pending_track_id == rid)
                            {
                                break;
                            }
                            st.spotify_state = SpotifyState::from_player(&ps);
                            st.spotify_pending_track_id.clear();
                        }
                        app.apply_spotify_context(&snap, &incoming);
                        let (idx, len) = {
                            let mut st = app.state();
                            st.current = Some(incoming.clone());
                            st.playback_state.track_id = incoming.id.clone();
                            st.playback_state.artwork = incoming.cover.clone();
                            st.playback_state.state = if ps.is_playing { "playing" } else { "paused" }.into();
                            st.playback_state.spotify_phase = "confirmed".into();
                            st.playback_state.context_uri = snap.context_uri.clone();
                            st.resume_position = ps.progress;
                            (st.context_index, st.play_context.len())
                        };
                        app.save_state();
                        app.state_changed(&["Metadata", "PlaybackStatus"]);
                        info!(
                            "spotify reconcile confirmed request={request} track_gen={stg} track={} context_index={idx} context_len={len}",
                            incoming.id
                        );
                        if let Some(u) = &ui {
                            let u2 = u.clone();
                            let title = incoming.title.clone();
                            u.post(Box::new(move || {
                                u2.status(&format!("Playing · {title}"));
                                u2.invalidate();
                            }));
                        }
                        app.state().spotify_polling = false;
                        app.spotify_poll_start(ui.as_ref());
                        return;
                    }
                    Err(e) => error!("Spotify reconciliation failed · {}", crate::redact::redact_text(&e.to_string())),
                }
                app.waker.sleep(1.0);
            }
            let st = app.state();
            info!(
                "spotify reconcile ended stale request={request}/{} track_gen={stg}/{} requested={rid}",
                st.playback_request, st.spotify_track_generation
            );
        });
    }

    /// Poll `/me/player` once a second while Spotify plays: native context
    /// advances, external changes, queue handoff, sleep timer, notifications.
    /// Stops by itself when playback pauses or another source takes over.
    pub fn spotify_poll_start(&self, ui: Option<&UiRef>) {
        let (gen, tgen) = {
            let mut st = self.state();
            if !st.spotify_pending_track_id.is_empty() {
                debug!(
                    "spotify poll deferred pending={}",
                    st.spotify_pending_track_id
                );
                return;
            }
            if st.spotify_polling && st.playback_state.source == "spotify" {
                return;
            }
            st.spotify_polling = true;
            info!(
                "spotify poll start playback_gen={} track_gen={} current={}",
                st.playback_generation,
                st.spotify_track_generation,
                st.current.as_ref().map(|t| t.id.as_str()).unwrap_or("")
            );
            (st.playback_generation, st.spotify_track_generation)
        };
        let app = self.arc();
        let ui = self.ui_or(ui);
        let _ = std::thread::Builder::new()
            .name("nplay-spotify-poll".into())
            .spawn(move || app.spotify_poll_loop(ui, gen, tgen));
    }

    fn spotify_poll_loop(self: Arc<Self>, ui: Option<UiRef>, gen: u64, tgen: u64) {
        loop {
            let (old_playing, old_index, old_id) = {
                let st = self.state();
                if !(st.spotify_polling
                    && gen == st.playback_generation
                    && tgen == st.spotify_track_generation
                    && st.is_spotify())
                {
                    break;
                }
                (
                    st.spotify_state.is_playing,
                    st.context_index,
                    st.current.as_ref().map(|t| t.id.clone()),
                )
            };
            let ps = match self.spotify.state() {
                Ok(ps) => ps,
                Err(e) => {
                    error!(
                        "Spotify poll failed · {}",
                        crate::redact::redact_text(&e.to_string())
                    );
                    self.waker.sleep(1.0);
                    continue;
                }
            };
            let mut changed_to: Option<Track> = None;
            {
                let mut st = self.state();
                if gen != st.playback_generation || tgen != st.spotify_track_generation {
                    debug!(
                        "spotify poll ignored stale playback_gen={gen}/{} track_gen={tgen}/{}",
                        st.playback_generation, st.spotify_track_generation
                    );
                    break;
                }
                st.spotify_state = SpotifyState::from_player(&ps);
                if let Some(mut incoming) = ps.track.clone() {
                    let spotify_ctx = ps
                        .context
                        .get("uri")
                        .map(crate::model::value_str)
                        .unwrap_or_default();
                    let active_ctx = st.playback_state.context_uri.clone();
                    let hit = st
                        .play_context
                        .iter()
                        .position(|x| x.source == "spotify" && x.id == incoming.id);
                    if let Some(h) = hit {
                        let from = st.play_context[h].clone();
                        merge_context_meta(&mut incoming, &from);
                    }
                    let differs = old_id.as_deref() != Some(incoming.id.as_str());
                    // A native album/playlist advance is valid only inside the
                    // context this session owns; an external change starts a
                    // fresh single-track context.
                    if differs
                        && (hit.is_none()
                            || (!active_ctx.is_empty()
                                && !spotify_ctx.is_empty()
                                && active_ctx != spotify_ctx))
                    {
                        info!(
                            "spotify external/context change old={} new={} spotify_context={spotify_ctx} active_context={active_ctx} · reset NPLAY context",
                            old_id.as_deref().unwrap_or("None"),
                            incoming.id
                        );
                        st.play_context = vec![incoming.clone()];
                        st.original_context = vec![incoming.clone()];
                        st.context_index = 0;
                        st.playback_state.context_uri.clear();
                    } else if let Some(h) = hit {
                        st.context_index = h as i64;
                    }
                    st.current = Some(incoming.clone());
                    st.playback_state.track_id = incoming.id.clone();
                    st.playback_state.artwork = incoming.cover.clone();
                    st.playback_state.state =
                        if ps.is_playing { "playing" } else { "paused" }.into();
                    st.resume_position = ps.progress;
                    let v = ps
                        .device
                        .get("volume_percent")
                        .map(crate::model::value_f64)
                        .unwrap_or(0.0);
                    if v != 0.0 {
                        st.volume = v;
                    }
                    if old_id.is_some() && differs {
                        info!(
                            "spotify track commit playback_gen={gen} track_gen={tgen} old={} new={} context_index={} context_len={}",
                            old_id.as_deref().unwrap_or(""),
                            incoming.id,
                            st.context_index,
                            st.play_context.len()
                        );
                        changed_to = Some(incoming);
                    }
                }
            }
            if let Some(incoming) = changed_to {
                self.finish_play_event(true);
                let (mode, ctx_len, queued) = {
                    let st = self.state();
                    (
                        st.sleep_mode.clone(),
                        st.play_context.len() as i64,
                        !st.queue.is_empty(),
                    )
                };
                if mode == "track" || (mode == "album" && ctx_len > 0 && old_index >= ctx_len - 1) {
                    self.state().sleep_mode = "off".into();
                    self.stop_playback(ui.as_ref());
                    break;
                }
                if queued {
                    let nxt = self.state().queue.remove(0);
                    self.save_state();
                    if let Some(u) = &ui {
                        let app = self.clone();
                        let u2 = u.clone();
                        u.post(Box::new(move || app.play(nxt, Some(&u2), true)));
                    }
                    break;
                }
                self.start_play_event(&incoming);
                let _ = self.db.history_add(&incoming);
                self.save_state();
                self.notifier.track_changed(&incoming, false, None);
                self.state_changed(&["Metadata", "PlaybackStatus"]);
            }
            if old_playing != ps.is_playing {
                self.state_changed(&["PlaybackStatus"]);
            }
            if let Some(u) = &ui {
                let u2 = u.clone();
                u.post(Box::new(move || u2.invalidate()));
            }
            if !ps.is_playing {
                // Nothing audible: stop polling (no wake-ups while paused).
                break;
            }
            self.waker.sleep(1.0);
        }
        let mut st = self.state();
        if gen == st.playback_generation && tgen == st.spotify_track_generation {
            st.spotify_polling = false;
        }
    }
}
