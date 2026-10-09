"""Listening statistics and durable, backwards-compatible sync outbox."""
import os
from pathlib import Path
import sqlite3
from contextlib import contextmanager
import threading
import time
import uuid
from ..config import DATA

class StatisticsStore:
 def __init__(self,path=None):
  self.path=Path(path) if path else DATA/'statistics.sqlite3'
  self.path.parent.mkdir(parents=True,exist_ok=True)
  self.lock=threading.RLock()
  with self._connect() as db:
   db.executescript("""
   CREATE TABLE IF NOT EXISTS identity(key TEXT PRIMARY KEY,value TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS listening(
    event_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, start_utc REAL NOT NULL,
    seconds REAL NOT NULL CHECK(seconds>0 AND seconds<=10),
    source TEXT NOT NULL, track_id TEXT NOT NULL, title TEXT NOT NULL,
    artist TEXT NOT NULL, album TEXT NOT NULL);
   CREATE INDEX IF NOT EXISTS listening_period ON listening(start_utc);
   CREATE INDEX IF NOT EXISTS listening_artist ON listening(artist);
   CREATE TABLE IF NOT EXISTS sync_outbox(
    event_id TEXT PRIMARY KEY REFERENCES listening(event_id),
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','acked')),
    created_utc REAL NOT NULL);
   """)
   db.execute("INSERT OR IGNORE INTO identity(key,value) VALUES('installation_id',?)",(str(uuid.uuid4()),))
  try:os.chmod(self.path,0o600)
  except OSError:pass
 @contextmanager
 def _connect(self):
  db=sqlite3.connect(self.path,timeout=0.05)
  try:
   db.execute('PRAGMA busy_timeout=50')
   db.execute('PRAGMA foreign_keys=ON')
   yield db
   db.commit()
  except Exception:
   db.rollback()
   raise
  finally:db.close()
 def installation_id(self):
  with self.lock,self._connect() as db:return db.execute("SELECT value FROM identity WHERE key='installation_id'").fetchone()[0]
 def get_meta(self,key,default=''):
  with self.lock,self._connect() as db:
   row=db.execute('SELECT value FROM identity WHERE key=?',(key,)).fetchone()
   return row[0] if row else default
 def set_meta(self,key,value):
  with self.lock,self._connect() as db:
   db.execute('INSERT INTO identity(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value',(key,str(value)))
 def add(self,session_id,start_utc,seconds,track,event_id=None):
  if not 0<seconds<=10:raise ValueError('Invalid listening interval')
  eid=event_id or str(uuid.uuid4())
  source=str(getattr(track,'source','') or 'unknown')[:40]
  tid=str(getattr(track,'id','') or '')[:512]
  title=str(getattr(track,'title','') or '')[:512]
  artist=str(getattr(track,'artist','') or '')[:512]
  album=str(getattr(track,'album','') or '')[:512]
  with self.lock,self._connect() as db:
   cur=db.execute('INSERT OR IGNORE INTO listening VALUES(?,?,?,?,?,?,?,?,?)',
    (eid,session_id,start_utc,seconds,source,tid,title,artist,album))
   if cur.rowcount:
    db.execute('INSERT OR IGNORE INTO sync_outbox(event_id,created_utc) VALUES(?,?)',(eid,time.time()))
  return eid
 def summary(self,period='week',now=None):
  now=time.time() if now is None else now
  from datetime import datetime,timedelta
  local=datetime.fromtimestamp(now).astimezone()
  if period=='day':start=local.replace(hour=0,minute=0,second=0,microsecond=0)
  elif period=='week':start=(local-timedelta(days=local.weekday())).replace(hour=0,minute=0,second=0,microsecond=0)
  elif period=='month':start=local.replace(day=1,hour=0,minute=0,second=0,microsecond=0)
  elif period=='year':start=local.replace(month=1,day=1,hour=0,minute=0,second=0,microsecond=0)
  elif period=='all':start=None
  else:raise ValueError('Unknown period')
  cutoff=start.timestamp() if start else 0
  with self.lock,self._connect() as db:
   where='WHERE start_utc>=?'
   total=db.execute('SELECT COALESCE(SUM(seconds),0),COUNT(DISTINCT session_id),COUNT(DISTINCT date(start_utc,\'unixepoch\',\'localtime\')) FROM listening '+where,(cutoff,)).fetchone()
   def top(field):
    return db.execute(f'SELECT {field},SUM(seconds) AS secs,COUNT(DISTINCT session_id) AS plays FROM listening '+where+f' AND {field} != \'\' GROUP BY {field} ORDER BY secs DESC,{field} COLLATE NOCASE LIMIT 10',(cutoff,)).fetchall()
   albums=db.execute('SELECT artist,album,SUM(seconds),COUNT(DISTINCT session_id) FROM listening '+where+" AND album != '' GROUP BY artist,album ORDER BY SUM(seconds) DESC LIMIT 10",(cutoff,)).fetchall()
   tracks=db.execute('SELECT artist,title,SUM(seconds),COUNT(DISTINCT session_id) FROM listening '+where+" AND title != '' GROUP BY artist,title ORDER BY SUM(seconds) DESC LIMIT 10",(cutoff,)).fetchall()
   return {'seconds':float(total[0]),'sessions':total[1],'days':total[2],
    'artists':top('artist'),'albums':albums,'tracks':tracks,'sources':top('source'),
    'pending':db.execute("SELECT COUNT(*) FROM sync_outbox WHERE status='pending'").fetchone()[0]}
 def pending_events(self,limit=100):
  """Read-only transport-neutral export. No HTTP transport is implemented."""
  with self.lock,self._connect() as db:
   db.row_factory=sqlite3.Row
   rows=db.execute("""SELECT l.* FROM listening l JOIN sync_outbox q USING(event_id)
    WHERE q.status='pending' ORDER BY l.start_utc,l.event_id LIMIT ?""",(max(1,min(500,int(limit))),)).fetchall()
   return [dict(r) for r in rows]
 def acknowledge(self,ids):
  """Only call after a verified server-side idempotent acknowledgment."""
  with self.lock,self._connect() as db:
   db.executemany("UPDATE sync_outbox SET status='acked' WHERE event_id=?",[(x,) for x in ids])
