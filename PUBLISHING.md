# Repository and data safety

NPLAY is developed primarily for a personal Linux environment. The public repository intentionally contains only application source, a configuration **template**, installer/uninstaller scripts, tests, and documentation.

## Do not commit

- `~/.config/nplay/config.ini` or `secrets.ini`
- `~/.local/share/nplay/` (especially `library.db`, `spotify-local/`, album artwork caches)
- `~/.local/state/nplay/` (sessions, logs, backups)
- OAuth authorization codes, access/refresh tokens, API keys, passwords, or private server URLs
- `__pycache__/`, `.pyc`, test caches, local virtual environments, editor caches
- Local audio files, credentials, private playlists, or generated backups

The sample file `config.example.ini` is safe to share as long as it remains free of personal credentials.

## Basic validation before publishing

```sh
python3 -m compileall -q nplay
python3 -m unittest discover -s tests -v
bash -n install.sh
bash -n uninstall.sh
PYTHONPATH=. python3 -m nplay.app --version
```

`compileall` creates ignored bytecode cache directories; they should not be committed.

## Upgrade layout

Installed program code is stored in `~/.local/lib/nplay/`; persistent state lives under XDG config/data/state directories. The installer must never replace the persistent data directory as part of a code upgrade. Backups are a safety net, not a substitute for the user's regular backups.

## Network environments

The Spotify Web API and Spotify Connect receiver use different network paths. On managed networks, the Web API may work while librespot access-point traffic times out (TCP 4070 has been observed). Report this as a network/device-registration diagnostic, not automatically as a failed OAuth login.
