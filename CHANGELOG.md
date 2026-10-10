# NPLAY 1.5.6 — Unicode prompt input

- Use curses wide-character input in interactive prompts (å, ä, ö and other Unicode).
- Preserve command history, completion, editing and terminal-width-aware cursor placement.
- No changes to playback, configuration, statistics or persistent data.

# NPLAY 1.5.5 — Local playback readiness

- Fix missed `file-loaded` events by reconciling IPC state (`path`, `idle-active`, `core-idle`) after mpv startup.
- Validate the loaded path matches the requested media before accepting playback.
- Preserve existing process lifecycle protections, Spotify fallback, statistics and synchronization.
- Add timeout state diagnostics for unresolved playback failures.
- No changes to user configuration, tokens, caches, or SQLite databases.

# NPLAY 1.5.5 — Statistics Sync & Optimization

- Verified n.rudolfsson.net 0.6.4 Statistics API v3 contract against the supplied PHP sources.
- Opt-in HTTPS statistics synchronization, 50-event batches, per-installation token, strict acknowledgment checks, timeout and exponential backoff.
- Non-blocking background worker, explicit manual sync, secure token entry and persistent last-success timestamp.
- Reduced new listening events to server-compatible ~5-second chunks; preserves all existing 1.5.2 events and outbox records without destructive migration.
- Flush on pause/stop/track transition/normal exit, with local-midnight splitting and conservative suspend handling.
- Statistics Settings & Sync shows server, privacy, state, pending count, errors and installation ID.
- Improved Spotify 403 restriction diagnostics; avoids unsafe blind retries. Spotify provider restrictions require account/device verification.
- Installer backs up statistics SQLite; adds Void xbps dependency installation path.

# NPLAY 1.5.5 — Statistics final polish

- Remove repeated period-help text; shorten ranking descriptions.
- Refresh visible Statistics overview, ranking and settings screens every ten seconds during playback.
- Preserve current selection and navigation history during automatic refresh.
- Avoid stacking menus when changing local statistics collection state or using Back to Overview.
- Document a transport-neutral event adapter contract for independent third-party integrations.
- No schema migration, no HTTP sync, no playback-engine changes.

# NPLAY 1.5.1 — Statistics UX and stability

- Display actual listening time and ranked listening durations directly in visible menu labels.
- Separate concise overview from dedicated Artists, Albums, Tracks and Sources rankings.
- Move local tracking toggle, offline sync status, pending outbox and installation ID into Settings & Sync.
- Add optional clipboard copy of the installation ID using available Linux clipboard utilities.
- Change period selection in-place instead of creating repeated navigation stack entries.
- No database migration, network synchronization, or changes to the playback engine.

# NPLAY 1.5.1 — Offline Statistics

- Separate, modular SQLite statistics with a stable per-installation UUID.
- Actual active listening intervals for Local, Navidrome, Spotify, YouTube and radio sources.
- Today, current week, month, year and all-time views in NPLAY TUI.
- Top artists, albums, tracks and sources ranked by time listened.
- Local collection toggle; persistent unsent event outbox with idempotent event IDs.
- No network synchronization until the Statistics API v2 contract is independently verified.
- Existing player database, playback pipeline, Spotify Connect recovery and themes retained.

# 1.5.1 — Spotify Connect self-recovery

- When a running NPLAY-owned librespot process does not appear in Spotify Connect, restart it once and retry device registration.
- Re-check visibility under a recovery lock to avoid redundant restarts.
- 90-second recovery cooldown prevents restart loops on blocked networks or invalid authorization.
- Reap stopped librespot children (including after SIGKILL fallback) and close parent-side log descriptors.
- Preserve cached credentials, external Connect mode, playback state and all user data.
- No automatic restart during successful playback.

# 1.5.1 — Custom theme installation UX

- Replace the technical in-app instructions with a short, actionable three-step guide.
- Display the actual XDG theme directory in the app.
- Add **Open theme directory**, using the desktop's `xdg-open` without executing theme content.
- Display **No custom themes installed** in the theme picker when appropriate.
- Keep **Reload custom themes** and all existing theme palettes.
- No playback, Spotify, database, or installer architecture changes.

# 1.4.0 — External theme support

- Load validated user TOML themes from XDG_CONFIG_HOME/nplay/themes.
- Theme picker includes installation guidance and a reload action.
- CLI: --list-themes and --check-theme FILE.
- No new bundled themes; existing built-in palettes and playback preserved.

