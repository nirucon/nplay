//! The `:` command prompt, UI-side helpers ported from 1.x `UI` (activate,
//! Actions menu, random play), `--doctor` and the exit sequence.

use super::{clip, track_rows, App, Row, UiRef};
use crate::model::Track;

/// The 29 completions of the `:` prompt (1.x `UI.prompt` order).
pub const COMMANDS: &[&str] = &[
    "about",
    "browse",
    "diagnostics",
    "discover",
    "gapless",
    "help",
    "local",
    "navidrome",
    "normalize",
    "now",
    "queue",
    "queue-clear",
    "queue-save",
    "radio",
    "rating",
    "repeat",
    "scan",
    "settings",
    "shuffle",
    "sleep",
    "smart",
    "spotify",
    "stop",
    "bookmarks",
    "sr",
    "stats",
    "theme",
    "view",
    "youtube",
];

pub const UNKNOWN_COMMAND: &str = "Unknown command · use :help · commands: :stop · :bookmarks · :now · :browse · :theme · :settings · :nav QUERY · :spotify QUERY · :sr QUERY · :yt QUERY · :radio p1 · :view normal|artwork|visualizer · :normalize · :gapless · :shuffle · :repeat · :sleep 30|track|album|off · :smart · :stats · :discover · :rating 0-5 · :queue-save · :queue-clear · :diagnostics · :about · :scan";

/// Tab completions for the text typed so far (1.x `suggestions`).
pub fn command_suggestions(text: &str) -> Vec<String> {
    let t = text.trim();
    let first = t.split_whitespace().next().unwrap_or("").to_lowercase();
    if !t.contains(' ') && !first.is_empty() {
        return COMMANDS
            .iter()
            .filter(|c| c.starts_with(&first))
            .map(|c| c.to_string())
            .collect();
    }
    let v: &[&str] = match first.as_str() {
        "normalize" => &[
            "normalize on",
            "normalize off",
            "normalize auto",
            "normalize track",
            "normalize album",
        ],
        "gapless" => &["gapless on", "gapless off"],
        "view" => &["view normal", "view artwork", "view visualizer"],
        "theme" => &[
            "theme noir",
            "theme satie",
            "theme larsson",
            "theme othala",
            "theme ingwaz",
            "theme hackerman",
            "theme c64",
            "theme omarchy",
        ],
        _ => &[],
    };
    v.iter().map(|s| s.to_string()).collect()
}

