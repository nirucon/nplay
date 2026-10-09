# Custom themes — NPLAY 1.5.2

NPLAY was built primarily for a personal Linux setup. Custom themes let other users customize colors without changing the player or installing Python plugins.

## In NPLAY

Open **Browse → Settings → Appearance → Theme**.

- **HOW TO INSTALL THEMES** shows the installation directory and a three-step guide, including **OPEN THEME DIRECTORY**.
- **RELOAD CUSTOM THEMES** discovers new/updated TOML files without restarting.
- Select a theme from the list to apply and save it immediately.
- If a selected custom theme disappears, choose Niru Noir as the fallback.

No additional themes are bundled in this release. A separate theme pack may be published later.

## Install a custom theme

Create the directory:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/nplay/themes"
```

Copy a downloaded `my-theme.toml` into that directory. A theme file must contain:

```toml
[theme]
id = "my-theme"
name = "My Theme"
author = "Your Name"
version = "1.0"

[colors]
fg = "#E0E0E0"
muted = "#909090"
accent = "#C0C0C0"
bg = "#151515"
select_fg = "#151515"
select_bg = "#C0C0C0"
```

This snippet illustrates the file format; it is not an installed theme. All six color keys are optional individually, but at least one is required. Unspecified colors inherit the internal fallback palette. `id` must match the filename and may contain lowercase letters, digits and hyphens.

Run `nplay --check-theme /path/to/my-theme.toml` to validate, or `nplay --list-themes` to see installed themes. Within the app choose **Reload custom themes** and then select your theme.

## Safety and compatibility

- TOML only; no Python or shell execution.
- Unknown keys, invalid hex values, reserved built-in IDs and symlinks are rejected.
- Files larger than 16 KiB are rejected; at most 128 custom theme files are scanned.
- Built-in themes and Omarchy-follow mode remain unchanged.
- A 256-color terminal is recommended. Basic-color terminals use NPLAY's readable fallback.
- Theme files live in user configuration and survive application upgrades.