# 1.3.5 — Installer path hotfix

- Fixes the 1.3.4 installer resolving the new application directory to `/nplay` when `~/.local/lib` did not already exist.
- Application code now installs deterministically to `~/.local/lib/nplay`; persistent data remains in `~/.local/share/nplay`.
- The failed 1.3.4 install occurs before code replacement, so existing NPLAY data/code is not intentionally removed by this failure path.
- No playback, Spotify state-machine, database or persistent-data changes.

# 1.3.4 — Upgrade-safe data layout & Spotify credential persistence

- Separates installed application code from persistent XDG data. Code now installs under `~/.local/lib/nplay`; `~/.local/share/nplay` is data only.
- Fixes a serious historical installer flaw where upgrading replaced `~/.local/share/nplay`, which could remove `library.db`, artwork caches and `spotify-local` librespot credentials.
- Creates a small pre-upgrade safety backup of library DB, Spotify local credentials and configuration under `~/.local/state/nplay/upgrades/<timestamp>/`.
- Existing 1.3.3 data is preserved in place; future upgrades no longer replace it.
- Spotify explicit AUTHORIZE / REAUTHORIZE now waits for the NPLAY Connect device to actually register before reporting success.
- Normal Spotify playback still never opens OAuth automatically. If credentials were already lost by an older upgrade, one explicit reauthorization is required once; credentials then survive future NPLAY upgrades.
- No database schema change.

# 1.3.4 — Spotify local engine lifecycle hardening

- Fixed the remaining 1.3.2 logger regression by restoring NPLAY's module logger in `spotify.py`; the librespot file handle remains separately named.
- Removed the unreliable `has_credentials()` preflight gate from normal Spotify playback. NPLAY now starts librespot without OAuth and lets librespot reuse its own persisted authentication state.
- Normal Spotify playback can never open an OAuth browser. Browser authorization is now a separate explicit `AUTHORIZE / REAUTHORIZE` action under Spotify → Local Playback.
- Split `START LOCAL ENGINE` (silent/no OAuth) from explicit authorization so diagnostics and recovery are predictable.
- Moved the managed librespot runtime log to the XDG state directory (`~/.local/state/nplay/librespot.log`) alongside `nplay.log` and `mpv.log`.
- Added librespot process-start, early-exit and device-registration diagnostics without logging credentials or tokens.
- Local diagnostics now show the engine log path and best-effort cached-file count. The cached-file count is diagnostic only and never blocks playback.
- No Spotify Web API state-machine, database, library, playlist or persistent-data migration.

# 1.3.2 — Spotify startup regression fix

- Fixed a Spotify local playback crash introduced in 1.3.1 where the librespot log file handle shadowed NPLAY's logger, causing `cannot access local variable 'log' where it is not associated with a value`.
- Spotify local startup logging now uses a distinct `log_file` handle and keeps the module logger intact.
- No database, library, playlist or persistent-data migration.

# 1.3.1 — Playback transition hardening

- mpv-backed Local, Navidrome, SR and YouTube startup now resolves media and waits for `file-loaded` off the curses/UI thread. Slow disks, network resolution or yt-dlp can no longer freeze navigation while a track starts.
- The previous authoritative Now Playing state remains intact until the replacement transport is actually ready, eliminating transient mixed states during source handoff.
- Added phase timing to `nplay.log` for source resolution and mpv `file-loaded`, making slow-start diagnosis deterministic.
- Stop now cancels in-flight playback requests as well as active transports, preventing a slow background load from starting after the user pressed Stop.
- Automatic Spotify playback never launches an OAuth browser. OAuth is now restricted to the explicit Spotify Local setup/start flow; missing local credentials produce an actionable error instead of surprising browser authorization.
- Added librespot credential/start diagnostics without logging secrets.
- Preserves database schema 5 and all 1.3.0 user data/configuration. No destructive migration.

# 1.3.0 — Modular runtime & stability

