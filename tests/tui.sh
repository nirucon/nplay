#!/bin/sh
# End-to-end test of the NPLAY 2 terminal UI through a pseudo terminal.
#
# Usage: tests/tui.sh [path/to/nplay]   (default: target/release/nplay)
#
# Sandboxed XDG dirs, no network sources, tools/fake-mpv as mpv and
# tests/fake_cava.py as cava, a few generated wav files. The python3 harness
# keeps a small VT screen model and checks: first full frame time, idle CPU,
# browsing local music, playing a track (status/transport), pause, next,
# the `:` command prompt, quitting with Q, and SIGTERM -> clean exit.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${1:-$ROOT/target/release/nplay}
BIN=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")
SB=$(mktemp -d)
trap 'rm -rf "$SB"' EXIT
export HOME="$SB/home" XDG_CONFIG_HOME="$SB/config" XDG_DATA_HOME="$SB/data" \
    XDG_STATE_HOME="$SB/state" XDG_CACHE_HOME="$SB/cache" XDG_RUNTIME_DIR="$SB/run"
mkdir -p "$HOME/Music/Album" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/nplay" "$SB/bin"
chmod 700 "$XDG_RUNTIME_DIR"
unset DBUS_SESSION_BUS_ADDRESS DISPLAY WAYLAND_DISPLAY KITTY_WINDOW_ID TERM_PROGRAM 2>/dev/null || true
ln -s "$ROOT/tools/fake-mpv" "$SB/bin/mpv"
printf '#!/bin/sh\nexec python3 "%s/tests/fake_cava.py" "$@"\n' "$ROOT" > "$SB/bin/cava"
chmod +x "$SB/bin/cava"
export PATH="$SB/bin:$PATH" FAKE_CAVA_MUSIC_FRAMES=100000
python3 - "$HOME/Music/Album" <<'PY'
import sys, wave
for i in range(3):
    w = wave.open(f"{sys.argv[1]}/{i+1:02d} Song {chr(65+i)}.wav", "wb")
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000)
    w.writeframes(b"\0\0" * 8000 * 30); w.close()
PY
cat > "$XDG_CONFIG_HOME/nplay/config.ini" <<CFG
[general]
music_dirs = $HOME/Music
youtube_enabled = false
sr_enabled = false
navidrome_enabled = false
mpris_enabled = false
track_notifications = false
auto_library_refresh = false
CFG
"$BIN" --scan >/dev/null
python3 - "$BIN" <<'PY'
import os, pty, sys, time, select, struct, fcntl, termios, signal, unicodedata, codecs, re

ROWS, COLS = 40, 120
BIN = sys.argv[1]
failures = []

class Screen:
    """Just enough VT100 for ratatui/crossterm output."""
    def __init__(s):
        s.g = [[" "] * COLS for _ in range(ROWS)]; s.y = s.x = 0; s.raw = b""
        s.dec = codecs.getincrementaldecoder("utf-8")("replace"); s.pending = ""
    def feed(s, data):
        s.raw += data
        text = s.pending + s.dec.decode(data); i = 0; n = len(text)
        while i < n:
            c = text[i]
            if c == "\x1b":
                if i + 1 >= n: break
                k = text[i + 1]
                if k == "[":
                    m = re.match(r"\x1b\[([?>]?)([0-9;]*)([ -/]*)([@-~])", text[i:])
                    if not m: break
                    s.csi(m.group(1), m.group(2), m.group(4)); i += m.end(); continue
                if k in "]_P^":
                    j = text.find("\x1b\\", i + 2); b = text.find("\x07", i + 2) if k == "]" else -1
                    ends = [e for e in (j, b) if e >= 0]
                    if not ends: break
                    e = min(ends); i = e + (2 if e == j else 1); continue
                i += 2; continue
            if c == "\r": s.x = 0
            elif c == "\n": s.y = min(ROWS - 1, s.y + 1)
            elif c == "\b": s.x = max(0, s.x - 1)
            elif ord(c) >= 32:
                if unicodedata.combining(c): i += 1; continue
                wdt = 2 if unicodedata.east_asian_width(c) in "WF" else 1
                if s.x < COLS and s.y < ROWS:
                    s.g[s.y][s.x] = c
                    if wdt == 2 and s.x + 1 < COLS: s.g[s.y][s.x + 1] = ""
                s.x += wdt
            i += 1
        s.pending = text[i:]
    def csi(s, priv, params, f):
        p = [int(x) if x else 0 for x in params.split(";")] if params else []
        a = lambda k, d=1: (p[k] if len(p) > k and p[k] else d)
        if priv: return
        if f in "Hf": s.y = min(ROWS - 1, a(0) - 1); s.x = min(COLS - 1, a(1) - 1)
        elif f == "J":
            if a(0, 0) == 2 or a(0, 0) == 3: s.g = [[" "] * COLS for _ in range(ROWS)]
            elif a(0, 0) == 0:
                s.g[s.y][s.x:] = [" "] * (COLS - s.x)
                for r in range(s.y + 1, ROWS): s.g[r] = [" "] * COLS
        elif f == "K":
            if a(0, 0) == 0: s.g[s.y][s.x:] = [" "] * (COLS - s.x)
            elif a(0, 0) == 2: s.g[s.y] = [" "] * COLS
        elif f == "A": s.y = max(0, s.y - a(0))
        elif f == "B": s.y = min(ROWS - 1, s.y + a(0))
        elif f == "C": s.x = min(COLS - 1, s.x + a(0))
        elif f == "D": s.x = max(0, s.x - a(0))
        elif f == "G": s.x = min(COLS - 1, a(0) - 1)
        elif f == "d": s.y = min(ROWS - 1, a(0) - 1)
    def text(s): return "\n".join("".join(r) for r in s.g)
    def row(s, r): return "".join(s.g[r if r >= 0 else ROWS + r])

