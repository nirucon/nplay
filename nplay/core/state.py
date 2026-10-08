"""Canonical, serialisable playback snapshot exposed to integrations."""
from dataclasses import dataclass,field

@dataclass
class PlaybackState:
 status:str='idle'
 transport:str=''
 source:str=''
 track_id:str=''
 generation:int=0
 position:float=0.0
 duration:float=0.0
 seekable:bool=False
 kind:str=''
 metadata:dict=field(default_factory=dict)
 def update_media(self,track,transport='',generation=0,status='playing'):
  self.status=status;self.transport=transport;self.source=getattr(track,'source','');self.track_id=getattr(track,'id','');self.generation=generation;self.duration=float(getattr(track,'duration',0) or 0);self.seekable=bool(getattr(track,'seekable',False));self.kind=getattr(track,'kind','');return self
 def clear(self):
  self.__dict__.update(PlaybackState().__dict__);return self
