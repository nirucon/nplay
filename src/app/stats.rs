//! Statistics views (overview, rankings, settings & sync) and smart playlists.

use super::{App, Row, UiRef};

/// `1234567` -> `1,234,567` (Python `f'{n:,}'`).
pub fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

const PERIODS: &[(&str, &str)] = &[
    ("TODAY", "day"),
    ("WEEK", "week"),
    ("MONTH", "month"),
    ("YEAR", "year"),
    ("ALL TIME", "all"),
];

impl App {
    /// `1h 02m`, or `3m 05s` under an hour.
    pub fn stats_time(seconds: f64) -> String {
        let s = if seconds.is_finite() {
            seconds.max(0.0) as i64
        } else {
            0
        };
        let (h, rem) = (s / 3600, s % 3600);
        let (m, sec) = (rem / 60, rem % 60);
        if h > 0 {
            format!("{}h {m:02}m", thousands(h))
        } else {
            format!("{m}m {sec:02}s")
        }
    }

    fn summary(&self, period: &str) -> crate::statistics::Summary {
        self.statistics.summary(period, None).unwrap_or_default()
    }

    /// Overview: every number is visible without selection.
    pub fn stats_home(&self, ui: &UiRef, period: &str, push: bool) {
        self.state().stats_period = period.to_string();
        if !self.cfg.getbool("statistics_enabled", true) {
            ui.show_menu(
                "STATISTICS",
                vec![
                    Row::entry("STATISTICS · OFF", "", "Listening history is preserved"),
                    Row::entry(
                        "ENABLE LOCAL STATISTICS",
                        "stats:toggle",
                        "No data is uploaded",
                    ),
                    Row::entry(
                        "SETTINGS & SYNC",
                        "stats:settings",
                        "Local privacy and installation identity",
                    ),
                ],
                push,
            );
            return;
        }
        let st = self.summary(period);
        let mut items = vec![
            Row::entry(
                format!("ACTUAL LISTENING · {}", App::stats_time(st.seconds)),
                "",
                format!("{} sessions · {} active days", st.sessions, st.days),
            ),
            Row::entry(
                format!("PERIOD · {}", period.to_uppercase()),
                "",
                "Select a period below",
            ),
        ];
        for (label, key) in PERIODS {
            let mark = if period == *key { "● " } else { "  " };
            items.push(Row::entry(
                format!("{mark}{label}"),
                format!("stats:period:{key}"),
                "",
            ));
        }
        items.push(Row::entry("", "", ""));
        items.push(Row::entry(
            "TOP ARTISTS",
            "stats:artists",
            "Open complete ranking",
        ));
        for (i, (name, sec, _)) in st.artists.iter().take(3).enumerate() {
            items.push(Row::header(format!(
                "{}. {name} · {}",
                i + 1,
                App::stats_time(*sec)
            )));
        }
        items.push(Row::entry(
            "TOP ALBUMS",
            "stats:albums",
            "Open complete ranking",
        ));
        for (i, (_, album, sec, _)) in st.albums.iter().take(3).enumerate() {
            items.push(Row::header(format!(
                "{}. {album} · {}",
                i + 1,
                App::stats_time(*sec)
            )));
        }
        items.push(Row::entry(
            "TOP TRACKS",
            "stats:tracks",
            "Open complete ranking",
        ));
        for (i, (_, title, sec, _)) in st.tracks.iter().take(3).enumerate() {
            items.push(Row::header(format!(
                "{}. {title} · {}",
                i + 1,
                App::stats_time(*sec)
            )));
        }
        items.push(Row::entry(
            "SOURCES",
            "stats:sources",
            "Listening time by source",
        ));
        items.push(Row::entry(
            "SETTINGS & SYNC",
            "stats:settings",
            "Collection · privacy · offline outbox",
        ));
        ui.show_menu(
            &format!("STATISTICS · {}", period.to_uppercase()),
            items,
            push,
        );
    }

