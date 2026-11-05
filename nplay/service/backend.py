"""Headless NPLAY service backend.

The backend reuses the established App controller rather than forking playback,
queue, provider or statistics behaviour. It only adds a bounded, serialisable
view of that state plus explicit commands that a local graphical client can use.
"""
import base64
import concurrent.futures
import json
import mimetypes
import os
import shutil
import threading
import time
from collections import OrderedDict
from pathlib import Path

from .. import __version__
from ..app import App
from ..model import Track
from ..providers import Navidrome, YouTube
from ..service.protocol import (
    BackendError,
    MAX_TRACKS,
    PROTOCOL_VERSION,
    ParamsError,
    RpcError,
)
from ..visualizer import Visualizer
from ..logging_utils import LOG_PATH, logger
from ..redact import redact_text

log = logger()

META_ALLOW = {
    "suffix", "bitrate", "codec", "track_no", "disc_no", "year", "genre",
    "album_artist", "sample_rate", "bit_depth", "channels", "song_count",
    "published_date", "published_ts", "explicit", "album_id", "artist_id",
    "playlist_id", "program_id", "homepage", "spotify_url", "webpage",
    "release_date", "item_count", "owner_id", "bookmark_pos", "local_artist",
}

CONTAINER_KINDS = {"album", "artist", "playlist", "program", "folder", "device"}


def playable_track(t):
    return (getattr(t, "kind", "") or "track") not in CONTAINER_KINDS


SAFE_SETTINGS = {
    "navidrome_enabled", "spotify_enabled", "sr_enabled", "youtube_enabled",
    "custom_radio_enabled", "normalization", "normalization_mode", "gapless",
    "track_notifications", "notification_artwork", "mpris_enabled",
    "preferred_source", "shuffle_mode", "repeat_mode",
}
BOOL_SETTINGS = {
    "navidrome_enabled", "spotify_enabled", "sr_enabled", "youtube_enabled",
    "custom_radio_enabled", "normalization", "gapless", "track_notifications",
    "notification_artwork", "mpris_enabled",
}


class ServiceUI:
    """Small UI shim so the existing controller can run without curses.

    App's optional-UI paths remain exactly the same ones the terminal uses.
    Callbacks posted from worker threads run in that worker; they only mutate
    controller state and never draw anything.
    """

    def __init__(self, backend):
        self.backend = backend
        self.running = True
        self.art = backend.app.artwork_cache
        self.visual = bool(shutil.which("cava"))
        self.layout = "normal"
        self.theme = "niru-noir"
        self.items = []
        self.sel = 0
        self.sort_mode = "relevance"

    def post(self, callback):
        try:
            callback()
        except Exception:
            log.exception("service UI callback failed")

    def status(self, message, *args):
        log.info("service status: %s", redact_text(message))

    def error(self, title, message=""):
        log.warning("service error: %s · %s", redact_text(title), redact_text(message))

    def invalidate(self):
        pass

    def now_playing(self):
        pass

    def replace_list(self, *args, **kwargs):
        pass

    def show_list(self, *args, **kwargs):
        pass

    def show_menu(self, *args, **kwargs):
        pass

    def loading(self, *args, **kwargs):
        pass

    def show_empty(self, *args, **kwargs):
        pass

    def prompt(self, *args, **kwargs):
        return ""

    def push(self):
        pass

    def cycle_layout(self):
        pass

    def toggle_visualizer(self):
        pass


