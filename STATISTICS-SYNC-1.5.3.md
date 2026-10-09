# NPLAY 1.5.5 — statistics sync, verified server contract

Verified against the supplied `n.rudolfsson.net-0.6.4-production-candidate(1).zip`:
- `site/app/api.php`
- `site/app/domain/statistics.php`
- `site/RELEASE-0.6.4.md`

**Production deployment and live database behavior are not verified.** No requests were sent to the production server. A staging token and a disposable installation should be used for a first live integration test.

## Exact website API contract

`POST https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3`

Headers: `Authorization: Bearer <dedicated token>` and `Content-Type: application/json`. The token must have **only** the `statistics:music` scope. It is bound to one `installation_id` UUID on the first successful batch.

Payload uses `nplay.listening-events`, `format_version: 1`, `installation_id`, and 1–50 events. Each event has `event_id` and `session_id` UUIDs, `started_at` as ISO-8601 with timezone, numeric `listened_seconds` (>0 and <=5.5), `source`, `source_track_id`, `title`, `artist`, and `album`.

Success HTTP 200: `{ "ok": true, "format_version": 1, "acknowledged_event_ids": ["uuid", ...] }`. Only acknowledged IDs are marked as `acked` locally; records are never deleted. Server deduplicates on `(installation_id,event_id)`. The client accepts partial acknowledgments but keeps all other IDs pending. 401/403, 413, 422 and 503 are handled without acknowledging anything. Server accepts only events from the previous 365 days, with a 5-minute future tolerance.

The website's `statistics_nplay_v3_ingest` validates all records before a single database transaction. No partial commit is expected from the current implementation; the client nonetheless safely handles partial acknowledgments.

## Why not 30-second records yet?

**The actual 0.6.4 server rejects `listened_seconds > 5.5`.** NPLAY 1.5.5 therefore buffers approximately five seconds of observed listening per event, reducing the number of new outbox rows by roughly 2–3x compared with two-second intervals. It flushes pending fractions on pause, stop, track change and normal exit. Longer gaps caused by suspend are excluded. This is a server-compatible improvement, not a claim that 30-second events are accepted.

To support ~30-second events in a future release, update the server's validated maximum and database column contract together, update the client, and test against a staging instance before enabling the new format. Do **not** silently reinterpret or compact the 1.5.2 outbox; the existing event IDs are already durable and must remain immutable for safe retries.

## Install and configure

Close NPLAY before upgrading. `bash install.sh` installs code under `~/.local/lib/nplay`, preserving data under `~/.local/share/nplay`, settings/secrets under `~/.config/nplay`, and logs under `~/.local/state/nplay`. The installer also backs up `statistics.sqlite3` into the upgrade backup directory. No destructive SQLite migration is performed.

1. In website Admin → API, create a **dedicated token per NPLAY installation** with only `statistics:music` selected. Save it when created; it will not be displayed again.
2. In Kitty, run `nplay --stats-set-token`. Enter the token at the hidden prompt. It is stored in `~/.config/nplay/secrets.ini` (mode 0600), not in shell history.
3. Open NPLAY → Statistics → Settings & Sync → **SYNC · ENABLE**. The default is OFFLINE ONLY.
4. Choose **MANUAL SYNC NOW**. Verify that the outbox pending count decreases and that website `/statistik/musik/` shows the expected time. Each request contains at most 50 events.
5. When enabled, NPLAY also attempts a batch asynchronously about once per minute; errors use bounded exponential backoff. Spotify/music playback is not blocked by HTTP requests.

You can run `nplay --stats-sync-now` for a single foreground diagnostic batch after enabling sync. It prints the number of server-acknowledged events, never the token. The server URL is configurable with `statistics_sync_url` in `[general]` of `~/.config/nplay/config.ini`; HTTPS is mandatory. Different services may require an alternate adapter and acknowledgment mapping.

The UI displays collection ON/OFF, privacy state, sync status, server URL, installation UUID, pending count, last success and sanitized error. Last successful sync persists in the existing `identity` key-value table.

## Spotify HTTP 403: Restriction violated

The observed error `Spotify 403 · Player command failed: Restriction violated` is returned by Spotify's Player API. Unlike a missing librespot Connect device, it **cannot be conclusively repaired by restarting librespot**. NPLAY now reports a targeted explanation instead of treating it as a registration timeout. Verify the account is Premium, that the chosen Spotify Connect device is available/usable, and that the same track plays through the official Spotify app. Check Spotify → Local playback → Diagnostics. If Spotify itself rejects the command, NPLAY cannot override the provider's restrictions. No automatic blind retry is performed, to avoid disrupting working playback.

## Privacy and limits

Only listening event metadata and the installation UUID are transmitted, never passwords, music files, local paths or Spotify credentials. TLS certificate and hostname verification are enabled; redirects are rejected so bearer tokens cannot be forwarded to a different endpoint. Each request has a 12-second timeout; background sync runs in a worker thread. Tokens are not logged or displayed. Existing 1.5.2 rows, outbox and installation identity are preserved; no backfill or destructive compaction is performed. SQLite event insertion remains best-effort and cannot interrupt audio playback.

Listening is sampled while the TUI runs; time spent between samples is conservatively excluded when the app is suspended. Events are split at local midnight; the website displays days in Europe/Stockholm. Machines in other timezones may have different local-day boundaries. Events older than one year, legacy records over 5.5 seconds, or malformed metadata may require explicit handling before sync; they are never silently discarded.
