# NPLAY 1.6.0 — installation

NPLAY is a custom application made for Nicklas Rudolfsson, but is fully functional for other users.

## Supported clean-install targets

- Arch Linux
- Omarchy and Arch-derived systems
- Debian
- Ubuntu and Debian-derived systems

Run:

```sh
unzip NPLAY-1.6.0.zip
cd NPLAY-1.6.0
./install.sh
```

The installer detects `pacman` or `apt`, shows missing dependencies, asks before using `sudo`, and installs the required/recommended runtime packages when available. Core playback requires Python 3 and mpv. CAVA enables visualization, yt-dlp enables YouTube audio, and Mutagen enables robust local metadata and embedded artwork.

The installer performs an atomic application-code replacement. Existing configuration, secrets, local library database, playlists, favorites, history and state under XDG config/data/state directories are not replaced.

librespot is optional. If it is unavailable, Spotify can use an external Spotify Connect device, including a separately managed `go-librespot` receiver. NPLAY's managed Local Playback mode still requires a librespot binary with a working audio backend.

After installation:

```sh
nplay --doctor
nplay
```

Kitty is not required. In Kitty, NPLAY can render image artwork through `kitten icat`. In other terminals the application remains usable and substitutes a compact source placeholder instead of leaving broken image space.

## Upgrade data safety

The installer replaces only the application code. Configuration, XDG state and `library.db` are preserved. This includes indexed Local Music, playlists, favorites, history and custom radio stations. The existing additive database migrations remain available when upgrading from older schemas; this UI-only release does not change the database schema.


## Privacy of logs and runtime files (1.8.0)

NPLAY 1.8.0 keeps credentials out of process arguments and logs. mpv receives
media URLs over its private IPC socket instead of on its command line, every
log line and mpv's own log are redacted (`t`, `s`, `p`, `token`, `apiKey`,
`access_token` query values become `REDACTED`), log files are created `0600`
and the state directory `0700`. On the first start after the upgrade (and at
every later start, idempotently) NPLAY redacts existing `nplay.log`, its
rotations and `mpv.log` in place; no unredacted copy is kept. The mpv IPC
socket and the CAVA config live in `$XDG_RUNTIME_DIR/nplay` (`0700`) instead
of `/tmp`, and leftover `/tmp/nplay-cava-*.conf` files are removed.
`nplay --doctor` reports each of these as `privacy … OK|WARN` with a one-line fix.

A Subsonic token that was written to an older log can be replayed until the
Navidrome password changes. If such logs were ever copied elsewhere, change the
Navidrome password.

## Spotify local authorization persistence (1.1.3)
Local librespot authorization is a one-time step under normal operation. Credentials are stored in NPLAY's persistent XDG data directory and are reused across NPLAY restarts and code upgrades. Reauthorization is only expected if credentials are removed/revoked, Spotify invalidates them, or the user explicitly disconnects/resets Spotify.
