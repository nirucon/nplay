#!/bin/sh
# install.sh replaces a Python NPLAY 1.x install and keeps every user file;
# uninstall.sh removes the program and keeps data unless --purge.
# Usage: tests/install.sh PATH/TO/nplay
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
HERE=$(cd "$(dirname "$0")/.." && pwd)
SB=$(mktemp -d)
trap 'rm -rf "$SB"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
H="$SB/home"
mkdir -p "$H/.local/bin" "$H/.local/lib/nplay/nplay" "$H/.local/share/nplay/spotify-local/cache" \
  "$H/.config/nplay" "$H/.local/state/nplay" "$H/.local/share/applications" "$SB/pkg" "$SB/fakebin"
printf '#!/usr/bin/env bash\nexec python3 -m nplay.app "$@"\n' > "$H/.local/bin/nplay"
echo "__version__='1.8.2'" > "$H/.local/lib/nplay/nplay/__init__.py"
echo lib > "$H/.local/share/nplay/library.db"
echo stats > "$H/.local/share/nplay/statistics.sqlite3"
echo cred > "$H/.local/share/nplay/spotify-local/cache/credentials.json"
printf '[general]\ntheme = niru-noir\n' > "$H/.config/nplay/config.ini"
printf '[navidrome]\npassword = x\n' > "$H/.config/nplay/secrets.ini"
echo '{}' > "$H/.local/state/nplay/session.json"
# A release directory: the binary, the scripts and checksums.
cp "$BIN" "$SB/pkg/nplay"
cp "$HERE/install.sh" "$HERE/uninstall.sh" "$HERE/config.example.ini" "$SB/pkg/"
(cd "$SB/pkg" && sha256sum nplay > SHA256SUMS)
# mpv must exist for the installer; a stub is enough here.
printf '#!/bin/sh\nexit 0\n' > "$SB/fakebin/mpv"; chmod +x "$SB/fakebin/mpv"
for t in cava yt-dlp notify-send librespot; do printf '#!/bin/sh\nexit 0\n' > "$SB/fakebin/$t"; chmod +x "$SB/fakebin/$t"; done
run() { env -i HOME="$H" PATH="$SB/fakebin:/usr/bin:/bin" TERM=dumb "$@" </dev/null; }
run bash "$SB/pkg/install.sh" --prebuilt >"$SB/out" 2>&1 || { cat "$SB/out"; fail "install failed"; }
[ -x "$H/.local/bin/nplay" ] || fail "no binary"
"$H/.local/bin/nplay" --version | grep -q '^NPLAY 2\.' || fail "wrong binary: $("$H/.local/bin/nplay" --version)"
[ ! -e "$H/.local/lib/nplay" ] || fail "old Python code kept"
for f in .local/share/nplay/library.db .local/share/nplay/statistics.sqlite3 \
  .local/share/nplay/spotify-local/cache/credentials.json .config/nplay/config.ini \
  .config/nplay/secrets.ini .local/state/nplay/session.json; do
  [ -f "$H/$f" ] || fail "lost $f"
done
grep -q 'theme = niru-noir' "$H/.config/nplay/config.ini" || fail "config changed"
ls "$H/.local/state/nplay/upgrades/"*/library.db >/dev/null || fail "no backup"
grep -q "Exec=$H/.local/bin/nplay" "$H/.local/share/applications/nplay.desktop" || fail "desktop entry"
# A tampered binary is refused.
echo junk >> "$SB/pkg/nplay"
run bash "$SB/pkg/install.sh" --prebuilt >"$SB/out" 2>&1 && fail "tampered binary installed"
grep -q 'SHA-256' "$SB/out" || fail "no checksum message"
run bash "$SB/pkg/uninstall.sh" >"$SB/out" 2>&1 || fail "uninstall failed"
[ ! -e "$H/.local/bin/nplay" ] || fail "binary kept"
[ -f "$H/.local/share/nplay/library.db" ] || fail "uninstall removed data"
run bash "$SB/pkg/uninstall.sh" --purge >"$SB/out" 2>&1 || fail "purge failed"
[ ! -e "$H/.local/share/nplay" ] || fail "purge kept data"
echo "install/uninstall ok"
