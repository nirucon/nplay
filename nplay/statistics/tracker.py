"""Actual elapsed listening, buffered to server-compatible <=5s events."""
import time
import uuid
from datetime import datetime, timedelta

class ListeningTracker:
 def __init__(self,store,enabled=True,clock=None,wall=None):
  self.store=store;self.enabled=bool(enabled)
  self.clock=clock or time.monotonic;self.wall=wall or time.time
  self.last=None;self.session=None;self.key=None;self.track=None
  self.pending=0.;self.started=0.
 def _write(self,seconds):
  if seconds<=0 or self.track is None:return
  # Split at local midnight: server buckets by started_at in Europe/Stockholm.
  while seconds>0.00001:
   start=self.started
   local=datetime.fromtimestamp(start).astimezone()
   midnight=(local+timedelta(days=1)).replace(hour=0,minute=0,second=0,microsecond=0).timestamp()
   amount=min(seconds,5.0,max(.001,midnight-start))
   try:self.store.add(self.session,start,amount,self.track)
   except Exception:pass # best effort; playback must remain unaffected
   self.started+=amount;seconds-=amount
 def _drain(self,force=False):
  while self.pending>=5.0-1e-6:
   self._write(5.0);self.pending-=5.0
  if force and self.pending>0.00001:self._write(self.pending);self.pending=0.
 def flush(self,playing=False):
  if self.last is not None and playing and self.track is not None:
   now=self.clock();elapsed=now-self.last
   if 0<elapsed<=5:self.pending+=elapsed
   self.last=now
  self._drain(True)
  self.last=None;self.session=None;self.key=None;self.track=None;self.pending=0.
 def reset(self):self.flush(False)
 def tick(self,track,playing):
  now=self.clock()
  if not self.enabled or not playing or track is None:
   self.flush(False);return
  key=(str(getattr(track,'source','')),str(getattr(track,'id','')))
  if self.key!=key:
   self.flush(False)
   self.key=key;self.track=track;self.session=str(uuid.uuid4());self.last=now;return
  elapsed=now-self.last;self.last=now
  if elapsed<=0 or elapsed>5:
   self._drain(True) # no fictitious time during suspend
   return
  if not self.pending:self.started=self.wall()-elapsed
  self.pending+=elapsed
  self._drain(False)
