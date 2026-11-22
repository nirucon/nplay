//! Playback sessions, contexts, queue, shuffle/repeat, sleep timer, the
//! playback ticker and the radio metadata poller (1.x `App` + `PlaybackController`).

use super::{say, track_key, App, SessionInfo, UiRef};
use crate::model::Track;
use crate::paths::monotonic;
use serde_json::{Map, Value};
use std::sync::Arc;

impl App {
    // ---- transport-neutral queries -------------------------------------------

    /// 1.x `playback_active`: Spotify reports playing, or mpv has media loaded
    /// (paused mpv counts as active, as in 1.x).
    pub fn playback_active(&self) -> bool {
        let (spotify, playing) = {
            let st = self.state();
            (st.is_spotify(), st.spotify_state.is_playing)
        };
        if spotify {
            return playing;
        }
        self.player.active()
    }

    /// Audible right now: like `playback_active` but false while mpv is paused.
    pub fn playback_playing(&self) -> bool {
        let (spotify, playing) = {
            let st = self.state();
            (st.is_spotify(), st.spotify_state.is_playing)
        };
        if spotify {
            return playing;
        }
        self.player.active() && self.player.prop_bool("pause") != Some(true)
    }

    /// Position in seconds; Spotify is interpolated from the last poll.
    pub fn playback_position(&self) -> f64 {
        let spotify = {
            let st = self.state();
            if st.is_spotify() {
                let s = &st.spotify_state;
                let base = s.progress.unwrap_or(st.resume_position);
                let now = monotonic();
                let seen = s.seen.unwrap_or(now);
                let dur = if s.duration > 0.0 {
                    s.duration
                } else {
                    st.current.as_ref().map(|t| t.duration).unwrap_or(0.0)
                };
                let dur = if dur > 0.0 { dur } else { 1e12 };
                let live = if s.is_playing {
                    (now - seen).max(0.0)
                } else {
                    0.0
                };
                Some((base + live).min(dur))
            } else {
                None
            }
        };
        if let Some(p) = spotify {
            return p;
        }
        match self.player.prop_f64("time-pos") {
            Some(p) if p != 0.0 => p,
            _ => self.state().resume_position,
        }
    }

    pub fn playback_duration(&self) -> f64 {
        let (spotify, sd, cd) = {
            let st = self.state();
            (
                st.is_spotify(),
                st.spotify_state.duration,
                st.current.as_ref().map(|t| t.duration).unwrap_or(0.0),
            )
        };
        if spotify {
            return if sd > 0.0 { sd } else { cd };
        }
        match self.player.prop_f64("duration") {
            Some(d) if d != 0.0 => d,
            _ => cd,
        }
    }

    pub fn playback_seek_relative(&self, d: f64) {
        if self.state().is_spotify() {
            let target = (self.playback_position() + d).max(0.0);
            let app = self.arc();
            self.spotify_control(
                self.ui().as_ref(),
                "seek",
                move || app.spotify_with_device(|did| app.spotify.seek(target, Some(did))),
                None::<fn(Value)>,
            );
            let mut st = self.state();
            st.spotify_state.progress = Some(target);
            st.spotify_state.seen = Some(monotonic());
        } else {
            self.player.seek(d);
        }
        self.state_changed(&["Position"]);
    }

    pub fn playback_volume(&self, d: f64) {
        if self.state().is_spotify() {
            let v = {
                let mut st = self.state();
                st.volume = (st.volume + d).clamp(0.0, 100.0);
                st.volume
            };
            let app = self.arc();
            self.spotify_control(
                self.ui().as_ref(),
                "volume",
                move || app.spotify_with_device(|did| app.spotify.volume(v, Some(did))),
                None::<fn(Value)>,
            );
        } else {
            self.player.volume(d);
            let mut st = self.state();
            st.volume = (st.volume + d).clamp(0.0, 150.0);
        }
        self.state_changed(&["Volume"]);
    }

    /// Absolute volume (0..150; Spotify 0..100).
    pub fn playback_set_volume(&self, v: f64) {
        let cur = self.volume();
        self.playback_volume(v - cur);
    }

    pub fn playback_mute(&self) {
        if self.state().is_spotify() {
            self.state().volume = 0.0;
            let app = self.arc();
            self.spotify_control(
                self.ui().as_ref(),
                "muted",
                move || app.spotify_with_device(|did| app.spotify.volume(0.0, Some(did))),
                None::<fn(Value)>,
            );
        } else {
            self.player.mute();
        }
    }

