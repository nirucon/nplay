from . import __version__
import hashlib,urllib.parse,urllib.request,time
from pathlib import Path
from .config import DATA

class ArtworkCache:
 def __init__(self,max_mb=500):
  self.root=DATA/'artwork';self.root.mkdir(parents=True,exist_ok=True);self.max_bytes=max(32,int(max_mb or 500))*1024*1024
 def key(self,url):
  key=str(url or '')
  try:
   u=urllib.parse.urlparse(key);q=urllib.parse.parse_qs(u.query)
   if u.path.endswith('/rest/getCoverArt.view') and q.get('id'):key=f'{u.scheme}://{u.netloc}{u.path}?id={q["id"][0]}'
  except Exception:pass
  return hashlib.sha256(key.encode()).hexdigest()
 def path(self,url):return self.root/(self.key(url)+'.art') if url else None
 def cached(self,url):
  if not url:return None
  s=str(url)
  if not s.startswith(('http://','https://')):
   p=Path(s).expanduser();return p if p.is_file() else None
  p=self.path(s)
  if p and p.exists() and p.stat().st_size>0:
   try:p.touch(exist_ok=True)
   except OSError:pass
   return p
  return None
 def fetch(self,url,timeout=8,user_agent=f'NPLAY/{__version__}'):
  hit=self.cached(url)
  if hit:return hit
  if not str(url or '').startswith(('http://','https://')):return None
  p=self.path(url)
  try:
   req=urllib.request.Request(str(url),headers={'User-Agent':user_agent,'Accept':'image/*'})
   with urllib.request.urlopen(req,timeout=timeout) as r:data=r.read(12*1024*1024)
   if not data:return None
   tmp=p.with_suffix('.tmp');tmp.write_bytes(data);tmp.replace(p);self.prune();return p
  except Exception:return None
 def stats(self):
  files=[];total=0
  for p in self.root.rglob('*'):
   if p.is_file() and not p.name.endswith('.tmp'):
    try:s=p.stat().st_size;total+=s;files.append(p)
    except OSError:pass
  return len(files),total
 def prune(self):
  files=[];total=0
  for p in self.root.rglob('*'):
   if p.is_file() and not p.name.endswith('.tmp'):
    try:st=p.stat();total+=st.st_size;files.append((st.st_atime or st.st_mtime,st.st_size,p))
    except OSError:pass
  if total<=self.max_bytes:return 0
  removed=0
  for _,size,p in sorted(files):
   if total<=int(self.max_bytes*.9):break
   try:p.unlink();total-=size;removed+=1
   except OSError:pass
  return removed
 def clear(self):
  n=0
  for p in self.root.rglob('*'):
   if p.is_file():
    try:p.unlink();n+=1
    except OSError:pass
  return n
