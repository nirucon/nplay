"""Optional Spotify Web API + Spotify Connect integration for NPLAY.

Uses Authorization Code with PKCE for the Web API. Local playback is optional and
provided by a managed librespot process; external Spotify Connect devices remain available.
"""
from __future__ import annotations
from . import __version__
import base64, hashlib, json, os, secrets, time, urllib.parse, urllib.request, urllib.error, webbrowser, subprocess, shutil, signal
from pathlib import Path
from http.server import BaseHTTPRequestHandler, HTTPServer
from .model import Track
from .config import DATA,STATE
from .logging_utils import logger
log=logger()

API='https://api.spotify.com/v1'
AUTH='https://accounts.spotify.com/authorize'
TOKEN='https://accounts.spotify.com/api/token'
SCOPES=' '.join([
 'user-read-private','user-read-playback-state','user-read-currently-playing','user-modify-playback-state',
 'user-read-recently-played','user-top-read','user-library-read','user-library-modify',
 'playlist-read-private','playlist-read-collaborative','playlist-modify-private','playlist-modify-public'
])

def _image(x):
 imgs=(x or {}).get('images') or []
 return (imgs[0].get('url') if imgs and isinstance(imgs[0],dict) else '') or ''
def _artists(x):return ', '.join(a.get('name','') for a in ((x or {}).get('artists') or []) if isinstance(a,dict))
def _track(x):
 x=x or {}; album=x.get('album') or {}; uri=x.get('uri') or ('spotify:track:'+str(x.get('id','')) if x.get('id') else '')
 return Track(source='spotify',id=str(x.get('id','')),title=x.get('name',''),artist=_artists(x),album=album.get('name',''),duration=float(x.get('duration_ms') or 0)/1000,cover=_image(album),kind='track',seekable=True,meta={'spotify_uri':uri,'spotify_url':(x.get('external_urls') or {}).get('spotify',''),'explicit':bool(x.get('explicit'))})
def _album(x):
 x=x or {};return Track(source='spotify',id=str(x.get('id','')),title=x.get('name',''),artist=_artists(x),album=x.get('name',''),cover=_image(x),kind='album',seekable=False,meta={'spotify_uri':x.get('uri',''),'spotify_url':(x.get('external_urls') or {}).get('spotify',''),'release_date':x.get('release_date','')})
def _artist(x):
 x=x or {};return Track(source='spotify',id=str(x.get('id','')),title=x.get('name',''),artist='Artist',cover=_image(x),kind='artist',seekable=False,meta={'spotify_uri':x.get('uri',''),'spotify_url':(x.get('external_urls') or {}).get('spotify','')})
def _playlist(x):
 x=x or {}; cnt=((x.get('items') or x.get('tracks') or {}).get('total',0) if isinstance(x.get('items') or x.get('tracks'),dict) else 0)
 return Track(source='spotify',id=str(x.get('id','')),title=x.get('name',''),artist=((x.get('owner') or {}).get('display_name') or 'Spotify'),cover=_image(x),kind='playlist',seekable=False,meta={'spotify_uri':x.get('uri',''),'spotify_url':(x.get('external_urls') or {}).get('spotify',''),'item_count':cnt,'owner_id':(x.get('owner') or {}).get('id','')})


