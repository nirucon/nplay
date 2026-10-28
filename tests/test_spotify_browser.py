import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

import nplay.spotify as spotify_mod
from nplay.spotify import Spotify


class BrowserResolverTests(unittest.TestCase):
    def test_browser_env_with_placeholder(self):
        with patch.dict(os.environ, {'BROWSER': 'firefox %s'}, clear=False), patch.object(
            spotify_mod.shutil, 'which', return_value=None
        ):
            commands = spotify_mod._browser_commands('https://example.test/auth')
        self.assertEqual(commands[0], (['firefox', 'https://example.test/auth'], 'BROWSER'))

    def test_browser_env_without_placeholder(self):
        with patch.dict(os.environ, {'BROWSER': 'helium'}, clear=False), patch.object(
            spotify_mod.shutil, 'which', return_value=None
        ):
            commands = spotify_mod._browser_commands('https://example.test/auth')
        self.assertEqual(commands[0], (['helium', 'https://example.test/auth'], 'BROWSER'))

    def test_browser_env_supports_multiple_candidates(self):
        with patch.dict(os.environ, {'BROWSER': 'firefox:chromium %s'}, clear=False), patch.object(
            spotify_mod.shutil, 'which', return_value=None
        ):
            commands = spotify_mod._browser_commands('https://example.test/auth')
        self.assertEqual(
            commands[:2],
            [
                (['firefox', 'https://example.test/auth'], 'BROWSER'),
                (['chromium', 'https://example.test/auth'], 'BROWSER'),
            ],
        )

    def test_desktop_exec_field_codes(self):
        with tempfile.TemporaryDirectory() as folder:
            desktop = Path(folder) / 'helium.desktop'
            desktop.write_text(
                '[Desktop Entry]\nName=Helium\nExec=helium --new-window %U --label=%%\n',
                encoding='utf-8',
            )
            command = spotify_mod._desktop_browser_command(str(desktop))
        self.assertEqual(command, ['helium', '--new-window', '{url}', '--label=%'])

    def test_desktop_without_url_placeholder_gets_url(self):
        with tempfile.TemporaryDirectory() as folder:
            desktop = Path(folder) / 'browser.desktop'
            desktop.write_text('[Desktop Entry]\nExec=browser --incognito\n', encoding='utf-8')
            command = spotify_mod._desktop_browser_command(str(desktop))
        self.assertEqual(command, ['browser', '--incognito', '{url}'])

    def test_open_uses_browser_env_and_returns_true(self):
        with patch.dict(os.environ, {'BROWSER': 'firefox %s'}, clear=False), patch.object(
            spotify_mod.shutil, 'which', return_value=None
        ), patch.object(spotify_mod.subprocess, 'Popen') as popen:
            opened = spotify_mod._open_authorization_url('https://example.test/auth')

        self.assertTrue(opened)
        popen.assert_called_once()
        self.assertEqual(popen.call_args.args[0], ['firefox', 'https://example.test/auth'])

    def test_no_browser_returns_false(self):
        with patch.dict(os.environ, {'BROWSER': ''}, clear=False), patch.object(
            spotify_mod.shutil, 'which', return_value=None
        ), patch.object(spotify_mod.webbrowser, 'open', return_value=False):
            opened = spotify_mod._open_authorization_url('https://example.test/auth')
        self.assertFalse(opened)


class OAuthServerLifecycleTests(unittest.TestCase):
    def make_spotify(self):
        obj = Spotify.__new__(Spotify)
        obj.cfg = MagicMock()
        obj.cfg.get.side_effect = lambda key, default='': (
            'client-id' if key == 'spotify_client_id' else default
        )
        return obj

    def test_callback_port_is_closed_when_browser_cannot_open(self):
        fake_server = MagicMock()
        with patch.object(spotify_mod, 'HTTPServer', return_value=fake_server), patch.object(
            spotify_mod, '_open_authorization_url', return_value=False
        ):
            with self.assertRaisesRegex(RuntimeError, 'could not be opened'):
                self.make_spotify().connect(open_browser=True)
        fake_server.server_close.assert_called_once_with()

    def test_callback_port_conflict_has_actionable_error(self):
        with patch.object(spotify_mod, 'HTTPServer', side_effect=OSError('Address already in use')):
            with self.assertRaisesRegex(RuntimeError, 'port 43821 is unavailable'):
                self.make_spotify().connect(open_browser=False)


if __name__ == '__main__':
    unittest.main()
