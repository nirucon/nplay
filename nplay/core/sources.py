"""Provider capability registry. UI/services query capabilities instead of source-name conditionals."""
from dataclasses import dataclass

@dataclass(frozen=True)
class SourceSpec:
 id:str;label:str;capabilities:frozenset

class SourceRegistry:
 def __init__(self):self._sources={}
 def register(self,id,label,*capabilities):self._sources[id]=SourceSpec(id,label,frozenset(capabilities));return self._sources[id]
 def get(self,id):return self._sources.get(id)
 def supports(self,id,capability):
  spec=self.get(id);return bool(spec and capability in spec.capabilities)
 def all(self):return tuple(self._sources.values())

def default_registry():
 r=SourceRegistry()
 r.register('local','Local Music','browse','search','random','radio-seed','seek','artwork','playlists','ratings')
 r.register('navidrome','Navidrome','browse','search','random','radio-seed','seek','artwork','playlists')
 r.register('spotify','Spotify','browse','search','radio-seed','seek','artwork','playlists','remote-transport')
 r.register('sr','Sveriges Radio','browse','search','live','podcast','artwork')
 r.register('youtube','YouTube','search','seek','artwork')
 r.register('radio','Custom Radio','live')
 return r
