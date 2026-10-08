"""Optional MPRIS2 bridge. Event-driven when dbus-python/GLib are available."""
import threading,time
class MPRIS:
 def __init__(self,app):self.app=app;self.available=False;self._player=None;self._loop=None;self._thread=None
 def start(self):
  try:
   import dbus,dbus.service
   from dbus.mainloop.glib import DBusGMainLoop
   from gi.repository import GLib
  except Exception:return False
  app=self.app
  class Service(dbus.service.Object):
   def __init__(self):
    DBusGMainLoop(set_as_default=True);self.bus=dbus.SessionBus();self.name=dbus.service.BusName('org.mpris.MediaPlayer2.nplay',bus=self.bus,do_not_queue=True);super().__init__(self.name,'/org/mpris/MediaPlayer2')
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def PlayPause(self):app.playback.play_pause(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def Play(self):
    if not app.playback.active():app.playback.play_pause(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def Pause(self):
    if app.playback.active():app.playback.play_pause(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def Next(self):app.playback.next(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def Previous(self):app.playback.previous(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player')
   def Stop(self):app.playback.stop(app.ui)
   @dbus.service.method('org.mpris.MediaPlayer2.Player',in_signature='x')
   def Seek(self,offset):app.playback.seek_relative(float(offset)/1000000)
   @dbus.service.method('org.mpris.MediaPlayer2.Player',in_signature='ox')
   def SetPosition(self,track_id,position):
    if app.current and app.current.seekable:
     target=max(0,float(position)/1000000);delta=target-app.playback.position();app.playback.seek_relative(delta)
   @dbus.service.method('org.mpris.MediaPlayer2')
   def Raise(self):pass
   @dbus.service.method('org.mpris.MediaPlayer2')
   def Quit(self):
    if app.ui:app.ui.running=False
   @dbus.service.signal('org.freedesktop.DBus.Properties',signature='sa{sv}as')
   def PropertiesChanged(self,interface,changed,invalidated):pass
   @dbus.service.method('org.freedesktop.DBus.Properties',in_signature='ss',out_signature='v')
   def Get(self,iface,prop):return self._props(iface).get(prop)
   @dbus.service.method('org.freedesktop.DBus.Properties',in_signature='s',out_signature='a{sv}')
   def GetAll(self,iface):return self._props(iface)
   @dbus.service.method('org.freedesktop.DBus.Properties',in_signature='ssv')
   def Set(self,iface,prop,value):
    if iface=='org.mpris.MediaPlayer2.Player' and prop=='Volume':app.playback.volume_relative((float(value)*100)-app.volume)
   def _props(self,iface):
    import dbus
    if iface=='org.mpris.MediaPlayer2':return {'CanQuit':dbus.Boolean(True),'CanRaise':dbus.Boolean(False),'HasTrackList':dbus.Boolean(False),'Identity':'NPLAY','DesktopEntry':'nplay','SupportedUriSchemes':dbus.Array(['file','http','https'],signature='s'),'SupportedMimeTypes':dbus.Array([],signature='s')}
    t=app.current;meta={}
    if t:
     display_title=t.title or '';display_artist=t.artist or '';display_album=t.album or ''
     if t.kind=='radio':
      info=app.radio_info.get(t.id,{}) or {};display_title=(info.get('program') or info.get('song') or t.title or 'Live radio');display_artist=t.title or t.artist or '';display_album='Sveriges Radio · LIVE' if t.source=='sr' else 'Live radio'
     meta={'mpris:trackid':dbus.ObjectPath('/org/nplay/track/'+(''.join(c if c.isalnum() else '_' for c in (str(t.source)+'_'+str(t.id))) or 'current')),'xesam:title':display_title,'xesam:artist':dbus.Array([display_artist] if display_artist else [],signature='s'),'xesam:album':display_album};
     if t.kind!='radio':meta['mpris:length']=dbus.Int64(int((app.playback.duration() or t.duration or 0)*1000000))
     art=''
     try:
      if app.ui and t.cover:
       p=app.ui.art.cached(t.cover);art=('file://'+str(p)) if p else t.cover
      else:art=t.cover
     except Exception:art=t.cover
     if art:meta['mpris:artUrl']=art if str(art).startswith(('file://','http://','https://')) else 'file://'+str(art)
    return {'PlaybackStatus':'Playing' if app.playback.active() else ('Paused' if t and app.playback_state.get('state')!='stopped' else 'Stopped'),'LoopStatus':{'track':'Track','context':'Playlist'}.get(app.repeat_mode,'None'),'Rate':dbus.Double(1.0),'Shuffle':dbus.Boolean(app.shuffle_mode!='off'),'Metadata':dbus.Dictionary(meta,signature='sv'),'Volume':dbus.Double(max(0,min(1.5,app.volume/100))),'Position':dbus.Int64(int(app.playback.position()*1000000)),'MinimumRate':dbus.Double(1.0),'MaximumRate':dbus.Double(1.0),'CanGoNext':dbus.Boolean(bool(app.queue or (t and t.kind!='radio' and app.play_context and (app.context_index+1<len(app.play_context) or app.repeat_mode=='context')))),'CanGoPrevious':dbus.Boolean(bool(app.current and (app.playback.position()>3 or app.back_stack or app.context_index>0))),'CanPlay':dbus.Boolean(True),'CanPause':dbus.Boolean(True),'CanSeek':dbus.Boolean(bool(t and t.seekable)),'CanControl':dbus.Boolean(True)}
   def emit_changed(self,names):
    props=self._props('org.mpris.MediaPlayer2.Player');changed={k:props[k] for k in names if k in props};self.PropertiesChanged('org.mpris.MediaPlayer2.Player',changed,[])
  def run():
   try:
    from gi.repository import GLib
    self._player=Service();self.available=True;self._loop=GLib.MainLoop();self._loop.run()
   except Exception:self.available=False
  self._thread=threading.Thread(target=run,daemon=True);self._thread.start();time.sleep(.05);return self.available
 def changed(self,*names):
  if not self.available or not self._player:return
  try:
   from gi.repository import GLib
   GLib.idle_add(lambda:(self._player.emit_changed(names or ('PlaybackStatus','Metadata','Volume','LoopStatus','Shuffle')),False)[1])
  except Exception:pass
 def stop(self):
  try:self._loop and self._loop.quit()
  except Exception:pass
