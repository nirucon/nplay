"""Small synchronous event bus for decoupling playback from integrations."""
from collections import defaultdict
from threading import RLock

class EventBus:
 def __init__(self):self._handlers=defaultdict(list);self._lock=RLock()
 def subscribe(self,event,handler):
  with self._lock:
   if handler not in self._handlers[event]:self._handlers[event].append(handler)
  return lambda:self.unsubscribe(event,handler)
 def unsubscribe(self,event,handler):
  with self._lock:
   if handler in self._handlers.get(event,[]):self._handlers[event].remove(handler)
 def emit(self,event,**payload):
  with self._lock:handlers=tuple(self._handlers.get(event,()))+tuple(self._handlers.get('*',()))
  for handler in handlers:
   try:handler(event,**payload)
   except Exception:pass
