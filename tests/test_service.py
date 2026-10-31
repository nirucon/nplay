"""End-to-end tests for the headless NPLAY control service.

These tests spawn the real service in an isolated XDG environment. They never
start mpv or contact a provider, so they are safe offline and in CI.
"""
import json
import os
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class ServiceProcess:
    def __init__(self, root, extra_args=(), extra_env=None):
        self.root = Path(root)
        self.sock_path = self.root / "run" / "nplay" / "control.sock"
        env = {k: v for k, v in os.environ.items() if not k.startswith(("XDG_", "NPLAY_"))}
        env.update(extra_env or {})
        env.update({
            "HOME": str(self.root / "home"),
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "XDG_STATE_HOME": str(self.root / "state"),
            "XDG_RUNTIME_DIR": str(self.root / "run"),
            "PYTHONPATH": str(ROOT),
            "PYTHONUNBUFFERED": "1",
        })
        Path(env["HOME"]).mkdir(parents=True, exist_ok=True)
        (self.root / "run").mkdir(parents=True, exist_ok=True)
        self.env = env
        self.args = [sys.executable, "-m", "nplay.app", "--serve", "--service-socket", str(self.sock_path), *extra_args]
        self.proc = None

    def start(self, timeout=15):
        self.proc = subprocess.Popen(
            self.args, cwd=str(ROOT), env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.sock_path.exists():
                try:
                    s = socket.socket(socket.AF_UNIX)
                    s.settimeout(0.2)
                    s.connect(str(self.sock_path))
                    s.close()
                    return self
                except OSError:
                    pass
            if self.proc.poll() is not None:
                out, err = self.proc.communicate(timeout=2)
                raise AssertionError(f"service exited early ({self.proc.returncode})\n{out}\n{err}")
            time.sleep(0.05)
        raise AssertionError("service did not create its socket")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            try:
                self.proc.terminate()
                self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=3)
        if self.proc:
            try:
                self.proc.communicate(timeout=1)
            except Exception:
                pass