    // ---- sessions ---------------------------------------------------------------

    /// Invalidate every previous async owner before a new track takes over.
    pub(crate) fn begin_playback_session(&self, t: &Track, transport: &str) -> u64 {
        let g = {
            let mut st = self.state();
            st.spotify_polling = false;
            if transport != "spotify" {
                st.spotify_pending_track_id.clear();
            }
            st.playback_generation += 1;
            let g = st.playback_generation;
            st.playback_state = SessionInfo {
                generation: g,
                source: t.source.clone(),
                transport: transport.into(),
                state: "starting".into(),
                track_id: t.id.clone(),
                artwork: t.cover.clone(),
                ..Default::default()
            };
            st.state.update_media(t, transport, g, "loading");
            g
        };
        self.waker.wake();
        self.emit_state("playback-state-changed", None);
        g
    }

    pub(crate) fn commit_playback_session(&self, t: &Track, transport: &str, state: &str) {
        self.statistics_tracker.flush(self.playback_playing());
        self.state().current = Some(t.clone());
        self.start_play_event(t);
        {
            let mut st = self.state();
            let g = st.playback_generation;
            st.playback_state = SessionInfo {
                generation: g,
                source: t.source.clone(),
                transport: transport.into(),
                state: state.into(),
                track_id: t.id.clone(),
                artwork: t.cover.clone(),
                ..Default::default()
            };
            st.state.update_media(t, transport, g, state);
        }
        self.emit_state("media-changed", Some(t));
        self.state_changed(&["PlaybackStatus", "Metadata"]);
    }

    pub(crate) fn finish_play_event(&self, completed: bool) {
        let ev = {
            let mut st = self.state();
            let ev = st
                .play_event_id
                .take()
                .map(|id| (id, st.play_event_listened.max(0.0)));
            st.play_event_started = 0.0;
            st.play_event_listened = 0.0;
            st.play_event_tick = 0.0;
            ev
        };
        if let Some((id, secs)) = ev {
            let _ = self.db.play_finished(id, secs, completed);
        }
    }

    pub(crate) fn start_play_event(&self, t: &Track) {
        self.finish_play_event(false);
        let id = self.db.play_started(t).ok().flatten();
        let mut st = self.state();
        st.play_event_id = id;
        if id.is_some() {
            st.play_event_started = monotonic();
            st.play_event_tick = st.play_event_started;
            st.play_event_listened = 0.0;
        }
    }

    /// Stop the transport (keeps Now Playing as a resume candidate).
    pub fn stop_playback(&self, ui: Option<&UiRef>) {
        self.statistics_tracker.flush(self.playback_playing());
        // Stop also cancels any in-flight asynchronous source handoff.
        self.state().playback_request += 1;
        self.finish_play_event(false);
        self.state().spotify_polling = false;
        self.waker.wake();
        self.player.stop();
        if self.state().is_spotify() {
            let app = self.arc();
            let app2 = self.arc();
            self.spotify_control(
                ui,
                "stopped",
                move || app.spotify_with_device(|did| app.spotify.pause(Some(did))),
                Some(move |_| app2.state().spotify_state.is_playing = false),
            );
        }
        self.state().playback_state.state = "stopped".into();
        self.mpris().changed(&["PlaybackStatus", "Metadata"]);
        let _ = self.statistics_sync.run_async(false, None);
    }

    /// User-facing Stop: clears the playback session and Now Playing.
    pub fn stop_and_clear(&self, ui: Option<&UiRef>) {
        self.stop_playback(ui);
        {
            let mut st = self.state();
            st.radio_poll_generation += 1;
            st.current = None;
            st.resume_candidate = false;
            st.resume_position = 0.0;
            st.play_context.clear();
            st.original_context.clear();
            st.context_index = -1;
            st.spotify_state = Default::default();
            st.playback_state = SessionInfo::default();
            st.state.clear();
        }
        self.waker.wake();
        self.emit_state("playback-state-changed", None);
        self.save_state();
        self.state_changed(&["PlaybackStatus", "Metadata"]);
        if let Some(u) = ui {
            u.status("Stopped · Now Playing cleared");
            u.invalidate();
        }
    }

    // ---- play -----------------------------------------------------------------

