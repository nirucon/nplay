# NPLAY 1.4.1

NPLAY 1.4.1 is a keyboard-first terminal music, radio and podcast player built around `mpv`, with local music, Navidrome/Subsonic, Sveriges Radio, custom radio streams, YouTube audio, playlists, artwork and CAVA visualization.

**NPLAY is a custom application made for Nicklas Rudolfsson, but it is fully functional and configurable for other users as well.**

## What makes NPLAY different

- One terminal UI for local files, Navidrome, radio, podcasts and YouTube audio.
- Recursive local library scan with tags, folder covers and embedded artwork.
- Playback contexts: albums, lists and playlists continue automatically; `n` / `p` move next/previous and `Ctrl+N` opens Now Playing.
- Cross-source local playlists, persistent queue, favorites and history.
- Source management: Navidrome, Sveriges Radio, YouTube and custom radio can be enabled/disabled live. Defaults are enabled.
- Quick Play / global search with `/`; only enabled sources participate.
- Responsive Kitty artwork, plus an intentional text/media-card fallback when terminal graphics are unavailable.
- Responsive full-width CAVA visualizer.
- Live themes: Niru Noir (default), Satie, C. Larsson, Hackerman, Commodore 64 and Omarchy theme following.
- Normal, Artwork and Visualizer Now Playing layouts.
- Context-sensitive footer and complete `?` help.

Browse is organized as **Library**, **Discover**, **Playback** and **NPLAY**. Settings is organized into **Appearance**, **Sources**, **Library**, **Playback** and **System**.

Start with `b` for Browse, `/` for Quick Play/Search and `?` for complete in-app help. Configuration lives under `~/.config/nplay/`; library/state data is preserved across upgrades.

See `INSTALL.md`, `KEYS.md`, `CONFIGURATION.md`, `ARCHITECTURE.md` and `CHANGELOG.md`.

## Persistence and resume

NPLAY deliberately does **not** autoplay after a restart. The last track is restored as a resume candidate instead. Press `Space` or `Enter` in Now Playing to continue. NPLAY persists the last track, playback position, volume, queue and playback context. Local playlists, favorites, history, custom radio stations and the indexed local library live in the persistent SQLite database and are not part of the replaceable application code.

Local Music is recursive and uses `~/Music` by default. Multiple roots can be configured under **Settings → Library → Music Folders**. Automatic incremental refresh on startup is enabled by default and can be disabled there; `u` always starts a manual refresh.

Local Music provides All Tracks, Artists, Albums, Folders, Recently Added, Random Tracks, Random Album and Search. Folder artwork is preferred when explicitly present; otherwise embedded artwork is extracted to NPLAY's cache without modifying the audio file.


## Playback quality

0.6.1 adds a playback-quality layer rather than another provider. ReplayGain normalization is **on by default** (`Auto` mode); gapless is **off by default**. Playback contexts support repeat, shuffle and Smart Shuffle, and Now Playing shows a compact `UP NEXT` line plus active playback badges such as `RG TRACK`, `GAPLESS`, `SHUFFLE SMART` and `REPEAT CONTEXT`.

Use `s` to cycle shuffle and `x` to cycle repeat. Playback quality can also be changed live under **Settings → Playback** or with `:normalize`, `:gapless`, `:shuffle` and `:repeat`.

### Command UX

The `:` prompt behaves like a small NPLAY command shell: Esc cancels, Tab completes commands/options, and Up/Down recall command history. This keeps advanced controls discoverable without crowding the normal browser UI.

### Resume artwork

Navidrome authentication URLs are never persisted as session data. NPLAY stores stable cover identity and reconstructs the authenticated URL when needed. The artwork cache uses that stable identity, so already cached artwork can be reused after restarting NPLAY.


## 0.6.1 note

Now Playing transport telemetry (elapsed time and progress indicator) refreshes independently at a controlled cadence while the CAVA visualizer retains its isolated high-frequency render path. This keeps playback state live without bringing back full-screen visualizer flicker.

## NPLAY 1.4.1 interaction model

