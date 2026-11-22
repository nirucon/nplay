//! Browser launching for Spotify consent pages (Web API PKCE and librespot
//! OAuth), port of 1.x `_browser_commands` / `_open_authorization_url`.
//!
//! Candidates, in preference order: every `$BROWSER` entry (`:` separated,
//! `%s` is the URL, else the URL is appended), `omarchy-launch-browser`, the
//! desktop default (`xdg-settings get default-web-browser`, then
//! `xdg-mime query default x-scheme-handler/https`, resolved through its
//! `.desktop` Exec line), known browsers on `$PATH`, then `xdg-open` and
//! `gio open`. The first candidate that spawns wins.
//!
//! Public API:
//! - `browser_commands(url) -> Vec<(Vec<String>, String)>`: argv + kind
//!   ("BROWSER", "omarchy", "desktop", "known", "fallback"), not launched.
//! - `open_authorization_url(url) -> bool`: launch the first candidate.
//! - `desktop_browser_command(desktop_id) -> Option<Vec<String>>`: a
//!   `.desktop` Exec line as argv with a `{url}` placeholder.
//! - `shlex_split(s) -> Option<Vec<String>>`: POSIX `shlex.split`.

use crate::config::Ini;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const KNOWN: &[&str] = &[
    "helium",
    "firefox",
    "firefox-esr",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "brave-browser",
    "brave",
    "microsoft-edge",
    "microsoft-edge-stable",
    "vivaldi",
    "opera",
    "qutebrowser",
    "epiphany",
    "falkon",
    "konqueror",
];

/// Python `shlex.split` (POSIX mode, no comments). `None` on an unclosed
/// quote or a trailing backslash (Python raises ValueError).
pub fn shlex_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            '\\' => {
                cur.push(chars.next()?);
                have = true;
            }
            '\'' => {
                have = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        x => cur.push(x),
                    }
                }
            }
            '"' => {
                have = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let n = chars.next()?;
                            if !matches!(n, '\\' | '"' | '$' | '`' | '\n') {
                                cur.push('\\');
                            }
                            cur.push(n);
                        }
                        x => cur.push(x),
                    }
                }
            }
            x => {
                cur.push(x);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    Some(out)
}

fn home() -> PathBuf {
    crate::paths::home_dir()
}

/// Resolve a `.desktop` file to an argv list with a `{url}` placeholder.
pub fn desktop_browser_command(desktop_id: &str) -> Option<Vec<String>> {
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    desktop_browser_command_in(desktop_id, &home(), &dirs)
}

