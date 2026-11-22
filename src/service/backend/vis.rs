//! CAVA lifecycle and pushed spectrum frames for the service (the
//! visualization half of 1.x `ServiceBackend`).
//!
//! Two ways to get frames:
//! - `visualization.data` (protocol 1.0): one frame per request (polling).
//! - `visualization.subscribe` (1.1): frames pushed from CAVA's own 30 fps
//!   clock to the subscribed connection, rate limited per subscriber (`fps`,
//!   20 % jitter tolerance), resampled to the subscriber's bar count, frames
//!   identical to the last one sent are suppressed (silence), `json` or `b64`.
//!
//! CAVA runs only while someone wants frames: with 0 subscribers and no data
//! request within [`VIS_DATA_RECENT`] seconds it stops at once; a poll-only
//! CAVA stops [`VIS_DATA_IDLE`] seconds after the last request. When CAVA dies
//! while subscribers remain it is restarted with a 1, 2, 4 ... 30 s backoff.
//! One monitor thread exists only while a CAVA instance exists and sleeps on
//! a condvar (deadline based, no periodic polling); with no visualization it
//! is gone, so an idle service has no visualizer threads at all.
//!
//! API (`VisHub`, `Clone + Send + Sync`): `new()`, `set_binary(path)`,
//! `available()`, `start(p)`, `stop(p)`, `data(p)`, `subscribe(p, client,
//! sink)`, `activate(client)`, `unsubscribe(client)`, `client_gone(client)`,
//! `status()`, `close()`.

use super::pyval::{backend_error, int_or, params_error, py_str};
use crate::paths::monotonic;
use crate::service::{FrameSink, RpcError};
use crate::visualizer::{resample, Visualizer};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

pub const VIS_DATA_RECENT: f64 = 2.0;
pub const VIS_DATA_IDLE: f64 = 15.0;
/// 1.x `Visualizer()` default bar count.
const DEFAULT_BARS: usize = 48;

struct Sub {
    sink: FrameSink,
    bars: usize,
    b64: bool,
    interval: f64,
    last_t: f64,
    last: Option<Vec<u8>>,
    active: bool,
    pushed: u64,
    suppressed: u64,
    dropped: u64,
}

#[derive(Default)]
struct State {
    vis: Option<Visualizer>,
    last: f64,
    subs: HashMap<u64, Sub>,
    failures: u32,
    restart_at: Option<f64>,
    closed: bool,
    monitor: bool,
}

struct Inner {
    st: Mutex<State>,
    cv: Condvar,
    binary: Mutex<String>,
}

#[derive(Clone)]
pub struct VisHub {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Default for VisHub {
    fn default() -> Self {
        Self::new()
    }
}

impl VisHub {
    pub fn new() -> VisHub {
        VisHub {
            inner: Arc::new(Inner {
                st: Mutex::new(State::default()),
                cv: Condvar::new(),
                binary: Mutex::new("cava".into()),
            }),
        }
    }

    /// CAVA executable (default `cava` on `$PATH`).
    pub fn set_binary(&self, path: impl Into<String>) {
        *lock(&self.inner.binary) = path.into();
    }

    /// CAVA is installed (1.x `shutil.which("cava")`).
    pub fn available(&self) -> bool {
        crate::paths::which(&lock(&self.inner.binary)).is_some()
    }

    fn new_vis(inner: &Arc<Inner>) -> Visualizer {
        let w1: Weak<Inner> = Arc::downgrade(inner);
        let w2 = w1.clone();
        let v = Visualizer::new(
            DEFAULT_BARS,
            Some(Arc::new(move |vis: &Visualizer, values: &[u8]| {
                if let Some(i) = w1.upgrade() {
                    on_frame(&i, vis, values);
                }
            })),
            Some(Arc::new(move |vis: &Visualizer| {
                if let Some(i) = w2.upgrade() {
                    on_exit(&i, vis);
                }
            })),
        );
        v.set_binary(lock(&inner.binary).clone());
        v
    }

    /// Start CAVA if needed (true when it was started now).
    fn ensure_locked(&self, st: &mut State) -> Result<bool, RpcError> {
        if st.vis.as_ref().is_some_and(Visualizer::running) {
            return Ok(false);
        }
        if !self.available() {
            return Err(backend_error(
                "CAVA is not installed",
                Some(json!({"remedy": "Install cava to enable visualization"})),
            ));
        }
        let vis = st
            .vis
            .get_or_insert_with(|| Self::new_vis(&self.inner))
            .clone();
        if !vis.running() {
            if !vis.start() {
                return Err(backend_error(
                    "CAVA could not start",
                    Some(
                        json!({"remedy": "Check the PipeWire/PulseAudio monitor source and restart cava"}),
                    ),
                ));
            }
            self.ensure_monitor(st);
            return Ok(true);
        }
        Ok(false)
    }