- Continued the 1.2 architecture work without rewriting the verified Spotify/SR playback paths.
- Split source/navigation orchestration out of `app.py`; the application core is substantially smaller and easier to evolve.
- Split curses rendering out of `ui.py`, keeping navigation/input separate from presentation.
- Added a small internal EventBus for playback/media/integration state changes.
- Added canonical `PlaybackState` and a stable `PlaybackController` façade used by MPRIS.
- Added a provider capability `SourceRegistry` so cross-source services can ask what a provider supports instead of growing source-name conditionals.
- Added atomic `SessionStore` for session persistence and future migrations.
- Added explicit error taxonomy for provider, network, authentication, playback, media and configuration failures.
- Browse now exposes a compact Continue section when a resumable/current session exists.
- Added regression tests around state, events, source capabilities, session persistence and stop semantics.
- Kept database schema 5 and persistent user data unchanged; no destructive migration is required.
- Preserved 1.2.0 features: real Stop, NPLAY Radio, federated Quick Find, bookmarks, queue-after-context, Othala/Ingwaz and selectable CAVA styles.

# 1.2.0 — Architecture, control & discovery

- Added a real user-facing Stop (`z` / `:stop` / MPRIS Stop) that stops transport and clears Now Playing instead of leaving a paused session behind.
- Split cross-source search and NPLAY Radio into dedicated service modules; App now delegates orchestration instead of owning every concern directly. This is the first behavior-preserving step away from the historical monolith.
- Universal Quick Find now queries connected remote providers concurrently and publishes results progressively; a slow provider no longer serially blocks the others.
- Added NPLAY Radio: build a provider-neutral discovery mix from a selected/current track using enabled Local, Navidrome and Spotify sources while avoiding duplicate identities and immediate artist repetition.
- Added long-form bookmarks with additive SQLite schema 5. Bookmarks persist track/episode + exact position and resume from that position.
- Added Othala and Ingwaz themes with deterministic xterm-256 palettes and portable fallbacks.
- Added selectable CAVA rendering styles: Classic (unchanged default), Gradient, Blocks and Dots. Styles are theme-aware and do not restart playback.
- Added CAVA style to Settings → Appearance and `C` as a quick cycle key.
- Preserved the verified Spotify 1.1.x state machine, 1.1.7 live-radio semantics, Local/Navidrome random tools, database contents and XDG configuration.
- Bandcamp and Spotify audiobooks intentionally remain out of scope for this release.

# 1.1.8 — Local crash fix & Random Play

- Fixed a SQLite syntax regression that crashed Local Music → All Tracks.
- Added Random 10, Random 50 and Random 100 songs to Local Music and Navidrome.
- Added artist-search shuffle: choose an artist and shuffle all indexed/server tracks by that artist.
- Added Random Artist: choose an artist at random and show that artist's catalogue in album/track order.
- Preserved the 1.1.7 live-radio model and all 1.1.x Spotify state fixes.
- Added regression smoke coverage for Local Music database queries and random helpers.

# 1.1.7 — Live Radio Polish

- Live radio is now a station, never a playback context: P1 no longer shows P2 as Up Next.
- SR Now Playing is programme-first and can show current programme times plus the next programme on the same channel when SR provides schedule data.
- ReplayGain, gapless, track counters and music Up Next are hidden for live streams.
- SR programme refresh is silent in normal UI; live status remains user-oriented.
- Custom radio uses the same LIVE model and polls mpv/ICY metadata for current artist/title when available.
- MPRIS no longer exposes a fake duration or seek capability for live radio and does not advertise station-list Next.
- Radio selection no longer creates a playlist context from neighboring stations.
- No Spotify, database, library or persistent-data changes.

# 1.1.6 — Light-theme contrast polish

- Satie now uses a darker aged-paper field with near-black graphite text, stronger secondary text and restrained sepia accents.
- C. Larsson now uses a darker linen/ochre Sundborn-inspired field instead of the bright lemon cast, retaining forest green, olive and restrained falun red.
- Built-in non-native themes no longer rely on terminal `A_DIM` for secondary information. They use their explicit semantic muted colour, fixing washed-out metadata, Up Next, footer/help text and timing information.
- Hackerman and Commodore 64 benefit from the same deterministic muted-text rendering; Niru Noir and Omarchy keep terminal-native dim behaviour.
- Fixed the Now Playing header version display to use the package version dynamically instead of a stale hard-coded `1.1.3`.
- No playback, Spotify, database, library or persistent-data changes from 1.1.5.

# 1.1.5 — Theme readability & retro palettes

