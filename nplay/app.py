import argparse,os,sys,threading,shutil,json,time
from pathlib import Path
from . import __version__
from .config import Config,STATE
from .db import DB
from .statistics import StatisticsStore,ListeningTracker
from .statistics.sync import StatisticsSync,DEFAULT_URL
from .player import Player
from .providers import Navidrome,SR,YouTube
from .spotify import Spotify
from .notifications import Notifier
from .artwork_cache import ArtworkCache
from .logging_utils import logger,LOG_PATH,recent_error_count
log=logger()
from .mpris import MPRIS
from .ui import UI
from .services.search import SearchCoordinator
from .services.radio import NPlayRadio
from .services.settings import SettingsController
from .services.playback_start import MpvStartCoordinator
from .navigation import NavigationMixin
from .core import EventBus,PlaybackState,default_registry,SessionStore,PlaybackController
class App(NavigationMixin):
 def __init__(self):
  self.cfg=Config();self.db=DB();self.statistics=StatisticsStore();self.statistics_tracker=ListeningTracker(self.statistics,self.cfg.getbool('statistics_enabled',True));self.statistics_sync=StatisticsSync(self.statistics,self.cfg);self.player=Player(self._on_track_end);self.current=None;self.resume_candidate=False;self.resume_position=0.0;self.volume=100.0;self.queue=[];self.later_queue=[];self.play_context=[];self.original_context=[];self.context_index=-1;self.back_stack=[];self.shuffle_mode=self.cfg.get('shuffle_mode','off');self.repeat_mode=self.cfg.get('repeat_mode','off');self.ui=None;self.sr=SR();self.yt=YouTube();self.spotify=Spotify(self.cfg);self.spotify_state={};self.spotify_polling=False;self.spotify_pending_track_id='';self.spotify_track_generation=0;self.playback_generation=0;self.playback_request=0;self.playback_state={};self.radio_info={};self.artwork_cache=ArtworkCache(int(self.cfg.get('artwork_cache_mb','500') or 500));self.notifier=Notifier(self.cfg,self.artwork_cache);self.mpris=MPRIS(self);self.sleep_deadline=0;self.sleep_mode='off';self.play_event_id=None;self.play_event_started=0.0;self.play_event_listened=0.0;self.play_event_tick=0.0;self.radio_poll_generation=0;self.state_path=STATE/'session.json';self.events=EventBus();self.state=PlaybackState();self.sources=default_registry();self.session=SessionStore(self.state_path);self.playback=PlaybackController(self);self.searcher=SearchCoordinator(self);self.radio_engine=NPlayRadio(self);self.settings=SettingsController(self);self.mpv_start=MpvStartCoordinator(self);self.load_state();self.mpris.start() if self.cfg.getbool('mpris_enabled',True) else None
 def load_state(self):
  try:
   d=self.session.load();from .model import Track
   self.current=Track.from_dict(d['current']) if d.get('current') else None;self.queue=[Track.from_dict(x) for x in d.get('queue',[])];self.later_queue=[Track.from_dict(x) for x in d.get('later_queue',[])];self.play_context=[Track.from_dict(x) for x in d.get('play_context',[])];self.context_index=int(d.get('context_index',-1));self.volume=float(d.get('volume',100) or 100);self.resume_position=float(d.get('position',0) or 0);self.resume_candidate=bool(self.current)
   # Rebuild ephemeral Navidrome cover URLs from stable coverArt ids.
   for t in ([self.current] if self.current else [])+self.queue+self.play_context:
    if t and t.source=='navidrome' and not t.cover and t.meta.get('cover_id'):
     try:
      n=self.nav();t.cover=n.cover(t.meta['cover_id']) if n else ''
     except Exception:pass
  except Exception:pass
 def save_state(self):
  try:
   self.session.save({'current':self._state_track(self.current),'queue':[self._state_track(x) for x in self.queue],'later_queue':[self._state_track(x) for x in self.later_queue],'play_context':[self._state_track(x) for x in self.play_context],'context_index':self.context_index,'position':self.resume_position,'volume':self.volume})
  except Exception:pass
 def _state_track(self,t):
  if not t:return None
  d=t.dict()
  # Navidrome auth URLs are ephemeral; recreate them from current credentials.
  if t.source=='navidrome':
   # Keep only stable identity in session state; auth-bearing URLs are regenerated.
   if t.cover:
    try:
     import urllib.parse
     cid=urllib.parse.parse_qs(urllib.parse.urlparse(t.cover).query).get('id',[''])[0]
     if cid:d.setdefault('meta',{})['cover_id']=cid
    except Exception:pass
   d['url']='';d['cover']=''
  return d
 def checkpoint(self,pos=None,volume=None):
  now=time.monotonic()
  if self.play_event_id and self.playback_active() and self.play_event_tick:self.play_event_listened+=max(0,min(5,now-self.play_event_tick))
  if self.play_event_id:self.play_event_tick=now
  if pos is not None:
   self.resume_position=max(0,float(pos or 0))
   if self.current:
    try:self.db.resume_set(f'{self.current.source}:{self.current.id}',self.resume_position)
    except Exception:pass  # playback checkpoints are best-effort and must never kill the UI
  if volume is not None:self.volume=max(0,min(150,float(volume)))
  self.save_state()
 def _spotify_device_id(self):
  # Connect device IDs are runtime session identifiers. Never reuse a persisted
  # librespot ID after restart.
  return str((self.spotify_state.get('device') or {}).get('id') or '').strip()
 def _spotify_local_device(self,timeout=20):
  self.spotify.local.start(oauth=False)
  name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY'
  d=self.spotify.wait_for_device(name,timeout);self.spotify_state['device']=d
  return d
 def _spotify_rebind(self,timeout=20):
  d=self._spotify_local_device(timeout);self.spotify.activate_device(d['id']);self.spotify_state['device']=d;return d
 def _spotify_with_device(self,action):
  """Run a transport action against a live device, rebinding once if stale."""
  local=self.cfg.get('spotify_playback','local')=='local';did=self._spotify_device_id()
  if not did:
   d=self._spotify_rebind() if local else self.spotify.playback_device();did=d.get('id','');self.spotify_state['device']=d
  try:return action(did)
  except RuntimeError as e:
   if not local or 'Device not found' not in str(e):raise
   d=self._spotify_rebind();return action(d['id'])
 def _spotify_control(self,ui,label,work,on_ok=None):
  """Run a Spotify control without ever leaking worker exceptions to curses/stderr."""
  def run():
   try:
    result=work()
    if ui:
     def done():
      if on_ok:on_ok(result)
      ui.status('Spotify · '+label);ui.invalidate()
     ui.post(done)
   except Exception as e:
    msg=str(e)
    if ui:ui.post(lambda msg=msg:ui.error('Spotify control unavailable',msg))
  threading.Thread(target=run,daemon=True).start()
 def play_pause(self,ui=None):
  if self.current and self.current.source=='spotify':
   did=self._spotify_device_id()
   if self.spotify_state.get('is_playing'):
    pos=self.playback_position()
    def pause_work():
     nonlocal did
     if not did:did=self._spotify_rebind()['id']
     try:return self.spotify.pause(did)
     except RuntimeError as e:
      if 'Device not found' not in str(e):raise
      did=self._spotify_rebind()['id'];return self.spotify.pause(did)
    self._spotify_control(ui,'paused',pause_work,lambda _:(self.spotify_state.update(is_playing=False,progress=pos,_seen=time.monotonic()),setattr(self,'resume_position',pos),self.save_state(),self._state_changed('PlaybackStatus')))
   else:
    pos=float(self.spotify_state.get('progress') or self.resume_position or 0)
    def resume_work():
     nonlocal did
     if not did:did=self._spotify_rebind()['id']
     try:self.spotify.resume(did)
     except RuntimeError as e:
      if 'Device not found' not in str(e):raise
      did=self._spotify_rebind()['id'];self.spotify.resume(did)
     # Recovery path: if Spotify lost the paused session, restore track+position.
     time.sleep(.15)
     st=self.spotify.state()
     if not st.get('track') or st['track'].id!=self.current.id:
      self.spotify.play(self.current,device_id=did)
      if pos>0:self.spotify.seek(pos,did)
     return True
    self._spotify_control(ui,'playing',resume_work,lambda _:(self.spotify_state.update(is_playing=True,progress=pos,_seen=time.monotonic()),self._state_changed('PlaybackStatus')))
   return
  if self.player.active():
   self.statistics_tracker.flush(self.playback_active())
   self.player.toggle();self._state_changed('PlaybackStatus');return
  if self.current:return self.play(self.current,ui,preserve_context=True,resume_pos=self.resume_position or self.db.resume_get(f'{self.current.source}:{self.current.id}',legacy_id=self.current.id))
  if ui:ui.status('Nothing to resume · browse or search for music')
 def fmt(self,s):s=max(0,int(s or 0));return f'{s//60}:{s%60:02d}'
 def log_debug(self,msg,e=None):
  log.debug(msg+((' · '+str(e)) if e else ''),exc_info=bool(e))
 def roots(self):return [str(Path(x).expanduser()) for x in self.cfg.get('music_dirs',str(Path.home()/'Music')).split(':') if x]
 def scan(self,progress=None,force=False):
  excludes=[x.strip() for x in self.cfg.get('music_excludes','Reaper-projects').split(':') if x.strip()]
  try:batch=max(25,min(2000,int(self.cfg.get('library_scan_batch','250') or 250)))
  except ValueError:batch=250
  return self.db.scan(self.roots(),progress=progress,excludes=excludes,batch_size=batch,force=force)
 def background_library_refresh(self,ui=None):
  if not self.cfg.getbool('auto_library_refresh',True):return
  def run():
   try:
    r=self.scan()
    if ui and (r.get('new') or r.get('updated') or r.get('removed')):ui.post(lambda:ui.status(f"Library refreshed · {r['new']} new · {r['updated']} updated · {r['removed']} removed"))
   except Exception:pass
  threading.Thread(target=run,daemon=True).start()
 def spotify_configured(self):return self.spotify.connected()
 def _begin_playback_session(self,t,transport):
  # Invalidate every previous async owner before a new source/track can take over.
  self.spotify_polling=False
  if transport!='spotify':self.spotify_pending_track_id=''
  self.playback_generation+=1
  self.playback_state={'generation':self.playback_generation,'source':getattr(t,'source',''),'transport':transport,'state':'starting','track_id':getattr(t,'id',''),'artwork':getattr(t,'cover','')};self.state.update_media(t,transport,self.playback_generation,'loading');self.events.emit('playback-state-changed',state=self.state)
  return self.playback_generation
 def _commit_playback_session(self,t,transport,state='playing'):
  self.statistics_tracker.flush(self.playback_active())
  self.current=t;self._start_play_event(t);self.playback_state={'generation':self.playback_generation,'source':getattr(t,'source',''),'transport':transport,'state':state,'track_id':getattr(t,'id',''),'artwork':getattr(t,'cover','')};self.state.update_media(t,transport,self.playback_generation,state);self.events.emit('media-changed',track=t,state=self.state)
  self._state_changed('PlaybackStatus','Metadata')
 def _stop_spotify_session(self):
  self.spotify_polling=False
  try:self.spotify.pause()
  except Exception:pass
 def _spotify_context_snapshot(self,t,preserve_context=False):
  """Freeze the context that belongs to a Spotify play request.

  Async Spotify startup must never commit whatever list happens to be in the UI
  later. If the requested track is not actually part of the current context, use
  a single-track context rather than leaking a previous search/album context.
  """
  key=self._track_key(t);context_uri=str((t.meta or {}).get('context_uri') or '')
  # Spotify search/top/recent lists are discovery lists, not native playback
  # contexts. Only album/playlist tracks carrying context_uri may own a multi-
  # track Spotify context; otherwise the requested track is intentionally 1/1.
  ctx=list(self.play_context) if context_uri and (preserve_context or self.play_context) else [t]
  if not ctx or not any(self._track_key(x)==key for x in ctx):ctx=[t]
  original=list(self.original_context) if self.original_context and any(self._track_key(x)==key for x in self.original_context) else list(ctx)
  idx=next((i for i,x in enumerate(ctx) if self._track_key(x)==key),0)
  return {'tracks':ctx,'original':original,'index':idx,'context_uri':context_uri}
 def _apply_spotify_context(self,snap,t):
  ctx=list((snap or {}).get('tracks') or [t]);key=self._track_key(t)
  if not any(self._track_key(x)==key for x in ctx):ctx=[t]
  self.play_context=ctx;self.original_context=list((snap or {}).get('original') or ctx)
  self.context_index=next((i for i,x in enumerate(ctx) if self._track_key(x)==key),0)
 def _spotify_observe_track(self,track_id,timeout=1.0,device_id=''):
  """Best-effort observation only. Spotify Web API is eventually consistent.

  A mismatch is never a playback failure: a successful play command may already
  be audible through librespot while /me/player still reports the previous item.
  """
  deadline=time.monotonic()+max(.1,float(timeout));last=None;last_observed=None
  while time.monotonic()<deadline:
   st=self.spotify.state();last=st;cur=st.get('track');observed=getattr(cur,'id','')
   if observed!=last_observed:
    log.info('spotify observe requested=%s observed=%s playing=%s device=%s',track_id,observed or 'none',st.get('is_playing'),(st.get('device') or {}).get('name',''));last_observed=observed
   if cur and cur.id==track_id and (not device_id or not (st.get('device') or {}).get('id') or (st.get('device') or {}).get('id')==device_id):return st
   time.sleep(.2)
  return None
 def _spotify_reconcile_start(self,ui,request,track_generation,requested_track,requested_context,device_id=''):
  """Reconcile pending Spotify playback without allowing stale Web API state back into UI."""
  playback_generation=self.playback_generation;requested_id=requested_track.id
  def run():
   last_observed=None;started=time.monotonic()
   while request==self.playback_request and track_generation==self.spotify_track_generation and playback_generation==self.playback_generation and self.spotify_pending_track_id==requested_id:
    try:
     st=self.spotify.state();st['_seen']=time.monotonic();incoming=st.get('track');observed=getattr(incoming,'id','')
     if observed!=last_observed:
      log.info('spotify reconcile request=%s track_gen=%s requested=%s observed=%s playing=%s age=%.1fs',request,track_generation,requested_id,observed or 'none',st.get('is_playing'),time.monotonic()-started);last_observed=observed
     if observed!=requested_id:
      # Critical invariant: stale Spotify state is diagnostic data only while an
      # explicit user request is pending. It must never replace current/context/artwork.
      time.sleep(.5 if time.monotonic()-started<20 else 2.0);continue
     incoming.meta={**(incoming.meta or {}),**{k:v for k,v in (requested_track.meta or {}).items() if k.startswith('context_')}}
     self.spotify_state=st;self.spotify_pending_track_id='';self._apply_spotify_context(requested_context,incoming);self.current=incoming;self.playback_state.update(track_id=incoming.id,artwork=incoming.cover,state='playing' if st.get('is_playing') else 'paused',spotify_phase='confirmed',context_uri=requested_context.get('context_uri',''));self.resume_position=float(st.get('progress') or 0)
     self.save_state();self._state_changed('Metadata','PlaybackStatus');log.info('spotify reconcile confirmed request=%s track_gen=%s track=%s context_index=%s context_len=%s',request,track_generation,incoming.id,self.context_index,len(self.play_context))
     if ui:ui.post(lambda:(ui.status('Playing · '+incoming.title),ui.invalidate()))
     self.spotify_polling=False;self.spotify_poll_start(ui);return
    except Exception:log.exception('Spotify reconciliation failed')
    time.sleep(1.0)
   log.info('spotify reconcile ended stale request=%s/%s track_gen=%s/%s requested=%s',request,self.playback_request,track_generation,self.spotify_track_generation,requested_id)
  threading.Thread(target=run,daemon=True).start()
 def spotify_poll_start(self,ui=None):
  if self.spotify_pending_track_id:
   log.debug('spotify poll deferred pending=%s',self.spotify_pending_track_id);return
  generation=self.playback_generation;track_generation=self.spotify_track_generation
  if self.spotify_polling and self.playback_state.get('source')=='spotify':return
  self.spotify_polling=True
  log.info('spotify poll start playback_gen=%s track_gen=%s current=%s',generation,track_generation,getattr(self.current,'id',''))
  def run():
   while self.spotify_polling and generation==self.playback_generation and track_generation==self.spotify_track_generation and self.current and self.current.source=='spotify':
    try:
     old_playing=bool(self.spotify_state.get('is_playing'));old_index=self.context_index;old_id=self.current.id if self.current else None
     st=self.spotify.state();st['_seen']=time.monotonic();observed=getattr(st.get('track'),'id','')
     if generation!=self.playback_generation or track_generation!=self.spotify_track_generation:
      log.debug('spotify poll ignored stale playback_gen=%s/%s track_gen=%s/%s observed=%s',generation,self.playback_generation,track_generation,self.spotify_track_generation,observed);break
     self.spotify_state=st
     if st.get('track'):
      incoming=st['track'];spotify_ctx=str((st.get('context') or {}).get('uri') or '')
      active_ctx=str(self.playback_state.get('context_uri') or '')
      hit=next((i for i,x in enumerate(self.play_context) if x.source=='spotify' and x.id==incoming.id),None)
      if hit is not None:incoming.meta={**(incoming.meta or {}),**{k:v for k,v in (self.play_context[hit].meta or {}).items() if k.startswith('context_')}}
      # A native album/playlist advance is valid only inside the context owned by
      # this session. An external Spotify change is accepted, but starts a fresh
      # single-track NPLAY context instead of inheriting stale Up Next entries.
      if incoming.id!=old_id and (hit is None or (active_ctx and spotify_ctx and active_ctx!=spotify_ctx)):
       log.info('spotify external/context change old=%s new=%s spotify_context=%s active_context=%s · reset NPLAY context',old_id,incoming.id,spotify_ctx,active_ctx)
       self.play_context=[incoming];self.original_context=[incoming];self.context_index=0;self.playback_state['context_uri']=''
      elif hit is not None:self.context_index=hit
      self.current=incoming;self.playback_state.update(track_id=incoming.id,artwork=incoming.cover,state='playing' if st.get('is_playing') else 'paused');self.resume_position=st.get('progress',0);self.volume=float((st.get('device') or {}).get('volume_percent') or self.volume)
      if old_id and old_id!=incoming.id:
       log.info('spotify track commit playback_gen=%s track_gen=%s old=%s new=%s context_index=%s context_len=%s',generation,track_generation,old_id,incoming.id,self.context_index,len(self.play_context))
       self._finish_play_event(True)
       if self.sleep_mode=='track' or (self.sleep_mode=='album' and self.play_context and old_index>=len(self.play_context)-1):
        self.sleep_mode='off';self.stop_playback(ui);break
       if self.queue:
        nxt=self.queue.pop(0);self.save_state();ui and ui.post(lambda nxt=nxt:self.play(nxt,ui,preserve_context=True));break
       self._start_play_event(incoming);self.db.history_add(incoming);self.save_state();self.notifier.track_changed(incoming);self._state_changed('Metadata','PlaybackStatus')
     if old_playing!=bool(st.get('is_playing')):self._state_changed('PlaybackStatus')
     if ui:ui.post(ui.invalidate)
    except Exception:log.exception('Spotify poll failed')
    time.sleep(1.0)
   if generation==self.playback_generation and track_generation==self.spotify_track_generation:self.spotify_polling=False
  threading.Thread(target=run,daemon=True).start()
 def _finish_play_event(self,completed=False):
  if self.play_event_id:
   try:self.db.play_finished(self.play_event_id,max(0,self.play_event_listened),completed)
   except Exception:pass
  self.play_event_id=None;self.play_event_started=0.0;self.play_event_listened=0.0;self.play_event_tick=0.0
 def _start_play_event(self,t):
  self._finish_play_event(False)
  try:self.play_event_id=self.db.play_started(t);self.play_event_started=time.monotonic();self.play_event_tick=self.play_event_started;self.play_event_listened=0.0
  except Exception:self.play_event_id=None
 def stop_playback(self,ui=None,reason='stopped'):
  self.statistics_tracker.flush(self.playback_active())
  # Stop also cancels any in-flight asynchronous source handoff.
  self.playback_request+=1;self._finish_play_event(False);self.spotify_polling=False;self.player.stop()
  if self.current and self.current.source=='spotify':
   self._spotify_control(ui,'stopped',lambda:self._spotify_with_device(lambda did:self.spotify.pause(did)),lambda _:self.spotify_state.update(is_playing=False))
  self.playback_state['state']='stopped';self.mpris.changed('PlaybackStatus','Metadata')
 def stop_and_clear(self,ui=None):
  # User-facing Stop: unlike pause, this intentionally clears the playback session.
  self.stop_playback(ui);self.radio_poll_generation+=1;self.current=None;self.resume_candidate=False;self.resume_position=0.0;self.play_context=[];self.original_context=[];self.context_index=-1;self.spotify_state={};self.playback_state={};self.state.clear();self.events.emit('playback-state-changed',state=self.state);self.save_state();self._state_changed('PlaybackStatus','Metadata');ui and ui.status('Stopped · Now Playing cleared');ui and ui.invalidate()
 def _state_changed(self,*props):
  self.events.emit('integration-state-changed',properties=props or ('PlaybackStatus','Metadata','Volume'))
  try:self.mpris.changed(*(props or ('PlaybackStatus','Metadata','Volume')))
  except Exception:pass
 def playback_active(self):
  if self.current and self.current.source=='spotify':return bool(self.spotify_state.get('is_playing'))
  return self.player.active()
 def playback_position(self):
  if self.current and self.current.source=='spotify':
   base=float(self.spotify_state['progress'] if 'progress' in self.spotify_state else (self.resume_position or 0));seen=float(self.spotify_state.get('_seen') or time.monotonic());return min(self.playback_duration() or 1e12,base+(max(0,time.monotonic()-seen) if self.spotify_state.get('is_playing') else 0))
  return float(self.player.prop('time-pos') or self.resume_position or 0)
 def playback_duration(self):
  if self.current and self.current.source=='spotify':return float(self.spotify_state.get('duration') or self.current.duration or 0)
  return float(self.player.prop('duration') or (self.current.duration if self.current else 0) or 0)
 def playback_seek_relative(self,d):
  if self.current and self.current.source=='spotify':
   target=max(0,self.playback_position()+d);self._spotify_control(self.ui,'seek',lambda:self._spotify_with_device(lambda did:self.spotify.seek(target,did)));self.spotify_state['progress']=target
  else:self.player.seek(d)
  self._state_changed('Position')
 def playback_volume(self,d):
  if self.current and self.current.source=='spotify':
   self.volume=max(0,min(100,self.volume+d));self._spotify_control(self.ui,'volume',lambda:self._spotify_with_device(lambda did:self.spotify.volume(self.volume,did)))
  else:self.player.volume(d);self.volume=max(0,min(150,self.volume+d))
  self._state_changed('Volume')
 def playback_mute(self):
  if self.current and self.current.source=='spotify':
   self.volume=0;self._spotify_control(self.ui,'muted',lambda:self._spotify_with_device(lambda did:self.spotify.volume(0,did)))
  else:self.player.mute()
 def nav(self):
  if not self.cfg.getbool('navidrome_enabled',True):return None
  u=self.cfg.get('navidrome_url');user=self.cfg.get('navidrome_user');pw=self.cfg.nav_password();return Navidrome(u,user,pw,int(self.cfg.get('navidrome_bitrate','0') or 0)) if u and user and pw else None
 def nav_configured(self):
  return bool(self.cfg.get('navidrome_url') and self.cfg.get('navidrome_user') and self.cfg.nav_password())
 def hydrate_resume_artwork(self,ui=None):
  t=self.current
  if not t or t.source!='navidrome' or t.cover:return
  def run():
   try:
    n=self.nav()
    if not n:return
    fresh=n.song(t.id);t.cover=fresh.cover or t.cover;t.meta={**(t.meta or {}),**(fresh.meta or {})}
    if fresh.cover:
     try:
      import urllib.parse
      cid=urllib.parse.parse_qs(urllib.parse.urlparse(fresh.cover).query).get('id',[''])[0]
      if cid:t.meta['cover_id']=cid
     except Exception:pass
     self.save_state()
     if ui:ui.post(lambda:(ui.art.prefetch(t.cover),ui.invalidate()))
   except Exception:pass
  threading.Thread(target=run,daemon=True).start()
 def radio_poll_start(self,t,ui=None):
  self.radio_poll_generation+=1;gen=self.radio_poll_generation
  def run():
   last=''
   while gen==self.radio_poll_generation and self.current and self.current.kind=='radio' and self.current.id==t.id:
    try:
     if t.source=='sr':info=self.sr.live_info(t.id)
     else:
      md=self.player.prop('metadata') or {};media=str(self.player.prop('media-title') or '').strip();artist=str(md.get('Artist') or md.get('artist') or '').strip();title=str(md.get('Title') or md.get('title') or media).strip();song=' – '.join(z for z in (artist,title) if z) if artist and title and artist.casefold() not in title.casefold() else title;info={'program':'','episode':'','song':song}
     key='|'.join(str(info.get(k,'')) for k in ('program','episode','song','next_program'))
     if info:self.radio_info[t.id]=info
     if key and key!=last:
      if last:self.notifier.track_changed(t,force=True,radio_info=info)
      last=key;self._state_changed('Metadata');ui and ui.post(ui.invalidate)
    except Exception:log.exception('Radio metadata poll failed')
    time.sleep(45 if t.source=='sr' else 15)
  threading.Thread(target=run,daemon=True).start()
 def async_run(self,ui,work,ok,fail='Unavailable'):
  def run():
   try:r=work();ui.post(lambda r=r:ok(r))
   except Exception as e:err=str(e);ui.post(lambda err=err:ui.error(fail,err))
  threading.Thread(target=run,daemon=True).start()
 def test_and_save_nav(self,url,user,pw,ui):
  def work():n=Navidrome(url,user,pw);n.ping();self.cfg.save_nav(url,user,pw);return True
  self.async_run(ui,work,lambda _:(ui.status('Navidrome connected'),self.open_nav_home(ui)),'Connection failed · nothing saved')
 def _track_key(self,t):return f"{getattr(t,'source','')}:{getattr(t,'id','')}" if t else ''
 def set_play_context(self,tracks,index):
  xs=list(tracks or []);self.original_context=list(xs)
  if not xs:self.play_context=[];self.context_index=-1;return
  index=max(0,min(index,len(xs)-1));chosen=xs[index]
  self.play_context=list(xs);self.context_index=index
  if self.shuffle_mode!='off':self.apply_shuffle_to_context(chosen)
 def normalization_mode(self,t=None):
  if not self.cfg.getbool('normalization',True):return 'no'
  mode=self.cfg.get('normalization_mode','auto').lower()
  if mode in ('track','album'):return mode
  # Auto preserves album dynamics only when the active context is one album.
  albums={x.album for x in self.play_context if getattr(x,'album','')}
  return 'album' if t and t.album and len(albums)==1 and len(self.play_context)>1 else 'track'
 def cycle_repeat(self,ui=None):
  modes=['off','track','context'];self.repeat_mode=modes[(modes.index(self.repeat_mode) if self.repeat_mode in modes else 0)+1 if (modes.index(self.repeat_mode) if self.repeat_mode in modes else 0)<2 else 0];self.cfg.set('repeat_mode',self.repeat_mode);self.save_state();self._state_changed('LoopStatus');ui and ui.status('Repeat · '+self.repeat_mode.capitalize())
 def cycle_shuffle(self,ui=None):
  modes=['off','shuffle','smart'];i=modes.index(self.shuffle_mode) if self.shuffle_mode in modes else 0;self.shuffle_mode=modes[(i+1)%len(modes)];self.cfg.set('shuffle_mode',self.shuffle_mode)
  if self.shuffle_mode=='off' and self.original_context:
   cur=self.current;self.play_context=list(self.original_context);self.context_index=next((i for i,x in enumerate(self.play_context) if cur and self._track_key(x)==self._track_key(cur)),0)
  elif self.shuffle_mode!='off':self.apply_shuffle_to_context(self.current)
  self.save_state();self._state_changed('Shuffle');ui and ui.status('Shuffle · '+self.shuffle_mode.capitalize())
 def _ordered_context(self,chosen=None):
  xs=list(self.original_context or self.play_context);chosen=chosen or self.current
  if chosen:
   xs=[x for x in xs if self._track_key(x)!=self._track_key(chosen)]
  import random;random.shuffle(xs)
  if self.shuffle_mode=='smart':
   recent={self._track_key(x) for x in self.db.history()[:25]};xs.sort(key=lambda x:(self._track_key(x) in recent,))
   out=[]
   while xs:
    last=(out[-1].artist if out else (chosen.artist if chosen else '') or '').casefold();j=next((i for i,x in enumerate(xs) if (x.artist or '').casefold()!=last),0);out.append(xs.pop(j))
   xs=out
  return ([chosen]+xs) if chosen else xs
 def apply_shuffle_to_context(self,chosen=None):
  if not (self.original_context or self.play_context) or self.shuffle_mode=='off':return
  self.play_context=self._ordered_context(chosen);self.context_index=0 if chosen else -1
 def up_next(self):
  if self.queue:return self.queue[0]
  if not self.play_context:return None
  if self.repeat_mode=='track' and self.current:return self.current
  if self.context_index+1<len(self.play_context):return self.play_context[self.context_index+1]
  return self.play_context[0] if self.repeat_mode=='context' and self.play_context else None
 def _on_track_end(self):
  self._finish_play_event(True)
  if self.sleep_mode=='track':
   self.sleep_mode='off';self.stop_playback(self.ui);self.ui and self.ui.post(lambda:self.ui.status('Sleep · stopped at end of track'));return
  if self.sleep_mode=='album' and self.play_context and self.context_index>=len(self.play_context)-1:
   self.sleep_mode='off';self.stop_playback(self.ui);self.ui and self.ui.post(lambda:self.ui.status('Sleep · stopped at end of album/context'));return
  if self.ui:self.ui.post(lambda:self.next_track(self.ui,automatic=True))
 def next_track(self,ui=None,automatic=False):
  if automatic and self.repeat_mode=='track' and self.current:return self.play(self.current,ui,preserve_context=True)
  if self.queue:
   t=self.queue.pop(0);self.save_state();return self.play(t,ui,preserve_context=True)
  if self.play_context and self.context_index+1<len(self.play_context):
   self.context_index+=1;return self.play(self.play_context[self.context_index],ui,preserve_context=True)
  if self.play_context and self.repeat_mode=='context':
   self.context_index=0;return self.play(self.play_context[0],ui,preserve_context=True)
  if self.later_queue:
   t=self.later_queue.pop(0);self.save_state();self.play_context=[];self.original_context=[];self.context_index=-1;return self.play(t,ui,preserve_context=True)
  if ui:ui.status('End of playback context')
 def previous_track(self,ui=None):
  pos=self.playback_position()
  if pos>3:
   if self.current and self.current.source=='spotify':self._spotify_control(ui,'restarted',lambda:self._spotify_with_device(lambda did:self.spotify.seek(0,did)));self.spotify_state['progress']=0
   else:self.player.seek(-pos)
   ui and ui.status('Restarted current track');return
  if self.back_stack:
   t=self.back_stack.pop();return self.play(t,ui,preserve_context=True,_from_history=True)
  if self.play_context and self.context_index>0:
   self.context_index-=1;return self.play(self.play_context[self.context_index],ui,preserve_context=True,_from_history=True)
  ui and ui.status('Start of playback context')
 def play(self,t,ui=None,preserve_context=False,resume_pos=0,_from_history=False):
  if not t:return
  if getattr(t,'kind','')=='radio':self.play_context=[];self.original_context=[];self.context_index=-1;preserve_context=False
  old_current=self.current;old_state=dict(self.playback_state);old_generation=self.playback_generation
  if old_current and not _from_history and self._track_key(old_current)!=self._track_key(t):
   self.back_stack.append(old_current);self.back_stack=self.back_stack[-100:]
  try:
   was_spotify=bool(self.current and self.current.source=='spotify')
   self.playback_request+=1;request=self.playback_request
   if t.source=='spotify':
    # Track-level ownership is separate from transport ownership: Spotify's Web API
    # can briefly report the previous track after a new play request.
    self.spotify_track_generation+=1;spotify_track_generation=self.spotify_track_generation;self.spotify_polling=False
    requested_context=self._spotify_context_snapshot(t,preserve_context)
    log.info('spotify play request request=%s track_gen=%s track=%s context_len=%s context_uri=%s',request,spotify_track_generation,t.id,len(requested_context['tracks']),requested_context['context_uri'])
    # Spotify can play locally through NPLAY-managed librespot or on an external
    # Connect device. Do not replace current playback until Spotify confirms start.
    # An explicit user choice owns presentation immediately after Spotify accepts
    # the play command. A restored paused session is only a resume candidate.
    previous_resume_candidate=self.resume_candidate;self.resume_candidate=False;self.spotify_pending_track_id=t.id
    local=self.cfg.get('spotify_playback','local')=='local'
    if ui:ui.status('Spotify · starting '+('locally…' if local else 'on Connect device…'))
    def work():
     if local:
      # Normal playback never opens OAuth. Let librespot reuse whatever
      # authentication it can actually load; cache-file discovery is diagnostic,
      # not an authoritative authorization gate.
      name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY';d=self.spotify.ensure_local_device(name,20);did=d['id'];self.spotify.activate_device(did)
      last_error=None
      for delay in (0,.35,.75,1.25):
       if delay:time.sleep(delay)
       try:self.spotify.play(t,device_id=did);last_error=None;break
       except RuntimeError as e:
        last_error=e
        if 'No active device' not in str(e):raise
        self.spotify.transfer(did,False)
      if last_error:raise RuntimeError('Local Spotify device registered but did not accept playback · '+str(last_error))
      if resume_pos and t.seekable:self.spotify.seek(resume_pos,did)
      self.cfg.set('spotify_device_name',name)
      try:d=next((x for x in self.spotify.devices() if x.get('id')==did),d)
      except Exception:pass
      return d
     d=self.spotify.play_track(t)
     if resume_pos and t.seekable:self.spotify.seek(resume_pos,d.get('id'))
     return d
    def started(device):
     if request!=self.playback_request or spotify_track_generation!=self.spotify_track_generation:
      log.info('spotify start ignored stale request=%s/%s track_gen=%s/%s track=%s',request,self.playback_request,spotify_track_generation,self.spotify_track_generation,t.id);return
     # The Spotify play command succeeded. Commit the user's requested metadata as
     # PENDING now; do not wait for eventually-consistent /me/player metadata.
     self._begin_playback_session(t,'spotify');self.player.stop();self._apply_spotify_context(requested_context,t);self._commit_playback_session(t,'spotify');self.playback_state.update(context_uri=requested_context.get('context_uri',''),spotify_phase='pending');self.spotify_pending_track_id=t.id;self.resume_position=float(resume_pos or 0)
     self.spotify_state={'track':t,'is_playing':True,'progress':float(resume_pos or 0),'duration':float(t.duration or 0),'device':device,'_seen':time.monotonic(),'pending':True}
     self.db.history_add(t);self.save_state();self.notifier.track_changed(t);self.ui=ui or self.ui
     log.info('spotify pending commit playback_gen=%s request=%s track_gen=%s track=%s context_index=%s context_len=%s',self.playback_generation,request,spotify_track_generation,t.id,self.context_index,len(self.play_context))
     if ui:
      ui.now_playing();ui.status('Spotify · playing · syncing metadata…')
      # Web API reconciliation may lag behind audible librespot playback for a long
      # time after restart. Keep that implementation detail transient in the UI;
      # reconciliation continues in the background and remains visible in the log.
      def clear_sync_hint():
       time.sleep(2.5)
       if request==self.playback_request and spotify_track_generation==self.spotify_track_generation and self.current and self.current.id==t.id:
        ui.post(lambda:(ui.status('Playing · '+t.title),ui.invalidate()))
      threading.Thread(target=clear_sync_hint,daemon=True).start()
     self._spotify_reconcile_start(ui,request,spotify_track_generation,t,requested_context,str(device.get('id') or ''))
    def failed(err):
     # Only a real play/device/auth failure reaches here. Never label a stale
     # /me/player observation as playback unavailable.
     if request==self.playback_request and spotify_track_generation==self.spotify_track_generation:
      self.spotify_pending_track_id='';self.resume_candidate=previous_resume_candidate
     ui.error('Spotify playback unavailable',err)
    if ui:
     def run_spotify():
      try:r=work();ui.post(lambda r=r:started(r))
      except Exception as e:err=str(e);log.exception('Spotify play request failed');ui.post(lambda err=err:failed(err))
     threading.Thread(target=run_spotify,daemon=True).start()
    else:
     device=work();started(device)
    return
   # mpv-backed sources resolve and wait for file-loaded off the curses thread.
   # This prevents slow disks/network extractors from freezing navigation and keeps
   # the previous authoritative Now Playing state until the new media is ready.
   if ui:
    self.mpv_start.start(t,ui,request,resume_pos,was_spotify);return
   prepared,url,_started=self.mpv_start._prepare(t)
   t=prepared;rg=self.normalization_mode(t);self.player.start(url,replaygain=rg,preamp=float(self.cfg.get('replaygain_preamp','0') or 0),clip=self.cfg.getbool('replaygain_clip',True),gapless=self.cfg.getbool('gapless',False),ytdl_path=(self.yt.binary if t.source=='youtube' else None))
   self._begin_playback_session(t,'mpv')
   if was_spotify:self._stop_spotify_session()
   self.player.cmd(['set_property','pause',False]);self._commit_playback_session(t,'mpv');self.resume_candidate=False;self.resume_position=0.0;self.player.set_volume(self.volume)
   if resume_pos and t.seekable:self.player.set_position(resume_pos);self.resume_position=float(resume_pos)
   self.db.history_add(t);self.save_state()
   if not (t.kind=='radio' and t.source=='sr'):self.notifier.track_changed(t)
   if t.kind=='radio':
    if t.source=='sr':self.refresh_radio_async([t],ui)
    self.radio_poll_start(t,ui)
  except Exception as e:
   if self.playback_state.get('state')=='starting':
    self.current=old_current;self.playback_state=old_state;self.playback_generation=old_generation
    if old_current and old_current.source=='spotify':self.spotify_poll_start(ui)
   if ui:ui.status(f'Playback failed · {e}')
 def queue_clear(self,ui=None):
  self.queue=[];self.save_state();ui and ui.replace_list('QUEUE',[],empty='Queue is empty',message='Queue cleared')
 def queue_save_playlist(self,ui=None):
  if not self.queue:return ui and ui.status('Queue is empty')
  name=ui.prompt('SAVE QUEUE AS PLAYLIST › ') if ui else ''
  if name:
   pid=self.db.playlist_create(name)
   for t in self.queue:self.db.playlist_add(pid,t)
   ui.status(f'Saved queue · {name} · {len(self.queue)} tracks')
 def set_rating(self,t,rating,ui=None):
  if not t:return
  self.db.rating_set(t.id,rating,t.source);ui and ui.status(f'Rating · {rating}/5 · {t.title}')
 def save_discovery(self,ui=None):
  t=self.current
  if not t:return ui and ui.status('Discovery · nothing playing')
  info=self.radio_info.get(t.id,{}) if t.kind=='radio' else {}
  raw=(info.get('song') or info.get('title') or '').strip();artist=(info.get('artist') or '').strip();title=raw or t.title
  if ' - ' in title and not artist:artist,title=title.split(' - ',1)
  ok=self.db.discovery_add(title,artist,t.source,{'station':t.title if t.kind=='radio' else ''});ui and ui.status(('Saved discovery · '+((' · '.join(x for x in (artist,title) if x)))) if ok else 'Discovery unavailable')
 def stats_time(self,seconds):
  seconds=max(0,int(seconds))
  h,rem=divmod(seconds,3600);m,sec=divmod(rem,60)
  return f'{h:,}h {m:02d}m' if h else f'{m}m {sec:02d}s'
 def stats_home(self,ui,period='week',push=True):
  """Small overview: every number remains visible without selection."""
  self._stats_period=period
  periods=[('TODAY','day'),('WEEK','week'),('MONTH','month'),('YEAR','year'),('ALL TIME','all')]
  if not self.cfg.getbool('statistics_enabled',True):
   ui.show_menu('STATISTICS',[
    ('STATISTICS · OFF','','Listening history is preserved'),
    ('ENABLE LOCAL STATISTICS','stats:toggle','No data is uploaded'),
    ('SETTINGS & SYNC','stats:settings','Local privacy and installation identity')],push=push)
   return
  st=self.statistics.summary(period)
  total=self.stats_time(st['seconds'])
  items=[('ACTUAL LISTENING · '+total,'',f"{st['sessions']} sessions · {st['days']} active days"),
   ('PERIOD · '+period.upper(),'','Select a period below')]
  items.extend([(('● ' if period==key else '  ')+label,'stats:period:'+key,'') for label,key in periods])
  items.extend([('','', ''),
   ('TOP ARTISTS','stats:artists','Open complete ranking')])
  items.extend([(f'{i}. {name} · {self.stats_time(sec)}','','') for i,(name,sec,plays) in enumerate(st['artists'][:3],1)])
  items.append(('TOP ALBUMS','stats:albums','Open complete ranking'))
  items.extend([(f'{i}. {album} · {self.stats_time(sec)}','','') for i,(artist,album,sec,plays) in enumerate(st['albums'][:3],1)])
  items.append(('TOP TRACKS','stats:tracks','Open complete ranking'))
  items.extend([(f'{i}. {title} · {self.stats_time(sec)}','','') for i,(artist,title,sec,plays) in enumerate(st['tracks'][:3],1)])
  items.extend([('SOURCES','stats:sources','Listening time by source'),
   ('SETTINGS & SYNC','stats:settings','Collection · privacy · offline outbox')])
  ui.show_menu('STATISTICS · '+period.upper(),items,push=push)
 def stats_details(self,ui,section,period=None,push=True):
  period=period or getattr(self,'_stats_period','week')
  self._stats_period=period
  st=self.statistics.summary(period)
  names={'artists':'ARTISTS','albums':'ALBUMS','tracks':'TRACKS','sources':'SOURCES'}
  if section not in names:return
  items=[('LISTENED · '+self.stats_time(st['seconds']),'',''),
         ('RANKED BY ACTUAL LISTENING TIME','','')]
  rows=st[section]
  for i,row in enumerate(rows,1):
   if section=='albums':
    artist,album,seconds,plays=row
    label=f'{i:2d}  {album} · {self.stats_time(seconds)}'
    detail=f'{artist} · {plays} sessions'
   elif section=='tracks':
    artist,title,seconds,plays=row
    label=f'{i:2d}  {title} · {self.stats_time(seconds)}'
    detail=f'{artist} · {plays} sessions'
   else:
    name,seconds,plays=row
    label=f'{i:2d}  {name} · {self.stats_time(seconds)}'
    detail=f'{plays} sessions'
   items.append((label,'',detail))
  if not rows:items.append(('NO LISTENING DATA YET','','Play some music to build rankings'))
  items.append(('BACK TO OVERVIEW','stats:overview','Return to statistics'))
  ui.show_menu('STATISTICS · '+names[section]+' · '+period.upper(),items,push=push)
 def stats_settings(self,ui,push=True):
  enabled=self.cfg.getbool('statistics_enabled',True)
  sync=self.statistics_sync
  online=sync.enabled()
  pending=self.statistics.summary('all')['pending']
  last=time.strftime('%Y-%m-%d %H:%M',time.localtime(sync.last_success)) if sync.last_success else 'NEVER'
  ui.show_menu('STATISTICS · SETTINGS & SYNC',[
   ('LOCAL COLLECTION · '+('ON' if enabled else 'OFF'),'stats:toggle','Toggle local listening statistics'),
   ('PRIVACY · '+('SYNC ENABLED' if online else 'OFFLINE ONLY'),'','Only listening metadata is sent when enabled'),
   ('SYNC · '+(sync.status if online else 'DISABLED'),'','Background HTTPS only'),
   ('SERVER · '+sync.url(),'','HTTPS API endpoint'),
   (f'OUTBOX · {pending} PENDING','','Existing events are never deleted'),
   ('LAST SUCCESS · '+last,'','Most recent successful batch in this session'),
   ('LAST ERROR · '+(sync.last_error or 'NONE'),'','No credentials in diagnostics'),
   ('MANUAL SYNC NOW','stats:sync-now','Send up to 50 pending events in background'),
   ('SYNC · '+('DISABLE' if online else 'ENABLE'),'stats:sync-toggle','Explicit opt-in · requires dedicated token'),
   ('INSTALLATION ID','','Unique per NPLAY data directory'),
   (self.statistics.installation_id(),'','Copy using the action below'),
   ('COPY INSTALLATION ID','stats:copy-id','Copy identifier to clipboard if supported'),
   ('BACK TO OVERVIEW','stats:overview','Return to statistics')],push=push)
 def statistics_sync_toggle(self,ui):
  new=not self.statistics_sync.enabled()
  if new and not self.cfg.statistics_token():
   ui.status('Configure a dedicated token first: nplay --stats-set-token');return
  self.cfg.set('statistics_sync_enabled','true' if new else 'false')
  self.statistics_sync.status='IDLE' if new else 'DISABLED'
  self.stats_settings(ui,push=False)
 def statistics_sync_now(self,ui):
  if not self.statistics_sync.enabled():ui.status('Enable sync first in Settings & Sync');return
  started=self.statistics_sync.run_async(manual=True,callback=lambda:ui.post(lambda:self.refresh_statistics_view(ui)))
  ui.status('Statistics syncing in background' if started else 'Sync already in progress')
  self.stats_settings(ui,push=False)
 def refresh_statistics_view(self,ui):
  """Refresh only an open statistics menu, retaining the user's selection.

  Never touch playback state or navigation history during a refresh.
  """
  if ui.mode!='menu' or not ui.title.startswith('STATISTICS'):return False
  title=ui.title
  previous=ui.sel
  action=ui.items[previous][1] if 0<=previous<len(ui.items) and isinstance(ui.items[previous],tuple) else None
  if title=='STATISTICS · SETTINGS & SYNC':self.stats_settings(ui,push=False)
  elif title.startswith('STATISTICS · ') and ' · ' in title[len('STATISTICS · '):]:
   parts=title.split(' · ')
   section={'ARTISTS':'artists','ALBUMS':'albums','TRACKS':'tracks','SOURCES':'sources'}.get(parts[1])
   if not section:return False
   self.stats_details(ui,section,push=False)
  elif title=='STATISTICS' or title.startswith('STATISTICS · '):
   self.stats_home(ui,getattr(self,'_stats_period','week'),push=False)
  else:return False
  if ui.items:
   # Prefer the same navigation action, otherwise retain row position.
   new_index=next((i for i,item in enumerate(ui.items) if action and isinstance(item,tuple) and item[1]==action),None)
   if new_index is None:new_index=min(previous,len(ui.items)-1)
   if ui.selectable(new_index):ui.sel=new_index
   else:
    ui.sel=new_index
    ui.edge_selection(False)
   ui.invalidate()
  return True
 def statistics_toggle(self,ui):
  enabled=not self.cfg.getbool('statistics_enabled',True)
  self.cfg.set('statistics_enabled','true' if enabled else 'false')
  self.statistics_tracker.enabled=enabled
  self.statistics_tracker.flush(self.playback_active())
  self.stats_settings(ui,push=False)
  ui.status('Local statistics '+('enabled' if enabled else 'disabled')+' · sync disabled')
 def smart_home(self,ui):
  ui.show_menu('SMART PLAYLISTS',[('RECENTLY ADDED','smart:recent','Newest local files'),('MOST PLAYED','smart:most','Based on NPLAY history'),('NEVER PLAYED','smart:never','Local tracks not in history'),('UNPLAYED ALBUMS','smart:albums','Albums with no played tracks'),('HIGHLY RATED','smart:rated','4–5 stars'),('RANDOM 50','local:random:50','Fresh local selection')])
 def sleep_set(self,arg,ui=None):
  a=(arg or '').strip().lower()
  if a in ('off','0'):
   self.sleep_deadline=0;self.sleep_mode='off';ui and ui.status('Sleep timer · off');return
  if a in ('track','album'):
   self.sleep_mode=a;self.sleep_deadline=0;ui and ui.status('Sleep · end of '+a);return
  try:mins=max(1,int(a));self.sleep_deadline=time.time()+mins*60;self.sleep_mode='time';ui and ui.status(f'Sleep · {mins} minutes')
  except ValueError:ui and ui.status('Sleep · minutes | track | album | off')
 def sleep_check(self):
  if self.sleep_mode=='time' and self.sleep_deadline and time.time()>=self.sleep_deadline:
   self.stop_playback(self.ui);self.sleep_deadline=0;self.sleep_mode='off';return True
  return False

 def command(self,q,ui):
  p=q.split(maxsplit=1);cmd=p[0].lower() if p else '';arg=p[1] if len(p)>1 else ''
  if cmd in ('about',):ui.show_about()
  elif cmd in ('browse','b'):self.root_browse(ui)
  elif cmd=='local':self.local_home(ui)
  elif cmd=='queue':self.open_source('queue',ui)
  elif cmd=='theme':ui.set_theme(arg) if arg else ui.theme_menu()
  elif cmd=='settings':self.open_source('settings',ui)
  elif cmd in ('nav','navidrome'):self.search_provider('nav',arg,ui) if arg else self.open_nav_home(ui)
  elif cmd=='spotify':self.spotify_search(arg,ui) if arg else self.open_spotify_home(ui)
  elif cmd in ('sr',):self.search_sr_all(arg,ui) if arg else self.open_source('srpod',ui)
  elif cmd in ('yt','youtube'):self.search_provider('yt',arg or ui.prompt('YOUTUBE SEARCH › '),ui)
  elif cmd=='radio':
   xs=self.sr.live();m=next((x for x in xs if arg.lower() in x.title.lower()),xs[0]);self.play(m,ui)
  elif cmd=='now':ui.now_playing()
  elif cmd=='stop':self.stop_and_clear(ui)
  elif cmd=='bookmarks':self.open_source('bookmarks',ui)
  elif cmd in ('next','n'):self.next_track(ui)
  elif cmd in ('previous','prev','p'):self.previous_track(ui)
  elif cmd=='view':
   if arg in ('normal','artwork','visualizer'):ui.layout=arg;self.cfg.set('now_playing_view',arg);ui.status('View · '+arg.capitalize());ui.invalidate()
   else:ui.status('View · normal | artwork | visualizer')
  elif cmd=='normalize':
   if arg in ('on','off'):self.cfg.set('normalization','true' if arg=='on' else 'false');ui.status('Normalization · '+arg)
   elif arg in ('auto','track','album'):self.cfg.set('normalization_mode',arg);ui.status('Normalization mode · '+arg)
   else:ui.status('Normalize · on | off | auto | track | album')
  elif cmd=='gapless':
   if arg in ('on','off'):self.cfg.set('gapless','true' if arg=='on' else 'false');ui.status('Gapless · '+arg+' · applies from next track')
   else:ui.status('Gapless · on | off')
  elif cmd=='shuffle':self.cycle_shuffle(ui)
  elif cmd=='repeat':self.cycle_repeat(ui)
  elif cmd=='sleep':self.sleep_set(arg,ui)
  elif cmd=='stats':self.stats_home(ui)
  elif cmd=='smart':self.smart_home(ui)
  elif cmd=='discover':self.save_discovery(ui)
  elif cmd=='rating':
   try:self.set_rating(self.current,int(arg),ui)
   except ValueError:ui.status('Rating · 0–5')
  elif cmd=='queue-clear':self.queue_clear(ui)
  elif cmd=='queue-save':self.queue_save_playlist(ui)
  elif cmd=='diagnostics':ui.show_diagnostics()
  elif cmd=='help':ui.push();ui.mode='help';ui._view_changed()
  elif cmd=='scan':
   r=self.scan();ui.status(f"Scan · {r['checked']} checked · {r['new']} new · {r['updated']} updated · {r['removed']} removed · {r['total']} indexed")
  else:ui.status('Unknown command · use :help · commands: :stop · :bookmarks · :now · :browse · :theme · :settings · :nav QUERY · :spotify QUERY · :sr QUERY · :yt QUERY · :radio p1 · :view normal|artwork|visualizer · :normalize · :gapless · :shuffle · :repeat · :sleep 30|track|album|off · :smart · :stats · :discover · :rating 0-5 · :queue-save · :queue-clear · :diagnostics · :about · :scan')
