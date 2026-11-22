//! Headless JSON-RPC control service (port of `nplay/service/`).
//!
//! Submodules:
//! - `protocol`: constants, `RpcError`, socket path rules, Python-identical
//!   JSON framing (`encode`), envelopes and `parse_request`.
//! - `server`: the Unix-socket server (`ControlServer`, `serve`).
//! - `probe`: one-shot diagnostics query of a running service
//!   (`service_info`, `vis_line`).
//!
//! The playback backend (Python `ServiceBackend`) lives elsewhere; the server
//! only needs the [`Backend`] trait below. Contract for implementors:
//! - `dispatch` handles every method except `state.subscribe`,
//!   `state.unsubscribe`, `service.shutdown`, `visualization.subscribe` and
//!   `visualization.unsubscribe`, which the server routes itself (it calls
//!   `snapshot()` for `state.subscribe`). Unknown methods return
//!   `RpcError::method("method not found")`. Where Python raised
//!   ValueError/KeyError/TypeError, return
//!   `RpcError::invalid_parameters(&detail)`; a panic in `dispatch` is caught
//!   and answered with -32603 "internal error".
//! - `visualization_subscribe` registers `sink` for `client_id` but must not
//!   push frames until `visualization_activate(client_id)` (called once the
//!   subscribe response is on the wire). `sink(frame)` never blocks; it
//!   returns false when the frame was dropped. A frame is an object like
//!   `{"vis":[..]}` or `{"vis":"<b64>","enc":"b64"}`.
//! - `visualization_client_gone(client_id)` runs when a connection closes.
//! - The notify callback (`set_notify`) broadcasts `method` + `params` as a
//!   droppable JSON-RPC notification to every `state.subscribe`d client; it
//!   never blocks and may be called from any thread.
//! - `set_socket_path` tells the backend its endpoint (for `snapshot()`'s
//!   `backend.socket` and `service.info`); `close()` runs once on shutdown.

pub mod backend;
pub mod probe;
pub mod protocol;
pub mod server;

pub use backend::run_service;
pub use protocol::RpcError;
pub use server::{serve, ControlServer};

use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Arc;

/// Broadcast callback: `(method, params)`.
pub type NotifyFn = Arc<dyn Fn(&str, Value) + Send + Sync>;
/// Visualization frame sink for one connection; returns false when dropped.
pub type FrameSink = Arc<dyn Fn(Value) -> bool + Send + Sync>;

/// What the control server needs from the playback backend.
pub trait Backend: Send + Sync {
    /// Full state snapshot (the `state.subscribe` result).
    fn snapshot(&self) -> Value;
    /// Execute one request.
    fn dispatch(&self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError>;
    /// Start a pushed frame stream for `client_id` (inactive until activated).
    fn visualization_subscribe(
        &self,
        params: &Map<String, Value>,
        client_id: u64,
        sink: FrameSink,
    ) -> Result<Value, RpcError>;
    /// The subscribe response is on the wire: frames may flow now.
    fn visualization_activate(&self, client_id: u64);
    fn visualization_unsubscribe(
        &self,
        params: &Map<String, Value>,
        client_id: u64,
    ) -> Result<Value, RpcError>;
    /// A connection closed (drop its stream, stop CAVA when unused).
    fn visualization_client_gone(&self, client_id: u64);
    fn set_notify(&self, notify: NotifyFn);
    fn set_socket_path(&self, path: &Path);
    fn close(&self);
}

/// Serialises tests that measure wakeups against tests that run servers.
#[cfg(test)]
pub(crate) fn test_server_lock() -> &'static std::sync::RwLock<()> {
    static L: std::sync::RwLock<()> = std::sync::RwLock::new(());
    &L
}