impl App {
    /// Run one `:` command line.
    pub fn command(&self, q: &str, ui: &UiRef) {
        let q = q.trim_start();
        let (cmd, arg) = match q.split_once(char::is_whitespace) {
            Some((c, a)) => (c.to_lowercase(), a.trim().to_string()),
            None => (q.to_lowercase(), String::new()),
        };
        let u = Some(ui);
        match cmd.as_str() {
            "about" => ui.show_about(),
            "browse" | "b" => self.root_browse(ui),
            "local" => self.local_home(ui),
            "queue" => self.open_source("queue", ui),
            "theme" => {
                if arg.is_empty() {
                    ui.theme_menu()
                } else {
                    ui.set_theme(&arg)
                }
            }
            "settings" => self.open_source("settings", ui),
            "nav" | "navidrome" => {
                if arg.is_empty() {
                    self.open_nav_home(ui)
                } else {
                    self.search_provider("nav", &arg, ui)
                }
            }
            "spotify" => {
                if arg.is_empty() {
                    self.open_spotify_home(ui)
                } else {
                    self.spotify_search(&arg, ui)
                }
            }
            "sr" => {
                if arg.is_empty() {
                    self.open_source("srpod", ui)
                } else {
                    self.search_sr_all(&arg, ui)
                }
            }
            "yt" | "youtube" => {
                if arg.is_empty() {
                    let app = self.arc();
                    let u2 = ui.clone();
                    ui.prompt(
                        "YOUTUBE SEARCH › ",
                        "",
                        false,
                        Box::new(move |q| app.search_provider("yt", &q.unwrap_or_default(), &u2)),
                    );
                } else {
                    self.search_provider("yt", &arg, ui)
                }
            }
            "radio" => {
                let xs = self.sr.live();
                let a = arg.to_lowercase();
                let m = xs
                    .iter()
                    .find(|x| x.title.to_lowercase().contains(&a))
                    .or(xs.first())
                    .cloned();
                if let Some(m) = m {
                    self.play(m, u, false);
                }
            }
            "now" => ui.now_playing(),
            "stop" => self.stop_and_clear(u),
            "bookmarks" => self.open_source("bookmarks", ui),
            "next" | "n" => self.next_track(u, false),
            "previous" | "prev" | "p" => self.previous_track(u),
            "view" => {
                if matches!(arg.as_str(), "normal" | "artwork" | "visualizer") {
                    ui.set_layout(&arg);
                    self.cfg.set("now_playing_view", &arg);
                    ui.status(&format!("View · {}", super::capitalize(&arg)));
                    ui.invalidate();
                } else {
                    ui.status("View · normal | artwork | visualizer");
                }
            }
            "normalize" => match arg.as_str() {
                "on" | "off" => {
                    self.cfg
                        .set("normalization", if arg == "on" { "true" } else { "false" });
                    ui.status(&format!("Normalization · {arg}"));
                }
                "auto" | "track" | "album" => {
                    self.cfg.set("normalization_mode", &arg);
                    ui.status(&format!("Normalization mode · {arg}"));
                }
                _ => ui.status("Normalize · on | off | auto | track | album"),
            },
            "gapless" => match arg.as_str() {
                "on" | "off" => {
                    self.cfg
                        .set("gapless", if arg == "on" { "true" } else { "false" });
                    ui.status(&format!("Gapless · {arg} · applies from next track"));
                }
                _ => ui.status("Gapless · on | off"),
            },
            "shuffle" => self.cycle_shuffle(u),
            "repeat" => self.cycle_repeat(u),
            "sleep" => self.sleep_set(&arg, u),
            "stats" => self.stats_home(ui, "week", true),
            "smart" => self.smart_home(ui),
            "discover" => self.save_discovery(u),
            "rating" => match arg.parse::<i64>() {
                Ok(r) => self.set_rating(self.current().as_ref(), r, u),
                Err(_) => ui.status("Rating · 0–5"),
            },
            "queue-clear" => self.queue_clear(u),
            "queue-save" => self.queue_save_playlist(u),
            "diagnostics" => ui.show_diagnostics(),
            "help" => ui.show_help(),
            "scan" => match self.scan(None, false) {
                Ok(r) => ui.status(&App::scan_message(&r)),
                Err(e) => ui.error("Library scan failed", &e.to_string()),
            },
            _ => ui.status(UNKNOWN_COMMAND),
        }
    }

    /// Enter on a row (1.x `UI.activate`). `title` is the view's title.
    pub fn activate(&self, ui: &UiRef, title: &str, rows: &[Row], sel: usize) {
        let Some(x) = rows.get(sel) else { return };
        match x {
            Row::Entry(e) => {
                let action = e.action.as_str();
                if action.is_empty() {
                    return;
                }
                if let Some(name) = action.strip_prefix("theme:") {
                    ui.set_theme(name);
                    ui.theme_menu();
                    return;
                }
                if let Some(a) = action.strip_prefix("nav:") {
                    self.nav_action(a, ui);
                } else {
                    self.open_source(action, ui);
                }
            }
            Row::Track(t) => {
                if title == "QUEUE" {
                    self.queue_play(sel, Some(ui));
                    return;
                }
                let playable: Vec<Track> = rows.iter().filter_map(Row::playable).cloned().collect();
                if x.playable().is_some() {
                    if t.kind == "radio" {
                        self.set_play_context(Vec::new(), 0);
                    } else {
                        let i = playable.iter().position(|p| p == t).unwrap_or(0);
                        self.set_play_context(playable, i as i64);
                    }
                }
                ui.push();
                self.open_item(t.clone(), ui);
            }
        }
    }

    /// `r`: shuffle the playable rows of the view and play the first.
    pub fn random_play(&self, ui: &UiRef, title: &str, rows: &[Row]) {
        let mut playable: Vec<Track> = rows.iter().filter_map(Row::playable).cloned().collect();
        if !playable.is_empty() {
            fastrand::shuffle(&mut playable);
            let first = playable[0].clone();
            self.set_play_context(playable, 0);
            let t = first.title.clone();
            self.play(first, Some(ui), true);
            ui.status(&format!("Random · {t}"));
            return;
        }
        if title.starts_with("LOCAL") {
            ui.show_list(
                "LOCAL MUSIC · RANDOM",
                track_rows(self.db.random_tracks(50).unwrap_or_default()),
                "No indexed music",
                None,
                true,
            );
        }
    }

