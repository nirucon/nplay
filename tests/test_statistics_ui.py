import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from nplay.statistics import StatisticsStore
from nplay.app import App
from nplay.statistics.sync import StatisticsSync

class FakeUI:
 def __init__(self):self.title='';self.items=[];self.push=None;self.message=''
 def show_menu(self,title,items,push=True):
  self.title=title;self.items=items;self.push=push
 def status(self,msg):self.message=msg

class FakeConfig:
 def __init__(self):self.values={'statistics_enabled':'true'}
 def getbool(self,k,d=False):return self.values.get(k,str(d).lower())=='true'
 def set(self,k,v):self.values[k]=v

class StatisticsUITests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory()
  self.app=App.__new__(App)
  self.app.statistics=StatisticsStore(Path(self.tmp.name)/'stats.db')
  self.app.cfg=FakeConfig()
  self.app.cfg.statistics_token=lambda: "test-token"
  self.app.cfg.get=lambda k,d="": d
  self.app.statistics_sync=StatisticsSync(self.app.statistics,self.app.cfg)
  self.app.statistics_tracker=SimpleNamespace(enabled=True,reset=lambda:None)
  self.ui=FakeUI()
  track=SimpleNamespace(source='navidrome',id='1',title='Havenless',artist='Enslaved',album='Below the Lights')
  self.app.statistics.add('session',1800000000,3,track)
 def tearDown(self):self.tmp.cleanup()
 def test_overview_exposes_time_and_rankings_without_selection(self):
  self.app.stats_home(self.ui,'all')
  labels=[x[0] for x in self.ui.items]
  self.assertIn('ACTUAL LISTENING · 0m 03s',labels)
  self.assertTrue(any('Enslaved · 0m 03s' in label for label in labels))
  self.assertTrue(any('Havenless · 0m 03s' in label for label in labels))
  self.assertNotIn('INSTALLATION ID',labels)
 def test_period_change_replaces_instead_of_stacking(self):
  self.app.stats_home(self.ui,'day',push=False)
  self.assertFalse(self.ui.push)
 def test_details_and_privacy(self):
  self.app.stats_details(self.ui,'artists','all')
  self.assertTrue(any('Enslaved' in x[0] for x in self.ui.items))
  self.app.stats_settings(self.ui)
  labels=[x[0] for x in self.ui.items]
  self.assertIn('SYNC · DISABLED',labels)
  self.assertIn('COPY INSTALLATION ID',labels)
 def test_disabled_state(self):
  self.app.cfg.set('statistics_enabled','false')
  self.app.stats_home(self.ui)
  self.assertTrue(any('ENABLE LOCAL STATISTICS'==x[0] for x in self.ui.items))
