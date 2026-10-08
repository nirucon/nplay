# NPLAY 1.4.1 — installation

NPLAY is a custom application made for Nicklas Rudolfsson, but is fully functional for other users.

## Supported clean-install targets

- Arch Linux
- Omarchy and Arch-derived systems
- Debian
- Ubuntu and Debian-derived systems

Run:

```sh
unzip NPLAY-1.4.1.zip
cd NPLAY-1.4.1
./install.sh
```

The installer detects `pacman` or `apt`, shows missing dependencies, asks before using `sudo`, and installs the required/recommended runtime packages when available. Core playback requires Python 3 and mpv. CAVA enables visualization, yt-dlp enables YouTube audio, and Mutagen enables robust local metadata and embedded artwork.

The installer performs an atomic application-code replacement. Existing configuration, secrets, local library database, playlists, favorites, history and state under XDG config/data/state directories are not replaced.

After installation:

```sh
nplay --doctor
nplay
```

Kitty is not required. In Kitty, NPLAY can render image artwork through `kitten icat`. In other terminals the application remains usable and substitutes a compact source placeholder instead of leaving broken image space.

## Upgrade data safety

The installer replaces only the application code. Configuration, XDG state and `library.db` are preserved. This includes indexed Local Music, playlists, favorites, history and custom radio stations. The existing additive database migrations remain available when upgrading from older schemas; this UI-only release does not change the database schema.


## Spotify local authorization persistence (1.1.3)
Local librespot authorization is a one-time step under normal operation. Credentials are stored in NPLAY's persistent XDG data directory and are reused across NPLAY restarts and code upgrades. Reauthorization is only expected if credentials are removed/revoked, Spotify invalidates them, or the user explicitly disconnects/resets Spotify.
