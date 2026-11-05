#!/usr/bin/env python3
"""CAVA stand-in for tests: raw 8-bit frames at 30 fps from the -p config.

FAKE_CAVA_MUSIC_FRAMES varying frames are followed by silence (all zeros)
forever. FAKE_CAVA_LOG (optional) receives one line per start with the pid and
the config path, so tests can check process lifetime and config location.
"""
import os
import re
import sys
import time

cfg = sys.argv[sys.argv.index("-p") + 1]
text = open(cfg).read()
bars = int(re.search(r"bars\s*=\s*(\d+)", text).group(1))
log = os.environ.get("FAKE_CAVA_LOG")
if log:
    with open(log, "a") as f:
        f.write(f"{os.getpid()} {cfg}\n")
music = int(os.environ.get("FAKE_CAVA_MUSIC_FRAMES", "15"))
out = sys.stdout.buffer
n = 0
try:
    while True:
        if n < music:
            frame = bytes(((i * 7 + n * 13) % 250) + 1 for i in range(bars))
        else:
            frame = bytes(bars)
        out.write(frame)
        out.flush()
        n += 1
        time.sleep(1 / 30)
except (BrokenPipeError, KeyboardInterrupt):
    pass
