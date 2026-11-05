import logging,os,stat,tempfile
from logging.handlers import RotatingFileHandler
from .config import STATE
from .redact import redact_text,contains_secret
LOG_PATH=STATE/'nplay.log'
MPV_LOG_PATH=STATE/'mpv.log'
_logger=None

def _private(path):
 try:os.chmod(path,0o600)
 except OSError:pass

class RedactingFilter(logging.Filter):
 """Redacts credentials from every record before any handler sees it."""
 def filter(self,record):
  try:
   msg=record.getMessage()
  except Exception:
   return True
  clean=redact_text(msg)
  if clean!=msg or record.args:
   record.msg=clean;record.args=None
  return True

class RedactingFormatter(logging.Formatter):
 """Second line of defence: tracebacks and stack info are redacted too."""
 def format(self,record):return redact_text(super().format(record))

class PrivateRotatingFileHandler(RotatingFileHandler):
 """A rotating log whose current and rotated files are always mode 0600."""
 def _open(self):
  fd=os.open(self.baseFilename,os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600)
  _private(self.baseFilename)
  return os.fdopen(fd,self.mode,encoding=self.encoding,errors=getattr(self,'errors',None))
 def doRollover(self):
  super().doRollover()
  for i in range(1,self.backupCount+1):
   p=f'{self.baseFilename}.{i}'
   if os.path.exists(p):_private(p)

def log_files():
 """Every NPLAY-owned log that may have been written by an older version."""
 out=[LOG_PATH]+[LOG_PATH.with_name(f'{LOG_PATH.name}.{i}') for i in range(1,10)]+[MPV_LOG_PATH]
 return [p for p in out if p.exists()]

def redact_file(path):
 """Redact credentials in one file in place (temp file + rename). Returns the count."""
 try:
  raw=path.read_bytes()
 except OSError:
  return 0
 text=raw.decode('utf-8','surrogateescape')
 if not contains_secret(text):
  _private(path);return 0
 count=0
 out=[]
 for line in text.splitlines(keepends=True):
  clean=redact_text(line)
  if clean!=line:count+=1
  out.append(clean)
 fd,tmp=tempfile.mkstemp(prefix='.'+path.name+'.',suffix='.tmp',dir=str(path.parent))
 try:
  with os.fdopen(fd,'wb') as f:
   f.write(''.join(out).encode('utf-8','surrogateescape'));f.flush();os.fsync(f.fileno())
  os.chmod(tmp,0o600);os.replace(tmp,path)
 except Exception:
  try:os.unlink(tmp)
  except OSError:pass
  raise
 return count

def migrate_logs():
 """Idempotent 1.8.0 upgrade migration: redact existing credential-bearing logs.

 It runs at every logger start (a scan of the bounded logs takes milliseconds),
 so logs written meanwhile by an older NPLAY process are cleaned as well. Only
 secret query values are replaced; every other byte is preserved. No backup copy
 is kept on purpose: a backup would preserve the credential being removed.
 Returns (files_changed, lines_redacted).
 """
 files=lines=0
 for p in log_files():
  try:n=redact_file(p)
  except Exception:n=0
  if n:files+=1;lines+=n
 _cleanup_tmp_cava()
 return (files,lines)

def _cleanup_tmp_cava():
 """Remove CAVA configs that NPLAY <=1.7 left in /tmp (only this user's own files)."""
 try:
  tmp=tempfile.gettempdir()
  for name in os.listdir(tmp):
   if name.startswith('nplay-cava-') and name.endswith('.conf'):
    p=os.path.join(tmp,name)
    try:
     st=os.lstat(p)
     if st.st_uid==os.getuid() and stat.S_ISREG(st.st_mode):os.unlink(p)
    except OSError:pass
 except OSError:pass

def logger():
 global _logger
 if _logger:return _logger
 x=logging.getLogger('nplay');x.setLevel(logging.INFO);x.propagate=False
 migrated=(0,0)
 if not x.handlers:
  try:migrated=migrate_logs()
  except Exception:migrated=(0,0)
  h=PrivateRotatingFileHandler(LOG_PATH,maxBytes=2*1024*1024,backupCount=3,encoding='utf-8');h.setFormatter(RedactingFormatter('%(asctime)s %(levelname)s %(name)s: %(message)s'));h.addFilter(RedactingFilter());x.addHandler(h)
 if not any(isinstance(f,RedactingFilter) for f in x.filters):x.addFilter(RedactingFilter())
 _logger=x
 if migrated[0]:x.info('log redaction migration: removed credentials from %d lines in %d files',migrated[1],migrated[0])
 return x
def recent_error_count():
 try:
  return sum(1 for line in LOG_PATH.read_text(errors='replace').splitlines() if ' ERROR ' in line or ' WARNING ' in line)
 except OSError:return 0