    fn ensure_monitor(&self, st: &mut State) {
        if st.monitor {
            self.inner.cv.notify_all();
            return;
        }
        let inner = self.inner.clone();
        let ok = std::thread::Builder::new()
            .name("nplay-visualizer-idle".into())
            .spawn(move || monitor(inner))
            .is_ok();
        st.monitor = ok;
    }

    pub fn start(&self, _p: &Map<String, Value>) -> Result<Value, RpcError> {
        let mut st = lock(&self.inner.st);
        self.ensure_locked(&mut st)?;
        st.last = monotonic();
        let bars = st
            .vis
            .as_ref()
            .map(Visualizer::bars)
            .unwrap_or(DEFAULT_BARS);
        Ok(json!({"available": true, "bars": bars}))
    }

    pub fn stop(&self, _p: &Map<String, Value>) -> Result<Value, RpcError> {
        let mut st = lock(&self.inner.st);
        // Never cut a push stream another connection is consuming.
        if st.subs.is_empty() {
            stop_locked(&self.inner, &mut st);
        }
        Ok(json!({"available": true}))
    }

    pub fn data(&self, p: &Map<String, Value>) -> Result<Value, RpcError> {
        let bars = int_or(p, "bars", 32)?.clamp(8, 96) as usize;
        let mut st = lock(&self.inner.st);
        if st.vis.as_ref().is_some_and(|v| !v.running()) {
            // CAVA can exit when the audio server or monitor source is not
            // ready yet: recover on the next request instead of staying dark.
            info!("service restarted CAVA after an unexpected exit");
        }
        self.ensure_locked(&mut st)?;
        st.last = monotonic();
        let vals = st.vis.as_ref().map(|v| v.get(bars)).unwrap_or_default();
        Ok(json!({"bars": vals}))
    }

    pub fn subscribe(
        &self,
        p: &Map<String, Value>,
        client: u64,
        sink: FrameSink,
    ) -> Result<Value, RpcError> {
        let bars = int_or(p, "bars", 32)?.clamp(8, 96);
        let fps = int_or(p, "fps", 30)?.clamp(1, 30);
        let encoding = super::pyval::get_or(p, "encoding")
            .map(py_str)
            .unwrap_or_else(|| "json".into());
        if encoding != "json" && encoding != "b64" {
            return Err(params_error("encoding must be json or b64", None));
        }
        let mut st = lock(&self.inner.st);
        self.ensure_locked(&mut st)?;
        st.subs.insert(
            client,
            Sub {
                sink,
                bars: bars as usize,
                b64: encoding == "b64",
                // 20 % tolerance so fps=30 accepts every CAVA frame despite jitter.
                interval: 0.8 / fps as f64,
                last_t: 0.0,
                last: None,
                active: false,
                pushed: 0,
                suppressed: 0,
                dropped: 0,
            },
        );
        self.inner.cv.notify_all();
        Ok(
            json!({"subscribed": true, "bars": bars, "fps": fps, "encoding": encoding, "source": "cava"}),
        )
    }

    /// Start pushing once the subscribe response is queued.
    pub fn activate(&self, client: u64) {
        if let Some(s) = lock(&self.inner.st).subs.get_mut(&client) {
            s.active = true;
        }
    }

    pub fn unsubscribe(&self, client: u64) -> Value {
        let mut st = lock(&self.inner.st);
        let was = st.subs.remove(&client).is_some();
        maybe_stop_locked(&self.inner, &mut st);
        self.inner.cv.notify_all();
        json!({"subscribed": false, "was_subscribed": was})
    }

    pub fn client_gone(&self, client: u64) {
        let mut st = lock(&self.inner.st);
        if st.subs.remove(&client).is_some() {
            maybe_stop_locked(&self.inner, &mut st);
            self.inner.cv.notify_all();
        }
    }

    pub fn status(&self) -> Value {
        let st = lock(&self.inner.st);
        let sum = |f: fn(&Sub) -> u64| st.subs.values().map(f).sum::<u64>();
        json!({
            "running": st.vis.as_ref().is_some_and(Visualizer::running),
            "source": "cava",
            "subscribers": st.subs.len(),
            "pushed": sum(|s| s.pushed),
            "suppressed": sum(|s| s.suppressed),
            "dropped": sum(|s| s.dropped),
            "polled_recently": st.vis.is_some() && monotonic() - st.last <= VIS_DATA_IDLE,
        })
    }

    pub fn close(&self) {
        let mut st = lock(&self.inner.st);
        st.closed = true;
        st.subs.clear();
        stop_locked(&self.inner, &mut st);
    }

