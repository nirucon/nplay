#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# Code and persistent data MUST be separate. Older installers placed both below
# ~/.local/share/nplay and atomically replaced that directory on upgrade, which
# could delete library.db and librespot credentials. 1.3.5 fixed that layout.
# Application code is deliberately outside XDG_DATA_HOME. Do not canonicalize
# this path via `cd` before it exists: on a first 1.3.x install that can turn
# an empty dirname into `/nplay` and require root privileges.
APPDEST="$HOME/.local/lib/nplay"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/nplay"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/nplay"
BIN="$HOME/.local/bin"; CFG="${XDG_CONFIG_HOME:-$HOME/.config}/nplay"
B='\033[1m'; D='\033[2m'; G='\033[32m'; Y='\033[33m'; R='\033[31m'; N='\033[0m'
printf "${B}NPLAY 1.5.5${N}\n${D}Terminal audio player · installer${N}\n\n"
if [[ -r /etc/os-release ]]; then . /etc/os-release; else ID=unknown; ID_LIKE=""; fi
id="${ID:-unknown}"; like=" ${ID_LIKE:-} "
if [[ "$id" =~ ^(arch|omarchy|cachyos|endeavouros)$ ]] || [[ "$like" == *" arch "* ]]; then
 PM=pacman
elif [[ "$id" =~ ^(void|voidlinux)$ ]]; then
 PM=xbps
elif [[ "$id" =~ ^(debian|ubuntu|linuxmint|pop)$ ]] || [[ "$like" == *" debian "* ]] || [[ "$like" == *" ubuntu "* ]]; then
 PM=apt
else
 PM=unknown
fi
# Runtime detection wins over distro branding (important for Omarchy and derivatives).
if [[ $PM == unknown ]] && command -v xbps-install >/dev/null 2>&1; then PM=xbps; fi
if [[ $PM == unknown ]] && command -v pacman >/dev/null 2>&1; then PM=pacman; fi
if [[ $PM == unknown ]] && command -v apt-get >/dev/null 2>&1; then PM=apt; fi
printf "SYSTEM\n  %-16s %s\n" "Distribution" "${PRETTY_NAME:-${NAME:-unknown}}"; printf "  %-16s %s\n\n" "Package manager" "$PM"
required=(); recommended=()
command -v python3 >/dev/null || required+=(python3)
command -v mpv >/dev/null || required+=(mpv)
command -v cava >/dev/null || recommended+=(cava)
command -v yt-dlp >/dev/null || recommended+=(yt-dlp)
command -v notify-send >/dev/null || recommended+=(notify)
# Mutagen gives robust local tag parsing. Install distro package when available.
python3 -c 'import mutagen' >/dev/null 2>&1 || recommended+=(mutagen)
# MPRIS2 desktop/media-key integration is optional but recommended.
python3 -c 'import dbus, gi' >/dev/null 2>&1 || recommended+=(mpris)
# Local Spotify playback. Only ask the package manager for librespot when the
# configured repository actually contains it; otherwise NPLAY remains usable
# and Spotify → Local playback explains the missing optional engine.
if ! command -v librespot >/dev/null 2>&1; then
 if [[ $PM == pacman ]] && pacman -Si librespot >/dev/null 2>&1; then recommended+=(librespot)
 elif [[ $PM == apt ]] && apt-cache show librespot >/dev/null 2>&1; then recommended+=(librespot)
 fi
