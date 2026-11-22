//! Settings menus and toggles (1.x `services/settings.py`).

use crate::app::{capitalize, App, Row, UiRef};

fn onoff(b: bool) -> &'static str {
    if b {
        "On"
    } else {
        "Off"
    }
}

fn tf(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// Open a `settings` / `settings:*` action.
pub fn open(a: &App, s: &str, ui: &UiRef) {
    let c = &a.cfg;
    let reopen = |what: &str| open(a, what, ui);
    let toggle = |key: &str, default: bool| -> bool {
        let enabled = !c.getbool(key, default);
        c.set(key, tf(enabled));
        enabled
    };
    match s {
        "settings" => ui.show_menu(
            "SETTINGS",
            vec![
                Row::entry(
                    "APPEARANCE",
                    "settings:appearance",
                    "Theme · Now Playing · visualizer",
                ),
                Row::entry(
                    "SOURCES",
                    "settings:sources",
                    "Navidrome · Spotify · Sveriges Radio · YouTube",
                ),
                Row::entry(
                    "LIBRARY",
                    "settings:library",
                    "Music folders · recursive scan",
                ),
                Row::entry(
                    "PLAYBACK",
                    "settings:playback",
                    "Queue and playback behaviour",
                ),
                Row::entry("SYSTEM", "settings:system", "Diagnostics · capabilities"),
            ],
            true,
        ),
        "settings:appearance" => ui.show_menu(
            "SETTINGS · APPEARANCE",
            vec![
                Row::entry("THEME", "settings:theme", ui.theme_label()),
                Row::entry(
                    "NOW PLAYING VIEW",
                    "settings:view",
                    capitalize(&ui.layout()),
                ),
                Row::entry("VISUALIZER", "settings:visual", onoff(ui.visualizer_on())),
                Row::entry(
                    "CAVA STYLE",
                    "settings:visual-style",
                    format!("{} · Classic is default", capitalize(&ui.visual_style())),
                ),
            ],
            true,
        ),
        "settings:sources" => {
            let nav_on = c.getbool("navidrome_enabled", true);
            let nav = if nav_on && a.nav_configured() {
                "On · configured"
            } else if nav_on {
                "On · set up server"
            } else {
                "Off"
            };
            let sp = if c.getbool("spotify_enabled", false) {
                if a.spotify_configured() {
                    "On · connected"
                } else {
                    "On · setup required"
                }
            } else {
                "Off"
            };
            ui.show_menu(
                "SETTINGS · SOURCES",
                vec![
                    Row::entry("NAVIDROME", "settings:toggle-nav", nav),
                    Row::entry("SPOTIFY", "settings:toggle-spotify", sp),
                    Row::entry(
                        "CONFIGURE SPOTIFY",
                        "spotify:setup",
                        "OAuth PKCE · Spotify Premium",
                    ),
                    Row::entry(
                        "SVERIGES RADIO",
                        "radio:toggle-sr",
                        onoff(c.getbool("sr_enabled", true)),
                    ),
                    Row::entry(
                        "YOUTUBE",
                        "settings:toggle-yt",
                        onoff(c.getbool("youtube_enabled", true)),
                    ),
                    Row::entry(
                        "CUSTOM RADIO",
                        "settings:toggle-custom",
                        onoff(c.getbool("custom_radio_enabled", true)),
                    ),
                    Row::entry(
                        "CONFIGURE NAVIDROME",
                        "nav:setup",
                        "Server · username · password",
                    ),
                    Row::entry(
                        "PREFERRED SOURCE",
                        "settings:preferred-source",
                        format!(
                            "{} · cross-source matches",
                            capitalize(&c.get("preferred_source", "local"))
                        ),
                    ),
                ],
                true,
            )
        }
        "settings:library" => ui.show_menu(
            "SETTINGS · LIBRARY",
            vec![
                Row::entry("MUSIC FOLDERS", "local:folders", a.roots().join(" · ")),
                Row::entry(
                    "RESCAN LIBRARY",
                    "local:scan",
                    "Recursive · tags · embedded artwork",
                ),
                Row::entry(
                    "AUTO REFRESH",
                    "settings:toggle-autoscan",
                    if c.getbool("auto_library_refresh", true) {
                        "On startup"
                    } else {
                        "Off"
                    },
                ),
                Row::entry(
                    "ARTWORK CACHE",
                    "settings:art-cache",
                    "Size · limit · clear",
                ),
            ],
            true,
        ),
        "settings:playback" => {
            let mode = capitalize(&c.get("normalization_mode", "auto"));
            let (shuffle, repeat, qlen) = {
                let st = a.state();
                (
                    st.shuffle_mode.clone(),
                    st.repeat_mode.clone(),
                    st.queue.len(),
                )
            };
            ui.show_menu(
                "SETTINGS · PLAYBACK",
                vec![
                    Row::entry(
                        "NORMALIZATION",
                        "settings:toggle-normalize",
                        if c.getbool("normalization", true) {
                            format!("On · {mode}")
                        } else {
                            "Off".into()
                        },
                    ),
                    Row::entry(
                        "NORMALIZATION MODE",
                        "settings:normalize-mode",
                        mode.clone(),
                    ),
                    Row::entry(
                        "GAPLESS",
                        "settings:toggle-gapless",
                        if c.getbool("gapless", false) {
                            "On"
                        } else {
                            "Off · default"
                        },
                    ),
                    Row::entry("SHUFFLE", "settings:shuffle", capitalize(&shuffle)),
                    Row::entry("REPEAT", "settings:repeat", capitalize(&repeat)),
                    Row::entry(
                        "TRACK NOTIFICATIONS",
                        "settings:toggle-notify",
                        if c.getbool("track_notifications", true) {
                            "On · system notification on track change"
                        } else {
                            "Off"
                        },
                    ),
                    Row::entry(
                        "NOTIFICATION ARTWORK",
                        "settings:toggle-notify-art",
                        onoff(c.getbool("notification_artwork", true)),
                    ),
                    Row::entry(
                        "TEST NOTIFICATION",
                        "settings:test-notify",
                        "Current track · or NPLAY test",
                    ),
                    Row::entry(
                        "MPRIS / MEDIA KEYS",
                        "settings:toggle-mpris",
                        if c.getbool("mpris_enabled", true) {
                            "On · playerctl / desktop integration"
                        } else {
                            "Off"
                        },
                    ),
                    Row::entry("QUEUE", "queue", format!("{qlen} items")),
                    Row::entry(
                        "NOW PLAYING VIEW",
                        "settings:view",
                        capitalize(&ui.layout()),
                    ),
                    Row::entry("VISUALIZER", "settings:visual", onoff(ui.visualizer_on())),
                ],
                true,
            )
        }
        "settings:system" => ui.show_menu(
            "SETTINGS · SYSTEM",
            vec![Row::entry(
                "DIAGNOSTICS",
                "settings:diag",
                "Runtime capabilities · sources · renderer",
            )],
            true,
        ),
        "settings:preferred-source" => {
            let modes = ["local", "navidrome", "spotify", "youtube"];
            let cur = c.get("preferred_source", "local");
            let next = match modes.iter().position(|m| *m == cur) {
                Some(i) => modes[(i + 1) % modes.len()],
                None => "local",
            };
            c.set("preferred_source", next);
            reopen("settings:sources");
        }
        "settings:toggle-nav"
        | "settings:toggle-spotify"
        | "settings:toggle-yt"
        | "settings:toggle-custom" => {
            let key = match s {
                "settings:toggle-nav" => "navidrome_enabled",
                "settings:toggle-spotify" => "spotify_enabled",
                "settings:toggle-yt" => "youtube_enabled",
                _ => "custom_radio_enabled",
            };
            let enabled = toggle(key, key != "spotify_enabled");
            let label = key.trim_end_matches("_enabled").replace('_', " ");
            let label: Vec<String> = label.split(' ').map(capitalize).collect();
            ui.status(&format!(
                "{} {}",
                label.join(" "),
                if enabled { "enabled" } else { "disabled" }
            ));
            reopen("settings:sources");
        }
        "settings:toggle-normalize" => {
            let e = toggle("normalization", true);
            ui.status(&format!(
                "Volume normalization {}",
                if e { "enabled" } else { "disabled" }
            ));
            reopen("settings:playback");
        }
        "settings:normalize-mode" => {
            let modes = ["auto", "track", "album"];
            let cur = c.get("normalization_mode", "auto");
            let i = modes.iter().position(|m| *m == cur).unwrap_or(0);
            c.set("normalization_mode", modes[(i + 1) % 3]);
            reopen("settings:playback");
        }
        "settings:toggle-gapless" => {
            let e = toggle("gapless", false);
            ui.status(&format!(
                "Gapless {}",
                if e {
                    "enabled · applies from next track"
                } else {
                    "disabled"
                }
            ));
            reopen("settings:playback");
        }
        "settings:toggle-notify" => {
            let e = toggle("track_notifications", true);
            ui.status(&format!(
                "Track notifications {}",
                if e { "enabled" } else { "disabled" }
            ));
            reopen("settings:playback");
        }
        "settings:toggle-notify-art" => {
            let e = toggle("notification_artwork", true);
            ui.status(&format!(
                "Notification artwork {}",
                if e { "enabled" } else { "disabled" }
            ));
            reopen("settings:playback");
        }
        "settings:toggle-mpris" => {
            let e = toggle("mpris_enabled", true);
            ui.status(&format!(
                "MPRIS {}",
                if e {
                    "enabled · restart NPLAY to activate"
                } else {
                    "disabled · restart NPLAY"
                }
            ));
            reopen("settings:playback");
        }
        "settings:test-notify" => ui.status(if a.notifier.test(a.current().as_ref()) {
            "Test notification sent"
        } else {
            "System notifications unavailable · notify-send / desktop session required"
        }),
        "settings:shuffle" => {
            a.cycle_shuffle(Some(ui));
            reopen("settings:playback");
        }
        "settings:repeat" => {
            a.cycle_repeat(Some(ui));
            reopen("settings:playback");
        }
        "settings:toggle-autoscan" => {
            let e = toggle("auto_library_refresh", true);
            ui.status(&format!(
                "Automatic library refresh {}",
                if e { "enabled" } else { "disabled" }
            ));
            reopen("settings:library");
        }
        "settings:art-cache" => {
            let (n, b) = a.artwork_cache.stats();
            ui.show_menu(
                "SETTINGS · ARTWORK CACHE",
                vec![
                    Row::entry(
                        "USAGE",
                        "",
                        format!(
                            "{} files · {:.1} MB",
                            crate::app::thousands(n as i64),
                            b as f64 / 1024.0 / 1024.0
                        ),
                    ),
                    Row::entry(
                        "LIMIT",
                        "settings:art-cache-limit",
                        format!("{} MB", c.get("artwork_cache_mb", "500")),
                    ),
                    Row::entry(
                        "CLEAR CACHE",
                        "settings:art-cache-clear",
                        "Downloaded and extracted artwork · safe to rebuild",
                    ),
                ],
                true,
            );
        }
        "settings:art-cache-limit" => {
            let cfg = c.clone();
            let u = ui.clone();
            ui.prompt(
                "ARTWORK CACHE LIMIT MB › ",
                "",
                false,
                Box::new(move |v| {
                    match v
                        .as_deref()
                        .map(str::trim)
                        .and_then(|v| v.parse::<i64>().ok())
                    {
                        Some(mb) => {
                            let mb = mb.max(32);
                            cfg.set("artwork_cache_mb", mb.to_string());
                            // The cache reads its limit at construction; apply now.
                            crate::artwork_cache::ArtworkCache::new(mb).prune();
                            u.status("Artwork cache limit updated");
                        }
                        None => u.status("Enter a size in MB"),
                    }
                }),
            );
        }
        "settings:art-cache-clear" => {
            ui.status(&format!(
                "Artwork cache cleared · {} files",
                a.artwork_cache.clear()
            ));
        }
        "settings:theme" => ui.theme_menu(),
        "settings:theme-reload" => {
            let names = crate::theme::available();
            let theme = ui.theme();
            if !names.iter().any(|(k, _)| *k == theme) {
                ui.set_theme("niru-noir");
            } else {
                ui.reload_theme();
                ui.invalidate();
            }
            ui.status(&format!("Themes reloaded · {} available", names.len()));
            ui.theme_menu();
        }
        "settings:theme-help" => {
            let dir = crate::theme::theme_dir();
            ui.show_menu(
                "CUSTOM THEMES · INSTALL GUIDE",
                vec![
                    Row::entry("THEME DIRECTORY", "", dir.display().to_string()),
                    Row::entry("", "", ""),
                    Row::entry(
                        "1  Copy a .toml theme file",
                        "",
                        "Place it in the directory above",
                    ),
                    Row::entry("2  Reload custom themes", "", "Use the action below"),
                    Row::entry(
                        "3  Select your new theme",
                        "",
                        "Choose it in the theme list",
                    ),
                    Row::entry("", "", ""),
                    Row::entry(
                        "OPEN THEME DIRECTORY",
                        "settings:theme-open",
                        "Open in your desktop file manager",
                    ),
                    Row::entry(
                        "RELOAD CUSTOM THEMES",
                        "settings:theme-reload",
                        "Scan for installed themes",
                    ),
                    Row::entry(
                        "BACK TO THEMES",
                        "settings:theme",
                        "Select an installed theme",
                    ),
                ],
                true,
            );
        }
        "settings:theme-open" => {
            let (_ok, msg) = crate::theme::open_theme_directory(&crate::theme::theme_dir());
            ui.status(&msg);
        }
        "settings:view" => ui.cycle_layout(),
        "settings:visual" => ui.toggle_visualizer(),
        "settings:visual-style" => ui.visualizer_style_menu(),
        "settings:diag" => ui.show_diagnostics(),
        _ => {}
    }
}
