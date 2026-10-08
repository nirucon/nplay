import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from nplay.theme_actions import open_theme_directory

class ThemeUXTests(unittest.TestCase):
 def test_directory_created_and_opened_with_argument_list(self):
  with tempfile.TemporaryDirectory() as home:
   target=Path(home)/"nplay"/"themes"
   with patch("nplay.theme_actions.shutil.which",return_value="/usr/bin/xdg-open"),patch("nplay.theme_actions.subprocess.Popen") as popen:
    ok,msg=open_theme_directory(target)
   self.assertTrue(ok,msg)
   self.assertTrue(target.is_dir())
   self.assertEqual(popen.call_args.args[0],["/usr/bin/xdg-open",str(target)])
   self.assertTrue(popen.call_args.kwargs["start_new_session"])
 def test_missing_opener_is_nonfatal(self):
  with tempfile.TemporaryDirectory() as home:
   with patch("nplay.theme_actions.shutil.which",return_value=None):
    ok,msg=open_theme_directory(Path(home)/"themes")
   self.assertFalse(ok)
   self.assertIn("No desktop file opener",msg)
 def test_symlink_is_rejected(self):
  with tempfile.TemporaryDirectory() as home:
   root=Path(home)
   target=root/"real";target.mkdir()
   link=root/"themes";link.symlink_to(target,target_is_directory=True)
   ok,msg=open_theme_directory(link)
   self.assertFalse(ok)
   self.assertIn("not a regular directory",msg)