- `Ctrl+P` opens **Universal Quick Find** across the local library and enabled providers. Results are grouped by source.
- `/` filters the **current** list locally and instantly; an empty filter restores the list.
- `S` cycles publication-date sorting where date metadata exists: Relevance → Newest → Oldest. SR episode lists default to newest first.
- YouTube and podcast results show publication dates when the provider exposes them without an expensive per-result lookup.
- The global transport footer remains live while browsing, searching, viewing playlists, radio and settings.
- Search/list rendering is responsive: wide terminals use title/creator/date columns; narrower terminals collapse metadata into a compact row.

Publication dates are best-effort. NPLAY deliberately does not perform dozens of extra network requests merely to populate missing dates.

### Spotify Preview

NPLAY 1.4.1 combines the official Spotify Web API with an optional local `librespot` playback engine. Search, playlists, metadata and library actions use the Web API; audio can play directly on this computer through the NPLAY-managed local engine. External Spotify Connect targets are still available but are no longer required. See `CONFIGURATION.md` for setup details.


## Spotify local playback (1.1.3)

`Spotify → Local playback` manages a private librespot receiver named **NPLAY**. On first use librespot may open its own browser OAuth authorization; its credential cache is stored under the NPLAY XDG data directory with mode 0700. Later starts reuse that cache. Audio uses the PulseAudio backend, which routes through PipeWire on normal Omarchy/Arch and modern Debian desktops.

NPLAY owns the librespot process and stops it when NPLAY exits. If librespot is unavailable, every non-Spotify source remains fully functional and Spotify can still use an external Connect device.


## Spotify local authorization persistence (1.1.3)
Local librespot authorization is a one-time step under normal operation. Credentials are stored in NPLAY's persistent XDG data directory and are reused across NPLAY restarts and code upgrades. Reauthorization is only expected if credentials are removed/revoked, Spotify invalidates them, or the user explicitly disconnects/resets Spotify.


## Spotify continuity and CAVA (1.1.3)

Local Spotify playback is a first-class NPLAY backend. Album and playlist playback continues automatically in context, pause/resume preserves position, local Connect sessions are rebound after restart, and CAVA can visualize the librespot output through the normal system audio capture path. Spotify audio is not routed through mpv and NPLAY does not apply ReplayGain/DSP to Spotify content.

## Linux desktop notifications

NPLAY 1.4.1 can show a system Now Playing notification when the track changes, including artwork when available. Notifications and notification artwork are both enabled by default and independently configurable in Settings → Playback.

## NPLAY 1.0

The first stable release adds Linux MPRIS/media-key integration, complete queue management, ratings, listening statistics, smart playlists, radio discoveries, sleep timers, richer track information and cross-source matching in Universal Quick Find. Local library scanning remains designed for large collections with WAL, short batched writes, incremental metadata checks, symlink-aware roots and non-destructive offline handling.

Useful commands: `:sleep 30`, `:sleep track`, `:sleep album`, `:sleep off`, `:smart`, `:stats`, `:discover`, `:rating 0-5`, `:queue-save`, `:queue-clear`.

Lyrics are intentionally outside the 1.0 scope.

## 1.1 playback integrity

NPLAY 1.1 treats Spotify and mpv as transports behind one NPLAY-owned playback model. Queue, Play Next, Sleep Timer, MPRIS and source handoff therefore keep the same semantics across Local, Navidrome, Spotify, YouTube and radio. Local library schema 4 adds rich tag/technical metadata and listening statistics now measure elapsed listening time rather than summing nominal track durations.

## 1.2 architecture and discovery

NPLAY 1.2 begins a behavior-preserving modularization of the application. Cross-source search lives in `nplay/services/search.py` and discovery-radio generation in `nplay/services/radio.py`; playback/provider code remains behind existing interfaces so the verified Spotify and radio state machines are not rewritten merely for architectural style.

Press `z` for a real Stop that clears Now Playing. Use Actions (`.`) on a playable item to start NPLAY Radio or bookmark a long-form position. CAVA keeps the existing Classic renderer by default; press `C` to cycle Classic, Gradient, Blocks and Dots, or choose the style under Settings → Appearance.

## Custom themes (1.4.1)

External TOML themes are supported without modifying the app. Open **Settings → Appearance → Theme → HOW TO INSTALL THEMES** for in-app instructions and a reload action. See [THEMES.md](THEMES.md) for the format and validation commands. No extra themes are bundled; a separate theme pack is planned.
