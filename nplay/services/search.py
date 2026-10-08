import copy,re,threading

class SearchCoordinator:
 def __init__(self,app):self.a=app
 def search(self,q,ui):
  if not q:return
  a=self.a;local=a.db.search(q,30);groups={'LOCAL':local};lock=threading.Lock();ui.loading(f'QUICK FIND · {q}',f'Local {len(local)} · searching connected sources…',local)
  order=('LOCAL','NAVIDROME','SPOTIFY','SR','YOUTUBE')
  def publish(final=False):
   with lock:snapshot={k:list(v) for k,v in groups.items()}
   xs=[]
   if final:
    by={}
    for name in order:
     for t in snapshot.get(name,[]):
      if getattr(t,'kind','track')!='track':continue
      key=(re.sub(r'[^a-z0-9]+','',(t.artist or '').casefold()),re.sub(r'[^a-z0-9]+','',(t.title or '').casefold()))
      if all(key):by.setdefault(key,[]).append(t)
    matches=[v for v in by.values() if len({x.source for x in v})>1]
    if matches:
     pref=a.cfg.get('preferred_source','local');rank={pref:0,'local':1,'navidrome':2,'spotify':3,'youtube':4};merged=[]
     for vv in matches:
      base=copy.deepcopy(sorted(vv,key=lambda x:rank.get(x.source,9))[0]);base.meta={**(base.meta or {}),'available_sources':sorted({x.source for x in vv}),'source_alternatives':[x.dict() for x in vv]};merged.append(base)
     xs.append(('AVAILABLE FROM MULTIPLE SOURCES','',f'{len(merged)} matched tracks · preferred {pref}'));xs.extend(merged[:20])
   for name in order:
    g=snapshot.get(name,[])
    if g:xs.append((name,'',f'{len(g)} results'));xs.extend(g)
   total=sum(len(snapshot.get(n,[])) for n in order);ui.sort_mode='relevance';ui.replace_list(f'QUICK FIND · {q} · {total} RESULTS',xs,message=' · '.join(f'{n} {len(snapshot.get(n,[]))}' for n in order if n in snapshot)+((' · complete') if final else ' · searching…'))
  publish(False)
  jobs=[]
  n=a.nav()
  if n and a.sources.supports('navidrome','search'):jobs.append(('NAVIDROME',lambda:n.search_library(q,10,6,4)))
  if a.sources.supports('spotify','search') and a.cfg.getbool('spotify_enabled',False) and a.spotify_configured():jobs.append(('SPOTIFY',lambda:a.spotify.search(q,6)))
  if a.sources.supports('sr','search') and a.cfg.getbool('sr_enabled',True):jobs.append(('SR',lambda:a.sr.search_programmes_and_episodes(q,18)))
  if a.sources.supports('youtube','search') and a.cfg.getbool('youtube_enabled',True) and a.yt.available():jobs.append(('YOUTUBE',lambda:a.yt.search(q,10)))
  remaining={'n':len(jobs)}
  if not jobs:return publish(True)
  def worker(name,fn):
   try:res=fn()
   except Exception as e:a.log_debug(f'Quick Find {name} failed',e);res=[]
   with lock:groups[name]=res;remaining['n']-=1;final=remaining['n']==0
   ui.post(lambda final=final:publish(final))
  for name,fn in jobs:threading.Thread(target=worker,args=(name,fn),daemon=True).start()