- Reworked Satie for substantially stronger contrast: graphite typography on a restrained score-paper background with a warm brown accent.
- Reworked C. Larsson around Sundborn/home-watercolour tones: warm cream paper, deep green, olive and restrained falun-red accents. Removed the accidental pink cast caused by xterm-256 RGB rounding.
- Added Hackerman: a readable black/phosphor-green terminal theme with restrained accent use.
- Added Commodore 64: a readable C64-inspired deep-blue/light-blue palette without changing the terminal font or sacrificing artwork colour.
- Built-in themes now pin deliberate xterm-256 colour indices so Kitty/256-colour terminals render the intended palette instead of coarse RGB cube surprises.
- Added `:theme hackerman` and `:theme c64` aliases/completion; all themes remain available from the live Theme menu.
- Niru Noir behaviour is intentionally unchanged. No playback, Spotify, database, library or persistent-data changes.

# 1.1.4 — Spotify pending timeline & UI reconciliation polish

- Fixed Spotify pending playback position incorrectly falling back to the previous session position when the new track legitimately starts at `0.0`. Zero is now treated as a real position, not as a missing value.
- Pending Spotify playback now advances its timeline locally from the requested start position while the eventually-consistent Spotify Web API catches up.
- Stale progress from the previous Spotify resume session cannot drive the new track timeline.
- `syncing metadata…` is now a short transition hint only. Reconciliation continues silently in the background and remains fully logged; normal UI returns to `Playing · <track>` after the grace period.
- Spotify confirmation no longer exposes a technical `synced` status to the normal playback UI.
- No database, provider, authorization or persistent-data format changes.

# 1.1.3 — Spotify restart/resume state-machine fix

- Reworked Spotify startup around explicit `resume candidate`, `pending playback`, and `confirmed playback` semantics.
- A successful Spotify play command now commits the user's requested track as pending immediately; stale `/me/player` data can no longer flash the previous artwork/title back into Now Playing.
- Spotify Web API mismatch is treated as eventual consistency, never as a playback failure by itself.
- Added background reconciliation: stale observations are ignored until Spotify reports the requested track, then metadata/context are atomically confirmed and normal polling begins.
- An explicit new Spotify selection invalidates restored paused-session ownership before playback starts.
- `Spotify playback unavailable` is now reserved for real play/device/auth failures, not delayed Web API convergence.
- Expanded INFO diagnostics for pending commits, reconciliation observations, confirmations, and stale request termination.
- No database schema change; existing Spotify authorization, library, playlists, ratings, history and settings remain persistent.

# 1.1.2 — Spotify state ownership hotfix

- Fixed the reproduced Spotify → Spotify stale-context bug where Now Playing could show the newly requested track while Up Next/session state still belonged to a previous Spotify search/context.
- Added a Spotify track-generation token in addition to playback-generation ownership. A new Spotify play request immediately invalidates poll/start work belonging to the previous requested track.
- Spotify playback is now committed only after the Web API confirms the exact requested track ID (and local device when available). Eventual-consistency responses containing the previous track are waited out instead of being rendered or notified.
- Spotify discovery/search/top/recent result lists are no longer treated as native playback contexts. A directly selected result starts as a clean 1/1 context unless the track explicitly carries an album/playlist `context_uri`.
- Album/playlist context is frozen at request time and committed atomically with the confirmed current track. Async UI changes can no longer substitute a different context before Spotify startup completes.
- Native Spotify album/playlist advances keep their context metadata. A Spotify change outside the active owned context is accepted as an external change but resets NPLAY to a fresh single-track context instead of inheriting stale Up Next entries.
- Session state is saved only after the confirmed track and its matching context have been committed together.
- Added actionable INFO-level Spotify diagnostics for play request, confirmation observations, committed generation/context, external context resets and stale request rejection.
- Spotify OAuth/librespot persistent authorization, Local/Navidrome/SR/YouTube playback, SQLite schema 4 and user data are unchanged.

# 1.1.1 — Playback hardening & consistency

- mpv playback now waits for `file-loaded`; startup errors/timeouts are surfaced instead of silently committing a false Playing state.
- Added rotating NPLAY log plus mpv log under XDG state for actionable diagnostics.
- Previous uses a cross-source playback back stack; Spotify is no longer a special history island.
- Shuffle preserves original context, restores album order when disabled and Smart Shuffle uses source-aware identities.
- Listening statistics distinguish starts from substantial plays (>=30s or >=50%) and completed plays.
- One shared artwork cache instance is used by UI and notifications; runtime limit changes prune the same cache.
- MPRIS adds SetPosition and dynamic CanGoNext/CanGoPrevious.
- Local sorting and album grouping use album artist/year/disc/track metadata more consistently, improving compilations.
- Embedded artwork participates in cache pruning.

