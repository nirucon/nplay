import curses, os, re, tomllib, logging
from pathlib import Path

# Built-in light themes use explicit xterm-256 indices as well as RGB metadata.
# The explicit indices are intentional: rounding arbitrary RGB values to the 6x6x6
# cube made the old C. Larsson paper colour land on xterm colour 224 (#ffd7d7),
# which is why it appeared distinctly pink in Kitty.
THEMES={
 'niru-noir':{'label':'Niru Noir','native':True},
 'satie':{
  'label':'Satie',
  # Aged score/manuscript paper: darker warm-neutral field, ink-black graphite, sepia accent.
  # Muted text is deliberately strong enough to remain readable without terminal A_DIM.
  'fg':(28,28,28),'muted':(68,68,68),'accent':(135,95,0),
  'select_fg':(238,238,215),'select_bg':(135,95,0),'bg':(208,208,208),
  'fg_idx':234,'muted_idx':238,'accent_idx':94,'select_fg_idx':230,'select_bg_idx':94,'bg_idx':252,
 },
 'c-larsson':{
  'label':'C. Larsson',
  # Sundborn/home-watercolour inspired: aged linen/ochre paper, forest green, olive and falun red.
  # Darker than 1.1.5 and intentionally earthy rather than lemon-yellow.
  'fg':(0,95,0),'muted':(95,95,0),'accent':(175,0,0),
  'select_fg':(238,238,215),'select_bg':(95,135,95),'bg':(215,215,175),
  'fg_idx':22,'muted_idx':58,'accent_idx':124,'select_fg_idx':230,'select_bg_idx':65,'bg_idx':187,
 },
 'hackerman':{
  'label':'Hackerman',
  # High-contrast phosphor terminal, intentionally sparse rather than neon-heavy.
  'fg':(0,215,0),'muted':(0,135,0),'accent':(95,255,0),
  'select_fg':(255,255,255),'select_bg':(0,95,0),'bg':(8,8,8),
  'fg_idx':40,'muted_idx':28,'accent_idx':82,'select_fg_idx':231,'select_bg_idx':22,'bg_idx':232,
 },
 'othala':{
  'label':'Othala',
  # Nordic stone / iron / winter sky: dark slate, bone text, cold steel accent.
  'fg':(215,215,205),'muted':(135,135,135),'accent':(95,135,175),
  'select_fg':(18,18,18),'select_bg':(175,175,175),'bg':(18,18,18),
  'fg_idx':252,'muted_idx':245,'accent_idx':67,'select_fg_idx':233,'select_bg_idx':250,'bg_idx':233,
 },
 'ingwaz':{
  'label':'Ingwaz',
  # Earth / seed / growth: deep soil, flax text, restrained moss and amber.
  'fg':(215,205,175),'muted':(135,135,95),'accent':(135,175,95),
  'select_fg':(18,18,18),'select_bg':(175,175,95),'bg':(28,28,18),
  'fg_idx':187,'muted_idx':101,'accent_idx':107,'select_fg_idx':233,'select_bg_idx':143,'bg_idx':234,
 },
 'commodore64':{
  'label':'Commodore 64',
  # C64-inspired blue field/light-blue type, tuned for terminal readability.
  'fg':(175,175,255),'muted':(135,135,215),'accent':(215,215,255),
  'select_fg':(38,38,95),'select_bg':(175,175,255),'bg':(38,38,95),
  'fg_idx':147,'muted_idx':110,'accent_idx':189,'select_fg_idx':17,'select_bg_idx':147,'bg_idx':17,
 },
}

def _rgb(hexv):
 h=hexv.lstrip('#');return tuple(int(h[i:i+2],16) for i in (0,2,4)) if len(h)==6 else None

def omarchy_palette():
 """Best-effort palette discovery. Never makes Omarchy a runtime dependency."""
 roots=[Path.home()/'.config/omarchy/current/theme',Path.home()/'.config/omarchy/themes/current',Path.home()/'.config/kitty/current-theme.conf',Path.home()/'.config/kitty/kitty.conf']
 vals={}
 for root in roots:
  files=[root] if root.is_file() else (list(root.rglob('*'))[:80] if root.is_dir() else [])
  for f in files:
   if not f.is_file():continue
   try:text=f.read_text(errors='ignore')
   except Exception:continue
   for key in ('background','foreground','color7','color8','color4'):
    if key in vals:continue
    m=re.search(r'(?mi)^\s*'+re.escape(key)+r'\s*[=: ]\s*(#[0-9a-f]{6})',text)
    if m:vals[key]=_rgb(m.group(1))
 bg=vals.get('background');fg=vals.get('foreground');muted=vals.get('color8');accent=vals.get('color4') or vals.get('color7')
 if fg and bg:return {'label':'Omarchy','fg':fg,'bg':bg,'muted':muted or fg,'accent':accent or fg,'select_fg':bg,'select_bg':accent or fg}
 return None

