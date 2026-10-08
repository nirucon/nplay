from . import __version__
import urllib.request,urllib.parse,json,subprocess,shutil,hashlib,secrets,re,html,gzip,time,os
from pathlib import Path
from .config import DATA
from .model import Track

def _published_meta(x):
 """Return stable publication metadata without forcing an extra network request."""
 raw=x.get('timestamp') or x.get('release_timestamp') or 0
 try: ts=float(raw or 0)
 except Exception: ts=0
 date=str(x.get('upload_date') or x.get('release_date') or '')
 if not date and ts:
  try: date=time.strftime('%Y-%m-%d',time.localtime(ts))
  except Exception: date=''
 elif len(date)==8 and date.isdigit(): date=f'{date[:4]}-{date[4:6]}-{date[6:8]}'
 return {'published_ts':ts,'published_date':date}

def _sr_date(e):
 raw=e.get('publishdateutc') or e.get('publishdate') or (e.get('broadcasttime',{}).get('starttimeutc') if isinstance(e.get('broadcasttime',{}),dict) else '')
 text=str(raw or '')
 m=re.search(r'/Date\((\d+)',text)
 ts=float(m.group(1))/1000 if m else 0
 if not ts and text:
  try:
   from datetime import datetime
   ts=datetime.fromisoformat(text.replace('Z','+00:00')).timestamp()
  except Exception: pass
 return {'published_ts':ts,'published_date':time.strftime('%Y-%m-%d',time.localtime(ts)) if ts else ''}

def _json(url,timeout=8):
 req=urllib.request.Request(url,headers={'User-Agent':f'NPLAY/{__version__} (+terminal audio player)'})
 with urllib.request.urlopen(req,timeout=timeout) as r:return json.load(r)

