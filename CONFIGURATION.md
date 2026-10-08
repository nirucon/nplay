# NPLAY configuration

NPLAY stores normal configuration in `~/.config/nplay/config.ini` and Navidrome credentials in `~/.config/nplay/secrets.ini` with mode `0600`. Existing files are preserved by upgrades.

## Sources

All source switches default to enabled. Change them live in **Browse → Settings → Sources**.

- `navidrome_enabled = true` — show/use Navidrome. Turning it off does **not** delete server credentials.
- `sr_enabled = true` — show/use Sveriges Radio live, programmes and episodes.
- `youtube_enabled = true` — show/use YouTube audio via yt-dlp.
- `custom_radio_enabled = true` — show/use user-defined radio streams.

Disabled sources disappear from Browse and are excluded from global Quick Play/Search.

## Local music

`music_dirs` defaults to `~/Music`. Multiple roots are separated with `:` on Linux. Scanning is recursive, ignores hidden directories, reads common audio tags, detects common folder-cover names and extracts embedded artwork without modifying the audio file.

Change roots in **Settings → Library → Music folders**, then rescan with `u` or **Rescan library**.

## Appearance

`theme = niru-noir` is the default. Niru Noir follows the terminal foreground/background instead of painting an opaque background. `t` changes theme live. Other built-ins are Satie, C. Larsson, Hackerman, Commodore 64 and Omarchy-follow mode.

`now_playing_view` can be `normal`, `artwork` or `visualizer`. `kitty_artwork = true` enables Kitty graphics when supported. Other terminals use the text/media fallback.

## Local library persistence

`music_dirs` is a colon-separated list of roots and defaults to `~/Music`. Scans recurse into subdirectories. `auto_library_refresh = true` performs an incremental background refresh at startup; unchanged files are skipped using their modification time.

The local index, playlists, favorites, history, resume positions and custom radio stations are stored in the XDG data directory (`~/.local/share/nplay/library.db` by default). Application upgrades must not replace this database.

NPLAY 0.4.3 introduces database schema version 2. The migration is additive and creates a one-time `library.pre-0.4.3.db` safety copy before altering an older tracks table.

## Session state

The XDG state directory stores the current resume candidate, playback position, volume, queue and playback context. NPLAY does not autoplay on startup. Navidrome authenticated media URLs are not written into new session state; they are regenerated when needed.


## Playback quality (0.5.1)

NPLAY enables ReplayGain-based volume normalization by default. Audio files/streams are never modified. `normalization_mode = auto` uses album gain when the active playback context is a single album and track gain for mixed contexts, random play and playlists. If ReplayGain metadata is absent, mpv leaves the programme level unchanged. Peak protection is enabled by default and preamp defaults to 0 dB.

```ini
normalization = true
normalization_mode = auto
replaygain_preamp = 0
replaygain_clip = true
gapless = false
shuffle_mode = off
repeat_mode = off
```

Gapless is deliberately **off by default**. Enabling it passes mpv's gapless audio mode to playback. It is most useful for local/Navidrome albums; remote-provider latency can still affect transitions because the next remote item may need resolving.

Shuffle modes are Off, Shuffle and Smart. Smart Shuffle uses recent history and artist separation to reduce immediate repeats. Repeat modes are Off, Track and Context. Settings are available live under **Settings → Playback**.


## Discovery / sorting

`youtube_sort` defaults to `relevance`. Supported runtime sort modes for dated result lists are `relevance`, `newest`, and `oldest`; use `S` to cycle without another network request.

## Spotify (optional, Premium)

Spotify is disabled by default. NPLAY uses Spotify's official Web API and Spotify Connect. It does not extract Spotify audio and does not require a client secret.

1. Create an app in the Spotify Developer Dashboard and enable Web API.
2. Add this exact Redirect URI to the app: `http://127.0.0.1:43821/callback`
3. In NPLAY open `Settings → Sources`, enable Spotify, then choose `Configure Spotify`.
4. Paste the app's **Client ID**. NPLAY opens the browser for authorization and receives the callback on the loopback interface only.
5. Open Spotify on at least one device. `Spotify → Devices` can select an available Spotify Connect target.

The Client ID is stored in `config.ini`. OAuth access/refresh tokens are stored in `secrets.ini`, which NPLAY writes with mode `0600`. NPLAY never asks for or stores your Spotify password or a Spotify client secret.

Relevant config keys:

```ini
[general]
spotify_enabled = false
spotify_client_id =
```

Spotify Development Mode has account/app limits controlled by Spotify. A Premium account is required. If Spotify changes API availability or scopes, NPLAY reports the provider error without affecting Local/Navidrome/SR/YouTube.


## Spotify local authorization persistence (1.1.3)
Local librespot authorization is a one-time step under normal operation. Credentials are stored in NPLAY's persistent XDG data directory and are reused across NPLAY restarts and code upgrades. Reauthorization is only expected if credentials are removed/revoked, Spotify invalidates them, or the user explicitly disconnects/resets Spotify.


## Large local libraries (1.1.3)

`music_dirs` may point directly at a symlinked library root such as `~/Music/mp3`; NPLAY resolves and follows that configured root. Nested symlink directories are not followed by default, preventing loops and accidental traversal of unrelated disks.

`music_excludes` is a colon-separated set of names/globs ignored by the scanner. The default includes `Reaper-projects`. Example: `Reaper-projects:stems:exports`.

`library_scan_batch` defaults to `250`. Scans write metadata in short WAL transactions so playback state, history and playlists remain responsive even with very large libraries. Missing/offline roots are retained in the index rather than interpreted as deleted media.

## Desktop track notifications (1.1.3)

Linux desktop notifications are enabled by default. `track_notifications = false` disables them and `notification_artwork = false` keeps text notifications while suppressing cover art. NPLAY uses the freedesktop notification stack through `notify-send`; playback remains fully functional if no notification daemon is available. Configure or test this under Settings → Playback.

## NPLAY 1.0 settings

`mpris_enabled=true` enables the optional Linux MPRIS2 service. If D-Bus Python/GLib bindings are unavailable NPLAY starts normally and Diagnostics reports MPRIS as unavailable.

`preferred_source=local` controls which source is surfaced first when Universal Quick Find identifies the same artist/title across multiple providers. Provider-specific results are always retained.
