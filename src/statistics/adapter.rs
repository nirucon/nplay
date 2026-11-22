//! Transport-neutral listening envelope, verified against website 0.6.4 API v3.

use super::store::{PendingEvent, StatisticsStore};
use super::time;
use crate::core::Result;
use serde_json::{json, Value};

pub const FORMAT_VERSION: i64 = 1;

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One envelope event (field order as 1.x).
pub fn event_json(row: &PendingEvent) -> Value {
    json!({
        "event_id": row.event_id,
        "session_id": row.session_id,
        "started_at": time::iso_utc(row.start_utc),
        "listened_seconds": row.seconds,
        "source": row.source,
        "source_track_id": cut(&row.track_id, 255),
        "title": cut(&row.title, 255),
        "artist": cut(&row.artist, 255),
        "album": cut(&row.album, 255),
    })
}

/// The envelope plus the rows it was built from.
pub fn pending_envelope_rows(
    store: &StatisticsStore,
    limit: i64,
) -> Result<(Value, Vec<PendingEvent>)> {
    let rows = store.pending_events(limit)?;
    let events: Vec<Value> = rows.iter().map(event_json).collect();
    let v = json!({
        "format": "nplay.listening-events",
        "format_version": FORMAT_VERSION,
        "installation_id": store.installation_id()?,
        "events": events,
    });
    Ok((v, rows))
}

/// Return a JSON batch for an explicit sync adapter. Calling this never
/// sends data, changes queue state, or acknowledges events.
pub fn pending_envelope(store: &StatisticsStore, limit: i64) -> Result<Value> {
    Ok(pending_envelope_rows(store, limit)?.0)
}
