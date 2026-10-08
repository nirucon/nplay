#!/usr/bin/env bash
set -euo pipefail
rm -f "$HOME/.local/bin/nplay"
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/nplay"
echo 'NPLAY application removed. User config/state were preserved.'