def doctor(a=None):
 a=a or App();print(f'NPLAY {__version__} by Nicklas Rudolfsson');print('python       OK');print('mpv          '+('OK' if shutil.which('mpv') else 'MISSING'));print('yt-dlp       '+((a.yt.version()+' · '+a.yt.binary) if a.yt.available() else 'optional / missing'));print('cava         '+('OK' if shutil.which('cava') else 'optional / missing'));print('mpris        '+('READY' if a.mpris.available else ('enabled / unavailable' if a.cfg.getbool('mpris_enabled',True) else 'off')));print('notifications '+(('ON · '+str(a.notifier.binary)) if a.notifier.enabled() else ('enabled / unavailable' if a.cfg.getbool('track_notifications',True) else 'off')));print('terminal     '+os.getenv('TERM','unknown'));print('kitty        '+('YES' if os.getenv('KITTY_WINDOW_ID') else 'no'));print('library      '+str(a.db.count())+' indexed tracks · schema '+str(a.db.schema_version()));print('navidrome    '+(('enabled / configured' if a.nav_configured() else 'enabled / not configured') if a.cfg.getbool('navidrome_enabled',True) else 'disabled'));print('spotify      '+(('enabled / connected' if a.spotify_configured() else 'enabled / setup required') if a.cfg.getbool('spotify_enabled',False) else 'disabled'));print('librespot     '+(('running' if a.spotify.local.running() else 'available') if a.spotify.local.available() else 'optional / missing'));print('sr           '+('enabled' if a.cfg.getbool('sr_enabled',True) else 'disabled'));print('youtube      '+('enabled' if a.cfg.getbool('youtube_enabled',True) else 'disabled'));print('custom radio '+('enabled' if a.cfg.getbool('custom_radio_enabled',True) else 'disabled'));print('music roots  '+' : '.join(a.roots()));print('config       '+str(a.cfg.path));print('log          '+str(LOG_PATH)+' · '+str(recent_error_count())+' recent warnings/errors');print('mpv log      '+str(STATE/'mpv.log'));return 0 if shutil.which('mpv') else 1