    /// `.`: the Actions menu for the selected row or the current track.
    pub fn action_menu(&self, ui: &UiRef, selected: Option<Track>) {
        let Some(t) = selected.or_else(|| self.current()) else {
            ui.status("Actions · nothing selected or playing");
            return;
        };
        self.set_action_target(Some(t.clone()));
        let mut items = vec![
            Row::entry("PLAY", "ctx:play", "Play this item"),
            Row::entry("PLAY NEXT", "ctx:next", "Insert at the front of the queue"),
            Row::entry("ADD TO QUEUE", "ctx:queue", "Append to queue"),
            Row::entry(
                "AFTER CURRENT CONTEXT",
                "ctx:after-context",
                "Play after the current album/list finishes",
            ),
            Row::entry("ADD TO PLAYLIST", "ctx:playlist", "Choose a local playlist"),
            Row::entry("INFORMATION", "ctx:info", "Metadata and playback context"),
            Row::entry("RATE ★★★★★", "ctx:rate5", "Set local NPLAY rating to 5"),
            Row::entry("RATE ★★★★", "ctx:rate4", "Set local NPLAY rating to 4"),
            Row::entry("CLEAR RATING", "ctx:rate0", "Remove NPLAY rating"),
        ];
        let sources = t
            .meta
            .get("available_sources")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if sources > 1 {
            items.push(Row::entry(
                "SWITCH SOURCE",
                "ctx:switch-source",
                "Choose Local · Navidrome · Spotify · YouTube match",
            ));
        }
        if matches!(t.kind.as_str(), "podcast" | "track") && t.seekable {
            items.push(Row::entry(
                "BOOKMARK POSITION",
                "ctx:bookmark",
                "Save current position for later",
            ));
        }
        if t.kind == "track" {
            items.push(Row::entry(
                "START NPLAY RADIO",
                "ctx:nplay-radio",
                "Build a cross-source discovery mix from this item",
            ));
        }
        if t.kind == "radio" {
            items.push(Row::entry(
                "SAVE DISCOVERY",
                "ctx:discover",
                "Save current radio metadata for later",
            ));
        }
        if t.source == "spotify" {
            items.push(Row::entry(
                "SAVE TO SPOTIFY LIBRARY",
                "ctx:spotify-save",
                "Save this item in your Spotify library",
            ));
            if t.kind == "track" {
                items.push(Row::entry(
                    "ADD TO SPOTIFY PLAYLIST",
                    "ctx:spotify-playlist",
                    "Choose one of your Spotify playlists",
                ));
            }
        }
        if t.source == "navidrome" {
            if !t.artist.is_empty() {
                items.push(Row::entry(
                    "GO TO ARTIST",
                    "ctx:artist",
                    "Browse this artist in Navidrome",
                ));
            }
            if !t.album.is_empty() && t.kind == "track" {
                items.push(Row::entry(
                    "GO TO ALBUM",
                    "ctx:album",
                    "Open this album in Navidrome",
                ));
            }
        }
        ui.show_menu(&format!("ACTIONS · {}", clip(&t.title, 42)), items, true);
    }

    // ---- --doctor ------------------------------------------------------------

