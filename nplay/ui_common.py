ACTIONS=[
 ('NAVIGATION',''),('Esc','Back one view'),('Ctrl+N','Now Playing'),('b','Browse'),('/','Filter current list'),('Ctrl+P','Universal Quick Find'),('q','Queue'),('i','Information / context'),('Tab','Now Playing → Queue → Context'),
 ('PLAYBACK',''),('SPACE','Play / pause'),('z','Stop and clear Now Playing'),('n / p','Next / previous track'),('s','Cycle shuffle: Off / Shuffle / Smart'),('S','Sort dated results: Relevance / Newest / Oldest'),('x','Cycle repeat: Off / Track / Context'),('Enter','Open / play'),('← / →','Seek ±5 s'),('H / L','Seek ±30 s'),('+ / -','Volume ±5'),('m','Mute'),
 ('QUEUE & LIBRARY',''),('a','Add selected to queue'),('A','Add selected to playlist'),('r','Random in current source/list'),('e','Play selected next'),('d','Remove selected from queue/playlist'),('J / K','Move queue/playlist item down / up'),('c','Clear queue (while viewing Queue)'),('W','Save queue as playlist'),('R','Rename current playlist'),('D','Delete current playlist (confirmed)'),('f','Favorite current'),('u','Rescan local library'),
 ('VIEW',''),('.','Context actions for selected/current item'),('v','Cycle Normal / Artwork / Visualizer'),('V','Visualizer on / off'),('C','Cycle CAVA style'),('j/k · ↑/↓','Move selection'),('g / G','First / last'),
 ('OTHER',''),(':','Command palette / commands'),('?','Help'),('Q','Quit')]
def clip(s,n):
 s=str(s or '');return s if len(s)<=n else s[:max(0,n-1)]+'…'
