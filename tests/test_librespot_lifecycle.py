"""Lifecycle of the NPLAY-managed librespot receiver (1.8.1).

Everything runs against tools/fake-librespot on a private PATH and in a
temporary HOME/XDG tree: no Spotify account, audio or real user data.
Covered:
  * terminal quit (Q), SIGINT, SIGTERM, SIGHUP and SIGKILL of NPLAY leave no
    receiver; a receiver started from a worker thread survives that thread;
  * the service stops its receiver on SIGTERM, SIGHUP, idle exit and SIGKILL;
  * an orphaned receiver (same --cache, no NPLAY parent) is replaced, a
    receiver owned by another live NPLAY is adopted: never two receivers;
  * without cached credentials a start fails at once with a clear error and
    the one automatic restart is not spent;
  * `spotify.local_authorize` / the terminal AUTHORIZE flow start librespot
    with --enable-oauth, open the printed URL with NPLAY's browser opener
    and report the result; a failed reauthorization restores credentials.
"""
import json
import os
import pty
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[1]
FAKE = ROOT / "tools" / "fake-librespot"
FAKE_MPV = ROOT / "tools" / "fake-mpv"

from nplay import spotify as spotify_mod  # noqa: E402


def wait_for(cond, timeout=8.0, step=0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        v = cond()
        if v:
            return v
        time.sleep(step)
    return cond()


class Sandbox:
    """A temporary HOME/XDG tree with fake librespot and mpv on PATH."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="nplay-librespot-")
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "librespot").symlink_to(FAKE)
        (self.bin / "mpv").symlink_to(FAKE_MPV)
        self.record = self.root / "librespot-starts.jsonl"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("XDG_", "NPLAY_", "FAKE_LIBRESPOT"))}
        self.env.update({
            "HOME": str(self.root / "home"),
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "XDG_STATE_HOME": str(self.root / "state"),
            "XDG_RUNTIME_DIR": str(self.root / "run"),
            "PATH": str(self.bin) + os.pathsep + os.environ.get("PATH", ""),
            "PYTHONPATH": str(ROOT),
            "PYTHONUNBUFFERED": "1",
            "FAKE_LIBRESPOT_RECORD": str(self.record),
            "TERM": "xterm",
        })
        for k in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"):
            Path(self.env[k]).mkdir(parents=True, exist_ok=True)
        os.chmod(self.env["XDG_RUNTIME_DIR"], 0o700)
        self.cache = self.root / "data" / "nplay" / "spotify-local" / "cache"
        self.system_cache = self.root / "data" / "nplay" / "spotify-local" / "system-cache"
        self.procs = []

    def authorize(self):
        self.system_cache.mkdir(parents=True, exist_ok=True)
        (self.system_cache / "credentials.json").write_text('{"username":"fake"}')

    def receivers(self):
        return spotify_mod.find_receivers(self.cache)

    def starts(self):
        if not self.record.exists():
            return []
        return [json.loads(x) for x in self.record.read_text().splitlines() if x.strip()]

    def orphan(self, oauth=False):
        """Start a fake receiver that has no NPLAY parent (like 1.8.0 left)."""
        args = [str(self.bin / "librespot"), "--name", "NPLAY", "--cache", str(self.cache),
                "--system-cache", str(self.system_cache), "--quiet"] + (["--enable-oauth"] if oauth else [])
        # Double fork: the receiver is reparented to init / the subreaper.
        code = ("import subprocess,sys;p=subprocess.Popen(sys.argv[1:],start_new_session=True,"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print(p.pid)")
        out = subprocess.run([sys.executable, "-c", code, *args], env=self.env, capture_output=True, text=True, timeout=10)
        pid = int(out.stdout.strip())
        self.procs.append(pid)
        wait_for(lambda: pid in self.receivers())
        return pid

    def terminal(self, mode="start"):
        """Run the real terminal entry point (stub UI) on a pty."""
        ready = self.root / f"ready-{len(self.procs)}-{time.monotonic_ns()}"
        master, slave = pty.openpty()
        def controlling_tty():
            # Like a terminal emulator: the pty is the session's controlling
            # terminal, so closing it delivers SIGHUP to NPLAY.
            import fcntl
            import termios
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        p = subprocess.Popen([sys.executable, str(ROOT / "tests" / "nplay_owner.py"), str(ready), mode],
                             cwd=str(ROOT), env=self.env, stdin=slave, stdout=slave, stderr=slave, close_fds=True,
                             start_new_session=True, preexec_fn=controlling_tty)
        os.close(slave)
        self.procs.append(p.pid)
        # Drain the pty so NPLAY never blocks on output.
        threading.Thread(target=self._drain, args=(master,), daemon=True).start()
        line = wait_for(lambda: ready.exists() and ready.read_text(), timeout=20)
        if not line:
            p.kill()
            raise AssertionError("terminal NPLAY did not become ready")
        parts = line.split()
        info = {"pid": int(parts[1]), "receiver": int(parts[2]), "alive": parts[3] == "1",
                "error": " ".join(parts[4:]), "proc": p, "master": master}
        return info

    @staticmethod
    def _drain(fd):
        # Never block in read(): a blocked read would keep the pty open after
        # a test closes it to simulate closing the terminal window.
        import select
        try:
            while True:
                r, _, _ = select.select([fd], [], [], 0.1)
                if r and not os.read(fd, 4096):
                    return
        except (OSError, ValueError):
            pass

    def cleanup(self):
        for pid in self.receivers():
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        for pid in self.procs:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        self.tmp.cleanup()


def gone(sb, pid=None):
    return wait_for(lambda: not sb.receivers() and (pid is None or not spotify_mod._alive(pid)), timeout=8)


@unittest.skipUnless(sys.platform.startswith("linux"), "PR_SET_PDEATHSIG and /proc are Linux-only")
class TerminalLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.sb = Sandbox()
        self.sb.authorize()

    def tearDown(self):
        self.sb.cleanup()

    def start_terminal(self):
        info = self.sb.terminal()
        self.assertFalse(info["error"], info["error"])
        self.assertTrue(info["receiver"])
        self.assertTrue(info["alive"], "receiver died with its short-lived spawning thread")
        self.assertEqual(self.sb.receivers(), [info["receiver"]])
        return info

    def test_quit_stops_receiver(self):
        info = self.start_terminal()
        os.write(info["master"], b"q\n")
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]))

    def test_sigint_stops_receiver(self):
        info = self.start_terminal()
        os.kill(info["pid"], signal.SIGINT)
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]))

    def test_sigterm_stops_receiver(self):
        info = self.start_terminal()
        os.kill(info["pid"], signal.SIGTERM)
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]))

    def test_sighup_stops_receiver(self):
        info = self.start_terminal()
        os.kill(info["pid"], signal.SIGHUP)
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]))

    def test_closing_the_terminal_stops_receiver(self):
        info = self.start_terminal()
        os.close(info["master"])  # the kernel sends SIGHUP to the session
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]))

    def test_sigkill_of_nplay_kills_receiver(self):
        info = self.start_terminal()
        os.kill(info["pid"], signal.SIGKILL)
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb, info["receiver"]), "PR_SET_PDEATHSIG did not fire")

    def test_orphan_is_replaced_by_terminal(self):
        orphan = self.sb.orphan()
        info = self.start_terminal()
        self.assertNotEqual(info["receiver"], orphan)
        self.assertFalse(spotify_mod._alive(orphan))
        self.assertEqual(self.sb.receivers(), [info["receiver"]])

    def test_second_nplay_adopts_and_never_doubles(self):
        first = self.start_terminal()
        second = self.sb.terminal()
        self.assertFalse(second["error"], second["error"])
        # The second NPLAY uses the first one's receiver: one Connect device.
        self.assertEqual(second["receiver"], first["receiver"])
        self.assertEqual(self.sb.receivers(), [first["receiver"]])
        self.assertEqual(len(self.sb.starts()), 1)
        # Quitting the adopter leaves the owner's receiver alone ...
        os.write(second["master"], b"q\n")
        second["proc"].wait(timeout=15)
        time.sleep(0.3)
        self.assertEqual(self.sb.receivers(), [first["receiver"]])
        # ... and quitting the owner stops it.
        os.write(first["master"], b"q\n")
        first["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb))

    def test_terminal_authorize_spawns_oauth_receiver(self):
        (self.sb.system_cache / "credentials.json").unlink()
        info = self.sb.terminal("authorize")
        self.assertFalse(info["error"], info["error"])
        self.assertIn("--enable-oauth", self.sb.starts()[-1]["argv"])
        self.assertTrue((self.sb.system_cache / "credentials.json").exists())
        os.kill(info["pid"], signal.SIGTERM)
        info["proc"].wait(timeout=15)
        self.assertTrue(gone(self.sb))


class Rpc:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(10)
        self.sock.connect(str(path))
        self.buf = b""
        self.next_id = 0
        self.notes = []

    def _line(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, method, params=None):
        self.next_id += 1
        rid = self.next_id
        self.sock.sendall((json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}) + "\n").encode())
        while True:
            msg = self._line()
            if msg.get("id") == rid:
                return msg
            self.notes.append(msg)

    def wait_note(self, method, pred=lambda p: True, timeout=15):
        deadline = time.monotonic() + timeout
        for m in self.notes:
            if m.get("method") == method and pred(m.get("params") or {}):
                return m["params"]
        self.sock.settimeout(1)
        while time.monotonic() < deadline:
            try:
                msg = self._line()
            except socket.timeout:
                continue
            self.notes.append(msg)
            if msg.get("method") == method and pred(msg.get("params") or {}):
                return msg["params"]
        raise AssertionError(f"no {method} notification")

    def close(self):
        self.sock.close()


@unittest.skipUnless(sys.platform.startswith("linux"), "PR_SET_PDEATHSIG and /proc are Linux-only")
class ServiceLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.sb = Sandbox()

    def tearDown(self):
        self.sb.cleanup()

    def service(self, *extra, env=None):
        sock = self.sb.root / "run" / "nplay" / "control.sock"
        e = dict(self.sb.env)
        e.update(env or {})
        p = subprocess.Popen([sys.executable, "-m", "nplay.app", "--serve", "--service-socket", str(sock), *extra],
                             cwd=str(ROOT), env=e, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.sb.procs.append(p.pid)
        self.assertTrue(wait_for(lambda: sock.exists(), timeout=15), "service did not start")
        rpc = wait_for(lambda: self._connect(sock), timeout=10)
        return p, rpc

    @staticmethod
    def _connect(sock):
        try:
            return Rpc(sock)
        except OSError:
            return None

    def authorize_via_service(self, rpc):
        init = rpc.call("initialize", {"protocol_versions": [1], "client": {"name": "test"}})["result"]
        rpc.call("state.subscribe")
        caps = init["capabilities"]
        self.assertTrue(caps["spotify_local_authorize"])
        self.assertTrue(caps["spotify_local"])
        self.assertFalse(caps["spotify_local_authorized"])
        self.assertGreaterEqual(init["protocol_minor"], 2)
        self.assertIn("spotify.local_auth", caps["messages"])
        self.assertIn("service.error", caps["messages"])
        self.assertFalse(rpc.call("settings.get")["result"]["spotify"]["local_authorized"])
        res = rpc.call("spotify.local_authorize", {"timeout": 20})["result"]
        self.assertTrue(res["started"])
        done = rpc.wait_note("spotify.local_auth", lambda p: p.get("phase") == "done", timeout=20)
        self.assertTrue(done["authorized"], done)
        self.assertIn("--enable-oauth", self.sb.starts()[-1]["argv"])
        status = rpc.call("settings.get")["result"]["spotify"]
        self.assertTrue(status["local_authorized"])
        self.assertTrue(status["local_running"])
        self.assertEqual(len(self.sb.receivers()), 1)
        return self.sb.receivers()[0]

    def test_authorize_then_sigterm(self):
        p, rpc = self.service()
        receiver = self.authorize_via_service(rpc)
        rpc.close()
        p.send_signal(signal.SIGTERM)
        p.wait(timeout=15)
        self.assertTrue(gone(self.sb, receiver))

    def test_sighup_stops_receiver(self):
        p, rpc = self.service()
        receiver = self.authorize_via_service(rpc)
        rpc.close()
        p.send_signal(signal.SIGHUP)
        p.wait(timeout=15)
        self.assertTrue(gone(self.sb, receiver))

    def test_sigkill_kills_receiver(self):
        p, rpc = self.service()
        receiver = self.authorize_via_service(rpc)
        rpc.close()
        p.kill()
        p.wait(timeout=15)
        self.assertTrue(gone(self.sb, receiver))

    def test_idle_exit_stops_receiver(self):
        p, rpc = self.service("--idle-exit", "2")
        receiver = self.authorize_via_service(rpc)
        rpc.close()
        p.wait(timeout=20)
        self.assertTrue(gone(self.sb, receiver))

    def test_authorize_replaces_orphan_and_failure_is_reported(self):
        orphan = self.sb.orphan()
        p, rpc = self.service(env={"FAKE_LIBRESPOT_AUTH": "deny"})
        rpc.call("initialize", {"protocol_versions": [1]})
        rpc.call("state.subscribe")
        self.assertTrue(rpc.call("spotify.local_authorize", {"timeout": 10})["result"]["started"])
        rpc.wait_note("spotify.local_auth", lambda q: q.get("phase") == "browser", timeout=10)
        self.assertFalse(spotify_mod._alive(orphan))
        self.assertEqual(len(self.sb.receivers()), 1)
        done = rpc.wait_note("spotify.local_auth", lambda q: q.get("phase") == "done", timeout=20)
        self.assertFalse(done["authorized"])
        self.assertIn("timed out", done["error"])
        self.assertTrue(gone(self.sb))
        rpc.close()
        p.terminate()
        p.wait(timeout=15)


@unittest.skipUnless(sys.platform.startswith("linux"), "/proc is Linux-only")
class EngineTests(unittest.TestCase):
    """In-process engine tests with patched data/state/runtime directories."""

    def setUp(self):
        self.sb = Sandbox()
        root = self.sb.root
        self.patches = [
            patch.object(spotify_mod, "DATA", root / "data" / "nplay"),
            patch.object(spotify_mod, "STATE", root / "state" / "nplay"),
            patch.dict(os.environ, {"XDG_RUNTIME_DIR": self.sb.env["XDG_RUNTIME_DIR"], "PATH": self.sb.env["PATH"],
                                    "FAKE_LIBRESPOT_RECORD": str(self.sb.record)}),
        ]
        for p in self.patches:
            p.start()
        (root / "state" / "nplay").mkdir(parents=True, exist_ok=True)
        cfg = MagicMock()
        cfg.get.side_effect = lambda k, d="": d
        self.engine = spotify_mod.LocalSpotifyEngine(cfg)

    def tearDown(self):
        try:
            self.engine.close()
        finally:
            for p in reversed(self.patches):
                p.stop()
            self.sb.cleanup()

    def test_no_credentials_fails_immediately_without_spawning(self):
        started = time.monotonic()
        with self.assertRaises(spotify_mod.LocalSpotifyNotAuthorized) as cm:
            self.engine.start(oauth=False)
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertIn("not authorized yet", str(cm.exception))
        self.assertIn("AUTHORIZE", str(cm.exception))
        self.assertEqual(self.sb.starts(), [])
        self.assertFalse(self.engine.authorized())

    def test_no_credentials_does_not_burn_the_restart(self):
        s = spotify_mod.Spotify.__new__(spotify_mod.Spotify)
        s.cfg = MagicMock()
        s.cfg.get.return_value = "NPLAY"
        s.local = self.engine
        s._recovery_lock = threading.Lock()
        s._last_recovery = float("-inf")
        s.wait_for_device = MagicMock()
        s.devices = MagicMock(return_value=[])
        with patch.object(self.engine, "restart") as restart:
            with self.assertRaises(spotify_mod.LocalSpotifyNotAuthorized):
                s.ensure_local_device("NPLAY")
        restart.assert_not_called()
        s.wait_for_device.assert_not_called()

    def test_orphan_with_credentials_missing_is_still_cleaned(self):
        orphan = self.sb.orphan()
        with self.assertRaises(spotify_mod.LocalSpotifyNotAuthorized):
            self.engine.start()
        self.assertFalse(spotify_mod._alive(orphan))
        self.assertEqual(self.sb.receivers(), [])

    def test_restart_after_close_is_refused(self):
        self.sb.authorize()
        self.assertTrue(self.engine.start())
        pid = self.engine.pid()
        self.engine.close()
        self.assertFalse(spotify_mod._alive(pid))
        with self.assertRaisesRegex(RuntimeError, "shutting down"):
            self.engine.start()
        self.assertEqual(self.sb.receivers(), [])

    def test_pidfile_records_owner_and_is_removed(self):
        self.sb.authorize()
        self.engine.start()
        d = json.loads(self.engine.pid_path().read_text())
        self.assertEqual(d["pid"], self.engine.pid())
        self.assertEqual(d["owner"], os.getpid())
        self.assertEqual(oct(self.engine.pid_path().stat().st_mode & 0o777), "0o600")
        self.engine.stop()
        self.assertFalse(self.engine.pid_path().exists())

    def test_find_receivers_verifies_cmdline_cache(self):
        self.sb.authorize()
        other = self.sb.root / "other-cache"
        p = subprocess.Popen([str(self.sb.bin / "librespot"), "--cache", str(other)], env=self.sb.env)
        try:
            self.engine.start()
            self.assertEqual(spotify_mod.find_receivers(self.engine.cache), [self.engine.pid()])
            self.assertEqual(spotify_mod.find_receivers(other), [p.pid])
            self.assertIsNone(p.poll(), "a receiver for another cache must not be touched")
        finally:
            p.kill()
            p.wait()

    def test_authorize_opens_printed_url_and_caches_credentials(self):
        opened = []
        self.assertTrue(self.engine.authorize(timeout=10, opener=lambda u: opened.append(u) or True))
        self.assertEqual(opened, ["https://accounts.spotify.com/authorize?client_id=fake&state=test"])
        argv = self.sb.starts()[-1]["argv"]
        self.assertIn("--enable-oauth", argv)
        self.assertTrue(self.engine.authorized())
        self.assertTrue(self.engine.running())

    def test_failed_reauthorize_restores_credentials(self):
        self.sb.authorize()
        before = (self.sb.system_cache / "credentials.json").read_text()
        with patch.dict(os.environ, {"FAKE_LIBRESPOT_AUTH": "deny"}):
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                self.engine.authorize(timeout=1.5, opener=lambda u: True)
        self.assertEqual((self.sb.system_cache / "credentials.json").read_text(), before)
        self.assertFalse(self.engine.running())
        self.assertEqual(self.sb.receivers(), [])

    def test_oauth_receiver_cannot_open_its_own_browser(self):
        d = spotify_mod._no_open_dir()
        for tool in ("xdg-open", "gio"):
            r = subprocess.run([str(d / tool), "https://example.test"], capture_output=True)
            self.assertEqual(r.returncode, 0)

    def test_service_error_carries_not_authorized_code(self):
        from nplay.service.backend import ServiceUI
        backend = MagicMock()
        ui = ServiceUI.__new__(ServiceUI)
        ui.backend = backend
        ui.error("Spotify playback unavailable", spotify_mod.NOT_AUTHORIZED_MESSAGE)
        method, params = backend.notify.call_args.args
        self.assertEqual(method, "service.error")
        self.assertEqual(params["code"], "spotify_local_unauthorized")
        self.assertEqual(params["remedy"], "spotify.local_authorize")


if __name__ == "__main__":
    unittest.main()