# 1.1.0 — Unified playback & stability

- Centralized stop/control semantics across mpv and Spotify; Sleep Timer and MPRIS Stop now work with both transports.
- Queue is NPLAY-owned across sources; cross-source Play Next/Queue no longer disappears into a separate Spotify queue.
- Spotify native auto-advance is reconciled with NPLAY Queue and sleep-at-track/context behavior.
- Playback handoff now prepares provider data before committing authoritative state, with rollback protection on failed starts.
- Cross-source handoff can preserve position into Spotify when track durations are compatible.
- MPRIS2 now emits PropertiesChanged and supports Play, Pause, Stop, Seek, volume, metadata and playback state updates.
- Listening telemetry stores actual elapsed listening time and completion separately from simple history starts.
- Ratings/history identities are source-qualified while legacy local records remain readable.
- Full Rescan uses an explicit force mode and no longer corrupts mtimes as a rescan marker.
- Local indexing schema 4 adds album artist, disc/track number, year, genre, duration, codec, sample rate, bit depth, channels and bitrate. Album order uses disc/track metadata.
- Unified artwork cache is shared by Kitty rendering and notifications, with configurable size, LRU pruning and cache clearing.
- SR live metadata refreshes continuously during playback and updates UI, MPRIS and notifications without restarting the stream.
- Command completion includes the 1.x commands; Track Info calls ratings NPLAY rating and exposes richer local technical metadata.
- Database migration is additive and creates `library.pre-1.1.0.db` before schema-4 migration.
- Lyrics remain intentionally out of scope.

# 1.0.1 — Playback state & desktop polish

- Fixed Spotify → Navidrome/local/YouTube handoff race: stale Spotify polling can no longer overwrite current track/artwork after another source takes ownership.
- Added generation-based authoritative playback session state shared by transport/UI integrations.
- Hardened Spotify/librespot credential reuse across code upgrades; persistent XDG cache/system-cache are both recognized.
- SR live notifications now become programme-first (`Ekonomiekot Extra` / `P1 · Sveriges Radio`) and refresh when programme metadata arrives.
- Desktop track notifications keep replacement semantics instead of intentionally stacking rapid track changes.
- Fixed Kitty artwork placement off-by-one so artwork and metadata begin on the same terminal row/column grid.
- Now Playing says `LIBRESPOT` instead of ambiguous `LOCAL` for Spotify's local engine.
- Playback context and Up Next labels are cleaner and de-duplicate common filename/tag noise.
- Cross-source Quick Find matches retain their alternatives and expose `Switch Source`; seekable handoffs continue near the current position.
- Configuration permissions are hardened to 0600 on save.
- No lyrics feature added.

# 1.0.0 — Integrated Linux music player

NPLAY 1.1.1 promotes the 0.9.x line to the first stable release. It preserves the verified Local, Navidrome, Spotify, Sveriges Radio, YouTube, CAVA, artwork, notification and large-library architecture and adds the missing player fundamentals around it.

- Optional MPRIS2 service (`org.mpris.MediaPlayer2.nplay`) for Linux media keys, `playerctl`, desktop metadata/artwork and play/pause/next/previous. NPLAY remains fully usable when D-Bus bindings are unavailable.
- Queue workflow completed: Play Next, append, reorder, remove, clear and save the current queue as a persistent local cross-source playlist.
- Local artist/album browsing remains first-class and album playback now preserves file/track order rather than alphabetizing song titles.
- Track information now includes file path, NPLAY rating, source availability and play count where known.
- Local 0–5 ratings, stored only in NPLAY's SQLite database; source files are never rewritten.
- Listening statistics are private/local and expose seven-day plays, top artists and source usage.
- Smart playlists: Recently Added, Most Played, Never Played, Unplayed Albums, Highly Rated and Random 50.
- Discoveries: save current radio metadata for later lookup without recording or copying the stream.
- Universal Quick Find detects exact artist/title matches across Local, Navidrome, Spotify and YouTube and surfaces a preferred-source match while retaining the provider-specific results below it.
- Incremental `Scan Changes` remains the default large-library operation; `Full Rescan` explicitly forces metadata re-reading. Offline-root retention and WAL/batched writes are preserved.
- Sleep timer commands: `:sleep MINUTES`, `:sleep track`, `:sleep album`, `:sleep off`.
- System notifications with artwork remain enabled by default and configurable.
- Lyrics are intentionally not included in 1.1.1.
- Additive DB schema migration to v3 adds ratings and discoveries without rebuilding or deleting the existing library, playlists, favorites or history.

