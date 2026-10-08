import curses,time,os,shutil
from . import __version__
from .ui_common import ACTIONS,clip
import platform

class RenderingMixin:
 def draw_transport_frame(self):
  if not self.st or self.mode=='home' or not self.a.current:return
  st=self.st;h,w=st.getmaxyx();t=self.a.current
  if t.kind=='radio':footer=f'● {t.title} · '+((((self.a.radio_info.get(t.id,{}) or {}).get('program')) or 'LIVE') if t.source=='sr' else 'LIVE')
  else:footer=f'▶ {t.artist} · {t.title}   {self.a.fmt(self.play_pos)} / {self.a.fmt(self.play_dur or t.duration or 0)}'
  try:
   st.move(h-2,0);st.clrtoeol();st.addnstr(h-2,2,clip(footer,w-4),w-4,self.dim_attr());st.noutrefresh();curses.doupdate()
  except curses.error:pass
 def footer_help(self,w):
  if w<76:return '? help  ^N now  n/p track  b browse  Q quit'
  if self.title=='QUEUE':return 'Enter play   d remove   J/K move   n/p track   Esc back   ? help'
  if self.title.startswith('PLAYLIST'):return 'Enter play   r random   d remove   J/K move   R rename   D delete   Esc back   ? help'
  if self.mode in ('list','menu'):return 'Enter open/play   / filter   S sort   . actions   a queue   A playlist   r random   Esc back   ? help'
  return 'SPACE resume   z stop   n/p track   Enter resume   b browse   ? help' if (self.a.current and not self.a.playback_active() and (self.a.resume_candidate or self.a.current.source=='spotify')) else 'SPACE pause   z stop   n/p track   ←/→ seek   +/- volume   v view   i info   b browse   t theme   ? help'
 def draw(self):
  st=self.st;st.bkgd(' ',curses.color_pair(1));st.erase();h,w=st.getmaxyx()
  if h<12 or w<42:st.addstr(0,0,'NPLAY · terminal too small');st.refresh();return
  st.addnstr(1,2,'NPLAY',w-4,curses.A_BOLD|curses.color_pair(3));right=__version__;st.addnstr(1,max(8,w-len(right)-2),right,len(right),self.dim_attr())
  if self.mode=='home':self.draw_home(st,h,w)
  elif self.mode in ('menu','list'):self.draw_list(st,h,w)
  elif self.mode=='help':self.draw_help(st,h,w)
  elif self.mode=='about':self.draw_about(st,h,w)
  elif self.mode=='context':self.draw_context(st,h,w)
  elif self.mode=='error':self.draw_error(st,h,w)
  elif self.mode=='diagnostics':self.draw_diagnostics(st,h,w)
  st.hline(h-3,2,curses.ACS_HLINE,max(1,w-4));footer=self.message
  if self.mode!='home' and self.a.current:
   t=self.a.current
   if t.kind=='radio':footer=f'● {t.title} · '+((((self.a.radio_info.get(t.id,{}) or {}).get('program')) or 'LIVE') if t.source=='sr' else 'LIVE')
   else:
    pos=self.play_pos;dur=self.play_dur or t.duration or 0;footer=f'▶ {t.artist} · {t.title}   {self.a.fmt(pos)} / {self.a.fmt(dur)}'
  st.addnstr(h-2,2,clip(footer,w-4),w-4,self.dim_attr());st.addnstr(h-1,2,self.footer_help(w),w-4,self.dim_attr());st.refresh()
  if self.mode=='home':self.render_art(h,w)
  elif self.mode in ('list','menu'):
   if not self.preview_due or time.monotonic()>=self.preview_due:self.render_preview(h,w)
  elif self.art_visible:self.art.clear();self.art_visible=False
 def home_geometry(self,h,w):
  # Keep artwork useful on compact Kitty terminals by shrinking it before hiding it.
  # Below these dimensions there is not enough room for metadata + transport + footer.
  if w<42 or h<16 or not self.art.kitty() or not self.a.cfg.getbool('kitty_artwork',True):return {'art':False,'cols':0,'rows':0,'row':4,'col':4,'textx':4}
  compact=w<70 or h<20
  if compact:
   # A small left-hand cover leaves at least ~24 columns for title/progress.
   cols=max(10,min(18,w-32));rows=max(4,min(7,h-10,int(cols*.55)));textx=cols+7
  elif self.layout=='artwork':cols=min(52,max(24,w//2-8));rows=min(max(9,h-13),max(12,int(cols*.62)));textx=min(w-28,cols+8)
  elif self.layout=='visualizer':cols=min(24,max(14,w//5));rows=min(max(7,h//4),max(8,int(cols*.55)));textx=min(w-28,cols+8)
  else:cols=min(34,max(16,w//3));rows=min(max(8,h-14),max(9,int(cols*.60)));textx=min(w-28,cols+8)
  if rows<4 or w-textx<20:return {'art':False,'cols':0,'rows':0,'row':4,'col':4,'textx':4}
  return {'art':True,'cols':cols,'rows':rows,'row':4,'col':4,'textx':textx}
 def preview_geometry(self,h,w):
  # List previews scale down instead of disappearing at the old 105-column cutoff.
  if w<76 or h<17:return None
  cols=max(10,min(24,w//5));rows=max(4,min(14,h-10,int(cols*.55)));col=w-cols-4
  right=col-3
  if right<44:return None
  return {'cols':cols,'rows':rows,'row':5,'col':col,'right':right}
 def render_art(self,h,w):
  t=self.a.current;g=self.home_geometry(h,w)
  if not t or not t.cover or not g['art'] or not self.a.cfg.getbool('kitty_artwork',True):
   if self.art_visible:self.art.clear();self.art_visible=False
   return
  self.art.prefetch(t.cover);shown=self.art.draw(t.cover,g['row'],g['col'],g['cols'],g['rows'])
  if shown:self.art_visible=True
 def render_preview(self,h,w):
  x=self.selected();g=self.preview_geometry(h,w)
  if not x or not getattr(x,'cover','') or not g or not self.art.kitty() or not self.a.cfg.getbool('kitty_artwork',True):
   if self.art_visible:self.art.clear();self.art_visible=False
   return
  self.art.prefetch(x.cover);shown=self.art.draw(x.cover,g['row'],g['col'],g['cols'],g['rows'])
  if shown:self.art_visible=True
 def placeholder(self,st,x,y,w,source,label=''):
  lines={'sr':['┌──────────┐','│    SR    │',f'│ {label:^8} │','│   LIVE   │','└──────────┘'],'youtube':['┌──────────┐','│ YOUTUBE  │','│  AUDIO   │','└──────────┘'],'local':['┌──────────┐','│  NPLAY   │','│  LOCAL   │','└──────────┘'],'navidrome':['┌──────────┐','│  NPLAY   │','│  ALBUM   │','└──────────┘'],'spotify':['┌──────────┐','│ SPOTIFY  │','│  NPLAY   │','└──────────┘']}.get(source,['┌──────────┐','│  NPLAY   │','└──────────┘'])
  for i,s in enumerate(lines):st.addnstr(y+i,x,s,min(w,len(s)),self.dim_attr())
 def draw_home(self,st,h,w):
  t=self.a.current
  if not t:
   y=max(5,h//3);st.addstr(y,4,'Nothing playing',curses.A_BOLD);st.addstr(y+2,4,'b  Browse your music');st.addstr(y+3,4,'^P Universal Quick Find');st.addstr(y+4,4,':  Command palette');return
  g=self.home_geometry(h,w);x=g['textx'] if g['art'] else 4
  if g['art'] and not t.cover:self.placeholder(st,5,5,g['cols'],t.source,t.title if t.kind=='radio' else '')
  st.addnstr(4,x,clip(t.title,w-x-4),w-x-4,curses.A_BOLD);st.addnstr(5,x,clip(t.artist,w-x-4),w-x-4);st.addnstr(6,x,clip(t.album,w-x-4),w-x-4,self.dim_attr())
  meta=t.source.upper();
  if t.source=='spotify':
   local=self.a.cfg.get('spotify_playback','local')=='local';dev=(self.a.spotify_state.get('device') or {}).get('name','')
   meta+=' · '+('LIBRESPOT' if local else 'CONNECT')
   if local:meta+=' · '+(('PAUSED' if not self.a.playback_active() else str(self.a.cfg.get('spotify_local_bitrate','320'))+' KBPS'))
   elif dev:meta+=' · '+str(dev)
  br=t.meta.get('bitrate') if t.meta else None;suf=t.meta.get('suffix') if t.meta else None
  if suf:meta+=f' · {str(suf).upper()}'
  if br:meta+=f' · {br} kbps'
  st.addnstr(8,x,meta,w-x-4,self.dim_attr());barw=max(10,w-x-5)
  if self.a.resume_candidate and not self.a.playback_active():meta_hint='Resume with SPACE · '+self.a.fmt(self.a.resume_position);st.addnstr(9,x,meta_hint,barw,self.dim_attr())
  content_bottom=13
  if t.kind=='radio':
   info=self.a.radio_info.get(t.id,{}) or {};st.addnstr(10,x,'● LIVE · '+t.title,barw,curses.A_BOLD);content_bottom=11
   prog=(info.get('program') or '').strip();ep=(info.get('episode') or '').strip();song=(info.get('song') or '').strip()
   if prog:st.addnstr(content_bottom,x,clip(prog,barw),barw);content_bottom+=1
   if ep and ep.casefold()!=prog.casefold():st.addnstr(content_bottom,x,clip(ep,barw),barw,self.dim_attr());content_bottom+=1
   times='–'.join(z for z in (info.get('start',''),info.get('end','')) if z)
   if times:st.addnstr(content_bottom,x,clip(times,barw),barw,self.dim_attr());content_bottom+=1
   if song:st.addnstr(content_bottom,x,clip('♪ '+song,barw),barw,self.dim_attr());content_bottom+=1
   nxt=(info.get('next_program') or '').strip()
   if nxt and content_bottom<h-7:
    nt=(info.get('next_start') or '').strip();st.addnstr(content_bottom,x,clip('NEXT  '+nxt+(' · '+nt if nt else ''),barw),barw,self.dim_attr());content_bottom+=1
  else:
   pos=self.play_pos;dur=self.play_dur or t.duration or 0;ratio=min(1,pos/dur) if dur else 0;n=min(barw-1,int((barw-1)*ratio));st.addnstr(10,x,'━'*n+'●'+'─'*max(0,barw-n-1),barw);st.addnstr(11,x,f'{self.a.fmt(pos)}   {self.a.fmt(dur)}',barw,self.dim_attr());content_bottom=12
  flags=[]
  if t.kind!='radio' and t.source!='spotify' and self.a.cfg.getbool('normalization',True):flags.append('RG '+self.a.normalization_mode(t).upper())
  if t.kind!='radio' and t.source!='spotify' and self.a.cfg.getbool('gapless',False):flags.append('GAPLESS')
  if self.a.shuffle_mode!='off':flags.append('SHUFFLE '+self.a.shuffle_mode.upper())
  if self.a.repeat_mode!='off':flags.append('REPEAT '+self.a.repeat_mode.upper())
  if t.kind!='radio' and self.a.play_context and 0<=self.a.context_index<len(self.a.play_context) and content_bottom<h-7:
   same_album=t.album and len({z.album for z in self.a.play_context if getattr(z,'album','')})==1
   context_name=t.album if same_album else ((t.meta or {}).get('context_name') or ('Spotify context' if t.source=='spotify' else 'Playback context'))
   st.addnstr(content_bottom,x,clip(f'{self.a.context_index+1} of {len(self.a.play_context)} · {context_name}',barw),barw,self.dim_attr());content_bottom+=1
  nxt=None if t.kind=='radio' else self.a.up_next()
  if nxt and content_bottom<h-7:
   import re
   nt=re.sub(r'^\s*\d{1,3}\s*[-._]\s*','',nxt.title or '').strip();artist=(nxt.artist or '').strip()
   # Avoid common "Artist - Title" duplication from filenames/tags.
   if artist and nt.casefold().startswith(artist.casefold()+' - '):nt=nt[len(artist)+3:].strip()
   st.addnstr(content_bottom,x,clip('UP NEXT  '+((artist+' · ') if artist else '')+nt,barw),barw,self.dim_attr());content_bottom+=1
  if flags and content_bottom<h-6:
   st.addnstr(content_bottom,x,' · '.join(flags),barw,self.dim_attr());content_bottom+=1
  if self.visual:
   available=max(0,h-4-max(content_bottom,g['row']+g['rows'] if g['art'] and self.layout=='artwork' else content_bottom)-1)
   if self.layout=='visualizer':vh=max(3,min(available,max(8,h//2)))
   elif self.layout=='artwork':vh=max(2,min(available,6))
   else:vh=max(3,min(available,max(5,h//4)))
   if vh>=2:
    top=h-4-vh;vx=3;self.draw_visual(st,top,h-4,w,vx)
 def draw_visual(self,st,y,bottom,w,x):
  rows=max(1,bottom-y);width=max(8,w-x-4);bars=max(12,width);vals=self.viz.get(bars);style=self.visual_style
  if style=='classic':
   frac=' ▁▂▃▄▅▆▇█'
   for r in range(rows):
    th=rows-r-1;line=''.join(frac[min(8,int(round(max(0,min(1,(v/255)*rows-th))*8)))] for v in vals)
    try:st.addnstr(y+r,x,line,width,self.dim_attr())
    except curses.error:pass
   return
  # Terminal-safe styles: no CAVA config rewrite and no playback interruption.
  # Gradient uses existing semantic theme colours, so it also works on 16-colour terminals.
  for r in range(rows):
   th=rows-r-1
   if style=='dots':line=''.join('•' if (v/255)*rows>th+.35 else ' ' for v in vals)
   else:line=''.join('█' if (v/255)*rows>th else ' ' for v in vals)
   if style=='gradient':
    ratio=(rows-r-1)/max(1,rows-1);attr=curses.color_pair(3) if ratio>.66 else (curses.color_pair(1) if ratio>.30 else self.dim_attr())
   elif style=='dots':attr=curses.color_pair(3)
   else:attr=self.dim_attr()
   try:st.addnstr(y+r,x,line,width,attr)
   except curses.error:pass
 def draw_visual_frame(self):
  if not self.st or self.mode!='home' or not self.a.current or not self.visual:return
  st=self.st;h,w=st.getmaxyx();g=self.home_geometry(h,w);content_bottom=12
  t=self.a.current
  if t.kind=='radio':content_bottom=13+(1 if (self.a.radio_info.get(t.id,{}) or {}).get('song') else 0)
  if self.a.up_next():content_bottom+=1
  flags=[]
  if self.a.cfg.getbool('normalization',True):flags.append(1)
  if self.a.cfg.getbool('gapless',False):flags.append(1)
  if self.a.shuffle_mode!='off':flags.append(1)
  if self.a.repeat_mode!='off':flags.append(1)
  if flags:content_bottom+=1
  available=max(0,h-4-max(content_bottom,g['row']+g['rows'] if g['art'] and self.layout=='artwork' else content_bottom)-1)
  vh=max(3,min(available,max(8,h//2))) if self.layout=='visualizer' else (max(2,min(available,6)) if self.layout=='artwork' else max(3,min(available,max(5,h//4))))
  if vh<2:return
  top=h-4-vh
  # Own and clear only CAVA's rectangle; footer/separator are never touched.
  blank=' '*max(1,w-7)
  for y in range(top,h-4):
   try:st.addnstr(y,3,blank,w-7)
   except curses.error:pass
  self.draw_visual(st,top,h-4,w,3)
  try:st.noutrefresh();curses.doupdate()
  except curses.error:pass
 def draw_list(self,st,h,w):
  heading=self.title + (f'  ·  SORT {self.sort_mode.upper()}' if any(not isinstance(z,tuple) and (getattr(z,'meta',{}) or {}).get('published_ts') for z in self.items) else '')
  st.addnstr(3,4,heading,w-8,curses.A_BOLD);pg=self.preview_geometry(h,w);preview=bool(pg);right=pg['right'] if pg else w-4;visible=max(1,h-8);start=max(0,min(self.sel-visible//2,max(0,len(self.items)-visible)));base=5
  for row,i in enumerate(range(start,min(len(self.items),start+visible)),base):
   x=self.items[i]
   if isinstance(x,tuple):
    if not x[1]:
     st.addnstr(row,4,x[0],max(1,right-6),curses.A_BOLD|curses.color_pair(3));continue
    namew=min(max((len(str(z[0])) for z in self.items if isinstance(z,tuple) and z[1]),default=18)+4,max(22,right//2));label=f'{x[0]:<{namew}}{x[2]}'
   else:
    maxline=max(16,right-8)
    if x.kind=='radio':info=self.a.radio_info.get(x.id,{}) or {};label=f'{x.title:<10} {info.get("program") or "LIVE"}'
    elif x.kind=='program':label=f'{x.title}  ·  {x.artist}'
    elif x.kind=='album':label=f'{x.title:<38} {x.artist}'
    elif x.kind=='artist':label=x.title
    elif x.kind=='podcast':
     date=(x.meta or {}).get('published_date','');label=f'{x.title:<46} {x.artist:<22} {date}' if right>=96 else f'{x.title}  ·  {x.artist}'+(f'  ·  {date}' if date else '')
    elif x.source=='youtube':
     date=(x.meta or {}).get('published_date','');label=f'{x.title:<46} {x.artist:<22} {date}' if right>=96 else f'{x.title}  ·  {x.artist}'+(f'  ·  {date}' if date else '')
    else:label=f'{x.title}  ·  {x.artist}'
    label=clip(label,maxline)
   st.addnstr(row,4,('› ' if i==self.sel else '  ')+label,max(1,right-6),(curses.color_pair(4)|curses.A_REVERSE) if i==self.sel and self.theme=='niru-noir' else (curses.color_pair(4) if i==self.sel else curses.color_pair(1)))
  if not self.items:st.addnstr(6,4,clip(self.empty,w-10),w-10,self.dim_attr())
  x=self.selected()
  if x and preview:
   cx=right+2;cw=max(12,w-cx-4);hasart=bool(getattr(x,'cover','')) and self.art.kitty() and self.a.cfg.getbool('kitty_artwork',True)
   if not hasart:self.placeholder(st,cx,6,cw,x.source,(x.title if x.kind=='radio' else x.source.upper())[:8])
   y=min(h-8,(pg['row']+pg['rows']+1) if hasart and pg else 12)
   if y<h-5:
    st.addnstr(y,cx,clip(x.title,cw),cw,curses.A_BOLD);y+=1
    if x.artist:st.addnstr(y,cx,clip(x.artist,cw),cw);y+=1
    details=[]
    if x.kind=='album':details=[str(x.meta.get('year') or ''),(str(x.meta.get('song_count'))+' tracks') if x.meta.get('song_count') else '']
    elif x.kind=='program':details=['SR programme']
    elif x.kind=='podcast':details=[(x.meta or {}).get('published_date',''),self.a.fmt(x.duration) if x.duration else 'SR episode']
    elif x.source=='youtube':details=[(x.meta or {}).get('published_date',''),self.a.fmt(x.duration) if x.duration else '']
    if any(details):st.addnstr(y,cx,clip(' · '.join(v for v in details if v),cw),cw,self.dim_attr())
 def draw_help(self,st,h,w):
  st.addstr(3,4,'HELP · KEYBINDINGS',curses.A_BOLD);st.addnstr(4,4,'Esc goes back one view · Ctrl+N opens Now Playing · Ctrl+P searches all sources · / filters the current view · n/p changes track · t changes theme · v changes layout',w-8,self.dim_attr());y=6
  for key,desc in ACTIONS:
   if y>=h-4:break
   if not desc:st.addnstr(y,4,key,w-8,curses.A_BOLD);y+=1;continue
   st.addnstr(y,5,f'{key:<16} {desc}',w-10);y+=1
 def draw_about(self,st,h,w):
  lines=[f'NPLAY {__version__}','Terminal-native music, radio & podcast player','','Custom-built for Nicklas Rudolfsson.','Fully functional and configurable for other users too.','',f'Python {platform.python_version()}',f'Terminal {os.getenv("TERM","unknown")}',f'Kitty artwork {"available" if self.art.kitty() else "fallback"}',f'mpv {"available" if shutil.which("mpv") else "missing"}',f'CAVA {"available" if shutil.which("cava") else "optional / missing"}',f'yt-dlp {"available" if shutil.which("yt-dlp") else "optional / missing"}','','Local · Navidrome · Spotify · Sveriges Radio · YouTube','Normal · Artwork · Visualizer views','Keyboard first · responsive · provider resilient']
  y=4
  for i,line in enumerate(lines):st.addnstr(y+i,5,line,w-10,curses.A_BOLD if i==0 else self.dim_attr() if i in (1,13,14) else 0)
 def draw_context(self,st,h,w):
  t=self.context_target or self.a.current;st.addstr(3,4,'CONTEXT',curses.A_BOLD)
  if not t:st.addstr(5,5,'Nothing selected or playing',self.dim_attr());return
  rows=[('Title',t.title),('Artist',t.artist),('Album',t.album),('Source',t.source.upper()),('Type',t.kind),('Duration',self.a.fmt(t.duration) if t.duration else 'Live / unknown'),('NPLAY rating',('★'*self.a.db.rating_get(t.id,t.source)) or '—')]
  if getattr(t,'path',''):rows.append(('File',clip(t.path,max(20,w-22))))
  if t is self.a.current and self.a.play_context:
   rows.append(('Playing from',f'{self.a.context_index+1} / {len(self.a.play_context)}'))
   if self.a.context_index+1<len(self.a.play_context):rows.append(('Up next',self.a.play_context[self.a.context_index+1].title))
  if t.meta:
   if t.meta.get('available_sources'):rows.append(('Available', ' · '.join(str(x).capitalize() for x in t.meta['available_sources'])))
   if t.meta.get('play_count'):rows.append(('Play count',str(t.meta['play_count'])))
   if t.meta.get('suffix'):rows.append(('Format',str(t.meta['suffix']).upper()))
   if t.meta.get('bitrate'):rows.append(('Bitrate',str(t.meta['bitrate'])+' kbps'))
   if t.meta.get('year'):rows.append(('Year',str(t.meta['year'])))
   if t.meta.get('genre'):rows.append(('Genre',str(t.meta['genre'])))
   if t.meta.get('track_no'):rows.append(('Track',str(t.meta['track_no'])))
   if t.meta.get('disc_no'):rows.append(('Disc',str(t.meta['disc_no'])))
   if t.meta.get('codec'):rows.append(('Codec',str(t.meta['codec']).upper()))
   if t.meta.get('sample_rate'):rows.append(('Sample rate',f"{int(t.meta['sample_rate'])/1000:g} kHz"))
   if t.meta.get('bit_depth'):rows.append(('Bit depth',str(t.meta['bit_depth'])+' bit'))
   if t.meta.get('channels'):rows.append(('Channels',str(t.meta['channels'])))
   if t.meta.get('description'):rows.append(('About',clip(t.meta['description'],max(20,w-22))))
  y=5
  for a,b in rows:
   if y>=h-4:break
   st.addnstr(y,5,f'{a:<12} {b}',w-10);y+=1
 def draw_error(self,st,h,w):
  st.addnstr(3,4,self.title,w-8,curses.A_BOLD);st.addnstr(6,4,clip(self.empty,w-8),w-8);st.addnstr(8,4,'Esc  Back',w-8,self.dim_attr());st.addnstr(9,4,'NPLAY remains available.',w-8,self.dim_attr())
 def draw_diagnostics(self,st,h,w):
  a=self.a;st.addstr(3,4,'DIAGNOSTICS',curses.A_BOLD);lines=[f'NPLAY          {__version__}',f'Input poll     25 ms',f'Now view       {self.layout}',f'Render         responsive / dirty / 30 fps visual',f'CAVA           {"running" if self.visual else "off"}',f'Artwork        {"Kitty" if self.art.kitty() else "Unicode fallback"}',f'MPRIS          '+('ready' if a.mpris.available else ('enabled · unavailable' if a.cfg.getbool('mpris_enabled',True) else 'off')),f'Notifications  '+(('on · artwork' if a.cfg.getbool('notification_artwork',True) else 'on · text only') if a.cfg.getbool('track_notifications',True) and a.notifier.available() else ('enabled · unavailable' if a.cfg.getbool('track_notifications',True) else 'off')),f'Spotify        '+(('enabled / connected' if a.spotify_configured() else 'enabled / setup required') if a.cfg.getbool('spotify_enabled',False) else 'disabled'),f'Navidrome      '+(('enabled / configured' if a.nav_configured() else 'enabled / not configured') if a.cfg.getbool('navidrome_enabled',True) else 'disabled'),f'SR             '+(('enabled · cached fallback' if a.sr.last_cache else 'enabled · live/cache') if a.cfg.getbool('sr_enabled',True) else 'disabled'),f'YouTube        '+(('enabled · yt-dlp '+(a.yt.version() or 'unknown')) if a.yt.available() else 'enabled · yt-dlp missing') if a.cfg.getbool('youtube_enabled',True) else 'disabled',f'Normalization  '+(('on · '+a.cfg.get('normalization_mode','auto')) if a.cfg.getbool('normalization',True) else 'off'),f'Gapless        '+('on' if a.cfg.getbool('gapless',False) else 'off'),f'Shuffle        {a.shuffle_mode}',f'Repeat         {a.repeat_mode}',f'Queue          {len(a.queue)} items',f'Log            {__import__("nplay.logging_utils",fromlist=["recent_error_count"]).recent_error_count()} recent warnings/errors · {__import__("nplay.logging_utils",fromlist=["LOG_PATH"]).LOG_PATH}']
  for i,line in enumerate(lines):st.addnstr(5+i,5,line,w-10)
