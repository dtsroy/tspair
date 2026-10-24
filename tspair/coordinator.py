"""Exit-node coordinator: the 3-state machine plus the tailscale actions
and peer messages that drive it.

States (ExitState):
    NONE        no exit-node relationship
    USING_OTHER this device routes via the peer  (tailscale set --exit-node=peer)
    BEING_USED  the peer routes via this device  (tailscale set --advertise-exit-node)

Rules:
  * Entering USING_OTHER requires an REQ_USE/GRANT handshake; the provider
    refuses if it is itself USING_OTHER, so a routing loop can never form.
  * Simultaneous "use" requests (both sides click at once) are broken by
    device_id: the side with the larger id yields.
  * When the user side drops back to NONE (STATE message), the provider
    automatically stops advertising and returns to NONE too.
"""

from __future__ import annotations

import logging
import threading
from typing import Callable, Optional

from . import tscli
from .exit_state import ExitState, LABELS
from .link import PeerLink

log = logging.getLogger(__name__)

REQUEST_TIMEOUT = 10.0  # seconds to wait for GRANT/DENY


class Coordinator:
    def __init__(self, device_id: str, role: str, port: int, ping_interval: float) -> None:
        self.link = PeerLink(
            device_id, role, port, ping_interval,
            on_connected=self._on_connected, on_msg=self._on_msg, on_down=self._on_down,
        )
        self.on_change: Callable[[], None] = lambda: None  # tray refresh hook

        self._id = device_id
        self._lock = threading.RLock()
        self._state = ExitState.NONE
        self._peer_state: Optional[ExitState] = None
        self._peer_ip = ""
        self._reply = threading.Event()      # set when GRANT/DENY arrives
        self._reply_granted = False
        self._reply_reason = ""
        self._pending_out = False            # an outbound REQ_USE is in flight

    # ---- properties --------------------------------------------------

    @property
    def state(self) -> ExitState:
        return self._state

    @property
    def peer_state(self) -> Optional[ExitState]:
        return self._peer_state

    def status_line(self) -> str:
        peer = "peer connected" if self.link.is_connected() else "peer offline"
        mine = LABELS[self._state]
        theirs = LABELS[self._peer_state] if self._peer_state else "unknown"
        return f"{peer} | Us: {mine} | Peer: {theirs}"

    def set_peer_ip(self, ip: str) -> None:
        self._peer_ip = ip

    # ---- user actions (called from the tray) --------------------------

    def use_peer(self) -> tuple[bool, str]:
        """Try to route this device's traffic via the peer (NONE -> USING_OTHER)."""
        with self._lock:
            if self._state != ExitState.NONE:
                return False, f"Cannot start: this device is {LABELS[self._state]}."
            if self._peer_state == ExitState.USING_OTHER:
                return False, "Peer is already using this device as its exit node."
            if not self.link.is_connected():
                return False, "Peer is not connected."
            self._reply.clear()
            self._pending_out = True
        try:
            if not self.link.send({"type": "REQ_USE"}):
                return False, "Peer is not connected."
            if not self._reply.wait(REQUEST_TIMEOUT):
                return False, "Peer did not answer the request (timeout)."
            if not self._reply_granted:
                return False, f"Peer refused: {self._reply_reason or 'unknown reason'}"

            peer_ip = self._peer_ip
            if not peer_ip:
                return False, "Peer Tailscale IP unknown yet; try again shortly."
            try:
                tscli.set_exit_node(peer_ip)
            except tscli.TailscaleError as e:
                # Provider is left advertising with no user — tell it to unwind.
                self.link.send({"type": "USE_FAILED"})
                return False, f"tailscale rejected the exit node: {e}"
        finally:
            with self._lock:
                self._pending_out = False

        with self._lock:
            self._state = ExitState.USING_OTHER
        self._broadcast_state()
        return True, f"Now using peer ({peer_ip}) as exit node."

    def stop_using_peer(self) -> tuple[bool, str]:
        """Stop routing via the peer (USING_OTHER -> NONE). Provider unwinds on its own."""
        with self._lock:
            if self._state != ExitState.USING_OTHER:
                return False, "This device is not using the peer as exit node."
        try:
            tscli.clear_exit_node()
        except tscli.TailscaleError as e:
            return False, f"Failed to clear exit node: {e}"
        with self._lock:
            self._state = ExitState.NONE
        self._broadcast_state()  # peer sees NONE and un-advertises by itself
        return True, "Back to normal routing."

    # ---- link callbacks ------------------------------------------------

    def _on_connected(self, _conn) -> None:
        # Sync state to the peer right after the handshake.
        self._broadcast_state()

    def _on_down(self) -> None:
        with self._lock:
            self._peer_state = None
            self._reply_granted = False
            self._reply_reason = "connection lost"
            self._reply.set()
        self.on_change()

    def _on_msg(self, msg: dict) -> None:
        mtype = msg.get("type")
        if mtype == "PING":
            self.link.send({"type": "PONG"})
        elif mtype == "PONG":
            pass
        elif mtype == "STATE":
            self._apply_peer_state(ExitState(msg.get("state", "none")))
        elif mtype == "REQ_USE":
            self._handle_request()
        elif mtype == "GRANT":
            with self._lock:
                self._reply_granted = True
                self._reply_reason = ""
                self._reply.set()
        elif mtype in ("DENY", "USE_FAILED"):
            if mtype == "USE_FAILED":
                # We granted, but the peer failed to apply the exit node.
                self._unadvertise()
            with self._lock:
                self._reply_granted = False
                self._reply_reason = str(msg.get("reason", "refused"))
                self._reply.set()
        else:
            log.debug("ignoring unknown message type %r", mtype)

    # ---- state transitions ----------------------------------------------

    def _apply_peer_state(self, new: ExitState) -> None:
        with self._lock:
            old = self._peer_state
            self._peer_state = new
            # Peer stopped routing via us -> stop advertising, return to NONE.
            if new == ExitState.NONE and self._state == ExitState.BEING_USED:
                log.info("peer returned to NONE; un-advertising exit node")
                self._unadvertise()
            if old != new:
                log.info("peer state: %s -> %s", old, new)
        self.on_change()

    def _unadvertise(self) -> None:
        """BEING_USED -> NONE (caller holds the lock or single-threaded ctx)."""
        self._state = ExitState.NONE
        try:
            tscli.advertise_exit_node(False)
        except tscli.TailscaleError as e:
            log.error("failed to stop advertising exit node: %s", e)
        self._broadcast_state()

    def _broadcast_state(self) -> None:
        self.link.send({"type": "STATE", "state": self._state.value})
        self.on_change()

    # ---- provider side of the handshake ---------------------------------

    def _handle_request(self) -> None:
        with self._lock:
            # I'm routing via the peer: granting would create a loop.
            if self._state == ExitState.USING_OTHER:
                self.link.send({"type": "DENY", "reason": "routing loop: both devices would use each other"})
                return
            if self._state == ExitState.BEING_USED:
                self.link.send({"type": "DENY", "reason": "already serving someone else"})
                return

            # Simultaneous requests: larger device_id yields.
            if self._pending_out and not self._reply.is_set():
                if self._id > _peer_id_of(self.link):
                    self.link.send({"type": "DENY", "reason": "simultaneous request; I yielded to you"})
                    return
                # They will yield to us (their id is larger) and grant ours.

            try:
                tscli.advertise_exit_node(True)
            except tscli.TailscaleError as e:
                self.link.send({"type": "DENY", "reason": f"tailscale error: {e}"})
                return
            self._state = ExitState.BEING_USED
        self.link.send({"type": "GRANT"})
        self._broadcast_state()
        log.info("peer may now use this device as exit node")


def _peer_id_of(link: PeerLink) -> str:
    """Peer id for tie-breaking; unknown ids sort last."""
    return link.last_peer_id or "zzzzzzzz"
