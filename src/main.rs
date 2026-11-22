//! NPLAY: terminal music, radio and podcast player built around mpv.
// Development of the Rust port: the modules are filled in phases.
#![allow(dead_code)]

#[macro_use]
pub mod log;
pub mod app;
pub mod artwork;
pub mod artwork_cache;
pub mod config;
pub mod core;
pub mod db;
pub mod model;
pub mod mpris;
pub mod navigation;
pub mod notifications;
pub mod paths;
pub mod player;
pub mod privacy;
pub mod providers;
pub mod redact;
pub mod service;
pub mod services;
pub mod spotify;
pub mod statistics;
pub mod theme;
pub mod tui;
pub mod visualizer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str =
    "usage: nplay [-h] [--version] [--doctor] [--scan] [--list-themes] [--check-theme FILE]
             [--stats-set-token] [--stats-sync-now] [--serve] [--service-socket PATH]
             [--idle-exit SECONDS]
             [query ...]";

const HELP: &str = "
positional arguments:
  query

options:
  -h, --help            show this help message and exit
  --version
  --doctor
  --scan
  --list-themes
  --check-theme FILE
  --stats-set-token
  --stats-sync-now
  --serve               Run the headless local control service
  --service-socket PATH
                        Control socket path (default: XDG runtime directory)
  --idle-exit SECONDS   Exit after this many seconds without clients (service mode)";

#[derive(Default)]
struct Args {
    query: Vec<String>,
    version: bool,
    doctor: bool,
    scan: bool,
    list_themes: bool,
    check_theme: Option<String>,
    stats_set_token: bool,
    stats_sync_now: bool,
    serve: bool,
    service_socket: Option<String>,
    idle_exit: u64,
}

fn usage_error(msg: &str) -> ! {
    eprintln!("{USAGE}\nnplay: error: {msg}");
    std::process::exit(2);
}

/// argparse-compatible parsing (`--opt value` and `--opt=value`).
fn parse_args(raw: Vec<String>) -> Args {
    let mut a = Args::default();
    let mut it = raw.into_iter();
    let mut rest_positional = false;
    while let Some(arg) = it.next() {
        if rest_positional || !arg.starts_with('-') || arg == "-" {
            a.query.push(arg);
            continue;
        }
        if arg == "--" {
            rest_positional = true;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |n: &str| -> String {
            inline
                .clone()
                .or_else(|| it.next())
                .unwrap_or_else(|| usage_error(&format!("argument {n}: expected one argument")))
        };
        match name.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}\n{HELP}");
                std::process::exit(0);
            }
            "--version" => a.version = true,
            "--doctor" => a.doctor = true,
            "--scan" => a.scan = true,
            "--list-themes" => a.list_themes = true,
            "--check-theme" => a.check_theme = Some(value("--check-theme")),
            "--stats-set-token" => a.stats_set_token = true,
            "--stats-sync-now" => a.stats_sync_now = true,
            "--serve" => a.serve = true,
            "--service-socket" => a.service_socket = Some(value("--service-socket")),
            "--idle-exit" => {
                let v = value("--idle-exit");
                a.idle_exit = v
                    .trim()
                    .parse::<i64>()
                    .map(|n| n.max(0) as u64)
                    .unwrap_or_else(|_| {
                        usage_error(&format!("argument --idle-exit: invalid int value: '{v}'"))
                    })
            }
            _ => usage_error(&format!("unrecognized arguments: {arg}")),
        }
    }
    a
}

/// Read one line from the terminal without echo (Python `getpass`).
fn read_hidden(prompt: &str) -> String {
    use std::io::{BufRead, Write};
    let fd = libc::STDIN_FILENO;
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::tcgetattr(fd, &mut old) } == 0;
    if tty {
        let mut t = old;
        t.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    if tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
        eprintln!();
    }
    line.trim_end_matches(['\n', '\r']).to_string()
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn main() {
    let args = parse_args(std::env::args().skip(1).collect());
    if args.serve {
        std::process::exit(service::run_service(
            args.service_socket.map(std::path::PathBuf::from),
            args.idle_exit,
        ));
    }
    if args.version {
        println!("NPLAY {VERSION}");
        return;
    }
    if args.list_themes {
        for l in theme::list_themes_lines() {
            println!("{l}");
        }
        return;
    }
    if let Some(f) = args.check_theme {
        let (ok, msg) = theme::validate_theme(&f);
        println!("{msg}");
        std::process::exit(if ok { 0 } else { 1 });
    }
    if args.stats_set_token {
        let token = read_hidden("Dedicated statistics API token (hidden): ");
        let token = token.trim();
        if token.is_empty() {
            fail("Cancelled: empty token");
        }
        if let Err(e) = config::Config::load().save_statistics_token(token) {
            fail(&e);
        }
        println!("Statistics token stored locally (0600). Enable sync separately in NPLAY Settings & Sync.");
        return;
    }
    let interactive = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let needs_app = args.stats_sync_now || args.doctor || args.scan || interactive;
    if !needs_app {
        fail("NPLAY needs an interactive terminal.");
    }
    let a = match app::App::new() {
        Ok(a) => a,
        Err(e) => fail(&format!("NPLAY failed to start: {e}")),
    };
    if args.stats_sync_now {
        match a.statistics_sync_once() {
            Ok(n) => println!("Acknowledged: {n}"),
            Err(e) => fail(&format!("Sync failed: {e}")),
        }
        return;
    }
    if args.doctor {
        let vis = service::probe::vis_line(false, None);
        std::process::exit(a.doctor(&vis));
    }
    if args.scan {
        match a.scan(None, false) {
            Ok(r) => println!("{}", app::App::scan_message(&r)),
            Err(e) => fail(&format!("Scan failed: {e}")),
        }
        return;
    }
    if paths::which("mpv").is_none() {
        fail("mpv is required. Run: nplay --doctor");
    }
    let code = tui::run(a.clone(), &args.query.join(" "));
    a.shutdown();
    std::process::exit(code);
}
