"""Loopback smoke test: two Coordinators talk to each other over localhost
with the tailscale CLI monkeypatched. Run: python tests/test_smoke.py
(also works under pytest)."""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from tspair import tscli
from tspair.coordinator import Coordinator
from tspair.exit_state import ExitState

PORT_A = 52911  # server listens here; client dials it


def wait(cond, timeout=5.0, what="condition"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if cond():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


def main() -> None:
    calls: list[str] = []

    def fake_advertise(on: bool) -> None:
        calls.append(f"advertise:{on}")

    def fake_set_exit(ip: str) -> None:
        calls.append(f"exit:{ip}")

    def fake_clear() -> None:
        calls.append("clear")

    tscli.advertise_exit_node = fake_advertise
    tscli.set_exit_node = fake_set_exit
    tscli.clear_exit_node = fake_clear

    a = Coordinator("aaaa-device", "server", PORT_A, ping_interval=1.0)
    b = Coordinator("bbbb-device", "client", PORT_A, ping_interval=1.0)
    a.set_peer_ip("127.0.0.1")
    b.set_peer_ip("127.0.0.1")
    a.link.start()
    b.link.start()

    # 1. Client dials the listening server -> one link, HELLO exchanged.
    time.sleep(0.2)  # give the server time to bind
    assert b.link.dial("127.0.0.1", PORT_A), "client failed to dial server"
    wait(lambda: a.link.is_connected() and b.link.is_connected(), what="handshake")
    # A duplicate dial must be rejected, not replace the live link.
    b.link.dial("127.0.0.1", PORT_A)
    time.sleep(0.2)
    assert a.link.is_connected() and b.link.is_connected(), "duplicate dial broke the link"

    # 2. State sync after connect.
    wait(lambda: a.peer_state == ExitState.NONE and b.peer_state == ExitState.NONE,
         what="initial state sync")

    # 3. A uses B as exit node: A -> USING_OTHER, B -> BEING_USED.
    ok, msg = a.use_peer()
    assert ok, msg
    wait(lambda: a.state == ExitState.USING_OTHER, what="A becomes user")
    wait(lambda: b.state == ExitState.BEING_USED, what="B becomes provider")
    assert "advertise:True" in calls and "exit:127.0.0.1" in calls

    # 4. Loop guard: B must not be able to use A back.
    ok, msg = b.use_peer()
    assert not ok, "mutual exit node was allowed!"
    assert a.state == ExitState.USING_OTHER and b.state == ExitState.BEING_USED

    # 5. A stops -> B automatically un-advertises; both back to NONE.
    ok, msg = a.stop_using_peer()
    assert ok, msg
    wait(lambda: a.state == ExitState.NONE, what="A returns to NONE")
    wait(lambda: b.state == ExitState.NONE, what="B auto-un-advertises")
    assert "advertise:False" in calls and "clear" in calls

    a.link.stop()
    b.link.stop()
    print("all smoke checks passed")


if __name__ == "__main__":
    main()