# 0.9.4 — Desktop track notifications

- Adds Linux desktop Now Playing notifications through the freedesktop notification stack (`notify-send`).
- Track notifications are enabled by default and can be disabled under Settings → Playback.
- Artwork is enabled by default and reuses NPLAY artwork/cache semantics; missing artwork degrades cleanly to text.
- Notifications fire on actual track changes across Local, Navidrome, Spotify, YouTube and podcast playback, including Spotify native album/playlist continuation.
- Pause/resume, seek, volume changes and repeated Spotify polling do not generate duplicate notifications.
- Adds a Test Notification action and diagnostics/capability reporting.
- `notify-send` remains an optional desktop integration; NPLAY playback never depends on a notification daemon.
- Existing 0.9.3 YouTube, Spotify, large-library/SQLite and responsive artwork behaviour is preserved.

# 0.9.3 — Startup hotfix & compact artwork

- Fixed Debian/portable startup regression in 0.9.2: the YouTube provider now imports `os` before checking executable permissions for the preferred yt-dlp binary.
- Added an executable smoke test for the YouTube provider to catch runtime import/name errors that bytecode compilation alone cannot detect.
- Now Playing artwork now scales down on Kitty terminals instead of disappearing below 70 columns / 20 rows; it remains visible down to a practical 42×16 terminal.
- Browse/list artwork previews now scale down and can remain visible from roughly 76×17 when enough text space remains.
- Compact geometry reserves text and footer space so artwork never wins over transport usability.
- YouTube diagnostics use NPLAY's actually selected yt-dlp binary/version.
- No Spotify, Navidrome, SR, local-library, SQLite, playlists or persistent-data behaviour was intentionally changed.

# 0.9.2 — YouTube playback portability

- YouTube playback now hands the canonical YouTube webpage to mpv and explicitly points mpv's ytdl hook at the yt-dlp binary selected by NPLAY. This matches the verified Debian playback path and avoids fragile bare stream URLs.
- NPLAY prefers an executable `~/.local/bin/yt-dlp` over an older distribution copy in `/bin` or `/usr/bin`, while retaining normal PATH discovery for Arch/Omarchy and other systems.
- YouTube search and fallback resolving use the same selected yt-dlp binary.
- `nplay --doctor` reports the actual yt-dlp version and path.
- No Spotify, Navidrome, SR, local-library, SQLite or playback-control behaviour was intentionally changed.

# NPLAY 0.9.2

## 0.9.2 — Large library & SQLite robustness

- SQLite WAL mode, 10 s busy timeout and bounded write retry.
- Long recursive scans commit in short batches instead of holding a database write lock for the whole library.
- Playback checkpoints are best-effort and can never crash curses because a scan is writing.
- Large-library scan progress is reported while playback remains available.
- Configured root symlinks are resolved and followed; nested symlink traversal stays disabled by default to avoid loops and duplicate libraries.
- Offline/missing library roots retain indexed tracks and never trigger mass deletion.
- Canonical paths prevent duplicate indexing through alternate symlink paths.
- Exclusions support `music_excludes` (colon separated); `Reaper-projects` is the safe default.
- Built-in exclusions cover common VCS/cache/trash directories.

# NPLAY 0.9.2

## 0.9.2 — Spotify session, continuity & visualization

- Spotify local Connect device IDs are runtime-only and rediscovered after every engine/session start.
- Stale `Device not found` controls automatically rebind to the current NPLAY librespot receiver.
- Space now performs a true Spotify pause/resume without resending the track URI; paused position is preserved.
- Recovery resume can reconstruct the current track and seek back to the saved position if Spotify lost the session.
- Spotify album and playlist tracks carry their native context URI, so Spotify advances automatically in album/playlist order when a track ends.
- Full Spotify playlists are paged into NPLAY (up to 500 accessible tracks) instead of silently stopping at the first API page.
- NPLAY playback context follows Spotify native track changes so Up Next and context position remain synchronized.
- CAVA visualization is enabled for local Spotify playback. It observes the normal system/PulseAudio-compatible output used by librespot; Spotify audio is not routed through mpv or modified.
- Spotify Now Playing clearly shows PAUSED versus local bitrate.
- Existing Local, Navidrome, SR, YouTube, mpv, artwork, queue and persistent data paths are preserved.

