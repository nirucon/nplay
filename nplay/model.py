from dataclasses import dataclass, field, asdict
from typing import Any
@dataclass
class Track:
    source:str='local'; id:str=''; title:str=''; artist:str=''; album:str=''; duration:float=0.0
    url:str=''; cover:str=''; path:str=''; kind:str='track'; seekable:bool=True; meta:dict[str,Any]=field(default_factory=dict)
    def dict(self): return asdict(self)
    @classmethod
    def from_dict(cls,d):
        keys=cls.__dataclass_fields__; return cls(**{k:v for k,v in d.items() if k in keys})