class LocalSpotifyEngine:
 """Lifecycle wrapper for an optional local librespot receiver.

 NPLAY owns the process: it is started on demand, uses a private XDG data/cache
 directory, and is stopped when NPLAY exits. Authentication is handled by
 librespot's OAuth flow and cached by librespot itself.
 """
 def __init__(self,cfg):
  self.cfg=cfg;self.proc=None;self.root=DATA/'spotify-local';self.cache=self.root/'cache';self.system_cache=self.root/'system-cache';self.log_path=STATE/'librespot.log'
  for d in (self.root,self.cache,self.system_cache):d.mkdir(parents=True,exist_ok=True);os.chmod(d,0o700)
 def binary(self):return shutil.which('librespot')
 def available(self):return bool(self.binary())
 def running(self):return bool(self.proc and self.proc.poll() is None)
 def credential_files(self):
  try:return [p for base in (self.system_cache,self.cache) for p in base.rglob('*') if p.is_file() and p.stat().st_size>0]
  except Exception:return []
 def has_credentials(self):
  """Best-effort indication that librespot persisted reusable authentication."""
  return bool(self.credential_files())
 def version(self):
  b=self.binary()
  if not b:return ''
  try:return subprocess.run([b,'--version'],capture_output=True,text=True,timeout=2).stdout.strip()
  except Exception:return ''
 def start(self,oauth=False):
  if self.running():return True
  b=self.binary()
  if not b:raise RuntimeError('librespot is not installed · use Spotify → Local playback for setup')
  bitrate=self.cfg.get('spotify_local_bitrate','320') or '320';name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY'
  backend=self.cfg.get('spotify_local_backend','pulseaudio') or 'pulseaudio'
  args=[b,'--name',name,'--device-type','computer','--bitrate',bitrate,'--cache',str(self.cache),'--system-cache',str(self.system_cache),'--backend',backend,'--initial-volume',str(int(float(self.cfg.get('spotify_local_volume','70') or 70))),'--quiet']
  # First run: librespot opens its own OAuth flow and persists its credential blob.
  # Subsequent runs reuse that blob and do not need a browser.
  if oauth:
   log.info('librespot OAuth explicitly requested by setup flow')
   args.append('--enable-oauth')
  else:log.info('librespot start without OAuth · cached_files=%d',len(self.credential_files()))
  log_file=open(self.log_path,'ab',buffering=0)
  env=os.environ.copy();env.setdefault('PULSE_PROP_application.name','NPLAY · Spotify');env.setdefault('PULSE_PROP_media.role','music')
  self.proc=subprocess.Popen(args,stdin=subprocess.DEVNULL,stdout=log_file,stderr=log_file,start_new_session=True,env=env)
  time.sleep(.25)
  if self.proc.poll() is not None:
   log.error('librespot exited during startup rc=%s · log=%s',self.proc.returncode,self.log_path)
   self.proc=None
   raise RuntimeError('librespot failed to start · open Spotify → Local playback → Diagnostics')
  log.info('librespot process started pid=%s oauth=%s log=%s',self.proc.pid,bool(oauth),self.log_path)
  return True
 def stop(self):
  p=self.proc
  if not p:return
  if p.poll() is None:
   try:os.killpg(p.pid,signal.SIGTERM);p.wait(timeout=2)
   except Exception:
    try:os.killpg(p.pid,signal.SIGKILL)
    except Exception:pass
  self.proc=None
 def log_tail(self,n=12):
  try:return '\n'.join(self.log_path.read_text(errors='replace').splitlines()[-n:])
  except Exception:return ''

