"""Non-blocking mpv startup and source handoff coordination.

Network resolution and mpv's file-loaded wait never run on the curses thread.
The previous authoritative media remains current until the new transport is ready.
"""
import threading,time
from ..logging_utils import logger
log=logger()

class MpvStartCoordinator:
 def __init__(self,app):self.app=app;self._start_lock=threading.Lock()
 def _prepare(self,t):
  a=self.app;started=time.monotonic();phase=started
  if t.source=='youtube':
   url=t.meta.get('webpage') or (f'https://www.youtube.com/watch?v={t.id}' if t.id else t.url)
  elif t.source=='sr' and not t.url:
   t=a.sr.resolve(t);url=t.url
  elif t.source=='navidrome':
   n=a.nav()
   if not n:raise RuntimeError('Navidrome is not configured or is disabled')
   if not t.url or not t.cover:
    fresh=n.song(t.id);t.url=fresh.url;t.cover=fresh.cover;t.duration=fresh.duration or t.duration;t.meta=fresh.meta or t.meta
   url=t.url
  else:url=t.url or t.path
  if not url:raise RuntimeError('No playable audio URL')
  log.info('mpv prepare request source=%s track=%s resolve_ms=%d',t.source,t.id,int((time.monotonic()-phase)*1000))
  return t,url,started
 def start(self,t,ui,request,resume_pos=0,was_spotify=False):
  a=self.app
  def work():
   try:
    track,url,started=self._prepare(t)
    with self._start_lock:
     if request!=a.playback_request:return
     rg=a.normalization_mode(track);load_started=time.monotonic()
     a.player.start(url,replaygain=rg,preamp=float(a.cfg.get('replaygain_preamp','0') or 0),clip=a.cfg.getbool('replaygain_clip',True),gapless=a.cfg.getbool('gapless',False),ytdl_path=(a.yt.binary if track.source=='youtube' else None))
    load_ms=int((time.monotonic()-load_started)*1000);total_ms=int((time.monotonic()-started)*1000)
    log.info('mpv ready request=%s source=%s track=%s load_ms=%d total_ms=%d',request,track.source,track.id,load_ms,total_ms)
    if request!=a.playback_request:
     log.info('mpv ready ignored stale request=%s current_request=%s',request,a.playback_request);return
    ui.post(lambda track=track:self._commit(track,ui,request,resume_pos,was_spotify,total_ms))
   except Exception as e:
    msg=str(e);log.exception('mpv start failed request=%s source=%s track=%s',request,t.source,t.id)
    if request==a.playback_request:ui.post(lambda msg=msg:ui.status('Playback failed · '+msg))
  ui.status('Loading · '+t.title);ui.invalidate();threading.Thread(target=work,daemon=True,name=f'nplay-mpv-{request}').start()
 def _commit(self,t,ui,request,resume_pos,was_spotify,total_ms):
  a=self.app
  if request!=a.playback_request:return
  a._begin_playback_session(t,'mpv')
  if was_spotify:a._stop_spotify_session()
  a.player.cmd(['set_property','pause',False]);a._commit_playback_session(t,'mpv');a.resume_candidate=False;a.resume_position=0.0;a.player.set_volume(a.volume)
  if resume_pos and t.seekable:a.player.set_position(resume_pos);a.resume_position=float(resume_pos)
  a.db.history_add(t);a.save_state()
  if not (t.kind=='radio' and t.source=='sr'):a.notifier.track_changed(t)
  a.ui=ui;ui.now_playing();ui.status(f'Playing · {t.title} · {total_ms/1000:.1f}s start' if total_ms>=2000 else f'Playing · {t.title}')
  if t.kind=='radio':
   if t.source=='sr':a.refresh_radio_async([t],ui)
   a.radio_poll_start(t,ui)
