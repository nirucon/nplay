import os,sys,threading,shutil,json,time,random
from pathlib import Path
from . import __version__
from .model import Track
from .providers import Navidrome,SR,YouTube

class NavigationMixin:
 def root_browse(self,ui):
  items=[]
  if self.current:
   detail=(self.current.artist+' · ' if self.current.artist else '')+self.current.title
   if self.play_context and self.context_index>=0:detail+=f' · {self.context_index+1}/{len(self.play_context)}'
   items += [('CONTINUE','',''),(self.current.title,'resume-current',detail),('LIBRARY','','')]
  else:items=[('LIBRARY','','')]
  items += [('LOCAL MUSIC','local',f'{self.db.count()} indexed tracks')]
  if self.cfg.getbool('navidrome_enabled',True):items.append(('NAVIDROME','nav','Configured' if self.nav_configured() else 'Set up server'))
  if self.cfg.getbool('spotify_enabled',False):items.append(('SPOTIFY','spotify','Connected' if self.spotify_configured() else 'Set up · Premium'))
  items += [('PLAYLISTS','playlists','Local cross-source playlists'),('SMART PLAYLISTS','smart','Dynamic local collections'),('STATISTICS','stats','Listening time · charts & rankings · offline'),('DISCOVERIES','discoveries','Songs saved from radio/discovery'),('DISCOVER','',''),('RADIO','radiohome','Live · podcasts · custom stations')]
  if self.cfg.getbool('youtube_enabled',True):items.append(('YOUTUBE · BETA','yt','Audio via yt-dlp'))
  items += [('PLAYBACK','',''),('QUEUE','queue',f'{len(self.queue)} items'),('FAVORITES','fav','Across sources'),('HISTORY','hist','Recently played'),('BOOKMARKS','bookmarks','Saved positions in long-form audio'),('NPLAY','',''),('SETTINGS','settings','Appearance · sources · library'),('ABOUT','about',f'NPLAY {__version__}')]
  ui.show_menu('BROWSE',items)
 def local_home(self,ui):
  root=' · '.join(self.roots());ui.show_menu('LOCAL MUSIC',[('ALL TRACKS','local:all',f'{self.db.count()} indexed tracks'),('ARTISTS','local:artists','A–Z'),('ALBUMS','local:albums','A–Z'),('FOLDERS','local:browsefolders','Browse indexed folders'),('RECENTLY ADDED','local:recent','Newest indexed files'),('RANDOM 10 SONGS','local:random:10','Fresh shuffled selection'),('RANDOM 50 SONGS','local:random:50','Fresh shuffled selection'),('RANDOM 100 SONGS','local:random:100','Fresh shuffled selection'),('RANDOM ARTIST · SHUFFLE ALL','local:randomartistsearch','Search artist · shuffle all their songs'),('RANDOM ARTIST','local:randomartist','Pick one artist · albums/tracks in order'),('RANDOM ALBUM','local:randomalbum','Pick an album and play it in order'),('SEARCH','local:search','Tracks · artists · albums'),('SCAN CHANGES','local:scan','Incremental metadata scan'),('FULL RESCAN','local:fullscan','Re-read metadata for all files'),('MUSIC FOLDERS','local:folders',root)])
 def radio_home(self,ui):
  items=[]
  if self.cfg.getbool('sr_enabled',True):items += [('LIVE RADIO','radio','Sveriges Radio · P1 · P2 · P3 · P4 Väst'),('PODCASTS & PROGRAMMES','srpod','Sveriges Radio · search · browse · discover')]
  if self.cfg.getbool('custom_radio_enabled',True):items += [('CUSTOM STATIONS','customradio',f'{len(self.db.radios())} saved stations'),('ADD STATION','radio:add','Name + stream URL')]
  if not items:items=[('RADIO SOURCES DISABLED','settings:sources','Enable sources in Settings')]
  ui.show_menu('RADIO',items)
 def scan_async(self,ui,force=False):
  ui.loading('LOCAL MUSIC','Scanning configured music folders recursively… playback remains available.')
  def progress(r):
   if r.get('done'):return
   msg=f"Scanning · {r.get('checked',0):,} audio files · {r.get('new',0):,} new · {r.get('updated',0):,} updated"
   if r.get('offline'):msg+=f" · {len(r['offline'])} root offline (retained)"
   ui.post(lambda msg=msg:ui.status(msg))
  def done(r):
   msg=f"Scan · {r['checked']:,} checked · {r['new']:,} new · {r['updated']:,} updated · {r['removed']:,} removed · {r['total']:,} indexed"
   if r.get('offline'):msg+=f" · {len(r['offline'])} offline root retained"
   ui.replace_list('LOCAL MUSIC',self.db.search(),empty='No supported audio found',message=msg)
  self.async_run(ui,lambda:self.scan(progress,force=force),done,'Library scan failed')
 def add_radio(self,ui):
  name=ui.prompt('STATION NAME › ');url=ui.prompt('STREAM URL › ')
  if not name or not url:return ui.status('Add station cancelled')
  try:self.db.radio_add(name,url);ui.status('Station saved · '+name);self.radio_home(ui)
  except Exception as e:ui.error('Invalid station',str(e))
 def playlists_home(self,ui):
  items=self.db.playlists()+[('+ NEW PLAYLIST','playlist:new','Create a local playlist')];ui.show_menu('PLAYLISTS',items)
 def add_selected_to_playlist(self,t,ui):
  pls=self.db.playlists()
  if not pls:
   name=ui.prompt('NEW PLAYLIST › ');pid=self.db.playlist_create(name) if name else None
  else:
   name=ui.prompt('PLAYLIST NAME › ',pls[0][0]);pid=self.db.playlist_create(name) if name else None
  if pid:self.db.playlist_add(pid,t);ui.status(f'Added to playlist · {name}')
 def spotify_add_playlist(self,t,ui):
  if t.source!='spotify':return ui.status('Spotify playlists accept Spotify items only')
  ui.action_target=t;ui.loading('SPOTIFY · ADD TO PLAYLIST','Loading your playlists…')
  def done(pls):ui.show_menu('SPOTIFY · ADD TO PLAYLIST',[(p.title,'spotify:add:'+p.id,str((p.meta or {}).get('item_count',0))+' items') for p in pls] or [('NO PLAYLISTS','','No Spotify playlists available')])
  self.async_run(ui,self.spotify.playlists,done,'Spotify playlists unavailable')
 def playlist_remove(self,pid,index,ui):
  self.db.playlist_remove(pid,index);ui.replace_list('PLAYLIST · '+self.db.playlist_name(pid),self.db.playlist_tracks(pid),empty='Playlist is empty');ui.status('Removed from playlist')
 def playlist_move(self,pid,index,delta,ui):
  j=self.db.playlist_move(pid,index,delta);ui.items=self.db.playlist_tracks(pid);ui.sel=j;ui.invalidate();return j
 def playlist_rename(self,pid,ui):
  old=self.db.playlist_name(pid);name=ui.prompt('RENAME PLAYLIST › ',old)
  if name and name!=old:self.db.playlist_rename(pid,name);ui.title='PLAYLIST · '+name;ui.status('Playlist renamed')
 def playlist_delete(self,pid,ui):
  name=self.db.playlist_name(pid);answer=ui.prompt(f'DELETE {name}? type YES › ')
  if answer=='YES':self.db.playlist_delete(pid);ui.current_playlist_pid=None;self.playlists_home(ui);ui.status('Playlist deleted')
  else:ui.status('Delete cancelled')
 def source_switch_menu(self,t,ui):
  from .model import Track
  raw=(t.meta or {}).get('source_alternatives') or []
  alts=[Track.from_dict(x) for x in raw if isinstance(x,dict)]
  if not alts:return ui.status('No matched alternate sources for this track')
  ui.source_switch_tracks={x.source:x for x in alts};ui.action_target=t
  labels={'local':'Local','navidrome':'Navidrome','spotify':'Spotify','youtube':'YouTube'}
  ui.show_menu('SWITCH SOURCE · '+t.title,[(labels.get(x.source,x.source.title()),'source-switch:'+x.source,('current' if x.source==t.source else 'continue near current position')) for x in alts])
 def source_handoff(self,t,ui):
  pos=self.playback_position()
  self.play(t,ui,preserve_context=True,resume_pos=pos if t.seekable else 0)
 def open_source(self,s,ui):
  if s=='stats:toggle':self.statistics_toggle(ui);return
  if s=='stats:overview':self.stats_home(ui,getattr(self,'_stats_period','week'),push=False);return
  if s=='stats:settings':self.stats_settings(ui);return
  if s in ('stats:artists','stats:albums','stats:tracks','stats:sources'):
   self.stats_details(ui,s.split(':',1)[1]);return
  if s=='stats:copy-id':
   import shutil,subprocess
   clip=shutil.which('wl-copy') or shutil.which('xclip') or shutil.which('xsel')
   if not clip:ui.status('Clipboard tool unavailable · install wl-clipboard, xclip or xsel');return
   try:
    args=([clip] if clip.endswith('wl-copy') else [clip,'-selection','clipboard'] if clip.endswith('xclip') else [clip,'--clipboard','--input'])
    subprocess.run(args,input=self.statistics.installation_id(),text=True,timeout=2,check=True)
    ui.status('Installation ID copied')
   except (OSError,subprocess.SubprocessError):ui.status('Unable to copy installation ID')
   return
  if s.startswith('stats:period:'):self.stats_home(ui,s.rsplit(':',1)[1],push=False);return
  if s=='resume-current':self.play_pause(ui);return
  if s.startswith('source-switch:'):
   src=s.split(':',1)[1];t=getattr(ui,'source_switch_tracks',{}).get(src)
   if t:self.source_handoff(t,ui)
   return
  if s.startswith('spotify:add:'):
   t=ui.action_target;pid=s.split(':',2)[2]
   if t:self.async_run(ui,lambda:self.spotify.add_to_playlist(pid,t),lambda _:ui.status('Added to Spotify playlist'),'Spotify playlist update failed')
   return
  if s=='spotify:open-web':
   import webbrowser;webbrowser.open('https://open.spotify.com/');ui.status('Spotify Web Player opened · activate playback, then refresh Devices');return
  if s=='spotify:devices-refresh':self.spotify_devices(ui);return
  if s.startswith('spotify:device:'):
   did=s.split(':',2)[2]
   name=getattr(ui,'spotify_device_names',{}).get(did,'Spotify device')
   def selected(_):
    self.cfg.set('spotify_device_id',did);self.cfg.set('spotify_device_name',name);ui.status('Spotify device · '+name);self.open_spotify_home(ui)
   self.async_run(ui,lambda:self.spotify.transfer(did,False),selected,'Device transfer failed');return
  if s.startswith('spotify:searchall:'):
   kind=s.rsplit(':',1)[1];items=getattr(ui,'spotify_search_groups',{}).get(kind,[])
   label={'artist':'ARTISTS','album':'ALBUMS','track':'TRACKS','playlist':'PLAYLISTS'}.get(kind,kind.upper())
   ui.show_list('SPOTIFY · '+label,items,empty='No Spotify matches');return
  if s.startswith('ctx:'):
   t=ui.action_target or self.current;action=s.split(':',1)[1]
   if not t:return ui.status('Actions · no target')
   if action=='play':self.open_item(t,ui)
   elif action=='next':self.queue_next(t,ui)
   elif action=='queue':self.queue_add(t,ui)
   elif action=='after-context':self.later_queue.append(t);self.save_state();ui.status('Queued after current context · '+t.title)
   elif action=='playlist':self.add_selected_to_playlist(t,ui)
   elif action=='info':ui.context_target=t;ui.push();ui.mode='context';ui._view_changed()
   elif action=='artist':self.nav_goto_artist(t,ui)
   elif action=='album':self.nav_goto_album(t,ui)
   elif action=='spotify-save':self.async_run(ui,lambda:self.spotify.save(t),lambda _:ui.status('Saved to Spotify library'),'Spotify save failed')
   elif action=='spotify-playlist':self.spotify_add_playlist(t,ui)
   elif action.startswith('rate'):
    self.set_rating(t,int(action[-1]),ui)
   elif action=='discover':self.save_discovery(ui)
   elif action=='switch-source':self.source_switch_menu(t,ui)
   elif action=='bookmark':
    pos=self.playback_position() if self.current and self._track_key(self.current)==self._track_key(t) else self.db.resume_get(self._track_key(t),legacy_id=t.id);self.db.bookmark_add(t,pos);ui.status('Bookmark saved · '+self.fmt(pos))
   elif action=='nplay-radio':
    ui.loading('NPLAY RADIO · '+t.title,'Building a cross-source mix…')
    self.async_run(ui,lambda:self.radio_engine.build(t),lambda xs:(ui.replace_list('NPLAY RADIO · '+t.title,xs,empty='Not enough matching music across enabled sources'),ui.status(f'NPLAY Radio · {len(xs)} tracks')),'NPLAY Radio unavailable')
   return
  if s=='local':self.local_home(ui)
  elif s=='local:all':ui.show_list('LOCAL MUSIC',self.db.search(),empty=f'No indexed music · folder: {" · ".join(self.roots())} · press u to scan')
  elif s.startswith('local:random:'):
   limit=int(s.rsplit(':',1)[1]);ui.show_list(f'LOCAL MUSIC · RANDOM {limit}',self.db.random_tracks(limit),empty='No indexed music · press u to scan')
  elif s=='local:randomartistsearch':
   q=ui.prompt('ARTIST › ')
   if q:
    hits=self.db.find_artists(q,50)
    if len(hits)==1:
     name=hits[0][0];ui.show_list('LOCAL MUSIC · '+name+' · SHUFFLED',self.db.shuffled_artist_tracks(name))
    elif hits:ui.show_menu('LOCAL MUSIC · CHOOSE ARTIST',[(name,'local:randomartisttracks:'+name,f'{n} tracks · shuffle all') for name,n in hits])
    else:ui.status('No matching local artist')
  elif s.startswith('local:randomartisttracks:'):
   name=s.split(':',2)[2];ui.show_list('LOCAL MUSIC · '+name+' · SHUFFLED',self.db.shuffled_artist_tracks(name))
  elif s=='local:randomartist':
   name,xs=self.db.random_artist_tracks();ui.show_list('LOCAL MUSIC · RANDOM ARTIST · '+name,xs,empty='No indexed artists · press u to scan')
  elif s=='local:recent':ui.show_list('LOCAL MUSIC · RECENTLY ADDED',self.db.recently_added(100),empty='No indexed music · press u to scan')
  elif s=='local:randomalbum':
   xs=self.db.random_album_tracks();ui.show_list('LOCAL MUSIC · RANDOM ALBUM',xs,empty='No indexed albums · press u to scan')
  elif s=='local:search':
   q=ui.prompt('LOCAL MUSIC SEARCH › ');ui.show_list('LOCAL MUSIC · SEARCH · '+q,self.db.search(q,200),empty='No local matches') if q else None
  elif s=='local:artists':ui.show_menu('LOCAL MUSIC · ARTISTS',self.db.local_artists())
  elif s=='local:albums':ui.show_menu('LOCAL MUSIC · ALBUMS',self.db.local_albums())
  elif s=='local:browsefolders':ui.show_menu('LOCAL MUSIC · FOLDERS',self.db.local_folders())
  elif s.startswith('local:artist:'):ui.show_list('LOCAL MUSIC · '+s.split(':',2)[2],self.db.tracks_by_artist(s.split(':',2)[2]))
  elif s.startswith('local:album:'):
   artist,album=s.split(':',2)[2].split('\t',1);ui.show_list('LOCAL MUSIC · '+album,self.db.tracks_by_album(artist,album))
  elif s.startswith('local:folder:'):ui.show_list('LOCAL MUSIC · '+Path(s.split(':',2)[2]).name,self.db.tracks_by_folder(s.split(':',2)[2]))
  elif s=='local:scan':self.scan_async(ui)
  elif s=='local:fullscan':
   self.scan_async(ui,force=True)
  elif s=='local:folders':
   value=ui.prompt('MUSIC FOLDERS (: separated) › ',self.cfg.get('music_dirs',str(Path.home()/'Music')));
   if value:self.cfg.set('music_dirs',value);ui.status('Music folders updated · press u to scan')
  elif s=='radiohome':self.radio_home(ui)
  elif s=='radio':
   if not self.cfg.getbool('sr_enabled',True):return ui.status('Sveriges Radio is disabled · Settings → Sources')
   xs=self.sr.live();ui.show_list('SVERIGES RADIO · LIVE',xs,message='Fetching current programmes…');self.refresh_radio_async(xs,ui)
  elif s=='customradio':
   if not self.cfg.getbool('custom_radio_enabled',True):return ui.status('Custom radio is disabled · Settings → Sources')
   ui.show_list('RADIO · CUSTOM STATIONS',self.db.radios(),empty='No custom stations · choose Add Station in Radio')
  elif s=='radio:add':self.add_radio(ui)
  elif s=='radio:toggle-sr':
   enabled=not self.cfg.getbool('sr_enabled',True);self.cfg.set('sr_enabled','true' if enabled else 'false');ui.status('Sveriges Radio '+('enabled' if enabled else 'disabled'));self.open_source('settings:sources',ui)
  elif s=='playlists':self.playlists_home(ui)
  elif s.startswith('visual-style:'):
   ui.visual_style=s.split(':',1)[1];self.cfg.set('visualizer_style',ui.visual_style);ui.status('CAVA style · '+ui.visual_style.capitalize());ui.visualizer_style_menu()
  elif s.startswith('settings:'):self.settings.open(s,ui)
  elif s=='settings':self.settings.open(s,ui)
  elif s=='playlist:new':
   name=ui.prompt('NEW PLAYLIST › ');
   if name:self.db.playlist_create(name);self.playlists_home(ui)
  elif s.startswith('playlist:'):
   pid=int(s.split(':',1)[1]);ui.current_playlist_pid=pid;ui.show_list('PLAYLIST · '+self.db.playlist_name(pid),self.db.playlist_tracks(pid),empty='Playlist is empty')
  elif s=='srpod':
   if not self.cfg.getbool('sr_enabled',True):return ui.status('Sveriges Radio is disabled · Settings → Sources')
   ui.show_menu('SR · PODCASTS & PROGRAMMES',[('SEARCH SR','srsearch','Programmes + episodes together'),('PROGRAMMES A–Ö','srprograms','Browse the complete programme catalogue'),('SEARCH PROGRAMMES','srprogsearch','Programme titles and descriptions'),('SEARCH EPISODES','srepisearch','Individual episodes'),('DOCUMENTARY','srcat:documentary','Documentaries and related programmes'),('HISTORY','srcat:history','History programmes and episodes'),('NEWS & SOCIETY','srcat:news','News, society and current affairs'),('MUSIC','srcat:music','Music programmes and episodes'),('RECENTLY PLAYED','hist','Your NPLAY history')])
  elif s=='srsearch':
   q=ui.prompt('SEARCH SVERIGES RADIO › ');self.search_sr_all(q,ui)
  elif s.startswith('srcat:'):
   terms={'documentary':'dokumentär','history':'historia','news':'nyheter','music':'musik'};self.search_sr_all(terms.get(s.split(':',1)[1],s.split(':',1)[1]),ui)
  elif s=='srprogsearch':
   q=ui.prompt('SR PROGRAMME SEARCH › ');self.search_sr_programs(q,ui)
  elif s=='srepisearch':
   q=ui.prompt('SR EPISODE SEARCH › ');self.search_provider('sr',q,ui)
  elif s=='srprograms':self.load_sr_programs('',ui)
  elif s=='smart':self.smart_home(ui)
  elif s=='smart:recent':ui.show_list('SMART · RECENTLY ADDED',self.db.recently_added(200))
  elif s=='smart:most':ui.show_list('SMART · MOST PLAYED',self.db.most_played(200),empty='No listening history yet')
  elif s=='smart:never':ui.show_list('SMART · NEVER PLAYED',self.db.never_played(200),empty='Everything has been played')
  elif s=='smart:albums':ui.show_menu('SMART · UNPLAYED ALBUMS',self.db.unplayed_albums(200))
  elif s=='smart:rated':ui.show_list('SMART · HIGHLY RATED',self.db.rated(4,200),empty='No 4–5 star local tracks')
  elif s=='stats':self.stats_home(ui)
  elif s=='discoveries':ui.show_list('DISCOVERIES',self.db.discoveries(),empty='No discoveries yet · save a radio song from Actions')
  elif s=='fav':ui.show_list('FAVORITES',self.db.favorites(),empty='No favorites yet')
  elif s=='hist':ui.show_list('HISTORY',self.db.history(),empty='Nothing played yet')
  elif s=='bookmarks':ui.show_list('BOOKMARKS',self.db.bookmarks(),empty='No bookmarks yet · use Actions on long-form audio')
  elif s=='queue':ui.show_list('QUEUE',self.queue,empty='Queue is empty')
  elif s=='nav':
   if not self.cfg.getbool('navidrome_enabled',True):return ui.status('Navidrome is disabled · Settings → Sources')
   self.open_nav_home(ui) if self.nav() else ui.nav_setup()
  elif s=='yt':
   if not self.cfg.getbool('youtube_enabled',True):return ui.status('YouTube is disabled · Settings → Sources')
   q=ui.prompt('YOUTUBE SEARCH › ');self.search_provider('yt',q,ui)
  elif s=='spotify':self.open_spotify_home(ui)
  elif s=='spotify:setup':self.spotify_setup(ui)
  elif s=='spotify:search':
   q=ui.prompt('SPOTIFY SEARCH › ');self.spotify_search(q,ui)
  elif s=='spotify:playlists':self.spotify_load('SPOTIFY · PLAYLISTS',self.spotify.playlists,ui)
  elif s=='spotify:recent':self.spotify_load('SPOTIFY · RECENTLY PLAYED',self.spotify.recent,ui)
  elif s=='spotify:toptracks':self.spotify_load('SPOTIFY · TOP TRACKS',self.spotify.top_tracks,ui)
  elif s=='spotify:topartists':self.spotify_load('SPOTIFY · TOP ARTISTS',self.spotify.top_artists,ui)
  elif s=='spotify:local':self.spotify_local_menu(ui)
  elif s=='spotify:local-start':
   ui.loading('SPOTIFY · LOCAL PLAYBACK','Starting NPLAY Spotify engine without browser authorization…')
   def local_started(_):ui.status('Local Spotify engine started · NPLAY will use this computer');self.cfg.set('spotify_playback','local');self.open_spotify_home(ui)
   self.async_run(ui,lambda:self.spotify.local.start(oauth=False),local_started,'Local Spotify engine failed')
  elif s=='spotify:local-authorize':
   ui.loading('SPOTIFY · LOCAL PLAYBACK','Starting explicit librespot authorization · browser may open…')
   def authorize_local():
    self.spotify.local.stop();self.spotify.local.start(oauth=True)
    name=self.cfg.get('spotify_local_name','NPLAY') or 'NPLAY'
    d=self.spotify.wait_for_device(name,180);self.spotify_state['device']=d
    return d
   def local_authorized(_):ui.status('Local Spotify authorized · credentials cached for future starts');self.cfg.set('spotify_playback','local');self.open_spotify_home(ui)
   self.async_run(ui,authorize_local,local_authorized,'Local Spotify authorization failed')
  elif s=='spotify:local-stop':self.spotify.local.stop();ui.status('Local Spotify engine stopped');self.spotify_local_menu(ui)
  elif s=='spotify:mode-local':self.cfg.set('spotify_playback','local');ui.status('Spotify playback · this computer');self.open_spotify_home(ui)
  elif s=='spotify:mode-connect':self.cfg.set('spotify_playback','connect');ui.status('Spotify playback · external Connect device');self.spotify_devices(ui)
  elif s=='spotify:local-diag':self.spotify_local_diagnostics(ui)
  elif s=='spotify:devices':self.spotify_devices(ui)
  elif s=='spotify:disconnect':self.spotify.disconnect();ui.status('Spotify disconnected');self.open_source('settings:sources',ui)
  elif s=='about':ui.show_about()
 def open_spotify_home(self,ui):
  if not self.cfg.getbool('spotify_enabled',False):return ui.status('Spotify is disabled · Settings → Sources')
  if not self.spotify_configured():return self.spotify_setup(ui)
  ui.show_menu('SPOTIFY',[('SEARCH','spotify:search','Artists · albums · tracks · playlists'),('PLAYLISTS','spotify:playlists','Your Spotify playlists'),('RECENTLY PLAYED','spotify:recent','Spotify listening history'),('TOP TRACKS','spotify:toptracks','Your medium-term favorites'),('TOP ARTISTS','spotify:topartists','Your medium-term favorites'),('LOCAL PLAYBACK','spotify:local','This computer · '+('ready' if self.spotify.local.available() else 'librespot required')),('DEVICES','spotify:devices',('Preferred · '+self.cfg.get('spotify_device_name','')) if self.cfg.get('spotify_device_name','') else 'Optional external Connect device'),('DISCONNECT','spotify:disconnect','Remove local authorization tokens')])
 def spotify_setup(self,ui):
  cid=ui.prompt('SPOTIFY CLIENT ID › ',self.cfg.get('spotify_client_id',''))
  if not cid:return ui.status('Spotify setup cancelled')
  self.cfg.set('spotify_client_id',cid.strip());self.cfg.set('spotify_enabled','true');ui.loading('SPOTIFY · CONNECT','Opening browser · authorize NPLAY · callback 127.0.0.1:43821…')
  self.async_run(ui,lambda:self.spotify.connect(),lambda p:(ui.status('Spotify connected · '+str(p.get('display_name') or p.get('id') or 'account')),self.open_spotify_home(ui)),'Spotify connection failed')
 def spotify_load(self,title,work,ui):
  if not self.spotify_configured():return self.spotify_setup(ui)
  ui.push();ui.loading(title,'Loading from Spotify…');self.async_run(ui,work,lambda xs:ui.replace_list(title,xs,empty='No Spotify items found'),'Spotify unavailable')
 def spotify_search(self,q,ui):
  if not q:return
  ui.push();ui.loading('SPOTIFY · SEARCH',f'Searching “{q}”…')
  def done(xs):
   # Fair-share the first screen: no single Spotify result type should starve
   # tracks or playlists. Full groups remain one Enter away.
   ui.spotify_search_groups={}
   grouped=[]
   caps={'artist':5,'album':6,'track':8,'playlist':4}
   for kind,label in [('artist','ARTISTS'),('album','ALBUMS'),('track','TRACKS'),('playlist','PLAYLISTS')]:
    part=[x for x in xs if x.kind==kind];ui.spotify_search_groups[kind]=part
    if not part:continue
    grouped.append((label,'',''));grouped.extend(part[:caps[kind]])
    if len(part)>caps[kind]:grouped.append((f'SHOW ALL {label} ({len(part)})',f'spotify:searchall:{kind}',f'{len(part)-caps[kind]} more'))
   ui.replace_list(f'SPOTIFY · {q} · {len(xs)} RESULTS',grouped,empty='No Spotify matches')
  self.async_run(ui,lambda:self.spotify.search(q,10),done,'Spotify search unavailable')
 def spotify_local_menu(self,ui):
  e=self.spotify.local;mode=self.cfg.get('spotify_playback','local');state='RUNNING' if e.running() else ('READY' if e.available() else 'NOT INSTALLED')
  items=[('STATUS','',state+' · '+('selected' if mode=='local' else 'not selected')),('USE THIS COMPUTER','spotify:mode-local','Default · NPLAY manages local Spotify audio'),('START LOCAL ENGINE','spotify:local-start','Start librespot silently · never opens browser'),('AUTHORIZE / REAUTHORIZE','spotify:local-authorize','Explicit OAuth only when local playback actually needs it'),('STOP LOCAL ENGINE','spotify:local-stop','Stop local Spotify receiver'),('USE EXTERNAL CONNECT','spotify:mode-connect','Phone · speaker · another Spotify device'),('DIAGNOSTICS','spotify:local-diag','Engine · backend · cache · recent log')]
  ui.show_menu('SPOTIFY · LOCAL PLAYBACK',items)
 def spotify_local_diagnostics(self,ui):
  e=self.spotify.local
  rows=[('LIBRESPOT','',e.version() or 'not installed'),('ENGINE','',('running' if e.running() else 'stopped')),('BACKEND','',self.cfg.get('spotify_local_backend','pulseaudio')),('BITRATE','',self.cfg.get('spotify_local_bitrate','320')+' kbps'),('DEVICE NAME','',self.cfg.get('spotify_local_name','NPLAY')),('PLAYBACK MODE','',self.cfg.get('spotify_playback','local')),('CACHE','',str(e.system_cache)),('ENGINE LOG','',str(e.log_path)),('CACHED FILES','',str(len(e.credential_files()))),('RECENT ENGINE LOG','','')]
  rows += [(line[:34],'',line[34:]) for line in (e.log_tail().splitlines() or ['No local engine log yet.'])]
  ui.show_menu('SPOTIFY · LOCAL DIAGNOSTICS',rows)
 def spotify_devices(self,ui):
  ui.push();ui.loading('SPOTIFY · DEVICES','Finding Spotify Connect devices…')
  def done(ds):
   preferred=self.cfg.get('spotify_device_id','');ui.spotify_device_names={str(d.get('id')):str(d.get('name') or 'Spotify device') for d in ds if d.get('id')}
   items=[]
   for d in ds:
    if not d.get('id'):continue
    mark='● ' if d.get('is_active') else ('◆ ' if d.get('id')==preferred else '')
    detail=str(d.get('type') or 'Device')+' · '+str(d.get('volume_percent') if d.get('volume_percent') is not None else '?')+'%'
    if d.get('is_active'):detail+=' · ACTIVE'
    elif d.get('id')==preferred:detail+=' · PREFERRED'
    items.append((mark+str(d.get('name') or 'Spotify device'),'spotify:device:'+str(d.get('id')),detail))
   items += [('ACTIONS','',''),('REFRESH DEVICES','spotify:devices-refresh','Rescan Spotify Connect'),('OPEN SPOTIFY WEB PLAYER','spotify:open-web','Use when no Connect device is available')]
   ui.show_menu('SPOTIFY · DEVICES',items)
  self.async_run(ui,self.spotify.devices,done,'Spotify devices unavailable')
 def open_nav_home(self,ui):ui.show_menu('NAVIDROME',[('RECENTLY ADDED','nav:newest','Albums'),('RANDOM 10 SONGS','nav:randomtracks:10','Fresh server selection'),('RANDOM 50 SONGS','nav:randomtracks:50','Fresh server selection'),('RANDOM 100 SONGS','nav:randomtracks:100','Fresh server selection'),('RANDOM ARTIST · SHUFFLE ALL','nav:randomartistsearch','Search artist · shuffle all their songs'),('RANDOM ARTIST','nav:randomartist','Pick one artist · albums/tracks in order'),('RANDOM ALBUMS','nav:random','Albums'),('ARTISTS','nav:artists','A–Z'),('ALBUMS','nav:albums','A–Z'),('PLAYLISTS','nav:playlists','Server playlists'),('SEARCH','nav:search','Artists · albums · songs'),('SETUP SERVER','nav:setup','Change connection')])
 def nav_action(self,action,ui):
  ui.push()
  n=self.nav()
  if not n:return ui.nav_setup()
  if action=='setup':return ui.nav_setup()
  if action=='search':
   q=ui.prompt('NAVIDROME SEARCH › ');return self.search_provider('nav',q,ui)
  ui.loading('NAVIDROME','Loading…')
  if action.startswith('randomtracks:'):
   limit=int(action.rsplit(':',1)[1]);self.async_run(ui,lambda:n.random_tracks(limit),lambda xs:ui.replace_list(f'NAVIDROME · RANDOM {limit}',xs),'Navidrome unavailable')
  elif action=='randomartistsearch':
   q=ui.prompt('ARTIST › ')
   if not q:return
   ui.loading('NAVIDROME · ARTIST','Searching…')
   def found(xs):
    artists=[x for x in xs if x.kind=='artist']
    if not artists:return ui.show_empty('NAVIDROME · ARTIST','No matching artist')
    ui.show_menu('NAVIDROME · CHOOSE ARTIST',[(x.title,'nav:randomartisttracks:'+(x.meta.get('artist_id') or x.id),'shuffle all songs') for x in artists])
   self.async_run(ui,lambda:n.search_library(q,0,0,50),found,'Navidrome artist search unavailable')
  elif action.startswith('randomartisttracks:'):
   aid=action.split(':',1)[1];ui.loading('NAVIDROME · ARTIST','Loading and shuffling all songs…');self.async_run(ui,lambda:n.artist_tracks(aid,True),lambda xs:ui.replace_list('NAVIDROME · ARTIST · SHUFFLED',xs),'Navidrome artist unavailable')
  elif action=='randomartist':
   def random_done(result):
    name,xs=result;ui.replace_list('NAVIDROME · RANDOM ARTIST · '+name,xs,empty='No Navidrome artists')
   self.async_run(ui,n.random_artist_tracks,random_done,'Navidrome random artist unavailable')
  elif action in ('newest','random','albums'):
   kind={'newest':'newest','random':'random','albums':'alphabeticalByName'}[action];self.async_run(ui,lambda:n.albums(kind,60),lambda xs:ui.replace_list('NAVIDROME · '+action.upper(),xs),'Navidrome unavailable')
  elif action=='artists':self.async_run(ui,n.artists,lambda xs:ui.replace_list('NAVIDROME · ARTISTS',xs),'Navidrome unavailable')
  elif action=='playlists':self.async_run(ui,n.playlists,lambda xs:ui.replace_list('NAVIDROME · PLAYLISTS',xs),'Navidrome unavailable')
 def nav_goto_artist(self,t,ui):
  if not t.artist:return ui.status('No artist metadata')
  ui.loading('NAVIDROME · '+t.artist,'Finding artist…')
  def work():
   xs=self.nav().search_library(t.artist,20,8,0);return next((x for x in xs if x.kind=='artist' and x.title.casefold()==t.artist.casefold()),next((x for x in xs if x.kind=='artist'),None))
  def done(x):
   if not x:return ui.show_empty('NAVIDROME · '+t.artist,'Artist not found')
   ui.loading(x.title,'Loading artist…');self.async_run(ui,lambda:self.nav().artist_albums(x.meta.get('artist_id') or x.id),lambda xs:ui.replace_list(x.title,xs),'Artist unavailable')
  self.async_run(ui,work,done,'Artist lookup unavailable')
 def nav_goto_album(self,t,ui):
  if not t.album:return ui.status('No album metadata')
  q=(t.artist+' '+t.album).strip();ui.loading('NAVIDROME · '+t.album,'Finding album…')
  def work():
   xs=self.nav().search_library(q,0,20,0);return next((x for x in xs if x.kind=='album' and x.title.casefold()==t.album.casefold()),next((x for x in xs if x.kind=='album'),None))
  def done(x):
   if not x:return ui.show_empty('NAVIDROME · '+t.album,'Album not found')
   ui.loading(x.title,'Loading album…');self.async_run(ui,lambda:self.nav().album_tracks(x.meta.get('album_id') or x.id),lambda xs:ui.replace_list(f'{x.artist} · {x.title}',xs),'Album unavailable')
  self.async_run(ui,work,done,'Album lookup unavailable')
 def open_item(self,t,ui):
  if t.source=='spotify' and t.kind=='playlist':
   ui.loading(t.title,'Loading Spotify playlist…');self.async_run(ui,lambda:self.spotify.playlist_items(t.id),lambda xs:ui.replace_list('SPOTIFY · '+t.title,xs,empty='Playlist has no accessible items'),'Spotify playlist unavailable');return
  if t.source=='spotify' and t.kind=='album':
   ui.loading(t.title,'Loading Spotify album…');self.async_run(ui,lambda:self.spotify.album_tracks(t.id),lambda xs:ui.replace_list('SPOTIFY · '+t.title,xs,empty='Album is empty'),'Spotify album unavailable');return
  if t.source=='spotify' and t.kind=='artist':
   return self.spotify_search(t.title,ui)
  if t.source=='navidrome' and t.kind=='album':
   ui.loading(t.title,'Loading album…');self.async_run(ui,lambda:self.nav().album_tracks(t.meta.get('album_id') or t.id),lambda xs:ui.replace_list(f'{t.artist} · {t.title}',xs),'Album unavailable');return
  if t.source=='navidrome' and t.kind=='artist':
   ui.loading(t.title,'Loading artist…');self.async_run(ui,lambda:self.nav().artist_albums(t.meta.get('artist_id') or t.id),lambda xs:ui.replace_list(t.title,xs),'Artist unavailable');return
  if t.source=='navidrome' and t.kind=='playlist':
   ui.loading(t.title,'Loading playlist…');self.async_run(ui,lambda:self.nav().playlist_tracks(t.meta.get('playlist_id') or t.id),lambda xs:ui.replace_list(t.title,xs),'Playlist unavailable');return
  if t.source=='sr' and t.kind=='program':
   ui.loading(t.title,'Loading episodes…')
   pid=t.meta.get('program_id') if t.meta else None
   work=(lambda:self.sr.program_episodes(pid,60)) if pid else (lambda:[x for x in self.sr.search_all(t.title,60) if x.kind=='podcast'])
   self.async_run(ui,work,lambda xs:(setattr(ui,'sort_mode','newest'),ui.replace_list('SR · '+t.title+f' · {len(xs)} EPISODES',xs,empty='No playable episodes found')), 'Programme unavailable');return
  resume=float((t.meta or {}).get('bookmark_pos') or 0);self.play(t,ui,resume_pos=resume)
 def load_sr_programs(self,q,ui):
  ui.loading('SR · PROGRAMMES','Loading programmes…');self.async_run(ui,lambda:self.sr.programs(q,200),lambda xs:ui.replace_list('SR · PROGRAMMES',xs),'SR unavailable')
 def search_sr_all(self,q,ui):
  if not q:return
  if len(q.strip())<2:return ui.status('SR search · type at least 2 characters')
  ui.push();ui.loading('SR · SEARCH',f'Searching programmes and episodes for “{q}”…')
  self.async_run(ui,lambda:self.sr.search_all(q,50),lambda xs:ui.replace_list(f'SR · SEARCH · {q}',xs,empty='No programmes or episodes found',message=f'{len(xs)} SR results'), 'SR search unavailable')
 def search_sr_programs(self,q,ui):
  if not q:return
  if len(q.strip())<2:return ui.status('SR search · type at least 2 characters')
  ui.push();ui.loading('SR · PROGRAMMES',f'Searching “{q}”…');self.async_run(ui,lambda:self.sr.programs(q,50),lambda xs:ui.replace_list(f'SR PROGRAMMES · {q}',xs),'SR unavailable')
 def refresh_radio_async(self,xs,ui=None):
  def work():
   info={}
   for t in xs:
    try:info[t.id]=self.sr.live_info(t.id)
    except Exception:info[t.id]={}
   self.radio_info.update(info);return info
  def done(info):
   if self.current and self.current.source=='sr' and self.current.kind=='radio' and self.current.id in info:
    self.notifier.track_changed(self.current,force=True,radio_info=info.get(self.current.id) or {})
   if ui:ui.status('Playing live · '+(self.current.title if self.current and self.current.kind=='radio' else 'Sveriges Radio'));ui.invalidate()
  self.async_run(ui,work,done,'SR metadata unavailable') if ui else threading.Thread(target=lambda:done(work()),daemon=True).start()
 def queue_add(self,t,ui=None):
  self.queue.append(t);self.save_state();ui and ui.status(f'Queued · {t.title}')
 def queue_next(self,t,ui=None):
  self.queue.insert(0,t);self.save_state();ui and ui.status(f'Play next · {t.title}')
 def queue_remove(self,index,ui=None):
  if self.queue:self.queue.pop(max(0,min(index,len(self.queue)-1)));self.save_state();ui.replace_list('QUEUE',self.queue,empty='Queue is empty',message=f'{len(self.queue)} items')
 def queue_move(self,index,delta,ui=None):
  if not self.queue:return 0
  j=max(0,min(len(self.queue)-1,index+delta))
  if j!=index:self.queue[index],self.queue[j]=self.queue[j],self.queue[index];self.save_state();ui.items=list(self.queue);ui.invalidate()
  return j
 def queue_play(self,index,ui=None):
  if self.queue:
   t=self.queue.pop(max(0,min(index,len(self.queue)-1)));self.save_state();self.play(t,ui)
 def search_provider(self,source,q,ui):
  if not q:return
  if source=='sr' and len(q.strip())<2:return ui.status('SR search · type at least 2 characters')
  title={'nav':'NAVIDROME','sr':'SR EPISODES','yt':'YOUTUBE'}[source];ui.push();ui.loading(title,f'Searching “{q}”…')
  def work():
   xs=self.nav().search_library(q) if source=='nav' else [x for x in self.sr.search(q,40) if x.kind=='podcast'] if source=='sr' else self.yt.search(q,25)
   if source=='sr':xs.sort(key=lambda x:float((x.meta or {}).get('published_ts') or 0),reverse=True)
   return xs
  def done(xs):
   ui.sort_mode='newest' if source=='sr' else self.cfg.get('youtube_sort','relevance') if source=='yt' else 'relevance'
   if ui.sort_mode in ('newest','oldest') and any((x.meta or {}).get('published_ts') for x in xs):xs.sort(key=lambda x:float((x.meta or {}).get('published_ts') or 0),reverse=ui.sort_mode=='newest')
   ui.replace_list(f'{title} · {q} · {len(xs)} RESULTS',xs,message=f'{len(xs)} results')
  self.async_run(ui,work,done,f'{title} unavailable')
 def search_async(self,q,ui):
  return self.searcher.search(q,ui)