class Navidrome:
 def __init__(self,url,user,password,bitrate=0):self.url=url.rstrip('/');self.user=user;self.password=password;self.bitrate=bitrate
 def params(self):
  salt=secrets.token_hex(6);token=hashlib.md5((self.password+salt).encode()).hexdigest();return {'u':self.user,'t':token,'s':salt,'v':'1.16.1','c':'nplay','f':'json'}
 def call(self,m,x=None):
  q={**self.params(),**(x or {})};u=f'{self.url}/rest/{m}.view?'+urllib.parse.urlencode(q)
  d=_json(u).get('subsonic-response',{})
  if d.get('status')!='ok':raise RuntimeError(d.get('error',{}).get('message','Navidrome/Subsonic error'))
  return d
 def ping(self):self.call('ping');return True
 def stats(self):
  d=self.call('getIndexes'); return {'version': d.get('version','Subsonic-compatible')}
 def recent(self,limit=50):
  d=self.call('getAlbumList2',{'type':'newest','size':limit}).get('albumList2',{}).get('album',[])
  out=[]
  for a in d:
   try:
    songs=self.call('getMusicDirectory',{'id':a.get('id')}).get('directory',{}).get('child',[])
    out.extend(Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}) for x in songs if not x.get('isDir'))
   except Exception: pass
  return out[:limit]
 def stream(self,i):return f'{self.url}/rest/stream.view?'+urllib.parse.urlencode({**self.params(),'id':i,**({'maxBitRate':self.bitrate} if self.bitrate else {})})
 def song(self,i):
  x=self.call('getSong',{'id':i}).get('song',{})
  if not x:raise RuntimeError('Navidrome track no longer exists')
  return Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)})
 def cover(self,i):return f'{self.url}/rest/getCoverArt.view?'+urllib.parse.urlencode({**self.params(),'id':i}) if i else ''
 def albums(self,kind='newest',limit=40):
  d=self.call('getAlbumList2',{'type':kind,'size':limit}).get('albumList2',{}).get('album',[])
  return [Track(source='navidrome',id=str(a.get('id','')),title=a.get('name',''),artist=a.get('artist',''),album=a.get('name',''),cover=self.cover(a.get('coverArt','')),kind='album',seekable=False,meta={'album_id':str(a.get('id','')),'song_count':a.get('songCount',0),'year':a.get('year')}) for a in d]
 def album_tracks(self,album_id):
  a=self.call('getAlbum',{'id':album_id}).get('album',{}); out=[]
  for x in a.get('song',[]) or []:
   out.append(Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt') or a.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}))
  return out
 def search_library(self,q,songs=30,albums=15,artists=10):
  d=self.call('search3',{'query':q,'songCount':songs,'albumCount':albums,'artistCount':artists}).get('searchResult3',{})
  out=[]
  for a in d.get('artist',[]) or []: out.append(Track(source='navidrome',id=str(a.get('id','')),title=a.get('name',''),artist='Artist',kind='artist',seekable=False,cover=self.cover(a.get('coverArt','')),meta={'artist_id':str(a.get('id',''))}))
  for a in d.get('album',[]) or []: out.append(Track(source='navidrome',id=str(a.get('id','')),title=a.get('name',''),artist=a.get('artist',''),album=a.get('name',''),kind='album',seekable=False,cover=self.cover(a.get('coverArt','')),meta={'album_id':str(a.get('id','')),'song_count':a.get('songCount',0),'year':a.get('year')}))
  for x in d.get('song',[]) or []: out.append(Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}))
  return out
 def artists(self):
  d=self.call('getArtists').get('artists',{}).get('index',[]) or [];out=[]
  for idx in d:
   for a in idx.get('artist',[]) or []:out.append(Track(source='navidrome',id=str(a.get('id','')),title=a.get('name',''),artist='Artist',kind='artist',seekable=False,meta={'artist_id':str(a.get('id',''))}))
  return out
 def artist_albums(self,artist_id):
  a=self.call('getArtist',{'id':artist_id}).get('artist',{});return [Track(source='navidrome',id=str(x.get('id','')),title=x.get('name',''),artist=a.get('name',''),album=x.get('name',''),cover=self.cover(x.get('coverArt','')),kind='album',seekable=False,meta={'album_id':str(x.get('id','')),'song_count':x.get('songCount',0),'year':x.get('year')}) for x in a.get('album',[]) or []]
 def playlists(self):
  xs=self.call('getPlaylists').get('playlists',{}).get('playlist',[]) or [];return [Track(source='navidrome',id=str(x.get('id','')),title=x.get('name',''),artist='Playlist',kind='playlist',seekable=False,meta={'playlist_id':str(x.get('id','')),'song_count':x.get('songCount',0)}) for x in xs]
 def playlist_tracks(self,pid):
  p=self.call('getPlaylist',{'id':pid}).get('playlist',{});out=[]
  for x in p.get('entry',[]) or []:out.append(Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}))
  return out
 def search(self,q,limit=50):
  d=self.call('search3',{'query':q,'songCount':limit,'albumCount':0,'artistCount':0}).get('searchResult3',{}).get('song',[])
  return [Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}) for x in d]
 def random_tracks(self,limit=50):
  d=self.call('getRandomSongs',{'size':limit}).get('randomSongs',{}).get('song',[]) or []
  return [Track(source='navidrome',id=str(x.get('id','')),title=x.get('title',''),artist=x.get('artist',''),album=x.get('album',''),duration=x.get('duration',0),url=self.stream(x.get('id')),cover=self.cover(x.get('coverArt','')),meta={'suffix':x.get('suffix',''),'bitrate':x.get('bitRate',0)}) for x in d]
 def artist_tracks(self,artist_id,shuffle=False):
  a=self.call('getArtist',{'id':artist_id}).get('artist',{});out=[]
  albums=sorted(a.get('album',[]) or [],key=lambda x:(int(x.get('year') or 0),str(x.get('name') or '').casefold()))
  for album in albums:out.extend(self.album_tracks(str(album.get('id',''))))
  if shuffle:
   import random;random.shuffle(out)
  return out
 def random_artist_tracks(self):
  artists=self.artists()
  if not artists:return ('',[])
  artist=secrets.choice(artists);return (artist.title,self.artist_tracks(artist.meta.get('artist_id') or artist.id,False))

