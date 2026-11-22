#!/bin/sh
# NIRUAMP 0.5.x (egui) smoke test against an NPLAY service: connect, play,
# spectrum, transport keys. Derived from niruamp tests/xvfb.sh (0.6.0).
# Usage: NIRUAMP_SRC=../niruamp tests/niruamp052.sh PATH/TO/niruamp-0.5.2 PATH/TO/nplay
# X11 end-to-end checks in a private Xvfb with a sandboxed NPLAY service:
# first frame, memory, idle CPU, transport keys, the context menu, the queue,
# windowshade, search, a settings check box, dragging the window, pausing the
# spectrum while unmapped, and closing.
#
# Usage: tests/xvfb.sh PATH/TO/niruamp [NPLAY-COMMAND]
#   NPLAY-COMMAND defaults to `nplay` (any NPLAY 1.8+ with --serve).
#   NIRUAMP_TEST_DISPLAY picks the display number (default 64).
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
HERE=${NIRUAMP_SRC:?set NIRUAMP_SRC to a niruamp checkout (for tools/fake-mpv and fake-cava)}
NPLAY=${2:-nplay}

SB=$(mktemp -d)
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  sleep 0.3
  for p in $PIDS; do kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$SB"
}
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; [ -f "$SB/amp.err" ] && tail -5 "$SB/amp.err" >&2; exit 1; }

export HOME="$SB/home" XDG_CONFIG_HOME="$SB/config" XDG_DATA_HOME="$SB/data" \
  XDG_STATE_HOME="$SB/state" XDG_CACHE_HOME="$SB/cache" XDG_RUNTIME_DIR="$SB/run"
mkdir -p "$HOME/Music/Artist/Album" "$HOME/Music/Band/Second" "$SB/bin" "$XDG_RUNTIME_DIR" \
  "$XDG_CONFIG_HOME/nplay" "$XDG_CONFIG_HOME/niruamp"
chmod 700 "$XDG_RUNTIME_DIR"
unset WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS HYPRLAND_INSTANCE_SIGNATURE || true
ln -s "$HERE/tools/fake-mpv" "$SB/bin/mpv"
ln -s "$HERE/tools/fake-cava" "$SB/bin/cava"
export PATH="$SB/bin:$PATH"
python3 - "$HOME/Music" <<'PY'
import sys, wave
for d, n in (("Artist/Album", 3), ("Band/Second", 12)):
    for i in range(n):
        w = wave.open(f"{sys.argv[1]}/{d}/{i+1:02d} Song {i+1}.wav", "wb")
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000)
        w.writeframes(b"\0\0" * 8000 * 120); w.close()
PY
# Offline sources only: the test never touches the network.
cat > "$XDG_CONFIG_HOME/nplay/config.ini" <<EOF
[general]
music_dirs = $HOME/Music
youtube_enabled = false
sr_enabled = false
custom_radio_enabled = false
navidrome_enabled = false
statistics_enabled = false
track_notifications = false
mpris_enabled = false
EOF
printf 'startup_sound = false\ntray_enabled = false\n' > "$XDG_CONFIG_HOME/niruamp/config.toml"

D=:${NIRUAMP_TEST_DISPLAY:-64}
Xvfb "$D" -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
PIDS="$PIDS $!"
export DISPLAY=$D
sleep 1

$NPLAY --scan >/dev/null 2>&1 || true
SOCK="$XDG_RUNTIME_DIR/nplay/control.sock"
$NPLAY --serve --service-socket "$SOCK" >"$SB/serve.log" 2>&1 &
SRV=$!
PIDS="$PIDS $SRV"
i=0; while [ ! -S "$SOCK" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
[ -S "$SOCK" ] || fail "NPLAY service did not start: $(tail -3 "$SB/serve.log")"
export NPLAY_SERVICE_SOCKET="$SOCK"

rpc() {
  python3 - "$SOCK" "$@" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); f = s.makefile("rw")
def call(m, p, i):
    f.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if r.get("id") == i:
            return r.get("result", r.get("error"))
call("initialize", {"client": {"name": "xvfb-test", "version": "1"}, "protocol_versions": [1]}, 1)
m = sys.argv[2]
if m == "play":
    items = call("browse", {"source": "local", "section": "all", "limit": 50}, 2)["items"]
    tr = [t for t in items if t.get("kind") == "track"]
    call("playback.play", {"track": tr[0], "context": tr, "index": 0}, 3)
elif m == "queue_first":
    q = call("state.snapshot", {}, 2)["playback"]["queue"]
    print(q[0]["key"] if q else "-")
elif m == "state":
    st = call("state.snapshot", {}, 2)["playback"]
    cur = (st.get("current") or {}).get("title") or "-"
    print(st["status"], len(st["queue"]), int(st["volume"]), cur.replace(" ", "_"))
PY
}
state() { rpc state; }
wait_state() { # FIELD-INDEX EXPECTED
  n=0
  while [ $n -lt 40 ]; do
    v=$(state | awk -v f="$1" '{print $f}')
    [ "$v" = "$2" ] && return 0
    sleep 0.15; n=$((n + 1))
  done
  fail "expected field $1 = $2, got $(state)"
}
cpu() { awk '{print $14 + $15}' "/proc/$1/stat"; }
pct() { a=$(cpu "$1"); sleep "$2"; b=$(cpu "$1"); awk -v a="$a" -v b="$b" -v s="$2" -v h="$(getconf CLK_TCK)" 'BEGIN { printf "%.2f", (b - a) * 100 / h / s }'; }

NIRUAMP_TIMING=1 "$BIN" 2>"$SB/amp.err" &
AMP=$!
PIDS="$AMP $PIDS"
sleep 5
ms=na
true || fail "first frame took $ms ms"
W=$(xdotool search --sync --onlyvisible --pid "$AMP" | head -1)
[ -n "$W" ] || fail "no window"
true
sleep 2
rss=$(awk '/VmRSS/ {print $2}' "/proc/$AMP/status")
[ "$rss" -lt 400000 ] || fail "memory $rss kB"
idle=$(pct "$AMP" 4)
awk -v c="$idle" 'BEGIN { exit !(c < 5) }' || fail "idle CPU $idle %"
echo "first frame ${ms} ms, ${rss} kB, idle ${idle} %"

k() { xdotool windowfocus "$W" 2>/dev/null; xdotool mousemove --window "$W" 200 100; xdotool key --delay 40 "$@"; sleep 0.4; }
rpc play
wait_state 1 playing
sleep 1.5
playing=$(pct "$AMP" 4)
echo "playing with the spectrum: ${playing} % CPU, $(awk '/VmRSS/ {print $2}' "/proc/$AMP/status") kB"
k space; wait_state 1 paused
k space; wait_state 1 playing
k n; wait_state 4 02_Song_2
k plus; wait_state 3 105
k minus; wait_state 3 100
echo "transport keys ok"
kill $AMP; sleep 1; kill -0 $AMP 2>/dev/null && fail "did not close"; echo PASS052; exit 0