    /// The `nplay --doctor` report. `vis_line` is the service probe's
    /// `VIS: CAVA, ...` line (the coordinator passes it; "" omits the row).
    pub fn doctor_lines(&self, vis_line: &str) -> Vec<String> {
        let w = crate::paths::which;
        let on = |k: &str, d: bool| self.cfg.getbool(k, d);
        let mut out = vec![
            format!("NPLAY {} by Nicklas Rudolfsson", crate::VERSION),
            "runtime      OK".to_string(),
            format!(
                "mpv          {}",
                if w("mpv").is_some() { "OK" } else { "MISSING" }
            ),
            format!(
                "yt-dlp       {}",
                if self.yt.available() {
                    format!("{} · {}", self.yt.version(), self.yt.binary)
                } else {
                    "optional / missing".into()
                }
            ),
            format!(
                "cava         {}",
                if w("cava").is_some() {
                    "OK"
                } else {
                    "optional / missing"
                }
            ),
            format!(
                "mpris        {}",
                if on("mpris_enabled", true) {
                    self.mpris().status()
                } else {
                    "off".into()
                }
            ),
            format!(
                "notifications {}",
                if self.notifier.enabled() {
                    format!(
                        "ON · {}",
                        w("notify-send")
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    )
                } else if on("track_notifications", true) {
                    "enabled / unavailable".into()
                } else {
                    "off".into()
                }
            ),
            format!(
                "terminal     {}",
                std::env::var("TERM").unwrap_or_else(|_| "unknown".into())
            ),
            format!(
                "kitty        {}",
                if std::env::var_os("KITTY_WINDOW_ID").is_some() {
                    "YES"
                } else {
                    "no"
                }
            ),
            format!(
                "library      {} indexed tracks · schema {}",
                self.db.count().unwrap_or(0),
                self.db.schema_version().unwrap_or(0)
            ),
            format!(
                "navidrome    {}",
                if on("navidrome_enabled", true) {
                    if self.nav_configured() {
                        "enabled / configured"
                    } else {
                        "enabled / not configured"
                    }
                } else {
                    "disabled"
                }
            ),
            format!(
                "spotify      {}",
                if on("spotify_enabled", false) {
                    if self.spotify_configured() {
                        "enabled / connected"
                    } else {
                        "enabled / setup required"
                    }
                } else {
                    "disabled"
                }
            ),
        ];
        let e = &self.spotify.local;
        out.push(format!(
            "librespot     {}",
            if e.available() {
                format!(
                    "{} · {}",
                    if e.running() { "running" } else { "available" },
                    if e.authorized() {
                        "authorized"
                    } else {
                        "not authorized · Spotify → Local playback → AUTHORIZE"
                    }
                )
            } else {
                "optional / missing".into()
            }
        ));
        let stale = e.stale_receivers();
        if !stale.is_empty() {
            let pids: Vec<String> = stale.iter().map(|p| p.to_string()).collect();
            out.push(format!(
                "librespot    WARNING {} orphaned receiver(s) pid {} · fix: the next NPLAY Spotify start replaces them, or kill {}",
                stale.len(),
                pids.join(","),
                pids.join(" ")
            ));
        }
        let ed = |k: &str| if on(k, true) { "enabled" } else { "disabled" };
        out.push(format!("sr           {}", ed("sr_enabled")));
        out.push(format!("youtube      {}", ed("youtube_enabled")));
        out.push(format!("custom radio {}", ed("custom_radio_enabled")));
        out.push(format!("music roots  {}", self.roots().join(" : ")));
        out.push(format!("config       {}", self.cfg.path.display()));
        out.push(format!(
            "log          {} · {} recent warnings/errors",
            crate::log::log_path().display(),
            crate::log::recent_error_count()
        ));
        out.push(format!(
            "mpv log      {}",
            crate::log::mpv_log_path().display()
        ));
        if !vis_line.is_empty() {
            out.push(format!(
                "visualizer   {}",
                vis_line.replace("terminal off · ", "")
            ));
        }
        out.extend(crate::privacy::doctor_lines());
        out
    }

    /// Print the doctor report; exit code 0 when mpv is installed, else 1.
    pub fn doctor(&self, vis_line: &str) -> i32 {
        for l in self.doctor_lines(vis_line) {
            println!("{l}");
        }
        if crate::paths::which("mpv").is_some() {
            0
        } else {
            1
        }
    }

    /// Terminal/service exit cleanup; each step runs even if one fails. The
    /// local receiver closes before `stop_playback` so the background
    /// Spotify pause cannot respawn it.
    pub fn shutdown(&self) {
        if self.current().is_some() && self.player.active() {
            let pos = self.playback_position();
            self.checkpoint(Some(pos), None);
        }
        self.save_state();
        self.spotify.local.close();
        self.stop_playback(None);
        self.mpris().stop();
        self.statistics_tracker.flush(false);
    }

    /// Statistics: `nplay --stats-sync-now` (the coordinator prints it).
    pub fn statistics_sync_once(&self) -> std::result::Result<usize, crate::statistics::SyncError> {
        self.statistics_sync.once()
    }
}