class SR:
 API='https://api.sr.se/api/v2'; WEB='https://www.sverigesradio.se'
 def __init__(self):
  self.cache=DATA/'sr';self.cache.mkdir(parents=True,exist_ok=True);self.last_cache=False
 ST=[('P1',132),('P2',163),('P3',164),('P4 Väst',212)]
 def _request(self,url,timeout=8):
  req=urllib.request.Request(url,headers={'User-Agent':f'NPLAY/{__version__} (+terminal audio player)','Accept':'application/json,text/html;q=0.9,*/*;q=0.8','Accept-Encoding':'gzip'})
  with urllib.request.urlopen(req,timeout=timeout) as r:
   raw=r.read()
   if raw[:2]==b'\x1f\x8b':
    try:raw=gzip.decompress(raw)
    except Exception:pass
   return raw
 def get(self,path,params=None):
  q={'format':'json','size':100};q.update(params or {})
  return json.loads(self._request(self.API+path+'?'+urllib.parse.urlencode(q)).decode('utf-8','replace'))
 def live(self):
  return [Track(source='sr',id=f'sr:{i}',title=name,artist='Sveriges Radio',url=f'https://sverigesradio.se/topsy/direkt/{i}-hi-mp3.pls',kind='radio',seekable=False) for name,i in self.ST]
 def _web_search(self,q,limit=20):
  raw=self._request(self.WEB+'/sok?'+urllib.parse.urlencode({'query':q}),6).decode('utf-8','replace');out=[];seen=set()
  decoded=html.unescape(raw).replace('\\/','/').replace('\\u002F','/').replace('\\u002f','/')
  for m in re.finditer(r"<a\b[^>]*href=['\"](?P<href>(?:https://www\.sverigesradio\.se)?/avsnitt/[^'\"#?]+)['\"][^>]*>(?P<body>.*?)</a>",decoded,re.I|re.S):
   href=m.group('href');href=href if href.startswith('http') else self.WEB+href
   if href in seen:continue
   title=html.unescape(re.sub(r'\s+',' ',re.sub(r'<[^>]+>',' ',m.group('body')))).strip()
   if len(title)<3:continue
   seen.add(href);slug=urllib.parse.unquote(urllib.parse.urlparse(href).path.rstrip('/').split('/')[-1])
   out.append(Track(source='sr',id='web:'+slug,title=title,artist='Sveriges Radio',kind='podcast',seekable=True,meta={'webpage':href,'needs_resolve':True}))
   if len(out)>=limit:return out
  if len(out)<limit:
   for m in re.finditer(r'(?:https://www\.sverigesradio\.se)?/avsnitt/[a-zA-Z0-9_%\-]+',decoded,re.I):
    href=m.group(0);href=href if href.startswith('http') else self.WEB+href
    if href in seen:continue
    seen.add(href);slug=urllib.parse.unquote(urllib.parse.urlparse(href).path.rstrip('/').split('/')[-1]);title=re.sub(r'[-_]+',' ',slug).strip().capitalize()
    if title:out.append(Track(source='sr',id='web:'+slug,title=title,artist='Sveriges Radio',kind='podcast',seekable=True,meta={'webpage':href,'needs_resolve':True}))
    if len(out)>=limit:break
  return out
 def _web_discovery(self,q,limit=30):
  """Best-effort enrichment from SR's public search page.
  The official Open API remains primary; this only adds public programmes/episodes
  that SR exposes on sverigesradio.se but that are missing from API search results.
  """
  if not str(q or '').strip():return []
  try:raw=self._request(self.WEB+'/sok?'+urllib.parse.urlencode({'query':q}),7).decode('utf-8','replace')
  except Exception:return []
  text=html.unescape(raw).replace('\\u002F','/').replace('\\u002f','/').replace('\\/','/')
  out=[];seen=set()
  # Search result pages can expose both /avsnitt/... and /program/... links.
  for kind,pat in (
   ('podcast',r'href=["\']((?:https://www\.sverigesradio\.se)?/avsnitt/[^"\'#?]+)["\']'),
   ('program',r'href=["\']((?:https://www\.sverigesradio\.se)?/(?:program|podd)/[^"\'#?]+)["\']')):
   for m in re.finditer(pat,text,re.I):
    href=m.group(1);href=href if href.startswith('http') else self.WEB+href
    if href in seen:continue
    seen.add(href);pos=m.start();window=text[max(0,pos-500):min(len(text),pos+900)]
    clean=re.sub(r'\s+',' ',re.sub(r'<[^>]+>',' ',window));slug=urllib.parse.unquote(urllib.parse.urlparse(href).path.rstrip('/').split('/')[-1]);title=re.sub(r'[-_]+',' ',slug).strip()
    # Prefer nearby aria-label/title text if available, otherwise readable slug.
    attrs=text[max(0,pos-350):pos+350]
    candidates=re.findall(r'(?:aria-label|title)=["\']([^"\']{3,180})["\']',attrs,re.I)
    if candidates:title=html.unescape(candidates[-1]).strip()
    if not title:continue
    if kind=='program':
     out.append(Track(source='sr',id='sr:webprogram:'+slug,title=title,artist='Sveriges Radio',album='Program',kind='program',seekable=False,meta={'webpage':href,'web_discovery':True,'description':clean[:400]}))
    else:
     out.append(Track(source='sr',id='sr:webepisode:'+slug,title=title,artist='Sveriges Radio',album='SR Podcast',kind='podcast',seekable=True,meta={'webpage':href,'needs_resolve':True,'web_discovery':True}))
    if len(out)>=limit:return out
  return out
 def search_all(self,q,limit=40):
  q=str(q or '').strip()
  if len(q)<2:return []
  groups=[]
  try:groups.append(self.programs(q,min(12,limit)))
  except Exception:groups.append([])
  try:groups.append(self._legacy_episode_search(q,min(24,limit)))
  except Exception:groups.append([])
  try:groups.append(self._web_discovery(q,min(20,limit)))
  except Exception:groups.append([])
  out=[];seen=set()
  for group in groups:
   for t in group:
    key=(t.kind,(t.title or '').strip().casefold(),(t.artist or '').strip().casefold())
    if key in seen:continue
    seen.add(key);out.append(t)
    if len(out)>=limit:return out
  return out
 def _program_search(self,q,limit=10):
  try:data=self.get('/programs',{'size':500})
  except Exception:return []
  needle=str(q or '').casefold();out=[]
  for p in data.get('programs',[]) or []:
   text=(str(p.get('name') or '')+' '+str(p.get('description') or '')).casefold()
   if needle and needle not in text:continue
   pid=p.get('id');name=p.get('name','Sveriges Radio')
   try:eps=self.get('/episodes/index',{'programid':pid,'pagesize':4,'size':4,'audioquality':'hi'}).get('episodes',[])
   except Exception:eps=[]
   for e in eps:
    audio=e.get('listenpodfile') or e.get('downloadpodfile') or {};url=audio.get('url','') if isinstance(audio,dict) else ''
    cover=e.get('imageurltemplate') or e.get('imageurl') or p.get('programimage','')
    out.append(Track(source='sr',id='sr:episode:'+str(e.get('id','')),title=e.get('title') or e.get('name') or 'Avsnitt',artist=name,album='SR Podcast',duration=float((audio.get('duration') if isinstance(audio,dict) else 0) or e.get('duration') or 0),url=url,cover=cover,kind='podcast',seekable=True,meta={'description':re.sub('<[^>]+>','',e.get('description','')),'needs_resolve':not bool(url),**_sr_date(e)}))
    if len(out)>=limit:return out
  return out
 def _episode_track(self,e):
  if not isinstance(e,dict):return None
  pod=e.get('listenpodfile') if isinstance(e.get('listenpodfile'),dict) else {}
  if not pod and isinstance(e.get('downloadpodfile'),dict):pod=e.get('downloadpodfile')
  program=e.get('program') if isinstance(e.get('program'),dict) else {}
  cover=e.get('imageurltemplate') or e.get('imageurl') or e.get('programimage') or ''
  if cover:cover=str(cover).replace('{format}','512x512').replace('{width}','512').replace('{height}','512')
  return Track(source='sr',id='sr:episode:'+str(e.get('id','')),title=str(e.get('title') or e.get('name') or 'Avsnitt'),artist=str(program.get('name') or 'Sveriges Radio'),album='SR Podcast',duration=float(pod.get('duration') or e.get('duration') or 0),url=str(pod.get('url') or ''),cover=cover,kind='podcast',seekable=True,meta={'description':re.sub('<[^>]+>','',str(e.get('description') or '')),'needs_resolve':not bool(pod.get('url')),**_sr_date(e)})
 def _legacy_episode_search(self,q,limit=20):
  try:
   data=self.get('/episodes/search',{'query':q,'size':min(25,limit),'audioquality':'hi'})
  except Exception:return []
  out=[]
  for e in data.get('episodes',[]) or []:
   t=self._episode_track(e)
   if t:out.append(t)
  return out[:limit]
 def search(self,q,limit=30):
  # Same resilient strategy as NIRUPLAY: every SR endpoint is independent.
  # A 403 from SR Play must never discard working Open API/program results.
  out=[];seen=set()
  sources=[]
  sources.append(self._legacy_episode_search(q,min(limit,20)))
  try:sources.append(self._program_search(q,min(12,limit)))
  except Exception:sources.append([])
  try:sources.append(self._web_search(q,min(12,limit)))
  except Exception:sources.append([])
  for source in sources:
   for t in source:
    key=t.id or (t.title.casefold(),t.artist.casefold())
    if key in seen:continue
    seen.add(key);out.append(t)
    if len(out)>=limit:return out
  return out
 def _program_catalogue(self):
  cache=self.cache/'programs.json';data=None;self.last_cache=False
  if cache.exists() and time.time()-cache.stat().st_mtime < 21600:
   try:data=json.loads(cache.read_text())
   except Exception:pass
  if data is None:
   try:
    data=self.get('/programs',{'size':500});tmp=cache.with_suffix('.tmp');tmp.write_text(json.dumps(data));tmp.replace(cache)
   except Exception:
    if cache.exists():data=json.loads(cache.read_text());self.last_cache=True
    else:raise
  return data
 def programs(self,q='',limit=100):
  data=self._program_catalogue();needle=str(q or '').strip().casefold();ranked=[]
  for p in data.get('programs',[]) or []:
   name=str(p.get('name') or '');desc=re.sub('<[^>]+>','',str(p.get('description') or ''));n=name.casefold();d=desc.casefold()
   if needle:
    if n==needle:score=0
    elif n.startswith(needle):score=1
    elif re.search(r'(^|\W)'+re.escape(needle)+r'($|\W)',n):score=2
    elif needle in n:score=3
    elif needle in d:score=5
    else:continue
   else:score=0
   cover=p.get('programimage') or p.get('programimagetemplate') or ''
   if cover:cover=str(cover).replace('{format}','512x512').replace('{width}','512').replace('{height}','512')
   t=Track(source='sr',id='sr:program:'+str(p.get('id','')),title=name,artist='Sveriges Radio',album='Program',cover=cover,kind='program',seekable=False,meta={'program_id':str(p.get('id','')),'description':desc})
   ranked.append((score,name.casefold(),t))
  ranked.sort(key=lambda x:(x[0],x[1]));return [x[2] for x in ranked[:limit]]
 def program_episodes(self,pid,limit=50):
  data=self.get('/episodes/index',{'programid':pid,'size':limit,'audioquality':'hi'});out=[]
  for e in data.get('episodes',[]) or []:
   t=self._episode_track(e)
   if t:out.append(t)
  out.sort(key=lambda t:float((t.meta or {}).get('published_ts') or 0),reverse=True)
  return out
 def search_programmes_and_episodes(self,q,limit=30):
  return self.search_all(q,limit)
 def live_info(self,station_id):
  cid=None
  try: cid=int(str(station_id).split(':')[-1])
  except Exception: pass
  if not cid:return {}
  out={'program':'','episode':'','song':'','next_program':'','start':'','end':'','next_start':'','next_end':''}
  try:
   data=self.get('/scheduledepisodes/rightnow',{'channelid':cid,'size':10}); ch=data.get('channel',{}) or {}; cur=ch.get('currentscheduledepisode',{}) or {}; prog=cur.get('program',{}) or {}; nxt=ch.get('nextscheduledepisode',{}) or {}; nprog=nxt.get('program',{}) or {}; out['program']=str(prog.get('name') or ''); out['episode']=str(cur.get('title') or ''); out['next_program']=str(nprog.get('name') or nxt.get('title') or '')
   def clock(v):
    m=re.search(r'\d{10,13}',str(v or ''))
    if not m:return ''
    try:
     import datetime; ts=int(m.group(0));ts=ts/1000 if ts>9999999999 else ts;return datetime.datetime.fromtimestamp(ts).strftime('%H:%M')
    except Exception:return ''
   out['start']=clock(cur.get('starttimeutc') or cur.get('starttime'));out['end']=clock(cur.get('endtimeutc') or cur.get('endtime'));out['next_start']=clock(nxt.get('starttimeutc') or nxt.get('starttime'));out['next_end']=clock(nxt.get('endtimeutc') or nxt.get('endtime'))
  except Exception:pass
  try:
   data=self.get('/playlists/rightnow',{'channelid':cid,'size':10}); pl=data.get('playlist',{}) or {}; song=pl.get('song',{}) or {}; artist=str(song.get('artist') or '').strip(); title=str(song.get('title') or '').strip(); out['song']=' – '.join(x for x in (artist,title) if x)
  except Exception:pass
  return out
 def resolve(self,t):
  if t.url:return t
  page=t.meta.get('webpage','') if t.meta else ''
  if not page:return t
  raw=self._request(page,12).decode('utf-8','replace');normalized=html.unescape(raw).replace('\\u0026','&').replace('\\/','/')
  def meta(prop):
   for pat in (rf"<meta[^>]+(?:property|name)=['\"]{re.escape(prop)}['\"][^>]+content=['\"]([^'\"]+)",rf"<meta[^>]+content=['\"]([^'\"]+)['\"][^>]+(?:property|name)=['\"]{re.escape(prop)}['\"]"):
    m=re.search(pat,raw,re.I)
    if m:return html.unescape(m.group(1)).strip()
   return ''
  urls=re.findall(r"https://[^'\"<>\s]+?\.(?:mp3|m4a|aac)(?:\?[^'\"<>\s]*)?",normalized,re.I)
  if not urls:return t
  t.url=urls[0];t.title=meta('og:title') or t.title;t.cover=meta('og:image') or t.cover;t.meta['needs_resolve']=False;return t

