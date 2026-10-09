import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from nplay.statistics import StatisticsStore
from nplay.statistics.adapter import pending_envelope
from nplay.app import App

class UI:
 def __init__(self):
  self.mode='menu';self.title='';self.items=[];self.sel=0;self.stack=[];self.invalidated=False
 def show_menu(self,title,items,push=True):
  if push:self.stack.append((self.title,self.items,self.sel))
  self.title=title;self.items=list(items);self.sel=next((i for i,x in enumerate(items) if x[1]),0)
 def selectable(self,i):return 0<=i<len(self.items) and bool(self.items[i][1])
 def edge_selection(self,last=False):
  self.sel=next((i for i,x in enumerate(self.items) if x[1]),0)
 def invalidate(self):self.invalidated=True

class Config:
 def getbool(self,k,default=False):return True

class StatisticsPolish(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory()
  self.store=StatisticsStore(Path(self.tmp.name)/'statistics.sqlite3')
  self.app=App.__new__(App);self.app.statistics=self.store;self.app.cfg=Config()
  self.ui=UI()
 def tearDown(self):self.tmp.cleanup()
 def test_refresh_preserves_selected_period_and_stack(self):
  self.app.stats_home(self.ui,'week')
  self.ui.sel=next(i for i,x in enumerate(self.ui.items) if x[1]=='stats:period:year')
  depth=len(self.ui.stack)
  self.assertTrue(self.app.refresh_statistics_view(self.ui))
  self.assertEqual(self.ui.items[self.ui.sel][1],'stats:period:year')
  self.assertEqual(len(self.ui.stack),depth)
  self.assertTrue(self.ui.invalidated)
 def test_refresh_details_keeps_navigation_history(self):
  self.app.stats_details(self.ui,'artists','all')
  depth=len(self.ui.stack)
  self.assertTrue(self.app.refresh_statistics_view(self.ui))
  self.assertEqual(len(self.ui.stack),depth)
  self.assertIn('ARTISTS',self.ui.title)
 def test_period_rows_have_no_repeated_helper_text(self):
  self.app.stats_home(self.ui,'day')
  self.assertTrue(all(not x[2] for x in self.ui.items if x[1].startswith('stats:period:')))
 def test_provider_neutral_envelope_read_only_and_idempotent(self):
  track=SimpleNamespace(source='local',id='x',title='Track',artist='Artist',album='Album')
  self.store.add('session',1800000000,2,track,event_id='fixed')
  batch=pending_envelope(self.store)
  self.assertEqual(batch['format'],'nplay.listening-events')
  self.assertEqual(batch['installation_id'],self.store.installation_id())
  self.assertEqual(batch['events'][0]['event_id'],'fixed')
  self.assertEqual(batch['events'][0]['started_at'],'2027-01-15T08:00:00Z')
  self.assertEqual(len(self.store.pending_events()),1)
  self.assertEqual(batch,pending_envelope(self.store))
 def test_refresh_ignores_other_menus(self):
  self.ui.title='PLAYLISTS'
  self.assertFalse(self.app.refresh_statistics_view(self.ui))
