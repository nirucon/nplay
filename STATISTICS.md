> NPLAY 1.5.5: see [STATISTICS-SYNC-1.5.5.md](STATISTICS-SYNC-1.5.5.md) for the verified website API v3 contract and opt-in HTTPS synchronization. Earlier API v2 discussion below is historical and not the active protocol.

# NPLAY Statistics 1.5.5

## Architecture

Local-first, offline by design. The `nplay/statistics/` package is separate from the existing `nplay/db.py` playback-history database:

- `tracker.py` observes confirmed active playback on the existing TUI loop, every ~2 seconds.
- `store.py` writes short actual-listening intervals to `~/.local/share/nplay/statistics.sqlite3` (or `$XDG_DATA_HOME/nplay/statistics.sqlite3`).
- Every interval has an independent immutable UUID `event_id`; a unique SQLite primary key prevents duplicate insertion.
- `sync_outbox` records every event once, in the same SQLite transaction.
- `identity.installation_id` is a randomly generated UUID retained across upgrades.
- Statistics display uses the current local day/week (Monday–Sunday)/month/year or all-time, based on local time.
- No external network requests, timers, HTTP client or sync worker are implemented.

### Accuracy boundaries

This measures **observed active wall-clock listening**, not track metadata duration, seek distance or play-start counts. Pauses and inactive periods are not counted. Gaps greater than five seconds (e.g. laptop suspend) are discarded. The first sample of each resumed session is a baseline, so small portions of audio around start/pause may be omitted. In an offline-first terminal app, these conservative estimates are preferable to overcounting. Spotify active state is derived from NPLAY's polled Spotify playback state; the poll can lag real activity. The tracker only observes while the NPLAY TUI event loop is running. Time intervals are attributed to their start timestamp; a two-second sample that crosses midnight belongs to the starting day. Older `play_events` are **not** automatically imported because their semantics differ and merging could double-count.

Top rankings are by **actual seconds listened**; a session is a contiguous active period of a track. Albums are grouped by artist and album. Track rankings group by artist and title (which can merge different recordings with the same title); source-specific IDs are stored for future refinements. Live radio is counted by station/program metadata available in NPLAY, not by unidentified individual songs. For custom radio streams `source=radio`; Sveriges Radio stations and podcasts may have distinct source values depending on the provider.

### Privacy

Statistics are enabled by default, can be turned off in the Statistics view, and existing data remains intact when disabled. The SQLite file is created with user-only `0600` permissions. No listening data or installation ID leaves the machine. Backup/restore of the data directory retains the same installation ID: to create a genuinely new client identity, use a fresh data directory, not a clone of the database.

## API v2: integration gate (NOT VERIFIED)

The n.rudolfsson.net 0.6.3 Statistics API v2 source/schema was **not supplied with the NPLAY 1.4.2 ZIP** and could not be verified. The client therefore **does not send any data** and has no endpoint, token or HTTP configuration. The local outbox is intentionally durable but inactive.

To enable integration, the server must document and confirm:

1. Exact HTTPS endpoint(s), method, request/response schema and API version negotiation.
2. Auth mechanism and token scopes; credentials in a separate `0600` secrets file, never embedded in event data.
3. Client identity registration and ownership; one `installation_id` per installation.
4. Idempotency key `(installation_id,event_id)` with server-side uniqueness; duplicates return an acknowledged success.
5. UTC timestamp format, fractional listening seconds, supported sources and metadata limits.
6. Batch limits, per-event partial acknowledgments, retryable vs permanent failures and rate limits.
7. Revocation, deletion/export and privacy handling, plus how clients aggregate across multiple machines.
8. Explicit opt-in to sync, independently of the local collection toggle.

### Suggested transport-neutral event model (PROPOSAL ONLY)

```json
{
  "api_version": 2,
  "installation_id": "uuid",
  "events": [{
    "event_id": "uuid",
    "session_id": "uuid",
    "start_utc": "2026-10-09T19:00:00Z",
    "seconds": 2.0,
    "source": "navidrome",
    "track_id": "source-stable-id",
    "title": "Track",
    "artist": "Artist",
    "album": "Album"
  }]
}
```

**This JSON is not a claim about the actual server contract.** A future adapter can translate stored events into the verified server representation, and should only mark rows `acked` after verified idempotent acknowledgment. Third-party services can implement separate adapters without changes to the statistics core.

## Testing

`python3 -m unittest discover -s tests -v` tests real elapsed time, pause/resume, disabled mode, five sources, idempotent event insertion, outbox acknowledgment, identity persistence, file permissions, and period filtering.

## Upgrade / rollback

No changes to existing `nplay/db.py` schema. The new SQLite file is additive and independent. Rollback to NPLAY 1.4.2 leaves the file untouched; upgrading again resumes existing statistics. No automatic migration or deletion of listening data.

## 1.5.5 Statistics TUI

Open Browse → Statistics. The overview shows actual listening time, period selectors and three visible entries each for artists, albums and tracks, with durations in the labels (not hidden in selection-only details). Open the individual Top Artists, Top Albums, Top Tracks or Sources menus to view up to ten entries. Settings & Sync contains the local collection toggle, offline-only status, outbox count and installation UUID. Copy Installation ID uses `wl-copy` (Wayland), `xclip` or `xsel` when available. No clipboard dependency is mandatory.

## 1.5.5: provider-independent integration

The statistics engine is independent of n.rudolfsson.net and has **no hardcoded server address**. `StatisticsStore.pending_events(limit)` exposes pending immutable event records; `StatisticsStore.acknowledge(event_ids)` is available only for use **after a future verified idempotent server acknowledgment**. See `nplay/statistics/adapter.py` for a transport-neutral canonical envelope. It does not send anything and is not the website's verified API v2 contract.

To integrate a different server, implement a separate adapter that converts this canonical envelope to the destination API's schema, securely authenticates the client, handles retries, and acknowledges only event IDs explicitly confirmed by that server. Preserve `(installation_id, event_id)` as a unique pair at the receiving end. A provider-specific sync toggle and opt-in must be added before any network transmission. Multiple clients may contribute to the same account, but their installation IDs must remain distinct.

The TUI statistics screen now refreshes at most once per ten seconds while open, preserving selection and avoiding navigation-stack growth. Listening collection remains independent of whether the Statistics screen is open.
