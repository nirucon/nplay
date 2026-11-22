//! librespot receiver lifecycle against tools/fake-librespot (port of
//! tests/test_librespot_lifecycle.py EngineTests plus the in-process parts
//! of the terminal tests: worker-thread start, adoption, orphan
//! replacement, and SIGKILL of the owner via a re-executed helper).

use super::*;
use serde_json::Value;
use std::sync::atomic::AtomicUsize;

const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/fake-librespot");

fn wait_for(mut cond: impl FnMut() -> bool, secs: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

struct Sandbox {
    root: PathBuf,
    bin: PathBuf,
    record: PathBuf,
    cache: PathBuf,
    system_cache: PathBuf,
    procs: Vec<i32>,
}

impl Sandbox {
    fn new() -> Sandbox {
        let root = super::super::tests::temp_root("ls");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(FAKE, bin.join("librespot")).unwrap();
        let sl = root.join("data").join("spotify-local");
        Sandbox {
            record: root.join("librespot-starts.jsonl"),
            cache: sl.join("cache"),
            system_cache: sl.join("system-cache"),
            root,
            bin,
            procs: Vec::new(),
        }
    }

    fn options(&self, extra: &[(&str, &str)]) -> EngineOptions {
        let mut env = vec![(
            "FAKE_LIBRESPOT_RECORD".to_string(),
            self.record.to_string_lossy().into_owned(),
        )];
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        EngineOptions {
            settings: Arc::new(|_, d| d.to_string()),
            data_dir: self.root.join("data"),
            state_dir: self.root.join("state"),
            runtime_dir: Some(self.root.join("run")),
            binary: Some(self.bin.join("librespot")),
            env,
        }
    }

    fn engine(&self) -> LocalSpotifyEngine {
        LocalSpotifyEngine::with_options(self.options(&[]))
    }

    fn authorize(&self) {
        std::fs::create_dir_all(&self.system_cache).unwrap();
        std::fs::write(
            self.system_cache.join("credentials.json"),
            r#"{"username":"fake"}"#,
        )
        .unwrap();
    }

    fn receivers(&self) -> Vec<i32> {
        find_receivers(&self.cache, &[])
    }

    fn starts(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.record)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A python helper that Popens `args` and prints the child pid. With
    /// `owner` the helper stays alive as the receiver's parent and its
    /// argv contains "nplay" (another live NPLAY); without, it exits and the
    /// receiver is reparented (an orphan).
    fn helper(&mut self, owner: bool, oauth: bool) -> i32 {
        let mut args = vec![
            self.bin.join("librespot").to_string_lossy().into_owned(),
            "--name".into(),
            "NPLAY".into(),
            "--cache".into(),
            self.cache.to_string_lossy().into_owned(),
            "--system-cache".into(),
            self.system_cache.to_string_lossy().into_owned(),
            "--quiet".into(),
        ];
        if oauth {
            args.push("--enable-oauth".into());
        }
        let code = "import subprocess,sys,time\np=subprocess.Popen(sys.argv[2:],start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\nprint(p.pid,flush=True)\nif sys.argv[1]=='owner':\n    time.sleep(120)\n";
        let script = self.root.join(if owner {
            "nplay-owner.py"
        } else {
            "orphan-maker.py"
        });
        std::fs::write(&script, code).unwrap();
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(if owner { "owner" } else { "orphan" })
            .args(&args)
            .env("FAKE_LIBRESPOT_RECORD", &self.record)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        {
            use std::io::BufRead;
            let out = child.stdout.take().unwrap();
            std::io::BufReader::new(out).read_line(&mut line).unwrap();
        }
        let pid: i32 = line.trim().parse().unwrap();
        self.procs.push(pid);
        if owner {
            self.procs.push(child.id() as i32);
            std::mem::forget(child);
        } else {
            let _ = child.wait();
        }
        assert!(wait_for(|| self.receivers().contains(&pid), 8.0));
        assert!(wait_for(
            || self
                .starts()
                .iter()
                .any(|s| s["pid"].as_i64() == Some(pid as i64)),
            8.0
        ));
        pid
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for pid in self
            .receivers()
            .into_iter()
            .chain(self.procs.iter().copied())
        {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn gone(sb: &Sandbox, pid: Option<i32>) -> bool {
    wait_for(
        || sb.receivers().is_empty() && pid.map(|p| !alive(p)).unwrap_or(true),
        8.0,
    )
}

#[test]
fn no_credentials_fails_immediately_without_spawning() {
    let sb = Sandbox::new();
    let e = sb.engine();
    let t = Instant::now();
    let err = e.start(false).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(super::super::is_not_authorized(&err));
    assert!(err.message.contains("not authorized yet") && err.message.contains("AUTHORIZE"));
    assert_eq!(err.message, super::super::NOT_AUTHORIZED_MESSAGE);
    assert!(sb.starts().is_empty());
    assert!(!e.authorized());
    e.close();
}

#[test]
fn no_credentials_does_not_burn_the_restart() {
    let sb = Sandbox::new();
    let e = sb.engine();
    struct Ops<'a>(&'a LocalSpotifyEngine, AtomicUsize, AtomicUsize);
    impl super::super::RecoveryOps for Ops<'_> {
        fn start(&self) -> Result<()> {
            self.0.start(false).map(|_| ())
        }
        fn wait(
            &self,
            _: &str,
            _: f64,
        ) -> std::result::Result<crate::model::Meta, super::super::WaitError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(Default::default())
        }
        fn devices(&self) -> Result<Vec<crate::model::Meta>> {
            Ok(vec![])
        }
        fn restart(&self) -> Result<()> {
            self.2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn now(&self) -> f64 {
            1e9
        }
    }
    let ops = Ops(&e, AtomicUsize::new(0), AtomicUsize::new(0));
    let last = Mutex::new(f64::NEG_INFINITY);
    let err = super::super::ensure_with(&ops, &last, "NPLAY", 20.0).unwrap_err();
    assert!(super::super::is_not_authorized(&err));
    assert_eq!(ops.1.load(Ordering::SeqCst), 0, "no registration wait");
    assert_eq!(ops.2.load(Ordering::SeqCst), 0, "restart not spent");
    e.close();
}

#[test]
fn orphan_with_credentials_missing_is_still_cleaned() {
    let mut sb = Sandbox::new();
    let orphan = sb.helper(false, false);
    let e = sb.engine();
    assert!(super::super::is_not_authorized(
        &e.start(false).unwrap_err()
    ));
    assert!(!alive(orphan));
    assert!(sb.receivers().is_empty());
    e.close();
}

#[test]
fn orphan_is_replaced_and_listed_as_stale() {
    let mut sb = Sandbox::new();
    sb.authorize();
    let orphan = sb.helper(false, false);
    let e = sb.engine();
    assert_eq!(e.stale_receivers(), vec![orphan]);
    assert!(e.start(false).unwrap());
    let pid = e.pid().unwrap();
    assert_ne!(pid, orphan);
    assert!(!alive(orphan));
    assert_eq!(sb.receivers(), vec![pid]);
    assert!(e.stale_receivers().is_empty());
    e.close();
    assert!(gone(&sb, Some(pid)));
}

#[test]
fn receiver_owned_by_other_nplay_is_adopted_never_doubled() {
    let mut sb = Sandbox::new();
    sb.authorize();
    let theirs = sb.helper(true, false);
    let e = sb.engine();
    assert!(e.start(false).unwrap());
    assert_eq!(e.pid(), Some(theirs));
    assert!(e.running() && !e.owned());
    assert_eq!(sb.receivers(), vec![theirs]);
    assert_eq!(sb.starts().len(), 1);
    // Starting again is a no-op while the adopted receiver lives.
    assert!(e.start(false).unwrap());
    assert_eq!(sb.starts().len(), 1);
    // Closing the adopter leaves the owner's receiver alone.
    e.close();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sb.receivers(), vec![theirs]);
}

#[test]
fn receiver_started_from_worker_thread_survives_it() {
    let sb = Sandbox::new();
    sb.authorize();
    let e = sb.engine();
    let e2 = e.clone();
    std::thread::spawn(move || e2.start(false).unwrap())
        .join()
        .unwrap();
    let pid = e.pid().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        alive(pid),
        "receiver died with its short-lived spawning thread"
    );
    assert_eq!(sb.receivers(), vec![pid]);
    e.close();
    assert!(gone(&sb, Some(pid)));
}

