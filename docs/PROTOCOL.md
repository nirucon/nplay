# NPLAY local control protocol (version 1.1)

NPLAY 1.7.0 adds a headless control service for graphical clients such as
NIRUAMP. The terminal player is unchanged and does not require the service, a
GUI toolkit or a display server.

```sh
nplay --serve
nplay --serve --service-socket /path/to/control.sock --idle-exit 25
```

The service prints its endpoint to stdout. `--idle-exit SECONDS` makes a
service that was launched on behalf of a GUI exit if no client remains
connected; a hand-started service defaults to staying alive.

## Transport

- Unix-domain socket, default `$XDG_RUNTIME_DIR/nplay/control.sock` (falling
  back to `$XDG_STATE_HOME/nplay/run/control.sock` or
  `~/.local/state/nplay/run/control.sock`).
- Override with `NPLAY_SERVICE_SOCKET` or `--service-socket`.
- The runtime directory is created with mode `0700`, the socket and lock file
  with mode `0600`. The directory and socket must be owned by the invoking
  user. An advisory lock prevents two services from owning one endpoint.
- Frames are one UTF-8 JSON object per line. Incoming frames are limited to
  1 MiB. Clients must ignore unknown response fields and unknown notifications.
- JSON-RPC 2.0: requests carry `jsonrpc`, `id`, `method`, `params`; responses
  carry the same `id` plus exactly one of `result` or `error`.
- Requests are processed concurrently with bounded per-client outstanding
  work. Slow provider calls cannot block the playback event dispatcher. A slow
  or disconnected client receives a bounded outbound queue; droppable state
  notifications are discarded before responses.

## Handshake

First request:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "client":{"name":"niruamp","version":"0.1.0"},
  "protocol_versions":[1]
}}
```

The result is the full snapshot below plus `client`. A protocol mismatch returns
error `-32010`; `capabilities.messages` lists notification methods.

`state.subscribe` and `state.unsubscribe` control notifications on the calling
connection. The subscribe result is a fresh snapshot.

## Snapshot

```json
{
  "protocol_version": 1,
  "backend": {"name":"nplay","version":"1.7.0","pid":123,"socket":"...","instance":"..."},
  "revision": 42,
  "server_time": 1790000000.0,
  "playback": {
    "status": "idle|loading|playing|paused|stopped",
    "active": false,
    "transport": "mpv|spotify|",
    "source": "local|navidrome|spotify|sr|youtube|radio|",
    "position": 12.5,
    "duration": 240.0,
    "seekable": true,
    "volume": 100.0,
    "muted": false,
    "shuffle": "off|shuffle|smart",
    "repeat": "off|track|context",
    "current": null,
    "context_index": -1,
    "context": [],
    "queue": [],
    "later": [],
    "radio": {},
    "sampled_at": 12345.6
  },
  "sources": [
    {"id":"local","label":"Local Music","capabilities":["search","browse"],"enabled":true,"configured":true}
  ],
  "capabilities": {
    "seek": true, "equalizer": false, "visualization": false,
    "artwork": true, "drag_drop": true, "queue_edit": true,
    "playlists": true, "favorites": true, "ratings": true,
    "spotify": false, "navidrome": false, "youtube": false,
    "cava": false, "mpv": true,
    "backend_version": "1.8.1", "protocol_version": 1, "protocol_minor": 2,
    "visualization_stream": true, "visualization_encodings": ["json","b64"],
    "visualization_max_fps": 30,
    "spotify_local_authorize": true, "spotify_local": true,
    "spotify_local_authorized": false,
    "messages": ["state.changed","library.scanned","spotify.auth",
                 "spotify.local_auth","service.error"],
    "push": ["vis"]
  }
}
```

Track objects are deliberately bounded and redacted:

```json
{"key":"local:abc","source":"local","id":"abc","title":"Song","artist":"Artist",
 "album":"Album","duration":240.0,"kind":"track","seekable":true,"has_art":true,
 "meta":{"track_no":1,"year":2026,"bitrate":320,"codec":"flac"}}