/// A small in-memory backend for transport tests.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    pub struct State {
        pub notify: Option<NotifyFn>,
        pub socket: String,
        pub sinks: HashMap<u64, (FrameSink, bool)>,
        pub gone: Vec<u64>,
        pub closed: bool,
        pub gate_open: bool,
        pub blocked: usize,
    }

    #[derive(Default)]
    pub struct FakeBackend {
        pub state: Mutex<State>,
        pub cv: Condvar,
    }

    impl FakeBackend {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub fn open_gate(&self) {
            self.state.lock().unwrap().gate_open = true;
            self.cv.notify_all();
        }

        /// Wait until `n` requests are parked in `block`.
        pub fn wait_blocked(&self, n: usize) {
            let mut st = self.state.lock().unwrap();
            while st.blocked < n {
                st = self.cv.wait(st).unwrap();
            }
        }

        /// The sink of the first active subscriber.
        pub fn active_sink(&self) -> Option<FrameSink> {
            let st = self.state.lock().unwrap();
            st.sinks.values().find(|(_, a)| *a).map(|(s, _)| s.clone())
        }

        pub fn notify(&self, method: &str, params: Value) {
            let n = self.state.lock().unwrap().notify.clone();
            if let Some(n) = n {
                n(method, params);
            }
        }
    }

    impl Backend for FakeBackend {
        fn snapshot(&self) -> Value {
            serde_json::json!({"revision": 1, "socket": self.state.lock().unwrap().socket})
        }

        fn dispatch(&self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
            match method {
                "ping" => Ok(serde_json::json!({"pong": 1.5})),
                "echo" => Ok(Value::Object(params.clone())),
                "block" => {
                    let mut st = self.state.lock().unwrap();
                    st.blocked += 1;
                    self.cv.notify_all();
                    while !st.gate_open {
                        st = self.cv.wait(st).unwrap();
                    }
                    st.blocked -= 1;
                    Ok(serde_json::json!({"ok": true}))
                }
                "bad" => Err(RpcError::invalid_parameters(
                    "bad url http://h/rest?u=me&p=hunter2",
                )),
                "panic" => panic!("fake backend exploded"),
                "notify" => {
                    self.notify("state.changed", Value::Object(params.clone()));
                    Ok(serde_json::json!({"ok": true}))
                }
                "service.info" => {
                    let subs = self.state.lock().unwrap().sinks.len();
                    Ok(serde_json::json!({
                        "protocol_minor": 2,
                        "visualization": {"running": subs > 0, "subscribers": subs, "polled_recently": false}
                    }))
                }
                _ => Err(RpcError::method("method not found")),
            }
        }

        fn visualization_subscribe(
            &self,
            params: &Map<String, Value>,
            client_id: u64,
            sink: FrameSink,
        ) -> Result<Value, RpcError> {
            // A frame offered before activation must never reach the wire
            // ahead of the response: the fake does not push it either.
            let bars = params.get("bars").and_then(Value::as_i64).unwrap_or(32);
            self.state
                .lock()
                .unwrap()
                .sinks
                .insert(client_id, (sink, false));
            Ok(serde_json::json!({"subscribed": true, "bars": bars}))
        }

        fn visualization_activate(&self, client_id: u64) {
            let sink = {
                let mut st = self.state.lock().unwrap();
                match st.sinks.get_mut(&client_id) {
                    Some(s) => {
                        s.1 = true;
                        s.0.clone()
                    }
                    None => return,
                }
            };
            // Immediately push one frame from the activation hook.
            sink(serde_json::json!({"vis": [1, 2, 3]}));
            self.cv.notify_all();
        }

        fn visualization_unsubscribe(
            &self,
            _params: &Map<String, Value>,
            client_id: u64,
        ) -> Result<Value, RpcError> {
            let was = self
                .state
                .lock()
                .unwrap()
                .sinks
                .remove(&client_id)
                .is_some();
            Ok(serde_json::json!({"subscribed": false, "was_subscribed": was}))
        }

        fn visualization_client_gone(&self, client_id: u64) {
            let mut st = self.state.lock().unwrap();
            st.sinks.remove(&client_id);
            st.gone.push(client_id);
            self.cv.notify_all();
        }

        fn set_notify(&self, notify: NotifyFn) {
            self.state.lock().unwrap().notify = Some(notify);
        }

        fn set_socket_path(&self, path: &Path) {
            self.state.lock().unwrap().socket = path.display().to_string();
        }

        fn close(&self) {
            self.state.lock().unwrap().closed = true;
        }
    }
}
