import random
class NPlayRadio:
 def __init__(self,app):self.a=app
 def build(self,seed,limit=60):
  a=self.a;pool=[]
  # Provider-neutral seed mix: local first, then Navidrome when configured.
  terms=[x for x in (seed.artist,seed.album,(seed.meta or {}).get('genre','')) if x]
  seen={f'{seed.source}:{seed.id}'}
  for q in terms[:2]:
   try:pool.extend(a.db.search(q,120))
   except Exception:pass
  n=a.nav()
  if n and seed.artist:
   try:pool.extend(n.search_library(seed.artist,40,5,5))
   except Exception:pass
  # Spotify search contributes discovery tracks but never blocks radio creation.
  if a.cfg.getbool('spotify_enabled',False) and a.spotify_configured() and seed.artist:
   try:pool.extend([x for x in a.spotify.search(seed.artist,10) if x.kind=='track'])
   except Exception:pass
  random.shuffle(pool);out=[];last_artist=(seed.artist or '').casefold()
  for t in pool:
   key=f'{t.source}:{t.id}';artist=(t.artist or '').casefold()
   if key in seen or t.kind!='track':continue
   # Avoid immediate artist repetition where possible.
   if artist and artist==last_artist and len(pool)>10:continue
   seen.add(key);out.append(t);last_artist=artist
   if len(out)>=limit:break
  return out
