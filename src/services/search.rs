//! Federated Quick Find (1.x `services/search.py`).
//!
//! Local results show at once; Navidrome, Spotify, Sveriges Radio and
//! YouTube run in parallel worker threads and the list is republished as
//! each finishes. The final publish puts tracks available from several
//! sources first, each merged into the preferred source's row with
//! `available_sources` and `source_alternatives` meta.

use crate::app::{App, Row, UiRef};
use crate::core::supports;
use crate::model::Track;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const ORDER: [&str; 5] = ["LOCAL", "NAVIDROME", "SPOTIFY", "SR", "YOUTUBE"];

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// Tracks found in more than one source, merged (1.x `publish(final=True)`).
pub fn merge_matches(groups: &HashMap<String, Vec<Track>>, preferred: &str) -> Vec<Track> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut by: HashMap<(String, String), Vec<Track>> = HashMap::new();
    for name in ORDER {
        for t in groups.get(name).into_iter().flatten() {
            if t.kind != "track" {
                continue;
            }
            let key = (norm(&t.artist), norm(&t.title));
            if key.0.is_empty() || key.1.is_empty() {
                continue;
            }
            if !by.contains_key(&key) {
                order.push(key.clone());
            }
            by.entry(key).or_default().push(t.clone());
        }
    }
    let rank = |s: &str| -> i32 {
        if s == preferred {
            return 0;
        }
        match s {
            "local" => 1,
            "navidrome" => 2,
            "spotify" => 3,
            "youtube" => 4,
            _ => 9,
        }
    };
    let mut merged = Vec::new();
    for key in order {
        let vv = &by[&key];
        let mut sources: Vec<String> = vv.iter().map(|x| x.source.clone()).collect();
        sources.sort();
        sources.dedup();
        if sources.len() < 2 {
            continue;
        }
        let mut sorted = vv.clone();
        sorted.sort_by_key(|x| rank(&x.source));
        let mut base = sorted[0].clone();
        base.meta.insert(
            "available_sources".into(),
            Value::Array(sources.into_iter().map(Value::String).collect()),
        );
        base.meta.insert(
            "source_alternatives".into(),
            Value::Array(vv.iter().map(Track::to_value).collect()),
        );
        merged.push(base);
    }
    merged
}

fn publish(app: &App, ui: &UiRef, q: &str, groups: &Mutex<HashMap<String, Vec<Track>>>, fin: bool) {
    let snapshot = groups.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut xs: Vec<Row> = Vec::new();
    if fin {
        let pref = app.cfg.get("preferred_source", "local");
        let merged = merge_matches(&snapshot, &pref);
        if !merged.is_empty() {
            xs.push(Row::entry(
                "AVAILABLE FROM MULTIPLE SOURCES",
                "",
                format!("{} matched tracks · preferred {pref}", merged.len()),
            ));
            xs.extend(merged.into_iter().take(20).map(Row::Track));
        }
    }
    for name in ORDER {
        if let Some(g) = snapshot.get(name).filter(|g| !g.is_empty()) {
            xs.push(Row::entry(name, "", format!("{} results", g.len())));
            xs.extend(g.iter().cloned().map(Row::Track));
        }
    }
    let total: usize = ORDER
        .iter()
        .map(|n| snapshot.get(*n).map(Vec::len).unwrap_or(0))
        .sum();
    let parts: Vec<String> = ORDER
        .iter()
        .filter_map(|n| snapshot.get(*n).map(|g| format!("{n} {}", g.len())))
        .collect();
    let msg = parts.join(" · ")
        + if fin {
            " · complete"
        } else {
            " · searching…"
        };
    ui.set_sort_mode("relevance");
    ui.replace_list(
        &format!("QUICK FIND · {q} · {total} RESULTS"),
        xs,
        "No items",
        Some(&msg),
    );
}

type Job = Box<dyn FnOnce() -> crate::core::Result<Vec<Track>> + Send>;

/// Quick Find across every enabled source.
pub fn search(app: &Arc<App>, q: &str, ui: &UiRef) {
    if q.is_empty() {
        return;
    }
    let local = app.db.search(q, 30).unwrap_or_default();
    ui.loading(
        &format!("QUICK FIND · {q}"),
        &format!("Local {} · searching connected sources…", local.len()),
        crate::app::track_rows(local.clone()),
    );
    let groups = Arc::new(Mutex::new(HashMap::from([("LOCAL".to_string(), local)])));
    publish(app, ui, q, &groups, false);
    let mut jobs: Vec<(&'static str, Job)> = Vec::new();
    let qs = q.to_string();
    if let Some(n) = app.nav() {
        if supports("navidrome", "search") {
            let q = qs.clone();
            jobs.push((
                "NAVIDROME",
                Box::new(move || Ok(n.search_library(&q, 10, 6, 4)?)),
            ));
        }
    }
    if supports("spotify", "search")
        && app.cfg.getbool("spotify_enabled", false)
        && app.spotify_configured()
    {
        let sp = app.spotify.clone();
        let q = qs.clone();
        jobs.push(("SPOTIFY", Box::new(move || sp.search(&q, 6))));
    }
    if supports("sr", "search") && app.cfg.getbool("sr_enabled", true) {
        let sr = app.sr.clone();
        let q = qs.clone();
        jobs.push((
            "SR",
            Box::new(move || Ok(sr.search_programmes_and_episodes(&q, 18))),
        ));
    }
    if supports("youtube", "search")
        && app.cfg.getbool("youtube_enabled", true)
        && app.yt.available()
    {
        let yt = app.yt.clone();
        let q = qs.clone();
        jobs.push(("YOUTUBE", Box::new(move || yt.search(&q, 10))));
    }
    if jobs.is_empty() {
        publish(app, ui, q, &groups, true);
        return;
    }
    let remaining = Arc::new(Mutex::new(jobs.len()));
    for (name, job) in jobs {
        let (app, ui, groups, remaining, q) = (
            app.clone(),
            ui.clone(),
            groups.clone(),
            remaining.clone(),
            qs.clone(),
        );
        std::thread::spawn(move || {
            let res = match job() {
                Ok(r) => r,
                Err(e) => {
                    app.log_debug(&format!("Quick Find {name} failed"), Some(&e));
                    Vec::new()
                }
            };
            let fin = {
                let mut g = groups.lock().unwrap_or_else(|e| e.into_inner());
                g.insert(name.to_string(), res);
                let mut r = remaining.lock().unwrap_or_else(|e| e.into_inner());
                *r -= 1;
                *r == 0
            };
            let u = ui.clone();
            ui.post(Box::new(move || publish(&app, &u, &q, &groups, fin)));
        });
    }
}
