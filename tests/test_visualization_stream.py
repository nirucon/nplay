"""P2/P3/P5: pushed visualization frames, silence suppression and CAVA lifetime."""
import base64
import os
import socket
import sys
import tempfile
import time
import unittest
from pathlib import Path

from test_service import ServiceProcess, Wire

HERE = Path(__file__).resolve().parent


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    try:
        with open(f"/proc/{pid}/stat") as f:
            return f.read().split()[2] != "Z"
    except OSError:
        return False


class VisualizationStreamTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        cava = bin_dir / "cava"
        cava.write_text(f"#!/bin/sh\nexec {sys.executable} {HERE / 'fake_cava.py'} \"$@\"\n")
        cava.chmod(0o755)
        self.cava_log = root / "cava.log"
        env = {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "FAKE_CAVA_LOG": str(self.cava_log),
            "FAKE_CAVA_MUSIC_FRAMES": "15",
        }
        self.svc = ServiceProcess(self.tmp.name, extra_args=("--idle-exit", "60"), extra_env=env).start()

    def tearDown(self):
        self.svc.stop()
        self.tmp.cleanup()

    def cava_starts(self):
        if not self.cava_log.exists():
            return []
        return [line.split(" ", 1) for line in self.cava_log.read_text().splitlines()]

    def wait_cava_stopped(self, pid, timeout=4):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not pid_alive(pid):
                return True
            time.sleep(0.05)
        return False

    def frames(self, w, seconds):
        """Collect pushed frames for `seconds`; JSON-RPC messages go to w.notes."""
        out = []
        deadline = time.monotonic() + seconds
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                return out
            w.sock.settimeout(left)
            try:
                msg = w.read()
            except socket.timeout:
                return out
            if "vis" in msg:
                out.append(msg)
            else:
                w.notes.append(msg)

    def test_capabilities_advertise_stream(self):
        with Wire(self.svc.sock_path) as w:
            snap = w.request(1, "initialize", {"protocol_versions": [1]})["result"]
            caps = snap["capabilities"]
            self.assertTrue(caps["visualization_stream"])
            self.assertEqual(caps["protocol_version"], 1)
            self.assertGreaterEqual(caps["protocol_minor"], 1)
            self.assertIn("json", caps["visualization_encodings"])
            self.assertIn("b64", caps["visualization_encodings"])
            self.assertGreaterEqual(snap["protocol_minor"], 1)
            info = w.request(2, "service.info")["result"]
            self.assertGreaterEqual(info["protocol_minor"], 1)

    def test_subscribe_push_silence_and_unsubscribe(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "visualization.subscribe", {"bars": 16, "fps": 30})
            self.assertTrue(r["result"]["subscribed"])
            self.assertEqual(r["result"]["bars"], 16)
            got = self.frames(w, 1.5)
            # ~15 distinct music frames, then exactly one silent frame, then nothing.
            self.assertGreaterEqual(len(got), 8, got[:3])
            self.assertLessEqual(len(got), 17)
            self.assertTrue(all(isinstance(f["vis"], list) and len(f["vis"]) == 16 for f in got))
            self.assertEqual(got[-1]["vis"], [0] * 16)
            self.assertEqual(sum(1 for f in got if f["vis"] == [0] * 16), 1)
            self.assertEqual(self.frames(w, 0.6), [], "silence must not be pushed")
            st = w.request(3, "visualization.status")["result"]
            self.assertTrue(st["running"])
            self.assertEqual(st["subscribers"], 1)
            self.assertGreater(st["suppressed"], 0)
            pid = int(self.cava_starts()[-1][0])
            r = w.request(4, "visualization.unsubscribe")
            self.assertFalse(r["result"]["subscribed"])
            self.assertTrue(self.wait_cava_stopped(pid), "CAVA must stop with 0 subscribers")
            st = w.request(5, "visualization.status")["result"]
            self.assertFalse(st["running"])
            self.assertEqual(st["subscribers"], 0)
            self.assertEqual(self.frames(w, 0.3), [])

    def test_b64_encoding_and_fps_limit(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "visualization.subscribe", {"bars": 24, "fps": 10, "encoding": "b64"})
            self.assertEqual(r["result"]["encoding"], "b64")
            got = self.frames(w, 0.45)
            self.assertTrue(got)
            for f in got:
                self.assertEqual(f["enc"], "b64")
                self.assertEqual(len(base64.b64decode(f["vis"])), 24)
            # 10 fps for 0.45 s of music: ~5 frames, never CAVA's full 30 fps.
            self.assertLessEqual(len(got), 7)
            bad = w.request(3, "visualization.subscribe", {"encoding": "xml"})
            self.assertEqual(bad["error"]["code"], -32602)

    def test_disconnect_ends_stream_and_stops_cava(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            w.request(2, "visualization.subscribe", {"bars": 16})
            self.assertTrue(self.frames(w, 0.3))
            pid = int(self.cava_starts()[-1][0])
            self.assertTrue(pid_alive(pid))
        self.assertTrue(self.wait_cava_stopped(pid), "CAVA must stop after the subscriber disconnects")
        with Wire(self.svc.sock_path) as w:
            st = w.request(1, "visualization.status")["result"]
            self.assertEqual(st["subscribers"], 0)
            self.assertFalse(st["running"])

    def test_legacy_data_request_still_works(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            r = w.request(2, "visualization.data", {"bars": 20})
            self.assertEqual(len(r["result"]["bars"]), 20)
            time.sleep(0.2)
            r = w.request(3, "visualization.data", {"bars": 20})
            self.assertEqual(len(r["result"]["bars"]), 20)
            self.assertTrue(any(r["result"]["bars"]))
            # A poller never receives pushed frames.
            self.assertEqual(self.frames(w, 0.3), [])
            # A subscriber leaving does not cut CAVA for a recent poller.
            w.request(4, "visualization.subscribe", {"bars": 8})
            w.request(5, "visualization.unsubscribe")
            self.frames(w, 0.1)
            st = w.request(6, "visualization.status")["result"]
            self.assertTrue(st["running"])
            self.assertTrue(st["polled_recently"])

    def test_cava_config_is_private_and_removed(self):
        with Wire(self.svc.sock_path) as w:
            w.request(1, "initialize", {"protocol_versions": [1]})
            w.request(2, "visualization.subscribe", {"bars": 16})
            self.frames(w, 0.3)
            pid_s, cfg = self.cava_starts()[-1]
            cfg = Path(cfg)
            self.assertFalse(str(cfg).startswith(tempfile.gettempdir() + "/nplay-cava-"))
            self.assertEqual(cfg.parent, Path(self.tmp.name) / "run" / "nplay")
            self.assertEqual(cfg.parent.stat().st_mode & 0o777, 0o700)
            self.assertEqual(cfg.stat().st_mode & 0o777, 0o600)
            w.request(3, "visualization.unsubscribe")
            self.assertTrue(self.wait_cava_stopped(int(pid_s)))
            deadline = time.monotonic() + 2
            while cfg.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertFalse(cfg.exists(), "CAVA config must be removed when CAVA stops")


if __name__ == "__main__":
    unittest.main()