    pub fn play(&self, t: Track, ui: Option<&UiRef>, preserve_context: bool) {
        self.play_with(t, ui, preserve_context, 0.0, false)
    }

    /// 1.x `play(t, ui, preserve_context, resume_pos, _from_history)`. With a
    /// UI the start is asynchronous (the previous media stays authoritative
    /// until the new one is ready); without one it is synchronous.
    pub fn play_with(
        &self,
        t: Track,
        ui: Option<&UiRef>,
        preserve_context: bool,
        resume_pos: f64,
        from_history: bool,
    ) {
        let mut preserve = preserve_context;
        let (old_current, old_state, old_generation, was_spotify, request) = {
            let mut st = self.state();
            if t.kind == "radio" {
                st.play_context.clear();
                st.original_context.clear();
                st.context_index = -1;
                preserve = false;
            }
            let old_current = st.current.clone();
            if let Some(oc) = &old_current {
                if !from_history && oc.key() != t.key() {
                    st.back_stack.push(oc.clone());
                    let n = st.back_stack.len();
                    if n > 100 {
                        st.back_stack.drain(..n - 100);
                    }
                }
            }
            let was_spotify = st.is_spotify();
            st.playback_request += 1;
            (
                old_current,
                st.playback_state.clone(),
                st.playback_generation,
                was_spotify,
                st.playback_request,
            )
        };
        if t.source == "spotify" {
            self.play_spotify(t, ui, preserve, resume_pos, request);
            return;
        }
        // mpv-backed sources resolve and wait for file-loaded off the UI
        // thread; the previous Now Playing state stays until the media is ready.
        if let Some(ui) = ui {
            self.mpv_start
                .start(&self.arc(), t, ui, request, resume_pos, was_spotify);
            return;
        }
        let res = (|| -> crate::core::Result<()> {
            let (prepared, url, _started) = crate::services::playback_start::prepare(self, t)?;
            let rg = self.normalization_mode(Some(&prepared));
            let ytdl = (prepared.source == "youtube").then(|| self.yt.binary.clone());
            let opts = crate::player::StartOptions::from_config(&self.cfg, rg, ytdl);
            self.player.start(&url, &opts)?;
            crate::services::playback_start::commit(
                self,
                prepared,
                None,
                request,
                resume_pos,
                was_spotify,
                0,
                false,
            );
            Ok(())
        })();
        if let Err(e) = res {
            self.restore_failed_start(old_current, old_state, old_generation, None);
            warn!(
                "Playback failed · {}",
                crate::redact::redact_text(&e.to_string())
            );
        }
    }

    pub(crate) fn restore_failed_start(
        &self,
        old_current: Option<Track>,
        old_state: SessionInfo,
        old_generation: u64,
        ui: Option<&UiRef>,
    ) {
        let respot = {
            let mut st = self.state();
            if st.playback_state.state != "starting" {
                return;
            }
            let sp = old_current.as_ref().is_some_and(|t| t.source == "spotify");
            st.current = old_current;
            st.playback_state = old_state;
            st.playback_generation = old_generation;
            sp
        };
        if respot {
            self.spotify_poll_start(ui);
        }
    }

    // ---- contexts -------------------------------------------------------------

    pub fn set_play_context(&self, tracks: Vec<Track>, index: i64) {
        let chosen = {
            let mut st = self.state();
            st.original_context = tracks.clone();
            if tracks.is_empty() {
                st.play_context.clear();
                st.context_index = -1;
                return;
            }
            let i = index.clamp(0, tracks.len() as i64 - 1);
            let chosen = tracks[i as usize].clone();
            st.play_context = tracks;
            st.context_index = i;
            if st.shuffle_mode == "off" {
                return;
            }
            chosen
        };
        self.apply_shuffle_to_context(Some(chosen));
    }

