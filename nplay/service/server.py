"""Unix-domain JSON-RPC server for the NPLAY headless control service."""
import fcntl
import json
import os
import signal
import socket
import stat
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from queue import Empty, Full, Queue

from ..logging_utils import logger
from ..redact import redact_text
from .backend import ServiceBackend
from .protocol import (
    MAX_FRAME,
    PROTOCOL_VERSION,
    RpcError,
    BackendError,
    encode,
    error_response,
    lock_path,
    parse_request,
    response,
    socket_path,
)

log = logger()

MAX_CLIENT_QUEUE = 256
MAX_OUTSTANDING = 8


class Client:
    def __init__(self, sock, server):
        self.sock = sock
        self.server = server
        self.queue = Queue(maxsize=MAX_CLIENT_QUEUE)
        self.alive = True
        self.subscribed = False
        self.outstanding = 0
        self.lock = threading.Lock()
        self.write_lock = threading.Lock()
        self.reader = threading.Thread(target=self._read, daemon=True, name="nplay-client-read")
        self.writer = threading.Thread(target=self._write, daemon=True, name="nplay-client-write")

    def start(self):
        self.writer.start()
        self.reader.start()

    def send(self, obj, droppable=False):
        if not self.alive:
            return False
        try:
            self.queue.put_nowait(obj)
            return True
        except Full:
            if droppable:
                return False
            self.close()
            return False

    def _write(self):
        while self.alive:
            try:
                obj = self.queue.get(timeout=0.5)
            except Empty:
                continue
            try:
                with self.write_lock:
                    self.sock.settimeout(5.0)
                    self.sock.sendall(encode(obj))
            except Exception:
                self.close()
                return

    def send_sync(self, obj):
        try:
            with self.write_lock:
                self.sock.settimeout(5.0)
                self.sock.sendall(encode(obj))
            return True
        except Exception:
            return False

    def _read(self):
        buf = b""
        self.sock.settimeout(0.5)
        while self.alive:
            try:
                chunk = self.sock.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if len(line) > MAX_FRAME:
                    self.send_sync(error_response(None, RpcError(-32600, "frame too large", {"limit": MAX_FRAME})))
                    self.close()
                    return
                if not line.strip():
                    continue
                self._submit(line)
            if len(buf) > MAX_FRAME:
                self.send_sync(error_response(None, RpcError(-32600, "frame too large", {"limit": MAX_FRAME})))
                self.close()
                return
        self.close()

    def _submit(self, line):
        try:
            req_id, method, params = parse_request(line)
        except RpcError as e:
            self.send(error_response(None, e))
            return
        with self.lock:
            if self.outstanding >= MAX_OUTSTANDING:
                self.send(error_response(req_id, RpcError(-32005, "too many outstanding requests")))
                return
            self.outstanding += 1
        self.server.executor.submit(self._run, req_id, method, params)

    def _run(self, req_id, method, params):
        try:
            if method == "state.subscribe":
                self.subscribed = True
                result = self.server.backend.snapshot()
            elif method == "state.unsubscribe":
                self.subscribed = False
                result = {"ok": True}
            elif method == "service.shutdown":
                result = {"ok": True, "pid": os.getpid()}
            else:
                result = self.server.backend.dispatch(method, params)
            self.send(response(req_id, result))
        except RpcError as e:
            self.send(error_response(req_id, e))
        except (ValueError, KeyError, TypeError) as e:
            self.send(error_response(req_id, RpcError(-32602, "invalid parameters", {"detail": redact_text(e)[:200]})))
        except Exception as e:
            log.exception("service request failed method=%s", method)
            self.send(error_response(req_id, RpcError(-32603, "internal error", {"detail": redact_text(e)[:200]})))
        finally:
            with self.lock:
                self.outstanding -= 1
            if method == "service.shutdown":
                self.server.request_stop("shutdown requested")

    def close(self):
        if not self.alive:
            return
        self.alive = False
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass
        self.server.client_gone(self)


