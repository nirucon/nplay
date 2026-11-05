"""Privacy self-checks for `nplay --doctor` (U2).

Every check returns (ok, label, detail, fix). `fix` is a single shell line or
instruction and is empty when the check passed. Nothing here prints or returns
a secret: log scans report counts only.
"""
import os
import stat
import tempfile

from .config import CONFIG, STATE, private_runtime_dir
from .logging_utils import log_files
from .redact import contains_secret


def _mode(path):
    return stat.S_IMODE(os.stat(path).st_mode)


def _short(path):
    home = os.path.expanduser("~")
    s = str(path)
    return "~" + s[len(home):] if home and s.startswith(home + os.sep) else s


def check_state_dir():
    try:
        m = _mode(STATE)
    except OSError as e:
        return (False, "state dir", f"{_short(STATE)} unreadable ({e.strerror})", f"mkdir -p {_short(STATE)} && chmod 700 {_short(STATE)}")
    if m & 0o077:
        return (False, "state dir", f"{_short(STATE)} mode {m:03o}", f"chmod 700 {_short(STATE)}")
    return (True, "state dir", f"{_short(STATE)} mode {m:03o}", "")


def check_log_modes():
    loose = []
    for p in log_files():
        try:
            m = _mode(p)
        except OSError:
            continue
        if m & 0o077:
            loose.append((p, m))
    if loose:
        names = " ".join(_short(p) for p, _ in loose)
        return (False, "log modes", ", ".join(f"{p.name} {m:03o}" for p, m in loose), f"chmod 600 {names}")
    return (True, "log modes", f"{len(log_files())} files · 600", "")


def check_logs_clean():
    dirty = []
    for p in log_files():
        try:
            text = p.read_text(errors="replace")
        except OSError:
            continue
        if contains_secret(text):
            dirty.append(p)
    if dirty:
        return (False, "log secrets", "credentials found in " + ", ".join(p.name for p in dirty),
                "start nplay once (the start-up migration redacts them) or delete: rm " + " ".join(_short(p) for p in dirty))
    return (True, "log secrets", "none in nplay.log, rotations, mpv.log", "")


def check_secrets_file():
    p = CONFIG / "secrets.ini"
    if not p.exists():
        return (True, "secrets", "no secrets file", "")
    m = _mode(p)
    if m & 0o077:
        return (False, "secrets", f"{_short(p)} mode {m:03o}", f"chmod 600 {_short(p)}")
    return (True, "secrets", f"{_short(p)} mode {m:03o}", "")


def check_runtime():
    """CAVA config and mpv IPC socket location (private runtime dir, no /tmp leftovers)."""
    leftovers = []
    tmp = tempfile.gettempdir()
    try:
        for name in os.listdir(tmp):
            if (name.startswith("nplay-cava-") and name.endswith(".conf")) or (name.startswith(f"nplay-mpv-{os.getuid()}-") and name.endswith(".sock")):
                try:
                    if os.lstat(os.path.join(tmp, name)).st_uid == os.getuid():
                        leftovers.append(os.path.join(tmp, name))
                except OSError:
                    pass
    except OSError:
        pass
    d = private_runtime_dir()
    try:
        m = _mode(d)
    except OSError:
        m = 0o777
    if m & 0o077:
        return (False, "cava config", f"{_short(d)} mode {m:03o}", f"chmod 700 {_short(d)}")
    if leftovers:
        return (False, "cava config", f"{_short(d)} · {len(leftovers)} old file(s) in {tmp}", "rm -f " + " ".join(leftovers[:4]) + (" …" if len(leftovers) > 4 else ""))
    return (True, "cava config", f"{_short(d)} (700), removed when CAVA stops", "")


def check_process_argv():
    """No running mpv of this user carries a credential in its argv (older NPLAY)."""
    bad = 0
    uid = os.getuid()
    try:
        pids = [p for p in os.listdir("/proc") if p.isdigit()]
    except OSError:
        return (True, "argv", "not checked (no /proc)", "")
    for pid in pids:
        try:
            if os.stat(f"/proc/{pid}").st_uid != uid:
                continue
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                argv = f.read().replace(b"\0", b" ").decode("utf-8", "replace")
        except OSError:
            continue
        if "mpv" in argv.split(" ", 1)[0] and contains_secret(argv):
            bad += 1
    if bad:
        return (False, "argv", f"{bad} mpv process(es) with a credential in argv (NPLAY <= 1.7)", "quit nplay and restart NIRUAMP / `nplay --serve` so mpv restarts under NPLAY 1.8")
    return (True, "argv", "no credential in mpv argv", "")


CHECKS = (check_state_dir, check_log_modes, check_logs_clean, check_secrets_file, check_runtime, check_process_argv)


def run_checks():
    out = []
    for fn in CHECKS:
        try:
            out.append(fn())
        except Exception as e:
            out.append((False, fn.__name__.replace("check_", ""), f"check failed: {type(e).__name__}", ""))
    return out


def doctor_lines():
    lines = []
    for ok, label, detail, fix in run_checks():
        lines.append(f"privacy {label:<13}{'OK' if ok else 'WARN'} · {detail}")
        if not ok and fix:
            lines.append(f"  fix: {fix}")
    return lines