def main():
 ap=argparse.ArgumentParser(prog='nplay');ap.add_argument('query',nargs='*');ap.add_argument('--version',action='store_true');ap.add_argument('--doctor',action='store_true');ap.add_argument('--scan',action='store_true');ap.add_argument('--list-themes',action='store_true');ap.add_argument('--check-theme',metavar='FILE');ap.add_argument('--stats-set-token',action='store_true');ap.add_argument('--stats-sync-now',action='store_true');args=ap.parse_args()
 if args.version:print(f'NPLAY {__version__}');return
 if args.list_themes:
  from .theme import available
  for key,label in available():print(f'{key:20} {label}')
  return
 if args.check_theme:
  from .theme import validate_theme
  ok,msg=validate_theme(args.check_theme);print(msg)
  if not ok:raise SystemExit(1)
  return
 if args.stats_set_token:
  import getpass
  token=getpass.getpass('Dedicated statistics API token (hidden): ').strip()
  if not token:raise SystemExit('Cancelled: empty token')
  Config().save_statistics_token(token)
  print('Statistics token stored locally (0600). Enable sync separately in NPLAY Settings & Sync.')
  return
 a=App()
 if args.stats_sync_now:
  try:print('Acknowledged:',a.statistics_sync.once())
  except Exception as e:raise SystemExit('Sync failed: '+str(e))
  return
 if args.doctor:raise SystemExit(doctor(a))
 if args.scan:r=a.scan();print(f"Scan · {r['checked']} checked · {r['new']} new · {r['updated']} updated · {r['removed']} removed · {r['total']} indexed");return
 if not sys.stdin.isatty() or not sys.stdout.isatty():raise SystemExit('NPLAY needs an interactive terminal.')
 if not shutil.which('mpv'):raise SystemExit('mpv is required. Run: nplay --doctor')
 ui=UI(a)
 if args.query:a.search_async(' '.join(args.query),ui)
 try:ui.run()
 except KeyboardInterrupt:pass
 finally:a.save_state();a.stop_playback(None);a.spotify.local.stop();a.mpris.stop()
if __name__=='__main__':main()
