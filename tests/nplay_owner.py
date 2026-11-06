"""Test helper: run the real terminal `nplay` entry point with a stub UI.

The stub UI starts the local librespot receiver from a short-lived worker
thread (as the terminal menus do), reports `ready <nplay pid> <receiver pid>`
to the file in argv[1] and then waits for "q" on stdin, like pressing Q.
Everything else (signal handling, shutdown, the engine) is NPLAY's own code.
Run it on a pty: NPLAY refuses to start without an interactive terminal.
"""
import os
import sys
import threading
import time

import nplay.app as app_mod

READY = sys.argv[1]
MODE = sys.argv[2] if len(sys.argv) > 2 else "start"


class StubUI:
    def __init__(self, app):
        self.app = app

    def run(self):
        box = {}

        def start():
            try:
                if MODE == "authorize":
                    self.app.spotify.local.authorize(timeout=20, opener=lambda url: True)
                else:
                    self.app.spotify.local.start(oauth=False)
                box["pid"] = self.app.spotify.local.pid()
            except Exception as e:
                box["error"] = repr(e)

        t = threading.Thread(target=start)
        t.start()
        t.join()
        # The worker thread has exited here: the receiver must survive that
        # (PR_SET_PDEATHSIG is tied to the spawning thread).
        time.sleep(0.3)
        alive = self.app.spotify.local.running()
        with open(READY + ".tmp", "w") as f:
            f.write(f"ready {os.getpid()} {box.get('pid') or 0} {int(alive)} {box.get('error', '')}\n")
        os.replace(READY + ".tmp", READY)
        while True:
            line = sys.stdin.readline()
            if not line or line.strip() == "q":
                return


app_mod.UI = StubUI
sys.argv = ["nplay"]
app_mod.main()
