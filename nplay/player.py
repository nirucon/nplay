import subprocess,socket,json,os,time,threading,signal
from .config import STATE
from .logging_utils import logger
log=logger()
class Player:
 def __init__(self,on_end=None):
  self.proc=None;self.sock=None;self._lock=threading.RLock();self.current=None;self.on_end=on_end;self.generation=0;self.ready=threading.Event();self.failed=threading.Event();self.last_error=''
 def start(self,url,replaygain="track",preamp=0,clip=True,gapless=False,ytdl_path=None,timeout=20):
  with self._lock:
   return self._start_locked(url,replaygain,preamp,clip,gapless,ytdl_path,timeout)
 def _start_locked(self,url,replaygain,preamp,clip,gapless,ytdl_path,timeout):
  self._stop_locked();self.generation+=1;gen=self.generation;self.ready.clear();self.failed.clear();self.last_error=''
  # Per-generation sockets prevent old event readers and delayed mpv exits from
  # interacting with the new transport. The PID is known only after spawn, so
  # use a generation plus a unique per-process random suffix.
  import secrets
  self.sock=f'/tmp/nplay-mpv-{os.getuid()}-{os.getpid()}-{secrets.token_hex(5)}.sock'
  mpvlog=STATE/'mpv.log'
  args=['mpv','--no-video','--idle=yes','--really-quiet',f'--log-file={mpvlog}',f'--input-ipc-server={self.sock}','--volume=100','--pause=yes','--keep-open=no',f'--replaygain={replaygain}',f'--replaygain-preamp={float(preamp):g}',f'--replaygain-clip={'yes' if clip else 'no'}',f'--gapless-audio={'yes' if gapless else 'no'}']
  if ytdl_path:args.append(f'--script-opts=ytdl_hook-ytdl_path={ytdl_path}')
  args.append(url);log.info('mpv start source=%s',str(url)[:240])
  self.proc=subprocess.Popen(args,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
  proc=self.proc;sock=self.sock
  log.info('mpv spawned pid=%s generation=%s socket=%s',proc.pid,gen,sock)
  for _ in range(100):
   if proc.poll() is not None:break
   if os.path.exists(sock):break
   time.sleep(.03)
  threading.Thread(target=self._events,args=(gen,proc,sock),daemon=True).start()
  deadline=time.monotonic()+max(2,float(timeout))
  next_probe=time.monotonic()+.15
  last_status=None
  while time.monotonic()<deadline:
   if self.ready.wait(.05):return True
   if self.failed.is_set() or proc.poll() is not None:break
   # file-loaded is an edge-triggered event. Fast local files may finish
   # loading before the event reader connects. Reconcile with mpv state.
   if time.monotonic()>=next_probe:
    next_probe=time.monotonic()+.25
    status=self._playback_status(sock)
    if status is not None:
     last_status=status
     path,idle,core_idle=status
     if path==url and idle is False and core_idle is False:
      log.info('mpv ready via IPC state pid=%s gen=%s path=%r idle=%r core_idle=%r',proc.pid,gen,path,idle,core_idle)
      self.ready.set();return True
  if last_status is not None:log.warning('mpv readiness timeout pid=%s gen=%s expected=%r status=%r',proc.pid,gen,url,last_status)
  err=self.last_error or ('mpv exited before playback started' if self.proc and self.proc.poll() is not None else 'Timed out waiting for mpv to load media')
  log.error('mpv playback failed pid=%s gen=%s: %s',proc.pid,gen,err);self._stop_locked();raise RuntimeError(err+' · see '+str(mpvlog))
 def _events(self,gen,proc,sock):
  try:
   s=socket.socket(socket.AF_UNIX);s.settimeout(.5);s.connect(sock);buf=b''
   while gen==self.generation and proc.poll() is None:
    try:d=s.recv(65536)
    except socket.timeout:continue
    if not d:break
    buf+=d
    while b'\n' in buf:
     line,buf=buf.split(b'\n',1)
     try:m=json.loads(line)
     except Exception:continue
     ev=m.get('event')
     if ev=='file-loaded' and gen==self.generation:self.ready.set()
     elif ev=='end-file':
      reason=m.get('reason','');err=m.get('file_error') or m.get('error') or reason
      if reason=='error' and gen==self.generation:self.last_error='mpv: '+str(err);self.failed.set();log.error(self.last_error);return
      if reason=='eof' and gen==self.generation and self.on_end:self.on_end();return
   s.close()
  except Exception as e:
   if gen==self.generation and not self.ready.is_set():self.last_error='mpv IPC: '+str(e);self.failed.set();log.exception('mpv IPC failure')
 def _playback_status(self,sock):
  # Use the generation-specific socket, never the mutable current socket.
  # Read the complete IPC response and check mpv's error field.
  values=[]
  for name in ('path','idle-active','core-idle'):
   result=self._ipc(sock,['get_property',name])
   if result is None or result.get('error')!='success':return None
   values.append(result.get('data'))
  return tuple(values)
 def _ipc(self,sock,args):
  if not sock or not os.path.exists(sock):return None
  try:
   with socket.socket(socket.AF_UNIX) as s:
    s.settimeout(.35);s.connect(sock)
    s.sendall((json.dumps({'command':args})+'\n').encode())
    data=b''
    while b'\n' not in data:
     chunk=s.recv(65536)
     if not chunk:return None
     data+=chunk
     if len(data)>1048576:return None
   return json.loads(data.split(b'\n',1)[0])
  except (OSError,ValueError):return None
 def cmd(self,args):
  sock=self.sock
  result=self._ipc(sock,args)
  return result.get('data') if result and result.get('error')=='success' else None
 def prop(self,n):return self.cmd(['get_property',n])
 def active(self):
  if not (self.proc and self.proc.poll() is None and self.sock and os.path.exists(self.sock)):return False
  idle=self.prop('idle-active');return idle is not True
 def toggle(self):self.cmd(['cycle','pause'])
 def set_volume(self,v):self.cmd(['set_property','volume',max(0,min(150,float(v)))])
 def set_position(self,v):self.cmd(['set_property','time-pos',max(0,float(v or 0))])
 def seek(self,d):self.cmd(['seek',d,'relative'])
 def volume(self,d):self.cmd(['add','volume',d])
 def mute(self):self.cmd(['cycle','mute'])
 def stop(self):
  with self._lock:self._stop_locked()
 def _stop_locked(self):
  # A terminated child must be reaped before a replacement is spawned.
  self.generation+=1;self.ready.clear();self.failed.clear()
  p=self.proc;sock=self.sock
  self.proc=None;self.sock=None
  if p is not None:
   if p.poll() is None:
    try:p.terminate()
    except ProcessLookupError:pass
    try:p.wait(timeout=1.5)
    except subprocess.TimeoutExpired:
     log.warning('mpv pid=%s did not terminate; killing',p.pid)
     try:p.kill()
     except ProcessLookupError:pass
     try:p.wait(timeout=2)
     except subprocess.TimeoutExpired:log.error('mpv pid=%s could not be reaped',p.pid)
   else:p.wait()
   log.info('mpv stopped pid=%s returncode=%s',p.pid,p.returncode)
  if sock:
   try:os.unlink(sock)
   except FileNotFoundError:pass
