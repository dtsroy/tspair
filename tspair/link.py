"""Peer TCP link: at most one live connection to the peer at any time.

The two ends of the pair have asymmetric roles (set in the config):
  * "server" — listens on the port, never dials.
  * "client" — dials the peer's Tailscale IP whenever the peer is online.

The link is transport-only; message semantics live in coordinator.py.
"""

from __future__ import annotations

import logging
import socket
import threading
from typing import Callable, Optional

from .protocol import RECV_BUF, FrameReader, encode

log = logging.getLogger(__name__)

ConnCallback = Callable[[socket.socket], None]   # connection established (after HELLO)
MsgCallback = Callable[[dict], None]             # message received
DownCallback = Callable[[], None]                # connection lost


class PeerLink:
    def __init__(self, device_id: str, role: str, port: int, ping_interval: float,
                 on_connected: ConnCallback, on_msg: MsgCallback, on_down: DownCallback) -> None:
        assert role in ("server", "client"), role
        self.device_id = device_id
        self.role = role
        self.port = port
        self.ping_interval = ping_interval
        self.on_connected = on_connected
        self.on_msg = on_msg
        self.on_down = on_down

        self._lock = threading.Lock()
        self._conn: Optional[socket.socket] = None  # live, HELLO-completed connection
        self.last_peer_id = ""                      # device_id from the last HELLO
        self._stop = threading.Event()

    # ---- lifecycle -------------------------------------------------

    def start(self) -> None:
        if self.role == "server":
            threading.Thread(target=self._listen_loop, name="link-listen",
                             daemon=True).start()
        threading.Thread(target=self._ping_loop, name="link-ping",
                         daemon=True).start()

    def stop(self) -> None:
        self._stop.set()
        with self._lock:
            if self._conn:
                try:
                    self._conn.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                self._conn.close()
                self._conn = None

    def is_connected(self) -> bool:
        with self._lock:
            return self._conn is not None

    # ---- sending ---------------------------------------------------

    def send(self, msg: dict) -> bool:
        with self._lock:
            conn = self._conn
        if conn is None:
            return False
        try:
            conn.sendall(encode(msg))
            return True
        except OSError:
            log.debug("send failed, dropping connection")
            self._drop(conn)
            return False

    # ---- inbound listener (server role) ------------------------------

    def _listen_loop(self) -> None:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as srv:
            srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            srv.bind(("0.0.0.0", self.port))
            srv.listen(2)
            log.info("listening on port %d", self.port)
            while not self._stop.is_set():
                srv.settimeout(1.0)
                try:
                    conn, addr = srv.accept()
                except socket.timeout:
                    continue
                except OSError:
                    break
                threading.Thread(target=self._serve, args=(conn, "inbound", addr),
                                 daemon=True, name="link-inbound").start()

    # ---- outbound dialer (client role) -------------------------------

    def dial(self, ip: str, timeout: float = 3.0) -> bool:
        """Try to establish the outbound connection; safe to call repeatedly.
        Server-role links only listen — dialing is a no-op for them."""
        if self.role == "server" or self.is_connected():
            return self.is_connected()
        try:
            conn = socket.create_connection((ip, self.port), timeout=timeout)
        except OSError as e:
            log.debug("dial %s:%d failed: %s", ip, self.port, e)
            return False
        threading.Thread(target=self._serve, args=(conn, "outbound", (ip, self.port)),
                         daemon=True, name="link-outbound").start()
        return True

    # ---- per-connection serving --------------------------------------

    def _serve(self, conn: socket.socket, kind: str, addr) -> None:
        conn.settimeout(10.0)  # bound the HELLO handshake
        reader = FrameReader()
        peer_id: Optional[str] = None
        log.info("%s connection from/to %s", kind, addr)
        try:
            # Handshake: both sides announce identity.
            conn.sendall(encode({"type": "HELLO", "id": self.device_id}))
            while True:
                data = conn.recv(RECV_BUF)
                if not data:
                    break
                for msg in reader.feed(data):
                    if peer_id is None:
                        if msg.get("type") != "HELLO":
                            raise ConnectionError("expected HELLO")
                        peer_id = str(msg.get("id", ""))
                        self.last_peer_id = peer_id
                        conn.settimeout(None)  # handshake done; block freely
                        if not self._admit(conn):
                            conn.close()
                            return
                        log.info("link established with %s (%s)", peer_id[:8], kind)
                        self.on_connected(conn)
                    else:
                        try:
                            self.on_msg(msg)
                        except Exception:
                            log.exception("error handling message %r", msg)
        except OSError as e:
            log.debug("connection error (%s): %s", kind, e)
        finally:
            self._drop(conn)

    def _admit(self, conn: socket.socket) -> bool:
        """Accept the connection if we have none; drop duplicates."""
        with self._lock:
            if self._conn is None:
                self._conn = conn
                return True
        log.info("duplicate connection rejected (already connected)")
        return False

    def _drop(self, conn: socket.socket) -> None:
        had = False
        with self._lock:
            if self._conn is conn:
                self._conn = None
                had = True
        try:
            conn.close()
        except OSError:
            pass
        if had and not self._stop.is_set():
            self.on_down()

    # ---- heartbeat ----------------------------------------------------

    def _ping_loop(self) -> None:
        while not self._stop.wait(self.ping_interval):
            if self.is_connected() and not self.send({"type": "PING"}):
                log.info("peer unreachable (ping failed)")