fi
install_deps(){
 pkgs=("${required[@]}" "${recommended[@]}"); [[ ${#pkgs[@]} -gt 0 ]] || return 0
 [[ $PM != unknown ]] || { printf "${R}Automatic dependency installation is unavailable for this distribution.${N}\nMissing: %s\nInstall these packages, then rerun.\n" "${pkgs[*]}"; exit 1; }
 mapped=(); for x in "${pkgs[@]}"; do
  case "$PM:$x" in pacman:python3) mapped+=(python);; pacman:mutagen) mapped+=(python-mutagen);; apt:mutagen) mapped+=(python3-mutagen);; pacman:notify) mapped+=(libnotify);; apt:notify) mapped+=(libnotify-bin);; pacman:mpris) mapped+=(python-dbus python-gobject);; xbps:python3) mapped+=(python3);; xbps:mutagen) mapped+=(python3-mutagen);; xbps:notify) mapped+=(libnotify);; xbps:mpris) mapped+=(python3-dbus python3-gobject);; apt:mpris) mapped+=(python3-dbus python3-gi);; *) mapped+=("$x");; esac
 done
 printf "DEPENDENCIES\n"; for x in "${mapped[@]}"; do printf "  ${Y}•${N} %s\n" "$x"; done
 if [[ ! -t 0 ]]; then printf "${R}Cannot ask permission without an interactive terminal.${N}\n"; exit 1; fi
 printf "\nInstall missing dependencies using sudo? [Y/n] "; read -r ans; [[ ${ans:-Y} =~ ^[Yy]$ ]] || { printf "Installation cancelled.\n"; exit 1; }
 if [[ $PM == pacman ]]; then sudo pacman -S --needed "${mapped[@]}"; elif [[ $PM == xbps ]]; then sudo xbps-install -S "${mapped[@]}"; else sudo apt-get update; sudo apt-get install -y "${mapped[@]}"; fi
}
install_deps
command -v python3 >/dev/null || { echo 'ERROR: python3 missing'; exit 1; }; command -v mpv >/dev/null || { echo 'ERROR: mpv missing'; exit 1; }
printf "\nINSTALL\n"
mkdir -p "$BIN" "$(dirname "$APPDEST")" "$CFG" "$DATA" "$STATE"
# Small safety backup of irreplaceable persistent state before code upgrade.
STAMP="$(date +%Y%m%d-%H%M%S)"; BACKUP="$STATE/upgrades/$STAMP"
mkdir -p "$BACKUP"
[[ -f "$DATA/library.db" ]] && cp -a "$DATA/library.db" "$BACKUP/library.db"
[[ -f "$DATA/statistics.sqlite3" ]] && cp -a "$DATA/statistics.sqlite3" "$BACKUP/statistics.sqlite3"
[[ -d "$DATA/spotify-local" ]] && cp -a "$DATA/spotify-local" "$BACKUP/spotify-local"
[[ -f "$CFG/config.ini" ]] && cp -a "$CFG/config.ini" "$BACKUP/config.ini"
[[ -f "$CFG/secrets.ini" ]] && cp -a "$CFG/secrets.ini" "$BACKUP/secrets.ini"
TMP="${APPDEST}.new.$$"; OLD="${APPDEST}.old.$$"; trap 'rm -rf "$TMP" "$OLD"' EXIT
mkdir -p "$TMP"; cp -a "$ROOT/nplay" "$TMP/"
[[ -d "$APPDEST" ]] && mv "$APPDEST" "$OLD"
if ! mv "$TMP" "$APPDEST"; then [[ -d "$OLD" ]] && mv "$OLD" "$APPDEST"; exit 1; fi
rm -rf "$OLD"
[[ -f "$CFG/config.ini" ]] || cp "$ROOT/config.example.ini" "$CFG/config.ini"
cat > "$BIN/nplay" <<EOF
#!/usr/bin/env bash
export PYTHONPATH="$APPDEST:\${PYTHONPATH:-}"
exec python3 -m nplay.app "\$@"
EOF
chmod 0755 "$BIN/nplay"
mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/nplay.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=NPLAY
Comment=Terminal-native music player
Exec=$BIN/nplay
Terminal=true
Categories=Audio;AudioVideo;Player;
EOF
printf "  ${G}✓${N} Application code: $APPDEST\n  ${G}✓${N} Persistent data preserved: $DATA\n  ${G}✓${N} Configuration preserved: $CFG\n  ${G}✓${N} Pre-upgrade state backup: $BACKUP\n  ${G}✓${N} Launcher ~/.local/bin/nplay\n"
printf "\nCAPABILITIES\n  %-16s %s\n" "mpv" "$(command -v mpv >/dev/null && echo ready || echo missing)"; printf "  %-16s %s\n" "CAVA" "$(command -v cava >/dev/null && echo ready || echo unavailable)"; printf "  %-16s %s\n" "Spotify" "Web API + local librespot"; printf "  %-16s %s\n" "librespot" "$(command -v librespot >/dev/null && echo ready || echo optional / install for local Spotify)"; printf "  %-16s %s\n" "YouTube" "$(command -v yt-dlp >/dev/null && echo ready || echo unavailable)"; printf "  %-16s %s\n" "Kitty artwork" "$(command -v kitten >/dev/null && echo available || echo text fallback)"; printf "  %-16s %s\n" "MPRIS2" "$(python3 -c 'import dbus,gi' >/dev/null 2>&1 && echo available || echo optional / unavailable)"; printf "  %-16s %s\n" "Notifications" "$(command -v notify-send >/dev/null && echo available || echo optional / unavailable)"
printf "\n${B}NPLAY is ready.${N}\nRun: nplay\nDoctor: nplay --doctor\n"
