# NPLAY architecture

NPLAY separates terminal UI, playback, providers, local persistence, artwork, themes and visualization.

- `app.py`: orchestration, navigation actions, playback context and providers.
- `ui.py`: curses UI, responsive geometry, key handling and contextual help.
- `player.py`: mpv IPC and end-of-track handling.
- `db.py`: SQLite local library, playlists, favorites, history and radio stations.
- `providers.py`: Navidrome, Sveriges Radio and YouTube adapters.
- `artwork.py`: asynchronous/cached artwork acquisition and Kitty rendering.
- `visualizer.py`: one persistent CAVA raw-output process sampled by the UI.
- `theme.py`: NPLAY palettes and best-effort Omarchy palette discovery.

The UI must remain functional without Kitty, CAVA, yt-dlp or a configured network provider. mpv and Python are core runtime dependencies. Provider failures are contained so they do not terminate the player.

## Spotify boundary (1.1.3)

`nplay/spotify.py` is an optional provider/control boundary. It owns Spotify OAuth PKCE, token refresh, Web API mapping and Spotify Connect commands. Spotify catalog objects are mapped into the existing `Track` model so list rendering, artwork, Quick Find and NPLAY cross-source playlists can reuse established UI paths.

Spotify is intentionally *not* routed through `Player`/mpv. `App` exposes small playback-dispatch helpers so the UI can operate either mpv-backed sources or Spotify Connect without destabilizing the established mpv path. Spotify state is polled in a background thread and interpolated locally for a smooth transport clock; network polling never runs at the UI frame rate.

Spotify OAuth tokens live only in the existing mode-0600 secrets file. PKCE means no client secret is required. The callback listener binds only to `127.0.0.1:43821` and exists only during authorization.

### Spotify local playback handshake (1.1.3)

Local librespot process startup, Spotify Connect registration and Player API activation are distinct states. NPLAY therefore never assumes that a running librespot process is immediately playable. The local path waits for the named device, transfers playback ownership, targets the concrete device ID on playback/transport calls and uses bounded retry/backoff for Spotify's eventually-consistent activation. Background control exceptions are marshalled back to the curses UI instead of reaching stderr.


## Spotify session model (1.1.3)

`librespot` Connect device IDs are ephemeral runtime identifiers. NPLAY persists the user's playback preference and librespot credentials, but never relies on a device ID from a previous process. The local receiver is discovered and rebound before controls are issued; a stale-device response triggers one controlled rediscovery path. Album and playlist playback uses Spotify context URIs with a track offset, allowing Spotify itself to perform gapless/native context advancement while NPLAY synchronizes its context cursor from playback state.

Spotify CAVA visualization listens to the host audio stack through CAVA's normal capture path. The Spotify stream remains owned by librespot and is not passed through mpv, ReplayGain, EQ, or another DSP stage.

## Desktop notification boundary (1.1.3)

`nplay/notifications.py` owns freedesktop notifications. Providers and transport code only signal a confirmed track change through the notifier. The notifier deduplicates by source/track identity, resolves artwork in a background thread and never participates in the critical playback path.

## 1.0 desktop and library model

MPRIS is an optional boundary in `nplay/mpris.py`; absence of dbus-python/GLib never prevents NPLAY from starting. Player state remains authoritative in `App`/`Player`. Ratings, discoveries and listening statistics live in the existing SQLite database and are migrated additively; NPLAY 1.1.4 uses schema 4. NPLAY never writes ratings back into audio tags.

The local scanner remains incremental, WAL-backed and batch-written. `Scan Changes` compares mtimes; `Full Rescan` invalidates scan mtimes and re-reads metadata. Offline roots remain non-destructive.

Universal Quick Find keeps provider results independent but may add a synthetic preferred-source section when normalized artist/title identities match across providers. It does not silently merge or delete provider results.


## Unified playback model (1.1.3)

NPLAY owns queue and authoritative media state across providers. mpv and Spotify are transports, not separate queue models. Playback changes are prepared before the new session is committed; generation guards reject stale asynchronous updates. UI, notifications and MPRIS consume the same current state.

SQLite schema 4 adds rich local technical metadata and `play_events` for actual listening time. Migration is additive and backed up before first schema-4 open. Artwork downloads are centralized through `ArtworkCache`, shared by terminal rendering and desktop notifications, with bounded cache size.

## 1.2 service boundary

`App` remains the compatibility façade for UI/MPRIS/provider integrations, but new orchestration is no longer added directly to the historical controller. `services/search.py` owns concurrent federated search and source-resolution aggregation. `services/radio.py` owns provider-neutral NPLAY Radio candidate generation. Future refactoring should continue this seam-by-seam extraction rather than rewriting verified playback state machines.

## 1.3 modular runtime

1.3 continues the seam-by-seam extraction while deliberately preserving the verified transport implementations.

- `core/playback.py`: stable playback façade consumed by integrations such as MPRIS.
- `core/state.py`: canonical transport-neutral playback snapshot.
- `core/events.py`: small synchronous event bus for media/playback/integration changes.
- `core/sources.py`: provider registry and declared capabilities (`search`, `browse`, `live`, `seek`, `radio-seed`, etc.).
- `core/session.py`: atomic session persistence boundary.
- `core/errors.py`: shared error taxonomy for future provider/transport hardening.
- `navigation.py`: source browsing, provider actions, queue/list navigation and asynchronous source loading extracted from the historical `App` class.
- `ui.py`: curses lifecycle, input, navigation state and commands.
- `ui_render.py`: terminal rendering and responsive Now Playing/list/diagnostics presentation.
- `ui_common.py`: UI-neutral labels/help and clipping helpers.

`App` remains a compatibility façade so existing providers and tested playback paths do not need a flag-day rewrite. New cross-source behavior should prefer `SourceRegistry`, `PlaybackController`, services and events instead of adding provider-specific branches to `App`.

The intended dependency direction is UI/integrations → controller/services → transports/providers/persistence. Provider-specific eventual-consistency state (notably Spotify) remains inside its verified boundary until it can be extracted with equivalent regression coverage.

## Playback startup hardening (1.3.5)

mpv-backed sources use `services/playback_start.py` as a serialized, asynchronous startup boundary. Provider resolution and the mpv `file-loaded` wait happen outside the curses thread. A monotonically increasing playback request id prevents stale work from taking ownership, while a startup lock prevents overlapping mpv launches from stopping a newer request. Stop invalidates pending requests. Spotify OAuth is intentionally excluded from ordinary playback and may only be initiated by the explicit Spotify Local setup flow.
