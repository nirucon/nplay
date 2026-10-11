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


class ReadinessTests(unittest.TestCase):
    def test_paused_loaded_track_is_ready(self):
        player = Player()
        expected = "/tmp/music.mp3"
        process = FakeProcess()
        # Avoid launching mpv; exercise the real readiness loop and IPC status.
        def fake_stop():
            player.generation += 1
            player.ready.clear()
            player.failed.clear()
        player._stop_locked = fake_stop
        player._playback_status = lambda sock: (expected, False, True)
        with patch("nplay.player.subprocess.Popen", return_value=process), \
             patch("nplay.player.os.path.exists", return_value=True), \
             patch.object(player, "_events", return_value=None):
            self.assertTrue(player.start(expected, timeout=2))

    def test_mismatched_path_is_not_accepted(self):
        player = Player()
        player._playback_status = lambda sock: ("/tmp/other.mp3", False, True)
        self.assertNotEqual(player._playback_status("unused")[0], "/tmp/music.mp3")


if __name__ == "__main__":
    unittest.main()
