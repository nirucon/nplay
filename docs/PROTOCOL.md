# NPLAY local control protocol (version 1)

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
    "backend_version": "1.7.0", "protocol_version": 1,
    "messages": ["state.changed","library.scanned","spotify.auth"]
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
| `visualization.start` / `stop` / `data` | optional `bars` 8–96 | Requires CAVA; no synthetic data |
| `settings.get` | `{}` | Sanitised; never returns secrets |
| `settings.set` | `settings` map | Allowlisted non-secret keys only |
| `navidrome.setup` / `navidrome.clear` | `url`,`user`,`password` / `{}` | Credentials go straight to NPLAY's 0600 secrets file |
| `spotify.connect` / `spotify.disconnect` | `{}` | Connect opens the normal OAuth browser flow in the background |
| `spotify.devices` / `spotify.transfer` | `{}` / `device_id`, `play` | |
| `library.scan` | optional `force` | Asynchronous; emits `library.scanned` |
| `library.stats` | `{}` | Counts, roots, schema |
| `service.info` | `{}` | Backend version, pid, uptime |
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
- Artwork fetches are limited to tracks in the reference catalogue and capped
  at 12 MiB; arbitrary filesystem reads are not exposed.

## Minimum versions

- Protocol version: `1`
- NPLAY minimum: `1.7.2`
- NIRUAMP minimum: `0.3.1`