class ControlServer:
    def __init__(self, path=None, idle_exit=0, backend=None):
        self.path = Path(socket_path(path))
        self.idle_exit = max(0, int(idle_exit or 0))
        self.backend = backend or ServiceBackend(idle_exit=self.idle_exit)
        self.backend._socket_path = self.path
        self.backend.notify = self.broadcast
        self.executor = ThreadPoolExecutor(max_workers=8, thread_name_prefix="nplay-rpc-worker")
        self.clients = set()
        self.clients_lock = threading.Lock()
        self.stop_event = threading.Event()
        self.listener = None
        self.lock_file = None
        self.started = time.monotonic()
        self.last_client = time.monotonic()
        self.had_client = False
        self.reason = ""
        self._shutdown_done = False

    def broadcast(self, method, params):
        with self.clients_lock:
            clients = [c for c in self.clients if c.subscribed and c.alive]
        for c in clients:
            c.send({"jsonrpc": "2.0", "method": method, "params": params}, droppable=True)

    def client_gone(self, client):
        with self.clients_lock:
            self.clients.discard(client)
            if not self.clients:
                self.last_client = time.monotonic()

    def request_stop(self, reason="stopped"):
        self.reason = reason
        self.stop_event.set()

    def _prepare(self):
        parent = self.path.parent
        parent.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(parent, 0o700)
        except OSError:
            pass
        try:
            st = os.lstat(parent)
        except OSError as e:
            raise BackendError("cannot access runtime directory", {"detail": str(e)})
        if st.st_uid != os.getuid():
            raise BackendError("runtime directory is not owned by this user", {"path": str(parent)})
        if not stat.S_ISDIR(st.st_mode):
            raise BackendError("runtime path is not a directory", {"path": str(parent)})
        lp = lock_path(self.path)
        flags = os.O_RDWR | os.O_CREAT
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(str(lp), flags, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            return False
        self.lock_file = os.fdopen(fd, "r+b")
        return True

    def _clean_stale(self):
        try:
            st = os.lstat(self.path)
        except FileNotFoundError:
            return
        if st.st_uid != os.getuid():
            raise BackendError("socket path is not owned by this user", {"path": str(self.path)})
        if not stat.S_ISSOCK(st.st_mode):
            raise BackendError("socket path exists and is not a socket", {"path": str(self.path)})
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.settimeout(0.25)
        try:
            probe.connect(str(self.path))
        except OSError:
            try:
                self.path.unlink()
            except FileNotFoundError:
                pass
        else:
            raise BackendError("another NPLAY service is already listening", {"path": str(self.path)})
        finally:
            probe.close()

    def start(self):
        if not self._prepare():
            raise BackendError("another NPLAY service holds the instance lock", {"path": str(self.path)})
        self._clean_stale()
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(str(self.path))
        os.chmod(self.path, 0o600)
        s.listen(16)
        s.settimeout(0.5)
        self.listener = s
        log.info("nplay service listening path=%s protocol=%s pid=%s", self.path, PROTOCOL_VERSION, os.getpid())

    def _idle_loop(self):
        while not self.stop_event.wait(0.5):
            if not self.idle_exit:
                continue
            with self.clients_lock:
                empty = not self.clients
            if empty and time.monotonic() - self.last_client >= self.idle_exit:
                self.request_stop("idle exit")
                return

    def run(self, prestarted=False):
        if not prestarted:
            self.start()
        threading.Thread(target=self._idle_loop, daemon=True, name="nplay-idle").start()
        try:
            while not self.stop_event.is_set():
                try:
                    conn, _ = self.listener.accept()
                except socket.timeout:
                    continue
                except OSError:
                    if self.stop_event.is_set():
                        break
                    continue
                client = Client(conn, self)
                with self.clients_lock:
                    self.clients.add(client)
                    self.had_client = True
                client.start()
        finally:
            self.shutdown()
        return self.reason

    def shutdown(self):
        if self._shutdown_done:
            return
        self._shutdown_done = True
        self.stop_event.set()
        time.sleep(0.2)
        try:
            if self.listener:
                self.listener.close()
        except OSError:
            pass
        with self.clients_lock:
            clients = list(self.clients)
        for c in clients:
            c.close()
        self.executor.shutdown(wait=False, cancel_futures=True)
        try:
            self.backend.close()
        except Exception:
            log.exception("backend close failed")
        try:
            if self.path.exists() and stat.S_ISSOCK(os.lstat(self.path).st_mode):
                self.path.unlink()
        except OSError:
            pass
        try:
            if self.lock_file:
                fcntl.flock(self.lock_file.fileno(), fcntl.LOCK_UN)
                self.lock_file.close()
        except OSError:
            pass
        log.info("nplay service stopped reason=%s", self.reason)


def serve(path=None, idle_exit=0):
    """Run the service in the foreground. Returns a process exit code."""
    server = None
    started = False
    try:
        server = ControlServer(path=path, idle_exit=idle_exit)
        def stop(signum, frame):
            server.request_stop("signal %d" % signum)
        old_term = signal.signal(signal.SIGTERM, stop)
        old_int = signal.signal(signal.SIGINT, stop)
        try:
            server.start()
            started = True
        except BackendError as e:
            print(f"NPLAY service: {e.message}", flush=True)
            if e.data.get("path"):
                print(f"Existing endpoint: {e.data['path']}", flush=True)
            return 0 if ("already" in e.message or "lock" in e.message) else 1
        print(f"NPLAY {PROTOCOL_VERSION} service listening on {server.path}", flush=True)
        print(f"pid {os.getpid()} · protocol {PROTOCOL_VERSION}", flush=True)
        server.run(prestarted=True)
        return 0
    except Exception as e:
        print(f"NPLAY service failed: {e}", flush=True)
        log.exception("service failed")
        return 1
    finally:
        if server and started:
            try:
                server.shutdown()
            except Exception:
                pass