class ServiceBackend:
    def __init__(self, notify=None, idle_exit=0):
        self.app = App()
        self._sanitize_queue_state()
        self._sanitize_saved_containers()
        self.ui = ServiceUI(self)
        self.app.ui = self.ui
        self.notify = notify or (lambda method, params: None)
        self.idle_exit = max(0, int(idle_exit or 0))
        self.revision = 0
        self.lock = threading.RLock()
        self._catalog = OrderedDict()
        self._started = time.monotonic()
        self._sample_at = 0.0
        self._sample_value = {"position": 0.0, "paused": False, "active": False, "muted": False}
        self._art_cache = OrderedDict()
        self._art_bytes = 0
        self._vis = None
        self._vis_lock = threading.Lock()
        self._vis_last = 0.0
        self._vis_stop = threading.Event()
        self._vis_monitor = threading.Thread(target=self._vis_monitor_loop, daemon=True, name="nplay-visualizer-idle")
        self._vis_monitor.start()
        self._scanning = False
        self._executor = concurrent.futures.ThreadPoolExecutor(max_workers=6, thread_name_prefix="nplay-rpc")
        self.app.events.subscribe("playback-state-changed", self._on_event)
        self.app.events.subscribe("media-changed", self._on_event)
        self.app.events.subscribe("integration-state-changed", self._on_event)

    def _sanitize_queue_state(self):
        """Drop legacy container rows that older frontends queued as tracks."""
        app = self.app
        queue = [t for t in (app.queue or []) if playable_track(t)]
        later = [t for t in (app.later_queue or []) if playable_track(t)]
        changed = queue != (app.queue or []) or later != (app.later_queue or [])
        if app.play_context:
            context = [t for t in app.play_context if playable_track(t)]
            if context != app.play_context:
                changed = True
                app.play_context = context
                if app.current and playable_track(app.current):
                    app.context_index = next((i for i, t in enumerate(context) if t.source == app.current.source and t.id == app.current.id), -1)
                else:
                    app.context_index = -1
        if changed:
            app.queue = queue
            app.later_queue = later
            try:
                app.save_state()
            except Exception:
                pass
            log.warning("service removed legacy non-playable queue/context rows")

    def _sanitize_saved_containers(self):
        """Remove legacy container rows from playlists/favorites.

        Older NIRUAMP builds could offer album/artist/playlist actions directly.
        Those rows cannot be played, so clean the damage once at service start.
        """
        removed = 0
        try:
            with self.app.db.con() as c:
                for row in c.execute("SELECT id,data FROM playlist_items").fetchall():
                    try:
                        kind = (json.loads(row["data"]) or {}).get("kind", "track")
                    except Exception:
                        continue
                    if kind in CONTAINER_KINDS:
                        c.execute("DELETE FROM playlist_items WHERE id=?", (row["id"],))
                        removed += 1
                for row in c.execute("SELECT item_key,data FROM favorites").fetchall():
                    try:
                        kind = (json.loads(row["data"]) or {}).get("kind", "track")
                    except Exception:
                        continue
                    if kind in CONTAINER_KINDS:
                        c.execute("DELETE FROM favorites WHERE item_key=?", (row["item_key"],))
                        removed += 1
        except Exception:
            log.debug("saved container cleanup failed", exc_info=True)
        if removed:
            log.warning("service removed %s legacy non-playable playlist/favorite rows", removed)

    # -- state -----------------------------------------------------------
    def _on_event(self, event, **payload):
        try:
            self._bump(event)
        except Exception:
            log.exception("service event broadcast failed")

    def _bump(self, reason="state"):
        with self.lock:
            self.revision += 1
            payload = {
                "revision": self.revision,
                "reason": reason,
                "playback": self._playback(True),
                "sources": self._sources(),
                "capabilities": self._capabilities(),
                "server_time": time.time(),
            }
        try:
            self.notify("state.changed", payload)
        except Exception:
            log.exception("service notification failed")

    def touch(self, reason="state"):
        self._bump(reason)

    def _sample(self, force=False):
        now = time.monotonic()
        if not force and now - self._sample_at < 0.45:
            return self._sample_value
        app = self.app
        value = dict(self._sample_value)
        try:
            if app.current and app.current.source == "spotify":
                value["position"] = float(app.playback_position())
                value["active"] = bool(app.spotify_state.get("is_playing"))
                value["paused"] = bool(app.current and not value["active"])
                value["muted"] = float(app.volume) <= 0.001
            else:
                proc = getattr(app.player, "proc", None)
                running = bool(proc and proc.poll() is None and app.player.sock and os.path.exists(app.player.sock))
                value["active"] = bool(running)
                value["paused"] = bool(running and app.player.prop("pause") is True)
                value["muted"] = bool(running and app.player.prop("mute") is True)
                raw_pos = app.player.prop("time-pos") if running else None
                value["position"] = float(raw_pos if raw_pos is not None else (app.resume_position or 0))
        except Exception:
            log.debug("transport sample failed", exc_info=True)
        self._sample_at = now
        self._sample_value = value
        return value

    def _status(self, sample=None):
        app = self.app
        sample = sample or self._sample()
        state = str((app.playback_state or {}).get("state") or "")
        if state == "starting":
            return "loading"
        if state == "loading" and not sample.get("active"):
            return "loading"
        if not app.current:
            return "idle"
        if app.current.source == "spotify":
            return "playing" if app.spotify_state.get("is_playing") else "paused"
        if state == "stopped":
            return "stopped"
        if sample.get("active"):
            return "paused" if sample.get("paused") else "playing"
        return "paused"

    def _playback(self, force=False):
        app = self.app
        sample = self._sample(force)
        status = self._status(sample)
        t = app.current
        return {
            "status": status,
            "active": status in ("playing", "paused", "loading"),
            "transport": "spotify" if t and t.source == "spotify" else ("mpv" if t else ""),
            "source": getattr(t, "source", "") if t else "",
            "position": max(0.0, float(sample.get("position") or 0)),
            "duration": float(app.playback_duration() or (t.duration if t else 0) or 0),
            "seekable": bool(t.seekable) if t else False,
            "volume": float(app.volume),
            "muted": bool(sample.get("muted")),
            "shuffle": app.shuffle_mode,
            "repeat": app.repeat_mode,
            "current": self.track_wire(t) if t else None,
            "context_index": int(app.context_index),
            "context": [self.track_wire(x) for x in (app.play_context or [])],
            "queue": [self.track_wire(x) for x in (app.queue or [])],
            "later": [self.track_wire(x) for x in (app.later_queue or [])],
            "radio": dict(app.radio_info.get(t.id, {})) if t and t.kind == "radio" else {},
            "sampled_at": time.monotonic(),
        }

    def _sources(self):
        app = self.app
        out = []
        for spec in app.sources.all():
            enabled = True
            configured = True
            if spec.id == "local":
                configured = app.db.count() > 0
            elif spec.id == "navidrome":
                enabled = app.cfg.getbool("navidrome_enabled", True)
                configured = app.nav_configured()
            elif spec.id == "spotify":
                enabled = app.cfg.getbool("spotify_enabled", False)
                configured = app.spotify_configured()
            elif spec.id == "sr":
                enabled = app.cfg.getbool("sr_enabled", True)
            elif spec.id == "youtube":
                enabled = app.cfg.getbool("youtube_enabled", True)
                configured = app.yt.available()
            elif spec.id == "radio":
                enabled = app.cfg.getbool("custom_radio_enabled", True)
                configured = bool(app.db.radios())
            out.append({
                "id": spec.id,
                "label": spec.label,
                "capabilities": sorted(spec.capabilities),
                "enabled": bool(enabled),
                "configured": bool(configured),
            })
        return out

    def _capabilities(self):
        app = self.app
        cava = bool(shutil.which("cava"))
        return {
            "seek": True,
            "equalizer": False,
            "visualization": cava,
            "artwork": True,
            "drag_drop": True,
            "queue_edit": True,
            "playlists": True,
            "favorites": True,
            "ratings": True,
            "spotify": bool(app.spotify_configured()),
            "navidrome": bool(app.nav_configured()),
            "youtube": bool(app.yt.available()),
            "cava": cava,
            "mpv": bool(shutil.which("mpv")),
            "backend_version": __version__,
            "protocol_version": PROTOCOL_VERSION,
        }

    def snapshot(self):
        with self.lock:
            return {
                "protocol_version": PROTOCOL_VERSION,
                "backend": {
                    "name": "nplay",
                    "version": __version__,
                    "pid": os.getpid(),
                    "socket": str(self._socket_path or ""),
                    "started": self._started,
                    "instance": f"{os.getpid()}-{int(self._started)}",
                },
                "revision": self.revision,
                "server_time": time.time(),
                "playback": self._playback(True),
                "sources": self._sources(),
                "capabilities": self._capabilities(),
            }

    # -- tracks ----------------------------------------------------------
    def _safe_meta(self, meta):
        out = {}
        for k, v in (meta or {}).items():
            if k not in META_ALLOW:
                continue
            if isinstance(v, (str, int, float, bool)) or v is None:
                if isinstance(v, str) and len(v) > 512:
                    v = v[:512]
                out[k] = v
        return out

    def track_wire(self, t):
        if not t:
            return None
        self._remember(t)
        return {
            "key": f"{t.source}:{t.id}",
            "source": t.source,
            "id": str(t.id),
            "title": t.title or "",
            "artist": t.artist or "",
            "album": t.album or "",
            "duration": float(t.duration or 0),
            "kind": t.kind or "track",
            "seekable": bool(t.seekable),
            "has_art": bool(t.cover),
            "meta": self._safe_meta(t.meta),
        }

    def _remember(self, t):
        if not t:
            return
        key = f"{t.source}:{t.id}"
        if key in self._catalog:
            self._catalog.move_to_end(key)
        self._catalog[key] = t
        while len(self._catalog) > MAX_TRACKS:
            self._catalog.popitem(last=False)

    def _remember_many(self, xs):
        for t in xs or []:
            self._remember(t)

    def _resolve(self, ref):
        if isinstance(ref, str):
            key = ref
        elif isinstance(ref, dict):
            key = ref.get("key") or (f"{ref.get('source')}:{ref.get('id')}" if ref.get("source") and ref.get("id") else "")
        else:
            key = ""
        if not key:
            raise ParamsError("track reference requires key or source+id")
        t = self._catalog.get(key)
        if t is None:
            raise ParamsError("unknown track reference; browse or search it first", {"track": str(key)[:200]})
        return t

    def _resolve_many(self, refs):
        if not isinstance(refs, list):
            raise ParamsError("context must be a list of track references")
        if len(refs) > 5000:
            raise ParamsError("context is too large")
        return [self._resolve(x) for x in refs]

    # -- playback --------------------------------------------------------
    def play(self, p):
        t = self._resolve(p.get("track"))
        if not playable_track(t):
            raise ParamsError("container items must be expanded into tracks before playback", {"kind": t.kind})
        context = p.get("context")
        index = p.get("index")
        with self.lock:
            if context is not None:
                xs = self._resolve_many(context)
                if not xs:
                    raise ParamsError("context is empty")
                idx = int(index if index is not None else next((i for i, x in enumerate(xs) if x.source == t.source and x.id == t.id), 0))
                self.app.set_play_context(xs, idx)
            resumed = float(p.get("resume_position") or 0)
            self.app.play(t, self.ui, preserve_context=bool(p.get("preserve_context")), resume_pos=resumed)
            self.revision += 1
            return self._playback()

    def toggle(self, p):
        with self.lock:
            self.app.play_pause(self.ui)
        self.touch("playback")
        return self._playback()

    def stop(self, p):
        with self.lock:
            self.app.stop_and_clear(self.ui)
        self.touch("playback")
        return self._playback()

    def next(self, p):
        with self.lock:
            self.app.next_track(self.ui)
        self.touch("playback")
        return self._playback()

    def previous(self, p):
        with self.lock:
            self.app.previous_track(self.ui)
        self.touch("playback")
        return self._playback()

    def seek(self, p):
        with self.lock:
            if "position" in p:
                target = max(0.0, float(p.get("position") or 0))
                delta = target - self.app.playback_position()
            else:
                delta = float(p.get("delta") or 0)
            if abs(delta) > 0.001:
                self.app.playback_seek_relative(delta)
        self.touch("position")
        return {"position": self.app.playback_position()}

    def volume(self, p):
        with self.lock:
            if "value" in p:
                target = max(0.0, min(150.0, float(p.get("value") or 0)))
                delta = target - float(self.app.volume)
            else:
                delta = float(p.get("delta") or 0)
            if abs(delta) > 0.001:
                self.app.playback_volume(delta)
        self.touch("volume")
        return {"volume": self.app.volume}

    def mute(self, p):
        with self.lock:
            self.app.playback_mute()
        self.touch("volume")
        return {"muted": self._sample(True).get("muted", False), "volume": self.app.volume}

    def repeat(self, p):
        mode = p.get("mode")
        with self.lock:
            if mode is not None:
                if mode not in ("off", "track", "context"):
                    raise ParamsError("repeat mode must be off, track or context")
                while self.app.repeat_mode != mode:
                    before = self.app.repeat_mode
                    self.app.cycle_repeat(self.ui)
                    if self.app.repeat_mode == before:
                        break
                self.app.cfg.set("repeat_mode", mode)
            else:
                self.app.cycle_repeat(self.ui)
            self.app.save_state()
        self.touch("repeat")
        return {"repeat": self.app.repeat_mode}

    def shuffle(self, p):
        mode = p.get("mode")
        with self.lock:
            if mode is not None:
                if mode not in ("off", "shuffle", "smart"):
                    raise ParamsError("shuffle mode must be off, shuffle or smart")
                while self.app.shuffle_mode != mode:
                    before = self.app.shuffle_mode
                    self.app.cycle_shuffle(self.ui)
                    if self.app.shuffle_mode == before:
                        break
            else:
                self.app.cycle_shuffle(self.ui)
            self.app.save_state()
        self.touch("shuffle")
        return {"shuffle": self.app.shuffle_mode}

    # -- queue -----------------------------------------------------------
    def queue_list(self, p):
        return {"items": [self.track_wire(t) for t in self.app.queue], "later": [self.track_wire(t) for t in self.app.later_queue]}

    def queue_add(self, p):
        t = self._resolve(p.get("track"))
        if not playable_track(t):
            raise ParamsError("container items must be expanded into tracks before queueing", {"kind": t.kind})
        with self.lock:
            if p.get("next"):
                self.app.queue_next(t, self.ui)
            else:
                self.app.queue_add(t, self.ui)
        self.touch("queue")
        return self.queue_list(p)

    def queue_add_many(self, p):
        refs = p.get("tracks")
        if not isinstance(refs, list) or not refs:
            raise ParamsError("tracks must be a non-empty list")
        if len(refs) > 1000:
            raise ParamsError("too many tracks")
        xs = self._resolve_many(refs)
        bad = next((t for t in xs if not playable_track(t)), None)
        if bad is not None:
            raise ParamsError("container items must be expanded into tracks before queueing", {"kind": bad.kind})
        with self.lock:
            if p.get("next"):
                self.app.queue[0:0] = xs
            else:
                self.app.queue.extend(xs)
            for t in xs:
                self._remember(t)
            self.app.save_state()
        self.touch("queue")
        return self.queue_list(p)

    def queue_play(self, p):
        index = int(p.get("index"))
        with self.lock:
            if index < 0 or index >= len(self.app.queue):
                raise ParamsError("queue index out of range")
            self.app.queue_play(index, self.ui)
        self.touch("queue")
        return self._playback()

    def queue_remove(self, p):
        index = int(p.get("index"))
        with self.lock:
            if index < 0 or index >= len(self.app.queue):
                raise ParamsError("queue index out of range")
            self.app.queue_remove(index, self.ui)
        self.touch("queue")
        return self.queue_list(p)

    def queue_reorder(self, p):
        src = int(p.get("from"))
        dst = int(p.get("to"))
        with self.lock:
            if src < 0 or src >= len(self.app.queue):
                raise ParamsError("queue source index out of range")
            dst = max(0, min(len(self.app.queue) - 1, dst))
            item = self.app.queue.pop(src)
            self.app.queue.insert(dst, item)
            self.app.save_state()
        self.touch("queue")
        return {"index": dst, **self.queue_list(p)}

    def queue_move(self, p):
        index = int(p.get("index"))
        delta = int(p.get("delta", 0))
        with self.lock:
            if index < 0 or index >= len(self.app.queue):
                raise ParamsError("queue index out of range")
            new_index = self.app.queue_move(index, delta, self.ui)
        self.touch("queue")
        return {"index": new_index, **self.queue_list(p)}

    def queue_clear(self, p):
        with self.lock:
            self.app.queue_clear(self.ui)
            self.app.later_queue = []
            self.app.save_state()
        self.touch("queue")
        return self.queue_list(p)

    def queue_save(self, p):
        name = str(p.get("name") or "").strip()
        if not name:
            raise ParamsError("playlist name is required")
        with self.lock:
            if not self.app.queue:
                raise BackendError("queue is empty")
            pid = self.app.db.playlist_create(name)
            for t in self.app.queue:
                self.app.db.playlist_add(pid, t)
        return {"playlist_id": pid, "name": name}

    # -- browse ----------------------------------------------------------
    def browse(self, p):
        source = str(p.get("source") or "local")
        section = str(p.get("section") or "all")
        query = str(p.get("query") or "").strip()
        limit = max(1, min(1000, int(p.get("limit") or 200)))
        offset = max(0, int(p.get("offset") or 0))
        args = p.get("args") if isinstance(p.get("args"), dict) else {}
        if source == "local":
            items, title = self._browse_local(section, query, limit, offset, args)
        elif source == "navidrome":
            items, title = self._browse_navidrome(section, query, limit, args)
        elif source == "library":
            items, title = self._browse_library(section, query, limit, offset, args)
        elif source == "sr":
            items, title = self._browse_sr(section, query, limit, args)
        elif source == "youtube":
            if not self.app.cfg.getbool("youtube_enabled", True) or not self.app.yt.available():
                raise BackendError("YouTube is disabled or yt-dlp is not installed")
            items = self.app.yt.search(query, limit) if query else []
            title = "YouTube · " + (query or "search")
        elif source == "spotify":
            items, title = self._browse_spotify(section, query, limit, args)
        elif source == "radio":
            items = self.app.db.radios()
            title = "Custom radio"
        else:
            raise ParamsError("unknown browse source", {"source": source})
        self._remember_many(items)
        return {"source": source, "section": section, "title": title, "items": [self.track_wire(x) for x in items]}

    def _browse_local(self, section, query, limit, offset, args):
        db = self.app.db
        if section in ("all", "search"):
            xs = db.search(query, limit + offset)
            xs = xs[offset:offset + limit]
            return xs, "Local music · " + (query or "all tracks")
        if section == "artists":
            return [Track(source="local", id=name, title=name, artist="Artist", kind="artist", seekable=False, meta={"local_artist": name}) for name, _ in db.local_artists()[:limit]], "Local artists"
        if section == "albums":
            out = []
            for title, action, _subtitle in db.local_albums()[:limit]:
                raw = action.split("local:album:", 1)[-1]
                artist, _, album = raw.partition("\t")
                out.append(Track(source="local", id=raw, title=album or title, artist=artist, album=album or title, kind="album", seekable=False, meta={"local_artist": artist}))
            return out, "Local albums"
        if section == "folders":
            out = []
            for title, action, _subtitle in db.local_folders()[:limit]:
                out.append(Track(source="local", id=action.split("local:folder:", 1)[-1], title=title, kind="folder", seekable=False))
            return out, "Local folders"
        if section == "recent":
            return db.recently_added(limit), "Recently added"
        if section == "random":
            return db.random_tracks(limit), "Random tracks"
        if section == "artist":
            name = str(args.get("name") or query)
            if not name:
                raise ParamsError("artist name is required")
            return db.tracks_by_artist(name), "Local · " + name
        if section == "album":
            artist = str(args.get("artist") or "")
            album = str(args.get("album") or query)
            if not album:
                raise ParamsError("album name is required")
            return db.tracks_by_album(artist, album), "Local · " + album
        if section == "folder":
            path = str(args.get("path") or query)
            if not path:
                raise ParamsError("folder path is required")
            return db.tracks_by_folder(path), "Local folder"
        raise ParamsError("unknown local section", {"section": section})

    def _nav(self):
        n = self.app.nav()
        if not n:
            raise BackendError("Navidrome is disabled or not configured")
        return n

    def _browse_navidrome(self, section, query, limit, args):
        n = self._nav()
        if section == "newest":
            return n.albums("newest", limit), "Navidrome · Recently added"
        if section == "random":
            return n.albums("random", limit), "Navidrome · Random albums"
        if section == "random_tracks":
            return n.random_tracks(limit), "Navidrome · Random tracks"
        if section == "artists":
            return n.artists(), "Navidrome · Artists"
        if section == "albums":
            return n.albums("alphabeticalByName", limit), "Navidrome · Albums"
        if section == "playlists":
            return n.playlists(), "Navidrome · Playlists"
        if section == "search":
            return n.search_library(query, limit, 20, 10), "Navidrome · " + query
        if section == "album":
            return n.album_tracks(str(args.get("id") or query)), "Navidrome album"
        if section == "artist":
            return n.artist_albums(str(args.get("id") or query)), "Navidrome artist"
        if section == "playlist":
            return n.playlist_tracks(str(args.get("id") or query)), "Navidrome playlist"
        raise ParamsError("unknown Navidrome section", {"section": section})

    def _browse_library(self, section, query, limit, offset, args):
        db = self.app.db
        if section == "playlists":
            out = []
            for name, action, subtitle in db.playlists()[:limit]:
                pid = action.split(":", 1)[-1]
                out.append(Track(source="library", id=pid, title=name, artist="Playlist", kind="playlist", seekable=False, meta={"item_count": subtitle}))
            return out, "Playlists"
        if section == "playlist":
            pid = int(args.get("id") or query)
            return db.playlist_tracks(pid), db.playlist_name(pid)
        if section == "favorites":
            return db.favorites()[:limit], "Favorites"
        if section == "history":
            return db.history(limit), "Recently played"
        if section == "discoveries":
            return db.discoveries(limit), "Discoveries"
        if section == "bookmarks":
            return db.bookmarks(limit), "Bookmarks"
        raise ParamsError("unknown library section", {"section": section})

    def _browse_sr(self, section, query, limit, args):
        if not self.app.cfg.getbool("sr_enabled", True):
            raise BackendError("Sveriges Radio is disabled")
        if section == "live":
            return self.app.sr.live(), "Sveriges Radio · Live"
        if section == "programs":
            return self.app.sr.programs(query, limit), "Sveriges Radio · Programmes"
        if section == "search":
            return self.app.sr.search_programmes_and_episodes(query, limit), "Sveriges Radio · " + query
        if section == "program":
            return self.app.sr.program_episodes(str(args.get("id") or query), limit), "Sveriges Radio · Episodes"
        raise ParamsError("unknown Sveriges Radio section", {"section": section})

    def _browse_spotify(self, section, query, limit, args):
        if not self.app.cfg.getbool("spotify_enabled", False) or not self.app.spotify_configured():
            raise BackendError("Spotify is disabled or not connected")
        sp = self.app.spotify
        if section == "search":
            return sp.search(query, limit), "Spotify · " + query
        if section == "playlists":
            return sp.playlists(), "Spotify · Playlists"
        if section == "playlist":
            return sp.playlist_items(str(args.get("id") or query)), "Spotify playlist"
        if section == "album":
            return sp.album_tracks(str(args.get("id") or query)), "Spotify album"
        if section == "top_tracks":
            return sp.top_tracks(), "Spotify · Top tracks"
        if section == "top_artists":
            return sp.top_artists(), "Spotify · Top artists"
        if section == "recent":
            return sp.recent(), "Spotify · Recently played"
        if section == "devices":
            return [Track(source="spotify", id=str(d.get("id", "")), title=str(d.get("name", "")), artist=str(d.get("type", "")), kind="device", seekable=False, meta={"explicit": bool(d.get("is_active"))}) for d in sp.devices()], "Spotify · Devices"
        raise ParamsError("unknown Spotify section", {"section": section})

    def search(self, p):
        query = str(p.get("query") or "").strip()
        if not query:
            return {"query": "", "groups": []}
        if len(query) > 200:
            raise ParamsError("query is too long")
        limit = max(1, min(100, int(p.get("limit") or 20)))
        requested = p.get("sources")
        allowed = set(requested) if isinstance(requested, list) else None
        jobs = {}
        app = self.app
        if allowed is None or "local" in allowed:
            jobs["local"] = ("Local", lambda: app.db.search(query, limit))
        if (allowed is None or "navidrome" in allowed) and app.nav_configured() and app.cfg.getbool("navidrome_enabled", True):
            jobs["navidrome"] = ("Navidrome", lambda: app.nav().search_library(query, limit, 10, 5))
        if (allowed is None or "spotify" in allowed) and app.spotify_configured() and app.cfg.getbool("spotify_enabled", False):
            jobs["spotify"] = ("Spotify", lambda: app.spotify.search(query, limit))
        if (allowed is None or "sr" in allowed) and app.cfg.getbool("sr_enabled", True):
            jobs["sr"] = ("Sveriges Radio", lambda: app.sr.search_programmes_and_episodes(query, limit))
        if (allowed is None or "youtube" in allowed) and app.yt.available() and app.cfg.getbool("youtube_enabled", True):
            jobs["youtube"] = ("YouTube", lambda: app.yt.search(query, limit))
        groups = []
        if jobs:
            with concurrent.futures.ThreadPoolExecutor(max_workers=min(5, len(jobs))) as pool:
                futures = {key: pool.submit(fn) for key, (_, fn) in jobs.items()}
                for key in ("local", "navidrome", "spotify", "sr", "youtube"):
                    if key not in futures:
                        continue
                    try:
                        items = futures[key].result(timeout=30)
                    except Exception as e:
                        log.warning("service search %s failed: %s", key, redact_text(e))
                        items = []
                        error = redact_text(e)[:200]
                    else:
                        error = ""
                    self._remember_many(items)
                    groups.append({
                        "source": key,
                        "label": jobs[key][0],
                        "items": [self.track_wire(x) for x in items],
                        "error": error,
                    })
        return {"query": query, "groups": groups}

    # -- playlists / favorites / ratings --------------------------------
    def playlists_list(self, p):
        out = []
        for name, action, subtitle in self.app.db.playlists():
            out.append({"id": int(action.split(":", 1)[-1]), "name": name, "subtitle": subtitle})
        return {"items": out}

    def playlist_tracks(self, p):
        pid = int(p.get("id"))
        xs = self.app.db.playlist_tracks(pid)
        self._remember_many(xs)
        return {"id": pid, "name": self.app.db.playlist_name(pid), "items": [self.track_wire(x) for x in xs]}

    def playlist_create(self, p):
        name = str(p.get("name") or "").strip()
        if not name or len(name) > 200:
            raise ParamsError("playlist name is required")
        pid = self.app.db.playlist_create(name)
        return {"id": pid, "name": name}

    def playlist_add(self, p):
        pid = int(p.get("id"))
        t = self._resolve(p.get("track"))
        if not playable_track(t):
            raise ParamsError("container items must be expanded into tracks before adding to a playlist", {"kind": t.kind})
        self.app.db.playlist_add(pid, t)
        return self.playlist_tracks({"id": pid})

    def playlist_add_many(self, p):
        pid = int(p.get("id"))
        refs = p.get("tracks")
        if not isinstance(refs, list) or not refs:
            raise ParamsError("tracks must be a non-empty list")
        if len(refs) > 5000:
            raise ParamsError("too many tracks")
        xs = self._resolve_many(refs)
        bad = next((t for t in xs if not playable_track(t)), None)
        if bad is not None:
            raise ParamsError("container items must be expanded into tracks before adding to a playlist", {"kind": bad.kind})
        with self.lock:
            for t in xs:
                self.app.db.playlist_add(pid, t)
            self._remember_many(xs)
        return self.playlist_tracks({"id": pid})

    def playlist_remove(self, p):
        pid = int(p.get("id"))
        index = int(p.get("index"))
        self.app.db.playlist_remove(pid, index)
        return self.playlist_tracks({"id": pid})

    def playlist_move(self, p):
        pid = int(p.get("id"))
        index = int(p.get("index"))
        delta = int(p.get("delta", 0))
        new_index = self.app.db.playlist_move(pid, index, delta)
        return {"index": new_index, **self.playlist_tracks({"id": pid})}

    def playlist_rename(self, p):
        pid = int(p.get("id"))
        name = str(p.get("name") or "").strip()
        if not name:
            raise ParamsError("playlist name is required")
        self.app.db.playlist_rename(pid, name)
        return {"id": pid, "name": name}

    def playlist_delete(self, p):
        pid = int(p.get("id"))
        self.app.db.playlist_delete(pid)
        return {"deleted": pid}

    def favorites_list(self, p):
        xs = self.app.db.favorites()
        self._remember_many(xs)
        return {"items": [self.track_wire(x) for x in xs]}

    def favorite_toggle(self, p):
        t = self._resolve(p.get("track"))
        if not playable_track(t):
            raise ParamsError("favorites are supported for playable tracks only", {"kind": t.kind})
        added = self.app.db.favorite(t)
        return {"added": bool(added), "track": self.track_wire(t)}

    def rating_get(self, p):
        t = self._resolve(p.get("track"))
        return {"rating": self.app.db.rating_get(t.id, t.source), "track": self.track_wire(t)}

    def rating_set(self, p):
        t = self._resolve(p.get("track"))
        if not playable_track(t):
            raise ParamsError("ratings are supported for playable tracks only", {"kind": t.kind})
        rating = max(0, min(5, int(p.get("rating") or 0)))
        self.app.db.rating_set(t.id, rating, t.source)
        return {"rating": rating, "track": self.track_wire(t)}

    # -- radio -----------------------------------------------------------
    def radio_add(self, p):
        name = str(p.get("name") or "").strip()
        url = str(p.get("url") or "").strip()
        if not name or not url.startswith(("http://", "https://")):
            raise ParamsError("station needs a name and an http(s) stream URL")
        self.app.db.radio_add(name, url)
        return {"ok": True}

    def radio_delete(self, p):
        station_id = int(p.get("id"))
        self.app.db.radio_delete(station_id)
        return {"deleted": station_id}

    # -- artwork ---------------------------------------------------------
    def artwork(self, p):
        t = self._resolve(p.get("track"))
        if not t.cover:
            return {"mime": "", "data": "", "empty": True}
        key = f"{t.source}:{t.id}:{t.cover}"
        with self.lock:
            hit = self._art_cache.get(key)
            if hit:
                self._art_cache.move_to_end(key)
                return {"mime": hit[0], "data": base64.b64encode(hit[1]).decode("ascii")}
        data = None
        path = None
        cover = str(t.cover)
        try:
            if cover.startswith(("http://", "https://")):
                path = self.app.artwork_cache.fetch(cover, timeout=8)
            else:
                path = Path(cover).expanduser()
                if not path.is_file():
                    path = None
            if path:
                if path.stat().st_size > 12 * 1024 * 1024:
                    raise BackendError("artwork is too large")
                data = path.read_bytes()
        except RpcError:
            raise
        except Exception as e:
            raise BackendError("artwork unavailable", {"detail": redact_text(e)[:200]})
        if not data:
            return {"mime": "", "data": "", "empty": True}
        mime = mimetypes.guess_type(str(path))[0] or "application/octet-stream"
        with self.lock:
            self._art_cache[key] = (mime, data)
            self._art_bytes += len(data)
            self._art_cache.move_to_end(key)
            while self._art_bytes > 24 * 1024 * 1024 and len(self._art_cache) > 1:
                _, (_, old) = self._art_cache.popitem(last=False)
                self._art_bytes -= len(old)
        return {"mime": mime, "data": base64.b64encode(data).decode("ascii")}

    # -- visualization ---------------------------------------------------
    def visualization_start(self, p):
        if not shutil.which("cava"):
            raise BackendError("CAVA is not installed", {"remedy": "Install cava to enable visualization"})
        with self._vis_lock:
            if self._vis is None:
                self._vis = Visualizer()
            if not self._vis.running and not self._vis.start():
                raise BackendError("CAVA could not start", {"remedy": "Check the PipeWire/PulseAudio monitor source and restart cava"})
            self._vis_last = time.monotonic()
            return {"available": True, "bars": self._vis.bars}

    def visualization_stop(self, p):
        with self._vis_lock:
            if self._vis:
                self._vis.stop()
                self._vis = None
            return {"available": True}

    def visualization_data(self, p):
        bars = max(8, min(96, int(p.get("bars") or 32)))
        with self._vis_lock:
            if self._vis is None:
                self._vis = Visualizer()
            if not self._vis.running:
                # CAVA can exit when the audio server or monitor source is not
                # ready yet. Recover on the next visualizer frame instead of
                # leaving the GUI stuck on "unavailable".
                if not self._vis.start():
                    raise BackendError("CAVA could not start", {"remedy": "Check the PipeWire/PulseAudio monitor source and restart cava"})
                log.info("service restarted CAVA after an unexpected exit")
            self._vis_last = time.monotonic()
            return {"bars": [int(x) for x in self._vis.get(bars)]}

    def _visualization_idle(self):
        with self._vis_lock:
            if self._vis and time.monotonic() - self._vis_last > 15:
                self._vis.stop()
                self._vis = None

    def _vis_monitor_loop(self):
        while not self._vis_stop.wait(5.0):
            try:
                self._visualization_idle()
            except Exception:
                log.debug("visualizer idle check failed", exc_info=True)

    # -- settings / sources ---------------------------------------------
    def settings_get(self, p):
        c = self.app.cfg
        out = {}
        for key in sorted(SAFE_SETTINGS):
            out[key] = c.getbool(key, False) if key in BOOL_SETTINGS else c.get(key, "")
        return {
            "settings": out,
            "navidrome": {"configured": self.app.nav_configured(), "url": c.get("navidrome_url", ""), "user": c.get("navidrome_user", "")},
            "spotify": {"configured": self.app.spotify_configured(), "enabled": c.getbool("spotify_enabled", False)},
            "library": {"roots": self.app.roots(), "tracks": self.app.db.count(), "last_scan": dict(self.app.db.last_scan or {})},
            "system": {"version": __version__, "protocol": PROTOCOL_VERSION, "log": str(LOG_PATH)},
        }

    def settings_set(self, p):
        values = p.get("settings")
        if not isinstance(values, dict):
            raise ParamsError("settings must be an object")
        changed = []
        with self.lock:
            for key, value in values.items():
                if key not in SAFE_SETTINGS:
                    raise ParamsError("setting is not writable through the control protocol", {"key": key})
                if key in BOOL_SETTINGS:
                    if isinstance(value, str):
                        if value.lower() not in ("true", "false"):
                            raise ParamsError("boolean setting requires true or false", {"key": key})
                        value = value.lower() == "true"
                    elif not isinstance(value, bool):
                        raise ParamsError("boolean setting requires true or false", {"key": key})
                    self.app.cfg.set(key, "true" if value else "false")
                else:
                    if key == "preferred_source" and value not in ("local", "navidrome", "spotify", "youtube"):
                        raise ParamsError("unknown preferred source")
                    if key == "normalization_mode" and value not in ("auto", "track", "album"):
                        raise ParamsError("unknown normalization mode")
                    self.app.cfg.set(key, str(value))
                changed.append(key)
        self.touch("settings")
        return {"changed": changed, **self.settings_get({})}

    def navidrome_setup(self, p):
        url = str(p.get("url") or "").strip().rstrip("/")
        user = str(p.get("user") or "").strip()
        password = str(p.get("password") or "")
        if not url.startswith(("http://", "https://")) or not user or not password:
            raise ParamsError("server URL, user and password are required")
        probe = Navidrome(url, user, password)
        try:
            probe.ping()
        except Exception as e:
            raise BackendError("Navidrome connection failed", {"detail": redact_text(e)[:200], "remedy": "check the URL and password in Settings → Navidrome"})
        self.app.cfg.save_nav(url, user, password)
        self.touch("settings")
        return {"configured": True, "url": url, "user": user}

    def navidrome_clear(self, p):
        self.app.cfg.clear_nav()
        self.touch("settings")
        return {"configured": False}

    def spotify_connect(self, p):
        if not self.app.cfg.get("spotify_client_id", "").strip():
            raise BackendError("Spotify client ID is not configured", {"remedy": "Set spotify_client_id in NPLAY settings or run `nplay` and use Spotify setup"})
        if self.app.spotify_configured():
            return {"connected": True}

        def work():
            try:
                self.app.spotify.connect(open_browser=True, timeout=180)
                self.notify("spotify.auth", {"connected": True})
            except Exception as e:
                self.notify("spotify.auth", {"connected": False, "error": str(e)[:300]})

        threading.Thread(target=work, daemon=True, name="nplay-spotify-auth").start()
        return {"started": True}

    def spotify_disconnect(self, p):
        self.app.spotify.disconnect()
        return {"connected": False}

    def spotify_devices(self, p):
        if not self.app.spotify_configured():
            raise BackendError("Spotify is not connected")
        return {"items": self._browse_spotify("devices", "", 50, {})[0]}

    def spotify_transfer(self, p):
        did = str(p.get("device_id") or "")
        if not did:
            raise ParamsError("device_id is required")
        self.app.spotify.transfer(did, bool(p.get("play")))
        return {"ok": True}

    # -- external sources / scan ----------------------------------------
    def play_external(self, p):
        path = str(p.get("path") or "").strip()
        url = str(p.get("url") or "").strip()
        title = str(p.get("title") or "").strip()
        source = "local"
        if url:
            if not url.startswith(("http://", "https://")):
                raise ParamsError("only http(s) URLs can be opened directly")
            if title and url.lower().endswith((".m3u", ".m3u8", ".pls")):
                raise ParamsError("playlist URLs are not supported; use NPLAY radio setup")
            t = Track(source="radio", id=url, title=title or url, artist="External stream", url=url, kind="radio", seekable=False)
            source = "radio"
        elif path:
            q = Path(path).expanduser()
            if not q.is_file():
                raise ParamsError("file does not exist", {"path": path[:300]})
            if q.suffix.lower() not in (".mp3", ".flac", ".ogg", ".opus", ".m4a", ".aac", ".wav", ".webm"):
                raise ParamsError("unsupported media file")
            t = Track(source="local", id=str(q), title=title or q.stem, artist="Dropped file", album=q.parent.name, path=str(q), url=str(q), seekable=True)
        else:
            raise ParamsError("path or url is required")
        self._remember(t)
        if p.get("queue"):
            with self.lock:
                if p.get("next"):
                    self.app.queue.insert(0, t)
                else:
                    self.app.queue.append(t)
                self.app.save_state()
            self.touch("queue")
            return {"track": self.track_wire(t), "source": source, "queued": True}
        with self.lock:
            self.app.set_play_context([t], 0)
            self.app.play(t, self.ui, preserve_context=False)
        self.touch("playback")
        return {"track": self.track_wire(t), "source": source, "queued": False}

    def library_scan(self, p):
        if self._scanning:
            raise BackendError("a library scan is already running")

        def work():
            self._scanning = True
            try:
                result = self.app.scan(force=bool(p.get("force")))
                self.notify("library.scanned", {"result": result})
            except Exception as e:
                self.notify("library.scanned", {"error": str(e)[:300]})
            finally:
                self._scanning = False
                self.touch("library")

        threading.Thread(target=work, daemon=True, name="nplay-library-scan").start()
        return {"started": True}

    def library_stats(self, p):
        return {
            "tracks": self.app.db.count(),
            "roots": self.app.roots(),
            "schema": self.app.db.schema_version(),
            "last_scan": dict(self.app.db.last_scan or {}),
        }

    # -- lifecycle -------------------------------------------------------
    def info(self, p):
        return {
            "protocol_version": PROTOCOL_VERSION,
            "backend_version": __version__,
            "pid": os.getpid(),
            "socket": str(self._socket_path or ""),
            "uptime": max(0.0, time.monotonic() - self._started),
            "idle_exit": self.idle_exit,
        }

    def close(self):
        self._vis_stop.set()
        try:
            self._executor.shutdown(wait=False, cancel_futures=True)
        except Exception:
            pass
        try:
            self.visualization_stop({})
        except Exception:
            pass
        try:
            self.app.save_state()
            self.app.stop_playback(None)
        except Exception:
            log.exception("service shutdown playback stop failed")
        try:
            self.app.spotify.local.stop()
        except Exception:
            pass
        try:
            self.app.mpris.stop()
        except Exception:
            pass

    _socket_path = None

    # -- dispatcher ------------------------------------------------------
    def dispatch(self, method, params):
        table = {
            "ping": lambda p: {"pong": time.time()},
            "initialize": lambda p: self._initialize(p),
            "state.snapshot": lambda p: self.snapshot(),
            "state.subscribe": lambda p: self.snapshot(),
            "state.unsubscribe": lambda p: {"ok": True},
            "playback.play": self.play,
            "playback.toggle": self.toggle,
            "playback.stop": self.stop,
            "playback.next": self.next,
            "playback.previous": self.previous,
            "playback.seek": self.seek,
            "playback.volume": self.volume,
            "playback.mute": self.mute,
            "playback.repeat": self.repeat,
            "playback.shuffle": self.shuffle,
            "playback.play_external": self.play_external,
            "queue.list": self.queue_list,
            "queue.add": self.queue_add,
            "queue.add_many": self.queue_add_many,
            "queue.play": self.queue_play,
            "queue.remove": self.queue_remove,
            "queue.move": self.queue_move,
            "queue.reorder": self.queue_reorder,
            "queue.clear": self.queue_clear,
            "queue.save": self.queue_save,
            "browse": self.browse,
            "search": self.search,
            "playlists.list": self.playlists_list,
            "playlists.tracks": self.playlist_tracks,
            "playlists.create": self.playlist_create,
            "playlists.add": self.playlist_add,
            "playlists.add_many": self.playlist_add_many,
            "playlists.remove": self.playlist_remove,
            "playlists.move": self.playlist_move,
            "playlists.rename": self.playlist_rename,
            "playlists.delete": self.playlist_delete,
            "favorites.list": self.favorites_list,
            "favorites.toggle": self.favorite_toggle,
            "ratings.get": self.rating_get,
            "ratings.set": self.rating_set,
            "radio.add": self.radio_add,
            "radio.delete": self.radio_delete,
            "artwork.get": self.artwork,
            "visualization.start": self.visualization_start,
            "visualization.stop": self.visualization_stop,
            "visualization.data": self.visualization_data,
            "settings.get": self.settings_get,
            "settings.set": self.settings_set,
            "navidrome.setup": self.navidrome_setup,
            "navidrome.clear": self.navidrome_clear,
            "spotify.connect": self.spotify_connect,
            "spotify.disconnect": self.spotify_disconnect,
            "spotify.devices": self.spotify_devices,
            "spotify.transfer": self.spotify_transfer,
            "library.scan": self.library_scan,
            "library.stats": self.library_stats,
            "service.info": self.info,
        }
        fn = table.get(method)
        if fn is None:
            raise RpcError(-32601, "method not found")
        return fn(params or {})

    def _initialize(self, p):
        client = p.get("client") or {}
        versions = p.get("protocol_versions")
        if isinstance(versions, list) and PROTOCOL_VERSION not in versions:
            raise RpcError(-32010, "incompatible protocol version", {"server": PROTOCOL_VERSION, "client": versions})
        snap = self.snapshot()
        snap["protocol_version"] = PROTOCOL_VERSION
        snap["client"] = {
            "name": str(client.get("name") or "unknown")[:80],
            "version": str(client.get("version") or "")[:40],
        }
        snap["capabilities"]["messages"] = [
            "state.changed", "library.scanned", "spotify.auth",
        ]
        return snap
