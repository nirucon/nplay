#!/usr/bin/env bash
# NPLAY installer: one static binary in ~/.local/bin/nplay.
#
#   ./install.sh             install the prebuilt release binary (verified with SHA-256)
#   ./install.sh --build     build from this checkout with cargo
#   ./install.sh --prebuilt  use ./nplay beside this script (release tarball)
#   ./install.sh --yes       install missing packages without asking
#
# Upgrading from NPLAY 1.x (Python) replaces the old program (the venv-free
# code in ~/.local/lib/nplay, the python launcher and its desktop entry). Your
# data, configuration, secrets and state are kept and backed up first: the
# library database, statistics, playlists, favorites, history, Spotify
# credentials, config.ini and secrets.ini open unchanged.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
ASSUME_YES=0
MODE=auto
[[ ${NPLAY_ASSUME_YES:-${NIRUX_ASSUME_YES:-0}} == 1 ]] && ASSUME_YES=1
for arg in "$@"; do
 case "$arg" in
  -y|--yes) ASSUME_YES=1 ;;
  --build) MODE=build ;;
  --prebuilt) MODE=prebuilt ;;
  -h|--help) echo "usage: ./install.sh [--build|--prebuilt] [--yes]   (--yes installs missing packages without asking)"; exit 0 ;;
  *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
 esac
done
[[ $(id -u) != 0 ]] || { echo "Run ./install.sh as your own user, not root." >&2; exit 1; }

VERSION="${NPLAY_VERSION:-}"
[[ -z $VERSION && -f Cargo.toml ]] && VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ -z $VERSION && -f VERSION ]] && VERSION="$(sed -n '1p' VERSION)"
[[ -n $VERSION ]] || { echo "Cannot determine the NPLAY version (set NPLAY_VERSION)." >&2; exit 1; }

APPDEST="$HOME/.local/lib/nplay"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/nplay"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/nplay"
BIN="$HOME/.local/bin"; CFG="${XDG_CONFIG_HOME:-$HOME/.config}/nplay"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
B='\033[1m'; D='\033[2m'; G='\033[32m'; Y='\033[33m'; R='\033[31m'; N='\033[0m'
printf "${B}NPLAY ${VERSION}${N}\n${D}Terminal audio player · installer${N}\n\n"
if [[ -r /etc/os-release ]]; then . /etc/os-release; else ID=unknown; ID_LIKE=""; fi
id="${ID:-unknown}"; like=" ${ID_LIKE:-} "
if [[ "$id" =~ ^(arch|omarchy|cachyos|endeavouros)$ ]] || [[ "$like" == *" arch "* ]]; then PM=pacman
elif [[ "$id" =~ ^(void|voidlinux)$ ]]; then PM=xbps
elif [[ "$id" =~ ^(debian|ubuntu|linuxmint|pop)$ ]] || [[ "$like" == *" debian "* ]] || [[ "$like" == *" ubuntu "* ]]; then PM=apt
else PM=unknown
fi
if [[ $PM == unknown ]] && command -v xbps-install >/dev/null 2>&1; then PM=xbps; fi
if [[ $PM == unknown ]] && command -v pacman >/dev/null 2>&1; then PM=pacman; fi
if [[ $PM == unknown ]] && command -v apt-get >/dev/null 2>&1; then PM=apt; fi
printf "SYSTEM\n  %-16s %s\n" "Distribution" "${PRETTY_NAME:-${NAME:-unknown}}"; printf "  %-16s %s\n\n" "Package manager" "$PM"

# The binary is static: tags, artwork, MPRIS and HTTPS are built in. Only the
# media programs it drives are packages.
required=(); recommended=()
command -v mpv >/dev/null || required+=(mpv)
command -v cava >/dev/null || recommended+=(cava)
command -v yt-dlp >/dev/null || recommended+=(yt-dlp)
command -v notify-send >/dev/null || recommended+=(notify)
if ! command -v librespot >/dev/null 2>&1; then
 if [[ $PM == pacman ]] && pacman -Si librespot >/dev/null 2>&1; then recommended+=(librespot)
 elif [[ $PM == apt ]] && apt-cache show librespot >/dev/null 2>&1; then recommended+=(librespot)
 fi