    /// A complete ranking (`artists`, `albums`, `tracks`, `sources`).
    pub fn stats_details(&self, ui: &UiRef, section: &str, period: Option<&str>, push: bool) {
        let period = match period {
            Some(p) => p.to_string(),
            None => self.state().stats_period.clone(),
        };
        self.state().stats_period = period.clone();
        let name = match section {
            "artists" => "ARTISTS",
            "albums" => "ALBUMS",
            "tracks" => "TRACKS",
            "sources" => "SOURCES",
            _ => return,
        };
        let st = self.summary(&period);
        let mut items = vec![
            Row::header(format!("LISTENED · {}", App::stats_time(st.seconds))),
            Row::header("RANKED BY ACTUAL LISTENING TIME"),
        ];
        let rows: Vec<(String, String)> = match section {
            "albums" => st
                .albums
                .iter()
                .enumerate()
                .map(|(i, (artist, album, s, plays))| {
                    (
                        format!("{:2}  {album} · {}", i + 1, App::stats_time(*s)),
                        format!("{artist} · {plays} sessions"),
                    )
                })
                .collect(),
            "tracks" => st
                .tracks
                .iter()
                .enumerate()
                .map(|(i, (artist, title, s, plays))| {
                    (
                        format!("{:2}  {title} · {}", i + 1, App::stats_time(*s)),
                        format!("{artist} · {plays} sessions"),
                    )
                })
                .collect(),
            _ => {
                let src = if section == "artists" {
                    &st.artists
                } else {
                    &st.sources
                };
                src.iter()
                    .enumerate()
                    .map(|(i, (n, s, plays))| {
                        (
                            format!("{:2}  {n} · {}", i + 1, App::stats_time(*s)),
                            format!("{plays} sessions"),
                        )
                    })
                    .collect()
            }
        };
        let empty = rows.is_empty();
        for (label, detail) in rows {
            items.push(Row::entry(label, "", detail));
        }
        if empty {
            items.push(Row::entry(
                "NO LISTENING DATA YET",
                "",
                "Play some music to build rankings",
            ));
        }
        items.push(Row::entry(
            "BACK TO OVERVIEW",
            "stats:overview",
            "Return to statistics",
        ));
        ui.show_menu(
            &format!("STATISTICS · {name} · {}", period.to_uppercase()),
            items,
            push,
        );
    }

    pub fn stats_settings(&self, ui: &UiRef, push: bool) {
        let enabled = self.cfg.getbool("statistics_enabled", true);
        let sync = &self.statistics_sync;
        let online = sync.enabled();
        let pending = self.summary("all").pending;
        let ls = sync.last_success();
        let last = if ls > 0.0 {
            format!(
                "{} {}",
                crate::providers::util::local_date(ls),
                crate::providers::util::local_hm(ls)
            )
        } else {
            "NEVER".into()
        };
        let err = sync.last_error();
        ui.show_menu(
            "STATISTICS · SETTINGS & SYNC",
            vec![
                Row::entry(
                    format!("LOCAL COLLECTION · {}", if enabled { "ON" } else { "OFF" }),
                    "stats:toggle",
                    "Toggle local listening statistics",
                ),
                Row::entry(
                    format!(
                        "PRIVACY · {}",
                        if online {
                            "SYNC ENABLED"
                        } else {
                            "OFFLINE ONLY"
                        }
                    ),
                    "",
                    "Only listening metadata is sent when enabled",
                ),
                Row::entry(
                    format!(
                        "SYNC · {}",
                        if online {
                            sync.status()
                        } else {
                            "DISABLED".into()
                        }
                    ),
                    "",
                    "Background HTTPS only",
                ),
                Row::entry(format!("SERVER · {}", sync.url()), "", "HTTPS API endpoint"),
                Row::entry(
                    format!("OUTBOX · {pending} PENDING"),
                    "",
                    "Existing events are never deleted",
                ),
                Row::entry(
                    format!("LAST SUCCESS · {last}"),
                    "",
                    "Most recent successful batch in this session",
                ),
                Row::entry(
                    format!(
                        "LAST ERROR · {}",
                        if err.is_empty() { "NONE".into() } else { err }
                    ),
                    "",
                    "No credentials in diagnostics",
                ),
                Row::entry(
                    "MANUAL SYNC NOW",
                    "stats:sync-now",
                    "Send up to 50 pending events in background",
                ),
                Row::entry(
                    format!("SYNC · {}", if online { "DISABLE" } else { "ENABLE" }),
                    "stats:sync-toggle",
                    "Explicit opt-in · requires dedicated token",
                ),
                Row::entry("INSTALLATION ID", "", "Unique per NPLAY data directory"),
                Row::entry(
                    self.statistics.installation_id().unwrap_or_default(),
                    "",
                    "Copy using the action below",
                ),
                Row::entry(
                    "COPY INSTALLATION ID",
                    "stats:copy-id",
                    "Copy identifier to clipboard if supported",
                ),
                Row::entry("BACK TO OVERVIEW", "stats:overview", "Return to statistics"),
            ],
            push,
        );
    }

