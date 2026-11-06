"""Optional Spotify Web API + Spotify Connect integration for NPLAY.

Uses Authorization Code with PKCE for the Web API. Local playback is optional and
provided by a managed librespot process; external Spotify Connect devices remain available.
"""
from __future__ import annotations
from . import __version__
import base64, configparser, hashlib, json, os, secrets, shlex, time, urllib.parse, urllib.request, urllib.error, webbrowser, subprocess, shutil, signal, threading
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

def _desktop_browser_command(desktop_id):
 """Resolve a .desktop file to an argv list with a {url} placeholder."""
 if not desktop_id:return None
 roots=[Path.home()/'.local/share/applications','/usr/local/share/applications','/usr/share/applications']
 roots += [Path(p)/'applications' for p in os.environ.get('XDG_DATA_DIRS','/usr/local/share:/usr/share').split(':') if p]
 seen=set()
 for root in roots:
  path=(root/desktop_id) if not str(desktop_id).startswith('/') else Path(desktop_id)
  if path in seen or not path.is_file():continue
  seen.add(path)
  parser=configparser.ConfigParser(interpolation=None,strict=False)
  try:parser.read(path)
  except Exception:continue
  exec_line=parser.get('Desktop Entry','Exec',fallback='').strip()
  if not exec_line:continue
  try:cmd=shlex.split(exec_line)
  except ValueError:continue
  if not cmd:continue
  has_url=False;out=[]
  for arg in cmd:
   if arg in ('%u','%U','%f','%F'):
    out.append('{url}');has_url=True
   elif arg in ('%i','%c','%k'):
    continue
   elif any(code in arg for code in ('%u','%U','%f','%F')):
    out.append(arg.replace('%u','{url}').replace('%U','{url}').replace('%f','{url}').replace('%F','{url}'));has_url=True
   else:
    out.append(arg.replace('%%','%'))
  if not has_url:out.append('{url}')
  return out
 return None

def _browser_commands(url):
 """Build browser launch candidates in preference order, without launching."""
 candidates=[];seen=set()
 def add(cmd,kind=''):
  if not cmd:return
  key=tuple(cmd)
  if key not in seen:seen.add(key);candidates.append((cmd,kind))
 configured=os.environ.get('BROWSER','').strip()
 if configured:
  for entry in configured.split(':'):
   try:cmd=shlex.split(entry)
   except ValueError:continue
   if not cmd:continue
   if any('%s' in x for x in cmd):cmd=[x.replace('%s','{url}') for x in cmd]
   else:cmd=cmd+['{url}']
   add(cmd,'BROWSER')
 # Omarchy: its launcher honours the user's chosen browser and Hyprland rules.
 omarchy=shutil.which('omarchy-launch-browser')
 if omarchy:add([omarchy,'{url}'],'omarchy')
 for tool,args in (('xdg-settings',['get','default-web-browser']),('xdg-mime',['query','default','x-scheme-handler/https'])):
  exe=shutil.which(tool)
  if not exe:continue
  try:
   result=subprocess.run([exe]+args,text=True,capture_output=True,timeout=3)
   desktop=result.stdout.strip().splitlines()[0] if result.stdout.strip() else ''
  except Exception:desktop=''
  cmd=_desktop_browser_command(desktop)
  if cmd:add(cmd,'desktop')
 for name in ('helium','firefox','firefox-esr','chromium','chromium-browser','google-chrome','google-chrome-stable','brave-browser','brave','microsoft-edge','microsoft-edge-stable','vivaldi','opera','qutebrowser','epiphany','falkon','konqueror'):
  exe=shutil.which(name)
  if exe:add([exe,'{url}'],'known')
 for tool,args in (('xdg-open',[]),('gio',['open'])):
  exe=shutil.which(tool)
  if exe:add([exe]+args+['{url}'],'fallback')
 return [([x.replace('{url}',url) for x in cmd],kind) for cmd,kind in candidates]

