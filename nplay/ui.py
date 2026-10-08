import curses,queue,time,shutil,sys,platform,os
from .visualizer import Visualizer
from .artwork import Artwork
from .ui_render import RenderingMixin
from .ui_common import ACTIONS,clip
from .theme import apply as apply_theme, available as available_themes
from . import __version__



class UI(RenderingMixin):
 def __init__(self,app):
  self.a=app;self.mode='home';self.sel=0;self.items=[];self.title='';self.empty='';self.message='Ready';self.running=True
  self.visual=self.a.cfg.getbool('visualizer',True);self.visual_style=self.a.cfg.get('visualizer_style','classic');self.layout=self.a.cfg.get('now_playing_view','normal') or 'normal'
  if self.layout not in ('normal','artwork','visualizer'):self.layout='normal'
  self.events=queue.Queue();self.st=None;self.viz=Visualizer(256);self.art=Artwork(lambda _url:self.post(self.invalidate),self.a.artwork_cache);self.stack=[];self.dirty=True;self.last_draw=0;self.art_visible=False;self.context_target=None;self.preview_due=0;self.last_size=None;self.play_pos=0.0;self.play_dur=0.0;self.last_telemetry=0;self.theme=self.a.cfg.get('theme','niru-noir');self.palette={};self.current_playlist_pid=None;self.last_checkpoint=0;self.command_history=[];self.command_history_pos=0;self.unfiltered_items=None;self.sort_mode='relevance';self.action_target=None
 def invalidate(self):self.dirty=True
 def status(self,s):self.message=s;self.invalidate()
 def dim_attr(self):
  # A_DIM is terminal-dependent and made light themes almost illegible.
  # Built-in palettes have an explicit semantic muted colour; terminal-native
  # themes keep the terminal's own dim behaviour.
  return curses.A_DIM if self.theme in ('niru-noir','omarchy') else curses.color_pair(2)
 def post(self,fn):self.events.put(fn)
 def snapshot(self):return (self.mode,self.title,list(self.items),self.sel,self.empty,self.message)
 def restore(self,s):self.mode,self.title,self.items,self.sel,self.empty,self.message=s;self._view_changed()
 def push(self):self.stack.append(self.snapshot())
 def back(self):
  if self.stack:self.restore(self.stack.pop())
  else:self.go_home()
 def _view_changed(self):
  self.sel=max(0,min(self.sel,max(0,len(self.items)-1)));self.art.clear();self.art_visible=False;self.preview_due=time.monotonic()+.10 if self.mode in ('list','menu') else 0;self._prefetch_selected();self.invalidate()
 def go_home(self,clear_stack=True):
  self.mode='home';self.items=[];self.sel=0;self.title=''
  if clear_stack:self.stack=[]
  self._view_changed()
 def now_playing(self):
  if self.mode!='home':self.push()
  self.go_home(clear_stack=False)
 def show_menu(self,title,items,push=True):
  self.unfiltered_items=None
  if push:self.push()
  self.mode='menu';self.title=title;self.items=list(items);self.sel=0;self.empty='';self._view_changed();self.edge_selection(False)
 def show_list(self,title,items,empty='No items',message=None,push=True):
  self.unfiltered_items=None
  if push:self.push()
  self.mode='list';self.title=title;self.items=list(items);self.sel=0;self.empty=empty;self.message=message if message is not None else f'{len(self.items)} items';self._view_changed()
 def replace_list(self,title,items,empty='No items',message=None):
  self.unfiltered_items=None
  self.mode='list';self.title=title;self.items=list(items);self.sel=0;self.empty=empty;self.message=message if message is not None else f'{len(self.items)} items';self._view_changed()
 def loading(self,title,msg,items=None):self.mode='list';self.title=title;self.items=list(items or []);self.sel=0;self.empty=msg;self.message=msg;self._view_changed()
 def show_empty(self,title,msg):self.replace_list(title,[],empty=msg,message=msg)
 def error(self,title,msg):self.mode='error';self.title=title;self.empty=msg;self.message=msg;self._view_changed()
 def show_diagnostics(self):self.push();self.mode='diagnostics';self._view_changed()
 def show_about(self):self.push();self.mode='about';self.title='ABOUT';self.items=[];self.sel=0;self._view_changed()
 def selected(self):
  if not self.items:return None
  x=self.items[self.sel];return None if isinstance(x,tuple) else x
 def selectable(self,i):
  if i<0 or i>=len(self.items):return False
  x=self.items[i]
  return not (isinstance(x,tuple) and not x[1])
 def move_selection(self,delta):
  if not self.items:return
  i=self.sel
  while True:
   j=i+delta
   if j<0 or j>=len(self.items):break
   i=j
   if self.selectable(i):break
  if self.selectable(i):self.sel=i
  self.preview_due=time.monotonic()+.10;self._prefetch_selected()
 def edge_selection(self,last=False):
  rng=range(len(self.items)-1,-1,-1) if last else range(len(self.items))
  for i in rng:
   if self.selectable(i):self.sel=i;break
  self.preview_due=time.monotonic()+.10;self._prefetch_selected()
 def page_selection(self,delta):
  step=max(1,(self.st.getmaxyx()[0]-8) if self.st else 10)
  for _ in range(step):self.move_selection(delta)
 def action_menu(self):
  t=self.selected() or self.a.current
  if not t:return self.status('Actions · nothing selected or playing')
  self.action_target=t
  items=[('PLAY','ctx:play','Play this item'),('PLAY NEXT','ctx:next','Insert at the front of the queue'),('ADD TO QUEUE','ctx:queue','Append to queue'),('AFTER CURRENT CONTEXT','ctx:after-context','Play after the current album/list finishes'),('ADD TO PLAYLIST','ctx:playlist','Choose a local playlist'),('INFORMATION','ctx:info','Metadata and playback context'),('RATE ★★★★★','ctx:rate5','Set local NPLAY rating to 5'),('RATE ★★★★','ctx:rate4','Set local NPLAY rating to 4'),('CLEAR RATING','ctx:rate0','Remove NPLAY rating')]
  if len((getattr(t,'meta',{}) or {}).get('available_sources',[]))>1:items.append(('SWITCH SOURCE','ctx:switch-source','Choose Local · Navidrome · Spotify · YouTube match'))
  if getattr(t,'kind','') in ('podcast','track') and getattr(t,'seekable',False):items.append(('BOOKMARK POSITION','ctx:bookmark','Save current position for later'))
  if getattr(t,'kind','') =='track':items.append(('START NPLAY RADIO','ctx:nplay-radio','Build a cross-source discovery mix from this item'))
  if getattr(t,'kind','')=='radio':items.append(('SAVE DISCOVERY','ctx:discover','Save current radio metadata for later'))
  if getattr(t,'source','')=='spotify':
   items.append(('SAVE TO SPOTIFY LIBRARY','ctx:spotify-save','Save this item in your Spotify library'))
   if getattr(t,'kind','')=='track':items.append(('ADD TO SPOTIFY PLAYLIST','ctx:spotify-playlist','Choose one of your Spotify playlists'))
  if getattr(t,'source','')=='navidrome':
   if getattr(t,'artist',''):items.append(('GO TO ARTIST','ctx:artist','Browse this artist in Navidrome'))
   if getattr(t,'album','') and getattr(t,'kind','')=='track':items.append(('GO TO ALBUM','ctx:album','Open this album in Navidrome'))
  self.show_menu('ACTIONS · '+clip(getattr(t,'title',''),42),items)
 def _prefetch_selected(self):
  x=self.selected()
  if x and getattr(x,'cover',''):self.art.prefetch(x.cover)
 def nav_setup(self):
  self.art.clear();url=self.prompt('NAVIDROME URL › ',self.a.cfg.get('navidrome_url','https://'))
  if not url:return self.status('Navidrome setup cancelled')
  if not url.startswith(('http://','https://')):url='https://'+url
  user=self.prompt('USERNAME › ',self.a.cfg.get('navidrome_user',''))
  if not user:return self.status('Navidrome setup cancelled')
  pw=self.prompt('PASSWORD › ',secret=True)
  if not pw:return self.status('Navidrome setup cancelled')
  self.loading('NAVIDROME · SETUP','Testing connection…');self.a.test_and_save_nav(url,user,pw,self)
 def run(self):return curses.wrapper(self.loop)
 def loop(self,st):
  self.st=st;curses.curs_set(0);st.timeout(25);st.keypad(True)
  try:curses.use_default_colors()
  except:pass
  self.palette=apply_theme(self.theme);self.a.ui=self;self.a.background_library_refresh(self);self.a.hydrate_resume_artwork(self)
  if self.visual:self.viz.start()
  try:
   while self.running:
    changed=False
    while True:
     try:self.events.get_nowait()();changed=True
     except queue.Empty:break
    if changed:self.dirty=True
    size=st.getmaxyx()
    if size!=self.last_size:
     self.last_size=size;self.art.clear();self.art_visible=False;self.dirty=True
     if self.mode in ('list','menu'):self.preview_due=time.monotonic()+.12
    k=st.getch()
    if k!=-1:self.key(k);self.dirty=True
    now=time.monotonic()
    if self.a.sleep_check():self.status('Sleep timer · playback stopped')
    if self.a.current and now-self.last_telemetry>=.20:
     active=self.a.playback_active();old_pos=self.play_pos;old_dur=self.play_dur
     self.play_pos=self.a.playback_position() if active else float(self.a.resume_position or 0);self.play_dur=self.a.playback_duration();self.last_telemetry=now
     # Now Playing contains dynamic transport telemetry. Redraw it at a modest
     # cadence when position/duration changes; CAVA still owns its faster 30 fps
     # rectangle-only animation path, so this does not reintroduce visual tearing.
     if abs(self.play_pos-old_pos)>=.10 or abs(self.play_dur-old_dur)>=.10:
      if self.mode=='home':self.dirty=True
      else:self.draw_transport_frame()
     if active and now-self.last_checkpoint>=2.0:self.a.checkpoint(self.play_pos,self.a.volume);self.last_checkpoint=now
    animated=self.mode=='home' and self.a.current and self.visual
    preview_ready=self.mode in ('list','menu') and self.preview_due and now>=self.preview_due
    if self.dirty or preview_ready:
     self.draw();self.last_draw=now;self.dirty=False
    elif animated and now-self.last_draw>=0.033:
     # Do not erase/redraw the whole curses screen for every CAVA frame.
     # Updating only the owned visualizer rectangle prevents tearing/flicker.
     self.draw_visual_frame();self.last_draw=now
    if preview_ready:self.preview_due=0
  finally:self.art.clear();self.viz.stop()
 def prompt(self,label,initial='',secret=False,complete=False):
  st=self.st;h,w=st.getmaxyx();buf=list(initial if not secret else '');pos=len(buf);histpos=len(self.command_history);matches=[];match_i=0
  curses.curs_set(1);curses.noecho();st.timeout(-1)
  commands=['about','browse','diagnostics','discover','gapless','help','local','navidrome','normalize','now','queue','queue-clear','queue-save','radio','rating','repeat','scan','settings','shuffle','sleep','smart','spotify','stop','bookmarks','sr','stats','theme','view','youtube']
  def suggestions(text):
   first=text.strip().split(maxsplit=1)[0].lower() if text.strip() else ''
   if ' ' not in text.strip() and first:return [x for x in commands if x.startswith(first)]
   if first=='normalize':return ['normalize on','normalize off','normalize auto','normalize track','normalize album']
   if first=='gapless':return ['gapless on','gapless off']
   if first=='view':return ['view normal','view artwork','view visualizer']
   if first=='theme':return ['theme noir','theme satie','theme larsson','theme othala','theme ingwaz','theme hackerman','theme c64','theme omarchy']
   return []
  try:
   while True:
    st.move(h-2,0);st.clrtoeol();st.addnstr(h-2,2,label,w-4,curses.A_BOLD);x=min(2+len(label),w-3);shown='•'*len(buf) if secret else ''.join(buf);st.addnstr(h-2,x,shown,max(1,w-x-2))
    if complete and not secret:
     ss=suggestions(''.join(buf));hint=('  ['+' · '.join(ss[:4])+']') if ss else ''
     if hint:st.addnstr(h-1,2,hint,w-4,self.dim_attr())
    st.move(h-2,min(w-2,x+pos));st.refresh();k=st.getch()
    if k==27:return None
    if k in (10,13,curses.KEY_ENTER):
     out=''.join(buf).strip()
     if complete and out:
      self.command_history=[x for x in self.command_history if x!=out]+[out];self.command_history=self.command_history[-50:]
     return out
    if k in (curses.KEY_BACKSPACE,127,8):
     if pos>0:buf.pop(pos-1);pos-=1
    elif k==curses.KEY_DC:
     if pos<len(buf):buf.pop(pos)
    elif k==curses.KEY_LEFT:pos=max(0,pos-1)
    elif k==curses.KEY_RIGHT:pos=min(len(buf),pos+1)
    elif complete and k==curses.KEY_UP and self.command_history:
     histpos=max(0,histpos-1);buf=list(self.command_history[histpos]);pos=len(buf)
    elif complete and k==curses.KEY_DOWN and self.command_history:
     histpos=min(len(self.command_history),histpos+1);buf=list(self.command_history[histpos]) if histpos<len(self.command_history) else [];pos=len(buf)
    elif complete and k==9:
     ss=suggestions(''.join(buf))
     if ss:
      current=''.join(buf);matches=ss if ss!=matches else matches;match_i=(match_i+1)%len(matches) if current in matches else 0;buf=list(matches[match_i]);pos=len(buf)
    elif 32<=k<=126 and len(buf)<max(1,w-x-3):buf.insert(pos,chr(k));pos+=1
  finally:
   curses.curs_set(0);st.timeout(25);st.move(h-1,0);st.clrtoeol();self.invalidate()
 def theme_label(self):
  for key,label in available_themes():
   if key==self.theme:return label
  return self.theme
 def theme_menu(self):
  self.show_menu('THEME',[(('✓ ' if k==self.theme else '  ')+label,'theme:'+k,'Live preview') for k,label in available_themes()])
 def set_theme(self,name):
  aliases={'noir':'niru-noir','niru':'niru-noir','larsson':'c-larsson','c.larsson':'c-larsson','hack':'hackerman','hacker':'hackerman','c64':'commodore64','commodore':'commodore64','commodore-64':'commodore64','oth':'othala','ing':'ingwaz','auto':'omarchy'};name=aliases.get((name or '').strip().lower(),(name or '').strip().lower())
  if name not in dict(available_themes()):return self.status('Theme · noir | satie | larsson | othala | ingwaz | hackerman | c64 | omarchy')
  self.theme=name;self.a.cfg.set('theme',name);self.palette=apply_theme(name);self.art.clear();self.art_visible=False;self.status('Theme · '+self.theme_label());self.invalidate()
 def cycle_visualizer_style(self):
  modes=['classic','gradient','blocks','dots'];cur=self.visual_style if self.visual_style in modes else 'classic';self.visual_style=modes[(modes.index(cur)+1)%len(modes)];self.a.cfg.set('visualizer_style',self.visual_style);self.status('CAVA style · '+self.visual_style.capitalize());self.invalidate()
 def visualizer_style_menu(self):
  modes=[('CLASSIC','classic','Current NPLAY style · default'),('GRADIENT','gradient','Theme-aware vertical gradient'),('BLOCKS','blocks','Solid terminal bars'),('DOTS','dots','Minimal dotted spectrum')]
  self.show_menu('CAVA STYLE',[(('✓ ' if key==self.visual_style else '  ')+label,'visual-style:'+key,desc) for label,key,desc in modes])
 def toggle_visualizer(self):
  self.visual=not self.visual;self.viz.start() if self.visual else self.viz.stop();self.a.cfg.set('visualizer','true' if self.visual else 'false');self.status('Visualizer '+('on' if self.visual else 'off'))
 def cycle_layout(self):
  modes=['normal','artwork','visualizer'];self.layout=modes[(modes.index(self.layout)+1)%len(modes)];self.a.cfg.set('now_playing_view',self.layout);self.art.clear();self.art_visible=False;self.status('View · '+self.layout.capitalize())
 def key(self,k):
  if k in (ord('Q'),3):self.running=False;return
  if k==ord('?'):self.push();self.mode='help';self._view_changed();return
  if k==27:self.back();return
  if k==14:self.now_playing();return
  if k==ord('n'):self.a.next_track(self);return
  if k==ord('p'):self.a.previous_track(self);return
  if k==ord('b'):self.a.root_browse(self);return
  if k==ord('q'):self.a.open_source('queue',self);return
  if k==9:
   if self.mode=='home':self.a.open_source('queue',self)
   elif self.title=='QUEUE':self.context_target=self.a.current;self.push();self.mode='context';self._view_changed()
   else:self.now_playing()
   return
  if k in (ord('j'),curses.KEY_DOWN):self.move_selection(1);return
  if k in (ord('k'),curses.KEY_UP):self.move_selection(-1);return
  if k in (ord('g'),curses.KEY_HOME):self.edge_selection(False);return
  if k in (ord('G'),curses.KEY_END):self.edge_selection(True);return
  if k==curses.KEY_NPAGE:self.page_selection(1);return
  if k==curses.KEY_PPAGE:self.page_selection(-1);return
  if k==ord(' '):self.a.play_pause(self);return
  if k==ord('z'):self.a.stop_and_clear(self);return
  if k==ord('C'):self.cycle_visualizer_style();return
  if k==curses.KEY_LEFT:self.a.playback_seek_relative(-5);return
  if k==curses.KEY_RIGHT:self.a.playback_seek_relative(5);return
  if k==ord('H'):self.a.playback_seek_relative(-30);return
  if k==ord('L'):self.a.playback_seek_relative(30);return
  if k in (ord('+'),ord('=')):self.a.playback_volume(5);return
  if k==ord('-'):self.a.playback_volume(-5);return
  if k==ord('m'):self.a.playback_mute();return
  if k==ord('s'):self.a.cycle_shuffle(self);return
  if k==ord('x'):self.a.cycle_repeat(self);return
  if k==ord('a'):
   x=self.selected();x and self.a.queue_add(x,self);return
  if k==ord('A'):
   x=self.selected();x and self.a.add_selected_to_playlist(x,self);return
  if k==ord('c') and self.title=='QUEUE':self.a.queue_clear(self);return
  if k==ord('W') and self.title=='QUEUE':self.a.queue_save_playlist(self);return
  if k==ord('r'):
   playable=[z for z in self.items if not isinstance(z,tuple) and getattr(z,'kind','') not in ('album','artist','playlist','program')]
   if playable:
    import random;random.shuffle(playable);self.a.set_play_context(playable,0);self.a.play(playable[0],self,preserve_context=True);self.status(f'Random · {playable[0].title}');return
   if self.title.startswith('LOCAL'):self.show_list('LOCAL MUSIC · RANDOM',self.a.db.random_tracks(50),empty='No indexed music');return
  if k==ord('e'):
   x=self.selected();x and self.a.queue_next(x,self);return
  if k==ord('d'):
   if self.title=='QUEUE':self.a.queue_remove(self.sel,self);return
   if self.current_playlist_pid is not None and self.title.startswith('PLAYLIST ·'):self.a.playlist_remove(self.current_playlist_pid,self.sel,self);return
  if k==ord('J'):
   if self.title=='QUEUE':self.sel=self.a.queue_move(self.sel,1,self);return
   if self.current_playlist_pid is not None and self.title.startswith('PLAYLIST ·'):self.sel=self.a.playlist_move(self.current_playlist_pid,self.sel,1,self);return
  if k==ord('K'):
   if self.title=='QUEUE':self.sel=self.a.queue_move(self.sel,-1,self);return
   if self.current_playlist_pid is not None and self.title.startswith('PLAYLIST ·'):self.sel=self.a.playlist_move(self.current_playlist_pid,self.sel,-1,self);return
  if k==ord('R') and self.current_playlist_pid is not None and self.title.startswith('PLAYLIST ·'):self.a.playlist_rename(self.current_playlist_pid,self);return
  if k==ord('D') and self.current_playlist_pid is not None and self.title.startswith('PLAYLIST ·'):self.a.playlist_delete(self.current_playlist_pid,self);return
  if k==16:
   q=self.prompt('UNIVERSAL QUICK FIND › ');self.a.search_async(q,self);return
  if k==ord('/'):
   if self.mode in ('list','menu') and self.items:
    q=self.prompt('FILTER CURRENT LIST › ');self.filter_current(q);return
   q=self.prompt('UNIVERSAL QUICK FIND › ');self.a.search_async(q,self);return
  if k==ord('S'):
   self.cycle_sort();return
  if k==ord(':'):
   q=self.prompt(':',complete=True);
   if q is not None:self.a.command(q,self)
   else:self.status('Command cancelled')
   return
  if k==ord('u'):self.a.scan_async(self);return
  if k==ord('f') and self.a.current:self.status('Favorite added' if self.a.db.favorite(self.a.current) else 'Favorite removed');return
  if k==ord('v'):self.cycle_layout();return
  if k==ord('V'):self.toggle_visualizer();return
  if k==ord('.'):self.action_menu();return
  if k==ord('t'):self.theme_menu();return
  if k==ord('i'):
   self.context_target=self.selected() or self.a.current;self.push();self.mode='context';self._view_changed();return
  if k in (10,13,curses.KEY_ENTER):
   if self.items:self.activate()
   elif self.mode=='home' and self.a.current:self.a.play_pause(self)
 def activate(self):
  x=self.items[self.sel]
  if isinstance(x,tuple):
   action=x[1]
   if not action:return
   if action.startswith('theme:'):self.set_theme(action.split(':',1)[1]);self.theme_menu();return
   if action.startswith('nav:'):self.a.nav_action(action.split(':',1)[1],self)
   else:self.a.open_source(action,self)
  elif hasattr(x,'url'):
   if self.title=='QUEUE':self.a.queue_play(self.sel,self)
   else:
    playable=[z for z in self.items if not isinstance(z,tuple) and getattr(z,'kind','') not in ('album','artist','playlist','program')]
    if x in playable:
     if getattr(x,'kind','')=='radio':self.a.set_play_context([],0)
     else:self.a.set_play_context(playable,playable.index(x))
    self.push();self.a.open_item(x,self)
 def filter_current(self,q):
  if q is None:return self.status('Filter cancelled')
  if self.unfiltered_items is None:self.unfiltered_items=list(self.items)
  q=(q or '').strip().casefold()
  if not q:
   self.items=list(self.unfiltered_items);self.sel=0;self.status(f'Filter cleared · {len(self.items)} items');self._view_changed();return
  def text(x):
   if isinstance(x,tuple):return ' '.join(str(v) for v in x)
   return ' '.join(str(v or '') for v in (getattr(x,'title',''),getattr(x,'artist',''),getattr(x,'album',''),(getattr(x,'meta',{}) or {}).get('published_date','')))
  self.items=[x for x in self.unfiltered_items if q in text(x).casefold()];self.sel=0;self.status(f'Filter “{q}” · {len(self.items)} matches');self._view_changed()
 def cycle_sort(self):
  tracks=[x for x in self.items if not isinstance(x,tuple)]
  if not tracks or not any((getattr(x,'meta',{}) or {}).get('published_ts') for x in tracks):return self.status('Sort · no publication dates in this view')
  if self.unfiltered_items is None:self.unfiltered_items=list(self.items)
  modes=['relevance','newest','oldest'];i=modes.index(self.sort_mode) if self.sort_mode in modes else 0;self.sort_mode=modes[(i+1)%3]
  if self.sort_mode=='relevance' and self.unfiltered_items is not None:self.items=list(self.unfiltered_items)
  elif self.sort_mode!='relevance':self.items=sorted(self.items,key=lambda x:float((getattr(x,'meta',{}) or {}).get('published_ts') or 0),reverse=self.sort_mode=='newest')
  self.sel=0;self.status('Sort · '+self.sort_mode.capitalize());self.invalidate()