pub(crate) fn desktop_browser_command_in(
    desktop_id: &str,
    home: &Path,
    xdg_data_dirs: &str,
) -> Option<Vec<String>> {
    if desktop_id.is_empty() {
        return None;
    }
    let mut roots = vec![
        home.join(".local/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ];
    roots.extend(
        xdg_data_dirs
            .split(':')
            .filter(|p| !p.is_empty())
            .map(|p| Path::new(p).join("applications")),
    );
    let mut seen = HashSet::new();
    for root in roots {
        let path = if desktop_id.starts_with('/') {
            PathBuf::from(desktop_id)
        } else {
            root.join(desktop_id)
        };
        if seen.contains(&path) || !path.is_file() {
            continue;
        }
        seen.insert(path.clone());
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let ini = Ini::parse(&text);
        let exec_line = ini.get("Desktop Entry", "Exec").unwrap_or("").trim();
        if exec_line.is_empty() {
            continue;
        }
        let Some(cmd) = shlex_split(exec_line) else {
            continue;
        };
        if cmd.is_empty() {
            continue;
        }
        let mut has_url = false;
        let mut out = Vec::new();
        for arg in cmd {
            if matches!(arg.as_str(), "%u" | "%U" | "%f" | "%F") {
                out.push("{url}".to_string());
                has_url = true;
            } else if matches!(arg.as_str(), "%i" | "%c" | "%k") {
                continue;
            } else if ["%u", "%U", "%f", "%F"].iter().any(|c| arg.contains(c)) {
                out.push(
                    arg.replace("%u", "{url}")
                        .replace("%U", "{url}")
                        .replace("%f", "{url}")
                        .replace("%F", "{url}"),
                );
                has_url = true;
            } else {
                out.push(arg.replace("%%", "%"));
            }
        }
        if !has_url {
            out.push("{url}".to_string());
        }
        return Some(out);
    }
    None
}

/// Run a short helper and return its stdout (bounded wait, killed after).
pub(crate) fn run_capture(exe: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

/// Build browser launch candidates in preference order, without launching.
pub fn browser_commands(url: &str) -> Vec<(Vec<String>, String)> {
    let browser = std::env::var("BROWSER").unwrap_or_default();
    browser_commands_with(url, &browser, &|n| crate::paths::which(n), &|exe, args| {
        run_capture(exe, args, Duration::from_secs(3))
    })
}

type Which<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;
type Query<'a> = &'a dyn Fn(&Path, &[&str]) -> Option<String>;

pub(crate) fn browser_commands_with(
    url: &str,
    browser_env: &str,
    which: Which,
    query: Query,
) -> Vec<(Vec<String>, String)> {
    let mut candidates: Vec<(Vec<String>, String)> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    let mut add = |cmd: Vec<String>, kind: &str| {
        if cmd.is_empty() {
            return;
        }
        if seen.insert(cmd.clone()) {
            candidates.push((cmd, kind.to_string()));
        }
    };
    let configured = browser_env.trim();
    if !configured.is_empty() {
        for entry in configured.split(':') {
            let Some(cmd) = shlex_split(entry) else {
                continue;
            };
            if cmd.is_empty() {
                continue;
            }
            let cmd = if cmd.iter().any(|x| x.contains("%s")) {
                cmd.iter().map(|x| x.replace("%s", "{url}")).collect()
            } else {
                let mut c = cmd;
                c.push("{url}".into());
                c
            };
            add(cmd, "BROWSER");
        }
    }
    // Omarchy: its launcher honours the user's chosen browser and Hyprland rules.
    if let Some(o) = which("omarchy-launch-browser") {
        add(
            vec![o.to_string_lossy().into_owned(), "{url}".into()],
            "omarchy",
        );
    }
    for (tool, args) in [
        ("xdg-settings", &["get", "default-web-browser"][..]),
        (
            "xdg-mime",
            &["query", "default", "x-scheme-handler/https"][..],
        ),
    ] {
        let Some(exe) = which(tool) else { continue };
        let out = query(&exe, args).unwrap_or_default();
        let desktop = out.trim().lines().next().unwrap_or("").trim().to_string();
        if let Some(cmd) = desktop_browser_command(&desktop) {
            add(cmd, "desktop");
        }
    }
    for name in KNOWN {
        if let Some(exe) = which(name) {
            add(
                vec![exe.to_string_lossy().into_owned(), "{url}".into()],
                "known",
            );
        }
    }
    for (tool, args) in [("xdg-open", &[][..]), ("gio", &["open"][..])] {
        if let Some(exe) = which(tool) {
            let mut c = vec![exe.to_string_lossy().into_owned()];
            c.extend(args.iter().map(|s: &&str| s.to_string()));
            c.push("{url}".into());
            add(c, "fallback");
        }
    }
    candidates
        .into_iter()
        .map(|(cmd, kind)| (cmd.iter().map(|x| x.replace("{url}", url)).collect(), kind))
        .collect()
}

/// Spawn a detached command (own session, no stdio); a short-lived thread
/// reaps it so no zombie is left behind.
pub(crate) fn spawn_detached(cmd: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let (exe, args) = cmd
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let mut c = Command::new(exe);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn()?;
    std::thread::Builder::new()
        .name("nplay-browser-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

/// Open the Spotify consent URL with the system or a common browser.
pub fn open_authorization_url(url: &str) -> bool {
    open_with(browser_commands(url), &|c| spawn_detached(c))
}

pub(crate) fn open_with(
    candidates: Vec<(Vec<String>, String)>,
    launch: &dyn Fn(&[String]) -> std::io::Result<()>,
) -> bool {
    for (command, kind) in candidates {
        match launch(&command) {
            Ok(()) => {
                info!(
                    "Spotify authorization browser candidate tried: {} ({})",
                    command[0], kind
                );
                return true;
            }
            Err(e) => warn!("Could not open Spotify browser {}: {}", command[0], e),
        }
    }
    warn!("Spotify authorization browser fallback failed: no browser candidate");
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<PathBuf> {
        None
    }
    fn noq(_: &Path, _: &[&str]) -> Option<String> {
        None
    }
    const U: &str = "https://example.test/auth";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn browser_env_with_placeholder() {
        let c = browser_commands_with(U, "firefox %s", &none, &noq);
        assert_eq!(c[0], (s(&["firefox", U]), "BROWSER".into()));
    }

    #[test]
    fn browser_env_without_placeholder() {
        let c = browser_commands_with(U, "helium", &none, &noq);
        assert_eq!(c[0], (s(&["helium", U]), "BROWSER".into()));
    }

    #[test]
    fn browser_env_supports_multiple_candidates() {
        let c = browser_commands_with(U, "firefox:chromium %s", &none, &noq);
        assert_eq!(
            c[..2],
            [
                (s(&["firefox", U]), "BROWSER".to_string()),
                (s(&["chromium", U]), "BROWSER".to_string())
            ]
        );
    }

    #[test]
    fn order_and_dedup() {
        let which = |n: &str| -> Option<PathBuf> {
            matches!(
                n,
                "omarchy-launch-browser" | "firefox" | "xdg-open" | "xdg-settings"
            )
            .then(|| PathBuf::from(format!("/bin/{n}")))
        };
        let dir = std::env::temp_dir().join(format!("nplay-br-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let desk = dir.join("ff.desktop");
        std::fs::write(&desk, "[Desktop Entry]\nExec=/bin/firefox %u\n").unwrap();
        let d = desk.to_string_lossy().into_owned();
        let q = move |_: &Path, _: &[&str]| Some(format!("{d}\n"));
        let c = browser_commands_with(U, "/bin/firefox", &which, &q);
        let kinds: Vec<&str> = c.iter().map(|x| x.1.as_str()).collect();
        // $BROWSER and the desktop entry are the same argv: listed once.
        assert_eq!(kinds, ["BROWSER", "omarchy", "fallback"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn desktop_exec_field_codes() {
        let dir = std::env::temp_dir().join(format!("nplay-br-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let desk = dir.join("helium.desktop");
        std::fs::write(
            &desk,
            "[Desktop Entry]\nName=Helium\nExec=helium --new-window %U --label=%%\n",
        )
        .unwrap();
        assert_eq!(
            desktop_browser_command_in(&desk.to_string_lossy(), &dir, "").unwrap(),
            s(&["helium", "--new-window", "{url}", "--label=%"])
        );
        let desk2 = dir.join("browser.desktop");
        std::fs::write(&desk2, "[Desktop Entry]\nExec=browser --incognito\n").unwrap();
        assert_eq!(
            desktop_browser_command_in(&desk2.to_string_lossy(), &dir, "").unwrap(),
            s(&["browser", "--incognito", "{url}"])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn desktop_lookup_falls_through_to_system_dirs() {
        let dir = std::env::temp_dir().join(format!("nplay-br-{}", fastrand::u64(..)));
        assert!(
            desktop_browser_command_in("nplay-test-missing-browser.desktop", &dir, "").is_none()
        );
        assert!(desktop_browser_command_in("", &dir, "").is_none());
    }

    #[test]
    fn open_uses_first_candidate_and_reports() {
        let calls = std::sync::Mutex::new(Vec::new());
        let ok = open_with(browser_commands_with(U, "firefox %s", &none, &noq), &|c| {
            calls.lock().unwrap().push(c.to_vec());
            Ok(())
        });
        assert!(ok);
        assert_eq!(*calls.lock().unwrap(), vec![s(&["firefox", U])]);
        assert!(!open_with(
            browser_commands_with(U, "", &none, &noq),
            &|_| Ok(())
        ));
        // A failing candidate falls through to the next one.
        let n = std::sync::atomic::AtomicUsize::new(0);
        let ok = open_with(browser_commands_with(U, "a:b", &none, &noq), &|_| {
            if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err(std::io::Error::other("no"))
            } else {
                Ok(())
            }
        });
        assert!(ok);
    }

    #[test]
    fn shlex() {
        assert_eq!(
            shlex_split("a 'b c' \"d\\\"e\" f\\ g").unwrap(),
            s(&["a", "b c", "d\"e", "f g"])
        );
        assert_eq!(shlex_split("''").unwrap(), s(&[""]));
        assert!(shlex_split("a 'b").is_none());
        assert!(shlex_split("a \\").is_none());
        assert_eq!(shlex_split("  ").unwrap(), Vec::<String>::new());
    }
}
