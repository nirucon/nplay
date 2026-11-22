//! Actual elapsed listening, buffered to server-compatible <= 5 s events.
//! Port of 1.x `statistics/tracker.py`.

use super::store::StatisticsStore;
use super::time;
use crate::model::Track;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default)]
struct State {
    last: Option<f64>,
    session: Option<String>,
    key: Option<(String, String)>,
    track: Option<Track>,
    pending: f64,
    started: f64,
}

struct Inner {
    store: StatisticsStore,
    enabled: AtomicBool,
    clock: Clock,
    wall: Clock,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct ListeningTracker {
    inner: Arc<Inner>,
}

impl ListeningTracker {
    pub fn new(store: StatisticsStore, enabled: bool) -> ListeningTracker {
        ListeningTracker::with_clocks(
            store,
            enabled,
            Arc::new(crate::paths::monotonic),
            Arc::new(crate::paths::now),
        )
    }

    /// `clock` is monotonic seconds, `wall` Unix seconds (tests inject both).
    pub fn with_clocks(
        store: StatisticsStore,
        enabled: bool,
        clock: Clock,
        wall: Clock,
    ) -> ListeningTracker {
        ListeningTracker {
            inner: Arc::new(Inner {
                store,
                enabled: AtomicBool::new(enabled),
                clock,
                wall,
                state: Mutex::new(State::default()),
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::SeqCst)
    }

    pub fn set_enabled(&self, on: bool) {
        self.inner.enabled.store(on, Ordering::SeqCst);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self, s: &mut State, mut seconds: f64) {
        if seconds <= 0.0 {
            return;
        }
        let Some(track) = s.track.clone() else { return };
        let session = s.session.clone().unwrap_or_default();
        // Split at local midnight: the server buckets by started_at in Europe/Stockholm.
        while seconds > 0.00001 {
            let start = s.started;
            let midnight = time::next_local_midnight(start);
            let amount = seconds.min(5.0).min((midnight - start).max(0.001));
            // Best effort; playback must remain unaffected.
            let _ = self.inner.store.add(&session, start, amount, &track, None);
            s.started += amount;
            seconds -= amount;
        }
    }

    fn drain(&self, s: &mut State, force: bool) {
        while s.pending >= 5.0 - 1e-6 {
            self.write(s, 5.0);
            s.pending -= 5.0;
        }
        if force && s.pending > 0.00001 {
            let p = s.pending;
            self.write(s, p);
            s.pending = 0.0;
        }
    }

    /// Write what is buffered and end the session.
    pub fn flush(&self, playing: bool) {
        let mut s = self.lock();
        self.flush_locked(&mut s, playing);
    }

    fn flush_locked(&self, s: &mut State, playing: bool) {
        if let Some(last) = s.last {
            if playing && s.track.is_some() {
                let now = (self.inner.clock)();
                let elapsed = now - last;
                if elapsed > 0.0 && elapsed <= 5.0 {
                    s.pending += elapsed;
                }
                s.last = Some(now);
            }
        }
        self.drain(s, true);
        s.last = None;
        s.session = None;
        s.key = None;
        s.track = None;
        s.pending = 0.0;
    }

    pub fn reset(&self) {
        self.flush(false);
    }

    /// Called by the player loop with the current track and play state.
    pub fn tick(&self, track: Option<&Track>, playing: bool) {
        let now = (self.inner.clock)();
        let mut s = self.lock();
        let track = match track {
            Some(t) if self.enabled() && playing => t,
            _ => {
                self.flush_locked(&mut s, false);
                return;
            }
        };
        let key = (track.source.clone(), track.id.clone());
        if s.key.as_ref() != Some(&key) {
            self.flush_locked(&mut s, false);
            s.key = Some(key);
            s.track = Some(track.clone());
            s.session = Some(time::uuid4());
            s.last = Some(now);
            return;
        }
        let elapsed = now - s.last.unwrap_or(now);
        s.last = Some(now);
        if elapsed <= 0.0 || elapsed > 5.0 {
            self.drain(&mut s, true); // no fictitious time during suspend
            return;
        }
        if s.pending == 0.0 {
            s.started = (self.inner.wall)() - elapsed;
        }
        s.pending += elapsed;
        self.drain(&mut s, false);
    }
}