class Wire:
    def __init__(self, path):
        self.path = path

    def __enter__(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10)
        self.sock.connect(str(self.path))
        self.buf = b""
        self.notes = []
        return self

    def __exit__(self, *exc):
        try:
            self.sock.close()
        except OSError:
            pass

    def send_raw(self, data):
        self.sock.sendall(data)

    def read(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("service closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def request(self, req_id, method, params=None):
        self.sock.sendall((json.dumps({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}) + "\n").encode())
        while True:
            msg = self.read()
            if msg.get("id") == req_id:
                return msg
            self.notes.append(msg)

    def wait_note(self, method, timeout=5):
        deadline = time.monotonic() + timeout
        while True:
            for i, msg in enumerate(self.notes):
                if msg.get("method") == method:
                    return self.notes.pop(i)
            if time.monotonic() > deadline:
                raise AssertionError(f"notification {method} not received")
            self.sock.settimeout(0.5)
            try:
                msg = self.read()
            except socket.timeout:
                continue
            except EOFError:
                raise AssertionError(f"service closed while waiting for {method}")
            self.notes.append(msg)


class ServiceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.svc = ServiceProcess(self.tmp.name, extra_args=("--idle-exit", "60")).start()

    def tearDown(self):
        self.svc.stop()
        self.tmp.cleanup()

    def test_handshake_snapshot_and_capabilities(self):
        with Wire(self.svc.sock_path) as w:
            msg = w.request(1, "initialize", {"client": {"name": "test", "version": "0.1"}, "protocol_versions": [1]})
            self.assertNotIn("error", msg)
            r = msg["result"]
            self.assertEqual(r["protocol_version"], 1)
            self.assertEqual(r["backend"]["name"], "nplay")
            self.assertEqual(r["playback"]["status"], "idle")
            self.assertIsNone(r["playback"]["current"])
            self.assertFalse(r["capabilities"]["equalizer"])
            self.assertIn("state.changed", r["capabilities"]["messages"])
            sources = {x["id"]: x for x in r["sources"]}
            self.assertTrue(sources["local"]["enabled"])
            self.assertIn("search", sources["local"]["capabilities"])

    def test_incompatible_protocol_is_rejected(self):
        with Wire(self.svc.sock_path) as w:
            msg = w.request(1, "initialize", {"protocol_versions": [999]})
            self.assertEqual(msg["error"]["code"], -32010)

    def test_queue_events_and_playlists(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            w.request(2, "state.subscribe")
            added = w.request(3, "radio.add", {"name": "Test Radio", "url": "http://127.0.0.1:9/live"})
            self.assertTrue(added["result"]["ok"])
            browsed = w.request(4, "browse", {"source": "radio", "section": "all"})
            items = browsed["result"]["items"]
            self.assertEqual(len(items), 1)
            track = items[0]
            self.assertEqual(track["source"], "radio")
            self.assertNotIn("url", track)
            self.assertNotIn("127.0.0.1", json.dumps(track))
            q = w.request(5, "queue.add", {"track": track})
            self.assertEqual(len(q["result"]["items"]), 1)
            note = w.wait_note("state.changed")
            self.assertGreaterEqual(note["params"]["revision"], 1)
            self.assertEqual(len(note["params"]["playback"]["queue"]), 1)
            w.request(6, "state.unsubscribe")
            # Local playlists can hold cross-source tracks without copying audio.
            p = w.request(7, "playlists.create", {"name": "Service list"})
            pid = p["result"]["id"]
            w.request(8, "playlists.add", {"id": pid, "track": track})
            tracks = w.request(9, "playlists.tracks", {"id": pid})
            self.assertEqual([x["key"] for x in tracks["result"]["items"]], [track["key"]])
            saved = w.request(10, "queue.save", {"name": "Saved queue"})
            self.assertEqual(saved["result"]["name"], "Saved queue")
            names = [x["name"] for x in w.request(11, "playlists.list")["result"]["items"]]
            self.assertIn("Saved queue", names)
            w.request(12, "playlists.delete", {"id": pid})

    def test_play_external_queue_and_bulk_queue(self):
        media = Path(self.tmp.name) / "drop.wav"
        with open(media, "wb") as f:
            f.write(b"RIFF\x24\x00\x00\x00WAVEfmt ")
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "playback.play_external", {"path": str(media), "title": "Dropped", "queue": True})
            self.assertTrue(r["result"]["queued"])
            self.assertEqual(r["result"]["track"]["title"], "Dropped")
            q = w.request(3, "queue.list")["result"]
            self.assertEqual([x["title"] for x in q["items"]], ["Dropped"])
            w.request(4, "radio.add", {"name": "Bulk", "url": "http://127.0.0.1:9/bulk"})
            track = w.request(5, "browse", {"source": "radio", "section": "all"})["result"]["items"][0]
            r = w.request(6, "queue.add_many", {"tracks": [track, track, track]})
            self.assertEqual(len(r["result"]["items"]), 4)
            w.request(7, "queue.clear")
            self.assertEqual(w.request(8, "queue.list")["result"]["items"], [])
            art = w.request(9, "artwork.get", {"track": r["result"]["items"][0]})
            self.assertTrue(art["result"]["empty"])

    def test_local_playback_through_real_mpv_boundary_with_fake_ipc(self):
        bin_dir = Path(self.tmp.name) / "bin"
        bin_dir.mkdir(exist_ok=True)
        fake = bin_dir / "mpv"
        try:
            fake.symlink_to(ROOT / "tools" / "fake-mpv")
        except AttributeError:
            pass
        if not fake.exists():
            fake.write_text(f"#!/bin/sh\nexec {sys.executable} {ROOT / 'tools' / 'fake-mpv'} \"$@\"\n")
            fake.chmod(0o755)
        # Restart the service with the fake transport ahead of PATH.
        self.svc.stop()
        env = dict(self.svc.env)
        env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
        self.svc = ServiceProcess(self.tmp.name, extra_args=("--idle-exit", "60"), extra_env={"PATH": env["PATH"]}).start()
        media = Path(self.tmp.name) / "tone.wav"
        with wave.open(str(media), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(8000)
            w.writeframes(b"\x00\x00" * 8000)
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            w.request(2, "state.subscribe")
            r = w.request(3, "playback.play_external", {"path": str(media), "title": "Tone"})
            self.assertFalse(r["result"]["queued"])
            playing = None
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                snap = w.request(20, "state.snapshot")["result"]
                if snap["playback"]["status"] == "playing" and snap["playback"]["current"]:
                    playing = snap
                    break
                time.sleep(0.15)
            self.assertIsNotNone(playing, "local playback never reached playing state")
            self.assertEqual(playing["playback"]["current"]["title"], "Tone")
            self.assertTrue(playing["playback"]["seekable"])
            first = playing["playback"]["position"]
            time.sleep(0.6)
            later = w.request(21, "state.snapshot")["result"]["playback"]["position"]
            self.assertGreater(later, first + 0.2)
            w.request(4, "playback.toggle")
            time.sleep(0.2)
            paused = w.request(22, "state.snapshot")["result"]["playback"]
            self.assertEqual(paused["status"], "paused")
            w.request(5, "playback.seek", {"delta": 10})
            time.sleep(0.2)
            self.assertGreater(w.request(23, "state.snapshot")["result"]["playback"]["position"], later)
            w.request(6, "playback.stop")
            time.sleep(0.3)
            idle = w.request(24, "state.snapshot")["result"]["playback"]
            self.assertEqual(idle["status"], "idle")
            self.assertIsNone(idle["current"])

    def test_queue_reorder_moves_absolute_index(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            for i in range(3):
                w.request(10 + i, "radio.add", {"name": f"Station {i}", "url": f"http://127.0.0.1:9/r{i}"})
            tracks = w.request(3, "browse", {"source": "radio", "section": "all"})["result"]["items"]
            self.assertEqual([x["title"] for x in tracks], ["Station 0", "Station 1", "Station 2"])
            for i, track in enumerate(tracks):
                w.request(4 + i, "queue.add", {"track": track})
            r = w.request(20, "queue.reorder", {"from": 0, "to": 2})
            self.assertEqual([x["title"] for x in r["result"]["items"]], ["Station 1", "Station 2", "Station 0"])
            self.assertEqual(r["result"]["index"], 2)
            r = w.request(21, "queue.reorder", {"from": 2, "to": 0})
            self.assertEqual([x["title"] for x in r["result"]["items"]], ["Station 0", "Station 1", "Station 2"])

    def test_unknown_method_and_bad_params_are_structured(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "does.not.exist")
            self.assertEqual(r["error"]["code"], -32601)
            r = w.request(3, "playback.play", {"track": {"key": "local:missing"}})
            self.assertEqual(r["error"]["code"], -32602)
            r = w.request(4, "settings.set", {"settings": {"secret_token": "x"}})
            self.assertEqual(r["error"]["code"], -32602)

    def test_settings_never_expose_secrets(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "settings.get")["result"]
            text = json.dumps(r).lower()
            self.assertNotIn("password", text)
            self.assertNotIn("refresh_token", text)
            self.assertNotIn("access_token", text)
            self.assertIn("capabilities", json.dumps(w.request(3, "initialize", {"protocol_versions": [1]})["result"]))

    def test_malformed_and_oversized_frames(self):
        with Wire(self.svc.sock_path) as w:
            w.send_raw(b"not json\n")
            msg = w.read()
            self.assertEqual(msg["error"]["code"], -32600)
            w.send_raw(b"[" * 100 + b"\n")
            msg = w.read()
            self.assertEqual(msg["error"]["code"], -32600)
        # A frame larger than the limit closes the connection after an error.
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(10)
        s.connect(str(self.svc.sock_path))
        s.sendall(b"{" + b" " * (1024 * 1024 + 10))
        data = b""
        while b"\n" not in data:
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
        s.close()
        self.assertIn(b"frame too large", data)

    def test_socket_and_directory_permissions(self):
        sock = self.svc.sock_path
        self.assertTrue(sock.exists())
        self.assertEqual(stat.S_IMODE(sock.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(sock.parent.stat().st_mode), 0o700)
        self.assertEqual(sock.stat().st_uid, os.getuid())

    def test_second_service_reuses_existing_endpoint(self):
        second = ServiceProcess(self.tmp.name)
        code = subprocess.run(second.args, cwd=str(ROOT), env=second.env, capture_output=True, text=True, timeout=20).returncode
        self.assertEqual(code, 0)
        with Wire(self.svc.sock_path) as w:
            r = w.request(1, "initialize", {"protocol_versions": [1]})
            self.assertNotIn("error", r)

    def test_shutdown_cleans_up_socket(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "service.shutdown")
            self.assertTrue(r["result"]["ok"])
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.svc.sock_path.exists():
            time.sleep(0.05)
        self.assertFalse(self.svc.sock_path.exists())


class IdleExitTests(unittest.TestCase):
    def test_service_exits_without_clients(self):
        with tempfile.TemporaryDirectory() as d:
            svc = ServiceProcess(d, extra_args=("--idle-exit", "1")).start()
            try:
                svc.proc.wait(timeout=8)
                self.assertEqual(svc.proc.returncode, 0)
            finally:
                svc.stop()


if __name__ == "__main__":
    unittest.main()
