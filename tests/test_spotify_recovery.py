import unittest
from unittest.mock import MagicMock, patch
from nplay.spotify import Spotify, LocalDeviceRegistrationTimeout

class SpotifyRecoveryTests(unittest.TestCase):
 def make(self):
  obj=Spotify.__new__(Spotify)
  obj.cfg=MagicMock()
  obj.cfg.get.return_value="NPLAY"
  obj.local=MagicMock()
  import threading
  obj._recovery_lock=threading.Lock()
  obj._last_recovery=0.0
  return obj
 def test_registered_device_does_not_restart(self):
  s=self.make();expected={"id":"123","name":"NPLAY"}
  s.wait_for_device=MagicMock(return_value=expected)
  self.assertEqual(s.ensure_local_device("NPLAY"),expected)
  s.local.restart.assert_not_called()
 def test_missing_device_restarts_once_and_retries(self):
  s=self.make();expected={"id":"new","name":"NPLAY"}
  s.wait_for_device=MagicMock(side_effect=[LocalDeviceRegistrationTimeout("missing"),expected])
  s.devices=MagicMock(return_value=[])
  self.assertEqual(s.ensure_local_device("NPLAY"),expected)
  s.local.restart.assert_called_once_with()
  self.assertEqual(s.wait_for_device.call_count,2)
 def test_second_failure_stops_after_one_restart(self):
  s=self.make()
  s.wait_for_device=MagicMock(side_effect=LocalDeviceRegistrationTimeout("missing"))
  s.devices=MagicMock(return_value=[])
  with self.assertRaisesRegex(RuntimeError,"after one automatic"):
   s.ensure_local_device("NPLAY")
  s.local.restart.assert_called_once_with()
 def test_cooldown_blocks_restart_loop(self):
  s=self.make();s.wait_for_device=MagicMock(side_effect=LocalDeviceRegistrationTimeout("missing"))
  s.devices=MagicMock(return_value=[])
  with patch("nplay.spotify.time.monotonic",return_value=200):
   s._last_recovery=180
   with self.assertRaisesRegex(RuntimeError,"cooldown"):
    s.ensure_local_device("NPLAY")
  s.local.restart.assert_not_called()
 def test_other_request_recovered_before_restart(self):
  s=self.make();expected={"id":"123","name":"NPLAY","is_restricted":False}
  s.wait_for_device=MagicMock(side_effect=LocalDeviceRegistrationTimeout("missing"))
  s.devices=MagicMock(return_value=[expected])
  self.assertEqual(s.ensure_local_device("NPLAY"),expected)
  s.local.restart.assert_not_called()