class YouTube:
 def __init__(self):
  local=Path.home()/'.local/bin/yt-dlp'
  self.binary=str(local) if local.is_file() and os.access(local,os.X_OK) else (shutil.which('yt-dlp') or '')
 def available(self):return bool(self.binary)
 def version(self):
  if not self.binary:return ''
  try:return subprocess.run([self.binary,'--version'],capture_output=True,text=True,timeout=5).stdout.strip()
  except Exception:return ''
 def search(self,q,limit=20):
  if not self.available():return []
  p=subprocess.run([self.binary,'--dump-single-json','--flat-playlist','--no-warnings',f'ytsearch{limit}:{q}'],capture_output=True,text=True,timeout=25)
  if p.returncode:return []
  d=json.loads(p.stdout);out=[]
  for x in d.get('entries',[]):
   vid=x.get('id','');out.append(Track(source='youtube',id=vid,title=x.get('title',''),artist=x.get('channel') or x.get('uploader',''),duration=float(x.get('duration') or 0),url=x.get('url',''),cover=(x.get('thumbnail') or (f'https://i.ytimg.com/vi/{vid}/hqdefault.jpg' if vid else '')),meta={'webpage':x.get('webpage_url') or f'https://www.youtube.com/watch?v={vid}',**_published_meta(x)}))
  # Keep YouTube's relevance as a signal, but strongly prefer results that
  # actually contain the user's query tokens across title + creator.
  tokens=[z.casefold() for z in re.findall(r'[^\W_]+',q,flags=re.UNICODE) if len(z)>1]
  if tokens:
   ranked=[]
   for pos,t in enumerate(out):
    title=t.title.casefold();creator=t.artist.casefold();combined=title+' '+creator
    hits=sum(1 for z in tokens if z in combined);title_hits=sum(1 for z in tokens if z in title)
    phrase=q.strip().casefold();phrase_hit=1 if phrase and phrase in combined else 0
    ranked.append((-(phrase_hit*20+hits*4+title_hits*2),pos,t))
   ranked.sort(key=lambda z:(z[0],z[1]));out=[z[2] for z in ranked]
  return out
 def resolve(self,t):
  p=subprocess.run([self.binary,'-f','bestaudio','-g','--no-warnings',t.meta.get('webpage',t.url)],capture_output=True,text=True,timeout=30)
  if p.returncode:raise RuntimeError(p.stderr.strip().splitlines()[-1] if p.stderr else 'yt-dlp failed')
  return p.stdout.strip().splitlines()[0]
