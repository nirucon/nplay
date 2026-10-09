"""Regression tests for mpv lifecycle and Spotify Web API context fallback."""
import os
import tempfile
import unittest
from unittest.mock import MagicMock
from subprocess import TimeoutExpired
from types import SimpleNamespace
from nplay.player import Player
from nplay.spotify import Spotify

class PlaybackStabilityTests(unittest.TestCase):
 def test_stop_reaps_and_removes_only_own_socket(self):
  player=Player()
  with tempfile.TemporaryDirectory() as d:
   sock=os.path.join(d,'owned.sock');other=os.path.join(d,'other.sock')
   open(sock,'w').close();open(other,'w').close()
   proc=MagicMock(pid=1234,returncode=0);proc.poll.return_value=None
   player.proc=proc;player.sock=sock;player.stop()
   proc.terminate.assert_called_once();proc.wait.assert_called_once()
   self.assertFalse(os.path.exists(sock));self.assertTrue(os.path.exists(other))
   self.assertIsNone(player.proc)
 def test_stuck_child_killed_and_reaped(self):
  player=Player();proc=MagicMock(pid=1234,returncode=-9);proc.poll.return_value=None
  proc.wait.side_effect=[TimeoutExpired('mpv',1),0]
  player.proc=proc;player.stop()
  proc.terminate.assert_called_once();proc.kill.assert_called_once()
  self.assertEqual(proc.wait.call_count,2)
 def test_context_403_fallback_to_track_once(self):
  spotify=Spotify.__new__(Spotify)
  spotify.put=MagicMock(side_effect=[RuntimeError('Spotify 403 · playback restricted by Spotify'),None])
  t=SimpleNamespace(id='song',meta={'spotify_uri':'spotify:track:song','context_uri':'spotify:album:album'})
  spotify.play(t,device_id='device')
  self.assertEqual(spotify.put.call_count,2)
  self.assertEqual(spotify.put.call_args.args[2],{'uris':['spotify:track:song']})
 def test_non_context_403_is_not_retried(self):
  spotify=Spotify.__new__(Spotify)
  spotify.put=MagicMock(side_effect=RuntimeError('Spotify 403 · playback restricted by Spotify'))
  t=SimpleNamespace(id='song',meta={'spotify_uri':'spotify:track:song'})
  with self.assertRaisesRegex(RuntimeError,'Spotify 403'):spotify.play(t,device_id='device')
  spotify.put.assert_called_once()
 def test_other_errors_not_retried(self):
  spotify=Spotify.__new__(Spotify)
  spotify.put=MagicMock(side_effect=RuntimeError('Spotify 401'))
  t=SimpleNamespace(id='song',meta={'spotify_uri':'spotify:track:song','context_uri':'spotify:album:album'})
  with self.assertRaisesRegex(RuntimeError,'Spotify 401'):spotify.play(t,device_id='device')
  spotify.put.assert_called_once()
