//! Privacy self-checks for `nplay --doctor` (port of 1.x `nplay/privacy.py`).
//!
//! Public API:
//! - [`Check`] `{ ok, label, detail, fix }`: `fix` is one shell line or
//!   instruction, empty when the check passed. Nothing here returns a secret:
//!   log scans report counts and file names only.
//! - [`run_checks()`] / [`run_checks_in(&PrivacyEnv)`]: the six checks in 1.x
//!   order (`state dir`, `log modes`, `log secrets`, `secrets`, `cava config`,
//!   `argv`).
//! - [`doctor_lines()`]: `privacy {label:<13}{OK|WARN} · {detail}` plus
//!   `  fix: ...` after each warning that has a fix.
//! - [`PrivacyEnv`]: the directories checked (defaults from `crate::paths`);
//!   tests build one over temp dirs.

use crate::redact::contains_secret;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub ok: bool,
    pub label: String,
    pub detail: String,
    pub fix: String,
}

fn check(ok: bool, label: &str, detail: String, fix: String) -> Check {
    Check {
        ok,
        label: label.into(),
        detail,
        fix,
    }
}

/// Where the checks look.
#[derive(Clone, Debug)]
pub struct PrivacyEnv {
    /// `$XDG_STATE_HOME/nplay` (logs, session).
    pub state: PathBuf,
    /// `$XDG_CONFIG_HOME/nplay` (secrets.ini).
    pub config: PathBuf,
    /// The shared temp dir scanned for old `/tmp` leftovers.
    pub tmp: PathBuf,
    /// NPLAY's private runtime dir (CAVA config, mpv socket).
    pub runtime: PathBuf,
    /// Home, for `~` shortening in messages.
    pub home: String,
    /// `/proc`, for the argv check.
    pub proc_dir: PathBuf,
}

impl PrivacyEnv {
    pub fn from_paths() -> PrivacyEnv {
        PrivacyEnv {
            state: crate::paths::state(),
            config: crate::paths::config(),
            tmp: std::env::temp_dir(),
            runtime: crate::paths::private_runtime_dir(),
            home: crate::paths::home_dir().to_string_lossy().into_owned(),
            proc_dir: PathBuf::from("/proc"),
        }
    }

    /// NPLAY-owned logs (`nplay.log`, `.1`-`.9`, `mpv.log`) that exist.
    pub fn log_files(&self) -> Vec<PathBuf> {
        let base = self.state.join("nplay.log");
        let mut out = vec![base.clone()];
        for i in 1..10 {
            out.push(PathBuf::from(format!("{}.{i}", base.display())));
        }
        out.push(self.state.join("mpv.log"));
        out.into_iter().filter(|p| p.exists()).collect()
    }

    fn short(&self, p: &Path) -> String {
        let s = p.to_string_lossy();
        if !self.home.is_empty() {
            if let Some(rest) = s.strip_prefix(&format!("{}/", self.home)) {
                return format!("~/{rest}");
            }
        }
        s.into_owned()
    }
}

fn mode(p: &Path) -> std::io::Result<u32> {
    Ok(std::fs::metadata(p)?.permissions().mode() & 0o7777)
}

fn strerror(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
            .to_string_lossy()
            .into_owned(),
        None => e.to_string(),
    }
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn check_state_dir(env: &PrivacyEnv) -> Check {
    let s = env.short(&env.state);
    match mode(&env.state) {
        Err(e) => check(
            false,
            "state dir",
            format!("{s} unreadable ({})", strerror(&e)),
            format!("mkdir -p {s} && chmod 700 {s}"),
        ),
        Ok(m) if m & 0o077 != 0 => check(
            false,
            "state dir",
            format!("{s} mode {m:03o}"),
            format!("chmod 700 {s}"),
        ),
        Ok(m) => check(
            true,
            "state dir",
            format!("{s} mode {m:03o}"),
            String::new(),
        ),
    }
}

