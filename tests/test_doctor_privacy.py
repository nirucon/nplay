"""U2: `nplay --doctor` privacy checks, each warning with a one-line fix."""
import os
import tempfile
import unittest
from pathlib import Path

from nplay import privacy
from nplay.config import STATE
from nplay.logging_utils import LOG_PATH


class DoctorPrivacyTests(unittest.TestCase):
    def setUp(self):
        self.saved = {}
        for p in (LOG_PATH, STATE / "mpv.log"):
            self.saved[p] = (p.read_bytes(), os.stat(p).st_mode) if p.exists() else None

    def tearDown(self):
        for p, v in self.saved.items():
            if v is None:
                p.unlink(missing_ok=True)
            else:
                p.write_bytes(v[0])
                os.chmod(p, v[1] & 0o777)

    def by_label(self):
        return {label: (ok, detail, fix) for ok, label, detail, fix in privacy.run_checks()}

    def test_clean_state_passes(self):
        LOG_PATH.write_text("2026-11-05 INFO nplay: mpv start source=https://m/rest/stream.view?u=a&t=REDACTED&s=REDACTED\n")
        os.chmod(LOG_PATH, 0o600)
        (STATE / "mpv.log").unlink(missing_ok=True)
        os.chmod(STATE, 0o700)
        checks = self.by_label()
        for label in ("state dir", "log modes", "log secrets", "argv"):
            self.assertTrue(checks[label][0], (label, checks[label]))

    def test_warnings_have_a_fix_and_no_secret(self):
        token = "5f4dcc3b5aa765d61d8327deb882cf99"
        (STATE / "mpv.log").write_text(f"Opening https://m/rest/stream.view?u=a&t={token}&s=abc123\n")
        os.chmod(STATE / "mpv.log", 0o664)
        checks = self.by_label()
        self.assertFalse(checks["log modes"][0])
        self.assertIn("chmod 600", checks["log modes"][2])
        self.assertFalse(checks["log secrets"][0])
        self.assertTrue(checks["log secrets"][2])
        lines = "\n".join(privacy.doctor_lines())
        self.assertNotIn(token, lines)
        for line in lines.splitlines():
            if line.startswith("privacy") and "WARN" in line:
                idx = lines.splitlines().index(line)
                self.assertTrue(lines.splitlines()[idx + 1].startswith("  fix: "), line)

    def test_tmp_cava_leftover_is_reported(self):
        fd, path = tempfile.mkstemp(prefix="nplay-cava-", suffix=".conf", dir=tempfile.gettempdir())
        os.close(fd)
        try:
            ok, detail, fix = self.by_label()["cava config"]
            self.assertFalse(ok)
            self.assertIn(path, fix)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
