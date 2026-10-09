import tempfile
from pathlib import Path
import unittest
from nplay.core import EventBus,PlaybackState,default_registry,SessionStore,PlaybackController
from nplay.model import Track

class CoreTests(unittest.TestCase):
 def test_event_bus(self):
  bus=EventBus();seen=[];bus.subscribe('x',lambda event,**p:seen.append((event,p['value'])));bus.emit('x',value=7);self.assertEqual(seen,[('x',7)])
 def test_playback_state_live_capabilities(self):
  t=Track(source='sr',id='p1',title='P1',kind='radio',seekable=False);s=PlaybackState().update_media(t,'mpv',3);self.assertEqual(s.status,'playing');self.assertFalse(s.seekable);self.assertEqual(s.kind,'radio');s.clear();self.assertEqual(s.status,'idle')
 def test_source_registry(self):
  r=default_registry();self.assertTrue(r.supports('local','search'));self.assertTrue(r.supports('sr','live'));self.assertFalse(r.supports('sr','random'))
 def test_atomic_session_roundtrip(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/'session.json';store=SessionStore(p);store.save({'position':12.5,'queue':[1,2]});self.assertEqual(store.load()['position'],12.5);self.assertFalse(p.with_suffix('.json.tmp').exists())
 def test_playback_controller_stop_clears(self):
  class A:
   def stop_and_clear(self,ui=None):self.cleared=True
  a=A();a.cleared=False;PlaybackController(a).stop();self.assertTrue(a.cleared)

 def test_spotify_local_start_has_module_logger_and_no_credential_gate(self):
  import inspect
  import nplay.spotify as spotify_mod
  from nplay.app import App
  self.assertTrue(hasattr(spotify_mod,'log'))
  src=inspect.getsource(App.play)
  self.assertNotIn('has_credentials()',src)
  self.assertIn('ensure_local_device(name,20)',src)

if __name__=='__main__':unittest.main()