pub fn check_log_modes(env: &PrivacyEnv) -> Check {
    let files = env.log_files();
    let loose: Vec<(PathBuf, u32)> = files
        .iter()
        .filter_map(|p| mode(p).ok().map(|m| (p.clone(), m)))
        .filter(|(_, m)| m & 0o077 != 0)
        .collect();
    if !loose.is_empty() {
        let names: Vec<String> = loose.iter().map(|(p, _)| env.short(p)).collect();
        let detail: Vec<String> = loose
            .iter()
            .map(|(p, m)| format!("{} {m:03o}", name(p)))
            .collect();
        return check(
            false,
            "log modes",
            detail.join(", "),
            format!("chmod 600 {}", names.join(" ")),
        );
    }
    check(
        true,
        "log modes",
        format!("{} files · 600", files.len()),
        String::new(),
    )
}

pub fn check_logs_clean(env: &PrivacyEnv) -> Check {
    let dirty: Vec<PathBuf> = env
        .log_files()
        .into_iter()
        .filter(|p| {
            std::fs::read(p)
                .map(|b| contains_secret(&String::from_utf8_lossy(&b)))
                .unwrap_or(false)
        })
        .collect();
    if !dirty.is_empty() {
        return check(
            false,
            "log secrets",
            format!(
                "credentials found in {}",
                dirty.iter().map(|p| name(p)).collect::<Vec<_>>().join(", ")
            ),
            format!(
                "start nplay once (the start-up migration redacts them) or delete: rm {}",
                dirty
                    .iter()
                    .map(|p| env.short(p))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
    }
    check(
        true,
        "log secrets",
        "none in nplay.log, rotations, mpv.log".into(),
        String::new(),
    )
}

pub fn check_secrets_file(env: &PrivacyEnv) -> Check {
    let p = env.config.join("secrets.ini");
    if !p.exists() {
        return check(true, "secrets", "no secrets file".into(), String::new());
    }
    let s = env.short(&p);
    match mode(&p) {
        Ok(m) if m & 0o077 != 0 => check(
            false,
            "secrets",
            format!("{s} mode {m:03o}"),
            format!("chmod 600 {s}"),
        ),
        Ok(m) => check(true, "secrets", format!("{s} mode {m:03o}"), String::new()),
        Err(_) => check(
            false,
            "secrets_file",
            "check failed: OSError".into(),
            String::new(),
        ),
    }
}

/// CAVA config and mpv IPC socket location (private runtime dir, no /tmp leftovers).
pub fn check_runtime(env: &PrivacyEnv) -> Check {
    let uid = unsafe { libc::getuid() };
    let mut leftovers: Vec<String> = Vec::new();
    let tmp = env.tmp.to_string_lossy().into_owned();
    if let Ok(rd) = std::fs::read_dir(&env.tmp) {
        let mpv_prefix = format!("nplay-mpv-{uid}-");
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let hit = (n.starts_with("nplay-cava-") && n.ends_with(".conf"))
                || (n.starts_with(&mpv_prefix) && n.ends_with(".sock"));
            if hit {
                let full = env.tmp.join(&n);
                if std::fs::symlink_metadata(&full).is_ok_and(|m| m.uid() == uid) {
                    leftovers.push(full.to_string_lossy().into_owned());
                }
            }
        }
    }
    let d = env.short(&env.runtime);
    let m = mode(&env.runtime).unwrap_or(0o777);
    if m & 0o077 != 0 {
        return check(
            false,
            "cava config",
            format!("{d} mode {m:03o}"),
            format!("chmod 700 {d}"),
        );
    }
    if !leftovers.is_empty() {
        let mut fix = format!(
            "rm -f {}",
            leftovers
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        );
        if leftovers.len() > 4 {
            fix.push_str(" …");
        }
        return check(
            false,
            "cava config",
            format!("{d} · {} old file(s) in {tmp}", leftovers.len()),
            fix,
        );
    }
    check(
        true,
        "cava config",
        format!("{d} (700), removed when CAVA stops"),
        String::new(),
    )
}

/// No running mpv of this user carries a credential in its argv (older NPLAY).
pub fn check_process_argv(env: &PrivacyEnv) -> Check {
    let uid = unsafe { libc::getuid() };
    let Ok(rd) = std::fs::read_dir(&env.proc_dir) else {
        return check(true, "argv", "not checked (no /proc)".into(), String::new());
    };
    let mut bad = 0;
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let dir = env.proc_dir.join(&n);
        if !std::fs::metadata(&dir).is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let Ok(raw) = std::fs::read(dir.join("cmdline")) else {
            continue;
        };
        let argv: String = String::from_utf8_lossy(&raw).replace('\0', " ");
        let argv0 = argv.split(' ').next().unwrap_or("");
        if argv0.contains("mpv") && contains_secret(&argv) {
            bad += 1;
        }
    }
    if bad > 0 {
        return check(
            false,
            "argv",
            format!("{bad} mpv process(es) with a credential in argv (NPLAY <= 1.7)"),
            "quit nplay and restart NIRUAMP / `nplay --serve` so mpv restarts under NPLAY 1.8"
                .into(),
        );
    }
    check(
        true,
        "argv",
        "no credential in mpv argv".into(),
        String::new(),
    )
}

