import tempfile, unittest, io, urllib.error
from pathlib import Path
from types import SimpleNamespace
from nplay.statistics import StatisticsStore, ListeningTracker
from nplay.statistics.sync import StatisticsSync, SyncError

class Cfg:
 def __init__(self):self.enabled=True;self.key='dummy'
 def getbool(self,key,default=False):return self.enabled
 def get(self,key,default=''):return default
 def statistics_token(self):return self.key

class SyncTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.store=StatisticsStore(Path(self.tmp.name)/'statistics.sqlite3')
  self.cfg=Cfg();self.track=SimpleNamespace(source='navidrome',id='id',title='Song',artist='Artist',album='Album')
  for i in range(5):self.store.add('session',1800000000+i*2,2,self.track)
 def tearDown(self):self.tmp.cleanup()
 def sync(self,transport):return StatisticsSync(self.store,self.cfg,transport=transport)
 def test_complete_ack(self):
  def transport(url,token,payload):
   self.assertTrue(url.startswith('https://'));self.assertEqual(token,'dummy')
   return {'ok':True,'format_version':1,'acknowledged_event_ids':[e['event_id'] for e in payload['events']]}
  sync=self.sync(transport)
  self.assertEqual(sync.once(),5);self.assertEqual(self.store.summary('all')['pending'],0)
  self.assertEqual(self.store.summary('all')['seconds'],10)
 def test_partial_ack(self):
  sync=self.sync(lambda u,t,p:{'ok':True,'format_version':1,'acknowledged_event_ids':[p['events'][0]['event_id']]})
  self.assertEqual(sync.once(),1);self.assertEqual(self.store.summary('all')['pending'],4)
 def test_timeout_preserves_all(self):
  sync=self.sync(lambda u,t,p:(_ for _ in ()).throw(TimeoutError('timeout')))
  with self.assertRaises(SyncError):sync.once()
  self.assertEqual(self.store.summary('all')['pending'],5)
  self.assertGreater(sync.next_retry,sync.clock())
 def test_duplicate_ack_and_unknown_ack_rejected(self):
  for fn in [lambda p:[p['events'][0]['event_id']]*2,lambda p:['not-a-real-event']]:
   sync=self.sync(lambda u,t,p:{'ok':True,'format_version':1,'acknowledged_event_ids':fn(p)})
   with self.assertRaises(SyncError):sync.once()
   self.assertEqual(self.store.summary('all')['pending'],5)
 def test_403_does_not_ack(self):
  sync=self.sync(lambda u,t,p:(_ for _ in ()).throw(urllib.error.HTTPError(u,403,'Forbidden',{},io.BytesIO(b'bad'))))
  with self.assertRaisesRegex(SyncError,'Invalid token'):sync.once()
  self.assertEqual(self.store.summary('all')['pending'],5)
  self.assertEqual(sync.next_retry,float('inf'))
 def test_offline_only(self):
  self.cfg.enabled=False
  sync=self.sync(lambda *args:self.fail('must not send'))
  self.assertEqual(sync.once(),0)
  self.assertEqual(self.store.summary('all')['pending'],5)
 def test_https_only(self):
  self.cfg.get=lambda key,default='':'http://insecure.test/'
  sync=self.sync(lambda *args:self.fail('must not send'))
  with self.assertRaisesRegex(SyncError,'HTTPS'):sync.once()
 def test_batch_max_50_and_existing_db(self):
  for i in range(60):self.store.add('session',1800000010+i*2,2,self.track)
  sync=self.sync(lambda u,t,p:{'ok':True,'format_version':1,'acknowledged_event_ids':[e['event_id'] for e in p['events']]})
  identity=self.store.installation_id()
  self.assertEqual(sync.once(),50)
  self.assertEqual(self.store.summary('all')['pending'],15)
  self.assertEqual(StatisticsStore(self.store.path).installation_id(),identity)
 def test_server_503_preserves_all(self):
  sync=self.sync(lambda u,t,p:(_ for _ in ()).throw(urllib.error.HTTPError(u,503,'Unavailable',{},io.BytesIO(b''))))
  with self.assertRaisesRegex(SyncError,'storage'):sync.once()
  self.assertEqual(self.store.summary('all')['pending'],5)
 def test_legacy_oversized_event_not_deleted(self):
  self.store.add('legacy',1800000000,9,self.track)
  # Sorted by start_utc; invalid record included in first batch.
  sync=self.sync(lambda *args:self.fail('must not send'))
  with self.assertRaisesRegex(SyncError,'Legacy interval'):sync.once()
  self.assertEqual(self.store.summary('all')['pending'],6)

class AggregationTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.store=StatisticsStore(Path(self.tmp.name)/'statistics.sqlite3')
  self.t=SimpleNamespace(source='spotify',id='track',title='Track',artist='Artist',album='Album')
  self.mon=[0.];self.wall=[1800000000.]
  self.tr=ListeningTracker(self.store,clock=lambda:self.mon[0],wall=lambda:self.wall[0])
 def tearDown(self):self.tmp.cleanup()
 def step(self,n):self.mon[0]+=n;self.wall[0]+=n;self.tr.tick(self.t,True)
 def test_five_second_aggregation(self):
  self.tr.tick(self.t,True)
  for i in range(15):self.step(2)
  self.assertEqual(self.store.summary('all')['seconds'],30)
  self.assertEqual(self.store.summary('all')['pending'],6)
 def test_stop_flush(self):
  self.tr.tick(self.t,True);self.step(2);self.tr.flush(True)
  self.assertEqual(self.store.summary('all')['seconds'],2)
 def test_pause_and_new_track(self):
  self.tr.tick(self.t,True);self.step(2);self.tr.tick(self.t,False)
  other=SimpleNamespace(source='spotify',id='other',title='Other',artist='Artist',album='Album')
  self.mon[0]+=1;self.wall[0]+=1;self.tr.tick(other,True)
  self.mon[0]+=2;self.wall[0]+=2;self.tr.tick(other,True);self.tr.flush()
  self.assertEqual(self.store.summary('all')['seconds'],4)
  self.assertEqual(self.store.summary('all')['sessions'],2)
 def test_suspend_gap(self):
  self.tr.tick(self.t,True);self.step(2);self.step(100)
  self.tr.flush()
  self.assertEqual(self.store.summary('all')['seconds'],2)

class BoundaryTests(unittest.TestCase):
 def test_midnight_split_utc(self):
  import os,time
  previous=os.environ.get('TZ')
  try:
   os.environ['TZ']='UTC';time.tzset()
   with tempfile.TemporaryDirectory() as folder:
    store=StatisticsStore(Path(folder)/'stats.sqlite3')
    # 2026-10-09 23:59:58 UTC
    from datetime import datetime,timezone
    stamp=datetime(2026,10,9,23,59,58,tzinfo=timezone.utc).timestamp()
    clock=[0.];wall=[stamp]
    t=SimpleNamespace(source='local',id='a',title='A',artist='B',album='C')
    tracker=ListeningTracker(store,clock=lambda:clock[0],wall=lambda:wall[0]);tracker.tick(t,True)
    clock[0]=4.;wall[0]+=4;tracker.tick(t,True);tracker.flush()
    self.assertAlmostEqual(store.summary('all')['seconds'],4)
    self.assertEqual(store.summary('all')['days'],2)
    self.assertEqual(len(store.pending_events()),2)
  finally:
   if previous is None:os.environ.pop('TZ',None)
   else:os.environ['TZ']=previous
   time.tzset()
 def test_existing_152_database_survives_open_and_ack(self):
  with tempfile.TemporaryDirectory() as folder:
   path=Path(folder)/'statistics.sqlite3'
   store=StatisticsStore(path)
   identity=store.installation_id()
   t=SimpleNamespace(source='spotify',id='track',title='Song',artist='Band',album='Album')
   for i in range(700):store.add('legacy-session',1800000000+i*2,2,t)
   reopened=StatisticsStore(path)
   self.assertEqual(reopened.installation_id(),identity)
   self.assertEqual(reopened.summary('all')['pending'],700)
   self.assertEqual(reopened.summary('all')['seconds'],1400)
   self.assertEqual(len(reopened.pending_events(50)),50)
   self.assertEqual(len(reopened.pending_events(700)),500) # bounded read

class TransportTests(unittest.TestCase):
 def test_actual_https_client_serialization_via_mocked_opener(self):
  from unittest.mock import patch
  import json
  from nplay.statistics.sync import StatisticsSync
  class Response:
   status=200
   def __enter__(self):return self
   def __exit__(self,*a):return False
   def read(self,n):return b'{"ok":true,"format_version":1,"acknowledged_event_ids":[]}'
  class Opener:
   def open(self,request,timeout):
    self_req=request
    assert self_req.full_url=='https://example.test/api'
    assert timeout==12
    assert self_req.get_header('Authorization')=='Bearer dummy'
    assert json.loads(self_req.data)['installation_id']=='00000000-0000-0000-0000-000000000000'
    return Response()
  with tempfile.TemporaryDirectory() as folder:
   store=StatisticsStore(Path(folder)/'stats.db')
   store.installation_id=lambda:'00000000-0000-0000-0000-000000000000'
   sync=StatisticsSync(store,Cfg())
   with patch('urllib.request.build_opener',return_value=Opener()):
    result=sync._send('https://example.test/api','dummy',{'installation_id':store.installation_id(),'events':[]})
   self.assertTrue(result['ok'])
