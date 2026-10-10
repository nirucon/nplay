import unittest
from nplay.ui import _cell_width, _printable_input

class UnicodePromptTests(unittest.TestCase):
 def test_swedish(self):
  for ch in 'åäöÅÄÖ':
   self.assertTrue(_printable_input(ch))
   self.assertEqual(_cell_width(ch), 1)
 def test_controls(self):
  for ch in ('\x1b','\t','\r','\n','\x7f'):
   self.assertFalse(_printable_input(ch))
 def test_width(self):
  self.assertEqual(_cell_width('Örebro'), 6)
  self.assertEqual(_cell_width('漢字'), 4)
  self.assertEqual(_cell_width('e\u0301'), 1)
if __name__=='__main__':unittest.main()
