import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from nplay.statistics import StatisticsStore,ListeningTracker

class StatisticsTests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory()
  self.store=StatisticsStore(Path(self.temp.name)/'stats.sqlite3')
  self.track=SimpleNamespace(source='spotify',id='track1',title='Song',artist='Artist',album='Album',duration=300)
 def tearDown(self):self.temp.cleanup()
 def test_identity_stable_and_private_db(self):
  identity=self.store.installation_id()
  self.assertEqual(identity,StatisticsStore(self.store.path).installation_id())
  self.assertEqual(self.store.path.stat().st_mode & 0o777,0o600)
 def test_idempotent_event_and_outbox(self):
  for _ in range(3):self.store.add('session',1000,2,self.track,'event-1')
  self.assertEqual(self.store.summary('all')['seconds'],2)
  self.assertEqual(len(self.store.pending_events()),1)
  self.store.acknowledge(['event-1'])
  self.assertEqual(len(self.store.pending_events()),0)
 def test_real_playback_pause_and_track_switch(self):
  mono=[0];wall=[1800000000]
  tracker=ListeningTracker(self.store,clock=lambda:mono[0],wall=lambda:wall[0])
  tracker.tick(self.track,True)
  mono[0]=2;wall[0]+=2;tracker.tick(self.track,True)
  mono[0]=4;wall[0]+=2;tracker.tick(self.track,False)
  mono[0]=8;wall[0]+=4;tracker.tick(self.track,True)
  mono[0]=10;wall[0]+=2;tracker.tick(self.track,True)
  self.assertEqual(self.store.summary('all')['seconds'],4)
  self.assertEqual(self.store.summary('all')['sessions'],2)
 def test_suspend_gap_ignored(self):
  mono=[1];tracker=ListeningTracker(self.store,clock=lambda:mono[0],wall=lambda:1800000000+mono[0])
  tracker.tick(self.track,True);mono[0]=900;tracker.tick(self.track,True)
  self.assertEqual(self.store.summary('all')['seconds'],0)
 def test_disabled_does_not_record(self):
  mono=[0];tracker=ListeningTracker(self.store,enabled=False,clock=lambda:mono[0])
  tracker.tick(self.track,True);mono[0]=2;tracker.tick(self.track,True)
  self.assertEqual(self.store.summary('all')['seconds'],0)
 def test_source_separation(self):
  for source in ('local','navidrome','spotify','youtube','radio'):
   t=SimpleNamespace(source=source,id='same',title='Song',artist='Artist',album='Album')
   self.store.add(source,1800000000,2,t)
  self.assertEqual(len(self.store.summary('all')['sources']),5)
 def test_periods(self):
  self.store.add('old',1000,2,self.track)
  self.store.add('new',1800000000,2,self.track)
  self.assertEqual(self.store.summary('all')['seconds'],4)
  self.assertEqual(self.store.summary('day',now=1800000001)['seconds'],2)