# NPLAY 0.9.2

- Spotify transport hardening: successful HTTP 204/empty responses are handled without JSON decoding errors.
- Local librespot authorization is persistent: browser OAuth is requested only when reusable local credentials are missing. Upgrades preserve XDG Spotify credentials.
- Spotify search ranking favors exact/token matches and suppresses unrelated artist hits.
- Grouped Spotify search uses fair-share sections with Show all actions so tracks are not starved by artists/albums.
- Spotify Now Playing metadata is cleaner (`SPOTIFY · LOCAL · 320 KBPS`); development placeholder text removed.
- Existing Local, Navidrome, SR, YouTube, mpv, playlists and CAVA behavior preserved.

# NPLAY 0.9.2

## 0.9.2 — Local Spotify playback

- Added optional NPLAY-managed librespot playback backend.
- Spotify now defaults to **This computer** instead of requiring another Spotify client.
- First local start performs librespot OAuth when credentials are not cached; later starts are headless.
- Local receiver uses 320 kbps, a private XDG cache and the PulseAudio backend (PipeWire-compatible).
- Spotify → Local playback exposes mode selection, engine start/stop and diagnostics.
- External Spotify Connect remains available as an optional playback target.
- Playback is only committed in NPLAY after the local receiver appears and Spotify accepts the track.
- NPLAY owns and terminates the local engine on exit.
- Installer detects a distro-provided librespot package without breaking installation on repositories where it is unavailable.
- Existing Local, Navidrome, SR, YouTube, playlists, mpv, artwork and CAVA paths are preserved.

# NPLAY 0.7.1

- Spotify playback now requires and resolves a real Spotify Connect device before NPLAY changes playback state.
- Failed Spotify starts no longer stop mpv or falsely show the selected Spotify track as playing.
- Spotify Devices now supports active/preferred device state, refresh and Web Player launch.
- Spotify album tracks retain album artwork and album name.
- Spotify search results are grouped by artists, albums, tracks and playlists.
- Removed subscription detection via `/me.product`, which Spotify removed from Development Mode responses in 2026. Premium is enforced by Spotify's Player API.

# Changelog

## 0.6.1

- Fixed live playback time/progress in Browse and every non-Now-Playing view via a footer-only transport refresh.
- Added publication metadata for YouTube and Sveriges Radio when exposed by provider responses.
- SR episode/program episode views prefer newest-first ordering.
- Added `S` date sort cycle: Relevance / Newest / Oldest.
- Added `Ctrl+P` Universal Quick Find with source-grouped results.
- Changed `/` to instant in-view filtering for long album, artist, programme and search lists.
- Added result counts and responsive title/creator/date rendering.
- Preserved the established playback engine, normalization, CAVA rectangle renderer, artwork lifecycle, playlists, database and upgrade-safe XDG state.

## 0.6.1 — transport telemetry regression fix

- Fixed Now Playing progress position and elapsed time becoming visually frozen during active playback.
- Dynamic transport telemetry now invalidates Now Playing at a controlled 5 Hz cadence when values change.
- CAVA keeps its separate 30 fps rectangle-only rendering path, preserving the anti-flicker fix from 0.5.1.
- Pause naturally freezes the timeline; resume/playback continues updating it without redrawing artwork unnecessarily.
- No database, configuration or persistent-state migration is required; upgrades preserve the existing library, playlists, favorites, history and session state.

## 0.5.1 — playback quality

- ReplayGain normalization enabled by default; no audio files are modified.
- Auto/Track/Album normalization modes, 0 dB preamp and clipping protection.
- Gapless playback option added and deliberately disabled by default.
- Repeat Off/Track/Context.
- Shuffle Off/Shuffle/Smart; Smart mode avoids recent tracks and immediate same-artist runs where possible.
- Compact Up Next information in Now Playing.
- Active playback badges in Now Playing.
- Playback controls exposed in Settings, commands, Help and documentation.


## 0.4.3 — persistence & local-library polish

