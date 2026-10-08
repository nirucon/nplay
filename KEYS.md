# NPLAY keybindings

NPLAY is keyboard-first. Press `?` at any time for the authoritative in-app help; the footer also changes with context.

| Key | Action |
|---|---|
| `Esc` | Back exactly one view |
| `Ctrl+N` | Now Playing |
| `b` | Browse |
| `/` | Search |
| `n` / `p` | Next / previous track |
| `Space` | Pause / resume |
| `Left` / `Right` | Seek -/+ 5 s |
| `H` / `L` | Seek -/+ 30 s |
| `+` / `-` | Volume |
| `m` | Mute |
| `a` | Add selected item to queue |
| `A` | Add selected item to local playlist |
| `r` | Randomize/play within the current playable list/source |
| `q` | Queue |
| `i` | Context / information |
| `v` | Cycle Now Playing layout |
| `V` | Toggle visualizer |
| `t` | Theme chooser; changes live |
| `u` | Rescan local library recursively |
| `j/k`, arrows | Move selection |
| `g/G` | First / last item |
| `:` | Command prompt |
| `Q` | Quit |

The design intentionally reserves `n` and `p` for track navigation. `Ctrl+N` is the global jump to Now Playing.

## Restart / resume behavior

When NPLAY restores a previous track after startup it is not playing yet. In Now Playing, `Space` or `Enter` starts that track again and seeks to the saved position when the source is seekable. After media is loaded, `Space` returns to normal play/pause behavior.


## Playback quality

- `s` — cycle Shuffle: Off → Shuffle → Smart
- `x` — cycle Repeat: Off → Track → Context
- `:normalize on|off|auto|track|album` — normalization controls
- `:gapless on|off` — gapless mode (off by default)
- `:shuffle` — cycle shuffle mode
- `:repeat` — cycle repeat mode

ReplayGain normalization is enabled by default. `Auto` chooses album gain for a single-album playback context and track gain for mixed contexts.

## Command mode

Press `:` to enter command mode. `Esc` cancels immediately. `Tab` completes command names/options; repeated Tab cycles matches. `Up` / `Down` browse command history. Examples: `:normalize auto`, `:navidrome`, `:local`, `:queue`, `:gapless off`, `:theme noir`, `:diagnostics`.


## Discovery and list tools (0.6.1)

- `Ctrl+P` — Universal Quick Find across enabled sources
- `/` — filter the current list/menu without a provider request
- `S` — cycle Relevance / Newest / Oldest when publication dates are available

NPLAY is a custom application built for Nicklas Rudolfsson, while remaining fully functional and configurable for other users.

### Context and long-list navigation

- `.` — open context actions for the selected/current item
- `Home` / `g` — first actionable item
- `End` / `G` — last actionable item
- `PageUp` / `PageDown` — move by one visible page

Section headings are informational only and are skipped automatically by keyboard navigation.

## Spotify

The normal transport keys are intentionally unchanged during Spotify Connect playback: `Space`, `n/p`, arrows, `H/L`, `+/-`. `.` on a Spotify item adds Spotify-specific actions such as Save to Spotify Library and Add to Spotify Playlist. Spotify device selection is under `Spotify → Devices`.

## NPLAY 1.2

- `z` — Stop playback and clear Now Playing.
- `C` — Cycle CAVA style: Classic → Gradient → Blocks → Dots. Classic remains the default.
- `.` → **Bookmark Position** — save the current position for a seekable track/episode.
- `.` → **Start NPLAY Radio** — build a cross-source discovery mix from the selected/current item.
- `:stop` — same semantic Stop as `z` and MPRIS Stop.
- `:bookmarks` — open saved long-form positions.
