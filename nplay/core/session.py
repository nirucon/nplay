"""Atomic session persistence kept separate from playback orchestration."""
import json
class SessionStore:
 def __init__(self,path):self.path=path
 def load(self):
  try:return json.loads(self.path.read_text()) if self.path.exists() else {}
  except (OSError,ValueError,TypeError):return {}
 def save(self,data):
  self.path.parent.mkdir(parents=True,exist_ok=True);tmp=self.path.with_suffix(self.path.suffix+'.tmp');tmp.write_text(json.dumps(data,separators=(',',':')));tmp.replace(self.path)
