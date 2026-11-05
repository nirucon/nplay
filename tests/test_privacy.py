"""P1/U1/U2: credentials never reach argv, logs or user-facing errors."""
import io
import logging
import os
import stat
import sys
import tempfile
import time
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

from nplay import logging_utils
from nplay.config import STATE
from nplay.logging_utils import logger, redact_file, migrate_logs, LOG_PATH
from nplay.player import Player
from nplay.providers import Navidrome, NavidromeError
from nplay.redact import SECRET_KEYS, contains_secret, redact_text, redact_url

ROOT = Path(__file__).resolve().parents[1]
PASSWORD = "correct horse battery staple"


def secrets_of(url):
    import urllib.parse
    q = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
    return [v[0] for k, v in q.items() if k in SECRET_KEYS and v and v[0]]


class Capture(logging.Handler):
    def __init__(self):
        super().__init__()
        self.lines = []

    def emit(self, record):
        self.lines.append(self.format(record))


class RedactionTable(unittest.TestCase):
    def test_provider_urls_are_redacted(self):
        nav = Navidrome("https://music.example", "niru", PASSWORD)
        cases = {
            "navidrome stream": nav.stream("tr-1"),
            "navidrome cover": nav.cover("al-1"),
            "navidrome bitrate stream": Navidrome("https://m.example/", "u", PASSWORD, 192).stream("x"),
            "subsonic plain password": "https://m.example/rest/ping.view?u=u&p=enc:70617373&v=1.16.1",
            "generic token": "https://api.example/feed?token=abc123&page=2",
            "api key": "https://api.example/x?apiKey=K3Y&q=1",
            "oauth access token": "https://api.example/x?access_token=ya29.secret&alt=json",
        }
        for name, url in cases.items():
            with self.subTest(name):
                values = secrets_of(url)
                self.assertTrue(values, name)
                for out in (redact_url(url), redact_text(f"start source={url} done")):
                    for v in values:
                        self.assertNotIn(v, out)
                    self.assertFalse(contains_secret(out))
                    self.assertIn("REDACTED", out)
                # Non-secret identifiers stay for diagnosis.
                self.assertIn(url.split("?")[0], redact_url(url))

    def test_non_secret_urls_unchanged(self):
        for url in ("https://www.youtube.com/watch?v=abc", "/home/u/Music/a.flac",
                    "https://sverigesradio.se/topsy/direkt/132-hi.mp3", ""):
            self.assertEqual(redact_url(url), url)
            self.assertEqual(redact_text(url), url)

    def test_redaction_is_idempotent(self):
        url = Navidrome("https://m.example", "u", PASSWORD).stream("1")
        once = redact_text(url)
        self.assertEqual(redact_text(once), once)