    pub fn statistics_sync_toggle(&self, ui: &UiRef) {
        let new = !self.statistics_sync.enabled();
        if new && self.cfg.statistics_token().is_empty() {
            ui.status("Configure a dedicated token first: nplay --stats-set-token");
            return;
        }
        self.cfg.set(
            "statistics_sync_enabled",
            if new { "true" } else { "false" },
        );
        self.statistics_sync
            .set_status(if new { "IDLE" } else { "DISABLED" });
        self.stats_settings(ui, false);
    }

    pub fn statistics_sync_now(&self, ui: &UiRef) {
        if !self.statistics_sync.enabled() {
            ui.status("Enable sync first in Settings & Sync");
            return;
        }
        let app = self.arc();
        let u = ui.clone();
        let cb: crate::statistics::sync::Callback = std::sync::Arc::new(move || {
            let app = app.clone();
            let u2 = u.clone();
            u.post(Box::new(move || {
                app.refresh_statistics_view(&u2);
            }));
        });
        let started = self.statistics_sync.run_async(true, Some(cb));
        ui.status(if started {
            "Statistics syncing in background"
        } else {
            "Sync already in progress"
        });
        self.stats_settings(ui, false);
    }

    /// Refresh an open statistics menu in place, keeping the selection.
    /// Never touches playback state or navigation history.
    pub fn refresh_statistics_view(&self, ui: &UiRef) -> bool {
        let Some(v) = ui.view() else { return false };
        if v.mode != "menu" || !v.title.starts_with("STATISTICS") {
            return false;
        }
        let previous = v.sel;
        let action = v
            .rows
            .get(previous)
            .and_then(|r| match r {
                Row::Entry(e) => Some(e.action.clone()),
                Row::Track(_) => None,
            })
            .unwrap_or_default();
        let title = v.title.as_str();
        let rest = title.strip_prefix("STATISTICS · ");
        if title == "STATISTICS · SETTINGS & SYNC" {
            self.stats_settings(ui, false);
        } else if rest.is_some_and(|r| r.contains(" · ")) {
            let parts: Vec<&str> = title.split(" · ").collect();
            let section = match parts.get(1).copied() {
                Some("ARTISTS") => "artists",
                Some("ALBUMS") => "albums",
                Some("TRACKS") => "tracks",
                Some("SOURCES") => "sources",
                _ => return false,
            };
            self.stats_details(ui, section, None, false);
        } else if title == "STATISTICS" || rest.is_some() {
            let p = self.state().stats_period.clone();
            self.stats_home(ui, &p, false);
        } else {
            return false;
        }
        if let Some(nv) = ui.view() {
            if !nv.rows.is_empty() {
                // Prefer the same navigation action, otherwise the row position.
                let idx = (!action.is_empty())
                    .then(|| nv.rows.iter().position(|r| r.action() == action))
                    .flatten()
                    .unwrap_or_else(|| previous.min(nv.rows.len() - 1));
                ui.set_selection(idx);
                if !nv.rows[idx].selectable() {
                    ui.edge_selection(false);
                }
                ui.invalidate();
            }
        }
        true
    }

    pub fn statistics_toggle(&self, ui: &UiRef) {
        let enabled = !self.cfg.getbool("statistics_enabled", true);
        self.cfg
            .set("statistics_enabled", if enabled { "true" } else { "false" });
        self.statistics_tracker.set_enabled(enabled);
        self.statistics_tracker.flush(self.playback_playing());
        self.stats_settings(ui, false);
        ui.status(&format!(
            "Local statistics {} · sync disabled",
            if enabled { "enabled" } else { "disabled" }
        ));
    }

    pub fn smart_home(&self, ui: &UiRef) {
        ui.show_menu(
            "SMART PLAYLISTS",
            vec![
                Row::entry("RECENTLY ADDED", "smart:recent", "Newest local files"),
                Row::entry("MOST PLAYED", "smart:most", "Based on NPLAY history"),
                Row::entry("NEVER PLAYED", "smart:never", "Local tracks not in history"),
                Row::entry(
                    "UNPLAYED ALBUMS",
                    "smart:albums",
                    "Albums with no played tracks",
                ),
                Row::entry("HIGHLY RATED", "smart:rated", "4–5 stars"),
                Row::entry("RANDOM 50", "local:random:50", "Fresh local selection"),
            ],
            true,
        );
    }
}
