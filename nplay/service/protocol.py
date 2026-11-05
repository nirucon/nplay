"""Versioned local control protocol shared by NPLAY's headless service.

Framing is newline-delimited JSON (UTF-8). The transport is a Unix-domain
socket owned by the invoking user. Requests and notifications use JSON-RPC 2.0
so existing tooling and small clients can stay simple.

This module deliberately contains no playback knowledge; it defines constants,
safe framing and structured errors only.
"""
import json
import os
from pathlib import Path

PROTOCOL_VERSION = 1
# Minor version: additions only, every 1.x request keeps working.
#   1.0  NPLAY 1.7.0  initial protocol
#   1.1  NPLAY 1.8.0  visualization.subscribe/unsubscribe/status push stream
PROTOCOL_MINOR = 1
SERVICE_NAME = "nplay"
MAX_FRAME = 1024 * 1024
MAX_TRACKS = 5000
POSITION_BUCKETS = 64


class RpcError(Exception):
    """A structured JSON-RPC error that is safe to show to local clients."""

    def __init__(self, code, message, data=None):
        super().__init__(message)
        self.code = int(code)
        self.message = str(message)
        self.data = data or {}

    def payload(self):
        out = {"code": self.code, "message": self.message}
        if self.data:
            out["data"] = self.data
        return out


class ProtocolError(RpcError):
    def __init__(self, message, data=None):
        super().__init__(-32600, message, data)


class MethodError(RpcError):
    def __init__(self, message, data=None):
        super().__init__(-32601, message, data)


class ParamsError(RpcError):
    def __init__(self, message, data=None):
        super().__init__(-32602, message, data)


class BackendError(RpcError):
    def __init__(self, message, data=None):
        super().__init__(-32000, message, data)


def runtime_dir():
    """Return the private runtime directory used for the control socket."""
    base = os.environ.get("XDG_RUNTIME_DIR")
    if base:
        return Path(base) / SERVICE_NAME
    state = os.environ.get("XDG_STATE_HOME")
    if state:
        return Path(state) / SERVICE_NAME / "run"
    return Path.home() / ".local" / "state" / SERVICE_NAME / "run"


def socket_path(override=None):
    if override:
        return Path(override).expanduser()
    env = os.environ.get("NPLAY_SERVICE_SOCKET")
    if env:
        return Path(env).expanduser()
    return runtime_dir() / "control.sock"


def lock_path(path=None):
    path = Path(path or socket_path())
    return path.with_name(path.name + ".lock")


def encode(obj):
    """Encode one frame. Never includes the trailing newline in error text."""
    try:
        return (json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    except (TypeError, ValueError) as e:
        raise BackendError("response is not serialisable", {"detail": str(e)[:200]})


def response(req_id, result):
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def error_response(req_id, err):
    return {"jsonrpc": "2.0", "id": req_id, "error": err.payload() if isinstance(err, RpcError) else {"code": -32603, "message": "internal error"}}


def notification(method, params):
    return {"jsonrpc": "2.0", "method": method, "params": params}


def parse_request(line):
    """Validate an incoming request envelope without executing anything."""
    if not isinstance(line, (bytes, bytearray)):
        raise ProtocolError("frame must be bytes")
    if len(line) > MAX_FRAME:
        raise ProtocolError("frame too large", {"limit": MAX_FRAME})
    try:
        value = json.loads(line.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise ProtocolError("malformed JSON")
    if not isinstance(value, dict):
        raise ProtocolError("request must be an object")
    if value.get("jsonrpc") != "2.0":
        raise ProtocolError("jsonrpc must be 2.0")
    method = value.get("method")
    if not isinstance(method, str) or not method or len(method) > 128:
        raise ProtocolError("method must be a non-empty string")
    params = value.get("params", {})
    if params is None:
        params = {}
    if not isinstance(params, dict):
        raise ParamsError("params must be an object")
    req_id = value.get("id")
    if req_id is not None and not isinstance(req_id, (str, int)):
        raise ProtocolError("id must be a string or integer")
    return req_id, method, params