class NavidromeErrors(unittest.TestCase):
    def test_http_error_shows_status_and_remedy_without_url(self):
        nav = Navidrome("https://music.example", "niru", PASSWORD)

        def boom(url, timeout=8):
            raise urllib.error.HTTPError(url, 401, "Unauthorized", {}, io.BytesIO(b""))
        with patch("nplay.providers._json", side_effect=boom):
            with self.assertRaises(NavidromeError) as cm:
                nav.ping()
        text = str(cm.exception)
        self.assertIn("HTTP 401", text)
        self.assertIn("Settings → Navidrome", text)
        self.assertNotIn("music.example", text)
        self.assertNotIn("t=", text)

    def test_subsonic_auth_failure(self):
        nav = Navidrome("https://music.example", "niru", "wrong")
        reply = {"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "Wrong username or password"}}}
        with patch("nplay.providers._json", return_value=reply):
            with self.assertRaises(NavidromeError) as cm:
                nav.ping()
        self.assertIn("authentication failed", str(cm.exception))
        self.assertIn("Settings → Navidrome", str(cm.exception))

    def test_unreachable(self):
        nav = Navidrome("https://music.example", "niru", PASSWORD)
        with patch("nplay.providers._json", side_effect=urllib.error.URLError("Name or service not known")):
            with self.assertRaises(NavidromeError) as cm:
                nav.ping()
        self.assertIn("unreachable", str(cm.exception))
        self.assertFalse(contains_secret(str(cm.exception)))


class SimulatedNavidromePlay(unittest.TestCase):
    """Plays a Navidrome stream URL through the real Player and the fake mpv."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        bin_dir = Path(self.tmp.name) / "bin"
        bin_dir.mkdir()
        fake = bin_dir / "mpv"
        fake.write_text(f"#!/bin/sh\nexec {sys.executable} {ROOT / 'tools' / 'fake-mpv'} \"$@\"\n")
        fake.chmod(0o755)
        self.path = patch.dict(os.environ, {"PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", "")})
        self.path.start()
        self.capture = Capture()
        self.capture.setFormatter(logging.Formatter("%(message)s"))
        logger().addHandler(self.capture)

    def tearDown(self):
        logger().removeHandler(self.capture)
        self.path.stop()
        self.tmp.cleanup()

    def test_no_secret_in_argv_log_records_or_files(self):
        nav = Navidrome("https://music.example", "niru", PASSWORD)
        url = nav.stream("track-42")
        values = secrets_of(url)
        player = Player()
        try:
            self.assertTrue(player.start(url, timeout=8))
            with open(f"/proc/{player.proc.pid}/cmdline", "rb") as f:
                argv = f.read().decode("utf-8", "replace")
            # Hostile-path logging, as older code did:
            logger().info("mpv start source=%s", url)
            logger().warning("failed %s", RuntimeError("open " + url))
        finally:
            player.stop()
        time.sleep(0.2)
        for v in values:
            self.assertNotIn(v, argv)
        self.assertNotIn("stream.view", argv)
        records = "\n".join(self.capture.lines)
        self.assertIn("REDACTED", records)
        mpv_log = (STATE / "mpv.log").read_text(errors="replace")
        self.assertIn("stream.view", mpv_log, "fake mpv should have logged the loadfile URL")
        files = records + LOG_PATH.read_text(errors="replace") + mpv_log
        for v in values:
            self.assertNotIn(v, files)
        self.assertFalse(contains_secret(files))
        for p in (LOG_PATH, STATE / "mpv.log"):
            self.assertEqual(stat.S_IMODE(os.stat(p).st_mode), 0o600, p)
        self.assertEqual(stat.S_IMODE(os.stat(STATE).st_mode), 0o700)


class LogMigration(unittest.TestCase):
    FIXTURE = (
        "2026-11-01 10:00:00,000 INFO nplay: mpv start source=https://m.example/rest/stream.view?u=niru&t=8f14e45fceea167a5a36dedd4bea2543&s=a1b2c3d4e5f6&v=1.16.1&c=nplay&f=json&id=1\n"
        "2026-11-01 10:00:01,000 INFO nplay: mpv ready request=1 source=navidrome track=1\n"
        "2026-11-01 10:00:02,000 WARNING nplay: mpv readiness timeout expected='https://m.example/rest/stream.view?u=niru&t=c9f0f895fb98ab9159f51fd0297e236d&s=0a1b2c3d4e5f&id=2'\n"
        "bytes \udcff kept\n"
    )

    def test_fixture_log_is_redacted_in_place(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "nplay.log"
            p.write_bytes(self.FIXTURE.encode("utf-8", "surrogateescape"))
            os.chmod(p, 0o664)
            self.assertEqual(redact_file(p), 2)
            out = p.read_bytes().decode("utf-8", "surrogateescape")
            self.assertNotIn("8f14e45fceea167a5a36dedd4bea2543", out)
            self.assertNotIn("a1b2c3d4e5f6", out)
            self.assertNotIn("c9f0f895fb98ab9159f51fd0297e236d", out)
            self.assertIn("mpv ready request=1 source=navidrome track=1", out)
            self.assertIn("u=niru&t=REDACTED&s=REDACTED&v=1.16.1", out)
            self.assertIn("\udcff", out)
            self.assertEqual(len(out.splitlines()), 4)
            self.assertEqual(stat.S_IMODE(os.stat(p).st_mode), 0o600)
            # Idempotent: a second pass changes nothing.
            before = p.read_bytes()
            self.assertEqual(redact_file(p), 0)
            self.assertEqual(p.read_bytes(), before)
            self.assertEqual(sorted(x.name for x in Path(d).iterdir()), ["nplay.log"])

    def test_migration_covers_rotated_and_mpv_logs(self):
        targets = [LOG_PATH.with_name("nplay.log.2"), STATE / "mpv.log"]
        saved = {p: (p.read_bytes() if p.exists() else None) for p in targets}
        try:
            for p in targets:
                p.write_text(self.FIXTURE.splitlines()[0] + "\n")
            files, lines = migrate_logs()
            self.assertGreaterEqual(files, 2)
            self.assertGreaterEqual(lines, 2)
            for p in targets:
                self.assertFalse(contains_secret(p.read_text()))
        finally:
            for p, data in saved.items():
                if data is None:
                    p.unlink(missing_ok=True)
                else:
                    p.write_bytes(data)


if __name__ == "__main__":
    unittest.main()
