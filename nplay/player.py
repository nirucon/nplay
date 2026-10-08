import subprocess,socket,json,os,time,threading
from .config import STATE
from .logging_utils import logger
log=logger()
class Player:
 def __init__(self,on_end=None):
  self.proc=None;self.sock=f'/tmp/nplay-mpv-{os.getuid()}.sock';self.current=None;self.on_end=on_end;self.generation=0;self.ready=threading.Event();self.failed=threading.Event();self.last_error=''
 def start(self,url,replaygain="track",preamp=0,clip=True,gapless=False,ytdl_path=None,timeout=20):
  self.stop();self.generation+=1;gen=self.generation;self.ready.clear();self.failed.clear();self.last_error=''
  try:os.unlink(self.sock)
  except FileNotFoundError:pass
  mpvlog=STATE/'mpv.log'
  args=['mpv','--no-video','--idle=yes','--really-quiet',f'--log-file={mpvlog}',f'--input-ipc-server={self.sock}','--volume=100','--pause=yes','--keep-open=no',f'--replaygain={replaygain}',f'--replaygain-preamp={float(preamp):g}',f'--replaygain-clip={'yes' if clip else 'no'}',f'--gapless-audio={'yes' if gapless else 'no'}']
  if ytdl_path:args.append(f'--script-opts=ytdl_hook-ytdl_path={ytdl_path}')
  args.append(url);log.info('mpv start source=%s',str(url)[:240])
  self.proc=subprocess.Popen(args,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
  for _ in range(100):
   if self.proc.poll() is not None:break
   if os.path.exists(self.sock):break
   time.sleep(.03)
  threading.Thread(target=self._events,args=(gen,),daemon=True).start()
  deadline=time.monotonic()+max(2,float(timeout))
  while time.monotonic()<deadline:
   if self.ready.wait(.05):return True
   if self.failed.is_set() or not self.proc or self.proc.poll() is not None:break
  err=self.last_error or ('mpv exited before playback started' if self.proc and self.proc.poll() is not None else 'Timed out waiting for mpv to load media')
  log.error('mpv playback failed: %s',err);self.stop();raise RuntimeError(err+' · see '+str(mpvlog))
 def _events(self,gen):
  try:
   s=socket.socket(socket.AF_UNIX);s.settimeout(.5);s.connect(self.sock);buf=b''
   while gen==self.generation and self.proc and self.proc.poll() is None:
    try:d=s.recv(65536)
    except socket.timeout:continue
    if not d:break
    buf+=d
    while b'\n' in buf:
     line,buf=buf.split(b'\n',1)
     try:m=json.loads(line)
     except Exception:continue
     ev=m.get('event')
     if ev=='file-loaded':self.ready.set()
     elif ev=='end-file':
      reason=m.get('reason','');err=m.get('file_error') or m.get('error') or reason
      if reason=='error':self.last_error='mpv: '+str(err);self.failed.set();log.error(self.last_error);return
      if reason=='eof' and gen==self.generation and self.on_end:self.on_end();return
   s.close()
  except Exception as e:
   if gen==self.generation and not self.ready.is_set():self.last_error='mpv IPC: '+str(e);self.failed.set();log.exception('mpv IPC failure')
 def cmd(self,args):
  if not os.path.exists(self.sock):return None
  try:
   s=socket.socket(socket.AF_UNIX);s.settimeout(.25);s.connect(self.sock);s.sendall((json.dumps({'command':args})+'\n').encode());data=b''
   while b'\n' not in data:data+=s.recv(65536)
   s.close();return json.loads(data.split(b'\n')[0]).get('data')
  except Exception:return None
 def prop(self,n):return self.cmd(['get_property',n])
 def active(self):
  if not (self.proc and self.proc.poll() is None and os.path.exists(self.sock)):return False
  idle=self.prop('idle-active');return idle is not True
 def toggle(self):self.cmd(['cycle','pause'])
 def set_volume(self,v):self.cmd(['set_property','volume',max(0,min(150,float(v)))])
 def set_position(self,v):self.cmd(['set_property','time-pos',max(0,float(v or 0))])
 def seek(self,d):self.cmd(['seek',d,'relative'])
 def volume(self,d):self.cmd(['add','volume',d])
 def mute(self):self.cmd(['cycle','mute'])
 def stop(self):
  self.generation+=1;self.ready.clear();self.failed.clear()
  if self.proc and self.proc.poll() is None:
   try:self.cmd(['quit'])
   except Exception:pass
   try:self.proc.wait(timeout=.5)
   except Exception:
    try:self.proc.terminate()
    except Exception:pass
  self.proc=None