```

They never contain playable URLs, authenticated stream URLs, filesystem paths,
cover URLs, tokens, passwords or arbitrary metadata. To play, add to a
playlist or fetch artwork for a track, clients pass `{"key":"..."}` (or
`source` + `id`) for a track learned from a browse or search result. The service
keeps a bounded reference catalogue; unknown references are rejected.

`position` is sampled when the snapshot is built. Clients may interpolate while
`status == "playing"` and must resynchronise on every `state.changed`.

## Commands

All params are objects. Playback and queue mutations are serialised by the
service.

| Method | Params | Notes |
|---|---|---|
| `ping` | `{}` | Liveness |
| `state.snapshot` | `{}` | Fresh snapshot |
| `state.subscribe` | `{}` | Snapshot + notifications |
| `state.unsubscribe` | `{}` | Stop notifications |
| `playback.play` | `track`, optional `context`, `index`, `preserve_context`, `resume_position` | Context is a list of track references |
| `playback.toggle` / `stop` / `next` / `previous` | `{}` | Same authoritative path as the terminal UI |
| `playback.seek` | `position` or `delta` | Clamped by the transport |
| `playback.volume` | `value` or `delta` | 0–150 |
| `playback.mute` | `{}` | |
| `playback.repeat` | optional `mode`: `off|track|context` | |
| `playback.shuffle` | optional `mode`: `off|shuffle|smart` | |
| `playback.play_external` | `path` or `url`, optional `title`, `queue`, `next` | Dropped local files and http(s) streams; `queue:true` registers and appends without starting playback |
| `queue.list` | `{}` | |
| `queue.add` | `track`, optional `next` | Container rows are rejected |
| `queue.add_many` | `tracks`, optional `next` | Max 1000 |
| `queue.play` / `queue.remove` / `queue.move` | `index`, optional `delta` | |
| `queue.reorder` | `from`, `to` | Move one queue item to an absolute index |
| `queue.clear` | `{}` | |
| `queue.save` | `name` | Saves the authoritative queue as a local playlist |
| `browse` | `source`, `section`, optional `query`, `limit`, `offset`, `args` | See sources below |
| `search` | `query`, optional `sources`, `limit` | Federated, bounded, async on the client |
| `playlists.list` | `{}` | |
| `playlists.tracks` | `id` | |
| `playlists.create` / `rename` / `delete` | `name` / `id` | |
| `playlists.add` | `id`, `track` | Container rows are rejected |
| `playlists.add_many` | `id`, `tracks` | Bulk add up to 5000 playable tracks |
| `playlists.remove` | `id`, `index` | |
| `playlists.move` | `id`, `index`, `delta` | |
| `favorites.list` / `favorites.toggle` | `{}` / `track` | |
| `ratings.get` / `ratings.set` | `track`, optional `rating` 0–5 | |
| `radio.add` / `radio.delete` | `name`,`url` / `id` | |
| `artwork.get` | `track` | Returns `mime` and base64 `data`; empty result uses `empty: true` |
| `visualization.start` / `stop` / `data` | optional `bars` 8–96 | Requires CAVA; no synthetic data. `data` is the 1.0 polling path and stays supported |
| `visualization.subscribe` | optional `bars` 8–96 (32), `fps` 1–30 (30), `encoding` `json`/`b64` (`json`) | 1.1: push frames to this connection, see below |
| `visualization.unsubscribe` | `{}` | 1.1: end the push stream on this connection |
| `visualization.status` | `{}` | 1.1: `running`, `subscribers`, `pushed`, `suppressed`, `dropped`, `polled_recently` |
| `settings.get` | `{}` | Sanitised; never returns secrets |
| `settings.set` | `settings` map | Allowlisted non-secret keys only |
| `navidrome.setup` / `navidrome.clear` | `url`,`user`,`password` / `{}` | Credentials go straight to NPLAY's 0600 secrets file |
| `spotify.connect` / `spotify.disconnect` | `{}` | Connect opens the normal OAuth browser flow in the background |
| `spotify.devices` / `spotify.transfer` | `{}` / `device_id`, `play` | |
| `spotify.local_authorize` | optional `timeout` 10–600 s (180) | 1.2: the terminal's Local playback → AUTHORIZE flow in the background (replaces the local receiver with one started with `--enable-oauth`, opens the consent page with NPLAY's browser candidates); returns `{"started":true}` or `{"started":false,"running":true}`; result in `spotify.local_auth` |
| `library.scan` | optional `force` | Asynchronous; emits `library.scanned` |
| `library.stats` | `{}` | Counts, roots, schema |
| `service.info` | `{}` | Backend version, protocol version/minor, pid, uptime, `visualization` status |
| `service.shutdown` | `{}` | Graceful stop; intended for an owned service |

### Browse sections

- `local`: `all`, `search`, `artists`, `albums`, `folders`, `recent`, `random`,
  `artist` (`args.name`), `album` (`args.artist`, `args.album`), `folder`
  (`args.path`).
- `navidrome`: `newest`, `albums`, `artists`, `playlists`, `random_tracks`,
  `search`, `album`, `artist`, `playlist` (`args.id`).
- `library`: `playlists`, `playlist` (`args.id`), `favorites`, `history`,
  `discoveries`, `bookmarks`.
- `sr`: `live`, `programs`, `search`, `program` (`args.id`).
- `youtube`: `search` (`query` required).
- `spotify`: `playlists`, `playlist`, `album`, `top_tracks`, `top_artists`,
  `recent`, `devices`, `search`.
- `radio`: `all`.

Results are returned as track-shaped rows. Non-track rows use `kind` values
such as `album`, `artist`, `playlist`, `program`, `folder`, `device`; opening
them is another `browse` with the stable IDs in `meta`.

## Notifications

`state.changed` is sent to subscribed clients after playback, queue, source or
capability changes:

```json
{"jsonrpc":"2.0","method":"state.changed","params":{
  "revision":43,"reason":"media-changed",
  "playback":{...},"sources":[...],"capabilities":{...},
  "server_time":1790000001.0
}}
```

`revision` is monotonic. A client that missed events must reconnect or call
`state.snapshot`; it must not infer queue or playback state from event ordering
alone. `library.scanned` and `spotify.auth` carry operation results.

Protocol 1.2 adds two notifications (only on subscribed connections, like the
others; old clients ignore unknown methods):

- `spotify.local_auth` reports `spotify.local_authorize`:
  `{"phase":"browser","authorized":false}` once the consent page was opened,
  then `{"phase":"done","authorized":true,"device":"NPLAY"}` or
  `{"phase":"done","authorized":false,"error":"…"}`.
- `service.error` reports an asynchronous failure that the terminal UI would
  show in its status line, e.g. a Spotify play that fails in a worker after
  `playback.play` already returned: `{"title":"Spotify playback
  unavailable","message":"…"}`. When local Spotify playback is not authorized
  the message is "Local Spotify playback is not authorized yet · choose
  Spotify → Local playback → AUTHORIZE" and the params also carry
  `"code":"spotify_local_unauthorized","remedy":"spotify.local_authorize"`.

Local receiver state (1.2): capabilities `spotify_local_authorize` (the
command exists), `spotify_local` (librespot installed) and
`spotify_local_authorized` (librespot credentials cached); `settings.get`
`spotify` adds `playback` (`local`/`connect`), `local_available`,
`local_authorized`, `local_running` and `local_authorizing`. A client hides
its authorize control when `spotify_local_authorize` is absent (NPLAY ≤ 1.8.0).

## Versioning

`protocol_version` is the major version (still `1`); clients keep sending
`"protocol_versions":[1]`. `protocol_minor` (snapshot, capabilities,
`service.info`, `settings.get` `system`) grows only for additions:

| Minor | NPLAY | Additions |
|---|---|---|
| 1.0 | 1.7.0 | Initial protocol (`protocol_minor` absent means 0) |
| 1.1 | 1.8.0 | `visualization.subscribe` / `unsubscribe` / `status`, capability `visualization_stream` |
| 1.2 | 1.8.1 | `spotify.local_authorize`, notifications `spotify.local_auth` and `service.error`, capabilities `spotify_local_authorize` / `spotify_local` / `spotify_local_authorized`, `settings.get` local receiver state |

## Visualization stream (1.1)

A client chooses the transport from the handshake capabilities:

- `capabilities.visualization_stream == true` and `capabilities.visualization`
  (CAVA installed) → call `visualization.subscribe` once while the spectrum is
  visible and `visualization.unsubscribe` when it is hidden.
- otherwise (NPLAY ≤ 1.7.x) → keep polling `visualization.data`.

```json
{"jsonrpc":"2.0","id":7,"method":"visualization.subscribe","params":{"bars":48,"fps":30,"encoding":"json"}}
{"jsonrpc":"2.0","id":7,"result":{"subscribed":true,"bars":48,"fps":30,"encoding":"json","source":"cava"}}
{"vis":[0,12,40,255, ...]}
{"vis":[0,10,38,250, ...]}
```

- Each pushed frame is one line, **not** a JSON-RPC message: an object with a
  `vis` key and no `jsonrpc`/`id`/`method`. With `encoding:"json"` `vis` is an
  array of `bars` integers 0–255; with `encoding:"b64"` it is the base64 of
  `bars` bytes and the frame also carries `"enc":"b64"`. Clients route a line
  with a `vis` key to the visualizer before JSON-RPC handling.
- The first frame always follows the `subscribe` response. Calling
  `subscribe` again on the same connection replaces `bars`/`fps`/`encoding`.
- Frames come from CAVA's own 30 fps clock, limited to the requested `fps`.
- A frame identical to the previous one sent to that connection is not sent.
  During silence the client receives one all-zero frame and then nothing until
  the audio changes; it must keep showing the last frame (no timeout-to-zero).
- Frames are droppable: a client that does not read gets at most a small
  backlog; responses and notifications are never displaced by frames.
- The stream ends on `visualization.unsubscribe` or disconnect. CAVA stops as
  soon as no connection is subscribed and no `visualization.data` request
  arrived in the last 2 s; a poll-only CAVA stops 15 s after the last request.
- If CAVA exits while a connection is subscribed, the service restarts it with
  backoff (1–30 s); the stream simply resumes.
- `visualization.stop` never ends another connection's stream.

Measured on Python 3.12 (per frame, encoding only): 32 bars `json` 3.8 µs /
125 B, `b64` 2.6 µs / 67 B; 96 bars `json` 7.5 µs / 349 B, `b64` 3.2 µs /
151 B. The old `visualization.data` response cost 4.3–8.0 µs to encode plus a
request parse, dispatch and two thread hand-offs per frame. `b64` is the
cheapest; `json` is the default for simple clients.

End to end (othala, service only, CAVA replaced by a 30 fps fake so the
numbers exclude CAVA's own cost, which is identical in both modes):

| Client | Service CPU | Service wakeups/s |
|---|---|---|
| poll `visualization.data` at 30 Hz (NPLAY 1.7.3 and 1.8.0) | 1.9 % | 137–144 |
| `visualization.subscribe` 30 fps, `json` | 0.7 % | 31 |
| `visualization.subscribe` 30 fps, `b64` | 0.6 % | 31 |
| idle, no client (1.7.3 → 1.8.0) | 0.05 % → 0.0 % | 4.3–6.6 → 0.0 |

During silence a subscribed connection costs only CAVA's frame reads; nothing
is sent.

## Errors

Standard JSON-RPC codes are used: `-32600` malformed request, `-32601` unknown
method, `-32602` invalid params, `-32603` internal error. Service codes:
`-32000` backend unavailable/provider error, `-32005` too many outstanding
requests, `-32010` incompatible protocol. Error `data` may include a bounded
`detail` or `remedy`; secrets and stream URLs are never included.

## Lifecycle and ownership

- One service owns one user endpoint and one authoritative queue, playback
  session, MPRIS identity and statistics pipeline.
- A second service started against the same endpoint acquires no lock, leaves
  the live endpoint untouched and exits successfully.
- A stale socket owned by the current user is replaced only after acquiring the
  lock and verifying that no process answers.
- A client that did not start the service must not call `service.shutdown`.
- The service never spawns an extra player merely because a GUI connects.
- The local librespot receiver is shared with a terminal NPLAY: whichever
  NPLAY starts it owns it, the other adopts it, and it stops with its owner
  (also on SIGKILL); see ARCHITECTURE.md, "Local receiver lifecycle and
  ownership".
- Artwork fetches are limited to tracks in the reference catalogue and capped
  at 12 MiB; arbitrary filesystem reads are not exposed.

## Minimum versions

- Protocol version: `1` (minor `1` with NPLAY 1.8.0, `2` with 1.8.1)
- NPLAY minimum: `1.7.3`; `1.8.0` for `visualization.subscribe`; `1.8.1` for
  `spotify.local_authorize`
- NIRUAMP minimum: `0.3.2` (0.3.x keeps polling `visualization.data`)