- Fixed restart/resume semantics. A restored track is now a **resume candidate**, not falsely treated as actively playing.
- `Space` or `Enter` on Now Playing resumes the restored track when mpv has no active media loaded.
- Playback position, volume, queue, playback context and context index are persisted. NPLAY never autoplays on startup.
- Navidrome session state no longer persists authenticated stream/cover URLs. URLs are regenerated from the current configuration when playback resumes.
- Added SQLite schema versioning (`user_version=2`) and an additive migration for local-library `added` timestamps.
- Before the 0.4.3 schema migration, NPLAY creates `library.pre-0.4.3.db` once as a safety backup.
- Existing playlists, playlist items, favorites, history, radio stations and indexed tracks are preserved.
- Local Music now has real Recently Added, Random Album and local Search in addition to tracks/artists/albums/folders/random tracks.
- Local library refresh runs in the background on startup by default. It is incremental: unchanged files do not have their metadata reparsed.
- Added Settings → Library → Auto Refresh. Manual `u` scan remains available.
- Local scans remain recursive across every configured music root.
- `nplay --doctor` reports the local database schema version.

## 0.4.2

- Added live source management and a clearer Settings structure.
- Navidrome, Sveriges Radio, YouTube and custom radio can be enabled/disabled without deleting configuration.
- Improved Browse grouping, Quick Play wording and diagnostics.
- Preserves configuration, database, playlists, favorites, history and session state across upgrades.

## 0.4.1

- Restored Niru Noir to terminal-native foreground/background behavior for good contrast on dark Omarchy themes.
- Selection uses reverse video and artwork remains in original colours.

## 0.4.0

- Added live themes: Niru Noir, Satie, C. Larsson and Omarchy-follow mode.
- Added responsive full-width visualizer, Local Music artist/album/folder browsing and contextual footer hints.

## 0.5.1 — interaction polish

- Fixed intermittent visualizer tearing by isolating CAVA animation redraws to its owned rectangle instead of erasing the complete curses screen at 30 fps.
- Command mode can now always be cancelled with Esc without executing a command.
- Added Tab completion and Up/Down command history to `:` command mode, with inline suggestions.
- Added direct `:local`, `:queue`, and `:help` commands and clearer unknown-command feedback.
- Navidrome resume artwork now persists by stable coverArt identity rather than authentication-bearing URLs.
- Artwork cache keys for Navidrome ignore changing authentication salt/token values, making cached covers reusable across sessions.
- Existing 0.5.0 sessions without a stored coverArt id are hydrated safely in the background after startup.
- Kept the distinction between stopped/resumable and actively playing state; startup never autoplays.

## 0.6.1 — navigation & discovery polish

- Browse and Quick Find section headings are structural and no longer receive keyboard focus.
- Home/End and PageUp/PageDown navigation added for long lists.
- YouTube search adds lightweight local token-aware reranking while retaining original provider relevance as a tie-breaker.
- Universal Quick Find now publishes results progressively: local results immediately, then Navidrome, SR and YouTube as each source completes.
- `.` opens a context-aware Actions menu for the selected or currently playing item.
- Navidrome track actions can jump directly to the corresponding artist or album.
- Help/footer documentation updated for the new interaction model.

## 0.7.1 — Spotify Preview

- Added Spotify as a fully optional source; disabled by default so existing installations are unchanged.
- Added Authorization Code with PKCE using a local loopback callback; no Spotify password or client secret is stored by NPLAY.
- Added Spotify search across artists, albums, tracks and playlists.
- Added personal Spotify playlists, recently played, top tracks and top artists.
- Added Spotify Connect device discovery/selection and remote playback controls: play/pause, previous/next, seek and volume.
- Added Spotify items to Universal Quick Find without delaying local/Navidrome results.
- Added Spotify context actions for Save to Spotify Library and Add to Spotify Playlist.
- Spotify tracks can still be saved in NPLAY cross-source playlists.
- Spotify queue actions use Spotify's own playback queue when the selected item is from Spotify.
- Spotify playback is deliberately separate from mpv, ReplayGain and gapless processing; Spotify content is not extracted or modified.
- CAVA visualization is disabled during Spotify playback; artwork and transport remain available.
- Added Spotify state to diagnostics and source settings.
- Existing Local, Navidrome, SR, YouTube, queue, playlist, artwork, CAVA and playback paths are otherwise preserved.