fi
install_deps(){
 pkgs=("${required[@]}" "${recommended[@]}"); [[ ${#pkgs[@]} -gt 0 ]] || return 0
 [[ $PM != unknown ]] || { printf "${R}Automatic dependency installation is unavailable for this distribution.${N}\nMissing: %s\nInstall these packages, then rerun.\n" "${pkgs[*]}"; [[ ${#required[@]} -eq 0 ]] && return 0; exit 1; }
 mapped=(); for x in "${pkgs[@]}"; do
  case "$PM:$x" in pacman:notify|xbps:notify) mapped+=(libnotify);; apt:notify) mapped+=(libnotify-bin);; *) mapped+=("$x");; esac
 done
 printf "DEPENDENCIES\n"; for x in "${mapped[@]}"; do printf "  ${Y}•${N} %s\n" "$x"; done
 local yes=0
 [[ $ASSUME_YES == 1 ]] && yes=1
 if [[ $yes == 0 && ! -t 0 ]]; then
  if [[ ${#required[@]} -gt 0 ]]; then
   printf "${R}Required packages are missing and there is no terminal to ask for permission.${N}\nRerun in a terminal, or with --yes to install them.\n"; exit 1
  fi
  printf "${Y}No terminal to ask for permission; skipping the recommended packages above.${N}\nRerun in a terminal, or with --yes, to install them.\n"
  return 0
 fi
 if [[ $yes == 0 ]]; then
  printf "\nInstall missing dependencies using sudo? [Y/n] "; read -r ans; [[ ${ans:-Y} =~ ^[Yy]$ ]] || { printf "Installation cancelled.\n"; exit 1; }
 fi
 if [[ $PM == pacman ]]; then sudo pacman -S --needed --noconfirm "${mapped[@]}"; elif [[ $PM == xbps ]]; then sudo xbps-install -Sy "${mapped[@]}"; else sudo apt-get update; sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y "${mapped[@]}"; fi
}

# The binary: built here, shipped beside this script, or downloaded and verified.
TMP=""; trap 'rm -rf "$TMP"' EXIT
verify(){ # FILE SUMS
 command -v sha256sum >/dev/null || { echo "sha256sum is required to verify the release binary" >&2; exit 1; }
 expected=$(awk '$2 == "nplay" || $2 == "*nplay" { print $1; exit }' "$2")
 actual=$(sha256sum "$1" | awk '{print $1}')
 [[ -n $expected && $expected == "$actual" ]] || { printf "${R}The NPLAY binary failed SHA-256 verification.${N}\n" >&2; exit 1; }
 printf "  ${G}✓${N} SHA-256 verification passed\n"
}
if [[ $MODE == build ]]; then
 command -v cargo >/dev/null || { echo "Rust/cargo is required for --build" >&2; exit 1; }
 printf "Building NPLAY ${VERSION} from source…\n"
 cargo build --release --locked
 BIN_SRC=target/release/nplay
elif [[ -x ./nplay && -f ./nplay ]]; then
 BIN_SRC=./nplay
 [[ -f SHA256SUMS ]] && verify "$BIN_SRC" SHA256SUMS
elif [[ $MODE == prebuilt ]]; then
 echo "No ./nplay binary beside install.sh" >&2; exit 1
else
 [[ $(uname -m) == x86_64 ]] || { echo "No prebuilt release for $(uname -m); use ./install.sh --build" >&2; exit 1; }
 TMP=$(mktemp -d)
 URL="https://github.com/nirucon/nplay/releases/download/v${VERSION}"
 printf "Downloading the prebuilt NPLAY ${VERSION}…\n"
 if command -v curl >/dev/null; then
  curl -fsSL --retry 3 -o "$TMP/nplay" "$URL/nplay" && curl -fsSL --retry 3 -o "$TMP/SHA256SUMS" "$URL/SHA256SUMS" \
   || { echo "Could not download the release binary (no local build is attempted; try --build)." >&2; exit 1; }
 elif command -v wget >/dev/null; then
  wget -qO "$TMP/nplay" "$URL/nplay" && wget -qO "$TMP/SHA256SUMS" "$URL/SHA256SUMS" \
   || { echo "Could not download the release binary (no local build is attempted; try --build)." >&2; exit 1; }
 else
  echo "curl or wget is required to download the release binary" >&2; exit 1
 fi
 verify "$TMP/nplay" "$TMP/SHA256SUMS"
 BIN_SRC="$TMP/nplay"
fi
chmod +x "$BIN_SRC"
"$BIN_SRC" --version >/dev/null || { echo "The NPLAY binary does not run on this system." >&2; exit 1; }

install_deps
command -v mpv >/dev/null || { echo 'ERROR: mpv missing'; exit 1; }

printf "\nINSTALL\n"
mkdir -p "$BIN" "$CFG" "$DATA" "$STATE" "$APPS"
chmod 700 "$STATE" 2>/dev/null || :
# Small safety backup of irreplaceable persistent state before the upgrade.
STAMP="$(date +%Y%m%d-%H%M%S)"; BACKUP="$STATE/upgrades/$STAMP"
mkdir -p "$BACKUP"; chmod 700 "$STATE/upgrades" 2>/dev/null || :
[[ -f "$DATA/library.db" ]] && cp -a "$DATA/library.db" "$BACKUP/library.db"
[[ -f "$DATA/statistics.sqlite3" ]] && cp -a "$DATA/statistics.sqlite3" "$BACKUP/statistics.sqlite3"
[[ -d "$DATA/spotify-local" ]] && cp -a "$DATA/spotify-local" "$BACKUP/spotify-local"
[[ -f "$CFG/config.ini" ]] && cp -a "$CFG/config.ini" "$BACKUP/config.ini"
[[ -f "$CFG/secrets.ini" ]] && cp -a "$CFG/secrets.ini" "$BACKUP/secrets.ini"
[[ -f "$STATE/session.json" ]] && cp -a "$STATE/session.json" "$BACKUP/session.json"

# Stop a running NPLAY service so the next client starts the new one. Only this
# user's headless services are stopped (SIGTERM is a graceful stop that saves
# the session); an open terminal NPLAY keeps playing until you quit it.
pkill -TERM -u "$(id -u)" -f 'python3 -m nplay\.app .*--serve' 2>/dev/null || :
pkill -TERM -u "$(id -u)" -f '(^|/)nplay --serve' 2>/dev/null || :

# Atomic replace of the launcher (the Python 1.x launcher was a shell script).
install -m 0755 "$BIN_SRC" "$BIN/nplay.new.$$"
mv -f "$BIN/nplay.new.$$" "$BIN/nplay"
# Remove the Python 1.x program code; never data.
if [[ -d $APPDEST ]]; then
 rm -rf "$APPDEST" "$APPDEST".new.* "$APPDEST".old.* 2>/dev/null || :
 printf "  ${G}✓${N} Removed the Python NPLAY 1.x program ($APPDEST)\n"
fi
[[ -f "$CFG/config.ini" ]] || { [[ -f "$ROOT/config.example.ini" ]] && cp "$ROOT/config.example.ini" "$CFG/config.ini" && chmod 600 "$CFG/config.ini"; } || :
cat > "$APPS/nplay.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=NPLAY
Comment=Terminal-native music player
Exec=$BIN/nplay
Terminal=true
Categories=Audio;AudioVideo;Player;
EOF
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" >/dev/null 2>&1 || :
printf "  ${G}✓${N} Program: $BIN/nplay ($("$BIN/nplay" --version))\n  ${G}✓${N} Persistent data preserved: $DATA\n  ${G}✓${N} Configuration preserved: $CFG\n  ${G}✓${N} Pre-upgrade state backup: $BACKUP\n"
printf "\nCAPABILITIES\n  %-16s %s\n" "mpv" "$(command -v mpv >/dev/null && echo ready || echo missing)"; printf "  %-16s %s\n" "CAVA" "$(command -v cava >/dev/null && echo ready || echo unavailable)"; printf "  %-16s %s\n" "Spotify" "Web API + local librespot"; printf "  %-16s %s\n" "librespot" "$(command -v librespot >/dev/null && echo ready || echo optional / install for local Spotify)"; printf "  %-16s %s\n" "YouTube" "$(command -v yt-dlp >/dev/null && echo ready || echo unavailable)"; printf "  %-16s %s\n" "Artwork" "kitty graphics protocol (kitty, NIRUTERM), text fallback elsewhere"; printf "  %-16s %s\n" "MPRIS2" "built in (needs a D-Bus session)"; printf "  %-16s %s\n" "Notifications" "$(command -v notify-send >/dev/null && echo available || echo optional / unavailable)"
printf "\n${B}NPLAY is ready.${N}\nRun: nplay\nDoctor: nplay --doctor\n"
