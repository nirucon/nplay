import logging
from logging.handlers import RotatingFileHandler
from .config import STATE
LOG_PATH=STATE/'nplay.log'
_logger=None
def logger():
 global _logger
 if _logger:return _logger
 x=logging.getLogger('nplay');x.setLevel(logging.INFO);x.propagate=False
 if not x.handlers:
  h=RotatingFileHandler(LOG_PATH,maxBytes=2*1024*1024,backupCount=3,encoding='utf-8');h.setFormatter(logging.Formatter('%(asctime)s %(levelname)s %(name)s: %(message)s'));x.addHandler(h)
 _logger=x;return x
def recent_error_count():
 try:
  return sum(1 for line in LOG_PATH.read_text(errors='replace').splitlines() if ' ERROR ' in line or ' WARNING ' in line)
 except OSError:return 0
