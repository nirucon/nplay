"""uninstall.sh keeps user data unless --purge is given (1.8.2)."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class UninstallTest(unittest.TestCase):
    def _home(self, tmp):
        home = Path(tmp)
        for rel in ('.local/bin/nplay', '.local/share/applications/nplay.desktop',
                    '.local/lib/nplay/nplay/__init__.py', '.local/share/nplay/library.db',
                    '.local/share/nplay/spotify-local/credentials.json',
                    '.config/nplay/config.ini', '.local/state/nplay/nplay.log',
                    '.cache/nplay/art.png'):
            p = home / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text('x')
        return home

    def _run(self, home, *args):
        env = {k: v for k, v in os.environ.items() if not k.startswith('XDG_')}
        env['HOME'] = str(home)
        return subprocess.run(['bash', str(ROOT / 'uninstall.sh'), *args], env=env,
                              capture_output=True, text=True, check=False)

    def test_default_keeps_data_config_and_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self._home(tmp)
            r = self._run(home)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertFalse((home / '.local/bin/nplay').exists())
            self.assertFalse((home / '.local/lib/nplay').exists())
            self.assertFalse((home / '.local/share/applications/nplay.desktop').exists())
            self.assertTrue((home / '.local/share/nplay/library.db').exists())
            self.assertTrue((home / '.local/share/nplay/spotify-local/credentials.json').exists())
            self.assertTrue((home / '.config/nplay/config.ini').exists())
            self.assertTrue((home / '.local/state/nplay/nplay.log').exists())

    def test_purge_removes_everything(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self._home(tmp)
            r = self._run(home, '--purge')
            self.assertEqual(r.returncode, 0, r.stderr)
            for rel in ('.local/share/nplay', '.config/nplay', '.local/state/nplay', '.cache/nplay',
                        '.local/lib/nplay', '.local/bin/nplay'):
                self.assertFalse((home / rel).exists(), rel)

    def test_unknown_option_is_refused_without_removing(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self._home(tmp)
            r = self._run(home, '--everything')
            self.assertEqual(r.returncode, 2)
            self.assertTrue((home / '.local/bin/nplay').exists())


class InstallerTest(unittest.TestCase):
    def test_installer_parses(self):
        subprocess.run(['bash', '-n', str(ROOT / 'install.sh')], check=True)
        subprocess.run(['bash', '-n', str(ROOT / 'uninstall.sh')], check=True)

    def test_installer_honours_unattended_consent(self):
        text = (ROOT / 'install.sh').read_text()
        self.assertIn('NIRUX_ASSUME_YES', text)
        self.assertIn('--noconfirm', text)


if __name__ == '__main__':
    unittest.main()
