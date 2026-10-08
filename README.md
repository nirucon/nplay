# NPLAY

NPLAY is a keyboard-driven terminal music, radio and podcast player for Linux. It brings local audio, self-hosted libraries and online services into a single interface powered by `mpv`.

## Audio sources

**Local Music:** Browse folders, artists, albums and tracks from a recursively indexed library. NPLAY reads metadata and cover art, supports multiple music directories and preserves the library database between upgrades.

**Navidrome / Subsonic:** Connect to a self-hosted music server to browse and play your collection alongside local files.

**Spotify:** Optional integration for Spotify Premium using the Spotify Web API and Spotify Connect, with device selection and local authorization. Spotify is disabled by default and requires separate setup. See [CONFIGURATION.md](CONFIGURATION.md).

**YouTube:** Search and play audio using `yt-dlp`, with configurable sorting of results.

**Sveriges Radio:** Listen to live radio, browse programmes and play podcast episodes.

**Custom Radio:** Add and manage your own internet radio streams.

## Player features

NPLAY includes global search, cross-source playlists, a persistent playback queue, favorites, listening history and playback resume. It offers shuffle and repeat modes, ReplayGain normalization, optional gapless playback and desktop track notifications.

The terminal interface supports keyboard navigation, album artwork in Kitty, text fallbacks in other terminals, a CAVA visualizer and multiple themes, including Niru Noir, Satie and Omarchy theme following.

## Installation

NPLAY supports Arch Linux, Omarchy, Debian and Ubuntu. Python 3 and `mpv` are core requirements; optional integrations require additional dependencies. From an extracted release:

```sh
./install.sh
nplay --doctor
nplay
```

The installer preserves existing configuration, credentials, playlists and library data. See [INSTALL.md](INSTALL.md) for details.

## Getting started

Press `b` to browse, `/` to search and `?` for keyboard shortcuts. Configure sources, music folders, playback and appearance inside NPLAY.

Further documentation: [Configuration](CONFIGURATION.md), [Keyboard shortcuts](KEYS.md) and [Architecture](ARCHITECTURE.md).

## Author and licensing

Created by **Ing Leif Nicklas Rudolfsson**.

License terms for the application and its dependencies should be verified before redistribution. Third-party services and software retain their respective terms.
