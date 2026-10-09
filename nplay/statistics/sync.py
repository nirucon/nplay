"""Opt-in HTTPS adapter for the verified n.rudolfsson.net 0.6.4 API v3.

Transport is isolated from playback. No token is logged or shown in the TUI.
"""
import json
import random
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime,timezone
from urllib.parse import urlsplit
from .adapter import pending_envelope

DEFAULT_URL='https://n.rudolfsson.net/?r=api&endpoint=statistics/music/v3'

class SyncError(Exception):pass

class StatisticsSync:
 def __init__(self,store,cfg,transport=None,clock=None,sleep=None):
  self.store=store;self.cfg=cfg;self.transport=transport or self._send
  self.clock=clock or time.time;self.sleep=sleep or time.sleep
  self.lock=threading.Lock();self.status='DISABLED';self.last_error='';self.last_success=float(store.get_meta('last_success','0'));self.next_retry=0.;self.failures=0
  self.thread=None
 def enabled(self):return self.cfg.getbool('statistics_sync_enabled',False)
 def token(self):return self.cfg.statistics_token()
 def url(self):return self.cfg.get('statistics_sync_url',DEFAULT_URL) or DEFAULT_URL
 def _send(self,url,token,payload):
  # urllib verifies CA and hostname by default. Never disable TLS validation.
  raw=json.dumps(payload,ensure_ascii=False,separators=(',',':')).encode('utf-8')
  if len(raw)>262144:raise SyncError('Batch exceeds server payload limit')
  req=urllib.request.Request(url,data=raw,method='POST',headers={
   'Authorization':'Bearer '+token,'Content-Type':'application/json','Accept':'application/json','User-Agent':'NPLAY statistics/1.5.5'})
  class NoRedirect(urllib.request.HTTPRedirectHandler):
   def redirect_request(self,*args,**kwargs):raise SyncError('Unexpected redirect; token not forwarded')
  opener=urllib.request.build_opener(NoRedirect())
  with opener.open(req,timeout=12) as res:
   if res.status!=200:raise SyncError('Unexpected server response')
   return json.loads(res.read(262145))
 def once(self):
  if not self.enabled():self.status='DISABLED';return 0
  token=self.token()
  if not token:
   self._failed('No statistics API token configured',retry=False)
   raise SyncError('No statistics API token configured')
  url=self.url();parsed=urlsplit(url)
  if parsed.scheme!='https' or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
   self._failed('Statistics server must use HTTPS without embedded credentials',retry=False)
   raise SyncError('Statistics server must use HTTPS without embedded credentials')
  payload=pending_envelope(self.store,50)
  if not payload['events']:self.status='IDLE';return 0
  if any(not 0<float(x['listened_seconds'])<=5.5 for x in payload['events']):
   self._failed('Legacy interval exceeds server maximum; preserved locally',retry=False)
   raise SyncError(self.last_error)
  ids={x['event_id'] for x in payload['events']}
  self.status='SYNCING'
  try:
   response=self.transport(url,token,payload)
   if not isinstance(response,dict) or response.get('ok') is not True or response.get('format_version')!=1:
    raise SyncError('Server response was not a valid acknowledgment')
   ack=response.get('acknowledged_event_ids')
   if not isinstance(ack,list) or any(not isinstance(x,str) or x not in ids for x in ack) or len(ack)!=len(set(ack)):
    raise SyncError('Server acknowledgment contains invalid event IDs')
   if not ack:raise SyncError('Server acknowledged no events')
   self.store.acknowledge(ack)
   self.last_success=self.clock();self.store.set_meta('last_success',self.last_success);self.failures=0;self.next_retry=0.;self.last_error='';self.status='IDLE'
   return len(ack)
  except urllib.error.HTTPError as e:
   msg={401:'Authorization required',403:'Invalid token, scope or installation binding',413:'Batch too large',422:'Server rejected an event',429:'Server rate limit',503:'Server storage unavailable'}.get(e.code,'HTTP '+str(e.code))
   self._failed(msg,retry=e.code in (429,500,502,503,504))
   raise SyncError(msg) from None
  except (urllib.error.URLError,TimeoutError,OSError,ValueError,SyncError) as e:
   msg='Network unavailable or timeout' if isinstance(e,(urllib.error.URLError,TimeoutError,OSError)) else str(e)
   self._failed(msg,retry=True)
   raise SyncError(msg) from None
 def _failed(self,msg,retry):
  self.status='ERROR';self.last_error=msg[:160]
  self.failures+=1
  self.next_retry=self.clock()+min(3600,10*(2**min(self.failures-1,8)))+random.random()*3 if retry else float('inf')
 def run_async(self,manual=False,callback=None):
  if not self.enabled():self.status='DISABLED';return False
  if not manual and self.clock()<self.next_retry:return False
  if not self.lock.acquire(False):return False
  def worker():
   try:self.once()
   except SyncError:pass
   finally:
    self.lock.release()
    if callback:callback()
  self.thread=threading.Thread(target=worker,daemon=True,name='nplay-statistics-sync');self.thread.start();return True
