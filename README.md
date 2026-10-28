# NPLAY

A minimalist, keyboard-first terminal player for Linux, built around mpv and optional streaming engines. Created by **Ing Leif Nicklas Rudolfsson** and released under the **MIT License**.

## Features

- **Local music:** indexed library, artists, albums, folders, metadata and artwork.
- **Navidrome / Subsonic:** browse and stream music, albums and playlists.
- **Spotify:** Spotify Web API browsing and playback through librespot or external Spotify Connect devices, with local-device recovery and clearer playback restriction diagnostics.
- **YouTube:** search and audio playback with yt-dlp.
- **Sveriges Radio:** live channels and podcasts; custom radio streams are supported too.
- **Playback:** queue, favorites, history, cross-source playlists, bookmarks, repeat/shuffle, resume, ReplayGain and optional gapless playback.
- **Terminal UI:** keyboard navigation, search, Unicode-capable prompts, commands, Kitty artwork, CAVA visualization, desktop notifications, MPRIS, built-in and external TOML themes.
- **Listening statistics:** actual observed playback time and top artists/albums/tracks/sources for day, week, month, year and all time; SQLite storage and a durable offline queue.
- **Optional statistics sync:** HTTPS adapter for the verified n.rudolfsson.net 0.6.4 API v3, with explicit opt-in, per-installation token, acknowledgment-based delivery and background retries. Offline-only by default.

## Install

On Arch/Omarchy, Debian/Ubuntu or Void, clone or extract a [release](https://github.com/nirucon/nplay/releases) or tag and run:

```sh
./install.sh
nplay --doctor
nplay
```

`mpv` and Python 3 are required; librespot, yt-dlp, CAVA, Mutagen, MPRIS and desktop notifications are optional or recommended. User data and secrets are stored separately from application code and preserved on normal upgrades.

### Command line

```sh
nplay                     # start the player
nplay "search terms"      # start and search immediately
nplay --doctor            # check dependencies and configuration
nplay --scan              # rescan the local library
nplay --version
nplay --list-themes
nplay --check-theme FILE  # validate a custom TOML theme
nplay --stats-set-token   # store the statistics sync token
nplay --stats-sync-now    # run a manual statistics sync
```

## Statistics sync

NPLAY works fully offline. To use your own server, configure its HTTPS URL and implement the documented event/acknowledgment contract. The included adapter targets n.rudolfsson.net API v3; other services can supply separate adapters.

See [STATISTICS.md](STATISTICS.md) for setup, server requirements and privacy safeguards. Tokens are stored in `~/.config/nplay/secrets.ini` and never displayed in the UI.

## Status

Current release: **1.5.9**, focused on Spotify browser launch reliability on minimal X11 sessions, plus the 1.5.8 mpv playback-startup and Unicode prompt fixes. No configuration, credential or database migrations in recent releases. See [CHANGELOG.md](CHANGELOG.md).

## Documentation

[Installation](INSTALL.md) · [Keyboard controls](KEYS.md) · [Configuration](CONFIGURATION.md) · [Themes](THEMES.md) · [Statistics and sync](STATISTICS.md) · [Architecture](ARCHITECTURE.md) · [Changelog](CHANGELOG.md)

NPLAY is an independent personal project, shared as open source for others to use and adapt. Third-party integrations depend on external accounts, APIs and playback engines.
