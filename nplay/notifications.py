from . import __version__
import os,shutil,subprocess,threading
from .artwork_cache import ArtworkCache

class Notifier:
 """Small freedesktop notification adapter. Playback providers never call notify-send directly."""
 def __init__(self,cfg,cache=None):
  self.cfg=cfg;self.binary=shutil.which('notify-send');self.cache=cache or ArtworkCache(int(cfg.get('artwork_cache_mb','500') or 500));self.last_key='';self.lock=threading.Lock()
 def available(self):return bool(self.binary and (os.getenv('DBUS_SESSION_BUS_ADDRESS') or os.getenv('DISPLAY') or os.getenv('WAYLAND_DISPLAY')))
 def enabled(self):return self.cfg.getbool('track_notifications',True) and self.available()
 def _artwork(self,cover):
  if not self.cfg.getbool('notification_artwork',True) or not cover:return None
  return self.cache.fetch(cover,timeout=6,user_agent=f'NPLAY/{__version__}')
 def _send(self,title,body='',icon=None):
  if not self.available():return False
  cmd=[self.binary,'--app-name=NPLAY','--urgency=low','--expire-time=5000','--hint=string:x-canonical-private-synchronous:nplay-now-playing']
  if icon:cmd+=['--icon',str(icon)]
  cmd += [str(title or 'NPLAY'),str(body or '')]
  try:return subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=4).returncode==0
  except Exception:return False
 def track_changed(self,t,force=False,radio_info=None):
  if not t or not self.enabled():return
  key=f'{getattr(t,"source","")}:{getattr(t,"id","") or getattr(t,"url","") or getattr(t,"path","")}'
  with self.lock:
   if not force and key==self.last_key:return
   self.last_key=key
  def work():
   art=self._artwork(getattr(t,'cover',''))
   artist=(getattr(t,'artist','') or '').strip();album=(getattr(t,'album','') or '').strip();src=getattr(t,'source','') or 'NPLAY';source=src.replace('sr','Sveriges Radio').replace('navidrome','Navidrome').replace('spotify','Spotify').replace('youtube','YouTube').replace('local','Local')
   # Live SR is programme-first: avoid P1 / Sveriges Radio / Sveriges Radio duplication.
   if src=='sr' and getattr(t,'kind','')=='radio':
    info=radio_info or {};program=(info.get('program') or info.get('episode') or '').strip();channel=(getattr(t,'title','') or 'Sveriges Radio').strip()
    title=program or channel;body=' · '.join(x for x in (channel if program else '', 'Sveriges Radio') if x)
    self._send(title,body,art);return
   lines=[]
   if artist:lines.append(artist)
   detail=' · '.join(x for x in (album,source) if x and x.casefold()!=artist.casefold())
   if detail:lines.append(detail)
   self._send(getattr(t,'title','') or 'Now playing','\n'.join(lines),art)
  threading.Thread(target=work,daemon=True).start()
 def test(self,t=None):
  if not self.available():return False
  if t:
   self.track_changed(t,force=True);return True
  threading.Thread(target=lambda:self._send('NPLAY notifications','System notifications are working.'),daemon=True).start();return True
