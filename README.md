# NPLAY

A keyboard-first terminal music, radio and podcast player for Linux. NPLAY brings local music, streaming services, radio and podcasts into a single minimalist interface built around **mpv** and optional source-specific playback engines.

Created by **Ing Leif Nicklas Rudolfsson**. Open source under the MIT License.

## Features

### Music and audio sources
- **Local music:** Recursive indexing of multiple music folders, artists, albums, tracks, folders, recent additions and random playback. Reads tags and embedded or folder artwork without modifying audio files.
- **Navidrome / Subsonic:** Browse and stream your music library, albums and playlists.
- **Spotify:** Search, browse and play via the Spotify Web API and optional NPLAY-managed **librespot** receiver, or use an external Spotify Connect device. Automatic recovery when the local Connect receiver loses registration.
- **YouTube:** Search and play audio using yt-dlp.
- **Sveriges Radio:** Live radio, channel discovery and podcasts.
- **Custom radio:** Add and manage your own streams.

Sources can be enabled or disabled independently in Settings.

### Playback and library
- Continuous album and playlist playback, with next/previous, seek, pause and stop.
- Persistent playback queue, **Play Next**, cross-source playlists, favorites, history and bookmarks.
- Repeat, shuffle and Smart Shuffle, plus sleep timers and smart playlists.
- ReplayGain normalization and optional gapless playback for compatible sources; Spotify uses its own audio engine.
- Resume the last track and position after restarting, **without autoplay**.
- Universal Quick Find across enabled sources, alongside fast filtering of the current list.
- Track ratings, detailed metadata and discovery features.

### Terminal experience
- Keyboard-driven TUI with contextual help, command completion and command history.
- Multiple Now Playing layouts, album artwork in Kitty and graceful fallback in other terminals.
- CAVA audio visualization with multiple styles.
- Linux desktop notifications, MPRIS and media-key support.
- Built-in themes, Omarchy theme following and safe external TOML themes.
- Responsive layouts for narrow and wide terminal windows.

### Listening statistics
- **Actual observed listening time**, not simply nominal track duration.
- Today, week, month, year and all-time totals.
- Top artists, albums, tracks and sources with listening durations.
- Dedicated Statistics screens in NPLAY; local tracking can be turned off.
- Offline SQLite storage, a unique installation ID and a durable event outbox prepared for future integrations.
- **No statistics are uploaded:** network synchronization remains disabled until a server API contract is verified and the user explicitly opts in.

See [STATISTICS.md](STATISTICS.md) for details and integration considerations.

## Requirements and installation

Linux with Python 3 and **mpv**. Optional integrations use librespot (Spotify), yt-dlp (YouTube), CAVA (visualization) and desktop audio/notification utilities. Tested installation targets include Omarchy/Arch and Debian-family distributions; see [INSTALL.md](INSTALL.md) for dependencies and supported setups.

Download the current [release](https://github.com/nirucon/nplay/releases), extract it and run from the extracted directory:

```sh
bash install.sh
nplay --doctor
nplay
```

Your settings, library database, Spotify credentials and listening statistics are stored separately from replaceable application code and are preserved during normal upgrades.

## Getting started

Use `b` to browse, `Ctrl+P` for Universal Quick Find, `/` to filter the current list, `?` for in-app help and `:` for commands. Playback and source configuration live under **Settings**.

For complete instructions, see [KEYS.md](KEYS.md), [CONFIGURATION.md](CONFIGURATION.md), [THEMES.md](THEMES.md) and [INSTALL.md](INSTALL.md).

## Project

NPLAY is an independent personal project, developed primarily for its author's Linux setup and made available for others to use and adapt. Features requiring third-party services depend on their APIs, accounts and available playback tools.

Technical documentation: [ARCHITECTURE.md](ARCHITECTURE.md) · [STATISTICS.md](STATISTICS.md)

Release history: [CHANGELOG.md](CHANGELOG.md) · [GitHub Releases](https://github.com/nirucon/nplay/releases)
