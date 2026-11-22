//! Listening statistics store, tracker, adapter and opt-in HTTPS sync. Port
//! of 1.x `nplay/statistics/*`; the database file and the wire format are
//! unchanged.
//!
//! # Public API (all handles are `Clone + Send + Sync`, methods take `&self`)
//! - `StatisticsStore` (`store.rs`): `<data>/statistics.sqlite3`, mode 0600,
//!   tables `identity`, `listening` (CHECK 0 < seconds <= 10), `sync_outbox`.
//!   `new()` / `open(path)`, `installation_id()`, `get_meta(key, default)`,
//!   `set_meta(key, value)`, `add(session, start_utc, seconds, &Track,
//!   event_id) -> event id` (idempotent per id; queues it in the outbox),
//!   `summary(period, now) -> Summary` (period `day|week|month|year|all`),
//!   `pending_events(limit) -> Vec<PendingEvent>` (1..=500, read-only),
//!   `acknowledge(&[ids])`.
//! - `ListeningTracker` (`tracker.rs`): `new(store, enabled)`,
//!   `with_clocks(store, enabled, monotonic, wall)`, `tick(Option<&Track>,
//!   playing)` (call from the player loop; elapsed time > 5 s, e.g. suspend,
//!   is never counted), `flush(playing)`, `reset()`, `enabled()`,
//!   `set_enabled(bool)`. Writes events of at most 5 s, split at local midnight.
//! - `adapter`: `pending_envelope(&store, limit) -> serde_json::Value`
//!   (`{format: "nplay.listening-events", format_version: 1,
//!   installation_id, events: [...]}`), `FORMAT_VERSION`.
//! - `StatisticsSync` (`sync.rs`): `new(store, Config)`, `with_options(store,
//!   Arc<dyn SyncConfig>, transport, clock)`, `once() -> Result<usize,
//!   SyncError>` (what `nplay --stats-sync-now` prints as `Acknowledged: n`,
//!   or `Sync failed: <error>`), `run_async(manual, callback) -> bool`,
//!   `join()`, `enabled()`, `token()`, `url()`, `status()` / `set_status()`
//!   (`DISABLED|IDLE|SYNCING|ERROR`), `last_error()`, `last_success()`,
//!   `next_retry()`, `failures()`. Config keys `statistics_sync_enabled`
//!   (default false) and `statistics_sync_url` (default `DEFAULT_URL`);
//!   token from `Config::statistics_token()`. `https_send` is the real
//!   transport (rustls, 12 s timeout, no redirects, 256 KiB limits).

pub mod adapter;
pub mod store;
pub mod sync;
pub mod time;
pub mod tracker;

pub use adapter::pending_envelope;
pub use store::{PendingEvent, StatisticsStore, Summary};
pub use sync::{StatisticsSync, SyncConfig, SyncError, TransportError, DEFAULT_URL};
pub use tracker::ListeningTracker;

#[cfg(test)]
mod tests;
