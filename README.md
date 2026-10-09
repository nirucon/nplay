# NPLAY

A minimalist, keyboard-first terminal player for Linux, built around mpv and optional streaming engines. Created by **Ing Leif Nicklas Rudolfsson** and released under the **MIT License**.

## Features

- **Local music:** indexed library, artists, albums, folders, metadata and artwork.
- **Navidrome / Subsonic:** browse and stream music, albums and playlists.
- **Spotify:** Spotify Web API browsing and playback through librespot or external Spotify Connect devices, with local-device recovery and clearer playback restriction diagnostics.
- **YouTube:** search and audio playback with yt-dlp.
- **Sveriges Radio:** live channels and podcasts; custom radio streams are supported too.
- **Playback:** queue, favorites, history, cross-source playlists, bookmarks, repeat/shuffle, resume, ReplayGain and optional gapless playback.
- **Terminal UI:** keyboard navigation, search, commands, Kitty artwork, CAVA visualization, desktop notifications, MPRIS, built-in and external TOML themes.
- **Listening statistics:** actual observed playback time and top artists/albums/tracks/sources for day, week, month, year and all time; SQLite storage and a durable offline queue.
- **Optional statistics sync:** HTTPS adapter for the verified n.rudolfsson.net 0.6.4 API v3, with explicit opt-in, per-installation token, acknowledgment-based delivery and background retries. Offline-only by default.

## Install

On Arch/Omarchy, Debian or Void, extract a [release](https://github.com/nirucon/nplay/releases) and run:

```sh
bash install.sh
nplay --doctor
nplay
```

`mpv` and Python 3 are required; librespot, yt-dlp, CAVA and desktop integrations are optional. User data and secrets are stored separately from application code and preserved on normal upgrades.

## Statistics sync

NPLAY works fully offline. To use your own server, configure its HTTPS URL and implement the documented event/acknowledgment contract. The included adapter targets n.rudolfsson.net API v3; other services can supply separate adapters.

See [STATISTICS.md](STATISTICS.md) for setup, server requirements and privacy safeguards. Tokens are stored in `~/.config/nplay/secrets.ini` and never displayed in the UI.

## Documentation

[Installation](INSTALL.md) · [Keyboard controls](KEYS.md) · [Configuration](CONFIGURATION.md) · [Themes](THEMES.md) · [Statistics and sync](STATISTICS.md) · [Architecture](ARCHITECTURE.md) · [Changelog](CHANGELOG.md)

NPLAY is an independent personal project, shared as open source for others to use and adapt. Third-party integrations depend on external accounts, APIs and playback engines.