    /// Whether the idle monitor thread exists (tests).
    pub fn monitor_running(&self) -> bool {
        lock(&self.inner.st).monitor
    }
}

fn stop_locked(inner: &Inner, st: &mut State) {
    if let Some(v) = st.vis.take() {
        v.stop();
    }
    st.restart_at = None;
    inner.cv.notify_all();
}

fn maybe_stop_locked(inner: &Inner, st: &mut State) {
    if st.subs.is_empty() && st.vis.is_some() && monotonic() - st.last > VIS_DATA_RECENT {
        stop_locked(inner, st);
    }
}

fn schedule_restart_locked(inner: &Inner, st: &mut State) {
    if st.subs.is_empty() || st.closed {
        return;
    }
    st.failures = (st.failures + 1).min(6);
    let delay = 2f64.powi(st.failures as i32 - 1).min(30.0);
    info!(
        "CAVA exited with {} subscriber(s); restarting in {:.0}s",
        st.subs.len(),
        delay
    );
    st.restart_at = Some(monotonic() + delay);
    inner.cv.notify_all();
}

fn retry_locked(inner: &Arc<Inner>, st: &mut State) {
    if st.subs.is_empty() || st.closed {
        return;
    }
    if st.vis.as_ref().is_some_and(Visualizer::running) {
        return;
    }
    let v = VisHub::new_vis(inner);
    st.vis = Some(v.clone());
    if !v.start() {
        schedule_restart_locked(inner, st);
    }
}

/// Runs only while a CAVA instance exists; sleeps until the next deadline
/// (poll-only idle stop or a scheduled restart) or a state change.
fn monitor(inner: Arc<Inner>) {
    let mut st = lock(&inner.st);
    loop {
        if st.closed || st.vis.is_none() {
            st.monitor = false;
            return;
        }
        let now = monotonic();
        if let Some(at) = st.restart_at {
            if now >= at {
                st.restart_at = None;
                retry_locked(&inner, &mut st);
                continue;
            }
        }
        let mut wait = st.restart_at.map(|at| at - now);
        if st.subs.is_empty() {
            let idle = now - st.last;
            if idle > VIS_DATA_IDLE {
                stop_locked(&inner, &mut st);
                continue;
            }
            let w = VIS_DATA_IDLE - idle + 0.05;
            wait = Some(wait.map_or(w, |x| x.min(w)));
        }
        st = match wait {
            Some(w) => {
                inner
                    .cv
                    .wait_timeout(st, Duration::from_secs_f64(w.max(0.001)))
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
            None => inner.cv.wait(st).unwrap_or_else(|e| e.into_inner()),
        };
    }
}

/// CAVA's reader thread, once per frame.
fn on_frame(inner: &Arc<Inner>, vis: &Visualizer, values: &[u8]) {
    let now = monotonic();
    let jobs: Vec<(u64, FrameSink, Vec<u8>, Value)> = {
        let mut st = lock(&inner.st);
        if !st.vis.as_ref().is_some_and(|v| Visualizer::same(v, vis)) {
            return;
        }
        if !st.subs.values().any(|s| s.active) {
            return;
        }
        st.failures = 0;
        let mut jobs = Vec::new();
        for (id, s) in st.subs.iter_mut() {
            if !s.active || now - s.last_t < s.interval {
                continue;
            }
            let frame = resample(values, s.bars);
            if s.last.as_deref() == Some(frame.as_slice()) {
                s.suppressed += 1;
                continue;
            }
            let payload = if s.b64 {
                json!({"vis": base64::engine::general_purpose::STANDARD.encode(&frame), "enc": "b64"})
            } else {
                json!({"vis": frame})
            };
            jobs.push((*id, s.sink.clone(), frame, payload));
        }
        jobs
    };
    if jobs.is_empty() {
        return;
    }
    // Sinks never block, but run them without the lock so a connection
    // closing concurrently can never wait on the frame producer.
    let results: Vec<(u64, Vec<u8>, bool)> = jobs
        .into_iter()
        .map(|(id, sink, frame, payload)| (id, frame, sink(payload)))
        .collect();
    let mut st = lock(&inner.st);
    for (id, frame, ok) in results {
        if let Some(s) = st.subs.get_mut(&id) {
            if ok {
                s.last = Some(frame);
                s.last_t = now;
                s.pushed += 1;
            } else {
                s.dropped += 1;
            }
        }
    }
}

/// CAVA ended. If it was not stopped on purpose and subscribers remain,
/// retry with backoff.
fn on_exit(inner: &Arc<Inner>, vis: &Visualizer) {
    let mut st = lock(&inner.st);
    if st.vis.as_ref().is_some_and(|v| Visualizer::same(v, vis)) {
        schedule_restart_locked(inner, &mut st);
    }
}
