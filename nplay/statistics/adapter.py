"""Provider-neutral listening event envelope; intentionally no network transport.

This is an internal, documented interchange format, NOT a claim that any
external API (including n.rudolfsson.net Statistics API v2) accepts it.
"""
from datetime import datetime, timezone

FORMAT_VERSION = 1


def pending_envelope(store, limit=100):
 """Return a JSON-serializable batch for a future, explicit sync adapter.

 Calling this never sends data, changes queue state, or acknowledges events.
 """
 events=[]
 for row in store.pending_events(limit):
  events.append({
   'event_id':row['event_id'],
   'session_id':row['session_id'],
   'started_at':datetime.fromtimestamp(row['start_utc'],timezone.utc).isoformat().replace('+00:00','Z'),
   'listened_seconds':row['seconds'],
   'source':row['source'],
   'source_track_id':row['track_id'],
   'title':row['title'],
   'artist':row['artist'],
   'album':row['album'],
  })
 return {'format':'nplay.listening-events','format_version':FORMAT_VERSION,
         'installation_id':store.installation_id(),'events':events}
