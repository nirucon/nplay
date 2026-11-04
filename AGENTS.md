# Agent assignment: NPLAY 1.8.0, a one-shot security, efficiency and service update

## 0. Quick start

1. Read this file, `README.md`, `ARCHITECTURE.md`, `CONFIGURATION.md`, `nplay/player.py`,
   `nplay/providers.py`, `nplay/service/backend.py`, `nplay/service/server.py` and the
   tests. The audit baseline is `2cfaa4c` (1.7.3). Revalidate each item against current
   HEAD and skip items that are already fixed, noting that you did.
2. Implement **P1–P6, then U1–U3 in order**. Commit per item. Keep
   `python3 -m compileall -q nplay` and `python3 -m unittest discover -s tests` green
   (82 tests at baseline).
3. Pass the gates in §4, release in §5 and report in §6.
4. **Order with NIRUAMP:** the NIRUAMP 0.4.0 assignment (nirucon/niruamp `AGENTS.md`)
   uses the visualization stream from P3. Release NPLAY first and keep the old
   `visualization.data` request working, so that the released NIRUAMP 0.3.x keeps working.

This is an **execution** assignment. Do not stop after a plan or a branch. Make sensible
decisions, document them, and continue without routine questions.

## 1. Invariants

- NPLAY stays a complete standalone terminal player. It needs no X11, NIRUX or NIRUAMP.
- One backend session has one authoritative state and one MPRIS owner. No double audio
  and no duplicate listening events.
- User data, configuration and secrets are preserved on upgrade (see `INSTALL.md`). Never
  delete the database or the statistics queue.
- Python and package requirements stay as today (`install.sh`). Add no new required
  dependency.
- Control protocol: bump the minor version only for additions, and keep existing
  requests working.

## 2. Findings (verified 2026-11-04 on niruxwork, read-only on the running service)

### P1: The Subsonic credential leaks into argv and logs (high, security)
`Navidrome.stream()` (`nplay/providers.py:56`) builds
`/rest/stream.view?u=…&t=<token>&s=<salt>…`. `player.py:21` appends that URL to **mpv's
argv**, so any local user can read it from `/proc/<pid>/cmdline` or `ps`. It also **logs
it**: `log.info('mpv start source=%s', url[:240])`. On niruxwork, `nplay.log` contained
**318** lines and `mpv.log` 4 lines with the auth parameters, both with mode 0664. A
token+salt pair can be replayed until the password changes.
**Fix:**
1. Never put a URL with credentials in argv. Start mpv idle (it already uses `--idle=yes`)
   and send `loadfile` over the IPC socket, which is 0600 in a private directory (verify
   that).
2. Add one `redact_url()` helper that removes the `t`, `s`, `p`, `token`, `apiKey` and
   `access_token` query values, and use it in **every** log call that can contain a source
   URL: player, providers and service.
3. Create log files with mode 0600, and the state directory with 0700.
4. One-time upgrade migration: redact the existing auth parameters in `nplay.log`,
   rotated logs and `mpv.log` in place (write to a temp file, then rename). Log that the
   migration ran, without the secret.
5. Add a unit test that fails if a redaction target appears in any log record during a
   simulated Navidrome play.

### P2: The visualization is polled at 30 Hz through JSON (efficiency)
While NIRUAMP shows the spectrum, it sends a `visualization.data` request every 33 ms.
Measured live while playing: the NPLAY service used about **1.8 % CPU at ~170
wakeups/s**, `cava` about **1.4 %**, and mpv's actual playback only **0.3 %**. Stopping
is sound: CAVA stops after 15 s without a request. But request/response JSON at 30 Hz is
the expensive part.
**Fix:** add `visualization.subscribe {bars, fps≤30}`. The service pushes compact frames
to that client as one JSON line `{"vis":[…]}`, or as a base64 byte array if that is
cheaper, and measure which one is. Then:
- stop pushing while the frame is identical to the previous one (silence),
- `visualization.unsubscribe` ends the stream, and so does a client disconnect,
- stop CAVA as soon as there are 0 subscribers and no recent `data` request.

Keep `visualization.data` unchanged for old clients, and advertise the capability in the
handshake/capabilities.

### P3: The capability flag for P2
Expose `visualization_stream: true` and the protocol minor version, so that NIRUAMP can
choose. Document the protocol in `docs/` or `ARCHITECTURE.md`.

### P4: The service shutdown sleep
`shutdown()` sleeps a fixed 0.2 s. That is harmless, but check the server loop for other
fixed-interval sleeps or polls while idle (accept loop, monitor threads). With no
client and nothing playing, the service should wake ≤ 1/s until `--idle-exit`.

### P5: The CAVA config file is left in /tmp
`cava -p /tmp/nplay-cava-*.conf` is written to `/tmp`. Write it to
`$XDG_RUNTIME_DIR/nplay/` (0700) instead, and remove it when CAVA stops.

### P6: Old branch
The remote has `chore/mit-license-2026`. Check whether it is merged into `main`. If it
is, delete it. If not, leave it and mention it in the report. Do not merge it.

## 3. UX (terminal UI)

### U1: Clearer Navidrome errors
When Navidrome authentication or a stream fails, show the HTTP status and a remedy
("check the URL and password in Settings → Navidrome"), never the URL with parameters.

### U2: `nplay --doctor` checks privacy
Add checks for log and state permissions, credential-free logs, and the location of the
CAVA config, with a one-line fix for each warning.

### U3: An "about" line for the visualizer
In the TUI status area, show "VIS: CAVA, n subscribers" in debug or status views, so
that NIRUAMP's effect on CPU is understandable. Keep it hidden by default.

Out of scope: new providers, statistics changes, and Spotify transport changes.

## 4. Gates

Automated:
- `compileall`, `unittest` (all of them), and the CI workflow `tests.yml` green.
- New tests:
  - P1 argv and IPC loading (mock mpv),
  - redaction (table of providers),
  - the log migration on a fixture log,
  - P2 subscribe/unsubscribe/disconnect,
  - silence suppression,
  - CAVA stopping with 0 subscribers (fake cava binary),
  - the compatibility of `visualization.data`.

Native, on **niruxwork** (the owner allows local testing; NPLAY and NIRUAMP are installed
and running):
- Play Navidrome, a local file, radio and YouTube.
- Confirm that `ps -o args= -C mpv` contains no credential.
- Confirm that `grep -c 'stream.view?u=' ~/.local/state/nplay/*.log` gives 0 after the
  migration.
- Measure the CPU and wakeups of the service, cava and mpv while NIRUAMP 0.3.x
  (old poll) shows the spectrum, and again with the new subscription (after NIRUAMP
  0.4.0, if it is available in the same session).
- Do not change the owner's library, playlists, statistics or credentials.

## 5. Release

Bump to **1.8.0**, update `CHANGELOG.md` and the docs, tag the release through the normal
workflow and verify the artifacts. NPLAY is a NIRUX extra (`extras-selected`). Update its
pin in `nirucon/nirux` through the extras mechanism used before (check the nirux git log
for "extras: pin"). Install on niruxwork with the normal extras update and verify that
the running service is the new version.

## 6. Final report

For each item P1–P6 and U1–U3: the change, the tests and the evidence (argv and log check
results, CPU and wakeups before and after). Include release URLs, the installed version
and the compatibility notes for NIRUAMP. Mark anything unfinished as **PARTIAL** and give
the exact remaining steps.
