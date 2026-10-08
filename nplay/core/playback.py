"""Stable playback façade for UI/MPRIS and future transports."""
class PlaybackController:
 def __init__(self,app):self.app=app
 def active(self):return self.app.playback_active()
 def position(self):return self.app.playback_position()
 def duration(self):return self.app.playback_duration()
 def play_pause(self,ui=None):return self.app.play_pause(ui)
 def stop(self,ui=None,clear=True):return self.app.stop_and_clear(ui) if clear else self.app.stop_playback(ui)
 def seek_relative(self,seconds):return self.app.playback_seek_relative(seconds)
 def volume_relative(self,delta):return self.app.playback_volume(delta)
 def mute(self):return self.app.playback_mute()
 def next(self,ui=None):return self.app.next_track(ui)
 def previous(self,ui=None):return self.app.previous_track(ui)
