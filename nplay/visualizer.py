import subprocess,tempfile,threading,shutil,os,time
class Visualizer:
 def __init__(self,bars=48):self.bars=bars;self.values=[0]*bars;self.proc=None;self.path=None;self.running=False
 def start(self):
  if not shutil.which('cava'):return False
  if self.proc is not None and self.proc.poll() is None and self.running:return True
  if self.proc is not None:
   try:self.proc.wait(timeout=.2)
   except Exception:pass
   self.proc=None
  cfg=f'''[general]\nbars = {self.bars}\nframerate = 30\n[output]\nmethod = raw\nraw_target = /dev/stdout\ndata_format = binary\nbit_format = 8bit\nchannels = mono\n[smoothing]\nnoise_reduction = 77\n'''
  f=tempfile.NamedTemporaryFile('w',delete=False,prefix='nplay-cava-',suffix='.conf');f.write(cfg);f.close();self.path=f.name
  try:self.proc=subprocess.Popen(['cava','-p',self.path],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL);self.running=True;threading.Thread(target=self._read,daemon=True).start();return True
  except Exception:self.stop();return False
 def _read(self):
  try:
   while self.running and self.proc and self.proc.stdout:
    d=self.proc.stdout.read(self.bars)
    if len(d)==self.bars:self.values=list(d)
    else:break
  finally:
   self.running=False
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
  if self.path:
   try:os.unlink(self.path)
   except Exception:pass
