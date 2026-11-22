//! CAVA spectrum source (port of `nplay/visualizer.py`).
//!
//! One persistent `cava -p <config>` process in raw 8-bit mono output mode at
//! 30 fps. The config lives in NPLAY's private 0700 runtime directory (0600,
//! never shared /tmp) and is removed as soon as CAVA ends or is stopped. A
//! reader thread blocks on CAVA's stdout and runs the frame callback for every
//! frame, so a push consumer needs no timer; nothing runs while stopped.
//!
//! Public API (`Visualizer` is `Clone + Send + Sync`; clones share one state):
//! - `Visualizer::new(bars, on_frame, on_exit)`; `on_frame(&vis, &[u8])` runs
//!   on the reader thread per frame, `on_exit(&vis)` when CAVA ends (also after
//!   `stop()`; compare with `same()` to ignore a replaced instance).
//! - `set_binary(path)`: CAVA executable (default `cava` on `$PATH`).
//! - `available()`: CAVA is installed.
//! - `start() -> bool`: start (true if already running); `start_with(bars)`
//!   changes the bar count first when not running.
//! - `stop()`: SIGTERM (then SIGKILL after 0.3 s), reap, remove the config.
//! - `running()`, `bars()`, `values()` (latest frame), `get(n)` (latest frame
//!   resampled to `n` bars, as 1.x), `pid()`, `config_path()`.
//! - `same(a, b)`: identity of two handles (the service's `vis is self._vis`).
//! - `resample(values, n)`: the shared nearest-neighbour resampler.
//! - `config_text(bars)`: the exact CAVA config 1.x writes.
//!
//! Restart with backoff is the service's job: it gets `on_exit` and decides.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type FrameCallback = Arc<dyn Fn(&Visualizer, &[u8]) + Send + Sync>;
pub type ExitCallback = Arc<dyn Fn(&Visualizer) + Send + Sync>;

struct State {
    bars: usize,
    values: Vec<u8>,
    proc: Option<Child>,
    path: Option<PathBuf>,
    running: bool,
    /// Bumped per start so a stale reader cannot clear a newer process.
    run: u64,
}

struct Inner {
    state: Mutex<State>,
    on_frame: Option<FrameCallback>,
    on_exit: Option<ExitCallback>,
    binary: Mutex<String>,
}

#[derive(Clone)]
pub struct Visualizer {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The CAVA configuration (identical to 1.x).
pub fn config_text(bars: usize) -> String {
    format!(
        "[general]\nbars = {bars}\nframerate = 30\n[output]\nmethod = raw\nraw_target = /dev/stdout\ndata_format = binary\nbit_format = 8bit\nchannels = mono\n[smoothing]\nnoise_reduction = 77\n"
    )
}

/// Nearest-neighbour resampling of a frame to `n` bars.
pub fn resample(vals: &[u8], n: usize) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    if vals.is_empty() {
        return vec![0; n];
    }
    if vals.len() == n {
        return vals.to_vec();
    }
    let m = vals.len();
    (0..n).map(|i| vals[(i * m / n).min(m - 1)]).collect()
}

