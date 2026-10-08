import sqlite3,json,time,hashlib,os,base64,shutil
from pathlib import Path
from .config import DATA
from .model import Track
AUDIO={'.mp3','.flac','.opus','.ogg','.oga','.m4a','.aac','.wav','.webm','.mp4','.wma','.ape','.aiff','.aif'}
COVERS=('cover.jpg','cover.jpeg','cover.png','folder.jpg','folder.jpeg','folder.png','front.jpg','front.png')
class DB:
 def __init__(self):self.path=DATA/'library.db';self.init();self.last_scan={}
 def con(self):
  c=sqlite3.connect(self.path,timeout=10.0)
  c.row_factory=sqlite3.Row
  c.execute('PRAGMA foreign_keys=ON')
  c.execute('PRAGMA busy_timeout=10000')
  return c
 def _write(self,fn,retries=6):
  """Run a short write transaction with bounded SQLITE_BUSY retry."""
  delay=.04
  for attempt in range(retries):
   try:
    with self.con() as c:return fn(c)
   except sqlite3.OperationalError as e:
    if 'locked' not in str(e).lower() and 'busy' not in str(e).lower():raise
    if attempt+1>=retries:raise
    time.sleep(delay);delay=min(.8,delay*2)
 def _executemany(self,sql,rows):
  if not rows:return
  self._write(lambda c:c.executemany(sql,rows))
 def init(self):
  # Schema migrations are deliberately additive. User libraries/playlists are never rebuilt.
  with self.con() as c:
   c.execute('PRAGMA journal_mode=WAL')
   c.execute('PRAGMA synchronous=NORMAL')
   c.execute('PRAGMA busy_timeout=10000')
   pre_version=int(c.execute('PRAGMA user_version').fetchone()[0])
   if pre_version<5 and self.path.exists():
    backup=self.path.with_name('library.pre-1.2.0.db')
    if not backup.exists():
     try:shutil.copy2(self.path,backup)
     except OSError:pass
   c.executescript("""CREATE TABLE IF NOT EXISTS tracks(id TEXT PRIMARY KEY,path TEXT UNIQUE,title TEXT,artist TEXT,album TEXT,mtime REAL,added REAL); CREATE TABLE IF NOT EXISTS favorites(id TEXT PRIMARY KEY,data TEXT,added REAL); CREATE TABLE IF NOT EXISTS history(id INTEGER PRIMARY KEY AUTOINCREMENT,item_id TEXT,data TEXT,played REAL); CREATE TABLE IF NOT EXISTS resume(item_id TEXT PRIMARY KEY,pos REAL,updated REAL); CREATE TABLE IF NOT EXISTS playlists(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE COLLATE NOCASE,created REAL); CREATE TABLE IF NOT EXISTS playlist_items(id INTEGER PRIMARY KEY AUTOINCREMENT,playlist_id INTEGER NOT NULL,data TEXT NOT NULL,position INTEGER NOT NULL,added REAL,FOREIGN KEY(playlist_id) REFERENCES playlists(id) ON DELETE CASCADE); CREATE TABLE IF NOT EXISTS radio_stations(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT UNIQUE COLLATE NOCASE,url TEXT NOT NULL,homepage TEXT,artwork TEXT,added REAL); CREATE TABLE IF NOT EXISTS ratings(item_id TEXT PRIMARY KEY,rating INTEGER NOT NULL,updated REAL); CREATE TABLE IF NOT EXISTS discoveries(id INTEGER PRIMARY KEY AUTOINCREMENT,title TEXT,artist TEXT,source TEXT,data TEXT,added REAL); CREATE TABLE IF NOT EXISTS bookmarks(id INTEGER PRIMARY KEY AUTOINCREMENT,item_key TEXT,data TEXT,pos REAL,added REAL);""")
   cols={r[1] for r in c.execute('PRAGMA table_info(tracks)')}
   if 'added' not in cols:
    backup=self.path.with_name('library.pre-0.4.3.db')
    if self.path.exists() and not backup.exists():
     try:shutil.copy2(self.path,backup)
     except OSError:pass
    c.execute('ALTER TABLE tracks ADD COLUMN added REAL')
    c.execute('UPDATE tracks SET added=coalesce(mtime,?) WHERE added IS NULL',(time.time(),))
   old_version=int(c.execute('PRAGMA user_version').fetchone()[0])
   if old_version<4:
    backup=self.path.with_name('library.pre-1.1.0.db')
    if self.path.exists() and not backup.exists():
     try:shutil.copy2(self.path,backup)
     except OSError:pass
   for col,typ in [('album_artist','TEXT'),('track_no','INTEGER'),('disc_no','INTEGER'),('year','INTEGER'),('genre','TEXT'),('duration','REAL'),('codec','TEXT'),('sample_rate','INTEGER'),('bit_depth','INTEGER'),('channels','INTEGER'),('bitrate','INTEGER')]:
    if col not in {r[1] for r in c.execute('PRAGMA table_info(tracks)')}:c.execute(f'ALTER TABLE tracks ADD COLUMN {col} {typ}')
   c.execute('CREATE TABLE IF NOT EXISTS play_events(id INTEGER PRIMARY KEY AUTOINCREMENT,item_key TEXT,data TEXT,started REAL,ended REAL,seconds REAL DEFAULT 0,completed INTEGER DEFAULT 0)')
   c.execute('PRAGMA user_version=5')
 def bookmark_add(self,t,pos):
  key=f"{t.source}:{t.id}";data=json.dumps(t.dict())
  return self._write(lambda c:c.execute('INSERT INTO bookmarks(item_key,data,pos,added) VALUES(?,?,?,?)',(key,data,max(0,float(pos or 0)),time.time())).lastrowid)
 def bookmarks(self,limit=200):
  with self.con() as c:rows=c.execute('SELECT id,data,pos,added FROM bookmarks ORDER BY added DESC LIMIT ?',(limit,)).fetchall()
  out=[]
  for r in rows:
   try:
    t=Track.from_dict(json.loads(r['data']));t.meta={**(t.meta or {}),'bookmark_id':r['id'],'bookmark_pos':float(r['pos'] or 0),'bookmark_added':r['added']};out.append(t)
   except Exception:pass
  return out
 def bookmark_delete(self,bid):self._write(lambda c:c.execute('DELETE FROM bookmarks WHERE id=?',(int(bid),)))
 def count(self):
  with self.con() as c:return c.execute('SELECT count(*) FROM tracks').fetchone()[0]
 def _metadata(self,f):
  out={'title':f.stem,'artist':'','album':f.parent.name,'album_artist':'','track_no':0,'disc_no':0,'year':0,'genre':'','duration':0.0,'codec':f.suffix.lower().lstrip('.'),'sample_rate':0,'bit_depth':0,'channels':0,'bitrate':0}
  try:
   from mutagen import File
   a=File(str(f),easy=False)
   if a is not None:
    easy=File(str(f),easy=True)
    def one(k,d=''):
     if easy is None:return d
     v=easy.get(k,[d]);return str(v[0]) if isinstance(v,(list,tuple)) and v else str(v or d)
    def num(v):
     try:return int(str(v or '0').split('/')[0].split('-')[0])
     except:return 0
    out.update(title=one('title',out['title']),artist=one('artist',''),album=one('album',out['album']),album_artist=one('albumartist',''),track_no=num(one('tracknumber','0')),disc_no=num(one('discnumber','0')),year=num(one('date','0')),genre=one('genre',''))
    info=getattr(a,'info',None)
    if info:
     out['duration']=float(getattr(info,'length',0) or 0);out['sample_rate']=int(getattr(info,'sample_rate',0) or 0);out['channels']=int(getattr(info,'channels',0) or 0);out['bitrate']=int((getattr(info,'bitrate',0) or 0)/1000);out['bit_depth']=int(getattr(info,'bits_per_sample',0) or 0)
     out['codec']=a.__class__.__name__.replace('File','').lower() or out['codec']
  except Exception:pass
  return out
 def scan(self,roots,progress=None,excludes=None,batch_size=250,force=False):
  """Incrementally scan large libraries without holding SQLite write locks.

  Configured root symlinks are resolved and followed. Symlinks discovered below
  a root are not followed. Missing/offline roots are retained and never treated
  as mass deletion. Database writes are committed in small batches so playback
  checkpoints, history and playlists remain responsive during long scans.
  """
  raw=[Path(x).expanduser() for x in roots if str(x).strip()]
  roots=[];offline=[]
  for r in raw:
   try:rr=r.resolve(strict=True)
   except (OSError,RuntimeError):offline.append(str(r));continue
   if rr.is_dir() and rr not in roots:roots.append(rr)
  patterns=[str(x).strip() for x in (excludes or []) if str(x).strip()]
  builtins={'.git','.svn','node_modules','__pycache__','.Trash-1000','.Trash'}
  def excluded(path,name):
   import fnmatch
   if name in builtins:return True
   rel=str(path)
   return any(fnmatch.fnmatch(name,p) or fnmatch.fnmatch(rel,p) or fnmatch.fnmatch(rel,'*/'+p) for p in patterns)
  with self.con() as c:
   existing={r['path']:(r['id'],r['mtime']) for r in c.execute('SELECT id,path,mtime FROM tracks')}
  seen=set();checked=new_count=updated=0;pending=[];last_report=0
  sql='INSERT OR REPLACE INTO tracks(id,path,title,artist,album,mtime,added,album_artist,track_no,disc_no,year,genre,duration,codec,sample_rate,bit_depth,channels,bitrate) VALUES(?,?,?,?,?,?,coalesce((SELECT added FROM tracks WHERE id=?),?),?,?,?,?,?,?,?,?,?,?,?)'
  def flush():
   nonlocal pending
   if pending:self._executemany(sql,pending);pending=[]
  for root in roots:
   for base,dirs,files in os.walk(root,followlinks=False):
    bp=Path(base)
    # Never recurse into hidden/excluded dirs or nested symlinks. A configured
    # root may itself be a symlink because it was resolved above.
    keep=[]
    for d in dirs:
     q=bp/d
     if d.startswith('.') or excluded(q,d) or q.is_symlink():continue
     keep.append(d)
    dirs[:]=keep
    for name in files:
     f=bp/name
     if f.suffix.lower() not in AUDIO or excluded(f,name):continue
     try:real=str(f.resolve(strict=True));st=f.stat()
     except (OSError,RuntimeError):continue
     if real in seen:continue
     seen.add(real);checked+=1;old=existing.get(real)
     if (not force) and old and abs(float(old[1] or 0)-st.st_mtime)<.001:
      if progress and checked-last_report>=500:
       progress({'checked':checked,'new':new_count,'updated':updated,'offline':offline});last_report=checked
      continue
     rid=old[0] if old else hashlib.sha1(real.encode()).hexdigest()
     m=self._metadata(f)
     pending.append((rid,real,m['title'],m['artist'],m['album'],st.st_mtime,rid,time.time(),m['album_artist'],m['track_no'],m['disc_no'],m['year'],m['genre'],m['duration'],m['codec'],m['sample_rate'],m['bit_depth'],m['channels'],m['bitrate']))
     updated+=bool(old);new_count+=not bool(old)
     if len(pending)>=max(25,int(batch_size)):flush()
     if progress and checked-last_report>=500:
      progress({'checked':checked,'new':new_count,'updated':updated,'offline':offline});last_report=checked
  flush()
  # Remove missing tracks only for roots that were actually online and fully
  # traversed. An unplugged disk or broken root symlink must never empty a library.
  remove=[]
  for path,(rid,_) in existing.items():
   try:inside=any(Path(path).is_relative_to(r) for r in roots)
   except Exception:inside=any(str(path).startswith(str(r)+os.sep) or str(path)==str(r) for r in roots)
   if inside and path not in seen:remove.append((rid,))
  if remove:self._executemany('DELETE FROM tracks WHERE id=?',remove)
  total=self.count();self.last_scan={'checked':checked,'new':new_count,'updated':updated,'removed':len(remove),'total':total,'offline':offline,'roots':[str(x) for x in roots]}
  if progress:progress(dict(self.last_scan,done=True))
  return self.last_scan
 def _embedded_cover(self,f):
  """Extract embedded artwork to NPLAY cache without modifying the audio file."""
  try:
   from mutagen import File
   audio=File(str(f),easy=False)
   if not audio:return ''
   data=None;mime='image/jpeg'
   # FLAC
   pics=getattr(audio,'pictures',None)
   if pics:
    pic=pics[0];data=pic.data;mime=getattr(pic,'mime',mime) or mime
   # MP3 / ID3 APIC
   if data is None:
    tags=getattr(audio,'tags',None)
    if tags is not None and hasattr(tags,'getall'):
     aps=tags.getall('APIC')
     if aps:
      pic=next((x for x in aps if getattr(x,'type',None)==3),aps[0]);data=pic.data;mime=getattr(pic,'mime',mime) or mime
   # MP4 / M4A covr
   if data is None:
    tags=getattr(audio,'tags',None)
    covr=tags.get('covr') if tags and hasattr(tags,'get') else None
    if covr:
     item=covr[0];data=bytes(item)
     fmt=getattr(item,'imageformat',None)
     if fmt==14:mime='image/png'
   # Ogg/Opus METADATA_BLOCK_PICTURE (base64 FLAC Picture block)
   if data is None:
    tags=getattr(audio,'tags',None);vals=tags.get('metadata_block_picture') if tags and hasattr(tags,'get') else None
    if vals:
     from mutagen.flac import Picture
     pic=Picture(base64.b64decode(vals[0]));data=pic.data;mime=pic.mime or mime
   if not data:return ''
   ext='.png' if 'png' in str(mime).lower() else '.jpg'
   cache=DATA/'artwork'/'embedded';cache.mkdir(parents=True,exist_ok=True)
   try:stamp=f.stat().st_mtime_ns
   except OSError:stamp=0
   base=hashlib.sha256(str(f.resolve()).encode()).hexdigest();out=cache/(base+'-'+str(stamp)+ext)
   if not out.exists() or out.stat().st_size!=len(data):
    tmp=out.with_suffix(out.suffix+'.tmp');tmp.write_bytes(data);tmp.replace(out)
   # One extracted generation per source file; tag/cover changes cannot leak cache forever.
   for old in cache.glob(base+'-*'):
    if old!=out:
     try:old.unlink()
     except OSError:pass
   return str(out)
  except Exception:return ''
 def _track(self,r):
  f=Path(r['path']);cover=''
  # An explicit folder cover is intentional and therefore wins over embedded art.
  for n in COVERS:
   q=f.parent/n
   if q.is_file():cover=str(q);break
  if not cover:cover=self._embedded_cover(f)
  return Track(source='local',id=r['id'],title=r['title'],artist=r['artist'],album=r['album'],duration=float(r['duration'] or 0),path=r['path'],url=r['path'],cover=cover,meta={'suffix':f.suffix.lower().lstrip('.'),'album_artist':r['album_artist'] or '','track_no':int(r['track_no'] or 0),'disc_no':int(r['disc_no'] or 0),'year':int(r['year'] or 0),'genre':r['genre'] or '','codec':r['codec'] or '','sample_rate':int(r['sample_rate'] or 0),'bit_depth':int(r['bit_depth'] or 0),'channels':int(r['channels'] or 0),'bitrate':int(r['bitrate'] or 0)})
 def search(self,q='',limit=500):
  with self.con() as c:
   if q:rows=c.execute("SELECT * FROM tracks WHERE title LIKE ? OR artist LIKE ? OR album_artist LIKE ? OR album LIKE ? ORDER BY coalesce(nullif(album_artist,''),artist) COLLATE NOCASE,year,album COLLATE NOCASE,disc_no,track_no,title COLLATE NOCASE LIMIT ?",(*(f'%{q}%',)*4,limit)).fetchall()
   else:rows=c.execute("SELECT * FROM tracks ORDER BY coalesce(nullif(album_artist,''),artist) COLLATE NOCASE,year,album COLLATE NOCASE,disc_no,track_no,title COLLATE NOCASE LIMIT ?",(limit,)).fetchall()
  return [self._track(r) for r in rows]
 def random_tracks(self,limit=50):
  with self.con() as c:rows=c.execute('SELECT * FROM tracks ORDER BY RANDOM() LIMIT ?',(limit,)).fetchall()
  return [self._track(r) for r in rows]
 def find_artists(self,q='',limit=50):
  q=(q or '').strip()
  with self.con() as c:
   if q:
    rows=c.execute("SELECT coalesce(nullif(album_artist,''),artist) artist,count(*) n FROM tracks WHERE trim(coalesce(nullif(album_artist,''),artist))<>'' AND coalesce(nullif(album_artist,''),artist) LIKE ? COLLATE NOCASE GROUP BY coalesce(nullif(album_artist,''),artist) ORDER BY artist COLLATE NOCASE LIMIT ?",(f'%{q}%',limit)).fetchall()
   else:
    rows=c.execute("SELECT coalesce(nullif(album_artist,''),artist) artist,count(*) n FROM tracks WHERE trim(coalesce(nullif(album_artist,''),artist))<>'' GROUP BY coalesce(nullif(album_artist,''),artist) ORDER BY artist COLLATE NOCASE LIMIT ?",(limit,)).fetchall()
  return [(r['artist'],int(r['n'] or 0)) for r in rows]
 def random_artist_tracks(self):
  with self.con() as c:
   r=c.execute("SELECT coalesce(nullif(album_artist,''),artist) artist FROM tracks WHERE trim(coalesce(nullif(album_artist,''),artist))<>'' GROUP BY coalesce(nullif(album_artist,''),artist) ORDER BY RANDOM() LIMIT 1").fetchone()
  return (r['artist'],self.tracks_by_artist(r['artist'])) if r else ('',[])
 def shuffled_artist_tracks(self,name):
  xs=self.tracks_by_artist(name);import random;random.shuffle(xs);return xs
 def recently_added(self,limit=100):
  with self.con() as c:rows=c.execute('SELECT * FROM tracks ORDER BY coalesce(added,mtime) DESC, artist,album,title LIMIT ?',(limit,)).fetchall()
  return [self._track(r) for r in rows]
 def random_album_tracks(self):
  with self.con() as c:
   a=c.execute("SELECT coalesce(nullif(album_artist,''),artist) artist,album FROM tracks WHERE trim(album)<>'' GROUP BY coalesce(nullif(album_artist,''),artist),album ORDER BY RANDOM() LIMIT 1").fetchone()
   if not a:return []
  return self.tracks_by_album(a['artist'],a['album'])
 def resume_set(self,item_id,pos):
  if not item_id:return
  self._write(lambda c:c.execute('INSERT OR REPLACE INTO resume(item_id,pos,updated) VALUES(?,?,?)',(item_id,max(0,float(pos or 0)),time.time())))
 def resume_get(self,item_id,legacy_id=None):
  if not item_id:return 0.0
  with self.con() as c:
   r=c.execute('SELECT pos FROM resume WHERE item_id=?',(item_id,)).fetchone()
   if not r and legacy_id:r=c.execute('SELECT pos FROM resume WHERE item_id=?',(legacy_id,)).fetchone()
   return float(r[0] or 0) if r else 0.0
 def schema_version(self):
  with self.con() as c:return int(c.execute('PRAGMA user_version').fetchone()[0])
 def favorite(self,t):
  key=f'{t.source}:{t.id}'
  with self.con() as c:
   row=c.execute('SELECT id FROM favorites WHERE id IN (?,?) ORDER BY CASE WHEN id=? THEN 0 ELSE 1 END LIMIT 1',(key,t.id,key)).fetchone()
   if row:c.execute('DELETE FROM favorites WHERE id=?',(row['id'],));return False
   c.execute('INSERT INTO favorites VALUES(?,?,?)',(key,json.dumps(t.dict()),time.time()));return True
 def favorites(self):
  with self.con() as c:rows=c.execute('SELECT data FROM favorites ORDER BY added DESC').fetchall()
  return [Track.from_dict(json.loads(r[0])) for r in rows]
 def history_add(self,t):
  with self.con() as c:c.execute('INSERT INTO history(item_id,data,played) VALUES(?,?,?)',(f'{t.source}:{t.id}',json.dumps(t.dict()),time.time()))
 def history(self,limit=200):
  with self.con() as c:rows=c.execute('SELECT data FROM history ORDER BY id DESC LIMIT ?',(limit,)).fetchall()
  return [Track.from_dict(json.loads(r[0])) for r in rows]
 def playlists(self):
  with self.con() as c:return [(r['name'],f"playlist:{r['id']}",f"{r['n']} tracks") for r in c.execute('SELECT p.id,p.name,count(i.id) n FROM playlists p LEFT JOIN playlist_items i ON i.playlist_id=p.id GROUP BY p.id ORDER BY p.name')]
 def playlist_create(self,name):
  name=name.strip()
  if not name:return None
  with self.con() as c:c.execute('INSERT OR IGNORE INTO playlists(name,created) VALUES(?,?)',(name,time.time()));return c.execute('SELECT id FROM playlists WHERE name=?',(name,)).fetchone()[0]
 def playlist_add(self,pid,t):
  with self.con() as c:
   pos=c.execute('SELECT coalesce(max(position),-1)+1 FROM playlist_items WHERE playlist_id=?',(pid,)).fetchone()[0];c.execute('INSERT INTO playlist_items(playlist_id,data,position,added) VALUES(?,?,?,?)',(pid,json.dumps(t.dict()),pos,time.time()))
 def playlist_tracks(self,pid):
  with self.con() as c:rows=c.execute('SELECT data FROM playlist_items WHERE playlist_id=? ORDER BY position,id',(pid,)).fetchall()
  return [Track.from_dict(json.loads(r[0])) for r in rows]

 def local_artists(self):
  with self.con() as c:rows=c.execute("SELECT coalesce(nullif(album_artist,''),artist) artist,count(*) n FROM tracks WHERE trim(coalesce(nullif(album_artist,''),artist))<>'' GROUP BY coalesce(nullif(album_artist,''),artist) ORDER BY artist COLLATE NOCASE").fetchall()
  return [(r['artist'],f"local:artist:{r['artist']}",f"{r['n']} tracks") for r in rows]
 def local_albums(self):
  with self.con() as c:rows=c.execute("SELECT album,coalesce(nullif(album_artist,''),artist) artist,count(*) n,min(year) year FROM tracks WHERE trim(album)<>'' GROUP BY album,coalesce(nullif(album_artist,''),artist) ORDER BY artist COLLATE NOCASE,year,album COLLATE NOCASE").fetchall()
  return [(r['album'],f"local:album:{r['artist']}\t{r['album']}",f"{r['artist']} · {r['n']} tracks") for r in rows]
 def local_folders(self):
  with self.con() as c:rows=c.execute('SELECT path FROM tracks ORDER BY path').fetchall()
  d={}
  for r in rows:d[str(Path(r['path']).parent)]=d.get(str(Path(r['path']).parent),0)+1
  return [(Path(k).name or k,'local:folder:'+k,f'{v} tracks · {k}') for k,v in sorted(d.items(),key=lambda x:x[0].casefold())]
 def tracks_by_artist(self,name):
  with self.con() as c:rows=c.execute('SELECT * FROM tracks WHERE artist=? OR album_artist=? ORDER BY year,album,disc_no,track_no,title,path',(name,name)).fetchall()
  return [self._track(r) for r in rows]
 def tracks_by_album(self,artist,album):
  with self.con() as c:rows=c.execute('SELECT * FROM tracks WHERE album=? AND (artist=? OR album_artist=?) ORDER BY disc_no,track_no,title,path',(album,artist,artist)).fetchall()
  return [self._track(r) for r in rows]
 def tracks_by_folder(self,path):
  with self.con() as c:rows=c.execute('SELECT * FROM tracks WHERE path LIKE ? ORDER BY artist,album,title',(str(Path(path)) + os.sep + '%',)).fetchall()
  return [self._track(r) for r in rows]
 def playlist_name(self,pid):
  with self.con() as c:
   r=c.execute('SELECT name FROM playlists WHERE id=?',(pid,)).fetchone();return r[0] if r else 'Playlist'
 def playlist_delete(self,pid):
  with self.con() as c:c.execute('DELETE FROM playlist_items WHERE playlist_id=?',(pid,));c.execute('DELETE FROM playlists WHERE id=?',(pid,))
 def playlist_rename(self,pid,name):
  name=name.strip()
  if name:
   with self.con() as c:c.execute('UPDATE playlists SET name=? WHERE id=?',(name,pid))
 def playlist_remove(self,pid,index):
  with self.con() as c:
   rows=c.execute('SELECT id FROM playlist_items WHERE playlist_id=? ORDER BY position,id',(pid,)).fetchall()
   if 0<=index<len(rows):c.execute('DELETE FROM playlist_items WHERE id=?',(rows[index][0],))
 def playlist_move(self,pid,index,delta):
  with self.con() as c:
   rows=c.execute('SELECT id FROM playlist_items WHERE playlist_id=? ORDER BY position,id',(pid,)).fetchall();j=max(0,min(len(rows)-1,index+delta))
   if not rows or j==index:return index
   a,b=rows[index][0],rows[j][0];c.execute('UPDATE playlist_items SET position=-1 WHERE id=?',(a,));c.execute('UPDATE playlist_items SET position=? WHERE id=?',(index,b));c.execute('UPDATE playlist_items SET position=? WHERE id=?',(j,a));return j

 def rating_get(self,item_id,source='local'):
  with self.con() as c:
   key=(str(source)+':'+str(item_id)) if ':' not in str(item_id) else str(item_id);r=c.execute('SELECT rating FROM ratings WHERE item_id IN (?,?) ORDER BY CASE WHEN item_id=? THEN 0 ELSE 1 END LIMIT 1',(key,str(item_id),key)).fetchone();return int(r[0]) if r else 0
 def rating_set(self,item_id,rating,source='local'):
  rating=max(0,min(5,int(rating)))
  if not item_id:return
  item_id=(str(source)+':'+str(item_id)) if ':' not in str(item_id) else str(item_id)
  if rating:self._write(lambda c:c.execute('INSERT OR REPLACE INTO ratings(item_id,rating,updated) VALUES(?,?,?)',(item_id,rating,time.time())))
  else:self._write(lambda c:c.execute('DELETE FROM ratings WHERE item_id=?',(item_id,)))
 def discovery_add(self,title,artist='',source='radio',data=None):
  title=str(title or '').strip();artist=str(artist or '').strip()
  if not title:return False
  payload=json.dumps(data or {},ensure_ascii=False)
  self._write(lambda c:c.execute('INSERT INTO discoveries(title,artist,source,data,added) VALUES(?,?,?,?,?)',(title,artist,source,payload,time.time())))
  return True
 def discoveries(self,limit=300):
  from .model import Track
  with self.con() as c:rows=c.execute('SELECT * FROM discoveries ORDER BY id DESC LIMIT ?',(limit,)).fetchall()
  return [Track(source='discovery',id=f"discovery:{r['id']}",title=r['title'],artist=r['artist'],kind='discovery',seekable=False,meta={'origin':r['source'],'added':r['added'],**(json.loads(r['data'] or '{}'))}) for r in rows]
 def most_played(self,limit=100):
  with self.con() as c:rows=c.execute('SELECT data,count(*) n,max(played) last FROM history GROUP BY item_id ORDER BY n DESC,last DESC LIMIT ?',(limit,)).fetchall()
  out=[]
  for r in rows:
   try:
    t=Track.from_dict(json.loads(r['data']));t.meta={**(t.meta or {}),'play_count':int(r['n'])};out.append(t)
   except Exception:pass
  return out
 def never_played(self,limit=200):
  with self.con() as c:rows=c.execute("SELECT * FROM tracks WHERE ('local:'||id) NOT IN (SELECT DISTINCT item_id FROM history) AND id NOT IN (SELECT DISTINCT item_id FROM history) ORDER BY coalesce(nullif(album_artist,''),artist) COLLATE NOCASE,year,album COLLATE NOCASE,disc_no,track_no,title COLLATE NOCASE LIMIT ?",(limit,)).fetchall()
  return [self._track(r) for r in rows]
 def unplayed_albums(self,limit=100):
  with self.con() as c:
   rows=c.execute("SELECT artist,album,count(*) n FROM tracks WHERE trim(album)<>'' GROUP BY artist,album HAVING sum(CASE WHEN (('local:'||id) IN (SELECT DISTINCT item_id FROM history) OR id IN (SELECT DISTINCT item_id FROM history)) THEN 1 ELSE 0 END)=0 ORDER BY artist,album LIMIT ?",(limit,)).fetchall()
  return [(r['album'],f"local:album:{r['artist']}\t{r['album']}",f"{r['artist']} · {r['n']} tracks · unplayed") for r in rows]
 def rated(self,min_rating=4,limit=200):
  with self.con() as c:rows=c.execute("SELECT t.* FROM tracks t JOIN ratings r ON r.item_id IN (t.id,'local:'||t.id) WHERE r.rating>=? ORDER BY r.rating DESC,t.artist,t.album,t.title LIMIT ?",(min_rating,limit)).fetchall()
  return [self._track(r) for r in rows]
 def play_started(self,t):
  if not t:return None
  key=f'{t.source}:{t.id}'
  return self._write(lambda c:c.execute('INSERT INTO play_events(item_key,data,started,seconds,completed) VALUES(?,?,?,?,0)',(key,json.dumps(t.dict()),time.time(),0)).lastrowid)
 def play_finished(self,event_id,seconds,completed=False):
  if not event_id:return
  self._write(lambda c:c.execute('UPDATE play_events SET ended=?,seconds=max(seconds,?),completed=? WHERE id=?',(time.time(),max(0,float(seconds or 0)),1 if completed else 0,event_id)))
 def listening_stats(self,days=7):
  cutoff=time.time()-days*86400
  with self.con() as c:rows=c.execute('SELECT data,seconds,completed FROM play_events WHERE started>=? ORDER BY started DESC',(cutoff,)).fetchall()
  tracks=[];artists={};sources={};seconds=0;completed=0;substantial=0
  for r in rows:
   try:
    t=Track.from_dict(json.loads(r['data']));tracks.append(t);heard=float(r['seconds'] or 0);seconds+=heard;completed+=int(r['completed'] or 0);is_sub=heard>=30 or (float(t.duration or 0)>0 and heard>=float(t.duration)*.5);substantial+=int(is_sub)
    if is_sub and t.artist:artists[t.artist]=artists.get(t.artist,0)+1
    if is_sub:sources[t.source]=sources.get(t.source,0)+1
   except Exception:pass
  return {'plays':len(tracks),'substantial':substantial,'completed':completed,'minutes':int(seconds/60),'artists':sorted(artists.items(),key=lambda x:(-x[1],x[0].casefold()))[:20],'sources':sorted(sources.items(),key=lambda x:-x[1])}
 def radios(self):
  with self.con() as c:rows=c.execute('SELECT * FROM radio_stations ORDER BY name').fetchall()
  return [Track(source='radio',id=f"custom:{r['id']}",title=r['name'],artist='Custom radio',url=r['url'],cover=r['artwork'] or '',kind='radio',seekable=False,meta={'homepage':r['homepage'] or ''}) for r in rows]
 def radio_add(self,name,url,homepage='',artwork=''):
  if not name.strip() or not url.startswith(('http://','https://')):raise ValueError('Station needs a name and an http(s) stream URL')
  with self.con() as c:c.execute('INSERT OR REPLACE INTO radio_stations(name,url,homepage,artwork,added) VALUES(?,?,?,?,?)',(name.strip(),url.strip(),homepage.strip(),artwork.strip(),time.time()))
