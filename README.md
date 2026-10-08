# NPLAY

**NPLAY 1.3.5** · A keyboard-first terminal music, radio, and podcast player for Linux.

> **Personal project:** NPLAY is designed primarily for my own Linux desktop setup, workflow, and listening habits. It is published as source code and documentation for anyone interested, but it is **not** a general-purpose commercial product or a promise of support for every environment. Features and priorities reflect my own needs.

NPLAY brings local music, Navidrome/Subsonic, Spotify, Sveriges Radio, custom radio stations, podcasts, and YouTube audio into a single minimal terminal UI. It uses `mpv` for most audio playback and optionally `librespot` for local Spotify Connect playback.

## Highlights

- **Keyboard-first interface:** fast browsing, contextual actions, search, queue management, playback controls, and command prompt.
- **Local music library:** recursive indexing, tags, embedded/folder artwork, albums, artists, folders, and random discovery.
- **Multiple sources:** local files, Navidrome/Subsonic, Spotify (optional), Sveriges Radio, custom streams, and YouTube audio (optional).
- **Listening workflow:** source-aware playlists, queue, favorites, history, ratings, bookmarks, sleep timer, and session resume.
- **Terminal presentation:** Kitty artwork where supported, usable text fallback elsewhere, CAVA visualization, and restrained themes including Niru Noir and Omarchy-follow mode.
- **Linux desktop integration:** optional MPRIS/media keys and desktop notifications.
- **Persistent data protection:** replaceable application code is kept separate from user configuration, credentials, SQLite library, and runtime state.

Some features depend on external services, their APIs, and system packages. Spotify local playback uses Spotify Connect through `librespot`, not `mpv`; a suitable Spotify account and network connectivity are required.

## My setup

NPLAY is primarily built for **Omarchy / Arch Linux**, running in **Kitty** with **Fish** and **Hyprland**. I also use/test it on **Debian**. The UI prefers a clean, dark, minimalist appearance, keyboard interaction, and integration with the surrounding Linux desktop. Kitty is optional: the player should remain usable without terminal graphics.

The code aims to support Arch-based and Debian-based distributions, but the full combination of providers, optional dependencies, and desktop integrations is not continuously tested across all systems.

## Install

On Arch / Omarchy (Fish or Bash), Debian, or Ubuntu:

```sh
git clone https://github.com/nirucon/nplay.git
cd nplay
./install.sh
```

The installer checks for `python3` and `mpv`, offers to install missing dependencies through `pacman` or `apt`, and treats CAVA, yt-dlp, librespot, MPRIS, and notification support as optional according to availability. It may request `sudo` to install distribution packages; NPLAY itself installs into the user's home directory.

After installation:

```sh
nplay --version
nplay --doctor
nplay
```

If `nplay` is not found in a new shell, ensure `~/.local/bin` is on `PATH`. On Fish, for example:

```fish
fish_add_path --move --prepend ~/.local/bin
```

**Upgrade:** pull the repository and rerun `./install.sh`. The installer keeps user data/configuration separate from replaceable program files and copies important existing state to an upgrade backup location before replacing code. Do not delete your XDG data/config directories while upgrading.

For details, see [INSTALL.md](INSTALL.md).

## Quick start

| Key | Action |
| --- | --- |
| `b` | Browse sources and libraries |
| `Ctrl+P` | Universal Quick Find |
| `/` | Filter the current list |
| `Space` | Play / pause |
| `z` | Stop |
| `n` / `p` | Next / previous |
| `Ctrl+N` | Now Playing |
| `.` | Contextual actions |
| `:` | Command prompt |
| `?` | Help |
| `Q` | Quit |

See [KEYS.md](KEYS.md) for the full reference. Settings are available from Browse; most providers can be enabled or disabled individually.

## Spotify: important network requirement

Spotify search and account actions use the Spotify Web API over HTTPS. **Local playback through librespot additionally needs to connect to Spotify access points**, including TCP port **4070** in environments we have tested. Corporate, managed, or restrictive Wi-Fi networks may allow browser authorization and Spotify search while blocking the actual Connect receiver. Typical symptom: browser OAuth succeeds, but the **NPLAY** playback device never appears.

When troubleshooting, first test the AP endpoint shown in your `librespot` log from the **same network** (for example, `nc -4 -vz -w 5 ap-gew4.spotify.com 4070`). The endpoint may change. A timeout is a network diagnostic, **not by itself proof of invalid authorization**. Do not weaken a managed network's firewall policies; use an authorized network or consult its administrator.

Spotify is optional and disabled by default. See [CONFIGURATION.md](CONFIGURATION.md) for Web API setup and the explicit `Spotify → Local Playback → AUTHORIZE / REAUTHORIZE` flow.

## Files and upgrade safety

| Location | Purpose |
| --- | --- |
| `~/.local/lib/nplay/` | Installed application code (replaceable) |
| `~/.local/bin/nplay` | Launcher |
| `~/.config/nplay/` | User configuration and secrets |
| `~/.local/share/nplay/` | Persistent library database, Spotify receiver cache, and other data |
| `~/.local/state/nplay/` | Logs, session state, and upgrade backups |

These are the default locations; XDG variables are respected for data, config, and state by the application/installer where applicable. Do not publish `config.ini`, `secrets.ini`, databases, logs, or Spotify cache from your own machine. This repository contains a **sample configuration only**.

## Project documentation

- [Installation](INSTALL.md) – dependencies, clean install, and upgrades
- [Configuration](CONFIGURATION.md) – sources, playback settings, credentials, and desktop integration
- [Keyboard reference](KEYS.md) – shortcuts and commands
- [Architecture](ARCHITECTURE.md) – project organization and playback architecture
- [Changelog](CHANGELOG.md) – release history
- [Publishing and data safety](PUBLISHING.md) – what belongs in the public repository

## Status and scope

This repository tracks a **personal, evolving application**, currently at version **1.3.5**. Working features are preserved where practical as the code is gradually made more modular. External provider behavior may change independently of NPLAY. This is provided **as-is**, without a support or compatibility guarantee.
