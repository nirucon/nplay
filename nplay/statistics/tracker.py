"""Non-blocking best-effort wall-clock listener; never derive time from track duration."""
import time
import uuid

class ListeningTracker:
 def __init__(self,store,enabled=True,clock=None,wall=None):
  self.store=store;self.enabled=bool(enabled)
  self.clock=clock or time.monotonic;self.wall=wall or time.time
  self.last=None;self.session=None;self.key=None
 def reset(self):
  self.last=None;self.session=None;self.key=None
 def tick(self,track,playing):
  now=self.clock()
  if not self.enabled or not playing or track is None:
   self.reset();return
  key=(str(getattr(track,'source','')),str(getattr(track,'id','')))
  if self.key!=key:
   self.key=key;self.session=str(uuid.uuid4());self.last=now;return
  if self.last is None:self.last=now;return
  elapsed=now-self.last
  self.last=now
  # Ignore long gaps caused by suspend, frozen UI or delayed callbacks.
  if elapsed<=0 or elapsed>5:return
  try:self.store.add(self.session,self.wall()-elapsed,elapsed,track)
  except Exception:pass # Statistics must never interrupt playback.
