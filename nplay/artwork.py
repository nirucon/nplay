from . import __version__
import os,subprocess,shutil,threading
from .artwork_cache import ArtworkCache
class Artwork:
 def __init__(self,on_ready=None,cache=None):
  self.last='';self.cache=cache or ArtworkCache();self.kitten=shutil.which('kitten');self.kitty_bin=shutil.which('kitty');self.on_ready=on_ready;self.pending=set();self.lock=threading.Lock()
 def kitty(self):return bool(os.getenv('KITTY_WINDOW_ID')) and bool(self.kitten or self.kitty_bin)
 def path(self,url):return self.cache.path(url)
 def cached(self,url):return self.cache.cached(url)
 def prefetch(self,url):
  if not url or self.cached(url) or not str(url).startswith(('http://','https://')):return
  with self.lock:
   if url in self.pending:return
   self.pending.add(url)
  def work():
   try:self.cache.fetch(url,user_agent=f'NPLAY/{__version__}')
   finally:
    with self.lock:self.pending.discard(url)
    if self.on_ready:
     try:self.on_ready(url)
     except Exception:pass
  threading.Thread(target=work,daemon=True).start()
 def _cmd(self,*args):return ([self.kitten,'icat',*args] if self.kitten else [self.kitty_bin,'+kitten','icat',*args]) if self.kitty() else None
 def clear(self):
  if self.kitty():
   try:subprocess.run(self._cmd('--clear','--silent'),stderr=subprocess.DEVNULL,timeout=1)
   except Exception:pass
  self.last=''
 def draw(self,url,row,col,cols,rows):
  if not self.kitty() or not url or cols<8 or rows<4:return False
  p=self.cached(url)
  if not p:self.prefetch(url);return False
  key=f'{url}:{row}:{col}:{cols}:{rows}'
  if key==self.last:return True
  place=f'{cols}x{rows}@{max(0,col)}x{max(0,row)}'
  if self.last and self.last!=key:self.clear()
  try:
   cp=subprocess.run(self._cmd('--silent','--transfer-mode=file','--place',place,'--scale-up',str(p)),stderr=subprocess.DEVNULL,timeout=2)
   if cp.returncode==0:self.last=key;return True
  except Exception:pass
  return False
