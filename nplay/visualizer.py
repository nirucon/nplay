import subprocess,tempfile,threading,shutil,os,time
from .config import private_runtime_dir
class Visualizer:
 def __init__(self,bars=48,on_frame=None,on_exit=None):
  # on_frame(vis,values) runs on the reader thread for every CAVA frame, so a
  # push consumer needs no timer of its own; on_exit(vis) runs when CAVA ends.
  self.bars=bars;self.values=[0]*bars;self.proc=None;self.path=None;self.running=False;self.on_frame=on_frame;self.on_exit=on_exit
 def start(self):
  if not shutil.which('cava'):return False
  if self.proc is not None and self.proc.poll() is None and self.running:return True
  if self.proc is not None:
   try:self.proc.wait(timeout=.2)
   except Exception:pass
   self.proc=None
  cfg=f'''[general]\nbars = {self.bars}\nframerate = 30\n[output]\nmethod = raw\nraw_target = /dev/stdout\ndata_format = binary\nbit_format = 8bit\nchannels = mono\n[smoothing]\nnoise_reduction = 77\n'''
  # Private 0700 runtime directory ($XDG_RUNTIME_DIR/nplay), never shared /tmp.
  self._remove_config()
  fd,self.path=tempfile.mkstemp(prefix='cava-',suffix='.conf',dir=str(private_runtime_dir()))
  with os.fdopen(fd,'w') as f:f.write(cfg)
  try:self.proc=subprocess.Popen(['cava','-p',self.path],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL);self.running=True;threading.Thread(target=self._read,args=(self.proc,),daemon=True,name='nplay-cava').start();return True
  except Exception:self.stop();return False
 def _read(self,proc):
  try:
   while self.running and proc.stdout:
    d=proc.stdout.read(self.bars)
    if len(d)==self.bars:
     self.values=list(d)
     if self.on_frame:
      try:self.on_frame(self,self.values)
      except Exception:pass
    else:break
  finally:
   if proc is self.proc:
    self.running=False
    # CAVA has read its config at start-up; remove it as soon as CAVA ends.
    self._remove_config()
   if self.on_exit:
    try:self.on_exit(self)
    except Exception:pass
 def get(self,n):
  vals=self.values
  if n<=0:return []
  if len(vals)==n:return vals
  return [vals[min(len(vals)-1,int(i*len(vals)/n))] for i in range(n)] if vals else [0]*n
 def stop(self):
  self.running=False
  if self.proc:
   try:self.proc.terminate();self.proc.wait(timeout=.3)
   except Exception:pass
  self.proc=None
  self._remove_config()
 def _remove_config(self):
  path,self.path=self.path,None
  if path:
   try:os.unlink(path)
   except OSError:pass
