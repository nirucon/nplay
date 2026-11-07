#!/usr/bin/env bash
# Remove NPLAY. By default only the program is removed: the launcher, the
# application code and the desktop entry. Your data (library database,
# playlists, favorites, history, statistics, Spotify credentials), the
# configuration and the state are kept. `--purge` removes those too.
set -euo pipefail

purge=0
for arg in "$@"; do
  case "$arg" in
    --purge) purge=1 ;;
    -h|--help)
      echo "usage: ./uninstall.sh [--purge]"
      echo "  default  remove the program, keep data, configuration and state"
      echo "  --purge  also delete ~/.local/share/nplay, ~/.config/nplay, ~/.local/state/nplay and ~/.cache/nplay"
      exit 0 ;;
    *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

BIN="$HOME/.local/bin/nplay"
APP="$HOME/.local/lib/nplay"
DESKTOP="${XDG_DATA_HOME:-$HOME/.local/share}/applications/nplay.desktop"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/nplay"
CFG="${XDG_CONFIG_HOME:-$HOME/.config}/nplay"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/nplay"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/nplay"

# Stop a running NPLAY service and its receiver so nothing keeps playing from
# code that is about to disappear. Only this user's processes are matched.
pkill -u "$(id -u)" -f 'python3 -m nplay\.app' 2>/dev/null || :

rm -f "$BIN" "$DESKTOP"
rm -rf "$APP"
# Leftover upgrade directories from an interrupted install.
rm -rf "$APP".new.* "$APP".old.* 2>/dev/null || :

if [[ $purge == 1 ]]; then
  rm -rf "$DATA" "$CFG" "$STATE" "$CACHE"
  echo "NPLAY removed, including its data, configuration, state and cache."
else
  echo "NPLAY program removed. Kept your data, configuration and state:"
  for d in "$DATA" "$CFG" "$STATE"; do [[ -e $d ]] && echo "  $d"; done
  echo "Run ./uninstall.sh --purge to delete them as well."
fi