def _open_authorization_url(url):
 """Open the Spotify consent URL with the system or a common browser."""
 for command,kind in _browser_commands(url):
  try:
   subprocess.Popen(command,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
   log.info('Spotify authorization browser candidate tried: %s (%s)',command[0],kind)
   return True
  except OSError as e:log.warning('Could not open Spotify browser %s: %s',command[0],e)
 try:
  opened=webbrowser.open(url)
  log.info('Spotify authorization webbrowser fallback returned %s',opened)
  return bool(opened)
 except Exception as e:
  log.warning('Spotify authorization browser fallback failed: %s',e)
  return False

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


# --- Local librespot receiver -------------------------------------------------
#
# Lifecycle rules (1.8.1):
# * A receiver NPLAY spawns dies with the NPLAY process that spawned it: normal
#   quit, SIGINT/SIGTERM/SIGHUP, service idle exit (explicit stop), and a crash
#   or SIGKILL of NPLAY (PR_SET_PDEATHSIG, Linux). The death signal follows the
#   *thread* that forked, so every spawn runs on one long-lived spawner thread
#   instead of short-lived worker threads.
# * One receiver per cache directory. Before spawning, NPLAY looks for librespot
#   processes of the same user whose argv uses the same --cache path (verified
#   in /proc/<pid>/cmdline). A receiver whose parent is a live NPLAY process is
#   adopted (shared, never killed on exit); any other one (an orphan from an
#   older NPLAY) is terminated. An OAuth start always replaces every receiver.
# * A flock in $XDG_RUNTIME_DIR/nplay serialises that decision between the
#   terminal UI and the service; a JSON pid file records the current owner.
# * Without cached credentials (credentials.json in --system-cache) a normal
#   start fails at once with LocalSpotifyNotAuthorized instead of waiting for a
#   device that can never register.

NOT_AUTHORIZED_CODE='spotify_local_unauthorized'
NOT_AUTHORIZED_MESSAGE='Local Spotify playback is not authorized yet · choose Spotify → Local playback → AUTHORIZE'

class LocalSpotifyNotAuthorized(RuntimeError):
 """No reusable librespot credentials are cached: OAuth is required first."""
 code=NOT_AUTHORIZED_CODE
 def __init__(self,message=NOT_AUTHORIZED_MESSAGE):super().__init__(message)

PR_SET_PDEATHSIG=1
_LIBC=None
def _libc():
 global _LIBC
 if _LIBC is None:
  try:
   import ctypes,ctypes.util
   _LIBC=ctypes.CDLL(ctypes.util.find_library('c') or 'libc.so.6',use_errno=True)
  except Exception:_LIBC=False
 return _LIBC or None

def _child_setup(parent_pid,deathsig=signal.SIGKILL):
 """Return a preexec_fn: kill the child when its spawning thread/process dies."""
 libc=_libc()
 def setup():
  if libc is not None:
   try:libc.prctl(PR_SET_PDEATHSIG,int(deathsig),0,0,0)
   except Exception:pass
  # The parent may have died between fork() and prctl(): do not linger.
  if os.getppid()!=parent_pid:os._exit(1)
 return setup

class _Spawner:
 """Run Popen on one long-lived daemon thread (see PR_SET_PDEATHSIG above)."""
 def __init__(self):
  self._q=None;self._thread=None;self._pid=None;self._lock=threading.Lock()
 def _loop(self,q):
  while True:
   fn,box,done=q.get()
   try:box['value']=fn()
   except BaseException as e:box['error']=e
   done.set()
 def run(self,fn):
  import queue
  with self._lock:
   # A fork()ed child process must not reuse the parent's (dead) thread.
   if self._thread is None or not self._thread.is_alive() or self._pid!=os.getpid():
    self._q=queue.Queue();self._pid=os.getpid()
    self._thread=threading.Thread(target=self._loop,args=(self._q,),daemon=True,name='nplay-librespot-spawner');self._thread.start()
   q=self._q
  if threading.current_thread() is self._thread:return fn()
  box={};done=threading.Event();q.put((fn,box,done));done.wait()
  if 'error' in box:raise box['error']
  return box['value']
_SPAWNER=_Spawner()

def _read_cmdline(pid):
 try:raw=Path(f'/proc/{int(pid)}/cmdline').read_bytes()
 except (OSError,ValueError):return []
 return [x.decode('utf-8','replace') for x in raw.split(b'\0') if x]

def _proc_uid(pid):
 try:return os.stat(f'/proc/{int(pid)}').st_uid
 except (OSError,ValueError):return None

def _proc_stat(pid):
 """Return (state, ppid) from /proc/<pid>/stat, or (None, None)."""
 try:raw=Path(f'/proc/{int(pid)}/stat').read_text()
 except (OSError,ValueError):return None,None
 rest=raw.rsplit(')',1)[-1].split()
 try:return rest[0],int(rest[1])
 except (IndexError,ValueError):return None,None

def _alive(pid):
 state,_=_proc_stat(pid)
 return state is not None and state not in ('Z','X')

def _is_librespot_argv(argv):
 # Native binary: argv[0]; script wrappers: interpreter + script path.
 return any(os.path.basename(a).startswith('librespot') for a in argv[:3])

def _argv_cache(argv):
 for i,a in enumerate(argv):
  if a in ('--cache','-c') and i+1<len(argv):return argv[i+1]
  if a.startswith('--cache='):return a.split('=',1)[1]
 return None

def _same_path(a,b):
 try:return os.path.realpath(a)==os.path.realpath(b)
 except (OSError,TypeError,ValueError):return False

def find_receivers(cache,exclude=()):
 """PIDs of live librespot processes of this user that use this --cache path."""
 out=[];uid=os.getuid();skip={int(p) for p in exclude if p}
 try:names=os.listdir('/proc')
 except OSError:return out
 for name in names:
  if not name.isdigit():continue
  pid=int(name)
  if pid in skip or pid==os.getpid() or _proc_uid(pid)!=uid:continue
  argv=_read_cmdline(pid)
  if not argv or not _is_librespot_argv(argv):continue
  c=_argv_cache(argv)
  if c and _same_path(c,cache) and _alive(pid):out.append(pid)
 return sorted(out)

def _is_nplay_process(pid):
 """A live NPLAY (terminal or service) process of this user."""
 if not pid or pid<=1 or _proc_uid(pid)!=os.getuid() or not _alive(pid):return False
 if pid==os.getpid():return True
 return any('nplay' in os.path.basename(a) or a.startswith('nplay.') or '/nplay/' in a for a in _read_cmdline(pid))

def terminate_pid(pid,timeout=3.0):
 """SIGTERM a receiver (its process group if it leads one), then SIGKILL."""
 def send(sig):
  try:
   if os.getpgid(pid)==pid:os.killpg(pid,sig)
   else:os.kill(pid,sig)
  except (ProcessLookupError,PermissionError):pass
 if not _alive(pid):return True
 send(signal.SIGTERM)
 for limit,sig in ((timeout,signal.SIGKILL),(2.0,None)):
  deadline=time.monotonic()+limit
  while time.monotonic()<deadline:
   if not _alive(pid):return True
   try:os.waitpid(pid,os.WNOHANG)  # reap if it happens to be our child
   except (ChildProcessError,OSError):pass
   time.sleep(.05)
  if sig:send(sig)
 return not _alive(pid)

_ENGINES=[]
def _close_all_engines():
 for e in list(_ENGINES):
  try:e.close()
  except Exception:pass
import atexit as _atexit
_atexit.register(_close_all_engines)

def _oauth_url_from_log(path,offset):
 """The consent URL librespot prints ("Browse to: https://…") after offset."""
 try:
  with open(path,'rb') as f:
   f.seek(offset);data=f.read(256*1024).decode('utf-8','replace')
 except OSError:return ''
 for line in data.splitlines():
  if 'Browse to:' in line:
   url=line.split('Browse to:',1)[1].strip()
   if url.startswith('https://'):return url
 return ''

_NO_OPEN_TOOLS=('xdg-open','gio','gnome-open','kde-open','wslview','open')
def _no_open_dir():
 """A private PATH prefix whose browser openers do nothing.

 librespot opens the consent page itself; NPLAY opens it with its own browser
 candidates (omarchy-launch-browser, $BROWSER, the desktop default, …)
 instead, so exactly one tab opens and it opens the same way as the Web API
 authorization.
 """
 from .config import private_runtime_dir
 d=private_runtime_dir()/'librespot-noopen'
 d.mkdir(mode=0o700,exist_ok=True)
 for tool in _NO_OPEN_TOOLS:
  p=d/tool
  if not p.exists():
   p.write_text('#!/bin/sh\n# NPLAY: the browser is opened by NPLAY, not by librespot.\nexit 0\n');os.chmod(p,0o700)
 return d


class LocalSpotifyEngine:
 """Lifecycle wrapper for an optional local librespot receiver.

 NPLAY owns the process: it is started on demand, uses a private XDG data/cache
 directory, and is stopped when NPLAY exits (see the lifecycle rules above).
 Authentication is handled by librespot's OAuth flow and cached by librespot.
 """
 AUTH_TIMEOUT=180.0
 def __init__(self,cfg):
  self.cfg=cfg;self.proc=None;self.adopted=None;self._closed=False;self._lock=threading.RLock();self.root=DATA/'spotify-local';self.cache=self.root/'cache';self.system_cache=self.root/'system-cache';self.log_path=STATE/'librespot.log'
  for d in (self.root,self.cache,self.system_cache):d.mkdir(parents=True,exist_ok=True);os.chmod(d,0o700)
  _ENGINES.append(self)
 def binary(self):return shutil.which('librespot')
 def available(self):return bool(self.binary())
 def _adopted_alive(self):
  pid=self.adopted
  if not pid:return False
  argv=_read_cmdline(pid)
  if argv and _is_librespot_argv(argv) and _same_path(_argv_cache(argv) or '',self.cache) and _alive(pid):return True
  self.adopted=None;return False
 def running(self):return bool(self.proc and self.proc.poll() is None) or self._adopted_alive()
 def owned(self):return bool(self.proc and self.proc.poll() is None)
 def pid(self):
  if self.owned():return self.proc.pid
  return self.adopted if self._adopted_alive() else None
 def credential_files(self):
  try:return [p for base in (self.system_cache,self.cache) for p in base.rglob('*') if p.is_file() and p.stat().st_size>0]
  except Exception:return []
 def credentials_path(self):return self.system_cache/'credentials.json'
 def has_credentials(self):
  """True when librespot has reusable credentials (credentials.json) cached.

  librespot stores them in --system-cache; older NPLAY builds without
  --system-cache left them in --cache.
  """
  for p in (self.credentials_path(),self.cache/'credentials.json'):
   try:
    if p.is_file() and p.stat().st_size>0:return True
   except OSError:pass
  return False
 def authorized(self):return self.has_credentials()
 def version(self):
  b=self.binary()
  if not b:return ''
  try:return subprocess.run([b,'--version'],capture_output=True,text=True,timeout=2).stdout.strip()
  except Exception:return ''
 # -- cross-process single instance ------------------------------------
 def _key(self):return hashlib.sha1(os.path.realpath(self.cache).encode()).hexdigest()[:12]
 def _runtime(self):
  from .config import private_runtime_dir
  return private_runtime_dir()
 def lock_path(self):return self._runtime()/f'librespot-{self._key()}.lock'
 def pid_path(self):return self._runtime()/f'librespot-{self._key()}.json'
 def _flock(self):
  import fcntl
  f=open(self.lock_path(),'a+');os.chmod(f.name,0o600)
  fcntl.flock(f.fileno(),fcntl.LOCK_EX)
  return f
 def _write_pidfile(self):
  try:
   tmp=self.pid_path().with_suffix('.tmp')
   tmp.write_text(json.dumps({'pid':self.proc.pid,'owner':os.getpid(),'cache':str(self.cache),'started':time.time()}));os.chmod(tmp,0o600);os.replace(tmp,self.pid_path())
  except (OSError,AttributeError):log.debug('librespot pid file not written',exc_info=True)
 def _clear_pidfile(self,pid):
  try:
   d=json.loads(self.pid_path().read_text())
   if int(d.get('pid') or 0)==int(pid):self.pid_path().unlink()
  except (OSError,ValueError,TypeError):pass
 def _reconcile_others_locked(self,replace):
  """Adopt a receiver another live NPLAY owns, or terminate stale ones.

  Returns the adopted pid or None. With replace=True every other receiver for
  this cache is terminated (OAuth, explicit restart).
  """
  own=self.proc.pid if self.owned() else None
  hint=None
  try:hint=int(json.loads(self.pid_path().read_text()).get('pid') or 0) or None
  except (OSError,ValueError,TypeError,AttributeError):pass
  pids=find_receivers(self.cache,exclude=[own] if own else [])
  if hint in pids:pids.remove(hint);pids.insert(0,hint)
  adopted=None
  for pid in pids:
   _,ppid=_proc_stat(pid)
   if not replace and adopted is None and _is_nplay_process(ppid) and ppid!=os.getpid():
    adopted=pid;log.info('librespot adopting receiver pid=%s owned by NPLAY pid=%s',pid,ppid);continue
   log.warning('librespot terminating %s receiver pid=%s parent=%s (same cache)','duplicate' if adopted else 'stale',pid,ppid)
   terminate_pid(pid)
  return adopted
 def stale_receivers(self):
  """Receivers for this cache that no live NPLAY owns (diagnostics, doctor)."""
  out=[]
  for pid in find_receivers(self.cache,exclude=[self.proc.pid] if self.owned() else []):
   _,ppid=_proc_stat(pid)
   if not _is_nplay_process(ppid):out.append(pid)
  return out
 # -- start/stop -------------------------------------------------------
 def start(self,oauth=False):
  with self._lock:
   return self._start_locked(oauth)
 def _start_locked(self,oauth=False):
  if self._closed:raise RuntimeError('NPLAY is shutting down · local Spotify engine not started')
  if not oauth and self.running():return True
  b=self.binary()
  if not b:raise RuntimeError('librespot is not installed · install it for local playback, or use an external Spotify Connect device')
  lock=self._flock()
  try:
   if oauth and self.owned():self._stop_owned_locked()
   self.adopted=None
   adopted=self._reconcile_others_locked(replace=bool(oauth))
   if adopted:
    self.adopted=adopted;return True
   if not oauth and not self.has_credentials():
    log.warning('librespot not started · no cached credentials (cached_files=%d) · authorization required',len(self.credential_files()))
    raise LocalSpotifyNotAuthorized()
   return self._spawn_locked(b,oauth)
  finally:lock.close()
 def _spawn_locked(self,b,oauth):
  bitrate=self.cfg.get('spotify_local_bitrate','320') or '320';name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY'
  backend=self.cfg.get('spotify_local_backend','pulseaudio') or 'pulseaudio'
  args=[b,'--name',name,'--device-type','computer','--bitrate',bitrate,'--cache',str(self.cache),'--system-cache',str(self.system_cache),'--backend',backend,'--initial-volume',str(int(float(self.cfg.get('spotify_local_volume','70') or 70))),'--quiet']
  env=os.environ.copy();env.setdefault('PULSE_PROP_application.name','NPLAY · Spotify');env.setdefault('PULSE_PROP_media.role','music')
  # First run: librespot runs its OAuth flow and persists its credential blob.
  # Subsequent runs reuse that blob and do not need a browser.
  if oauth:
   log.info('librespot OAuth explicitly requested by setup flow')
   args.append('--enable-oauth')
   try:
    env['PATH']=str(_no_open_dir())+os.pathsep+env.get('PATH','');env.pop('BROWSER',None)
   except OSError:log.warning('librespot no-open PATH prefix unavailable; librespot may open its own browser tab')
  else:log.info('librespot start without OAuth · credentials cached · cached_files=%d',len(self.credential_files()))
  parent=os.getpid()
  def spawn():
   log_file=open(self.log_path,'ab',buffering=0)
   try:os.chmod(self.log_path,0o600)
   except OSError:pass
   try:return subprocess.Popen(args,stdin=subprocess.DEVNULL,stdout=log_file,stderr=log_file,start_new_session=True,env=env,preexec_fn=_child_setup(parent))
   finally:log_file.close()
  self.proc=_SPAWNER.run(spawn)
  time.sleep(.25)
  if self.proc.poll() is not None:
   log.error('librespot exited during startup rc=%s · log=%s',self.proc.returncode,self.log_path)
   self.proc=None
   raise RuntimeError('librespot failed to start · open Spotify → Local playback → Diagnostics')
  self._write_pidfile()
  log.info('librespot process started pid=%s oauth=%s log=%s',self.proc.pid,bool(oauth),self.log_path)
  return True
 def _stop_owned_locked(self):
  p=self.proc
  if not p:return
  try:
   if p.poll() is None:
    try:os.killpg(p.pid,signal.SIGTERM)
    except ProcessLookupError:pass
    try:p.wait(timeout=3)
    except subprocess.TimeoutExpired:
     try:os.killpg(p.pid,signal.SIGKILL)
     except ProcessLookupError:pass
     p.wait(timeout=3)
   else:p.wait()
  finally:
   if self.proc is p:self.proc=None
   self._clear_pidfile(p.pid)
 def stop(self,release_only=False):
  """Stop the receiver this engine uses.

  An owned receiver is terminated. An adopted one (owned by another live
  NPLAY) is terminated too unless release_only, which just forgets it.
  """
  with self._lock:
   adopted=self.adopted;self.adopted=None
   if adopted and not release_only and _alive(adopted):
    log.info('librespot stopping adopted receiver pid=%s',adopted);terminate_pid(adopted)
   self._stop_owned_locked()
 def close(self):
  """Final shutdown: stop the owned receiver and refuse later starts."""
  with self._lock:
   self._closed=True
   self.stop(release_only=True)
 def restart(self):
  """Restart the local receiver once; preserve OAuth caches."""
  with self._lock:
   self.stop()
   return self.start(oauth=False)
 def authorize(self,timeout=None,opener=None,on_url=None):
  """Run librespot's OAuth flow; return once fresh credentials are cached.

  Any receiver for this cache is replaced. librespot ignores --enable-oauth
  while credentials.json exists, so an existing blob is moved aside and put
  back if the flow fails. The consent URL that librespot prints is opened with
  NPLAY's browser candidates (opener, default _open_authorization_url).
  """
  timeout=float(timeout or self.AUTH_TIMEOUT);opener=opener or _open_authorization_url
  with self._lock:
   creds=self.credentials_path();backup=creds.with_name('credentials.json.nplay-previous')
   self.stop()
   for old in (creds,self.cache/'credentials.json'):
    if old.exists():os.replace(old,backup)
   try:offset=self.log_path.stat().st_size
   except OSError:offset=0
   try:self.start(oauth=True)
   except BaseException:
    self._restore_credentials(backup);raise
  opened=False;deadline=time.monotonic()+timeout
  try:
   while time.monotonic()<deadline:
    if self.has_credentials():
     try:backup.unlink()
     except OSError:pass
     log.info('librespot OAuth completed · credentials cached')
     return True
    if not self.running():raise RuntimeError('librespot exited during authorization · open Spotify → Local playback → Diagnostics')
    if not opened:
     url=_oauth_url_from_log(self.log_path,offset)
     if url:
      opened=True
      if on_url:
       try:on_url(url)
       except Exception:pass
      if not opener(url):log.warning('librespot OAuth URL could not be opened in a browser · see %s',self.log_path)
    time.sleep(.25)
   raise RuntimeError(f'Local Spotify authorization timed out after {int(timeout)}s · retry and complete authorization in the browser')
  except BaseException:
   with self._lock:
    self.stop()
    self._restore_credentials(backup)
   raise
 def _restore_credentials(self,backup):
  if backup.exists() and not self.credentials_path().exists():
   try:os.replace(backup,self.credentials_path());log.info('librespot previous credentials restored after failed authorization')
   except OSError:pass
 def log_tail(self,n=12):
  try:return '\n'.join(self.log_path.read_text(errors='replace').splitlines()[-n:])
  except Exception:return ''

class LocalDeviceRegistrationTimeout(RuntimeError):
 """Managed receiver is alive but not visible through Spotify Connect."""


class Spotify:
 def __init__(self,cfg):
  self.cfg=cfg;self.local=LocalSpotifyEngine(cfg);self._recovery_lock=threading.Lock();self._last_recovery=0.0
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
   if e.code==403 and 'Restriction violated' in self._http_error(e):
    raise RuntimeError('Spotify 403 · playback restricted by Spotify · verify Premium, account/device and Spotify Connect in Diagnostics; retry from Spotify app. This is not a librespot registration timeout')
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
    # Only the registered loopback callback may mutate the authorization result.
    # Browsers routinely request /favicon.ico; that must never alter OAuth state.
    if urllib.parse.urlparse(s.path).path!='/callback':
     b=b'Not found';s.send_response(404);s.send_header('Content-Type','text/plain; charset=utf-8');s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b);return
    q=urllib.parse.parse_qs(urllib.parse.urlparse(s.path).query);result.update({k:v[0] for k,v in q.items()})
    msg='NPLAY Spotify connected. You can close this tab and return to the terminal.' if q.get('code') else 'NPLAY Spotify authorization was not completed.'
    b=('<html><body style="background:#111;color:#eee;font:16px sans-serif;padding:3rem"><h2>'+msg+'</h2></body></html>').encode();s.send_response(200);s.send_header('Content-Type','text/html; charset=utf-8');s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)
   def log_message(self,*a):pass
  try:
   server=HTTPServer(('127.0.0.1',43821),Handler)
  except OSError as e:
   raise RuntimeError(f'Spotify callback port 43821 is unavailable · {e}') from e
  server.timeout=1
  redirect='http://127.0.0.1:43821/callback'
  url=AUTH+'?'+urllib.parse.urlencode({'client_id':cid,'response_type':'code','redirect_uri':redirect,'scope':SCOPES,'state':state,'code_challenge_method':'S256','code_challenge':challenge})
  try:
   if open_browser and not _open_authorization_url(url):
    raise RuntimeError('Spotify authorization URL could not be opened in a browser. Open this URL manually: '+url)
   deadline=time.time()+timeout
   while time.time()<deadline and not result:server.handle_request()
  finally:
   # Always release the registered loopback port, including browser-open
   # failures and interrupted callbacks. Otherwise a retry can fail with
   # EADDRINUSE until the Python object is garbage-collected.
   server.server_close()
  if not result:raise RuntimeError(f'Spotify authorization timed out after {timeout}s · retry and complete authorization in the browser')
  if result.get('state')!=state:raise RuntimeError('Spotify authorization state mismatch · retry and complete authorization in the browser')
  if result.get('error'):raise RuntimeError('Spotify authorization denied · '+result['error'])
  code=result.get('code')
  if not code:raise RuntimeError('Spotify authorization returned no code · retry authorization')
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
  raise LocalDeviceRegistrationTimeout(f'NPLAY local Spotify device did not register in time · visible devices: {names} · use Spotify → Local playback → Diagnostics; if authorization is required choose AUTHORIZE / REAUTHORIZE')
 def ensure_local_device(self,name=None,timeout=20.0):
  """Recover once from a running-but-unregistered local Spotify receiver.

  Never restart an external Connect device or a currently registered receiver.
  Rate-limit restarts to prevent repeated failures from causing restart loops.
  """
  name=name or (self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY')
  self.local.start(oauth=False)
  try:return self.wait_for_device(name,timeout)
  except LocalDeviceRegistrationTimeout as first_error:
   with self._recovery_lock:
    # Another request may have recovered the receiver while we were waiting.
    try:
     for d in self.devices():
      if d.get('id') and d.get('name')==name and not d.get('is_restricted'):return d
    except RuntimeError:pass
    now=time.monotonic()
    if now-self._last_recovery<90:
     raise RuntimeError(str(first_error)+' · automatic recovery cooldown active') from first_error
    self._last_recovery=now
    log.warning('Spotify Connect missing: restarting owned librespot once')
    self.local.restart()
    try:return self.wait_for_device(name,timeout)
    except LocalDeviceRegistrationTimeout as e:
     raise RuntimeError('Spotify Connect unavailable after one automatic local receiver restart · '+str(e)) from e
 def authorize_local(self,timeout=180.0,on_url=None,device_timeout=30.0):
  """Shared terminal/service OAuth flow for the local receiver.

  Success means librespot cached fresh credentials. When the Web API is
  connected too, wait (bounded) for the receiver to register and return it;
  otherwise return {} (the receiver keeps running and registers on its own).
  """
  self.local.authorize(timeout,on_url=on_url)
  if not self.connected():return {}
  name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY'
  try:return self.wait_for_device(name,device_timeout)
  except RuntimeError as e:
   log.warning('local Spotify authorized but device not visible yet · %s',e)
   return {}
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
  try:
   return self.put('/me/player/play',{'device_id':device_id} if device_id else None,body or None)
  except RuntimeError as exc:
   # Context resolution can fail in librespot after reconnect. Retry exactly
   # once with a plain track URI, never for arbitrary HTTP/network failures.
   # A second 403 remains an actual Spotify API refusal, not an auth prompt.
   if context_uri and t and 'Spotify 403' in str(exc):
    uri=(t.meta or {}).get('spotify_uri') or 'spotify:track:'+t.id
    if not uri.startswith('spotify:track:'):raise
    log.warning('Spotify rejected context playback (403); retrying track URI only')
    return self.put('/me/player/play',{'device_id':device_id} if device_id else None,{'uris':[uri]})
   raise
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
