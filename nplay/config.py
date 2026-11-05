from pathlib import Path
import configparser, os
APP='nplay'; DATA=Path(os.getenv('XDG_DATA_HOME',Path.home()/'.local/share'))/APP; STATE=Path(os.getenv('XDG_STATE_HOME',Path.home()/'.local/state'))/APP; CONFIG=Path(os.getenv('XDG_CONFIG_HOME',Path.home()/'.config'))/APP
for p in (DATA,STATE,CONFIG):p.mkdir(parents=True,exist_ok=True)
# Logs and session state can reveal listening history: keep the state directory private.
try:os.chmod(STATE,0o700)
except OSError:pass
def private_runtime_dir():
 """Return NPLAY's private (0700, owner-only) runtime directory for sockets and temp configs."""
 import stat,tempfile
 base=os.getenv('XDG_RUNTIME_DIR')
 d=(Path(base)/APP) if base else (STATE/'run')
 try:
  d.mkdir(parents=True,exist_ok=True);os.chmod(d,0o700);st=os.lstat(d)
  if stat.S_ISDIR(st.st_mode) and st.st_uid==os.getuid() and len(str(d))<=80:return d
 except OSError:pass
 # Unusable or too long for AF_UNIX: a fresh private directory, never a shared /tmp file.
 global _FALLBACK_RUNTIME
 if _FALLBACK_RUNTIME is None or not os.path.isdir(_FALLBACK_RUNTIME):_FALLBACK_RUNTIME=tempfile.mkdtemp(prefix='nplay-')
 return Path(_FALLBACK_RUNTIME)
_FALLBACK_RUNTIME=None
DEFAULT_KEYS={'play_pause':'SPACE','next':'n','previous':'p','seek_back':'LEFT','seek_forward':'RIGHT','seek_back_long':'H','seek_forward_long':'L','volume_up':'+','volume_down':'-','mute':'m','search':'/','browse':'b','queue':'q','favorite':'f','visualizer':'v','help':'?','back':'ESC','quit':'Q','down':'j','up':'k','activate':'ENTER','command':':','refresh':'u','home':'g'}
class Config:
 def __init__(self):
  self.path=CONFIG/'config.ini';self.secret_path=CONFIG/'secrets.ini';self.c=configparser.ConfigParser();self.c.read(self.path);self.s=configparser.ConfigParser();self.s.read(self.secret_path);changed=False
  if 'general' not in self.c:self.c['general']={};changed=True
  defaults={'statistics_enabled':'true','statistics_sync_enabled':'false','statistics_sync_url':'https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3','music_dirs':str(Path.home()/'Music'),'youtube_enabled':'true','navidrome_enabled':'true','navidrome_url':'','navidrome_user':'','navidrome_bitrate':'0','visualizer':'true','visualizer_height':'10','kitty_artwork':'true','now_playing_view':'normal','theme':'niru-noir','sr_enabled':'true','custom_radio_enabled':'true','auto_library_refresh':'true','normalization':'true','normalization_mode':'auto','replaygain_preamp':'0','replaygain_clip':'true','gapless':'false','shuffle_mode':'off','repeat_mode':'off','youtube_sort':'relevance','spotify_enabled':'false','spotify_client_id':'','spotify_device_id':'','spotify_device_name':'','spotify_playback':'local','spotify_local_name':'NPLAY','spotify_local_backend':'pulseaudio','spotify_local_bitrate':'320','spotify_local_volume':'70','music_excludes':'Reaper-projects','library_scan_batch':'250','track_notifications':'true','notification_artwork':'true','mpris_enabled':'true','preferred_source':'local','visualizer_style':'classic','visualizer_gradient':'theme'}
  for k,v in defaults.items():
   if k not in self.c['general']:self.c['general'][k]=v;changed=True
  if 'keys' not in self.c:self.c['keys']=DEFAULT_KEYS;changed=True
  if changed:self.save()
 def get(self,k,d=''):return self.c['general'].get(k,d)
 def getbool(self,k,d=False):return self.c['general'].getboolean(k,fallback=d)
 def set(self,k,v):self.c['general'][k]=str(v);self.save()
 def keys(self):return {**DEFAULT_KEYS,**dict(self.c['keys'])}
 def nav_password(self):return os.getenv('NPLAY_NAVIDROME_PASSWORD') or self.s.get('navidrome','password',fallback='')
 def save_nav(self,url,user,password):
  self.c['general']['navidrome_url']=url.rstrip('/');self.c['general']['navidrome_user']=user;self.save()
  if 'navidrome' not in self.s:self.s['navidrome']={}
  self.s['navidrome']['password']=password
  fd=os.open(self.secret_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
  with os.fdopen(fd,'w') as f:self.s.write(f)
  os.chmod(self.secret_path,0o600)
 def clear_nav(self):
  self.c['general']['navidrome_url']='';self.c['general']['navidrome_user']='';self.save();self.s.remove_section('navidrome')
  if self.secret_path.exists():
   with self.secret_path.open('w') as f:self.s.write(f)
   os.chmod(self.secret_path,0o600)

 def statistics_token(self):
  return self.s.get('statistics','token',fallback='')
 def save_statistics_token(self,token):
  if not token or '\n' in token or '\r' in token or len(token)>1024:raise ValueError('Invalid token')
  if 'statistics' not in self.s:self.s['statistics']={}
  self.s['statistics']['token']=token
  fd=os.open(self.secret_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
  with os.fdopen(fd,'w') as f:self.s.write(f)
  os.chmod(self.secret_path,0o600)
 def spotify_tokens(self):
  return dict(self.s['spotify']) if 'spotify' in self.s else {}
 def save_spotify_tokens(self,tokens):
  if 'spotify' not in self.s:self.s['spotify']={}
  for k in ('access_token','refresh_token','expires_at','scope','token_type'):
   if k in tokens:self.s['spotify'][k]=str(tokens[k])
  fd=os.open(self.secret_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
  with os.fdopen(fd,'w') as f:self.s.write(f)
  os.chmod(self.secret_path,0o600)
 def clear_spotify_tokens(self):
  self.s.remove_section('spotify')
  fd=os.open(self.secret_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
  with os.fdopen(fd,'w') as f:self.s.write(f)
  os.chmod(self.secret_path,0o600)
 def save(self):
  with self.path.open('w') as f:self.c.write(f)
  try:os.chmod(self.path,0o600)
  except OSError:pass