class Spotify:
 def __init__(self,cfg):self.cfg=cfg;self.local=LocalSpotifyEngine(cfg)
 def client_id(self):return self.cfg.get('spotify_client_id','').strip()
 def enabled(self):return self.cfg.getbool('spotify_enabled',False)
 def token_data(self):return self.cfg.spotify_tokens()
 def connected(self):return bool(self.client_id() and self.token_data().get('refresh_token'))
 def _save_tokens(self,d):
  old=self.token_data();d={**old,**d};d['expires_at']=time.time()+float(d.get('expires_in',3600))-60;self.cfg.save_spotify_tokens(d)
 def access_token(self):
  d=self.token_data()
  if d.get('access_token') and float(d.get('expires_at') or 0)>time.time():return d['access_token']
  refresh=d.get('refresh_token')
  if not refresh:raise RuntimeError('Spotify is not connected')
  body=urllib.parse.urlencode({'grant_type':'refresh_token','refresh_token':refresh,'client_id':self.client_id()}).encode()
  req=urllib.request.Request(TOKEN,data=body,headers={'Content-Type':'application/x-www-form-urlencoded','User-Agent':f'NPLAY/{__version__}'})
  try:
   with urllib.request.urlopen(req,timeout=12) as r:out=json.load(r)
  except urllib.error.HTTPError as e:raise RuntimeError('Spotify token refresh failed · '+self._http_error(e))
  self._save_tokens(out);return out['access_token']
 def _http_error(self,e):
  try:
   d=json.loads(e.read().decode());return str((d.get('error') or {}).get('message') if isinstance(d.get('error'),dict) else d.get('error_description') or d.get('error') or e.reason)
  except Exception:return str(getattr(e,'reason',e))
 def request(self,method,path,params=None,body=None,timeout=12):
  url=(path if path.startswith('http') else API+path)
  if params:url+='&' if '?' in url else '?';url+=urllib.parse.urlencode(params)
  data=json.dumps(body).encode() if body is not None else None
  req=urllib.request.Request(url,data=data,method=method,headers={'Authorization':'Bearer '+self.access_token(),'Content-Type':'application/json','User-Agent':f'NPLAY/{__version__}'})
  try:
   with urllib.request.urlopen(req,timeout=timeout) as r:
    raw=r.read()
    # Spotify Player commands commonly return 204 No Content. Some proxies/
    # stacks may leave only whitespace. A successful empty response is success,
    # never a JSON decoding error.
    if r.status==204 or not raw or not raw.strip():return None
    ctype=(r.headers.get('Content-Type') or '').lower()
    if 'json' not in ctype:return raw.decode('utf-8','replace')
    try:return json.loads(raw)
    except json.JSONDecodeError as e:raise RuntimeError(f'Spotify returned invalid JSON for {method} {path} · HTTP {r.status}') from e
  except urllib.error.HTTPError as e:
   if e.code==429:raise RuntimeError('Spotify rate limit · try again shortly')
   raise RuntimeError(f'Spotify {e.code} · {self._http_error(e)}')
 def get(self,p,params=None):return self.request('GET',p,params)
 def put(self,p,params=None,body=None):return self.request('PUT',p,params,body)
 def post(self,p,params=None,body=None):return self.request('POST',p,params,body)
 def profile(self):return self.get('/me')
 def connect(self,open_browser=True,timeout=180):
  cid=self.client_id()
  if not cid:raise RuntimeError('Spotify Client ID is not configured')
  verifier=base64.urlsafe_b64encode(os.urandom(64)).decode().rstrip('=')
  challenge=base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip('=')
  state=secrets.token_urlsafe(24);result={}
  class Handler(BaseHTTPRequestHandler):
   def do_GET(s):
    q=urllib.parse.parse_qs(urllib.parse.urlparse(s.path).query);result.update({k:v[0] for k,v in q.items()})
    msg='NPLAY Spotify connected. You can close this tab and return to the terminal.' if q.get('code') else 'NPLAY Spotify authorization was not completed.'
    b=('<html><body style="background:#111;color:#eee;font:16px sans-serif;padding:3rem"><h2>'+msg+'</h2></body></html>').encode();s.send_response(200);s.send_header('Content-Type','text/html; charset=utf-8');s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)
   def log_message(self,*a):pass
  server=HTTPServer(('127.0.0.1',43821),Handler);server.timeout=1
  redirect='http://127.0.0.1:43821/callback'
  url=AUTH+'?'+urllib.parse.urlencode({'client_id':cid,'response_type':'code','redirect_uri':redirect,'scope':SCOPES,'state':state,'code_challenge_method':'S256','code_challenge':challenge})
  if open_browser:webbrowser.open(url)
  deadline=time.time()+timeout
  while time.time()<deadline and not result:server.handle_request()
  server.server_close()
  if result.get('state')!=state:raise RuntimeError('Spotify authorization state mismatch or timeout')
  if result.get('error'):raise RuntimeError('Spotify authorization denied · '+result['error'])
  code=result.get('code')
  if not code:raise RuntimeError('Spotify authorization timed out')
  data=urllib.parse.urlencode({'client_id':cid,'grant_type':'authorization_code','code':code,'redirect_uri':redirect,'code_verifier':verifier}).encode()
  req=urllib.request.Request(TOKEN,data=data,headers={'Content-Type':'application/x-www-form-urlencoded','User-Agent':f'NPLAY/{__version__}'})
  try:
   with urllib.request.urlopen(req,timeout=12) as r:tokens=json.load(r)
  except urllib.error.HTTPError as e:raise RuntimeError('Spotify token exchange failed · '+self._http_error(e))
  self._save_tokens(tokens);return self.profile()
 def disconnect(self):self.cfg.clear_spotify_tokens()
 def search(self,q,limit=20):
  # Dev Mode search max is 10/request as of 2026. Rank each result type locally
  # so exact/name-token matches beat Spotify's broader recommendation-like hits.
  d=self.get('/search',{'q':q,'type':'artist,album,track,playlist','limit':min(10,max(1,limit))}) or {};out=[]
  query=' '.join(str(q).casefold().split());qt=set(query.split())
  def score(t):
   title=' '.join(str(t.title or '').casefold().split());artist=' '.join(str(t.artist or '').casefold().split())
   words=set((title+' '+artist).split());overlap=len(qt & words)
   return (1000 if title==query else 0)+(500 if title.startswith(query) else 0)+(120*overlap)+(30 if query and query in title else 0)
  for key,fn in [('artists',_artist),('albums',_album),('tracks',_track),('playlists',_playlist)]:
   part=[fn(x) for x in ((d.get(key) or {}).get('items') or []) if x]
   part=sorted(enumerate(part),key=lambda z:(-score(z[1]),z[0]))
   # Artist search is especially noisy: suppress zero-token matches when Spotify
   # returned at least one genuine name match. Other groups retain all results.
   vals=[t for _,t in part]
   if key=='artists' and any(score(t)>0 for t in vals):vals=[t for t in vals if score(t)>0]
   out.extend(vals)
  return out
 def playlists(self):return [_playlist(x) for x in ((self.get('/me/playlists',{'limit':50}) or {}).get('items') or []) if x]
 def playlist_items(self,pid):
  out=[];context_uri='spotify:playlist:'+str(pid);url=f'/playlists/{pid}/items';params={'limit':50}
  # Preserve the full playlist context, not just the first API page.
  while url and len(out)<500:
   d=self.get(url,params) or {};params=None
   for row in d.get('items') or []:
    x=(row.get('item') or row.get('track') or row) if isinstance(row,dict) else None
    if x and x.get('type')=='track':
     t=_track(x);t.meta={**(t.meta or {}),'context_uri':context_uri,'context_kind':'playlist','context_index':len(out)};out.append(t)
   url=d.get('next')
  return out
 def album_tracks(self,aid):
  # Album-track objects do not contain the album artwork. Fetch the album once
  # and enrich all rows so list preview/Now Playing keep the expected cover.
  album=self.get(f'/albums/{aid}') or {}
  d=self.get(f'/albums/{aid}/tracks',{'limit':50}) or {}
  parent={'id':aid,'name':album.get('name',''),'images':album.get('images') or []}
  out=[]
  for x in (d.get('items') or []):
   if not x:continue
   t=_track({**x,'album':parent});t.meta={**(t.meta or {}),'context_uri':'spotify:album:'+str(aid),'context_kind':'album','context_index':len(out)};out.append(t)
  return out
 def playback_device(self):
  """Return the best usable Connect device, without guessing across many devices."""
  ds=[d for d in self.devices() if d.get('id') and not d.get('is_restricted')]
  active=next((d for d in ds if d.get('is_active')),None)
  if active:return active
  preferred=self.cfg.get('spotify_device_id','').strip()
  if preferred:
   hit=next((d for d in ds if d.get('id')==preferred),None)
   if hit:return hit
  if len(ds)==1:return ds[0]
  if not ds:raise RuntimeError('No Spotify Connect device available · open Spotify on a phone, desktop or web player, then use Spotify → Devices')
  raise RuntimeError('No active Spotify Connect device · choose one in Spotify → Devices')
 def play_track(self,t):
  d=self.playback_device()
  self.play(t,device_id=d.get('id'))
  return d
 def top_tracks(self):return [_track(x) for x in ((self.get('/me/top/tracks',{'limit':50,'time_range':'medium_term'}) or {}).get('items') or []) if x]
 def top_artists(self):return [_artist(x) for x in ((self.get('/me/top/artists',{'limit':50,'time_range':'medium_term'}) or {}).get('items') or []) if x]
 def recent(self):
  return [_track(row.get('track')) for row in ((self.get('/me/player/recently-played',{'limit':50}) or {}).get('items') or []) if row.get('track')]
 def devices(self):return (self.get('/me/player/devices') or {}).get('devices') or []
 def state(self):
  d=self.get('/me/player') or {};x=d.get('item') or {};t=_track(x) if x and x.get('type')=='track' else None
  return {'track':t,'is_playing':bool(d.get('is_playing')),'progress':float(d.get('progress_ms') or 0)/1000,'duration':float(x.get('duration_ms') or 0)/1000 if x else 0,'device':d.get('device') or {},'shuffle':bool(d.get('shuffle_state')),'repeat':d.get('repeat_state','off'),'context':d.get('context') or {}}
 def wait_for_device(self,name=None,timeout=20.0,poll=.5):
  """Wait until librespot is visible to Web API and return its concrete device.

  Device registration is eventually consistent. Never treat process start as proof
  that Spotify is ready to accept Player API commands.
  """
  name=name or (self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY')
  deadline=time.monotonic()+max(1.0,float(timeout))
  last=[]
  while time.monotonic()<deadline:
   if not self.local.running():raise RuntimeError('Local Spotify engine stopped · open Spotify → Local playback → Diagnostics')
   try:
    last=self.devices()
    hit=next((d for d in last if d.get('id') and not d.get('is_restricted') and d.get('name')==name),None)
    if hit:return hit
   except RuntimeError:
    pass
   time.sleep(poll)
  names=', '.join(str(d.get('name')) for d in last if d.get('name')) or 'none'
  log.warning('librespot device registration timeout · visible_devices=%s · engine_running=%s',names,self.local.running())
  raise RuntimeError(f'NPLAY local Spotify device did not register in time · visible devices: {names} · use Spotify → Local playback → Diagnostics; if authorization is required choose AUTHORIZE / REAUTHORIZE')
 def activate_device(self,device_id,timeout=8.0):
  """Transfer playback ownership and wait until Spotify reports this device active."""
  if not device_id:raise RuntimeError('Spotify device has no device ID')
  self.transfer(device_id,False)
  deadline=time.monotonic()+max(1.0,float(timeout))
  while time.monotonic()<deadline:
   try:
    ds=self.devices()
    hit=next((d for d in ds if d.get('id')==device_id),None)
    if hit and hit.get('is_active'):return hit
   except RuntimeError:
    pass
   time.sleep(.35)
  # Some Spotify backends only become active when the first explicit play command
  # arrives. Returning the concrete device is safe because play() still targets it.
  return next((d for d in self.devices() if d.get('id')==device_id),{'id':device_id})
 def preferred_device_id(self):
  return (self.cfg.get('spotify_device_id','') or '').strip()
 def play(self,t=None,device_id=None,context_uri=None,resume=False):
  # Resume must send no URI/context; sending the current URI restarts at 0:00.
  if resume:return self.put('/me/player/play',{'device_id':device_id} if device_id else None,None)
  body={}
  context_uri=context_uri or ((t.meta or {}).get('context_uri') if t else None)
  if context_uri:
   body['context_uri']=context_uri
   if t:body['offset']={'uri':(t.meta or {}).get('spotify_uri') or 'spotify:track:'+t.id}
  elif t:body['uris']=[t.meta.get('spotify_uri') or 'spotify:track:'+t.id]
  return self.put('/me/player/play',{'device_id':device_id} if device_id else None,body or None)
 def resume(self,device_id=None):return self.play(device_id=device_id,resume=True)
 def pause(self,device_id=None):return self.put('/me/player/pause',{'device_id':device_id} if device_id else None)
 def next(self,device_id=None):return self.post('/me/player/next',{'device_id':device_id} if device_id else None)
 def previous(self,device_id=None):return self.post('/me/player/previous',{'device_id':device_id} if device_id else None)
 def seek(self,seconds,device_id=None):return self.put('/me/player/seek',{'position_ms':max(0,int(seconds*1000)),'device_id':device_id} if device_id else {'position_ms':max(0,int(seconds*1000))})
 def volume(self,pct,device_id=None):return self.put('/me/player/volume',{'volume_percent':max(0,min(100,int(pct))),'device_id':device_id} if device_id else {'volume_percent':max(0,min(100,int(pct)))})
 def transfer(self,device_id,play=False):return self.put('/me/player',body={'device_ids':[device_id],'play':bool(play)})
 def queue_add(self,t):return self.post('/me/player/queue',{'uri':t.meta.get('spotify_uri') or 'spotify:track:'+t.id})
 def save(self,t):
  uri=t.meta.get('spotify_uri') or ('spotify:'+t.kind+':'+t.id);return self.put('/me/library',{'uris':uri})
 def add_to_playlist(self,pid,t):
  uri=t.meta.get('spotify_uri') or 'spotify:track:'+t.id;return self.post(f'/playlists/{pid}/items',{'uris':uri})
