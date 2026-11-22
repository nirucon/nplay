#!/usr/bin/env python3
"""Protocol conformance dump for the NPLAY control service.

Runs one fixed JSON-RPC session against `NPLAY-COMMAND --serve` in a private
sandbox (offline sources only, fake mpv and fake CAVA) and prints every
response with volatile values masked. The dump of NPLAY 2.x must equal the
golden dump recorded from the Python NPLAY 1.8.2 (tests/golden/protocol.txt):

    tests/protocol_compat.py NPLAY-COMMAND > out.txt
    diff -u tests/golden/protocol.txt out.txt

Usage: protocol_compat.py [--raw] NPLAY-COMMAND...
  FAKE_MPV / FAKE_CAVA override the test doubles (default tools/fake-mpv and
  tools/fake-cava next to this repository).
"""
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import wave

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VOLATILE = {"server_time", "sampled_at", "pid", "uptime", "instance", "revision",
            "position", "started", "started_at", "elapsed", "last_frame", "pushed",
            "suppressed", "dropped", "mtime", "added", "played_at", "created",
            "updated", "ts", "time", "at", "scanned_at", "seconds", "ms", "took", "pong",
            "duration_ms", "last_scan", "uptime_s"}
VERSIONS = {"version", "backend_version"}
RACY = {"playback.next", "playback.previous", "queue.play", "play", "toggle-pause", "toggle-play"}


def mask(v, key="", sb=""):
    if isinstance(v, dict):
        return {k: mask(x, k, sb) for k, x in v.items()}
    if isinstance(v, list):
        return [mask(x, key, sb) for x in v]
    if key == "instance":
        return "<instance>"
    if key in VOLATILE and isinstance(v, (int, float)) and not isinstance(v, bool):
        return "<%s>" % type(v).__name__
    if key in VERSIONS and isinstance(v, str):
        return "<version>"
    if isinstance(v, str) and sb and sb in v:
        return v.replace(sb, "$SB")
    return v


