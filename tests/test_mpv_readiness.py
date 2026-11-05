"""Regression coverage for mpv's paused startup state (no mpv required)."""
import threading
import unittest
from unittest.mock import patch
from nplay.player import Player


class FakeProcess:
    pid = 123
    returncode = None

    def poll(self):
        return None


class FakeSocket:
    def __init__(self, *a, **k):
        pass

    def connect(self, path):
        pass

    def close(self):
        pass

    def shutdown(self, how):
        pass


def start_with_fakes(player, url, popen_calls, ipc_calls):
    process = FakeProcess()

    def fake_popen(args, **kwargs):
        popen_calls.append(list(args))
        return process

    def fake_ipc(sock, args):
        ipc_calls.append(list(args))
        return {"error": "success", "data": None}

    def fake_stop():
        player.generation += 1
        player.ready.clear()
        player.failed.clear()

    player._stop_locked = fake_stop
    with patch("nplay.player.subprocess.Popen", side_effect=fake_popen), \
         patch("nplay.player.os.path.exists", return_value=True), \
         patch("nplay.player.socket.socket", FakeSocket), \
         patch("nplay.player.threading.Thread"), \
         patch.object(player, "_ipc", side_effect=fake_ipc), \
         patch.object(player, "_events", return_value=None):
        return player.start(url, timeout=2)


class ReadinessTests(unittest.TestCase):
    def test_paused_loaded_track_is_ready(self):
        player = Player()
        expected = "/tmp/music.mp3"
        player._playback_status = lambda sock: (expected, False, True)
        self.assertTrue(start_with_fakes(player, expected, [], []))

    def test_mismatched_path_is_not_accepted(self):
        player = Player()
        player._playback_status = lambda sock: ("/tmp/other.mp3", False, True)
        self.assertNotEqual(player._playback_status("unused")[0], "/tmp/music.mp3")

    def test_media_url_is_loaded_over_ipc_not_argv(self):
        player = Player()
        url = "https://music.example/rest/stream.view?u=me&t=0123456789abcdef&s=a1b2c3&v=1.16.1&c=nplay&id=42"
        player._playback_status = lambda sock: (url, False, True)
        popen_calls, ipc_calls = [], []
        self.assertTrue(start_with_fakes(player, url, popen_calls, ipc_calls))
        self.assertEqual(len(popen_calls), 1)
        argv = " ".join(popen_calls[0])
        self.assertNotIn(url, argv)
        self.assertNotIn("0123456789abcdef", argv)
        self.assertNotIn("stream.view", argv)
        self.assertIn("--idle=yes", popen_calls[0])
        self.assertIn(["loadfile", url, "replace"], ipc_calls)

    def test_ipc_socket_is_in_private_directory(self):
        import os
        import stat
        player = Player()
        player._playback_status = lambda sock: ("/tmp/x.mp3", False, True)
        popen_calls = []
        start_with_fakes(player, "/tmp/x.mp3", popen_calls, [])
        sock = next(a.split("=", 1)[1] for a in popen_calls[0] if a.startswith("--input-ipc-server="))
        self.assertFalse(sock.startswith("/tmp/nplay-mpv-"))
        st = os.stat(os.path.dirname(sock))
        self.assertEqual(stat.S_IMODE(st.st_mode), 0o700)
        self.assertEqual(st.st_uid, os.getuid())


if __name__ == "__main__":
    unittest.main()