# External themes are declarative TOML, never executable code.
THEME_DIR=Path(os.environ.get("XDG_CONFIG_HOME",str(Path.home()/".config")))/"nplay"/"themes"
LOG=logging.getLogger(__name__)
FIELDS=("fg","muted","accent","select_fg","select_bg","bg")
HEX=re.compile(r"^#[0-9a-fA-F]{6}$")
ID=re.compile(r"^[a-z][a-z0-9-]{0,47}$")

def load_custom(path):
 if path.suffix.lower()!=".toml" or path.is_symlink() or not path.is_file():
  raise ValueError("Expected a regular .toml file (not a symlink)")
 if path.stat().st_size>16384:raise ValueError("Theme exceeds 16 KiB")
 with path.open("rb") as stream:data=tomllib.load(stream)
 if set(data)-{"theme","colors"}:raise ValueError("Unknown section")
 meta,colors=data.get("theme"),data.get("colors")
 if not isinstance(meta,dict) or not isinstance(colors,dict):raise ValueError("Missing [theme] or [colors]")
 key=meta.get("id")
 if not isinstance(key,str) or not ID.fullmatch(key) or key!=path.stem:raise ValueError("id must match filename")
 if key in THEMES or key=="omarchy":raise ValueError("Reserved theme id")
 if set(meta)-{"id","name","author","version"} or set(colors)-set(FIELDS):raise ValueError("Unknown theme field")
 label=meta.get("name",key)
 if not isinstance(label,str) or not 1<=len(label)<=48 or any(ord(c)<32 for c in label):raise ValueError("Invalid name")
 if not colors:raise ValueError("At least one color required")
 p={k:v for k,v in THEMES["othala"].items() if not k.endswith("_idx") and k!="label"}
 p["label"]=label
 for field,value in colors.items():
  if not isinstance(value,str) or not HEX.fullmatch(value):raise ValueError(field+": expected #RRGGBB")
  p[field]=_rgb(value)
 return key,p

def custom_themes():
 found={}
 if THEME_DIR.is_dir():
  for path in sorted(THEME_DIR.glob("*.toml"))[:128]:
   try:
    key,palette=load_custom(path);found[key]=palette
   except (ValueError,OSError,tomllib.TOMLDecodeError) as exc:LOG.warning("Invalid theme %s: %s",path,exc)
 return found

def validate_theme(path):
 try:
  key,palette=load_custom(Path(path).expanduser())
  return True,f"OK: {palette['label']} ({key})"
 except (ValueError,OSError,tomllib.TOMLDecodeError) as exc:return False,f"Invalid theme: {exc}"

def available():
 builtins=[(key,THEMES[key]["label"]) for key in ("niru-noir","satie","c-larsson","hackerman","commodore64","othala","ingwaz")]
 return builtins+[("omarchy","Omarchy · follow active theme")]+[(k,p["label"]+" · Custom") for k,p in custom_themes().items()]

def apply(name):
 p=omarchy_palette() if name=='omarchy' else THEMES.get(name) or custom_themes().get(name) or THEMES['niru-noir']
 if name=='omarchy' and not p:p=THEMES['niru-noir'].copy();p['label']='Omarchy · fallback to Niru Noir'
 try:
  curses.start_color();curses.use_default_colors()
  def color(idx,rgb,exact=None):
   if exact is not None and curses.COLORS>=256:return exact
   if rgb is None:return -1
   if curses.COLORS>=256:
    r,g,b=rgb;return 16+36*round(r/255*5)+6*round(g/255*5)+round(b/255*5)
   # Keep non-256-colour terminals usable. Light themes become black-on-white;
   # dark themes become white/green-on-black instead of collapsing to white-on-white.
   avg=sum(rgb)/3
   return curses.COLOR_BLACK if avg<128 else curses.COLOR_WHITE
  # Niru Noir remains deliberately terminal-native and unchanged.
  if p.get('native'):
   curses.init_pair(1,-1,-1)
   muted=curses.COLOR_BLACK + 8 if getattr(curses,'COLORS',0)>=16 else -1
   curses.init_pair(2,muted,-1)
   curses.init_pair(3,-1,-1)
   curses.init_pair(4,-1,-1)
   return p
  if curses.COLORS<256:
   # Portable readable fallback when only the basic ANSI palette is available.
   light_bg=(sum(p.get('bg',(0,0,0)))/3)>=128
   bg=curses.COLOR_WHITE if light_bg else curses.COLOR_BLACK
   fg=curses.COLOR_BLACK if light_bg else curses.COLOR_WHITE
   mut=fg
   acc=(curses.COLOR_RED if light_bg else curses.COLOR_GREEN)
   sfg=bg;sbg=fg
  else:
   bg=color(20,p.get('bg'),p.get('bg_idx')) if p.get('bg') else -1
   fg=color(21,p.get('fg'),p.get('fg_idx'));mut=color(22,p.get('muted'),p.get('muted_idx'));acc=color(23,p.get('accent'),p.get('accent_idx'));sfg=color(24,p.get('select_fg'),p.get('select_fg_idx'));sbg=color(25,p.get('select_bg'),p.get('select_bg_idx'))
  curses.init_pair(1,fg,bg);curses.init_pair(2,mut,bg);curses.init_pair(3,acc,bg);curses.init_pair(4,sfg,sbg)
  return p
 except Exception:return p