def main():
    raw = "--raw" in sys.argv
    cmd = [a for a in sys.argv[1:] if a != "--raw"]
    # A fixed sandbox path: local track ids hash the file path.
    sb = os.environ.get("COMPAT_SB", "/tmp/nplay-compat-sandbox")
    shutil.rmtree(sb, ignore_errors=True)
    os.makedirs(sb, mode=0o700)
    env = dict(os.environ)
    env.update(HOME=sb + "/home", XDG_CONFIG_HOME=sb + "/config", XDG_DATA_HOME=sb + "/data",
               XDG_STATE_HOME=sb + "/state", XDG_CACHE_HOME=sb + "/cache", XDG_RUNTIME_DIR=sb + "/run",
               TZ="UTC", LC_ALL="C.UTF-8")
    for k in ("DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY", "DISPLAY", "NPLAY_SERVICE_SOCKET", "KITTY_WINDOW_ID"):
        env.pop(k, None)
    os.makedirs(sb + "/bin")
    os.makedirs(sb + "/run", mode=0o700)
    os.makedirs(sb + "/config/nplay")
    os.symlink(os.environ.get("FAKE_MPV", HERE + "/tools/fake-mpv"), sb + "/bin/mpv")
    os.symlink(os.environ.get("FAKE_CAVA", HERE + "/tools/fake-cava"), sb + "/bin/cava")
    env["PATH"] = sb + "/bin:" + env["PATH"]
    music = sb + "/home/Music"
    for d, n in (("Artist/Album", 3), ("Band/Second", 4)):
        os.makedirs(music + "/" + d)
        for i in range(n):
            w = wave.open("%s/%s/%02d Song %d.wav" % (music, d, i + 1, i + 1), "wb")
            w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000)
            w.writeframes(b"\0\0" * 8000 * 30); w.close()
    with open(sb + "/config/nplay/config.ini", "w") as f:
        f.write("[general]\nmusic_dirs = %s\nyoutube_enabled = false\nsr_enabled = false\n"
                "custom_radio_enabled = true\nnavidrome_enabled = false\nstatistics_enabled = false\n"
                "track_notifications = false\nmpris_enabled = false\n" % music)
    subprocess.run(cmd + ["--scan"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    sock_path = sb + "/run/nplay/control.sock"
    srv = subprocess.Popen(cmd + ["--serve"], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    for _ in range(200):
        if os.path.exists(sock_path):
            break
        time.sleep(0.05)
    out = []
    try:
        s = socket.socket(socket.AF_UNIX)
        s.connect(sock_path)
        s.settimeout(10)
        f = s.makefile("rwb")
        notes = []
        state = {"id": 0}

        def send_raw(b):
            f.write(b + b"\n"); f.flush()

        def read_until(rid):
            while True:
                line = f.readline()
                if not line:
                    raise SystemExit("service closed the connection")
                m = json.loads(line)
                if "vis" in m:
                    notes.append("vis:%s" % ("b64" if m.get("enc") == "b64" else "json"))
                    continue
                if "method" in m and "id" not in m:
                    notes.append(m["method"])
                    continue
                if m.get("id") == rid:
                    return m, line

        def call(method, params=None, label=None):
            state["id"] += 1
            rid = state["id"]
            req = {"jsonrpc": "2.0", "id": rid, "method": method}
            if params is not None:
                req["params"] = params
            send_raw(json.dumps(req).encode())
            m, line = read_until(rid)
            if raw:
                out.append("## %s\n%s" % (label or method, line.decode().rstrip()))
            else:
                mm = mask(m, sb=sb)
                # Transport state right after a track change races the player.
                if (label or method) in RACY and isinstance(mm.get("result"), dict) and "status" in mm["result"]:
                    mm["result"]["status"] = "<status>"
                out.append("## %s\n%s" % (label or method, json.dumps(mm, ensure_ascii=False)))
            return m.get("result")

        def settle():
            time.sleep(0.6)

        call("initialize", {"client": {"name": "compat", "version": "1"}, "protocol_versions": [1]})
        call("initialize", {"client": {"name": "compat", "version": "1"}, "protocol_versions": [9]}, "initialize-bad-version")
        call("ping", {})
        call("state.snapshot", {})
        call("state.subscribe", {})
        call("no.such.method", {})
        call("browse", {"source": "nope", "section": "all"}, "browse-bad-source")
        allt = call("browse", {"source": "local", "section": "all", "limit": 50}, "browse-local-all")
        for sec in ("artists", "albums", "folders", "recent", "search"):
            call("browse", {"source": "local", "section": sec, "query": "Song 2", "limit": 20}, "browse-local-" + sec)
        call("browse", {"source": "local", "section": "artist", "args": {"name": "Artist"}}, "browse-local-artist")
        call("browse", {"source": "local", "section": "album", "args": {"artist": "Band", "album": "Second"}}, "browse-local-album")
        call("browse", {"source": "local", "section": "folder", "args": {"path": "Artist"}}, "browse-local-folder")
        call("browse", {"source": "library", "section": "playlists"}, "browse-library-playlists")
        call("browse", {"source": "radio", "section": "all"}, "browse-radio")
        call("search", {"query": "Song 3", "limit": 10})
        tracks = [t for t in (allt or {}).get("items", []) if t.get("kind") == "track"]
        call("playback.play", {"track": tracks[0], "context": tracks, "index": 0}, "play")
        settle()
        call("state.snapshot", {}, "snapshot-playing")
        call("playback.toggle", {}, "toggle-pause"); settle()
        call("playback.toggle", {}, "toggle-play"); settle()
        call("playback.volume", {"delta": 5}, "volume-delta")
        call("playback.volume", {"value": 80}, "volume-value")
        call("playback.volume", {"value": 900}, "volume-clamp")
        call("playback.mute", {}); call("playback.mute", {}, "unmute")
        call("playback.seek", {"position": 10}, "seek-abs")
        call("playback.seek", {"delta": -5}, "seek-rel")
        call("playback.repeat", {}, "repeat-cycle")
        call("playback.repeat", {"mode": "off"}, "repeat-off")
        call("playback.repeat", {"mode": "bogus"}, "repeat-bad")
        call("playback.shuffle", {"mode": "shuffle"}, "shuffle-on")
        call("playback.shuffle", {"mode": "off"}, "shuffle-off")
        call("playback.next", {}); settle()
        call("playback.previous", {}); settle()
        call("queue.add", {"track": tracks[2]})
        call("queue.add", {"track": tracks[3], "next": True}, "queue.add-next")
        call("queue.add_many", {"tracks": tracks[4:6]})
        call("queue.add", {"track": {"key": "local:does-not-exist"}}, "queue.add-unknown")
        call("queue.list", {})
        call("queue.move", {"index": 0, "delta": 1})
        call("queue.reorder", {"from": 2, "to": 0})
        call("queue.remove", {"index": 1})
        call("queue.save", {"name": "Compat queue"})
        call("queue.play", {"index": 0}); settle()
        call("queue.clear", {})
        pl = call("playlists.create", {"name": "Mix"})
        pls = call("playlists.list", {})
        pid = None
        for p in (pls or {}).get("items", pls if isinstance(pls, list) else []):
            if isinstance(p, dict) and (p.get("title") == "Mix" or p.get("name") == "Mix"):
                pid = p.get("id") or (p.get("meta") or {}).get("id")
        if pid is None and isinstance(pl, dict):
            pid = pl.get("id")
        call("playlists.add", {"id": pid, "track": tracks[0]})
        call("playlists.add_many", {"id": pid, "tracks": tracks[1:4]})
        call("playlists.tracks", {"id": pid})
        call("playlists.move", {"id": pid, "index": 0, "delta": 1})
        call("playlists.remove", {"id": pid, "index": 1})
        call("playlists.rename", {"id": pid, "name": "Mix 2"})
        call("browse", {"source": "library", "section": "playlist", "args": {"id": pid}}, "browse-library-playlist")
        call("playlists.delete", {"id": pid})
        call("favorites.toggle", {"track": tracks[1]})
        call("favorites.list", {})
        call("browse", {"source": "library", "section": "favorites"}, "browse-library-favorites")
        call("browse", {"source": "library", "section": "history"}, "browse-library-history")
        call("favorites.toggle", {"track": tracks[1]}, "favorites.untoggle")
        call("ratings.set", {"track": tracks[1], "rating": 4})
        call("ratings.get", {"track": tracks[1]})
        call("ratings.set", {"track": tracks[1], "rating": 9}, "ratings.set-bad")
        r = call("radio.add", {"name": "Test FM", "url": "http://127.0.0.1:9/stream"})
        rad = call("browse", {"source": "radio", "section": "all"}, "browse-radio-after-add")
        rid = None
        for it in (rad or {}).get("items", []):
            if it.get("title") == "Test FM":
                rid = (it.get("meta") or {}).get("station_id") or it.get("id")
        call("radio.delete", {"id": int(str(rid).split(":")[-1]) if rid else 0})
        call("artwork.get", {"track": tracks[0]})
        call("settings.get", {})
        call("settings.set", {"settings": {"gapless": True}})
        call("settings.set", {"settings": {"navidrome_password": "x"}}, "settings.set-secret")
        call("library.stats", {})
        call("service.info", {})
        call("visualization.status", {})
        call("visualization.data", {"bars": 16}, "visualization.data")
        call("visualization.subscribe", {"bars": 16, "fps": 10, "encoding": "b64"})
        time.sleep(0.8)
        call("visualization.status", {}, "visualization.status-subscribed")
        call("visualization.unsubscribe", {})
        call("visualization.stop", {})
        call("playback.play_external", {"path": music + "/Artist/Album/01 Song 1.wav", "queue": True}, "play_external-queue")
        call("playback.stop", {}); settle()
        call("state.snapshot", {}, "snapshot-stopped")
        call("library.scan", {}); time.sleep(1.5)
        call("state.unsubscribe", {})
        send_raw(b"{not json")
        m, _ = read_until(None)
        out.append("## malformed\n" + json.dumps(mask(m, sb=sb)))
        call("service.shutdown", {})
        kinds = sorted(set(n for n in notes if not n.startswith("vis:")))
        out.append("## notifications\n" + json.dumps(kinds))
        out.append("## vis-frames\n" + json.dumps(sorted(set(n for n in notes if n.startswith("vis:")))))
    finally:
        try:
            srv.wait(5)
        except subprocess.TimeoutExpired:
            srv.kill()
        shutil.rmtree(sb, ignore_errors=True)
    print("\n".join(out))


if __name__ == "__main__":
    main()