    /// ReplayGain mode for `t` (`no`, `track`, `album`).
    pub fn normalization_mode(&self, t: Option<&Track>) -> &'static str {
        let ctx: Vec<String> = self
            .state()
            .play_context
            .iter()
            .map(|x| x.album.clone())
            .collect();
        let refs: Vec<&str> = ctx.iter().map(String::as_str).collect();
        crate::player::normalization_mode(
            self.cfg.getbool("normalization", true),
            &self.cfg.get("normalization_mode", "auto"),
            t.map(|t| t.album.as_str()).unwrap_or(""),
            &refs,
        )
    }

    pub fn cycle_repeat(&self, ui: Option<&UiRef>) {
        let mode = {
            let mut st = self.state();
            let modes = ["off", "track", "context"];
            let i = modes.iter().position(|m| *m == st.repeat_mode).unwrap_or(0);
            st.repeat_mode = modes[(i + 1) % 3].into();
            st.repeat_mode.clone()
        };
        self.cfg.set("repeat_mode", &mode);
        self.save_state();
        self.state_changed(&["LoopStatus"]);
        say(ui, &format!("Repeat · {}", super::capitalize(&mode)));
    }

    pub fn cycle_shuffle(&self, ui: Option<&UiRef>) {
        let (mode, reshuffle) = {
            let mut st = self.state();
            let modes = ["off", "shuffle", "smart"];
            let i = modes
                .iter()
                .position(|m| *m == st.shuffle_mode)
                .unwrap_or(0);
            st.shuffle_mode = modes[(i + 1) % 3].into();
            let mode = st.shuffle_mode.clone();
            let mut reshuffle = None;
            if mode == "off" && !st.original_context.is_empty() {
                let cur = track_key(st.current.as_ref());
                st.play_context = st.original_context.clone();
                st.context_index = if st.current.is_some() {
                    st.play_context
                        .iter()
                        .position(|x| x.key() == cur)
                        .unwrap_or(0) as i64
                } else {
                    0
                };
            } else if mode != "off" {
                reshuffle = Some(st.current.clone());
            }
            (mode, reshuffle)
        };
        self.cfg.set("shuffle_mode", &mode);
        if let Some(cur) = reshuffle {
            self.apply_shuffle_to_context(cur);
        }
        self.save_state();
        self.state_changed(&["Shuffle"]);
        say(ui, &format!("Shuffle · {}", super::capitalize(&mode)));
    }

    /// Shuffled context with `chosen` first; `smart` puts recently played
    /// tracks last and avoids the same artist twice in a row.
    pub(crate) fn ordered_context(&self, chosen: Option<Track>) -> Vec<Track> {
        let (mut xs, chosen, smart) = {
            let st = self.state();
            let xs = if st.original_context.is_empty() {
                st.play_context.clone()
            } else {
                st.original_context.clone()
            };
            (
                xs,
                chosen.or_else(|| st.current.clone()),
                st.shuffle_mode == "smart",
            )
        };
        if let Some(c) = &chosen {
            let k = c.key();
            xs.retain(|x| x.key() != k);
        }
        fastrand::shuffle(&mut xs);
        if smart {
            let recent: std::collections::HashSet<String> = self
                .db
                .history(200)
                .unwrap_or_default()
                .iter()
                .take(25)
                .map(Track::key)
                .collect();
            xs.sort_by_key(|x| recent.contains(&x.key()));
            let mut out: Vec<Track> = Vec::new();
            while !xs.is_empty() {
                let last = match out.last() {
                    Some(t) => t.artist.to_lowercase(),
                    None => chosen
                        .as_ref()
                        .map(|c| c.artist.to_lowercase())
                        .unwrap_or_default(),
                };
                let j = xs
                    .iter()
                    .position(|x| x.artist.to_lowercase() != last)
                    .unwrap_or(0);
                out.push(xs.remove(j));
            }
            xs = out;
        }
        match chosen {
            Some(c) => std::iter::once(c).chain(xs).collect(),
            None => xs,
        }
    }

    pub fn apply_shuffle_to_context(&self, chosen: Option<Track>) {
        {
            let st = self.state();
            if (st.original_context.is_empty() && st.play_context.is_empty())
                || st.shuffle_mode == "off"
            {
                return;
            }
        }
        let has = chosen.is_some();
        let ctx = self.ordered_context(chosen);
        let mut st = self.state();
        st.play_context = ctx;
        st.context_index = if has { 0 } else { -1 };
    }

    /// What plays next: queue head, repeat-track, next in context, or wrap.
    pub fn up_next(&self) -> Option<Track> {
        let st = self.state();
        if let Some(q) = st.queue.first() {
            return Some(q.clone());
        }
        if st.play_context.is_empty() {
            return None;
        }
        if st.repeat_mode == "track" && st.current.is_some() {
            return st.current.clone();
        }
        let i = st.context_index + 1;
        if i >= 0 && (i as usize) < st.play_context.len() {
            return Some(st.play_context[i as usize].clone());
        }
        if st.repeat_mode == "context" {
            return st.play_context.first().cloned();
        }
        None
    }

    // ---- track end / next / previous ---------------------------------------------

    /// Player end-of-file callback (runs on the player's event thread).
    pub fn on_track_end(&self) {
        let app = self.arc();
        // Leave the player's event thread before starting the next mpv.
        std::thread::spawn(move || app.track_ended());
    }

    fn track_ended(self: Arc<Self>) {
        self.finish_play_event(true);
        let ui = self.ui();
        let (mode, at_end) = {
            let st = self.state();
            (
                st.sleep_mode.clone(),
                !st.play_context.is_empty() && st.context_index >= st.play_context.len() as i64 - 1,
            )
        };
        if mode == "track" || (mode == "album" && at_end) {
            self.state().sleep_mode = "off".into();
            self.stop_playback(ui.as_ref());
            if let Some(u) = ui {
                let u2 = u.clone();
                let msg = if mode == "track" {
                    "Sleep · stopped at end of track"
                } else {
                    "Sleep · stopped at end of album/context"
                };
                u.post(Box::new(move || u2.status(msg)));
            }
            return;
        }
        match ui {
            Some(u) => {
                let app = self.clone();
                let u2 = u.clone();
                u.post(Box::new(move || app.next_track(Some(&u2), true)));
            }
            None => self.next_track(None, true),
        }
    }

    pub fn next_track(&self, ui: Option<&UiRef>, automatic: bool) {
        enum Next {
            Play(Track),
            Later(Track),
            End,
        }
        let next = {
            let mut st = self.state();
            if automatic && st.repeat_mode == "track" && st.current.is_some() {
                Next::Play(st.current.clone().unwrap())
            } else if !st.queue.is_empty() {
                Next::Later(st.queue.remove(0))
            } else if !st.play_context.is_empty()
                && st.context_index + 1 < st.play_context.len() as i64
            {
                st.context_index += 1;
                Next::Play(st.play_context[st.context_index as usize].clone())
            } else if !st.play_context.is_empty() && st.repeat_mode == "context" {
                st.context_index = 0;
                Next::Play(st.play_context[0].clone())
            } else if !st.later_queue.is_empty() {
                let t = st.later_queue.remove(0);
                st.play_context.clear();
                st.original_context.clear();
                st.context_index = -1;
                Next::Later(t)
            } else {
                Next::End
            }
        };
        match next {
            Next::Play(t) => self.play(t, ui, true),
            Next::Later(t) => {
                self.save_state();
                self.play(t, ui, true)
            }
            Next::End => say(ui, "End of playback context"),
        }
    }

    pub fn previous_track(&self, ui: Option<&UiRef>) {
        let pos = self.playback_position();
        if pos > 3.0 {
            if self.state().is_spotify() {
                let app = self.arc();
                self.spotify_control(
                    ui,
                    "restarted",
                    move || app.spotify_with_device(|did| app.spotify.seek(0.0, Some(did))),
                    None::<fn(Value)>,
                );
                let mut st = self.state();
                st.spotify_state.progress = Some(0.0);
                st.spotify_state.seen = Some(monotonic());
            } else {
                self.player.seek(-pos);
            }
            say(ui, "Restarted current track");
            return;
        }
        let t = {
            let mut st = self.state();
            if let Some(t) = st.back_stack.pop() {
                Some(t)
            } else if !st.play_context.is_empty() && st.context_index > 0 {
                st.context_index -= 1;
                Some(st.play_context[st.context_index as usize].clone())
            } else {
                None
            }
        };
        match t {
            Some(t) => self.play_with(t, ui, true, 0.0, true),
            None => say(ui, "Start of playback context"),
        }
    }

    /// Space: pause/resume, or resume the restored session.
    pub fn play_pause(&self, ui: Option<&UiRef>) {
        if self.state().is_spotify() {
            self.spotify_play_pause(ui);
            return;
        }
        if self.player.active() {
            self.statistics_tracker.flush(self.playback_playing());
            self.player.toggle();
            self.state_changed(&["PlaybackStatus"]);
            // Radio metadata is polled only while the stream plays.
            if let Some(t) = self.current() {
                if t.kind == "radio" && self.playback_playing() && !self.state().radio_polling {
                    self.radio_poll_start(&t, ui);
                }
            }
            return;
        }
        if let Some(t) = self.current() {
            let rp = self.state().resume_position;
            let pos = if rp != 0.0 {
                rp
            } else {
                self.db.resume_get(&t.key(), Some(&t.id)).unwrap_or(0.0)
            };
            self.play_with(t, ui, true, pos, false);
            return;
        }
        say(ui, "Nothing to resume · browse or search for music");
    }

    // ---- queue / library actions -------------------------------------------------

    pub fn queue_add(&self, t: Track, ui: Option<&UiRef>) {
        let title = t.title.clone();
        self.state().queue.push(t);
        self.save_state();
        say(ui, &format!("Queued · {title}"));
    }

    pub fn queue_next(&self, t: Track, ui: Option<&UiRef>) {
        let title = t.title.clone();
        self.state().queue.insert(0, t);
        self.save_state();
        say(ui, &format!("Play next · {title}"));
    }

    pub fn queue_after_context(&self, t: Track, ui: Option<&UiRef>) {
        let title = t.title.clone();
        self.state().later_queue.push(t);
        self.save_state();
        say(ui, &format!("Queued after current context · {title}"));
    }

    pub fn queue_remove(&self, index: usize, ui: Option<&UiRef>) {
        let q = {
            let mut st = self.state();
            if st.queue.is_empty() {
                return;
            }
            let i = index.min(st.queue.len() - 1);
            st.queue.remove(i);
            st.queue.clone()
        };
        self.save_state();
        if let Some(u) = ui {
            let n = q.len();
            u.replace_list(
                "QUEUE",
                super::track_rows(q),
                "Queue is empty",
                Some(&format!("{n} items")),
            );
        }
    }

    /// Move a queue entry; returns its new index.
    pub fn queue_move(&self, index: usize, delta: i64, ui: Option<&UiRef>) -> usize {
        let (j, q) = {
            let mut st = self.state();
            if st.queue.is_empty() {
                return 0;
            }
            let n = st.queue.len() as i64;
            let index = (index as i64).min(n - 1);
            let j = (index + delta).clamp(0, n - 1);
            if j == index {
                return j as usize;
            }
            st.queue.swap(index as usize, j as usize);
            (j as usize, st.queue.clone())
        };
        self.save_state();
        if let Some(u) = ui {
            u.update_items(super::track_rows(q), j);
            u.invalidate();
        }
        j
    }

    pub fn queue_play(&self, index: usize, ui: Option<&UiRef>) {
        let t = {
            let mut st = self.state();
            if st.queue.is_empty() {
                return;
            }
            let i = index.min(st.queue.len() - 1);
            st.queue.remove(i)
        };
        self.save_state();
        self.play(t, ui, false);
    }

    pub fn queue_clear(&self, ui: Option<&UiRef>) {
        self.state().queue.clear();
        self.save_state();
        if let Some(u) = ui {
            u.replace_list("QUEUE", Vec::new(), "Queue is empty", Some("Queue cleared"));
        }
    }

    pub fn queue_save_playlist(&self, ui: Option<&UiRef>) {
        if self.state().queue.is_empty() {
            say(ui, "Queue is empty");
            return;
        }
        let Some(ui) = ui else { return };
        let app = self.arc();
        let u = ui.clone();
        ui.prompt(
            "SAVE QUEUE AS PLAYLIST › ",
            "",
            false,
            Box::new(move |name| {
                let Some(name) = name.filter(|n| !n.is_empty()) else {
                    return;
                };
                let Ok(Some(pid)) = app.db.playlist_create(&name) else {
                    return;
                };
                let q = app.queue();
                for t in &q {
                    let _ = app.db.playlist_add(pid, t);
                }
                u.status(&format!("Saved queue · {name} · {} tracks", q.len()));
            }),
        );
    }

    pub fn set_rating(&self, t: Option<&Track>, rating: i64, ui: Option<&UiRef>) {
        let Some(t) = t else { return };
        let _ = self.db.rating_set(&t.id, rating, &t.source);
        say(ui, &format!("Rating · {rating}/5 · {}", t.title));
    }

    /// `f`: toggle the current track as favorite; true = added.
    pub fn favorite_toggle(&self, ui: Option<&UiRef>) -> Option<bool> {
        let t = self.current()?;
        let added = self.db.favorite(&t).unwrap_or(false);
        say(
            ui,
            if added {
                "Favorite added"
            } else {
                "Favorite removed"
            },
        );
        Some(added)
    }

    /// Save what the radio is playing (or the current track) as a discovery.
    pub fn save_discovery(&self, ui: Option<&UiRef>) {
        let Some(t) = self.current() else {
            say(ui, "Discovery · nothing playing");
            return;
        };
        let info = if t.kind == "radio" {
            self.radio_info(&t.id)
        } else {
            Map::new()
        };
        let g = |k: &str| info.get(k).map(crate::model::value_str).unwrap_or_default();
        let song = g("song");
        let raw = if song.is_empty() { g("title") } else { song };
        let raw = raw.trim().to_string();
        let mut artist = g("artist").trim().to_string();
        let mut title = if raw.is_empty() { t.title.clone() } else { raw };
        if title.contains(" - ") && artist.is_empty() {
            let (a, b) = title.split_once(" - ").unwrap();
            artist = a.to_string();
            title = b.to_string();
        }
        let mut data = Map::new();
        data.insert(
            "station".into(),
            Value::String(if t.kind == "radio" {
                t.title.clone()
            } else {
                String::new()
            }),
        );
        let ok = self
            .db
            .discovery_add(&title, &artist, &t.source, Some(&data))
            .unwrap_or(false);
        let shown: Vec<&str> = [artist.as_str(), title.as_str()]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect();
        say(
            ui,
            &if ok {
                format!("Saved discovery · {}", shown.join(" · "))
            } else {
                "Discovery unavailable".into()
            },
        );
    }

    // ---- sleep timer ------------------------------------------------------------

    /// `minutes | track | album | off`.
    pub fn sleep_set(&self, arg: &str, ui: Option<&UiRef>) {
        let a = arg.trim().to_lowercase();
        if a == "off" || a == "0" {
            let mut st = self.state();
            st.sleep_deadline = 0.0;
            st.sleep_mode = "off".into();
            drop(st);
            say(ui, "Sleep timer · off");
            return;
        }
        if a == "track" || a == "album" {
            let mut st = self.state();
            st.sleep_mode = a.clone();
            st.sleep_deadline = 0.0;
            drop(st);
            say(ui, &format!("Sleep · end of {a}"));
            return;
        }
        match a.parse::<i64>() {
            Ok(m) => {
                let mins = m.max(1);
                {
                    let mut st = self.state();
                    st.sleep_deadline = crate::paths::now() + (mins * 60) as f64;
                    st.sleep_mode = "time".into();
                }
                say(ui, &format!("Sleep · {mins} minutes"));
                self.ensure_ticker();
            }
            Err(_) => say(ui, "Sleep · minutes | track | album | off"),
        }
    }

    /// Stop when a timed sleep is due; true when it fired.
    pub fn sleep_check(&self) -> bool {
        let due = {
            let st = self.state();
            st.sleep_mode == "time"
                && st.sleep_deadline > 0.0
                && crate::paths::now() >= st.sleep_deadline
        };
        if !due {
            return false;
        }
        self.stop_playback(self.ui().as_ref());
        let mut st = self.state();
        st.sleep_deadline = 0.0;
        st.sleep_mode = "off".into();
        true
    }

    // ---- playback ticker ------------------------------------------------------------

    fn ticker_wanted(&self) -> Option<f64> {
        let (has_current, timed, deadline) = {
            let st = self.state();
            (
                st.current.is_some(),
                st.sleep_mode == "time" && st.sleep_deadline > 0.0,
                st.sleep_deadline,
            )
        };
        if !has_current {
            return None;
        }
        if self.playback_playing() {
            return Some(2.0);
        }
        if timed {
            return Some((deadline - crate::paths::now()).clamp(0.05, 3600.0));
        }
        None
    }

    /// Start the ticker thread when something plays (idempotent). It saves a
    /// checkpoint and feeds the listening tracker every 2 s, fires the sleep
    /// timer and runs the statistics sync every 60 s; it exits as soon as
    /// nothing plays (no wake-ups while idle or paused).
    pub(crate) fn ensure_ticker(&self) {
        {
            let mut st = self.state();
            if st.ticker_running || st.current.is_none() {
                return;
            }
            st.ticker_running = true;
        }
        if self.ticker_wanted().is_none() {
            self.state().ticker_running = false;
            return;
        }
        let app = self.arc();
        let spawned = std::thread::Builder::new()
            .name("nplay-ticker".into())
            .spawn(move || app.ticker_loop());
        if spawned.is_err() {
            self.state().ticker_running = false;
        }
    }

    fn ticker_loop(self: Arc<Self>) {
        let mut last_sync = monotonic();
        while let Some(wait) = self.ticker_wanted() {
            self.waker.sleep(wait);
            if self.sleep_check() {
                if let Some(u) = self.ui() {
                    let u2 = u.clone();
                    u.post(Box::new(move || {
                        u2.status("Sleep timer · playback stopped")
                    }));
                }
                continue;
            }
            let playing = self.playback_playing();
            let cur = self.current();
            if playing {
                let pos = self.playback_position();
                let vol = self.volume();
                self.checkpoint(Some(pos), Some(vol));
            }
            self.statistics_tracker.tick(cur.as_ref(), playing);
            if monotonic() - last_sync >= 60.0 {
                last_sync = monotonic();
                let _ = self.statistics_sync.run_async(false, None);
            }
        }
        self.statistics_tracker.flush(false);
        self.state().ticker_running = false;
        // A state change between the last check and the flag reset must not
        // leave playback without a ticker.
        if self.ticker_wanted().is_some() {
            self.ensure_ticker();
        }
    }

    // ---- radio metadata --------------------------------------------------------------

    /// Poll live metadata while `t` (a radio stream) plays: SR every 45 s, any
    /// other stream's ICY metadata every 15 s. Notifies on a change.
    pub fn radio_poll_start(&self, t: &Track, ui: Option<&UiRef>) {
        let gen = {
            let mut st = self.state();
            st.radio_poll_generation += 1;
            st.radio_polling = true;
            st.radio_poll_generation
        };
        self.waker.wake();
        let app = self.arc();
        let t = t.clone();
        let ui = ui.cloned();
        let _ = std::thread::Builder::new()
            .name("nplay-radio-meta".into())
            .spawn(move || {
                let mut last = String::new();
                loop {
                    let owned = {
                        let st = app.state();
                        gen == st.radio_poll_generation
                            && st
                                .current
                                .as_ref()
                                .is_some_and(|c| c.kind == "radio" && c.id == t.id)
                    };
                    if !owned || !app.player.active() || app.player.prop_bool("pause") == Some(true)
                    {
                        break;
                    }
                    let info = app.radio_metadata(&t);
                    let key = ["program", "episode", "song", "next_program"]
                        .iter()
                        .map(|k| {
                            info.get(*k)
                                .map(crate::model::value_str)
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join("|");
                    if !info.is_empty() {
                        app.state().radio_info.insert(t.id.clone(), info.clone());
                    }
                    if key != last {
                        if !last.is_empty() {
                            app.notifier.track_changed(&t, true, Some(&info));
                        }
                        last = key;
                        app.state_changed(&["Metadata"]);
                        if let Some(u) = &ui {
                            let u2 = u.clone();
                            u.post(Box::new(move || u2.invalidate()));
                        }
                    }
                    app.waker.sleep(if t.source == "sr" { 45.0 } else { 15.0 });
                }
                let mut st = app.state();
                if st.radio_poll_generation == gen {
                    st.radio_polling = false;
                }
            });
    }

    fn radio_metadata(&self, t: &Track) -> Map<String, Value> {
        if t.source == "sr" {
            return self
                .sr
                .live_info(&t.id)
                .and_then(|i| serde_json::to_value(i).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
        }
        let md = self
            .player
            .prop("metadata")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let media = self
            .player
            .prop("media-title")
            .map(|v| crate::model::value_str(&v))
            .unwrap_or_default();
        let media = media.trim().to_string();
        let g = |a: &str, b: &str| {
            md.get(a)
                .or_else(|| md.get(b))
                .map(crate::model::value_str)
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let artist = g("Artist", "artist");
        let mut title = g("Title", "title");
        if title.is_empty() {
            title = media;
        }
        let song = if !artist.is_empty()
            && !title.is_empty()
            && !title.to_lowercase().contains(&artist.to_lowercase())
        {
            format!("{artist} – {title}")
        } else {
            title
        };
        let mut m = Map::new();
        m.insert("program".into(), Value::String(String::new()));
        m.insert("episode".into(), Value::String(String::new()));
        m.insert("song".into(), Value::String(song));
        m
    }
}