def start(env={}):
    t0 = time.monotonic()
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ.update(env)
        os.execv(BIN, [BIN])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    return pid, fd, t0

scr = Screen()
pid, fd, t0 = start()

def pump(secs, once=False):
    end = time.monotonic() + secs
    while True:
        left = end - time.monotonic()
        if left <= 0: return
        r, _, _ = select.select([fd], [], [], left)
        if r:
            try: d = os.read(fd, 65536)
            except OSError: return
            if not d: return
            scr.feed(d)
            if once: return

def wait_for(pred, secs, what):
    end = time.monotonic() + secs
    while time.monotonic() < end:
        if pred(): return True
        pump(0.05, once=True)
    if pred(): return True
    failures.append(what)
    print("FAIL:", what); print(scr.text())
    return False

def has(s): return lambda: s in scr.text()
def send(b): os.write(fd, b); pump(0.15)

first = None
if wait_for(lambda: "? help" in scr.row(-1), 5, "first frame with footer help"):
    first = (time.monotonic() - t0) * 1000
print(f"first full frame: {first:.0f} ms" if first else "first frame: none")
wait_for(has("Nothing playing"), 2, "idle home view")

# Idle: no CPU, no wakeups.
pump(0.5)
def cpu(): f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split(); return int(f[11]) + int(f[12])
def switches():
    n = 0
    for t in os.listdir(f"/proc/{pid}/task"):
        for l in open(f"/proc/{pid}/task/{t}/status"):
            if "ctxt_switches" in l: n += int(l.split()[1])
    return n
c0, s0 = cpu(), switches(); pump(5); c1, s1 = cpu(), switches()
rss = [l for l in open(f"/proc/{pid}/status") if l.startswith("VmRSS")][0].split()[1]
print(f"idle 5 s: {c1 - c0} CPU ticks, {s1 - s0} context switches, RSS {rss} kB")
if c1 - c0 > 2: failures.append(f"idle CPU {c1 - c0} ticks")
if s1 - s0 > 25: failures.append(f"idle wakeups {s1 - s0}")

# Browse local music: b -> BROWSE (LOCAL MUSIC selected) -> ALL TRACKS.
send(b"b"); wait_for(has("BROWSE"), 3, "browse menu")
wait_for(lambda: "› LOCAL MUSIC" in scr.text(), 2, "LOCAL MUSIC selected")
send(b"\r"); wait_for(lambda: "› ALL TRACKS" in scr.text(), 3, "local music menu")
send(b"\r"); wait_for(has("Song A"), 3, "local track list")
send(b"j"); wait_for(lambda: "› " in scr.text() and "Song B" in scr.text(), 2, "selection moves")
send(b"k")
# Filter the list.
send(b"/"); wait_for(has("FILTER CURRENT LIST"), 2, "filter prompt")
send(b"song c\r"); wait_for(lambda: "Song C" in scr.text() and "Song A" not in scr.text(), 3, "filtered list")
send(b"/"); send(b"\r"); wait_for(has("Song A"), 2, "filter cleared")

