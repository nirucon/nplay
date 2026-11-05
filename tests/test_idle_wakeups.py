"""P4: an idle service (no client, nothing playing) wakes at most once per second."""
import os
import socket
import tempfile
import time
import unittest
from pathlib import Path

from test_service import ServiceProcess, Wire


def switches(pid):
    total = 0
    for task in os.listdir(f"/proc/{pid}/task"):
        try:
            with open(f"/proc/{pid}/task/{task}/status") as f:
                for line in f:
                    if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")):
                        total += int(line.split()[1])
        except OSError:
            pass
    return total


@unittest.skipUnless(Path("/proc/self/task").exists(), "needs Linux /proc")
class IdleWakeupTests(unittest.TestCase):
    def measure(self, svc, seconds=3.0):
        time.sleep(1.5)  # let start-up work (library refresh, etc.) settle
        a = switches(svc.proc.pid)
        time.sleep(seconds)
        return (switches(svc.proc.pid) - a) / seconds

    def test_idle_without_clients(self):
        with tempfile.TemporaryDirectory() as d:
            svc = ServiceProcess(d, extra_args=("--idle-exit", "600")).start()
            try:
                rate = self.measure(svc)
                self.assertLessEqual(rate, 1.0, f"{rate:.2f} wakeups/s while idle")
            finally:
                svc.stop()

    def test_idle_with_connected_subscribed_client(self):
        with tempfile.TemporaryDirectory() as d:
            svc = ServiceProcess(d, extra_args=("--idle-exit", "600")).start()
            try:
                with Wire(svc.sock_path) as w:
                    w.request(1, "initialize", {"protocol_versions": [1]})
                    w.request(2, "state.subscribe")
                    rate = self.measure(svc)
                    self.assertLessEqual(rate, 1.0, f"{rate:.2f} wakeups/s with an idle client")
                    self.assertIn("result", w.request(3, "ping"))
            finally:
                svc.stop()

    def test_idle_exit_still_fires_and_shutdown_is_fast(self):
        with tempfile.TemporaryDirectory() as d:
            svc = ServiceProcess(d, extra_args=("--idle-exit", "1")).start()
            with Wire(svc.sock_path) as w:
                w.request(1, "ping")
            started = time.monotonic()
            svc.proc.wait(timeout=10)
            self.assertLess(time.monotonic() - started, 6)
            self.assertFalse(svc.sock_path.exists())
            svc.stop()


if __name__ == "__main__":
    unittest.main()