pub fn run_checks_in(env: &PrivacyEnv) -> Vec<Check> {
    vec![
        check_state_dir(env),
        check_log_modes(env),
        check_logs_clean(env),
        check_secrets_file(env),
        check_runtime(env),
        check_process_argv(env),
    ]
}

pub fn run_checks() -> Vec<Check> {
    run_checks_in(&PrivacyEnv::from_paths())
}

/// Format checks as `--doctor` lines.
pub fn format_lines(checks: &[Check]) -> Vec<String> {
    let mut lines = Vec::new();
    for c in checks {
        lines.push(format!(
            "privacy {:<13}{} · {}",
            c.label,
            if c.ok { "OK" } else { "WARN" },
            c.detail
        ));
        if !c.ok && !c.fix.is_empty() {
            lines.push(format!("  fix: {}", c.fix));
        }
    }
    lines
}

pub fn doctor_lines() -> Vec<String> {
    format_lines(&run_checks())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Env {
        root: PathBuf,
        env: PrivacyEnv,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn setup() -> Env {
        let root = std::env::temp_dir().join(format!("nplay-privacy-test-{:x}", fastrand::u64(..)));
        let env = PrivacyEnv {
            state: root.join("home/.local/state/nplay"),
            config: root.join("home/.config/nplay"),
            tmp: root.join("tmp"),
            runtime: root.join("run/nplay"),
            home: root.join("home").to_string_lossy().into_owned(),
            proc_dir: PathBuf::from("/proc"),
        };
        for (d, m) in [
            (&env.state, 0o700),
            (&env.config, 0o700),
            (&env.tmp, 0o1777),
            (&env.runtime, 0o700),
        ] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(m)).unwrap();
        }
        Env { root, env }
    }

    fn by_label(env: &PrivacyEnv) -> HashMap<String, Check> {
        run_checks_in(env)
            .into_iter()
            .map(|c| (c.label.clone(), c))
            .collect()
    }

    fn write(p: &Path, text: &str, m: u32) {
        std::fs::write(p, text).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    }

    #[test]
    fn clean_state_passes() {
        let e = setup();
        let env = &e.env;
        write(&env.state.join("nplay.log"), "2026-11-05 INFO nplay: mpv start source=https://m/rest/stream.view?u=a&t=REDACTED&s=REDACTED\n", 0o600);
        let checks = by_label(env);
        for label in [
            "state dir",
            "log modes",
            "log secrets",
            "argv",
            "secrets",
            "cava config",
        ] {
            assert!(checks[label].ok, "{label}: {:?}", checks[label]);
        }
        assert_eq!(checks["state dir"].detail, "~/.local/state/nplay mode 700");
        assert_eq!(checks["log modes"].detail, "1 files · 600");
        assert_eq!(checks["secrets"].detail, "no secrets file");
        assert!(checks["cava config"]
            .detail
            .ends_with("(700), removed when CAVA stops"));
        let lines = format_lines(&run_checks_in(env));
        assert_eq!(lines.len(), 6);
        assert_eq!(
            lines[0],
            "privacy state dir    OK · ~/.local/state/nplay mode 700"
        );
    }

    #[test]
    fn warnings_have_a_fix_and_no_secret() {
        let e = setup();
        let env = &e.env;
        let token = "5f4dcc3b5aa765d61d8327deb882cf99";
        write(
            &env.state.join("mpv.log"),
            &format!("Opening https://m/rest/stream.view?u=a&t={token}&s=abc123\n"),
            0o664,
        );
        write(
            &env.config.join("secrets.ini"),
            "[navidrome]\npassword = x\n",
            0o644,
        );
        std::fs::set_permissions(&env.state, std::fs::Permissions::from_mode(0o755)).unwrap();
        let checks = by_label(env);
        assert!(!checks["log modes"].ok);
        assert_eq!(checks["log modes"].detail, "mpv.log 664");
        assert_eq!(
            checks["log modes"].fix,
            "chmod 600 ~/.local/state/nplay/mpv.log"
        );
        assert!(!checks["log secrets"].ok);
        assert_eq!(checks["log secrets"].detail, "credentials found in mpv.log");
        assert!(!checks["log secrets"].fix.is_empty());
        assert_eq!(
            checks["secrets"].fix,
            "chmod 600 ~/.config/nplay/secrets.ini"
        );
        assert_eq!(checks["state dir"].fix, "chmod 700 ~/.local/state/nplay");
        let lines = format_lines(&run_checks_in(env));
        let all = lines.join("\n");
        assert!(!all.contains(token));
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("privacy") && line.contains("WARN") {
                assert!(lines[i + 1].starts_with("  fix: "), "{line}");
            }
        }
    }

    #[test]
    fn missing_state_dir_and_loose_runtime() {
        let e = setup();
        let mut env = e.env.clone();
        env.state = e.root.join("home/missing");
        let c = check_state_dir(&env);
        assert!(!c.ok);
        assert_eq!(c.detail, "~/missing unreadable (No such file or directory)");
        assert_eq!(c.fix, "mkdir -p ~/missing && chmod 700 ~/missing");
        std::fs::set_permissions(&env.runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = check_runtime(&env);
        assert!(!c.ok);
        assert!(c.fix.starts_with("chmod 700 "));
        let mut noproc = env.clone();
        noproc.proc_dir = e.root.join("noproc");
        assert_eq!(check_process_argv(&noproc).detail, "not checked (no /proc)");
    }

    #[test]
    fn tmp_cava_leftover_is_reported() {
        let e = setup();
        let env = &e.env;
        let uid = unsafe { libc::getuid() };
        let mut paths = Vec::new();
        for i in 0..5 {
            let p = env.tmp.join(format!("nplay-cava-{i}.conf"));
            write(&p, "", 0o600);
            paths.push(p);
        }
        write(&env.tmp.join(format!("nplay-mpv-{uid}-1.sock")), "", 0o600);
        write(&env.tmp.join("unrelated.conf"), "", 0o600);
        let c = by_label(env).remove("cava config").unwrap();
        assert!(!c.ok);
        assert!(c.detail.contains("6 old file(s) in "));
        assert!(c.fix.starts_with("rm -f "));
        assert!(c.fix.ends_with(" …"));
        assert_eq!(c.fix.matches(&*env.tmp.to_string_lossy()).count(), 4);
        for p in &paths {
            std::fs::remove_file(p).unwrap();
        }
        std::fs::remove_file(env.tmp.join(format!("nplay-mpv-{uid}-1.sock"))).unwrap();
        let only = env.tmp.join("nplay-cava-x.conf");
        write(&only, "", 0o600);
        let c = check_runtime(env);
        assert!(c.fix.contains(&*only.to_string_lossy()));
    }
}
