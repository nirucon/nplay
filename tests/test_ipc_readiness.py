import json, os, socket, tempfile, threading, unittest
from unittest.mock import patch
from nplay.player import Player

class ReadinessTests(unittest.TestCase):
 def test_status_matches_loaded_media(self):
  player=Player()
  with tempfile.TemporaryDirectory() as d:
   sock=os.path.join(d,'mpv.sock')
   server=socket.socket(socket.AF_UNIX)
   server.bind(sock);server.listen(3)
   def respond():
    for expected,value in [('path','/tmp/song.mp3'),('idle-active',False),('core-idle',False)]:
     conn,_=server.accept()
     with conn:
      command=json.loads(conn.recv(4096).decode())['command']
      assert command==['get_property',expected]
      conn.sendall((json.dumps({'error':'success','data':value})+'\n').encode())
   t=threading.Thread(target=respond);t.start()
   self.assertEqual(player._playback_status(sock),('/tmp/song.mp3',False,False))
   t.join(timeout=2);server.close()
 def test_status_missing_socket(self):
  self.assertIsNone(Player()._playback_status('/tmp/nplay-test-nonexistent-socket'))