#[test]
fn restart_after_close_is_refused() {
    let sb = Sandbox::new();
    sb.authorize();
    let e = sb.engine();
    assert!(e.start(false).unwrap());
    let pid = e.pid().unwrap();
    e.close();
    assert!(!alive(pid));
    let err = e.start(false).unwrap_err();
    assert!(err.message.contains("shutting down"));
    assert!(e.restart().is_err());
    assert!(sb.receivers().is_empty());
}

#[test]
fn pidfile_records_owner_and_is_removed() {
    let sb = Sandbox::new();
    sb.authorize();
    let e = sb.engine();
    e.start(false).unwrap();
    let d: Value = serde_json::from_str(&std::fs::read_to_string(e.pid_path()).unwrap()).unwrap();
    assert_eq!(d["pid"].as_i64().unwrap() as i32, e.pid().unwrap());
    assert_eq!(d["owner"].as_i64().unwrap() as u32, std::process::id());
    assert_eq!(d["cache"], json!(sb.cache.to_string_lossy()));
    let mode = std::fs::metadata(e.pid_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let lmode = std::fs::metadata(e.lock_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(lmode, 0o600);
    // 1.x naming: sha1(realpath(cache))[:12].
    let key = sha1::sha1_hex(realpath(&sb.cache).to_string_lossy().as_bytes())[..12].to_string();
    assert_eq!(
        e.pid_path().file_name().unwrap().to_string_lossy(),
        format!("librespot-{key}.json")
    );
    assert_eq!(
        e.lock_path().file_name().unwrap().to_string_lossy(),
        format!("librespot-{key}.lock")
    );
    e.stop(false);
    assert!(!e.pid_path().exists());
    e.close();
}

#[test]
fn spawn_arguments_and_settings() {
    let sb = Sandbox::new();
    sb.authorize();
    let e = sb.engine();
    e.start(false).unwrap();
    let argv: Vec<String> = serde_json::from_value(sb.starts()[0]["argv"].clone()).unwrap();
    let cache = sb.cache.to_string_lossy().into_owned();
    let sys = sb.system_cache.to_string_lossy().into_owned();
    assert_eq!(
        argv,
        [
            "--name",
            "NPLAY",
            "--device-type",
            "computer",
            "--bitrate",
            "320",
            "--cache",
            &cache,
            "--system-cache",
            &sys,
            "--backend",
            "pulseaudio",
            "--initial-volume",
            "70",
            "--quiet"
        ]
    );
    assert_eq!(
        sb.starts()[0]["ppid"].as_i64().unwrap() as u32,
        std::process::id()
    );
    e.close();

    let mut opts = sb.options(&[]);
    opts.settings = Arc::new(|k, d| match k {
        "spotify_local_name" => "Den".into(),
        "spotify_local_volume" => "55.7".into(),
        "spotify_local_bitrate" => "".into(),
        "spotify_local_backend" => "alsa".into(),
        _ => d.into(),
    });
    let e = LocalSpotifyEngine::with_options(opts);
    e.start(false).unwrap();
    let argv: Vec<String> = serde_json::from_value(sb.starts()[1]["argv"].clone()).unwrap();
    assert_eq!(
        &argv[..6],
        [
            "--name",
            "Den",
            "--device-type",
            "computer",
            "--bitrate",
            "320"
        ]
    );
    assert_eq!(
        &argv[10..14],
        ["--backend", "alsa", "--initial-volume", "55"]
    );
    assert!(!argv.contains(&"--enable-oauth".to_string()));
    assert!(e.version().contains("NPLAY test fake"));
    assert!(e
        .credential_files()
        .iter()
        .any(|p| p.ends_with("credentials.json")));
    e.close();
}

#[test]
fn find_receivers_verifies_cmdline_cache() {
    let sb = Sandbox::new();
    sb.authorize();
    let other = sb.root.join("other-cache");
    let mut p = Command::new(sb.bin.join("librespot"))
        .arg("--cache")
        .arg(&other)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pp = p.id() as i32;
    assert!(wait_for(|| find_receivers(&other, &[]) == vec![pp], 8.0));
    let e = sb.engine();
    e.start(false).unwrap();
    assert_eq!(find_receivers(e.cache(), &[]), vec![e.pid().unwrap()]);
    assert_eq!(find_receivers(&other, &[]), vec![pp]);
    assert!(
        p.try_wait().unwrap().is_none(),
        "a receiver for another cache must not be touched"
    );
    let _ = p.kill();
    let _ = p.wait();
    e.close();
}

#[test]
fn authorize_opens_printed_url_and_caches_credentials() {
    let sb = Sandbox::new();
    let e = sb.engine();
    let opened = Mutex::new(Vec::<String>::new());
    let told = Mutex::new(Vec::<String>::new());
    let opener = |u: &str| {
        opened.lock().unwrap().push(u.to_string());
        true
    };
    let on_url = |u: &str| told.lock().unwrap().push(u.to_string());
    assert!(e
        .authorize(Some(10.0), Some(&opener), Some(&on_url))
        .unwrap());
    let want = "https://accounts.spotify.com/authorize?client_id=fake&state=test";
    assert_eq!(*opened.lock().unwrap(), vec![want.to_string()]);
    assert_eq!(*told.lock().unwrap(), vec![want.to_string()]);
    let argv: Vec<String> =
        serde_json::from_value(sb.starts().last().unwrap()["argv"].clone()).unwrap();
    assert!(argv.contains(&"--enable-oauth".to_string()));
    assert!(e.authorized());
    assert!(e.running());
    assert!(!sb
        .system_cache
        .join("credentials.json.nplay-previous")
        .exists());
    e.close();
    assert!(gone(&sb, None));
}

#[test]
fn authorize_replaces_orphan_and_existing_receiver() {
    let mut sb = Sandbox::new();
    sb.authorize();
    let orphan = sb.helper(false, false);
    let e = sb.engine();
    assert!(e.authorize(Some(10.0), Some(&|_| true), None).unwrap());
    assert!(!alive(orphan));
    assert_eq!(sb.receivers().len(), 1);
    let first = e.pid().unwrap();
    // A second authorization replaces the receiver again.
    assert!(e.authorize(Some(10.0), Some(&|_| true), None).unwrap());
    assert_ne!(e.pid().unwrap(), first);
    assert_eq!(sb.receivers().len(), 1);
    e.close();
}

#[test]
fn failed_reauthorize_restores_credentials() {
    let sb = Sandbox::new();
    sb.authorize();
    let before = std::fs::read_to_string(sb.system_cache.join("credentials.json")).unwrap();
    let e = LocalSpotifyEngine::with_options(sb.options(&[("FAKE_LIBRESPOT_AUTH", "deny")]));
    let err = e.authorize(Some(1.5), Some(&|_| true), None).unwrap_err();
    assert!(err.message.contains("timed out after 1s"), "{err}");
    assert_eq!(
        std::fs::read_to_string(sb.system_cache.join("credentials.json")).unwrap(),
        before
    );
    assert!(!e.running());
    assert!(sb.receivers().is_empty());
    e.close();
}

#[test]
fn oauth_receiver_cannot_open_its_own_browser() {
    let sb = Sandbox::new();
    let d = no_open_dir(&sb.root.join("run")).unwrap();
    assert_eq!(
        std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for tool in ["xdg-open", "gio"] {
        // ETXTBSY: another test thread may fork while the script is open.
        let mut st = None;
        for _ in 0..50 {
            match Command::new(d.join(tool))
                .arg("https://example.test")
                .status()
            {
                Ok(s) => {
                    st = Some(s);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(st.unwrap().success());
    }
    // The receiver's PATH starts with it and $BROWSER is removed.
    let e = LocalSpotifyEngine::with_options(sb.options(&[("FAKE_LIBRESPOT_AUTH", "deny")]));
    e.start(true).unwrap();
    let pid = e.pid().unwrap();
    let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    let env: Vec<String> = env
        .split(|b| *b == 0)
        .map(|x| String::from_utf8_lossy(x).into_owned())
        .collect();
    let path = env.iter().find(|v| v.starts_with("PATH=")).unwrap();
    assert!(path.starts_with(&format!("PATH={}", d.display())), "{path}");
    assert!(!env.iter().any(|v| v.starts_with("BROWSER=")));
    assert!(
        env.iter().any(|v| v == "PULSE_PROP_media.role=music")
            || std::env::var_os("PULSE_PROP_media.role").is_some()
    );
    e.close();
}

#[test]
fn oauth_url_is_read_after_offset() {
    let sb = Sandbox::new();
    let log = sb.root.join("l.log");
    std::fs::write(&log, "Browse to: https://old.example/x\nnoise\n").unwrap();
    let off = std::fs::metadata(&log).unwrap().len();
    assert_eq!(oauth_url_from_log(&log, 0), "https://old.example/x");
    assert_eq!(oauth_url_from_log(&log, off), "");
    std::fs::write(&log, "Browse to: https://old.example/x\nnoise\nINFO Browse to: http://insecure\n[x] Browse to:  https://accounts.spotify.com/a?b=c \n").unwrap();
    assert_eq!(
        oauth_url_from_log(&log, off),
        "https://accounts.spotify.com/a?b=c"
    );
    assert_eq!(oauth_url_from_log(&sb.root.join("missing"), 0), "");
}

#[test]
fn proc_helpers() {
    assert!(alive(std::process::id() as i32));
    assert!(!alive(0) && !alive(-5));
    assert!(is_nplay_process(std::process::id() as i32));
    assert!(!is_nplay_process(1));
    let a = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(is_librespot_argv(&a(&[
        "/usr/bin/python3",
        "/x/bin/librespot",
        "--cache",
        "c"
    ])));
    assert!(is_librespot_argv(&a(&["librespot-0.8"])));
    assert!(!is_librespot_argv(&a(&["a", "b", "c", "librespot"])));
    assert_eq!(argv_cache(&a(&["l", "-c", "/p"])), Some("/p".into()));
    assert_eq!(argv_cache(&a(&["l", "--cache=/q"])), Some("/q".into()));
    assert_eq!(argv_cache(&a(&["l", "--cache"])), None);
    assert!(terminate_pid(999_999_999, 0.1));
}

/// Helper body for `sigkill_of_owner_kills_receiver` (run in a child).
#[test]
#[ignore]
fn owner_process_helper() {
    let Some(root) = std::env::var_os("NPLAY_SPOTIFY_TEST_OWNER") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut sb = Sandbox::new();
    let _ = std::fs::remove_dir_all(&sb.root);
    sb.root = root.clone();
    sb.bin = root.join("bin");
    sb.record = root.join("librespot-starts.jsonl");
    let e = sb.engine();
    e.start(false).unwrap();
    std::fs::write(root.join("ready"), format!("{}", e.pid().unwrap())).unwrap();
    std::thread::sleep(Duration::from_secs(60));
    std::mem::forget(sb);
}

#[test]
fn sigkill_of_owner_kills_receiver() {
    let sb = Sandbox::new();
    sb.authorize();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "spotify::local::tests::owner_process_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("NPLAY_SPOTIFY_TEST_OWNER", &sb.root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ready = sb.root.join("ready");
    assert!(wait_for(
        || ready.exists()
            && !std::fs::read_to_string(&ready)
                .unwrap_or_default()
                .is_empty(),
        20.0
    ));
    let pid: i32 = std::fs::read_to_string(&ready)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid));
    assert_eq!(sb.receivers(), vec![pid]);
    // The owning test binary is an NPLAY process for adoption purposes.
    let e = sb.engine();
    assert!(e.start(false).unwrap());
    assert_eq!(e.pid(), Some(pid));
    assert!(!e.owned());
    e.close();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();
    assert!(gone(&sb, Some(pid)), "PR_SET_PDEATHSIG did not fire");
}