fn write_config(bars: usize) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = crate::paths::private_runtime_dir();
    for _ in 0..32 {
        let p = dir.join(format!("cava-{:08x}.conf", fastrand::u32(..)));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&p)
        {
            Ok(mut f) => {
                f.write_all(config_text(bars).as_bytes())?;
                return Ok(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free config name"))
}

fn terminate(mut c: Child) {
    unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_millis(300);
    loop {
        if !matches!(c.try_wait(), Ok(None)) {
            return;
        }
        if Instant::now() >= end {
            let _ = c.kill();
            let _ = c.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Visualizer {
    pub fn new(
        bars: usize,
        on_frame: Option<FrameCallback>,
        on_exit: Option<ExitCallback>,
    ) -> Visualizer {
        let bars = bars.max(1);
        Visualizer {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    bars,
                    values: vec![0; bars],
                    proc: None,
                    path: None,
                    running: false,
                    run: 0,
                }),
                on_frame,
                on_exit,
                binary: Mutex::new("cava".into()),
            }),
        }
    }

    pub fn same(a: &Visualizer, b: &Visualizer) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }

    pub fn set_binary(&self, path: impl Into<String>) {
        *lock(&self.inner.binary) = path.into();
    }

    fn binary(&self) -> Option<PathBuf> {
        crate::paths::which(&lock(&self.inner.binary))
    }

    pub fn available(&self) -> bool {
        self.binary().is_some()
    }

    pub fn running(&self) -> bool {
        lock(&self.inner.state).running
    }

    pub fn bars(&self) -> usize {
        lock(&self.inner.state).bars
    }

    pub fn values(&self) -> Vec<u8> {
        lock(&self.inner.state).values.clone()
    }

    pub fn get(&self, n: usize) -> Vec<u8> {
        resample(&lock(&self.inner.state).values, n)
    }

    pub fn pid(&self) -> Option<u32> {
        lock(&self.inner.state).proc.as_ref().map(|c| c.id())
    }

    pub fn config_path(&self) -> Option<PathBuf> {
        lock(&self.inner.state).path.clone()
    }

    pub fn start_with(&self, bars: usize) -> bool {
        {
            let mut st = lock(&self.inner.state);
            if !st.running && bars > 0 && st.bars != bars {
                st.bars = bars;
                st.values = vec![0; bars];
            }
        }
        self.start()
    }

    pub fn start(&self) -> bool {
        let Some(bin) = self.binary() else {
            return false;
        };
        let mut st = lock(&self.inner.state);
        if st.running && st.proc.is_some() {
            return true;
        }
        // Reap a CAVA that ended by itself.
        if let Some(mut old) = st.proc.take() {
            if matches!(old.try_wait(), Ok(None)) {
                drop(st);
                terminate(old);
                st = lock(&self.inner.state);
            }
        }
        if let Some(p) = st.path.take() {
            let _ = std::fs::remove_file(p);
        }
        let path = match write_config(st.bars) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let spawned = Command::new(bin)
            .arg("-p")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                st.running = false;
                return false;
            }
        };
        let Some(out) = child.stdout.take() else {
            drop(st);
            terminate(child);
            let _ = std::fs::remove_file(&path);
            return false;
        };
        st.run += 1;
        let run = st.run;
        let bars = st.bars;
        st.proc = Some(child);
        st.path = Some(path);
        st.running = true;
        drop(st);
        let me = self.clone();
        let ok = std::thread::Builder::new()
            .name("nplay-cava".into())
            .spawn(move || me.read(out, run, bars))
            .is_ok();
        if !ok {
            self.stop();
        }
        ok
    }

    fn read(&self, mut out: std::process::ChildStdout, run: u64, bars: usize) {
        let mut frame = vec![0u8; bars];
        loop {
            if !self.running() {
                break;
            }
            if out.read_exact(&mut frame).is_err() {
                break;
            }
            {
                let mut st = lock(&self.inner.state);
                if st.run != run || !st.running {
                    break;
                }
                st.values.clear();
                st.values.extend_from_slice(&frame);
            }
            if let Some(cb) = &self.inner.on_frame {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(self, &frame)));
            }
        }
        drop(out);
        let ended = {
            let mut st = lock(&self.inner.state);
            if st.run == run {
                st.running = false;
                // CAVA read its config at start-up; remove it once CAVA ends.
                if let Some(p) = st.path.take() {
                    let _ = std::fs::remove_file(p);
                }
                st.proc.take()
            } else {
                None
            }
        };
        if let Some(c) = ended {
            terminate(c);
        }
        if let Some(cb) = &self.inner.on_exit {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(self)));
        }
    }

    pub fn stop(&self) {
        let (proc, path) = {
            let mut st = lock(&self.inner.state);
            st.running = false;
            (st.proc.take(), st.path.take())
        };
        if let Some(c) = proc {
            terminate(c);
        }
        if let Some(p) = path {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fake_cava(name: &str, body: &str) -> Option<PathBuf> {
        crate::paths::which("python3")?;
        let d = std::env::temp_dir().join(format!(
            "nplay-vis-{name}-{}-{:x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&d).ok()?;
        let p = d.join("cava");
        std::fs::write(&p, body).ok()?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).ok()?;
        Some(p)
    }

    /// Same behaviour as tests/fake_cava.py: varying frames, then silence.
    const FAKE: &str = r#"#!/usr/bin/env python3
import re, sys, time, os
cfg = sys.argv[sys.argv.index("-p") + 1]
text = open(cfg).read()
bars = int(re.search(r"bars\s*=\s*(\d+)", text).group(1))
mode = oct(os.stat(cfg).st_mode & 0o777)
sys.stderr.write(mode)
out = sys.stdout.buffer
n = 0
limit = int(os.environ.get("FAKE_CAVA_LIMIT", "0"))
try:
    while True:
        out.write(bytes(((i * 7 + n * 13) % 250) + 1 for i in range(bars)) if n < 15 else bytes(bars))
        out.flush()
        n += 1
        if limit and n >= limit:
            break
        time.sleep(1 / 30)
except (BrokenPipeError, KeyboardInterrupt):
    pass
"#;

    #[test]
    fn config_and_resample() {
        let c = config_text(48);
        assert!(c.contains("bars = 48\nframerate = 30"));
        assert!(c.contains("method = raw") && c.contains("bit_format = 8bit"));
        assert_eq!(resample(&[1, 2, 3, 4], 2), vec![1, 3]);
        assert_eq!(resample(&[1, 2], 4), vec![1, 1, 2, 2]);
        assert_eq!(resample(&[], 3), vec![0, 0, 0]);
        assert!(resample(&[1], 0).is_empty());
    }

    #[test]
    fn missing_cava_does_not_start() {
        let v = Visualizer::new(16, None, None);
        v.set_binary("/nonexistent/cava");
        assert!(!v.available());
        assert!(!v.start());
        assert!(!v.running());
    }

    #[test]
    fn frames_flow_and_config_is_private_and_removed() {
        let Some(bin) = fake_cava("flow", FAKE) else {
            return;
        };
        let frames = Arc::new(AtomicUsize::new(0));
        let exits = Arc::new(AtomicUsize::new(0));
        let (f2, e2) = (frames.clone(), exits.clone());
        let v = Visualizer::new(
            8,
            Some(Arc::new(move |_v: &Visualizer, vals: &[u8]| {
                assert_eq!(vals.len(), 24);
                f2.fetch_add(1, Ordering::SeqCst);
            })),
            Some(Arc::new(move |_v: &Visualizer| {
                e2.fetch_add(1, Ordering::SeqCst);
            })),
        );
        v.set_binary(bin.to_string_lossy());
        assert!(v.start_with(24));
        assert!(v.start(), "second start is a no-op");
        let cfg = v.config_path().unwrap();
        assert!(cfg.starts_with(crate::paths::private_runtime_dir()));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let end = Instant::now() + Duration::from_secs(5);
        while frames.load(Ordering::SeqCst) < 5 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(frames.load(Ordering::SeqCst) >= 5);
        assert!(v.values().iter().any(|x| *x > 0));
        assert_eq!(v.get(12).len(), 12);
        let pid = v.pid().unwrap();
        v.stop();
        assert!(!v.running());
        assert!(!cfg.exists());
        assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0, "CAVA reaped");
        let end = Instant::now() + Duration::from_secs(2);
        while exits.load(Ordering::SeqCst) == 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(exits.load(Ordering::SeqCst), 1);
        // Restart works on the same handle.
        assert!(v.start());
        assert!(v.running());
        v.stop();
        let _ = std::fs::remove_dir_all(bin.parent().unwrap());
    }

    #[test]
    fn unexpected_exit_notifies_and_cleans_up() {
        let body = FAKE.replace("limit = int(os.environ.get(\"FAKE_CAVA_LIMIT\", \"0\"))", "limit = 3");
        let Some(bin) = fake_cava("exit", &body) else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let v = Visualizer::new(
            16,
            None,
            Some(Arc::new(move |vis: &Visualizer| {
                let _ = lock(&tx).send(vis.running());
            })),
        );
        v.set_binary(bin.to_string_lossy());
        assert!(v.start());
        let cfg = v.config_path().unwrap();
        let running_at_exit = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!running_at_exit);
        assert!(!v.running());
        assert!(!cfg.exists());
        assert!(v.pid().is_none(), "exited CAVA is reaped");
        let _ = std::fs::remove_dir_all(bin.parent().unwrap());
    }

    #[test]
    fn niruamp_fake_cava_if_present() {
        let p = PathBuf::from("/media/usbdata/projects/niruamp/tools/fake-cava");
        let alt = crate::paths::home_dir().join("Git/niruamp/tools/fake-cava");
        let Some(bin) = [p, alt].into_iter().find(|p| p.exists()) else {
            return;
        };
        if crate::paths::which("python3").is_none() {
            return;
        }
        let v = Visualizer::new(32, None, None);
        v.set_binary(bin.to_string_lossy());
        assert!(v.start());
        let end = Instant::now() + Duration::from_secs(3);
        while v.values().iter().all(|x| *x == 0) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(v.values().iter().any(|x| *x > 0));
        v.stop();
    }
}
