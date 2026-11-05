"""One-shot, read-only query of a running NPLAY service (diagnostics only)."""
import json
import socket

from .protocol import socket_path


def service_info(timeout=0.3, path=None):
    """Return the running service's `service.info` result, or None if none answers."""
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout)
            s.connect(str(socket_path(path)))
            s.sendall(b'{"jsonrpc":"2.0","id":1,"method":"service.info","params":{}}\n')
            buf = b""
            while b"\n" not in buf and len(buf) < 65536:
                chunk = s.recv(65536)
                if not chunk:
                    return None
                buf += chunk
        msg = json.loads(buf.split(b"\n", 1)[0])
        return msg.get("result") if isinstance(msg, dict) else None
    except (OSError, ValueError):
        return None


def vis_line(local_running, path=None):
    """The U3 diagnostics line: 'VIS: CAVA, n subscribers' (+ who runs CAVA)."""
    info = service_info(path=path)
    vis = (info or {}).get("visualization") if info else None
    parts = [f"terminal {'running' if local_running else 'off'}"]
    if vis is None:
        parts.append("service not running" if info is None else "service ≤1.7 (polling only)")
        subs = 0
    else:
        subs = int(vis.get("subscribers") or 0)
        parts.append(f"service {'running' if vis.get('running') else 'off'}" + (" · polled" if vis.get("polled_recently") else ""))
    return f"VIS: CAVA, {subs} subscriber{'s' if subs != 1 else ''} · " + " · ".join(parts)
