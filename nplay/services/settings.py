class SettingsController:
 def __init__(self,app):self.a=app
 def open(self,s,ui):
  a=self.a;c=a.cfg
  if s=='settings':ui.show_menu('SETTINGS',[('APPEARANCE','settings:appearance','Theme · Now Playing · visualizer'),('SOURCES','settings:sources','Navidrome · Spotify · Sveriges Radio · YouTube'),('LIBRARY','settings:library','Music folders · recursive scan'),('PLAYBACK','settings:playback','Queue and playback behaviour'),('SYSTEM','settings:system','Diagnostics · capabilities')])
  elif s=='settings:appearance':ui.show_menu('SETTINGS · APPEARANCE',[('THEME','settings:theme',ui.theme_label()),('NOW PLAYING VIEW','settings:view',ui.layout.capitalize()),('VISUALIZER','settings:visual','On' if ui.visual else 'Off'),('CAVA STYLE','settings:visual-style',ui.visual_style.capitalize()+' · Classic is default')])
  elif s=='settings:sources':ui.show_menu('SETTINGS · SOURCES',[('NAVIDROME','settings:toggle-nav','On · configured' if c.getbool('navidrome_enabled',True) and a.nav_configured() else ('On · set up server' if c.getbool('navidrome_enabled',True) else 'Off')),('SPOTIFY','settings:toggle-spotify',('On · connected' if a.spotify_configured() else 'On · setup required') if c.getbool('spotify_enabled',False) else 'Off'),('CONFIGURE SPOTIFY','spotify:setup','OAuth PKCE · Spotify Premium'),('SVERIGES RADIO','radio:toggle-sr','On' if c.getbool('sr_enabled',True) else 'Off'),('YOUTUBE','settings:toggle-yt','On' if c.getbool('youtube_enabled',True) else 'Off'),('CUSTOM RADIO','settings:toggle-custom','On' if c.getbool('custom_radio_enabled',True) else 'Off'),('CONFIGURE NAVIDROME','nav:setup','Server · username · password'),('PREFERRED SOURCE','settings:preferred-source',c.get('preferred_source','local').capitalize()+' · cross-source matches')])
  elif s=='settings:library':ui.show_menu('SETTINGS · LIBRARY',[('MUSIC FOLDERS','local:folders',' · '.join(a.roots())),('RESCAN LIBRARY','local:scan','Recursive · tags · embedded artwork'),('AUTO REFRESH','settings:toggle-autoscan','On startup' if c.getbool('auto_library_refresh',True) else 'Off'),('ARTWORK CACHE','settings:art-cache','Size · limit · clear')])
  elif s=='settings:playback':ui.show_menu('SETTINGS · PLAYBACK',[('NORMALIZATION','settings:toggle-normalize','On · '+c.get('normalization_mode','auto').capitalize() if c.getbool('normalization',True) else 'Off'),('NORMALIZATION MODE','settings:normalize-mode',c.get('normalization_mode','auto').capitalize()),('GAPLESS','settings:toggle-gapless','On' if c.getbool('gapless',False) else 'Off · default'),('SHUFFLE','settings:shuffle',a.shuffle_mode.capitalize()),('REPEAT','settings:repeat',a.repeat_mode.capitalize()),('TRACK NOTIFICATIONS','settings:toggle-notify','On · system notification on track change' if c.getbool('track_notifications',True) else 'Off'),('NOTIFICATION ARTWORK','settings:toggle-notify-art','On' if c.getbool('notification_artwork',True) else 'Off'),('TEST NOTIFICATION','settings:test-notify','Current track · or NPLAY test'),('MPRIS / MEDIA KEYS','settings:toggle-mpris','On · playerctl / desktop integration' if c.getbool('mpris_enabled',True) else 'Off'),('QUEUE','queue',f'{len(a.queue)} items'),('NOW PLAYING VIEW','settings:view',ui.layout.capitalize()),('VISUALIZER','settings:visual','On' if ui.visual else 'Off')])
  elif s=='settings:system':ui.show_menu('SETTINGS · SYSTEM',[('DIAGNOSTICS','settings:diag','Runtime capabilities · sources · renderer')])
  elif s=='settings:preferred-source':
   modes=['local','navidrome','spotify','youtube'];cur=c.get('preferred_source','local');c.set('preferred_source',modes[(modes.index(cur)+1)%len(modes)] if cur in modes else 'local');self.open('settings:sources',ui)
  elif s in ('settings:toggle-nav','settings:toggle-spotify','settings:toggle-yt','settings:toggle-custom'):
   key={'settings:toggle-nav':'navidrome_enabled','settings:toggle-spotify':'spotify_enabled','settings:toggle-yt':'youtube_enabled','settings:toggle-custom':'custom_radio_enabled'}[s];default=False if key=='spotify_enabled' else True;enabled=not c.getbool(key,default);c.set(key,'true' if enabled else 'false');ui.status(key.replace('_enabled','').replace('_',' ').title()+' '+('enabled' if enabled else 'disabled'));self.open('settings:sources',ui)
  elif s=='settings:toggle-normalize':
   enabled=not c.getbool('normalization',True);c.set('normalization','true' if enabled else 'false');ui.status('Volume normalization '+('enabled' if enabled else 'disabled'));self.open('settings:playback',ui)
  elif s=='settings:normalize-mode':
   modes=['auto','track','album'];cur=c.get('normalization_mode','auto');i=modes.index(cur) if cur in modes else 0;c.set('normalization_mode',modes[(i+1)%len(modes)]);self.open('settings:playback',ui)
  elif s=='settings:toggle-gapless':
   enabled=not c.getbool('gapless',False);c.set('gapless','true' if enabled else 'false');ui.status('Gapless '+('enabled · applies from next track' if enabled else 'disabled'));self.open('settings:playback',ui)
  elif s=='settings:toggle-notify':
   enabled=not c.getbool('track_notifications',True);c.set('track_notifications','true' if enabled else 'false');ui.status('Track notifications '+('enabled' if enabled else 'disabled'));self.open('settings:playback',ui)
  elif s=='settings:toggle-notify-art':
   enabled=not c.getbool('notification_artwork',True);c.set('notification_artwork','true' if enabled else 'false');ui.status('Notification artwork '+('enabled' if enabled else 'disabled'));self.open('settings:playback',ui)
  elif s=='settings:toggle-mpris':
   enabled=not c.getbool('mpris_enabled',True);c.set('mpris_enabled','true' if enabled else 'false');ui.status('MPRIS '+('enabled · restart NPLAY to activate' if enabled else 'disabled · restart NPLAY'));self.open('settings:playback',ui)
  elif s=='settings:test-notify':ui.status('Test notification sent' if a.notifier.test(a.current) else 'System notifications unavailable · notify-send / desktop session required')
  elif s=='settings:shuffle':a.cycle_shuffle(ui);self.open('settings:playback',ui)
  elif s=='settings:repeat':a.cycle_repeat(ui);self.open('settings:playback',ui)
  elif s=='settings:toggle-autoscan':
   enabled=not c.getbool('auto_library_refresh',True);c.set('auto_library_refresh','true' if enabled else 'false');ui.status('Automatic library refresh '+('enabled' if enabled else 'disabled'));self.open('settings:library',ui)
  elif s=='settings:art-cache':
   n,b=a.artwork_cache.stats();ui.show_menu('SETTINGS · ARTWORK CACHE',[('USAGE','',f'{n:,} files · {b/1024/1024:.1f} MB'),('LIMIT','settings:art-cache-limit',c.get('artwork_cache_mb','500')+' MB'),('CLEAR CACHE','settings:art-cache-clear','Downloaded and extracted artwork · safe to rebuild')])
  elif s=='settings:art-cache-limit':
   v=ui.prompt('ARTWORK CACHE LIMIT MB › ')
   try:c.set('artwork_cache_mb',str(max(32,int(v))));a.artwork_cache.max_bytes=max(32,int(v))*1024*1024;a.artwork_cache.prune();ui.status('Artwork cache limit updated')
   except (ValueError,TypeError):ui.status('Enter a size in MB')
  elif s=='settings:art-cache-clear':ui.status(f'Artwork cache cleared · {a.artwork_cache.clear()} files')
  elif s=='settings:theme':ui.theme_menu()
  elif s=='settings:theme-reload':
   from ..theme import available,apply
   names=dict(available())
   if ui.theme not in names:ui.set_theme('niru-noir')
   else:ui.palette=apply(ui.theme);ui.invalidate()
   ui.status(f'Themes reloaded · {len(names)} available');ui.theme_menu()
  elif s=='settings:theme-help':
   from ..theme import THEME_DIR
   ui.show_menu('CUSTOM THEMES · INSTALL GUIDE',[
    ('THEME DIRECTORY','',str(THEME_DIR)),
    ('','', ''),
    ('1  Copy a .toml theme file','', 'Place it in the directory above'),
    ('2  Reload custom themes','', 'Use the action below'),
    ('3  Select your new theme','', 'Choose it in the theme list'),
    ('','', ''),
    ('OPEN THEME DIRECTORY','settings:theme-open','Open in your desktop file manager'),
    ('RELOAD CUSTOM THEMES','settings:theme-reload','Scan for installed themes'),
    ('BACK TO THEMES','settings:theme','Select an installed theme')])
  elif s=='settings:theme-open':
   from ..theme import THEME_DIR
   from ..theme_actions import open_theme_directory
   ok,msg=open_theme_directory(THEME_DIR)
   ui.status(msg)
  elif s=='settings:view':ui.cycle_layout()
  elif s=='settings:visual':ui.toggle_visualizer()
  elif s=='settings:visual-style':ui.visualizer_style_menu()
  elif s=='settings:diag':ui.show_diagnostics()