# Play the first track: Now Playing opens with the status line.
send(b"g"); send(b"\r")
wait_for(lambda: "Playing" in scr.row(-2) and "Song A" in scr.row(-2), 5, "playing Song A (status line)")
send(b"b")
wait_for(lambda: "▶" in scr.row(-2) and "Song A" in scr.row(-2), 3, "transport line under lists")
send(b"\x0e")  # Ctrl+N: Now Playing
wait_for(lambda: "Song A" in scr.row(4) and "●" in scr.row(10), 3, "now playing view")
def position():
    m = re.search(r"(\d+):(\d\d)   (\d+):(\d\d)", scr.text())
    return int(m.group(1)) * 60 + int(m.group(2)) if m else None
wait_for(lambda: (position() or 0) >= 2, 5, "progress advances while playing")
wait_for(lambda: any(ch in scr.text() for ch in "▁▂▃▄▅▆▇█"), 3, "visualizer bars while playing")
c0 = cpu(); pump(3); c1 = cpu()
print(f"playing with visualizer: {(c1 - c0) / 3:.1f} CPU ticks/s (100 = one core)")
send(b" ")  # pause
pump(0.5); p1 = position(); pump(1.6); p2 = position()
print(f"paused at {p1}, after 1.6 s {p2}")
if p1 is None or p1 != p2: failures.append(f"pause did not stop the clock ({p1} -> {p2})")
send(b" ")  # resume
send(b"n")
wait_for(lambda: "Song B" in scr.row(4), 5, "next track Song B")

# Command prompt with completion.
send(b":"); wait_for(lambda: ":" in scr.row(-2), 2, "command prompt")
send(b"vi\t"); wait_for(lambda: "view" in scr.row(-2), 2, "tab completes view")
send(b" artwork\r"); wait_for(has("View · Artwork"), 3, ":view artwork status")
send(b":stats\r"); wait_for(has("STATISTICS"), 3, ":stats opens statistics")
send(b"\x1b"); send(b"?"); wait_for(has("HELP · KEYBINDINGS"), 2, "help view")
send(b"\x1b"); send(b":"); send(b"\x1b"); wait_for(has("Command cancelled"), 2, "Esc cancels prompt")

# Quit cleanly.
send(b"Q")
code = None
end = time.monotonic() + 5
while time.monotonic() < end:
    pump(0.05)
    r = os.waitpid(pid, os.WNOHANG)
    if r[0]: code = os.waitstatus_to_exitcode(r[1]); break
print("exit code after Q:", code)
if code != 0: failures.append(f"Q exit code {code}")
if b"\x1b[?1049l" not in scr.raw: failures.append("alternate screen not left")
os.close(fd)

# SIGTERM -> orderly exit (main runs App::shutdown, exit code 0).
scr = Screen(); pid, fd, t0 = start()
wait_for(lambda: "? help" in scr.row(-1), 5, "second start")
os.kill(pid, signal.SIGTERM)
code = None; end = time.monotonic() + 5
while time.monotonic() < end:
    pump(0.05)
    r = os.waitpid(pid, os.WNOHANG)
    if r[0]: code = os.waitstatus_to_exitcode(r[1]); break
print("exit code after SIGTERM:", code)
if code != 0: failures.append(f"SIGTERM exit code {code}")
if b"\x1b[?1049l" not in scr.raw: failures.append("SIGTERM: terminal not restored")

# Kitty graphics terminal: cover placeholder in Now Playing, and resizing.
scr = Screen(); pid, fd, t0 = start({"KITTY_WINDOW_ID": "1"})
wait_for(lambda: "? help" in scr.row(-1), 5, "kitty start")
# The session restored the last track, shown with its cover placeholder.
wait_for(has("│  LOCAL   │"), 5, "kitty: cover placeholder on Now Playing")
wait_for(has("Resume with SPACE"), 2, "kitty: restored session offers resume")
ROWS, COLS = 24, 80
scr.g = [[" "] * COLS for _ in range(ROWS)]
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
os.kill(pid, signal.SIGWINCH)
wait_for(lambda: "SPACE resume" in scr.row(-1) and "Song B" in scr.text(), 3, "redraw after resize to 80x24")
send(b"Q")
end = time.monotonic() + 5; code = None
while time.monotonic() < end:
    pump(0.05)
    r = os.waitpid(pid, os.WNOHANG)
    if r[0]: code = os.waitstatus_to_exitcode(r[1]); break
if code != 0: failures.append(f"kitty run exit code {code}")

if failures:
    print("FAILED:", "; ".join(failures)); sys.exit(1)
print("tui.sh: all checks passed")
PY
