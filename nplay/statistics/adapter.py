"""Transport-neutral listening envelope, verified against website 0.6.4 API v3."""
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
   'source_track_id':row['track_id'][:255],
   'title':row['title'][:255],
   'artist':row['artist'][:255],
   'album':row['album'][:255],
  })
 return {'format':'nplay.listening-events','format_version':FORMAT_VERSION,
         'installation_id':store.installation_id(),'events':events}
